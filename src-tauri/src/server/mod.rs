//! Local HTTP/WebSocket server.
//!
//! Exposes inverter data and control endpoints via an Axum-based
//! HTTP API and a WebSocket real-time data stream.

pub mod api;
pub mod ws;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

use crate::inverter::poll::AppState;

pub fn create_router(state: Arc<AppState>) -> Router {
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
        .with_state(state)
}

/// Build the Axum router with API routes + frontend static file serving.
///
/// In production Tauri builds, the window navigates to `http://127.0.0.1:7337`
/// to avoid Windows WebView2 mixed-content blocking. The Axum server serves
/// the Vite dist files as a fallback for all non-API routes.
pub fn create_router_with_frontend(state: Arc<AppState>, dist_dir: &str) -> Router {
    let router = create_router(state);
    router.fallback_service(
        ServeDir::new(dist_dir).fallback(ServeDir::new(format!("{}/index.html", dist_dir))),
    )
}

/// Start the HTTP server on the specified bind address and port.
pub async fn start_server(state: Arc<AppState>, bind_addr: &str, port: u16) {
    let app = create_router(state);
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
pub async fn start_server_with_frontend(state: Arc<AppState>, bind_addr: &str, port: u16, dist_dir: &str) {
    let app = create_router_with_frontend(state, dist_dir);
    let addr = format!("{}:{}", bind_addr, port);
    tracing::info!("HTTP server starting on {} (serving frontend from {})", addr, dist_dir);
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
