//! The back-stop floor for exports (issue #407).
//!
//! An export sells battery energy ahead of a cheap import window. The floor
//! is the state of charge the battery must still hold when the export ends
//! so the house is covered, on a pessimistic day, until that window starts:
//! the caller passes hours built from the p75 consumption and the low solar
//! band. It always wins: nothing the median forecast says may lower it.
//! Pure; nothing here touches the inverter.

use super::simulate::{SimHourInput, SimulationParams};

/// Points kept on top of what the pessimistic day needs.
pub const BACKSTOP_MARGIN_PCT: f64 = 5.0;

/// The wider margin while the consumption profile is still thin
/// (`insufficient_consumption_history`).
pub const THIN_HISTORY_MARGIN_PCT: f64 = 10.0;

/// The SOC (%) the battery must hold at `from_ts` to supply the house until
/// `until_ts`, plus `margin_pct`, never below `planner_min_pct` or the
/// modelled reserve in `params`, never above 100.
pub fn export_floor_soc(
    hours: &[SimHourInput],
    params: &SimulationParams,
    from_ts: i64,
    until_ts: i64,
    planner_min_pct: f64,
    margin_pct: f64,
) -> f64 {
    let cap = params.capacity_kwh;
    let eta_c = params.charge_efficiency;
    let eta_d = params.discharge_efficiency;
    let usable = [
        cap,
        eta_c,
        eta_d,
        params.reserve_soc_pct,
        planner_min_pct,
        margin_pct,
    ]
    .iter()
    .all(|v| v.is_finite())
        && cap > 0.0
        && eta_c > 0.0
        && eta_d > 0.0;
    if !usable {
        return 100.0;
    }
    let reserve = params.reserve_soc_pct;
    if reserve >= 100.0 {
        return 100.0;
    }
    // Walk back from the window's start: what the battery must hold before
    // each hour so it holds `need` after it. A shortfall adds to the need,
    // sun the battery can store takes from it, and it never drops below the
    // reserve, where the battery stops.
    let mut need = reserve;
    for hour in hours.iter().rev() {
        let start = hour.timestamp.max(from_ts);
        let end = (hour.timestamp + HOUR_SECS).min(until_ts);
        if end <= start {
            continue;
        }
        let fraction = (end - start) as f64 / HOUR_SECS as f64;
        let values = [
            hour.solar_kwh,
            hour.consumption_kwh,
            hour.ev_kwh,
            hour.held_fraction,
        ];
        if values.iter().any(|v| !v.is_finite()) {
            return 100.0;
        }
        // The battery supplies the shortfall except while a dispatch holds it.
        let deficit_ac = hour.net_load_kwh() * hour.discharge_share() * fraction;
        let surplus_ac = hour.surplus_kwh() * fraction;
        let change = deficit_ac / eta_d / cap * 100.0 - surplus_ac * eta_c / cap * 100.0;
        need = (need + change).clamp(reserve, 100.0);
    }
    (need + margin_pct).clamp(planner_min_pct.max(reserve), 100.0)
}

const HOUR_SECS: i64 = 3600;

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3600;
    const T0: i64 = 1_700_000_000 - 1_700_000_000 % H;

    fn params() -> SimulationParams {
        SimulationParams {
            capacity_kwh: 10.0,
            start_soc_pct: 80.0,
            reserve_soc_pct: 5.0,
            max_charge_kw: 3.0,
            max_discharge_kw: 3.0,
            charge_efficiency: 1.0,
            discharge_efficiency: 1.0,
        }
    }

    fn hour(i: i64, solar: f64, load: f64) -> SimHourInput {
        SimHourInput {
            timestamp: T0 + i * H,
            solar_kwh: solar,
            consumption_kwh: load,
            ..Default::default()
        }
    }

    fn floor(hours: &[SimHourInput], from: i64, until: i64, min: f64, margin: f64) -> f64 {
        export_floor_soc(hours, &params(), T0 + from * H, T0 + until * H, min, margin)
    }

    #[test]
    fn covers_the_house_to_the_window_then_adds_the_margin() {
        // 3 h at 1 kWh on a 10 kWh battery is 30 points above the 5 point
        // reserve, plus the 5 point margin.
        let hours: Vec<_> = (0..3).map(|i| hour(i, 0.0, 1.0)).collect();
        assert!((floor(&hours, 0, 3, 0.0, 5.0) - 40.0).abs() < 1e-9);
    }

    #[test]
    fn solar_between_now_and_the_window_lowers_the_need() {
        // The middle hour's 1 kWh surplus refills 10 points of the 30 needed.
        let hours = vec![hour(0, 0.0, 1.0), hour(1, 2.0, 1.0), hour(2, 0.0, 1.0)];
        assert!((floor(&hours, 0, 3, 0.0, 0.0) - 15.0).abs() < 1e-9);
    }

    #[test]
    fn solar_after_the_shortfall_does_not_help_it() {
        // The battery has to get through the dark hours before the sun comes.
        let hours = vec![hour(0, 0.0, 1.0), hour(1, 0.0, 1.0), hour(2, 2.0, 1.0)];
        assert!((floor(&hours, 0, 3, 0.0, 0.0) - 25.0).abs() < 1e-9);
    }

    #[test]
    fn a_surplus_hour_cannot_lower_the_need_below_the_reserve() {
        let hours = vec![hour(0, 5.0, 0.0), hour(1, 0.0, 0.2)];
        assert!((floor(&hours, 0, 2, 0.0, 0.0) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn never_below_the_planner_minimum() {
        let hours = vec![hour(0, 0.0, 0.1)];
        assert!((floor(&hours, 0, 1, 30.0, 5.0) - 30.0).abs() < 1e-9);
    }

    #[test]
    fn never_below_the_reserve_whatever_the_margin() {
        assert!((floor(&[], 0, 1, 0.0, 0.0) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn never_above_100() {
        let hours: Vec<_> = (0..12).map(|i| hour(i, 0.0, 3.0)).collect();
        assert_eq!(floor(&hours, 0, 12, 0.0, 5.0), 100.0);
    }

    #[test]
    fn counts_only_the_part_of_an_hour_inside_the_span() {
        // Half of one 2 kWh hour is 1 kWh: 10 points over the reserve.
        let hours = vec![hour(0, 0.0, 2.0)];
        let f = export_floor_soc(&hours, &params(), T0 + H / 2, T0 + H, 0.0, 0.0);
        assert!((f - 15.0).abs() < 1e-9);
    }

    #[test]
    fn hours_outside_the_span_are_ignored() {
        let hours = vec![hour(0, 0.0, 5.0), hour(1, 0.0, 1.0), hour(2, 0.0, 5.0)];
        assert!((floor(&hours, 1, 2, 0.0, 0.0) - 15.0).abs() < 1e-9);
    }

    #[test]
    fn a_dispatch_hold_leaves_its_share_to_the_grid() {
        let mut held = hour(0, 0.0, 2.0);
        held.held_fraction = 0.5;
        assert!((floor(&[held], 0, 1, 0.0, 0.0) - 15.0).abs() < 1e-9);
    }

    #[test]
    fn the_car_counts_as_load() {
        let mut with_car = hour(0, 0.0, 1.0);
        with_car.ev_kwh = 1.0;
        assert!((floor(&[with_car], 0, 1, 0.0, 0.0) - 25.0).abs() < 1e-9);
    }

    #[test]
    fn discharge_efficiency_raises_the_need() {
        let mut p = params();
        p.discharge_efficiency = 0.5;
        let f = export_floor_soc(&[hour(0, 0.0, 1.0)], &p, T0, T0 + H, 0.0, 0.0);
        assert!((f - 25.0).abs() < 1e-9);
    }

    #[test]
    fn a_span_that_is_empty_or_backwards_needs_only_the_margin_floor() {
        let hours = vec![hour(0, 0.0, 5.0)];
        assert!((floor(&hours, 1, 1, 0.0, 5.0) - 10.0).abs() < 1e-9);
        assert!((floor(&hours, 2, 1, 0.0, 5.0) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn a_reserve_of_100_means_no_export_at_all() {
        let mut p = params();
        p.reserve_soc_pct = 100.0;
        assert_eq!(export_floor_soc(&[], &p, T0, T0 + H, 0.0, 5.0), 100.0);
    }

    #[test]
    fn garbage_inputs_fail_safe_to_100() {
        let mut p = params();
        p.capacity_kwh = 0.0;
        assert_eq!(
            export_floor_soc(&[hour(0, 0.0, 1.0)], &p, T0, T0 + H, 0.0, 5.0),
            100.0
        );
        let bad = SimHourInput {
            consumption_kwh: f64::NAN,
            ..hour(0, 0.0, 1.0)
        };
        assert_eq!(floor(&[bad], 0, 1, 0.0, 5.0), 100.0);
        assert_eq!(floor(&[], 0, 1, f64::NAN, 5.0), 100.0);
    }
}
