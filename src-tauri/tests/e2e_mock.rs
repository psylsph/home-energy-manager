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
//!   * `GET /api/history/summary` — route wiring and missing-DB response
//!   * `GET /api/charging-mode` / `/api/adaptive-charge` — automation defaults
//!   * `GET/POST /api/temperature-limiter` — defaults, update, and validation
//!   * `GET /api/mini/status` — tokenless glance summary (empty + seeded)
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

use std::sync::{Arc, OnceLock};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use givenergy_local::inverter::poll::AppState;
use givenergy_local::server::create_router;
use serde_json::{json, Value};
use tower::ServiceExt;

/// Max body size for the small JSON responses these tests produce.
const BODY_LIMIT: usize = 64 * 1024;

/// Serialise integration tests that alter the process-global config override.
fn config_dir_mutex() -> &'static parking_lot::Mutex<()> {
    static MUTEX: OnceLock<parking_lot::Mutex<()>> = OnceLock::new();
    MUTEX.get_or_init(|| parking_lot::Mutex::new(()))
}

struct IsolatedConfig {
    _lock: parking_lot::MutexGuard<'static, ()>,
    dir: std::path::PathBuf,
    previous: Option<std::ffi::OsString>,
}

impl IsolatedConfig {
    fn enter() -> Self {
        let lock = config_dir_mutex().lock();
        let dir = std::env::temp_dir().join(format!(
            "givenergy-local-e2e-mock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time after Unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create isolated integration-test config dir");
        let previous = std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR");
        std::env::set_var("GIVENERGY_LOCAL_CONFIG_DIR", &dir);
        Self {
            _lock: lock,
            dir,
            previous,
        }
    }
}

impl Drop for IsolatedConfig {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            std::env::set_var("GIVENERGY_LOCAL_CONFIG_DIR", previous);
        } else {
            std::env::remove_var("GIVENERGY_LOCAL_CONFIG_DIR");
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct IsolatedRouter {
    router: axum::Router,
    _config: IsolatedConfig,
}

impl std::ops::Deref for IsolatedRouter {
    type Target = axum::Router;

    fn deref(&self) -> &Self::Target {
        &self.router
    }
}

/// Build a fresh router backed by a fresh `AppState` and a unique temporary
/// config directory that remains active for the router's lifetime.
fn fresh_router() -> IsolatedRouter {
    let config = IsolatedConfig::enter();
    let state = Arc::new(AppState::new());
    IsolatedRouter {
        router: create_router(state),
        _config: config,
    }
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
async fn post_json(router: &axum::Router, uri: &str, body: &Value) -> (StatusCode, Value) {
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
async fn put_json(router: &axum::Router, uri: &str, body: &Value) -> (StatusCode, Value) {
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
// GET /api/latest-version ("new version available" cache)
// ====================================================================
// The endpoint now triggers a background refresh when the cache is stale,
// but only when the update loop has been registered (`loop_registered`).
// Tests don't spawn the loop, so the cache stays empty and no network
// call is made — keeping these tests hermetic. This pins the route wiring
// + payload shape + the current-version field.

#[tokio::test]
async fn latest_version_empty_cache_reports_current_and_no_update() {
    let router = fresh_router();
    let (status, body) = get_json(&router, "/api/latest-version").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(true));
    // current_version always reflects the compile-time package version.
    assert_eq!(body["current_version"], json!(env!("CARGO_PKG_VERSION")));
    // Empty cache → no latest known yet, never claims an update.
    assert_eq!(body["update_available"], Value::Bool(false));
    assert!(body.get("latest_version").is_none_or(|v| v.is_null()));
    assert!(body.get("release_url").is_none_or(|v| v.is_null()));
}

#[tokio::test]
async fn latest_version_disabled_when_check_for_updates_off() {
    // Write a settings.json with check_for_updates = false into the
    // isolated config dir so the endpoint reports the opt-out.
    let config = IsolatedConfig::enter();
    {
        let s = givenergy_local::settings::Settings {
            check_for_updates: false,
            ..Default::default()
        };
        s.save().expect("save settings");
    }
    let state = Arc::new(AppState::new());
    let router = create_router(state);
    let (status, body) = get_json(&router, "/api/latest-version").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["disabled"], Value::Bool(true));
    assert_eq!(body["current_version"], json!(env!("CARGO_PKG_VERSION")));
    drop(config);
}

// ====================================================================
// GET /api/mini/status
// ====================================================================

#[tokio::test]
async fn mini_status_empty_state_returns_defaults() {
    let router = fresh_router();
    let (status, body) = get_json(&router, "/api/mini/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(false));
    assert_eq!(body["conn"], Value::String("disconnected".into()));
    assert_eq!(body["device"], Value::String("".into()));
    assert_eq!(body["soc"], serde_json::json!(0));
    assert_eq!(body["fault"], Value::Bool(false));
    // Every field is present even with no snapshot (no Option leaks for the
    // Shortcut to nil-check).
    for key in [
        "ok",
        "ts",
        "age_s",
        "conn",
        "device",
        "solar_kw",
        "battery_kw",
        "grid_kw",
        "home_kw",
        "soc",
        "battery_state",
        "battery_mode",
        "fault",
    ] {
        assert!(body.get(key).is_some(), "mini missing key {key}");
    }
}

#[tokio::test]
async fn mini_status_seeded_snapshot_round_trips_and_is_no_store() {
    use givenergy_local::inverter::model::{BatteryMode, BatteryState, InverterSnapshot};

    let _config = IsolatedConfig::enter();
    let state = Arc::new(AppState::new());
    *state.latest_snapshot.lock().await = Some(InverterSnapshot {
        timestamp: 1_700_000_000,
        solar_power: 4213,
        battery_power: -1798,
        grid_power: 930,
        home_power: 1485,
        soc: 64,
        battery_state: BatteryState::Discharging,
        battery_mode: BatteryMode::Eco,
        grid_loss: false,
        inverter_trip: false,
        battery_over_temp: false,
        device_type_display: String::from("Gen3 Hybrid"),
        ..Default::default()
    });
    let router = create_router(state);

    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/mini/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router call");
    assert_eq!(resp.status(), StatusCode::OK);
    // Fresh-data directive is set so a Shortcut tap never sees a cached body.
    assert_eq!(resp.headers().get("cache-control").unwrap(), "no-store");

    let bytes = to_bytes(resp.into_body(), BODY_LIMIT).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["device"], Value::String("Gen3 Hybrid".into()));
    assert_eq!(body["soc"], serde_json::json!(64));
    assert_eq!(body["battery_state"], Value::String("discharging".into()));
    assert_eq!(body["battery_mode"], Value::String("eco".into()));
    assert!((body["solar_kw"].as_f64().unwrap() - 4.2).abs() < 1e-6);
    assert!((body["battery_kw"].as_f64().unwrap() - (-1.8)).abs() < 1e-6);
    assert!((body["grid_kw"].as_f64().unwrap() - 0.9).abs() < 1e-6);
    assert!((body["home_kw"].as_f64().unwrap() - 1.5).abs() < 1e-6);
}

// ====================================================================
// GET /mini (tiny GUI page)
// ====================================================================

#[tokio::test]
async fn mini_page_serves_self_contained_html() {
    let router = fresh_router();
    let resp = router
        .clone()
        .oneshot(Request::builder().uri("/mini").body(Body::empty()).unwrap())
        .await
        .expect("router call");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/html; charset=utf-8"
    );
    let bytes = to_bytes(resp.into_body(), BODY_LIMIT).await.unwrap();
    let html = std::str::from_utf8(&bytes).unwrap();
    // The page must fetch the JSON data source (same origin).
    assert!(
        html.contains("/api/mini/status"),
        "page must reference the JSON endpoint"
    );
    // And render the four KPIs.
    for label in ["Solar", "Home", "Battery", "Grid"] {
        assert!(html.contains(label), "page must label {label}");
    }
    // Self-contained: inline CSS + JS, no external assets to fetch.
    assert!(html.contains("<style>") && html.contains("<script>"));
    // Under the 16KB budget a tiny watch WebView prefers.
    assert!(
        bytes.len() < 16 * 1024,
        "mini page too large: {} bytes",
        bytes.len()
    );
}

// ====================================================================
// Automation defaults and 404 handling
// ====================================================================

#[tokio::test]
async fn adaptive_charge_endpoints_return_safe_defaults() {
    let router = fresh_router();

    let (status, mode) = get_json(&router, "/api/charging-mode").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(mode["ok"], true);
    assert_eq!(mode["mode"], "standard");
    assert_eq!(mode["adaptive_state"], "inactive");

    let (status, adaptive) = get_json(&router, "/api/adaptive-charge").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(adaptive["ok"], true);
    assert_eq!(adaptive["data"]["enabled"], false);
    assert_eq!(adaptive["data"]["config"]["confirmation_readings"], 2);
    assert_eq!(
        adaptive["data"]["config"]["periods"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
}

#[tokio::test]
async fn temperature_limiter_round_trip_and_validation() {
    let router = fresh_router();

    let (status, initial) = get_json(&router, "/api/temperature-limiter").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(initial["data"]["config"]["enabled"], false);
    assert_eq!(initial["data"]["config"]["high_threshold"], 60.0);
    assert_eq!(initial["data"]["config"]["recovery_threshold"], 55.0);

    let (status, _) = post_json(
        &router,
        "/api/temperature-limiter",
        &serde_json::json!({
            "enabled": true,
            "high_threshold": 64.0,
            "recovery_threshold": 56.0,
            "confirmation_readings": 4
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, updated) = get_json(&router, "/api/temperature-limiter").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["data"]["config"]["enabled"], true);
    assert_eq!(updated["data"]["config"]["high_threshold"], 64.0);
    assert_eq!(updated["data"]["config"]["confirmation_readings"], 4);

    let (status, invalid) = post_json(
        &router,
        "/api/temperature-limiter",
        &serde_json::json!({
            "high_threshold": 55.0,
            "recovery_threshold": 55.0
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(invalid["error"]
        .as_str()
        .is_some_and(|message| { message.contains("Recovery threshold must be below") }));
}

#[tokio::test]
async fn history_summary_route_is_wired() {
    let router = fresh_router();
    let (status, body) = get_json(
        &router,
        "/api/history/summary?range=1h&rolling=true&start_ms=1000&end_ms=2000",
    )
    .await;
    // Fresh AppState intentionally has no HistoryDb installed. A 500 from the
    // handler proves this is the real route rather than the catch-all 404.
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"], "History database not available");
}

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

// ====================================================================
// POST /api/settings — immediate re-stamp of kWp-derived snapshot fields
// ====================================================================

/// Issue #282 follow-up / local-E2E "solar arrays never appear": the poll
/// loop stamps `solar_arrays` / `pv1_pct` / `pv2_pct` from settings on every
/// cycle, but a control-page write storm can monopolise the loop for minutes
/// (each queued register write costs a 1.5 s inter-write gap), so a
/// `pv1_rated_kw` save made right after mode changes was invisible in the
/// snapshot long past any UI patience. Saving settings must re-stamp those
/// fields on the CURRENT snapshot immediately — no poll cycle required.
#[tokio::test]
async fn settings_save_restamps_solar_array_fields_without_poll_cycle() {
    use givenergy_local::inverter::model::{BatteryMode, BatteryState, InverterSnapshot};

    let _config = IsolatedConfig::enter();
    let state = Arc::new(AppState::new());
    *state.latest_snapshot.lock().await = Some(InverterSnapshot {
        timestamp: 1_700_000_000,
        solar_power: 3_321,
        pv1_power: 3_321,
        pv2_power: 0,
        battery_power: 0,
        grid_power: -2_700,
        home_power: 600,
        soc: 64,
        battery_state: BatteryState::Idle,
        battery_mode: BatteryMode::Eco,
        grid_loss: false,
        inverter_trip: false,
        battery_over_temp: false,
        device_type_display: String::from("Gen3 Hybrid"),
        ..Default::default()
    });
    let router = create_router(state);

    // Before: no rated kWp configured → no pv percentages, no array summary.
    let (status, body) = get_json(&router, "/api/snapshot").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["data"]["pv1_pct"].is_null());
    assert_eq!(
        body["data"]["solar_arrays"].as_array().map(Vec::len),
        Some(0)
    );

    // Save the rated kWp like the Solar page settings UI does.
    let (status, body) = post_json(
        &router,
        "/api/settings",
        &json!({ "pv1_rated_kw": 5.0, "pv2_rated_kw": 3.0 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "settings save failed: {body}");
    assert_eq!(body["ok"], Value::Bool(true));

    // The SAME snapshot must now expose the stamped fields — without any
    // poll cycle having run (no inverter is connected in this harness).
    let (status, body) = get_json(&router, "/api/snapshot").await;
    assert_eq!(status, StatusCode::OK);
    let pv1_pct = body["data"]["pv1_pct"].as_f64().expect("pv1_pct stamped");
    assert!(
        (pv1_pct - 66.42).abs() < 0.01,
        "pv1_pct = {pv1_pct}, want ~66.42 (3321 W / 5000 W)"
    );
    let arrays = body["data"]["solar_arrays"].as_array().expect("arrays");
    assert_eq!(arrays.len(), 2, "PV1 + PV2 entries: {arrays:?}");
    assert_eq!(arrays[0]["rated_kw"], serde_json::json!(5.0));
    assert_eq!(arrays[1]["rated_kw"], serde_json::json!(3.0));
}

/// Clearing the rated kWp must equally take effect immediately — the Solar
/// page hides the Solar Arrays card when no array is configured, so a stale
/// non-empty `solar_arrays` would keep the card on screen for minutes.
#[tokio::test]
async fn settings_save_clearing_kwp_restamps_snapshot_immediately() {
    use givenergy_local::inverter::model::{BatteryMode, BatteryState, InverterSnapshot};

    let _config = IsolatedConfig::enter();
    let state = Arc::new(AppState::new());
    // Configure the rated kWp first (persisted on disk), then seed the
    // snapshot as the poll loop would have stamped it.
    {
        let router = create_router(state.clone());
        let (status, body) = post_json(
            &router,
            "/api/settings",
            &json!({ "pv1_rated_kw": 5.0, "pv2_rated_kw": 3.0 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "settings save failed: {body}");
    }
    *state.latest_snapshot.lock().await = Some(InverterSnapshot {
        timestamp: 1_700_000_000,
        solar_power: 3_321,
        pv1_power: 3_321,
        pv2_power: 0,
        soc: 64,
        battery_state: BatteryState::Idle,
        battery_mode: BatteryMode::Eco,
        pv1_pct: Some(66.42),
        pv2_pct: Some(0.0),
        ..Default::default()
    });
    let router = create_router(state);

    let (status, body) = post_json(
        &router,
        "/api/settings",
        &json!({ "pv1_rated_kw": 0.0, "pv2_rated_kw": 0.0 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "settings save failed: {body}");

    let (status, body) = get_json(&router, "/api/snapshot").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["data"]["pv1_pct"].is_null(),
        "pv1_pct must clear immediately: {}",
        body["data"]["pv1_pct"]
    );
    assert!(
        body["data"]["pv2_pct"].is_null(),
        "pv2_pct must clear immediately: {}",
        body["data"]["pv2_pct"]
    );
    assert_eq!(
        body["data"]["solar_arrays"].as_array().map(Vec::len),
        Some(0),
        "solar_arrays must clear immediately"
    );
}

// ====================================================================
// GET /api/forecast — issue #283 Phase 1
// ====================================================================

/// A fresh install (no weather, no history, no snapshot) must return a
/// well-formed payload with degradation codes — never zeros pretending
/// to be a prediction, and never a 500.
#[tokio::test]
async fn forecast_endpoint_degrades_cleanly_with_empty_state() {
    let _config = IsolatedConfig::enter();
    let state = Arc::new(AppState::new());
    let router = create_router(state);

    let (status, body) = get_json(&router, "/api/forecast").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(true));
    let codes = body["data"]["status"].as_array().expect("status array");
    for code in ["weather_disabled", "no_forecast_data", "no_snapshot"] {
        assert!(
            codes.iter().any(|c| c == code),
            "expected status code {code} in {codes:?}"
        );
    }
    assert!(body["data"]["battery"].is_null());
    assert_eq!(body["data"]["solar"].as_array().map(Vec::len), Some(0));
}

/// Fully seeded state returns numbers: forward radiation, calibrated PR,
/// consumption profile and a battery projection wired through settings.
#[tokio::test]
async fn forecast_endpoint_serves_seeded_forecast() {
    use chrono::TimeZone;
    use givenergy_local::forecast::ForecastSolarHour;
    use givenergy_local::history::{ForecastValueRow, HistoryDb};
    use givenergy_local::inverter::model::InverterSnapshot;

    let _ = std::mem::size_of::<ForecastSolarHour>(); // shape compiles
    let config = IsolatedConfig::enter();
    let state = Arc::new(AppState::new());

    let db = HistoryDb::open(&config.dir.join("history.db")).unwrap();
    let now = chrono::Local::now();
    let now_ts = now.timestamp();
    let hour_start = now_ts - now_ts.rem_euclid(3600);
    for h in 0..72i64 {
        db.insert_forecast_values(&[ForecastValueRow {
            timestamp: hour_start + h * 3600,
            variable: "shortwave_radiation".to_string(),
            value: 500.0,
            source: "open-meteo".to_string(),
            fetched_at: now_ts,
        }])
        .unwrap();
    }
    db.set_meta_value("forecast_pr", "0.8").unwrap();
    db.set_meta_value("forecast_pr_days", "12").unwrap();
    db.set_meta_value("forecast_calibrated_at", &now_ts.to_string())
        .unwrap();

    // Seven days of hourly consumption counters (+0.5 kWh/h).
    let mut date = now.date_naive() - chrono::Duration::days(7);
    while date < now.date_naive() {
        let mut counter = 0.0_f64;
        for hour in 0..24u32 {
            let ts = chrono::Local
                .from_local_datetime(&date.and_hms_opt(hour, 0, 0).unwrap())
                .earliest()
                .unwrap()
                .timestamp();
            db.insert_reading(&InverterSnapshot {
                timestamp: ts,
                home_energy_today_kwh: counter as f32,
                ..Default::default()
            });
            counter += 0.5;
        }
        date = date.succ_opt().unwrap();
    }
    *state.history.lock().await = Some(Arc::new(db));

    // Weather enabled with coordinates so the forecast isn't gated off.
    {
        let mut ws = state.weather.lock().await;
        ws.config.enabled = true;
        ws.config.latitude = Some(51.5);
        ws.config.longitude = Some(-0.13);
    }

    *state.latest_snapshot.lock().await = Some(InverterSnapshot {
        soc: 50,
        battery_capacity_kwh: 10.0,
        max_battery_power_w: 5000,
        charge_rate: 50,
        discharge_rate: 50,
        battery_reserve: 10,
        ..Default::default()
    });

    let router = create_router(state.clone());
    let (status, body) = post_json(
        &router,
        "/api/settings",
        &json!({ "pv1_rated_kw": 5.0 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "settings save failed: {body}");

    let (status, body) = get_json(&router, "/api/forecast").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(true));
    let status_codes = body["data"]["status"].as_array().expect("status");
    assert!(
        status_codes.is_empty(),
        "expected no degradation codes: {status_codes:?}"
    );
    let tomorrow = body["data"]["solar_tomorrow_kwh"].as_f64().unwrap();
    assert!(
        (tomorrow - 48.0).abs() < 0.5,
        "solar_tomorrow_kwh = {tomorrow}, want ~48"
    );
    assert_eq!(body["data"]["performance_ratio"], serde_json::json!(0.8));
    assert_eq!(body["data"]["consumption_sufficient"], Value::Bool(true));
    let battery = body["data"]["battery"].as_object().expect("battery");
    assert_eq!(battery["start_soc_pct"], serde_json::json!(50.0));
    let end_soc = battery["end_soc_pct"].as_f64().unwrap();
    assert!((end_soc - 100.0).abs() < 1e-6, "end_soc = {end_soc}");
}

// ====================================================================
// GET /api/forecast/plan — issue #283 Phase 2
// ====================================================================

/// Empty state: no tariff, no snapshot → NoPlan with a reason, 200 OK.
#[tokio::test]
async fn forecast_plan_endpoint_degrades_to_no_plan() {
    let _config = IsolatedConfig::enter();
    let state = Arc::new(AppState::new());
    let router = create_router(state);

    let (status, body) = get_json(&router, "/api/forecast/plan").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], Value::Bool(true));
    let rec = body["data"]["recommendation"].as_object().expect("rec");
    assert_eq!(rec["kind"], Value::String("no_plan".to_string()));
    // Either gate is fine for an empty state — the test's contract is
    // "no_plan with a human-readable reason", not a specific message.
    let reason = rec["reason"].as_str().unwrap().to_lowercase();
    assert!(
        reason.contains("tariff")
            || reason.contains("projection")
            || reason.contains("history"),
        "reason: {}",
        rec["reason"]
    );
}

/// Fully seeded: cloudy forecast + Flux tariff + drained battery → a
/// Charge recommendation with the off-peak window and kWh, plus the
/// exact Apply payload the UI posts to the existing control endpoints.
#[tokio::test]
async fn forecast_plan_endpoint_recommends_overnight_charge() {
    use chrono::TimeZone;
    use givenergy_local::history::{ForecastValueRow, HistoryDb};
    use givenergy_local::inverter::model::InverterSnapshot;

    let config = IsolatedConfig::enter();
    let state = Arc::new(AppState::new());

    // Cloudy forward forecast: 100 W/m² for tomorrow's daylight.
    let db = HistoryDb::open(&config.dir.join("history.db")).unwrap();
    let now = chrono::Local::now();
    let now_ts = now.timestamp();
    let hour_start = now_ts - now_ts.rem_euclid(3600);
    for h in 0..48i64 {
        let ts = hour_start + h * 3600;
        db.insert_forecast_values(&[ForecastValueRow {
            timestamp: ts,
            variable: "shortwave_radiation".to_string(),
            value: if h % 24 >= 6 && h % 24 <= 19 { 100.0 } else { 0.0 },
            source: "open-meteo".to_string(),
            fetched_at: now_ts,
        }])
        .unwrap();
    }
    db.set_meta_value("forecast_pr", "0.8").unwrap();
    db.set_meta_value("forecast_pr_days", "12").unwrap();

    // A week of consumption history (+0.5/h).
    let mut date = now.date_naive() - chrono::Duration::days(7);
    while date < now.date_naive() {
        let mut counter = 0.0_f64;
        for hour in 0..24u32 {
            let ts = chrono::Local
                .from_local_datetime(&date.and_hms_opt(hour, 0, 0).unwrap())
                .earliest()
                .unwrap()
                .timestamp();
            db.insert_reading(&InverterSnapshot {
                timestamp: ts,
                home_energy_today_kwh: counter as f32,
                ..Default::default()
            });
            counter += 0.5;
        }
        date = date.succ_opt().unwrap();
    }
    *state.history.lock().await = Some(Arc::new(db));

    // Drained battery + rate limits on the snapshot.
    *state.latest_snapshot.lock().await = Some(InverterSnapshot {
        soc: 20,
        battery_capacity_kwh: 9.5,
        max_battery_power_w: 3000,
        charge_rate: 50,
        discharge_rate: 50,
        battery_reserve: 10,
        ..Default::default()
    });
    {
        let mut ws = state.weather.lock().await;
        ws.config.enabled = true;
        ws.config.latitude = Some(51.5);
        ws.config.longitude = Some(-0.13);
    }

    let router = create_router(state.clone());

    // Flux-like tariff via the existing settings endpoint.
    let (status, body) = post_json(
        &router,
        "/api/settings",
        &json!({
            "pv1_rated_kw": 5.0,
            "import_tariff_config": {
                "slots": [
                    { "start": "00:00", "end": "02:00", "rate": 0.26 },
                    { "start": "02:00", "end": "05:00", "rate": 0.09 },
                    { "start": "05:00", "end": "16:00", "rate": 0.26 },
                    { "start": "16:00", "end": "21:00", "rate": 0.35 },
                    { "start": "21:00", "end": "23:59", "rate": 0.26 }
                ]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "settings save failed: {body}");

    let (status, body) = get_json(&router, "/api/forecast/plan").await;
    assert_eq!(status, StatusCode::OK);
    let rec = body["data"]["recommendation"].as_object().expect("rec");
    assert_eq!(rec["kind"], Value::String("charge".to_string()));
    let window = rec["window"].as_object().expect("window");
    assert_eq!(window["start"], serde_json::json!("02:00"));
    assert_eq!(window["end"], serde_json::json!("05:00"));
    let kwh = rec["kwh"].as_f64().expect("kwh");
    assert!(kwh > 0.0 && kwh < 9.5, "kwh = {kwh}");
    assert!(
        rec["rationale"].as_str().unwrap().contains("9.0p"),
        "rationale: {}",
        rec["rationale"]
    );
    // The Apply payload mirrors what the Control page posts.
    let apply = body["data"]["apply"].as_object().expect("apply");
    // The API returns target_soc as an integer; rec["target_soc_pct"]
    // holds the float from the recommendation.
    let target_soc_int = rec["target_soc_pct"].as_f64().unwrap() as u64;
    assert_eq!(apply["charge_slot"], serde_json::json!({
        "slot": 1,
        "enabled": true,
        "start_hour": 2, "start_minute": 0,
        "end_hour": 5, "end_minute": 0,
        "target_soc": target_soc_int,
    }));
    assert_eq!(apply["timed_charge"], serde_json::json!({ "enabled": true }));
}
