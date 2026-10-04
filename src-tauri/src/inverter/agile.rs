//! Agile Octopus glue for the poll loop.
//!
//! The slot decision itself lives in [`state_machines::evaluate_agile_slot`];
//! this module holds the parts around it that the loop used to inline: where
//! prices come from (URL, fetch, parse, current-slot lookup), whether another
//! automation has the discharge-control domain this cycle, and how an action
//! becomes a register command.

use crate::inverter::encoder::ControlCommand;
use crate::inverter::state_machines::{
    sort_price_slots_newest_first, AgileSlotAction, DischargeControlArbiter, DischargeControlOwner,
    PriceSlot,
};

/// The real Octopus endpoint, used unless `agile_api_base_url` overrides it
/// (tests and self-hosters point it at a mock or mirror).
const DEFAULT_OCTOPUS_API_BASE: &str = "https://api.octopus.energy";

/// Per-request ceiling. An unreachable or slow price source must never stall
/// the poll loop for ureq's default connect timeout.
const FETCH_TIMEOUT_SECS: u64 = 10;

/// URL of today's Agile unit rates for `region`.
///
/// Anchored to the start of TODAY (UTC) so the response always includes the
/// current slot. The endpoint returns results newest-first, so a bare
/// `page_size=48` returns tomorrow's slots once they are published (~1pm) and
/// the current slot drops out of the window, which silently left the state
/// machine Idle and never discharged.
pub(crate) fn agile_rates_url(base_override: &str, region: &str, today: &str) -> String {
    let base = if base_override.is_empty() {
        DEFAULT_OCTOPUS_API_BASE
    } else {
        base_override
    };
    format!(
        "{base}/v1/products/AGILE-24-10-01/electricity-tariffs/E-1R-AGILE-24-10-01-{region}/standard-unit-rates/?period_from={today}T00:00:00Z&page_size=96"
    )
}

/// Parse an Octopus unit-rates response into price slots, newest first.
/// Rows missing a price or a valid timestamp are skipped rather than failing
/// the whole response.
pub(crate) fn parse_agile_rates(body: &str) -> Result<Vec<PriceSlot>, String> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("JSON error: {e}"))?;
    let results = json["results"]
        .as_array()
        .ok_or_else(|| "missing results".to_string())?;
    let mut slots: Vec<PriceSlot> = results
        .iter()
        .filter_map(|r| {
            let pence = r["value_inc_vat"].as_f64()?;
            let from = r["valid_from"].as_str()?;
            let to = r["valid_to"].as_str()?;
            let valid_from = chrono::DateTime::parse_from_rfc3339(from).ok()?.timestamp();
            let valid_to = chrono::DateTime::parse_from_rfc3339(to).ok()?.timestamp();
            Some(PriceSlot {
                pence,
                valid_from,
                valid_to,
            })
        })
        .collect();
    sort_price_slots_newest_first(&mut slots);
    Ok(slots)
}

/// Fetch and parse the rates at `url`. Blocking: run it on a blocking thread.
///
/// No idle keep-alive connections: tests spin up throwaway mock Octopus
/// servers and close them per test, and a lingering pooled connection would
/// hang the close.
pub(crate) fn fetch_agile_rates(url: &str) -> Result<Vec<PriceSlot>, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS)))
        .max_idle_connections(0)
        .max_idle_connections_per_host(0)
        .build();
    let mut resp = ureq::Agent::new_with_config(agent)
        .get(url)
        .call()
        .map_err(|e| format!("HTTP error: {e}"))?;
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("read error: {e}"))?;
    parse_agile_rates(&body)
}

/// Price (pence) of the slot containing `now_ts`; `valid_from` is inclusive and
/// `valid_to` exclusive. `None` when no cached slot covers it.
pub(crate) fn price_at(prices: &[PriceSlot], now_ts: i64) -> Option<f64> {
    prices
        .iter()
        .find(|s| now_ts >= s.valid_from && now_ts < s.valid_to)
        .map(|s| s.pence)
}

/// Defer entirely while another automation owns the discharge-control domain
/// this cycle (issue #289 priority: scheduled Timed Export outranks Agile on
/// the discharge side). Agile's discharge slots and mode writes would cancel
/// an active export window, and even its charge actions clobber HR27 / the
/// three-phase discharge enable on some models.
pub(crate) fn defer_if_outranked(
    action: AgileSlotAction,
    arbiter: DischargeControlArbiter,
) -> AgileSlotAction {
    if arbiter.can_request(DischargeControlOwner::Agile) {
        action
    } else {
        tracing::debug!(
            owner = ?arbiter.selected_owner(),
            "Agile: deferring — another discharge-control owner won this cycle"
        );
        AgileSlotAction::Defer
    }
}

/// The register command for an action, routed to the three-phase or the
/// single-phase slot registers. `Defer` maps to a clear command that the
/// caller must not write (see `should_write_agile_action`).
pub(crate) fn agile_command(action: &AgileSlotAction, three_phase: bool) -> ControlCommand {
    match action {
        AgileSlotAction::Charge {
            start_hhmm,
            end_hhmm,
            target_soc,
        } => {
            tracing::info!(
                "Agile: cheap window, charging {start_hhmm:04}–{end_hhmm:04} to {target_soc}%"
            );
            if three_phase {
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
        AgileSlotAction::Discharge {
            start_hhmm,
            end_hhmm,
        } => {
            tracing::info!(
                "Agile: expensive window, discharging (export) {start_hhmm:04}–{end_hhmm:04}"
            );
            if three_phase {
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
            // Cosy or auto-winter owns this side. Don't touch the inverter.
            // Logged at debug only because this fires every poll during a cosy
            // slot.
            tracing::debug!("Agile: deferring (cosy/auto-winter owns charge side)");
            ControlCommand::AgileClearActiveSlot
        }
        AgileSlotAction::Idle => {
            // Mid-band price, out-of-scope mode, or no price data. Disarm any
            // preloaded slot.
            tracing::debug!("Agile: idle, clearing active slot");
            if three_phase {
                ControlCommand::ThreePhaseAgileClearActiveSlot
            } else {
                ControlCommand::AgileClearActiveSlot
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    // ---- agile_rates_url ----------------------------------------------

    #[test]
    fn the_url_defaults_to_the_real_octopus_endpoint() {
        let url = agile_rates_url("", "C", "2026-10-04");
        assert!(url.starts_with("https://api.octopus.energy/v1/products/AGILE-24-10-01/"));
    }

    #[test]
    fn an_override_replaces_the_base_url() {
        let url = agile_rates_url("http://127.0.0.1:9999", "C", "2026-10-04");
        assert!(url.starts_with("http://127.0.0.1:9999/v1/products/"));
        assert!(!url.contains("octopus.energy"));
    }

    #[test]
    fn the_url_carries_the_region_and_anchors_to_the_start_of_today() {
        let url = agile_rates_url("", "H", "2026-10-04");
        assert!(url.contains("/E-1R-AGILE-24-10-01-H/"), "{url}");
        assert!(url.contains("period_from=2026-10-04T00:00:00Z"), "{url}");
        // A full day (48 half hours) plus tomorrow's if published.
        assert!(url.ends_with("page_size=96"), "{url}");
    }

    // ---- parse_agile_rates --------------------------------------------

    fn rate(pence: f64, from: &str, to: &str) -> String {
        format!(r#"{{"value_inc_vat":{pence},"valid_from":"{from}","valid_to":"{to}"}}"#)
    }

    fn body(rows: &[String]) -> String {
        format!(
            r#"{{"count":{},"results":[{}]}}"#,
            rows.len(),
            rows.join(",")
        )
    }

    #[test]
    fn rates_are_parsed_with_timestamps_and_returned_newest_first() {
        // The API order is not a contract: feed oldest-first.
        let b = body(&[
            rate(10.5, "2026-10-04T00:00:00Z", "2026-10-04T00:30:00Z"),
            rate(22.0, "2026-10-04T00:30:00Z", "2026-10-04T01:00:00Z"),
        ]);
        let slots = parse_agile_rates(&b).unwrap();
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].pence, 22.0, "newest first");
        assert_eq!(slots[1].pence, 10.5);
        assert_eq!(slots[0].valid_to - slots[0].valid_from, 1800);
        assert_eq!(slots[1].valid_to, slots[0].valid_from, "contiguous");
    }

    #[test]
    fn negative_prices_are_preserved() {
        // Agile genuinely goes negative (paid to consume).
        let b = body(&[rate(-4.2, "2026-10-04T12:00:00Z", "2026-10-04T12:30:00Z")]);
        assert_eq!(parse_agile_rates(&b).unwrap()[0].pence, -4.2);
    }

    #[test]
    fn rows_with_a_missing_or_malformed_field_are_skipped_not_fatal() {
        let b = format!(
            r#"{{"results":[{good},
                {{"valid_from":"2026-10-04T01:00:00Z","valid_to":"2026-10-04T01:30:00Z"}},
                {{"value_inc_vat":"cheap","valid_from":"2026-10-04T01:30:00Z","valid_to":"2026-10-04T02:00:00Z"}},
                {{"value_inc_vat":9.0,"valid_from":"not a time","valid_to":"2026-10-04T02:30:00Z"}},
                {{"value_inc_vat":9.0,"valid_from":"2026-10-04T02:00:00Z","valid_to":"garbage"}},
                {{"value_inc_vat":9.0,"valid_from":"2026-10-04T02:00:00Z"}}]}}"#,
            good = rate(15.0, "2026-10-04T00:00:00Z", "2026-10-04T00:30:00Z"),
        );
        let slots = parse_agile_rates(&b).unwrap();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].pence, 15.0);
    }

    #[test]
    fn an_empty_results_array_is_ok_and_empty() {
        assert!(parse_agile_rates(r#"{"results":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn a_response_without_results_is_an_error() {
        assert_eq!(
            parse_agile_rates(r#"{"detail":"Not found."}"#).unwrap_err(),
            "missing results"
        );
        assert_eq!(
            parse_agile_rates(r#"{"results":"nope"}"#).unwrap_err(),
            "missing results"
        );
    }

    #[test]
    fn a_non_json_response_is_an_error() {
        let err = parse_agile_rates("<html>502 Bad Gateway</html>").unwrap_err();
        assert!(err.starts_with("JSON error:"), "{err}");
        assert!(parse_agile_rates("")
            .unwrap_err()
            .starts_with("JSON error:"));
    }

    // ---- price_at -------------------------------------------------------

    fn slot(pence: f64, from: i64, to: i64) -> PriceSlot {
        PriceSlot {
            pence,
            valid_from: from,
            valid_to: to,
        }
    }

    #[test]
    fn the_price_is_that_of_the_slot_containing_now() {
        let prices = vec![slot(30.0, 1800, 3600), slot(10.0, 0, 1800)];
        assert_eq!(price_at(&prices, 100), Some(10.0));
        assert_eq!(price_at(&prices, 2000), Some(30.0));
    }

    #[test]
    fn a_slot_includes_its_start_but_not_its_end() {
        let prices = vec![slot(30.0, 1800, 3600), slot(10.0, 0, 1800)];
        assert_eq!(price_at(&prices, 1800), Some(30.0), "start is inclusive");
        assert_eq!(price_at(&prices, 1799), Some(10.0));
        assert_eq!(price_at(&prices, 3600), None, "end is exclusive");
    }

    #[test]
    fn no_price_when_nothing_covers_now() {
        assert_eq!(price_at(&[], 100), None);
        assert_eq!(price_at(&[slot(10.0, 0, 1800)], -1), None);
        // A gap between slots.
        assert_eq!(
            price_at(&[slot(10.0, 0, 1000), slot(20.0, 2000, 3000)], 1500),
            None
        );
    }

    // ---- defer_if_outranked ---------------------------------------------

    fn charge() -> AgileSlotAction {
        AgileSlotAction::Charge {
            start_hhmm: 130,
            end_hhmm: 530,
            target_soc: 100,
        }
    }

    #[test]
    fn with_no_other_owner_the_action_stands() {
        let arbiter = DischargeControlArbiter::default();
        for action in [
            charge(),
            AgileSlotAction::Discharge {
                start_hhmm: 1600,
                end_hhmm: 1900,
            },
            AgileSlotAction::Idle,
            AgileSlotAction::Defer,
        ] {
            assert_eq!(
                std::mem::discriminant(&defer_if_outranked(action.clone(), arbiter)),
                std::mem::discriminant(&action)
            );
        }
    }

    #[test]
    fn any_other_owner_makes_agile_defer() {
        // Agile is the lowest-priority owner, so every other one outranks it.
        for owner in [
            DischargeControlOwner::TimedCharge,
            DischargeControlOwner::TimedExport,
            DischargeControlOwner::ManualMode,
            DischargeControlOwner::ExplicitPause,
            DischargeControlOwner::ManualForce,
            DischargeControlOwner::Safety,
        ] {
            let mut arbiter = DischargeControlArbiter::default();
            arbiter.request(owner);
            assert!(
                matches!(
                    defer_if_outranked(charge(), arbiter),
                    AgileSlotAction::Defer
                ),
                "{owner:?}"
            );
        }
    }

    #[test]
    fn agile_does_not_defer_to_itself() {
        let mut arbiter = DischargeControlArbiter::default();
        arbiter.request(DischargeControlOwner::Agile);
        assert!(matches!(
            defer_if_outranked(charge(), arbiter),
            AgileSlotAction::Charge { .. }
        ));
    }

    // ---- agile_command ----------------------------------------------------

    #[test]
    fn charge_routes_to_the_single_or_three_phase_slot_registers() {
        assert!(matches!(
            agile_command(&charge(), false),
            ControlCommand::AgileChargeSlot {
                start_hhmm: 130,
                end_hhmm: 530,
                target_soc: 100
            }
        ));
        assert!(matches!(
            agile_command(&charge(), true),
            ControlCommand::ThreePhaseAgileChargeSlot {
                start_hhmm: 130,
                end_hhmm: 530,
                target_soc: 100
            }
        ));
    }

    #[test]
    fn discharge_routes_to_the_single_or_three_phase_slot_registers() {
        let discharge = AgileSlotAction::Discharge {
            start_hhmm: 1600,
            end_hhmm: 1900,
        };
        assert!(matches!(
            agile_command(&discharge, false),
            ControlCommand::AgileDischargeSlot {
                start_hhmm: 1600,
                end_hhmm: 1900
            }
        ));
        assert!(matches!(
            agile_command(&discharge, true),
            ControlCommand::ThreePhaseAgileDischargeSlot {
                start_hhmm: 1600,
                end_hhmm: 1900
            }
        ));
    }

    #[test]
    fn idle_clears_the_slot_on_the_right_register_family() {
        assert!(matches!(
            agile_command(&AgileSlotAction::Idle, false),
            ControlCommand::AgileClearActiveSlot
        ));
        assert!(matches!(
            agile_command(&AgileSlotAction::Idle, true),
            ControlCommand::ThreePhaseAgileClearActiveSlot
        ));
    }

    #[test]
    fn defer_maps_to_a_clear_that_must_never_be_written() {
        // Same command either way; the caller's should_write_agile_action
        // guard is what keeps it off the wire.
        for three_phase in [false, true] {
            assert!(matches!(
                agile_command(&AgileSlotAction::Defer, three_phase),
                ControlCommand::AgileClearActiveSlot
            ));
        }
        use crate::settings::AgileScope;
        for scope in [AgileScope::Full, AgileScope::ChargeOnly, AgileScope::Off] {
            assert!(
                !crate::inverter::state_machines::should_write_agile_action(
                    scope,
                    &AgileSlotAction::Defer
                ),
                "{scope:?}"
            );
        }
    }

    #[test]
    fn every_action_encodes_to_writes_for_both_register_families() {
        for three_phase in [false, true] {
            for action in [
                charge(),
                AgileSlotAction::Discharge {
                    start_hhmm: 1600,
                    end_hhmm: 1900,
                },
                AgileSlotAction::Idle,
            ] {
                let writes = agile_command(&action, three_phase)
                    .encode()
                    .unwrap_or_else(|e| panic!("{action:?} 3ph={three_phase}: {e:?}"));
                assert!(!writes.is_empty(), "{action:?} 3ph={three_phase}");
            }
        }
    }

    // ---- fetch_agile_rates (local HTTP server, hermetic) ----------------

    /// Serve one canned HTTP response on an ephemeral localhost port.
    fn serve_once(status: &str, payload: &str) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        );
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (url, handle)
    }

    #[test]
    fn a_good_response_is_fetched_and_parsed() {
        let payload = body(&[rate(12.5, "2026-10-04T00:00:00Z", "2026-10-04T00:30:00Z")]);
        let (url, server) = serve_once("200 OK", &payload);
        let slots = fetch_agile_rates(&url).unwrap();
        server.join().unwrap();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].pence, 12.5);
    }

    #[test]
    fn a_server_error_is_reported_as_an_http_error() {
        let (url, server) = serve_once("500 Internal Server Error", "{}");
        let err = fetch_agile_rates(&url).unwrap_err();
        server.join().unwrap();
        assert!(err.starts_with("HTTP error:"), "{err}");
    }

    #[test]
    fn a_response_that_is_not_json_is_reported_as_a_json_error() {
        let (url, server) = serve_once("200 OK", "<html>nope</html>");
        let err = fetch_agile_rates(&url).unwrap_err();
        server.join().unwrap();
        assert!(err.starts_with("JSON error:"), "{err}");
    }

    #[test]
    fn an_unreachable_server_fails_fast_with_an_http_error() {
        // Bind then drop, so the port is closed.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let started = std::time::Instant::now();
        let err = fetch_agile_rates(&url).unwrap_err();
        assert!(err.starts_with("HTTP error:"), "{err}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(FETCH_TIMEOUT_SECS),
            "a refused connection must not wait out the timeout"
        );
    }
}
