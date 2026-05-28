//! REST API routes and handlers.

use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;
use serde_json::{json, Value};

use crate::inverter::encoder::ControlCommand;
use crate::inverter::poll::{AppState, PollSettings};
use crate::modbus::registers::encode_hhmm;

// ---------------------------------------------------------------------------
// Helper: standard JSON response
// ---------------------------------------------------------------------------

fn ok_response(message: &str) -> Json<Value> {
    Json(json!({ "ok": true, "message": message }))
}

fn error_response(error: &str) -> Json<Value> {
    Json(json!({ "ok": false, "error": error }))
}

// ---------------------------------------------------------------------------
// Data endpoints
// ---------------------------------------------------------------------------

/// GET /api/snapshot
pub async fn get_snapshot(State(state): State<Arc<AppState>>) -> Json<Value> {
    let snapshot = state.latest_snapshot.lock().await;
    match snapshot.as_ref() {
        Some(snap) => Json(json!({ "ok": true, "data": snap })),
        None => Json(json!({ "ok": false, "error": "No snapshot available yet" })),
    }
}

/// GET /api/settings
pub async fn get_settings(State(state): State<Arc<AppState>>) -> Json<Value> {
    let settings = state.settings.lock().await;
    Json(json!({
        "ok": true,
        "data": {
            "host": settings.host,
            "port": settings.port,
            "serial": settings.serial,
            "interval_secs": settings.interval_secs,
        }
    }))
}

/// POST /api/settings
pub async fn update_settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<Value> {
    let new_settings = match parse_settings(&body) {
        Ok(s) => s,
        Err(e) => return error_response(&e),
    };

    let mut settings = state.settings.lock().await;
    *settings = new_settings;

    // Persist to disk
    let persist = crate::settings::Settings {
        host: settings.host.clone(),
        port: settings.port,
        serial: settings.serial.clone(),
        poll_interval: settings.interval_secs,
        auto_connect: true,
    };
    if let Err(e) = persist.save() {
        tracing::warn!("Failed to persist settings: {}", e);
    }

    let msg = format!(
        "Settings updated: host={}, port={}, interval={}s",
        settings.host, settings.port, settings.interval_secs
    );
    tracing::info!("{}", msg);
    ok_response(&msg)
}

fn parse_settings(body: &serde_json::Value) -> Result<PollSettings, String> {
    let host = body["host"].as_str().unwrap_or("").to_string();
    let port = body["port"].as_u64().unwrap_or(8899) as u16;
    let serial = body["serial"].as_str().unwrap_or("").to_string();
    let interval_secs = body["interval_secs"].as_u64().unwrap_or(10);

    if !host.is_empty() && port == 0 {
        return Err("Invalid port".to_string());
    }
    if interval_secs == 0 {
        return Err("interval_secs must be > 0".to_string());
    }

    Ok(PollSettings {
        host,
        port,
        serial,
        interval_secs,
    })
}

// ---------------------------------------------------------------------------
// Control endpoints
// ---------------------------------------------------------------------------

/// POST /api/control/mode — set battery operating mode.
///
/// Body: `{"mode": "eco"}` or `{"mode": "timed_export"}`, etc.
/// Optionally include `soc_reserve` (defaults to 4).
pub async fn set_mode(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<Value> {
    let mode_str = match body["mode"].as_str() {
        Some(m) => m,
        None => return error_response("Missing 'mode' field"),
    };
    let soc_reserve = body["soc_reserve"].as_u64().unwrap_or(4) as u16;

    let cmd = match mode_str {
        "eco" => ControlCommand::SetEcoMode { soc_reserve },
        "eco_paused" => ControlCommand::PauseBattery,
        "timed_demand" => ControlCommand::SetTimedDemandMode { soc_reserve },
        "timed_export" => ControlCommand::SetTimedExportMode { soc_reserve },
        "export_paused" => ControlCommand::SetBatteryPowerMode { mode: 0 },
        _ => return error_response(&format!("Unknown mode: '{}'", mode_str)),
    };

    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("Mode command encoded: {:?}", writes);
            ok_response(&format!("Mode set to {}", mode_str))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/charge-slot — configure a charge schedule slot.
///
/// Body: `{"slot": 1, "start_hour": 6, "start_minute": 0, "end_hour": 10, "end_minute": 0}`
pub async fn set_charge_slot(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<Value> {
    let slot: u8 = match body["slot"].as_u64() {
        Some(s) => s as u8,
        None => return error_response("Missing 'slot' field (1-2)"),
    };

    let start_hour = body["start_hour"].as_u64().unwrap_or(0) as u8;
    let start_minute = body["start_minute"].as_u64().unwrap_or(0) as u8;
    let end_hour = body["end_hour"].as_u64().unwrap_or(0) as u8;
    let end_minute = body["end_minute"].as_u64().unwrap_or(0) as u8;

    let start = encode_hhmm(start_hour, start_minute);
    let end = encode_hhmm(end_hour, end_minute);

    let cmd = match slot {
        1 => ControlCommand::SetChargeSlot1 { start, end },
        2 => ControlCommand::SetChargeSlot2 { start, end },
        _ => return error_response("Slot must be 1 or 2"),
    };

    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("SetChargeSlot {} encoded: {:?}", slot, writes);
            ok_response(&format!("Charge slot {} configured", slot))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/discharge-slot — configure a discharge schedule slot.
pub async fn set_discharge_slot(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<Value> {
    let slot: u8 = match body["slot"].as_u64() {
        Some(s) => s as u8,
        None => return error_response("Missing 'slot' field (1-2)"),
    };

    let start_hour = body["start_hour"].as_u64().unwrap_or(0) as u8;
    let start_minute = body["start_minute"].as_u64().unwrap_or(0) as u8;
    let end_hour = body["end_hour"].as_u64().unwrap_or(0) as u8;
    let end_minute = body["end_minute"].as_u64().unwrap_or(0) as u8;

    let start = encode_hhmm(start_hour, start_minute);
    let end = encode_hhmm(end_hour, end_minute);

    let cmd = match slot {
        1 => ControlCommand::SetDischargeSlot1 { start, end },
        2 => ControlCommand::SetDischargeSlot2 { start, end },
        _ => return error_response("Slot must be 1 or 2"),
    };

    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("SetDischargeSlot {} encoded: {:?}", slot, writes);
            ok_response(&format!("Discharge slot {} configured", slot))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/reserve — set battery reserve SoC percentage.
pub async fn set_reserve(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<Value> {
    let soc: u16 = match body["soc"].as_u64() {
        Some(s) => s as u16,
        None => return error_response("Missing 'soc' field (0-100)"),
    };

    let cmd = ControlCommand::SetBatterySocReserve { reserve: soc };
    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("SetReserve encoded: {:?}", writes);
            ok_response(&format!("Battery reserve set to {}%", soc))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/charge-rate — set battery charge limit percentage.
pub async fn set_charge_rate(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<Value> {
    let limit: u16 = match body["limit"].as_u64() {
        Some(r) => r as u16,
        None => return error_response("Missing 'limit' field"),
    };

    let cmd = ControlCommand::SetChargeLimit { limit };
    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("SetChargeLimit encoded: {:?}", writes);
            ok_response(&format!("Charge limit set to {}%", limit))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/discharge-rate — set battery discharge limit percentage.
pub async fn set_discharge_rate(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<Value> {
    let limit: u16 = match body["limit"].as_u64() {
        Some(r) => r as u16,
        None => return error_response("Missing 'limit' field"),
    };

    let cmd = ControlCommand::SetDischargeLimit { limit };
    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("SetDischargeLimit encoded: {:?}", writes);
            ok_response(&format!("Discharge limit set to {}%", limit))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/pause — pause the battery.
pub async fn pause_battery(State(_state): State<Arc<AppState>>) -> Json<Value> {
    let cmd = ControlCommand::PauseBattery;
    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("PauseBattery encoded: {:?}", writes);
            ok_response("Battery paused")
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

// ---------------------------------------------------------------------------
// Discovery endpoint
// ---------------------------------------------------------------------------

/// GET /api/discover — scan the local network for GivEnergy inverters.
pub async fn discover(State(state): State<Arc<AppState>>) -> Json<Value> {
    tracing::info!("Network discovery requested");

    // Try to determine the local gateway and scan
    let gateway = crate::inverter::discovery::detect_local_subnet();
    let gateway_str = gateway.as_deref().unwrap_or("192.168.1.1");

    let inverters = crate::inverter::discovery::scan_subnet(gateway_str).await;

    Json(json!({
        "ok": true,
        "inverters": inverters,
    }))
}
