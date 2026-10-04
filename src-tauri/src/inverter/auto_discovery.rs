//! Auto-discovery of the inverter after persistent connection failures.
//!
//! After several consecutive failures to reach the configured host, the poll
//! loop scans the LAN in case the dongle's IP changed (DHCP renewal and the
//! like). If exactly one alternative is found it switches to it. The scan
//! itself is network I/O and stays in the loop; the decisions around it are
//! here so they can be tested.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::inverter::discovery::DiscoveredInverter;
use crate::inverter::poll::AppState;

/// Consecutive connect failures before a LAN scan is considered.
pub(crate) const DISCOVERY_AFTER_FAILURES: u32 = 5;

/// Minimum time between scans, so a dongle that is simply switched off does not
/// trigger a scan on every retry.
pub(crate) const DISCOVERY_COOLDOWN: Duration = Duration::from_secs(300);

/// Whether to scan the LAN now.
pub(crate) fn discovery_due(
    auto_discovery_disabled: bool,
    consecutive_failures: u32,
    last_scan: Option<Instant>,
    now: Instant,
) -> bool {
    !auto_discovery_disabled
        && consecutive_failures >= DISCOVERY_AFTER_FAILURES
        && last_scan.is_none_or(|t| now.saturating_duration_since(t) >= DISCOVERY_COOLDOWN)
}

/// What a scan result means for the configured host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiscoveryChoice {
    /// Nothing but (at most) the unreachable configured host answered.
    NoneFound,
    /// Exactly one alternative inverter: switch to it.
    Switch { ip: String, port: u16 },
    /// Several alternatives: refuse to guess. Holds `ip:port` for each.
    Ambiguous(Vec<String>),
}

/// Pick the inverter to switch to, ignoring the configured host (it is clearly
/// not responding). Compared by address only.
pub(crate) fn choose_discovery_target(
    found: &[DiscoveredInverter],
    configured_host: &str,
) -> DiscoveryChoice {
    let candidates: Vec<&DiscoveredInverter> = found
        .iter()
        .filter(|inv| inv.ip != configured_host)
        .collect();
    match candidates.as_slice() {
        [] => DiscoveryChoice::NoneFound,
        [only] => DiscoveryChoice::Switch {
            ip: only.ip.clone(),
            port: only.port,
        },
        many => DiscoveryChoice::Ambiguous(
            many.iter()
                .map(|inv| format!("{}:{}", inv.ip, inv.port))
                .collect(),
        ),
    }
}

/// Act on a scan result. Returns `true` when the host was switched, so the
/// caller resets its failure counter and connect back-off and tries the new
/// host straight away.
pub(crate) async fn apply_discovery(
    state: &Arc<AppState>,
    configured_host: &str,
    configured_port: u16,
    found: &[DiscoveredInverter],
) -> bool {
    match choose_discovery_target(found, configured_host) {
        DiscoveryChoice::NoneFound => {
            tracing::warn!(
                "Auto-discovery: no alternative inverters found on LAN ({configured_host}:{configured_port} unreachable). Dongle may be powered off or network changed."
            );
            false
        }
        DiscoveryChoice::Ambiguous(alternatives) => {
            tracing::warn!(
                "Auto-discovery: found {} alternative inverters — ambiguous, not auto-switching: {}",
                alternatives.len(),
                alternatives.join(", ")
            );
            false
        }
        DiscoveryChoice::Switch { ip, port } => {
            tracing::warn!(
                "Auto-discovery: found alternative inverter at {ip}:{port}. Auto-switching from {configured_host}:{configured_port}."
            );
            // Persist the new host so it survives a restart.
            let (new_host, new_port) = (ip.clone(), port);
            let persisted = tokio::task::spawn_blocking(move || {
                crate::settings::Settings::update(move |s| {
                    s.host = new_host;
                    s.port = new_port;
                })
                .map(|_| ())
            })
            .await
            .map_err(|error| format!("settings worker failed: {error}"))
            .and_then(|result| result);
            if let Err(e) = persisted {
                tracing::warn!("Auto-discovery: failed to persist new host: {e}");
            }

            // Update the in-memory settings and bump the version so the next
            // loop iteration picks up the new host.
            let mut settings = state.settings.lock().await;
            settings.host = ip;
            settings.port = port;
            settings.version = settings.version.wrapping_add(1);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::with_isolated_config_dir_async;

    fn inv(ip: &str, port: u16) -> DiscoveredInverter {
        DiscoveredInverter {
            ip: ip.to_string(),
            port,
        }
    }

    fn ago(now: Instant, secs: u64) -> Instant {
        now.checked_sub(Duration::from_secs(secs))
            .expect("monotonic clock has run for long enough")
    }

    // ---- discovery_due ------------------------------------------------------

    #[test]
    fn a_scan_waits_for_enough_consecutive_failures() {
        let now = Instant::now();
        assert!(!discovery_due(
            false,
            DISCOVERY_AFTER_FAILURES - 1,
            None,
            now
        ));
        assert!(discovery_due(false, DISCOVERY_AFTER_FAILURES, None, now));
        assert!(discovery_due(
            false,
            DISCOVERY_AFTER_FAILURES + 50,
            None,
            now
        ));
    }

    #[test]
    fn a_scan_never_runs_when_auto_discovery_is_disabled() {
        assert!(!discovery_due(true, 1_000, None, Instant::now()));
    }

    #[test]
    fn a_recent_scan_holds_the_next_one_back_until_the_cooldown_ends() {
        let now = Instant::now();
        let cooldown = DISCOVERY_COOLDOWN.as_secs();
        assert!(!discovery_due(false, 10, Some(ago(now, 1)), now));
        assert!(!discovery_due(false, 10, Some(ago(now, cooldown - 1)), now));
        assert!(
            discovery_due(false, 10, Some(ago(now, cooldown)), now),
            "the cooldown boundary itself is due"
        );
        assert!(discovery_due(false, 10, Some(ago(now, cooldown + 60)), now));
    }

    #[test]
    fn a_clock_that_has_not_advanced_does_not_panic_or_fire() {
        // `last_scan` equal to (or after) `now` must be treated as "just now".
        let now = Instant::now();
        assert!(!discovery_due(false, 10, Some(now), now));
    }

    // ---- choose_discovery_target ----------------------------------------------

    #[test]
    fn an_empty_scan_finds_nothing() {
        assert_eq!(
            choose_discovery_target(&[], "192.168.1.50"),
            DiscoveryChoice::NoneFound
        );
    }

    #[test]
    fn the_unreachable_configured_host_is_ignored() {
        let found = [inv("192.168.1.50", 8899)];
        assert_eq!(
            choose_discovery_target(&found, "192.168.1.50"),
            DiscoveryChoice::NoneFound
        );
    }

    #[test]
    fn a_single_alternative_is_chosen() {
        let found = [inv("192.168.1.77", 8899)];
        assert_eq!(
            choose_discovery_target(&found, "192.168.1.50"),
            DiscoveryChoice::Switch {
                ip: "192.168.1.77".into(),
                port: 8899
            }
        );
    }

    #[test]
    fn the_configured_host_does_not_make_a_single_alternative_ambiguous() {
        let found = [inv("192.168.1.50", 8899), inv("192.168.1.77", 8899)];
        assert_eq!(
            choose_discovery_target(&found, "192.168.1.50"),
            DiscoveryChoice::Switch {
                ip: "192.168.1.77".into(),
                port: 8899
            }
        );
    }

    #[test]
    fn several_alternatives_are_never_guessed_between() {
        let found = [inv("192.168.1.77", 8899), inv("192.168.1.88", 8899)];
        assert_eq!(
            choose_discovery_target(&found, "192.168.1.50"),
            DiscoveryChoice::Ambiguous(vec![
                "192.168.1.77:8899".to_string(),
                "192.168.1.88:8899".to_string()
            ])
        );
    }

    #[test]
    fn matching_is_by_address_not_port() {
        // Existing behaviour, pinned: the same address on another port is still
        // "the configured host" and is not offered as an alternative.
        let found = [inv("192.168.1.50", 9999)];
        assert_eq!(
            choose_discovery_target(&found, "192.168.1.50"),
            DiscoveryChoice::NoneFound
        );
    }

    #[test]
    fn a_hostname_never_matches_a_discovered_address() {
        // A configured hostname can't be compared with a scanned IP, so the
        // device found by IP counts as an alternative.
        let found = [inv("192.168.1.77", 8899)];
        assert!(matches!(
            choose_discovery_target(&found, "dongle.local"),
            DiscoveryChoice::Switch { .. }
        ));
    }

    // ---- apply_discovery --------------------------------------------------------

    async fn state_pointing_at(host: &str, port: u16) -> Arc<AppState> {
        let state = Arc::new(AppState::new());
        {
            let mut settings = state.settings.lock().await;
            settings.host = host.to_string();
            settings.port = port;
        }
        state
    }

    #[tokio::test]
    async fn a_single_alternative_is_switched_to_and_persisted() {
        with_isolated_config_dir_async(|| async {
            let state = state_pointing_at("192.168.1.50", 8899).await;
            let before = state.settings.lock().await.version;

            let switched =
                apply_discovery(&state, "192.168.1.50", 8899, &[inv("192.168.1.77", 8900)]).await;

            assert!(switched);
            let settings = state.settings.lock().await.clone();
            assert_eq!(settings.host, "192.168.1.77");
            assert_eq!(settings.port, 8900);
            assert_eq!(
                settings.version,
                before.wrapping_add(1),
                "the version bump is what makes the loop reconnect"
            );
            let on_disk = crate::settings::Settings::load();
            assert_eq!(on_disk.host, "192.168.1.77", "must survive a restart");
            assert_eq!(on_disk.port, 8900);
        })
        .await;
    }

    #[tokio::test]
    async fn nothing_changes_when_nothing_else_is_found() {
        with_isolated_config_dir_async(|| async {
            let state = state_pointing_at("192.168.1.50", 8899).await;
            let before = state.settings.lock().await.version;
            let switched =
                apply_discovery(&state, "192.168.1.50", 8899, &[inv("192.168.1.50", 8899)]).await;
            assert!(!switched);
            let settings = state.settings.lock().await.clone();
            assert_eq!(settings.host, "192.168.1.50");
            assert_eq!(settings.version, before);
        })
        .await;
    }

    #[tokio::test]
    async fn nothing_changes_when_the_result_is_ambiguous() {
        with_isolated_config_dir_async(|| async {
            let state = state_pointing_at("192.168.1.50", 8899).await;
            let before = state.settings.lock().await.version;
            let switched = apply_discovery(
                &state,
                "192.168.1.50",
                8899,
                &[inv("192.168.1.77", 8899), inv("192.168.1.88", 8899)],
            )
            .await;
            assert!(!switched);
            let settings = state.settings.lock().await.clone();
            assert_eq!(settings.host, "192.168.1.50");
            assert_eq!(settings.version, before);
            assert_eq!(
                crate::settings::Settings::load().host,
                crate::settings::Settings::default().host,
                "an ambiguous scan must not write anything to disk"
            );
        })
        .await;
    }
}
