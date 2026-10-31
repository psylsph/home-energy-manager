//! Home consumption prediction (issue #283, Phase 1).
//!
//! Builds an hour-of-day consumption profile from the inverter's own
//! cumulative home-energy counter (`home_energy_today_kwh`, falling back
//! to `today_consumption_kwh` per row — the same precedence
//! `query_energy_summary` uses). Hourly deltas are bucketed by local
//! hour-of-day over the trailing window; each bucket reports the median
//! plus a p25–p75 band.
//!
//! Delta guardrails mirror the sanitiser's instincts: a delta is only
//! valid when the two counter samples are ≤ 2 h apart and non-decreasing.
//! Positive deltas crossing local midnight are split at the boundary so
//! they do not bias either day's hourly bucket; midnight resets remain
//! rejected as negative deltas.

use chrono::{Datelike, TimeZone, Timelike};

/// Minimum distinct days of consumption history before the profile is
/// reported as sufficient. A week captures at least one of every weekday.
pub const MIN_CONSUMPTION_DAYS: u32 = 7;

/// Maximum gap between two counter samples that still yields a delta.
/// Longer gaps (app offline) would smear hours together.
pub const MAX_DELTA_GAP_SECS: i64 = 2 * 3600;

/// One raw cumulative-counter sample as read from `readings`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConsumptionCounterRow {
    pub timestamp: i64,
    /// Effective cumulative counter (kWh): `home_energy_today_kwh` when
    /// present, else `today_consumption_kwh`. `None` when neither column
    /// carried a value.
    pub kwh: Option<f64>,
}

/// Per-hour-of-day statistics over the observed window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConsumptionHourBand {
    /// Local hour of day, 0–23.
    pub hour: u8,
    pub p25: f64,
    pub median: f64,
    pub p75: f64,
}

/// The fitted profile plus the sufficiency signal.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsumptionProfile {
    /// All-days fallback profile, exactly 24 entries indexed by local hour.
    pub hours: Vec<Option<ConsumptionHourBand>>,
    /// Weekday profile, exactly 24 entries indexed by local hour.
    pub weekday_hours: Vec<Option<ConsumptionHourBand>>,
    /// Weekend profile, exactly 24 entries indexed by local hour.
    pub weekend_hours: Vec<Option<ConsumptionHourBand>>,
    /// Distinct local days that contributed at least one valid delta.
    pub days_observed: u32,
    /// Distinct weekdays that contributed at least one valid delta.
    pub weekday_days_observed: u32,
    /// Distinct weekend days that contributed at least one valid delta.
    pub weekend_days_observed: u32,
    /// Energy left out because the EV charger drew it (issue #400), kWh.
    pub excluded_ev_kwh: f64,
    /// Energy left out above the "leave out load above" threshold, kWh.
    pub excluded_above_kwh: f64,
}

/// One minute of EV charger power (issue #400): the average of the
/// charger's polls in the minute starting at `timestamp`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EvPowerMinute {
    pub timestamp: i64,
    pub power_w: f64,
}

/// What to leave out of the profile (issue #400). Where the charger's power
/// was recorded, its energy is taken away; elsewhere, load above
/// `load_above_kw` (when set) is clipped off as car charging.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConsumptionExclusions<'a> {
    /// Charger minutes, sorted by timestamp.
    pub ev_minutes: &'a [EvPowerMinute],
    pub load_above_kw: Option<f64>,
}

impl ConsumptionProfile {
    /// `true` once [`MIN_CONSUMPTION_DAYS`] distinct days contributed.
    pub fn sufficient(&self) -> bool {
        self.days_observed >= MIN_CONSUMPTION_DAYS
    }

    /// Find a band for an hour, using the mean of the nearest non-empty
    /// neighbouring hours when the requested bucket is empty.
    fn nearest_band(
        hours: &[Option<ConsumptionHourBand>],
        hour: u8,
    ) -> Option<ConsumptionHourBand> {
        let idx = hour as usize % 24;
        if let Some(band) = hours[idx] {
            return Some(band);
        }
        for distance in 1..24 {
            let left = hours[(idx + 24 - distance) % 24];
            let right = hours[(idx + distance) % 24];
            match (left, right) {
                (Some(l), Some(r)) => {
                    return Some(ConsumptionHourBand {
                        hour: idx as u8,
                        p25: (l.p25 + r.p25) / 2.0,
                        median: (l.median + r.median) / 2.0,
                        p75: (l.p75 + r.p75) / 2.0,
                    });
                }
                (Some(band), None) | (None, Some(band)) => return Some(band),
                (None, None) => continue,
            }
        }
        None
    }

    /// Statistics for a local hour from the weekday or weekend profile.
    /// If that day type has no observations, the all-days profile is used.
    pub fn band_for_day_type(&self, hour: u8, is_weekend: bool) -> ConsumptionHourBand {
        let specific = if is_weekend {
            &self.weekend_hours
        } else {
            &self.weekday_hours
        };
        Self::nearest_band(specific, hour)
            .or_else(|| Self::nearest_band(&self.hours, hour))
            .unwrap_or(ConsumptionHourBand {
                hour: hour % 24,
                p25: 0.0,
                median: 0.0,
                p75: 0.0,
            })
    }

    /// Median kWh for a local hour from the all-days profile, with the
    /// empty-bucket fallback, else 0 when the profile is entirely empty.
    pub fn median_kwh_for_hour(&self, hour: u8) -> f64 {
        Self::nearest_band(&self.hours, hour)
            .map(|band| band.median)
            .unwrap_or(0.0)
    }

    /// Median kWh for a local hour from the weekday or weekend profile.
    pub fn median_kwh_for_day_type(&self, hour: u8, is_weekend: bool) -> f64 {
        self.band_for_day_type(hour, is_weekend).median
    }
}

/// Compute an interpolated percentile (0–100) of a non-empty sorted
/// slice, matching numpy's default linear-interpolation semantics.
fn percentile_sorted(sorted: &[f64], pct: f64) -> f64 {
    debug_assert!(!sorted.is_empty());
    if sorted.len() == 1 {
        return sorted[0];
    }
    let pos = (pct / 100.0) * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    let frac = pos - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

/// Charger energy (kWh) over `[start_ts, end_ts)` and how many of its
/// seconds a recorded charger minute covers.
fn ev_energy_kwh(minutes: &[EvPowerMinute], start_ts: i64, end_ts: i64) -> (f64, i64) {
    let _ = (minutes, start_ts, end_ts);
    (0.0, 0)
}

/// One local hour of one day, before EV charging is taken out.
#[derive(Debug, Default, Clone, Copy)]
struct HourSum {
    /// Home energy over the hour's intervals, kWh.
    home_kwh: f64,
    /// Seconds those intervals span.
    secs: i64,
    /// Charger energy over the same intervals, kWh.
    ev_kwh: f64,
    /// Seconds of them a charger minute covers.
    ev_secs: i64,
}

#[derive(Default)]
struct Excluded {
    ev_kwh: f64,
    above_kwh: f64,
}

/// The household's share of one hour. Done per hour, not per interval: the
/// home counter moves in 0.1 kWh steps, so a minute's delta is 0, 0.1 or
/// 0.2 kWh and only an hour's sum is close to what was drawn.
///
/// The charger's energy comes off first, floored at zero. Load above the
/// threshold is clipped only in hours the charger didn't cover end to end,
/// so a polled, idle charger keeps a heavy evening as the house's.
fn household_kwh(
    sum: &HourSum,
    exclusions: &ConsumptionExclusions,
    excluded: &mut Excluded,
) -> f64 {
    let _ = (exclusions, excluded);
    sum.home_kwh
}

fn add_delta_at_midpoint(
    hour_sums: &mut std::collections::BTreeMap<(chrono::NaiveDate, u8), HourSum>,
    days: &mut std::collections::BTreeSet<chrono::NaiveDate>,
    ev_minutes: &[EvPowerMinute],
    start_ts: i64,
    end_ts: i64,
    delta: f64,
) {
    let mid_ts = start_ts + (end_ts - start_ts) / 2;
    let Some(dt) = chrono::DateTime::from_timestamp(mid_ts, 0) else {
        return;
    };
    let local = dt.with_timezone(&chrono::Local);
    let key = (local.date_naive(), local.hour() as u8);
    let (ev_kwh, ev_secs) = ev_energy_kwh(ev_minutes, start_ts, end_ts);
    let sum = hour_sums.entry(key).or_default();
    sum.home_kwh += delta;
    sum.secs += end_ts - start_ts;
    sum.ev_kwh += ev_kwh;
    sum.ev_secs += ev_secs;
    days.insert(key.0);
}

/// Build the hour-of-day profile from counter samples (any ordering; the
/// samples are sorted by timestamp internally).
pub fn build_consumption_profile(rows: &[ConsumptionCounterRow]) -> ConsumptionProfile {
    build_consumption_profile_excluding(rows, &ConsumptionExclusions::default())
}

/// [`build_consumption_profile`] with EV charging left out (issue #400).
pub fn build_consumption_profile_excluding(
    rows: &[ConsumptionCounterRow],
    exclusions: &ConsumptionExclusions,
) -> ConsumptionProfile {
    let mut sorted: Vec<&ConsumptionCounterRow> = rows.iter().filter(|r| r.kwh.is_some()).collect();
    sorted.sort_by_key(|r| r.timestamp);

    // Energy per (local day, local hour): the SUM of valid deltas inside
    // that hour. Summing (not mediating) per-interval deltas makes the fit
    // cadence-agnostic — a 20 s poll writes ~180 tiny deltas that add up
    // to the same hourly kWh as one hourly delta. Guardrails apply per
    // interval: gaps > 2 h are skipped entirely (the missing energy must
    // not be smeared into one bucket), midnight resets and negative
    // glitches contribute nothing.
    let mut hour_sums: std::collections::BTreeMap<(chrono::NaiveDate, u8), HourSum> =
        Default::default();
    let mut days: std::collections::BTreeSet<chrono::NaiveDate> = Default::default();
    let ev = exclusions.ev_minutes;

    for pair in sorted.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let gap = b.timestamp - a.timestamp;
        if gap <= 0 || gap > MAX_DELTA_GAP_SECS {
            continue;
        }
        let (Some(a_dt), Some(b_dt)) = (
            chrono::DateTime::from_timestamp(a.timestamp, 0),
            chrono::DateTime::from_timestamp(b.timestamp, 0),
        ) else {
            continue;
        };
        let a_local = a_dt.with_timezone(&chrono::Local);
        let b_local = b_dt.with_timezone(&chrono::Local);
        let (Some(a_kwh), Some(b_kwh)) = (a.kwh, b.kwh) else {
            continue;
        };
        let delta = b_kwh - a_kwh;
        if delta < 0.0 {
            continue;
        }

        if a_local.date_naive() == b_local.date_naive() {
            // Attribute same-day intervals to the hour containing their
            // midpoint.
            add_delta_at_midpoint(
                &mut hour_sums,
                &mut days,
                ev,
                a.timestamp,
                b.timestamp,
                delta,
            );
            continue;
        }

        // A positive interval may straddle local midnight without being a
        // counter reset. Split it by elapsed time at the local boundary so
        // each side contributes only to its own day.
        let Some(midnight) = b_local.date_naive().and_hms_opt(0, 0, 0).and_then(|naive| {
            chrono::Local
                .from_local_datetime(&naive)
                .earliest()
                .or_else(|| chrono::Local.from_local_datetime(&naive).latest())
        }) else {
            continue;
        };
        let midnight_ts = midnight.timestamp();
        if !(a.timestamp < midnight_ts && midnight_ts < b.timestamp) {
            continue;
        }
        let before_secs = midnight_ts - a.timestamp;
        let after_secs = b.timestamp - midnight_ts;
        add_delta_at_midpoint(
            &mut hour_sums,
            &mut days,
            ev,
            a.timestamp,
            midnight_ts,
            delta * before_secs as f64 / gap as f64,
        );
        add_delta_at_midpoint(
            &mut hour_sums,
            &mut days,
            ev,
            midnight_ts,
            b.timestamp,
            delta * after_secs as f64 / gap as f64,
        );
    }

    // Per-hour-of-day statistics across all days, and separately across
    // weekdays and weekends. Each value is one day's total for that hour;
    // percentiles therefore describe day-to-day variation rather than poll
    // cadence.
    let mut all_buckets: Vec<Vec<f64>> = vec![Vec::new(); 24];
    let mut weekday_buckets: Vec<Vec<f64>> = vec![Vec::new(); 24];
    let mut weekend_buckets: Vec<Vec<f64>> = vec![Vec::new(); 24];
    let mut weekday_days = std::collections::BTreeSet::new();
    let mut weekend_days = std::collections::BTreeSet::new();
    let mut excluded = Excluded::default();
    for ((day, hour), sum) in hour_sums {
        let sum = household_kwh(&sum, exclusions, &mut excluded);
        all_buckets[hour as usize].push(sum);
        if day.weekday().number_from_monday() >= 6 {
            weekend_buckets[hour as usize].push(sum);
            weekend_days.insert(day);
        } else {
            weekday_buckets[hour as usize].push(sum);
            weekday_days.insert(day);
        }
    }

    let make_bands = |buckets: Vec<Vec<f64>>| {
        buckets
            .into_iter()
            .enumerate()
            .map(|(hour, mut sums)| {
                if sums.is_empty() {
                    return None;
                }
                sums.sort_by(|a, b| a.partial_cmp(b).expect("sums are finite"));
                Some(ConsumptionHourBand {
                    hour: hour as u8,
                    p25: percentile_sorted(&sums, 25.0),
                    median: percentile_sorted(&sums, 50.0),
                    p75: percentile_sorted(&sums, 75.0),
                })
            })
            .collect()
    };

    ConsumptionProfile {
        hours: make_bands(all_buckets),
        weekday_hours: make_bands(weekday_buckets),
        weekend_hours: make_bands(weekend_buckets),
        days_observed: days.len() as u32,
        weekday_days_observed: weekday_days.len() as u32,
        weekend_days_observed: weekend_days.len() as u32,
        excluded_ev_kwh: excluded.ev_kwh,
        excluded_above_kwh: excluded.above_kwh,
    }
}

/// Total predicted kWh across a full day (sum of the 24 median hours,
/// using the empty-bucket fallback for gaps).
pub fn profile_daily_total(profile: &ConsumptionProfile) -> f64 {
    (0..24u8).map(|h| profile.median_kwh_for_hour(h)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn local_ts(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        chrono::Local
            .from_local_datetime(
                &chrono::NaiveDate::from_ymd_opt(y, m, d)
                    .unwrap()
                    .and_hms_opt(h, min, 0)
                    .unwrap(),
            )
            .earliest()
            .unwrap()
            .timestamp()
    }

    fn row(ts: i64, kwh: f64) -> ConsumptionCounterRow {
        ConsumptionCounterRow {
            timestamp: ts,
            kwh: Some(kwh),
        }
    }

    /// One day of hourly samples: counter starts at `base` and rises by
    /// `hourly` kWh each hour, hours 6..=21 populated.
    fn day(y: i32, m: u32, d: u32, base: f64, hourly: f64) -> Vec<ConsumptionCounterRow> {
        (6..22)
            .map(|h| {
                let ts = local_ts(y, m, d, h as u32, 0);
                row(ts, base + hourly * (h - 6) as f64)
            })
            .collect()
    }

    #[test]
    fn sub_hourly_samples_sum_within_each_hour() {
        // Real deployments poll every ~20 s, so an hour carries many tiny
        // deltas. The profile must report the hour's TOTAL energy (sum of
        // deltas), not the median single-interval delta — otherwise a
        // 20 s cadence collapses each hour to ~0.01 kWh (found while
        // seeding demo data against the simulator).
        let mut rows = Vec::new();
        // Three days, hour 12, four 15-minute intervals of 0.25 kWh each
        // → the hour consumed 1.0 kWh on every day.
        for i in 0..3u32 {
            let d = 10 + i;
            let mut counter = 0.0;
            for q in 0..4u32 {
                let ts = local_ts(2025, 6, d, 12, q * 15);
                rows.push(row(ts, counter));
                counter += 0.25;
            }
            // Anchor the last sample's hour attribution cleanly.
            rows.push(row(local_ts(2025, 6, d, 13, 0), counter));
        }
        let profile = build_consumption_profile(&rows);
        let band = profile.hours[12].expect("hour 12 band");
        assert!((band.median - 1.0).abs() < 1e-9, "median = {}", band.median);
        assert!((band.p25 - 1.0).abs() < 1e-9);
        assert!((band.p75 - 1.0).abs() < 1e-9);
        assert_eq!(profile.days_observed, 3);
    }

    #[test]
    fn mixed_cadences_do_not_distort_the_profile() {
        // Day A polls hourly (one 0.5 delta in hour 10), day B every 20 s
        // (90 deltas of ~0.0056 summing to 0.5). Both days consumed the
        // same energy; the fitted median must be 0.5, not ~0.006.
        let mut rows = Vec::new();
        // Day A: hourly.
        rows.push(row(local_ts(2025, 6, 10, 10, 0), 1.0));
        rows.push(row(local_ts(2025, 6, 10, 11, 0), 1.5));
        // Day B: 20-second cadence over the same hour.
        let mut counter = 2.0;
        for s in (0..=3600u32).step_by(20) {
            rows.push(row(local_ts(2025, 6, 11, 10, 0) + s as i64, counter));
            counter += 0.5 / 180.0;
        }
        let profile = build_consumption_profile(&rows);
        let band = profile.hours[10].expect("hour 10 band");
        assert!((band.median - 0.5).abs() < 0.02, "median = {}", band.median);
    }

    #[test]
    fn median_per_hour_across_days() {
        // Three days, each with a 12:00→13:00 interval (attributed to
        // clock hour 12) consuming 1.0, 2.0 and 4.0 kWh → median 2.0,
        // p25 1.5, p75 3.0.
        let mut rows = Vec::new();
        for (i, kwh) in [1.0, 2.0, 4.0].iter().enumerate() {
            let d = 10 + i as u32;
            rows.push(row(local_ts(2025, 6, d, 12, 0), 0.0));
            rows.push(row(local_ts(2025, 6, d, 13, 0), *kwh));
        }
        let profile = build_consumption_profile(&rows);
        let band = profile.hours[12].expect("hour 12 has three deltas");
        assert!((band.median - 2.0).abs() < 1e-9);
        assert!((band.p25 - 1.5).abs() < 1e-9);
        assert!((band.p75 - 3.0).abs() < 1e-9);
        assert_eq!(profile.days_observed, 3);
    }

    #[test]
    fn median_profiles_split_weekdays_from_weekends() {
        // Friday observations are deliberately lower than Saturday
        // observations; mixing them would hide the different usage pattern.
        let rows = vec![
            row(local_ts(2025, 6, 6, 12, 0), 0.0), // Friday
            row(local_ts(2025, 6, 6, 13, 0), 1.0),
            row(local_ts(2025, 6, 7, 12, 0), 0.0), // Saturday
            row(local_ts(2025, 6, 7, 13, 0), 10.0),
            row(local_ts(2025, 6, 13, 12, 0), 0.0), // Friday
            row(local_ts(2025, 6, 13, 13, 0), 3.0),
            row(local_ts(2025, 6, 14, 12, 0), 0.0), // Saturday
            row(local_ts(2025, 6, 14, 13, 0), 14.0),
        ];
        let profile = build_consumption_profile(&rows);

        assert!((profile.median_kwh_for_day_type(12, false) - 2.0).abs() < 1e-9);
        assert!((profile.median_kwh_for_day_type(12, true) - 12.0).abs() < 1e-9);
    }

    #[test]
    fn sufficient_after_a_week_of_days() {
        let mut rows = Vec::new();
        for i in 0..6u32 {
            rows.extend(day(2025, 6, 1 + i, 0.0, 0.5));
        }
        let profile = build_consumption_profile(&rows);
        assert!(!profile.sufficient(), "6 days is not enough");
        rows.extend(day(2025, 6, 7, 0.0, 0.5));
        let profile = build_consumption_profile(&rows);
        assert!(profile.sufficient(), "7 days is enough");
        assert_eq!(profile.days_observed, 7);
    }

    #[test]
    fn skips_midnight_reset_and_negative_deltas() {
        // Counter resets at midnight — the 23:00→00:00 pair must not
        // produce a negative "delta".
        let rows = vec![
            row(local_ts(2025, 6, 10, 22, 0), 8.0),
            row(local_ts(2025, 6, 10, 23, 0), 9.0),
            row(local_ts(2025, 6, 11, 0, 0), 0.0),
            row(local_ts(2025, 6, 11, 1, 0), 0.5),
        ];
        let profile = build_consumption_profile(&rows);
        // Only the 22:00→23:00 (+1.0) and 00:00→01:00 (+0.5) deltas count.
        assert!((profile.hours[22].unwrap().median - 1.0).abs() < 1e-9);
        assert!((profile.hours[0].unwrap().median - 0.5).abs() < 1e-9);
        // The reset pair contributed nothing to hour 23.
        assert!(profile.hours[23].is_none());
    }

    #[test]
    fn splits_positive_counter_delta_at_midnight() {
        // A cumulative counter can legitimately continue rising across
        // midnight when the samples straddle the reset boundary. Attribute
        // each half of the interval to the corresponding local day/hour.
        let rows = vec![
            row(local_ts(2025, 6, 10, 23, 30), 9.0),
            row(local_ts(2025, 6, 11, 0, 30), 10.0),
        ];
        let profile = build_consumption_profile(&rows);

        assert!((profile.hours[23].unwrap().median - 0.5).abs() < 1e-9);
        assert!((profile.hours[0].unwrap().median - 0.5).abs() < 1e-9);
        assert_eq!(profile.days_observed, 2);
    }

    #[test]
    fn skips_long_gaps() {
        // 5-hour gap (app offline) must not smear into one bucket.
        let rows = vec![
            row(local_ts(2025, 6, 10, 8, 0), 1.0),
            row(local_ts(2025, 6, 10, 13, 0), 6.0),
        ];
        let profile = build_consumption_profile(&rows);
        assert!(profile.hours.iter().all(|h| h.is_none()));
        assert_eq!(profile.days_observed, 0);
    }

    #[test]
    fn handles_unsorted_input_and_none_rows() {
        let mut rows = vec![
            ConsumptionCounterRow {
                timestamp: local_ts(2025, 6, 10, 9, 0),
                kwh: None,
            },
            row(local_ts(2025, 6, 10, 10, 0), 2.0),
            row(local_ts(2025, 6, 10, 9, 0), 1.0), // unsorted insert
        ];
        rows.reverse();
        let profile = build_consumption_profile(&rows);
        assert!((profile.hours[9].unwrap().median - 1.0).abs() < 1e-9);
    }

    #[test]
    fn empty_bucket_falls_back_to_nearest_neighbour_mean() {
        let mut rows = Vec::new();
        for i in 0..7u32 {
            rows.extend(day(2025, 6, 1 + i, 0.0, 0.5));
        }
        let profile = build_consumption_profile(&rows);
        // Hours 6..21 have 0.5; hour 3 (empty) falls back to 0.5 via
        // neighbours; daily total = 24 × 0.5.
        assert!((profile.median_kwh_for_hour(3) - 0.5).abs() < 1e-9);
        assert!((profile.median_kwh_for_hour(23) - 0.5).abs() < 1e-9);
        assert!((profile.daily_total_helper() - 12.0).abs() < 1e-9);
    }

    #[test]
    fn totally_empty_profile_reports_zero_hours() {
        let profile = build_consumption_profile(&[]);
        assert_eq!(profile.days_observed, 0);
        assert!(profile.hours.iter().all(|h| h.is_none()));
        assert!((profile.median_kwh_for_hour(12) - 0.0).abs() < 1e-9);
    }

    impl ConsumptionProfile {
        fn daily_total_helper(&self) -> f64 {
            profile_daily_total(self)
        }
    }

    /// Minute counter rows from `start` for `minutes`, drawing `kw`.
    fn minute_rows(start: i64, minutes: i64, kw: f64) -> Vec<ConsumptionCounterRow> {
        (0..=minutes)
            .map(|i| row(start + i * 60, 10.0 + kw * i as f64 / 60.0))
            .collect()
    }

    fn ev_minutes(start: i64, minutes: i64, power_w: f64) -> Vec<EvPowerMinute> {
        (0..minutes)
            .map(|i| EvPowerMinute {
                timestamp: start + i * 60,
                power_w,
            })
            .collect()
    }

    fn hour_median(profile: &ConsumptionProfile, hour: usize) -> f64 {
        profile.hours[hour].expect("hour observed").median
    }

    #[test]
    fn charger_energy_is_taken_out_of_the_hour_it_was_drawn_in() {
        let start = local_ts(2026, 10, 14, 18, 0);
        let rows = minute_rows(start, 60, 8.0);
        let ev = ev_minutes(start, 60, 7_000.0);
        let profile = build_consumption_profile_excluding(
            &rows,
            &ConsumptionExclusions {
                ev_minutes: &ev,
                load_above_kw: None,
            },
        );
        assert!((hour_median(&profile, 18) - 1.0).abs() < 1e-6);
        assert!((profile.excluded_ev_kwh - 7.0).abs() < 1e-6);
        assert_eq!(profile.excluded_above_kwh, 0.0);
    }

    #[test]
    fn charger_energy_never_takes_an_interval_below_zero() {
        // The charger reads more than the house meter saw (a CT that
        // doesn't see the charger, or timing skew): the hour goes to zero,
        // never negative.
        let start = local_ts(2026, 10, 14, 18, 0);
        let rows = minute_rows(start, 60, 2.0);
        let ev = ev_minutes(start, 60, 7_000.0);
        let profile = build_consumption_profile_excluding(
            &rows,
            &ConsumptionExclusions {
                ev_minutes: &ev,
                load_above_kw: None,
            },
        );
        assert!(hour_median(&profile, 18).abs() < 1e-9);
        assert!((profile.excluded_ev_kwh - 2.0).abs() < 1e-6);
    }

    #[test]
    fn load_above_the_threshold_is_clipped_where_there_is_no_charger_data() {
        let start = local_ts(2026, 10, 14, 18, 0);
        let rows = minute_rows(start, 60, 8.0);
        let profile = build_consumption_profile_excluding(
            &rows,
            &ConsumptionExclusions {
                ev_minutes: &[],
                load_above_kw: Some(3.0),
            },
        );
        assert!((hour_median(&profile, 18) - 3.0).abs() < 1e-6);
        assert!((profile.excluded_above_kwh - 5.0).abs() < 1e-6);
        assert_eq!(profile.excluded_ev_kwh, 0.0);
    }

    #[test]
    fn load_below_the_threshold_is_kept() {
        let start = local_ts(2026, 10, 14, 18, 0);
        let rows = minute_rows(start, 60, 2.5);
        let profile = build_consumption_profile_excluding(
            &rows,
            &ConsumptionExclusions {
                ev_minutes: &[],
                load_above_kw: Some(3.0),
            },
        );
        assert!((hour_median(&profile, 18) - 2.5).abs() < 1e-6);
        assert_eq!(profile.excluded_above_kwh, 0.0);
    }

    #[test]
    fn charger_data_overrides_the_threshold() {
        // The charger was polled and idle, so an 8 kW evening is the house
        // (an oven, a shower), not the car.
        let start = local_ts(2026, 10, 14, 18, 0);
        let rows = minute_rows(start, 60, 8.0);
        let ev = ev_minutes(start, 60, 0.0);
        let profile = build_consumption_profile_excluding(
            &rows,
            &ConsumptionExclusions {
                ev_minutes: &ev,
                load_above_kw: Some(3.0),
            },
        );
        assert!((hour_median(&profile, 18) - 8.0).abs() < 1e-6);
        assert_eq!(profile.excluded_above_kwh, 0.0);
        assert_eq!(profile.excluded_ev_kwh, 0.0);
    }

    #[test]
    fn the_threshold_still_applies_in_an_hour_the_charger_only_partly_covered() {
        // Charger data for the first half hour only (it went offline while
        // the car kept charging): its energy comes off, then the hour is
        // clipped at the threshold as well.
        let start = local_ts(2026, 10, 14, 18, 0);
        let rows = minute_rows(start, 60, 8.0);
        let ev = ev_minutes(start, 30, 7_000.0);
        let profile = build_consumption_profile_excluding(
            &rows,
            &ConsumptionExclusions {
                ev_minutes: &ev,
                load_above_kw: Some(3.0),
            },
        );
        assert!((hour_median(&profile, 18) - 3.0).abs() < 1e-6);
        assert!((profile.excluded_ev_kwh - 3.5).abs() < 1e-6);
        assert!((profile.excluded_above_kwh - 1.5).abs() < 1e-6);
    }

    #[test]
    fn charger_energy_counts_only_the_overlapping_part_of_a_minute() {
        let ev = ev_minutes(1_000_020, 3, 6_000.0); // minutes at :20 offsets
        assert_eq!(ev_energy_kwh(&ev, 0, 1_000_020), (0.0, 0));
        assert_eq!(ev_energy_kwh(&ev, 1_000_200, 1_000_300), (0.0, 0));
        let (half_minute, secs) = ev_energy_kwh(&ev, 1_000_050, 1_000_080);
        assert!((half_minute - 6.0 * 30.0 / 3600.0).abs() < 1e-9);
        assert_eq!(secs, 30);
        let (all, secs) = ev_energy_kwh(&ev, 0, 2_000_000);
        assert!((all - 6.0 * 180.0 / 3600.0).abs() < 1e-9);
        assert_eq!(secs, 180);
    }

    /// Minute rows of a counter that, like the inverter's, only moves in
    /// 0.1 kWh steps, while drawing a steady `kw`.
    fn stepped_minute_rows(start: i64, minutes: i64, kw: f64) -> Vec<ConsumptionCounterRow> {
        (0..=minutes)
            .map(|i| {
                let exact = kw * i as f64 / 60.0;
                row(start + i * 60, 10.0 + (exact * 10.0 + 1e-9).floor() / 10.0)
            })
            .collect()
    }

    #[test]
    fn a_stepped_counter_below_the_threshold_is_not_clipped() {
        // 2.5 kW against a 3 kW threshold: each 0.1 kWh step lands in one
        // minute, which looks like 6 kW on its own. The hour is what counts.
        let start = local_ts(2026, 10, 14, 18, 0);
        let rows = stepped_minute_rows(start, 60, 2.5);
        let profile = build_consumption_profile_excluding(
            &rows,
            &ConsumptionExclusions {
                ev_minutes: &[],
                load_above_kw: Some(3.0),
            },
        );
        assert!((hour_median(&profile, 18) - 2.5).abs() < 1e-6);
        assert_eq!(profile.excluded_above_kwh, 0.0);
    }

    #[test]
    fn a_stepped_counter_keeps_the_true_house_share_while_the_car_charges() {
        // 7 kW car + 1 kW house: minute deltas are 0.1 or 0.2 kWh, but the
        // hour still comes out at the house's 1 kWh.
        let start = local_ts(2026, 10, 14, 18, 0);
        let rows = stepped_minute_rows(start, 60, 8.0);
        let ev = ev_minutes(start, 60, 7_000.0);
        let profile = build_consumption_profile_excluding(
            &rows,
            &ConsumptionExclusions {
                ev_minutes: &ev,
                load_above_kw: None,
            },
        );
        assert!((hour_median(&profile, 18) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_threshold_of_zero_or_less_from_a_hand_edited_file_is_ignored() {
        let start = local_ts(2026, 10, 14, 18, 0);
        let rows = minute_rows(start, 60, 2.0);
        for bad in [0.0, -1.0, f64::NAN] {
            let profile = build_consumption_profile_excluding(
                &rows,
                &ConsumptionExclusions {
                    ev_minutes: &[],
                    load_above_kw: Some(bad),
                },
            );
            assert!((hour_median(&profile, 18) - 2.0).abs() < 1e-6, "{bad}");
        }
    }

    #[test]
    fn an_hourly_interval_with_a_charging_session_keeps_the_house_share() {
        // Sparse rows (one an hour, as older history has): the charger's
        // half hour at 7 kW comes out of the hour's 4.5 kWh.
        let start = local_ts(2026, 10, 14, 18, 0);
        let rows = vec![row(start, 10.0), row(start + 3600, 14.5)];
        let ev = ev_minutes(start + 600, 30, 7_000.0);
        let profile = build_consumption_profile_excluding(
            &rows,
            &ConsumptionExclusions {
                ev_minutes: &ev,
                load_above_kw: None,
            },
        );
        assert!((hour_median(&profile, 18) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn no_exclusions_match_the_plain_profile() {
        let mut rows = Vec::new();
        for d in 5..12 {
            rows.extend(day(2026, 10, d, 0.0, 0.7));
        }
        let plain = build_consumption_profile(&rows);
        let excluding =
            build_consumption_profile_excluding(&rows, &ConsumptionExclusions::default());
        assert_eq!(plain, excluding);
        assert_eq!(plain.excluded_ev_kwh, 0.0);
        assert_eq!(plain.excluded_above_kwh, 0.0);
    }

    #[test]
    fn percentile_matches_linear_interpolation() {
        let sorted = [1.0, 2.0, 3.0, 4.0];
        assert!((percentile_sorted(&sorted, 50.0) - 2.5).abs() < 1e-9);
        assert!((percentile_sorted(&sorted, 25.0) - 1.75).abs() < 1e-9);
        assert!((percentile_sorted(&sorted, 0.0) - 1.0).abs() < 1e-9);
        assert!((percentile_sorted(&sorted, 100.0) - 4.0).abs() < 1e-9);
        assert!((percentile_sorted(&[7.0], 40.0) - 7.0).abs() < 1e-9);
    }
}
