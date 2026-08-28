//! Tariff-aware charge planner (issue #283, Phase 2).
//!
//! Turns the Phase 1 predictions into a recommendation the user can
//! apply with one tap: whether the battery needs an overnight grid
//! charge, how much, and in which cheapest tariff window. Pure and
//! synchronous — the API layer feeds it predictions + tariff config,
//! and Apply happens only from the UI through the existing control
//! endpoints. Nothing autonomous.

use crate::forecast::simulate::{SimHourInput, SimHourResult, SimulationOutput, SimulationParams};
use crate::settings::TariffConfig;
use chrono::Timelike;

/// One candidate charging window derived from the import tariff slots.
///
/// `start_min` and `end_min` are wall-clock minutes (0..=1439) of the
/// tariff slot that the planner picked. `tomorrow = true` means the
/// occurrence falls on the calendar day after the planner's `now` —
/// i.e. the post-midnight half of the flow window cycle. Both flags
/// exist so a UI can render "Tomorrow 02:00–05:00" or "02:00–05:00"
/// unambiguously and so Apply can map them to the correct calendar
/// slot in the inverter.
#[derive(Debug, Clone, PartialEq)]
pub struct ChargeWindow {
    pub start_min: u16,
    pub end_min: u16,
    pub tomorrow: bool,
    pub rate: f64,
}

/// The planner's recommendation.
///
/// The objective is **minimum SOC across the forward window**, not
/// end-of-day target SOC. A battery might end at 60% but dip to 5%
/// overnight before solar arrives — the end-SOC view would say
/// "fine", the user would say "charge anyway". The planner finds the
/// *lowest SOC hour* in the simulation and asks for enough charge to
/// lift the trough above the user-configured `min_soc_pct`.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanRecommendation {
    /// No overnight charge needed — the projected trough across the
    /// forward window is already at or above the minimum.
    NoChargeNeeded {
        /// Current SOC at the moment of the plan request, %.
        current_soc_pct: f64,
        /// The user's configured minimum-allowable SOC, %.
        min_soc_pct: f64,
        /// The lowest SOC observed in the simulation, %.
        observed_min_soc_pct: f64,
    },
    /// Charge `kwh` AC in the window; the trough then lands at
    /// `after_min_soc_pct` (at or above `min_soc_pct`).
    Charge {
        window: ChargeWindow,
        /// AC kWh to draw from the grid, per night.
        kwh: f64,
        /// The user's configured minimum-allowable SOC, %.
        min_soc_pct: f64,
        /// The lowest SOC observed in the uncharged simulation, %.
        observed_min_soc_pct: f64,
        /// The lowest SOC after the charge is applied (measured across
        /// the hours the charge can influence — from the end of the
        /// charge window onwards), %.
        after_min_soc_pct: f64,
        /// The SOC the battery reaches by the end of the charge window,
        /// % — the level the inverter's charge-slot target must be set
        /// to for the plan to hold when applied.
        charge_target_soc_pct: f64,
        /// Current SOC at the moment of the plan request, %.
        current_soc_pct: f64,
        /// Human-readable rationale shown under the recommendation.
        rationale: String,
        /// Per-hour SOC trajectory when the recommended charge is
        /// applied to every window occurrence in the forward horizon.
        /// The Battery tab chart overlays it as a dashed line on top
        /// of the recorded SOC history so the user can see what
        /// enacting the recommendation actually does. Empty when the
        /// planner couldn't size an injection (no window run).
        with_charge_series: Vec<(i64, f64)>,
        /// Tomorrow's grid import under the recommended plan, kWh — the
        /// window's grid draw (once per occurrence starting tomorrow;
        /// the what-if sim models the charge as free surplus so its own
        /// import for window hours is zero and the draw must be added
        /// back) plus the residual import of tomorrow's what-if hours.
        /// Lets the Tomorrow tiles agree with the plan instead of the
        /// uncharged simulation.
        import_tomorrow_with_charge_kwh: f64,
        /// Tomorrow's grid export under the recommended plan, kWh
        /// (tomorrow's what-if hours, summed).
        export_tomorrow_with_charge_kwh: f64,
    },
    /// Inputs insufficient for a plan (no tariff config, no
    /// consumption history, etc.) — the UI shows a reason, not zeros.
    NoPlan { reason: String },
}

/// Inputs the planner needs beyond the Phase 1 predictions.
#[derive(Debug, Clone)]
pub struct PlanInputs<'a> {
    /// Phase 1 battery simulation across the forward window (today's
    /// remaining hours + tomorrow). Empty = no projection available.
    pub simulation: &'a SimulationOutput,
    /// The original hourly inputs the forecast used to produce
    /// `simulation`. Required by the planner to re-run the simulation
    /// with the proposed charge applied — the v2 fix for the analytic
    /// "uniform lift" bug — see `plan_overnight_charge`. `None` keeps
    /// the legacy analytic-lift behaviour (suitable for callers that
    /// don't have the per-hour solar/consumption vectors handy, e.g.
    /// older tests).
    pub sim_hours: Option<&'a [SimHourInput]>,
    /// Simulation params (capacity, efficiencies, rates) for the
    /// what-if recomputation.
    pub params: &'a SimulationParams,
    /// Import tariff; None or flat = no cheap window to exploit.
    pub import_tariff: Option<&'a TariffConfig>,
    /// The user's configured minimum-allowable SOC, %. The planner
    /// sizes the charge to keep every hour of the simulation at or
    /// above this threshold.
    pub target_soc_pct: f64,
    /// Predicted consumption for tomorrow, kWh (for the rationale text).
    pub consumption_tomorrow_kwh: f64,
    /// Whether the consumption history was sufficient (Phase 1 gate).
    pub consumption_sufficient: bool,
    /// Current SOC at the moment of the request, % (sourced from the
    /// live snapshot so the user can see the planner is reading fresh
    /// data — a stuck value here would mean the page isn't refetching).
    pub current_soc_pct: f64,
    /// `now` minute-of-day, for picking windows that haven't passed.
    pub now_min: u16,
}

/// Parse the tariff's cheapest contiguous window that starts at or
/// after `now_min` and is at least `min_duration_min` long. Ties break
/// to the earlier window. Pure helper exposed for tests.
pub fn cheapest_import_window(
    tariff: &TariffConfig,
    now_min: u16,
    min_duration_min: u16,
) -> Option<ChargeWindow> {
    let parsed = tariff.parsed_slots();
    // Occurrences of each slot in the forward 24 h cycle. A 02:00–05:00
    // off-peak recurs each night, so `now_min` only determines WHICH 24 h
    // slice is closest — never whether the slot is reachable tomorrow.
    let mut candidates: Vec<ChargeWindow> = Vec::new();
    for (start, end, rate) in &parsed {
        let (Some(base_start), Some(base_end)) = (*start, *end) else {
            continue;
        };
        // Shift the slot into the [now_min, now_min+1440) window. If the
        // slot started before `now_min`, its next occurrence is +1440.
        let offset_start: u16 = if base_start < now_min { 1440 } else { 0 };
        let slot_start = base_start + offset_start;
        let slot_end = base_end + offset_start;
        if slot_end <= slot_start {
            continue;
        }
        if slot_end - slot_start < min_duration_min {
            continue;
        }
        candidates.push(ChargeWindow {
            start_min: base_start,
            end_min: base_end,
            tomorrow: offset_start != 0,
            rate: *rate,
        });
    }
    // Cheapest first; ties to the earlier window.
    candidates.sort_by(|a, b| {
        a.rate
            .partial_cmp(&b.rate)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then((a.tomorrow as u8).cmp(&(b.tomorrow as u8)))
    });
    candidates.into_iter().next()
}

fn hhmm(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// Outcome of re-running the forward simulation with a proposed charge
/// applied. Used by the planner to size the recommendation against the
/// *real* post-charge trajectory instead of an analytic estimate.
#[derive(Debug, Clone)]
struct ChargeOutcome {
    /// Lowest SOC across the hours the charge can influence — from the
    /// end of the (first) charge window onwards, %.
    trough_pct: f64,
    /// SOC at the end of the first charge-window occurrence, % — the
    /// level the inverter's slot target must be set to.
    charge_level_pct: f64,
    /// Full hourly SOC series produced by the what-if simulation. The
    /// Battery tab chart uses this to overlay the "if we follow the
    /// recommendation" trajectory on top of the recorded SOC history;
    /// the Forecast page's old mini-chart used it for the same
    /// purpose inline.
    series: Vec<SimHourResult>,
}
/// Indices (into `sim_hours`) of every contiguous run of hours whose
/// local minute-of-day falls inside the window. The forecast series
/// only contains hours at or after `now`, so every run is a reachable
/// occurrence of the tariff slot — and because an applied charge slot
/// recurs nightly, the planner charges in *all* of them, not just the
/// first.
fn window_runs(sim_hours: &[SimHourInput], window: &ChargeWindow) -> Vec<Vec<usize>> {
    let mut runs: Vec<Vec<usize>> = Vec::new();
    for (i, h) in sim_hours.iter().enumerate() {
        let Some(dt) = chrono::DateTime::from_timestamp(h.timestamp, 0) else {
            continue;
        };
        let local = dt.with_timezone(&chrono::Local);
        let m = local.hour() as u16 * 60 + local.minute() as u16;
        let hit = m >= window.start_min && m < window.end_min;
        if !hit {
            continue;
        }
        match runs.last_mut() {
            // Consecutive hit — extend the current run.
            Some(run) if run.last() == Some(&(i - 1)) => run.push(i),
            // First hit of a new occurrence — start a fresh run.
            _ => runs.push(vec![i]),
        }
    }
    runs
}

/// Re-run the forward simulation with `kwh_ac` of grid charging applied
/// (per night) across the window's hours, and report the resulting
/// post-window trough plus the SOC the battery reaches by the end of
/// the first window.
///
/// The grid charge is injected by lifting each window hour's supply to
/// `per_hour_ac` above whatever the home's load was already consuming:
/// `solar += per_hour_ac + max(0, consumption - solar)`. The load offset
/// matters — the eco-mode simulator only charges from *surplus*, so
/// without it a charge scheduled during an overnight-deficit hour would
/// never reach the battery at all. With the offset, the hour's net
/// becomes exactly `per_hour_ac` above its uncharged net, which is the
/// physics of "grid powers the home and charges the battery
/// simultaneously".
fn simulate_with_charge(
    sim_hours: &[SimHourInput],
    params: &SimulationParams,
    runs: &[Vec<usize>],
    kwh_ac: f64,
) -> ChargeOutcome {
    let Some(first_run) = runs.first() else {
        return ChargeOutcome {
            trough_pct: f64::NAN,
            charge_level_pct: f64::NAN,
            series: Vec::new(),
        };
    };
    let mut hours: Vec<SimHourInput> = sim_hours.to_vec();
    for run in runs {
        let per_hour_ac = kwh_ac / run.len() as f64;
        for &i in run {
            if let Some(h) = hours.get_mut(i) {
                let unmet_load = (h.consumption_kwh - h.solar_kwh).max(0.0);
                h.solar_kwh += per_hour_ac + unmet_load;
            }
        }
    }
    let sim = crate::forecast::simulate::simulate_battery(&hours, params);
    let last_of_first = *first_run.last().expect("non-empty run");
    let charge_level_pct = sim
        .hours
        .get(last_of_first)
        .map(|h| h.soc_pct)
        .unwrap_or(0.0);
    let post = &sim.hours[(last_of_first + 1).min(sim.hours.len())..];
    let trough_pct = if post.is_empty() {
        charge_level_pct
    } else {
        post.iter().map(|h| h.soc_pct).fold(f64::INFINITY, f64::min)
    };
    ChargeOutcome {
        trough_pct,
        charge_level_pct,
        series: sim.hours,
    }
}

/// Compute the recommendation. See [`PlanRecommendation`] for the cases.
pub fn plan_overnight_charge(inputs: &PlanInputs) -> PlanRecommendation {
    // Gate: no projection to reason about.
    if inputs.simulation.hours.is_empty() {
        return PlanRecommendation::NoPlan {
            reason: "no battery projection available — connect to the \
                     inverter and wait for forecast data"
                .to_string(),
        };
    }
    // Gate: the consumption prediction underpins the deficit estimate.
    if !inputs.consumption_sufficient {
        return PlanRecommendation::NoPlan {
            reason: "not enough consumption history to plan yet — about a \
                     week of data is needed"
                .to_string(),
        };
    }
    // Gate: without an import tariff there is no cheap window to aim at.
    let Some(tariff) = inputs.import_tariff else {
        return PlanRecommendation::NoPlan {
            reason: "no import tariff configured — set your day/night rates \
                     in Settings so the planner can find your cheapest window"
                .to_string(),
        };
    };

    // Where the SOC bottoms out across the forward window. The charge
    // asks for enough to lift this *trough* above the user's
    // `min_soc_pct`, not just the end-of-window — a battery that ends
    // fine but dips overnight still needs a charge.
    let observed_min_soc_pct = inputs
        .simulation
        .hours
        .iter()
        .map(|h| h.soc_pct)
        .fold(f64::INFINITY, f64::min);
    if observed_min_soc_pct >= inputs.target_soc_pct {
        return PlanRecommendation::NoChargeNeeded {
            current_soc_pct: inputs.current_soc_pct,
            min_soc_pct: inputs.target_soc_pct,
            observed_min_soc_pct,
        };
    }

    // A 30-minute floor: shorter windows can't meaningfully charge.
    let Some(window) = cheapest_import_window(tariff, inputs.now_min, 30) else {
        return PlanRecommendation::NoPlan {
            reason: "no usable import window remaining today".to_string(),
        };
    };

    // Clamp the AC ask by what the window + rate can physically deliver.
    let capacity = inputs.params.capacity_kwh;
    let window_hours = (window.end_min - window.start_min) as f64 / 60.0;
    let deliverable_ac = inputs.params.max_charge_kw * window_hours;
    let eta = inputs.params.charge_efficiency.clamp(0.01, 1.0);

    // Size the charge against the trough the charge can actually
    // influence: hours from the end of the charge window onwards.
    // Hours between now and the window are unfixable by this plan (the
    // battery is already there) — the full-window `observed_min_soc_pct`
    // still triggers the recommendation, but the kWh ask protects the
    // post-window hours. Without this split the iteration below can
    // never converge: no amount of future charging lifts a past hour.
    let sim_hours = inputs.sim_hours.unwrap_or(&[]);
    let runs = window_runs(sim_hours, &window);
    let (fixable_trough, soc_at_window_end) = match (
        runs.first(),
        sim_hours.len() == inputs.simulation.hours.len(),
    ) {
        (Some(run), true) => {
            let last = *run.last().expect("non-empty run");
            let end_soc = inputs.simulation.hours[last].soc_pct;
            let post = &inputs.simulation.hours[(last + 1).min(inputs.simulation.hours.len())..];
            let trough = if post.is_empty() {
                end_soc
            } else {
                post.iter().map(|h| h.soc_pct).fold(f64::INFINITY, f64::min)
            };
            (trough, end_soc)
        }
        _ => (observed_min_soc_pct, observed_min_soc_pct),
    };

    // Deficit: lift BOTH the post-window trough AND the window-end SOC
    // to the minimum. A charge that merely survives the trough but
    // leaves the battery below the floor all night is still a violation;
    // a charge that reaches the floor at 05:00 but dips below it by the
    // evening trough is the classic under-charge this v2 objective
    // exists to catch.
    let stored_needed_kwh =
        (inputs.target_soc_pct - fixable_trough.min(soc_at_window_end)).max(0.0) / 100.0 * capacity;
    let ac_needed = stored_needed_kwh / eta;
    let mut kwh = ac_needed.min(deliverable_ac);

    // Re-simulate with the charge applied and iterate until the
    // post-window trough clears the minimum (or the window's
    // deliverable cap is reached). The previous analytic "uniform
    // lift" model (`observed_min + kwh * eta / capacity * 100`)
    // overstates the lift whenever the trough falls AFTER the charge
    // window — intermediate discharge consumes the charge before the
    // trough is reached, so the real SOC there is unchanged. Only a
    // re-run of the hourly simulation with the charge injected into
    // the window hours catches that; the shortfall-feedback loop then
    // grows the ask until the simulated trajectory actually holds.
    // Callers without the per-hour series (tests / degraded inputs)
    // keep the analytic estimate.
    let (
        after_min_soc_pct,
        charge_target_soc_pct,
        with_charge_series,
        import_tomorrow_with_charge_kwh,
        export_tomorrow_with_charge_kwh,
    ) = if !runs.is_empty() {
        let mut outcome = simulate_with_charge(sim_hours, inputs.params, &runs, kwh);
        for _ in 0..10 {
            if outcome.trough_pct >= inputs.target_soc_pct
                || kwh >= deliverable_ac - 1e-9
                || !outcome.trough_pct.is_finite()
            {
                break;
            }
            let shortfall_stored =
                (inputs.target_soc_pct - outcome.trough_pct).max(0.0) / 100.0 * capacity;
            let next_kwh = (kwh + shortfall_stored / eta).min(deliverable_ac);
            if (next_kwh - kwh).abs() < 1e-6 {
                break;
            }
            kwh = next_kwh;
            outcome = simulate_with_charge(sim_hours, inputs.params, &runs, kwh);
        }
        // Minimise: the grow loop above only ever ADDS to the ask, and
        // its fixed-point steps overshoot when the trough responds
        // super-linearly to the per-night kWh (which it does whenever
        // the window repeats across several nights — each night's
        // charge lifts the next day's starting point, and near the
        // reserve floor the response is a cliff). The whole point of
        // the plan is to buy as little grid import as the floor needs,
        // so once the loop holds the floor, bisect back down to the
        // smallest ask that still holds it. This includes the case
        // where the ask was clamped AT the deliverable cap and the
        // capped ask holds the floor (deep deficit + narrow window):
        // every bisection probe stays <= the cap, so shrinking is
        // safe. Only a capped ask that still FAILS the floor has
        // nothing to shrink — the capped caveat below reports that
        // honestly.
        if outcome.trough_pct >= inputs.target_soc_pct && kwh > 0.0 {
            let mut lo = 0.0_f64; // known-failing
            let mut hi = kwh; // known-good
            for _ in 0..24 {
                if hi - lo <= 0.01 {
                    break;
                }
                let mid = (lo + hi) / 2.0;
                let probe = simulate_with_charge(sim_hours, inputs.params, &runs, mid);
                if probe.trough_pct >= inputs.target_soc_pct {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            kwh = hi;
            outcome = simulate_with_charge(sim_hours, inputs.params, &runs, kwh);
        }
        // Tomorrow's import/export under the plan, for the Tomorrow
        // tiles. "Tomorrow" = the first local date change in the
        // forward series (the series starts at the current hour, so
        // its first date is today). The what-if sim models the charge
        // as free surplus — window hours report import 0 — so the
        // window's grid draw is added back explicitly, once per
        // occurrence that STARTS tomorrow (the horizon spans several
        // nights; the tile is a one-day summary, so later occurrences
        // belong to their own day's numbers).
        let local_date = |ts: i64| {
            chrono::DateTime::from_timestamp(ts, 0)
                .map(|dt| dt.with_timezone(&chrono::Local).date_naive())
        };
        let tomorrow_date = outcome
            .series
            .first()
            .and_then(|h| local_date(h.timestamp))
            .map(|d| d + chrono::Duration::days(1));
        let (import_tw, export_tw) = match tomorrow_date {
            Some(td) => {
                let is_t = |ts: i64| local_date(ts).is_some_and(|d| d == td);
                let residual: f64 = outcome
                    .series
                    .iter()
                    .filter(|h| is_t(h.timestamp))
                    .map(|h| h.import_kwh)
                    .sum();
                let export: f64 = outcome
                    .series
                    .iter()
                    .filter(|h| is_t(h.timestamp))
                    .map(|h| h.export_kwh)
                    .sum();
                let nights_tomorrow = runs
                    .iter()
                    .filter(|r| {
                        r.first()
                            .and_then(|&i| sim_hours.get(i))
                            .map(|h| is_t(h.timestamp))
                            .unwrap_or(false)
                    })
                    .count() as f64;
                (kwh * nights_tomorrow + residual, export)
            }
            None => (0.0, 0.0),
        };
        (
            outcome.trough_pct,
            outcome.charge_level_pct.max(inputs.target_soc_pct),
            outcome
                .series
                .iter()
                .map(|h| (h.timestamp, h.soc_pct))
                .collect(),
            import_tw,
            export_tw,
        )
    } else {
        let lift = kwh * eta / capacity * 100.0;
        (
            (observed_min_soc_pct + lift).min(100.0),
            (inputs.current_soc_pct + lift)
                .min(100.0)
                .max(inputs.target_soc_pct),
            // No window to inject into, so no with-charge projection to
            // overlay. The Battery tab falls back to solar-only.
            Vec::new(),
            0.0,
            0.0,
        )
    };

    // The honest caveat: when even the full deliverable charge can't
    // hold the minimum, say so instead of pretending the plan succeeds.
    let reaches_minimum = after_min_soc_pct >= inputs.target_soc_pct - 0.05;
    let rationale = if reaches_minimum {
        format!(
            "Battery is at {:.0}% now and the forecast trough drops to {:.0}% \
             over the next 48h. Charging {:.1} kWh per night in the {:.1}p \
             window ({}–{}) lifts the trough to {:.0}% — at or above your \
             {:.0}% minimum (about £{:.2} per night of grid import).",
            inputs.current_soc_pct,
            observed_min_soc_pct,
            kwh,
            window.rate * 100.0,
            hhmm(window.start_min),
            hhmm(window.end_min),
            after_min_soc_pct,
            inputs.target_soc_pct,
            kwh * window.rate,
        )
    } else {
        format!(
            "Battery is at {:.0}% now and the forecast trough drops to {:.0}% \
             over the next 48h. Even charging the full {:.1} kWh the {:.1}p \
             window ({}–{}) can deliver only lifts the trough to {:.0}% — \
             still below your {:.0}% minimum; the forecast drain is more \
             than this window can cover (about £{:.2} per night of grid import).",
            inputs.current_soc_pct,
            observed_min_soc_pct,
            kwh,
            window.rate * 100.0,
            hhmm(window.start_min),
            hhmm(window.end_min),
            after_min_soc_pct,
            inputs.target_soc_pct,
            kwh * window.rate,
        )
    };

    PlanRecommendation::Charge {
        window,
        kwh,
        min_soc_pct: inputs.target_soc_pct,
        observed_min_soc_pct,
        current_soc_pct: inputs.current_soc_pct,
        after_min_soc_pct,
        charge_target_soc_pct,
        rationale,
        with_charge_series,
        import_tomorrow_with_charge_kwh,
        export_tomorrow_with_charge_kwh,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forecast::simulate::{simulate_battery, SimHourInput};
    use crate::settings::TariffSlot;
    use chrono::{TimeZone, Timelike};

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

    /// Flux-like tariff: off-peak 02:00–05:00 at 9p, day 26p,
    /// peak 16:00–21:00 at 35p.
    fn flux_tariff() -> TariffConfig {
        tariff(&[
            ("00:00", "02:00", 0.26),
            ("02:00", "05:00", 0.09),
            ("05:00", "16:00", 0.26),
            ("16:00", "21:00", 0.35),
            ("21:00", "23:59", 0.26),
        ])
    }

    fn params() -> SimulationParams {
        SimulationParams {
            capacity_kwh: 10.0,
            start_soc_pct: 30.0,
            reserve_soc_pct: 10.0,
            max_charge_kw: 2.5,
            max_discharge_kw: 2.5,
            charge_efficiency: 0.9,
            discharge_efficiency: 0.95,
        }
    }

    fn hour_ts(h: u32, offset_days: i64) -> i64 {
        let date = chrono::Local::now().date_naive() + chrono::Duration::days(offset_days);
        chrono::Local
            .from_local_datetime(&date.and_hms_opt(h, 0, 0).unwrap())
            .earliest()
            .unwrap()
            .timestamp()
    }

    /// 24 hours of tomorrow: `solar[h]` / `cons[h]` per hour.
    fn simulate_tomorrow(
        solar: f64,
        cons: f64,
        p: &SimulationParams,
    ) -> (SimulationOutput, Vec<SimHourInput>) {
        let hours: Vec<SimHourInput> = (0..24)
            .map(|h| SimHourInput {
                timestamp: hour_ts(h, 1),
                solar_kwh: if (6..=19).contains(&h) { solar } else { 0.0 },
                consumption_kwh: cons,
            })
            .collect();
        let sim = simulate_battery(&hours, p);
        (sim, hours)
    }

    fn plan_inputs<'a>(
        sim: &'a SimulationOutput,
        sim_hours: &'a [SimHourInput],
        p: &'a SimulationParams,
        tariff: Option<&'a TariffConfig>,
    ) -> PlanInputs<'a> {
        PlanInputs {
            simulation: sim,
            sim_hours: Some(sim_hours),
            params: p,
            import_tariff: tariff,
            target_soc_pct: 60.0,
            consumption_tomorrow_kwh: 12.0,
            consumption_sufficient: true,
            // 22:00 — well past solar, close enough to Flux's 02:00 that
            // the planner's "ready for tomorrow" branch is exercised.
            now_min: 22 * 60,
            current_soc_pct: 30.0,
        }
    }

    #[test]
    fn cheapest_window_picks_the_off_peak_slot() {
        // 02:00–05:00 at 9p, planning at 22:00 means the slot is on
        // TOMORROW's calendar day.
        let w = cheapest_import_window(&flux_tariff(), 12 * 60, 60).unwrap();
        assert_eq!(w.start_min, 2 * 60);
        assert_eq!(w.end_min, 5 * 60);
        assert!(w.tomorrow);
        assert!((w.rate - 0.09).abs() < 1e-9);
    }

    #[test]
    fn cheapest_window_skips_windows_already_passed() {
        // Planning at 04:00: only 60 min of the off-peak remain, which
        // still satisfies a 60-minute minimum.
        let w = cheapest_import_window(&flux_tariff(), 4 * 60, 60).unwrap();
        assert_eq!(w.start_min, 2 * 60);
        assert!(w.tomorrow);
        // At 04:30 nothing of tonight's off-peak remains (30 min < 60) —
        // the planner must not return a window in the past.
        let w = cheapest_import_window(&flux_tariff(), 4 * 60 + 30, 60);
        let w = w.expect("some window");
        // Wrapping: tomorrow's 02:00 is the earliest forward occurrence.
        assert!(w.tomorrow && w.start_min == 2 * 60);
    }

    #[test]
    fn cheapest_window_rejects_too_short_remainders() {
        // With no off-peak slot in the tariff, the planner must fall
        // back to the cheapest day-rate window that satisfies the
        // minimum duration. (The Flux off-peak recurs each night and
        // always fits a 120-min minimum — so we use a no-off-peak tariff
        // here to exercise the fallback path.)
        let flat_top = tariff(&[
            ("00:00", "06:00", 0.26),
            ("06:00", "16:00", 0.20),
            ("16:00", "21:00", 0.35),
            ("21:00", "23:59", 0.26),
        ]);
        let w = cheapest_import_window(&flat_top, 12 * 60, 120).unwrap();
        assert!((w.rate - 0.20).abs() < 1e-9);
        assert_eq!(w.start_min, 6 * 60);
        assert_eq!(w.end_min, 16 * 60);
        assert!(w.end_min - w.start_min >= 120);
    }

    #[test]
    fn pure_offpeak_at_60min_minimum_uses_wrapped_window() {
        // The off-peak recurs each night, so even at 04:30 (after the
        // tonight occurrence) the planner wraps to tomorrow's.
        let w = cheapest_import_window(&flux_tariff(), 4 * 60 + 30, 60).unwrap();
        assert!(w.tomorrow);
        assert_eq!(w.start_min, 2 * 60);
        assert_eq!(w.end_min, 5 * 60);
    }

    #[test]
    fn flat_tariff_has_no_exploitable_window() {
        let flat = tariff(&[("00:00", "23:59", 0.25)]);
        // A single all-day slot is returned as-is (charge anytime); the
        // planner's *recommendation* logic treats it as no cheap window.
        let w = cheapest_import_window(&flat, 0, 60).unwrap();
        assert_eq!(w.start_min, 0);
        assert_eq!(w.end_min, 23 * 60 + 59);
        assert!(!w.tomorrow);
    }

    /// A tariff slot that wraps midnight ("22:00"–"02:00") is never a
    /// candidate charge window: `cheapest_import_window` skips any slot
    /// whose end is at or before its start, and `TariffConfig::validate`
    /// rejects such slots outright. This is what keeps every
    /// charge-window occurrence inside a single calendar day —
    /// `window_runs` matches minute-of-day within `[start, end)`, so a
    /// non-wrapping window's runs can never straddle midnight, which in
    /// turn makes the Tomorrow tiles' "count the window draw once per
    /// occurrence that starts tomorrow" attribution exact rather than
    /// an approximation. If the wrap-skip here is ever relaxed, the
    /// tile attribution and `window_runs` both need re-examining first.
    #[test]
    fn wrapping_tariff_slot_is_never_selected() {
        let wrap = tariff(&[
            ("02:00", "22:00", 0.30),
            ("22:00", "02:00", 0.05), // wraps midnight — must be skipped
        ]);
        // Even though the wrapping slot is the cheapest, it must never
        // win: the valid day slot is selected instead.
        let w = cheapest_import_window(&wrap, 12 * 60, 30).unwrap();
        assert_eq!(w.start_min, 2 * 60);
        assert_eq!(w.end_min, 22 * 60);
        // And with ONLY the wrapping slot present there is no window
        // at all.
        let only_wrap = tariff(&[("22:00", "02:00", 0.05)]);
        assert!(cheapest_import_window(&only_wrap, 12 * 60, 30).is_none());
    }

    #[test]
    fn sunny_day_needs_no_charge() {
        // Steady solar + light load keeps SOC above 60% across all
        // hours (start 80% with 1.4 kWh/h solar — even tiny loads keep
        // the trough above the 60% minimum).
        let p = params();
        let solar = {
            let mut s = [0.0; 24];
            for h in 6..=19 {
                s[h as usize] = 1.4;
            }
            s
        };
        let cons = [0.15; 24];
        let mut p_high = p;
        p_high.start_soc_pct = 80.0;
        let (sim, sim_hours) = simulate_tomorrow_at(80.0, solar, cons, &p_high);
        let flux = flux_tariff();
        match plan_overnight_charge(&plan_inputs(&sim, &sim_hours, &p_high, Some(&flux))) {
            PlanRecommendation::NoChargeNeeded {
                observed_min_soc_pct,
                min_soc_pct,
                ..
            } => {
                assert!(
                    observed_min_soc_pct > min_soc_pct,
                    "sunny projection must exceed min: trough={observed_min_soc_pct} min={min_soc_pct}"
                );
            }
            other => panic!("expected NoChargeNeeded, got {other:?}"),
        }
    }

    /// 24-hour sim starting from `soc` (the existing helper hard-codes
    /// 30%, which is fine for many tests but the new sunny-day test
    /// needs to start high enough that the overnight trough stays
    /// above min_soc_pct = 60).
    fn simulate_tomorrow_at(
        soc: f64,
        solar_kwh_per_hour: [f64; 24],
        cons_kwh_per_hour: [f64; 24],
        p: &SimulationParams,
    ) -> (SimulationOutput, Vec<SimHourInput>) {
        let mut p = *p;
        p.start_soc_pct = soc;
        let hours: Vec<SimHourInput> = (0..24)
            .map(|h| SimHourInput {
                timestamp: hour_ts(h as u32, 1),
                solar_kwh: solar_kwh_per_hour[h as usize],
                consumption_kwh: cons_kwh_per_hour[h as usize],
            })
            .collect();
        let sim = simulate_battery(&hours, &p);
        (sim, hours)
    }

    #[test]
    fn cloudy_day_charges_in_the_cheap_window() {
        // Winter-ish: 14 h × 0.2 kWh solar vs 24 h × 0.5 consumption —
        // the battery drains; the planner should recommend tomorrow's
        // 02:00–05:00 off-peak. The all-day drain exceeds what one
        // window can deliver, so the honest recommendation charges the
        // window's full capacity and reports a truthful trough that
        // stays below the 60% minimum (the old analytic model claimed
        // the charge reached it — the bug this v2 objective fixes).
        let p = params();
        let (sim, sim_hours) = simulate_tomorrow(0.2, 0.5, &p);
        let flux = flux_tariff();
        match plan_overnight_charge(&plan_inputs(&sim, &sim_hours, &p, Some(&flux))) {
            PlanRecommendation::Charge {
                window,
                kwh,
                min_soc_pct,
                observed_min_soc_pct,
                after_min_soc_pct,
                charge_target_soc_pct,
                rationale,
                current_soc_pct,
                ..
            } => {
                assert!(window.tomorrow);
                assert_eq!(window.start_min, 2 * 60);
                assert_eq!(window.end_min, 5 * 60);
                assert!((window.rate - 0.09).abs() < 1e-9);
                // The ask is clamped at what the 3h window at 2.5 kW can
                // physically deliver — the deficit (60% floor vs a
                // battery pinned at reserve all day) exceeds it.
                assert!((kwh - 7.5).abs() < 1e-6, "kwh {kwh} should hit the cap");
                assert!((min_soc_pct - 60.0).abs() < 1e-9);
                // The trough dipped below the minimum.
                assert!(observed_min_soc_pct < min_soc_pct);
                // The charge helps (truthful trough above the uncharged
                // one) but the re-simulated trajectory still dips below
                // the minimum — and the planner says so instead of
                // pretending otherwise.
                assert!(
                    after_min_soc_pct > observed_min_soc_pct,
                    "capped charge still lifts the trough: {after_min_soc_pct} vs {observed_min_soc_pct}"
                );
                assert!(after_min_soc_pct < min_soc_pct);
                assert!(rationale.contains("still below"), "rationale: {rationale}");
                // The slot target is the level the battery actually
                // reaches in the window — above the 60% floor ask.
                assert!(charge_target_soc_pct > 60.0);
                // The planner round-trips the live current SOC so the UI
                // can show the user which number drove the kWh ask.
                assert!((current_soc_pct - 30.0).abs() < 1e-9);
            }
            other => panic!("expected Charge, got {other:?}"),
        }
    }

    /// The `Charge` recommendation carries the planner's re-simulated
    /// `with_charge_series` so the Battery tab chart can overlay the
    /// "if we follow the recommendation" trajectory on top of the
    /// recorded SOC history. The uncharged trough is lower than the
    /// with-charge trough, the series spans the full forecast horizon,
    /// and its timestamps align with the uncharged simulation.
    /// The Tomorrow tiles must reflect the plan, not the uncharged
    /// simulation: when a charge is recommended, tomorrow's expected
    /// import includes the window's grid draw (the what-if sim models
    /// the charge as free surplus, so its own `import_kwh` for window
    /// hours is zero — the recommendation must add the per-night kWh
    /// back for every window occurrence that starts tomorrow, and only
    /// tomorrow: the horizon spans several nights but the tile is a
    /// one-day summary). Export comes from the same what-if hours, no
    /// adjustment. The live report that surfaced this was an Expected
    /// Import tile reading 0.3 kWh against a 6.2 kWh charge plan.
    #[test]
    fn charge_recommendation_carries_tomorrow_import_with_charge() {
        let p = params();
        let solar = {
            let mut s = [0.0; 24];
            for slot in s.iter_mut().take(17).skip(9) {
                *slot = 1.4;
            }
            s
        };
        let cons: [f64; 24] =
            std::array::from_fn(|h| if (17..=21).contains(&h) { 1.0 } else { 0.45 });
        let (sim, sim_hours) = fixed_72h(46.0, solar, cons, &p);
        let flux = flux_tariff();
        let rec = plan_overnight_charge(&plan_inputs_with_min(
            &sim,
            &sim_hours,
            &p,
            Some(&flux),
            20.0,
        ));
        let PlanRecommendation::Charge {
            kwh,
            window,
            import_tomorrow_with_charge_kwh,
            export_tomorrow_with_charge_kwh,
            ..
        } = rec
        else {
            panic!("expected Charge, got {rec:?}")
        };
        assert!(kwh > 0.0);
        let runs = window_runs(&sim_hours, &window);
        assert!(!runs.is_empty());
        let tomorrow_date = chrono::DateTime::from_timestamp(sim_hours[0].timestamp, 0)
            .unwrap()
            .with_timezone(&chrono::Local)
            .date_naive()
            + chrono::Duration::days(1);
        let is_tomorrow = |ts: i64| {
            chrono::DateTime::from_timestamp(ts, 0)
                .unwrap()
                .with_timezone(&chrono::Local)
                .date_naive()
                == tomorrow_date
        };
        let what_if = simulate_with_charge(&sim_hours, &p, &runs, kwh);
        let residual: f64 = what_if
            .series
            .iter()
            .filter(|h| is_tomorrow(h.timestamp))
            .map(|h| h.import_kwh)
            .sum();
        let export: f64 = what_if
            .series
            .iter()
            .filter(|h| is_tomorrow(h.timestamp))
            .map(|h| h.export_kwh)
            .sum();
        // Exactly one window occurrence starts tomorrow (the 72h series
        // holds two nights; only the first counts toward the tile).
        let runs_tomorrow = runs
            .iter()
            .filter(|r| is_tomorrow(sim_hours[r[0]].timestamp))
            .count();
        assert_eq!(runs_tomorrow, 1, "72h fixture: one window starts tomorrow");
        assert_eq!(runs.len(), 3, "...and the horizon holds three nights total");
        assert!(
            (import_tomorrow_with_charge_kwh - (kwh + residual)).abs() < 1e-6,
            "import tile = window draw + residual: {} vs {}",
            import_tomorrow_with_charge_kwh,
            kwh + residual
        );
        assert!((export_tomorrow_with_charge_kwh - export).abs() < 1e-6);
        // The window draw is included exactly once — not once per night.
        assert!(import_tomorrow_with_charge_kwh >= kwh - 1e-9);
        assert!(import_tomorrow_with_charge_kwh < 2.0 * kwh);
    }

    #[test]
    fn charge_recommendation_exposes_with_charge_series() {
        let p = params();
        let (sim, sim_hours) = simulate_tomorrow(0.2, 0.5, &p);
        let flux = flux_tariff();
        let PlanRecommendation::Charge {
            with_charge_series,
            observed_min_soc_pct,
            after_min_soc_pct,
            ..
        } = plan_overnight_charge(&plan_inputs(&sim, &sim_hours, &p, Some(&flux)))
        else {
            panic!("expected Charge recommendation");
        };
        assert_eq!(
            with_charge_series.len(),
            sim.hours.len(),
            "with_charge_series should cover the same horizon as sim_hours"
        );
        for (i, (ts, _soc)) in with_charge_series.iter().enumerate() {
            assert_eq!(
                *ts, sim.hours[i].timestamp,
                "timestamps must align with the uncharged horizon so the UI can plot them on one x-axis"
            );
        }
        let with_charge_min = with_charge_series
            .iter()
            .map(|(_, s)| *s)
            .fold(f64::INFINITY, f64::min);
        let uncharged_min = sim
            .hours
            .iter()
            .map(|h| h.soc_pct)
            .fold(f64::INFINITY, f64::min);
        assert!(
            with_charge_min >= uncharged_min - 1e-9,
            "with-charge trough {with_charge_min} should not dip below uncharged trough {uncharged_min}"
        );
        assert!(
            (with_charge_min - after_min_soc_pct).abs() < 1e-6,
            "after_min_soc_pct ({after_min_soc_pct}) should equal the min of the with_charge_series ({with_charge_min})"
        );
        assert!(
            observed_min_soc_pct < with_charge_min,
            "planning must show real lift: observed {observed_min_soc_pct} < with-charge {with_charge_min}"
        );
    }

    /// While the recommended charge is running, the battery's SOC can
    /// only rise or hold — a timed AC charge covers the window hours'
    /// load and adds charge on top, so the eco simulation never draws
    /// the battery down inside the window. Pins the property behind the
    /// user-reported "SOC decreasing during the charging period": that
    /// reading came from comparing against the misaligned midnight-
    /// anchored consumption chart, not from the with-charge line itself.
    /// The planner's objective is MINIMAL import: charge just enough
    /// that the re-simulated trough holds the user's floor — never more.
    /// The original grow-only sizing loop overshot badly on multi-night
    /// horizons (each top-up step adds the shortfall measured at the
    /// trough, but because the window repeats every night the trough
    /// responds super-linearly, so the last step jumped well past the
    /// floor and the ask was left oversized — the live-data report was
    /// a 2.7 kWh ask landing the trough at 31% against a 20% floor).
    /// This test scans for the true minimal ask and requires the
    /// recommendation to be within 0.05 kWh of it.
    #[test]
    fn charge_ask_is_minimal_not_oversized() {
        // Deterministic 72-hour fixture (fixed date, all hours present)
        // shaped like the live report: start SOC 46%, flat overnight
        // drain with an evening peak, daytime solar that recovers most
        // but not all of the day's use. The uncharged post-window trough
        // dips well below the 20% floor, so a charge is required.
        let p = params();
        let solar = {
            let mut s = [0.0; 24];
            for slot in s.iter_mut().take(17).skip(9) {
                *slot = 1.4;
            }
            s
        };
        let cons: [f64; 24] =
            std::array::from_fn(|h| if (17..=21).contains(&h) { 1.0 } else { 0.45 });
        let (sim, sim_hours) = fixed_72h(46.0, solar, cons, &p);
        let flux = flux_tariff();
        let rec = plan_overnight_charge(&plan_inputs_with_min(
            &sim,
            &sim_hours,
            &p,
            Some(&flux),
            20.0,
        ));
        let PlanRecommendation::Charge {
            kwh,
            window,
            after_min_soc_pct,
            ..
        } = rec
        else {
            panic!("expected Charge, got {rec:?}")
        };
        assert!(
            after_min_soc_pct >= 20.0,
            "the recommendation must hold the floor (got {after_min_soc_pct})"
        );
        // Ground truth: scan for the minimal per-night kWh whose
        // re-simulated post-window trough still clears 20%.
        let runs = window_runs(&sim_hours, &window);
        assert!(!runs.is_empty());
        let holds = |ask: f64| simulate_with_charge(&sim_hours, &p, &runs, ask).trough_pct >= 20.0;
        assert!(
            holds(kwh),
            "recommended {kwh} kWh must itself hold the floor"
        );
        // Tightness: shaving a further 0.1 kWh off the recommended ask
        // must break the floor. If a smaller ask still holds, the
        // planner is buying more grid import than the floor needs (the
        // grow-only sizing loop's failure mode — a coarse scan can't
        // measure this when the ask is already near-minimal, but the
        // boundary probe can).
        let slack = kwh - 0.1;
        if slack > 0.0 {
            assert!(
                !holds(slack),
                "ask {kwh:.3} kWh is not minimal — {slack:.3} kWh still holds the floor, so the planner is buying more import than the floor needs"
            );
        }
    }

    /// The minimisation bisection must run whenever the capped ask
    /// HOLDS the floor — including when the ask sits at exactly the
    /// window's deliverable cap. Deep-deficit / narrow-window shapes
    /// hit this: the analytic shortfall exceeds what the window can
    /// deliver, so `kwh` is clamped to `deliverable_ac` from the very
    /// first sizing step, and the multi-night carry-over (each night's
    /// capped charge lifts the next day's starting point) makes that
    /// capped charge hold the floor with room to spare. The grow loop
    /// breaks immediately on `trough >= target`, and the old
    /// `kwh < deliverable_ac` guard then skipped the bisection —
    /// leaving the ask pinned at the cap even though a smaller
    /// per-night charge holds the floor, i.e. the planner buying more
    /// off-peak import than the minimum needs (the same failure mode
    /// `charge_ask_is_minimal_not_oversized` pins for the uncapped
    /// case, surviving at the cap boundary).
    #[test]
    fn charge_ask_is_minimal_even_when_capped() {
        // One-hour cheapest window: deliverable cap = 2.5 kW x 1 h.
        let narrow = tariff(&[("00:00", "01:00", 0.05), ("01:00", "23:59", 0.30)]);
        let p = params();
        // Start at 30%, flat 0.025 kWh/h drain, no solar: the 72 h
        // uncharged trough lands near 11% against a 45% floor, so the
        // analytic ask (~3.8 kWh) overshoots the 2.5 kWh cap — but
        // three nights of capped charge compound to hold the floor
        // from day 0's end (46.4%) onward, while the true minimal
        // per-night ask is ~2.34 kWh.
        let cons = [0.025; 24];
        let (sim, sim_hours) = fixed_72h(30.0, [0.0; 24], cons, &p);
        let rec = plan_overnight_charge(&plan_inputs_with_min(
            &sim,
            &sim_hours,
            &p,
            Some(&narrow),
            45.0,
        ));
        let PlanRecommendation::Charge {
            kwh,
            window,
            after_min_soc_pct,
            ..
        } = rec
        else {
            panic!("expected Charge, got {rec:?}")
        };
        // Sanity: the fixture really is the capped-and-holding shape.
        // If the floor did NOT hold at the cap, the recommendation
        // would be the honest capped caveat instead — retune the
        // fixture before trusting a failure here.
        assert!(
            after_min_soc_pct >= 45.0,
            "capped ask must hold the floor (got {after_min_soc_pct})"
        );
        let runs = window_runs(&sim_hours, &window);
        assert!(!runs.is_empty());
        let holds = |ask: f64| simulate_with_charge(&sim_hours, &p, &runs, ask).trough_pct >= 45.0;
        assert!(
            holds(kwh),
            "recommended {kwh} kWh must itself hold the floor"
        );
        // The ask must not sit pinned at the window's deliverable cap
        // (2.5 kWh here) when a smaller ask still holds the floor.
        let cap = p.max_charge_kw * (window.end_min - window.start_min) as f64 / 60.0;
        assert!(
            kwh < cap - 1e-6,
            "ask {kwh:.3} kWh is pinned at the deliverable cap {cap:.3} — \
             the bisection must shrink it to the minimal holding ask"
        );
        // Tightness: shaving a further 0.1 kWh off the recommended ask
        // must break the floor (same boundary probe as the uncapped
        // test).
        let slack = kwh - 0.1;
        if slack > 0.0 {
            assert!(
                !holds(slack),
                "ask {kwh:.3} kWh is not minimal — {slack:.3} kWh still holds the floor, so the planner is buying more import than the floor needs"
            );
        }
    }

    /// Deterministic series on a FIXED local date (no wall-clock
    /// dependence): every hour present, so the window occurrences land
    /// at stable indices regardless of when the test runs.
    /// `build_48h_series` anchors on `now` and makes trough shapes vary
    /// by run time — unusable for a test that pins sizing behaviour
    /// (the `sunny_48h` test shipped that way and only passed when run
    /// early enough in the day for its same-day solar to top the battery
    /// up; evening/CI runs dipped below the floor and failed).
    fn fixed_series(
        days: i64,
        start_soc: f64,
        solar_kwh_per_hour: [f64; 24],
        cons_kwh_per_hour: [f64; 24],
        p: &SimulationParams,
    ) -> (SimulationOutput, Vec<SimHourInput>) {
        use chrono::TimeZone;
        let base = chrono::NaiveDate::from_ymd_opt(2024, 6, 15).unwrap();
        let mut hours: Vec<SimHourInput> = Vec::new();
        for d in 0..days {
            for h in 0..24u32 {
                let day = base + chrono::Duration::days(d);
                let ts = chrono::Local
                    .from_local_datetime(&day.and_hms_opt(h, 0, 0).unwrap())
                    .earliest()
                    .unwrap()
                    .timestamp();
                hours.push(SimHourInput {
                    timestamp: ts,
                    solar_kwh: solar_kwh_per_hour[h as usize],
                    consumption_kwh: cons_kwh_per_hour[h as usize],
                });
            }
        }
        let mut p2 = *p;
        p2.start_soc_pct = start_soc;
        let sim = simulate_battery(&hours, &p2);
        (sim, hours)
    }

    /// 48-hour fixed-date variant — the horizon the min-soc sizing tests
    /// were written against. See [`fixed_series`] for why the date is
    /// pinned instead of anchored on the wall clock.
    fn fixed_48h(
        start_soc: f64,
        solar_kwh_per_hour: [f64; 24],
        cons_kwh_per_hour: [f64; 24],
        p: &SimulationParams,
    ) -> (SimulationOutput, Vec<SimHourInput>) {
        fixed_series(2, start_soc, solar_kwh_per_hour, cons_kwh_per_hour, p)
    }

    /// 72-hour fixed-date variant. See [`fixed_series`].
    fn fixed_72h(
        start_soc: f64,
        solar_kwh_per_hour: [f64; 24],
        cons_kwh_per_hour: [f64; 24],
        p: &SimulationParams,
    ) -> (SimulationOutput, Vec<SimHourInput>) {
        fixed_series(3, start_soc, solar_kwh_per_hour, cons_kwh_per_hour, p)
    }

    #[test]
    fn with_charge_soc_never_decreases_inside_the_window() {
        let p = params();
        let (sim, sim_hours) = simulate_tomorrow(0.2, 0.5, &p);
        let flux = flux_tariff();
        let rec = plan_overnight_charge(&plan_inputs(&sim, &sim_hours, &p, Some(&flux)));
        let PlanRecommendation::Charge {
            window,
            with_charge_series,
            kwh,
            ..
        } = rec
        else {
            panic!("expected Charge, got {rec:?}")
        };
        assert!(kwh > 0.0);
        // Re-find the window hours in the series and check monotone
        // non-decreasing SOC across each run's hours.
        let runs = window_runs(&sim_hours, &window);
        assert!(
            !runs.is_empty(),
            "recommendation implies at least one window run"
        );
        for run in &runs {
            for w in run.windows(2) {
                let (_, soc_a) = with_charge_series[w[0]];
                let (_, soc_b) = with_charge_series[w[1]];
                assert!(
                    soc_b >= soc_a - 1e-9,
                    "SOC fell inside the charge window: {soc_a}% -> {soc_b}% at series index {w:?}"
                );
            }
        }
    }

    #[test]
    fn charge_kwh_accounts_for_predicted_solar() {
        // Compare a sunny case (battery reaches target on its own) to a
        // cloudy case (needs a charge). The ask must be zero / smaller
        // in the sunny case — the planner is reading simulated SOC.
        let p = params();
        let flux = flux_tariff();
        let (sim_sunny, sunny_hours) = simulate_tomorrow(1.5, 0.3, &p);
        let (sim_cloudy, cloudy_hours) = simulate_tomorrow(0.2, 0.5, &p);
        let kwh_sunny =
            match plan_overnight_charge(&plan_inputs(&sim_sunny, &sunny_hours, &p, Some(&flux))) {
                PlanRecommendation::NoChargeNeeded { .. } => 0.0,
                PlanRecommendation::Charge { kwh, .. } => kwh,
                other => panic!("unexpected {other:?}"),
            };
        let kwh_cloudy = match plan_overnight_charge(&plan_inputs(
            &sim_cloudy,
            &cloudy_hours,
            &p,
            Some(&flux),
        )) {
            PlanRecommendation::Charge { kwh, .. } => kwh,
            other => panic!("expected Charge, got {other:?}"),
        };
        assert!(
            kwh_sunny < kwh_cloudy,
            "sunny case must ask for less: {kwh_sunny} !< {kwh_cloudy}"
        );
    }

    #[test]
    fn charge_is_clamped_by_window_time_and_rate() {
        // 1 kWh battery-ish scale but a huge deficit: the ask cannot
        // exceed max_charge_kw × window hours, no matter the deficit.
        let p = params();
        let (sim, sim_hours) = simulate_tomorrow(0.0, 0.8, &p);
        let flux = flux_tariff();
        match plan_overnight_charge(&plan_inputs(&sim, &sim_hours, &p, Some(&flux))) {
            PlanRecommendation::Charge { window, kwh, .. } => {
                let hours = (window.end_min - window.start_min) as f64 / 60.0;
                assert!(
                    kwh <= p.max_charge_kw * hours + 1e-9,
                    "kwh {kwh} exceeds rate cap {}",
                    p.max_charge_kw * hours
                );
            }
            other => panic!("expected Charge, got {other:?}"),
        }
    }

    #[test]
    fn no_tariff_config_yields_no_plan() {
        let p = params();
        let (sim, sim_hours) = simulate_tomorrow(0.2, 0.5, &p);
        match plan_overnight_charge(&plan_inputs(&sim, &sim_hours, &p, None)) {
            PlanRecommendation::NoPlan { reason } => {
                assert!(reason.to_lowercase().contains("tariff"), "{reason}");
            }
            other => panic!("expected NoPlan, got {other:?}"),
        }
    }

    #[test]
    fn insufficient_history_yields_no_plan() {
        let p = params();
        let (sim, sim_hours) = simulate_tomorrow(0.2, 0.5, &p);
        let flux = flux_tariff();
        let mut inputs = plan_inputs(&sim, &sim_hours, &p, Some(&flux));
        inputs.consumption_sufficient = false;
        match plan_overnight_charge(&inputs) {
            PlanRecommendation::NoPlan { reason } => {
                assert!(reason.to_lowercase().contains("history"), "{reason}");
            }
            other => panic!("expected NoPlan, got {other:?}"),
        }
    }

    #[test]
    fn no_projection_yields_no_plan() {
        let empty = SimulationOutput {
            hours: Vec::new(),
            total_import_kwh: 0.0,
            total_export_kwh: 0.0,
        };
        let p = params();
        let flux = flux_tariff();
        let mut inputs = plan_inputs(&empty, &[], &p, Some(&flux));
        inputs.consumption_sufficient = true;
        match plan_overnight_charge(&inputs) {
            PlanRecommendation::NoPlan { reason } => {
                assert!(reason.to_lowercase().contains("projection"), "{reason}");
            }
            other => panic!("expected NoPlan, got {other:?}"),
        }
    }

    #[test]
    fn rationale_mentions_the_numbers() {
        let p = params();
        let flux = flux_tariff();
        let (sim, sim_hours) = simulate_tomorrow(0.2, 0.5, &p);
        if let PlanRecommendation::Charge {
            rationale,
            current_soc_pct,
            ..
        } = plan_overnight_charge(&plan_inputs(&sim, &sim_hours, &p, Some(&flux)))
        {
            // The rate should always appear in the rationale regardless
            // of which window was picked.
            assert!(rationale.contains("9.0p") || rationale.contains("26.0p"));
            assert!(rationale.contains("02:00") || rationale.contains("05:00"));
            // The live current SOC drives the rationale and the kWh ask,
            // so it must be present in the user-visible text.
            assert!(rationale.contains("30%"), "rationale: {rationale}");
            assert!((current_soc_pct - 30.0).abs() < 1e-9);
        } else {
            panic!("expected Charge");
        }
    }
    // --- Phase 2 v2: "minimum SOC across the window" objective ---------------
    //
    // The v2 sizing tests below all use fixed_48h (fixed date, every hour
    // present). There is deliberately NO wall-clock-anchored builder here:
    // the old build_48h_sim anchored its series on Local::now(), so the
    // same test dipped below the min-soc floor or not depending on the
    // hour it ran at — green in daytime CI runs, red in the evening
    // (v0.75.8's release run caught it). Tests that genuinely exercise
    // the forward-only payload contract use build_48h_series and derive
    // their expectations from the same series they feed the planner.

    fn plan_inputs_with_min<'a>(
        sim: &'a SimulationOutput,
        sim_hours: &'a [SimHourInput],
        p: &'a SimulationParams,
        tariff: Option<&'a TariffConfig>,
        min_soc_pct: f64,
    ) -> PlanInputs<'a> {
        PlanInputs {
            simulation: sim,
            sim_hours: Some(sim_hours),
            params: p,
            import_tariff: tariff,
            // The existing input field is `target_soc_pct` — during the
            // swap to the v2 objective we'll repurpose it to the min-soc.
            target_soc_pct: min_soc_pct,
            consumption_tomorrow_kwh: 12.0,
            consumption_sufficient: true,
            now_min: 22 * 60,
            current_soc_pct: 30.0,
        }
    }

    #[test]
    fn sunny_48h_above_minimum_returns_no_charge() {
        // Steady solar + light load keeps SOC above 50% throughout — no
        // charge needed even with min_soc_pct = 20.
        let p = params();
        let solar = {
            let mut s = [0.0; 24];
            for h in 6..=19 {
                s[h as usize] = 1.5;
            }
            s
        };
        let cons = [0.3; 24];
        let (sim, sim_hours) = fixed_48h(50.0, solar, cons, &p);
        let flux = flux_tariff();
        match plan_overnight_charge(&plan_inputs_with_min(
            &sim,
            &sim_hours,
            &p,
            Some(&flux),
            20.0,
        )) {
            PlanRecommendation::NoChargeNeeded {
                current_soc_pct,
                min_soc_pct,
                observed_min_soc_pct,
                ..
            } => {
                assert_eq!(min_soc_pct, 20.0);
                assert_eq!(current_soc_pct, 30.0);
                assert!(observed_min_soc_pct >= 20.0, "should not need a charge");
            }
            other => panic!("expected NoChargeNeeded, got {other:?}"),
        }
    }

    #[test]
    fn overnight_dip_below_minimum_asks_for_a_charge() {
        // An overnight-peak drain profile the planner CAN hold above the
        // 20% floor: 0.25 kWh/h baseline with a 1.0 kWh/h morning peak,
        // partly covered by midday solar. The uncharged trajectory dips
        // below 20% (floored at the 10% reserve), so a charge is required;
        // the sized charge must lift the re-simulated trough back to ≥ 20%.
        // (The original 0.6/3.0 profile drained ~2× the battery capacity
        // per day — no charge could hold 20% and the truthful answer was
        // the capped caveat.)
        let p = params();
        let solar = {
            let mut s = [0.0; 24];
            for h in 10..=15 {
                s[h as usize] = 1.0;
            }
            s
        };
        let cons: [f64; 24] =
            std::array::from_fn(|h| if (3..=5).contains(&h) { 1.0 } else { 0.25 });
        let (sim, sim_hours) = fixed_48h(30.0, solar, cons, &p);
        let flux = flux_tariff();
        match plan_overnight_charge(&plan_inputs_with_min(
            &sim,
            &sim_hours,
            &p,
            Some(&flux),
            20.0,
        )) {
            PlanRecommendation::Charge {
                window,
                kwh,
                min_soc_pct,
                observed_min_soc_pct,
                after_min_soc_pct,
                current_soc_pct,
                ..
            } => {
                assert_eq!(min_soc_pct, 20.0);
                assert_eq!(current_soc_pct, 30.0);
                assert!(observed_min_soc_pct < 20.0, "expected dip");
                assert!(after_min_soc_pct >= 19.9, "expected after_min >=20");
                assert!(kwh > 0.0);
                assert_eq!(window.start_min, 2 * 60);
            }
            other => panic!("expected Charge, got {other:?}"),
        }
    }

    #[test]
    fn charge_kwh_scales_with_min_soc() {
        // Same trajectory: a higher min_soc_pct asks for more kWh.
        let p = params();
        let solar = {
            let mut s = [0.0; 24];
            for h in 10..=15 {
                s[h as usize] = 1.0;
            }
            s
        };
        let cons: [f64; 24] = std::array::from_fn(|h| if (3..=5).contains(&h) { 3.0 } else { 0.6 });
        let flux = flux_tariff();
        let kwh_for = |m: f64| -> f64 {
            let (sim, sim_hours) = fixed_48h(30.0, solar, cons, &p);
            match plan_overnight_charge(&plan_inputs_with_min(&sim, &sim_hours, &p, Some(&flux), m))
            {
                PlanRecommendation::Charge { kwh, .. } => kwh,
                other => panic!("expected Charge, got {other:?}"),
            }
        };
        let kwh_mid = kwh_for(30.0);
        let kwh_high = kwh_for(60.0);
        assert!(
            kwh_high >= kwh_mid,
            "higher min must ask for at least as much: {kwh_high} vs {kwh_mid}"
        );
    }

    #[test]
    fn min_soc_floor_charges_even_with_high_end_soc() {
        // Battery already ends high (sunny tomorrow) — the old target-SOC
        // logic would say "no charge". With a high min_soc_pct and a real
        // overnight dip, the planner still has to charge.
        let p = params();
        let solar = {
            let mut s = [0.0; 24];
            for h in 10..=18 {
                s[h as usize] = 2.0;
            }
            s
        };
        let cons: [f64; 24] = std::array::from_fn(|h| if (3..=5).contains(&h) { 3.0 } else { 0.4 });
        let flux = flux_tariff();
        let (sim, sim_hours) = fixed_48h(80.0, solar, cons, &p);
        match plan_overnight_charge(&plan_inputs_with_min(
            &sim,
            &sim_hours,
            &p,
            Some(&flux),
            30.0,
        )) {
            PlanRecommendation::Charge {
                observed_min_soc_pct,
                kwh,
                ..
            } => {
                assert!(observed_min_soc_pct < 30.0, "expected dip");
                assert!(kwh > 0.0);
            }
            other => panic!("expected Charge from min_soc floor, got {other:?}"),
        }
    }

    /// Build a 72-hour series with explicit hourly solar/cons arrays,
    /// starting at the current hour (the forecast payload's forward-only
    /// contract, same horizon as the stored radiation window). Returns the
    /// simulation output and the hour inputs together — the planner needs
    /// the original series to re-run the simulation with the proposed
    /// charge applied.
    fn build_48h_series(
        start_soc: f64,
        solar_kwh_per_hour: [f64; 24],
        cons_kwh_per_hour: [f64; 24],
        p: &SimulationParams,
    ) -> (SimulationOutput, Vec<SimHourInput>) {
        let now = chrono::Local::now();
        let now_hour = now.hour();
        let mut hours: Vec<SimHourInput> = Vec::new();
        for d in 0..3i64 {
            for h in 0..24u32 {
                if d == 0 && h < now_hour {
                    continue;
                }
                let date = now.date_naive() + chrono::Duration::days(d);
                let ts = chrono::Local
                    .from_local_datetime(&date.and_hms_opt(h, 0, 0).unwrap())
                    .earliest()
                    .unwrap()
                    .timestamp();
                hours.push(SimHourInput {
                    timestamp: ts,
                    solar_kwh: solar_kwh_per_hour[h as usize],
                    consumption_kwh: cons_kwh_per_hour[h as usize],
                });
            }
        }
        let mut p = *p;
        p.start_soc_pct = start_soc;
        let sim = simulate_battery(&hours, &p);
        (sim, hours)
    }

    /// Re-simulate `sim_hours` with `kwh` AC injected into every
    /// 02:00–05:00 occurrence (the Flux off-peak), using the same load-
    /// offset semantics the planner's internal helper applies, and return
    /// the lowest SOC across hours after the FIRST window ends. Pure —
    /// used by tests to independently verify `after_min_soc_pct`.
    fn resim_trough_after_flux_window(
        sim_hours: &[SimHourInput],
        p: &SimulationParams,
        kwh: f64,
    ) -> f64 {
        let mut hours: Vec<SimHourInput> = sim_hours.to_vec();
        let per_hour_ac = kwh / 3.0;
        for h in hours.iter_mut() {
            let local = chrono::DateTime::from_timestamp(h.timestamp, 0)
                .unwrap()
                .with_timezone(&chrono::Local);
            let m = local.hour() as u16 * 60 + local.minute() as u16;
            if (120..300).contains(&m) {
                let unmet = (h.consumption_kwh - h.solar_kwh).max(0.0);
                h.solar_kwh += per_hour_ac + unmet;
            }
        }
        let sim = simulate_battery(&hours, p);
        // Trough over hours AFTER the first window occurrence ends (the
        // hour starting 05:00 is the first post-window hour).
        let mut seen_window = false;
        let mut min = f64::INFINITY;
        for h in &sim.hours {
            let local = chrono::DateTime::from_timestamp(h.timestamp, 0)
                .unwrap()
                .with_timezone(&chrono::Local);
            let m = local.hour() as u16 * 60 + local.minute() as u16;
            let in_window = (120..300).contains(&m);
            if in_window {
                seen_window = true;
                continue;
            }
            if seen_window {
                min = min.min(h.soc_pct);
            }
        }
        min
    }

    #[test]
    fn after_min_is_actual_resimulated_trough_not_analytic_lift() {
        // The user-reported bug (issue #283 planner v2): battery at 4%, min
        // SOC target 20%, planner proposes a charge sized only to lift the
        // TROUGH-hour SOC analytically (4%→20% ≈ 1.8 kWh) — but the charge
        // lands in the 02:00–05:00 window and the following day's drain
        // consumes it long before the next trough, so the battery still
        // falls to ~4%. The corrected planner must size the charge against
        // a real re-simulation of the whole forward window (charging far
        // more than the naive trough lift), report the truthful
        // `after_min_soc_pct`, and set a charge-slot target high enough for
        // the applied plan to actually hold.
        let p = params();
        // Modest, survivable drain: 0.12 kWh/h baseline with a 0.45 kWh/h
        // evening peak — sized so a full-window charge CAN hold 20% across
        // the inter-window span (≈40% of capacity per day).
        let solar = [0.0; 24];
        let cons: [f64; 24] =
            std::array::from_fn(|h| if (18..=21).contains(&h) { 0.45 } else { 0.12 });
        let (sim, sim_hours) = build_48h_series(4.0, solar, cons, &p);
        // Sanity: the uncharged trough sits at the 4% start SOC.
        let pre_min = sim
            .hours
            .iter()
            .map(|h| h.soc_pct)
            .fold(f64::INFINITY, f64::min);
        assert!(
            pre_min < 5.0,
            "pre-condition: uncharged trough ≈ 4%, got {pre_min}"
        );
        let flux = flux_tariff();
        let rec = plan_overnight_charge(&plan_inputs_with_min(
            &sim,
            &sim_hours,
            &p,
            Some(&flux),
            20.0,
        ));
        let PlanRecommendation::Charge {
            kwh,
            observed_min_soc_pct,
            after_min_soc_pct,
            charge_target_soc_pct,
            rationale,
            ..
        } = rec
        else {
            panic!("expected Charge, got {rec:?}")
        };
        assert!(observed_min_soc_pct < 20.0);
        // THE regression assertion: the naive analytic ask (≈1.8 kWh to
        // lift 4%→20%) is NOT enough — the planner must charge more to
        // survive the whole inter-window drain.
        let naive_ask =
            (20.0 - observed_min_soc_pct) / 100.0 * p.capacity_kwh / p.charge_efficiency;
        assert!(
            kwh > naive_ask + 1.0,
            "kwh {kwh} should exceed the naive analytic ask {naive_ask:.2} by a wide margin"
        );
        // The re-simulated trajectory actually holds the minimum...
        assert!(
            after_min_soc_pct >= 19.9,
            "after_min should reflect the real re-simulated trough (≥20%), got {after_min_soc_pct}"
        );
        // ...and the reported number matches an independent re-simulation
        // with the same kWh injected into the same window.
        let real_after_min = resim_trough_after_flux_window(&sim_hours, &p, kwh);
        assert!(
        (real_after_min - after_min_soc_pct).abs() < 0.5,
        "planner's after_min_soc_pct={after_min_soc_pct} should match independent re-sim trough {real_after_min}"
    );
        // The charge-slot target the Apply payload writes must be the level
        // the battery needs to REACH in the window (well above the 20%
        // minimum) — a slot target of 20% would stop charging at 20% and
        // let the battery crash back down, which is exactly the reported
        // failure mode.
        assert!(
            charge_target_soc_pct > 50.0,
            "charge slot target {charge_target_soc_pct} must be well above the 20% minimum"
        );
        // No caveat in the rationale — the plan holds.
        assert!(!rationale.contains("still below"), "rationale: {rationale}");
    }

    #[test]
    fn capped_window_reports_truthful_trough_below_minimum() {
        // A tiny charge rate can't deliver the kWh needed to bridge the
        // inter-window drain. The planner must NOT pretend the plan holds:
        // it reports the capped kWh with the truthful re-simulated trough
        // and a rationale that says the minimum can't be met — never the
        // analytic over-estimate the old code produced.
        let mut p = params();
        p.max_charge_kw = 0.5; // 3h window → max 1.5 kWh AC
        let solar = [0.0; 24];
        let cons: [f64; 24] =
            std::array::from_fn(|h| if (18..=21).contains(&h) { 0.45 } else { 0.12 });
        let (sim, sim_hours) = build_48h_series(4.0, solar, cons, &p);
        let flux = flux_tariff();
        let rec = plan_overnight_charge(&plan_inputs_with_min(
            &sim,
            &sim_hours,
            &p,
            Some(&flux),
            20.0,
        ));
        let PlanRecommendation::Charge {
            kwh,
            after_min_soc_pct,
            rationale,
            ..
        } = rec
        else {
            panic!("expected Charge, got {rec:?}")
        };
        // The ask is clamped at what the window can physically deliver.
        assert!(
            (kwh - 1.5).abs() < 1e-6,
            "kwh {kwh} should hit the 1.5 kWh cap"
        );
        // The trough truthfully stays below the minimum.
        assert!(
        after_min_soc_pct < 20.0,
        "after_min {after_min_soc_pct} must be the truthful capped trough, not the analytic 20%"
    );
        // And the rationale says so.
        assert!(rationale.contains("still below"), "rationale: {rationale}");
    }

    #[test]
    fn window_runs_finds_every_night_occurrence() {
        // 48h of hours at/after now: the 02:00–05:00 window appears on both
        // nights; each occurrence is a contiguous 3-hour run, and the two
        // runs are separated by the day's non-window hours.
        let p = params();
        let (sim, sim_hours) = build_48h_series(50.0, [0.0; 24], [0.2; 24], &p);
        let window = ChargeWindow {
            start_min: 2 * 60,
            end_min: 5 * 60,
            tomorrow: true,
            rate: 0.09,
        };
        let runs = window_runs(&sim_hours, &window);
        assert_eq!(runs.len(), 2, "both nights' occurrences: {runs:?}");
        for run in &runs {
            assert_eq!(run.len(), 3, "3-hour window: {run:?}");
            // Contiguous indices.
            for pair in run.windows(2) {
                assert_eq!(pair[1], pair[0] + 1);
            }
        }
        // The two runs are ~21 hours apart (first run ends hours before the
        // second starts).
        assert!(
            runs[1][0] - runs[0][0] >= 20,
            "runs not separated: {runs:?}"
        );
        let _ = sim; // series and output share length by construction
    }

    #[test]
    fn window_runs_no_match_yields_empty() {
        let p = params();
        let (_sim, sim_hours) = build_48h_series(50.0, [0.0; 24], [0.2; 24], &p);
        let window = ChargeWindow {
            start_min: 6 * 60 + 30, // 06:30–07:00 — contains no hour mark
            end_min: 7 * 60,
            tomorrow: false,
            rate: 0.09,
        };
        assert!(window_runs(&sim_hours, &window).is_empty());
    }

    #[test]
    fn charge_injection_reaches_battery_during_deficit_hours() {
        // The eco-mode simulator only charges from surplus, so the grid
        // charge injection must offset the hour's unmet load first: an
        // overnight deficit hour (cons 1.0, solar 0) with 2 kWh AC injected
        // must land 2 kWh × eta in the battery (+18pp on 10 kWh), not be
        // swallowed by the deficit.
        let mut p = params();
        p.start_soc_pct = 10.0;
        let now = chrono::Local::now();
        let ts = chrono::Local
            .from_local_datetime(&now.date_naive().and_hms_opt(2, 0, 0).unwrap())
            .earliest()
            .unwrap()
            .timestamp();
        // A single window hour in its own run.
        let hours = vec![SimHourInput {
            timestamp: ts,
            solar_kwh: 0.0,
            consumption_kwh: 1.0,
        }];
        let run = vec![0usize];
        let outcome = simulate_with_charge(&hours, &p, &[run], 2.0);
        assert!(
            (outcome.charge_level_pct - 28.0).abs() < 1e-9,
            "10% + 2kWh×0.9/10kWh = 28%, got {}",
            outcome.charge_level_pct
        );
        // Post-window hours are empty → trough == the window-end level.
        assert!((outcome.trough_pct - outcome.charge_level_pct).abs() < 1e-9);
    }

    #[test]
    fn charge_injection_adds_to_existing_surplus() {
        // A daytime hour that already has 0.5 kWh surplus: injecting 1 kWh
        // AC must charge 1.5 kWh AC total (surplus + grid), i.e. +13.5pp
        // on a 10 kWh battery from 10%.
        let mut p = params();
        p.start_soc_pct = 10.0;
        let now = chrono::Local::now();
        let ts = chrono::Local
            .from_local_datetime(&now.date_naive().and_hms_opt(2, 0, 0).unwrap())
            .earliest()
            .unwrap()
            .timestamp();
        let hours = vec![SimHourInput {
            timestamp: ts,
            solar_kwh: 1.0,
            consumption_kwh: 0.5,
        }];
        let run = vec![0usize];
        let outcome = simulate_with_charge(&hours, &p, &[run], 1.0);
        assert!(
            (outcome.charge_level_pct - 23.5).abs() < 1e-9,
            "10% + (0.5+1.0)×0.9/10 = 23.5%, got {}",
            outcome.charge_level_pct
        );
    }
}
