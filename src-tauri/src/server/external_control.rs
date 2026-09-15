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
use crate::{
    inverter::{
        model::{DeviceType, ExternalControlOperation},
        poll::{AppState, ConnectionState},
    },
    settings::Settings,
};

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

fn capability_error(status: StatusCode, code: &'static str, error: &str) -> Response {
    (
        status,
        Json(json!({"ok": false, "code": code, "error": error})),
    )
        .into_response()
}

/// Reject an authenticated mutation unless its complete register path is
/// confirmed for a fresh, connected inverter snapshot. This runs before rate
/// limiting, command reservation, accepted-action audit, baseline capture, or
/// queueing, so unsupported hardware cannot leave any command side effects.
async fn require_operation_capability(
    state: &Arc<AppState>,
    operation: ExternalControlOperation,
) -> Result<(), Box<Response>> {
    require_operation_capability_inner(state, operation, true).await
}

async fn require_stop_capability(
    state: &Arc<AppState>,
    operation: ExternalControlOperation,
) -> Result<(), Box<Response>> {
    require_operation_capability_inner(state, operation, false).await
}

async fn require_operation_capability_inner(
    state: &Arc<AppState>,
    operation: ExternalControlOperation,
    require_pause_baseline: bool,
) -> Result<(), Box<Response>> {
    if *state.connection_state.lock().await != ConnectionState::Connected {
        return Err(Box::new(capability_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "state_unavailable",
            "Current inverter state is unavailable; control request refused for safety",
        )));
    }

    let interval_secs = state.settings.lock().await.interval_secs;
    let (device_type, firmware_version, timestamp) = {
        let snapshot = state.latest_snapshot.lock().await;
        let Some(snapshot) = snapshot.as_ref() else {
            return Err(Box::new(capability_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "state_unavailable",
                "No inverter snapshot is available; control request refused for safety",
            )));
        };
        (
            snapshot.device_type,
            snapshot.firmware_version.clone(),
            snapshot.timestamp,
        )
    };
    let age_secs = chrono::Utc::now().timestamp().saturating_sub(timestamp);
    let stale_after_secs = interval_secs.saturating_mul(3).max(60).min(i64::MAX as u64) as i64;
    if age_secs > stale_after_secs || age_secs < -5 {
        return Err(Box::new(capability_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "state_unavailable",
            "The inverter snapshot is stale; control request refused for safety",
        )));
    }
    if matches!(device_type, DeviceType::Unknown(_)) {
        return Err(Box::new(capability_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "state_unavailable",
            "The inverter model has not been identified; control request refused for safety",
        )));
    }

    let arm_fw = firmware_version.parse::<u16>().ok();
    let pause_requires_firmware = matches!(device_type, DeviceType::Gen3Hybrid)
        && matches!(
            operation,
            ExternalControlOperation::PauseCharge
                | ExternalControlOperation::PauseDischarge
                | ExternalControlOperation::PauseBoth
        );
    if pause_requires_firmware && arm_fw.is_none() {
        return Err(Box::new(capability_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "state_unavailable",
            "The inverter firmware is unavailable; control request refused for safety",
        )));
    }
    let firmware = arm_fw.unwrap_or(0);
    let needs_pause_baseline = matches!(
        operation,
        ExternalControlOperation::ForceDischarge
            | ExternalControlOperation::PauseCharge
            | ExternalControlOperation::PauseDischarge
            | ExternalControlOperation::PauseBoth
    ) && device_type.supports_pause_registers(firmware);
    if require_pause_baseline
        && needs_pause_baseline
        && snapshot_pause_baseline_unavailable(state).await
    {
        return Err(Box::new(capability_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "state_unavailable",
            "The pause-register baseline is unavailable; control request refused for safety",
        )));
    }
    if !device_type.supports_external_control(operation, firmware) {
        return Err(Box::new(capability_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_control",
            "This control operation is not supported on the connected inverter",
        )));
    }
    Ok(())
}

async fn snapshot_pause_baseline_unavailable(state: &Arc<AppState>) -> bool {
    fn valid_hhmm(value: u16) -> bool {
        let hour = value / 100;
        let minute = value % 100;
        hour < 24 && minute < 60
    }

    let snapshot = state.latest_snapshot.lock().await;
    let Some(snapshot) = snapshot.as_ref() else {
        return true;
    };
    snapshot.battery_pause_mode_raw.is_none()
        || snapshot.battery_pause_slot_start_raw.is_none()
        || snapshot.battery_pause_slot_end_raw.is_none()
        || snapshot.battery_pause_registers_observed_at != Some(snapshot.timestamp)
        || snapshot.battery_pause_mode_raw.is_some_and(|mode| mode > 3)
        || snapshot
            .battery_pause_slot_start_raw
            .is_some_and(|value| !valid_hhmm(value))
        || snapshot
            .battery_pause_slot_end_raw
            .is_some_and(|value| !valid_hhmm(value))
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

#[derive(Debug, Clone, Copy, Deserialize)]
enum PauseMode {
    #[serde(rename = "charge", alias = "pause_charge")]
    Charge,
    #[serde(rename = "discharge", alias = "pause_discharge")]
    Discharge,
    #[serde(rename = "both", alias = "pause_both")]
    Both,
}

impl PauseMode {
    fn register_value(self) -> u16 {
        match self {
            Self::Charge => 1,
            Self::Discharge => 2,
            Self::Both => 3,
        }
    }

    fn action(self) -> &'static str {
        match self {
            Self::Charge => "pause_charge",
            Self::Discharge => "pause_discharge",
            Self::Both => "pause_both",
        }
    }

    fn operation(self) -> ExternalControlOperation {
        match self {
            Self::Charge => ExternalControlOperation::PauseCharge,
            Self::Discharge => ExternalControlOperation::PauseDischarge,
            Self::Both => ExternalControlOperation::PauseBoth,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PauseModeRequest {
    mode: PauseMode,
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

async fn pause_mode_body(request: Request) -> Result<(PauseMode, u64), Box<Response>> {
    let content_type = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !content_type.starts_with("application/json") {
        return Err(Box::new(bad_duration_request().into_response()));
    }
    let bytes = axum::body::to_bytes(request.into_body(), 64 * 1024)
        .await
        .map_err(|_| Box::new(bad_duration_request().into_response()))?;
    let body: PauseModeRequest = serde_json::from_slice(&bytes)
        .map_err(|_| Box::new(bad_duration_request().into_response()))?;
    if !(1..=1439).contains(&body.minutes) {
        return Err(Box::new(bad_duration_request().into_response()));
    }
    Ok((body.mode, body.minutes))
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

async fn run_pause_start(
    state: Arc<AppState>,
    fingerprint: String,
    idem_key: String,
    mode: PauseMode,
    minutes: u64,
) -> Response {
    let _action_guard = state.force_action_lock.lock().await;
    let reservation = match state.command_ledger.reserve_start(
        &fingerprint,
        mode.action(),
        minutes,
        &idem_key,
        chrono::Utc::now().timestamp_millis(),
    ) {
        Ok(reservation) => reservation,
        Err(e) => return capability_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &e),
    };
    let command_id = match reservation {
        Reservation::Accepted { command_id } => command_id,
        Reservation::Replayed { response } => return replay_response(response),
        Reservation::InProgress { command_id } => return in_progress_response(command_id),
        Reservation::Conflict {
            existing_command_id,
        } => return conflict_response(existing_command_id),
    };

    if state.force_charge_revert.lock().await.is_some()
        || state.force_discharge_revert.lock().await.is_some()
        || state.pause_mode_revert.lock().await.is_some()
        || state
            .command_ledger
            .has_active_battery_control_except(&command_id)
            .unwrap_or(false)
    {
        let _ = state.command_ledger.finish(
            &command_id,
            "failed",
            &json!({"status":409,"body":{"ok":false,"error":"Another battery control action is active"}}).to_string(),
        );
        return capability_error(
            StatusCode::CONFLICT,
            "control_conflict",
            "Another battery control action is active",
        );
    }
    if let Some(response) = audit_action(
        &state,
        &fingerprint,
        "action_start",
        Some(format!("{} minutes={minutes}", mode.action())),
    ) {
        let _ = state.command_ledger.mark_state(&command_id, "failed");
        return response;
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    let (start, end) = {
        let snapshot = state.latest_snapshot.lock().await;
        let Some(snapshot) = snapshot.as_ref() else {
            let _ = state.command_ledger.mark_state(&command_id, "failed");
            return capability_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "state_unavailable",
                "No inverter snapshot is available; pause request refused for safety",
            );
        };
        let Some(start_minute) = crate::inverter::state_machines::inverter_minute_of_day(snapshot)
        else {
            let _ = state.command_ledger.mark_state(&command_id, "failed");
            return capability_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "state_unavailable",
                "The inverter clock is unavailable; pause request refused for safety",
            );
        };
        let end_minute = (start_minute + minutes as u16) % 1440;
        (
            (start_minute / 60) * 100 + start_minute % 60,
            (end_minute / 60) * 100 + end_minute % 60,
        )
    };
    let mut revert =
        match api::capture_pause_mode_revert(&state, now_ms, now_ms + (minutes as i64) * 60_000)
            .await
        {
            Ok(revert) => revert,
            Err(error) => {
                let _ = state.command_ledger.mark_state(&command_id, "failed");
                return capability_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "state_unavailable",
                    &error,
                );
            }
        };
    revert.external_owner = Some(fingerprint.clone());
    revert.requested_mode = mode.register_value();
    revert.requested_slot_start = start;
    revert.requested_slot_end = end;
    let writes = api::build_pause_mode_writes(mode.register_value(), start, end);
    let (rx, budget) = api::queue_owned_writes_fail_fast(
        &state,
        writes,
        crate::inverter::state_machines::DischargeControlOwner::ExplicitPause,
    )
    .await;
    *state.pause_mode_revert.lock().await = Some(revert.clone());
    let mut body = json!({"ok":true,"message":"Battery pause queued","command_id":command_id});
    match api::await_write_outcome_with_timeout(rx, budget).await {
        Ok(()) => {
            let _ = state.command_ledger.mark_state(&command_id, "queued");
            let _ = state.command_ledger.record_recovery(
                &command_id,
                &serde_json::to_string(&revert).unwrap_or_default(),
            );
            store_envelope(&state, &command_id, StatusCode::OK, &body);
        }
        Err(error) => {
            let _ = state.command_ledger.finish(
                &command_id,
                "failed",
                &json!({"status":502,"body":{"ok":false,"error":error}}).to_string(),
            );
            // The fail-fast batch may have applied its window before failing.
            // Queue exact rollback while retaining pause ownership.
            let rollback = api::build_pause_mode_writes(
                revert.battery_pause_mode,
                revert.battery_pause_slot_start,
                revert.battery_pause_slot_end,
            );
            let _ = api::queue_owned_writes_fail_fast(
                &state,
                rollback,
                crate::inverter::state_machines::DischargeControlOwner::ExplicitPause,
            )
            .await;
            body = json!({"ok":false,"error":format!("Battery pause could not be applied safely: {error}"),"command_id":command_id});
        }
    }
    (
        if body["ok"] == true {
            StatusCode::OK
        } else {
            StatusCode::BAD_GATEWAY
        },
        Json(body),
    )
        .into_response()
}

pub async fn pause_mode(State(state): State<Arc<AppState>>, request: Request) -> Response {
    let fingerprint = identity(&request);
    let idem_key = match idempotency_key(&request) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    let (mode, minutes) = match pause_mode_body(request).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match idempotency_preflight(state.command_ledger.lookup_start(
        &fingerprint,
        mode.action(),
        minutes,
        &idem_key,
    )) {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(response) => return *response,
    }
    if let Err(response) = require_operation_capability(&state, mode.operation()).await {
        return *response;
    }
    run_pause_start(state, fingerprint, idem_key, mode, minutes).await
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
    match idempotency_preflight(state.command_ledger.lookup_start(
        &fingerprint,
        "force_charge",
        minutes_u64,
        &idem_key,
    )) {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(response) => return *response,
    }
    if let Err(response) =
        require_operation_capability(&state, ExternalControlOperation::ForceCharge).await
    {
        return *response;
    }
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
    match idempotency_preflight(state.command_ledger.lookup_start(
        &fingerprint,
        "force_discharge",
        minutes_u64,
        &idem_key,
    )) {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(response) => return *response,
    }
    if let Err(response) =
        require_operation_capability(&state, ExternalControlOperation::ForceDischarge).await
    {
        return *response;
    }
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
    match idempotency_preflight(state.command_ledger.lookup_stop(
        &fingerprint,
        "force_charge",
        &idem_key,
    )) {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(response) => return *response,
    }
    if let Err(response) =
        require_stop_capability(&state, ExternalControlOperation::ForceCharge).await
    {
        return *response;
    }
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
    match idempotency_preflight(state.command_ledger.lookup_stop(
        &fingerprint,
        "force_discharge",
        &idem_key,
    )) {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(response) => return *response,
    }
    if let Err(response) =
        require_stop_capability(&state, ExternalControlOperation::ForceDischarge).await
    {
        return *response;
    }
    run_stop(state, fingerprint, idem_key, "force_discharge").await
}

pub async fn pause_mode_stop(State(state): State<Arc<AppState>>, request: Request) -> Response {
    let fingerprint = identity(&request);
    let idem_key = match idempotency_key(&request) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    match idempotency_preflight(state.command_ledger.lookup_stop(
        &fingerprint,
        "pause_mode",
        &idem_key,
    )) {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(response) => return *response,
    }
    let _action_guard = state.force_action_lock.lock().await;
    let settings = Settings::load_async().await;
    let owned = state
        .pause_mode_revert
        .lock()
        .await
        .as_ref()
        .and_then(|revert| revert.external_owner.as_ref())
        .is_some();
    if !settings.api_control_enabled && !owned {
        audit_denial(
            &state,
            AuditEvent {
                kind: "authz_denied",
                actor: Some(fingerprint),
                source: None,
                method: Some("POST".into()),
                path: Some("/api/control/pause-mode/stop".into()),
                outcome: "denied",
                detail: Some("control permission off; no external pause action owned".into()),
            },
        );
        return capability_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "External battery control is disabled",
        );
    }
    if let Err(response) =
        require_stop_capability(&state, ExternalControlOperation::PauseBoth).await
    {
        return *response;
    }

    let reservation = match state.command_ledger.reserve_stop(
        &fingerprint,
        "pause_mode",
        &idem_key,
        chrono::Utc::now().timestamp_millis(),
    ) {
        Ok(reservation) => reservation,
        Err(error) => {
            return capability_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &error)
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
        Some("pause_mode".into()),
    ) {
        let _ = state.command_ledger.mark_state(&command_id, "failed");
        return response;
    }

    let revert = state.pause_mode_revert.lock().await.clone();
    let Some(revert) = revert else {
        let body =
            json!({"ok":true,"message":"Battery pause was not active","command_id":command_id});
        let _ = state
            .command_ledger
            .mark_state(&command_id, "readback_confirmed");
        store_envelope(&state, &command_id, StatusCode::OK, &body);
        return (StatusCode::OK, Json(body)).into_response();
    };
    {
        let snapshot = state.latest_snapshot.lock().await;
        let Some(snapshot) = snapshot.as_ref() else {
            let _ = state.command_ledger.mark_state(&command_id, "failed");
            return capability_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "state_unavailable",
                "Current inverter identity is unavailable; pause restoration refused for safety",
            );
        };
        if snapshot.device_type != revert.device_type
            || snapshot.inverter_serial != revert.inverter_serial
            || snapshot.firmware_version != revert.firmware_version
        {
            let _ = state.command_ledger.mark_state(&command_id, "failed");
            return capability_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "state_unavailable",
                "The inverter identity changed; pause restoration refused for safety",
            );
        }
    }
    let _ = state.command_ledger.record_recovery(
        &command_id,
        &serde_json::to_string(&revert).unwrap_or_default(),
    );
    let writes = api::build_pause_mode_writes(
        revert.battery_pause_mode,
        revert.battery_pause_slot_start,
        revert.battery_pause_slot_end,
    );
    {
        let mut stored = state.pause_mode_revert.lock().await;
        if let Some(current) = stored.as_mut() {
            current.restoring = true;
            current.restoration_requested_at_ms = Some(chrono::Utc::now().timestamp_millis());
        }
    }
    let (rx, budget) = api::queue_owned_writes_fail_fast(
        &state,
        writes,
        crate::inverter::state_machines::DischargeControlOwner::ExplicitPause,
    )
    .await;
    let body = match api::await_write_outcome_with_timeout(rx, budget).await {
        Ok(()) => {
            let body = json!({"ok":true,"message":"Battery pause restoration queued","command_id":command_id});
            let _ = state.command_ledger.mark_state(&command_id, "queued");
            store_envelope(&state, &command_id, StatusCode::OK, &body);
            body
        }
        Err(error) => {
            let _ = state.command_ledger.finish(
                &command_id,
                "failed",
                &json!({"status":502,"body":{"ok":false,"error":error}}).to_string(),
            );
            json!({"ok":false,"error":format!("Battery pause could not be restored safely: {error}"),"command_id":command_id})
        }
    };
    let status = if body["ok"] == true {
        StatusCode::OK
    } else {
        StatusCode::BAD_GATEWAY
    };
    (status, Json(body)).into_response()
}

fn idempotency_preflight(
    result: Result<Option<Reservation>, String>,
) -> Result<Option<Response>, Box<Response>> {
    match result {
        Ok(Some(Reservation::Replayed { response })) => Ok(Some(replay_response(response))),
        Ok(Some(Reservation::InProgress { command_id })) => {
            Ok(Some(in_progress_response(command_id)))
        }
        Ok(Some(Reservation::Conflict {
            existing_command_id,
        })) => Ok(Some(conflict_response(existing_command_id))),
        Ok(None) => Ok(None),
        Err(error) => Err(Box::new(capability_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            &error,
        ))),
        Ok(Some(Reservation::Accepted { .. })) => unreachable!("lookup cannot accept a command"),
    }
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

    const ACTIONS: [&str; 6] = [
        "force-charge",
        "force-charge/stop",
        "force-discharge",
        "force-discharge/stop",
        "pause-mode",
        "pause-mode/stop",
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
            timestamp: chrono::Utc::now().timestamp(),
            device_type: DeviceType::ACCoupled,
            ..Default::default()
        });
        *state.connection_state.lock().await = ConnectionState::Connected;
        state
    }

    async fn setup_native_pause() -> Arc<AppState> {
        let state = setup(true).await;
        let mut snapshot_guard = state.latest_snapshot.lock().await;
        let snapshot = snapshot_guard.as_mut().unwrap();
        snapshot.device_type = DeviceType::AllInOne3_6kW;
        snapshot.inverter_serial = "AIO-TEST-001".into();
        snapshot.firmware_version = "400".into();
        snapshot.inverter_time = "2027-01-15 12:34:00".into();
        snapshot.battery_pause_mode_raw = Some(0);
        snapshot.battery_pause_slot_start_raw = Some(0);
        snapshot.battery_pause_slot_end_raw = Some(0);
        snapshot.battery_pause_registers_observed_at = Some(snapshot.timestamp);
        drop(snapshot_guard);
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
                let (status, body) = request(
                    state.clone(),
                    action,
                    Some("integration-key"),
                    json!({"minutes":30}),
                )
                .await;
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(body["code"], "state_unavailable");
            }
            assert!(state.pending_writes.lock().await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn unsupported_inverters_reject_every_external_mutation_before_side_effects() {
        with_isolated_config_dir_async(|| async {
            for device in [
                DeviceType::PvInverter,
                DeviceType::Ems,
                DeviceType::EmsCommercial,
                DeviceType::Gen4Hybrid,
                DeviceType::Unknown(0x9999),
            ] {
                let state = setup(true).await;
                state
                    .latest_snapshot
                    .lock()
                    .await
                    .as_mut()
                    .unwrap()
                    .device_type = device;

                for action in ACTIONS {
                    let body = if action.ends_with("/stop") {
                        Value::Null
                    } else if action == "pause-mode" {
                        json!({"mode":"pause_both","minutes":30})
                    } else {
                        json!({"minutes":30})
                    };
                    let (status, response) =
                        request(state.clone(), action, Some("integration-key"), body).await;
                    let (expected_status, expected_code) =
                        if matches!(device, DeviceType::Unknown(_)) {
                            (StatusCode::SERVICE_UNAVAILABLE, "state_unavailable")
                        } else {
                            (StatusCode::UNPROCESSABLE_ENTITY, "unsupported_control")
                        };
                    assert_eq!(status, expected_status, "{device:?} {action}");
                    assert_eq!(response["code"], expected_code);
                }

                assert!(state.pending_writes.lock().await.is_empty());
                assert!(state.force_charge_revert.lock().await.is_none());
                assert!(state.force_discharge_revert.lock().await.is_none());
                assert!(!state
                    .command_ledger
                    .has_active_start("force_charge")
                    .unwrap());
                assert!(!state
                    .command_ledger
                    .has_active_start("force_discharge")
                    .unwrap());
            }
        })
        .await;
    }

    #[tokio::test]
    async fn unsupported_inverters_keep_authenticated_reads_available() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            state
                .latest_snapshot
                .lock()
                .await
                .as_mut()
                .unwrap()
                .device_type = DeviceType::PvInverter;

            for path in ["/api/snapshot", "/api/control/status"] {
                let response = create_authenticated_router(state.clone())
                    .oneshot(
                        Request::builder()
                            .uri(path)
                            .header("Authorization", "Bearer integration-key")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK, "{path}");
            }
        })
        .await;
    }

    #[tokio::test]
    async fn pause_mode_validates_mode_duration_and_unknown_fields() {
        with_isolated_config_dir_async(|| async {
            let state = setup_native_pause().await;
            for body in [
                json!({}),
                json!({"mode":"invalid","minutes":30}),
                json!({"mode":"pause_charge","minutes":0}),
                json!({"mode":"pause_charge","minutes":1440}),
                json!({"mode":"pause_charge","minutes":1.5}),
                json!({"mode":"pause_charge","minutes":"30"}),
                json!({"mode":"pause_charge","minutes":30,"extra":true}),
                Value::Null,
            ] {
                assert_eq!(
                    request(
                        state.clone(),
                        "pause-mode",
                        Some("integration-key"),
                        body.clone()
                    )
                    .await
                    .0,
                    StatusCode::BAD_REQUEST,
                    "{body}"
                );
            }
            assert!(state.pending_writes.lock().await.is_empty());
            assert!(state.pause_mode_revert.lock().await.is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn pause_mode_rejects_corrupt_register_baseline() {
        with_isolated_config_dir_async(|| async {
            let state = setup_native_pause().await;
            state
                .latest_snapshot
                .lock()
                .await
                .as_mut()
                .unwrap()
                .battery_pause_slot_end_raw = Some(2360);
            let response = request(
                state.clone(),
                "pause-mode",
                Some("integration-key"),
                json!({"mode":"pause_both","minutes":30}),
            )
            .await;
            assert_eq!(response.0, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response.1["code"], "state_unavailable");
            assert!(state.pending_writes.lock().await.is_empty());
            assert!(state.pause_mode_revert.lock().await.is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn pause_mode_retry_replays_before_connection_guard() {
        with_isolated_config_dir_async(|| async {
            let state = setup_native_pause().await;
            let key = format!("pause-replay-{}", next_key());
            let first = request_with_key(
                state.clone(),
                "pause-mode",
                Some("integration-key"),
                &key,
                json!({"mode":"pause_both","minutes":30}),
            )
            .await;
            assert_eq!(first.0, StatusCode::OK);
            *state.connection_state.lock().await = ConnectionState::Disconnected;
            let replay = request_with_key(
                state,
                "pause-mode",
                Some("integration-key"),
                &key,
                json!({"mode":"pause_both","minutes":30}),
            )
            .await;
            assert_eq!(replay, first);
        })
        .await;
    }

    #[tokio::test]
    async fn pause_mode_start_uses_inverter_clock_and_queues_window_before_mode() {
        with_isolated_config_dir_async(|| async {
            let state = setup_native_pause().await;
            let response = request(
                state.clone(),
                "pause-mode",
                Some("integration-key"),
                json!({"mode":"both","minutes":30}),
            )
            .await;
            assert_eq!(response.0, StatusCode::OK);
            let writes: Vec<_> = state
                .pending_writes
                .lock()
                .await
                .iter()
                .flat_map(|batch| {
                    batch
                        .writes
                        .iter()
                        .map(|write| (write.address, write.value))
                })
                .collect();
            assert_eq!(writes[0], (319, 1234));
            assert_eq!(writes[1], (320, 1304));
            assert_eq!(writes[2], (318, 3));
            assert!(state.pause_mode_revert.lock().await.is_some());
            state.pending_writes.lock().await.clear();
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
                    pause_registers_supported: false,
                    battery_pause_mode_raw: None,
                    battery_pause_slot_start_raw: None,
                    battery_pause_slot_end_raw: None,
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
