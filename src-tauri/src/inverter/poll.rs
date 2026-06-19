//! Periodic inverter polling loop.
//!
//! Drives the timed read cycle that queries all relevant input
//! registers and publishes updated state to subscribers via
//! the WebSocket broadcast channel.
//!
//! ## Architecture
//!
//! The [`AppState`] struct is the central shared object. It holds:
//! - The latest [`InverterSnapshot`] behind an `Arc<Mutex<...>>`
//! - The current [`ConnectionState`]
//! - A [`broadcast::Sender`] that pushes snapshot and connection-state
//!   updates to all active WebSocket clients
//! - Mutable [`PollSettings`] (host, port, serial, interval)
//!
//! [`run_poll_loop`] is the main async entry point, intended to be
//! spawned as a long-lived Tokio task. It handles auto-reconnection
//! with exponential back-off.
//!
//! ## Lock-ordering safety (`parking_lot::Mutex` vs `tokio::sync::Mutex`)
//!
//! Most `AppState` fields use `tokio::sync::Mutex` (which is [fair][tokio-fair],
//! yielding during contention). One field — [`connected_clients`](AppState::connected_clients) —
//! uses `parking_lot::Mutex` because its access pattern is purely synchronous
//! (lock, read/write, unlock, never held across an `.await`).
//!
//! **Invariant (must never be violated):** `parking_lot::Mutex` MAY be locked
//! *inside* a `tokio::sync::Mutex` guard, but `parking_lot::Mutex` MUST NOT be
//! held **across** an `.await` point. Because `parking_lot::Mutex` does not
//! participate in the Tokio runtime's cooperative scheduling, holding it across
//! an `.await` would block the executor thread until the guard is dropped.
//!
//! Conversely, a `tokio::sync::Mutex` guard IS safe to hold while acquiring a
//! `parking_lot::Mutex` — the `parking_lot` lock is acquired for an instant
//! (no `.await` inside the critical section) and then dropped before the
//! next `.await`.
//!
//! ### Practical rule
//!
//! All code that accesses `connected_clients` must:
//! 1. Lock the `parking_lot::Mutex`.
//! 2. Do the synchronous work (read/write the clients map).
//! 3. Drop the guard BEFORE any `.await` on the same task.
//!
//! If you find yourself tempted to hold the guard while calling an async
//! function, refactor the code so the async call happens *after* the guard
//! is dropped, or switch that field to `tokio::sync::Mutex`.
//!
//! [tokio-fair]: https://docs.rs/tokio/latest/tokio/sync/struct.Mutex.html#fairness

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Timelike;

use crate::server::logs::LogRing;
use crate::server::ws::ConnectedClients;
use tokio::sync::{broadcast, Mutex, Notify};

use crate::alerts::AlertType;
use crate::history::HistoryDb;
use crate::inverter::decoder::decode_snapshot;
use crate::inverter::encoder::{ControlCommand, RegisterWrite};
use crate::inverter::model::{BatteryMode, DeviceType, InverterSnapshot};
use crate::inverter::sanitizer::{
    carry_forward_battery_modules_with, carry_forward_optional_block_values,
    derive_battery_fields_from_bms, is_block_suspicious, sanitize_snapshot, validate_battery_bms,
    ConsecutiveSuspectCounts, DeltaCorrectionCounts, GraceCumulativeSamples,
};
use crate::inverter::state_machines::{
    check_auto_winter, check_load_limiter, clear_cosy_slot_registers, cosy_slot_register_writes,
    persist_agile_state, persist_cosy_active, write_registers_to_inverter,
};
pub use crate::inverter::state_machines::{
    AgileState, AutoWinterConfig, AutoWinterSaved, AutoWinterState, LoadLimiterConfig,
    LoadLimiterState, PriceSlot,
};
use crate::modbus::client::ModbusClient;
use crate::modbus::registers::{HR_ENABLE_CHARGE, HR_ENABLE_CHARGE_TARGET};

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
    Snapshot(Box<InverterSnapshot>),
    /// The connection state has changed.
    Connection {
        /// New connection state.
        state: ConnectionState,
        /// Host we are connected to (or trying to reach).
        host: String,
        /// Epoch millis when the current connection was established (None if not connected).
        #[serde(skip_serializing_if = "Option::is_none")]
        connected_since_epoch_ms: Option<u64>,
    },
    /// EV charger data has been polled.
    Evc(Box<crate::evc::EvcSnapshot>),
    /// EV charger is disconnected.
    EvcDisconnected,
}

// ---------------------------------------------------------------------------
// Poll settings
// ---------------------------------------------------------------------------

// Agile price types (PriceSlot, AgileState) and the automation state-machine
// types (AutoWinter*, LoadLimiter*) live in [`state_machines`]. They are
// re-exported below so existing `crate::inverter::poll::*` references keep
// working.

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
    /// Monotonically increasing version - bumped by the settings API
    /// so the poll loop can detect that a reconnect is needed.
    pub version: u32,
    /// EV Charger IP address (standard Modbus TCP).
    pub evc_host: String,
    /// EV Charger TCP port (default 502).
    pub evc_port: u16,
    /// When true, skip auto-discovery of the dongle on persistent connection failure.
    pub disable_auto_discovery: bool,
}

impl Default for PollSettings {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 8899,
            serial: String::new(),
            interval_secs: 60,
            version: 0,
            evc_host: String::new(),
            evc_port: 502,
            disable_auto_discovery: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Shared application state
// ---------------------------------------------------------------------------

/// Shared state accessible from HTTP handlers, the WebSocket endpoint, etc.
pub struct AppState {
    /// Most recently decoded snapshot (or `None` if never polled).
    pub latest_snapshot: Arc<Mutex<Option<InverterSnapshot>>>,
    /// Current connection state (read by the status endpoint).
    pub connection_state: Arc<Mutex<ConnectionState>>,
    /// Broadcast sender - every poll cycle sends a [`PollMessage::Snapshot`]
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
    ///
    /// Uses `parking_lot::Mutex` (not `tokio::sync::Mutex`) because all
    /// operations are synchronous (lock/unlock within a single `.await`
    /// boundary) and `parking_lot` avoids the async Mutex's fairness
    /// overhead.
    ///
    /// # SAFETY (lock ordering)
    ///
    /// See the [module-level lock-ordering docs](crate::inverter::poll) for
    /// the invariant: this mutex MUST NOT be held across an `.await` point.
    /// It MAY be acquired while holding a `tokio::sync::Mutex` guard, but
    /// the `parking_lot` guard must be dropped before the next `.await`.
    pub connected_clients: Arc<parking_lot::Mutex<ConnectedClients>>,
    /// Auto winter mode configuration (volatile, can be synced to settings).
    pub auto_winter_config: Arc<Mutex<AutoWinterConfig>>,
    /// Auto winter mode state machine.
    pub auto_winter_state: Arc<Mutex<AutoWinterState>>,
    /// Saved register values to restore when winter mode deactivates.
    pub auto_winter_saved: Arc<Mutex<Option<AutoWinterSaved>>>,
    /// Load discharge limiter configuration.
    pub load_limiter_config: Arc<Mutex<LoadLimiterConfig>>,
    /// Load discharge limiter state machine.
    pub load_limiter_state: Arc<Mutex<LoadLimiterState>>,
    /// Whether cosy charging is currently active (force-charging in a slot).
    pub cosy_active: Arc<Mutex<bool>>,
    /// Agile Octopus state machine (Idle / Charging / Discharging).
    pub agile_state: Arc<Mutex<AgileState>>,
    /// Cached Octopus Agile prices for the current region.
    pub cached_agile_prices: Arc<Mutex<Vec<PriceSlot>>>,
    /// Most recently decoded EV charger snapshot.
    pub latest_evc: Arc<Mutex<Option<crate::evc::EvcSnapshot>>>,
    /// Email alert configuration.
    pub alert_config: Arc<Mutex<crate::settings::AlertsConfig>>,
    /// Email alert debounce tracker (in-memory only).
    pub alert_debounce: Arc<Mutex<crate::alerts::AlertDebounce>>,
    /// Last date a daily consumption report was sent.
    pub last_report_date: Arc<Mutex<Option<chrono::NaiveDate>>>,
    /// Wall-clock time when the current connection was established (None if disconnected).
    pub connected_since: Arc<std::sync::Mutex<Option<std::time::SystemTime>>>,
    /// How many consecutive TCP connect attempts have failed since the last success.
    pub connect_failures: Arc<std::sync::atomic::AtomicU32>,
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
            load_limiter_config: Arc::new(Mutex::new(LoadLimiterConfig::default())),
            load_limiter_state: Arc::new(Mutex::new(LoadLimiterState::default())),
            cosy_active: Arc::new(Mutex::new(
                crate::settings::Settings::load().cosy_active_persisted,
            )),
            agile_state: Arc::new(Mutex::new(AgileState::Idle)),
            cached_agile_prices: Arc::new(Mutex::new(Vec::new())),
            alert_config: Arc::new(Mutex::new(crate::settings::Settings::load().alerts_config)),
            alert_debounce: Arc::new(Mutex::new(crate::alerts::AlertDebounce::new())),
            last_report_date: Arc::new(Mutex::new(None)),
            latest_evc: Arc::new(Mutex::new(None)),
            connected_since: Arc::new(std::sync::Mutex::new(None)),
            connect_failures: Arc::new(std::sync::atomic::AtomicU32::new(0)),
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
            load_limiter_config: Arc::new(Mutex::new(LoadLimiterConfig::default())),
            load_limiter_state: Arc::new(Mutex::new(LoadLimiterState::default())),
            cosy_active: Arc::new(Mutex::new(
                crate::settings::Settings::load().cosy_active_persisted,
            )),
            agile_state: Arc::new(Mutex::new(AgileState::Idle)),
            cached_agile_prices: Arc::new(Mutex::new(Vec::new())),
            alert_config: Arc::new(Mutex::new(crate::settings::Settings::load().alerts_config)),
            alert_debounce: Arc::new(Mutex::new(crate::alerts::AlertDebounce::new())),
            last_report_date: Arc::new(Mutex::new(None)),
            latest_evc: Arc::new(Mutex::new(None)),
            connected_since: Arc::new(std::sync::Mutex::new(None)),
            connect_failures: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }
}

// ---------------------------------------------------------------------------
// Poll-cycle decision helpers
// ---------------------------------------------------------------------------

/// Whether the first successful model-detection poll should immediately
/// re-poll with the model-specific configuration.
///
/// Triggers a re-poll when the detected model needs a different operational
/// slave address, requests extra poll blocks (AC config / extended slots /
/// three-phase config), or needs gateway-specific input blocks. Without this,
/// model-specific registers can lag a full poll interval behind detection.
fn should_repoll_after_model_detection(device_type: DeviceType, current_slave: u8) -> bool {
    device_type.preferred_read_slave_address() != current_slave
        || !device_type.extra_poll_blocks().is_empty()
        || device_type.needs_gateway_input_blocks()
}

/// Whether to probe for external CT clamp meters on this cycle.
///
/// The discovery policy is:
/// - **First scan** (after model detection, before any probe): always runs.
/// - **If meters were found**: done - no further probing.
/// - **If no meters found AND `enable_ammeter` or EM115 is configured**:
///   retry every `METER_RETRY_INTERVAL` cycles, up to `METER_MAX_RETRIES`
///   times, because the meter may be slow to respond (e.g. LoRA-linked
///   EM115).
/// - **If no meters found AND ammeter is not configured**: one-shot scan,
///   then stop - nothing to find.
fn should_probe_external_meters(
    known_device_type: Option<DeviceType>,
    meter_probe_done: bool,
    enable_ammeter: bool,
    meter_type: u8,
    meter_retry_count: u8,
    meter_cycle_since_last: u8,
) -> bool {
    // Never probe until model is known and three-phase models skip external
    // meters (they use the inverter's internal grid CT at IR 1079-1082).
    // Batteryless devices (Gateway / EMS / PvInverter) also skip - the Gateway
    // has its own built-in grid meter (IR 1609); EMS/PvInverter have no battery
    // bus to instrument.
    let dt = match known_device_type {
        Some(dt) => dt,
        None => return false,
    };
    if dt.needs_three_phase_input_blocks() || dt.is_batteryless() {
        return false;
    }

    // First scan - always run.
    if !meter_probe_done {
        return true;
    }

    // Ammeter is expected but no meters found yet - retry on cadence.
    let ammeter_expected = enable_ammeter || meter_type == 1; // EM115 == 1
    if ammeter_expected
        && meter_retry_count < METER_MAX_RETRIES
        && meter_cycle_since_last >= METER_RETRY_INTERVAL
    {
        return true;
    }

    false
}

/// Maximum number of meter discovery retries after the initial scan fails
/// to find any meters despite the inverter being configured for an external
/// ammeter.
const METER_MAX_RETRIES: u8 = 10;

/// Retry meter discovery every N poll cycles.
const METER_RETRY_INTERVAL: u8 = 5;

/// Whether to probe for HV battery BCU stacks (0xA0 / 0x70+) on this cycle.
///
/// Only HV-capable device types use the BCU/BMU protocol; LV models answer at
/// 0x32 instead. The probe runs once after model detection, then the per-cycle
/// BCU cluster reads take over.
fn should_probe_hv_stacks(known_device_type: Option<DeviceType>, hv_probe_done: bool) -> bool {
    known_device_type
        .map(|dt| !hv_probe_done && dt.uses_hv_battery())
        .unwrap_or(false)
}
// ---------------------------------------------------------------------------
// Main poll loop
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
/// the first response frame header and persisted to settings - only the host
/// IP is required to connect.
pub async fn run_poll_loop(state: Arc<AppState>) {
    // Start the Telegram /status command poller
    crate::alerts::spawn_telegram_poller(state.clone());

    let mut backoff = Duration::from_secs(5);
    // Track consecutive connection failures to trigger auto-discovery.
    let mut consecutive_connect_failures: u32 = 0;
    // When we last ran LAN discovery (to avoid scanning too often).
    let mut last_discovery_time: Option<Instant> = None;
    // After this many consecutive failures, trigger auto-discovery.
    const DISCOVERY_AFTER_FAILURES: u32 = 5;
    // Minimum interval between auto-discovery scans.
    const DISCOVERY_COOLDOWN: Duration = Duration::from_secs(300);

    loop {
        // ---- Read current settings ----
        let settings = state.settings.lock().await.clone();

        // Wait until a host is configured. Serial may be empty - it will be
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

                // Reset auto-discovery state on successful connection.
                consecutive_connect_failures = 0;
                last_discovery_time = None;

                // Record connection timestamp for uptime tracking.
                let now = std::time::SystemTime::now();
                if let Ok(mut guard) = state.connected_since.lock() {
                    *guard = Some(now);
                }
                {
                    state
                        .connect_failures
                        .store(0, std::sync::atomic::Ordering::Relaxed);
                }

                // Convert to epoch millis for the frontend.
                let connected_since_epoch_ms = now
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis() as u64);

                // Broadcast connected state.
                {
                    let mut cs = state.connection_state.lock().await;
                    *cs = ConnectionState::Connected;
                }
                let _ = state.tx.send(PollMessage::Connection {
                    state: ConnectionState::Connected,
                    host: settings.host.clone(),
                    connected_since_epoch_ms,
                });

                // Allow the dongle time to initialise after TCP connect.
                // The GivEnergy dongle has a slow processor and may return
                // Modbus exception code 67 (busy/not-ready) if queried too soon.
                tokio::time::sleep(Duration::from_millis(500)).await;

                // Drain any stale data the dongle buffered from a previous
                // session - without this, cached responses corrupt the
                // request-response pairing for the first poll.

                // Warmup reads: discard the first register reads after connect.
                // The dongle's internal state can be stale after a TCP reconnect,
                // causing the first reads to return garbage values (e.g.
                // today_import_kwh = 0.6 when the real value is 39.0). We do
                // multiple warmup reads because a single discard isn't enough -
                // the dongle can return corrupted data for several reads.
                for i in 0..3 {
                    match client.read_all_standard().await {
                        Ok(blocks) => {
                            tracing::debug!(
                                "Warmup read {}/3 - OK ({} blocks)",
                                i + 1,
                                blocks.len()
                            );
                        }
                        Err(e) => {
                            tracing::warn!("Warmup read {}/3 - FAILED: {e}", i + 1,);
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }

                // Clear any previous snapshot so the next reading is accepted
                // without delta sanitization. After a reconnect, the previous
                // snapshot may contain stale or corrupted values from the old
                // session. The absolute range check (0-200 kWh) still applies.
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
                // Track persistent failures ACROSS intervals. consecutive_failures
                // resets within each interval, so a dead socket loops: 3 failures
                // → sleep → 3 failures → sleep forever. Now we break out to
                // reconnect immediately on the 3rd consecutive failure.
                // Track consecutive dongle memory-leak fingerprint hits.
                // After MAX_SUSPICIOUS_CYCLES, break to reconnect - persistent
                // corruption means the dongle has probably crashed.
                let mut consecutive_suspicious: u8 = 0;
                const MAX_SUSPICIOUS_CYCLES: u8 = 6;

                // Grace period: for the first few reads after connect, skip
                // delta sanitization. The dongle can return plausible-but-wrong
                // values (e.g. 0.6 kWh import when real is 39.0) that pass the
                // absolute range check but corrupt the "previous" reference.
                // After GRACE_READINGS the delta checks kick in.
                let mut readings_since_connect: u8 = 0;
                const GRACE_READINGS: u8 = 3;
                // Collect cumulative-counter samples during the grace period so
                // the delta-check baseline can be set to the median of the grace
                // readings rather than trusting whichever one happened to land
                // first. A single corrupted-but-in-range grace reading would
                // otherwise poison the baseline and cause every subsequent
                // correct lower reading to be rejected as a "decrease".
                let mut grace_cumulative_samples: Vec<GraceCumulativeSamples> = Vec::new();
                let mut pending_mode: Option<BatteryMode> = None;
                let mut delta_corrections = DeltaCorrectionCounts::default();
                let mut suspect_counts = ConsecutiveSuspectCounts::default();
                let mut known_device_type: Option<crate::inverter::model::DeviceType> = None;
                let mut detected_meters: Vec<u8> = Vec::new();
                let mut meter_probe_done = false;
                // Meter discovery retry state: when enable_ammeter or EM115 is
                // configured but the initial scan finds nothing, we retry every
                // METER_RETRY_INTERVAL cycles up to METER_MAX_RETRIES times.
                let mut meter_retry_count: u8 = 0;
                let mut meter_cycle_since_last: u8 = 0;
                // HV battery stacks discovered via the BMS (0xA0) / BCU (0x70+)
                // probe. Each entry is (bcu_offset, num_modules). Populated once
                // after model detection for devices that use the HV BCU protocol.
                let mut detected_hv_stacks: Vec<(u8, u8)> = Vec::new();
                let mut hv_probe_done = false;
                // Tracks PV energy computed from solar_power * delta_time
                // integration as the authoritative source for today_solar_kwh,
                // cross-checked against the (often corrupted) register value.
                let mut solar_energy_tracker: Option<(tokio::time::Instant, f32)> = None;
                // Tracks which Cosy slot index was last preloaded into the
                // inverter's charge slot registers. Only re-writes when the
                // "next upcoming slot" changes (e.g. after a slot ends).
                let mut cosy_last_preloaded_slot: Option<usize> = None;

                // Restore cosy_active from persisted settings on restart.
                // Without this, a client reboot during OR after a cosy slot
                // would leave the inverter in the previous force-charge state.
                // AppState::new already seeded `state.cosy_active` from
                // `cosy_active_persisted`; here we only log what we restored.
                {
                    let settings = crate::settings::Settings::load();
                    if settings.cosy_enabled && settings.cosy_active_persisted {
                        let now = chrono::Local::now();
                        let now_minutes = now.hour() as u16 * 60 + now.minute() as u16;
                        let in_slot =
                            crate::settings::cosy_active_slot(now_minutes, &settings.cosy_slots);
                        if in_slot.is_some() {
                            tracing::info!(
                                "Cosy: restart detected inside slot - force-charge will be re-sent on next poll"
                            );
                            // Reset the in-memory flag so the entry logic
                            // re-fires and re-sends the force-charge writes.
                            *state.cosy_active.lock().await = false;
                        } else {
                            tracing::info!(
                                "Cosy: restart detected AFTER slot ended - CosyExit will be sent on next poll to restore Eco mode"
                            );
                        }
                    }

                    // The in-memory `agile_state` always starts at Idle, so the
                    // Agile state machine will re-evaluate the current price and
                    // (re)send the appropriate command on the first poll. We log
                    // the persisted value here so a restart that left the inverter
                    // mid-charge/discharge is visible in the logs.
                    if settings.agile_enabled
                        && settings.agile_state_persisted != "idle"
                        && !settings.agile_state_persisted.is_empty()
                    {
                        tracing::info!(
                            persisted = %settings.agile_state_persisted,
                            "Agile: restart detected with active persisted state - will re-evaluate current price and re-send command on first poll"
                        );
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
                            "Settings changed (v{} → v{}) - reconnecting",
                            current_version,
                            settings_version_at_connect
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
                        for writes in &pending {
                            for w in writes {
                                match client.write_register(w.address, w.value).await {
                                    Ok(()) => {
                                        tracing::info!(
                                            "Wrote register {} = {}",
                                            w.address,
                                            w.value
                                        );
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            "Failed to write register {} = {}: {e}",
                                            w.address,
                                            w.value
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
                    }

                    // The consumer task handles stale frames - unmatched
                    // responses (including duplicate write ACKs) are silently
                    // dropped during the read cycle. No explicit flush needed.

                    let (poll_ok, sanitized, connection_lost) = async {
                        match client.read_all_with_extras(known_device_type.as_ref()).await {
                            Ok(blocks) => {
                                let mut snapshot = decode_snapshot(&blocks);

                                // Check all 60-register blocks against the known dongle
                                // memory-leak corruption fingerprint. If the dongle serves
                                // its own TCP/IP memory instead of register values, the
                                // entire poll cycle is suspect - trigger a re-poll.
                                let block_suspicious = blocks
                                    .iter()
                                    .any(|b| b.block.start % 60 == 0 && b.block.count == 60 && is_block_suspicious(&b.data));
                                if block_suspicious {
                                    for br in &blocks {
                                        if br.block.start % 60 == 0 && br.block.count == 60 && is_block_suspicious(&br.data) {
                                            tracing::warn!(
                                                block = br.block.name,
                                                start = br.block.start,
                                                "Block matched dongle memory-leak fingerprint - re-polling",
                                            );
                                        }
                                    }
                                }
                                if block_suspicious {
                                    consecutive_suspicious += 1;
                                    if consecutive_suspicious >= MAX_SUSPICIOUS_CYCLES {
                                        tracing::warn!(
                                            suspicious = consecutive_suspicious,
                                            max = MAX_SUSPICIOUS_CYCLES,
                                            "Persistent fingerprint corruption - reconnecting"
                                        );
                                    } else {
                                        tracing::warn!(
                                            suspicious = consecutive_suspicious,
                                            max = MAX_SUSPICIOUS_CYCLES,
                                            "Dongle memory-leak corruption detected - skipping broadcast, waiting for next poll cycle"
                                        );
                                    }
                                    return (true, false, false);
                                }
                                let has_ac_config_block = blocks.iter().any(|b| {
                                    b.block.register_type == crate::modbus::registers::RegisterType::Holding
                                        && b.block.start == 300
                                        && b.block.count == 60
                                });
                                let has_extended_slots_block = blocks.iter().any(|b| {
                                    b.block.register_type == crate::modbus::registers::RegisterType::Holding
                                        && b.block.start == 240
                                        && b.block.count == 60
                                });
                                let has_three_phase_config_block = blocks.iter().any(|b| {
                                    b.block.register_type == crate::modbus::registers::RegisterType::Holding
                                        && b.block.start == 1080
                                        && b.block.count == 45
                                });

                                // Cache the device type for subsequent polls.
                                // This enables model-aware polling (extra blocks).
                                // 'Unknown(0)' means we haven't identified the model yet.
                                let is_new_model = known_device_type.is_none()
                                    && !matches!(snapshot.device_type, crate::inverter::model::DeviceType::Unknown(_));
                                if is_new_model {
                                    tracing::info!(
                                        device_type = ?snapshot.device_type,
                                        extra_blocks = ?snapshot.device_type.extra_poll_blocks().iter().map(|b| b.name).collect::<Vec<_>>(),
                                        "Device model identified - enabling model-aware polling"
                                    );
                                    let preferred_slave = snapshot.device_type.preferred_read_slave_address();
                                    let slave_changed = preferred_slave != client.slave_address();
                                    let should_repoll = should_repoll_after_model_detection(
                                        snapshot.device_type,
                                        client.slave_address(),
                                    );
                                    if slave_changed {
                                        tracing::info!(
                                            from = client.slave_address(),
                                            to = preferred_slave,
                                            "Switching operational read slave address for detected model"
                                        );
                                        client.set_slave(preferred_slave);
                                    }

                                    // Three-phase models read 15+ blocks per cycle and
                                    // need a longer inter-request delay to avoid
                                    // overwhelming the dongle's slow processor.
                                    if snapshot.device_type.needs_three_phase_input_blocks() {
                                        tracing::info!(
                                            "Three-phase model detected - increasing inter-request delay to {}ms",
                                            ModbusClient::INTER_REQUEST_DELAY_3PH.as_millis()
                                        );
                                        client.set_inter_request_delay(
                                            ModbusClient::INTER_REQUEST_DELAY_3PH,
                                        );
                                    }

                                    let has_extra_blocks = !snapshot.device_type.extra_poll_blocks().is_empty();
                                    known_device_type = Some(snapshot.device_type);

                                    // The first detection poll is intentionally minimal: it discovers
                                    // the model, then immediately re-polls with the model-specific
                                    // slave address and optional blocks (AC HR300-359, Gen3 HR240-299).
                                    // Without this, AC-coupled HR313/314 limits can take a full poll
                                    // interval to appear after startup.
                                    if should_repoll {
                                        tracing::info!(
                                            slave_changed,
                                            has_extra_blocks,
                                            "Model-specific poll enabled - re-reading immediately"
                                        );
                                        return (true, true, false);
                                    }

                                } else if let Some(cached_type) = known_device_type {
                                    // Lock the device type to prevent dongle register corruption
                                    // (especially HR(21) arm_firmware_version) from flipping the
                                    // displayed model on a subsequent poll. Once identified, the
                                    // snapshot always carries the cached type - the decoder still
                                    // runs for the raw DTC and firmware string, but the refinement
                                    // result is ignored in favour of the known-good detection.
                                    if snapshot.device_type != cached_type {
                                        tracing::debug!(
                                            decoded = ?snapshot.device_type,
                                            cached = ?cached_type,
                                            "Device type mismatch - locking to cached value"
                                        );
                                        snapshot.device_type = cached_type;
                                        snapshot.device_type_display = cached_type.display_name().to_string();
                                    }
                                }

                                if should_probe_external_meters(
                                    known_device_type,
                                    meter_probe_done,
                                    snapshot.enable_ammeter,
                                    snapshot.meter_type,
                                    meter_retry_count,
                                    meter_cycle_since_last,
                                ) {
                                    // Probe for external CT clamp meters (device addresses 0x01-0x08).
                                    // Per givenergy-modbus, a meter is present when V_phase_1
                                    // (IR 60) is non-zero. Three-phase/HV models use the
                                    // inverter's internal grid CT at IR 1079-1082 instead of
                                    // separate external meters, so skip.
                                    //
                                    // Uses a short 3-second timeout with no retries for the
                                    // initial scan. If the inverter is configured for an
                                    // external ammeter but no meters are found, discovery is
                                    // retried on a slow cadence (every 5 cycles, up to 10
                                    // attempts) to handle LoRA-linked EM115 meters that may
                                    // be slow to respond.
                                    let is_retry = meter_probe_done;
                                    let ammeter_expected = snapshot.enable_ammeter || snapshot.meter_type == 1;
                                    if is_retry {
                                        tracing::info!(
                                            retry = meter_retry_count,
                                            max = METER_MAX_RETRIES,
                                            "Retrying external CT meter discovery (ammeter expected)"
                                        );
                                    } else {
                                        tracing::info!(
                                            enable_ammeter = snapshot.enable_ammeter,
                                            meter_type = snapshot.meter_type,
                                            device_type = ?snapshot.device_type,
                                            "Probing for external CT meters..."
                                        );
                                    }
                                    let mut found_meters: Vec<u8> = Vec::new();
                                    for &addr in crate::modbus::registers::METER_ADDRESSES {
                                        match client
                                            .probe_registers_at_slave(
                                                addr,
                                                crate::modbus::framer::RegisterType::Input,
                                                60,
                                                30,
                                                Duration::from_secs(3),
                                            )
                                            .await
                                        {
                                            Ok(data) => {
                                                let (valid, v1) = crate::inverter::decoder::validate_meter_data(&data);
                                                if valid {
                                                    let meter =
                                                        crate::inverter::decoder::decode_meter_data(
                                                            &data, addr,
                                                        );
                                                    tracing::info!(
                                                        "Meter detected at addr 0x{addr:02X}: {:.1}V, {:.0}W",
                                                        meter.v_phase_1,
                                                        meter.p_active_total
                                                    );
                                                    found_meters.push(addr);
                                                    snapshot.meters.push(meter);
                                                } else if v1 > 0.0 {
                                                    tracing::debug!(
                                                        "Meter addr 0x{addr:02X}: responded with implausible voltage ({v1:.1}V) - rejected"
                                                    );
                                                } else {
                                                    tracing::debug!(
                                                        "Meter addr 0x{addr:02X}: responded with zero voltage - no meter present"
                                                    );
                                                }
                                            }
                                            Err(e) => {
                                                tracing::debug!(
                                                    "Meter addr 0x{addr:02X}: no response: {e}",
                                                );
                                            }
                                        }
                                    }

                                    if !found_meters.is_empty() {
                                        // Merge with any previously detected meters.
                                        for addr in &found_meters {
                                            if !detected_meters.contains(addr) {
                                                detected_meters.push(*addr);
                                            }
                                        }
                                        meter_probe_done = true;
                                        meter_retry_count = 0;
                                        tracing::info!(
                                            "Detected {} meter(s) at addresses: {:02X?}",
                                            detected_meters.len(), detected_meters
                                        );
                                    } else if !meter_probe_done {
                                        // First scan found nothing.
                                        meter_probe_done = true;
                                        if ammeter_expected {
                                            tracing::info!(
                                                "No external CT meters detected on first scan - will retry (ammeter expected)"
                                            );
                                            // Don't increment retry_count yet; the first
                                            // retry happens after METER_RETRY_INTERVAL cycles.
                                            meter_cycle_since_last = 0;
                                        } else {
                                            tracing::info!("No external CT meters detected");
                                        }
                                    } else {
                                        // Retry scan found nothing.
                                        meter_retry_count += 1;
                                        meter_cycle_since_last = 0;
                                        if meter_retry_count >= METER_MAX_RETRIES {
                                            tracing::warn!(
                                                retries = meter_retry_count,
                                                "Meter discovery exhausted all retries - external ammeter configured but no meter responding"
                                            );
                                        } else {
                                            tracing::info!(
                                                retry = meter_retry_count,
                                                max = METER_MAX_RETRIES,
                                                "No external CT meters found - will retry"
                                            );
                                        }
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
                                        "Auto-discovered serial is suspect (truncated frame) - keeping empty serial for all requests. If the connection fails, try setting the serial manually in Settings."
                                    );
                                }

                                // Once the model is identified, freeze the device type in the
                                // snapshot to prevent a corrupted ARM firmware register read (HR 21)
                                // from flipping the displayed model on subsequent polls. The
                                // dongle occasionally returns garbage for any register.
                                if let Some(kdt) = known_device_type {
                                    snapshot.device_type = kdt;
                                    snapshot.device_type_display = kdt.display_name().to_string();
                                    snapshot.max_charge_slots = kdt.max_charge_slots();
                                    snapshot.max_discharge_slots = kdt.max_discharge_slots();
                                }

                                // Populated by the HV battery path below; consumed by
                                // derive_battery_fields_from_bms(). Hoisted here so it
                                // is available after the batteryless-skip block.
                                let mut hv_cluster: Option<
                                    crate::inverter::decoder::HvBcuCluster,
                                > = None;

                                // --- Battery BMS module reads ---
                                //
                                // Two distinct battery protocols exist in the GivEnergy
                                // ecosystem (per givenergy-modbus model/hv_bcu.py and GivTCP):
                                //
                                //   LV packs:     BMS at 0x32 (battery #1) + 0x33-0x37, IR 60-119
                                //   HV stacks:    BCU at 0x70+i (cluster) + BMU at 0x50+m, IR 60-119
                                //
                                // HV stackable batteries (e.g. GIV-BAT-3.4-HV modules) do NOT
                                // answer at 0x32. Device type decides which path runs.
                                // Batteryless devices (Gateway, EMS, PvInverter) skip entirely
                                // - they have no directly-attached battery to probe.
                                if known_device_type.is_some_and(|dt| dt.is_batteryless()) {
                                    // Batteryless device (Gateway / EMS / PvInverter):
                                    // no directly-attached battery to probe. The Gateway
                                    // aggregation bank decoder populates battery fields;
                                    // EMS/PvInverter have none.
                                } else {
                                let is_hv = known_device_type
                                    .map(|dt| dt.uses_hv_battery())
                                    .unwrap_or(false);
                                if is_hv {
                                    // --- HV battery: BCU cluster read ---
                                    //
                                    // Discover the BCU layout once (via the BMS at 0xA0),
                                    // then read each stack's cluster block every cycle.
                                    if should_probe_hv_stacks(known_device_type, hv_probe_done) {
                                        tracing::info!("Probing for HV battery BCU stacks...");
                                        let mut found: Vec<(u8, u8)> = Vec::new();
                                        // BMS at 0xA0 reports the number of BCUs at IR(61).
                                        match client
                                            .read_registers_at_slave(
                                                crate::modbus::registers::HV_BMS_ADDRESS,
                                                crate::modbus::framer::RegisterType::Input,
                                                60,
                                                5,
                                            )
                                            .await
                                        {
                                            Ok(bms) => {
                                                let num_bcus = *bms.get(1).unwrap_or(&0) as u8;
                                                tracing::info!(
                                                    num_bcus,
                                                    "BMS reports {num_bcus} HV BCU stack(s)"
                                                );
                                                for offset in 0..num_bcus {
                                                    // Each BCU's IR(64) holds its module count.
                                                    let bcu_addr = crate::modbus::registers::
                                                        HV_BCU_BASE_ADDRESS.wrapping_add(offset);
                                                    match client
                                                        .read_registers_at_slave(
                                                            bcu_addr,
                                                            crate::modbus::framer::RegisterType::Input,
                                                            60,
                                                            60,
                                                        )
                                                        .await
                                                    {
                                                        Ok(data)
                                                            if crate::inverter::decoder::
                                                                validate_hv_bcu(&data) =>
                                                        {
                                                            let cluster =
                                                                crate::inverter::decoder::
                                                                    decode_hv_bcu_cluster(&data);
                                                            tracing::info!(
                                                                bcu_offset = offset,
                                                                modules = cluster.number_of_modules,
                                                                version = %cluster.pack_software_version,
                                                                "HV BCU at 0x{bcu_addr:02X} - {} modules",
                                                                cluster.number_of_modules
                                                            );
                                                            found.push((
                                                                offset,
                                                                cluster.number_of_modules as u8,
                                                            ));
                                                        }
                                                        Ok(_) => {
                                                            tracing::debug!(
                                                                bcu_offset = offset,
                                                                "BCU 0x{bcu_addr:02X} probe: invalid version - no stack"
                                                            );
                                                        }
                                                        Err(e) => {
                                                            tracing::debug!(
                                                                bcu_offset = offset,
                                                                "BCU 0x{bcu_addr:02X} probe: no response: {e}"
                                                            );
                                                        }
                                                    }
                                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                                }
                                            }
                                            Err(e) => {
                                                tracing::debug!(
                                                    "BMS 0xA0 probe failed: {e} - falling back to direct BCU 0x70 probe"
                                                );
                                                // Fallback: probe BCU 0x70 directly (single-stack
                                                // installs where the BMS aggregation isn't exposed).
                                                if let Ok(data) = client
                                                    .read_registers_at_slave(
                                                        crate::modbus::registers::HV_BCU_BASE_ADDRESS,
                                                        crate::modbus::framer::RegisterType::Input,
                                                        60,
                                                        60,
                                                    )
                                                    .await
                                                {
                                                    if crate::inverter::decoder::validate_hv_bcu(&data)
                                                    {
                                                        let cluster =
                                                            crate::inverter::decoder::
                                                                decode_hv_bcu_cluster(&data);
                                                        found.push((0, cluster.number_of_modules as u8));
                                                    }
                                                }
                                            }
                                        }
                                        detected_hv_stacks = found;
                                        hv_probe_done = true;
                                        if detected_hv_stacks.is_empty() {
                                            tracing::info!("No HV battery BCU stacks detected");
                                        } else {
                                            tracing::info!(
                                                "Detected {} HV BCU stack(s): {:?}",
                                                detected_hv_stacks.len(),
                                                detected_hv_stacks
                                            );
                                        }
                                    }

                                    // Read each detected stack's cluster block this cycle.
                                    for &(offset, _modules) in &detected_hv_stacks {
                                        let bcu_addr = crate::modbus::registers::HV_BCU_BASE_ADDRESS
                                            .wrapping_add(offset);
                                        match client
                                            .read_registers_at_slave(
                                                bcu_addr,
                                                crate::modbus::framer::RegisterType::Input,
                                                60,
                                                60,
                                            )
                                            .await
                                        {
                                            Ok(data)
                                                if crate::inverter::decoder::validate_hv_bcu(&data) =>
                                            {
                                                let cluster =
                                                    crate::inverter::decoder::decode_hv_bcu_cluster(
                                                        &data,
                                                    );
                                                tracing::debug!(
                                                    bcu_offset = offset,
                                                    voltage = cluster.battery_voltage,
                                                    current = cluster.battery_current,
                                                    modules = cluster.number_of_modules,
                                                    "HV BCU cluster read OK"
                                                );
                                                if hv_cluster.is_none() {
                                                    hv_cluster = Some(cluster);
                                                }
                                            }
                                            Ok(_) => {
                                                tracing::debug!(
                                                    bcu_offset = offset,
                                                    "HV BCU 0x{bcu_addr:02X} read: invalid version"
                                                );
                                            }
                                            Err(e) => {
                                                tracing::debug!(
                                                    bcu_offset = offset,
                                                    "HV BCU 0x{bcu_addr:02X} read failed: {e}"
                                                );
                                            }
                                        }
                                    }

                                    // --- HV battery: BMU per-module cell reads ---
                                    //
                                    // Each BMU (device 0x50+m) exposes one module's
                                    // cell-level data for the Battery page. The read base
                                    // shifts by 120*bcu_offset so the returned slice
                                    // always starts at v_cell_01 (per GivTCP's read
                                    // convention; givenergy-modbus resolves the same
                                    // layout via the BMU stride within a BCU).
                                    let mut module_index: usize = 0;
                                    for &(offset, num_modules) in &detected_hv_stacks {
                                        let base = 60u16 + 120u16 * offset as u16;
                                        for bmu_num in 0..num_modules {
                                            let bmu_addr = crate::modbus::registers::
                                                HV_BMU_BASE_ADDRESS.wrapping_add(bmu_num);
                                            match client
                                                .read_registers_at_slave(
                                                    bmu_addr,
                                                    crate::modbus::framer::RegisterType::Input,
                                                    base,
                                                    60,
                                                )
                                                .await
                                            {
                                                Ok(data)
                                                    if crate::inverter::decoder::
                                                        validate_hv_bmu(&data) =>
                                                {
                                                    let module = crate::inverter::decoder::
                                                        decode_hv_bmu_block(&data, module_index);
                                                    tracing::debug!(
                                                        bcu_offset = offset,
                                                        bmu = bmu_num,
                                                        module = module_index,
                                                        cells = module.cell_voltages.len(),
                                                        voltage = module.voltage,
                                                        "HV BMU read OK"
                                                    );
                                                    snapshot.battery_modules.push(module);
                                                }
                                                Ok(_) => {
                                                    tracing::debug!(
                                                        bcu_offset = offset,
                                                        bmu = bmu_num,
                                                        "HV BMU 0x{bmu_addr:02X}: invalid serial - not present"
                                                    );
                                                }
                                                Err(e) => {
                                                    tracing::debug!(
                                                        bcu_offset = offset,
                                                        bmu = bmu_num,
                                                        "HV BMU 0x{bmu_addr:02X}: no response: {e}"
                                                    );
                                                }
                                            }
                                            module_index += 1;
                                            tokio::time::sleep(Duration::from_millis(100)).await;
                                        }
                                    }

                                    // HV BMU modules do not expose a per-module SOC register
                                    // (confirmed against GivTCP's hvbmu.py - the BMU bank is
                                    // cell voltages, cell temps and serial only). The BCU
                                    // cluster reports the stack-wide SOC spread and per-module
                                    // Ah capacity, which we backfill onto each module so the
                                    // Battery page shows a sensible non-zero per-module SOC
                                    // and capacity instead of 0%.
                                    if let Some(cluster) = &hv_cluster {
                                        crate::inverter::decoder::backfill_hv_module_fields(
                                            &mut snapshot.battery_modules,
                                            cluster,
                                        );
                                    }
                                } else {
                                    // --- LV battery: BMS pack reads ---
                                    //
                                    // Per givenergy-modbus reference, LV batteries expose BMS
                                    // data on the inverter's IR 60-119 at device address 0x32
                                    // (battery #1) and additional batteries at 0x33, 0x34, ... 0x37.
                                    // Battery #1 IR 60-119 is NOT part of the standard poll
                                    // blocks (those only read IR 0-59), so we issue a separate
                                    // read here. Additional batteries also need separate reads
                                    // at their own device addresses.

                                    // Read battery #1 BMS (device 0x32, IR 60-119).
                                    // Do not use the model-specific operational read address
                                    // here: AC/Gen1 switch to 0x31 and newer models use 0x11,
                                    // while the first LV battery BMS cache remains exposed at
                                    // 0x32.
                                    match client
                                        .read_registers_at_slave(
                                            0x32,
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
                                                        "Battery addr 0x{:02X}: SOC={} - not present",
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
                                            "Inverter SOC was 0 - aggregate from {} modules: {}%",
                                            snapshot.battery_modules.len(),
                                            snapshot.soc
                                        );
                                    }
                                }

                                // Override battery temperature from BMS data for all
                                // device types (IR(56) is frequently garbage - #48).
                                // For three-phase inverters, also derives battery capacity
                                // and max power from the BMS data since those are absent
                                // from the inverter register blocks entirely.
                                derive_battery_fields_from_bms(&mut snapshot, hv_cluster.as_ref());

                                // Determine battery calibration support based on actual BMS
                                // firmware from the first LV battery module, not inverter type.
                                // Gen3+ batteries (bms_firmware >= 3000) auto-calibrate via BMS OCV
                                // and should not be manually calibrated via HR(29).
                                // Falls back to device type for HV stacks (bms_firmware=0) or when
                                // no battery modules are present.
                                snapshot.supports_battery_calibration = if let Some(bms) = snapshot.battery_modules.first() {
                                    if bms.bms_firmware > 0 {
                                        bms.bms_firmware < 3000
                                    } else {
                                        // No BMS firmware reported (HV stacks, or read failed).
                                        // Fall back to device type - Gen3+ types don't need it.
                                        snapshot.device_type.supports_manual_battery_calibration()
                                    }
                                } else {
                                    false // No battery modules - no calibration
                                };

                                // Store latest snapshot.
                                // Sanitize against physically impossible values first.
                                // Skip delta checks during the grace period after connect.
                                let in_grace = readings_since_connect < GRACE_READINGS;
                                let (sanitized, prev_modules) = {
                                    let prev = state.latest_snapshot.lock().await;
                                    let mut s = sanitize_snapshot(&mut snapshot, prev.as_ref(), in_grace, &mut pending_mode, &mut delta_corrections, &mut suspect_counts);
                                    if carry_forward_optional_block_values(
                                        &mut snapshot,
                                        prev.as_ref(),
                                        has_ac_config_block,
                                        has_extended_slots_block,
                                        has_three_phase_config_block,
                                    ) {
                                        s = true;
                                    }
                                    let mods = prev.as_ref().map(|p| p.battery_modules.clone());
                                    (s, mods)
                                };
                                carry_forward_battery_modules_with(&mut snapshot, prev_modules.as_deref());

                                // Grace-period baseline hardening: capture this
                                // reading's cumulative counters, and on the final
                                // grace reading replace them with the median of all
                                // grace samples. This prevents a single corrupted
                                // grace reading from poisoning the delta baseline.
                                if in_grace {
                                    grace_cumulative_samples
                                        .push(GraceCumulativeSamples::from_snapshot(&snapshot));
                                    if readings_since_connect == GRACE_READINGS - 1
                                        && grace_cumulative_samples.len() >= 2
                                    {
                                        let median =
                                            GraceCumulativeSamples::median(&grace_cumulative_samples);
                                        tracing::info!(
                                            n = grace_cumulative_samples.len(),
                                            consumption_samples = ?grace_cumulative_samples
                                                .iter()
                                                .map(|s| s.today_consumption_kwh)
                                                .collect::<Vec<_>>(),
                                            median_consumption = median.today_consumption_kwh,
                                            "Grace period complete - cumulative baseline set to median of grace readings"
                                        );
                                        median.apply_to(&mut snapshot);
                                    }
                                }

                                readings_since_connect = readings_since_connect.saturating_add(1);

                                if readings_since_connect == 1 {
                                    tracing::info!(
                                        soc = snapshot.soc,
                                        solar_w = snapshot.solar_power,
                                        battery_w = snapshot.battery_power,
                                        grid_w = snapshot.grid_power,
                                        "First poll read after connect - data is flowing"
                                    );
                                }
                                // Load settings from disk on a blocking thread
                                // so synchronous file I/O doesn't stall the poll
                                // loop on slow/networked filesystems.
                                let poll_settings = tokio::task::spawn_blocking(
                                    crate::settings::Settings::load,
                                )
                                .await
                                .unwrap_or_default();

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
                                    // Load cosy_enabled from settings so the frontend
                                    // knows cosy is configured even between slots.
                                    // (cosy_active is set later, AFTER the cosy state
                                    // machine runs, so the broadcast reflects the
                                    // post-transition value.)
                                    snapshot.cosy_enabled = poll_settings.cosy_enabled;
                                    snapshot.agile_enabled = poll_settings.agile_enabled;

                                    // Persist saved values to disk so they survive a
                                    // restart. When winter mode deactivates, saved
                                    // becomes None - this clears the persisted values.
                                    let persist_saved = saved.clone();
                                    drop(config);
                                    drop(aw_state);
                                    drop(saved);

                                    let mut app_settings = poll_settings.clone();
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

                                // ---- Load discharge limiter ----
                                {
                                    let config = state.load_limiter_config.lock().await;
                                    let mut ll_state = state.load_limiter_state.lock().await;
                                    let writes = check_load_limiter(
                                        &snapshot,
                                        &config,
                                        &mut ll_state,
                                        poll_settings.poll_interval,
                                    );

                                    // Tag the snapshot so the frontend knows.
                                    snapshot.load_limiter_active =
                                        matches!(*ll_state, LoadLimiterState::Paused)
                                        || matches!(*ll_state, LoadLimiterState::PausedFromRestart);

                                    let was_active = poll_settings.load_limiter_active_persisted;
                                    let now_active = snapshot.load_limiter_active;
                                    drop(config);
                                    drop(ll_state);

                                    // Persist active flag to disk so a crash/restart can detect it.
                                    if was_active != now_active {
                                        let mut app_settings = poll_settings.clone();
                                        app_settings.load_limiter_active_persisted = now_active;
                                        if let Err(e) = app_settings.save() {
                                            tracing::warn!("Failed to persist load limiter state: {e}");
                                        }
                                    }

                                    if let Some(writes) = writes {
                                        for w in &writes {
                                            match client.write_register(w.address, w.value).await {
                                                Ok(()) => tracing::info!(
                                                    "Load limiter: wrote reg {} = {}",
                                                    w.address, w.value
                                                ),
                                                Err(e) => tracing::error!(
                                                    "Load limiter: write reg {} failed: {e}",
                                                    w.address
                                                ),
                                            }
                                            tokio::time::sleep(Duration::from_millis(1500)).await;
                                        }
                                    }
                                }

                                // ---- Cosy charging mode ----
                                //
                                // Writes Cosy slot schedules into the inverter's own charge slot
                                // registers so the inverter follows the schedule independently.
                                //
                                // When a Cosy slot is ACTIVE: writes the current slot times +
                                // enable_charge + target SOC to the inverter.
                                //
                                // When no Cosy slot is active: preloads the NEXT upcoming slot's
                                // times into the inverter registers (with enable_charge=0) so the
                                // inverter has the schedule ready. If there's no next slot, clears
                                // the registers.
                                //
                                // This means if the app crashes, the inverter already has the
                                // correct schedule loaded and can act on it.
                                {
                                    let settings = &poll_settings;
                                    let now = chrono::Local::now();
                                    let now_minutes = now.hour() as u16 * 60 + now.minute() as u16;

                                    // Check if we're inside any enabled cosy slot. When cosy mode is
                                    // disabled, treat as "not in slot" so any lingering cosy_active
                                    // flag gets cleared on the next poll (otherwise the inverter stays
                                    // force-charging after switching away from Cosy mode).
                                    let current_slot = if settings.cosy_enabled {
                                        settings.cosy_slots.iter().enumerate().find(|(_, s)| s.enabled && s.contains_minutes(now_minutes))
                                    } else {
                                        None
                                    };
                                    let in_slot = current_slot.is_some();

                                    let cosy_active = state.cosy_active.lock().await;
                                    if in_slot && !*cosy_active {
                                        // ---- Entering a cosy slot ----
                                        // Write the active slot's times into the inverter's charge
                                        // slot registers and enable charging.
                                        let (slot_idx, cosy_slot) = current_slot.unwrap();
                                        tracing::info!(
                                            "Cosy: entering slot {} ({}:{:02}-{}:{:02}), target SOC {}%",
                                            slot_idx,
                                            cosy_slot.start_hour, cosy_slot.start_minute,
                                            cosy_slot.end_hour, cosy_slot.end_minute,
                                            cosy_slot.target_soc
                                        );
                                        drop(cosy_active);

                                        let writes = cosy_slot_register_writes(
                                            cosy_slot, snapshot.device_type, true,
                                        );
                                        let ok = write_registers_to_inverter(
                                            &mut client, &writes, "Cosy enter",
                                        ).await;

                                        if ok {
                                            *state.cosy_active.lock().await = true;
                                            persist_cosy_active(true);
                                            // Mark the preloaded slot as stale since we're now active.
                                            cosy_last_preloaded_slot = None;
                                        } else {
                                            tracing::warn!("Cosy: enter writes failed - will retry on next poll");
                                        }
                                    } else if *cosy_active && !in_slot {
                                        // ---- Exiting a cosy slot ----
                                        // Disable charging and preload the next upcoming slot's
                                        // times (or clear if no next slot).
                                        tracing::info!("Cosy: exiting slot, restoring Eco mode");
                                        drop(cosy_active);

                                        // First, disable charge and charge target.
                                        let mut writes = vec![
                                            RegisterWrite { address: HR_ENABLE_CHARGE, value: 0 },
                                            RegisterWrite { address: HR_ENABLE_CHARGE_TARGET, value: 0 },
                                        ];
                                        // For three-phase models, also clear force flags.
                                        if snapshot.device_type.uses_three_phase_schedule_slots() {
                                            use crate::modbus::registers::{
                                                HR_3PH_FORCE_CHARGE_ENABLE,
                                                HR_3PH_AC_CHARGE_ENABLE,
                                                HR_3PH_FORCE_DISCHARGE_ENABLE,
                                            };
                                            writes.push(RegisterWrite { address: HR_3PH_FORCE_CHARGE_ENABLE, value: 0 });
                                            writes.push(RegisterWrite { address: HR_3PH_AC_CHARGE_ENABLE, value: 0 });
                                            writes.push(RegisterWrite { address: HR_3PH_FORCE_DISCHARGE_ENABLE, value: 0 });
                                        }
                                        // Restore eco mode.
                                        use crate::modbus::registers::HR_BATTERY_POWER_MODE;
                                        writes.push(RegisterWrite { address: HR_BATTERY_POWER_MODE, value: 1 });
                                        // Also clear enable_discharge to match CosyExit behaviour.
                                        use crate::modbus::registers::HR_ENABLE_DISCHARGE;
                                        writes.push(RegisterWrite { address: HR_ENABLE_DISCHARGE, value: 0 });

                                        // Now preload the next upcoming slot's times (with
                                        // enable_charge=0 so the inverter doesn't act on it yet).
                                        if settings.cosy_enabled {
                                            let next = crate::settings::find_next_cosy_slot(
                                                now_minutes, &settings.cosy_slots,
                                            );
                                            if let Some((next_idx, next_slot, minutes_until)) = next {
                                                tracing::info!(
                                                    "Cosy: preloading next slot {} ({}:{:02}-{}:{:02}) in {} min",
                                                    next_idx,
                                                    next_slot.start_hour, next_slot.start_minute,
                                                    next_slot.end_hour, next_slot.end_minute,
                                                    minutes_until
                                                );
                                                writes.extend(cosy_slot_register_writes(
                                                    next_slot, snapshot.device_type, false,
                                                ));
                                            } else {
                                                tracing::info!("Cosy: no upcoming slot - clearing charge slot registers");
                                                writes.extend(clear_cosy_slot_registers(snapshot.device_type));
                                            }
                                        } else {
                                            // Cosy mode was disabled while active - clear registers.
                                            writes.extend(clear_cosy_slot_registers(snapshot.device_type));
                                        }

                                        let ok = write_registers_to_inverter(
                                            &mut client, &writes, "Cosy exit",
                                        ).await;

                                        if ok {
                                            *state.cosy_active.lock().await = false;
                                            persist_cosy_active(false);
                                            // Update the preloaded tracker to the next slot (or None).
                                            cosy_last_preloaded_slot = if settings.cosy_enabled {
                                                crate::settings::find_next_cosy_slot(
                                                    now_minutes, &settings.cosy_slots,
                                                ).map(|(idx, _, _)| idx)
                                            } else {
                                                None
                                            };
                                        } else {
                                            tracing::warn!("Cosy: exit writes failed - will retry on next poll");
                                        }
                                    } else if !in_slot && !*cosy_active {
                                        // ---- Idle: ensure the next upcoming slot is preloaded ----
                                        // Only re-writes when the "next upcoming slot" index changes
                                        // (e.g. after a slot ends or on first poll after connect).
                                        drop(cosy_active);
                                        if settings.cosy_enabled {
                                            let next = crate::settings::find_next_cosy_slot(
                                                now_minutes, &settings.cosy_slots,
                                            );
                                            let next_idx = next.as_ref().map(|(idx, _, _)| *idx);
                                            // Only write when the next slot changes or on first poll.
                                            if next_idx != cosy_last_preloaded_slot {
                                                if let Some((next_idx, next_slot, minutes_until)) = next {
                                                    tracing::info!(
                                                        "Cosy: preloading next slot {} ({}:{:02}-{}:{:02}) in {} min",
                                                        next_idx,
                                                        next_slot.start_hour, next_slot.start_minute,
                                                        next_slot.end_hour, next_slot.end_minute,
                                                        minutes_until
                                                    );
                                                    let writes = cosy_slot_register_writes(
                                                        next_slot, snapshot.device_type, false,
                                                    );
                                                    let ok = write_registers_to_inverter(
                                                        &mut client, &writes, "Cosy preload",
                                                    ).await;
                                                    if ok {
                                                        cosy_last_preloaded_slot = Some(next_idx);
                                                    }
                                                } else {
                                                    // No upcoming slot - clear registers if they were set.
                                                    if cosy_last_preloaded_slot.is_some() {
                                                        tracing::info!("Cosy: no upcoming slot - clearing charge slot registers");
                                                        let writes = clear_cosy_slot_registers(snapshot.device_type);
                                                        let ok = write_registers_to_inverter(
                                                            &mut client, &writes, "Cosy clear",
                                                        ).await;
                                                        if ok {
                                                            cosy_last_preloaded_slot = None;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    } else {
                                        // Already in an active cosy slot - nothing to do.
                                        drop(cosy_active);
                                    }
                                }

                                // ---- Agile Octopus mode ----
                                {
                                    let settings = &poll_settings;
                                    if settings.agile_enabled {
                                        // Find current price from cache, or refresh
                                        let now_ts = chrono::Utc::now().timestamp();
                                        let prices = state.cached_agile_prices.lock().await;
                                        let current_price = prices.iter().find(|s| now_ts >= s.valid_from && now_ts < s.valid_to).map(|s| s.pence);

                                        let price = if current_price.is_some() {
                                            current_price
                                        } else {
                                            // Cache miss - fetch fresh prices from Octopus API.
                                            // Anchor to the start of TODAY (UTC) so the response always
                                            // includes the current slot. The Agile endpoint returns
                                            // results newest-first, so a bare page_size=48 returns
                                            // tomorrow's slots once they're published (~1pm) and the
                                            // current slot drops out of the window - which silently
                                            // leaves the state machine Idle and never discharges.
                                            drop(prices);
                                            let region = &settings.agile_region;
                                            let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
                                            let url = format!(
                                                "https://api.octopus.energy/v1/products/AGILE-24-10-01/electricity-tariffs/E-1R-AGILE-24-10-01-{region}/standard-unit-rates/?period_from={today}T00:00:00Z&page_size=96"
                                            );
                                            let fetch_result = tokio::task::spawn_blocking(move || -> Result<Vec<PriceSlot>, String> {
                                                let mut resp = ureq::get(&url)
                                                    .call()
                                                    .map_err(|e| format!("HTTP error: {e}"))?;
                                                let body = resp.body_mut().read_to_string()
                                                    .map_err(|e| format!("read error: {e}"))?;
                                                let json: serde_json::Value = serde_json::from_str(&body)
                                                    .map_err(|e| format!("JSON error: {e}"))?;
                                                let results = json["results"]
                                                    .as_array()
                                                    .ok_or_else(|| "missing results".to_string())?;
                                                let slots: Vec<PriceSlot> = results
                                                    .iter()
                                                    .filter_map(|r| {
                                                        let pence = r["value_inc_vat"].as_f64()?;
                                                        let from = r["valid_from"].as_str()?;
                                                        let to = r["valid_to"].as_str()?;
                                                        let from_ts = chrono::DateTime::parse_from_rfc3339(from).ok()?.timestamp();
                                                        let to_ts = chrono::DateTime::parse_from_rfc3339(to).ok()?.timestamp();
                                                        Some(PriceSlot { pence, valid_from: from_ts, valid_to: to_ts })
                                                    })
                                                    .collect();
                                                Ok(slots)
                                            }).await;

                                            match fetch_result {
                                                Ok(Ok(fresh)) => {
                                                    let mut prices = state.cached_agile_prices.lock().await;
                                                    *prices = fresh;
                                                    prices.iter().find(|s| now_ts >= s.valid_from && now_ts < s.valid_to).map(|s| s.pence)
                                                }
                                                Ok(Err(e)) => {
                                                    tracing::warn!("Agile: failed to fetch prices: {e}");
                                                    None
                                                }
                                                Err(e) => {
                                                    tracing::error!("Agile: spawn_blocking failed: {e}");
                                                    None
                                                }
                                            }
                                        };

                                        if let Some(price) = price {
                                            let charge_threshold = settings.agile_charge_threshold;
                                            let discharge_threshold = settings.agile_discharge_threshold;

                                            let ag_state = state.agile_state.lock().await;
                                            tracing::debug!(
                                                price,
                                                charge_threshold,
                                                discharge_threshold,
                                                state = ?*ag_state,
                                                inverter_mode = ?snapshot.battery_mode,
                                                "Agile: evaluating current slot",
                                            );

                                            if price <= charge_threshold {
                                                if *ag_state != AgileState::Charging {
                                                    // Enter charge mode
                                                                                    tracing::info!("Agile: price {price}p ≤ {charge_threshold}p - force charging");
                                                    drop(ag_state);
                                                    let use_3ph = snapshot.device_type.uses_three_phase_schedule_slots();
                                                    let cmd = if use_3ph {
                                                        ControlCommand::ThreePhaseForceCharge { target_soc: 100 }
                                                    } else {
                                                        ControlCommand::ForceCharge { target_soc: 100 }
                                                    };
                                                    let mut all_ok = true;
                                                    if let Ok(writes) = cmd.encode() {
                                                        for w in &writes {
                                                            if let Err(e) = client.write_register(w.address, w.value).await {
                                                                tracing::error!("Agile: write reg {} failed: {e}", w.address);
                                                                all_ok = false;
                                                            }
                                                            tokio::time::sleep(Duration::from_millis(1500)).await;
                                                        }
                                                    } else {
                                                        all_ok = false;
                                                    }
                                                    if all_ok {
                                                        *state.agile_state.lock().await = AgileState::Charging;
                                                        persist_agile_state(AgileState::Charging);
                                                    }
                                                }
                                            } else if price >= discharge_threshold {
                                                if *ag_state != AgileState::Discharging {
                                                    // Enter discharge mode
                                                                                    tracing::info!("Agile: price {price}p ≥ {discharge_threshold}p - force discharging");
                                                    drop(ag_state);
                                                    let use_3ph = snapshot.device_type.uses_three_phase_schedule_slots();
                                                    let cmd = if use_3ph {
                                                        ControlCommand::ThreePhaseForceDischarge
                                                    } else {
                                                        ControlCommand::ForceDischarge
                                                    };
                                                    let mut all_ok = true;
                                                    if let Ok(writes) = cmd.encode() {
                                                        for w in &writes {
                                                            if let Err(e) = client.write_register(w.address, w.value).await {
                                                                tracing::error!("Agile: write reg {} failed: {e}", w.address);
                                                                all_ok = false;
                                                            }
                                                            tokio::time::sleep(Duration::from_millis(1500)).await;
                                                        }
                                                    } else {
                                                        all_ok = false;
                                                    }
                                                    if all_ok {
                                                        *state.agile_state.lock().await = AgileState::Discharging;
                                                        persist_agile_state(AgileState::Discharging);
                                                    }
                                                }
                                            } else {
                                                // Hold - price between thresholds: revert to Eco mode
                                                if *ag_state != AgileState::Idle {
                                                                                    tracing::info!("Agile: hold (price {price}p), reverting to Eco");
                                                    drop(ag_state);
                                                    let use_3ph = snapshot.device_type.uses_three_phase_schedule_slots();
                                                    let cmd = if use_3ph {
                                                        ControlCommand::ThreePhaseCosyExit
                                                    } else {
                                                        ControlCommand::CosyExit
                                                    };
                                                    let mut all_ok = true;
                                                    if let Ok(writes) = cmd.encode() {
                                                        for w in &writes {
                                                            if let Err(e) = client.write_register(w.address, w.value).await {
                                                                tracing::error!("Agile: write reg {} failed: {e}", w.address);
                                                                all_ok = false;
                                                            }
                                                            tokio::time::sleep(Duration::from_millis(1500)).await;
                                                        }
                                                    } else {
                                                        all_ok = false;
                                                    }
                                                    if all_ok {
                                                        *state.agile_state.lock().await = AgileState::Idle;
                                                        persist_agile_state(AgileState::Idle);
                                                    }
                                                }
                                            }
                                        } else {
                                            // No price data available for current time
                                            // Reset to idle so we don't get stuck in previous state
                                            let cached_count = state.cached_agile_prices.lock().await.len();
                                            let mut ag_state = state.agile_state.lock().await;
                                            if *ag_state != AgileState::Idle {
                                                *ag_state = AgileState::Idle;
                                                persist_agile_state(AgileState::Idle);
                                                tracing::warn!(
                                                    cached_slots = cached_count,
                                                    "Agile: no price data for current time, reset to idle",
                                                );
                                            } else {
                                                tracing::warn!(
                                                    cached_slots = cached_count,
                                                    "Agile: no price data for current time (still idle)",
                                                );
                                            }
                                        }
                                    } else {
                                        // Agile mode disabled - if we were actively
                                        // charging/discharging, revert to Eco so the
                                        // inverter doesn't stay force-charging after a
                                        // switch to Standard mode.
                                        //
                                        // IMPORTANT: check if cosy is actively in slot
                                        // before sending CosyExit. The cosy block runs
                                        // BEFORE the agile block in each poll, so if
                                        // cosy just entered, we'd undo its force-charge.
                                        // In that case, just clear the agile flag
                                        // without sending conflicting writes.
                                        let ag_state = state.agile_state.lock().await;
                                        let was_state = *ag_state;
                                        if *ag_state != AgileState::Idle {
                                            if *state.cosy_active.lock().await {
                                                // Cosy is in control - just clear the
                                                // agile flag, don't send CosyExit
                                                // (which would stop the cosy charge).
                                                drop(ag_state);
                                                *state.agile_state.lock().await = AgileState::Idle;
                                                persist_agile_state(AgileState::Idle);
                                                tracing::info!(
                                                    "Agile: disabled while {:?} but cosy is active - cleared flag without reverting",
                                                    was_state
                                                );
                                            } else {
                                                tracing::info!("Agile: mode disabled while {:?} - reverting to Eco", was_state);
                                                drop(ag_state);
                                                let use_3ph = snapshot.device_type.uses_three_phase_schedule_slots();
                                                let cmd = if use_3ph {
                                                    ControlCommand::ThreePhaseCosyExit
                                                } else {
                                                    ControlCommand::CosyExit
                                                };
                                                let mut all_ok = true;
                                                if let Ok(writes) = cmd.encode() {
                                                    for w in &writes {
                                                        if let Err(e) = client.write_register(w.address, w.value).await {
                                                            tracing::error!("Agile: write reg {} failed: {e}", w.address);
                                                            all_ok = false;
                                                        }
                                                        tokio::time::sleep(Duration::from_millis(1500)).await;
                                                    }
                                                } else {
                                                    all_ok = false;
                                                }
                                                if all_ok {
                                                    *state.agile_state.lock().await = AgileState::Idle;
                                                    persist_agile_state(AgileState::Idle);
                                                } else {
                                                    tracing::warn!("Agile: exit writes failed - will retry on next poll");
                                                }
                                            }
                                        }
                                    }
                                }

                                // ---- Email alerts ----
                                //
                                // Evaluate the sanitized snapshot against user-
                                // configured thresholds and send email via Brevo
                                // if any alerts are triggered (debounced).
                                {
                                    let settings_cfg = state.alert_config.lock().await;
                                    let config = settings_cfg.clone();
                                    if config.enabled {
                                        tracing::debug!(
                                            "Alerts: evaluating (grid_loss={}, batt_over_temp={}, soc={})",
                                            snapshot.grid_loss,
                                            snapshot.battery_over_temp,
                                            snapshot.soc,
                                        );
                                        let triggered =
                                            crate::alerts::evaluate_alerts(&snapshot, &config);
                                        let mut debounce =
                                            state.alert_debounce.lock().await;

                                        // Register-corruption defence for the inverter's
                                        // hardware battery warning flag (IR 57). The raw
                                        // flag is fed into the debounce's consecutive-read
                                        // counter every cycle; the BatteryOverTemp alert
                                        // is only kept if the flag has now read `true` for
                                        // BATTERY_WARNING_CONFIRM_CYCLES cycles in a row.
                                        // This prevents a single transient garbage read on
                                        // IR(57) from firing a spurious warning (e.g. the
                                        // reported 21.5°C over-temp false positive), while
                                        // still allowing a genuine sustained warning
                                        // through regardless of the configured °C limit.
                                        let confirmed =
                                            debounce.confirm_battery_warning(
                                                snapshot.battery_over_temp
                                                    && config.battery_over_temp_enabled,
                                            );
                                        // Precision defence for the solar-clipping
                                        // alert: feed this cycle's "solar above the
                                        // configured ceiling" flag into a
                                        // consecutive-read counter. The alert only
                                        // survives if solar has been over the
                                        // ceiling for SOLAR_CLIPPING_CONFIRM_CYCLES
                                        // cycles, so a momentary cloud-edge spike
                                        // does not fire it.
                                        let clipping_confirmed =
                                            debounce.confirm_solar_clipping(
                                                config.solar_clipping_enabled
                                                    && config.solar_clipping_ceiling_w > 0
                                                    && snapshot.solar_power
                                                        > config.solar_clipping_ceiling_w as i32,
                                            );
                                        let confirmed_triggered: Vec<AlertType> = triggered
                                            .iter()
                                            .copied()
                                            .filter(|a| match *a {
                                                AlertType::BatteryOverTemp => confirmed,
                                                AlertType::SolarClipping => clipping_confirmed,
                                                _ => true,
                                            })
                                            .collect();
                                        let triggered = confirmed_triggered;
                                        if !triggered.is_empty() {
                                            tracing::warn!("Alerts: triggered={:?}", triggered);
                                        }
                                        let (to_send, suppressed): (Vec<_>, Vec<_>) = triggered
                                            .iter()
                                            .copied()
                                            .partition(|a| debounce.should_fire(*a, config.cooldown_minutes));
                                        if !suppressed.is_empty() {
                                            tracing::warn!(
                                                "Alerts: {:?} triggered but suppressed by cooldown",
                                                suppressed
                                            );
                                        }
                                        // Detect alerts that were previously active but have
                                        // now returned to normal.
                                        let cleared = debounce.extract_cleared(&triggered);
                                        let _cooldown = config.cooldown_minutes;
                                        drop(debounce);

                                        // Send "problem cleared" notifications
                                        if !cleared.is_empty() {
                                            let text = crate::alerts::build_cleared_message(
                                                &snapshot, &cleared,
                                            );
                                            let token = config.telegram_bot_token.clone();
                                            let chat_id = config.telegram_chat_id.clone();
                                            let ntfy_text = text.clone();
                                            let cleared_names = cleared
                                                .iter()
                                                .map(|a| a.human_name())
                                                .collect::<Vec<_>>()
                                                .join(", ");

                                            if !token.is_empty() && !chat_id.is_empty() {
                                                tokio::task::spawn_blocking(move || {
                                                    match crate::alerts::send_telegram_message(
                                                        &token,
                                                        &chat_id,
                                                        &text,
                                                    ) {
                                                        Ok(()) => tracing::warn!(
                                                            "Cleared alert sent: {cleared_names}"
                                                        ),
                                                        Err(e) => tracing::warn!(
                                                            "Failed to send cleared alert: {e}"
                                                        ),
                                                    }
                                                });
                                            }

                                            let ntfy_topic = config.ntfy_topic.clone();
                                            let ntfy_server = config.ntfy_server.clone();
                                            tokio::task::spawn_blocking(move || {
                                                if ntfy_topic.is_empty() {
                                                    return;
                                                }
                                                match crate::alerts::send_ntfy_message(
                                                    &ntfy_topic,
                                                    &ntfy_server,
                                                    &ntfy_text,
                                                ) {
                                                    Ok(()) => tracing::warn!("ntfy cleared alert sent"),
                                                    Err(e) => tracing::warn!("ntfy cleared alert failed: {e}"),
                                                }
                                            });
                                        }

                                        if !to_send.is_empty() {
                                            let text = crate::alerts::build_alert_message(
                                                &snapshot, &to_send,
                                            );
                                            let token = config.telegram_bot_token.clone();
                                            let chat_id = config.telegram_chat_id.clone();
                                            let ntfy_text = text.clone();

                                            if !token.is_empty() && !chat_id.is_empty() {
                                                tokio::task::spawn_blocking(move || {
                                                    match crate::alerts::send_telegram_message(
                                                        &token,
                                                        &chat_id,
                                                        &text,
                                                    ) {
                                                        Ok(()) => tracing::warn!(
                                                            "Alert sent: {:?}",
                                                            to_send
                                                        ),
                                                        Err(e) => tracing::warn!(
                                                            "Failed to send alert: {e}"
                                                        ),
                                                    }
                                                });
                                            }

                                            // Also send via ntfy if topic configured
                                            let ntfy_topic = config.ntfy_topic.clone();
                                            let ntfy_server = config.ntfy_server.clone();
                                            tokio::task::spawn_blocking(move || {
                                                if ntfy_topic.is_empty() {
                                                    return;
                                                }
                                                match crate::alerts::send_ntfy_message(
                                                    &ntfy_topic,
                                                    &ntfy_server,
                                                    &ntfy_text,
                                                ) {
                                                    Ok(()) => tracing::warn!("ntfy alert sent"),
                                                    Err(e) => tracing::warn!("ntfy alert failed: {e}"),
                                                }
                                            });
                                        }
                                    }
                                    drop(settings_cfg);
                                }

                                // ---- Daily consumption report ----
                                {
                                    let settings_cfg = state.alert_config.lock().await;
                                    let config = settings_cfg.clone();
                                    drop(settings_cfg);

                                    if config.daily_report_enabled {
                                        let today = chrono::Local::now().date_naive();
                                        let mut last_sent = state.last_report_date.lock().await;
                                        if *last_sent != Some(today) {
                                            let now = chrono::Local::now();
                                            let minutes_since_midnight =
                                                now.hour() * 60 + now.minute();
                                            let send_minutes = config.daily_report_hour as u32 * 60
                                                + config.daily_report_minute as u32;

                                            if minutes_since_midnight >= send_minutes {
                                                let yesterday = today
                                                    .checked_sub_signed(
                                                        chrono::Duration::days(1),
                                                    )
                                                    .unwrap_or(today);
                                                let db_guard = state.history.lock().await;
                                                let db = db_guard.clone();
                                                drop(db_guard);

                                                if let Some(ref db) = db {
                                                    match db.get_readings_for_date(yesterday) {
                                                        Ok(rows) => {
                                                            let date_str = yesterday
                                                                .format("%A %d %B %Y")
                                                                .to_string();
                                                            let html = crate::alerts::report::
                                                                generate_daily_report_html(
                                                                    &rows, &date_str,
                                                                );
                                                            if let Some(ref report_body) = html {
                                                                let caption = crate::alerts::report::
                                                                    generate_daily_summary_text(
                                                                        &rows,
                                                                        &yesterday
                                                                            .format("%A %d %B %Y")
                                                                            .to_string(),
                                                                        &crate::settings::Settings::load(),
                                                                    )
                                                                    .unwrap_or_default();

                                                                let token = config.telegram_bot_token.clone();
                                                                let chat_id = config.telegram_chat_id.clone();
                                                                let filename = format!(
                                                                    "hem-report-{}.html",
                                                                    yesterday
                                                                );
                                                                let body = report_body.clone();
                                                                tokio::task::spawn_blocking(move || {
                                                                    match crate::alerts::send_telegram_document(
                                                                        &token,
                                                                        &chat_id,
                                                                        &caption,
                                                                        &filename,
                                                                        body.as_bytes(),
                                                                    ) {
                                                                        Ok(()) => tracing::warn!(
                                                                            "Daily report sent"
                                                                        ),
                                                                        Err(e) => tracing::warn!(
                                                                            "Failed to send daily report: {e}"
                                                                        ),
                                                                    }
                                                                });
                                                                *last_sent = Some(today);
                                                            } else {
                                                                tracing::debug!(
                                                                    "Daily report: insufficient data for {yesterday}",
                                                                );
                                                                *last_sent = Some(today);
                                                            }
                                                        }
                                                        Err(e) => {
                                                            tracing::warn!(
                                                                "Failed to query history for daily report: {e}"
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                // Reflect the (possibly updated) cosy_active flag
                                // AFTER the cosy state machine has run. Without this,
                                // the broadcast snapshot would carry the previous
                                // cycle's value for one poll after a slot transition
                                // - e.g. showing "Cosy Active" for an extra poll
                                // after the slot actually ended.
                                snapshot.cosy_active = *state.cosy_active.lock().await;
                                let ag = state.agile_state.lock().await;
                                snapshot.agile_active = *ag != AgileState::Idle;
                                snapshot.agile_state = format!("{:?}", *ag);

                                // ---- Solar energy from PV power integration ----
                                // Compute today_solar_kwh by integrating solar_power
                                // over time, rather than relying on the register
                                // value which can get stuck at a corrupted baseline.
                                {
                                    let now = tokio::time::Instant::now();
                                    if let Some((last_time, accumulated_kwh)) =
                                        solar_energy_tracker.as_mut()
                                    {
                                        let elapsed_secs =
                                            now.duration_since(*last_time).as_secs_f64();
                                        // Only accumulate for reasonable time gaps
                                        if elapsed_secs > 0.0 && elapsed_secs < 600.0 {
                                            let power_w = snapshot.solar_power.max(0) as f64;
                                            let added_kwh =
                                                power_w * elapsed_secs / 3_600_000.0;
                                            *accumulated_kwh += added_kwh as f32;

                                            // Detect midnight rollover: register value
                                            // drops significantly while tracker still
                                            // has yesterday's accumulated value
                                            if snapshot.today_solar_kwh
                                                < *accumulated_kwh - 1.0
                                            {
                                                tracing::info!(
                                                    register = snapshot.today_solar_kwh,
                                                    accumulated = *accumulated_kwh,
                                                    "Solar tracker reset (midnight rollover)"
                                                );
                                                *accumulated_kwh =
                                                    snapshot.today_solar_kwh;
                                            }

                                            // Log significant cross-check discrepancies
                                            let diff = (*accumulated_kwh
                                                - snapshot.today_solar_kwh)
                                                .abs();
                                            if diff > 0.5 {
                                                tracing::info!(
                                                    register = snapshot.today_solar_kwh,
                                                    computed = *accumulated_kwh,
                                                    diff,
                                                    "Solar energy cross-check: register vs PV-integrated"
                                                );
                                            }

                                            // Use the computed value
                                            snapshot.today_solar_kwh = *accumulated_kwh;
                                        }
                                        *last_time = now;
                                    } else {
                                        // First poll: seed from register
                                        solar_energy_tracker =
                                            Some((now, snapshot.today_solar_kwh));
                                    }
                                }

                                {
                                    let mut latest = state.latest_snapshot.lock().await;
                                    *latest = Some(snapshot.clone());
                                }

                                // Broadcast to WebSocket subscribers.
                                let _ = state.tx.send(PollMessage::Snapshot(Box::new(snapshot.clone())));

                                // Persist to history database. Clone the Arc and
                                // drop the lock so synchronous SQLite I/O doesn't
                                // block the Tokio worker (same pattern as get_history).
                                if snapshot.soc > 0 {
                                    let db_guard = state.history.lock().await;
                                    let db = db_guard.clone();
                                    drop(db_guard);
                                    if let Some(db) = db {
                                        let snap = snapshot.clone();
                                        tokio::task::spawn_blocking(move || {
                                            db.insert_reading(&snap);
                                        });
                                    }
                                }

                                (true, sanitized || block_suspicious, false)
                            }
                            Err(e) => {
                                let connection_lost = e.is_connection_lost();
                                if connection_lost {
                                    tracing::warn!(
                                        error = %e,
                                        "Poll read failed - connection lost, reconnecting"
                                    );
                                } else {
                                    tracing::warn!(
                                        error = %e,
                                        consecutive_failures = consecutive_failures + 1,
                                        max = MAX_CONSECUTIVE_FAILURES,
                                        "Poll read failed"
                                    );
                                }
                                (false, false, connection_lost)
                            }
                        }
                    }.await;

                    match poll_ok {
                        true => {
                            consecutive_failures = 0;
                            consecutive_suspicious = 0;

                            // Tick the meter retry cadence counter.
                            if meter_probe_done
                                && meter_retry_count > 0
                                && meter_retry_count < METER_MAX_RETRIES
                            {
                                meter_cycle_since_last += 1;
                            }
                            // If the first scan found nothing and ammeter is
                            // expected, start the retry cadence.
                            if meter_probe_done
                                && meter_retry_count == 0
                                && detected_meters.is_empty()
                            {
                                meter_cycle_since_last += 1;
                            }

                            // Sanitization was applied - corrupted register data
                            // detected. Re-poll immediately instead of waiting
                            // for the next interval, so the frontend gets a
                            // fresh reading as soon as possible.
                            if sanitized {
                                tracing::debug!("Corrupted data detected - re-reading immediately");
                                continue;
                            }
                        }
                        false => {
                            if connection_lost {
                                break;
                            }
                            consecutive_failures += 1;
                            if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                                tracing::warn!(
                                    consecutive_failures,
                                    max = MAX_CONSECUTIVE_FAILURES,
                                    "Poll read failed 3× - reconnecting"
                                );
                                break;
                                // of breaking out of the inner loop - staying connected
                                // avoids the warmup + grace period on the next poll.
                            } else {
                                // Transient error - retry after a short pause
                                tracing::debug!(
                                    "Poll read failed ({}/{}) - retrying",
                                    consecutive_failures,
                                    MAX_CONSECUTIVE_FAILURES,
                                );
                                tokio::time::sleep(Duration::from_secs(2)).await;
                                continue; // stay in the inner loop
                            }
                        }
                    }

                    // If consecutive fingerprint corruption exceeds the
                    // threshold, break out of the inner loop to force a
                    // reconnect (the dongle may have crashed and needs a
                    // fresh TCP session to recover).
                    if consecutive_suspicious >= MAX_SUSPICIOUS_CYCLES {
                        tracing::warn!(
                            suspicious = consecutive_suspicious,
                            max = MAX_SUSPICIOUS_CYCLES,
                            "Persistent fingerprint corruption - disconnecting"
                        );
                        break;
                    }

                    // Sleep for the configured interval, but wake early if:
                    //   • settings changed (new host → reconnect)
                    //   • new writes were queued (apply immediately)
                    //
                    // NOTE: current_version was captured at the TOP of this
                    // iteration (before the poll). Do NOT re-capture here -
                    // the sleep loop compares against the PRE-POLL version
                    // so it detects version bumps that happened during the poll.
                    let interval_secs = state.settings.lock().await.interval_secs;
                    let sleep_deadline =
                        tokio::time::Instant::now() + Duration::from_secs(interval_secs);
                    loop {
                        // Wait up to 1 second, or until writes are queued
                        tokio::select! {
                            _ = state.write_notify.notified() => {
                                // Writes queued - wake immediately
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
                            tracing::info!(
                                "Settings changed (v{} → v{}) - reconnecting",
                                current_version,
                                cur.version
                            );
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
                    "Disconnecting from inverter - will reconnect"
                );
                client.disconnect().await;

                // Clear the latest snapshot so the next connection starts fresh.
                // Without this, stale/corrupted values from the old session
                // persist as the sanitizer's "previous" reference.
                {
                    let mut latest = state.latest_snapshot.lock().await;
                    *latest = None;
                }

                tracing::debug!("Disconnected - entering reconnect cycle");

                // Clear connection timestamp when the connection drops.
                if let Ok(mut guard) = state.connected_since.lock() {
                    *guard = None;
                }

                {
                    let mut cs = state.connection_state.lock().await;
                    *cs = ConnectionState::Reconnecting;
                }
                let _ = state.tx.send(PollMessage::Connection {
                    state: ConnectionState::Reconnecting,
                    host: settings.host.clone(),
                    connected_since_epoch_ms: None,
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
                if let Ok(mut guard) = state.connected_since.lock() {
                    *guard = None;
                }

                let _ = state.tx.send(PollMessage::Connection {
                    state: ConnectionState::Disconnected,
                    host: settings.host.clone(),
                    connected_since_epoch_ms: None,
                });

                // Track consecutive connect failures for frontend.
                state
                    .connect_failures
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                // ---- Auto-discovery on persistent connection failure ----
                // After N consecutive failures, scan the LAN for the dongle
                // in case its IP changed (DHCP renewal, etc.). If exactly one
                // alternative inverter is found, auto-switch to it.
                consecutive_connect_failures = consecutive_connect_failures.wrapping_add(1);
                let should_discover = !settings.disable_auto_discovery
                    && consecutive_connect_failures >= DISCOVERY_AFTER_FAILURES
                    && last_discovery_time
                        .is_none_or(|t| t.elapsed() >= DISCOVERY_COOLDOWN);

                if should_discover {
                    last_discovery_time = Some(Instant::now());
                    tracing::warn!(
                        "Auto-discovery: {} consecutive failures to reach {}:{}. Scanning LAN...",
                        consecutive_connect_failures,
                        settings.host,
                        settings.port
                    );

                    let subnets = crate::inverter::discovery::detect_lan_subnets();
                    let inverters = crate::inverter::discovery::scan_multiple_subnets(&subnets).await;

                    // Filter out the configured host (it's clearly not responding).
                    let candidates: Vec<_> = inverters
                        .iter()
                        .filter(|inv| inv.ip != settings.host)
                        .collect();

                    match candidates.len() {
                        0 => {
                            tracing::warn!(
                                "Auto-discovery: no alternative inverters found on LAN ({}:{} unreachable). Dongle may be powered off or network changed.",
                                settings.host,
                                settings.port
                            );
                        }
                        1 => {
                            let new = &candidates[0];
                            tracing::warn!(
                                "Auto-discovery: found alternative inverter at {}:{}. Auto-switching from {}:{}.",
                                new.ip, new.port, settings.host, settings.port
                            );

                            // Persist the new host to disk so it survives restart.
                            let mut persist = crate::settings::Settings::load();
                            persist.host = new.ip.clone();
                            persist.port = new.port;
                            if let Err(e) = persist.save() {
                                tracing::warn!("Auto-discovery: failed to persist new host: {e}");
                            }

                            // Update in-memory settings + bump version so the
                            // next loop iteration picks up the new host.
                            let mut poll_settings = state.settings.lock().await;
                            poll_settings.host = new.ip.clone();
                            poll_settings.port = new.port;
                            poll_settings.version = poll_settings.version.wrapping_add(1);
                            drop(poll_settings);

                            // Reset counters so we try the new host immediately
                            // with a fresh TCP connect, not a stale backoff.
                            consecutive_connect_failures = 0;
                            backoff = Duration::from_secs(5);
                        }
                        n => {
                            let alts: Vec<_> = candidates
                                .iter()
                                .map(|i| format!("{}:{}", i.ip, i.port))
                                .collect();
                            tracing::warn!(
                                "Auto-discovery: found {} alternative inverters — ambiguous, not auto-switching: {}",
                                n,
                                alts.join(", ")
                            );
                        }
                    }
                }
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
    use crate::inverter::model::DeviceType;

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
        crate::test_util::with_isolated_config_dir(|| {
            let state = AppState::new();
            // Can obtain a receiver from the broadcast channel.
            let _rx = state.tx.subscribe();
        });
    }

    /// Cosy crash-recovery: when the app restarts, in-memory `cosy_active`
    /// is seeded from `cosy_active_persisted` in settings. If the persisted
    /// flag is `true` (we crashed mid-Cosy), the cosy state machine on the
    /// next poll will either re-send ForceCharge (if still inside a slot)
    /// or fire CosyExit (if the slot ended while we were down).
    #[test]
    fn cosy_active_seeds_from_persisted_flag() {
        crate::test_util::with_isolated_config_dir(|| {
            // Persist cosy_active_persisted=true to settings.
            persist_cosy_active(true);
            let state = AppState::new();
            let seeded = *state.cosy_active.blocking_lock();

            assert!(
                seeded,
                "AppState::new should seed cosy_active from cosy_active_persisted"
            );
        });
    }

    #[test]
    fn model_detection_repolls_for_ac_coupled_address_switch_and_extra_block() {
        // ACCoupled: slave changes from detection 0x11 to operational 0x31,
        // AND the optional HR300-359 (AC config) block needs to be requested.
        assert!(should_repoll_after_model_detection(
            DeviceType::ACCoupled,
            0x11
        ));
        // Even once already on 0x31, AC still needs an immediate model-aware
        // re-poll so the optional HR300-359 block is requested.
        assert!(should_repoll_after_model_detection(
            DeviceType::ACCoupled,
            0x31
        ));
    }

    #[test]
    fn model_detection_repolls_for_models_with_extra_blocks() {
        // Gen3 uses the extended HR240-299 block, so it should re-poll after
        // detection even when the slave address is already correct.
        assert!(should_repoll_after_model_detection(
            DeviceType::Gen3Hybrid,
            0x11
        ));
        // Three-phase models use the HR1080-1124 block.
        assert!(should_repoll_after_model_detection(
            DeviceType::ThreePhase,
            0x11
        ));
        // Gateway needs an immediate re-poll to request the IR 1600+ blocks
        // and the HR1080-1124 three-phase config block.
        assert!(should_repoll_after_model_detection(
            DeviceType::Gateway,
            0x11
        ));
    }

    #[test]
    fn model_detection_does_not_repoll_for_plain_gen2_on_0x11() {
        assert!(!should_repoll_after_model_detection(
            DeviceType::Gen2Hybrid,
            0x11
        ));
    }

    #[test]
    fn external_meter_probe_runs_after_ac_model_repoll() {
        // AC-coupled models trigger an immediate model-aware re-poll after detection.
        // The CT meter scan must therefore be allowed on the following poll, once
        // known_device_type is set but no meter probe has completed yet.
        assert!(should_probe_external_meters(
            Some(DeviceType::ACCoupled),
            false, // meter_probe_done
            false, // enable_ammeter
            0,     // meter_type
            0,     // meter_retry_count
            0,     // meter_cycle_since_last
        ));
        assert!(should_probe_external_meters(
            Some(DeviceType::ACCoupledMk2),
            false,
            false,
            0,
            0,
            0,
        ));
    }

    #[test]
    fn external_meter_probe_skips_batteryless_gateway() {
        // Batteryless devices (Gateway, EMS, PvInverter) should never probe
        // for external CT meters - they have their own built-in metering.
        // The scan should not run even on the very first cycle after detection.
        assert!(!should_probe_external_meters(
            Some(DeviceType::Gateway),
            false,
            false,
            0,
            0,
            0,
        ));
        assert!(!should_probe_external_meters(
            Some(DeviceType::Ems),
            false,
            false,
            0,
            0,
            0,
        ));
        assert!(!should_probe_external_meters(
            Some(DeviceType::PvInverter),
            false,
            false,
            0,
            0,
            0,
        ));
    }

    #[test]
    fn external_meter_probe_is_single_shot_without_ammeter() {
        // No ammeter configured, first scan already done - no further probing.
        assert!(!should_probe_external_meters(
            Some(DeviceType::ACCoupled),
            true,  // meter_probe_done
            false, // enable_ammeter
            0,     // meter_type
            0,     // meter_retry_count
            5,     // meter_cycle_since_last
        ));
    }

    #[test]
    fn external_meter_probe_skips_three_phase() {
        assert!(!should_probe_external_meters(
            Some(DeviceType::ThreePhase),
            false,
            false,
            0,
            0,
            0,
        ));
    }

    #[test]
    fn external_meter_probe_skips_unknown_device() {
        assert!(!should_probe_external_meters(None, false, false, 0, 0, 0,));
    }

    #[test]
    fn meter_retry_fires_when_ammeter_expected() {
        // EM115 configured (meter_type=1), first scan done, no meters found,
        // enough cycles have passed - should retry.
        assert!(should_probe_external_meters(
            Some(DeviceType::Gen3Hybrid),
            true,                 // meter_probe_done
            false,                // enable_ammeter
            1,                    // meter_type = EM115
            0,                    // meter_retry_count (first retry)
            METER_RETRY_INTERVAL, // enough cycles elapsed
        ));
    }

    #[test]
    fn meter_retry_respects_cadence() {
        // EM115 configured but not enough cycles since last attempt - skip.
        assert!(!should_probe_external_meters(
            Some(DeviceType::Gen3Hybrid),
            true,
            false,
            1,
            0,
            METER_RETRY_INTERVAL - 1,
        ));
    }

    #[test]
    fn meter_retry_stops_after_max_retries() {
        assert!(!should_probe_external_meters(
            Some(DeviceType::Gen3Hybrid),
            true,
            true, // enable_ammeter
            0,
            METER_MAX_RETRIES, // exhausted
            METER_RETRY_INTERVAL,
        ));
    }

    #[test]
    fn meter_retry_enabled_by_enable_ammeter_flag() {
        // enable_ammeter=true is sufficient even without EM115 meter_type.
        assert!(should_probe_external_meters(
            Some(DeviceType::Gen3Hybrid),
            true,
            true, // enable_ammeter
            0,    // meter_type (not EM115)
            3,    // some retries used
            METER_RETRY_INTERVAL,
        ));
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
        let msg = PollMessage::Snapshot(Box::new(snap));
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
            connected_since_epoch_ms: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"connection\""));
        // When None, the field should be skipped.
        assert!(!json.contains("connected_since_epoch_ms"));
        let de: PollMessage = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(de, PollMessage::Connection { state: ConnectionState::Reconnecting, ref host, connected_since_epoch_ms } if host == "192.168.1.100" && connected_since_epoch_ms.is_none())
        );
    }

    #[test]
    fn poll_message_connection_with_since() {
        let msg = PollMessage::Connection {
            state: ConnectionState::Connected,
            host: "10.0.0.5".to_string(),
            connected_since_epoch_ms: Some(1_700_000_000_000u64),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("connected_since_epoch_ms"));
        assert!(json.contains("1700000000000"));
        let de: PollMessage = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(de, PollMessage::Connection { state: ConnectionState::Connected, ref host, connected_since_epoch_ms: Some(ts) } if host == "10.0.0.5" && ts == 1_700_000_000_000u64)
        );
    }

    #[test]
    fn poll_message_connection_backward_compat() {
        // Old-format JSON without connected_since_epoch_ms must still deserialize.
        let json = r#"{"type":"connection","state":"disconnected","host":"192.168.1.1"}"#;
        let de: PollMessage = serde_json::from_str(json).unwrap();
        assert!(
            matches!(de, PollMessage::Connection { state: ConnectionState::Disconnected, ref host, connected_since_epoch_ms } if host == "192.168.1.1" && connected_since_epoch_ms.is_none())
        );
    }

    #[test]
    fn app_state_latest_snapshot_starts_none() {
        crate::test_util::with_isolated_config_dir(|| {
            let state = Arc::new(AppState::new());
            let snapshot = state.latest_snapshot.blocking_lock();
            assert!(snapshot.is_none());
        });
    }

    #[test]
    fn app_state_connection_starts_disconnected() {
        crate::test_util::with_isolated_config_dir(|| {
            let state = Arc::new(AppState::new());
            let cs = state.connection_state.blocking_lock();
            assert_eq!(*cs, ConnectionState::Disconnected);
        });
    }

    #[test]
    fn app_state_connected_since_starts_none() {
        crate::test_util::with_isolated_config_dir(|| {
            let state = Arc::new(AppState::new());
            let cs = state.connected_since.lock().unwrap();
            assert!(cs.is_none());
        });
    }

    #[test]
    fn app_state_connect_failures_starts_zero() {
        crate::test_util::with_isolated_config_dir(|| {
            let state = Arc::new(AppState::new());
            assert_eq!(
                state.connect_failures.load(std::sync::atomic::Ordering::Relaxed),
                0
            );
        });
    }

    #[test]
    fn app_state_connected_since_set_and_clear() {
        crate::test_util::with_isolated_config_dir(|| {
            let state = Arc::new(AppState::new());
            // Set connected_since
            *state.connected_since.lock().unwrap() = Some(std::time::SystemTime::now());
            assert!(state.connected_since.lock().unwrap().is_some());
            // Clear it
            *state.connected_since.lock().unwrap() = None;
            assert!(state.connected_since.lock().unwrap().is_none());
        });
    }

    #[test]
    fn app_state_connect_failures_increment_and_reset() {
        crate::test_util::with_isolated_config_dir(|| {
            let state = Arc::new(AppState::new());
            assert_eq!(
                state.connect_failures.load(std::sync::atomic::Ordering::Relaxed),
                0
            );
            state
                .connect_failures
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            assert_eq!(
                state.connect_failures.load(std::sync::atomic::Ordering::Relaxed),
                1
            );
            state
                .connect_failures
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            assert_eq!(
                state.connect_failures.load(std::sync::atomic::Ordering::Relaxed),
                2
            );
            state
                .connect_failures
                .store(0, std::sync::atomic::Ordering::Relaxed);
            assert_eq!(
                state.connect_failures.load(std::sync::atomic::Ordering::Relaxed),
                0
            );
        });
    }

    // -----------------------------------------------------------------
    // Lock-ordering concurrency test
    // -----------------------------------------------------------------

    /// Verify that the real concurrent access pattern (hold `tokio::sync::Mutex`
    /// guard, acquire `parking_lot::Mutex` guard while holding it, drop the
    /// `parking_lot` guard, then `.await`) does not deadlock.
    ///
    /// This exercises the documented invariant from the module doc:
    /// `parking_lot::Mutex` MAY be acquired inside a `tokio::sync::Mutex`
    /// guard, but MUST NOT be held across an `.await`.
    ///
    /// The test spawns two concurrent tasks that emulate the real access
    /// pattern found in `ws.rs` and `api.rs`: each task repeatedly locks
    /// `settings` (tokio), then within the scope locks `connected_clients`
    /// (parking_lot), does a brief read+write, drops the parking_lot guard,
    /// then .awaits something (simulated yield). A `tokio::time::timeout`
    /// catches any deadlock introduced by violating the invariant.
    #[tokio::test]
    async fn app_state_concurrent_tokio_and_parking_lot_mutex_no_deadlock() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());

            let mut handles = Vec::new();

            for _i in 0..4 {
                let s = state.clone();
                handles.push(tokio::spawn(async move {
                    for _ in 0..100 {
                        // Real access pattern from ws.rs / api.rs:
                        // 1. Lock a tokio::sync::Mutex field.
                        let _settings = s.settings.lock().await;

                        // 2. Within the tokio guard scope, acquire the
                        //    parking_lot::Mutex. Do synchronous work.
                        {
                            let mut clients = s.connected_clients.lock();
                            let addr: std::net::SocketAddr = "127.0.0.1:6000".parse().unwrap();
                            let cid = clients.add(addr);
                            clients.remove(cid);
                        } // parking_lot guard dropped HERE, before any .await.

                        // 3. Also lock another tokio Mutex.
                        {
                            let cs = s.connection_state.lock().await;
                            drop(cs);
                        }

                        // 4. Yield — if a parking_lot guard were held across
                        //    this point, the executor thread would block.
                        tokio::task::yield_now().await;

                        drop(_settings); // tokio guard dropped
                    }
                }));
            }

            let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                for h in handles {
                    h.await.expect("task panicked");
                }
            })
            .await;

            assert!(
                result.is_ok(),
                "concurrent tokio + parking_lot mutex access deadlocked (timeout)"
            );
        })
        .await;
    }
}
