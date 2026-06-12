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

/// Decode two register blocks (60 + 55) into an `EvcSnapshot`.
fn decode_evc(regs: &[u16]) -> EvcSnapshot {
    // regs[0..60] = block 1, regs[60..115] = block 2
    if regs.len() < 115 {
        tracing::warn!(
            "EVC: short register read ({} regs, expected 115)",
            regs.len()
        );
        return EvcSnapshot::default();
    }

    let charging_state = decode_charging_state(regs[0]);
    let connection_status = decode_connection_status(regs[2]);
    let current_l1 = regs[6] as f32 / 10.0;
    let current_l2 = regs[8] as f32 / 10.0;
    let current_l3 = regs[10] as f32 / 10.0;
    let active_power = regs[13] as i32;
    let meter_energy_kwh = regs[29] as f32 / 10.0;
    let charge_limit_a = regs[36] as f32 / 10.0;

    // Serial number: HR 38–68 → regs[38..69], ASCII chars, skip nulls
    let mut serial = String::new();
    for &w in &regs[38..69] {
        if w != 0 {
            serial.push(char::from_u32(w as u32).unwrap_or('?'));
        }
    }

    let session_energy_kwh = regs[72] as f32 / 10.0;
    let session_duration_secs = regs[79] as u32;

    let voltage_l1 = regs[109] as f32 / 10.0;
    let voltage_l2 = regs[111] as f32 / 10.0;
    let voltage_l3 = regs[113] as f32 / 10.0;

    EvcSnapshot {
        charging_state,
        connection_status,
        active_power,
        current_l1,
        current_l2,
        current_l3,
        voltage_l1,
        voltage_l2,
        voltage_l3,
        meter_energy_kwh,
        session_energy_kwh,
        session_duration_secs,
        charge_limit_a,
        serial_number: serial,
    }
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
/// Each host is probed by connecting and attempting to read HR 0 (Charging_State).
/// If the response is a valid Modbus frame, the device is reported.
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

/// Probe a single IP for a standard Modbus TCP device on port 502.
///
/// Connects, sends FC3 read for HR 0–14, and checks we get a valid
/// Modbus response. If successful, also extracts the serial number
/// from HR 38–68.
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

        // Step 2: Build a standard Modbus TCP FC3 (Read Holding Registers) request.
        // Transaction ID: 0x0001, Protocol: 0x0000, Length: 6, Unit: 1,
        // FC: 3 (Read Holding Registers), Start: 0, Count: 14
        let request: [u8; 12] = [
            0x00, 0x01, // Transaction ID
            0x00, 0x00, // Protocol (Modbus)
            0x00, 0x06, // Length
            0x01, // Unit ID (slave address)
            0x03, // Function code: Read Holding Registers
            0x00, 0x00, // Start address: 0
            0x00, 0x0E, // Quantity: 14 registers
        ];
        stream.write_all(&request).ok()?;

        // Step 3: Read response. A valid FC3 response starts with the same
        // transaction ID and has function code 0x03.
        let mut buf = [0u8; 256];
        let n = stream.read(&mut buf).ok()?;
        if n < 9 {
            return None;
        }
        // Check: transaction ID = 0x0001, protocol = 0x0000, FC = 0x03
        let txn = u16::from_be_bytes([buf[0], buf[1]]);
        let proto = u16::from_be_bytes([buf[2], buf[3]]);
        let fc = buf[7];
        if txn != 0x0001 || proto != 0x0000 || fc != 0x03 {
            return None;
        }

        Some(())
    })
    .await;

    if matches!(result, Ok(Some(()))) {
        tracing::debug!(
            "EVC: found Modbus device at {}:{}",
            ip_clone,
            EVC_MODBUS_PORT
        );
        Some(DiscoveredEvc {
            ip: ip_clone,
            port: EVC_MODBUS_PORT,
            serial: None,
        })
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Poll loop
// ---------------------------------------------------------------------------

/// Background poll loop for the EV charger. Reads settings from the shared
/// `AppState` to determine the EVC host/port. When configured, polls via
/// standard Modbus TCP every 10 seconds and broadcasts `PollMessage::Evc`
/// to all WebSocket clients.
pub async fn run_evc_poll_loop(state: Arc<AppState>) {
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
                tracing::warn!("EVC: invalid address '{socket_addr}': {e}");
                sleep(backoff).await;
                continue;
            }
        };

        let ctx = match tcp::connect_slave(addr, Slave(1)).await {
            Ok(ctx) => {
                tracing::info!(host = %evc_host, "EVC: connected");
                backoff = Duration::from_secs(10);
                ctx
            }
            Err(e) => {
                tracing::warn!("EVC: connect failed: {e}");
                {
                    let mut evc = state.latest_evc.lock().await;
                    *evc = None;
                }
                let _ = state.tx.send(PollMessage::EvcDisconnected);
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
            let result1 = ctx
                .as_mut()
                .unwrap()
                .read_holding_registers(0x0000, 60)
                .await;
            let regs1 = match result1 {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    tracing::warn!("EVC: Modbus exception reading HR 0–59: {e:?}");
                    ctx.take();
                    break;
                }
                Err(e) => {
                    tracing::warn!("EVC: read error HR 0–59: {e}");
                    ctx.take();
                    break;
                }
            };

            // Read block 2: HR 60–114
            let result2 = ctx.as_mut().unwrap().read_holding_registers(60, 55).await;
            let regs2 = match result2 {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    tracing::warn!("EVC: Modbus exception reading HR 60–114: {e:?}");
                    ctx.take();
                    break;
                }
                Err(e) => {
                    tracing::warn!("EVC: read error HR 60–114: {e}");
                    ctx.take();
                    break;
                }
            };

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

            // Store and broadcast
            {
                let mut evc = state.latest_evc.lock().await;
                *evc = Some(snapshot.clone());
            }
            let _ = state.tx.send(PollMessage::Evc(Box::new(snapshot)));

            sleep(poll_interval).await;
        }

        // Context dropped — reconnect after backoff
        tracing::warn!("EVC: connection lost, reconnecting in {:?}", backoff);
        {
            let mut evc = state.latest_evc.lock().await;
            *evc = None;
        }
        let _ = state.tx.send(PollMessage::EvcDisconnected);
        sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(120));
    }
}
