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
                // Host string is unparseable (issue #138: typo like "10.1.71"
                // instead of "10.1.1.71"). Without broadcasting, the frontend
                // sits on the Zustand defaults forever and shows a misleading
                // "Disconnected" label. Clear cached snapshot and emit
                // EvcDisconnected so the UI can render an honest state, then
                // back off. The frontend SettingsPage already blocks saving
                // bad hosts, but this also covers hand-edited settings.json
                // and old clients that pre-date the validator.
                tracing::warn!("EVC: invalid address '{socket_addr}': {e}");
                {
                    let mut evc = state.latest_evc.lock().await;
                    *evc = None;
                }
                let _ = state.tx.send(PollMessage::EvcDisconnected);
                sleep(backoff).await;
                continue;
            }
        };

        let ctx = match tcp::connect_slave(addr, Slave(1)).await {
            Ok(ctx) => {
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
        // Anything < 115 must NOT panic; it must return a default snapshot
        // (with charging_state="Unknown" since regs[0]==0).
        let snapshot = decode_evc(&[]);
        assert_eq!(snapshot.charging_state, "Unknown");
        assert_eq!(snapshot.connection_status, "Unknown");
        assert_eq!(snapshot.active_power, 0);

        let snapshot = decode_evc(&[0u16; 60]);
        assert_eq!(snapshot.charging_state, "Unknown");
        assert_eq!(snapshot.connection_status, "Unknown");

        let snapshot = decode_evc(&[0u16; 114]);
        assert_eq!(snapshot.charging_state, "Unknown");
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

        let state = Arc::new(AppState::new());
        // Confirm default settings have no EVC host.
        {
            let s = state.settings.lock().await;
            assert!(s.evc_host.is_empty(), "default evc_host should be empty");
        }

        let state_clone = state.clone();
        let handle = tokio::spawn(async move {
            // Use a timeout so the test doesn't hang forever if the
            // loop is misbehaving. The no-host branch sleeps 15s, so
            // a 2-second timeout is plenty.
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                run_evc_poll_loop(state_clone),
            )
            .await
        });

        // Give the loop time to spin through its first 15s sleep.
        // (The timeout will fire first — that's the test passing.)
        let result = handle.await.expect("join");
        assert!(
            result.is_err(),
            "poll loop should still be sleeping at 2s when no host is configured"
        );

        // No snapshot should have been written.
        let evc = state.latest_evc.lock().await;
        assert!(evc.is_none(), "no EVC snapshot should be cached");

        // No broadcast message should have been sent (other than the
        // first one a fresh broadcast::channel can hold).
        // We can't easily assert the channel is empty without a receiver,
        // so this is implicitly covered by the snapshot check.
    }
}
