//! Spread the planned charge across the cheap window (issue #483), advice
//! only for now.
//!
//! Every strategy charges at the inverter's full power from the start of
//! the cheap window. On a long window that is a hard hour and then nothing.
//! This works out the lower rate that would deliver the same charge and
//! finish about 90% of the way through the window, or 80% for a charge
//! above 90% ([`spread_finish_fraction`]),
//! as a slot spanning the whole window with a target SOC so it can't
//! overshoot. The plan itself is untouched: the result is shown beside it
//! so the numbers can be checked on real nights before HEM writes a lower
//! limit (the limit also caps solar charging, so writing it means owning
//! and restoring it, a later step).
//!
//! Invariants:
//! - the plan, its Apply payload and its rationale are exactly what they
//!   are with the setting off; this only adds advice;
//! - a spread reaches the same target as the full-power charge inside the
//!   window, and its trough, simulated with the lower limit inside the
//!   window (solar included: the one limit caps both), never falls below
//!   that full-power charge's, nor under a minimum the plan holds;
//!   otherwise the advice is full rate;
//! - the rate is one the inverter's limit register can hold, rounded up,
//!   so the written value would deliver at least the rate the maths asked
//!   for, and never the register's top value, which limits nothing.

use super::hold::{prepare_hold, HoldContext, HoldOutcome, MIN_TARGET_PCT};
use super::{window_duration_minutes, ChargeWindow, PlanInputs, PlanRecommendation};

/// Shortest cheap window worth spreading, minutes. A backstop: the
/// worthwhile test decides most nights.
pub const SPREAD_MIN_WINDOW_MIN: u16 = 120;
/// Where in the window a spread charge to above [`SPREAD_NEAR_FULL_PCT`]
/// aims to finish. The 20% left over is the margin for charging slowing as
/// the battery nears full.
pub const SPREAD_FINISH_FRACTION: f64 = 0.8;
/// Where a charge to [`SPREAD_NEAR_FULL_PCT`] or lower aims to finish: it
/// never reaches the part where charging slows, so only a little is kept
/// back for a cold battery or a wrong forecast (issue #483).
pub const SPREAD_FINISH_FRACTION_BELOW_FULL: f64 = 0.8;
/// Targets above this charge into the part where the battery slows down.
pub const SPREAD_NEAR_FULL_PCT: u8 = 90;

/// How far through the window a spread charge to `target_pct` aims to finish.
pub fn spread_finish_fraction(_target_pct: u8) -> f64 {
    SPREAD_FINISH_FRACTION
}
/// Skip the spread when a full-power charge already fills this share of the
/// window: slowing it would barely change anything (peterb99's test).
pub const SPREAD_WORTHWHILE_FRACTION: f64 = 0.65;
/// Lowest spread rate, kW, before [`SPREAD_FLOOR_FRACTION`] of the maximum
/// is considered: very low rates lose more to the inverter's own overhead.
pub const SPREAD_FLOOR_KW: f64 = 0.5;
/// Lowest spread rate as a share of the inverter's maximum.
pub const SPREAD_FLOOR_FRACTION: f64 = 0.2;
/// Slack on energy and SOC comparisons, so float noise never flips a
/// decision.
const KWH_TOLERANCE: f64 = 0.01;
const SOC_TOLERANCE_PCT: f64 = 0.05;

/// What the Planner would do with the charge limit if it spread the charge.
#[derive(Debug, Clone, PartialEq)]
pub enum SpreadAdvice {
    /// Charge at `rate_kw` across the whole window to `target_soc_pct`.
    Spread {
        /// The limit to write, % of the inverter's maximum battery power.
        rate_percent: u16,
        /// The power that limit allows, kW.
        rate_kw: f64,
        /// The slot's target SOC, %.
        target_soc_pct: u8,
        /// AC kWh the spread charge draws from the grid into the battery;
        /// sun in the window adds to it.
        kwh: f64,
        /// When the charge should reach its target, local minute of day.
        finish_min: u16,
        /// When the plan's full-power charge finishes, local minute of day.
        full_rate_finish_min: u16,
        /// How long full power takes to reach the same target, sun
        /// included, minutes.
        full_rate_minutes: u16,
        /// The cheap window's length, minutes.
        window_minutes: u16,
        /// The lowest SOC before the next cheap period with the spread, %.
        after_min_soc_pct: f64,
    },
    /// Keep today's full-power charge, and why.
    FullRate(SpreadSkip),
}

/// Why the advice is to charge at full power.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpreadSkip {
    /// The cheap window is shorter than [`SPREAD_MIN_WINDOW_MIN`].
    WindowTooShort { window_minutes: u16 },
    /// Full power already fills [`SPREAD_WORTHWHILE_FRACTION`] or more of
    /// the window.
    NotWorthwhile {
        full_rate_minutes: u16,
        window_minutes: u16,
    },
    /// Finishing by [`spread_finish_fraction`] of the window would need
    /// the full rate anyway: the register's top value, or the inverter's
    /// maximum.
    NeedsFullRate,
    /// The limit register needs the pack size to convert a partial rate,
    /// and it isn't known.
    NeedsPackSize,
    /// Adaptive Charge sets the charge limit, so the Planner leaves it.
    AdaptiveCharge,
    /// No live snapshot, or no charge rating in it, to size the limit
    /// register's steps from.
    NoChargeRating,
    /// At the lower rate the battery wouldn't reach the charge or would
    /// dip lower before the next cheap period (solar capped in the window,
    /// say), so full power stays.
    WorseTrough,
}

/// The spread advice for `plan`, or `None` when there's nothing to spread:
/// no charge planned, a window already under way (a replan), or no hourly
/// forecast to simulate it with. `rate_at(percent)` is the limit a request
/// for `percent` (1–100) of the inverter's maximum writes on this inverter:
/// the percent the Control page reads it back as and the power it allows,
/// kW. `None` when it can't be converted without the pack size.
pub fn spread_charge_advice(
    inputs: &PlanInputs,
    plan: &PlanRecommendation,
    rate_at: impl Fn(u16) -> Option<(u16, f64)>,
) -> Option<SpreadAdvice> {
    if inputs.running_window_start.is_some() {
        return None;
    }
    let max_kw = inputs.params.max_charge_kw;
    if max_kw <= 0.0 {
        return None;
    }
    let (kwh, hold_target, plan_trough) = match plan {
        PlanRecommendation::Charge {
            kwh,
            after_min_soc_pct,
            ..
        } => (*kwh, None, *after_min_soc_pct),
        PlanRecommendation::ChargeAndHold {
            kwh,
            target_soc_pct,
            after_min_soc_pct,
            ..
        } => (*kwh, Some(*target_soc_pct), *after_min_soc_pct),
        PlanRecommendation::NoChargeNeeded { .. } | PlanRecommendation::NoPlan { .. } => {
            return None
        }
    };
    if kwh <= KWH_TOLERANCE {
        return None;
    }
    let ctx = prepare_hold(inputs)?;
    let window_minutes = window_length(&ctx.window);
    if window_minutes < SPREAD_MIN_WINDOW_MIN {
        return Some(SpreadAdvice::FullRate(SpreadSkip::WindowTooShort {
            window_minutes,
        }));
    }
    // The target, and the full-power hold to it: what the spread has to
    // match. For a hold plan the target is the plan's own; for the
    // minimum-SOC plan the rounded-up target can add a little.
    let worse = Some(SpreadAdvice::FullRate(SpreadSkip::WorseTrough));
    let full = match hold_target {
        Some(target) => ctx.simulate(inputs.params, target).map(|o| (target, o)),
        None => Some(target_for_energy(&ctx, inputs, kwh)),
    };
    let Some((target, full)) = full else {
        return worse;
    };
    // How long full power takes to reach it, simulated with the sun: the
    // same charge the spread is timed to. A target full power never
    // reaches fills the whole window.
    let full_minutes_to_target = full
        .target_reached_min
        .map_or(f64::from(window_minutes), |m| {
            m.min(f64::from(window_minutes))
        });
    let full_rate_minutes = full_minutes_to_target.ceil() as u16;
    if !worth_spreading(full_rate_minutes, window_minutes) {
        return Some(SpreadAdvice::FullRate(SpreadSkip::NotWorthwhile {
            full_rate_minutes,
            window_minutes,
        }));
    }

    // At full power the battery takes the maximum, sun and grid together,
    // until it reaches the target: that energy has to fit in the first part
    // of the window at the lower limit, which caps the sun's share too.
    let available_h = f64::from(window_minutes) * spread_finish_fraction(target) / 60.0;
    let floor_kw = SPREAD_FLOOR_KW.max(SPREAD_FLOOR_FRACTION * max_kw);
    let wanted_kw = (max_kw * full_minutes_to_target / 60.0 / available_h).max(floor_kw);
    let (rate_percent, rate_kw) = match register_rate(wanted_kw, max_kw, &rate_at) {
        RegisterRate::Rate(percent, kw) => (percent, kw),
        RegisterRate::AtMaximum => return Some(SpreadAdvice::FullRate(SpreadSkip::NeedsFullRate)),
        RegisterRate::NeedsPackSize => {
            return Some(SpreadAdvice::FullRate(SpreadSkip::NeedsPackSize))
        }
    };

    let Some(outcome) = ctx.simulate_at_rate(inputs.params, target, rate_kw) else {
        return worse;
    };
    // The same target, and a trough no lower than full power's to it. In
    // sun the slower charge spends sun the battery would otherwise have
    // stored on top of the target, which the trough catches. A plan that
    // holds the minimum must still hold it, judged as the planner does.
    let floor = inputs.target_soc_pct - SOC_TOLERANCE_PCT;
    let trough_ok = outcome.trough_pct() >= full.trough_pct() - SOC_TOLERANCE_PCT
        && (plan_trough < floor || outcome.holds(ctx.sim_hours, inputs.params, floor));
    let Some(minutes_to_target) = outcome.target_reached_min.filter(|_| trough_ok) else {
        return worse;
    };
    let charge_minutes = (minutes_to_target.ceil() as u16).min(window_minutes);
    let finish_min = (ctx.window.start_min + charge_minutes) % 1440;
    // What full power would show: the minimum-SOC plan's own slot, which is
    // what Apply writes and the card's Time shows; a hold's time to target.
    let full_rate_finish_min = match plan {
        PlanRecommendation::Charge { window, .. } => window.end_min,
        _ => (ctx.window.start_min + full_rate_minutes) % 1440,
    };
    Some(SpreadAdvice::Spread {
        rate_percent,
        rate_kw,
        target_soc_pct: target,
        kwh: outcome.charge_kwh,
        finish_min,
        full_rate_finish_min,
        full_rate_minutes,
        window_minutes,
        after_min_soc_pct: outcome.trough_pct(),
    })
}

/// The window's length, minutes, with a 23:59 end read as midnight: a
/// tariff can't say 24:00, so Cosy's 22:00–00:00 is stored as 22:00–23:59.
fn window_length(window: &ChargeWindow) -> u16 {
    window_duration_minutes(window) + u16::from(window.end_min == 1439)
}

/// peterb99's test: spreading is worth it only while a full-power charge
/// fills less than [`SPREAD_WORTHWHILE_FRACTION`] of the window.
fn worth_spreading(full_rate_minutes: u16, window_minutes: u16) -> bool {
    f64::from(full_rate_minutes) < f64::from(window_minutes) * SPREAD_WORTHWHILE_FRACTION
}

enum RegisterRate {
    /// The lowest limit allowing at least the wanted rate, as the Control
    /// page shows it (% of the maximum), and the power it allows, kW.
    Rate(u16, f64),
    /// Only the full rate allows the wanted rate: the inverter's maximum,
    /// or the register's top value, which is what 100% writes.
    AtMaximum,
    NeedsPackSize,
}

/// Round `wanted_kw` up to a value the limit register can hold.
fn register_rate(
    wanted_kw: f64,
    max_kw: f64,
    rate_at: &impl Fn(u16) -> Option<(u16, f64)>,
) -> RegisterRate {
    // A half-scale register tops out at 0.5C, below the rating of an
    // inverter on a small pack.
    let full_kw = rate_at(100).map_or(max_kw, |(_, top)| top.min(max_kw));
    if wanted_kw >= full_kw - 1e-9 {
        return RegisterRate::AtMaximum;
    }
    for percent in 1..100u16 {
        let Some((shown, kw)) = rate_at(percent) else {
            return RegisterRate::NeedsPackSize;
        };
        if kw >= full_kw - 1e-9 {
            return RegisterRate::AtMaximum;
        }
        if kw + 1e-9 >= wanted_kw {
            return RegisterRate::Rate(shown, kw);
        }
    }
    RegisterRate::AtMaximum
}

/// The lowest whole-percent target whose full-power hold draws at least
/// `kwh` into the battery, and that hold: the minimum-SOC plan's charge as
/// a target, so the slot can span the whole window without overshooting.
/// 100 when even a full charge draws less.
fn target_for_energy(ctx: &HoldContext, inputs: &PlanInputs, kwh: f64) -> (u8, HoldOutcome) {
    let delivers = |target: u8| {
        ctx.simulate(inputs.params, target)
            .filter(|o| o.charge_kwh >= kwh - KWH_TOLERANCE)
    };
    let full = || (100, ctx.full.clone());
    if ctx.full.charge_kwh < kwh - KWH_TOLERANCE {
        return full();
    }
    let mut lo = ctx.lowest.max(MIN_TARGET_PCT);
    if let Some(outcome) = delivers(lo) {
        return (lo, outcome);
    }
    // `lo` fails and 100 delivers; delivery is monotonic in the target.
    let mut hi = full();
    while hi.0 - lo > 1 {
        let mid = lo + (hi.0 - lo) / 2;
        match delivers(mid) {
            Some(outcome) => hi = (mid, outcome),
            None => lo = mid,
        }
    }
    hi
}

#[cfg(test)]
#[path = "spread_tests.rs"]
mod tests;
