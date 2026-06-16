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
use crate::modbus::registers::encode_hhmm;

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

/// Lock the latest snapshot once and resolve the current [`DeviceType`].
///
/// Every control handler that routes behaviour on device type MUST obtain it
/// through this helper (or [`device_type_flags`]) so each derived flag comes
/// from a single consistent view of the snapshot. Locking the snapshot
/// independently per check — e.g. once for AC-coupled, again for three-phase —
/// lets the poll loop update the snapshot between the two locks, so the flags
/// can disagree (both `false` on a race, or both `true` across a device-type
/// change) and the handler picks the wrong command/register set.
///
/// Defaults to [`DeviceType::Gen2Hybrid`] when no snapshot is available yet,
/// which preserves the previous "no snapshot → neither AC-coupled nor
/// three-phase" behaviour (`Gen2Hybrid` satisfies neither predicate).
async fn latest_device_type(state: &Arc<AppState>) -> DeviceType {
    state
        .latest_snapshot
        .lock()
        .await
        .as_ref()
        .map(|s| s.device_type)
        .unwrap_or(DeviceType::Gen2Hybrid)
}

/// Resolve the AC-coupled and three-phase routing flags from a single lock,
/// returning `(is_ac_coupled, is_three_phase)`.
///
/// `is_three_phase` takes priority over `is_ac_coupled` in the command
/// selection (matching the original `if is_three_phase { … } else if
/// is_ac_coupled { … }` ordering) — no real device is both, but computing them
/// from one locked view guarantees they can never transiently disagree.
///
/// Handlers that need the full [`DeviceType`] (e.g. for
/// `clear_discharge_slot_writes`) should call [`latest_device_type`] directly
/// instead of discarding the enum.
async fn device_type_flags(state: &Arc<AppState>) -> (bool, bool) {
    let dt = latest_device_type(state).await;
    (
        matches!(dt, DeviceType::ACCoupled | DeviceType::ACCoupledMk2),
        dt.uses_three_phase_schedule_slots(),
    )
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

/// Produce whitelist-validated register writes that clear both standard
/// discharge slots (1 and 2) by setting them to 00:00–00:00 (disabled).
///
/// Routes through the encoder's `SetDischargeSlot*` commands so every target
/// address is checked against `SAFE_WRITE_REGS`. Three-phase models write
/// HR 1118-1121; all others write the classic HR 44-45/56-57 pair. Use this
/// instead of constructing raw `RegisterWrite` structs, which would bypass
/// the encoder's whitelist validation (the security invariant that *all*
/// register writes must be validated by the encoder).
fn clear_discharge_slot_writes(device_type: DeviceType) -> Vec<RegisterWrite> {
    let mut out = Vec::new();
    for slot in [1u8, 2u8] {
        match discharge_slot_command_for_device(device_type, slot, false, 0, 0) {
            Ok(cmd) => match cmd.encode() {
                Ok(mut w) => out.append(&mut w),
                Err(e) => tracing::warn!("Failed to encode discharge slot {} clear: {}", slot, e),
            },
            Err(e) => tracing::warn!("Unsupported discharge slot {} on this model: {}", slot, e),
        }
    }
    out
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
            "evc_host": settings.evc_host,
            "evc_port": settings.evc_port,
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
    if let Some(evc_host) = body.get("evc_host").and_then(|v| v.as_str()) {
        persist.evc_host = evc_host.to_string();
    }
    if let Some(evc_port) = body.get("evc_port").and_then(|v| v.as_u64()) {
        persist.evc_port = evc_port.min(u16::MAX as u64) as u16;
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
    // Sync EVC settings from persisted config to in-memory PollSettings
    {
        let disk = crate::settings::Settings::load();
        settings.evc_host = disk.evc_host.clone();
        settings.evc_port = disk.evc_port;
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
        interval_secs,           // caller will merge: 0 means "keep existing"
        version: 0,              // not set by the API; caller bumps it
        evc_host: String::new(), // merged from disk settings separately
        evc_port: 502,
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
            // Three-phase models and Gateway use different slot addresses
            // (HR 1118-1121) than single-phase (HR 44-45/56-57).
            if mode_str == "eco" || mode_str == "eco_paused" {
                // Clear ALL discharge slot registers to prevent Gen3 inverter
                // firmware from auto-re-enabling enable_discharge. The Gen3
                // keeps HR59=1 when discharge slot registers are non-zero,
                // making it impossible to stay in Eco. Three-phase models use
                // different slot addresses (HR 1118-1121) than single-phase
                // (HR 44-45/56-57). Routed through the encoder's
                // whitelist-validated SetDischargeSlot* commands (00:00–00:00
                // = disabled) rather than raw writes.
                let device_type = latest_device_type(&state).await;
                writes.extend(clear_discharge_slot_writes(device_type));
            }

            // When switching to Timed mode, the frontend may include
            // discharge_slots that were configured locally in Eco mode.
            // Write them atomically BEFORE the enable_discharge flag so the
            // inverter never sees HR59=1 without slot constraints.
            if is_timed {
                if let Some(slots) = body["discharge_slots"].as_array() {
                    let device_type = latest_device_type(&state).await;

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
    let device_type = latest_device_type(&state).await;
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
                    // Clear the force-charge target flag (HR 20 = 0) so a stale
                    // flag from a previous operation doesn't keep
                    // snapshotForceCharge (enable_charge && enable_charge_target)
                    // asserted. Routed through the encoder so the address is
                    // checked against SAFE_WRITE_REGS, matching the invariant
                    // that *all* register writes are validated by the encoder.
                    if let Ok(flag_writes) =
                        (ControlCommand::ClearChargeTargetFlag).encode()
                    {
                        writes.extend(flag_writes);
                    }
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
    let device_type = latest_device_type(&state).await;
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

    let is_three_phase = latest_device_type(&state)
        .await
        .uses_three_phase_schedule_slots();
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

    let (is_ac_coupled, is_three_phase) = device_type_flags(&state).await;
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

    let (is_ac_coupled, is_three_phase) = device_type_flags(&state).await;
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

/// POST /api/control/pause — pause the battery (restore to Eco mode).
///
/// Disables discharge, restores self-consumption mode, clears any stale
/// discharge slot registers, and re-enables charge so solar can charge
/// the battery. This safely cancels an active ForceDischarge or ForceCharge
/// and returns the inverter to normal Eco self-consumption mode.
///
/// For three-phase models, also clears the three-phase force discharge
/// and force charge enable flags.
pub async fn pause_battery(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    // Resolve the device type with a single lock so `is_three_phase` and the
    // `device_type` used below are derived from the same snapshot view (a
    // previous version locked twice here — once for the flag and again for the
    // enum — which could race with the poll loop and disagree).
    let device_type = latest_device_type(&state).await;
    let is_three_phase = device_type.uses_three_phase_schedule_slots();
    let mut writes = match ControlCommand::PauseBattery.encode() {
        Ok(w) => w,
        Err(e) => return error_response(&format!("Validation error: {}", e)),
    };

    // Clear stale discharge slot registers to prevent Gen3 inverter
    // firmware from auto-re-enabling enable_discharge (the Gen3 keeps
    // HR59=1 when non-zero slot registers are present, which would
    // counteract the eco mode switch). Same pattern as set_mode("eco").
    // Routed through the encoder's whitelist-validated SetDischargeSlot*
    // commands (00:00–00:00 = disabled) rather than raw writes.
    writes.extend(clear_discharge_slot_writes(device_type));

    if is_three_phase {
        // Three-phase: clear force charge/discharge + AC charge flags and
        // restore eco (self-consumption) power mode in one validated batch
        // (ThreePhaseCosyExit encodes HR 1123/1112/1122 + HR 27). Avoids a
        // redundant HR 27 write that a separate SetBatteryPowerMode would add.
        writes.extend(ControlCommand::ThreePhaseCosyExit.encode().unwrap_or_default());
    } else {
        // Restore self-consumption (Eco) mode so the inverter doesn't stay
        // in export mode after a force discharge is cancelled.
        writes.extend(
            ControlCommand::SetBatteryPowerMode { mode: 1 }
                .encode()
                .unwrap_or_default(),
        );
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
    let is_three_phase = latest_device_type(&state)
        .await
        .uses_three_phase_schedule_slots();
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
    let is_three_phase = latest_device_type(&state)
        .await
        .uses_three_phase_schedule_slots();
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
    /// Explicit UTC epoch millisecond start boundary supplied by the browser.
    pub start_ms: Option<i64>,
    /// Explicit UTC epoch millisecond end boundary supplied by the browser.
    pub end_ms: Option<i64>,
}

/// GET /api/history — aggregated time-series data for charts.
///
/// Query params: `range`, `fields`, `offset`, `rolling`, optional `start_ms`/`end_ms`.
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
        if let (Some(start_ms), Some(end_ms)) = (params.start_ms, params.end_ms) {
            if start_ms >= end_ms {
                return error_response("Invalid history window: start_ms must be before end_ms");
            }
            // Convert browser-supplied UTC epoch milliseconds to the seconds used
            // by the SQLite readings table. The frontend computes calendar-day
            // boundaries in the user's local timezone and sends the absolute epoch
            // window so backend/server timezone cannot shift "Today" by an hour.
            let start_ts = start_ms.div_euclid(1000);
            let end_ts = (end_ms + 999).div_euclid(1000);
            Some((start_ts, end_ts))
        } else if rolling && range_str != "month" && range_str != "today" {
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
// EVC discovery endpoint
// ---------------------------------------------------------------------------

/// GET /api/evc/discover — scan the local network for EV chargers on port 502.
pub async fn evc_discover(State(_state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    tracing::info!("EVC network discovery requested");

    let subnets = crate::inverter::discovery::detect_lan_subnets();
    tracing::info!("EVC scanning subnets: {:?}", subnets);

    let chargers = crate::evc::scan_evc_multiple_subnets(&subnets).await;

    (
        StatusCode::OK,
        Json(json!({
        "ok": true,
        "subnets": subnets,
        "chargers": chargers,
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
    use crate::test_util::with_isolated_config_dir_async;

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

    /// `clear_discharge_slot_writes` must produce ONLY whitelist-validated
    /// register addresses (the security invariant fixed by routing the
    /// Eco/Pause slot-clearing through the encoder instead of raw writes).
    /// It must clear exactly the model-appropriate discharge slot pair.
    #[test]
    fn clear_discharge_slots_only_emits_whitelisted_addresses() {
        use crate::modbus::registers::{
            HR_3PH_DISCHARGE_SLOT_1_END, HR_3PH_DISCHARGE_SLOT_1_START,
            HR_3PH_DISCHARGE_SLOT_2_END, HR_3PH_DISCHARGE_SLOT_2_START,
            HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_END,
            HR_DISCHARGE_SLOT_2_START, SAFE_WRITE_REGS,
        };

        // Single-phase: classic HR 44-45 (slot 2) + HR 56-57 (slot 1).
        let writes = clear_discharge_slot_writes(DeviceType::Gen2Hybrid);
        assert_eq!(writes.len(), 4, "single-phase clears 2 slots x start/end");
        for w in &writes {
            assert_eq!(w.value, 0);
            assert!(
                SAFE_WRITE_REGS.contains(&w.address),
                "address {} must be whitelisted",
                w.address
            );
        }
        // Length is 4 and all 4 distinct single-phase slot registers are
        // present, so the set is exactly {44, 45, 56, 57}.
        let addrs: Vec<u16> = writes.iter().map(|w| w.address).collect();
        assert!(addrs.contains(&HR_DISCHARGE_SLOT_1_START));
        assert!(addrs.contains(&HR_DISCHARGE_SLOT_1_END));
        assert!(addrs.contains(&HR_DISCHARGE_SLOT_2_START));
        assert!(addrs.contains(&HR_DISCHARGE_SLOT_2_END));

        // Three-phase: HR 1118-1121.
        let writes = clear_discharge_slot_writes(DeviceType::ThreePhase);
        assert_eq!(writes.len(), 4, "three-phase clears 2 slots x start/end");
        for w in &writes {
            assert_eq!(w.value, 0);
            assert!(SAFE_WRITE_REGS.contains(&w.address));
        }
        let addrs: Vec<u16> = writes.iter().map(|w| w.address).collect();
        assert!(addrs.contains(&HR_3PH_DISCHARGE_SLOT_1_START));
        assert!(addrs.contains(&HR_3PH_DISCHARGE_SLOT_1_END));
        assert!(addrs.contains(&HR_3PH_DISCHARGE_SLOT_2_START));
        assert!(addrs.contains(&HR_3PH_DISCHARGE_SLOT_2_END));
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

    // -----------------------------------------------------------------------
    // Security invariant: every register write queued by the control API
    // must be validated against SAFE_WRITE_REGS by the encoder. These tests
    // drive the handlers end-to-end and assert no raw (unvalidated) writes
    // slip through — covering the Eco/Pause discharge-slot clearing and the
    // charge-slot force-charge-flag clearing.
    // -----------------------------------------------------------------------

    /// Seed `latest_snapshot` with a snapshot carrying the given device type
    /// and return a fresh `AppState` for exercising a control handler.
    async fn make_state_with_device(device_type: DeviceType) -> Arc<AppState> {
        let state = Arc::new(AppState::new());
        let snapshot = crate::inverter::model::InverterSnapshot {
            device_type,
            ..Default::default()
        };
        *state.latest_snapshot.lock().await = Some(snapshot);
        state
    }

    /// Drain the pending-writes queue and flatten the batches into one vec.
    async fn drain_pending_writes(state: &Arc<AppState>) -> Vec<RegisterWrite> {
        let mut pw = state.pending_writes.lock().await;
        let batches = std::mem::take(&mut *pw);
        drop(pw);
        batches.into_iter().flatten().collect()
    }

    fn assert_all_whitelisted(writes: &[RegisterWrite]) {
        use crate::modbus::registers::SAFE_WRITE_REGS;
        assert!(!writes.is_empty(), "handler should queue at least one write");
        for w in writes {
            assert!(
                SAFE_WRITE_REGS.contains(&w.address),
                "address {} not whitelisted (encoder bypass)",
                w.address
            );
        }
    }

    #[tokio::test]
    async fn set_charge_slot_only_emits_whitelisted_writes() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_ENABLE_CHARGE, HR_ENABLE_CHARGE_TARGET};
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 6, "start_minute": 0,
                "end_hour": 10, "end_minute": 0,
                "enabled": true,
            });
            let _ = set_charge_slot(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            // Enabling a single-phase charge slot must clear the stale
            // force-charge flag (HR_ENABLE_CHARGE_TARGET=0, via the encoder)
            // and set enable_charge=1 — both whitelisted.
            let target = writes.iter().find(|w| w.address == HR_ENABLE_CHARGE_TARGET);
            assert!(target.is_some(), "must clear HR_ENABLE_CHARGE_TARGET");
            assert_eq!(target.unwrap().value, 0);
            let enable = writes.iter().find(|w| w.address == HR_ENABLE_CHARGE);
            assert!(enable.is_some(), "must set HR_ENABLE_CHARGE");
            assert_eq!(enable.unwrap().value, 1);
        })
        .await;
    }

    #[tokio::test]
    async fn set_eco_mode_only_emits_whitelisted_writes() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START,
                HR_DISCHARGE_SLOT_2_END, HR_DISCHARGE_SLOT_2_START,
            };
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let body = serde_json::json!({ "mode": "eco", "soc_reserve": 10 });
            let _ = set_mode(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            // Eco mode clears all standard discharge slots via the encoder.
            for reg in [
                HR_DISCHARGE_SLOT_1_START,
                HR_DISCHARGE_SLOT_1_END,
                HR_DISCHARGE_SLOT_2_START,
                HR_DISCHARGE_SLOT_2_END,
            ] {
                let w = writes.iter().find(|w| w.address == reg);
                assert!(w.is_some(), "eco mode must clear discharge slot register {}", reg);
                assert_eq!(w.unwrap().value, 0);
            }
        })
        .await;
    }

    #[tokio::test]
    async fn pause_battery_single_phase_only_emits_whitelisted_writes() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_START,
            };
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let _ = pause_battery(State(state.clone())).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            // Pause clears discharge slots and restores eco power mode.
            assert!(
                writes.iter().any(|w| w.address == HR_DISCHARGE_SLOT_1_START && w.value == 0),
                "pause must clear discharge slot 1"
            );
            assert!(
                writes.iter().any(|w| w.address == HR_DISCHARGE_SLOT_2_START && w.value == 0),
                "pause must clear discharge slot 2"
            );
            assert!(
                writes.iter().any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1),
                "pause must restore eco power mode"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn pause_battery_three_phase_only_emits_whitelisted_writes() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_3PH_AC_CHARGE_ENABLE, HR_3PH_DISCHARGE_SLOT_1_START,
                HR_3PH_FORCE_CHARGE_ENABLE, HR_3PH_FORCE_DISCHARGE_ENABLE,
            };
            let state = make_state_with_device(DeviceType::ThreePhase).await;
            let _ = pause_battery(State(state.clone())).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            // Three-phase pause clears the HR 1118-1121 discharge slots via
            // the encoder and clears force flags via ThreePhaseCosyExit.
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_DISCHARGE_SLOT_1_START && w.value == 0),
                "three-phase pause must clear discharge slot 1"
            );
            assert!(
                writes.iter().any(|w| w.address == HR_3PH_FORCE_CHARGE_ENABLE && w.value == 0),
                "three-phase pause must clear force charge flag"
            );
            assert!(
                writes.iter().any(|w| w.address == HR_3PH_FORCE_DISCHARGE_ENABLE && w.value == 0),
                "three-phase pause must clear force discharge flag"
            );
            assert!(
                writes.iter().any(|w| w.address == HR_3PH_AC_CHARGE_ENABLE && w.value == 0),
                "three-phase pause must clear AC charge flag"
            );
        })
        .await;
    }

    // -----------------------------------------------------------------------
    // Device-type routing: every control handler must derive its AC-coupled /
    // three-phase flags from a SINGLE locked view of the snapshot (via
    // latest_device_type / device_type_flags) rather than two independent
    // locks that can race with the poll loop. These tests lock in the routing
    // per device family end-to-end and cover the helper defaults.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn latest_device_type_defaults_to_gen2hybrid_with_no_snapshot() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            // No snapshot seeded.
            assert_eq!(
                latest_device_type(&state).await,
                DeviceType::Gen2Hybrid,
                "no-snapshot default must be Gen2Hybrid (neither AC nor 3-phase)"
            );
            // The flags derived from that default are both false.
            let (ac, tp) = device_type_flags(&state).await;
            assert!(!ac);
            assert!(!tp);
        })
        .await;
    }

    #[tokio::test]
    async fn device_type_flags_matches_each_device_family() {
        with_isolated_config_dir_async(|| async {
            // (device, is_ac_coupled, is_three_phase)
            let cases = [
                (DeviceType::Gen2Hybrid, false, false),
                (DeviceType::Gen3Hybrid, false, false),
                (DeviceType::Gen1Hybrid, false, false),
                (DeviceType::ACCoupled, true, false),
                (DeviceType::ACCoupledMk2, true, false),
                (DeviceType::ThreePhase, false, true),
                (DeviceType::ACThreePhase, false, true),
                (DeviceType::AioCommercial, false, true),
                (DeviceType::HybridHvGen3, false, true),
                (DeviceType::AllInOneHybrid, false, true),
                (DeviceType::Gateway, false, true),
            ];
            for (dt, want_ac, want_tp) in cases {
                let state = make_state_with_device(dt).await;
                let (ac, tp) = device_type_flags(&state).await;
                assert_eq!(
                    (ac, tp),
                    (want_ac, want_tp),
                    "device_type_flags wrong for {:?}",
                    dt
                );
                // Consistency: the helper's flag must equal deriving it from
                // the same single-locked device type.
                let resolved = latest_device_type(&state).await;
                assert_eq!(ac, matches!(resolved, DeviceType::ACCoupled | DeviceType::ACCoupledMk2));
                assert_eq!(tp, resolved.uses_three_phase_schedule_slots());
            }
        })
        .await;
    }

    #[tokio::test]
    async fn set_charge_rate_routes_to_correct_register_per_device() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_3PH_BATTERY_CHARGE_LIMIT, HR_AC_BATTERY_CHARGE_LIMIT, HR_BATTERY_CHARGE_LIMIT,
            };
            // (device, expected register) — three-phase wins over AC priority.
            let cases = [
                (DeviceType::Gen2Hybrid, HR_BATTERY_CHARGE_LIMIT),
                (DeviceType::Gen3Hybrid, HR_BATTERY_CHARGE_LIMIT),
                (DeviceType::ACCoupled, HR_AC_BATTERY_CHARGE_LIMIT),
                (DeviceType::ACCoupledMk2, HR_AC_BATTERY_CHARGE_LIMIT),
                (DeviceType::ThreePhase, HR_3PH_BATTERY_CHARGE_LIMIT),
                (DeviceType::HybridHvGen3, HR_3PH_BATTERY_CHARGE_LIMIT),
            ];
            for (dt, want_reg) in cases {
                let state = make_state_with_device(dt).await;
                let body = serde_json::json!({ "limit": 30 });
                let _ = set_charge_rate(State(state.clone()), Json(body)).await;
                let writes = drain_pending_writes(&state).await;
                assert_all_whitelisted(&writes);
                assert_eq!(writes.len(), 1, "one register write expected for {:?}", dt);
                assert_eq!(
                    writes[0].address, want_reg,
                    "charge-rate routed to wrong register for {:?}",
                    dt
                );
                assert_eq!(writes[0].value, 30);
            }
        })
        .await;
    }

    #[tokio::test]
    async fn set_discharge_rate_routes_to_correct_register_per_device() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_3PH_BATTERY_DISCHARGE_LIMIT, HR_AC_BATTERY_DISCHARGE_LIMIT,
                HR_BATTERY_DISCHARGE_LIMIT,
            };
            let cases = [
                (DeviceType::Gen2Hybrid, HR_BATTERY_DISCHARGE_LIMIT),
                (DeviceType::Gen3Hybrid, HR_BATTERY_DISCHARGE_LIMIT),
                (DeviceType::ACCoupled, HR_AC_BATTERY_DISCHARGE_LIMIT),
                (DeviceType::ACCoupledMk2, HR_AC_BATTERY_DISCHARGE_LIMIT),
                (DeviceType::ThreePhase, HR_3PH_BATTERY_DISCHARGE_LIMIT),
                (DeviceType::AllInOneHybrid, HR_3PH_BATTERY_DISCHARGE_LIMIT),
            ];
            for (dt, want_reg) in cases {
                let state = make_state_with_device(dt).await;
                let body = serde_json::json!({ "limit": 25 });
                let _ = set_discharge_rate(State(state.clone()), Json(body)).await;
                let writes = drain_pending_writes(&state).await;
                assert_all_whitelisted(&writes);
                assert_eq!(writes.len(), 1, "one register write expected for {:?}", dt);
                assert_eq!(writes[0].address, want_reg, "wrong discharge-rate register for {:?}", dt);
                assert_eq!(writes[0].value, 25);
            }
        })
        .await;
    }

    #[tokio::test]
    async fn set_reserve_routes_single_phase_vs_three_phase() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_3PH_BATTERY_SOC_RESERVE, HR_BATTERY_SOC_RESERVE};
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let _ = set_reserve(State(state.clone()), Json(json!({ "soc": 20 }))).await;
            let writes = drain_pending_writes(&state).await;
            assert_eq!(writes[0].address, HR_BATTERY_SOC_RESERVE);

            let state = make_state_with_device(DeviceType::ThreePhase).await;
            let _ = set_reserve(State(state.clone()), Json(json!({ "soc": 20 }))).await;
            let writes = drain_pending_writes(&state).await;
            assert_eq!(writes[0].address, HR_3PH_BATTERY_SOC_RESERVE);
        })
        .await;
    }

    #[tokio::test]
    async fn force_charge_routes_single_phase_vs_three_phase() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_3PH_CHARGE_TARGET_SOC, HR_CHARGE_TARGET_SOC};
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let _ = force_charge(State(state.clone())).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes.iter().any(|w| w.address == HR_CHARGE_TARGET_SOC),
                "single-phase force charge must target HR_CHARGE_TARGET_SOC"
            );

            let state = make_state_with_device(DeviceType::ThreePhase).await;
            let _ = force_charge(State(state.clone())).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes.iter().any(|w| w.address == HR_3PH_CHARGE_TARGET_SOC),
                "three-phase force charge must target HR_3PH_CHARGE_TARGET_SOC"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_discharge_routes_single_phase_vs_three_phase() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_3PH_FORCE_DISCHARGE_ENABLE, HR_ENABLE_DISCHARGE,
            };
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let _ = force_discharge(State(state.clone())).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes.iter().any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 1),
                "single-phase force discharge must set HR_ENABLE_DISCHARGE=1"
            );

            let state = make_state_with_device(DeviceType::ThreePhase).await;
            let _ = force_discharge(State(state.clone())).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes.iter().any(|w| w.address == HR_3PH_FORCE_DISCHARGE_ENABLE && w.value == 1),
                "three-phase force discharge must set HR_3PH_FORCE_DISCHARGE_ENABLE=1"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn pause_battery_uses_consistent_device_type_for_slot_and_mode() {
        // Regression guard for the pause_battery race: the discharge-slot
        // clearing and the three-phase vs single-phase mode restore must both
        // come from the SAME locked device-type view. We assert the
        // single-phase and three-phase paths each produce a self-consistent
        // batch (correct slot-clear registers AND the correct power-mode path).
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_3PH_DISCHARGE_SLOT_1_START, HR_BATTERY_POWER_MODE,
                HR_DISCHARGE_SLOT_1_START,
            };
            // Single-phase: classic slots cleared + eco power mode restored.
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let _ = pause_battery(State(state.clone())).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes.iter().any(|w| w.address == HR_DISCHARGE_SLOT_1_START && w.value == 0),
                "single-phase pause clears HR 56 slot 1"
            );
            assert!(
                writes.iter().any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1),
                "single-phase pause restores eco power mode"
            );

            // Three-phase: HR 1118 slots cleared (NOT the single-phase HR 56) —
            // proves the slot-clear and the mode-restore saw the same device
            // type rather than disagreeing.
            let state = make_state_with_device(DeviceType::ThreePhase).await;
            let _ = pause_battery(State(state.clone())).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes.iter().any(|w| w.address == HR_3PH_DISCHARGE_SLOT_1_START && w.value == 0),
                "three-phase pause clears HR 1118 slot 1"
            );
            assert!(
                !writes.iter().any(|w| w.address == HR_DISCHARGE_SLOT_1_START),
                "three-phase pause must NOT touch single-phase slot registers"
            );
            assert!(
                writes.iter().any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1),
                "three-phase pause restores eco power mode"
            );
        })
        .await;
    }
}
