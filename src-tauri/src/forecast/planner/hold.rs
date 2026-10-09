//! Hold-through-window charge strategy (issue #359).

use super::{plan_overnight_charge, ChargeWindow, PlanInputs, PlanRecommendation};
use crate::forecast::simulate::{SimHourInput, SimHourResult, SimulationParams};

/// Outcome of simulating a hold slot with a given target.
#[derive(Debug, Clone)]
pub(super) struct HoldOutcome {
    pub(super) series: Vec<SimHourResult>,
}

impl HoldOutcome {
    pub(super) fn holds(
        &self,
        _sim_hours: &[SimHourInput],
        _params: &SimulationParams,
        _floor: f64,
    ) -> bool {
        false
    }
}

/// Simulate the selected window occurrence as a hold slot with `target_pct`.
pub(super) fn simulate_hold(
    _sim_hours: &[SimHourInput],
    _params: &SimulationParams,
    _window: &ChargeWindow,
    _now_ts: i64,
    _target_pct: u8,
) -> Option<HoldOutcome> {
    None
}

/// Compute the hold-through-window recommendation.
pub fn plan_hold_through_window(inputs: &PlanInputs) -> PlanRecommendation {
    plan_overnight_charge(inputs)
}
