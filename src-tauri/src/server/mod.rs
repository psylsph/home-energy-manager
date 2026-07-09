//! Local HTTP/WebSocket server.
//!
//! Exposes inverter data and control endpoints via an Axum-based
//! HTTP API and a WebSocket real-time data stream.

pub mod api;
pub mod logs;
pub mod mini;
pub mod ws;

use std::sync::Arc;

use axum::extract::Request;
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;
use tower::ServiceBuilder;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

use crate::inverter::poll::AppState;

pub fn create_router(state: Arc<AppState>) -> Router {
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

    Router::new()
        // Data endpoints
        .route("/api/snapshot", get(api::get_snapshot))
        .route("/api/status", get(api::get_status))
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
            get(api::get_settings).post(api::update_settings),
        )
        .route("/api/history", get(api::get_history))
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
        // Auto winter mode
        .route(
            "/api/auto-winter",
            get(api::get_auto_winter).post(api::set_auto_winter),
        )
        // Cosy charging
        .route("/api/cosy", get(api::get_cosy).post(api::set_cosy))
        // Agile Octopus
        .route("/api/agile", get(api::get_agile).post(api::set_agile))
        // Load discharge limiter
        .route(
            "/api/load-limiter",
            get(api::get_load_limiter).post(api::set_load_limiter),
        )
        // Email alerts
        .route("/api/alerts", get(api::get_alerts).post(api::set_alerts))
        .route("/api/alerts/test", post(api::test_alerts))
        // Weather (Open-Meteo integration)
        .route("/api/weather", get(api::get_weather).post(api::set_weather))
        .route("/api/weather/backfill", post(api::backfill_weather))
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
// Read-only API server (external access with API key auth)
// ---------------------------------------------------------------------------

/// API key authentication middleware.
///
/// Checks for a `Bearer <key>` token in the `Authorization` header.
/// Returns 401 Unauthorized if the key is missing or doesn't match.
async fn api_key_auth(req: Request, next: Next) -> Response {
    let expected_key = crate::settings::Settings::load().api_key;

    if expected_key.is_empty() {
        // No key configured — deny all requests (shouldn't happen since
        // the server isn't started without a key, but defend anyway).
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"ok": false, "error": "API key not configured"})),
        )
            .into_response();
    }

    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if let Some(token) = auth_header.strip_prefix("Bearer ") {
        if token == expected_key {
            return next.run(req).await;
        }
    }

    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"ok": false, "error": "Unauthorized: invalid or missing API key"})),
    )
        .into_response()
}

/// Create a minimal read-only router with API key authentication.
///
/// Serves only `GET /api/snapshot` — no control endpoints, no settings,
/// no WebSocket. All requests require a valid `Authorization: Bearer <key>`
/// header matching the configured `api_key`.
pub fn create_readonly_router(state: Arc<AppState>) -> Router {
    use axum::response::IntoResponse;

    async fn not_found_404() -> impl IntoResponse {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "Not found"})),
        )
    }

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/api/snapshot", get(api::get_snapshot))
        .route_layer(middleware::from_fn_with_state(state.clone(), api_key_auth))
        .layer(cors)
        .with_state(state)
        .route("/api/{*rest}", get(not_found_404))
}

/// Start the read-only API server on a separate port.
///
/// Only serves `GET /api/snapshot` with Bearer-token authentication.
/// The main server on `http_port` is unaffected.
pub async fn start_readonly_server(state: Arc<AppState>, bind_addr: &str, port: u16) {
    let app = create_readonly_router(state).into_make_service();
    let addr = format!("{}:{}", bind_addr, port);
    tracing::info!("Read-only API server starting on {}", addr);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind read-only API server on {}: {e}", addr);
            return;
        }
    };
    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!("Read-only API server error: {e}");
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
    // Read-only API server (external access with Bearer-token auth)
    // ======================================================================

    /// Seed the isolated config dir with a Settings that has the given
    /// api_key and port, then return the read-only router.
    async fn make_readonly_router_with_key(key: &str, port: u16) -> Router {
        let mut s = crate::settings::Settings::load();
        s.api_key = key.to_string();
        s.api_port = port;
        s.save().expect("settings save");
        create_readonly_router(Arc::new(AppState::new()))
    }

    #[tokio::test]
    async fn readonly_router_requires_bearer_token() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let app = make_readonly_router_with_key("secret-xyz", 7338).await;

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
    async fn readonly_router_rejects_wrong_bearer_token() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let app = make_readonly_router_with_key("secret-xyz", 7338).await;

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
    async fn readonly_router_accepts_valid_bearer_token() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let app = make_readonly_router_with_key("secret-xyz", 7338).await;

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

    #[tokio::test]
    async fn readonly_router_rejects_non_snapshot_paths() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let app = make_readonly_router_with_key("secret-xyz", 7338).await;

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

            // /ws (WebSocket) not exposed on the read-only server.
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
    async fn readonly_router_is_get_only() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let app = make_readonly_router_with_key("secret-xyz", 7338).await;

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
    async fn readonly_router_no_key_configured_returns_401() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let app = make_readonly_router_with_key("", 7338).await;
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
        let state = Arc::new(AppState::new());
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
