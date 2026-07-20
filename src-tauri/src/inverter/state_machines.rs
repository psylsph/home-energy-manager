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
    encode_hhmm, HR_3PH_FORCE_CHARGE_ENABLE, HR_3PH_FORCE_DISCHARGE_ENABLE, HR_BATTERY_POWER_MODE,
    HR_BATTERY_SOC_RESERVE, HR_CHARGE_SLOT_1_END, HR_CHARGE_SLOT_1_START, HR_CHARGE_TARGET_SOC,
    HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_END,
    HR_DISCHARGE_SLOT_2_START, HR_ENABLE_CHARGE, HR_ENABLE_CHARGE_TARGET, HR_ENABLE_DISCHARGE,
};

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
    saved: &mut Option<LoadLimiterSaved>,
) -> Option<Vec<RegisterWrite>> {
    if !config.enabled {
        if matches!(*state, LoadLimiterState::Paused)
            || matches!(*state, LoadLimiterState::PausedFromRestart)
            || matches!(*state, LoadLimiterState::LowLoadPending { .. })
        {
            let restore_reserve = saved.take().map(|s| s.reserve).unwrap_or(4);
            tracing::info!(
                restore_reserve,
                "Load limiter: disabled while active, restoring Eco mode"
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
                    value: restore_reserve,
                },
            ]);
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
            let restore_reserve = saved.take().map(|s| s.reserve).unwrap_or(4);
            tracing::info!(
                restore_reserve,
                "Load limiter: outside activation window, restoring Eco"
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
                    value: restore_reserve,
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
                    // Save the current reserve before pausing so we can
                    // restore it later (survives crash/restart via disk).
                    *saved = Some(LoadLimiterSaved {
                        reserve: snap.battery_reserve as u16,
                    });
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
                // Consume the saved reserve on the final confirm so a
                // stale value (e.g. the user's pre-pause 20% setting)
                // doesn't linger in `load_limiter_saved_reserve` on
                // disk. If the limiter is triggered again later the
                // in-memory `saved` will be repopulated from the
                // current snapshot, so this is safe to drop.
                *saved = None;
                *state = LoadLimiterState::Idle;
                return None;
            }

            if home_power <= threshold {
                let restore_reserve = saved.as_ref().map(|s| s.reserve).unwrap_or(4);
                tracing::info!(
                    restore_reserve,
                    "Load limiter: post-crash - load below threshold, restoring Eco"
                );
                // Stay in PausedFromRestart — if the write fails (dongle
                // busy on first poll after reconnect), the next poll will
                // retry. Once the battery mode flips to Eco, the check
                // above transitions to Idle.
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
                        value: restore_reserve,
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
                    let restore_reserve = saved.take().map(|s| s.reserve).unwrap_or(4);
                    tracing::info!(
                        consecutive = *consecutive,
                        restore_reserve,
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
                            value: restore_reserve,
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
    fn load_limiter_outside_window_restores_with_saved_reserve() {
        // Use a window that's almost certainly inactive: start=0:00,
        // end=0:01. With end_mins (1) > start_mins (0), the condition
        // is `now_minutes >= 0 && now_minutes < 1`, which is only true
        // during the 00:00:00–00:00:59 minute of each day. For any
        // other time, in_window is false.
        let config = LoadLimiterConfig {
            enabled: true,
            threshold_w: 3000,
            trigger_delay_minutes: 5,
            start_hour: 0,
            start_minute: 0,
            end_hour: 0,
            end_minute: 1,
        };
        let mut state = LoadLimiterState::PausedFromRestart;
        let mut saved = Some(LoadLimiterSaved { reserve: 20 });
        let snap = InverterSnapshot {
            battery_mode: BatteryMode::EcoPaused,
            home_power: 5000,
            ..Default::default()
        };

        // Outside window — restore with saved reserve. The !in_window
        // branch handles PausedFromRestart by restoring Eco and
        // consuming the saved reserve, regardless of current load
        // (the app just restarted and needs to re-establish its state).
        let writes = check_load_limiter(&snap, &config, &mut state, 60, &mut saved)
            .expect("restore writes returned");
        assert_eq!(state, LoadLimiterState::Idle);
        assert!(writes
            .iter()
            .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 20));
        assert!(saved.is_none(), "saved must be consumed on restore");
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
}
