//! Local HTTP/WebSocket server.
//!
//! Exposes inverter data and control endpoints via an Axum-based
//! HTTP API and a WebSocket real-time data stream.

pub mod api;
pub mod ws;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::Any;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

use crate::inverter::poll::AppState;

/// Build the Axum router with all API routes, WebSocket endpoint, CORS,
/// and frontend static file serving.
///
/// In production (Tauri), the frontend is served from the Axum server so
/// everything is same-origin — no mixed-content issues on Windows.
pub fn create_router(state: Arc<AppState>, frontend_dir: Option<&str>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let mut router = Router::new()
        // Data endpoints
        .route("/api/snapshot", get(api::get_snapshot))
        .route("/api/status", get(api::get_status))
        .route(
            "/api/settings",
            get(api::get_settings).post(api::update_settings),
        )
        // Control endpoints
        .route("/api/control/mode", post(api::set_mode))
        .route("/api/control/charge-slot", post(api::set_charge_slot))
        .route("/api/control/discharge-slot", post(api::set_discharge_slot))
        .route("/api/control/reserve", post(api::set_reserve))
        .route("/api/control/charge-rate", post(api::set_charge_rate))
        .route("/api/control/discharge-rate", post(api::set_discharge_rate))
        .route("/api/control/pause", post(api::pause_battery))
        // Discovery
        .route("/api/discover", get(api::discover))
        // WebSocket real-time stream
        .route("/ws", get(ws::ws_handler))
        .layer(cors)
        .with_state(state);

    // Serve frontend static files (production only)
    if let Some(dir) = frontend_dir {
        router = router.fallback_service(
            ServeDir::new(dir).fallback(ServeDir::new(format!("{}/index.html", dir))),
        );
    }

    router
}

/// Start the HTTP server.
///
/// If `frontend_dir` is `Some`, serves the Vite dist files as a fallback
/// so the Tauri window can load from `http://127.0.0.1:7337/` (same-origin).
pub async fn start_server(state: Arc<AppState>, bind_addr: &str, port: u16, frontend_dir: Option<&str>) {
    let app = create_router(state, frontend_dir);
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
