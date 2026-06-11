//! REST API routes and handlers.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use chrono::{Datelike, TimeZone};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::inverter::encoder::{ControlCommand, RegisterWrite};
use crate::inverter::model::DeviceType;
use crate::inverter::poll::{AppState, PollSettings};
use crate::modbus::registers::{encode_hhmm, HR_ENABLE_CHARGE_TARGET};

// ---------------------------------------------------------------------------
// Helper: standard JSON response
// ---------------------------------------------------------------------------

fn ok_response(message: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({ "ok": true, "message": message })),
    )
}

fn error_response(error: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "ok": false, "error": error })),
    )
}

/// Return a 500 Internal Server Error response. Use for backend failures
/// (database errors, save failures) where the client should distinguish
/// these from bad-input 400s.
fn server_error(error: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "ok": false, "error": error })),
    )
}

/// Return true when the latest snapshot is from an AC-coupled inverter.
async fn is_ac_coupled_snapshot(state: &Arc<AppState>) -> bool {
    let snapshot = state.latest_snapshot.lock().await;
    snapshot
        .as_ref()
        .map(|s| {
            matches!(
                s.device_type,
                DeviceType::ACCoupled | DeviceType::ACCoupledMk2
            )
        })
        .unwrap_or(false)
}

async fn is_three_phase_limit_snapshot(state: &Arc<AppState>) -> bool {
    let snapshot = state.latest_snapshot.lock().await;
    snapshot
        .as_ref()
        .map(|s| s.device_type.uses_three_phase_schedule_slots())
        .unwrap_or(false)
}

fn charge_slot_command_for_device(
    device_type: DeviceType,
    slot: u8,
    enabled: bool,
    start: u16,
    end: u16,
) -> Result<ControlCommand, String> {
    let (start, end) = if enabled { (start, end) } else { (0, 0) };
    match (device_type.uses_three_phase_schedule_slots(), slot, enabled) {
        (true, 1, _) => Ok(ControlCommand::SetThreePhaseChargeSlot1 { start, end }),
        (true, 2, _) => Ok(ControlCommand::SetThreePhaseChargeSlot2 { start, end }),
        (true, 3..=10, _) => Ok(ControlCommand::SetChargeSlotN { slot, start, end }),
        (false, _, false) => Ok(ControlCommand::SetEnableCharge { enabled: false }),
        (false, 1, true) => Ok(ControlCommand::SetChargeSlot1 { start, end }),
        // Gen3/AIO/HV-Gen3 use HR 243-244 for charge slot 2 (the extended-block
        // copy is authoritative on these models; classic HR 31-32 may be stale).
        (false, 2, true) if device_type.supports_gen3_extended() => {
            Ok(ControlCommand::SetGen3ChargeSlot2 { start, end })
        }
        (false, 2, true) => Ok(ControlCommand::SetChargeSlot2 { start, end }),
        (false, 3..=10, true) => Ok(ControlCommand::SetChargeSlotN { slot, start, end }),
        (_, _, _) => Err(format!("Unsupported charge slot {}", slot)),
    }
}

fn discharge_slot_command_for_device(
    device_type: DeviceType,
    slot: u8,
    enabled: bool,
    start: u16,
    end: u16,
) -> Result<ControlCommand, String> {
    // When disabled, clear the slot times (write 0/0). We deliberately do NOT
    // touch the master enable_discharge flag here — that is controlled by the
    // battery mode (Timed Demand/Export). Keeping slot configuration
    // independent of mode selection matches the givenergy-modbus reference,
    // where set_discharge_slot() writes only the slot registers. Coupling the
    // two forced an immediate Eco→TimedDemand mode switch whenever a discharge
    // slot was saved.
    let (start, end) = if enabled { (start, end) } else { (0, 0) };
    match (device_type.uses_three_phase_schedule_slots(), slot) {
        (true, 1) => Ok(ControlCommand::SetThreePhaseDischargeSlot1 { start, end }),
        (true, 2) => Ok(ControlCommand::SetThreePhaseDischargeSlot2 { start, end }),
        (false, 1) => Ok(ControlCommand::SetDischargeSlot1 { start, end }),
        (false, 2) => Ok(ControlCommand::SetDischargeSlot2 { start, end }),
        (_, 3..=10) => Ok(ControlCommand::SetDischargeSlotN { slot, start, end }),
        (_, _) => Err(format!("Unsupported discharge slot {}", slot)),
    }
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
pub async fn get_snapshot(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let snapshot = state.latest_snapshot.lock().await;
    match snapshot.as_ref() {
        Some(snap) => (StatusCode::OK, Json(json!({ "ok": true, "data": snap }))),
        None => (
            StatusCode::OK,
            Json(json!({ "ok": false, "error": "No snapshot available yet" })),
        ),
    }
}

/// GET /api/status — current connection state and LAN IP
pub async fn get_status(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let cs = state.connection_state.lock().await.clone();
    let host = state.settings.lock().await.host.clone();
    let lan_ip = tokio::task::spawn_blocking(crate::inverter::discovery::detect_lan_ip)
        .await
        .unwrap_or(None);
    let clients = state.connected_clients.lock();
    let client_addrs: Vec<String> = clients.list().into_iter().map(|a| a.to_string()).collect();
    let client_count = clients.count();
    drop(clients);
    (
        StatusCode::OK,
        Json(json!({
        "ok": true,
        "connection": cs,
        "host": host,
        "lan_ip": lan_ip,
        "clients": client_addrs,
        "client_count": client_count,
        })),
    )
}

/// GET /api/settings
pub async fn get_settings(State(_state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let settings = crate::settings::Settings::load();
    (
        StatusCode::OK,
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
            "hidden_panels": settings.hidden_panels,
        }
        })),
    )
}

/// POST /api/settings
///
/// Accepts a partial update — fields that are present are applied,
/// fields that are absent are left unchanged. This lets the Connect
/// button send `{host,port,serial}` without clobbering `interval_secs`.
pub async fn update_settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let incoming = match parse_settings(&body) {
        Ok(s) => s,
        Err(e) => return error_response(&e),
    };

    // Read tariff defaults from disk BEFORE acquiring the in-memory lock,
    // so the synchronous file I/O doesn't block the Tokio worker thread
    // while the poll loop contends for the same lock.
    let disk_settings = crate::settings::Settings::load();
    let import_tariff_default = disk_settings.import_tariff;
    let export_tariff_default = disk_settings.export_tariff;
    let import_tariff_config_default = disk_settings.import_tariff_config.clone();
    let export_tariff_config_default = disk_settings.export_tariff_config.clone();
    drop(disk_settings);

    // Update tariffs if provided (use pre-loaded defaults from disk).
    let import_tariff = body
        .get("import_tariff")
        .and_then(|v| v.as_f64())
        .unwrap_or(import_tariff_default);
    let export_tariff = body
        .get("export_tariff")
        .and_then(|v| v.as_f64())
        .unwrap_or(export_tariff_default);

    // Update tariff config objects if provided
    let import_tariff_config = body
        .get("import_tariff_config")
        .and_then(|v| {
        if v.is_null() {
            return None;
        }
        serde_json::from_value::<crate::settings::TariffConfig>(v.clone()).ok()
        })
        .or(import_tariff_config_default);
    let export_tariff_config = body
        .get("export_tariff_config")
        .and_then(|v| {
        if v.is_null() {
            return None;
        }
        serde_json::from_value::<crate::settings::TariffConfig>(v.clone()).ok()
        })
        .or(export_tariff_config_default);

    // Build the disk-persist struct from the request body and current
    // disk state. Save to disk BEFORE touching the in-memory settings,
    // so a failed save doesn't leave in-memory state out of sync with disk
    // (the poll loop would reconnect to a new host that settings.json
    // doesn't remember on restart).
    let mut persist = crate::settings::Settings::load();
    if !incoming.host.is_empty() {
        persist.host = incoming.host.clone();
    }
    persist.port = if incoming.port != 0 {
        incoming.port
    } else {
        persist.port
    };
    if !incoming.serial.is_empty() || body.get("serial").is_some() {
        persist.serial = incoming.serial.clone();
    }
    if incoming.interval_secs > 0 {
        persist.poll_interval = incoming.interval_secs;
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
    if let Some(hp) = body.get("http_port").and_then(|v| v.as_u64()) {
        persist.http_port = hp.min(u16::MAX as u64) as u16;
    }
    if let Some(hp) = body.get("hidden_panels").and_then(|v| v.as_array()) {
        let panels: Vec<String> = hp
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        persist.hidden_panels = panels;
    }
    if let Err(e) = persist.save() {
        tracing::warn!("Failed to persist settings: {}", e);
        return server_error(&format!("Failed to save settings: {}", e));
    }
    drop(persist);

    // Now that disk is updated, apply changes to the in-memory state.
    // Lock is held briefly — no file I/O while holding it.
    let mut settings = state.settings.lock().await;

    let prev_host = settings.host.clone();
    let prev_port = settings.port;
    let prev_serial = settings.serial.clone();

    if !incoming.host.is_empty() {
        settings.host = incoming.host.clone();
    }
    settings.port = if incoming.port != 0 {
        incoming.port
    } else {
        settings.port
    };
    if !incoming.serial.is_empty() || body.get("serial").is_some() {
        settings.serial = incoming.serial.clone();
    }
    if incoming.interval_secs > 0 {
        settings.interval_secs = incoming.interval_secs;
    }

    let connection_changed =
        settings.host != prev_host || settings.port != prev_port || settings.serial != prev_serial;

    if connection_changed {
        settings.version = settings.version.wrapping_add(1);
        state.write_notify.notify_one();
    } else if incoming.interval_secs > 0 {
        state.write_notify.notify_one();
    }

    drop(settings);

    let msg = format!(
        "Settings updated: host={}, port={}, serial={}, interval={}s",
        incoming.host, incoming.port, incoming.serial, incoming.interval_secs,
    );

    tracing::info!("{}", msg);
    ok_response(&msg)
}

fn parse_settings(body: &serde_json::Value) -> Result<PollSettings, String> {
    let host = body["host"].as_str().unwrap_or("").to_string();
    let port_raw = body.get("port").and_then(|v| v.as_u64());
    let port = port_raw.unwrap_or(0) as u16;
    let serial = body["serial"].as_str().unwrap_or("").to_string();
    // Only overwrite interval if explicitly provided; otherwise keep current value.
    // The Connect button sends {host,port,serial} without interval_secs,
    // so we must not clobber it with a default.
    let interval_secs = body
        .get("interval_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(0); // 0 = "not provided"

    // Only reject port=0 when it was explicitly provided.
    if !host.is_empty() && port == 0 && body.get("port").is_some() {
        return Err("Invalid port: must be > 0".to_string());
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
) -> (StatusCode, Json<Value>) {
    let mode_str = match body["mode"].as_str() {
        Some(m) => m,
        None => return error_response("Missing 'mode' field"),
    };
    let soc_reserve = body["soc_reserve"].as_u64().unwrap_or(4) as u16;

    let is_timed = mode_str == "timed_demand" || mode_str == "timed_export";

    let cmd = match mode_str {
        "eco" => ControlCommand::SetEcoMode { soc_reserve },
        "eco_paused" => ControlCommand::PauseBattery,
        "timed_demand" => ControlCommand::SetTimedDemandMode { soc_reserve },
        "timed_export" => ControlCommand::SetTimedExportMode { soc_reserve },
        "export_paused" => ControlCommand::SetBatteryPowerMode { mode: 0 },
        _ => return error_response(&format!("Unknown mode: '{}'", mode_str)),
    };

    match cmd.encode() {
        Ok(mut writes) => {
            tracing::info!("Mode command encoded: {:?}", writes);

            // When switching to Eco mode, clear ALL discharge slot registers
            // to prevent Gen3 inverter firmware from auto-re-enabling
            // enable_discharge. The Gen3 keeps HR59=1 when discharge slot
            // registers are non-zero, making it impossible to stay in Eco.
            if mode_str == "eco" || mode_str == "eco_paused" {
                // Discharge slot 2: HR44-45
                writes.push(RegisterWrite {
                    address: 44,
                    value: 0,
                });
                writes.push(RegisterWrite {
                    address: 45,
                    value: 0,
                });
                // Discharge slot 1: HR56-57
                writes.push(RegisterWrite {
                    address: 56,
                    value: 0,
                });
                writes.push(RegisterWrite {
                    address: 57,
                    value: 0,
                });
            }

            // When switching to Timed mode, the frontend may include
            // discharge_slots that were configured locally in Eco mode.
            // Write them atomically BEFORE the enable_discharge flag so the
            // inverter never sees HR59=1 without slot constraints.
            if is_timed {
                if let Some(slots) = body["discharge_slots"].as_array() {
                    let device_type = state
                        .latest_snapshot
                        .lock()
                        .await
                        .as_ref()
                        .map(|s| s.device_type)
                        .unwrap_or(DeviceType::Gen2Hybrid);

                    // Prepend slot writes before the mode writes.
                    let mut slot_writes = Vec::new();
                    for slot_obj in slots {
                        let slot_num = match slot_obj["slot"].as_u64() {
                            Some(s) => s as u8,
                            None => continue,
                        };
                        let enabled = slot_obj["enabled"].as_bool().unwrap_or(true);
                        let start_hour = slot_obj["start_hour"].as_u64().unwrap_or(0) as u8;
                        let start_minute = slot_obj["start_minute"].as_u64().unwrap_or(0) as u8;
                        let end_hour = slot_obj["end_hour"].as_u64().unwrap_or(0) as u8;
                        let end_minute = slot_obj["end_minute"].as_u64().unwrap_or(0) as u8;
                        let target_soc = slot_obj["target_soc"].as_u64().unwrap_or(100) as u8;

                        let (start, end) = (
                            encode_hhmm(start_hour, start_minute),
                            encode_hhmm(end_hour, end_minute),
                        );

                        let cmd = match discharge_slot_command_for_device(
                            device_type,
                            slot_num,
                            enabled,
                            start,
                            end,
                        ) {
                            Ok(cmd) => cmd,
                            Err(e) => {
                                tracing::warn!("Skipping discharge slot {}: {}", slot_num, e);
                                continue;
                            }
                        };

                        match cmd.encode() {
                            Ok(mut w) => {
                                // Write per-slot discharge target SOC for extended models.
                                if enabled
                                    && target_soc > 0
                                    && device_type.uses_extended_schedule_slots()
                                {
                                    if let Ok(target_writes) =
                                        (ControlCommand::SetDischargeTargetSocSlot {
                                            slot: slot_num,
                                            soc: target_soc as u16,
                                        }
                                        .encode())
                                    {
                                        w.extend(target_writes);
                                    }
                                }
                                slot_writes.extend(w);
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to encode discharge slot {}: {}",
                                    slot_num,
                                    e
                                );
                            }
                        }
                    }
                    // Slot writes go FIRST so they're on the inverter before
                    // HR59=1 is set.
                    let mut combined = slot_writes;
                    combined.append(&mut writes);
                    writes = combined;
                }
            }

            queue_writes(&state, writes).await;
            ok_response(&format!("Mode set to {}", mode_str))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/charge-slot — configure a charge schedule slot.
///
/// Body: `{"slot": 1, "start_hour": 6, "start_minute": 0, "end_hour": 10, "end_minute": 0,
///         "enabled": true}`
///
/// If `enabled` is false, the slot times are set to sentinel 0 (disabled).
/// When enabling a slot, `enable_charge` (HR 96) is set to 1 to allow
/// slot-based scheduled charging. This does NOT trigger immediate force
/// charge — only `enable_charge_target + charge_target_soc` do.
/// Also updates `enable_charge` based on whether any charge slot remains active.
pub async fn set_charge_slot(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let slot_raw = match body["slot"].as_u64() {
        Some(s) => s,
        None => return error_response("Missing 'slot' field (1-2)"),
    };
    let device_type = state
        .latest_snapshot
        .lock()
        .await
        .as_ref()
        .map(|s| s.device_type)
        .unwrap_or(DeviceType::Gen2Hybrid);
    // AC Coupled and Gen1 Hybrid only support charge slot 1 (HR 94-95).
    let max_slots = device_type.max_charge_slots();
    if slot_raw > u8::MAX as u64 {
        return error_response(&format!("Slot must be 1..{max_slots}, got {slot_raw}"));
    }
    let slot = slot_raw as u8;
    if slot > max_slots {
        return error_response(&format!(
            "Charge slot {} not supported on this inverter model (max {})",
            slot, max_slots
        ));
    }

    if !(1..=max_slots).contains(&slot) {
        return error_response(&format!("Slot must be 1..{}, got {}", max_slots, slot));
    }

    let enabled = body["enabled"].as_bool().unwrap_or(true);

    let start_hour = body["start_hour"].as_u64().unwrap_or(0) as u8;
    let start_minute = body["start_minute"].as_u64().unwrap_or(0) as u8;
    let end_hour = body["end_hour"].as_u64().unwrap_or(0) as u8;
    let end_minute = body["end_minute"].as_u64().unwrap_or(0) as u8;
    let target_soc = body["target_soc"].as_u64().unwrap_or(100) as u8;

    if start_hour > 23 || end_hour > 23 {
        return error_response("Hour must be 0-23");
    }
    if start_minute > 59 || end_minute > 59 {
        return error_response("Minute must be 0-59");
    }

    let (start, end) = (
        encode_hhmm(start_hour, start_minute),
        encode_hhmm(end_hour, end_minute),
    );

    let cmd = match charge_slot_command_for_device(device_type, slot, enabled, start, end) {
        Ok(cmd) => cmd,
        Err(e) => return error_response(&e),
    };

    match cmd.encode() {
        Ok(mut writes) => {
            // When enabling a slot, also set enable_charge = 1 so the
            // inverter allows scheduled charging. Per the givenergy-modbus
            // reference, enable_charge alone (without enable_charge_target)
            // enables slot-based charging — NOT immediate force charge.
            //
            // We also clear enable_charge_target (HR 20) to 0 so that a
            // stale force-charge flag from a previous operation doesn't
            // cause snapshotForceCharge (enable_charge && enable_charge_target)
            // to show as true when the user simply configured a schedule slot.
            if enabled {
                if !device_type.uses_three_phase_schedule_slots() {
                    writes.push(RegisterWrite {
                        address: HR_ENABLE_CHARGE_TARGET,
                        value: 0,
                    });
                    if let Ok(enable_writes) =
                        (ControlCommand::SetEnableCharge { enabled: true }).encode()
                    {
                        writes.extend(enable_writes);
                    }
                }
                // Write per-slot target SOC (extended registers HR 242+) when the
                // inverter supports the HR240-299 schedule/target block.
                if target_soc > 0 && device_type.uses_extended_schedule_slots() {
                    if let Ok(target_writes) = (ControlCommand::SetChargeTargetSocSlot {
                        slot,
                        soc: target_soc as u16,
                    })
                    .encode()
                    {
                        writes.extend(target_writes);
                    }
                }
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
/// This writes ONLY the slot time registers — it does not touch the master
/// `enable_discharge` flag, which is controlled by the battery mode (Timed
/// Demand/Export). The schedule becomes active when the user selects a timed
/// mode, keeping slot configuration independent of mode selection.
pub async fn set_discharge_slot(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let slot_raw = match body["slot"].as_u64() {
        Some(s) => s,
        None => return error_response("Missing 'slot' field (1-2)"),
    };
    let device_type = state
        .latest_snapshot
        .lock()
        .await
        .as_ref()
        .map(|s| s.device_type)
        .unwrap_or(DeviceType::Gen2Hybrid);
    // Check model support — AC Coupled/Gen1 only have 1 discharge slot.
    let max_slots = device_type.max_discharge_slots();
    if slot_raw > u8::MAX as u64 {
        return error_response(&format!("Slot must be 1..{max_slots}, got {slot_raw}"));
    }
    let slot = slot_raw as u8;
    if slot > max_slots {
        return error_response(&format!(
            "Discharge slot {} not supported on this inverter model (max {})",
            slot, max_slots
        ));
    }

    if !(1..=max_slots).contains(&slot) {
        return error_response(&format!("Slot must be 1..{}, got {}", max_slots, slot));
    }

    let enabled = body["enabled"].as_bool().unwrap_or(true);

    let start_hour = body["start_hour"].as_u64().unwrap_or(0) as u8;
    let start_minute = body["start_minute"].as_u64().unwrap_or(0) as u8;
    let end_hour = body["end_hour"].as_u64().unwrap_or(0) as u8;
    let end_minute = body["end_minute"].as_u64().unwrap_or(0) as u8;
    let target_soc = body["target_soc"].as_u64().unwrap_or(100) as u8;

    if start_hour > 23 || end_hour > 23 {
        return error_response("Hour must be 0-23");
    }
    if start_minute > 59 || end_minute > 59 {
        return error_response("Minute must be 0-59");
    }

    let (start, end) = (
        encode_hhmm(start_hour, start_minute),
        encode_hhmm(end_hour, end_minute),
    );

    let cmd = match discharge_slot_command_for_device(device_type, slot, enabled, start, end) {
        Ok(cmd) => cmd,
        Err(e) => return error_response(&e),
    };

    match cmd.encode() {
        Ok(mut writes) => {
            // We do NOT set enable_discharge here. That flag is the master
            // "timed discharge" switch and is controlled by the battery mode
            // (Timed Demand/Export). Setting it from a slot save forced an
            // immediate Eco→TimedDemand mode switch. Per givenergy-modbus,
            // set_discharge_slot() writes only the slot time registers.
            // Write per-slot discharge target SOC (extended registers HR 272+)
            // when the inverter supports the HR240-299 schedule/target block.
            if enabled && target_soc > 0 && device_type.uses_extended_schedule_slots() {
                if let Ok(target_writes) = (ControlCommand::SetDischargeTargetSocSlot {
                    slot,
                    soc: target_soc as u16,
                })
                .encode()
                {
                    writes.extend(target_writes);
                }
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
) -> (StatusCode, Json<Value>) {
    let soc: u16 = match body["soc"].as_u64() {
        Some(s) => s as u16,
        None => return error_response("Missing 'soc' field (4-100)"),
    };

    let is_three_phase = is_three_phase_limit_snapshot(&state).await;
    let cmd = if is_three_phase {
        ControlCommand::SetThreePhaseBatterySocReserve { reserve: soc }
    } else {
        ControlCommand::SetBatterySocReserve { reserve: soc }
    };
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
) -> (StatusCode, Json<Value>) {
    let limit: u16 = match body["limit"].as_u64() {
        Some(r) => r as u16,
        None => return error_response("Missing 'limit' field (0-50)"),
    };

    let is_ac_coupled = is_ac_coupled_snapshot(&state).await;
    let is_three_phase = is_three_phase_limit_snapshot(&state).await;
    let cmd = if is_three_phase {
        ControlCommand::SetThreePhaseChargeLimit { limit }
    } else if is_ac_coupled {
        ControlCommand::SetAcChargeLimit { limit }
    } else {
        ControlCommand::SetChargeLimit { limit }
    };
    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("SetChargeLimit encoded: {:?}", writes);
            queue_writes(&state, writes).await;
            let label = if is_three_phase {
                "Three-phase"
            } else if is_ac_coupled {
                "AC-coupled"
            } else {
                "Battery"
            };
            ok_response(&format!("{} charge limit set to {}%", label, limit))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/discharge-rate — set battery discharge limit percentage.
pub async fn set_discharge_rate(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let limit: u16 = match body["limit"].as_u64() {
        Some(r) => r as u16,
        None => return error_response("Missing 'limit' field (0-50)"),
    };

    let is_ac_coupled = is_ac_coupled_snapshot(&state).await;
    let is_three_phase = is_three_phase_limit_snapshot(&state).await;
    let cmd = if is_three_phase {
        ControlCommand::SetThreePhaseDischargeLimit { limit }
    } else if is_ac_coupled {
        ControlCommand::SetAcDischargeLimit { limit }
    } else {
        ControlCommand::SetDischargeLimit { limit }
    };
    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("SetDischargeLimit encoded: {:?}", writes);
            queue_writes(&state, writes).await;
            let label = if is_three_phase {
                "Three-phase"
            } else if is_ac_coupled {
                "AC-coupled"
            } else {
                "Battery"
            };
            ok_response(&format!("{} discharge limit set to {}%", label, limit))
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/active-power-rate — set inverter max output active power rate.
pub async fn set_active_power_rate(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
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
///
/// Disables charge and discharge. For three-phase models, also clears
/// the three-phase force discharge and force charge enable flags.
pub async fn pause_battery(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let is_three_phase = is_three_phase_limit_snapshot(&state).await;
    let mut writes = match ControlCommand::PauseBattery.encode() {
        Ok(w) => w,
        Err(e) => return error_response(&format!("Validation error: {}", e)),
    };
    if is_three_phase {
        // Also clear three-phase-specific force discharge/charge flags
        use crate::modbus::registers::{
            HR_3PH_AC_CHARGE_ENABLE, HR_3PH_FORCE_CHARGE_ENABLE, HR_3PH_FORCE_DISCHARGE_ENABLE,
        };
        writes.push(crate::inverter::encoder::RegisterWrite {
            address: HR_3PH_FORCE_DISCHARGE_ENABLE,
            value: 0,
        });
        writes.push(crate::inverter::encoder::RegisterWrite {
            address: HR_3PH_FORCE_CHARGE_ENABLE,
            value: 0,
        });
        writes.push(crate::inverter::encoder::RegisterWrite {
            address: HR_3PH_AC_CHARGE_ENABLE,
            value: 0,
        });
    }
    tracing::info!("PauseBattery encoded: {:?}", writes);
    queue_writes(&state, writes).await;
    ok_response("Battery paused")
}

/// POST /api/control/force-charge — enable charging with target SOC.
///
/// Uses three-phase registers (HR 1123/1111) for three-phase, commercial,
/// and HV hybrid inverters; single-phase registers (HR 96/116) for all others.
pub async fn force_charge(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let is_three_phase = is_three_phase_limit_snapshot(&state).await;
    let cmd = if is_three_phase {
        ControlCommand::ThreePhaseForceCharge { target_soc: 100 }
    } else {
        ControlCommand::ForceCharge { target_soc: 100 }
    };
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
///
/// Uses three-phase register (HR 1122) for three-phase, commercial,
/// and HV hybrid inverters; single-phase register (HR 59 + slots) for all others.
pub async fn force_discharge(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let is_three_phase = is_three_phase_limit_snapshot(&state).await;
    let cmd = if is_three_phase {
        ControlCommand::ThreePhaseForceDischarge
    } else {
        ControlCommand::ForceDischarge
    };
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
pub async fn sync_clock(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
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
    /// Time range shorthand: "1h", "6h", "12h", "24h", "today", "7d", "30d", "6m", "1y"
    pub range: Option<String>,
    /// Comma-separated field names
    pub fields: Option<String>,
    /// Number of windows to page back (default 0)
    pub offset: Option<i64>,
    /// Use a rolling [now - range, now] window instead of an aligned bucket.
    pub rolling: Option<bool>,
}

/// GET /api/history — aggregated time-series data for charts.
///
/// Query params: `range`, `fields`, `offset`, `rolling`.
/// Returns `{ok: true, data: {field: [{t, v}, ...]}}`.
pub async fn get_history(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HistoryQuery>,
) -> (StatusCode, Json<Value>) {
    let range_str = params.range.as_deref().unwrap_or("24h");
    let fields_str = params.fields.as_deref().unwrap_or("soc");
    let offset = params.offset.unwrap_or(0);
    let rolling = params.rolling.unwrap_or(false);

    let (range_secs, bucket_secs) = match range_str {
        "1h" => (3600, 30),
        "6h" => (3600 * 6, 60),
        "12h" => (3600 * 12, 120),
        "24h" => (86400, 300),
        "today" => (86400, 300),
        "7d" => (86400 * 7, 1800),
        "30d" => (86400 * 30, 7200),
        "6m" => (86400 * 180, 43200),
        "1y" => (86400 * 365, 86400),
        "month" => (0, 3600), // calendar month — uses explicit window
        _ => {
            return error_response(
                "Invalid range. Use: 1h, 6h, 12h, 24h, today, 7d, 30d, 6m, 1y, month",
            )
        }
    };

    let explicit_window: Option<(i64, i64)> =
        if rolling && range_str != "month" && range_str != "today" {
        let end_ts = chrono::Utc::now().timestamp() - offset * range_secs;
        Some((end_ts - range_secs, end_ts))
    } else if range_str == "today" {
        let now = chrono::Local::now();
        let start_date = now.date_naive() - chrono::Duration::days(offset);
        let start_local = start_date.and_hms_opt(0, 0, 0).unwrap();
        let start_local_dt = chrono::Local
            .from_local_datetime(&start_local)
            .earliest()
            .unwrap();
        let end_date = start_date.succ_opt().unwrap();
        let end_local = end_date.and_hms_opt(0, 0, 0).unwrap();
        let end_ts = chrono::Local
            .from_local_datetime(&end_local)
            .earliest()
            .unwrap()
            .timestamp();

        Some((start_local_dt.timestamp(), end_ts))
    } else if range_str == "month" {
        // Compute calendar month boundaries in local time.
        let now = chrono::Local::now();
        // Apply offset (month offset, since month windows have variable length)
        let total_months = now.year() * 12 + (now.month() as i32) - 1 - offset as i32;
        let target_year = total_months.div_euclid(12);
        let target_month = (total_months.rem_euclid(12) + 1) as u32;

        // Start of target month (local midnight of the 1st)
        let start_local = chrono::NaiveDate::from_ymd_opt(target_year, target_month, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let start_local_dt = chrono::Local
            .from_local_datetime(&start_local)
            .earliest()
            .unwrap();

        // End of target month = start of next month
        let (next_year, next_month) = if target_month == 12 {
            (target_year + 1, 1u32)
        } else {
            (target_year, target_month + 1)
        };
        let end_local = chrono::NaiveDate::from_ymd_opt(next_year, next_month, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let end_local_dt = chrono::Local
            .from_local_datetime(&end_local)
            .earliest()
            .unwrap();

        let start_ts = start_local_dt.timestamp();
        let end_ts = end_local_dt.timestamp();

        Some((start_ts, end_ts))
    } else {
        None
    };

    let fields: Vec<String> = fields_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if fields.is_empty() {
        return error_response("No fields specified");
    }

    // Clone the HistoryDb handle and drop the async lock so the synchronous
    // SQLite query runs on a blocking thread instead of pinning the Tokio
    // worker while holding the async mutex.
    let history_db = state.history.lock().await.clone();

    match history_db {
        Some(db) => {
            let fields_clone = fields.clone();
            let result = tokio::task::spawn_blocking(move || {
                db.query_history(
                    range_secs,
                    bucket_secs,
                    offset,
                    &fields_clone,
                    explicit_window,
                )
            })
            .await;

            match result {
                Ok(Ok(data)) => {
                    let map: HashMap<String, Value> = data.into_iter().collect();
                    (StatusCode::OK, Json(json!({ "ok": true, "data": map })))
                }
                Ok(Err(e)) => error_response(&e),
                Err(e) => error_response(&format!("History query join error: {e}")),
            }
        }
        None => server_error("History database not available"),
    }
}

// ---------------------------------------------------------------------------
// Auto winter mode endpoints
// ---------------------------------------------------------------------------

/// GET /api/auto-winter — current config and state.
pub async fn get_auto_winter(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let config = state.auto_winter_config.lock().await.clone();
    let aw_state = state.auto_winter_state.lock().await.clone();
    (
        StatusCode::OK,
        Json(json!({
        "ok": true,
        "data": {
            "config": config,
            "state": aw_state,
        }
        })),
    )
}

/// POST /api/auto-winter — update auto winter config.
///
/// Body fields are optional — only provided fields are updated.
/// Fields: `enabled`, `cold_threshold`, `recovery_threshold`, `target_soc`, `debounce_readings`.
pub async fn set_auto_winter(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
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
        return server_error(&format!("Failed to save: {e}"));
    }

    ok_response("Auto winter config updated")
}

// ---------------------------------------------------------------------------
// Discovery endpoint
// ---------------------------------------------------------------------------

/// GET /api/discover — scan the local network for GivEnergy inverters.
pub async fn discover(State(_state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    tracing::info!("Network discovery requested");

    let subnets = crate::inverter::discovery::detect_lan_subnets();
    tracing::info!("Scanning subnets: {:?}", subnets);

    let inverters = crate::inverter::discovery::scan_multiple_subnets(&subnets).await;

    (
        StatusCode::OK,
        Json(json!({
        "ok": true,
        "subnets": subnets,
        "inverters": inverters,
        })),
    )
}

// ---------------------------------------------------------------------------
// Cosy charging endpoints
// ---------------------------------------------------------------------------

/// GET /api/cosy — get cosy charging config.
pub async fn get_cosy(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let settings = crate::settings::Settings::load();
    let active = *state.cosy_active.lock().await;
    (
        StatusCode::OK,
        Json(json!({
        "ok": true,
        "enabled": settings.cosy_enabled,
        "active": active,
        "slots": settings.cosy_slots,
        })),
    )
}

/// POST /api/cosy — update cosy charging config.
pub async fn set_cosy(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let enabled = body["enabled"].as_bool().unwrap_or(false);
    let mut app_settings = crate::settings::Settings::load();
    app_settings.cosy_enabled = enabled;

    if let Some(slots) = body["slots"].as_array() {
        app_settings.cosy_slots = slots
            .iter()
            .map(|s| crate::settings::CosySlot {
                enabled: s["enabled"].as_bool().unwrap_or(false),
                start_hour: s["start_hour"].as_u64().map(|v| v.min(23)).unwrap_or(0) as u8,
                start_minute: s["start_minute"].as_u64().map(|v| v.min(59)).unwrap_or(0) as u8,
                end_hour: s["end_hour"].as_u64().map(|v| v.min(23)).unwrap_or(0) as u8,
                end_minute: s["end_minute"].as_u64().map(|v| v.min(59)).unwrap_or(0) as u8,
                target_soc: s["target_soc"].as_u64().unwrap_or(100) as u8,
            })
            .collect();
    }

    if let Err(e) = app_settings.save() {
        tracing::warn!("Failed to persist cosy config: {e}");
        return server_error(&format!("Failed to save: {e}"));
    }

    ok_response("Cosy config updated")
}

// ---------------------------------------------------------------------------
// Agile Octopus endpoints
// ---------------------------------------------------------------------------

/// GET /api/agile — get Agile Octopus config.
pub async fn get_agile(State(_state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let settings = crate::settings::Settings::load();
    (
        StatusCode::OK,
        Json(json!({
        "ok": true,
        "enabled": settings.agile_enabled,
        "region": settings.agile_region,
        "charge_threshold": settings.agile_charge_threshold,
        "discharge_threshold": settings.agile_discharge_threshold,
        })),
    )
}

/// POST /api/agile — update Agile Octopus config.
pub async fn set_agile(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let mut app_settings = crate::settings::Settings::load();
    app_settings.agile_enabled = body["enabled"].as_bool().unwrap_or(false);
    if let Some(r) = body["region"].as_str() {
        app_settings.agile_region = r.to_string();
    }
    app_settings.agile_charge_threshold = body["charge_threshold"].as_f64().unwrap_or(10.0);
    app_settings.agile_discharge_threshold = body["discharge_threshold"].as_f64().unwrap_or(30.0);

    if let Err(e) = app_settings.save() {
        tracing::warn!("Failed to persist agile config: {e}");
        return server_error(&format!("Failed to save: {e}"));
    }

    ok_response("Agile config updated")
}

// ---------------------------------------------------------------------------
// Load discharge limiter endpoints
// ---------------------------------------------------------------------------

/// GET /api/load-limiter — current config and state.
pub async fn get_load_limiter(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let config = state.load_limiter_config.lock().await.clone();
    let ll_state = state.load_limiter_state.lock().await.clone();
    (
        StatusCode::OK,
        Json(json!({
        "ok": true,
        "data": {
            "config": config,
            "state": ll_state,
        }
        })),
    )
}

/// POST /api/load-limiter — update load discharge limiter config.
///
/// Body fields are optional — only provided fields are updated.
/// Fields: `enabled`, `threshold_w`, `trigger_delay_minutes`,
///         `start_hour`, `start_minute`, `end_hour`, `end_minute`.
pub async fn set_load_limiter(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let mut config = state.load_limiter_config.lock().await;

    if let Some(v) = body.get("enabled").and_then(|v| v.as_bool()) {
        config.enabled = v;
    }
    if let Some(v) = body.get("threshold_w").and_then(|v| v.as_u64()) {
        config.threshold_w = v.clamp(100, 50000) as u32;
    }
    if let Some(v) = body.get("trigger_delay_minutes").and_then(|v| v.as_u64()) {
        config.trigger_delay_minutes = v.clamp(1, 120) as u32;
    }
    if let Some(v) = body.get("start_hour").and_then(|v| v.as_u64()) {
        config.start_hour = v.min(23) as u8;
    }
    if let Some(v) = body.get("start_minute").and_then(|v| v.as_u64()) {
        config.start_minute = v.min(59) as u8;
    }
    if let Some(v) = body.get("end_hour").and_then(|v| v.as_u64()) {
        config.end_hour = v.min(23) as u8;
    }
    if let Some(v) = body.get("end_minute").and_then(|v| v.as_u64()) {
        config.end_minute = v.min(59) as u8;
    }

    tracing::info!("Load limiter config updated: {:?}", config);

    // Persist to settings.json
    let mut app_settings = crate::settings::Settings::load();
    app_settings.load_limiter_enabled = config.enabled;
    app_settings.load_limiter_threshold_w = config.threshold_w;
    app_settings.load_limiter_trigger_delay_minutes = config.trigger_delay_minutes;
    app_settings.load_limiter_start_hour = config.start_hour;
    app_settings.load_limiter_start_minute = config.start_minute;
    app_settings.load_limiter_end_hour = config.end_hour;
    app_settings.load_limiter_end_minute = config.end_minute;
    drop(config);
    if let Err(e) = app_settings.save() {
        tracing::warn!("Failed to persist load limiter config: {e}");
        return server_error(&format!("Failed to save: {e}"));
    }

    ok_response("Load limiter config updated")
}

// ---------------------------------------------------------------------------
// Battery calibration endpoint (developer mode)
// ---------------------------------------------------------------------------

/// POST /api/control/calibration — set battery calibration stage.
pub async fn set_calibration(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
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
pub async fn reboot_inverter(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{with_isolated_config_dir, with_isolated_config_dir_async};

    #[test]
    fn three_phase_slot_selection_uses_three_phase_register_commands() {
        assert!(matches!(
            charge_slot_command_for_device(DeviceType::ThreePhase, 1, true, 130, 530).unwrap(),
            ControlCommand::SetThreePhaseChargeSlot1 {
                start: 130,
                end: 530
            }
        ));
        assert!(matches!(
            charge_slot_command_for_device(DeviceType::AioCommercial, 2, true, 600, 900).unwrap(),
            ControlCommand::SetThreePhaseChargeSlot2 {
                start: 600,
                end: 900
            }
        ));
        assert!(matches!(
            discharge_slot_command_for_device(DeviceType::ACThreePhase, 1, true, 1600, 1900)
                .unwrap(),
            ControlCommand::SetThreePhaseDischargeSlot1 {
                start: 1600,
                end: 1900
            }
        ));
        assert!(matches!(
            discharge_slot_command_for_device(DeviceType::HybridHvGen3, 2, true, 2000, 2230)
                .unwrap(),
            ControlCommand::SetThreePhaseDischargeSlot2 {
                start: 2000,
                end: 2230
            }
        ));
    }

    #[test]
    fn three_phase_slot_disable_clears_specific_slot_not_global_flag() {
        assert!(matches!(
            charge_slot_command_for_device(DeviceType::ThreePhase, 1, false, 130, 530).unwrap(),
            ControlCommand::SetThreePhaseChargeSlot1 { start: 0, end: 0 }
        ));
        assert!(matches!(
            discharge_slot_command_for_device(DeviceType::AllInOneHybrid, 2, false, 2000, 2230)
                .unwrap(),
            ControlCommand::SetThreePhaseDischargeSlot2 { start: 0, end: 0 }
        ));
        assert!(matches!(
            charge_slot_command_for_device(DeviceType::Gen3Hybrid, 1, false, 130, 530).unwrap(),
            ControlCommand::SetEnableCharge { enabled: false }
        ));
        assert!(matches!(
            discharge_slot_command_for_device(DeviceType::Gen3Hybrid, 1, false, 1600, 1900)
                .unwrap(),
            ControlCommand::SetDischargeSlot1 { start: 0, end: 0 }
        ));
    }

    #[test]
    fn slot_selection_keeps_existing_single_phase_and_extended_behaviour() {
        assert!(matches!(
            charge_slot_command_for_device(DeviceType::Gen3Hybrid, 1, true, 100, 500).unwrap(),
            ControlCommand::SetChargeSlot1 {
                start: 100,
                end: 500
            }
        ));
        assert!(matches!(
            charge_slot_command_for_device(DeviceType::Gen3Hybrid, 3, true, 2300, 30).unwrap(),
            ControlCommand::SetChargeSlotN {
                slot: 3,
                start: 2300,
                end: 30
            }
        ));
        assert!(matches!(
            discharge_slot_command_for_device(DeviceType::Gen3Hybrid, 2, true, 1600, 1900).unwrap(),
            ControlCommand::SetDischargeSlot2 {
                start: 1600,
                end: 1900
            }
        ));
        assert!(matches!(
            discharge_slot_command_for_device(DeviceType::Gen3Hybrid, 10, true, 2000, 2230)
                .unwrap(),
            ControlCommand::SetDischargeSlotN {
                slot: 10,
                start: 2000,
                end: 2230
            }
        ));
    }

    #[test]
    fn gen3_charge_slot2_uses_extended_register() {
        // Gen3Hybrid should route slot 2 to the extended block (HR 243-244)
        assert!(matches!(
            charge_slot_command_for_device(DeviceType::Gen3Hybrid, 2, true, 315, 415).unwrap(),
            ControlCommand::SetGen3ChargeSlot2 {
                start: 315,
                end: 415
            }
        ));
        // AIO models also use the extended block
        assert!(matches!(
            charge_slot_command_for_device(DeviceType::AllInOne5kW, 2, true, 100, 200).unwrap(),
            ControlCommand::SetGen3ChargeSlot2 {
                start: 100,
                end: 200
            }
        ));
        // HV Gen3 uses three-phase schedule slots, so slot 2 should NOT
        // use the Gen3 extended variant — it goes through 3ph dispatch.
        assert!(matches!(
            charge_slot_command_for_device(DeviceType::HybridHvGen3, 2, true, 300, 400).unwrap(),
            ControlCommand::SetThreePhaseChargeSlot2 {
                start: 300,
                end: 400
            }
        ));
        // Gen1/Gen2 should still use the classic register (HR 31-32)
        assert!(matches!(
            charge_slot_command_for_device(DeviceType::Gen1Hybrid, 2, true, 500, 600).unwrap(),
            ControlCommand::SetChargeSlot2 {
                start: 500,
                end: 600
            }
        ));
        assert!(matches!(
            charge_slot_command_for_device(DeviceType::Gen2Hybrid, 2, true, 700, 800).unwrap(),
            ControlCommand::SetChargeSlot2 {
                start: 700,
                end: 800
            }
        ));
    }

    /// Changing only the refresh rate must NOT bump the settings version —
    /// that would force a full TCP reconnect. The poll loop's sleep watcher
    /// picks up interval changes without dropping the connection.
    #[tokio::test]
    async fn interval_change_does_not_bump_version_or_disconnect() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());

            // Seed connection-affecting fields so the test isn't dependent on
            // whether the user has configured anything yet.
            {
                let mut s = state.settings.lock().await;
                s.host = "192.168.1.50".to_string();
                s.port = 8899;
                s.serial = "TEST".to_string();
                s.interval_secs = 60;
            }
            let version_before = state.settings.lock().await.version;

            // POST an interval-only update.
            let body = serde_json::json!({ "interval_secs": 20 });
            let _ = update_settings(State(state.clone()), Json(body)).await;

            let s = state.settings.lock().await;
            assert_eq!(s.interval_secs, 20, "interval should be applied");
            assert_eq!(
                s.version, version_before,
                "interval-only change must NOT bump version (would force reconnect)"
            );
        })
        .await;
    }

    /// Changing host/port/serial must bump the settings version so the poll
    /// loop tears down the TCP connection and reconnects to the new endpoint.
    #[tokio::test]
    async fn host_change_bumps_version_for_reconnect() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            {
                let mut s = state.settings.lock().await;
                s.host = "192.168.1.50".to_string();
                s.port = 8899;
                s.serial = "TEST".to_string();
                s.interval_secs = 60;
            }
            let version_before = state.settings.lock().await.version;

            let body = serde_json::json!({ "host": "192.168.1.99" });
            let _ = update_settings(State(state.clone()), Json(body)).await;

            let s = state.settings.lock().await;
            assert_eq!(s.host, "192.168.1.99");
            assert_eq!(
                s.version,
                version_before.wrapping_add(1),
                "host change must bump version (poll loop should reconnect)"
            );
        })
        .await;
    }
}
