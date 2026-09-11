//! Authenticated adapters for the existing Quick Actions (issue #301).
//!
//! Authentication and write permission are router middleware; these adapters
//! only validate external input, then call exactly the same UI handlers.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::api;
use super::external_commands::Reservation;
use super::{audit_control_event_or_block, AuthenticatedIdentity};
use crate::server::audit::AuditEvent;
use crate::{inverter::poll::AppState, settings::Settings};

/// The authenticated identity attached by the auth middleware (present on
/// every request that reached permission validation).
/// The required `Idempotency-Key` header value (U6). Every external
/// mutation must carry one: retries reuse the same key and are replayed or
/// resolved against the running command; a missing key is refused before
/// any state changes.
fn idempotency_key(request: &Request) -> Result<String, Box<Response>> {
    match request
        .headers()
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
    {
        Some(key) if (16..=128).contains(&key.len()) && !key.contains(char::is_whitespace) => {
            Ok(key.to_string())
        }
        Some(_) => Err(Box::new((
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false,
            "error": "Idempotency-Key must be 16-128 characters without whitespace (a UUID is ideal)"})),
        )
            .into_response())),
        None => Err(Box::new((
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false,
            "error": "An Idempotency-Key header is required for control actions; reuse the same key to retry safely"})),
        )
            .into_response())),
    }
}

fn identity(request: &Request) -> String {
    request
        .extensions()
        .get::<AuthenticatedIdentity>()
        .map(|identity| identity.fingerprint.clone())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Fail-open audit for denials (no mutation happens, so a failed audit
/// write cannot make the system unsafe — it is logged locally instead).
fn audit_denial(state: &Arc<AppState>, mut event: AuditEvent) {
    event.outcome = "denied";
    if let Err(e) = state.audit.record(event) {
        tracing::warn!("Audit write failed: {e}");
    }
}

pub async fn require_control_permission(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    if !Settings::load_async().await.api_control_enabled {
        audit_denial(
            &state,
            AuditEvent {
                kind: "authz_denied",
                actor: Some(identity(&request)),
                source: None,
                method: Some(request.method().to_string()),
                path: Some(request.uri().path().to_string()),
                outcome: "denied",
                detail: None,
            },
        );
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"ok":false,
            "error":"External battery control is disabled"})),
        )
            .into_response();
    }
    next.run(request).await
}

/// Enforce the per-identity start budget. Starts are deliberately tighter
/// than stops: a flood of starts must not be able to thrash the battery.
fn check_start_limit(state: &Arc<AppState>, fingerprint: &str) -> Option<Response> {
    let decision = state
        .action_start_limiter
        .lock()
        .check(fingerprint.to_string());
    if decision.allowed {
        return None;
    }
    Some(
        (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"ok":false,
        "error":"Too many start requests; retry later"})),
        )
            .into_response(),
    )
}

/// Enforce the per-identity stop budget (wider than starts so recovery is
/// never starved by read traffic or retries).
fn check_stop_limit(state: &Arc<AppState>, fingerprint: &str) -> Option<Response> {
    let decision = state
        .action_stop_limiter
        .lock()
        .check(fingerprint.to_string());
    if decision.allowed {
        return None;
    }
    Some(
        (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"ok":false,
        "error":"Too many stop requests; retry later"})),
        )
            .into_response(),
    )
}

/// Fail-closed audit record for an accepted control action: the command is
/// not queued unless this write succeeds.
fn audit_action(
    state: &Arc<AppState>,
    fingerprint: &str,
    kind: &'static str,
    detail: Option<String>,
) -> Option<Response> {
    audit_control_event_or_block(
        state,
        AuditEvent {
            kind,
            actor: Some(fingerprint.to_string()),
            source: None,
            method: Some("POST".to_string()),
            path: None,
            outcome: "accepted",
            detail,
        },
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurationRequest {
    minutes: u64,
}

fn bad_duration_request() -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"ok":false,
        "error":"Expected only an integer minutes field between 1 and 1439"})),
    )
}

/// Parse and validate an external start body. Manual parsing (instead of
/// the `Json` extractor) because these handlers also need the request for
/// the authenticated-identity extension, and `Request` must be the last
/// extractor. serde's derived visitor still rejects duplicate fields, and
/// `deny_unknown_fields` still rejects anything beyond `minutes`.
async fn duration_body(
    request: Request,
) -> (Option<String>, Result<Value, (StatusCode, Json<Value>)>) {
    let content_type = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !content_type.starts_with("application/json") {
        return (None, Err(bad_duration_request()));
    }
    let bytes = match axum::body::to_bytes(request.into_body(), 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return (None, Err(bad_duration_request())),
    };
    match serde_json::from_slice::<DurationRequest>(&bytes) {
        Ok(body) if (1..=1439).contains(&body.minutes) => (
            Some(body.minutes.to_string()),
            Ok(json!({"minutes": body.minutes})),
        ),
        _ => (None, Err(bad_duration_request())),
    }
}

/// Shared start-adapter body: limits, reservation, fail-closed audit,
/// handler delegation, command state bookkeeping and recovery baseline.
async fn run_start(
    state: Arc<AppState>,
    fingerprint: String,
    idem_key: String,
    action: &'static str,
    minutes: u64,
) -> Response {
    if let Some(response) = check_start_limit(&state, &fingerprint) {
        return response;
    }
    let reservation = match state.command_ledger.reserve_start(
        &fingerprint,
        action,
        minutes,
        idem_key.as_str(),
        chrono::Utc::now().timestamp_millis(),
    ) {
        Ok(reservation) => reservation,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": e})),
            )
                .into_response()
        }
    };
    let command_id = match reservation {
        Reservation::Accepted { command_id } => command_id,
        Reservation::Replayed { response } => return replay_response(response),
        Reservation::InProgress { command_id } => return in_progress_response(command_id),
        Reservation::Conflict {
            existing_command_id,
        } => return conflict_response(existing_command_id),
    };
    if let Some(response) = audit_action(
        &state,
        &fingerprint,
        "action_start",
        Some(format!("{action} minutes={minutes}")),
    ) {
        let _ = state.command_ledger.mark_state(&command_id, "failed");
        return response;
    }
    // Force Discharge owns the no-snapshot guard for external callers: the
    // UI cannot offer Quick Actions before its first snapshot, and an
    // unknown restore baseline must never be created.
    if action == "force_discharge" && state.latest_snapshot.lock().await.is_none() {
        let _ = state.command_ledger.mark_state(&command_id, "failed");
        return (
            StatusCode::CONFLICT,
            Json(json!({"ok":false,
            "error":"Force Discharge requires an inverter snapshot before it can start"})),
        )
            .into_response();
    }
    let (status, Json(mut response_body)) = if action == "force_discharge" {
        api::force_discharge(
            State(state.clone()),
            Some(Json(json!({"minutes": minutes}))),
        )
        .await
    } else {
        api::force_charge(
            State(state.clone()),
            Some(Json(json!({"minutes": minutes}))),
        )
        .await
    };
    response_body["command_id"] = Value::String(command_id.clone());
    if status.is_success() {
        let _ = state.command_ledger.mark_state(&command_id, "queued");
        // Mark the in-memory action as externally owned: a revoked start
        // permission still allows recovery of this action (U5 recovery
        // stops), while a read-only credential gains nothing.
        if action == "force_charge" {
            if let Some(revert) = state.force_charge_revert.lock().await.as_mut() {
                revert.external_owner = Some(fingerprint.clone());
            }
        } else if let Some(revert) = state.force_discharge_revert.lock().await.as_mut() {
            revert.external_owner = Some(fingerprint.clone());
        }
        record_recovery_baseline(&state, &command_id, action).await;
        store_envelope(&state, &command_id, status, &response_body);
    } else {
        let _ = state.command_ledger.finish(
            &command_id,
            "failed",
            &json!({"status": status.as_u16(), "body": response_body.clone()}).to_string(),
        );
    }
    (status, Json(response_body)).into_response()
}

pub async fn force_charge(State(state): State<Arc<AppState>>, request: Request) -> Response {
    let fingerprint = identity(&request);
    let idem_key = match idempotency_key(&request) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    let (minutes, body) = duration_body(request).await;
    // The validated body is rebuilt inside run_start; here we only need the
    // parse/validation side effect.
    if let Err(error) = body {
        return error.into_response();
    }
    let minutes_u64: u64 = minutes.as_deref().and_then(|m| m.parse().ok()).unwrap_or(0);
    run_start(state, fingerprint, idem_key, "force_charge", minutes_u64).await
}

pub async fn force_discharge(State(state): State<Arc<AppState>>, request: Request) -> Response {
    let fingerprint = identity(&request);
    let idem_key = match idempotency_key(&request) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    let (minutes, body) = duration_body(request).await;
    // The validated body is rebuilt inside run_start; here we only need the
    // parse/validation side effect.
    if let Err(error) = body {
        return error.into_response();
    }
    let minutes_u64: u64 = minutes.as_deref().and_then(|m| m.parse().ok()).unwrap_or(0);
    run_start(state, fingerprint, idem_key, "force_discharge", minutes_u64).await
}

/// Stops are conditional on permission: with the control toggle on they
/// behave exactly as before (idempotent stop of whatever is active). With
/// the toggle off they are allowed ONLY as recovery of a HEM-owned external
/// action — a compromised read-only credential cannot stop arbitrary local
/// actions, but a revoked external action always has a remote recovery path.
async fn run_stop(
    state: Arc<AppState>,
    fingerprint: String,
    idem_key: String,
    action: &'static str,
) -> Response {
    let settings = Settings::load_async().await;
    if !settings.api_control_enabled {
        let externally_owned = match action {
            "force_charge" => state
                .force_charge_revert
                .lock()
                .await
                .as_ref()
                .and_then(|revert| revert.external_owner.clone()),
            _ => state
                .force_discharge_revert
                .lock()
                .await
                .as_ref()
                .and_then(|revert| revert.external_owner.clone()),
        };
        if externally_owned.is_none() {
            audit_denial(
                &state,
                AuditEvent {
                    kind: "authz_denied",
                    actor: Some(fingerprint.clone()),
                    source: None,
                    method: Some("POST".to_string()),
                    path: Some(format!("/api/control/{action}/stop")),
                    outcome: "denied",
                    detail: Some("control permission off; no external action owned".into()),
                },
            );
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"ok":false,
                "error":"External battery control is disabled"})),
            )
                .into_response();
        }
    }
    if let Some(response) = check_stop_limit(&state, &fingerprint) {
        return response;
    }
    let reservation = match state.command_ledger.reserve_stop(
        &fingerprint,
        action,
        idem_key.as_str(),
        chrono::Utc::now().timestamp_millis(),
    ) {
        Ok(reservation) => reservation,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": e})),
            )
                .into_response()
        }
    };
    let command_id = match reservation {
        Reservation::Accepted { command_id } => command_id,
        Reservation::Replayed { response } => return replay_response(response),
        Reservation::InProgress { command_id } => return in_progress_response(command_id),
        Reservation::Conflict {
            existing_command_id,
        } => return conflict_response(existing_command_id),
    };
    if let Some(response) = audit_action(
        &state,
        &fingerprint,
        "action_stop",
        Some(action.to_string()),
    ) {
        let _ = state.command_ledger.mark_state(&command_id, "failed");
        return response;
    }
    let (status, Json(mut response_body)) = if action == "force_charge" {
        api::force_charge_stop(State(state.clone())).await
    } else {
        api::force_discharge_stop(State(state.clone())).await
    };
    response_body["command_id"] = Value::String(command_id.clone());
    if status.is_success() {
        let _ = state.command_ledger.mark_state(&command_id, "queued");
        store_envelope(&state, &command_id, status, &response_body);
    } else {
        let _ = state.command_ledger.finish(
            &command_id,
            "failed",
            &json!({"status": status.as_u16(), "body": response_body.clone()}).to_string(),
        );
    }
    (status, Json(response_body)).into_response()
}

/// External stop adapter: per-identity stop budget plus fail-closed audit.
pub async fn force_charge_stop(State(state): State<Arc<AppState>>, request: Request) -> Response {
    let fingerprint = identity(&request);
    let idem_key = match idempotency_key(&request) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    run_stop(state, fingerprint, idem_key, "force_charge").await
}

/// See [`force_charge_stop`].
pub async fn force_discharge_stop(
    State(state): State<Arc<AppState>>,
    request: Request,
) -> Response {
    let fingerprint = identity(&request);
    let idem_key = match idempotency_key(&request) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    run_stop(state, fingerprint, idem_key, "force_discharge").await
}

fn replay_response(response: Value) -> Response {
    let status = response
        .get("status")
        .and_then(|v| v.as_u64())
        .and_then(|code| StatusCode::from_u16(code as u16).ok())
        .unwrap_or(StatusCode::OK);
    let body = response.get("body").cloned().unwrap_or(Value::Null);
    (status, Json(body)).into_response()
}

fn in_progress_response(command_id: String) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({"ok": false,
        "error": "A request with this Idempotency-Key is still in progress",
        "command_id": command_id})),
    )
        .into_response()
}

fn conflict_response(existing_command_id: String) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({"ok": false,
        "error": "Idempotency key already used with a different request",
        "command_id": existing_command_id})),
    )
        .into_response()
}

/// Snapshot the in-memory revert as the durable recovery baseline for a
/// start command, so a restart can reconcile instead of guessing.
async fn record_recovery_baseline(state: &Arc<AppState>, command_id: &str, action: &str) {
    let recovery = if action == "force_charge" {
        state
            .force_charge_revert
            .lock()
            .await
            .as_ref()
            .and_then(|revert| serde_json::to_string(revert).ok())
    } else {
        state
            .force_discharge_revert
            .lock()
            .await
            .as_ref()
            .and_then(|revert| serde_json::to_string(revert).ok())
    };
    if let Some(recovery) = recovery {
        let _ = state.command_ledger.record_recovery(command_id, &recovery);
    }
}

fn store_envelope(state: &Arc<AppState>, command_id: &str, status: StatusCode, body: &Value) {
    let envelope = json!({"status": status.as_u16(), "body": body});
    let _ = state
        .command_ledger
        .store_response(command_id, &envelope.to_string());
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
        // A new AppState models a fresh session: the durable ledger survives
        // it by design, so reconcile exactly like a real startup does.
        state.command_ledger.reconcile_startup().unwrap();
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

    /// Sequential key suffix so every request is a fresh idempotency scope
    /// unless a test supplies an explicit key.
    fn next_key() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        format!("test-key-{:016x}", COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    async fn request(
        state: Arc<AppState>,
        action: &str,
        token: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        request_with_key(state, action, token, &next_key(), body).await
    }

    async fn request_with_key(
        state: Arc<AppState>,
        action: &str,
        token: Option<&str>,
        key: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri(format!("/api/control/{action}"))
            .header("Content-Type", "application/json")
            .header("Idempotency-Key", key);
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let payload = if body.is_null() {
            String::new()
        } else {
            body.to_string()
        };
        let response = create_authenticated_router(state)
            .oneshot(request.body(Body::from(payload)).unwrap())
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
                    state.action_start_limiter.lock().clear();
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
                    external_owner: None,
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
                    external_owner: None,
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
                StatusCode::OK,
                "revoked start permission must still permit recovery of the owned action"
            );
            assert!(
                state.force_charge_revert.lock().await.is_none(),
                "the recovery stop consumed the accepted action"
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

    /// U6: the Idempotency-Key header is mandatory on all four mutations.
    #[tokio::test]
    async fn external_mutations_require_an_idempotency_key() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            for action in ACTIONS {
                let mut request = Request::builder()
                    .method("POST")
                    .uri(format!("/api/control/{action}"))
                    .header("Content-Type", "application/json");
                if let Some(token) = Some("integration-key") {
                    request = request.header("Authorization", format!("Bearer {token}"));
                }
                let payload = if action.ends_with("stop") {
                    String::new()
                } else {
                    r#"{"minutes":30}"#.to_string()
                };
                let response = create_authenticated_router(state.clone())
                    .oneshot(request.body(Body::from(payload)).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{action}");
            }
            assert!(
                state.pending_writes.lock().await.is_empty(),
                "no writes without a key"
            );
        })
        .await;
    }

    /// Retrying with the same key replays the original response and never
    /// enqueues a second command.
    #[tokio::test]
    async fn external_replay_with_same_key_returns_original_response() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            let first = request_with_key(
                state.clone(),
                "force-charge",
                Some("integration-key"),
                "retry-key-00000001",
                json!({"minutes":30}),
            )
            .await;
            assert_eq!(first.0, StatusCode::OK);
            let command_id = first.1["command_id"].as_str().unwrap().to_string();
            state.pending_writes.lock().await.clear();

            let second = request_with_key(
                state.clone(),
                "force-charge",
                Some("integration-key"),
                "retry-key-00000001",
                json!({"minutes":30}),
            )
            .await;
            assert_eq!(second.0, StatusCode::OK);
            assert_eq!(
                second.1["command_id"], command_id,
                "replay must return the original command"
            );
            assert!(
                state.pending_writes.lock().await.is_empty(),
                "replay must not enqueue new writes"
            );
        })
        .await;
    }

    /// The same key with a different payload conflicts without changing
    /// command state.
    #[tokio::test]
    async fn external_conflicting_key_usage_returns_409() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            let first = request_with_key(
                state.clone(),
                "force-charge",
                Some("integration-key"),
                "conflict-key-000001",
                json!({"minutes":30}),
            )
            .await;
            assert_eq!(first.0, StatusCode::OK);
            state.pending_writes.lock().await.clear();

            let second = request_with_key(
                state.clone(),
                "force-charge",
                Some("integration-key"),
                "conflict-key-000001",
                json!({"minutes":90}),
            )
            .await;
            assert_eq!(second.0, StatusCode::CONFLICT);
            assert!(
                second.1["error"]
                    .as_str()
                    .unwrap()
                    .contains("different request"),
                "{second:?}"
            );
        })
        .await;
    }

    /// The command status endpoint reports lifecycle state and 404s unknown
    /// ids.
    #[tokio::test]
    async fn command_status_endpoint_reports_lifecycle_state() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            let first = request_with_key(
                state.clone(),
                "force-charge",
                Some("integration-key"),
                "status-key-000001",
                json!({"minutes":30}),
            )
            .await;
            let command_id = first.1["command_id"].as_str().unwrap().to_string();

            let get = |command_id: &str| {
                let state = state.clone();
                let uri = format!("/api/commands/{command_id}");
                async move {
                    let req = Request::builder()
                        .uri(uri)
                        .header("Authorization", "Bearer integration-key")
                        .body(Body::empty())
                        .unwrap();
                    let response = create_authenticated_router(state)
                        .oneshot(req)
                        .await
                        .unwrap();
                    let status = response.status();
                    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
                    (
                        status,
                        serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
                    )
                }
            };
            let (status, body) = get(&command_id).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["data"]["command_id"], command_id);
            assert_eq!(body["data"]["action"], "force_charge");
            // The writes are queued but no inverter readback has happened in
            // this test: the state must be an honest intermediate value.
            assert_eq!(body["data"]["state"], "queued");

            let (status, body) = get("nonexistent").await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert_eq!(body["error"], "Unknown command id");
        })
        .await;
    }

    /// Recovery stops are refused when the permission is off AND no external
    /// action is owned — a revoked read-only credential gains nothing.
    #[tokio::test]
    async fn recovery_stop_without_external_action_stays_forbidden() {
        with_isolated_config_dir_async(|| async {
            let state = setup(false).await;
            let (status, body) = request_with_key(
                state,
                "force-charge/stop",
                Some("integration-key"),
                "recovery-key-000001",
                Value::Null,
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert!(
                body["error"].as_str().unwrap().contains("disabled"),
                "{body:?}"
            );
        })
        .await;
    }

    /// Control mutations fail closed when the audit write fails: no command
    /// may be queued without its durable audit record.
    #[tokio::test]
    async fn external_actions_fail_closed_when_audit_write_fails() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            // Point the audit log at an unusable path (a directory where the
            // database file must go).
            let dir = crate::test_util::make_unique_test_dir("audit-broken");
            std::fs::create_dir_all(dir.join("audit.db")).unwrap();
            state.audit.override_path(dir.join("audit.db"));

            let (status, body) = request(
                state.clone(),
                "force-charge",
                Some("integration-key"),
                json!({"minutes":30}),
            )
            .await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(
                body["error"].as_str().unwrap().contains("Audit"),
                "{body:?}"
            );
            assert!(
                state.pending_writes.lock().await.is_empty(),
                "no write may be queued without its audit record"
            );
        })
        .await;
    }

    /// Starts are budgeted per credential identity (2/min by default): the
    /// third start inside the same window must be refused with 429 before
    /// reaching the Quick Action handler.
    #[tokio::test]
    async fn external_starts_enforce_per_identity_budget() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            state.pending_writes.lock().await.clear();
            // Request 1 enqueues; request 2 is a duplicate active start —
            // the ledger CAS answers 409 with the existing command; request
            // 3 hits the per-identity budget with 429.
            for expected in [
                StatusCode::OK,
                StatusCode::CONFLICT,
                StatusCode::TOO_MANY_REQUESTS,
            ] {
                let (status, _) = request(
                    state.clone(),
                    "force-charge",
                    Some("integration-key"),
                    json!({"minutes":30}),
                )
                .await;
                assert_eq!(status, expected);
                state.pending_writes.lock().await.clear();
            }
        })
        .await;
    }
}
