//! Forecast plan automation for the poll loop (issue #283).
//!
//! The plan is sized for ONE charge cycle, but the inverter treats an applied
//! charge slot as a nightly recurring schedule. Two machine writers keep it
//! honest:
//!
//! - **Auto-refresh**: shortly before each cheap period, re-compute the plan
//!   from the live SOC and forecast, then rewrite - or clear - charge slot 1 so
//!   the inverter never repeats a stale duration on later nights.
//! - **Auto-apply** (user-configured lead time): the same, exactly as the
//!   Forecast page's Apply button would, plus a notification. While enabled it
//!   supersedes auto-refresh, so charge slot 1 gets exactly one machine write
//!   per day.
//!
//! Rate-limit contract (CODE_REVIEW.md Major 2): the plan's duration maths
//! assumes the inverter charges at its hardware maximum, so every write batch
//! re-writes the charge-limit register (via `PLAN_CHARGE_RATE_PERCENT`). A
//! manual Control-page rate change or an automation that lowers the limit after
//! a write still wins until the next one re-asserts it.
//!
//! The due-checks live in [`crate::forecast::refresh`]. This module holds what
//! the poll loop used to inline around them: turning a recommendation into
//! register writes and a notification (both pure), and the orchestration.
//! Every entry point takes `now` so the clock can be pinned.

use std::sync::Arc;

use chrono::{DateTime, Local, NaiveDate, Timelike};

use crate::forecast::planner::PlanRecommendation;
use crate::forecast::refresh::{
    plan_auto_apply_adaptive_warning_due, plan_auto_apply_decision_with_adaptive,
    plan_refresh_action, plan_refresh_due, plan_refresh_due_with_adaptive, PlanApplyDecision,
    PlanRefreshAction, PLAN_CHARGE_RATE_PERCENT, SLOT_TARGET_SOC_NONE,
};
use crate::inverter::encoder::RegisterWrite;
use crate::inverter::model::InverterSnapshot;
use crate::inverter::poll::AppState;
use crate::settings::Settings;

/// Which automation is acting on a recommendation: decides the log wording and
/// whether the user is notified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanTrigger {
    /// The fixed nightly auto-refresh. Logs only.
    Refresh,
    /// The user-configured auto-apply. Also notifies.
    AutoApply,
}

impl PlanTrigger {
    fn label(self) -> &'static str {
        match self {
            Self::Refresh => "Forecast plan refresh",
            Self::AutoApply => "Forecast plan auto-apply",
        }
    }
}

/// What a recommendation means for charge slot 1.
#[derive(Debug, Clone)]
pub(crate) enum SlotPlan {
    /// Write the window and arm Timed Charge.
    Write {
        start_hhmm: u16,
        end_hhmm: u16,
        kwh: f64,
        /// The window starts tomorrow rather than tonight.
        tomorrow: bool,
        writes: Result<Vec<RegisterWrite>, String>,
    },
    /// The plan needs no charge: clear the slot so a previously applied one
    /// cannot keep charging nightly.
    Clear {
        writes: Result<Vec<RegisterWrite>, String>,
    },
    /// No usable plan this cycle.
    NoPlan { reason: String },
    /// Write the whole cheap window with a target SOC the inverter charges
    /// to and then holds (issue #359), and arm Timed Charge.
    WriteHold {
        start_hhmm: u16,
        end_hhmm: u16,
        target_soc: u8,
        kwh: f64,
        /// The window starts tomorrow rather than tonight.
        tomorrow: bool,
        writes: Result<Vec<RegisterWrite>, String>,
    },
}

/// AC kWh a recommendation asks to charge (0 when it asks for none).
pub(crate) fn rec_kwh(rec: &PlanRecommendation) -> f64 {
    match rec {
        PlanRecommendation::Charge { kwh, .. } => *kwh,
        PlanRecommendation::ChargeAndHold { kwh, .. } => *kwh,
        PlanRecommendation::NoChargeNeeded { .. } | PlanRecommendation::NoPlan { .. } => 0.0,
    }
}

/// [`plan_slot_plan`] for a planner that may have armed `planner_armed`
/// with an earlier hold plan (issue #359): a full charge, a full hold or a
/// cleared slot also disarms that target, if the inverter still reports it.
pub(crate) fn plan_slot_plan_for(
    rec: &PlanRecommendation,
    snapshot: &InverterSnapshot,
    planner_armed: Option<u8>,
) -> SlotPlan {
    let reset = |writes: Result<Vec<RegisterWrite>, String>| {
        writes.map(|mut writes| {
            crate::server::api::extend_unwritten(
                &mut writes,
                crate::server::api::planner_target_reset(
                    snapshot.device_type,
                    snapshot.target_soc,
                    planner_armed,
                ),
            );
            writes
        })
    };
    match plan_slot_plan(rec, snapshot) {
        SlotPlan::Write {
            start_hhmm,
            end_hhmm,
            kwh,
            tomorrow,
            writes,
        } => SlotPlan::Write {
            start_hhmm,
            end_hhmm,
            kwh,
            tomorrow,
            writes: reset(writes),
        },
        SlotPlan::Clear { writes } => SlotPlan::Clear {
            writes: reset(writes),
        },
        SlotPlan::WriteHold {
            start_hhmm,
            end_hhmm,
            target_soc,
            kwh,
            tomorrow,
            writes,
        } if target_soc >= 100 => SlotPlan::WriteHold {
            start_hhmm,
            end_hhmm,
            target_soc,
            kwh,
            tomorrow,
            writes: reset(writes),
        },
        plan => plan,
    }
}

/// The planner's armed target after queuing `plan`: a hold below 100 arms
/// its target, any other successful write leaves the planner owning none,
/// and a plan that wrote nothing changes nothing.
fn armed_target_after(plan: &SlotPlan) -> Option<Option<u8>> {
    match plan {
        SlotPlan::WriteHold {
            target_soc,
            writes: Ok(_),
            ..
        } => Some((*target_soc < 100).then_some(*target_soc)),
        SlotPlan::Write { writes: Ok(_), .. } | SlotPlan::Clear { writes: Ok(_) } => Some(None),
        SlotPlan::Write { writes: Err(_), .. }
        | SlotPlan::WriteHold { writes: Err(_), .. }
        | SlotPlan::Clear { writes: Err(_) }
        | SlotPlan::NoPlan { .. } => None,
    }
}

/// Translate a recommendation into the writes for charge slot 1 on this
/// inverter. Pure.
pub(crate) fn plan_slot_plan(rec: &PlanRecommendation, snapshot: &InverterSnapshot) -> SlotPlan {
    match plan_refresh_action(rec) {
        PlanRefreshAction::WriteSlot {
            start_hhmm,
            end_hhmm,
        } => SlotPlan::Write {
            start_hhmm,
            end_hhmm,
            kwh: rec_kwh(rec),
            tomorrow: matches!(
                rec,
                PlanRecommendation::Charge { window, .. } if window.tomorrow
            ),
            writes: crate::server::api::build_charge_slot_writes(
                snapshot.device_type,
                1,
                true,
                start_hhmm,
                end_hhmm,
                SLOT_TARGET_SOC_NONE,
                Some(
                    crate::inverter::power_limit::ChargeRateRequest::for_snapshot(
                        PLAN_CHARGE_RATE_PERCENT,
                        snapshot,
                    ),
                ),
            ),
        },
        PlanRefreshAction::ClearSlot => SlotPlan::Clear {
            writes: crate::server::api::build_charge_slot_writes(
                snapshot.device_type,
                1,
                false,
                0,
                0,
                SLOT_TARGET_SOC_NONE,
                None,
            ),
        },
        PlanRefreshAction::WriteSlotWithTarget {
            start_hhmm,
            end_hhmm,
            target_soc,
        } => SlotPlan::WriteHold {
            start_hhmm,
            end_hhmm,
            target_soc,
            kwh: rec_kwh(rec),
            tomorrow: matches!(
                rec,
                PlanRecommendation::ChargeAndHold { window, .. } if window.tomorrow
            ),
            // The rate is still re-asserted at the maximum: the target, not
            // the rate, decides where charging stops.
            writes: crate::server::api::build_charge_slot_writes(
                snapshot.device_type,
                1,
                true,
                start_hhmm,
                end_hhmm,
                target_soc,
                Some(
                    crate::inverter::power_limit::ChargeRateRequest::for_snapshot(
                        PLAN_CHARGE_RATE_PERCENT,
                        snapshot,
                    ),
                ),
            ),
        },
        PlanRefreshAction::None => SlotPlan::NoPlan {
            reason: match rec {
                PlanRecommendation::NoPlan { reason } => reason.clone(),
                _ => "no plan available".to_string(),
            },
        },
    }
}

/// The message to send the user for this outcome. Only the auto-apply
/// notifies; the quiet nightly refresh never does. Pure.
pub(crate) fn plan_notification(plan: &SlotPlan, trigger: PlanTrigger) -> Option<String> {
    if trigger != PlanTrigger::AutoApply {
        return None;
    }
    use crate::alerts::{
        build_plan_applied_hold_message, build_plan_applied_message, build_plan_cleared_message,
        build_plan_unavailable_message,
    };
    Some(match plan {
        SlotPlan::Write {
            writes: Ok(_),
            start_hhmm,
            end_hhmm,
            kwh,
            tomorrow,
        } => build_plan_applied_message(*start_hhmm, *end_hhmm, *kwh, *tomorrow),
        SlotPlan::Write { writes: Err(_), .. } => {
            build_plan_unavailable_message("the charge slot could not be encoded for this inverter")
        }
        SlotPlan::Clear { writes: Ok(_) } => build_plan_cleared_message(),
        SlotPlan::Clear { writes: Err(_) } => {
            build_plan_unavailable_message("the charge slot could not be cleared for this inverter")
        }
        SlotPlan::WriteHold {
            writes: Ok(_),
            start_hhmm,
            end_hhmm,
            target_soc,
            kwh,
            tomorrow,
        } => build_plan_applied_hold_message(*start_hhmm, *end_hhmm, *target_soc, *kwh, *tomorrow),
        SlotPlan::WriteHold { writes: Err(_), .. } => {
            build_plan_unavailable_message("the charge slot could not be encoded for this inverter")
        }
        SlotPlan::NoPlan { reason } => build_plan_unavailable_message(reason),
    })
}

/// True the first time it is called for `today`, then false until the date
/// changes: the once-per-day gate for "Adaptive Charge owns the rate" warnings.
pub(crate) fn first_report_today(last: &mut Option<NaiveDate>, today: NaiveDate) -> bool {
    if *last == Some(today) {
        return false;
    }
    *last = Some(today);
    true
}

/// Queue the writes for a recommendation (through the shared write pump, so
/// the ~1.5 s Modbus round-trips drain off the read path and the loop keeps
/// broadcasting snapshots) and notify the user if the trigger calls for it.
pub(crate) async fn apply_plan_recommendation(
    state: &Arc<AppState>,
    snapshot: &InverterSnapshot,
    rec: &PlanRecommendation,
    trigger: PlanTrigger,
) {
    let planner_armed = Settings::load_async().await.forecast_plan_armed_target_pct;
    let plan = plan_slot_plan_for(rec, snapshot, planner_armed);
    let label = trigger.label();
    match &plan {
        SlotPlan::Write {
            start_hhmm,
            end_hhmm,
            kwh,
            writes: Ok(writes),
            ..
        } => {
            match trigger {
                PlanTrigger::Refresh => tracing::info!(
                    start = start_hhmm,
                    end = end_hhmm,
                    kwh = format!("{kwh:.2}"),
                    "{label}: rewriting charge slot 1 for tonight's cheap period"
                ),
                PlanTrigger::AutoApply => tracing::info!(
                    start = start_hhmm,
                    end = end_hhmm,
                    kwh = format!("{kwh:.2}"),
                    "{label}: writing charge slot 1 for the cheap tariff window"
                ),
            }
            crate::server::api::queue_writes(state, writes.clone()).await;
        }
        SlotPlan::Write { writes: Err(e), .. } => {
            tracing::warn!("{label}: could not encode slot writes: {e}");
        }
        SlotPlan::WriteHold {
            start_hhmm,
            end_hhmm,
            target_soc,
            kwh,
            writes: Ok(writes),
            ..
        } => {
            tracing::info!(
                start = start_hhmm,
                end = end_hhmm,
                target_soc,
                kwh = format!("{kwh:.2}"),
                "{label}: writing charge slot 1 to charge to its target and hold through the cheap window"
            );
            crate::server::api::queue_writes(state, writes.clone()).await;
        }
        SlotPlan::WriteHold { writes: Err(e), .. } => {
            tracing::warn!("{label}: could not encode slot writes: {e}");
        }
        SlotPlan::Clear { writes: Ok(writes) } => {
            tracing::info!("{label}: fresh plan needs no charge — clearing charge slot 1");
            crate::server::api::queue_writes(state, writes.clone()).await;
        }
        SlotPlan::Clear { writes: Err(e) } => {
            tracing::warn!("{label}: could not encode slot clear: {e}");
        }
        SlotPlan::NoPlan { reason } => match trigger {
            PlanTrigger::Refresh => {
                tracing::debug!("{label}: no plan available this cycle");
            }
            PlanTrigger::AutoApply => {
                tracing::debug!(
                    reason = reason.as_str(),
                    "{label}: no plan available this cycle"
                );
            }
        },
    }
    if let Some(next) = armed_target_after(&plan) {
        if next != planner_armed {
            if let Err(error) = Settings::update_async(move |settings| {
                settings.forecast_plan_armed_target_pct = next;
            })
            .await
            {
                tracing::warn!("{label}: could not record the planner's charge target: {error}");
            }
        }
    }
    if let Some(text) = plan_notification(&plan, trigger) {
        crate::alerts::send_plan_notification(state, &text).await;
    }
}

/// Compute a fresh recommendation from the live snapshot. SQLite reads and a
/// 72 h simulation run on a blocking thread, off the poll task.
///
/// `settings` is a shallow clone: the planner only reads a read-only subset
/// (import tariff, forecast_*, weather coordinates), so nested fields such as
/// `timed_export_schedule` are never touched. Keep this in mind if new fields
/// are added to the planner's inputs.
async fn compute_plan(
    state: &Arc<AppState>,
    settings: &Settings,
    snapshot: &InverterSnapshot,
    now: DateTime<Local>,
) -> Result<ComputedPlan, tokio::task::JoinError> {
    let (weather_enabled, coords) = {
        let ws = state.weather.lock().await;
        (
            ws.config.enabled,
            ws.config.latitude.zip(ws.config.longitude),
        )
    };
    let history = state.history.lock().await.clone();
    let live_snapshot = snapshot.clone();
    let settings = settings.clone();
    tokio::task::spawn_blocking(move || {
        let forecast = crate::forecast::build_forecast_payload(&crate::forecast::ForecastInputs {
            db: history.as_deref(),
            settings: &settings,
            snapshot: Some(&live_snapshot),
            weather_enabled,
            weather_coords: coords,
            now,
        });
        let (rec, export) = crate::server::api::compute_full_plan(
            &forecast,
            &settings,
            Some(&live_snapshot),
            now.timestamp(),
        );
        ComputedPlan {
            rec,
            export,
            forecast,
        }
    })
    .await
}

/// A freshly computed plan with the export advice and forecast behind it.
struct ComputedPlan {
    rec: PlanRecommendation,
    export: Option<crate::forecast::planner::ExportAdvice>,
    forecast: crate::forecast::ForecastPayload,
}

/// Store a Planner decision and the forecast behind it in the plan history
/// (issue #361), pruning decisions past the retention period. Failures are
/// logged and never affect the plan itself.
async fn record_plan_snapshot(
    state: &Arc<AppState>,
    settings: &Settings,
    kind: &'static str,
    plan: &ComputedPlan,
    now: DateTime<Local>,
) {
    let Some(db) = state.history.lock().await.clone() else {
        return;
    };
    let forecast_json = match serde_json::to_string(&plan.forecast) {
        Ok(json) => json,
        Err(error) => {
            tracing::warn!("Plan history: could not serialise the forecast: {error}");
            return;
        }
    };
    let row = crate::history::PlanSnapshotRow {
        recorded_at: now.timestamp(),
        kind: kind.to_string(),
        strategy: serde_json::to_value(settings.forecast_plan_strategy)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_default(),
        // Exactly what GET /api/forecast/plan served: the recommendation,
        // its Apply payload and the export advice.
        plan_json: crate::server::api::plan_response_json(&plan.rec, plan.export.as_ref())
            .to_string(),
        forecast_json,
    };
    let cutoff = now.timestamp() - crate::history::PLAN_SNAPSHOT_RETENTION_DAYS * 24 * 3600;
    let stored = tokio::task::spawn_blocking(move || {
        db.insert_plan_snapshot(&row)?;
        db.prune_plan_snapshots(cutoff)
    })
    .await;
    match stored {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => tracing::warn!("Plan history: could not store the decision: {error}"),
        Err(error) => tracing::warn!("Plan history: store task failed: {error}"),
    }
}

/// Whether Adaptive Charge currently owns the charge-limit register (the plan
/// writes it too, so the two must not fight).
fn adaptive_owns_rate(settings: &Settings) -> bool {
    settings.adaptive_charge_enabled || settings.adaptive_charge_saved_limit.is_some()
}

/// The fixed nightly auto-refresh. Does nothing unless it is enabled and the
/// auto-apply trigger is not (that supersedes it).
pub(crate) async fn run_plan_refresh(
    state: &Arc<AppState>,
    settings: &Settings,
    snapshot: &InverterSnapshot,
    now: DateTime<Local>,
) {
    if !settings.forecast_plan_auto_refresh || settings.forecast_plan_auto_apply_enabled {
        return;
    }
    let adaptive_owns_rate = adaptive_owns_rate(settings);
    let last = *state.forecast_plan_refresh_date.lock().await;
    let tariff = settings.import_tariff_config.as_ref();
    if plan_refresh_due_with_adaptive(now, last, tariff, adaptive_owns_rate) {
        let planned = compute_plan(state, settings, snapshot, now).await;
        // Mark today done regardless of the outcome shape so a persistent
        // failure can't turn into a per-poll write storm.
        *state.forecast_plan_refresh_date.lock().await = Some(now.date_naive());
        match planned {
            Ok(plan) => {
                record_plan_snapshot(state, settings, "refresh", &plan, now).await;
                apply_plan_recommendation(state, snapshot, &plan.rec, PlanTrigger::Refresh).await
            }
            Err(e) => tracing::warn!("Forecast plan refresh failed: {e}"),
        }
    } else if adaptive_owns_rate && plan_refresh_due(now, last, tariff) {
        // The refresh would be due but Adaptive Charge owns the charge-limit
        // register: warn once per day (tracked separately so disabling Adaptive
        // later in the same lead window still lets the refresh fire) instead
        // of silently skipping on every poll (CODE_REVIEW.md Major 3).
        let mut warned = state.forecast_plan_refresh_warned.lock().await;
        if first_report_today(&mut warned, now.date_naive()) {
            tracing::warn!(
                "Forecast plan auto-refresh is enabled but Adaptive Charge owns the charge rate — skipping tonight's slot rewrite"
            );
        }
    }
}

/// The user-configured auto-apply. Does nothing unless it is enabled.
pub(crate) async fn run_plan_auto_apply(
    state: &Arc<AppState>,
    settings: &Settings,
    snapshot: &InverterSnapshot,
    now: DateTime<Local>,
) {
    if !settings.forecast_plan_auto_apply_enabled {
        return;
    }
    let adaptive_owns_rate = adaptive_owns_rate(settings);
    let last = *state.forecast_plan_apply_date.lock().await;
    let tariff = settings.import_tariff_config.as_ref();
    let lead = settings.forecast_plan_auto_apply_lead_minutes;
    match plan_auto_apply_decision_with_adaptive(now, last, tariff, lead, adaptive_owns_rate) {
        PlanApplyDecision::NotDue { reason } => {
            // Would-be-due but Adaptive Charge owns the charge-limit register:
            // warn + notify once per day instead of on every poll. Gated on the
            // apply actually being due, or an Adaptive user with no cheap window
            // near would get a false alarm every single day.
            if adaptive_owns_rate && plan_auto_apply_adaptive_warning_due(now, last, tariff, lead) {
                let first = {
                    let mut warned = state.forecast_plan_apply_warned.lock().await;
                    first_report_today(&mut warned, now.date_naive())
                };
                if first {
                    tracing::warn!(
                        "Forecast plan auto-apply is enabled but Adaptive Charge owns the charge rate — skipping tonight's apply"
                    );
                    crate::alerts::send_plan_notification(
                        state,
                        &crate::alerts::build_plan_unavailable_message(
                            "Adaptive Charge owns the charge rate, so tonight's charging is left to it",
                        ),
                    )
                    .await;
                }
            } else {
                tracing::debug!(reason, "Forecast plan auto-apply standing down");
            }
        }
        PlanApplyDecision::Due { .. } => {
            let planned = compute_plan(state, settings, snapshot, now).await;
            // Mark today done regardless of the outcome shape so a persistent
            // failure can't turn into a per-poll write/notification storm.
            *state.forecast_plan_apply_date.lock().await = Some(now.date_naive());
            match planned {
                Ok(plan) => {
                    record_plan_snapshot(state, settings, "auto_apply", &plan, now).await;
                    apply_plan_recommendation(state, snapshot, &plan.rec, PlanTrigger::AutoApply)
                        .await
                }
                Err(e) => {
                    tracing::warn!("Forecast plan auto-apply failed: {e}");
                    crate::alerts::send_plan_notification(
                        state,
                        &crate::alerts::build_plan_unavailable_message(
                            "the plan computation failed",
                        ),
                    )
                    .await;
                }
            }
        }
    }
}

/// Minutes before the cheap window at which the Planner's decision is
/// recorded for the plan history (issue #361) when no automation did. Wide
/// enough that a few missed polls still land inside it; if none does (the
/// app asleep or restarting through the whole lead), that night has no
/// row rather than a late one, which would describe the following night.
pub const PLAN_SNAPSHOT_LEAD_MINUTES: u16 = 30;

/// Record the Planner's decision shortly before the cheap window for the
/// plan history (issue #361), once per window, for installs where no
/// automation records it. With auto-apply or the nightly refresh running,
/// they record the plan they acted on instead; if Adaptive Charge owns the
/// charge rate they stand down, so this records instead. Needs the solar
/// forecast (weather with coordinates), without which there is no plan.
pub(crate) async fn run_plan_snapshot(
    state: &Arc<AppState>,
    settings: &Settings,
    snapshot: &InverterSnapshot,
    now: DateTime<Local>,
) {
    let automation_records = (settings.forecast_plan_auto_apply_enabled
        || settings.forecast_plan_auto_refresh)
        && !adaptive_owns_rate(settings);
    if automation_records || state.history.lock().await.is_none() {
        return;
    }
    let weather_ready = {
        let weather = state.weather.lock().await;
        weather.config.enabled
            && weather.config.latitude.is_some()
            && weather.config.longitude.is_some()
    };
    if !weather_ready {
        return;
    }
    let Some(tariff) = settings.import_tariff_config.as_ref() else {
        return;
    };
    let now_min = now.hour() as u16 * 60 + now.minute() as u16;
    let Some(window) = crate::forecast::planner::cheapest_import_window(tariff, now_min, 30) else {
        return;
    };
    let until_start = (window.start_min + 1440 - now_min) % 1440;
    if until_start > PLAN_SNAPSHOT_LEAD_MINUTES {
        return;
    }
    // One record per window occurrence, keyed on the date the window opens,
    // so a window just after midnight isn't recorded again once the date
    // rolls over.
    let window_date = (now + chrono::Duration::minutes(i64::from(until_start))).date_naive();
    {
        let mut recorded = state.forecast_plan_snapshot_date.lock().await;
        if *recorded == Some(window_date) {
            return;
        }
        // Mark first so a failing computation can't retry every poll.
        *recorded = Some(window_date);
    }
    match compute_plan(state, settings, snapshot, now).await {
        Ok(plan) => record_plan_snapshot(state, settings, "before_window", &plan, now).await,
        Err(error) => tracing::warn!("Plan history: plan computation failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forecast::planner::ChargeWindow;
    use crate::inverter::model::DeviceType;
    use crate::settings::{TariffConfig, TariffSlot};
    use crate::test_util::with_isolated_config_dir_async;
    use chrono::TimeZone;

    fn local_dt(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local
            .from_local_datetime(
                &NaiveDate::from_ymd_opt(y, m, d)
                    .unwrap()
                    .and_hms_opt(h, min, 0)
                    .unwrap(),
            )
            .earliest()
            .unwrap()
    }

    /// Twenty minutes before the 02:00 cheap window of `flux_tariff`.
    fn before_the_cheap_window() -> DateTime<Local> {
        local_dt(2026, 8, 31, 1, 40)
    }

    fn flux_tariff() -> TariffConfig {
        let slot = |s: &str, e: &str, r: f64| TariffSlot {
            start: s.into(),
            end: e.into(),
            rate: r,
        };
        TariffConfig {
            slots: vec![
                slot("00:00", "02:00", 0.26),
                slot("02:00", "05:00", 0.09),
                slot("05:00", "16:00", 0.26),
                slot("16:00", "21:00", 0.35),
                slot("21:00", "23:59", 0.26),
            ],
        }
    }

    fn gen2() -> InverterSnapshot {
        InverterSnapshot {
            device_type: DeviceType::Gen2Hybrid,
            battery_capacity_kwh: 9.5,
            max_battery_power_w: 3600,
            ..Default::default()
        }
    }

    fn charge(start_min: u16, end_min: u16, tomorrow: bool, kwh: f64) -> PlanRecommendation {
        PlanRecommendation::Charge {
            window: ChargeWindow {
                start_min,
                end_min,
                tomorrow,
                rate: 0.09,
            },
            kwh,
            min_soc_pct: 10.0,
            observed_min_soc_pct: 4.0,
            after_min_soc_pct: 12.0,
            current_soc_pct: 30.0,
            rationale: "test".into(),
            with_charge_series: Vec::new(),
            import_tomorrow_with_charge_kwh: 0.0,
            export_tomorrow_with_charge_kwh: 0.0,
        }
    }

    fn no_charge() -> PlanRecommendation {
        PlanRecommendation::NoChargeNeeded {
            current_soc_pct: 80.0,
            min_soc_pct: 10.0,
            observed_min_soc_pct: 40.0,
        }
    }

    fn no_plan(reason: &str) -> PlanRecommendation {
        PlanRecommendation::NoPlan {
            reason: reason.into(),
        }
    }

    fn written(plan: &SlotPlan) -> Vec<(u16, u16)> {
        let writes = match plan {
            SlotPlan::Write { writes, .. }
            | SlotPlan::WriteHold { writes, .. }
            | SlotPlan::Clear { writes } => writes,
            SlotPlan::NoPlan { .. } => panic!("no writes for {plan:?}"),
        };
        writes
            .as_ref()
            .unwrap()
            .iter()
            .map(|w| (w.address, w.value))
            .collect()
    }

    // ---- plan_slot_plan ---------------------------------------------------

    #[test]
    fn a_charge_plan_writes_the_window_and_arms_timed_charge() {
        let plan = plan_slot_plan(&charge(120, 300, false, 3.2), &gen2());
        let SlotPlan::Write {
            start_hhmm,
            end_hhmm,
            kwh,
            tomorrow,
            ..
        } = &plan
        else {
            panic!("expected a write, got {plan:?}");
        };
        assert_eq!((*start_hhmm, *end_hhmm), (200, 500));
        assert_eq!(*kwh, 3.2);
        assert!(!tomorrow);
        let regs = written(&plan);
        assert!(regs.contains(&(94, 200)), "slot 1 start: {regs:?}");
        assert!(regs.contains(&(95, 500)), "slot 1 end: {regs:?}");
        assert!(regs.contains(&(96, 1)), "enable charge: {regs:?}");
    }

    #[test]
    fn a_charge_plan_re_asserts_the_full_charge_rate() {
        // The duration maths assume the hardware maximum, so every write batch
        // resets the limit (100% is the DC-hybrid value 50, i.e. no limit).
        let regs = written(&plan_slot_plan(&charge(120, 300, false, 3.2), &gen2()));
        assert!(regs.contains(&(111, 50)), "charge limit: {regs:?}");
    }

    #[test]
    fn a_window_starting_tomorrow_is_flagged() {
        let SlotPlan::Write { tomorrow, .. } =
            plan_slot_plan(&charge(120, 300, true, 3.2), &gen2())
        else {
            panic!("expected a write");
        };
        assert!(tomorrow);
    }

    #[test]
    fn a_window_ending_at_midnight_is_clamped_so_the_slot_stays_enabled() {
        // 22:00-24:00 must not encode as 22:00-00:00 (the ambiguous end).
        let SlotPlan::Write { end_hhmm, .. } =
            plan_slot_plan(&charge(22 * 60, 24 * 60 - 1, false, 2.0), &gen2())
        else {
            panic!("expected a write");
        };
        assert_eq!(end_hhmm, 2359);
    }

    #[test]
    fn no_charge_needed_clears_the_slot_without_touching_the_rate() {
        let plan = plan_slot_plan(&no_charge(), &gen2());
        assert!(matches!(plan, SlotPlan::Clear { .. }));
        let regs = written(&plan);
        assert!(
            regs.contains(&(94, 0)) && regs.contains(&(95, 0)),
            "{regs:?}"
        );
        assert!(
            regs.iter().all(|(addr, _)| *addr != 111),
            "clearing must not rewrite the charge limit: {regs:?}"
        );
    }

    #[test]
    fn no_plan_carries_its_reason_through() {
        let plan = plan_slot_plan(&no_plan("no consumption history yet"), &gen2());
        assert!(
            matches!(&plan, SlotPlan::NoPlan { reason } if reason == "no consumption history yet")
        );
    }

    #[test]
    fn three_phase_inverters_use_their_own_slot_registers() {
        let three_phase = InverterSnapshot {
            device_type: DeviceType::ThreePhase,
            battery_capacity_kwh: 9.5,
            ..Default::default()
        };
        let regs = written(&plan_slot_plan(&charge(120, 300, false, 3.2), &three_phase));
        assert!(
            regs.iter().any(|(a, _)| (1113..=1116).contains(a)),
            "three-phase slot registers: {regs:?}"
        );
        assert!(
            regs.iter().all(|(a, _)| *a != 94 && *a != 95),
            "must not write the single-phase slot: {regs:?}"
        );
    }

    // ---- golden register writes (issue #359, Phase 0) ---------------------

    /// Every device family the planner can drive, with the snapshot fields
    /// the slot encoder reads.
    fn golden_devices() -> Vec<(&'static str, InverterSnapshot)> {
        [
            ("gen1", DeviceType::Gen1Hybrid),
            ("gen2", DeviceType::Gen2Hybrid),
            ("gen3", DeviceType::Gen3Hybrid),
            ("gen3_plus", DeviceType::Gen3PlusHybrid),
            ("gen4", DeviceType::Gen4Hybrid),
            ("ac_coupled", DeviceType::ACCoupled),
            ("three_phase", DeviceType::ThreePhase),
            ("aio", DeviceType::AllInOne6kW),
            ("hv_gen3", DeviceType::HybridHvGen3),
            ("gateway", DeviceType::Gateway),
        ]
        .into_iter()
        .map(|(name, device_type)| {
            (
                name,
                InverterSnapshot {
                    device_type,
                    battery_capacity_kwh: 19.0,
                    max_battery_power_w: 3600,
                    ..Default::default()
                },
            )
        })
        .collect()
    }

    fn golden_writes(plan: &SlotPlan) -> String {
        let writes = match plan {
            SlotPlan::Write { writes, .. }
            | SlotPlan::WriteHold { writes, .. }
            | SlotPlan::Clear { writes } => writes,
            SlotPlan::NoPlan { reason } => return format!("no_plan {reason}"),
        };
        match writes {
            Ok(writes) => writes
                .iter()
                .map(|w| format!("{}={}", w.address, w.value))
                .collect::<Vec<_>>()
                .join(" "),
            Err(error) => format!("error {error}"),
        }
    }

    /// The exact writes the default planner queues today, per device family:
    /// `(device, wrapping 23:00-03:34 charge, clear)`. The #359 strategies
    /// add new write paths; these must stay byte-identical.
    const GOLDEN_WRITES: &[(&str, &str, &str)] = &[
        ("gen1", "94=2300 95=334 20=0 96=1 111=50", "94=0 95=0 96=0"),
        ("gen2", "94=2300 95=334 20=0 96=1 111=50", "94=0 95=0 96=0"),
        (
            "gen3",
            "94=2300 95=334 20=0 96=1 111=50 242=100",
            "94=0 95=0 96=0",
        ),
        (
            "gen3_plus",
            "94=2300 95=334 20=0 96=1 111=50",
            "94=0 95=0 96=0",
        ),
        (
            "gen4",
            "94=2300 95=334 20=0 96=1 111=50 242=100",
            "94=0 95=0 96=0",
        ),
        (
            "ac_coupled",
            "94=2300 95=334 20=0 96=1 313=100",
            "94=0 95=0 96=0",
        ),
        (
            "three_phase",
            "1113=2300 1114=334 1110=100 242=100",
            "1113=0 1114=0",
        ),
        (
            "aio",
            "94=2300 95=334 20=0 96=1 111=50 242=100",
            "94=0 95=0 96=0",
        ),
        (
            "hv_gen3",
            "1113=2300 1114=334 1110=100 242=100",
            "1113=0 1114=0",
        ),
        (
            "gateway",
            "94=2300 95=334 20=0 96=1 313=100",
            "94=0 95=0 96=0",
        ),
    ];

    #[test]
    fn default_plan_writes_match_the_golden_table() {
        let devices = golden_devices();
        assert_eq!(devices.len(), GOLDEN_WRITES.len());
        for ((name, snapshot), (golden_name, golden_charge, golden_clear)) in
            devices.iter().zip(GOLDEN_WRITES)
        {
            assert_eq!(name, golden_name);
            let charge_plan = plan_slot_plan(&charge(23 * 60, 3 * 60 + 34, false, 16.44), snapshot);
            assert_eq!(
                golden_writes(&charge_plan),
                *golden_charge,
                "{name}: charge writes changed"
            );
            assert_eq!(
                golden_writes(&plan_slot_plan(&no_charge(), snapshot)),
                *golden_clear,
                "{name}: clear writes changed"
            );
            assert_eq!(
                golden_writes(&plan_slot_plan(&no_plan("why"), snapshot)),
                "no_plan why",
                "{name}: no-plan must queue nothing"
            );
        }
    }

    // ---- hold-through-window plans (issue #359) ----------------------------

    fn hold(tomorrow: bool) -> PlanRecommendation {
        PlanRecommendation::ChargeAndHold {
            window: ChargeWindow {
                start_min: 23 * 60,
                end_min: 6 * 60,
                tomorrow,
                rate: 0.07,
            },
            target_soc_pct: 62,
            kwh: 5.4,
            min_soc_pct: 20.0,
            observed_min_soc_pct: 4.0,
            after_min_soc_pct: 21.0,
            current_soc_pct: 30.0,
            rationale: "why".into(),
            with_charge_series: Vec::new(),
            import_tomorrow_with_charge_kwh: 0.0,
            export_tomorrow_with_charge_kwh: 0.0,
        }
    }

    #[test]
    fn a_hold_plan_carries_its_target_and_window() {
        let plan = plan_slot_plan(&hold(true), &gen2());
        let SlotPlan::WriteHold {
            start_hhmm,
            end_hhmm,
            target_soc,
            kwh,
            tomorrow,
            writes,
        } = &plan
        else {
            panic!("expected a hold write, got {plan:?}");
        };
        assert_eq!((*start_hhmm, *end_hhmm, *target_soc), (2300, 600, 62));
        assert_eq!(*kwh, 5.4);
        assert!(*tomorrow);
        assert!(writes.is_ok());
    }

    #[test]
    fn a_hold_plan_writes_its_target_on_gen3() {
        // Gen3 (Bruce's inverter): the global and per-slot targets, with the
        // charge-target flag left cleared so it never force-charges.
        let gen3 = InverterSnapshot {
            device_type: DeviceType::Gen3Hybrid,
            battery_capacity_kwh: 19.0,
            max_battery_power_w: 3600,
            ..Default::default()
        };
        assert_eq!(
            golden_writes(&plan_slot_plan(&hold(false), &gen3)),
            "94=2300 95=600 116=62 20=0 96=1 111=50 242=62"
        );
    }

    #[test]
    fn a_hold_plan_writes_its_target_on_gen2() {
        // Gen1/2 take the target through HR 116 with the charge-target flag,
        // the same writes the Control page makes for a slot target.
        let regs = written(&plan_slot_plan(&hold(false), &gen2()));
        for expected in [
            (94, 2300),
            (95, 600),
            (20, 1),
            (116, 62),
            (96, 1),
            (111, 50),
        ] {
            assert!(regs.contains(&expected), "missing {expected:?} in {regs:?}");
        }
    }

    #[test]
    fn a_hold_plan_notification_names_the_target() {
        let plan = plan_slot_plan(&hold(false), &gen2());
        let text = plan_notification(&plan, PlanTrigger::AutoApply).expect("auto-apply notifies");
        for needle in ["62%", "23:00", "06:00", "tonight", "hold"] {
            assert!(text.contains(needle), "{needle:?} missing from {text:?}");
        }
        assert!(plan_notification(&plan, PlanTrigger::Refresh).is_none());
    }

    // ---- stale hold targets (issue #359 review) ----------------------------

    fn armed(device_type: DeviceType, target_soc: u8) -> InverterSnapshot {
        InverterSnapshot {
            device_type,
            battery_capacity_kwh: 19.0,
            max_battery_power_w: 3600,
            target_soc,
            ..Default::default()
        }
    }

    fn writes_to(regs: &[(u16, u16)], address: u16) -> Vec<u16> {
        regs.iter()
            .filter(|(a, _)| *a == address)
            .map(|(_, v)| *v)
            .collect()
    }

    fn hold_to(target: u8) -> PlanRecommendation {
        match hold(false) {
            PlanRecommendation::ChargeAndHold {
                window,
                kwh,
                min_soc_pct,
                observed_min_soc_pct,
                after_min_soc_pct,
                current_soc_pct,
                rationale,
                with_charge_series,
                import_tomorrow_with_charge_kwh,
                export_tomorrow_with_charge_kwh,
                ..
            } => PlanRecommendation::ChargeAndHold {
                window,
                target_soc_pct: target,
                kwh,
                min_soc_pct,
                observed_min_soc_pct,
                after_min_soc_pct,
                current_soc_pct,
                rationale,
                with_charge_series,
                import_tomorrow_with_charge_kwh,
                export_tomorrow_with_charge_kwh,
            },
            other => other,
        }
    }

    #[test]
    fn a_full_charge_resets_the_target_the_planner_armed() {
        // A hold plan armed HR 116 at 62; models that follow the global
        // target would otherwise stop the next full charge at 62%.
        for device in [DeviceType::Gen3Hybrid, DeviceType::AllInOne6kW] {
            let regs = written(&plan_slot_plan_for(
                &charge(120, 300, false, 3.2),
                &armed(device, 62),
                Some(62),
            ));
            assert_eq!(writes_to(&regs, 116), vec![100], "{device:?}: {regs:?}");
        }
    }

    #[test]
    fn clearing_the_plan_resets_the_target_the_planner_armed() {
        let regs = written(&plan_slot_plan_for(
            &no_charge(),
            &armed(DeviceType::AllInOne6kW, 62),
            Some(62),
        ));
        assert_eq!(writes_to(&regs, 116), vec![100], "{regs:?}");
        // Flag-gated models disarm by clearing HR 20, which a clear
        // doesn't otherwise touch.
        let regs = written(&plan_slot_plan_for(
            &no_charge(),
            &armed(DeviceType::Gen2Hybrid, 62),
            Some(62),
        ));
        assert_eq!(writes_to(&regs, 20), vec![0], "{regs:?}");
        assert!(writes_to(&regs, 116).is_empty(), "{regs:?}");
    }

    #[test]
    fn a_target_the_user_set_is_never_reset() {
        // No record of the planner arming it, or the user has since
        // changed it: the target is theirs and stays.
        for (snapshot_target, recorded) in [(80, None), (80, Some(62))] {
            let snapshot = armed(DeviceType::AllInOne6kW, snapshot_target);
            for rec in [charge(120, 300, false, 3.2), no_charge()] {
                let regs = written(&plan_slot_plan_for(&rec, &snapshot, recorded));
                assert!(
                    writes_to(&regs, 116).is_empty(),
                    "{snapshot_target} {recorded:?}: {regs:?}"
                );
            }
        }
        // The plain entry point knows of no armed target at all.
        let regs = written(&plan_slot_plan(
            &charge(120, 300, false, 3.2),
            &armed(DeviceType::AllInOne6kW, 62),
        ));
        assert!(writes_to(&regs, 116).is_empty(), "{regs:?}");
    }

    #[test]
    fn no_target_reset_when_none_is_armed() {
        // 100 is "no limit" and 4 is the decoder's unset value.
        for target in [100, 4] {
            let snapshot = armed(DeviceType::AllInOne6kW, target);
            for rec in [charge(120, 300, false, 3.2), no_charge()] {
                let regs = written(&plan_slot_plan_for(&rec, &snapshot, Some(target)));
                assert!(writes_to(&regs, 116).is_empty(), "{target}: {regs:?}");
            }
        }
    }

    #[test]
    fn a_full_hold_resets_the_target_the_planner_armed() {
        // Even 100% couldn't hold the minimum tonight: the hold slot asks
        // for 100, which must not leave last night's 62 in HR 116.
        let regs = written(&plan_slot_plan_for(
            &hold_to(100),
            &armed(DeviceType::AllInOne6kW, 62),
            Some(62),
        ));
        assert_eq!(writes_to(&regs, 116), vec![100], "{regs:?}");
    }

    #[test]
    fn a_new_hold_target_replaces_the_armed_one_without_a_reset() {
        let regs = written(&plan_slot_plan_for(
            &hold(false),
            &armed(DeviceType::Gen3Hybrid, 70),
            Some(70),
        ));
        assert_eq!(writes_to(&regs, 116), vec![62], "{regs:?}");
    }

    #[tokio::test]
    async fn the_planner_records_the_target_it_arms_and_undoes_only_that() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            apply_plan_recommendation(
                &state,
                &armed(DeviceType::AllInOne6kW, 100),
                &hold(false),
                PlanTrigger::AutoApply,
            )
            .await;
            assert_eq!(Settings::load().forecast_plan_armed_target_pct, Some(62));

            // Next night: a full charge with the inverter still at 62.
            apply_plan_recommendation(
                &state,
                &armed(DeviceType::AllInOne6kW, 62),
                &charge(120, 300, false, 3.2),
                PlanTrigger::AutoApply,
            )
            .await;
            let batches = queued(&state).await;
            let last = batches.last().expect("queued");
            assert_eq!(writes_to(last, 116), vec![100], "{last:?}");
            assert_eq!(Settings::load().forecast_plan_armed_target_pct, None);
        })
        .await;
    }

    // ---- plan_notification --------------------------------------------------

    fn all_plans() -> Vec<SlotPlan> {
        let ok = || Ok(Vec::new());
        vec![
            plan_slot_plan(&charge(120, 300, false, 3.2), &gen2()),
            plan_slot_plan(&no_charge(), &gen2()),
            plan_slot_plan(&no_plan("why"), &gen2()),
            SlotPlan::Write {
                start_hhmm: 200,
                end_hhmm: 500,
                kwh: 1.0,
                tomorrow: false,
                writes: Err("boom".into()),
            },
            SlotPlan::Clear {
                writes: Err("boom".into()),
            },
            SlotPlan::Clear { writes: ok() },
        ]
    }

    #[test]
    fn the_quiet_refresh_never_notifies() {
        for plan in all_plans() {
            assert_eq!(
                plan_notification(&plan, PlanTrigger::Refresh),
                None,
                "{plan:?}"
            );
        }
    }

    #[test]
    fn auto_apply_always_tells_the_user_something() {
        for plan in all_plans() {
            assert!(
                plan_notification(&plan, PlanTrigger::AutoApply).is_some(),
                "{plan:?}"
            );
        }
    }

    #[test]
    fn the_applied_message_names_the_window_and_energy() {
        let tonight = plan_slot_plan(&charge(150, 216, false, 3.24), &gen2());
        assert_eq!(
            plan_notification(&tonight, PlanTrigger::AutoApply).unwrap(),
            "📋 Charging plan applied — 3.2 kWh tonight 02:30–03:36 (charge rate 100%)."
        );
        let tomorrow = plan_slot_plan(&charge(15, 300, true, 2.0), &gen2());
        assert!(plan_notification(&tomorrow, PlanTrigger::AutoApply)
            .unwrap()
            .contains("tomorrow 00:15–05:00"));
    }

    #[test]
    fn each_failure_has_its_own_explanation() {
        let note = |plan: &SlotPlan| plan_notification(plan, PlanTrigger::AutoApply).unwrap();
        assert!(note(&plan_slot_plan(&no_charge(), &gen2())).contains("slot 1 cleared"));
        assert!(note(&plan_slot_plan(&no_plan("no tariff"), &gen2())).contains("no tariff"));
        assert!(note(&SlotPlan::Write {
            start_hhmm: 0,
            end_hhmm: 0,
            kwh: 0.0,
            tomorrow: false,
            writes: Err("x".into()),
        })
        .contains("could not be encoded"));
        assert!(note(&SlotPlan::Clear {
            writes: Err("x".into())
        })
        .contains("could not be cleared"));
    }

    // ---- first_report_today ---------------------------------------------------

    #[test]
    fn a_warning_is_reported_once_per_calendar_day() {
        let day = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
        let mut last = None;
        assert!(first_report_today(&mut last, day));
        assert!(!first_report_today(&mut last, day));
        assert!(!first_report_today(&mut last, day));
        assert!(first_report_today(&mut last, day.succ_opt().unwrap()));
        assert!(!first_report_today(&mut last, day.succ_opt().unwrap()));
    }

    // ---- apply_plan_recommendation ---------------------------------------------

    async fn queued(state: &Arc<AppState>) -> Vec<Vec<(u16, u16)>> {
        state
            .pending_writes
            .lock()
            .await
            .iter()
            .map(|b| b.writes.iter().map(|w| (w.address, w.value)).collect())
            .collect()
    }

    #[tokio::test]
    async fn a_charge_recommendation_queues_one_batch_for_either_trigger() {
        for trigger in [PlanTrigger::Refresh, PlanTrigger::AutoApply] {
            with_isolated_config_dir_async(|| async move {
                let state = Arc::new(AppState::new());
                let rec = charge(120, 300, false, 3.2);
                apply_plan_recommendation(&state, &gen2(), &rec, trigger).await;
                let batches = queued(&state).await;
                assert_eq!(batches.len(), 1, "{trigger:?}");
                assert!(batches[0].contains(&(94, 200)), "{trigger:?}");
            })
            .await;
        }
    }

    #[tokio::test]
    async fn a_no_charge_recommendation_queues_the_slot_clear() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            apply_plan_recommendation(&state, &gen2(), &no_charge(), PlanTrigger::Refresh).await;
            let batches = queued(&state).await;
            assert_eq!(batches.len(), 1);
            assert!(batches[0].contains(&(94, 0)) && batches[0].contains(&(95, 0)));
        })
        .await;
    }

    #[tokio::test]
    async fn no_plan_queues_nothing() {
        for trigger in [PlanTrigger::Refresh, PlanTrigger::AutoApply] {
            with_isolated_config_dir_async(|| async move {
                let state = Arc::new(AppState::new());
                apply_plan_recommendation(&state, &gen2(), &no_plan("none"), trigger).await;
                assert!(queued(&state).await.is_empty(), "{trigger:?}");
            })
            .await;
        }
    }

    // ---- orchestration (clock pinned) ---------------------------------------------

    fn auto_apply_settings() -> Settings {
        Settings {
            forecast_plan_auto_apply_enabled: true,
            forecast_plan_auto_apply_lead_minutes: 30,
            import_tariff_config: Some(flux_tariff()),
            ..Default::default()
        }
    }

    fn auto_refresh_settings() -> Settings {
        Settings {
            forecast_plan_auto_refresh: true,
            import_tariff_config: Some(flux_tariff()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn auto_apply_does_nothing_when_disabled() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let mut settings = auto_apply_settings();
            settings.forecast_plan_auto_apply_enabled = false;
            run_plan_auto_apply(&state, &settings, &gen2(), before_the_cheap_window()).await;
            assert_eq!(*state.forecast_plan_apply_date.lock().await, None);
        })
        .await;
    }

    #[tokio::test]
    async fn auto_apply_waits_outside_the_lead_window() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            run_plan_auto_apply(
                &state,
                &auto_apply_settings(),
                &gen2(),
                local_dt(2026, 8, 31, 15, 0),
            )
            .await;
            assert_eq!(*state.forecast_plan_apply_date.lock().await, None);
        })
        .await;
    }

    #[tokio::test]
    async fn auto_apply_needs_a_tariff() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let mut settings = auto_apply_settings();
            settings.import_tariff_config = None;
            run_plan_auto_apply(&state, &settings, &gen2(), before_the_cheap_window()).await;
            assert_eq!(*state.forecast_plan_apply_date.lock().await, None);
        })
        .await;
    }

    #[tokio::test]
    async fn auto_apply_marks_the_day_done_even_when_no_plan_comes_back() {
        // With no history the planner has nothing to work from. The day must
        // still be marked, or a persistent failure would recompute and
        // re-notify on every poll.
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let now = before_the_cheap_window();
            run_plan_auto_apply(&state, &auto_apply_settings(), &gen2(), now).await;
            assert_eq!(
                *state.forecast_plan_apply_date.lock().await,
                Some(now.date_naive())
            );
            assert!(queued(&state).await.is_empty(), "no plan means no writes");
        })
        .await;
    }

    #[tokio::test]
    async fn auto_apply_runs_again_the_next_day() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let yesterday = local_dt(2026, 8, 30, 1, 40).date_naive();
            *state.forecast_plan_apply_date.lock().await = Some(yesterday);
            let now = before_the_cheap_window();
            run_plan_auto_apply(&state, &auto_apply_settings(), &gen2(), now).await;
            assert_eq!(
                *state.forecast_plan_apply_date.lock().await,
                Some(now.date_naive())
            );
        })
        .await;
    }

    #[tokio::test]
    async fn auto_apply_leaves_the_slot_alone_when_adaptive_owns_the_rate() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let mut settings = auto_apply_settings();
            settings.adaptive_charge_enabled = true;
            let now = before_the_cheap_window();
            run_plan_auto_apply(&state, &settings, &gen2(), now).await;
            assert_eq!(
                *state.forecast_plan_apply_date.lock().await,
                None,
                "the apply must not fire while Adaptive owns the rate"
            );
            assert_eq!(
                *state.forecast_plan_apply_warned.lock().await,
                Some(now.date_naive()),
                "but the user is told, once"
            );
            assert!(queued(&state).await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn the_adaptive_warning_needs_the_apply_to_actually_be_due() {
        // An Adaptive user with no cheap window near must not get a daily false
        // alarm.
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let mut settings = auto_apply_settings();
            settings.adaptive_charge_enabled = true;
            run_plan_auto_apply(&state, &settings, &gen2(), local_dt(2026, 8, 31, 15, 0)).await;
            assert_eq!(*state.forecast_plan_apply_warned.lock().await, None);
        })
        .await;
    }

    #[tokio::test]
    async fn a_saved_adaptive_limit_counts_as_adaptive_owning_the_rate() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let mut settings = auto_apply_settings();
            settings.adaptive_charge_enabled = false;
            settings.adaptive_charge_saved_limit =
                Some(crate::settings::AdaptiveChargeSavedLimit {
                    inverter_serial: "SN1".into(),
                    device_type_code: "2001".into(),
                    register_address: 111,
                    raw_value: 40,
                });
            let now = before_the_cheap_window();
            run_plan_auto_apply(&state, &settings, &gen2(), now).await;
            assert_eq!(*state.forecast_plan_apply_date.lock().await, None);
            assert_eq!(
                *state.forecast_plan_apply_warned.lock().await,
                Some(now.date_naive())
            );
        })
        .await;
    }

    #[tokio::test]
    async fn refresh_does_nothing_when_disabled() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let mut settings = auto_refresh_settings();
            settings.forecast_plan_auto_refresh = false;
            run_plan_refresh(&state, &settings, &gen2(), before_the_cheap_window()).await;
            assert_eq!(*state.forecast_plan_refresh_date.lock().await, None);
        })
        .await;
    }

    #[tokio::test]
    async fn refresh_marks_the_day_done_inside_the_lead_window() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let now = before_the_cheap_window();
            run_plan_refresh(&state, &auto_refresh_settings(), &gen2(), now).await;
            assert_eq!(
                *state.forecast_plan_refresh_date.lock().await,
                Some(now.date_naive())
            );
        })
        .await;
    }

    #[tokio::test]
    async fn refresh_waits_outside_the_lead_window() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            run_plan_refresh(
                &state,
                &auto_refresh_settings(),
                &gen2(),
                local_dt(2026, 8, 31, 15, 0),
            )
            .await;
            assert_eq!(*state.forecast_plan_refresh_date.lock().await, None);
        })
        .await;
    }

    #[tokio::test]
    async fn auto_apply_supersedes_the_nightly_refresh() {
        // Exactly one machine write of slot 1 per day: with both enabled only
        // the auto-apply acts.
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let mut settings = auto_apply_settings();
            settings.forecast_plan_auto_refresh = true;
            let now = before_the_cheap_window();
            run_plan_refresh(&state, &settings, &gen2(), now).await;
            assert_eq!(
                *state.forecast_plan_refresh_date.lock().await,
                None,
                "refresh must stand down"
            );
            run_plan_auto_apply(&state, &settings, &gen2(), now).await;
            assert_eq!(
                *state.forecast_plan_apply_date.lock().await,
                Some(now.date_naive())
            );
        })
        .await;
    }

    #[tokio::test]
    async fn refresh_warns_once_and_stands_down_when_adaptive_owns_the_rate() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let mut settings = auto_refresh_settings();
            settings.adaptive_charge_enabled = true;
            let now = before_the_cheap_window();
            run_plan_refresh(&state, &settings, &gen2(), now).await;
            assert_eq!(*state.forecast_plan_refresh_date.lock().await, None);
            assert_eq!(
                *state.forecast_plan_refresh_warned.lock().await,
                Some(now.date_naive())
            );
        })
        .await;
    }

    #[tokio::test]
    async fn the_refresh_warning_needs_the_refresh_to_be_due() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let mut settings = auto_refresh_settings();
            settings.adaptive_charge_enabled = true;
            run_plan_refresh(&state, &settings, &gen2(), local_dt(2026, 8, 31, 15, 0)).await;
            assert_eq!(*state.forecast_plan_refresh_warned.lock().await, None);
        })
        .await;
    }

    // ---- plan history (issue #361) ----------------------------------------------

    async fn state_with_history() -> Arc<AppState> {
        let dir = crate::test_util::make_unique_test_dir("plan-snapshot");
        let db = crate::history::HistoryDb::open(&dir.join("history.db")).expect("open db");
        let state = Arc::new(AppState::new());
        *state.history.lock().await = Some(Arc::new(db));
        // The solar forecast needs weather with coordinates.
        state.weather.lock().await.config = crate::settings::WeatherConfig {
            enabled: true,
            latitude: Some(51.5),
            longitude: Some(-0.13),
            ..Default::default()
        };
        state
    }

    async fn recorded(state: &Arc<AppState>) -> Vec<crate::history::PlanSnapshotRow> {
        state
            .history
            .lock()
            .await
            .clone()
            .expect("history db")
            .query_plan_snapshots(0, i64::MAX)
            .expect("query")
    }

    fn snapshot_settings() -> Settings {
        Settings {
            import_tariff_config: Some(flux_tariff()),
            forecast_plan_strategy: crate::settings::PlanStrategy::HoldWindow,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn the_decision_is_recorded_once_shortly_before_the_cheap_window() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            // Ten minutes before Flux's 02:00 window.
            let now = local_dt(2026, 8, 31, 1, 50);
            run_plan_snapshot(&state, &snapshot_settings(), &gen2(), now).await;
            let rows = recorded(&state).await;
            assert_eq!(rows.len(), 1, "{rows:?}");
            assert_eq!(rows[0].kind, "before_window");
            assert_eq!(rows[0].strategy, "hold_window");
            assert_eq!(rows[0].recorded_at, now.timestamp());
            let plan: serde_json::Value = serde_json::from_str(&rows[0].plan_json).unwrap();
            // The same shape GET /api/forecast/plan serves.
            assert!(plan["recommendation"]["kind"].is_string(), "{plan}");
            assert!(
                plan.get("apply").is_some() && plan.get("export").is_some(),
                "{plan}"
            );
            let forecast: serde_json::Value = serde_json::from_str(&rows[0].forecast_json).unwrap();
            assert!(forecast["solar"].is_array(), "{forecast}");

            // Later polls the same day record nothing more.
            run_plan_snapshot(
                &state,
                &snapshot_settings(),
                &gen2(),
                now + chrono::Duration::minutes(5),
            )
            .await;
            assert_eq!(recorded(&state).await.len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn no_decision_is_recorded_outside_the_lead_window() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            run_plan_snapshot(
                &state,
                &snapshot_settings(),
                &gen2(),
                local_dt(2026, 8, 31, 1, 20),
            )
            .await;
            assert!(recorded(&state).await.is_empty());
            assert_eq!(*state.forecast_plan_snapshot_date.lock().await, None);
        })
        .await;
    }

    #[tokio::test]
    async fn no_extra_decision_is_recorded_while_an_automation_records_the_plan() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let now = local_dt(2026, 8, 31, 1, 50);
            for (auto_apply, auto_refresh) in [(true, false), (false, true)] {
                let settings = Settings {
                    forecast_plan_auto_apply_enabled: auto_apply,
                    forecast_plan_auto_refresh: auto_refresh,
                    ..snapshot_settings()
                };
                run_plan_snapshot(&state, &settings, &gen2(), now).await;
            }
            assert!(recorded(&state).await.is_empty());
            // Adaptive Charge makes the automation stand down, so the
            // decision is recorded here instead.
            let adaptive = Settings {
                forecast_plan_auto_apply_enabled: true,
                adaptive_charge_enabled: true,
                ..snapshot_settings()
            };
            run_plan_snapshot(&state, &adaptive, &gen2(), now).await;
            assert_eq!(recorded(&state).await.len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn a_window_just_after_midnight_is_recorded_once_across_the_date_change() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let settings = Settings {
                import_tariff_config: Some(TariffConfig {
                    slots: vec![
                        TariffSlot {
                            start: "00:00".into(),
                            end: "00:30".into(),
                            rate: 0.26,
                        },
                        TariffSlot {
                            start: "00:30".into(),
                            end: "05:30".into(),
                            rate: 0.07,
                        },
                        TariffSlot {
                            start: "05:30".into(),
                            end: "23:59".into(),
                            rate: 0.26,
                        },
                    ],
                }),
                ..snapshot_settings()
            };
            // 00:10 and 00:20 are both inside the lead of the 00:30 window
            // and belong to the same occurrence.
            run_plan_snapshot(&state, &settings, &gen2(), local_dt(2026, 8, 31, 0, 10)).await;
            run_plan_snapshot(&state, &settings, &gen2(), local_dt(2026, 8, 31, 0, 20)).await;
            assert_eq!(recorded(&state).await.len(), 1);
            run_plan_snapshot(&state, &settings, &gen2(), local_dt(2026, 9, 1, 0, 10)).await;
            assert_eq!(
                recorded(&state).await.len(),
                2,
                "next night is a new window"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn nothing_is_recorded_without_the_solar_forecast_or_a_history_db() {
        with_isolated_config_dir_async(|| async {
            let now = local_dt(2026, 8, 31, 1, 50);
            // No weather: there is no solar forecast to plan from.
            let state = state_with_history().await;
            state.weather.lock().await.config.enabled = false;
            run_plan_snapshot(&state, &snapshot_settings(), &gen2(), now).await;
            assert!(recorded(&state).await.is_empty());
            // No history database.
            let bare = Arc::new(AppState::new());
            run_plan_snapshot(&bare, &snapshot_settings(), &gen2(), now).await;
            assert_eq!(*bare.forecast_plan_snapshot_date.lock().await, None);
        })
        .await;
    }

    #[tokio::test]
    async fn recording_prunes_decisions_past_the_retention_period() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let now = local_dt(2026, 8, 31, 1, 50);
            let db = state.history.lock().await.clone().expect("db");
            let day = 24 * 3600;
            for age_days in [crate::history::PLAN_SNAPSHOT_RETENTION_DAYS + 1, 5] {
                db.insert_plan_snapshot(&crate::history::PlanSnapshotRow {
                    recorded_at: now.timestamp() - age_days * day,
                    kind: "before_window".into(),
                    strategy: "min_soc".into(),
                    plan_json: "{}".into(),
                    forecast_json: "{}".into(),
                })
                .unwrap();
            }
            run_plan_snapshot(&state, &snapshot_settings(), &gen2(), now).await;
            let ages: Vec<i64> = recorded(&state)
                .await
                .iter()
                .map(|row| (now.timestamp() - row.recorded_at) / day)
                .collect();
            assert_eq!(ages, vec![5, 0], "the old row is pruned, newer ones kept");
        })
        .await;
    }

    #[tokio::test]
    async fn auto_apply_records_the_plan_it_acted_on() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let now = before_the_cheap_window();
            run_plan_auto_apply(&state, &auto_apply_settings(), &gen2(), now).await;
            let rows = recorded(&state).await;
            assert_eq!(rows.len(), 1, "{rows:?}");
            assert_eq!(rows[0].kind, "auto_apply");
            assert_eq!(rows[0].strategy, "min_soc");
        })
        .await;
    }

    #[tokio::test]
    async fn the_nightly_refresh_records_the_plan_it_acted_on() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let settings = Settings {
                forecast_plan_auto_refresh: true,
                import_tariff_config: Some(flux_tariff()),
                ..Default::default()
            };
            run_plan_refresh(&state, &settings, &gen2(), before_the_cheap_window()).await;
            let rows = recorded(&state).await;
            assert_eq!(rows.len(), 1, "{rows:?}");
            assert_eq!(rows[0].kind, "refresh");
        })
        .await;
    }
}
