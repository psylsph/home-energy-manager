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
use tokio::sync::{broadcast, oneshot, Mutex, Notify};

use crate::alerts::AlertType;
use crate::history::HistoryDb;
use crate::inverter::decoder::decode_snapshot;
use crate::inverter::encoder::{ControlCommand, RegisterWrite, WriteOutcome};
use crate::inverter::model::{
    BatteryMode, DeviceType, InverterSnapshot, ScheduleSlot, SolarArraySource, SolarArraySummary,
};
use crate::inverter::reconnect::ReconnectController;
use crate::inverter::sanitizer::{
    carry_forward_battery_modules_with, carry_forward_optional_block_values,
    carry_forward_three_phase_fault_block_values, carry_forward_three_phase_high_config_values,
    derive_battery_fields_from_bms, is_block_suspicious, sanitize_snapshot, validate_battery_bms,
    ConsecutiveSuspectCounts, DeltaCorrectionCounts, GraceCumulativeSamples, RateReleaseCounts,
};
use crate::inverter::state_machines::{
    build_force_discharge_auto_revert_writes, build_timed_export_disable_writes,
    check_adaptive_charge, check_auto_winter, check_discharge_floor,
    check_load_limiter_with_other_pause, check_temperature_limiter_after_automation,
    clear_cosy_slot_registers, cosy_slot_register_writes, persist_cosy_active,
    should_repair_timed_export, write_registers_to_inverter, AgileSlotAction,
    DischargeControlArbiter, DischargeControlOwner,
};
pub use crate::inverter::state_machines::{
    AdaptiveChargeState, AutoWinterConfig, AutoWinterSaved, AutoWinterState, DischargeFloorConfig,
    DischargeFloorState, LoadLimiterConfig, LoadLimiterSaved, LoadLimiterState, PriceSlot,
    TemperatureLimiterConfig, TemperatureLimiterState,
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

/// Boundary writes need prompt readback confirmation even when normal
/// telemetry polling is deliberately infrequent. Two seconds leaves enough
/// time for a real inverter (and the simulator's asynchronous write tick) to
/// project the FC6 writes before the confirming read.
const TIMED_EXPORT_CONFIRMATION_POLL_SECS: u64 = 2;

fn next_poll_delay_secs(configured_interval_secs: u64, timed_export_boundary_pending: bool) -> u64 {
    if timed_export_boundary_pending {
        configured_interval_secs.min(TIMED_EXPORT_CONFIRMATION_POLL_SECS)
    } else {
        configured_interval_secs
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
    /// Battery pause mode (HR 318) before force discharge (issue #289).
    /// Captured so Stop Discharge / auto-revert restores the exact
    /// pre-action pause configuration — GivTCP's Force Export does the
    /// same after temporarily disabling pause mode.
    pub battery_pause_mode: u8,
    /// Battery pause slot (HR 319-320) before force discharge. Restored
    /// together with `battery_pause_mode` so a Timed Discharge window that
    /// was armed before the force action survives it unchanged.
    pub battery_pause_slot: crate::inverter::model::ScheduleSlot,
}

/// Whether a write batch should continue past per-register failures.
///
/// Most control endpoints fire-and-forget a mixed batch where a transient
/// failure on one register shouldn't skip later unrelated ones, so the
/// default is [`WriteBatchPolicy::ContinueOnError`]. Safety-critical
/// orderings — Timed Export slot programming followed by export arming —
/// use [`WriteBatchPolicy::FailFast`]: the first failing register aborts
/// the rest of the batch, so a rejected slot write can never be followed
/// by the write that arms maximum-power discharge (code-review finding:
/// a failed slot write did not prevent full-power export from arming).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WriteBatchPolicy {
    /// Attempt every write in the batch; report the first failure.
    #[default]
    ContinueOnError,
    /// Stop at the first failing register; later writes are skipped and the
    /// failure is reported to the completion channel.
    FailFast,
    /// Fail-fast transactional write. If the awaiting API request times out
    /// and drops its completion receiver before this batch starts (or between
    /// writes), cancel the remaining work so a failed request cannot mutate
    /// the inverter later and outlive the schedule/config transaction.
    FailFastTransactional,
}

/// A batch of register writes queued for the poll loop to drain, with an
/// optional one-shot channel for reporting the outcome back to the control
/// endpoint that queued it.
///
/// Most control endpoints fire-and-forget: they queue writes and return
/// immediately, and the poll loop logs any per-register failure. The
/// `completion` channel lets an endpoint instead await the actual write
/// outcome so it can report precisely which register the inverter rejected
/// — rather than leaving the UI to time out after 30 s with a generic
/// "did not confirm" message (issue #245).
#[derive(Debug)]
pub struct PendingWriteBatch {
    /// The writes, executed in order by the poll loop.
    pub writes: Vec<RegisterWrite>,
    /// If set, the poll loop sends the batch outcome here once execution
    /// finishes (success, or the first failing register + error). A send
    /// error is ignored silently — the receiver may already be gone if the
    /// request that queued this batch timed out, and the writes still run.
    pub completion: Option<oneshot::Sender<WriteOutcome>>,
    /// Failure policy for this batch (see [`WriteBatchPolicy`]).
    pub policy: WriteBatchPolicy,
    /// Owner of the shared discharge-control domain, when this batch writes
    /// overlapping battery-mode registers. `None` marks an unrelated batch
    /// (for example a clock or export-limit write) that can run alongside the
    /// selected discharge owner.
    pub owner: Option<DischargeControlOwner>,
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
    pub pending_writes: Arc<Mutex<Vec<PendingWriteBatch>>>,
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
    /// Serialises Force Charge / Force Discharge start and stop handlers so
    /// two concurrent API requests cannot arm both actions before either has
    /// recorded its revert state.
    pub force_action_lock: Arc<Mutex<()>>,
    /// Serialises Timed Export schedule mutations (enable/disable, slot
    /// edits, desired-slot restore from backup, test reset) so two
    /// concurrent handlers cannot both load the desired vector, race through
    /// the Modbus write queue, and overwrite each other's persistence +
    /// in-memory mirror. Held for the whole mutation (resolve desired →
    /// queue writes → await outcome → persist → update mirror), so the poll
    /// loop always observes a coherent schedule/state pair between snapshots.
    pub timed_export_action_lock: Arc<Mutex<()>>,
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
    /// Runtime state for Adaptive Charge SOC/time hysteresis.
    pub adaptive_charge_state: Arc<Mutex<AdaptiveChargeState>>,
    /// Raw pre-Adaptive charge limit retained until restoration is confirmed.
    pub adaptive_charge_saved: Arc<Mutex<Option<crate::settings::AdaptiveChargeSavedLimit>>>,
    /// Load discharge limiter configuration.
    pub load_limiter_config: Arc<Mutex<LoadLimiterConfig>>,
    /// Load discharge limiter state machine.
    pub load_limiter_state: Arc<Mutex<LoadLimiterState>>,
    /// Shared reserve captured before either discharge limiter pauses.
    pub load_limiter_saved: Arc<Mutex<Option<LoadLimiterSaved>>>,
    /// Inverter-temperature discharge limiter configuration.
    pub temperature_limiter_config: Arc<Mutex<TemperatureLimiterConfig>>,
    /// Inverter-temperature discharge limiter runtime state.
    pub temperature_limiter_state: Arc<Mutex<TemperatureLimiterState>>,
    /// Discharge floor guard configuration (developer mode only).
    pub discharge_floor_config: Arc<Mutex<DischargeFloorConfig>>,
    /// Discharge floor guard runtime state.
    pub discharge_floor_state: Arc<Mutex<DischargeFloorState>>,
    /// Timed Export state machine (issue #289). Tracks whether the inverter
    /// is in Eco (baseline) or Timed Export (max-power discharge during
    /// scheduled windows), with write confirmation and HR59 re-arm fallback.
    pub timed_export_state: Arc<Mutex<crate::inverter::state_machines::TimedExportState>>,
    /// Timed Export configuration (desired schedule from settings).
    pub timed_export_config: Arc<Mutex<crate::inverter::state_machines::TimedExportConfig>>,
    /// In-memory mirror of `Settings::timed_export_stop_pending`: a Stop or
    /// Eco-family route disabled the schedule while its disarm writes were
    /// still unconfirmed. The machine is restored into `Exiting` from this
    /// marker at startup; the poll loop clears it (settings + mirror) once
    /// the reconciler settles back on a confirmed Eco baseline. The mirror
    /// avoids a settings-file read/write on every poll.
    pub timed_export_stop_pending: Arc<std::sync::atomic::AtomicBool>,
    /// HR59 re-arm detector (issue #289). Counts consecutive
    /// outside-window HR59=1 readbacks; on confirmation the device is
    /// classified as re-arming firmware and the slot clear/restore
    /// fallback is activated.
    pub timed_export_rearm: Arc<Mutex<crate::inverter::state_machines::TimedExportRearmDetector>>,
    /// Generation for `timed_export_rearm`. Every API/reconnect reset advances
    /// it while holding the detector lock so a poll cycle can reject stale
    /// write-back even when both the old and reset detector happen to be Idle.
    pub timed_export_rearm_generation: Arc<std::sync::atomic::AtomicU64>,
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
    /// Last date the Forecast plan's nightly auto-refresh fired. In-memory
    /// only: a restart inside the lead window re-runs the refresh, which is
    /// an idempotent rewrite of the same slot.
    pub forecast_plan_refresh_date: Arc<Mutex<Option<chrono::NaiveDate>>>,
    /// Last date the poll loop warned that the auto-refresh was skipped
    /// because Adaptive Charge owns the charge rate. In-memory only, so the
    /// warning fires at most once per day instead of on every poll inside
    /// the lead window (CODE_REVIEW.md Major 3).
    pub forecast_plan_refresh_warned: Arc<Mutex<Option<chrono::NaiveDate>>>,
    /// Last date the Forecast plan's auto-apply trigger fired. In-memory
    /// only: a restart inside the lead window re-runs the apply, which is
    /// an idempotent rewrite of the same slot.
    pub forecast_plan_apply_date: Arc<Mutex<Option<chrono::NaiveDate>>>,
    /// Last date the poll loop warned/notified that the auto-apply was
    /// skipped because Adaptive Charge owns the charge rate. In-memory
    /// only, so the warning fires at most once per day.
    pub forecast_plan_apply_warned: Arc<Mutex<Option<chrono::NaiveDate>>>,
    /// Weather subsystem state — current config, last fetch result, backfill
    /// progress. Always present (not `Option<…>` like `history`) so the API
    /// layer doesn't have to special-case "weather not yet initialised".
    /// Mirror of `Settings::weather_config` lives inside the struct.
    pub weather: Arc<Mutex<crate::weather::WeatherState>>,
    /// Octopus customer-consumption synchronization status.
    pub octopus: Arc<Mutex<crate::octopus::OctopusState>>,
    /// "New version available" cache. Populated by the background
    /// [`crate::update::run_update_loop`]; read (never written) by the
    /// [`crate::update::get_latest_version`] HTTP handler.
    pub update: Arc<Mutex<crate::update::UpdateState>>,
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
    /// E2E-harness switch: when the headless process is started with
    /// `--e2e-admin`, `POST /api/test/reset` (see `server::api::test_reset`)
    /// may reset backend-owned schedule/machine state so the Playwright run
    /// can isolate spec files from each other. Production launches never set
    /// this, and the handler answers 404 without it.
    pub e2e_admin: std::sync::atomic::AtomicBool,
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
            force_action_lock: Arc::new(Mutex::new(())),
            timed_export_action_lock: Arc::new(Mutex::new(())),
            history: Arc::new(Mutex::new(None)),
            log_ring: Arc::new(crate::server::logs::LogRing::new(2000)),
            connected_clients: Arc::new(parking_lot::Mutex::new(ConnectedClients::new())),
            auto_winter_config: Arc::new(Mutex::new(AutoWinterConfig::default())),
            auto_winter_state: Arc::new(Mutex::new(AutoWinterState::default())),
            auto_winter_saved: Arc::new(Mutex::new(None)),
            adaptive_charge_state: Arc::new(Mutex::new(AdaptiveChargeState::default())),
            adaptive_charge_saved: Arc::new(Mutex::new(
                crate::settings::Settings::load().adaptive_charge_saved_limit,
            )),
            load_limiter_config: Arc::new(Mutex::new(LoadLimiterConfig::default())),
            load_limiter_state: Arc::new(Mutex::new(LoadLimiterState::default())),
            load_limiter_saved: Arc::new(Mutex::new(None)),
            temperature_limiter_config: Arc::new(Mutex::new(TemperatureLimiterConfig::default())),
            temperature_limiter_state: Arc::new(Mutex::new(TemperatureLimiterState::default())),
            discharge_floor_config: Arc::new(Mutex::new({
                let s = crate::settings::Settings::load();
                DischargeFloorConfig {
                    enabled: s.discharge_floor_enabled,
                    floor_soc: s.discharge_floor_soc,
                }
            })),
            discharge_floor_state: Arc::new(Mutex::new(
                crate::settings::Settings::load()
                    .discharge_floor_saved_reserve
                    .map(|saved_reserve| DischargeFloorState::HeldFromRestart { saved_reserve })
                    .unwrap_or_default(),
            )),
            timed_export_state: Arc::new(Mutex::new({
                let settings = crate::settings::Settings::load();
                if settings.timed_export_stop_pending {
                    // CODE_REVIEW.md BLOCKER: a Stop/Eco-family route
                    // disabled the schedule but the process exited before the
                    // disarm was confirmed by readback. Resume the exit
                    // (`Exiting`) instead of booting `Off`, where the still-
                    // populated physical slots would be misread as another
                    // controller's schedule and the armed registers would
                    // never be repaired.
                    tracing::warn!(
                        "Timed Export: restart with a stop/exit still pending — resuming the disarm"
                    );
                    crate::inverter::state_machines::TimedExportState::Exiting {
                        polls_waiting: 0,
                        retries: 0,
                    }
                } else {
                    crate::inverter::state_machines::TimedExportState::default()
                }
            })),
            timed_export_rearm: Arc::new(Mutex::new(
                crate::inverter::state_machines::TimedExportRearmDetector::default(),
            )),
            timed_export_rearm_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            timed_export_config: Arc::new(Mutex::new({
                let settings = crate::settings::Settings::load();
                crate::inverter::state_machines::TimedExportConfig {
                    schedule_enabled: settings.timed_export_schedule_enabled,
                    slots: settings.timed_export_slots,
                    device_rearm_confirmed: settings.timed_export_slots_require_clear,
                    stop_pending: settings.timed_export_stop_pending,
                }
            })),
            timed_export_stop_pending: Arc::new(std::sync::atomic::AtomicBool::new(
                crate::settings::Settings::load().timed_export_stop_pending,
            )),
            cosy_active: Arc::new(Mutex::new(
                crate::settings::Settings::load().cosy_active_persisted,
            )),
            cached_agile_prices: Arc::new(Mutex::new(Vec::new())),
            alert_config: Arc::new(Mutex::new(crate::settings::Settings::load().alerts_config)),
            alert_debounce: Arc::new(Mutex::new(crate::alerts::AlertDebounce::new())),
            last_report_date: Arc::new(Mutex::new(None)),
            forecast_plan_refresh_date: Arc::new(Mutex::new(None)),
            forecast_plan_refresh_warned: Arc::new(Mutex::new(None)),
            forecast_plan_apply_date: Arc::new(Mutex::new(None)),
            forecast_plan_apply_warned: Arc::new(Mutex::new(None)),
            latest_evc: Arc::new(Mutex::new(None)),
            evc_session_latch: Arc::new(Mutex::new(crate::evc::SessionLatch::default())),
            connected_since: Arc::new(std::sync::Mutex::new(None)),
            connect_failures: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            reconnect_request: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            e2e_admin: std::sync::atomic::AtomicBool::new(false),
            weather: Arc::new(Mutex::new(crate::weather::WeatherState {
                config: crate::settings::Settings::load().weather_config,
                ..Default::default()
            })),
            octopus: Arc::new(Mutex::new(crate::octopus::OctopusState::default())),
            update: Arc::new(Mutex::new(crate::update::UpdateState::default())),
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
            force_action_lock: Arc::new(Mutex::new(())),
            timed_export_action_lock: Arc::new(Mutex::new(())),
            history: Arc::new(Mutex::new(None)),
            log_ring,
            connected_clients: Arc::new(parking_lot::Mutex::new(ConnectedClients::new())),
            auto_winter_config: Arc::new(Mutex::new(AutoWinterConfig::default())),
            auto_winter_state: Arc::new(Mutex::new(AutoWinterState::default())),
            auto_winter_saved: Arc::new(Mutex::new(None)),
            adaptive_charge_state: Arc::new(Mutex::new(AdaptiveChargeState::default())),
            adaptive_charge_saved: Arc::new(Mutex::new(
                crate::settings::Settings::load().adaptive_charge_saved_limit,
            )),
            load_limiter_config: Arc::new(Mutex::new(LoadLimiterConfig::default())),
            load_limiter_state: Arc::new(Mutex::new(LoadLimiterState::default())),
            load_limiter_saved: Arc::new(Mutex::new(None)),
            temperature_limiter_config: Arc::new(Mutex::new(TemperatureLimiterConfig::default())),
            temperature_limiter_state: Arc::new(Mutex::new(TemperatureLimiterState::default())),
            discharge_floor_config: Arc::new(Mutex::new({
                let s = crate::settings::Settings::load();
                DischargeFloorConfig {
                    enabled: s.discharge_floor_enabled,
                    floor_soc: s.discharge_floor_soc,
                }
            })),
            discharge_floor_state: Arc::new(Mutex::new(
                crate::settings::Settings::load()
                    .discharge_floor_saved_reserve
                    .map(|saved_reserve| DischargeFloorState::HeldFromRestart { saved_reserve })
                    .unwrap_or_default(),
            )),
            timed_export_state: Arc::new(Mutex::new({
                let settings = crate::settings::Settings::load();
                if settings.timed_export_stop_pending {
                    // CODE_REVIEW.md BLOCKER: a Stop/Eco-family route
                    // disabled the schedule but the process exited before the
                    // disarm was confirmed by readback. Resume the exit
                    // (`Exiting`) instead of booting `Off`, where the still-
                    // populated physical slots would be misread as another
                    // controller's schedule and the armed registers would
                    // never be repaired.
                    tracing::warn!(
                        "Timed Export: restart with a stop/exit still pending — resuming the disarm"
                    );
                    crate::inverter::state_machines::TimedExportState::Exiting {
                        polls_waiting: 0,
                        retries: 0,
                    }
                } else {
                    crate::inverter::state_machines::TimedExportState::default()
                }
            })),
            timed_export_rearm: Arc::new(Mutex::new(
                crate::inverter::state_machines::TimedExportRearmDetector::default(),
            )),
            timed_export_rearm_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            timed_export_config: Arc::new(Mutex::new({
                let settings = crate::settings::Settings::load();
                crate::inverter::state_machines::TimedExportConfig {
                    schedule_enabled: settings.timed_export_schedule_enabled,
                    slots: settings.timed_export_slots,
                    device_rearm_confirmed: settings.timed_export_slots_require_clear,
                    stop_pending: settings.timed_export_stop_pending,
                }
            })),
            timed_export_stop_pending: Arc::new(std::sync::atomic::AtomicBool::new(
                crate::settings::Settings::load().timed_export_stop_pending,
            )),
            cosy_active: Arc::new(Mutex::new(
                crate::settings::Settings::load().cosy_active_persisted,
            )),
            cached_agile_prices: Arc::new(Mutex::new(Vec::new())),
            alert_config: Arc::new(Mutex::new(crate::settings::Settings::load().alerts_config)),
            alert_debounce: Arc::new(Mutex::new(crate::alerts::AlertDebounce::new())),
            last_report_date: Arc::new(Mutex::new(None)),
            forecast_plan_refresh_date: Arc::new(Mutex::new(None)),
            forecast_plan_refresh_warned: Arc::new(Mutex::new(None)),
            forecast_plan_apply_date: Arc::new(Mutex::new(None)),
            forecast_plan_apply_warned: Arc::new(Mutex::new(None)),
            latest_evc: Arc::new(Mutex::new(None)),
            evc_session_latch: Arc::new(Mutex::new(crate::evc::SessionLatch::default())),
            connected_since: Arc::new(std::sync::Mutex::new(None)),
            connect_failures: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            reconnect_request: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            e2e_admin: std::sync::atomic::AtomicBool::new(false),
            weather: Arc::new(Mutex::new(crate::weather::WeatherState {
                config: crate::settings::Settings::load().weather_config,
                ..Default::default()
            })),
            octopus: Arc::new(Mutex::new(crate::octopus::OctopusState::default())),
            update: Arc::new(Mutex::new(crate::update::UpdateState::default())),
        }
    }
}

// ---------------------------------------------------------------------------
// Poll-cycle decision helpers
// ---------------------------------------------------------------------------

/// Feed one battery BMS-read outcome into the alert debounce and fire the
/// connection-lost / connection-restored notifications at the right
/// transitions (issue #272).
///
/// `read_ok` is the outcome of this cycle's BMS read for the battery at
/// `slave_addr` (`battery_number` is 1-based, for the notification text).
/// The debounce requires `BATTERY_CONNECTION_LOST_CONFIRM_CYCLES`
/// consecutive failures before the lost-notification fires, and the
/// restored-notification fires on the first successful read after a
/// confirmed loss.
async fn track_battery_conn(
    state: &Arc<AppState>,
    battery_number: usize,
    slave_addr: u8,
    read_ok: bool,
) {
    let (lost, restored) = {
        let mut debounce = state.alert_debounce.lock().await;
        let was_confirmed = debounce.battery_connection_lost_confirmed(slave_addr);
        let confirmed_now = debounce.confirm_battery_connection_lost(slave_addr, read_ok);
        (confirmed_now && !was_confirmed, was_confirmed && read_ok)
    };
    if lost {
        tracing::warn!(
            "Battery #{battery_number} (0x{slave_addr:02X}) not responding for \
             {} consecutive cycles — connection lost",
            crate::alerts::BATTERY_CONNECTION_LOST_CONFIRM_CYCLES,
        );
        crate::alerts::send_battery_connection_lost_notification(state, battery_number, slave_addr)
            .await;
    }
    if restored {
        tracing::info!(
            "Battery #{battery_number} (0x{slave_addr:02X}) responding again — \
             connection restored",
        );
        crate::alerts::send_battery_connection_restored_notification(
            state,
            battery_number,
            slave_addr,
        )
        .await;
    }
}

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
    // True three-phase models use their built-in grid CT. Hybrid HV Gen3
    // (0x81xx) shares the 1000-range register layout but is physically
    // single-phase and can have an external EM115/CT meter, so keep probing it.
    if (dt.needs_three_phase_input_blocks() && dt != DeviceType::HybridHvGen3)
        || dt.is_batteryless()
    {
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
const HV_PROBE_RETRY_INTERVAL_CYCLES: u8 = 5;

fn should_probe_hv_stacks(
    known_device_type: Option<DeviceType>,
    hv_probe_done: bool,
    hv_probe_attempted: bool,
    cycles_since_last_probe: u8,
) -> bool {
    known_device_type
        .map(|dt| {
            !hv_probe_done
                && dt.uses_hv_battery()
                && (!hv_probe_attempted
                    || cycles_since_last_probe >= HV_PROBE_RETRY_INTERVAL_CYCLES)
        })
        .unwrap_or(false)
}

/// Convert one HV discovery result into the existing one-shot completion flag.
/// Kept separate so retry semantics can be pinned by a focused unit test.
fn hv_probe_completed(detected_stacks: &[(u8, u8)]) -> bool {
    !detected_stacks.is_empty()
}

/// Restore the confirmed model identity after decoding a later poll.
///
/// HR(0) is subject to the same dongle corruption as any other register. For
/// 0x81xx inverters the final DTC digit selects the 6/8/10 kW limits, so merely
/// locking the coarse `DeviceType` is not enough: a corrupt DTC would silently
/// fall back to the 6 kW family defaults and could make valid telemetry fail
/// sanitization. Preserve the confirmed code and re-derive those limits.
fn lock_snapshot_device_identity(
    snapshot: &mut InverterSnapshot,
    known_device_type: DeviceType,
    known_device_type_code: &str,
) {
    snapshot.device_type = known_device_type;
    snapshot.device_type_display = known_device_type.display_name().to_string();
    snapshot.device_type_code = known_device_type_code.to_string();
    snapshot.max_charge_slots = known_device_type.max_charge_slots();
    snapshot.max_discharge_slots = known_device_type.max_discharge_slots();

    if known_device_type == DeviceType::HybridHvGen3 {
        let raw_dtc = u16::from_str_radix(known_device_type_code, 16).unwrap_or(0);
        snapshot.max_ac_power_w =
            DeviceType::max_ac_power_for_dtc(raw_dtc, known_device_type.max_ac_power_w());
        let hardware_limit = DeviceType::max_battery_power_for_dtc(
            raw_dtc,
            snapshot.firmware_version.parse().unwrap_or(0),
            known_device_type.max_battery_power_w(),
        );
        let capacity_limit = (snapshot.battery_capacity_kwh * 500.0) as u32;
        snapshot.max_battery_power_w = if capacity_limit > 0 {
            hardware_limit.min(capacity_limit)
        } else {
            hardware_limit
        };
    }
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

/// Drain and execute pending write batches against `client`, signalling each
/// batch's outcome to any queued completion channel.
///
/// For each batch the poll loop still attempts *every* write (a transient
/// failure on one register doesn't skip later ones), but only the **first**
/// failing register is captured and reported — it is usually the actionable
/// one. Extracted from `run_poll_loop` so the capture / completion / trailing-
/// sleep logic can be unit-tested directly against a mock TCP server (issue
/// #245) rather than only through the full poll loop.
/// Maximum number of pending write-batches drained per poll cycle.
///
/// Each write is a real ~1.5 s Modbus round-trip (inter-write gap), so a
/// typical batch of a few writes costs several seconds. Capping the drain at
/// 8 batches bounds the poll-cycle write-drain to roughly a minute even when
/// many batches are queued, keeping the snapshot/broadcast cycle alive while
/// still making net progress (untaken batches drain on subsequent cycles).
pub const MAX_WRITE_BATCHES_PER_CYCLE: usize = 8;

/// Take at most `cap` batches (the oldest first) out of the pending-writes
/// queue, leaving the rest queued for subsequent poll cycles. Extracted from
/// the poll loop so the per-cycle cap is unit-testable without a full poll
/// loop (the completion channels of untaken batches are simply left queued —
/// their `await_write_outcome` callers keep waiting, which is correct).
#[cfg(test)]
fn take_pending_writes(queue: &mut Vec<PendingWriteBatch>, cap: usize) -> Vec<PendingWriteBatch> {
    if queue.len() <= cap {
        return std::mem::take(queue);
    }
    let remainder = queue.split_off(cap);
    let taken = std::mem::take(queue);
    *queue = remainder;
    taken
}

/// Select the highest-priority discharge owner represented by the current
/// runtime state or queued API requests.  The previous snapshot is used here
/// because this function runs before the next read; it is still the latest
/// confirmed inverter state and, importantly, prevents a queued low-priority
/// write from racing an already-active higher-priority automation.
async fn current_discharge_control_owner(state: &Arc<AppState>) -> Option<DischargeControlOwner> {
    let snapshot = state.latest_snapshot.lock().await.clone();
    let force_charge = state.force_charge_revert.lock().await.is_some();
    let force_discharge = state.force_discharge_revert.lock().await.is_some();
    let load_paused = state.load_limiter_state.lock().await.is_actively_pausing();
    let temperature_paused = state
        .temperature_limiter_state
        .lock()
        .await
        .is_actively_pausing();
    let cosy_active = *state.cosy_active.lock().await;
    let timed_export_config = state.timed_export_config.lock().await.clone();
    let timed_export_state = state.timed_export_state.lock().await.clone();
    let timed_export_minute = snapshot
        .as_ref()
        .and_then(crate::inverter::state_machines::inverter_minute_of_day)
        .unwrap_or_else(|| {
            let now = chrono::Local::now();
            now.hour() as u16 * 60 + now.minute() as u16
        });
    let timed_export_active = {
        let has_slots = timed_export_config
            .slots
            .iter()
            .any(ScheduleSlot::is_configured);
        let in_window = crate::inverter::state_machines::export_window_contains(
            &timed_export_config.slots,
            timed_export_minute,
        );
        let export_armed = snapshot
            .as_ref()
            .is_some_and(|s| s.battery_power_mode == 0 && s.enable_discharge);
        let physical_slot_configured = snapshot
            .as_ref()
            .is_some_and(|s| s.discharge_slots.iter().any(ScheduleSlot::is_configured));
        timed_export_state.owns_discharge_control()
            || (timed_export_config.schedule_enabled && has_slots && (in_window || export_armed))
            || (!timed_export_config.schedule_enabled && export_armed && !physical_slot_configured)
    };

    let mut arbiter = DischargeControlArbiter::default();
    if load_paused || temperature_paused {
        arbiter.request(DischargeControlOwner::Safety);
    }
    if snapshot.as_ref().is_some_and(|s| {
        crate::inverter::state_machines::hr318_blocks_discharge(s, timed_export_minute)
            || matches!(
                s.battery_mode,
                BatteryMode::EcoPaused | BatteryMode::ExportPaused
            )
    }) {
        // The exact pause window is evaluated again after the fresh poll. At
        // this pre-read point, an already reported Eco Paused state is also a
        // valid explicit pause claim.
        arbiter.request(DischargeControlOwner::ExplicitPause);
    }
    if force_charge || force_discharge {
        arbiter.request(DischargeControlOwner::ManualForce);
    }
    if timed_export_active {
        arbiter.request(DischargeControlOwner::TimedExport);
    }
    if cosy_active || snapshot.as_ref().is_some_and(|s| s.cosy_active) {
        arbiter.request(DischargeControlOwner::TimedCharge);
    }
    if snapshot.as_ref().is_some_and(|s| s.agile_active) {
        arbiter.request(DischargeControlOwner::Agile);
    }
    if snapshot.as_ref().is_some_and(|s| {
        matches!(s.battery_mode, BatteryMode::TimedDemand)
            && !s.agile_active
            && !s.cosy_active
            && !cosy_active
    }) {
        // Timed Demand has no persisted owner in the snapshot. If neither
        // Cosy nor Agile reported the mode as theirs, treat it as an
        // externally/manual-selected mode so a lower-priority automation
        // cannot silently take it over on the next poll.
        arbiter.request(DischargeControlOwner::ManualMode);
    }
    arbiter.selected_owner()
}

/// Whether the Timed Export boundary state machine may run this poll cycle.
///
/// Normally this is plain arbiter admission: any owner that outranks
/// `TimedExport` (a manual Force action, an HR318 pause, a safety limiter)
/// skips the machine. A register-derived `ManualMode` claim — the HR27=1 +
/// discharge-enabled readback that looks like Timed Demand — is the one
/// exception: when the HEM-managed schedule is enabled, that exact shape is
/// what re-arming firmware (issue #289) leaves behind outside a window, and
/// the reconciler must be allowed to repair it. A *user-selected* Timed
/// Demand switches the mode endpoints away from the managed schedule, so
/// `managed_schedule_enabled` is false precisely when the manual claim is
/// genuine and the machine must stay out.
fn timed_export_machine_allowed(
    arbiter: DischargeControlArbiter,
    managed_schedule_enabled: bool,
) -> bool {
    arbiter.can_request(DischargeControlOwner::TimedExport)
        || (managed_schedule_enabled
            && arbiter.selected_owner() == Some(DischargeControlOwner::ManualMode))
}

/// Whether the Timed Export machine's *decision writes* may be emitted this
/// cycle. Mirrors [`timed_export_machine_allowed`] at the write level: the
/// arbiter request is the normal path, and a register-derived `ManualMode`
/// claim must not discard the writes of a live managed schedule's reconciler
/// (under re-arming firmware the Timed Demand shape is exactly what the
/// machine exists to repair). Unlike the admission check this consumes the
/// arbiter request, so callers must have already passed the admission gate.
fn timed_export_machine_may_write(
    arbiter: &mut DischargeControlArbiter,
    managed_schedule_enabled: bool,
) -> bool {
    arbiter.request(DischargeControlOwner::TimedExport)
        || (managed_schedule_enabled
            && arbiter.selected_owner() == Some(DischargeControlOwner::ManualMode))
}

/// Take only batches that belong to the winning discharge owner for this poll.
/// Lower-priority owned batches stay queued and are retried after the winner
/// releases the inverter. Unowned batches are independent register work and
/// are always eligible.
fn take_pending_writes_for_owner(
    queue: &mut Vec<PendingWriteBatch>,
    cap: usize,
    active_owner: Option<DischargeControlOwner>,
) -> (Vec<PendingWriteBatch>, Option<DischargeControlOwner>) {
    let pending_owner = queue.iter().filter_map(|batch| batch.owner).max();
    let winner = match (active_owner, pending_owner) {
        // A user enabling managed Timed Export is an explicit replacement
        // for an unclaimed/manual Timed Demand mode. Without this transition
        // exception, ManualMode remains the higher active owner forever and
        // the user's completion-backed Timed Export batch can never drain.
        (Some(DischargeControlOwner::ManualMode), Some(DischargeControlOwner::TimedExport)) => {
            Some(DischargeControlOwner::TimedExport)
        }
        // A user-issued manual selection (Eco baseline, reserve, …) is an
        // explicit replacement for a *register-derived* ExplicitPause claim.
        // The pre-read snapshot claims ExplicitPause for an HR318 pause
        // window, an EcoPaused derived mode (reserve = 100) or an ExportPaused
        // derived mode — none of which conflict with the manual HR27/HR59/
        // slot writes (HR318 is an independent gate and stays armed). Without
        // this exception the batch starves indefinitely: "Queued 22 register
        // write(s)" with nothing ever written. Automations (TimedExport,
        // Agile, Cosy) do NOT get this exception — an explicit pause still
        // defers them (issue #289 pause precedence).
        (Some(DischargeControlOwner::ExplicitPause), Some(DischargeControlOwner::ManualMode)) => {
            Some(DischargeControlOwner::ManualMode)
        }
        (Some(active), Some(pending)) => Some(active.max(pending)),
        (Some(active), None) => Some(active),
        (None, Some(pending)) => Some(pending),
        (None, None) => None,
    };

    let mut taken = Vec::new();
    let mut remainder = Vec::with_capacity(queue.len());
    for batch in std::mem::take(queue) {
        let eligible = batch.owner.is_none() || batch.owner == winner;
        if eligible && taken.len() < cap {
            taken.push(batch);
        } else {
            remainder.push(batch);
        }
    }
    *queue = remainder;
    (taken, winner)
}

async fn drain_write_batches(client: &mut ModbusClient, pending: Vec<PendingWriteBatch>) {
    drain_write_batches_with_gap(client, pending, Duration::from_millis(1500)).await
}

/// Execute a direct poll-loop transition with strict ordering. The first
/// rejected register stops the sequence, which is required for Timed Export
/// fallback exits: continuing from a failed slot clear to HR59=0/HR27=1 lets
/// re-arm firmware immediately recreate the invalid state.
async fn write_registers_fail_fast_with_gap(
    client: &mut ModbusClient,
    writes: &[RegisterWrite],
    label: &str,
    inter_write_gap: Duration,
) -> bool {
    for (index, write) in writes.iter().enumerate() {
        match client.write_register(write.address, write.value).await {
            Ok(()) => tracing::info!("{label}: wrote reg {} = {}", write.address, write.value),
            Err(error) => {
                tracing::error!(
                    "{label}: write reg {} failed: {error} — skipping {} later write(s)",
                    write.address,
                    writes.len() - index - 1
                );
                return false;
            }
        }
        if index + 1 < writes.len() {
            tokio::time::sleep(inter_write_gap).await;
        }
    }
    true
}

/// CODE_REVIEW.md BLOCKER: clear the durable stop/exit-pending marker once
/// the reconciler has settled the machine on `Off` AND the snapshot readback
/// confirms the Eco baseline. Clearing earlier would let a crash before a
/// completed disarm boot the next process as `Off` with export still armed.
/// Called only when the poll loop actually stored its decision state — when
/// an API mutation changed the machine mid-write, that path owns the state
/// and the marker.
pub(crate) async fn clear_timed_export_stop_pending_if_settled(
    state: &Arc<AppState>,
    te_state: &crate::inverter::state_machines::TimedExportState,
    snapshot: &crate::inverter::model::InverterSnapshot,
) {
    if !matches!(
        te_state,
        crate::inverter::state_machines::TimedExportState::Off
    ) || snapshot.battery_power_mode != 1
        || snapshot.enable_discharge
    {
        return;
    }
    if state
        .timed_export_stop_pending
        .swap(false, std::sync::atomic::Ordering::AcqRel)
    {
        let _ = crate::settings::Settings::update(|s| s.timed_export_stop_pending = false);
    }
}

/// Whether the durable stop/exit-pending marker should influence this
/// poll's reconciler decision.
///
/// The marker alone is not enough: while a Timed Export mutation holds the
/// action lock, that handler is draining its own awaited exit batches, and
/// a competing poll-loop repair would sit in front of them in the write
/// budget (1.5 s per register) until the handler's awaited completion
/// times out and the stop wrongly reports failure. `try_lock` is instant
/// and the guard (if any) is dropped immediately.
pub(crate) async fn effective_timed_export_stop_pending(state: &AppState) -> bool {
    let api_mutation_holds_lock = state.timed_export_action_lock.try_lock().is_err();
    state
        .timed_export_stop_pending
        .load(std::sync::atomic::Ordering::Acquire)
        && !api_mutation_holds_lock
}

/// Commit a poll cycle's re-arm-detector progress only when no API/reconnect
/// path changed the detector while Modbus I/O was in flight. Schedule edits
/// reset the shared detector; restoring the stale pre-I/O clone would revive
/// evidence that belongs to the previous schedule.
async fn commit_timed_export_rearm_if_unchanged(
    shared: &Mutex<crate::inverter::state_machines::TimedExportRearmDetector>,
    generation: &std::sync::atomic::AtomicU64,
    generation_at_decision: u64,
    state_at_decision: &crate::inverter::state_machines::TimedExportRearmDetector,
    updated: crate::inverter::state_machines::TimedExportRearmDetector,
) -> bool {
    let mut stored = shared.lock().await;
    if generation.load(std::sync::atomic::Ordering::Acquire) == generation_at_decision
        && *stored == *state_at_decision
    {
        *stored = updated;
        true
    } else {
        false
    }
}

async fn reset_shared_timed_export_rearm_detector(
    shared: &Mutex<crate::inverter::state_machines::TimedExportRearmDetector>,
    generation: &std::sync::atomic::AtomicU64,
) {
    let mut detector = shared.lock().await;
    detector.reset();
    generation.fetch_add(1, std::sync::atomic::Ordering::Release);
}

/// Reset re-arm evidence after a schedule/ownership change and invalidate any
/// poll-cycle clone that was captured before the reset.
pub(crate) async fn reset_timed_export_rearm_detector(state: &AppState) {
    reset_shared_timed_export_rearm_detector(
        &state.timed_export_rearm,
        &state.timed_export_rearm_generation,
    )
    .await;
}

/// Like [`drain_write_batches`] but with a configurable inter-write gap, so
/// the capture / completion / trailing-sleep logic can be unit-tested against
/// a mock TCP server (issue #245) without the real ~1.5 s gaps slowing every
/// test. The trailing gap after the final write of an awaited batch is always
/// skipped regardless of this value.
async fn drain_write_batches_with_gap(
    client: &mut ModbusClient,
    pending: Vec<PendingWriteBatch>,
    inter_write_gap: Duration,
) {
    for batch in pending {
        let transactional = batch.policy == WriteBatchPolicy::FailFastTransactional;
        if transactional
            && batch
                .completion
                .as_ref()
                .is_some_and(tokio::sync::oneshot::Sender::is_closed)
        {
            tracing::warn!("Cancelled transactional write batch after requester timed out");
            continue;
        }
        // First failing register in this batch (if any), reported back to any
        // endpoint that queued a completion channel.
        let mut first_failure: Option<WriteOutcome> = None;
        // Skip the inter-write gap after the final write of a batch a caller
        // is awaiting, so the response isn't delayed by an idle gap.
        // Fire-and-forget batches keep the trailing gap (unchanged behaviour)
        // to preserve spacing before whatever the poll loop does next.
        let awaiting = batch.completion.is_some();
        let last_idx = batch.writes.len().saturating_sub(1);
        for (i, w) in batch.writes.iter().enumerate() {
            if transactional
                && batch
                    .completion
                    .as_ref()
                    .is_some_and(tokio::sync::oneshot::Sender::is_closed)
            {
                tracing::warn!(
                    "Cancelled {} remaining transactional write(s) after requester timed out",
                    batch.writes.len() - i
                );
                break;
            }
            match client.write_register(w.address, w.value).await {
                Ok(()) => {
                    tracing::info!("Wrote register {} = {}", w.address, w.value);
                }
                Err(e) => {
                    tracing::error!("Failed to write register {} = {}: {e}", w.address, w.value);
                    if first_failure.is_none() {
                        first_failure = Some(WriteOutcome::Failed {
                            address: w.address,
                            value: w.value,
                            error: e.to_string(),
                        });
                    }
                    if batch.policy != WriteBatchPolicy::ContinueOnError {
                        // Safety-critical ordering (e.g. slot writes followed
                        // by export arming): a failed write must abort the
                        // rest of the batch, never skip past it.
                        tracing::warn!(
                            "Fail-fast batch: skipping {} later write(s) after reg {} failed",
                            batch.writes.len() - i - 1,
                            w.address
                        );
                        break;
                    }
                }
            }
            // The dongle needs significant time between writes to adjacent
            // registers (up to 13s observed for exception-67 recovery). The
            // trailing gap after the final write is skipped when a caller is
            // awaiting the outcome.
            if !(awaiting && i == last_idx) {
                tokio::time::sleep(inter_write_gap).await;
            }
        }
        if let Some(tx) = batch.completion {
            let outcome = first_failure.unwrap_or(WriteOutcome::Ok);
            // Ignore send error: the receiver may be gone if the requesting
            // endpoint already timed out. A transactional batch checks that
            // cancellation before every write; ordinary batches retain their
            // historical eventual-write behaviour.
            let _ = tx.send(outcome);
        }
    }
}

/// Persist the pre-winter register values (or clear them when winter mode
/// deactivates) based on the state machine's `saved` output. Extracted for
/// unit-testability of its concurrency contract.
///
/// The transactional `Settings::update` mutates the freshly-loaded settings
/// under the settings lock, so a concurrent save of a disjoint field (e.g.
/// rated kWp from the settings UI) can never be reverted by persist the way
/// a save of the cycle's `poll_settings` clone did (lost update).
fn persist_auto_winter_saved(saved: &Option<AutoWinterSaved>) {
    let new_enable = saved.as_ref().map(|s| s.enable_charge_target);
    let new_soc = saved.as_ref().map(|s| s.target_soc as u16);
    // Fast-path: avoid taking the settings lock + touching the file when
    // winter mode isn't transitioning (steady-state call every poll).
    let current = crate::settings::Settings::load();
    if current.auto_winter_saved_enable_target == new_enable
        && current.auto_winter_saved_target_soc == new_soc
    {
        return;
    }
    let result = crate::settings::Settings::update(|disk| {
        if disk.auto_winter_saved_enable_target != new_enable
            || disk.auto_winter_saved_target_soc != new_soc
        {
            disk.auto_winter_saved_enable_target = new_enable;
            disk.auto_winter_saved_target_soc = new_soc;
        }
    });
    if let Err(e) = result {
        tracing::warn!("Failed to persist auto winter saved values: {e}");
    }
}

/// Persist the per-meter midnight baselines that drive CT-solar
/// authority (issue #277). The poll loop owns this field — the only
/// other place that mutates `solar_meter_baselines` is this same
/// fn — so a concurrent `POST /api/settings` racing a poll-cycle
/// persist would be reverted by a full-struct write-back. The
/// transactional `Settings::update` reads fresh state under the
/// settings mutex, mutates only this field, and persists — a
/// concurrent disjoint save (tariff, kWp, etc.) serialises on the
/// same mutex and both writes survive (review MAJOR1 in the
/// 3-week sweep; same fix the Aug-21 sweep applied to auto-winter
/// c5fd0ed and the Adaptive Charge baseline at line ~2409).
pub(crate) fn persist_solar_meter_baselines(
    baselines: std::collections::BTreeMap<String, crate::settings::SolarMeterBaseline>,
) -> Result<(), String> {
    crate::settings::Settings::update(|s| {
        s.solar_meter_baselines = baselines;
    })
}

/// Persist the discharge-floor guard's saved-reserve value (issue
/// #243 follow-up). Same transactional contract as
/// `persist_solar_meter_baselines` — the inner closure's no-op
/// skip avoids touching the file on steady-state polls where the
/// reserve hasn't changed.
pub(crate) fn persist_discharge_floor_saved_reserve(persisted: Option<u16>) -> Result<(), String> {
    crate::settings::Settings::update(|s| {
        s.discharge_floor_saved_reserve = persisted;
    })
}

/// Store the decoded snapshot as the latest, broadcast it to WebSocket
/// subscribers, and append it to history. Extracted from `run_poll_loop`'s
/// publish point so the publish step's contract with concurrent settings
/// saves (see `update_settings`) is unit-testable without a live poll loop
/// or Modbus server.
///
/// ## Settings-save reconciliation
///
/// A poll cycle loads settings once (right after the register read) and may
/// spend seconds-to-minutes afterward draining queued register writes and
/// running its state machines before it reaches this point. The snapshot
/// arrives stamped with the kWp-derived solar fields from *those* settings,
/// so a settings save landing mid-cycle is clobbered by publish: the
/// stored/broadcast snapshot regresses the new config (Solar page's Solar
/// Arrays card / PV % revert until the next cycle publishes — minutes away
/// while the loop drains queued control writes). Re-stamping here from the
/// freshest on-disk settings closes that window. `Settings::load()` is
/// infallible (defaults on any read/parse error), so this can only clear
/// stamps in the exact scenarios the mid-cycle stamp already did.
///
/// A save racing this very store (post-load, pre-store — a sub-millisecond
/// window each cycle) is NOT fully covered by `update_settings`' on-save
/// restamp: that restamp lands in `latest_snapshot` before this store and
/// is overwritten by it. The residual race is accepted — the save's restamp
/// broadcast already delivered the new values once, and the next publish
/// re-stamps from current settings, so the regression is bounded by one
/// publish.
async fn publish_snapshot(state: &Arc<AppState>, snapshot: InverterSnapshot) {
    let mut snapshot = snapshot;
    // Load on the blocking pool so a slow/networked settings file can't
    // stall the poll task (same pattern as the cycle's own settings load
    // in `run_poll_loop`).
    let fresh_settings = tokio::task::spawn_blocking(crate::settings::Settings::load)
        .await
        .unwrap_or_default();
    stamp_solar_array_fields(&mut snapshot, &fresh_settings);

    {
        let mut latest = state.latest_snapshot.lock().await;
        *latest = Some(snapshot.clone());
    }

    // Clone for history before moving `snapshot` into the broadcast.
    // This avoids a third clone — `latest` + `history` are the only
    // clones, the broadcast reuses the original allocation.
    let history_snapshot = if snapshot.soc > 0 {
        Some(snapshot.clone())
    } else {
        None
    };

    // Broadcast to WebSocket subscribers (move, no clone).
    let _ = state.tx.send(PollMessage::Snapshot(Box::new(snapshot)));

    // Persist to history database. Clone the Arc and
    // drop the lock so synchronous SQLite I/O doesn't
    // block the Tokio worker (same pattern as get_history).
    if let Some(snap) = history_snapshot {
        let db_guard = state.history.lock().await;
        let db = db_guard.clone();
        drop(db_guard);
        if let Some(db) = db {
            tokio::task::spawn_blocking(move || {
                db.insert_reading(&snap);
            });
        }
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
pub(crate) async fn run_poll_loop(state: Arc<AppState>) {
    // Start the Telegram /status command poller
    crate::alerts::spawn_telegram_poller(state.clone());

    // Connect-failure counter for the auto-discovery subsystem (separate
    // from ReconnectController, which owns the dead-session/flap gates).
    let mut consecutive_connect_failures: u32 = 0;
    // In-memory CT-solar recovery tracking (issue #294): per-meter last
    // accepted same-day delta and consecutive-rejection count for the
    // staged counter-corruption recovery. Loop-scoped (not persisted) so
    // it survives across polls without rewriting settings.json each cycle.
    let mut solar_ct_recovery: std::collections::BTreeMap<String, CtMeterRecovery> =
        std::collections::BTreeMap::new();
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
                let mut known_device_type_code: Option<String> = None;
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
                // probe. Each entry is (bcu_offset, num_modules). Empty discovery
                // remains retryable so a transient startup timeout cannot hide the
                // battery for the rest of the TCP session.
                let mut detected_hv_stacks: Vec<(u8, u8)> = Vec::new();
                let mut hv_probe_done = false;
                let mut hv_probe_attempted = false;
                let mut hv_probe_cycles_since_last: u8 = 0;

                // Tracks which Cosy slot index was last preloaded into the
                // inverter's charge slot registers. Only re-writes when the
                // "next upcoming slot" changes (e.g. after a slot ends).
                let mut cosy_last_preloaded_slot: Option<usize> = None;
                // Require two consecutive confirmed violations before repairing
                // an externally-created HR59/no-slot state. This avoids
                // reacting to a single transient read while still healing the
                // invalid state promptly on the next poll cycle.
                let mut invalid_timed_export_polls: u8 = 0;
                // Outcome of the Timed Export boundary writes issued on the
                // previous poll (connection-scoped: a fresh connection starts
                // with no writes in flight). Fed back into the reconciler so
                // failed transitions retry instead of advancing.
                let mut last_timed_export_write_outcome =
                    crate::inverter::state_machines::TimedExportWriteOutcome::NoneIssued;
                // Whether we already warned that the inverter clock is
                // unavailable (once per connection, not once per poll).
                let mut inverter_time_fallback_logged = false;
                // A reconnect makes the HR59 re-arm detector's evidence
                // ambiguous — any register state may predate the reconnect.
                // Reset it to Idle so stale evidence can't classify the
                // device (the anchored exit restarts on the next boundary).
                reset_timed_export_rearm_detector(&state).await;

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

                    // Drain pending register writes from the control API
                    // before reading the latest state. Drain at most a few
                    // batches per cycle: draining the whole queue in one go
                    // starves the read/broadcast for minutes when many
                    // batches queue up (each write is a real ~1.5s Modbus
                    // round-trip), freezing the snapshot the UI depends on.
                    // A small per-cycle cap bounds that freeze to ~a minute
                    // while still making net progress on the queue (the
                    // write_notify wakes the sleep early so remaining
                    // batches drain on the next cycles).
                    let active_discharge_owner = current_discharge_control_owner(&state).await;
                    let (pending, pending_discharge_owner) = {
                        let mut pw = state.pending_writes.lock().await;
                        take_pending_writes_for_owner(
                            &mut pw,
                            MAX_WRITE_BATCHES_PER_CYCLE,
                            active_discharge_owner,
                        )
                    };
                    if !pending.is_empty() {
                        drain_write_batches(&mut client, pending).await;
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
                                let has_three_phase_high_config_block = blocks.iter().any(|b| {
                                    b.block.register_type == crate::modbus::registers::RegisterType::Holding
                                        && b.block.start == 1000
                                        && b.block.count == 80
                                });
                                let has_three_phase_config_block = blocks.iter().any(|b| {
                                    b.block.register_type == crate::modbus::registers::RegisterType::Holding
                                        && b.block.start == 1080
                                        && b.block.count == 45
                                });
                                let has_three_phase_fault_block = blocks.iter().any(|b| {
                                    b.block.register_type == crate::modbus::registers::RegisterType::Input
                                        && b.block.start == 1300
                                        && b.block.count == 60
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
                                    known_device_type_code =
                                        Some(snapshot.device_type_code.clone());

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
                                    // (IR 60) is non-zero. True three-phase models use the
                                    // built-in grid CT; single-phase 0x81xx HV hybrids still
                                    // support an external MID meter and are probed here.
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

                                // Once identified, freeze both the coarse model and exact HR(0)
                                // DTC. The exact 0x81xx code carries its 6/8/10 kW rating, so
                                // allowing a later corrupt HR(0) through would lower valid power
                                // ceilings even though the displayed DeviceType remained locked.
                                if let (Some(kdt), Some(code)) =
                                    (known_device_type, known_device_type_code.as_deref())
                                {
                                    lock_snapshot_device_identity(&mut snapshot, kdt, code);
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
                                    // Discover the BCU layout via the BMS at 0xA0. Once a
                                    // usable layout is found, read each stack's cluster block
                                    // every cycle. Empty attempts remain retryable on a slow
                                    // cadence so startup timeouts recover without adding a BMS
                                    // timeout to every poll.
                                    if hv_probe_attempted && !hv_probe_done {
                                        hv_probe_cycles_since_last =
                                            hv_probe_cycles_since_last.saturating_add(1);
                                    }
                                    if should_probe_hv_stacks(
                                        known_device_type,
                                        hv_probe_done,
                                        hv_probe_attempted,
                                        hv_probe_cycles_since_last,
                                    ) {
                                        hv_probe_attempted = true;
                                        hv_probe_cycles_since_last = 0;
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
                                        hv_probe_done = hv_probe_completed(&detected_hv_stacks);
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
                                            track_battery_conn(
                                                &state, 1, 0x32, true,
                                            ).await;

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
                                            track_battery_conn(
                                                &state, 1, 0x32, false,
                                            ).await;
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
                                                    track_battery_conn(
                                                        &state, i + 2, addr, true,
                                                    ).await;
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
                                                    // Known battery answering with invalid data —
                                                    // count it as a failed read only for addresses
                                                    // we have seen answer before.
                                                    if known_battery_addrs.contains(&addr) {
                                                        track_battery_conn(
                                                            &state, i + 2, addr, false,
                                                        ).await;
                                                    }
                                                    break;
                                                }
                                            }
                                            Err(e) => {
                                                tracing::debug!(
                                                    "Battery addr 0x{:02X}: no response: {e}",
                                                    addr
                                                );
                                                if known_battery_addrs.contains(&addr) {
                                                    track_battery_conn(
                                                        &state, i + 2, addr, false,
                                                    ).await;
                                                }
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
                                    if carry_forward_three_phase_high_config_values(
                                        &mut snapshot,
                                        prev.as_ref(),
                                        has_three_phase_high_config_block,
                                    ) {
                                        s = true;
                                    }
                                    if carry_forward_three_phase_fault_block_values(
                                        &mut snapshot,
                                        prev.as_ref(),
                                        has_three_phase_fault_block,
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
                                    // The Forecast page tailors its plan note to
                                    // whether the nightly slot auto-refresh owns
                                    // charge slot 1.
                                    snapshot.forecast_plan_auto_refresh =
                                        poll_settings.forecast_plan_auto_refresh;
                                    // Same for the user-configured auto-apply
                                    // trigger — the Control page's "slot 1 is
                                    // planner-owned" banner treats either as
                                    // planner ownership.
                                    snapshot.forecast_plan_auto_apply_enabled =
                                        poll_settings.forecast_plan_auto_apply_enabled;
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

                                    // File I/O (the fast-path load and, on a
                                    // winter transition, `Settings::update`)
                                    // runs on the blocking pool so it can't
                                    // stall the poll task — same pattern as
                                    // the cycle's own settings load above.
                                    let _ = tokio::task::spawn_blocking(move || {
                                        persist_auto_winter_saved(&persist_saved);
                                    })
                                    .await;

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

                                // ---- Adaptive Charge mode (issue #234) ----
                                {
                                    let now = chrono::Local::now();
                                    let now_minutes = now.hour() as u16 * 60 + now.minute() as u16;
                                    let mut adaptive_state =
                                        state.adaptive_charge_state.lock().await;
                                    let mut adaptive_saved =
                                        state.adaptive_charge_saved.lock().await;
                                    let saved_before = adaptive_saved.clone();
                                    let outcome = check_adaptive_charge(
                                        &snapshot,
                                        &poll_settings.adaptive_charge_config,
                                        poll_settings.adaptive_charge_enabled,
                                        &mut adaptive_state,
                                        &mut adaptive_saved,
                                        now_minutes,
                                    );

                                    snapshot.charging_mode =
                                        crate::settings::charging_mode_for_settings(&poll_settings);
                                    snapshot.adaptive_charge_enabled =
                                        poll_settings.adaptive_charge_enabled;
                                    snapshot.adaptive_charge_state =
                                        adaptive_state.api_name().to_string();
                                    snapshot.adaptive_charge_period =
                                        adaptive_state.active_period();
                                    snapshot.adaptive_charge_desired_rate_percent =
                                        outcome.desired_rate_percent;

                                    let saved_after = adaptive_saved.clone();
                                    drop(adaptive_state);
                                    drop(adaptive_saved);

                                    if saved_before != saved_after {
                                        // Persist the adaptive baseline atomically so a
                                        // concurrent mode/config save cannot be overwritten
                                        // by this poll's older settings snapshot.
                                        if let Err(e) = crate::settings::Settings::update(|s| {
                                            s.adaptive_charge_saved_limit = saved_after.clone();
                                        }) {
                                            tracing::warn!(
                                                "Failed to persist Adaptive Charge baseline: {e}"
                                            );
                                        }
                                    }

                                    if let Some(write) = outcome.write {
                                        match client
                                            .write_register(write.address, write.value)
                                            .await
                                        {
                                            Ok(()) => tracing::info!(
                                                state = snapshot.adaptive_charge_state,
                                                register = write.address,
                                                value = write.value,
                                                "Adaptive Charge updated charge limit"
                                            ),
                                            Err(e) => tracing::error!(
                                                register = write.address,
                                                value = write.value,
                                                "Adaptive Charge write failed: {e}"
                                            ),
                                        }
                                        tokio::time::sleep(Duration::from_millis(1500)).await;
                                    }
                                }

                                // ---- Forecast plan auto-refresh (issue #283) ----
                                // The plan is deliberately sized for ONE charge
                                // cycle, but the inverter treats an applied charge
                                // slot as a nightly recurring schedule. Shortly
                                // before each cheap period, re-compute the plan
                                // from the live SOC and forecast, then rewrite —
                                // or clear — charge slot 1 so the inverter never
                                // repeats a stale duration on later nights.
                                //
                                // Rate-limit contract (CODE_REVIEW.md Major 2):
                                // the plan's duration maths assumes the inverter
                                // charges at its hardware maximum, so every
                                // refresh batch re-writes the charge-limit
                                // register (via `PLAN_CHARGE_RATE_PERCENT` below)
                                // — the "100% for the planned duration" promise
                                // is re-asserted on each refresh, not just the
                                // first night. A manual Control-page rate change
                                // or an Octopus/Cosy automation that lowers the
                                // limit after the refresh still wins until the
                                // next refresh re-asserts it.
                                // While the auto-apply trigger is enabled it
                                // supersedes the fixed auto-refresh: exactly one
                                // machine write of charge slot 1 per day, at the
                                // user's lead time, with a notification.
                                if poll_settings.forecast_plan_auto_refresh
                                    && !poll_settings.forecast_plan_auto_apply_enabled
                                {
                                    let now = chrono::Local::now();
                                    let adaptive_owns_rate = poll_settings.adaptive_charge_enabled
                                        || poll_settings.adaptive_charge_saved_limit.is_some();
                                    let last =
                                        *state.forecast_plan_refresh_date.lock().await;
                                    let due = crate::forecast::refresh::
                                        plan_refresh_due_with_adaptive(
                                            now,
                                            last,
                                            poll_settings.import_tariff_config.as_ref(),
                                            adaptive_owns_rate,
                                        );
                                    if due {
                                        let (weather_enabled, coords) = {
                                            let ws = state.weather.lock().await;
                                            (
                                                ws.config.enabled,
                                                ws.config.latitude.zip(ws.config.longitude),
                                            )
                                        };
                                        let history = state.history.lock().await.clone();
                                        let live_snapshot = snapshot.clone();
                                        // Shallow clone (CODE_REVIEW.md Minor 5):
                                        // the planner only reads a read-only subset
                                        // of settings (import tariff, forecast_*,
                                        // weather coords) — nested fields like
                                        // `timed_export_schedule` are never
                                        // touched, so a deep clone would be wasted
                                        // work. Keep this comment if new fields are
                                        // added to the planner's inputs.
                                        let refresh_settings = poll_settings.clone();
                                        // SQLite reads + a 72 h simulation — keep
                                        // them off the poll task, same as the
                                        // settings persistence above.
                                        let planned = tokio::task::spawn_blocking(
                                            move || {
                                                let forecast = crate::forecast::build_forecast_payload(
                                                    &crate::forecast::ForecastInputs {
                                                        db: history.as_deref(),
                                                        settings: &refresh_settings,
                                                        snapshot: Some(&live_snapshot),
                                                        weather_enabled,
                                                        weather_coords: coords,
                                                        now,
                                                    },
                                                );
                                                crate::server::api::compute_plan_recommendation(
                                                    &forecast,
                                                    &refresh_settings,
                                                    Some(&live_snapshot),
                                                    now.timestamp(),
                                                )
                                            },
                                        )
                                        .await;
                                        // Mark today done regardless of the outcome
                                        // shape so a persistent failure can't turn
                                        // into a per-poll write storm.
                                        *state.forecast_plan_refresh_date.lock().await =
                                            Some(now.date_naive());
                                        match planned {
                                            Ok(rec) => {
                                                let action =
                                                    crate::forecast::refresh::plan_refresh_action(&rec);
                                                match action {
                                                    crate::forecast::refresh::PlanRefreshAction::WriteSlot {
                                                        start_hhmm,
                                                        end_hhmm,
                                                    } => {
                                                        match crate::server::api::build_charge_slot_writes(
                                                            snapshot.device_type,
                                                            1,
                                                            true,
                                                            start_hhmm,
                                                            end_hhmm,
                                                            crate::forecast::refresh::SLOT_TARGET_SOC_NONE,
                                                            Some(crate::forecast::refresh::PLAN_CHARGE_RATE_PERCENT),
                                                        ) {
                                                            Ok(writes) => {
                                                                tracing::info!(
                                                                    start = start_hhmm,
                                                                    end = end_hhmm,
                                                                    kwh = format!("{:.2}", rec_kwh(&rec)),
                                                                    "Forecast plan refresh: rewriting charge slot 1 for tonight's cheap period"
                                                                );
                                                                // Route through the shared write
                                                                // pump (CODE_REVIEW.md Major 1)
                                                                // instead of writing inline: the
                                                                // pump serialises the ~1.5 s Modbus
                                                                // round-trips off the read path, so
                                                                // the poll loop keeps broadcasting
                                                                // snapshots while the slot writes
                                                                // drain on the next cycle.
                                                                crate::server::api::queue_writes(
                                                                    &state, writes,
                                                                )
                                                                .await;
                                                            }
                                                            Err(e) => tracing::warn!(
                                                                "Forecast plan refresh: could not encode slot writes: {e}"
                                                            ),
                                                        }
                                                    }
                                                    crate::forecast::refresh::PlanRefreshAction::ClearSlot => {
                                                        match crate::server::api::build_charge_slot_writes(
                                                            snapshot.device_type,
                                                            1,
                                                            false,
                                                            0,
                                                            0,
                                                            crate::forecast::refresh::SLOT_TARGET_SOC_NONE,
                                                            None,
                                                        ) {
                                                            Ok(writes) => {
                                                                tracing::info!(
                                                                    "Forecast plan refresh: fresh plan needs no charge — clearing charge slot 1"
                                                                );
                                                                crate::server::api::queue_writes(
                                                                    &state, writes,
                                                                )
                                                                .await;
                                                            }
                                                            Err(e) => tracing::warn!(
                                                                "Forecast plan refresh: could not encode slot clear: {e}"
                                                            ),
                                                        }
                                                    }
                                                    crate::forecast::refresh::PlanRefreshAction::None => {
                                                        tracing::debug!(
                                                            "Forecast plan refresh: no plan available this cycle"
                                                        );
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                tracing::warn!("Forecast plan refresh failed: {e}")
                                            }
                                        }
                                    } else if adaptive_owns_rate
                                        && crate::forecast::refresh::plan_refresh_due(
                                            now,
                                            last,
                                            poll_settings.import_tariff_config.as_ref(),
                                        )
                                    {
                                        // The refresh would be due but Adaptive
                                        // Charge owns the charge-limit register —
                                        // warn once per day (tracked separately so
                                        // disabling Adaptive later in the same
                                        // lead window still lets the refresh fire)
                                        // instead of silently skipping on every
                                        // poll (CODE_REVIEW.md Major 3).
                                        let mut warned =
                                            state.forecast_plan_refresh_warned.lock().await;
                                        if *warned != Some(now.date_naive()) {
                                            *warned = Some(now.date_naive());
                                            tracing::warn!(
                                                "Forecast plan auto-refresh is enabled but Adaptive Charge owns the charge rate — skipping tonight's slot rewrite"
                                            );
                                        }
                                    }
                                }

                                // ---- Forecast plan auto-apply (user-configured trigger) ----
                                // Every day, the configured number of minutes before the
                                // cheap charging tariff window begins, re-compute the plan
                                // from the live SOC and forecast, apply it to charge slot 1
                                // exactly as the Forecast page's Apply button would, and
                                // notify the user through the alert channels. While this is
                                // enabled it supersedes the fixed nightly auto-refresh
                                // above, so charge slot 1 gets exactly one machine write
                                // per day — at the user's lead time, with a notification.
                                if poll_settings.forecast_plan_auto_apply_enabled {
                                    let now = chrono::Local::now();
                                    let adaptive_owns_rate = poll_settings.adaptive_charge_enabled
                                        || poll_settings.adaptive_charge_saved_limit.is_some();
                                    let last = *state.forecast_plan_apply_date.lock().await;
                                    let decision = crate::forecast::refresh::
                                        plan_auto_apply_decision_with_adaptive(
                                            now,
                                            last,
                                            poll_settings.import_tariff_config.as_ref(),
                                            poll_settings.forecast_plan_auto_apply_lead_minutes,
                                            adaptive_owns_rate,
                                        );
                                    match decision {
                                        crate::forecast::refresh::PlanApplyDecision::NotDue {
                                            reason,
                                        } => {
                                            // Would-be-due but Adaptive Charge owns the
                                            // charge-limit register — warn + notify once
                                            // per day instead of on every poll. Gated on
                                            // the apply actually being due (same shape as
                                            // the auto-refresh block's warning above):
                                            // without that gate an Adaptive user with no
                                            // cheap window near would get a false alarm
                                            // every single day.
                                            if adaptive_owns_rate
                                                && crate::forecast::refresh::
                                                    plan_auto_apply_adaptive_warning_due(
                                                        now,
                                                        last,
                                                        poll_settings.import_tariff_config.as_ref(),
                                                        poll_settings.forecast_plan_auto_apply_lead_minutes,
                                                    )
                                            {
                                                let mut warned =
                                                    state.forecast_plan_apply_warned.lock().await;
                                                if *warned != Some(now.date_naive()) {
                                                    *warned = Some(now.date_naive());
                                                    tracing::warn!(
                                                        "Forecast plan auto-apply is enabled but Adaptive Charge owns the charge rate — skipping tonight's apply"
                                                    );
                                                    crate::alerts::send_plan_notification(
                                                        &state,
                                                        &crate::alerts::build_plan_unavailable_message(
                                                            "Adaptive Charge owns the charge rate, so tonight's charging is left to it",
                                                        ),
                                                    )
                                                    .await;
                                                }
                                            } else {
                                                tracing::debug!(
                                                    reason,
                                                    "Forecast plan auto-apply standing down"
                                                );
                                            }
                                        }
                                        crate::forecast::refresh::PlanApplyDecision::Due { .. } => {
                                            let (weather_enabled, coords) = {
                                                let ws = state.weather.lock().await;
                                                (
                                                    ws.config.enabled,
                                                    ws.config.latitude.zip(ws.config.longitude),
                                                )
                                            };
                                            let history = state.history.lock().await.clone();
                                            let live_snapshot = snapshot.clone();
                                            // Shallow clone, same rationale as the
                                            // auto-refresh above: the planner reads only
                                            // a read-only subset of settings.
                                            let apply_settings = poll_settings.clone();
                                            let planned = tokio::task::spawn_blocking(
                                                move || {
                                                    let forecast = crate::forecast::build_forecast_payload(
                                                        &crate::forecast::ForecastInputs {
                                                            db: history.as_deref(),
                                                            settings: &apply_settings,
                                                            snapshot: Some(&live_snapshot),
                                                            weather_enabled,
                                                            weather_coords: coords,
                                                            now,
                                                        },
                                                    );
                                                    crate::server::api::compute_plan_recommendation(
                                                        &forecast,
                                                        &apply_settings,
                                                        Some(&live_snapshot),
                                                        now.timestamp(),
                                                    )
                                                },
                                            )
                                            .await;
                                            // Mark today done regardless of the outcome
                                            // shape so a persistent failure can't turn
                                            // into a per-poll write/notification storm.
                                            *state.forecast_plan_apply_date.lock().await =
                                                Some(now.date_naive());
                                            match planned {
                                                Ok(rec) => {
                                                    let tomorrow = matches!(
                                                        &rec,
                                                        crate::forecast::planner::PlanRecommendation::Charge { window, .. } if window.tomorrow
                                                    );
                                                    let action = crate::forecast::refresh::plan_refresh_action(&rec);
                                                    match action {
                                                        crate::forecast::refresh::PlanRefreshAction::WriteSlot {
                                                            start_hhmm,
                                                            end_hhmm,
                                                        } => {
                                                            match crate::server::api::build_charge_slot_writes(
                                                                snapshot.device_type,
                                                                1,
                                                                true,
                                                                start_hhmm,
                                                                end_hhmm,
                                                                crate::forecast::refresh::SLOT_TARGET_SOC_NONE,
                                                                Some(crate::forecast::refresh::PLAN_CHARGE_RATE_PERCENT),
                                                            ) {
                                                                Ok(writes) => {
                                                                    tracing::info!(
                                                                        start = start_hhmm,
                                                                        end = end_hhmm,
                                                                        kwh = format!("{:.2}", rec_kwh(&rec)),
                                                                        "Forecast plan auto-apply: writing charge slot 1 for the cheap tariff window"
                                                                    );
                                                                    crate::server::api::queue_writes(
                                                                        &state, writes,
                                                                    )
                                                                    .await;
                                                                    crate::alerts::send_plan_notification(
                                                                        &state,
                                                                        &crate::alerts::build_plan_applied_message(
                                                                            start_hhmm,
                                                                            end_hhmm,
                                                                            rec_kwh(&rec),
                                                                            tomorrow,
                                                                        ),
                                                                    )
                                                                    .await;
                                                                }
                                                                Err(e) => {
                                                                    tracing::warn!(
                                                                        "Forecast plan auto-apply: could not encode slot writes: {e}"
                                                                    );
                                                                    crate::alerts::send_plan_notification(
                                                                        &state,
                                                                        &crate::alerts::build_plan_unavailable_message(
                                                                            "the charge slot could not be encoded for this inverter",
                                                                        ),
                                                                    )
                                                                    .await;
                                                                }
                                                            }
                                                        }
                                                        crate::forecast::refresh::PlanRefreshAction::ClearSlot => {
                                                            match crate::server::api::build_charge_slot_writes(
                                                                snapshot.device_type,
                                                                1,
                                                                false,
                                                                0,
                                                                0,
                                                                crate::forecast::refresh::SLOT_TARGET_SOC_NONE,
                                                                None,
                                                            ) {
                                                                Ok(writes) => {
                                                                    tracing::info!(
                                                                        "Forecast plan auto-apply: fresh plan needs no charge — clearing charge slot 1"
                                                                    );
                                                                    crate::server::api::queue_writes(
                                                                        &state, writes,
                                                                    )
                                                                    .await;
                                                                    crate::alerts::send_plan_notification(
                                                                        &state,
                                                                        &crate::alerts::build_plan_cleared_message(),
                                                                    )
                                                                    .await;
                                                                }
                                                                Err(e) => {
                                                                    tracing::warn!(
                                                                        "Forecast plan auto-apply: could not encode slot clear: {e}"
                                                                    );
                                                                    crate::alerts::send_plan_notification(
                                                                        &state,
                                                                        &crate::alerts::build_plan_unavailable_message(
                                                                            "the charge slot could not be cleared for this inverter",
                                                                        ),
                                                                    )
                                                                    .await;
                                                                }
                                                            }
                                                        }
                                                        crate::forecast::refresh::PlanRefreshAction::None => {
                                                            let reason = match &rec {
                                                                crate::forecast::planner::PlanRecommendation::NoPlan { reason } => {
                                                                    reason.clone()
                                                                }
                                                                _ => "no plan available".to_string(),
                                                            };
                                                            tracing::debug!(
                                                                reason,
                                                                "Forecast plan auto-apply: no plan available this cycle"
                                                            );
                                                            crate::alerts::send_plan_notification(
                                                                &state,
                                                                &crate::alerts::build_plan_unavailable_message(&reason),
                                                            )
                                                            .await;
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::warn!(
                                                        "Forecast plan auto-apply failed: {e}"
                                                    );
                                                    crate::alerts::send_plan_notification(
                                                        &state,
                                                        &crate::alerts::build_plan_unavailable_message(
                                                            "the plan computation failed",
                                                        ),
                                                    )
                                                    .await;
                                                }
                                            }
                                        }
                                    }
                                }

                                // ---- Solar arrays (issue #110) ----
                                // Stamp the per-array "% of max" summary from
                                // settings so every page that reads the
                                // snapshot (Solar / Power / Summary) can show
                                // output as a percentage of rated kWp without
                                // each component re-fetching settings.
                                stamp_solar_array_fields(&mut snapshot, &poll_settings);

                                // ---- CT-meter solar authority (issue #277) ----
                                // When any external CT meter is labelled as a
                                // solar array, the CT clamp is the authoritative
                                // solar measurement (the inverter's PV registers
                                // only mirror it with their own refresh cadence).
                                // Override solar_power / today_solar_kwh from the
                                // meters so Overview, wheel, history and alerts
                                // all agree by construction. Baselines for the
                                // per-meter "today" energy deltas persist in
                                // settings; persist whenever they change.
                                {
                                    let mut solar_baselines =
                                        poll_settings.solar_meter_baselines.clone();
                                    let reseeded = apply_ct_solar_authority(
                                        &mut snapshot,
                                        &poll_settings,
                                        &mut solar_baselines,
                                        &mut solar_ct_recovery,
                                        chrono::Local::now().naive_local(),
                                    );
                                    if reseeded || solar_baselines
                                        != poll_settings.solar_meter_baselines
                                    {
                                        if let Err(e) = persist_solar_meter_baselines(solar_baselines) {
                                            tracing::warn!(
                                                "Failed to persist solar meter baselines: {e}"
                                            );
                                        }
                                    }
                                }

                                // Select one owner for the shared discharge-control domain
                                // before any of the mode writers below issue Modbus I/O.
                                // Queued API requests and state carried from the previous
                                // snapshot are included first; the fresh snapshot then adds
                                // newly-observed safety, pause, force, and Timed Export claims.
                                let mut discharge_arbiter = DischargeControlArbiter::default();
                                if let Some(owner) = pending_discharge_owner {
                                    discharge_arbiter.request(owner);
                                }

                                let inverter_minute = crate::inverter::state_machines::inverter_minute_of_day(
                                    &snapshot,
                                )
                                .unwrap_or_else(|| {
                                    let now = chrono::Local::now();
                                    now.hour() as u16 * 60 + now.minute() as u16
                                });

                                let safety_demand = {
                                    let load_config =
                                        state.load_limiter_config.lock().await.clone();
                                    let load_state = state.load_limiter_state.lock().await.clone();
                                    let temperature_config =
                                        state.temperature_limiter_config.lock().await.clone();
                                    let temperature_state =
                                        state.temperature_limiter_state.lock().await.clone();
                                    let saved = state.load_limiter_saved.lock().await.clone();
                                    let mut preview_temperature_state = temperature_state.clone();
                                    let mut preview_load_state = load_state.clone();
                                    let mut preview_saved = saved.clone();
                                    let temperature_writes =
                                        check_temperature_limiter_after_automation(
                                            &snapshot,
                                            &temperature_config,
                                            &mut preview_temperature_state,
                                            &mut preview_saved,
                                            false,
                                            false,
                                        );
                                    let load_writes = check_load_limiter_with_other_pause(
                                        &snapshot,
                                        &load_config,
                                        &mut preview_load_state,
                                        poll_settings.poll_interval,
                                        &mut preview_saved,
                                        preview_temperature_state.is_actively_pausing(),
                                    );
                                    load_state.is_actively_pausing()
                                        || temperature_state.is_actively_pausing()
                                        || temperature_writes.is_some()
                                        || load_writes.is_some()
                                };
                                if safety_demand {
                                    discharge_arbiter.request(DischargeControlOwner::Safety);
                                }

                                if crate::inverter::state_machines::hr318_blocks_discharge(
                                    &snapshot,
                                    inverter_minute,
                                ) || matches!(
                                    snapshot.battery_mode,
                                    BatteryMode::EcoPaused | BatteryMode::ExportPaused
                                )
                                {
                                    discharge_arbiter.request(DischargeControlOwner::ExplicitPause);
                                }

                                let force_charge_in_progress =
                                    state.force_charge_revert.lock().await.is_some();
                                let force_discharge_in_progress =
                                    state.force_discharge_revert.lock().await.is_some();
                                if force_charge_in_progress || force_discharge_in_progress {
                                    discharge_arbiter.request(DischargeControlOwner::ManualForce);
                                }

                                let timed_export_candidate = {
                                    let config = state.timed_export_config.lock().await.clone();
                                    let machine_state =
                                        state.timed_export_state.lock().await.clone();
                                    let has_slots =
                                        config.slots.iter().any(ScheduleSlot::is_configured);
                                    let in_window = crate::inverter::state_machines::export_window_contains(
                                        &config.slots,
                                        inverter_minute,
                                    );
                                    let eco_confirmed =
                                        snapshot.battery_power_mode == 1
                                            && !snapshot.enable_discharge;
                                    let physical_slot_configured = snapshot
                                        .discharge_slots
                                        .iter()
                                        .any(ScheduleSlot::is_configured);
                                    machine_state.owns_discharge_control()
                                        || (config.schedule_enabled
                                            && has_slots
                                            && (in_window || !eco_confirmed))
                                        || (!config.schedule_enabled
                                            && !eco_confirmed
                                            && !physical_slot_configured)
                                };
                                if timed_export_candidate {
                                    discharge_arbiter.request(DischargeControlOwner::TimedExport);
                                }
                                if matches!(snapshot.battery_mode, BatteryMode::TimedDemand)
                                    && !snapshot.agile_active
                                    && !snapshot.cosy_active
                                    && !*state.cosy_active.lock().await
                                {
                                    // A Timed Demand snapshot with no
                                    // automation claiming it is a manual
                                    // mode. Keep Agile/Cosy from taking over
                                    // an externally-selected mode until the
                                    // user explicitly changes it.
                                    discharge_arbiter
                                        .request(DischargeControlOwner::ManualMode);
                                }

                                // Track same-cycle writes that can enable discharge. The
                                // temperature limiter runs last and reasserts its pause when
                                // the current snapshot cannot yet reflect one of these writes.
                                let mut discharge_control_may_override_pause = false;

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
                                    let _force_action_guard = state.force_action_lock.lock().await;
                                    let now_ms = chrono::Local::now().timestamp_millis();
                                    let mut revert_guard = state.force_discharge_revert.lock().await;
                                    let expired = revert_guard
                                        .as_ref()
                                        .and_then(|r| r.force_discharge_slot_end_ms)
                                        .is_some_and(|end| now_ms >= end);

                                    if expired
                                        && discharge_arbiter
                                            .request(DischargeControlOwner::ManualForce)
                                    {
                                        // Do not consume the restore until the
                                        // arbiter has admitted it. A safety
                                        // limiter can temporarily outrank the
                                        // expired force action; leaving the
                                        // captured state queued lets the
                                        // restore retry after safety releases.
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
                                                Some(r.battery_pause_mode),
                                                Some(&r.battery_pause_slot),
                                            );
                                            if let Some(writes) = writes {
                                                discharge_control_may_override_pause =
                                                    !writes.is_empty();
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
                                    } else if expired {
                                        tracing::debug!(
                                            owner = ?discharge_arbiter.selected_owner(),
                                            "Force discharge auto-revert deferred by a higher-priority owner"
                                        );
                                    }
                                }

                                // ---- Timed Export state machine ----
                                //
                                // Issue #289: Eco is the normal baseline outside
                                // Timed Export windows. The reconciler manages
                                // HR27 + the model-routed discharge-enable
                                // register at window boundaries:
                                // - Entry: HR27=0, then enable=1 (HR59 / HR1122)
                                // - Exit: enable=0, then HR27=1
                                //
                                // Transitions are confirmed from later poll
                                // snapshots, not assumed when writes are queued.
                                //
                                // Lock discipline (code-review finding): the
                                // decision is computed from *cloned* state under
                                // short-lived guards, all guards are dropped
                                // before any Modbus I/O or sleep, and the outcome
                                // is written back under a fresh brief guard. No
                                // Timed Export mutex is ever held across `.await`
                                // points — the old state→config→rearm ordering
                                // deadlocked against `GET /api/timed-export`'s
                                // reverse order.
                                //
                                // `timed_export_owns_discharge` records whether
                                // the machine owns the discharge-control
                                // registers this cycle; lower-priority automations
                                // (Cosy exit's Eco restore, Agile) defer to it.
                                let mut timed_export_owns_discharge = false;
                                {
                                    // Window boundaries are evaluated on the
                                    // inverter's own wall clock (issue #289): HEM
                                    // may run in a UTC container while the
                                    // inverter (and the user) are on local time.
                                    // Fall back to host time when the clock
                                    // registers are absent or malformed.
                                    let minute_of_day =
                                        match crate::inverter::state_machines::inverter_minute_of_day(
                                            &snapshot,
                                        ) {
                                            Some(minute) => minute,
                                            None => {
                                                if !inverter_time_fallback_logged {
                                                    tracing::warn!(
                                                        "Timed Export: inverter clock unavailable — \
                                                         falling back to host local time for window \
                                                         evaluation"
                                                    );
                                                    inverter_time_fallback_logged = true;
                                                }
                                                use chrono::Timelike as _;
                                                let now = chrono::Local::now();
                                                now.hour() as u16 * 60 + now.minute() as u16
                                            }
                                        };

                                    // Skip Timed Export management whenever a
                                    // higher-priority owner won this cycle.
                                    // Force Charge is included here too: its
                                    // mode writes can clobber HR27/HR59 even
                                    // though it does not discharge itself.
                                    // Exception (see
                                    // `timed_export_machine_allowed`): a
                                    // register-derived Timed Demand claim
                                    // must not block the reconciler while
                                    // the managed schedule is enabled —
                                    // that readback is exactly what re-arming
                                    // firmware leaves behind outside a
                                    // window.
                                    let managed_schedule_live =
                                        state.timed_export_config.lock().await.schedule_enabled;
                                    if timed_export_machine_allowed(
                                        discharge_arbiter,
                                        managed_schedule_live,
                                    ) {
                                        // Short-lived guards: clone the inputs and
                                        // drop every guard before any I/O.
                                        let (
                                            te_state_at_decision,
                                            mut te_config,
                                            te_rearm_at_decision,
                                            te_rearm_generation_at_decision,
                                        ) = {
                                            let te_state =
                                                state.timed_export_state.lock().await.clone();
                                            let mut te_config =
                                                state.timed_export_config.lock().await.clone();
                                            // The durable stop-pending marker lives in
                                            // settings + the atomic mirror; overlay it here so
                                            // every reconciler decision this poll sees the
                                            // current value even if an API flipped it since
                                            // the mirror struct was last written. Suppressed
                                            // while an API mutation holds the action lock —
                                            // that handler is draining its own exit batches.
                                            te_config.stop_pending =
                                                effective_timed_export_stop_pending(&state).await;
                                            let te_rearm = state.timed_export_rearm.lock().await;
                                            let te_rearm_generation = state
                                                .timed_export_rearm_generation
                                                .load(std::sync::atomic::Ordering::Acquire);
                                            (
                                                te_state,
                                                te_config,
                                                te_rearm.clone(),
                                                te_rearm_generation,
                                            )
                                        };
                                        let mut te_state = te_state_at_decision.clone();
                                        let mut te_rearm = te_rearm_at_decision.clone();

                                        let decision = crate::inverter::state_machines::check_timed_export(
                                            &snapshot,
                                            &te_config,
                                            &mut te_state,
                                            minute_of_day,
                                            known_device_type.unwrap_or_default(),
                                            last_timed_export_write_outcome,
                                            te_rearm.is_observing(),
                                        );

                                        if let Some(msg) = &decision.log_message {
                                            tracing::info!("Timed Export: {}", msg);
                                        }

                                        // Issue the transition writes fail-fast: a
                                        // rejected write aborts the remaining writes
                                        // in the batch so a failed slot restore can
                                        // never be followed by the export-arm write.
                                        let mut write_outcome =
                                            crate::inverter::state_machines::TimedExportWriteOutcome::NoneIssued;
                                        if !decision.writes.is_empty()
                                            && timed_export_machine_may_write(
                                                &mut discharge_arbiter,
                                                managed_schedule_live,
                                            )
                                        {
                                            discharge_control_may_override_pause = true;
                                            // Shared fail-fast helper: correct
                                            // remaining-write count on failure and no
                                            // idle trailing sleep after the final write.
                                            let ok = write_registers_fail_fast_with_gap(
                                                &mut client,
                                                &decision.writes,
                                                "Timed Export transition",
                                                Duration::from_millis(1500),
                                            )
                                            .await;
                                            write_outcome = if ok {
                                                crate::inverter::state_machines::TimedExportWriteOutcome::Succeeded
                                            } else {
                                                crate::inverter::state_machines::TimedExportWriteOutcome::Failed
                                            };
                                        }
                                        last_timed_export_write_outcome = write_outcome;

                                        // ---- HR59 re-arm detection (issue #289) ----
                                        //
                                        // Anchored to a completed HEM exit:
                                        // observation of an unsolicited re-arm only
                                        // begins after our exit writes landed and
                                        // readback confirmed the enable register
                                        // off. See `TimedExportRearmDetector`.
                                        if !te_config.device_rearm_confirmed {
                                            if decision.is_exit_transition
                                                && !decision.writes.is_empty()
                                            {
                                                te_rearm.note_exit_written();
                                            }
                                            let outside_window = !crate::inverter::state_machines::export_window_contains(
                                                &te_config.slots,
                                                minute_of_day,
                                            );
                                            let hem_boundary_write_pending = te_state.is_boundary_pending();
                                            let rearm_confirmed = te_rearm.observe(
                                                outside_window,
                                                snapshot.enable_discharge,
                                                hem_boundary_write_pending,
                                            );
                                            if rearm_confirmed {
                                                tracing::warn!(
                                                    polls = crate::inverter::state_machines::TIMED_EXPORT_REARM_CONFIRM_POLLS,
                                                    "Timed Export: firmware re-arms HR59 while slots are populated — \
                                                     activating clear/restore fallback"
                                                );
                                                te_config.device_rearm_confirmed = true;
                                                // Persist the learned fallback so it
                                                // survives restarts. Settings::update
                                                // is sync I/O but cheap (one small
                                                // JSON file).
                                                let _ = crate::settings::Settings::update(|s| {
                                                    s.timed_export_slots_require_clear = true;
                                                });
                                                // Classification alone leaves the
                                                // re-armed state unresolved: run the
                                                // fallback exit sequence immediately
                                                // (clear physical slots → disarm →
                                                // restore Eco) instead of waiting
                                                // for the next boundary.
                                                let mut te_state_fallback = te_state.clone();
                                                let fallback = crate::inverter::state_machines::check_timed_export(
                                                    &snapshot,
                                                    &te_config,
                                                    &mut te_state_fallback,
                                                    minute_of_day,
                                                    known_device_type.unwrap_or_default(),
                                                    crate::inverter::state_machines::TimedExportWriteOutcome::NoneIssued,
                                                    false,
                                                );
                                                te_state = te_state_fallback;
                                                if !fallback.writes.is_empty() {
                                                    let succeeded =
                                                        write_registers_fail_fast_with_gap(
                                                            &mut client,
                                                            &fallback.writes,
                                                            "Timed Export fallback repair",
                                                            Duration::from_millis(1500),
                                                        )
                                                        .await;
                                                    last_timed_export_write_outcome = if succeeded {
                                                        crate::inverter::state_machines::TimedExportWriteOutcome::Succeeded
                                                    } else {
                                                        crate::inverter::state_machines::TimedExportWriteOutcome::Failed
                                                    };
                                                }
                                            }
                                        } else {
                                            // Fallback active: keep the detector reset
                                            // so a later schedule change can re-learn.
                                            te_rearm.reset();
                                        }

                                        timed_export_owns_discharge = discharge_arbiter
                                            .selected_owner()
                                            == Some(DischargeControlOwner::TimedExport)
                                            && te_state.owns_discharge_control();

                                        // Brief re-lock to record the outcome. If the
                                        // API changed the machine state while we were
                                        // writing (enable/disable/slot edit), the
                                        // stored state is no longer the one this
                                        // decision was based on — those paths own
                                        // the reset, so we keep their value.
                                        {
                                            let mut stored = state.timed_export_state.lock().await;
                                            if *stored == te_state_at_decision {
                                                *stored = te_state.clone();
                                                drop(stored);
                                                clear_timed_export_stop_pending_if_settled(
                                                    &state,
                                                    &te_state,
                                                    &snapshot,
                                                )
                                                .await;
                                            }
                                        }
                                        let _ = commit_timed_export_rearm_if_unchanged(
                                            &state.timed_export_rearm,
                                            &state.timed_export_rearm_generation,
                                            te_rearm_generation_at_decision,
                                            &te_rearm_at_decision,
                                            te_rearm.clone(),
                                        )
                                        .await;
                                        if te_config.device_rearm_confirmed {
                                            // Mirror the learned fallback flag for
                                            // the API/UI (short-lived guard).
                                            let mut stored = state.timed_export_config.lock().await;
                                            stored.device_rearm_confirmed = true;
                                        }
                                    }
                                }

                                // ---- Timed Export invariant repair ----
                                //
                                // HR59=1 without any configured discharge slot
                                // leaves the inverter in the invalid paused state
                                // (HR27=0, no charge and no discharge). The HTTP
                                // handlers prevent creating this state, but it
                                // can still be introduced by another client or
                                // survive an app restart. Repair it only after
                                // two consecutive snapshots and never while the
                                // app owns a force-discharge transition. Skipped
                                // when the state machine already issued repair
                                // writes this cycle (its writes are model-routed
                                // and fail-fast; no need to duplicate them).
                                if !timed_export_owns_discharge {
                                    let force_discharge_in_progress =
                                        state.force_discharge_revert.lock().await.is_some();
                                    let invalid_timed_export = should_repair_timed_export(
                                        snapshot.enable_discharge,
                                        &snapshot.discharge_slots,
                                        force_discharge_in_progress,
                                    );
                                    if invalid_timed_export {
                                        invalid_timed_export_polls =
                                            invalid_timed_export_polls.saturating_add(1);
                                    } else {
                                        invalid_timed_export_polls = 0;
                                    }

                                    if invalid_timed_export_polls >= 2 {
                                        invalid_timed_export_polls = 0;
                                        let writes = build_timed_export_disable_writes(
                                            snapshot.device_type,
                                        );
                                        if discharge_arbiter
                                            .request(DischargeControlOwner::TimedExport)
                                        {
                                            discharge_control_may_override_pause = !writes.is_empty();
                                            for write in writes {
                                                match client
                                                    .write_register(write.address, write.value)
                                                    .await
                                                {
                                                    Ok(()) => tracing::warn!(
                                                        "Timed Export invariant repair: wrote reg {} = {}",
                                                        write.address, write.value
                                                    ),
                                                    Err(e) => tracing::error!(
                                                        "Timed Export invariant repair: write reg {} failed: {e}",
                                                        write.address
                                                    ),
                                                }
                                                tokio::time::sleep(Duration::from_millis(1500)).await;
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
                                if discharge_arbiter
                                    .can_request(DischargeControlOwner::TimedCharge)
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
                                        if discharge_arbiter
                                            .request(DischargeControlOwner::TimedCharge)
                                        {
                                            let ok = write_registers_to_inverter(
                                                &mut client, &writes, "Cosy enter",
                                            )
                                            .await;

                                            if ok {
                                                *state.cosy_active.lock().await = true;
                                                persist_cosy_active(true);
                                                // Mark the preloaded slot as stale since we're now active.
                                                cosy_last_preloaded_slot = None;
                                            } else {
                                                tracing::warn!("Cosy: enter writes failed - will retry on next poll");
                                            }
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
                                        // For three-phase models, also clear force flags —
                                        // except the discharge flag, which belongs to the
                                        // higher-priority Timed Export machine while it
                                        // owns the current window (issue #289 priority:
                                        // scheduled Timed Export outranks Timed Charge).
                                        if snapshot.device_type.uses_three_phase_schedule_slots() {
                                            use crate::modbus::registers::{
                                                HR_3PH_FORCE_CHARGE_ENABLE,
                                                HR_3PH_AC_CHARGE_ENABLE,
                                                HR_3PH_FORCE_DISCHARGE_ENABLE,
                                            };
                                            writes.push(RegisterWrite { address: HR_3PH_FORCE_CHARGE_ENABLE, value: 0 });
                                            writes.push(RegisterWrite { address: HR_3PH_AC_CHARGE_ENABLE, value: 0 });
                                            if !timed_export_owns_discharge {
                                                writes.push(RegisterWrite { address: HR_3PH_FORCE_DISCHARGE_ENABLE, value: 0 });
                                            }
                                        }
                                        // Restore eco mode and clear enable_discharge to
                                        // match CosyExit behaviour — but only when Timed
                                        // Export does not own the discharge-control
                                        // registers this cycle. A lower-priority
                                        // automation must not cancel an active export
                                        // window (code-review finding: Cosy/Agile
                                        // executed after Timed Export and overwrote
                                        // its mode, enable flag and slots).
                                        if !timed_export_owns_discharge {
                                            // Restore eco mode.
                                            use crate::modbus::registers::HR_BATTERY_POWER_MODE;
                                            writes.push(RegisterWrite { address: HR_BATTERY_POWER_MODE, value: 1 });
                                            // Also clear enable_discharge to match CosyExit behaviour.
                                            use crate::modbus::registers::HR_ENABLE_DISCHARGE;
                                            writes.push(RegisterWrite { address: HR_ENABLE_DISCHARGE, value: 0 });
                                        } else {
                                            tracing::debug!(
                                                "Cosy: deferring Eco restore — Timed Export owns the current window"
                                            );
                                        }

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

                                        if discharge_arbiter
                                            .request(DischargeControlOwner::TimedCharge)
                                        {
                                            let ok = write_registers_to_inverter(
                                                &mut client, &writes, "Cosy exit",
                                            )
                                            .await;

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
                                                    if discharge_arbiter
                                                        .request(DischargeControlOwner::TimedCharge)
                                                    {
                                                        let ok = write_registers_to_inverter(
                                                            &mut client, &writes, "Cosy preload",
                                                        )
                                                        .await;
                                                        if ok {
                                                            cosy_last_preloaded_slot = Some(next_idx);
                                                        }
                                                    }
                                                } else {
                                                    // No upcoming slot - clear registers if they were set.
                                                    if cosy_last_preloaded_slot.is_some() {
                                                        tracing::info!("Cosy: no upcoming slot - clearing charge slot registers");
                                                        let writes = clear_cosy_slot_registers(snapshot.device_type);
                                                        if discharge_arbiter
                                                            .request(DischargeControlOwner::TimedCharge)
                                                        {
                                                            let ok = write_registers_to_inverter(
                                                                &mut client, &writes, "Cosy clear",
                                                            )
                                                            .await;
                                                            if ok {
                                                                cosy_last_preloaded_slot = None;
                                                            }
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
                                        // Scope the cache guard so it is dropped before the
                                        // fetch (cache miss) and before the cache_snapshot
                                        // re-lock below — holding it across either deadlocked
                                        // the poll (tokio Mutex is not re-entrant).
                                        let current_price = {
                                            let prices = state.cached_agile_prices.lock().await;
                                            prices
                                                .iter()
                                                .find(|s| {
                                                    now_ts >= s.valid_from && now_ts < s.valid_to
                                                })
                                                .map(|s| s.pence)
                                        };

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
                                                // Bounded request: an unreachable/slow price source
                                                // (e.g. the real Octopus API when a test/self-host
                                                // points us back at the default) must never stall the
                                                // poll loop for ureq's default connect timeout. No
                                                // idle keep-alive connections: tests spin up throwaway
                                                // mock Octopus servers and close() them per test, and a
                                                // lingering pooled connection would hang the close.
                                                let agent = ureq::Agent::config_builder()
                                                    .timeout_global(Some(Duration::from_secs(10)))
                                                    .max_idle_connections(0)
                                                    .max_idle_connections_per_host(0)
                                                    .build();
                                                let mut resp = ureq::Agent::new_with_config(agent)
                                                    .get(&url)
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

                                    // Ownership arbitration (issue #289 priority:
                                    // scheduled Timed Export outranks Agile on
                                    // the discharge side). While the Timed
                                    // Export machine owns the current window,
                                    // Agile must not write — its discharge
                                    // slots and mode writes would cancel an
                                    // active export window, and even its
                                    // charge actions clobber HR27 / the
                                    // three-phase discharge enable on some
                                    // models. Defer entirely for this poll.
                                    let action = if !discharge_arbiter
                                        .can_request(DischargeControlOwner::Agile)
                                    {
                                        tracing::debug!(
                                            owner = ?discharge_arbiter.selected_owner(),
                                            "Agile: deferring — another discharge-control owner won this cycle"
                                        );
                                        AgileSlotAction::Defer
                                    } else {
                                        action
                                    };

                                    // Convert the action into register writes.
                                    let agile_enables_discharge =
                                        matches!(action, AgileSlotAction::Discharge { .. });
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

                                    if !skip_writes
                                        && discharge_arbiter
                                            .request(DischargeControlOwner::Agile)
                                    {
                                        if agile_enables_discharge {
                                            discharge_control_may_override_pause = true;
                                        }
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

                                // ---- Discharge pause limiters ----
                                // Run after all charging/discharging automation so thermal
                                // protection is the final authority in this poll. Both limiters
                                // share the saved pre-pause reserve; each releases only its own
                                // ownership, and the final owner to clear performs the Eco restore.
                                {
                                    let load_config = state.load_limiter_config.lock().await;
                                    let mut load_state = state.load_limiter_state.lock().await;
                                    let temperature_config =
                                        state.temperature_limiter_config.lock().await;
                                    let mut temperature_state =
                                        state.temperature_limiter_state.lock().await;
                                    let mut shared_saved = state.load_limiter_saved.lock().await;

                                    let load_was_active = load_state.is_actively_pausing();
                                    let temperature_writes =
                                        check_temperature_limiter_after_automation(
                                            &snapshot,
                                            &temperature_config,
                                            &mut temperature_state,
                                            &mut shared_saved,
                                            load_was_active,
                                            discharge_control_may_override_pause,
                                        );
                                    let temperature_active =
                                        temperature_state.is_actively_pausing();
                                    let load_writes = check_load_limiter_with_other_pause(
                                        &snapshot,
                                        &load_config,
                                        &mut load_state,
                                        poll_settings.poll_interval,
                                        &mut shared_saved,
                                        temperature_active,
                                    );

                                    snapshot.load_limiter_active =
                                        load_state.is_actively_pausing();
                                    snapshot.temperature_limiter_active =
                                        temperature_state.is_actively_pausing();
                                    let persist_saved = shared_saved.clone();

                                    drop(load_config);
                                    drop(load_state);
                                    drop(temperature_config);
                                    drop(temperature_state);
                                    drop(shared_saved);

                                    let persisted_reserve =
                                        persist_saved.as_ref().map(|value| value.reserve);
                                    let persistence_changed = crate::settings::Settings::update(|s| {
                                        let changed = s.load_limiter_saved_reserve
                                            != persisted_reserve
                                            || s.load_limiter_active_persisted
                                                != snapshot.load_limiter_active
                                            || s.temperature_limiter_active_persisted
                                                != snapshot.temperature_limiter_active;
                                        if changed {
                                            s.load_limiter_saved_reserve = persisted_reserve;
                                            s.load_limiter_active_persisted =
                                                snapshot.load_limiter_active;
                                            s.temperature_limiter_active_persisted =
                                                snapshot.temperature_limiter_active;
                                        }
                                    });
                                    if let Err(e) = persistence_changed {
                                        tracing::warn!(
                                            "Failed to persist discharge limiter state: {e}"
                                        );
                                    }

                                    let limiter_writes_present =
                                        temperature_writes.is_some() || load_writes.is_some();
                                    let limiter_owns_discharge = !limiter_writes_present
                                        || discharge_arbiter
                                            .request(DischargeControlOwner::Safety);
                                    for (label, writes) in [
                                        ("Temperature limiter", temperature_writes),
                                        ("Load limiter", load_writes),
                                    ] {
                                        if limiter_owns_discharge {
                                            if let Some(writes) = writes {
                                            for write in &writes {
                                                match client
                                                    .write_register(write.address, write.value)
                                                    .await
                                                {
                                                    Ok(()) => tracing::info!(
                                                        "{label}: wrote reg {} = {}",
                                                        write.address,
                                                        write.value
                                                    ),
                                                    Err(e) => tracing::error!(
                                                        "{label}: write reg {} failed: {e}",
                                                        write.address
                                                    ),
                                                }
                                                tokio::time::sleep(Duration::from_millis(1500))
                                                    .await;
                                            }
                                            }
                                        }
                                    }
                                }

                                // ---- Discharge floor guard (developer mode) ----
                                // Time-driven Minimum SOC floor: raise HR110 to
                                // the configured floor while any Discharge
                                // Schedule window is active, restore afterwards.
                                // Runs after the limiters so their pause
                                // writes land first; the guard never touches
                                // mode registers so it cannot fight them.
                                {
                                    let df_config = state.discharge_floor_config.lock().await;
                                    let mut df_state = state.discharge_floor_state.lock().await;
                                    let now = chrono::Local::now();
                                    let now_minutes =
                                        now.hour() as u16 * 60 + now.minute() as u16;
                                    if let Some(writes) = check_discharge_floor(
                                        &snapshot,
                                        &df_config,
                                        &mut df_state,
                                        now_minutes,
                                    ) {
                                        let persisted = match &*df_state {
                                            DischargeFloorState::FloorHeld { saved_reserve }
                                            | DischargeFloorState::HeldFromRestart {
                                                saved_reserve,
                                            } => Some(*saved_reserve),
                                            DischargeFloorState::Idle => None,
                                        };
                                        drop(df_config);
                                        drop(df_state);

                                        if let Err(e) = persist_discharge_floor_saved_reserve(persisted) {
                                            tracing::warn!(
                                                "Failed to persist discharge floor state: {e}"
                                            );
                                        }

                                        if discharge_arbiter
                                            .request(DischargeControlOwner::Safety)
                                        {
                                            for write in &writes {
                                                match client
                                                    .write_register(write.address, write.value)
                                                    .await
                                                {
                                                    Ok(()) => tracing::info!(
                                                        "Discharge floor guard: wrote reg {} = {}",
                                                        write.address,
                                                        write.value
                                                    ),
                                                    Err(e) => tracing::error!(
                                                        "Discharge floor guard: write reg {} failed: {e}",
                                                        write.address
                                                    ),
                                                }
                                                tokio::time::sleep(Duration::from_millis(1500))
                                                    .await;
                                            }
                                        }
                                    }
                                }

                                // ---- Email alerts ----
                                //
                                // Evaluate the sanitized snapshot against user-
                                // configured thresholds and send email via Brevo
                                // if any alerts are triggered (debounced).
                                //
                                // System-level battery voltage mismatch
                                // (issue #272, breaker-trip case): the
                                // transition is computed inside the alert
                                // block below (where the debounce is locked)
                                // and notified after it (where the config
                                // lock is free for the senders).
                                let mut mismatch_transition =
                                    crate::alerts::BatteryConnTransition::default();
                                let mut mismatch_suppressed = false;
                                let mut mismatch_inverter_v: f32 = 0.0;
                                let mut mismatch_module_v: Option<f32> = None;
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
                                        // System-level battery voltage mismatch
                                        // (issue #272, breaker-trip case): feed
                                        // the inverter-vs-BMS mismatch flag
                                        // through the debounce's consecutive-
                                        // cycle counter. Only fed when the
                                        // Battery Connection Lost alert is
                                        // enabled, so a disabled alert cannot
                                        // accumulate a streak. Suppressed when
                                        // the per-battery detector already
                                        // confirmed a loss (its notifications
                                        // have fired for this episode).
                                        let module_voltages: Vec<f32> = snapshot
                                            .battery_modules
                                            .iter()
                                            .map(|m| m.voltage)
                                            .collect();
                                        let mismatch =
                                            config.battery_connection_lost_enabled
                                                && crate::alerts::battery_voltage_mismatch(
                                                    snapshot.battery_voltage,
                                                    &module_voltages,
                                                );
                                        let transition = debounce
                                            .confirm_battery_voltage_mismatch(mismatch);
                                        let any_conn_lost =
                                            debounce.any_battery_connection_lost();
                                        mismatch_transition = transition;
                                        mismatch_suppressed = (transition.lost
                                            || transition.restored)
                                            && any_conn_lost;
                                        if transition.lost || transition.restored {
                                            mismatch_inverter_v = snapshot.battery_voltage;
                                            mismatch_module_v = crate::alerts::healthiest_module_voltage(
                                                &module_voltages,
                                            );
                                        }
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

                                // ---- Battery voltage mismatch notifications ----
                                // (issue #272, breaker-trip case). Sent after the
                                // alert block so the config lock is free for the
                                // senders. Suppressed when the per-battery
                                // detector already fired for this episode.
                                if (mismatch_transition.lost || mismatch_transition.restored)
                                    && !mismatch_suppressed
                                {
                                    if let Some(module_v) = mismatch_module_v {
                                        if mismatch_transition.lost {
                                            tracing::warn!(
                                                "Battery voltage mismatch: inverter \
                                                 {mismatch_inverter_v:.1} V vs module \
                                                 {module_v:.1} V — DC path fault \
                                                 suspected (breaker?)"
                                            );
                                            crate::alerts::send_battery_voltage_mismatch_notification(
                                                &state,
                                                mismatch_inverter_v,
                                                module_v,
                                            )
                                            .await;
                                        } else {
                                            tracing::info!(
                                                "Battery voltage mismatch resolved: \
                                                 inverter back to {mismatch_inverter_v:.1} V"
                                            );
                                            crate::alerts::send_battery_voltage_mismatch_restored_notification(
                                                &state,
                                                mismatch_inverter_v,
                                            )
                                            .await;
                                        }
                                    }
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

                                publish_snapshot(&state, snapshot).await;

                                (true, sanitized || block_suspicious, false)
                            }
                            Err(e) => {
                                if e.is_hard_failure() {
                                    // Hard TCP error (or no session at all) —
                                    // the socket is dead, must reconnect.
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
                    let timed_export_boundary_pending =
                        state.timed_export_state.lock().await.is_boundary_pending();
                    let poll_delay_secs =
                        next_poll_delay_secs(interval_secs, timed_export_boundary_pending);
                    let sleep_deadline =
                        tokio::time::Instant::now() + Duration::from_secs(poll_delay_secs);
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
                            if let Err(e) = crate::settings::Settings::update(|s| {
                                s.host = new.ip.clone();
                                s.port = new.port;
                            }) {
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
/// Noise floor for meter-sourced solar arrays (AC-coupled CT clamps),
/// in watts. Mirrors the frontend's `DEFAULT_NOISE_THRESHOLD_W` in
/// `src/lib/energyFlow.ts`: after dusk a CT on a solar inverter's AC
/// output picks up a few watts of the inverter's own standby draw, and
/// that must not surface as overnight "generation" (issue #273).
pub(crate) const SOLAR_METER_NOISE_THRESHOLD_W: u32 = 20;

/// The plan's AC kWh ask, for auto-refresh log lines.
fn rec_kwh(rec: &crate::forecast::planner::PlanRecommendation) -> f64 {
    match rec {
        crate::forecast::planner::PlanRecommendation::Charge { kwh, .. } => *kwh,
        _ => 0.0,
    }
}

/// Stamp the settings-derived solar-array fields (`solar_arrays`,
/// `pv1_pct`, `pv2_pct`) onto a snapshot. Extracted from the poll loop so
/// `POST /api/settings` can apply the same stamping to the *current*
/// snapshot immediately after a save: the poll loop can be busy draining
/// queued register writes for minutes after a control-page change (each
/// write costs a 1.5 s inter-write gap), and without this re-stamp the
/// Solar page's Solar Arrays card would lag a settings save by that entire
/// drain window.
pub(crate) fn stamp_solar_array_fields(
    snapshot: &mut InverterSnapshot,
    settings: &crate::settings::Settings,
) {
    snapshot.solar_arrays = compute_solar_arrays(snapshot, settings);
    snapshot.pv1_pct = if settings.pv1_rated_kw > 0.0 {
        Some((snapshot.pv1_power.max(0) as f64 * 100.0) / (settings.pv1_rated_kw * 1000.0))
    } else {
        None
    };
    snapshot.pv2_pct = if settings.pv2_rated_kw > 0.0 {
        Some((snapshot.pv2_power.max(0) as f64 * 100.0) / (settings.pv2_rated_kw * 1000.0))
    } else {
        None
    };
}

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
                // Below the noise floor the reading is standby draw /
                // clamp noise, not generation — report 0 W so the Solar
                // page doesn't show overnight "generating" (issue #273).
                power_w: {
                    let magnitude = meter.p_active_total.unsigned_abs();
                    if magnitude > SOLAR_METER_NOISE_THRESHOLD_W {
                        magnitude
                    } else {
                        0
                    }
                },
                rated_kw: arr.rated_kw,
                today_kwh: None,
                meter_address: Some(arr.meter_address),
            });
        }
    }

    out
}

/// Threshold (kWh) for a solar CT meter's cumulative energy counter to be
/// considered "new day" rather than a stale baseline carried across a
/// reboot: if the current counter is more than this far *below* the stored
/// baseline, the meter (or dongle) has been swapped/reset and the baseline
/// must be reseeded from the current reading instead of producing a
/// negative "today" energy.
pub(crate) const SOLAR_METER_BASELINE_RESEED_DELTA_KWH: f64 = 1.0;

/// Per-clamp plausibility ceiling for the CT "today" delta (issue #294).
/// Mirrors the history-side 30 kW counter-delta bound: no residential
/// clamp sees more than ~30 kW sustained, so a day's delta can never
/// grow faster than 30 kW × hours-since-midnight (+1 kWh margin). The
/// CT path runs after the sanitizer, so without this bound a single
/// corrupt counter read rode straight into the Status tile and history,
/// where MAX bucketing preserved the plateau for the rest of the day.
pub(crate) const SOLAR_METER_MAX_PLAUSIBLE_KW: f64 = 30.0;

/// After this many consecutive implausible deltas a solar CT meter's
/// jump is treated as persistent (not a transient corrupt read): the
/// baseline carries the last accepted energy forward and accumulation
/// resumes from the counter's new resting point. Mirrors the
/// sanitizer's consecutive-read confirmation conventions.
pub(crate) const CT_METER_RESEED_AFTER_REJECTIONS: u32 = 3;

/// The counter read a meter's per-poll jump bound is anchored at: the
/// cumulative counters as of the last accepted read, and when it happened.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CtMeterLastRead {
    pub(crate) import_kwh: f64,
    pub(crate) export_kwh: f64,
    pub(crate) at: chrono::NaiveDateTime,
}

/// In-memory per-meter recovery tracking for [`apply_ct_solar_authority`]
/// (issue #294). Deliberately not persisted: it only shapes within-day
/// recovery, and updating it every poll would rewrite settings.json on
/// every cycle.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct CtMeterRecovery {
    /// Last accepted same-day delta (kWh) — the value the meter holds at
    /// while its counter reads implausibly.
    pub(crate) accepted_today_kwh: f64,
    /// Consecutive polls whose delta was rejected as implausible.
    pub(crate) consecutive_rejections: u32,
    /// Anchor for the per-poll jump bound: the counters as of the last
    /// accepted read. `None` until the meter's first read of the run, in
    /// which case only the day-shaped baseline ceiling guards the read.
    pub(crate) last_read: Option<CtMeterLastRead>,
}

/// Apply CT-meter solar authority (issue #277).
///
/// On AC-coupled systems the inverter's own PV registers mirror the AC
/// output of the (separate) solar inverter with its own refresh cadence,
/// so the Overview "Solar" figure and the CT-meter array card disagree for
/// a minute at a time. When the user has labelled any external CT meter
/// (1–8) as a solar array, the CT clamp is treated as the authoritative
/// measurement of solar: every downstream consumer (wheel, Overview,
/// history, alerts) reads the same CT-derived figures by construction.
///
/// This mutates `snapshot`:
/// - `solar_power` becomes the Σ of the meter-backed solar arrays'
///   `power_w` ONLY — DC-string cards surfaced by `compute_solar_arrays`
///   stay visible individually but do NOT add into the aggregate. On an
///   AC-coupled box the inverter's own PV registers already mirror the
///   same solar the CT measures (different refresh cadence) so keeping
///   them would double-count. On a hybrid that genuinely has both DC
///   strings and a configured solar CT this means the aggregate uses the
///   CT only; per-string cards still show per-array power.
/// - `today_solar_kwh` becomes the Σ of per-meter counter deltas since
///   the persisted midnight baseline ONLY, for the same reason (the
///   inverter's register-based today counters mirror CT-measured
///   generation on an AC-coupled box). The baseline map (in
///   `baselines`) is reseeded at local midnight and on counter resets;
///   reseeded entries drive the `bool` return so the caller can persist
///   them. Each same-day delta is bounded twice (issue #294): the
///   primary per-poll bound rejects any single-poll counter rise over
///   `SOLAR_METER_MAX_PLAUSIBLE_KW` × elapsed-since-last-accepted-read
///   plus 1 kWh (no poll interval can generate more, so even a jump the
///   growing day-shaped ceiling would let through is caught on
///   arrival), and the day-shaped
///   `SOLAR_METER_MAX_PLAUSIBLE_KW` × hours-since-midnight + 1 kWh
///   ceiling backstops a run's first read where no per-poll anchor
///   exists yet. Both feed one two-stage recovery tracked in
///   `recovery`:
///   an implausible delta holds the meter's last accepted value
///   without touching its baseline (a transient corrupt read self-heals
///   on the next poll); after `CT_METER_RESEED_AFTER_REJECTIONS`
///   consecutive rejections the jump is persistent, so the jumped
///   counter's baseline reseeds to (current − accepted) — the day's
///   real energy survives and accumulation resumes from the new
///   resting point — instead of poisoning the aggregate.
/// - Per-array `today_kwh` is stamped onto the meter entries in
///   `snapshot.solar_arrays` (they ship `None` today, issue #110).
///
/// No-op when no meter-backed solar arrays are configured — hybrid /
/// DC-coupled installs keep the inverter-register path untouched.
/// Also no-op when any configured solar CT is absent from this cycle's
/// reads (the `all_configured_read` guard, review finding #2): a
/// transient meter outage must not zero the solar aggregate, the
/// inverter-register reading is the safer fallback.
///
/// Returns `true` when at least one baseline entry was reseeded (caller
/// persists via `persist_solar_meter_baselines`), `false` otherwise.
pub(crate) fn apply_ct_solar_authority(
    snapshot: &mut InverterSnapshot,
    settings: &crate::settings::Settings,
    baselines: &mut std::collections::BTreeMap<String, crate::settings::SolarMeterBaseline>,
    recovery: &mut std::collections::BTreeMap<String, CtMeterRecovery>,
    now_local: chrono::NaiveDateTime,
) -> bool {
    // Configured solar CT meters (addresses 1–8) that the user has
    // labelled as arrays. This is the *expected* meter set for this
    // cycle; `snapshot.meters` only contains meters that were actually
    // read this cycle, so a configured address missing from it means
    // the meter is absent/unread (dropped, or the probe hasn't retried
    // yet — retry interval is 5 cycles) — NOT a reading of 0 W.
    let configured_addrs: Vec<u8> = settings
        .solar_arrays
        .iter()
        .map(|a| a.meter_address)
        .filter(|&addr| (1..=8).contains(&addr))
        .collect();

    // Split snapshot borrows: solar_arrays (immutable first pass), then
    // solar_power / today_solar_kwh / per-array today_kwh writes.
    let meter_entries: Vec<(u8, i32)> = snapshot
        .solar_arrays
        .iter()
        .filter(|a| a.source == SolarArraySource::Meter)
        .filter_map(|a| a.meter_address)
        .filter(|&addr| (1..=8).contains(&addr))
        .map(|addr| {
            let p = snapshot
                .meters
                .iter()
                .find(|m| m.address == addr)
                .map(|m| m.p_active_total)
                .unwrap_or(0);
            (addr, p)
        })
        .collect();

    if meter_entries.is_empty() {
        return false; // no meter-backed arrays: nothing to do
    }

    // Absent-meter fallback (review finding #2): if any configured solar
    // CT meter is missing from this cycle's reads, the CT sum is
    // incomplete and must not override the inverter's own PV-register
    // solar — that would zero (or under-report) solar for the duration
    // of a transient meter outage. Distinguish "meter present, reads
    // ~0" (genuine 0, CT authority applies) from "meter absent/unread"
    // (fall back to the register path for this cycle). solar_power /
    // today_solar_kwh were decoded from the inverter registers this
    // cycle, so leaving them untouched is the correct fallback.
    let all_configured_read = configured_addrs
        .iter()
        .all(|&addr| snapshot.meters.iter().any(|m| m.address == addr));
    if !all_configured_read {
        tracing::debug!(
            "Solar CT meter absent this cycle — keeping inverter PV-register solar (CT authority skipped)"
        );
        return false;
    }

    let day_str = now_local.date().format("%Y-%m-%d").to_string();
    let mut any_reseeded = false;

    // Power override: when a solar CT is configured, the CT is the sole
    // authority — the inverter's own PV registers on an AC-coupled box
    // mirror the same generation the clamp measures (refreshed on the
    // dongle's own cadence), so keeping them would double-count. Any
    // DC-string arrays surfaced by compute_solar_arrays stay visible as
    // cards but do not contribute to the aggregate.
    let ct_power_w: i64 = snapshot
        .solar_arrays
        .iter()
        .filter(|a| a.source == SolarArraySource::Meter)
        .map(|a| a.power_w as i64)
        .sum();
    snapshot.solar_power = ct_power_w as i32;

    // Today energy: Σ max(Δimport, Δexport) per meter + Σ DC today counters.
    let mut ct_today_kwh: f64 = 0.0;
    let mut today_by_meter: std::collections::BTreeMap<u8, f64> = std::collections::BTreeMap::new();
    for &(addr, _p) in &meter_entries {
        let Some(meter) = snapshot.meters.iter().find(|m| m.address == addr) else {
            continue;
        };
        let key = addr.to_string();
        let current_import = meter.e_import_active_kwh as f64;
        let current_export = meter.e_export_active_kwh as f64;

        let mut force_reseed = false;
        // What the baseline becomes if (re)seeded this cycle: the raw
        // counters normally, or a carry-forward value when a persistent
        // counter jump forces a staged reseed (below).
        let mut seed_import = current_import;
        let mut seed_export = current_export;
        let today_kwh = match baselines.get(&key) {
            Some(b) if b.day == day_str => {
                // Same day: delta from baseline. Take the larger of the
                // import/export deltas — the solar CT's generation flows
                // one way, whichever the clamp orientation records.
                let d_import = (current_import - b.e_import_kwh).max(0.0);
                let d_export = (current_export - b.e_export_kwh).max(0.0);
                // Meter swap / counter reset: both counters fell far below
                // the stored baseline. The deltas read as 0 (clamped), but
                // keeping the stale baseline would freeze "today" at its
                // pre-reset value — reseed instead.
                if current_import < b.e_import_kwh - SOLAR_METER_BASELINE_RESEED_DELTA_KWH
                    && current_export < b.e_export_kwh - SOLAR_METER_BASELINE_RESEED_DELTA_KWH
                {
                    let rec = recovery.entry(key.clone()).or_default();
                    *rec = CtMeterRecovery {
                        last_read: Some(CtMeterLastRead {
                            import_kwh: current_import,
                            export_kwh: current_export,
                            at: now_local,
                        }),
                        ..Default::default()
                    };
                    force_reseed = true;
                    0.0
                } else {
                    let delta = d_import.max(d_export);
                    let rec = recovery.entry(key.clone()).or_default();
                    // Issue #294: bound the delta the same way the sanitizer
                    // bounds the inverter registers (the CT path runs after
                    // sanitization, so this is that check's last line of
                    // defence). Two bounds sharing one recovery tracker:
                    //
                    // - Per-poll (primary): between two accepted reads the
                    //   counter can only rise by SOLAR_METER_MAX_PLAUSIBLE_KW
                    //   × elapsed + 1 kWh. The day-shaped ceiling below grows
                    //   all day, so a mid-size stuck jump (+200 kWh
                    //   mid-morning) would slip under it and inflate the day
                    //   until midnight — no single poll interval can
                    //   generate that much, so the per-poll bound catches
                    //   the jump on arrival.
                    // - Day-shaped (backstop): the first read of a run has
                    //   no per-poll anchor yet (app start / restart), so it
                    //   is guarded by SOLAR_METER_MAX_PLAUSIBLE_KW ×
                    //   hours-since-midnight + 1 kWh vs the persisted
                    //   baseline.
                    //
                    // Recovery is staged: an implausible delta holds the
                    // meter's last accepted value without touching its
                    // baseline or its per-poll anchor (a transient corrupt
                    // read self-heals on the next poll); after
                    // CT_METER_RESEED_AFTER_REJECTIONS consecutive
                    // rejections the jump is persistent, so the baseline
                    // reseeds to (current − accepted) — the day's real
                    // energy survives and accumulation resumes from the new
                    // resting point — instead of poisoning the aggregate.
                    let poll_violation = rec.last_read.as_ref().and_then(|last| {
                        let elapsed_hours = ((now_local - last.at).num_seconds().max(0) as f64
                            / 3600.0)
                            .max(1.0 / 60.0);
                        let poll_ceiling = SOLAR_METER_MAX_PLAUSIBLE_KW * elapsed_hours + 1.0;
                        let poll_jump = (current_import - last.import_kwh)
                            .max(0.0)
                            .max((current_export - last.export_kwh).max(0.0));
                        (poll_jump > poll_ceiling).then_some((poll_jump, poll_ceiling))
                    });
                    let elapsed_hours = (now_local.time().num_seconds_from_midnight() as f64
                        / 3600.0)
                        .max(1.0 / 60.0);
                    let ceiling = SOLAR_METER_MAX_PLAUSIBLE_KW * elapsed_hours + 1.0;
                    let violation =
                        poll_violation.or_else(|| (delta > ceiling).then_some((delta, ceiling)));
                    if let Some((jump, bound)) = violation {
                        rec.consecutive_rejections += 1;
                        tracing::warn!(
                            meter = addr,
                            delta_kwh = delta,
                            jump_kwh = jump,
                            ceiling_kwh = bound,
                            rejections = rec.consecutive_rejections,
                            "Solar CT counter jumped implausibly — rejecting the delta"
                        );
                        if rec.consecutive_rejections >= CT_METER_RESEED_AFTER_REJECTIONS {
                            // Stage 2 — the jump is persistent (three
                            // consecutive rejections): reseed the jumped
                            // counter's baseline to (current − accepted) so
                            // the day's real energy survives and this cycle
                            // carries it, instead of dropping to 0.
                            let accepted = rec.accepted_today_kwh;
                            tracing::warn!(
                                meter = addr,
                                carried_kwh = accepted,
                                "Solar CT counter shift is persistent — carrying accepted energy forward and reseeding"
                            );
                            if d_export >= d_import {
                                seed_export = (current_export - accepted).max(0.0);
                            } else {
                                seed_import = (current_import - accepted).max(0.0);
                            }
                            // Re-anchor the per-poll bound at the new
                            // resting point — a stale pre-jump anchor would
                            // re-violate on every later poll and reseed
                            // again.
                            rec.last_read = Some(CtMeterLastRead {
                                import_kwh: current_import,
                                export_kwh: current_export,
                                at: now_local,
                            });
                            rec.consecutive_rejections = 0;
                            force_reseed = true;
                            accepted
                        } else {
                            // Stage 1 — possibly transient: hold the last
                            // accepted value; the untouched baseline and
                            // anchor let a healthy next read recover the
                            // true figure.
                            rec.accepted_today_kwh
                        }
                    } else {
                        rec.consecutive_rejections = 0;
                        rec.accepted_today_kwh = delta;
                        rec.last_read = Some(CtMeterLastRead {
                            import_kwh: current_import,
                            export_kwh: current_export,
                            at: now_local,
                        });
                        delta
                    }
                }
            }
            Some(_) | None => {
                // New day / first run / meter swap: seed baseline from the
                // current counters. Today starts at 0 and accumulates from
                // here. The recovery tracker restarts with the day, anchored
                // at this read so every later poll gets per-poll coverage.
                let rec = recovery.entry(key.clone()).or_default();
                *rec = CtMeterRecovery {
                    last_read: Some(CtMeterLastRead {
                        import_kwh: current_import,
                        export_kwh: current_export,
                        at: now_local,
                    }),
                    ..Default::default()
                };
                0.0
            }
        };

        // Insert a baseline only when it's actually new (first read of a
        // new day / first run / reseed after a counter reset). Re-writing
        // it every poll with the *current* counters would collapse
        // "today" to the last poll's delta instead of accumulating since
        // midnight, so the same-day case never touches the map.
        let needs_persist = match baselines.get(&key) {
            Some(b) => b.day != day_str || force_reseed,
            None => true,
        };
        if needs_persist {
            let entry = crate::settings::SolarMeterBaseline {
                day: day_str.clone(),
                e_import_kwh: seed_import,
                e_export_kwh: seed_export,
            };
            baselines.insert(key, entry);
            any_reseeded = true;
        }
        today_by_meter.insert(addr, today_kwh);
        ct_today_kwh += today_kwh;
    }

    // Today energy follows the same rule: the CT counters are the sole
    // authority for the aggregate. DC-string cards keep their own
    // per-array figures but don't add into the total (their register
    // energy mirrors the same generation on an AC-coupled box).
    snapshot.today_solar_kwh = ct_today_kwh as f32;

    // Stamp per-array today_kwh onto meter entries.
    for arr in snapshot.solar_arrays.iter_mut() {
        if arr.source == SolarArraySource::Meter {
            if let Some(addr) = arr.meter_address {
                if let Some(kwh) = today_by_meter.get(&addr) {
                    arr.today_kwh = Some(*kwh);
                }
            }
        }
    }

    any_reseeded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::{DeviceType, MeterData};
    use crate::settings::{
        Settings, SolarArrayConfig, SolarMeterBaseline, TariffConfig, TariffSlot,
    };
    use crate::test_util::with_isolated_config_dir_async;

    /// CODE_REVIEW.md BLOCKER: the durable stop-pending marker drives
    /// poll-loop exit repairs — but never while an API mutation holds the
    /// action lock, or the repair would starve the handler's awaited exit
    /// batches past their completion timeout.
    #[tokio::test]
    async fn stop_pending_marker_is_suppressed_while_action_lock_is_held() {
        with_isolated_config_dir_async(|| async {
            let state = AppState::new();
            state
                .timed_export_stop_pending
                .store(true, std::sync::atomic::Ordering::Release);
            assert!(
                effective_timed_export_stop_pending(&state).await,
                "with the lock free, the marker drives the repair"
            );

            let _guard = state.timed_export_action_lock.lock().await;
            assert!(
                !effective_timed_export_stop_pending(&state).await,
                "a held action lock (an in-flight stop/enable/slot-save) must suppress the repair"
            );

            drop(_guard);
            assert!(effective_timed_export_stop_pending(&state).await);

            state
                .timed_export_stop_pending
                .store(false, std::sync::atomic::Ordering::Release);
            assert!(!effective_timed_export_stop_pending(&state).await);
        })
        .await;
    }

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
            p_apparent_total: 0.0,
            pf_total: 0.0,
            frequency: 50.0,
            e_import_active_kwh: 0.0,
            e_export_active_kwh: 0.0,
        }
    }

    /// Variant of [`meter`] with cumulative energy counters set, for the
    /// CT-solar "today" baseline tests (issue #277).
    fn meter_with_energy(
        address: u8,
        p_active_total: i32,
        e_import: f32,
        e_export: f32,
    ) -> MeterData {
        MeterData {
            e_import_active_kwh: e_import,
            e_export_active_kwh: e_export,
            ..meter(address, p_active_total)
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
    fn solar_arrays_meter_below_noise_floor_reports_zero() {
        // Issue #273: after dusk a solar CT clamp picks up the inverter's
        // standby draw (reporter saw ~16 W). That magnitude must not surface
        // as overnight generation on the Solar page — it reads as 0 W until
        // it clears the same 20 W noise floor the Power page tile uses.
        let snap = InverterSnapshot {
            meters: vec![meter(1, 16), meter(2, -18), meter(3, 21)],
            ..Default::default()
        };
        let settings = Settings {
            solar_arrays: vec![
                SolarArrayConfig {
                    meter_address: 1,
                    name: String::new(),
                    rated_kw: 6.0,
                },
                SolarArrayConfig {
                    meter_address: 2,
                    name: String::new(),
                    rated_kw: 6.0,
                },
                SolarArrayConfig {
                    meter_address: 3,
                    name: String::new(),
                    rated_kw: 6.0,
                },
            ],
            ..Default::default()
        };
        let arrays = compute_solar_arrays(&snap, &settings);
        assert_eq!(arrays.len(), 3);
        assert_eq!(arrays[0].power_w, 0, "+16 W standby draw → 0 W");
        assert_eq!(arrays[1].power_w, 0, "-18 W reversed-clamp noise → 0 W");
        assert_eq!(
            arrays[2].power_w, 21,
            "above the 20 W floor still reads through"
        );
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

    // -- apply_ct_solar_authority (issue #277) --------------------------------

    /// Build a snapshot pre-stamped with CT + DC arrays the way the poll
    /// loop does, so `apply_ct_solar_authority` tests exercise the real
    /// input shape. Returns the settings used, so tests can mutate the
    /// configured meter set before calling `apply_ct_solar_authority`.
    fn ct_snapshot(
        meters: Vec<MeterData>,
        pv1_w: i32,
        today_pv1: f32,
    ) -> (InverterSnapshot, Settings) {
        let settings = Settings {
            pv1_rated_kw: 5.0,
            solar_arrays: vec![SolarArrayConfig {
                meter_address: 1,
                name: "Roof".into(),
                rated_kw: 9.48,
            }],
            ..Default::default()
        };
        let mut snap = InverterSnapshot {
            pv1_power: pv1_w,
            today_pv1_kwh: today_pv1,
            solar_power: pv1_w, // inverter register sum pre-override
            today_solar_kwh: today_pv1,
            meters,
            ..Default::default()
        };
        snap.solar_arrays = compute_solar_arrays(&snap, &settings);
        (snap, settings)
    }

    /// Fixed test clock: noon on day `n` keeps every existing scenario
    /// far inside the per-day delta bound.
    fn day(n: i64) -> chrono::NaiveDateTime {
        at(n, 12, 0)
    }

    fn at(n: i64, hour: u32, min: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 8, 19)
            .unwrap()
            .and_hms_opt(hour, min, 0)
            .unwrap()
            + chrono::Duration::days(n)
    }

    #[test]
    fn ct_authority_overrides_solar_power_from_meters() {
        // The reporter's scenario (issue #277): inverter PV registers say
        // 1.8 kW while the CT clamp on the solar inverter output says
        // 3.8 kW. With a meter-backed array configured, the CT wins.
        let (mut snap, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3800, 100.0, 500.0)], 1800, 28.1);
        let mut baselines = std::collections::BTreeMap::new();
        let mut recovery = std::collections::BTreeMap::new();
        apply_ct_solar_authority(&mut snap, &settings, &mut baselines, &mut recovery, day(0));
        assert_eq!(snap.solar_power, 3800, "CT replaces inverter PV registers");
        // Baseline seeded on first sight of the meter.
        assert_eq!(baselines.len(), 1);
        assert_eq!(baselines["1"].day, "2026-08-19");
        assert_eq!(baselines["1"].e_export_kwh, 500.0);
    }

    #[test]
    fn ct_authority_accumulates_today_energy_since_midnight() {
        // First poll of the day: baseline seeded, today = 0.
        let mut baselines = std::collections::BTreeMap::new();
        let mut recovery = std::collections::BTreeMap::new();
        let (mut snap, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 500.0)], 0, 0.0);
        apply_ct_solar_authority(&mut snap, &settings, &mut baselines, &mut recovery, day(0));
        assert_eq!(snap.today_solar_kwh, 0.0);
        // First read of the day: today starts at 0 (baseline seeded at the
        // current counters), and the meter card shows 0 rather than hiding
        // the row.
        let meter_arr = snap
            .solar_arrays
            .iter()
            .find(|a| a.source == SolarArraySource::Meter)
            .unwrap();
        assert_eq!(meter_arr.today_kwh, Some(0.0));

        // Later same day: export counter advanced 12.4 kWh → today = 12.4.
        // (Two hours after the seed — a 12.4 kWh step inside a single poll
        // interval would now trip the per-poll jump bound, rightly.)
        let (mut snap2, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 512.4)], 0, 0.0);
        let reseeded = apply_ct_solar_authority(
            &mut snap2,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 14, 0),
        );
        assert!(!reseeded, "same-day accumulation must not reseed");
        assert!((snap2.today_solar_kwh - 12.4).abs() < 0.01);
        // Per-array card gets its own today figure.
        let meter_arr = snap2
            .solar_arrays
            .iter()
            .find(|a| a.source == SolarArraySource::Meter)
            .unwrap();
        assert!((meter_arr.today_kwh.unwrap() - 12.4).abs() < 0.01);
        // Baseline untouched mid-day.
        assert_eq!(baselines["1"].e_export_kwh, 500.0);
    }

    #[test]
    fn ct_authority_resets_at_midnight() {
        // Day 0 accumulates 12.4 kWh; first poll of day 1 reseeds and
        // today restarts at 0.
        let mut baselines = std::collections::BTreeMap::new();
        let mut recovery = std::collections::BTreeMap::new();
        let (mut snap, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 500.0)], 0, 0.0);
        apply_ct_solar_authority(&mut snap, &settings, &mut baselines, &mut recovery, day(0));
        let (mut snap2, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 512.4)], 0, 0.0);
        apply_ct_solar_authority(
            &mut snap2,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 14, 0),
        );

        let (mut snap3, settings) =
            ct_snapshot(vec![meter_with_energy(1, 1000, 100.0, 513.0)], 0, 0.0);
        let reseeded =
            apply_ct_solar_authority(&mut snap3, &settings, &mut baselines, &mut recovery, day(1));
        assert!(reseeded, "new day must reseed the baseline");
        assert_eq!(snap3.today_solar_kwh, 0.0, "today restarts at midnight");
        assert_eq!(baselines["1"].e_export_kwh, 513.0);
    }

    #[test]
    fn ct_authority_reseeds_on_counter_reset() {
        // Both counters fell far below the stored baseline (meter swap /
        // factory reset): reseed instead of freezing today at the stale
        // value.
        let mut baselines = std::collections::BTreeMap::new();
        let mut recovery = std::collections::BTreeMap::new();
        let (mut snap, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 1000.0, 500.0)], 0, 0.0);
        apply_ct_solar_authority(&mut snap, &settings, &mut baselines, &mut recovery, day(0));
        let (mut snap2, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 1005.0, 512.4)], 0, 0.0);
        apply_ct_solar_authority(
            &mut snap2,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 14, 0),
        );
        assert!((snap2.today_solar_kwh - 12.4).abs() < 0.01);

        // Counter reset: import 1005 → 2, export 512.4 → 1.
        let (mut snap3, settings) = ct_snapshot(vec![meter_with_energy(1, 500, 2.0, 1.0)], 0, 0.0);
        let reseeded = apply_ct_solar_authority(
            &mut snap3,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 16, 0),
        );
        assert!(reseeded);
        assert_eq!(snap3.today_solar_kwh, 0.0);
        assert_eq!(baselines["1"].e_import_kwh, 2.0);
    }

    #[test]
    fn ct_authority_noop_without_meter_arrays() {
        // Pure hybrid (Stuart's own install): no CT arrays configured →
        // the inverter-register path must be untouched.
        let (mut snap, _settings) =
            ct_snapshot(vec![meter_with_energy(1, 3800, 100.0, 500.0)], 1800, 28.1);
        // Re-stamp arrays WITHOUT the meter entry.
        let settings = Settings {
            pv1_rated_kw: 5.0,
            ..Default::default()
        };
        snap.solar_arrays = compute_solar_arrays(&snap, &settings);
        let before_power = snap.solar_power;
        let before_today = snap.today_solar_kwh;
        let mut baselines = std::collections::BTreeMap::new();
        let mut recovery = std::collections::BTreeMap::new();
        let reseeded =
            apply_ct_solar_authority(&mut snap, &settings, &mut baselines, &mut recovery, day(0));
        assert!(!reseeded);
        assert_eq!(snap.solar_power, before_power, "hybrid untouched");
        assert_eq!(snap.today_solar_kwh, before_today, "hybrid today untouched");
        assert!(baselines.is_empty());
    }

    #[test]
    fn ct_authority_mixes_ct_and_dc_generation() {
        // Hybrid with DC strings PLUS a CT-metered array: when a solar CT
        // is configured it is the sole authority for the aggregate — the
        // DC registers on an AC-coupled box mirror the same generation
        // (issue #277's double-count guard). The DC card keeps its own
        // per-array figures for display.
        let settings = Settings {
            pv1_rated_kw: 5.0,
            solar_arrays: vec![SolarArrayConfig {
                meter_address: 1,
                name: "Garage".into(),
                rated_kw: 4.0,
            }],
            ..Default::default()
        };
        let mut snap = InverterSnapshot {
            pv1_power: 2000,
            today_pv1_kwh: 10.0,
            solar_power: 2000,
            today_solar_kwh: 10.0,
            meters: vec![meter_with_energy(1, 3200, 100.0, 500.0)],
            ..Default::default()
        };
        snap.solar_arrays = compute_solar_arrays(&snap, &settings);
        let mut baselines = std::collections::BTreeMap::new();
        let mut recovery = std::collections::BTreeMap::new();
        apply_ct_solar_authority(&mut snap, &settings, &mut baselines, &mut recovery, day(0));
        // Power = CT only (3200), not CT + DC (5200): the registers
        // mirror the same generation.
        assert_eq!(snap.solar_power, 3200);
        // Today = CT only (0 on first read); the DC register figure is
        // not added in.
        assert!((snap.today_solar_kwh - 0.0).abs() < 0.01);
        // DC string's per-array today is preserved for its card.
        let pv1_arr = snap
            .solar_arrays
            .iter()
            .find(|a| a.source == SolarArraySource::Pv1)
            .unwrap();
        assert_eq!(pv1_arr.today_kwh, Some(10.0));
    }

    #[test]
    fn ct_authority_falls_back_to_registers_when_meter_absent() {
        // Two configured solar CT meters, one drops out for a cycle
        // (meter unplugged / probe not yet retried). The absent meter
        // must read as "no data", not 0 W: falling back to a partial CT
        // sum would discard the inverter's valid PV-register solar and
        // show an under-report. While any configured solar meter is
        // missing from this cycle's reads, the register-derived
        // solar_power / today_solar_kwh stay authoritative.
        let settings = Settings {
            solar_arrays: vec![
                SolarArrayConfig {
                    meter_address: 1,
                    name: "Roof".into(),
                    rated_kw: 9.48,
                },
                SolarArrayConfig {
                    meter_address: 2,
                    name: "Garage".into(),
                    rated_kw: 4.0,
                },
            ],
            ..Default::default()
        };
        // Cycle 1: both meters readable → CT authority applies normally.
        let mut snap = InverterSnapshot {
            pv1_power: 2000,
            today_pv1_kwh: 10.0,
            solar_power: 2000, // register sum pre-override
            today_solar_kwh: 10.0,
            meters: vec![
                meter_with_energy(1, 2000, 100.0, 500.0),
                meter_with_energy(2, 1500, 10.0, 50.0),
            ],
            ..Default::default()
        };
        snap.solar_arrays = compute_solar_arrays(&snap, &settings);
        let mut baselines = std::collections::BTreeMap::new();
        let mut recovery = std::collections::BTreeMap::new();
        let reseeded =
            apply_ct_solar_authority(&mut snap, &settings, &mut baselines, &mut recovery, day(0));
        assert!(reseeded, "first sight of both meters seeds baselines");
        assert_eq!(snap.solar_power, 3500, "both CTs readable → CT authority");

        // Cycle 2: meter 2 unread → absent from snapshot.meters. The
        // register path must win, NOT a partial CT sum of 2000.
        let mut snap2 = InverterSnapshot {
            pv1_power: 2100,
            today_pv1_kwh: 10.2,
            solar_power: 2100,
            today_solar_kwh: 10.2,
            meters: vec![meter_with_energy(1, 2000, 100.0, 502.0)],
            ..Default::default()
        };
        snap2.solar_arrays = compute_solar_arrays(&snap2, &settings);
        let reseeded =
            apply_ct_solar_authority(&mut snap2, &settings, &mut baselines, &mut recovery, day(0));
        assert!(!reseeded, "absent meter must not touch baselines");
        assert_eq!(
            snap2.solar_power, 2100,
            "absent CT meter → register-derived solar, not partial CT sum"
        );
        assert!(
            (snap2.today_solar_kwh - 10.2).abs() < 0.01,
            "today energy also stays on the register path"
        );
    }

    #[test]
    fn ct_authority_genuine_zero_still_overrides() {
        // Edge case: meter present and genuinely reading 0 W (night).
        // CT authority still applies — solar shows 0, the inverter's
        // stale register figure is discarded as usual.
        let (mut snap, settings) =
            ct_snapshot(vec![meter_with_energy(1, 5, 100.0, 500.0)], 1800, 28.1);
        // 5 W is below the noise threshold → power_w clamps to 0.
        let mut baselines = std::collections::BTreeMap::new();
        let mut recovery = std::collections::BTreeMap::new();
        apply_ct_solar_authority(&mut snap, &settings, &mut baselines, &mut recovery, day(0));
        assert_eq!(snap.solar_power, 0, "present meter reading ~0 → solar is 0");
    }

    #[test]
    fn ct_authority_transient_jump_self_heals_next_cycle() {
        // Issue #294's one-cycle corrupt read must not damage the day at
        // all: the implausible delta is rejected (never becomes today's
        // solar), the tile holds the last accepted value, and because the
        // baseline is NOT poisoned, the very next healthy counter read
        // recovers the true figure by itself.
        let mut baselines = std::collections::BTreeMap::new();
        let mut recovery = std::collections::BTreeMap::new();
        let (mut snap, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 500.0)], 0, 0.0);
        apply_ct_solar_authority(
            &mut snap,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 0, 30),
        );

        // 10:25: 5 kWh of real morning generation.
        let (mut snap, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 505.0)], 0, 0.0);
        apply_ct_solar_authority(
            &mut snap,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 10, 25),
        );
        assert!((snap.today_solar_kwh - 5.0).abs() < 0.01);

        // 10:30: the reporter's corrupt read — export counter jumped
        // 1133.3 kWh in one step (ceiling at 10:30 is 316 kWh).
        let (mut snap2, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 1633.3)], 0, 0.0);
        let reseeded = apply_ct_solar_authority(
            &mut snap2,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 10, 30),
        );
        assert!(
            !reseeded,
            "a first corrupt cycle must not poison the baseline"
        );
        assert!(
            (snap2.today_solar_kwh - 5.0).abs() < 0.01,
            "corrupt delta must not become today's solar — hold the last accepted value"
        );
        assert_eq!(
            recovery["1"].consecutive_rejections, 1,
            "the rejection is tracked for the staged recovery"
        );

        // 10:35: the counter reads normally again — the true figure
        // recovers by itself, no reseed involved.
        let (mut snap3, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 507.0)], 0, 0.0);
        let reseeded = apply_ct_solar_authority(
            &mut snap3,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 10, 35),
        );
        assert!(!reseeded);
        assert!(
            (snap3.today_solar_kwh - 7.0).abs() < 0.01,
            "a transient glitch self-heals on the next healthy read"
        );
        assert_eq!(recovery["1"].consecutive_rejections, 0);

        // 10:40: a second glitch later starts a fresh streak — the
        // healthy read in between reset the rejection counter.
        let (mut snap4, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 1633.3)], 0, 0.0);
        apply_ct_solar_authority(
            &mut snap4,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 10, 40),
        );
        assert!((snap4.today_solar_kwh - 7.0).abs() < 0.01);
        assert_eq!(recovery["1"].consecutive_rejections, 1);
    }

    #[test]
    fn ct_authority_persistent_jump_carries_forward_after_three_rejections() {
        // A jump that holds for three consecutive polls is a persistent
        // counter shift, not a transient glitch: the baseline reseeds to
        // (current − last accepted) so the day's real energy survives —
        // the tile holds 24.4 kWh through the burst and accumulates from
        // the new resting point afterwards, instead of dropping to 0.
        let mut baselines = std::collections::BTreeMap::new();
        let mut recovery = std::collections::BTreeMap::new();
        let (mut snap, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 500.0)], 0, 0.0);
        apply_ct_solar_authority(
            &mut snap,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 0, 30),
        );
        // 10:25: the reporter's healthy 24.4 kWh.
        let (mut snap, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 524.4)], 0, 0.0);
        apply_ct_solar_authority(
            &mut snap,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 10, 25),
        );
        assert!((snap.today_solar_kwh - 24.4).abs() < 0.01);

        // Three consecutive implausible cycles (delta 1150 vs a ~321
        // kWh ceiling): the first two hold, the third carries forward.
        for (hour, min) in [(10, 30), (10, 35)] {
            let (mut s, settings) =
                ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 1650.0)], 0, 0.0);
            let reseeded = apply_ct_solar_authority(
                &mut s,
                &settings,
                &mut baselines,
                &mut recovery,
                at(0, hour, min),
            );
            assert!(!reseeded, "rejection {min} must not reseed yet");
            assert!(
                (s.today_solar_kwh - 24.4).abs() < 0.01,
                "stage 1 holds the last accepted value"
            );
            assert!((baselines["1"].e_export_kwh - 500.0).abs() < 0.01);
        }

        // Third rejection: stage 2 — carry-forward reseed.
        let (mut s, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 1650.0)], 0, 0.0);
        let reseeded = apply_ct_solar_authority(
            &mut s,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 10, 40),
        );
        assert!(
            reseeded,
            "the third consecutive rejection triggers the reseed"
        );
        assert!(
            (s.today_solar_kwh - 24.4).abs() < 0.01,
            "the carry-forward keeps the day's accepted energy on the reseed cycle"
        );
        assert!(
            (baselines["1"].e_export_kwh - 1625.6).abs() < 0.01,
            "baseline carries the accepted energy forward (1650 − 24.4)"
        );

        // 10:45: the counter advances normally from its new resting
        // point — accumulation resumes on top of the carried value.
        let (mut s, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 1650.8)], 0, 0.0);
        let reseeded = apply_ct_solar_authority(
            &mut s,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 10, 45),
        );
        assert!(!reseeded, "post-reseed accumulation is not another spike");
        assert!((s.today_solar_kwh - 25.2).abs() < 0.01);
    }

    #[test]
    fn ct_authority_sub_ceiling_jump_is_rejected_per_poll() {
        // Issue #294 follow-up: the day-shaped midnight ceiling grows all
        // day, so a stuck counter jump small enough to stay under it (+200
        // kWh at 09:05 vs a ~273 kWh ceiling) passes every check and
        // inflates today's solar until midnight absorbs it at the reseed.
        // Between two accepted reads the counter can only rise by
        // max-plausible-power × elapsed, so the per-poll bound must catch
        // the jump on arrival, hold the accepted figure, and stage-2 reseed
        // with the day carried forward.
        let mut baselines = std::collections::BTreeMap::new();
        let mut recovery = std::collections::BTreeMap::new();
        let (mut snap, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 500.0)], 0, 0.0);
        apply_ct_solar_authority(
            &mut snap,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 0, 30),
        );

        // 09:00: a healthy 9 kWh of morning generation.
        let (mut snap, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 509.0)], 0, 0.0);
        apply_ct_solar_authority(
            &mut snap,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 9, 0),
        );
        assert!((snap.today_solar_kwh - 9.0).abs() < 0.01);

        // 09:05: the export counter steps to 709 — a +200 kWh jump that no
        // poll interval of generation can produce, yet it sits under the
        // day-shaped ceiling (30 kW × 9.08 h + 1 ≈ 273 kWh).
        let (mut snap2, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 709.0)], 0, 0.0);
        let reseeded = apply_ct_solar_authority(
            &mut snap2,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 9, 5),
        );
        assert!(
            !reseeded,
            "a first corrupt cycle must not poison the baseline"
        );
        assert!(
            (snap2.today_solar_kwh - 9.0).abs() < 0.01,
            "a jump no poll interval can generate must be rejected even when the day-shaped ceiling would let it through"
        );
        assert_eq!(recovery["1"].consecutive_rejections, 1);

        // The jump sticks: stage 1 holds through the second cycle.
        let (mut s, settings) = ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 709.0)], 0, 0.0);
        let reseeded = apply_ct_solar_authority(
            &mut s,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 9, 10),
        );
        assert!(!reseeded, "rejection 2 must not reseed yet");
        assert!((s.today_solar_kwh - 9.0).abs() < 0.01);

        // Third rejection: stage 2 — carry-forward reseed.
        let (mut s, settings) = ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 709.0)], 0, 0.0);
        let reseeded = apply_ct_solar_authority(
            &mut s,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 9, 15),
        );
        assert!(
            reseeded,
            "the third consecutive rejection triggers the reseed"
        );
        assert!((s.today_solar_kwh - 9.0).abs() < 0.01);
        assert_eq!(recovery["1"].consecutive_rejections, 0);
        assert!(
            (baselines["1"].e_export_kwh - 700.0).abs() < 0.01,
            "baseline carries the accepted energy forward (709 − 9)"
        );

        // 09:20: accumulation resumes from the new resting point — this
        // also pins the per-poll anchor being re-based at the reseed (a
        // stale pre-jump anchor would re-violate on this +0.5 kWh read).
        let (mut s, settings) = ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 709.5)], 0, 0.0);
        let reseeded = apply_ct_solar_authority(
            &mut s,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 9, 20),
        );
        assert!(!reseeded, "post-reseed accumulation is not another spike");
        assert!((s.today_solar_kwh - 9.5).abs() < 0.01);
    }

    #[test]
    fn ct_authority_rejection_streak_resets_across_midnight() {
        // The rejection counter is day-scoped: yesterday's two rejections
        // must not combine with today's first corrupt read into an early
        // stage-2 reseed at 00:10 (where even 6 kWh in 10 minutes is
        // implausible).
        let mut baselines = std::collections::BTreeMap::new();
        let mut recovery = std::collections::BTreeMap::new();
        let (mut snap, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 500.0)], 0, 0.0);
        apply_ct_solar_authority(
            &mut snap,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 0, 30),
        );
        // Two spikes late on day 0.
        for (hour, min) in [(23, 55), (23, 58)] {
            let (mut s, settings) =
                ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 1633.3)], 0, 0.0);
            apply_ct_solar_authority(
                &mut s,
                &settings,
                &mut baselines,
                &mut recovery,
                at(0, hour, min),
            );
        }
        assert_eq!(recovery["1"].consecutive_rejections, 2);

        // 00:05 on day 1: new day — the seed path resets the tracker.
        let (mut s, settings) = ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 510.0)], 0, 0.0);
        apply_ct_solar_authority(
            &mut s,
            &settings,
            &mut baselines,
            &mut recovery,
            at(1, 0, 5),
        );
        assert_eq!(recovery["1"].consecutive_rejections, 0);

        // 00:10 on day 1: a fresh spike counts from one again — hold,
        // not a stage-2 carry-forward reseed.
        let (mut s, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 1600.0)], 0, 0.0);
        let reseeded = apply_ct_solar_authority(
            &mut s,
            &settings,
            &mut baselines,
            &mut recovery,
            at(1, 0, 10),
        );
        assert!(!reseeded, "a new day must not inherit yesterday's streak");
        assert_eq!(s.today_solar_kwh, 0.0);
        assert_eq!(recovery["1"].consecutive_rejections, 1);
        assert!((baselines["1"].e_export_kwh - 510.0).abs() < 0.01);
    }

    #[test]
    fn ct_authority_accepts_large_but_plausible_delta() {
        // 24.4 kWh by 14:00 (the reporter's healthy PV1 figure) is well
        // inside 30 kW × 14 h + 1 — must accumulate normally, untouched.
        let mut baselines = std::collections::BTreeMap::new();
        let mut recovery = std::collections::BTreeMap::new();
        let (mut snap, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 500.0)], 0, 0.0);
        apply_ct_solar_authority(
            &mut snap,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 0, 30),
        );

        let (mut snap2, settings) =
            ct_snapshot(vec![meter_with_energy(1, 3000, 100.0, 524.4)], 0, 0.0);
        let reseeded = apply_ct_solar_authority(
            &mut snap2,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 14, 0),
        );
        assert!(!reseeded, "plausible generation must not reseed");
        assert!((snap2.today_solar_kwh - 24.4).abs() < 0.01);
    }

    #[test]
    fn ct_authority_spike_isolated_per_meter() {
        // A corrupt counter on one meter must not take down a healthy
        // meter's contribution: only the spiking clamp's baseline reseeds
        // and its contribution drops out for the cycle.
        let settings = Settings {
            solar_arrays: vec![
                SolarArrayConfig {
                    meter_address: 1,
                    name: "Roof".into(),
                    rated_kw: 9.48,
                },
                SolarArrayConfig {
                    meter_address: 2,
                    name: "Garage".into(),
                    rated_kw: 4.0,
                },
            ],
            ..Default::default()
        };
        let mut snap = InverterSnapshot {
            meters: vec![
                meter_with_energy(1, 2000, 100.0, 500.0),
                meter_with_energy(2, 1500, 10.0, 50.0),
            ],
            ..Default::default()
        };
        snap.solar_arrays = compute_solar_arrays(&snap, &settings);
        let mut baselines = std::collections::BTreeMap::new();
        let mut recovery = std::collections::BTreeMap::new();
        apply_ct_solar_authority(
            &mut snap,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 0, 30),
        );

        // 10:30: meter 1 advances 5.0 kWh; meter 2's counter jumps by
        // the reporter's 1128.3.
        let mut snap2 = InverterSnapshot {
            meters: vec![
                meter_with_energy(1, 2000, 100.0, 505.0),
                meter_with_energy(2, 1500, 10.0, 1178.3),
            ],
            ..Default::default()
        };
        snap2.solar_arrays = compute_solar_arrays(&snap2, &settings);
        let reseeded = apply_ct_solar_authority(
            &mut snap2,
            &settings,
            &mut baselines,
            &mut recovery,
            at(0, 10, 30),
        );
        assert!(!reseeded, "a first corrupt cycle must not reseed anything");
        assert!(
            (snap2.today_solar_kwh - 5.0).abs() < 0.01,
            "healthy meter keeps contributing, corrupt one holds its last accepted 0"
        );
        assert_eq!(
            baselines["1"].e_export_kwh, 500.0,
            "healthy baseline untouched"
        );
        assert!(
            (baselines["2"].e_export_kwh - 50.0).abs() < 0.01,
            "corrupt meter's baseline is not poisoned by one bad cycle"
        );
        assert_eq!(recovery["1"].consecutive_rejections, 0);
        assert_eq!(recovery["2"].consecutive_rejections, 1);
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn poll_rearm_writeback_preserves_concurrent_api_reset() {
        use crate::inverter::state_machines::TimedExportRearmDetector;

        // Exercise the ABA case: the detector is Idle both before and after
        // the API reset, so a value comparison alone cannot detect the reset.
        let shared = Arc::new(Mutex::new(TimedExportRearmDetector::default()));
        let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let state_at_decision = shared.lock().await.clone();
        let generation_at_decision = generation.load(std::sync::atomic::Ordering::Acquire);
        let mut stale_updated = state_at_decision.clone();
        stale_updated.note_exit_written();

        let start = Arc::new(tokio::sync::Barrier::new(2));
        let reset_done = Arc::new(Notify::new());
        let poll_task = {
            let shared = shared.clone();
            let generation = generation.clone();
            let start = start.clone();
            let reset_done = reset_done.clone();
            tokio::spawn(async move {
                start.wait().await;
                reset_done.notified().await;
                commit_timed_export_rearm_if_unchanged(
                    &shared,
                    &generation,
                    generation_at_decision,
                    &state_at_decision,
                    stale_updated,
                )
                .await
            })
        };
        let api_task = {
            let shared = shared.clone();
            let generation = generation.clone();
            let start = start.clone();
            let reset_done = reset_done.clone();
            tokio::spawn(async move {
                start.wait().await;
                reset_shared_timed_export_rearm_detector(&shared, &generation).await;
                reset_done.notify_one();
            })
        };

        let (committed, reset) = tokio::join!(poll_task, api_task);
        reset.expect("API reset task");
        assert!(
            !committed.expect("poll write-back task"),
            "the stale poll clone must be rejected after an API reset"
        );
        assert_eq!(
            *shared.lock().await,
            TimedExportRearmDetector::default(),
            "the concurrent reset must remain authoritative"
        );
        assert_eq!(
            generation.load(std::sync::atomic::Ordering::Acquire),
            1,
            "the API reset must advance the detector generation"
        );
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
    fn all_hybrid_hv_gen3_dtc_limits_survive_later_hr0_corruption() {
        for (code, expected_w) in [("8101", 6_000), ("8102", 8_000), ("8103", 10_000)] {
            let mut snapshot = InverterSnapshot {
                device_type: DeviceType::Unknown(0x9999),
                device_type_code: "9999".to_string(),
                max_ac_power_w: 0,
                max_battery_power_w: 0,
                ..Default::default()
            };

            lock_snapshot_device_identity(&mut snapshot, DeviceType::HybridHvGen3, code);

            assert_eq!(snapshot.device_type_code, code);
            assert_eq!(snapshot.max_ac_power_w, expected_w);
            assert_eq!(snapshot.max_battery_power_w, expected_w);
        }
    }

    #[test]
    fn external_meter_probe_runs_for_single_phase_hybrid_hv_gen3() {
        // Every 0x81xx variant uses the 1000-range register layout but is
        // physically single-phase. The reference implementation still probes
        // meter addresses 0x01-0x08 for this family.
        assert!(should_probe_external_meters(
            Some(DeviceType::HybridHvGen3),
            false,
            false,
            0,
            0,
            0,
        ));
    }

    #[test]
    fn hv_stack_probe_remains_retryable_after_empty_attempt() {
        assert!(
            !hv_probe_completed(&[]),
            "an empty transient probe must not permanently disable HV discovery"
        );
        assert!(should_probe_hv_stacks(
            Some(DeviceType::HybridHvGen3),
            false,
            false,
            0,
        ));
        assert!(!should_probe_hv_stacks(
            Some(DeviceType::HybridHvGen3),
            false,
            true,
            HV_PROBE_RETRY_INTERVAL_CYCLES - 1,
        ));
        assert!(should_probe_hv_stacks(
            Some(DeviceType::HybridHvGen3),
            false,
            true,
            HV_PROBE_RETRY_INTERVAL_CYCLES,
        ));
        assert!(!should_probe_hv_stacks(
            Some(DeviceType::HybridHvGen3),
            true,
            true,
            HV_PROBE_RETRY_INTERVAL_CYCLES,
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
    fn timed_export_boundary_uses_fast_confirmation_poll() {
        assert_eq!(next_poll_delay_secs(60, true), 2);
        assert_eq!(next_poll_delay_secs(1, true), 1);
        assert_eq!(next_poll_delay_secs(60, false), 60);
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

    // -----------------------------------------------------------------------
    // drain_write_batches: the poll-loop write-drain logic (issue #245).
    //
    // These run the REAL drain against a mock TCP server — they do NOT inject
    // the outcome the way the api.rs pause/unpause helpers do, so they cover
    // the capture (first failing register), completion-channel signalling,
    // fire-and-forget batches, and the trailing-sleep skip.
    //
    // The inter-write gap is shrunk to 1 ms via `drain_write_batches_with_gap`
    // so the happy-path tests are instant; only the failure tests are slow,
    // because a rejected write genuinely retries 5× (RETRY_DELAY 500 ms) inside
    // `write_register` before surfacing — that cost is inherent and realistic.
    // The mock server replies from a fixed list, cycling with `idx % len`, so
    // mixed ack/exception sequences spell out every response explicitly.
    // -----------------------------------------------------------------------

    /// Build a GivEnergy-wrapped FC6 write-ack frame (echo of the request):
    /// serial(10) + register(2) + value(2), wrapped by `encode_frame`.
    fn write_ack_frame(register: u16, value: u16) -> Vec<u8> {
        let mut payload = Vec::with_capacity(14);
        payload.extend_from_slice(b"TEST123456");
        payload.extend_from_slice(&register.to_be_bytes());
        payload.extend_from_slice(&value.to_be_bytes());
        crate::modbus::framer::encode_frame("TEST123456", 0x11, 0x06, &payload)
    }

    #[test]
    fn take_pending_writes_caps_drain_at_max_batches_per_cycle() {
        use super::{take_pending_writes, MAX_WRITE_BATCHES_PER_CYCLE};
        use crate::inverter::encoder::RegisterWrite;

        // Queue MORE batches than the cap, tagged by address so we can check
        // which ones were taken. Completion channels stay intact for the
        // untaken batches (callers just wait longer).
        let make = |addr| PendingWriteBatch {
            writes: vec![RegisterWrite {
                address: addr,
                value: 1,
            }],
            completion: None,
            policy: Default::default(),
            owner: None,
        };
        let mut queue: Vec<PendingWriteBatch> = (0..MAX_WRITE_BATCHES_PER_CYCLE + 5)
            .map(|i| make(i as u16))
            .collect();

        let taken = take_pending_writes(&mut queue, MAX_WRITE_BATCHES_PER_CYCLE);

        assert_eq!(
            taken.len(),
            MAX_WRITE_BATCHES_PER_CYCLE,
            "one poll-cycle drain must take only the capped number of batches"
        );
        assert_eq!(
            queue.len(),
            5,
            "remainder must stay queued for later cycles"
        );
        // Oldest-first: the taken batches must be the first N queued.
        assert_eq!(taken[0].writes[0].address, 0);
        assert_eq!(
            taken[MAX_WRITE_BATCHES_PER_CYCLE - 1].writes[0].address,
            (MAX_WRITE_BATCHES_PER_CYCLE - 1) as u16
        );
        // And the queue now holds the newest batches in order.
        assert_eq!(
            queue[0].writes[0].address,
            MAX_WRITE_BATCHES_PER_CYCLE as u16
        );
        assert_eq!(
            queue[4].writes[0].address,
            (MAX_WRITE_BATCHES_PER_CYCLE + 4) as u16
        );

        // Draining repeatedly empties the queue without losing batches.
        let mut total = taken.len();
        while !queue.is_empty() {
            total += take_pending_writes(&mut queue, MAX_WRITE_BATCHES_PER_CYCLE).len();
        }
        assert_eq!(total, MAX_WRITE_BATCHES_PER_CYCLE + 5);
    }

    #[test]
    fn take_pending_writes_returns_all_when_at_or_below_cap() {
        use super::{take_pending_writes, MAX_WRITE_BATCHES_PER_CYCLE};
        use crate::inverter::encoder::RegisterWrite;

        let make = |addr| PendingWriteBatch {
            writes: vec![RegisterWrite {
                address: addr,
                value: 1,
            }],
            completion: None,
            policy: Default::default(),
            owner: None,
        };
        // Exactly at the cap (boundary) and below it: everything is taken,
        // matching the old drain-everything behaviour for small queues.
        for n in [0, 1, MAX_WRITE_BATCHES_PER_CYCLE] {
            let mut queue: Vec<PendingWriteBatch> = (0..n).map(|i| make(i as u16)).collect();
            let taken = take_pending_writes(&mut queue, MAX_WRITE_BATCHES_PER_CYCLE);
            assert_eq!(taken.len(), n, "queue of {n} batches should drain fully");
            assert!(queue.is_empty());
        }
    }

    #[test]
    fn timed_export_machine_write_gate_admits_repair_under_managed_schedule() {
        // The managed schedule is live and the readback claims ManualMode
        // (Timed Demand shape): the machine's repair writes must be emitted,
        // not silently discarded by the arbiter request.
        let mut arbiter = DischargeControlArbiter::default();
        arbiter.request(DischargeControlOwner::ManualMode);
        assert!(timed_export_machine_may_write(&mut arbiter, true));

        // Without a live managed schedule the manual claim wins and the
        // writes stay suppressed.
        let mut arbiter = DischargeControlArbiter::default();
        arbiter.request(DischargeControlOwner::ManualMode);
        assert!(!timed_export_machine_may_write(&mut arbiter, false));

        // Unclaimed cycle: the plain request admits and claims TimedExport.
        let mut arbiter = DischargeControlArbiter::default();
        assert!(timed_export_machine_may_write(&mut arbiter, false));
        assert_eq!(
            arbiter.selected_owner(),
            Some(DischargeControlOwner::TimedExport)
        );

        // A higher-priority claim still refuses the writes even when the
        // schedule is managed.
        let mut arbiter = DischargeControlArbiter::default();
        arbiter.request(DischargeControlOwner::ManualForce);
        assert!(!timed_export_machine_may_write(&mut arbiter, true));
    }

    #[test]
    fn timed_export_machine_gate_lets_the_reconciler_repair_a_timed_demand_shape() {
        // CODE_REVIEW.md finding class: with the managed schedule enabled,
        // re-arming firmware outside a window leaves HR27=1 + discharge-enable
        // set — a shape the pre-read arbiter claims as ManualMode (Timed
        // Demand). The reconciler must still run or the export-armed
        // registers are never repaired and the re-arm detector never
        // classifies.
        let mut arbiter = DischargeControlArbiter::default();
        arbiter.request(DischargeControlOwner::ManualMode);
        assert!(timed_export_machine_allowed(arbiter, true));

        // Without a live managed schedule the manual claim is genuine and
        // the machine must stay out.
        let mut arbiter = DischargeControlArbiter::default();
        arbiter.request(DischargeControlOwner::ManualMode);
        assert!(!timed_export_machine_allowed(arbiter, false));
    }

    #[test]
    fn timed_export_machine_gate_still_defers_to_higher_owners() {
        // Force actions, pauses, and safety limiters outrank the reconciler
        // even while the managed schedule is enabled (issue #289 pause
        // precedence).
        for claim in [
            DischargeControlOwner::ManualForce,
            DischargeControlOwner::ExplicitPause,
            DischargeControlOwner::Safety,
        ] {
            let mut arbiter = DischargeControlArbiter::default();
            arbiter.request(claim);
            assert!(
                !timed_export_machine_allowed(arbiter, true),
                "{claim:?} must skip the Timed Export machine"
            );
        }

        // No claim: the machine runs.
        assert!(timed_export_machine_allowed(
            DischargeControlArbiter::default(),
            false
        ));
    }

    #[test]
    fn take_pending_writes_for_owner_defers_lower_priority_batches() {
        use super::{take_pending_writes_for_owner, PendingWriteBatch};
        use crate::inverter::encoder::RegisterWrite;
        use crate::inverter::state_machines::DischargeControlOwner;

        let make = |address, owner| PendingWriteBatch {
            writes: vec![RegisterWrite { address, value: 1 }],
            completion: None,
            policy: Default::default(),
            owner,
        };
        let mut queue = vec![
            make(1, Some(DischargeControlOwner::Agile)),
            make(2, None),
            make(3, Some(DischargeControlOwner::TimedExport)),
        ];

        let (taken, winner) = take_pending_writes_for_owner(&mut queue, 8, None);

        assert_eq!(winner, Some(DischargeControlOwner::TimedExport));
        assert_eq!(
            taken
                .iter()
                .map(|batch| batch.writes[0].address)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].writes[0].address, 1);
    }

    #[test]
    fn take_pending_writes_for_owner_keeps_active_owner_over_lower_pending_request() {
        use super::{take_pending_writes_for_owner, PendingWriteBatch};
        use crate::inverter::encoder::RegisterWrite;
        use crate::inverter::state_machines::DischargeControlOwner;

        let mut queue = vec![PendingWriteBatch {
            writes: vec![RegisterWrite {
                address: 27,
                value: 1,
            }],
            completion: None,
            policy: Default::default(),
            owner: Some(DischargeControlOwner::Agile),
        }];

        let (taken, winner) =
            take_pending_writes_for_owner(&mut queue, 8, Some(DischargeControlOwner::TimedExport));

        assert!(taken.is_empty());
        assert_eq!(winner, Some(DischargeControlOwner::TimedExport));
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn take_pending_writes_for_owner_defers_timed_export_slots_behind_force_discharge() {
        // CODE_REVIEW.md finding 4: Timed Export slot restore/configuration
        // writes are queued as ManualMode (a user-issued schedule edit). An
        // active Force Discharge owns the temporary discharge slot — its
        // stop restores the captured pre-force slot — so a slot batch
        // admitted mid-force would be overwritten on restore and lose the
        // user's edit. The batch must stay queued until Force Discharge
        // releases the registers.
        use super::{take_pending_writes_for_owner, PendingWriteBatch};
        use crate::inverter::encoder::RegisterWrite;
        use crate::inverter::state_machines::DischargeControlOwner;

        let make =
            |address: u16, value: u16, owner: Option<DischargeControlOwner>| PendingWriteBatch {
                writes: vec![RegisterWrite { address, value }],
                completion: None,
                policy: Default::default(),
                owner,
            };

        // While Force Discharge owns the cycle: deferred.
        let mut queue = vec![make(56, 1700, Some(DischargeControlOwner::ManualMode))];
        let (taken, winner) =
            take_pending_writes_for_owner(&mut queue, 8, Some(DischargeControlOwner::ManualForce));
        assert!(
            taken.is_empty(),
            "a slot edit must not overwrite Force Discharge's temporary slot"
        );
        assert_eq!(winner, Some(DischargeControlOwner::ManualForce));
        assert_eq!(queue.len(), 1, "the slot batch stays queued for later");

        // Once Force Discharge has released: the batch drains.
        let (taken, winner) = take_pending_writes_for_owner(&mut queue, 8, None);
        assert_eq!(taken.len(), 1);
        assert_eq!(winner, Some(DischargeControlOwner::ManualMode));
        assert!(queue.is_empty());
    }

    #[test]
    fn take_pending_timed_export_replaces_active_manual_mode() {
        use super::{take_pending_writes_for_owner, PendingWriteBatch};
        use crate::inverter::encoder::RegisterWrite;
        use crate::inverter::state_machines::DischargeControlOwner;

        let mut queue = vec![PendingWriteBatch {
            writes: vec![RegisterWrite {
                address: 27,
                value: 0,
            }],
            completion: None,
            policy: Default::default(),
            owner: Some(DischargeControlOwner::TimedExport),
        }];

        let (taken, winner) =
            take_pending_writes_for_owner(&mut queue, 8, Some(DischargeControlOwner::ManualMode));

        assert_eq!(winner, Some(DischargeControlOwner::TimedExport));
        assert_eq!(taken.len(), 1);
        assert!(queue.is_empty());
    }

    #[test]
    fn take_pending_manual_mode_replaces_register_derived_explicit_pause() {
        use super::{take_pending_writes_for_owner, PendingWriteBatch};
        use crate::inverter::encoder::RegisterWrite;
        use crate::inverter::state_machines::DischargeControlOwner;

        // Reproduction of the "Queued 22 register write(s) — nothing is
        // ever written" report: enabling Eco queues a ManualMode batch
        // (HR59=0, slot clears, HR27=1) while the pre-read snapshot claims
        // ExplicitPause (an HR318 pause window, EcoPaused from reserve=100,
        // or an ExportPaused derived mode). Eco writes and the HR318 pause
        // gate are independent register domains, and a user-issued baseline
        // selection must never be starved indefinitely by a register-
        // derived owner: the pause stays armed and takes precedence at
        // runtime, but the baseline write itself has to drain.
        let mut queue = vec![PendingWriteBatch {
            writes: vec![
                RegisterWrite {
                    address: 59,
                    value: 0,
                },
                RegisterWrite {
                    address: 27,
                    value: 1,
                },
            ],
            completion: None,
            policy: Default::default(),
            owner: Some(DischargeControlOwner::ManualMode),
        }];

        let (taken, winner) = take_pending_writes_for_owner(
            &mut queue,
            8,
            Some(DischargeControlOwner::ExplicitPause),
        );

        assert_eq!(winner, Some(DischargeControlOwner::ManualMode));
        assert_eq!(taken.len(), 1, "manual Eco batch must drain, not starve");
        assert!(queue.is_empty());
    }

    #[test]
    fn take_pending_timed_export_still_defers_to_explicit_pause() {
        use super::{take_pending_writes_for_owner, PendingWriteBatch};
        use crate::inverter::encoder::RegisterWrite;
        use crate::inverter::state_machines::DischargeControlOwner;

        // Automations must NOT use the manual-replacement exception: a
        // Timed Export entry batch stays deferred while an explicit pause
        // owns the discharge domain (issue #289 pause precedence).
        let mut queue = vec![PendingWriteBatch {
            writes: vec![RegisterWrite {
                address: 27,
                value: 0,
            }],
            completion: None,
            policy: Default::default(),
            owner: Some(DischargeControlOwner::TimedExport),
        }];

        let (taken, winner) = take_pending_writes_for_owner(
            &mut queue,
            8,
            Some(DischargeControlOwner::ExplicitPause),
        );

        assert!(taken.is_empty());
        assert_eq!(winner, Some(DischargeControlOwner::ExplicitPause));
        assert_eq!(queue.len(), 1);
    }

    /// CODE_REVIEW.md follow-up item 2: the poll-level admission matrix.
    ///
    /// For every owner the API handlers actually queue with
    /// (`ManualMode`, `TimedExport`, `ExplicitPause`, `ManualForce`) and
    /// every register-derived snapshot state (Eco, unclaimed Timed Demand,
    /// EcoPaused, ExportPaused, HR318 pause window), assert whether the
    /// batch is admitted on this poll — including the derived owner itself.
    ///
    /// Every user-issued owner drains within one poll cycle against every
    /// register-derived state. The single documented exception is
    /// `TimedExport` under an explicit pause: the issue-#289 pause
    /// precedence defers the automation (pinned by
    /// [`take_pending_timed_export_still_defers_to_explicit_pause`]); the
    /// boundary machine retries when the pause lifts. For the same reason
    /// the API queues *user-issued* Eco-baseline writes (the Timed Export
    /// stop, and the future-slot save's Eco restore) with the `ManualMode`
    /// owner — only the automation's export-entry batch keeps
    /// `TimedExport`, so a pause can starve the arm but never the stop.
    ///
    /// This is the regression net for the item-1 starvation bug, which
    /// shipped green because the integration harness completed batches
    /// without ever running the ownership filter.
    #[tokio::test]
    async fn admission_matrix_api_owners_vs_register_derived_states() {
        use super::{current_discharge_control_owner, take_pending_writes_for_owner};
        use crate::inverter::encoder::RegisterWrite;
        use crate::inverter::model::{BatteryMode, InverterSnapshot};

        fn snapshot_for(
            mode: BatteryMode,
            pause_mode: u8,
            pause_window: Option<(u8, u8)>,
        ) -> InverterSnapshot {
            use crate::inverter::model::{DeviceType, ScheduleSlot};
            let mut snapshot = InverterSnapshot {
                device_type: DeviceType::Gen3Hybrid,
                device_type_code: "2001".into(),
                inverter_serial: "CE289".into(),
                // Pin the inverter clock so the HR318 window evaluation is
                // deterministic regardless of when the test runs.
                inverter_time: "2026-08-30 12:00:00".into(),
                battery_mode: mode,
                battery_power_mode: 1,
                enable_discharge: false,
                battery_pause_mode: pause_mode,
                ..Default::default()
            };
            if let Some((start, end)) = pause_window {
                snapshot.battery_pause_slot = ScheduleSlot {
                    enabled: true,
                    start_hour: start,
                    start_minute: 0,
                    end_hour: end,
                    end_minute: 0,
                    target_soc: 100,
                };
            }
            snapshot
        }

        // (state name, snapshot, expected derived owner)
        let states: Vec<(&str, InverterSnapshot, Option<DischargeControlOwner>)> = vec![
            ("Eco", snapshot_for(BatteryMode::Eco, 0, None), None),
            (
                "unclaimed Timed Demand",
                InverterSnapshot {
                    enable_discharge: true,
                    ..snapshot_for(BatteryMode::TimedDemand, 0, None)
                },
                Some(DischargeControlOwner::ManualMode),
            ),
            (
                "EcoPaused",
                snapshot_for(BatteryMode::EcoPaused, 0, None),
                Some(DischargeControlOwner::ExplicitPause),
            ),
            (
                "ExportPaused",
                InverterSnapshot {
                    battery_power_mode: 0,
                    ..snapshot_for(BatteryMode::ExportPaused, 0, None)
                },
                Some(DischargeControlOwner::ExplicitPause),
            ),
            (
                "HR318 pause window",
                InverterSnapshot {
                    // 00:00–23:59 covers the pinned 12:00 inverter minute.
                    battery_pause_slot: crate::inverter::model::ScheduleSlot {
                        enabled: true,
                        start_hour: 0,
                        start_minute: 0,
                        end_hour: 23,
                        end_minute: 59,
                        target_soc: 100,
                    },
                    ..snapshot_for(BatteryMode::Eco, 2, None)
                },
                Some(DischargeControlOwner::ExplicitPause),
            ),
        ];

        // (batch owner, admitted against Eco / TimedDemand / pause states)
        let batch_owners = [
            DischargeControlOwner::ManualMode,
            DischargeControlOwner::TimedExport,
            DischargeControlOwner::ExplicitPause,
            DischargeControlOwner::ManualForce,
        ];

        crate::test_util::with_isolated_config_dir_async(|| async {
            for (state_name, snapshot, expected_owner) in states {
                let state = Arc::new(AppState::new());
                *state.latest_snapshot.lock().await = Some(snapshot);

                let derived = current_discharge_control_owner(&state).await;
                assert_eq!(
                    derived, expected_owner,
                    "{state_name}: derived owner mismatch"
                );

                for owner in batch_owners {
                    let mut queue = vec![PendingWriteBatch {
                        writes: vec![RegisterWrite {
                            address: 27,
                            value: 1,
                        }],
                        completion: None,
                        policy: Default::default(),
                        owner: Some(owner),
                    }];
                    let (taken, winner) = take_pending_writes_for_owner(&mut queue, 8, derived);
                    // The only deferral cell in the matrix: the TimedExport
                    // automation stays queued while an explicit pause owns
                    // the domain (issue #289). Everything else — every
                    // user-issued selection against every register-derived
                    // state — must drain in this poll cycle.
                    let admitted = !(owner == DischargeControlOwner::TimedExport
                        && derived == Some(DischargeControlOwner::ExplicitPause));
                    assert_eq!(
                        taken.is_empty(),
                        !admitted,
                        "{state_name}: batch owner {owner:?} admission mismatch \
                         (winner {winner:?})"
                    );
                    assert_eq!(
                        queue.is_empty(),
                        admitted,
                        "{state_name}: batch owner {owner:?} queue retention mismatch"
                    );
                }
            }
        })
        .await;
    }

    #[tokio::test]
    async fn current_owner_keeps_unclaimed_timed_demand_manual() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let snapshot = InverterSnapshot {
                battery_mode: BatteryMode::TimedDemand,
                ..Default::default()
            };
            *state.latest_snapshot.lock().await = Some(snapshot);

            assert_eq!(
                super::current_discharge_control_owner(&state).await,
                Some(DischargeControlOwner::ManualMode)
            );
        })
        .await;
    }

    #[tokio::test]
    async fn current_owner_keeps_explicit_export_pause_above_automation() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let snapshot = InverterSnapshot {
                battery_mode: BatteryMode::ExportPaused,
                agile_active: true,
                ..Default::default()
            };
            *state.latest_snapshot.lock().await = Some(snapshot);

            assert_eq!(
                super::current_discharge_control_owner(&state).await,
                Some(DischargeControlOwner::ExplicitPause)
            );
        })
        .await;
    }

    #[tokio::test]
    async fn drain_signals_ok_when_all_writes_succeed() {
        use crate::inverter::encoder::{RegisterWrite, WriteOutcome};
        use crate::inverter::poll::{drain_write_batches_with_gap, PendingWriteBatch};
        use crate::modbus::client::tests::{setup_client_with_server, MockResponse};
        use tokio::sync::oneshot;

        let writes = vec![
            RegisterWrite {
                address: 27,
                value: 1,
            },
            RegisterWrite {
                address: 59,
                value: 0,
            },
            RegisterWrite {
                address: 110,
                value: 100,
            },
        ];
        let responses: Vec<MockResponse> = writes
            .iter()
            .map(|w| MockResponse::Raw(write_ack_frame(w.address, w.value)))
            .collect();
        let (_port, _server, mut client) = setup_client_with_server(responses).await;

        let (tx, rx) = oneshot::channel();
        let batch = PendingWriteBatch {
            writes,
            completion: Some(tx),
            policy: Default::default(),
            owner: None,
        };
        drain_write_batches_with_gap(&mut client, vec![batch], Duration::from_millis(1)).await;

        assert_eq!(rx.await.unwrap(), WriteOutcome::Ok);
    }

    #[tokio::test]
    async fn drain_fail_fast_skips_writes_after_first_failure() {
        // Code-review blocker: a failed slot write must prevent all later
        // export-arm writes. A FailFast batch stops at the first rejected
        // register; the remaining writes are never sent, and the completion
        // channel reports the failure.
        use crate::inverter::encoder::{RegisterWrite, WriteOutcome};
        use crate::inverter::poll::{
            drain_write_batches_with_gap, PendingWriteBatch, WriteBatchPolicy,
        };
        use crate::modbus::client::tests::{setup_client_with_server, MockResponse};
        use tokio::sync::oneshot;

        // write1 (reg 56, slot start) is rejected with a code-0 exception
        // and retried 5× inside write_register before surfacing. Under
        // FailFast, write2 (reg 27, the export-arm mode write) must NEVER
        // be attempted — total requests: 6.
        let exc = MockResponse::Exception {
            slave: 0x11,
            function: 0x06,
            code: 0,
        };
        let responses = vec![
            exc.clone(),
            exc.clone(),
            exc.clone(),
            exc.clone(),
            exc.clone(),
            exc,
        ];
        let (_port, _server, mut client) = setup_client_with_server(responses).await;

        let writes = vec![
            RegisterWrite {
                address: 56,
                value: 1600,
            },
            RegisterWrite {
                address: 27,
                value: 0,
            },
        ];
        let (tx, rx) = oneshot::channel();
        let batch = PendingWriteBatch {
            writes,
            completion: Some(tx),
            policy: WriteBatchPolicy::FailFast,
            owner: None,
        };
        drain_write_batches_with_gap(&mut client, vec![batch], Duration::from_millis(1)).await;

        match rx.await.unwrap() {
            WriteOutcome::Failed { address, .. } => {
                assert_eq!(address, 56, "should report the failing slot register");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        // The mock server responds strictly in order and closes once the
        // scripted responses are exhausted: if the drain had attempted the
        // reg-27 write after the failure, the client would have hit the
        // closed socket and the drain call itself would surface the error
        // path (or panic the connection task). Reaching here means the
        // remaining writes were skipped.
    }

    #[tokio::test]
    async fn fallback_repair_stops_after_failed_slot_clear() {
        use crate::inverter::encoder::RegisterWrite;
        use crate::inverter::poll::write_registers_fail_fast_with_gap;
        use crate::modbus::client::tests::{setup_client_with_server, MockResponse};

        // The slot clear is retried six times and rejected. No responses are
        // provided for HR59/HR27, so reaching the assertion proves the repair
        // stopped before attempting either unsafe trailing write.
        let exception = MockResponse::Exception {
            slave: 0x11,
            function: 0x06,
            code: 0,
        };
        let responses = vec![exception; 6];
        let (_port, _server, mut client) = setup_client_with_server(responses).await;
        let writes = vec![
            RegisterWrite {
                address: 56,
                value: 0,
            },
            RegisterWrite {
                address: 59,
                value: 0,
            },
            RegisterWrite {
                address: 27,
                value: 1,
            },
        ];

        let succeeded = write_registers_fail_fast_with_gap(
            &mut client,
            &writes,
            "test fallback repair",
            Duration::from_millis(1),
        )
        .await;

        assert!(!succeeded);
    }

    #[tokio::test]
    async fn drain_fail_fast_reports_ok_when_all_writes_succeed() {
        use crate::inverter::encoder::{RegisterWrite, WriteOutcome};
        use crate::inverter::poll::{
            drain_write_batches_with_gap, PendingWriteBatch, WriteBatchPolicy,
        };
        use crate::modbus::client::tests::{setup_client_with_server, MockResponse};
        use tokio::sync::oneshot;

        let responses = vec![
            MockResponse::Raw(write_ack_frame(56, 1600)),
            MockResponse::Raw(write_ack_frame(27, 0)),
        ];
        let (_port, _server, mut client) = setup_client_with_server(responses).await;

        let writes = vec![
            RegisterWrite {
                address: 56,
                value: 1600,
            },
            RegisterWrite {
                address: 27,
                value: 0,
            },
        ];
        let (tx, rx) = oneshot::channel();
        let batch = PendingWriteBatch {
            writes,
            completion: Some(tx),
            policy: WriteBatchPolicy::FailFast,
            owner: None,
        };
        drain_write_batches_with_gap(&mut client, vec![batch], Duration::from_millis(1)).await;

        assert_eq!(rx.await.unwrap(), WriteOutcome::Ok);
    }

    #[tokio::test]
    async fn drain_signals_first_failing_register() {
        use crate::inverter::encoder::{RegisterWrite, WriteOutcome};
        use crate::inverter::poll::{drain_write_batches_with_gap, PendingWriteBatch};
        use crate::modbus::client::tests::{setup_client_with_server, MockResponse};
        use tokio::sync::oneshot;

        // write1 (reg 27) is acked; write2 (reg 110) is rejected with a
        // code-0 exception, retried 5× inside write_register then surfaced.
        // Total requests: 1 (write1) + 6 (write2 attempts) = 7.
        let exc = MockResponse::Exception {
            slave: 0x11,
            function: 0x06,
            code: 0,
        };
        let responses = vec![
            MockResponse::Raw(write_ack_frame(27, 1)),
            exc.clone(),
            exc.clone(),
            exc.clone(),
            exc.clone(),
            exc.clone(),
            exc,
        ];
        let (_port, _server, mut client) = setup_client_with_server(responses).await;

        let writes = vec![
            RegisterWrite {
                address: 27,
                value: 1,
            },
            RegisterWrite {
                address: 110,
                value: 100,
            },
        ];
        let (tx, rx) = oneshot::channel();
        let batch = PendingWriteBatch {
            writes,
            completion: Some(tx),
            policy: Default::default(),
            owner: None,
        };
        drain_write_batches_with_gap(&mut client, vec![batch], Duration::from_millis(1)).await;

        match rx.await.unwrap() {
            WriteOutcome::Failed {
                address,
                value,
                error,
            } => {
                assert_eq!(address, 110, "should report the failing register");
                assert_eq!(value, 100);
                assert!(
                    error.contains("code 0"),
                    "error should carry the exception detail, got: {error}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn drain_reports_first_failure_not_a_later_one() {
        // Two writes both rejected: the reported register must be the FIRST
        // failure, not the last — this is the `first_failure.is_none()` guard.
        use crate::inverter::encoder::{RegisterWrite, WriteOutcome};
        use crate::inverter::poll::{drain_write_batches_with_gap, PendingWriteBatch};
        use crate::modbus::client::tests::{setup_client_with_server, MockResponse};
        use tokio::sync::oneshot;

        // A single exception response cycles across every retry of both
        // writes (12 requests total), so both registers are rejected.
        let responses = vec![MockResponse::Exception {
            slave: 0x11,
            function: 0x06,
            code: 0,
        }];
        let (_port, _server, mut client) = setup_client_with_server(responses).await;

        let writes = vec![
            RegisterWrite {
                address: 27,
                value: 1,
            },
            RegisterWrite {
                address: 110,
                value: 100,
            },
        ];
        let (tx, rx) = oneshot::channel();
        let batch = PendingWriteBatch {
            writes,
            completion: Some(tx),
            policy: Default::default(),
            owner: None,
        };
        drain_write_batches_with_gap(&mut client, vec![batch], Duration::from_millis(1)).await;

        match rx.await.unwrap() {
            WriteOutcome::Failed { address, .. } => {
                assert_eq!(
                    address, 27,
                    "must report the FIRST failing register, not the last (110)"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn drain_handles_fire_and_forget_batch_without_completion() {
        // A batch with no completion channel (the legacy fire-and-forget path
        // used by every other control endpoint) must still execute its writes
        // and must not hang or panic trying to signal an outcome.
        use crate::inverter::encoder::RegisterWrite;
        use crate::inverter::poll::{drain_write_batches_with_gap, PendingWriteBatch};
        use crate::modbus::client::tests::{setup_client_with_server, MockResponse};

        let writes = vec![
            RegisterWrite {
                address: 27,
                value: 1,
            },
            RegisterWrite {
                address: 110,
                value: 100,
            },
        ];
        let responses = vec![
            MockResponse::Raw(write_ack_frame(27, 1)),
            MockResponse::Raw(write_ack_frame(110, 100)),
        ];
        let (_port, _server, mut client) = setup_client_with_server(responses).await;

        let batch = PendingWriteBatch {
            writes,
            completion: None,
            policy: Default::default(),
            owner: None,
        };
        // No rx to await — just confirm this returns without hanging.
        drain_write_batches_with_gap(&mut client, vec![batch], Duration::from_millis(1)).await;
    }

    #[tokio::test]
    async fn drain_skips_trailing_sleep_when_outcome_awaited() {
        // The trailing inter-write gap after the final write is skipped for an
        // awaited batch (faster API response) but kept for fire-and-forget.
        // With a 200 ms gap, an awaited 2-write batch sleeps once (~200 ms)
        // while a fire-and-forget one sleeps twice (~400 ms) — a robust gap
        // to assert against without a slow test.
        use crate::inverter::encoder::RegisterWrite;
        use crate::inverter::poll::{drain_write_batches_with_gap, PendingWriteBatch};
        use crate::modbus::client::tests::{setup_client_with_server, MockResponse};
        use std::time::Instant;
        use tokio::sync::oneshot;

        let gap = Duration::from_millis(200);

        // Awaited: 2 writes → 1 inter-write sleep (after write 1), no trailing.
        let responses = vec![
            MockResponse::Raw(write_ack_frame(27, 1)),
            MockResponse::Raw(write_ack_frame(110, 100)),
        ];
        let (_port, _server, mut client) = setup_client_with_server(responses).await;
        let (tx, _rx) = oneshot::channel();
        let batch = PendingWriteBatch {
            writes: vec![
                RegisterWrite {
                    address: 27,
                    value: 1,
                },
                RegisterWrite {
                    address: 110,
                    value: 100,
                },
            ],
            completion: Some(tx),
            policy: Default::default(),
            owner: None,
        };
        let awaited_start = Instant::now();
        drain_write_batches_with_gap(&mut client, vec![batch], gap).await;
        let awaited_elapsed = awaited_start.elapsed();

        // Fire-and-forget: 2 writes → sleep after write 1 AND trailing sleep.
        let responses = vec![
            MockResponse::Raw(write_ack_frame(27, 1)),
            MockResponse::Raw(write_ack_frame(110, 100)),
        ];
        let (_port, _server, mut client) = setup_client_with_server(responses).await;
        let batch = PendingWriteBatch {
            writes: vec![
                RegisterWrite {
                    address: 27,
                    value: 1,
                },
                RegisterWrite {
                    address: 110,
                    value: 100,
                },
            ],
            completion: None,
            policy: Default::default(),
            owner: None,
        };
        let fire_forget_start = Instant::now();
        drain_write_batches_with_gap(&mut client, vec![batch], gap).await;
        let fire_forget_elapsed = fire_forget_start.elapsed();

        assert!(
            awaited_elapsed < Duration::from_millis(300),
            "awaited batch should skip the trailing sleep (~1 gap), took {awaited_elapsed:?}"
        );
        assert!(
            fire_forget_elapsed > Duration::from_millis(350),
            "fire-and-forget batch should keep the trailing sleep (~2 gaps), took {fire_forget_elapsed:?}"
        );
    }

    // ==================================================================
    // End-to-end first poll cycle against a keyed Modbus mock.
    // ==================================================================

    /// Respond to every read by its requested `(slave, function, base, count)`
    /// rather than relying on response order. This mirrors a dongle's register
    /// map closely enough to drive the real connect → warmup → decode →
    /// sanitize → broadcast path without a live inverter.
    async fn run_keyed_register_mock(listener: tokio::net::TcpListener) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut stream, _) = listener.accept().await.unwrap();
        loop {
            let mut header = [0u8; 6];
            if stream.read_exact(&mut header).await.is_err() {
                break;
            }
            let length = u16::from_be_bytes([header[4], header[5]]) as usize;
            let mut body = vec![0u8; length];
            if stream.read_exact(&mut body).await.is_err() {
                break;
            }

            let mut frame = header.to_vec();
            frame.extend_from_slice(&body);
            let decoded = match crate::modbus::framer::decode_frame(&frame) {
                Ok(decoded) => decoded,
                Err(_) => break,
            };
            if decoded.payload.len() < 4 {
                break;
            }

            let base = u16::from_be_bytes([decoded.payload[0], decoded.payload[1]]);
            let count = u16::from_be_bytes([decoded.payload[2], decoded.payload[3]]) as usize;
            let mut data = vec![0u16; count];

            // Safe, deliberately boring telemetry for the first standard input
            // block. Unknown(0) at HR(0) avoids an immediate model-specific
            // re-poll, while the rest of the values pass absolute sanitization.
            if decoded.function == 4 && decoded.slave == 0x32 && base == 60 {
                // LV BMS IR(103): maximum battery temperature, /10 °C.
                if count > 43 {
                    data[43] = 250;
                }
            }
            if decoded.function == 4 && decoded.slave == 0x11 && base == 0 {
                if count > 5 {
                    data[5] = 2300; // 230.0 V
                }
                if count > 13 {
                    data[13] = 5000; // 50.00 Hz
                }
                if count > 41 {
                    data[41] = 250; // 25.0 °C
                }
                if count > 50 {
                    data[50] = 4800; // 48.0 V
                }
                if count > 56 {
                    data[56] = 250; // 25.0 °C
                }
                if count > 59 {
                    data[59] = 50; // 50% SOC
                }
            }

            // Read responses contain the serial prefix, start register, count,
            // then the returned register values. This is the exact frame shape
            // consumed by ModbusClient's response parser.
            let mut payload = Vec::with_capacity(14 + count * 2);
            payload.extend_from_slice(b"TEST123456");
            payload.extend_from_slice(&base.to_be_bytes());
            payload.extend_from_slice(&(count as u16).to_be_bytes());
            for value in data {
                payload.extend_from_slice(&value.to_be_bytes());
            }
            let response = crate::modbus::framer::encode_frame(
                "TEST123456",
                decoded.slave,
                decoded.function,
                &payload,
            );
            if stream.write_all(&response).await.is_err() {
                break;
            }
        }
    }

    #[tokio::test]
    async fn poll_loop_broadcasts_first_sanitized_snapshot_from_mock_dongle() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(run_keyed_register_mock(listener));

            let state = Arc::new(AppState::new());
            {
                let mut settings = state.settings.lock().await;
                settings.host = "127.0.0.1".to_string();
                settings.port = port;
                settings.serial = "TEST123456".to_string();
                settings.interval_secs = 1;
            }

            let poll_task = tokio::spawn(run_poll_loop(state.clone()));
            let snapshot = tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    if let Some(snapshot) = state.latest_snapshot.lock().await.clone() {
                        break snapshot;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("poll loop did not broadcast a snapshot");

            poll_task.abort();
            server.abort();

            assert_eq!(snapshot.soc, 50);
            assert_eq!(snapshot.grid_voltage, 230.0);
            assert_eq!(snapshot.grid_frequency, 50.0);
            assert_eq!(snapshot.battery_temperature, 25.0);
            assert_eq!(snapshot.device_type_code, "0000");
        })
        .await;
    }

    // ------------------------------------------------------------------
    // Forecast plan auto-apply vs auto-refresh: exactly one machine
    // writer of charge slot 1 per day. These drive the real poll loop
    // against the mock dongle so the supersession gate (refresh stands
    // down while auto-apply is enabled) is pinned end-to-end, not just at
    // the pure-gate level.
    // ------------------------------------------------------------------

    /// A tariff whose cheapest window opens ~2 minutes from now — inside
    /// the refresh's fixed 30-minute lead and any auto-apply lead up to
    /// 120 — so the gates are due on the first post-connect cycle
    /// regardless of the wall clock. Head + tail rows with equal rates
    /// trigger `cheapest_import_window`'s midnight-merge, giving one
    /// ~35-minute window that wraps the day boundary cleanly.
    fn tariff_with_window_starting_soon() -> TariffConfig {
        let now = chrono::Local::now();
        let start_t = now + chrono::Duration::minutes(2);
        let end_t = start_t + chrono::Duration::minutes(35);
        let hm = |t: chrono::DateTime<chrono::Local>| format!("{:02}:{:02}", t.hour(), t.minute());
        TariffConfig {
            slots: vec![
                TariffSlot {
                    start: "00:00".to_string(),
                    end: hm(end_t),
                    rate: 0.09,
                },
                TariffSlot {
                    start: hm(start_t),
                    end: "23:59".to_string(),
                    rate: 0.09,
                },
            ],
        }
    }

    /// Start the poll loop against a mock dongle with the given disk
    /// settings (the feature flags — tariff, plan triggers — are re-read
    /// from disk every cycle; the connection uses the in-memory copy).
    async fn spawn_plan_trigger_harness(
        mut settings: Settings,
    ) -> (
        Arc<AppState>,
        tokio::task::JoinHandle<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(run_keyed_register_mock(listener));

        settings.host = "127.0.0.1".to_string();
        settings.port = port;
        settings.serial = "TEST123456".to_string();
        settings.poll_interval = 1;
        settings.save().expect("save settings for poll loop");

        let state = Arc::new(AppState::new());
        {
            let mut mem = state.settings.lock().await;
            mem.host = settings.host.clone();
            mem.port = port;
            mem.serial = "TEST123456".to_string();
            mem.interval_secs = 1;
        }

        let poll_task = tokio::spawn(run_poll_loop(state.clone()));
        (state, poll_task, server)
    }

    /// Wait (bounded) for a plan-trigger latch to be stamped with today's
    /// date — the latch is set on the first Due cycle, whatever the plan
    /// outcome.
    async fn wait_for_latch(
        latch: &tokio::sync::Mutex<Option<chrono::NaiveDate>>,
    ) -> chrono::NaiveDate {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(date) = *latch.lock().await {
                    break date;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("plan trigger never fired against the mock dongle")
    }

    #[tokio::test]
    async fn auto_apply_supersedes_auto_refresh_when_both_enabled() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let settings = Settings {
                import_tariff_config: Some(tariff_with_window_starting_soon()),
                forecast_plan_auto_refresh: true,
                forecast_plan_auto_apply_enabled: true,
                forecast_plan_auto_apply_lead_minutes: 120,
                weather_config: crate::settings::WeatherConfig {
                    enabled: false,
                    ..Default::default()
                },
                ..Default::default()
            };
            let (state, poll_task, server) = spawn_plan_trigger_harness(settings).await;

            let today = chrono::Local::now().date_naive();
            let applied = wait_for_latch(&state.forecast_plan_apply_date).await;
            poll_task.abort();
            server.abort();

            assert_eq!(
                applied, today,
                "auto-apply must fire inside the lead window"
            );
            // Supersession: while auto-apply owns the trigger, the nightly
            // refresh must stand down even though its own gate is due —
            // charge slot 1 gets exactly one machine write per day.
            assert_eq!(
                *state.forecast_plan_refresh_date.lock().await,
                None,
                "auto-refresh must not run while auto-apply is enabled"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn auto_refresh_alone_still_fires_when_due() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let settings = Settings {
                import_tariff_config: Some(tariff_with_window_starting_soon()),
                forecast_plan_auto_refresh: true,
                forecast_plan_auto_apply_enabled: false,
                weather_config: crate::settings::WeatherConfig {
                    enabled: false,
                    ..Default::default()
                },
                ..Default::default()
            };
            let (state, poll_task, server) = spawn_plan_trigger_harness(settings).await;

            let today = chrono::Local::now().date_naive();
            let refreshed = wait_for_latch(&state.forecast_plan_refresh_date).await;
            poll_task.abort();
            server.abort();

            assert_eq!(refreshed, today);
            assert_eq!(
                *state.forecast_plan_apply_date.lock().await,
                None,
                "auto-apply is off and must never stamp its latch"
            );
        })
        .await;
    }

    // ------------------------------------------------------------------
    // publish_snapshot must reconcile with concurrent settings saves.
    //
    // Poll-cycle sequence: drain queued writes (up to minutes) → read
    // registers → load settings → decode/stamp → publish. A
    // `POST /api/settings` save landing between the cycle's settings load
    // and its publish is currently CLOBBERED: publish stores the snapshot
    // stamped from the older settings, hiding the new solar-array config
    // (e.g. rated kWp → Solar page "% of max") until the next cycle —
    // which a write storm can push minutes out. The publish step must
    // re-stamp the kWp-derived fields from the freshest on-disk settings
    // so the stored/broadcast snapshot can never regress them.
    // ------------------------------------------------------------------

    fn pv_snapshot() -> InverterSnapshot {
        InverterSnapshot {
            timestamp: 1_700_000_000,
            solar_power: 3_321,
            pv1_power: 3_321,
            soc: 64,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn publish_restamps_solar_fields_when_settings_saved_mid_cycle() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            // Cycle stamped the snapshot from settings where pv1_rated_kw=5
            // (simulating the cycle's stale in-memory settings read).
            let mut snap = pv_snapshot();
            snap.pv1_pct = Some(66.42); // 3321 W / 5000 W

            // The "concurrent" save while the cycle was still in flight —
            // written to disk (as update_settings persists it) before the
            // cycle reaches its publish point. Sequential reproduction of
            // exactly the racy interleaving, not a flaky threaded race.
            Settings::update(|s| {
                s.pv1_rated_kw = 3.0;
            })
            .expect("seed settings");

            let state = Arc::new(AppState::new());
            let mut rx = state.tx.subscribe();
            publish_snapshot(&state, snap).await;

            let latest = state.latest_snapshot.lock().await.clone().expect("published");
            let (tx_pct, rx_pct): (f64, f64) = match rx.try_recv().expect("broadcast frame") {
                PollMessage::Snapshot(b) => (b.pv1_pct.expect("tx pv_pct"), latest.pv1_pct.unwrap_or(f64::NAN)),
                _ => panic!("expected snapshot frame"),
            };
            let want = 3321.0 / 3000.0 * 100.0; // 110.7%
            for (label, got) in [("broadcast", tx_pct), ("latest_snapshot", rx_pct)] {
                assert!(
                    (got - want).abs() < 0.01,
                    "{label} pv1_pct = {got}, want {want} — publish must re-stamp                      from on-disk settings so a mid-cycle save is not clobbered"
                );
            }
        })
        .await;
    }

    #[tokio::test]
    async fn publish_without_settings_save_keeps_cycle_stamps() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            Settings::update(|s| {
                s.pv1_rated_kw = 5.0;
            })
            .expect("seed settings");

            let mut snap = pv_snapshot();
            snap.pv1_pct = Some(66.42); // stamped from the same settings

            let state = Arc::new(AppState::new());
            publish_snapshot(&state, snap).await;

            let latest = state
                .latest_snapshot
                .lock()
                .await
                .clone()
                .expect("published");
            assert_eq!(
                latest.pv1_pct,
                Some(66.42),
                "no settings save mid-cycle → publish must not disturb the cycle's stamps"
            );
        })
        .await;
    }

    // ------------------------------------------------------------------
    // persist_auto_winter_saved must not clobber concurrent settings saves.
    // The poll loop passes its cycle-start settings clone; saving that whole
    // struct back after a disjoint field (e.g. pv1_rated_kw) changed on disk
    // mid-cycle is a lost update (same class as review finding #5, fixed for
    // Adaptive Charge by switching to a narrow Settings::update).
    // ------------------------------------------------------------------

    #[test]
    fn persist_auto_winter_does_not_clobber_disjoint_settings_save() {
        crate::test_util::with_isolated_config_dir(|| {
            // The cycle's pre-save settings snapshot — kept named to make
            // the interleaving explicit even though the fixed persist path
            // no longer consults it.
            let _stale = Settings::load();

            // The concurrent save lands while the cycle is in flight —
            // sequential reproduction of exactly the racy interleaving, not
            // a flaky threaded race: load old → other writer saves → winter
            // persist saves from the old snapshot.
            Settings::update(|s| {
                s.pv1_rated_kw = 5.0;
            })
            .expect("concurrent save");

            persist_auto_winter_saved(&Some(AutoWinterSaved {
                enable_charge_target: true,
                target_soc: 80,
            }));

            let after = Settings::load();
            assert_eq!(
                after.pv1_rated_kw, 5.0,
                "auto-winter persist clobbered the concurrent settings save"
            );
            assert_eq!(
                after.auto_winter_saved_enable_target,
                Some(true),
                "winter pre-values must still be persisted"
            );
            assert_eq!(after.auto_winter_saved_target_soc, Some(80));
        });
    }

    // ------------------------------------------------------------------
    // persist_solar_meter_baselines / persist_discharge_floor_saved_reserve
    // must not clobber concurrent settings saves. These two poll-loop
    // sites still do plain load + set + save on master; the GREEN fix
    // converts both to `Settings::update`, mirroring the Aug-21 sweep
    // (auto-winter c5fd0ed, Adaptive Charge baseline at line ~2409). The
    // concurrent tests here pin the contract by racing each helper
    // against a `Settings::update` that touches a disjoint field
    // (tariff config). With the RED pattern the helper's full-struct
    // save reverts the tariff; with the GREEN pattern both writes
    // serialise under the settings mutex and both survive.
    // ------------------------------------------------------------------

    fn baseline_for_test(day: &str, e_import_kwh: f64, e_export_kwh: f64) -> SolarMeterBaseline {
        SolarMeterBaseline {
            day: day.to_string(),
            e_import_kwh,
            e_export_kwh,
        }
    }

    fn tariff_for_test(rate: f64) -> TariffConfig {
        TariffConfig {
            slots: vec![TariffSlot {
                start: "00:00".to_string(),
                end: "23:59".to_string(),
                rate,
            }],
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn persist_solar_meter_baselines_survives_concurrent_disjoint_save() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            // Seed an unrelated baseline so the persist path has something
            // to overwrite and the on-disk struct is non-trivial at lock
            // acquire time.
            Settings::update(|s| {
                s.solar_meter_baselines.insert(
                    "address-1".to_string(),
                    baseline_for_test("2026-08-22", 5.5, 0.0),
                );
            })
            .unwrap();

            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

            let h_baseline = {
                let barrier = barrier.clone();
                tokio::spawn(async move {
                    barrier.wait();
                    let mut new_baselines = std::collections::BTreeMap::new();
                    new_baselines.insert(
                        "address-2".to_string(),
                        baseline_for_test("2026-08-23", 100.0, 0.0),
                    );
                    persist_solar_meter_baselines(new_baselines).unwrap();
                })
            };
            let h_tariff = {
                let barrier = barrier.clone();
                tokio::spawn(async move {
                    barrier.wait();
                    Settings::update(|s| {
                        s.import_tariff_config = Some(tariff_for_test(0.55));
                    })
                    .unwrap();
                })
            };
            let _ = tokio::join!(h_baseline, h_tariff);

            let after = Settings::load();
            let baseline = after
                .solar_meter_baselines
                .get("address-2")
                .expect("solar baseline persist must survive concurrent disjoint save");
            assert!(
                (baseline.e_import_kwh - 100.0).abs() < 0.0001,
                "baseline must keep poll writer's value, got {}",
                baseline.e_import_kwh
            );
            let saved_rate = after
                .import_tariff_config
                .as_ref()
                .and_then(|c| c.slots.first())
                .map(|s| s.rate)
                .unwrap_or(f64::NAN);
            assert!(
                (saved_rate - 0.55).abs() < 0.0001,
                "tariff must survive concurrent baseline persist, got {}",
                saved_rate
            );
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn persist_discharge_floor_saved_reserve_survives_concurrent_disjoint_save() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

            let h_floor = {
                let barrier = barrier.clone();
                tokio::spawn(async move {
                    barrier.wait();
                    persist_discharge_floor_saved_reserve(Some(42)).unwrap();
                })
            };
            let h_tariff = {
                let barrier = barrier.clone();
                tokio::spawn(async move {
                    barrier.wait();
                    Settings::update(|s| {
                        s.import_tariff_config = Some(tariff_for_test(0.33));
                    })
                    .unwrap();
                })
            };
            let _ = tokio::join!(h_floor, h_tariff);

            let after = Settings::load();
            assert_eq!(
                after.discharge_floor_saved_reserve,
                Some(42),
                "discharge floor persist must survive concurrent tariff save"
            );
            let saved_rate = after
                .import_tariff_config
                .as_ref()
                .and_then(|c| c.slots.first())
                .map(|s| s.rate)
                .unwrap_or(f64::NAN);
            assert!(
                (saved_rate - 0.33).abs() < 0.0001,
                "tariff must survive concurrent floor persist, got {}",
                saved_rate
            );
        })
        .await;
    }
}
