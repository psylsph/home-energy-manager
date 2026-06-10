//! Local HTTP/WebSocket server.
//!
//! Exposes inverter data and control endpoints via an Axum-based
//! HTTP API and a WebSocket real-time data stream.

pub mod api;
pub mod logs;
pub mod ws;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::response::Json;
use axum::Router;
use axum::http::StatusCode;
use serde_json::json;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

use crate::inverter::poll::AppState;

pub fn create_router(state: Arc<AppState>) -> Router {
    use axum::response::IntoResponse;

    async fn not_found_404() -> impl IntoResponse {
        (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "Not found" })))
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
        .route(
            "/api/control/active-power-rate",
            post(api::set_active_power_rate),
        )
        .route("/api/control/pause", post(api::pause_battery))
        .route("/api/control/force-charge", post(api::force_charge))
        .route("/api/control/force-discharge", post(api::force_discharge))
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
        // Discovery
        .route("/api/discover", get(api::discover))
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

/// Build the Axum router with API routes + frontend static file serving.
///
/// In production Tauri builds, the window navigates to `http://127.0.0.1:7337`
/// so that API/WebSocket calls are same-origin (avoids WebView2 cross-origin
/// blocking). The bundled `dist/` resources serve the Vite output.
pub fn create_router_with_frontend(state: Arc<AppState>, dist_dir: &str) -> Router {
    let router = create_router(state);
    router.fallback_service(
        ServeDir::new(dist_dir).fallback(ServeDir::new(format!("{}/index.html", dist_dir))),
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
