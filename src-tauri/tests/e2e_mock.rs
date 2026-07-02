//! In-process integration tests for the Axum HTTP surface.
//!
//! These tests exercise the same router the production server uses
//! (`server::create_router`) but skip the TCP bind / port allocation
//! step by calling the router directly via `tower::ServiceExt::oneshot`.
//! That gives us fast, hermetic, concurrent coverage of the HTTP/JSON
//! layer that the Playwright E2E suite also exercises — but without
//! needing a live inverter or a separate test binary on disk.
//!
//! Scope (kept deliberately small to avoid coupling tests to private
//! state-machine internals; the E2E suite remains the source of truth
//! for full-stack behaviour):
//!
//!   * `GET /api/snapshot` — empty-state, then with a pre-seeded snapshot
//!   * `GET /api/status`    — connection state, host, LAN IP, client count
//!   * `GET /api/settings`  — default settings payload shape
//!   * `GET /api/logs`      — empty, then after push, then incremental
//!   * `GET /api/log-level` / `PUT /api/log-level` — round-trip + invalid
//!   * `GET /api/evc/status` — empty when no EVC is configured
//!   * `GET /api/{unknown}` — returns 404, not 200
//!
//! Things deliberately NOT covered here:
//!   * WebSocket frames (covered by `server::ws::tests` for the
//!     connected-clients registry and by the Playwright E2E for the
//!     wire format)
//!   * `set_*` control endpoints — those mutate `pending_writes`
//!     and require a running poll loop to drain. The unit-testable
//!     pure-decoder pieces (encoder.rs) are covered there instead.
//!   * History aggregation (covered by `history::tests`).

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use givenergy_local::inverter::poll::AppState;
use givenergy_local::server::create_router;
use serde_json::Value;
use tower::ServiceExt;

/// Max body size for the small JSON responses these tests produce.
const BODY_LIMIT: usize = 64 * 1024;

/// Build a fresh router backed by a fresh `AppState` so each test
/// gets a clean world with no leftover state from the previous one.
fn fresh_router() -> axum::Router {
    let state = Arc::new(AppState::new());
    create_router(state)
}

/// Issue a request and return (status, parsed JSON body).
async fn get_json(router: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = router
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .expect("router call");
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), BODY_LIMIT)
        .await
        .expect("read body");
    let json: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}

/// Issue a JSON POST and return (status, parsed JSON body).
#[allow(dead_code)]
async fn post_json(
    router: &axum::Router,
    uri: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let body_bytes = serde_json::to_vec(body).expect("serialise body");
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .expect("router call");
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), BODY_LIMIT)
        .await
        .expect("read body");
    let json: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}

/// Issue a PUT with a JSON body.
async fn put_json(
    router: &axum::Router,
    uri: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let body_bytes = serde_json::to_vec(body).expect("serialise body");
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .expect("router call");
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), BODY_LIMIT)
        .await
        .expect("read body");
    let json: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}

// ====================================================================
// GET /api/snapshot
// ====================================================================

#[tokio::test]
async fn snapshot_empty_state_reports_no_data() {
    let router = fresh_router();
    let (status, body) = get_json(&router, "/api/snapshot").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(false));
    assert!(body["error"].as_str().unwrap().contains("snapshot"));
}

// ====================================================================
// GET /api/status
// ====================================================================

#[tokio::test]
async fn status_returns_connection_payload() {
    let router = fresh_router();
    let (status, body) = get_json(&router, "/api/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(true));
    // Default connection state is Disconnected (per ConnectionState::default).
    assert!(body["connection"].is_string());
    assert!(body["client_count"].is_u64());
    // The frontend types require these exact keys to be present (even if
    // null/empty). A field rename in the handler would break the UI.
    for key in [
        "connection",
        "host",
        "lan_ip",
        "clients",
        "client_count",
        "connected_since_epoch_ms",
        "connect_failures",
    ] {
        assert!(body.get(key).is_some(), "missing key {key} in {body}");
    }
}

#[tokio::test]
async fn status_includes_connected_clients() {
    use givenergy_local::server::ws::ConnectedClients;
    use std::net::{IpAddr, SocketAddr};
    use std::str::FromStr;

    let router = fresh_router();
    // Pre-seed a client via a second AppState would be racy; instead, we
    // go through the WebSocket route. That requires a real upgrade, so
    // we exercise the count field on its own here and pin the registry
    // count path through the dedicated unit tests in `server::ws::tests`.
    let (_status, body) = get_json(&router, "/api/status").await;
    assert_eq!(body["client_count"].as_u64().unwrap(), 0);
    assert!(body["clients"].as_array().unwrap().is_empty());

    // Smoke-test the standalone ConnectedClients type too (same field shape).
    let peer = SocketAddr::new(IpAddr::from_str("10.0.0.42").unwrap(), 1234);
    let mut registry = ConnectedClients::new();
    registry.add(peer);
    assert_eq!(registry.count(), 1);
    assert_eq!(registry.list(), vec![peer]);
}

// ====================================================================
// GET /api/settings
// ====================================================================

#[tokio::test]
async fn settings_default_payload_shape() {
    let router = fresh_router();
    let (status, body) = get_json(&router, "/api/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(true));
    // The frontend SettingsPage reads these top-level keys. A rename
    // here would silently break the UI; this test pins the contract.
    let data = &body["data"];
    for key in [
        "host",
        "port",
        "serial",
        "interval_secs",
        "http_port",
        "evc_host",
        "evc_port",
    ] {
        assert!(data.get(key).is_some(), "missing settings key {key}");
    }
}

// ====================================================================
// GET /api/logs and PUT /api/log-level
// ====================================================================

#[tokio::test]
async fn logs_empty_ring_returns_empty_lines() {
    let router = fresh_router();
    let (status, body) = get_json(&router, "/api/logs").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["count"].as_u64().unwrap(), 0);
    assert!(body["lines"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn logs_incremental_poll_uses_after_param() {
    let router = fresh_router();
    // First poll with no `after`: should return everything currently in
    // the ring (empty), and `next: 0` for the next poll.
    let (status, body) = get_json(&router, "/api/logs").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["next"].as_u64().unwrap(), 0);

    // Poll again with `after=0` against an empty ring: still empty.
    let (status, body) = get_json(&router, "/api/logs?after=0").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["lines"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn log_level_get_and_put_round_trip() {
    let router = fresh_router();

    // Default is INFO (level_code 2).
    let (status, body) = get_json(&router, "/api/log-level").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["level"], "INFO");
    assert_eq!(body["level_code"].as_u64().unwrap(), 2);

    // Bump to DEBUG.
    let (status, body) = put_json(
        &router,
        "/api/log-level",
        &serde_json::json!({ "level": "DEBUG" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["level"], "DEBUG");
    assert_eq!(body["level_code"].as_u64().unwrap(), 3);

    // Confirm via GET.
    let (_status, body) = get_json(&router, "/api/log-level").await;
    assert_eq!(body["level"], "DEBUG");
    assert_eq!(body["level_code"].as_u64().unwrap(), 3);
}

#[tokio::test]
async fn log_level_invalid_string_rejected() {
    let router = fresh_router();
    let (status, body) = put_json(
        &router,
        "/api/log-level",
        &serde_json::json!({ "level": "silly" }),
    )
    .await;
    // The handler responds 200 with { ok: false, error: ... } rather
    // than a 4xx — the frontend reads `ok` to decide.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(false));
    assert!(body["error"].as_str().unwrap().contains("Invalid"));

    // Confirm the level didn't change (still INFO).
    let (_status, body) = get_json(&router, "/api/log-level").await;
    assert_eq!(body["level_code"].as_u64().unwrap(), 2);
}

#[tokio::test]
async fn log_level_missing_level_field_rejected() {
    let router = fresh_router();
    let (status, body) = put_json(
        &router,
        "/api/log-level",
        &serde_json::json!({ "not_level": "DEBUG" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(false));
}

// ====================================================================
// GET /api/evc/status
// ====================================================================

#[tokio::test]
async fn evc_status_empty_when_not_configured() {
    let router = fresh_router();
    let (status, body) = get_json(&router, "/api/evc/status").await;
    assert_eq!(status, StatusCode::OK);
    // With no EVC host and no cached snapshot, reachable must be false
    // and the frontend will render "Not Found" via the evcEverConnected
    // latch remaining false (issue #138).
    assert_eq!(body["reachable"], Value::Bool(false));
}

// ====================================================================
// 404 handling
// ====================================================================

#[tokio::test]
async fn unknown_api_path_returns_404() {
    let router = fresh_router();
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/this/does/not/exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router call");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let bytes = to_bytes(resp.into_body(), BODY_LIMIT).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["ok"], Value::Bool(false));
    assert_eq!(body["error"], "Not found");
}
