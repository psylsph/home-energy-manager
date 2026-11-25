//! Putting the battery charge limit back after a Spread Charge plan
//! (issue #508, the write half of #483).
//!
//! With Spread Charge on, the Planner writes a lower charge limit for the
//! cheap window so the battery fills near the window's end. That register caps
//! solar charging too, so it has to go back. This module owns the going back.
//!
//! Nothing is lowered when the plan is applied. The plan *arms* a window in
//! memory (and writes its slot and full-rate limit exactly as it does with the
//! setting off), and the poll loop lowers the limit only once the window's
//! slot is running. That keeps the lowered limit off the register before the
//! window (where it would cap solar charging the plan didn't model) and means
//! HEM being away at the start simply leaves the charge at full power.
//!
//! Invariants:
//! - **The baseline is on disk before the lowered value is queued**, and it is
//!   the user's value, never HEM's own lowered one.
//! - **The limit always goes back.** A run ends only by a restore that a later
//!   read confirms, by giving up after 15 minutes with an alert that says what
//!   to put back, or because the user changed the limit themselves.
//! - **The user's change wins.** A charge-limit write that isn't HEM's own, or
//!   a read-back showing a value that is neither HEM's nor the baseline (seen
//!   on two reads from different frames, so one corrupt frame can't end a
//!   run), ends the run without a restore.
//! - **A read-back that still shows the baseline never ends a run.** The write
//!   may simply not have landed yet; the restore at the window's end is then a
//!   harmless write of the value already there.
//! - **Nothing is written for another inverter.** A run for a different serial
//!   is dropped, never restored onto the wrong unit.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::inverter::encoder::RegisterWrite;
use crate::inverter::model::{DeviceType, InverterSnapshot};

/// Unconfirmed attempts sent back to back before backing off.
const FAST_ATTEMPTS: u32 = 2;
/// The wait between attempts once backed off.
pub const RETRY_SECS: i64 = 5 * 60;
/// How long a lowered write that still reads back as the baseline waits
/// before it is sent again.
pub const LOWER_RETRY_SECS: i64 = 2 * 60;
/// How long the restore is retried before HEM alerts and lets go.
pub const RESTORE_GIVE_UP_SECS: i64 = 15 * 60;
/// How long shutdown waits for the restore to be taken.
pub const SHUTDOWN_RESTORE_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanRatePhase {
    /// The lowered limit is on (or on its way to) the inverter.
    Lowered,
    /// The baseline is being written back.
    Restoring,
}

/// A lowered charge limit that must go back, persisted in
/// `Settings::plan_charge_rate_run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRateRun {
    pub phase: PlanRatePhase,
    /// The inverter the limit was read from.
    pub serial: String,
    pub device_type: DeviceType,
    /// The limit register's value before HEM lowered it (the user's value).
    pub baseline_raw: u16,
    /// The baseline as a percentage of the inverter's maximum, for telling the
    /// user what to put back by hand.
    #[serde(default)]
    pub baseline_percent: u16,
    /// The value HEM wrote for the window.
    pub lowered_raw: u16,
    /// When the window ends and the baseline goes back.
    pub window_end: DateTime<Utc>,
    /// Snapshot time (Unix seconds) of the last restore write.
    #[serde(default)]
    pub last_sent: i64,
    /// Restore attempts sent since read-back last matched.
    #[serde(default)]
    pub unconfirmed: u32,
    /// When the restore started (snapshot Unix seconds).
    #[serde(default)]
    pub restore_started: Option<i64>,
    /// Charge slot 1 as the plan wrote it. A slot that stops reading this way
    /// was changed by someone else, and the limit goes back early. `None` for
    /// a run saved before this was recorded: it is not checked.
    #[serde(default)]
    pub slot: Option<(u16, u16)>,
}

/// The run's state in `AppState`, under one lock.
#[derive(Debug, Clone, Default)]
pub struct PlanRateShared {
    /// Mirrors `Settings::plan_charge_rate_run`.
    pub run: Option<PlanRateRun>,
    /// A window the Planner wants spread, waiting for its slot to start.
    /// Memory only: after a restart the charge simply runs at full power.
    pub armed: Option<ArmedSpread>,
    /// What to put back by hand, set when a restore was given up and taken
    /// once by the poll loop to alert.
    pub gave_up: Option<String>,
    /// Set at shutdown once the restore is queued: nothing more is written.
    pub stopped: bool,
    /// Consecutive reads (by snapshot time) that don't look like this run's
    /// inverter or limit. One corrupt frame mustn't make HEM forget a
    /// baseline it still has to put back, so a run is dropped only after a
    /// second such read from a different frame.
    pub foreign_reads: u8,
    /// Snapshot time of the last foreign read counted.
    pub foreign_at: i64,
}

/// A cheap window the Planner has asked to spread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmedSpread {
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    /// The limit to write, % of the inverter's maximum battery power.
    pub lowered_percent: u16,
    /// Charge slot 1's start and end (minutes of the day) as the plan wrote
    /// them. The limit is lowered only while slot 1 still reads this way: a
    /// slot the user has since changed isn't the plan's charge.
    pub slot: (u16, u16),
}

/// Whether charge slot 1 still reads as the slot a plan wrote. The end of the
/// day may be written as 23:59 or 00:00; both mean the same.
pub fn planned_slot_present(snap: &InverterSnapshot, slot: (u16, u16)) -> bool {
    let day_end = |minute: u16| if minute == 23 * 60 + 59 { 0 } else { minute };
    let live = &snap.charge_slots[0];
    live.enabled
        && u16::from(live.start_hour) * 60 + u16::from(live.start_minute) == slot.0
        && day_end(u16::from(live.end_hour) * 60 + u16::from(live.end_minute)) == day_end(slot.1)
}

/// What the poll loop knows this poll.
#[derive(Debug, Clone, Copy)]
pub struct Inputs {
    pub now: DateTime<Utc>,
    /// Charge slot 1 is running on the inverter now.
    pub slot_running: bool,
    /// Something else needs full power or owns the limit (Force Charge, the
    /// #434 top-up, calibration, Adaptive Charge, a safety limiter): a
    /// lowered limit is put back, and none is started.
    pub blocked: bool,
}

/// One poll's result.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Step {
    pub writes: Vec<RegisterWrite>,
    /// The run changed (or ended) and must be persisted before any write.
    pub persist: bool,
    /// A run was created this poll: if it can't be saved, the write must not
    /// go out.
    pub started: bool,
}

/// Why a lowered limit can't be taken on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartRefusal {
    /// The inverter hasn't reported its identity, so the baseline couldn't be
    /// tied to a unit.
    NoIdentity,
    /// The inverter's battery writes don't reach the battery (a Gateway).
    ForeignBattery,
}

/// The register that holds this device's charge limit.
pub fn limit_register(device: DeviceType) -> u16 {
    device.power_limit_bank().charge_register()
}

fn lower_writes(run: &PlanRateRun) -> Vec<RegisterWrite> {
    crate::inverter::calibration::rate_writes(
        run.device_type.power_limit_bank(),
        Some(run.lowered_raw),
        None,
    )
    .unwrap_or_default()
}

fn restore_writes(run: &PlanRateRun) -> Vec<RegisterWrite> {
    crate::inverter::calibration::rate_writes(
        run.device_type.power_limit_bank(),
        Some(run.baseline_raw),
        None,
    )
    .unwrap_or_default()
}

/// Capture the baseline for a lowered limit from a live snapshot. The run is
/// returned for the caller to persist *before* it queues the lowered write.
pub fn start(
    snap: &InverterSnapshot,
    lowered_raw: u16,
    window_end: DateTime<Utc>,
) -> Result<PlanRateRun, StartRefusal> {
    if snap.inverter_serial.is_empty() {
        return Err(StartRefusal::NoIdentity);
    }
    if !snap.device_type.battery_controls_reach_battery() {
        return Err(StartRefusal::ForeignBattery);
    }
    let baseline_raw = snap.charge_rate as u16;
    let scale = crate::inverter::power_limit::PowerLimitScale::from_snapshot(snap);
    Ok(PlanRateRun {
        phase: PlanRatePhase::Lowered,
        serial: snap.inverter_serial.clone(),
        device_type: snap.device_type,
        baseline_raw,
        baseline_percent: scale.raw_to_percent(baseline_raw),
        lowered_raw,
        window_end,
        // The lowered write goes out with this run: a retry waits from here.
        last_sent: snap.timestamp,
        unconfirmed: 0,
        restore_started: None,
        slot: None,
    })
}

/// Run one poll's step.
pub fn step(shared: &mut PlanRateShared, snap: &InverterSnapshot, inputs: Inputs) -> Step {
    if shared.stopped {
        return Step::default();
    }
    let now = inputs.now;
    if shared.run.is_none() {
        return step_armed(shared, snap, inputs);
    }
    let run = shared.run.as_mut().expect("checked above");
    let ended = Step {
        persist: true,
        ..Step::default()
    };
    // A snapshot that hasn't reported its identity yet (just after a restart or
    // reconnect) says nothing about whose limit this is: wait for one that does.
    if snap.inverter_serial.is_empty() {
        return Step::default();
    }
    let reading = snap.charge_rate as u16;
    let other_inverter = snap.inverter_serial != run.serial;
    // A value that is neither HEM's nor the baseline was set by someone else
    // (the GE app, or a write that never passed through HEM): theirs stands.
    // A reading of the baseline proves nothing, as the lowered write may not
    // have landed yet.
    let foreign = reading != run.lowered_raw && reading != run.baseline_raw;
    if other_inverter || foreign {
        // Neither is acted on from one frame: a corrupt read would otherwise
        // make HEM forget a baseline it still has to put back.
        if !foreign_confirmed(shared, snap.timestamp) {
            return Step::default();
        }
        if other_inverter {
            tracing::warn!(
                "Spread Charge: the saved charge limit belongs to another inverter; dropping it without writing"
            );
        } else {
            tracing::info!(
                reading,
                "Spread Charge: the charge limit was changed by someone else; leaving it as they set it"
            );
        }
        shared.run = None;
        shared.foreign_reads = 0;
        return ended;
    }
    shared.foreign_reads = 0;
    let run = shared.run.as_mut().expect("checked above");
    match run.phase {
        PlanRatePhase::Lowered => {
            // Something that needs full power starts the restore early, and so
            // does a slot 1 that no longer reads as the plan wrote it.
            let slot_changed = run
                .slot
                .is_some_and(|slot| !planned_slot_present(snap, slot));
            if now < run.window_end && !inputs.blocked && !slot_changed {
                // A lowered write the inverter turned away (busy, a dropped
                // frame) still reads as the baseline: send it again, so the
                // plan isn't quietly charging at full power while the page
                // says the limit is held down. A newer read than the last
                // send is needed, and a retry is never saved: the baseline
                // is already on disk.
                if reading == run.baseline_raw
                    && run.baseline_raw != run.lowered_raw
                    && snap.timestamp > run.last_sent
                    && snap.timestamp - run.last_sent >= LOWER_RETRY_SECS
                {
                    run.last_sent = snap.timestamp;
                    return Step {
                        writes: lower_writes(run),
                        persist: false,
                        started: false,
                    };
                }
                return Step::default();
            }
            let ts = snap.timestamp;
            run.phase = PlanRatePhase::Restoring;
            run.restore_started = Some(ts);
            run.unconfirmed = 1;
            run.last_sent = ts;
            tracing::info!(
                "Spread Charge: the cheap window has ended; putting the charge limit back"
            );
            Step {
                writes: restore_writes(run),
                persist: true,
                started: false,
            }
        }
        PlanRatePhase::Restoring => step_restoring(shared, snap),
    }
}

/// Count a read that doesn't look like the run's inverter or limit; true on
/// the second one from a different frame.
fn foreign_confirmed(shared: &mut PlanRateShared, ts: i64) -> bool {
    if shared.foreign_reads == 0 || ts != shared.foreign_at {
        shared.foreign_reads = shared.foreign_reads.saturating_add(1);
        shared.foreign_at = ts;
    }
    shared.foreign_reads >= 2
}

/// A replan wrote a different slot: the armed window no longer describes it,
/// and a limit already lowered for the old one is put back now rather than
/// holding the new slot down.
pub fn release_for_replan(shared: &mut PlanRateShared, now: DateTime<Utc>) {
    shared.armed = None;
    if let Some(run) = shared.run.as_mut() {
        if run.phase == PlanRatePhase::Lowered && run.window_end > now {
            run.window_end = now;
        }
    }
}

fn step_restoring(shared: &mut PlanRateShared, snap: &InverterSnapshot) -> Step {
    let run = shared.run.as_mut().expect("checked by the caller");
    let ts = snap.timestamp;
    // A restore stamped later than this read means the host clock stepped
    // back; re-stamp rather than wait on a time that may never come.
    let skewed = run.last_sent > ts;
    if !skewed && ts <= run.last_sent {
        // Nothing newer than the last write to react to.
        return Step::default();
    }
    let done = Step {
        persist: true,
        ..Step::default()
    };
    if !skewed && snap.charge_rate as u16 == run.baseline_raw {
        tracing::info!("Spread Charge: charge limit restored");
        shared.run = None;
        return done;
    }
    let started = match run.restore_started {
        Some(started) if started <= ts => started,
        _ => {
            run.restore_started = Some(ts);
            ts
        }
    };
    if ts - started >= RESTORE_GIVE_UP_SECS {
        tracing::warn!(
            "Spread Charge: the inverter didn't take the charge limit back in 15 minutes; giving up"
        );
        shared.gave_up = Some(describe_restore(run));
        shared.run = None;
        return done;
    }
    if skewed || run.unconfirmed < FAST_ATTEMPTS || ts - run.last_sent >= RETRY_SECS {
        run.unconfirmed = run.unconfirmed.saturating_add(1);
        run.last_sent = ts;
        return Step {
            writes: restore_writes(run),
            persist: true,
            started: false,
        };
    }
    Step::default()
}

/// With no run: lower the limit once an armed window's slot is running.
fn step_armed(shared: &mut PlanRateShared, snap: &InverterSnapshot, inputs: Inputs) -> Step {
    let Some(armed) = shared.armed.clone() else {
        return Step::default();
    };
    if inputs.now >= armed.window_end {
        shared.armed = None;
        return Step::default();
    }
    if inputs.now < armed.window_start {
        return Step::default();
    }
    if inputs.blocked {
        // Someone else needs the limit; sit this window out.
        shared.armed = None;
        return Step::default();
    }
    if !inputs.slot_running || !planned_slot_present(snap, armed.slot) {
        // The slot hasn't started on the inverter's clock yet, was cleared,
        // or no longer reads as the plan wrote it (the user changed slot 1
        // after Apply): wait, and let the window pass.
        return Step::default();
    }
    let request =
        crate::inverter::power_limit::ChargeRateRequest::for_snapshot(armed.lowered_percent, snap);
    if request.needs_pack_size() {
        // The pack size may simply not be read yet: stay armed.
        return Step::default();
    }
    let lowered_raw = request.to_raw();
    let mut run = match start(snap, lowered_raw, armed.window_end) {
        Ok(run) => run,
        Err(StartRefusal::NoIdentity) => {
            // No serial yet (just after a reconnect): try again next poll.
            return Step::default();
        }
        Err(StartRefusal::ForeignBattery) => {
            shared.armed = None;
            return Step::default();
        }
    };
    // From here the window is used up, whether or not anything is lowered.
    shared.armed = None;
    // Nothing to lower when the limit already sits at or below the plan's.
    if lowered_raw >= run.baseline_raw {
        return Step::default();
    }
    run.slot = Some(armed.slot);
    let writes = crate::inverter::calibration::rate_writes(
        run.device_type.power_limit_bank(),
        Some(lowered_raw),
        None,
    )
    .unwrap_or_default();
    if writes.is_empty() {
        return Step::default();
    }
    tracing::info!(
        baseline = run.baseline_raw,
        lowered = lowered_raw,
        "Spread Charge: lowering the charge limit for the cheap window"
    );
    shared.run = Some(run);
    Step {
        writes,
        persist: true,
        started: true,
    }
}

/// What a write someone made during a run means for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Absorb {
    /// Not the charge limit, or HEM's own write: nothing changes.
    Unchanged,
    /// Someone else set the limit: their value wins, the run ends unrestored.
    UserWon,
}

/// Classify an acknowledged write against the run.
pub fn absorb(run: &PlanRateRun, write: &RegisterWrite) -> Absorb {
    if write.address != limit_register(run.device_type) {
        return Absorb::Unchanged;
    }
    let ours = write.value == run.lowered_raw
        || (run.phase == PlanRatePhase::Restoring && write.value == run.baseline_raw);
    if ours {
        Absorb::Unchanged
    } else {
        Absorb::UserWon
    }
}

/// The charge limit a new owner of the limit (Force Charge, the smart-charge
/// top-up, a calibration cycle) should treat as "the user's setting" and put
/// back afterwards: the value HEM will restore while a spread run holds the
/// limit down, otherwise what the inverter reports. Capturing the lowered
/// value instead would strand it there once they finish.
pub fn baseline_charge_rate(run: Option<&PlanRateRun>, snap: &InverterSnapshot) -> u8 {
    match run {
        Some(run) if run.serial == snap.inverter_serial => {
            u8::try_from(run.baseline_raw).unwrap_or(snap.charge_rate)
        }
        _ => snap.charge_rate,
    }
}

/// `snap` with its charge limit read as [`baseline_charge_rate`], for the
/// baselines other features capture from a snapshot.
pub fn with_baseline_limit(run: Option<&PlanRateRun>, snap: &InverterSnapshot) -> InverterSnapshot {
    InverterSnapshot {
        charge_rate: baseline_charge_rate(run, snap),
        ..snap.clone()
    }
}

/// What to put back by hand when a restore is given up.
pub fn describe_restore(run: &PlanRateRun) -> String {
    format!(
        "Set the battery charge limit back to {}% on the Control page",
        run.baseline_percent
    )
}

/// Apply the result of saving the run to a step that must be saved first. A
/// new run that can't be saved is dropped with its write: a lowered limit
/// nobody could put back must never reach the inverter. A failed save of a
/// restore only warrants a warning: sending it is safe, and a restart that
/// finds the old run just restores again. Returns whether the step's writes
/// may go out.
pub fn settle_persist(
    shared: &mut PlanRateShared,
    outcome: &mut Step,
    saved: Result<(), String>,
) -> bool {
    let Err(e) = saved else {
        return true;
    };
    if outcome.started {
        tracing::warn!(
            "Spread Charge: couldn't save the charge limit to put back, not lowering it: {e}"
        );
        shared.run = None;
        outcome.writes.clear();
        false
    } else {
        tracing::warn!("Spread Charge: couldn't save its state: {e}");
        true
    }
}

/// Persist the run.
pub async fn persist_run(run: Option<PlanRateRun>) -> Result<(), String> {
    crate::settings::Settings::update_async(move |s| s.plan_charge_rate_run = run).await
}

/// On a graceful shutdown, put the charge limit back so a lowered limit isn't
/// left on the inverter while HEM is away. With the inverter unreachable the
/// run stays on disk and the next start finishes it.
pub async fn restore_on_shutdown(
    state: &std::sync::Arc<crate::inverter::poll::AppState>,
    limit: std::time::Duration,
) {
    let writes = {
        let mut guard = state.plan_rate.lock().await;
        guard.stopped = true;
        match guard.run.as_ref() {
            Some(run) => restore_writes(run),
            None => return,
        }
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    crate::server::api::queue_writes_with_policy(
        state,
        writes,
        crate::inverter::poll::WriteBatchPolicy::FailFast,
        Some(tx),
        None,
    )
    .await;
    match tokio::time::timeout(limit, rx).await {
        Ok(Ok(crate::inverter::encoder::WriteOutcome::Ok)) => {
            tracing::info!("Spread Charge: charge limit restored on shutdown");
            let mut guard = state.plan_rate.lock().await;
            match persist_run(None).await {
                Ok(()) => guard.run = None,
                Err(e) => tracing::warn!("Spread Charge: couldn't clear its run: {e}"),
            }
        }
        _ => tracing::warn!(
            "Spread Charge: couldn't restore the charge limit on shutdown; it is put back when HEM starts again"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modbus::registers::{HR_AC_BATTERY_CHARGE_LIMIT, HR_BATTERY_CHARGE_LIMIT};
    use chrono::TimeZone;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    /// The plan's slot 1, as the helpers below arm and read it.
    const PLANNED_SLOT: (u16, u16) = (23 * 60, 6 * 60);

    fn snap(charge_rate: u8, timestamp: i64) -> InverterSnapshot {
        let mut snap = InverterSnapshot {
            device_type: DeviceType::Gen3Hybrid,
            inverter_serial: "SA1234".to_string(),
            charge_rate,
            timestamp,
            // 0.5 register units per percent on a half-scale pack.
            max_battery_power_w: 5_000,
            battery_capacity_kwh: 9.5,
            ..Default::default()
        };
        snap.charge_slots[0] = crate::inverter::model::ScheduleSlot {
            enabled: true,
            start_hour: 23,
            start_minute: 0,
            end_hour: 6,
            end_minute: 0,
            ..Default::default()
        };
        snap
    }

    fn inp(secs: i64) -> Inputs {
        Inputs {
            now: at(secs),
            slot_running: true,
            blocked: false,
        }
    }

    fn armed(start: i64, end: i64, percent: u16) -> PlanRateShared {
        PlanRateShared {
            armed: Some(ArmedSpread {
                window_start: at(start),
                window_end: at(end),
                lowered_percent: percent,
                slot: PLANNED_SLOT,
            }),
            ..Default::default()
        }
    }

    fn lowered_run(window_end: i64) -> PlanRateRun {
        start(&snap(50, 1_000), 20, at(window_end)).unwrap()
    }

    fn shared(run: PlanRateRun) -> PlanRateShared {
        PlanRateShared {
            run: Some(run),
            ..Default::default()
        }
    }

    fn rate(value: u16) -> RegisterWrite {
        RegisterWrite {
            address: HR_BATTERY_CHARGE_LIMIT,
            value,
        }
    }

    // ----- baselines other features capture -----

    #[test]
    fn a_new_owner_of_the_limit_captures_the_baseline_while_a_run_holds_it_down() {
        let run = lowered_run(9_000);
        let mut reading = snap(20, 2_000);
        reading.charge_rate = 20;
        assert_eq!(baseline_charge_rate(Some(&run), &reading), 50);
        assert_eq!(with_baseline_limit(Some(&run), &reading).charge_rate, 50);
        // Nothing else changes in the snapshot.
        assert_eq!(
            with_baseline_limit(Some(&run), &reading).inverter_serial,
            reading.inverter_serial
        );
    }

    #[test]
    fn with_no_run_or_another_inverters_run_the_reading_stands() {
        let mut reading = snap(35, 2_000);
        reading.charge_rate = 35;
        assert_eq!(baseline_charge_rate(None, &reading), 35);
        let mut foreign = lowered_run(9_000);
        foreign.serial = "OTHER".to_string();
        assert_eq!(baseline_charge_rate(Some(&foreign), &reading), 35);
    }

    // ----- start -----

    #[test]
    fn start_captures_the_current_limit_as_the_baseline() {
        let run = start(&snap(50, 1_000), 20, at(9_000)).unwrap();
        assert_eq!(run.phase, PlanRatePhase::Lowered);
        assert_eq!(run.baseline_raw, 50);
        assert_eq!(run.lowered_raw, 20);
        assert_eq!(run.serial, "SA1234");
        assert_eq!(run.window_end, at(9_000));
    }

    #[test]
    fn start_refuses_without_an_identity() {
        let mut s = snap(50, 1_000);
        s.inverter_serial.clear();
        assert_eq!(
            start(&s, 20, at(9_000)).unwrap_err(),
            StartRefusal::NoIdentity
        );
    }

    #[test]
    fn start_refuses_a_gateway_whose_battery_writes_do_not_reach_the_battery() {
        let mut s = snap(50, 1_000);
        s.device_type = DeviceType::Gateway;
        assert_eq!(
            start(&s, 20, at(9_000)).unwrap_err(),
            StartRefusal::ForeignBattery
        );
    }

    // ----- restore at the window's end -----

    #[test]
    fn nothing_happens_before_the_window_ends() {
        let mut s = shared(lowered_run(9_000));
        let step = step(&mut s, &snap(20, 2_000), inp(2_000));
        assert_eq!(step, Step::default());
        assert_eq!(s.run.as_ref().unwrap().phase, PlanRatePhase::Lowered);
    }

    #[test]
    fn at_the_window_end_the_baseline_is_written_and_the_run_is_saved_first() {
        let mut s = shared(lowered_run(9_000));
        let step = step(&mut s, &snap(20, 9_000), inp(9_000));
        assert_eq!(step.writes, vec![rate(50)]);
        assert!(step.persist, "the run must be saved before the write");
        let run = s.run.as_ref().unwrap();
        assert_eq!(run.phase, PlanRatePhase::Restoring);
        assert_eq!(run.last_sent, 9_000);
        assert_eq!(run.restore_started, Some(9_000));
    }

    #[test]
    fn the_restore_needs_a_newer_read_to_confirm() {
        let mut s = shared(lowered_run(9_000));
        step(&mut s, &snap(20, 9_000), inp(9_000));
        // A read from the same frame as the write doesn't confirm, even if
        // it happens to show the baseline.
        let same_frame = step(&mut s, &snap(50, 9_000), inp(9_000));
        assert!(
            s.run.is_some(),
            "a read no newer than the write confirms nothing"
        );
        assert!(same_frame.writes.is_empty());
        // A newer read showing the baseline ends the run.
        let done = step(&mut s, &snap(50, 9_003), inp(9_003));
        assert!(done.persist);
        assert!(done.writes.is_empty());
        assert!(s.run.is_none());
        assert!(s.gave_up.is_none());
    }

    #[test]
    fn a_restore_that_does_not_land_is_retried_then_backs_off() {
        let mut s = shared(lowered_run(9_000));
        step(&mut s, &snap(20, 9_000), inp(9_000)); // attempt 1
        let second = step(&mut s, &snap(20, 9_003), inp(9_003)); // attempt 2
        assert_eq!(second.writes, vec![rate(50)]);
        // Backed off: nothing for the next few polls.
        let quiet = step(&mut s, &snap(20, 9_006), inp(9_006));
        assert!(quiet.writes.is_empty());
        // After the retry interval another attempt goes out.
        let later = step(
            &mut s,
            &snap(20, 9_003 + RETRY_SECS),
            inp(9_003 + RETRY_SECS),
        );
        assert_eq!(later.writes, vec![rate(50)]);
    }

    #[test]
    fn a_restore_that_never_lands_is_given_up_with_an_alert_and_the_run_cleared() {
        let mut s = shared(lowered_run(9_000));
        step(&mut s, &snap(20, 9_000), inp(9_000));
        let t = 9_000 + RESTORE_GIVE_UP_SECS;
        let last = step(&mut s, &snap(20, t), inp(t));
        assert!(last.persist);
        assert!(last.writes.is_empty());
        assert!(s.run.is_none(), "a run is never kept past the give-up");
        let detail = s.gave_up.take().expect("one alert is owed");
        assert!(detail.contains("charge limit"), "{detail}");
    }

    #[test]
    fn a_run_past_its_window_end_at_startup_restores_at_once() {
        // HEM was away through the window's end: the first poll restores.
        let mut s = shared(lowered_run(9_000));
        let step = step(&mut s, &snap(20, 90_000), inp(90_000));
        assert_eq!(step.writes, vec![rate(50)]);
    }

    #[test]
    fn a_clock_that_jumps_back_does_not_restore_early_or_lose_the_run() {
        let mut s = shared(lowered_run(9_000));
        let early = step(&mut s, &snap(20, 500), inp(500));
        assert_eq!(early, Step::default());
        assert!(s.run.is_some());
    }

    #[test]
    fn a_restore_start_in_the_future_is_reset_rather_than_waited_on() {
        // A host clock that stepped back after the restore began: the start
        // is later than now, so it is re-stamped, not trusted forever.
        let mut s = shared(lowered_run(9_000));
        step(&mut s, &snap(20, 9_000), inp(9_000));
        s.run.as_mut().unwrap().restore_started = Some(1_000_000);
        step(&mut s, &snap(20, 9_010), inp(9_010));
        let run = s.run.as_ref().expect("still restoring");
        assert_eq!(run.restore_started, Some(9_010));
    }

    // ----- the user wins -----

    #[test]
    fn a_read_back_that_is_neither_ours_nor_the_baseline_ends_the_run_unrestored() {
        let mut s = shared(lowered_run(9_000));
        // One frame is not enough: it may be a corrupt read.
        assert_eq!(step(&mut s, &snap(35, 2_000), inp(2_000)), Step::default());
        assert!(s.run.is_some());
        let step = step(&mut s, &snap(35, 2_010), inp(2_010));
        assert!(step.writes.is_empty(), "their value stands");
        assert!(step.persist);
        assert!(s.run.is_none());
    }

    #[test]
    fn one_corrupt_frame_never_makes_hem_forget_the_baseline() {
        let mut s = shared(lowered_run(9_000));
        // A bad frame, then clean ones: the run carries on and the baseline
        // still goes back at the window's end.
        step(&mut s, &snap(0, 2_000), inp(2_000));
        step(&mut s, &snap(20, 2_010), inp(2_010));
        // A later bad frame starts the count afresh.
        step(&mut s, &snap(0, 2_020), inp(2_020));
        assert!(s.run.is_some());
        step(&mut s, &snap(20, 2_030), inp(2_030));
        let end = step(&mut s, &snap(20, 9_000), inp(9_000));
        assert_eq!(end.writes, vec![rate(50)]);
    }

    #[test]
    fn the_same_foreign_frame_read_twice_counts_once() {
        let mut s = shared(lowered_run(9_000));
        step(&mut s, &snap(35, 2_000), inp(2_000));
        step(&mut s, &snap(35, 2_000), inp(2_000));
        assert!(s.run.is_some(), "one frame, however often it is looked at");
    }

    #[test]
    fn a_read_back_still_showing_the_baseline_never_ends_the_run() {
        // The lowered write may not have landed yet.
        let mut s = shared(lowered_run(9_000));
        let step = step(&mut s, &snap(50, 1_030), inp(1_030));
        assert_eq!(step, Step::default());
        assert!(s.run.is_some());
    }

    #[test]
    fn a_lowered_write_that_still_reads_as_the_baseline_is_sent_again() {
        // The run starts at 1_000 with the baseline (50) still on the inverter.
        let mut s = shared(lowered_run(9_000));
        let before = s.run.clone();
        let out = step(
            &mut s,
            &snap(50, 1_000 + LOWER_RETRY_SECS),
            inp(1_000 + LOWER_RETRY_SECS),
        );
        assert_eq!(out.writes, vec![rate(20)], "the lowered value again");
        assert!(
            !out.persist && !out.started,
            "the baseline is already on disk"
        );
        assert_eq!(s.run.as_ref().unwrap().phase, PlanRatePhase::Lowered);
        assert_ne!(s.run, before, "the send is remembered");
        // Not again until another interval has passed.
        let again = step(
            &mut s,
            &snap(50, 1_000 + LOWER_RETRY_SECS + 10),
            inp(1_000 + LOWER_RETRY_SECS + 10),
        );
        assert_eq!(again, Step::default());
        let later = 1_000 + 2 * LOWER_RETRY_SECS;
        assert_eq!(
            step(&mut s, &snap(50, later), inp(later)).writes,
            vec![rate(20)]
        );
    }

    #[test]
    fn a_lowered_limit_that_reads_back_is_not_sent_again() {
        let mut s = shared(lowered_run(9_000));
        let out = step(
            &mut s,
            &snap(20, 1_000 + LOWER_RETRY_SECS),
            inp(1_000 + LOWER_RETRY_SECS),
        );
        assert_eq!(out, Step::default());
    }

    #[test]
    fn the_lowered_write_is_not_retried_while_something_else_holds_the_limit() {
        // Blocked: the restore starts, not another lowering.
        let mut s = shared(lowered_run(9_000));
        let mut blocked = inp(1_000 + LOWER_RETRY_SECS);
        blocked.blocked = true;
        let out = step(&mut s, &snap(50, 1_000 + LOWER_RETRY_SECS), blocked);
        assert_eq!(out.writes, vec![rate(50)]);
        assert_eq!(s.run.as_ref().unwrap().phase, PlanRatePhase::Restoring);
    }

    #[test]
    fn a_snapshot_without_an_identity_never_drops_a_run() {
        // After a restart or reconnect the first reads carry no serial.
        let mut s = shared(lowered_run(9_000));
        for ts in [2_000, 2_010, 2_020, 2_030] {
            let mut unnamed = snap(20, ts);
            unnamed.inverter_serial = String::new();
            assert_eq!(step(&mut s, &unnamed, inp(ts)), Step::default());
        }
        assert!(s.run.is_some(), "the baseline is still held");
        let end = step(&mut s, &snap(20, 9_000), inp(9_000));
        assert_eq!(end.writes, vec![rate(50)]);
    }

    #[test]
    fn a_user_write_to_the_limit_ends_the_run() {
        let run = lowered_run(9_000);
        assert_eq!(absorb(&run, &rate(35)), Absorb::UserWon);
    }

    #[test]
    fn hems_own_lowered_write_is_not_a_user_change() {
        let run = lowered_run(9_000);
        assert_eq!(absorb(&run, &rate(20)), Absorb::Unchanged);
    }

    #[test]
    fn hems_own_restore_write_is_not_a_user_change() {
        let mut s = shared(lowered_run(9_000));
        step(&mut s, &snap(20, 9_000), inp(9_000));
        let run = s.run.as_ref().unwrap();
        assert_eq!(absorb(run, &rate(50)), Absorb::Unchanged);
        assert_eq!(absorb(run, &rate(35)), Absorb::UserWon);
    }

    #[test]
    fn a_write_to_another_register_is_ignored() {
        let run = lowered_run(9_000);
        let other = RegisterWrite {
            address: HR_AC_BATTERY_CHARGE_LIMIT,
            value: 35,
        };
        // Gen3 hybrid's limit is HR 111; HR 313 is another bank's.
        assert_eq!(absorb(&run, &other), Absorb::Unchanged);
        let unrelated = RegisterWrite {
            address: 116,
            value: 80,
        };
        assert_eq!(absorb(&run, &unrelated), Absorb::Unchanged);
    }

    // ----- safety -----

    #[test]
    fn a_run_for_another_inverter_is_dropped_without_a_write() {
        let mut s = shared(lowered_run(9_000));
        let mut other = snap(20, 9_500);
        other.inverter_serial = "ZZ9999".to_string();
        assert_eq!(step(&mut s, &other, inp(9_500)), Step::default());
        assert!(s.run.is_some(), "one odd frame is not enough");
        other.timestamp = 9_510;
        let step = step(&mut s, &other, inp(9_510));
        assert!(step.writes.is_empty());
        assert!(step.persist);
        assert!(s.run.is_none());
    }

    #[test]
    fn a_new_run_that_cannot_be_saved_is_dropped_with_its_write() {
        let mut s = armed(1_000, 9_000, 20);
        let mut out = step(&mut s, &snap(50, 1_100), inp(1_100));
        assert!(out.started && !out.writes.is_empty());
        assert!(!settle_persist(&mut s, &mut out, Err("disk full".into())));
        assert!(
            out.writes.is_empty(),
            "no lowered write without a saved baseline"
        );
        assert!(s.run.is_none());
    }

    #[test]
    fn a_restore_that_cannot_be_saved_is_still_sent() {
        let mut s = shared(lowered_run(9_000));
        let mut out = step(&mut s, &snap(20, 9_000), inp(9_000));
        assert!(!out.started && !out.writes.is_empty());
        assert!(settle_persist(&mut s, &mut out, Err("disk full".into())));
        assert_eq!(out.writes, vec![rate(50)]);
        assert!(s.run.is_some(), "kept, so a restart restores again");
    }

    #[test]
    fn a_saved_step_goes_out_untouched() {
        let mut s = armed(1_000, 9_000, 20);
        let mut out = step(&mut s, &snap(50, 1_100), inp(1_100));
        let before = out.writes.clone();
        assert!(settle_persist(&mut s, &mut out, Ok(())));
        assert_eq!(out.writes, before);
        assert!(s.run.is_some());
    }

    #[test]
    fn a_slot_1_the_user_changed_after_apply_is_not_throttled() {
        // The plan wrote 23:00-06:00; the user then set a short charge.
        let mut s = armed(1_000, 9_000, 20);
        let mut changed = snap(50, 1_100);
        changed.charge_slots[0].end_hour = 23;
        changed.charge_slots[0].end_minute = 30;
        assert_eq!(step(&mut s, &changed, inp(1_100)), Step::default());
        assert!(s.run.is_none() && s.armed.is_some(), "waits, still armed");
        // Not enabled either.
        changed.charge_slots[0] = Default::default();
        assert_eq!(step(&mut s, &changed, inp(1_100)), Step::default());
        assert!(s.run.is_none());
    }

    #[test]
    fn a_window_ending_at_the_end_of_the_day_matches_either_way_it_is_written() {
        let mut snap = snap(50, 1_100);
        snap.charge_slots[0].end_hour = 23;
        snap.charge_slots[0].end_minute = 59;
        assert!(planned_slot_present(&snap, (23 * 60, 0)));
        snap.charge_slots[0].end_hour = 0;
        snap.charge_slots[0].end_minute = 0;
        assert!(planned_slot_present(&snap, (23 * 60, 23 * 60 + 59)));
        assert!(!planned_slot_present(&snap, (22 * 60, 0)));
    }

    #[test]
    fn a_slot_1_changed_during_the_window_puts_the_limit_back_early() {
        let mut s = armed(1_000, 9_000, 20);
        step(&mut s, &snap(50, 1_100), inp(1_100));
        assert!(s.run.is_some());
        let lowered = s.run.as_ref().unwrap().lowered_raw as u8;
        let mut edited = snap(lowered, 1_200);
        edited.charge_slots[0].end_hour = 1;
        let out = step(&mut s, &edited, inp(1_200));
        assert_eq!(out.writes, vec![rate(50)]);
        assert_eq!(s.run.as_ref().unwrap().phase, PlanRatePhase::Restoring);
    }

    #[test]
    fn a_start_refused_for_want_of_a_serial_keeps_the_window_armed() {
        let mut s = armed(1_000, 9_000, 20);
        let mut unnamed = snap(50, 1_100);
        unnamed.inverter_serial = String::new();
        assert_eq!(step(&mut s, &unnamed, inp(1_100)), Step::default());
        assert!(s.armed.is_some(), "tried again next poll");
        let out = step(&mut s, &snap(50, 1_110), inp(1_110));
        assert!(out.started, "lowered once the serial is known");
        assert!(s.armed.is_none());
    }

    #[test]
    fn a_window_for_a_gateway_is_dropped_for_good() {
        let mut s = armed(1_000, 9_000, 20);
        let mut gateway = snap(50, 1_100);
        gateway.device_type = DeviceType::Gateway;
        assert_eq!(step(&mut s, &gateway, inp(1_100)), Step::default());
        assert!(s.armed.is_none() && s.run.is_none());
    }

    #[test]
    fn a_replan_clears_the_armed_window() {
        let mut s = armed(1_000, 9_000, 20);
        release_for_replan(&mut s, at(500));
        assert!(s.armed.is_none());
        assert_eq!(step(&mut s, &snap(50, 1_100), inp(1_100)), Step::default());
        assert!(
            s.run.is_none(),
            "nothing is lowered for a window that was replaced"
        );
    }

    #[test]
    fn a_replan_puts_back_a_limit_already_lowered_for_the_old_slot() {
        let mut s = shared(lowered_run(9_000));
        release_for_replan(&mut s, at(2_000));
        let step = step(&mut s, &snap(20, 2_010), inp(2_010));
        assert_eq!(step.writes, vec![rate(50)]);
        assert_eq!(s.run.as_ref().unwrap().phase, PlanRatePhase::Restoring);
    }

    #[test]
    fn once_stopped_nothing_more_is_written() {
        let mut s = shared(lowered_run(9_000));
        s.stopped = true;
        assert_eq!(step(&mut s, &snap(20, 9_500), inp(9_500)), Step::default());
    }

    #[test]
    fn the_run_round_trips_through_settings_json() {
        let run = lowered_run(9_000);
        let json = serde_json::to_string(&run).unwrap();
        let back: PlanRateRun = serde_json::from_str(&json).unwrap();
        assert_eq!(run, back);
    }

    #[test]
    fn the_restore_uses_the_models_own_limit_register() {
        let mut s = snap(50, 1_000);
        s.device_type = DeviceType::ACCoupled;
        let run = start(&s, 20, at(9_000)).unwrap();
        assert_eq!(restore_writes(&run)[0].address, HR_AC_BATTERY_CHARGE_LIMIT);
        assert_eq!(
            limit_register(DeviceType::ACCoupled),
            HR_AC_BATTERY_CHARGE_LIMIT
        );
    }

    // ----- arming and lowering -----

    #[test]
    fn nothing_is_lowered_before_the_window_starts() {
        let mut s = armed(5_000, 9_000, 40);
        let step = step(&mut s, &snap(50, 1_000), inp(1_000));
        assert_eq!(step, Step::default());
        assert!(s.run.is_none());
        assert!(s.armed.is_some(), "still waiting for its window");
    }

    #[test]
    fn the_limit_is_lowered_once_the_slot_runs_and_the_baseline_is_saved_first() {
        let mut s = armed(5_000, 9_000, 40);
        let step = step(&mut s, &snap(50, 5_010), inp(5_010));
        assert_eq!(step.writes, vec![rate(20)], "40% of the maximum is raw 20");
        assert!(
            step.persist && step.started,
            "the run is saved before the write"
        );
        let run = s.run.as_ref().unwrap();
        assert_eq!(run.baseline_raw, 50);
        assert_eq!(run.lowered_raw, 20);
        assert_eq!(run.window_end, at(9_000));
        assert!(s.armed.is_none());
    }

    #[test]
    fn the_limit_waits_for_the_slot_to_run_on_the_inverters_clock() {
        let mut s = armed(5_000, 9_000, 40);
        let mut input = inp(5_010);
        input.slot_running = false;
        assert_eq!(step(&mut s, &snap(50, 5_010), input), Step::default());
        assert!(s.armed.is_some());
        assert!(s.run.is_none());
    }

    #[test]
    fn an_armed_window_that_has_passed_is_forgotten_without_a_write() {
        let mut s = armed(5_000, 9_000, 40);
        assert_eq!(step(&mut s, &snap(50, 9_500), inp(9_500)), Step::default());
        assert!(s.armed.is_none());
        assert!(s.run.is_none());
    }

    #[test]
    fn a_blocked_window_is_sat_out() {
        let mut s = armed(5_000, 9_000, 40);
        let mut input = inp(5_010);
        input.blocked = true;
        assert_eq!(step(&mut s, &snap(50, 5_010), input), Step::default());
        assert!(s.armed.is_none(), "it doesn't start later in the window");
    }

    #[test]
    fn a_limit_already_at_or_below_the_plans_is_left_alone() {
        // The user's own cap (raw 10) is lower than the spread rate (raw 20).
        let mut s = armed(5_000, 9_000, 40);
        assert_eq!(step(&mut s, &snap(10, 5_010), inp(5_010)), Step::default());
        assert!(s.run.is_none());
    }

    #[test]
    fn a_partial_rate_that_needs_the_pack_size_is_refused() {
        let mut s = armed(5_000, 9_000, 40);
        let mut unknown = snap(50, 5_010);
        unknown.battery_capacity_kwh = 0.0;
        assert_eq!(step(&mut s, &unknown, inp(5_010)), Step::default());
        assert!(s.run.is_none());
    }

    #[test]
    fn something_needing_full_power_restores_the_limit_early() {
        let mut s = shared(lowered_run(9_000));
        let mut input = inp(2_000);
        input.blocked = true;
        let step = step(&mut s, &snap(20, 2_000), input);
        assert_eq!(step.writes, vec![rate(50)]);
        assert_eq!(s.run.as_ref().unwrap().phase, PlanRatePhase::Restoring);
    }

    #[test]
    fn the_armed_state_is_not_persisted_so_a_restart_charges_at_full_power() {
        // Only `run` mirrors settings.json; arming lives in memory.
        let s = armed(5_000, 9_000, 40);
        assert!(s.run.is_none());
    }
}
