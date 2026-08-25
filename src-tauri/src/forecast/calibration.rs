//! Performance-ratio calibration (issue #283, Phase 0).
//!
//! Converts the site's own generation history into a single *performance
//! ratio* (PR): the fraction of theoretically available insolation the
//! installed array actually converts to kWh. PR absorbs tilt, azimuth,
//! shading, soiling, inverter clipping and understated nameplates, which
//! is how the forecast model avoids asking the user for panel geometry.
//!
//! Method: for each *complete* past local day, compare actual solar
//! generation (`today_solar_kwh` max, from `history.db::readings`) against
//! the insolation delivered that day (integral of hourly
//! `shortwave_radiation` in kWh/m²) times the configured rated kWp:
//!
//! ```text
//! ratio_day = actual_kwh / (insolation_kwh_per_m2 × rated_kwp)
//! PR        = clamp(median(ratio_day), 0.05, 1.5)
//! ```
//!
//! Days are rejected when they would bias the median: partial poll
//! coverage (< 12 distinct local hours), very dark days (insolation below
//! 0.5 kWh/m² — the ratio is dominated by noise), days with negligible
//! actual generation (< 0.5 kWh), days without radiation data, and the
//! current (incomplete) day. Fewer than 5 usable days yields `None` — the
//! caller must show "not enough history yet" rather than a guess.

use std::collections::BTreeMap;

use crate::forecast::solar::SolarForecastSample;
use crate::history::DailySolarTotal;

/// Minimum number of usable days before a PR is reported. Below this the
/// caller must show "not enough history yet" — a guess would look like
/// a prediction.
pub const MIN_CALIBRATION_DAYS: usize = 5;
/// Days delivering less insolation than this (kWh/m²) are rejected: the
/// actual/radiation ratio is dominated by noise in near-dark conditions.
const MIN_DAILY_INSOLATION_KWH_M2: f64 = 0.5;
/// Days with less actual generation than this (kWh) are rejected — dead
/// or zero-export days say nothing about conversion efficiency.
const MIN_DAILY_ACTUAL_KWH: f64 = 0.5;
/// A day must have readings in at least this many distinct local hours
/// to count as fully polled; partial days understate the counter.
const MIN_DAY_COVERAGE_HOURS: u32 = 12;
/// Physical clamp for the fitted ratio. A median above ~1.5 means the
/// configured kWp is badly understated (or readings are wrong); below
/// ~0.05 the array would be effectively dead.
const PR_CLAMP_MIN: f64 = 0.05;
const PR_CLAMP_MAX: f64 = 1.5;

/// Fit the performance ratio from the trailing history. `samples` are the
/// past portion of the latest radiation fetch (any forward samples are
/// simply not matched by a past-day total, so mixing them in is harmless).
/// `today` is passed in rather than read from the clock so the whole
/// function is deterministic under test.
///
/// Returns `None` when fewer than [`MIN_CALIBRATION_DAYS`] usable days
/// remain after the rejection filters (dark / partial / dead / no-data /
/// current day).
pub fn calibrate_performance_ratio(
    samples: &[SolarForecastSample],
    totals: &[DailySolarTotal],
    rated_kw: f64,
    today: chrono::NaiveDate,
) -> Option<f64> {
    if rated_kw <= 0.0 {
        return None;
    }

    // Integrate hourly radiation per local day. Samples are hourly on
    // UTC boundaries but generation days run local-midnight to
    // local-midnight (the inverter counter resets on local midnight), so
    // group by the sample's local date. Σ(W/m² × 1 h) / 1000 = kWh/m².
    let mut insolation: BTreeMap<chrono::NaiveDate, f64> = BTreeMap::new();
    for s in samples {
        let Some(utc) = chrono::DateTime::from_timestamp(s.timestamp, 0) else {
            continue;
        };
        let local_date = utc.with_timezone(&chrono::Local).date_naive();
        *insolation.entry(local_date).or_insert(0.0) += s.shortwave_radiation as f64 / 1000.0;
    }

    let mut ratios: Vec<f64> = totals
        .iter()
        .filter(|t| {
            t.date < today
                && t.hours_covered >= MIN_DAY_COVERAGE_HOURS
                && t.kwh >= MIN_DAILY_ACTUAL_KWH
        })
        .filter_map(|t| {
            let day_insolation = insolation.get(&t.date).copied()?;
            if day_insolation < MIN_DAILY_INSOLATION_KWH_M2 {
                return None;
            }
            Some(t.kwh / (day_insolation * rated_kw))
        })
        .collect();

    if ratios.len() < MIN_CALIBRATION_DAYS {
        return None;
    }

    // Median (not mean): a single bad dongle day or a stale counter must
    // not drag the fitted ratio around.
    ratios.sort_by(|a, b| a.partial_cmp(b).expect("ratios are finite"));
    let n = ratios.len();
    let median = if n % 2 == 1 {
        ratios[n / 2]
    } else {
        (ratios[n / 2 - 1] + ratios[n / 2]) / 2.0
    };
    Some(median.clamp(PR_CLAMP_MIN, PR_CLAMP_MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn d(y: i32, m: u32, day: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    /// Epoch seconds for a local wall-clock hour on `date`, so sample
    /// day-grouping works on any test-runner timezone.
    fn local_hour(date: chrono::NaiveDate, hour: u32) -> i64 {
        chrono::Local
            .from_local_datetime(&date.and_hms_opt(hour, 0, 0).unwrap())
            .earliest()
            .unwrap()
            .timestamp()
    }

    /// Ten daylight hours (07:00–16:00) at a constant `g` W/m². With hourly
    /// integration that day delivers `g × 10 / 1000` kWh/m².
    fn day_of_radiation(date: chrono::NaiveDate, g: f32) -> Vec<SolarForecastSample> {
        (7..17)
            .map(|h| SolarForecastSample {
                timestamp: local_hour(date, h as u32),
                shortwave_radiation: g,
                direct_radiation: None,
                cloud_cover: None,
            })
            .collect()
    }

    fn total(date: chrono::NaiveDate, kwh: f64, hours: u32) -> DailySolarTotal {
        DailySolarTotal {
            date,
            kwh,
            hours_covered: hours,
        }
    }

    /// Five consecutive complete days ending 2025-06-09, "today" fixed at
    /// 2025-06-15. With `g = 700` each day delivers 7.0 kWh/m², so at
    /// 5 kWp the model expects 35 kWh/day.
    const RATED: f64 = 5.0;
    const G: f32 = 700.0;

    fn five_valid_days() -> (Vec<SolarForecastSample>, Vec<DailySolarTotal>, chrono::NaiveDate) {
        let days = [
            d(2025, 6, 5),
            d(2025, 6, 6),
            d(2025, 6, 7),
            d(2025, 6, 8),
            d(2025, 6, 9),
        ];
        // Ratios 0.7, 0.8, 0.9, 1.0, 0.6 → median 0.8.
        let kwhs = [24.5, 28.0, 31.5, 35.0, 21.0];
        let samples: Vec<_> = days.iter().flat_map(|&day| day_of_radiation(day, G)).collect();
        let totals: Vec<_> = days
            .iter()
            .zip(kwhs)
            .map(|(&day, kwh)| total(day, kwh, 14))
            .collect();
        (samples, totals, d(2025, 6, 15))
    }

    #[test]
    fn calibrates_to_median_daily_ratio() {
        let (samples, totals, today) = five_valid_days();
        let pr = calibrate_performance_ratio(&samples, &totals, RATED, today).unwrap();
        assert!((pr - 0.8).abs() < 1e-9, "pr = {pr}");
    }

    #[test]
    fn returns_none_with_too_few_usable_days() {
        let (samples, totals, today) = five_valid_days();
        // Drop the last usable day → only 4 remain.
        let samples = samples[..samples.len() - 10].to_vec();
        let totals = totals[..4].to_vec();
        assert!(calibrate_performance_ratio(&samples, &totals, RATED, today).is_none());
    }

    #[test]
    fn returns_none_with_no_history() {
        let (samples, _, today) = five_valid_days();
        assert!(calibrate_performance_ratio(&samples, &[], RATED, today).is_none());
        assert!(calibrate_performance_ratio(&[], &[], RATED, today).is_none());
    }

    #[test]
    fn returns_none_for_zero_rated_capacity() {
        let (samples, totals, today) = five_valid_days();
        assert!(calibrate_performance_ratio(&samples, &totals, 0.0, today).is_none());
    }

    #[test]
    fn skips_dark_days() {
        // 4 valid days plus a complete-but-dark day (0.2 kWh/m²): the dark
        // day must be excluded, leaving too few usable days.
        let (samples, mut totals, today) = five_valid_days();
        let samples = samples[..samples.len() - 10].to_vec();
        totals.truncate(4);
        let dark = d(2025, 6, 4);
        let mut dark_samples = day_of_radiation(dark, 20.0);
        dark_samples.extend(samples);
        totals.push(total(dark, 7.0, 14)); // complete, plausible actuals
        assert!(calibrate_performance_ratio(&dark_samples, &totals, RATED, today).is_none());

        // With 5 valid days the dark day still must not skew the median.
        let (samples, mut totals, today) = five_valid_days();
        let dark = d(2025, 6, 4);
        let mut all = day_of_radiation(dark, 20.0);
        all.extend(samples);
        totals.push(total(dark, 7.0, 14));
        let pr = calibrate_performance_ratio(&all, &totals, RATED, today).unwrap();
        assert!((pr - 0.8).abs() < 1e-9, "pr = {pr}");
    }

    #[test]
    fn skips_partial_coverage_days() {
        // A day where the app only polled 6 distinct hours under-states
        // the daily counter and would bias the ratio low.
        let (samples, mut totals, today) = five_valid_days();
        let samples = samples[..samples.len() - 10].to_vec();
        totals.truncate(4);
        let partial_day = d(2025, 6, 4);
        let mut all = day_of_radiation(partial_day, G);
        all.extend(samples);
        totals.push(total(partial_day, 28.0, 6));
        assert!(calibrate_performance_ratio(&all, &totals, RATED, today).is_none());
    }

    #[test]
    fn skips_days_with_negligible_actual_generation() {
        // A dead day (0.1 kWh actual) is unusable regardless of sunshine.
        let (samples, mut totals, today) = five_valid_days();
        let samples = samples[..samples.len() - 10].to_vec();
        totals.truncate(4);
        let dead = d(2025, 6, 4);
        let mut all = day_of_radiation(dead, G);
        all.extend(samples);
        totals.push(total(dead, 0.1, 14));
        assert!(calibrate_performance_ratio(&all, &totals, RATED, today).is_none());
    }

    #[test]
    fn skips_days_without_radiation_samples() {
        // A total whose day has no radiation data (e.g. fetch gap) cannot
        // produce a ratio and must not count towards the minimum.
        let (samples, mut totals, today) = five_valid_days();
        let samples = samples[..samples.len() - 10].to_vec();
        totals.truncate(4);
        totals.push(total(d(2025, 6, 4), 28.0, 14)); // complete, no samples
        assert!(calibrate_performance_ratio(&samples, &totals, RATED, today).is_none());
    }

    #[test]
    fn ignores_today_and_future_days() {
        // The current day is incomplete and future days have no actuals;
        // neither may influence the median even with wild ratios.
        let (mut samples, mut totals, today) = five_valid_days();
        // Today at 300% and tomorrow at 5% — both must be ignored.
        samples.extend(day_of_radiation(today, G));
        samples.extend(day_of_radiation(d(2025, 6, 16), G));
        totals.push(total(today, 105.0, 14)); // ratio 3.0
        totals.push(total(d(2025, 6, 16), 1.75, 14)); // ratio 0.05
        let pr = calibrate_performance_ratio(&samples, &totals, RATED, today).unwrap();
        assert!((pr - 0.8).abs() < 1e-9, "pr = {pr}");
    }

    #[test]
    fn clamps_absurd_medians_to_physical_range() {
        // Consistently seeing 3× the rated output means the nameplate is
        // understated — cap PR at 1.5 rather than predicting the impossible.
        let days = [
            d(2025, 6, 5),
            d(2025, 6, 6),
            d(2025, 6, 7),
            d(2025, 6, 8),
            d(2025, 6, 9),
        ];
        let samples: Vec<_> = days.iter().flat_map(|&day| day_of_radiation(day, G)).collect();
        let today = d(2025, 6, 15);

        let high: Vec<_> = days.iter().map(|&day| total(day, 105.0, 14)).collect();
        let pr = calibrate_performance_ratio(&samples, &high, RATED, today).unwrap();
        assert!((pr - 1.5).abs() < 1e-9, "pr = {pr}");

        // Ratio 0.02/day (0.7 kWh actual ≥ dead-day floor) clamps up to 0.05.
        let low: Vec<_> = days.iter().map(|&day| total(day, 0.7, 14)).collect();
        let pr = calibrate_performance_ratio(&samples, &low, RATED, today).unwrap();
        assert!((pr - 0.05).abs() < 1e-9, "pr = {pr}");
    }

    #[test]
    fn median_of_even_count_averages_middle_two() {
        // Six usable days → median is the mean of the 3rd and 4th ratios.
        let days = [
            d(2025, 6, 4),
            d(2025, 6, 5),
            d(2025, 6, 6),
            d(2025, 6, 7),
            d(2025, 6, 8),
            d(2025, 6, 9),
        ];
        // Ratios 0.6, 0.7, 0.8, 0.9, 1.0, 1.1 → median (0.8 + 0.9) / 2.
        let kwhs = [21.0, 24.5, 28.0, 31.5, 35.0, 38.5];
        let samples: Vec<_> = days.iter().flat_map(|&day| day_of_radiation(day, G)).collect();
        let totals: Vec<_> = days
            .iter()
            .zip(kwhs)
            .map(|(&day, kwh)| total(day, kwh, 14))
            .collect();
        let pr = calibrate_performance_ratio(&samples, &totals, RATED, d(2025, 6, 15)).unwrap();
        assert!((pr - 0.85).abs() < 1e-9, "pr = {pr}");
    }
}
