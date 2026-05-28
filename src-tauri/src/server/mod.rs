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

use crate::inverter::poll::AppState;

/// Build the Axum router with all API routes, WebSocket endpoint, and CORS.
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

/// Start the HTTP server on the specified bind address and port.
pub async fn start_server(state: Arc<AppState>, bind_addr: &str, port: u16) {
    let app = create_router(state);
    let addr = format!("{}:{}", bind_addr, port);
    tracing::info!("HTTP server starting on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Failed to bind HTTP server");
    axum::serve(listener, app).await.expect("HTTP server error");
}
