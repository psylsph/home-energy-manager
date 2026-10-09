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
    cheapest_import_window, energy_on_local_day, first_reachable_occurrence, floor_held, hhmm,
    plan_overnight_charge, post_range, window_overlap_segments, ChargeWindow, PlanInputs,
    PlanRecommendation,
};
use crate::forecast::simulate::{
    simulate_battery_segment, SimHourInput, SimHourResult, SimulationParams,
};
use crate::forecast::time_windows::split_hour_segments;
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
pub(super) fn simulate_hold(
    sim_hours: &[SimHourInput],
    params: &SimulationParams,
    window: &ChargeWindow,
    now_ts: i64,
    target_pct: u8,
) -> Option<HoldOutcome> {
    let (selected, next_occurrence_start) = first_reachable_occurrence(sim_hours, window, now_ts);
    let run = selected?.run;
    let eta_c = params.charge_efficiency.clamp(0.01, 1.0);
    let target = f64::from(target_pct.min(100));
    let mut soc = params.start_soc_pct;
    let mut series = Vec::with_capacity(sim_hours.len());
    let mut charge_kwh = 0.0;
    let mut grid_kwh = 0.0;

    for (index, hour) in sim_hours.iter().copied().enumerate() {
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
            let mut segment_hour = SimHourInput {
                timestamp: hour.timestamp,
                solar_kwh: hour.solar_kwh * fraction,
                consumption_kwh: hour.consumption_kwh * fraction,
            };
            let segment_params = SimulationParams {
                start_soc_pct: soc,
                ..*params
            };
            let mut drawn = 0.0;
            if inside {
                let room_ac = (target - soc).max(0.0) / 100.0 * params.capacity_kwh / eta_c;
                let charge_ac = (params.max_charge_kw * fraction).min(room_ac);
                let unmet_load = (hour.consumption_kwh - hour.solar_kwh).max(0.0) * fraction;
                segment_hour.solar_kwh += charge_ac + unmet_load;
                charge_kwh += charge_ac;
                drawn = charge_ac + unmet_load;
            }
            let res = simulate_battery_segment(segment_hour, &segment_params, fraction)?;
            soc = res.soc_pct;
            grid_kwh += drawn;
            combined.soc_pct = res.soc_pct;
            combined.import_kwh += res.import_kwh + drawn;
            combined.export_kwh += res.export_kwh;
            combined.charge_kwh += res.charge_kwh;
            combined.discharge_kwh += res.discharge_kwh;
        }
        series.push(combined);
    }

    let last = *run.last()?;
    let post = post_range(last, next_occurrence_start.unwrap_or(sim_hours.len()));
    Some(HoldOutcome {
        series,
        charge_kwh,
        grid_kwh,
        post,
        next_occurrence_start,
    })
}

/// The part of a hold window spent charging.
pub fn hold_charging_window(
    _window: &ChargeWindow,
    _kwh: f64,
    _max_charge_kw: f64,
) -> Option<ChargeWindow> {
    None
}

/// Compute the hold-through-window recommendation. `NoPlan` and
/// `NoChargeNeeded` from the default planner pass through unchanged, as
/// does its plan when there are no hourly inputs to simulate a hold with.
pub fn plan_hold_through_window(inputs: &PlanInputs) -> PlanRecommendation {
    let base = plan_overnight_charge(inputs);
    let PlanRecommendation::Charge {
        min_soc_pct,
        observed_min_soc_pct,
        current_soc_pct,
        ..
    } = base
    else {
        return base;
    };
    let Some(sim_hours) = inputs
        .sim_hours
        .filter(|hours| hours.len() == inputs.simulation.hours.len())
    else {
        return base;
    };
    let Some(tariff) = inputs.import_tariff else {
        return base;
    };
    let now_min = chrono::DateTime::from_timestamp(inputs.now_ts, 0)
        .map(|dt| {
            let local = dt.with_timezone(&chrono::Local);
            local.hour() as u16 * 60 + local.minute() as u16
        })
        .unwrap_or(0);
    let Some(window) = cheapest_import_window(tariff, now_min, 30) else {
        return base;
    };
    let params = inputs.params;
    let simulate = |target: u8| simulate_hold(sim_hours, params, &window, inputs.now_ts, target);
    let Some(full) = simulate(100) else {
        return base;
    };

    // Never target below the battery's level when the window opens:
    // whether an inverter discharges down to a lower target during a
    // charge slot is unconfirmed, while charging up to a target and
    // holding it is what issue #359 confirmed.
    let first = first_reachable_occurrence(sim_hours, &window, inputs.now_ts)
        .0
        .map(|occurrence| occurrence.first_index())
        .unwrap_or(0);
    let soc_at_start = first
        .checked_sub(1)
        .and_then(|i| full.series.get(i))
        .map(|h| h.soc_pct)
        .unwrap_or(params.start_soc_pct);
    let lowest = soc_at_start.ceil().clamp(f64::from(MIN_TARGET_PCT), 100.0) as u8;

    let floor = inputs.target_soc_pct;
    let holds = |target: u8| simulate(target).is_some_and(|o| o.holds(sim_hours, params, floor));
    let (target, reaches_minimum) = if !full.holds(sim_hours, params, floor) {
        (100, false)
    } else if holds(lowest) {
        (lowest, true)
    } else {
        // `lowest` fails and 100 holds; holding is monotonic in the target.
        let (mut lo, mut hi) = (lowest, 100u8);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if holds(mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        (hi, true)
    };
    let Some(outcome) = simulate(target) else {
        return base;
    };
    let after_min_soc_pct = outcome.trough_pct();

    // Same overlay and Tomorrow-tile conventions as the default planner:
    // the line stops at the next cheap period; the tiles read tomorrow's
    // local calendar day across the whole horizon.
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

    let rationale = hold_rationale(&HoldRationale {
        current_soc_pct,
        observed_min_soc_pct,
        after_min_soc_pct,
        floor,
        reserve: params.reserve_soc_pct,
        target,
        reaches_minimum,
        window: &window,
        charge_cost: outcome.charge_kwh * window.rate,
        house_cost: (outcome.grid_kwh - outcome.charge_kwh).max(0.0) * window.rate,
    });

    PlanRecommendation::ChargeAndHold {
        window,
        target_soc_pct: target,
        kwh: outcome.charge_kwh,
        min_soc_pct,
        observed_min_soc_pct,
        after_min_soc_pct,
        current_soc_pct,
        rationale,
        with_charge_series,
        import_tomorrow_with_charge_kwh,
        export_tomorrow_with_charge_kwh,
    }
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
