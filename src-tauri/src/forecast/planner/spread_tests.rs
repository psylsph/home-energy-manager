//! Spread the planned charge across the cheap window (issue #483). Pinned
//! to a fixed local date so nothing reads the clock.

use super::super::{
    plan_hold_through_window, plan_overnight_charge, PlanInputs, PlanRecommendation,
};
use super::*;
use crate::forecast::simulate::{
    simulate_battery, SimHourInput, SimulationOutput, SimulationParams,
};
use crate::settings::{TariffConfig, TariffSlot};
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

/// Cheap 02:00–05:00 (3 h).
fn flux() -> TariffConfig {
    tariff(&[
        ("00:00", "02:00", 0.26),
        ("02:00", "05:00", 0.09),
        ("05:00", "16:00", 0.26),
        ("16:00", "21:00", 0.35),
        ("21:00", "23:59", 0.26),
    ])
}

/// Cheap 02:00–03:30 (1.5 h).
fn short_window() -> TariffConfig {
    tariff(&[
        ("00:00", "02:00", 0.26),
        ("02:00", "03:30", 0.09),
        ("03:30", "23:59", 0.26),
    ])
}

/// Intelligent Octopus Go: 23:30–05:30, across midnight.
fn iog() -> TariffConfig {
    tariff(&[
        ("00:00", "05:30", 0.07),
        ("05:30", "23:30", 0.30),
        ("23:30", "23:59", 0.07),
    ])
}

/// Cosy: cheap 04:00–07:00, 13:00–16:00 and 22:00–00:00.
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

fn params(max_charge_kw: f64) -> SimulationParams {
    SimulationParams {
        capacity_kwh: 10.0,
        start_soc_pct: 50.0,
        reserve_soc_pct: 4.0,
        max_charge_kw,
        max_discharge_kw: 5.0,
        charge_efficiency: 0.95,
        discharge_efficiency: 0.95,
    }
}

fn day_ts(day: i64, hour: u32, minute: u32) -> i64 {
    let date = chrono::NaiveDate::from_ymd_opt(2024, 11, 15).unwrap() + chrono::Duration::days(day);
    chrono::Local
        .from_local_datetime(&date.and_hms_opt(hour, minute, 0).unwrap())
        .earliest()
        .unwrap()
        .timestamp()
}

/// 48 hours from 00:00 on day 0, the same solar and load every day.
fn series(
    start_soc: f64,
    solar: [f64; 24],
    load: [f64; 24],
    p: &SimulationParams,
) -> (SimulationOutput, Vec<SimHourInput>, SimulationParams) {
    let hours: Vec<SimHourInput> = (0..2)
        .flat_map(|d| {
            (0..24u32).map(move |h| SimHourInput {
                timestamp: day_ts(d, h, 0),
                solar_kwh: solar[h as usize],
                consumption_kwh: load[h as usize],
                ..Default::default()
            })
        })
        .collect();
    let p = SimulationParams {
        start_soc_pct: start_soc,
        ..*p
    };
    (simulate_battery(&hours, &p), hours, p)
}

fn inputs<'a>(
    sim: &'a SimulationOutput,
    hours: &'a [SimHourInput],
    p: &'a SimulationParams,
    tariff: &'a TariffConfig,
    min_soc: f64,
    now_ts: i64,
) -> PlanInputs<'a> {
    PlanInputs {
        simulation: sim,
        sim_hours: Some(hours),
        params: p,
        import_tariff: Some(tariff),
        target_soc_pct: min_soc,
        consumption_tomorrow_kwh: 12.0,
        consumption_sufficient: true,
        now_ts,
        current_soc_pct: p.start_soc_pct,
        running_window_start: None,
        every_cheap_window: false,
    }
}

/// A direct-bank inverter: `percent` of the maximum, in 1% steps.
fn direct(max_kw: f64) -> impl Fn(u16) -> Option<(u16, f64)> {
    move |percent| Some((percent, f64::from(percent) / 100.0 * max_kw))
}

/// No solar, a steady 0.2 kWh an hour: about an hour's charge at 5 kW.
fn winter_night() -> ([f64; 24], [f64; 24]) {
    ([0.0; 24], [0.2; 24])
}

fn spread(advice: Option<SpreadAdvice>) -> (u16, f64, u8, f64, u16, u16, u16, f64) {
    match advice {
        Some(SpreadAdvice::Spread {
            rate_percent,
            rate_kw,
            target_soc_pct,
            kwh,
            finish_min,
            full_rate_finish_min: _,
            full_rate_minutes,
            window_minutes,
            after_min_soc_pct,
            window: _,
        }) => (
            rate_percent,
            rate_kw,
            target_soc_pct,
            kwh,
            finish_min,
            full_rate_minutes,
            window_minutes,
            after_min_soc_pct,
        ),
        other => panic!("expected a spread, got {other:?}"),
    }
}

#[test]
fn the_thresholds_are_the_ones_the_help_text_quotes() {
    // The Forecast page's ⓘ and the changelog say 80% of the window, 65%
    // of it, 2 hours and max(0.5 kW, 20% of the maximum): change them too.
    // 90% of the window for a target of 90% or lower, 80% above it.
    assert_eq!(SPREAD_FINISH_FRACTION, 0.8);
    assert_eq!(SPREAD_FINISH_FRACTION_BELOW_FULL, 0.9);
    assert_eq!(SPREAD_NEAR_FULL_PCT, 90);
    assert_eq!(SPREAD_WORTHWHILE_FRACTION, 0.65);
    assert_eq!(SPREAD_MIN_WINDOW_MIN, 120);
    assert_eq!(SPREAD_FLOOR_KW, 0.5);
    assert_eq!(SPREAD_FLOOR_FRACTION, 0.2);
}

/// Issue #483 (peterb99, 23 Nov): a 13:00–16:00 charge to 80% finished
/// about 15:20, leaving 40 minutes unused. The 20% margin is for charging
/// slowing near full, which a target of 90% or lower never reaches, so those
/// aim for 90% of the window and only fuller charges keep 80%.
#[test]
fn only_a_charge_near_full_keeps_the_wider_margin() {
    assert_eq!(spread_finish_fraction(80), 0.9);
    assert_eq!(spread_finish_fraction(90), 0.9);
    assert_eq!(spread_finish_fraction(91), 0.8);
    assert_eq!(spread_finish_fraction(100), 0.8);
}

#[test]
fn a_charge_well_short_of_full_finishes_near_the_windows_end() {
    // The 22:00–00:00 window from below, to a target well under 90%: no
    // later than 90% of the way through (23:48), and after the latest an 80%
    // finish (23:36) would allow, so the lower rate is really used.
    let (solar, load) = winter_night();
    let p = params(5.0);
    let (sim, hours, p) = series(80.0, solar, load, &p);
    let t = cosy();
    let mut inp = inputs(&sim, &hours, &p, &t, 40.0, day_ts(0, 20, 0));
    inp.every_cheap_window = true;
    let plan = plan_overnight_charge(&inp);
    let (_, _, target, _, finish, _, window_min, _) =
        spread(spread_charge_advice(&inp, &plan, direct(5.0)));
    assert_eq!(window_min, 120);
    assert!(target <= SPREAD_NEAR_FULL_PCT, "target {target}");
    assert!(
        (23 * 60 + 37..=23 * 60 + 48).contains(&finish),
        "finish {finish}"
    );
}

#[test]
fn worthwhile_test_boundary_is_65_percent_of_the_window() {
    // 65% of a 180-minute window is 117 minutes.
    assert!(worth_spreading(116, 180));
    assert!(!worth_spreading(117, 180));
    assert!(!worth_spreading(180, 180));
    assert!(worth_spreading(0, 120));
}

#[test]
fn a_short_charge_in_a_long_window_is_spread_to_finish_near_its_end() {
    let (solar, load) = winter_night();
    let p = params(5.0);
    let (sim, hours, p) = series(80.0, solar, load, &p);
    let t = flux();
    let inp = inputs(&sim, &hours, &p, &t, 20.0, day_ts(0, 22, 0));
    let plan = plan_overnight_charge(&inp);
    assert!(
        matches!(plan, PlanRecommendation::Charge { .. }),
        "{plan:?}"
    );
    let (percent, rate_kw, target, kwh, finish, full_min, window_min, after) =
        spread(spread_charge_advice(&inp, &plan, direct(5.0)));
    assert_eq!(window_min, 180);
    assert!(full_min < 117, "full-rate minutes {full_min}");
    assert!((20..100).contains(&percent), "rate {percent}%");
    // Rounded up to the 1% step, and just enough to finish by the target's
    // finish point (90% of the window for a target of 90% or lower).
    let available_h = 3.0 * spread_finish_fraction(target);
    assert!((rate_kw - f64::from(percent) * 0.05).abs() < 1e-9);
    assert!(
        rate_kw * available_h + 1e-9 >= kwh,
        "{rate_kw} kW for {kwh} kWh"
    );
    assert!((f64::from(percent) - 1.0) * 0.05 * available_h < kwh || percent == 20);
    // Finishes inside the window, no later than that point.
    let latest = 2 * 60 + (180.0 * spread_finish_fraction(target)).round() as u16;
    assert!((2 * 60..=latest).contains(&finish), "finish {finish}");
    assert!((5..=100).contains(&target));
    assert!(after >= 20.0 - 0.05, "trough {after}");
    // Never less charge than the full-rate plan.
    let PlanRecommendation::Charge { kwh: plan_kwh, .. } = plan else {
        unreachable!()
    };
    assert!(kwh >= plan_kwh - 0.01, "{kwh} < {plan_kwh}");
}

#[test]
fn the_plan_is_never_changed_by_the_advice() {
    let (solar, load) = winter_night();
    let p = params(5.0);
    let (sim, hours, p) = series(80.0, solar, load, &p);
    let t = flux();
    let inp = inputs(&sim, &hours, &p, &t, 20.0, day_ts(0, 22, 0));
    let plan = plan_overnight_charge(&inp);
    let before = format!("{plan:?}");
    let _ = spread_charge_advice(&inp, &plan, direct(5.0));
    assert_eq!(format!("{plan:?}"), before);
    assert_eq!(format!("{:?}", plan_overnight_charge(&inp)), before);
}

#[test]
fn a_window_under_two_hours_keeps_full_power() {
    let (solar, load) = winter_night();
    let p = params(5.0);
    let (sim, hours, p) = series(80.0, solar, load, &p);
    let t = short_window();
    let inp = inputs(&sim, &hours, &p, &t, 20.0, day_ts(0, 22, 0));
    let plan = plan_overnight_charge(&inp);
    assert!(
        matches!(plan, PlanRecommendation::Charge { .. }),
        "{plan:?}"
    );
    assert_eq!(
        spread_charge_advice(&inp, &plan, direct(5.0)),
        Some(SpreadAdvice::FullRate(SpreadSkip::WindowTooShort {
            window_minutes: 90
        }))
    );
}

#[test]
fn a_charge_that_already_fills_most_of_the_window_keeps_full_power() {
    // A slow inverter and a big drain: full power needs most of the window.
    let p = params(2.0);
    let (sim, hours, p) = series(20.0, [0.0; 24], [0.8; 24], &p);
    let t = flux();
    let inp = inputs(&sim, &hours, &p, &t, 20.0, day_ts(0, 22, 0));
    let plan = plan_overnight_charge(&inp);
    let PlanRecommendation::Charge { window, .. } = &plan else {
        panic!("{plan:?}")
    };
    let full = super::super::window_duration_minutes(window);
    assert!(full >= 117, "fixture should fill the window: {full} min");
    assert_eq!(
        spread_charge_advice(&inp, &plan, direct(2.0)),
        Some(SpreadAdvice::FullRate(SpreadSkip::NotWorthwhile {
            full_rate_minutes: full,
            window_minutes: 180,
        }))
    );
}

#[test]
fn a_tiny_charge_is_clamped_up_to_the_rate_floor() {
    // Barely below the minimum: a few minutes at full power.
    let (solar, load) = ([0.0; 24], [0.17; 24]);
    let p = params(5.0);
    let (sim, hours, p) = series(100.0, solar, load, &p);
    let t = flux();
    let inp = inputs(&sim, &hours, &p, &t, 15.0, day_ts(0, 22, 0));
    let plan = plan_overnight_charge(&inp);
    let PlanRecommendation::Charge { kwh, .. } = &plan else {
        panic!("{plan:?}")
    };
    assert!(*kwh < 1.0, "fixture should need little: {kwh}");
    let (percent, rate_kw, ..) = spread(spread_charge_advice(&inp, &plan, direct(5.0)));
    // max(0.5 kW, 20% of 5 kW) = 1 kW.
    assert_eq!(percent, 20);
    assert!((rate_kw - 1.0).abs() < 1e-9);
    // On a small inverter the 0.5 kW floor is the larger one.
    let p2 = params(2.0);
    let (sim2, hours2, p2) = series(100.0, solar, load, &p2);
    let inp2 = inputs(&sim2, &hours2, &p2, &t, 15.0, day_ts(0, 22, 0));
    let plan2 = plan_overnight_charge(&inp2);
    let (percent2, rate2, ..) = spread(spread_charge_advice(&inp2, &plan2, direct(2.0)));
    assert_eq!(percent2, 25);
    assert!((rate2 - 0.5).abs() < 1e-9);
}

#[test]
fn the_rate_rounds_up_to_the_registers_own_step() {
    // A half-scale register on a 9.5 kWh pack: steps of 95 W, read back
    // through the percentage the Control endpoints take. 1.3 kW wanted.
    let max_kw = 5.0;
    let step = |raw: u16| f64::from(raw) * 0.095;
    let rate_at = move |percent: u16| {
        let raw = (f64::from(percent) * 5000.0 / 9500.0).round() as u16;
        Some((percent, step(raw).min(max_kw)))
    };
    let RegisterRate::Rate(percent, kw) = register_rate(1.3, max_kw, &rate_at) else {
        panic!("expected a rate");
    };
    assert!(kw >= 1.3, "{kw}");
    assert!((kw - step(14)).abs() < 1e-9, "{kw}");
    // The lowest percentage that reaches it.
    assert!(rate_at(percent - 1).unwrap().1 < 1.3);
}

#[test]
fn a_rate_at_the_maximum_or_needing_the_pack_size_keeps_full_power() {
    assert!(matches!(
        register_rate(5.0, 5.0, &direct(5.0)),
        RegisterRate::AtMaximum
    ));
    assert!(matches!(
        register_rate(4.99, 5.0, &direct(5.0)),
        RegisterRate::Rate(100, _) | RegisterRate::AtMaximum
    ));
    assert!(matches!(
        register_rate(1.0, 5.0, &|_| None),
        RegisterRate::NeedsPackSize
    ));
    // Through the planner too.
    let (solar, load) = winter_night();
    let p = params(5.0);
    let (sim, hours, p) = series(80.0, solar, load, &p);
    let t = flux();
    let inp = inputs(&sim, &hours, &p, &t, 20.0, day_ts(0, 22, 0));
    let plan = plan_overnight_charge(&inp);
    assert_eq!(
        spread_charge_advice(&inp, &plan, |_| None),
        Some(SpreadAdvice::FullRate(SpreadSkip::NeedsPackSize))
    );
}

#[test]
fn a_hold_plan_keeps_its_target_and_only_the_rate_changes() {
    let (solar, load) = winter_night();
    let p = params(5.0);
    let (sim, hours, p) = series(80.0, solar, load, &p);
    let t = flux();
    let inp = inputs(&sim, &hours, &p, &t, 20.0, day_ts(0, 22, 0));
    let plan = plan_hold_through_window(&inp);
    let PlanRecommendation::ChargeAndHold {
        target_soc_pct,
        kwh: plan_kwh,
        ..
    } = &plan
    else {
        panic!("{plan:?}")
    };
    let (percent, _, target, kwh, ..) = spread(spread_charge_advice(&inp, &plan, direct(5.0)));
    assert_eq!(target, *target_soc_pct);
    assert!(percent < 100);
    assert!(kwh >= plan_kwh - 0.01);
}

#[test]
fn nothing_to_spread_without_a_charge_or_in_a_running_window() {
    // A full battery and a light load: no charge needed.
    let p = params(5.0);
    let (sim, hours, p) = series(95.0, [0.0; 24], [0.1; 24], &p);
    let t = flux();
    let inp = inputs(&sim, &hours, &p, &t, 20.0, day_ts(0, 22, 0));
    let plan = plan_overnight_charge(&inp);
    assert!(
        matches!(plan, PlanRecommendation::NoChargeNeeded { .. }),
        "{plan:?}"
    );
    assert_eq!(spread_charge_advice(&inp, &plan, direct(5.0)), None);
    let no_plan = PlanRecommendation::NoPlan { reason: "x".into() };
    assert_eq!(spread_charge_advice(&inp, &no_plan, direct(5.0)), None);

    // A replan inside the window is a later step.
    let (solar, load) = winter_night();
    let (sim, hours, p) = series(80.0, solar, load, &params(5.0));
    let mut running = inputs(&sim, &hours, &p, &t, 20.0, day_ts(1, 2, 30));
    running.running_window_start = Some(day_ts(1, 2, 0));
    let plan = plan_overnight_charge(&running);
    assert_eq!(spread_charge_advice(&running, &plan, direct(5.0)), None);
}

#[test]
fn a_window_across_midnight_finishes_after_midnight() {
    let (solar, load) = winter_night();
    let p = params(5.0);
    let (sim, hours, p) = series(80.0, solar, load, &p);
    let t = iog();
    let inp = inputs(&sim, &hours, &p, &t, 20.0, day_ts(0, 21, 0));
    let plan = plan_overnight_charge(&inp);
    let PlanRecommendation::Charge { window, .. } = &plan else {
        panic!("{plan:?}")
    };
    let advice = spread_charge_advice(&inp, &plan, direct(5.0));
    let Some(SpreadAdvice::Spread {
        full_rate_finish_min,
        ..
    }) = advice
    else {
        panic!("{advice:?}")
    };
    // The plan's own slot from 23:30, wrapped past midnight.
    assert_eq!(full_rate_finish_min, window.end_min);
    assert!(full_rate_finish_min < 23 * 60 + 30);
    let (_, _, _, _, finish, _, window_min, _) = spread(advice);
    assert_eq!(window_min, 360);
    // 23:30 + no more than 80% of 6 h = 04:18 at the latest.
    assert!(finish < 4 * 60 + 18 + 1, "finish {finish}");
}

#[test]
fn a_daytime_window_whose_solar_the_lower_limit_would_cap_keeps_full_power() {
    // Cosy's 13:00–16:00 window with 0.6 kWh an hour of surplus sun. At full
    // power the battery reaches its target in about half an hour and the
    // sun then lifts it further. The lower limit caps sun and grid together,
    // so the slower charge spends that sun reaching the target instead and
    // the battery ends the window lower, too low for the evening.
    let solar: [f64; 24] = std::array::from_fn(|h| if (13..16).contains(&h) { 0.9 } else { 0.0 });
    let p = params(5.0);
    let (sim, hours, p) = series(30.0, solar, [0.3; 24], &p);
    let t = cosy();
    let mut inp = inputs(&sim, &hours, &p, &t, 20.0, day_ts(0, 11, 0));
    inp.every_cheap_window = true;
    let plan = plan_overnight_charge(&inp);
    let advice = spread_charge_advice(&inp, &plan, direct(5.0));
    let PlanRecommendation::Charge { window, .. } = &plan else {
        panic!("{plan:?}")
    };
    assert!(super::super::window_duration_minutes(window) < 117);
    assert_eq!(
        advice,
        Some(SpreadAdvice::FullRate(SpreadSkip::WorseTrough))
    );
}

#[test]
fn the_lower_limit_inside_the_window_caps_solar_charging_too() {
    // The simulation itself: the same hold, with the limit lowered inside
    // the window, takes less solar into the battery there.
    let solar: [f64; 24] = std::array::from_fn(|h| if (13..16).contains(&h) { 4.5 } else { 0.0 });
    let p = params(5.0);
    let (sim, hours, p) = series(15.0, solar, [0.4; 24], &p);
    let t = cosy();
    let mut inp = inputs(&sim, &hours, &p, &t, 20.0, day_ts(0, 11, 0));
    inp.every_cheap_window = true;
    let ctx = prepare_hold(&inp).expect("hold context");
    assert_eq!(ctx.window.start_min, 13 * 60);
    let full = ctx.simulate(&p, 100).unwrap();
    let slow = ctx.simulate_at_rate(&p, 100, 1.0).unwrap();
    let in_window = |o: &super::super::hold::HoldOutcome| {
        o.series
            .iter()
            .zip(&hours)
            .filter(|(_, h)| (day_ts(0, 13, 0)..day_ts(0, 16, 0)).contains(&h.timestamp))
            .map(|(r, _)| r.charge_kwh)
            .sum::<f64>()
    };
    assert!(in_window(&slow) < in_window(&full) - 1.0);
    assert!(in_window(&slow) <= 3.0 + 1e-6);
}

/// The half-scale register the DC hybrids use, converted exactly as the
/// server does: `max_w` watts of inverter on a `pack_kwh` pack.
fn half_scale(max_w: u32, pack_kwh: f32) -> impl Fn(u16) -> Option<(u16, f64)> {
    use crate::inverter::{model::PowerLimitBank, power_limit::PowerLimitScale};
    let scale = PowerLimitScale::new(PowerLimitBank::HalfScale, max_w, pack_kwh);
    move |percent| {
        scale.can_convert(percent).then(|| {
            let raw = scale.percent_to_raw(percent);
            (scale.raw_to_percent(raw), scale.raw_to_watts(raw) / 1000.0)
        })
    }
}

#[test]
fn a_cheap_window_ending_at_midnight_counts_as_two_hours() {
    // Cosy's 22:00–00:00 is stored as 22:00–23:59: a tariff can't say 24:00.
    let (solar, load) = winter_night();
    let p = params(5.0);
    let (sim, hours, p) = series(80.0, solar, load, &p);
    let t = cosy();
    let mut inp = inputs(&sim, &hours, &p, &t, 40.0, day_ts(0, 20, 0));
    inp.every_cheap_window = true;
    let plan = plan_overnight_charge(&inp);
    let PlanRecommendation::Charge { window, .. } = &plan else {
        panic!("{plan:?}")
    };
    assert_eq!(window.start_min, 22 * 60);
    let (.., finish, _, window_min, _) = spread(spread_charge_advice(&inp, &plan, direct(5.0)));
    assert_eq!(window_min, 120);
    // No later than 90% of the way through (the target is under 90%): 23:48.
    assert!(
        (22 * 60..=23 * 60 + 48).contains(&finish),
        "finish {finish}"
    );
}

#[test]
fn the_registers_top_value_is_full_power_even_below_the_inverters_rating() {
    // A 3.6 kW inverter on a 5.2 kWh pack: register 50 (0.5C) allows only
    // 2.6 kW, and it is also what 100% writes, so a "99%" that rounds to it
    // limits nothing.
    let rate_at = half_scale(3600, 5.2);
    assert!((rate_at(100).unwrap().1 - 2.6).abs() < 1e-6);
    assert!(matches!(
        register_rate(2.58, 3.6, &rate_at),
        RegisterRate::AtMaximum
    ));
    // Just under the top stays a real limit.
    let RegisterRate::Rate(_, kw) = register_rate(2.5, 3.6, &rate_at) else {
        panic!("expected a rate below the top");
    };
    assert!(kw < 2.6 - 1e-6, "{kw}");
}

#[test]
fn needing_the_full_rate_is_not_reported_as_filling_the_window() {
    // Full power takes 103 of 180 minutes, under the 65% test, but finishing
    // by 90% of the window needs about 2.29 kW, more than the register's top
    // value allows on a 4.4 kWh pack (0.5C, 2.2 kW).
    let p = params(3.6);
    let (sim, hours, p) = series(30.0, [0.0; 24], [0.2; 24], &p);
    let t = flux();
    let inp = inputs(&sim, &hours, &p, &t, 20.0, day_ts(0, 22, 0));
    let plan = plan_overnight_charge(&inp);
    let PlanRecommendation::Charge { window, .. } = &plan else {
        panic!("{plan:?}")
    };
    assert!(worth_spreading(
        super::super::window_duration_minutes(window),
        180
    ));
    let advice = spread_charge_advice(&inp, &plan, half_scale(3600, 4.4));
    assert!(
        !matches!(
            advice,
            Some(SpreadAdvice::Spread { .. })
                | Some(SpreadAdvice::FullRate(SpreadSkip::NotWorthwhile { .. }))
        ),
        "{advice:?}"
    );
    assert_eq!(
        advice,
        Some(SpreadAdvice::FullRate(SpreadSkip::NeedsFullRate))
    );
}

#[test]
fn solar_in_a_daytime_window_counts_toward_the_rate_and_the_finish() {
    // Cosy's 13:00–16:00 window with 2 kWh an hour of surplus sun and a
    // busy evening: the one limit caps sun and grid together, so the rate
    // has to carry the sun's share as well, and the finish comes from the
    // simulated SOC rather than the grid's kWh.
    let solar: [f64; 24] = std::array::from_fn(|h| if (13..16).contains(&h) { 2.3 } else { 0.0 });
    let load: [f64; 24] = std::array::from_fn(|h| if (16..22).contains(&h) { 1.2 } else { 0.3 });
    let p = params(5.0);
    let (sim, hours, p) = series(50.0, solar, load, &p);
    let t = cosy();
    let mut inp = inputs(&sim, &hours, &p, &t, 20.0, day_ts(0, 11, 0));
    inp.every_cheap_window = true;
    let plan = plan_overnight_charge(&inp);
    assert!(
        matches!(plan, PlanRecommendation::Charge { .. }),
        "{plan:?}"
    );
    let (_, rate_kw, target, _, finish, _, window_min, after) =
        spread(spread_charge_advice(&inp, &plan, direct(5.0)));
    assert_eq!(window_min, 180);
    assert!(after >= 20.0 - 0.05, "trough {after}");
    assert!(
        (13 * 60..=13 * 60 + 144).contains(&finish),
        "finish {finish}"
    );
    // The simulated battery reaches the target in the finish's hour, not
    // in the hour before.
    let ctx = prepare_hold(&inp).unwrap();
    let run = ctx.simulate_at_rate(&p, target, rate_kw).unwrap();
    let hour = usize::from((finish - 1) / 60);
    let target = f64::from(target);
    assert!(
        run.series[hour].soc_pct >= target - 0.05,
        "{:?}",
        run.series[hour]
    );
    assert!(run.series[hour - 1].soc_pct < target - 0.05);
}
#[test]
fn a_spread_never_ends_lower_than_full_power_even_above_the_minimum() {
    // A hold to 70% in Cosy's 13:00–16:00 window with 0.6 kWh an hour of
    // surplus sun. At full power the battery reaches 70% early and the sun
    // lifts it further; slowed, the sun goes into reaching 70% instead.
    // Both stay above the 20% minimum, but the spread would leave the
    // battery lower all evening, so it isn't the same plan at a gentler
    // rate.
    let solar: [f64; 24] = std::array::from_fn(|h| if (13..16).contains(&h) { 0.9 } else { 0.0 });
    let p = params(5.0);
    let (sim, hours, p) = series(30.0, solar, [0.3; 24], &p);
    let t = cosy();
    let mut inp = inputs(&sim, &hours, &p, &t, 20.0, day_ts(0, 11, 0));
    inp.every_cheap_window = true;
    let ctx = prepare_hold(&inp).unwrap();
    let full = ctx.simulate(&p, 70).unwrap();
    assert!(full.trough_pct() > 30.0, "fixture: {}", full.trough_pct());
    let plan = PlanRecommendation::ChargeAndHold {
        window: ctx.window.clone(),
        target_soc_pct: 70,
        strategy: crate::settings::PlanStrategy::HoldWindow,
        kwh: full.charge_kwh,
        min_soc_pct: 20.0,
        observed_min_soc_pct: 4.0,
        after_min_soc_pct: full.trough_pct(),
        current_soc_pct: 30.0,
        rationale: String::new(),
        with_charge_series: Vec::new(),
        import_tomorrow_with_charge_kwh: 0.0,
        export_tomorrow_with_charge_kwh: 0.0,
    };
    let advice = spread_charge_advice(&inp, &plan, direct(5.0));
    if let Some(SpreadAdvice::Spread {
        after_min_soc_pct, ..
    }) = advice
    {
        assert!(
            after_min_soc_pct >= full.trough_pct() - 0.05,
            "spread trough {after_min_soc_pct} below full power's {}",
            full.trough_pct()
        );
    }
}

#[test]
fn the_advised_percent_is_the_one_the_control_page_shows_for_that_register() {
    // 9.5 kWh pack, 3.6 kW inverter: about 0.38 register units per percent,
    // so several percents write the same register value. The advice names
    // the one the Control page reads that value back as.
    use crate::inverter::{model::PowerLimitBank, power_limit::PowerLimitScale};
    let scale = PowerLimitScale::new(PowerLimitBank::HalfScale, 3600, 9.5);
    let rate_at = half_scale(3600, 9.5);
    for wanted in [0.7, 0.8, 0.85, 0.9, 1.0, 1.2, 1.5, 2.0] {
        let RegisterRate::Rate(percent, _) = register_rate(wanted, 3.6, &rate_at) else {
            panic!("expected a rate for {wanted}");
        };
        let raw = scale.percent_to_raw(percent);
        assert_eq!(scale.raw_to_percent(raw), percent, "{wanted} kW");
    }
}

#[test]
fn full_power_on_a_minimum_soc_plan_finishes_when_the_plans_own_slot_ends() {
    // The card's Time is the slot Apply writes (the kWh at full power); the
    // spread's "instead of" names the same end, not a hold to the rounded-up
    // target with the sun on top.
    let solar: [f64; 24] = std::array::from_fn(|h| if (13..16).contains(&h) { 2.3 } else { 0.0 });
    let load: [f64; 24] = std::array::from_fn(|h| if (16..22).contains(&h) { 1.2 } else { 0.3 });
    let p = params(5.0);
    let (sim, hours, p) = series(50.0, solar, load, &p);
    let t = cosy();
    let mut inp = inputs(&sim, &hours, &p, &t, 20.0, day_ts(0, 11, 0));
    inp.every_cheap_window = true;
    let plan = plan_overnight_charge(&inp);
    let PlanRecommendation::Charge { window, .. } = &plan else {
        panic!("{plan:?}")
    };
    let advice = spread_charge_advice(&inp, &plan, direct(5.0));
    let Some(SpreadAdvice::Spread {
        full_rate_finish_min,
        ..
    }) = advice
    else {
        panic!("{advice:?}")
    };
    assert_eq!(full_rate_finish_min, window.end_min);
}
