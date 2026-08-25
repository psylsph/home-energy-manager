//! Tariff-aware charge planner (issue #283, Phase 2).
//!
//! Turns the Phase 1 predictions into a recommendation the user can
//! apply with one tap: whether the battery needs an overnight grid
//! charge, how much, and in which cheapest tariff window. Pure and
//! synchronous — the API layer feeds it predictions + tariff config,
//! and Apply happens only from the UI through the existing control
//! endpoints. Nothing autonomous.

use crate::forecast::simulate::{SimulationParams, SimulationOutput};
use crate::settings::TariffConfig;

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
#[derive(Debug, Clone, PartialEq)]
pub enum PlanRecommendation {
    /// No overnight charge needed — the SOC projection already ends
    /// above the target.
    NoChargeNeeded {
        /// Projected end-of-window SOC, %.
        projected_end_soc_pct: f64,
    },
    /// Charge `kwh` AC in the window; the battery should then reach
    /// `projected_end_soc_pct` by the window's end.
    Charge {
        window: ChargeWindow,
        /// AC kWh to draw from the grid (battery kWh ÷ charge efficiency,
        /// before rate/time clamping).
        kwh: f64,
        /// Target SOC the charge aims for, %.
        target_soc_pct: f64,
        projected_end_soc_pct: f64,
        /// Human-readable rationale shown under the recommendation.
        rationale: String,
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
    /// Simulation params (capacity, efficiencies, rates) for the
    /// what-if recomputation.
    pub params: &'a SimulationParams,
    /// Import tariff; None or flat = no cheap window to exploit.
    pub import_tariff: Option<&'a TariffConfig>,
    /// SOC the evening should end above (typically the user's target or
    /// reserve + margin), percent.
    pub target_soc_pct: f64,
    /// Predicted consumption for tomorrow, kWh (for the rationale text).
    pub consumption_tomorrow_kwh: f64,
    /// Whether the consumption history was sufficient (Phase 1 gate).
    pub consumption_sufficient: bool,
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

    // Where the SOC lands without intervention: the simulation's final
    // value (covers the forward window through tomorrow evening).
    let projected_end = inputs
        .simulation
        .hours
        .last()
        .map(|h| h.soc_pct)
        .unwrap_or(0.0);
    if projected_end >= inputs.target_soc_pct {
        return PlanRecommendation::NoChargeNeeded {
            projected_end_soc_pct: projected_end,
        };
    }

    // Deficit: stored energy needed to lift the end-of-window SOC to the
    // target. The charge lands in the window before tomorrow's solar, so
    // the same solar the no-intervention simulation saw still arrives
    // afterwards — the stored gap is the ask.
    let capacity = inputs.params.capacity_kwh;
    let stored_needed_kwh = (inputs.target_soc_pct - projected_end) / 100.0 * capacity;

    // A 30-minute floor: shorter windows can't meaningfully charge.
    let Some(window) = cheapest_import_window(tariff, inputs.now_min, 30) else {
        return PlanRecommendation::NoPlan {
            reason: "no usable import window remaining today".to_string(),
        };
    };

    // Clamp the AC ask by what the window + rate can physically deliver.
    let window_hours = (window.end_min - window.start_min) as f64 / 60.0;
    let deliverable_ac = inputs.params.max_charge_kw * window_hours;
    let eta = inputs.params.charge_efficiency.clamp(0.01, 1.0);
    let ac_needed = stored_needed_kwh / eta;
    let kwh = ac_needed.min(deliverable_ac);

    // Recompute the projected end SOC with the charge applied
    // analytically: the grid kWh (× efficiency) lands as stored energy
    // on top of the no-intervention trajectory, capped at full.
    let projected_after =
        (projected_end + kwh * eta / capacity * 100.0).min(100.0);

    let rationale = format!(
        "Tomorrow's solar won't lift the battery to {:.0}% — the projection \
         ends at {:.0}%. Charging {:.1} kWh in the {:.1}p window ({}–{}) \
         covers the gap for about £{:.2}.",
        inputs.target_soc_pct,
        projected_end,
        kwh,
        window.rate * 100.0,
        hhmm(window.start_min),
        hhmm(window.end_min),
        kwh * window.rate,
    );

    PlanRecommendation::Charge {
        window,
        kwh,
        target_soc_pct: inputs.target_soc_pct,
        projected_end_soc_pct: projected_after,
        rationale,
    }
}

#[cfg(test)]
mod tests {
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
    fn simulate_tomorrow(solar: f64, cons: f64, p: &SimulationParams) -> SimulationOutput {
        let hours: Vec<SimHourInput> = (0..24)
            .map(|h| SimHourInput {
                timestamp: hour_ts(h, 1),
                solar_kwh: if (6..=19).contains(&h) { solar } else { 0.0 },
                consumption_kwh: cons,
            })
            .collect();
        simulate_battery(&hours, p)
    }

    fn plan_inputs<'a>(
        sim: &'a SimulationOutput,
        p: &'a SimulationParams,
        tariff: Option<&'a TariffConfig>,
    ) -> PlanInputs<'a> {
        PlanInputs {
            simulation: sim,
            params: p,
            import_tariff: tariff,
            target_soc_pct: 60.0,
            consumption_tomorrow_kwh: 12.0,
            consumption_sufficient: true,
            // 22:00 — well past solar, close enough to Flux's 02:00 that
            // the planner's "ready for tomorrow" branch is exercised.
            now_min: 22 * 60,
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

    #[test]
    fn sunny_day_needs_no_charge() {
        // 14 h × 1.5 kWh solar vs 24 h × 0.3 consumption: the 10 kWh
        // battery ends well above the 60% target from 30% start.
        let p = params();
        let sim = simulate_tomorrow(1.5, 0.3, &p);
        let flux = flux_tariff();
        match plan_overnight_charge(&plan_inputs(&sim, &p, Some(&flux))) {
            PlanRecommendation::NoChargeNeeded { projected_end_soc_pct } => {
                assert!(
                    projected_end_soc_pct > 60.0,
                    "sunny projection must exceed target: {projected_end_soc_pct}"
                );
            }
            other => panic!("expected NoChargeNeeded, got {other:?}"),
        }
    }

    #[test]
    fn cloudy_day_charges_in_the_cheap_window() {
        // Winter-ish: 14 h × 0.2 kWh solar vs 24 h × 0.5 consumption —
        // the battery drains; the planner should recommend tomorrow's
        // 02:00–05:00 off-peak.
        let p = params();
        let sim = simulate_tomorrow(0.2, 0.5, &p);
        let flux = flux_tariff();
        match plan_overnight_charge(&plan_inputs(&sim, &p, Some(&flux))) {
            PlanRecommendation::Charge {
                window,
                kwh,
                target_soc_pct,
                projected_end_soc_pct,
                ..
            } => {
                assert!(window.tomorrow);
                assert_eq!(window.start_min, 2 * 60);
                assert_eq!(window.end_min, 5 * 60);
                assert!((window.rate - 0.09).abs() < 1e-9);
                assert!(kwh > 0.0 && kwh <= 10.0 / 0.9);
                assert!((target_soc_pct - 60.0).abs() < 1e-9);
                assert!(projected_end_soc_pct >= 59.0);
            }
            other => panic!("expected Charge, got {other:?}"),
        }
    }

    #[test]
    fn charge_kwh_accounts_for_predicted_solar() {
        // Compare a sunny case (battery reaches target on its own) to a
        // cloudy case (needs a charge). The ask must be zero / smaller
        // in the sunny case — the planner is reading simulated SOC.
        let p = params();
        let flux = flux_tariff();
        let sim_sunny = simulate_tomorrow(1.5, 0.3, &p);
        let sim_cloudy = simulate_tomorrow(0.2, 0.5, &p);
        let kwh_sunny = match plan_overnight_charge(&plan_inputs(&sim_sunny, &p, Some(&flux))) {
            PlanRecommendation::NoChargeNeeded { .. } => 0.0,
            PlanRecommendation::Charge { kwh, .. } => kwh,
            other => panic!("unexpected {other:?}"),
        };
        let kwh_cloudy = match plan_overnight_charge(&plan_inputs(&sim_cloudy, &p, Some(&flux))) {
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
        let sim = simulate_tomorrow(0.0, 0.8, &p);
        let flux = flux_tariff();
        match plan_overnight_charge(&plan_inputs(&sim, &p, Some(&flux))) {
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
        let sim = simulate_tomorrow(0.2, 0.5, &p);
        match plan_overnight_charge(&plan_inputs(&sim, &p, None)) {
            PlanRecommendation::NoPlan { reason } => {
                assert!(reason.to_lowercase().contains("tariff"), "{reason}");
            }
            other => panic!("expected NoPlan, got {other:?}"),
        }
    }

    #[test]
    fn insufficient_history_yields_no_plan() {
        let p = params();
        let sim = simulate_tomorrow(0.2, 0.5, &p);
        let flux = flux_tariff();
        let mut inputs = plan_inputs(&sim, &p, Some(&flux));
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
        let mut inputs = plan_inputs(&empty, &p, Some(&flux));
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
        let sim = simulate_tomorrow(0.2, 0.5, &p);
        let flux = flux_tariff();
        if let PlanRecommendation::Charge { rationale, .. } =
            plan_overnight_charge(&plan_inputs(&sim, &p, Some(&flux)))
        {
            // The rate should always appear in the rationale regardless
            // of which window was picked.
            assert!(rationale.contains("9.0p") || rationale.contains("26.0p"));
            assert!(rationale.contains("02:00") || rationale.contains("05:00"));
        } else {
            panic!("expected Charge");
        }
    }
}
