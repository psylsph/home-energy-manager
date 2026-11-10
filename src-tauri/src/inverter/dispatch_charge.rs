//! Top the battery up during an Intelligent Octopus Go smart charge
//! (issue #434).
//!
//! Octopus bills the whole house at the off-peak rate for the half-hours a
//! smart dispatch covers, so while the dispatch hold pauses discharge the
//! battery can also charge cheaply. This reuses the Force Charge writes and
//! their capture/restore: charge slot 1 is set to the dispatch's window (the
//! inverter only charges inside a slot, and stops by itself at the slot's end
//! even with HEM away), charging is enabled to the target, and the captured
//! baseline is written back when the dispatch ends.
//!
//! Invariants:
//! - It only charges while the hold is holding a fresh smart dispatch, never
//!   a boost, and never past the dispatch's end.
//! - It only starts from a battery the hold paused from plain Eco, with no
//!   other charge owner (Force Charge, calibration, a running charge slot,
//!   Adaptive Charge, Cosy/Agile, a safety limiter).
//! - The baseline is on disk before the first write, and once a run exists
//!   it ends only by a restore that a later read confirms, or by giving up
//!   after 15 minutes with an alert saying what to put back.
//! - A change the user makes to the charge settings during a run is taken
//!   into the baseline, so the restore puts back their new value.
//! - It writes no rate register and never touches the reserve or HR 318;
//!   the hold and the charge cap own those.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::inverter::calibration::CalibrationWrite;
use crate::inverter::encoder::{ControlCommand, RegisterWrite};
use crate::inverter::model::{DeviceType, InverterSnapshot};
use crate::inverter::poll::ForceChargeRevert;
use crate::server::api::{
    build_force_charge_stop_writes, charge_slot_command_for_device,
    force_charge_revert_from_snapshot, snapshot_matches_writes,
};

/// Unconfirmed attempts sent back to back before backing off.
const FAST_ATTEMPTS: u32 = 2;
/// How far SOC must fall below the target, after a top-up has already
/// reached it during this smart charge, before another one starts: a BMS
/// wobble of a point or two would otherwise restart the whole cycle.
pub const RESTART_MARGIN: u8 = 5;
/// The wait between attempts once backed off.
pub const RETRY_SECS: i64 = 5 * 60;
/// How long the restore is retried before HEM alerts and lets go.
pub const RESTORE_GIVE_UP_SECS: i64 = 15 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchChargePhase {
    /// The charge settings are on the inverter.
    Charging,
    /// The baseline is being written back.
    Restoring,
}

/// A running top-up, persisted in `Settings::dispatch_charge_run`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchChargeRun {
    pub phase: DispatchChargePhase,
    /// What to put back, captured before the first write.
    pub revert: ForceChargeRevert,
    /// The SOC it charges to: 100, or the charge cap's level.
    pub target_soc: u8,
    /// The charge settings in force, re-sent when they don't read back.
    pub writes: Vec<CalibrationWrite>,
    /// The end of the dispatch the slot runs to.
    pub dispatch_end: DateTime<Utc>,
    /// Inverter clock minus host clock, in minutes, fixed at the start so
    /// the slot's end doesn't move with rounding between polls.
    pub skew_minutes: i32,
    /// Snapshot time (Unix seconds) of the last write.
    pub last_sent: i64,
    /// Attempts sent since read-back last matched.
    pub unconfirmed: u32,
    /// When the restore started.
    #[serde(default)]
    pub restore_started: Option<i64>,
}

/// Why the top-up isn't running while a smart charge is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    HoldNotHolding,
    HoldFailing,
    Calibration,
    ManualForce,
    ChargeSlot,
    Adaptive,
    TariffAutomation,
    Safety,
    Full,
    NoIdentity,
    CaptureNotSaved,
    ForeignInverter,
}

impl Reason {
    pub fn code(self) -> &'static str {
        match self {
            Self::HoldNotHolding => "hold_not_holding",
            Self::HoldFailing => "hold_failing",
            Self::Calibration => "calibration",
            Self::ManualForce => "manual_force",
            Self::ChargeSlot => "charge_slot",
            Self::Adaptive => "adaptive_charge",
            Self::TariffAutomation => "tariff_automation",
            Self::Safety => "safety_limiter",
            Self::Full => "full",
            Self::NoIdentity => "no_identity",
            Self::CaptureNotSaved => "capture_not_saved",
            Self::ForeignInverter => "foreign_inverter",
        }
    }

    pub fn message(self, target_soc: u8) -> String {
        match self {
            Self::HoldNotHolding => {
                "The battery isn't paused for this smart charge, so it isn't topped up".into()
            }
            Self::HoldFailing => {
                "The inverter isn't taking the pause, so the battery isn't topped up".into()
            }
            Self::Calibration => "A battery calibration is running".into(),
            Self::ManualForce => "Force Charge is running".into(),
            Self::ChargeSlot => "A charge slot is already charging the battery".into(),
            Self::Adaptive => "Adaptive Charge is managing the charge rate".into(),
            Self::TariffAutomation => "Cosy or Agile automation owns the charge slots".into(),
            Self::Safety => "A safety limiter is holding the battery".into(),
            Self::Full => format!("The battery is already at {target_soc}%"),
            Self::NoIdentity => "Waiting for the inverter to report its identity".into(),
            Self::CaptureNotSaved => {
                "Couldn't save the charge settings to put back, so not charging".into()
            }
            Self::ForeignInverter => {
                "Connected to a different inverter from the one being topped up".into()
            }
        }
    }
}

/// The top-up's state in `AppState`, under one lock with its run.
#[derive(Debug, Clone, Default)]
pub struct DispatchChargeShared {
    /// Mirrors `Settings::dispatch_charge_run`.
    pub run: Option<DispatchChargeRun>,
    /// Why it is sitting out this smart charge, if it is.
    pub reason: Option<Reason>,
    /// The charge settings aren't reading back after repeated attempts.
    pub failing: bool,
    /// The failure has been alerted during this dispatch.
    pub notified: bool,
    /// What to put back by hand, set when a restore was given up and taken
    /// once by the poll loop to alert.
    pub gave_up: Option<String>,
    /// Set at shutdown once the restore is queued: nothing more is written.
    pub stopped: bool,
    /// A top-up reached its target during this smart charge.
    pub charged_this_dispatch: bool,
}

impl DispatchChargeShared {
    /// True once per dispatch, the first time the charge is failing.
    pub fn take_notification(&mut self) -> bool {
        let due = self.failing && !self.notified;
        if due {
            self.notified = true;
        }
        due
    }

    pub fn charging(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|run| run.phase == DispatchChargePhase::Charging)
    }

    /// A status word for the Octopus page.
    pub fn state_code(&self) -> &'static str {
        match self.run.as_ref().map(|run| run.phase) {
            Some(DispatchChargePhase::Charging) if self.failing => "failing",
            Some(DispatchChargePhase::Charging) => "charging",
            Some(DispatchChargePhase::Restoring) => "restoring",
            None if self.reason.is_some() => "standing_aside",
            None => "idle",
        }
    }
}

/// The smart charge now: when it ends (taking in any smart dispatch that
/// carries straight on from it), and that end as a host-local minute of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchWindow {
    pub end: DateTime<Utc>,
    pub end_host_minute: u16,
}

/// What the poll loop knows this poll.
#[derive(Debug, Clone, Copy)]
pub struct Inputs {
    /// The option is on and the hold is wanted.
    pub enabled: bool,
    /// The smart dispatch running now by a fresh list, outside a sit-out.
    pub dispatch: Option<DispatchWindow>,
    /// Restarted mid-charge and the first dispatch check hasn't finished:
    /// keep things as they are rather than restore and charge again.
    pub waiting_for_list: bool,
    /// The hold is holding this dispatch.
    pub hold_holding: bool,
    /// The inverter isn't taking the hold's pause.
    pub hold_failing: bool,
    /// Another charge owner, by priority.
    pub blocker: Option<Reason>,
    /// A charge slot of the user's is running now. Only counts before a run
    /// starts: once running, the slot is the top-up's own.
    pub charge_slot_running: bool,
    /// The charge cap's level, when the cap is on.
    pub cap_soc: Option<u8>,
    pub host_minute: u16,
    pub inverter_minute: u16,
}

/// One poll's result.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Step {
    pub writes: Vec<RegisterWrite>,
    /// The run changed and must be persisted; a new run must be on disk
    /// before its writes go out.
    pub persist: bool,
    /// A run was created this poll.
    pub started: bool,
}

/// Cosy or Agile automation owns the charge slots, so a top-up never starts.
/// Shared by the poll loop and the Planner so they can't disagree.
pub fn tariff_automation_owns_slots(settings: &crate::settings::Settings) -> bool {
    settings.cosy_enabled || settings.agile_scope != crate::settings::AgileScope::Off
}

/// The SOC a top-up charges to.
pub fn target_soc(cap_soc: Option<u8>) -> u8 {
    cap_soc.map_or(100, |cap| cap.clamp(4, 100))
}

fn hhmm(minute_of_day: i32) -> u16 {
    let minute = minute_of_day.rem_euclid(1440) as u16;
    (minute / 60) * 100 + minute % 60
}

/// Inverter minus host minute of day, in -720..720.
fn skew(inverter_minute: u16, host_minute: u16) -> i32 {
    (i32::from(inverter_minute) - i32::from(host_minute) + 720).rem_euclid(1440) - 720
}

fn registers(writes: &[CalibrationWrite]) -> Vec<RegisterWrite> {
    writes.iter().map(RegisterWrite::from).collect()
}

/// The charge settings: slot 1 over the window, then Force Charge to the
/// target (and slot 1's own target on 10-slot models, when it is lower).
pub(crate) fn charge_writes(
    device: DeviceType,
    start_hhmm: u16,
    end_hhmm: u16,
    target: u8,
    revert: &ForceChargeRevert,
) -> Result<Vec<RegisterWrite>, String> {
    let mut writes =
        charge_slot_command_for_device(device, 1, true, start_hhmm, end_hhmm)?.encode()?;
    let target16 = u16::from(target);
    let force = if device.uses_three_phase_schedule_slots() {
        ControlCommand::ThreePhaseForceCharge {
            target_soc: target16,
        }
    } else {
        ControlCommand::ForceCharge {
            target_soc: target16,
        }
    };
    writes.extend(force.encode()?);
    if device.uses_extended_schedule_slots()
        && revert
            .charge_slot_1_target_soc
            .is_some_and(|slot_target| slot_target < target)
    {
        writes.extend(
            ControlCommand::SetChargeTargetSocSlot {
                slot: 1,
                soc: target16,
            }
            .encode()?,
        );
    }
    Ok(writes)
}

fn restore_writes(run: &DispatchChargeRun) -> Vec<RegisterWrite> {
    build_force_charge_stop_writes(run.revert.device_type, &run.revert)
}

fn attempt_due(run: &DispatchChargeRun, now: i64) -> bool {
    run.unconfirmed < FAST_ATTEMPTS || now < run.last_sent || now - run.last_sent >= RETRY_SECS
}

/// Why a top-up can't start now, before anything is captured.
fn start_blocker(
    snap: &InverterSnapshot,
    inputs: &Inputs,
    target: u8,
    charged_this_dispatch: bool,
) -> Option<Reason> {
    if inputs.hold_failing {
        return Some(Reason::HoldFailing);
    }
    if !inputs.hold_holding {
        // The hold leaves a smart charge alone while a charge slot is
        // charging the battery; say so, as the hold's own note does.
        return Some(if inputs.charge_slot_running {
            Reason::ChargeSlot
        } else {
            Reason::HoldNotHolding
        });
    }
    if snap.battery_power_mode != 1 || snap.enable_discharge {
        // The hold's pause to plain Eco hasn't read back yet: a baseline
        // captured now would put Timed Export back over the hold's restore.
        return Some(Reason::HoldNotHolding);
    }
    if let Some(reason) = inputs.blocker {
        return Some(reason);
    }
    let full = if charged_this_dispatch {
        snap.soc > target.saturating_sub(RESTART_MARGIN)
    } else {
        snap.soc >= target
    };
    if full {
        return Some(Reason::Full);
    }
    if inputs.charge_slot_running {
        return Some(Reason::ChargeSlot);
    }
    if snap.inverter_serial.is_empty() {
        return Some(Reason::NoIdentity);
    }
    None
}

fn begin_restore(run: &mut DispatchChargeRun, now: i64) -> Vec<RegisterWrite> {
    run.phase = DispatchChargePhase::Restoring;
    run.restore_started = Some(now);
    run.unconfirmed = 1;
    run.last_sent = now;
    restore_writes(run)
}

/// What to put back by hand when a restore is given up.
fn describe_restore(revert: &ForceChargeRevert) -> String {
    let slot = match (revert.charge_slot_1_start, revert.charge_slot_1_end) {
        (Some((sh, sm)), Some((eh, em))) => {
            format!("charge slot 1 {sh:02}:{sm:02}–{eh:02}:{em:02}")
        }
        _ => "charge slot 1 cleared".to_string(),
    };
    let charging = if revert.enable_charge { "on" } else { "off" };
    let target = if revert.enable_charge_target {
        format!("{}%", revert.target_soc)
    } else {
        "100%".to_string()
    };
    format!("{slot}, scheduled charging {charging}, charge target {target}")
}

/// Run one poll's step.
pub fn step(shared: &mut DispatchChargeShared, snap: &InverterSnapshot, inputs: Inputs) -> Step {
    let now = snap.timestamp;
    let target = target_soc(inputs.cap_soc);
    if shared.stopped {
        return Step::default();
    }
    if inputs.dispatch.is_none() && !inputs.waiting_for_list {
        // The dispatch is over: the next one may alert again, and top up
        // again.
        shared.notified = false;
        shared.charged_this_dispatch = false;
    }

    let Some(run) = shared.run.as_ref() else {
        shared.failing = false;
        shared.reason = None;
        let Some(window) = inputs.dispatch.filter(|_| inputs.enabled) else {
            return Step::default();
        };
        if let Some(reason) = start_blocker(snap, &inputs, target, shared.charged_this_dispatch) {
            if shared.reason != Some(reason) {
                tracing::debug!(reason = reason.code(), "Dispatch charge: standing aside");
            }
            shared.reason = Some(reason);
            return Step::default();
        }
        let Some(mut revert) = force_charge_revert_from_snapshot(snap, snap.device_type) else {
            shared.reason = Some(Reason::NoIdentity);
            return Step::default();
        };
        revert.started_at_ms = now.saturating_mul(1000);
        let skew_minutes = skew(inputs.inverter_minute, inputs.host_minute);
        let start = hhmm(i32::from(inputs.inverter_minute));
        let end = hhmm(i32::from(window.end_host_minute) + skew_minutes);
        if start == end {
            // Less than a minute left: nothing worth charging for.
            return Step::default();
        }
        let writes = match charge_writes(snap.device_type, start, end, target, &revert) {
            Ok(writes) => writes,
            Err(error) => {
                tracing::warn!("Dispatch charge: couldn't build the charge settings: {error}");
                return Step::default();
            }
        };
        tracing::info!(
            target,
            "Dispatch charge: smart charging running, topping the battery up"
        );
        shared.run = Some(DispatchChargeRun {
            phase: DispatchChargePhase::Charging,
            revert,
            target_soc: target,
            writes: writes.iter().map(CalibrationWrite::from).collect(),
            dispatch_end: window.end,
            skew_minutes,
            last_sent: now,
            unconfirmed: 1,
            restore_started: None,
        });
        return Step {
            writes,
            persist: true,
            started: true,
        };
    };

    if !snap.inverter_serial.is_empty() && snap.inverter_serial != run.revert.inverter_serial {
        // Nothing is written to an inverter the baseline wasn't read from.
        shared.reason = Some(Reason::ForeignInverter);
        return Step::default();
    }
    if snap.inverter_serial.is_empty() {
        return Step::default();
    }
    let mut run = shared.run.take().expect("checked above");
    let (step, keep) = match run.phase {
        DispatchChargePhase::Charging => step_charging(shared, &mut run, snap, &inputs, target),
        DispatchChargePhase::Restoring => step_restoring(shared, &mut run, snap),
    };
    if keep {
        shared.run = Some(run);
    }
    step
}

fn step_charging(
    shared: &mut DispatchChargeShared,
    run: &mut DispatchChargeRun,
    snap: &InverterSnapshot,
    inputs: &Inputs,
    target: u8,
) -> (Step, bool) {
    let now = snap.timestamp;
    shared.reason = None;
    if inputs.waiting_for_list && inputs.dispatch.is_none() {
        return (Step::default(), true);
    }
    let charged = snap.soc >= run.target_soc.min(target);
    let stop_reason = if !inputs.enabled {
        Some("switched off")
    } else if inputs.dispatch.is_none() {
        Some("smart charging ended")
    } else if !inputs.hold_holding || inputs.hold_failing {
        Some("the hold isn't holding")
    } else if inputs.blocker.is_some() {
        Some("another owner wants the battery")
    } else if charged {
        Some("the battery is charged")
    } else {
        None
    };
    if let Some(why) = stop_reason {
        tracing::info!("Dispatch charge: {why}, restoring the charge settings");
        shared.failing = false;
        shared.reason = inputs.blocker;
        shared.charged_this_dispatch |= charged;
        let writes = begin_restore(run, now);
        return (
            Step {
                writes,
                persist: true,
                started: false,
            },
            true,
        );
    }
    let Some(window) = inputs.dispatch else {
        return (Step::default(), true);
    };
    let mut moved = false;
    if window.end != run.dispatch_end {
        // Moved or extended (#404): the slot follows it.
        let end = hhmm(i32::from(window.end_host_minute) + run.skew_minutes);
        for write in &mut run.writes {
            if write.address == crate::modbus::registers::HR_CHARGE_SLOT_1_END
                || write.address == crate::modbus::registers::HR_3PH_CHARGE_SLOT_1_END
            {
                write.value = end;
            }
        }
        run.dispatch_end = window.end;
        run.unconfirmed = 0;
        moved = true;
    }
    let writes = registers(&run.writes);
    if !moved && snapshot_matches_writes(snap, &writes) {
        let persist = run.unconfirmed != 0;
        run.unconfirmed = 0;
        shared.failing = false;
        return (
            Step {
                persist,
                ..Step::default()
            },
            true,
        );
    }
    if run.unconfirmed >= FAST_ATTEMPTS && !shared.failing {
        shared.failing = true;
        tracing::warn!(
            "Dispatch charge: the inverter isn't taking the charge settings; retrying every 5 minutes"
        );
    }
    if moved || attempt_due(run, now) {
        run.unconfirmed = run.unconfirmed.saturating_add(1);
        run.last_sent = now;
        return (
            Step {
                writes,
                persist: true,
                started: false,
            },
            true,
        );
    }
    (Step::default(), true)
}

fn step_restoring(
    shared: &mut DispatchChargeShared,
    run: &mut DispatchChargeRun,
    snap: &InverterSnapshot,
) -> (Step, bool) {
    let now = snap.timestamp;
    let done = Step {
        persist: true,
        ..Step::default()
    };
    let writes = restore_writes(run);
    if now > run.last_sent && snapshot_matches_writes(snap, &writes) {
        tracing::info!("Dispatch charge: charge settings restored");
        shared.failing = false;
        return (done, false);
    }
    let started = match run.restore_started {
        Some(started) if started <= now => started,
        _ => {
            run.restore_started = Some(now);
            now
        }
    };
    if now - started >= RESTORE_GIVE_UP_SECS {
        tracing::warn!(
            "Dispatch charge: the inverter didn't take the restore in 15 minutes; giving up"
        );
        shared.gave_up = Some(describe_restore(&run.revert));
        shared.failing = false;
        return (done, false);
    }
    if attempt_due(run, now) {
        run.unconfirmed = run.unconfirmed.saturating_add(1);
        run.last_sent = now;
        return (
            Step {
                writes,
                persist: true,
                started: false,
            },
            true,
        );
    }
    (Step::default(), true)
}

/// Take a write someone else made during a run into its baseline, so the
/// restore puts back their value rather than the one from before. True when
/// the baseline changed.
pub fn absorb(run: &mut DispatchChargeRun, write: &RegisterWrite) -> bool {
    use crate::modbus::registers::*;
    let before = run.revert.clone();
    let revert = &mut run.revert;
    let on = write.value != 0;
    let time = |value: u16| -> Option<(u8, u8)> {
        let (hour, minute) = ((value / 100) as u8, (value % 100) as u8);
        (hour < 24 && minute < 60).then_some((hour, minute))
    };
    match write.address {
        HR_ENABLE_CHARGE => revert.enable_charge = on,
        HR_ENABLE_CHARGE_TARGET => revert.enable_charge_target = on,
        HR_ENABLE_DISCHARGE => revert.enable_discharge = on,
        HR_BATTERY_POWER_MODE => revert.battery_power_mode = write.value.min(1) as u8,
        HR_CHARGE_TARGET_SOC | HR_3PH_CHARGE_TARGET_SOC if (4..=100).contains(&write.value) => {
            revert.target_soc = write.value as u8;
        }
        HR_CHARGE_TARGET_SOC_1 if (4..=100).contains(&write.value) => {
            revert.charge_slot_1_target_soc = Some(write.value as u8);
        }
        HR_CHARGE_SLOT_1_START | HR_3PH_CHARGE_SLOT_1_START => {
            revert.charge_slot_1_start = time(write.value);
        }
        HR_CHARGE_SLOT_1_END | HR_3PH_CHARGE_SLOT_1_END => {
            revert.charge_slot_1_end = time(write.value);
        }
        HR_3PH_FORCE_CHARGE_ENABLE => revert.three_phase_force_charge_enable = Some(on),
        HR_3PH_AC_CHARGE_ENABLE => revert.three_phase_ac_charge_enable = Some(on),
        _ => {}
    }
    run.revert != before
}

/// Persist the run.
pub async fn persist_run(run: Option<DispatchChargeRun>) -> Result<(), String> {
    crate::settings::Settings::update_async(move |s| s.dispatch_charge_run = run).await
}

/// How long shutdown waits for the restore to be taken.
pub const SHUTDOWN_RESTORE_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

/// On a graceful shutdown, put the charge settings back so the user's own
/// charge slot isn't left replaced while HEM is away. With the inverter
/// unreachable the run stays on disk and the next start finishes it.
pub async fn restore_on_shutdown(
    state: &std::sync::Arc<crate::inverter::poll::AppState>,
    limit: std::time::Duration,
) {
    let writes = {
        let mut guard = state.dispatch_charge.lock().await;
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
            tracing::info!("Dispatch charge: charge settings restored on shutdown");
            let mut guard = state.dispatch_charge.lock().await;
            match persist_run(None).await {
                Ok(()) => guard.run = None,
                Err(e) => tracing::warn!("Dispatch charge: couldn't clear its run: {e}"),
            }
        }
        _ => tracing::warn!(
            "Dispatch charge: couldn't restore the charge settings on shutdown; they're put back when HEM starts again"
        ),
    }
}

/// The end of the smart charge running at `now`, carried on through any
/// smart dispatch that starts by the time the previous one ends.
pub fn running_dispatch_end(
    dispatches: &[crate::octopus_dispatches::Dispatch],
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let mut end = dispatches
        .iter()
        .filter(|dispatch| dispatch.holds_at(now))
        .map(|dispatch| dispatch.end)
        .max()?;
    loop {
        let next = dispatches
            .iter()
            .filter(|d| d.is_smart() && d.start <= end && d.end > end)
            .map(|d| d.end)
            .max();
        match next {
            Some(later) => end = later,
            None => return Some(end),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::{BatteryMode, ScheduleSlot};
    use crate::modbus::registers::*;
    use crate::server::api::tests::snapshot_decoding_writes;

    const T0: i64 = 1_800_000_000;
    const MIN: i64 = 60;

    fn utc(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(seconds, 0).unwrap()
    }

    fn slot(start: (u8, u8), end: (u8, u8)) -> ScheduleSlot {
        ScheduleSlot {
            enabled: true,
            start_hour: start.0,
            start_minute: start.1,
            end_hour: end.0,
            end_minute: end.1,
            target_soc: 4,
        }
    }

    /// An AC-coupled inverter the hold has paused at 14:00, with an
    /// overnight charge slot to 80%.
    fn held() -> InverterSnapshot {
        let mut snap = InverterSnapshot {
            device_type: DeviceType::ACCoupled,
            inverter_serial: "SN1".to_string(),
            firmware_version: "200".to_string(),
            timestamp: T0,
            soc: 50,
            battery_reserve: 100,
            battery_mode: BatteryMode::EcoPaused,
            battery_power_mode: 1,
            enable_charge: true,
            enable_charge_target: true,
            target_soc: 80,
            enable_discharge: false,
            ..Default::default()
        };
        snap.charge_slots[0] = slot((23, 30), (5, 30));
        snap
    }

    /// A smart charge from 14:00 to 15:00 on both clocks.
    fn inputs() -> Inputs {
        Inputs {
            enabled: true,
            dispatch: Some(DispatchWindow {
                end: utc(T0 + 60 * MIN),
                end_host_minute: 15 * 60,
            }),
            waiting_for_list: false,
            hold_holding: true,
            hold_failing: false,
            blocker: None,
            charge_slot_running: false,
            cap_soc: None,
            host_minute: 14 * 60,
            inverter_minute: 14 * 60,
        }
    }

    fn value(writes: &[RegisterWrite], address: u16) -> Option<u16> {
        writes
            .iter()
            .rev()
            .find(|w| w.address == address)
            .map(|w| w.value)
    }

    fn after(base: &InverterSnapshot, writes: &[RegisterWrite], at: i64) -> InverterSnapshot {
        snapshot_decoding_writes(base, writes, at)
    }

    /// Started at T0; returns the state and the inverter after the writes.
    fn charging() -> (DispatchChargeShared, InverterSnapshot) {
        let mut shared = DispatchChargeShared::default();
        let step = step(&mut shared, &held(), inputs());
        assert!(step.started);
        (shared, after(&held(), &step.writes, T0 + MIN))
    }

    // ---- starting ----

    #[test]
    fn starts_with_a_slot_over_the_dispatch_and_force_charge_to_100() {
        let mut shared = DispatchChargeShared::default();
        let step = step(&mut shared, &held(), inputs());
        assert!(step.started && step.persist);
        assert_eq!(value(&step.writes, HR_CHARGE_SLOT_1_START), Some(1400));
        assert_eq!(value(&step.writes, HR_CHARGE_SLOT_1_END), Some(1500));
        assert_eq!(value(&step.writes, HR_ENABLE_CHARGE), Some(1));
        assert_eq!(value(&step.writes, HR_ENABLE_CHARGE_TARGET), Some(0));
        assert_eq!(value(&step.writes, HR_CHARGE_TARGET_SOC), Some(100));
        // The slot goes before charging is enabled.
        let slot_at = step
            .writes
            .iter()
            .position(|w| w.address == HR_CHARGE_SLOT_1_START)
            .unwrap();
        let enable_at = step
            .writes
            .iter()
            .position(|w| w.address == HR_ENABLE_CHARGE)
            .unwrap();
        assert!(slot_at < enable_at);
        // Never the reserve, a rate or the pause register.
        for address in [
            HR_BATTERY_SOC_RESERVE,
            HR_AC_BATTERY_CHARGE_LIMIT,
            HR_BATTERY_CHARGE_LIMIT,
            HR_BATTERY_PAUSE_MODE,
        ] {
            assert_eq!(value(&step.writes, address), None, "{address}");
        }
        assert!(shared.charging());
        assert_eq!(shared.state_code(), "charging");
    }

    #[test]
    fn the_baseline_is_the_settings_from_before_the_first_write() {
        let (shared, _) = charging();
        let revert = &shared.run.as_ref().unwrap().revert;
        assert!(revert.enable_charge && revert.enable_charge_target);
        assert_eq!(revert.target_soc, 80);
        assert_eq!(revert.charge_slot_1_start, Some((23, 30)));
        assert_eq!(revert.charge_slot_1_end, Some((5, 30)));
        assert_eq!(revert.inverter_serial, "SN1");
    }

    #[test]
    fn it_only_starts_while_the_hold_holds_with_nothing_else_charging() {
        let cases: Vec<(Inputs, InverterSnapshot, Reason)> = vec![
            (
                Inputs {
                    hold_holding: false,
                    ..inputs()
                },
                held(),
                Reason::HoldNotHolding,
            ),
            (
                Inputs {
                    hold_failing: true,
                    ..inputs()
                },
                held(),
                Reason::HoldFailing,
            ),
            (
                Inputs {
                    blocker: Some(Reason::ManualForce),
                    ..inputs()
                },
                held(),
                Reason::ManualForce,
            ),
            (
                Inputs {
                    charge_slot_running: true,
                    ..inputs()
                },
                held(),
                Reason::ChargeSlot,
            ),
            (
                inputs(),
                InverterSnapshot { soc: 100, ..held() },
                Reason::Full,
            ),
            (
                inputs(),
                InverterSnapshot {
                    inverter_serial: String::new(),
                    ..held()
                },
                Reason::NoIdentity,
            ),
        ];
        for (inputs, snap, reason) in cases {
            let mut shared = DispatchChargeShared::default();
            assert_eq!(step(&mut shared, &snap, inputs), Step::default());
            assert!(shared.run.is_none());
            assert_eq!(shared.reason, Some(reason));
            assert_eq!(shared.state_code(), "standing_aside");
        }
    }

    #[test]
    fn it_waits_for_the_hold_s_pause_to_eco_to_read_back() {
        // The hold went to Holding this poll, but the snapshot predates its
        // writes: the inverter still reads as Timed Export (or Timed Demand).
        for (power_mode, discharge) in [(0, true), (1, true), (0, false)] {
            let snap = InverterSnapshot {
                battery_power_mode: power_mode,
                enable_discharge: discharge,
                battery_mode: BatteryMode::from_registers(power_mode.into(), discharge, 4),
                battery_reserve: 4,
                ..held()
            };
            let mut shared = DispatchChargeShared::default();
            assert_eq!(step(&mut shared, &snap, inputs()), Step::default());
            assert!(shared.run.is_none());
            assert_eq!(shared.reason, Some(Reason::HoldNotHolding));
        }
        // An All-in-One's hold pauses through HR 318 and leaves the reserve
        // alone: plain Eco without reserve 100 still starts.
        let aio = InverterSnapshot {
            battery_reserve: 4,
            battery_mode: BatteryMode::Eco,
            ..held()
        };
        let mut shared = DispatchChargeShared::default();
        assert!(step(&mut shared, &aio, inputs()).started);
    }

    #[test]
    fn a_hold_sitting_out_for_a_charge_slot_says_the_slot_is_charging() {
        let mut shared = DispatchChargeShared::default();
        let sat_out = Inputs {
            hold_holding: false,
            charge_slot_running: true,
            ..inputs()
        };
        assert_eq!(step(&mut shared, &held(), sat_out), Step::default());
        assert_eq!(shared.reason, Some(Reason::ChargeSlot));
    }

    #[test]
    fn the_charge_cap_pausing_inside_a_slot_reads_as_full_not_as_the_slot() {
        let mut shared = DispatchChargeShared::default();
        let capped = Inputs {
            charge_slot_running: true,
            cap_soc: Some(80),
            ..inputs()
        };
        let snap = InverterSnapshot { soc: 80, ..held() };
        assert_eq!(step(&mut shared, &snap, capped), Step::default());
        assert_eq!(shared.reason, Some(Reason::Full));
    }

    #[test]
    fn a_small_dip_after_reaching_the_target_doesn_t_start_again() {
        let (mut shared, snap) = charging();
        // Charged to 100: the restore goes out and is confirmed.
        let full = InverterSnapshot {
            soc: 100,
            timestamp: T0 + 10 * MIN,
            ..snap
        };
        let restore = step(&mut shared, &full, inputs());
        assert!(!restore.writes.is_empty());
        let restored = after(&full, &restore.writes, T0 + 11 * MIN);
        step(&mut shared, &restored, inputs());
        assert!(shared.run.is_none());

        // A point or four below doesn't restart it in the same dispatch.
        for soc in [99, 96] {
            let dipped = InverterSnapshot {
                soc,
                timestamp: T0 + 12 * MIN,
                ..restored.clone()
            };
            assert_eq!(step(&mut shared, &dipped, inputs()), Step::default());
            assert_eq!(shared.reason, Some(Reason::Full));
        }
        // A real fall of the margin does.
        let fallen = InverterSnapshot {
            soc: 100 - RESTART_MARGIN,
            timestamp: T0 + 13 * MIN,
            ..restored.clone()
        };
        assert!(step(&mut shared, &fallen, inputs()).started);
    }

    #[test]
    fn the_next_dispatch_tops_up_from_one_point_below() {
        let (mut shared, snap) = charging();
        let full = InverterSnapshot {
            soc: 100,
            timestamp: T0 + 10 * MIN,
            ..snap
        };
        let restore = step(&mut shared, &full, inputs());
        let restored = after(&full, &restore.writes, T0 + 11 * MIN);
        step(&mut shared, &restored, inputs());
        // The dispatch ends; a later one starts.
        let between = Inputs {
            dispatch: None,
            ..inputs()
        };
        step(&mut shared, &restored, between);
        let next = InverterSnapshot {
            soc: 99,
            timestamp: T0 + 120 * MIN,
            ..restored
        };
        assert!(step(&mut shared, &next, inputs()).started);
    }

    #[test]
    fn nothing_happens_without_a_smart_charge_or_with_the_option_off() {
        for inputs in [
            Inputs {
                dispatch: None,
                ..inputs()
            },
            Inputs {
                enabled: false,
                ..inputs()
            },
        ] {
            let mut shared = DispatchChargeShared::default();
            assert_eq!(step(&mut shared, &held(), inputs), Step::default());
            assert!(shared.run.is_none() && shared.reason.is_none());
            assert_eq!(shared.state_code(), "idle");
        }
    }

    #[test]
    fn the_charge_cap_is_the_ceiling() {
        let mut shared = DispatchChargeShared::default();
        let capped = Inputs {
            cap_soc: Some(90),
            ..inputs()
        };
        let step_ = step(&mut shared, &held(), capped);
        assert_eq!(value(&step_.writes, HR_ENABLE_CHARGE_TARGET), Some(1));
        assert_eq!(value(&step_.writes, HR_CHARGE_TARGET_SOC), Some(90));
        // At the cap there is nothing to do.
        let mut shared = DispatchChargeShared::default();
        let full = InverterSnapshot { soc: 90, ..held() };
        assert_eq!(step(&mut shared, &full, capped), Step::default());
        assert_eq!(shared.reason, Some(Reason::Full));
    }

    #[test]
    fn the_slot_is_on_the_inverter_clock() {
        // Inverter ten minutes ahead of the host.
        let mut shared = DispatchChargeShared::default();
        let skewed = Inputs {
            inverter_minute: 14 * 60 + 10,
            ..inputs()
        };
        let step = step(&mut shared, &held(), skewed);
        assert_eq!(value(&step.writes, HR_CHARGE_SLOT_1_START), Some(1410));
        assert_eq!(value(&step.writes, HR_CHARGE_SLOT_1_END), Some(1510));
    }

    #[test]
    fn a_dispatch_over_midnight_wraps_the_slot() {
        let mut shared = DispatchChargeShared::default();
        let late = Inputs {
            host_minute: 23 * 60,
            inverter_minute: 23 * 60,
            dispatch: Some(DispatchWindow {
                end: utc(T0 + 120 * MIN),
                end_host_minute: 60,
            }),
            ..inputs()
        };
        let step = step(&mut shared, &held(), late);
        assert_eq!(value(&step.writes, HR_CHARGE_SLOT_1_START), Some(2300));
        assert_eq!(value(&step.writes, HR_CHARGE_SLOT_1_END), Some(100));
    }

    #[test]
    fn ten_slot_models_raise_slot_1_s_own_target() {
        let mut snap = InverterSnapshot {
            device_type: DeviceType::Gen3Hybrid,
            raw_charge_slot_1_target_soc: Some(80),
            ..held()
        };
        snap.charge_slots[0] = slot((23, 30), (5, 30));
        let mut shared = DispatchChargeShared::default();
        let step = step(&mut shared, &snap, inputs());
        assert_eq!(value(&step.writes, HR_CHARGE_TARGET_SOC_1), Some(100));
        assert_eq!(
            shared.run.unwrap().revert.charge_slot_1_target_soc,
            Some(80)
        );
    }

    #[test]
    fn three_phase_uses_its_own_registers() {
        let snap = InverterSnapshot {
            device_type: DeviceType::ThreePhase,
            ..held()
        };
        let mut shared = DispatchChargeShared::default();
        let step = step(&mut shared, &snap, inputs());
        assert_eq!(value(&step.writes, HR_3PH_CHARGE_SLOT_1_START), Some(1400));
        assert_eq!(value(&step.writes, HR_3PH_CHARGE_SLOT_1_END), Some(1500));
        assert_eq!(value(&step.writes, HR_3PH_FORCE_CHARGE_ENABLE), Some(1));
        assert_eq!(value(&step.writes, HR_ENABLE_CHARGE), None);
    }

    // ---- while charging ----

    #[test]
    fn settings_that_read_back_are_left_alone() {
        let (mut shared, snap) = charging();
        assert_eq!(step(&mut shared, &snap, inputs()).writes, Vec::new());
        let later = InverterSnapshot {
            timestamp: T0 + 2 * MIN,
            ..snap
        };
        assert_eq!(step(&mut shared, &later, inputs()).writes, Vec::new());
        assert!(!shared.failing);
    }

    #[test]
    fn settings_that_dont_read_back_are_retried_then_backed_off_and_alerted_once() {
        let (mut shared, _) = charging();
        let refused = |at: i64| InverterSnapshot {
            timestamp: at,
            ..held()
        };
        // Second attempt at once.
        assert!(!step(&mut shared, &refused(T0 + MIN), inputs())
            .writes
            .is_empty());
        // Then backed off, and failing.
        assert!(step(&mut shared, &refused(T0 + 2 * MIN), inputs())
            .writes
            .is_empty());
        assert!(shared.failing);
        assert_eq!(shared.state_code(), "failing");
        assert!(shared.take_notification());
        assert!(!shared.take_notification());
        // Every five minutes after that.
        assert!(!step(&mut shared, &refused(T0 + 6 * MIN), inputs())
            .writes
            .is_empty());
        assert!(!shared.take_notification(), "one alert per dispatch");
    }

    #[test]
    fn a_moved_or_extended_dispatch_moves_the_slot_end() {
        let (mut shared, snap) = charging();
        let extended = Inputs {
            dispatch: Some(DispatchWindow {
                end: utc(T0 + 90 * MIN),
                end_host_minute: 15 * 60 + 30,
            }),
            ..inputs()
        };
        let step_ = step(&mut shared, &snap, extended);
        assert!(step_.persist);
        assert_eq!(value(&step_.writes, HR_CHARGE_SLOT_1_END), Some(1530));
        assert_eq!(value(&step_.writes, HR_CHARGE_SLOT_1_START), Some(1400));
        assert_eq!(
            shared.run.as_ref().unwrap().dispatch_end,
            utc(T0 + 90 * MIN)
        );
    }

    #[test]
    fn each_way_of_ending_restores_the_baseline_once() {
        let ends: Vec<(Inputs, Option<u8>)> = vec![
            (
                Inputs {
                    dispatch: None,
                    ..inputs()
                },
                None,
            ),
            (
                Inputs {
                    enabled: false,
                    ..inputs()
                },
                None,
            ),
            (
                Inputs {
                    hold_holding: false,
                    ..inputs()
                },
                None,
            ),
            (
                Inputs {
                    blocker: Some(Reason::Calibration),
                    ..inputs()
                },
                None,
            ),
            (inputs(), Some(100)),
        ];
        for (ending, soc) in ends {
            let (mut shared, mut snap) = charging();
            if let Some(soc) = soc {
                snap.soc = soc;
            }
            let restore = step(&mut shared, &snap, ending);
            assert_eq!(value(&restore.writes, HR_CHARGE_SLOT_1_START), Some(2330));
            assert_eq!(value(&restore.writes, HR_CHARGE_SLOT_1_END), Some(530));
            assert_eq!(value(&restore.writes, HR_CHARGE_TARGET_SOC), Some(80));
            assert_eq!(value(&restore.writes, HR_ENABLE_CHARGE_TARGET), Some(1));
            assert_eq!(shared.state_code(), "restoring");
            assert!(!shared.charging());
            // A fresh read showing it back ends the run, once.
            let back = after(&snap, &restore.writes, T0 + 2 * MIN);
            let done = step(&mut shared, &back, ending);
            assert!(done.persist && done.writes.is_empty());
            assert!(shared.run.is_none());
            let again = InverterSnapshot {
                timestamp: T0 + 3 * MIN,
                ..back
            };
            assert!(step(&mut shared, &again, ending).writes.is_empty());
        }
    }

    #[test]
    fn a_read_from_before_the_restore_write_proves_nothing() {
        let (mut shared, snap) = charging();
        let ended = Inputs {
            dispatch: None,
            ..inputs()
        };
        let restore = step(&mut shared, &snap, ended);
        // Same timestamp as the poll that sent it.
        let stale = after(&snap, &restore.writes, snap.timestamp);
        step(&mut shared, &stale, ended);
        assert!(shared.run.is_some());
    }

    #[test]
    fn a_restore_that_never_lands_gives_up_after_15_minutes_and_says_what_to_put_back() {
        let (mut shared, snap) = charging();
        let ended = Inputs {
            dispatch: None,
            ..inputs()
        };
        step(&mut shared, &snap, ended);
        let mut sent = 0;
        for minute in 2..=16 {
            let poll = InverterSnapshot {
                timestamp: T0 + minute * MIN,
                ..snap.clone()
            };
            if !step(&mut shared, &poll, ended).writes.is_empty() {
                sent += 1;
            }
        }
        assert!(shared.run.is_none());
        let detail = shared.gave_up.take().unwrap();
        assert!(detail.contains("23:30–05:30"), "{detail}");
        assert!(detail.contains("80%"), "{detail}");
        assert!(sent <= 4, "backed off, sent {sent}");
    }

    #[test]
    fn a_restart_waiting_for_the_first_dispatch_check_keeps_charging() {
        let (mut shared, snap) = charging();
        let waiting = Inputs {
            dispatch: None,
            waiting_for_list: true,
            ..inputs()
        };
        assert_eq!(step(&mut shared, &snap, waiting), Step::default());
        assert!(shared.charging());
    }

    #[test]
    fn a_restart_with_the_list_gone_stale_restores() {
        let (shared, snap) = charging();
        // Round-trip the run through JSON, as a restart does.
        let json = serde_json::to_string(&shared.run).unwrap();
        let mut restarted = DispatchChargeShared {
            run: serde_json::from_str(&json).unwrap(),
            ..Default::default()
        };
        let ended = Inputs {
            dispatch: None,
            ..inputs()
        };
        let restore = step(&mut restarted, &snap, ended);
        assert_eq!(value(&restore.writes, HR_CHARGE_SLOT_1_START), Some(2330));
        drop(shared);
    }

    #[test]
    fn nothing_is_written_to_a_different_inverter() {
        let (mut shared, snap) = charging();
        let other = InverterSnapshot {
            inverter_serial: "SN2".to_string(),
            ..snap
        };
        let ended = Inputs {
            dispatch: None,
            ..inputs()
        };
        assert_eq!(step(&mut shared, &other, ended), Step::default());
        assert_eq!(shared.reason, Some(Reason::ForeignInverter));
        assert!(shared.charging());
    }

    #[test]
    fn shutdown_stops_every_write() {
        let (mut shared, snap) = charging();
        shared.stopped = true;
        let ended = Inputs {
            dispatch: None,
            ..inputs()
        };
        assert_eq!(step(&mut shared, &snap, ended), Step::default());
    }

    // ---- the user's own changes ----

    #[test]
    fn a_change_made_during_the_top_up_is_what_the_restore_puts_back() {
        let (mut shared, snap) = charging();
        let run = shared.run.as_mut().unwrap();
        for write in [
            RegisterWrite {
                address: HR_CHARGE_SLOT_1_START,
                value: 30,
            },
            RegisterWrite {
                address: HR_CHARGE_SLOT_1_END,
                value: 430,
            },
            RegisterWrite {
                address: HR_CHARGE_TARGET_SOC,
                value: 90,
            },
        ] {
            assert!(absorb(run, &write), "{write:?} changes the baseline");
        }
        // Not a charge setting, or the value already there: nothing changes,
        // so the poll loop needn't save the run.
        for write in [
            RegisterWrite {
                address: HR_BATTERY_SOC_RESERVE,
                value: 20,
            },
            RegisterWrite {
                address: HR_CHARGE_TARGET_SOC,
                value: 90,
            },
        ] {
            assert!(!absorb(run, &write), "{write:?} leaves the baseline alone");
        }
        let ended = Inputs {
            dispatch: None,
            ..inputs()
        };
        let restore = step(&mut shared, &snap, ended);
        assert_eq!(value(&restore.writes, HR_CHARGE_SLOT_1_START), Some(30));
        assert_eq!(value(&restore.writes, HR_CHARGE_SLOT_1_END), Some(430));
        assert_eq!(value(&restore.writes, HR_CHARGE_TARGET_SOC), Some(90));
        assert_eq!(value(&restore.writes, HR_BATTERY_SOC_RESERVE), None);
    }

    // ---- the dispatch window ----

    fn dispatch(start: i64, end: i64, kind: &str) -> crate::octopus_dispatches::Dispatch {
        crate::octopus_dispatches::Dispatch {
            start: utc(T0 + start * MIN),
            end: utc(T0 + end * MIN),
            kind: Some(kind.to_string()),
            energy_added_kwh: None,
        }
    }

    #[test]
    fn back_to_back_smart_dispatches_run_as_one() {
        let list = vec![
            dispatch(0, 30, "SMART"),
            dispatch(30, 60, "SMART"),
            dispatch(60, 90, "BOOST"),
            dispatch(120, 150, "SMART"),
        ];
        assert_eq!(
            running_dispatch_end(&list, utc(T0 + 10 * MIN)),
            Some(utc(T0 + 60 * MIN))
        );
    }

    #[test]
    fn a_boost_or_a_future_dispatch_is_not_a_window() {
        let list = vec![dispatch(0, 30, "BOOST"), dispatch(60, 90, "SMART")];
        assert_eq!(running_dispatch_end(&list, utc(T0 + 10 * MIN)), None);
        assert_eq!(running_dispatch_end(&list, utc(T0 + 30 * MIN)), None);
    }
}
