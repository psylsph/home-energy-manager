//! EV Charger (GivEVC) read-only monitoring via standard Modbus TCP.
//!
//! The GivEnergy EV charger uses **standard Modbus TCP** (FC3 read holding
//! registers) on port 502 — completely separate from the proprietary framing
//! protocol used by the inverter dongle on port 8899.
//!
//! Register layout extracted from GivTCP `evc.py`:
//!   - Block 1: HR 0–59  (60 registers)
//!   - Block 2: HR 60–114 (55 registers)
//!
//! Key registers:
//!   HR 0   Charging_State       (enum)
//!   HR 2   Connection_Status    (enum)
//!   HR 6   Current_L1           (÷10 A)
//!   HR 13  Active_Power         (W)
//!   HR 29  Meter_Energy         (÷10 kWh)
//!   HR 36  Charge_Limit         (÷10 A)
//!   HR 72  Charge_Session_Energy (÷10 kWh)
//!   HR 79  Charge_Session_Duration (seconds)
//!   HR 109 Voltage_L1           (÷10 V)

use std::sync::Arc;
use std::time::SystemTime;
use tokio::time::{sleep, Duration};
use tokio_modbus::prelude::*;

use crate::inverter::poll::{AppState, PollMessage};

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Snapshot of EV charger state decoded from Modbus registers.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvcSnapshot {
    /// Charging state decoded from HR 0.
    pub charging_state: String,
    /// Connection status decoded from HR 2.
    pub connection_status: String,
    /// Active power in watts (HR 13).
    pub active_power: i32,
    /// L1 current in amps × 10 (HR 6).
    pub current_l1: f32,
    /// L2 current in amps × 10 (HR 8).
    pub current_l2: f32,
    /// L3 current in amps × 10 (HR 10).
    pub current_l3: f32,
    /// L1 voltage in volts × 10 (HR 109).
    pub voltage_l1: f32,
    /// L2 voltage in volts × 10 (HR 111).
    pub voltage_l2: f32,
    /// L3 voltage in volts × 10 (HR 113).
    pub voltage_l3: f32,
    /// Total meter energy in kWh × 10 (HR 29).
    pub meter_energy_kwh: f32,
    /// Charge session energy in kWh × 10 (HR 72).
    pub session_energy_kwh: f32,
    /// Charge session duration in seconds (HR 79).
    pub session_duration_secs: u32,
    /// Charge current limit in amps × 10 (HR 36).
    pub charge_limit_a: f32,
    /// Serial number decoded from HR 38–68 (ASCII).
    pub serial_number: String,
}

impl Default for EvcSnapshot {
    fn default() -> Self {
        Self {
            charging_state: "Unknown".into(),
            connection_status: "Unknown".into(),
            active_power: 0,
            current_l1: 0.0,
            current_l2: 0.0,
            current_l3: 0.0,
            voltage_l1: 0.0,
            voltage_l2: 0.0,
            voltage_l3: 0.0,
            meter_energy_kwh: 0.0,
            session_energy_kwh: 0.0,
            session_duration_secs: 0,
            charge_limit_a: 0.0,
            serial_number: String::new(),
        }
    }
}

/// Keep a recently successful EVC snapshot during a short outage so one
/// failed poll does not make the UI flap between connected and disconnected.
/// After this deadline the cached data is no longer considered reachable.
pub const EVC_CACHE_GRACE_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvcConnectionState {
    #[default]
    NeverConnected,
    Connected,
    Degraded,
    Disconnected,
}

/// Reachability metadata returned by the API and sent alongside EVC events.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EvcReachabilityStatus {
    pub connection_state: EvcConnectionState,
    pub reachable: bool,
    pub stale: bool,
    pub last_success_at_epoch_ms: Option<u64>,
    pub age_seconds: Option<u64>,
}

/// Backend-owned EVC connection state. The clock is supplied by callers so
/// the grace and expiry rules remain deterministic in tests.
#[derive(Debug, Clone, Default)]
pub struct EvcReachability {
    connection_state: EvcConnectionState,
    last_success_at_epoch_ms: Option<u64>,
}

impl EvcReachability {
    pub fn record_success(&mut self, now_ms: u64) {
        self.connection_state = EvcConnectionState::Connected;
        self.last_success_at_epoch_ms = Some(now_ms);
    }

    pub fn record_failure(&mut self, now_ms: u64) {
        let within_grace = self
            .last_success_at_epoch_ms
            .is_some_and(|last| now_ms.saturating_sub(last) <= EVC_CACHE_GRACE_MS);
        self.connection_state = if within_grace {
            EvcConnectionState::Degraded
        } else if self.last_success_at_epoch_ms.is_some() {
            EvcConnectionState::Disconnected
        } else {
            EvcConnectionState::NeverConnected
        };
    }

    pub fn reset(&mut self) {
        self.connection_state = EvcConnectionState::NeverConnected;
        self.last_success_at_epoch_ms = None;
    }

    pub fn status_at(&self, now_ms: u64) -> EvcReachabilityStatus {
        let age_ms = self
            .last_success_at_epoch_ms
            .map(|last| now_ms.saturating_sub(last));
        let within_grace = age_ms.is_some_and(|age| age <= EVC_CACHE_GRACE_MS);
        let reachable = within_grace
            && matches!(
                self.connection_state,
                EvcConnectionState::Connected | EvcConnectionState::Degraded
            );
        let connection_state = if reachable {
            self.connection_state
        } else if self.last_success_at_epoch_ms.is_some() {
            EvcConnectionState::Disconnected
        } else {
            EvcConnectionState::NeverConnected
        };

        EvcReachabilityStatus {
            connection_state,
            reachable,
            stale: self.last_success_at_epoch_ms.is_some()
                && (self.connection_state != EvcConnectionState::Connected || !reachable),
            age_seconds: age_ms.map(|age| age / 1_000),
            last_success_at_epoch_ms: self.last_success_at_epoch_ms,
        }
    }
}

pub fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

// ---------------------------------------------------------------------------
// Enum decoders
// ---------------------------------------------------------------------------

const CHARGING_STATES: &[&str] = &[
    "Unknown",         // 0
    "Idle",            // 1
    "Connected",       // 2
    "Starting",        // 3
    "Charging",        // 4
    "Startup Failure", // 5
    "End of Charging", // 6
    "System Failure",  // 7
    "Scheduled",       // 8
    "Updating",        // 9
    "Unstable CP",     // 10
];

const CONNECTION_STATUSES: &[&str] = &[
    "Not Connected", // 0
    "Connected",     // 1
];

fn decode_charging_state(val: u16) -> String {
    CHARGING_STATES
        .get(val as usize)
        .unwrap_or(&"Unknown")
        .to_string()
}

fn decode_connection_status(val: u16) -> String {
    CONNECTION_STATUSES
        .get(val as usize)
        .unwrap_or(&"Unknown")
        .to_string()
}

// ---------------------------------------------------------------------------
// Register decoder
// ---------------------------------------------------------------------------

fn decode_serial(regs: &[u16]) -> String {
    regs.iter()
        .filter_map(|&w| {
            if w == 0 {
                None
            } else {
                Some(char::from_u32(w as u32).unwrap_or('?'))
            }
        })
        .collect()
}

fn decode_discovery_serial(regs: &[u16]) -> Option<String> {
    if regs.len() < 31 {
        return None;
    }
    let serial_regs = &regs[..31];
    if serial_regs
        .iter()
        .any(|&w| w != 0 && !(0x20..=0x7e).contains(&w))
    {
        return None;
    }
    let serial = decode_serial(serial_regs);
    (!serial.trim().is_empty()).then_some(serial)
}

/// Decode the mandatory block 1 (HR 0–59). Block 2 contains supplementary
/// session and voltage fields, so a valid block 1 still provides a useful
/// charger snapshot when that optional read fails.
fn decode_evc_block1(regs: &[u16]) -> EvcSnapshot {
    if regs.len() < 60 {
        return EvcSnapshot::default();
    }

    EvcSnapshot {
        charging_state: decode_charging_state(regs[0]),
        connection_status: decode_connection_status(regs[2]),
        active_power: regs[13] as i32,
        current_l1: regs[6] as f32 / 10.0,
        current_l2: regs[8] as f32 / 10.0,
        current_l3: regs[10] as f32 / 10.0,
        meter_energy_kwh: regs[29] as f32 / 10.0,
        charge_limit_a: regs[36] as f32 / 10.0,
        serial_number: decode_serial(&regs[38..60]),
        ..Default::default()
    }
}

/// Decode two register blocks (60 + 55) into an `EvcSnapshot`.
fn decode_evc(regs: &[u16]) -> EvcSnapshot {
    // regs[0..60] = block 1, regs[60..115] = block 2
    if regs.len() < 60 {
        tracing::warn!(
            "EVC: short block-1 register read ({} regs, expected 60)",
            regs.len()
        );
        return EvcSnapshot::default();
    }

    let mut snapshot = decode_evc_block1(regs);
    if regs.len() < 115 {
        tracing::warn!(
            "EVC: short optional block-2 register read ({} regs, expected 115)",
            regs.len()
        );
        return snapshot;
    }

    // The serial spans the end of block 1 and the start of block 2.
    snapshot
        .serial_number
        .push_str(&decode_serial(&regs[60..69]));
    snapshot.session_energy_kwh = regs[72] as f32 / 10.0;
    snapshot.session_duration_secs = regs[79] as u32;
    snapshot.voltage_l1 = regs[109] as f32 / 10.0;
    snapshot.voltage_l2 = regs[111] as f32 / 10.0;
    snapshot.voltage_l3 = regs[113] as f32 / 10.0;
    snapshot
}

// ---------------------------------------------------------------------------
// Discovery — scan for Modbus devices on port 502
// ---------------------------------------------------------------------------

/// The default Modbus TCP port used by GivEnergy EV chargers.
const EVC_MODBUS_PORT: u16 = 502;

/// A discovered EV charger.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredEvc {
    pub ip: String,
    pub port: u16,
    /// Serial number if decoded during probe.
    pub serial: Option<String>,
}

/// Scan a /24 subnet for devices responding to standard Modbus TCP on port 502.
///
/// Each host is probed by reading the EVC serial identity at HR 38–68. Only a
/// valid FC3 response with printable serial data is reported.
pub async fn scan_evc_subnet(subnet_base: &str) -> Vec<DiscoveredEvc> {
    tracing::info!("EVC scan: {}.x:{}", subnet_base, EVC_MODBUS_PORT);

    let mut tasks = Vec::new();
    for host in 1..255u8 {
        let ip = format!("{}.{}", subnet_base, host);
        tasks.push(probe_evc_host(ip));
    }

    let results = futures_util::future::join_all(tasks).await;
    let found: Vec<_> = results.into_iter().flatten().collect();

    tracing::info!(
        "EVC scan: {}.x found {} charger(s): {}",
        subnet_base,
        found.len(),
        found
            .iter()
            .map(|d| format!("{}:{}", d.ip, d.port))
            .collect::<Vec<_>>()
            .join(", "),
    );
    found
}

/// Scan multiple subnets for EV chargers.
pub async fn scan_evc_multiple_subnets(subnets: &[String]) -> Vec<DiscoveredEvc> {
    let mut all = Vec::new();
    for subnet in subnets {
        all.extend(scan_evc_subnet(subnet).await);
    }
    all
}

/// Probe a single IP for a GivEVC on standard Modbus TCP port 502.
///
/// Connects, reads the EVC serial identity from HR 38–68, and only reports
/// a device when the response is a valid FC3 frame containing printable
/// serial data. This avoids treating an arbitrary FC3 responder as a charger.
async fn probe_evc_host(ip: String) -> Option<DiscoveredEvc> {
    let addr = format!("{}:{}", ip, EVC_MODBUS_PORT);
    let ip_clone = ip.clone();

    let result = tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        // Step 1: TCP connect
        let mut stream =
            TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_millis(800)).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .ok()?;

        // Read the 31-register serial field (HR 38–68), which spans the end
        // of the normal EVC blocks and provides the discovery identity.
        let request: [u8; 12] = [
            0x00, 0x01, // Transaction ID
            0x00, 0x00, // Protocol (Modbus)
            0x00, 0x06, // Length
            0x01, // Unit ID (slave address)
            0x03, // Function code: Read Holding Registers
            0x00, 0x26, // Start address: 38
            0x00, 0x1F, // Quantity: 31 registers
        ];
        stream.write_all(&request).ok()?;

        // Read and validate the complete MBAP + FC3 response before decoding
        // the serial registers.
        let mut header = [0u8; 7];
        stream.read_exact(&mut header).ok()?;
        let txn = u16::from_be_bytes([header[0], header[1]]);
        let proto = u16::from_be_bytes([header[2], header[3]]);
        let length = u16::from_be_bytes([header[4], header[5]]);
        let unit = header[6];
        // MBAP length includes unit id; the remaining PDU is FC + byte count
        // + 31 register values = 64 bytes.
        if txn != 0x0001 || proto != 0x0000 || unit != 1 || length != 65 {
            return None;
        }
        let mut pdu = [0u8; 64];
        stream.read_exact(&mut pdu).ok()?;
        if pdu[0] != 0x03 || pdu[1] != 62 {
            return None;
        }

        let regs: Vec<u16> = pdu[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        decode_discovery_serial(&regs)
    })
    .await;

    if let Ok(Some(serial)) = result {
        tracing::debug!(
            "EVC: found Modbus device at {}:{}",
            ip_clone,
            EVC_MODBUS_PORT
        );
        Some(DiscoveredEvc {
            ip: ip_clone,
            port: EVC_MODBUS_PORT,
            serial: Some(serial),
        })
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Session-energy latch (issue #189)
// ---------------------------------------------------------------------------

/// In-memory latch for the EV charger session-energy display.
///
/// HR 72 (`Charge_Session_Energy`) drops to 0 the moment a charging session
/// ends, so a naive display would flash "0.0 kWh" the instant the charge
/// stops and stay there until the next session. To keep the completed
/// session's total visible on the diagram, we hold the last non-zero
/// reading and only clear it when the physical cable transitions
/// "No Cable" → "Cable In" — i.e. a genuinely new session has started.
///
/// The rule (confirmed with the project owner):
///  - While charging (`Charging` / `Starting`): trust the live register,
///    even if it briefly reads 0 (dongle jitter / very start of a session).
///  - Otherwise: show the latched peak, or 0.0 if no session has delivered
///    energy since the last cable plug-in.
///  - On the "No Cable" → "Cable In" transition: clear the latch so the new
///    session starts from a clean 0. A "Cable In" → "No Cable" unplug does
///    *not* clear it — the last session's total stays visible until the
///    cable is plugged back in.
///  - Any non-zero reading refreshes the latch, so the peak survives the
///    end-of-session zeroing of HR 72 (and the brief window where the EVC
///    reports the final value under a non-charging state before zeroing).
///
/// Mirrors the intent of GivTCP's `evc.py` "hold the previous charge
/// session energy" cache (`evc.py:231`), but ties the reset to the cable
/// transition rather than leaving the value pinned indefinitely.
///
/// Volatile: not persisted to disk. On backend restart the latch starts
/// empty and repopulates from the next charging session — matching the
/// behaviour of the rest of the EVC state on `AppState` (`latest_evc`).
#[derive(Debug, Clone, Default)]
pub struct SessionLatch {
    /// Whether the cable was connected on the previous poll. `None` before
    /// the first observation, so we don't mistake the initial reading for a
    /// plug-in transition.
    prev_cable: Option<bool>,
    /// The last non-zero session energy seen, in kWh. Cleared on the
    /// "No Cable" → "Cable In" transition.
    latched_kwh: Option<f32>,
    /// Whether a raw duration value has been observed for the current cable
    /// session. HR 79 is a 16-bit seconds counter, so it wraps after 65,535.
    duration_raw_secs: Option<u32>,
    /// Number of complete 16-bit counter cycles preceding the raw value.
    duration_offset_secs: u64,
    /// Last unwrapped duration, held while the cable is unplugged or the
    /// charger briefly reports zero after a session ends.
    duration_secs: u64,
}

impl SessionLatch {
    /// Apply the latch rule to a freshly decoded snapshot and return the
    /// effective session energy (in kWh) the frontend should display.
    ///
    /// Call this with the **raw** decoded `session_energy_kwh` (before any
    /// override), then write the returned value back into the snapshot so
    /// the broadcast, the `latest_evc` cache, and `GET /api/evc/status` all
    /// carry the display value.
    pub fn observe(&mut self, snapshot: &EvcSnapshot) -> f32 {
        self.observe_with_duration(snapshot).0
    }

    /// Apply the energy latch and unwrap the 16-bit session-duration counter
    /// in one state transition. The pair is used by the publisher so both
    /// values are derived from the same cable/session observation.
    pub fn observe_with_duration(&mut self, snapshot: &EvcSnapshot) -> (f32, u32) {
        let curr_cable = snapshot.connection_status == "Connected";
        let curr_kwh = snapshot.session_energy_kwh;
        let charging = matches!(snapshot.charging_state.as_str(), "Charging" | "Starting");

        // Detect the cable plug-in transition. Skip on the very first
        // observation (prev_cable is None) so an already-plugged-in cable
        // at startup doesn't clobber a latch captured from a prior session.
        if matches!(self.prev_cable, Some(false)) && curr_cable {
            self.latched_kwh = None;
            self.duration_raw_secs = None;
            self.duration_offset_secs = 0;
            self.duration_secs = 0;
        }

        // Any non-zero reading refreshes the latch (covers the live ramp-up
        // during charging AND the brief post-session frame where the EVC
        // reports the final value before zeroing HR 72).
        if curr_kwh > 0.0 {
            self.latched_kwh = Some(curr_kwh);
        }

        // HR 79 is a u16 seconds counter. Ignore the post-session zero while
        // idle, but unwrap a genuine large backwards jump while connected.
        if curr_cable && (charging || snapshot.session_duration_secs > 0) {
            if let Some(previous) = self.duration_raw_secs {
                if snapshot.session_duration_secs < previous {
                    if previous - snapshot.session_duration_secs > u32::from(u16::MAX) / 2 {
                        self.duration_offset_secs = self
                            .duration_offset_secs
                            .saturating_add(u64::from(u16::MAX) + 1);
                    } else {
                        // A small backwards jump is a new/reset session on a
                        // cable that stayed connected, not a counter wrap.
                        self.duration_offset_secs = 0;
                    }
                }
            }
            self.duration_raw_secs = Some(snapshot.session_duration_secs);
            self.duration_secs = self
                .duration_offset_secs
                .saturating_add(u64::from(snapshot.session_duration_secs));
        }

        self.prev_cable = Some(curr_cable);

        let effective_kwh = if charging {
            // Trust the live register while charging — even a 0 read (dongle
            // jitter, or the very first sub-0.05 kWh poll that rounds to 0).
            curr_kwh
        } else {
            // Session over (or never started): hold the peak, or 0.0 if no
            // energy has been delivered since the last cable plug-in.
            self.latched_kwh.unwrap_or(0.0)
        };
        (
            effective_kwh,
            self.duration_secs.min(u64::from(u32::MAX)) as u32,
        )
    }
}

// ---------------------------------------------------------------------------
// Poll loop
// ---------------------------------------------------------------------------

/// Connect timeout for the EVC poll loop's Modbus TCP handshake.
///
/// Review H12: `tcp::connect_slave` had no deadline, so a blackholed host
/// blocked the loop forever while `/api/evc/status` kept serving a frozen
/// snapshot as `reachable: true`. 3 s matches the inverter client's
/// per-request budget; `probe_evc_host` uses a tighter 800 ms budget for
/// subnet scans, which this loop doesn't need.
const EVC_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Per-read timeout for the EVC poll loop's register reads. Same rationale
/// as [`EVC_CONNECT_TIMEOUT`] — a charger that accepts and then goes silent
/// must drop the connection, not freeze the poll loop inside one read.
const EVC_READ_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) async fn record_evc_success(state: &Arc<AppState>) {
    let status = {
        let now_ms = current_epoch_ms();
        let mut reachability = state.evc_reachability.lock().await;
        reachability.record_success(now_ms);
        reachability.status_at(now_ms)
    };
    let _ = state.tx.send(PollMessage::EvcStatus(Box::new(status)));
}

pub(crate) async fn record_evc_failure(state: &Arc<AppState>) {
    let status = {
        let now_ms = current_epoch_ms();
        let mut reachability = state.evc_reachability.lock().await;
        reachability.record_failure(now_ms);
        reachability.status_at(now_ms)
    };
    if !status.reachable {
        let mut evc = state.latest_evc.lock().await;
        *evc = None;
    }
    let _ = state.tx.send(PollMessage::EvcDisconnected);
    let _ = state.tx.send(PollMessage::EvcStatus(Box::new(status)));
}

async fn publish_evc_snapshot(state: &Arc<AppState>, mut snapshot: EvcSnapshot) {
    // Apply the session-energy latch (issue #189). `observe` reads the raw
    // HR 72 value; overwrite the snapshot field so the broadcast, cache and
    // API all carry the same display value.
    let (effective_kwh, effective_duration_secs) = {
        let mut latch = state.evc_session_latch.lock().await;
        latch.observe_with_duration(&snapshot)
    };
    snapshot.session_energy_kwh = effective_kwh;
    snapshot.session_duration_secs = effective_duration_secs;

    {
        let mut evc = state.latest_evc.lock().await;
        *evc = Some(snapshot.clone());
    }
    let _ = state.tx.send(PollMessage::Evc(Box::new(snapshot)));
    record_evc_success(state).await;
}

pub(crate) async fn reset_evc_state(state: &Arc<AppState>) {
    {
        let mut reachability = state.evc_reachability.lock().await;
        reachability.reset();
    }
    {
        let mut evc = state.latest_evc.lock().await;
        *evc = None;
    }
    let status = {
        let reachability = state.evc_reachability.lock().await;
        reachability.status_at(current_epoch_ms())
    };
    let _ = state.tx.send(PollMessage::EvcDisconnected);
    let _ = state.tx.send(PollMessage::EvcStatus(Box::new(status)));
}

/// Background poll loop for the EV charger. Reads settings from the shared
/// `AppState` to determine the EVC host/port. When configured, polls via
/// standard Modbus TCP every 10 seconds and broadcasts `PollMessage::Evc`
/// to all WebSocket clients.
pub async fn run_evc_poll_loop(state: Arc<AppState>) {
    run_evc_poll_loop_with_timeouts(state, EVC_CONNECT_TIMEOUT, EVC_READ_TIMEOUT).await;
}

/// Inner poll loop with injectable connect/read timeouts so tests can
/// exercise the blackhole paths in milliseconds instead of seconds.
pub(crate) async fn run_evc_poll_loop_with_timeouts(
    state: Arc<AppState>,
    connect_timeout: Duration,
    read_timeout: Duration,
) {
    let mut backoff = Duration::from_secs(10);
    let poll_interval = Duration::from_secs(10);

    loop {
        // ---- Read EVC settings ----
        let (evc_host, evc_port) = {
            let s = state.settings.lock().await;
            (s.evc_host.clone(), s.evc_port)
        };

        if evc_host.is_empty() {
            // No EVC configured — sleep and check again later.
            sleep(Duration::from_secs(15)).await;
            continue;
        }

        tracing::info!(host = %evc_host, port = evc_port, "EVC: connecting");

        // ---- Connect via standard Modbus TCP ----
        let socket_addr = format!("{evc_host}:{evc_port}");
        let addr: std::net::SocketAddr = match socket_addr.parse() {
            Ok(a) => a,
            Err(e) => {
                // Host string is unparseable (issue #138: typo like "10.1.71"
                // instead of "10.1.1.71"). Without broadcasting, the frontend
                // sits on the Zustand defaults forever and shows a misleading
                // "Disconnected" label. Clear cached snapshot and emit
                // EvcDisconnected so the UI can render an honest state, then
                // back off. The frontend SettingsPage already blocks saving
                // bad hosts, but this also covers hand-edited settings.json
                // and old clients that pre-date the validator.
                tracing::warn!("EVC: invalid address '{socket_addr}': {e}");
                record_evc_failure(&state).await;
                sleep(backoff).await;
                continue;
            }
        };

        let connect_result =
            tokio::time::timeout(connect_timeout, tcp::connect_slave(addr, Slave(1))).await;
        let ctx = match connect_result {
            Ok(Ok(ctx)) => {
                tracing::info!(host = %evc_host, "EVC: connected");
                backoff = Duration::from_secs(10);
                // Broadcast a connect event immediately so the frontend can
                // latch "we've reached the host" without waiting for the
                // first successful register read (issue #138). If the
                // first read fails and we drop back to EvcDisconnected, the
                // latch will be cleared by resetEvc() on the next save —
                // in the meantime the UI shows an honest "Connected"
                // rather than the misleading "Not Found".
                let _ = state.tx.send(PollMessage::EvcConnected);
                ctx
            }
            Err(_) => {
                tracing::warn!(
                    "EVC: connect timed out after {connect_timeout:?} — host unreachable or blackholed"
                );
                record_evc_failure(&state).await;
                sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(120));
                continue;
            }
            Ok(Err(e)) => {
                tracing::warn!("EVC: connect failed: {e}");
                record_evc_failure(&state).await;
                sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(120));
                continue;
            }
        };

        // We need to hold the context in a mutable container since
        // tokio-modbus reads require `&mut`. Wrap in an Option so we can
        // take it out on error.
        let mut ctx = Some(ctx);

        // ---- Polling loop ----
        loop {
            // Re-check settings in case EVC was disabled or changed.
            let (h, p) = {
                let s = state.settings.lock().await;
                (s.evc_host.clone(), s.evc_port)
            };
            if h.is_empty() {
                tracing::info!("EVC: host cleared, stopping poll");
                ctx.take();
                break;
            }
            if h != evc_host || p != evc_port {
                tracing::info!("EVC: settings changed, reconnecting");
                ctx.take();
                break;
            }

            if ctx.is_none() {
                break; // reconnect outer loop
            }

            // Read block 1: HR 0–59
            let result1 = tokio::time::timeout(
                read_timeout,
                ctx.as_mut().unwrap().read_holding_registers(0x0000, 60),
            )
            .await;
            let regs1 = match result1 {
                Ok(Ok(Ok(r))) => r,
                Ok(Ok(Err(e))) => {
                    tracing::warn!("EVC: Modbus exception reading HR 0–59: {e:?}");
                    ctx.take();
                    break;
                }
                Ok(Err(e)) => {
                    tracing::warn!("EVC: read error HR 0–59: {e}");
                    ctx.take();
                    break;
                }
                Err(_) => {
                    tracing::warn!("EVC: read timed out after {read_timeout:?} on HR 0–59");
                    ctx.take();
                    break;
                }
            };

            // Read block 2: HR 60–114
            let result2 = tokio::time::timeout(
                read_timeout,
                ctx.as_mut().unwrap().read_holding_registers(60, 55),
            )
            .await;
            let regs2 = match result2 {
                Ok(Ok(Ok(r))) => r,
                Ok(Ok(Err(e))) => {
                    tracing::warn!("EVC: Modbus exception reading HR 60–114: {e:?}");
                    Vec::new()
                }
                Ok(Err(e)) => {
                    tracing::warn!("EVC: read error HR 60–114: {e}");
                    Vec::new()
                }
                Err(_) => {
                    tracing::warn!("EVC: read timed out after {read_timeout:?} on HR 60–114");
                    Vec::new()
                }
            };

            if regs2.is_empty() {
                // Block 1 is the mandatory live-state block. Keep its valid
                // snapshot visible when supplementary block 2 is unavailable
                // and retry the optional read on the next poll.
                let mut snapshot = decode_evc_block1(&regs1);
                // Preserve supplementary values while the optional block is
                // temporarily unavailable; otherwise a charging session
                // flashes back to zero for one poll.
                if let Some(previous) = state.latest_evc.lock().await.clone() {
                    snapshot.session_energy_kwh = previous.session_energy_kwh;
                    snapshot.session_duration_secs = previous.session_duration_secs;
                    snapshot.voltage_l1 = previous.voltage_l1;
                    snapshot.voltage_l2 = previous.voltage_l2;
                    snapshot.voltage_l3 = previous.voltage_l3;
                    if snapshot.serial_number.len() < previous.serial_number.len() {
                        snapshot.serial_number = previous.serial_number;
                    }
                }
                tracing::debug!(
                    power = snapshot.active_power,
                    state = %snapshot.charging_state,
                    "EVC: retaining valid block-1 snapshot after block-2 failure"
                );
                publish_evc_snapshot(&state, snapshot).await;
                sleep(poll_interval).await;
                continue;
            }

            // Combine and decode
            let mut regs = regs1;
            regs.extend_from_slice(&regs2);

            let snapshot = decode_evc(&regs);

            tracing::debug!(
                power = snapshot.active_power,
                state = %snapshot.charging_state,
                connection = %snapshot.connection_status,
                hr0 = regs[0],
                hr2 = regs[2],
                hr13 = regs[13],
                hr29 = regs[29],
                "EVC: polled"
            );

            publish_evc_snapshot(&state, snapshot).await;

            sleep(poll_interval).await;
        }

        // Context dropped — reconnect after backoff
        tracing::warn!("EVC: connection lost, reconnecting in {:?}", backoff);
        record_evc_failure(&state).await;
        sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(120));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // Enum decoders
    // -----------------------------------------------------------------

    #[test]
    fn decode_charging_state_known_values() {
        // Every entry in the table must round-trip.
        let cases = [
            (0, "Unknown"),
            (1, "Idle"),
            (2, "Connected"),
            (3, "Starting"),
            (4, "Charging"),
            (5, "Startup Failure"),
            (6, "End of Charging"),
            (7, "System Failure"),
            (8, "Scheduled"),
            (9, "Updating"),
            (10, "Unstable CP"),
        ];
        for (val, expected) in cases {
            assert_eq!(
                decode_charging_state(val),
                expected,
                "value {val} should decode to {expected}"
            );
        }
    }

    #[test]
    fn decode_charging_state_unknown_value_falls_back() {
        // 11 is past the end of the table — must return "Unknown", not panic.
        assert_eq!(decode_charging_state(11), "Unknown");
        assert_eq!(decode_charging_state(255), "Unknown");
        assert_eq!(decode_charging_state(u16::MAX), "Unknown");
    }

    #[test]
    fn decode_connection_status_known_values() {
        assert_eq!(decode_connection_status(0), "Not Connected");
        assert_eq!(decode_connection_status(1), "Connected");
    }

    #[test]
    fn decode_connection_status_unknown_falls_back() {
        assert_eq!(decode_connection_status(2), "Unknown");
        assert_eq!(decode_connection_status(99), "Unknown");
    }

    // -----------------------------------------------------------------
    // EvcSnapshot::default
    // -----------------------------------------------------------------

    #[test]
    fn evc_snapshot_default_is_zero_and_unknown() {
        let s = EvcSnapshot::default();
        assert_eq!(s.charging_state, "Unknown");
        assert_eq!(s.connection_status, "Unknown");
        assert_eq!(s.active_power, 0);
        assert_eq!(s.current_l1, 0.0);
        assert_eq!(s.current_l2, 0.0);
        assert_eq!(s.current_l3, 0.0);
        assert_eq!(s.voltage_l1, 0.0);
        assert_eq!(s.voltage_l2, 0.0);
        assert_eq!(s.voltage_l3, 0.0);
        assert_eq!(s.meter_energy_kwh, 0.0);
        assert_eq!(s.session_energy_kwh, 0.0);
        assert_eq!(s.session_duration_secs, 0);
        assert_eq!(s.charge_limit_a, 0.0);
        assert!(s.serial_number.is_empty());
    }

    #[test]
    fn evc_snapshot_serializes_to_json() {
        let s = EvcSnapshot::default();
        let json = serde_json::to_string(&s).expect("serialise");
        // The struct uses default serde field naming (snake_case).
        // The frontend's TypeScript layer is responsible for the
        // camelCase mapping — this test pins the wire format so
        // a rename in the struct causes a test failure.
        for key in [
            "charging_state",
            "connection_status",
            "active_power",
            "current_l1",
            "voltage_l1",
            "meter_energy_kwh",
            "session_energy_kwh",
            "session_duration_secs",
            "charge_limit_a",
            "serial_number",
        ] {
            assert!(json.contains(key), "missing key {key} in {json}");
        }
    }

    // -----------------------------------------------------------------
    // decode_evc
    // -----------------------------------------------------------------

    /// Build a 115-register test vector with all fields zeroed.
    fn zero_regs() -> Vec<u16> {
        vec![0u16; 115]
    }

    #[test]
    fn decode_evc_short_register_buffer_returns_default() {
        // Anything shorter than block 1 must NOT panic; a complete block 1
        // is useful on its own and leaves optional block-2 fields at default.
        let snapshot = decode_evc(&[]);
        assert_eq!(snapshot.charging_state, "Unknown");
        assert_eq!(snapshot.connection_status, "Unknown");
        assert_eq!(snapshot.active_power, 0);

        let snapshot = decode_evc(&[0u16; 60]);
        assert_eq!(snapshot.charging_state, "Unknown");
        assert_eq!(snapshot.connection_status, "Not Connected");

        let snapshot = decode_evc(&[0u16; 114]);
        assert_eq!(snapshot.charging_state, "Unknown");
        assert_eq!(snapshot.connection_status, "Not Connected");
        // Last valid index is 114, so voltages at 109/111/113 should be 0.
        assert_eq!(snapshot.voltage_l1, 0.0);
    }

    #[test]
    fn decode_evc_charging_and_connection_states() {
        let mut regs = zero_regs();
        regs[0] = 4; // Charging
        regs[2] = 1; // Connected
        let s = decode_evc(&regs);
        assert_eq!(s.charging_state, "Charging");
        assert_eq!(s.connection_status, "Connected");
    }

    #[test]
    fn decode_evc_block1_preserves_snapshot_without_optional_block2() {
        let mut regs = vec![0u16; 60];
        regs[0] = 4; // Charging
        regs[2] = 1; // Connected
        regs[6] = 160; // 16.0 A
        regs[13] = 7400; // 7400 W
        regs[29] = 12345; // 1234.5 kWh meter
        regs[36] = 32; // 3.2 A
        regs[38..45].copy_from_slice(&[
            b'G' as u16,
            b'E' as u16,
            b'V' as u16,
            b'C' as u16,
            b'1' as u16,
            b'2' as u16,
            b'3' as u16,
        ]);

        let snapshot = decode_evc_block1(&regs);

        assert_eq!(snapshot.charging_state, "Charging");
        assert_eq!(snapshot.connection_status, "Connected");
        assert_eq!(snapshot.active_power, 7400);
        assert!((snapshot.current_l1 - 16.0).abs() < 0.01);
        assert!((snapshot.meter_energy_kwh - 1234.5).abs() < 0.01);
        assert!((snapshot.charge_limit_a - 3.2).abs() < 0.01);
        assert_eq!(snapshot.serial_number, "GEVC123");
        // Block 2 was unavailable, so its fields remain safe defaults.
        assert_eq!(snapshot.session_energy_kwh, 0.0);
        assert_eq!(snapshot.voltage_l1, 0.0);
    }

    #[test]
    fn decode_evc_currents_divide_by_ten() {
        let mut regs = zero_regs();
        regs[6] = 160; // 16.0 A
        regs[8] = 155; // 15.5 A
        regs[10] = 32; // 3.2 A
        let s = decode_evc(&regs);
        assert!((s.current_l1 - 16.0).abs() < 0.01);
        assert!((s.current_l2 - 15.5).abs() < 0.01);
        assert!((s.current_l3 - 3.2).abs() < 0.01);
    }

    #[test]
    fn decode_evc_voltages_divide_by_ten() {
        let mut regs = zero_regs();
        regs[109] = 2354; // 235.4 V
        regs[111] = 2360; // 236.0 V
        regs[113] = 2349; // 234.9 V
        let s = decode_evc(&regs);
        assert!((s.voltage_l1 - 235.4).abs() < 0.01);
        assert!((s.voltage_l2 - 236.0).abs() < 0.01);
        assert!((s.voltage_l3 - 234.9).abs() < 0.01);
    }

    #[test]
    fn decode_evc_active_power_and_energy() {
        let mut regs = zero_regs();
        regs[13] = 7400; // 7400 W
        regs[29] = 12345; // 1234.5 kWh meter
        regs[72] = 567; // 56.7 kWh session
        regs[79] = 3600; // 1 hour
        let s = decode_evc(&regs);
        assert_eq!(s.active_power, 7400);
        assert!((s.meter_energy_kwh - 1234.5).abs() < 0.01);
        assert!((s.session_energy_kwh - 56.7).abs() < 0.01);
        assert_eq!(s.session_duration_secs, 3600);
    }

    #[test]
    fn decode_evc_charge_limit_divide_by_ten() {
        let mut regs = zero_regs();
        regs[36] = 32; // 3.2 A
        let s = decode_evc(&regs);
        assert!((s.charge_limit_a - 3.2).abs() < 0.01);
    }

    #[test]
    fn decode_evc_serial_number_skips_nulls() {
        // "GEVC123" = [0x47, 0x45, 0x56, 0x43, 0x31, 0x32, 0x33] then nulls
        let mut regs = zero_regs();
        let serial = b"GEVC123";
        for (i, b) in serial.iter().enumerate() {
            regs[38 + i] = *b as u16;
        }
        let s = decode_evc(&regs);
        assert_eq!(s.serial_number, "GEVC123");
    }

    #[test]
    fn decode_evc_serial_number_full_buffer_with_trailing_nulls() {
        // Simulate a fully populated serial at HR 38..69 (31 chars) with
        // nulls only at the end. Loop terminates at the first null.
        let mut regs = zero_regs();
        let serial = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ12345";
        for (i, b) in serial.iter().enumerate() {
            regs[38 + i] = *b as u16;
        }
        let s = decode_evc(&regs);
        assert_eq!(s.serial_number, "ABCDEFGHIJKLMNOPQRSTUVWXYZ12345");
    }

    #[test]
    fn decode_evc_serial_number_invalid_utf16_replaced_with_question_mark() {
        // char::from_u32 returns None for some code points; the decoder
        // must substitute '?' rather than panic.
        let mut regs = zero_regs();
        regs[38] = 0xD800; // surrogate — invalid as a scalar value
        let s = decode_evc(&regs);
        assert_eq!(s.serial_number, "?");
    }

    #[test]
    fn discovery_requires_a_printable_evc_serial() {
        let mut regs = vec![0u16; 31];
        regs[..7].copy_from_slice(&[
            b'G' as u16,
            b'E' as u16,
            b'V' as u16,
            b'C' as u16,
            b'1' as u16,
            b'2' as u16,
            b'3' as u16,
        ]);
        assert_eq!(decode_discovery_serial(&regs), Some("GEVC123".to_string()));
        assert_eq!(decode_discovery_serial(&[0; 31]), None);

        regs[0] = 0x01;
        assert_eq!(decode_discovery_serial(&regs), None);
        assert_eq!(decode_discovery_serial(&regs[..30]), None);
    }

    // -----------------------------------------------------------------
    // SessionLatch (issue #189)
    //
    // The latch keeps a completed session's kWh on the diagram after HR 72
    // zeroes, and resets only on the physical cable "No Cable" → "Cable In"
    // transition. These tests walk every documented branch of the rule.
    // -----------------------------------------------------------------

    /// Build a snapshot with just the fields `SessionLatch::observe` reads.
    fn snap(charging_state: &str, connection_status: &str, session_kwh: f32) -> EvcSnapshot {
        EvcSnapshot {
            charging_state: charging_state.into(),
            connection_status: connection_status.into(),
            session_energy_kwh: session_kwh,
            ..Default::default()
        }
    }

    #[test]
    fn latch_shows_zero_before_any_session() {
        // Fresh latch, charger idle, no cable, no energy. The diagram
        // should read 0.0 kWh, not NaN or a stale value.
        let mut latch = SessionLatch::default();
        let eff = latch.observe(&snap("Idle", "Not Connected", 0.0));
        assert!((eff - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn latch_tracks_live_energy_while_charging() {
        // Three successive polls during a charge: 0 → 0.5 → 1.2 kWh.
        // The effective value always equals the live register while
        // charging (even the initial 0).
        let mut latch = SessionLatch::default();
        let a = latch.observe(&snap("Charging", "Connected", 0.0));
        let b = latch.observe(&snap("Charging", "Connected", 0.5));
        let c = latch.observe(&snap("Charging", "Connected", 1.2));
        assert!((a - 0.0).abs() < f32::EPSILON);
        assert!((b - 0.5).abs() < 0.001);
        assert!((c - 1.2).abs() < 0.001);
    }

    #[test]
    fn latch_holds_peak_after_session_ends_and_hr72_zeroes() {
        // Charge to 10.0 kWh, then HR 72 zeroes as the EVC declares the
        // session over (state flips to Idle). The display must keep
        // showing 10.0 kWh — that's the whole point of the latch.
        let mut latch = SessionLatch::default();
        latch.observe(&snap("Charging", "Connected", 10.0));
        let eff = latch.observe(&snap("Idle", "Connected", 0.0));
        assert!((eff - 10.0).abs() < 0.001, "should hold peak, got {eff}");
    }

    #[test]
    fn latch_holds_peak_across_unplug_while_idle() {
        // "Cable In" → "No Cable" must NOT reset the latch. The user can
        // unplug the car and the last session's total stays visible until
        // they plug in again.
        let mut latch = SessionLatch::default();
        latch.observe(&snap("Charging", "Connected", 7.5));
        latch.observe(&snap("Idle", "Connected", 0.0));
        let eff = latch.observe(&snap("Idle", "Not Connected", 0.0));
        assert!(
            (eff - 7.5).abs() < 0.001,
            "unplug must not reset, got {eff}"
        );
    }

    #[test]
    fn latch_resets_on_cable_plug_in_transition() {
        // Full cycle: charge → end → unplug → plug back in. The plug-in
        // transition clears the latch, so the new session starts from 0
        // (not the previous session's 7.5 kWh).
        let mut latch = SessionLatch::default();
        latch.observe(&snap("Charging", "Connected", 7.5));
        latch.observe(&snap("Idle", "Connected", 0.0));
        latch.observe(&snap("Idle", "Not Connected", 0.0));
        let eff = latch.observe(&snap("Idle", "Connected", 0.0));
        assert!(
            (eff - 0.0).abs() < f32::EPSILON,
            "plug-in should reset, got {eff}"
        );
    }

    #[test]
    fn latch_does_not_reset_on_first_observation_with_cable_in() {
        // App started mid-session (cable already in, charging). The first
        // observation must NOT be treated as a plug-in transition, so the
        // live value is captured and the latch is primed for after the
        // session ends.
        let mut latch = SessionLatch::default();
        let eff = latch.observe(&snap("Charging", "Connected", 5.0));
        assert!((eff - 5.0).abs() < 0.001);
        // Session ends → latch should hold the 5.0 captured above.
        let held = latch.observe(&snap("Idle", "Connected", 0.0));
        assert!(
            (held - 5.0).abs() < 0.001,
            "first-obs capture should persist, got {held}"
        );
    }

    #[test]
    fn latch_trusts_live_zero_during_charging_jitter() {
        // Dongle jitter: a 0 read arrives while still Charging. We show
        // the live 0 (not the latched peak) because the user's spec says
        // the live register wins while charging — even a spurious 0.
        let mut latch = SessionLatch::default();
        latch.observe(&snap("Charging", "Connected", 5.0));
        let eff = latch.observe(&snap("Charging", "Connected", 0.0));
        assert!(
            (eff - 0.0).abs() < f32::EPSILON,
            "charging should trust live register, got {eff}"
        );
        // But the latch itself is untouched, so once the jitter clears the
        // peak is still recoverable after the session.
        let held = latch.observe(&snap("Idle", "Connected", 0.0));
        assert!(
            (held - 5.0).abs() < 0.001,
            "latch should survive jitter, got {held}"
        );
    }

    #[test]
    fn latch_captures_final_value_reported_under_non_charging_state() {
        // Some EVCs report the final kWh for one poll under a non-charging
        // state (e.g. "End of Charging") before zeroing HR 72. That reading
        // should refresh the latch so it's the value we hold afterwards.
        let mut latch = SessionLatch::default();
        latch.observe(&snap("Charging", "Connected", 9.8));
        let eff = latch.observe(&snap("End of Charging", "Connected", 9.9));
        assert!((eff - 9.9).abs() < 0.001);
        let held = latch.observe(&snap("Idle", "Connected", 0.0));
        assert!(
            (held - 9.9).abs() < 0.001,
            "should hold the refreshed peak, got {held}"
        );
    }

    #[test]
    fn latch_treats_starting_as_charging() {
        // `Starting` (state=3) is the brief pre-charge ramp-up. It should
        // follow the live register like `Charging`, not latch-and-hold.
        let mut latch = SessionLatch::default();
        let eff = latch.observe(&snap("Starting", "Connected", 0.0));
        assert!((eff - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn latch_unwraps_duration_counter_past_u16_limit() {
        let mut latch = SessionLatch::default();
        let mut before_wrap = snap("Charging", "Connected", 0.0);
        before_wrap.session_duration_secs = 65_530;
        let (_, duration_before_wrap) = latch.observe_with_duration(&before_wrap);
        assert_eq!(duration_before_wrap, 65_530);

        let mut after_wrap = snap("Charging", "Connected", 0.0);
        after_wrap.session_duration_secs = 3;
        let (_, duration_after_wrap) = latch.observe_with_duration(&after_wrap);
        assert_eq!(duration_after_wrap, 65_539);
    }

    // -----------------------------------------------------------------
    // Scan: empty subnet returns empty list
    //
    // We can't test the real network probe without a live Modbus server
    // or a root-raw socket, but we can verify the function signature
    // and the empty input case for `scan_evc_multiple_subnets`.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn scan_evc_multiple_subnets_empty_input() {
        let result = scan_evc_multiple_subnets(&[]).await;
        assert!(result.is_empty());
    }

    // -----------------------------------------------------------------
    // run_evc_poll_loop: no-host path
    //
    // With evc_host unset, the loop must sleep and check again, never
    // touching `latest_evc` or sending any message. We let it run for
    // a short window and confirm no message was sent and no panic.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn run_evc_poll_loop_silently_sleeps_when_no_host() {
        use crate::inverter::poll::AppState;

        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            // Confirm isolated default settings have no EVC host.
            {
                let s = state.settings.lock().await;
                assert!(s.evc_host.is_empty(), "default evc_host should be empty");
            }

            let state_clone = state.clone();
            let handle = tokio::spawn(async move {
                tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    run_evc_poll_loop(state_clone),
                )
                .await
            });

            let result = handle.await.expect("join");
            assert!(
                result.is_err(),
                "poll loop should still be sleeping at 2s when no host is configured"
            );

            let evc = state.latest_evc.lock().await;
            assert!(evc.is_none(), "no EVC snapshot should be cached");
        })
        .await;
    }

    // -----------------------------------------------------------------
    // run_evc_poll_loop: charger stops answering (review H12)
    //
    // A charger that completes the TCP handshake and then never responds
    // (lost power without RST, blackholed host) must not wedge the loop
    // forever. The read must time out, the frozen snapshot must be
    // dropped from `latest_evc` and EvcDisconnected must be broadcast so
    // `/api/evc/status` stops claiming `reachable: true`.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn run_evc_poll_loop_times_out_when_charger_stops_answering() {
        use crate::inverter::poll::{AppState, PollMessage};
        use tokio::net::TcpListener;

        crate::test_util::with_isolated_config_dir_async(|| async {
            // Accept the connection and then go silent — the exact failure
            // mode of a charger that lost power without sending RST.
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                if let Ok((_socket, _)) = listener.accept().await {
                    // Hold the socket open without ever writing a response.
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                }
            });

            let state = Arc::new(AppState::new());
            {
                let mut s = state.settings.lock().await;
                s.evc_host = "127.0.0.1".to_string();
                s.evc_port = port;
            }
            let mut rx = state.tx.subscribe();

            let state_clone = state.clone();
            let handle = tokio::spawn(async move {
                run_evc_poll_loop_with_timeouts(
                    state_clone,
                    std::time::Duration::from_millis(500),
                    std::time::Duration::from_millis(300),
                )
                .await
            });

            // The read must time out and surface as EvcDisconnected.
            let saw_disconnected = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    match rx.recv().await {
                        Ok(PollMessage::EvcDisconnected) => break true,
                        Ok(PollMessage::EvcConnected) => continue,
                        Ok(_) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break false,
                    }
                }
            })
            .await;

            assert_eq!(
                saw_disconnected,
                Ok(true),
                "EvcDisconnected must be broadcast when the charger stops answering"
            );
            let evc = state.latest_evc.lock().await;
            assert!(
                evc.is_none(),
                "a frozen snapshot must not stay cached as reachable"
            );
            handle.abort();
        })
        .await;
    }

    #[test]
    fn reachability_has_a_pinned_grace_period_and_resets_for_a_new_host() {
        let mut reachability = EvcReachability::default();
        assert_eq!(
            reachability.status_at(1_000).connection_state,
            EvcConnectionState::NeverConnected
        );
        assert!(!reachability.status_at(1_000).reachable);

        reachability.record_success(1_000);
        let connected = reachability.status_at(1_000);
        assert_eq!(connected.connection_state, EvcConnectionState::Connected);
        assert!(connected.reachable);
        assert!(!connected.stale);
        assert_eq!(connected.age_seconds, Some(0));

        reachability.record_failure(1_001);
        let degraded = reachability.status_at(1_001);
        assert_eq!(degraded.connection_state, EvcConnectionState::Degraded);
        assert!(degraded.reachable, "a transient failure stays within grace");
        assert!(degraded.stale, "the cached snapshot is no longer fresh");

        let expired_at = 1_000 + EVC_CACHE_GRACE_MS + 1;
        let expired = reachability.status_at(expired_at);
        assert_eq!(expired.connection_state, EvcConnectionState::Disconnected);
        assert!(!expired.reachable);
        assert!(expired.stale);
        assert_eq!(expired.age_seconds, Some((EVC_CACHE_GRACE_MS + 1) / 1_000));

        reachability.record_success(expired_at + 1);
        let reconnected = reachability.status_at(expired_at + 1);
        assert_eq!(reconnected.connection_state, EvcConnectionState::Connected);
        assert!(reconnected.reachable);
        assert!(!reconnected.stale);
        assert_eq!(reconnected.age_seconds, Some(0));

        reachability.reset();
        let reset = reachability.status_at(expired_at + 2);
        assert_eq!(reset.connection_state, EvcConnectionState::NeverConnected);
        assert!(!reset.reachable);
        assert!(!reset.stale);
        assert_eq!(reset.last_success_at_epoch_ms, None);
    }
}
