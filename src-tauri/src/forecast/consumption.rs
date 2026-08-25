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
//! valid when the two counter samples are ≤ 2 h apart, share the same
//! local day (no midnight-reset artefacts), and are non-decreasing.

use chrono::Timelike;

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
    /// Exactly 24 entries indexed by local hour; `None` where no valid
    /// delta was ever observed for that hour (the caller substitutes a
    /// fallback rather than pretending the house uses nothing).
    pub hours: Vec<Option<ConsumptionHourBand>>,
    /// Distinct local days that contributed at least one valid delta.
    pub days_observed: u32,
}

impl ConsumptionProfile {
    /// `true` once [`MIN_CONSUMPTION_DAYS`] distinct days contributed.
    pub fn sufficient(&self) -> bool {
        self.days_observed >= MIN_CONSUMPTION_DAYS
    }

    /// Median kWh for a local hour, with the empty-bucket fallback: the
    /// mean of the nearest non-empty neighbouring hours (searching both
    /// directions), else 0 when the profile is entirely empty.
    pub fn median_kwh_for_hour(&self, hour: u8) -> f64 {
        let idx = hour as usize % 24;
        if let Some(b) = &self.hours[idx] {
            return b.median;
        }
        for distance in 1..24 {
            let left = self.hours[(idx + 24 - distance) % 24];
            let right = self.hours[(idx + distance) % 24];
            match (left, right) {
                (Some(l), Some(r)) => return (l.median + r.median) / 2.0,
                (Some(l), None) => return l.median,
                (None, Some(r)) => return r.median,
                (None, None) => continue,
            }
        }
        0.0
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

/// Build the hour-of-day profile from counter samples (any ordering; the
/// samples are sorted by timestamp internally).
pub fn build_consumption_profile(rows: &[ConsumptionCounterRow]) -> ConsumptionProfile {
    let mut sorted: Vec<&ConsumptionCounterRow> =
        rows.iter().filter(|r| r.kwh.is_some()).collect();
    sorted.sort_by_key(|r| r.timestamp);

    // Energy per (local day, local hour): the SUM of valid deltas inside
    // that hour. Summing (not mediating) per-interval deltas makes the fit
    // cadence-agnostic — a 20 s poll writes ~180 tiny deltas that add up
    // to the same hourly kWh as one hourly delta. Guardrails apply per
    // interval: gaps > 2 h are skipped entirely (the missing energy must
    // not be smeared into one bucket), midnight resets and negative
    // glitches contribute nothing.
    let mut hour_sums:
        std::collections::BTreeMap<(chrono::NaiveDate, u8), f64> = Default::default();
    let mut days: std::collections::BTreeSet<chrono::NaiveDate> = Default::default();

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
        // A midnight counter reset shows up as a day change; a mid-day
        // reset as a negative delta. Neither is real consumption.
        if a_local.date_naive() != b_local.date_naive() {
            continue;
        }
        let (Some(a_kwh), Some(b_kwh)) = (a.kwh, b.kwh) else {
            continue;
        };
        let delta = b_kwh - a_kwh;
        if delta < 0.0 {
            continue;
        }
        // Attribute to the hour containing the interval midpoint — an
        // interval never spans midnight (same-day check above), so this
        // keeps every delta inside its own hour or an adjacent one.
        let mid_ts = a.timestamp + gap / 2;
        let (day, hour) = match chrono::DateTime::from_timestamp(mid_ts, 0) {
            Some(dt) => {
                let local = dt.with_timezone(&chrono::Local);
                (local.date_naive(), local.hour() as u8)
            }
            None => continue,
        };
        *hour_sums.entry((day, hour)).or_insert(0.0) += delta;
        days.insert(day);
    }

    // Per-hour-of-day statistics ACROSS days.
    let mut buckets: Vec<Vec<f64>> = vec![Vec::new(); 24];
    for ((_, hour), sum) in hour_sums {
        buckets[hour as usize].push(sum);
    }

    let hours = buckets
        .into_iter()
        .enumerate()
        .map(|(hour, mut sums)| {
            if sums.is_empty() {
                return None;
            }
            sums.sort_by(|a, b| a.partial_cmp(b).expect("sums are finite"));
            let band = ConsumptionHourBand {
                hour: hour as u8,
                p25: percentile_sorted(&sums, 25.0),
                median: percentile_sorted(&sums, 50.0),
                p75: percentile_sorted(&sums, 75.0),
            };
            Some(band)
        })
        .collect();

    ConsumptionProfile {
        hours,
        days_observed: days.len() as u32,
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
