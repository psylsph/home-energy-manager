//! Intelligent Octopus Go smart-charging dispatches (issue #394).
//!
//! On Intelligent Octopus Go, Octopus schedules the car's charging and can
//! start it outside the 23:30–05:30 window. The house load jumps, the battery
//! covers it, and the car ends up charging from the battery instead of the
//! cheap grid. With the hold on, HEM reads the account's planned dispatches
//! and pauses battery discharge while a smart one is running.
//!
//! Everything here fails soft. A failed or stale read means "no dispatch is
//! known", and the battery behaves as normal; HEM never guesses. Boost
//! (bump) charges are billed at the peak rate, so the battery covering one
//! is what the user wants and they are never held.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::inverter::poll::AppState;
use crate::octopus_sessions::{endpoints, obtain_token, post_graphql};
use crate::settings::Settings;

/// How often dispatches are read. Octopus allows about five dispatch queries
/// a minute; this stays far below that.
const POLL_INTERVAL: StdDuration = StdDuration::from_secs(3 * 60);
/// A dispatch list older than this is not trusted to hold the battery.
pub const STALE_AFTER_MINUTES: i64 = 15;
/// How often the account's car and charger list is re-read.
const DEVICE_REFRESH_HOURS: i64 = 6;
/// How long an access token is reused. Octopus tokens last an hour.
const TOKEN_REUSE_MINUTES: i64 = 50;

const DEVICES_QUERY: &str = "query($account: String!) { devices(accountNumber: $account) { \
    id deviceType status { current } } }";
const DISPATCHES_QUERY: &str =
    "query($device: String!) { flexPlannedDispatches(deviceId: $device) { \
    start end type energyAddedKwh } }";

/// One planned dispatch for the car or charger.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Dispatch {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// Octopus's dispatch type (`SMART`, `BOOST`), when given.
    pub kind: Option<String>,
    pub energy_added_kwh: Option<f64>,
}

impl Dispatch {
    /// Whether this is an Octopus-scheduled smart charge, billed at the
    /// off-peak rate. An unlabelled dispatch counts as smart, as it does for
    /// Octopus; a boost or anything unrecognised does not.
    pub fn is_smart(&self) -> bool {
        false
    }
}

/// What the last check found.
#[derive(Debug, Clone, Default)]
pub struct DispatchesState {
    /// When Octopus last answered successfully.
    pub checked_at: Option<DateTime<Utc>>,
    /// Why the latest check failed, if it did. The dispatches below are
    /// then the last good ones, trusted until they go stale.
    pub error: Option<String>,
    /// Smart-charging cars and chargers on the account.
    pub devices: Vec<String>,
    devices_checked_at: Option<DateTime<Utc>>,
    pub dispatches: Vec<Dispatch>,
    /// Reused between checks; never serialised or logged.
    token: Option<(String, DateTime<Utc>)>,
    /// Bumped when the hold is switched, so a check that was already asking
    /// Octopus cannot bring back a list the user has just turned off.
    generation: u64,
}

impl DispatchesState {
    /// Whether a smart dispatch is running now, by a list fresh enough to
    /// trust. Unknown means no.
    pub fn hold_now(&self, _now: DateTime<Utc>) -> bool {
        false
    }

    /// Forget everything, as when the hold is turned off.
    fn reset(&mut self) {
        *self = Self {
            generation: self.generation.wrapping_add(1),
            ..Self::default()
        };
    }
}

/// Smart-charging devices from the `devices` response: live cars and chargers.
pub fn parse_devices(_response: &Value) -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

/// Planned dispatches from the `flexPlannedDispatches` response. An error or
/// a malformed entry fails the whole read: a partial list could miss the
/// dispatch that is running.
pub fn parse_dispatches(_response: &Value) -> Result<Vec<Dispatch>, String> {
    Ok(Vec::new())
}

/// One pass: read the dispatches and publish them.
pub async fn check_once(_state: &Arc<AppState>, _now: DateTime<Utc>) {}

/// Background loop. Wakes early when the hold is switched.
pub async fn run_dispatches_loop(state: Arc<AppState>) {
    loop {
        check_once(&state, Utc::now()).await;
        tokio::select! {
            () = tokio::time::sleep(POLL_INTERVAL) => {}
            () = state.octopus_dispatches_wake.notified() => {}
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

/// GET /api/octopus/dispatches
pub async fn get_dispatches(State(_state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    (StatusCode::OK, Json(json!({ "ok": true })))
}

/// Unknown fields are refused so a misspelled one is not silently dropped.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldRequest {
    enabled: bool,
}

/// POST /api/octopus/dispatches/hold — body `{ "enabled": true }`.
pub async fn set_hold(
    State(_state): State<Arc<AppState>>,
    Json(_body): Json<HoldRequest>,
) -> (StatusCode, Json<Value>) {
    (StatusCode::OK, Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::with_isolated_config_dir_async;
    use axum::extract::Request;

    fn utc(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    const NOW: &str = "2026-10-17T12:00:00+00:00";
    const KEY: &str = "sk_live_test_SECRETKEY";
    const CAR: &str = "00000000-0000-0000-0000-00000000c4r0";

    fn devices_response() -> Value {
        json!({ "data": { "devices": [
            { "id": CAR, "deviceType": "ELECTRIC_VEHICLES", "status": { "current": "LIVE" } },
            { "id": "battery", "deviceType": "BATTERIES", "status": { "current": "LIVE" } },
            { "id": "old-car", "deviceType": "ELECTRIC_VEHICLES", "status": { "current": "RETIRED" } },
        ] } })
    }

    fn dispatch(start: &str, end: &str, kind: Value, energy: Value) -> Value {
        json!({ "start": start, "end": end, "type": kind, "energyAddedKwh": energy })
    }

    fn dispatches_response(items: Value) -> Value {
        json!({ "data": { "flexPlannedDispatches": items } })
    }

    fn smart(start: &str, end: &str) -> Dispatch {
        Dispatch {
            start: utc(start),
            end: utc(end),
            kind: Some("SMART".to_string()),
            energy_added_kwh: None,
        }
    }

    fn checked(at: &str, dispatches: Vec<Dispatch>) -> DispatchesState {
        DispatchesState {
            checked_at: Some(utc(at)),
            dispatches,
            ..Default::default()
        }
    }

    // ---- parsing ----

    #[test]
    fn devices_keep_live_cars_and_chargers_only() {
        assert_eq!(
            parse_devices(&devices_response()),
            Ok(vec![CAR.to_string()])
        );
        assert_eq!(
            parse_devices(&json!({ "data": { "devices": [] } })),
            Ok(Vec::new())
        );
    }

    #[test]
    fn device_errors_and_unexpected_shapes_are_errors() {
        let error = parse_devices(&json!({
            "errors": [{ "message": "Unauthorized", "extensions": { "errorCode": "KT-CT-1139" } }]
        }))
        .unwrap_err();
        assert!(error.contains("Unauthorized"), "{error}");
        assert!(error.contains("KT-CT-1139"), "{error}");
        assert!(parse_devices(&json!({ "data": { "devices": null } })).is_err());
        assert!(parse_devices(&json!({})).is_err());
    }

    #[test]
    fn dispatches_read_times_type_and_energy() {
        let parsed = parse_dispatches(&dispatches_response(json!([
            dispatch(
                "2026-10-17T13:00:00+00:00",
                "2026-10-17T13:30:00+00:00",
                json!("SMART"),
                json!(3.5)
            ),
            // Octopus has sent the energy as a decimal string.
            dispatch(
                "2026-10-17T14:00:00+01:00",
                "2026-10-17T14:30:00+01:00",
                json!("BOOST"),
                json!("1.25")
            ),
            dispatch(
                "2026-10-17T15:00:00+00:00",
                "2026-10-17T15:30:00+00:00",
                Value::Null,
                Value::Null
            ),
        ])))
        .unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].start, utc("2026-10-17T13:00:00+00:00"));
        assert_eq!(parsed[0].end, utc("2026-10-17T13:30:00+00:00"));
        assert_eq!(parsed[0].kind.as_deref(), Some("SMART"));
        assert_eq!(parsed[0].energy_added_kwh, Some(3.5));
        assert_eq!(
            parsed[1].start,
            utc("2026-10-17T13:00:00+00:00"),
            "offset honoured"
        );
        assert_eq!(parsed[1].energy_added_kwh, Some(1.25));
        assert_eq!(parsed[2].kind, None);
        assert_eq!(parsed[2].energy_added_kwh, None);
    }

    #[test]
    fn a_failed_dispatch_read_is_an_error_not_an_empty_list() {
        // Octopus's intermittent KT-CT-4340 failure.
        let error = parse_dispatches(&json!({
            "errors": [{
                "message": "Unable to fetch planned dispatches.",
                "extensions": { "errorCode": "KT-CT-4340" }
            }],
            "data": { "flexPlannedDispatches": null }
        }))
        .unwrap_err();
        assert!(error.contains("KT-CT-4340"), "{error}");
    }

    #[test]
    fn no_dispatches_is_empty_but_a_malformed_list_is_an_error() {
        assert_eq!(
            parse_dispatches(&dispatches_response(Value::Null)),
            Ok(Vec::new())
        );
        assert_eq!(
            parse_dispatches(&dispatches_response(json!([]))),
            Ok(Vec::new())
        );
        assert!(parse_dispatches(&json!({ "data": {} })).is_err());
        assert!(parse_dispatches(&dispatches_response(json!({}))).is_err());
        assert!(parse_dispatches(&dispatches_response(json!([dispatch(
            "not a time",
            "2026-10-17T13:30:00+00:00",
            json!("SMART"),
            Value::Null
        )])))
        .is_err());
        // An end before its start is not a usable window.
        assert!(parse_dispatches(&dispatches_response(json!([dispatch(
            "2026-10-17T14:00:00+00:00",
            "2026-10-17T13:30:00+00:00",
            json!("SMART"),
            Value::Null
        )])))
        .is_err());
    }

    #[test]
    fn smart_and_unlabelled_dispatches_are_held_boosts_are_not() {
        let mut d = smart("2026-10-17T13:00:00+00:00", "2026-10-17T13:30:00+00:00");
        assert!(d.is_smart());
        d.kind = Some("smart-charge".to_string());
        assert!(d.is_smart());
        d.kind = None;
        assert!(d.is_smart());
        d.kind = Some("BOOST".to_string());
        assert!(!d.is_smart());
        d.kind = Some("bump-charge".to_string());
        assert!(!d.is_smart());
        d.kind = Some("SOMETHING_NEW".to_string());
        assert!(!d.is_smart());
    }

    // ---- hold decision ----

    #[test]
    fn holds_only_inside_a_smart_dispatch() {
        let state = checked(
            "2026-10-17T12:58:00+00:00",
            vec![smart(
                "2026-10-17T13:00:00+00:00",
                "2026-10-17T13:30:00+00:00",
            )],
        );
        assert!(!state.hold_now(utc("2026-10-17T12:59:59+00:00")));
        assert!(state.hold_now(utc("2026-10-17T13:00:00+00:00")));
        assert!(state.hold_now(utc("2026-10-17T13:10:00+00:00")));
        assert!(
            !state.hold_now(utc("2026-10-17T13:30:00+00:00")),
            "end is exclusive"
        );
    }

    #[test]
    fn a_boost_is_never_held() {
        let mut boost = smart("2026-10-17T13:00:00+00:00", "2026-10-17T13:30:00+00:00");
        boost.kind = Some("BOOST".to_string());
        let state = checked("2026-10-17T13:00:00+00:00", vec![boost]);
        assert!(!state.hold_now(utc("2026-10-17T13:10:00+00:00")));
    }

    #[test]
    fn an_unknown_or_stale_list_holds_nothing() {
        let window = smart("2026-10-17T13:00:00+00:00", "2026-10-17T15:00:00+00:00");
        let never_checked = DispatchesState {
            dispatches: vec![window.clone()],
            ..Default::default()
        };
        assert!(!never_checked.hold_now(utc("2026-10-17T13:10:00+00:00")));

        let state = checked("2026-10-17T13:00:00+00:00", vec![window]);
        assert!(
            state.hold_now(utc("2026-10-17T13:15:00+00:00")),
            "15 min old is fresh"
        );
        assert!(
            !state.hold_now(utc("2026-10-17T13:15:01+00:00")),
            "older is stale"
        );
        // The clock stepped back: a check stamped well in the future is not
        // evidence about now.
        assert!(!state.hold_now(utc("2026-10-17T12:40:00+00:00")));
    }

    #[test]
    fn a_failed_check_keeps_trusting_the_last_good_list_until_it_is_stale() {
        let mut state = checked(
            "2026-10-17T13:00:00+00:00",
            vec![smart(
                "2026-10-17T13:00:00+00:00",
                "2026-10-17T14:00:00+00:00",
            )],
        );
        state.error = Some("Octopus returned an error: KT-CT-4340".to_string());
        assert!(state.hold_now(utc("2026-10-17T13:06:00+00:00")));
        assert!(!state.hold_now(utc("2026-10-17T13:20:00+00:00")));
    }

    // ---- check loop against a mock Octopus ----

    #[derive(Clone, Debug)]
    struct Seen {
        auth: Option<String>,
        body: String,
    }

    struct MockOctopus {
        base: String,
        seen: Arc<std::sync::Mutex<Vec<Seen>>>,
        devices_body: Arc<std::sync::Mutex<String>>,
        dispatches_body: Arc<std::sync::Mutex<String>>,
        /// Milliseconds the dispatch query waits before answering.
        dispatches_delay_ms: Arc<std::sync::atomic::AtomicU64>,
        _shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl MockOctopus {
        async fn spawn(dispatches: Value) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            let devices_body = Arc::new(std::sync::Mutex::new(devices_response().to_string()));
            let dispatches_body = Arc::new(std::sync::Mutex::new(dispatches.to_string()));
            let dispatches_delay_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let (s2, v2, b2, d2) = (
                seen.clone(),
                devices_body.clone(),
                dispatches_body.clone(),
                dispatches_delay_ms.clone(),
            );
            let app = axum::Router::new().fallback(move |req: Request| {
                let (s2, v2, b2, d2) = (s2.clone(), v2.clone(), b2.clone(), d2.clone());
                async move {
                    let auth = req
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    let bytes = axum::body::to_bytes(req.into_body(), 1 << 20)
                        .await
                        .unwrap();
                    let body = String::from_utf8_lossy(&bytes).to_string();
                    s2.lock().unwrap().push(Seen {
                        auth,
                        body: body.clone(),
                    });
                    if body.contains("obtainKrakenToken") {
                        let token =
                            json!({ "data": { "obtainKrakenToken": { "token": "jwt-abc" } } });
                        (StatusCode::OK, token.to_string())
                    } else if body.contains("flexPlannedDispatches") {
                        let delay = d2.load(std::sync::atomic::Ordering::Relaxed);
                        if delay > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        }
                        (StatusCode::OK, b2.lock().unwrap().clone())
                    } else if body.contains("devices") {
                        (StatusCode::OK, v2.lock().unwrap().clone())
                    } else {
                        (StatusCode::NOT_FOUND, "no route".to_string())
                    }
                }
            });
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = rx.await;
                    })
                    .await;
            });
            Self {
                base: format!("http://{addr}"),
                seen,
                devices_body,
                dispatches_body,
                dispatches_delay_ms,
                _shutdown: Some(tx),
            }
        }

        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }

        fn count(&self, needle: &str) -> usize {
            self.seen()
                .iter()
                .filter(|seen| seen.body.contains(needle))
                .count()
        }

        fn set_dispatches(&self, body: Value) {
            *self.dispatches_body.lock().unwrap() = body.to_string();
        }
    }

    impl Drop for MockOctopus {
        fn drop(&mut self) {
            if let Some(tx) = self._shutdown.take() {
                let _ = tx.send(());
            }
        }
    }

    fn one_dispatch() -> Value {
        dispatches_response(json!([dispatch(
            "2026-10-17T12:00:00+00:00",
            "2026-10-17T13:00:00+00:00",
            json!("SMART"),
            json!(4.2)
        )]))
    }

    async fn configure(mock: &MockOctopus, hold: bool) -> Arc<AppState> {
        Settings::update(|s| {
            s.octopus_api_key = KEY.to_string();
            s.octopus_account_number = "A-TEST1".to_string();
            s.octopus_api_base_url = mock.base.clone();
            s.octopus_dispatch_hold = hold;
        })
        .unwrap();
        Arc::new(AppState::new())
    }

    #[tokio::test]
    async fn a_check_reads_the_devices_then_their_dispatches() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            check_once(&state, utc(NOW)).await;

            let seen = mock.seen();
            assert_eq!(seen.len(), 3, "token, devices, one car's dispatches");
            assert!(seen[0].body.contains("obtainKrakenToken"));
            assert!(seen[0].body.contains(KEY));
            assert_eq!(seen[0].auth, None);
            assert!(seen[1].body.contains("A-TEST1"));
            assert_eq!(seen[1].auth.as_deref(), Some("jwt-abc"));
            assert!(seen[2].body.contains(CAR));
            assert!(!seen[2].body.contains("old-car"));
            assert!(!seen[1].body.contains(KEY) && !seen[2].body.contains(KEY));

            let current = state.octopus_dispatches.lock().await.clone();
            assert_eq!(current.checked_at, Some(utc(NOW)));
            assert_eq!(current.error, None);
            assert_eq!(current.devices, vec![CAR.to_string()]);
            assert_eq!(current.dispatches.len(), 1);
            assert!(current.hold_now(utc("2026-10-17T12:10:00+00:00")));
        })
        .await;
    }

    #[tokio::test]
    async fn later_checks_reuse_the_token_and_the_device_list() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            check_once(&state, utc(NOW)).await;
            check_once(&state, utc("2026-10-17T12:03:00+00:00")).await;
            assert_eq!(mock.count("obtainKrakenToken"), 1);
            assert_eq!(mock.count("devices("), 1);
            assert_eq!(mock.count("flexPlannedDispatches"), 2);

            // An hour on the token is renewed; six hours on, so is the list.
            check_once(&state, utc("2026-10-17T13:00:00+00:00")).await;
            assert_eq!(mock.count("obtainKrakenToken"), 2);
            assert_eq!(mock.count("devices("), 1);
            check_once(&state, utc("2026-10-17T18:00:00+00:00")).await;
            assert_eq!(mock.count("devices("), 2);
        })
        .await;
    }

    #[tokio::test]
    async fn a_failed_check_reports_the_error_and_keeps_the_last_good_list() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            check_once(&state, utc(NOW)).await;

            mock.set_dispatches(json!({
                "errors": [{ "message": "Unable to fetch planned dispatches.",
                             "extensions": { "errorCode": "KT-CT-4340" } }],
                "data": { "flexPlannedDispatches": null }
            }));
            check_once(&state, utc("2026-10-17T12:03:00+00:00")).await;
            let current = state.octopus_dispatches.lock().await.clone();
            assert!(current.error.as_deref().unwrap().contains("KT-CT-4340"));
            assert_eq!(current.checked_at, Some(utc(NOW)), "last success kept");
            assert_eq!(current.dispatches.len(), 1);
            assert!(current.hold_now(utc("2026-10-17T12:05:00+00:00")));
            assert!(!current.hold_now(utc("2026-10-17T12:16:00+00:00")));

            // A failure drops the cached token: it may be what went wrong.
            check_once(&state, utc("2026-10-17T12:06:00+00:00")).await;
            assert_eq!(mock.count("obtainKrakenToken"), 2);

            mock.set_dispatches(one_dispatch());
            check_once(&state, utc("2026-10-17T12:09:00+00:00")).await;
            assert_eq!(state.octopus_dispatches.lock().await.error, None);
        })
        .await;
    }

    #[tokio::test]
    async fn an_unreachable_octopus_is_reported_without_the_key() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            Settings::update(|s| s.octopus_api_base_url = "http://127.0.0.1:1".to_string())
                .unwrap();
            check_once(&state, utc(NOW)).await;
            let current = state.octopus_dispatches.lock().await.clone();
            let error = current.error.clone().unwrap();
            assert!(error.contains("request failed"), "{error}");
            assert!(!error.contains(KEY));
            assert!(!current.hold_now(utc(NOW)));
        })
        .await;
    }

    #[tokio::test]
    async fn an_account_without_a_smart_charging_car_holds_nothing() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            *mock.devices_body.lock().unwrap() = json!({ "data": { "devices": [] } }).to_string();
            let state = configure(&mock, true).await;
            check_once(&state, utc(NOW)).await;
            assert_eq!(mock.count("flexPlannedDispatches"), 0);
            let current = state.octopus_dispatches.lock().await.clone();
            assert_eq!(current.checked_at, Some(utc(NOW)));
            assert!(current.devices.is_empty());
            assert!(current.dispatches.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn the_hold_off_makes_no_requests_and_forgets_the_list() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            check_once(&state, utc(NOW)).await;
            let before = mock.seen().len();

            Settings::update(|s| s.octopus_dispatch_hold = false).unwrap();
            check_once(&state, utc("2026-10-17T12:03:00+00:00")).await;
            assert_eq!(mock.seen().len(), before, "no further requests");
            let current = state.octopus_dispatches.lock().await.clone();
            assert!(current.dispatches.is_empty());
            assert_eq!(current.checked_at, None);
        })
        .await;
    }

    #[tokio::test]
    async fn turning_the_hold_off_during_a_slow_check_discards_its_answer() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            mock.dispatches_delay_ms
                .store(400, std::sync::atomic::Ordering::Relaxed);
            let state = configure(&mock, true).await;

            let checking = tokio::spawn({
                let state = state.clone();
                async move { check_once(&state, utc(NOW)).await }
            });
            // Wait until the check is waiting on the dispatch query.
            for _ in 0..200 {
                if mock.count("flexPlannedDispatches") > 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            assert_eq!(mock.count("flexPlannedDispatches"), 1);
            let (status, _) =
                set_hold(State(state.clone()), Json(HoldRequest { enabled: false })).await;
            assert_eq!(status, StatusCode::OK);
            checking.await.unwrap();

            let current = state.octopus_dispatches.lock().await.clone();
            assert!(current.dispatches.is_empty(), "the late answer is dropped");
            assert!(!current.hold_now(utc(NOW)));
        })
        .await;
    }

    #[tokio::test]
    async fn the_handlers_report_the_list_and_switch_the_hold() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, false).await;

            let (status, Json(body)) = get_dispatches(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["ok"], true);
            assert_eq!(body["enabled"], false);
            assert_eq!(body["configured"], true);
            assert_eq!(body["holding"], false);
            assert!(body["dispatches"].as_array().unwrap().is_empty());

            let (status, Json(reply)) =
                set_hold(State(state.clone()), Json(HoldRequest { enabled: true })).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(reply["enabled"], true);
            assert!(Settings::load().octopus_dispatch_hold);
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                state.octopus_dispatches_wake.notified(),
            )
            .await
            .expect("the loop is woken so the change applies straight away");

            mock.set_dispatches(dispatches_response(json!([
                // Already over: not shown.
                dispatch(
                    "2000-01-01T00:00:00+00:00",
                    "2000-01-01T01:00:00+00:00",
                    json!("SMART"),
                    Value::Null
                ),
                dispatch(
                    "2099-01-01T01:00:00+00:00",
                    "2099-01-01T02:00:00+00:00",
                    json!("BOOST"),
                    json!(2.0)
                ),
                dispatch(
                    "2099-01-01T00:00:00+00:00",
                    "2099-01-01T00:30:00+00:00",
                    json!("SMART"),
                    json!(1.5)
                ),
            ])));
            check_once(&state, Utc::now()).await;
            *state.dispatch_hold_state.lock().await =
                crate::inverter::poll::DispatchHoldState::Holding;
            let (_, Json(body)) = get_dispatches(State(state.clone())).await;
            assert_eq!(body["enabled"], true);
            assert_eq!(body["devices"], 1);
            assert_eq!(body["holding"], true);
            assert!(body["checked_at"].is_string());
            assert_eq!(body["error"], Value::Null);
            let listed = body["dispatches"].as_array().unwrap();
            assert_eq!(listed.len(), 2);
            assert_eq!(
                listed[0]["start"], "2099-01-01T00:00:00Z",
                "sorted by start"
            );
            assert_eq!(listed[0]["smart"], true);
            assert_eq!(listed[0]["energy_added_kwh"], 1.5);
            assert_eq!(listed[1]["type"], "BOOST");
            assert_eq!(listed[1]["smart"], false);

            let (status, _) =
                set_hold(State(state.clone()), Json(HoldRequest { enabled: false })).await;
            assert_eq!(status, StatusCode::OK);
            assert!(!Settings::load().octopus_dispatch_hold);
            let (_, Json(body)) = get_dispatches(State(state.clone())).await;
            assert!(body["dispatches"].as_array().unwrap().is_empty());
        })
        .await;
    }

    #[test]
    fn the_hold_request_refuses_unknown_fields() {
        assert!(serde_json::from_value::<HoldRequest>(json!({ "enabled": true })).is_ok());
        assert!(serde_json::from_value::<HoldRequest>(json!({ "enable": true })).is_err());
    }
}
