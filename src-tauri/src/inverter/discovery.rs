//! LAN discovery of GivEnergy inverters.
//!
//! Scans the local network for devices listening on the Modbus port (8899).
//!
//! ## Strategy
//!
//! 1. Enumerate local IPv4 interfaces via `local_ip_address` crate.
//! 2. Collect all interfaces in **10.x.x.x** or **192.168.x.x** ranges
//!    (typical home/office LANs) and derive the /24 gateway for each.
//! 3. If no physical LAN interfaces are found (e.g. WSL2, Docker-only host),
//!    probe a set of **common home subnets** directly: 192.168.{0-3}.x, 10.0.0.x.
//! 4. **Never** scan 172.16-31.x.x — this range is exclusively used by
//!    Docker bridges, WSL2 virtual networks, and libvirt.
//!
//! For each candidate subnet, all 254 host addresses are probed concurrently
//! on port 8899 with a short connect-timeout.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::net::IpAddr;
use std::time::Duration;

use crate::modbus::framer;

/// The Modbus TCP port used by GivEnergy dongles.
pub const MODBUS_PORT: u16 = 8899;

/// A discovered inverter.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredInverter {
    pub ip: String,
    pub port: u16,
}

/// Common home-router subnets to try when no physical LAN interface is found.
const FALLBACK_SUBNETS: &[&str] = &[
    "192.168.0", // very common
    "192.168.1", // very common
    "192.168.2", // some ISPs
    "192.168.3", // less common
    "10.0.0",    // some routers
    "10.0.1",    // Apple routers
    "10.1.1",    // some ISPs
];

/// Scan the local network for inverters.
///
/// Uses the provided gateway IP to infer the /24 subnet, then probes
/// each address on MODBUS_PORT (8899) concurrently.
pub async fn scan_subnet(subnet_base: &str) -> Vec<DiscoveredInverter> {
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

    log::info!(
        "Subnet scan on {}.x found {} inverter(s)",
        subnet_base,
        found.len()
    );
    found
}

/// Scan multiple subnets concurrently and return all discovered inverters.
pub async fn scan_multiple_subnets(subnets: &[String]) -> Vec<DiscoveredInverter> {
    let mut all_found = Vec::new();
    for subnet in subnets {
        let found = scan_subnet(subnet).await;
        all_found.extend(found);
    }
    all_found
}

/// Probe a single IP:port to verify it speaks the GivEnergy Modbus protocol.
///
/// After confirming the TCP port is open, sends a minimal Modbus read request
/// and checks that the response contains a valid GivEnergy frame header
/// (transaction ID 0x5959). Devices that merely have port 8899 open but don't
/// speak the protocol will fail this check.
async fn probe_host(ip: String) -> Option<DiscoveredInverter> {
    let addr = format!("{}:{}", ip, MODBUS_PORT);
    let ip_for_closure = ip.clone();
    let ip_for_result = ip.clone();
    let result = tokio::task::spawn_blocking(move || {
        // Step 1: TCP connect
        let mut stream = TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_millis(800)).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
        stream.set_write_timeout(Some(Duration::from_secs(2))).ok()?;

        // Step 2: Send a minimal GivEnergy Modbus read request
        // Read 1 input register at address 0, slave 0x32, empty serial
        let request = framer::build_read_request("", 0x32, framer::RegisterType::Input, 0, 1);
        if stream.write_all(&request).is_err() {
            return None;
        }

        // Step 3: Read back enough bytes to verify the GivEnergy header
        // Minimum GivEnergy frame is 30 bytes; read up to 64.
        let mut buf = [0u8; 64];
        match stream.read(&mut buf) {
            Ok(n) if n >= 6 => {
                // Check transaction ID = 0x5959 (GivEnergy magic)
                let txn_id = u16::from_be_bytes([buf[0], buf[1]]);
                if txn_id == 0x5959 {
                    return Some(());
                }
                // Not a GivEnergy device
                log::debug!(
                    "Non-GivEnergy response from {}:{} (txn=0x{:04X})",
                    ip_for_closure, MODBUS_PORT, txn_id
                );
                None
            }
            Ok(_) => None, // too few bytes
            Err(_) => None, // read timeout / error
        }
    })
    .await;

    match result {
        Ok(Some(_)) => {
            log::debug!("Found GivEnergy device at {}:{}", ip_for_result, MODBUS_PORT);
            Some(DiscoveredInverter {
                ip: ip_for_result,
                port: MODBUS_PORT,
            })
        }
        _ => None,
    }
}

/// Auto-detect physical LAN subnets to scan.
///
/// Returns a list of /24 subnet base strings (e.g. `["192.168.1"]`).
pub fn detect_lan_subnets() -> Vec<String> {
    // Strategy 1: enumerate local interfaces, pick those in 10.x or 192.168.x
    if let Ok(interfaces) = local_ip_address::list_afinet_netifas() {
        let subnets = collect_physical_subnets(&interfaces);
        if !subnets.is_empty() {
            return subnets;
        }
    }

    // Strategy 2: no physical LAN interface found (WSL2, Docker-only, etc.)
    // Fall back to probing common home-router subnets.
    log::info!(
        "No 10.x or 192.168.x interfaces found — trying common home subnets"
    );
    FALLBACK_SUBNETS.iter().map(|s| (*s).to_string()).collect()
}

/// Given a list of (interface_name, IP) pairs, collect unique /24 subnet bases
/// for interfaces in 10.x.x.x or 192.168.x.x ranges, excluding virtual interfaces.
fn collect_physical_subnets(interfaces: &[(String, IpAddr)]) -> Vec<String> {
    let mut subnets = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for (name, ip) in interfaces {
        let IpAddr::V4(ipv4) = ip else { continue };
        let octets = ipv4.octets();

        // Skip loopback
        if octets[0] == 127 {
            continue;
        }

        // Skip known virtual interface name prefixes
        let name_lower = name.to_lowercase();
        if name_lower.starts_with("docker")
            || name_lower.starts_with("br-")
            || name_lower.starts_with("veth")
            || name_lower == "virbr0"
            || name_lower.starts_with("vmnet")
        {
            continue;
        }

        // Only accept 10.x.x.x and 192.168.x.x — these are home/office LANs.
        // 172.16-31.x.x is exclusively Docker/WSL/libvirt and should never be scanned.
        let is_physical = match octets {
            [192, 168, ..] => true,
            [10, ..] => true,
            _ => false,
        };

        if !is_physical {
            continue;
        }

        let subnet = format!("{}.{}.{}", octets[0], octets[1], octets[2]);
        if seen.insert(subnet.clone()) {
            subnets.push(subnet);
        }
    }

    subnets
}

/// Given a gateway like "192.168.1.1", return "192.168.1".
#[cfg(test)]
fn infer_subnet_base(gateway: &str) -> String {
    let parts: Vec<&str> = gateway.split('.').collect();
    if parts.len() == 4 {
        format!("{}.{}.{}", parts[0], parts[1], parts[2])
    } else {
        "192.168.1".to_string()
    }
}

/// Detect the local machine's LAN IP address.
///
/// Returns the first IP address on a physical interface in the 10.x.x.x or
/// 192.168.x.x range, excluding virtual interfaces (Docker, WSL, libvirt).
pub fn detect_lan_ip() -> Option<String> {
    let interfaces = local_ip_address::list_afinet_netifas().ok()?;

    for (name, ip) in &interfaces {
        let IpAddr::V4(ipv4) = ip else { continue };
        let octets = ipv4.octets();

        // Skip loopback
        if octets[0] == 127 {
            continue;
        }

        // Skip known virtual interface name prefixes
        let name_lower = name.to_lowercase();
        if name_lower.starts_with("docker")
            || name_lower.starts_with("br-")
            || name_lower.starts_with("veth")
            || name_lower == "virbr0"
            || name_lower.starts_with("vmnet")
        {
            continue;
        }

        // Only accept 10.x.x.x and 192.168.x.x
        match octets {
            [192, 168, ..] | [10, ..] => return Some(ipv4.to_string()),
            _ => continue,
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
    fn detect_lan_subnets_returns_something() {
        let subnets = detect_lan_subnets();
        assert!(!subnets.is_empty());
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

    #[test]
    fn collect_physical_subnets_192_168() {
        use std::str::FromStr;
        let interfaces = vec![
            ("eth0".to_string(), IpAddr::from_str("192.168.1.100").unwrap()),
            ("docker0".to_string(), IpAddr::from_str("172.17.0.1").unwrap()),
        ];
        let subnets = collect_physical_subnets(&interfaces);
        assert_eq!(subnets, vec!["192.168.1"]);
    }

    #[test]
    fn collect_physical_subnets_10_network() {
        use std::str::FromStr;
        let interfaces = vec![
            ("ens192".to_string(), IpAddr::from_str("10.0.5.100").unwrap()),
        ];
        let subnets = collect_physical_subnets(&interfaces);
        assert_eq!(subnets, vec!["10.0.5"]);
    }

    #[test]
    fn collect_physical_subnets_skips_172() {
        use std::str::FromStr;
        let interfaces = vec![
            ("eth0".to_string(), IpAddr::from_str("172.22.59.58").unwrap()),
            ("docker0".to_string(), IpAddr::from_str("172.17.0.1").unwrap()),
        ];
        let subnets = collect_physical_subnets(&interfaces);
        assert!(subnets.is_empty());
    }

    #[test]
    fn collect_physical_subnets_skips_virtual_names() {
        use std::str::FromStr;
        let interfaces = vec![
            ("docker0".to_string(), IpAddr::from_str("192.168.1.1").unwrap()),
            ("br-abc".to_string(), IpAddr::from_str("10.0.0.1").unwrap()),
            ("veth123".to_string(), IpAddr::from_str("192.168.1.50").unwrap()),
        ];
        let subnets = collect_physical_subnets(&interfaces);
        assert!(subnets.is_empty());
    }

    #[test]
    fn collect_physical_subnets_deduplicates() {
        use std::str::FromStr;
        let interfaces = vec![
            ("eth0".to_string(), IpAddr::from_str("192.168.1.100").unwrap()),
            ("wlan0".to_string(), IpAddr::from_str("192.168.1.101").unwrap()),
        ];
        let subnets = collect_physical_subnets(&interfaces);
        assert_eq!(subnets, vec!["192.168.1"]);
    }

    #[test]
    fn collect_physical_subnets_no_loopback() {
        use std::str::FromStr;
        let interfaces = vec![
            ("lo".to_string(), IpAddr::from_str("127.0.0.1").unwrap()),
        ];
        let subnets = collect_physical_subnets(&interfaces);
        assert!(subnets.is_empty());
    }
}
