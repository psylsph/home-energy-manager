//! Cell spread trend per battery module (issue #393).
//!
//! A failing cell shows first as a spread between the module's cells that
//! keeps growing. Only the near-full spread (`full_spread_mv`) is compared:
//! the day's worst spread depends on how deep the battery was cycled, so it
//! cannot be trended like for like.
//!
//! [`spread_trend`] is pure. The daily alert and `/api/battery-health` both
//! use it, so the page and the notification cannot disagree.

use std::collections::BTreeMap;

use chrono::{Duration, NaiveDate};
use serde::Serialize;

use super::battery_health::BatteryHealthRow;

/// The recent window: today and the six days before it.
pub const RECENT_DAYS: i64 = 7;
/// Near-full days the recent window needs before it counts.
pub const RECENT_MIN_DAYS: usize = 4;
/// The baseline window, in days before today (inclusive).
pub const BASELINE_FROM_DAYS_AGO: i64 = 60;
pub const BASELINE_TO_DAYS_AGO: i64 = 31;
/// Near-full days the baseline window needs before it counts.
pub const BASELINE_MIN_DAYS: usize = 7;
/// Growth over the baseline that counts as widening, in mV.
pub const WIDENING_DELTA_MV: f64 = 25.0;
/// A widening spread below this is still healthy, in mV.
pub const WIDENING_FLOOR_MV: f64 = 60.0;
/// A recent spread at or above this is flagged whatever the baseline, in mV.
pub const HIGH_SPREAD_MV: f64 = 150.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpreadStatus {
    Ok,
    Widening,
    High,
    /// Too few near-full days to judge.
    Insufficient,
}

impl SpreadStatus {
    /// Whether this status is worth telling the user about.
    pub fn is_alert(self) -> bool {
        matches!(self, Self::Widening | Self::High)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpreadTrend {
    pub module_index: i64,
    pub status: SpreadStatus,
    /// Median near-full spread over the recent window, mV.
    pub recent_mv: Option<f64>,
    /// Median near-full spread over the baseline window, mV.
    pub baseline_mv: Option<f64>,
    /// Near-full days in the recent window.
    pub recent_days: usize,
    /// [`RECENT_MIN_DAYS`], so the page need not copy it.
    pub recent_min_days: usize,
}

/// Each module's spread trend as of `today`, in slot order. Every module slot
/// present in `rows` gets an entry, even one with only old data.
pub fn spread_trend(rows: &[BatteryHealthRow], today: NaiveDate) -> Vec<SpreadTrend> {
    let recent_from = today - Duration::days(RECENT_DAYS - 1);
    let baseline_from = today - Duration::days(BASELINE_FROM_DAYS_AGO);
    let baseline_to = today - Duration::days(BASELINE_TO_DAYS_AGO);

    // Slot -> (recent spreads, baseline spreads).
    let mut modules: BTreeMap<i64, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
    for row in rows {
        let windows = modules.entry(row.module_index).or_default();
        let Some(spread) = row.full_spread_mv.filter(|v| v.is_finite()) else {
            continue;
        };
        let Ok(day) = NaiveDate::parse_from_str(&row.day, "%Y-%m-%d") else {
            continue;
        };
        if (recent_from..=today).contains(&day) {
            windows.0.push(spread);
        } else if (baseline_from..=baseline_to).contains(&day) {
            windows.1.push(spread);
        }
    }

    modules
        .into_iter()
        .map(|(module_index, (recent, baseline))| {
            let recent_mv = median(recent.clone());
            let baseline_mv = (baseline.len() >= BASELINE_MIN_DAYS)
                .then(|| median(baseline))
                .flatten();
            let status = match (recent.len() >= RECENT_MIN_DAYS, recent_mv, baseline_mv) {
                (true, Some(r), _) if r >= HIGH_SPREAD_MV => SpreadStatus::High,
                (true, Some(r), Some(b))
                    if r >= WIDENING_FLOOR_MV && r - b >= WIDENING_DELTA_MV =>
                {
                    SpreadStatus::Widening
                }
                (true, Some(_), Some(_)) => SpreadStatus::Ok,
                _ => SpreadStatus::Insufficient,
            };
            SpreadTrend {
                module_index,
                status,
                recent_mv,
                baseline_mv,
                recent_days: recent.len(),
                recent_min_days: RECENT_MIN_DAYS,
            }
        })
        .collect()
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 30).unwrap()
    }

    /// A row `days_ago` before [`today`] for `module` with a near-full spread.
    fn row(module: i64, days_ago: i64, full_spread_mv: Option<f64>) -> BatteryHealthRow {
        let day = today() - chrono::Duration::days(days_ago);
        BatteryHealthRow {
            day: day.format("%Y-%m-%d").to_string(),
            module_index: module,
            min_cell_v: Some(3.30),
            max_cell_v: Some(3.40),
            max_spread_mv: Some(100.0),
            full_spread_mv,
            cycles: Some(500),
            max_temp_c: Some(20.0),
            capacity_ah: None,
            design_capacity_ah: None,
            bms_firmware: Some(3015),
            samples: 100,
            soc_jumps: 0,
            soc_stuck: 0,
        }
    }

    /// Rows for `module`: a baseline of `baseline_mv` on days 31..=40 ago and
    /// `recent_mv` on days 0..=6 ago.
    fn history(module: i64, baseline_mv: f64, recent_mv: f64) -> Vec<BatteryHealthRow> {
        let mut rows: Vec<_> = (31..=40)
            .map(|d| row(module, d, Some(baseline_mv)))
            .collect();
        rows.extend((0..=6).map(|d| row(module, d, Some(recent_mv))));
        rows
    }

    fn only(trends: Vec<SpreadTrend>) -> SpreadTrend {
        assert_eq!(trends.len(), 1, "{trends:?}");
        trends.into_iter().next().unwrap()
    }

    #[test]
    fn steady_spread_is_ok() {
        let t = only(spread_trend(&history(0, 30.0, 32.0), today()));
        assert_eq!(t.status, SpreadStatus::Ok);
        assert_eq!(t.recent_mv, Some(32.0));
        assert_eq!(t.baseline_mv, Some(30.0));
        assert_eq!(t.recent_days, 7);
    }

    #[test]
    fn spread_that_has_grown_is_widening() {
        let t = only(spread_trend(&history(0, 40.0, 70.0), today()));
        assert_eq!(t.status, SpreadStatus::Widening);
        assert!(t.status.is_alert());
    }

    #[test]
    fn growth_below_the_floor_is_still_ok() {
        // 20 -> 50 mV is a big relative jump but a healthy pack.
        let t = only(spread_trend(&history(0, 20.0, 50.0), today()));
        assert_eq!(t.status, SpreadStatus::Ok);
    }

    #[test]
    fn growth_just_under_the_delta_is_ok() {
        let t = only(spread_trend(&history(0, 50.0, 74.0), today()));
        assert_eq!(t.status, SpreadStatus::Ok);
    }

    #[test]
    fn high_spread_is_flagged_without_a_baseline() {
        // A new install has no baseline yet, but 160 mV near full is bad.
        let rows: Vec<_> = (0..=6).map(|d| row(0, d, Some(160.0))).collect();
        let t = only(spread_trend(&rows, today()));
        assert_eq!(t.status, SpreadStatus::High);
        assert_eq!(t.baseline_mv, None);
    }

    #[test]
    fn high_wins_over_widening() {
        let t = only(spread_trend(&history(0, 40.0, 200.0), today()));
        assert_eq!(t.status, SpreadStatus::High);
    }

    #[test]
    fn without_a_baseline_a_moderate_spread_is_insufficient() {
        let rows: Vec<_> = (0..=6).map(|d| row(0, d, Some(80.0))).collect();
        let t = only(spread_trend(&rows, today()));
        assert_eq!(t.status, SpreadStatus::Insufficient);
        assert_eq!(t.recent_mv, Some(80.0));
    }

    #[test]
    fn too_few_recent_near_full_days_is_insufficient() {
        // A battery capped below 95% never records a near-full spread.
        // Only 3 recent near-full days: not enough even for High.
        let mut rows: Vec<_> = (31..=40).map(|d| row(0, d, Some(30.0))).collect();
        rows.extend((0..=2).map(|d| row(0, d, Some(200.0))));
        rows.extend((3..=6).map(|d| row(0, d, None)));
        let t = only(spread_trend(&rows, today()));
        assert_eq!(t.status, SpreadStatus::Insufficient);
        assert_eq!(t.recent_days, 3);
    }

    #[test]
    fn too_few_baseline_days_is_insufficient() {
        let mut rows: Vec<_> = (31..=36).map(|d| row(0, d, Some(30.0))).collect();
        rows.extend((0..=6).map(|d| row(0, d, Some(90.0))));
        let t = only(spread_trend(&rows, today()));
        assert_eq!(t.status, SpreadStatus::Insufficient);
        assert_eq!(t.baseline_mv, None);
    }

    #[test]
    fn worst_spread_is_never_trended() {
        // max_spread_mv swings with depth of discharge; only full_spread counts.
        let mut rows = history(0, 30.0, 30.0);
        for r in &mut rows {
            r.max_spread_mv = Some(if r.day.as_str() >= "2026-10-24" {
                400.0
            } else {
                10.0
            });
        }
        assert_eq!(only(spread_trend(&rows, today())).status, SpreadStatus::Ok);
    }

    #[test]
    fn median_ignores_a_single_outlier_day() {
        let mut rows = history(0, 30.0, 35.0);
        let last = rows.iter_mut().find(|r| r.day == "2026-10-30").unwrap();
        last.full_spread_mv = Some(400.0);
        let t = only(spread_trend(&rows, today()));
        assert_eq!(t.status, SpreadStatus::Ok);
        assert_eq!(t.recent_mv, Some(35.0));
    }

    #[test]
    fn median_of_an_even_count_averages_the_middle_pair() {
        let rows: Vec<_> = [(0, 100.0), (1, 110.0), (2, 120.0), (3, 130.0)]
            .into_iter()
            .map(|(d, v)| row(0, d, Some(v)))
            .collect();
        assert_eq!(only(spread_trend(&rows, today())).recent_mv, Some(115.0));
    }

    #[test]
    fn days_between_the_windows_and_older_than_the_baseline_are_ignored() {
        let mut rows = history(0, 30.0, 32.0);
        // Days 7..=30 ago and 61+ ago are outside both windows.
        rows.extend((7..=30).map(|d| row(0, d, Some(500.0))));
        rows.extend((61..=90).map(|d| row(0, d, Some(1.0))));
        let t = only(spread_trend(&rows, today()));
        assert_eq!(t.status, SpreadStatus::Ok);
        assert_eq!(t.baseline_mv, Some(30.0));
        assert_eq!(t.recent_mv, Some(32.0));
    }

    #[test]
    fn future_days_are_ignored() {
        // A clock that was once ahead can leave rows dated after today.
        let mut rows = history(0, 30.0, 32.0);
        rows.extend((1..=5).map(|d| row(0, -d, Some(500.0))));
        assert_eq!(only(spread_trend(&rows, today())).recent_mv, Some(32.0));
    }

    #[test]
    fn non_finite_spreads_are_ignored() {
        let mut rows = history(0, 30.0, 32.0);
        rows.push(row(0, 0, Some(f64::NAN)));
        rows.push(row(0, 1, Some(f64::INFINITY)));
        let t = only(spread_trend(&rows, today()));
        assert_eq!(t.recent_mv, Some(32.0));
        assert_eq!(t.recent_days, 7);
    }

    #[test]
    fn modules_are_judged_independently_in_slot_order() {
        let mut rows = history(1, 40.0, 70.0);
        rows.extend(history(0, 30.0, 31.0));
        let trends = spread_trend(&rows, today());
        assert_eq!(trends.len(), 2);
        assert_eq!(trends[0].module_index, 0);
        assert_eq!(trends[0].status, SpreadStatus::Ok);
        assert_eq!(trends[1].module_index, 1);
        assert_eq!(trends[1].status, SpreadStatus::Widening);
    }

    #[test]
    fn a_module_with_only_old_data_is_listed_as_insufficient() {
        // A removed battery still has history, but nothing recent to judge.
        let rows: Vec<_> = (31..=45).map(|d| row(2, d, Some(30.0))).collect();
        let t = only(spread_trend(&rows, today()));
        assert_eq!(t.module_index, 2);
        assert_eq!(t.status, SpreadStatus::Insufficient);
        assert_eq!(t.recent_mv, None);
        assert_eq!(t.recent_days, 0);
    }

    #[test]
    fn malformed_days_are_ignored() {
        let mut rows = history(0, 30.0, 32.0);
        let mut bad = row(0, 0, Some(500.0));
        bad.day = "not-a-day".into();
        rows.push(bad);
        assert_eq!(only(spread_trend(&rows, today())).recent_mv, Some(32.0));
    }

    #[test]
    fn no_rows_means_no_trends() {
        assert!(spread_trend(&[], today()).is_empty());
    }
}
