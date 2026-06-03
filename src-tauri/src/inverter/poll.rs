//! Periodic inverter polling loop.
//!
//! Drives the timed read cycle that queries all relevant input
//! registers and publishes updated state to subscribers via
//! the WebSocket broadcast channel.
//!
//! ## Architecture
//!
//! The [`AppState`] struct is the central shared object. It holds:
//! - The latest [`InverterSnapshot`] behind an `Arc<Mutex<…>>`
//! - The current [`ConnectionState`]
//! - A [`broadcast::Sender`] that pushes snapshot and connection-state
//!   updates to all active WebSocket clients
//! - Mutable [`PollSettings`] (host, port, serial, interval)
//!
//! [`run_poll_loop`] is the main async entry point, intended to be
//! spawned as a long-lived Tokio task. It handles auto-reconnection
//! with exponential back-off.

use std::sync::Arc;
use std::time::Duration;

use chrono::Timelike;

use tokio::sync::{broadcast, Mutex, Notify};
use crate::server::logs::LogRing;
use crate::server::ws::ConnectedClients;

use crate::history::HistoryDb;
use crate::inverter::decoder::decode_snapshot;
use crate::inverter::encoder::{ControlCommand, RegisterWrite};
use crate::inverter::model::{BatteryMode, InverterSnapshot};
use crate::modbus::client::ModbusClient;
use crate::modbus::registers::{HR_CHARGE_TARGET_SOC, HR_ENABLE_CHARGE_TARGET};

// ---------------------------------------------------------------------------
// Connection state
// ---------------------------------------------------------------------------

/// Connection state for UI display.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    /// Successfully connected to the inverter and actively polling.
    Connected,
    /// Connection was lost; automatic reconnection is in progress.
    Reconnecting,
    /// No connection (initial state or explicit disconnect).
    Disconnected,
}

// ---------------------------------------------------------------------------
// Broadcast message
// ---------------------------------------------------------------------------

/// Message broadcast to WebSocket clients.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum PollMessage {
    /// A fresh snapshot has been decoded from the inverter registers.
    Snapshot(InverterSnapshot),
    /// The connection state has changed.
    Connection {
        /// New connection state.
        state: ConnectionState,
        /// Host we are connected to (or trying to reach).
        host: String,
    },
}

// ---------------------------------------------------------------------------
// Poll settings
// ---------------------------------------------------------------------------

/// Configurable parameters that control the polling loop behaviour.
#[derive(Debug, Clone)]
pub struct PollSettings {
    /// Hostname or IP address of the GivEnergy data adapter.
    pub host: String,
    /// TCP port (typically 8899).
    pub port: u16,
    /// Data adapter serial number.
    pub serial: String,
    /// Seconds between successive poll cycles.
    pub interval_secs: u64,
    /// Monotonically increasing version — bumped by the settings API
    /// so the poll loop can detect that a reconnect is needed.
    pub version: u32,
}

impl Default for PollSettings {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 8899,
            serial: String::new(),
            interval_secs: 60,
            version: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Shared application state
// ---------------------------------------------------------------------------

/// State machine for temperature-triggered auto winter mode.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
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
    /// Master toggle — must be on for automatic winter mode to function.
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
#[derive(Debug, Clone)]
pub struct AutoWinterSaved {
    pub enable_charge_target: bool,
    pub target_soc: u8,
}

/// Shared state accessible from HTTP handlers, the WebSocket endpoint, etc.
pub struct AppState {
    /// Most recently decoded snapshot (or `None` if never polled).
    pub latest_snapshot: Arc<Mutex<Option<InverterSnapshot>>>,
    /// Current connection state (read by the status endpoint).
    pub connection_state: Arc<Mutex<ConnectionState>>,
    /// Broadcast sender — every poll cycle sends a [`PollMessage::Snapshot`]
    /// and connection-state changes send [`PollMessage::Connection`].
    pub tx: broadcast::Sender<PollMessage>,
    /// Runtime configuration (host, serial, interval, etc.).
    pub settings: Arc<Mutex<PollSettings>>,
    /// Pending register writes queued by the control API.
    /// The poll loop drains this queue and writes to the inverter.
    pub pending_writes: Arc<Mutex<Vec<Vec<RegisterWrite>>>>,
    /// Signaled when new writes are queued so the poll loop wakes immediately.
    pub write_notify: Arc<Notify>,
    /// SQLite history database (set after startup).
    pub history: Arc<Mutex<Option<Arc<HistoryDb>>>>,
    /// Ring buffer of recent log lines for the developer console.
    pub log_ring: Arc<LogRing>,
    /// Connected WebSocket clients (for Network Access display).
    pub connected_clients: Arc<parking_lot::Mutex<ConnectedClients>>,
    /// Auto winter mode configuration (volatile, can be synced to settings).
    pub auto_winter_config: Arc<Mutex<AutoWinterConfig>>,
    /// Auto winter mode state machine.
    pub auto_winter_state: Arc<Mutex<AutoWinterState>>,
    /// Saved register values to restore when winter mode deactivates.
    pub auto_winter_saved: Arc<Mutex<Option<AutoWinterSaved>>>,
    /// Whether cosy charging is currently active (force-charging in a slot).
    pub cosy_active: Arc<Mutex<bool>>,
}

impl AppState {
    /// Create a new `AppState` with sensible defaults.
    ///
    /// The broadcast channel is sized for 32 lagging consumers. Receivers
    /// can be obtained with `state.tx.subscribe()`.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(32);
        Self {
            latest_snapshot: Arc::new(Mutex::new(None)),
            connection_state: Arc::new(Mutex::new(ConnectionState::Disconnected)),
            tx,
            settings: Arc::new(Mutex::new(PollSettings::default())),
            pending_writes: Arc::new(Mutex::new(Vec::new())),
            write_notify: Arc::new(Notify::new()),
            history: Arc::new(Mutex::new(None)),
            log_ring: Arc::new(crate::server::logs::LogRing::new(2000)),
            connected_clients: Arc::new(parking_lot::Mutex::new(ConnectedClients::new())),
            auto_winter_config: Arc::new(Mutex::new(AutoWinterConfig::default())),
            auto_winter_state: Arc::new(Mutex::new(AutoWinterState::default())),
            auto_winter_saved: Arc::new(Mutex::new(None)),
            cosy_active: Arc::new(Mutex::new(false)),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    /// Create `AppState` with an externally-created log ring
    /// (used when the tracing capture layer needs the ring before
    /// the state is constructed).
    pub fn with_log_ring(log_ring: Arc<crate::server::logs::LogRing>) -> Self {
        let (tx, _) = broadcast::channel(32);
        Self {
            latest_snapshot: Arc::new(Mutex::new(None)),
            connection_state: Arc::new(Mutex::new(ConnectionState::Disconnected)),
            tx,
            settings: Arc::new(Mutex::new(PollSettings::default())),
            pending_writes: Arc::new(Mutex::new(Vec::new())),
            write_notify: Arc::new(Notify::new()),
            history: Arc::new(Mutex::new(None)),
            log_ring,
            connected_clients: Arc::new(parking_lot::Mutex::new(ConnectedClients::new())),
            auto_winter_config: Arc::new(Mutex::new(AutoWinterConfig::default())),
            auto_winter_state: Arc::new(Mutex::new(AutoWinterState::default())),
            auto_winter_saved: Arc::new(Mutex::new(None)),
            cosy_active: Arc::new(Mutex::new(false)),
        }
    }
}

// ---------------------------------------------------------------------------
// Poll loop
// ---------------------------------------------------------------------------

/// Runs the polling loop indefinitely (spawn as a Tokio task).
///
/// ## Behaviour
///
/// 1. If `settings.host` is empty, sleep 5 s and retry.
/// 2. Attempt to connect. On success, broadcast `Connected` and enter the
///    inner poll loop.
/// 3. On each tick: call `read_all_standard`, decode into an
///    [`InverterSnapshot`], store it, and broadcast it.
/// 4. If a poll or I/O error occurs, break out of the inner loop,
///    disconnect, broadcast `Reconnecting`, and attempt reconnection
///    with exponential back-off (5 s → 60 s cap).
///
/// If `settings.serial` is empty the dongle serial is auto-discovered from
/// the first response frame header and persisted to settings — only the host
/// IP is required to connect.
///
/// Sanitize a snapshot against physically impossible register values.
/// Compares against the previous snapshot to detect and correct garbled
/// readings before they reach the frontend or history database.
/// Returns `true` if any field was sanitized (fallback applied).
/// Carry forward battery module data from the previous snapshot when the current
/// poll cycle failed to read one or more BMS modules (Modbus read error or corruption).
///
/// Without this, a transient BMS read failure causes the frontend to show empty or
/// missing module panels, which is jarring. Instead, we keep the last known-good data
/// until a fresh read succeeds.
fn carry_forward_battery_modules_with(snap: &mut InverterSnapshot, prev_modules: Option<&[super::model::BatteryModule]>) {
    if let Some(prev) = prev_modules {
        if !prev.is_empty() {
            // If NO modules were read this cycle, carry forward all previous modules.
            if snap.battery_modules.is_empty() {
                tracing::debug!(
                    count = prev.len(),
                    "Battery modules empty this cycle — carrying forward from previous"
                );
                snap.battery_modules = prev.to_vec();
                return;
            }
            // If we got fewer modules than before, fill in the gaps by index.
            // Modules are identified by their `index` field (0-based).
            let max_index = snap.battery_modules.iter().map(|m| m.index).max().unwrap_or(0);
            let prev_max = prev.iter().map(|m| m.index).max().unwrap_or(0);
            if prev_max > max_index {
                let present: std::collections::HashSet<usize> =
                    snap.battery_modules.iter().map(|m| m.index).collect();
                for prev_mod in prev {
                    if !present.contains(&prev_mod.index) {
                        tracing::debug!(
                            index = prev_mod.index,
                            "Battery module missing this cycle — carrying forward"
                        );
                        snap.battery_modules.push(prev_mod.clone());
                    }
                }
                // Re-sort by index for consistent ordering
                snap.battery_modules.sort_by_key(|m| m.index);
            }
        }
    }
}

/// Returns `true` if any field was sanitized (fallback applied).
///
/// `pending_mode` tracks a mode that differs from the previous reading but
/// hasn't yet been confirmed by a second consecutive reading. This prevents
/// mode flicker caused by a single corrupt register read.
fn sanitize_snapshot(
    snap: &mut InverterSnapshot,
    prev: Option<&InverterSnapshot>,
    skip_delta: bool,
    pending_mode: &mut Option<BatteryMode>,
) -> bool {
    let mut sanitized = false;
    let max_battery_power: i32 = 10_000; // 10 kW — residential battery limit

    // Battery power: reject impossible spikes (>10 kW)
    if snap.battery_power.abs() > max_battery_power {
        if let Some(p) = prev {
            tracing::warn!(
                raw = snap.battery_power,
                prev = p.battery_power,
                "Battery power out of range — using previous value"
            );
            snap.battery_power = p.battery_power;
        } else {
            snap.battery_power = 0;
        }
        sanitized = true;
    }

    // SOC: if 0 but power is flowing, clearly a garbled register
    if snap.soc == 0 && (snap.solar_power > 0 || snap.battery_power != 0 || snap.grid_power != 0) {
        if let Some(p) = prev {
            tracing::warn!(prev_soc = p.soc, "SOC=0 with live power — using previous SOC");
            snap.soc = p.soc;
            sanitized = true;
        }
    }

    // SOC: if 100 but battery is actively charging at high power, impossible
    if snap.soc == 100 && snap.battery_power > 500 {
        if let Some(p) = prev {
            tracing::warn!(prev_soc = p.soc, "SOC=100 while charging >500W — using previous SOC");
            snap.soc = p.soc;
            sanitized = true;
        }
    }

    // Inverter temperature: reject physically impossible values.
    // A heatsink >100°C means hardware damage is imminent; anything above
    // 80°C is unusual. Raw register corruption can produce values like 239°C.
    if snap.inverter_temperature > 100.0 || snap.inverter_temperature < -20.0 {
        if let Some(p) = prev {
            tracing::warn!(
                raw = snap.inverter_temperature,
                prev = p.inverter_temperature,
                "Inverter temperature out of range — using previous"
            );
            snap.inverter_temperature = p.inverter_temperature;
        } else {
            snap.inverter_temperature = 0.0;
        }
        sanitized = true;
    }

    // Battery temperature: reject physically impossible values.
    // Lithium batteries operate in -20°C to 60°C range; anything above
    // 80°C is a safety concern and almost certainly corrupt data.
    if snap.battery_temperature > 80.0 || snap.battery_temperature < -20.0 {
        if let Some(p) = prev {
            tracing::warn!(
                raw = snap.battery_temperature,
                prev = p.battery_temperature,
                "Battery temperature out of range — using previous"
            );
            snap.battery_temperature = p.battery_temperature;
        } else {
            snap.battery_temperature = 0.0;
        }
        sanitized = true;
    }

    // Grid power: reject impossible values (>10 kW for a typical UK single-phase supply)
    let max_grid_power: i32 = 10_000;
    if snap.grid_power.abs() > max_grid_power {
        if let Some(p) = prev {
            tracing::warn!(raw = snap.grid_power, prev = p.grid_power, "Grid power out of range — using previous");
            snap.grid_power = p.grid_power;
        } else {
            snap.grid_power = 0;
        }
        sanitized = true;
    }

    // Grid voltage: UK single-phase is nominally 230V ±10% (207–253V).
    // Anything outside 180–280V is clearly corrupt register data.
    if snap.grid_voltage < 180.0 || snap.grid_voltage > 280.0 {
        if let Some(p) = prev {
            tracing::warn!(
                raw = snap.grid_voltage, prev = p.grid_voltage,
                "Grid voltage out of range — using previous"
            );
            snap.grid_voltage = p.grid_voltage;
        } else {
            snap.grid_voltage = 230.0; // nominal
        }
        sanitized = true;
    }

    // Grid frequency: UK is nominally 50 Hz ±1% (49.5–50.5 Hz).
    // Anything outside 45–55 Hz is clearly corrupt.
    if snap.grid_frequency < 45.0 || snap.grid_frequency > 55.0 {
        if let Some(p) = prev {
            tracing::warn!(
                raw = snap.grid_frequency, prev = p.grid_frequency,
                "Grid frequency out of range — using previous"
            );
            snap.grid_frequency = p.grid_frequency;
        } else {
            snap.grid_frequency = 50.0; // nominal
        }
        sanitized = true;
    }

    // Solar power: reject impossible values (>10 kW residential)
    let max_solar_power: i32 = 10_000;
    if snap.solar_power > max_solar_power {
        if let Some(p) = prev {
            tracing::warn!(raw = snap.solar_power, prev = p.solar_power, "Solar power out of range — using previous");
            snap.solar_power = p.solar_power;
        } else {
            snap.solar_power = 0;
        }
        sanitized = true;
    }

    // Battery module voltage: reject impossible values.
    // LV packs run ~48-57V, HV packs up to ~345V. Anything above 500V is
    // clearly a register glitch (e.g. 30,000V from corrupt BMS uint32).
    for module in &mut snap.battery_modules {
        if module.voltage > 500.0 || module.voltage < 0.0 {
            if let Some(p) = prev {
                if let Some(prev_mod) = p.battery_modules.get(module.index) {
                    tracing::warn!(
                        raw = module.voltage,
                        prev = prev_mod.voltage,
                        "Battery module {} voltage out of range — using previous",
                        module.index
                    );
                    module.voltage = prev_mod.voltage;
                } else {
                    module.voltage = 0.0;
                }
            } else {
                module.voltage = 0.0;
            }
            sanitized = true;
        }
    }

    // Home power: reject impossible values.
    // Typical UK home peak is ~10 kW; even with EV charging rarely exceeds 15 kW.
    // Also reject negative home power (can't have negative consumption).
    let max_home_power: i32 = 15_000;
    if snap.home_power.abs() > max_home_power || snap.home_power < 0 {
        if let Some(p) = prev {
            tracing::warn!(raw = snap.home_power, prev = p.home_power, "Home power out of range — using previous");
            snap.home_power = p.home_power;
        } else {
            snap.home_power = 0;
        }
        sanitized = true;
    }

    // Daily energy totals (`today_*_kwh`): cumulative kWh counters that
    // monotonically increase from 0 and reset to 0 at midnight.
    //
    // Sanitization rules (applied ALWAYS, even on first reading):
    //   0. Value must be in [0, 100] kWh — a residential system can't
    //      consume/generate more than 100 kWh in a single day.
    //      This catches the common corruption patterns (245, 275, 311, 1010).
    //
    // Additional rules (only when previous reading exists):
    //   1. Counter must NOT decrease during the day (register corruption)
    //   2. Counter must NOT jump up faster than elapsed time allows:
    //      max_increase = elapsed_hours × 10 kW + 1 kWh margin
    //
    // Midnight rollover: when the counter resets to ~0, the raw value
    // will legitimately drop below the previous value. We detect this
    // by checking if raw is small (< 5 kWh) and prev is large.
    //
    // IMPORTANT: the absolute range check (rule 0) runs REGARDLESS of
    // whether prev exists. Previously it was gated behind `if let Some(p)
    // = prev`, which meant the first reading after every reconnect had
    // zero validation — corrupted values like 1010 kWh passed through
    // and became the "previous" reference, poisoning all subsequent reads.
    //
    // When the value is out of range, we use the previous reading's value
    // instead of clamping to 0. Clamping to 0 poisons the delta baseline:
    // the next reading sees prev < 1.0 and skips the delta check, allowing
    // a corrupted value through and causing massive cost spikes.
    {
        let max_daily_kwh: f32 = 200.0; // hard ceiling: 10kW × 20h theoretical max

        macro_rules! check_energy_field {
            ($name:literal, $value:expr, $prev_val:expr) => {
                let raw = $value;
                if raw < 0.0 || raw > max_daily_kwh {
                    let prev_v: Option<f32> = $prev_val;
                    if let Some(pv) = prev_v {
                        tracing::warn!(
                            field = $name, raw, max = max_daily_kwh, prev = pv,
                            "Daily energy out of plausible daily range — using previous",
                        );
                        $value = pv;
                    } else {
                        tracing::warn!(
                            field = $name, raw, max = max_daily_kwh,
                            "Daily energy out of plausible daily range — no previous, clamping to 0",
                        );
                        $value = 0.0;
                    }
                    sanitized = true;
                }
            };
        }

        check_energy_field!("today_solar_kwh", snap.today_solar_kwh, prev.map(|p| p.today_solar_kwh));
        check_energy_field!("today_import_kwh", snap.today_import_kwh, prev.map(|p| p.today_import_kwh));
        check_energy_field!("today_export_kwh", snap.today_export_kwh, prev.map(|p| p.today_export_kwh));
        check_energy_field!("today_charge_kwh", snap.today_charge_kwh, prev.map(|p| p.today_charge_kwh));
        check_energy_field!("today_discharge_kwh", snap.today_discharge_kwh, prev.map(|p| p.today_discharge_kwh));
        check_energy_field!("today_consumption_kwh", snap.today_consumption_kwh, prev.map(|p| p.today_consumption_kwh));
        check_energy_field!("today_ac_charge_kwh", snap.today_ac_charge_kwh, prev.map(|p| p.today_ac_charge_kwh));
    }

    // Delta checks — only when we have a previous reading AND we're past
    // the grace period after connect. During the grace period, only the
    // absolute range check applies — the dongle can return plausible-but-wrong
    // values that would poison the delta baseline.
    if !skip_delta {
    if let Some(p) = prev {
        // Time-based increase threshold: scale with elapsed time since
        // last reading so that reconnect/restart gaps don't trigger false
        // rejections. 10 kW is a generous residential circuit capacity.
        let elapsed_secs = (snap.timestamp - p.timestamp).max(0) as f32;
        let max_increase_kwh = (elapsed_secs / 3600.0) * 10.0 + 1.0;

        macro_rules! check_energy_delta {
            ($name:literal, $value:expr, $prev:expr) => {
                let raw = $value;
                let prev_val = $prev;

                // If prev is 0 or near-zero, it may have been clamped by
                // the absolute range check (corrupted) or be a genuine
                // start-of-day reading.
                //
                // Previously we skipped the delta check entirely when
                // prev < 1.0, but this allowed corrupted values through
                // (e.g. 42.5 after prev was clamped to 0). Instead, we now
                // use a tighter absolute ceiling scaled by elapsed time.
                // The absolute range check above already validates against
                // the 200 kWh daily max, so we only need to catch jumps
                // that are plausible daily values but implausible deltas.
                if prev_val < 1.0 {
                    // prev is unreliable — apply a tighter max-increase check.
                    // Since prev could be a genuine start-of-day 0, accept
                    // raw only if it's a plausible single-interval increase.
                    if raw > max_increase_kwh {
                        tracing::warn!(
                            field = $name, raw, prev = prev_val,
                            elapsed_secs, max_increase_kwh,
                            "Daily energy jumped from near-zero baseline — clamping to max_increase",
                        );
                        $value = prev_val + max_increase_kwh;
                        sanitized = true;
                    }
                    // Otherwise accept raw (plausible increase from 0)
                }
                // Midnight rollover: counter legitimately reset to ~0.
                // Allow if raw is small and prev was large.
                else if raw < prev_val && raw < 5.0 && prev_val > 5.0 {
                    // Legitimate midnight reset — accept raw as-is
                }
                // Counter must not decrease (register corruption)
                else if raw < prev_val {
                    tracing::warn!(
                        field = $name, raw, prev = prev_val,
                        "Daily energy decreased (register corruption) — using previous",
                    );
                    $value = prev_val;
                    sanitized = true;
                }
                // Increase must be plausible for elapsed time
                else if raw > prev_val + max_increase_kwh {
                    tracing::warn!(
                        field = $name, raw, prev = prev_val,
                        elapsed_secs, max_increase_kwh,
                        "Daily energy jumped too fast — using previous",
                    );
                    $value = prev_val;
                    sanitized = true;
                }
            };
        }

        check_energy_delta!("today_solar_kwh", snap.today_solar_kwh, p.today_solar_kwh);
        check_energy_delta!("today_import_kwh", snap.today_import_kwh, p.today_import_kwh);
        check_energy_delta!("today_export_kwh", snap.today_export_kwh, p.today_export_kwh);
        check_energy_delta!("today_charge_kwh", snap.today_charge_kwh, p.today_charge_kwh);
        check_energy_delta!("today_discharge_kwh", snap.today_discharge_kwh, p.today_discharge_kwh);
        check_energy_delta!("today_consumption_kwh", snap.today_consumption_kwh, p.today_consumption_kwh);
        check_energy_delta!("today_ac_charge_kwh", snap.today_ac_charge_kwh, p.today_ac_charge_kwh);
    }
    } // skip_delta

    // Clamp battery limits to valid ranges (registers can return corrupted values)
    snap.charge_rate = snap.charge_rate.min(100);
    snap.discharge_rate = snap.discharge_rate.min(100);
    snap.active_power_rate = snap.active_power_rate.min(100);
    snap.battery_reserve = snap.battery_reserve.min(100);

    // Battery voltage: reject spurious readings. Nominal is 51.2V (LV) or 307V (HV).
    // Anything > 60V on an LV system or > 400V on an HV system is a corrupt register.
    let max_battery_voltage = match snap.device_type {
        crate::inverter::model::DeviceType::AllInOne6kW
        | crate::inverter::model::DeviceType::AllInOne5kW
        | crate::inverter::model::DeviceType::AIO8kW
        | crate::inverter::model::DeviceType::AIO10kW => 400.0,
        crate::inverter::model::DeviceType::ThreePhase => 100.0,
        _ => 60.0,
    };
    if snap.battery_voltage > max_battery_voltage || snap.battery_voltage < 0.0 {
        if let Some(p) = prev {
            tracing::warn!(raw = snap.battery_voltage, prev = p.battery_voltage, "Battery voltage out of range — using previous");
            snap.battery_voltage = p.battery_voltage;
        } else {
            snap.battery_voltage = 0.0;
        }
        sanitized = true;
    }

    // Battery mode debounce: require 2 consecutive identical readings
    // before accepting a mode change. A single corrupt register read can
    // flip enable_discharge or battery_power_mode, causing the derived mode
    // to flicker for one poll cycle.
    if let Some(p) = prev {
        if snap.battery_mode != p.battery_mode {
            // Mode changed — is this a confirmation of a pending change?
            if let Some(pm) = pending_mode {
                if *pm == snap.battery_mode {
                    // Second consecutive reading with the same new mode — accept it.
                    *pending_mode = None;
                    tracing::debug!(
                        new_mode = ?snap.battery_mode,
                        "Battery mode change confirmed after debounce"
                    );
                } else {
                    // Different transient mode — still pending, revert.
                    tracing::warn!(
                        new_mode = ?snap.battery_mode,
                        prev_mode = ?p.battery_mode,
                        pending = ?pm,
                        "Battery mode flicker (3rd different value) — keeping previous"
                    );
                    snap.battery_mode = p.battery_mode;
                    sanitized = true;
                }
            } else {
                // First reading with a different mode — don't accept yet, pend it.
                tracing::debug!(
                    new_mode = ?snap.battery_mode,
                    prev_mode = ?p.battery_mode,
                    "Battery mode change pending confirmation"
                );
                *pending_mode = Some(snap.battery_mode);
                snap.battery_mode = p.battery_mode;
                sanitized = true;
            }
        } else if pending_mode.is_some() {
            // Mode reverted back to previous — the pending change was a glitch.
            tracing::debug!("Battery mode reverted — pending change was a glitch");
            *pending_mode = None;
        }
    }

    // ---- Slot data sanitization ----
    // Charge/discharge slot times are stored in HR registers that the dongle
    // can corrupt just as easily as telemetry registers. Apply delta checks
    // so a single corrupted register read doesn't flip the UI.
    //
    // Enable_charge and enable_discharge flips are flagged for re-read but
    // NOT reverted — intentional changes from the control API must propagate.
    // The immediate re-read confirms the change on the next poll cycle.
    if let Some(p) = prev {
        if snap.enable_charge != p.enable_charge {
            tracing::debug!(
                raw = snap.enable_charge,
                prev = p.enable_charge,
                "enable_charge changed — re-reading to confirm"
            );
            sanitized = true;
        }
        if snap.enable_discharge != p.enable_discharge {
            tracing::debug!(
                raw = snap.enable_discharge,
                prev = p.enable_discharge,
                "enable_discharge changed — re-reading to confirm"
            );
            sanitized = true;
        }

        // Slot time delta checks: a slot's start/end times should not change
        // by more than 12 hours between consecutive polls (one poll cycle is
        // at most 60 seconds; no legitimate schedule change causes a 12h jump).
        // We compare each slot against its previous counterpart.
        const MAX_SLOT_HOUR_SHIFT: i16 = 12;

        for i in 0..snap.charge_slots.len().min(p.charge_slots.len()) {
            let cur_sh = snap.charge_slots[i].start_hour;
            let cur_eh = snap.charge_slots[i].end_hour;
            let cur_en = snap.charge_slots[i].enabled;

            let prev_sh = p.charge_slots[i].start_hour;
            let prev_eh = p.charge_slots[i].end_hour;
            let prev_en = p.charge_slots[i].enabled;

            if cur_en != prev_en && (cur_sh != prev_sh || cur_eh != prev_eh) {
                tracing::warn!(
                    slot = i, cur_enabled = cur_en, prev_enabled = prev_en,
                    "Charge slot {i} enabled changed with different times — reverting",
                );
                snap.charge_slots[i].enabled = prev_en;
                sanitized = true;
            }
            if cur_sh.abs_diff(prev_sh) as i16 > MAX_SLOT_HOUR_SHIFT
                || cur_eh.abs_diff(prev_eh) as i16 > MAX_SLOT_HOUR_SHIFT
            {
                tracing::warn!(
                    slot = i, cur_sh, prev_sh, cur_eh, prev_eh,
                    "Charge slot {i} times jumped by >12h — using previous",
                );
                snap.charge_slots[i].start_hour = prev_sh;
                snap.charge_slots[i].start_minute = p.charge_slots[i].start_minute;
                snap.charge_slots[i].end_hour = prev_eh;
                snap.charge_slots[i].end_minute = p.charge_slots[i].end_minute;
                sanitized = true;
            }
        }

        for i in 0..snap.discharge_slots.len().min(p.discharge_slots.len()) {
            let cur_sh = snap.discharge_slots[i].start_hour;
            let cur_eh = snap.discharge_slots[i].end_hour;
            let cur_en = snap.discharge_slots[i].enabled;

            let prev_sh = p.discharge_slots[i].start_hour;
            let prev_eh = p.discharge_slots[i].end_hour;
            let prev_en = p.discharge_slots[i].enabled;

            if cur_en != prev_en && (cur_sh != prev_sh || cur_eh != prev_eh) {
                tracing::warn!(
                    slot = i, cur_enabled = cur_en, prev_enabled = prev_en,
                    "Discharge slot {i} enabled changed with different times — reverting",
                );
                snap.discharge_slots[i].enabled = prev_en;
                sanitized = true;
            }
            if cur_sh.abs_diff(prev_sh) as i16 > MAX_SLOT_HOUR_SHIFT
                || cur_eh.abs_diff(prev_eh) as i16 > MAX_SLOT_HOUR_SHIFT
            {
                tracing::warn!(
                    slot = i, cur_sh, prev_sh, cur_eh, prev_eh,
                    "Discharge slot {i} times jumped by >12h — using previous",
                );
                snap.discharge_slots[i].start_hour = prev_sh;
                snap.discharge_slots[i].start_minute = p.discharge_slots[i].start_minute;
                snap.discharge_slots[i].end_hour = prev_eh;
                snap.discharge_slots[i].end_minute = p.discharge_slots[i].end_minute;
                sanitized = true;
            }
        }

        // Target SOC (HR 116): must be 0-100 (validated on decode, but
        // double-check here too since it drives charging behavior).
        if snap.target_soc > 100 {
            tracing::warn!(
                raw = snap.target_soc,
                prev = p.target_soc,
                "Target SOC out of range — using previous"
            );
            snap.target_soc = p.target_soc;
            sanitized = true;
        }

        // Battery reserve (HR 110): must be 0-100.
        if snap.battery_reserve > 100 {
            tracing::warn!(
                raw = snap.battery_reserve,
                prev = p.battery_reserve,
                "Battery reserve out of range — using previous"
            );
            snap.battery_reserve = p.battery_reserve;
            sanitized = true;
        }
    }

    sanitized
}

// ---------------------------------------------------------------------------
// Auto winter mode
// ---------------------------------------------------------------------------

/// Evaluate the auto-winter state machine and return register writes if a
/// state transition requires changing the inverter's configuration (enabling
/// or disabling winter mode).
///
/// The state machine uses two temperature thresholds with hysteresis:
///   * `cold_threshold` — temperature below which we start counting
///   * `recovery_threshold` — temperature above which we start counting
///
/// To prevent a single corrupt temperature reading from triggering a
/// transition, the state machine requires `debounce_readings` consecutive
/// polls with the temperature on the same side of the threshold before
/// acting. A single reading on the other side resets the counter.
fn check_auto_winter(
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
                    "Auto winter: battery cold — counting",
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
                    // disk after a restart — those reflect the original state
                    // before winter mode first activated.
                    if saved.is_none() {
                        *saved = Some(AutoWinterSaved {
                            enable_charge_target: snap.enable_charge_target,
                            target_soc: snap.target_soc,
                        });
                    }
                    *state = AutoWinterState::WinterActive;
                    return Some(vec![
                        RegisterWrite { address: HR_ENABLE_CHARGE_TARGET, value: 1 },
                        RegisterWrite { address: HR_CHARGE_TARGET_SOC, value: config.target_soc as u16 },
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
                    "Auto winter: battery warming — counting",
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
                        Some(s) => (s.target_soc as u16, if s.enable_charge_target { 1 } else { 0 }),
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
                        RegisterWrite { address: HR_ENABLE_CHARGE_TARGET, value: restore_enable },
                        RegisterWrite { address: HR_CHARGE_TARGET_SOC, value: restore_target },
                    ]);
                }
            } else if temp < config.cold_threshold {
                *state = AutoWinterState::WinterActive;
            }
        }
    }

    None
}

/// Validate raw battery BMS register data to reject garbage from non-existent
/// batteries on multi-battery probe addresses (0x33-0x37).
///
/// The dongle can return stale or corrupted data for addresses that don't have
/// a real battery. The SOC check (`soc > 0 && soc <= 100`) isn't sufficient
/// because garbage data can produce a non-zero SOC. This function checks:
///
/// 1. **Serial number** (IR 110-114, 5 regs) — must contain printable ASCII
///    characters (not all whitespace). A non-existent battery produces empty
///    or non-printable serials.
///
/// 2. **Module voltage** (IR 82-83, uint32 mV) — must be 30-65V. LV batteries
///    typically operate at 45-58V. Garbage from non-existent batteries produces
///    either 0V or extreme values.
///
/// 3. **Calibrated capacity** (IR 84-85, uint32 0.01 Ah) — must be > 0.
///    A non-existent battery returns 0.
fn validate_battery_bms(data: &[u16]) -> bool {
    // 1. Serial number must be printable and non-empty
    let serial = crate::inverter::decoder::decode_serial(data, 110 - 60, 5);
    let trimmed = serial.trim();
    if trimmed.is_empty() || trimmed.len() < 4 {
        return false;
    }
    if !trimmed.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
        return false;
    }

    // 2. Module voltage: IR(82-83) uint32 milli-V → V
    let v_raw = ((data.get(82 - 60).copied().unwrap_or(0) as u32) << 16)
        | (data.get(83 - 60).copied().unwrap_or(0) as u32);
    let voltage = v_raw as f32 * 0.001;
    if !(30.0..=65.0).contains(&voltage) {
        return false;
    }

    // 3. Calibrated capacity: IR(84-85) uint32 0.01 Ah → Ah
    let cap_raw = ((data.get(84 - 60).copied().unwrap_or(0) as u32) << 16)
        | (data.get(85 - 60).copied().unwrap_or(0) as u32);
    let capacity_ah = cap_raw as f32 * 0.01;
    if capacity_ah <= 0.0 {
        return false;
    }

    true
}

pub async fn run_poll_loop(state: Arc<AppState>) {
    let mut backoff = Duration::from_secs(5);

    loop {
        // ---- Read current settings ----
        let settings = state.settings.lock().await.clone();

        // Wait until a host is configured. Serial may be empty — it will be
        // auto-discovered from the first response.
        if settings.host.is_empty() {
            tracing::debug!("Poll loop: waiting for host setting");
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }

        // ---- Create client and connect ----
        let mut client = ModbusClient::new(&settings.host, settings.port, &settings.serial);

        match client.connect().await {
            Ok(()) => {
                tracing::info!(
                    host = %settings.host,
                    port = settings.port,
                    "Connected to inverter"
                );

                // Broadcast connected state.
                {
                    let mut cs = state.connection_state.lock().await;
                    *cs = ConnectionState::Connected;
                }
                let _ = state.tx.send(PollMessage::Connection {
                    state: ConnectionState::Connected,
                    host: settings.host.clone(),
                });

                // Allow the dongle time to initialise after TCP connect.
                // The GivEnergy dongle has a slow processor and may return
                // Modbus exception code 67 (busy/not-ready) if queried too soon.
                tokio::time::sleep(Duration::from_millis(500)).await;

                // Drain any stale data the dongle buffered from a previous
                // session — without this, cached responses corrupt the
                // request-response pairing for the first poll.
                client.drain().await;

                // Warmup reads: discard the first register reads after connect.
                // The dongle's internal state can be stale after a TCP reconnect,
                // causing the first reads to return garbage values (e.g.
                // today_import_kwh = 0.6 when the real value is 39.0). We do
                // multiple warmup reads because a single discard isn't enough —
                // the dongle can return corrupted data for several reads.
                for i in 0..3 {
                    match client.read_all_standard().await {
                        Ok(blocks) => {
                            tracing::debug!(
                                "Warmup read {}/3 — OK ({} blocks)",
                                i + 1,
                                blocks.len()
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Warmup read {}/3 — FAILED: {e}",
                                i + 1,
                            );
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }

                // Clear any previous snapshot so the next reading is accepted
                // without delta sanitization. After a reconnect, the previous
                // snapshot may contain stale or corrupted values from the old
                // session. The absolute range check (0–200 kWh) still applies.
                {
                    let mut latest = state.latest_snapshot.lock().await;
                    *latest = None;
                }

                // Reset back-off on successful connection.
                backoff = Duration::from_secs(5);

                // Track consecutive poll failures within this connection.
                // Transient errors (dongle busy, stale frames) are retried
                // without disconnecting; only after repeated failures do we
                // tear down the connection.
                let mut consecutive_failures: u8 = 0;
                const MAX_CONSECUTIVE_FAILURES: u8 = 3;

                // Grace period: for the first few reads after connect, skip
                // delta sanitization. The dongle can return plausible-but-wrong
                // values (e.g. 0.6 kWh import when real is 39.0) that pass the
                // absolute range check but corrupt the "previous" reference.
                // After GRACE_READINGS the delta checks kick in.
                let mut readings_since_connect: u8 = 0;
                const GRACE_READINGS: u8 = 3;
                let mut pending_mode: Option<BatteryMode> = None;
                let mut known_device_type: Option<crate::inverter::model::DeviceType> = None;
                let mut detected_meters: Vec<u8> = Vec::new();

                // Restore cosy_active from persisted settings on restart.
                // Without this, a client reboot during a cosy slot would leave
                // the inverter in the previous force-charge state but the client
                // thinking cosy is inactive, never sending the exit command.
                {
                    let settings = crate::settings::Settings::load();
                    if settings.cosy_enabled {
                        let now = chrono::Local::now();
                        let now_minutes = now.hour() as u16 * 60 + now.minute() as u16;
                        let in_slot = crate::settings::cosy_active_slot(now_minutes, &settings.cosy_slots);
                        if let Some(restore_target_soc) = in_slot {
                            tracing::info!(
                                "Cosy: restart detected inside slot (target SOC {}%) — force-charge will be retried after first poll",
                                restore_target_soc
                            );
                            // Leave cosy_active=false so the normal Cosy state machine
                            // re-sends ForceCharge after the first successful poll. If a
                            // write fails, it will keep retrying on subsequent polls.
                        }
                    }
                }

                // ---- Inner poll loop ----
                // settings_version tracks the settings version at connection start.
                // Each iteration captures the CURRENT version before the poll and
                // compares it against this baseline after the poll, so a version
                // bump by the API is always detected regardless of timing.
                let settings_version_at_connect = state.settings.lock().await.version;
                loop {
                    // Capture version BEFORE the poll to detect changes that
                    // happen during the poll (API bumps version while we read).
                    // NOTE: this is the INSTANTANEOUS version, not a stored
                    // baseline. The baseline check happens after the sleep.
                    let current_version = state.settings.lock().await.version;

                    // If version changed since we last connected, break immediately.
                    if current_version != settings_version_at_connect {
                        tracing::info!(
                            "Settings changed (v{} → v{}) — reconnecting",
                            current_version, settings_version_at_connect
                        );
                        break;
                    }

                    // Drain and execute any pending register writes from the
                    // control API before reading the latest state.
                    let pending: Vec<Vec<RegisterWrite>> = {
                        let mut pw = state.pending_writes.lock().await;
                        std::mem::take(&mut *pw)
                    };
                    if !pending.is_empty() {
                        // Flush stale read responses from the previous poll cycle
                        client.drain_stale_frames().await;

                        for writes in &pending {
                            for w in writes {
                                match client.write_register(w.address, w.value).await {
                                    Ok(()) => {
                                        tracing::info!(
                                            "Wrote register {} = {}",
                                            w.address, w.value
                                        );
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            "Failed to write register {} = {}: {e}",
                                            w.address, w.value
                                        );
                                    }
                                }
                                // Pause between individual register writes
                                // The dongle needs significant time between writes
                                // to adjacent registers (up to 13s observed for
                                // exception-67 recovery)
                                tokio::time::sleep(Duration::from_millis(1500)).await;
                            }
                        }

                        // Flush any stale frames left over from write responses
                        // before starting the read cycle
                        client.drain_stale_frames().await;
                    }

                    let (poll_ok, sanitized) = async {
                        match client.read_all_with_extras(known_device_type.as_ref()).await {
                            Ok(blocks) => {
                                let mut snapshot = decode_snapshot(&blocks);

                                // Cache the device type for subsequent polls.
                                // This enables model-aware polling (extra blocks).
                                // 'Unknown(0)' means we haven't identified the model yet.
                                let is_new_model = known_device_type.is_none()
                                    && !matches!(snapshot.device_type, crate::inverter::model::DeviceType::Unknown(_));
                                if is_new_model {
                                    tracing::info!(
                                        device_type = ?snapshot.device_type,
                                        extra_blocks = ?snapshot.device_type.extra_poll_blocks().iter().map(|b| b.name).collect::<Vec<_>>(),
                                        "Device model identified — enabling model-aware polling"
                                    );
                                    known_device_type = Some(snapshot.device_type);

                                    // Probe for external CT clamp meters (device addresses 0x01-0x08).
                                    // A meter is present if V_phase_1 (IR 60) is non-zero and >100V.
                                    tracing::info!("Probing for external CT meters...");
                                    let mut found_meters: Vec<u8> = Vec::new();
                                    for &addr in crate::modbus::registers::METER_ADDRESSES {
                                        match client.read_registers_at_slave(
                                            addr,
                                            crate::modbus::framer::RegisterType::Input,
                                            60,
                                            30,
                                        ).await {
                                            Ok(data) => {
                                                if crate::inverter::decoder::validate_meter_data(&data) {
                                                    let meter = crate::inverter::decoder::decode_meter_data(&data, addr);
                                                    tracing::info!(
                                                        "Meter detected at addr 0x{addr:02X}: {:.1}V, {:.0}W",
                                                        meter.v_phase_1, meter.p_active_total
                                                    );
                                                    found_meters.push(addr);
                                                    snapshot.meters.push(meter);
                                                }
                                            }
                                            Err(e) => {
                                                tracing::debug!(
                                                    "Meter addr 0x{addr:02X}: no response: {e}",
                                                );
                                            }
                                        }
                                        tokio::time::sleep(Duration::from_millis(100)).await;
                                    }
                                    detected_meters = found_meters;
                                    if detected_meters.is_empty() {
                                        tracing::info!("No external CT meters detected");
                                    } else {
                                        tracing::info!(
                                            "Detected {} meter(s) at addresses: {:02X?}",
                                            detected_meters.len(), detected_meters
                                        );
                                    }
                                }

                                // If the dongle serial was auto-discovered from the
                                // response, persist it to settings so it survives restarts.
                                if client.serial_was_discovered() {
                                    let discovered = client.serial().to_string();
                                    tracing::info!(serial = %discovered, "Persisting auto-discovered serial");
                                    {
                                        let mut ps = state.settings.lock().await;
                                        ps.serial = discovered.clone();
                                    }
                                    let mut persist = crate::settings::Settings::load();
                                    persist.host = settings.host.clone();
                                    persist.port = settings.port;
                                    persist.serial = discovered;
                                    persist.poll_interval = settings.interval_secs;
                                    persist.auto_connect = true;
                                    if let Err(e) = persist.save() {
                                        tracing::warn!("Failed to persist discovered serial: {e}");
                                    }
                                } else if client.serial_is_suspect() {
                                    tracing::warn!(
                                        "Auto-discovered serial is suspect (truncated frame) — keeping empty serial for all requests. If the connection fails, try setting the serial manually in Settings."
                                    );
                                }

                                // --- Battery BMS module reads ---
                                //
                                // Per givenergy-modbus reference, LV batteries expose BMS data
                                // on the inverter's IR 60-119 at device address 0x32 (battery #1)
                                // and additional batteries at 0x33, 0x34, … 0x37.
                                //
                                // Battery #1 IR 60-119 is NOT part of the standard poll blocks
                                // (those only read IR 0-59), so we issue a separate read here.
                                // Additional batteries also need separate reads at their own
                                // device addresses.

                                // Read battery #1 BMS (device 0x32, IR 60-119)
                                match client
                                    .read_registers(
                                        crate::modbus::framer::RegisterType::Input,
                                        60,
                                        60,
                                    )
                                    .await
                                {
                                    Ok(data) => {
                                        crate::inverter::decoder::decode_battery_block_into(
                                            &data, 0, &mut snapshot, "",
                                        );
                                        tracing::debug!("Battery #1 BMS read OK");

                                        // Override SOC with BMS module SOC (IR 100) only when
                                        // When inverter IR(59) returns 0 (corrupted), calculate
                                        // aggregate SOC from capacity-weighted average of all
                                        // battery modules.
                                        // Note: full aggregate is computed below after all
                                        // additional batteries are read.
                                        if snapshot.soc == 0 && !snapshot.battery_modules.is_empty() {
                                            if let Some(bms) = snapshot.battery_modules.first() {
                                                if bms.soc > 0 && bms.soc <= 99 {
                                                    snapshot.soc = bms.soc;
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::debug!("Battery #1 BMS read skipped: {e}");
                                    }
                                }

                                // Probe additional LV batteries (device addresses 0x33-0x37)
                                for (i, &addr) in crate::modbus::registers::LV_BATTERY_ADDRESSES
                                    .iter()
                                    .enumerate()
                                {
                                    match client.read_registers_at_slave(
                                        addr,
                                        crate::modbus::framer::RegisterType::Input,
                                        60,
                                        60,
                                    ).await {
                                        Ok(data) => {
                                            let soc = *data.get(100 - 60).unwrap_or(&0) as u8;
                                            if soc > 0 && soc <= 100 && validate_battery_bms(&data) {
                                                crate::inverter::decoder::decode_battery_block_into(
                                                    &data, i + 1, &mut snapshot, "",
                                                );
                                                tracing::info!(
                                                    "Battery #{} detected at addr 0x{:02X} (SOC={}%)",
                                                    i + 2, addr, soc
                                                );
                                            } else {
                                                tracing::debug!(
                                                    "Battery addr 0x{:02X}: SOC={} — not present",
                                                    addr, soc
                                                );
                                                break;
                                            }
                                        }
                                        Err(e) => {
                                            tracing::debug!(
                                                "Battery addr 0x{:02X}: no response: {e}",
                                                addr
                                            );
                                            break;
                                        }
                                    }
                                }

                                // --- External CT meter reads ---
                                // Read all previously detected meters on every poll cycle.
                                // If a meter stops responding, we skip it silently.
                                let mut fresh_meters: Vec<crate::inverter::model::MeterData> =
                                    Vec::with_capacity(detected_meters.len());
                                for &addr in &detected_meters {
                                    match client.read_registers_at_slave(
                                        addr,
                                        crate::modbus::framer::RegisterType::Input,
                                        60,
                                        30,
                                    ).await {
                                        Ok(data) => {
                                            fresh_meters.push(
                                                crate::inverter::decoder::decode_meter_data(&data, addr)
                                            );
                                        }
                                        Err(e) => {
                                            tracing::debug!(
                                                "Meter addr 0x{addr:02X}: read failed: {e}",
                                            );
                                        }
                                    }
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                }
                                if !fresh_meters.is_empty() {
                                    snapshot.meters = fresh_meters;
                                }

                                // If inverter IR(59) was 0, recalculate SOC from
                                // capacity-weighted average of ALL battery modules
                                // (now that additional batteries have been read).
                                if snapshot.soc == 0 && snapshot.battery_modules.len() > 1 {
                                    let total_cap: f32 = snapshot
                                        .battery_modules.iter().map(|m| m.capacity_ah).sum();
                                    let total_rem: f32 = snapshot
                                        .battery_modules.iter().map(|m| m.remaining_capacity_ah).sum();
                                    if total_cap > 0.0 {
                                        let agg = (total_rem / total_cap * 100.0).round() as u8;
                                        snapshot.soc = agg.min(100);
                                        tracing::debug!(
                                            "Inverter SOC was 0 — aggregate from {} modules: {}%",
                                            snapshot.battery_modules.len(),
                                            snapshot.soc
                                        );
                                    }
                                }

                                // Store latest snapshot.
                                // Sanitize against physically impossible values first.
                                // Skip delta checks during the grace period after connect.
                                let in_grace = readings_since_connect < GRACE_READINGS;
                                let (sanitized, prev_modules) = {
                                    let prev = state.latest_snapshot.lock().await;
                                    let s = sanitize_snapshot(&mut snapshot, prev.as_ref(), in_grace, &mut pending_mode);
                                    let mods = prev.as_ref().map(|p| p.battery_modules.clone());
                                    (s, mods)
                                };
                                carry_forward_battery_modules_with(&mut snapshot, prev_modules.as_deref());
                                readings_since_connect = readings_since_connect.saturating_add(1);

                                if readings_since_connect == 1 {
                                    tracing::info!(
                                        soc = snapshot.soc,
                                        solar_w = snapshot.solar_power,
                                        battery_w = snapshot.battery_power,
                                        grid_w = snapshot.grid_power,
                                        "First poll read after connect — data is flowing"
                                    );
                                }

                                // ---- Auto winter mode ----
                                {
                                    let config = state.auto_winter_config.lock().await;
                                    let mut aw_state = state.auto_winter_state.lock().await;
                                    let mut saved = state.auto_winter_saved.lock().await;
                                    let writes = check_auto_winter(
                                        &snapshot, &config, &mut aw_state, &mut saved,
                                    );
                                    // Tag the snapshot so the frontend knows
                                    // whether winter mode was triggered by
                                    // this system vs. manually.
                                    snapshot.auto_winter_active =
                                        matches!(*aw_state, AutoWinterState::WinterActive);
                                    snapshot.cosy_active = *state.cosy_active.lock().await;
                                    // Load cosy_enabled from settings so the frontend
                                    // knows cosy is configured even between slots.
                                    snapshot.cosy_enabled = crate::settings::Settings::load().cosy_enabled;

                                    // Persist saved values to disk so they survive a
                                    // restart. When winter mode deactivates, saved
                                    // becomes None — this clears the persisted values.
                                    let persist_saved = saved.clone();
                                    drop(config);
                                    drop(aw_state);
                                    drop(saved);

                                    let mut app_settings = crate::settings::Settings::load();
                                    let changed = app_settings.auto_winter_saved_enable_target
                                        != persist_saved.as_ref().map(|s| s.enable_charge_target)
                                        || app_settings.auto_winter_saved_target_soc
                                        != persist_saved.as_ref().map(|s| s.target_soc as u16);
                                    if changed {
                                        app_settings.auto_winter_saved_enable_target =
                                            persist_saved.as_ref().map(|s| s.enable_charge_target);
                                        app_settings.auto_winter_saved_target_soc =
                                            persist_saved.as_ref().map(|s| s.target_soc as u16);
                                        if let Err(e) = app_settings.save() {
                                            tracing::warn!("Failed to persist auto winter saved values: {e}");
                                        }
                                    }

                                    if let Some(writes) = writes {
                                        for w in &writes {
                                            match client.write_register(w.address, w.value).await {
                                                Ok(()) => tracing::info!(
                                                    "Auto winter: wrote reg {} = {}",
                                                    w.address, w.value
                                                ),
                                                Err(e) => tracing::error!(
                                                    "Auto winter: write reg {} failed: {e}",
                                                    w.address
                                                ),
                                            }
                                            tokio::time::sleep(Duration::from_millis(1500)).await;
                                        }
                                    }
                                }

                                // ---- Cosy charging mode ----
                                {
                                    let settings = crate::settings::Settings::load();
                                    if settings.cosy_enabled {
                                        let now = chrono::Local::now();
                                        let now_minutes = now.hour() as u16 * 60 + now.minute() as u16;

                                        // Check if we're inside any enabled cosy slot
                                        let slot_target_soc = crate::settings::cosy_active_slot(now_minutes, &settings.cosy_slots);
                                        let in_slot = slot_target_soc.is_some();
                                        let slot_target_soc = slot_target_soc.unwrap_or(100);

                                        let cosy_active = state.cosy_active.lock().await;
                                        if in_slot && !*cosy_active {
                                            // Entering a cosy slot — start force charge.
                                            // Drain stale frames first to avoid function code
                                            // mismatches (stale read responses can be mistaken
                                            // for write acknowledgments).
                                            client.drain_stale_frames().await;
                                            tracing::info!("Cosy: entering slot, force-charging to {}%", slot_target_soc);
                                            drop(cosy_active);
                                            let cmd = ControlCommand::ForceCharge { target_soc: slot_target_soc as u16 };
                                            let mut all_writes_ok = true;
                                            if let Ok(writes) = cmd.encode() {
                                                for w in &writes {
                                                    match client.write_register(w.address, w.value).await {
                                                        Ok(()) => tracing::info!("Cosy: wrote reg {} = {}", w.address, w.value),
                                                        Err(e) => {
                                                            tracing::error!("Cosy: write reg {} failed: {e}", w.address);
                                                            all_writes_ok = false;
                                                        }
                                                    }
                                                    tokio::time::sleep(Duration::from_millis(1500)).await;
                                                }
                                            } else {
                                                all_writes_ok = false;
                                            }

                                            if all_writes_ok {
                                                *state.cosy_active.lock().await = true;
                                            } else {
                                                tracing::warn!("Cosy: force-charge writes failed — will retry on next poll");
                                            }
                                        } else if !in_slot && *cosy_active {
                                            // Exiting a cosy slot — restore normal Eco mode.
                                            // Drain stale frames for the same reason as entry.
                                            client.drain_stale_frames().await;
                                            tracing::info!("Cosy: exiting slot, restoring Eco mode");
                                            drop(cosy_active);
                                            let cmd = ControlCommand::CosyExit;
                                            let mut all_writes_ok = true;
                                            if let Ok(writes) = cmd.encode() {
                                                for w in &writes {
                                                    match client.write_register(w.address, w.value).await {
                                                        Ok(()) => tracing::info!("Cosy: wrote reg {} = {}", w.address, w.value),
                                                        Err(e) => {
                                                            tracing::error!("Cosy: write reg {} failed: {e}", w.address);
                                                            all_writes_ok = false;
                                                        }
                                                    }
                                                    tokio::time::sleep(Duration::from_millis(1500)).await;
                                                }
                                            } else {
                                                all_writes_ok = false;
                                            }

                                            if all_writes_ok {
                                                *state.cosy_active.lock().await = false;
                                            } else {
                                                tracing::warn!("Cosy: exit writes failed — will retry on next poll");
                                            }
                                        } else {
                                            drop(cosy_active);
                                        }
                                    }
                                }

                                {
                                    let mut latest = state.latest_snapshot.lock().await;
                                    *latest = Some(snapshot.clone());
                                }

                                // Broadcast to WebSocket subscribers.
                                let _ = state.tx.send(PollMessage::Snapshot(snapshot.clone()));

                                // Persist to history database.
                                // The snapshot has already been sanitized, so skip
                                // only if SOC is still 0 (no previous fallback available).
                                {
                                    let h = state.history.lock().await;
                                    if let Some(ref db) = *h {
                                        if snapshot.soc > 0 {
                                            db.insert_reading(&snapshot);
                                        }
                                    }
                                }

                                (true, sanitized)
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    consecutive_failures = consecutive_failures + 1,
                                    max = MAX_CONSECUTIVE_FAILURES,
                                    "Poll read failed"
                                );
                                (false, false)
                            }
                        }
                    }.await;

                    match poll_ok {
                        true => {
                            consecutive_failures = 0;

                            // Sanitization was applied — corrupted register data
                            // detected. Re-poll immediately instead of waiting
                            // for the next interval, so the frontend gets a
                            // fresh reading as soon as possible.
                            if sanitized {
                                tracing::debug!("Corrupted data detected — re-reading immediately");
                                continue;
                            }
                        }
                        false => {
                            consecutive_failures += 1;
                            if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                                tracing::warn!(
                                    "Poll read failed ({}/{}): — disconnecting",
                                    consecutive_failures, MAX_CONSECUTIVE_FAILURES,
                                );
                                break; // tear down connection and reconnect
                            }
                            // Transient error — retry after a short pause
                            tracing::debug!(
                                "Poll read failed ({}/{}): — retrying",
                                consecutive_failures, MAX_CONSECUTIVE_FAILURES,
                            );
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue; // stay in the inner loop
                        }
                    }

                    // Sleep for the configured interval, but wake early if:
                    //   • settings changed (new host → reconnect)
                    //   • new writes were queued (apply immediately)
                    //
                    // NOTE: current_version was captured at the TOP of this
                    // iteration (before the poll). Do NOT re-capture here —
                    // the sleep loop compares against the PRE-POLL version
                    // so it detects version bumps that happened during the poll.
                    let interval_secs = state.settings.lock().await.interval_secs;
                    let sleep_deadline = tokio::time::Instant::now() + Duration::from_secs(interval_secs);
                    loop {
                        // Wait up to 1 second, or until writes are queued
                        tokio::select! {
                            _ = state.write_notify.notified() => {
                                // Writes queued — wake immediately
                                tracing::debug!("Write notification received, waking early");
                                break;
                            }
                            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                        }
                        if tokio::time::Instant::now() >= sleep_deadline {
                            break;
                        }
                        let cur = state.settings.lock().await;
                        if cur.version != current_version {
                            tracing::info!("Settings changed (v{} → v{}) — reconnecting",
                                current_version, cur.version);
                            break;
                        }
                        if cur.interval_secs != interval_secs {
                            break;
                        }
                    }
                    // If settings version changed, reconnect
                    let cur = state.settings.lock().await;
                    if cur.version != current_version {
                        break; // exit inner loop → outer loop re-reads settings
                    }
                }

                // ---- Disconnected (fell out of inner loop) ----
                tracing::warn!(
                    host = %settings.host,
                    consecutive_failures,
                    max_failures = MAX_CONSECUTIVE_FAILURES,
                    "Disconnecting from inverter — will reconnect"
                );
                client.disconnect().await;

                // Clear the latest snapshot so the next connection starts fresh.
                // Without this, stale/corrupted values from the old session
                // persist as the sanitizer's "previous" reference.
                {
                    let mut latest = state.latest_snapshot.lock().await;
                    *latest = None;
                }

                tracing::debug!("Disconnected — entering reconnect cycle");

                {
                    let mut cs = state.connection_state.lock().await;
                    *cs = ConnectionState::Reconnecting;
                }
                let _ = state.tx.send(PollMessage::Connection {
                    state: ConnectionState::Reconnecting,
                    host: settings.host.clone(),
                });
            }
            Err(e) => {
                tracing::warn!(
                    "Connection to {}:{} failed: {e}",
                    settings.host,
                    settings.port
                );

                {
                    let mut cs = state.connection_state.lock().await;
                    *cs = ConnectionState::Disconnected;
                }
                let _ = state.tx.send(PollMessage::Connection {
                    state: ConnectionState::Disconnected,
                    host: settings.host.clone(),
                });
            }
        }

        // ---- Back-off before retry ----
        tracing::debug!("Retrying connection in {:?}", backoff);
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_settings_default() {
        let s = PollSettings::default();
        assert!(s.host.is_empty());
        assert!(s.serial.is_empty());
        assert_eq!(s.port, 8899);
        assert_eq!(s.interval_secs, 60);
    }

    #[test]
    fn app_state_new_creates_valid_state() {
        let state = AppState::new();
        // Can obtain a receiver from the broadcast channel.
        let _rx = state.tx.subscribe();
    }

    #[test]
    fn connection_state_serde() {
        let cs = ConnectionState::Connected;
        let json = serde_json::to_string(&cs).unwrap();
        assert!(json.contains("connected"));
    }

    #[test]
    fn poll_message_snapshot_roundtrip() {
        let snap = InverterSnapshot::default();
        let msg = PollMessage::Snapshot(snap);
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"snapshot\""));
        let de: PollMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(de, PollMessage::Snapshot(s) if s.timestamp == 0));
    }

    #[test]
    fn poll_message_connection_roundtrip() {
        let msg = PollMessage::Connection {
            state: ConnectionState::Reconnecting,
            host: "192.168.1.100".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"connection\""));
        let de: PollMessage = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(de, PollMessage::Connection { state: ConnectionState::Reconnecting, ref host } if host == "192.168.1.100")
        );
    }

    #[tokio::test]
    async fn app_state_latest_snapshot_starts_none() {
        let state = Arc::new(AppState::new());
        let snapshot = state.latest_snapshot.lock().await;
        assert!(snapshot.is_none());
    }

    #[tokio::test]
    async fn app_state_connection_starts_disconnected() {
        let state = Arc::new(AppState::new());
        let cs = state.connection_state.lock().await;
        assert_eq!(*cs, ConnectionState::Disconnected);
    }
}
