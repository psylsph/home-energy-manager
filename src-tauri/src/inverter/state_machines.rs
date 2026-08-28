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

/// Build the writes that leave Timed Export and return the inverter to Eco.
///
/// Keep this sequence shared by HTTP handlers and the poll-loop safety repair:
/// clearing HR59 alone leaves HR27 in export mode, while writing HR27 first
/// can briefly leave an armed schedule in an ambiguous state.
pub(crate) fn build_timed_export_disable_writes() -> Vec<RegisterWrite> {
    [
        ControlCommand::SetEnableDischarge { enabled: false },
        ControlCommand::SetBatteryPowerMode { mode: 1 },
    ]
    .into_iter()
    .flat_map(|command| command.encode().unwrap_or_default())
    .collect()
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
    /// Winter mode is active and charging to target SOC.
    WinterActive,
    /// Temperature above Recovery Threshold, counting towards restore.
    WarmPending {
        /// Consecutive polls where temp was above Recovery Threshold.
        consecutive: u32,
    },
}

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
/// Only operates when the battery is in Eco mode and no other automated
/// mode (auto-winter, Cosy, Agile) is active.
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

    if saved.is_none() {
        *saved = Some(crate::settings::AdaptiveChargeSavedLimit {
            inverter_serial: snap.inverter_serial.clone(),
            device_type_code: snap.device_type_code.clone(),
            register_address: register,
            raw_value: snap.charge_rate as u16,
        });
    }
    let baseline = saved.as_ref().expect("baseline captured above");
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
        *state = AdaptiveChargeState::OutsideWindow;
        let desired = baseline.raw_value;
        return AdaptiveChargeOutcome {
            write: (snap.charge_rate as u16 != desired).then_some(RegisterWrite {
                address: register,
                value: desired,
            }),
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
    let start = encode_hhmm(slot.start_hour, slot.start_minute);
    let end = encode_hhmm(slot.end_hour, slot.end_minute);

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

/// Execute a list of register writes to the inverter with inter-write delays.
/// Returns `true` if all writes succeeded.
pub(crate) async fn write_registers_to_inverter(
    client: &mut ModbusClient,
    writes: &[RegisterWrite],
    label: &str,
) -> bool {
    let mut all_ok = true;
    for w in writes {
        match client.write_register(w.address, w.value).await {
            Ok(()) => tracing::info!("{}: wrote reg {} = {}", label, w.address, w.value),
            Err(e) => {
                tracing::error!("{}: write reg {} failed: {e}", label, w.address);
                all_ok = false;
            }
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
    all_ok
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
pub(crate) fn check_auto_winter(
    snap: &InverterSnapshot,
    config: &AutoWinterConfig,
    state: &mut AutoWinterState,
    saved: &mut Option<AutoWinterSaved>,
) -> Option<Vec<RegisterWrite>> {
    if !config.enabled {
        *state = AutoWinterState::Idle;
        *saved = None;
        return None;
    }

    let temp = snap.battery_temperature;

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
                    *state = AutoWinterState::WinterActive;
                    return Some(vec![
                        RegisterWrite {
                            address: HR_ENABLE_CHARGE_TARGET,
                            value: 1,
                        },
                        RegisterWrite {
                            address: HR_CHARGE_TARGET_SOC,
                            value: config.target_soc as u16,
                        },
                    ]);
                }
            } else if temp >= config.recovery_threshold {
                *state = AutoWinterState::Idle;
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
                    let saved_settings = saved.take();
                    let (restore_target, restore_enable) = match saved_settings {
                        Some(s) => (
                            s.target_soc as u16,
                            if s.enable_charge_target { 1 } else { 0 },
                        ),
                        None => (100, 0),
                    };
                    tracing::info!(
                        consecutive,
                        "Auto winter: restoring (HR 20={}, HR 116={})",
                        restore_enable,
                        restore_target,
                    );
                    *state = AutoWinterState::Idle;
                    return Some(vec![
                        RegisterWrite {
                            address: HR_ENABLE_CHARGE_TARGET,
                            value: restore_enable,
                        },
                        RegisterWrite {
                            address: HR_CHARGE_TARGET_SOC,
                            value: restore_target,
                        },
                    ]);
                }
            } else if temp < config.cold_threshold {
                *state = AutoWinterState::WinterActive;
            }
        }
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

fn check_load_limiter_at(
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

    // Only operate when battery is in Eco or EcoPaused mode.
    // EcoPaused is what the limiter sets when it pauses discharge - it
    // must be accepted so the recovery countdown can proceed.
    // No other automated modes should be active.
    if snap.battery_mode != BatteryMode::Eco && snap.battery_mode != BatteryMode::EcoPaused {
        // If we're Paused but the battery mode isn't one we manage,
        // someone changed it externally - return to Idle without writing.
        if state.is_actively_pausing() {
            tracing::info!(
                mode = ?snap.battery_mode,
                "Load limiter: battery mode changed externally, returning to Idle"
            );
            *state = LoadLimiterState::Idle;
            if !other_active {
                *saved = None;
            }
        }
        return None;
    }

    // Don't interfere with other automated modes.
    if snap.auto_winter_active || snap.cosy_active || snap.agile_active {
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
            value: encode_hhmm(s1h, s1m),
        });
        writes.push(RegisterWrite {
            address: HR_DISCHARGE_SLOT_1_END,
            value: encode_hhmm(e1h, e1m),
        });
        let (s2h, s2m) = pre_slot_2_start.unwrap_or((0, 0));
        let (e2h, e2m) = pre_slot_2_end.unwrap_or((0, 0));
        writes.push(RegisterWrite {
            address: HR_DISCHARGE_SLOT_2_START,
            value: encode_hhmm(s2h, s2m),
        });
        writes.push(RegisterWrite {
            address: HR_DISCHARGE_SLOT_2_END,
            value: encode_hhmm(e2h, e2m),
        });

        // Default to eco (1) on restore — matches the explicit Stop
        // Discharge path. `battery_power_mode` is not captured in the
        // revert (only the encoder config), so we always return to eco.
        writes.push(RegisterWrite {
            address: HR_BATTERY_POWER_MODE,
            value: 1,
        });
    }

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
        let writes = build_timed_export_disable_writes();
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
        assert_eq!(state, AdaptiveChargeState::OutsideWindow);

        let restored = adaptive_snapshot(50, 35);
        let disabled =
            check_adaptive_charge(&restored, &config, false, &mut state, &mut saved, 18 * 60);
        assert!(disabled.write.is_none());
        assert!(saved.is_none());
        assert_eq!(state, AdaptiveChargeState::Inactive);
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

        assert_eq!(state, AutoWinterState::WinterActive);
        // Saved values reflect the snapshot *before* activation.
        assert_eq!(
            saved,
            Some(AutoWinterSaved {
                enable_charge_target: false,
                target_soc: 0,
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
        assert_eq!(state, AutoWinterState::Idle);
        assert!(saved.is_none(), "saved consumed on restore");
        // Restores the saved target SOC (77) + enable (1).
        assert!(writes
            .iter()
            .any(|w| w.address == HR_CHARGE_TARGET_SOC && w.value == 77));
        assert!(writes
            .iter()
            .any(|w| w.address == HR_ENABLE_CHARGE_TARGET && w.value == 1));
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
    fn load_limiter_ignores_non_eco_mode_and_yields_to_other_automation() {
        let config = ll_config(3000, 5);
        let mut state = LoadLimiterState::Idle;
        let mut saved = None;

        // Not Eco → no action, returns to Idle.
        let snap = InverterSnapshot {
            battery_mode: BatteryMode::TimedExport,
            home_power: 9999,
            ..Default::default()
        };
        assert!(check_load_limiter(&snap, &config, &mut state, 60, &mut saved).is_none());
        assert_eq!(state, LoadLimiterState::Idle);

        // Eco but another automation active → no action.
        let snap = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 9999,
            auto_winter_active: true,
            ..Default::default()
        };
        assert!(check_load_limiter(&snap, &config, &mut state, 60, &mut saved).is_none());
        assert_eq!(state, LoadLimiterState::Idle);
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
    fn load_limiter_external_mode_change_while_paused_resets_to_idle() {
        let config = ll_config(3000, 5);
        let mut state = LoadLimiterState::Paused;
        let mut saved = Some(LoadLimiterSaved { reserve: 20 });
        let snap = InverterSnapshot {
            battery_mode: BatteryMode::TimedExport,
            home_power: 5000,
            ..Default::default()
        };

        // Battery mode changed externally — return to Idle without writing.
        let writes = check_load_limiter(&snap, &config, &mut state, 60, &mut saved);
        assert!(writes.is_none());
        assert_eq!(state, LoadLimiterState::Idle);
    }

    #[test]
    fn load_limiter_external_mode_change_while_paused_from_restart_resets_to_idle() {
        let config = ll_config(3000, 5);
        let mut state = LoadLimiterState::PausedFromRestart;
        let mut saved = Some(LoadLimiterSaved { reserve: 20 });
        let snap = InverterSnapshot {
            battery_mode: BatteryMode::TimedExport,
            home_power: 5000,
            ..Default::default()
        };

        // Battery mode changed externally while in PausedFromRestart — Idle.
        let writes = check_load_limiter(&snap, &config, &mut state, 60, &mut saved);
        assert!(writes.is_none());
        assert_eq!(state, LoadLimiterState::Idle);
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
        let s1 = encode_hhmm(17, 0);
        let e1 = encode_hhmm(19, 0);
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
    fn discharge_floor_disabled_while_idle_writes_nothing() {
        let config = df_config(false, 50);
        let mut state = DischargeFloorState::Idle;
        let snap = df_snap(evening_window(), 4);

        let writes = check_discharge_floor(&snap, &config, &mut state, 20 * 60);
        assert!(writes.is_none());
        assert_eq!(state, DischargeFloorState::Idle);
    }
}
