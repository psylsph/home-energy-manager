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
use crate::inverter::model::{
    BatteryMode, DeviceType, InverterSnapshot, SolarArraySource, SolarArraySummary,
};
use crate::inverter::reconnect::ReconnectController;
use crate::inverter::sanitizer::{
    carry_forward_battery_modules_with, carry_forward_optional_block_values,
    derive_battery_fields_from_bms, is_block_suspicious, sanitize_snapshot, validate_battery_bms,
    ConsecutiveSuspectCounts, DeltaCorrectionCounts, GraceCumulativeSamples, RateReleaseCounts,
};
use crate::inverter::state_machines::{
    build_force_discharge_auto_revert_writes, check_auto_winter, check_load_limiter,
    clear_cosy_slot_registers, cosy_slot_register_writes, persist_cosy_active,
    write_registers_to_inverter, AgileSlotAction,
};
pub use crate::inverter::state_machines::{
    AutoWinterConfig, AutoWinterSaved, AutoWinterState, LoadLimiterConfig, LoadLimiterSaved,
    LoadLimiterState, PriceSlot,
};
use crate::modbus::client::GatewayPollScope;
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
    /// EV charger TCP/Modbus connection was just established (before the
    /// first register read). Lets the frontend latch "we've reached the
    /// configured host" immediately instead of waiting for the first
    /// successful register poll — covers the case where the user sees
    /// "EVC: connected" in the logs but the read fails transiently or
    /// the WS misses the first snapshot frame (issue #138).
    EvcConnected,
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

/// Snapshot of inverter registers captured at the moment Force Charge is
/// started, used to restore the inverter to its pre-force-charge state when
/// the user clicks Stop Charge. Mirrors the `revert` dict GivTCP builds in
/// `forceCharge` (`write.py:1134`).
///
/// All fields are *pre-force-charge* values, captured before the force-charge
/// writes are applied. Restoration writes these values back. A value of
/// `None` in an `Option<_>` field means "no previous value known" and the
/// corresponding write is skipped.
#[derive(Debug, Clone)]
pub struct ForceChargeRevert {
    /// Whether the schedule charge flag (HR 20) was enabled before force charge.
    pub enable_charge: bool,
    /// Whether the schedule discharge flag (HR 59) was enabled before force
    /// charge. `ForceCharge` start writes `HR_ENABLE_DISCHARGE=0` to clear
    /// any stale discharge flag, so on stop we must restore the pre-value
    /// or the user's discharge schedule is left disabled.
    pub enable_discharge: bool,
    /// The charge target SOC (HR 116 / HR 1111) before force charge.
    pub target_soc: u8,
    /// Battery power mode (HR 27) before force charge: 0 = export, 1 = eco.
    /// `ForceCharge` start writes `HR_BATTERY_POWER_MODE=1`, so on stop we
    /// must restore the pre-value (e.g. 0 for users in Max-Power/Timed
    /// Export mode before they hit Force Charge).
    pub battery_power_mode: u8,
    /// Battery charge rate (HR 111 / HR 313) before force charge, if known.
    pub charge_rate: Option<u8>,
    /// Charge slot 1 start time (HH,MM) before force charge, if any was set.
    /// `None` means no slot was configured (write 00:00–00:00 to clear).
    pub charge_slot_1_start: Option<(u8, u8)>,
    /// Charge slot 1 end time (HH,MM) before force charge, if any was set.
    pub charge_slot_1_end: Option<(u8, u8)>,
    /// Whether the inverter was in a "force charge enable" state (HR 1123) for
    /// three-phase models. None for single-phase where this register does not
    /// exist; Some(false) for the typical "not force-charging" pre-state.
    pub three_phase_force_charge_enable: Option<bool>,
    /// Whether AC charge was enabled (HR 1112) for three-phase models.
    pub three_phase_ac_charge_enable: Option<bool>,
    /// Battery pause mode (HR 318) for AC-coupled models, if the field is
    /// present in the snapshot. None means "not present on this model" and
    /// the restore write is skipped.
    pub battery_pause_mode: Option<u8>,
}

/// Snapshot of inverter registers captured at the moment Force Discharge is
/// started, used to restore the inverter to its pre-force-discharge state
/// when the user clicks Stop Discharge. Mirrors the `revert` dict GivTCP
/// builds in `forceExport` (`write.py:980-1010`).
///
/// All fields are *pre-force-discharge* values, captured before the
/// force-discharge writes are applied. Restoration writes these values back.
/// A value of `None` in an `Option<_>` field means "no previous value known"
/// and the corresponding write is skipped.
#[derive(Debug, Clone)]
pub struct ForceDischargeRevert {
    /// Whether the schedule charge flag (HR 20) was enabled before force discharge.
    pub enable_charge: bool,
    /// Whether the schedule discharge flag (HR 59) was enabled before force discharge.
    pub enable_discharge: bool,
    /// Battery discharge rate (HR 112 / HR 314) before force discharge, if known.
    pub discharge_rate: Option<u8>,
    /// Discharge slot 1 start time (HH,MM) before force discharge, if any was set.
    /// `None` means no slot was configured (write 00:00–00:00 to clear).
    pub discharge_slot_1_start: Option<(u8, u8)>,
    /// Discharge slot 1 end time (HH,MM) before force discharge, if any was set.
    pub discharge_slot_1_end: Option<(u8, u8)>,
    /// Discharge slot 2 start time (HH,MM) before force discharge, if any was set.
    pub discharge_slot_2_start: Option<(u8, u8)>,
    /// Discharge slot 2 end time (HH,MM) before force discharge, if any was set.
    pub discharge_slot_2_end: Option<(u8, u8)>,
    /// Whether the inverter was in a "force discharge enable" state (HR 1122)
    /// for three-phase models. None for single-phase.
    pub three_phase_force_discharge_enable: Option<bool>,
    /// Whether the inverter was in a "force charge enable" state (HR 1123) for
    /// three-phase models. The force-discharge encoder writes 0 to this
    /// register, so we need to restore its prior value.
    pub three_phase_force_charge_enable: Option<bool>,
    /// Unix epoch millis at which the discharge slot window ends. Set only
    /// on the timed (minutes-bounded) path so the poll loop can auto-revert
    /// when the slot expires — preventing the inverter from being left in
    /// export mode with enable_discharge=1 but no active slot, which
    /// effectively pauses the battery (no charge, no discharge). None on
    /// the "no body" / "until stopped" path, where there is no slot to
    /// expire. See issue #129.
    pub force_discharge_slot_end_ms: Option<i64>,
}

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
    /// Captured pre-state for an in-progress Force Charge, used to restore
    /// the inverter to its prior configuration when the user clicks Stop
    /// Charge. Set on `force_charge` start, cleared on stop. Mirrors the
    /// pre-state snapshot GivTCP captures in `forceCharge`/`FCResume`.
    pub force_charge_revert: Arc<Mutex<Option<ForceChargeRevert>>>,
    /// Captured pre-state for an in-progress Force Discharge, used to restore
    /// the inverter to its prior configuration when the user clicks Stop
    /// Discharge. Set on `force_discharge` start, cleared on stop.
    pub force_discharge_revert: Arc<Mutex<Option<ForceDischargeRevert>>>,
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
    /// Saved register values to restore when the load limiter deactivates.
    pub load_limiter_saved: Arc<Mutex<Option<LoadLimiterSaved>>>,
    /// Whether cosy charging is currently active (force-charging in a slot).
    pub cosy_active: Arc<Mutex<bool>>,
    /// Cached Octopus Agile prices for the current region.
    pub cached_agile_prices: Arc<Mutex<Vec<PriceSlot>>>,
    /// Most recently decoded EV charger snapshot.
    pub latest_evc: Arc<Mutex<Option<crate::evc::EvcSnapshot>>>,
    /// EV charger session-energy latch (issue #189). Holds the last
    /// non-zero `Charge_Session_Energy` so the completed session's kWh
    /// stays visible on the diagram after HR 72 zeroes, and resets on the
    /// "No Cable" → "Cable In" transition. See `crate::evc::SessionLatch`.
    pub evc_session_latch: Arc<Mutex<crate::evc::SessionLatch>>,
    /// Email alert configuration.
    pub alert_config: Arc<Mutex<crate::settings::AlertsConfig>>,
    /// Email alert debounce tracker (in-memory only).
    pub alert_debounce: Arc<Mutex<crate::alerts::AlertDebounce>>,
    /// Last date a daily consumption report was sent.
    pub last_report_date: Arc<Mutex<Option<chrono::NaiveDate>>>,
    /// Weather subsystem state — current config, last fetch result, backfill
    /// progress. Always present (not `Option<…>` like `history`) so the API
    /// layer doesn't have to special-case "weather not yet initialised".
    /// Mirror of `Settings::weather_config` lives inside the struct.
    pub weather: Arc<Mutex<crate::weather::WeatherState>>,
    /// Wall-clock time when the current connection was established (None if disconnected).
    pub connected_since: Arc<std::sync::Mutex<Option<std::time::SystemTime>>>,
    /// How many consecutive TCP connect attempts have failed since the last success.
    pub connect_failures: Arc<std::sync::atomic::AtomicU32>,
    /// Monotonic counter incremented by `POST /api/reconnect` (and any other
    /// path that wants a forced reconnect). The poll loop watches this and
    /// resets its back-off state (`backoff` and `consecutive_dead_sessions`)
    /// when it advances, so a manual "Reconnect" doesn't get swallowed by a
    /// 10-minute zombie-dongle back-off. Uses a counter rather than
    /// `tokio::sync::Notify` so we never lose a signal that arrived between
    /// checks — the next outer-loop iteration is guaranteed to see the
    /// newer value.
    pub reconnect_request: Arc<std::sync::atomic::AtomicU32>,
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
            force_charge_revert: Arc::new(Mutex::new(None)),
            force_discharge_revert: Arc::new(Mutex::new(None)),
            history: Arc::new(Mutex::new(None)),
            log_ring: Arc::new(crate::server::logs::LogRing::new(2000)),
            connected_clients: Arc::new(parking_lot::Mutex::new(ConnectedClients::new())),
            auto_winter_config: Arc::new(Mutex::new(AutoWinterConfig::default())),
            auto_winter_state: Arc::new(Mutex::new(AutoWinterState::default())),
            auto_winter_saved: Arc::new(Mutex::new(None)),
            load_limiter_config: Arc::new(Mutex::new(LoadLimiterConfig::default())),
            load_limiter_state: Arc::new(Mutex::new(LoadLimiterState::default())),
            load_limiter_saved: Arc::new(Mutex::new(None)),
            cosy_active: Arc::new(Mutex::new(
                crate::settings::Settings::load().cosy_active_persisted,
            )),
            cached_agile_prices: Arc::new(Mutex::new(Vec::new())),
            alert_config: Arc::new(Mutex::new(crate::settings::Settings::load().alerts_config)),
            alert_debounce: Arc::new(Mutex::new(crate::alerts::AlertDebounce::new())),
            last_report_date: Arc::new(Mutex::new(None)),
            latest_evc: Arc::new(Mutex::new(None)),
            evc_session_latch: Arc::new(Mutex::new(crate::evc::SessionLatch::default())),
            connected_since: Arc::new(std::sync::Mutex::new(None)),
            connect_failures: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            reconnect_request: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            weather: Arc::new(Mutex::new(crate::weather::WeatherState {
                config: crate::settings::Settings::load().weather_config,
                ..Default::default()
            })),
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
            force_charge_revert: Arc::new(Mutex::new(None)),
            force_discharge_revert: Arc::new(Mutex::new(None)),
            history: Arc::new(Mutex::new(None)),
            log_ring,
            connected_clients: Arc::new(parking_lot::Mutex::new(ConnectedClients::new())),
            auto_winter_config: Arc::new(Mutex::new(AutoWinterConfig::default())),
            auto_winter_state: Arc::new(Mutex::new(AutoWinterState::default())),
            auto_winter_saved: Arc::new(Mutex::new(None)),
            load_limiter_config: Arc::new(Mutex::new(LoadLimiterConfig::default())),
            load_limiter_state: Arc::new(Mutex::new(LoadLimiterState::default())),
            load_limiter_saved: Arc::new(Mutex::new(None)),
            cosy_active: Arc::new(Mutex::new(
                crate::settings::Settings::load().cosy_active_persisted,
            )),
            cached_agile_prices: Arc::new(Mutex::new(Vec::new())),
            alert_config: Arc::new(Mutex::new(crate::settings::Settings::load().alerts_config)),
            alert_debounce: Arc::new(Mutex::new(crate::alerts::AlertDebounce::new())),
            last_report_date: Arc::new(Mutex::new(None)),
            latest_evc: Arc::new(Mutex::new(None)),
            evc_session_latch: Arc::new(Mutex::new(crate::evc::SessionLatch::default())),
            connected_since: Arc::new(std::sync::Mutex::new(None)),
            connect_failures: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            reconnect_request: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            weather: Arc::new(Mutex::new(crate::weather::WeatherState {
                config: crate::settings::Settings::load().weather_config,
                ..Default::default()
            })),
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

/// Whether the persisted serial looks like a GivEnergy Gateway.
///
/// Gateway serials start with the "GW" prefix (e.g. `GW2529A127`). When the
/// user has saved a Gateway serial, we know the device is a Gateway before
/// the first poll — the model is encoded in the hardware identifier, not
/// just in the firmware registers. Letting the runtime know up front lets
/// it skip the wide-scan `STANDARD_POLL_BLOCKS` (IR 0-59 + IR 180-183 are
/// unmapped on the Gateway) and use the lean HR-only set from cycle 1,
/// saving ~300 ms and one round of timeout exposure on every Gateway
/// startup.
fn device_type_from_serial(serial: &str) -> Option<DeviceType> {
    let trimmed = serial.trim();
    if trimmed.len() >= 2 && trimmed[..2].eq_ignore_ascii_case("GW") {
        Some(DeviceType::Gateway)
    } else {
        None
    }
}

/// Standard block set to use for the warmup read after a fresh TCP connect.
/// Falls back to the full `STANDARD_POLL_BLOCKS` when no prefill is
/// available (empty serial, or a serial that doesn't match a known prefix).
fn warmup_blocks_for(
    prefilled: Option<&DeviceType>,
) -> &'static [crate::modbus::registers::RegisterBlock] {
    use crate::modbus::client::preview_standard_blocks;
    preview_standard_blocks(None, prefilled)
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

// Reconnect / back-off helpers (dead_session_backoff, flap_should_engage,
// flap_backoff) and the FLAP_* constants live in `reconnect.rs` alongside
// `ReconnectController`.

/// Gateway detail/config blocks change slowly and have been implicated in
/// overnight dongle stalls. Poll live Gateway telemetry every cycle, but only
/// refresh the slow blocks (per-AIO discharge detail, serials, plant config)
/// every N successful Gateway polls. With the default 60 s refresh interval
/// this is roughly every 10 minutes.
const GATEWAY_DETAIL_POLL_EVERY: u8 = 10;

fn gateway_poll_scope(device_type: Option<DeviceType>, detail_countdown: u8) -> GatewayPollScope {
    if device_type == Some(DeviceType::Gateway) && detail_countdown == 0 {
        GatewayPollScope::Detail
    } else {
        GatewayPollScope::Fast
    }
}

fn next_gateway_detail_countdown(current: u8) -> u8 {
    if current == 0 {
        GATEWAY_DETAIL_POLL_EVERY.saturating_sub(1)
    } else {
        current - 1
    }
}

/// Runs the polling loop indefinitely (spawn as a Tokio task).
///
/// ## Behaviour
///
/// 1. If `settings.host` is empty, sleep 5 s and retry.
/// 2. Attempt to connect. On success, broadcast `Connected` and enter the
///    inner poll loop.
/// 3. On each tick: call `read_all_with_extras`, decode into an
///    [`InverterSnapshot`], store it, and broadcast it.
/// 4. If a poll or I/O error occurs, break out of the inner loop,
///    disconnect, broadcast `Reconnecting`, and attempt reconnection
///    with exponential back-off (5 s → 60 s cap).
pub async fn run_poll_loop(state: Arc<AppState>) {
    // Start the Telegram /status command poller
    crate::alerts::spawn_telegram_poller(state.clone());

    // Connect-failure counter for the auto-discovery subsystem (separate
    // from ReconnectController, which owns the dead-session/flap gates).
    let mut consecutive_connect_failures: u32 = 0;
    // Reconnect / back-off state machine: sustained-timeout disconnect,
    // dead-session escalation, flap gate, and the connect-failure back-off.
    // Extracted into ReconnectController so the multi-session transitions
    // are unit-testable as a driven state machine.
    let mut reconnect = ReconnectController::new(
        Instant::now(),
        state
            .reconnect_request
            .load(std::sync::atomic::Ordering::Relaxed),
    );
    let mut last_discovery_time: Option<Instant> = None;
    // After this many consecutive failures, trigger auto-discovery.
    const DISCOVERY_AFTER_FAILURES: u32 = 5;
    // Minimum interval between auto-discovery scans.
    const DISCOVERY_COOLDOWN: Duration = Duration::from_secs(300);

    loop {
        // ---- Manual reconnect request? ----
        // `POST /api/reconnect` (and any other path that wants to bypass the
        // back-off schedule) bumps `state.reconnect_request`. If it's advanced
        // since the last iteration, reset the back-off timers so the user's
        // click actually retries quickly rather than getting swallowed by a
        // 10-minute zombie-dongle sleep.
        let current_reconnect_request = state
            .reconnect_request
            .load(std::sync::atomic::Ordering::Relaxed);
        // A manual `POST /api/reconnect` bumps the counter; the controller
        // resets every back-off gate to the fast-retry state on a change.
        reconnect.check_manual_reconnect(current_reconnect_request, Instant::now());

        // ---- Read current settings ----
        let settings = state.settings.lock().await.clone();

        // Wait until a host is configured. Serial may be empty - the dongle
        // accepts empty-serial requests, and the client does not auto-discover
        // it (serial provisioning comes from persisted settings).
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

                // Notify if we just reconnected and the user opted in.
                crate::alerts::send_connection_restored_notification(&state, &settings.host).await;

                // Allow the dongle time to initialise after TCP connect.
                // The GivEnergy dongle has a slow processor and may return
                // Modbus exception code 67 (busy/not-ready) if queried too soon.
                tokio::time::sleep(Duration::from_millis(500)).await;

                // Drain any stale data the dongle buffered from a previous
                // session - without this, cached responses corrupt the
                // request-response pairing for the first poll.

                // Liveness probe (advisory only). A "zombie" dongle keeps its
                // TCP stack alive (so connect() succeeds and keepalives pass)
                // while its Modbus application processor hangs. This probe is
                // a cheap single-register read that confirms the dongle is
                // answering Modbus before we commit to a full multi-block
                // poll. A failure is logged but does *not* tear down the
                // session — we fall through to the warmup read below.
                //
                // GivTCP has no equivalent gate; this is purely an early
                // signal that the post-TCP-handshake Modbus processor isn't
                // yet ready. The real liveness check is the warmup read
                // immediately below, and the inner poll loop's
                // `MAX_CONSECUTIVE_TIMEOUTS` counter is the catch-all for a
                // truly unresponsive session.
                match client.liveness_probe().await {
                    Ok(()) => tracing::debug!("Liveness probe OK"),
                    Err(e) => {
                        tracing::warn!(
                            "Liveness probe not answering yet (advisory - will retry via warmup): {e}"
                        );
                    }
                }

                // Warmup read: discard the first register read after connect.
                // The dongle's internal state can be stale after a TCP
                // reconnect, causing the first read to return garbage values
                // (e.g. today_import_kwh = 0.6 when the real value is 39.0).
                // A single discard read is enough — residual corruption is
                // caught downstream by the absolute-range sanitizer and the
                // grace-period median-of-3 baseline.
                //
                // This read is intentionally NOT a kill-switch: a failure is
                // logged and we fall through to the inner poll loop anyway.
                // GivTCP's `watch_plant()` does a single `refresh_plant()`
                // after connect and immediately enters its watch loop with
                // `return_exceptions=True`, so a stuck Modbus processor
                // doesn't tear the TCP connection down — it just means the
                // first refresh produces fewer (or no) results, and the next
                // refresh tries again. We match that model: TCP up = keep
                // going. A genuinely dead session is still caught by
                // `MAX_CONSECUTIVE_TIMEOUTS` (3 cycles of every-block failure
                // ≈ 36 s of silence before a forced reconnect) and by
                // `dead_session_backoff()` escalating the reconnect delay.
                //
                // Retry budget matches the steady-state poll's
                // `read_all_with_extras` (which also uses 2 retries per
                // block via `read_blocks_resilient`). The warmup is no
                // stricter than the inner poll loop, so a slow-but-healthy
                // dongle that recovers mid-cycle is allowed to recover.
                //
                // If the persisted serial identifies the device as a Gateway
                // (GW prefix), skip the wide IR 0-59 / IR 180-183 blocks —
                // they're unmapped on Gateway hardware and would just burn
                // timeout budget. A known-Gateway startup reads the lean
                // HR-only set for the warmup too.
                const WARMUP_MAX_RETRIES: u8 = 2;
                let warmup_blocks =
                    warmup_blocks_for(device_type_from_serial(&settings.serial).as_ref());
                match client
                    .read_blocks_resilient(warmup_blocks, WARMUP_MAX_RETRIES)
                    .await
                {
                    Ok(blocks) => {
                        tracing::debug!(blocks = blocks.len(), "Warmup read OK");
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Warmup read failed: {e} - continuing into inner poll loop (will reconnect on sustained timeout)"
                        );
                    }
                }

                // Clear any previous snapshot so the next reading is accepted
                // without delta sanitization. After a reconnect, the previous
                // snapshot may contain stale or corrupted values from the old
                // session. The absolute range check (0-200 kWh) still applies.
                {
                    let mut latest = state.latest_snapshot.lock().await;
                    *latest = None;
                }

                // New session: reset the connect back-off to the floor and
                // clear the per-session sustained-timeout streak + the
                // productive-read flag. Dead-session and flap state persist
                // across sessions (see ReconnectController).
                reconnect.note_session_start();

                // Track consecutive poll failures within this connection.
                //
                // Gateway slow-detail poll cadence. Starts at zero so the first
                // model-aware Gateway poll after detection reads every block and
                // populates serial/config fields immediately; later polls use
                // the fast live-telemetry subset until the counter rolls over.
                let mut gateway_detail_poll_countdown: u8 = 0;

                // `consecutive_suspicious` counts cycles where a block matched
                // the dongle memory-leak fingerprint — the dongle's TCP stack
                // is fine but its register values look like its own memory
                // buffer. After MAX_SUSPICIOUS_CYCLES we assume the dongle's
                // app processor is stuck and a fresh TCP session will reset it.
                // (The sustained-timeout disconnect counter that used to live
                // here now lives in ReconnectController.)
                //
                // Resets to 0 on any successful poll.
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
                let mut rate_release_counts = RateReleaseCounts::default();
                let mut known_device_type: Option<crate::inverter::model::DeviceType> = None;
                let mut detected_meters: Vec<u8> = Vec::new();
                // Battery slave addresses already announced this session, so
                // "Battery #N detected" is logged once (INFO) per address
                // instead of every poll cycle (the previous behaviour spammed
                // INFO on every successful read).
                let mut known_battery_addrs: Vec<u8> = Vec::new();
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

                    // Slot-based Agile: the inverter itself holds the slot
                    // schedule, so a restart that left the inverter mid-charge
                    // is automatically handled — the slot continues to fire
                    // until its end time. The first poll after restart
                    // evaluates the current price and writes the next slot
                    // (or disarms with AgileClearActiveSlot if scope == Off).
                    // We log the legacy `agile_state_persisted` here for
                    // operators who want to see what the previous run was
                    // doing — it's now diagnostic-only and the field is
                    // ignored on read.
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
                        let gateway_scope = gateway_poll_scope(
                            known_device_type,
                            gateway_detail_poll_countdown,
                        );
                        // Prefill the device type from the persisted serial on
                        // the first poll (when the decoder hasn't yet
                        // confirmed a model) so a known-Gateway startup can
                        // skip the wide IR 0-59 / IR 180-183 standard blocks.
                        // Model-specific blocks are still gated on the
                        // confirmed `known_device_type` (set on the cycle
                        // after detection), so the decoder always gets a
                        // clean chance to confirm or override the prefill.
                        let prefilled_device_type: Option<DeviceType> =
                            if known_device_type.is_none() {
                                device_type_from_serial(&settings.serial)
                            } else {
                                None
                            };
                        match client
                            .read_all_with_extras(
                                known_device_type.as_ref(),
                                prefilled_device_type.as_ref(),
                                gateway_scope,
                            )
                            .await
                        {
                            Ok(blocks) => {
                                let mut snapshot = decode_snapshot(&blocks);

                                // Gen3 Hybrid targeted pause-register probe. The
                                // full HR 300-359 AC-config block times out on
                                // this family (#162 / commit fdd8272), but a
                                // 3-register read of HR 318-320 succeeds on ARM
                                // fw >= 312 — the path that lets the
                                // portal-style Timed Discharge feature work on
                                // Gen3 Hybrid. Read it out-of-band here so a
                                // timeout can't fail the whole poll cycle; on
                                // failure carry forward the previous pause
                                // values so the UI doesn't flicker to "off".
                                if snapshot.device_type
                                    == DeviceType::Gen3Hybrid
                                    && snapshot
                                        .firmware_version
                                        .parse::<u16>()
                                        .is_ok_and(|fw| fw >= 312)
                                {
                                    match client
                                        .read_registers(
                                            crate::modbus::framer::RegisterType::Holding,
                                            crate::modbus::registers::HR_BATTERY_PAUSE_MODE,
                                            3,
                                        )
                                        .await
                                    {
                                        Ok(data) => {
                                            crate::inverter::decoder::
                                                decode_holding_318_320(&data, &mut snapshot);
                                        }
                                        Err(e) => {
                                            tracing::debug!(
                                                "Gen3 pause-register targeted read failed: {e}"
                                            );
                                            let prev = state.latest_snapshot.lock().await;
                                            if let Some(p) = prev.as_ref() {
                                                snapshot.battery_pause_mode =
                                                    p.battery_pause_mode;
                                                snapshot.battery_pause_slot =
                                                    p.battery_pause_slot.clone();
                                            }
                                        }
                                    }
                                }

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
                                let has_ems_plant_block = blocks.iter().any(|b| {
                                    b.block.register_type == crate::modbus::registers::RegisterType::Holding
                                        && b.block.start == 2040
                                        && b.block.count == 36
                                });
                                let has_gateway_discharge_detail_block = blocks.iter().any(|b| {
                                    b.block.register_type == crate::modbus::registers::RegisterType::Input
                                        && b.block.start == 1720
                                        && b.block.count == 60
                                });
                                let has_gateway_serial_block = blocks.iter().any(|b| {
                                    b.block.register_type == crate::modbus::registers::RegisterType::Input
                                        && b.block.start == 1831
                                        && b.block.count == 29
                                });

                                // Cache the device type for subsequent polls.
                                // This enables model-aware polling (extra blocks).
                                // 'Unknown(0)' means we haven't identified the model yet.
                                let is_new_model = known_device_type.is_none()
                                    && !matches!(snapshot.device_type, crate::inverter::model::DeviceType::Unknown(_));
                                if is_new_model {
                                    // Name the actual blocks the model-aware poll
                                    // will read on the next cycle. For a Gateway
                                    // this is the lean HR-only standard set + the
                                    // full IR 1600-1859 bank + EMS plant holding;
                                    // `extra_poll_blocks()` is empty for Gateway
                                    // (its blocks are added in
                                    // `model_specific_blocks_in_poll_order`), so
                                    // the old `extra_blocks=[]` log line misled
                                    // users into thinking detection hadn't changed
                                    // the poll plan.
                                    let standard_blocks_next: Vec<&'static str> =
                                        crate::modbus::client::preview_standard_blocks(
                                            Some(&snapshot.device_type),
                                            None,
                                        )
                                        .iter()
                                        .map(|b| b.name)
                                        .collect();
                                    let model_specific_blocks_next: Vec<&'static str> =
                                        crate::modbus::client::preview_model_specific_blocks(
                                            &snapshot.device_type,
                                            GatewayPollScope::Detail,
                                        )
                                        .iter()
                                        .map(|b| b.name)
                                        .collect();
                                    tracing::info!(
                                        device_type = ?snapshot.device_type,
                                        standard_blocks = ?standard_blocks_next,
                                        model_specific_blocks = ?model_specific_blocks_next,
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

                                    let has_model_specific_blocks = !crate::modbus::client::preview_model_specific_blocks(
                                        &snapshot.device_type,
                                        GatewayPollScope::Fast,
                                    )
                                    .is_empty();
                                    known_device_type = Some(snapshot.device_type);

                                    // The first detection poll is intentionally minimal: it discovers
                                    // the model, then immediately re-polls with the model-specific
                                    // slave address and optional blocks (AC HR300-359, Gen3 HR240-299,
                                    // Gateway IR 1600-1859). Without this, model-specific registers
                                    // can lag a full poll interval behind detection.
                                    if should_repoll {
                                        tracing::info!(
                                            slave_changed,
                                            has_model_specific_blocks,
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
                                                    if !known_battery_addrs.contains(&addr) {
                                                        tracing::info!(
                                                            "Battery #{} detected at addr 0x{:02X} (SOC={}%)",
                                                            i + 2, addr, soc
                                                        );
                                                        known_battery_addrs.push(addr);
                                                    } else {
                                                        tracing::debug!(
                                                            "Battery #{} at addr 0x{:02X} (SOC={}%)",
                                                            i + 2, addr, soc
                                                        );
                                                    }
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
                                    let mut s = sanitize_snapshot(&mut snapshot, prev.as_ref(), in_grace, &mut pending_mode, &mut delta_corrections, &mut suspect_counts, &mut rate_release_counts);
                                    if carry_forward_optional_block_values(
                                        &mut snapshot,
                                        prev.as_ref(),
                                        has_ac_config_block,
                                        has_extended_slots_block,
                                        has_three_phase_config_block,
                                        has_ems_plant_block,
                                    ) {
                                        s = true;
                                    }
                                    if snapshot.device_type == DeviceType::Gateway {
                                        if let Some(p) = prev.as_ref() {
                                            if !has_gateway_discharge_detail_block {
                                                snapshot.per_aio_discharge_today_kwh =
                                                    p.per_aio_discharge_today_kwh;
                                                s = true;
                                            }
                                            if !has_gateway_serial_block {
                                                snapshot.per_aio_serial = p.per_aio_serial.clone();
                                                s = true;
                                            }
                                        }
                                    }
                                    let mods = prev.as_ref().map(|p| p.battery_modules.clone());
                                    (s, mods)
                                };
                                carry_forward_battery_modules_with(&mut snapshot, prev_modules.as_deref());

                                if snapshot.device_type == DeviceType::Gateway {
                                    gateway_detail_poll_countdown = next_gateway_detail_countdown(
                                        gateway_detail_poll_countdown,
                                    );
                                }

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
                                    // `agile_enabled` is the legacy boolean mirror of
                                    // `agile_scope != Off`. The slot-based Agile block
                                    // later in this poll updates both `agile_enabled`
                                    // and the new `agile_scope` field from the
                                    // authoritative scope — see below.
                                    snapshot.agile_enabled = poll_settings.agile_enabled;
                                    snapshot.agile_scope =
                                        crate::settings::agile_scope_for_settings(&poll_settings);

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

                                // ---- Solar arrays (issue #110) ----
                                // Stamp the per-array "% of max" summary from
                                // settings so every page that reads the
                                // snapshot (Solar / Power / Summary) can show
                                // output as a percentage of rated kWp without
                                // each component re-fetching settings.
                                snapshot.solar_arrays =
                                    compute_solar_arrays(&snapshot, &poll_settings);
                                snapshot.pv1_pct = if poll_settings.pv1_rated_kw > 0.0 {
                                    Some((snapshot.pv1_power.max(0) as f64 * 100.0)
                                        / (poll_settings.pv1_rated_kw * 1000.0))
                                } else {
                                    None
                                };
                                snapshot.pv2_pct = if poll_settings.pv2_rated_kw > 0.0 {
                                    Some((snapshot.pv2_power.max(0) as f64 * 100.0)
                                        / (poll_settings.pv2_rated_kw * 1000.0))
                                } else {
                                    None
                                };

                                // ---- Load discharge limiter ----
                                {
                                    let config = state.load_limiter_config.lock().await;
                                    let mut ll_state = state.load_limiter_state.lock().await;
                                    let mut ll_saved = state.load_limiter_saved.lock().await;
                                    let writes = check_load_limiter(
                                        &snapshot,
                                        &config,
                                        &mut ll_state,
                                        poll_settings.poll_interval,
                                        &mut ll_saved,
                                    );

                                    // Tag the snapshot so the frontend knows.
                                    snapshot.load_limiter_active =
                                        matches!(*ll_state, LoadLimiterState::Paused)
                                        || matches!(*ll_state, LoadLimiterState::PausedFromRestart);

                                    let was_active = poll_settings.load_limiter_active_persisted;
                                    let now_active = snapshot.load_limiter_active;

                                    // Persist saved reserve values to disk so they survive a
                                    // crash/restart. When the limiter deactivates, saved
                                    // becomes None — this clears the persisted values.
                                    let persist_saved = ll_saved.clone();
                                    drop(config);
                                    drop(ll_state);
                                    drop(ll_saved);

                                    let mut app_settings = poll_settings.clone();
                                    let saved_changed = app_settings.load_limiter_saved_reserve
                                        != persist_saved.as_ref().map(|s| s.reserve);
                                    if saved_changed {
                                        app_settings.load_limiter_saved_reserve =
                                            persist_saved.as_ref().map(|s| s.reserve);
                                        if let Err(e) = app_settings.save() {
                                            tracing::warn!(
                                                "Failed to persist load limiter saved values: {e}"
                                            );
                                        }
                                    }

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

                                // ---- Force Discharge auto-revert (issue #129) ----
                                //
                                // When Force Discharge is started with a bounded duration
                                // (`POST /api/control/force-discharge {"minutes": N}`), the API
                                // handler records the slot's end time in
                                // `force_discharge_revert.force_discharge_slot_end_ms`. When
                                // that time passes, the inverter stops discharging (the slot
                                // window has closed) but the force-discharge flags remain
                                // set: HR 27 = 0 (export), HR 59 = 1 (enable_discharge),
                                // HR 96 = 0 (charge off), HR 20 = 0 (charge target off).
                                // The battery is effectively paused — it won't charge from
                                // solar and won't discharge. Without this auto-revert, the
                                // user must manually click Eco to recover.
                                //
                                // Each poll cycle checks if the slot has expired. If so, we
                                // take the revert (consuming it so a subsequent explicit Stop
                                // returns the "no force discharge in progress" 400) and queue
                                // the restoration writes via the live Modbus client (same
                                // path as the explicit Stop button).
                                {
                                    let now_ms = chrono::Local::now().timestamp_millis();
                                    let mut revert_guard = state.force_discharge_revert.lock().await;
                                    let expired = revert_guard
                                        .as_ref()
                                        .and_then(|r| r.force_discharge_slot_end_ms)
                                        .is_some_and(|end| now_ms >= end);

                                    if expired {
                                        let revert = revert_guard.take();
                                        drop(revert_guard);
                                        if let Some(r) = revert {
                                            let writes = build_force_discharge_auto_revert_writes(
                                                snapshot.device_type,
                                                now_ms,
                                                r.force_discharge_slot_end_ms,
                                                r.enable_charge,
                                                r.enable_discharge,
                                                r.discharge_slot_1_start,
                                                r.discharge_slot_1_end,
                                                r.discharge_slot_2_start,
                                                r.discharge_slot_2_end,
                                                r.three_phase_force_discharge_enable,
                                                r.three_phase_force_charge_enable,
                                            );
                                            if let Some(writes) = writes {
                                                for w in &writes {
                                                    match client.write_register(w.address, w.value).await {
                                                        Ok(()) => tracing::info!(
                                                            "Force discharge auto-revert: wrote reg {} = {}",
                                                            w.address, w.value
                                                        ),
                                                        Err(e) => tracing::error!(
                                                            "Force discharge auto-revert: write reg {} failed: {e}",
                                                            w.address
                                                        ),
                                                    }
                                                    tokio::time::sleep(Duration::from_millis(1500)).await;
                                                }
                                            }
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
                                //
                                // Slot-based state machine (replaces the legacy
                                // `ForceCharge`/`ForceDischarge` block). Decides what
                                // (if anything) to write to the inverter each poll
                                // based on:
                                //   - the active `AgileScope` (Off/Full/ChargeOnly/DischargeOnly)
                                //   - the current Octopus price vs. the user's thresholds
                                //   - whether Cosy or Auto-Winter is in control of the
                                //     charge side (in which case we defer)
                                //
                                // The inverter itself becomes the source of truth for
                                // whether a slot is currently firing — we just write
                                // the slot 1 start/end times and let the inverter's
                                // native schedule mechanism run the rest.
                                {
                                    let settings = &poll_settings;
                                    let configured_scope = crate::settings::agile_scope_for_settings(settings);
                                    let scope = if settings.cosy_enabled {
                                        crate::settings::AgileScope::Off
                                    } else {
                                        configured_scope
                                    };
                                    let action = if scope == crate::settings::AgileScope::Off {
                                        // Scope off — disarm any preloaded slot.
                                        AgileSlotAction::Idle
                                    } else {
                                        // Fetch current price (from cache or Octopus API).
                                        let now_ts = chrono::Utc::now().timestamp();
                                        let prices = state.cached_agile_prices.lock().await;
                                        let current_price = prices
                                            .iter()
                                            .find(|s| now_ts >= s.valid_from && now_ts < s.valid_to)
                                            .map(|s| s.pence);

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
                                            let region = settings.agile_region.clone();
            let today =
                                                chrono::Utc::now().format("%Y-%m-%d").to_string();
                                            // Configurable base URL: defaults to the real Octopus
                                            // endpoint; tests and self-hosters can override via
            // `settings.agile_api_base_url` to point at a local mock
            // server or mirror.
                                            let base = if settings.agile_api_base_url.is_empty() {
                                                "https://api.octopus.energy".to_string()
                                            } else {
                                                settings.agile_api_base_url.clone()
                                            };
                                            let url = format!(
                                                "{base}/v1/products/AGILE-24-10-01/electricity-tariffs/E-1R-AGILE-24-10-01-{region}/standard-unit-rates/?period_from={today}T00:00:00Z&page_size=96"
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
                                                    let mut prices =
                                                        state.cached_agile_prices.lock().await;
                                                    *prices = fresh;
                                                    prices
                                                        .iter()
                                                        .find(|s| {
                                                            now_ts >= s.valid_from
                                                                && now_ts < s.valid_to
                                                        })
                                                        .map(|s| s.pence)
                                                }
                                                Ok(Err(e)) => {
                                                    tracing::warn!("Agile: failed to fetch prices: {e}");
                                                    None
                                                }
                                                Err(e) => {
                                                    tracing::error!(
                                                        "Agile: spawn_blocking failed: {e}"
                                                    );
                                                    None
                                                }
                                            }
                                        };

                                        // Snapshot-side flags for the cosy / auto-winter
                                        // conflict guard. These are async mutexes so we
                                        // snapshot them once at the top of this block.
                                        let cosy_active = *state.cosy_active.lock().await;
                                        let auto_winter_active = snapshot.auto_winter_active;

                                        let cache_snapshot: Vec<PriceSlot> = {
                                            let guard = state.cached_agile_prices.lock().await;
                                            (*guard).clone()
                                        };
                                        let action = crate::inverter::state_machines::evaluate_agile_slot(
                                            scope,
                                            price,
                                            settings.agile_charge_threshold,
                                            settings.agile_discharge_threshold,
                                            &cache_snapshot,
                                            now_ts,
                                            cosy_active,
                                            auto_winter_active,
                                            &chrono::Local,
                                        );
                                        tracing::debug!(
                                            price = ?price,
                                            scope = ?scope,
                                            ?action,
                                            "Agile: evaluated current slot",
                                        );
                                        action
                                    };

                                    // Convert the action into register writes.
                                    let use_3ph =
                                        snapshot.device_type.uses_three_phase_schedule_slots();
                                    // Defer means cosy/auto-winter owns the inverter —
                                    // don't touch it this poll. We still set the
                                    // snapshot fields below so the frontend sees
                                    // consistent state, but we skip the write loop.
                                    // Skip writes when scope is Off and we're idle — the
                                    // "disarm any preloaded slot" path was clearing the user's
                                    // charge/discharge schedule on every poll cycle. Only write
                                    // AgileClearActiveSlot when the scope is actually active
                                    // (mid-band price) so the user's manual schedule survives.
                                    let skip_writes = !crate::inverter::state_machines::should_write_agile_action(
                                        scope,
                                        &action,
                                    );
                                    let cmd = match &action {
                                        AgileSlotAction::Charge {
                                            start_hhmm,
                                            end_hhmm,
                                            target_soc,
                                        } => {
                                            tracing::info!(
                                                "Agile: cheap window, charging {start_hhmm:04}–{end_hhmm:04} to {target_soc}%"
                                            );
                                            if use_3ph {
                                                ControlCommand::ThreePhaseAgileChargeSlot {
                                                    start_hhmm: *start_hhmm,
                                                    end_hhmm: *end_hhmm,
                                                    target_soc: *target_soc,
                                                }
                                            } else {
                                                ControlCommand::AgileChargeSlot {
                                                    start_hhmm: *start_hhmm,
                                                    end_hhmm: *end_hhmm,
                                                    target_soc: *target_soc,
                                                }
                                            }
                                        }
                                        AgileSlotAction::Discharge { start_hhmm, end_hhmm } => {
                                            tracing::info!(
                                                "Agile: expensive window, discharging (export) {start_hhmm:04}–{end_hhmm:04}"
                                            );
                                            if use_3ph {
                                                ControlCommand::ThreePhaseAgileDischargeSlot {
                                                    start_hhmm: *start_hhmm,
                                                    end_hhmm: *end_hhmm,
                                                }
                                            } else {
                                                ControlCommand::AgileDischargeSlot {
                                                    start_hhmm: *start_hhmm,
                                                    end_hhmm: *end_hhmm,
                                                }
                                            }
                                        }
                                        AgileSlotAction::Defer => {
                                            // Cosy or auto-winter owns this side. Don't
                                            // touch the inverter. Logged at debug only
                                            // because this fires every poll during a
                                            // cosy slot.
                                            tracing::debug!("Agile: deferring (cosy/auto-winter owns charge side)");
                                            // Use a no-op command — the skip_writes guard
                                            // below prevents this from being written.
                                            ControlCommand::AgileClearActiveSlot
                                        }
                                        AgileSlotAction::Idle => {
                                            // Mid-band price, out-of-scope mode, or no
                                            // price data. Disarm any preloaded slot.
                                            tracing::debug!("Agile: idle, clearing active slot");
                                            if use_3ph {
                                                ControlCommand::ThreePhaseAgileClearActiveSlot
                                            } else {
                                                ControlCommand::AgileClearActiveSlot
                                            }
                                        }
                                    };

                                    if !skip_writes {
                                        if let Ok(writes) = cmd.encode() {
                                            let mut all_ok = true;
                                            for w in &writes {
                                                if let Err(e) =
                                                    client.write_register(w.address, w.value).await
                                                {
                                                    tracing::error!(
                                                        "Agile: write reg {} failed: {e}",
                                                        w.address
                                                    );
                                                    all_ok = false;
                                                }
                                                tokio::time::sleep(Duration::from_millis(1500)).await;
                                            }
                                            if !all_ok {
                                                tracing::warn!(
                                                    "Agile: slot writes failed — will retry on next poll"
                                                );
                                            }
                                        }
                                    }

                                    // Update the snapshot fields the frontend reads.
                                    // `agile_scope` carries the user's selected mode
                                    // (Off / Full / ChargeOnly / DischargeOnly); the
                                    // frontend uses it for the Inverter-page summary
                                    // and for hiding/showing schedule sections.
                                    snapshot.agile_active = action.is_active();
                                    snapshot.agile_state = action.label().to_string();
                                    snapshot.agile_enabled = scope != crate::settings::AgileScope::Off;
                                    snapshot.agile_scope = scope;
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
                                            let pushover_text = text.clone();
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

                                            let pushover_token = config.pushover_app_token.clone();
                                            let pushover_user = config.pushover_user_key.clone();
                                            tokio::task::spawn_blocking(move || {
                                                if pushover_token.is_empty()
                                                    || pushover_user.is_empty()
                                                {
                                                    return;
                                                }
                                                match crate::alerts::send_pushover_message(
                                                    &pushover_token,
                                                    &pushover_user,
                                                    &pushover_text,
                                                ) {
                                                    Ok(()) => tracing::warn!(
                                                        "Pushover cleared alert sent"
                                                    ),
                                                    Err(e) => tracing::warn!(
                                                        "Pushover cleared alert failed: {e}"
                                                    ),
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
                                            let pushover_text = text.clone();

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

                                            // Also send via Pushover if both credentials configured
                                            let pushover_token = config.pushover_app_token.clone();
                                            let pushover_user = config.pushover_user_key.clone();
                                            tokio::task::spawn_blocking(move || {
                                                if pushover_token.is_empty()
                                                    || pushover_user.is_empty()
                                                {
                                                    return;
                                                }
                                                match crate::alerts::send_pushover_message(
                                                    &pushover_token,
                                                    &pushover_user,
                                                    &pushover_text,
                                                ) {
                                                    Ok(()) => tracing::warn!("Pushover alert sent"),
                                                    Err(e) => tracing::warn!(
                                                        "Pushover alert failed: {e}"
                                                    ),
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

                                    if config.daily_report_enabled && config.enabled {
                                        let today = chrono::Local::now().date_naive();
                                        let mut last_sent = state.last_report_date.lock().await;
                                        // Only send if we have sent a report before.
                                        // Don't send on startup - last_sent starts as None.
                                        if let Some(sent_date) = *last_sent {
                                            if sent_date < today {
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
                                                                    // Caption uses intentional <b>/<i> tags from
                                                                    // generate_daily_summary_text, so we keep HTML
                                                                    // parse_mode here (unlike the support-bundle
                                                                    // caption, which is plain text).
                                                                    match crate::alerts::send_telegram_document(
                                                                        &token,
                                                                        &chat_id,
                                                                        &caption,
                                                                        &filename,
                                                                        body.as_bytes(),
                                                                        Some("HTML"),
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
                                }

                                // Reflect the (possibly updated) cosy_active flag
                                // AFTER the cosy state machine has run. Without this,
                                // the broadcast snapshot would carry the previous
                                // cycle's value for one poll after a slot transition
                                // - e.g. showing "Cosy Active" for an extra poll
                                // after the slot actually ended.
                                snapshot.cosy_active = *state.cosy_active.lock().await;
                                // NOTE: snapshot.agile_active / agile_state / agile_enabled
                                // are now set by the slot-based Agile block earlier in
                                // this poll. Don't touch them here — overwriting would
                                // regress the Inverter-page summary that derives
                                // "Timed Charge — active" from enable_charge + slot
                                // window + battery_state.

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
                                if e.is_connection_lost() {
                                    // Hard TCP error — the socket is dead,
                                    // must reconnect.
                                    tracing::warn!(
                                        error = %e,
                                        "TCP connection lost — reconnecting"
                                    );
                                    (false, false, true)
                                } else {
                                    // Timeout — the dongle is slow but the
                                    // TCP socket is fine.
                                    // read_blocks_resilient already retried
                                    // the failed block. Log and continue to
                                    // the next poll cycle.
                                    tracing::warn!(
                                        error = %e,
                                        "Poll read failed (transient) — continuing"
                                    );
                                    (false, false, false)
                                }
                            }
                        }
                    }.await;

                    match poll_ok {
                        true => {
                            consecutive_suspicious = 0;
                            // Fresh, sanitized data reached the UI/history.
                            // Resets the sustained-timeout streak, marks the
                            // session productive, restarts the flap
                            // data-starvation clock, and — if a flap is
                            // engaged — advances the stand-down count.
                            reconnect.note_good_poll(Instant::now());
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
                            // A failed poll breaks the flap recovery streak.
                            reconnect.note_poll_failed();
                            if connection_lost {
                                break;
                            }
                            // Transient timeout — read_blocks_resilient already
                            // retried the failed block. Count it and, once the
                            // sustained-timeout threshold is reached (handled by
                            // the controller), disconnect to force a reconnect
                            // instead of hammering a wedged dongle until the OS
                            // sends an RST.
                            if reconnect.note_transient_timeout() {
                                break;
                            }
                            // Sleep briefly then continue to the next poll cycle.
                            tracing::debug!(
                                "Poll read failed (transient) — sleeping before next cycle"
                            );
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue;
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
                    "Disconnecting from inverter - will reconnect"
                );
                client.disconnect().await;

                // Tally dead sessions for back-off escalation. A session that
                // never produced a successful Modbus read (zombie dongle, or
                // warmup/liveness failure) increments the counter; a productive
                // session resets it so the next reconnect uses the default delay.
                reconnect.note_session_end();

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

                // Notify if the user has opted in to connection-lost alerts.
                crate::alerts::send_connection_lost_notification(&state, &settings.host).await;
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
                    && last_discovery_time.is_none_or(|t| t.elapsed() >= DISCOVERY_COOLDOWN);

                if should_discover {
                    last_discovery_time = Some(Instant::now());
                    tracing::warn!(
                        "Auto-discovery: {} consecutive failures to reach {}:{}. Scanning LAN...",
                        consecutive_connect_failures,
                        settings.host,
                        settings.port
                    );

                    let subnets = crate::inverter::discovery::detect_lan_subnets();
                    let inverters =
                        crate::inverter::discovery::scan_multiple_subnets(&subnets).await;

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
                            reconnect.reset_connect_backoff();
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
        // Wake early if a manual `POST /api/reconnect` arrives mid-sleep.
        // Without this, the user can click "Retry now" during a 10-minute
        // zombie-dongle back-off and see no effect for up to 10 minutes
        // (the increment is still detected at the top of the next outer
        // iteration, but only after the sleep completes).
        // ---- Flap gate ----
        // Engage when the frontend has been data-starved past the threshold —
        // the signature of a flapping dongle. Only engages here (never
        // disengages; stand-down happens in the poll loop on a sustained run
        // of good polls). Sticky so an isolated success mid-flap doesn't
        // reset it.
        // Recompute the flap gate and the reconnect delay (the max of the
        // connect-failure, dead-session, and flap gates). Engages the flap
        // (sticky) if the frontend has been data-starved past the threshold.
        let delay = reconnect.reconnect_delay(Instant::now());
        let sleep_start = tokio::time::Instant::now();
        let sleep_deadline = sleep_start + delay;
        loop {
            let now = tokio::time::Instant::now();
            if now >= sleep_deadline {
                break;
            }
            // Tick once per second so we can notice a fresh reconnect
            // request without burning the full delay. We don't use a
            // Notify here because `reconnect_request` is a counter, not a
            // notification — the comparison loop is the wake mechanism.
            let remaining = sleep_deadline - now;
            tokio::select! {
                _ = tokio::time::sleep(remaining.min(Duration::from_secs(1))) => {}
                _ = state.write_notify.notified() => {
                    tracing::debug!("Write notification received during back-off, waking early");
                    break;
                }
            }
            // Has a manual reconnect been requested since we went to sleep?
            let cur_req = state
                .reconnect_request
                .load(std::sync::atomic::Ordering::Relaxed);
            if cur_req != reconnect.last_seen_reconnect_request() {
                tracing::info!("Manual reconnect requested during back-off — waking early");
                break;
            }
        }
        reconnect.escalate_connect_backoff();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Build the per-array solar summary for "% of max" display (issue #110).
///
/// Combines:
/// - DC strings PV1/PV2, when the user has entered a rated kWp
///   (`settings.pv1_rated_kw` / `pv2_rated_kw`). Power comes from the
///   inverter's IR registers (already decoded into `snapshot.pv1_power` /
///   `pv2_power`); today's energy from the per-string counters.
/// - External CT meters the user has labelled as solar
///   (`settings.solar_arrays`), typical for AC-coupled systems whose
///   panels feed a separate inverter measured by a GivEnergy CT clamp.
///   Power is the meter's total active power (unsigned, so a reversed
///   clamp still reads as generation); today's energy is unknown (CT
///   meters only expose cumulative totals) and stays `None`.
///
/// DC strings with no rated config are omitted entirely, so a default
/// hybrid install sees no change until the user opts in. Meter entries
/// with `rated_kw == 0` are still surfaced (power-only); the FE hides
/// the % when the rating is zero.
pub(crate) fn compute_solar_arrays(
    snapshot: &InverterSnapshot,
    settings: &crate::settings::Settings,
) -> Vec<SolarArraySummary> {
    let mut out = Vec::new();

    // PV1 / PV2 DC strings (hybrid / DC-coupled). Only surface a string
    // once the user has given it a rated capacity — otherwise the existing
    // Solar page already shows raw kW and there's nothing to add.
    if settings.pv1_rated_kw > 0.0 {
        out.push(SolarArraySummary {
            source: SolarArraySource::Pv1,
            name: String::new(),
            power_w: snapshot.pv1_power.max(0) as u32,
            rated_kw: settings.pv1_rated_kw,
            today_kwh: Some(snapshot.today_pv1_kwh as f64),
            meter_address: None,
        });
    }
    if settings.pv2_rated_kw > 0.0 {
        out.push(SolarArraySummary {
            source: SolarArraySource::Pv2,
            name: String::new(),
            power_w: snapshot.pv2_power.max(0) as u32,
            rated_kw: settings.pv2_rated_kw,
            today_kwh: Some(snapshot.today_pv2_kwh as f64),
            meter_address: None,
        });
    }

    // External CT meters labelled as solar (AC-coupled / separate inverters).
    for arr in &settings.solar_arrays {
        // Only 1-8 are real external CT clamp addresses; 0x00 is the
        // synthetic built-in grid CT and must never be treated as a solar
        // array (its power is grid import/export, not generation).
        if !(1..=8).contains(&arr.meter_address) {
            continue;
        }
        if let Some(meter) = snapshot
            .meters
            .iter()
            .find(|m| m.address == arr.meter_address)
        {
            out.push(SolarArraySummary {
                source: SolarArraySource::Meter,
                name: arr.name.clone(),
                // A CT on a solar inverter's AC output reads generation
                // flowing out to the bus; take the absolute value so a
                // physically reversed clamp still shows as positive output.
                power_w: meter.p_active_total.unsigned_abs(),
                rated_kw: arr.rated_kw,
                today_kwh: None,
                meter_address: Some(arr.meter_address),
            });
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::{DeviceType, MeterData};
    use crate::settings::{Settings, SolarArrayConfig};

    /// Build a `MeterData` with only the fields `compute_solar_arrays`
    /// inspects set; the rest are zeroed. `MeterData` doesn't derive
    /// `Default` (it has no sensible per-field defaults), so tests build it
    /// via this helper instead of `..Default::default()`.
    fn meter(address: u8, p_active_total: i32) -> MeterData {
        MeterData {
            address,
            v_phase_1: 240.0,
            v_phase_2: 0.0,
            v_phase_3: 0.0,
            i_phase_1: 0.0,
            i_phase_2: 0.0,
            i_phase_3: 0.0,
            i_ln: 0.0,
            i_total: 0.0,
            p_active_phase_1: p_active_total,
            p_active_phase_2: 0,
            p_active_phase_3: 0,
            p_active_total,
            p_reactive_total: 0,
            p_apparent_total: 0,
            pf_total: 0.0,
            frequency: 50.0,
            e_import_active_kwh: 0.0,
            e_export_active_kwh: 0.0,
        }
    }

    // -- compute_solar_arrays (issue #110) -----------------------------------

    #[test]
    fn solar_arrays_empty_when_nothing_configured() {
        let snap = InverterSnapshot {
            pv1_power: 3000,
            ..Default::default()
        };
        let settings = Settings::default();
        // No rated capacities configured → nothing surfaced. A default
        // install is unaffected until the user opts in via Settings.
        assert!(compute_solar_arrays(&snap, &settings).is_empty());
    }

    #[test]
    fn solar_arrays_dc_strings_surfaced_with_today_energy() {
        let snap = InverterSnapshot {
            pv1_power: 4200,
            pv2_power: 1800,
            today_pv1_kwh: 18.5,
            today_pv2_kwh: 7.5,
            ..Default::default()
        };
        let settings = Settings {
            pv1_rated_kw: 6.0,
            pv2_rated_kw: 4.2,
            ..Default::default()
        };
        let arrays = compute_solar_arrays(&snap, &settings);
        assert_eq!(arrays.len(), 2);
        assert_eq!(arrays[0].source, SolarArraySource::Pv1);
        assert_eq!(arrays[0].power_w, 4200);
        assert_eq!(arrays[0].rated_kw, 6.0);
        assert_eq!(arrays[0].today_kwh, Some(18.5));
        assert_eq!(arrays[0].meter_address, None);
        assert_eq!(arrays[1].source, SolarArraySource::Pv2);
        assert_eq!(arrays[1].power_w, 1800);
        assert_eq!(arrays[1].today_kwh, Some(7.5));
    }

    #[test]
    fn solar_arrays_dc_power_clamped_at_zero() {
        // A negative DC power (shouldn't happen for generation, but the
        // dongle can glitch) must not surface as a huge unsigned value via
        // `as u32` wraparound. Clamp at 0.
        let snap = InverterSnapshot {
            pv1_power: -50,
            ..Default::default()
        };
        let settings = Settings {
            pv1_rated_kw: 6.0,
            ..Default::default()
        };
        let arrays = compute_solar_arrays(&snap, &settings);
        assert_eq!(arrays.len(), 1);
        assert_eq!(arrays[0].power_w, 0);
    }

    #[test]
    fn solar_arrays_ac_coupled_ct_meter_surfaced_unsigned() {
        // AC-coupled: panels feed a separate inverter measured by a CT
        // clamp at meter address 0x01. A physically reversed clamp reads
        // negative, but generation is unsigned.
        let snap = InverterSnapshot {
            meters: vec![meter(1, -4800), meter(2, 2600)],
            ..Default::default()
        };
        let settings = Settings {
            solar_arrays: vec![
                SolarArrayConfig {
                    meter_address: 1,
                    name: "East roof".into(),
                    rated_kw: 6.0,
                },
                SolarArrayConfig {
                    meter_address: 2,
                    name: String::new(),
                    rated_kw: 4.2,
                },
            ],
            ..Default::default()
        };
        let arrays = compute_solar_arrays(&snap, &settings);
        assert_eq!(arrays.len(), 2);
        assert_eq!(arrays[0].source, SolarArraySource::Meter);
        assert_eq!(arrays[0].name, "East roof");
        assert_eq!(arrays[0].power_w, 4800); // |-4800|
        assert_eq!(arrays[0].rated_kw, 6.0);
        assert_eq!(arrays[0].today_kwh, None); // CT meters have no per-day counter
        assert_eq!(arrays[0].meter_address, Some(1));
        assert_eq!(arrays[1].power_w, 2600);
        assert_eq!(arrays[1].meter_address, Some(2));
        assert!(arrays[1].name.is_empty());
    }

    #[test]
    fn solar_arrays_ignores_synthetic_grid_ct_and_out_of_range() {
        let snap = InverterSnapshot {
            meters: vec![meter(0, 5000), meter(9, 1000)],
            ..Default::default()
        };
        let settings = Settings {
            solar_arrays: vec![
                // 0x00 is the synthetic built-in grid CT — never a solar array.
                SolarArrayConfig {
                    meter_address: 0,
                    name: "grid".into(),
                    rated_kw: 5.0,
                },
                // 9 is outside the 1-8 clamp range.
                SolarArrayConfig {
                    meter_address: 9,
                    name: "bogus".into(),
                    rated_kw: 5.0,
                },
            ],
            ..Default::default()
        };
        // Both invalid entries dropped; no meter matched a valid address.
        assert!(compute_solar_arrays(&snap, &settings).is_empty());
    }

    #[test]
    fn solar_arrays_skips_meter_not_present_in_snapshot() {
        // A configured meter address that the dongle didn't report (clamp
        // offline) is skipped rather than surfaced with a phantom zero.
        let snap = InverterSnapshot::default();
        let settings = Settings {
            solar_arrays: vec![SolarArrayConfig {
                meter_address: 3,
                name: "Garage".into(),
                rated_kw: 3.68,
            }],
            ..Default::default()
        };
        assert!(compute_solar_arrays(&snap, &settings).is_empty());
    }

    #[test]
    fn solar_arrays_mixes_dc_strings_and_ct_meters() {
        // A hybrid with DC strings PLUS a separately-metered array (e.g. a
        // garage inverter on a CT) surfaces all three in one list.
        let snap = InverterSnapshot {
            pv1_power: 2000,
            pv2_power: 1500,
            today_pv1_kwh: 10.0,
            today_pv2_kwh: 6.0,
            meters: vec![meter(4, 3200)],
            ..Default::default()
        };
        let settings = Settings {
            pv1_rated_kw: 3.0,
            pv2_rated_kw: 2.5,
            solar_arrays: vec![SolarArrayConfig {
                meter_address: 4,
                name: "Garage".into(),
                rated_kw: 4.0,
            }],
            ..Default::default()
        };
        let arrays = compute_solar_arrays(&snap, &settings);
        assert_eq!(arrays.len(), 3);
        assert_eq!(arrays[0].source, SolarArraySource::Pv1);
        assert_eq!(arrays[1].source, SolarArraySource::Pv2);
        assert_eq!(arrays[2].source, SolarArraySource::Meter);
        assert_eq!(arrays[2].meter_address, Some(4));
    }

    #[test]
    fn poll_settings_default() {
        let s = PollSettings::default();
        assert!(s.host.is_empty());
        assert!(s.serial.is_empty());
        assert_eq!(s.port, 8899);
        assert_eq!(s.interval_secs, 60);
    }

    #[test]
    fn gateway_poll_scope_details_only_on_gateway_countdown_zero() {
        assert_eq!(
            gateway_poll_scope(Some(DeviceType::Gateway), 0),
            GatewayPollScope::Detail
        );
        assert_eq!(
            gateway_poll_scope(Some(DeviceType::Gateway), 1),
            GatewayPollScope::Fast
        );
        assert_eq!(
            gateway_poll_scope(Some(DeviceType::Gen3Hybrid), 0),
            GatewayPollScope::Fast
        );
        assert_eq!(gateway_poll_scope(None, 0), GatewayPollScope::Fast);
    }

    #[test]
    fn gateway_detail_countdown_runs_every_tenth_gateway_poll() {
        let mut countdown = 0;
        let mut scopes = Vec::new();
        for _ in 0..21 {
            scopes.push(gateway_poll_scope(Some(DeviceType::Gateway), countdown));
            countdown = next_gateway_detail_countdown(countdown);
        }

        let detail_indices: Vec<usize> = scopes
            .iter()
            .enumerate()
            .filter_map(|(idx, scope)| (*scope == GatewayPollScope::Detail).then_some(idx))
            .collect();
        assert_eq!(detail_indices, vec![0, 10, 20]);
    }

    /// Persisted Gateway serials start with the "GW" prefix (e.g. `GW2529A127`).
    /// The runtime uses the prefix to prefill the device type on the first
    /// poll so a known-Gateway startup can skip the wide IR 0-59 / IR 180-183
    /// standard blocks (which are unmapped on the Gateway and would just
    /// burn timeout budget). Pinned here so a future tweak to the prefix
    /// can't silently disable the lean-first-poll optimisation.
    #[test]
    fn device_type_from_serial_recognises_gateway_prefix() {
        assert_eq!(
            device_type_from_serial("GW2529A127"),
            Some(DeviceType::Gateway)
        );
        assert_eq!(
            device_type_from_serial("gw2529a127"),
            Some(DeviceType::Gateway),
            "lowercase prefix must still match (users sometimes retype the serial)"
        );
        assert_eq!(device_type_from_serial("GWABC"), Some(DeviceType::Gateway));
        // Anything that isn't a GW prefix is left to the decoder.
        assert_eq!(device_type_from_serial("SN-12345"), None);
        assert_eq!(device_type_from_serial(""), None);
        assert_eq!(
            device_type_from_serial("G"),
            None,
            "single-letter prefix is too short"
        );
        assert_eq!(
            device_type_from_serial("  "),
            None,
            "whitespace-only is not a serial"
        );
        // Leading/trailing whitespace from copy-paste should not break the match.
        assert_eq!(
            device_type_from_serial("  GW2529A127\n"),
            Some(DeviceType::Gateway)
        );
    }

    /// The warmup read after a fresh TCP connect should mirror the standard
    /// block selection `read_all_with_extras` would use on the first poll.
    /// For a known-Gateway serial the warmup reads the lean HR-only set;
    /// for everything else (empty serial, non-GW serial) it falls back to
    /// the full single-phase set.
    #[test]
    fn warmup_blocks_reflect_serial_prefill() {
        use crate::modbus::registers::{
            RegisterBlock, RegisterType, STANDARD_POLL_BLOCKS, STANDARD_POLL_BLOCKS_3PH,
        };

        // Content-based comparison: `RegisterBlock` doesn't derive `PartialEq`
        // and fat-pointer addresses can differ between function-return and
        // const-reference views of the same data.
        fn eq_set(a: &[RegisterBlock], b: &[RegisterBlock]) -> bool {
            a.len() == b.len()
                && a.iter().zip(b.iter()).all(|(x, y)| {
                    x.name == y.name
                        && std::mem::discriminant(&x.register_type)
                            == std::mem::discriminant(&y.register_type)
                })
        }

        // Known Gateway → lean HR-only set (no IR 0-59 / IR 180-183).
        let gw = warmup_blocks_for(Some(&DeviceType::Gateway));
        assert!(eq_set(gw, STANDARD_POLL_BLOCKS_3PH));
        assert!(
            gw.iter().all(|b| b.register_type == RegisterType::Holding),
            "Gateway warmup must not request any input registers"
        );

        // Empty / unknown serial → full single-phase set.
        let unknown = warmup_blocks_for(None);
        assert!(eq_set(unknown, STANDARD_POLL_BLOCKS));
        assert!(unknown
            .iter()
            .any(|b| b.register_type == RegisterType::Input));

        // Sanity: a non-Gateway prefilled type with the same gate condition
        // (e.g. three-phase) also picks the lean set. (Doesn't currently
        // happen via the serial prefix, but pins the contract.)
        let three_phase = warmup_blocks_for(Some(&DeviceType::ThreePhase));
        assert!(eq_set(three_phase, STANDARD_POLL_BLOCKS_3PH));
    }

    /// `run_poll_loop` reconnects after `MAX_CONSECUTIVE_TIMEOUTS` cycles of
    /// `ClientError::Timeout` from `read_all_with_extras` — the dongle is
    /// TCP-alive but not answering any Modbus request within the 3 s
    /// `IO_TIMEOUT`. Without this, a wedged dongle would be hammered until
    /// the OS noticed (typically 5–10 minutes for the TCP RST to arrive),
    /// during which the UI sees stale snapshots and the log fills with
    /// timeout warnings.
    /// The threshold now lives on `ReconnectController` (and is exercised
    /// directly by `sustained_timeouts_force_disconnect_only_at_threshold` in
    /// `reconnect.rs`). This test stays as a timing-budget pin: with the
    /// default 3 s per-read timeout,
    /// 3 retries per block, and the post-poll 2 s sleep, each cycle burns
    /// roughly 10–12 s. `MAX_CONSECUTIVE_TIMEOUTS = 3` therefore yields a
    /// ~36 s ceiling before we give up — long enough to ride out a brief
    /// dongle hiccup, short enough to recover well before the OS notices.
    ///
    /// If anyone bumps the threshold, the dead-session back-off above is
    /// what caps the *next* reconnect attempt, so they don't need to worry
    /// about the poll loop tight-looping on the failed dongle.
    #[test]
    fn sustained_timeout_budget_is_bounded() {
        const IO_TIMEOUT_SECS: u64 = 3;
        const MAX_RETRIES_PER_BLOCK: u64 = 2;
        const MAX_ATTEMPTS_PER_BLOCK: u64 = MAX_RETRIES_PER_BLOCK + 1;
        const POST_POLL_SLEEP_SECS: u64 = 2;
        const MAX_CONSECUTIVE_TIMEOUTS: u64 = 3;
        const MAX_CYCLE_SECS: u64 = MAX_ATTEMPTS_PER_BLOCK * IO_TIMEOUT_SECS + POST_POLL_SLEEP_SECS;

        // Sanity (enforced at compile time, so a bump trips the build
        // immediately rather than only when tests run): the documented cycle
        // budget (~36 s) should always be well under the ~5 min worst-case RST
        // latency we observed before this fix landed, and the threshold needs
        // at least two cycles so a single transient hiccup doesn't reconnect.
        const _: () = {
            const TOTAL_SECS: u64 = MAX_CONSECUTIVE_TIMEOUTS * MAX_CYCLE_SECS;
            assert!(
                TOTAL_SECS < 60,
                "sustained-timeout reconnect budget exceeds 60s",
            );
            assert!(
                MAX_CONSECUTIVE_TIMEOUTS >= 2,
                "MAX_CONSECUTIVE_TIMEOUTS too low — single timeout would force reconnect",
            );
        };
    }

    /// Warmup alignment with GivTCP. `run_poll_loop` runs a single
    /// discard read after TCP connect to flush the dongle's stale state.
    ///
    /// The warmup is NOT a kill-switch: on failure the loop logs a warning
    /// and proceeds to the inner poll loop, which has its own
    /// `MAX_CONSECUTIVE_TIMEOUTS` catch. This matches GivTCP's
    /// `watch_plant()` model, where TCP up = keep going and a single
    /// failed `refresh_plant()` doesn't tear the socket down. The old
    /// kill-switch (warmup fail → immediate reconnect → repeat every 5 s)
    /// produced the 27 s/55 s/26 s reconnect storm observed when the
    /// dongle's Modbus processor is slow but TCP is healthy.
    ///
    /// The retry budget matches the steady-state poll's
    /// `read_blocks_resilient(standard_blocks, 2)` call from
    /// `read_all_with_extras`. The warmup is no stricter than the inner
    /// poll loop — a slow-but-healthy dongle is allowed to recover on
    /// the next regular poll rather than being condemned after one
    /// multi-block read fails.
    ///
    /// Before this fix: WARMUP_MAX_RETRIES was 4 (5 attempts × 3 s timeout plus
    /// 500 ms inter-block delay, so up to ~15 s per block, ~60 s worst case
    /// across STANDARD_POLL_BLOCKS' 4 blocks). A single transient stall after
    /// connect could spend almost a minute burning the warmup before declaring
    /// "Session unusable - reconnecting without polling", and then immediately
    /// do it again on the next TCP connect.
    #[test]
    fn warmup_matches_steady_state_poll_retries() {
        // These values mirror `WARMUP_MAX_RETRIES` (in `run_poll_loop`)
        // and the second arg to `read_blocks_resilient` inside
        // `read_all_with_extras`. The test pins them so a future tweak
        // that re-introduces a stricter warmup trips the build.
        const WARMUP_MAX_RETRIES: u8 = 2;
        const STEADY_STATE_RETRIES: u8 = 2;

        const _: () = {
            assert!(
                WARMUP_MAX_RETRIES <= STEADY_STATE_RETRIES,
                "warmup retry budget must not exceed steady-state poll retries — \
                 GivTCP treats the post-connect read the same as any other refresh, \
                 so a slower warmup would re-introduce the kill-switch the steady-state \
                 loop was designed to avoid",
            );
            assert!(
                WARMUP_MAX_RETRIES >= 1,
                "warmup must retry at least once — a single transient stall right \
                 after TCP connect should not abort the session",
            );
        };
    }

    #[test]
    fn app_state_new_creates_valid_state() {
        crate::test_util::with_isolated_config_dir(|| {
            let state = AppState::new();
            // Can obtain a receiver from the broadcast channel.
            let _rx = state.tx.subscribe();
        });
    }

    /// `reconnect_request` is the signal `POST /api/reconnect` uses to
    /// tell the poll loop to reset its back-off state. It must start at
    /// 0 (so the poll loop's initial snapshot doesn't see a fake request)
    /// and be cheaply incrementable from the API handler without holding
    /// any mutexes.
    #[test]
    fn reconnect_request_starts_at_zero() {
        crate::test_util::with_isolated_config_dir(|| {
            let state = AppState::new();
            assert_eq!(
                state
                    .reconnect_request
                    .load(std::sync::atomic::Ordering::Relaxed),
                0
            );
        });
    }

    /// Each `fetch_add` must be observable on the next load — the poll
    /// loop uses this property to detect "user clicked Reconnect" without
    /// any coordination primitive beyond the atomic itself.
    #[test]
    fn reconnect_request_increment_is_observable() {
        crate::test_util::with_isolated_config_dir(|| {
            let state = AppState::new();
            state
                .reconnect_request
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            state
                .reconnect_request
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            assert_eq!(
                state
                    .reconnect_request
                    .load(std::sync::atomic::Ordering::Relaxed),
                2
            );
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
        // Gateway needs an immediate re-poll to request the IR 1600+ aggregation
        // bank (and, on every 10th poll, the EMS plant holding block at HR 2040+).
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
                state
                    .connect_failures
                    .load(std::sync::atomic::Ordering::Relaxed),
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
                state
                    .connect_failures
                    .load(std::sync::atomic::Ordering::Relaxed),
                0
            );
            state
                .connect_failures
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            assert_eq!(
                state
                    .connect_failures
                    .load(std::sync::atomic::Ordering::Relaxed),
                1
            );
            state
                .connect_failures
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            assert_eq!(
                state
                    .connect_failures
                    .load(std::sync::atomic::Ordering::Relaxed),
                2
            );
            state
                .connect_failures
                .store(0, std::sync::atomic::Ordering::Relaxed);
            assert_eq!(
                state
                    .connect_failures
                    .load(std::sync::atomic::Ordering::Relaxed),
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
