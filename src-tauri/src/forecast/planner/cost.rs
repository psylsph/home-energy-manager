//! Cheapest-overall charge strategy (issue #359).

use super::hold::{plan_hold_through_window, HoldContext, HoldOutcome};
use super::{PlanInputs, PlanRecommendation};
use crate::forecast::simulate::{SimHourInput, SimulationParams};
use crate::settings::TariffConfig;

/// What the cheapest strategy needs beyond the standard planner inputs.
#[derive(Debug, Clone, Copy, Default)]
pub struct CostInputs<'a> {
    pub export_tariff: Option<&'a TariffConfig>,
    pub low_solar_hours: Option<&'a [SimHourInput]>,
}

pub(super) fn cycle_cost(
    _ctx: &HoldContext,
    _outcome: &HoldOutcome,
    _params: &SimulationParams,
    _import_tariff: &TariffConfig,
    _export_tariff: Option<&TariffConfig>,
) -> f64 {
    0.0
}

/// Compute the cheapest-overall recommendation (stub: the hold plan).
pub fn plan_cheapest_overall(inputs: &PlanInputs, _extras: &CostInputs) -> PlanRecommendation {
    plan_hold_through_window(inputs)
}
