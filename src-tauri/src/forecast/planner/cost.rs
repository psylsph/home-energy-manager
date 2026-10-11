//! Cheapest-overall charge strategy (issue #359).
//!
//! The hold strategy charges to the lowest target that keeps the user's
//! minimum. This one picks the target that costs least over the whole cycle
//! to the next cheap window. Charging further at the cheap rate pays when
//! it replaces dearer grid import later, and stops paying once the day is
//! covered, or once the extra charge would crowd out solar that would
//! otherwise be stored or sold.
//!
//! On a single forecast the minimum plan already avoids every unit of
//! import, so the gain comes from uncertainty: each target is priced
//! against the median forecast and the low solar estimate, half and half.
//! Charging a little further costs next to nothing when the sun shows up,
//! because what is left in the battery is credited at what refilling it
//! would cost, and saves peak-rate import when it doesn't.
//!
//! Each candidate is simulated as a hold slot and priced hour by hour from
//! the tariffs. The minimum is still a floor, judged on the median forecast
//! like every other strategy: no candidate is below the lowest target that
//! keeps it. A pure Eco night, with no slot at all, is a candidate too.

use super::hold::{
    build_hold_plan, hold_recommendation, lowest_holding_target, prepare_hold, HoldContext,
    HoldOutcome,
};
use super::{hhmm, plan_overnight_charge, PlanInputs, PlanRecommendation};
use crate::forecast::simulate::{SimHourInput, SimulationParams};
use crate::settings::{PlanStrategy, TariffConfig};
use chrono::Timelike;

/// Targets whose cost is within this of the cheapest are treated as equal
/// and the lowest wins, so a pound of noise never means cycling the
/// battery further for nothing. The plan is the cheapest to within this.
const COST_TIE_GBP: f64 = 0.01;

/// How much of each target's cost comes from the low solar estimate rather
/// than the median forecast. Half: a dull day is as likely as the forecast
/// says, no likelier, and the other half keeps charging from being bought
/// against a worst case that rarely arrives.
const LOW_SOLAR_WEIGHT: f64 = 0.5;

/// What a pure hold, charging nothing, must save over leaving the battery
/// to Eco before it is worth writing a slot for, £.
const MIN_HOLD_SAVING_GBP: f64 = 0.05;

/// What the cheapest strategy needs beyond the standard planner inputs.
#[derive(Debug, Clone, Copy, Default)]
pub struct CostInputs<'a> {
    /// Prices what the plan exports, if there is an export tariff.
    pub export_tariff: Option<&'a TariffConfig>,
    /// The same hourly series as the planner's with the low solar
    /// estimate, one entry per hour. Each target is priced against both
    /// forecasts, so charging a little further is rewarded when the sun
    /// might not show up. `None`, or a series of another length, prices
    /// the median forecast alone.
    pub low_solar_hours: Option<&'a [SimHourInput]>,
}

/// The local minute of day an hour starting at `timestamp` begins at.
fn minute_of_day(timestamp: i64) -> Option<u16> {
    let local = chrono::DateTime::from_timestamp(timestamp, 0)?.with_timezone(&chrono::Local);
    Some(local.hour() as u16 * 60 + local.minute() as u16)
}

/// The mean £/kWh over the hour starting at `timestamp`: its rate at the
/// start and at the half hour, since tariffs change on half hours.
fn hourly_rate(tariff: &TariffConfig, timestamp: i64) -> f64 {
    let Some(minute) = minute_of_day(timestamp) else {
        return 0.0;
    };
    let at = |m: u16| tariff.rate_for_minutes(m % 1440).unwrap_or(0.0);
    (at(minute) + at(minute + 30)) / 2.0
}

/// The £/kWh for import outside the cheap window in the hour starting at
/// `timestamp`: the rate of whichever half hour isn't the window's, so a
/// window opening or closing on the half hour doesn't drag the rest of
/// that hour towards the cheap rate.
fn rate_outside_window(tariff: &TariffConfig, timestamp: i64, window_rate: f64) -> f64 {
    let Some(minute) = minute_of_day(timestamp) else {
        return window_rate;
    };
    let at = |m: u16| tariff.rate_for_minutes(m % 1440).unwrap_or(window_rate);
    let (first, second) = (at(minute), at(minute + 30));
    let is_window = |rate: f64| (rate - window_rate).abs() < 1e-9;
    match (is_window(first), is_window(second)) {
        (true, false) => second,
        (false, true) => first,
        _ => (first + second) / 2.0,
    }
}

/// The net cost of the cycle from the window's start to the next cheap
/// window, £: grid import, less export income, less the value of what is
/// left in the battery. The grid's in-window supply is priced at the
/// window's rate and any other import at the rate outside it.
pub(super) fn cycle_cost(
    ctx: &HoldContext,
    outcome: &HoldOutcome,
    params: &SimulationParams,
    import_tariff: &TariffConfig,
    export_tariff: Option<&TariffConfig>,
) -> f64 {
    let end = ctx
        .next_occurrence_start
        .unwrap_or(outcome.series.len())
        .min(outcome.series.len());
    let start = ctx.first_index.min(end);
    let window_rate = ctx.window.rate;
    let import: f64 = (start..end)
        .map(|i| {
            let hour = &outcome.series[i];
            let in_window = outcome.window_grid.get(i).copied().unwrap_or(0.0);
            in_window * window_rate
                + (hour.import_kwh - in_window).max(0.0)
                    * rate_outside_window(import_tariff, hour.timestamp, window_rate)
        })
        .sum();
    let export: f64 = export_tariff.map_or(0.0, |tariff| {
        outcome.series[start..end]
            .iter()
            .map(|h| h.export_kwh * hourly_rate(tariff, h.timestamp))
            .sum()
    });
    let eta_c = params.charge_efficiency.clamp(0.01, 1.0);
    let eta_d = params.discharge_efficiency.clamp(0.01, 1.0);
    let left_over = outcome.series[start..end].last().map_or(0.0, |h| {
        (h.soc_pct - params.reserve_soc_pct).max(0.0) / 100.0 * params.capacity_kwh * eta_d
    });
    import - export - left_over * window_rate / (eta_c * eta_d)
}

/// Compute the cheapest-overall recommendation. `NoPlan` from the default
/// planner passes through, a minimum of 0% means no grid charge as in every
/// strategy, and the default plan stands when there are no hourly inputs.
pub fn plan_cheapest_overall(inputs: &PlanInputs, extras: &CostInputs) -> PlanRecommendation {
    let base = plan_overnight_charge(inputs);
    let observed_min_soc_pct = match &base {
        PlanRecommendation::Charge {
            observed_min_soc_pct,
            ..
        }
        | PlanRecommendation::NoChargeNeeded {
            observed_min_soc_pct,
            ..
        } => *observed_min_soc_pct,
        PlanRecommendation::ChargeAndHold { .. } | PlanRecommendation::NoPlan { .. } => {
            return base
        }
    };
    let floor = inputs.target_soc_pct;
    let (Some(import_tariff), true) = (inputs.import_tariff, floor > 0.0) else {
        return base;
    };
    let Some(ctx) = prepare_hold(inputs) else {
        return base;
    };
    let params = inputs.params;
    let sim_hours = ctx.sim_hours;

    let (minimum_target, reaches_minimum, minimum_outcome) =
        lowest_holding_target(&ctx, params, floor);
    if !reaches_minimum {
        // Even a full battery can't hold the minimum: nothing to optimise,
        // and the plan says so.
        return build_hold_plan(
            inputs,
            ctx,
            observed_min_soc_pct,
            (minimum_target, reaches_minimum, minimum_outcome),
            PlanStrategy::Cheapest,
        );
    }

    // The low solar estimate counts only when it covers the same hours.
    let low_hours = extras
        .low_solar_hours
        .filter(|hours| hours.len() == sim_hours.len());
    // A cost blends the median forecast with the low estimate, or is the
    // median alone when there is none. `simulate` yields the outcome on the
    // given hours; `None` when either forecast can't be simulated, so every
    // priced candidate is on the same scale.
    let blended = |simulate: &dyn Fn(&[SimHourInput]) -> Option<HoldOutcome>| -> Option<f64> {
        let price = |hours: &[SimHourInput]| {
            simulate(hours)
                .map(|o| cycle_cost(&ctx, &o, params, import_tariff, extras.export_tariff))
        };
        let median = price(sim_hours)?;
        Some(match low_hours {
            Some(low) => (1.0 - LOW_SOLAR_WEIGHT) * median + LOW_SOLAR_WEIGHT * price(low)?,
            None => median,
        })
    };

    let costs: Vec<(u8, f64)> = (minimum_target..=100)
        .filter_map(|target| {
            // Candidates must keep the minimum on the median forecast.
            if !ctx
                .simulate(params, target)?
                .holds(sim_hours, params, floor)
            {
                return None;
            }
            let cost = blended(&|hours| ctx.simulate_on(hours, params, target))?;
            Some((target, cost))
        })
        .collect();
    let Some(cheapest) = costs.iter().map(|(_, cost)| *cost).reduce(f64::min) else {
        return base;
    };
    let minimum_cost = costs.first().map_or(cheapest, |(_, cost)| *cost);
    let Some(&(target, cost)) = costs
        .iter()
        .find(|(_, cost)| *cost <= cheapest + COST_TIE_GBP)
    else {
        return base;
    };
    let Some(outcome) = ctx.simulate(params, target) else {
        return base;
    };

    // Doing nothing is a candidate too: plain Eco, if it keeps the minimum.
    // A pure hold has to beat it by more than a token amount to be worth a
    // slot; a charging plan only has to beat it at all.
    // A target is the level rounded up to a whole percent, so a hold may
    // top up a fraction of a point; under a full point it is a pure hold.
    let one_point_ac = params.capacity_kwh / 100.0 / params.charge_efficiency.clamp(0.01, 1.0);
    let pure_hold = outcome.charge_kwh < one_point_ac;
    let margin = if pure_hold {
        MIN_HOLD_SAVING_GBP
    } else {
        COST_TIE_GBP
    };
    let eco_wins = ctx
        .simulate_eco(sim_hours, params)
        .filter(|eco| eco.holds(sim_hours, params, floor))
        .and_then(|_| blended(&|hours| ctx.simulate_eco(hours, params)))
        .is_some_and(|eco_cost| eco_cost <= cost + margin);
    if eco_wins {
        return PlanRecommendation::NoChargeNeeded {
            current_soc_pct: inputs.current_soc_pct,
            min_soc_pct: floor,
            observed_min_soc_pct,
        };
    }

    let window = format!(
        "the {:.1}p window ({}–{})",
        ctx.window.rate * 100.0,
        hhmm(ctx.window.start_min),
        hhmm(ctx.window.end_min),
    );
    let action = if pure_hold {
        format!("Holding the battery at {target}%")
    } else {
        format!("Charging to {target}%")
    };
    let comparison = if target > minimum_target {
        format!(
            "on average about £{:.2} less than charging only to {minimum_target}%, the least \
             that keeps your {floor:.0}% minimum",
            (minimum_cost - cost).max(0.0),
        )
    } else {
        "on average, as charging any further would cost more than it saves".to_string()
    };
    let weighing = if low_hours.is_some() {
        " It weighs the forecast against the low solar estimate, so a dull day costs less."
    } else {
        ""
    };
    let rationale = format!(
        "Battery is at {:.0}% now. {action} in {window}, then holding it there while the grid \
         supplies the house until {}, is the cheapest way to get to the next cheap period: \
         {comparison}.{weighing}",
        inputs.current_soc_pct,
        hhmm(ctx.window.end_min),
    );
    hold_recommendation(
        inputs,
        ctx,
        target,
        &outcome,
        observed_min_soc_pct,
        rationale,
        PlanStrategy::Cheapest,
    )
}
