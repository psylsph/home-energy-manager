//! Mini display endpoint — a tokenless, read-only, minimal-field status
//! summary for tiny text-only displays (primarily an Apple Watch via an
//! iPhone Shortcut; see INSTALL.md → "Glance from your Apple Watch").
//!
//! ## Why a separate endpoint
//!
//! `GET /api/snapshot` returns the *full* [`InverterSnapshot`] — serial
//! numbers, firmware versions, per-cell module voltages, meter details.
//! That's right for the dashboard, but far more than a wrist-glance needs,
//! and this endpoint is deliberately **unauthenticated** (Apple Watch
//! Shortcuts cannot send `Authorization` headers).
//!
//! Two guarantees make the missing auth acceptable:
//!
//! 1. **Read-only** — only a `GET` handler; no control / settings surface.
//! 2. **Minimal hand-picked subset** — [`build_mini_response`] assembles the
//!    payload field-by-field from the allowlist below. It must NEVER delegate
//!    to `get_snapshot` or forward the full snapshot, or "read-only due to no
//!    auth" stops holding at the data level (not just the HTTP-verb level).
//!    The `build_never_leaks_sensitive_snapshot_fields` test pins the exact
//!    key set so a future "helpfully add a field" refactor can't silently
//!    widen the exposure.

use std::sync::Arc;

use axum::extract::State;
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Json, Response};

use chrono::Local;
use serde::Serialize;

use crate::inverter::model::InverterSnapshot;
use crate::inverter::poll::{AppState, ConnectionState};

/// Round a watt reading to a one-decimal-place kilowatt value.
///
/// A watch face shows at most ~3–4 significant digits at its DPI, so `4.2`
/// is all the resolution a glance needs; it also keeps the payload tiny and
/// the Shortcut's dictionary parsing trivial.
fn kw(watts: i32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let v = watts as f32 / 1000.0;
    (v * 10.0).round() / 10.0
}

/// Flat, always-present glance summary.
///
/// Every field is populated (defaults applied when there is no snapshot) so
/// the consuming Shortcut can build its text line without nil-checking each
/// key. Sign convention matches the rest of HEM:
/// - `battery_kw` > 0 = discharging, < 0 = charging
/// - `grid_kw`    > 0 = importing,    < 0 = exporting
#[derive(Debug, Serialize)]
struct MiniStatusResponse {
    ok: bool,
    /// Snapshot timestamp (epoch seconds). `0` when no snapshot exists.
    ts: i64,
    /// `now - ts`, clamped at 0, so the watch can flag a stale reading
    /// without its own clock needing to be in sync with the server.
    age_s: i64,
    /// `connected` | `reconnecting` | `disconnected`.
    conn: ConnectionState,
    /// Inverter model display name (e.g. "Gen3 Hybrid"); empty if unknown.
    device: String,
    solar_kw: f32,
    battery_kw: f32,
    grid_kw: f32,
    home_kw: f32,
    /// State of charge, 0–100.
    soc: u8,
    /// `idle` | `charging` | `discharging`.
    battery_state: crate::inverter::model::BatteryState,
    /// `BatteryMode` snake_case repr (`eco`, `timed_demand`, …, `unknown`).
    battery_mode: crate::inverter::model::BatteryMode,
    /// `true` when any of grid_loss / inverter_trip / battery_over_temp is set.
    fault: bool,
}

/// Build the glance summary from a (possibly absent) snapshot and the live
/// connection state. Pure: no locking, no I/O — extracted so the field
/// selection, rounding, and fault derivation are unit-testable directly.
fn build_mini_response(
    snapshot: Option<&InverterSnapshot>,
    conn: ConnectionState,
    now_secs: i64,
) -> MiniStatusResponse {
    match snapshot {
        Some(s) => MiniStatusResponse {
            ok: true,
            ts: s.timestamp,
            age_s: (now_secs - s.timestamp).max(0),
            conn,
            device: s.device_type_display.clone(),
            solar_kw: kw(s.solar_power),
            battery_kw: kw(s.battery_power),
            grid_kw: kw(s.grid_power),
            home_kw: kw(s.home_power),
            soc: s.soc,
            battery_state: s.battery_state,
            battery_mode: s.battery_mode,
            fault: s.grid_loss || s.inverter_trip || s.battery_over_temp,
        },
        None => MiniStatusResponse {
            ok: false,
            ts: 0,
            age_s: 0,
            conn,
            device: String::new(),
            solar_kw: 0.0,
            battery_kw: 0.0,
            grid_kw: 0.0,
            home_kw: 0.0,
            soc: 0,
            battery_state: crate::inverter::model::BatteryState::Idle,
            battery_mode: crate::inverter::model::BatteryMode::Unknown,
            fault: false,
        },
    }
}

/// `GET /api/mini/status` — tokenless, read-only glance summary.
///
/// Locks `latest_snapshot` and `connection_state` once each, then delegates
/// to [`build_mini_response`]. Marked `Cache-Control: no-store` so a fresh
/// Shortcut tap always sees fresh data (the iPhone Shortcut itself doesn't
/// cache across runs, but the directive also covers any browser / proxy use
/// of the same URL).
pub async fn mini_status(State(state): State<Arc<AppState>>) -> Response {
    let now_secs = Local::now().timestamp();
    let conn = state.connection_state.lock().await.clone();
    let snapshot = state.latest_snapshot.lock().await;
    let resp = build_mini_response(snapshot.as_ref(), conn, now_secs);
    drop(snapshot);

    let mut response = (StatusCode::OK, Json(resp)).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `GET /mini` — tiny self-contained GUI that fetches `/api/mini/status`
/// and renders a glance display sized for a phone or Apple Watch screen.
///
/// The HTML, CSS, and render script are all inline (one file, no external
/// assets) so it loads fast in a short-lived WKWebView and needs no DNS /
/// CDN access. At home on Wi-Fi the watch's browser can open this URL
/// directly; away from home the iPhone can reach it over Tailscale (see
/// INSTALL.md).
pub async fn mini_page() -> Html<&'static str> {
    Html(include_str!("mini.html"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::{BatteryMode, BatteryState, InverterSnapshot};
    use serde_json::Value;

    /// Snapshot with every field the mini response reads set to known
    /// values, plus sensitive fields populated to prove they do NOT leak.
    fn sample_snapshot() -> InverterSnapshot {
        InverterSnapshot {
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
            // Sensitive fields — must NOT appear in the mini payload.
            inverter_serial: String::from("SAUNDERS-SECRET-123"),
            firmware_version: String::from("SPAaaaaaaaaa"),
            device_type_code: String::from("2201"),
            ..Default::default()
        }
    }

    #[test]
    fn build_rounds_power_to_one_decimal_and_keeps_signs() {
        let snap = sample_snapshot();
        let resp = build_mini_response(Some(&snap), ConnectionState::Connected, 1_700_000_004);
        assert!(resp.ok);
        assert!((resp.solar_kw - 4.2).abs() < 1e-6);
        // Charging sign (negative) is preserved through the kW rounding.
        assert!((resp.battery_kw - (-1.8)).abs() < 1e-6);
        assert!((resp.grid_kw - 0.9).abs() < 1e-6);
        assert!((resp.home_kw - 1.5).abs() < 1e-6);
        assert_eq!(resp.soc, 64);
        assert_eq!(resp.age_s, 4);
        assert_eq!(resp.device, "Gen3 Hybrid");
    }

    #[test]
    fn build_serialises_conn_state_and_mode_as_snake_case() {
        let snap = sample_snapshot();
        let resp = build_mini_response(Some(&snap), ConnectionState::Connected, snap.timestamp);
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["conn"], Value::String("connected".into()));
        assert_eq!(v["battery_state"], Value::String("discharging".into()));
        assert_eq!(v["battery_mode"], Value::String("eco".into()));
    }

    #[test]
    fn build_fault_is_true_when_any_fault_flag_is_set() {
        let snap = sample_snapshot();
        assert!(!build_mini_response(Some(&snap), ConnectionState::Connected, 0).fault);

        let mut snap = sample_snapshot();
        snap.grid_loss = true;
        assert!(build_mini_response(Some(&snap), ConnectionState::Connected, 0).fault);

        let mut snap = sample_snapshot();
        snap.inverter_trip = true;
        assert!(build_mini_response(Some(&snap), ConnectionState::Connected, 0).fault);

        let mut snap = sample_snapshot();
        snap.battery_over_temp = true;
        assert!(build_mini_response(Some(&snap), ConnectionState::Connected, 0).fault);
    }

    #[test]
    fn build_with_no_snapshot_returns_defaults_and_carries_conn() {
        let resp = build_mini_response(None, ConnectionState::Reconnecting, 1_700_000_000);
        assert!(!resp.ok);
        assert_eq!(resp.ts, 0);
        assert_eq!(resp.age_s, 0);
        assert_eq!(resp.device, "");
        assert_eq!(resp.solar_kw, 0.0);
        assert_eq!(resp.soc, 0);
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["conn"], Value::String("reconnecting".into()));
        assert_eq!(v["battery_state"], Value::String("idle".into()));
        assert_eq!(v["battery_mode"], Value::String("unknown".into()));
    }

    #[test]
    fn build_never_leaks_sensitive_snapshot_fields() {
        // The mini endpoint is unauthenticated, so the response must contain
        // ONLY the hand-picked glance fields — never serial numbers,
        // firmware, device-type codes, per-module data, etc. This is the
        // data-level half of "read-only due to lack of auth": even though
        // the snapshot carries all of these, only the allowlist is emitted.
        let snap = sample_snapshot();
        let resp = build_mini_response(Some(&snap), ConnectionState::Connected, snap.timestamp);
        let v = serde_json::to_value(&resp).unwrap();
        let mut got: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        got.sort_unstable();
        let mut want = [
            "age_s",
            "battery_kw",
            "battery_mode",
            "battery_state",
            "conn",
            "device",
            "fault",
            "grid_kw",
            "home_kw",
            "ok",
            "soc",
            "solar_kw",
            "ts",
        ];
        want.sort_unstable();
        assert_eq!(
            got, want,
            "mini response key set must be exactly the allowlist"
        );

        // And the sensitive values really are absent (belt-and-braces: the
        // key-set check above already proves this, but pin the names so a
        // future regression message points straight at what leaked).
        assert!(v.get("inverter_serial").is_none());
        assert!(v.get("firmware_version").is_none());
        assert!(v.get("device_type_code").is_none());
        assert!(v.get("battery_modules").is_none());
        assert!(v.get("meters").is_none());
    }
}
