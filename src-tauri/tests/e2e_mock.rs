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
//!   * Most `set_*` control endpoints. Timed Export is covered here with a
//!     small completion driver; the unit-testable encoder paths cover the
//!     broader control surface.
//!   * History aggregation (covered by `history::tests`).

use std::sync::{Arc, OnceLock};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use givenergy_local::inverter::encoder::WriteOutcome;
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

/// Issue a control POST while emulating the poll loop accepting every
/// completion-backed write batch. The batches remain queued so callers can
/// inspect physical write ordering after the handler commits its settings.
async fn post_json_accepting_writes(
    router: &axum::Router,
    state: &Arc<AppState>,
    uri: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let router = router.clone();
    let uri = uri.to_string();
    let body = body.clone();
    let handle = tokio::spawn(async move { post_json(&router, &uri, &body).await });

    loop {
        if handle.is_finished() {
            break;
        }
        let mut pending = state.pending_writes.lock().await;
        if let Some(batch) = pending.iter_mut().find(|batch| batch.completion.is_some()) {
            if let Some(completion) = batch.completion.take() {
                let _ = completion.send(WriteOutcome::Ok);
            }
        }
        drop(pending);
        tokio::task::yield_now().await;
    }

    handle.await.expect("control request task")
}

/// Like [`post_json_accepting_writes`], but any completion-backed batch
/// containing `reject_address` is completed with a simulated dongle failure
/// instead of success — for tests that need one specific register write
/// (e.g. the HR110 reserve write) to be rejected while every other phase
/// succeeds.
async fn post_json_rejecting_register_writes(
    router: &axum::Router,
    state: &Arc<AppState>,
    uri: &str,
    body: &Value,
    reject_address: u16,
) -> (StatusCode, Value) {
    let router = router.clone();
    let uri = uri.to_string();
    let body = body.clone();
    let handle = tokio::spawn(async move { post_json(&router, &uri, &body).await });

    loop {
        if handle.is_finished() {
            break;
        }
        let mut pending = state.pending_writes.lock().await;
        if let Some(batch) = pending.iter_mut().find(|batch| batch.completion.is_some()) {
            if let Some(completion) = batch.completion.take() {
                let outcome = if batch.writes.iter().any(|w| w.address == reject_address) {
                    WriteOutcome::Failed {
                        address: reject_address,
                        value: 50,
                        error: "simulated dongle exception 67".into(),
                    }
                } else {
                    WriteOutcome::Ok
                };
                let _ = completion.send(outcome);
            }
        }
        drop(pending);
        tokio::task::yield_now().await;
    }

    handle.await.expect("control request task")
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
// Forecast plan auto-refresh setting
// ====================================================================

#[tokio::test]
async fn settings_round_trips_forecast_plan_auto_refresh() {
    let router = fresh_router();
    let (status, body) = get_json(&router, "/api/settings").await;
    assert_eq!(status, StatusCode::OK);
    // Opt-in: a fresh install must not let the backend rewrite charge
    // slots on its own.
    assert_eq!(
        body["data"]["forecast_plan_auto_refresh"],
        Value::Bool(false),
        "auto-refresh must default to off"
    );

    let (status, body) = post_json(
        &router,
        "/api/settings",
        &json!({ "forecast_plan_auto_refresh": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = get_json(&router, "/api/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"]["forecast_plan_auto_refresh"],
        Value::Bool(true),
        "the toggle must persist"
    );
}

// ====================================================================
// Forecast plan auto-apply setting
// ====================================================================

#[tokio::test]
async fn settings_round_trips_forecast_plan_auto_apply() {
    let router = fresh_router();
    let (status, body) = get_json(&router, "/api/settings").await;
    assert_eq!(status, StatusCode::OK);
    // Opt-in: a fresh install must not rewrite charge slot 1 on its own.
    assert_eq!(
        body["data"]["forecast_plan_auto_apply_enabled"],
        Value::Bool(false),
        "auto-apply must default to off"
    );
    assert_eq!(
        body["data"]["forecast_plan_auto_apply_lead_minutes"], 30,
        "lead must default to 30 minutes"
    );

    let (status, body) = post_json(
        &router,
        "/api/settings",
        &json!({
            "forecast_plan_auto_apply_enabled": true,
            "forecast_plan_auto_apply_lead_minutes": 90,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = get_json(&router, "/api/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"]["forecast_plan_auto_apply_enabled"],
        Value::Bool(true),
        "the toggle must persist"
    );
    assert_eq!(
        body["data"]["forecast_plan_auto_apply_lead_minutes"], 90,
        "the lead must persist"
    );
}

#[tokio::test]
async fn auto_apply_lead_minutes_above_120_is_rejected() {
    let router = fresh_router();
    let (status, body) = post_json(
        &router,
        "/api/settings",
        &json!({ "forecast_plan_auto_apply_lead_minutes": 121 }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["ok"], Value::Bool(false));
    assert!(
        body["error"].as_str().is_some_and(|e| e.contains("120")),
        "the error must name the bound"
    );
    // The rejected save must not have persisted anything.
    let (status, body) = get_json(&router, "/api/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"]["forecast_plan_auto_apply_lead_minutes"], 30,
        "the default lead must survive a rejected save"
    );

    // The boundary itself is accepted (0–120, inclusive).
    let (status, body) = post_json(
        &router,
        "/api/settings",
        &json!({ "forecast_plan_auto_apply_lead_minutes": 120 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = get_json(&router, "/api/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["forecast_plan_auto_apply_lead_minutes"], 120);
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

/// The E2E harness reset endpoint is gated behind `--e2e-admin`: production
/// launches (and this default test state) must not expose it at all.
#[tokio::test]
async fn test_reset_route_is_gated_behind_e2e_admin_flag() {
    let (router, _state) = fresh_router_with_state().await;
    let (status, body) = post_json(&router, "/api/test/reset", &json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["ok"], Value::Bool(false));
}

/// With the flag set, the reset must clear every piece of backend-owned
/// schedule/machine state the mock-register reset cannot reach: the persisted
/// Timed Export schedule (enabled flag, slots, learned re-arm fallback, slot
/// backup), the in-memory config mirror, the boundary state machine, the
/// re-arm detector, and captured Force Charge/Discharge restore snapshots.
#[tokio::test]
async fn test_reset_clears_schedule_state_and_force_reverts() {
    let (router, state) = fresh_router_with_state().await;
    state
        .e2e_admin
        .store(true, std::sync::atomic::Ordering::SeqCst);

    // Dirty every surface the reset owns.
    givenergy_local::settings::Settings::update(|s| {
        s.timed_export_schedule_enabled = true;
        s.timed_export_slots = vec![givenergy_local::inverter::model::ScheduleSlot {
            enabled: true,
            start_hour: 16,
            start_minute: 0,
            end_hour: 19,
            end_minute: 0,
            target_soc: 4,
        }];
        s.timed_export_slots_require_clear = true;
        s.discharge_slots_backup = Some(Vec::new());
    })
    .unwrap();
    {
        let mut config = state.timed_export_config.lock().await;
        config.schedule_enabled = true;
        config.slots = vec![givenergy_local::inverter::model::ScheduleSlot {
            enabled: true,
            start_hour: 16,
            start_minute: 0,
            end_hour: 19,
            end_minute: 0,
            target_soc: 4,
        }];
        config.device_rearm_confirmed = true;
    }
    *state.timed_export_state.lock().await =
        givenergy_local::inverter::state_machines::TimedExportState::Active;
    *state.force_charge_revert.lock().await =
        Some(givenergy_local::inverter::poll::ForceChargeRevert {
            enable_charge: true,
            enable_discharge: false,
            target_soc: 100,
            battery_power_mode: 1,
            charge_rate: Some(100),
            charge_slot_1_start: Some((0, 0)),
            charge_slot_1_end: Some((6, 0)),
            three_phase_force_charge_enable: None,
            three_phase_ac_charge_enable: None,
            battery_pause_mode: None,
        });
    *state.force_discharge_revert.lock().await =
        Some(givenergy_local::inverter::poll::ForceDischargeRevert {
            enable_charge: false,
            enable_discharge: true,
            discharge_rate: Some(100),
            discharge_slot_1_start: Some((16, 0)),
            discharge_slot_1_end: Some((19, 0)),
            discharge_slot_2_start: None,
            discharge_slot_2_end: None,
            three_phase_force_discharge_enable: None,
            three_phase_force_charge_enable: None,
            force_discharge_slot_end_ms: None,
            battery_pause_mode: 0,
            battery_pause_slot: Default::default(),
        });

    let (status, body) = post_json(&router, "/api/test/reset", &json!({})).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let settings = givenergy_local::settings::Settings::load();
    assert!(!settings.timed_export_schedule_enabled);
    assert!(settings.timed_export_slots.is_empty());
    assert!(!settings.timed_export_slots_require_clear);
    assert!(settings.discharge_slots_backup.is_none());
    {
        let config = state.timed_export_config.lock().await;
        assert!(!config.schedule_enabled);
        assert!(config.slots.is_empty());
        assert!(!config.device_rearm_confirmed);
    }
    assert_eq!(
        *state.timed_export_state.lock().await,
        givenergy_local::inverter::state_machines::TimedExportState::Off
    );
    assert!(state.force_charge_revert.lock().await.is_none());
    assert!(state.force_discharge_revert.lock().await.is_none());
    assert!(
        state.pending_writes.lock().await.is_empty(),
        "the reset must drop leftover queued write bursts"
    );
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
// Issue #289 — Timed Export desired-schedule API behaviour
// ====================================================================

/// Build a router plus its `AppState` so tests can seed snapshots and
/// inspect queued pending writes.
async fn fresh_router_with_state() -> (IsolatedRouter, Arc<AppState>) {
    let config = IsolatedConfig::enter();
    let state = Arc::new(AppState::new());
    let router = create_router(state.clone());
    (
        IsolatedRouter {
            router,
            _config: config,
        },
        state,
    )
}

/// Seed a Gen3-hybrid snapshot in Eco mode with the given discharge slots.
async fn seed_eco_snapshot(
    state: &Arc<AppState>,
    slots: Vec<givenergy_local::inverter::model::ScheduleSlot>,
    pause_mode: u8,
) {
    use givenergy_local::inverter::model::{
        BatteryMode, BatteryState, DeviceType, InverterSnapshot, ScheduleSlot,
    };
    let mut snapshot = InverterSnapshot {
        device_type: DeviceType::Gen3Hybrid,
        device_type_code: "2001".into(),
        inverter_serial: "CE289".into(),
        soc: 50,
        battery_mode: BatteryMode::Eco,
        battery_state: BatteryState::Idle,
        battery_power_mode: 1,
        enable_charge: false,
        enable_discharge: false,
        battery_pause_mode: pause_mode,
        ..Default::default()
    };
    let mut arr: [ScheduleSlot; 10] = std::array::from_fn(|_| ScheduleSlot::default());
    for (dst, src) in arr.iter_mut().zip(slots) {
        *dst = src;
    }
    snapshot.discharge_slots = arr;
    let mut guard = state.latest_snapshot.lock().await;
    *guard = Some(snapshot);
}

/// Compute an (hour, minute) pair offset from now, wrapping past midnight.
fn time_offset_from_now(minutes: i64) -> (u8, u8) {
    use chrono::Timelike;
    let t = chrono::Local::now() + chrono::Duration::minutes(minutes);
    (t.hour() as u8, t.minute() as u8)
}

/// Drain the pending-write queue and flatten the (address, value) pairs.
async fn drain_pending_writes(state: &Arc<AppState>) -> Vec<(u16, u16)> {
    let mut guard = state.pending_writes.lock().await;
    guard
        .drain(..)
        .flat_map(|batch| batch.writes.into_iter().map(|w| (w.address, w.value)))
        .collect()
}

/// Drain the pending-write queue preserving each batch's discharge-control
/// owner, so tests can assert which owner the API queued a batch with (the
/// admission matrix in `poll.rs` decides admission from that owner).
async fn drain_pending_batches_with_owners(
    state: &Arc<AppState>,
) -> Vec<(
    Option<givenergy_local::inverter::state_machines::DischargeControlOwner>,
    Vec<(u16, u16)>,
)> {
    let mut guard = state.pending_writes.lock().await;
    guard
        .drain(..)
        .map(|batch| {
            (
                batch.owner,
                batch
                    .writes
                    .into_iter()
                    .map(|w| (w.address, w.value))
                    .collect(),
            )
        })
        .collect()
}

/// Saving an enabled discharge slot that starts in the future must NOT
/// immediately queue HR27=0/HR59=1 export writes (issue #289): Eco stays
/// the baseline until the poll-loop boundary state machine enters the
/// window.
#[tokio::test]
async fn discharge_slot_future_save_persists_schedule_without_arming_export() {
    let (router, state) = fresh_router_with_state().await;
    seed_eco_snapshot(
        &state,
        vec![givenergy_local::inverter::model::ScheduleSlot::default(); 2],
        0,
    )
    .await;

    let (start_h, start_m) = time_offset_from_now(60);
    let (end_h, end_m) = time_offset_from_now(90);
    let (status, body) = post_json_accepting_writes(
        &router,
        &state,
        "/api/control/discharge-slot",
        &json!({
            "slot": 1,
            "enabled": true,
            "start_hour": start_h,
            "start_minute": start_m,
            "end_hour": end_h,
            "end_minute": end_m,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let writes = drain_pending_writes(&state).await;
    assert!(
        writes.iter().any(|(a, _)| *a == 56 || *a == 57),
        "slot registers must still be written, got {writes:?}"
    );
    assert!(
        !writes.iter().any(|(a, v)| *a == 27 && *v == 0),
        "future slot must not switch HR27 to export mode, got {writes:?}"
    );
    assert!(
        !writes.iter().any(|(a, v)| *a == 59 && *v == 1),
        "future slot must not arm HR59, got {writes:?}"
    );

    // The desired schedule is persisted for the boundary state machine.
    let settings = givenergy_local::settings::Settings::load();
    assert!(settings.timed_export_schedule_enabled);
    assert!(settings
        .timed_export_slots
        .iter()
        .any(|s| s.enabled && s.start_hour == start_h && s.start_minute == start_m));
}

/// Saving an enabled slot whose window contains "now" (constructed at test
/// time so the assertion holds for any wall clock) arms export immediately
/// — HR27=0 before HR59=1.
#[tokio::test]
async fn discharge_slot_in_window_save_arms_export_immediately() {
    let (router, state) = fresh_router_with_state().await;
    seed_eco_snapshot(
        &state,
        vec![givenergy_local::inverter::model::ScheduleSlot::default(); 2],
        0,
    )
    .await;

    let (start_h, start_m) = time_offset_from_now(-10);
    let (end_h, end_m) = time_offset_from_now(20);
    let (status, body) = post_json_accepting_writes(
        &router,
        &state,
        "/api/control/discharge-slot",
        &json!({
            "slot": 1,
            "enabled": true,
            "start_hour": start_h,
            "start_minute": start_m,
            "end_hour": end_h,
            "end_minute": end_m,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let writes = drain_pending_writes(&state).await;
    let hr27 = writes.iter().position(|(a, v)| *a == 27 && *v == 0);
    let hr59 = writes.iter().position(|(a, v)| *a == 59 && *v == 1);
    assert!(
        hr27.is_some(),
        "in-window save arms max-power mode: {writes:?}"
    );
    assert!(hr59.is_some(), "in-window save arms discharge: {writes:?}");
    assert!(
        hr27.unwrap() < hr59.unwrap(),
        "entry ordering: HR27=0 must precede HR59=1"
    );
}

/// Enabling Timed Export with only a future slot configured enables the
/// HEM-managed schedule without leaving HR27 in export mode all day.
#[tokio::test]
async fn timed_export_enable_with_future_slot_persists_without_arming() {
    let (router, state) = fresh_router_with_state().await;
    let (start_h, start_m) = time_offset_from_now(60);
    let (end_h, end_m) = time_offset_from_now(90);
    seed_eco_snapshot(
        &state,
        vec![
            givenergy_local::inverter::model::ScheduleSlot {
                enabled: true,
                start_hour: start_h,
                start_minute: start_m,
                end_hour: end_h,
                end_minute: end_m,
                target_soc: 4,
            },
            givenergy_local::inverter::model::ScheduleSlot::default(),
        ],
        0,
    )
    .await;

    let (status, body) = post_json_accepting_writes(
        &router,
        &state,
        "/api/control/timed-export",
        &json!({ "enabled": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let writes = drain_pending_writes(&state).await;
    assert!(
        !writes.iter().any(|(a, v)| *a == 27 && *v == 0),
        "future slot enable must not write HR27=0, got {writes:?}"
    );
    assert!(
        !writes.iter().any(|(a, v)| *a == 59 && *v == 1),
        "future slot enable must not write HR59=1, got {writes:?}"
    );

    let settings = givenergy_local::settings::Settings::load();
    assert!(settings.timed_export_schedule_enabled);
}

/// GET /api/timed-export exposes the desired schedule + state-machine
/// state so the UI can show Configured slots even when the physical
/// inverter slots are temporarily cleared.
#[tokio::test]
async fn timed_export_get_returns_schedule_state() {
    let (router, state) = fresh_router_with_state().await;
    let (start_h, start_m) = time_offset_from_now(60);
    let (end_h, end_m) = time_offset_from_now(90);
    seed_eco_snapshot(
        &state,
        vec![
            givenergy_local::inverter::model::ScheduleSlot {
                enabled: true,
                start_hour: start_h,
                start_minute: start_m,
                end_hour: end_h,
                end_minute: end_m,
                target_soc: 4,
            },
            givenergy_local::inverter::model::ScheduleSlot::default(),
        ],
        0,
    )
    .await;

    let (status, body) = post_json_accepting_writes(
        &router,
        &state,
        "/api/control/timed-export",
        &json!({ "enabled": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, body) = get_json(&router, "/api/timed-export").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["data"]["schedule_enabled"], Value::Bool(true));
    let slots = body["data"]["slots"].as_array().expect("slots array");
    assert!(!slots.is_empty());
    assert_eq!(slots[0]["enabled"], Value::Bool(true));
    assert_eq!(slots[0]["start_hour"], json!(start_h));
}

/// Seed an "unclaimed manual Timed Demand" snapshot: HR27=1 / HR59=1 with
/// no automation (Agile/Cosy) owning the mode. This is the state the
/// simulator run in CODE_REVIEW.md showed blocking the Timed Export arm —
/// `current_discharge_control_owner` treats it as `ManualMode`.
async fn seed_timed_demand_snapshot(state: &Arc<AppState>) {
    use givenergy_local::inverter::model::{
        BatteryMode, BatteryState, DeviceType, InverterSnapshot, ScheduleSlot,
    };
    let mut snapshot = InverterSnapshot {
        device_type: DeviceType::Gen3Hybrid,
        device_type_code: "2001".into(),
        inverter_serial: "CE289".into(),
        soc: 71,
        battery_mode: BatteryMode::TimedDemand,
        battery_state: BatteryState::Idle,
        battery_power_mode: 1,
        enable_charge: false,
        enable_discharge: true,
        battery_pause_mode: 0,
        ..Default::default()
    };
    snapshot.discharge_slots = std::array::from_fn(|_| ScheduleSlot::default());
    *state.latest_snapshot.lock().await = Some(snapshot);
}

/// Seed a "physically exporting under an active HR318 pause" snapshot:
/// HR27=0 / HR59=1 plus a Timed Discharge pause window covering the pinned
/// inverter minute. This is the live-session scenario where the
/// register-derived `ExplicitPause` owner deferred the user's stop batch
/// until the 15 s completion timeout fired — while the inverter was, in
/// that session, already back at the Eco baseline.
async fn seed_exporting_under_pause_snapshot(state: &Arc<AppState>) {
    use givenergy_local::inverter::model::{
        BatteryMode, BatteryState, DeviceType, InverterSnapshot, ScheduleSlot,
    };
    let mut snapshot = InverterSnapshot {
        device_type: DeviceType::Gen3Hybrid,
        device_type_code: "2001".into(),
        inverter_serial: "CE289".into(),
        // Pin the inverter clock so the 00:00–23:59 pause window (which
        // misses only minute 1439) deterministically covers it.
        inverter_time: "2026-08-30 12:00:00".into(),
        soc: 71,
        battery_mode: BatteryMode::TimedExport,
        battery_state: BatteryState::Idle,
        battery_power_mode: 0,
        enable_charge: false,
        enable_discharge: true,
        battery_pause_mode: 2,
        battery_pause_slot: ScheduleSlot {
            enabled: true,
            start_hour: 0,
            start_minute: 0,
            end_hour: 23,
            end_minute: 59,
            target_soc: 100,
        },
        ..Default::default()
    };
    snapshot.discharge_slots = std::array::from_fn(|_| ScheduleSlot::default());
    *state.latest_snapshot.lock().await = Some(snapshot);
}

/// CODE_REVIEW.md scenario: saving a valid in-window Timed Export slot must
/// replace an unclaimed manual Timed Demand mode. The HR27=0 + model-routed
/// discharge-enable arm writes are admitted (not deferred behind the
/// ManualMode owner) and the request only reports success once they were
/// accepted.
#[tokio::test]
async fn timed_export_save_over_unclaimed_timed_demand_arms_export() {
    let (router, state) = fresh_router_with_state().await;
    seed_timed_demand_snapshot(&state).await;

    let (start_h, start_m) = time_offset_from_now(-10);
    let (end_h, end_m) = time_offset_from_now(20);
    let (status, body) = post_json_accepting_writes(
        &router,
        &state,
        "/api/control/discharge-slot",
        &json!({
            "slot": 1,
            "enabled": true,
            "start_hour": start_h,
            "start_minute": start_m,
            "end_hour": end_h,
            "end_minute": end_m,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // The arm writes were queued despite the snapshot's Timed Demand state.
    let writes = drain_pending_writes(&state).await;
    let hr27 = writes.iter().position(|(a, v)| *a == 27 && *v == 0);
    let hr59 = writes.iter().position(|(a, v)| *a == 59 && *v == 1);
    assert!(
        hr27.is_some(),
        "save over manual Timed Demand must arm max-power mode: {writes:?}"
    );
    assert!(
        hr59.is_some(),
        "save over manual Timed Demand must arm discharge: {writes:?}"
    );
    assert!(
        hr27.unwrap() < hr59.unwrap(),
        "entry ordering: HR27=0 must precede HR59=1"
    );

    let settings = givenergy_local::settings::Settings::load();
    assert!(
        settings.timed_export_schedule_enabled,
        "accepted arm must persist the desired schedule"
    );
}

/// Drive a control request while answering unowned (slot) write batches with
/// `WriteOutcome::Ok` and every owned discharge-control batch with a
/// rejection — the "inverter refused the arm write" failure path.
async fn post_json_rejecting_arm_writes(
    router: &axum::Router,
    state: &Arc<AppState>,
    uri: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let router = router.clone();
    let uri = uri.to_string();
    let body = body.clone();
    let handle = tokio::spawn(async move { post_json(&router, &uri, &body).await });

    loop {
        if handle.is_finished() {
            break;
        }
        let mut pending = state.pending_writes.lock().await;
        if let Some(batch) = pending.iter_mut().find(|batch| batch.completion.is_some()) {
            if let Some(completion) = batch.completion.take() {
                // Reject the mode-transition batch (it contains HR27); let
                // slot/target batches through so tests reach the phase under
                // test. Ownership is deliberately not the discriminator: the
                // arm phase is TimedExport-owned, but the stop's disarm is
                // ManualMode-owned (a user baseline selection), and both must
                // be rejectable here.
                let outcome = if batch.writes.iter().any(|w| w.address == 27) {
                    WriteOutcome::Failed {
                        address: 27,
                        value: 0,
                        error: "simulated dongle exception 67".into(),
                    }
                } else {
                    WriteOutcome::Ok
                };
                let _ = completion.send(outcome);
            }
        }
        drop(pending);
        tokio::task::yield_now().await;
    }

    handle.await.expect("control request task")
}

/// CODE_REVIEW.md failure requirement: a rejected arm write must not report
/// physical activation, and the desired slot configuration must be retained
/// so the boundary state machine (or a manual retry) can arm later.
#[tokio::test]
async fn timed_export_arm_rejection_retains_schedule_without_claiming_activation() {
    let (router, state) = fresh_router_with_state().await;
    seed_eco_snapshot(
        &state,
        vec![givenergy_local::inverter::model::ScheduleSlot::default(); 2],
        0,
    )
    .await;

    let (start_h, start_m) = time_offset_from_now(-10);
    let (end_h, end_m) = time_offset_from_now(20);
    let (status, body) = post_json_rejecting_arm_writes(
        &router,
        &state,
        "/api/control/discharge-slot",
        &json!({
            "slot": 1,
            "enabled": true,
            "start_hour": start_h,
            "start_minute": start_m,
            "end_hour": end_h,
            "end_minute": end_m,
        }),
    )
    .await;

    // Actionable failure — not a success envelope that would make the UI
    // report an armed/active Timed Export.
    assert_eq!(status, StatusCode::BAD_GATEWAY, "body: {body}");
    assert_ne!(body["ok"], Value::Bool(true));
    let error = body["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("could not be armed"),
        "error must explain the arm failure: {error}"
    );

    // The desired schedule is retained for retry…
    let settings = givenergy_local::settings::Settings::load();
    assert!(
        settings.timed_export_schedule_enabled,
        "rejected arm must still retain the desired schedule"
    );
    assert!(settings
        .timed_export_slots
        .iter()
        .any(|slot| slot.enabled && slot.start_hour == start_h && slot.start_minute == start_m));

    // …without claiming physical activation: the machine has not advanced to
    // Active.
    let (status, body) = get_json(&router, "/api/timed-export").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let machine_state = body["data"]["machine_state"].clone();
    assert_ne!(
        machine_state,
        json!("Active"),
        "a rejected arm must not surface machine state Active"
    );
}

/// CODE_REVIEW.md finding: the optional `soc_reserve` write in
/// `set_timed_export` was queued fire-and-forget after the arm/Eco phases —
/// a rejected reserve never failed the request even though the response said
/// "Timed Export enabled". The reserve write must be model-routed,
/// transactional, and complete before the arm: a rejected reserve fails the
/// enable with the schedule unpersisted and maximum-power export never armed.
#[tokio::test]
async fn timed_export_reserve_write_rejection_fails_enable_without_persisting() {
    let (router, state) = fresh_router_with_state().await;
    // Eco readback with a configured in-window slot. With nothing persisted
    // yet, the enable seeds its desired schedule from the live snapshot.
    let (start_h, start_m) = time_offset_from_now(-10);
    let (end_h, end_m) = time_offset_from_now(20);
    seed_eco_snapshot(
        &state,
        vec![givenergy_local::inverter::model::ScheduleSlot {
            enabled: true,
            start_hour: start_h,
            start_minute: start_m,
            end_hour: end_h,
            end_minute: end_m,
            target_soc: 0,
        }],
        0,
    )
    .await;

    let (status, body) = post_json_rejecting_register_writes(
        &router,
        &state,
        "/api/control/timed-export",
        &json!({ "enabled": true, "soc_reserve": 50 }),
        110,
    )
    .await;

    // Actionable failure — not a success envelope that would make the UI
    // report an enabled schedule whose reserve write was rejected.
    assert_eq!(status, StatusCode::BAD_GATEWAY, "body: {body}");
    assert_ne!(body["ok"], Value::Bool(true));
    let error = body["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("reserve"),
        "error must name the failed reserve write: {error}"
    );

    // The schedule must not be persisted: the required physical writes did
    // not all succeed.
    let settings = givenergy_local::settings::Settings::load();
    assert!(
        !settings.timed_export_schedule_enabled,
        "a rejected reserve write must not leave the schedule enabled"
    );
    assert!(
        !settings.timed_export_slots.iter().any(|slot| slot.enabled),
        "a rejected reserve write must not persist the desired slots"
    );
    let writes = drain_pending_writes(&state).await;
    assert!(
        !writes
            .iter()
            .any(|(address, value)| (*address == 27 && *value == 0)
                || (*address == 59 && *value == 1)),
        "a rejected reserve must fail before any maximum-power arm writes: {writes:?}"
    );
}

/// CODE_REVIEW.md finding 1: Stop (disable) must not be fire-and-forget.
/// Enable is fully transactional, but disable queued its writes without a
/// completion channel and returned OK unconditionally — a rejected or
/// indefinitely-deferred disarm left the inverter exporting on its own
/// armed schedule while the response (and UI) claimed success. The stop
/// must await the disarm batch and report an actionable failure instead.
#[tokio::test]
async fn timed_export_disable_rejection_reports_failure_instead_of_ok() {
    let (router, state) = fresh_router_with_state().await;
    // Physically exporting (HR27=0/HR59=1) — the disarm batch is required,
    // so a rejecting inverter must surface its failure. (The stop always
    // queues the disarm now, even when readback already shows Eco, so a
    // later poll-cycle transition cannot re-arm export under the user's
    // nose — see `timed_export_stop_always_issues_disarm_even_when_eco_already_confirmed`.)
    seed_exporting_under_pause_snapshot(&state).await;

    // An enabled schedule is live: a Stop request against a rejecting
    // inverter must not report success.
    givenergy_local::settings::Settings::update(|s| {
        s.timed_export_schedule_enabled = true;
    })
    .expect("seed enabled schedule");

    let (status, body) = post_json_rejecting_arm_writes(
        &router,
        &state,
        "/api/control/timed-export",
        &json!({ "enabled": false }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_GATEWAY, "body: {body}");
    assert_ne!(body["ok"], Value::Bool(true));
    let error = body["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("could not be stopped"),
        "error must explain the stop failure: {error}"
    );

    // The desired Off state is still persisted, so a later poll/retry
    // converges — but the request itself must not claim success.
    let settings = givenergy_local::settings::Settings::load();
    assert!(
        !settings.timed_export_schedule_enabled,
        "rejected stop still persists the disabled schedule for retry"
    );
}

/// Live-session follow-up: stopping Timed Export while a Timed Discharge
/// pause window blocks discharge must not time out. The register-derived
/// `ExplicitPause` owner deferred the stop's `TimedExport`-owned batch
/// forever (issue #289 precedence defers automations behind a pause), so
/// the transactional stop reported "did not complete within 15 seconds".
/// A user-issued stop is a manual baseline selection — HR27=1/HR59=0 do
/// not conflict with the independent HR318 gate — so the batch must queue
/// as `ManualMode`, which the admission matrix (poll.rs) shows always
/// drains within one poll cycle.
#[tokio::test]
async fn timed_export_stop_disarm_batch_uses_manual_mode_owner() {
    use givenergy_local::inverter::state_machines::DischargeControlOwner;

    let (router, state) = fresh_router_with_state().await;
    seed_exporting_under_pause_snapshot(&state).await;
    givenergy_local::settings::Settings::update(|s| {
        s.timed_export_schedule_enabled = true;
    })
    .expect("seed enabled schedule");

    let (status, body) = post_json_accepting_writes(
        &router,
        &state,
        "/api/control/timed-export",
        &json!({ "enabled": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let batches = drain_pending_batches_with_owners(&state).await;
    let disarm = batches
        .iter()
        .find(|(owner, writes)| owner.is_some() && writes.iter().any(|(a, v)| *a == 59 && *v == 0));
    let Some((owner, _)) = disarm else {
        panic!("disarm batch must be queued: {batches:?}");
    };
    assert_eq!(
        *owner,
        Some(DischargeControlOwner::ManualMode),
        "user-issued stop must queue as ManualMode so an HR318 pause cannot starve it"
    );
}

/// Stop must always issue the disarm writes — even when the snapshot
/// already shows the Eco baseline. Skipping the disarm left a window
/// where a poll-cycle transition could re-arm export after the endpoint
/// returned 200 OK. The disarm is idempotent (HR27 and the model-routed
/// enable register are both safe to rewrite) and bounded by the
/// completion timeout, so a future-window stop no longer than an
/// in-window stop.
#[tokio::test]
async fn timed_export_stop_always_issues_disarm_even_when_eco_already_confirmed() {
    let (router, state) = fresh_router_with_state().await;
    // Eco snapshot (HR27=1, HR59=0) with an armed Timed Discharge pause —
    // exactly the live session's state when Stop was clicked.
    seed_eco_snapshot(
        &state,
        vec![givenergy_local::inverter::model::ScheduleSlot::default(); 2],
        2,
    )
    .await;
    givenergy_local::settings::Settings::update(|s| {
        s.timed_export_schedule_enabled = true;
    })
    .expect("seed enabled schedule");

    let (status, body) = post_json_accepting_writes(
        &router,
        &state,
        "/api/control/timed-export",
        &json!({ "enabled": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // The disarm writes must be queued even though the snapshot already
    // shows Eco: a poll-cycle transition between this response and the
    // next poll could otherwise re-arm export while the user believes
    // Timed Export is off.
    let writes = drain_pending_writes(&state).await;
    let disarm_hr27 = writes.iter().any(|(a, v)| *a == 27 && *v == 1);
    let disarm_hr59 = writes.iter().any(|(a, v)| *a == 59 && *v == 0);
    assert!(
        disarm_hr27 && disarm_hr59,
        "Stop must always write HR27=1 and HR59=0; got {writes:?}"
    );
}

/// The future-slot save's Eco-baseline restore is also user-issued (the
/// user just saved a schedule whose window hasn't opened). It must queue
/// as `ManualMode` too, or an armed HR318 pause starves it the same way
/// it starved the stop.
#[tokio::test]
async fn timed_export_future_slot_eco_restore_uses_manual_mode_owner() {
    use givenergy_local::inverter::state_machines::DischargeControlOwner;

    let (router, state) = fresh_router_with_state().await;
    // Unclaimed Timed Demand: HR27=1/HR59=1, so the Eco baseline is NOT
    // confirmed and the future-slot save must restore it.
    seed_timed_demand_snapshot(&state).await;

    let (start_h, start_m) = time_offset_from_now(60);
    let (end_h, end_m) = time_offset_from_now(90);
    let (status, body) = post_json_accepting_writes(
        &router,
        &state,
        "/api/control/discharge-slot",
        &json!({
            "slot": 1,
            "enabled": true,
            "start_hour": start_h,
            "start_minute": start_m,
            "end_hour": end_h,
            "end_minute": end_m,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let batches = drain_pending_batches_with_owners(&state).await;
    let restore = batches
        .iter()
        .find(|(owner, writes)| owner.is_some() && writes.iter().any(|(a, v)| *a == 27 && *v == 1));
    let Some((owner, _)) = restore else {
        panic!("Eco-baseline restore batch must be queued: {batches:?}");
    };
    assert_eq!(
        *owner,
        Some(DischargeControlOwner::ManualMode),
        "user-issued Eco restore must queue as ManualMode so a pause cannot starve it"
    );
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
    let (status, body) = post_json(&router, "/api/settings", &json!({ "pv1_rated_kw": 5.0 })).await;
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
    // No slots configured → the current-schedule projection is omitted:
    // it would merely duplicate the Eco line (issue #297).
    assert!(battery["with_current_schedule"].is_null());
}

/// An enabled overnight charge slot must surface the current-schedule
/// projection through the API: same hour count as the Eco projection,
/// but actually shaped by the slot — charged up to the slot target
/// overnight while Eco bleeds to the reserve (issue #297).
#[tokio::test]
async fn forecast_endpoint_includes_current_schedule_when_slots_enabled() {
    use chrono::{TimeZone, Timelike};
    use givenergy_local::forecast::{META_FORECAST_PR, META_FORECAST_PR_DAYS};
    use givenergy_local::history::{ForecastValueRow, HistoryDb};
    use givenergy_local::inverter::model::{InverterSnapshot, ScheduleSlot};

    let config = IsolatedConfig::enter();
    let state = Arc::new(AppState::new());

    // Cloudy forward forecast: daylight only (06:00–19:00 local at
    // 100 W/m²), so the nights are genuinely dark and the charge slot
    // matters. The strongest solar hour (0.4 kWh after PR) is weaker
    // than the 0.5 kWh/h household load below, so every assertion holds
    // no matter when the test runs.
    let db = HistoryDb::open(&config.dir.join("history.db")).unwrap();
    let now = chrono::Local::now();
    let now_ts = now.timestamp();
    let hour_start = now_ts - now_ts.rem_euclid(3600);
    for h in 0..72i64 {
        let local_hour = chrono::DateTime::from_timestamp(hour_start + h * 3600, 0)
            .map(|dt| dt.with_timezone(&chrono::Local).hour())
            .unwrap_or(0);
        db.insert_forecast_values(&[ForecastValueRow {
            timestamp: hour_start + h * 3600,
            variable: "shortwave_radiation".to_string(),
            value: if (6..=19).contains(&local_hour) {
                100.0
            } else {
                0.0
            },
            source: "open-meteo".to_string(),
            fetched_at: now_ts,
        }])
        .unwrap();
    }
    db.set_meta_value(META_FORECAST_PR, "0.8").unwrap();
    db.set_meta_value(META_FORECAST_PR_DAYS, "12").unwrap();

    // A week of consumption history (+0.5 kWh/h).
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

    // Low battery with an armed overnight charge slot to 65%.
    let mut snap = InverterSnapshot {
        soc: 20,
        battery_capacity_kwh: 9.5,
        max_battery_power_w: 3000,
        charge_rate: 50,
        discharge_rate: 50,
        battery_reserve: 10,
        ..Default::default()
    };
    snap.enable_charge = true;
    snap.charge_slots[0] = ScheduleSlot {
        enabled: true,
        start_hour: 23,
        start_minute: 0,
        end_hour: 6,
        end_minute: 0,
        target_soc: 65,
    };
    *state.latest_snapshot.lock().await = Some(snap);
    {
        let mut ws = state.weather.lock().await;
        ws.config.enabled = true;
        ws.config.latitude = Some(51.5);
        ws.config.longitude = Some(-0.13);
    }

    let router = create_router(state.clone());
    let (status, body) = post_json(&router, "/api/settings", &json!({ "pv1_rated_kw": 5.0 })).await;
    assert_eq!(status, StatusCode::OK, "settings save failed: {body}");

    let (status, body) = get_json(&router, "/api/forecast").await;
    assert_eq!(status, StatusCode::OK);
    let battery = body["data"]["battery"].as_object().expect("battery");
    let hours = battery["hours"].as_array().expect("eco hours");
    let schedule = battery["with_current_schedule"]
        .as_array()
        .expect("enabled charge slot must yield with_current_schedule");
    assert_eq!(schedule.len(), hours.len());

    let eco_max = hours
        .iter()
        .filter_map(|p| p[1].as_f64())
        .fold(f64::MIN, f64::max);
    let schedule_max = schedule
        .iter()
        .filter_map(|p| p[1].as_f64())
        .fold(f64::MIN, f64::max);
    // Eco never recovers from the 20% start (daylight is too weak to
    // outpace the household load), while the slot charges to 65%.
    assert!(
        eco_max < 30.0,
        "eco projection must stay drained overnight, max = {eco_max}"
    );
    assert!(
        schedule_max >= 64.0,
        "schedule projection must reach the 65% slot target, max = {schedule_max}"
    );
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
        reason.contains("tariff") || reason.contains("projection") || reason.contains("history"),
        "reason: {}",
        rec["reason"]
    );
    // Without a plan there is no export advice either — the field is
    // null so the UI hides the export card entirely.
    assert!(body["data"]["export"].is_null());
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
    // The full 72 h planning horizon, so the forward series always holds
    // TWO cheap-window occurrences (the charged one plus the next start
    // the one-cycle plan stops at) no matter what time of day the suite
    // runs at.
    for h in 0..73i64 {
        let ts = hour_start + h * 3600;
        db.insert_forecast_values(&[ForecastValueRow {
            timestamp: ts,
            variable: "shortwave_radiation".to_string(),
            value: if h % 24 >= 6 && h % 24 <= 19 {
                100.0
            } else {
                0.0
            },
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
    // One-cycle sizing: the slot is the SHORTEST max-rate window that
    // holds the 20% floor until the next cheap-period start, so the end
    // sits inside the cheap period (strictly after its start, at or
    // before its end). The exact minute is hour-of-day dependent — the
    // handler reads the real clock against the seeded forward series —
    // so the deterministic exact-minute minimality is pinned by the
    // planner unit tests instead (fixed fixtures, injectable `now_ts`).
    let end = window["end"].as_str().expect("window end string");
    let (end_h, end_m) = {
        let parts: Vec<u32> = end.split(':').map(|p| p.parse().expect("HH:MM")).collect();
        (parts[0], parts[1])
    };
    let end_min = end_h * 60 + end_m;
    assert!(
        end_min > 2 * 60 && end_min <= 5 * 60,
        "window end {end} must fall inside the 02:00–05:00 cheap period"
    );
    let kwh = rec["kwh"].as_f64().expect("kwh");
    // The window's deliverable ceiling is 3 kW × 3 h.
    assert!(kwh > 0.0 && kwh <= 9.0 + 1e-6, "kwh = {kwh}");
    assert!(
        rec["rationale"].as_str().unwrap().contains("9.0p"),
        "rationale: {}",
        rec["rationale"]
    );
    // The re-simulated post-charge trough is reported alongside the
    // uncharged one (planner v2 objective).
    assert!(rec["observed_min_soc_pct"].as_f64().unwrap() < 20.0);
    assert!(
        rec["after_min_soc_pct"].as_f64().unwrap() >= rec["observed_min_soc_pct"].as_f64().unwrap()
    );
    // Tomorrow's import/export under the plan drive the Tomorrow tiles.
    // The import includes the window's grid draw exactly once (the
    // what-if sim hides it as free surplus) plus the residual of
    // tomorrow's what-if hours — so it must be at least the nightly
    // kWh, and less than two nights' worth.
    let import_tw = rec["import_tomorrow_with_charge_kwh"]
        .as_f64()
        .expect("import_tomorrow_with_charge_kwh");
    let kwh_val = rec["kwh"].as_f64().unwrap();
    assert!(
        import_tw >= kwh_val - 1e-6,
        "tomorrow import ({import_tw}) must include the window draw ({kwh_val})"
    );
    assert!(
        import_tw < 2.0 * kwh_val,
        "window draw counted once, not per night"
    );
    assert!(rec["export_tomorrow_with_charge_kwh"]
        .as_f64()
        .is_some_and(|v| v >= 0.0));
    // The Apply payload uses the inverter's maximum charge rate for the
    // shortened slot duration the plan sized. Target SOC 100 means the
    // inverter must not stop early at an SOC threshold; after the slot
    // ends, normal Eco behaviour resumes for the rest of the cheap
    // period. The payload must mirror the recommendation's window
    // exactly, whatever minute the bisection landed on.
    let apply = body["data"]["apply"].as_object().expect("apply");
    // The planner's re-simulated trajectory ("SOC if the recommended
    // charge is enacted") is what the Battery tab chart plots as a
    // dashed overlay on top of the recorded SOC history. It ends at
    // the next cheap-period start so the following charge can be
    // recalculated from a fresh live SOC.
    let with_charge_series = rec["with_charge_series"]
        .as_array()
        .expect("with_charge_series");
    let forecast = get_json(&router, "/api/forecast").await;
    assert_eq!(forecast.0, StatusCode::OK);
    let uncharged_series = forecast.1["data"]["battery"]["hours"]
        .as_array()
        .expect("battery.hours");
    assert!(
        with_charge_series.len() < uncharged_series.len(),
        "with_charge_series should end before the next cheap-period charge"
    );
    for (i, (charged, uncharged)) in with_charge_series
        .iter()
        .zip(uncharged_series.iter())
        .enumerate()
    {
        let ts = charged[0].as_i64().expect("timestamp");
        let soc = charged[1].as_f64().expect("soc");
        let u_ts = uncharged[0].as_i64().expect("timestamp");
        let u_soc = uncharged[1].as_f64().expect("soc");
        assert_eq!(ts, u_ts, "timestamps must align (hour {i})");
        assert!(
            soc >= u_soc - 1e-6,
            "with-charge must not dip below uncharged at hour {i}"
        );
    }
    assert_eq!(
        apply["charge_slot"],
        serde_json::json!({
            "slot": 1,
            "enabled": true,
            "start_hour": 2, "start_minute": 0,
            "end_hour": end_h, "end_minute": end_m,
            "target_soc": 100,
            "charge_rate_percent": 100,
        })
    );
    assert_eq!(
        apply["timed_charge"],
        serde_json::json!({ "enabled": true })
    );
}

/// Sunny forecast + Flux import/export tariffs + a battery that is full
/// long before the evening peak → the plan payload carries export
/// advice alongside the charge recommendation: sell the peak window
/// down to the same floor the charge planner holds. Which verdict the
/// endpoint returns depends on where "now" sits in the daily cycle —
/// the peak is only sellable while it is still ahead of the next
/// cheap-rate import window — so the test reads the clock once and
/// pins the matching branch (02:00–16:00 → export; otherwise the
/// window is in progress, passed, or beyond the cycle → no_export).
#[tokio::test]
async fn forecast_plan_endpoint_includes_export_advice() {
    use chrono::TimeZone;
    use chrono::Timelike;
    use givenergy_local::history::{ForecastValueRow, HistoryDb};
    use givenergy_local::inverter::model::InverterSnapshot;

    let config = IsolatedConfig::enter();
    let state = Arc::new(AppState::new());

    // Sunny forward forecast: 800 W/m² for the daylight hours — the
    // 9.5 kWh battery is full by mid-morning, leaving real surplus
    // above the floor by the 16:00 peak.
    let db = HistoryDb::open(&config.dir.join("history.db")).unwrap();
    let now = chrono::Local::now();
    let now_ts = now.timestamp();
    let hour_start = now_ts - now_ts.rem_euclid(3600);
    for h in 0..73i64 {
        let ts = hour_start + h * 3600;
        db.insert_forecast_values(&[ForecastValueRow {
            timestamp: ts,
            variable: "shortwave_radiation".to_string(),
            value: if h % 24 >= 6 && h % 24 <= 19 {
                800.0
            } else {
                0.0
            },
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

    // A healthy battery: full-ish with the hardware rate limits.
    *state.latest_snapshot.lock().await = Some(InverterSnapshot {
        soc: 90,
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

    // Flux-like import AND export tariffs via the existing settings
    // endpoint: 9p overnight import, 35p evening export peak.
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
            },
            "export_tariff_config": {
                "slots": [
                    { "start": "00:00", "end": "16:00", "rate": 0.05 },
                    { "start": "16:00", "end": "21:00", "rate": 0.35 },
                    { "start": "21:00", "end": "23:59", "rate": 0.05 }
                ]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "settings save failed: {body}");

    let (status, body) = get_json(&router, "/api/forecast/plan").await;
    assert_eq!(status, StatusCode::OK);

    // One clock read pins the expected branch; the planner reads the
    // same clock moments later inside the handler.
    let local_now = chrono::Local::now();
    let minute_of_day = local_now.hour() * 60 + local_now.minute();
    let export_in_cycle = (2 * 60..16 * 60).contains(&minute_of_day);
    let export = &body["data"]["export"];
    if export_in_cycle {
        assert_eq!(export["kind"], Value::String("export".to_string()));
        assert_eq!(export["window"]["start"], serde_json::json!("16:00"));
        let end = export["window"]["end"].as_str().expect("window end");
        let parts: Vec<u32> = end.split(':').map(|p| p.parse().expect("HH:MM")).collect();
        let end_min = parts[0] * 60 + parts[1];
        assert!(
            end_min > 16 * 60 && end_min <= 21 * 60,
            "export window end {end} must fall inside the 16:00–21:00 peak"
        );
        let kwh = export["kwh"].as_f64().expect("kwh");
        assert!(kwh > 0.0, "kwh = {kwh}");
        let rate = export["window"]["rate"].as_f64().expect("rate");
        assert!((rate - 0.35).abs() < 1e-9, "peak export rate, got {rate}");
        let earning = export["earning"].as_f64().expect("earning");
        assert!(
            (earning - kwh * rate).abs() < 1e-9,
            "earning {earning} must equal kwh × rate"
        );
        assert!(
            export["after_min_soc_pct"].as_f64().expect("after_min") >= 20.0,
            "the export must hold the 20% floor"
        );
        assert!(!export["with_export_series"]
            .as_array()
            .expect("series")
            .is_empty());
        // Read-only v1: no apply payload on the export advice.
        assert!(export.get("apply").is_none());
    } else {
        assert_eq!(export["kind"], Value::String("no_export".to_string()));
        assert!(
            !export["reason"].as_str().expect("reason").is_empty(),
            "a stood-down verdict still explains itself"
        );
    }
}
