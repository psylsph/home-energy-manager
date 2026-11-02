//! Planned car charging as known load (issue #406).
//!
//! Turns the account's Intelligent Octopus dispatch list into, for each
//! forecast hour, the car's planned energy and the share of the hour a smart
//! dispatch holds the battery (issue #394). The simulator then runs the car
//! from the battery outside a hold and from the grid inside one.
//!
//! Pure: the caller decides whether the list is fresh enough to use and
//! whether an hour's history is free of car charging (`add_load`), so this
//! never counts the car twice.

use crate::octopus_dispatches::Dispatch;

/// The fastest home charger there is (three-phase 22 kW). A dispatch
/// claiming more energy than this could deliver in its time is not believed.
pub const MAX_CHARGER_KW: f64 = 22.0;

/// One forecast hour's planned car charging.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize)]
pub struct DispatchHour {
    /// Start of the hour, unix seconds.
    pub timestamp: i64,
    /// The car's planned energy in the hour, kWh.
    pub ev_kwh: f64,
    /// Share of the hour (0–1) a smart dispatch holds the battery for.
    pub held_fraction: f64,
}

/// One planned car charge as Octopus gave it (issue #406), for the page to
/// say when it runs; the hours above are what the plan simulates.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct DispatchWindow {
    /// Start and end, unix seconds.
    pub start: i64,
    pub end: i64,
    /// Whether the battery is held while it runs.
    pub held: bool,
    /// The planned energy, when Octopus gave a believable figure, kWh.
    pub kwh: Option<f64>,
}

/// [`dispatch_overlay`]'s answer: one entry per hour asked about, in order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DispatchOverlay {
    pub hours: Vec<DispatchHour>,
    /// A dispatch in the horizon gave no believable energy: its hold is
    /// modelled, its load is not.
    pub energy_unknown: bool,
    /// Car load was left out of an hour because its history may still hold
    /// the car (`add_load` said no).
    pub load_not_added: bool,
}

/// Each hour `[start, start + 1 h)` in `hour_starts` against `dispatches`.
///
/// - `held_fraction`: the time smart dispatches overlap the hour (merged, so
///   two cars at once hold it once), as `Dispatch::holds_at` decides.
/// - `ev_kwh`: each dispatch's energy pro rata to its overlap, smart or not,
///   where `add_load(hour_start)` allows. Boosts and unknown kinds add load
///   but no hold, as the poll loop never holds them.
pub fn dispatch_overlay(
    hour_starts: &[i64],
    dispatches: &[Dispatch],
    add_load: impl Fn(i64) -> bool,
) -> DispatchOverlay {
    let mut overlay = DispatchOverlay {
        hours: Vec::with_capacity(hour_starts.len()),
        ..Default::default()
    };
    for &hour_start in hour_starts {
        let hour_end = hour_start.saturating_add(HOUR_SECS);
        let mut held: Vec<(i64, i64)> = Vec::new();
        let mut ev_kwh = 0.0;
        for dispatch in dispatches {
            let (start, end) = (dispatch.start.timestamp(), dispatch.end.timestamp());
            let overlap = (end.min(hour_end) - start.max(hour_start)).max(0);
            if overlap == 0 {
                continue;
            }
            // `Dispatch::holds_at` for every moment of the overlap: the
            // overlap is inside the dispatch, so only its kind decides.
            if dispatch.is_smart() {
                held.push((start.max(hour_start), end.min(hour_end)));
            }
            let Some(energy) = believable_energy(dispatch) else {
                overlay.energy_unknown = true;
                continue;
            };
            if add_load(hour_start) {
                ev_kwh += energy * overlap as f64 / (end - start) as f64;
            } else {
                overlay.load_not_added = true;
            }
        }
        overlay.hours.push(DispatchHour {
            timestamp: hour_start,
            ev_kwh,
            held_fraction: merged_secs(&mut held) as f64 / HOUR_SECS as f64,
        });
    }
    overlay
}

/// Put the car and the hold onto the simulator's hours, matched by start
/// time. The forecast and the plan both build their hours through this, so
/// they always agree.
///
/// `net_of_profile`: the profile may still hold part of the car (only the
/// threshold trimmed it), so an hour adds just the car energy above its own
/// load. That can leave out some of the house during a dispatch, but never
/// counts the car twice.
pub fn apply_dispatch_hours(
    hours: &mut [crate::forecast::simulate::SimHourInput],
    dispatch_hours: &[DispatchHour],
    net_of_profile: bool,
) {
    for hour in hours {
        if let Some(found) = dispatch_hours
            .iter()
            .find(|dispatch_hour| dispatch_hour.timestamp == hour.timestamp)
        {
            hour.ev_kwh = if net_of_profile {
                (found.ev_kwh - hour.consumption_kwh).max(0.0)
            } else {
                found.ev_kwh
            };
            hour.held_fraction = found.held_fraction;
        }
    }
}

const HOUR_SECS: i64 = 3600;

/// The dispatch's planned energy, if it's a finite, positive figure a home
/// charger could deliver in the dispatch's time.
pub(crate) fn believable_energy(dispatch: &Dispatch) -> Option<f64> {
    believable_kwh(
        dispatch.start.timestamp(),
        dispatch.end.timestamp(),
        dispatch.energy_added_kwh,
    )
}

/// `energy_kwh` over `[start, end)` (Unix seconds), if it's a finite,
/// positive figure a home charger could deliver in that time. Shared with
/// the profile, so both trust the same Octopus figures.
pub(crate) fn believable_kwh(start: i64, end: i64, energy_kwh: Option<f64>) -> Option<f64> {
    let energy = energy_kwh?;
    let hours = (end - start) as f64 / HOUR_SECS as f64;
    (energy.is_finite() && energy > 0.0 && energy <= MAX_CHARGER_KW * hours + 1e-9)
        .then_some(energy)
}

/// Total length of `intervals` with overlaps counted once.
fn merged_secs(intervals: &mut [(i64, i64)]) -> i64 {
    intervals.sort_unstable();
    let mut total = 0;
    let mut current: Option<(i64, i64)> = None;
    for &(start, end) in intervals.iter() {
        match current {
            Some((_, current_end)) if start <= current_end => {
                current = current.map(|(s, e)| (s, e.max(end)));
            }
            _ => {
                if let Some((s, e)) = current {
                    total += e - s;
                }
                current = Some((start, end));
            }
        }
    }
    total + current.map_or(0, |(s, e)| e - s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    const H: i64 = 3600;
    /// 2026-10-17 18:00 UTC.
    const T0: i64 = 1_792_260_000;

    fn at(ts: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(ts, 0).unwrap()
    }

    fn dispatch(start: i64, end: i64, kind: Option<&str>, kwh: Option<f64>) -> Dispatch {
        Dispatch {
            start: at(start),
            end: at(end),
            kind: kind.map(str::to_string),
            energy_added_kwh: kwh,
        }
    }

    fn smart(start: i64, end: i64, kwh: f64) -> Dispatch {
        dispatch(start, end, Some("SMART"), Some(kwh))
    }

    fn hours(n: i64) -> Vec<i64> {
        (0..n).map(|i| T0 + i * H).collect()
    }

    fn always(_: i64) -> bool {
        true
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn no_dispatches_means_no_car_and_no_hold_in_every_hour() {
        let overlay = dispatch_overlay(&hours(3), &[], always);
        assert_eq!(overlay.hours.len(), 3);
        for (i, hour) in overlay.hours.iter().enumerate() {
            assert_eq!(hour.timestamp, T0 + i as i64 * H);
            assert_eq!(hour.ev_kwh, 0.0);
            assert_eq!(hour.held_fraction, 0.0);
        }
        assert!(!overlay.energy_unknown);
        assert!(!overlay.load_not_added);
    }

    #[test]
    fn a_dispatch_across_an_hour_boundary_splits_its_energy_and_hold() {
        // 18:30–19:30, 7 kWh: half in each hour.
        let overlay =
            dispatch_overlay(&hours(3), &[smart(T0 + H / 2, T0 + 3 * H / 2, 7.0)], always);
        let [first, second, third] = overlay.hours[..] else {
            panic!("three hours");
        };
        assert!(
            close(first.ev_kwh, 3.5) && close(first.held_fraction, 0.5),
            "{first:?}"
        );
        assert!(
            close(second.ev_kwh, 3.5) && close(second.held_fraction, 0.5),
            "{second:?}"
        );
        assert_eq!((third.ev_kwh, third.held_fraction), (0.0, 0.0));
    }

    #[test]
    fn energy_is_spread_by_time_over_a_long_dispatch() {
        // 18:00–20:30, 10 kWh: 4 kWh an hour, 2 kWh in the last half hour.
        let overlay = dispatch_overlay(&hours(3), &[smart(T0, T0 + 5 * H / 2, 10.0)], always);
        let kwh: Vec<f64> = overlay.hours.iter().map(|h| h.ev_kwh).collect();
        assert!(
            close(kwh[0], 4.0) && close(kwh[1], 4.0) && close(kwh[2], 2.0),
            "{kwh:?}"
        );
        assert!(close(overlay.hours[2].held_fraction, 0.5));
    }

    #[test]
    fn overlapping_smart_dispatches_hold_the_hour_once_but_add_both_cars() {
        // Two cars: 18:00–18:45 and 18:15–19:00.
        let overlay = dispatch_overlay(
            &hours(1),
            &[
                smart(T0, T0 + 45 * 60, 3.0),
                smart(T0 + 15 * 60, T0 + H, 6.0),
            ],
            always,
        );
        let hour = overlay.hours[0];
        assert!(close(hour.held_fraction, 1.0), "{hour:?}");
        assert!(close(hour.ev_kwh, 9.0), "{hour:?}");
    }

    #[test]
    fn separate_smart_dispatches_in_one_hour_add_their_held_time() {
        let overlay = dispatch_overlay(
            &hours(1),
            &[
                smart(T0, T0 + 15 * 60, 1.0),
                smart(T0 + 30 * 60, T0 + 45 * 60, 1.0),
            ],
            always,
        );
        assert!(
            close(overlay.hours[0].held_fraction, 0.5),
            "{:?}",
            overlay.hours[0]
        );
    }

    #[test]
    fn a_boost_or_unknown_kind_adds_load_but_never_holds() {
        for kind in ["BOOST", "SOMETHING_NEW"] {
            let overlay = dispatch_overlay(
                &hours(1),
                &[dispatch(T0, T0 + H, Some(kind), Some(7.0))],
                always,
            );
            let hour = overlay.hours[0];
            assert!(close(hour.ev_kwh, 7.0), "{kind}: {hour:?}");
            assert_eq!(hour.held_fraction, 0.0, "{kind}");
        }
    }

    #[test]
    fn an_unlabelled_dispatch_is_smart() {
        let overlay = dispatch_overlay(&hours(1), &[dispatch(T0, T0 + H, None, Some(7.0))], always);
        assert!(close(overlay.hours[0].held_fraction, 1.0));
    }

    #[test]
    fn unbelievable_energy_keeps_the_hold_but_adds_no_load() {
        for kwh in [
            None,
            Some(0.0),
            Some(-3.0),
            Some(f64::NAN),
            Some(f64::INFINITY),
            Some(22.1),
        ] {
            let overlay = dispatch_overlay(
                &hours(1),
                &[dispatch(T0, T0 + H, Some("SMART"), kwh)],
                always,
            );
            let hour = overlay.hours[0];
            assert_eq!(hour.ev_kwh, 0.0, "{kwh:?}");
            assert!(close(hour.held_fraction, 1.0), "{kwh:?}");
            assert!(overlay.energy_unknown, "{kwh:?}");
        }
        // 22 kWh in an hour is a 22 kW charger: believable.
        let overlay = dispatch_overlay(&hours(1), &[smart(T0, T0 + H, 22.0)], always);
        assert!(close(overlay.hours[0].ev_kwh, 22.0));
        assert!(!overlay.energy_unknown);
    }

    #[test]
    fn unknown_energy_outside_the_horizon_is_not_reported() {
        let overlay = dispatch_overlay(
            &hours(1),
            &[dispatch(T0 - 2 * H, T0 - H, Some("SMART"), None)],
            always,
        );
        assert!(!overlay.energy_unknown);
    }

    #[test]
    fn a_dispatch_already_over_or_not_yet_reached_changes_nothing() {
        let overlay = dispatch_overlay(
            &hours(2),
            &[
                smart(T0 - 2 * H, T0, 7.0),
                smart(T0 + 2 * H, T0 + 3 * H, 7.0),
            ],
            always,
        );
        for hour in &overlay.hours {
            assert_eq!((hour.ev_kwh, hour.held_fraction), (0.0, 0.0), "{hour:?}");
        }
    }

    #[test]
    fn a_dispatch_running_since_before_the_horizon_counts_only_its_rest() {
        // 17:00–19:00, 8 kWh: the 18:00 hour gets the second half.
        let overlay = dispatch_overlay(&hours(1), &[smart(T0 - H, T0 + H, 8.0)], always);
        assert!(
            close(overlay.hours[0].ev_kwh, 4.0),
            "{:?}",
            overlay.hours[0]
        );
        assert!(close(overlay.hours[0].held_fraction, 1.0));
    }

    #[test]
    fn hours_whose_history_may_hold_the_car_get_the_hold_but_not_the_load() {
        let only_second = |ts: i64| ts == T0 + H;
        let overlay = dispatch_overlay(&hours(2), &[smart(T0, T0 + 2 * H, 14.0)], only_second);
        assert_eq!(overlay.hours[0].ev_kwh, 0.0);
        assert!(close(overlay.hours[0].held_fraction, 1.0));
        assert!(close(overlay.hours[1].ev_kwh, 7.0));
        assert!(overlay.load_not_added);
    }

    fn sim_hour(timestamp: i64, consumption_kwh: f64) -> crate::forecast::simulate::SimHourInput {
        crate::forecast::simulate::SimHourInput {
            timestamp,
            consumption_kwh,
            ..Default::default()
        }
    }

    #[test]
    fn hours_get_their_car_and_hold_whole_or_net_of_the_profile() {
        let dispatch_hours = [
            DispatchHour {
                timestamp: T0,
                ev_kwh: 3.0,
                held_fraction: 1.0,
            },
            DispatchHour {
                timestamp: T0 + H,
                ev_kwh: 0.4,
                held_fraction: 0.5,
            },
        ];
        let base = vec![
            sim_hour(T0, 0.5),
            sim_hour(T0 + H, 0.5),
            sim_hour(T0 + 2 * H, 0.5),
        ];

        let mut whole = base.clone();
        apply_dispatch_hours(&mut whole, &dispatch_hours, false);
        let got: Vec<_> = whole.iter().map(|h| (h.ev_kwh, h.held_fraction)).collect();
        assert_eq!(got, vec![(3.0, 1.0), (0.4, 0.5), (0.0, 0.0)]);

        // Net of the profile: only the car above the hour's own load, and
        // never below nothing; the hold is unchanged.
        let mut net = base;
        apply_dispatch_hours(&mut net, &dispatch_hours, true);
        let got: Vec<_> = net.iter().map(|h| (h.ev_kwh, h.held_fraction)).collect();
        assert_eq!(got, vec![(2.5, 1.0), (0.0, 0.5), (0.0, 0.0)]);
        assert!(net.iter().all(|h| h.consumption_kwh == 0.5));
    }

    #[test]
    fn load_not_added_is_only_reported_where_there_was_load_to_add() {
        let overlay = dispatch_overlay(&hours(2), &[smart(T0, T0 + H, 7.0)], |ts| ts == T0);
        assert!(!overlay.load_not_added, "the refused hour had no car");
    }
}
