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
    (status, Json(error_body(code, error))).into_response()
}

fn error_body(code: &'static str, error: &str) -> Value {
    json!({"ok": false, "code": code, "error": error})
}

fn finish_response(
    state: &Arc<AppState>,
    command_id: &str,
    status: StatusCode,
    lifecycle: &str,
    body: &Value,
) {
    let envelope = json!({"status": status.as_u16(), "body": body});
    let _ = state
        .command_ledger
        .finish(command_id, lifecycle, &envelope.to_string());
}

/// Reject an authenticated mutation unless its complete register path is
/// confirmed for a fresh, connected inverter snapshot. This runs before rate
/// limiting, command reservation, accepted-action audit, baseline capture, or
/// queueing, so unsupported hardware cannot leave any command side effects.
async fn require_operation_capability(
    state: &Arc<AppState>,
    operation: ExternalControlOperation,
) -> Result<(), Box<Response>> {
    require_operation_capability_inner(state, operation, true, true).await
}

async fn require_stop_capability(
    state: &Arc<AppState>,
    operation: ExternalControlOperation,
) -> Result<(), Box<Response>> {
    let memory_owned = match operation {
        ExternalControlOperation::ForceCharge => state.force_charge_revert.lock().await.is_some(),
        ExternalControlOperation::ForceDischarge => {
            state.force_discharge_revert.lock().await.is_some()
        }
        ExternalControlOperation::PauseBoth => state.pause_mode_revert.lock().await.is_some(),
        _ => false,
    };
    let durable_owned = match operation {
        ExternalControlOperation::ForceCharge => {
            state.command_ledger.has_active_start("force_charge")
        }
        ExternalControlOperation::ForceDischarge => {
            state.command_ledger.has_active_start("force_discharge")
        }
        ExternalControlOperation::PauseBoth => state.command_ledger.has_active_pause_control(),
        _ => Ok(false),
    }
    .map_err(|error| {
        Box::new(capability_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            &format!("Could not verify recovery ownership: {error}"),
        ))
    })?;
    // Recovery must remain available after a firmware downgrade or model
    // capability change, but an unsupported inverter with no owned recovery
    // must still be rejected like every other mutation.
    require_operation_capability_inner(state, operation, false, !(memory_owned || durable_owned))
        .await
}

async fn require_operation_capability_inner(
    state: &Arc<AppState>,
    operation: ExternalControlOperation,
    require_pause_baseline: bool,
    require_supported: bool,
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
    if require_supported && pause_requires_firmware && arm_fw.is_none() {
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
    if require_supported && !device_type.supports_external_control(operation, firmware) {
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
        let endpoint = match request.uri().path() {
            "/api/control/force-charge" => Some("/api/control/force_charge"),
            "/api/control/force-discharge" => Some("/api/control/force_discharge"),
            "/api/control/pause-mode" => Some("/api/control/pause-mode"),
            _ => None,
        };
        let replayable = endpoint.and_then(|endpoint| {
            request
                .headers()
                .get("idempotency-key")
                .and_then(|value| value.to_str().ok())
                .filter(|key| (16..=128).contains(&key.len()) && !key.contains(char::is_whitespace))
                .map(|key| (endpoint, key.to_owned()))
        });
        if let Some((endpoint, idem_key)) = replayable {
            match state.command_ledger.has_idempotency_scope(
                &identity(&request),
                endpoint,
                &idem_key,
            ) {
                Ok(true) => return next.run(request).await,
                Ok(false) => {}
                Err(error) => {
                    return capability_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "server_error",
                        &format!("Could not inspect idempotent control request: {error}"),
                    );
                }
            }
        }
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

    fn canonical(self) -> &'static str {
        match self {
            Self::Charge => "charge",
            Self::Discharge => "discharge",
            Self::Both => "both",
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
    // Validate the capability while holding the same action lock used by the
    // native handler. This keeps the snapshot/model used for admission from
    // changing before the register family is selected and the baseline is
    // captured.
    let operation = if action == "force_discharge" {
        ExternalControlOperation::ForceDischarge
    } else {
        ExternalControlOperation::ForceCharge
    };
    let force_action_guard = state.force_action_lock.lock().await;
    if let Err(response) = require_operation_capability(&state, operation).await {
        drop(force_action_guard);
        return *response;
    }
    if let Some(response) = check_start_limit(&state, &fingerprint) {
        drop(force_action_guard);
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
    // Every post-reservation exit stores the exact terminal envelope so a
    // retry with the same Idempotency-Key replays the exact response the
    // first request returned.
    if audit_action(
        &state,
        &fingerprint,
        "action_start",
        Some(format!("{action} minutes={minutes}")),
    )
    .is_some()
    {
        let body = json!({"ok": false,
            "error": "Audit logging unavailable; control request refused for safety"});
        finish_response(
            &state,
            &command_id,
            StatusCode::SERVICE_UNAVAILABLE,
            "failed",
            &body,
        );
        return (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
    }
    // Force Discharge owns the no-snapshot guard for external callers: the
    // UI cannot offer Quick Actions before its first snapshot, and an
    // unknown restore baseline must never be created.
    if action == "force_discharge" && state.latest_snapshot.lock().await.is_none() {
        let body = json!({"ok":false,
            "error":"Force Discharge requires an inverter snapshot before it can start"});
        finish_response(&state, &command_id, StatusCode::CONFLICT, "failed", &body);
        return (StatusCode::CONFLICT, Json(body)).into_response();
    }
    // Keep the shared action lock across the native start, ownership marking,
    // and durable baseline persistence. Otherwise a UI stop/start can clear or
    // replace the in-memory baseline after the native handler returns but
    // before the authenticated command records its recovery data.
    let (status, Json(mut response_body)) = if action == "force_discharge" {
        api::force_discharge_at_unlocked(
            state.clone(),
            Some(Json(json!({"minutes": minutes}))),
            chrono::Local::now(),
            Some(&command_id),
        )
        .await
    } else {
        api::force_charge_at_unlocked(
            state.clone(),
            Some(Json(json!({"minutes": minutes}))),
            chrono::Local::now(),
            Some(&command_id),
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
        // The durable baseline is the only thing that lets a later session
        // recover the inverter, and the physical writes are already queued by
        // now. If it cannot be stored, fail closed: undo the action while the
        // in-memory revert is still available, rather than reporting success
        // and leaving a forced mode that no later session can restore.
        if let Err(error) = record_recovery_baseline(&state, &command_id, action).await {
            // Compensation uses the public stop handler, which acquires the
            // same lock. Release it before entering that fail-closed path.
            drop(force_action_guard);
            return undo_start_without_baseline(&state, &command_id, action, &error).await;
        }
        store_envelope(&state, &command_id, status, &response_body);
        drop(force_action_guard);
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
    if let Err(response) = require_operation_capability(&state, mode.operation()).await {
        return *response;
    }
    let reservation = match state.command_ledger.reserve_pause_start(
        &fingerprint,
        mode.action(),
        mode.canonical(),
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

    let ledger_conflict = match state
        .command_ledger
        .has_active_battery_control_except(&command_id)
    {
        Ok(conflict) => conflict,
        Err(error) => {
            let body = error_body(
                "server_error",
                &format!("Could not verify existing battery-control ownership: {error}"),
            );
            finish_response(
                &state,
                &command_id,
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed",
                &body,
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response();
        }
    };
    if state.force_charge_revert.lock().await.is_some()
        || state.force_discharge_revert.lock().await.is_some()
        || state.pause_mode_revert.lock().await.is_some()
        || ledger_conflict
    {
        let body = error_body(
            "control_conflict",
            "Another battery control action is active",
        );
        finish_response(&state, &command_id, StatusCode::CONFLICT, "failed", &body);
        return (StatusCode::CONFLICT, Json(body)).into_response();
    }
    if audit_action(
        &state,
        &fingerprint,
        "action_start",
        Some(format!("{} minutes={minutes}", mode.action())),
    )
    .is_some()
    {
        let body = json!({"ok": false,
            "error": "Audit logging unavailable; control request refused for safety"});
        finish_response(
            &state,
            &command_id,
            StatusCode::SERVICE_UNAVAILABLE,
            "failed",
            &body,
        );
        return (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    let (start, end) = {
        let snapshot = state.latest_snapshot.lock().await;
        let Some(snapshot) = snapshot.as_ref() else {
            let body = error_body(
                "state_unavailable",
                "No inverter snapshot is available; pause request refused for safety",
            );
            finish_response(
                &state,
                &command_id,
                StatusCode::SERVICE_UNAVAILABLE,
                "failed",
                &body,
            );
            return (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
        };
        let Some(start_minute) = crate::inverter::state_machines::inverter_minute_of_day(snapshot)
        else {
            let body = error_body(
                "state_unavailable",
                "The inverter clock is unavailable; pause request refused for safety",
            );
            finish_response(
                &state,
                &command_id,
                StatusCode::SERVICE_UNAVAILABLE,
                "failed",
                &body,
            );
            return (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
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
                let body = error_body("state_unavailable", &error);
                finish_response(
                    &state,
                    &command_id,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "failed",
                    &body,
                );
                return (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
            }
        };
    revert.command_id = Some(command_id.clone());
    revert.external_owner = Some(fingerprint.clone());
    revert.requested_mode = mode.register_value();
    revert.requested_slot_start = start;
    revert.requested_slot_end = end;
    let recovery = match serde_json::to_string(&revert) {
        Ok(recovery) => recovery,
        Err(error) => {
            let message = format!("Could not serialize pause recovery baseline: {error}");
            let body = error_body("server_error", &message);
            finish_response(
                &state,
                &command_id,
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed",
                &body,
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response();
        }
    };
    if let Err(error) = state.command_ledger.record_recovery(&command_id, &recovery) {
        let message = format!("Could not persist pause recovery baseline: {error}");
        let body = error_body("server_error", &message);
        finish_response(
            &state,
            &command_id,
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed",
            &body,
        );
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response();
    }
    *state.pause_mode_revert.lock().await = Some(revert.clone());
    let writes = api::build_pause_mode_writes(mode.register_value(), start, end);
    let (rx, budget) = api::queue_owned_writes_fail_fast(
        &state,
        writes,
        crate::inverter::state_machines::DischargeControlOwner::ExplicitPause,
        Some(&command_id),
    )
    .await;
    // Mark queued before the completion await: the poll loop may extract and
    // dispatch this batch while we wait, and `mark_dispatched` only advances
    // rows already in the `queued` state.
    let _ = state.command_ledger.mark_state(&command_id, "queued");
    drop(_action_guard);
    let mut body = json!({"ok":true,"message":"Battery pause queued","command_id":command_id});
    match api::await_required_write_outcome_with_timeout(rx, budget).await {
        Ok(()) => {
            store_envelope(&state, &command_id, StatusCode::OK, &body);
        }
        Err(error) => {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let mut rollback_revert = revert.clone();
            rollback_revert.restoring = true;
            rollback_revert.restoration_requested_at_ms = Some(now_ms);
            if let Ok(recovery) = serde_json::to_string(&rollback_revert) {
                let _ = state.command_ledger.record_recovery(&command_id, &recovery);
            }
            let _ = state.command_ledger.mark_recovery_pending(&command_id);
            *state.pause_mode_revert.lock().await = Some(rollback_revert.clone());
            // The fail-fast batch may have applied its window before failing.
            // Queue exact rollback while retaining pause ownership; the poll
            // loop retries it until fresh exact readback confirms the baseline.
            let rollback = api::build_pause_mode_writes(
                rollback_revert.battery_pause_mode,
                rollback_revert.battery_pause_slot_start,
                rollback_revert.battery_pause_slot_end,
            );
            let _ = api::queue_owned_writes_fail_fast(
                &state,
                rollback,
                crate::inverter::state_machines::DischargeControlOwner::ExplicitPause,
                None,
            )
            .await;
            body = json!({"ok":false,"error":format!("Battery pause could not be applied safely: {error}"),"command_id":command_id});
            finish_response(
                &state,
                &command_id,
                StatusCode::BAD_GATEWAY,
                "failed",
                &body,
            );
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
    match idempotency_preflight(state.command_ledger.lookup_pause_start(
        &fingerprint,
        mode.canonical(),
        minutes,
        &idem_key,
    )) {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(response) => return *response,
    }
    if let Some(response) = check_start_limit(&state, &fingerprint) {
        return response;
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
    // Store the exact terminal envelope: a retry with the same
    // Idempotency-Key must replay this exact refusal.
    if audit_action(
        &state,
        &fingerprint,
        "action_stop",
        Some(action.to_string()),
    )
    .is_some()
    {
        let body = json!({"ok": false,
            "error": "Audit logging unavailable; control request refused for safety"});
        finish_response(
            &state,
            &command_id,
            StatusCode::SERVICE_UNAVAILABLE,
            "failed",
            &body,
        );
        return (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
    }
    // Persist the exact stop baseline before queuing restoration. This makes
    // readback command-specific and preserves recovery across a crash or
    // rejected Modbus batch.
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
    }
    .or_else(|| {
        state
            .command_ledger
            .active_recovery_for_action(action)
            .ok()
            .flatten()
    });
    if let Some(recovery) = recovery {
        if let Err(error) = state.command_ledger.record_recovery(&command_id, &recovery) {
            let body = error_body(
                "server_error",
                &format!("Could not persist Force restoration baseline: {error}"),
            );
            finish_response(
                &state,
                &command_id,
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed",
                &body,
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response();
        }
    }
    let (status, Json(mut response_body)) = if action == "force_charge" {
        api::force_charge_stop_with_command(state.clone(), Some(&command_id)).await
    } else {
        api::force_discharge_stop_with_command(state.clone(), Some(&command_id)).await
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
    if let Some(response) = check_stop_limit(&state, &fingerprint) {
        return response;
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
    if audit_action(
        &state,
        &fingerprint,
        "action_stop",
        Some("pause_mode".into()),
    )
    .is_some()
    {
        let body = json!({"ok": false,
            "error": "Audit logging unavailable; control request refused for safety"});
        finish_response(
            &state,
            &command_id,
            StatusCode::SERVICE_UNAVAILABLE,
            "failed",
            &body,
        );
        return (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
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
        let snapshot_guard = state.latest_snapshot.lock().await;
        let Some(snapshot) = snapshot_guard.as_ref() else {
            let body = error_body(
                "state_unavailable",
                "Current inverter identity is unavailable; pause restoration refused for safety",
            );
            finish_response(
                &state,
                &command_id,
                StatusCode::SERVICE_UNAVAILABLE,
                "failed",
                &body,
            );
            return (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
        };
        // Serial + device class only (firmware is not identity), and the same
        // release-on-provably-foreign rule the Force stops use: retaining a
        // baseline that can never be applied would block every battery control
        // forever — the pause revert refuses force and pause starts alike, and
        // its row is re-hydrated at each boot.
        if !crate::inverter::poll::force_baseline_matches_inverter(
            Some(snapshot),
            revert.device_type,
            &revert.inverter_serial,
        ) {
            let foreign = crate::inverter::poll::force_baseline_belongs_to_other_inverter(
                Some(snapshot),
                revert.device_type,
                &revert.inverter_serial,
            );
            // Release the snapshot lock before touching the revert/ledger.
            drop(snapshot_guard);
            if foreign {
                tracing::warn!(
                    captured = %revert.inverter_serial,
                    "Native pause baseline belongs to a different inverter; releasing stale ownership"
                );
                *state.pause_mode_revert.lock().await = None;
                if let Some(start_command_id) = revert.command_id.as_deref() {
                    if let Err(error) = state.command_ledger.clear_recovery(start_command_id) {
                        tracing::warn!("Failed to release pause ownership: {error}");
                    }
                }
                if let Err(error) = state.audit.record(AuditEvent {
                    kind: "stale_ownership_released",
                    actor: Some(fingerprint.clone()),
                    source: None,
                    method: Some("POST".to_string()),
                    path: Some("/api/control/pause-mode/stop".to_string()),
                    outcome: "released",
                    detail: Some(format!(
                        "pause baseline captured on {}",
                        revert.inverter_serial
                    )),
                }) {
                    tracing::warn!("Audit write failed: {error}");
                }
                let body = error_body(
                    "state_unavailable",
                    "The inverter identity changed; the stale pause baseline was released",
                );
                finish_response(
                    &state,
                    &command_id,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "failed",
                    &body,
                );
                return (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
            }
            let body = error_body(
                "state_unavailable",
                "No verified inverter identity is available; pause restoration refused for safety",
            );
            finish_response(
                &state,
                &command_id,
                StatusCode::SERVICE_UNAVAILABLE,
                "failed",
                &body,
            );
            return (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
        }
    }
    let recovery_json = match serde_json::to_string(&revert) {
        Ok(recovery) => recovery,
        Err(error) => {
            let body = error_body(
                "server_error",
                &format!("Could not serialize pause restoration baseline: {error}"),
            );
            finish_response(
                &state,
                &command_id,
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed",
                &body,
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response();
        }
    };
    if let Err(error) = state
        .command_ledger
        .record_recovery(&command_id, &recovery_json)
    {
        let body = error_body(
            "server_error",
            &format!("Could not persist pause restoration baseline: {error}"),
        );
        finish_response(
            &state,
            &command_id,
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed",
            &body,
        );
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response();
    }
    let writes = api::build_pause_mode_writes(
        revert.battery_pause_mode,
        revert.battery_pause_slot_start,
        revert.battery_pause_slot_end,
    );
    let restoration_recovery = {
        let mut stored = state.pause_mode_revert.lock().await;
        if let Some(current) = stored.as_mut() {
            current.restoring = true;
            current.restoration_requested_at_ms = Some(chrono::Utc::now().timestamp_millis());
            Some(current.clone())
        } else {
            None
        }
    };
    if let Some(recovery) = restoration_recovery {
        if let Some(start_command_id) = recovery.command_id.as_deref() {
            if let Ok(recovery_json) = serde_json::to_string(&recovery) {
                let _ = state
                    .command_ledger
                    .record_recovery(start_command_id, &recovery_json);
                let _ = state.command_ledger.mark_recovery_pending(start_command_id);
            }
        }
    }
    let (rx, budget) = api::queue_owned_writes_fail_fast(
        &state,
        writes,
        crate::inverter::state_machines::DischargeControlOwner::ExplicitPause,
        Some(&command_id),
    )
    .await;
    let _ = state.command_ledger.mark_state(&command_id, "queued");
    drop(_action_guard);
    let body = match api::await_required_write_outcome_with_timeout(rx, budget).await {
        Ok(()) => {
            let body = json!({"ok":true,"message":"Battery pause restoration queued","command_id":command_id});
            store_envelope(&state, &command_id, StatusCode::OK, &body);
            body
        }
        Err(error) => {
            let body = json!({"ok":false,"error":format!("Battery pause could not be restored safely: {error}"),"command_id":command_id});
            finish_response(
                &state,
                &command_id,
                StatusCode::BAD_GATEWAY,
                "failed",
                &body,
            );
            body
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

/// Undo a Force start whose durable recovery baseline could not be persisted.
///
/// The physical writes are already queued at this point, so the action is undone
/// through the same stop handler a user would call. Queueing is not readback:
/// the row is marked as having a pending restoration even when the compensating
/// stop returns successfully, and remains owned until a fresh exact snapshot
/// confirms it. If compensation is refused (for example the inverter identity
/// changed or the snapshot vanished), the same pending state ensures the
/// unrestored action is still surfaced to a later session.
async fn undo_start_without_baseline(
    state: &Arc<AppState>,
    command_id: &str,
    action: &str,
    error: &str,
) -> Response {
    tracing::error!("{action} start could not persist its recovery baseline: {error}");
    let (undo_status, _) = if action == "force_charge" {
        api::force_charge_stop(State(state.clone())).await
    } else {
        api::force_discharge_stop(State(state.clone())).await
    };
    // Queue acceptance is not restoration confirmation. Always re-persist the
    // captured baseline and retain the command's ownership until a fresh exact
    // readback clears it, including when the compensating stop returned 2xx.
    // Otherwise a crash or failed write after queueing would turn an unrestored
    // inverter into an unowned failed command.
    if let Err(retry_error) = record_recovery_baseline(state, command_id, action).await {
        tracing::warn!("Could not re-persist the pending compensation for {action}: {retry_error}");
    }
    if let Err(mark_error) = state.command_ledger.mark_recovery_pending(command_id) {
        tracing::warn!("Could not mark the pending compensation for {action}: {mark_error}");
    }
    let body = json!({
        "ok": false,
        "error": if undo_status.is_success() {
            format!(
                "Could not persist the recovery baseline ({error}); the action was undone to keep the inverter recoverable"
            )
        } else {
            format!(
                "Could not persist the recovery baseline ({error}); undoing the action was also refused ({})",
                undo_status.as_u16()
            )
        },
        "command_id": command_id,
    });
    let _ = state.command_ledger.finish(
        command_id,
        "failed",
        &json!({
            "status": StatusCode::INTERNAL_SERVER_ERROR.as_u16(),
            "body": body.clone(),
        })
        .to_string(),
    );
    (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
}

/// Snapshot the in-memory revert as the durable recovery baseline for a
/// start command, so a restart can reconcile instead of guessing.
///
/// The result matters: the caller must not report success when the baseline it
/// just promised did not actually persist, because that baseline is the only
/// thing that lets a later session recover the inverter.
async fn record_recovery_baseline(
    state: &Arc<AppState>,
    command_id: &str,
    action: &str,
) -> Result<(), String> {
    let recovery = if action == "force_charge" {
        let revert = state.force_charge_revert.lock().await.clone();
        let revert = revert.ok_or_else(|| format!("no {action} baseline was captured"))?;
        serde_json::to_string(&revert)
            .map_err(|error| format!("could not serialize the {action} baseline: {error}"))?
    } else {
        let revert = state.force_discharge_revert.lock().await.clone();
        let revert = revert.ok_or_else(|| format!("no {action} baseline was captured"))?;
        serde_json::to_string(&revert)
            .map_err(|error| format!("could not serialize the {action} baseline: {error}"))?
    };
    state.command_ledger.record_recovery(command_id, &recovery)
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

    // Deliberately future-dated so capability freshness checks cannot depend on
    // the wall clock while these router tests exercise replay ordering.
    const FIXED_NOW_SECS: i64 = 4_000_000_000;
    const FIXED_NOW_MS: i64 = FIXED_NOW_SECS * 1000;

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
            // Capability admission compares against the production wall clock;
            // this fixture intentionally represents a fresh read.
            timestamp: chrono::Utc::now().timestamp(),
            device_type: DeviceType::ACCoupled,
            // A real identity, so these tests exercise the identity barrier
            // rather than short-circuiting through the empty-serial path.
            inverter_serial: "HEM-TEST-001".into(),
            firmware_version: "400".into(),
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

    /// Point a test state at `device` and model a connected inverter whose
    /// pause registers have been read: Gen3Hybrid requires that baseline before
    /// Force Discharge is admitted, and both compared paths must see the same
    /// inverter for their write sets to be comparable.
    async fn set_device_with_pause_baseline(state: &Arc<AppState>, device: DeviceType) {
        let mut snapshot = state.latest_snapshot.lock().await;
        let snapshot = snapshot.as_mut().unwrap();
        snapshot.device_type = device;
        snapshot.battery_pause_mode_raw = Some(0);
        snapshot.battery_pause_slot_start_raw = Some(0);
        snapshot.battery_pause_slot_end_raw = Some(0);
        snapshot.battery_pause_registers_observed_at = Some(snapshot.timestamp);
    }

    /// Sequential key suffix so every request is a fresh idempotency scope
    /// unless a test supplies an explicit key.
    fn next_key() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        format!("test-key-{:016x}", COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Run a pause-mode start and deterministically fail its write batch by
    /// dropping the queued batch: the dropped completion channel is the same
    /// Modbus-failure path the handler sees in production. Returns once the
    /// exact terminal envelope has been stored.
    async fn fail_pause_start_with_dropped_batch(
        state: &Arc<AppState>,
        key: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let handler_state = state.clone();
        let key = key.to_string();
        let handler = tokio::spawn(async move {
            request_with_key(
                handler_state,
                "pause-mode",
                Some("integration-key"),
                &key,
                body,
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while state.pending_writes.lock().await.is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("pause request queued its fail-fast batch");
        state.pending_writes.lock().await.clear();
        tokio::time::timeout(std::time::Duration::from_secs(5), handler)
            .await
            .expect("pause request must finish once its batch is dropped")
            .expect("handler task panicked")
    }

    async fn complete_pause_start(
        state: &Arc<AppState>,
        key: &str,
        body: Value,
    ) -> (StatusCode, Value, Vec<(u16, u16)>) {
        let handler_state = state.clone();
        let key = key.to_string();
        let handler = tokio::spawn(async move {
            request_with_key(
                handler_state,
                "pause-mode",
                Some("integration-key"),
                &key,
                body,
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while state.pending_writes.lock().await.is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("pause request queued its batch");
        let batch = state.pending_writes.lock().await.pop().unwrap();
        let writes = batch
            .writes
            .iter()
            .map(|write| (write.address, write.value))
            .collect();
        batch
            .completion
            .expect("pause request has a completion channel")
            .send(crate::inverter::encoder::WriteOutcome::Ok)
            .unwrap();
        let response = tokio::time::timeout(std::time::Duration::from_secs(5), handler)
            .await
            .expect("pause request must finish after its batch succeeds")
            .expect("handler task panicked");
        (response.0, response.1, writes)
    }

    /// Same for the pause stop endpoint.
    async fn fail_pause_stop_with_dropped_batch(
        state: &Arc<AppState>,
        key: &str,
    ) -> (StatusCode, Value) {
        let handler_state = state.clone();
        let key = key.to_string();
        let handler = tokio::spawn(async move {
            request_with_key(
                handler_state,
                "pause-mode/stop",
                Some("integration-key"),
                &key,
                Value::Null,
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while state.pending_writes.lock().await.is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("pause stop queued its fail-fast batch");
        state.pending_writes.lock().await.clear();
        tokio::time::timeout(std::time::Duration::from_secs(5), handler)
            .await
            .expect("pause stop must finish once its batch is dropped")
            .expect("handler task panicked")
    }

    /// When the compensating undo is itself refused — here the snapshot
    /// vanished between the start and the undo, so the inverter identity cannot
    /// be verified — the failed row keeps a pending restoration instead of
    /// being finished as an unowned failure, so the unrestored action is still
    /// surfaced to a later session.
    #[tokio::test]
    async fn undo_refusal_keeps_the_compensation_pending() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            let command_id = match state
                .command_ledger
                .reserve_start("fp", "force_charge", 30, "undo-refused-key", 1_000)
                .unwrap()
            {
                crate::server::external_commands::Reservation::Accepted { command_id } => {
                    command_id
                }
                other => panic!("expected accepted, got {other:?}"),
            };
            // A captured baseline exists, but no inverter identity is available
            // to verify the undo against.
            *state.force_charge_revert.lock().await =
                Some(crate::inverter::poll::ForceChargeRevert {
                    started_at_ms: 0,
                    external_owner: Some("fp".into()),
                    force_charge_slot_end_ms: None,
                    enable_charge: false,
                    enable_charge_target: false,
                    device_type: DeviceType::ACCoupled,
                    inverter_serial: "SN1".into(),
                    firmware_version: "400".into(),
                    enable_discharge: false,
                    target_soc: 100,
                    battery_power_mode: 1,
                    charge_rate: None,
                    charge_slot_1_start: None,
                    charge_slot_1_end: None,
                    three_phase_force_charge_enable: None,
                    three_phase_ac_charge_enable: None,
                    battery_pause_mode: Some(0),
                });
            *state.latest_snapshot.lock().await = None;

            let response = super::undo_start_without_baseline(
                &state,
                &command_id,
                "force_charge",
                "disk full",
            )
            .await;
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

            assert!(
                state
                    .command_ledger
                    .recovery_pending_for_test(&command_id)
                    .unwrap(),
                "a refused compensation must stay pending so it is not forgotten"
            );
            assert!(
                state.force_charge_revert.lock().await.is_some(),
                "the baseline is retained for a later verified stop"
            );
            assert!(
                state.pending_writes.lock().await.is_empty(),
                "no writes may be queued while the identity is unknown"
            );
        })
        .await;
    }

    /// Even a successful compensating stop only queues writes. The failed start
    /// remains durably owned until a later fresh readback proves restoration.
    #[tokio::test]
    async fn successful_compensation_retains_recovery_until_readback() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            let command_id = match state
                .command_ledger
                .reserve_start("fp", "force_charge", 30, "undo-success-key", 1_000)
                .unwrap()
            {
                crate::server::external_commands::Reservation::Accepted { command_id } => {
                    command_id
                }
                other => panic!("expected accepted, got {other:?}"),
            };
            *state.force_charge_revert.lock().await =
                Some(crate::inverter::poll::ForceChargeRevert {
                    started_at_ms: 1_000,
                    external_owner: Some("fp".into()),
                    force_charge_slot_end_ms: None,
                    enable_charge: false,
                    enable_charge_target: false,
                    device_type: DeviceType::ACCoupled,
                    inverter_serial: "HEM-TEST-001".into(),
                    firmware_version: "400".into(),
                    enable_discharge: false,
                    target_soc: 100,
                    battery_power_mode: 1,
                    charge_rate: None,
                    charge_slot_1_start: None,
                    charge_slot_1_end: None,
                    three_phase_force_charge_enable: None,
                    three_phase_ac_charge_enable: None,
                    battery_pause_mode: Some(0),
                });

            let response = super::undo_start_without_baseline(
                &state,
                &command_id,
                "force_charge",
                "transient persistence failure",
            )
            .await;
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            assert!(
                state
                    .command_ledger
                    .recovery_pending_for_test(&command_id)
                    .unwrap(),
                "a queued successful compensation must remain pending"
            );
            assert!(
                state
                    .command_ledger
                    .active_recoveries()
                    .unwrap()
                    .iter()
                    .any(|(action, _)| action == "force_charge"),
                "the baseline must remain available after the failed start"
            );
        })
        .await;
    }

    /// A Force start must not queue physical writes before its durable recovery
    /// baseline is stored. If persistence fails, the request fails before the
    /// inverter can be changed and no in-memory owner is left behind.
    #[tokio::test]
    async fn force_start_undoes_the_action_when_the_recovery_baseline_cannot_be_persisted() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            // Make only the durable-baseline write fail. The reservation is
            // already created, but the physical write must not be queued after
            // this failure.
            let db = crate::settings::Settings::settings_dir().join("external_commands.db");
            rusqlite::Connection::open(&db)
                .unwrap()
                .execute_batch("ALTER TABLE external_commands DROP COLUMN recovery;")
                .unwrap();

            let (status, body) = request(
                state.clone(),
                "force-charge",
                Some("integration-key"),
                json!({"minutes": 30}),
            )
            .await;

            assert_eq!(
                status,
                StatusCode::INTERNAL_SERVER_ERROR,
                "a start whose baseline cannot be persisted must not report success: {body}"
            );
            assert_eq!(body["ok"], false);
            let command_id = body["command_id"].as_str().expect("command id in failure");
            assert!(
                body["error"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("recovery"),
                "the refusal must name the unpersistable recovery: {body}"
            );
            assert!(
                state.pending_writes.lock().await.is_empty(),
                "the start must not queue physical writes before persistence"
            );
            assert!(
                state.force_charge_revert.lock().await.is_none(),
                "a failed preflight must not leave an in-memory owner"
            );
            assert!(
                !state
                    .command_ledger
                    .recovery_pending_for_test(command_id)
                    .unwrap_or(false),
                "no compensation is needed when no physical write was queued"
            );
        })
        .await;
    }

    /// The idempotent "nothing to stop" pause-stop path: no revert means the
    /// pause already ended, which is reported as success rather than an error.
    #[tokio::test]
    async fn pause_mode_stop_without_an_active_pause_reports_success() {
        with_isolated_config_dir_async(|| async {
            let state = setup_native_pause().await;
            assert!(state.pause_mode_revert.lock().await.is_none());

            let (status, body) = request(
                state.clone(),
                "pause-mode/stop",
                Some("integration-key"),
                Value::Null,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["ok"], true);
            assert_eq!(body["message"], "Battery pause was not active");
        })
        .await;
    }

    /// A pause restoration must be refused after an inverter swap, and the
    /// unappliable baseline RELEASED: retaining it would refuse every force and
    /// pause start forever, across restarts, because the pause revert is
    /// re-hydrated at each boot and no path could ever clear it.
    #[tokio::test]
    async fn pause_mode_stop_releases_baseline_after_inverter_identity_change() {
        with_isolated_config_dir_async(|| async {
            let state = setup_native_pause().await;
            *state.pause_mode_revert.lock().await = Some(crate::inverter::poll::PauseModeRevert {
                started_at_ms: FIXED_NOW_MS,
                expires_at_ms: FIXED_NOW_MS + 60_000,
                command_id: None,
                restoring: false,
                restoration_requested_at_ms: None,
                external_owner: Some("integration-key".into()),
                device_type: DeviceType::AllInOne3_6kW,
                inverter_serial: "AIO-TEST-001".into(),
                firmware_version: "400".into(),
                requested_mode: 3,
                requested_slot_start: 1234,
                requested_slot_end: 1304,
                battery_pause_mode: 0,
                battery_pause_slot_start: 0,
                battery_pause_slot_end: 0,
                registers_observed_at: FIXED_NOW_SECS,
            });
            {
                let mut snapshot = state.latest_snapshot.lock().await.take().unwrap();
                snapshot.inverter_serial = "AIO-REPLACED".into();
                *state.latest_snapshot.lock().await = Some(snapshot);
            }

            let (status, body) = request(
                state.clone(),
                "pause-mode/stop",
                Some("integration-key"),
                Value::Null,
            )
            .await;
            assert_eq!(
                status,
                StatusCode::SERVICE_UNAVAILABLE,
                "a swap must refuse the restoration: {body}"
            );
            assert_eq!(body["code"], "state_unavailable");
            assert!(
                body["error"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("released"),
                "the refusal must say the stale baseline was released: {body}"
            );
            assert!(
                state.pause_mode_revert.lock().await.is_none(),
                "an unappliable pause baseline must be released, not retained forever"
            );
        })
        .await;
    }

    /// When the identity cannot be established at all (unreadable serial), the
    /// restoration is refused WITHOUT releasing: the baseline may still belong
    /// to this inverter on a later cycle.
    #[tokio::test]
    async fn pause_mode_stop_retains_baseline_when_identity_is_unverifiable() {
        with_isolated_config_dir_async(|| async {
            let state = setup_native_pause().await;
            *state.pause_mode_revert.lock().await = Some(crate::inverter::poll::PauseModeRevert {
                started_at_ms: FIXED_NOW_MS,
                expires_at_ms: FIXED_NOW_MS + 60_000,
                command_id: None,
                restoring: false,
                restoration_requested_at_ms: None,
                external_owner: Some("integration-key".into()),
                device_type: DeviceType::AllInOne3_6kW,
                inverter_serial: "AIO-TEST-001".into(),
                firmware_version: "400".into(),
                requested_mode: 3,
                requested_slot_start: 1234,
                requested_slot_end: 1304,
                battery_pause_mode: 0,
                battery_pause_slot_start: 0,
                battery_pause_slot_end: 0,
                registers_observed_at: FIXED_NOW_SECS,
            });
            {
                let mut snapshot = state.latest_snapshot.lock().await.take().unwrap();
                snapshot.inverter_serial = String::new(); // unreadable this cycle
                *state.latest_snapshot.lock().await = Some(snapshot);
            }

            let (status, body) = request(
                state.clone(),
                "pause-mode/stop",
                Some("integration-key"),
                Value::Null,
            )
            .await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
            assert!(
                state.pause_mode_revert.lock().await.is_some(),
                "an unverifiable baseline must be retained for a verified inverter"
            );
        })
        .await;
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
            let (status, body, _) =
                complete_pause_start(&state, &key, json!({"mode":"pause_both","minutes":30})).await;
            let first = (status, body);
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
    async fn pause_mode_start_uses_the_external_start_budget() {
        with_isolated_config_dir_async(|| async {
            let state = setup_native_pause().await;
            let fingerprint = crate::server::audit::token_fingerprint("integration-key");
            for _ in 0..2 {
                assert!(
                    state
                        .action_start_limiter
                        .lock()
                        .check(fingerprint.clone())
                        .allowed
                );
            }
            state
                .command_ledger
                .reserve_start(&fingerprint, "pause_both", 30, "active-key", 1_000)
                .unwrap();

            let (status, body) = request(
                state,
                "pause-mode",
                Some("integration-key"),
                json!({"mode":"pause_both","minutes":30}),
            )
            .await;
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
            assert!(body["error"].as_str().unwrap().contains("Too many"));
        })
        .await;
    }

    #[tokio::test]
    async fn pause_mode_stop_uses_the_external_stop_budget() {
        with_isolated_config_dir_async(|| async {
            let state = setup_native_pause().await;
            let fingerprint = crate::server::audit::token_fingerprint("integration-key");
            for _ in 0..10 {
                assert!(
                    state
                        .action_stop_limiter
                        .lock()
                        .check(fingerprint.clone())
                        .allowed
                );
            }

            let (status, body) = request(
                state,
                "pause-mode/stop",
                Some("integration-key"),
                Value::Null,
            )
            .await;
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
            assert!(body["error"].as_str().unwrap().contains("Too many"));
        })
        .await;
    }

    #[tokio::test]
    async fn pause_mode_start_uses_inverter_clock_and_queues_window_before_mode() {
        with_isolated_config_dir_async(|| async {
            let state = setup_native_pause().await;
            let (status, _body, writes) =
                complete_pause_start(&state, &next_key(), json!({"mode":"both","minutes":30}))
                    .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(writes[0], (319, 1234));
            assert_eq!(writes[1], (320, 1304));
            assert_eq!(writes[2], (318, 3));
            assert!(state.pause_mode_revert.lock().await.is_some());
            state.pending_writes.lock().await.clear();
        })
        .await;
    }

    /// Pause idempotency: aliases canonicalize to one scope, the same key
    /// with the same canonical mode + duration replays the exact stored
    /// response, and any payload difference conflicts.
    #[tokio::test]
    async fn pause_idempotency_canonicalizes_aliases_and_replays_exactly() {
        with_isolated_config_dir_async(|| async {
            let state = setup_native_pause().await;
            let key = "pause-alias-key-000001";

            let first = fail_pause_start_with_dropped_batch(
                &state,
                key,
                json!({"mode":"pause_charge","minutes":30}),
            )
            .await;
            assert_eq!(first.0, StatusCode::BAD_GATEWAY);
            // The failed start queues a fire-and-forget rollback batch; drop
            // it so the replay assertions below see exactly what a replay
            // adds (nothing).
            state.pending_writes.lock().await.clear();

            // The canonical alias replays the EXACT stored response without
            // queueing anything new (so no batch-drop is needed: replay
            // resolves before any queueing).
            let replay = request_with_key(
                state.clone(),
                "pause-mode",
                Some("integration-key"),
                key,
                json!({"mode":"charge","minutes":30}),
            )
            .await;
            assert_eq!(replay.0, first.0);
            assert_eq!(replay.1, first.1, "replay must return the exact body");
            assert!(state.pending_writes.lock().await.is_empty());

            // Same key with a different mode or duration: conflict.
            for body in [
                json!({"mode":"discharge","minutes":30}),
                json!({"mode":"charge","minutes":45}),
                json!({"mode":"pause_charge","minutes":45}),
            ] {
                let (status, _) = request_with_key(
                    state.clone(),
                    "pause-mode",
                    Some("integration-key"),
                    key,
                    body,
                )
                .await;
                assert_eq!(status, StatusCode::CONFLICT);
            }
        })
        .await;
    }

    /// A failed pause start replays its exact terminal envelope even after
    /// mutable connection/capability/rate-limit state has changed: replay is
    /// resolved from the ledger before any live-state guard runs.
    #[tokio::test]
    async fn failed_pause_start_replay_ignores_mutable_state_changes() {
        with_isolated_config_dir_async(|| async {
            let state = setup_native_pause().await;
            let key = "pause-fail-key-00001";
            let first = fail_pause_start_with_dropped_batch(
                &state,
                key,
                json!({"mode":"both","minutes":30}),
            )
            .await;
            assert_eq!(first.0, StatusCode::BAD_GATEWAY);

            // Change every piece of live state the pre-replay guards read.
            *state.connection_state.lock().await = ConnectionState::Reconnecting;
            *state.latest_snapshot.lock().await = None;
            state.action_start_limiter.lock().clear();

            let replay = request_with_key(
                state.clone(),
                "pause-mode",
                Some("integration-key"),
                key,
                json!({"mode":"both","minutes":30}),
            )
            .await;
            assert_eq!(replay.0, first.0);
            assert_eq!(
                replay.1, first.1,
                "replay must be byte-exact at the JSON level"
            );
        })
        .await;
    }

    /// A failed pause stop likewise replays its exact terminal envelope.
    #[tokio::test]
    async fn failed_pause_stop_replays_exactly() {
        with_isolated_config_dir_async(|| async {
            let state = setup_native_pause().await;
            // First fail a start so an externally-owned pause exists.
            fail_pause_start_with_dropped_batch(
                &state,
                "pause-stop-setup-key-1",
                json!({"mode":"both","minutes":30}),
            )
            .await;
            assert!(state.pause_mode_revert.lock().await.is_some());
            // Drop the failed start's rollback batch so the stop helper waits
            // on the stop's own batch.
            state.pending_writes.lock().await.clear();

            let key = "pause-stop-key-0000001";
            let first = fail_pause_stop_with_dropped_batch(&state, key).await;
            assert_eq!(first.0, StatusCode::BAD_GATEWAY);
            assert!(state.pending_writes.lock().await.is_empty());

            // Mutable state changes must not alter the replay.
            *state.connection_state.lock().await = ConnectionState::Reconnecting;
            state.action_stop_limiter.lock().clear();

            let replay = request_with_key(
                state.clone(),
                "pause-mode/stop",
                Some("integration-key"),
                key,
                Value::Null,
            )
            .await;
            assert_eq!(replay.0, first.0);
            assert_eq!(replay.1, first.1, "stop replay must return the exact body");
            assert!(state.pending_writes.lock().await.is_empty());
        })
        .await;
    }

    /// Regression for the pause deadlock: `run_pause_start` must drop
    /// `force_action_lock` after queueing its fail-fast batch and BEFORE
    /// awaiting the write completion. Poll-loop snapshot publication needs
    /// that same lock, so a request holding it across the await would
    /// deadlock against the very poll cycle it is waiting on. The contender
    /// here completes only while the request is still awaiting its write
    /// outcome — with the lock held across the await both sides would stall
    /// until the (deliberately short) test timeout.
    #[tokio::test]
    async fn pause_start_does_not_hold_the_action_lock_while_awaiting_writes() {
        with_isolated_config_dir_async(|| async {
            let state = setup_native_pause().await;
            let handler_state = state.clone();
            let handler = tokio::spawn(async move {
                request(
                    handler_state,
                    "pause-mode",
                    Some("integration-key"),
                    json!({"mode":"both","minutes":30}),
                )
                .await
                .0
            });

            // Wait for the request to queue its fail-fast batch.
            let batch = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let popped = state.pending_writes.lock().await.pop();
                    if let Some(batch) = popped {
                        break batch;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("pause request queued its write batch");

            // While the request is awaiting the batch outcome, a
            // poll-publication-style contender must be able to take the
            // action lock. We only complete the batch AFTER the contender
            // acquired the lock — with the lock held across the request's
            // await, the contender can never proceed and the test times out.
            let contender_state = state.clone();
            let mut contender = tokio::spawn(async move {
                let _guard = contender_state.force_action_lock.lock().await;
                "acquired"
            });
            tokio::time::timeout(std::time::Duration::from_secs(2), &mut contender)
                .await
                .expect(
                    "contender must acquire the action lock while the request awaits its writes",
                )
                .expect("contender task panicked");

            if let Some(tx) = batch.completion {
                let _ = tx.send(crate::inverter::encoder::WriteOutcome::Ok);
            }
            let status = tokio::time::timeout(std::time::Duration::from_secs(5), handler)
                .await
                .expect("pause request must complete once its batch outcome arrives")
                .expect("handler task panicked");
            assert_eq!(status, StatusCode::OK);
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
                set_device_with_pause_baseline(&state, device).await;
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
                        .collect();
                    let direct = setup(true).await;
                    set_device_with_pause_baseline(&direct, device).await;
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
                    assert_eq!(
                        actual
                            .iter()
                            .map(|w| (w.address, w.value))
                            .collect::<Vec<_>>(),
                        expected
                    );

                    // The stop retains ownership until a causally fresh exact
                    // readback; confirm it so the next loop iteration starts
                    // from a clean slate.
                    assert!(
                        state.force_charge_revert.lock().await.is_some()
                            || state.force_discharge_revert.lock().await.is_some(),
                        "stop must retain the revert until fresh exact readback"
                    );
                    let base = state.latest_snapshot.lock().await.clone().unwrap();
                    let fresh_ts = FIXED_NOW_SECS + 2;
                    let confirming = api::tests::snapshot_decoding_writes(&base, &actual, fresh_ts);
                    crate::inverter::poll::clear_confirmed_force_restorations(&state, &confirming)
                        .await;
                    assert!(state.force_charge_revert.lock().await.is_none());
                    assert!(state.force_discharge_revert.lock().await.is_none());
                    state.force_charge_restoration.lock().await.take();
                    state.force_discharge_restoration.lock().await.take();
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
            // Anchor the action window to the fresh fixture reading created by
            // setup, so the route's real-clock freshness check remains valid.
            let now_secs = state
                .latest_snapshot
                .lock()
                .await
                .as_ref()
                .expect("setup creates a snapshot")
                .timestamp;
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
                    enable_charge_target: false,
                    device_type: DeviceType::ACCoupled,
                    inverter_serial: "TEST".into(),
                    firmware_version: String::new(),
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
                    enable_charge_target: false,
                    device_type: DeviceType::ACCoupled,
                    inverter_serial: "TEST".into(),
                    firmware_version: String::new(),
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
            // Drop the start's own queued writes so the recovery-stop
            // comparison below sees only the stop batch.
            state.pending_writes.lock().await.clear();
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
            // Ownership is retained until a causally fresh exact readback
            // confirms the restoration writes — even when the recovery stop
            // ran under revoked start permission.
            let stop_writes: Vec<_> = state
                .pending_writes
                .lock()
                .await
                .drain(..)
                .flat_map(|b| b.writes)
                .collect();
            assert!(
                !stop_writes.is_empty(),
                "the recovery stop queued restoration writes"
            );
            assert!(
                state.force_charge_revert.lock().await.is_some(),
                "the recovery stop retains ownership until fresh exact readback"
            );
            let base = state.latest_snapshot.lock().await.clone().unwrap();
            let fresh_ts = FIXED_NOW_SECS + 2;
            let confirming = api::tests::snapshot_decoding_writes(&base, &stop_writes, fresh_ts);
            crate::inverter::poll::clear_confirmed_force_restorations(&state, &confirming).await;
            assert!(
                state.force_charge_revert.lock().await.is_none(),
                "fresh exact readback releases the recovered action"
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

    #[tokio::test]
    async fn external_replay_survives_permission_revocation() {
        with_isolated_config_dir_async(|| async {
            let state = setup(true).await;
            let router = create_authenticated_router(state.clone());
            let key = format!("permission-replay-{}", next_key());
            let request = || {
                Request::builder()
                    .method("POST")
                    .uri("/api/control/force-charge")
                    .header("Authorization", "Bearer integration-key")
                    .header("Idempotency-Key", &key)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"minutes":30}"#))
                    .unwrap()
            };
            let first = router.clone().oneshot(request()).await.unwrap();
            let first_status = first.status();
            let first_body = to_bytes(first.into_body(), usize::MAX).await.unwrap();
            assert_eq!(first_status, StatusCode::OK);

            let _ = api::update_settings(State(state), Json(json!({"api_control_enabled": false})))
                .await;
            let replay = router.oneshot(request()).await.unwrap();
            let replay_status = replay.status();
            let replay_body = to_bytes(replay.into_body(), usize::MAX).await.unwrap();
            assert_eq!(replay_status, first_status);
            assert_eq!(replay_body, first_body);
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
            let pending = state.pending_writes.lock().await;
            assert!(
                pending
                    .iter()
                    .any(|batch| batch.command_id.as_deref() == Some(command_id.as_str())),
                "external Force batches must retain their command id for dispatch tracking"
            );
            drop(pending);

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
