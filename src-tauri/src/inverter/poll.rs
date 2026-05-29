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

use crate::inverter::decoder::decode_snapshot;
use crate::inverter::encoder::RegisterWrite;
use crate::inverter::model::InverterSnapshot;
use crate::modbus::client::ModbusClient;

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

                    let poll_result = async {
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
                                    let persist = crate::settings::Settings {
                                        host: settings.host.clone(),
                                        port: settings.port,
                                        serial: discovered,
                                        poll_interval: settings.interval_secs,
                                        auto_connect: true,
                                    };
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
                                    client.set_slave(addr);
                                    match client.read_registers(
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

                                // Reset slave address back to inverter default
                                client.set_slave(0x32);

                                // Store latest snapshot.
                                {
                                    let mut latest = state.latest_snapshot.lock().await;
                                    *latest = Some(snapshot.clone());
                                }

                                // Broadcast to WebSocket subscribers.
                                let _ = state.tx.send(PollMessage::Snapshot(snapshot));

                                Ok(())
                            }
                            Err(e) => Err(e),
                        }
                    }.await;

                    match poll_result {
                        Ok(()) => {
                            consecutive_failures = 0;
                        }
                        Err(e) => {
                            consecutive_failures += 1;
                            if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                                tracing::warn!(
                                    "Poll read failed ({}/{}): {e} — disconnecting",
                                    consecutive_failures, MAX_CONSECUTIVE_FAILURES,
                                );
                                break; // tear down connection and reconnect
                            }
                            // Transient error — retry after a short pause
                            tracing::debug!(
                                "Poll read failed ({}/{}): {e} — retrying",
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
