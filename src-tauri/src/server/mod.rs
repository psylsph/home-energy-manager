//! Local HTTP/WebSocket server.
//!
//! Exposes inverter data and control endpoints via an Axum-based
//! HTTP API and a WebSocket real-time data stream.

pub mod api;
pub mod audit;
pub mod authenticated_lifecycle;
mod control_status;
pub mod external_commands;
mod external_control;
pub mod external_snapshot;
pub mod logs;
pub mod mini;
pub mod ratelimit;
pub mod ws;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;
use tokio::sync::watch;
use tower::ServiceBuilder;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

use crate::inverter::poll::AppState;
use audit::AuditEvent;

// ---------------------------------------------------------------------------
// Authenticated API abuse-resistance constants (U3)
// ---------------------------------------------------------------------------

/// Failed authentications accepted per source address per minute before the
/// source is locked out of the authenticated API.
pub const FAILED_AUTH_LIMIT: u32 = 10;
/// Authenticated reads per source address per minute.
pub const READ_LIMIT: u32 = 120;
/// External control starts per credential identity per minute.
pub const ACTION_START_LIMIT: u32 = 2;
/// External control stops (incl. recovery stops) per credential identity per
/// minute — deliberately higher than starts so recovery is never starved.
pub const ACTION_STOP_LIMIT: u32 = 10;
/// Upper bound on distinct keys per limiter map (memory bound under address
/// or token flooding).
pub const RATE_LIMITER_MAX_ENTRIES: usize = 4096;
/// Hard wall-clock budget for any single request on the authenticated API.
pub const REQUEST_TIMEOUT_SECS: u64 = 30;
/// Route-level cap on control request bodies. The external contract is a
/// single `minutes` field; anything beyond a few bytes is noise or abuse.
pub const CONTROL_BODY_LIMIT_BYTES: usize = 16 * 1024;

/// The verified credential identity for a request, inserted by the auth
/// middleware and consumed by the action limiters / audit records.
#[derive(Debug, Clone)]
pub struct AuthenticatedIdentity {
    /// One-way fingerprint of the presented token.
    pub fingerprint: String,
}

pub fn create_router(state: Arc<AppState>) -> Router {
    create_router_at(state, None)
}

/// Build the normal router surface with a fixed forecast clock.
///
/// This is a deterministic integration-test seam: production callers use
/// [`create_router`], whose forecast handlers read `Local::now()` once per
/// request. Both forecast routes share the supplied instant so tests exercise
/// the real HTTP/JSON surface without depending on wall-clock time.
#[doc(hidden)]
pub fn create_router_with_forecast_now(
    state: Arc<AppState>,
    now: chrono::DateTime<chrono::Local>,
) -> Router {
    create_router_at(state, Some(now))
}

fn create_router_at(
    state: Arc<AppState>,
    forecast_now: Option<chrono::DateTime<chrono::Local>>,
) -> Router {
    use axum::response::IntoResponse;

    async fn not_found_404() -> impl IntoResponse {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "error": "Not found" })),
        )
    }
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);
    let forecast_route = match forecast_now {
        Some(now) => get(move |State(state): State<Arc<AppState>>| async move {
            api::get_forecast_at(state, now).await
        }),
        None => get(api::get_forecast),
    };
    let forecast_plan_route = match forecast_now {
        Some(now) => get(move |State(state): State<Arc<AppState>>| async move {
            api::get_forecast_plan_at(state, now).await
        }),
        None => get(api::get_forecast_plan),
    };

    Router::new()
        // Data endpoints
        .route("/api/snapshot", get(api::get_snapshot))
        .route("/api/status", get(api::get_status))
        // Issue #289: HEM-managed Timed Export schedule state — the
        // desired slots + boundary state-machine state, so the UI can
        // show Configured slots even when the physical inverter slots
        // are temporarily cleared (HR59 re-arm fallback).
        .route("/api/timed-export", get(api::get_timed_export))
        // Mini display — tokenless, read-only glance summary for an Apple
        // Watch or any small-screen browser (INSTALL.md → “Glance from
        // your Apple Watch”). Deliberately a separate, minimal-field surface
        // rather than a filtered /api/snapshot; see server::mini docs.
        .route("/api/mini/status", get(mini::mini_status))
        // Tiny self-contained GUI page that renders the mini status. Open
        // this URL in a phone/watch browser; it fetches /api/mini/status.
        .route("/mini", get(mini::mini_page))
        .route(
            "/api/settings",
            get(api::get_settings)
                .post(api::update_settings)
                .layer(middleware::from_fn(require_local_for_api_security_fields)),
        )
        .route("/api/history", get(api::get_history))
        .route("/api/history/summary", get(api::get_history_summary))
        // Cost totals for the Power page Consumption Report (issue #131).
        // Accepts the same range/offset/explicit-window params as
        // /api/history but returns a flat JSON object with per-direction
        // cost totals + standing-charge breakdown.
        .route("/api/report", get(api::get_report))
        // Control endpoints
        .route("/api/control/mode", post(api::set_mode))
        .route("/api/control/eco", post(api::set_eco))
        .route("/api/control/timed-charge", post(api::set_timed_charge))
        .route("/api/control/timed-export", post(api::set_timed_export))
        .route(
            "/api/control/timed-discharge",
            post(api::set_timed_discharge),
        )
        .route("/api/control/charge-slot", post(api::set_charge_slot))
        .route("/api/control/discharge-slot", post(api::set_discharge_slot))
        .route("/api/control/reserve", post(api::set_reserve))
        .route("/api/control/charge-rate", post(api::set_charge_rate))
        .route("/api/control/discharge-rate", post(api::set_discharge_rate))
        .route("/api/control/eps", post(api::set_eps))
        .route(
            "/api/control/active-power-rate",
            post(api::set_active_power_rate),
        )
        .route("/api/control/export-limit", post(api::set_export_limit))
        .route("/api/control/pause", post(api::pause_battery))
        .route("/api/control/unpause", post(api::unpause_battery))
        .route("/api/control/force-charge", post(api::force_charge))
        .route(
            "/api/control/force-charge/stop",
            post(api::force_charge_stop),
        )
        .route("/api/control/force-discharge", post(api::force_discharge))
        .route(
            "/api/control/force-discharge/stop",
            post(api::force_discharge_stop),
        )
        .route("/api/control/sync-clock", post(api::sync_clock))
        .route("/api/control/calibration", post(api::set_calibration))
        .route("/api/control/reboot", post(api::reboot_inverter))
        // E2E-harness-only state reset (answers 404 without --e2e-admin).
        .route("/api/test/reset", post(api::test_reset))
        // Auto winter mode
        .route(
            "/api/auto-winter",
            get(api::get_auto_winter).post(api::set_auto_winter),
        )
        // Unified charging-mode selection and Adaptive Charge
        .route(
            "/api/charging-mode",
            get(api::get_charging_mode).post(api::set_charging_mode),
        )
        .route(
            "/api/adaptive-charge",
            get(api::get_adaptive_charge).post(api::set_adaptive_charge),
        )
        // Cosy charging
        .route("/api/cosy", get(api::get_cosy).post(api::set_cosy))
        // Agile Octopus battery automation
        .route("/api/agile", get(api::get_agile).post(api::set_agile))
        // Authenticated Octopus customer consumption (issue #212)
        .route("/api/octopus/status", get(crate::octopus::get_status))
        .route("/api/octopus/sync", post(crate::octopus::start_sync))
        .route("/api/octopus/history", get(crate::octopus::get_history))
        .route("/api/octopus/summary", get(crate::octopus::get_summary))
        .route(
            "/api/octopus/comparison",
            get(crate::octopus::get_comparison),
        )
        // "New version available" detection. Read-only cache populated by
        // the background `run_update_loop`; never fetches on the request path.
        .route(
            "/api/latest-version",
            get(crate::update::get_latest_version),
        )
        // Load discharge limiter
        .route(
            "/api/load-limiter",
            get(api::get_load_limiter).post(api::set_load_limiter),
        )
        // Inverter-temperature discharge protection
        .route(
            "/api/temperature-limiter",
            get(api::get_temperature_limiter).post(api::set_temperature_limiter),
        )
        // Discharge floor guard (developer mode only)
        .route(
            "/api/discharge-floor",
            get(api::get_discharge_floor).post(api::set_discharge_floor),
        )
        // Email alerts
        .route("/api/alerts", get(api::get_alerts).post(api::set_alerts))
        .route("/api/alerts/test", post(api::test_alerts))
        // Weather (Open-Meteo integration)
        .route("/api/weather", get(api::get_weather).post(api::set_weather))
        .route("/api/weather/backfill", post(api::backfill_weather))
        .route("/api/forecast", forecast_route)
        .route("/api/forecast/plan", forecast_plan_route)
        // Reconnect control
        .route("/api/reconnect", post(api::post_reconnect))
        // Discovery
        .route("/api/discover", get(api::discover))
        .route("/api/evc/discover", get(api::evc_discover))
        // EVC reachability snapshot (issue #138) — lets the frontend
        // seed `evcEverConnected` on page load without waiting for the
        // next WS broadcast.
        .route("/api/evc/status", get(api::evc_status))
        // Developer logs
        .route("/api/logs", get(logs::get_logs))
        .route(
            "/api/log-level",
            get(logs::get_log_level).put(logs::set_log_level),
        )
        // WebSocket real-time stream
        .route("/ws", get(ws::ws_handler))
        .layer(cors)
        .with_state(state)
        // Unknown /api/* paths should return 404, not serve index.html.
        .route("/api/{*rest}", get(not_found_404).post(not_found_404))
}

/// Cache-Control policy for the bundled frontend.
///
/// Vite content-hashes every JS/CSS chunk into `/assets/` (e.g.
/// `index-j_xyKjm8.js`), so those filenames change whenever the content does —
/// they are safe to cache immutably for a year. Everything else `ServeDir`
/// hands out (`index.html`, `manifest.json`, `favicon.svg`, PWA icons) is *not*
/// hashed, so it must revalidate on every request.
///
/// `tower-http`'s `ServeDir` emits `Last-Modified`/`ETag` but never
/// `Cache-Control`. Without an explicit directive the embedded WebView falls
/// back to heuristic caching and will keep reusing a stale `index.html` after
/// an app upgrade — and that stale `index.html` points at the previous
/// version's hashed asset filenames, so the old UI renders on every fresh
/// launch until the user force-refreshes (see issue #80). Marking `index.html`
/// `no-cache` forces a conditional request each launch; `ServeDir` answers
/// `304 Not Modified` when it is unchanged, so the steady-state cost is one
/// tiny round-trip rather than a re-download.
async fn static_cache_control(request: Request, next: Next) -> Response {
    let immutable = request.uri().path().starts_with("/assets/");
    let mut response = next.run(request).await;
    let value = if immutable {
        HeaderValue::from_static("public, max-age=31536000, immutable")
    } else {
        HeaderValue::from_static("no-cache")
    };
    response.headers_mut().insert(CACHE_CONTROL, value);
    response
}

/// Build the Axum router with API routes + frontend static file serving.
///
/// In production Tauri builds, the window navigates to `http://127.0.0.1:7337`
/// so that API/WebSocket calls are same-origin (avoids WebView2 cross-origin
/// blocking). The bundled `dist/` resources serve the Vite output.
pub fn create_router_with_frontend(state: Arc<AppState>, dist_dir: &str) -> Router {
    let router = create_router(state);
    let serve_dir =
        ServeDir::new(dist_dir).fallback(ServeDir::new(format!("{}/index.html", dist_dir)));
    router.fallback_service(
        ServiceBuilder::new()
            .layer(middleware::from_fn(static_cache_control))
            .service(serve_dir),
    )
}

/// Start the HTTP server (API + WebSocket only, no frontend serving).
pub async fn start_server(state: Arc<AppState>, bind_addr: &str, port: u16) {
    let app = create_router(state).into_make_service_with_connect_info::<std::net::SocketAddr>();
    let addr = format!("{}:{}", bind_addr, port);
    tracing::info!("HTTP server starting on {}", addr);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind HTTP server on {}: {e}", addr);
            return;
        }
    };
    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!("HTTP server error: {e}");
    }
}

/// Start the HTTP server with frontend static file serving.
pub async fn start_server_with_frontend(
    state: Arc<AppState>,
    bind_addr: &str,
    port: u16,
    dist_dir: String,
) {
    let app = create_router_with_frontend(state, &dist_dir)
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    let addr = format!("{}:{}", bind_addr, port);
    tracing::info!(
        "HTTP server starting on {} (serving frontend from {})",
        addr,
        dist_dir
    );
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind HTTP server on {}: {e}", addr);
            return;
        }
    };
    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!("HTTP server error: {e}");
    }
}

/// Start the HTTP server with frontend static file serving on a single port.
///
/// Desktop Tauri windows navigate to the Axum origin for same-origin API and
/// WebSocket access. Only the requested `port` is ever bound — if it is already
/// taken (typically another Home Energy Manager instance still running, or some
/// other process squatting on the port) the function reports a clear error
/// rather than silently grabbing the next free port. This keeps the app on the
/// configured port (GUI `http_port` or headless `--port`) and avoids the
/// confusion of a second server running on an unexpected port while an existing
/// instance answers on the configured one.
///
/// `bound_tx` receives `Ok(port)` once the bind succeeds (before serving begins)
/// or `Err(message)` with a user-facing explanation, so the desktop window
/// navigates only after a successful bind and surfaces a clear error otherwise.
pub async fn start_server_with_frontend_on_port(
    state: Arc<AppState>,
    bind_addr: &str,
    port: u16,
    dist_dir: String,
    bound_tx: std::sync::mpsc::Sender<Result<u16, String>>,
) {
    let addr = format!("{}:{}", bind_addr, port);
    tracing::info!(
        "HTTP server attempting bind on {} (serving frontend from {})",
        addr,
        dist_dir
    );

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(e) => {
            let message = if e.kind() == std::io::ErrorKind::AddrInUse {
                format!(
                    "Port {port} is already in use. Another Home Energy Manager instance is \
                     likely already running — quit it (or change the port in Settings / via \
                     --port) and reopen the app. (Details: {e})"
                )
            } else {
                format!("Failed to bind HTTP server on {addr}: {e}")
            };
            tracing::error!("HTTP server bind failed: {message}");
            let _ = bound_tx.send(Err(message));
            return;
        }
    };

    tracing::info!("HTTP server bound on {}", addr);
    let _ = bound_tx.send(Ok(port));
    let app = create_router_with_frontend(state, &dist_dir)
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!("HTTP server error: {e}");
    }
}

// ---------------------------------------------------------------------------
// Authenticated API server (read-only by default, optional Quick Actions)
// ---------------------------------------------------------------------------

/// Settings fields that change the authenticated external API's security
/// posture: its credential, control permission, and network exposure. The
/// main server stays permissive for ordinary dashboard settings, but this
/// control-plane subset may only be changed from the machine running HEM —
/// a remote, tokenless caller must not be able to grant itself an API
/// credential or switch on external battery control. (`api_bind_address`
/// and `api_allowed_origins` are introduced by the listener hardening.)
const API_SECURITY_SETTINGS_FIELDS: &[&str] = &[
    "api_key",
    "api_key_generate",
    "api_port",
    "api_control_enabled",
    "api_bind_address",
    "api_allowed_origins",
];

/// POST /api/settings on the main server, gated to local callers for the
/// API-security field subset. Requests arriving from a non-loopback peer
/// that touch any of [`API_SECURITY_SETTINGS_FIELDS`] are rejected with 403;
/// every other settings write keeps its existing tokenless behaviour.
/// In-process callers without connection info (unit-test oneshots) are
/// treated as local: production servers always build the make-service with
/// `with_connect_info`, so real requests always carry a peer address.
async fn require_local_for_api_security_fields(req: Request, next: Next) -> Response {
    // Buffer the (small) settings JSON so field names can be inspected and
    // the body re-attached for the handler.
    const MAX_SETTINGS_BODY_BYTES: usize = 1024 * 1024;
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_SETTINGS_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": "Invalid request body"})),
            )
                .into_response();
        }
    };
    let touches_security = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .map(|value| {
            API_SECURITY_SETTINGS_FIELDS
                .iter()
                .any(|field| value.get(field).is_some())
        })
        .unwrap_or(false);
    if touches_security {
        use axum::extract::connect_info::MockConnectInfo;
        let peer_ip = parts
            .extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|info| info.0.ip())
            .or_else(|| {
                // Mirrors the ConnectInfo extractor's test fallback.
                parts
                    .extensions
                    .get::<MockConnectInfo<std::net::SocketAddr>>()
                    .map(|mock| mock.0.ip())
            });
        let is_local = peer_ip.map(|ip| ip.is_loopback()).unwrap_or(true);
        if !is_local {
            tracing::warn!(
                "Rejected non-local settings write to authenticated API security fields"
            );
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"ok": false,
                "error": "Authenticated API settings can only be changed from the machine running Home Energy Manager"})),
            )
                .into_response();
        }
    }
    next.run(Request::from_parts(parts, axum::body::Body::from(bytes)))
        .await
}

fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"ok": false, "error": "Unauthorized: invalid or missing API key"})),
    )
        .into_response()
}

fn too_many_requests_response(retry_after_secs: u64) -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({"ok": false,
        "error": "Too many requests; retry later"})),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&retry_after_secs.to_string()) {
        response.headers_mut().insert("retry-after", value);
    }
    response
}

fn bearer_token(req: &Request) -> Option<&str> {
    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())?;
    auth_header.strip_prefix("Bearer ")
}

/// Resolve the client address for rate limiting and audit records: the
/// direct socket peer, or — only when the peer is an explicitly trusted
/// proxy — the first address in `X-Forwarded-For`. Forwarded headers from
/// untrusted peers are ignored, so a direct client cannot spoof its source.
fn client_ip(req: &Request, trusted: &[std::net::IpAddr]) -> Option<std::net::IpAddr> {
    use axum::extract::connect_info::MockConnectInfo;
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip())
        // Mirrors the ConnectInfo extractor's test fallback.
        .or_else(|| {
            req.extensions()
                .get::<MockConnectInfo<std::net::SocketAddr>>()
                .map(|mock| mock.0.ip())
        })?;
    if trusted.contains(&peer) {
        if let Some(forwarded) = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
        {
            if let Some(first) = forwarded.split(',').next() {
                if let Ok(ip) = first.trim().parse::<std::net::IpAddr>() {
                    return Some(ip);
                }
            }
        }
    }
    Some(peer)
}

/// Bounded wall-clock budget for every request on the authenticated API.
async fn request_timeout(req: Request, next: Next) -> Response {
    match tokio::time::timeout(
        std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS),
        next.run(req),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(json!({"ok": false, "error": "Request timed out"})),
        )
            .into_response(),
    }
}

/// Per-source limit on authenticated reads (snapshot + status).
async fn limit_authenticated_reads(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let settings = crate::settings::Settings::load_async().await;
    let trusted = parse_trusted_proxies(&settings.api_trusted_proxies);
    let source = client_ip(&req, &trusted);
    if let Some(ip) = source {
        let limited = state.read_limiter.lock().is_limited(&ip);
        if !limited.allowed {
            tracing::warn!("Authenticated read rate limit hit for {ip}");
            audit_event(
                &state,
                AuditEvent {
                    kind: "read_rate_limited",
                    actor: bearer_token(&req).map(audit::token_fingerprint),
                    source: Some(ip.to_string()),
                    method: Some(req.method().to_string()),
                    path: Some(req.uri().path().to_string()),
                    outcome: "rate_limited",
                    detail: None,
                },
            );
            return too_many_requests_response(limited.retry_after_secs);
        }
        let _ = state.read_limiter.lock().check(ip);
    }
    next.run(req).await
}

/// Parse the trusted-proxy list, skipping invalid entries with a warning.
fn parse_trusted_proxies(entries: &[String]) -> Vec<std::net::IpAddr> {
    entries
        .iter()
        .filter_map(|entry| match entry.trim().parse::<std::net::IpAddr>() {
            Ok(ip) => Some(ip),
            Err(_) => {
                tracing::warn!("Ignoring invalid api_trusted_proxies entry {entry:?}");
                None
            }
        })
        .collect()
}

/// API key authentication middleware.
///
/// Verifies the `Bearer <key>` token against the configured credential:
/// a generated verifier in constant time, or the legacy plaintext during
/// its one-time migration state. Returns 401 Unauthorized if the key is
/// missing or doesn't match, and throttles sources that repeatedly fail
/// (U3): failed attempts charge a per-source budget that returns 429 with
/// `Retry-After` once exhausted. Every result is recorded in the audit
/// trail with a token fingerprint — never the token.
async fn api_key_auth(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let settings = crate::settings::Settings::load_async().await;
    let trusted = parse_trusted_proxies(&settings.api_trusted_proxies);
    let source = client_ip(&req, &trusted);

    // Lockout pre-check: once a source exhausts its failed-auth budget it
    // is refused outright (even with a valid credential) until the window
    // resets. The pre-check does not consume budget.
    if let Some(ip) = source {
        let limited = state.auth_limiter.lock().is_limited(&ip);
        if !limited.allowed {
            audit_event(
                &state,
                AuditEvent {
                    kind: "auth_rate_limited",
                    actor: bearer_token(&req).map(audit::token_fingerprint),
                    source: Some(ip.to_string()),
                    method: Some(req.method().to_string()),
                    path: Some(req.uri().path().to_string()),
                    outcome: "rate_limited",
                    detail: None,
                },
            );
            return too_many_requests_response(limited.retry_after_secs);
        }
    }

    let verify = |token: &str| -> bool {
        // Generated credential: constant-time verifier check.
        if let Some(credential) = settings.api_credential.as_ref() {
            return credential.verify(token);
        }
        // Legacy plaintext migration state.
        settings.authenticating_secret().is_some_and(|expected| {
            crate::settings::constant_time_eq(token.as_bytes(), expected.as_bytes())
        })
    };

    let decision = match bearer_token(&req) {
        Some(token) if verify(token) => {
            let fingerprint = audit::token_fingerprint(token);
            req.extensions_mut().insert(AuthenticatedIdentity {
                fingerprint: fingerprint.clone(),
            });
            Some(fingerprint)
        }
        _ => None,
    };

    match decision {
        Some(fingerprint) => {
            // Charge nothing extra on success, but record the event.
            audit_event(
                &state,
                AuditEvent {
                    kind: "auth_success",
                    actor: Some(fingerprint),
                    source: source.map(|ip| ip.to_string()),
                    method: Some(req.method().to_string()),
                    path: Some(req.uri().path().to_string()),
                    outcome: "ok",
                    detail: None,
                },
            );

            // Legacy plaintext: after a successful authentication, atomically
            // replace the plaintext with a verifier. If the migration cannot
            // commit, fail closed — the request is refused so a plaintext
            // credential cannot keep working indefinitely without ever being
            // upgraded.
            if settings.api_credential.is_none() {
                let migration = crate::settings::Settings::update_async(|persist| {
                    persist.apply_legacy_migration()
                })
                .await;
                if migration.is_err() {
                    tracing::error!(
                        "API credential migration could not be persisted; refusing request (fail closed)"
                    );
                    audit_event(
                        &state,
                        AuditEvent {
                            kind: "credential_migration_failed",
                            actor: None,
                            source: source.map(|ip| ip.to_string()),
                            method: Some(req.method().to_string()),
                            path: Some(req.uri().path().to_string()),
                            outcome: "error",
                            detail: None,
                        },
                    );
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"ok": false,
                        "error": "API credential migration failed; fix settings persistence and retry"})),
                    )
                        .into_response();
                }
            }
            next.run(req).await
        }
        None => {
            // Charge the failed-auth budget for this source.
            if let Some(ip) = source {
                let decision = state.auth_limiter.lock().check(ip);
                if !decision.allowed {
                    audit_event(
                        &state,
                        AuditEvent {
                            kind: "auth_rate_limited",
                            actor: bearer_token(&req).map(audit::token_fingerprint),
                            source: Some(ip.to_string()),
                            method: Some(req.method().to_string()),
                            path: Some(req.uri().path().to_string()),
                            outcome: "rate_limited",
                            detail: None,
                        },
                    );
                    return too_many_requests_response(decision.retry_after_secs);
                }
            }
            audit_event(
                &state,
                AuditEvent {
                    kind: "auth_failure",
                    actor: bearer_token(&req).map(audit::token_fingerprint),
                    source: source.map(|ip| ip.to_string()),
                    method: Some(req.method().to_string()),
                    path: Some(req.uri().path().to_string()),
                    outcome: "denied",
                    detail: None,
                },
            );
            unauthorized_response()
        }
    }
}

/// Record an audit event, failing open (the HTTP outcome is unchanged) with
/// a local warning when the audit write itself fails. Control mutations use
/// the fail-closed helper instead.
fn audit_event(state: &Arc<AppState>, event: AuditEvent) {
    if let Err(e) = state.audit.record(event) {
        tracing::warn!("Audit write failed: {e}");
    }
}

/// Fail-closed audit for control mutations: the command must not be queued
/// unless its audit record was durably written.
pub(crate) fn audit_control_event_or_block(
    state: &Arc<AppState>,
    event: AuditEvent,
) -> Option<Response> {
    state.audit.record(event).err().map(|e| {
        tracing::error!("Control mutation refused; audit write failed: {e}");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false,
            "error": "Audit logging unavailable; control request refused for safety"})),
        )
            .into_response()
    })
}

/// Separate integration router: snapshots and summary status are read-only;
/// four Quick Actions additionally require explicit write permission.
/// No settings or WebSocket endpoints are exposed.
///
/// `allowed_origins` is the exact-origin CORS allow-list (U2): an empty list
/// means no CORS headers at all, which is the safe default for
/// machine-to-machine integrations. Machine clients never send `Origin` and
/// are unaffected; only deliberate browser integrations need entries here.
pub fn create_authenticated_router_with_origins(
    state: Arc<AppState>,
    allowed_origins: &[String],
) -> Router {
    use axum::response::IntoResponse;

    async fn not_found_404() -> impl IntoResponse {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "Not found"})),
        )
    }

    // Starts require the explicit control-permission toggle (403 without).
    let starts = Router::new()
        .route(
            "/api/control/force-charge",
            post(external_control::force_charge),
        )
        .route(
            "/api/control/force-discharge",
            post(external_control::force_discharge),
        )
        .layer(DefaultBodyLimit::max(CONTROL_BODY_LIMIT_BYTES))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            external_control::require_control_permission,
        ));
    // Stops are NOT permission-gated at the route layer: the adapters allow
    // recovery of a HEM-owned external action even after the permission was
    // revoked (U5/R10), while refusing everything else exactly like starts.
    let stops = Router::new()
        .route(
            "/api/control/force-charge/stop",
            post(external_control::force_charge_stop),
        )
        .route(
            "/api/control/force-discharge/stop",
            post(external_control::force_discharge_stop),
        )
        .layer(DefaultBodyLimit::max(CONTROL_BODY_LIMIT_BYTES));

    let router = Router::new()
        .merge(starts)
        .merge(stops)
        // U4: the external route serves the least-data projection, not the
        // internal snapshot serializer used by the dashboard.
        // internal snapshot serializer used by the dashboard.
        .route(
            "/api/snapshot",
            get(external_snapshot::get_snapshot).layer(middleware::from_fn_with_state(
                state.clone(),
                limit_authenticated_reads,
            )),
        )
        .route(
            "/api/control/status",
            get(control_status::get_status).layer(middleware::from_fn_with_state(
                state.clone(),
                limit_authenticated_reads,
            )),
        )
        // U5: per-command status (accepted/queued/dispatched/confirmed/…).
        .route(
            "/api/commands/{command_id}",
            get(external_commands::command_status).layer(middleware::from_fn_with_state(
                state.clone(),
                limit_authenticated_reads,
            )),
        );

    let router = if allowed_origins.is_empty() {
        router
    } else {
        let origins: Vec<HeaderValue> = allowed_origins
            .iter()
            .filter_map(|origin| HeaderValue::from_str(origin).ok())
            .collect();
        router.layer(
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods([
                    axum::http::Method::GET,
                    axum::http::Method::POST,
                    axum::http::Method::OPTIONS,
                ])
                .allow_headers([
                    axum::http::header::AUTHORIZATION,
                    axum::http::header::CONTENT_TYPE,
                ]),
        )
    };

    router
        // Added last so authentication runs before permission/body validation.
        .route_layer(middleware::from_fn_with_state(state.clone(), api_key_auth))
        .layer(middleware::from_fn(request_timeout))
        .with_state(state)
        .route("/api/{*rest}", get(not_found_404))
}

/// Compatibility wrapper for existing callers and tests: no CORS origins.
pub fn create_authenticated_router(state: Arc<AppState>) -> Router {
    create_authenticated_router_with_origins(state, &[])
}

/// Serve the authenticated integration API until `shutdown` flips to `true`.
///
/// Reports bind success/failure through `bound_tx` before serving begins so
/// [`authenticated_lifecycle::AuthenticatedLifecycle`] can commit or roll
/// back its transaction. The make-service carries `ConnectInfo` so source
/// identity is available to middleware (U3 rate limits/audit).
pub async fn start_authenticated_server(
    state: Arc<AppState>,
    bind_ip: String,
    port: u16,
    allowed_origins: Vec<String>,
    mut shutdown: watch::Receiver<bool>,
    bound_tx: tokio::sync::oneshot::Sender<Result<(), String>>,
) {
    let app = create_authenticated_router_with_origins(state, &allowed_origins)
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    let addr = format!("{bind_ip}:{port}");
    tracing::info!("Authenticated API server starting on {}", addr);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            let message = format!("Failed to bind authenticated API server on {addr}: {e}");
            tracing::error!("{}", message);
            let _ = bound_tx.send(Err(message));
            return;
        }
    };
    tracing::info!("Authenticated API server bound on {}", addr);
    let _ = bound_tx.send(Ok(()));
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        let _ = shutdown.changed().await;
        tracing::info!("Authenticated API server shutting down");
    });
    if let Err(e) = server.await {
        tracing::error!("Authenticated API server error: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tower::ServiceExt;

    /// Minimal `dist/` tree on a unique temp path, removed when dropped.
    struct TempDist {
        path: PathBuf,
    }

    impl TempDist {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "hem-cache-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(path.join("assets")).unwrap();
            fs::write(path.join("index.html"), "<!doctype html>").unwrap();
            fs::write(
                path.join("assets").join("index-AbCd1234.js"),
                "console.log(1)",
            )
            .unwrap();
            fs::write(path.join("manifest.json"), "{}").unwrap();
            TempDist { path }
        }
    }

    impl Drop for TempDist {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// Build the router against a throwaway dist and return the response's
    /// `Cache-Control` header for the given request URI.
    async fn cache_control_for(uri: &str) -> Option<String> {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let dist = TempDist::new();
            let app =
                create_router_with_frontend(Arc::new(AppState::new()), dist.path.to_str().unwrap());
            let request = Request::builder()
                .uri(uri)
                .body(axum::body::Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            response
                .headers()
                .get(CACHE_CONTROL)
                .map(|v| v.to_str().unwrap().to_string())
        })
        .await
    }

    #[tokio::test]
    async fn hashed_assets_cache_immutably() {
        // Vite-hashed filename under /assets/ → immutable one-year cache.
        assert_eq!(
            cache_control_for("/assets/index-AbCd1234.js").await,
            Some("public, max-age=31536000, immutable".to_string())
        );
    }

    #[tokio::test]
    async fn index_html_must_revalidate() {
        // index.html drives which hashed assets load, so it must always
        // revalidate — otherwise a stale copy resurrects the old UI.
        assert_eq!(cache_control_for("/").await, Some("no-cache".to_string()));
        assert_eq!(
            cache_control_for("/index.html").await,
            Some("no-cache".to_string())
        );
    }

    #[tokio::test]
    async fn unhashed_root_files_must_revalidate() {
        // Non-hashed files (manifest, icons) change without a filename bump,
        // so they revalidate too. ServeDir answers 304 when unchanged.
        assert_eq!(
            cache_control_for("/manifest.json").await,
            Some("no-cache".to_string())
        );
    }

    // ======================================================================
    // Authenticated API server (external access with Bearer-token auth)
    // ======================================================================

    /// Seed the isolated config dir with a Settings that has the given
    /// api_key and port, then return the authenticated router.
    async fn make_authenticated_router_with_key(key: &str, port: u16) -> Router {
        let mut s = crate::settings::Settings::load();
        s.api_key = key.to_string();
        s.api_port = port;
        s.save().expect("settings save");
        create_authenticated_router(Arc::new(AppState::new()))
    }

    #[tokio::test]
    async fn authenticated_router_requires_bearer_token() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let app = make_authenticated_router_with_key("secret-xyz", 7338).await;

            // No Authorization header at all → 401.
            let request = Request::builder()
                .uri("/api/snapshot")
                .body(axum::body::Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let body: serde_json::Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(body["error"], "Unauthorized: invalid or missing API key");
        })
        .await;
    }

    #[tokio::test]
    async fn authenticated_router_rejects_wrong_bearer_token() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let app = make_authenticated_router_with_key("secret-xyz", 7338).await;

            // Wrong token → 401.
            let request = Request::builder()
                .uri("/api/snapshot")
                .header("Authorization", "Bearer wrong-token")
                .body(axum::body::Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        })
        .await;
    }

    #[tokio::test]
    async fn authenticated_router_accepts_valid_bearer_token() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let app = make_authenticated_router_with_key("secret-xyz", 7338).await;

            // Valid token → 200 (snapshot may be empty, but not 401).
            let request = Request::builder()
                .uri("/api/snapshot")
                .header("Authorization", "Bearer secret-xyz")
                .body(axum::body::Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body: serde_json::Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap(),
            )
            .unwrap();
            // No snapshot available yet, but the response is {ok: false, error: "..."}
            // rather than 401.
            assert_eq!(body["ok"], false);
        })
        .await;
    }

    /// A source that exhausts its failed-auth budget is locked out with
    /// 429 + Retry-After — even when presenting a valid credential.
    /// The authenticated snapshot must be the safe projection: documented
    /// operating fields present, internal identifiers/telemetry absent, and
    /// no-store caching — while the main router keeps serving the full
    /// snapshot.
    #[tokio::test]
    async fn authenticated_snapshot_serves_safe_projection_only() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            // Configure a credential for the authenticated router.
            let mut settings = crate::settings::Settings::load();
            settings.api_credential = Some(crate::settings::ApiCredential::from_secret(
                "projection-key",
            ));
            settings.save().unwrap();

            let state = Arc::new(AppState::new());
            *state.latest_snapshot.lock().await = Some(crate::inverter::model::InverterSnapshot {
                timestamp: chrono::Utc::now().timestamp() - 2,
                device_type: crate::inverter::model::DeviceType::Gen3Hybrid,
                soc: 71,
                solar_power: 1800,
                battery_modules: vec![crate::inverter::model::BatteryModule {
                    index: 1,
                    serial: "BG-SECRET-SERIAL".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            });

            // Authenticated router: safe projection.
            let request = Request::builder()
                .uri("/api/snapshot")
                .header("Authorization", "Bearer projection-key")
                .body(axum::body::Body::empty())
                .unwrap();
            let response = create_authenticated_router(state.clone())
                .oneshot(request)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["cache-control"], "no-store");
            let body: serde_json::Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(body["ok"], true);
            assert_eq!(body["soc"], 71);
            assert_eq!(body["solar_power"], 1800);
            assert!(body.get("battery_modules").is_none());
            assert!(
                !body.to_string().contains("BG-SECRET-SERIAL"),
                "module serials must never be exposed"
            );

            // Main router: unchanged full snapshot contract.
            let request = Request::builder()
                .uri("/api/snapshot")
                .body(axum::body::Body::empty())
                .unwrap();
            let response = create_router(state).oneshot(request).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(body["ok"], true);
            assert_eq!(
                body["data"]["battery_modules"][0]["serial"], "BG-SECRET-SERIAL",
                "the main router contract must remain the full snapshot"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn authenticated_router_locks_out_source_after_failed_auth_budget() {
        use axum::extract::connect_info::MockConnectInfo;
        crate::test_util::with_isolated_config_dir_async(|| async {
            let app = make_authenticated_router_with_key("secret-xyz", 7338)
                .await
                .layer(MockConnectInfo(
                    "203.0.113.9:40000".parse::<std::net::SocketAddr>().unwrap(),
                ));

            let attempt = |token: &'static str| {
                let app = app.clone();
                async move {
                    let request = Request::builder()
                        .uri("/api/snapshot")
                        .header("Authorization", format!("Bearer {token}"))
                        .body(axum::body::Body::empty())
                        .unwrap();
                    let response = app.oneshot(request).await.unwrap();
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .map(|v| v.to_string());
                    (response.status(), retry_after)
                }
            };

            // 10 failed attempts exhaust the default budget…
            for _ in 0..FAILED_AUTH_LIMIT {
                assert_eq!(attempt("wrong-token").await.0, StatusCode::UNAUTHORIZED);
            }
            // …then even a valid credential is refused with 429.
            let (status, retry_after) = attempt("secret-xyz").await;
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
            assert!(retry_after.is_some());
        })
        .await;
    }

    /// Forwarded identity is honoured only from explicitly trusted proxies;
    /// direct clients cannot spoof their source via X-Forwarded-For.
    #[test]
    fn client_ip_honours_forwarded_header_only_from_trusted_proxies() {
        let trusted: Vec<std::net::IpAddr> = vec!["10.0.0.2".parse().unwrap()];
        let build_request = |peer: &str, forwarded: Option<&str>| {
            let mut builder =
                Request::builder()
                    .uri("/api/snapshot")
                    .extension(axum::extract::ConnectInfo(
                        peer.parse::<std::net::SocketAddr>().unwrap(),
                    ));
            if let Some(forwarded) = forwarded {
                builder = builder.header("x-forwarded-for", forwarded);
            }
            builder.body(axum::body::Body::empty()).unwrap()
        };

        // Direct client: peer address wins; spoofed header ignored.
        let request = build_request("203.0.113.5:4444", Some("1.2.3.4"));
        assert_eq!(
            client_ip(&request, &trusted).unwrap().to_string(),
            "203.0.113.5"
        );

        // Trusted proxy peer: forwarded identity is used.
        let request = build_request("10.0.0.2:5555", Some("198.51.100.7, 10.0.0.2"));
        assert_eq!(
            client_ip(&request, &trusted).unwrap().to_string(),
            "198.51.100.7"
        );

        // Trusted proxy without a forwarded header falls back to the peer.
        let request = build_request("10.0.0.2:5555", None);
        assert_eq!(
            client_ip(&request, &trusted).unwrap().to_string(),
            "10.0.0.2"
        );
    }

    #[tokio::test]
    async fn authenticated_router_migrates_legacy_key_to_verifier_on_first_use() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let app = make_authenticated_router_with_key("legacy-key", 7338).await;

            // First valid request: authenticated via the legacy plaintext…
            let request = Request::builder()
                .uri("/api/snapshot")
                .header("Authorization", "Bearer legacy-key")
                .body(axum::body::Body::empty())
                .unwrap();
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                StatusCode::OK
            );

            // …and the plaintext has been replaced by a verifier.
            let settings = crate::settings::Settings::load();
            assert!(settings.api_key.is_empty(), "plaintext must be cleared");
            let credential = settings.api_credential.as_ref().expect("verifier created");
            assert!(credential.verify("legacy-key"));
            assert!(!credential.verify("wrong-key"));

            // A second request now authenticates via the verifier.
            let request = Request::builder()
                .uri("/api/snapshot")
                .header("Authorization", "Bearer legacy-key")
                .body(axum::body::Body::empty())
                .unwrap();
            assert_eq!(app.oneshot(request).await.unwrap().status(), StatusCode::OK);
        })
        .await;
    }

    #[tokio::test]
    async fn authenticated_router_fails_closed_when_legacy_migration_cannot_persist() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let app = make_authenticated_router_with_key("legacy-key", 7338).await;

            // Inject a settings-save failure for exactly one update.
            let _guard = crate::settings::InjectUpdateFailures::arm(1);

            let request = Request::builder()
                .uri("/api/snapshot")
                .header("Authorization", "Bearer legacy-key")
                .body(axum::body::Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

            // The plaintext was NOT migrated and stays authoritative —
            // nothing is half-committed.
            let settings = crate::settings::Settings::load();
            assert_eq!(settings.api_key, "legacy-key");
            assert!(settings.api_credential.is_none());

            // Once persistence works again the same request succeeds.
            let request = Request::builder()
                .uri("/api/snapshot")
                .header("Authorization", "Bearer legacy-key")
                .body(axum::body::Body::empty())
                .unwrap();
            assert_eq!(app.oneshot(request).await.unwrap().status(), StatusCode::OK);
            let settings = crate::settings::Settings::load();
            assert!(settings.api_credential.is_some());
        })
        .await;
    }

    /// A remote (non-loopback) tokenless caller must not be able to grant
    /// itself an API credential or flip external-control permission through
    /// the main settings route.
    #[tokio::test]
    async fn remote_settings_calls_cannot_change_api_security_fields() {
        use axum::extract::connect_info::MockConnectInfo;
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let remote: std::net::SocketAddr = "192.168.1.77:51000".parse().unwrap();
            let app = create_router(state).layer(MockConnectInfo(remote));

            for field in [
                serde_json::json!({"api_key_generate": true}),
                serde_json::json!({"api_key": "attacker-key"}),
                serde_json::json!({"api_control_enabled": true}),
                serde_json::json!({"api_port": 1234}),
            ] {
                let request = Request::builder()
                    .method("POST")
                    .uri("/api/settings")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(field.to_string()))
                    .unwrap();
                let response = app.clone().oneshot(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN, "{field}");
            }

            // Nothing persisted.
            let saved = crate::settings::Settings::load();
            assert!(!saved.has_api_auth());
            assert!(!saved.api_control_enabled);
        })
        .await;
    }

    /// Local callers keep full access to the security fields, and remote
    /// callers keep access to ordinary dashboard settings.
    #[tokio::test]
    async fn local_settings_calls_keep_api_security_access() {
        use axum::extract::connect_info::MockConnectInfo;
        crate::test_util::with_isolated_config_dir_async(|| async {
            let local: std::net::SocketAddr = "127.0.0.1:51001".parse().unwrap();

            // Loopback caller: generate works and returns the one-time secret.
            let app = create_router(Arc::new(AppState::new())).layer(MockConnectInfo(local));
            let request = Request::builder()
                .method("POST")
                .uri("/api/settings")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(r#"{"api_key_generate": true}"#))
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body: serde_json::Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(body["data"]["api_key"].as_str().map(str::len), Some(43));

            // Remote caller: ordinary settings still work.
            let remote: std::net::SocketAddr = "192.168.1.77:51002".parse().unwrap();
            let app = create_router(Arc::new(AppState::new())).layer(MockConnectInfo(remote));
            let request = Request::builder()
                .method("POST")
                .uri("/api/settings")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(r#"{"hidden_panels": ["battery"]}"#))
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        })
        .await;
    }

    #[tokio::test]
    async fn authenticated_router_rejects_non_snapshot_paths() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let app = make_authenticated_router_with_key("secret-xyz", 7338).await;

            // Even with a valid token, /api/settings is not exposed → 404.
            let request = Request::builder()
                .uri("/api/settings")
                .header("Authorization", "Bearer secret-xyz")
                .body(axum::body::Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);

            // /api/control/* paths also forbidden.
            let request = Request::builder()
                .uri("/api/control/mode")
                .header("Authorization", "Bearer secret-xyz")
                .body(axum::body::Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);

            // /ws (WebSocket) not exposed on the authenticated server.
            let request = Request::builder()
                .uri("/ws")
                .header("Authorization", "Bearer secret-xyz")
                .body(axum::body::Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        })
        .await;
    }

    #[tokio::test]
    async fn authenticated_router_is_get_only() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let app = make_authenticated_router_with_key("secret-xyz", 7338).await;

            // POST to /api/snapshot is not allowed (GET only).
            let request = Request::builder()
                .method("POST")
                .uri("/api/snapshot")
                .header("Authorization", "Bearer secret-xyz")
                .body(axum::body::Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        })
        .await;
    }

    #[tokio::test]
    async fn authenticated_router_no_key_configured_returns_401() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let app = make_authenticated_router_with_key("", 7338).await;
            let request = Request::builder()
                .uri("/api/snapshot")
                .body(axum::body::Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        })
        .await;
    }

    // ======================================================================
    // Single-port binding (no fall-forward)
    // ======================================================================

    /// Spawn `start_server_with_frontend_on_port` with a fresh state and a
    /// throwaway `dist_dir` path. Returns the channel receiver and the spawned
    /// task handle so the caller can assert on the bind outcome and abort the
    /// task either way. `dist_dir` is only consulted by `ServeDir` when
    /// serving actual HTTP requests, which these tests never make — the bind
    /// path itself does not touch the filesystem, so the path doesn't have
    /// to point at a real directory.
    fn spawn_single_port_bind(
        port: u16,
    ) -> (
        std::sync::mpsc::Receiver<Result<u16, String>>,
        tokio::task::JoinHandle<()>,
    ) {
        let state = crate::test_util::with_isolated_config_dir(|| Arc::new(AppState::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = tokio::spawn(async move {
            start_server_with_frontend_on_port(
                state,
                "127.0.0.1",
                port,
                String::from("/tmp/nonexistent-dist-for-test"),
                tx,
            )
            .await;
        });
        (rx, handle)
    }

    /// Reports the exact requested port on success — the predecessor function
    /// reported `Ok(preferred_port + offset)` after a fall-forward loop, so
    /// asserting equality here pins the removed behaviour.
    ///
    /// Uses the multi-threaded runtime flavour because the test body blocks
    /// on `std::sync::mpsc::recv_timeout` while the spawned server task is
    /// itself blocked on `send`; under current-thread (the default) that
    /// deadlocks. Production doesn't hit this because the receiver runs in
    /// `tauri::Builder.setup` (a sync thread), not a tokio runtime.
    #[tokio::test(flavor = "multi_thread")]
    async fn binds_only_the_specified_port_on_success() {
        // Grab a free ephemeral port via the OS, then release it so the
        // function can bind it. The reuse window between drop and bind is
        // tiny on localhost; if the OS ever does reuse the port, the
        // assertions below catch it.
        let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = holder.local_addr().unwrap().port();
        drop(holder);

        let (rx, handle) = spawn_single_port_bind(port);

        let result = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("bind should report within timeout");
        assert_eq!(
            result,
            Ok(port),
            "function must report Ok exactly the requested port (no fall-forward)"
        );

        handle.abort();
    }

    /// Key regression test: when the requested port is already in use, the
    /// function must report `Err` rather than silently retrying successive
    /// ports. The old behaviour would have sent `Ok(port + 1)` after a
    /// single `AddrInUse` failure.
    ///
    /// Multi-threaded flavour for the same reason as the success-path test:
    /// the blocking `recv_timeout` cannot share a single-threaded runtime
    /// with the spawned server task.
    #[tokio::test(flavor = "multi_thread")]
    async fn does_not_fall_forward_when_port_in_use() {
        // Hold the port for the entire test so the in-use state is
        // deterministic — the function cannot ever succeed here.
        let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = holder.local_addr().unwrap().port();

        let (rx, handle) = spawn_single_port_bind(port);

        let result = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("bind should report outcome within timeout");
        let err = result.expect_err(
            "the predecessor function would have sent Ok(port+1) here; the new \
             function must report Err so the desktop window surfaces an error \
             instead of silently starting on the next free port",
        );
        assert!(
            err.contains("already in use"),
            "user-facing error must explain the port is in use, got: {err}"
        );
        assert!(
            err.contains(&port.to_string()),
            "user-facing error must name the offending port, got: {err}"
        );

        drop(holder);
        handle.abort();
    }
}
