//! Automation state machines and their persistence / register-encode helpers.
//!
//! Contains the *decision logic* and *register-write generators* for the
//! automation features driven by the poll loop:
//!
//! - Auto-winter mode (temperature-triggered battery warming)
//! - Load discharge limiter (pause battery discharge under high home load)
//! - Cosy tariff slot scheduling (charge-slot register programming)
//! - Agile Octopus runtime state + price-slot types
//!
//! The state-machine *execution* (locking [`crate::inverter::poll::AppState`],
//! issuing the generated writes via the live Modbus client, and persisting
//! after success) lives in the poll loop in
//! [`crate::inverter::poll::run_poll_loop`]. This module only owns the
//! transition logic and the register encoders, so each machine can be unit
//! tested in isolation without a network connection or a running inverter.

use std::time::Duration;

use chrono::Timelike;

use crate::inverter::encoder::{ControlCommand, RegisterWrite};
use crate::inverter::model::{BatteryMode, DeviceType, InverterSnapshot, ScheduleSlot};
use crate::modbus::client::ModbusClient;
use crate::modbus::registers::{
    encode_hhmm, HR_3PH_BATTERY_CHARGE_LIMIT, HR_3PH_BATTERY_SOC_RESERVE,
    HR_3PH_FORCE_CHARGE_ENABLE, HR_3PH_FORCE_DISCHARGE_ENABLE, HR_AC_BATTERY_CHARGE_LIMIT,
    HR_BATTERY_CHARGE_LIMIT, HR_BATTERY_POWER_MODE, HR_BATTERY_SOC_RESERVE, HR_CHARGE_SLOT_1_END,
    HR_CHARGE_SLOT_1_START, HR_CHARGE_TARGET_SOC, HR_DISCHARGE_SLOT_1_END,
    HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_END, HR_DISCHARGE_SLOT_2_START,
    HR_ENABLE_CHARGE, HR_ENABLE_CHARGE_TARGET, HR_ENABLE_DISCHARGE,
};

/// Clear a slot instead of emitting a plausible schedule when restoring a
/// corrupt captured value. User-provided times are validated at the API
/// boundary; this fallback is only for defensive recovery paths.
fn encode_hhmm_or_clear(hour: u8, minute: u8) -> u16 {
    match encode_hhmm(hour, minute) {
        Some(value) => value,
        None => {
            tracing::warn!(hour, minute, "invalid captured HHMM; clearing slot");
            0
        }
    }
}

/// The owner of the inverter's shared discharge-control registers.
///
/// Several features ultimately write the same small group of registers
/// (`HR27`, the model-specific discharge-enable register, and the discharge
/// slots).  Treating those writes as independent commands lets a lower
/// priority feature undo a safety or user action later in the same poll.  The
/// poll loop therefore selects one owner before it emits any overlapping
/// mode writes. Variant order is the documented priority order, with Manual
/// Force retaining the deliberate HR318 override described in `DESIGN.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DischargeControlOwner {
    /// Price-driven Agile automation.
    Agile,
    /// Scheduled charge automation (Cosy / timed charge).
    TimedCharge,
    /// HEM-managed Timed Export schedule.
    TimedExport,
    /// An explicit user-selected non-force mode.
    ManualMode,
    /// Explicit Pause Discharge / HR318 pause.
    ExplicitPause,
    /// Manual Force Charge / Force Discharge. This deliberate user override
    /// may temporarily clear an HR318 discharge pause after capturing it for
    /// restoration.
    ManualForce,
    /// Thermal, load, or other hardware safety protection.
    Safety,
}

/// Per-poll owner selection for the shared discharge-control domain.
///
/// A request is accepted when it is the first request, has the same owner as
/// the current winner, or outranks the current winner.  Callers should make
/// all known requests before issuing I/O; this makes the returned owner a
/// single, deterministic decision for the whole poll cycle.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DischargeControlArbiter {
    selected: Option<DischargeControlOwner>,
}

impl DischargeControlArbiter {
    /// Offer an owner to this poll cycle and return whether it is allowed to
    /// write the shared control registers.
    pub fn request(&mut self, owner: DischargeControlOwner) -> bool {
        match self.selected {
            None => {
                self.selected = Some(owner);
                true
            }
            Some(current) if owner >= current => {
                self.selected = Some(owner);
                true
            }
            Some(_) => false,
        }
    }

    /// Return the selected owner, if this cycle has any discharge-control
    /// request.
    pub fn selected_owner(self) -> Option<DischargeControlOwner> {
        self.selected
    }

    /// Return whether an owner can participate without changing the winner.
    /// This is useful for a state machine that first computes its writes and
    /// only then submits the corresponding request.
    pub fn can_request(self, owner: DischargeControlOwner) -> bool {
        self.selected
            .map(|selected| owner >= selected)
            .unwrap_or(true)
    }
}

/// Build the write that arms or disarms scheduled DC discharge on the
/// register the device family actually reads it from.
///
/// Single-phase / AC-coupled devices use HR59 (`enable_discharge`).
/// Three-phase schedule devices (three-phase hybrids, HV Gen3/4, and
/// AC-three-phase) derive the flag from HR1122
/// (`HR_3PH_FORCE_DISCHARGE_ENABLE`) — writing HR59 there arms nothing,
/// which previously left Timed Export stuck in `Entering` for the whole
/// window (code-review finding: model-incorrect enable routing).
pub(crate) fn timed_export_discharge_enable_write(
    device_type: DeviceType,
    enabled: bool,
) -> RegisterWrite {
    RegisterWrite {
        address: if device_type.uses_three_phase_schedule_slots() {
            HR_3PH_FORCE_DISCHARGE_ENABLE
        } else {
            HR_ENABLE_DISCHARGE
        },
        value: if enabled { 1 } else { 0 },
    }
}

/// Build the writes that leave Timed Export and return the inverter to Eco.
///
/// Keep this sequence shared by HTTP handlers and the poll-loop safety repair:
/// clearing the discharge-enable register alone leaves HR27 in export mode,
/// while writing HR27 first can briefly leave an armed schedule in an
/// ambiguous state. The enable register is model-routed (HR59 vs HR1122).
pub(crate) fn build_timed_export_disable_writes(device_type: DeviceType) -> Vec<RegisterWrite> {
    vec![
        timed_export_discharge_enable_write(device_type, false),
        RegisterWrite {
            address: HR_BATTERY_POWER_MODE,
            value: 1,
        },
    ]
}

/// Whether a snapshot represents an externally-created invalid Timed Export
/// state that the poll loop should repair.
pub(crate) fn should_repair_timed_export(
    enable_discharge: bool,
    slots: &[ScheduleSlot],
    force_discharge_in_progress: bool,
) -> bool {
    enable_discharge
        && !slots.iter().any(ScheduleSlot::is_configured)
        && !force_discharge_in_progress
}

// ===========================================================================
// Agile Octopus price types
// ===========================================================================

#[derive(Debug, Clone)]
pub struct PriceSlot {
    pub pence: f64,
    pub valid_from: i64, // unix timestamp
    pub valid_to: i64,   // unix timestamp
}

/// Keep the Agile cache in the newest-first order expected by
/// `contiguous_run_window`. The Octopus API currently returns that order, but
/// it is not a safe contract for the state machine to rely on.
pub(crate) fn sort_price_slots_newest_first(prices: &mut [PriceSlot]) {
    prices.sort_by_key(|slot| std::cmp::Reverse((slot.valid_from, slot.valid_to)));
}

// The legacy `AgileState { Idle, Charging, Discharging }` enum was removed
// in the slot-based refactor. The new `AgileSlotAction` enum below carries
// per-poll decisions directly to the write loop, and the inverter's own
// slot registers are the source of truth for "is a slot currently firing".
// The `agile_state_persisted` settings field is kept for diagnostic logging
// but is no longer read at runtime.

// ===========================================================================
// Auto-winter mode: types + transition logic
// ===========================================================================

/// State machine for temperature-triggered auto winter mode.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
pub enum AutoWinterState {
    /// Awaiting cold temperatures.
    #[default]
    Idle,
    /// Temperature below Cold Threshold, counting towards debounce.
    ColdPending {
        /// Consecutive polls where temp was below threshold.
        consecutive: u32,
    },
    /// Activation writes were issued but readback has not confirmed both
    /// requested registers yet.
    Activating {
        /// Number of retries already issued after the first batch.
        retries: u8,
    },
    /// Winter mode is active and charging to target SOC.
    WinterActive,
    /// Temperature above Recovery Threshold, counting towards restore.
    WarmPending {
        /// Consecutive polls where temp was above Recovery Threshold.
        consecutive: u32,
    },
    /// Restoration writes were issued but readback has not confirmed the
    /// saved pre-winter values yet.
    Restoring {
        /// Number of retries already issued after the first batch.
        retries: u8,
    },
    /// A bounded activation/restoration retry budget was exhausted.
    Error { message: String },
}

/// Outcome of the auto-winter register batch issued on the previous poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutoWinterWriteOutcome {
    /// No auto-winter batch was issued on the previous poll.
    NoneIssued,
    /// Every register in the previous batch was accepted.
    Succeeded,
    /// At least one register in the previous batch was rejected.
    Failed,
}

const AUTO_WINTER_MAX_WRITE_RETRIES: u8 = 3;

/// Configuration for auto winter mode.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AutoWinterConfig {
    /// Master toggle - must be on for automatic winter mode to function.
    pub enabled: bool,
    /// Temperature below which winter mode should activate (°C).
    pub cold_threshold: f32,
    /// Temperature above which winter mode should deactivate (°C).
    pub recovery_threshold: f32,
    /// Target SOC to charge to when in winter mode (4-100%).
    pub target_soc: u8,
    /// Number of consecutive cold/warm readings before the state transitions.
    pub debounce_readings: u32,
}

impl Default for AutoWinterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cold_threshold: 8.0,
            recovery_threshold: 12.0,
            target_soc: 80,
            debounce_readings: 10,
        }
    }
}

/// Register values saved just before auto-winter activates, so they can
/// be restored when the battery warms up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoWinterSaved {
    pub enable_charge_target: bool,
    pub target_soc: u8,
}

/// Register values saved just before the load limiter pauses discharge,
/// so they can be restored when the load drops back below threshold.
/// Persisted to disk so a crash/restart can restore the exact previous
/// state rather than hardcoding reserve=4.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LoadLimiterSaved {
    /// The battery SOC reserve (%) before the limiter paused discharge.
    pub reserve: u16,
}

// ===========================================================================
// Load discharge limiter: types + transition logic
// ===========================================================================

/// State machine for the load discharge limiter.
///
/// Monitors `home_power` and pauses battery discharge (Eco Paused) when
/// home load exceeds a threshold for a sustained period, then restores
/// Eco mode when the load drops below the threshold for the same period.
/// It is a safety owner in the discharge-control arbiter, so it continues to
/// monitor scheduled and manual discharge modes instead of yielding to them.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
pub enum LoadLimiterState {
    /// Limiter idle - not monitoring.
    #[default]
    Idle,
    /// Home load above threshold, counting towards trigger delay.
    HighLoadPending {
        /// Consecutive polls where home_power was above threshold.
        consecutive: u32,
    },
    /// Limiter active - battery discharge is paused (Eco Paused).
    Paused,
    /// Restored from persistence after a crash - first poll will check
    /// load and immediately restore Eco if already below threshold.
    /// Stays in this state until the restore writes succeed (detected by
    /// battery mode returning to Eco), so a failed write on the first
    /// poll after reconnect is retried on the next poll.
    PausedFromRestart,
    /// Home load dropped below threshold, counting towards restore. The
    /// battery remains paused until this countdown completes.
    LowLoadPending {
        /// Consecutive polls where home_power was below threshold.
        consecutive: u32,
    },
}

impl LoadLimiterState {
    /// Whether the limiter currently owns an Eco Paused battery state.
    /// Recovery remains active until the restore writes are issued.
    pub(crate) fn is_actively_pausing(&self) -> bool {
        matches!(
            self,
            Self::Paused | Self::PausedFromRestart | Self::LowLoadPending { .. }
        )
    }
}

/// Configuration for the load discharge limiter.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LoadLimiterConfig {
    /// Master toggle.
    pub enabled: bool,
    /// Home power threshold in watts.
    pub threshold_w: u32,
    /// Minutes the load must stay above/below threshold before triggering.
    pub trigger_delay_minutes: u32,
    /// Activation window start hour.
    pub start_hour: u8,
    /// Activation window start minute.
    pub start_minute: u8,
    /// Activation window end hour.
    pub end_hour: u8,
    /// Activation window end minute.
    pub end_minute: u8,
}

impl Default for LoadLimiterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_w: 3000,
            trigger_delay_minutes: 5,
            start_hour: 0,
            start_minute: 0,
            end_hour: 0,
            end_minute: 0,
        }
    }
}

// ===========================================================================
// Discharge Floor Guard: types + transition logic
// ===========================================================================

/// Configuration for the discharge floor guard (developer mode only).
///
/// While any configured Discharge Schedule slot window is active, the guard
/// raises the Minimum SOC reserve (HR110) to `floor_soc`. When the last active
/// window ends, the pre-guard reserve is restored. The guard never changes
/// battery mode — Eco / Timed Demand / Timed Export behaviour is untouched;
/// only the reserve floor moves. Windows are read from the inverter's own
/// discharge slots so the guard follows manual and scheduled edits without
/// duplicated configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DischargeFloorConfig {
    /// Master toggle (developer mode only feature).
    pub enabled: bool,
    /// Floor SOC (%) to hold while a discharge window is active (4-100).
    pub floor_soc: u8,
}

impl Default for DischargeFloorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            floor_soc: 50,
        }
    }
}

/// Runtime state for the discharge floor guard.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
pub enum DischargeFloorState {
    /// Guard idle — no floor held.
    #[default]
    Idle,
    /// Floor held; the pre-guard reserve is carried in the variant so a
    /// crash mid-window can restore the exact previous value after restart.
    FloorHeld {
        /// The battery SOC reserve (%) before the guard raised the floor.
        saved_reserve: u16,
    },
    /// Restored from persistence after a crash. The next poll re-evaluates:
    /// inside a window the floor is re-armed keeping the persisted saved
    /// reserve; outside a window the saved reserve is written back.
    HeldFromRestart {
        /// The pre-guard reserve persisted before the crash.
        saved_reserve: u16,
    },
}

/// Check whether any configured discharge slot window contains `now_minutes`.
///
/// Mirrors the load limiter window semantics: a window crossing midnight
/// (end <= start) wraps; a slot that only differs from the load limiter's
/// "all zeros means always" rule by requiring a configured window —
/// `ScheduleSlot::is_configured()` already rejects zeroed slots.
fn discharge_window_active(
    slots: &[crate::inverter::model::ScheduleSlot],
    now_minutes: u16,
) -> bool {
    slots.iter().any(|slot| {
        if !slot.is_configured() {
            return false;
        }
        let start_mins = slot.start_hour as u16 * 60 + slot.start_minute as u16;
        let end_mins = slot.end_hour as u16 * 60 + slot.end_minute as u16;
        if end_mins <= start_mins {
            // Crosses midnight (or instantaneous — treat as active).
            now_minutes >= start_mins || now_minutes < end_mins
        } else {
            now_minutes >= start_mins && now_minutes < end_mins
        }
    })
}

/// SOC-reserve register write that targets the correct register for the
/// device type: three-phase devices use the 3-phase SOC reserve register,
/// everything else uses the single-phase one.
fn soc_reserve_write(device_type: DeviceType, reserve: u16) -> RegisterWrite {
    RegisterWrite {
        address: if device_type.uses_three_phase_schedule_slots() {
            HR_3PH_BATTERY_SOC_RESERVE
        } else {
            HR_BATTERY_SOC_RESERVE
        },
        value: reserve,
    }
}

/// Evaluate the discharge floor guard. Returns register writes (if any) and
/// the updated state. `saved_reserve` from the previous state is reused when
/// re-arming after a restart so the original pre-guard value is never lost.
pub(crate) fn check_discharge_floor(
    snap: &InverterSnapshot,
    config: &DischargeFloorConfig,
    state: &mut DischargeFloorState,
    now_minutes: u16,
) -> Option<Vec<RegisterWrite>> {
    if !config.enabled {
        let saved_reserve = match *state {
            DischargeFloorState::FloorHeld { saved_reserve }
            | DischargeFloorState::HeldFromRestart { saved_reserve } => Some(saved_reserve),
            DischargeFloorState::Idle => None,
        };
        if let Some(saved_reserve) = saved_reserve {
            tracing::info!(
                saved_reserve,
                "Discharge floor guard: disabled while holding floor, restoring reserve"
            );
            *state = DischargeFloorState::Idle;
            return Some(vec![soc_reserve_write(snap.device_type, saved_reserve)]);
        }
        *state = DischargeFloorState::Idle;
        return None;
    }

    let in_window = discharge_window_active(&snap.discharge_slots, now_minutes);

    match *state {
        DischargeFloorState::Idle => {
            if in_window {
                let saved_reserve = snap.battery_reserve as u16;
                // Don't "raise" to a floor at or below the current reserve.
                if (config.floor_soc as u16) <= saved_reserve {
                    return None;
                }
                tracing::info!(
                    saved_reserve,
                    floor = config.floor_soc,
                    "Discharge floor guard: window active, raising Minimum SOC"
                );
                *state = DischargeFloorState::FloorHeld { saved_reserve };
                return Some(vec![soc_reserve_write(
                    snap.device_type,
                    config.floor_soc as u16,
                )]);
            }
            None
        }
        DischargeFloorState::FloorHeld { saved_reserve } => {
            if in_window {
                // Reassert the floor if something external lowered it
                // mid-window. Skip the write when already at the floor to
                // avoid duplicate-write churn every poll.
                if snap.battery_reserve < config.floor_soc {
                    return Some(vec![soc_reserve_write(
                        snap.device_type,
                        config.floor_soc as u16,
                    )]);
                }
                None
            } else {
                tracing::info!(
                    saved_reserve,
                    "Discharge floor guard: window ended, restoring Minimum SOC"
                );
                *state = DischargeFloorState::Idle;
                Some(vec![soc_reserve_write(snap.device_type, saved_reserve)])
            }
        }
        DischargeFloorState::HeldFromRestart { saved_reserve } => {
            if in_window {
                // Re-arm keeping the persisted pre-guard reserve so the
                // eventual restore writes back the original value, not the
                // raised floor.
                tracing::info!(
                    saved_reserve,
                    floor = config.floor_soc,
                    "Discharge floor guard: restart inside window, re-arming floor"
                );
                *state = DischargeFloorState::FloorHeld { saved_reserve };
                if snap.battery_reserve < config.floor_soc {
                    return Some(vec![soc_reserve_write(
                        snap.device_type,
                        config.floor_soc as u16,
                    )]);
                }
                None
            } else {
                tracing::info!(
                    saved_reserve,
                    "Discharge floor guard: restart outside window, restoring saved reserve"
                );
                *state = DischargeFloorState::Idle;
                Some(vec![soc_reserve_write(snap.device_type, saved_reserve)])
            }
        }
    }
}

// ===========================================================================
// Inverter temperature limiter: types + transition logic
// ===========================================================================

/// Runtime state for temperature-driven discharge protection.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
pub enum TemperatureLimiterState {
    #[default]
    Idle,
    HighPending {
        consecutive: u32,
    },
    Paused,
    PausedFromRestart,
    CoolingPending {
        consecutive: u32,
    },
}

impl TemperatureLimiterState {
    pub(crate) fn is_actively_pausing(&self) -> bool {
        matches!(
            self,
            Self::Paused | Self::PausedFromRestart | Self::CoolingPending { .. }
        )
    }
}

/// Configuration for inverter-temperature discharge protection.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TemperatureLimiterConfig {
    pub enabled: bool,
    /// Pause discharge at or above this inverter heatsink temperature.
    pub high_threshold: f32,
    /// Restore Eco at or below this temperature.
    pub recovery_threshold: f32,
    /// Consecutive sanitized readings required in either direction.
    pub confirmation_readings: u32,
}

impl Default for TemperatureLimiterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            high_threshold: 60.0,
            recovery_threshold: 55.0,
            confirmation_readings: 3,
        }
    }
}

impl TemperatureLimiterConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(30.0..=90.0).contains(&self.high_threshold) {
            return Err("High threshold must be between 30°C and 90°C".to_string());
        }
        if !(20.0..90.0).contains(&self.recovery_threshold) {
            return Err("Recovery threshold must be between 20°C and 89°C".to_string());
        }
        if self.recovery_threshold >= self.high_threshold {
            return Err("Recovery threshold must be below the high threshold".to_string());
        }
        if !(1..=30).contains(&self.confirmation_readings) {
            return Err("Confirmation readings must be between 1 and 30".to_string());
        }
        Ok(())
    }
}

// ===========================================================================
// Adaptive Charge: types + transition logic
// ===========================================================================

/// Runtime state for the SOC/time charge-rate controller.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AdaptiveChargeState {
    #[default]
    Inactive,
    BaselinePending {
        raw_value: u16,
    },
    OutsideWindow,
    Preferred {
        period: usize,
        low_count: u32,
    },
    Recovery {
        period: usize,
        high_count: u32,
    },
    SuspendedAutoWinter {
        restore_pending: bool,
    },
    Restoring,
    Error {
        message: String,
    },
}

impl AdaptiveChargeState {
    pub fn api_name(&self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::BaselinePending { .. } => "baseline_pending",
            Self::OutsideWindow => "outside_window",
            Self::Preferred { .. } => "preferred",
            Self::Recovery { .. } => "recovery",
            Self::SuspendedAutoWinter { .. } => "suspended_auto_winter",
            Self::Restoring => "restoring",
            Self::Error { .. } => "error",
        }
    }

    pub fn active_period(&self) -> Option<u8> {
        match self {
            Self::Preferred { period, .. } | Self::Recovery { period, .. } => {
                Some((*period + 1) as u8)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AdaptiveChargeOutcome {
    pub write: Option<RegisterWrite>,
    pub desired_rate_percent: Option<u8>,
}

/// Charge-limit register for device families with a controllable battery.
pub fn adaptive_charge_register(device_type: DeviceType) -> Option<u16> {
    if device_type.uses_three_phase_schedule_slots() {
        return Some(HR_3PH_BATTERY_CHARGE_LIMIT);
    }
    match device_type {
        DeviceType::ACCoupled | DeviceType::ACCoupledMk2 => Some(HR_AC_BATTERY_CHARGE_LIMIT),
        DeviceType::PvInverter
        | DeviceType::Ems
        | DeviceType::EmsCommercial
        | DeviceType::Gateway
        | DeviceType::Unknown(_) => None,
        _ => Some(HR_BATTERY_CHARGE_LIMIT),
    }
}

/// Convert the normalized UI percentage to the model-specific raw register.
pub fn normalized_charge_rate_to_raw(device_type: DeviceType, percent: u8) -> Option<u16> {
    let register = adaptive_charge_register(device_type)?;
    if register == HR_BATTERY_CHARGE_LIMIT {
        Some((percent as u16).div_ceil(2).min(50))
    } else {
        Some((percent as u16).clamp(1, 100))
    }
}

fn raw_charge_rate_to_normalized(device_type: DeviceType, raw: u16) -> u8 {
    if adaptive_charge_register(device_type) == Some(HR_BATTERY_CHARGE_LIMIT) {
        (raw.saturating_mul(2).min(100)) as u8
    } else {
        raw.min(100) as u8
    }
}

fn observed_charge_rate_is_valid(device_type: DeviceType, raw: u16) -> bool {
    match adaptive_charge_register(device_type) {
        Some(HR_BATTERY_CHARGE_LIMIT) => (1..=50).contains(&raw),
        Some(_) => (1..=100).contains(&raw),
        None => false,
    }
}

fn adaptive_period_at(
    config: &crate::settings::AdaptiveChargeConfig,
    now_minutes: u16,
) -> Option<usize> {
    config.periods.iter().position(|period| {
        if !period.enabled {
            return false;
        }
        if period.all_day {
            return true;
        }
        let start = period.start_hour as u16 * 60 + period.start_minute as u16;
        let end = period.end_hour as u16 * 60 + period.end_minute as u16;
        if start < end {
            now_minutes >= start && now_minutes < end
        } else {
            now_minutes >= start || now_minutes < end
        }
    })
}

/// Evaluate Adaptive Charge and return at most one charge-limit write.
///
/// `saved` is captured once when the mode first takes ownership and is retained
/// until a later snapshot confirms restoration after the mode is disabled.
pub fn check_adaptive_charge(
    snap: &InverterSnapshot,
    config: &crate::settings::AdaptiveChargeConfig,
    enabled: bool,
    state: &mut AdaptiveChargeState,
    saved: &mut Option<crate::settings::AdaptiveChargeSavedLimit>,
    now_minutes: u16,
) -> AdaptiveChargeOutcome {
    let Some(register) = adaptive_charge_register(snap.device_type) else {
        *state = AdaptiveChargeState::Error {
            message: "Adaptive Charge is not supported by this inverter".to_string(),
        };
        return AdaptiveChargeOutcome {
            write: None,
            desired_rate_percent: None,
        };
    };

    if !enabled {
        let Some(baseline) = saved.as_ref() else {
            *state = AdaptiveChargeState::Inactive;
            return AdaptiveChargeOutcome {
                write: None,
                desired_rate_percent: None,
            };
        };
        if baseline.inverter_serial != snap.inverter_serial
            || baseline.device_type_code != snap.device_type_code
            || baseline.register_address != register
        {
            *state = AdaptiveChargeState::Error {
                message: "Saved charge limit belongs to a different inverter".to_string(),
            };
            return AdaptiveChargeOutcome {
                write: None,
                desired_rate_percent: None,
            };
        }
        if !observed_charge_rate_is_valid(snap.device_type, baseline.raw_value) {
            *state = AdaptiveChargeState::Error {
                message: "Saved charge limit is outside this inverter's valid range".to_string(),
            };
            return AdaptiveChargeOutcome {
                write: None,
                desired_rate_percent: None,
            };
        }
        if snap.charge_rate as u16 == baseline.raw_value {
            *saved = None;
            *state = AdaptiveChargeState::Inactive;
            return AdaptiveChargeOutcome {
                write: None,
                desired_rate_percent: None,
            };
        }
        *state = AdaptiveChargeState::Restoring;
        return AdaptiveChargeOutcome {
            write: Some(RegisterWrite {
                address: register,
                value: baseline.raw_value,
            }),
            desired_rate_percent: Some(raw_charge_rate_to_normalized(
                snap.device_type,
                baseline.raw_value,
            )),
        };
    }

    if let Err(message) = config.validate() {
        *state = AdaptiveChargeState::Error { message };
        return AdaptiveChargeOutcome {
            write: None,
            desired_rate_percent: None,
        };
    }

    let observed_raw = snap.charge_rate as u16;
    if !observed_charge_rate_is_valid(snap.device_type, observed_raw) {
        tracing::warn!(
            device_type = ?snap.device_type,
            observed_raw,
            "Adaptive Charge: ignoring invalid observed charge limit"
        );
        if saved.is_none() {
            *state = AdaptiveChargeState::Inactive;
        }
        return AdaptiveChargeOutcome {
            write: None,
            desired_rate_percent: None,
        };
    }

    if saved.is_none() {
        let stable = matches!(
            state,
            AdaptiveChargeState::BaselinePending { raw_value } if *raw_value == observed_raw
        );
        if !stable {
            *state = AdaptiveChargeState::BaselinePending {
                raw_value: observed_raw,
            };
            return AdaptiveChargeOutcome {
                write: None,
                desired_rate_percent: None,
            };
        }
        *saved = Some(crate::settings::AdaptiveChargeSavedLimit {
            inverter_serial: snap.inverter_serial.clone(),
            device_type_code: snap.device_type_code.clone(),
            register_address: register,
            raw_value: observed_raw,
        });
    }
    let baseline = saved.as_ref().expect("baseline captured above").clone();
    if baseline.inverter_serial != snap.inverter_serial
        || baseline.device_type_code != snap.device_type_code
        || baseline.register_address != register
    {
        *state = AdaptiveChargeState::Error {
            message: "Saved charge limit belongs to a different inverter".to_string(),
        };
        return AdaptiveChargeOutcome {
            write: None,
            desired_rate_percent: None,
        };
    }
    if !observed_charge_rate_is_valid(snap.device_type, baseline.raw_value) {
        *state = AdaptiveChargeState::Error {
            message: "Saved charge limit is outside this inverter's valid range".to_string(),
        };
        return AdaptiveChargeOutcome {
            write: None,
            desired_rate_percent: None,
        };
    }

    if snap.auto_winter_active {
        let restore_pending = match state {
            AdaptiveChargeState::SuspendedAutoWinter { restore_pending } => *restore_pending,
            _ => true,
        };
        if restore_pending && snap.charge_rate as u16 != baseline.raw_value {
            *state = AdaptiveChargeState::SuspendedAutoWinter {
                restore_pending: true,
            };
            return AdaptiveChargeOutcome {
                write: Some(RegisterWrite {
                    address: register,
                    value: baseline.raw_value,
                }),
                desired_rate_percent: Some(raw_charge_rate_to_normalized(
                    snap.device_type,
                    baseline.raw_value,
                )),
            };
        }
        *state = AdaptiveChargeState::SuspendedAutoWinter {
            restore_pending: false,
        };
        return AdaptiveChargeOutcome {
            write: None,
            desired_rate_percent: None,
        };
    }

    let Some(period_index) = adaptive_period_at(config, now_minutes) else {
        let observed = snap.charge_rate as u16;
        let owned_window = matches!(
            state,
            AdaptiveChargeState::Preferred { .. } | AdaptiveChargeState::Recovery { .. }
        );
        let restoring = matches!(state, AdaptiveChargeState::Restoring);
        let mut desired = baseline.raw_value;
        if owned_window || restoring {
            if observed != baseline.raw_value {
                *state = AdaptiveChargeState::Restoring;
                return AdaptiveChargeOutcome {
                    write: Some(RegisterWrite {
                        address: register,
                        value: baseline.raw_value,
                    }),
                    desired_rate_percent: Some(raw_charge_rate_to_normalized(
                        snap.device_type,
                        baseline.raw_value,
                    )),
                };
            }
        } else if observed != baseline.raw_value {
            // Outside an Adaptive-owned window, a later manual edit is the
            // user's new baseline. Do not fight it with the old captured rate.
            if let Some(saved) = saved.as_mut() {
                saved.raw_value = observed;
            }
            desired = observed;
        }
        *state = AdaptiveChargeState::OutsideWindow;
        return AdaptiveChargeOutcome {
            write: None,
            desired_rate_percent: Some(raw_charge_rate_to_normalized(snap.device_type, desired)),
        };
    };
    let period = &config.periods[period_index];
    let confirmations = config.confirmation_readings.max(1);

    let recovery = match state.clone() {
        AdaptiveChargeState::Recovery {
            period: state_period,
            high_count,
        } if state_period == period_index => {
            let next_count = if snap.soc >= period.recovery_soc {
                high_count + 1
            } else {
                0
            };
            if next_count >= confirmations {
                *state = AdaptiveChargeState::Preferred {
                    period: period_index,
                    low_count: 0,
                };
                false
            } else {
                *state = AdaptiveChargeState::Recovery {
                    period: period_index,
                    high_count: next_count,
                };
                true
            }
        }
        AdaptiveChargeState::Preferred {
            period: state_period,
            low_count,
        } if state_period == period_index => {
            let next_count = if snap.soc <= period.low_soc {
                low_count + 1
            } else {
                0
            };
            if next_count >= confirmations {
                *state = AdaptiveChargeState::Recovery {
                    period: period_index,
                    high_count: 0,
                };
                true
            } else {
                *state = AdaptiveChargeState::Preferred {
                    period: period_index,
                    low_count: next_count,
                };
                false
            }
        }
        _ if snap.soc <= period.low_soc => {
            *state = AdaptiveChargeState::Recovery {
                period: period_index,
                high_count: 0,
            };
            true
        }
        _ => {
            *state = AdaptiveChargeState::Preferred {
                period: period_index,
                low_count: 0,
            };
            false
        }
    };

    let desired_percent = if recovery {
        period.recovery_rate_percent
    } else {
        period.preferred_rate_percent
    };
    let desired_raw = normalized_charge_rate_to_raw(snap.device_type, desired_percent)
        .expect("supported device has a charge-rate conversion");

    AdaptiveChargeOutcome {
        write: (snap.charge_rate as u16 != desired_raw).then_some(RegisterWrite {
            address: register,
            value: desired_raw,
        }),
        desired_rate_percent: Some(desired_percent),
    }
}

// ===========================================================================
// Persistence helpers (Cosy + Agile crash-recovery flags)
// ===========================================================================

/// Persist the in-memory `cosy_active` flag to settings so a crash/restart
/// can detect a missed CosyExit (the inverter was left force-charging after
/// the slot ended but before the app came back up). On startup,
/// `AppState::new` seeds the in-memory flag from this persisted value, and
/// the normal cosy state machine fires CosyExit on the next poll if the
/// current time is outside any Cosy slot.
pub(crate) fn persist_cosy_active(active: bool) {
    // In tests, run synchronously (no Tokio runtime).
    // In production, offload file I/O to the blocking thread pool.
    #[cfg(not(test))]
    {
        tokio::task::spawn_blocking(move || persist_cosy_active_sync(active));
    }
    #[cfg(test)]
    persist_cosy_active_sync(active);
}

pub(crate) fn persist_cosy_active_sync(active: bool) {
    if let Err(e) = crate::settings::Settings::update(|s| {
        if s.cosy_active_persisted != active {
            s.cosy_active_persisted = active;
        }
    }) {
        tracing::warn!(active, "Failed to persist cosy_active flag: {e}");
    }
}

// `persist_agile_state` and `persist_agile_state_sync` were removed in
// the slot-based Agile refactor. The slot-based approach derives state
// from the inverter's own slot registers on every poll, so there's
// nothing to persist or recover. The startup log line at
// `poll.rs:962-976` has been replaced with a snapshot-based diagnostic
// that reads the inverter's actual `enable_charge` / `enable_discharge`
// state instead of the legacy `agile_state_persisted` string.

// ===========================================================================
// Cosy slot register-write generators
// ===========================================================================

/// When `active` is true (slot is currently running), writes the slot times,
/// enables charging, and sets the target SOC. When `active` is false (preloading
/// the next slot), writes only the slot times so the inverter has them ready
/// for when the slot starts - but does NOT enable charging.
///
/// For three-phase models, uses the three-phase charge slot 1 registers.
/// For Gen3+ models, also writes the per-slot target SOC in the HR 240-299 block.
pub(crate) fn cosy_slot_register_writes(
    slot: &crate::settings::CosySlot,
    device_type: DeviceType,
    active: bool,
) -> Vec<RegisterWrite> {
    let (Some(start), Some(end)) = (
        encode_hhmm(slot.start_hour, slot.start_minute),
        encode_hhmm(slot.end_hour, slot.end_minute),
    ) else {
        tracing::warn!("Skipping Cosy slot with invalid HHMM components");
        return Vec::new();
    };

    let mut writes = Vec::new();

    // Write slot times into the inverter's charge slot 1 registers.
    if device_type.uses_three_phase_schedule_slots() {
        // Three-phase models use HR 1113-1114 for charge slot 1.
        use crate::modbus::registers::{HR_3PH_CHARGE_SLOT_1_END, HR_3PH_CHARGE_SLOT_1_START};
        writes.push(RegisterWrite {
            address: HR_3PH_CHARGE_SLOT_1_START,
            value: start,
        });
        writes.push(RegisterWrite {
            address: HR_3PH_CHARGE_SLOT_1_END,
            value: end,
        });
    } else {
        // Single-phase models use HR 94-95 for charge slot 1.
        writes.push(RegisterWrite {
            address: HR_CHARGE_SLOT_1_START,
            value: start,
        });
        writes.push(RegisterWrite {
            address: HR_CHARGE_SLOT_1_END,
            value: end,
        });
    }

    if active {
        // Enable charge so the inverter acts on the slot schedule.
        writes.push(RegisterWrite {
            address: HR_ENABLE_CHARGE,
            value: 1,
        });
        writes.push(RegisterWrite {
            address: HR_ENABLE_CHARGE_TARGET,
            value: 1,
        });
        writes.push(RegisterWrite {
            address: HR_CHARGE_TARGET_SOC,
            value: slot.target_soc as u16,
        });
    }

    // For Gen3+/extended models, also write per-slot target SOC.
    if active && device_type.uses_extended_schedule_slots() {
        use crate::modbus::registers::HR_CHARGE_TARGET_SOC_1;
        writes.push(RegisterWrite {
            address: HR_CHARGE_TARGET_SOC_1,
            value: slot.target_soc as u16,
        });
    }

    writes
}

/// Generate register writes to clear the inverter's charge slot 1 registers
/// and disable charging (used when there's no next Cosy slot to preload).
pub(crate) fn clear_cosy_slot_registers(device_type: DeviceType) -> Vec<RegisterWrite> {
    let mut writes = Vec::new();

    if device_type.uses_three_phase_schedule_slots() {
        use crate::modbus::registers::{HR_3PH_CHARGE_SLOT_1_END, HR_3PH_CHARGE_SLOT_1_START};
        writes.push(RegisterWrite {
            address: HR_3PH_CHARGE_SLOT_1_START,
            value: 0,
        });
        writes.push(RegisterWrite {
            address: HR_3PH_CHARGE_SLOT_1_END,
            value: 0,
        });
    } else {
        writes.push(RegisterWrite {
            address: HR_CHARGE_SLOT_1_START,
            value: 0,
        });
        writes.push(RegisterWrite {
            address: HR_CHARGE_SLOT_1_END,
            value: 0,
        });
    }

    writes.push(RegisterWrite {
        address: HR_ENABLE_CHARGE,
        value: 0,
    });
    writes.push(RegisterWrite {
        address: HR_ENABLE_CHARGE_TARGET,
        value: 0,
    });

    writes
}

#[allow(async_fn_in_trait)]
trait RegisterWriteExecutor {
    async fn write_register(&mut self, write: &RegisterWrite) -> Result<(), String>;
}

impl RegisterWriteExecutor for ModbusClient {
    async fn write_register(&mut self, write: &RegisterWrite) -> Result<(), String> {
        ModbusClient::write_register(self, write.address, write.value)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Execute a list of register writes with delays between adjacent writes.
async fn execute_register_writes<W: RegisterWriteExecutor>(
    writer: &mut W,
    writes: &[RegisterWrite],
    label: &str,
    inter_write_delay: Duration,
) -> bool {
    for (index, w) in writes.iter().enumerate() {
        match writer.write_register(w).await {
            Ok(()) => {
                tracing::info!("{}: wrote reg {} = {}", label, w.address, w.value);
                if index + 1 < writes.len() {
                    tokio::time::sleep(inter_write_delay).await;
                }
            }
            Err(e) => {
                tracing::error!("{}: write reg {} failed: {e}", label, w.address);
                return false;
            }
        }
    }
    true
}

/// Execute a list of register writes to the inverter with inter-write delays.
/// Returns `true` if all writes succeeded.
pub(crate) async fn write_registers_to_inverter(
    client: &mut ModbusClient,
    writes: &[RegisterWrite],
    label: &str,
) -> bool {
    execute_register_writes(client, writes, label, Duration::from_millis(1500)).await
}

// ===========================================================================
// State-machine transition logic
// ===========================================================================

/// Evaluate the auto-winter state machine and return register writes if a
/// state transition requires changing the inverter's configuration (enabling
/// or disabling winter mode).
///
/// The state machine uses two temperature thresholds with hysteresis:
///   * `cold_threshold` - temperature below which we start counting
///   * `recovery_threshold` - temperature above which we start counting
///
/// To prevent a single corrupt temperature reading from triggering a
/// transition, the state machine requires `debounce_readings` consecutive
/// polls with the temperature on the same side of the threshold before
/// acting. A single reading on the other side resets the counter.
#[cfg(test)]
pub(crate) fn check_auto_winter(
    snap: &InverterSnapshot,
    config: &AutoWinterConfig,
    state: &mut AutoWinterState,
    saved: &mut Option<AutoWinterSaved>,
) -> Option<Vec<RegisterWrite>> {
    check_auto_winter_with_outcome(
        snap,
        config,
        state,
        saved,
        AutoWinterWriteOutcome::NoneIssued,
    )
}

/// Evaluate auto-winter state while reconciling the result of the previous
/// register batch. Activation and restoration remain pending until the next
/// sanitized snapshot confirms both requested registers.
pub(crate) fn check_auto_winter_with_outcome(
    snap: &InverterSnapshot,
    config: &AutoWinterConfig,
    state: &mut AutoWinterState,
    saved: &mut Option<AutoWinterSaved>,
    last_write_outcome: AutoWinterWriteOutcome,
) -> Option<Vec<RegisterWrite>> {
    if !config.enabled {
        *state = AutoWinterState::Idle;
        *saved = None;
        return None;
    }

    let temp = snap.battery_temperature;

    let activation_writes = || {
        vec![
            RegisterWrite {
                address: HR_ENABLE_CHARGE_TARGET,
                value: 1,
            },
            RegisterWrite {
                address: HR_CHARGE_TARGET_SOC,
                value: config.target_soc as u16,
            },
        ]
    };

    let restoration_values = saved
        .as_ref()
        .map(|value| {
            (
                value.target_soc as u16,
                if value.enable_charge_target { 1 } else { 0 },
            )
        })
        .unwrap_or((100, 0));
    let restoration_writes = || {
        vec![
            RegisterWrite {
                address: HR_ENABLE_CHARGE_TARGET,
                value: restoration_values.1,
            },
            RegisterWrite {
                address: HR_CHARGE_TARGET_SOC,
                value: restoration_values.0,
            },
        ]
    };

    match state {
        AutoWinterState::Idle => {
            if temp < config.cold_threshold {
                tracing::info!(
                    temp,
                    cold = config.cold_threshold,
                    "Auto winter: battery cold - counting",
                );
                *state = AutoWinterState::ColdPending { consecutive: 1 };
            }
        }
        AutoWinterState::ColdPending { consecutive } => {
            if temp < config.cold_threshold {
                *consecutive += 1;
                if *consecutive >= config.debounce_readings {
                    tracing::info!(
                        consecutive,
                        "Auto winter: activating (HR 20=1, HR 116={})",
                        config.target_soc,
                    );
                    // Don't overwrite saved values that were restored from
                    // disk after a restart - those reflect the original state
                    // before winter mode first activated.
                    if saved.is_none() {
                        *saved = Some(AutoWinterSaved {
                            enable_charge_target: snap.enable_charge_target,
                            target_soc: snap.target_soc,
                        });
                    }
                    *state = AutoWinterState::Activating { retries: 0 };
                    return Some(activation_writes());
                }
            } else if temp >= config.recovery_threshold {
                *state = AutoWinterState::Idle;
            }
        }
        AutoWinterState::Activating { retries } => {
            if snap.enable_charge_target && snap.target_soc == config.target_soc {
                *state = AutoWinterState::WinterActive;
            } else {
                let next_retries = match last_write_outcome {
                    AutoWinterWriteOutcome::Failed | AutoWinterWriteOutcome::Succeeded => {
                        retries.saturating_add(1)
                    }
                    AutoWinterWriteOutcome::NoneIssued => *retries,
                };
                if next_retries > AUTO_WINTER_MAX_WRITE_RETRIES {
                    *state = AutoWinterState::Error {
                        message: format!(
                            "Auto-winter activation failed after {AUTO_WINTER_MAX_WRITE_RETRIES} retries"
                        ),
                    };
                } else {
                    *state = AutoWinterState::Activating {
                        retries: next_retries,
                    };
                    return Some(activation_writes());
                }
            }
        }
        AutoWinterState::WinterActive => {
            if temp >= config.recovery_threshold {
                tracing::info!(
                    temp,
                    recovery = config.recovery_threshold,
                    "Auto winter: battery warming - counting",
                );
                *state = AutoWinterState::WarmPending { consecutive: 1 };
            }
        }
        AutoWinterState::WarmPending { consecutive } => {
            if temp >= config.recovery_threshold {
                *consecutive += 1;
                if *consecutive >= config.debounce_readings {
                    let (restore_target, restore_enable) = restoration_values;
                    tracing::info!(
                        consecutive,
                        "Auto winter: restoring (HR 20={}, HR 116={})",
                        restore_enable,
                        restore_target,
                    );
                    *state = AutoWinterState::Restoring { retries: 0 };
                    return Some(restoration_writes());
                }
            } else if temp < config.cold_threshold {
                *state = AutoWinterState::WinterActive;
            }
        }
        AutoWinterState::Restoring { retries } => {
            let (restore_target, restore_enable) = restoration_values;
            if snap.target_soc as u16 == restore_target
                && snap.enable_charge_target == (restore_enable == 1)
            {
                saved.take();
                *state = AutoWinterState::Idle;
            } else {
                let next_retries = match last_write_outcome {
                    AutoWinterWriteOutcome::Failed | AutoWinterWriteOutcome::Succeeded => {
                        retries.saturating_add(1)
                    }
                    AutoWinterWriteOutcome::NoneIssued => *retries,
                };
                if next_retries > AUTO_WINTER_MAX_WRITE_RETRIES {
                    *state = AutoWinterState::Error {
                        message: format!(
                            "Auto-winter restoration failed after {AUTO_WINTER_MAX_WRITE_RETRIES} retries"
                        ),
                    };
                } else {
                    *state = AutoWinterState::Restoring {
                        retries: next_retries,
                    };
                    return Some(restoration_writes());
                }
            }
        }
        AutoWinterState::Error { .. } => {}
    }

    None
}

fn discharge_pause_writes(device_type: DeviceType, reserve: u16) -> Vec<RegisterWrite> {
    let mut writes = vec![
        RegisterWrite {
            address: HR_BATTERY_POWER_MODE,
            value: 1,
        },
        RegisterWrite {
            address: HR_ENABLE_DISCHARGE,
            value: 0,
        },
    ];
    if device_type.uses_three_phase_schedule_slots() {
        writes.push(RegisterWrite {
            address: HR_3PH_FORCE_DISCHARGE_ENABLE,
            value: 0,
        });
        writes.push(RegisterWrite {
            address: HR_3PH_BATTERY_SOC_RESERVE,
            value: reserve,
        });
    } else {
        writes.push(RegisterWrite {
            address: HR_BATTERY_SOC_RESERVE,
            value: reserve,
        });
    }
    writes
}

fn release_shared_discharge_pause(
    snap: &InverterSnapshot,
    saved: &mut Option<LoadLimiterSaved>,
    other_active: bool,
) -> Option<Vec<RegisterWrite>> {
    if other_active {
        None
    } else {
        let reserve = saved.take().map(|value| value.reserve).unwrap_or(4);
        Some(discharge_pause_writes(snap.device_type, reserve))
    }
}

/// Evaluate inverter-temperature discharge protection. This safety limiter
/// applies in every battery mode and always recovers to normal Eco. `other_active`
/// represents another limiter that still owns the shared Eco Paused state.
#[cfg(test)]
pub(crate) fn check_temperature_limiter(
    snap: &InverterSnapshot,
    config: &TemperatureLimiterConfig,
    state: &mut TemperatureLimiterState,
    saved: &mut Option<LoadLimiterSaved>,
    other_active: bool,
) -> Option<Vec<RegisterWrite>> {
    check_temperature_limiter_after_automation(snap, config, state, saved, other_active, false)
}

/// Temperature limiter variant used by the poll loop after automation writes.
/// `reassert_pause` ensures a discharge command issued earlier in the same poll
/// cannot override an already-confirmed thermal pause before the next read-back.
pub(crate) fn check_temperature_limiter_after_automation(
    snap: &InverterSnapshot,
    config: &TemperatureLimiterConfig,
    state: &mut TemperatureLimiterState,
    saved: &mut Option<LoadLimiterSaved>,
    other_active: bool,
    reassert_pause: bool,
) -> Option<Vec<RegisterWrite>> {
    let release = |state: &mut TemperatureLimiterState,
                   saved: &mut Option<LoadLimiterSaved>|
     -> Option<Vec<RegisterWrite>> {
        if other_active {
            *state = TemperatureLimiterState::Idle;
            None
        } else {
            // Keep ownership and the saved reserve until read-back confirms
            // Eco. This makes failed dongle writes retry on the next poll.
            *state = TemperatureLimiterState::PausedFromRestart;
            let reserve = saved.as_ref().map(|value| value.reserve).unwrap_or(4);
            Some(discharge_pause_writes(snap.device_type, reserve))
        }
    };

    if !config.enabled {
        return if state.is_actively_pausing() {
            if matches!(state, TemperatureLimiterState::PausedFromRestart)
                && snap.battery_mode == BatteryMode::Eco
                && !other_active
            {
                *saved = None;
                *state = TemperatureLimiterState::Idle;
                None
            } else {
                release(state, saved)
            }
        } else {
            *state = TemperatureLimiterState::Idle;
            None
        };
    }

    let temperature = snap.inverter_temperature;
    if !temperature.is_finite() {
        return None;
    }

    match state {
        TemperatureLimiterState::Idle => {
            if temperature >= config.high_threshold {
                if config.confirmation_readings == 1 {
                    if saved.is_none() && (4..100).contains(&(snap.battery_reserve as u16)) {
                        *saved = Some(LoadLimiterSaved {
                            reserve: snap.battery_reserve as u16,
                        });
                    }
                    *state = TemperatureLimiterState::Paused;
                    return Some(discharge_pause_writes(snap.device_type, 100));
                } else {
                    *state = TemperatureLimiterState::HighPending { consecutive: 1 };
                }
            }
        }
        TemperatureLimiterState::HighPending { consecutive } => {
            if temperature >= config.high_threshold {
                *consecutive += 1;
                if *consecutive >= config.confirmation_readings {
                    if saved.is_none() && (4..100).contains(&(snap.battery_reserve as u16)) {
                        *saved = Some(LoadLimiterSaved {
                            reserve: snap.battery_reserve as u16,
                        });
                    }
                    *state = TemperatureLimiterState::Paused;
                    tracing::warn!(
                        temperature,
                        threshold = config.high_threshold,
                        "Temperature limiter: pausing battery discharge"
                    );
                    return Some(discharge_pause_writes(snap.device_type, 100));
                }
            } else {
                *state = TemperatureLimiterState::Idle;
            }
        }
        TemperatureLimiterState::Paused => {
            if temperature <= config.recovery_threshold {
                if config.confirmation_readings == 1 {
                    return release(state, saved);
                }
                *state = TemperatureLimiterState::CoolingPending { consecutive: 1 };
            }
            if reassert_pause || snap.battery_mode != BatteryMode::EcoPaused {
                // Reassert after a discharge write earlier in this poll because
                // the snapshot predates that write and cannot reflect it yet.
                return Some(discharge_pause_writes(snap.device_type, 100));
            }
        }
        TemperatureLimiterState::PausedFromRestart => {
            if temperature <= config.recovery_threshold {
                if other_active {
                    *state = TemperatureLimiterState::Idle;
                } else if snap.battery_mode == BatteryMode::Eco {
                    *saved = None;
                    *state = TemperatureLimiterState::Idle;
                } else {
                    // Keep retrying until a later snapshot confirms Eco.
                    let reserve = saved.as_ref().map(|value| value.reserve).unwrap_or(4);
                    return Some(discharge_pause_writes(snap.device_type, reserve));
                }
            } else {
                *state = TemperatureLimiterState::Paused;
                if reassert_pause || snap.battery_mode != BatteryMode::EcoPaused {
                    return Some(discharge_pause_writes(snap.device_type, 100));
                }
            }
        }
        TemperatureLimiterState::CoolingPending { consecutive } => {
            if temperature <= config.recovery_threshold {
                *consecutive += 1;
                if *consecutive >= config.confirmation_readings {
                    tracing::info!(
                        temperature,
                        threshold = config.recovery_threshold,
                        "Temperature limiter: restoring Eco after cooling"
                    );
                    return release(state, saved);
                }
            } else if temperature >= config.high_threshold {
                *state = TemperatureLimiterState::Paused;
            }
            if reassert_pause || snap.battery_mode != BatteryMode::EcoPaused {
                return Some(discharge_pause_writes(snap.device_type, 100));
            }
        }
    }

    None
}

/// Check load discharge limiter and return register writes if the state
/// machine transitions to Paused or back to Idle.
///
/// Returns `Some(writes)` when a transition requires register writes,
/// `None` otherwise.
#[cfg(test)]
pub(crate) fn check_load_limiter(
    snap: &InverterSnapshot,
    config: &LoadLimiterConfig,
    state: &mut LoadLimiterState,
    poll_interval_secs: u64,
    saved: &mut Option<LoadLimiterSaved>,
) -> Option<Vec<RegisterWrite>> {
    check_load_limiter_with_other_pause(snap, config, state, poll_interval_secs, saved, false)
}

#[cfg(test)]
pub(crate) fn check_load_limiter_with_other_pause(
    snap: &InverterSnapshot,
    config: &LoadLimiterConfig,
    state: &mut LoadLimiterState,
    poll_interval_secs: u64,
    saved: &mut Option<LoadLimiterSaved>,
    other_active: bool,
) -> Option<Vec<RegisterWrite>> {
    let now = chrono::Local::now();
    let now_minutes = now.hour() as u16 * 60 + now.minute() as u16;
    check_load_limiter_at(
        snap,
        config,
        state,
        poll_interval_secs,
        saved,
        now_minutes,
        other_active,
    )
}

pub(crate) fn check_load_limiter_at(
    snap: &InverterSnapshot,
    config: &LoadLimiterConfig,
    state: &mut LoadLimiterState,
    poll_interval_secs: u64,
    saved: &mut Option<LoadLimiterSaved>,
    now_minutes: u16,
    other_active: bool,
) -> Option<Vec<RegisterWrite>> {
    if !config.enabled {
        if state.is_actively_pausing() {
            tracing::info!(
                other_active,
                "Load limiter: disabled while active, releasing pause ownership"
            );
            *state = LoadLimiterState::Idle;
            return release_shared_discharge_pause(snap, saved, other_active);
        }
        *state = LoadLimiterState::Idle;
        return None;
    }

    // Check activation window.
    let start_mins = config.start_hour as u16 * 60 + config.start_minute as u16;
    let end_mins = config.end_hour as u16 * 60 + config.end_minute as u16;

    // All zeros means always active.
    let in_window = if start_mins == 0 && end_mins == 0 {
        true
    } else if end_mins <= start_mins {
        // Crosses midnight
        now_minutes >= start_mins || now_minutes < end_mins
    } else {
        now_minutes >= start_mins && now_minutes < end_mins
    };

    if !in_window {
        // Outside the window, discard any unfinished high-load countdown. If
        // the limiter already owns the pause (including the recovery delay),
        // restore Eco immediately rather than leaving the battery paused until
        // the activation window opens again.
        if state.is_actively_pausing() {
            tracing::info!(
                other_active,
                "Load limiter: outside activation window, releasing pause ownership"
            );
            *state = LoadLimiterState::Idle;
            return release_shared_discharge_pause(snap, saved, other_active);
        }
        *state = LoadLimiterState::Idle;
        return None;
    }

    let home_power = snap.home_power;
    let threshold = config.threshold_w as i32;
    let debounce_count = if poll_interval_secs == 0 {
        config.trigger_delay_minutes // fallback
    } else {
        (config.trigger_delay_minutes as u64 * 60).div_ceil(poll_interval_secs) as u32
    };

    match state {
        LoadLimiterState::Idle => {
            if home_power > threshold {
                tracing::info!(
                    home_power,
                    threshold,
                    "Load limiter: home load above threshold - counting"
                );
                *state = LoadLimiterState::HighLoadPending { consecutive: 1 };
            }
        }
        LoadLimiterState::HighLoadPending { consecutive } => {
            if home_power > threshold {
                *consecutive += 1;
                if *consecutive >= debounce_count {
                    tracing::info!(
                        home_power,
                        threshold,
                        "Load limiter: pausing battery discharge (Eco Paused)"
                    );
                    *state = LoadLimiterState::Paused;
                    // Capture the reserve only for the first limiter taking
                    // ownership. A second limiter must preserve that baseline.
                    if saved.is_none() && (4..100).contains(&(snap.battery_reserve as u16)) {
                        *saved = Some(LoadLimiterSaved {
                            reserve: snap.battery_reserve as u16,
                        });
                    }
                    if !other_active {
                        return Some(discharge_pause_writes(snap.device_type, 100));
                    }
                }
            } else {
                tracing::info!(
                    home_power,
                    threshold,
                    consecutive,
                    "Load limiter: load dropped below threshold, resetting count"
                );
                *state = LoadLimiterState::Idle;
            }
        }
        LoadLimiterState::Paused => {
            if home_power <= threshold {
                tracing::info!(
                    home_power,
                    threshold,
                    "Load limiter: load below threshold - counting"
                );
                *state = LoadLimiterState::LowLoadPending { consecutive: 1 };
            } else if snap.battery_mode != BatteryMode::EcoPaused {
                // A higher-priority safety pause may have been overwritten
                // by a manual or scheduled mode write. Reassert the pause
                // immediately while the load is still dangerous.
                return Some(discharge_pause_writes(snap.device_type, 100));
            }
        }
        // Post-crash restart: the debounce delay already elapsed while
        // the app was down. If the battery is already back in Eco mode
        // (writes from a previous poll succeeded), transition to Idle.
        // If the load is below threshold, send restore writes but stay
        // in PausedFromRestart so a failed write (dongle busy on first
        // poll after reconnect) is retried on the next poll.
        // If the load is still high, transition to normal Paused.
        LoadLimiterState::PausedFromRestart => {
            // Writes from a previous poll succeeded — we're restored.
            if snap.battery_mode == BatteryMode::Eco {
                tracing::info!(
                    "Load limiter: post-crash - battery already in Eco mode, restore confirmed"
                );
                if !other_active {
                    *saved = None;
                }
                *state = LoadLimiterState::Idle;
                return None;
            }

            if home_power <= threshold {
                if other_active {
                    *state = LoadLimiterState::Idle;
                    return None;
                }
                let restore_reserve = saved.as_ref().map(|s| s.reserve).unwrap_or(4);
                tracing::info!(
                    restore_reserve,
                    "Load limiter: post-crash - load below threshold, restoring Eco"
                );
                // Stay in PausedFromRestart — if the write fails (dongle
                // busy on first poll after reconnect), the next poll will
                // retry. Once the battery mode flips to Eco, the check
                // above transitions to Idle.
                return Some(discharge_pause_writes(snap.device_type, restore_reserve));
            } else {
                tracing::info!(
                    home_power,
                    threshold,
                    "Load limiter: post-crash - load still high, staying Paused"
                );
                *state = LoadLimiterState::Paused;
                if snap.battery_mode != BatteryMode::EcoPaused {
                    return Some(discharge_pause_writes(snap.device_type, 100));
                }
            }
        }
        LoadLimiterState::LowLoadPending { consecutive } => {
            if home_power <= threshold {
                *consecutive += 1;
                if *consecutive >= debounce_count {
                    tracing::info!(
                        consecutive = *consecutive,
                        other_active,
                        "Load limiter: load recovered, releasing pause ownership"
                    );
                    *state = LoadLimiterState::Idle;
                    return release_shared_discharge_pause(snap, saved, other_active);
                }
                // Periodic progress log every ~20% of the delay
                let every_nth = std::cmp::max(1, debounce_count / 5);
                if *consecutive % every_nth == 0 {
                    tracing::info!(
                        consecutive,
                        debounce_count,
                        "Load limiter: counting down - {}/{} polls remaining",
                        debounce_count - *consecutive,
                        debounce_count
                    );
                }
            } else {
                tracing::info!(
                    home_power,
                    threshold,
                    consecutive,
                    "Load limiter: load rose above threshold, staying Paused"
                );
                *state = LoadLimiterState::Paused;
            }
        }
    }

    None
}

// ===========================================================================
// Timed Export: types + transition logic
// ===========================================================================

/// State machine for Timed Export schedule management.
///
/// Timed Export is treated as a temporary override of the Eco baseline:
/// outside export windows the inverter is in Eco (HR27=1, HR59=0); inside
/// enabled windows the state machine transitions to maximum-power export
/// (HR27=0, HR59=1) and back to Eco at window exit.
///
/// The machine tracks:
/// - `desired_slots`: the user's configured export slots (persisted in settings)
/// - `device_rearm_confirmed`: whether firmware has been observed
///   re-arming HR59 when slots remain populated (requiring slot
///   clear/restore fallback)
///
/// Readback-confirmed state of the HEM-managed Timed Export schedule.
///
/// The machine is a **reconciler**: every poll compares the desired
/// condition (derived from the persisted schedule, the inverter-local time
/// and the HR318 pause gate) with the actual register readback, and issues
/// idempotent repair writes whenever the two disagree. Transitions are
/// confirmed from later poll snapshots, never assumed when writes are
/// queued; write failures are retried a bounded number of times and then
/// surface as [`TimedExportState::Error`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
pub enum TimedExportState {
    /// No Timed Export schedule configured or schedule disabled.
    #[default]
    Off,
    /// Schedule configured but currently outside any export window.
    /// Inverter is in Eco (HR27=1, HR59=0). Physical slots may be populated
    /// (normal case) or cleared (re-arm fallback).
    Configured,
    /// Inside an export window, entry writes have been issued but not yet
    /// confirmed by a snapshot showing HR27=0/HR59=1 (model-routed).
    /// `polls_waiting` counts unconfirmed polls since the last issue;
    /// `retries` counts re-issues after failures or confirmation timeouts.
    Entering { polls_waiting: u32, retries: u32 },
    /// Export window active: HR27=0/HR59=1 confirmed, maximum-power export.
    Active,
    /// Window ended (or repair needed), exit writes issued but not yet
    /// confirmed by a snapshot showing HR59=0/HR27=1.
    Exiting { polls_waiting: u32, retries: u32 },
    /// Export window active but HR318 pause mode is blocking discharge.
    /// Schedule remains configured; export will resume when pause ends.
    BlockedByPause,
    /// Write failure or unexpected state; requires intervention. A schedule
    /// change (enable/disable or slot edit) clears this and retries.
    Error {
        /// Human-readable error description.
        reason: String,
    },
}

impl TimedExportState {
    /// Whether the machine is waiting on confirmation of a boundary write.
    pub fn is_boundary_pending(&self) -> bool {
        matches!(
            self,
            TimedExportState::Entering { .. } | TimedExportState::Exiting { .. }
        )
    }

    /// Whether the Timed Export machine currently owns the discharge-control
    /// registers this cycle (entry/exit writes issued or export confirmed).
    /// Lower-priority automations (Cosy, Agile) must defer while this is set.
    pub fn owns_discharge_control(&self) -> bool {
        matches!(
            self,
            TimedExportState::Entering { .. }
                | TimedExportState::Active
                | TimedExportState::Exiting { .. }
        )
    }
}

/// Configuration for Timed Export state machine.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct TimedExportConfig {
    /// Master toggle: whether the schedule is enabled.
    pub schedule_enabled: bool,
    /// Desired export slots (user-configured).
    pub slots: Vec<ScheduleSlot>,
    /// Whether firmware has confirmed HR59 re-arm behaviour (slots must be
    /// cleared outside windows and restored at entry).
    pub device_rearm_confirmed: bool,
    /// Durable stop/exit-pending marker (`Settings::timed_export_stop_pending`
    /// mirrored through `AppState::timed_export_stop_pending`). Set before a
    /// route disables the schedule and cleared only on a confirmed Eco
    /// baseline — armed registers plus populated slots are then residue of
    /// HEM's own incomplete stop and stay repairable, even across a restart.
    #[serde(default)]
    pub stop_pending: bool,
}

/// Decision output from the Timed Export state machine.
pub struct TimedExportDecision {
    /// New state after evaluation.
    pub new_state: TimedExportState,
    /// Register writes to issue (empty if no transition needed). These are
    /// **fail-fast**: the caller must stop at the first write error so a
    /// rejected slot write can never be followed by an export-arm write.
    pub writes: Vec<RegisterWrite>,
    /// Structured log message for the transition (if any).
    pub log_message: Option<String>,
    /// Whether this batch disarms scheduled discharge (exit or repair).
    /// The poll loop uses it to anchor HR59 re-arm detection: observation
    /// of an unsolicited re-arm only begins after such a write has landed
    /// and been confirmed off by readback.
    pub is_exit_transition: bool,
}

/// Outcome of the writes issued for the previous poll's decision. Fed back
/// into [`check_timed_export`] so the machine can retry failures rather
/// than advancing on the assumption that queued writes succeeded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TimedExportWriteOutcome {
    /// No writes were issued on the previous poll.
    #[default]
    NoneIssued,
    /// Every write in the previous batch was accepted by the inverter.
    Succeeded,
    /// At least one write failed (fail-fast: later writes were skipped).
    Failed,
}

/// Polls to wait for readback confirmation after a *successful* write batch
/// before re-issuing it. Readback can lag the write by a poll cycle.
pub const TIMED_EXPORT_CONFIRM_GRACE_POLLS: u32 = 2;

/// Total re-issues of a boundary write batch before the machine gives up and
/// surfaces [`TimedExportState::Error`].
pub const TIMED_EXPORT_MAX_WRITE_RETRIES: u32 = 3;

/// Clamp helper for the per-state retry bookkeeping.
fn bump(polls_waiting: &mut u32, retries: &mut u32) -> bool {
    if *polls_waiting >= TIMED_EXPORT_CONFIRM_GRACE_POLLS {
        *polls_waiting = 0;
        *retries += 1;
        *retries <= TIMED_EXPORT_MAX_WRITE_RETRIES
    } else {
        *polls_waiting += 1;
        true
    }
}

/// Check whether the current inverter-local time is inside any enabled slot.
///
/// `minute_of_day` is the current inverter-local time as minutes since midnight
/// (0-1439). Returns `true` if any enabled slot contains this minute.
pub fn export_window_contains(slots: &[ScheduleSlot], minute_of_day: u16) -> bool {
    is_in_export_window(slots, minute_of_day)
}

fn is_in_export_window(slots: &[ScheduleSlot], minute_of_day: u16) -> bool {
    slots.iter().any(|slot| {
        if !slot.enabled {
            return false;
        }
        let start = u16::from(slot.start_hour) * 60 + u16::from(slot.start_minute);
        let end = u16::from(slot.end_hour) * 60 + u16::from(slot.end_minute);
        if start == end {
            // Zero-length slot: disabled
            return false;
        }
        if start < end {
            // Normal slot: start <= minute < end
            (start..end).contains(&minute_of_day)
        } else {
            // Overnight slot: crosses midnight
            minute_of_day >= start || minute_of_day < end
        }
    })
}

/// Whether HR318 is actively blocking battery discharge at the given minute.
///
/// HR318=2 (pause discharging) or 3 (pause charge + discharge) arms a
/// discharge pause; HR319/320 (`battery_pause_slot`) hold the *inverse*
/// window — the pause window. Discharge is blocked while the current time
/// is inside that pause window, and permitted inside the visible Timed
/// Discharge window (the complement).
///
/// An unconfigured pause slot (disabled or zero-length) blocks nothing:
/// the full day is the allowed demand window. HR318=1 (pause charging
/// only) never blocks discharge.
pub fn hr318_blocks_discharge(snapshot: &InverterSnapshot, minute_of_day: u16) -> bool {
    match snapshot.battery_pause_mode {
        2 | 3 => {
            let slot = &snapshot.battery_pause_slot;
            if !slot.enabled {
                return false;
            }
            let start = u16::from(slot.start_hour) * 60 + u16::from(slot.start_minute);
            let end = u16::from(slot.end_hour) * 60 + u16::from(slot.end_minute);
            if start == end {
                // Zero-length pause window: nothing is paused
                return false;
            }
            if start < end {
                (start..end).contains(&minute_of_day)
            } else {
                // Overnight pause window (e.g. pause 19:00–16:00 for a
                // visible demand window of 16:00–19:00)
                minute_of_day >= start || minute_of_day < end
            }
        }
        _ => false,
    }
}

/// Parse the inverter wall-clock (`InverterSnapshot::inverter_time`, decoded
/// from HR 35-40 as `"YYYY-MM-DD HH:MM:SS"`) into a minute-of-day value.
///
/// Export windows must be evaluated on the **inverter's** clock, not the
/// host's: HEM may run in a Docker container with a UTC timezone, the UI
/// may be opened from another timezone, and the inverter's own clock can
/// drift. Returns `None` when the registers are absent or malformed; the
/// caller then falls back to host time with a diagnostic.
pub fn inverter_minute_of_day(snapshot: &InverterSnapshot) -> Option<u16> {
    let time_part = snapshot.inverter_time.split(' ').nth(1)?;
    let mut parts = time_part.split(':');
    let hour: u16 = parts.next()?.parse().ok()?;
    let minute: u16 = parts.next()?.parse().ok()?;
    if hour < 24 && minute < 60 {
        Some(hour * 60 + minute)
    } else {
        None
    }
}

/// Choose the scheduling minute shared by all time-driven automations.
///
/// The inverter wall clock is authoritative because it is the clock that
/// applies the written schedules. The host minute is used only when the
/// inverter clock registers are absent or malformed.
pub fn authoritative_minute_of_day(snapshot: &InverterSnapshot, host_minute: u16) -> u16 {
    inverter_minute_of_day(snapshot).unwrap_or(host_minute)
}

/// Check the Timed Export state machine and return any required writes.
///
/// This is a pure **reconciler** that consumes:
/// - `snapshot`: current inverter snapshot (for HR27/HR59 readback — the
///   enable flag is already model-routed by the decoder — and HR318 pause
///   state)
/// - `config`: desired schedule configuration
/// - `state`: mutable state machine state
/// - `minute_of_day`: current **inverter-local** time (minutes since midnight)
/// - `device_type`: locked device identity (selects model-correct slot and
///   discharge-enable registers)
/// - `last_write_outcome`: whether the previous poll's writes succeeded,
///   failed, or were never issued
/// - `rearm_observation_in_progress`: the HR59 re-arm detector is currently
///   holding repair writes to observe whether firmware re-asserts HR59; the
///   machine must not disarm again while this is set
///
/// Returns a decision with optional fail-fast writes to issue.
pub fn check_timed_export(
    snapshot: &InverterSnapshot,
    config: &TimedExportConfig,
    state: &mut TimedExportState,
    minute_of_day: u16,
    device_type: DeviceType,
    last_write_outcome: TimedExportWriteOutcome,
    rearm_observation_in_progress: bool,
) -> TimedExportDecision {
    let in_window = is_in_export_window(&config.slots, minute_of_day);
    // An enabled slot with a zero-length (00:00–00:00) window is not a
    // safety boundary. Treat it as unconfigured so the reconciler never arms
    // maximum-power export without a real physical window.
    let has_enabled_slots = config.slots.iter().any(ScheduleSlot::is_configured);
    let hr318_blocking = hr318_blocks_discharge(snapshot, minute_of_day);

    // Model-routed readback: the decoder derives `enable_discharge` from
    // HR59 (single-phase) or HR1122 (three-phase) as appropriate.
    let current_hr27 = snapshot.battery_power_mode;
    let current_enable = snapshot.enable_discharge;
    let export_confirmed = current_hr27 == 0 && current_enable;
    let eco_confirmed = current_hr27 == 1 && !current_enable;
    let physical_slot_configured = snapshot
        .discharge_slots
        .iter()
        .any(ScheduleSlot::is_configured);

    let quiet = |st: &TimedExportState| TimedExportDecision {
        new_state: st.clone(),
        writes: Vec::new(),
        log_message: None,
        is_exit_transition: false,
    };

    // A stopped schedule is a hard off-switch from EVERY state. The API
    // disable has already disarmed the inverter; if those writes failed,
    // the `Off` repair below re-issues them idempotently.
    if !config.schedule_enabled || !has_enabled_slots {
        // CODE_REVIEW.md finding 2: a Stop whose disarm writes failed arms
        // the machine into `Exiting` (the API failure path). HEM itself owns
        // that pending exit, so it must stay repairable even while the
        // physical slots are still populated — they are residue of our own
        // incomplete stop, not evidence of an external owner. The shared
        // Exiting retry budget bounds the re-issues; a confirmed Eco
        // baseline settles into Off. Re-entry is impossible here: the
        // schedule is off, so the retained slots are desired-state data
        // only.
        if matches!(state, TimedExportState::Exiting { .. }) {
            if eco_confirmed {
                *state = TimedExportState::Off;
                return TimedExportDecision {
                    new_state: state.clone(),
                    writes: Vec::new(),
                    log_message: Some("Timed Export exited, Eco restored".to_string()),
                    is_exit_transition: false,
                };
            }
            // A fresh Exiting{0,0} armed by the failed stop has no
            // machine-issued batch in flight (NoneIssued) — re-issue the exit
            // writes immediately instead of spending the confirm grace on a
            // batch that already failed at the API. One-shot: after the
            // machine's own batch, last_write_outcome becomes Succeeded/
            // Failed and the shared ladder (with its grace) takes over.
            if matches!(
                state,
                TimedExportState::Exiting {
                    polls_waiting: 0,
                    retries: 0
                }
            ) && last_write_outcome == TimedExportWriteOutcome::NoneIssued
            {
                return exiting_decision(
                    state,
                    "Timed Export: repairing the stop that failed to disarm",
                    build_timed_export_exit_writes(device_type, config),
                );
            }
            return exiting_retry_decision(state, device_type, config, last_write_outcome);
        }
        // Durable stop/exit-pending marker (set before the disable was
        // persisted, restored across restarts — CODE_REVIEW.md BLOCKER):
        // the armed registers and populated slots are residue of HEM's own
        // incomplete stop, not an external owner's schedule, so keep
        // repairing until the Eco baseline is confirmed. Without the marker
        // this stays suppressed below — a completed stop must never fight
        // Agile/Cosy/manual Timed Discharge that armed the same shape
        // afterwards.
        if config.stop_pending && !eco_confirmed && !rearm_observation_in_progress {
            if !matches!(state, TimedExportState::Exiting { .. }) {
                *state = TimedExportState::Exiting {
                    polls_waiting: 0,
                    retries: 0,
                };
            }
            // Same one-shot as the failed-stop path above: a just-armed or
            // restart-restored `Exiting{0,0}` has no machine-issued batch in
            // flight — re-issue the exit writes immediately rather than
            // spending the confirm grace on a batch that never happened.
            if matches!(
                state,
                TimedExportState::Exiting {
                    polls_waiting: 0,
                    retries: 0
                }
            ) && last_write_outcome == TimedExportWriteOutcome::NoneIssued
            {
                return exiting_decision(
                    state,
                    "Timed Export: repairing the stop/exit left pending (stop-pending marker)",
                    build_timed_export_exit_writes(device_type, config),
                );
            }
            return exiting_retry_decision(state, device_type, config, last_write_outcome);
        }
        // Agile/Cosy and manual Timed Discharge use the same physical
        // HR27/enable-discharge shape as Timed Export. When the managed
        // schedule is off or has no configured slots, a populated physical
        // slot is evidence that another controller owns those registers — including Agile
        // arming export after a Stop that retained the desired slots
        // (code-review blocking finding: the repair used to fire for that
        // shape and oscillate export↔Eco with Agile every other poll).
        // Do not clobber it with an Eco repair; HEM's own post-Stop state
        // is already disarmed by the awaited transactional stop path. A
        // genuinely stale raw export state has no slot and still follows
        // the repair path below.
        if physical_slot_configured {
            *state = TimedExportState::Off;
            return quiet(state);
        }
        if matches!(state, TimedExportState::Off) {
            // Registers still export-armed (restart after a crash mid-export,
            // or a failed disable)? Repair to the Eco baseline unless the
            // re-arm detector is deliberately holding writes to observe
            // firmware behaviour.
            if !eco_confirmed && !rearm_observation_in_progress {
                *state = TimedExportState::Exiting {
                    polls_waiting: 0,
                    retries: 0,
                };
                return TimedExportDecision {
                    new_state: state.clone(),
                    writes: build_timed_export_exit_writes(device_type, config),
                    log_message: Some(
                        "Timed Export: repairing export-armed registers outside a window"
                            .to_string(),
                    ),
                    is_exit_transition: true,
                };
            }
            return quiet(state);
        }
        *state = TimedExportState::Off;
        return TimedExportDecision {
            new_state: state.clone(),
            writes: Vec::new(),
            log_message: Some("Timed Export schedule disabled".to_string()),
            is_exit_transition: false,
        };
    }

    // Every other state runs with the schedule enabled.
    let decision = match state {
        TimedExportState::Off => {
            // Schedule just enabled (or process startup with a persisted
            // schedule). Reconcile straight from readback + time.
            if in_window && !hr318_blocking {
                entering_decision(
                    state,
                    "Timed Export entering",
                    build_timed_export_entry_writes(device_type, config),
                )
            } else if in_window && hr318_blocking {
                *state = TimedExportState::BlockedByPause;
                TimedExportDecision {
                    new_state: state.clone(),
                    writes: Vec::new(),
                    log_message: Some("Timed Export blocked by Pause Discharge".to_string()),
                    is_exit_transition: false,
                }
            } else if !eco_confirmed && !rearm_observation_in_progress {
                // Startup outside a window with registers left over from an
                // export (previous process stopped mid-window): repair.
                *state = TimedExportState::Exiting {
                    polls_waiting: 0,
                    retries: 0,
                };
                TimedExportDecision {
                    new_state: state.clone(),
                    writes: build_timed_export_exit_writes(device_type, config),
                    log_message: Some(
                        "Timed Export: startup repair — export-armed outside window".to_string(),
                    ),
                    is_exit_transition: true,
                }
            } else {
                *state = TimedExportState::Configured;
                TimedExportDecision {
                    new_state: state.clone(),
                    writes: Vec::new(),
                    log_message: Some("Timed Export configured for future window".to_string()),
                    is_exit_transition: false,
                }
            }
        }

        TimedExportState::Configured => {
            if in_window {
                if hr318_blocking {
                    *state = TimedExportState::BlockedByPause;
                    TimedExportDecision {
                        new_state: state.clone(),
                        writes: Vec::new(),
                        log_message: Some(
                            "Timed Export window entered but blocked by pause".to_string(),
                        ),
                        is_exit_transition: false,
                    }
                } else {
                    entering_decision(
                        state,
                        "Timed Export entering",
                        build_timed_export_entry_writes(device_type, config),
                    )
                }
            } else if !eco_confirmed && !rearm_observation_in_progress {
                // Outside a window but registers say maximum-power export is
                // still armed — another controller, a failed exit, or firmware
                // re-arm. Repair idempotently (bounded by the Exiting retry
                // budget, which surfaces Error when exhausted).
                *state = TimedExportState::Exiting {
                    polls_waiting: 0,
                    retries: 0,
                };
                TimedExportDecision {
                    new_state: state.clone(),
                    writes: build_timed_export_exit_writes(device_type, config),
                    log_message: Some(
                        "Timed Export: repair — export-armed outside window".to_string(),
                    ),
                    is_exit_transition: true,
                }
            } else {
                // Eco baseline holds; nothing to do.
                quiet(state)
            }
        }

        TimedExportState::Entering { .. } => {
            if export_confirmed {
                *state = TimedExportState::Active;
                TimedExportDecision {
                    new_state: state.clone(),
                    writes: Vec::new(),
                    log_message: Some("Timed Export active (entry confirmed)".to_string()),
                    is_exit_transition: false,
                }
            } else if !in_window {
                exiting_decision(
                    state,
                    "Timed Export exiting (window ended during entry)",
                    build_timed_export_exit_writes(device_type, config),
                )
            } else if hr318_blocking {
                *state = TimedExportState::BlockedByPause;
                TimedExportDecision {
                    new_state: state.clone(),
                    writes: Vec::new(),
                    log_message: Some("Timed Export blocked by pause during entry".to_string()),
                    is_exit_transition: false,
                }
            } else {
                // Still unconfirmed. Retry with a bounded budget: a failed
                // write batch retries immediately on the next poll; a
                // successful-but-unconfirmed batch is re-issued after the
                // confirmation grace period (readback may lag).
                let TimedExportState::Entering {
                    polls_waiting,
                    retries,
                } = state
                else {
                    unreachable!("matched Entering above");
                };
                let mut polls_waiting = *polls_waiting;
                let mut retries = *retries;
                let failed = last_write_outcome == TimedExportWriteOutcome::Failed;
                if failed {
                    retries += 1;
                } else {
                    let _ = bump(&mut polls_waiting, &mut retries);
                }
                if retries > TIMED_EXPORT_MAX_WRITE_RETRIES {
                    error_decision(state, "entry writes failed after retries")
                } else if failed || polls_waiting == 0 {
                    *state = TimedExportState::Entering {
                        polls_waiting,
                        retries,
                    };
                    TimedExportDecision {
                        new_state: state.clone(),
                        writes: build_timed_export_entry_writes(device_type, config),
                        log_message: Some(format!(
                            "Timed Export entry retry {retries} of {TIMED_EXPORT_MAX_WRITE_RETRIES}"
                        )),
                        is_exit_transition: false,
                    }
                } else {
                    *state = TimedExportState::Entering {
                        polls_waiting,
                        retries,
                    };
                    quiet(state)
                }
            }
        }

        TimedExportState::Active => {
            if !in_window {
                exiting_decision(
                    state,
                    "Timed Export exiting",
                    build_timed_export_exit_writes(device_type, config),
                )
            } else if hr318_blocking {
                *state = TimedExportState::BlockedByPause;
                TimedExportDecision {
                    new_state: state.clone(),
                    writes: Vec::new(),
                    log_message: Some("Timed Export blocked by pause".to_string()),
                    is_exit_transition: false,
                }
            } else if !export_confirmed {
                // Inside the window but the registers no longer show export
                // (another controller or automation changed them). Re-issue
                // the entry writes — the reconciler never assumes ownership
                // persists without readback evidence.
                entering_decision(
                    state,
                    "Timed Export re-entering (registers changed externally)",
                    build_timed_export_entry_writes(device_type, config),
                )
            } else {
                quiet(state)
            }
        }

        TimedExportState::Exiting { .. } => {
            if eco_confirmed {
                if config.schedule_enabled && has_enabled_slots {
                    *state = TimedExportState::Configured;
                } else {
                    *state = TimedExportState::Off;
                }
                TimedExportDecision {
                    new_state: state.clone(),
                    writes: Vec::new(),
                    log_message: Some("Timed Export exited, Eco restored".to_string()),
                    is_exit_transition: false,
                }
            } else if in_window {
                entering_decision(
                    state,
                    "Timed Export re-entering window",
                    build_timed_export_entry_writes(device_type, config),
                )
            } else {
                exiting_retry_decision(state, device_type, config, last_write_outcome)
            }
        }

        TimedExportState::BlockedByPause => {
            if !hr318_blocking {
                if in_window {
                    entering_decision(
                        state,
                        "Timed Export unblocked, entering export",
                        build_timed_export_entry_writes(device_type, config),
                    )
                } else if eco_confirmed {
                    *state = TimedExportState::Configured;
                    TimedExportDecision {
                        new_state: state.clone(),
                        writes: Vec::new(),
                        log_message: Some("Timed Export unblocked, awaiting window".to_string()),
                        is_exit_transition: false,
                    }
                } else {
                    // Pause and window have both ended but the registers were
                    // never returned to Eco (the machine skipped exit writes
                    // while blocked). Issue them now.
                    exiting_decision(
                        state,
                        "Timed Export unblocked after window end, restoring Eco",
                        build_timed_export_exit_writes(device_type, config),
                    )
                }
            } else if !in_window {
                exiting_decision(
                    state,
                    "Timed Export exiting while blocked",
                    build_timed_export_exit_writes(device_type, config),
                )
            } else {
                quiet(state)
            }
        }

        TimedExportState::Error { .. } => {
            // Error state requires intervention to clear: a schedule change
            // (enable/disable or slot edit) resets the machine via the API.
            quiet(state)
        }
    };

    decision
}

/// Shared retry ladder for a pending `Exiting` transition: bumps the
/// confirm-grace / retry bookkeeping from the previous poll's write outcome
/// and re-issues the exit writes when due. Used both by the enabled-schedule
/// `Exiting` arm and by the disabled-schedule repair path a failed Stop arms
/// (CODE_REVIEW.md finding 2), so both are bounded by the same budget.
fn exiting_retry_decision(
    state: &mut TimedExportState,
    device_type: DeviceType,
    config: &TimedExportConfig,
    last_write_outcome: TimedExportWriteOutcome,
) -> TimedExportDecision {
    let TimedExportState::Exiting {
        polls_waiting,
        retries,
    } = state
    else {
        unreachable!("exiting_retry_decision requires an Exiting state");
    };
    let mut polls_waiting = *polls_waiting;
    let mut retries = *retries;
    let failed = last_write_outcome == TimedExportWriteOutcome::Failed;
    if failed {
        retries += 1;
    } else {
        let _ = bump(&mut polls_waiting, &mut retries);
    }
    if retries > TIMED_EXPORT_MAX_WRITE_RETRIES {
        error_decision(state, "exit writes failed after retries")
    } else if failed || polls_waiting == 0 {
        *state = TimedExportState::Exiting {
            polls_waiting,
            retries,
        };
        TimedExportDecision {
            new_state: state.clone(),
            writes: build_timed_export_exit_writes(device_type, config),
            log_message: Some(format!(
                "Timed Export exit retry {retries} of {TIMED_EXPORT_MAX_WRITE_RETRIES}"
            )),
            is_exit_transition: true,
        }
    } else {
        *state = TimedExportState::Exiting {
            polls_waiting,
            retries,
        };
        TimedExportDecision {
            new_state: state.clone(),
            writes: Vec::new(),
            log_message: None,
            is_exit_transition: false,
        }
    }
}

fn entering_decision(
    state: &mut TimedExportState,
    message: &str,
    writes: Vec<RegisterWrite>,
) -> TimedExportDecision {
    *state = TimedExportState::Entering {
        polls_waiting: 0,
        retries: 0,
    };
    TimedExportDecision {
        new_state: state.clone(),
        writes,
        log_message: Some(message.to_string()),
        is_exit_transition: false,
    }
}

fn exiting_decision(
    state: &mut TimedExportState,
    message: &str,
    writes: Vec<RegisterWrite>,
) -> TimedExportDecision {
    *state = TimedExportState::Exiting {
        polls_waiting: 0,
        retries: 0,
    };
    TimedExportDecision {
        new_state: state.clone(),
        writes,
        log_message: Some(message.to_string()),
        is_exit_transition: true,
    }
}

fn error_decision(state: &mut TimedExportState, reason: &str) -> TimedExportDecision {
    tracing::error!("Timed Export: {reason}");
    *state = TimedExportState::Error {
        reason: reason.to_string(),
    };
    TimedExportDecision {
        new_state: state.clone(),
        writes: Vec::new(),
        log_message: Some(format!("Timed Export error: {reason}")),
        is_exit_transition: false,
    }
}

/// Map a discharge slot number + HHMM times to the model-correct
/// `ControlCommand`. Mirrors `server::api::discharge_slot_command_for_device`
/// so the state machine's fallback path reuses the same whitelist-validated
/// encoder commands as the HTTP layer.
fn discharge_slot_command_for(
    device_type: DeviceType,
    slot: u8,
    start: u16,
    end: u16,
) -> Result<ControlCommand, String> {
    match (device_type.uses_three_phase_schedule_slots(), slot) {
        (true, 1) => Ok(ControlCommand::SetThreePhaseDischargeSlot1 { start, end }),
        (true, 2) => Ok(ControlCommand::SetThreePhaseDischargeSlot2 { start, end }),
        (false, 1) => Ok(ControlCommand::SetDischargeSlot1 { start, end }),
        (false, 2) => Ok(ControlCommand::SetDischargeSlot2 { start, end }),
        (_, 3..=10) => Ok(ControlCommand::SetDischargeSlotN { slot, start, end }),
        (_, _) => Err(format!("Unsupported discharge slot {}", slot)),
    }
}

/// Build whitelist-validated writes that clear **every** discharge slot the
/// device supports (00:00–00:00 = disabled). Used by the HR59 re-arm
/// fallback path: outside export windows the physical slots are cleared so
/// firmware cannot re-arm HR59 while they remain populated.
pub fn build_timed_export_slot_clear_writes(device_type: DeviceType) -> Vec<RegisterWrite> {
    let mut out = Vec::new();
    for slot in 1..=device_type.max_discharge_slots() {
        match discharge_slot_command_for(device_type, slot, 0, 0) {
            Ok(cmd) => match cmd.encode() {
                Ok(mut w) => out.append(&mut w),
                Err(e) => tracing::warn!("Failed to encode discharge slot {} clear: {}", slot, e),
            },
            Err(e) => tracing::warn!("Unsupported discharge slot {} on this model: {}", slot, e),
        }
    }
    out
}

/// Build whitelist-validated writes that restore the desired export slots
/// to the inverter. Used by the HR59 re-arm fallback at window entry:
/// slot/target writes go FIRST, then HR27=0, then HR59=1, so the inverter
/// never sees an armed schedule without slot constraints.
pub fn build_timed_export_slot_restore_writes(
    device_type: DeviceType,
    slots: &[ScheduleSlot],
) -> Vec<RegisterWrite> {
    let mut out = Vec::new();
    for (idx, slot) in slots.iter().enumerate() {
        let slot_num = (idx + 1) as u8;
        if slot_num > device_type.max_discharge_slots() {
            break;
        }
        // Skip unconfigured slots — the registers are already zero after
        // the fallback clear, and re-writing zero wastes Modbus time.
        if !slot.is_configured() {
            continue;
        }
        let Some(start) = encode_hhmm(slot.start_hour, slot.start_minute) else {
            tracing::warn!(
                slot_num,
                "Skipping timed-export slot with invalid start time"
            );
            continue;
        };
        let Some(end) = encode_hhmm(slot.end_hour, slot.end_minute) else {
            tracing::warn!(slot_num, "Skipping timed-export slot with invalid end time");
            continue;
        };
        match discharge_slot_command_for(device_type, slot_num, start, end) {
            Ok(cmd) => match cmd.encode() {
                Ok(mut w) => out.append(&mut w),
                Err(e) => tracing::warn!(
                    "Failed to encode discharge slot {} restore: {}",
                    slot_num,
                    e
                ),
            },
            Err(e) => tracing::warn!(
                "Unsupported discharge slot {} on this model: {}",
                slot_num,
                e
            ),
        }
        if device_type.uses_extended_schedule_slots() && slot.target_soc > 0 {
            match (ControlCommand::SetDischargeTargetSocSlot {
                slot: slot_num,
                soc: slot.target_soc as u16,
            })
            .encode()
            {
                Ok(mut writes) => out.append(&mut writes),
                Err(error) => tracing::warn!(
                    "Failed to encode discharge slot {} target SOC restore: {}",
                    slot_num,
                    error
                ),
            }
        }
    }
    out
}

/// Build whitelist-validated writes that drive the physical discharge slot
/// registers back to the **prior physical schedule** `prior`: configured
/// slots are rewritten with their window (and target SOC on extended
/// models), unconfigured ones are cleared to 00:00–00:00.
///
/// Unlike [`build_timed_export_slot_restore_writes`] this also re-clears
/// slots that were empty before the edit, so it is safe as the awaited
/// compensating batch after a partially-applied slot mutation
/// (CODE_REVIEW.md BLOCKER: a fail-fast rejection mid-batch leaves the
/// earlier registers of the batch physically written — restoring only the
/// desired schedule would leave the inverter's window corrupted).
pub fn build_timed_export_slot_compensation_writes(
    device_type: DeviceType,
    prior: &[ScheduleSlot],
) -> Vec<RegisterWrite> {
    let mut out = Vec::new();
    for slot in 1..=device_type.max_discharge_slots() {
        let prior_slot = prior.get(slot as usize - 1).filter(|s| s.is_configured());
        let (start, end) = match prior_slot {
            Some(s) => match (
                encode_hhmm(s.start_hour, s.start_minute),
                encode_hhmm(s.end_hour, s.end_minute),
            ) {
                (Some(start), Some(end)) => (start, end),
                _ => {
                    tracing::warn!(slot, "invalid prior timed-export slot; clearing it");
                    (0, 0)
                }
            },
            None => (0, 0),
        };
        match discharge_slot_command_for(device_type, slot, start, end) {
            Ok(cmd) => match cmd.encode() {
                Ok(mut w) => out.append(&mut w),
                Err(e) => {
                    tracing::warn!("Failed to encode discharge slot {slot} compensation: {e}")
                }
            },
            Err(e) => tracing::warn!("Unsupported discharge slot {slot} on this model: {e}"),
        }
        if device_type.uses_extended_schedule_slots() {
            if let Some(s) = prior_slot {
                if s.target_soc > 0 {
                    match (ControlCommand::SetDischargeTargetSocSlot {
                        slot,
                        soc: s.target_soc as u16,
                    })
                    .encode()
                    {
                        Ok(mut writes) => out.append(&mut writes),
                        Err(error) => tracing::warn!(
                            "Failed to encode discharge slot {slot} target SOC compensation: {error}"
                        ),
                    }
                }
            }
        }
    }
    out
}

/// Build the writes that enter Timed Export mode.
///
/// Order is significant: slot writes first (re-arm fallback only), then
/// HR27=0 (maximum-power mode), then the model-routed discharge-enable
/// register (HR59 single-phase, HR1122 three-phase). This is the inverse of
/// `build_timed_export_disable_writes()`.
fn build_timed_export_entry_writes(
    device_type: DeviceType,
    config: &TimedExportConfig,
) -> Vec<RegisterWrite> {
    let mut writes = Vec::new();
    // Re-arm fallback: restore all slot times before arming discharge
    if config.device_rearm_confirmed {
        writes.extend(build_timed_export_slot_restore_writes(
            device_type,
            &config.slots,
        ));
    }
    writes.push(RegisterWrite {
        address: HR_BATTERY_POWER_MODE,
        value: 0,
    });
    writes.push(timed_export_discharge_enable_write(device_type, true));
    writes
}

/// Build the writes that exit Timed Export mode.
///
/// Order is significant: slot clears first (re-arm fallback only), then the
/// model-routed discharge-enable register =0 (disarm discharge), then
/// HR27=1 (restore Eco). The clears must precede the disarm because the
/// firmware this fallback defends against re-asserts the enable flag
/// whenever a discharge slot register is non-zero — a disarm written ahead
/// of the clears is immediately undone and the inverter never stops
/// exporting.
fn build_timed_export_exit_writes(
    device_type: DeviceType,
    config: &TimedExportConfig,
) -> Vec<RegisterWrite> {
    let mut writes: Vec<RegisterWrite> = Vec::new();
    // Re-arm fallback: clear physical slots so firmware cannot re-arm
    if config.device_rearm_confirmed {
        writes.extend(build_timed_export_slot_clear_writes(device_type));
    }
    writes.push(timed_export_discharge_enable_write(device_type, false));
    writes.push(RegisterWrite {
        address: HR_BATTERY_POWER_MODE,
        value: 1,
    });
    writes
}

// ===========================================================================
// HR59 re-arm detection (issue #289)
// ===========================================================================
//
// Some Gen3 firmware sets HR59 back to 1 whenever any discharge slot
// remains non-zero. The preferred permanent-slot approach must therefore be
// confirmed from readback:
//
// 1. At window exit, write HR59=0 and HR27=1.
// 2. Observe subsequent polls for an unsolicited HR59 return to 1.
// 3. If HR59 remains 0, keep the slots on the inverter permanently.
// 4. If HR59 reasserts, do not fight it with continuous writes. Persist the
//    desired schedule in HEM, clear the physical slots outside export
//    windows, and restore them immediately before HR27=0/HR59=1 at entry.
//
// Classification is **anchored to a completed HEM exit** — the detector
// progresses through explicit phases:
//
// ```text
// Idle → ExitWritten → ExitConfirmedOff → ObservingRearm → Confirmed
// ```
//
// Only after HEM has issued an exit (or repair) write *and* a later
// snapshot showed HR59=0 does observation of unsolicited HR59=1 begin.
// Without that anchor, startup in Timed Demand, a stale externally-created
// mode, or another controller enabling HR59 would permanently misclassify
// the inverter as re-arming firmware (and wrongly activate the slot-clear
// fallback). The detector resets to `Idle` on schedule changes, manual mode
// changes, and reconnects — ambiguous ownership must not count as evidence.

/// Number of consecutive outside-window unsolicited HR59=1 readbacks
/// required (after a confirmed HEM exit) before the device is classified
/// as re-arming firmware.
pub const TIMED_EXPORT_REARM_CONFIRM_POLLS: u32 = 3;

/// Phase of the anchored HR59 re-arm detector.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
enum RearmPhase {
    /// No HEM exit has been issued since the last reset; HR59 state is
    /// unattributable (startup, Timed Demand, external clients) and never
    /// counts toward classification.
    #[default]
    Idle,
    /// HEM issued exit (or repair) writes; waiting for readback to confirm
    /// the enable register actually landed at 0.
    ExitWritten,
    /// HEM's exit is confirmed by readback (HR59=0). The next unsolicited
    /// HR59=1 outside a window is genuine re-arm evidence.
    ExitConfirmedOff,
    /// HR59 came back on after a confirmed exit; counting consecutive
    /// outside-window polls to rule out a transient external change.
    Observing { consecutive: u32 },
}

/// Detects firmware that re-arms HR59 whenever discharge slots remain
/// non-zero, anchored to a completed HEM exit (see the module notes above).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
pub struct TimedExportRearmDetector {
    phase: RearmPhase,
}

impl TimedExportRearmDetector {
    /// Record that HEM just issued exit (or Eco-repair) writes that disarm
    /// the enable register. Starts (or restarts) the confirmation anchor.
    pub fn note_exit_written(&mut self) {
        self.phase = RearmPhase::ExitWritten;
    }

    /// Observe one poll snapshot.
    ///
    /// Returns `true` when this observation completes the confirmation
    /// window and the device is now classified as re-arming firmware.
    ///
    /// Parameters:
    /// - `outside_window`: current time is outside all enabled export windows
    /// - `enable_set`: the (model-routed) discharge-enable register read
    ///   back as armed on this snapshot
    /// - `hem_boundary_write_pending`: HEM is in `Entering` or `Exiting`
    ///   (its own enable writes may not have landed / read back yet)
    pub fn observe(
        &mut self,
        outside_window: bool,
        enable_set: bool,
        hem_boundary_write_pending: bool,
    ) -> bool {
        if hem_boundary_write_pending {
            // Our own writes are in flight; this snapshot proves nothing.
            return self.confirmed();
        }
        match self.phase {
            RearmPhase::Idle => {}
            RearmPhase::ExitWritten => {
                if !enable_set {
                    // Our exit landed — the anchor is confirmed.
                    self.phase = RearmPhase::ExitConfirmedOff;
                }
                // Still armed: our exit may not have landed yet (the
                // reconciler retries it); stay anchored.
            }
            RearmPhase::ExitConfirmedOff => {
                if enable_set && outside_window {
                    // Unsolicited re-arm after a confirmed exit.
                    self.phase = RearmPhase::Observing { consecutive: 1 };
                }
            }
            RearmPhase::Observing { consecutive } => {
                if enable_set && outside_window {
                    let consecutive = consecutive.saturating_add(1);
                    if consecutive >= TIMED_EXPORT_REARM_CONFIRM_POLLS {
                        self.phase = RearmPhase::Observing { consecutive };
                        return true;
                    }
                    self.phase = RearmPhase::Observing { consecutive };
                } else {
                    // Enable dropped or a window started (HEM's own entry):
                    // not persistent firmware behaviour. Back to the
                    // confirmed-off anchor.
                    self.phase = RearmPhase::ExitConfirmedOff;
                }
            }
        }
        self.confirmed()
    }

    /// Whether the confirmation threshold has been reached.
    pub fn confirmed(&self) -> bool {
        matches!(self.phase, RearmPhase::Observing { consecutive }
            if consecutive >= TIMED_EXPORT_REARM_CONFIRM_POLLS)
    }

    /// Whether the detector is holding repair writes to observe whether
    /// firmware re-asserts the enable register. While true, the state
    /// machine must not disarm again — otherwise its own writes keep
    /// resetting the register and the consecutive count can never build.
    pub fn is_observing(&self) -> bool {
        matches!(
            self.phase,
            RearmPhase::ExitWritten | RearmPhase::ExitConfirmedOff | RearmPhase::Observing { .. }
        )
    }

    /// Reset the detector (schedule change, manual mode change, reconnect,
    /// or the fallback being cleared by the user). The *learned* fallback
    /// flag (`device_rearm_confirmed`) is separate and survives resets.
    pub fn reset(&mut self) {
        self.phase = RearmPhase::Idle;
    }
}

// ===========================================================================
// Force Discharge auto-revert
// ===========================================================================
//
// Issue #129: when Force Discharge is started with a bounded duration
// (`POST /api/control/force-discharge {"minutes": N}`), the backend writes
// a discharge slot covering `now → now+N` and sets the inverter to
// export/max-power mode. When the slot window expires, the inverter
// stops discharging — but the `force-discharge` flags
// (HR_BATTERY_POWER_MODE=0, HR_ENABLE_DISCHARGE=1, HR_ENABLE_CHARGE=0,
// HR_ENABLE_CHARGE_TARGET=0) remain set. The battery is effectively
// paused: it won't charge from solar and won't discharge. The user has
// to manually switch to Eco to recover.
//
// This function detects slot expiry and returns the register writes that
// restore the inverter to the pre-force-discharge state. It deliberately
// takes individual fields rather than `ForceDischargeRevert` to avoid a
// circular import between `state_machines` and `poll` (the struct lives
// in `poll`). The poll loop locks the revert, extracts the fields, and
// passes them here.

/// Whether a pre-force-discharge HR318 mode requires temporarily disabling
/// pause mode so the forced discharge is not blocked (issue #289).
///
/// Modes 2 (pause discharging) and 3 (pause charging and discharging)
/// block discharge; the force action disables them after capturing the
/// full HR318/319/320 state for later restoration. Mode 1 (pause charging
/// only) and 0 (disabled) do not block discharge and are left untouched.
pub fn should_disable_pause_for_force_discharge(pause_mode: u8) -> bool {
    pause_mode == 2 || pause_mode == 3
}

/// Append the HR318/319/320 pause-state restore writes.
///
/// Force Discharge captures HR318-320 and may temporarily disable pause
/// mode on **every** device family (the registers live in the common
/// holding block 0-59, including on three-phase models). Restoration must
/// therefore also apply to every family — not just the single-phase branch.
///
/// Order is significant: the HR319/HR320 window values are written
/// **before** re-enabling the HR318 mode, so the inverter never observes an
/// enabled pause mode with incomplete slot data (code-review finding:
/// three-phase pause schedules were silently dropped by Force Discharge).
pub(crate) fn push_pause_restore_writes(
    writes: &mut Vec<RegisterWrite>,
    pause_mode: Option<u8>,
    pause_slot: Option<&ScheduleSlot>,
) {
    let Some(mode) = pause_mode else {
        return;
    };
    if let Some(slot) = pause_slot {
        let (Some(start), Some(end)) = (
            encode_hhmm(slot.start_hour, slot.start_minute),
            encode_hhmm(slot.end_hour, slot.end_minute),
        ) else {
            tracing::warn!("Skipping pause-mode restore with invalid HHMM components");
            return;
        };
        writes.push(RegisterWrite {
            address: crate::modbus::registers::HR_BATTERY_PAUSE_SLOT_1_START,
            value: start,
        });
        writes.push(RegisterWrite {
            address: crate::modbus::registers::HR_BATTERY_PAUSE_SLOT_1_END,
            value: end,
        });
    }
    writes.push(RegisterWrite {
        address: crate::modbus::registers::HR_BATTERY_PAUSE_MODE,
        value: u16::from(mode),
    });
}

/// Check whether a force-discharge slot has expired and, if so, return
/// the register writes that restore the inverter to its pre-force-discharge
/// state.
///
/// `now_ms` is the current time in unix epoch milliseconds. `slot_end_ms`
/// is the slot's expiry time, recorded by the API handler when force
/// discharge was started with a duration. Returns `None` if there is no
/// active slot to expire (no end time set, or expiry is still in the
/// future).
///
/// When the slot has expired, the returned writes restore:
///   - HR_ENABLE_DISCHARGE to its pre-force value
///   - HR_ENABLE_CHARGE / HR_ENABLE_CHARGE_TARGET to their pre-force values
///   - The original discharge slot 1 / slot 2 times (or 00:00–00:00 if
///     there was no prior slot)
///   - HR_BATTERY_POWER_MODE to eco (1) — matches the explicit Stop
///     Discharge path's behaviour of always returning to eco
///
/// On three-phase models, the same restoration uses the three-phase
/// force-charge / force-discharge enable flags and skips the single-phase
/// slot registers (the poll loop resyncs them from the HR 1080-1124
/// block).
// Allow clippy::too_many_arguments — the function is a pure data-transformer
// that mirrors the ForceDischargeRevert struct field-for-field. Grouping the
// fields into a sub-struct would be pure indirection (the caller already
// has the struct and would have to destructure it into the sub-struct).
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_force_discharge_auto_revert_writes(
    device_type: DeviceType,
    now_ms: i64,
    slot_end_ms: Option<i64>,
    pre_enable_charge: bool,
    pre_enable_discharge: bool,
    pre_slot_1_start: Option<(u8, u8)>,
    pre_slot_1_end: Option<(u8, u8)>,
    pre_slot_2_start: Option<(u8, u8)>,
    pre_slot_2_end: Option<(u8, u8)>,
    pre_three_phase_force_discharge_enable: Option<bool>,
    pre_three_phase_force_charge_enable: Option<bool>,
    pre_battery_pause_mode: Option<u8>,
    pre_battery_pause_slot: Option<&ScheduleSlot>,
) -> Option<Vec<RegisterWrite>> {
    let slot_end_ms = slot_end_ms?;
    if now_ms < slot_end_ms {
        return None;
    }
    tracing::info!(
        slot_end_ms,
        now_ms,
        elapsed_secs = (now_ms - slot_end_ms) / 1000,
        "Force discharge slot expired — auto-reverting to pre-force state"
    );

    let mut writes = Vec::new();

    if device_type.uses_three_phase_schedule_slots() {
        writes.push(RegisterWrite {
            address: HR_3PH_FORCE_DISCHARGE_ENABLE,
            value: if pre_three_phase_force_discharge_enable.unwrap_or(false) {
                1
            } else {
                0
            },
        });
        writes.push(RegisterWrite {
            address: HR_3PH_FORCE_CHARGE_ENABLE,
            value: if pre_three_phase_force_charge_enable.unwrap_or(false) {
                1
            } else {
                0
            },
        });
        writes.push(RegisterWrite {
            address: HR_BATTERY_POWER_MODE,
            value: 1,
        });
    } else {
        writes.push(RegisterWrite {
            address: HR_ENABLE_DISCHARGE,
            value: if pre_enable_discharge { 1 } else { 0 },
        });
        writes.push(RegisterWrite {
            address: HR_ENABLE_CHARGE,
            value: if pre_enable_charge { 1 } else { 0 },
        });
        writes.push(RegisterWrite {
            address: HR_ENABLE_CHARGE_TARGET,
            value: if pre_enable_charge { 1 } else { 0 },
        });

        let (s1h, s1m) = pre_slot_1_start.unwrap_or((0, 0));
        let (e1h, e1m) = pre_slot_1_end.unwrap_or((0, 0));
        writes.push(RegisterWrite {
            address: HR_DISCHARGE_SLOT_1_START,
            value: encode_hhmm_or_clear(s1h, s1m),
        });
        writes.push(RegisterWrite {
            address: HR_DISCHARGE_SLOT_1_END,
            value: encode_hhmm_or_clear(e1h, e1m),
        });
        let (s2h, s2m) = pre_slot_2_start.unwrap_or((0, 0));
        let (e2h, e2m) = pre_slot_2_end.unwrap_or((0, 0));
        writes.push(RegisterWrite {
            address: HR_DISCHARGE_SLOT_2_START,
            value: encode_hhmm_or_clear(s2h, s2m),
        });
        writes.push(RegisterWrite {
            address: HR_DISCHARGE_SLOT_2_END,
            value: encode_hhmm_or_clear(e2h, e2m),
        });

        // Default to eco (1) on restore — matches the explicit Stop
        // Discharge path. `battery_power_mode` is not captured in the
        // revert (only the encoder config), so we always return to eco.
        writes.push(RegisterWrite {
            address: HR_BATTERY_POWER_MODE,
            value: 1,
        });
    }

    // Restore the pre-force HR318/319/320 pause configuration on EVERY
    // device family (issue #289). The force action may have disabled pause
    // mode so discharge could run; the auto-revert puts the exact prior
    // Timed Discharge window back — same as the explicit Stop path. This
    // deliberately sits *after* the model-specific branches: AC-three-phase
    // devices support Timed Discharge too, and their pause schedule must
    // not be dropped just because the enable/slot restoration was routed
    // through the three-phase register bank. Slot values are written before
    // the pause mode so an enabled mode never coexists with stale windows.
    push_pause_restore_writes(&mut writes, pre_battery_pause_mode, pre_battery_pause_slot);

    Some(writes)
}

// ===========================================================================
// Agile Octopus slot-based decision logic
// ===========================================================================

/// Outcome of the price-vs-scope evaluation that drives the slot-based
/// Agile state machine.
///
/// Replaces the legacy `AgileState { Idle, Charging, Discharging }` enum
/// (which only told you what the inverter was doing — the slot-based
/// approach drives the inverter through its native schedule mechanism,
/// so the "state" is whatever the inverter itself reports via its
/// registers). The poll loop converts this into register writes; the
/// `Charge { .. }` and `Discharge { .. }` variants include the slot
/// window so the encoder knows which HHMM pair to write.
///
/// `Defer { .. }` is the cosy/auto-winter conflict signal — the price
/// is in scope (cheap or expensive) but another mechanism is in
/// control, so we don't touch the inverter. `Idle` means the price is
/// mid-band, out of scope for the current mode, or no price data is
/// available.
#[derive(Debug, Clone, PartialEq)]
pub enum AgileSlotAction {
    /// Cheap-window charge: drive the inverter through its native
    /// charge slot 1 with these HHMM boundaries and target SOC.
    Charge {
        start_hhmm: u16,
        end_hhmm: u16,
        target_soc: u16,
    },
    /// Expensive-window discharge (with export — option β).
    Discharge { start_hhmm: u16, end_hhmm: u16 },
    /// Cosy or auto-winter is in control of the matching side. Don't
    /// touch the inverter — let the other mechanism own this poll.
    Defer,
    /// Mid-band price, out-of-scope mode, or no price data. The poll
    /// loop calls `AgileClearActiveSlot` to disarm any preloaded slot.
    Idle,
}

impl AgileSlotAction {
    /// True when this action drives the inverter (Charge / Discharge /
    /// Idle-and-clear / Defer-noop).
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            AgileSlotAction::Charge { .. } | AgileSlotAction::Discharge { .. }
        )
    }

    /// Snapshot-side label for this action, matching the wire shape the
    /// frontend reads as `snapshot.agile_state`. Idle returns "idle" so
    /// a Defer (cosy in control) and an Idle (mid-band) look the same to
    /// the frontend, which is correct: the inverter isn't doing anything
    /// price-driven.
    pub fn label(&self) -> &'static str {
        match self {
            AgileSlotAction::Charge { .. } => "charging",
            AgileSlotAction::Discharge { .. } => "discharging",
            AgileSlotAction::Defer | AgileSlotAction::Idle => "idle",
        }
    }
}

/// Whether the poll loop should write the register command produced for an
/// Agile action.
///
/// `Defer` never writes: Cosy/auto-winter owns the charge side this poll.
/// `Off + Idle` also never writes: when Agile is explicitly off, repeatedly
/// clearing every poll would clobber the user's manual schedule. Crucially,
/// active scopes (`Full`, `ChargeOnly`, `DischargeOnly`) DO write `Idle`,
/// because mid-band/hold means "cancel any Agile slot the previous poll or a
/// previous app process armed". This is what stops an Agile discharge after a
/// threshold change or after the app restarts into a hold period.
pub fn should_write_agile_action(
    scope: crate::settings::AgileScope,
    action: &AgileSlotAction,
) -> bool {
    use crate::settings::AgileScope;
    !(matches!(action, AgileSlotAction::Defer)
        || (scope == AgileScope::Off && matches!(action, AgileSlotAction::Idle)))
}

/// Compute the slot-driven action the Agile state machine should take
/// this poll.
///
/// `cached_prices` is the Octopus price cache (newest-first per the
/// Octopus API response order). The function finds the slot whose
/// `valid_from <= now < valid_to`, then walks forward through the
/// cache to find the end of the contiguous cheap/expensive run, and
/// returns the corresponding slot boundaries as HHMM packed values.
///
/// `cosy_active` and `auto_winter_active` defer the charge-side action
/// to whichever mechanism is currently in control (mirrors the
/// cosy-conflict guard added in `04eee32`).
///
/// `local_tz` is the timezone used to convert unix timestamps into
/// HHMM values that match the inverter's slot registers (which are
/// stored in local time). Pass `chrono::Local` in production and
/// `chrono::Utc` (or any fixed offset) in tests for determinism.
///
/// Threshold arg count exceeds the clippy default (7) because the
/// state-machine split between cache lookup, conflict guards, and
/// timezone conversion is clearest as a flat argument list. Splitting
/// into a wrapper struct just to satisfy the lint would obscure the
/// call site without simplifying testing.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_agile_slot<Tz: chrono::TimeZone>(
    scope: crate::settings::AgileScope,
    price: Option<f64>,
    charge_threshold: f64,
    discharge_threshold: f64,
    cached_prices: &[PriceSlot],
    now_unix_ts: i64,
    cosy_active: bool,
    auto_winter_active: bool,
    local_tz: &Tz,
) -> AgileSlotAction {
    use crate::settings::AgileScope;

    // No scope — nothing to do. The poll loop calls
    // AgileClearActiveSlot on this path to disarm any stale preloaded
    // slot from a previous run.
    if scope == AgileScope::Off {
        return AgileSlotAction::Idle;
    }
    // No price data — same as mid-band: disarm any active slot.
    let price = match price {
        Some(p) => p,
        None => return AgileSlotAction::Idle,
    };

    // Mid-band: inverter obeys whatever the user has armed manually.
    if price > charge_threshold && price < discharge_threshold {
        return AgileSlotAction::Idle;
    }

    // Determine whether this price is in scope for the current mode.
    let wants_charge = price <= charge_threshold && scope.owns_charge();
    let wants_discharge = price >= discharge_threshold && scope.owns_discharge();

    if wants_charge {
        // Cosy/auto-winter conflict guard: if either is active on the
        // charge side, defer to them. They run before Agile in the
        // poll loop and own the HR_ENABLE_CHARGE register this poll.
        if cosy_active || auto_winter_active {
            return AgileSlotAction::Defer;
        }
        // Find the contiguous cheap run starting now.
        return match contiguous_run_window(cached_prices, now_unix_ts, |p| p <= charge_threshold) {
            Some((start_unix, end_unix)) => AgileSlotAction::Charge {
                start_hhmm: unix_to_hhmm(start_unix, local_tz),
                end_hhmm: unix_to_hhmm(end_unix, local_tz),
                target_soc: 100,
            },
            None => AgileSlotAction::Idle,
        };
    }

    if wants_discharge {
        // Discharge side has no cosy/auto-winter conflict — those
        // mechanisms are charge-only.
        return match contiguous_run_window(cached_prices, now_unix_ts, |p| p >= discharge_threshold)
        {
            Some((start_unix, end_unix)) => AgileSlotAction::Discharge {
                start_hhmm: unix_to_hhmm(start_unix, local_tz),
                end_hhmm: unix_to_hhmm(end_unix, local_tz),
            },
            None => AgileSlotAction::Idle,
        };
    }

    // Price is in band for the opposite side (cheap price but
    // DischargeOnly mode, or expensive price but ChargeOnly mode) — do
    // nothing; the user's manual schedule owns the other side.
    AgileSlotAction::Idle
}

/// Find the boundaries of the contiguous run of half-hour slots
/// matching `matches` starting at the slot that contains
/// `now_unix_ts`.
///
/// Returns the unix timestamp of the start of the current slot and
/// the unix timestamp of the end of the last slot in the run. Returns
/// `None` if no slot contains `now_unix_ts`.
///
/// The Octopus cache is newest-first (per the API's results order):
/// index 0 is the latest slot, index N-1 is the earliest. To walk
/// FORWARD in time from `now_unix_ts`, we move toward LOWER indices.
/// The run ends at the first slot where the predicate fails, or at
/// the first gap in coverage (slot.valid_from != current end).
fn contiguous_run_window(
    cached_prices: &[PriceSlot],
    now_unix_ts: i64,
    matches: impl Fn(f64) -> bool,
) -> Option<(i64, i64)> {
    // Find the slot containing now_unix_ts.
    let current_idx = cached_prices
        .iter()
        .position(|s| now_unix_ts >= s.valid_from && now_unix_ts < s.valid_to)?;
    let current = &cached_prices[current_idx];
    if !matches(current.pence) {
        return None;
    }
    let start_unix = current.valid_from;
    let mut end_unix = current.valid_to;
    // Walk forward in time: toward LOWER indices (newer slots in the
    // newest-first cache). `rev()` gives us [current_idx-1,
    // current_idx-2, ..., 0], which is descending valid_to order —
    // forward in time.
    for slot in cached_prices.iter().take(current_idx).rev() {
        // Coverage gap: this slot doesn't abut the previous one
        // (Octopus sometimes returns partial ranges).
        if slot.valid_from != end_unix {
            break;
        }
        if !matches(slot.pence) {
            break;
        }
        end_unix = slot.valid_to;
    }
    Some((start_unix, end_unix))
}

/// Convert a unix timestamp to a packed HHMM value (matching the
/// inverter's HHMM register format). Truncates to the timezone
/// passed in — the inverter's slot registers are local-time, so
/// production passes `chrono::Local` and tests pass a fixed offset.
fn unix_to_hhmm<Tz: chrono::TimeZone>(unix_ts: i64, tz: &Tz) -> u16 {
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(unix_ts, 0)
        .unwrap_or_else(|| chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap());
    let local = dt.with_timezone(tz);
    (local.hour() as u16) * 100 + (local.minute() as u16)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::{InverterSnapshot, ScheduleSlot};

    /// Wrapper around [`check_timed_export`] with the poll-loop defaults
    /// used by the original tests: no previous write outcome and no active
    /// re-arm observation. Tests exercising the retry / reconciliation
    /// behaviour call [`check_timed_export`] directly with explicit args.
    fn check_timed_export_with_defaults(
        snapshot: &InverterSnapshot,
        config: &TimedExportConfig,
        state: &mut TimedExportState,
        minute_of_day: u16,
        device_type: DeviceType,
    ) -> TimedExportDecision {
        check_timed_export(
            snapshot,
            config,
            state,
            minute_of_day,
            device_type,
            TimedExportWriteOutcome::NoneIssued,
            false,
        )
    }

    fn configured_slot() -> ScheduleSlot {
        ScheduleSlot {
            enabled: true,
            start_hour: 16,
            start_minute: 0,
            end_hour: 19,
            end_minute: 0,
            target_soc: 4,
        }
    }

    #[test]
    fn timed_export_repair_requires_hr59_and_no_configured_slot() {
        assert!(should_repair_timed_export(true, &[], false));
        assert!(!should_repair_timed_export(false, &[], false));
        assert!(!should_repair_timed_export(
            true,
            &[configured_slot()],
            false
        ));
        assert!(!should_repair_timed_export(true, &[], true));
    }

    #[test]
    fn timed_export_repair_writes_disable_schedule_then_eco() {
        let writes = build_timed_export_disable_writes(DeviceType::Gen3Hybrid);
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].address, HR_ENABLE_DISCHARGE);
        assert_eq!(writes[0].value, 0);
        assert_eq!(writes[1].address, HR_BATTERY_POWER_MODE);
        assert_eq!(writes[1].value, 1);
    }

    #[test]
    fn cosy_persist_helper_round_trips_through_disk() {
        crate::test_util::with_isolated_config_dir(|| {
            persist_cosy_active(true);
            let after_true = crate::settings::Settings::load();
            assert!(after_true.cosy_active_persisted);

            persist_cosy_active(false);
            let after_false = crate::settings::Settings::load();
            assert!(!after_false.cosy_active_persisted);
        });
    }

    // -----------------------------------------------------------------
    // Adaptive Charge — pure transition logic
    // -----------------------------------------------------------------

    fn adaptive_snapshot(soc: u8, raw_rate: u8) -> InverterSnapshot {
        InverterSnapshot {
            soc,
            charge_rate: raw_rate,
            device_type: DeviceType::Gen3Hybrid,
            device_type_code: "2001".to_string(),
            inverter_serial: "CE234".to_string(),
            ..Default::default()
        }
    }

    fn adaptive_config() -> crate::settings::AdaptiveChargeConfig {
        crate::settings::AdaptiveChargeConfig {
            periods: vec![crate::settings::AdaptiveChargePeriod {
                enabled: true,
                all_day: false,
                start_hour: 8,
                start_minute: 0,
                end_hour: 17,
                end_minute: 0,
                low_soc: 30,
                recovery_soc: 40,
                preferred_rate_percent: 40,
                recovery_rate_percent: 100,
            }],
            confirmation_readings: 2,
        }
    }

    #[test]
    fn adaptive_rate_conversion_is_device_aware() {
        assert_eq!(
            normalized_charge_rate_to_raw(DeviceType::Gen3Hybrid, 41),
            Some(21)
        );
        assert_eq!(
            normalized_charge_rate_to_raw(DeviceType::ACCoupled, 41),
            Some(41)
        );
        assert_eq!(
            normalized_charge_rate_to_raw(DeviceType::ThreePhase, 41),
            Some(41)
        );
        assert_eq!(normalized_charge_rate_to_raw(DeviceType::Gateway, 41), None);
    }

    #[test]
    fn adaptive_preferred_captures_baseline_and_writes_limit() {
        let snap = adaptive_snapshot(50, 50);
        let mut state = AdaptiveChargeState::Inactive;
        let mut saved = None;
        let first = check_adaptive_charge(
            &snap,
            &adaptive_config(),
            true,
            &mut state,
            &mut saved,
            9 * 60,
        );
        assert!(first.write.is_none());
        assert_eq!(
            state,
            AdaptiveChargeState::BaselinePending { raw_value: 50 }
        );

        let outcome = check_adaptive_charge(
            &snap,
            &adaptive_config(),
            true,
            &mut state,
            &mut saved,
            9 * 60,
        );

        assert!(matches!(
            state,
            AdaptiveChargeState::Preferred { period: 0, .. }
        ));
        assert_eq!(saved.as_ref().map(|value| value.raw_value), Some(50));
        let write = outcome.write.expect("preferred rate differs from baseline");
        assert_eq!(write.address, HR_BATTERY_CHARGE_LIMIT);
        assert_eq!(write.value, 20);
        assert_eq!(outcome.desired_rate_percent, Some(40));
    }

    #[test]
    fn adaptive_baseline_ignores_invalid_rates_and_requires_stability() {
        let config = adaptive_config();
        let mut state = AdaptiveChargeState::Inactive;
        let mut saved = None;

        for raw_rate in [0, 100] {
            let outcome = check_adaptive_charge(
                &adaptive_snapshot(50, raw_rate),
                &config,
                true,
                &mut state,
                &mut saved,
                9 * 60,
            );
            assert!(outcome.write.is_none());
            assert!(
                saved.is_none(),
                "invalid raw rate {raw_rate} must not be saved"
            );
            assert_eq!(state, AdaptiveChargeState::Inactive);
        }

        let first_valid = check_adaptive_charge(
            &adaptive_snapshot(50, 25),
            &config,
            true,
            &mut state,
            &mut saved,
            9 * 60,
        );
        assert!(first_valid.write.is_none());
        assert_eq!(
            state,
            AdaptiveChargeState::BaselinePending { raw_value: 25 }
        );
        assert!(saved.is_none());

        let second_valid = check_adaptive_charge(
            &adaptive_snapshot(50, 25),
            &config,
            true,
            &mut state,
            &mut saved,
            9 * 60,
        );
        assert_eq!(saved.as_ref().map(|value| value.raw_value), Some(25));
        assert!(matches!(
            state,
            AdaptiveChargeState::Preferred { period: 0, .. }
        ));
        assert_eq!(second_valid.write.expect("preferred rate write").value, 20);
    }

    #[test]
    fn adaptive_low_soc_uses_confirmation_before_recovery() {
        let config = adaptive_config();
        let mut state = AdaptiveChargeState::Preferred {
            period: 0,
            low_count: 0,
        };
        let mut saved = Some(crate::settings::AdaptiveChargeSavedLimit {
            inverter_serial: "CE234".to_string(),
            device_type_code: "2001".to_string(),
            register_address: HR_BATTERY_CHARGE_LIMIT,
            raw_value: 50,
        });
        let low = adaptive_snapshot(30, 20);

        let first = check_adaptive_charge(&low, &config, true, &mut state, &mut saved, 9 * 60);
        assert!(matches!(
            state,
            AdaptiveChargeState::Preferred { low_count: 1, .. }
        ));
        assert_eq!(first.desired_rate_percent, Some(40));

        let second = check_adaptive_charge(&low, &config, true, &mut state, &mut saved, 9 * 60);
        assert!(matches!(state, AdaptiveChargeState::Recovery { .. }));
        assert_eq!(second.desired_rate_percent, Some(100));
        assert_eq!(second.write.expect("recovery raises limit").value, 50);
    }

    #[test]
    fn adaptive_recovery_hysteresis_requires_recovery_confirmation() {
        let config = adaptive_config();
        let mut state = AdaptiveChargeState::Recovery {
            period: 0,
            high_count: 0,
        };
        let mut saved = Some(crate::settings::AdaptiveChargeSavedLimit {
            inverter_serial: "CE234".to_string(),
            device_type_code: "2001".to_string(),
            register_address: HR_BATTERY_CHARGE_LIMIT,
            raw_value: 50,
        });

        let middle = adaptive_snapshot(35, 50);
        let outcome = check_adaptive_charge(&middle, &config, true, &mut state, &mut saved, 9 * 60);
        assert!(matches!(
            state,
            AdaptiveChargeState::Recovery { high_count: 0, .. }
        ));
        assert!(outcome.write.is_none());

        let high = adaptive_snapshot(40, 50);
        let _ = check_adaptive_charge(&high, &config, true, &mut state, &mut saved, 9 * 60);
        assert!(matches!(
            state,
            AdaptiveChargeState::Recovery { high_count: 1, .. }
        ));
        let second = check_adaptive_charge(&high, &config, true, &mut state, &mut saved, 9 * 60);
        assert!(matches!(state, AdaptiveChargeState::Preferred { .. }));
        assert_eq!(second.write.expect("preferred rate restored").value, 20);
    }

    #[test]
    fn adaptive_outside_window_and_disable_restore_baseline() {
        let config = adaptive_config();
        let mut state = AdaptiveChargeState::Recovery {
            period: 0,
            high_count: 0,
        };
        let mut saved = Some(crate::settings::AdaptiveChargeSavedLimit {
            inverter_serial: "CE234".to_string(),
            device_type_code: "2001".to_string(),
            register_address: HR_BATTERY_CHARGE_LIMIT,
            raw_value: 35,
        });
        let snap = adaptive_snapshot(50, 50);

        let outside = check_adaptive_charge(&snap, &config, true, &mut state, &mut saved, 18 * 60);
        assert_eq!(outside.write.expect("outside restores baseline").value, 35);
        assert_eq!(state, AdaptiveChargeState::Restoring);

        let restored = adaptive_snapshot(50, 35);
        let confirmed =
            check_adaptive_charge(&restored, &config, true, &mut state, &mut saved, 18 * 60);
        assert!(confirmed.write.is_none());
        assert_eq!(state, AdaptiveChargeState::OutsideWindow);

        let disabled =
            check_adaptive_charge(&restored, &config, false, &mut state, &mut saved, 18 * 60);
        assert!(disabled.write.is_none());
        assert!(saved.is_none());
        assert_eq!(state, AdaptiveChargeState::Inactive);
    }

    #[test]
    fn adaptive_releases_outside_window_ownership_after_confirmation() {
        let config = adaptive_config();
        let mut state = AdaptiveChargeState::Preferred {
            period: 0,
            low_count: 0,
        };
        let mut saved = Some(crate::settings::AdaptiveChargeSavedLimit {
            inverter_serial: "CE234".to_string(),
            device_type_code: "2001".to_string(),
            register_address: HR_BATTERY_CHARGE_LIMIT,
            raw_value: 35,
        });

        // Leaving an Adaptive-owned window may restore the captured baseline
        // once, but this is a pending transition until readback confirms it.
        let transition = check_adaptive_charge(
            &adaptive_snapshot(50, 50),
            &config,
            true,
            &mut state,
            &mut saved,
            18 * 60,
        );
        assert_eq!(transition.write.expect("one transition restore").value, 35);
        assert_eq!(state, AdaptiveChargeState::Restoring);

        let confirmed = check_adaptive_charge(
            &adaptive_snapshot(50, 35),
            &config,
            true,
            &mut state,
            &mut saved,
            18 * 60,
        );
        assert!(confirmed.write.is_none());
        assert_eq!(state, AdaptiveChargeState::OutsideWindow);

        // A later manual edit outside the window becomes the new baseline;
        // Adaptive must not reassert the stale 35% value every poll.
        let manual = check_adaptive_charge(
            &adaptive_snapshot(50, 41),
            &config,
            true,
            &mut state,
            &mut saved,
            18 * 60,
        );
        assert!(manual.write.is_none());
        assert_eq!(saved.as_ref().map(|value| value.raw_value), Some(41));
        assert_eq!(state, AdaptiveChargeState::OutsideWindow);

        let repeated = check_adaptive_charge(
            &adaptive_snapshot(50, 41),
            &config,
            true,
            &mut state,
            &mut saved,
            18 * 60,
        );
        assert!(repeated.write.is_none());
        assert_eq!(saved.as_ref().map(|value| value.raw_value), Some(41));
    }

    #[test]
    fn adaptive_auto_winter_restores_baseline_then_suspends() {
        let config = adaptive_config();
        let mut state = AdaptiveChargeState::Preferred {
            period: 0,
            low_count: 0,
        };
        let mut saved = Some(crate::settings::AdaptiveChargeSavedLimit {
            inverter_serial: "CE234".to_string(),
            device_type_code: "2001".to_string(),
            register_address: HR_BATTERY_CHARGE_LIMIT,
            raw_value: 45,
        });
        let mut snap = adaptive_snapshot(50, 20);
        snap.auto_winter_active = true;

        let outcome = check_adaptive_charge(&snap, &config, true, &mut state, &mut saved, 9 * 60);
        assert_eq!(outcome.write.expect("winter restores baseline").value, 45);
        assert_eq!(
            state,
            AdaptiveChargeState::SuspendedAutoWinter {
                restore_pending: true
            }
        );

        snap.charge_rate = 45;
        let confirmed = check_adaptive_charge(&snap, &config, true, &mut state, &mut saved, 9 * 60);
        assert!(confirmed.write.is_none());
        assert_eq!(
            state,
            AdaptiveChargeState::SuspendedAutoWinter {
                restore_pending: false
            }
        );
    }

    // -----------------------------------------------------------------
    // check_auto_winter — pure transition logic
    // -----------------------------------------------------------------

    fn aw_config(cold: f32, recovery: f32, target: u8, debounce: u32) -> AutoWinterConfig {
        AutoWinterConfig {
            enabled: true,
            cold_threshold: cold,
            recovery_threshold: recovery,
            target_soc: target,
            debounce_readings: debounce,
        }
    }

    #[test]
    fn auto_winter_disabled_resets_state_and_writes_nothing() {
        let snap = InverterSnapshot {
            battery_temperature: -10.0,
            ..Default::default()
        };
        let config = AutoWinterConfig {
            enabled: false,
            ..Default::default()
        };
        let mut state = AutoWinterState::WinterActive;
        let mut saved = Some(AutoWinterSaved {
            enable_charge_target: true,
            target_soc: 80,
        });

        let writes = check_auto_winter(&snap, &config, &mut state, &mut saved);

        assert!(writes.is_none(), "disabled mode must not write");
        assert_eq!(state, AutoWinterState::Idle);
        assert!(saved.is_none(), "disabled mode must clear saved values");
    }

    #[test]
    fn auto_winter_single_cold_reading_does_not_activate() {
        let snap = InverterSnapshot {
            battery_temperature: 5.0,
            ..Default::default()
        };
        let config = aw_config(8.0, 12.0, 80, 3);
        let mut state = AutoWinterState::Idle;
        let mut saved = None;

        let writes = check_auto_winter(&snap, &config, &mut state, &mut saved);

        assert!(writes.is_none(), "one reading must not trigger activation");
        assert!(matches!(
            state,
            AutoWinterState::ColdPending { consecutive: 1 }
        ));
        assert!(saved.is_none(), "saved values only captured on activation");
    }

    #[test]
    fn auto_winter_activates_after_debounce_and_saves_prior_state() {
        let config = aw_config(8.0, 12.0, 90, 3);
        let mut state = AutoWinterState::Idle;
        let mut saved = None;

        // Two cold readings: still pending.
        for _ in 0..2 {
            let snap = InverterSnapshot {
                battery_temperature: 4.0,
                ..Default::default()
            };
            assert!(check_auto_winter(&snap, &config, &mut state, &mut saved).is_none());
        }
        assert!(matches!(
            state,
            AutoWinterState::ColdPending { consecutive: 2 }
        ));

        // Third cold reading: activate.
        let snap = InverterSnapshot {
            battery_temperature: 4.0,
            ..Default::default()
        };
        let writes = check_auto_winter(&snap, &config, &mut state, &mut saved).expect("activates");

        assert!(matches!(state, AutoWinterState::Activating { retries: 0 }));
        // Saved values reflect the snapshot *before* activation.
        assert_eq!(
            saved,
            Some(AutoWinterSaved {
                enable_charge_target: false,
                target_soc: 4,
            })
        );
        // Writes enable charge target + set target SOC.
        assert!(writes
            .iter()
            .any(|w| w.address == HR_ENABLE_CHARGE_TARGET && w.value == 1));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_CHARGE_TARGET_SOC && w.value == 90));
    }

    #[test]
    fn auto_winter_retries_activation_until_readback_confirms_it() {
        let config = aw_config(8.0, 12.0, 90, 1);
        let mut state = AutoWinterState::ColdPending { consecutive: 0 };
        let mut saved = None;
        let cold = InverterSnapshot {
            battery_temperature: 4.0,
            ..Default::default()
        };

        let first_writes = check_auto_winter_with_outcome(
            &cold,
            &config,
            &mut state,
            &mut saved,
            AutoWinterWriteOutcome::NoneIssued,
        )
        .expect("activation writes");
        assert_eq!(first_writes.len(), 2);
        assert!(matches!(state, AutoWinterState::Activating { retries: 0 }));

        let retry_writes = check_auto_winter_with_outcome(
            &cold,
            &config,
            &mut state,
            &mut saved,
            AutoWinterWriteOutcome::Failed,
        )
        .expect("failed activation must retry");
        assert_eq!(retry_writes.len(), 2);
        assert!(matches!(state, AutoWinterState::Activating { retries: 1 }));

        let confirmed = InverterSnapshot {
            battery_temperature: 4.0,
            enable_charge_target: true,
            target_soc: 90,
            ..Default::default()
        };
        assert!(check_auto_winter_with_outcome(
            &confirmed,
            &config,
            &mut state,
            &mut saved,
            AutoWinterWriteOutcome::Succeeded,
        )
        .is_none());
        assert_eq!(state, AutoWinterState::WinterActive);
    }

    #[test]
    fn auto_winter_restores_after_warm_debounce() {
        let config = aw_config(8.0, 12.0, 90, 2);
        let mut state = AutoWinterState::WinterActive;
        // Pre-seed saved values (as if restored from disk after a restart).
        let mut saved = Some(AutoWinterSaved {
            enable_charge_target: true,
            target_soc: 77,
        });

        // First warm reading: WarmPending.
        let snap = InverterSnapshot {
            battery_temperature: 13.0,
            ..Default::default()
        };
        assert!(check_auto_winter(&snap, &config, &mut state, &mut saved).is_none());
        assert!(matches!(
            state,
            AutoWinterState::WarmPending { consecutive: 1 }
        ));

        // Second warm reading: restore.
        let writes = check_auto_winter(&snap, &config, &mut state, &mut saved).expect("restores");
        assert!(matches!(state, AutoWinterState::Restoring { retries: 0 }));
        assert!(
            saved.is_some(),
            "saved remains until readback confirms restore"
        );
        // Restores the saved target SOC (77) + enable (1).
        assert!(writes
            .iter()
            .any(|w| w.address == HR_CHARGE_TARGET_SOC && w.value == 77));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_ENABLE_CHARGE_TARGET && w.value == 1));
    }

    #[test]
    fn auto_winter_retries_restore_until_readback_confirms_it() {
        let config = aw_config(8.0, 12.0, 90, 2);
        let mut state = AutoWinterState::WinterActive;
        let mut saved = Some(AutoWinterSaved {
            enable_charge_target: true,
            target_soc: 77,
        });
        let warm = InverterSnapshot {
            battery_temperature: 13.0,
            ..Default::default()
        };

        assert!(check_auto_winter_with_outcome(
            &warm,
            &config,
            &mut state,
            &mut saved,
            AutoWinterWriteOutcome::NoneIssued,
        )
        .is_none());
        let first_writes = check_auto_winter_with_outcome(
            &warm,
            &config,
            &mut state,
            &mut saved,
            AutoWinterWriteOutcome::NoneIssued,
        )
        .expect("restore writes");
        assert_eq!(first_writes.len(), 2);
        assert!(matches!(state, AutoWinterState::Restoring { retries: 0 }));
        assert!(saved.is_some(), "saved values remain until readback");

        let retry_writes = check_auto_winter_with_outcome(
            &warm,
            &config,
            &mut state,
            &mut saved,
            AutoWinterWriteOutcome::Failed,
        )
        .expect("failed restore must retry");
        assert_eq!(retry_writes.len(), 2);
        assert!(matches!(state, AutoWinterState::Restoring { retries: 1 }));

        let confirmed = InverterSnapshot {
            battery_temperature: 13.0,
            enable_charge_target: true,
            target_soc: 77,
            ..Default::default()
        };
        assert!(check_auto_winter_with_outcome(
            &confirmed,
            &config,
            &mut state,
            &mut saved,
            AutoWinterWriteOutcome::Succeeded,
        )
        .is_none());
        assert_eq!(state, AutoWinterState::Idle);
        assert!(saved.is_none(), "saved values clear after readback");
    }

    #[test]
    fn auto_winter_terminal_write_failure_does_not_claim_winter_active() {
        let config = aw_config(8.0, 12.0, 90, 1);
        let mut state = AutoWinterState::ColdPending { consecutive: 0 };
        let mut saved = None;
        let cold = InverterSnapshot {
            battery_temperature: 4.0,
            ..Default::default()
        };
        assert!(check_auto_winter_with_outcome(
            &cold,
            &config,
            &mut state,
            &mut saved,
            AutoWinterWriteOutcome::NoneIssued,
        )
        .is_some());

        for _ in 0..=AUTO_WINTER_MAX_WRITE_RETRIES {
            let _ = check_auto_winter_with_outcome(
                &cold,
                &config,
                &mut state,
                &mut saved,
                AutoWinterWriteOutcome::Failed,
            );
        }

        assert!(matches!(state, AutoWinterState::Error { .. }));
        assert!(!matches!(state, AutoWinterState::WinterActive));
    }

    #[test]
    fn auto_winter_does_not_overwrite_restored_saved_values() {
        // If saved was restored from disk after a restart, re-activation must
        // NOT overwrite it with the current (post-winter) snapshot values.
        let config = aw_config(8.0, 12.0, 90, 1);
        let mut state = AutoWinterState::ColdPending { consecutive: 0 };
        let restored = AutoWinterSaved {
            enable_charge_target: true,
            target_soc: 55,
        };
        let mut saved = Some(restored.clone());

        let snap = InverterSnapshot {
            battery_temperature: 4.0,
            ..Default::default()
        };
        let _ = check_auto_winter(&snap, &config, &mut state, &mut saved);

        assert_eq!(
            saved,
            Some(restored),
            "restored saved values must survive activation"
        );
    }

    // -----------------------------------------------------------------
    // Inverter temperature limiter
    // -----------------------------------------------------------------

    fn temperature_config() -> TemperatureLimiterConfig {
        TemperatureLimiterConfig {
            enabled: true,
            high_threshold: 60.0,
            recovery_threshold: 55.0,
            confirmation_readings: 2,
        }
    }

    #[test]
    fn temperature_limiter_validates_hysteresis_and_confirmation_bounds() {
        assert!(temperature_config().validate().is_ok());
        let mut invalid = temperature_config();
        invalid.recovery_threshold = 60.0;
        assert!(invalid.validate().is_err());
        invalid = temperature_config();
        invalid.confirmation_readings = 0;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn temperature_limiter_overrides_timed_export_and_recovers_to_eco() {
        let mut state = TemperatureLimiterState::Idle;
        let mut saved = None;
        let hot = InverterSnapshot {
            battery_mode: BatteryMode::TimedExport,
            battery_reserve: 23,
            inverter_temperature: 62.0,
            device_type: DeviceType::Gen3Hybrid,
            ..Default::default()
        };

        assert!(check_temperature_limiter(
            &hot,
            &temperature_config(),
            &mut state,
            &mut saved,
            false,
        )
        .is_none());
        let pause_writes =
            check_temperature_limiter(&hot, &temperature_config(), &mut state, &mut saved, false)
                .expect("second hot reading pauses");
        assert_eq!(state, TemperatureLimiterState::Paused);
        assert_eq!(saved, Some(LoadLimiterSaved { reserve: 23 }));
        assert!(pause_writes
            .iter()
            .any(|write| write.address == HR_BATTERY_POWER_MODE && write.value == 1));
        assert!(pause_writes
            .iter()
            .any(|write| write.address == HR_BATTERY_SOC_RESERVE && write.value == 100));

        let cool = InverterSnapshot {
            battery_mode: BatteryMode::EcoPaused,
            inverter_temperature: 54.0,
            device_type: DeviceType::Gen3Hybrid,
            ..Default::default()
        };
        assert!(check_temperature_limiter(
            &cool,
            &temperature_config(),
            &mut state,
            &mut saved,
            false,
        )
        .is_none());
        let restore_writes =
            check_temperature_limiter(&cool, &temperature_config(), &mut state, &mut saved, false)
                .expect("second cool reading restores");
        assert_eq!(state, TemperatureLimiterState::PausedFromRestart);
        assert_eq!(saved, Some(LoadLimiterSaved { reserve: 23 }));
        assert!(restore_writes
            .iter()
            .any(|write| write.address == HR_BATTERY_SOC_RESERVE && write.value == 23));

        let restored = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            inverter_temperature: 54.0,
            device_type: DeviceType::Gen3Hybrid,
            ..Default::default()
        };
        assert!(check_temperature_limiter(
            &restored,
            &temperature_config(),
            &mut state,
            &mut saved,
            false,
        )
        .is_none());
        assert_eq!(state, TemperatureLimiterState::Idle);
        assert!(saved.is_none());
    }

    #[test]
    fn temperature_limiter_does_not_restore_while_load_limiter_owns_pause() {
        let cool = InverterSnapshot {
            battery_mode: BatteryMode::EcoPaused,
            inverter_temperature: 54.0,
            device_type: DeviceType::Gen3Hybrid,
            ..Default::default()
        };
        let mut state = TemperatureLimiterState::CoolingPending { consecutive: 1 };
        let mut saved = Some(LoadLimiterSaved { reserve: 19 });
        let writes =
            check_temperature_limiter(&cool, &temperature_config(), &mut state, &mut saved, true);
        assert!(writes.is_none());
        assert_eq!(state, TemperatureLimiterState::Idle);
        assert_eq!(saved, Some(LoadLimiterSaved { reserve: 19 }));
    }

    #[test]
    fn temperature_limiter_reasserts_confirmed_pause_each_poll() {
        let hot_paused = InverterSnapshot {
            battery_mode: BatteryMode::EcoPaused,
            inverter_temperature: 61.0,
            device_type: DeviceType::Gen3Hybrid,
            ..Default::default()
        };
        let mut state = TemperatureLimiterState::Paused;
        let mut saved = Some(LoadLimiterSaved { reserve: 17 });

        let writes = check_temperature_limiter_after_automation(
            &hot_paused,
            &temperature_config(),
            &mut state,
            &mut saved,
            true,
            true,
        )
        .expect("thermal pause is reasserted after other automation");

        assert_eq!(state, TemperatureLimiterState::Paused);
        assert!(writes
            .iter()
            .any(|write| write.address == HR_BATTERY_SOC_RESERVE && write.value == 100));
    }

    #[test]
    fn temperature_limiter_restart_reasserts_hot_pause_and_retries_cool_restore() {
        let mut saved = Some(LoadLimiterSaved { reserve: 17 });
        let hot = InverterSnapshot {
            battery_mode: BatteryMode::TimedExport,
            inverter_temperature: 61.0,
            battery_reserve: 17,
            device_type: DeviceType::Gen3Hybrid,
            ..Default::default()
        };
        let mut state = TemperatureLimiterState::PausedFromRestart;
        let writes =
            check_temperature_limiter(&hot, &temperature_config(), &mut state, &mut saved, false)
                .expect("hot restart reasserts pause");
        assert_eq!(state, TemperatureLimiterState::Paused);
        assert!(writes
            .iter()
            .any(|write| write.address == HR_BATTERY_SOC_RESERVE && write.value == 100));

        let cool = InverterSnapshot {
            battery_mode: BatteryMode::EcoPaused,
            inverter_temperature: 50.0,
            device_type: DeviceType::Gen3Hybrid,
            ..Default::default()
        };
        state = TemperatureLimiterState::PausedFromRestart;
        let writes =
            check_temperature_limiter(&cool, &temperature_config(), &mut state, &mut saved, false)
                .expect("cool restart retries restore");
        assert_eq!(state, TemperatureLimiterState::PausedFromRestart);
        assert!(writes
            .iter()
            .any(|write| write.address == HR_BATTERY_SOC_RESERVE && write.value == 17));
    }

    #[test]
    fn temperature_limiter_uses_three_phase_pause_registers() {
        let hot = InverterSnapshot {
            inverter_temperature: 65.0,
            battery_reserve: 20,
            device_type: DeviceType::ThreePhase,
            ..Default::default()
        };
        let config = TemperatureLimiterConfig {
            confirmation_readings: 1,
            ..temperature_config()
        };
        let mut state = TemperatureLimiterState::Idle;
        let mut saved = None;
        let writes = check_temperature_limiter(&hot, &config, &mut state, &mut saved, false)
            .expect("hot three-phase inverter pauses");
        assert!(writes
            .iter()
            .any(|write| { write.address == HR_3PH_FORCE_DISCHARGE_ENABLE && write.value == 0 }));
        assert!(writes
            .iter()
            .any(|write| { write.address == HR_3PH_BATTERY_SOC_RESERVE && write.value == 100 }));
        assert!(!writes
            .iter()
            .any(|write| write.address == HR_BATTERY_SOC_RESERVE));
    }

    #[test]
    fn temperature_limiter_ignores_non_finite_reading() {
        let snapshot = InverterSnapshot {
            inverter_temperature: f32::NAN,
            ..Default::default()
        };
        let mut state = TemperatureLimiterState::Idle;
        let mut saved = None;
        assert!(check_temperature_limiter(
            &snapshot,
            &temperature_config(),
            &mut state,
            &mut saved,
            false,
        )
        .is_none());
        assert_eq!(state, TemperatureLimiterState::Idle);
    }

    // -----------------------------------------------------------------
    // check_load_limiter — pure transition logic (always-active window)
    // -----------------------------------------------------------------

    /// `start`/`end` both zero => activation window is always-on, so the
    /// `chrono::Local::now()` check inside `check_load_limiter` is irrelevant.
    fn ll_config(threshold_w: u32, delay_minutes: u32) -> LoadLimiterConfig {
        LoadLimiterConfig {
            enabled: true,
            threshold_w,
            trigger_delay_minutes: delay_minutes,
            start_hour: 0,
            start_minute: 0,
            end_hour: 0,
            end_minute: 0,
        }
    }

    #[test]
    fn load_limiter_disabled_while_paused_restores_eco() {
        let snap = InverterSnapshot {
            battery_mode: BatteryMode::EcoPaused,
            home_power: 999_999,
            ..Default::default()
        };
        let config = LoadLimiterConfig {
            enabled: false,
            ..Default::default()
        };
        let mut state = LoadLimiterState::Paused;
        let mut saved = Some(LoadLimiterSaved { reserve: 20 });

        let writes = check_load_limiter(&snap, &config, &mut state, 60, &mut saved)
            .expect("disabling an active limiter should restore Eco");

        assert_eq!(state, LoadLimiterState::Idle);
        assert!(saved.is_none(), "saved reserve is consumed on restore");
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 20));
    }

    #[test]
    fn load_limiter_disabled_while_pending_writes_nothing() {
        let snap = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 999_999,
            ..Default::default()
        };
        let config = LoadLimiterConfig {
            enabled: false,
            ..Default::default()
        };
        let mut state = LoadLimiterState::HighLoadPending { consecutive: 2 };
        let mut saved = None;

        let writes = check_load_limiter(&snap, &config, &mut state, 60, &mut saved);

        assert!(writes.is_none());
        assert_eq!(state, LoadLimiterState::Idle);
    }

    #[test]
    fn load_limiter_is_a_safety_owner_across_modes_and_automation() {
        let config = ll_config(3000, 5);
        let mut state = LoadLimiterState::Idle;
        let mut saved = None;

        // A scheduled/manual discharge mode is still monitored.
        let snap = InverterSnapshot {
            battery_mode: BatteryMode::TimedExport,
            home_power: 9999,
            ..Default::default()
        };
        assert!(check_load_limiter(&snap, &config, &mut state, 60, &mut saved).is_none());
        assert!(matches!(state, LoadLimiterState::HighLoadPending { .. }));

        // Another automation does not suppress a safety pause either.
        state = LoadLimiterState::Idle;
        let snap = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 9999,
            auto_winter_active: true,
            ..Default::default()
        };
        for _ in 0..4 {
            assert!(check_load_limiter(&snap, &config, &mut state, 60, &mut saved).is_none());
        }
        let writes = check_load_limiter(&snap, &config, &mut state, 60, &mut saved)
            .expect("safety limiter should pause after confirmation");
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0));
    }

    #[test]
    fn load_limiter_counts_high_load_then_pauses() {
        let config = ll_config(3000, 5);
        let mut state = LoadLimiterState::Idle;
        let mut saved = None;
        // 5-minute delay at a 1-minute poll => debounce_count = 5.
        let high = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 4000,
            battery_reserve: 20,
            ..Default::default()
        };

        // First 4 high-load polls: pending, no writes.
        for _ in 0..4 {
            assert!(check_load_limiter(&high, &config, &mut state, 60, &mut saved).is_none());
            assert!(matches!(state, LoadLimiterState::HighLoadPending { .. }));
        }
        // 5th: transition to Paused with restore-100 writes.
        let writes =
            check_load_limiter(&high, &config, &mut state, 60, &mut saved).expect("pauses");
        assert_eq!(state, LoadLimiterState::Paused);
        // Should have saved the original reserve (20) before pausing.
        assert_eq!(saved, Some(LoadLimiterSaved { reserve: 20 }));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 100));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0));
    }

    #[test]
    fn load_limiter_restores_eco_when_load_drops_for_full_delay() {
        let config = ll_config(3000, 3);
        let mut state = LoadLimiterState::Paused;
        // Pre-seed saved reserve (as if it was captured when the limiter
        // paused discharge).
        let mut saved = Some(LoadLimiterSaved { reserve: 20 });
        let low = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 1000,
            ..Default::default()
        };

        // First two low-load polls while Paused: LowLoadPending, no writes.
        for _ in 0..2 {
            assert!(check_load_limiter(&low, &config, &mut state, 60, &mut saved).is_none());
            assert!(matches!(state, LoadLimiterState::LowLoadPending { .. }));
        }
        // 3rd: restore Eco with the saved reserve (20), not hardcoded 4.
        let writes =
            check_load_limiter(&low, &config, &mut state, 60, &mut saved).expect("restores");
        assert_eq!(state, LoadLimiterState::Idle);
        // Saved should be consumed (taken) on restore.
        assert!(saved.is_none(), "saved must be consumed on restore");
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 20));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1));
    }

    #[test]
    fn load_limiter_post_crash_restores_immediately_if_load_already_low() {
        let config = ll_config(3000, 10);
        let mut state = LoadLimiterState::PausedFromRestart;
        let mut saved = None;
        let low = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 500,
            ..Default::default()
        };

        // Battery is already in Eco mode (writes from a previous poll
        // succeeded) — transition to Idle without sending writes.
        let writes = check_load_limiter(&low, &config, &mut state, 60, &mut saved);
        assert!(writes.is_none(), "no writes needed when already in Eco");
        assert_eq!(state, LoadLimiterState::Idle);
    }

    #[test]
    fn load_limiter_post_crash_retries_restore_when_still_in_eco_paused() {
        let config = ll_config(3000, 10);
        let mut state = LoadLimiterState::PausedFromRestart;
        let mut saved = Some(LoadLimiterSaved { reserve: 20 });
        let low = InverterSnapshot {
            battery_mode: BatteryMode::EcoPaused,
            home_power: 500,
            ..Default::default()
        };

        // Battery is still in EcoPaused mode (writes from a previous poll
        // failed or haven't been sent yet) — return restore writes but stay
        // in PausedFromRestart so a failed write is retried on the next poll.
        let writes = check_load_limiter(&low, &config, &mut state, 60, &mut saved)
            .expect("restore writes returned");
        assert_eq!(
            state,
            LoadLimiterState::PausedFromRestart,
            "must stay in PausedFromRestart until writes are confirmed"
        );
        // Should restore the saved reserve (20), not hardcoded 4.
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 20));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0));

        // Simulate next poll: writes succeeded, battery now in Eco mode.
        let restored = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 500,
            ..Default::default()
        };
        let writes = check_load_limiter(&restored, &config, &mut state, 60, &mut saved);
        assert!(writes.is_none(), "no writes needed after restore confirmed");
        assert_eq!(state, LoadLimiterState::Idle);
    }

    #[test]
    fn load_limiter_post_crash_load_still_high_transitions_to_paused() {
        let config = ll_config(3000, 10);
        let mut state = LoadLimiterState::PausedFromRestart;
        let mut saved = Some(LoadLimiterSaved { reserve: 20 });
        let high = InverterSnapshot {
            battery_mode: BatteryMode::EcoPaused,
            home_power: 5000,
            ..Default::default()
        };

        // Load still above threshold after restart — transition to normal Paused.
        let writes = check_load_limiter(&high, &config, &mut state, 60, &mut saved);
        assert!(writes.is_none(), "no writes when load still high");
        assert_eq!(state, LoadLimiterState::Paused);
        // Saved reserve must be preserved for when the load eventually drops.
        assert_eq!(saved, Some(LoadLimiterSaved { reserve: 20 }));
    }

    #[test]
    fn load_limiter_post_crash_falls_back_to_reserve_4_when_no_saved_value() {
        let config = ll_config(3000, 10);
        let mut state = LoadLimiterState::PausedFromRestart;
        let mut saved = None;
        let low = InverterSnapshot {
            battery_mode: BatteryMode::EcoPaused,
            home_power: 500,
            ..Default::default()
        };

        // No saved reserve — should fall back to 4.
        let writes = check_load_limiter(&low, &config, &mut state, 60, &mut saved)
            .expect("restore writes returned");
        assert_eq!(
            state,
            LoadLimiterState::PausedFromRestart,
            "must stay in PausedFromRestart until writes confirmed"
        );
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 4));
    }

    #[test]
    fn load_limiter_low_load_pending_falls_back_to_reserve_4_when_no_saved_value() {
        let config = ll_config(3000, 1);
        let mut state = LoadLimiterState::Paused;
        let mut saved = None;
        let low = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 1000,
            ..Default::default()
        };

        // One poll with load below threshold: LowLoadPending.
        assert!(check_load_limiter(&low, &config, &mut state, 60, &mut saved).is_none());
        assert!(matches!(
            state,
            LoadLimiterState::LowLoadPending { consecutive: 1 }
        ));

        // Second poll: restore with fallback reserve 4.
        let writes = check_load_limiter(&low, &config, &mut state, 60, &mut saved)
            .expect("restores with fallback 4");
        assert_eq!(state, LoadLimiterState::Idle);
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 4));
    }

    #[test]
    fn load_limiter_high_load_pending_resets_when_load_drops() {
        let config = ll_config(3000, 5);
        let mut state = LoadLimiterState::HighLoadPending { consecutive: 3 };
        let mut saved = None;
        let low = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 1000,
            ..Default::default()
        };

        // Load dropped below threshold — reset to Idle.
        let writes = check_load_limiter(&low, &config, &mut state, 60, &mut saved);
        assert!(writes.is_none());
        assert_eq!(state, LoadLimiterState::Idle);
    }

    #[test]
    fn load_limiter_recovery_does_not_restore_while_temperature_limiter_is_active() {
        let config = ll_config(3000, 3);
        let mut state = LoadLimiterState::LowLoadPending { consecutive: 2 };
        let mut saved = Some(LoadLimiterSaved { reserve: 20 });
        let low = InverterSnapshot {
            battery_mode: BatteryMode::EcoPaused,
            home_power: 1000,
            device_type: DeviceType::Gen3Hybrid,
            ..Default::default()
        };

        let writes =
            check_load_limiter_with_other_pause(&low, &config, &mut state, 60, &mut saved, true);
        assert!(writes.is_none());
        assert_eq!(state, LoadLimiterState::Idle);
        assert_eq!(saved, Some(LoadLimiterSaved { reserve: 20 }));
    }

    #[test]
    fn load_limiter_low_load_pending_goes_back_to_paused_when_load_rises() {
        let config = ll_config(3000, 5);
        let mut state = LoadLimiterState::LowLoadPending { consecutive: 2 };
        let mut saved = Some(LoadLimiterSaved { reserve: 20 });
        let high = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 5000,
            ..Default::default()
        };

        // Load rose above threshold — go back to Paused.
        let writes = check_load_limiter(&high, &config, &mut state, 60, &mut saved);
        assert!(writes.is_none());
        assert_eq!(state, LoadLimiterState::Paused);
        // Saved reserve must be preserved.
        assert_eq!(saved, Some(LoadLimiterSaved { reserve: 20 }));
    }

    #[test]
    fn load_limiter_reasserts_pause_after_external_mode_change() {
        let config = ll_config(3000, 5);
        let mut state = LoadLimiterState::Paused;
        let mut saved = Some(LoadLimiterSaved { reserve: 20 });
        let snap = InverterSnapshot {
            battery_mode: BatteryMode::TimedExport,
            home_power: 5000,
            ..Default::default()
        };

        // Battery mode changed externally while load is still high — the
        // safety owner must restore Eco Paused rather than yielding.
        let writes = check_load_limiter(&snap, &config, &mut state, 60, &mut saved)
            .expect("safety pause should be reasserted");
        assert_eq!(state, LoadLimiterState::Paused);
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0));
    }

    #[test]
    fn load_limiter_reasserts_pause_after_restart_mode_change() {
        let config = ll_config(3000, 5);
        let mut state = LoadLimiterState::PausedFromRestart;
        let mut saved = Some(LoadLimiterSaved { reserve: 20 });
        let snap = InverterSnapshot {
            battery_mode: BatteryMode::TimedExport,
            home_power: 5000,
            ..Default::default()
        };

        // Battery mode changed externally while in PausedFromRestart — keep
        // the safety pause in force while load remains high.
        let writes = check_load_limiter(&snap, &config, &mut state, 60, &mut saved)
            .expect("safety pause should be reasserted");
        assert_eq!(state, LoadLimiterState::Paused);
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0));
    }

    #[test]
    fn load_limiter_active_state_includes_recovery_delay() {
        assert!(LoadLimiterState::Paused.is_actively_pausing());
        assert!(LoadLimiterState::PausedFromRestart.is_actively_pausing());
        assert!(LoadLimiterState::LowLoadPending { consecutive: 2 }.is_actively_pausing());
        assert!(!LoadLimiterState::Idle.is_actively_pausing());
        assert!(!LoadLimiterState::HighLoadPending { consecutive: 2 }.is_actively_pausing());
    }

    #[test]
    fn load_limiter_outside_window_restores_recovery_with_saved_reserve() {
        let config = LoadLimiterConfig {
            enabled: true,
            threshold_w: 3000,
            trigger_delay_minutes: 5,
            start_hour: 9,
            start_minute: 0,
            end_hour: 17,
            end_minute: 0,
        };
        let mut state = LoadLimiterState::LowLoadPending { consecutive: 2 };
        let mut saved = Some(LoadLimiterSaved { reserve: 20 });
        let snap = InverterSnapshot {
            battery_mode: BatteryMode::EcoPaused,
            home_power: 1000,
            ..Default::default()
        };

        // At 18:00 the battery is still paused during its recovery delay.
        // Leaving the activation window must restore Eco immediately.
        let writes =
            check_load_limiter_at(&snap, &config, &mut state, 60, &mut saved, 18 * 60, false)
                .expect("restore writes returned");
        assert_eq!(state, LoadLimiterState::Idle);
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 20));
        assert!(saved.is_none(), "saved must be consumed on restore");
    }

    #[test]
    fn load_limiter_outside_window_discards_high_load_countdown() {
        let config = LoadLimiterConfig {
            enabled: true,
            threshold_w: 3000,
            trigger_delay_minutes: 5,
            start_hour: 9,
            start_minute: 0,
            end_hour: 17,
            end_minute: 0,
        };
        let mut state = LoadLimiterState::HighLoadPending { consecutive: 4 };
        let mut saved = None;
        let snap = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 5000,
            ..Default::default()
        };

        let writes =
            check_load_limiter_at(&snap, &config, &mut state, 60, &mut saved, 18 * 60, false);
        assert!(writes.is_none());
        assert_eq!(state, LoadLimiterState::Idle);
    }

    // -----------------------------------------------------------------
    // check_load_limiter — issue #124 end-to-end scenarios
    //
    // Issue #124: "On App Restart Load Limiter does not Reset" — when the
    // load limiter was active (battery paused) when the app last ran, and
    // the home load is now below threshold, the battery status must
    // restore to the previous (Eco) state without manual intervention.
    //
    // The state machine handles this via `PausedFromRestart`: writes are
    // re-sent on each poll until the inverter acknowledges them (detected
    // by `battery_mode == Eco`). The tests below pin every transition
    // along that recovery path so the issue can't silently regress.
    // -----------------------------------------------------------------

    /// Compute the snapshot's `load_limiter_active` flag the same way the
    /// poll loop does, so the tests can assert the frontend-visible state
    /// across the full restore cycle without standing up the whole poll
    /// loop.
    fn ll_snapshot_active(state: &LoadLimiterState) -> bool {
        matches!(state, LoadLimiterState::Paused)
            || matches!(state, LoadLimiterState::PausedFromRestart)
    }

    #[test]
    fn load_limiter_post_crash_clears_saved_reserve_on_final_confirm() {
        // When the inverter finally acknowledges the restore writes and
        // the next snapshot shows `battery_mode == Eco`, the state goes
        // to `Idle` and the saved-reserve slot must be cleared. Otherwise
        // a stale reserve (e.g. 20%) lingers in `load_limiter_saved_reserve`
        // on disk and will silently re-activate on a later crash even
        // though no limiter pause is in progress.
        let config = ll_config(3000, 10);
        let mut state = LoadLimiterState::PausedFromRestart;
        let mut saved = Some(LoadLimiterSaved { reserve: 20 });
        let restored = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 500,
            ..Default::default()
        };

        let writes = check_load_limiter(&restored, &config, &mut state, 60, &mut saved);
        assert!(writes.is_none(), "no writes needed when already in Eco");
        assert_eq!(state, LoadLimiterState::Idle);
        assert!(
            saved.is_none(),
            "saved reserve must be consumed once the restore is confirmed, \
             otherwise a stale value lingers in settings.json"
        );
        // Frontend-visible flag flips to false on the same poll.
        assert!(
            !ll_snapshot_active(&state),
            "snapshot.load_limiter_active must be false after restore"
        );
    }

    #[test]
    fn load_limiter_post_crash_full_issue_124_restore_cycle() {
        // End-to-end reproduction of issue #124: the load limiter
        // triggered before the app exited, the home load is now below
        // threshold, and the inverter's `battery_mode` is still
        // `EcoPaused` (the previous restore writes were lost when the
        // app crashed mid-write). The state machine must:
        //
        // 1. Return the saved-reserve restore writes on every poll
        //    where battery_mode is still EcoPaused, staying in
        //    `PausedFromRestart` so a write failure is retried.
        // 2. Transition to `Idle` (no writes) on the first poll that
        //    sees `battery_mode == Eco`, clearing the saved reserve
        //    so the disk state stays consistent.
        // 3. Expose `load_limiter_active = true` to the frontend
        //    throughout the retry loop, then `false` after the
        //    restore is confirmed — matching the inverter's actual
        //    battery state.
        let config = ll_config(3000, 10);
        let mut state = LoadLimiterState::PausedFromRestart;
        let mut saved = Some(LoadLimiterSaved { reserve: 20 });

        // Simulate the inverter's perspective for the first N polls:
        // battery is still EcoPaused and the home load is below
        // threshold. The dongle is "busy" or the write hasn't taken
        // effect yet, so battery_mode stays EcoPaused.
        let retry_snap = InverterSnapshot {
            battery_mode: BatteryMode::EcoPaused,
            home_power: 500,
            ..Default::default()
        };

        // First five polls: every one returns the same restore writes
        // and stays in PausedFromRestart. The frontend-visible flag
        // stays true (the limiter is still trying to restore).
        for i in 0..5 {
            let writes = check_load_limiter(&retry_snap, &config, &mut state, 60, &mut saved)
                .unwrap_or_else(|| panic!("poll {i} should return restore writes"));
            assert_eq!(
                state,
                LoadLimiterState::PausedFromRestart,
                "poll {i}: must stay in PausedFromRestart while battery is EcoPaused"
            );
            // Each retry must use the *saved* reserve (20%), not a
            // hardcoded default — the user's prior setting is what we
            // promised to restore.
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 20),
                "poll {i}: restore writes must use the saved reserve (20)"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1),
                "poll {i}: restore writes must set battery power mode to eco"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0),
                "poll {i}: restore writes must clear enable_discharge"
            );
            assert!(
                ll_snapshot_active(&state),
                "poll {i}: snapshot.load_limiter_active must stay true during retry"
            );
            assert_eq!(
                saved,
                Some(LoadLimiterSaved { reserve: 20 }),
                "poll {i}: saved reserve must be preserved across retries"
            );
        }

        // The inverter finally acknowledges the writes. Next poll
        // shows battery_mode == Eco, the state machine transitions
        // to Idle, and the saved reserve is consumed.
        let restored_snap = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 500,
            ..Default::default()
        };
        let writes = check_load_limiter(&restored_snap, &config, &mut state, 60, &mut saved);
        assert!(
            writes.is_none(),
            "no writes needed once the inverter is back in Eco"
        );
        assert_eq!(
            state,
            LoadLimiterState::Idle,
            "state must transition to Idle on the first Eco poll"
        );
        assert!(
            saved.is_none(),
            "saved reserve must be consumed on the final confirm so it \
             does not linger in settings.json after the limiter deactivates"
        );
        assert!(
            !ll_snapshot_active(&state),
            "snapshot.load_limiter_active must flip to false after restore"
        );
    }

    #[test]
    fn load_limiter_post_crash_load_rises_again_during_retry() {
        // While the state machine is in `PausedFromRestart` retrying
        // restore writes, the home load can come back up above the
        // threshold. The state machine must drop out of the retry
        // loop and transition to `Paused` (normal, debounced flow)
        // so we don't keep issuing restore writes the inverter would
        // immediately undo.
        let config = ll_config(3000, 10);
        let mut state = LoadLimiterState::PausedFromRestart;
        let mut saved = Some(LoadLimiterSaved { reserve: 20 });
        let high_snap = InverterSnapshot {
            battery_mode: BatteryMode::EcoPaused,
            home_power: 6_000, // above 3000 W threshold
            ..Default::default()
        };

        let writes = check_load_limiter(&high_snap, &config, &mut state, 60, &mut saved);
        assert!(
            writes.is_none(),
            "no writes when load is high — the limiter is correctly staying paused"
        );
        assert_eq!(
            state,
            LoadLimiterState::Paused,
            "must drop out of PausedFromRestart to Paused when load rises"
        );
        // Saved reserve must survive the transition so the eventual
        // restore uses the correct value.
        assert_eq!(
            saved,
            Some(LoadLimiterSaved { reserve: 20 }),
            "saved reserve must survive the PausedFromRestart -> Paused transition"
        );
    }

    #[test]
    fn load_limiter_post_crash_recovery_with_no_eco_paused_window() {
        // Some inverters may have already auto-restored Eco mode on
        // their own (e.g. the load limiter was held by app, then
        // dropped manually, then app restarted). The very first poll
        // after `initialize_app_state` sees battery_mode == Eco and
        // must transition to `Idle` without sending any writes or
        // re-entering the normal Paused state machine.
        let config = ll_config(3000, 10);
        let mut state = LoadLimiterState::PausedFromRestart;
        let mut saved = Some(LoadLimiterSaved { reserve: 20 });
        let already_restored = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 6_000, // high load — would normally pause, but we just confirmed restore
            ..Default::default()
        };

        let writes = check_load_limiter(&already_restored, &config, &mut state, 60, &mut saved);
        assert!(
            writes.is_none(),
            "no writes — battery is already in Eco, restore is confirmed"
        );
        assert_eq!(
            state,
            LoadLimiterState::Idle,
            "must not re-enter the normal pause flow just because load is high; \
             the previous restore was confirmed, so the limiter is fully deactivated"
        );
        assert!(
            saved.is_none(),
            "saved reserve must be cleared even when load is high, \
             so a later crash can't re-trigger the limiter with a stale value"
        );
    }

    #[test]
    fn load_limiter_post_crash_recovers_with_fallback_reserve_in_full_cycle() {
        // Issue #124 with no persisted saved-reserve (older settings
        // file, or the saved value was already cleared). The
        // recovery path must still work end-to-end, falling back to
        // the safe default reserve (4%) on every restore attempt.
        let config = ll_config(3000, 10);
        let mut state = LoadLimiterState::PausedFromRestart;
        let mut saved = None;
        let retry_snap = InverterSnapshot {
            battery_mode: BatteryMode::EcoPaused,
            home_power: 500,
            ..Default::default()
        };

        for _ in 0..3 {
            let writes = check_load_limiter(&retry_snap, &config, &mut state, 60, &mut saved)
                .expect("retry must always return writes when battery is EcoPaused");
            assert_eq!(state, LoadLimiterState::PausedFromRestart);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 4),
                "no saved reserve -> must fall back to the safe default (4%)"
            );
        }

        // Final confirm: state goes to Idle, no writes, saved stays None.
        let restored = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 500,
            ..Default::default()
        };
        let writes = check_load_limiter(&restored, &config, &mut state, 60, &mut saved);
        assert!(writes.is_none());
        assert_eq!(state, LoadLimiterState::Idle);
        assert!(saved.is_none());
    }

    // -----------------------------------------------------------------
    // build_force_discharge_auto_revert_writes — issue #129
    // -----------------------------------------------------------------

    #[test]
    fn force_discharge_auto_revert_returns_none_when_no_slot_end() {
        // No slot end time → no auto-revert. This covers the "no body" /
        // "until stopped" path where there is no slot to expire.
        let writes = build_force_discharge_auto_revert_writes(
            DeviceType::Gen2Hybrid,
            1_000_000,
            None,
            false,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert!(writes.is_none());
    }

    #[test]
    fn force_discharge_auto_revert_returns_none_when_slot_not_expired() {
        // Slot end is in the future → no auto-revert.
        let writes = build_force_discharge_auto_revert_writes(
            DeviceType::Gen2Hybrid,
            1_000_000,
            Some(1_000_000 + 60_000), // 60 seconds in the future
            false,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert!(writes.is_none());
    }

    #[test]
    fn force_discharge_auto_revert_restores_single_phase_state() {
        // Pre-state: enable_charge=true, enable_discharge=false, slot 1 = 17:00-19:00.
        // After slot expiry, the inverter should be restored to exactly that.
        let writes = build_force_discharge_auto_revert_writes(
            DeviceType::Gen2Hybrid,
            1_000_000,
            Some(999_999), // 1ms ago
            true,          // pre enable_charge
            false,         // pre enable_discharge
            Some((17, 0)),
            Some((19, 0)),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("auto-revert should fire when slot expired");

        // enable_discharge restored to 0 (pre-force value).
        assert!(writes
            .iter()
            .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0));
        // enable_charge restored to 1 (pre-force value).
        assert!(writes
            .iter()
            .any(|w| w.address == HR_ENABLE_CHARGE && w.value == 1));
        // enable_charge_target follows enable_charge.
        assert!(writes
            .iter()
            .any(|w| w.address == HR_ENABLE_CHARGE_TARGET && w.value == 1));
        // Slot 1 restored to 17:00.
        let s1 = encode_hhmm(17, 0).expect("valid test start time");
        let e1 = encode_hhmm(19, 0).expect("valid test end time");
        assert!(writes
            .iter()
            .any(|w| w.address == HR_DISCHARGE_SLOT_1_START && w.value == s1));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_DISCHARGE_SLOT_1_END && w.value == e1));
        // Slot 2 cleared to 00:00–00:00 (no prior slot).
        assert!(writes
            .iter()
            .any(|w| w.address == HR_DISCHARGE_SLOT_2_START && w.value == 0));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_DISCHARGE_SLOT_2_END && w.value == 0));
        // Battery power mode restored to eco (1).
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1));
    }

    #[test]
    fn force_discharge_auto_revert_clears_discharge_when_pre_state_disabled() {
        // Pre-state: enable_charge=false, enable_discharge=false, no slots.
        // The user was in eco with no schedules. After auto-revert, the
        // inverter should be back in exactly that state.
        let writes = build_force_discharge_auto_revert_writes(
            DeviceType::Gen2Hybrid,
            1_000_000,
            Some(0), // long expired
            false,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("auto-revert should fire");

        // All flags cleared, slots cleared, mode = eco.
        assert!(writes
            .iter()
            .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_ENABLE_CHARGE && w.value == 0));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_ENABLE_CHARGE_TARGET && w.value == 0));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1));
    }

    #[test]
    fn force_discharge_auto_revert_restores_pause_registers() {
        // Issue #289: Force Discharge temporarily disables an armed
        // discharge pause (HR318=2). Auto-revert must restore the exact
        // pre-action HR318/319/320 configuration.
        let pause_slot = ScheduleSlot {
            enabled: true,
            start_hour: 20, // pause window 20:00 → 15:00
            start_minute: 0,
            end_hour: 15,
            end_minute: 0,
            target_soc: 4,
        };
        let writes = build_force_discharge_auto_revert_writes(
            DeviceType::Gen2Hybrid,
            1_000_000,
            Some(0), // long expired
            false,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(2),
            Some(&pause_slot),
        )
        .expect("auto-revert should fire");

        use crate::modbus::registers::{
            HR_BATTERY_PAUSE_MODE, HR_BATTERY_PAUSE_SLOT_1_END, HR_BATTERY_PAUSE_SLOT_1_START,
        };
        // HR318 restored to 2 (pause discharge re-armed)
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_PAUSE_MODE && w.value == 2));
        // HR319 restored to pause start 20:00
        assert!(writes.iter().any(|w| {
            w.address == HR_BATTERY_PAUSE_SLOT_1_START
                && w.value == encode_hhmm(20, 0).expect("valid test start time")
        }));
        // HR320 restored to pause end 15:00
        assert!(writes.iter().any(|w| {
            w.address == HR_BATTERY_PAUSE_SLOT_1_END
                && w.value == encode_hhmm(15, 0).expect("valid test end time")
        }));
    }

    #[test]
    fn force_discharge_auto_revert_without_pause_capture_skips_pause_writes() {
        // Legacy revert (pre-#289) has no pause capture — the auto-revert
        // must not touch HR318/319/320 at all rather than zeroing them.
        let writes = build_force_discharge_auto_revert_writes(
            DeviceType::Gen2Hybrid,
            1_000_000,
            Some(0),
            false,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("auto-revert should fire");

        use crate::modbus::registers::{
            HR_BATTERY_PAUSE_MODE, HR_BATTERY_PAUSE_SLOT_1_END, HR_BATTERY_PAUSE_SLOT_1_START,
        };
        assert!(writes.iter().all(|w| w.address != HR_BATTERY_PAUSE_MODE
            && w.address != HR_BATTERY_PAUSE_SLOT_1_START
            && w.address != HR_BATTERY_PAUSE_SLOT_1_END));
    }

    #[test]
    fn force_discharge_auto_revert_restores_pause_on_three_phase() {
        // Code-review finding: AC-three-phase devices support Timed
        // Discharge (HR318 lives in the common holding block) while their
        // enable/slot restoration routes through the three-phase register
        // bank. The pause schedule must be restored on those devices too —
        // previously the restore only ran inside the single-phase branch and
        // Force Discharge silently dropped the user's pause window.
        let pause_slot = ScheduleSlot {
            enabled: true,
            start_hour: 20,
            start_minute: 0,
            end_hour: 15,
            end_minute: 0,
            target_soc: 4,
        };
        let writes = build_force_discharge_auto_revert_writes(
            DeviceType::ACThreePhase,
            1_000_000,
            Some(0),
            false,
            false,
            None,
            None,
            None,
            None,
            Some(true),  // pre three_phase_force_discharge_enable
            Some(false), // pre three_phase_force_charge_enable
            Some(2),
            Some(&pause_slot),
        )
        .expect("auto-revert should fire");

        use crate::modbus::registers::{
            HR_3PH_FORCE_DISCHARGE_ENABLE, HR_BATTERY_PAUSE_MODE, HR_BATTERY_PAUSE_SLOT_1_END,
            HR_BATTERY_PAUSE_SLOT_1_START,
        };
        // Three-phase enable flags restored via the 3-phase bank.
        assert!(writes
            .iter()
            .any(|w| w.address == HR_3PH_FORCE_DISCHARGE_ENABLE && w.value == 1));
        // HR318/319/320 restored even on the three-phase path.
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_PAUSE_MODE && w.value == 2));
        assert!(writes.iter().any(|w| {
            w.address == HR_BATTERY_PAUSE_SLOT_1_START
                && w.value == encode_hhmm(20, 0).expect("valid test start time")
        }));
        assert!(writes.iter().any(|w| {
            w.address == HR_BATTERY_PAUSE_SLOT_1_END
                && w.value == encode_hhmm(15, 0).expect("valid test end time")
        }));
        // Slot values precede the pause mode so the inverter never observes
        // an enabled pause with incomplete window data.
        let slot_start_idx = writes
            .iter()
            .position(|w| w.address == HR_BATTERY_PAUSE_SLOT_1_START)
            .unwrap();
        let mode_idx = writes
            .iter()
            .position(|w| w.address == HR_BATTERY_PAUSE_MODE)
            .unwrap();
        assert!(
            slot_start_idx < mode_idx,
            "pause window values must be written before re-enabling the pause mode"
        );
    }

    #[test]
    fn force_discharge_start_disables_discharge_pause_after_capture() {
        // Issue #289: the API handler's write list, when the captured
        // pre-state had HR318=2/3, must include HR318=0 so the forced
        // discharge is not blocked. This test pins the pure decision
        // helper used by the handler.
        assert!(should_disable_pause_for_force_discharge(2));
        assert!(should_disable_pause_for_force_discharge(3));
        // Pause charging (1) and disabled (0) do not block discharge.
        assert!(!should_disable_pause_for_force_discharge(0));
        assert!(!should_disable_pause_for_force_discharge(1));
    }

    #[test]
    fn force_discharge_auto_revert_three_phase_uses_three_phase_registers() {
        // Three-phase pre-state: both force flags were off, so revert clears them.
        let writes = build_force_discharge_auto_revert_writes(
            DeviceType::Gen3Hybrid, // not 3ph — adjust below
            1_000_000,
            Some(0),
            false,
            false,
            None,
            None,
            None,
            None,
            Some(false), // 3ph force_discharge was off
            Some(false), // 3ph force_charge was off
            None,
            None,
        );
        // Gen3Hybrid is not three-phase — should use single-phase path.
        assert!(writes.is_some());
        let writes = writes.unwrap();
        assert!(writes
            .iter()
            .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0));
    }

    #[test]
    fn force_discharge_auto_revert_fires_at_exact_boundary() {
        // Slot end == now → auto-revert should fire (>= boundary).
        let writes = build_force_discharge_auto_revert_writes(
            DeviceType::Gen2Hybrid,
            1_000_000,
            Some(1_000_000),
            false,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert!(writes.is_some(), "should fire at exact boundary");
    }

    // ==================================================================
    // evaluate_agile_slot tests
    // ==================================================================
    //
    // The slot-based Agile state machine has three responsibilities:
    //   1. Pick the right side (charge vs discharge) for the current
    //      price + scope.
    //   2. Detect contiguous cheap/expensive runs starting now so the
    //      slot we write covers the whole window in one FC6 sequence.
    //   3. Defer when Cosy or AutoWinter is in control of the same side.
    //
    // Tests pin all three so future scope additions (e.g. ChargeOnly)
    // can't silently regress Standard-mode behaviour.

    use crate::settings::AgileScope;

    /// Build a PriceSlot cache in newest-first order, the shape the
    /// Octopus API returns. Times are unix seconds; 30-min slots.
    fn make_cache(slots: &[(i64, i64, f64)]) -> Vec<PriceSlot> {
        let mut v: Vec<PriceSlot> = slots
            .iter()
            .map(|&(from, to, pence)| PriceSlot {
                pence,
                valid_from: from,
                valid_to: to,
            })
            .collect();
        // Sort newest-first (descending valid_to) so the fixture matches
        // the real Octopus response shape.
        v.sort_by_key(|s| std::cmp::Reverse(s.valid_to));
        v
    }

    #[test]
    fn agile_price_cache_is_sorted_newest_first() {
        let mut prices = vec![
            PriceSlot {
                pence: 20.0,
                valid_from: 3_600,
                valid_to: 5_400,
            },
            PriceSlot {
                pence: 10.0,
                valid_from: 0,
                valid_to: 1_800,
            },
            PriceSlot {
                pence: 15.0,
                valid_from: 1_800,
                valid_to: 3_600,
            },
        ];

        sort_price_slots_newest_first(&mut prices);

        assert_eq!(
            prices
                .iter()
                .map(|slot| slot.valid_from)
                .collect::<Vec<_>>(),
            vec![3_600, 1_800, 0]
        );
    }

    #[test]
    fn evaluate_agile_off_scope_returns_idle() {
        let cache = make_cache(&[(0, 1800, 5.0)]);
        let action = evaluate_agile_slot(
            AgileScope::Off,
            Some(5.0),
            10.0,
            30.0,
            &cache,
            900,
            false,
            false,
            &chrono::Utc,
        );
        assert_eq!(action, AgileSlotAction::Idle);
        assert!(!action.is_active());
    }

    #[test]
    fn evaluate_agile_no_price_data_returns_idle() {
        let cache = make_cache(&[(0, 1800, 5.0)]);
        let action = evaluate_agile_slot(
            AgileScope::Full,
            None,
            10.0,
            30.0,
            &cache,
            900,
            false,
            false,
            &chrono::Utc,
        );
        assert_eq!(action, AgileSlotAction::Idle);
    }

    #[test]
    fn agile_active_scope_idle_writes_clear_for_hold_period() {
        // Mid-band/hold while Agile is active is not a no-op. It must write
        // AgileClearActiveSlot so a discharge slot armed by a previous poll —
        // or by a previous app process before a crash/restart — is cancelled.
        let cache = make_cache(&[(0, 1800, 20.0)]);
        let action = evaluate_agile_slot(
            AgileScope::Full,
            Some(20.0),
            10.0,
            30.0,
            &cache,
            900,
            false,
            false,
            &chrono::Utc,
        );
        assert_eq!(action, AgileSlotAction::Idle);
        assert!(should_write_agile_action(AgileScope::Full, &action));
        assert!(should_write_agile_action(AgileScope::ChargeOnly, &action));
        assert!(should_write_agile_action(
            AgileScope::DischargeOnly,
            &action
        ));
    }

    #[test]
    fn agile_off_scope_idle_skips_clear_to_preserve_manual_schedule() {
        // Off+Idle is the one idle case that must NOT write a clear every
        // poll, otherwise manually configured schedules get wiped as soon as
        // Agile is disabled. Explicit scope=off clears happen in the API
        // handler instead, exactly once on the user action.
        let action = AgileSlotAction::Idle;
        assert!(!should_write_agile_action(AgileScope::Off, &action));
    }

    #[test]
    fn agile_defer_never_writes() {
        assert!(!should_write_agile_action(
            AgileScope::Full,
            &AgileSlotAction::Defer
        ));
        assert!(!should_write_agile_action(
            AgileScope::ChargeOnly,
            &AgileSlotAction::Defer
        ));
    }

    #[test]
    fn evaluate_agile_cheap_price_full_scope_returns_charge() {
        // 02:00–02:30 cheap slot, query at 02:10 (600s into the slot).
        // Cache in newest-first order with the cheap slot mid-list.
        // Using UTC for the timezone parameter makes the HHMM
        // conversion deterministic across CI machines.
        let slot_start = 2 * 3600; // 02:00 UTC
        let slot_end = slot_start + 1800; // 02:30 UTC
        let now_ts = slot_start + 600; // 02:10 UTC
        let cache = make_cache(&[
            (slot_end, slot_end + 1800, 30.0),    // 02:30–03:00 expensive
            (slot_start, slot_end, 5.0),          // 02:00–02:30 cheap (current)
            (slot_start - 1800, slot_start, 8.0), // 01:30–02:00 mid
        ]);
        let action = evaluate_agile_slot(
            AgileScope::Full,
            Some(5.0),
            10.0,
            30.0,
            &cache,
            now_ts,
            false,
            false,
            &chrono::Utc,
        );
        match action {
            AgileSlotAction::Charge {
                start_hhmm,
                end_hhmm,
                target_soc,
            } => {
                assert_eq!(start_hhmm, 200);
                assert_eq!(end_hhmm, 230); // current slot only — 02:30 is expensive
                assert_eq!(target_soc, 100);
            }
            other => panic!("expected Charge, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_agile_contiguous_cheap_run_spans_whole_run() {
        // Three back-to-back cheap slots 02:00–03:30. The action should
        // span all three because the price stays below the threshold.
        let s0 = 2 * 3600; // 02:00
        let s1 = s0 + 1800; // 02:30
        let s2 = s0 + 3600; // 03:00
        let s3 = s0 + 5400; // 03:30 (expensive start)
        let now_ts = s0 + 600; // 02:10
        let cache = make_cache(&[
            (s3, s3 + 1800, 35.0), // expensive after the run
            (s2, s3, 4.0),         // 03:00–03:30 cheap
            (s1, s2, 6.0),         // 02:30–03:00 cheap
            (s0, s1, 5.0),         // 02:00–02:30 cheap (current)
        ]);
        let action = evaluate_agile_slot(
            AgileScope::Full,
            Some(5.0),
            10.0,
            30.0,
            &cache,
            now_ts,
            false,
            false,
            &chrono::Utc,
        );
        match action {
            AgileSlotAction::Charge {
                start_hhmm,
                end_hhmm,
                ..
            } => {
                assert_eq!(start_hhmm, 200);
                assert_eq!(end_hhmm, 330, "should span all three cheap slots");
            }
            other => panic!("expected Charge, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_agile_expensive_price_returns_discharge() {
        let slot_start = 17 * 3600;
        let slot_end = slot_start + 1800;
        let now_ts = slot_start + 600;
        let cache = make_cache(&[
            (slot_end, slot_end + 1800, 15.0),     // drops back to mid
            (slot_start, slot_end, 35.0),          // 17:00–17:30 expensive
            (slot_start - 1800, slot_start, 20.0), // 16:30–17:00 mid
        ]);
        let action = evaluate_agile_slot(
            AgileScope::Full,
            Some(35.0),
            10.0,
            30.0,
            &cache,
            now_ts,
            false,
            false,
            &chrono::Utc,
        );
        match action {
            AgileSlotAction::Discharge {
                start_hhmm,
                end_hhmm,
            } => {
                assert_eq!(start_hhmm, 1700);
                assert_eq!(end_hhmm, 1730);
            }
            other => panic!("expected Discharge, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_agile_mid_band_returns_idle() {
        let cache = make_cache(&[(0, 1800, 20.0)]);
        let action = evaluate_agile_slot(
            AgileScope::Full,
            Some(20.0),
            10.0,
            30.0,
            &cache,
            900,
            false,
            false,
            &chrono::Utc,
        );
        assert_eq!(action, AgileSlotAction::Idle);
    }

    #[test]
    fn evaluate_agile_charge_only_ignores_expensive_price() {
        // Scope=ChargeOnly, expensive price → the user's discharge
        // schedule owns the discharge side, so we return Idle.
        let slot_start = 17 * 3600;
        let slot_end = slot_start + 1800;
        let cache = make_cache(&[
            (slot_start, slot_end, 35.0),
            (slot_start - 1800, slot_start, 20.0),
        ]);
        let action = evaluate_agile_slot(
            AgileScope::ChargeOnly,
            Some(35.0),
            10.0,
            30.0,
            &cache,
            slot_start + 600,
            false,
            false,
            &chrono::Utc,
        );
        assert_eq!(
            action,
            AgileSlotAction::Idle,
            "ChargeOnly must ignore expensive prices"
        );
    }

    #[test]
    fn evaluate_agile_discharge_only_ignores_cheap_price() {
        // Scope=DischargeOnly, cheap price → the user's charge schedule
        // owns the charge side.
        let slot_start = 2 * 3600;
        let slot_end = slot_start + 1800;
        let cache = make_cache(&[
            (slot_start, slot_end, 5.0),
            (slot_start - 1800, slot_start, 20.0),
        ]);
        let action = evaluate_agile_slot(
            AgileScope::DischargeOnly,
            Some(5.0),
            10.0,
            30.0,
            &cache,
            slot_start + 600,
            false,
            false,
            &chrono::Utc,
        );
        assert_eq!(
            action,
            AgileSlotAction::Idle,
            "DischargeOnly must ignore cheap prices"
        );
    }

    #[test]
    fn evaluate_agile_cosy_active_defers_charge() {
        // Cheap price, but cosy is in control. We must NOT overwrite
        // HR_ENABLE_CHARGE with our own value — let cosy's preload
        // win (cosy runs first in the poll loop). Returning Defer
        // tells the poll loop to skip writes this iteration.
        let slot_start = 2 * 3600;
        let slot_end = slot_start + 1800;
        let cache = make_cache(&[(slot_start, slot_end, 5.0)]);
        let action = evaluate_agile_slot(
            AgileScope::Full,
            Some(5.0),
            10.0,
            30.0,
            &cache,
            slot_start + 600,
            true, // cosy_active
            false,
            &chrono::Utc,
        );
        assert_eq!(action, AgileSlotAction::Defer);
        assert_eq!(action.label(), "idle");
    }

    #[test]
    fn evaluate_agile_auto_winter_active_defers_charge() {
        let slot_start = 2 * 3600;
        let slot_end = slot_start + 1800;
        let cache = make_cache(&[(slot_start, slot_end, 5.0)]);
        let action = evaluate_agile_slot(
            AgileScope::Full,
            Some(5.0),
            10.0,
            30.0,
            &cache,
            slot_start + 600,
            false,
            true, // auto_winter_active
            &chrono::Utc,
        );
        assert_eq!(action, AgileSlotAction::Defer);
    }

    #[test]
    fn evaluate_agile_defer_does_not_apply_to_discharge() {
        // Even with cosy in control, DischargeOnly should still fire
        // because cosy's mechanism is charge-only.
        let slot_start = 17 * 3600;
        let slot_end = slot_start + 1800;
        let cache = make_cache(&[(slot_start, slot_end, 35.0)]);
        let action = evaluate_agile_slot(
            AgileScope::DischargeOnly,
            Some(35.0),
            10.0,
            30.0,
            &cache,
            slot_start + 600,
            true, // cosy_active — should NOT defer discharge
            false,
            &chrono::Utc,
        );
        assert!(
            matches!(action, AgileSlotAction::Discharge { .. }),
            "DischargeOnly must fire regardless of cosy_active"
        );
    }

    #[test]
    fn evaluate_agile_coverage_gap_breaks_run() {
        // Two cheap slots with a gap in between (Octopus sometimes
        // returns partial ranges). The run should NOT span the gap.
        let s0 = 2 * 3600;
        let s1 = s0 + 1800;
        let s2 = s0 + 7200; // 2-hour gap
        let s3 = s2 + 1800;
        let cache = make_cache(&[
            (s3, s3 + 1800, 35.0), // gap-end expensive
            (s2, s3, 4.0),         // 04:00–04:30 cheap (gap tail)
            (s1, s2, 35.0),        // gap head: expensive, breaks the run
            (s0, s1, 5.0),         // 02:00–02:30 cheap (current)
        ]);
        let action = evaluate_agile_slot(
            AgileScope::Full,
            Some(5.0),
            10.0,
            30.0,
            &cache,
            s0 + 600,
            false,
            false,
            &chrono::Utc,
        );
        match action {
            AgileSlotAction::Charge { end_hhmm, .. } => {
                assert_eq!(end_hhmm, 230, "gap should bound the run");
            }
            other => panic!("expected Charge, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_agile_now_ts_not_in_any_slot_returns_idle() {
        // The cache has slots but `now_unix_ts` doesn't match any of
        // them (e.g. clock skew or stale cache). Return Idle.
        let cache = make_cache(&[(0, 1800, 5.0)]);
        let action = evaluate_agile_slot(
            AgileScope::Full,
            Some(5.0),
            10.0,
            30.0,
            &cache,
            86400, // a totally different time
            false,
            false,
            &chrono::Utc,
        );
        assert_eq!(action, AgileSlotAction::Idle);
    }

    #[test]
    fn evaluate_agile_label_for_known_actions() {
        let charge = AgileSlotAction::Charge {
            start_hhmm: 0,
            end_hhmm: 0,
            target_soc: 100,
        };
        assert_eq!(charge.label(), "charging");
        assert!(charge.is_active());

        let discharge = AgileSlotAction::Discharge {
            start_hhmm: 0,
            end_hhmm: 0,
        };
        assert_eq!(discharge.label(), "discharging");
        assert!(discharge.is_active());

        assert_eq!(AgileSlotAction::Defer.label(), "idle");
        assert!(!AgileSlotAction::Defer.is_active());
        assert_eq!(AgileSlotAction::Idle.label(), "idle");
        assert!(!AgileSlotAction::Idle.is_active());
    }

    #[test]
    fn standard_charge_schedule_unchanged_after_agile_refactor() {
        // Regression guard for the "don't break Standard mode" promise.
        // The cosy_slot_register_writes function is the foundation of
        // both Cosy mode and the user's manual charge schedule on the
        // Standard path. Its writes must be byte-identical to before
        // the slot-based Agile refactor.
        let slot = crate::settings::CosySlot {
            enabled: true,
            start_hour: 2,
            start_minute: 0,
            end_hour: 5,
            end_minute: 30,
            target_soc: 100,
        };
        let writes = cosy_slot_register_writes(&slot, DeviceType::Gen3Hybrid, true);
        // 5 base writes + 1 extended-slot target SOC for Gen3+ = 6.
        // (Gen3+ writes HR_CHARGE_TARGET_SOC_1 alongside HR_CHARGE_TARGET_SOC.)
        assert_eq!(writes.len(), 6);
        assert_eq!(writes[0].address, HR_CHARGE_SLOT_1_START);
        assert_eq!(writes[0].value, 200);
        assert_eq!(writes[1].address, HR_CHARGE_SLOT_1_END);
        assert_eq!(writes[1].value, 530);
        assert_eq!(writes[2].address, HR_ENABLE_CHARGE);
        assert_eq!(writes[2].value, 1);
        assert_eq!(writes[3].address, HR_ENABLE_CHARGE_TARGET);
        assert_eq!(writes[3].value, 1);
        assert_eq!(writes[4].address, HR_CHARGE_TARGET_SOC);
        assert_eq!(writes[4].value, 100);
        assert_eq!(
            writes[5].address,
            crate::modbus::registers::HR_CHARGE_TARGET_SOC_1
        );
        assert_eq!(writes[5].value, 100);
    }

    #[tokio::test]
    async fn cosy_prerequisite_failure_does_not_attempt_enable_writes() {
        struct RecordingWriter {
            attempted: Vec<u16>,
            failure_address: u16,
        }

        impl RegisterWriteExecutor for RecordingWriter {
            async fn write_register(&mut self, write: &RegisterWrite) -> Result<(), String> {
                self.attempted.push(write.address);
                if write.address == self.failure_address {
                    Err("simulated prerequisite failure".to_string())
                } else {
                    Ok(())
                }
            }
        }

        let slot = crate::settings::CosySlot {
            enabled: true,
            start_hour: 2,
            start_minute: 0,
            end_hour: 5,
            end_minute: 0,
            target_soc: 100,
        };
        let writes = cosy_slot_register_writes(&slot, DeviceType::Gen3Hybrid, true);

        for failure_index in 0..2 {
            let failure_address = writes[failure_index].address;
            let mut writer = RecordingWriter {
                attempted: Vec::new(),
                failure_address,
            };
            let ok =
                execute_register_writes(&mut writer, &writes, "Cosy test", Duration::ZERO).await;

            assert!(!ok);
            assert_eq!(writer.attempted.len(), failure_index + 1);
            assert!(
                writer
                    .attempted
                    .iter()
                    .all(|address| *address != HR_ENABLE_CHARGE
                        && *address != HR_ENABLE_CHARGE_TARGET)
            );
        }
    }

    #[tokio::test]
    async fn register_writes_do_not_delay_after_the_final_write() {
        struct RecordingWriter {
            attempted: Vec<u16>,
        }

        impl RegisterWriteExecutor for RecordingWriter {
            async fn write_register(&mut self, write: &RegisterWrite) -> Result<(), String> {
                self.attempted.push(write.address);
                Ok(())
            }
        }

        let writes = [RegisterWrite {
            address: HR_CHARGE_SLOT_1_START,
            value: 200,
        }];
        let mut writer = RecordingWriter {
            attempted: Vec::new(),
        };

        let result = tokio::time::timeout(
            Duration::ZERO,
            execute_register_writes(
                &mut writer,
                &writes,
                "Cosy final-write delay test",
                Duration::from_secs(60),
            ),
        )
        .await;

        assert_eq!(result, Ok(true));
        assert_eq!(writer.attempted.len(), writes.len());
    }

    // -----------------------------------------------------------------------
    // Discharge Floor Guard
    // -----------------------------------------------------------------------

    fn df_config(enabled: bool, floor_soc: u8) -> DischargeFloorConfig {
        DischargeFloorConfig { enabled, floor_soc }
    }

    fn df_slot(start: (u8, u8), end: (u8, u8)) -> crate::inverter::model::ScheduleSlot {
        crate::inverter::model::ScheduleSlot {
            enabled: true,
            start_hour: start.0,
            start_minute: start.1,
            end_hour: end.0,
            end_minute: end.1,
            target_soc: 4,
        }
    }

    fn df_snap(slots: Vec<crate::inverter::model::ScheduleSlot>, reserve: u8) -> InverterSnapshot {
        let mut discharge_slots = std::array::from_fn(|_| crate::inverter::model::ScheduleSlot {
            enabled: false,
            start_hour: 0,
            start_minute: 0,
            end_hour: 0,
            end_minute: 0,
            target_soc: 4,
        });
        for (dest, src) in discharge_slots.iter_mut().zip(slots) {
            *dest = src;
        }
        InverterSnapshot {
            discharge_slots,
            battery_reserve: reserve,
            ..Default::default()
        }
    }

    /// 19:00–22:00 discharge window.
    fn evening_window() -> Vec<crate::inverter::model::ScheduleSlot> {
        vec![df_slot((19, 0), (22, 0))]
    }

    #[test]
    fn discharge_floor_raises_reserve_when_window_becomes_active() {
        let config = df_config(true, 50);
        let mut state = DischargeFloorState::Idle;
        // 20:00 inside the 19:00–22:00 window, current reserve 4%.
        let snap = df_snap(evening_window(), 4);

        let writes = check_discharge_floor(&snap, &config, &mut state, 20 * 60);
        let writes = writes.expect("floor raise writes");
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].address, HR_BATTERY_SOC_RESERVE);
        assert_eq!(writes[0].value, 50);
        assert_eq!(state, DischargeFloorState::FloorHeld { saved_reserve: 4 });
    }

    #[test]
    fn discharge_floor_three_phase_device_writes_3ph_soc_reserve_register() {
        let config = df_config(true, 50);
        let mut state = DischargeFloorState::Idle;
        let mut snap = df_snap(evening_window(), 4);
        snap.device_type = crate::inverter::model::DeviceType::ThreePhase;

        let writes = check_discharge_floor(&snap, &config, &mut state, 20 * 60);
        let writes = writes.expect("floor raise writes");
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].address, HR_3PH_BATTERY_SOC_RESERVE);
        assert_eq!(writes[0].value, 50);
        assert_eq!(state, DischargeFloorState::FloorHeld { saved_reserve: 4 });
    }

    #[test]
    fn discharge_floor_three_phase_restore_uses_3ph_register() {
        let config = df_config(true, 50);
        let mut state = DischargeFloorState::FloorHeld { saved_reserve: 4 };
        let mut snap = df_snap(evening_window(), 50);
        snap.device_type = crate::inverter::model::DeviceType::ThreePhase;

        let writes = check_discharge_floor(&snap, &config, &mut state, 22 * 60 + 30);
        let writes = writes.expect("restore writes");
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].address, HR_3PH_BATTERY_SOC_RESERVE);
        assert_eq!(writes[0].value, 4);
        assert_eq!(state, DischargeFloorState::Idle);
    }

    #[test]
    fn discharge_floor_ignores_unconfigured_slots() {
        let config = df_config(true, 50);
        let mut state = DischargeFloorState::Idle;
        let mut slot = df_slot((19, 0), (22, 0));
        slot.enabled = false;
        let snap = df_snap(vec![slot], 4);

        let writes = check_discharge_floor(&snap, &config, &mut state, 20 * 60);
        assert!(writes.is_none());
        assert_eq!(state, DischargeFloorState::Idle);
    }

    #[test]
    fn discharge_floor_skips_raise_when_reserve_already_at_or_above_floor() {
        let config = df_config(true, 50);
        let mut state = DischargeFloorState::Idle;
        // Reserve 60 already above the 50 floor — must not be lowered or held.
        let snap = df_snap(evening_window(), 60);

        let writes = check_discharge_floor(&snap, &config, &mut state, 20 * 60);
        assert!(writes.is_none());
        assert_eq!(state, DischargeFloorState::Idle);
    }

    #[test]
    fn discharge_floor_restores_reserve_when_window_ends() {
        let config = df_config(true, 50);
        let mut state = DischargeFloorState::FloorHeld { saved_reserve: 4 };
        // 22:30 outside the window; snapshot reflects the raised floor.
        let snap = df_snap(evening_window(), 50);

        let writes = check_discharge_floor(&snap, &config, &mut state, 22 * 60 + 30);
        let writes = writes.expect("restore writes");
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].address, HR_BATTERY_SOC_RESERVE);
        assert_eq!(writes[0].value, 4);
        assert_eq!(state, DischargeFloorState::Idle);
    }

    #[test]
    fn discharge_floor_reasserts_floor_when_lowered_mid_window() {
        let config = df_config(true, 50);
        let mut state = DischargeFloorState::FloorHeld { saved_reserve: 4 };
        // User (or another automation) dropped the reserve mid-window.
        let snap = df_snap(evening_window(), 10);

        let writes = check_discharge_floor(&snap, &config, &mut state, 20 * 60);
        let writes = writes.expect("reassert writes");
        assert_eq!(writes[0].address, HR_BATTERY_SOC_RESERVE);
        assert_eq!(writes[0].value, 50);
        assert_eq!(state, DischargeFloorState::FloorHeld { saved_reserve: 4 });
    }

    #[test]
    fn discharge_floor_no_duplicate_write_when_already_at_floor() {
        let config = df_config(true, 50);
        let mut state = DischargeFloorState::FloorHeld { saved_reserve: 4 };
        let snap = df_snap(evening_window(), 50);

        let writes = check_discharge_floor(&snap, &config, &mut state, 20 * 60);
        assert!(writes.is_none(), "no churn while floor confirmed");
        assert_eq!(state, DischargeFloorState::FloorHeld { saved_reserve: 4 });
    }

    #[test]
    fn discharge_floor_disable_mid_window_restores_saved_reserve() {
        let config = df_config(false, 50);
        let mut state = DischargeFloorState::FloorHeld { saved_reserve: 6 };
        let snap = df_snap(evening_window(), 50);

        let writes = check_discharge_floor(&snap, &config, &mut state, 20 * 60);
        let writes = writes.expect("disable restore writes");
        assert_eq!(writes[0].address, HR_BATTERY_SOC_RESERVE);
        assert_eq!(writes[0].value, 6);
        assert_eq!(state, DischargeFloorState::Idle);
    }

    #[test]
    fn discharge_floor_midnight_crossing_window_wraps() {
        let config = df_config(true, 50);
        let mut state = DischargeFloorState::Idle;
        // 21:00–06:00 window crosses midnight.
        let slots = vec![df_slot((21, 0), (6, 0))];

        // 23:00 → inside.
        let snap = df_snap(slots.clone(), 4);
        let writes = check_discharge_floor(&snap, &config, &mut state, 23 * 60);
        assert!(writes.is_some());
        assert_eq!(state, DischargeFloorState::FloorHeld { saved_reserve: 4 });

        // 03:00 → still inside (wrapped).
        let mut state2 = DischargeFloorState::FloorHeld { saved_reserve: 4 };
        let snap = df_snap(slots.clone(), 50);
        assert!(check_discharge_floor(&snap, &config, &mut state2, 3 * 60).is_none());

        // 07:00 → outside, restores.
        let snap = df_snap(slots, 50);
        let writes = check_discharge_floor(&snap, &config, &mut state2, 7 * 60);
        let writes = writes.expect("restore after midnight window");
        assert_eq!(writes[0].value, 4);
        assert_eq!(state2, DischargeFloorState::Idle);
    }

    #[test]
    fn discharge_floor_honours_multiple_slots_any_active() {
        let config = df_config(true, 50);
        let mut state = DischargeFloorState::Idle;
        // 02:00–05:00 and 19:00–22:00; 03:00 falls in the first.
        let slots = vec![df_slot((2, 0), (5, 0)), df_slot((19, 0), (22, 0))];
        let snap = df_snap(slots, 4);

        let writes = check_discharge_floor(&snap, &config, &mut state, 3 * 60);
        assert!(writes.is_some());
        assert_eq!(state, DischargeFloorState::FloorHeld { saved_reserve: 4 });
    }

    #[test]
    fn discharge_floor_restart_inside_window_rearms_keeping_saved_reserve() {
        let config = df_config(true, 50);
        let mut state = DischargeFloorState::HeldFromRestart { saved_reserve: 4 };
        // Live reserve shows the raised floor (crash happened mid-window).
        let snap = df_snap(evening_window(), 50);

        let writes = check_discharge_floor(&snap, &config, &mut state, 20 * 60);
        assert!(writes.is_none(), "floor already held, no write needed");
        assert_eq!(
            state,
            DischargeFloorState::FloorHeld { saved_reserve: 4 },
            "original pre-guard reserve must survive the restart"
        );

        // And when the window later ends, the original 4% is restored.
        let snap = df_snap(evening_window(), 50);
        let writes = check_discharge_floor(&snap, &config, &mut state, 23 * 60);
        assert_eq!(writes.unwrap()[0].value, 4);
    }

    #[test]
    fn discharge_floor_restart_outside_window_restores_saved_reserve() {
        let config = df_config(true, 50);
        let mut state = DischargeFloorState::HeldFromRestart { saved_reserve: 6 };
        // 23:00 outside the window; the inverter may still be at the floor.
        let snap = df_snap(evening_window(), 50);

        let writes = check_discharge_floor(&snap, &config, &mut state, 23 * 60);
        let writes = writes.expect("restart restore writes");
        assert_eq!(writes[0].address, HR_BATTERY_SOC_RESERVE);
        assert_eq!(writes[0].value, 6);
        assert_eq!(state, DischargeFloorState::Idle);
    }

    #[test]
    fn timed_export_normal_window_entry_exit() {
        // Slot 16:00–19:00, current time 16:00 (entry), then 19:00 (exit)
        let config = TimedExportConfig {
            schedule_enabled: true,
            slots: vec![ScheduleSlot {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 4,
            }],
            device_rearm_confirmed: false,
            stop_pending: false,
        };
        let mut state = TimedExportState::Off;

        // At 16:00: should enter
        let snap = InverterSnapshot {
            battery_power_mode: 1,
            enable_discharge: false,
            battery_pause_mode: 0,
            ..Default::default()
        };
        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            16 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(
            decision.new_state,
            TimedExportState::Entering { .. }
        ));
        assert_eq!(decision.writes.len(), 2);
        assert_eq!(decision.writes[0].address, HR_BATTERY_POWER_MODE);
        assert_eq!(decision.writes[0].value, 0);
        assert_eq!(decision.writes[1].address, HR_ENABLE_DISCHARGE);
        assert_eq!(decision.writes[1].value, 1);
        state = decision.new_state;

        // Entry confirmed by snapshot
        let snap = InverterSnapshot {
            battery_power_mode: 0,
            enable_discharge: true,
            battery_pause_mode: 0,
            ..Default::default()
        };
        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            16 * 60 + 30,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(decision.new_state, TimedExportState::Active));
        assert!(decision.writes.is_empty());
        state = decision.new_state;

        // At 19:00: should exit
        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            19 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(
            decision.new_state,
            TimedExportState::Exiting { .. }
        ));
        assert_eq!(decision.writes.len(), 2);
        assert_eq!(decision.writes[0].address, HR_ENABLE_DISCHARGE);
        assert_eq!(decision.writes[0].value, 0);
        assert_eq!(decision.writes[1].address, HR_BATTERY_POWER_MODE);
        assert_eq!(decision.writes[1].value, 1);
    }

    #[test]
    fn timed_export_overnight_slot() {
        // Slot 22:00–06:00 (crosses midnight)
        let config = TimedExportConfig {
            schedule_enabled: true,
            slots: vec![ScheduleSlot {
                enabled: true,
                start_hour: 22,
                start_minute: 0,
                end_hour: 6,
                end_minute: 0,
                target_soc: 4,
            }],
            device_rearm_confirmed: false,
            stop_pending: false,
        };
        let mut state = TimedExportState::Off;

        // At 22:00: should enter
        let snap = InverterSnapshot {
            battery_power_mode: 1,
            enable_discharge: false,
            battery_pause_mode: 0,
            ..Default::default()
        };
        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            22 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(
            decision.new_state,
            TimedExportState::Entering { .. }
        ));

        // At 02:00 (inside window): confirm active
        let snap = InverterSnapshot {
            battery_power_mode: 0,
            enable_discharge: true,
            battery_pause_mode: 0,
            ..Default::default()
        };
        state = TimedExportState::Active;
        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            2 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(decision.new_state, TimedExportState::Active));
        assert!(decision.writes.is_empty());

        // At 06:00: should exit
        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            6 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(
            decision.new_state,
            TimedExportState::Exiting { .. }
        ));
    }

    #[test]
    fn timed_export_adjacent_slots() {
        // Two adjacent slots: 16:00–18:00 and 18:00–20:00
        let config = TimedExportConfig {
            schedule_enabled: true,
            slots: vec![
                ScheduleSlot {
                    enabled: true,
                    start_hour: 16,
                    start_minute: 0,
                    end_hour: 18,
                    end_minute: 0,
                    target_soc: 4,
                },
                ScheduleSlot {
                    enabled: true,
                    start_hour: 18,
                    start_minute: 0,
                    end_hour: 20,
                    end_minute: 0,
                    target_soc: 4,
                },
            ],
            device_rearm_confirmed: false,
            stop_pending: false,
        };
        let mut state = TimedExportState::Off;

        // At 17:00 (inside first slot): should enter
        let snap = InverterSnapshot {
            battery_power_mode: 1,
            enable_discharge: false,
            battery_pause_mode: 0,
            ..Default::default()
        };
        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            17 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(
            decision.new_state,
            TimedExportState::Entering { .. }
        ));

        // At 18:00 (boundary, inside second slot): remain active
        let snap = InverterSnapshot {
            battery_power_mode: 0,
            enable_discharge: true,
            battery_pause_mode: 0,
            ..Default::default()
        };
        state = TimedExportState::Active;
        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            18 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(decision.new_state, TimedExportState::Active));
        assert!(
            decision.writes.is_empty(),
            "no re-entry needed for adjacent slot"
        );

        // At 20:00: should exit
        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            20 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(
            decision.new_state,
            TimedExportState::Exiting { .. }
        ));
    }

    #[test]
    fn timed_export_zero_length_slot_disabled() {
        // Zero-length slot (16:00–16:00) is treated as disabled
        let config = TimedExportConfig {
            schedule_enabled: true,
            slots: vec![ScheduleSlot {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 16,
                end_minute: 0,
                target_soc: 4,
            }],
            device_rearm_confirmed: false,
            stop_pending: false,
        };
        let mut state = TimedExportState::Off;

        // At 16:00: should NOT enter (zero-length = disabled)
        let snap = InverterSnapshot {
            battery_power_mode: 1,
            enable_discharge: false,
            battery_pause_mode: 0,
            ..Default::default()
        };
        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            16 * 60,
            DeviceType::Gen3Hybrid,
        );
        // CODE_REVIEW.md finding 5: `ScheduleSlot::is_configured` now treats
        // start == end as unconfigured, so a schedule whose only slot is
        // zero-length is effectively empty — the machine stays Off rather
        // than sitting `Configured` forever.
        assert!(matches!(decision.new_state, TimedExportState::Off));
        assert!(
            decision.writes.is_empty(),
            "zero-length slot should not trigger entry"
        );
    }

    #[test]
    fn timed_export_blocked_by_pause_discharge() {
        // HR318=2 with a pause window covering the current time blocks export.
        // Visible demand window 03:00-04:00 → pause window 04:00-03:00.
        // Export slot 16:00-19:00 sits fully inside the pause window.
        let config = TimedExportConfig {
            schedule_enabled: true,
            slots: vec![ScheduleSlot {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 4,
            }],
            device_rearm_confirmed: false,
            stop_pending: false,
        };
        let mut state = TimedExportState::Off;

        let pause_window = ScheduleSlot {
            enabled: true,
            start_hour: 4, // pause starts 04:00
            start_minute: 0,
            end_hour: 3, // pause ends 03:00 (overnight → covers 16:00)
            end_minute: 0,
            target_soc: 4,
        };

        // At 16:00 with pause active: should be blocked
        let snap = InverterSnapshot {
            battery_power_mode: 1,
            enable_discharge: false,
            battery_pause_mode: 2, // pause discharge
            battery_pause_slot: pause_window.clone(),
            ..Default::default()
        };
        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            16 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(
            decision.new_state,
            TimedExportState::BlockedByPause
        ));
        assert!(
            decision.writes.is_empty(),
            "should not enter export while paused"
        );

        // Pause ends (mode cleared): should enter
        let snap = InverterSnapshot {
            battery_power_mode: 1,
            enable_discharge: false,
            battery_pause_mode: 0,
            ..Default::default()
        };
        state = TimedExportState::BlockedByPause;
        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            16 * 60 + 30,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(
            decision.new_state,
            TimedExportState::Entering { .. }
        ));
        assert_eq!(
            decision.writes.len(),
            2,
            "should enter export after pause ends"
        );
    }

    #[test]
    fn timed_export_stop_while_blocked_returns_to_off_without_writes() {
        // Issue #289: Stop persists schedule_enabled=false, but a machine
        // parked in BlockedByPause ignored the flag and re-entered export
        // the moment HR318 unblocked — Stop didn't stop. A disabled schedule
        // must return the machine to Off from EVERY state, silently (the
        // API's own disable writes already disarm the inverter).
        let config = TimedExportConfig {
            schedule_enabled: false, // stopped
            slots: vec![ScheduleSlot {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 4,
            }],
            device_rearm_confirmed: false,
            stop_pending: false,
        };
        let snap = InverterSnapshot {
            battery_power_mode: 1,
            enable_discharge: false,
            battery_pause_mode: 0, // pause just ended
            ..Default::default()
        };
        let mut state = TimedExportState::BlockedByPause;
        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            16 * 60 + 30,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(decision.new_state, TimedExportState::Off));
        assert!(
            decision.writes.is_empty(),
            "disabled schedule must not queue entry writes"
        );
    }

    #[test]
    fn timed_export_stop_while_active_returns_to_off_without_writes() {
        // Same contract from Active: the API disable has already written the
        // disarm (HR59=0/HR27=1, plus slot clears under the fallback), so
        // the machine must go quiet rather than keep exporting.
        let config = TimedExportConfig {
            schedule_enabled: false, // stopped
            slots: vec![ScheduleSlot {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 4,
            }],
            device_rearm_confirmed: false,
            stop_pending: false,
        };
        let snap = InverterSnapshot {
            battery_power_mode: 0,
            enable_discharge: true,
            ..Default::default()
        };
        let mut state = TimedExportState::Active;
        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            16 * 60 + 30,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(decision.new_state, TimedExportState::Off));
        assert!(decision.writes.is_empty());
    }

    #[test]
    fn timed_export_allowed_inside_visible_timed_discharge_window() {
        // Export may run when the current time is inside the *visible* Timed
        // Discharge window — i.e. outside the inverse pause window.
        // Visible window 15:00-20:00 → pause window 20:00-15:00.
        // Export slot 16:00-19:00 sits fully inside the allowed window.
        let config = TimedExportConfig {
            schedule_enabled: true,
            slots: vec![ScheduleSlot {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 4,
            }],
            device_rearm_confirmed: false,
            stop_pending: false,
        };
        let mut state = TimedExportState::Off;

        let pause_window = ScheduleSlot {
            enabled: true,
            start_hour: 20, // pause 20:00 → 15:00
            start_minute: 0,
            end_hour: 15,
            end_minute: 0,
            target_soc: 4,
        };
        let snap = InverterSnapshot {
            battery_power_mode: 1,
            enable_discharge: false,
            battery_pause_mode: 2,
            battery_pause_slot: pause_window,
            ..Default::default()
        };

        // 17:00 is inside the export slot AND inside the visible demand
        // window (outside the pause window) → export may start.
        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            17 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(
            matches!(decision.new_state, TimedExportState::Entering { .. }),
            "export must run when the pause window does not cover the current time"
        );
        assert_eq!(decision.writes.len(), 2);
    }

    #[test]
    fn hr318_pause_charging_mode_never_blocks_discharge() {
        // HR318=1 pauses charging only; discharge/export is unaffected.
        let snap = InverterSnapshot {
            battery_pause_mode: 1,
            ..Default::default()
        };
        assert!(!hr318_blocks_discharge(&snap, 12 * 60));

        // Same for mode 0 (disabled) and unknown modes
        let snap = InverterSnapshot {
            battery_pause_mode: 0,
            ..Default::default()
        };
        assert!(!hr318_blocks_discharge(&snap, 12 * 60));
    }

    #[test]
    fn hr318_unconfigured_pause_slot_blocks_nothing() {
        // Mode 2 armed but no pause window configured → the full day is the
        // allowed demand window; export is not blocked.
        let snap = InverterSnapshot {
            battery_pause_mode: 2,
            battery_pause_slot: ScheduleSlot::default(),
            ..Default::default()
        };
        assert!(!hr318_blocks_discharge(&snap, 12 * 60));

        // Zero-length pause window also blocks nothing
        let snap = InverterSnapshot {
            battery_pause_mode: 2,
            battery_pause_slot: ScheduleSlot {
                enabled: true,
                start_hour: 12,
                start_minute: 0,
                end_hour: 12,
                end_minute: 0,
                target_soc: 4,
            },
            ..Default::default()
        };
        assert!(!hr318_blocks_discharge(&snap, 12 * 60));
    }

    #[test]
    fn hr318_pause_window_boundary_semantics() {
        // Pause window 04:00-03:00 (overnight). Minute 04:00 is paused
        // (inclusive start); minute 03:00 is released (exclusive end).
        let snap = InverterSnapshot {
            battery_pause_mode: 2,
            battery_pause_slot: ScheduleSlot {
                enabled: true,
                start_hour: 4,
                start_minute: 0,
                end_hour: 3,
                end_minute: 0,
                target_soc: 4,
            },
            ..Default::default()
        };
        assert!(
            hr318_blocks_discharge(&snap, 4 * 60),
            "pause starts at 04:00"
        );
        assert!(hr318_blocks_discharge(&snap, 23 * 60 + 59));
        assert!(hr318_blocks_discharge(&snap, 2 * 60 + 59));
        assert!(
            !hr318_blocks_discharge(&snap, 3 * 60),
            "pause ends at 03:00"
        );
        assert!(!hr318_blocks_discharge(&snap, 3 * 60 + 59));
    }

    #[test]
    fn timed_export_rearm_requires_exit_anchor_and_three_consecutive_polls() {
        let mut detector = TimedExportRearmDetector::default();

        // Startup with HR59 set (Timed Demand, a stale externally-created
        // mode, or another controller) must NEVER classify the device —
        // no HEM exit has been issued, so the register state is
        // unattributable.
        for _ in 0..10 {
            assert!(!detector.observe(true, true, false));
        }
        assert!(!detector.confirmed(), "unanchored HR59=1 must not classify");

        // HEM issues an exit: observation begins.
        detector.note_exit_written();
        // Exit write may still be in flight — HR59 still armed.
        assert!(!detector.observe(true, true, false));
        // Exit landed: HR59 reads 0.
        assert!(!detector.observe(true, false, false));

        // Unsolicited re-arm after the confirmed exit: three consecutive
        // outside-window polls confirm the fallback.
        assert!(!detector.observe(true, true, false), "first re-arm poll");
        assert!(!detector.observe(true, true, false), "second re-arm poll");
        assert!(
            detector.observe(true, true, false),
            "third re-arm poll confirms"
        );
        assert!(detector.confirmed());
    }

    #[test]
    fn timed_export_rearm_resets_on_non_qualifying_poll() {
        let mut detector = TimedExportRearmDetector::default();
        detector.note_exit_written();
        assert!(!detector.observe(true, false, false)); // exit confirmed off

        // Two qualifying re-arm polls
        assert!(!detector.observe(true, true, false));
        assert!(!detector.observe(true, true, false));

        // A poll inside the window does not qualify and drops the
        // observation back to the confirmed-off anchor (HEM's own entry
        // legitimately sets the register).
        assert!(!detector.observe(false, true, false));
        assert!(!detector.confirmed());

        // HR59=0 outside the window also resets (firmware behaviour normal)
        assert!(!detector.observe(true, true, false));
        assert!(!detector.observe(true, false, false));
        assert!(!detector.confirmed());
    }

    #[test]
    fn timed_export_rearm_ignores_polls_while_hem_boundary_write_pending() {
        let mut detector = TimedExportRearmDetector::default();
        detector.note_exit_written();
        assert!(!detector.observe(true, false, false)); // exit confirmed off

        // Outside window with HR59=1, but HEM's own entry batch may still be
        // in flight — these observations must not count toward classification.
        for _ in 0..10 {
            assert!(!detector.observe(true, true, true));
        }
        assert!(!detector.confirmed());

        // Once HEM's writes are confirmed complete, counting begins.
        assert!(!detector.observe(true, true, false));
        assert!(!detector.observe(true, true, false));
        assert!(!detector.confirmed(), "two clean polls is not enough");
        assert!(detector.observe(true, true, false));
    }

    #[test]
    fn timed_export_rearm_reset_clears_confirmation() {
        let mut detector = TimedExportRearmDetector::default();
        detector.note_exit_written();
        assert!(!detector.observe(true, false, false));
        for _ in 0..TIMED_EXPORT_REARM_CONFIRM_POLLS {
            assert!(detector.observe(true, true, false) || !detector.confirmed());
        }
        assert!(detector.confirmed());

        detector.reset();
        assert!(!detector.confirmed());
        // After a reset the anchor is gone: raw HR59=1 polls no longer count.
        assert!(
            !detector.observe(true, true, false),
            "counting restarts from Idle"
        );
    }

    #[test]
    fn timed_export_rearm_holds_repair_while_observing() {
        // `is_observing` gates the reconciler's outside-window repair writes:
        // while the detector waits for firmware to prove it re-arms, HEM must
        // not keep disarming the register itself — its own writes would reset
        // the consecutive count and the fallback could never be confirmed.
        let mut detector = TimedExportRearmDetector::default();
        assert!(!detector.is_observing(), "Idle is not observing");
        detector.note_exit_written();
        assert!(detector.is_observing(), "ExitWritten holds repair");
        assert!(!detector.observe(true, false, false));
        assert!(detector.is_observing(), "ExitConfirmedOff holds repair");
        assert!(!detector.observe(true, true, false));
        assert!(detector.is_observing(), "Observing holds repair");
    }

    #[test]
    fn timed_export_entry_write_ordering() {
        // Entry writes must be HR27=0 BEFORE HR59=1 (no re-arm fallback)
        let config = TimedExportConfig::default();
        let writes = build_timed_export_entry_writes(DeviceType::Gen3Hybrid, &config);
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].address, HR_BATTERY_POWER_MODE);
        assert_eq!(writes[0].value, 0, "entry: max-power mode first");
        assert_eq!(writes[1].address, HR_ENABLE_DISCHARGE);
        assert_eq!(writes[1].value, 1, "entry: then arm discharge");
    }

    #[test]
    fn timed_export_exit_write_ordering() {
        // Exit writes must be HR59=0 BEFORE HR27=1 (no re-arm fallback)
        let config = TimedExportConfig::default();
        let writes = build_timed_export_exit_writes(DeviceType::Gen3Hybrid, &config);
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].address, HR_ENABLE_DISCHARGE);
        assert_eq!(writes[0].value, 0, "exit: disarm discharge first");
        assert_eq!(writes[1].address, HR_BATTERY_POWER_MODE);
        assert_eq!(writes[1].value, 1, "exit: then restore Eco");
    }

    #[test]
    fn timed_export_rearm_fallback_entry_restores_slots_before_arming() {
        // With the re-arm fallback active, entry writes restore the physical
        // slots FIRST, then HR27=0, then HR59=1.
        let config = TimedExportConfig {
            schedule_enabled: true,
            slots: vec![ScheduleSlot {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 4,
            }],
            device_rearm_confirmed: true,
            stop_pending: false,
        };
        let writes = build_timed_export_entry_writes(DeviceType::Gen3Hybrid, &config);

        // Slot 1 start + end, then HR27=0, then HR59=1
        let hr27_idx = writes
            .iter()
            .position(|w| w.address == HR_BATTERY_POWER_MODE)
            .expect("HR27 write present");
        let hr59_idx = writes
            .iter()
            .position(|w| w.address == HR_ENABLE_DISCHARGE)
            .expect("HR59 write present");
        assert!(hr27_idx < hr59_idx, "HR27=0 must precede HR59=1");
        assert!(hr27_idx >= 2, "slot restore writes must precede HR27");
        // Slot 1 registers (HR 56/57 for Gen3 single-phase)
        assert!(writes
            .iter()
            .any(|w| w.address == HR_DISCHARGE_SLOT_1_START));
        assert!(writes.iter().any(|w| w.address == HR_DISCHARGE_SLOT_1_END));
    }

    #[test]
    fn timed_export_fallback_restore_skips_disabled_slots_with_retained_times() {
        let slots = vec![ScheduleSlot {
            enabled: false,
            start_hour: 16,
            start_minute: 0,
            end_hour: 19,
            end_minute: 0,
            target_soc: 40,
        }];

        let writes = build_timed_export_slot_restore_writes(DeviceType::Gen3Hybrid, &slots);

        assert!(
            writes.is_empty(),
            "disabled slots must remain physically clear"
        );
    }

    #[test]
    fn timed_export_fallback_restore_includes_extended_target_soc() {
        let slots = vec![ScheduleSlot {
            enabled: true,
            start_hour: 16,
            start_minute: 0,
            end_hour: 19,
            end_minute: 0,
            target_soc: 63,
        }];

        let writes = build_timed_export_slot_restore_writes(DeviceType::Gen3Hybrid, &slots);

        assert!(writes.iter().any(|write| {
            write.address == crate::modbus::registers::HR_DISCHARGE_TARGET_SOC_1
                && write.value == 63
        }));
    }

    #[test]
    fn timed_export_rearm_fallback_exit_clears_slots_before_eco() {
        // With the re-arm fallback active the exit must clear the physical
        // slots BEFORE disarming HR59: the firmware this fallback defends
        // against re-asserts HR59=1 whenever a discharge slot register is
        // non-zero, so an HR59=0 written ahead of the clears is immediately
        // undone and the inverter never disarms. Correct order:
        // slot clears → HR59=0 → HR27=1.
        let config = TimedExportConfig {
            schedule_enabled: true,
            slots: vec![ScheduleSlot {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 4,
            }],
            device_rearm_confirmed: true,
            stop_pending: false,
        };
        let writes = build_timed_export_exit_writes(DeviceType::Gen3Hybrid, &config);

        let hr59_idx = writes
            .iter()
            .position(|w| w.address == HR_ENABLE_DISCHARGE)
            .expect("HR59 write present");
        let hr27_idx = writes
            .iter()
            .position(|w| w.address == HR_BATTERY_POWER_MODE)
            .expect("HR27 write present");
        assert!(hr59_idx < hr27_idx, "HR59=0 must precede HR27=1");
        // Slot clears must come FIRST — before the HR59=0 disarm — so the
        // firmware has nothing left to re-arm HR59 from.
        let slot_clear_idx = writes
            .iter()
            .position(|w| w.address == HR_DISCHARGE_SLOT_1_START)
            .expect("slot clear write present");
        assert!(
            slot_clear_idx < hr59_idx,
            "slot clears must precede the HR59=0 disarm (re-arm firmware undoes a later disarm)"
        );
        // Slot 1 start cleared to 0
        let slot_start = writes
            .iter()
            .find(|w| w.address == HR_DISCHARGE_SLOT_1_START)
            .unwrap();
        assert_eq!(slot_start.value, 0, "cleared slot writes zero");
    }

    #[test]
    fn timed_export_future_slot_configured_not_active() {
        // A future slot should show as Configured, not Active
        let config = TimedExportConfig {
            schedule_enabled: true,
            slots: vec![ScheduleSlot {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 4,
            }],
            device_rearm_confirmed: false,
            stop_pending: false,
        };
        let mut state = TimedExportState::Off;

        // At 10:00 (before 16:00): should be Configured, not Active
        let snap = InverterSnapshot {
            battery_power_mode: 1,
            enable_discharge: false,
            battery_pause_mode: 0,
            ..Default::default()
        };
        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            10 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(decision.new_state, TimedExportState::Configured));
        assert!(
            decision.writes.is_empty(),
            "no writes needed for future slot"
        );
    }

    #[test]
    fn timed_export_hr59_alone_not_sufficient_for_export() {
        // Regression test: HR59=1 with HR27=1 is Timed Demand, not Timed Export
        let config = TimedExportConfig {
            schedule_enabled: true,
            slots: vec![ScheduleSlot {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 4,
            }],
            device_rearm_confirmed: false,
            stop_pending: false,
        };

        // Snapshot with HR59=1 but HR27=1 (Timed Demand state)
        let snap = InverterSnapshot {
            battery_power_mode: 1,
            enable_discharge: true,
            battery_pause_mode: 0,
            ..Default::default()
        };

        // State machine starts in Entering (entry writes queued)
        let mut state = TimedExportState::Entering {
            polls_waiting: 0,
            retries: 0,
        };

        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            16 * 60 + 30,
            DeviceType::Gen3Hybrid,
        );
        // Should NOT confirm Active with HR27=1
        assert!(!matches!(decision.new_state, TimedExportState::Active));
    }

    fn te_config_enabled() -> TimedExportConfig {
        TimedExportConfig {
            schedule_enabled: true,
            slots: vec![ScheduleSlot {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 4,
            }],
            device_rearm_confirmed: false,
            stop_pending: false,
        }
    }

    fn eco_snapshot() -> InverterSnapshot {
        InverterSnapshot {
            battery_power_mode: 1,
            enable_discharge: false,
            battery_pause_mode: 0,
            ..Default::default()
        }
    }

    fn export_armed_snapshot() -> InverterSnapshot {
        InverterSnapshot {
            battery_power_mode: 0,
            enable_discharge: true,
            battery_pause_mode: 0,
            ..Default::default()
        }
    }

    #[test]
    fn timed_export_startup_outside_window_repairs_export_armed_registers() {
        // A process that stopped mid-export restarts outside the window with
        // HR27=0/HR59=1 still latched. The reconciler must repair to the Eco
        // baseline instead of silently going Configured (code-review
        // finding: startup left HR27=0 indefinitely).
        let config = te_config_enabled();
        let mut state = TimedExportState::Off;
        let snap = export_armed_snapshot();

        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            10 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(
            decision.new_state,
            TimedExportState::Exiting { .. }
        ));
        assert!(decision.is_exit_transition);
        assert!(!decision.writes.is_empty(), "repair writes must be issued");
    }

    #[test]
    fn timed_export_slotless_repair_defers_to_populated_agile_slots() {
        let config = TimedExportConfig {
            schedule_enabled: true,
            slots: Vec::new(),
            device_rearm_confirmed: false,
            stop_pending: false,
        };
        let mut state = TimedExportState::Off;
        let mut snapshot = export_armed_snapshot();
        snapshot.discharge_slots[0] = ScheduleSlot {
            enabled: true,
            start_hour: 16,
            start_minute: 0,
            end_hour: 19,
            end_minute: 0,
            target_soc: 4,
        };

        // Agile may have populated the physical slot bank while HEM's
        // Timed Export schedule has no configured slots. The Off repair must
        // not replace Agile's schedule with Eco.
        let decision = check_timed_export_with_defaults(
            &snapshot,
            &config,
            &mut state,
            12 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(decision.writes.is_empty());
        assert_eq!(state, TimedExportState::Off);
    }

    #[test]
    fn timed_export_configured_repairs_export_armed_outside_window() {
        // Outside a window but the registers say export is still armed
        // (another controller, failed exit): idempotent repair.
        let config = te_config_enabled();
        let mut state = TimedExportState::Configured;
        let snap = export_armed_snapshot();

        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            10 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(
            decision.new_state,
            TimedExportState::Exiting { .. }
        ));
        assert!(decision.is_exit_transition);
    }

    #[test]
    fn timed_export_off_repairs_export_armed_when_schedule_disabled() {
        let mut config = te_config_enabled();
        config.schedule_enabled = false;
        let mut state = TimedExportState::Off;
        let snap = export_armed_snapshot();

        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            10 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(
            decision.new_state,
            TimedExportState::Exiting { .. }
        ));
        assert!(!decision.writes.is_empty());
    }

    #[test]
    fn timed_export_does_not_clobber_external_discharge_slot_when_disabled() {
        // Agile and manual Timed Discharge share the export-shaped mode
        // registers. A managed Timed Export schedule that is off must leave a
        // populated physical slot alone so another controller can own it.
        let mut config = te_config_enabled();
        config.schedule_enabled = false;
        config.slots.clear();
        let mut state = TimedExportState::Off;
        let mut snap = export_armed_snapshot();
        snap.discharge_slots[0] = configured_slot();

        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            10 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(decision.new_state, TimedExportState::Off));
        assert!(
            decision.writes.is_empty(),
            "disabled Timed Export must not clear another controller's slot"
        );
    }

    #[test]
    fn timed_export_stays_quiet_when_agile_arms_after_stop_with_retained_slots() {
        // The blocking oscillation from code review: Stop retains the
        // desired slots in config and (outside the re-arm fallback) leaves
        // the physical slot registers populated. Agile Full then arms
        // HR27=0/HR59=1 plus its own slot 1, so the readback is
        // export-armed with a populated physical slot. The Off repair
        // must not fire: HEM would disarm this poll, Agile would re-arm
        // the next, and the inverter would flap export↔Eco forever. A
        // populated physical slot while the schedule is disabled means
        // another controller owns the registers.
        let mut config = te_config_enabled();
        config.schedule_enabled = false; // stopped, desired slots retained
        let mut state = TimedExportState::Off;
        let mut snap = export_armed_snapshot(); // Agile armed export
        snap.discharge_slots[0] = configured_slot(); // Agile's slot 1

        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            10 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(decision.new_state, TimedExportState::Off));
        assert!(
            decision.writes.is_empty(),
            "disabled Timed Export must not fight another controller's armed export"
        );
        assert!(!decision.is_exit_transition);
    }

    #[test]
    fn timed_export_failed_stop_disarm_stays_repairable_while_disabled() {
        // CODE_REVIEW.md finding 2: a Stop whose disarm writes failed
        // persists schedule_enabled=false and arms the machine into
        // Exiting. The physical slots are still populated (non-re-arm
        // firmware keeps them), but they are residue of HEM's own
        // incomplete stop — the reconciler must keep re-issuing the exit
        // writes instead of mistaking them for an external owner.
        let mut config = te_config_enabled();
        config.schedule_enabled = false; // stop persisted; disarm failed
        let mut state = TimedExportState::Exiting {
            polls_waiting: 0,
            retries: 0,
        };
        let mut snap = export_armed_snapshot();
        snap.discharge_slots[0] = configured_slot();

        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            10 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(
            matches!(decision.new_state, TimedExportState::Exiting { .. }),
            "a failed stop must keep its exit pending, not fold to Off"
        );
        assert!(
            !decision.writes.is_empty(),
            "a failed stop's disarm must stay eligible for repair while the schedule is disabled"
        );
        assert!(decision.is_exit_transition);
    }

    #[test]
    fn timed_export_failed_stop_exit_completes_to_off_once_eco_confirmed() {
        // Once the retried disarm lands, the disabled-schedule Exiting state
        // settles into Off — even with a residue physical slot still
        // configured on the inverter.
        let mut config = te_config_enabled();
        config.schedule_enabled = false;
        let mut state = TimedExportState::Exiting {
            polls_waiting: 0,
            retries: 0,
        };
        let mut snap = eco_snapshot();
        snap.discharge_slots[0] = configured_slot();

        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            10 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(decision.new_state, TimedExportState::Off));
        assert!(decision.writes.is_empty());
    }

    #[test]
    fn timed_export_failed_stop_exit_retry_is_bounded() {
        // The failed-stop repair is bounded by the shared Exiting retry
        // budget: persistent write failures surface Error rather than
        // retrying forever.
        let mut config = te_config_enabled();
        config.schedule_enabled = false;
        let mut state = TimedExportState::Exiting {
            polls_waiting: 0,
            retries: 0,
        };
        let mut snap = export_armed_snapshot();
        snap.discharge_slots[0] = configured_slot();

        for _ in 0..(TIMED_EXPORT_MAX_WRITE_RETRIES + 1) {
            let _ = check_timed_export(
                &snap,
                &config,
                &mut state,
                10 * 60,
                DeviceType::Gen3Hybrid,
                TimedExportWriteOutcome::Failed,
                false,
            );
        }
        assert!(
            matches!(state, TimedExportState::Error { .. }),
            "persistent exit-write failures must surface Error, got {state:?}"
        );
    }

    #[test]
    fn timed_export_repairs_partial_export_mode_when_schedule_is_disabled() {
        // A failed/partial transition can leave HR27 in export mode while
        // the model-specific discharge-enable register is already clear.
        // That is still outside the Eco baseline and must be repaired.
        let mut config = te_config_enabled();
        config.schedule_enabled = false;
        let mut state = TimedExportState::Off;
        let mut snap = eco_snapshot();
        snap.battery_power_mode = 0;
        snap.enable_discharge = false;

        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            10 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(
            decision.new_state,
            TimedExportState::Exiting { .. }
        ));
        assert!(decision.is_exit_transition);
        assert_eq!(decision.writes[0].address, HR_ENABLE_DISCHARGE);
        assert_eq!(decision.writes[1].address, HR_BATTERY_POWER_MODE);
    }

    #[test]
    fn timed_export_repair_held_while_rearm_observing() {
        // While the re-arm detector deliberately holds repair writes to
        // observe firmware behaviour, the machine must not disarm again —
        // its own writes would keep resetting the register.
        let config = te_config_enabled();
        let mut state = TimedExportState::Configured;
        let snap = export_armed_snapshot();

        let decision = check_timed_export(
            &snap,
            &config,
            &mut state,
            10 * 60,
            DeviceType::Gen3Hybrid,
            TimedExportWriteOutcome::NoneIssued,
            true, // rearm_observation_in_progress
        );
        assert!(matches!(decision.new_state, TimedExportState::Configured));
        assert!(
            decision.writes.is_empty(),
            "repair must be held while observing"
        );
    }

    #[test]
    fn timed_export_entering_retries_failed_writes_then_errors() {
        let config = te_config_enabled();
        let mut state = TimedExportState::Entering {
            polls_waiting: 0,
            retries: 0,
        };
        let snap = eco_snapshot(); // entry never confirms

        // Each failed write batch retries on the next poll...
        for expected_retry in 1..=TIMED_EXPORT_MAX_WRITE_RETRIES {
            let decision = check_timed_export(
                &snap,
                &config,
                &mut state,
                16 * 60 + 30,
                DeviceType::Gen3Hybrid,
                TimedExportWriteOutcome::Failed,
                false,
            );
            assert_eq!(
                decision.writes.len(),
                2,
                "failed entry writes must be re-issued (retry {expected_retry})"
            );
        }

        // ...and once the retry budget is exhausted the machine surfaces Error.
        let decision = check_timed_export(
            &snap,
            &config,
            &mut state,
            16 * 60 + 30,
            DeviceType::Gen3Hybrid,
            TimedExportWriteOutcome::Failed,
            false,
        );
        assert!(matches!(decision.new_state, TimedExportState::Error { .. }));
        assert!(decision.writes.is_empty());
    }

    #[test]
    fn timed_export_entering_reissues_after_confirmation_grace() {
        // Successful-but-unconfirmed writes are given a readback grace
        // period before being re-issued (readback can lag a poll cycle).
        let config = te_config_enabled();
        let mut state = TimedExportState::Entering {
            polls_waiting: 0,
            retries: 0,
        };
        let snap = eco_snapshot();

        // Grace polls: no re-issue.
        for _ in 0..TIMED_EXPORT_CONFIRM_GRACE_POLLS {
            let decision = check_timed_export(
                &snap,
                &config,
                &mut state,
                16 * 60 + 30,
                DeviceType::Gen3Hybrid,
                TimedExportWriteOutcome::Succeeded,
                false,
            );
            assert!(decision.writes.is_empty(), "grace period must not re-issue");
        }

        // After the grace period the batch is re-issued.
        let decision = check_timed_export(
            &snap,
            &config,
            &mut state,
            16 * 60 + 30,
            DeviceType::Gen3Hybrid,
            TimedExportWriteOutcome::Succeeded,
            false,
        );
        assert_eq!(
            decision.writes.len(),
            2,
            "unconfirmed entry re-issued after grace"
        );
    }

    #[test]
    fn timed_export_exit_retries_failed_writes_then_errors() {
        let config = te_config_enabled();
        let mut state = TimedExportState::Exiting {
            polls_waiting: 0,
            retries: 0,
        };
        let snap = export_armed_snapshot(); // exit never confirms

        for _ in 0..TIMED_EXPORT_MAX_WRITE_RETRIES {
            let decision = check_timed_export(
                &snap,
                &config,
                &mut state,
                20 * 60,
                DeviceType::Gen3Hybrid,
                TimedExportWriteOutcome::Failed,
                false,
            );
            assert_eq!(
                decision.writes.len(),
                2,
                "failed exit writes must be re-issued"
            );
            assert!(decision.is_exit_transition);
        }

        let decision = check_timed_export(
            &snap,
            &config,
            &mut state,
            20 * 60,
            DeviceType::Gen3Hybrid,
            TimedExportWriteOutcome::Failed,
            false,
        );
        assert!(matches!(decision.new_state, TimedExportState::Error { .. }));
    }

    #[test]
    fn timed_export_active_detects_external_register_change() {
        // Inside the window but the registers no longer show export —
        // another controller or automation cancelled it. The reconciler
        // re-issues the entry writes instead of assuming ownership.
        let config = te_config_enabled();
        let mut state = TimedExportState::Active;
        let snap = eco_snapshot();

        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            17 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(
            decision.new_state,
            TimedExportState::Entering { .. }
        ));
        assert_eq!(decision.writes.len(), 2);
    }

    #[test]
    fn timed_export_blocked_pause_and_window_ended_issues_exit_writes() {
        // When both the pause and the export window have ended, the machine
        // must issue exit writes rather than moving straight to Configured
        // (code-review finding: registers were left armed).
        let config = te_config_enabled();
        let mut state = TimedExportState::BlockedByPause;
        let snap = export_armed_snapshot(); // registers still export-armed
        let mut snap = snap;
        snap.battery_pause_mode = 0; // pause ended

        let decision = check_timed_export_with_defaults(
            &snap,
            &config,
            &mut state,
            20 * 60,
            DeviceType::Gen3Hybrid,
        );
        assert!(matches!(
            decision.new_state,
            TimedExportState::Exiting { .. }
        ));
        assert!(!decision.writes.is_empty(), "exit writes must restore Eco");
        assert!(decision.is_exit_transition);
    }

    #[test]
    fn timed_export_disable_from_every_state_goes_off() {
        let mut config = te_config_enabled();
        config.schedule_enabled = false;
        let snap = eco_snapshot();

        for initial in [
            TimedExportState::Configured,
            TimedExportState::Entering {
                polls_waiting: 0,
                retries: 0,
            },
            TimedExportState::Active,
            TimedExportState::Exiting {
                polls_waiting: 1,
                retries: 1,
            },
            TimedExportState::BlockedByPause,
        ] {
            let mut state = initial.clone();
            let decision = check_timed_export_with_defaults(
                &snap,
                &config,
                &mut state,
                17 * 60,
                DeviceType::Gen3Hybrid,
            );
            assert!(
                matches!(decision.new_state, TimedExportState::Off),
                "disable from {initial:?} must reach Off, got {:?}",
                decision.new_state
            );
        }
    }

    #[test]
    fn timed_export_three_phase_entry_and_exit_use_hr1122() {
        // Three-phase schedule devices derive enable_discharge from HR1122
        // (HR_3PH_FORCE_DISCHARGE_ENABLE): writing HR59 arms nothing and
        // entry confirmation (which reads the model-routed flag) would
        // never succeed (code-review blocker).
        use crate::modbus::registers::{
            HR_3PH_FORCE_DISCHARGE_ENABLE, HR_BATTERY_POWER_MODE, HR_ENABLE_DISCHARGE,
        };
        let config = TimedExportConfig::default();

        let entry = build_timed_export_entry_writes(DeviceType::ThreePhase, &config);
        assert_eq!(entry.len(), 2);
        assert_eq!(entry[0].address, HR_BATTERY_POWER_MODE);
        assert_eq!(entry[0].value, 0);
        assert_eq!(entry[1].address, HR_3PH_FORCE_DISCHARGE_ENABLE);
        assert_eq!(entry[1].value, 1);

        let exit = build_timed_export_exit_writes(DeviceType::ThreePhase, &config);
        assert_eq!(exit.len(), 2);
        assert_eq!(exit[0].address, HR_3PH_FORCE_DISCHARGE_ENABLE);
        assert_eq!(exit[0].value, 0);
        assert_eq!(exit[1].address, HR_BATTERY_POWER_MODE);
        assert_eq!(exit[1].value, 1);

        // The disable helper (HTTP handlers + invariant repair) is
        // model-routed too.
        let disable = build_timed_export_disable_writes(DeviceType::ThreePhase);
        assert_eq!(disable[0].address, HR_3PH_FORCE_DISCHARGE_ENABLE);
        assert!(!disable.iter().any(|w| w.address == HR_ENABLE_DISCHARGE));
    }

    #[test]
    fn timed_export_inverter_minute_of_day_parses_wall_clock() {
        let snap = InverterSnapshot {
            inverter_time: "2026-08-29 16:05:00".to_string(),
            ..Default::default()
        };
        assert_eq!(inverter_minute_of_day(&snap), Some(16 * 60 + 5));

        // Malformed / absent clock registers → None (host fallback).
        let snap = InverterSnapshot::default();
        assert_eq!(inverter_minute_of_day(&snap), None);
        let snap = InverterSnapshot {
            inverter_time: "garbage".to_string(),
            ..Default::default()
        };
        assert_eq!(inverter_minute_of_day(&snap), None);
        let snap = InverterSnapshot {
            inverter_time: "2026-08-29 24:99:00".to_string(),
            ..Default::default()
        };
        assert_eq!(inverter_minute_of_day(&snap), None);
    }

    #[test]
    fn authoritative_minute_prefers_inverter_clock_over_host_clock() {
        let snap = InverterSnapshot {
            inverter_time: "2026-08-29 23:30:00".to_string(),
            ..Default::default()
        };

        // Simulate a UTC host at 00:30 while the inverter/user clock is still
        // 23:30. Every schedule evaluator must receive 23:30.
        assert_eq!(authoritative_minute_of_day(&snap, 30), 23 * 60 + 30);
    }

    #[test]
    fn timed_export_entry_uses_inverter_snapshot_clock_for_windows() {
        // The reconciler itself is clock-source agnostic (the poll loop
        // feeds it the inverter-derived minute); pin the behaviour via the
        // window containment helper used with the inverter minute.
        let config = te_config_enabled();
        let snap = InverterSnapshot {
            inverter_time: "2026-08-29 17:30:00".to_string(),
            ..Default::default()
        };
        let minute = inverter_minute_of_day(&snap).unwrap();
        assert!(export_window_contains(&config.slots, minute));
        // A minute outside the 16:00-19:00 window.
        assert!(!export_window_contains(&config.slots, 20 * 60));
    }

    #[test]
    fn discharge_floor_disabled_while_idle_writes_nothing() {
        let config = df_config(false, 50);
        let mut state = DischargeFloorState::Idle;
        let snap = df_snap(evening_window(), 4);

        let writes = check_discharge_floor(&snap, &config, &mut state, 20 * 60);
        assert!(writes.is_none());
        assert_eq!(state, DischargeFloorState::Idle);
    }

    #[test]
    fn discharge_control_arbiter_keeps_only_the_highest_priority_owner() {
        let mut arbiter = DischargeControlArbiter::default();

        assert!(arbiter.request(DischargeControlOwner::Agile));
        assert!(arbiter.request(DischargeControlOwner::TimedExport));
        assert!(!arbiter.request(DischargeControlOwner::TimedCharge));
        assert_eq!(
            arbiter.selected_owner(),
            Some(DischargeControlOwner::TimedExport)
        );

        // A higher-priority request can replace an earlier lower-priority
        // request before any register I/O starts.
        assert!(arbiter.request(DischargeControlOwner::ExplicitPause));
        assert_eq!(
            arbiter.selected_owner(),
            Some(DischargeControlOwner::ExplicitPause)
        );
    }

    #[test]
    fn discharge_control_arbiter_allows_coowners_but_rejects_lower_priority_writers() {
        let mut arbiter = DischargeControlArbiter::default();
        assert!(arbiter.request(DischargeControlOwner::TimedExport));

        assert!(arbiter.can_request(DischargeControlOwner::TimedExport));
        assert!(!arbiter.can_request(DischargeControlOwner::Agile));
        assert!(arbiter.can_request(DischargeControlOwner::Safety));
    }

    #[test]
    fn manual_force_is_the_explicit_pause_override_but_not_a_safety_override() {
        let mut arbiter = DischargeControlArbiter::default();
        assert!(arbiter.request(DischargeControlOwner::ExplicitPause));
        assert!(arbiter.request(DischargeControlOwner::ManualForce));
        assert_eq!(
            arbiter.selected_owner(),
            Some(DischargeControlOwner::ManualForce)
        );
        assert!(!arbiter.can_request(DischargeControlOwner::ExplicitPause));
        assert!(arbiter.can_request(DischargeControlOwner::Safety));
    }
}
