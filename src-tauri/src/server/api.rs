//! REST API routes and handlers.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::inverter::encoder::{ControlCommand, RegisterWrite};
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

/// Queue register writes for execution by the poll loop.
async fn queue_writes(state: &Arc<AppState>, writes: Vec<RegisterWrite>) {
    let mut pw = state.pending_writes.lock().await;
    tracing::info!("Queued {} register write(s)", writes.len());
    pw.push(writes);
    drop(pw);
    // Wake the poll loop immediately so writes are applied without
    // waiting for the next read cycle or sleep interval.
    state.write_notify.notify_one();
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

/// GET /api/status — current connection state and LAN IP
pub async fn get_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let cs = state.connection_state.lock().await.clone();
    let host = state.settings.lock().await.host.clone();
    let lan_ip = crate::inverter::discovery::detect_lan_ip();
    let clients = state.connected_clients.lock();
    let client_addrs: Vec<String> = clients.list().into_iter().map(|a| a.to_string()).collect();
    let client_count = clients.count();
    drop(clients);
    Json(json!({
        "ok": true,
        "connection": cs,
        "host": host,
        "lan_ip": lan_ip,
        "clients": client_addrs,
        "client_count": client_count,
    }))
}

/// GET /api/settings
pub async fn get_settings(State(_state): State<Arc<AppState>>) -> Json<Value> {
    let settings = crate::settings::Settings::load();
    Json(json!({
        "ok": true,
        "data": {
            "host": settings.host,
            "port": settings.port,
            "serial": settings.serial,
            "interval_secs": settings.poll_interval,
            "http_port": settings.http_port,
            "import_tariff": settings.import_tariff,
            "export_tariff": settings.export_tariff,
            "import_tariff_config": settings.import_tariff_config,
            "export_tariff_config": settings.export_tariff_config,
        }
    }))
}

/// POST /api/settings
///
/// Accepts a partial update — fields that are present are applied,
/// fields that are absent are left unchanged. This lets the Connect
/// button send `{host,port,serial}` without clobbering `interval_secs`.
pub async fn update_settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<Value> {
    let incoming = match parse_settings(&body) {
        Ok(s) => s,
        Err(e) => return error_response(&e),
    };

    let mut settings = state.settings.lock().await;

    // Always overwrite host/port/serial when provided.
    if !incoming.host.is_empty() {
        settings.host = incoming.host.clone();
    }
    settings.port = if incoming.port != 0 { incoming.port } else { settings.port };
    if !incoming.serial.is_empty() || body.get("serial").is_some() {
        settings.serial = incoming.serial.clone();
    }
    // Only overwrite interval when explicitly provided (> 0).
    if incoming.interval_secs > 0 {
        settings.interval_secs = incoming.interval_secs;
    }

    // Update tariffs if provided
    let current_settings = crate::settings::Settings::load();
    let import_tariff = body
        .get("import_tariff")
        .and_then(|v| v.as_f64())
        .unwrap_or(current_settings.import_tariff);
    let export_tariff = body
        .get("export_tariff")
        .and_then(|v| v.as_f64())
        .unwrap_or(current_settings.export_tariff);

    // Update tariff config objects if provided
    let import_tariff_config = body.get("import_tariff_config").and_then(|v| {
        if v.is_null() { return None; }
        serde_json::from_value::<crate::settings::TariffConfig>(v.clone()).ok()
    });
    let export_tariff_config = body.get("export_tariff_config").and_then(|v| {
        if v.is_null() { return None; }
        serde_json::from_value::<crate::settings::TariffConfig>(v.clone()).ok()
    });

    // Bump version so the poll loop notices the change and reconnects.
    settings.version = settings.version.wrapping_add(1);

    // Persist to disk
    let mut persist = crate::settings::Settings::load();
    persist.host = settings.host.clone();
    persist.port = settings.port;
    persist.serial = settings.serial.clone();
    persist.poll_interval = settings.interval_secs;
    // http_port change requires restart to take effect
    if let Some(hp) = body.get("http_port").and_then(|v| v.as_u64()) {
        persist.http_port = hp as u16;
    }
    persist.auto_connect = true;
    persist.import_tariff = import_tariff;
    persist.export_tariff = export_tariff;
    if let Some(ref cfg) = import_tariff_config {
        persist.import_tariff_config = Some(cfg.clone());
    }
    if let Some(ref cfg) = export_tariff_config {
        persist.export_tariff_config = Some(cfg.clone());
    }
    if let Err(e) = persist.save() {
        tracing::warn!("Failed to persist settings: {}", e);
    }

    let msg = format!(
        "Settings updated: host={}, port={}, serial={}, interval={}s",
        settings.host, settings.port, settings.serial, settings.interval_secs
    );
    tracing::info!("{}", msg);
    ok_response(&msg)
}

fn parse_settings(body: &serde_json::Value) -> Result<PollSettings, String> {
    let host = body["host"].as_str().unwrap_or("").to_string();
    let port = body["port"].as_u64().unwrap_or(8899) as u16;
    let serial = body["serial"].as_str().unwrap_or("").to_string();
    // Only overwrite interval if explicitly provided; otherwise keep current value.
    // The Connect button sends {host,port,serial} without interval_secs,
    // so we must not clobber it with a default.
    let interval_secs = body
        .get("interval_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(0); // 0 = "not provided"

    if !host.is_empty() && port == 0 {
        return Err("Invalid port".to_string());
    }
    if interval_secs > 0 && interval_secs < 5 {
        return Err("interval_secs must be >= 5".to_string());
    }

    Ok(PollSettings {
        host,
        port,
        serial,
        interval_secs, // caller will merge: 0 means "keep existing"
        version: 0,    // not set by the API; caller bumps it
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
    State(state): State<Arc<AppState>>,
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
            queue_writes(&state, writes).await;
            ok_response(&format!("Mode set to {}", mode_str))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/charge-slot — configure a charge schedule slot.
///
/// Body: `{"slot": 1, "start_hour": 6, "start_minute": 0, "end_hour": 10, "end_minute": 0,
///         "enabled": true, "target_soc": 100}`
///
/// If `enabled` is false, the slot times are set to 0 (per givenergy-modbus reference).
/// `target_soc` sets the global charge target SOC register.
/// Also updates `enable_charge` based on whether any charge slot remains active.
pub async fn set_charge_slot(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<Value> {
    let slot: u8 = match body["slot"].as_u64() {
        Some(s) => s as u8,
        None => return error_response("Missing 'slot' field (1-2)"),
    };
    if !(1..=2).contains(&slot) {
        return error_response("Slot must be 1 or 2");
    }

    let enabled = body["enabled"].as_bool().unwrap_or(true);

    let start_hour = body["start_hour"].as_u64().unwrap_or(0) as u8;
    let start_minute = body["start_minute"].as_u64().unwrap_or(0) as u8;
    let end_hour = body["end_hour"].as_u64().unwrap_or(0) as u8;
    let end_minute = body["end_minute"].as_u64().unwrap_or(0) as u8;
    let target_soc = body["target_soc"].as_u64().unwrap_or(100) as u8;

    let (start, end) = if enabled {
        (encode_hhmm(start_hour, start_minute), encode_hhmm(end_hour, end_minute))
    } else {
        // Disabled: write 0 to clear the slot (per givenergy-modbus reference library)
        (0, 0)
    };

    let cmd = match slot {
        1 => ControlCommand::SetChargeSlot1 { start, end },
        2 => ControlCommand::SetChargeSlot2 { start, end },
        _ => unreachable!(),
    };

    match cmd.encode() {
        Ok(mut writes) => {
            // If enabled and target_soc provided, also set the charge target SOC
            if enabled && target_soc > 0 {
                if let Ok(target_writes) =
                    (ControlCommand::SetChargeTargetSoc { soc: target_soc as u16 }).encode()
                {
                    writes.extend(target_writes);
                }
            }

            // Determine whether enable_charge should be on or off.
            // Read the latest snapshot to check other slots' states,
            // then factor in the slot we're about to write.
            let any_enabled = {
                let snap = state.latest_snapshot.lock().await;
                match snap.as_ref() {
                    Some(s) => {
                        // Check all charge slots: are any enabled (other than this one)?
                        let mut found_enabled = false;
                        for (i, cs) in s.charge_slots.iter().enumerate() {
                            let slot_idx = (slot - 1) as usize;
                            if i == slot_idx {
                                // This is the slot we're updating — use the new state
                                if enabled {
                                    found_enabled = true;
                                }
                            } else if cs.enabled {
                                found_enabled = true;
                            }
                        }
                        found_enabled
                    }
                    None => enabled, // No snapshot yet — just use this slot's state
                }
            };

            if let Ok(enable_writes) =
                (ControlCommand::SetEnableCharge { enabled: any_enabled }).encode()
            {
                writes.extend(enable_writes);
            }

            tracing::info!("SetChargeSlot {} encoded: {:?}", slot, writes);
            queue_writes(&state, writes).await;
            ok_response(&format!("Charge slot {} configured", slot))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/discharge-slot — configure a discharge schedule slot.
///
/// Body: `{"slot": 1, "start_hour": 16, "start_minute": 0, "end_hour": 19, "end_minute": 0,
///         "enabled": true}`
///
/// If `enabled` is false, the slot times are set to 0 (per givenergy-modbus reference).
/// Also updates `enable_discharge` based on whether any discharge slot remains active.
pub async fn set_discharge_slot(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<Value> {
    let slot: u8 = match body["slot"].as_u64() {
        Some(s) => s as u8,
        None => return error_response("Missing 'slot' field (1-2)"),
    };
    if !(1..=2).contains(&slot) {
        return error_response("Slot must be 1 or 2");
    }

    let enabled = body["enabled"].as_bool().unwrap_or(true);

    let start_hour = body["start_hour"].as_u64().unwrap_or(0) as u8;
    let start_minute = body["start_minute"].as_u64().unwrap_or(0) as u8;
    let end_hour = body["end_hour"].as_u64().unwrap_or(0) as u8;
    let end_minute = body["end_minute"].as_u64().unwrap_or(0) as u8;

    let (start, end) = if enabled {
        (encode_hhmm(start_hour, start_minute), encode_hhmm(end_hour, end_minute))
    } else {
        (0, 0)
    };

    let cmd = match slot {
        1 => ControlCommand::SetDischargeSlot1 { start, end },
        2 => ControlCommand::SetDischargeSlot2 { start, end },
        _ => unreachable!(),
    };

    match cmd.encode() {
        Ok(mut writes) => {
            // Determine whether enable_discharge should be on or off.
            let any_enabled = {
                let snap = state.latest_snapshot.lock().await;
                match snap.as_ref() {
                    Some(s) => {
                        let mut found_enabled = false;
                        for (i, ds) in s.discharge_slots.iter().enumerate() {
                            let slot_idx = (slot - 1) as usize;
                            if i == slot_idx {
                                if enabled {
                                    found_enabled = true;
                                }
                            } else if ds.enabled {
                                found_enabled = true;
                            }
                        }
                        found_enabled
                    }
                    None => enabled,
                }
            };

            if let Ok(enable_writes) =
                (ControlCommand::SetEnableDischarge { enabled: any_enabled }).encode()
            {
                writes.extend(enable_writes);
            }

            tracing::info!("SetDischargeSlot {} encoded: {:?}", slot, writes);
            queue_writes(&state, writes).await;
            ok_response(&format!("Discharge slot {} configured", slot))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/reserve — set battery reserve SoC percentage.
pub async fn set_reserve(
    State(state): State<Arc<AppState>>,
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
            queue_writes(&state, writes).await;
            ok_response(&format!("Battery reserve set to {}%", soc))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/charge-rate — set battery charge limit percentage.
pub async fn set_charge_rate(
    State(state): State<Arc<AppState>>,
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
            queue_writes(&state, writes).await;
            ok_response(&format!("Charge limit set to {}%", limit))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/discharge-rate — set battery discharge limit percentage.
pub async fn set_discharge_rate(
    State(state): State<Arc<AppState>>,
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
            queue_writes(&state, writes).await;
            ok_response(&format!("Discharge limit set to {}%", limit))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/active-power-rate — set inverter max output active power rate.
pub async fn set_active_power_rate(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<Value> {
    let rate: u16 = match body["rate"].as_u64() {
        Some(r) => r as u16,
        None => return error_response("Missing 'rate' field"),
    };

    let cmd = ControlCommand::SetActivePowerRate { rate };
    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("SetActivePowerRate encoded: {:?}", writes);
            queue_writes(&state, writes).await;
            ok_response(&format!("Active power rate set to {}%", rate))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/pause — pause the battery.
pub async fn pause_battery(State(state): State<Arc<AppState>>) -> Json<Value> {
    let cmd = ControlCommand::PauseBattery;
    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("PauseBattery encoded: {:?}", writes);
            queue_writes(&state, writes).await;
            ok_response("Battery paused")
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/force-charge — enable charging with target SOC.
pub async fn force_charge(State(state): State<Arc<AppState>>) -> Json<Value> {
    let cmd = ControlCommand::ForceCharge { target_soc: 100 };
    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("ForceCharge encoded: {:?}", writes);
            queue_writes(&state, writes).await;
            ok_response("Force charge enabled")
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/force-discharge — enable discharge with a full-day slot.
pub async fn force_discharge(State(state): State<Arc<AppState>>) -> Json<Value> {
    let cmd = ControlCommand::ForceDischarge;
    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("ForceDischarge encoded: {:?}", writes);
            queue_writes(&state, writes).await;
            ok_response("Force discharge enabled")
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/sync-clock — sync inverter clock to system time.
pub async fn sync_clock(State(state): State<Arc<AppState>>) -> Json<Value> {
    let cmd = ControlCommand::SyncClock;
    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("SyncClock encoded: {:?}", writes);
            queue_writes(&state, writes).await;
            ok_response("Clock sync queued")
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

// ---------------------------------------------------------------------------
// History endpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    /// Time range shorthand: "1h", "6h", "24h", "7d", "30d", "6m", "1y"
    pub range: Option<String>,
    /// Comma-separated field names
    pub fields: Option<String>,
    /// Number of windows to page back (default 0)
    pub offset: Option<i64>,
}

/// GET /api/history — aggregated time-series data for charts.
///
/// Query params: `range`, `fields`, `offset`.
/// Returns `{ok: true, data: {field: [{t, v}, ...]}}`.
pub async fn get_history(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HistoryQuery>,
) -> Json<Value> {
    let range_str = params.range.as_deref().unwrap_or("24h");
    let fields_str = params.fields.as_deref().unwrap_or("soc");
    let offset = params.offset.unwrap_or(0);

    let (range_secs, bucket_secs) = match range_str {
        "1h" => (3600, 30),
        "6h" => (3600 * 6, 60),
        "24h" => (86400, 300),
        "7d" => (86400 * 7, 1800),
        "30d" => (86400 * 30, 7200),
        "6m" => (86400 * 180, 43200),
        "1y" => (86400 * 365, 86400),
        _ => return error_response("Invalid range. Use: 1h, 6h, 24h, 7d, 30d, 6m, 1y"),
    };

    let fields: Vec<String> = fields_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if fields.is_empty() {
        return error_response("No fields specified");
    }

    let history = state.history.lock().await;
    match history.as_ref() {
        Some(db) => match db.query_history(range_secs, bucket_secs, offset, &fields) {
            Ok(data) => {
                let map: HashMap<String, Value> =
                    data.into_iter().collect();
                Json(json!({ "ok": true, "data": map }))
            }
            Err(e) => error_response(&e),
        },
        None => error_response("History database not available"),
    }
}

// ---------------------------------------------------------------------------
// Auto winter mode endpoints
// ---------------------------------------------------------------------------

/// GET /api/auto-winter — current config and state.
pub async fn get_auto_winter(State(state): State<Arc<AppState>>) -> Json<Value> {
    let config = state.auto_winter_config.lock().await.clone();
    let aw_state = state.auto_winter_state.lock().await.clone();
    Json(json!({
        "ok": true,
        "data": {
            "config": config,
            "state": aw_state,
        }
    }))
}

/// POST /api/auto-winter — update auto winter config.
///
/// Body fields are optional — only provided fields are updated.
/// Fields: `enabled`, `cold_threshold`, `recovery_threshold`, `target_soc`, `debounce_readings`.
pub async fn set_auto_winter(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<Value> {
    let mut config = state.auto_winter_config.lock().await;

    if let Some(v) = body.get("enabled").and_then(|v| v.as_bool()) {
        config.enabled = v;
    }
    if let Some(v) = body.get("cold_threshold").and_then(|v| v.as_f64()) {
        config.cold_threshold = v as f32;
    }
    if let Some(v) = body.get("recovery_threshold").and_then(|v| v.as_f64()) {
        config.recovery_threshold = v as f32;
    }
    if let Some(v) = body.get("target_soc").and_then(|v| v.as_u64()) {
        config.target_soc = v.clamp(4, 100) as u8;
    }
    if let Some(v) = body.get("debounce_readings").and_then(|v| v.as_u64()) {
        config.debounce_readings = v.max(1) as u32;
    }

    tracing::info!("Auto winter config updated: {:?}", config);

    // Persist to settings.json
    let mut app_settings = crate::settings::Settings::load();
    app_settings.auto_winter_enabled = config.enabled;
    app_settings.auto_winter_cold_threshold = config.cold_threshold;
    app_settings.auto_winter_recovery_threshold = config.recovery_threshold;
    app_settings.auto_winter_target_soc = config.target_soc;
    app_settings.auto_winter_debounce_readings = config.debounce_readings;
    drop(config);
    if let Err(e) = app_settings.save() {
        tracing::warn!("Failed to persist auto winter config: {e}");
    }

    ok_response("Auto winter config updated")
}

// ---------------------------------------------------------------------------
// Discovery endpoint
// ---------------------------------------------------------------------------

/// GET /api/discover — scan the local network for GivEnergy inverters.
pub async fn discover(State(_state): State<Arc<AppState>>) -> Json<Value> {
    tracing::info!("Network discovery requested");

    let subnets = crate::inverter::discovery::detect_lan_subnets();
    tracing::info!("Scanning subnets: {:?}", subnets);

    let inverters = crate::inverter::discovery::scan_multiple_subnets(&subnets).await;

    Json(json!({
        "ok": true,
        "inverters": inverters,
    }))
}

// ---------------------------------------------------------------------------
// Cosy charging endpoints
// ---------------------------------------------------------------------------

/// GET /api/cosy — get cosy charging config.
pub async fn get_cosy(State(state): State<Arc<AppState>>) -> Json<Value> {
    let settings = crate::settings::Settings::load();
    let active = *state.cosy_active.lock().await;
    Json(json!({
        "ok": true,
        "enabled": settings.cosy_enabled,
        "active": active,
        "slots": settings.cosy_slots,
    }))
}

/// POST /api/cosy — update cosy charging config.
pub async fn set_cosy(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<Value> {
    let enabled = body["enabled"].as_bool().unwrap_or(false);
    let mut app_settings = crate::settings::Settings::load();
    app_settings.cosy_enabled = enabled;

    if let Some(slots) = body["slots"].as_array() {
        app_settings.cosy_slots = slots.iter().map(|s| crate::settings::CosySlot {
            enabled: s["enabled"].as_bool().unwrap_or(false),
            start_hour: s["start_hour"].as_u64().unwrap_or(0) as u8,
            start_minute: s["start_minute"].as_u64().unwrap_or(0) as u8,
            end_hour: s["end_hour"].as_u64().unwrap_or(0) as u8,
            end_minute: s["end_minute"].as_u64().unwrap_or(0) as u8,
            target_soc: s["target_soc"].as_u64().unwrap_or(100) as u8,
        }).collect();
    }

    if let Err(e) = app_settings.save() {
        tracing::warn!("Failed to persist cosy config: {e}");
        return error_response(&format!("Failed to save: {e}"));
    }

    ok_response("Cosy config updated")
}

// ---------------------------------------------------------------------------
// Battery calibration endpoint (developer mode)
// ---------------------------------------------------------------------------

/// POST /api/control/calibration — set battery calibration stage.
pub async fn set_calibration(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<Value> {
    let stage: u16 = match body["stage"].as_u64() {
        Some(s) => s as u16,
        None => return error_response("Missing 'stage' field"),
    };

    let cmd = ControlCommand::SetCalibrationStage { stage };
    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("SetCalibrationStage {} encoded: {:?}", stage, writes);
            queue_writes(&state, writes).await;
            ok_response(&format!("Calibration stage set to {}", stage))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/reboot — reboot the inverter.
pub async fn reboot_inverter(
    State(state): State<Arc<AppState>>,
) -> Json<Value> {
    let cmd = ControlCommand::RebootInverter;
    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("RebootInverter encoded: {:?}", writes);
            queue_writes(&state, writes).await;
            ok_response("Reboot command sent")
        }
        Err(e) => error_response(&format!("Error: {}", e)),
    }
}
