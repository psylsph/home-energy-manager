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
#[derive(Debug, Clone, PartialEq)]
pub struct ChargeWindow {
    /// Start minute of day.
    pub start_min: u16,
    /// End minute of day (exclusive).
    pub end_min: u16,
    /// Window rate in £/kWh.
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
    // RED stub — Phase 2 implementation lands in the following commit.
    let _ = (tariff, now_min, min_duration_min);
    None
}

/// Compute the recommendation. See [`PlanRecommendation`] for the cases.
pub fn plan_overnight_charge(inputs: &PlanInputs) -> PlanRecommendation {
    // RED stub — Phase 2 implementation lands in the following commit.
    let _ = inputs;
    PlanRecommendation::NoPlan {
        reason: "not implemented".to_string(),
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
            now_min: 12 * 60,
        }
    }

    #[test]
    fn cheapest_window_picks_the_off_peak_slot() {
        let w = cheapest_import_window(&flux_tariff(), 12 * 60, 60).unwrap();
        assert_eq!(w.start_min, 2 * 60);
        assert_eq!(w.end_min, 5 * 60);
        assert!((w.rate - 0.09).abs() < 1e-9);
    }

    #[test]
    fn cheapest_window_skips_windows_already_passed() {
        // Planning at 04:00: only 60 min of the off-peak remain, which
        // still satisfies a 60-minute minimum.
        let w = cheapest_import_window(&flux_tariff(), 4 * 60, 60).unwrap();
        assert_eq!(w.start_min, 4 * 60);
        // At 04:30 nothing of tonight's off-peak remains (30 min < 60) —
        // the planner must not return a window in the past.
        let w = cheapest_import_window(&flux_tariff(), 4 * 60 + 30, 60);
        // The next cheapest forward window is tomorrow's day rate
        // (26p) — a flat fallback rather than a past off-peak.
        let w = w.expect("some window");
        assert!(w.start_min >= 4 * 60 + 30, "start={}", w.start_min);
    }
    #[test]
    fn cheapest_window_rejects_too_short_remainders() {
        let w = cheapest_import_window(&flux_tariff(), 4 * 60 + 30, 120);
        // 02:00–05:00 is 2.5 h but only 30 min remain; the 2-hour minimum
        // can't fit anywhere cheap — fallback to the cheapest day-rate
        // window that fits (05:00–16:00 at 26p).
        let w = w.expect("fallback window");
        assert!((w.rate - 0.26).abs() < 1e-9);
        assert!(w.end_min - w.start_min >= 120);
    }

    #[test]
    fn flat_tariff_has_no_exploitable_window() {
        let flat = tariff(&[("00:00", "23:59", 0.25)]);
        // A single all-day slot is returned as-is (charge anytime); the
        // planner's *recommendation* logic treats it as no cheap window.
        let w = cheapest_import_window(&flat, 0, 60).unwrap();
        assert_eq!(w.start_min, 0);
        assert_eq!(w.end_min, 23 * 60 + 59);
    }

    #[test]
    fn sunny_day_needs_no_charge() {
        // 14 h × 1.5 kWh solar vs 24 h × 0.3 consumption: the 10 kWh
        // battery ends full from 30%.
        let p = params();
        let sim = simulate_tomorrow(1.5, 0.3, &p);
        let flux = flux_tariff();
        match plan_overnight_charge(&plan_inputs(&sim, &p, Some(&flux))) {
            PlanRecommendation::NoChargeNeeded { projected_end_soc_pct } => {
                assert!(projected_end_soc_pct > 95.0);
            }
            other => panic!("expected NoChargeNeeded, got {other:?}"),
        }
    }

    #[test]
    fn cloudy_day_charges_in_the_cheap_window() {
        // Winter-ish: 14 h × 0.2 kWh solar vs 24 h × 0.5 consumption —
        // the battery drains well below target; the planner should
        // recommend a charge in the 02:00–05:00 off-peak.
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
                assert_eq!(window.start_min, 2 * 60);
                assert_eq!(window.end_min, 5 * 60);
                assert!((window.rate - 0.09).abs() < 1e-9);
                // Battery needs (60-?)% — with 10 kWh and 0.9 efficiency
                // the AC draw is the stored delta / 0.9.
                assert!(kwh > 0.0 && kwh <= 10.0 / 0.9);
                assert!((target_soc_pct - 60.0).abs() < 1e-9);
                assert!(projected_end_soc_pct >= 59.9);
            }
            other => panic!("expected Charge, got {other:?}"),
        }
    }

    #[test]
    fn charge_kwh_accounts_for_predicted_solar() {
        // Same cloudy scenario but with a modest solar contribution —
        // the planner must ask for LESS grid energy than the no-solar
        // case (solar during the window/day offsets the need).
        let p = params();
        let sim_low = simulate_tomorrow(0.05, 0.5, &p);
        let sim_high = simulate_tomorrow(0.5, 0.5, &p);
        let flux = flux_tariff();
        let kwh_low = match plan_overnight_charge(&plan_inputs(&sim_low, &p, Some(&flux))) {
            PlanRecommendation::Charge { kwh, .. } => kwh,
            other => panic!("expected Charge, got {other:?}"),
        };
        let kwh_high = match plan_overnight_charge(&plan_inputs(&sim_high, &p, Some(&flux))) {
            PlanRecommendation::Charge { kwh, .. } => kwh,
            other => panic!("expected Charge, got {other:?}"),
        };
        assert!(
            kwh_high < kwh_low,
            "more solar must reduce the grid ask: {kwh_high} !< {kwh_low}"
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
            assert!(rationale.contains("9.0p"), "rationale: {rationale}");
            assert!(rationale.contains("02:00"), "rationale: {rationale}");
        } else {
            panic!("expected Charge");
        }
    }
}
