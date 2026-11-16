//! When to look for external CT clamp meters (issue #473).
//!
//! The first scan after a connection checks every address 0x01-0x08 once.
//! A meter that misses that one 3 s try (slow reply, or the dongle's empty
//! answer) used to stay hidden until the next reconnect, so discovery now
//! carries on afterwards, one address per step:
//!
//! - an **expected** address (named in Settings → Meters, used by a solar
//!   array, or any address while an ammeter is configured and no meter has
//!   answered) is tried every 2 minutes for as long as it's missing;
//! - any other missing address is tried every 2 minutes for the first 30
//!   minutes after the connection, then every 15 minutes, so a clamp that
//!   was switched on later still turns up without a restart.
//!
//! A step is a single probe (at most one 3 s timeout), and an expected
//! address always goes before an unexpected one. Everything here is pure:
//! the clock is passed in.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use crate::modbus::registers::METER_ADDRESSES;

/// Gap between probes of a missing address the user expects to exist.
pub(crate) const EXPECTED_INTERVAL: Duration = Duration::from_secs(2 * 60);
/// Gap between probes of any other missing address while the connection is
/// young.
pub(crate) const OTHER_FAST_INTERVAL: Duration = Duration::from_secs(2 * 60);
/// How long the connection counts as young.
pub(crate) const OTHER_FAST_WINDOW: Duration = Duration::from_secs(30 * 60);
/// Gap between probes of any other missing address afterwards.
pub(crate) const OTHER_SLOW_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// What the poll loop should probe this cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MeterProbePlan {
    /// The first scan of the connection: every address.
    Scan,
    /// One missing address.
    Address(u8),
}

/// Discovery state for one connection (a reconnect starts afresh).
#[derive(Debug, Default)]
pub(crate) struct MeterDiscovery {
    started: Option<Instant>,
    last_expected: Option<Instant>,
    last_other: Option<Instant>,
    cursor_expected: u8,
    cursor_other: u8,
}

impl MeterDiscovery {
    /// What to probe now, if anything.
    ///
    /// `eligible` is false for models that never carry an external meter
    /// (three-phase, batteryless) or before the model is known. `present`
    /// is every address whose meter is being read now; `expected` is every
    /// address the user says exists.
    pub(crate) fn next(
        &mut self,
        now: Instant,
        eligible: bool,
        present: &BTreeSet<u8>,
        expected: &BTreeSet<u8>,
    ) -> Option<MeterProbePlan> {
        if !eligible {
            return None;
        }
        let Some(started) = self.started else {
            self.started = Some(now);
            self.last_expected = Some(now);
            self.last_other = Some(now);
            return Some(MeterProbePlan::Scan);
        };

        let missing = |want_expected: bool| -> Vec<u8> {
            METER_ADDRESSES
                .iter()
                .copied()
                .filter(|a| !present.contains(a) && expected.contains(a) == want_expected)
                .collect()
        };

        let missing_expected = missing(true);
        if !missing_expected.is_empty() && due(self.last_expected, now, EXPECTED_INTERVAL) {
            self.last_expected = Some(now);
            let addr = rotate(&missing_expected, self.cursor_expected);
            self.cursor_expected = addr;
            return Some(MeterProbePlan::Address(addr));
        }

        let missing_other = missing(false);
        let other_interval = if now.saturating_duration_since(started) < OTHER_FAST_WINDOW {
            OTHER_FAST_INTERVAL
        } else {
            OTHER_SLOW_INTERVAL
        };
        if !missing_other.is_empty() && due(self.last_other, now, other_interval) {
            self.last_other = Some(now);
            let addr = rotate(&missing_other, self.cursor_other);
            self.cursor_other = addr;
            return Some(MeterProbePlan::Address(addr));
        }
        None
    }
}

fn due(last: Option<Instant>, now: Instant, interval: Duration) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= interval)
}

/// The first address after `cursor`, wrapping to the lowest. `addrs` is
/// sorted and non-empty.
fn rotate(addrs: &[u8], cursor: u8) -> u8 {
    addrs
        .iter()
        .copied()
        .find(|&a| a > cursor)
        .unwrap_or(addrs[0])
}

/// The addresses the user says have a meter: named in Settings → Meters,
/// used by a solar array, or (when an ammeter is configured and nothing has
/// answered yet) every address, as a slow EM115 may sit on any of them.
pub(crate) fn expected_addresses<'a>(
    meter_names: impl IntoIterator<Item = &'a String>,
    solar_array_addresses: impl IntoIterator<Item = u8>,
    ammeter_configured: bool,
    any_present: bool,
) -> BTreeSet<u8> {
    let mut out = BTreeSet::new();
    for key in meter_names {
        if let Ok(addr) = key.parse::<u8>() {
            if METER_ADDRESSES.contains(&addr) {
                out.insert(addr);
            }
        }
    }
    for addr in solar_array_addresses {
        if METER_ADDRESSES.contains(&addr) {
            out.insert(addr);
        }
    }
    if ammeter_configured && !any_present {
        out.extend(METER_ADDRESSES.iter().copied());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(addrs: &[u8]) -> BTreeSet<u8> {
        addrs.iter().copied().collect()
    }

    fn secs(base: Instant, s: u64) -> Instant {
        base + Duration::from_secs(s)
    }

    #[test]
    fn first_call_is_a_full_scan_and_only_once() {
        let t0 = Instant::now();
        let mut d = MeterDiscovery::default();
        assert_eq!(
            d.next(t0, true, &set(&[]), &set(&[])),
            Some(MeterProbePlan::Scan)
        );
        assert_eq!(d.next(t0, true, &set(&[]), &set(&[])), None);
    }

    #[test]
    fn never_probes_when_not_eligible() {
        let t0 = Instant::now();
        let mut d = MeterDiscovery::default();
        assert_eq!(d.next(t0, false, &set(&[]), &set(&[3])), None);
        assert_eq!(d.next(secs(t0, 3600), false, &set(&[]), &set(&[3])), None);
        // Becoming eligible later (model identified) still starts with a scan.
        assert_eq!(
            d.next(secs(t0, 3601), true, &set(&[]), &set(&[3])),
            Some(MeterProbePlan::Scan)
        );
    }

    #[test]
    fn a_meter_missed_by_the_first_scan_is_probed_after_two_minutes() {
        let t0 = Instant::now();
        let mut d = MeterDiscovery::default();
        // 0x03 is expected but missing; nothing else is.
        let present = set(&[1, 2, 4, 5, 6, 7, 8]);
        let expected = set(&[1, 2, 3]);
        assert_eq!(
            d.next(t0, true, &present, &expected),
            Some(MeterProbePlan::Scan)
        );
        assert_eq!(d.next(secs(t0, 119), true, &present, &expected), None);
        assert_eq!(
            d.next(secs(t0, 120), true, &present, &expected),
            Some(MeterProbePlan::Address(3))
        );
        // Still missing: again two minutes later, never giving up.
        assert_eq!(d.next(secs(t0, 239), true, &present, &expected), None);
        assert_eq!(
            d.next(secs(t0, 240), true, &present, &expected),
            Some(MeterProbePlan::Address(3))
        );
        assert_eq!(
            d.next(secs(t0, 3 * 3600), true, &present, &expected),
            Some(MeterProbePlan::Address(3))
        );
    }

    #[test]
    fn a_present_meter_is_never_probed_again() {
        let t0 = Instant::now();
        let mut d = MeterDiscovery::default();
        let present = set(&[1, 2, 3]);
        let expected = set(&[1, 2, 3]);
        d.next(t0, true, &present, &expected);
        for m in [2u64, 10, 40, 600] {
            let plan = d.next(secs(t0, m * 60), true, &present, &expected);
            assert!(
                !matches!(plan, Some(MeterProbePlan::Address(1..=3))),
                "{plan:?}"
            );
        }
    }

    #[test]
    fn a_meter_that_leaves_the_present_set_is_looked_for_again() {
        let t0 = Instant::now();
        let mut d = MeterDiscovery::default();
        let expected = set(&[1, 2, 3]);
        d.next(t0, true, &set(&[1, 2, 3]), &expected);
        // 0x03's cached reading was dropped after repeated failures.
        assert_eq!(
            d.next(secs(t0, 120), true, &set(&[1, 2]), &expected),
            Some(MeterProbePlan::Address(3))
        );
    }

    #[test]
    fn one_address_per_step_rotating_through_the_missing_ones() {
        let t0 = Instant::now();
        let mut d = MeterDiscovery::default();
        let present = set(&[1]);
        let expected = set(&[1, 3, 5]);
        d.next(t0, true, &present, &expected);
        let mut got = Vec::new();
        for i in 1..=4u64 {
            if let Some(MeterProbePlan::Address(a)) =
                d.next(secs(t0, i * 120), true, &present, &expected)
            {
                got.push(a);
            }
        }
        assert_eq!(got, vec![3, 5, 3, 5]);
    }

    #[test]
    fn expected_addresses_go_before_the_others() {
        let t0 = Instant::now();
        let mut d = MeterDiscovery::default();
        let present = set(&[1]);
        let expected = set(&[1, 3]);
        d.next(t0, true, &present, &expected);
        // Both an expected (3) and unexpected addresses (2, 4..8) are due.
        assert_eq!(
            d.next(secs(t0, 120), true, &present, &expected),
            Some(MeterProbePlan::Address(3))
        );
        // The unexpected ones get the next call, not starved by 0x03.
        assert_eq!(
            d.next(secs(t0, 120), true, &present, &expected),
            Some(MeterProbePlan::Address(2))
        );
        assert_eq!(d.next(secs(t0, 120), true, &present, &expected), None);
    }

    #[test]
    fn unexpected_addresses_slow_down_after_thirty_minutes() {
        let t0 = Instant::now();
        let mut d = MeterDiscovery::default();
        let present = set(&[1]);
        let expected = set(&[1]);
        d.next(t0, true, &present, &expected);
        // First 30 minutes: every 2 minutes.
        assert_eq!(
            d.next(secs(t0, 120), true, &present, &expected),
            Some(MeterProbePlan::Address(2))
        );
        assert_eq!(
            d.next(secs(t0, 29 * 60), true, &present, &expected),
            Some(MeterProbePlan::Address(3))
        );
        // After 30 minutes: only every 15 (the last probe was at 29).
        assert_eq!(d.next(secs(t0, 31 * 60), true, &present, &expected), None);
        assert_eq!(d.next(secs(t0, 43 * 60), true, &present, &expected), None);
        assert_eq!(
            d.next(secs(t0, 44 * 60), true, &present, &expected),
            Some(MeterProbePlan::Address(4))
        );
        assert_eq!(d.next(secs(t0, 58 * 60), true, &present, &expected), None);
        assert_eq!(
            d.next(secs(t0, 59 * 60), true, &present, &expected),
            Some(MeterProbePlan::Address(5))
        );
    }

    #[test]
    fn nothing_to_probe_when_every_address_is_present() {
        let t0 = Instant::now();
        let mut d = MeterDiscovery::default();
        let all = set(&[1, 2, 3, 4, 5, 6, 7, 8]);
        d.next(t0, true, &all, &all);
        assert_eq!(d.next(secs(t0, 7200), true, &all, &all), None);
    }

    #[test]
    fn a_clock_that_steps_back_does_not_probe_or_panic() {
        let t0 = Instant::now() + Duration::from_secs(1000);
        let mut d = MeterDiscovery::default();
        let present = set(&[1]);
        let expected = set(&[1, 3]);
        d.next(t0, true, &present, &expected);
        assert_eq!(
            d.next(t0 - Duration::from_secs(500), true, &present, &expected),
            None
        );
    }

    #[test]
    fn expected_covers_names_solar_arrays_and_ignores_bad_keys() {
        let names = ["2".to_string(), "9".to_string(), "x".to_string()];
        let out = expected_addresses(names.iter(), [3u8, 0, 12], false, true);
        assert_eq!(out, set(&[2, 3]));
    }

    #[test]
    fn expected_is_every_address_while_an_ammeter_is_configured_and_none_found() {
        let none: [String; 0] = [];
        assert_eq!(
            expected_addresses(none.iter(), [], true, false),
            set(&[1, 2, 3, 4, 5, 6, 7, 8])
        );
        // Once one answers, the blanket expectation stops.
        assert!(expected_addresses(none.iter(), [], true, true).is_empty());
        // And without an ammeter configured there's nothing to expect.
        assert!(expected_addresses(none.iter(), [], false, false).is_empty());
    }
}
