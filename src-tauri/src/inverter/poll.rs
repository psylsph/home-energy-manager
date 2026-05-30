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

use tokio::sync::{broadcast, Mutex, Notify};
use crate::server::logs::LogRing;
use crate::server::ws::ConnectedClients;

use crate::history::HistoryDb;
use crate::inverter::decoder::decode_snapshot;
use crate::inverter::encoder::RegisterWrite;
use crate::inverter::model::InverterSnapshot;
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AutoWinterState {
    /// Awaiting cold temperatures.
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

impl Default for AutoWinterState {
    fn default() -> Self {
        Self::Idle
    }
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
        }
    }

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

/// Sanitize a snapshot against physically impossible register values.
/// Compares against the previous snapshot to detect and correct garbled
/// readings before they reach the frontend or history database.
/// Returns `true` if any field was sanitized (fallback applied).
fn sanitize_snapshot(snap: &mut InverterSnapshot, prev: Option<&InverterSnapshot>) -> bool {
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
    // Sanitization rules:
    //   1. Value must be in [0, 1000] kWh (residential daily limit)
    //   2. Value must NOT decrease during the day (register corruption
    //      often returns a near-zero value like 39.0 → 0.6 → 39.0)
    //   3. Value must NOT jump up by more than the elapsed time allows:
    //      max_increase = elapsed_hours × 10 kW + 1 kWh margin
    //      This handles normal polls (60s → 0.27 kWh limit) and also
    //      reconnect/restart gaps (hours → generous catch-up allowance)
    //
    // Midnight rollover: when the counter resets to ~0, the raw value
    // will legitimately drop below the previous value. We detect this
    // by checking if raw is small (< 5 kWh) — a legitimate new-day
    // reading after midnight — and prev is large. In this case we
    // ALLOW the rollover.
    if let Some(p) = prev {
        let max_kwh: f32 = 1000.0;

        // Time-based increase threshold: scale with elapsed time since
        // last reading so that reconnect/restart gaps don't trigger false
        // rejections. 10 kW is a generous residential circuit capacity.
        let elapsed_secs = (snap.timestamp - p.timestamp).max(0) as f32;
        let max_increase_kwh = (elapsed_secs / 3600.0) * 10.0 + 1.0;

        macro_rules! check_energy_field {
            ($name:literal, $value:expr, $prev:expr) => {
                let raw = $value;
                let prev_val = $prev;

                // Rule 1: absolute range
                if raw < 0.0 || raw > max_kwh {
                    tracing::warn!(
                        field = $name, raw, prev = prev_val,
                        "Daily energy out of plausible range — using previous",
                    );
                    $value = prev_val;
                    sanitized = true;
                }
                // Midnight rollover: counter legitimately reset to ~0.
                // Allow if raw is small and prev was large.
                else if raw < prev_val && raw < 5.0 && prev_val > 5.0 {
                    // Legitimate midnight reset — accept raw as-is
                }
                // Rule 2: counter must not decrease (register corruption)
                else if raw < prev_val {
                    tracing::warn!(
                        field = $name, raw, prev = prev_val,
                        "Daily energy decreased (register corruption) — using previous",
                    );
                    $value = prev_val;
                    sanitized = true;
                }
                // Rule 3: increase must be plausible for elapsed time
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

        check_energy_field!("today_solar_kwh", snap.today_solar_kwh, p.today_solar_kwh);
        check_energy_field!("today_import_kwh", snap.today_import_kwh, p.today_import_kwh);
        check_energy_field!("today_export_kwh", snap.today_export_kwh, p.today_export_kwh);
        check_energy_field!("today_charge_kwh", snap.today_charge_kwh, p.today_charge_kwh);
        check_energy_field!("today_discharge_kwh", snap.today_discharge_kwh, p.today_discharge_kwh);
        check_energy_field!("today_consumption_kwh", snap.today_consumption_kwh, p.today_consumption_kwh);
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

                // Reset back-off on successful connection.
                backoff = Duration::from_secs(5);

                // Track consecutive poll failures within this connection.
                // Transient errors (dongle busy, stale frames) are retried
                // without disconnecting; only after repeated failures do we
                // tear down the connection.
                let mut consecutive_failures: u8 = 0;
                const MAX_CONSECUTIVE_FAILURES: u8 = 3;

                // ---- Inner poll loop ----
                loop {
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
                        match client.read_all_standard().await {
                            Ok(blocks) => {
                                let mut snapshot = decode_snapshot(&blocks);

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

                                        // Override SOC with BMS module SOC (IR 100) which is
                                        // more reliable than the inverter-level IR(59) that
                                        // intermittently returns 0.
                                        //
                                        // Validation: only override when the BMS value is
                                        // plausible — not 0 (garbage) and not a wild jump
                                        // from the inverter reading. If the inverter SOC is
                                        // already reasonable (> 0), the BMS must be within
                                        // ±30 points to be trusted. If inverter SOC is 0,
                                        // accept any BMS value 1–99 (skip 100 as it's also
                                        // a common garbage value).
                                        if let Some(bms) = snapshot.battery_modules.first() {
                                            let inverter_soc = snapshot.soc as i16;
                                            let bms_soc = bms.soc as i16;
                                            let plausible = if bms_soc <= 0 || bms_soc > 99 {
                                                false
                                            } else if inverter_soc > 0 {
                                                (bms_soc - inverter_soc).unsigned_abs() <= 30
                                            } else {
                                                true // inverter is 0, trust BMS (1-99)
                                            };
                                            if plausible {
                                                snapshot.soc = bms.soc;
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
                                            if soc <= 100 && soc > 0 {
                                                crate::inverter::decoder::decode_battery_block_into(
                                                    &data, i + 1, &mut snapshot, "",
                                                );
                                                tracing::debug!("Battery #{} BMS read OK (addr 0x{:02X})", i + 2, addr);
                                            } else {
                                                tracing::debug!("Battery addr 0x{:02X}: SOC={} — not present", addr, soc);
                                                break;
                                            }
                                        }
                                        Err(_) => {
                                            tracing::debug!("Battery addr 0x{:02X}: no response", addr);
                                            break;
                                        }
                                    }
                                }

                                // Store latest snapshot.
                                // Sanitize against physically impossible values first.
                                let sanitized = {
                                    let prev = state.latest_snapshot.lock().await;
                                    sanitize_snapshot(&mut snapshot, prev.as_ref())
                                };

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
                            Err(_) => (false, false),
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
                    let current_version = state.settings.lock().await.version;
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
                client.disconnect().await;

                tracing::warn!("Disconnected from inverter – will reconnect");

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
