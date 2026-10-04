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

use chrono::{DateTime, Local, NaiveDate};

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
}

/// AC kWh a recommendation asks to charge (0 when it asks for none).
pub(crate) fn rec_kwh(rec: &PlanRecommendation) -> f64 {
    match rec {
        PlanRecommendation::Charge { kwh, .. } => *kwh,
        _ => 0.0,
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
        build_plan_applied_message, build_plan_cleared_message, build_plan_unavailable_message,
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
    let plan = plan_slot_plan(rec, snapshot);
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
) -> Result<PlanRecommendation, tokio::task::JoinError> {
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
        crate::server::api::compute_plan_recommendation(
            &forecast,
            &settings,
            Some(&live_snapshot),
            now.timestamp(),
        )
    })
    .await
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
            Ok(rec) => apply_plan_recommendation(state, snapshot, &rec, PlanTrigger::Refresh).await,
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
                Ok(rec) => {
                    apply_plan_recommendation(state, snapshot, &rec, PlanTrigger::AutoApply).await
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
            SlotPlan::Write { writes, .. } | SlotPlan::Clear { writes } => writes,
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
}
