//! Authenticated adapters for the existing Quick Actions (issue #301).

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{body::{Body, to_bytes}, extract::State, http::{Request, StatusCode}, Json};
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use crate::{inverter::{model::{DeviceType, InverterSnapshot}, poll::AppState}, settings::Settings};
    use crate::server::{api, create_readonly_router};
    use crate::test_util::with_isolated_config_dir_async;

    const ACTIONS: [&str; 4] = ["force-charge", "force-charge/stop", "force-discharge", "force-discharge/stop"];

    async fn setup(enabled: bool) -> Arc<AppState> {
        let mut settings = Settings::load();
        settings.api_key = "integration-key".into();
        settings.save().unwrap();
        let state = Arc::new(AppState::new());
        let (status, _) = api::update_settings(State(state.clone()), Json(json!({"api_control_enabled": enabled}))).await;
        assert_eq!(status, StatusCode::OK);
        *state.latest_snapshot.lock().await = Some(InverterSnapshot {
            timestamp: 1_800_000_000,
            device_type: DeviceType::ACCoupled,
            ..Default::default()
        });
        state
    }

    async fn request(state: Arc<AppState>, action: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
        let mut request = Request::builder().method("POST")
            .uri(format!("/api/control/{action}"))
            .header("Content-Type", "application/json");
        if let Some(token) = token { request = request.header("Authorization", format!("Bearer {token}")); }
        let response = create_readonly_router(state).oneshot(request.body(Body::from(body.to_string())).unwrap()).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn external_actions_require_authentication_and_explicit_permission() {
        with_isolated_config_dir_async(|| async {
            let state = setup(false).await;
            for action in ACTIONS {
                for token in [None, Some("wrong-key")] {
                    assert_eq!(request(state.clone(), action, token, json!({"minutes":30})).await.0, StatusCode::UNAUTHORIZED);
                }
                assert_eq!(request(state.clone(), action, Some("integration-key"), json!({"minutes":30})).await.0, StatusCode::FORBIDDEN);
            }
            assert!(state.pending_writes.lock().await.is_empty());
            assert!(state.force_charge_revert.lock().await.is_none());
            assert!(state.force_discharge_revert.lock().await.is_none());
        }).await;
    }

    #[tokio::test]
    async fn external_permission_defaults_false_persists_and_rejects_invalid_values() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            assert_eq!(serde_json::to_value(Settings::load()).unwrap()["api_control_enabled"], false);
            for enabled in [true, false] {
                let (status, _) = api::update_settings(State(state.clone()), Json(json!({"api_control_enabled": enabled}))).await;
                assert_eq!(status, StatusCode::OK);
                let (_, response) = api::get_settings(State(state.clone())).await;
                assert_eq!(response.0["data"]["api_control_enabled"], enabled);
                let _ = api::update_settings(State(state.clone()), Json(json!({"api_port": 7338}))).await;
                assert_eq!(serde_json::to_value(Settings::load()).unwrap()["api_control_enabled"], enabled);
            }
            for invalid in [json!("true"), json!(1), Value::Null] {
                let (status, _) = api::update_settings(State(state.clone()), Json(json!({"api_control_enabled": invalid, "api_port": 9999}))).await;
                assert_eq!(status, StatusCode::BAD_REQUEST);
                assert_eq!(Settings::load().api_port, 7338);
            }
        }).await;
    }

    #[tokio::test]
    async fn external_starts_validate_duration_before_changing_state() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            for action in ["force-charge", "force-discharge"] {
                for body in [json!({}), json!({"minutes":0}), json!({"minutes":-1}), json!({"minutes":1.5}), json!({"minutes":"30"}), json!({"minutes":1440}), json!({"minutes":30,"start":"tomorrow"}), Value::Null] {
                    assert_eq!(request(state.clone(), action, Some("integration-key"), body.clone()).await.0, StatusCode::BAD_REQUEST, "{action}: {body}");
                }
            }
            assert!(state.pending_writes.lock().await.is_empty());
            assert!(state.force_charge_revert.lock().await.is_none());
            assert!(state.force_discharge_revert.lock().await.is_none());
        }).await;
    }

    #[tokio::test]
    async fn external_actions_reuse_quick_action_restore_and_conflict_behaviour() {
        with_isolated_config_dir_async(|| async {
            for device in [DeviceType::ACCoupled, DeviceType::Gen3Hybrid, DeviceType::ThreePhase, DeviceType::Gateway] {
                let state = setup(true).await;
                state.latest_snapshot.lock().await.as_mut().unwrap().device_type = device;
                for (start, stop, opposite) in [("force-charge", "force-charge/stop", "force-discharge"), ("force-discharge", "force-discharge/stop", "force-charge")] {
                    assert_eq!(request(state.clone(), start, Some("integration-key"), json!({"minutes":30})).await.0, StatusCode::OK);
                    assert!(!state.pending_writes.lock().await.is_empty());
                    assert_eq!(request(state.clone(), opposite, Some("integration-key"), json!({"minutes":30})).await.0, StatusCode::BAD_REQUEST);
                    state.pending_writes.lock().await.clear();
                    assert_eq!(request(state.clone(), stop, Some("integration-key"), Value::Null).await.0, StatusCode::OK);
                    let actual: Vec<_> = state.pending_writes.lock().await.drain(..).flat_map(|b| b.writes).map(|w| (w.address,w.value)).collect();
                    let direct = setup(true).await;
                    direct.latest_snapshot.lock().await.as_mut().unwrap().device_type = device;
                    if start == "force-charge" { api::force_charge(State(direct.clone()), Some(Json(json!({"minutes":30})))).await; }
                    else { api::force_discharge(State(direct.clone()), Some(Json(json!({"minutes":30})))).await; }
                    direct.pending_writes.lock().await.clear();
                    if start == "force-charge" { api::force_charge_stop(State(direct.clone())).await; }
                    else { api::force_discharge_stop(State(direct.clone())).await; }
                    let expected: Vec<_> = direct.pending_writes.lock().await.drain(..).flat_map(|b| b.writes).map(|w| (w.address,w.value)).collect();
                    assert_eq!(actual, expected);
                }
            }
        }).await;
    }

    #[tokio::test]
    async fn external_status_is_authenticated_read_only_without_write_permission() {
        with_isolated_config_dir_async(|| async {
            let state = setup(false).await;
            for (token, expected) in [(None, StatusCode::UNAUTHORIZED), (Some("integration-key"), StatusCode::OK)] {
                let mut request = Request::builder().uri("/api/control/status");
                if let Some(token) = token { request = request.header("Authorization", format!("Bearer {token}")); }
                let response = create_readonly_router(state.clone()).oneshot(request.body(Body::empty()).unwrap()).await.unwrap();
                assert_eq!(response.status(), expected);
                if expected == StatusCode::OK { assert_eq!(response.headers()["cache-control"], "no-store"); }
            }
            assert!(state.pending_writes.lock().await.is_empty());
        }).await;
    }

    #[tokio::test]
    async fn external_key_rotation_and_permission_revocation_take_effect_live() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            api::update_settings(State(state.clone()), Json(json!({"api_key":"new-key"}))).await;
            assert_eq!(request(state.clone(), "force-charge", Some("integration-key"), json!({"minutes":30})).await.0, StatusCode::UNAUTHORIZED);
            assert_eq!(request(state.clone(), "force-charge", Some("new-key"), json!({"minutes":30})).await.0, StatusCode::OK);
            api::update_settings(State(state.clone()), Json(json!({"api_control_enabled":false}))).await;
            assert_eq!(request(state.clone(), "force-charge/stop", Some("new-key"), Value::Null).await.0, StatusCode::FORBIDDEN);
            assert!(state.force_charge_revert.lock().await.is_some(), "revoking access does not stop an accepted action");
            api::update_settings(State(state.clone()), Json(json!({"api_key":""}))).await;
            assert_eq!(request(state, "force-charge/stop", Some("new-key"), Value::Null).await.0, StatusCode::UNAUTHORIZED);
        }).await;
    }
}
