//! LAN discovery of GivEnergy inverters.
//!
//! Scans the local subnet for devices listening on the Modbus port (8899).
//! Just like GivTCP, this is a simple TCP port scan — the serial number is
//! configured separately by the user.

use std::net::TcpStream;
use std::time::Duration;

/// The Modbus TCP port used by GivEnergy dongles.
pub const MODBUS_PORT: u16 = 8899;

/// A discovered inverter.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredInverter {
    pub ip: String,
    pub port: u16,
}

/// Scan the local subnet for inverters.
///
/// Uses the provided gateway IP to infer the /24 subnet, then probes
/// each address on MODBUS_PORT (8899) concurrently.
pub async fn scan_subnet(gateway: &str) -> Vec<DiscoveredInverter> {
    let subnet_base = infer_subnet_base(gateway);
    log::info!("Starting subnet scan on {}.x:{}", subnet_base, MODBUS_PORT);

    let mut tasks = Vec::new();
    for host in 1..255u8 {
        let ip = format!("{}.{}", subnet_base, host);
        tasks.push(probe_host(ip));
    }

    let results = futures_util::future::join_all(tasks).await;
    let mut found = Vec::new();
    for result in results {
        if let Some(inverter) = result {
            found.push(inverter);
        }
    }

    log::info!("Subnet scan found {} inverter(s)", found.len());
    found
}

/// Probe a single IP:port to see if a GivEnergy dongle is listening.
async fn probe_host(ip: String) -> Option<DiscoveredInverter> {
    let addr = format!("{}:{}", ip, MODBUS_PORT);
    let ip_clone = ip.clone();
    let result = tokio::task::spawn_blocking(move || {
        match TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_millis(800)) {
            Ok(_) => Some(()),
            Err(_) => None,
        }
    })
    .await;

    match result {
        Ok(Some(_)) => {
            log::debug!("Found open port at {}:{}", ip_clone, MODBUS_PORT);
            Some(DiscoveredInverter {
                ip: ip_clone,
                port: MODBUS_PORT,
            })
        }
        _ => None,
    }
}

/// Given a gateway like "192.168.1.1", return "192.168.1".
fn infer_subnet_base(gateway: &str) -> String {
    let parts: Vec<&str> = gateway.split('.').collect();
    if parts.len() == 4 {
        format!("{}.{}.{}", parts[0], parts[1], parts[2])
    } else {
        "192.168.1".to_string()
    }
}

/// Auto-detect the local subnet gateway from system interfaces.
pub fn detect_local_subnet() -> Option<String> {
    if let Ok(output) = std::process::Command::new("ip")
        .args(["route", "show", "default"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.starts_with("default via ") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 {
                    return Some(parts[2].to_string());
                }
            }
        }
    }
    None
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_subnet_from_gateway() {
        assert_eq!(infer_subnet_base("192.168.1.1"), "192.168.1");
        assert_eq!(infer_subnet_base("10.0.0.1"), "10.0.0");
        assert_eq!(infer_subnet_base("172.16.0.254"), "172.16.0");
    }

    #[test]
    fn infer_subnet_short_input() {
        assert_eq!(infer_subnet_base("not-an-ip"), "192.168.1");
    }

    #[test]
    fn detect_local_subnet_returns_something() {
        let _ = detect_local_subnet();
    }

    #[test]
    fn discovered_inverter_serializes() {
        let inv = DiscoveredInverter {
            ip: "192.168.1.36".to_string(),
            port: 8899,
        };
        let json = serde_json::to_string(&inv).unwrap();
        assert!(json.contains("192.168.1.36"));
        assert!(json.contains("8899"));
    }
}
