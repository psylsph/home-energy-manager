//! Guided battery calibration cycle (issue #418).
//!
//! BMS firmware 3007–3022 mis-estimates state of charge, and a full cycle is
//! what usually brings it back into line. The inverter's own HR 29
//! calibration is hidden for BMS firmware 3000 and up, so HEM drives the
//! cycle itself:
//!
//! 1. **Discharging**: export at full rate with the reserve at
//!    [`FLOOR_SOC`] until the battery reaches it (or stops discharging near
//!    it, see [`STALL_SOC`]).
//! 2. **Charging**: charge at full rate to 100%.
//! 3. **Holding**: keep charging at 100% for the chosen hold time so the
//!    cells balance.
//! 4. **Restoring**: put back every register the cycle changed, confirmed
//!    from a snapshot read after the writes.
//!
//! Each phase is a fixed list of register writes, built once at start from
//! the same snapshot the restore values come from ([`build_plan`]). Every
//! poll, [`step`] checks whether the current phase's writes read back and
//! re-applies them (at most every [`REAPPLY_AFTER_MS`]) when they do not, so
//! a restart, a dropped write or a safety limiter that paused the battery in
//! between all heal the same way. The run is persisted after every change.
//!
//! The thermal and load limiters still outrank the cycle: while one holds,
//! the cycle writes nothing and waits. Cosy, Agile, Timed Export, Adaptive
//! Charge, auto-winter, plan auto-apply, the floor guard and the Intelligent
//! Octopus Go dispatch hold stay out of the way until it ends.

use serde::{Deserialize, Serialize};

use crate::inverter::encoder::{ControlCommand, RegisterWrite};
use crate::inverter::model::{DeviceType, InverterSnapshot, PowerLimitBank};
use crate::modbus::registers::{
    HR_BATTERY_PAUSE_MODE, HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START,
    HR_DISCHARGE_SLOT_2_END, HR_DISCHARGE_SLOT_2_START,
};
use crate::server::api::{
    build_force_charge_stop_writes, build_force_discharge_stop_writes,
    force_charge_revert_from_snapshot, force_discharge_pause_mode_supported,
    force_discharge_revert_from_snapshot, snapshot_matches_writes,
};

/// The reserve the battery is exported down to.
pub const FLOOR_SOC: u8 = 4;
/// Default time at 100% for the cells to balance.
pub const DEFAULT_HOLD_MINUTES: u32 = 120;
/// Shortest hold the user can choose.
pub const MIN_HOLD_MINUTES: u32 = 30;
/// Longest hold the user can choose.
pub const MAX_HOLD_MINUTES: u32 = 480;
/// Minimum gap between two attempts to apply the same phase's writes.
pub const REAPPLY_AFTER_MS: i64 = 5 * 60_000;
/// A discharge or charge that has not finished in this long gives up and
/// restores the settings.
pub const PHASE_TIMEOUT_MS: i64 = 24 * 3_600_000;
/// Restore attempts before the cycle reports that it could not put the
/// settings back.
pub const RESTORE_ATTEMPTS: u32 = 6;
/// At or below this SOC, a battery that has stopped discharging counts as
/// empty: some BMSs stop a few points above the reserve.
pub const STALL_SOC: u8 = 10;
/// Discharge power at or below this, in watts, counts as stopped.
pub const STALL_POWER_W: i32 = 50;
/// How long the battery must stay stopped near the floor to count as empty.
pub const STALL_MS: i64 = 15 * 60_000;

/// One register write, in the form the run is persisted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationWrite {
    pub address: u16,
    pub value: u16,
}

impl From<&RegisterWrite> for CalibrationWrite {
    fn from(write: &RegisterWrite) -> Self {
        Self {
            address: write.address,
            value: write.value,
        }
    }
}

impl From<&CalibrationWrite> for RegisterWrite {
    fn from(write: &CalibrationWrite) -> Self {
        Self {
            address: write.address,
            value: write.value,
        }
    }
}

/// How a cycle ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum CalibrationOutcome {
    Completed,
    Cancelled,
    Failed { reason: String },
}

/// Where a running cycle is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum CalibrationPhase {
    Discharging,
    Charging,
    Holding {
        since_ms: i64,
    },
    /// Putting the settings back; `outcome` is reported once they read back.
    Restoring {
        outcome: CalibrationOutcome,
        attempts: u32,
    },
}

/// A running cycle, persisted in settings so it survives a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationRun {
    /// Identity of the inverter the cycle was started on. Nothing is written
    /// to any other inverter.
    pub device_type: DeviceType,
    pub inverter_serial: String,
    pub started_at_ms: i64,
    pub phase_started_at_ms: i64,
    pub hold_minutes: u32,
    pub phase: CalibrationPhase,
    /// When the current phase's writes were last issued; `None` until the
    /// first attempt in this phase.
    #[serde(default)]
    pub last_applied_ms: Option<i64>,
    /// Since when the battery has sat stopped near the floor.
    #[serde(default)]
    pub low_since_ms: Option<i64>,
    pub discharge_writes: Vec<CalibrationWrite>,
    pub charge_writes: Vec<CalibrationWrite>,
    pub restore_writes: Vec<CalibrationWrite>,
}

/// The last finished cycle, kept for the Control page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationRecord {
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub outcome: CalibrationOutcome,
}

/// The register writes for each phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalibrationPlan {
    pub discharge: Vec<CalibrationWrite>,
    pub charge: Vec<CalibrationWrite>,
    pub restore: Vec<CalibrationWrite>,
}

/// What one poll's [`step`] decided.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CalibrationStep {
    /// Writes to issue now, in order.
    pub writes: Vec<RegisterWrite>,
    /// The run changed and must be persisted.
    pub changed: bool,
    /// The cycle is over; the caller clears the run and records this.
    pub finished: Option<CalibrationOutcome>,
}

/// Whether HEM can run the cycle on this model: single-phase inverters with
/// a low-voltage battery (Gen 1–3 hybrids and AC-coupled). The All-in-Ones
/// use the HV battery protocol and are left out until someone can test one.
pub fn supports(device_type: DeviceType) -> bool {
    let _ = device_type;
    false
}

/// Validate a requested hold time.
pub fn validate_hold_minutes(minutes: u32) -> Result<u32, String> {
    if (MIN_HOLD_MINUTES..=MAX_HOLD_MINUTES).contains(&minutes) {
        Ok(minutes)
    } else {
        Err(format!(
            "The hold at 100% must be between {} minutes and {} hours",
            MIN_HOLD_MINUTES,
            MAX_HOLD_MINUTES / 60
        ))
    }
}

fn encode(command: ControlCommand) -> Result<Vec<RegisterWrite>, String> {
    command.encode()
}

/// The (charge, discharge) rate that means "no limit" on a bank.
fn full_rates(bank: PowerLimitBank) -> (u16, u16) {
    match bank {
        // DC hybrids: 50 = 0.5C, the highest value HR 111/112 accept.
        PowerLimitBank::HalfScale => (50, 50),
        PowerLimitBank::AcBank | PowerLimitBank::ThreePhase => (100, 100),
    }
}

fn rate_writes(
    bank: PowerLimitBank,
    charge: Option<u16>,
    discharge: Option<u16>,
) -> Result<Vec<RegisterWrite>, String> {
    let mut writes = Vec::new();
    if let Some(limit) = charge {
        writes.extend(encode(match bank {
            PowerLimitBank::HalfScale => ControlCommand::SetChargeLimit { limit },
            PowerLimitBank::AcBank => ControlCommand::SetAcChargeLimit { limit },
            PowerLimitBank::ThreePhase => ControlCommand::SetThreePhaseChargeLimit { limit },
        })?);
    }
    if let Some(limit) = discharge {
        writes.extend(encode(match bank {
            PowerLimitBank::HalfScale => ControlCommand::SetDischargeLimit { limit },
            PowerLimitBank::AcBank => ControlCommand::SetAcDischargeLimit { limit },
            PowerLimitBank::ThreePhase => ControlCommand::SetThreePhaseDischargeLimit { limit },
        })?);
    }
    Ok(writes)
}

/// Keep only the last write to each register, in order. The Force Charge and
/// Force Discharge restores share a few registers; both were captured from
/// the same snapshot, and the later (charge) one restores the exact power
/// mode where the discharge stop would always return to Eco.
fn last_write_per_register(writes: Vec<RegisterWrite>) -> Vec<RegisterWrite> {
    let mut kept: Vec<RegisterWrite> = Vec::with_capacity(writes.len());
    for write in writes.into_iter().rev() {
        if !kept.iter().any(|w| w.address == write.address) {
            kept.push(write);
        }
    }
    kept.reverse();
    kept
}

fn persisted(writes: &[RegisterWrite]) -> Vec<CalibrationWrite> {
    writes.iter().map(CalibrationWrite::from).collect()
}

/// Build the phase writes and the restore writes from one snapshot.
///
/// Refuses when anything the cycle changes could not be written back as it
/// was, so a cycle never starts that it could not undo.
pub fn build_plan(snapshot: &InverterSnapshot) -> Result<CalibrationPlan, String> {
    let _ = snapshot;
    Err("not implemented".to_string())
}

/// Start a run from a plan.
pub fn new_run(
    snapshot: &InverterSnapshot,
    plan: CalibrationPlan,
    hold_minutes: u32,
    now_ms: i64,
) -> CalibrationRun {
    CalibrationRun {
        device_type: snapshot.device_type,
        inverter_serial: snapshot.inverter_serial.clone(),
        started_at_ms: now_ms,
        phase_started_at_ms: now_ms,
        hold_minutes,
        phase: CalibrationPhase::Discharging,
        last_applied_ms: None,
        low_since_ms: None,
        discharge_writes: plan.discharge,
        charge_writes: plan.charge,
        restore_writes: plan.restore,
    }
}

/// Advance the cycle for one poll.
pub fn step(
    run: &mut CalibrationRun,
    snapshot: &InverterSnapshot,
    now_ms: i64,
    safety_holding: bool,
) -> CalibrationStep {
    let _ = (run, snapshot, now_ms, safety_holding);
    CalibrationStep::default()
}

fn registers(writes: &[CalibrationWrite]) -> Vec<RegisterWrite> {
    writes.iter().map(RegisterWrite::from).collect()
}

fn failed(reason: String) -> CalibrationOutcome {
    CalibrationOutcome::Failed { reason }
}

fn enter(run: &mut CalibrationRun, phase: CalibrationPhase, now_ms: i64) {
    run.phase = phase;
    run.phase_started_at_ms = now_ms;
    run.last_applied_ms = None;
    run.low_since_ms = None;
}

fn begin_restore(run: &mut CalibrationRun, outcome: CalibrationOutcome, now_ms: i64) {
    enter(
        run,
        CalibrationPhase::Restoring {
            outcome,
            attempts: 0,
        },
        now_ms,
    );
}

/// Whether the battery has sat stopped near the floor, with the discharge
/// settings in place, for [`STALL_MS`].
fn stalled_near_floor(
    run: &mut CalibrationRun,
    snapshot: &InverterSnapshot,
    now_ms: i64,
    out: &mut CalibrationStep,
) -> bool {
    let stopped = snapshot.soc <= STALL_SOC
        && snapshot.battery_power <= STALL_POWER_W
        && snapshot_matches_writes(snapshot, &registers(&run.discharge_writes));
    if !stopped {
        if run.low_since_ms.take().is_some() {
            out.changed = true;
        }
        return false;
    }
    let since = match run.low_since_ms {
        Some(since) => since,
        None => {
            run.low_since_ms = Some(now_ms);
            out.changed = true;
            now_ms
        }
    };
    now_ms - since >= STALL_MS
}

/// Ask a running cycle to stop and restore. Returns false when it is
/// already restoring.
pub fn cancel(run: &mut CalibrationRun, now_ms: i64) -> bool {
    let _ = (run, now_ms);
    false
}

/// Run one poll's step for the cycle in `state`, if any: persist the run
/// when it changed, and on the last step clear it, record the outcome and
/// notify. Returns the writes to issue now. `safety_holding` is true when a
/// higher owner (a thermal or load limiter) holds discharge this poll.
///
/// The run lock is held while the run is persisted, so a cancel from the API
/// (which persists under the same lock) can never be overwritten by an older
/// copy of the run.
pub async fn advance(
    state: &std::sync::Arc<crate::inverter::poll::AppState>,
    snapshot: &InverterSnapshot,
    now_ms: i64,
    safety_holding: bool,
) -> Vec<RegisterWrite> {
    let mut guard = state.calibration_run.lock().await;
    let Some(run) = guard.as_mut() else {
        return Vec::new();
    };
    let started_at_ms = run.started_at_ms;
    let step = step(run, snapshot, now_ms, safety_holding);

    if let Some(outcome) = step.finished {
        let record = CalibrationRecord {
            started_at_ms,
            finished_at_ms: now_ms,
            outcome,
        };
        let saved = record.clone();
        if let Err(e) = crate::settings::Settings::update_async(move |s| {
            s.battery_calibration_run = None;
            s.battery_calibration_last = Some(saved);
        })
        .await
        {
            tracing::warn!("Failed to persist the finished battery calibration: {e}");
        }
        guard.take();
        drop(guard);
        log_and_notify(state, &record);
        return Vec::new();
    }

    if step.changed {
        let saved = guard.clone();
        if let Err(e) =
            crate::settings::Settings::update_async(move |s| s.battery_calibration_run = saved)
                .await
        {
            tracing::warn!("Failed to persist the battery calibration: {e}");
        }
    }
    step.writes
}

/// Persist a run (or clear it) under the caller's run lock.
pub async fn persist_run(run: Option<CalibrationRun>) -> Result<(), String> {
    crate::settings::Settings::update_async(move |s| s.battery_calibration_run = run).await
}

/// The text sent when a cycle ends.
pub fn finished_message(outcome: &CalibrationOutcome) -> String {
    match outcome {
        CalibrationOutcome::Completed => "🔋 HEM battery calibration finished. The battery was \
            emptied, charged to 100% and held while the cells balanced, and your battery \
            settings are back as they were."
            .to_string(),
        CalibrationOutcome::Cancelled => "🔋 HEM battery calibration cancelled. Your battery \
            settings are back as they were."
            .to_string(),
        CalibrationOutcome::Failed { reason } => {
            format!("⚠️ HEM battery calibration stopped. {reason}")
        }
    }
}

fn log_and_notify(
    state: &std::sync::Arc<crate::inverter::poll::AppState>,
    record: &CalibrationRecord,
) {
    let text = finished_message(&record.outcome);
    match &record.outcome {
        CalibrationOutcome::Failed { reason } => {
            tracing::warn!("Battery calibration stopped: {reason}")
        }
        outcome => tracing::info!(?outcome, "Battery calibration ended"),
    }
    // Delivery can take seconds; never hold up the poll for it.
    let state = state.clone();
    tokio::spawn(async move {
        crate::alerts::send_text_notification(&state, &text, "battery-calibration").await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::ScheduleSlot;
    use crate::modbus::registers::*;
    use crate::server::api::tests::snapshot_decoding_writes;

    const T0: i64 = 1_800_000_000_000;
    const MIN: i64 = 60_000;

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

    /// A Gen 2 hybrid on Eco with an overnight charge slot to 80%, a
    /// teatime export slot armed, a 20% reserve and limited rates.
    fn baseline() -> InverterSnapshot {
        let mut snap = InverterSnapshot {
            device_type: DeviceType::Gen2Hybrid,
            inverter_serial: "SN1".to_string(),
            firmware_version: "449".to_string(),
            timestamp: T0 / 1000,
            soc: 60,
            battery_reserve: 20,
            charge_rate: 25,
            discharge_rate: 30,
            battery_power_mode: 1,
            enable_charge: true,
            enable_charge_target: true,
            target_soc: 80,
            enable_discharge: false,
            ..Default::default()
        };
        snap.charge_slots[0] = slot((0, 30), (4, 30));
        snap.discharge_slots[0] = slot((16, 0), (19, 0));
        snap
    }

    fn plan() -> CalibrationPlan {
        build_plan(&baseline()).expect("plan")
    }

    fn regs(writes: &[CalibrationWrite]) -> Vec<RegisterWrite> {
        writes.iter().map(RegisterWrite::from).collect()
    }

    fn value_of(writes: &[CalibrationWrite], address: u16) -> Option<u16> {
        writes
            .iter()
            .rev()
            .find(|w| w.address == address)
            .map(|w| w.value)
    }

    fn running() -> CalibrationRun {
        new_run(&baseline(), plan(), DEFAULT_HOLD_MINUTES, T0)
    }

    /// The snapshot the inverter would report at `now_ms` after `writes`.
    fn after(base: &InverterSnapshot, writes: &[RegisterWrite], now_ms: i64) -> InverterSnapshot {
        snapshot_decoding_writes(base, writes, now_ms / 1000)
    }

    fn at(base: &InverterSnapshot, now_ms: i64, soc: u8, battery_power: i32) -> InverterSnapshot {
        let mut snap = base.clone();
        snap.timestamp = now_ms / 1000;
        snap.soc = soc;
        snap.battery_power = battery_power;
        snap
    }

    // ---- model support and hold time ----

    #[test]
    fn supports_single_phase_lv_models_only() {
        for supported in [
            DeviceType::Gen1Hybrid,
            DeviceType::Gen2Hybrid,
            DeviceType::Gen3Hybrid,
            DeviceType::Gen3PlusHybrid,
            DeviceType::PolarHybrid,
            DeviceType::ACCoupled,
            DeviceType::ACCoupledMk2,
        ] {
            assert!(supports(supported), "{supported:?}");
            assert!(!supported.uses_hv_battery(), "{supported:?} is HV");
            assert!(
                !supported.uses_three_phase_schedule_slots(),
                "{supported:?}"
            );
        }
        for unsupported in [
            DeviceType::AllInOne6kW,
            DeviceType::AllInOne3_6kW,
            DeviceType::AllInOne5kW,
            DeviceType::ThreePhase,
            DeviceType::ACThreePhase,
            DeviceType::HybridHvGen3,
            DeviceType::AllInOneHybrid,
            DeviceType::Gen4Hybrid,
            DeviceType::Gateway,
            DeviceType::Ems,
            DeviceType::PvInverter,
            DeviceType::Unknown(0x4101),
        ] {
            assert!(!supports(unsupported), "{unsupported:?}");
        }
    }

    #[test]
    fn hold_time_is_half_an_hour_to_eight_hours() {
        assert_eq!(validate_hold_minutes(DEFAULT_HOLD_MINUTES), Ok(120));
        assert_eq!(validate_hold_minutes(30), Ok(30));
        assert_eq!(validate_hold_minutes(480), Ok(480));
        assert!(validate_hold_minutes(29).is_err());
        assert!(validate_hold_minutes(481).is_err());
        assert!(validate_hold_minutes(0).is_err());
    }

    // ---- plan ----

    #[test]
    fn discharge_exports_at_full_rate_down_to_the_floor_all_day() {
        let plan = plan();
        let d = &plan.discharge;
        assert_eq!(value_of(d, HR_BATTERY_SOC_RESERVE), Some(FLOOR_SOC as u16));
        // DC hybrid: 50 is the no-limit value of HR 112.
        assert_eq!(value_of(d, HR_BATTERY_DISCHARGE_LIMIT), Some(50));
        assert_eq!(value_of(d, HR_BATTERY_POWER_MODE), Some(0));
        assert_eq!(value_of(d, HR_ENABLE_DISCHARGE), Some(1));
        assert_eq!(value_of(d, HR_ENABLE_CHARGE), Some(0));
        assert_eq!(value_of(d, HR_DISCHARGE_SLOT_1_START), Some(0));
        assert_eq!(value_of(d, HR_DISCHARGE_SLOT_1_END), Some(2359));
        // The slot is written before discharge is armed.
        let slot_at = d.iter().position(|w| w.address == HR_DISCHARGE_SLOT_1_END);
        let arm_at = d.iter().position(|w| w.address == HR_ENABLE_DISCHARGE);
        assert!(slot_at < arm_at, "{d:?}");
        assert_eq!(value_of(d, HR_BATTERY_CHARGE_LIMIT), None);
    }

    #[test]
    fn charge_runs_at_full_rate_to_100_all_day() {
        let plan = plan();
        let c = &plan.charge;
        assert_eq!(value_of(c, HR_BATTERY_CHARGE_LIMIT), Some(50));
        assert_eq!(value_of(c, HR_CHARGE_SLOT_1_START), Some(0));
        assert_eq!(value_of(c, HR_CHARGE_SLOT_1_END), Some(2359));
        assert_eq!(value_of(c, HR_ENABLE_CHARGE), Some(1));
        assert_eq!(value_of(c, HR_CHARGE_TARGET_SOC), Some(100));
        assert_eq!(value_of(c, HR_BATTERY_POWER_MODE), Some(1));
        assert_eq!(value_of(c, HR_ENABLE_DISCHARGE), Some(0));
    }

    #[test]
    fn ac_coupled_rates_use_the_ac_bank_at_100() {
        let mut snap = baseline();
        snap.device_type = DeviceType::ACCoupled;
        snap.charge_rate = 60;
        snap.discharge_rate = 70;
        let plan = build_plan(&snap).expect("plan");
        assert_eq!(
            value_of(&plan.discharge, HR_AC_BATTERY_DISCHARGE_LIMIT),
            Some(100)
        );
        assert_eq!(
            value_of(&plan.charge, HR_AC_BATTERY_CHARGE_LIMIT),
            Some(100)
        );
        assert_eq!(
            value_of(&plan.restore, HR_AC_BATTERY_DISCHARGE_LIMIT),
            Some(70)
        );
        assert_eq!(
            value_of(&plan.restore, HR_AC_BATTERY_CHARGE_LIMIT),
            Some(60)
        );
        assert_eq!(value_of(&plan.discharge, HR_BATTERY_DISCHARGE_LIMIT), None);
    }

    #[test]
    fn ten_slot_models_raise_slot_1_target_to_100_and_put_it_back() {
        let mut snap = baseline();
        snap.device_type = DeviceType::Gen3Hybrid;
        snap.raw_charge_slot_1_target_soc = Some(85);
        let plan = build_plan(&snap).expect("plan");
        assert_eq!(value_of(&plan.charge, HR_CHARGE_TARGET_SOC_1), Some(100));
        assert_eq!(value_of(&plan.restore, HR_CHARGE_TARGET_SOC_1), Some(85));
    }

    #[test]
    fn restore_puts_every_changed_setting_back() {
        let base = baseline();
        let plan = plan();
        let calibrated = after(&base, &regs(&plan.discharge), T0);
        let calibrated = after(&calibrated, &regs(&plan.charge), T0 + MIN);
        let restored = after(&calibrated, &regs(&plan.restore), T0 + 2 * MIN);

        assert_eq!(restored.battery_reserve, 20);
        assert_eq!(restored.charge_rate, 25);
        assert_eq!(restored.discharge_rate, 30);
        assert_eq!(restored.battery_power_mode, 1);
        assert!(restored.enable_charge);
        assert!(restored.enable_charge_target);
        assert_eq!(restored.target_soc, 80);
        assert!(!restored.enable_discharge);
        assert_eq!(restored.charge_slots[0].start_hour, 0);
        assert_eq!(restored.charge_slots[0].start_minute, 30);
        assert_eq!(restored.charge_slots[0].end_hour, 4);
        assert_eq!(restored.discharge_slots[0].start_hour, 16);
        assert_eq!(restored.discharge_slots[0].end_hour, 19);
        assert!(!restored.discharge_slots[1].enabled);
        assert!(crate::server::api::snapshot_matches_writes(
            &restored,
            &regs(&plan.restore)
        ));
    }

    #[test]
    fn restore_keeps_an_armed_export_mode() {
        // Timed Export armed before the cycle: HR 27 = 0 and discharge
        // enabled. The Force Discharge stop alone would return to Eco.
        let mut base = baseline();
        base.battery_power_mode = 0;
        base.enable_discharge = true;
        let plan = build_plan(&base).expect("plan");
        assert_eq!(value_of(&plan.restore, HR_BATTERY_POWER_MODE), Some(0));
        assert_eq!(value_of(&plan.restore, HR_ENABLE_DISCHARGE), Some(1));
        // Each register is restored once, so the order is unambiguous.
        let mut addresses: Vec<u16> = plan.restore.iter().map(|w| w.address).collect();
        let count = addresses.len();
        addresses.sort_unstable();
        addresses.dedup();
        assert_eq!(addresses.len(), count, "{:?}", plan.restore);
    }

    #[test]
    fn a_battery_pause_is_lifted_for_the_cycle_and_put_back() {
        let mut base = baseline();
        base.device_type = DeviceType::Gen3Hybrid;
        base.firmware_version = "320".to_string();
        assert!(base.device_type.supports_pause_registers(320));
        base.battery_pause_mode_raw = Some(2);
        base.battery_pause_slot_start_raw = Some(1600);
        base.battery_pause_slot_end_raw = Some(1900);
        let plan = build_plan(&base).expect("plan");
        assert_eq!(value_of(&plan.discharge, HR_BATTERY_PAUSE_MODE), Some(0));
        assert_eq!(value_of(&plan.charge, HR_BATTERY_PAUSE_MODE), Some(0));
        assert_eq!(value_of(&plan.restore, HR_BATTERY_PAUSE_MODE), Some(2));
    }

    #[test]
    fn plan_refuses_what_it_could_not_put_back() {
        let mut snap = baseline();
        snap.inverter_serial.clear();
        assert!(build_plan(&snap).is_err(), "no identity");

        let mut snap = baseline();
        snap.device_type = DeviceType::ThreePhase;
        assert!(build_plan(&snap).is_err(), "unsupported model");

        // HR 111 only accepts 0-50: a corrupt 60 could not be written back.
        let mut snap = baseline();
        snap.charge_rate = 60;
        assert!(build_plan(&snap).is_err(), "unwritable charge rate");

        let mut snap = baseline();
        snap.battery_reserve = 101;
        assert!(build_plan(&snap).is_err(), "unwritable reserve");
    }

    // ---- step: discharging ----

    #[test]
    fn first_poll_applies_the_discharge_writes() {
        let mut run = running();
        let step = step(&mut run, &at(&baseline(), T0, 60, 0), T0, false);
        assert_eq!(step.writes, regs(&run.discharge_writes));
        assert!(step.changed);
        assert_eq!(run.last_applied_ms, Some(T0));
        assert_eq!(run.phase, CalibrationPhase::Discharging);
    }

    #[test]
    fn writes_that_have_not_read_back_are_retried_after_five_minutes() {
        let mut run = running();
        let base = baseline();
        step(&mut run, &at(&base, T0, 60, 0), T0, false);

        // Still the old settings a minute later: not yet.
        let quiet = step(&mut run, &at(&base, T0 + MIN, 60, 0), T0 + MIN, false);
        assert!(quiet.writes.is_empty());

        let retry = step(
            &mut run,
            &at(&base, T0 + 5 * MIN, 60, 0),
            T0 + 5 * MIN,
            false,
        );
        assert_eq!(retry.writes, regs(&run.discharge_writes));
        assert_eq!(run.last_applied_ms, Some(T0 + 5 * MIN));
    }

    #[test]
    fn writes_that_read_back_are_left_alone() {
        let mut run = running();
        let applied = after(&baseline(), &regs(&run.discharge_writes), T0);
        step(&mut run, &at(&applied, T0, 60, 3000), T0, false);
        let later = step(
            &mut run,
            &at(&applied, T0 + 30 * MIN, 40, 3000),
            T0 + 30 * MIN,
            false,
        );
        assert!(later.writes.is_empty());
        assert!(!later.changed);
    }

    #[test]
    fn settings_changed_mid_cycle_are_put_back() {
        // A restart, a limiter release or another app returned to Eco.
        let mut run = running();
        let applied = after(&baseline(), &regs(&run.discharge_writes), T0);
        step(&mut run, &at(&applied, T0, 60, 3000), T0, false);
        let mut drifted = at(&applied, T0 + 10 * MIN, 50, 0);
        drifted.battery_power_mode = 1;
        let step = step(&mut run, &drifted, T0 + 10 * MIN, false);
        assert_eq!(step.writes, regs(&run.discharge_writes));
    }

    #[test]
    fn reaching_the_floor_starts_charging_in_the_same_poll() {
        let mut run = running();
        let applied = after(&baseline(), &regs(&run.discharge_writes), T0);
        step(&mut run, &at(&applied, T0, 60, 3000), T0, false);
        let now = T0 + 3 * 60 * MIN;
        let step = step(&mut run, &at(&applied, now, FLOOR_SOC, 0), now, false);
        assert_eq!(run.phase, CalibrationPhase::Charging);
        assert_eq!(run.phase_started_at_ms, now);
        assert_eq!(step.writes, regs(&run.charge_writes));
        assert!(step.changed);
    }

    #[test]
    fn an_empty_battery_at_start_goes_straight_to_charging() {
        let mut run = running();
        let step = step(&mut run, &at(&baseline(), T0, 3, 0), T0, false);
        assert_eq!(run.phase, CalibrationPhase::Charging);
        assert_eq!(step.writes, regs(&run.charge_writes));
    }

    #[test]
    fn a_battery_that_stops_just_above_the_floor_counts_as_empty() {
        let mut run = running();
        let applied = after(&baseline(), &regs(&run.discharge_writes), T0);
        step(&mut run, &at(&applied, T0, 60, 3000), T0, false);

        let stop = T0 + 120 * MIN;
        step(&mut run, &at(&applied, stop, 7, 0), stop, false);
        assert_eq!(run.phase, CalibrationPhase::Discharging);
        step(
            &mut run,
            &at(&applied, stop + 14 * MIN, 7, 20),
            stop + 14 * MIN,
            false,
        );
        assert_eq!(run.phase, CalibrationPhase::Discharging);
        step(
            &mut run,
            &at(&applied, stop + 15 * MIN, 7, 0),
            stop + 15 * MIN,
            false,
        );
        assert_eq!(run.phase, CalibrationPhase::Charging);
    }

    #[test]
    fn a_pause_above_the_stall_level_or_a_restart_of_discharge_does_not_count() {
        let mut run = running();
        let applied = after(&baseline(), &regs(&run.discharge_writes), T0);
        step(&mut run, &at(&applied, T0, 60, 3000), T0, false);

        // Stopped at 25%: not near enough the floor.
        let t = T0 + 60 * MIN;
        step(&mut run, &at(&applied, t, 25, 0), t, false);
        step(
            &mut run,
            &at(&applied, t + 30 * MIN, 25, 0),
            t + 30 * MIN,
            false,
        );
        assert_eq!(run.phase, CalibrationPhase::Discharging);

        // Stopped at 8%, then discharging again before 15 minutes: restart.
        let t = T0 + 180 * MIN;
        step(&mut run, &at(&applied, t, 8, 0), t, false);
        step(
            &mut run,
            &at(&applied, t + 10 * MIN, 8, 1500),
            t + 10 * MIN,
            false,
        );
        step(
            &mut run,
            &at(&applied, t + 20 * MIN, 7, 0),
            t + 20 * MIN,
            false,
        );
        assert_eq!(run.phase, CalibrationPhase::Discharging);
        assert_eq!(run.low_since_ms, Some(t + 20 * MIN));
    }

    #[test]
    fn a_stall_only_counts_while_the_discharge_settings_are_in_place() {
        // Not discharging because the settings never landed is not empty.
        let mut run = running();
        let base = baseline();
        step(&mut run, &at(&base, T0, 8, 0), T0, false);
        step(
            &mut run,
            &at(&base, T0 + 20 * MIN, 8, 0),
            T0 + 20 * MIN,
            false,
        );
        assert_eq!(run.phase, CalibrationPhase::Discharging);
    }

    // ---- step: charging and holding ----

    fn charging() -> (CalibrationRun, InverterSnapshot) {
        let mut run = running();
        run.phase = CalibrationPhase::Charging;
        run.phase_started_at_ms = T0;
        run.last_applied_ms = Some(T0);
        let applied = after(&baseline(), &regs(&run.charge_writes), T0);
        (run, applied)
    }

    #[test]
    fn reaching_100_starts_the_hold() {
        let (mut run, applied) = charging();
        let now = T0 + 4 * 60 * MIN;
        step(
            &mut run,
            &at(&applied, now - MIN, 99, -2500),
            now - MIN,
            false,
        );
        assert_eq!(run.phase, CalibrationPhase::Charging);
        let s = step(&mut run, &at(&applied, now, 100, -300), now, false);
        assert_eq!(run.phase, CalibrationPhase::Holding { since_ms: now });
        assert!(s.writes.is_empty(), "charge settings stay in place");
        assert!(s.changed);
    }

    #[test]
    fn the_hold_lasts_the_chosen_time_then_restores() {
        let (mut run, applied) = charging();
        run.hold_minutes = 90;
        run.phase = CalibrationPhase::Holding { since_ms: T0 };
        let s = step(
            &mut run,
            &at(&applied, T0 + 89 * MIN, 100, 0),
            T0 + 89 * MIN,
            false,
        );
        assert!(s.writes.is_empty());
        // Dropping below 100 during the hold does not restart it.
        let s = step(
            &mut run,
            &at(&applied, T0 + 60 * MIN, 99, 0),
            T0 + 60 * MIN,
            false,
        );
        assert!(s.writes.is_empty());
        assert_eq!(run.phase, CalibrationPhase::Holding { since_ms: T0 });

        let now = T0 + 90 * MIN;
        let s = step(&mut run, &at(&applied, now, 100, 0), now, false);
        assert_eq!(
            run.phase,
            CalibrationPhase::Restoring {
                outcome: CalibrationOutcome::Completed,
                attempts: 1
            }
        );
        assert_eq!(s.writes, regs(&run.restore_writes));
        assert_eq!(run.last_applied_ms, Some(now));
    }

    #[test]
    fn the_hold_keeps_the_charge_settings_in_place() {
        let (mut run, applied) = charging();
        run.phase = CalibrationPhase::Holding { since_ms: T0 };
        let mut drifted = at(&applied, T0 + 30 * MIN, 100, 0);
        drifted.enable_charge = false;
        let s = step(&mut run, &drifted, T0 + 30 * MIN, false);
        assert_eq!(s.writes, regs(&run.charge_writes));
    }

    // ---- step: restoring ----

    fn restoring(outcome: CalibrationOutcome) -> (CalibrationRun, InverterSnapshot) {
        let (mut run, applied) = charging();
        run.phase = CalibrationPhase::Restoring {
            outcome,
            attempts: 1,
        };
        run.phase_started_at_ms = T0;
        run.last_applied_ms = Some(T0);
        (run, applied)
    }

    #[test]
    fn the_cycle_ends_when_a_fresh_read_shows_the_settings_back() {
        let (mut run, applied) = restoring(CalibrationOutcome::Completed);
        let restored = after(&applied, &regs(&run.restore_writes), T0 + 30_000);
        let s = step(&mut run, &restored, T0 + 30_000, false);
        assert_eq!(s.finished, Some(CalibrationOutcome::Completed));
        assert!(s.writes.is_empty());
    }

    #[test]
    fn a_read_from_before_the_restore_writes_proves_nothing() {
        let (mut run, applied) = restoring(CalibrationOutcome::Cancelled);
        // Matching values, but read in the same second the writes went out.
        let restored = after(&applied, &regs(&run.restore_writes), T0 / 1000 * 1000);
        let s = step(&mut run, &restored, T0 + 30_000, false);
        assert_eq!(s.finished, None);
    }

    #[test]
    fn restoring_retries_then_gives_up_and_says_so() {
        let (mut run, applied) = restoring(CalibrationOutcome::Completed);
        let mut now = T0;
        for attempt in 2..=RESTORE_ATTEMPTS {
            now += REAPPLY_AFTER_MS;
            let s = step(&mut run, &at(&applied, now, 100, 0), now, false);
            assert_eq!(s.writes, regs(&run.restore_writes), "attempt {attempt}");
            assert_eq!(
                run.phase,
                CalibrationPhase::Restoring {
                    outcome: CalibrationOutcome::Completed,
                    attempts: attempt
                }
            );
        }
        now += REAPPLY_AFTER_MS;
        let s = step(&mut run, &at(&applied, now, 100, 0), now, false);
        assert!(s.writes.is_empty());
        assert!(matches!(
            s.finished,
            Some(CalibrationOutcome::Failed { .. })
        ));
    }

    // ---- step: timeouts, safety, identity ----

    #[test]
    fn a_discharge_or_charge_that_never_finishes_restores_and_fails() {
        let mut run = running();
        let applied = after(&baseline(), &regs(&run.discharge_writes), T0);
        step(&mut run, &at(&applied, T0, 60, 3000), T0, false);
        let now = T0 + PHASE_TIMEOUT_MS;
        let s = step(&mut run, &at(&applied, now, 30, 0), now, false);
        assert!(matches!(
            run.phase,
            CalibrationPhase::Restoring {
                outcome: CalibrationOutcome::Failed { .. },
                attempts: 1
            }
        ));
        assert_eq!(s.writes, regs(&run.restore_writes));

        let (mut run, applied) = charging();
        let now = T0 + PHASE_TIMEOUT_MS;
        step(&mut run, &at(&applied, now, 90, 0), now, false);
        assert!(matches!(
            run.phase,
            CalibrationPhase::Restoring {
                outcome: CalibrationOutcome::Failed { .. },
                ..
            }
        ));
    }

    #[test]
    fn a_safety_limiter_holding_discharge_stops_the_cycle_writing() {
        let mut run = running();
        let s = step(&mut run, &at(&baseline(), T0, 60, 0), T0, true);
        assert!(s.writes.is_empty());
        assert!(!s.changed);
        assert_eq!(run.last_applied_ms, None);

        // Nor does it restore under a limiter: the limiter releases first.
        let (mut run, applied) = restoring(CalibrationOutcome::Cancelled);
        let now = T0 + REAPPLY_AFTER_MS;
        let s = step(&mut run, &at(&applied, now, 50, 0), now, true);
        assert!(s.writes.is_empty());
        assert_eq!(s.finished, None);
    }

    #[test]
    fn nothing_is_written_to_a_different_inverter() {
        let mut run = running();
        let mut other = at(&baseline(), T0, 60, 0);
        other.inverter_serial = "SN2".to_string();
        let s = step(&mut run, &other, T0, false);
        assert!(s.writes.is_empty());
        assert!(matches!(
            s.finished,
            Some(CalibrationOutcome::Failed { .. })
        ));

        // Same serial but a different rate bank is not the inverter the
        // writes were built for.
        let mut run = running();
        let mut other = at(&baseline(), T0, 60, 0);
        other.device_type = DeviceType::ACCoupled;
        let s = step(&mut run, &other, T0, false);
        assert!(s.writes.is_empty());
        assert!(matches!(
            s.finished,
            Some(CalibrationOutcome::Failed { .. })
        ));
    }

    #[test]
    fn a_model_refined_on_the_same_inverter_carries_on() {
        // A Gen 2 read later refined to Gen 3 by its ARM firmware is the
        // same inverter with the same registers: never abandon it unrestored.
        let mut run = running();
        let mut refined = at(&baseline(), T0, 60, 0);
        refined.device_type = DeviceType::Gen3Hybrid;
        let s = step(&mut run, &refined, T0, false);
        assert_eq!(s.finished, None);
        assert_eq!(s.writes, regs(&run.discharge_writes));
    }

    #[test]
    fn a_snapshot_without_identity_yet_waits() {
        let mut run = running();
        let mut unknown = at(&baseline(), T0, 60, 0);
        unknown.inverter_serial.clear();
        let s = step(&mut run, &unknown, T0, false);
        assert_eq!(s, CalibrationStep::default());
    }

    // ---- cancel and persistence ----

    #[test]
    fn cancel_restores_on_the_next_poll() {
        let mut run = running();
        let applied = after(&baseline(), &regs(&run.discharge_writes), T0);
        step(&mut run, &at(&applied, T0, 60, 3000), T0, false);

        assert!(cancel(&mut run, T0 + MIN));
        assert_eq!(
            run.phase,
            CalibrationPhase::Restoring {
                outcome: CalibrationOutcome::Cancelled,
                attempts: 0
            }
        );
        assert_eq!(run.last_applied_ms, None);
        let s = step(&mut run, &at(&applied, T0 + MIN, 58, 3000), T0 + MIN, false);
        assert_eq!(s.writes, regs(&run.restore_writes));
        assert!(!cancel(&mut run, T0 + 2 * MIN), "already restoring");
    }

    #[test]
    fn a_cancelled_run_whose_settings_never_changed_ends_at_once() {
        let mut run = running();
        assert!(cancel(&mut run, T0));
        let mut untouched = baseline();
        untouched.timestamp = T0 / 1000;
        let s = step(&mut run, &untouched, T0, false);
        assert_eq!(s.finished, Some(CalibrationOutcome::Cancelled));
        assert!(s.writes.is_empty());
    }

    #[test]
    fn a_run_survives_a_restart() {
        let (mut run, _) = charging();
        run.phase = CalibrationPhase::Holding { since_ms: T0 + 5 };
        run.low_since_ms = Some(T0);
        let json = serde_json::to_string(&run).expect("serialize");
        let back: CalibrationRun = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, run);

        let failed = CalibrationRecord {
            started_at_ms: T0,
            finished_at_ms: T0 + 1,
            outcome: CalibrationOutcome::Failed {
                reason: "x".to_string(),
            },
        };
        let json = serde_json::to_string(&failed).expect("serialize");
        assert!(json.contains("\"result\":\"failed\""), "{json}");
        assert_eq!(
            serde_json::from_str::<CalibrationRecord>(&json).unwrap(),
            failed
        );
    }

    // ---- advance: the poll loop's side ----

    #[tokio::test]
    async fn advance_without_a_cycle_does_nothing() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = std::sync::Arc::new(crate::inverter::poll::AppState::new());
            assert!(advance(&state, &baseline(), T0, false).await.is_empty());
            assert_eq!(
                crate::settings::Settings::load().battery_calibration_run,
                None
            );
        })
        .await;
    }

    #[tokio::test]
    async fn advance_returns_the_writes_and_persists_the_run() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = std::sync::Arc::new(crate::inverter::poll::AppState::new());
            *state.calibration_run.lock().await = Some(running());
            let writes = advance(&state, &at(&baseline(), T0, 60, 0), T0, false).await;
            let run = state.calibration_run.lock().await.clone().expect("running");
            assert_eq!(writes, regs(&run.discharge_writes));
            assert_eq!(run.last_applied_ms, Some(T0));
            assert_eq!(
                crate::settings::Settings::load().battery_calibration_run,
                Some(run)
            );
        })
        .await;
    }

    #[tokio::test]
    async fn advance_records_the_outcome_and_clears_the_run_when_done() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = std::sync::Arc::new(crate::inverter::poll::AppState::new());
            let (run, applied) = restoring(CalibrationOutcome::Completed);
            let restored = after(&applied, &regs(&run.restore_writes), T0 + 30_000);
            *state.calibration_run.lock().await = Some(run);
            let mut settings = crate::settings::Settings::load();
            settings.battery_calibration_run = state.calibration_run.lock().await.clone();
            settings.save().expect("save");

            let writes = advance(&state, &restored, T0 + 30_000, false).await;
            assert!(writes.is_empty());
            assert!(state.calibration_run.lock().await.is_none());
            let settings = crate::settings::Settings::load();
            assert_eq!(settings.battery_calibration_run, None);
            assert_eq!(
                settings.battery_calibration_last,
                Some(CalibrationRecord {
                    started_at_ms: T0,
                    finished_at_ms: T0 + 30_000,
                    outcome: CalibrationOutcome::Completed,
                })
            );
        })
        .await;
    }

    #[tokio::test]
    async fn advance_does_not_lose_a_concurrent_settings_save() {
        // The run is written with a transactional update, never a stale
        // copy of the whole settings file.
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = std::sync::Arc::new(crate::inverter::poll::AppState::new());
            *state.calibration_run.lock().await = Some(running());
            crate::settings::Settings::update(|s| s.pv1_rated_kw = 5.5).expect("save");
            advance(&state, &at(&baseline(), T0, 60, 0), T0, false).await;
            let settings = crate::settings::Settings::load();
            assert_eq!(settings.pv1_rated_kw, 5.5);
            assert!(settings.battery_calibration_run.is_some());
        })
        .await;
    }
}
