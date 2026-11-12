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

use chrono::{DateTime, Local, NaiveDate, Timelike, Utc};

use crate::forecast::planner::PlanRecommendation;
use crate::forecast::refresh::{
    plan_auto_apply_adaptive_warning_due, plan_auto_apply_decision_with_adaptive,
    plan_refresh_action, plan_refresh_due, plan_refresh_due_with_adaptive, PlanApplyDecision,
    PlanRefreshAction, PLAN_CHARGE_RATE_PERCENT, SLOT_TARGET_SOC_NONE,
};
use crate::inverter::encoder::RegisterWrite;
use crate::inverter::model::InverterSnapshot;
use crate::inverter::plan_replan::{PlanCycle, QueuedSlot};
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
    /// A rewrite after Octopus moved a dispatch (issue #404). Notifies on
    /// its own terms, through [`crate::inverter::plan_replan`].
    Replan,
}

impl PlanTrigger {
    fn label(self) -> &'static str {
        match self {
            Self::Refresh => "Forecast plan refresh",
            Self::AutoApply => "Forecast plan auto-apply",
            Self::Replan => "Forecast plan replan",
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
pub(crate) fn plan_notification(
    plan: &SlotPlan,
    trigger: PlanTrigger,
    every_cheap_window: bool,
) -> Option<String> {
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
        SlotPlan::Clear { writes: Ok(_) } => build_plan_cleared_message(every_cheap_window),
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
/// Returns what charge slot 1 was set to and its time writes, when writes
/// were queued.
pub(crate) async fn apply_plan_recommendation(
    state: &Arc<AppState>,
    snapshot: &InverterSnapshot,
    rec: &PlanRecommendation,
    trigger: PlanTrigger,
) -> Option<QueuedSlot> {
    let settings = Settings::load_async().await;
    let planner_armed = settings.forecast_plan_armed_target_pct;
    let plan = plan_slot_plan_for(rec, snapshot, planner_armed);
    queue_slot_plan(state, &plan, trigger, planner_armed).await;
    if let Some(text) = plan_notification(&plan, trigger, settings.forecast_plan_every_cheap_window)
    {
        crate::alerts::send_plan_notification(state, &text).await;
    }
    QueuedSlot::of(&plan)
}

/// Log and queue a slot plan's writes, and record the planner's armed hold
/// target after them.
pub(crate) async fn queue_slot_plan(
    state: &Arc<AppState>,
    plan: &SlotPlan,
    trigger: PlanTrigger,
    planner_armed: Option<u8>,
) {
    let label = trigger.label();
    match plan {
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
                PlanTrigger::Replan => tracing::info!(
                    start = start_hhmm,
                    end = end_hhmm,
                    kwh = format!("{kwh:.2}"),
                    "{label}: rewriting charge slot 1 after Octopus moved a smart charge"
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
            PlanTrigger::Refresh | PlanTrigger::Replan => {
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
    if let Some(next) = armed_target_after(plan) {
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
}

/// Compute a fresh recommendation from the live snapshot. SQLite reads and a
/// 72 h simulation run on a blocking thread, off the poll task.
///
/// `running_window_start` (Unix seconds) plans the rest of a cheap window
/// already under way instead of the next one (issue #404). The forecast's
/// hours then start at the current hour, because they otherwise start at
/// the next one and the plan couldn't see the charge until then; the caller
/// only asks near the top of an hour, so the minutes counted twice are few.
/// No export advice comes with it.
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
    running_window_start: Option<i64>,
) -> Result<ComputedPlan, tokio::task::JoinError> {
    let (weather_enabled, coords) = {
        let ws = state.weather.lock().await;
        (
            ws.config.enabled,
            ws.config.latitude.zip(ws.config.longitude),
        )
    };
    let history = state.history.lock().await.clone();
    let dispatches =
        crate::octopus_dispatches::planning_dispatches(state, settings, now.with_timezone(&Utc))
            .await;
    // Only a fresh list says what Octopus plans; a plan made without one
    // records none, so the first fresh list counts as a change.
    let dispatch_revision =
        (dispatches.checked_at.is_some() && !dispatches.stale).then_some(dispatches.revision);
    // A window under way, or one starting later this hour, needs the
    // current hour in the series or its first part isn't in the plan.
    let include_current_hour = running_window_start.is_some()
        || crate::forecast::planner::plan_needs_current_hour(
            settings.import_tariff_config.as_ref(),
            now,
            settings.forecast_plan_every_cheap_window,
        );
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
            dispatches: &dispatches,
            include_current_hour,
        });
        let (rec, export) = match running_window_start {
            Some(start) => (
                crate::server::api::compute_running_window_plan(
                    &forecast,
                    &settings,
                    Some(&live_snapshot),
                    now.timestamp(),
                    start,
                ),
                None,
            ),
            None => crate::server::api::compute_full_plan(
                &forecast,
                &settings,
                Some(&live_snapshot),
                now.timestamp(),
            ),
        };
        ComputedPlan {
            rec,
            export,
            forecast,
            dispatch_revision,
        }
    })
    .await
}

/// A freshly computed plan with the export advice and forecast behind it.
struct ComputedPlan {
    rec: PlanRecommendation,
    export: Option<crate::forecast::planner::ExportAdvice>,
    forecast: crate::forecast::ForecastPayload,
    /// The dispatch list revision the plan was made with (issue #404); none
    /// when it had no fresh list.
    dispatch_revision: Option<u64>,
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

/// The window a once-a-day write at `now` is for (see
/// [`crate::forecast::refresh::once_a_day_window_key`]).
fn planned_window_key(
    settings: &Settings,
    now: DateTime<Local>,
    lead_minutes: u16,
) -> Option<crate::forecast::refresh::WindowKey> {
    let tariff = settings.import_tariff_config.as_ref()?;
    crate::forecast::refresh::once_a_day_window_key(tariff, now, lead_minutes)
}

/// Log a cheap window that started before HEM could plan it (issue #453).
fn log_missed_window(label: &str, key: crate::forecast::refresh::WindowKey) {
    tracing::info!(
        "{label}: the {:02}:{:02} cheap period started before HEM could plan it; charge slot 1 keeps its previous times until the next one",
        key.1 / 60,
        key.1 % 60
    );
}

/// When the daily write is due: once a day for the cheapest window, or with
/// every cheap window planned, once per window (issue #453).
enum DailyDue {
    /// Plan now, latching this window occurrence. `running` is the start
    /// (Unix seconds) of a window already under way that is planned as one.
    Due {
        key: Option<crate::forecast::refresh::WindowKey>,
        running: Option<i64>,
    },
    /// Would be due, but Adaptive Charge owns the charge rate.
    AdaptiveOwnsRate,
    NotDue,
}

/// The start, Unix seconds, of the window occurrence `key` names; `None`
/// when the clock skipped it.
fn running_start(key: crate::forecast::refresh::WindowKey) -> Option<i64> {
    crate::inverter::plan_replan::local_ts(key.0, u32::from(key.1))
}

/// What the every-cheap-window gate means for a daily write (issue #453).
/// A missed window is latched in `latch` and logged here.
fn every_window_daily(
    due: crate::forecast::refresh::EveryWindowDue,
    latch: &mut Option<crate::forecast::refresh::WindowKey>,
    adaptive_owns_rate: bool,
    label: &str,
) -> DailyDue {
    use crate::forecast::refresh::EveryWindowDue;
    match due {
        EveryWindowDue::Due(_) | EveryWindowDue::DueRunning(_) if adaptive_owns_rate => {
            DailyDue::AdaptiveOwnsRate
        }
        EveryWindowDue::Due(key) => DailyDue::Due {
            key: Some(key),
            running: None,
        },
        EveryWindowDue::DueRunning(key) => match running_start(key) {
            Some(start) => DailyDue::Due {
                key: Some(key),
                running: Some(start),
            },
            // A start the clock skipped can't anchor a window under way:
            // planned from now, it would move the running slot.
            None => every_window_daily(
                EveryWindowDue::Missed(key),
                latch,
                adaptive_owns_rate,
                label,
            ),
        },
        EveryWindowDue::Missed(key) => {
            *latch = Some(key);
            if !adaptive_owns_rate {
                log_missed_window(label, key);
            }
            DailyDue::NotDue
        }
        EveryWindowDue::NotDue => DailyDue::NotDue,
    }
}

/// The fixed nightly auto-refresh. Does nothing unless it is enabled and the
/// auto-apply trigger is not (that supersedes it).
pub(crate) async fn run_plan_refresh(
    state: &Arc<AppState>,
    settings: &Settings,
    snapshot: &InverterSnapshot,
    now: DateTime<Local>,
) {
    // A Gateway doesn't pass the charge slot on to the AIO (issue #428).
    if !settings.forecast_plan_auto_refresh
        || settings.forecast_plan_auto_apply_enabled
        || !snapshot.device_type.battery_controls_reach_battery()
    {
        return;
    }
    let adaptive_owns_rate = adaptive_owns_rate(settings);
    let tariff = settings.import_tariff_config.as_ref();
    let lead = crate::forecast::refresh::PLAN_REFRESH_LEAD_MINUTES;
    let due = if settings.forecast_plan_every_cheap_window {
        let mut latch = state.forecast_plan_refresh_window.lock().await;
        let gate = crate::forecast::refresh::every_window_due(now, *latch, tariff, lead);
        every_window_daily(
            gate,
            &mut latch,
            adaptive_owns_rate,
            PlanTrigger::Refresh.label(),
        )
    } else {
        let last = *state.forecast_plan_refresh_date.lock().await;
        let key = planned_window_key(settings, now, lead);
        // The date moves on at midnight, but not the window: one planned
        // before midnight (or by every-window planning) isn't planned again.
        if key.is_some() && key == *state.forecast_plan_refresh_window.lock().await {
            DailyDue::NotDue
        } else if plan_refresh_due_with_adaptive(now, last, tariff, adaptive_owns_rate) {
            DailyDue::Due { key, running: None }
        } else if adaptive_owns_rate && plan_refresh_due(now, last, tariff) {
            DailyDue::AdaptiveOwnsRate
        } else {
            DailyDue::NotDue
        }
    };
    match due {
        DailyDue::Due { key, running } => {
            let planned = compute_plan(state, settings, snapshot, now, running).await;
            // Mark this window done regardless of the outcome shape so a
            // persistent failure can't turn into a per-poll write storm. The
            // date says today's once-a-day plan is in the slot: a plan for
            // one of several cheap windows replaces it, so switching back
            // still plans the next (the window latch stops a second plan).
            *state.forecast_plan_refresh_date.lock().await =
                (!settings.forecast_plan_every_cheap_window).then(|| now.date_naive());
            *state.forecast_plan_refresh_window.lock().await = key;
            match planned {
                Ok(plan) => {
                    record_plan_snapshot(state, settings, "refresh", &plan, now).await;
                    let slot =
                        apply_plan_recommendation(state, snapshot, &plan.rec, PlanTrigger::Refresh)
                            .await;
                    let cycle = CycleFor {
                        trigger: PlanTrigger::Refresh,
                        now,
                        running,
                    };
                    start_cycle(state, settings, cycle, slot, &plan).await;
                }
                Err(e) => tracing::warn!("Forecast plan refresh failed: {e}"),
            }
        }
        DailyDue::AdaptiveOwnsRate => {
            // The refresh would be due but Adaptive Charge owns the
            // charge-limit register: warn once per day (tracked separately so
            // disabling Adaptive later in the same lead window still lets the
            // refresh fire) instead of silently skipping on every poll
            // (CODE_REVIEW.md Major 3).
            let mut warned = state.forecast_plan_refresh_warned.lock().await;
            if first_report_today(&mut warned, now.date_naive()) {
                tracing::warn!(
                    "Forecast plan auto-refresh is enabled but Adaptive Charge owns the charge rate — skipping the next cheap period's slot rewrite"
                );
            }
        }
        DailyDue::NotDue => {}
    }
}

/// The user-configured auto-apply. Does nothing unless it is enabled.
pub(crate) async fn run_plan_auto_apply(
    state: &Arc<AppState>,
    settings: &Settings,
    snapshot: &InverterSnapshot,
    now: DateTime<Local>,
) {
    // A Gateway doesn't pass the charge slot on to the AIO (issue #428).
    if !settings.forecast_plan_auto_apply_enabled
        || !snapshot.device_type.battery_controls_reach_battery()
    {
        return;
    }
    let adaptive_owns_rate = adaptive_owns_rate(settings);
    let tariff = settings.import_tariff_config.as_ref();
    let lead = settings.forecast_plan_auto_apply_lead_minutes;
    let due = if settings.forecast_plan_every_cheap_window {
        let mut latch = state.forecast_plan_apply_window.lock().await;
        let gate = crate::forecast::refresh::every_window_due(now, *latch, tariff, lead);
        every_window_daily(
            gate,
            &mut latch,
            adaptive_owns_rate,
            PlanTrigger::AutoApply.label(),
        )
    } else {
        let last = *state.forecast_plan_apply_date.lock().await;
        let key = planned_window_key(settings, now, lead);
        let planned_already =
            key.is_some() && key == *state.forecast_plan_apply_window.lock().await;
        match plan_auto_apply_decision_with_adaptive(now, last, tariff, lead, adaptive_owns_rate) {
            // The date moves on at midnight, but not the window: one planned
            // before midnight (or by every-window planning) isn't planned again.
            _ if planned_already => DailyDue::NotDue,
            PlanApplyDecision::Due { .. } => DailyDue::Due { key, running: None },
            // Would-be-due but Adaptive Charge owns the charge-limit register.
            // Gated on the apply actually being due, or an Adaptive user with
            // no cheap window near would get a false alarm every single day.
            PlanApplyDecision::NotDue { .. }
                if adaptive_owns_rate
                    && plan_auto_apply_adaptive_warning_due(now, last, tariff, lead) =>
            {
                DailyDue::AdaptiveOwnsRate
            }
            PlanApplyDecision::NotDue { reason } => {
                tracing::debug!(reason, "Forecast plan auto-apply standing down");
                DailyDue::NotDue
            }
        }
    };
    match due {
        DailyDue::AdaptiveOwnsRate => {
            // Warn + notify once per day instead of on every poll.
            let first = {
                let mut warned = state.forecast_plan_apply_warned.lock().await;
                first_report_today(&mut warned, now.date_naive())
            };
            if first {
                tracing::warn!(
                    "Forecast plan auto-apply is enabled but Adaptive Charge owns the charge rate — skipping the next cheap period's apply"
                );
                crate::alerts::send_plan_notification(
                    state,
                    &crate::alerts::build_plan_unavailable_message(
                        "Adaptive Charge owns the charge rate, so the next cheap period's charging is left to it",
                    ),
                )
                .await;
            }
        }
        DailyDue::Due { key, running } => {
            let planned = compute_plan(state, settings, snapshot, now, running).await;
            // Mark this window done regardless of the outcome shape so a
            // persistent failure can't turn into a per-poll write/notification
            // storm. The date is the once-a-day plan's, as for the refresh.
            *state.forecast_plan_apply_date.lock().await =
                (!settings.forecast_plan_every_cheap_window).then(|| now.date_naive());
            *state.forecast_plan_apply_window.lock().await = key;
            match planned {
                Ok(plan) => {
                    record_plan_snapshot(state, settings, "auto_apply", &plan, now).await;
                    let slot = apply_plan_recommendation(
                        state,
                        snapshot,
                        &plan.rec,
                        PlanTrigger::AutoApply,
                    )
                    .await;
                    let cycle = CycleFor {
                        trigger: PlanTrigger::AutoApply,
                        now,
                        running,
                    };
                    start_cycle(state, settings, cycle, slot, &plan).await;
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
        DailyDue::NotDue => {}
    }
}

/// Which daily write a cycle is for: its trigger, when it ran, and the
/// start of the window under way it planned, if it planned one.
struct CycleFor {
    trigger: PlanTrigger,
    now: DateTime<Local>,
    running: Option<i64>,
}

/// Start the replan's cycle (issue #404) after a daily write: the slot it
/// set, for the cheap-window occurrence it was for, with the dispatch list
/// it was planned with. A write that queued nothing leaves no cycle.
async fn start_cycle(
    state: &Arc<AppState>,
    settings: &Settings,
    CycleFor {
        trigger,
        now,
        running,
    }: CycleFor,
    queued: Option<QueuedSlot>,
    plan: &ComputedPlan,
) {
    // A window planned under way is the one that started at `running`.
    let picked_at = running
        .and_then(|start| DateTime::from_timestamp(start, 0))
        .map_or(now, |start| start.with_timezone(&Local));
    let window = settings.import_tariff_config.as_ref().and_then(|tariff| {
        crate::inverter::plan_replan::cycle_window(
            tariff,
            picked_at,
            settings.forecast_plan_every_cheap_window,
        )
    });
    let cycle =
        queued
            .zip(window)
            .map(
                |(queued, (window_start, window_end, window_start_hhmm))| PlanCycle {
                    trigger,
                    written_at: now.timestamp(),
                    window_start,
                    window_end,
                    window_start_hhmm,
                    slot: queued.summary,
                    dispatch_revision: plan.dispatch_revision,
                    slot_writes: queued.slot_writes,
                    mismatched_reads: 0,
                },
            );
    *state.forecast_plan_cycle.lock().await = cycle;
}

/// Whether the cycle's owner still writes charge slot 1.
fn cycle_owner_on(trigger: PlanTrigger, settings: &Settings) -> bool {
    match trigger {
        PlanTrigger::AutoApply => settings.forecast_plan_auto_apply_enabled,
        PlanTrigger::Refresh => {
            settings.forecast_plan_auto_refresh && !settings.forecast_plan_auto_apply_enabled
        }
        PlanTrigger::Replan => false,
    }
}

/// Re-plan charge slot 1 when Octopus has moved a dispatch since the plan
/// was written (issue #404). The decisions are in
/// [`crate::inverter::plan_replan`]; this gathers the facts and acts.
pub(crate) async fn run_plan_replan(
    state: &Arc<AppState>,
    settings: &Settings,
    snapshot: &InverterSnapshot,
    now: DateTime<Local>,
    sit_out: bool,
) {
    run_plan_replan_with(state, settings, snapshot, now, sit_out, |running| {
        compute_plan(state, settings, snapshot, now, running)
    })
    .await;
}

/// [`run_plan_replan`] with the plan computation passed in, so tests can
/// choose what the fresh plan says.
async fn run_plan_replan_with<F, Fut>(
    state: &Arc<AppState>,
    settings: &Settings,
    snapshot: &InverterSnapshot,
    now: DateTime<Local>,
    sit_out: bool,
    compute: F,
) where
    F: FnOnce(Option<i64>) -> Fut,
    Fut: std::future::Future<Output = Result<ComputedPlan, tokio::task::JoinError>>,
{
    use crate::inverter::plan_replan::{
        mismatch_count, replan_message, replan_outcome, replan_step, ReplanFacts, ReplanOutcome,
        ReplanStep, TAKEN_OVER_READS,
    };
    let Some(mut cycle) = state.forecast_plan_cycle.lock().await.clone() else {
        return;
    };
    let planning =
        crate::octopus_dispatches::planning_dispatches(state, settings, now.with_timezone(&Utc))
            .await;
    // Only a read taken after the plan's writes drained can say whether the
    // slot is still what the plan wrote, and not during a top-up, which has
    // its own slot 1 on the inverter until it puts the plan's back.
    let top_up_running = state.dispatch_charge.lock().await.run.is_some();
    let writes_drained = state.pending_writes.lock().await.is_empty();
    let matches = (writes_drained && !top_up_running && snapshot.timestamp > cycle.written_at)
        .then(|| crate::server::api::snapshot_matches_writes(snapshot, &cycle.slot_writes));
    let mismatched = mismatch_count(cycle.mismatched_reads, matches);
    if mismatched != cycle.mismatched_reads
        && update_cycle(state, &cycle, |c| c.mismatched_reads = mismatched).await
    {
        cycle.mismatched_reads = mismatched;
    }
    let taken_over = cycle.mismatched_reads >= TAKEN_OVER_READS;
    let facts = ReplanFacts {
        now: now.timestamp(),
        minute_of_hour: now.minute(),
        owner_on: cycle_owner_on(cycle.trigger, settings),
        mismatched_reads: cycle.mismatched_reads,
        reaches_battery: snapshot.device_type.battery_controls_reach_battery(),
        adaptive_owns_rate: adaptive_owns_rate(settings),
        sit_out,
        top_up_running,
        list_fresh: planning.checked_at.is_some() && !planning.stale,
        revision: planning.revision,
    };
    let label = PlanTrigger::Replan.label();
    let running = match replan_step(&cycle, &facts) {
        ReplanStep::Idle => return,
        ReplanStep::Wait(reason) => {
            tracing::debug!(reason, "{label}: Octopus moved a smart charge; waiting");
            return;
        }
        ReplanStep::EndCycle => {
            if taken_over {
                tracing::info!(
                    "{label}: charge slot 1 no longer reads back as the plan set it; leaving it alone tonight"
                );
            }
            end_cycle(state, &cycle).await;
            return;
        }
        ReplanStep::Compute { running } => running,
    };
    let computed = match compute(running).await {
        Ok(computed) => computed,
        Err(error) => {
            tracing::warn!("{label}: plan computation failed: {error}");
            // Not retried until the list changes again.
            update_cycle(state, &cycle, |c| {
                c.dispatch_revision = Some(facts.revision);
            })
            .await;
            return;
        }
    };
    let planner_armed = Settings::load_async().await.forecast_plan_armed_target_pct;
    let plan = plan_slot_plan_for(&computed.rec, snapshot, planner_armed);
    match replan_outcome(&cycle, &plan, now.timestamp()) {
        ReplanOutcome::Keep(reason) => {
            tracing::info!(
                reason,
                "{label}: Octopus moved a smart charge; charge slot 1 stays"
            );
            update_cycle(state, &cycle, |c| {
                c.dispatch_revision = computed.dispatch_revision;
            })
            .await;
        }
        ReplanOutcome::Rewrite(queued) => {
            let new = queued.summary;
            // Claim the cycle first: one a daily write has since replaced
            // must not get this plan's writes.
            let updated = update_cycle(state, &cycle, |c| {
                c.slot = new;
                c.slot_writes = queued.slot_writes;
                c.dispatch_revision = computed.dispatch_revision;
                c.written_at = now.timestamp();
                c.mismatched_reads = 0;
            })
            .await;
            if !updated {
                return;
            }
            queue_slot_plan(state, &plan, PlanTrigger::Replan, planner_armed).await;
            record_plan_snapshot(state, settings, "replan", &computed, now).await;
            if cycle.trigger == PlanTrigger::AutoApply {
                let text = replan_message(
                    cycle.slot,
                    new,
                    cycle.kwh_from(new, rec_kwh(&computed.rec), now.timestamp()),
                    cycle.stops_charging(new, now.timestamp()),
                );
                crate::alerts::send_plan_notification(state, &text).await;
            }
        }
    }
}

/// Apply `change` to the stored cycle if it is still `cycle` (a daily write
/// may have replaced it meanwhile). True when it was.
async fn update_cycle(
    state: &Arc<AppState>,
    cycle: &PlanCycle,
    change: impl FnOnce(&mut PlanCycle),
) -> bool {
    let mut stored = state.forecast_plan_cycle.lock().await;
    match stored.as_mut() {
        Some(current) if current == cycle => {
            change(current);
            true
        }
        _ => false,
    }
}

async fn end_cycle(state: &Arc<AppState>, cycle: &PlanCycle) {
    let mut stored = state.forecast_plan_cycle.lock().await;
    if stored.as_ref() == Some(cycle) {
        *stored = None;
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
    if settings.forecast_plan_every_cheap_window {
        // Once per cheap window (issue #453).
        use crate::forecast::refresh::{every_window_due, EveryWindowDue};
        let mut recorded = state.forecast_plan_snapshot_window.lock().await;
        let EveryWindowDue::Due(key) =
            every_window_due(now, *recorded, Some(tariff), PLAN_SNAPSHOT_LEAD_MINUTES)
        else {
            return;
        };
        // Mark first so a failing computation can't retry every poll.
        *recorded = Some(key);
    } else {
        let now_min = now.hour() as u16 * 60 + now.minute() as u16;
        let Some(window) = crate::forecast::planner::cheapest_import_window(tariff, now_min, 30)
        else {
            return;
        };
        let until_start = (window.start_min + 1440 - now_min) % 1440;
        if until_start > PLAN_SNAPSHOT_LEAD_MINUTES {
            return;
        }
        // One record per window occurrence, keyed on the date the window
        // opens, so a window just after midnight isn't recorded again once
        // the date rolls over, nor one every-window planning recorded.
        let key = crate::forecast::refresh::occurrence_key(&window, now);
        let mut recorded = state.forecast_plan_snapshot_window.lock().await;
        if *recorded == Some(key) {
            return;
        }
        // Mark first so a failing computation can't retry every poll.
        *recorded = Some(key);
        *state.forecast_plan_snapshot_date.lock().await = Some(key.0);
    }
    match compute_plan(state, settings, snapshot, now, None).await {
        Ok(plan) => record_plan_snapshot(state, settings, "before_window", &plan, now).await,
        Err(error) => tracing::warn!("Plan history: plan computation failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forecast::planner::ChargeWindow;
    use crate::inverter::model::DeviceType;
    use crate::inverter::plan_replan::SlotSummary;
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
            ("aio_hybrid", DeviceType::AllInOneHybrid),
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
            "aio_hybrid",
            "94=2300 95=334 20=0 96=1 111=50 242=100",
            "94=0 95=0 96=0",
        ),
        // The HV Gen3 and the AIO Hybrid write the single-phase slot map like
        // the Gen3/AIO (issue #432).
        (
            "hv_gen3",
            "94=2300 95=334 20=0 96=1 111=50 242=100",
            "94=0 95=0 96=0",
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
            strategy: crate::settings::PlanStrategy::HoldWindow,
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
        let text =
            plan_notification(&plan, PlanTrigger::AutoApply, false).expect("auto-apply notifies");
        for needle in ["62%", "23:00", "06:00", "tonight", "hold"] {
            assert!(text.contains(needle), "{needle:?} missing from {text:?}");
        }
        assert!(plan_notification(&plan, PlanTrigger::Refresh, false).is_none());
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
                strategy: crate::settings::PlanStrategy::HoldWindow,
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
                plan_notification(&plan, PlanTrigger::Refresh, false),
                None,
                "{plan:?}"
            );
        }
    }

    #[test]
    fn auto_apply_always_tells_the_user_something() {
        for plan in all_plans() {
            assert!(
                plan_notification(&plan, PlanTrigger::AutoApply, false).is_some(),
                "{plan:?}"
            );
        }
    }

    #[test]
    fn the_applied_message_names_the_window_and_energy() {
        let tonight = plan_slot_plan(&charge(150, 216, false, 3.24), &gen2());
        assert_eq!(
            plan_notification(&tonight, PlanTrigger::AutoApply, false).unwrap(),
            "📋 Charging plan applied — 3.2 kWh tonight 02:30–03:36 (charge rate 100%)."
        );
        let tomorrow = plan_slot_plan(&charge(15, 300, true, 2.0), &gen2());
        assert!(plan_notification(&tomorrow, PlanTrigger::AutoApply, false)
            .unwrap()
            .contains("tomorrow 00:15–05:00"));
    }

    #[test]
    fn each_failure_has_its_own_explanation() {
        let note =
            |plan: &SlotPlan| plan_notification(plan, PlanTrigger::AutoApply, false).unwrap();
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

    fn gateway() -> InverterSnapshot {
        InverterSnapshot {
            device_type: DeviceType::Gateway,
            ..gen2()
        }
    }

    /// Issue #428: a Gateway keeps its own copy of the charge slot and doesn't
    /// pass writes to the AIO, so a plan applied there would never charge.
    /// Both automations sit out — no plan, no writes, no "applied" message —
    /// and pick up again once HEM is connected to the AIO.
    #[tokio::test]
    async fn auto_apply_and_refresh_sit_out_on_a_gateway() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let now = before_the_cheap_window();
            run_plan_auto_apply(&state, &auto_apply_settings(), &gateway(), now).await;
            run_plan_refresh(&state, &auto_refresh_settings(), &gateway(), now).await;
            assert_eq!(*state.forecast_plan_apply_date.lock().await, None);
            assert_eq!(*state.forecast_plan_refresh_date.lock().await, None);
            assert_eq!(*state.forecast_plan_apply_warned.lock().await, None);
            assert!(queued(&state).await.is_empty());
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
    async fn the_recorded_plan_sees_the_live_dispatch_list() {
        // Issue #406, through AppState: the plan reads the poller's list
        // with the hold on, and records what it saw.
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let now = local_dt(2026, 8, 31, 1, 50);
            {
                let db = state.history.lock().await.clone().unwrap();
                let hour = now.timestamp() - now.timestamp().rem_euclid(3600);
                let rows: Vec<_> = (0..72)
                    .map(|h| crate::history::ForecastValueRow {
                        timestamp: hour + h * 3600,
                        variable: "shortwave_radiation".to_string(),
                        value: 0.0,
                        source: "open-meteo".to_string(),
                        fetched_at: now.timestamp(),
                    })
                    .collect();
                db.insert_forecast_values(&rows).unwrap();
            }
            let utc = |dt: DateTime<Local>| dt.with_timezone(&chrono::Utc);
            {
                let mut list = state.octopus_dispatches.lock().await;
                list.checked_at = Some(utc(now));
                list.dispatches = vec![crate::octopus_dispatches::Dispatch {
                    start: utc(local_dt(2026, 8, 31, 2, 0)),
                    end: utc(local_dt(2026, 8, 31, 4, 0)),
                    kind: Some("SMART".to_string()),
                    energy_added_kwh: Some(7.0),
                }];
            }
            let settings = Settings {
                octopus_api_key: "sk_live_test".to_string(),
                octopus_account_number: "A-TEST1".to_string(),
                octopus_dispatch_hold: true,
                ..snapshot_settings()
            };
            run_plan_snapshot(&state, &settings, &gen2(), now).await;
            let rows = recorded(&state).await;
            assert_eq!(rows.len(), 1);
            let forecast: serde_json::Value = serde_json::from_str(&rows[0].forecast_json).unwrap();
            assert_eq!(forecast["dispatches_checked_at"], now.timestamp());
            let held: Vec<(i64, f64)> = forecast["ev_dispatch_hours"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| {
                    (
                        h["timestamp"].as_i64().unwrap(),
                        h["held_fraction"].as_f64().unwrap(),
                    )
                })
                .collect();
            assert_eq!(
                held,
                vec![
                    (local_dt(2026, 8, 31, 2, 0).timestamp(), 1.0),
                    (local_dt(2026, 8, 31, 3, 0).timestamp(), 1.0)
                ]
            );
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

    // ---- replan after Octopus moves a dispatch (issue #404) -----------------

    /// Auto-apply on, with the Octopus dispatch hold wanted.
    fn replan_settings() -> Settings {
        Settings {
            octopus_api_key: "sk_test".into(),
            octopus_account_number: "A-TEST1".into(),
            octopus_dispatch_hold: true,
            ..auto_apply_settings()
        }
    }

    /// A fresh dispatch list checked at `now`, as a new revision.
    async fn fresh_list(state: &Arc<AppState>, now: DateTime<Local>) -> u64 {
        let mut current = state.octopus_dispatches.lock().await;
        current.reset();
        current.checked_at = Some(now.with_timezone(&Utc));
        drop(current);
        crate::octopus_dispatches::planning_dispatches(
            state,
            &replan_settings(),
            now.with_timezone(&Utc),
        )
        .await
        .revision
    }

    /// The cycle auto-apply leaves after writing 02:00–04:00 at `written`,
    /// planned with dispatch list `revision`.
    async fn applied_cycle(state: &Arc<AppState>, written: DateTime<Local>, revision: u64) {
        let queued = apply_plan_recommendation(
            state,
            &gen2(),
            &charge(120, 240, false, 3.0),
            PlanTrigger::Refresh,
        )
        .await;
        let mut plan = computed(charge(120, 240, false, 3.0)).await;
        plan.dispatch_revision = Some(revision);
        let cycle = CycleFor {
            trigger: PlanTrigger::AutoApply,
            now: written,
            running: None,
        };
        start_cycle(state, &replan_settings(), cycle, queued, &plan).await;
        state.pending_writes.lock().await.clear();
    }

    /// A computed plan saying `rec`, with whatever forecast an empty state
    /// gives (only plan history stores it).
    async fn computed(rec: PlanRecommendation) -> ComputedPlan {
        let empty = Arc::new(AppState::new());
        let mut plan = compute_plan(
            &empty,
            &Settings::default(),
            &gen2(),
            local_dt(2026, 8, 31, 1, 0),
            None,
        )
        .await
        .unwrap();
        plan.rec = rec;
        plan
    }

    /// A top-up during a smart charge, as #434 leaves one while it runs.
    fn top_up_run() -> crate::inverter::dispatch_charge::DispatchChargeRun {
        crate::inverter::dispatch_charge::DispatchChargeRun {
            phase: crate::inverter::dispatch_charge::DispatchChargePhase::Charging,
            revert: crate::server::api::force_charge_revert_from_snapshot(
                &InverterSnapshot {
                    inverter_serial: "SA1234".into(),
                    ..gen2()
                },
                DeviceType::Gen2Hybrid,
            )
            .unwrap(),
            target_soc: 100,
            writes: Vec::new(),
            dispatch_end: Utc::now(),
            skew_minutes: 0,
            last_sent: 0,
            unconfirmed: 0,
            restore_started: None,
        }
    }

    async fn replan(state: &Arc<AppState>, now: DateTime<Local>, rec: PlanRecommendation) {
        let revision = crate::octopus_dispatches::planning_dispatches(
            state,
            &replan_settings(),
            now.with_timezone(&Utc),
        )
        .await
        .revision;
        let mut plan = computed(rec).await;
        plan.dispatch_revision = Some(revision);
        // Before the window it plans the next one; inside it, the rest of
        // the one under way.
        let expected = state
            .forecast_plan_cycle
            .lock()
            .await
            .as_ref()
            .and_then(|cycle| {
                (now.timestamp() >= cycle.window_start).then_some(cycle.window_start)
            });
        run_plan_replan_with(
            state,
            &replan_settings(),
            &gen2(),
            now,
            false,
            |running| async move {
                assert_eq!(running, expected, "which window the replan plans");
                Ok(plan)
            },
        )
        .await;
    }

    async fn replan_must_not_compute(state: &Arc<AppState>, now: DateTime<Local>, sit_out: bool) {
        run_plan_replan_with(
            state,
            &replan_settings(),
            &gen2(),
            now,
            sit_out,
            |_| async {
                panic!("the replan computed a plan it had no reason to");
            },
        )
        .await;
    }

    #[tokio::test]
    async fn auto_apply_leaves_a_cycle_for_tonights_window() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let written = local_dt(2026, 8, 31, 1, 30);
            applied_cycle(&state, written, 7).await;
            let cycle = state.forecast_plan_cycle.lock().await.clone().unwrap();
            assert_eq!(cycle.trigger, PlanTrigger::AutoApply);
            assert_eq!(cycle.window_start_hhmm, 200);
            assert_eq!(cycle.window_start, local_dt(2026, 8, 31, 2, 0).timestamp());
            assert_eq!(cycle.window_end, local_dt(2026, 8, 31, 5, 0).timestamp());
            assert_eq!(cycle.dispatch_revision, Some(7));
            assert_eq!(
                cycle.slot_writes,
                vec![
                    RegisterWrite {
                        address: 94,
                        value: 200
                    },
                    RegisterWrite {
                        address: 95,
                        value: 400
                    },
                ]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_moved_dispatch_rewrites_the_slot_and_records_it() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let now = local_dt(2026, 8, 31, 1, 40);
            let first = fresh_list(&state, local_dt(2026, 8, 31, 1, 30)).await;
            applied_cycle(&state, local_dt(2026, 8, 31, 1, 30), first).await;

            let moved = fresh_list(&state, now).await;
            replan(&state, now, charge(120, 310, false, 4.5)).await;

            let batches = queued(&state).await;
            assert_eq!(batches.len(), 1, "{batches:?}");
            assert!(batches[0].contains(&(94, 200)) && batches[0].contains(&(95, 510)));
            let cycle = state.forecast_plan_cycle.lock().await.clone().unwrap();
            assert_eq!(cycle.dispatch_revision, Some(moved));
            assert_eq!(
                cycle.slot,
                SlotSummary::Charge {
                    start_hhmm: 200,
                    end_hhmm: 510
                }
            );
            let rows = recorded(&state).await;
            assert_eq!(rows.len(), 1, "{rows:?}");
            assert_eq!(rows[0].kind, "replan");

            // The same list again: nothing more to do.
            replan_must_not_compute(&state, local_dt(2026, 8, 31, 1, 43), false).await;
        })
        .await;
    }

    #[tokio::test]
    async fn inside_the_window_the_rest_of_it_is_replanned() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            applied_cycle(&state, local_dt(2026, 8, 31, 1, 30), 0).await;
            // 03:05, an hour into the 02:00–05:00 window, the car's charge
            // moved and the battery needs charging on to 04:30.
            let now = local_dt(2026, 8, 31, 3, 5);
            fresh_list(&state, now).await;
            replan(&state, now, charge(120, 270, false, 6.0)).await;
            let batches = queued(&state).await;
            assert_eq!(batches.len(), 1, "{batches:?}");
            assert!(batches[0].contains(&(94, 200)) && batches[0].contains(&(95, 430)));

            // It moves again at 03:52: mid-hour, so the change waits...
            fresh_list(&state, local_dt(2026, 8, 31, 3, 52)).await;
            replan_must_not_compute(&state, local_dt(2026, 8, 31, 3, 52), false).await;
            // ...for 04:02, when the battery has enough: the slot is
            // cleared, stopping the charge.
            let later = local_dt(2026, 8, 31, 4, 2);
            replan(&state, later, no_charge()).await;
            let batches = queued(&state).await;
            assert_eq!(batches.len(), 2, "{batches:?}");
            assert!(batches[1].contains(&(94, 0)) && batches[1].contains(&(96, 0)));

            // At the window's end the cycle is over.
            replan_must_not_compute(&state, local_dt(2026, 8, 31, 5, 0), false).await;
            assert!(state.forecast_plan_cycle.lock().await.is_none());
        })
        .await;
    }

    /// Intelligent Octopus Go: 23:30–05:30, split across midnight.
    fn iog_tariff() -> TariffConfig {
        let slot = |s: &str, e: &str, r: f64| TariffSlot {
            start: s.into(),
            end: e.into(),
            rate: r,
        };
        TariffConfig {
            slots: vec![
                slot("00:00", "05:30", 0.07),
                slot("05:30", "23:30", 0.30),
                slot("23:30", "23:59", 0.07),
            ],
        }
    }

    /// Run the real forecast and planner behind the replan: a week of
    /// history, sun from 08:00 to 16:00 only and a battery at `soc`, inside a running
    /// cheap window whose daily write was at `written`. Returns charge slot
    /// 1's start and end as queued, and the unanchored plan (sized for the
    /// next occurrence instead) for contrast.
    async fn real_replan_inside(
        tariff: TariffConfig,
        written: DateTime<Local>,
        now: DateTime<Local>,
        soc: u8,
    ) -> ((u16, u16), PlanRecommendation) {
        let (state, settings, snapshot) = seeded_plan_state(tariff, now, soc).await;
        let unanchored = compute_plan(&state, &settings, &snapshot, now, None)
            .await
            .unwrap()
            .rec;
        real_replan_continue(state, settings, snapshot, written, now, unanchored).await
    }

    /// A week of history, sun from 08:00 to 16:00 only (forecast fetched at
    /// `fetched`) and a 10 kWh battery at `soc`, for the real planner.
    async fn seeded_plan_state(
        tariff: TariffConfig,
        fetched: DateTime<Local>,
        soc: u8,
    ) -> (Arc<AppState>, Settings, InverterSnapshot) {
        let now = fetched;
        let state = state_with_history().await;
        {
            let db = state.history.lock().await.clone().unwrap();
            crate::forecast::tests::seed_full_forecast_state(&db, now);
            let now_ts = now.timestamp();
            let hour_start = now_ts - now_ts.rem_euclid(3600);
            for h in 0..72i64 {
                let ts = hour_start + h * 3600;
                let hour = chrono::DateTime::from_timestamp(ts, 0)
                    .unwrap()
                    .with_timezone(&Local)
                    .hour();
                db.insert_forecast_values(&[crate::history::ForecastValueRow {
                    timestamp: ts,
                    variable: "shortwave_radiation".to_string(),
                    value: if (8..16).contains(&hour) { 500.0 } else { 0.0 },
                    source: "open-meteo".to_string(),
                    fetched_at: now_ts,
                }])
                .unwrap();
            }
        }
        let settings = Settings {
            pv1_rated_kw: 5.0,
            import_tariff_config: Some(tariff),
            ..replan_settings()
        };
        let snapshot = InverterSnapshot {
            soc,
            battery_capacity_kwh: 10.0,
            max_battery_power_w: 3600,
            charge_rate: 50,
            discharge_rate: 50,
            battery_reserve: 10,
            ..gen2()
        };
        (state, settings, snapshot)
    }

    async fn real_replan_continue(
        state: Arc<AppState>,
        settings: Settings,
        snapshot: InverterSnapshot,
        written: DateTime<Local>,
        now: DateTime<Local>,
        unanchored: PlanRecommendation,
    ) -> ((u16, u16), PlanRecommendation) {
        // The daily write's cycle, for the window that is now running.
        let queued_slot = apply_plan_recommendation(
            &state,
            &snapshot,
            &charge(0, 30, false, 1.0),
            PlanTrigger::Refresh,
        )
        .await;
        let mut written_plan = computed(charge(0, 30, false, 1.0)).await;
        written_plan.dispatch_revision = Some(0);
        let cycle = CycleFor {
            trigger: PlanTrigger::AutoApply,
            now: written,
            running: None,
        };
        start_cycle(&state, &settings, cycle, queued_slot, &written_plan).await;
        state.pending_writes.lock().await.clear();
        fresh_list(&state, now).await;
        run_plan_replan(&state, &settings, &snapshot, now, false).await;

        let batches = queued(&state).await;
        assert_eq!(batches.len(), 1, "{batches:?}");
        let rows = recorded(&state).await;
        assert_eq!(rows.last().map(|row| row.kind.as_str()), Some("replan"));
        let register = |address: u16| {
            batches[0]
                .iter()
                .find(|(a, _)| *a == address)
                .map(|(_, value)| *value)
                .unwrap()
        };
        ((register(94), register(95)), unanchored)
    }

    /// The daily write at 23:05 for Intelligent Octopus Go's 23:30 window:
    /// the forecast's hours start at the next full hour, so without the
    /// current one the plan couldn't see the charge from 23:30 to midnight
    /// and sized the slot up to half an hour long. It must match the plan
    /// made at 23:00 sharp, whose series starts at 23:00.
    #[tokio::test]
    async fn the_daily_plan_sees_a_window_starting_later_this_hour() {
        with_isolated_config_dir_async(|| async {
            let at_the_hour = local_dt(2026, 8, 30, 23, 0);
            let (state, settings, snapshot) =
                seeded_plan_state(iog_tariff(), at_the_hour, 30).await;
            let end = |rec: &PlanRecommendation| match rec {
                PlanRecommendation::Charge { window, .. } => {
                    assert_eq!(window.start_min, 23 * 60 + 30, "{window:?}");
                    window.end_min
                }
                other => panic!("expected a charge, got {other:?}"),
            };
            let sharp = compute_plan(&state, &settings, &snapshot, at_the_hour, None)
                .await
                .unwrap()
                .rec;
            let later = compute_plan(
                &state,
                &settings,
                &snapshot,
                local_dt(2026, 8, 30, 23, 5),
                None,
            )
            .await
            .unwrap()
            .rec;
            let (sharp_end, later_end) = (end(&sharp), end(&later));
            assert!(
                sharp_end.abs_diff(later_end) <= 5,
                "23:00 plans to {sharp_end}, 23:05 to {later_end}"
            );
        })
        .await;
    }

    /// Anchored at the window's start, the real planner sizes the rest of
    /// tonight's charge; unanchored it would size tomorrow's.
    #[tokio::test]
    async fn the_real_planner_sizes_the_rest_of_a_running_window() {
        with_isolated_config_dir_async(|| async {
            let ((start, end), unanchored) = real_replan_inside(
                flux_tariff(),
                local_dt(2026, 8, 31, 1, 30),
                local_dt(2026, 8, 31, 3, 5),
                20,
            )
            .await;
            assert!(
                matches!(&unanchored, PlanRecommendation::Charge { window, .. } if window.tomorrow),
                "unanchored, it sizes tomorrow's window: {unanchored:?}"
            );
            assert_eq!(start, 200, "keeps the start");
            assert!(
                (311..=500).contains(&end),
                "charges on from 03:10 inside tonight's window: ends {end}"
            );
        })
        .await;
    }

    /// The same across midnight: at 01:10 the running window began at
    /// 23:30 the evening before.
    #[tokio::test]
    async fn the_real_planner_sizes_the_rest_of_a_window_that_crossed_midnight() {
        with_isolated_config_dir_async(|| async {
            let ((start, end), unanchored) = real_replan_inside(
                iog_tariff(),
                local_dt(2026, 8, 30, 23, 0),
                local_dt(2026, 8, 31, 1, 5),
                30,
            )
            .await;
            assert_eq!(start, 2330, "keeps the start");
            assert!(
                (111..530).contains(&end),
                "charges on from 01:10, short of the window's end: ends {end}"
            );
            // Unanchored, the planner sizes tomorrow night's 23:30 (the
            // flag can't say so: 23:30 is later than 01:10 on the same
            // day), after a day's drain: a longer charge.
            let PlanRecommendation::Charge { window, .. } = &unanchored else {
                panic!("{unanchored:?}");
            };
            let unanchored_end = (window.end_min / 60) * 100 + window.end_min % 60;
            assert!(unanchored_end > end, "{unanchored:?} vs {end}");
        })
        .await;
    }

    #[tokio::test]
    async fn a_small_move_writes_nothing_but_is_not_recomputed() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let now = local_dt(2026, 8, 31, 1, 40);
            applied_cycle(&state, local_dt(2026, 8, 31, 1, 30), 0).await;
            let moved = fresh_list(&state, now).await;
            replan(&state, now, charge(120, 250, false, 3.3)).await;
            assert!(queued(&state).await.is_empty());
            let cycle = state.forecast_plan_cycle.lock().await.clone().unwrap();
            assert_eq!(cycle.dispatch_revision, Some(moved));
            assert!(matches!(
                cycle.slot,
                SlotSummary::Charge { end_hhmm: 400, .. }
            ));
            replan_must_not_compute(&state, local_dt(2026, 8, 31, 1, 43), false).await;
        })
        .await;
    }

    #[tokio::test]
    async fn a_gated_change_stays_pending_until_the_gate_lifts() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let now = local_dt(2026, 8, 31, 1, 40);
            applied_cycle(&state, local_dt(2026, 8, 31, 1, 30), 0).await;
            fresh_list(&state, now).await;

            replan_must_not_compute(&state, now, true).await;

            state.dispatch_charge.lock().await.run = Some(top_up_run());
            // The top-up's own slot 1 reads back meanwhile: not counted.
            let mut top_up_slot = gen2();
            top_up_slot.timestamp = now.timestamp();
            replan_must_not_compute_with(&state, &top_up_slot, now).await;
            replan_must_not_compute_with(&state, &top_up_slot, now).await;
            assert_eq!(
                state
                    .forecast_plan_cycle
                    .lock()
                    .await
                    .as_ref()
                    .map(|c| c.mismatched_reads),
                Some(0)
            );
            state.dispatch_charge.lock().await.run = None;

            replan(&state, local_dt(2026, 8, 31, 1, 45), no_charge()).await;
            let batches = queued(&state).await;
            assert_eq!(batches.len(), 1, "the change still counted: {batches:?}");
            assert!(batches[0].contains(&(94, 0)) && batches[0].contains(&(95, 0)));
        })
        .await;
    }

    #[tokio::test]
    async fn a_slot_someone_else_changed_is_left_alone() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            applied_cycle(&state, local_dt(2026, 8, 31, 1, 30), 0).await;
            // Octopus has moved a charge, so a replan is pending...
            fresh_list(&state, local_dt(2026, 8, 31, 1, 40)).await;
            // ...but reads after the plan's writes drained show another slot.
            let read = |minute: u32| {
                let mut snapshot = gen2();
                snapshot.timestamp = local_dt(2026, 8, 31, 1, minute).timestamp();
                snapshot.charge_slots[0] = crate::inverter::model::ScheduleSlot {
                    enabled: true,
                    start_hour: 1,
                    start_minute: 0,
                    end_hour: 6,
                    end_minute: 0,
                    target_soc: 100,
                };
                snapshot
            };
            for minute in [40, 41] {
                run_plan_replan_with(
                    &state,
                    &replan_settings(),
                    &read(minute),
                    local_dt(2026, 8, 31, 1, minute),
                    false,
                    |_| async {
                        panic!("must not plan over someone else's slot");
                    },
                )
                .await;
                // One stale read doesn't end the cycle; two in a row do.
                let cycle = state.forecast_plan_cycle.lock().await.clone();
                assert_eq!(
                    cycle.is_some(),
                    minute == 40,
                    "after the read at 01:{minute}"
                );
            }
            assert!(queued(&state).await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_read_that_shows_the_plans_slot_again_clears_a_stale_one() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            applied_cycle(&state, local_dt(2026, 8, 31, 1, 30), 0).await;
            let mut stale = gen2();
            stale.timestamp = local_dt(2026, 8, 31, 1, 40).timestamp();
            replan_must_not_compute_with(&state, &stale, local_dt(2026, 8, 31, 1, 40)).await;
            assert_eq!(
                state
                    .forecast_plan_cycle
                    .lock()
                    .await
                    .as_ref()
                    .map(|c| c.mismatched_reads),
                Some(1)
            );
            // The plan's own 02:00–04:00 reads back.
            let mut good = gen2();
            good.timestamp = local_dt(2026, 8, 31, 1, 41).timestamp();
            good.charge_slots[0] = crate::inverter::model::ScheduleSlot {
                enabled: true,
                start_hour: 2,
                start_minute: 0,
                end_hour: 4,
                end_minute: 0,
                target_soc: 100,
            };
            replan_must_not_compute_with(&state, &good, local_dt(2026, 8, 31, 1, 41)).await;
            assert_eq!(
                state
                    .forecast_plan_cycle
                    .lock()
                    .await
                    .as_ref()
                    .map(|c| c.mismatched_reads),
                Some(0)
            );
        })
        .await;
    }

    async fn replan_must_not_compute_with(
        state: &Arc<AppState>,
        snapshot: &InverterSnapshot,
        now: DateTime<Local>,
    ) {
        run_plan_replan_with(state, &replan_settings(), snapshot, now, false, |_| async {
            panic!("nothing changed to plan for");
        })
        .await;
    }

    #[tokio::test]
    async fn the_cycle_ends_when_auto_apply_is_switched_off() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let now = local_dt(2026, 8, 31, 1, 40);
            applied_cycle(&state, local_dt(2026, 8, 31, 1, 30), 0).await;
            fresh_list(&state, now).await;
            let settings = Settings {
                forecast_plan_auto_apply_enabled: false,
                ..replan_settings()
            };
            run_plan_replan_with(&state, &settings, &gen2(), now, false, |_| async {
                panic!("auto-apply is off");
            })
            .await;
            assert!(state.forecast_plan_cycle.lock().await.is_none());
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

    // ---- Every cheap window (issue #453) ----

    fn cosy_tariff() -> TariffConfig {
        let slot = |s: &str, e: &str, r: f64| TariffSlot {
            start: s.into(),
            end: e.into(),
            rate: r,
        };
        TariffConfig {
            slots: vec![
                slot("00:00", "04:00", 0.27),
                slot("04:00", "07:00", 0.13),
                slot("07:00", "13:00", 0.27),
                slot("13:00", "16:00", 0.13),
                slot("16:00", "19:00", 0.40),
                slot("19:00", "22:00", 0.27),
                slot("22:00", "23:59", 0.13),
            ],
        }
    }

    fn every_window(mut settings: Settings) -> Settings {
        settings.import_tariff_config = Some(cosy_tariff());
        settings.forecast_plan_every_cheap_window = true;
        settings
    }

    /// A poll every five minutes through one day from midnight.
    fn polls_through_the_day() -> impl Iterator<Item = DateTime<Local>> {
        let midnight = local_dt(2026, 1, 12, 0, 0);
        (0..288).map(move |i| midnight + chrono::Duration::minutes(i * 5))
    }

    fn kinds(rows: &[crate::history::PlanSnapshotRow], kind: &str) -> Vec<i64> {
        rows.iter()
            .filter(|row| row.kind == kind)
            .map(|row| row.recorded_at)
            .collect()
    }

    #[tokio::test]
    async fn the_refresh_plans_each_cosy_window_once() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let settings = every_window(auto_refresh_settings());
            for now in polls_through_the_day() {
                run_plan_refresh(&state, &settings, &gen2(), now).await;
            }
            assert_eq!(
                kinds(&recorded(&state).await, "refresh"),
                vec![
                    local_dt(2026, 1, 12, 3, 30).timestamp(),
                    local_dt(2026, 1, 12, 12, 30).timestamp(),
                    local_dt(2026, 1, 12, 21, 30).timestamp(),
                ]
            );
            let day = NaiveDate::from_ymd_opt(2026, 1, 12).unwrap();
            assert_eq!(
                *state.forecast_plan_refresh_window.lock().await,
                Some((day, 22 * 60))
            );
            // The once-a-day latch is left alone: none of these plans was
            // the once-a-day plan.
            assert_eq!(*state.forecast_plan_refresh_date.lock().await, None);
        })
        .await;
    }

    #[tokio::test]
    async fn the_auto_apply_plans_each_cosy_window_once() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let mut settings = every_window(auto_apply_settings());
            settings.forecast_plan_auto_apply_lead_minutes = 120;
            for now in polls_through_the_day() {
                run_plan_auto_apply(&state, &settings, &gen2(), now).await;
            }
            assert_eq!(
                kinds(&recorded(&state).await, "auto_apply"),
                vec![
                    local_dt(2026, 1, 12, 2, 0).timestamp(),
                    local_dt(2026, 1, 12, 11, 0).timestamp(),
                    local_dt(2026, 1, 12, 20, 0).timestamp(),
                ]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_window_that_started_unplanned_is_latched_and_left_alone() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let settings = every_window(auto_refresh_settings());
            // Waking inside the 13:00 window.
            let now = local_dt(2026, 1, 12, 13, 20);
            run_plan_refresh(&state, &settings, &gen2(), now).await;
            run_plan_refresh(
                &state,
                &settings,
                &gen2(),
                now + chrono::Duration::minutes(5),
            )
            .await;
            assert!(recorded(&state).await.is_empty());
            assert!(state.pending_writes.lock().await.is_empty());
            assert_eq!(
                *state.forecast_plan_refresh_window.lock().await,
                Some((NaiveDate::from_ymd_opt(2026, 1, 12).unwrap(), 13 * 60))
            );
            assert_eq!(*state.forecast_plan_refresh_date.lock().await, None);
        })
        .await;
    }

    #[tokio::test]
    async fn switching_it_on_never_plans_the_same_window_twice() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            // The once-a-day refresh plans Cosy's 04:00 at 03:30...
            let mut settings = auto_refresh_settings();
            settings.import_tariff_config = Some(cosy_tariff());
            run_plan_refresh(&state, &settings, &gen2(), local_dt(2026, 1, 12, 3, 30)).await;
            // ...then the user switches every window on in the same lead.
            let on = every_window(settings.clone());
            run_plan_refresh(&state, &on, &gen2(), local_dt(2026, 1, 12, 3, 40)).await;
            assert_eq!(kinds(&recorded(&state).await, "refresh").len(), 1);
            // The next window is still planned.
            run_plan_refresh(&state, &on, &gen2(), local_dt(2026, 1, 12, 12, 40)).await;
            assert_eq!(kinds(&recorded(&state).await, "refresh").len(), 2);
            // Switched off again, the 22:00 window is planned: the 13:00
            // plan only had to last until it.
            run_plan_refresh(&state, &settings, &gen2(), local_dt(2026, 1, 12, 21, 30)).await;
            assert_eq!(kinds(&recorded(&state).await, "refresh").len(), 3);
            // But not twice.
            run_plan_refresh(&state, &settings, &gen2(), local_dt(2026, 1, 12, 21, 40)).await;
            assert_eq!(kinds(&recorded(&state).await, "refresh").len(), 3);
        })
        .await;
    }

    #[tokio::test]
    async fn adaptive_charge_still_owns_the_rate_with_every_window() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let settings = Settings {
                adaptive_charge_enabled: true,
                ..every_window(auto_apply_settings())
            };
            for now in polls_through_the_day() {
                run_plan_auto_apply(&state, &settings, &gen2(), now).await;
            }
            assert!(recorded(&state).await.is_empty());
            assert!(state.pending_writes.lock().await.is_empty());
            // Warned once for the day, not per window.
            assert_eq!(
                *state.forecast_plan_apply_warned.lock().await,
                Some(NaiveDate::from_ymd_opt(2026, 1, 12).unwrap())
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_zero_lead_auto_apply_plans_each_cosy_window_at_its_start() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let mut settings = every_window(auto_apply_settings());
            settings.forecast_plan_auto_apply_lead_minutes = 0;
            for now in polls_through_the_day() {
                run_plan_auto_apply(&state, &settings, &gen2(), now).await;
            }
            assert_eq!(
                kinds(&recorded(&state).await, "auto_apply"),
                vec![
                    local_dt(2026, 1, 12, 4, 0).timestamp(),
                    local_dt(2026, 1, 12, 13, 0).timestamp(),
                    local_dt(2026, 1, 12, 22, 0).timestamp(),
                ]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_window_planned_under_way_leaves_its_own_cycle() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let settings = every_window(auto_apply_settings());
            let queued = apply_plan_recommendation(
                &state,
                &gen2(),
                &charge(13 * 60, 15 * 60, false, 3.0),
                PlanTrigger::AutoApply,
            )
            .await;
            let plan = computed(charge(13 * 60, 15 * 60, false, 3.0)).await;
            // Planned at 13:05 for the 13:00 window under way, not the
            // 22:00 one a plan from 13:05 would otherwise be for.
            let cycle = CycleFor {
                trigger: PlanTrigger::AutoApply,
                now: local_dt(2026, 1, 12, 13, 5),
                running: Some(local_dt(2026, 1, 12, 13, 0).timestamp()),
            };
            start_cycle(&state, &settings, cycle, queued, &plan).await;
            let cycle = state
                .forecast_plan_cycle
                .lock()
                .await
                .clone()
                .expect("a cycle");
            assert_eq!(cycle.window_start, local_dt(2026, 1, 12, 13, 0).timestamp());
            assert_eq!(cycle.window_end, local_dt(2026, 1, 12, 16, 0).timestamp());
        })
        .await;
    }

    /// A cheap period opening just after midnight, 00:20–05:00.
    fn after_midnight_tariff() -> TariffConfig {
        let slot = |s: &str, e: &str, r: f64| TariffSlot {
            start: s.into(),
            end: e.into(),
            rate: r,
        };
        TariffConfig {
            slots: vec![
                slot("00:00", "00:20", 0.27),
                slot("00:20", "05:00", 0.09),
                slot("05:00", "23:59", 0.27),
            ],
        }
    }

    /// A poll every five minutes from 22:00 for four hours.
    fn polls_over_midnight() -> impl Iterator<Item = DateTime<Local>> {
        let start = local_dt(2026, 1, 12, 22, 0);
        (0..48).map(move |i| start + chrono::Duration::minutes(i * 5))
    }

    #[tokio::test]
    async fn the_refresh_plans_an_iog_night_once() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let mut settings = auto_refresh_settings();
            settings.import_tariff_config = Some(iog_tariff());
            for now in polls_over_midnight() {
                run_plan_refresh(&state, &settings, &gen2(), now).await;
            }
            // Not again at midnight, when the date moves on mid-window.
            assert_eq!(
                kinds(&recorded(&state).await, "refresh"),
                vec![local_dt(2026, 1, 12, 23, 0).timestamp()]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn the_auto_apply_plans_a_window_after_midnight_once() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let mut settings = auto_apply_settings();
            settings.import_tariff_config = Some(after_midnight_tariff());
            settings.forecast_plan_auto_apply_lead_minutes = 120;
            for now in polls_over_midnight() {
                run_plan_auto_apply(&state, &settings, &gen2(), now).await;
            }
            assert_eq!(
                kinds(&recorded(&state).await, "auto_apply"),
                vec![local_dt(2026, 1, 12, 22, 20).timestamp()]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn switching_it_off_after_midnight_never_plans_the_window_again() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let mut off = auto_refresh_settings();
            off.import_tariff_config = Some(after_midnight_tariff());
            let on = Settings {
                forecast_plan_every_cheap_window: true,
                ..off.clone()
            };
            // Every window plans the 00:20 period at 23:50...
            run_plan_refresh(&state, &on, &gen2(), local_dt(2026, 1, 12, 23, 50)).await;
            // ...and the user switches it off at 00:05.
            run_plan_refresh(&state, &off, &gen2(), local_dt(2026, 1, 13, 0, 5)).await;
            assert_eq!(kinds(&recorded(&state).await, "refresh").len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn switching_it_off_still_snapshots_the_next_window() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let on = every_window(snapshot_settings());
            let off = Settings {
                forecast_plan_every_cheap_window: false,
                ..on.clone()
            };
            run_plan_snapshot(&state, &on, &gen2(), local_dt(2026, 1, 12, 12, 30)).await;
            run_plan_snapshot(&state, &off, &gen2(), local_dt(2026, 1, 12, 21, 30)).await;
            // The 22:00 window once, not again on switching back on.
            run_plan_snapshot(&state, &on, &gen2(), local_dt(2026, 1, 12, 21, 40)).await;
            assert_eq!(
                kinds(&recorded(&state).await, "before_window"),
                vec![
                    local_dt(2026, 1, 12, 12, 30).timestamp(),
                    local_dt(2026, 1, 12, 21, 30).timestamp(),
                ]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn the_snapshot_records_each_cosy_window_once() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let settings = every_window(snapshot_settings());
            for now in polls_through_the_day() {
                run_plan_snapshot(&state, &settings, &gen2(), now).await;
            }
            assert_eq!(
                kinds(&recorded(&state).await, "before_window"),
                vec![
                    local_dt(2026, 1, 12, 3, 30).timestamp(),
                    local_dt(2026, 1, 12, 12, 30).timestamp(),
                    local_dt(2026, 1, 12, 21, 30).timestamp(),
                ]
            );
        })
        .await;
    }
}
