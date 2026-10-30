//! `/api/battery/calibration`: start, follow and cancel the guided battery
//! calibration cycle (issue #418). The cycle itself runs in the poll loop
//! (`inverter::calibration`); these handlers only create or cancel the
//! persisted run.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, Json};
use chrono::Timelike;
use serde_json::{json, Value};

use crate::inverter::calibration::{
    self, CalibrationRun, DEFAULT_HOLD_MINUTES, FLOOR_SOC, MAX_HOLD_MINUTES, MIN_HOLD_MINUTES,
};
use crate::inverter::poll::{AppState, ConnectionState};

fn reply(status: StatusCode, error: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "ok": false, "error": error })))
}

/// The run as the Control page sees it: the phase (with its own fields) and
/// timings, not the register lists.
fn view(run: &CalibrationRun) -> Value {
    let mut value = serde_json::to_value(&run.phase).unwrap_or_else(|_| json!({}));
    if let Some(object) = value.as_object_mut() {
        object.remove("attempts");
        object.insert("started_at_ms".into(), json!(run.started_at_ms));
        object.insert("phase_started_at_ms".into(), json!(run.phase_started_at_ms));
        object.insert("hold_minutes".into(), json!(run.hold_minutes));
    }
    value
}

/// GET /api/battery/calibration — whether the cycle is available on this
/// inverter, the running cycle if any, and how the last one ended.
pub async fn get_calibration(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let supported = state
        .latest_snapshot
        .lock()
        .await
        .as_ref()
        .is_some_and(|snap| calibration::supports(snap.device_type));
    let run = state.calibration_run.lock().await.clone();
    let last =
        tokio::task::spawn_blocking(|| crate::settings::Settings::load().battery_calibration_last)
            .await
            .unwrap_or_default();
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "supported": supported,
            "running": run.is_some(),
            "calibration": run.as_ref().map(view),
            "last": last,
            "default_hold_minutes": DEFAULT_HOLD_MINUTES,
            "min_hold_minutes": MIN_HOLD_MINUTES,
            "max_hold_minutes": MAX_HOLD_MINUTES,
            "floor_soc": FLOOR_SOC,
        })),
    )
}

/// What already holds the battery, if anything, so the cycle would capture
/// someone else's temporary state as the settings to restore.
async fn blocker(
    state: &AppState,
    snap: &crate::inverter::model::InverterSnapshot,
) -> Option<&'static str> {
    if state.force_charge_revert.lock().await.is_some()
        || state.force_charge_restoration.lock().await.is_some()
    {
        return Some("Stop Force Charge before starting a calibration");
    }
    if state.force_discharge_revert.lock().await.is_some()
        || state.force_discharge_restoration.lock().await.is_some()
    {
        return Some("Stop Force Discharge before starting a calibration");
    }
    if state.pause_mode_revert.lock().await.is_some() {
        return Some("Stop the battery pause before starting a calibration");
    }
    if state.load_limiter_state.lock().await.is_actively_pausing()
        || state
            .temperature_limiter_state
            .lock()
            .await
            .is_actively_pausing()
    {
        return Some(
            "A safety limiter is holding the battery; start the calibration once it releases",
        );
    }
    if state.dispatch_hold_state.lock().await.is_actively_pausing() {
        return Some(
            "The Intelligent Octopus Go dispatch hold is pausing the battery; start the calibration once it releases",
        );
    }
    if *state.cosy_active.lock().await || snap.cosy_active {
        return Some("A Cosy charging slot is running; start the calibration after it ends");
    }
    if snap.agile_active {
        return Some("An Agile slot is running; start the calibration after it ends");
    }
    let config = state.timed_export_config.lock().await.clone();
    let machine = state.timed_export_state.lock().await.clone();
    let now = chrono::Local::now();
    let minute = crate::inverter::state_machines::authoritative_minute_of_day(
        snap,
        now.hour() as u16 * 60 + now.minute() as u16,
    );
    if crate::inverter::state_machines::timed_export_claims_discharge(
        &config, &machine, snap, minute,
    ) {
        return Some("A Timed Export window is running; start the calibration after it ends");
    }
    None
}

/// POST /api/battery/calibration — start a cycle. Body: `{"hold_minutes": n}`
/// (optional, default 120). The poll loop applies the writes.
pub async fn start_calibration(
    State(state): State<Arc<AppState>>,
    body: Option<Json<Value>>,
) -> (StatusCode, Json<Value>) {
    let hold_minutes = match body.as_ref().and_then(|Json(b)| b.get("hold_minutes")) {
        None | Some(Value::Null) => DEFAULT_HOLD_MINUTES,
        Some(value) => match value.as_u64().and_then(|m| u32::try_from(m).ok()) {
            Some(minutes) => minutes,
            None => {
                return reply(
                    StatusCode::BAD_REQUEST,
                    "hold_minutes must be a whole number of minutes",
                )
            }
        },
    };
    if let Err(e) = calibration::validate_hold_minutes(hold_minutes) {
        return reply(StatusCode::BAD_REQUEST, &e);
    }

    // Serialised with Force and pause starts, which check for a running cycle
    // under the same lock.
    let _action_guard = state.force_action_lock.lock().await;
    if matches!(
        *state.connection_state.lock().await,
        ConnectionState::Disconnected
    ) {
        return reply(StatusCode::CONFLICT, "The inverter is not connected");
    }
    let Some(snap) = state.latest_snapshot.lock().await.clone() else {
        return reply(
            StatusCode::CONFLICT,
            "No reading from the inverter yet; try again shortly",
        );
    };
    if !calibration::supports(snap.device_type) {
        return reply(
            StatusCode::BAD_REQUEST,
            &format!(
                "The calibration cycle isn't available for this inverter ({})",
                snap.device_type.display_name()
            ),
        );
    }
    let mut run_guard = state.calibration_run.lock().await;
    if run_guard.is_some() {
        return reply(
            StatusCode::CONFLICT,
            "A calibration cycle is already running",
        );
    }
    if let Some(reason) = blocker(&state, &snap).await {
        return reply(StatusCode::CONFLICT, reason);
    }
    let plan = match calibration::build_plan(&snap) {
        Ok(plan) => plan,
        Err(e) => return reply(StatusCode::BAD_REQUEST, &e),
    };
    let run = calibration::new_run(
        &snap,
        plan,
        hold_minutes,
        chrono::Utc::now().timestamp_millis(),
    );
    if let Err(e) = calibration::persist_run(Some(run.clone())).await {
        return reply(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Couldn't save the calibration, so it wasn't started: {e}"),
        );
    }
    tracing::info!(hold_minutes, "Battery calibration started");
    let body = json!({
        "ok": true,
        "message": "Battery calibration started",
        "calibration": view(&run),
    });
    *run_guard = Some(run);
    (StatusCode::OK, Json(body))
}

/// POST /api/battery/calibration/cancel — stop the cycle and restore the
/// settings it changed. The poll loop writes and confirms the restore.
pub async fn cancel_calibration(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let mut run_guard = state.calibration_run.lock().await;
    let Some(run) = run_guard.as_mut() else {
        return reply(StatusCode::CONFLICT, "No calibration cycle is running");
    };
    if !calibration::cancel(run, chrono::Utc::now().timestamp_millis()) {
        return reply(
            StatusCode::CONFLICT,
            "The calibration is already putting your settings back",
        );
    }
    let body = json!({
        "ok": true,
        "message": "Calibration cancelled; putting your settings back",
        "calibration": view(run),
    });
    if let Err(e) = calibration::persist_run(run_guard.clone()).await {
        // The cancel still takes effect in this process.
        tracing::warn!("Failed to persist the cancelled battery calibration: {e}");
    }
    tracing::info!("Battery calibration cancelled");
    (StatusCode::OK, Json(body))
}

/// Whether a calibration cycle is running; manual actions that would fight
/// it are refused while it is.
pub(crate) async fn calibration_running(state: &AppState) -> bool {
    state.calibration_run.lock().await.is_some()
}

/// The refusal for a manual battery action while a cycle runs.
pub(crate) const CALIBRATION_RUNNING: &str =
    "A battery calibration cycle is running; cancel it on the Control page first";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::calibration::{
        CalibrationOutcome, CalibrationPhase, CalibrationRecord, DEFAULT_HOLD_MINUTES,
    };
    use crate::inverter::model::{DeviceType, InverterSnapshot};
    use crate::inverter::poll::{ConnectionState, LoadLimiterState};
    use crate::test_util::with_isolated_config_dir_async;

    async fn state_with(device_type: DeviceType) -> Arc<AppState> {
        let state = Arc::new(AppState::new());
        *state.latest_snapshot.lock().await = Some(InverterSnapshot {
            device_type,
            inverter_serial: "HEM-TEST-001".to_string(),
            firmware_version: "449".to_string(),
            soc: 60,
            battery_reserve: 20,
            charge_rate: 25,
            discharge_rate: 30,
            battery_power_mode: 1,
            enable_charge: true,
            target_soc: 100,
            ..Default::default()
        });
        *state.connection_state.lock().await = ConnectionState::Connected;
        state
    }

    async fn start(state: &Arc<AppState>, body: Option<Value>) -> (StatusCode, Value) {
        let (status, Json(value)) = start_calibration(State(state.clone()), body.map(Json)).await;
        (status, value)
    }

    async fn status(state: &Arc<AppState>) -> Value {
        let (code, Json(value)) = get_calibration(State(state.clone())).await;
        assert_eq!(code, StatusCode::OK);
        value
    }

    #[tokio::test]
    async fn start_begins_discharging_and_persists_the_run() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(DeviceType::Gen2Hybrid).await;
            let (code, body) = start(&state, None).await;
            assert_eq!(code, StatusCode::OK, "{body}");
            assert_eq!(body["calibration"]["phase"], "discharging");
            assert_eq!(body["calibration"]["hold_minutes"], DEFAULT_HOLD_MINUTES);

            let run = state.calibration_run.lock().await.clone().expect("running");
            assert_eq!(run.phase, CalibrationPhase::Discharging);
            assert_eq!(run.inverter_serial, "HEM-TEST-001");
            assert_eq!(
                crate::settings::Settings::load().battery_calibration_run,
                Some(run)
            );
            // The poll loop applies the writes, under the arbiter.
            assert!(state.pending_writes.lock().await.is_empty());
            assert!(calibration_running(&state).await);
        })
        .await;
    }

    #[tokio::test]
    async fn start_takes_the_hold_time() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(DeviceType::ACCoupled).await;
            let (code, _) = start(&state, Some(json!({"hold_minutes": 180}))).await;
            assert_eq!(code, StatusCode::OK);
            assert_eq!(
                state
                    .calibration_run
                    .lock()
                    .await
                    .as_ref()
                    .map(|r| r.hold_minutes),
                Some(180)
            );
        })
        .await;
    }

    #[tokio::test]
    async fn start_refuses_a_hold_time_out_of_range() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(DeviceType::Gen2Hybrid).await;
            for bad in [
                json!({"hold_minutes": 10}),
                json!({"hold_minutes": 600}),
                json!({"hold_minutes": "2h"}),
            ] {
                let (code, body) = start(&state, Some(bad.clone())).await;
                assert_eq!(code, StatusCode::BAD_REQUEST, "{bad}: {body}");
            }
            assert!(state.calibration_run.lock().await.is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn start_refuses_unsupported_models() {
        with_isolated_config_dir_async(|| async {
            for device in [
                DeviceType::ThreePhase,
                DeviceType::HybridHvGen3,
                DeviceType::Gateway,
            ] {
                let state = state_with(device).await;
                let (code, body) = start(&state, None).await;
                assert_eq!(code, StatusCode::BAD_REQUEST, "{device:?}: {body}");
                assert!(state.calibration_run.lock().await.is_none());
            }
        })
        .await;
    }

    #[tokio::test]
    async fn start_needs_a_reading_from_a_connected_inverter() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let (code, _) = start(&state, None).await;
            assert_eq!(code, StatusCode::CONFLICT);

            let state = state_with(DeviceType::Gen2Hybrid).await;
            *state.connection_state.lock().await = ConnectionState::Disconnected;
            let (code, _) = start(&state, None).await;
            assert_eq!(code, StatusCode::CONFLICT);
            assert!(state.calibration_run.lock().await.is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn start_refuses_while_another_action_holds_the_battery() {
        with_isolated_config_dir_async(|| async {
            // Force Charge running: its capture is not the user's own setup.
            let state = state_with(DeviceType::Gen2Hybrid).await;
            let (code, _) = crate::server::api::force_charge(State(state.clone()), None).await;
            assert_eq!(code, StatusCode::OK);
            let (code, body) = start(&state, None).await;
            assert_eq!(code, StatusCode::CONFLICT, "{body}");

            // A load limiter pause: its Eco Paused state would be captured.
            let state = state_with(DeviceType::Gen2Hybrid).await;
            *state.load_limiter_state.lock().await = LoadLimiterState::Paused;
            let (code, body) = start(&state, None).await;
            assert_eq!(code, StatusCode::CONFLICT, "{body}");
            assert!(state.calibration_run.lock().await.is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn start_refuses_a_second_cycle() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(DeviceType::Gen2Hybrid).await;
            assert_eq!(start(&state, None).await.0, StatusCode::OK);
            let first = state.calibration_run.lock().await.clone();
            assert_eq!(start(&state, None).await.0, StatusCode::CONFLICT);
            assert_eq!(*state.calibration_run.lock().await, first);
        })
        .await;
    }

    #[tokio::test]
    async fn manual_actions_are_refused_while_calibrating() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(DeviceType::Gen2Hybrid).await;
            assert_eq!(start(&state, None).await.0, StatusCode::OK);

            let (code, _) = crate::server::api::force_charge(State(state.clone()), None).await;
            assert_ne!(code, StatusCode::OK);
            let (code, _) = crate::server::api::force_discharge(State(state.clone()), None).await;
            assert_ne!(code, StatusCode::OK);
            let (code, _) = crate::server::api::pause_battery(State(state.clone())).await;
            assert_ne!(code, StatusCode::OK);
            assert!(state.force_charge_revert.lock().await.is_none());
            assert!(state.force_discharge_revert.lock().await.is_none());
            assert!(state.pending_writes.lock().await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn cancel_moves_to_restoring_and_persists() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(DeviceType::Gen2Hybrid).await;
            assert_eq!(start(&state, None).await.0, StatusCode::OK);

            let (code, Json(body)) = cancel_calibration(State(state.clone())).await;
            assert_eq!(code, StatusCode::OK, "{body}");
            let run = state
                .calibration_run
                .lock()
                .await
                .clone()
                .expect("still restoring");
            assert_eq!(
                run.phase,
                CalibrationPhase::Restoring {
                    outcome: CalibrationOutcome::Cancelled,
                    attempts: 0
                }
            );
            assert_eq!(
                crate::settings::Settings::load().battery_calibration_run,
                Some(run)
            );

            // A second cancel has nothing left to stop.
            let (code, _) = cancel_calibration(State(state.clone())).await;
            assert_eq!(code, StatusCode::CONFLICT);
        })
        .await;
    }

    #[tokio::test]
    async fn cancel_without_a_cycle_is_a_conflict() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(DeviceType::Gen2Hybrid).await;
            let (code, _) = cancel_calibration(State(state.clone())).await;
            assert_eq!(code, StatusCode::CONFLICT);
        })
        .await;
    }

    #[tokio::test]
    async fn status_reports_support_the_run_and_the_last_result() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(DeviceType::Gen2Hybrid).await;
            let idle = status(&state).await;
            assert_eq!(idle["supported"], true);
            assert_eq!(idle["running"], false);
            assert_eq!(idle["calibration"], Value::Null);
            assert_eq!(idle["default_hold_minutes"], 120);
            assert_eq!(idle["min_hold_minutes"], 30);
            assert_eq!(idle["max_hold_minutes"], 480);
            assert_eq!(idle["floor_soc"], 4);

            let mut settings = crate::settings::Settings::load();
            settings.battery_calibration_last = Some(CalibrationRecord {
                started_at_ms: 1,
                finished_at_ms: 2,
                outcome: CalibrationOutcome::Failed {
                    reason: "Battery did not reach 4%".to_string(),
                },
            });
            settings.save().expect("save");
            assert_eq!(start(&state, None).await.0, StatusCode::OK);

            let running = status(&state).await;
            assert_eq!(running["running"], true);
            assert_eq!(running["calibration"]["phase"], "discharging");
            assert!(running["calibration"]["started_at_ms"].is_i64());
            assert!(running["calibration"]["phase_started_at_ms"].is_i64());
            assert_eq!(running["last"]["outcome"]["result"], "failed");
            assert_eq!(
                running["last"]["outcome"]["reason"],
                "Battery did not reach 4%"
            );
            // The register lists are internal.
            assert_eq!(running["calibration"]["restore_writes"], Value::Null);

            let three_phase = state_with(DeviceType::ThreePhase).await;
            assert_eq!(status(&three_phase).await["supported"], false);
        })
        .await;
    }
}
