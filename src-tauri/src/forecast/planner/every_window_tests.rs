//! Plan every cheap window (issue #453): the window set, the pick, and the
//! one-gap horizon. Pinned to a fixed local date so nothing reads the clock.

use super::*;
use crate::forecast::simulate::{simulate_battery, SimHourInput};
use crate::settings::TariffSlot;
use chrono::TimeZone;

fn tariff(slots: &[(&str, &str, f64)]) -> TariffConfig {
    TariffConfig {
        slots: slots
            .iter()
            .map(|(s, e, r)| TariffSlot {
                start: s.to_string(),
                end: e.to_string(),
                rate: *r,
            })
            .collect(),
    }
}

/// Cosy: three equal cheap periods, 04:00–07:00, 13:00–16:00, 22:00–00:00.
fn cosy() -> TariffConfig {
    tariff(&[
        ("00:00", "04:00", 0.27),
        ("04:00", "07:00", 0.13),
        ("07:00", "13:00", 0.27),
        ("13:00", "16:00", 0.13),
        ("16:00", "19:00", 0.40),
        ("19:00", "22:00", 0.27),
        ("22:00", "23:59", 0.13),
    ])
}

fn flux() -> TariffConfig {
    tariff(&[
        ("00:00", "02:00", 0.26),
        ("02:00", "05:00", 0.09),
        ("05:00", "16:00", 0.26),
        ("16:00", "21:00", 0.35),
        ("21:00", "23:59", 0.26),
    ])
}

/// Intelligent Octopus Go: one cheap period split across midnight.
fn iog() -> TariffConfig {
    tariff(&[
        ("00:00", "05:30", 0.07),
        ("05:30", "23:30", 0.30),
        ("23:30", "23:59", 0.07),
    ])
}

fn starts(windows: &[ChargeWindow]) -> Vec<(u16, u16)> {
    windows.iter().map(|w| (w.start_min, w.end_min)).collect()
}

const H: u16 = 60;

#[test]
fn cosy_has_three_cheap_windows_in_time_order_from_now() {
    let at_0330 = cheap_import_windows(&cosy(), 3 * H + 30, 30);
    assert_eq!(
        starts(&at_0330),
        vec![(4 * H, 7 * H), (13 * H, 16 * H), (22 * H, 1439)]
    );
    // From 08:00 the 04:00 window is tomorrow's, so it comes last.
    let at_0800 = cheap_import_windows(&cosy(), 8 * H, 30);
    assert_eq!(
        starts(&at_0800),
        vec![(13 * H, 16 * H), (22 * H, 1439), (4 * H, 7 * H)]
    );
    assert!(!at_0800[0].tomorrow && !at_0800[1].tomorrow && at_0800[2].tomorrow);
    // Inside a window, that window's next occurrence is tomorrow's: last.
    let inside = cheap_import_windows(&cosy(), 14 * H, 30);
    assert_eq!(inside[0].start_min, 22 * H);
    assert_eq!(inside[2].start_min, 13 * H);
}

#[test]
fn one_cheap_window_tariffs_give_one_window() {
    assert_eq!(
        starts(&cheap_import_windows(&flux(), 12 * H, 30)),
        vec![(2 * H, 5 * H)]
    );
    // The wrapping merge stands for the whole period; its post-midnight
    // fragment is inside it and dropped, the 29-minute fragment too short.
    assert_eq!(
        starts(&cheap_import_windows(&iog(), 22 * H, 30)),
        vec![(23 * H + 30, 5 * H + 30)]
    );
    assert_eq!(
        starts(&cheap_import_windows(&iog(), 2 * H, 30)),
        vec![(23 * H + 30, 5 * H + 30)]
    );
}

#[test]
fn cheap_means_within_the_tolerance_of_the_cheapest() {
    // 13.00p and 13.04p are one price; 13.10p is not.
    let t = tariff(&[
        ("00:00", "04:00", 0.27),
        ("04:00", "07:00", 0.1300),
        ("07:00", "13:00", 0.27),
        ("13:00", "16:00", 0.1304),
        ("16:00", "22:00", 0.27),
        ("22:00", "23:59", 0.1310),
    ]);
    assert_eq!(
        starts(&cheap_import_windows(&t, 0, 30)),
        vec![(4 * H, 7 * H), (13 * H, 16 * H)]
    );
}

#[test]
fn touching_slots_at_one_price_are_one_window() {
    // 13.02p then 13.00p: one cheap period, so one window whose charge has
    // to last past both, not two that each end the other's horizon.
    let t = tariff(&[
        ("00:00", "04:00", 0.27),
        ("04:00", "05:00", 0.1302),
        ("05:00", "07:00", 0.1300),
        ("07:00", "13:00", 0.27),
        ("13:00", "16:00", 0.13),
        ("16:00", "23:59", 0.27),
    ]);
    assert_eq!(
        starts(&cheap_import_windows(&t, 0, 30)),
        vec![(4 * H, 7 * H), (13 * H, 16 * H)]
    );
    // The same across midnight.
    let t = tariff(&[
        ("00:00", "02:00", 0.1300),
        ("02:00", "13:00", 0.27),
        ("13:00", "16:00", 0.13),
        ("16:00", "23:00", 0.27),
        ("23:00", "23:59", 0.1302),
    ]);
    assert_eq!(
        starts(&cheap_import_windows(&t, 12 * H, 30)),
        vec![(13 * H, 16 * H), (23 * H, 2 * H)]
    );
}

#[test]
fn windows_shorter_than_the_minimum_are_left_out() {
    let t = tariff(&[
        ("00:00", "04:00", 0.27),
        ("04:00", "07:00", 0.13),
        ("07:00", "13:00", 0.27),
        ("13:00", "13:20", 0.13),
        ("13:20", "23:59", 0.27),
    ]);
    assert_eq!(
        starts(&cheap_import_windows(&t, 0, 30)),
        vec![(4 * H, 7 * H)]
    );
    // A 20-minute window doesn't set the cheapest rate either.
    let t = tariff(&[
        ("00:00", "04:00", 0.27),
        ("04:00", "07:00", 0.13),
        ("07:00", "13:00", 0.27),
        ("13:00", "13:20", 0.05),
        ("13:20", "23:59", 0.27),
    ]);
    assert_eq!(
        starts(&cheap_import_windows(&t, 0, 30)),
        vec![(4 * H, 7 * H)]
    );
}

#[test]
fn no_tariff_rows_means_no_windows() {
    assert!(cheap_import_windows(&tariff(&[]), 0, 30).is_empty());
}

#[test]
fn every_window_pick_is_the_earliest_ahead_not_the_longest() {
    // `cheapest_import_window` prefers the longer of two same-day windows.
    let t = tariff(&[
        ("00:00", "01:00", 0.10),
        ("01:00", "13:00", 0.27),
        ("13:00", "14:00", 0.10),
        ("14:00", "22:00", 0.27),
        ("22:00", "23:59", 0.10),
    ]);
    let off = plan_window(&t, 8 * H, false).unwrap();
    let on = plan_window(&t, 8 * H, true).unwrap();
    assert_eq!(off.start_min, 22 * H);
    assert_eq!(on.start_min, 13 * H);
}

#[test]
fn with_one_cheap_window_the_pick_is_the_same_on_and_off() {
    for t in [flux(), iog()] {
        for now_min in 0..1440u16 {
            assert_eq!(
                plan_window(&t, now_min, true),
                plan_window(&t, now_min, false),
                "minute {now_min}"
            );
        }
    }
}

#[test]
fn plan_needs_current_hour_follows_the_every_window_pick() {
    // At 13:05 the next cheap window is 13:30, later this hour; the
    // cheapest-by-length pick is the three hours from 22:00.
    let t = tariff(&[
        ("00:00", "01:00", 0.10),
        ("01:00", "13:30", 0.27),
        ("13:30", "14:30", 0.10),
        ("14:30", "22:00", 0.27),
        ("22:00", "23:59", 0.10),
    ]);
    let at = Local
        .from_local_datetime(&fixed_date(0).and_hms_opt(13, 5, 0).unwrap())
        .earliest()
        .unwrap();
    assert!(plan_needs_current_hour(Some(&t), at, true));
    assert!(!plan_needs_current_hour(Some(&t), at, false));
}

// ---- The planner across several cheap windows ----

use chrono::Local;

fn fixed_date(offset_days: i64) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 12).unwrap() + chrono::Duration::days(offset_days)
}

fn local_ts(offset_days: i64, h: u32, m: u32) -> i64 {
    Local
        .from_local_datetime(&fixed_date(offset_days).and_hms_opt(h, m, 0).unwrap())
        .earliest()
        .unwrap()
        .timestamp()
}

fn params(start_soc: f64) -> SimulationParams {
    SimulationParams {
        capacity_kwh: 10.0,
        start_soc_pct: start_soc,
        reserve_soc_pct: 4.0,
        max_charge_kw: 3.0,
        max_discharge_kw: 3.0,
        charge_efficiency: 0.95,
        discharge_efficiency: 0.95,
    }
}

/// 48 forward hours from `first_hour` on day 0, solar from `solar(day, h)`
/// and a flat `load` kWh an hour.
fn hours(first_hour: u32, load: f64, solar: impl Fn(i64, u32) -> f64) -> Vec<SimHourInput> {
    (0..48)
        .map(|i| {
            let hour = first_hour + i;
            let (day, h) = (i64::from(hour / 24), hour % 24);
            SimHourInput {
                timestamp: local_ts(day, h, 0),
                solar_kwh: solar(day, h),
                consumption_kwh: load,
                ..Default::default()
            }
        })
        .collect()
}

fn inputs<'a>(
    sim: &'a SimulationOutput,
    hours: &'a [SimHourInput],
    p: &'a SimulationParams,
    t: &'a TariffConfig,
    now_ts: i64,
    every: bool,
) -> PlanInputs<'a> {
    PlanInputs {
        simulation: sim,
        sim_hours: Some(hours),
        params: p,
        import_tariff: Some(t),
        target_soc_pct: 20.0,
        consumption_tomorrow_kwh: 12.0,
        consumption_sufficient: true,
        current_soc_pct: p.start_soc_pct,
        now_ts,
        running_window_start: None,
        every_cheap_window: every,
    }
}

fn window_of(rec: &PlanRecommendation) -> Option<(u16, u16)> {
    match rec {
        PlanRecommendation::Charge { window, .. }
        | PlanRecommendation::ChargeAndHold { window, .. } => {
            Some((window.start_min, window.end_min))
        }
        _ => None,
    }
}

#[test]
fn the_horizon_ends_at_the_next_cheap_window_of_any_kind() {
    let hrs = hours(4, 0.5, |_, _| 0.0);
    let t = cosy();
    let windows = cheap_import_windows(&t, 3 * H + 30, 30);
    let now = local_ts(0, 3, 30);
    let (selected, next) =
        first_reachable_occurrence_in(&hrs, &windows[0], &windows, OccurrenceAt::NextFrom(now));
    let selected = selected.unwrap();
    assert_eq!(selected.start_ts, local_ts(0, 4, 0));
    // 13:00 today is index 9 in a series starting at 04:00.
    assert_eq!(next, Some(9));
    // With the window alone, the horizon is tomorrow's 04:00.
    let (_, alone) = first_reachable_occurrence_in(
        &hrs,
        &windows[0],
        std::slice::from_ref(&windows[0]),
        OccurrenceAt::NextFrom(now),
    );
    assert_eq!(alone, Some(24));
}

#[test]
fn off_plans_the_04_00_window_against_a_whole_day() {
    // A dark winter day at 0.5 kWh an hour from 40%: by itself the 04:00
    // window has to carry the battery for 24 hours.
    let p = params(40.0);
    let hrs = hours(4, 0.5, |_, _| 0.0);
    let sim = simulate_battery(&hrs, &p);
    let t = cosy();
    let now = local_ts(0, 3, 30);
    let off = plan_overnight_charge(&inputs(&sim, &hrs, &p, &t, now, false));
    let on = plan_overnight_charge(&inputs(&sim, &hrs, &p, &t, now, true));
    let (
        PlanRecommendation::Charge { kwh: off_kwh, .. },
        PlanRecommendation::Charge { kwh: on_kwh, .. },
    ) = (&off, &on)
    else {
        panic!("both charge: off {off:?}, on {on:?}");
    };
    assert_eq!(window_of(&off).unwrap().0, 4 * H);
    assert_eq!(window_of(&on).unwrap().0, 4 * H);
    // On, 04:00 only has to reach 13:00, so it charges less.
    assert!(on_kwh < off_kwh, "on {on_kwh} off {off_kwh}");
}

#[test]
fn a_gap_already_covered_needs_no_charge_in_that_window() {
    // A sunny day: 04:00 at 90% holds to 13:00 easily.
    let p = params(90.0);
    let hrs = hours(4, 0.4, |_, h| if (9..16).contains(&h) { 1.5 } else { 0.0 });
    let sim = simulate_battery(&hrs, &p);
    let t = cosy();
    let rec = plan_overnight_charge(&inputs(&sim, &hrs, &p, &t, local_ts(0, 3, 30), true));
    assert!(
        matches!(rec, PlanRecommendation::NoChargeNeeded { .. }),
        "{rec:?}"
    );
}

#[test]
fn the_13_00_run_plans_13_00_to_hold_until_22_00() {
    // At 12:30 from 15%: the evening drain to 22:00 needs a top-up.
    let p = params(15.0);
    let hrs = hours(13, 0.8, |_, _| 0.0);
    let sim = simulate_battery(&hrs, &p);
    let t = cosy();
    let now = local_ts(0, 12, 30);
    let on = plan_overnight_charge(&inputs(&sim, &hrs, &p, &t, now, true));
    assert_eq!(window_of(&on).map(|w| w.0), Some(13 * H), "{on:?}");
    // Its overlay stops at the 22:00 window, one hour into it.
    let PlanRecommendation::Charge {
        with_charge_series, ..
    } = &on
    else {
        unreachable!()
    };
    assert_eq!(with_charge_series.last().unwrap().0, local_ts(0, 22, 0));
}

#[test]
fn the_22_00_run_plans_22_00_to_hold_until_04_00() {
    let p = params(10.0);
    let hrs = hours(22, 0.8, |_, _| 0.0);
    let sim = simulate_battery(&hrs, &p);
    let t = cosy();
    let now = local_ts(0, 21, 30);
    let on = plan_overnight_charge(&inputs(&sim, &hrs, &p, &t, now, true));
    assert_eq!(window_of(&on).map(|w| w.0), Some(22 * H), "{on:?}");
    let PlanRecommendation::Charge {
        with_charge_series, ..
    } = &on
    else {
        unreachable!()
    };
    assert_eq!(with_charge_series.last().unwrap().0, local_ts(1, 4, 0));
}

#[test]
fn the_hold_strategies_plan_the_same_window_and_gap() {
    let p = params(15.0);
    let hrs = hours(13, 0.8, |_, _| 0.0);
    let sim = simulate_battery(&hrs, &p);
    let t = cosy();
    let now = local_ts(0, 12, 30);
    let inputs = inputs(&sim, &hrs, &p, &t, now, true);
    let hold = plan_hold_through_window(&inputs);
    assert_eq!(window_of(&hold), Some((13 * H, 16 * H)), "{hold:?}");
    let PlanRecommendation::ChargeAndHold {
        with_charge_series,
        target_soc_pct,
        ..
    } = &hold
    else {
        panic!("{hold:?}");
    };
    assert_eq!(with_charge_series.last().unwrap().0, local_ts(0, 22, 0));
    assert!(*target_soc_pct < 100, "only to 22:00, not a whole day");
    let cheapest = plan_cheapest_overall(
        &inputs,
        &CostInputs {
            export_tariff: None,
            low_solar_hours: None,
            smart_charges: None,
        },
    );
    assert_eq!(window_of(&cheapest), Some((13 * H, 16 * H)), "{cheapest:?}");
}

#[test]
fn the_export_advice_ends_its_cycle_at_the_window_the_plan_picks() {
    // Cheap 13:00–14:00 and 22:00–00:00; exports pay best 16:00–19:00.
    // Planned at 10:00, every window plans 13:00, so a 16:00 export is
    // the next cycle's; once a day plans the longer 22:00, so it isn't.
    let import = tariff(&[
        ("00:00", "13:00", 0.27),
        ("13:00", "14:00", 0.10),
        ("14:00", "22:00", 0.27),
        ("22:00", "23:59", 0.10),
    ]);
    let export = tariff(&[
        ("00:00", "16:00", 0.05),
        ("16:00", "19:00", 0.30),
        ("19:00", "23:59", 0.05),
    ]);
    let hours = hours(10, 0.2, |_, _| 0.0);
    let p = params(90.0);
    let advice = |every: bool| {
        plan_export_window(&ExportPlanInputs {
            sim_hours: &hours,
            params: &p,
            export_tariff: &export,
            import_tariff: &import,
            charge_window: None,
            hold_window: None,
            floor_pct: 20.0,
            now_ts: local_ts(0, 10, 0),
            every_cheap_window: every,
        })
    };
    let beyond = |a: &ExportAdvice| matches!(a, ExportAdvice::NoExport { reason } if reason.contains("beyond this charge cycle"));
    assert!(beyond(&advice(true)), "{:?}", advice(true));
    assert!(!beyond(&advice(false)), "{:?}", advice(false));
}

#[test]
fn with_one_cheap_window_every_strategy_plans_the_same_on_and_off() {
    let t = flux();
    for (first_hour, start_soc) in [(22u32, 30.0), (1, 12.0), (12, 50.0)] {
        let p = params(start_soc);
        let hrs = hours(first_hour, 0.7, |_, h| {
            if (8..17).contains(&h) {
                1.0
            } else {
                0.0
            }
        });
        let sim = simulate_battery(&hrs, &p);
        let now = local_ts(0, first_hour, 0) - 30 * 60;
        let off = inputs(&sim, &hrs, &p, &t, now, false);
        let on = inputs(&sim, &hrs, &p, &t, now, true);
        let extras = CostInputs {
            export_tariff: None,
            low_solar_hours: None,
            smart_charges: None,
        };
        for strategy in [
            PlanStrategy::MinSoc,
            PlanStrategy::HoldWindow,
            PlanStrategy::Cheapest,
        ] {
            assert_eq!(
                plan_charge(&off, strategy, &extras),
                plan_charge(&on, strategy, &extras),
                "{strategy:?} from {first_hour}:00"
            );
        }
    }
}
