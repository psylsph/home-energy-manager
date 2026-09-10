//! Authenticated adapters for the existing Quick Actions (issue #301).
//!
//! Authentication and write permission are router middleware; these adapters
//! only validate external input, then call exactly the same UI handlers.

use std::sync::Arc;

use axum::{
    extract::{rejection::JsonRejection, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::api;
use crate::{inverter::poll::AppState, settings::Settings};

pub async fn require_control_permission(request: Request, next: Next) -> Response {
    if !Settings::load_async().await.api_control_enabled {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"ok":false,
            "error":"External battery control is disabled"})),
        )
            .into_response();
    }
    next.run(request).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurationRequest {
    minutes: u64,
}

fn duration(
    body: Result<Json<DurationRequest>, JsonRejection>,
) -> Result<Value, (StatusCode, Json<Value>)> {
    match body {
        Ok(Json(body)) if (1..=1439).contains(&body.minutes) => Ok(json!({"minutes":body.minutes})),
        _ => Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"ok":false,
            "error":"Expected only an integer minutes field between 1 and 1439"})),
        )),
    }
}

pub async fn force_charge(
    State(state): State<Arc<AppState>>,
    body: Result<Json<DurationRequest>, JsonRejection>,
) -> (StatusCode, Json<Value>) {
    match duration(body) {
        Ok(body) => api::force_charge(State(state), Some(Json(body))).await,
        Err(error) => error,
    }
}

pub async fn force_discharge(
    State(state): State<Arc<AppState>>,
    body: Result<Json<DurationRequest>, JsonRejection>,
) -> (StatusCode, Json<Value>) {
    match duration(body) {
        Ok(body) => {
            // The UI cannot offer Quick Actions before its first snapshot.
            // External callers have no UI guard, so refuse an unknown restore
            // baseline here rather than using the handler's legacy fallback.
            if state.latest_snapshot.lock().await.is_none() {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({"ok":false,
                    "error":"Force Discharge requires an inverter snapshot before it can start"})),
                );
            }
            api::force_discharge(State(state), Some(Json(body))).await
        }
        Err(error) => error,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        body::{to_bytes, Body},
        extract::State,
        http::{Request, StatusCode},
        Json,
    };
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use crate::server::{api, create_authenticated_router};
    use crate::test_util::with_isolated_config_dir_async;
    use crate::{
        inverter::{
            model::{DeviceType, InverterSnapshot},
            poll::{AppState, ConnectionState},
        },
        settings::Settings,
    };

    const ACTIONS: [&str; 4] = [
        "force-charge",
        "force-charge/stop",
        "force-discharge",
        "force-discharge/stop",
    ];

    async fn setup(enabled: bool) -> Arc<AppState> {
        let mut settings = Settings::load();
        settings.api_key = "integration-key".into();
        settings.save().unwrap();
        let state = Arc::new(AppState::new());
        let (status, _) = api::update_settings(
            State(state.clone()),
            Json(json!({"api_control_enabled": enabled})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        *state.latest_snapshot.lock().await = Some(InverterSnapshot {
            timestamp: 1_800_000_000,
            device_type: DeviceType::ACCoupled,
            ..Default::default()
        });
        state
    }

    async fn request(
        state: Arc<AppState>,
        action: &str,
        token: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri(format!("/api/control/{action}"))
            .header("Content-Type", "application/json");
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let response = create_authenticated_router(state)
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn external_and_ui_concurrent_actions_share_mutual_exclusion() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            let (external, ui) = tokio::join!(
                request(
                    state.clone(),
                    "force-charge",
                    Some("integration-key"),
                    json!({"minutes":30})
                ),
                api::force_discharge(State(state.clone()), Some(Json(json!({"minutes":30}))))
            );
            assert!(matches!(
                (external.0, ui.0),
                (StatusCode::OK, StatusCode::BAD_REQUEST)
                    | (StatusCode::BAD_REQUEST, StatusCode::OK)
            ));
            assert_eq!(state.pending_writes.lock().await.len(), 1);
            assert_ne!(
                state.force_charge_revert.lock().await.is_some(),
                state.force_discharge_revert.lock().await.is_some()
            );
        })
        .await;
    }

    #[tokio::test]
    async fn external_duration_boundaries_and_missing_snapshot_match_ui() {
        with_isolated_config_dir_async(|| async {
            for minutes in [1, 1439] {
                for action in ["force-charge", "force-discharge"] {
                    let state = setup(true).await;
                    assert_eq!(
                        request(
                            state,
                            action,
                            Some("integration-key"),
                            json!({"minutes":minutes})
                        )
                        .await
                        .0,
                        StatusCode::OK
                    );
                }
            }
            let state = setup(true).await;
            *state.latest_snapshot.lock().await = None;
            for action in ["force-charge", "force-discharge"] {
                assert_eq!(
                    request(
                        state.clone(),
                        action,
                        Some("integration-key"),
                        json!({"minutes":30})
                    )
                    .await
                    .0,
                    StatusCode::CONFLICT
                );
            }
            assert!(state.pending_writes.lock().await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn malformed_external_json_is_rejected_and_other_controls_stay_hidden() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            for action in ["force-charge", "force-discharge"] {
                for body in ["{", "{\"minutes\":30,\"minutes\":60}"] {
                    let req = Request::builder()
                        .method("POST")
                        .uri(format!("/api/control/{action}"))
                        .header("Authorization", "Bearer integration-key")
                        .header("Content-Type", "application/json")
                        .body(Body::from(body))
                        .unwrap();
                    assert_eq!(
                        create_authenticated_router(state.clone())
                            .oneshot(req)
                            .await
                            .unwrap()
                            .status(),
                        StatusCode::BAD_REQUEST
                    );
                }
            }
            for path in [
                "/api/settings",
                "/api/control/reboot",
                "/api/control/mode",
                "/api/control/status",
            ] {
                let req = Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("Authorization", "Bearer integration-key")
                    .body(Body::empty())
                    .unwrap();
                assert_eq!(
                    create_authenticated_router(state.clone())
                        .oneshot(req)
                        .await
                        .unwrap()
                        .status(),
                    StatusCode::METHOD_NOT_ALLOWED
                );
            }
            // Main-server Quick Actions deliberately remain tokenless.
            let req = Request::builder()
                .method("POST")
                .uri("/api/control/force-charge")
                .header("Content-Type", "application/json")
                .body(Body::from("{\"minutes\":30}"))
                .unwrap();
            assert_eq!(
                crate::server::create_router(state)
                    .oneshot(req)
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
        })
        .await;
    }

    #[tokio::test]
    async fn external_actions_require_authentication_and_explicit_permission() {
        with_isolated_config_dir_async(|| async {
            let state = setup(false).await;
            for action in ACTIONS {
                for token in [None, Some("wrong-key")] {
                    assert_eq!(
                        request(state.clone(), action, token, json!({"minutes":30}))
                            .await
                            .0,
                        StatusCode::UNAUTHORIZED
                    );
                }
                assert_eq!(
                    request(
                        state.clone(),
                        action,
                        Some("integration-key"),
                        json!({"minutes":30})
                    )
                    .await
                    .0,
                    StatusCode::FORBIDDEN
                );
            }
            assert!(state.pending_writes.lock().await.is_empty());
            assert!(state.force_charge_revert.lock().await.is_none());
            assert!(state.force_discharge_revert.lock().await.is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn external_permission_defaults_false_persists_and_rejects_invalid_values() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            assert_eq!(
                serde_json::to_value(Settings::load()).unwrap()["api_control_enabled"],
                false
            );
            for enabled in [true, false] {
                let (status, _) = api::update_settings(
                    State(state.clone()),
                    Json(json!({"api_control_enabled": enabled})),
                )
                .await;
                assert_eq!(status, StatusCode::OK);
                let (_, response) = api::get_settings(State(state.clone())).await;
                assert_eq!(response.0["data"]["api_control_enabled"], enabled);
                let _ = api::update_settings(State(state.clone()), Json(json!({"api_port": 7338})))
                    .await;
                assert_eq!(
                    serde_json::to_value(Settings::load()).unwrap()["api_control_enabled"],
                    enabled
                );
            }
            for invalid in [json!("true"), json!(1), Value::Null] {
                let (status, _) = api::update_settings(
                    State(state.clone()),
                    Json(json!({"api_control_enabled": invalid, "api_port": 9999})),
                )
                .await;
                assert_eq!(status, StatusCode::BAD_REQUEST);
                assert_eq!(Settings::load().api_port, 7338);
            }
        })
        .await;
    }

    #[tokio::test]
    async fn external_starts_validate_duration_before_changing_state() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            for action in ["force-charge", "force-discharge"] {
                for body in [
                    json!({}),
                    json!({"minutes":0}),
                    json!({"minutes":-1}),
                    json!({"minutes":1.5}),
                    json!({"minutes":"30"}),
                    json!({"minutes":1440}),
                    json!({"minutes":30,"start":"tomorrow"}),
                    Value::Null,
                ] {
                    assert_eq!(
                        request(state.clone(), action, Some("integration-key"), body.clone())
                            .await
                            .0,
                        StatusCode::BAD_REQUEST,
                        "{action}: {body}"
                    );
                }
            }
            assert!(state.pending_writes.lock().await.is_empty());
            assert!(state.force_charge_revert.lock().await.is_none());
            assert!(state.force_discharge_revert.lock().await.is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn external_actions_reuse_quick_action_restore_and_conflict_behaviour() {
        with_isolated_config_dir_async(|| async {
            for device in [
                DeviceType::ACCoupled,
                DeviceType::Gen3Hybrid,
                DeviceType::ThreePhase,
                DeviceType::Gateway,
            ] {
                let state = setup(true).await;
                state
                    .latest_snapshot
                    .lock()
                    .await
                    .as_mut()
                    .unwrap()
                    .device_type = device;
                for (start, stop, opposite) in [
                    ("force-charge", "force-charge/stop", "force-discharge"),
                    ("force-discharge", "force-discharge/stop", "force-charge"),
                ] {
                    assert_eq!(
                        request(
                            state.clone(),
                            start,
                            Some("integration-key"),
                            json!({"minutes":30})
                        )
                        .await
                        .0,
                        StatusCode::OK
                    );
                    assert!(!state.pending_writes.lock().await.is_empty());
                    assert_eq!(
                        request(
                            state.clone(),
                            opposite,
                            Some("integration-key"),
                            json!({"minutes":30})
                        )
                        .await
                        .0,
                        StatusCode::BAD_REQUEST
                    );
                    state.pending_writes.lock().await.clear();
                    assert_eq!(
                        request(state.clone(), stop, Some("integration-key"), Value::Null)
                            .await
                            .0,
                        StatusCode::OK
                    );
                    let actual: Vec<_> = state
                        .pending_writes
                        .lock()
                        .await
                        .drain(..)
                        .flat_map(|b| b.writes)
                        .map(|w| (w.address, w.value))
                        .collect();
                    let direct = setup(true).await;
                    direct
                        .latest_snapshot
                        .lock()
                        .await
                        .as_mut()
                        .unwrap()
                        .device_type = device;
                    if start == "force-charge" {
                        let _ = api::force_charge(
                            State(direct.clone()),
                            Some(Json(json!({"minutes":30}))),
                        )
                        .await;
                    } else {
                        let _ = api::force_discharge(
                            State(direct.clone()),
                            Some(Json(json!({"minutes":30}))),
                        )
                        .await;
                    }
                    direct.pending_writes.lock().await.clear();
                    if start == "force-charge" {
                        let _ = api::force_charge_stop(State(direct.clone())).await;
                    } else {
                        let _ = api::force_discharge_stop(State(direct.clone())).await;
                    }
                    let expected: Vec<_> = direct
                        .pending_writes
                        .lock()
                        .await
                        .drain(..)
                        .flat_map(|b| b.writes)
                        .map(|w| (w.address, w.value))
                        .collect();
                    assert_eq!(actual, expected);
                }
            }
        })
        .await;
    }

    #[tokio::test]
    async fn status_reports_hem_owned_force_actions_through_the_route() {
        with_isolated_config_dir_async(|| async {
            let state = setup(false).await;
            let read_status = || async {
                let req = Request::builder()
                    .uri("/api/control/status")
                    .header("Authorization", "Bearer integration-key")
                    .body(Body::empty())
                    .unwrap();
                let response = create_authenticated_router(state.clone())
                    .oneshot(req)
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
                serde_json::from_slice::<Value>(&bytes).unwrap()
            };
            // get_status freshness-checks against the real clock, so anchor
            // the reading just in the past rather than a fixed epoch.
            let now_secs = chrono::Utc::now().timestamp();
            *state.connection_state.lock().await = ConnectionState::Connected;
            *state.latest_snapshot.lock().await = Some(InverterSnapshot {
                timestamp: now_secs - 1,
                device_type: DeviceType::ACCoupled,
                ..Default::default()
            });
            // Both closure branches of the handler's force-window read.
            *state.force_charge_revert.lock().await =
                Some(crate::inverter::poll::ForceChargeRevert {
                    started_at_ms: (now_secs - 5) * 1000,
                    force_charge_slot_end_ms: Some((now_secs + 3600) * 1000),
                    enable_charge: true,
                    enable_discharge: false,
                    target_soc: 100,
                    battery_power_mode: 1,
                    charge_rate: None,
                    charge_slot_1_start: None,
                    charge_slot_1_end: None,
                    three_phase_force_charge_enable: None,
                    three_phase_ac_charge_enable: None,
                    battery_pause_mode: None,
                });
            let value = read_status().await;
            assert_eq!(value["control_source"], "force_charge");
            assert_eq!(value["quick_action"]["action"], "force_charge");
            *state.force_charge_revert.lock().await = None;
            *state.force_discharge_revert.lock().await =
                Some(crate::inverter::poll::ForceDischargeRevert {
                    started_at_ms: (now_secs - 5) * 1000,
                    enable_charge: false,
                    enable_discharge: true,
                    discharge_rate: None,
                    discharge_slot_1_start: None,
                    discharge_slot_1_end: None,
                    discharge_slot_2_start: None,
                    discharge_slot_2_end: None,
                    three_phase_force_discharge_enable: None,
                    three_phase_force_charge_enable: None,
                    force_discharge_slot_end_ms: None,
                    battery_pause_mode: 0,
                    battery_pause_slot: Default::default(),
                });
            let value = read_status().await;
            assert_eq!(value["control_source"], "force_discharge");
            assert_eq!(value["quick_action"]["action"], "force_discharge");
        })
        .await;
    }

    #[tokio::test]
    async fn external_status_is_authenticated_read_only_without_write_permission() {
        with_isolated_config_dir_async(|| async {
            let state = setup(false).await;
            for (token, expected) in [
                (None, StatusCode::UNAUTHORIZED),
                (Some("integration-key"), StatusCode::OK),
            ] {
                let mut request = Request::builder().uri("/api/control/status");
                if let Some(token) = token {
                    request = request.header("Authorization", format!("Bearer {token}"));
                }
                let response = create_authenticated_router(state.clone())
                    .oneshot(request.body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), expected);
                if expected == StatusCode::OK {
                    assert_eq!(response.headers()["cache-control"], "no-store");
                }
            }
            assert!(state.pending_writes.lock().await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn external_key_rotation_and_permission_revocation_take_effect_live() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            // Rotate the credential via the generate flow; the response
            // carries the one-time secret.
            let (_, response) = api::update_settings(
                State(state.clone()),
                Json(json!({"api_key_generate": true})),
            )
            .await;
            let new_key = response["data"]["api_key"].as_str().unwrap().to_string();
            assert_eq!(
                request(
                    state.clone(),
                    "force-charge",
                    Some("integration-key"),
                    json!({"minutes":30})
                )
                .await
                .0,
                StatusCode::UNAUTHORIZED,
                "the previous credential must be invalidated by rotation"
            );
            assert_eq!(
                request(
                    state.clone(),
                    "force-charge",
                    Some(&new_key),
                    json!({"minutes":30})
                )
                .await
                .0,
                StatusCode::OK
            );
            let _ = api::update_settings(
                State(state.clone()),
                Json(json!({"api_control_enabled":false})),
            )
            .await;
            assert_eq!(
                request(
                    state.clone(),
                    "force-charge/stop",
                    Some(&new_key),
                    Value::Null
                )
                .await
                .0,
                StatusCode::FORBIDDEN
            );
            assert!(
                state.force_charge_revert.lock().await.is_some(),
                "revoking access does not stop an accepted action"
            );
            let _ = api::update_settings(State(state.clone()), Json(json!({"api_key":""}))).await;
            assert_eq!(
                request(state, "force-charge/stop", Some(&new_key), Value::Null)
                    .await
                    .0,
                StatusCode::UNAUTHORIZED
            );
        })
        .await;
    }
}
