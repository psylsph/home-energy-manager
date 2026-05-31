//! Local HTTP/WebSocket server.
//!
//! Exposes inverter data and control endpoints via an Axum-based
//! HTTP API and a WebSocket real-time data stream.

pub mod api;
pub mod logs;
pub mod ws;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};

use crate::inverter::poll::AppState;

pub fn create_router(state: Arc<AppState>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/api/snapshot", get(api::get_snapshot))
        .route("/api/status", get(api::get_status))
        .route("/api/settings", get(api::get_settings).post(api::update_settings))
        .route("/api/history", get(api::get_history))
        .route("/api/control/mode", post(api::set_mode))
        .route("/api/control/charge-slot", post(api::set_charge_slot))
        .route("/api/control/discharge-slot", post(api::set_discharge_slot))
        .route("/api/control/reserve", post(api::set_reserve))
        .route("/api/control/charge-rate", post(api::set_charge_rate))
        .route("/api/control/discharge-rate", post(api::set_discharge_rate))
        .route("/api/control/pause", post(api::pause_battery))
        .route("/api/control/force-charge", post(api::force_charge))
        .route("/api/control/force-discharge", post(api::force_discharge))
        .route("/api/control/sync-clock", post(api::sync_clock))
        .route("/api/auto-winter", get(api::get_auto_winter).post(api::set_auto_winter))
        .route("/api/cosy", get(api::get_cosy).post(api::set_cosy))
        .route("/api/discover", get(api::discover))
        .route("/api/logs", get(logs::get_logs))
        .route("/ws", get(ws::ws_handler))
        .layer(cors)
        .with_state(state)
}

/// Build the Axum router with API routes + frontend static file serving.
pub fn create_router_with_frontend(state: Arc<AppState>, dist_dir: &str) -> Router {
    let router = create_router(state);
    let index = format!("{}/index.html", dist_dir);
    router.fallback_service(
        tower_http::services::ServeDir::new(dist_dir)
            .fallback(tower_http::services::ServeFile::new(index)),
    )
}

/// Start the HTTP server (API + WebSocket only, no frontend serving).
pub async fn start_server(state: Arc<AppState>, bind_addr: &str, port: u16) {
    let app = create_router(state);
    let addr = format!("{}:{}", bind_addr, port);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap_or_else(|e| {
        tracing::error!("Failed to bind to {addr}: {e}");
        std::process::exit(1);
    });
    tracing::info!("HTTP server starting on {} (API only)", addr);
    axum::serve(listener, app).await.unwrap_or_else(|e| {
        tracing::error!("Server error: {e}");
    });
}

/// Start the HTTP server with frontend static file serving.
pub async fn start_server_with_frontend(
    state: Arc<AppState>,
    bind_addr: &str,
    port: u16,
    dist_dir: String,
) {
    let app = create_router_with_frontend(state, &dist_dir);
    let addr = format!("{}:{}", bind_addr, port);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap_or_else(|e| {
        tracing::error!("Failed to bind to {addr}: {e}");
        std::process::exit(1);
    });
    tracing::info!("HTTP server starting on {} (serving frontend from {})", addr, dist_dir);
    axum::serve(listener, app).await.unwrap_or_else(|e| {
        tracing::error!("Server error: {e}");
    });
}
