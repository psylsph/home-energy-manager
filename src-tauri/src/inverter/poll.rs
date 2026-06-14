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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Timelike;

use crate::server::logs::LogRing;
use crate::server::ws::ConnectedClients;
use tokio::sync::{broadcast, Mutex, Notify};

use crate::history::HistoryDb;
use crate::inverter::decoder::decode_snapshot;
use crate::inverter::encoder::{ControlCommand, RegisterWrite};
use crate::inverter::model::{BatteryMode, DeviceType, InverterSnapshot};
use crate::modbus::client::ModbusClient;
use crate::modbus::registers::{
    encode_hhmm, HR_BATTERY_POWER_MODE, HR_BATTERY_SOC_RESERVE, HR_CHARGE_SLOT_1_END,
    HR_CHARGE_SLOT_1_START, HR_CHARGE_TARGET_SOC, HR_ENABLE_CHARGE, HR_ENABLE_CHARGE_TARGET,
    HR_ENABLE_DISCHARGE,
};

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
    },
    /// EV charger data has been polled.
    Evc(Box<crate::evc::EvcSnapshot>),
    /// EV charger is disconnected.
    EvcDisconnected,
}

// ---------------------------------------------------------------------------
// Agile Octopus price types
// ---------------------------------------------------------------------------

/// A single half-hour price slot from the Octopus API.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    /// EV Charger IP address (standard Modbus TCP).
    pub evc_host: String,
    /// EV Charger TCP port (default 502).
    pub evc_port: u16,
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

// ---------------------------------------------------------------------------
// Load discharge limiter
// ---------------------------------------------------------------------------

/// State machine for the load discharge limiter.
///
/// Monitors `home_power` and pauses battery discharge (Eco Paused) when
/// home load exceeds a threshold for a sustained period, then restores
/// Eco mode when the load drops below the threshold for the same period.
/// Only operates when the battery is in Eco mode and no other automated
/// mode (auto-winter, Cosy, Agile) is active.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub enum LoadLimiterState {
    /// Limiter idle — not monitoring.
    #[default]
    Idle,
    /// Home load above threshold, counting towards trigger delay.
    HighLoadPending {
        /// Consecutive polls where home_power was above threshold.
        consecutive: u32,
    },
    /// Limiter active — battery discharge is paused (Eco Paused).
    Paused,
    /// Restored from persistence after a crash — first poll will check
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
    /// Uses parking_lot::Mutex (not tokio::sync::Mutex) because all
    /// operations are synchronous (lock/unlock within a single .await),
    /// and parking_lot avoids the async Mutex's fairness overhead.
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
            latest_evc: Arc::new(Mutex::new(None)),
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
            latest_evc: Arc::new(Mutex::new(None)),
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
/// Check whether raw register data matches the known GivEnergy dongle memory-leak
/// corruption fingerprint (ported from givenergy-modbus `read_registers.py:is_suspicious()`).
///
/// The GivEnergy data adapter sometimes returns its own internal memory buffer
/// (TCP/IP stack, DHCP lease data, network interface names) instead of actual
/// inverter register values. This manifests as specific hex values at characteristic
/// offsets within a 60-register block (e.g. `0xC0A8` = `192.168` at offset 41/43
/// — the dongle's IP address leaking into register space).
///
/// The fingerprint was established empirically from the givenergy-modbus reference
/// library. If more than 5 of the known-leaked values appear at their characteristic
/// positions, the block is almost certainly corrupted and should trigger a re-poll.
fn is_block_suspicious(data: &[u16]) -> bool {
    if data.len() != 60 {
        return false;
    }
    // Count matches against the known 60-register corruption pattern.
    let count = [
        data[28] == 0x4C32,
        data[30] == 0xA119,
        data[31] == 0x34EA,
        data[32] == 0xE77F,
        data[33] == 0xD475,
        data[35] == 0x4500,
        data[40] == 0xE4F9 || data[40] == 0xB619,
        data[41] == 0xC0A8,
        data[43] == 0xC0A8,
        data[46] == 0xC5E9,
        data[50] == 0x60EF || data[50] == 0x503C,
        data[51] == 0x8018,
        data[52] == 0x43E0,
        data[53] == 0xF6CE,
        data[56] == 0x080A,
        data[58] == 0xFCC1,
        data[59] == 0x661E,
    ]
    .into_iter()
    .filter(|&b| b)
    .count();

    count > 5
}

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
fn should_repoll_after_model_detection(device_type: DeviceType, current_slave: u8) -> bool {
    device_type.preferred_read_slave_address() != current_slave
        || !device_type.extra_poll_blocks().is_empty()
}

/// Whether to probe for external CT clamp meters on this cycle.
///
/// The discovery policy is:
/// - **First scan** (after model detection, before any probe): always runs.
/// - **If meters were found**: done — no further probing.
/// - **If no meters found AND `enable_ammeter` or EM115 is configured**:
///   retry every `METER_RETRY_INTERVAL` cycles, up to `METER_MAX_RETRIES`
///   times, because the meter may be slow to respond (e.g. LoRA-linked
///   EM115).
/// - **If no meters found AND ammeter is not configured**: one-shot scan,
///   then stop — nothing to find.
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
    let dt = match known_device_type {
        Some(dt) => dt,
        None => return false,
    };
    if dt.needs_three_phase_input_blocks() {
        return false;
    }

    // First scan — always run.
    if !meter_probe_done {
        return true;
    }

    // Ammeter is expected but no meters found yet — retry on cadence.
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

/// Cumulative energy counters captured during the post-connect grace period.
///
/// The first few reads after a TCP reconnect are the dongle's most corruption-
/// prone window. A single plausible-but-wrong value (e.g. `today_consumption_kwh
/// = 44.5` when the real reading is `~43.4`) that lands during the grace period
/// is only checked against the loose 0–200 kWh absolute range, so it sails
/// through and becomes the delta-check baseline. Every subsequent correct
/// reading is then rejected as a "decrease" until real consumption climbs back
/// above the poisoned baseline.
///
/// To prevent this, we collect the cumulative counters from each grace reading
/// and, at the end of the grace window, replace the snapshot's counters with the
/// **median** of the samples. The median is robust against a single spike (the
/// common case): for three readings `[43.4, 44.5, 43.5]` it picks `43.5`, the
/// true value, instead of trusting whichever reading happened to land first.
#[derive(Clone, Copy, Default)]
struct GraceCumulativeSamples {
    today_solar_kwh: f32,
    today_import_kwh: f32,
    today_export_kwh: f32,
    today_charge_kwh: f32,
    today_discharge_kwh: f32,
    today_consumption_kwh: f32,
    today_ac_charge_kwh: f32,
    total_import_kwh: f32,
    total_export_kwh: f32,
    total_charge_kwh: f32,
    total_discharge_kwh: f32,
}

impl GraceCumulativeSamples {
    /// Capture the cumulative counters from a sanitized snapshot.
    fn from_snapshot(s: &InverterSnapshot) -> Self {
        Self {
            today_solar_kwh: s.today_solar_kwh,
            today_import_kwh: s.today_import_kwh,
            today_export_kwh: s.today_export_kwh,
            today_charge_kwh: s.today_charge_kwh,
            today_discharge_kwh: s.today_discharge_kwh,
            today_consumption_kwh: s.today_consumption_kwh,
            today_ac_charge_kwh: s.today_ac_charge_kwh,
            total_import_kwh: s.total_import_kwh,
            total_export_kwh: s.total_export_kwh,
            total_charge_kwh: s.total_charge_kwh,
            total_discharge_kwh: s.total_discharge_kwh,
        }
    }

    /// Overwrite a snapshot's cumulative counters with the median values.
    fn apply_to(&self, s: &mut InverterSnapshot) {
        s.today_solar_kwh = self.today_solar_kwh;
        s.today_import_kwh = self.today_import_kwh;
        s.today_export_kwh = self.today_export_kwh;
        s.today_charge_kwh = self.today_charge_kwh;
        s.today_discharge_kwh = self.today_discharge_kwh;
        s.today_consumption_kwh = self.today_consumption_kwh;
        s.today_ac_charge_kwh = self.today_ac_charge_kwh;
        s.total_import_kwh = self.total_import_kwh;
        s.total_export_kwh = self.total_export_kwh;
        s.total_charge_kwh = self.total_charge_kwh;
        s.total_discharge_kwh = self.total_discharge_kwh;
    }

    /// Compute the per-field median across grace samples.
    ///
    /// For an odd sample count this is the middle element; for an even count it
    /// is the lower-middle (we only ever collect 3 samples, so the standard
    /// odd-count median applies). Requires at least one sample.
    fn median(samples: &[Self]) -> Self {
        debug_assert!(!samples.is_empty());
        // Collect each field into its own Vec, sort, and take the middle.
        macro_rules! median_field {
            ($field:ident) => {{
                let mut v: Vec<f32> = samples.iter().map(|s| s.$field).collect();
                v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                v[v.len() / 2]
            }};
        }
        Self {
            today_solar_kwh: median_field!(today_solar_kwh),
            today_import_kwh: median_field!(today_import_kwh),
            today_export_kwh: median_field!(today_export_kwh),
            today_charge_kwh: median_field!(today_charge_kwh),
            today_discharge_kwh: median_field!(today_discharge_kwh),
            today_consumption_kwh: median_field!(today_consumption_kwh),
            today_ac_charge_kwh: median_field!(today_ac_charge_kwh),
            total_import_kwh: median_field!(total_import_kwh),
            total_export_kwh: median_field!(total_export_kwh),
            total_charge_kwh: median_field!(total_charge_kwh),
            total_discharge_kwh: median_field!(total_discharge_kwh),
        }
    }
}

fn carry_forward_optional_block_values(
    snap: &mut InverterSnapshot,
    prev: Option<&InverterSnapshot>,
    has_ac_config_block: bool,
    has_extended_slots_block: bool,
    has_three_phase_config_block: bool,
) -> bool {
    let Some(prev) = prev else { return false };
    let mut changed = false;

    // AC-coupled config is read from optional HR(300-359). If that optional
    // block is skipped for one poll, keep the previous AC config values rather
    // than flashing defaults/zeros in the UI.
    if !has_ac_config_block
        && matches!(
            snap.device_type,
            DeviceType::ACCoupled | DeviceType::ACCoupledMk2 | DeviceType::ACThreePhase
        )
        && snap.device_type == prev.device_type
    {
        if snap.charge_rate != prev.charge_rate || snap.discharge_rate != prev.discharge_rate {
            tracing::warn!(
                charge_prev = prev.charge_rate,
                discharge_prev = prev.discharge_rate,
                "AC config block missing — carrying forward AC charge/discharge limits"
            );
            snap.charge_rate = prev.charge_rate;
            snap.discharge_rate = prev.discharge_rate;
            changed = true;
        }
        if snap.ac_export_priority != prev.ac_export_priority {
            snap.ac_export_priority = prev.ac_export_priority;
            changed = true;
        }
        if snap.ac_eps_enabled != prev.ac_eps_enabled {
            snap.ac_eps_enabled = prev.ac_eps_enabled;
            changed = true;
        }
        if snap.battery_pause_mode != prev.battery_pause_mode {
            snap.battery_pause_mode = prev.battery_pause_mode;
            changed = true;
        }
        if snap.battery_pause_slot.enabled != prev.battery_pause_slot.enabled
            || snap.battery_pause_slot.start_hour != prev.battery_pause_slot.start_hour
            || snap.battery_pause_slot.start_minute != prev.battery_pause_slot.start_minute
            || snap.battery_pause_slot.end_hour != prev.battery_pause_slot.end_hour
            || snap.battery_pause_slot.end_minute != prev.battery_pause_slot.end_minute
        {
            snap.battery_pause_slot = prev.battery_pause_slot.clone();
            changed = true;
        }
    }

    // Three-phase/commercial/HV models get limit/reserve values from optional
    // HR(1080-1124). If that optional block is skipped for one poll, keep the
    // previous values rather than flashing defaults/zeros in the UI.
    if !has_three_phase_config_block
        && matches!(
            snap.device_type,
            DeviceType::ThreePhase
                | DeviceType::ACThreePhase
                | DeviceType::AioCommercial
                | DeviceType::HybridHvGen3
                | DeviceType::AllInOneHybrid
        )
        && snap.device_type == prev.device_type
        && (snap.charge_rate != prev.charge_rate
            || snap.discharge_rate != prev.discharge_rate
            || snap.battery_reserve != prev.battery_reserve
            || snap.target_soc != prev.target_soc)
    {
        tracing::warn!(
            charge_prev = prev.charge_rate,
            discharge_prev = prev.discharge_rate,
            reserve_prev = prev.battery_reserve,
            target_prev = prev.target_soc,
            "Three-phase config block missing — carrying forward previous limits/reserve"
        );
        snap.charge_rate = prev.charge_rate;
        snap.discharge_rate = prev.discharge_rate;
        snap.battery_reserve = prev.battery_reserve;
        snap.target_soc = prev.target_soc;
        changed = true;
    }

    // Gen3/AIO/HV/three-phase extended schedules are read from optional HR(240-299).
    // If the block is missed, preserve values that only exist there: per-slot
    // targets for slots 1/2 and extended slots 3-10. Slot times for slots 1/2
    // are decoded from either the standard single-phase map or the three-phase
    // HR(1113-1121) config block, so avoid replacing them here.
    if !has_extended_slots_block
        && snap.device_type.uses_extended_schedule_slots()
        && snap.device_type == prev.device_type
    {
        for idx in 0..2 {
            if snap.charge_slots[idx].target_soc == 4 && prev.charge_slots[idx].target_soc > 4 {
                snap.charge_slots[idx].target_soc = prev.charge_slots[idx].target_soc;
                changed = true;
            }
            if snap.discharge_slots[idx].target_soc == 4 && prev.discharge_slots[idx].target_soc > 4
            {
                snap.discharge_slots[idx].target_soc = prev.discharge_slots[idx].target_soc;
                changed = true;
            }
        }
        for idx in 2..snap.charge_slots.len() {
            if !snap.charge_slots[idx].enabled && prev.charge_slots[idx].enabled {
                snap.charge_slots[idx] = prev.charge_slots[idx].clone();
                changed = true;
            }
            if !snap.discharge_slots[idx].enabled && prev.discharge_slots[idx].enabled {
                snap.discharge_slots[idx] = prev.discharge_slots[idx].clone();
                changed = true;
            }
        }
        if changed {
            tracing::warn!(
                "Extended schedule block missing — carrying forward previous extended slot data"
            );
        }
    }

    changed
}

fn carry_forward_battery_modules_with(
    snap: &mut InverterSnapshot,
    prev_modules: Option<&[super::model::BatteryModule]>,
) {
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
            let max_index = snap
                .battery_modules
                .iter()
                .map(|m| m.index)
                .max()
                .unwrap_or(0);
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

/// Derive battery temperature, capacity and max power for three-phase / HV /
/// commercial inverters from the BMS data.
///
/// The three-phase inverter register blocks (IR 1000-1413, HR 1080-1124) do
/// NOT expose battery pack temperature or capacity — only converter heatsink
/// temperatures (`t_inverter`/`t_boost`/`t_buck_boost`) and SOC/power/current.
/// Single-phase gets these from IR(56) and HR(55), but those registers are not
/// populated on three-phase hardware, so `decode_input_0_59` /
/// `decode_holding_0_59` leave either garbage (IR 56) or zero (HR 55) behind.
///
/// The authoritative source for battery temperature is always the BMS
/// per-module cell temperature probes, averaged across all modules. This
/// is more reliable than the BCU cluster's IR(68) register (which can
/// return stale or garbage values on some battery firmware versions, e.g.
/// DA0.011 — see #48) and far more reliable than the inverter's IR(56).
/// Capacity, voltage, current, and SOC for three-phase/HV still come from
/// the BCU cluster when available, as the BMU blocks don't expose those.
///
/// Override battery temperature from BMS module data for ALL device types.
///
/// The inverter register block IR(56) frequently carries stale or garbage
/// data even on single-phase inverters (#48). The BMS module temperatures
/// are the authoritative source — their per-module cell-group maxima are
/// always more accurate than the inverter's single register.
///
/// For three-phase / HV / commercial inverters, also derives battery
/// capacity and max power from the BMS data (since their inverter register
/// blocks lack this information entirely). Single-phase gets those from
/// the standard HR(55)/IR decode paths.
fn derive_battery_fields_from_bms(
    snap: &mut InverterSnapshot,
    hv_cluster: Option<&crate::inverter::decoder::HvBcuCluster>,
) {
    let is_three_phase = snap.device_type.needs_three_phase_input_blocks();

    // --- Temperature: always from BMS module average when available ---
    // The BCU cluster IR(68) and inverter IR(56) are both unreliable
    // (stale/garbage on some firmware versions, e.g. DA0.011 — #48).
    // The per-module cell temperature probes are the authoritative source.
    if !snap.battery_modules.is_empty() {
        let count = snap.battery_modules.len() as f32;
        let temp_sum: f32 = snap.battery_modules.iter().map(|m| m.temperature).sum();
        snap.battery_temperature = temp_sum / count;
    } else if is_three_phase {
        // No BMS module data at all on a three-phase inverter: clear
        // the garbage that IR(56) leaves behind.
        snap.battery_temperature = f32::NAN;
    }
    // Single-phase with no BMS modules: keep IR(56) as fallback.

    // --- HV cluster: capacity, voltage, current, SOC (NOT temperature) ---
    if let Some(cluster) = hv_cluster {
        if is_three_phase {
            let nominal_v = snap.device_type.nominal_battery_voltage();
            snap.battery_capacity_kwh = cluster.total_capacity_ah() * nominal_v / 1000.0;
            if cluster.battery_voltage > 0.0 {
                snap.battery_voltage = cluster.battery_voltage;
                snap.battery_current = cluster.battery_current;
            }
            if snap.soc == 0 && cluster.battery_soc_max > 0 {
                snap.soc = cluster.battery_soc_max.min(100);
            }
        }
    } else if snap.battery_modules.is_empty() {
        // --- No BMS data at all ---
        if is_three_phase {
            snap.battery_capacity_kwh = 0.0;
            snap.max_battery_power_w = snap.device_type.max_battery_power_w();
        }
        return;
    } else if is_three_phase {
        // --- LV BMS module data only (no HV cluster) ---
        let total_cap_ah: f32 = snap.battery_modules.iter().map(|m| m.capacity_ah).sum();
        let nominal_v = snap.device_type.nominal_battery_voltage();
        snap.battery_capacity_kwh = total_cap_ah * nominal_v / 1000.0;
    }

    // Max battery power for three-phase: device hardware limit capped at
    // half the capacity.
    if is_three_phase {
        let cap_w = snap.battery_capacity_kwh * 1000.0;
        snap.max_battery_power_w = if cap_w > 0.0 {
            snap.device_type
                .max_battery_power_w()
                .min((cap_w / 2.0) as u32)
        } else {
            snap.device_type.max_battery_power_w()
        };
    }
}

/// Persist the in-memory `cosy_active` flag to settings so a crash/restart
/// can detect a missed CosyExit (the inverter was left force-charging after
/// the slot ended but before the app came back up). On startup,
/// [`AppState::new`] seeds the in-memory flag from this persisted value, and
/// the normal cosy state machine fires CosyExit on the next poll if the
/// current time is outside any Cosy slot.
fn persist_cosy_active(active: bool) {
    // In tests, run synchronously (no Tokio runtime).
    // In production, offload file I/O to the blocking thread pool.
    #[cfg(not(test))]
    {
        tokio::task::spawn_blocking(move || persist_cosy_active_sync(active));
    }
    #[cfg(test)]
    persist_cosy_active_sync(active);
}

fn persist_cosy_active_sync(active: bool) {
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
fn persist_agile_state(ag_state: AgileState) {
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

fn persist_agile_state_sync(label: String) {
    let mut settings = crate::settings::Settings::load();
    if settings.agile_state_persisted != label {
        settings.agile_state_persisted = label.clone();
        if let Err(e) = settings.save() {
            tracing::warn!(state = &label, "Failed to persist agile_state: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Cosy slot register writes
// ---------------------------------------------------------------------------

/// Generate register writes to program a Cosy slot into the inverter's charge
/// slot 1 registers and optionally enable charging.
///
/// When `active` is true (slot is currently running), writes the slot times,
/// enables charging, and sets the target SOC. When `active` is false (preloading
/// the next slot), writes only the slot times so the inverter has them ready
/// for when the slot starts — but does NOT enable charging.
///
/// For three-phase models, uses the three-phase charge slot 1 registers.
/// For Gen3+ models, also writes the per-slot target SOC in the HR 240-299 block.
fn cosy_slot_register_writes(
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
fn clear_cosy_slot_registers(device_type: DeviceType) -> Vec<RegisterWrite> {
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
async fn write_registers_to_inverter(
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

/// Returns `true` if any field was sanitized (fallback applied).
///
/// `pending_mode` tracks a mode that differs from the previous reading but
/// hasn't yet been confirmed by a second consecutive reading. This prevents
/// mode flicker caused by a single corrupt register read.
/// Tracks how many consecutive poll cycles each cumulative field has been
/// corrected downward (raw < prev). If the inverter consistently reports the
/// same lower value for many cycles, the *baseline* was likely the corrupted
/// one (e.g. a grace-period spike). After `DELTA_CORRECTION_RELEASE_THRESHOLD`
/// consecutive corrections, we accept the raw value and reset the counter.
#[derive(Default)]
struct DeltaCorrectionCounts(HashMap<&'static str, u8>);

/// Number of consecutive downward corrections before we accept that the
/// raw value is correct and the baseline was wrong.
const DELTA_CORRECTION_RELEASE_THRESHOLD: u8 = 10;

/// After this many consecutive corrections for the same field, downgrade the
/// WARN to DEBUG to avoid log spam when the dongle is stuck on a corrupted
/// value. A final INFO is logged on release (when the threshold is reached).
const RATE_LIMIT_AFTER: u8 = 3;

/// Tracks how many consecutive poll cycles each absolute-range-checked field
/// has been out of range (exceeding its threshold). If a field persistently
/// reports the same out-of-range value for `SUSPECT_RELEASE_THRESHOLD` cycles,
/// we accept it as legitimate — the threshold was too conservative for this
/// installation (e.g. home power >15 kW on a three-phase inverter).
#[derive(Default)]
struct ConsecutiveSuspectCounts(HashMap<&'static str, u8>);

/// Number of consecutive out-of-range readings before we release the clamp
/// and accept the raw value as legitimate.
const SUSPECT_RELEASE_THRESHOLD: u8 = 10;

/// Apply the 10-readings suspect-count method to a signed power field.
/// Returns `true` if the value was sanitized (replaced with previous).
fn check_power_field(
    raw_value: i32,
    prev_value: Option<i32>,
    limit: i32,
    label: &'static str,
    suspect_counts: &mut ConsecutiveSuspectCounts,
) -> (i32, bool) {
    if raw_value.abs() <= limit {
        suspect_counts.0.remove(label);
        return (raw_value, false);
    }

    // If the previous value was also out-of-range and matches raw,
    // we already accepted this in a prior cycle.
    let already_accepted = prev_value.is_some_and(|pv| raw_value == pv && pv.abs() > limit);

    if already_accepted {
        suspect_counts.0.remove(label);
        return (raw_value, false);
    }

    let count = suspect_counts.0.entry(label).or_insert(0);
    *count += 1;
    if *count >= SUSPECT_RELEASE_THRESHOLD {
        tracing::info!(
            raw = raw_value,
            count = *count,
            "{label} persistently out of range — accepting as legitimate"
        );
        suspect_counts.0.remove(label);
        (raw_value, false)
    } else if let Some(pv) = prev_value {
        tracing::warn!(
            raw = raw_value,
            prev = pv,
            count = *count,
            "{label} out of range — using previous"
        );
        (pv, true)
    } else {
        tracing::debug!(
            raw = raw_value,
            count = *count,
            "{label} out of range — no previous, accepting raw"
        );
        (raw_value, false)
    }
}

fn sanitize_snapshot(
    snap: &mut InverterSnapshot,
    prev: Option<&InverterSnapshot>,
    skip_delta: bool,
    pending_mode: &mut Option<BatteryMode>,
    delta_corrections: &mut DeltaCorrectionCounts,
    suspect_counts: &mut ConsecutiveSuspectCounts,
) -> bool {
    let mut sanitized = false;
    let max_battery_power: i32 = 10_000; // 10 kW — residential battery limit
    let max_grid_power: i32 = 15_000; // 15 kW — UK single-phase import can exceed 10 kW with EV charging (100A fuse ≈ 23 kW); matches max_home_power which carries the same EV-charging margin. Corruption spikes (e.g. ±32767) are still well above this.
    let max_solar_power: i32 = 10_000; // 10 kW — residential PV limit
    let max_home_power: i32 = 15_000; // 15 kW — includes EV charging margin

    // Power fields: apply the 10-readings suspect-count method.
    // On first out-of-range encounter, fall back to previous (safe).
    // If the value persists for SUSPECT_RELEASE_THRESHOLD cycles,
    // accept it as legitimate (conservative threshold was wrong for
    // this installation — e.g. 100 A supply, three-phase, commercial).
    let prev_battery = prev.map(|p| p.battery_power);
    let (val, was_sanitized) = check_power_field(
        snap.battery_power,
        prev_battery,
        max_battery_power,
        "battery_power",
        suspect_counts,
    );
    snap.battery_power = val;
    sanitized |= was_sanitized;

    let prev_grid = prev.map(|p| p.grid_power);
    let (val, was_sanitized) = check_power_field(
        snap.grid_power,
        prev_grid,
        max_grid_power,
        "grid_power",
        suspect_counts,
    );
    snap.grid_power = val;
    sanitized |= was_sanitized;

    let prev_solar = prev.map(|p| p.solar_power);
    let (val, was_sanitized) = check_power_field(
        snap.solar_power,
        prev_solar,
        max_solar_power,
        "solar_power",
        suspect_counts,
    );
    snap.solar_power = val;
    sanitized |= was_sanitized;

    let prev_home = prev.map(|p| p.home_power);
    let (val, was_sanitized) = check_power_field(
        snap.home_power,
        prev_home,
        max_home_power,
        "home_power",
        suspect_counts,
    );
    snap.home_power = val;
    sanitized |= was_sanitized;

    // SOC: if 0 but power is flowing, clearly a garbled register
    if snap.soc == 0 && (snap.solar_power > 0 || snap.battery_power != 0 || snap.grid_power != 0) {
        if let Some(p) = prev {
            tracing::warn!(
                prev_soc = p.soc,
                "SOC=0 with live power — using previous SOC"
            );
            snap.soc = p.soc;
            sanitized = true;
        }
    }

    // SOC: if 100 but battery is actively charging at high power, impossible
    if snap.soc == 100 && snap.battery_power > 2000 {
        if let Some(p) = prev {
            tracing::warn!(
                prev_soc = p.soc,
                "SOC=100 while charging >2000W — using previous SOC"
            );
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

    if !snap.grid_online {
        // The inverter fault/status word is authoritative for grid loss. Do not
        // let the normal range checks carry forward the last healthy voltage /
        // frequency and accidentally hide the outage state in the UI.
        snap.grid_voltage = snap.grid_voltage.max(0.0);
        snap.grid_frequency = snap.grid_frequency.max(0.0);
    } else {
        // Grid voltage:
        //   Single-phase: UK nominal 230V ±10% (207–253V), anything outside 180–280V
        //     is clearly corrupt register data.
        //   Three-phase (line-to-line): UK nominal 415V ±10% (373–456V), match the
        //     reference library's v_ac1 bounds of 0–500V (IR(1061)).
        let (v_min, v_max) = if snap.device_type.needs_three_phase_input_blocks() {
            (0.0, 500.0)
        } else {
            (180.0, 280.0)
        };
        if snap.grid_voltage < v_min || snap.grid_voltage > v_max {
            if let Some(p) = prev {
                tracing::warn!(
                    raw = snap.grid_voltage,
                    prev = p.grid_voltage,
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
                    raw = snap.grid_frequency,
                    prev = p.grid_frequency,
                    "Grid frequency out of range — using previous"
                );
                snap.grid_frequency = p.grid_frequency;
            } else {
                snap.grid_frequency = 50.0; // nominal
            }
            sanitized = true;
        }
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

        check_energy_field!(
            "today_solar_kwh",
            snap.today_solar_kwh,
            prev.map(|p| p.today_solar_kwh)
        );
        check_energy_field!(
            "today_import_kwh",
            snap.today_import_kwh,
            prev.map(|p| p.today_import_kwh)
        );
        check_energy_field!(
            "today_export_kwh",
            snap.today_export_kwh,
            prev.map(|p| p.today_export_kwh)
        );
        check_energy_field!(
            "today_charge_kwh",
            snap.today_charge_kwh,
            prev.map(|p| p.today_charge_kwh)
        );
        check_energy_field!(
            "today_discharge_kwh",
            snap.today_discharge_kwh,
            prev.map(|p| p.today_discharge_kwh)
        );
        check_energy_field!(
            "today_consumption_kwh",
            snap.today_consumption_kwh,
            prev.map(|p| p.today_consumption_kwh)
        );
        check_energy_field!(
            "today_ac_charge_kwh",
            snap.today_ac_charge_kwh,
            prev.map(|p| p.today_ac_charge_kwh)
        );

        // Lifetime total energy (total_import_kwh / total_export_kwh):
        // These are cumulative counters that monotonically increase over the
        // lifetime of the inverter. They can reach tens of thousands of kWh
        // for a multi-year installation. The same absolute range check
        // pattern applies, but with a much higher ceiling (100,000 kWh).
        // Lifetime totals are uint32 with 0.1 scaling, so the native max is
        // ~430,000 kWh; we cap at 100,000 as a generous residential bound.
        // Delta checks are even more important here since a single corrupted
        // uint32 can produce values like 4 billion.
        let max_lifetime_kwh: f32 = 100_000.0;

        macro_rules! check_total_energy_field {
            ($name:literal, $value:expr, $prev_val:expr) => {
                let raw = $value;
                if raw < 0.0 || raw > max_lifetime_kwh {
                    let prev_v: Option<f32> = $prev_val;
                    if let Some(pv) = prev_v {
                        tracing::warn!(
                            field = $name, raw, max = max_lifetime_kwh, prev = pv,
                            "Lifetime total energy out of plausible range — using previous",
                        );
                        $value = pv;
                    } else {
                        tracing::warn!(
                            field = $name, raw, max = max_lifetime_kwh,
                            "Lifetime total energy out of plausible range — no previous, clamping to 0",
                        );
                        $value = 0.0;
                    }
                    sanitized = true;
                }
            };
        }

        check_total_energy_field!(
            "total_import_kwh",
            snap.total_import_kwh,
            prev.map(|p| p.total_import_kwh)
        );
        check_total_energy_field!(
            "total_export_kwh",
            snap.total_export_kwh,
            prev.map(|p| p.total_export_kwh)
        );
        check_total_energy_field!(
            "total_charge_kwh",
            snap.total_charge_kwh,
            prev.map(|p| p.total_charge_kwh)
        );
        check_total_energy_field!(
            "total_discharge_kwh",
            snap.total_discharge_kwh,
            prev.map(|p| p.total_discharge_kwh)
        );
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
            // Daily energy registers are typically 0.1 kWh resolution, and
            // derived counters can wobble by one tick due to read timing or
            // float representation (e.g. 7.6 → 7.5). Treat tiny decreases as
            // reading noise: keep the displayed/history value monotonic, but
            // don't warn or trigger an immediate re-poll.
            let decrease_noise_tolerance_kwh = 0.15;

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
                    delta_corrections.0.remove($name);
                }
                // Tiny one-tick decreases are normal read noise; carry the
                // previous value forward silently so cumulative values remain
                // monotonic without spamming warnings or forcing a re-poll.
                else if raw < prev_val && raw + decrease_noise_tolerance_kwh >= prev_val {
                    tracing::debug!(
                        field = $name, raw, prev = prev_val,
                        tolerance_kwh = decrease_noise_tolerance_kwh,
                        "Daily energy decreased within noise tolerance — carrying forward previous",
                    );
                    $value = prev_val;
                }
                // Counter must not decrease materially (register corruption).
                // However, if the inverter consistently reports the same lower
                // value for many cycles, the baseline was likely wrong — release.
                else if raw < prev_val {
                    let count = delta_corrections.0.entry($name).or_insert(0);
                    *count += 1;
                    if *count >= DELTA_CORRECTION_RELEASE_THRESHOLD {
                        tracing::info!(
                            field = $name, raw, prev = prev_val,
                            count = *count,
                            "Daily energy consistently lower — accepting raw, baseline was likely wrong",
                        );
                        $value = raw;
                        delta_corrections.0.remove($name);
                        // Don't set sanitized=true — we're accepting raw, not rejecting it
                    } else if *count >= RATE_LIMIT_AFTER {
                        tracing::debug!(
                            field = $name, raw, prev = prev_val,
                            count = *count,
                            "Daily energy decreased (register corruption, repeated) — using previous",
                        );
                        $value = prev_val;
                        sanitized = true;
                    } else {
                        tracing::warn!(
                            field = $name, raw, prev = prev_val,
                            "Daily energy decreased (register corruption) — using previous",
                        );
                        $value = prev_val;
                        sanitized = true;
                    }
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
                else {
                    // Normal increase within rate limit — raw accepted, reset counter
                    delta_corrections.0.remove($name);
                }
            };
        }

            check_energy_delta!("today_solar_kwh", snap.today_solar_kwh, p.today_solar_kwh);
            check_energy_delta!(
                "today_import_kwh",
                snap.today_import_kwh,
                p.today_import_kwh
            );
            check_energy_delta!(
                "today_export_kwh",
                snap.today_export_kwh,
                p.today_export_kwh
            );
            check_energy_delta!(
                "today_charge_kwh",
                snap.today_charge_kwh,
                p.today_charge_kwh
            );
            check_energy_delta!(
                "today_discharge_kwh",
                snap.today_discharge_kwh,
                p.today_discharge_kwh
            );
            check_energy_delta!(
                "today_consumption_kwh",
                snap.today_consumption_kwh,
                p.today_consumption_kwh
            );
            check_energy_delta!(
                "today_ac_charge_kwh",
                snap.today_ac_charge_kwh,
                p.today_ac_charge_kwh
            );

            // Lifetime total energy delta check:
            // Lifetime counters are STRICTLY monotonically increasing — they
            // NEVER reset (unlike daily counters which reset at midnight).
            // Any decrease is register corruption. The same elapsed-time
            // rate limit applies, with a slightly more generous headroom
            // (15 kW peak circuit capacity instead of 10 kW).
            let max_lifetime_rate_kw: f32 = 15.0;
            let max_lifetime_increase_kwh = (elapsed_secs / 3600.0) * max_lifetime_rate_kw + 1.0;

            macro_rules! check_total_energy_delta {
            ($name:literal, $value:expr, $prev:expr) => {
                let raw = $value;
                let prev_val = $prev;

                if prev_val < 1.0 {
                    // prev is unreliable (near-zero from initial clamp)
                    if raw > max_lifetime_increase_kwh {
                        tracing::warn!(
                            field = $name, raw, prev = prev_val,
                            elapsed_secs, max = max_lifetime_increase_kwh,
                            "Lifetime total jumped from near-zero baseline — clamping",
                        );
                        $value = prev_val + max_lifetime_increase_kwh;
                        sanitized = true;
                    }
                }
                // Lifetime counters NEVER reset — any decrease is corruption.
                // (No midnight rollover check needed.)
                // Tiny one-tick decreases are normal read noise; carry
                // previous value forward silently.
                else if raw < prev_val && raw + decrease_noise_tolerance_kwh >= prev_val {
                    tracing::debug!(
                        field = $name, raw, prev = prev_val,
                        tolerance_kwh = decrease_noise_tolerance_kwh,
                        "Lifetime total decreased within noise tolerance — carrying forward",
                    );
                    $value = prev_val;
                }
                // Counter must not decrease materially.
                // Same consecutive-correction release as daily counters.
                else if raw < prev_val {
                    let count = delta_corrections.0.entry($name).or_insert(0);
                    *count += 1;
                    if *count >= DELTA_CORRECTION_RELEASE_THRESHOLD {
                        tracing::info!(
                            field = $name, raw, prev = prev_val,
                            count = *count,
                            "Lifetime total consistently lower — accepting raw, baseline was likely wrong",
                        );
                        $value = raw;
                        delta_corrections.0.remove($name);
                    } else if *count >= RATE_LIMIT_AFTER {
                        tracing::debug!(
                            field = $name, raw, prev = prev_val,
                            count = *count,
                            "Lifetime total decreased (register corruption, repeated) — using previous",
                        );
                        $value = prev_val;
                        sanitized = true;
                    } else {
                        tracing::warn!(
                            field = $name, raw, prev = prev_val,
                            "Lifetime total decreased (register corruption) — using previous",
                        );
                        $value = prev_val;
                        sanitized = true;
                    }
                }
                // Increase must be plausible for elapsed time
                else if raw > prev_val + max_lifetime_increase_kwh {
                    tracing::warn!(
                        field = $name, raw, prev = prev_val,
                        elapsed_secs, max = max_lifetime_increase_kwh,
                        "Lifetime total jumped too fast — using previous",
                    );
                    $value = prev_val;
                    sanitized = true;
                }
                else {
                    // Normal increase within rate limit — raw accepted, reset counter
                    delta_corrections.0.remove($name);
                }
            };
        }

            check_total_energy_delta!(
                "total_import_kwh",
                snap.total_import_kwh,
                p.total_import_kwh
            );
            check_total_energy_delta!(
                "total_export_kwh",
                snap.total_export_kwh,
                p.total_export_kwh
            );
            check_total_energy_delta!(
                "total_charge_kwh",
                snap.total_charge_kwh,
                p.total_charge_kwh
            );
            check_total_energy_delta!(
                "total_discharge_kwh",
                snap.total_discharge_kwh,
                p.total_discharge_kwh
            );
        }
    } // skip_delta

    // Slot time corruption check: the dongle sometimes returns garbage values
    // for specific holding register pairs, especially HR(31-32) (charge slot 2).
    // A slot showing a sub-10-minute window (e.g. 00:01-00:04, raw values 1 and 4)
    // when the previous poll had a legitimate multi-hour window strongly indicates
    // register corruption at those addresses. Carry forward the previous times.
    #[allow(clippy::collapsible_if)]
    if let Some(p) = prev {
        for i in 0..snap.charge_slots.len() {
            let slot = &snap.charge_slots[i];
            let prev_slot = &p.charge_slots[i];
            if slot.enabled
                && slot.start_hour as u16 * 60 + slot.start_minute as u16 <= 10
                && prev_slot.enabled
                && prev_slot.start_hour as u16 * 60 + prev_slot.start_minute as u16 > 10
            {
                tracing::warn!(
                    slot = i,
                    raw_start = format!("{:02}:{:02}", slot.start_hour, slot.start_minute),
                    raw_end = format!("{:02}:{:02}", slot.end_hour, slot.end_minute),
                    prev_start =
                        format!("{:02}:{:02}", prev_slot.start_hour, prev_slot.start_minute),
                    prev_end = format!("{:02}:{:02}", prev_slot.end_hour, prev_slot.end_minute),
                    "Charge slot times suspiciously small — carrying forward previous"
                );
                snap.charge_slots[i] = prev_slot.clone();
                sanitized = true;
            }
        }
        for i in 0..snap.discharge_slots.len() {
            let slot = &snap.discharge_slots[i];
            let prev_slot = &p.discharge_slots[i];
            if slot.enabled
                && slot.start_hour as u16 * 60 + slot.start_minute as u16 <= 10
                && prev_slot.enabled
                && prev_slot.start_hour as u16 * 60 + prev_slot.start_minute as u16 > 10
            {
                tracing::warn!(
                    slot = i,
                    raw_start = format!("{:02}:{:02}", slot.start_hour, slot.start_minute),
                    raw_end = format!("{:02}:{:02}", slot.end_hour, slot.end_minute),
                    prev_start =
                        format!("{:02}:{:02}", prev_slot.start_hour, prev_slot.start_minute),
                    prev_end = format!("{:02}:{:02}", prev_slot.end_hour, prev_slot.end_minute),
                    "Discharge slot times suspiciously small — carrying forward previous"
                );
                snap.discharge_slots[i] = prev_slot.clone();
                sanitized = true;
            }
        }
    }

    // AC-coupled inverters get charge/discharge limits from optional AC config
    // registers HR(313/314). If that optional block is skipped or times out for
    // one poll, the values can decode as 0. Since AC bounds are 1-100, carry
    // forward the last known-good values rather than showing a transient 0%.
    if matches!(
        snap.device_type,
        DeviceType::ACCoupled | DeviceType::ACCoupledMk2
    ) {
        if let Some(p) = prev {
            if matches!(
                p.device_type,
                DeviceType::ACCoupled | DeviceType::ACCoupledMk2
            ) {
                if snap.charge_rate == 0 && p.charge_rate > 0 {
                    tracing::warn!(
                        prev = p.charge_rate,
                        "AC charge limit missing/zero — carrying forward previous value"
                    );
                    snap.charge_rate = p.charge_rate;
                    sanitized = true;
                }
                if snap.discharge_rate == 0 && p.discharge_rate > 0 {
                    tracing::warn!(
                        prev = p.discharge_rate,
                        "AC discharge limit missing/zero — carrying forward previous value"
                    );
                    snap.discharge_rate = p.discharge_rate;
                    sanitized = true;
                }
            }
        }
    }

    // Clamp battery limits to valid ranges (registers can return corrupted values)
    snap.charge_rate = snap.charge_rate.min(100);
    snap.discharge_rate = snap.discharge_rate.min(100);
    snap.active_power_rate = snap.active_power_rate.min(100);

    // Battery voltage: reject spurious readings. Nominal is 51.2V (LV) or 307V (HV).
    // Anything > 60V on an LV system or > 400V on an HV system is a corrupt register.
    let max_battery_voltage = match snap.device_type {
        crate::inverter::model::DeviceType::AllInOne6kW
        | crate::inverter::model::DeviceType::AllInOne3_6kW
        | crate::inverter::model::DeviceType::AllInOne5kW
        | crate::inverter::model::DeviceType::AioCommercial
        | crate::inverter::model::DeviceType::HybridHvGen3
        | crate::inverter::model::DeviceType::AllInOneHybrid => 400.0,
        crate::inverter::model::DeviceType::ThreePhase
        | crate::inverter::model::DeviceType::ACThreePhase => 600.0,
        _ => 60.0,
    };
    if snap.battery_voltage > max_battery_voltage || snap.battery_voltage < 0.0 {
        if let Some(p) = prev {
            tracing::warn!(
                raw = snap.battery_voltage,
                prev = p.battery_voltage,
                "Battery voltage out of range — using previous"
            );
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

        // Slot times are user-configured holding registers — they only change
        // when the user explicitly writes them. A delta check would incorrectly
        // reject legitimate overnight transitions (e.g. start jumping from 00:00
        // to 23:00). The existing decode_timeslot already returns disabled when
        // start==end, guarding against partial-write reads. We only sanity-check
        // the enabled/times consistency (enabled toggled without times).
        // Slot times only change via explicit user writes — the encode path
        // validates HHMM values and the decode_timeslot guard (disabled when
        // start==end) handles partial-write reads. No revert needed.
        for i in 0..snap.charge_slots.len().min(p.charge_slots.len()) {
            if snap.charge_slots[i].enabled != p.charge_slots[i].enabled
                && (snap.charge_slots[i].start_hour != p.charge_slots[i].start_hour
                    || snap.charge_slots[i].end_hour != p.charge_slots[i].end_hour)
            {
                tracing::debug!(
                    slot = i,
                    cur_enabled = snap.charge_slots[i].enabled,
                    prev_enabled = p.charge_slots[i].enabled,
                    "Charge slot {i} enabled + times changed (expected after write)",
                );
            }
        }

        // Discharge slots — same reasoning as charge slots above.
        for i in 0..snap.discharge_slots.len().min(p.discharge_slots.len()) {
            if snap.discharge_slots[i].enabled != p.discharge_slots[i].enabled
                && (snap.discharge_slots[i].start_hour != p.discharge_slots[i].start_hour
                    || snap.discharge_slots[i].end_hour != p.discharge_slots[i].end_hour)
            {
                tracing::debug!(
                    slot = i,
                    cur_enabled = snap.discharge_slots[i].enabled,
                    prev_enabled = p.discharge_slots[i].enabled,
                    "Discharge slot {i} enabled + times changed (expected after write)",
                );
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

        // Battery reserve (HR 110): must be 4-100.
        if !(4..=100).contains(&snap.battery_reserve) {
            tracing::warn!(
                raw = snap.battery_reserve,
                prev = p.battery_reserve,
                "Battery reserve out of range — using previous"
            );
            snap.battery_reserve = p.battery_reserve.clamp(4, 100);
            sanitized = true;
        }
    } else if !(4..=100).contains(&snap.battery_reserve) {
        tracing::warn!(
            raw = snap.battery_reserve,
            "Battery reserve out of range — clamping to valid range"
        );
        snap.battery_reserve = snap.battery_reserve.clamp(4, 100);
        sanitized = true;
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
fn check_load_limiter(
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
    // EcoPaused is what the limiter sets when it pauses discharge — it
    // must be accepted so the recovery countdown can proceed.
    // No other automated modes should be active.
    if snap.battery_mode != BatteryMode::Eco && snap.battery_mode != BatteryMode::EcoPaused {
        // If we're Paused but the battery mode isn't one we manage,
        // someone changed it externally — return to Idle without writing.
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
        // Outside window — if we're Paused, restore Eco.
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
                    "Load limiter: home load above threshold — counting"
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
                    "Load limiter: load below threshold — counting"
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
                    "Load limiter: post-crash — load below threshold, restoring Eco immediately"
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
                    "Load limiter: post-crash — load still high, staying Paused"
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
                        "Load limiter: restoring Eco mode — load below threshold for full delay"
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
                        "Load limiter: counting down — {}/{} polls remaining",
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
                            tracing::warn!("Warmup read {}/3 — FAILED: {e}", i + 1,);
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
                // Track persistent failures ACROSS intervals. consecutive_failures
                // resets within each interval, so a dead socket loops: 3 failures
                // → sleep → 3 failures → sleep forever. Now we break out to
                // reconnect immediately on the 3rd consecutive failure.
                // Track consecutive dongle memory-leak fingerprint hits.
                // After MAX_SUSPICIOUS_CYCLES, break to reconnect — persistent
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
                                "Cosy: restart detected inside slot — force-charge will be re-sent on next poll"
                            );
                            // Reset the in-memory flag so the entry logic
                            // re-fires and re-sends the force-charge writes.
                            *state.cosy_active.lock().await = false;
                        } else {
                            tracing::info!(
                                "Cosy: restart detected AFTER slot ended — CosyExit will be sent on next poll to restore Eco mode"
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
                            "Agile: restart detected with active persisted state — will re-evaluate current price and re-send command on first poll"
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
                            "Settings changed (v{} → v{}) — reconnecting",
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

                    // The consumer task handles stale frames — unmatched
                    // responses (including duplicate write ACKs) are silently
                    // dropped during the read cycle. No explicit flush needed.

                    let (poll_ok, sanitized, connection_lost) = async {
                        match client.read_all_with_extras(known_device_type.as_ref()).await {
                            Ok(blocks) => {
                                let mut snapshot = decode_snapshot(&blocks);

                                // Check all 60-register blocks against the known dongle
                                // memory-leak corruption fingerprint. If the dongle serves
                                // its own TCP/IP memory instead of register values, the
                                // entire poll cycle is suspect — trigger a re-poll.
                                let block_suspicious = blocks
                                    .iter()
                                    .any(|b| b.block.start % 60 == 0 && b.block.count == 60 && is_block_suspicious(&b.data));
                                if block_suspicious {
                                    for br in &blocks {
                                        if br.block.start % 60 == 0 && br.block.count == 60 && is_block_suspicious(&br.data) {
                                            tracing::warn!(
                                                block = br.block.name,
                                                start = br.block.start,
                                                "Block matched dongle memory-leak fingerprint — re-polling",
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
                                            "Persistent fingerprint corruption — reconnecting"
                                        );
                                    } else {
                                        tracing::warn!(
                                            suspicious = consecutive_suspicious,
                                            max = MAX_SUSPICIOUS_CYCLES,
                                            "Dongle memory-leak corruption detected — skipping broadcast, waiting for next poll cycle"
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
                                        "Device model identified — enabling model-aware polling"
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
                                            "Three-phase model detected — increasing inter-request delay to {}ms",
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
                                            "Model-specific poll enabled — re-reading immediately"
                                        );
                                        return (true, true, false);
                                    }

                                } else if let Some(cached_type) = known_device_type {
                                    // Lock the device type to prevent dongle register corruption
                                    // (especially HR(21) arm_firmware_version) from flipping the
                                    // displayed model on a subsequent poll. Once identified, the
                                    // snapshot always carries the cached type — the decoder still
                                    // runs for the raw DTC and firmware string, but the refinement
                                    // result is ignored in favour of the known-good detection.
                                    if snapshot.device_type != cached_type {
                                        tracing::debug!(
                                            decoded = ?snapshot.device_type,
                                            cached = ?cached_type,
                                            "Device type mismatch — locking to cached value"
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
                                                        "Meter addr 0x{addr:02X}: responded with implausible voltage ({v1:.1}V) — rejected"
                                                    );
                                                } else {
                                                    tracing::debug!(
                                                        "Meter addr 0x{addr:02X}: responded with zero voltage — no meter present"
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
                                                "No external CT meters detected on first scan — will retry (ammeter expected)"
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
                                                "Meter discovery exhausted all retries — external ammeter configured but no meter responding"
                                            );
                                        } else {
                                            tracing::info!(
                                                retry = meter_retry_count,
                                                max = METER_MAX_RETRIES,
                                                "No external CT meters found — will retry"
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
                                        "Auto-discovered serial is suspect (truncated frame) — keeping empty serial for all requests. If the connection fails, try setting the serial manually in Settings."
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
                                let is_hv = known_device_type
                                    .map(|dt| dt.uses_hv_battery())
                                    .unwrap_or(false);
                                // Populated by the HV path below; consumed by
                                // derive_battery_fields_from_bms().
                                let mut hv_cluster: Option<
                                    crate::inverter::decoder::HvBcuCluster,
                                > = None;

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
                                                                "HV BCU at 0x{bcu_addr:02X} — {} modules",
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
                                                                "BCU 0x{bcu_addr:02X} probe: invalid version — no stack"
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
                                                    "BMS 0xA0 probe failed: {e} — falling back to direct BCU 0x70 probe"
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
                                                        "HV BMU 0x{bmu_addr:02X}: invalid serial — not present"
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
                                    // (confirmed against GivTCP's hvbmu.py — the BMU bank is
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
                                    // (battery #1) and additional batteries at 0x33, 0x34, … 0x37.
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

                                // Override battery temperature from BMS data for all
                                // device types (IR(56) is frequently garbage — #48).
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
                                        // Fall back to device type — Gen3+ types don't need it.
                                        snapshot.device_type.supports_manual_battery_calibration()
                                    }
                                } else {
                                    false // No battery modules — no calibration
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
                                            "Grace period complete — cumulative baseline set to median of grace readings"
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
                                        "First poll read after connect — data is flowing"
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
                                    // becomes None — this clears the persisted values.
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
                                            tracing::warn!("Cosy: enter writes failed — will retry on next poll");
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
                                                tracing::info!("Cosy: no upcoming slot — clearing charge slot registers");
                                                writes.extend(clear_cosy_slot_registers(snapshot.device_type));
                                            }
                                        } else {
                                            // Cosy mode was disabled while active — clear registers.
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
                                            tracing::warn!("Cosy: exit writes failed — will retry on next poll");
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
                                                    // No upcoming slot — clear registers if they were set.
                                                    if cosy_last_preloaded_slot.is_some() {
                                                        tracing::info!("Cosy: no upcoming slot — clearing charge slot registers");
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
                                        // Already in an active cosy slot — nothing to do.
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
                                            // Cache miss — fetch fresh prices from Octopus API.
                                            // Anchor to the start of TODAY (UTC) so the response always
                                            // includes the current slot. The Agile endpoint returns
                                            // results newest-first, so a bare page_size=48 returns
                                            // tomorrow's slots once they're published (~1pm) and the
                                            // current slot drops out of the window — which silently
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
                                                                                    tracing::info!("Agile: price {price}p ≤ {charge_threshold}p — force charging");
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
                                                                                    tracing::info!("Agile: price {price}p ≥ {discharge_threshold}p — force discharging");
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
                                                // Hold — price between thresholds: revert to Eco mode
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
                                        // Agile mode disabled — if we were actively
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
                                                // Cosy is in control — just clear the
                                                // agile flag, don't send CosyExit
                                                // (which would stop the cosy charge).
                                                drop(ag_state);
                                                *state.agile_state.lock().await = AgileState::Idle;
                                                persist_agile_state(AgileState::Idle);
                                                tracing::info!(
                                                    "Agile: disabled while {:?} but cosy is active — cleared flag without reverting",
                                                    was_state
                                                );
                                            } else {
                                                tracing::info!("Agile: mode disabled while {:?} — reverting to Eco", was_state);
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
                                                    tracing::warn!("Agile: exit writes failed — will retry on next poll");
                                                }
                                            }
                                        }
                                    }
                                }

                                // Reflect the (possibly updated) cosy_active flag
                                // AFTER the cosy state machine has run. Without this,
                                // the broadcast snapshot would carry the previous
                                // cycle's value for one poll after a slot transition
                                // — e.g. showing "Cosy Active" for an extra poll
                                // after the slot actually ended.
                                snapshot.cosy_active = *state.cosy_active.lock().await;
                                let ag = state.agile_state.lock().await;
                                snapshot.agile_active = *ag != AgileState::Idle;
                                snapshot.agile_state = format!("{:?}", *ag);

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
                                        "Poll read failed — connection lost, reconnecting"
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
                            if connection_lost {
                                break;
                            }
                            consecutive_failures += 1;
                            if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                                tracing::warn!(
                                    consecutive_failures,
                                    max = MAX_CONSECUTIVE_FAILURES,
                                    "Poll read failed 3× — reconnecting"
                                );
                                break;
                                // of breaking out of the inner loop — staying connected
                                // avoids the warmup + grace period on the next poll.
                            } else {
                                // Transient error — retry after a short pause
                                tracing::debug!(
                                    "Poll read failed ({}/{}) — retrying",
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
                            "Persistent fingerprint corruption — disconnecting"
                        );
                        break;
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
                    let sleep_deadline =
                        tokio::time::Instant::now() + Duration::from_secs(interval_secs);
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
                            tracing::info!(
                                "Settings changed (v{} → v{}) — reconnecting",
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
    use crate::inverter::model::{BatteryModule, DeviceType};

    #[test]
    fn derive_battery_fields_from_bms_overrides_single_phase_temp() {
        // Single-phase temperature from IR(56) is unreliable (#48). With BMS
        // module data available, battery_temperature is overridden with the
        // module average. Capacity and max_power come from HR(55)/decoder and
        // must be left alone.
        let mut snap = InverterSnapshot::default();
        snap.device_type = DeviceType::Gen3Hybrid;
        snap.battery_temperature = 31.5; // garbage-ish IR(56) value
        snap.battery_capacity_kwh = 5.12;
        snap.max_battery_power_w = 3600;
        snap.battery_modules = vec![
            BatteryModule {
                index: 0,
                temperature: 28.0,
                ..Default::default()
            },
            BatteryModule {
                index: 1,
                temperature: 30.0,
                ..Default::default()
            },
        ];
        derive_battery_fields_from_bms(&mut snap, None);
        // Temperature overridden to module average: (28.0 + 30.0) / 2 = 29.0
        assert!((snap.battery_temperature - 29.0).abs() < 0.01);
        // Capacity and max_power must remain untouched (single-phase path).
        assert_eq!(snap.battery_capacity_kwh, 5.12);
        assert_eq!(snap.max_battery_power_w, 3600);
    }

    #[test]
    fn derive_battery_fields_from_bms_three_phase_from_modules() {
        // Two modules: 100Ah + 200Ah at 76.8V nominal (three-phase) = 23.04 kWh.
        let mut snap = InverterSnapshot::default();
        snap.device_type = DeviceType::ThreePhase;
        // Garbage left by the single-phase IR(56)/HR(55) decode paths.
        snap.battery_temperature = 999.0;
        snap.battery_capacity_kwh = 0.0;
        snap.max_battery_power_w = 0;
        snap.battery_modules = vec![
            BatteryModule {
                index: 0,
                capacity_ah: 100.0,
                temperature: 28.0,
                ..Default::default()
            },
            BatteryModule {
                index: 1,
                capacity_ah: 200.0,
                temperature: 31.5,
                ..Default::default()
            },
        ];
        derive_battery_fields_from_bms(&mut snap, None);
        // Capacity: 300Ah * 76.8V / 1000 = 23.04 kWh.
        assert!((snap.battery_capacity_kwh - 23.04).abs() < 0.01);
        // Temperature: module average (28.0 + 31.5) / 2 = 29.75, not max.
        assert!((snap.battery_temperature - 29.75).abs() < 0.01);
        // Max power: min(6000 hardware, 23040W/2) = 6000.
        assert_eq!(snap.max_battery_power_w, 6000);
    }

    #[test]
    fn derive_battery_fields_from_bms_capacity_caps_max_power() {
        // Small battery: capacity-derived cap is below the hardware limit.
        // 50Ah at 76.8V = 3.84 kWh -> cap at 1920W < 6000W hardware limit.
        let mut snap = InverterSnapshot::default();
        snap.device_type = DeviceType::ThreePhase;
        snap.battery_modules = vec![BatteryModule {
            index: 0,
            capacity_ah: 50.0,
            temperature: 25.0,
            ..Default::default()
        }];
        derive_battery_fields_from_bms(&mut snap, None);
        assert!((snap.battery_capacity_kwh - 3.84).abs() < 0.01);
        // Temperature: single module, so average = 25.0.
        assert!((snap.battery_temperature - 25.0).abs() < 0.01);
        assert_eq!(snap.max_battery_power_w, 1920);
    }

    #[test]
    fn derive_battery_fields_from_bms_clears_garbage_when_no_bms() {
        // BMS read error and no HV cluster: no modules.
        // Must clear the garbage IR(56) value and fall back to hardware max.
        let mut snap = InverterSnapshot::default();
        snap.device_type = DeviceType::ThreePhase;
        snap.battery_temperature = 999.0; // garbage from IR(56)
        snap.battery_capacity_kwh = 999.0; // garbage from HR(55)
        snap.max_battery_power_w = 0;
        snap.battery_modules = vec![];
        derive_battery_fields_from_bms(&mut snap, None);
        assert!(
            snap.battery_temperature.is_nan(),
            "missing BMS data should produce NaN, not 0.0"
        );
        assert_eq!(snap.battery_capacity_kwh, 0.0);
        assert_eq!(snap.max_battery_power_w, 6000); // uncapped hardware limit
    }

    #[test]
    fn derive_battery_fields_from_bms_from_hv_cluster_with_modules() {
        // HV stack: 5 modules × 51Ah at 76.8V nominal (three-phase) = 19.58 kWh.
        // Matches a GIV-BAT-17.0-HV (5 × GIV-BAT-3.4-HV) on a GIV-3HY-11.
        // Temperature comes from BMU module average, NOT the BCU cluster IR(68)
        // (which can return stale/garbage values on some firmware — #48).
        use crate::inverter::decoder::HvBcuCluster;
        let cluster = HvBcuCluster {
            pack_software_version: "GA000005".to_string(),
            number_of_modules: 5,
            cells_per_module: 24,
            cluster_cell_voltage: 3.2,
            status: 0x01,
            battery_voltage: 384.0,
            battery_current: -12.5,
            battery_power_w: -4800,
            battery_soc_max: 87,
            battery_soc_min: 85,
            battery_soh: 99,
            temperature: 6.0, // Stale/garbage from IR(68) on firmware DA0.011
            nominal_capacity_ah: 51.0,
            remaining_capacity_ah: 44.0,
        };
        let mut snap = InverterSnapshot::default();
        snap.device_type = DeviceType::ThreePhase;
        snap.battery_voltage = 0.0; // inverter IR block missed / garbage
        snap.battery_current = 0.0;
        snap.soc = 0; // inverter IR(1132) returned 0
                      // Garbage left by the single-phase IR(56)/HR(55) decode paths.
        snap.battery_temperature = 999.0;
        snap.battery_capacity_kwh = 999.0;
        snap.max_battery_power_w = 0;
        // BMU modules with accurate cell-probe temperatures.
        snap.battery_modules = vec![
            BatteryModule {
                index: 0,
                temperature: 18.5,
                ..Default::default()
            },
            BatteryModule {
                index: 1,
                temperature: 19.0,
                ..Default::default()
            },
            BatteryModule {
                index: 2,
                temperature: 18.8,
                ..Default::default()
            },
            BatteryModule {
                index: 3,
                temperature: 19.2,
                ..Default::default()
            },
            BatteryModule {
                index: 4,
                temperature: 18.6,
                ..Default::default()
            },
        ];
        derive_battery_fields_from_bms(&mut snap, Some(&cluster));
        // Capacity: 5 × 51Ah × 76.8V / 1000 = 19.58 kWh.
        assert!((snap.battery_capacity_kwh - 19.584).abs() < 0.01);
        // Temperature from BMU module average (NOT cluster IR(68) = 6.0).
        // (18.5 + 19.0 + 18.8 + 19.2 + 18.6) / 5 = 18.82
        assert!((snap.battery_temperature - 18.82).abs() < 0.01);
        // Voltage/current overridden authoritatively from the BCU.
        assert!((snap.battery_voltage - 384.0).abs() < 0.01);
        assert!((snap.battery_current - -12.5).abs() < 0.01);
        // SOC fallback from the BCU's highest module SOC.
        assert_eq!(snap.soc, 87);
        // Max power: min(6000 hardware, 19584W/2) = 6000.
        assert_eq!(snap.max_battery_power_w, 6000);
    }

    #[test]
    fn derive_battery_fields_from_bms_hv_cluster_no_modules() {
        // HV cluster available but BMU reads failed — no modules.
        // Temperature is NaN (don't trust stale IR(68)), but capacity/
        // voltage/current/SOC still derived from the cluster.
        use crate::inverter::decoder::HvBcuCluster;
        let cluster = HvBcuCluster {
            pack_software_version: "GA000005".to_string(),
            number_of_modules: 5,
            cells_per_module: 24,
            cluster_cell_voltage: 3.2,
            status: 0x01,
            battery_voltage: 384.0,
            battery_current: -12.5,
            battery_power_w: -4800,
            battery_soc_max: 87,
            battery_soc_min: 85,
            battery_soh: 99,
            temperature: 6.0,
            nominal_capacity_ah: 51.0,
            remaining_capacity_ah: 44.0,
        };
        let mut snap = InverterSnapshot::default();
        snap.device_type = DeviceType::ThreePhase;
        snap.battery_temperature = 999.0;
        snap.battery_capacity_kwh = 999.0;
        snap.battery_voltage = 0.0;
        snap.battery_current = 0.0;
        snap.soc = 0;
        snap.max_battery_power_w = 0;
        snap.battery_modules = vec![];
        derive_battery_fields_from_bms(&mut snap, Some(&cluster));
        // Temperature: NaN — no modules to average, and IR(68) is untrusted.
        assert!(
            snap.battery_temperature.is_nan(),
            "expected NaN when no modules available, got {}",
            snap.battery_temperature
        );
        // Capacity, voltage, current, SOC still derived from cluster.
        assert!((snap.battery_capacity_kwh - 19.584).abs() < 0.01);
        assert!((snap.battery_voltage - 384.0).abs() < 0.01);
        assert_eq!(snap.soc, 87);
        assert_eq!(snap.max_battery_power_w, 6000);
    }

    #[test]
    fn hv_cluster_total_capacity_multiplies_by_module_count() {
        // Per-module Ah × module count = stack total.
        use crate::inverter::decoder::HvBcuCluster;
        let cluster = HvBcuCluster {
            nominal_capacity_ah: 51.0,
            remaining_capacity_ah: 44.0,
            number_of_modules: 5,
            ..Default::default()
        };
        assert!((cluster.total_capacity_ah() - 255.0).abs() < 0.001);
        assert!((cluster.total_remaining_ah() - 220.0).abs() < 0.001);
    }

    #[test]
    fn grace_median_rejects_single_spike_in_consumption() {
        // Reproduces the reported scenario: the first stable read after a
        // restart latched a corrupted-high `today_consumption_kwh = 44.5`,
        // then every correct reading (~43.4) was rejected as a "decrease".
        // The median of the three grace readings picks the true value.
        let mk = |consumption: f32| {
            let mut s = GraceCumulativeSamples::default();
            s.today_consumption_kwh = consumption;
            s
        };
        let samples = [mk(43.4), mk(44.5), mk(43.5)];
        let median = GraceCumulativeSamples::median(&samples);
        // Sorted: [43.4, 43.5, 44.5] -> middle is 43.5, the true reading.
        assert_eq!(median.today_consumption_kwh, 43.5);

        // A single low outlier is also rejected.
        let samples = [mk(44.5), mk(10.0), mk(44.6)];
        let median = GraceCumulativeSamples::median(&samples);
        assert_eq!(median.today_consumption_kwh, 44.5);
    }

    #[test]
    fn grace_median_handles_all_cumulative_fields_independently() {
        let mk = |consumption: f32, import: f32, total_import: f32| GraceCumulativeSamples {
            today_consumption_kwh: consumption,
            today_import_kwh: import,
            total_import_kwh: total_import,
            ..Default::default()
        };
        let samples = [
            mk(43.4, 5.0, 1000.0),
            mk(44.5, 50.0, 1000.0), // corrupted daily import spike
            mk(43.5, 5.1, 1000.1),
        ];
        let median = GraceCumulativeSamples::median(&samples);
        assert_eq!(median.today_consumption_kwh, 43.5);
        assert_eq!(median.today_import_kwh, 5.1);
        assert_eq!(median.total_import_kwh, 1000.0);
    }

    #[test]
    fn grace_median_apply_writes_all_fields_back() {
        let median = GraceCumulativeSamples {
            today_consumption_kwh: 43.5,
            total_import_kwh: 1000.0,
            ..Default::default()
        };
        let mut snap = InverterSnapshot {
            today_consumption_kwh: 44.5, // poisoned value
            total_import_kwh: 0.0,
            ..Default::default()
        };
        median.apply_to(&mut snap);
        assert_eq!(snap.today_consumption_kwh, 43.5);
        assert_eq!(snap.total_import_kwh, 1000.0);
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
    fn daily_energy_tiny_decrease_is_noise_not_repoll() {
        let prev = InverterSnapshot {
            timestamp: 100,
            battery_mode: BatteryMode::Eco,
            grid_voltage: 230.0,
            grid_frequency: 50.0,
            battery_reserve: 4,
            today_consumption_kwh: 7.6,
            ..Default::default()
        };
        let mut snap = InverterSnapshot {
            timestamp: 104,
            battery_mode: BatteryMode::Eco,
            grid_voltage: 230.0,
            grid_frequency: 50.0,
            battery_reserve: 4,
            today_consumption_kwh: 7.5,
            ..Default::default()
        };
        let mut pending_mode = None;
        let mut delta_corrections = DeltaCorrectionCounts::default();
        let mut suspect_counts = ConsecutiveSuspectCounts::default();

        let sanitized = sanitize_snapshot(
            &mut snap,
            Some(&prev),
            false,
            &mut pending_mode,
            &mut delta_corrections,
            &mut suspect_counts,
        );

        assert!(
            !sanitized,
            "noise tolerance must not force immediate re-poll"
        );
        assert_eq!(snap.today_consumption_kwh, prev.today_consumption_kwh);
    }

    #[test]
    fn daily_energy_material_decrease_is_sanitized() {
        let prev = InverterSnapshot {
            timestamp: 100,
            battery_mode: BatteryMode::Eco,
            grid_voltage: 230.0,
            grid_frequency: 50.0,
            battery_reserve: 4,
            today_consumption_kwh: 7.6,
            ..Default::default()
        };
        let mut snap = InverterSnapshot {
            timestamp: 104,
            battery_mode: BatteryMode::Eco,
            grid_voltage: 230.0,
            grid_frequency: 50.0,
            battery_reserve: 4,
            today_consumption_kwh: 7.3,
            ..Default::default()
        };
        let mut pending_mode = None;
        let mut delta_corrections = DeltaCorrectionCounts::default();
        let mut suspect_counts = ConsecutiveSuspectCounts::default();

        let sanitized = sanitize_snapshot(
            &mut snap,
            Some(&prev),
            false,
            &mut pending_mode,
            &mut delta_corrections,
            &mut suspect_counts,
        );

        assert!(sanitized);
        assert_eq!(snap.today_consumption_kwh, prev.today_consumption_kwh);
    }

    #[test]
    fn daily_energy_consecutive_decrease_releases_baseline() {
        // Simulate the scenario: the inverter consistently reports 7.1 kWh
        // but our baseline was 7.7 (from a corrupted grace-period reading).
        // After DELTA_CORRECTION_RELEASE_THRESHOLD consecutive corrections,
        // the raw value should be accepted.
        let prev_consumption = 7.7;
        let raw_consumption = 7.1;
        let mut delta_corrections = DeltaCorrectionCounts::default();
        let mut suspect_counts = ConsecutiveSuspectCounts::default();

        for i in 0..DELTA_CORRECTION_RELEASE_THRESHOLD {
            let prev = InverterSnapshot {
                timestamp: 100 + i as i64 * 3,
                battery_mode: BatteryMode::Eco,
                grid_voltage: 230.0,
                grid_frequency: 50.0,
                today_consumption_kwh: prev_consumption,
                ..Default::default()
            };
            let mut snap = InverterSnapshot {
                timestamp: 100 + (i as i64 + 1) * 3,
                battery_mode: BatteryMode::Eco,
                grid_voltage: 230.0,
                grid_frequency: 50.0,
                today_consumption_kwh: raw_consumption,
                ..Default::default()
            };
            let mut pending_mode = None;

            let _sanitized = sanitize_snapshot(
                &mut snap,
                Some(&prev),
                false,
                &mut pending_mode,
                &mut delta_corrections,
                &mut suspect_counts,
            );

            if i < DELTA_CORRECTION_RELEASE_THRESHOLD - 1 {
                // Before threshold: carry forward the baseline
                assert_eq!(
                    snap.today_consumption_kwh, prev_consumption,
                    "cycle {}: should carry forward baseline",
                    i
                );
            } else {
                // On threshold cycle: accept raw — the baseline was wrong
                assert_eq!(
                    snap.today_consumption_kwh, raw_consumption,
                    "cycle {}: should accept raw value after threshold",
                    i
                );
            }
        }

        // Counter should be reset after release
        assert!(
            delta_corrections.0.get("today_consumption_kwh").is_none()
                || *delta_corrections.0.get("today_consumption_kwh").unwrap() == 0
        );
    }

    #[test]
    fn daily_energy_correction_count_resets_on_normal_increase() {
        // If we get a few corrections then a normal increase, the counter resets.
        let mut delta_corrections = DeltaCorrectionCounts::default();
        let mut suspect_counts = ConsecutiveSuspectCounts::default();

        // First 3 cycles: decrease → correction
        for i in 0..3u8 {
            let prev = InverterSnapshot {
                timestamp: 100 + i as i64 * 3,
                battery_mode: BatteryMode::Eco,
                grid_voltage: 230.0,
                grid_frequency: 50.0,
                today_consumption_kwh: 7.7,
                ..Default::default()
            };
            let mut snap = InverterSnapshot {
                timestamp: 100 + (i as i64 + 1) * 3,
                battery_mode: BatteryMode::Eco,
                grid_voltage: 230.0,
                grid_frequency: 50.0,
                today_consumption_kwh: 7.1,
                ..Default::default()
            };
            let mut pending_mode = None;
            let _ = sanitize_snapshot(
                &mut snap,
                Some(&prev),
                false,
                &mut pending_mode,
                &mut delta_corrections,
                &mut suspect_counts,
            );
        }
        assert_eq!(
            *delta_corrections.0.get("today_consumption_kwh").unwrap(),
            3
        );

        // Normal increase: raw > prev → counter resets
        let prev = InverterSnapshot {
            timestamp: 109,
            battery_mode: BatteryMode::Eco,
            grid_voltage: 230.0,
            grid_frequency: 50.0,
            today_consumption_kwh: 7.1,
            ..Default::default()
        };
        let mut snap = InverterSnapshot {
            timestamp: 112,
            battery_mode: BatteryMode::Eco,
            grid_voltage: 230.0,
            grid_frequency: 50.0,
            today_consumption_kwh: 7.5,
            ..Default::default()
        };
        let mut pending_mode = None;
        let _ = sanitize_snapshot(
            &mut snap,
            Some(&prev),
            false,
            &mut pending_mode,
            &mut delta_corrections,
            &mut suspect_counts,
        );
        assert!(
            delta_corrections.0.get("today_consumption_kwh").is_none(),
            "counter should be reset after normal increase"
        );
    }

    #[test]
    fn external_meter_probe_is_single_shot_without_ammeter() {
        // No ammeter configured, first scan already done — no further probing.
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
        // enough cycles have passed — should retry.
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
        // EM115 configured but not enough cycles since last attempt — skip.
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
    fn optional_ac_config_carries_forward_when_block_missing() {
        let mut prev = InverterSnapshot {
            device_type: DeviceType::ACCoupled,
            charge_rate: 80,
            discharge_rate: 70,
            ac_export_priority: 2,
            ac_eps_enabled: true,
            battery_pause_mode: 1,
            ..Default::default()
        };
        prev.battery_pause_slot.enabled = true;
        prev.battery_pause_slot.start_hour = 1;
        prev.battery_pause_slot.end_hour = 2;

        let mut snap = InverterSnapshot {
            device_type: DeviceType::ACCoupled,
            ..Default::default()
        };

        let changed =
            carry_forward_optional_block_values(&mut snap, Some(&prev), false, true, true);
        assert!(changed);
        assert_eq!(snap.charge_rate, 80);
        assert_eq!(snap.discharge_rate, 70);
        assert_eq!(snap.ac_export_priority, 2);
        assert!(snap.ac_eps_enabled);
        assert_eq!(snap.battery_pause_mode, 1);
        assert!(snap.battery_pause_slot.enabled);
        assert_eq!(snap.battery_pause_slot.start_hour, 1);
        assert_eq!(snap.battery_pause_slot.end_hour, 2);
    }

    #[test]
    fn optional_ac_config_does_not_carry_forward_when_block_present() {
        let prev = InverterSnapshot {
            device_type: DeviceType::ACCoupled,
            charge_rate: 80,
            discharge_rate: 70,
            ..Default::default()
        };
        let mut snap = InverterSnapshot {
            device_type: DeviceType::ACCoupled,
            charge_rate: 20,
            discharge_rate: 30,
            ..Default::default()
        };

        let changed = carry_forward_optional_block_values(&mut snap, Some(&prev), true, true, true);
        assert!(!changed);
        assert_eq!(snap.charge_rate, 20);
        assert_eq!(snap.discharge_rate, 30);
    }

    #[test]
    fn optional_extended_slots_carry_forward_when_block_missing() {
        let mut prev = InverterSnapshot {
            device_type: DeviceType::Gen3Hybrid,
            ..Default::default()
        };
        prev.charge_slots[0].target_soc = 80;
        prev.discharge_slots[1].target_soc = 40;
        prev.charge_slots[2].enabled = true;
        prev.charge_slots[2].start_hour = 3;
        prev.charge_slots[2].end_hour = 4;
        prev.charge_slots[2].target_soc = 90;
        prev.discharge_slots[2].enabled = true;
        prev.discharge_slots[2].start_hour = 17;
        prev.discharge_slots[2].end_hour = 18;

        let mut snap = InverterSnapshot {
            device_type: DeviceType::Gen3Hybrid,
            ..Default::default()
        };

        let changed =
            carry_forward_optional_block_values(&mut snap, Some(&prev), true, false, true);
        assert!(changed);
        assert_eq!(snap.charge_slots[0].target_soc, 80);
        assert_eq!(snap.discharge_slots[1].target_soc, 40);
        assert!(snap.charge_slots[2].enabled);
        assert_eq!(snap.charge_slots[2].start_hour, 3);
        assert_eq!(snap.charge_slots[2].end_hour, 4);
        assert_eq!(snap.charge_slots[2].target_soc, 90);
        assert!(snap.discharge_slots[2].enabled);
        assert_eq!(snap.discharge_slots[2].start_hour, 17);
    }

    #[test]
    fn optional_extended_slots_do_not_carry_forward_when_block_present() {
        let mut prev = InverterSnapshot {
            device_type: DeviceType::Gen3Hybrid,
            ..Default::default()
        };
        prev.charge_slots[2].enabled = true;

        let mut snap = InverterSnapshot {
            device_type: DeviceType::Gen3Hybrid,
            ..Default::default()
        };

        let changed = carry_forward_optional_block_values(&mut snap, Some(&prev), true, true, true);
        assert!(!changed);
        assert!(!snap.charge_slots[2].enabled);
    }

    #[test]
    fn optional_three_phase_config_carries_forward_when_block_missing() {
        let prev = InverterSnapshot {
            device_type: DeviceType::ThreePhase,
            charge_rate: 81,
            discharge_rate: 72,
            battery_reserve: 15,
            target_soc: 95,
            ..Default::default()
        };
        let mut snap = InverterSnapshot {
            device_type: DeviceType::ThreePhase,
            ..Default::default()
        };

        let changed =
            carry_forward_optional_block_values(&mut snap, Some(&prev), true, true, false);
        assert!(changed);
        assert_eq!(snap.charge_rate, 81);
        assert_eq!(snap.discharge_rate, 72);
        assert_eq!(snap.battery_reserve, 15);
        assert_eq!(snap.target_soc, 95);
    }

    #[test]
    fn is_block_suspicious_rejects_known_corruption() {
        // Build a 60-register block matching the dongle memory-leak fingerprint.
        let mut data = [0u16; 60];
        data[28] = 0x4C32;
        data[30] = 0xA119;
        data[31] = 0x34EA;
        data[32] = 0xE77F;
        data[33] = 0xD475;
        data[35] = 0x4500;
        data[40] = 0xE4F9;
        data[41] = 0xC0A8;
        data[43] = 0xC0A8;
        data[46] = 0xC5E9;
        data[50] = 0x60EF;
        data[51] = 0x8018;
        data[52] = 0x43E0;
        data[53] = 0xF6CE;
        data[56] = 0x080A;
        data[58] = 0xFCC1;
        data[59] = 0x661E;
        assert!(is_block_suspicious(&data), "fingerprint should match");
    }

    #[test]
    fn is_block_suspicious_accepts_clean_data() {
        // All zeros should not match the fingerprint (need >5 hits).
        let data = [0u16; 60];
        assert!(!is_block_suspicious(&data), "clean data should not match");
    }

    #[test]
    fn is_block_suspicious_requires_60_registers() {
        let data = vec![0u16; 30];
        assert!(!is_block_suspicious(&data), "short block should not match");
    }

    #[test]
    fn is_block_suspicious_sub_threshold_is_not_suspicious() {
        // 3 hits is below the threshold of >5.
        let mut data = [0u16; 60];
        data[28] = 0x4C32;
        data[41] = 0xC0A8;
        data[43] = 0xC0A8;
        assert!(
            !is_block_suspicious(&data),
            "3hits should be below threshold"
        );
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
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"connection\""));
        let de: PollMessage = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(de, PollMessage::Connection { state: ConnectionState::Reconnecting, ref host } if host == "192.168.1.100")
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
}
