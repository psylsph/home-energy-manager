//! Least-data projection of the inverter snapshot for the authenticated
//! external API (U4).
//!
//! The main dashboard serializes the complete [`InverterSnapshot`] — that is
//! a deliberate compatibility surface for the local UI. External integrations
//! have no need for serials, firmware details, per-module battery telemetry,
//! or plant identifiers, so the authenticated route returns an explicit
//! allow-list instead. Fields are added here deliberately; the regression
//! test fails if a future internal field becomes exposed by accident.
//!
//! The projection is safe-to-publish operating data: live power flows, the
//! battery's state of charge and temperature, energy counters, and the
//! configuration values the status endpoint already documents (`limits`).

use crate::inverter::model::InverterSnapshot;
use crate::inverter::poll::AppState;
use axum::{
    extract::State,
    http::{header::CACHE_CONTROL, HeaderValue},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::sync::Arc;

/// Build the external snapshot payload. Wraps the allow-listed operating
/// data with freshness metadata (`observed_at` / `age_seconds`) so callers
/// can reject stale data without correlating another endpoint.
pub fn external_snapshot(snapshot: &InverterSnapshot, now_secs: i64) -> Value {
    json!({
        "solar_power": snapshot.solar_power,
        "battery_power": snapshot.battery_power,
        "grid_power": snapshot.grid_power,
        "home_power": snapshot.home_power,
        "soc": snapshot.soc,
        "battery_temperature": snapshot.battery_temperature,
        "battery_state": snapshot.battery_state,
        "inverter_temperature": snapshot.inverter_temperature,
        "grid_voltage": snapshot.grid_voltage,
        "grid_frequency": snapshot.grid_frequency,
        "grid_online": snapshot.grid_online,
        "battery_reserve": snapshot.battery_reserve,
        "target_soc": snapshot.target_soc,
        "today_solar_kwh": snapshot.today_solar_kwh,
        "today_import_kwh": snapshot.today_import_kwh,
        "today_export_kwh": snapshot.today_export_kwh,
        "today_charge_kwh": snapshot.today_charge_kwh,
        "today_discharge_kwh": snapshot.today_discharge_kwh,
        "today_consumption_kwh": snapshot.today_consumption_kwh,
        "observed_at": chrono::DateTime::<chrono::Utc>::from_timestamp(snapshot.timestamp, 0)
            .map(|t| t.to_rfc3339()),
        "age_seconds": now_secs.saturating_sub(snapshot.timestamp).max(0),
    })
}

/// GET /api/snapshot on the authenticated router: the allow-listed
/// projection with `Cache-Control: no-store`, so intermediaries never cache
/// household data.
pub async fn get_snapshot(State(state): State<Arc<AppState>>) -> Response {
    let now_secs = chrono::Utc::now().timestamp();
    let payload = {
        let snapshot = state.latest_snapshot.lock().await;
        match snapshot.as_ref() {
            Some(snapshot) => {
                let mut payload = external_snapshot(snapshot, now_secs);
                payload["ok"] = json!(true);
                payload
            }
            None => json!({
                "ok": false,
                "error": "No inverter data available yet",
                "observed_at": Value::Null,
                "age_seconds": Value::Null,
            }),
        }
    };
    let mut response = Json(payload).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::{BatteryModule, BatteryState, DeviceType};

    fn fixture() -> InverterSnapshot {
        InverterSnapshot {
            timestamp: 1_800_000_000,
            device_type: DeviceType::Gen3Hybrid,
            solar_power: 2100,
            battery_power: -1500,
            grid_power: 300,
            home_power: 900,
            soc: 64,
            battery_temperature: 21.5,
            battery_state: BatteryState::Charging,
            inverter_temperature: 33.0,
            grid_voltage: 240.0,
            grid_frequency: 50.0,
            grid_online: true,
            battery_reserve: 4,
            target_soc: 100,
            today_solar_kwh: 12.5,
            today_import_kwh: 3.25,
            today_export_kwh: 1.75,
            today_charge_kwh: 4.5,
            today_discharge_kwh: 2.0,
            today_consumption_kwh: 8.1,
            ..Default::default()
        }
    }

    #[test]
    fn exposes_documented_operating_fields() {
        let payload = external_snapshot(&fixture(), 1_800_000_004);
        assert_eq!(payload["soc"], 64);
        assert_eq!(payload["battery_power"], -1500);
        assert_eq!(payload["solar_power"], 2100);
        assert_eq!(payload["grid_power"], 300);
        assert_eq!(payload["home_power"], 900);
        assert_eq!(payload["battery_temperature"], 21.5);
        assert_eq!(payload["battery_state"], "charging");
        assert_eq!(payload["battery_reserve"], 4);
        assert_eq!(payload["target_soc"], 100);
        assert_eq!(payload["today_solar_kwh"], 12.5);
        assert_eq!(payload["age_seconds"], 4);
        assert!(payload["observed_at"].is_string());
    }

    /// The regression guard: sensitive internal fields must never appear,
    /// whatever the internal snapshot carries.
    #[test]
    fn never_exposes_internal_identifiers_or_module_detail() {
        let mut snapshot = fixture();
        snapshot.battery_modules = vec![BatteryModule {
            index: 1,
            soc: 60,
            temperature: 20.0,
            voltage: 51.2,
            current: 1.0,
            serial: "BG-1234".to_string(),
            num_cycles: 12,
            ..Default::default()
        }];
        let payload = external_snapshot(&snapshot, 1_800_000_000).to_string();
        for forbidden in [
            // Identifiers / firmware / plant metadata.
            "serial",
            "firmware",
            "fw_version",
            "arm_fw",
            "dsp_fw",
            "model",
            // Per-module battery detail.
            "bms_warnings",
            "battery_modules",
            "cell_",
            "cmu_",
            // Plant topology / PV string electricals.
            "pv1_voltage",
            "pv1_current",
            "pv2_voltage",
            "pv2_current",
            // Internal bookkeeping.
            "device_type",
            "inverter_time",
            "total_throughput",
        ] {
            assert!(
                !payload.contains(forbidden),
                "leaked {forbidden:?}: {payload}"
            );
        }
    }
}
