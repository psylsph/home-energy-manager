//! Hold-through-window charge strategy (issue #359).
//!
//! The default planner charges at full rate for the shortest slot that
//! holds the minimum, then hands back to Eco, so the battery discharges
//! for the rest of the cheap window and pays round-trip losses on energy
//! the grid could have supplied at the cheap rate. This strategy writes a
//! slot spanning the whole cheap window with a target SOC instead: the
//! inverter charges to the target, then holds it while the grid supplies
//! the house until the window ends (confirmed on a Gen3 in issue #359).
//! The target is the lowest whole percent that still holds the minimum
//! until the next cheap window.
//!
//! Whether to charge at all stays the default planner's decision —
//! [`plan_overnight_charge`] runs first, unchanged, and only its `Charge`
//! outcome is reshaped here.

use super::{
    charge_duration_minutes, charge_window_for_duration, cheapest_import_window,
    energy_on_local_day, first_reachable_occurrence, floor_held, hhmm, plan_overnight_charge,
    post_range, window_overlap_segments, ChargeWindow, PlanInputs, PlanRecommendation,
};
use crate::forecast::simulate::{
    simulate_battery_segment, SimHourInput, SimHourResult, SimulationParams,
};
use crate::forecast::time_windows::split_hour_segments;
use crate::settings::PlanStrategy;
use chrono::Timelike;

/// Lowest target the slot is ever given. HR 116 decodes 4 as "unset" and
/// the Control endpoints only treat 5–99 as an unambiguous target.
const MIN_TARGET_PCT: u8 = 5;

/// The forward simulation with the selected window occurrence run as a
/// hold slot.
#[derive(Debug, Clone)]
pub(super) struct HoldOutcome {
    /// Hourly results. `import_kwh` includes the grid draw inside the
    /// window, which the simulation otherwise models as free surplus.
    pub(super) series: Vec<SimHourResult>,
    /// AC kWh drawn into the battery during the window.
    pub(super) charge_kwh: f64,
    /// AC kWh the grid supplies during the window: the charge plus the
    /// house load the held battery no longer covers.
    pub(super) grid_kwh: f64,
    /// The part of each hour's `import_kwh` that is that in-window supply,
    /// by hour, so it can be priced at the window's rate even when the
    /// window opens or closes part-way through an hour.
    pub(super) window_grid: Vec<f64>,
    /// The hours the minimum is judged across (see [`post_range`]).
    post: (usize, usize),
    /// Hour index where the next cheap-window occurrence begins.
    next_occurrence_start: Option<usize>,
}

impl HoldOutcome {
    /// Whether the trajectory holds `floor` (see [`floor_held`]).
    pub(super) fn holds(
        &self,
        sim_hours: &[SimHourInput],
        params: &SimulationParams,
        floor: f64,
    ) -> bool {
        !self.series.is_empty()
            && floor_held(
                &self.series,
                sim_hours,
                params,
                self.post.0,
                self.post.1,
                floor,
            )
    }

    /// Lowest SOC across the judged hours, %.
    fn trough_pct(&self) -> f64 {
        self.series[self.post.0..self.post.1.min(self.series.len())]
            .iter()
            .map(|h| h.soc_pct)
            .fold(f64::INFINITY, f64::min)
    }
}

/// Simulate the first window occurrence starting at or after `now_ts` as a
/// hold slot with `target_pct`: charge at the inverter maximum until the
/// target, then hold, with the grid covering whatever load solar doesn't.
/// Solar surplus inside the window still charges the battery as in Eco.
/// The same physics as a configured charge slot in
/// [`crate::forecast::current_schedule`], applied to one occurrence only
/// like the default planner. `None` when the window has no occurrence or
/// the parameters can't be simulated.
#[cfg(test)]
pub(super) fn simulate_hold(
    sim_hours: &[SimHourInput],
    params: &SimulationParams,
    window: &ChargeWindow,
    now_ts: i64,
    target_pct: u8,
) -> Option<HoldOutcome> {
    let (selected, next_occurrence_start) = first_reachable_occurrence(sim_hours, window, now_ts);
    let run = selected?.run;
    let last = *run.last()?;
    simulate_hold_run(
        sim_hours,
        params,
        window,
        &run,
        last,
        next_occurrence_start,
        target_pct,
    )
}

/// Simulate the hold for an occurrence already found: `run` is the hours
/// the slot is active (empty for plain Eco), `last_of_run` the occurrence's
/// last hour, from which the minimum is judged, and `next_occurrence_start`
/// where the following one begins.
fn simulate_hold_run(
    sim_hours: &[SimHourInput],
    params: &SimulationParams,
    window: &ChargeWindow,
    run: &[usize],
    last_of_run: usize,
    next_occurrence_start: Option<usize>,
    target_pct: u8,
) -> Option<HoldOutcome> {
    let eta_c = params.charge_efficiency.clamp(0.01, 1.0);
    let target = f64::from(target_pct.min(100));
    let mut soc = params.start_soc_pct;
    let mut series = Vec::with_capacity(sim_hours.len());
    let mut window_grid = Vec::with_capacity(sim_hours.len());
    let mut charge_kwh = 0.0;
    let mut grid_kwh = 0.0;

    for (index, hour) in sim_hours.iter().copied().enumerate() {
        let mut hour_drawn = 0.0;
        let active = if run.contains(&index) {
            window_overlap_segments(hour.timestamp, window)
        } else {
            Vec::new()
        };
        let mut combined = SimHourResult {
            timestamp: hour.timestamp,
            soc_pct: soc,
            import_kwh: 0.0,
            export_kwh: 0.0,
            charge_kwh: 0.0,
            discharge_kwh: 0.0,
        };
        for (start, end, inside) in split_hour_segments(&active) {
            let fraction = end - start;
            let mut segment_hour = hour.segment(fraction);
            let segment_params = SimulationParams {
                start_soc_pct: soc,
                ..*params
            };
            let mut drawn = 0.0;
            if inside {
                // Solar surplus charges first, as in Eco; the grid tops up only
                // what is left of the room to the target and of the rate.
                let surplus = (hour.solar_kwh - hour.consumption_kwh).max(0.0) * fraction;
                let room_ac = (target - soc).max(0.0) / 100.0 * params.capacity_kwh / eta_c;
                let charge_ac = (params.max_charge_kw * fraction - surplus)
                    .min(room_ac - surplus)
                    .max(0.0);
                let unmet_load = (hour.consumption_kwh - hour.solar_kwh).max(0.0) * fraction;
                segment_hour.solar_kwh += charge_ac + unmet_load;
                charge_kwh += charge_ac;
                drawn = charge_ac + unmet_load;
            }
            let res = simulate_battery_segment(segment_hour, &segment_params, fraction)?;
            soc = res.soc_pct;
            grid_kwh += drawn;
            hour_drawn += drawn;
            combined.soc_pct = res.soc_pct;
            combined.import_kwh += res.import_kwh + drawn;
            combined.export_kwh += res.export_kwh;
            combined.charge_kwh += res.charge_kwh;
            combined.discharge_kwh += res.discharge_kwh;
        }
        series.push(combined);
        window_grid.push(hour_drawn);
    }

    let post = post_range(
        last_of_run,
        next_occurrence_start.unwrap_or(sim_hours.len()),
    );
    Some(HoldOutcome {
        series,
        charge_kwh,
        grid_kwh,
        window_grid,
        post,
        next_occurrence_start,
    })
}

/// The stretch of a hold window spent charging: `kwh` at the inverter's
/// maximum from the window's start, in whole minutes and never longer
/// than the window. `None` for a pure hold that charges nothing.
pub fn hold_charging_window(
    window: &ChargeWindow,
    kwh: f64,
    max_charge_kw: f64,
) -> Option<ChargeWindow> {
    let minutes = charge_duration_minutes(kwh, max_charge_kw);
    (minutes > 0).then(|| charge_window_for_duration(window, minutes))
}

/// Everything both hold-based strategies need to evaluate a target: the
/// cheap window, the occurrence being planned, and the lowest target worth
/// considering.
pub(super) struct HoldContext<'a> {
    pub(super) sim_hours: &'a [SimHourInput],
    pub(super) window: ChargeWindow,
    run: Vec<usize>,
    last: usize,
    pub(super) first_index: usize,
    pub(super) next_occurrence_start: Option<usize>,
    /// The battery's level when the window opens, rounded up: never target
    /// below it. Whether an inverter discharges down to a lower target
    /// during a charge slot is unconfirmed, while charging up to a target
    /// and holding it is what issue #359 confirmed. With the lowest target
    /// the battery only holds from the window's start, so the end of its
    /// first hour reads that level, even mid-hour; solar inside that hour
    /// can only lift it, keeping the floor on the safe side.
    pub(super) lowest: u8,
    /// The hold at a 100% target, the most the window can do.
    pub(super) full: HoldOutcome,
}

impl HoldContext<'_> {
    /// Simulate the occurrence as a hold slot with `target`.
    pub(super) fn simulate(&self, params: &SimulationParams, target: u8) -> Option<HoldOutcome> {
        self.simulate_on(self.sim_hours, params, target)
    }

    /// [`Self::simulate`] on another forecast for the same hours, such as
    /// the low solar estimate.
    pub(super) fn simulate_on(
        &self,
        hours: &[SimHourInput],
        params: &SimulationParams,
        target: u8,
    ) -> Option<HoldOutcome> {
        simulate_hold_run(
            hours,
            params,
            &self.window,
            &self.run,
            self.last,
            self.next_occurrence_start,
            target,
        )
    }

    /// The same hours with no slot at all: plain Eco, the battery running
    /// the house through the window. The baseline a hold is worth beating.
    pub(super) fn simulate_eco(
        &self,
        hours: &[SimHourInput],
        params: &SimulationParams,
    ) -> Option<HoldOutcome> {
        simulate_hold_run(
            hours,
            params,
            &self.window,
            &[],
            self.last,
            self.next_occurrence_start,
            100,
        )
    }
}

/// Find the window to plan and the bounds to search, or `None` when the
/// inputs can't support a hold (no hourly inputs, no tariff window, no
/// occurrence ahead).
pub(super) fn prepare_hold<'a>(inputs: &PlanInputs<'a>) -> Option<HoldContext<'a>> {
    let sim_hours = inputs
        .sim_hours
        .filter(|hours| hours.len() == inputs.simulation.hours.len())?;
    let tariff = inputs.import_tariff?;
    let now_min = chrono::DateTime::from_timestamp(inputs.now_ts, 0)
        .map(|dt| {
            let local = dt.with_timezone(&chrono::Local);
            local.hour() as u16 * 60 + local.minute() as u16
        })
        .unwrap_or(0);
    let window = cheapest_import_window(tariff, now_min, 30)?;
    let (selected, next_occurrence_start) =
        first_reachable_occurrence(sim_hours, &window, inputs.now_ts);
    let occurrence = selected?;
    let first_index = occurrence.first_index();
    let run = occurrence.run;
    let last = *run.last()?;
    let params = inputs.params;
    let simulate = |target: u8| {
        simulate_hold_run(
            sim_hours,
            params,
            &window,
            &run,
            last,
            next_occurrence_start,
            target,
        )
    };
    let full = simulate(100)?;
    let soc_at_start = simulate(MIN_TARGET_PCT)?
        .series
        .get(first_index)
        .map(|h| h.soc_pct)?;
    let lowest = soc_at_start.ceil().clamp(f64::from(MIN_TARGET_PCT), 100.0) as u8;
    Some(HoldContext {
        sim_hours,
        window,
        run,
        last,
        first_index,
        next_occurrence_start,
        lowest,
        full,
    })
}

/// Build the recommendation for a hold at `target` with `outcome`, using
/// the default planner's overlay and Tomorrow-tile conventions: the line
/// stops at the next cheap period; the tiles read tomorrow's local calendar
/// day across the whole horizon.
pub(super) fn hold_recommendation(
    inputs: &PlanInputs,
    ctx: HoldContext,
    target: u8,
    outcome: &HoldOutcome,
    observed_min_soc_pct: f64,
    rationale: String,
    strategy: PlanStrategy,
) -> PlanRecommendation {
    let with_charge_end = outcome
        .next_occurrence_start
        .map(|i| (i + 1).min(outcome.series.len()))
        .unwrap_or(outcome.series.len());
    let with_charge_series = outcome
        .series
        .iter()
        .take(with_charge_end)
        .map(|h| (h.timestamp, h.soc_pct))
        .collect();
    let tomorrow = chrono::DateTime::from_timestamp(inputs.now_ts, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).date_naive() + chrono::Duration::days(1));
    let (import_tomorrow_with_charge_kwh, export_tomorrow_with_charge_kwh) = match tomorrow {
        Some(date) => (
            energy_on_local_day(&outcome.series, date, &chrono::Local, |h| h.import_kwh),
            energy_on_local_day(&outcome.series, date, &chrono::Local, |h| h.export_kwh),
        ),
        None => (0.0, 0.0),
    };
    PlanRecommendation::ChargeAndHold {
        window: ctx.window,
        target_soc_pct: target,
        strategy,
        kwh: outcome.charge_kwh,
        min_soc_pct: inputs.target_soc_pct,
        observed_min_soc_pct,
        after_min_soc_pct: outcome.trough_pct(),
        current_soc_pct: inputs.current_soc_pct,
        rationale,
        with_charge_series,
        import_tomorrow_with_charge_kwh,
        export_tomorrow_with_charge_kwh,
    }
}

/// The lowest target, from the battery's level when the window opens up to
/// 100, whose hold keeps `floor` until the next cheap period; whether any
/// does; and that hold's simulation. When even 100% can't, `(100, false,
/// the 100% hold)`.
pub(super) fn lowest_holding_target(
    ctx: &HoldContext,
    params: &SimulationParams,
    floor: f64,
) -> (u8, bool, HoldOutcome) {
    let sim_hours = ctx.sim_hours;
    // Search for the lowest holding target, keeping the winning simulation
    // so it isn't run again.
    let holding = |target: u8| {
        ctx.simulate(params, target)
            .filter(|o| o.holds(sim_hours, params, floor))
    };
    if !ctx.full.holds(sim_hours, params, floor) {
        return (100, false, ctx.full.clone());
    }
    if let Some(at_lowest) = holding(ctx.lowest) {
        return (ctx.lowest, true, at_lowest);
    }
    // `lowest` fails and 100 holds; holding is monotonic in the target.
    let (mut lo, mut hi, mut best) = (ctx.lowest, 100u8, ctx.full.clone());
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        match holding(mid) {
            Some(at_mid) => {
                hi = mid;
                best = at_mid;
            }
            None => lo = mid,
        }
    }
    (hi, true, best)
}

/// Compute the hold-through-window recommendation. `NoPlan` and
/// `NoChargeNeeded` from the default planner pass through unchanged, as
/// does its plan when there are no hourly inputs to simulate a hold with.
pub fn plan_hold_through_window(inputs: &PlanInputs) -> PlanRecommendation {
    let base = plan_overnight_charge(inputs);
    let PlanRecommendation::Charge {
        observed_min_soc_pct,
        ..
    } = base
    else {
        return base;
    };
    let Some(ctx) = prepare_hold(inputs) else {
        return base;
    };
    let (target, reaches_minimum, outcome) =
        lowest_holding_target(&ctx, inputs.params, inputs.target_soc_pct);
    build_hold_plan(
        inputs,
        ctx,
        observed_min_soc_pct,
        (target, reaches_minimum, outcome),
        PlanStrategy::HoldWindow,
    )
}

/// The hold recommendation for a target search result `(target, whether it
/// keeps the minimum, its simulation)`, with the explanation in the default
/// planner's voice.
pub(super) fn build_hold_plan(
    inputs: &PlanInputs,
    ctx: HoldContext,
    observed_min_soc_pct: f64,
    (target, reaches_minimum, outcome): (u8, bool, HoldOutcome),
    strategy: PlanStrategy,
) -> PlanRecommendation {
    let params = inputs.params;
    let rationale = hold_rationale(&HoldRationale {
        current_soc_pct: inputs.current_soc_pct,
        observed_min_soc_pct,
        after_min_soc_pct: outcome.trough_pct(),
        floor: inputs.target_soc_pct,
        reserve: params.reserve_soc_pct,
        target,
        reaches_minimum,
        window: &ctx.window,
        charge_cost: outcome.charge_kwh * ctx.window.rate,
        house_cost: (outcome.grid_kwh - outcome.charge_kwh).max(0.0) * ctx.window.rate,
    });
    hold_recommendation(
        inputs,
        ctx,
        target,
        &outcome,
        observed_min_soc_pct,
        rationale,
        strategy,
    )
}

struct HoldRationale<'a> {
    current_soc_pct: f64,
    observed_min_soc_pct: f64,
    after_min_soc_pct: f64,
    floor: f64,
    reserve: f64,
    target: u8,
    reaches_minimum: bool,
    window: &'a ChargeWindow,
    /// £ of the charge drawn into the battery.
    charge_cost: f64,
    /// £ of the house's grid use while the battery is held.
    house_cost: f64,
}

/// The explanation under the plan, in the default planner's voice.
fn hold_rationale(r: &HoldRationale) -> String {
    let window = format!(
        "the {:.1}p window ({}–{})",
        r.window.rate * 100.0,
        hhmm(r.window.start_min),
        hhmm(r.window.end_min),
    );
    let hold = format!(
        "then holding it there while the grid supplies the house until {}",
        hhmm(r.window.end_min)
    );
    let costs = format!(
        "(about £{:.2} to charge, plus £{:.2} for the house's use during the window)",
        r.charge_cost, r.house_cost
    );
    if r.floor <= r.reserve {
        let outcome = if r.reaches_minimum {
            "keeps it off the reserve until then"
        } else {
            "still leaves it running empty before the next cheap period; the forecast drain \
             is more than this window can cover"
        };
        format!(
            "Battery is at {:.0}% now and the forecast has it running empty at its {:.0}% \
             reserve, drawing from the grid, before the next cheap period. Charging to {}% \
             in {}, {}, {} {}.",
            r.current_soc_pct, r.reserve, r.target, window, hold, outcome, costs,
        )
    } else if r.reaches_minimum {
        format!(
            "Battery is at {:.0}% now and the forecast trough drops to {:.0}% before the next \
             cheap period. Charging to {}% in {}, {}, lifts the trough to {:.0}% — at or above \
             your {:.0}% minimum {}.",
            r.current_soc_pct,
            r.observed_min_soc_pct,
            r.target,
            window,
            hold,
            r.after_min_soc_pct,
            r.floor,
            costs,
        )
    } else {
        format!(
            "Battery is at {:.0}% now and the forecast trough drops to {:.0}% before the next \
             cheap period. Even charging to {}% in {}, {}, lifts the trough to only {:.0}% — \
             still below your {:.0}% minimum; the forecast drain is more than this window can \
             cover {}.",
            r.current_soc_pct,
            r.observed_min_soc_pct,
            r.target,
            window,
            hold,
            r.after_min_soc_pct,
            r.floor,
            costs,
        )
    }
}
