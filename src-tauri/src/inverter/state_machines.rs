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

use crate::inverter::encoder::RegisterWrite;
use crate::inverter::model::{BatteryMode, DeviceType, InverterSnapshot};
use crate::modbus::client::ModbusClient;
use crate::modbus::registers::{
    encode_hhmm, HR_BATTERY_POWER_MODE, HR_BATTERY_SOC_RESERVE, HR_CHARGE_SLOT_1_END,
    HR_CHARGE_SLOT_1_START, HR_CHARGE_TARGET_SOC, HR_ENABLE_CHARGE, HR_ENABLE_CHARGE_TARGET,
    HR_ENABLE_DISCHARGE,
};

// ===========================================================================
// Agile Octopus price types
// ===========================================================================

pub struct PriceSlot {
    pub pence: f64,
    pub valid_from: i64, // unix timestamp
    pub valid_to: i64,   // unix timestamp
}

/// Current state of the Agile Octopus state machine.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AgileState {
    #[default]
    Idle,
    Charging,
    Discharging,
}

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
    PausedFromRestart,
    /// Home load dropped below threshold, counting towards restore.
    LowLoadPending {
        /// Consecutive polls where home_power was below threshold.
        consecutive: u32,
    },
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
    let mut settings = crate::settings::Settings::load();
    if settings.cosy_active_persisted != active {
        settings.cosy_active_persisted = active;
        if let Err(e) = settings.save() {
            tracing::warn!(active, "Failed to persist cosy_active flag: {e}");
        }
    }
}

/// Persist the Agile Octopus runtime state so a crash/restart can detect
/// that the inverter was left mid-charge/discharge and re-evaluate on the
/// first poll. The in-memory `agile_state` always restarts at Idle, forcing
/// a fresh decision (and command send) regardless of the persisted value.
pub(crate) fn persist_agile_state(ag_state: AgileState) {
    let label = match ag_state {
        AgileState::Idle => "idle",
        AgileState::Charging => "charging",
        AgileState::Discharging => "discharging",
    };
    let label_str = label.to_string();
    #[cfg(not(test))]
    {
        tokio::task::spawn_blocking(move || persist_agile_state_sync(label_str));
    }
    #[cfg(test)]
    persist_agile_state_sync(label_str);
}

pub(crate) fn persist_agile_state_sync(label: String) {
    let mut settings = crate::settings::Settings::load();
    if settings.agile_state_persisted != label {
        settings.agile_state_persisted = label.clone();
        if let Err(e) = settings.save() {
            tracing::warn!(state = &label, "Failed to persist agile_state: {e}");
        }
    }
}

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


/// Check load discharge limiter and return register writes if the state
/// machine transitions to Paused or back to Idle.
///
/// Returns `Some(writes)` when a transition requires register writes,
/// `None` otherwise.
pub(crate) fn check_load_limiter(
    snap: &InverterSnapshot,
    config: &LoadLimiterConfig,
    state: &mut LoadLimiterState,
    poll_interval_secs: u64,
) -> Option<Vec<RegisterWrite>> {
    if !config.enabled {
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
        if matches!(*state, LoadLimiterState::Paused)
            || matches!(*state, LoadLimiterState::PausedFromRestart)
            || matches!(*state, LoadLimiterState::LowLoadPending { .. })
        {
            tracing::info!(
                mode = ?snap.battery_mode,
                "Load limiter: battery mode changed externally, returning to Idle"
            );
            *state = LoadLimiterState::Idle;
        }
        return None;
    }

    // Don't interfere with other automated modes.
    if snap.auto_winter_active || snap.cosy_active || snap.agile_active {
        return None;
    }

    // Check activation window.
    let now = chrono::Local::now();
    let now_minutes = now.hour() as u16 * 60 + now.minute() as u16;
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
        // Outside window - if we're Paused, restore Eco.
        if matches!(*state, LoadLimiterState::Paused)
            || matches!(*state, LoadLimiterState::PausedFromRestart)
        {
            tracing::info!("Load limiter: outside activation window, restoring Eco");
            *state = LoadLimiterState::Idle;
            return Some(vec![
                RegisterWrite {
                    address: HR_BATTERY_POWER_MODE,
                    value: 1, // self-consumption
                },
                RegisterWrite {
                    address: HR_ENABLE_DISCHARGE,
                    value: 0,
                },
                RegisterWrite {
                    address: HR_BATTERY_SOC_RESERVE,
                    value: 4, // default reserve
                },
            ]);
        }
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
                    return Some(vec![
                        RegisterWrite {
                            address: HR_BATTERY_POWER_MODE,
                            value: 1, // self-consumption
                        },
                        RegisterWrite {
                            address: HR_ENABLE_DISCHARGE,
                            value: 0,
                        },
                        RegisterWrite {
                            address: HR_BATTERY_SOC_RESERVE,
                            value: 100, // Eco Paused = reserve 100%
                        },
                    ]);
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
        // the app was down. If the load is already below threshold,
        // restore Eco immediately. If still high, transition to normal Paused.
        LoadLimiterState::PausedFromRestart => {
            if home_power <= threshold {
                tracing::info!(
                    "Load limiter: post-crash - load below threshold, restoring Eco immediately"
                );
                *state = LoadLimiterState::Idle;
                return Some(vec![
                    RegisterWrite {
                        address: HR_BATTERY_POWER_MODE,
                        value: 1,
                    },
                    RegisterWrite {
                        address: HR_ENABLE_DISCHARGE,
                        value: 0,
                    },
                    RegisterWrite {
                        address: HR_BATTERY_SOC_RESERVE,
                        value: 4,
                    },
                ]);
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
                        consecutive,
                        "Load limiter: restoring Eco mode - load below threshold for full delay"
                    );
                    *state = LoadLimiterState::Idle;
                    return Some(vec![
                        RegisterWrite {
                            address: HR_BATTERY_POWER_MODE,
                            value: 1, // self-consumption
                        },
                        RegisterWrite {
                            address: HR_ENABLE_DISCHARGE,
                            value: 0,
                        },
                        RegisterWrite {
                            address: HR_BATTERY_SOC_RESERVE,
                            value: 4, // default reserve
                        },
                    ]);
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
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::InverterSnapshot;

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
        assert!(matches!(state, AutoWinterState::ColdPending { consecutive: 1 }));
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
        assert!(matches!(state, AutoWinterState::ColdPending { consecutive: 2 }));

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
        assert!(writes.iter().any(|w| w.address == HR_ENABLE_CHARGE_TARGET && w.value == 1));
        assert!(writes.iter().any(|w| w.address == HR_CHARGE_TARGET_SOC && w.value == 90));
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
        assert!(matches!(state, AutoWinterState::WarmPending { consecutive: 1 }));

        // Second warm reading: restore.
        let writes = check_auto_winter(&snap, &config, &mut state, &mut saved).expect("restores");
        assert_eq!(state, AutoWinterState::Idle);
        assert!(saved.is_none(), "saved consumed on restore");
        // Restores the saved target SOC (77) + enable (1).
        assert!(writes.iter().any(|w| w.address == HR_CHARGE_TARGET_SOC && w.value == 77));
        assert!(writes.iter().any(|w| w.address == HR_ENABLE_CHARGE_TARGET && w.value == 1));
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

        assert_eq!(saved, Some(restored), "restored saved values must survive activation");
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
    fn load_limiter_disabled_resets_state_and_writes_nothing() {
        let snap = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 999_999,
            ..Default::default()
        };
        let config = LoadLimiterConfig {
            enabled: false,
            ..Default::default()
        };
        let mut state = LoadLimiterState::Paused;

        let writes = check_load_limiter(&snap, &config, &mut state, 60);

        assert!(writes.is_none());
        assert_eq!(state, LoadLimiterState::Idle);
    }

    #[test]
    fn load_limiter_ignores_non_eco_mode_and_yields_to_other_automation() {
        let config = ll_config(3000, 5);
        let mut state = LoadLimiterState::Idle;

        // Not Eco → no action, returns to Idle.
        let snap = InverterSnapshot {
            battery_mode: BatteryMode::TimedExport,
            home_power: 9999,
            ..Default::default()
        };
        assert!(check_load_limiter(&snap, &config, &mut state, 60).is_none());
        assert_eq!(state, LoadLimiterState::Idle);

        // Eco but another automation active → no action.
        let snap = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 9999,
            auto_winter_active: true,
            ..Default::default()
        };
        assert!(check_load_limiter(&snap, &config, &mut state, 60).is_none());
        assert_eq!(state, LoadLimiterState::Idle);
    }

    #[test]
    fn load_limiter_counts_high_load_then_pauses() {
        let config = ll_config(3000, 5);
        let mut state = LoadLimiterState::Idle;
        // 5-minute delay at a 1-minute poll => debounce_count = 5.
        let high = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 4000,
            ..Default::default()
        };

        // First 4 high-load polls: pending, no writes.
        for _ in 0..4 {
            assert!(check_load_limiter(&high, &config, &mut state, 60).is_none());
            assert!(matches!(
                state,
                LoadLimiterState::HighLoadPending { .. }
            ));
        }
        // 5th: transition to Paused with restore-100 writes.
        let writes = check_load_limiter(&high, &config, &mut state, 60).expect("pauses");
        assert_eq!(state, LoadLimiterState::Paused);
        assert!(writes.iter().any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 100));
        assert!(writes.iter().any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0));
    }

    #[test]
    fn load_limiter_restores_eco_when_load_drops_for_full_delay() {
        let config = ll_config(3000, 3);
        let mut state = LoadLimiterState::Paused;
        let low = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 1000,
            ..Default::default()
        };

        // First two low-load polls while Paused: LowLoadPending, no writes.
        for _ in 0..2 {
            assert!(check_load_limiter(&low, &config, &mut state, 60).is_none());
            assert!(matches!(state, LoadLimiterState::LowLoadPending { .. }));
        }
        // 3rd: restore Eco (reserve back to 4).
        let writes = check_load_limiter(&low, &config, &mut state, 60).expect("restores");
        assert_eq!(state, LoadLimiterState::Idle);
        assert!(writes.iter().any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 4));
        assert!(writes.iter().any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1));
    }

    #[test]
    fn load_limiter_post_crash_restores_immediately_if_load_already_low() {
        let config = ll_config(3000, 10);
        let mut state = LoadLimiterState::PausedFromRestart;
        let low = InverterSnapshot {
            battery_mode: BatteryMode::Eco,
            home_power: 500,
            ..Default::default()
        };

        let writes = check_load_limiter(&low, &config, &mut state, 60).expect("immediate restore");
        assert_eq!(state, LoadLimiterState::Idle);
        assert!(writes.iter().any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 4));
    }
}
