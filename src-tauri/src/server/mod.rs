//! Local HTTP/WebSocket server.
//!
//! Exposes inverter data and control endpoints via an Axum-based
//! HTTP API and a WebSocket real-time data stream.

pub mod api;
pub mod logs;
pub mod ws;

use std::sync::Arc;

use axum::extract::Request;
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Json, Response};
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
        .route(
            "/api/settings",
            get(api::get_settings).post(api::update_settings),
        )
        .route("/api/history", get(api::get_history))
        // Control endpoints
        .route("/api/control/mode", post(api::set_mode))
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
        .route("/api/control/pause", post(api::pause_battery))
        .route("/api/control/force-charge", post(api::force_charge))
        .route("/api/control/force-charge/stop", post(api::force_charge_stop))
        .route("/api/control/force-discharge", post(api::force_discharge))
        .route("/api/control/force-discharge/stop", post(api::force_discharge_stop))
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
        // Reconnect control
        .route("/api/reconnect", post(api::post_reconnect))
        // Discovery
        .route("/api/discover", get(api::discover))
        .route("/api/evc/discover", get(api::evc_discover))
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

/// Start the HTTP server with frontend static file serving, trying successive
/// ports if the preferred port is already in use.
///
/// Desktop Tauri windows navigate to the Axum origin for same-origin API and
/// WebSocket access. If an older app version is still listening on the preferred
/// port, blindly navigating there shows the old frontend. This helper reports
/// the actual bound port before serving so the window always attaches to the
/// newly-started process.
pub async fn start_server_with_frontend_on_available_port(
    state: Arc<AppState>,
    bind_addr: &str,
    preferred_port: u16,
    dist_dir: String,
    bound_tx: std::sync::mpsc::Sender<Result<u16, String>>,
) {
    const MAX_PORT_ATTEMPTS: u16 = 20;

    let mut last_error = None;
    for offset in 0..MAX_PORT_ATTEMPTS {
        let Some(port) = preferred_port.checked_add(offset) else {
            break;
        };
        let addr = format!("{}:{}", bind_addr, port);
        tracing::info!(
            "HTTP server attempting bind on {} (serving frontend from {})",
            addr,
            dist_dir
        );

        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(listener) => listener,
            Err(e) => {
                let message = format!("Failed to bind HTTP server on {addr}: {e}");
                if e.kind() == std::io::ErrorKind::AddrInUse {
                    tracing::warn!("{message}; trying next port");
                    last_error = Some(message);
                    continue;
                }

                tracing::error!("{message}");
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
        return;
    }

    let message = last_error.unwrap_or_else(|| {
        format!(
            "No available HTTP server port in range {}-{}",
            preferred_port,
            preferred_port.saturating_add(MAX_PORT_ATTEMPTS - 1)
        )
    });
    tracing::error!("{message}");
    let _ = bound_tx.send(Err(message));
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
}
