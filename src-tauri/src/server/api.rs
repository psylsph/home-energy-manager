//! REST API routes and handlers.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use chrono::{Datelike, Duration as ChronoDuration, Local, TimeZone, Timelike};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::inverter::encoder::{ControlCommand, RegisterWrite};
use crate::inverter::model::DeviceType;
use crate::inverter::poll::{AppState, ForceChargeRevert, ForceDischargeRevert, PollSettings};
use crate::settings::TariffConfig;
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
    // When `enabled` is false, force the slot times to the (0, 0) sentinel
    // before dispatch so the matching arm below emits a slot-register write
    // with start/end both zero. This mirrors how `discharge_slot_command_for_device`
    // always writes the slot registers (start, end) on disable, and is the
    // prerequisite for the slot toggling to round-trip through the UI:
    // leaving the previous times in HR 94/95 (or HR 243/244 on Gen3/AIO)
    // makes the next decode see the slot as configured, so the UI shows the
    // toggle as ON again after the user turned it OFF.
    let (start, end) = if enabled { (start, end) } else { (0, 0) };
    match (device_type.uses_three_phase_schedule_slots(), slot) {
        (true, 1) => Ok(ControlCommand::SetThreePhaseChargeSlot1 { start, end }),
        (true, 2) => Ok(ControlCommand::SetThreePhaseChargeSlot2 { start, end }),
        (true, 3..=10) => Ok(ControlCommand::SetChargeSlotN { slot, start, end }),
        (false, 1) => Ok(ControlCommand::SetChargeSlot1 { start, end }),
        // Gen3/AIO/HV-Gen3 use HR 243-244 for charge slot 2 (the extended-block
        // copy is authoritative on these models; classic HR 31-32 may be stale).
        (false, 2) if device_type.supports_gen3_extended() => {
            Ok(ControlCommand::SetGen3ChargeSlot2 { start, end })
        }
        (false, 2) => Ok(ControlCommand::SetChargeSlot2 { start, end }),
        (false, 3..=10) => Ok(ControlCommand::SetChargeSlotN { slot, start, end }),
        (_, _) => Err(format!("Unsupported charge slot {}", slot)),
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

fn reserve_writes_for_device(
    device_type: DeviceType,
    reserve: u16,
) -> Result<Vec<RegisterWrite>, String> {
    let cmd = if device_type.uses_three_phase_schedule_slots() {
        ControlCommand::SetThreePhaseBatterySocReserve { reserve }
    } else {
        ControlCommand::SetBatterySocReserve { reserve }
    };
    cmd.encode()
}

fn force_charge_slot_writes(
    device_type: DeviceType,
    minutes: u64,
) -> Result<Vec<RegisterWrite>, String> {
    let minutes = minutes.clamp(1, 1439);
    let start = Local::now();
    let end = start + ChronoDuration::minutes(minutes as i64);
    let start_hhmm = encode_hhmm(start.hour() as u8, start.minute() as u8);
    let end_hhmm = encode_hhmm(end.hour() as u8, end.minute() as u8);
    charge_slot_command_for_device(device_type, 1, true, start_hhmm, end_hhmm)?.encode()
}

/// Build the discharge-slot writes for a duration-limited Force Discharge.
///
/// Writes discharge slot 1 to `now → now + minutes` (so the inverter has
/// a finite window to discharge through) and clears discharge slot 2 (the
/// force-discharge encoder's default behaviour, but we have to re-state
/// it here because the encoder's slot writes are filtered out on the
/// minutes path). Mirrors GivTCP's `forceExport` (`write.py:1015-1019`),
/// which writes `discharge_slot_1=TimeSlot{now, now+exportTime}` before
/// arming the force-discharge flag.
///
/// Used only on the `minutes` body path. The no-body path falls through
/// to `ControlCommand::ForceDischarge`/`ThreePhaseForceDischarge`, which
/// writes a 00:00–23:59 slot (effectively "until stopped").
fn force_discharge_slot_writes(
    device_type: DeviceType,
    minutes: u64,
) -> Result<Vec<RegisterWrite>, String> {
    let minutes = minutes.clamp(1, 1439);
    let start = Local::now();
    let end = start + ChronoDuration::minutes(minutes as i64);
    let start_hhmm = encode_hhmm(start.hour() as u8, start.minute() as u8);
    let end_hhmm = encode_hhmm(end.hour() as u8, end.minute() as u8);
    let mut out = Vec::new();
    // Slot 1: now → now+minutes
    match discharge_slot_command_for_device(device_type, 1, true, start_hhmm, end_hhmm) {
        Ok(cmd) => match cmd.encode() {
            Ok(mut w) => out.append(&mut w),
            Err(e) => {
                return Err(format!("Failed to encode discharge slot 1: {}", e));
            }
        },
        Err(e) => return Err(format!("Unsupported discharge slot 1: {}", e)),
    }
    // Slot 2: clear (the no-body encoder path clears it; we have to do
    // the same here because we filter the encoder's slot writes on the
    // minutes path).
    match discharge_slot_command_for_device(device_type, 2, false, 0, 0) {
        Ok(cmd) => match cmd.encode() {
            Ok(mut w) => out.append(&mut w),
            Err(e) => return Err(format!("Failed to encode discharge slot 2 clear: {}", e)),
        },
        Err(e) => return Err(format!("Unsupported discharge slot 2: {}", e)),
    }
    Ok(out)
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

/// Capture the pre-force-charge state of the inverter into a `ForceChargeRevert`
/// so the Stop Charge endpoint can restore the inverter to its prior
/// configuration. Mirrors GivTCP's `revert` dict in `forceCharge` (write.py:1148).
///
/// Returns `None` if no snapshot is available yet (degenerate case — the user
/// clicked Force Charge before the first poll completed). In that case the
/// stop path will refuse to run rather than try to restore unknown state.
async fn capture_force_charge_revert(
    state: &Arc<AppState>,
    device_type: DeviceType,
) -> Option<ForceChargeRevert> {
    let snap_arc = state.latest_snapshot.clone();
    let snap = snap_arc.lock().await;
    let snap = snap.as_ref()?;

    let slot = &snap.charge_slots[0];
    // Only remember the slot if it was actually enabled. An unconfigured
    // (00:00–00:00) slot is a no-op to restore to, so we capture None
    // and the restore path will clear the slot with (0,0).
    let charge_slot_1_start = if slot.enabled {
        Some((slot.start_hour, slot.start_minute))
    } else {
        None
    };
    let charge_slot_1_end = if slot.enabled {
        Some((slot.end_hour, slot.end_minute))
    } else {
        None
    };

    let (three_phase_force_charge_enable, three_phase_ac_charge_enable) =
        if device_type.uses_three_phase_schedule_slots() {
            // For 3PH, `enable_charge` is the OR of HR 1112 and HR 1123.
            // We can't read them individually from the snapshot (only the
            // combined `enable_charge` is stored), but we approximate by
            // reading the raw input blocks if present. Simplest sound default:
            // treat `enable_charge=true` as "both might be set" and clear
            // both on restore. The poll loop will resync from the inverter
            // on the next cycle so any value we don't know is overwritten
            // with the live reading.
            let was_enabled = snap.enable_charge;
            (Some(was_enabled), Some(was_enabled))
        } else {
            (None, None)
        };

    Some(ForceChargeRevert {
        enable_charge: snap.enable_charge,
        enable_discharge: snap.enable_discharge,
        target_soc: snap.target_soc,
        battery_power_mode: snap.battery_power_mode,
        charge_rate: Some(snap.charge_rate),
        charge_slot_1_start,
        charge_slot_1_end,
        three_phase_force_charge_enable,
        three_phase_ac_charge_enable,
        // AC-coupled models populate `battery_pause_mode` (HR 318). On other
        // models the field defaults to 0, which is the "no pause" state and
        // matches the typical pre-force-charge value, so we capture it
        // unconditionally and skip the write if it's 0 to avoid touching
        // the register on models that don't use it.
        battery_pause_mode: Some(snap.battery_pause_mode),
    })
}

/// Build the writes that restore the inverter to a captured `ForceChargeRevert`.
/// Mirrors GivTCP's `FCResume` (`write.py:1042-1091`).
fn build_force_charge_stop_writes(
    device_type: DeviceType,
    revert: &ForceChargeRevert,
) -> Vec<RegisterWrite> {
    use crate::modbus::registers::{
        HR_3PH_AC_CHARGE_ENABLE, HR_3PH_FORCE_CHARGE_ENABLE, HR_BATTERY_POWER_MODE,
        HR_CHARGE_SLOT_1_END, HR_CHARGE_SLOT_1_START, HR_CHARGE_TARGET_SOC,
        HR_ENABLE_CHARGE, HR_ENABLE_CHARGE_TARGET, HR_ENABLE_DISCHARGE,
    };
    let mut writes = Vec::new();

    if device_type.uses_three_phase_schedule_slots() {
        // Three-phase path: clear the force-charge and AC-charge enable flags
        // and restore the battery power mode. The target SOC, charge rate,
        // and slot registers are read from the HR 1080-1124 block, so the
        // poll loop will resync them naturally. The user can also set them
        // via the normal controls.
        writes.push(RegisterWrite {
            address: HR_3PH_FORCE_CHARGE_ENABLE,
            value: if revert.three_phase_force_charge_enable.unwrap_or(false) { 1 } else { 0 },
        });
        writes.push(RegisterWrite {
            address: HR_3PH_AC_CHARGE_ENABLE,
            value: if revert.three_phase_ac_charge_enable.unwrap_or(false) { 1 } else { 0 },
        });
        // Restore the pre-force-charge power mode (0 = export, 1 = eco).
        // ThreePhaseForceCharge start writes 1; a user in Max-Power /
        // Timed Export before force-charge would otherwise be stuck in
        // eco after stop. Clamp defensively.
        let mode = revert.battery_power_mode.min(1);
        writes.push(RegisterWrite {
            address: HR_BATTERY_POWER_MODE,
            value: mode as u16,
        });
    } else {
        // Single-phase / AC-coupled path: restore all the captured values.
        writes.push(RegisterWrite {
            address: HR_ENABLE_DISCHARGE,
            value: if revert.enable_discharge { 1 } else { 0 },
        });
        writes.push(RegisterWrite {
            address: HR_ENABLE_CHARGE,
            value: if revert.enable_charge { 1 } else { 0 },
        });
        writes.push(RegisterWrite {
            address: HR_ENABLE_CHARGE_TARGET,
            value: if revert.enable_charge { 1 } else { 0 },
        });
        writes.push(RegisterWrite {
            address: HR_CHARGE_TARGET_SOC,
            value: revert.target_soc as u16,
        });
        // Restore the battery power mode (HR 27) to its pre-force-charge
        // value (0 = export, 1 = eco). `ForceCharge` start writes 1, so a
        // user in Max-Power / Timed Export before force-charge would
        // otherwise be stuck in eco after stop. Clamp to {0,1} defensively
        // — the encoder only accepts those values, and the snapshot should
        // only ever contain them, but a corrupted snapshot should not
        // produce an out-of-range register write.
        let mode = revert.battery_power_mode.min(1);
        writes.push(RegisterWrite {
            address: HR_BATTERY_POWER_MODE,
            value: mode as u16,
        });

        // Restore the original charge slot. If there was no prior slot,
        // clear it with (00:00, 00:00). The encoder treats this as the
        // explicit "no slot" value.
        let (start_h, start_m) = revert.charge_slot_1_start.unwrap_or((0, 0));
        let (end_h, end_m) = revert.charge_slot_1_end.unwrap_or((0, 0));
        let start_hhmm = encode_hhmm(start_h, start_m);
        let end_hhmm = encode_hhmm(end_h, end_m);
        writes.push(RegisterWrite {
            address: HR_CHARGE_SLOT_1_START,
            value: start_hhmm,
        });
        writes.push(RegisterWrite {
            address: HR_CHARGE_SLOT_1_END,
            value: end_hhmm,
        });
    }

    writes
}

/// Capture the pre-force-discharge state of the inverter into a
/// `ForceDischargeRevert` so the Stop Discharge endpoint can restore the
/// inverter to its prior configuration. Mirrors GivTCP's `revert` dict in
/// `forceExport` (`write.py:980-1010`).
///
/// Returns `None` if no snapshot is available yet (degenerate case — the
/// user clicked Force Discharge before the first poll completed). In that
/// case the stop path will refuse to run rather than try to restore
/// unknown state.
async fn capture_force_discharge_revert(
    state: &Arc<AppState>,
    device_type: DeviceType,
) -> Option<ForceDischargeRevert> {
    let snap = state.latest_snapshot.lock().await;
    let snap = snap.as_ref()?;

    // `(start HHMM, end HHMM)` for a discharge slot, or `None` for both if
    // the slot was unconfigured (write 00:00–00:00 to clear).
    type SlotTimes = (Option<(u8, u8)>, Option<(u8, u8)>);
    let capture_slot = |slot: &crate::inverter::model::ScheduleSlot| -> SlotTimes {
        if slot.enabled {
            (Some((slot.start_hour, slot.start_minute)), Some((slot.end_hour, slot.end_minute)))
        } else {
            (None, None)
        }
    };
    let (d1_start, d1_end) = capture_slot(&snap.discharge_slots[0]);
    let (d2_start, d2_end) = capture_slot(&snap.discharge_slots[1]);

    let (three_phase_force_discharge_enable, three_phase_force_charge_enable) =
        if device_type.uses_three_phase_schedule_slots() {
            // For 3PH the snapshot doesn't store the individual HR 1122/1123
            // values — only the derived `enable_discharge`/`enable_charge`
            // booleans. We approximate by treating `enable_discharge` as the
            // prior force-discharge state and clearing force-charge on stop
            // (the encoder always writes force_charge=0, so on stop we
            // restore the prior force_charge state inferred from
            // `enable_charge`).
            (Some(snap.enable_discharge), Some(snap.enable_charge))
        } else {
            (None, None)
        };

    Some(ForceDischargeRevert {
        enable_charge: snap.enable_charge,
        enable_discharge: snap.enable_discharge,
        discharge_rate: Some(snap.discharge_rate),
        discharge_slot_1_start: d1_start,
        discharge_slot_1_end: d1_end,
        discharge_slot_2_start: d2_start,
        discharge_slot_2_end: d2_end,
        three_phase_force_discharge_enable,
        three_phase_force_charge_enable,
        // Set by the API handler when the timed (minutes-bounded) path
        // is used, after the capture is taken. The poll loop reads this
        // field to auto-revert when the slot window expires (issue #129).
        force_discharge_slot_end_ms: None,
    })
}

/// Build the writes that restore the inverter to a captured `ForceDischargeRevert`.
/// Mirrors GivTCP's `FEResume` (`write.py:1042`-ish, adapted for the
/// force-discharge case which GivTCP rolls into `forceExport`).
fn build_force_discharge_stop_writes(
    device_type: DeviceType,
    revert: &ForceDischargeRevert,
) -> Vec<RegisterWrite> {
    use crate::modbus::registers::{
        HR_3PH_FORCE_CHARGE_ENABLE, HR_3PH_FORCE_DISCHARGE_ENABLE, HR_BATTERY_POWER_MODE,
        HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_END,
        HR_DISCHARGE_SLOT_2_START, HR_ENABLE_CHARGE, HR_ENABLE_CHARGE_TARGET,
        HR_ENABLE_DISCHARGE,
    };
    let mut writes = Vec::new();

    if device_type.uses_three_phase_schedule_slots() {
        // Three-phase path: restore the force-discharge and force-charge
        // enable flags, then return to eco mode. The poll loop will resync
        // the slot registers from the HR 1080-1124 block.
        writes.push(RegisterWrite {
            address: HR_3PH_FORCE_DISCHARGE_ENABLE,
            value: if revert.three_phase_force_discharge_enable.unwrap_or(false) { 1 } else { 0 },
        });
        writes.push(RegisterWrite {
            address: HR_3PH_FORCE_CHARGE_ENABLE,
            value: if revert.three_phase_force_charge_enable.unwrap_or(false) { 1 } else { 0 },
        });
        writes.push(RegisterWrite {
            address: HR_BATTERY_POWER_MODE,
            value: 1,
        });
    } else {
        // Single-phase / AC-coupled path: restore all the captured values.
        writes.push(RegisterWrite {
            address: HR_ENABLE_DISCHARGE,
            value: if revert.enable_discharge { 1 } else { 0 },
        });
        writes.push(RegisterWrite {
            address: HR_ENABLE_CHARGE,
            value: if revert.enable_charge { 1 } else { 0 },
        });
        writes.push(RegisterWrite {
            address: HR_ENABLE_CHARGE_TARGET,
            value: if revert.enable_charge { 1 } else { 0 },
        });

        // Restore the original discharge slots. If there was no prior slot,
        // clear with (00:00, 00:00). The encoder treats this as the
        // explicit "no slot" value.
        let (s1h, s1m) = revert.discharge_slot_1_start.unwrap_or((0, 0));
        let (e1h, e1m) = revert.discharge_slot_1_end.unwrap_or((0, 0));
        writes.push(RegisterWrite {
            address: HR_DISCHARGE_SLOT_1_START,
            value: encode_hhmm(s1h, s1m),
        });
        writes.push(RegisterWrite {
            address: HR_DISCHARGE_SLOT_1_END,
            value: encode_hhmm(e1h, e1m),
        });
        let (s2h, s2m) = revert.discharge_slot_2_start.unwrap_or((0, 0));
        let (e2h, e2m) = revert.discharge_slot_2_end.unwrap_or((0, 0));
        writes.push(RegisterWrite {
            address: HR_DISCHARGE_SLOT_2_START,
            value: encode_hhmm(s2h, s2m),
        });
        writes.push(RegisterWrite {
            address: HR_DISCHARGE_SLOT_2_END,
            value: encode_hhmm(e2h, e2m),
        });

        // Default to eco (1) on restore. `battery_power_mode` is not in the
        // snapshot (only in the raw decoder config), so we can't restore
        // the exact pre-state. The user can re-set via the mode buttons
        // if they need max-power (0) or timed (2). Matches `pause_battery`'s
        // behaviour of always returning to eco.
        writes.push(RegisterWrite {
            address: HR_BATTERY_POWER_MODE,
            value: 1,
        });
    }

    writes
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

/// GET /api/status — current connection state, timing info, and LAN IP
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

    // Connection timestamp (epoch millis) and consecutive failure count.
    let cs_val = state
        .connected_since
        .lock()
        .ok()
        .and_then(|guard| *guard);
    let connected_since_epoch_ms = cs_val
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64);
    let connect_failures: u32 = state
        .connect_failures
        .load(std::sync::atomic::Ordering::Relaxed);

    (
        StatusCode::OK,
        Json(json!({
        "ok": true,
        "connection": cs,
        "host": host,
        "lan_ip": lan_ip,
        "clients": client_addrs,
        "client_count": client_count,
        "connected_since_epoch_ms": connected_since_epoch_ms,
        "connect_failures": connect_failures,
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
            "disable_auto_discovery": settings.disable_auto_discovery,
            "minimal_telemetry_mode": settings.minimal_telemetry_mode,
            "autostart_enabled": settings.autostart_enabled,
            "api_key": settings.api_key,
            "api_port": settings.api_port,
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

    // Update tariff config objects if provided. Server-side validation
    // rejects any malformed or invalid config with a 400 — we never silently
    // replace with defaults because that would lose the user's edits without
    // explanation. The UI also validates before posting, so this is defence
    // in depth (hand-edited settings.json, direct API calls, etc.).
    let import_tariff_config =
        match parse_and_validate_tariff(body.get("import_tariff_config"), "import_tariff_config") {
            Ok(v) => v,
            Err(e) => return error_response(&e),
        };
    let import_tariff_config = import_tariff_config.or(import_tariff_config_default);
    let export_tariff_config =
        match parse_and_validate_tariff(body.get("export_tariff_config"), "export_tariff_config") {
            Ok(v) => v,
            Err(e) => return error_response(&e),
        };
    let export_tariff_config = export_tariff_config.or(export_tariff_config_default);

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
    if let Some(d) = body.get("disable_auto_discovery").and_then(|v| v.as_bool()) {
        persist.disable_auto_discovery = d;
    }
    if let Some(m) = body.get("minimal_telemetry_mode").and_then(|v| v.as_bool()) {
        persist.minimal_telemetry_mode = m;
    }
    // Persist the user's autostart preference. The actual platform
    // autostart entry is driven from the frontend via the
    // @tauri-apps/plugin-autostart JS bindings (so the toast and
    // toggling happen client-side), and the startup self-heal path in
    // lib.rs re-applies it after a crash/restart. See issue #117.
    if let Some(a) = body.get("autostart_enabled").and_then(|v| v.as_bool()) {
        persist.autostart_enabled = a;
    }
    // Persist the read-only API key and port. The read-only server is
    // started/stopped on the next app launch (no hot-reload of the
    // second server). An empty key disables the read-only server.
    if let Some(k) = body.get("api_key").and_then(|v| v.as_str()) {
        persist.api_key = k.to_string();
    }
    if let Some(p) = body.get("api_port").and_then(|v| v.as_u64()) {
        persist.api_port = p.min(u16::MAX as u64) as u16;
    }
    if let Err(e) = persist.save() {
        tracing::warn!("Failed to persist settings: {}", e);
        return server_error(&format!("Failed to save settings: {}", e));
    }

    // Build the log/response message before the in-memory state update
    // (and before `persist` is dropped) so it reflects the values that
    // were actually written to disk. The previous format hard-coded the
    // four connection fields, which produced a misleading
    // `host=, port=0, serial=, interval=0s` line for every non-connection
    // save (tariffs, read-only API key, panel visibility, etc.).
    let fields = settings_log_fields(&body, &persist);
    let msg = if fields.is_empty() {
        "Settings updated: (no fields in request body)".to_string()
    } else {
        format!("Settings updated: {}", fields.join(", "))
    };
    tracing::info!("{}", msg);
    let response = ok_response(&msg);

    // Now that disk is updated, apply changes to the in-memory state.
    // Lock is held briefly — no file I/O while holding it.
    drop(persist);

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
    // Sync EVC settings + auto-discovery flag from persisted config to in-memory PollSettings
    {
        let disk = crate::settings::Settings::load();
        settings.evc_host = disk.evc_host.clone();
        settings.evc_port = disk.evc_port;
        settings.disable_auto_discovery = disk.disable_auto_discovery;
        settings.minimal_telemetry_mode = disk.minimal_telemetry_mode;
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

    response
}

/// Build a list of `key=value` strings for the fields that were actually
/// present in the request body, using the **persisted** values from
/// `persist` (so the log reflects what was just written to disk, not
/// the raw request body).
///
/// The previous hard-coded log always printed host/port/serial/interval_secs
/// with empty defaults when those fields weren't in the body, which made
/// every non-connection save (tariffs, read-only API key, panel visibility,
/// etc.) look like an empty connection update.
///
/// The read-only API key is **redacted** — its plaintext value never
/// appears in the log, only whether it was set/cleared and its length.
/// This prevents secrets from being captured in log files sent via the
/// in-app support bundle feature.
fn settings_log_fields(
    body: &serde_json::Value,
    persist: &crate::settings::Settings,
) -> Vec<String> {
    let mut out = Vec::new();
    let is_present = |key: &str| body.get(key).is_some();

    if is_present("host") {
        out.push(format!("host={}", persist.host));
    }
    if is_present("port") {
        out.push(format!("port={}", persist.port));
    }
    if is_present("serial") {
        out.push(format!("serial={}", persist.serial));
    }
    if is_present("interval_secs") {
        out.push(format!("interval={}s", persist.poll_interval));
    }
    if is_present("import_tariff") {
        out.push(format!("import_tariff={}", persist.import_tariff));
    }
    if is_present("export_tariff") {
        out.push(format!("export_tariff={}", persist.export_tariff));
    }
    if is_present("import_tariff_config") {
        let slots = persist
            .import_tariff_config
            .as_ref()
            .map(|c| c.slots.len())
            .unwrap_or(0);
        out.push(format!("import_tariff_config={} slots", slots));
    }
    if is_present("export_tariff_config") {
        let slots = persist
            .export_tariff_config
            .as_ref()
            .map(|c| c.slots.len())
            .unwrap_or(0);
        out.push(format!("export_tariff_config={} slots", slots));
    }
    if is_present("http_port") {
        out.push(format!("http_port={}", persist.http_port));
    }
    if is_present("hidden_panels") {
        out.push(format!("hidden_panels={} entries", persist.hidden_panels.len()));
    }
    if is_present("evc_host") {
        out.push(format!("evc_host={}", persist.evc_host));
    }
    if is_present("evc_port") {
        out.push(format!("evc_port={}", persist.evc_port));
    }
    if is_present("disable_auto_discovery") {
        out.push(format!(
            "disable_auto_discovery={}",
            persist.disable_auto_discovery
        ));
    }
    if is_present("minimal_telemetry_mode") {
        out.push(format!(
            "minimal_telemetry_mode={}",
            persist.minimal_telemetry_mode
        ));
    }
    if is_present("autostart_enabled") {
        out.push(format!("autostart_enabled={}", persist.autostart_enabled));
    }
    if is_present("api_key") {
        // Redact the key value — never log the plaintext. The length
        // gives the user enough information to verify they pasted
        // the right key without leaking it into log files.
        if persist.api_key.is_empty() {
            out.push("api_key=cleared".to_string());
        } else {
            out.push(format!("api_key=set ({} chars)", persist.api_key.len()));
        }
    }
    if is_present("api_port") {
        out.push(format!("api_port={}", persist.api_port));
    }
    out
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

    let disable_auto_discovery = body
        .get("disable_auto_discovery")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    Ok(PollSettings {
        host,
        port,
        serial,
        interval_secs,          // caller will merge: 0 means "keep existing"
        version: 0,             // not set by the API; caller bumps it
        evc_host: String::new(),// merged from disk settings separately
        evc_port: 502,
        disable_auto_discovery,
        minimal_telemetry_mode: body
            .get("minimal_telemetry_mode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

/// Parse and validate an optional tariff config from a request body field.
///
/// Returns `Ok(None)` when the field is missing or `null` (the caller should
/// fall back to the existing on-disk value). Returns `Err(msg)` when the
/// field is present but malformed or fails validation — the message is
/// surfaced to the user as a 400 response.
///
/// Validation enforces (see [`TariffConfig::validate`]):
/// - non-empty slot list
/// - all slots parse as `HH:MM` in `[00:00, 23:59]`
/// - rates are finite and non-negative
/// - first slot starts at `00:00`, last slot ends at `23:59`
/// - contiguous, non-overlapping tiling of the full day
fn parse_and_validate_tariff(
    value: Option<&serde_json::Value>,
    field_name: &str,
) -> Result<Option<TariffConfig>, String> {
    let Some(v) = value else {
        return Ok(None);
    };
    if v.is_null() {
        return Ok(None);
    }
    let cfg = serde_json::from_value::<TariffConfig>(v.clone())
        .map_err(|e| format!("{field_name} is malformed: {e}"))?;
    cfg.validate()
        .map_err(|e| format!("{field_name} invalid: {e}"))?;
    Ok(Some(cfg))
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
        "export_paused" => ControlCommand::SetExportPaused { soc_reserve },
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
            if mode_str == "eco" || mode_str == "eco_paused" || mode_str == "export_paused" {
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
                    if target_soc >= 100 {
                        // Charge to full ("charge to 100%" / default): no
                        // target limit is needed, so clear the charge target
                        // flag so a stale force-charge flag from a previous
                        // operation doesn't keep snapshotForceCharge asserted.
                        if let Ok(flag_writes) = (ControlCommand::ClearChargeTargetFlag).encode() {
                            writes.extend(flag_writes);
                        }
                    } else if device_type.uses_extended_schedule_slots() {
                        // Extended-slot models (Gen3+ hybrid, AIO, HV Gen3):
                        // write the GLOBAL charge target SOC (HR 116) so models
                        // that key off the global register — notably the
                        // All-in-One, confirmed by GivTCP's setChargeSlot →
                        // set_charge_target_only — actually honour the target.
                        // Per-slot HR 242 is written below for models that use
                        // it. The enable_charge_target flag (HR 20) is left
                        // cleared so we don't arm immediate "winter mode"
                        // force-charging.
                        if let Ok(target_writes) = (ControlCommand::SetChargeTargetSocOnly {
                            soc: target_soc as u16,
                        })
                        .encode()
                        {
                            writes.extend(target_writes);
                        }
                        if let Ok(flag_writes) = (ControlCommand::ClearChargeTargetFlag).encode() {
                            writes.extend(flag_writes);
                        }
                    } else {
                        // Non-extended-slot models (AC-coupled, Gen1/Gen2
                        // hybrid) with an explicit target SOC < 100%: write
                        // the target to the standard HR116 register and enable
                        // the charge target flag. Without this, the user's
                        // target SOC slider value is silently dropped — the
                        // battery would charge to 100% regardless of what
                        // target they set. SetChargeTargetSoc encodes both
                        // HR20=1 (enable_charge_target) and HR116=<soc>.
                        if let Ok(target_writes) = (ControlCommand::SetChargeTargetSoc {
                            soc: target_soc as u16,
                        })
                        .encode()
                        {
                            writes.extend(target_writes);
                        }
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
            } else {
                // Disabling a slot: the slot register write above (from
                // `charge_slot_command_for_device`) has already zeroed the
                // slot times, so the next decode will see the slot as
                // unconfigured and the UI toggle will round-trip correctly
                // (fix for issue #106: AIO charge slot toggle reverted to ON
                // after navigating away and back). On single-phase models
                // also clear the master enable_charge flag (HR 96) so the
                // inverter actually stops honouring the (now-cleared) slot —
                // this mirrors the `SetEnableCharge { enabled: true }` write
                // in the `if enabled` branch above. Three-phase manages its
                // enable bits via ThreePhaseForceCharge etc., not here.
                if !device_type.uses_three_phase_schedule_slots() {
                    if let Ok(disable_writes) =
                        (ControlCommand::SetEnableCharge { enabled: false }).encode()
                    {
                        writes.extend(disable_writes);
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

/// POST /api/control/eps — set Emergency Power Supply (EPS) mode.
pub async fn set_eps(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let enabled: bool = match body["enabled"].as_bool() {
        Some(e) => e,
        None => return error_response("Missing 'enabled' field (boolean)"),
    };

    // HR 317 only exists on AC-coupled / AC-three-phase / All-in-One models
    // (see DeviceType::supports_eps). On every other family the firmware
    // silently drops the write — earlier we returned 200 anyway, which left
    // the user with a successful response but a UI that still showed EPS
    // off on the next snapshot (because we don't poll HR 300-359 for those
    // devices). Refuse the write up front with a clear error so the toggle
    // can be hidden in the UI.
    let device_type = latest_device_type(&state).await;
    if !device_type.supports_eps() {
        return error_response(&format!(
            "Emergency Power Supply is not supported on {} inverters",
            device_type.display_name()
        ));
    }

    let cmd = ControlCommand::SetEps { enabled };
    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("SetEps encoded: {:?}", writes);
            queue_writes(&state, writes).await;
            ok_response(&format!(
                "Emergency Power Supply (EPS) mode {}",
                if enabled { "enabled" } else { "disabled" }
            ))
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

/// POST /api/control/export-limit — set the inverter export power limit.
///
/// Routes the write to the correct register based on the active device type:
///   - EMS / Gateway / EmsCommercial → HR 2071 (`SetEmsExportLimit`, raw W)
///   - Three-phase / HV / AIO        → HR 1063 (`SetThreePhaseExportLimit`, deci-W)
///   - All other models                → 400 (single-phase models have no
///     user-writable export limit register in the givenergy-modbus reference;
///
///     HR(26) is read-only `grid_port_max_power_output`. The UI is expected
///     to gate this control on `deviceSupportsExportLimit` so the endpoint
///     rejects out-of-gauge requests cleanly.)
pub async fn set_export_limit(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let watts: u16 = match body["watts"].as_u64() {
        Some(w) => w as u16,
        None => return error_response("Missing 'watts' field"),
    };

    let device_type = latest_device_type(&state).await;
    let cmd = if device_type.needs_gateway_input_blocks()
        || matches!(
            device_type,
            DeviceType::Ems | DeviceType::EmsCommercial
        )
    {
        // EMS / Gateway plant-level (HR 2071).
        ControlCommand::SetEmsExportLimit { watts }
    } else if device_type.needs_three_phase_input_blocks() {
        // Three-phase / HV / AIO plant-level (HR 1063, deci-W).
        ControlCommand::SetThreePhaseExportLimit { watts }
    } else {
        return error_response(
            "Export limit control is not available on this inverter (single-phase / AC-coupled)",
        );
    };

    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("SetExportLimit ({:?}) encoded: {:?}", device_type, writes);
            queue_writes(&state, writes).await;
            ok_response(&format!("Export limit set to {} W", watts))
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
        writes.extend(
            ControlCommand::ThreePhaseCosyExit
                .encode()
                .unwrap_or_default(),
        );
    } else {
        // Restore self-consumption (Eco) mode so the inverter doesn't stay
        // in export mode after a force discharge is cancelled.
        writes.extend(
            ControlCommand::SetBatteryPowerMode { mode: 1 }
                .encode()
                .unwrap_or_default(),
        );
    }
    // Set SOC reserve to 100 so Eco Paused actually prevents discharge
    // (the inverter otherwise continues exporting at the previous reserve rate).
    if let Ok(mut reserve_writes) = reserve_writes_for_device(device_type, 100) {
        writes.append(&mut reserve_writes);
    }
    tracing::info!("PauseBattery encoded: {:?}", writes);
    queue_writes(&state, writes).await;
    ok_response("Battery paused")
}

/// POST /api/control/force-charge — enable charging with target SOC.
///
/// Uses three-phase registers (HR 1123/1111) for three-phase, commercial,
/// and HV hybrid inverters; single-phase registers (HR 96/116) for all others.
/// When a JSON body with `minutes` is provided, also writes a charge slot
/// covering now → now + minutes so the inverter has an active charging window
/// (matching GivTCP's forceCharge behaviour).
///
/// On start, captures the pre-force-charge state into `AppState::force_charge_revert`
/// so the Stop Charge endpoint can restore the inverter to its prior
/// configuration. Mirrors GivTCP's `revert` dict in `forceCharge`.
pub async fn force_charge(
    State(state): State<Arc<AppState>>,
    body: Option<Json<serde_json::Value>>,
) -> (StatusCode, Json<Value>) {
    let device_type = latest_device_type(&state).await;
    let is_three_phase = device_type.uses_three_phase_schedule_slots();
    let mut writes = Vec::new();

    // Capture the pre-force-charge state BEFORE queuing the force-charge
    // writes, so the stop endpoint can restore the inverter to this point.
    // This is the Rust equivalent of GivTCP's `revert` dict (write.py:1148).
    let revert = capture_force_charge_revert(&state, device_type).await;
    *state.force_charge_revert.lock().await = revert;

    // If minutes provided, write a charge slot first (now → now+minutes).
    let minutes = body
        .as_ref()
        .and_then(|j| j.0.get("minutes").and_then(|v| v.as_u64()));
    if let Some(minutes) = minutes {
        match force_charge_slot_writes(device_type, minutes) {
            Ok(mut slot_writes) => writes.append(&mut slot_writes),
            Err(e) => return error_response(&format!("Failed to encode charge slot: {}", e)),
        }
    }

    // Then set the force-charge flags (enable_charge + target_soc).
    let cmd = if is_three_phase {
        ControlCommand::ThreePhaseForceCharge { target_soc: 100 }
    } else {
        ControlCommand::ForceCharge { target_soc: 100 }
    };
    match cmd.encode() {
        Ok(mut cmd_writes) => {
            writes.append(&mut cmd_writes);
            tracing::info!("ForceCharge encoded: {:?}", writes);
            queue_writes(&state, writes).await;
            ok_response("Force charge enabled")
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/force-charge/stop — cancel a Force Charge and restore
/// the inverter to its pre-force-charge state.
///
/// Reads the `ForceChargeRevert` snapshot captured when Force Charge was
/// started and queues writes that restore the original charge flag, target
/// SOC, charge slot, and (for three-phase) the force-charge and AC-charge
/// enable flags. Mirrors GivTCP's `FCResume` (`write.py:1042`).
///
/// Returns an error if no Force Charge is in progress (i.e. the revert
/// is `None`) — this prevents the user from accidentally clearing a
/// working charge schedule they didn't intend to.
pub async fn force_charge_stop(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<Value>) {
    // Take the revert so a concurrent force-charge re-arm can't race with
    // us: the second start will overwrite the field, but the writes we
    // queue now restore the FIRST start's captured state, which is what
    // the user is asking to undo. Once consumed, a fresh force-charge
    // must run to set a new revert.
    let revert = state.force_charge_revert.lock().await.take();
    let revert = match revert {
        Some(r) => r,
        None => {
            return error_response("No force charge in progress to stop");
        }
    };

    let device_type = latest_device_type(&state).await;
    let writes = build_force_charge_stop_writes(device_type, &revert);
    if writes.is_empty() {
        return error_response("No restore writes generated for this device");
    }
    tracing::info!("ForceCharge stop encoded: {:?}", writes);
    queue_writes(&state, writes).await;
    ok_response("Force charge stopped")
}

/// POST /api/control/force-discharge — enable discharge with a full-day slot
/// (no body) or a duration-limited slot (body `{"minutes": N}`).
///
/// Uses three-phase register (HR 1122) for three-phase, commercial,
/// and HV hybrid inverters; single-phase register (HR 59 + slots) for all others.
///
/// On start, captures the pre-force-discharge state into
/// `AppState::force_discharge_revert` so the Stop Discharge endpoint can
/// restore the inverter to its prior configuration. Mirrors GivTCP's
/// `revert` dict in `forceExport` (`write.py:980-1010`).
///
/// When a JSON body with `minutes` is provided, writes a discharge slot
/// covering now → now + minutes (matching GivTCP's `forceExport`
/// `set_mode_storage(discharge_slot_1=TimeSlot{now, now+exportTime})`
/// at `write.py:1019`). The no-body path keeps the existing behaviour
/// of a 00:00–23:59 slot (effectively "until stopped") for backward
/// compatibility with any callers that don't supply a duration.
pub async fn force_discharge(
    State(state): State<Arc<AppState>>,
    body: Option<Json<serde_json::Value>>,
) -> (StatusCode, Json<Value>) {
    let device_type = latest_device_type(&state).await;
    let is_three_phase = device_type.uses_three_phase_schedule_slots();

    // Capture the pre-force-discharge state BEFORE queuing writes.
    let mut revert = capture_force_discharge_revert(&state, device_type).await;

    // If minutes provided, write the duration slot first (slot 1 =
    // now → now+minutes, slot 2 = cleared). The poll loop processes
    // writes sequentially, so writing the slot before arming the
    // discharge flag is important: the inverter needs the slot to
    // exist before enable_discharge=1 has anything to gate.
    let minutes = body
        .as_ref()
        .and_then(|j| j.0.get("minutes").and_then(|v| v.as_u64()));
    let mut writes = Vec::new();
    if let Some(minutes) = minutes {
        // Record the slot's end time so the poll loop can auto-revert
        // when the window expires (issue #129). Without this, the
        // inverter is left in export mode (HR 27=0) with enable_discharge=1
        // and enable_charge=0 once the slot window closes — effectively
        // pausing the battery (no charge from solar, no discharge).
        // Compute expiry from the same `start + minutes` that the slot
        // helper uses, so the auto-revert fires when the inverter does.
        let clamped_minutes = minutes.clamp(1, 1439) as i64;
        let expiry = Local::now() + ChronoDuration::minutes(clamped_minutes);
        if let Some(r) = revert.as_mut() {
            r.force_discharge_slot_end_ms =
                Some(expiry.timestamp_millis());
        }
        match force_discharge_slot_writes(device_type, minutes) {
            Ok(mut slot_writes) => writes.append(&mut slot_writes),
            Err(e) => return error_response(&format!("Failed to encode discharge slot: {}", e)),
        }
    }

    *state.force_discharge_revert.lock().await = revert;

    // Then the force-discharge flags (mode, enable_charge=0,
    // enable_charge_target=0, enable_discharge=1, and — on the no-body
    // path only — the 00:00–23:59 slot).
    let cmd = if is_three_phase {
        ControlCommand::ThreePhaseForceDischarge
    } else {
        ControlCommand::ForceDischarge
    };
    match cmd.encode() {
        Ok(cmd_writes) => {
            if minutes.is_some() {
                // On the minutes path, drop the encoder's slot writes so
                // the duration slot from `force_discharge_slot_writes`
                // isn't overwritten by 00:00–23:59. The encoder writes:
                //   single-phase: HR_DISCHARGE_SLOT_1_START/END, HR_DISCHARGE_SLOT_2_START/END
                //   3PH:          HR_3PH_DISCHARGE_SLOT_1_START/END, HR_3PH_DISCHARGE_SLOT_2_START/END
                use crate::modbus::registers::{
                    HR_3PH_DISCHARGE_SLOT_1_END, HR_3PH_DISCHARGE_SLOT_1_START,
                    HR_3PH_DISCHARGE_SLOT_2_END, HR_3PH_DISCHARGE_SLOT_2_START,
                    HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START,
                    HR_DISCHARGE_SLOT_2_END, HR_DISCHARGE_SLOT_2_START,
                };
                let slot_addrs: &[u16] = if is_three_phase {
                    &[
                        HR_3PH_DISCHARGE_SLOT_1_START,
                        HR_3PH_DISCHARGE_SLOT_1_END,
                        HR_3PH_DISCHARGE_SLOT_2_START,
                        HR_3PH_DISCHARGE_SLOT_2_END,
                    ]
                } else {
                    &[
                        HR_DISCHARGE_SLOT_1_START,
                        HR_DISCHARGE_SLOT_1_END,
                        HR_DISCHARGE_SLOT_2_START,
                        HR_DISCHARGE_SLOT_2_END,
                    ]
                };
                for w in cmd_writes {
                    if !slot_addrs.contains(&w.address) {
                        writes.push(w);
                    }
                }
            } else {
                writes.extend(cmd_writes);
            }
            tracing::info!("ForceDischarge encoded: {:?}", writes);
            queue_writes(&state, writes).await;
            ok_response("Force discharge enabled")
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// POST /api/control/force-discharge/stop — cancel a Force Discharge and
/// restore the inverter to its pre-force-discharge state.
///
/// Reads the `ForceDischargeRevert` snapshot captured when Force Discharge
/// was started and queues writes that restore the original charge flag,
/// discharge flag, discharge slots, and (for three-phase) the
/// force-discharge and force-charge enable flags.
///
/// Returns an error if no Force Discharge is in progress — this prevents
/// the user from accidentally clearing a working discharge schedule they
/// didn't intend to.
pub async fn force_discharge_stop(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<Value>) {
    let revert = state.force_discharge_revert.lock().await.take();
    let revert = match revert {
        Some(r) => r,
        None => {
            return error_response("No force discharge in progress to stop");
        }
    };

    let device_type = latest_device_type(&state).await;
    let writes = build_force_discharge_stop_writes(device_type, &revert);
    if writes.is_empty() {
        return error_response("No restore writes generated for this device");
    }
    tracing::info!("ForceDischarge stop encoded: {:?}", writes);
    queue_writes(&state, writes).await;
    ok_response("Force discharge stopped")
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

    // The Cost tab requests the derived `_import_cost` / `_export_income`
    // fields, which aren't stored columns - the server integrates them from
    // the `today_*_kwh` counters and the configured tariff at native reading
    // resolution. Split them out from the directly-aggregated
    // fields so each goes down its own path.
    let want_import_cost = fields.iter().any(|f| f == crate::history::IMPORT_COST_FIELD);
    let want_export_income = fields.iter().any(|f| f == crate::history::EXPORT_INCOME_FIELD);
    let normal_fields: Vec<String> = fields
        .iter()
        .filter(|f| !crate::history::is_cost_field(f))
        .cloned()
        .collect();

    // Resolve tariff configs only when a cost field is requested, falling back
    // to a flat single-slot config built from the legacy scalar rate.
    let cost_cfgs = if want_import_cost || want_export_income {
        let s = crate::settings::Settings::load();
        let import_cfg = s
            .import_tariff_config
            .clone()
            .unwrap_or_else(|| crate::settings::TariffConfig::flat(s.import_tariff));
        let export_cfg = s
            .export_tariff_config
            .clone()
            .unwrap_or_else(|| crate::settings::TariffConfig::flat(s.export_tariff));
        Some((import_cfg, export_cfg, s.import_tariff, s.export_tariff))
    } else {
        None
    };

    // Clone the HistoryDb handle and drop the async lock so the synchronous
    // SQLite query runs on a blocking thread instead of pinning the Tokio
    // worker while holding the async mutex.
    let history_db = state.history.lock().await.clone();

    match history_db {
        Some(db) => {
            let result = tokio::task::spawn_blocking(
                move || -> Result<serde_json::Map<String, Value>, String> {
                    // Shared window spec so the aggregated and cost paths cover
                    // the exact same span of readings.
                    let window = crate::history::HistoryWindow {
                        range_secs,
                        offset,
                        explicit_window,
                    };

                    let mut data = if normal_fields.is_empty() {
                        serde_json::Map::new()
                    } else {
                        db.query_history(
                            window.range_secs,
                            bucket_secs,
                            window.offset,
                            &normal_fields,
                            window.explicit_window,
                        )?
                    };

                    if let Some((import_cfg, export_cfg, flat_import, flat_export)) = &cost_cfgs {
                        if want_import_cost {
                            let series = db.query_cost_series(
                                &window,
                                bucket_secs,
                                "today_import_kwh",
                                import_cfg,
                                *flat_import,
                            )?;
                            data.insert(
                                crate::history::IMPORT_COST_FIELD.to_string(),
                                serde_json::to_value(&series).unwrap_or(Value::Null),
                            );
                        }
                        if want_export_income {
                            let series = db.query_cost_series(
                                &window,
                                bucket_secs,
                                "today_export_kwh",
                                export_cfg,
                                *flat_export,
                            )?;
                            data.insert(
                                crate::history::EXPORT_INCOME_FIELD.to_string(),
                                serde_json::to_value(&series).unwrap_or(Value::Null),
                            );
                        }
                    }

                    Ok(data)
                },
            )
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
// Email alerts endpoints
// ---------------------------------------------------------------------------

/// GET /api/alerts — current alert config and debounce status.
pub async fn get_alerts(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let config = state.alert_config.lock().await.clone();
    let debounce_count = state.alert_debounce.lock().await.len();
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "data": {
                "config": config,
                "debounce_entries": debounce_count,
            }
        })),
    )
}

/// POST /api/alerts — update alert config.
///
/// All body fields are optional — only provided fields are updated.
pub async fn set_alerts(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let mut config = state.alert_config.lock().await;

    if let Some(v) = body.get("enabled").and_then(|v| v.as_bool()) {
        config.enabled = v;
    }
    if let Some(v) = body.get("telegram_bot_token").and_then(|v| v.as_str()) {
        config.telegram_bot_token = v.to_string();
    }
    if let Some(v) = body.get("telegram_chat_id").and_then(|v| v.as_str()) {
        config.telegram_chat_id = v.to_string();
    }
    if let Some(v) = body.get("cooldown_minutes").and_then(|v| v.as_u64()) {
        config.cooldown_minutes = v.clamp(1, 1440) as u32;
    }
    if let Some(v) = body.get("batt_temp_min").and_then(|v| v.as_f64()) {
        config.batt_temp_min = v as f32;
    }
    if let Some(v) = body.get("batt_temp_max").and_then(|v| v.as_f64()) {
        config.batt_temp_max = v as f32;
    }
    if let Some(v) = body.get("soc_min").and_then(|v| v.as_u64()) {
        config.soc_min = v.min(100) as u8;
    }
    if let Some(v) = body.get("soc_max").and_then(|v| v.as_u64()) {
        config.soc_max = v.min(100) as u8;
    }
    if let Some(v) = body.get("grid_offline_enabled").and_then(|v| v.as_bool()) {
        config.grid_offline_enabled = v;
    }
    if let Some(v) = body.get("connection_lost_enabled").and_then(|v| v.as_bool()) {
        config.connection_lost_enabled = v;
    }
    if let Some(v) = body
        .get("battery_over_temp_enabled")
        .and_then(|v| v.as_bool())
    {
        config.battery_over_temp_enabled = v;
    }
    if let Some(v) = body.get("solar_clipping_enabled").and_then(|v| v.as_bool()) {
        config.solar_clipping_enabled = v;
    }
    if let Some(v) = body
        .get("solar_clipping_ceiling_w")
        .and_then(|v| v.as_u64())
    {
        // Clamp to a sane range: 0 (disabled) up to 100kW.
        config.solar_clipping_ceiling_w = v.min(100_000) as u32;
    }
    if let Some(v) = body.get("daily_report_enabled").and_then(|v| v.as_bool()) {
        config.daily_report_enabled = v;
    }
    if let Some(v) = body.get("daily_report_hour").and_then(|v| v.as_u64()) {
        config.daily_report_hour = v.min(23) as u8;
    }
    if let Some(v) = body.get("daily_report_minute").and_then(|v| v.as_u64()) {
        config.daily_report_minute = v.min(59) as u8;
    }
    if let Some(v) = body.get("ntfy_topic").and_then(|v| v.as_str()) {
        config.ntfy_topic = v.to_string();
    }
    if let Some(v) = body.get("ntfy_server").and_then(|v| v.as_str()) {
        config.ntfy_server = v.to_string();
    }
    if let Some(v) = body.get("pushover_app_token").and_then(|v| v.as_str()) {
        config.pushover_app_token = v.to_string();
    }
    if let Some(v) = body.get("pushover_user_key").and_then(|v| v.as_str()) {
        config.pushover_user_key = v.to_string();
    }

    // Reset debounce so toggling an alert off/on immediately re-enables
    // notification delivery on the next poll cycle.
    state.alert_debounce.lock().await.clear();

    tracing::info!("Alert config updated");

    // Persist to settings.json
    let mut app_settings = crate::settings::Settings::load();
    app_settings.alerts_config = config.clone();
    drop(config);
    if let Err(e) = app_settings.save() {
        tracing::warn!("Failed to persist alert config: {e}");
        return server_error(&format!("Failed to save: {e}"));
    }

    ok_response("Alert config updated")
}

/// POST /api/alerts/test — send a test notification using the current alert config.
///
/// Uses whatever credentials are currently saved.
pub async fn test_alerts(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let config = state.alert_config.lock().await.clone();

    let has_telegram = !config.telegram_bot_token.is_empty() && !config.telegram_chat_id.is_empty();
    let has_ntfy = !config.ntfy_topic.is_empty();
    let has_pushover =
        !config.pushover_app_token.is_empty() && !config.pushover_user_key.is_empty();

    if !has_telegram && !has_ntfy && !has_pushover {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "message": "No notification channels configured. Set up Telegram, ntfy, or Pushover first."
            })),
        );
    }

    let mut results: Vec<String> = Vec::new();

    if has_telegram {
        let token = config.telegram_bot_token.clone();
        let chat_id = config.telegram_chat_id.clone();
        let r = tokio::task::spawn_blocking(move || {
            crate::alerts::send_telegram_message(
                &token,
                &chat_id,
                "✅ <b>Test Alert</b>\n\nThis is a test from Home Energy Manager.",
            )
        })
        .await;
        match r {
            Ok(Ok(())) => results.push("Telegram: OK".into()),
            Ok(Err(e)) => results.push(format!("Telegram: {e}")),
            Err(_) => results.push("Telegram: internal error".into()),
        }
    }

    if has_ntfy {
        let topic = config.ntfy_topic.clone();
        let server = config.ntfy_server.clone();
        let r = tokio::task::spawn_blocking(move || {
            crate::alerts::send_ntfy_message(
                &topic,
                &server,
                "✅ Test Alert\n\nThis is a test from Home Energy Manager.",
            )
        })
        .await;
        match r {
            Ok(Ok(())) => results.push("ntfy: OK".into()),
            Ok(Err(e)) => results.push(format!("ntfy: {e}")),
            Err(_) => results.push("ntfy: internal error".into()),
        }
    }

    if has_pushover {
        let token = config.pushover_app_token.clone();
        let user_key = config.pushover_user_key.clone();
        let r = tokio::task::spawn_blocking(move || {
            crate::alerts::send_pushover_message(
                &token,
                &user_key,
                "\u{2705} Test Alert\n\nThis is a test from Home Energy Manager.",
            )
        })
        .await;
        match r {
            Ok(Ok(())) => results.push("Pushover: OK".into()),
            Ok(Err(e)) => results.push(format!("Pushover: {e}")),
            Err(_) => results.push("Pushover: internal error".into()),
        }
    }

    let all_ok = results
        .iter()
        .all(|r| r.contains("OK") || r.contains("delivered"));
    let joined = results.join("; ");
    tracing::info!("Test notification: {joined}");
    if all_ok {
        ok_response(&format!("Sent! {joined}"))
    } else {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "ok": false,
                "message": joined
            })),
        )
    }
}

// ---------------------------------------------------------------------------
// Support bundle submission (issue #125)
// ---------------------------------------------------------------------------

/// POST /api/support/submit — assemble a support bundle and deliver it via
/// ntfy to the shared [`crate::support::SUPPORT_NTFY_TOPIC`] topic.
///
/// Gathers the current snapshot, the developer log ring, a sanitised view of
/// the alert/notification settings (secrets redacted), and an optional recent
/// history tail, packs them into a single JSON bundle, and uploads it to the
/// hard-coded public ntfy.sh server. The maintainer subscribes to that one
/// topic and receives every submission; each bundle is disambiguated by its
/// serial-derived ID (see [`crate::support::generate_bundle_id`]). When the
/// user supplies a GitHub issue number, an ntfy `Click` header deep-links the
/// notification straight to that issue.
///
/// Rate-limited to one submission per [`crate::support::SUBMIT_COOLDOWN_SECS`]
/// to stop double-clicks or a stuck UI from flooding the shared topic.
pub async fn submit_support_bundle(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    // Parse + validate the request before touching any shared state, so a
    // malformed body can't hold locks or burn the rate-limit token.
    let request: crate::support::SupportRequest = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(e) => {
            return error_response(&format!("Invalid request body: {e}"));
        }
    };
    if let Err(e) = crate::support::validate_request(&request) {
        return error_response(&e);
    }

    // Rate limit: only a successful delivery stamps the timestamp below, so a
    // failed attempt does NOT consume the cooldown window — the user can retry
    // immediately after a network error rather than being locked out.
    let now_ms = chrono::Local::now().timestamp_millis();
    let last_ms = state
        .last_support_submit_ms
        .load(std::sync::atomic::Ordering::Relaxed);
    if last_ms > 0 {
        let elapsed_secs = (now_ms - last_ms) / 1000;
        if elapsed_secs < crate::support::SUBMIT_COOLDOWN_SECS {
            let wait = crate::support::SUBMIT_COOLDOWN_SECS - elapsed_secs;
            return error_response(&format!(
                "Please wait {wait} more second{} before submitting another bundle.",
                if wait == 1 { "" } else { "s" }
            ));
        }
    }

    // Gather inputs under short-lived locks, cloning what we need so the
    // (synchronous, blocking) delivery never holds a lock across an await.
    let snapshot = state.latest_snapshot.lock().await.clone();
    let logs = state.log_ring.read_all();
    let log_capture_level = {
        let code = state
            .log_ring
            .min_level
            .load(std::sync::atomic::Ordering::Relaxed);
        match code {
            0 => "ERROR",
            1 => "WARN",
            2 => "INFO",
            3 => "DEBUG",
            4 => "TRACE",
            _ => "INFO",
        }
        .to_string()
    };
    let (host, port, serial, interval_secs) = {
        let s = state.settings.lock().await;
        (s.host.clone(), s.port, s.serial.clone(), s.interval_secs)
    };
    let alerts_config = state.alert_config.lock().await.clone();

    // History tail: today's readings, capped to the newest N. Best-effort —
    // a missing/unwritable DB must not block the rest of the bundle.
    let history_rows = if request.include_history {
        let db_guard = state.history.lock().await;
        if let Some(db) = db_guard.as_ref() {
            let today = chrono::Local::now().date_naive();
            match db.get_readings_for_date(today) {
                Ok(rows) => crate::support::cap_history_rows(rows),
                Err(e) => {
                    tracing::warn!("Support bundle history query failed: {e}");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    let inputs = crate::support::BundleInputs {
        snapshot,
        logs,
        log_capture_level,
        host,
        port,
        serial,
        interval_secs,
        alerts_config,
        history_rows,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: std::env::consts::OS.to_string(),
        request,
    };

    let bundle = match crate::support::build_bundle(inputs) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("Support bundle build failed: {e}");
            return error_response(&e);
        }
    };

    let size_bytes = bundle.json.len();
    let bundle_id = bundle.id.clone();
    let title = format!("HEM Support: {}", bundle.id);
    let click = bundle.issue_url.clone();

    tracing::info!(
        bundle_id = %bundle_id,
        size_bytes,
        issue_number = ?bundle.issue_number,
        "Submitting support bundle to ntfy"
    );

    // Deliver on a blocking thread — ureq is synchronous. ntfy always wins
    // (the topic is hard-coded, independent of the user's alert config) so
    // every user can submit regardless of whether they have set up alerts.
    let manifest_summary = bundle.manifest_summary.clone();
    let filename = bundle.filename.clone();
    let json_body = bundle.json.clone();
    let click_for_task = click.clone();
    let ntfy_result = tokio::task::spawn_blocking(move || {
        crate::alerts::send_ntfy_attachment(
            crate::support::SUPPORT_NTFY_TOPIC,
            crate::support::SUPPORT_NTFY_SERVER,
            &filename,
            "application/json",
            &crate::alerts::NtfyMessage {
                title: &title,
                message: &manifest_summary,
                tags: "package,support",
                click: click_for_task.as_deref(),
            },
            &json_body,
        )
    })
    .await;

    let sent_to = match ntfy_result {
        Ok(Ok(())) => {
            // Only stamp the rate-limit timestamp on a successful delivery.
            state.last_support_submit_ms.store(
                chrono::Local::now().timestamp_millis(),
                std::sync::atomic::Ordering::Relaxed,
            );
            serde_json::json!([{ "channel": "ntfy", "ok": true }])
        }
        Ok(Err(e)) => {
            tracing::warn!("Support bundle ntfy delivery failed: {e}");
            serde_json::json!([{ "channel": "ntfy", "ok": false, "error": e }])
        }
        Err(join_err) => {
            tracing::warn!("Support bundle delivery task failed: {join_err}");
            serde_json::json!([{
                "channel": "ntfy",
                "ok": false,
                "error": format!("internal error: {join_err}")
            }])
        }
    };

    let any_ok = sent_to
        .as_array()
        .map(|a| a.iter().any(|r| r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)))
        .unwrap_or(false);

    let message = if any_ok {
        format!("Bundle {bundle_id} submitted.")
    } else {
        "Bundle was assembled but could not be delivered. Check your internet connection and try again."
            .to_string()
    };
    let status = if any_ok {
        StatusCode::OK
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (
        status,
        Json(json!({
            "ok": any_ok,
            "bundle_id": bundle_id,
            "size_bytes": size_bytes,
            "sent_to": sent_to,
            "message": message,
        })),
    )
}

// ---------------------------------------------------------------------------
// Reconnect endpoint
// ---------------------------------------------------------------------------

/// POST /api/reconnect — force a disconnect and reconnection cycle.
///
/// Bumps the settings version to wake the poll loop, which detects
/// the version change and disconnects, then reconnects with a fresh
/// TCP session. Also increments `reconnect_request` so the poll loop
/// resets its back-off state (`backoff` and `consecutive_dead_sessions`)
/// — without this, a manual "Reconnect" against a chronically-hung
/// dongle would only fire one extra attempt before the loop fell back
/// into a 10-minute zombie-dongle back-off sleep, which is not what the
/// user expects when they click "Retry now".
pub async fn post_reconnect(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<Value>) {
    // Bump settings version so the poll loop detects the change
    // and breaks out of the inner loop → disconnect → reconnect.
    let mut settings = state.settings.lock().await;
    settings.version = settings.version.wrapping_add(1);
    drop(settings);

    // Also increment the reconnect-request counter so the poll loop
    // resets its back-off timers on the next outer-loop iteration.
    // Without this, the post-reconnect attempt is followed by another
    // 10-minute sleep (when consecutive_dead_sessions ≥ 5), which
    // makes the button feel unresponsive.
    state
        .reconnect_request
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    state.write_notify.notify_one();
    tracing::info!("Reconnect requested — bumped settings version to force cycle");
    ok_response("Reconnecting...")
}

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

/// GET /api/evc/status — current EV charger reachability snapshot.
///
/// The frontend calls this on page load (before WS frames start flowing)
/// to seed `evcEverConnected`. The WS broadcast channel doesn't replay
/// past messages, so a late subscriber would otherwise miss the initial
/// `EvcConnected` / `Evc(snapshot)` event and the diagram would stay
/// pinned on the misleading "Not Found" label forever (issue #138).
///
/// `reachable` is derived from the cached `latest_evc`: if a snapshot has
/// been decoded since the process started, the configured host has been
/// reached at least once. Once true, it stays true until process restart.
pub async fn evc_status(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let evc_host = state.settings.lock().await.evc_host.clone();
    let evc_port = state.settings.lock().await.evc_port;
    let cached = state.latest_evc.lock().await.clone();
    let reachable = cached.is_some();
    let last_snapshot = cached.map(|s| {
        serde_json::json!({
            "charging_state": s.charging_state,
            "connection_status": s.connection_status,
            "active_power": s.active_power,
            "current_l1": s.current_l1,
            "current_l2": s.current_l2,
            "current_l3": s.current_l3,
            "voltage_l1": s.voltage_l1,
            "voltage_l2": s.voltage_l2,
            "voltage_l3": s.voltage_l3,
            "meter_energy_kwh": s.meter_energy_kwh,
            "session_energy_kwh": s.session_energy_kwh,
            "session_duration_secs": s.session_duration_secs,
            "charge_limit_a": s.charge_limit_a,
            "serial_number": s.serial_number,
        })
    });
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "evc_host": evc_host,
            "evc_port": evc_port,
            "reachable": reachable,
            "snapshot": last_snapshot,
        })),
    )
}

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

// ---------------------------------------------------------------------------
// Weather (Open-Meteo integration)
// ---------------------------------------------------------------------------

/// Resolve a UK postcode to lat/lon via api.postcodes.io. Returns the
/// canonicalised postcode string alongside the coordinates so the caller
/// can persist both. `None` means the lookup failed (network error, 404,
/// or malformed body) — the caller surfaces the failure to the user.
fn lookup_postcode(postcode: &str) -> Option<(String, f64, f64)> {
    let trimmed = postcode.trim();
    if trimmed.is_empty() {
        return None;
    }
    let url = format!(
        "https://api.postcodes.io/postcodes/{}",
        percent_encoding::utf8_percent_encode(
            trimmed,
            percent_encoding::NON_ALPHANUMERIC,
        )
    );
    // postcodes.io is a low-volume, trusted UK government-backed API. We
    // reuse the shared weather agent (10 s timeout, no idle pooling) —
    // consistent with how we talk to Open-Meteo.
    let resp = crate::weather::weather_agent().get(&url).call().ok()?;
    let mut resp = resp;
    let body = resp.body_mut().read_to_string().ok()?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    if json.get("status").and_then(|v| v.as_u64()) != Some(200) {
        return None;
    }
    let result = json.get("result")?;
    let lat = result.get("latitude")?.as_f64()?;
    let lon = result.get("longitude")?.as_f64()?;
    let canonical = result
        .get("postcode")
        .and_then(|v| v.as_str())
        .unwrap_or(trimmed)
        .to_string();
    Some((canonical, lat, lon))
}

/// GET /api/weather — return the current weather subsystem state.
///
/// Includes the persisted config, the most recent live fetch result, the
/// resolved Open-Meteo grid cell, and backfill progress. The Settings UI
/// polls this to render the current state and backfill spinner.
pub async fn get_weather(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let ws = state.weather.lock().await.clone();
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "data": ws,
        })),
    )
}

/// POST /api/weather — update the weather config.
///
/// Accepts a partial update: any of `enabled`, `postcode`, `latitude`,
/// `longitude`, `open_meteo_base_url`. When `postcode` is provided without
/// coordinates, the server resolves it via api.postcodes.io and stores the
/// resulting lat/lon. When coordinates are provided directly they take
/// precedence (manual override for non-UK users or self-hosters).
pub async fn set_weather(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let mut ws = state.weather.lock().await;

    if let Some(v) = body.get("enabled").and_then(|v| v.as_bool()) {
        ws.config.enabled = v;
    }
    if let Some(v) = body.get("postcode").and_then(|v| v.as_str()) {
        ws.config.postcode = v.to_string();
    }
    // Manual coordinate override — takes precedence over any postcode.
    if let Some(lat) = body.get("latitude").and_then(|v| v.as_f64()) {
        ws.config.latitude = Some(lat);
    }
    if let Some(lon) = body.get("longitude").and_then(|v| v.as_f64()) {
        ws.config.longitude = Some(lon);
    }
    if let Some(v) = body.get("open_meteo_base_url").and_then(|v| v.as_str()) {
        let trimmed = v.trim_end_matches('/');
        if !trimmed.is_empty() {
            ws.config.open_meteo_base_url = trimmed.to_string();
        }
    }

    // If we have a postcode but no coordinates, try to resolve now so the
    // user gets immediate feedback (rather than waiting for the next poll
    // tick). Failure is non-fatal — the user can enter coords manually.
    if !ws.config.postcode.is_empty() && (ws.config.latitude.is_none() || ws.config.longitude.is_none()) {
        match lookup_postcode(&ws.config.postcode) {
            Some((canonical, lat, lon)) => {
                ws.config.postcode = canonical;
                ws.config.latitude = Some(lat);
                ws.config.longitude = Some(lon);
            }
            None => {
                tracing::info!(
                    postcode = %ws.config.postcode,
                    "postcode lookup failed; leaving coordinates unset",
                );
            }
        }
    }

    let config_clone = ws.config.clone();
    drop(ws);

    // Persist to settings.json so the config survives a restart.
    let mut app_settings = crate::settings::Settings::load();
    app_settings.weather_config = config_clone;
    if let Err(e) = app_settings.save() {
        tracing::warn!("Failed to persist weather config: {e}");
        return server_error(&format!("Failed to save: {e}"));
    }

    tracing::info!("Weather config updated");
    ok_response("Weather config updated")
}

/// POST /api/weather/backfill — kick off a one-shot backfill of historical
/// weather data from Open-Meteo's archive endpoint. Returns immediately;
/// the frontend polls `GET /api/weather` for `backfill_in_progress` and
/// `last_backfill_completed` to track progress.
pub async fn backfill_weather(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    crate::weather::spawn_backfill(state.clone());
    ok_response("Backfill started")
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
        // Single-phase charge-slot disable now mirrors discharge-slot behaviour:
        // the slot time registers are zeroed so the next decode sees the slot
        // as unconfigured. The master enable_charge flag (HR 96) is cleared
        // separately by the `set_charge_slot` handler in its `else` branch —
        // see `aio_slot_disable_writes_both_slot_times_and_hr96` for the
        // end-to-end regression test (issue #106).
        assert!(matches!(
            charge_slot_command_for_device(DeviceType::Gen3Hybrid, 1, false, 130, 530).unwrap(),
            ControlCommand::SetChargeSlot1 { start: 0, end: 0 }
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
            HR_3PH_DISCHARGE_SLOT_2_END, HR_3PH_DISCHARGE_SLOT_2_START, HR_DISCHARGE_SLOT_1_END,
            HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_END, HR_DISCHARGE_SLOT_2_START,
            SAFE_WRITE_REGS,
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
        assert!(
            !writes.is_empty(),
            "handler should queue at least one write"
        );
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

    /// The GivEnergy Gateway (DTC 0x7001) is a single-phase-class control
    /// device for scheduling purposes — it forwards standard charge/discharge
    /// slot and SOC writes to its child AIO(s). It must NOT route through the
    /// three-phase control bank (HR 1080-1124), which a real Gateway dongle
    /// has no registers for (issue #149: quick actions silently did nothing).
    /// Mirrors dewet22/givenergy-modbus `slot_map` (Gateway →
    /// SINGLE_PHASE_SLOTS) and GivTCP ("gateway" is not "3ph" for write
    /// routing). Regression test: route force-charge to single-phase registers.
    #[tokio::test]
    async fn gateway_force_charge_routes_to_single_phase_registers() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_CHARGE_SLOT_1_END, HR_CHARGE_SLOT_1_START, HR_CHARGE_TARGET_SOC,
                HR_ENABLE_CHARGE, HR_3PH_CHARGE_SLOT_1_START, HR_3PH_FORCE_CHARGE_ENABLE,
            };
            let state = make_state_with_device(DeviceType::Gateway).await;

            let (status, body) =
                force_charge(State(state.clone()), Some(Json(serde_json::json!({"minutes": 30})))).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body.0["ok"], serde_json::Value::Bool(true));

            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            // Single-phase charge registers must be written.
            let slot_start = writes.iter().find(|w| w.address == HR_CHARGE_SLOT_1_START);
            assert!(slot_start.is_some(), "Gateway must write HR 94 (charge slot 1 start)");
            let _ = writes.iter().find(|w| w.address == HR_CHARGE_SLOT_1_END)
                .expect("Gateway must write HR 95 (charge slot 1 end)");
            assert_eq!(
                writes.iter().find(|w| w.address == HR_ENABLE_CHARGE).map(|w| w.value),
                Some(1),
                "Gateway must set HR 96 (enable_charge)",
            );
            assert_eq!(
                writes.iter().find(|w| w.address == HR_CHARGE_TARGET_SOC).map(|w| w.value),
                Some(100),
                "Gateway must write HR 116 (charge target SOC)",
            );
            // The three-phase control bank must NOT be touched.
            assert!(
                writes.iter().all(|w| w.address != HR_3PH_CHARGE_SLOT_1_START),
                "Gateway must NOT write HR 1113 (three-phase charge slot)",
            );
            assert!(
                writes.iter().all(|w| w.address != HR_3PH_FORCE_CHARGE_ENABLE),
                "Gateway must NOT write HR 1123 (three-phase force charge)",
            );
        })
        .await;
    }

    /// Same routing invariant for Force Discharge: the Gateway must target
    /// single-phase discharge registers (HR 56/57 slot, HR 59 enable) and not
    /// the three-phase bank (HR 1118-1122). See issue #149.
    #[tokio::test]
    async fn gateway_force_discharge_routes_to_single_phase_registers() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_DISCHARGE_SLOT_1_START, HR_ENABLE_DISCHARGE, HR_3PH_FORCE_DISCHARGE_ENABLE,
            };
            let state = make_state_with_device(DeviceType::Gateway).await;

            let (status, body) =
                force_discharge(State(state.clone()), Some(Json(serde_json::json!({"minutes": 30}))))
                    .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body.0["ok"], serde_json::Value::Bool(true));

            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes.iter().any(|w| w.address == HR_DISCHARGE_SLOT_1_START),
                "Gateway must write HR 56 (discharge slot 1 start)",
            );
            assert_eq!(
                writes.iter().find(|w| w.address == HR_ENABLE_DISCHARGE).map(|w| w.value),
                Some(1),
                "Gateway must set HR 59 (enable_discharge)",
            );
            assert!(
                writes.iter().all(|w| w.address != HR_3PH_FORCE_DISCHARGE_ENABLE),
                "Gateway must NOT write HR 1122 (three-phase force discharge)",
            );
        })
        .await;
    }

    /// The All-in-One honours the GLOBAL charge target SOC (HR 116) — confirmed
    /// by GivTCP's setChargeSlot → set_charge_target_only. Configuring a charge
    /// slot with an explicit target SOC < 100% must therefore write HR 116 (in
    /// addition to the per-slot HR 242), otherwise the AIO ignores the target
    /// and charges to 100% (issue: "SOC target just jumps back").
    #[tokio::test]
    async fn set_charge_slot_writes_global_hr116_for_aio() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_CHARGE_TARGET_SOC, HR_CHARGE_TARGET_SOC_1, HR_ENABLE_CHARGE,
                HR_ENABLE_CHARGE_TARGET,
            };
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 6, "start_minute": 0,
                "end_hour": 10, "end_minute": 0,
                "enabled": true,
                "target_soc": 80,
            });
            let _ = set_charge_slot(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            // Global charge target SOC must be written (HR 116).
            let global = writes.iter().find(|w| w.address == HR_CHARGE_TARGET_SOC);
            assert!(
                global.is_some(),
                "AIO charge slot must write the global charge target SOC (HR 116)"
            );
            assert_eq!(global.unwrap().value, 80);
            // Per-slot target SOC (HR 242) is also written for extended-slot models.
            let per_slot = writes.iter().find(|w| w.address == HR_CHARGE_TARGET_SOC_1);
            assert!(per_slot.is_some(), "extended-slot model must write per-slot HR 242");
            assert_eq!(per_slot.unwrap().value, 80);
            // enable_charge armed, force-charge flag cleared.
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_ENABLE_CHARGE)
                    .unwrap()
                    .value,
                1
            );
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_ENABLE_CHARGE_TARGET)
                    .unwrap()
                    .value,
                0
            );
        })
        .await;
    }

    /// Disabling a charge slot on the All-in-One must zero BOTH the slot times
    /// (HR 94/95) AND the master enable_charge flag (HR 96). Without both, the
    /// next decode sees the slot as still configured and the UI shows the
    /// toggle as ON again after the user navigated away and back (issue #106:
    /// "it will turn it on and off but when it is off if I leave settings tab
    /// and return back to settings it'll show it as being turned on when it
    /// isn't really on"). Regression guard for the `(false, _, false)` arm of
    /// `charge_slot_command_for_device` plus the matching `else` branch in
    /// `set_charge_slot`.
    #[tokio::test]
    async fn aio_slot_disable_writes_both_slot_times_and_hr96() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_CHARGE_SLOT_1_END, HR_CHARGE_SLOT_1_START, HR_ENABLE_CHARGE,
            };
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            // Slot 1, times don't matter for the disable path (handler zeros
            // them), but we send plausible times so any future change that
            // forwards them through unchanged still has a valid HHMM pair.
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 6, "start_minute": 0,
                "end_hour": 10, "end_minute": 0,
                "enabled": false,
            });
            let _ = set_charge_slot(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            // Slot 1 times must be zeroed so the decode sees the slot as
            // unconfigured.
            let start = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_SLOT_1_START)
                .expect("AIO charge slot disable must zero HR 94 (slot 1 start)");
            assert_eq!(start.value, 0, "slot 1 start must be cleared on disable");
            let end = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_SLOT_1_END)
                .expect("AIO charge slot disable must zero HR 95 (slot 1 end)");
            assert_eq!(end.value, 0, "slot 1 end must be cleared on disable");
            // Master enable_charge must be cleared so the inverter actually
            // stops honouring the (now-cleared) slot.
            let enable = writes
                .iter()
                .find(|w| w.address == HR_ENABLE_CHARGE)
                .expect("AIO charge slot disable must clear HR 96 (enable_charge)");
            assert_eq!(enable.value, 0, "HR 96 must be 0 on disable");
        })
        .await;
    }

    /// Enabling EPS on the All-in-One must write HR 317 = 1.
    #[tokio::test]
    async fn aio_eps_toggle() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_ENABLE_EPS;
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({ "enabled": true });
            let _ = set_eps(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            let eps = writes
                .iter()
                .find(|w| w.address == HR_ENABLE_EPS)
                .expect("AIO EPS enable must write HR 317");
            assert_eq!(eps.value, 1, "HR 317 must be 1 on enable");
        })
        .await;
    }

    /// Enabling charge slot 2 on the All-in-One must write the extended-block
    /// registers HR 243/244 (not the classic HR 31/32).
    #[tokio::test]
    async fn aio_charge_slot2_extended_enable() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_CHARGE_SLOT_2_GEN3_END, HR_CHARGE_SLOT_2_GEN3_START, HR_ENABLE_CHARGE,
                HR_ENABLE_CHARGE_TARGET,
            };
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({
                "slot": 2,
                "start_hour": 3, "start_minute": 15,
                "end_hour": 4, "end_minute": 15,
                "enabled": true,
                "target_soc": 100,
            });
            let _ = set_charge_slot(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            let start = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_SLOT_2_GEN3_START)
                .expect("AIO charge slot 2 must write HR 243");
            assert_eq!(start.value, 315, "slot 2 start must be 03:15");
            let end = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_SLOT_2_GEN3_END)
                .expect("AIO charge slot 2 must write HR 244");
            assert_eq!(end.value, 415, "slot 2 end must be 04:15");
            let enable = writes
                .iter()
                .find(|w| w.address == HR_ENABLE_CHARGE)
                .expect("AIO charge slot 2 must set HR 96");
            assert_eq!(enable.value, 1, "HR 96 must be 1 on enable");
            let flag = writes
                .iter()
                .find(|w| w.address == HR_ENABLE_CHARGE_TARGET)
                .expect("AIO charge slot 2 must clear HR 20");
            assert_eq!(flag.value, 0, "HR 20 must be 0 on slot enable");
        })
        .await;
    }

    /// Disabling charge slot 2 on the All-in-One must zero HR 243/244 and clear HR 96.
    #[tokio::test]
    async fn aio_charge_slot2_extended_disable() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_CHARGE_SLOT_2_GEN3_END, HR_CHARGE_SLOT_2_GEN3_START, HR_ENABLE_CHARGE,
            };
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({
                "slot": 2,
                "start_hour": 3, "start_minute": 15,
                "end_hour": 4, "end_minute": 15,
                "enabled": false,
            });
            let _ = set_charge_slot(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            let start = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_SLOT_2_GEN3_START)
                .expect("AIO charge slot 2 disable must write HR 243");
            assert_eq!(start.value, 0, "slot 2 start must be cleared on disable");
            let end = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_SLOT_2_GEN3_END)
                .expect("AIO charge slot 2 disable must write HR 244");
            assert_eq!(end.value, 0, "slot 2 end must be cleared on disable");
            let enable = writes
                .iter()
                .find(|w| w.address == HR_ENABLE_CHARGE)
                .expect("AIO charge slot 2 disable must clear HR 96");
            assert_eq!(enable.value, 0, "HR 96 must be 0 on disable");
        })
        .await;
    }

    /// Enabling a charge slot with target_soc=100 must NOT write HR 116 (no limit).
    #[tokio::test]
    async fn aio_charge_slot_enable_target_soc_100() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_CHARGE_SLOT_1_END, HR_CHARGE_SLOT_1_START, HR_CHARGE_TARGET_SOC,
                HR_ENABLE_CHARGE, HR_ENABLE_CHARGE_TARGET,
            };
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 6, "start_minute": 0,
                "end_hour": 10, "end_minute": 0,
                "enabled": true,
                "target_soc": 100,
            });
            let _ = set_charge_slot(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            let start = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_SLOT_1_START)
                .expect("AIO charge slot must write HR 94");
            assert_eq!(start.value, 600);
            let end = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_SLOT_1_END)
                .expect("AIO charge slot must write HR 95");
            assert_eq!(end.value, 1000);
            let enable = writes
                .iter()
                .find(|w| w.address == HR_ENABLE_CHARGE)
                .expect("AIO charge slot must set HR 96");
            assert_eq!(enable.value, 1);
            let flag = writes
                .iter()
                .find(|w| w.address == HR_ENABLE_CHARGE_TARGET)
                .expect("AIO charge slot must clear HR 20");
            assert_eq!(flag.value, 0);
            let global = writes.iter().find(|w| w.address == HR_CHARGE_TARGET_SOC);
            assert!(global.is_none(), "HR 116 must NOT be written when target_soc=100");
        })
        .await;
    }

    /// Enabling charge slot 3 on the All-in-One must write HR 246/247.
    #[tokio::test]
    async fn aio_charge_slot3_enable() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_CHARGE_SLOT_3_END, HR_CHARGE_SLOT_3_START, HR_ENABLE_CHARGE,
            };
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({
                "slot": 3,
                "start_hour": 12, "start_minute": 0,
                "end_hour": 14, "end_minute": 0,
                "enabled": true,
                "target_soc": 100,
            });
            let _ = set_charge_slot(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            let start = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_SLOT_3_START)
                .expect("AIO charge slot 3 must write HR 246");
            assert_eq!(start.value, 1200);
            let end = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_SLOT_3_END)
                .expect("AIO charge slot 3 must write HR 247");
            assert_eq!(end.value, 1400);
            let enable = writes
                .iter()
                .find(|w| w.address == HR_ENABLE_CHARGE)
                .expect("AIO charge slot 3 must set HR 96");
            assert_eq!(enable.value, 1);
        })
        .await;
    }

    /// Disabling charge slot 3 on the All-in-One must zero HR 246/247 and clear HR 96.
    #[tokio::test]
    async fn aio_charge_slot3_disable() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_CHARGE_SLOT_3_END, HR_CHARGE_SLOT_3_START, HR_ENABLE_CHARGE,
            };
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({
                "slot": 3,
                "start_hour": 12, "start_minute": 0,
                "end_hour": 14, "end_minute": 0,
                "enabled": false,
            });
            let _ = set_charge_slot(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            let start = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_SLOT_3_START)
                .expect("AIO charge slot 3 disable must write HR 246");
            assert_eq!(start.value, 0, "slot 3 start must be cleared on disable");
            let end = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_SLOT_3_END)
                .expect("AIO charge slot 3 disable must write HR 247");
            assert_eq!(end.value, 0, "slot 3 end must be cleared on disable");
            let enable = writes
                .iter()
                .find(|w| w.address == HR_ENABLE_CHARGE)
                .expect("AIO charge slot 3 disable must clear HR 96");
            assert_eq!(enable.value, 0, "HR 96 must be 0 on disable");
        })
        .await;
    }

    /// Enabling discharge slot 1 on the All-in-One must write HR 56/57.
    #[tokio::test]
    async fn aio_discharge_slot1_enable() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START,
            };
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 16, "start_minute": 0,
                "end_hour": 19, "end_minute": 0,
                "enabled": true,
            });
            let _ = set_discharge_slot(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            let start = writes
                .iter()
                .find(|w| w.address == HR_DISCHARGE_SLOT_1_START)
                .expect("AIO discharge slot 1 must write HR 56");
            assert_eq!(start.value, 1600);
            let end = writes
                .iter()
                .find(|w| w.address == HR_DISCHARGE_SLOT_1_END)
                .expect("AIO discharge slot 1 must write HR 57");
            assert_eq!(end.value, 1900);
        })
        .await;
    }

    /// Disabling discharge slot 1 on the All-in-One must zero HR 56/57.
    #[tokio::test]
    async fn aio_discharge_slot1_disable() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START,
            };
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 16, "start_minute": 0,
                "end_hour": 19, "end_minute": 0,
                "enabled": false,
            });
            let _ = set_discharge_slot(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            let start = writes
                .iter()
                .find(|w| w.address == HR_DISCHARGE_SLOT_1_START)
                .expect("AIO discharge slot 1 disable must write HR 56");
            assert_eq!(start.value, 0, "slot 1 start must be cleared on disable");
            let end = writes
                .iter()
                .find(|w| w.address == HR_DISCHARGE_SLOT_1_END)
                .expect("AIO discharge slot 1 disable must write HR 57");
            assert_eq!(end.value, 0, "slot 1 end must be cleared on disable");
        })
        .await;
    }

    /// Invalid slot numbers must be rejected for the All-in-One.
    #[tokio::test]
    async fn aio_charge_slot_invalid_slot() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({
                "slot": 0,
                "start_hour": 6, "start_minute": 0,
                "end_hour": 10, "end_minute": 0,
                "enabled": true,
            });
            let (status, _) = set_charge_slot(State(state.clone()), Json(body)).await;
            assert!(!status.is_success(), "slot 0 must be rejected");
            let body = serde_json::json!({
                "slot": 11,
                "start_hour": 6, "start_minute": 0,
                "end_hour": 10, "end_minute": 0,
                "enabled": true,
            });
            let (status, _) = set_charge_slot(State(state.clone()), Json(body)).await;
            assert!(!status.is_success(), "slot 11 must be rejected");
        })
        .await;
    }

    /// Invalid times must be rejected for the All-in-One.
    #[tokio::test]
    async fn aio_charge_slot_invalid_times() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 24, "start_minute": 0,
                "end_hour": 10, "end_minute": 0,
                "enabled": true,
            });
            let (status, _) = set_charge_slot(State(state.clone()), Json(body)).await;
            assert!(!status.is_success(), "hour 24 must be rejected");
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 6, "start_minute": 0,
                "end_hour": 10, "end_minute": 60,
                "enabled": true,
            });
            let (status, _) = set_charge_slot(State(state.clone()), Json(body)).await;
            assert!(!status.is_success(), "minute 60 must be rejected");
        })
        .await;
    }

    /// Charge limit 0 and 50 must be accepted, 51 must be rejected for AIO.
    #[tokio::test]
    async fn aio_charge_limit_0_50_51() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_BATTERY_CHARGE_LIMIT;
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({ "limit": 0 });
            let (status, _) = set_charge_rate(State(state.clone()), Json(body)).await;
            assert!(status.is_success(), "charge limit 0 must be accepted");
            let writes = drain_pending_writes(&state).await;
            assert_eq!(writes[0].address, HR_BATTERY_CHARGE_LIMIT);
            assert_eq!(writes[0].value, 0);
            let body = serde_json::json!({ "limit": 50 });
            let (status, _) = set_charge_rate(State(state.clone()), Json(body)).await;
            assert!(status.is_success(), "charge limit 50 must be accepted");
            let writes = drain_pending_writes(&state).await;
            assert_eq!(writes[0].value, 50);
            let body = serde_json::json!({ "limit": 51 });
            let (status, _) = set_charge_rate(State(state.clone()), Json(body)).await;
            assert!(!status.is_success(), "charge limit 51 must be rejected");
        })
        .await;
    }

    /// Discharge limit 0 and 50 must be accepted, 51 must be rejected for AIO.
    #[tokio::test]
    async fn aio_discharge_limit_0_50_51() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_BATTERY_DISCHARGE_LIMIT;
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({ "limit": 0 });
            let (status, _) = set_discharge_rate(State(state.clone()), Json(body)).await;
            assert!(status.is_success(), "discharge limit 0 must be accepted");
            let writes = drain_pending_writes(&state).await;
            assert_eq!(writes[0].address, HR_BATTERY_DISCHARGE_LIMIT);
            assert_eq!(writes[0].value, 0);
            let body = serde_json::json!({ "limit": 50 });
            let (status, _) = set_discharge_rate(State(state.clone()), Json(body)).await;
            assert!(status.is_success(), "discharge limit 50 must be accepted");
            let writes = drain_pending_writes(&state).await;
            assert_eq!(writes[0].value, 50);
            let body = serde_json::json!({ "limit": 51 });
            let (status, _) = set_discharge_rate(State(state.clone()), Json(body)).await;
            assert!(!status.is_success(), "discharge limit 51 must be rejected");
        })
        .await;
    }

    /// Enabling a charge slot with target_soc=4 (minimum) must write HR 116 = 4.
    #[tokio::test]
    async fn aio_charge_slot_target_soc_4() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_CHARGE_SLOT_1_START, HR_CHARGE_TARGET_SOC,
                HR_ENABLE_CHARGE, HR_ENABLE_CHARGE_TARGET,
            };
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 6, "start_minute": 0,
                "end_hour": 10, "end_minute": 0,
                "enabled": true,
                "target_soc": 4,
            });
            let _ = set_charge_slot(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            let start = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_SLOT_1_START)
                .expect("AIO charge slot must write HR 94");
            assert_eq!(start.value, 600);
            let global = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_TARGET_SOC)
                .expect("AIO charge slot with target_soc=4 must write HR 116");
            assert_eq!(global.value, 4, "HR 116 must be 4");
            let flag = writes
                .iter()
                .find(|w| w.address == HR_ENABLE_CHARGE_TARGET)
                .expect("AIO charge slot must clear HR 20");
            assert_eq!(flag.value, 0);
            let enable = writes
                .iter()
                .find(|w| w.address == HR_ENABLE_CHARGE)
                .expect("AIO charge slot must set HR 96");
            assert_eq!(enable.value, 1);
        })
        .await;
    }

    /// Enabling a charge slot with target_soc=100 when HR 116 was previously
    /// set to 80 must NOT overwrite HR 116 (100 = no limit).
    #[tokio::test]
    async fn aio_charge_slot_target_soc_100_with_previous_hr116() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_CHARGE_SLOT_1_START, HR_CHARGE_TARGET_SOC,
                HR_ENABLE_CHARGE, HR_ENABLE_CHARGE_TARGET,
            };
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 6, "start_minute": 0,
                "end_hour": 10, "end_minute": 0,
                "enabled": true,
                "target_soc": 100,
            });
            let _ = set_charge_slot(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            let start = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_SLOT_1_START)
                .expect("AIO charge slot must write HR 94");
            assert_eq!(start.value, 600);
            let global = writes.iter().find(|w| w.address == HR_CHARGE_TARGET_SOC);
            assert!(global.is_none(), "HR 116 must NOT be written when target_soc=100");
            let flag = writes
                .iter()
                .find(|w| w.address == HR_ENABLE_CHARGE_TARGET)
                .expect("AIO charge slot must clear HR 20");
            assert_eq!(flag.value, 0);
            let enable = writes
                .iter()
                .find(|w| w.address == HR_ENABLE_CHARGE)
                .expect("AIO charge slot must set HR 96");
            assert_eq!(enable.value, 1);
        })
        .await;
    }

    #[tokio::test]
    async fn set_eco_mode_only_emits_whitelisted_writes() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_END,
                HR_DISCHARGE_SLOT_2_START,
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
                assert!(
                    w.is_some(),
                    "eco mode must clear discharge slot register {}",
                    reg
                );
                assert_eq!(w.unwrap().value, 0);
            }
        })
        .await;
    }

    #[tokio::test]
    async fn pause_battery_single_phase_only_emits_whitelisted_writes() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_BATTERY_SOC_RESERVE, HR_DISCHARGE_SLOT_1_START,
                HR_DISCHARGE_SLOT_2_START,
            };
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let _ = pause_battery(State(state.clone())).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            // Pause clears discharge slots and restores eco power mode.
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_1_START && w.value == 0),
                "pause must clear discharge slot 1"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_2_START && w.value == 0),
                "pause must clear discharge slot 2"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1),
                "pause must restore eco power mode"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 100),
                "pause must set SOC reserve to 100 so Eco Paused actually pauses discharge"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn pause_battery_three_phase_only_emits_whitelisted_writes() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_3PH_AC_CHARGE_ENABLE, HR_3PH_DISCHARGE_SLOT_1_START, HR_3PH_FORCE_CHARGE_ENABLE,
                HR_3PH_FORCE_DISCHARGE_ENABLE,
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
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_FORCE_CHARGE_ENABLE && w.value == 0),
                "three-phase pause must clear force charge flag"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_FORCE_DISCHARGE_ENABLE && w.value == 0),
                "three-phase pause must clear force discharge flag"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_AC_CHARGE_ENABLE && w.value == 0),
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
                // Gateway is single-phase-class for control (issue #149): not
                // AC-coupled, and not three-phase for schedule-slot routing.
                (DeviceType::Gateway, false, false),
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
                assert_eq!(
                    ac,
                    matches!(resolved, DeviceType::ACCoupled | DeviceType::ACCoupledMk2)
                );
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
                // All-in-One family uses the DC-hybrid HR 111 (not AC HR 313).
                (DeviceType::AllInOne6kW, HR_BATTERY_CHARGE_LIMIT),
                (DeviceType::AllInOne3_6kW, HR_BATTERY_CHARGE_LIMIT),
                (DeviceType::AllInOne5kW, HR_BATTERY_CHARGE_LIMIT),
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
                // All-in-One family uses the DC-hybrid HR 112 (not AC HR 314).
                (DeviceType::AllInOne6kW, HR_BATTERY_DISCHARGE_LIMIT),
                (DeviceType::AllInOne3_6kW, HR_BATTERY_DISCHARGE_LIMIT),
                (DeviceType::AllInOne5kW, HR_BATTERY_DISCHARGE_LIMIT),
            ];
            for (dt, want_reg) in cases {
                let state = make_state_with_device(dt).await;
                let body = serde_json::json!({ "limit": 25 });
                let _ = set_discharge_rate(State(state.clone()), Json(body)).await;
                let writes = drain_pending_writes(&state).await;
                assert_all_whitelisted(&writes);
                assert_eq!(writes.len(), 1, "one register write expected for {:?}", dt);
                assert_eq!(
                    writes[0].address, want_reg,
                    "wrong discharge-rate register for {:?}",
                    dt
                );
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

            // All-in-One uses the single-phase path (HR 110).
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let _ = set_reserve(State(state.clone()), Json(json!({ "soc": 20 }))).await;
            let writes = drain_pending_writes(&state).await;
            assert_eq!(writes[0].address, HR_BATTERY_SOC_RESERVE);
        })
        .await;
    }

    #[tokio::test]
    async fn force_charge_routes_single_phase_vs_three_phase() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_3PH_CHARGE_TARGET_SOC, HR_CHARGE_TARGET_SOC};
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let _ = force_charge(State(state.clone()), None).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes.iter().any(|w| w.address == HR_CHARGE_TARGET_SOC),
                "single-phase force charge must target HR_CHARGE_TARGET_SOC"
            );

            let state = make_state_with_device(DeviceType::ThreePhase).await;
            let _ = force_charge(State(state.clone()), None).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes.iter().any(|w| w.address == HR_3PH_CHARGE_TARGET_SOC),
                "three-phase force charge must target HR_3PH_CHARGE_TARGET_SOC"
            );

            // All-in-One uses the single-phase path (HR 116).
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let _ = force_charge(State(state.clone()), None).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes.iter().any(|w| w.address == HR_CHARGE_TARGET_SOC),
                "AIO force charge must target HR_CHARGE_TARGET_SOC"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_charge_with_minutes_writes_charge_slot_before_enable() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_CHARGE_SLOT_1_END, HR_CHARGE_SLOT_1_START, HR_ENABLE_CHARGE,
            };
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let _ = force_charge(State(state.clone()), Some(Json(json!({ "minutes": 30 })))).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);

            let start_idx = writes
                .iter()
                .position(|w| w.address == HR_CHARGE_SLOT_1_START)
                .expect("force charge must write charge slot start");
            let end_idx = writes
                .iter()
                .position(|w| w.address == HR_CHARGE_SLOT_1_END)
                .expect("force charge must write charge slot end");
            let enable_idx = writes
                .iter()
                .position(|w| w.address == HR_ENABLE_CHARGE)
                .expect("force charge must enable charge after slot is present");

            assert!(
                start_idx < enable_idx,
                "slot start must be written before HR96=1"
            );
            assert!(
                end_idx < enable_idx,
                "slot end must be written before HR96=1"
            );
            assert_ne!(writes[start_idx].value, writes[end_idx].value);
        })
        .await;
    }

    #[tokio::test]
    async fn force_discharge_routes_single_phase_vs_three_phase() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_3PH_FORCE_DISCHARGE_ENABLE, HR_ENABLE_DISCHARGE};
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let _ = force_discharge(State(state.clone()), None).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 1),
                "single-phase force discharge must set HR_ENABLE_DISCHARGE=1"
            );

            let state = make_state_with_device(DeviceType::ThreePhase).await;
            let _ = force_discharge(State(state.clone()), None).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_FORCE_DISCHARGE_ENABLE && w.value == 1),
                "three-phase force discharge must set HR_3PH_FORCE_DISCHARGE_ENABLE=1"
            );

            // All-in-One uses the single-phase path (HR 59).
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let _ = force_discharge(State(state.clone()), None).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 1),
                "AIO force discharge must set HR_ENABLE_DISCHARGE=1"
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
                HR_3PH_DISCHARGE_SLOT_1_START, HR_BATTERY_POWER_MODE, HR_DISCHARGE_SLOT_1_START,
            };
            // Single-phase: classic slots cleared + eco power mode restored.
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let _ = pause_battery(State(state.clone())).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_1_START && w.value == 0),
                "single-phase pause clears HR 56 slot 1"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1),
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
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_DISCHARGE_SLOT_1_START && w.value == 0),
                "three-phase pause clears HR 1118 slot 1"
            );
            assert!(
                !writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_1_START),
                "three-phase pause must NOT touch single-phase slot registers"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1),
                "three-phase pause restores eco power mode"
            );
        })
        .await;
    }

    // -----------------------------------------------------------------------
    // Additional device-type routing tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn pause_battery_gen2_hybrid_only_emits_whitelisted_writes() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_BATTERY_SOC_RESERVE, HR_DISCHARGE_SLOT_1_START,
                HR_DISCHARGE_SLOT_2_START,
            };
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let _ = pause_battery(State(state.clone())).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_1_START && w.value == 0),
                "gen2 pause must clear discharge slot 1"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_2_START && w.value == 0),
                "gen2 pause must clear discharge slot 2"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1),
                "gen2 pause must restore eco power mode"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 100),
                "gen2 pause must set SOC reserve to 100"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn pause_battery_ac_coupled_only_emits_whitelisted_writes() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_BATTERY_SOC_RESERVE, HR_DISCHARGE_SLOT_1_START,
            };
            let state = make_state_with_device(DeviceType::ACCoupled).await;
            let _ = pause_battery(State(state.clone())).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            // AC Coupled only has 1 discharge slot, but clear_discharge_slot_writes
            // still writes both slots (HR 56-57 pair), so slot 1 gets cleared.
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_1_START && w.value == 0),
                "ac pause must clear discharge slot 1"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1),
                "ac pause must restore eco power mode"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 100),
                "ac pause must set SOC reserve to 100"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_charge_ac_coupled_uses_ac_registers() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_ENABLE_DISCHARGE;
            let state = make_state_with_device(DeviceType::ACCoupled).await;
            let _ = force_charge(State(state.clone()), None).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            // AC Coupled is not three-phase, so uses single-phase force charge.
            // Force charge writes: eco mode (HR27=1), clear discharge (HR59=0),
            // enable charge (HR96=1), enable charge target (HR20=1), target SOC (HR116=100).
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0),
                "AC force charge must clear discharge"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_charge_with_edge_minutes_0_uses_clamp() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_CHARGE_SLOT_1_START;
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let _ = force_charge(State(state.clone()), Some(Json(json!({ "minutes": 0 })))).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            // minutes=0 is clamped to 1, so a charge slot should be written.
            assert!(
                writes.iter().any(|w| w.address == HR_CHARGE_SLOT_1_START),
                "force charge with minutes=0 should still write charge slot (clamped to 1)"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_charge_with_edge_minutes_1439() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_CHARGE_SLOT_1_END, HR_CHARGE_SLOT_1_START};
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let _ =
                force_charge(State(state.clone()), Some(Json(json!({ "minutes": 1439 })))).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes.iter().any(|w| w.address == HR_CHARGE_SLOT_1_START),
                "force charge with minutes=1439 should write charge slot"
            );
            assert!(
                writes.iter().any(|w| w.address == HR_CHARGE_SLOT_1_END),
                "force charge with minutes=1439 should write charge slot end"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_charge_ac_coupled_without_body_backward_compat() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_CHARGE_SLOT_1_START, HR_ENABLE_CHARGE};
            let state = make_state_with_device(DeviceType::ACCoupled).await;
            // No body at all — backward-compatible force charge without minutes.
            let _ = force_charge(State(state.clone()), None).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            // No charge slot writes since no minutes provided.
            let has_slot = writes.iter().any(|w| w.address == HR_CHARGE_SLOT_1_START);
            assert!(
                !has_slot,
                "force charge without body should not write charge slot"
            );
            // But force charge flags must be present.
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_CHARGE && w.value == 1),
                "force charge without body must enable charge"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_charge_ac_coupled_with_minutes() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_CHARGE_SLOT_1_START;
            let state = make_state_with_device(DeviceType::ACCoupled).await;
            let _ = force_charge(State(state.clone()), Some(Json(json!({ "minutes": 30 })))).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes.iter().any(|w| w.address == HR_CHARGE_SLOT_1_START),
                "AC force charge with minutes must write charge slot"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_discharge_for_each_device_family() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_3PH_FORCE_DISCHARGE_ENABLE, HR_ENABLE_DISCHARGE};

            // Gen2: single-phase force discharge
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let _ = force_discharge(State(state.clone()), None).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 1),
                "Gen2 force discharge must set HR_ENABLE_DISCHARGE=1"
            );

            // Gen3: single-phase force discharge
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let _ = force_discharge(State(state.clone()), None).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 1),
                "Gen3 force discharge must set HR_ENABLE_DISCHARGE=1"
            );

            // AC Coupled: single-phase force discharge
            let state = make_state_with_device(DeviceType::ACCoupled).await;
            let _ = force_discharge(State(state.clone()), None).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 1),
                "AC force discharge must set HR_ENABLE_DISCHARGE=1"
            );

            // ThreePhase: three-phase force discharge
            let state = make_state_with_device(DeviceType::ThreePhase).await;
            let _ = force_discharge(State(state.clone()), None).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_FORCE_DISCHARGE_ENABLE && w.value == 1),
                "ThreePhase force discharge must set HR_3PH_FORCE_DISCHARGE_ENABLE=1"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn reserve_writes_for_device_single_phase_vs_three_phase() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_3PH_BATTERY_SOC_RESERVE, HR_BATTERY_SOC_RESERVE};

            let writes = reserve_writes_for_device(DeviceType::Gen2Hybrid, 30).unwrap();
            assert_eq!(writes[0].address, HR_BATTERY_SOC_RESERVE);
            assert_eq!(writes[0].value, 30);

            let writes = reserve_writes_for_device(DeviceType::ThreePhase, 30).unwrap();
            assert_eq!(writes[0].address, HR_3PH_BATTERY_SOC_RESERVE);
            assert_eq!(writes[0].value, 30);
        })
        .await;
    }

    #[tokio::test]
    async fn clear_discharge_slot_writes_three_phase() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_3PH_DISCHARGE_SLOT_1_END, HR_3PH_DISCHARGE_SLOT_1_START,
                HR_3PH_DISCHARGE_SLOT_2_END, HR_3PH_DISCHARGE_SLOT_2_START, SAFE_WRITE_REGS,
            };

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
        })
        .await;
    }

    #[tokio::test]
    async fn set_charge_slot_2_non_gen3_uses_classic_hr31_32() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_CHARGE_SLOT_2_END, HR_CHARGE_SLOT_2_START};
            // Gen3PlusHybrid supports 2 charge slots via classic HR31-32
            // (not gen3-extended).
            let state = make_state_with_device(DeviceType::Gen3PlusHybrid).await;
            let body = serde_json::json!({
                "slot": 2,
                "start_hour": 23, "start_minute": 0,
                "end_hour": 1, "end_minute": 0,
                "enabled": true,
            });
            let _ = set_charge_slot(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_CHARGE_SLOT_2_START && w.value == 2300),
                "Non-gen3 charge slot 2 must write classic HR31"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_CHARGE_SLOT_2_END && w.value == 100),
                "Non-gen3 charge slot 2 must write classic HR32"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn set_charge_slot_2_gen3_uses_extended_hr243_244() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_CHARGE_SLOT_2_GEN3_END, HR_CHARGE_SLOT_2_GEN3_START,
            };
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let body = serde_json::json!({
                "slot": 2,
                "start_hour": 3, "start_minute": 15,
                "end_hour": 4, "end_minute": 15,
                "enabled": true,
            });
            let _ = set_charge_slot(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_CHARGE_SLOT_2_GEN3_START && w.value == 315),
                "Gen3 charge slot 2 must write extended HR243"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_CHARGE_SLOT_2_GEN3_END && w.value == 415),
                "Gen3 charge slot 2 must write extended HR244"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn set_eps_rejects_dc_hybrid() {
        with_isolated_config_dir_async(|| async {
            // DC hybrids have no AC output stage, so HR 317 is meaningless
            // and the firmware silently drops the write. The API should
            // refuse with a clear error rather than returning success.
            let cases = [
                DeviceType::Gen1Hybrid,
                DeviceType::Gen2Hybrid,
                DeviceType::Gen3Hybrid,
                DeviceType::Gen4Hybrid,
                DeviceType::PolarHybrid,
                DeviceType::Gen3PlusHybrid,
                DeviceType::ThreePhase,
                DeviceType::AioCommercial,
                DeviceType::HybridHvGen3,
                DeviceType::AllInOneHybrid,
                DeviceType::Gateway,
                DeviceType::Ems,
                DeviceType::PvInverter,
            ];
            for dt in cases {
                let state = make_state_with_device(dt).await;
                let body = serde_json::json!({ "enabled": true });
                let (status, payload) =
                    set_eps(State(state.clone()), Json(body)).await;
                assert_eq!(
                    status,
                    StatusCode::BAD_REQUEST,
                    "expected 400 for {:?}, got {:?}",
                    dt,
                    payload
                );
                assert_eq!(
                    payload["ok"], false,
                    "expected ok=false for {:?}, got {:?}",
                    dt,
                    payload
                );
                assert!(
                    payload["error"]
                        .as_str()
                        .unwrap_or("")
                        .contains("Emergency Power Supply"),
                    "expected clear EPS error for {:?}, got {:?}",
                    dt,
                    payload
                );
                let writes = drain_pending_writes(&state).await;
                assert!(
                    writes.is_empty(),
                    "handler must not queue writes for unsupported {:?}, got {:?}",
                    dt,
                    writes
                );
            }
        })
        .await;
    }

    #[tokio::test]
    async fn set_eps_writes_hr317_for_supported_devices() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_ENABLE_EPS;
            let cases = [
                DeviceType::ACCoupled,
                DeviceType::ACCoupledMk2,
                DeviceType::ACThreePhase,
                DeviceType::AllInOne6kW,
                DeviceType::AllInOne3_6kW,
                DeviceType::AllInOne5kW,
            ];
            for dt in cases {
                let state = make_state_with_device(dt).await;
                let body = serde_json::json!({ "enabled": true });
                let (status, payload) =
                    set_eps(State(state.clone()), Json(body)).await;
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "expected 200 for {:?}, got {:?}",
                    dt,
                    payload
                );
                assert_eq!(payload["ok"], true);
                let writes = drain_pending_writes(&state).await;
                assert_all_whitelisted(&writes);
                assert!(
                    writes
                        .iter()
                        .any(|w| w.address == HR_ENABLE_EPS && w.value == 1),
                    "expected HR 317 = 1 for {:?}, got {:?}",
                    dt,
                    writes
                );
            }

            // Same path with enabled=false should write 0.
            let state = make_state_with_device(DeviceType::ACCoupled).await;
            let body = serde_json::json!({ "enabled": false });
            let (status, _) = set_eps(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let writes = drain_pending_writes(&state).await;
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_EPS && w.value == 0),
                "expected HR 317 = 0 for disable, got {:?}",
                writes
            );
        })
        .await;
    }

    #[tokio::test]
    async fn set_eps_rejects_missing_enabled_field() {
        with_isolated_config_dir_async(|| async {
            // Even on a supported device, an invalid body should still 400.
            let state = make_state_with_device(DeviceType::ACCoupled).await;
            let body = serde_json::json!({});
            let (status, payload) = set_eps(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(payload["ok"], false);
            assert!(
                payload["error"]
                    .as_str()
                    .unwrap_or("")
                    .contains("Missing"),
                "expected 'Missing' error, got {:?}",
                payload
            );
            let writes = drain_pending_writes(&state).await;
            assert!(writes.is_empty());
        })
        .await;
    }

    // -----------------------------------------------------------------------
    // Force Charge / Stop Charge round-trip tests
    //
    // These exercise the snapshot/restore path that lets the user click
    // Stop Charge and have the inverter return to its pre-force-charge
    // state. Mirrors GivTCP's `forceCharge`/`FCResume` behaviour.
    // -----------------------------------------------------------------------

    /// Helper: seed the latest snapshot with a representative "user had
    /// Eco mode + a 02:00–04:00 charge slot + target SOC 60 + battery
    /// pause mode 0" pre-state. Returns the state for the caller to use.
    async fn seed_charging_pre_state(state: &Arc<AppState>) {
        use crate::inverter::model::ScheduleSlot;
        let mut snap = crate::inverter::model::InverterSnapshot {
            device_type: DeviceType::Gen2Hybrid,
            // User was in Timed Demand mode (eco=1, discharge enabled).
            // ForceCharge start will clobber enable_discharge to 0; the
            // stop path must restore it.
            enable_charge: true,
            enable_discharge: true,
            target_soc: 60,
            charge_rate: 35,
            battery_power_mode: 1, // eco
            charge_slots: Default::default(),
            discharge_slots: Default::default(),
            battery_pause_mode: 0,
            ..Default::default()
        };
        snap.charge_slots[0] = ScheduleSlot {
            enabled: true,
            start_hour: 2,
            start_minute: 0,
            end_hour: 4,
            end_minute: 0,
            target_soc: 60,
        };
        *state.latest_snapshot.lock().await = Some(snap);
    }

    #[tokio::test]
    async fn force_charge_captures_pre_state_into_revert() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            seed_charging_pre_state(&state).await;

            // Sanity: revert is None before force-charge.
            assert!(state.force_charge_revert.lock().await.is_none());

            let _ = force_charge(State(state.clone()), Some(Json(json!({ "minutes": 30 })))).await;
            // Drain the force-charge writes (we don't care about them here).
            let _ = drain_pending_writes(&state).await;

            let revert = state
                .force_charge_revert
                .lock()
                .await
                .clone()
                .expect("force_charge should have populated the revert");
            assert!(revert.enable_charge, "pre-state had enable_charge=true");
            assert!(
                revert.enable_discharge,
                "pre-state had enable_discharge=true"
            );
            assert_eq!(revert.target_soc, 60);
            assert_eq!(revert.battery_power_mode, 1, "pre-state was in eco");
            assert_eq!(revert.charge_rate, Some(35));
            assert_eq!(revert.charge_slot_1_start, Some((2, 0)));
            assert_eq!(revert.charge_slot_1_end, Some((4, 0)));
            assert!(
                revert.three_phase_force_charge_enable.is_none(),
                "single-phase should not capture three-phase flags"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_charge_captures_none_slot_when_no_prior_slot() {
        with_isolated_config_dir_async(|| async {
            // Pre-state has enable_charge=true but no charge slot configured.
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let snap = crate::inverter::model::InverterSnapshot {
                device_type: DeviceType::Gen2Hybrid,
                enable_charge: true,
                target_soc: 80,
                charge_rate: 50,
                ..Default::default()
            };
            *state.latest_snapshot.lock().await = Some(snap);

            let _ = force_charge(State(state.clone()), Some(Json(json!({ "minutes": 30 })))).await;
            let _ = drain_pending_writes(&state).await;

            let revert = state.force_charge_revert.lock().await.clone().unwrap();
            assert!(revert.enable_charge);
            assert_eq!(revert.target_soc, 80);
            assert!(
                revert.charge_slot_1_start.is_none() && revert.charge_slot_1_end.is_none(),
                "no prior slot should mean None"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_charge_stop_restores_captured_state_single_phase() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_CHARGE_SLOT_1_END, HR_CHARGE_SLOT_1_START,
                HR_CHARGE_TARGET_SOC, HR_ENABLE_CHARGE, HR_ENABLE_CHARGE_TARGET,
                HR_ENABLE_DISCHARGE,
            };

            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            seed_charging_pre_state(&state).await;

            // Start force charge, then stop it.
            let _ = force_charge(State(state.clone()), Some(Json(json!({ "minutes": 30 })))).await;
            let _ = drain_pending_writes(&state).await;

            let (status, payload) = force_charge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK, "stop should succeed: {:?}", payload);
            assert_eq!(payload["ok"], true);

            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes.iter().any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 1),
                "stop should restore enable_discharge to its pre-force value (1) — \
                 ForceCharge start clears it, and not restoring it leaves the \
                 user's discharge schedule silently disabled"
            );
            assert!(
                writes.iter().any(|w| w.address == HR_ENABLE_CHARGE && w.value == 1),
                "stop should restore enable_charge to its pre-force value (1)"
            );
            assert!(
                writes.iter().any(|w| w.address == HR_ENABLE_CHARGE_TARGET && w.value == 1),
                "stop should restore enable_charge_target to match enable_charge"
            );
            assert!(
                writes.iter().any(|w| w.address == HR_CHARGE_TARGET_SOC && w.value == 60),
                "stop should restore target_soc to 60"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1),
                "stop should restore battery_power_mode to eco (1)"
            );
            // 02:00 = 200 HHMM encoded.
            let start = encode_hhmm(2, 0);
            // 04:00 = 400 HHMM encoded.
            let end = encode_hhmm(4, 0);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_CHARGE_SLOT_1_START && w.value == start),
                "stop should restore charge slot 1 start to 02:00"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_CHARGE_SLOT_1_END && w.value == end),
                "stop should restore charge slot 1 end to 04:00"
            );

            // The revert should be consumed.
            assert!(
                state.force_charge_revert.lock().await.is_none(),
                "stop should clear the revert"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_charge_stop_clears_slot_when_pre_state_had_none() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_CHARGE_SLOT_1_END, HR_CHARGE_SLOT_1_START};

            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            // No prior slot.
            let snap = crate::inverter::model::InverterSnapshot {
                device_type: DeviceType::Gen2Hybrid,
                enable_charge: true,
                target_soc: 80,
                charge_rate: 50,
                ..Default::default()
            };
            *state.latest_snapshot.lock().await = Some(snap);

            let _ = force_charge(State(state.clone()), Some(Json(json!({ "minutes": 30 })))).await;
            let _ = drain_pending_writes(&state).await;

            let (status, _) = force_charge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);

            let writes = drain_pending_writes(&state).await;
            // Slot registers should be cleared to (0, 0).
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_CHARGE_SLOT_1_START && w.value == 0),
                "stop should clear the charge slot start to 0"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_CHARGE_SLOT_1_END && w.value == 0),
                "stop should clear the charge slot end to 0"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_charge_stop_three_phase_clears_force_and_ac_flags() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_3PH_AC_CHARGE_ENABLE, HR_3PH_FORCE_CHARGE_ENABLE, HR_BATTERY_POWER_MODE,
            };

            let state = make_state_with_device(DeviceType::ThreePhase).await;
            // Pre-state: 3PH, force charge enabled, in Max-Power (export) mode.
            // ForceCharge start writes HR_BATTERY_POWER_MODE=1, so on stop
            // we must restore the pre-value (0) instead of leaving the user
            // stuck in eco.
            let snap = crate::inverter::model::InverterSnapshot {
                device_type: DeviceType::ThreePhase,
                enable_charge: true,
                target_soc: 90,
                battery_power_mode: 0,
                ..Default::default()
            };
            *state.latest_snapshot.lock().await = Some(snap);

            let _ = force_charge(State(state.clone()), None).await;
            let _ = drain_pending_writes(&state).await;

            let (status, _) = force_charge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);

            let writes = drain_pending_writes(&state).await;
            // Three-phase force/AC flags should be restored to 1 (the pre-state).
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_FORCE_CHARGE_ENABLE && w.value == 1),
                "3PH stop should restore HR_3PH_FORCE_CHARGE_ENABLE=1"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_AC_CHARGE_ENABLE && w.value == 1),
                "3PH stop should restore HR_3PH_AC_CHARGE_ENABLE=1"
            );
            // Battery power mode should be restored to the pre-state (0 = export).
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 0),
                "3PH stop should restore HR_BATTERY_POWER_MODE to pre-state (0 = export), \
                 not leave the user in eco (1)"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_charge_stop_without_active_force_charge_errors() {
        with_isolated_config_dir_async(|| async {
            // No force_charge_revert captured, no force_charge has been called.
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;

            let (status, payload) = force_charge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(payload["ok"], false);
            let writes = drain_pending_writes(&state).await;
            assert!(
                writes.is_empty(),
                "stop with no active force charge should queue no writes"
            );
        })
        .await;
    }

    /// Regression test for the bug where Stop Force Charge left the
    /// discharge schedule silently disabled. Specifically: a user in
    /// Timed Demand mode (eco=1, enable_discharge=true) hits Force Charge,
    /// which writes HR_ENABLE_DISCHARGE=0, then Stop. Before this fix the
    /// stop path didn't restore HR_ENABLE_DISCHARGE, leaving the user
    /// with a discharge schedule that no longer fires.
    #[tokio::test]
    async fn force_charge_stop_restores_discharge_flag() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_ENABLE_DISCHARGE;

            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            // Pre-state: user in Timed Demand (discharge enabled).
            let snap = crate::inverter::model::InverterSnapshot {
                device_type: DeviceType::Gen2Hybrid,
                enable_charge: true,
                enable_discharge: true,
                target_soc: 60,
                battery_power_mode: 1,
                ..Default::default()
            };
            *state.latest_snapshot.lock().await = Some(snap);

            let _ = force_charge(State(state.clone()), Some(Json(json!({ "minutes": 30 })))).await;
            let _ = drain_pending_writes(&state).await;

            // The capture should have read enable_discharge=true.
            let revert = state.force_charge_revert.lock().await.clone().unwrap();
            assert!(revert.enable_discharge);

            let (status, _) = force_charge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);
            let writes = drain_pending_writes(&state).await;
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 1),
                "Stop Force Charge must restore HR_ENABLE_DISCHARGE=1 — \
                 otherwise the user's discharge schedule is silently disabled"
            );
        })
        .await;
    }

    /// Regression test: a user in Max-Power (export) mode hits Force
    /// Charge, then Stop, and should be back in Max-Power (0), not stuck
    /// in eco (1).
    #[tokio::test]
    async fn force_charge_stop_restores_max_power_mode() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_BATTERY_POWER_MODE;

            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            // Pre-state: user in Max-Power (export) mode.
            let snap = crate::inverter::model::InverterSnapshot {
                device_type: DeviceType::Gen2Hybrid,
                enable_charge: true,
                target_soc: 60,
                battery_power_mode: 0, // export
                ..Default::default()
            };
            *state.latest_snapshot.lock().await = Some(snap);

            let _ = force_charge(State(state.clone()), Some(Json(json!({ "minutes": 30 })))).await;
            let _ = drain_pending_writes(&state).await;

            let (status, _) = force_charge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);
            let writes = drain_pending_writes(&state).await;
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 0),
                "Stop Force Charge must restore HR_BATTERY_POWER_MODE=0 (export) — \
                 otherwise the user is stuck in eco (1) after a force charge"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_charge_stop_is_one_shot_per_revert() {
        with_isolated_config_dir_async(|| async {
            // Calling stop a second time should fail because the first
            // call already consumed the revert.
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            seed_charging_pre_state(&state).await;
            let _ = force_charge(State(state.clone()), Some(Json(json!({ "minutes": 30 })))).await;
            let _ = drain_pending_writes(&state).await;

            let (status, _) = force_charge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);
            let _ = drain_pending_writes(&state).await;

            let (status, _) = force_charge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let writes = drain_pending_writes(&state).await;
            assert!(writes.is_empty(), "second stop should queue no writes");
        })
        .await;
    }

    // -----------------------------------------------------------------------
    // Force Discharge / Stop Discharge round-trip tests
    //
    // Mirror of the force-charge stop tests above. Exercises the
    // snapshot/restore path for the discharge side of the Quick Action.
    // -----------------------------------------------------------------------

    /// Helper: seed the latest snapshot with a representative "user had a
    /// 17:00–19:00 discharge slot + discharge enabled" pre-state.
    async fn seed_discharging_pre_state(state: &Arc<AppState>) {
        use crate::inverter::model::ScheduleSlot;
        let mut snap = crate::inverter::model::InverterSnapshot {
            device_type: DeviceType::Gen2Hybrid,
            enable_discharge: true,
            enable_charge: true,
            discharge_rate: 40,
            discharge_slots: Default::default(),
            ..Default::default()
        };
        snap.discharge_slots[0] = ScheduleSlot {
            enabled: true,
            start_hour: 17,
            start_minute: 0,
            end_hour: 19,
            end_minute: 0,
            target_soc: 50,
        };
        *state.latest_snapshot.lock().await = Some(snap);
    }

    #[tokio::test]
    async fn force_discharge_captures_pre_state_into_revert() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            seed_discharging_pre_state(&state).await;

            // Sanity: revert is None before force-discharge.
            assert!(state.force_discharge_revert.lock().await.is_none());

            let _ = force_discharge(State(state.clone()), None).await;
            let _ = drain_pending_writes(&state).await;

            let revert = state
                .force_discharge_revert
                .lock()
                .await
                .clone()
                .expect("force_discharge should have populated the revert");
            assert!(revert.enable_discharge, "pre-state had enable_discharge=true");
            assert!(revert.enable_charge, "pre-state had enable_charge=true");
            assert_eq!(revert.discharge_rate, Some(40));
            assert_eq!(revert.discharge_slot_1_start, Some((17, 0)));
            assert_eq!(revert.discharge_slot_1_end, Some((19, 0)));
            assert!(
                revert.discharge_slot_2_start.is_none() && revert.discharge_slot_2_end.is_none(),
                "no prior slot 2 should mean None"
            );
            assert!(
                revert.three_phase_force_discharge_enable.is_none(),
                "single-phase should not capture three-phase flags"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_discharge_stop_restores_captured_state_single_phase() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START,
                HR_DISCHARGE_SLOT_2_END, HR_DISCHARGE_SLOT_2_START, HR_ENABLE_CHARGE,
                HR_ENABLE_CHARGE_TARGET, HR_ENABLE_DISCHARGE,
            };

            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            seed_discharging_pre_state(&state).await;

            let _ = force_discharge(State(state.clone()), None).await;
            let _ = drain_pending_writes(&state).await;

            let (status, payload) = force_discharge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK, "stop should succeed: {:?}", payload);
            assert_eq!(payload["ok"], true);

            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes.iter().any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 1),
                "stop should restore enable_discharge to its pre-force value (1)"
            );
            assert!(
                writes.iter().any(|w| w.address == HR_ENABLE_CHARGE && w.value == 1),
                "stop should restore enable_charge to its pre-force value (1)"
            );
            assert!(
                writes.iter().any(|w| w.address == HR_ENABLE_CHARGE_TARGET && w.value == 1),
                "stop should restore enable_charge_target to match enable_charge"
            );
            // 17:00 = 1700 HHMM encoded.
            let start = encode_hhmm(17, 0);
            // 19:00 = 1900 HHMM encoded.
            let end = encode_hhmm(19, 0);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_1_START && w.value == start),
                "stop should restore discharge slot 1 start to 17:00"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_1_END && w.value == end),
                "stop should restore discharge slot 1 end to 19:00"
            );
            // Slot 2 was unconfigured, should be cleared.
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_2_START && w.value == 0),
                "stop should clear discharge slot 2 start"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_2_END && w.value == 0),
                "stop should clear discharge slot 2 end"
            );
            // Always return to eco mode (1) on stop — matches pause_battery.
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1),
                "stop should set battery power mode to eco (1)"
            );

            // The revert should be consumed.
            assert!(
                state.force_discharge_revert.lock().await.is_none(),
                "stop should clear the revert"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_discharge_stop_three_phase_restores_force_flags() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_3PH_FORCE_CHARGE_ENABLE, HR_3PH_FORCE_DISCHARGE_ENABLE};

            let state = make_state_with_device(DeviceType::ThreePhase).await;
            // Pre-state had three-phase force discharge enabled, force charge
            // disabled.
            let snap = crate::inverter::model::InverterSnapshot {
                device_type: DeviceType::ThreePhase,
                enable_discharge: true,
                enable_charge: false,
                ..Default::default()
            };
            *state.latest_snapshot.lock().await = Some(snap);

            let _ = force_discharge(State(state.clone()), None).await;
            let _ = drain_pending_writes(&state).await;

            let (status, _) = force_discharge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);

            let writes = drain_pending_writes(&state).await;
            // Pre-state had enable_discharge=true → restore 1.
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_FORCE_DISCHARGE_ENABLE && w.value == 1),
                "3PH stop should restore HR_3PH_FORCE_DISCHARGE_ENABLE=1"
            );
            // Pre-state had enable_charge=false → restore 0.
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_FORCE_CHARGE_ENABLE && w.value == 0),
                "3PH stop should restore HR_3PH_FORCE_CHARGE_ENABLE=0"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_discharge_stop_without_active_force_discharge_errors() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;

            let (status, payload) = force_discharge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(payload["ok"], false);
            let writes = drain_pending_writes(&state).await;
            assert!(
                writes.is_empty(),
                "stop with no active force discharge should queue no writes"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_discharge_stop_is_one_shot_per_revert() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            seed_discharging_pre_state(&state).await;
            let _ = force_discharge(State(state.clone()), None).await;
            let _ = drain_pending_writes(&state).await;

            let (status, _) = force_discharge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);
            let _ = drain_pending_writes(&state).await;

            let (status, _) = force_discharge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let writes = drain_pending_writes(&state).await;
            assert!(writes.is_empty(), "second stop should queue no writes");
        })
        .await;
    }

    /// When `minutes` is provided, the encoder's hardcoded 00:00–23:59
    /// discharge slot must be replaced with a `now → now+minutes` window.
    /// Without this, the duration slider has no effect on Force Discharge.
    #[tokio::test]
    async fn force_discharge_with_minutes_writes_duration_slot_not_full_day() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_END,
                HR_DISCHARGE_SLOT_2_START, HR_ENABLE_DISCHARGE,
            };

            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let _ = force_discharge(
                State(state.clone()),
                Some(Json(json!({ "minutes": 60 }))),
            )
            .await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);

            // Slot 1 start must be "now" (we can't pin the exact value
            // without freezing Local::now, so just assert the writes are
            // present and not the encoder's default of 0/2359).
            assert!(
                writes.iter().any(|w| w.address == HR_DISCHARGE_SLOT_1_START),
                "force discharge with minutes must write discharge slot 1 start"
            );
            assert!(
                writes.iter().any(|w| w.address == HR_DISCHARGE_SLOT_1_END),
                "force discharge with minutes must write discharge slot 1 end"
            );

            // Critically, the slot 1 end must NOT be 2359 (the encoder's
            // full-day default). For minutes=60, end is "now + 1h" which
            // is at most 23:59 in the rare wrap-around case but generally
            // not 23:59. To make this test deterministic, we assert the
            // end is not 2359 OR the start is not 0 — at least one of
            // them has to differ from the full-day default for the
            // duration to have had any effect.
            let slot1_end = writes
                .iter()
                .find(|w| w.address == HR_DISCHARGE_SLOT_1_END)
                .map(|w| w.value);
            let slot1_start = writes
                .iter()
                .find(|w| w.address == HR_DISCHARGE_SLOT_1_START)
                .map(|w| w.value);
            assert!(
                slot1_end != Some(2359) || slot1_start != Some(0),
                "force discharge with minutes=60 should not produce the encoder's \
                 full-day 00:00–23:59 slot — end={:?}, start={:?}",
                slot1_end,
                slot1_start
            );

            // Slot 2 must be cleared (00:00–00:00).
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_2_START && w.value == 0),
                "force discharge with minutes must clear slot 2 start"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_2_END && w.value == 0),
                "force discharge with minutes must clear slot 2 end"
            );

            // The force-discharge flag must still be armed.
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 1),
                "force discharge with minutes must still arm HR_ENABLE_DISCHARGE=1"
            );
        })
        .await;
    }

    /// The slot writes must come BEFORE the enable_discharge arm, mirroring
    /// the force-charge slot-before-enable ordering test. The inverter
    /// needs the slot to exist before enable_discharge=1 has anything
    /// to gate.
    #[tokio::test]
    async fn force_discharge_with_minutes_writes_slot_before_enable() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START, HR_ENABLE_DISCHARGE,
            };

            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let _ = force_discharge(
                State(state.clone()),
                Some(Json(json!({ "minutes": 30 }))),
            )
            .await;
            let writes = drain_pending_writes(&state).await;

            let start_idx = writes
                .iter()
                .position(|w| w.address == HR_DISCHARGE_SLOT_1_START)
                .expect("force discharge with minutes must write slot 1 start");
            let end_idx = writes
                .iter()
                .position(|w| w.address == HR_DISCHARGE_SLOT_1_END)
                .expect("force discharge with minutes must write slot 1 end");
            let enable_idx = writes
                .iter()
                .position(|w| w.address == HR_ENABLE_DISCHARGE)
                .expect("force discharge with minutes must arm enable_discharge");

            assert!(
                start_idx < enable_idx,
                "slot 1 start must be written before HR_ENABLE_DISCHARGE=1"
            );
            assert!(
                end_idx < enable_idx,
                "slot 1 end must be written before HR_ENABLE_DISCHARGE=1"
            );
        })
        .await;
    }

    /// The no-body path must keep the original behaviour: a 00:00–23:59
    /// slot so the discharge runs "until stopped". Regression guard for
    /// any future change that might collapse the two paths.
    #[tokio::test]
    async fn force_discharge_without_minutes_keeps_full_day_slot() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START};

            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let _ = force_discharge(State(state.clone()), None).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);

            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_1_START && w.value == 0),
                "no-body force discharge must keep slot 1 start = 00:00"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_1_END && w.value == 2359),
                "no-body force discharge must keep slot 1 end = 23:59"
            );
        })
        .await;
    }

    /// Issue #129: when the `minutes` path is used, the handler must
    /// record the slot's end time in the revert so the poll loop can
    /// auto-revert when the window expires. Without this, the inverter
    /// is left in export mode (HR 27=0) with enable_discharge=1 but no
    /// active slot, which effectively pauses the battery.
    #[tokio::test]
    async fn force_discharge_with_minutes_records_slot_end_for_auto_revert() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let before = chrono::Local::now().timestamp_millis();

            let _ = force_discharge(
                State(state.clone()),
                Some(Json(json!({ "minutes": 30 }))),
            )
            .await;
            let _ = drain_pending_writes(&state).await;

            let revert = state
                .force_discharge_revert
                .lock()
                .await
                .clone()
                .expect("revert should be set");
            let slot_end = revert
                .force_discharge_slot_end_ms
                .expect("slot end must be recorded on the minutes path");

            // Slot end must be in the future (30 min from "before").
            let expected_min = before + 30 * 60 * 1000;
            let expected_max = before + 30 * 60 * 1000 + 5_000; // 5s slack
            assert!(
                slot_end >= expected_min && slot_end <= expected_max,
                "slot end should be ~30 min from now: before={before}, slot_end={slot_end}, \
                 expected=[{expected_min}, {expected_max}]"
            );
        })
        .await;
    }

    /// The no-body path must leave `force_discharge_slot_end_ms` as None
    /// so the poll loop doesn't auto-revert — the user explicitly chose
    /// "until stopped" and must hit Stop to end it.
    #[tokio::test]
    async fn force_discharge_without_minutes_leaves_slot_end_unset() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let _ = force_discharge(State(state.clone()), None).await;
            let _ = drain_pending_writes(&state).await;

            let revert = state
                .force_discharge_revert
                .lock()
                .await
                .clone()
                .expect("revert should be set");
            assert!(
                revert.force_discharge_slot_end_ms.is_none(),
                "no-body path must not set slot end (no auto-revert)"
            );
        })
        .await;
    }

    /// Three-phase force discharge must also honour the duration, writing
    /// the 3PH slot registers (HR 1118/1119) to the duration window.
    #[tokio::test]
    async fn force_discharge_with_minutes_three_phase_uses_3ph_slot_registers() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_3PH_DISCHARGE_SLOT_1_END, HR_3PH_DISCHARGE_SLOT_1_START,
                HR_3PH_DISCHARGE_SLOT_2_END, HR_3PH_DISCHARGE_SLOT_2_START,
                HR_3PH_FORCE_DISCHARGE_ENABLE,
            };

            let state = make_state_with_device(DeviceType::ThreePhase).await;
            let _ = force_discharge(
                State(state.clone()),
                Some(Json(json!({ "minutes": 60 }))),
            )
            .await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);

            // Slot writes must use the 3PH register addresses, not the
            // classic single-phase HR 56/57.
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_DISCHARGE_SLOT_1_START),
                "3PH force discharge with minutes must write HR 1118"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_DISCHARGE_SLOT_1_END),
                "3PH force discharge with minutes must write HR 1119"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_DISCHARGE_SLOT_2_START && w.value == 0),
                "3PH force discharge with minutes must clear slot 2 (HR 1120)"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_DISCHARGE_SLOT_2_END && w.value == 0),
                "3PH force discharge with minutes must clear slot 2 (HR 1121)"
            );

            // Critically: the classic single-phase slot addresses must
            // NOT appear in the 3PH write list.
            use crate::modbus::registers::{
                HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_END,
                HR_DISCHARGE_SLOT_2_START,
            };
            assert!(
                !writes.iter().any(|w| w.address == HR_DISCHARGE_SLOT_1_START),
                "3PH force discharge must not write classic HR 56"
            );
            assert!(
                !writes.iter().any(|w| w.address == HR_DISCHARGE_SLOT_1_END),
                "3PH force discharge must not write classic HR 57"
            );
            assert!(
                !writes.iter().any(|w| w.address == HR_DISCHARGE_SLOT_2_START),
                "3PH force discharge must not write classic HR 44"
            );
            assert!(
                !writes.iter().any(|w| w.address == HR_DISCHARGE_SLOT_2_END),
                "3PH force discharge must not write classic HR 45"
            );

            // The 3PH force flag must still be armed.
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_FORCE_DISCHARGE_ENABLE && w.value == 1),
                "3PH force discharge with minutes must arm HR_3PH_FORCE_DISCHARGE_ENABLE=1"
            );
        })
        .await;
    }

    /// Sanity check: charge and discharge reverts are independent. A
    /// force-charge followed by a force-discharge must leave the
    /// force-charge revert in place (or whatever its lifecycle is), not
    /// clobber the discharge revert or vice versa.
    #[tokio::test]
    async fn force_charge_and_force_discharge_reverts_are_independent() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            seed_charging_pre_state(&state).await;

            // Start a force charge, then verify its revert is captured.
            let _ = force_charge(State(state.clone()), Some(Json(json!({ "minutes": 30 })))).await;
            let _ = drain_pending_writes(&state).await;
            assert!(state.force_charge_revert.lock().await.is_some());
            assert!(state.force_discharge_revert.lock().await.is_none());

            // Now start a force discharge. The discharge revert should be
            // populated independently. The charge revert should be untouched
            // (we don't auto-stop the charge; the user has to do that
            // explicitly).
            let _ = force_discharge(State(state.clone()), None).await;
            let _ = drain_pending_writes(&state).await;
            assert!(state.force_discharge_revert.lock().await.is_some());
            assert!(
                state.force_charge_revert.lock().await.is_some(),
                "force_discharge should not clobber the force_charge_revert"
            );

            // Stop the discharge; the charge revert should still be there.
            let (status, _) = force_discharge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);
            let _ = drain_pending_writes(&state).await;
            assert!(state.force_discharge_revert.lock().await.is_none());
            assert!(
                state.force_charge_revert.lock().await.is_some(),
                "discharge stop should not touch the charge revert"
            );
        })
        .await;
    }

    // -----------------------------------------------------------------------
    // Alerts config: Pushover channel (issue #101)
    // -----------------------------------------------------------------------

    /// POST /api/alerts must accept `pushover_app_token` and
    /// `pushover_user_key`, store them on the live `alert_config`, AND
    /// persist them to `settings.json` so they survive a restart.
    #[tokio::test]
    async fn set_alerts_persists_pushover_credentials() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "enabled": true,
                "pushover_app_token": "app-token-123",
                "pushover_user_key": "user-key-456",
            });
            let (status, _) = set_alerts(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            // Live in-memory state is updated immediately.
            let cfg = state.alert_config.lock().await.clone();
            assert_eq!(cfg.pushover_app_token, "app-token-123");
            assert_eq!(cfg.pushover_user_key, "user-key-456");
            assert!(cfg.enabled);

            // Reload from disk — the persistence write must have landed.
            let reloaded = crate::settings::Settings::load();
            assert_eq!(reloaded.alerts_config.pushover_app_token, "app-token-123");
            assert_eq!(reloaded.alerts_config.pushover_user_key, "user-key-456");
        })
        .await;
    }

    /// Omitting the Pushover fields from the POST body must leave them
    /// untouched (the API does partial updates). This guards against a
    /// future regression that would wipe credentials whenever the frontend
    /// sends an unrelated field.
    #[tokio::test]
    async fn set_alerts_pushover_fields_are_optional_partial_update() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            // Seed existing Pushover credentials.
            {
                let mut cfg = state.alert_config.lock().await;
                cfg.pushover_app_token = "pre-existing-token".to_string();
                cfg.pushover_user_key = "pre-existing-user".to_string();
            }

            // POST an unrelated field with no Pushover keys in the body.
            let body = serde_json::json!({ "cooldown_minutes": 15 });
            let (status, _) = set_alerts(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let cfg = state.alert_config.lock().await.clone();
            assert_eq!(
                cfg.pushover_app_token, "pre-existing-token",
                "partial update must not wipe the app token"
            );
            assert_eq!(
                cfg.pushover_user_key, "pre-existing-user",
                "partial update must not wipe the user key"
            );
            assert_eq!(cfg.cooldown_minutes, 15);
        })
        .await;
    }

    /// `test_alerts` must short-circuit with a 400 BEFORE any HTTP call when
    /// no channel (Telegram, ntfy, or Pushover) is configured. This is the
    /// network-free path of the gate, so it's safe to assert without mocking.
    #[tokio::test]
    async fn test_alerts_rejects_when_no_channel_configured() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let (status, res) = test_alerts(State(state.clone())).await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "no channel configured should be a 400, not a network call"
            );
            let msg = res.get("message").and_then(|v| v.as_str()).unwrap_or("");
            assert!(
                msg.contains("Pushover"),
                "gate message should mention Pushover, got: {msg}"
            );
        })
        .await;
    }

    // -------------------------------------------------------------------
    // Tariff config validation in update_settings — server is the last
    // line of defence against malformed/overlapping/incomplete configs
    // sneaking in via hand-edited settings.json or direct API calls.
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn update_settings_accepts_valid_tariff_config() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "import_tariff_config": {
                    "slots": [
                        { "start": "00:00", "end": "05:30", "rate": 0.10 },
                        { "start": "05:30", "end": "23:59", "rate": 0.30 },
                    ],
                },
            });
            let (status, json) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(json.get("ok").and_then(|v| v.as_bool()), Some(true));
            // The tariff config is persisted to disk by update_settings;
            // verify by reloading from the isolated config dir.
            let saved = crate::settings::Settings::load();
            assert_eq!(saved.import_tariff_config.as_ref().unwrap().slots.len(), 2);
        })
        .await;
    }

    #[tokio::test]
    async fn update_settings_rejects_overlapping_tariff_config() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "import_tariff_config": {
                    "slots": [
                        { "start": "00:00", "end": "06:00", "rate": 0.20 },
                        { "start": "05:00", "end": "23:59", "rate": 0.30 },
                    ],
                },
            });
            let (status, json) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let err = json.get("error").and_then(|v| v.as_str()).unwrap_or("");
            assert!(err.contains("overlap"), "got: {err}");
        })
        .await;
    }

    #[tokio::test]
    async fn update_settings_rejects_gap_in_tariff_config() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "import_tariff_config": {
                    "slots": [
                        { "start": "00:00", "end": "05:00", "rate": 0.20 },
                        { "start": "06:00", "end": "23:59", "rate": 0.30 },
                    ],
                },
            });
            let (status, json) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let err = json.get("error").and_then(|v| v.as_str()).unwrap_or("");
            assert!(err.contains("gap"), "got: {err}");
        })
        .await;
    }

    #[tokio::test]
    async fn update_settings_rejects_last_slot_not_at_23_59() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "import_tariff_config": {
                    "slots": [
                        { "start": "00:00", "end": "20:00", "rate": 0.20 },
                    ],
                },
            });
            let (status, json) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let err = json.get("error").and_then(|v| v.as_str()).unwrap_or("");
            assert!(err.contains("23:59"), "got: {err}");
        })
        .await;
    }

    #[tokio::test]
    async fn update_settings_rejects_legacy_24_00_end() {
        // "24:00" used to be the legacy end-of-day marker. With the new
        // model (final slot ends at "23:59" inclusive), "24:00" must be
        // rejected rather than silently broken.
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "import_tariff_config": {
                    "slots": [
                        { "start": "00:00", "end": "24:00", "rate": 0.20 },
                    ],
                },
            });
            let (status, _) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        })
        .await;
    }

    #[tokio::test]
    async fn update_settings_rejects_negative_rate() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "export_tariff_config": {
                    "slots": [
                        { "start": "00:00", "end": "23:59", "rate": -0.10 },
                    ],
                },
            });
            let (status, json) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let err = json.get("error").and_then(|v| v.as_str()).unwrap_or("");
            assert!(err.contains("negative"), "got: {err}");
        })
        .await;
    }

    #[tokio::test]
    async fn update_settings_keeps_existing_config_when_field_omitted() {
        // When the request doesn't mention tariff config, the on-disk value
        // must be preserved (not overwritten with defaults).
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            // Seed a known config on disk via a first call.
            let seed = serde_json::json!({
                "import_tariff_config": {
                    "slots": [{ "start": "00:00", "end": "23:59", "rate": 0.42 }],
                },
            });
            let _ = update_settings(State(state.clone()), Json(seed)).await;
            // Now send an unrelated update with no tariff field.
            let body = serde_json::json!({ "interval_secs": 30 });
            let (status, _) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let saved = crate::settings::Settings::load();
            assert_eq!(
                saved.import_tariff_config.as_ref().unwrap().slots[0].rate,
                0.42,
                "omitted tariff_config must not overwrite"
            );
        })
        .await;
    }

    // -----------------------------------------------------------------------
    // Support bundle submission (#125).
    //
    // Only the validation and rate-limit paths are exercised here because they
    // return *before* the ntfy delivery call — the success path hits the live
    // public ntfy.sh server and is covered by the Playwright E2E test.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn support_submit_rejects_empty_description() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "description": "   ",
                "category": "other",
            });
            let (status, res) = submit_support_bundle(State(state), Json(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(!res["ok"].as_bool().unwrap_or(true));
        })
        .await;
    }

    #[tokio::test]
    async fn support_submit_rejects_invalid_category() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "description": "something is broken",
                "category": "bogus",
            });
            let (status, res) = submit_support_bundle(State(state), Json(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let err = res["error"].as_str().unwrap_or("");
            assert!(err.contains("Invalid category"), "got {err}");
        })
        .await;
    }

    #[tokio::test]
    async fn support_submit_rejects_invalid_issue_number() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "description": "something is broken",
                "category": "other",
                "issue_number": "not-a-number",
            });
            let (status, res) = submit_support_bundle(State(state), Json(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let err = res["error"].as_str().unwrap_or("");
            assert!(err.contains("Issue number"), "got {err}");
        })
        .await;
    }

    #[tokio::test]
    async fn support_submit_rejects_malformed_body() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            // Missing required `description` field → deserialise error.
            let body = serde_json::json!({ "category": "other" });
            let (status, _) = submit_support_bundle(State(state), Json(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        })
        .await;
    }

    /// A submission made within the cooldown window must be rejected *before*
    /// any network delivery. Pre-seeding `last_support_submit_ms` simulates a
    /// successful prior submission without having to hit ntfy.sh.
    #[tokio::test]
    async fn support_submit_rate_limits_within_cooldown() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            state.last_support_submit_ms.store(
                chrono::Local::now().timestamp_millis(),
                std::sync::atomic::Ordering::Relaxed,
            );
            let body = serde_json::json!({
                "description": "follow-up issue",
                "category": "other",
            });
            let (status, res) = submit_support_bundle(State(state), Json(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let err = res["error"].as_str().unwrap_or("");
            assert!(err.contains("wait"), "expected cooldown message, got {err}");
        })
        .await;
    }

    // ======================================================================
    // get_settings + update_settings — read-only API key/port fields
    // ======================================================================

    #[tokio::test]
    async fn get_settings_returns_api_key_and_port() {
        with_isolated_config_dir_async(|| async {
            // Seed a known api_key/api_port on disk.
            let mut s = crate::settings::Settings::load();
            s.api_key = "test-key-456".to_string();
            s.api_port = 9999;
            s.save().expect("settings save");

            let state = Arc::new(AppState::new());
            let (status, body) = get_settings(State(state)).await;

            assert_eq!(status, StatusCode::OK);
            let data = &body["data"];
            assert_eq!(data["api_key"], "test-key-456");
            assert_eq!(data["api_port"], 9999);
        })
        .await;
    }

    #[tokio::test]
    async fn get_settings_returns_empty_key_by_default() {
        with_isolated_config_dir_async(|| async {
            // No api_key/api_port saved — should get defaults.
            let state = Arc::new(AppState::new());
            let (status, body) = get_settings(State(state)).await;

            assert_eq!(status, StatusCode::OK);
            let data = &body["data"];
            assert_eq!(data["api_key"], "");
            assert_eq!(data["api_port"], 7338);
        })
        .await;
    }

    #[tokio::test]
    async fn update_settings_persists_api_key_and_port() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());

            let body = serde_json::json!({
                "api_key": "my-api-key",
                "api_port": 8443,
            });
            let (status, _) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            // Verify on disk from scratch.
            let saved = crate::settings::Settings::load();
            assert_eq!(saved.api_key, "my-api-key");
            assert_eq!(saved.api_port, 8443);

            // Verify get_settings returns them too.
            let (_, get_body) = get_settings(State(state)).await;
            assert_eq!(get_body["data"]["api_key"], "my-api-key");
            assert_eq!(get_body["data"]["api_port"], 8443);
        })
        .await;
    }

    #[tokio::test]
    async fn update_settings_persists_api_key_only_preserves_port() {
        with_isolated_config_dir_async(|| async {
            // Seed a known port on disk first.
            let mut s = crate::settings::Settings::load();
            s.api_key = "old-key".to_string();
            s.api_port = 7338;
            s.save().expect("settings save");

            let state = Arc::new(AppState::new());

            // Now send just a new api_key, no port.
            let body = serde_json::json!({
                "api_key": "new-key",
            });
            let (status, _) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let saved = crate::settings::Settings::load();
            assert_eq!(saved.api_key, "new-key", "api_key should be updated");
            assert_eq!(
                saved.api_port, 7338,
                "api_port should stay at its previous value when not in the request"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn update_settings_persists_port_only_preserves_key() {
        with_isolated_config_dir_async(|| async {
            // Seed a known key on disk first.
            let mut s = crate::settings::Settings::load();
            s.api_key = "existing-key".to_string();
            s.api_port = 7338;
            s.save().expect("settings save");

            let state = Arc::new(AppState::new());

            // Send just a new port.
            let body = serde_json::json!({
                "api_port": 9090,
            });
            let (status, _) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let saved = crate::settings::Settings::load();
            assert_eq!(
                saved.api_key, "existing-key",
                "api_key should stay at its previous value when not in the request"
            );
            assert_eq!(saved.api_port, 9090, "api_port should be updated");
        })
        .await;
    }

    #[tokio::test]
    async fn update_settings_clears_api_key_to_empty_string() {
        with_isolated_config_dir_async(|| async {
            // Seed a key on disk first.
            let mut s = crate::settings::Settings::load();
            s.api_key = "remove-me".to_string();
            s.api_port = 7338;
            s.save().expect("settings save");

            let state = Arc::new(AppState::new());

            // Send empty string to clear the key.
            let body = serde_json::json!({
                "api_key": "",
            });
            let (status, _) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let saved = crate::settings::Settings::load();
            assert_eq!(saved.api_key, "", "api_key should be cleared to empty");
            assert_eq!(
                saved.api_port, 7338,
                "api_port should not be affected when clearing api_key"
            );
        })
        .await;
    }

    // -----------------------------------------------------------------------
    // Log-message format for POST /api/settings
    //
    // The previous hard-coded log always printed host/port/serial/interval_secs
    // with empty defaults when those fields weren't in the request body, so
    // every non-connection save (tariffs, read-only API key, panel visibility,
    // etc.) produced the misleading line
    //     Settings updated: host=, port=0, serial=, interval=0s
    // regardless of what the user actually changed. These tests pin the
    // field-aware format and the api_key redaction, and verify the values
    // match what was actually written to settings.json on disk.
    // -----------------------------------------------------------------------

    /// Extract the `message` field from the JSON response. The handler
    /// reuses the same string for both the response body and the log line,
    /// so this also reflects the log content for non-tracing assertions.
    fn response_message(json: &serde_json::Value) -> String {
        json.get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }

    #[test]
    fn settings_log_fields_lists_only_fields_present_in_body() {
        use crate::settings::Settings;

        // A body that only carries the read-only API key and port (the
        // exact scenario that triggered the bug report). The log should
        // mention only those two fields, NOT the four connection fields
        // as empty defaults.
        let body = serde_json::json!({
            "api_key": "secret-token-1234",
            "api_port": 7338,
        });
        let persist = Settings {
            api_key: "secret-token-1234".to_string(),
            api_port: 7338,
            ..Default::default()
        };

        let fields = settings_log_fields(&body, &persist);
        let joined = fields.join(", ");

        assert_eq!(fields.len(), 2, "only the two fields in the body should appear, got: {joined}");
        assert!(joined.contains("api_key="), "expected api_key entry, got: {joined}");
        assert!(joined.contains("api_port=7338"), "expected api_port entry, got: {joined}");
        // The bug was: these connection fields appeared as empty/0 in the
        // log even when the body didn't carry them. They must NOT appear.
        // Use precise matches to avoid false positives from `api_port=`
        // (which contains the substring `port=`).
        assert!(!joined.contains("host="), "host must not appear when absent from body, got: {joined}");
        assert!(
            !joined.split(',').any(|s| s.trim_start().starts_with("port=")),
            "port must not appear when absent from body, got: {joined}"
        );
        assert!(!joined.contains("serial="), "serial must not appear when absent from body, got: {joined}");
        assert!(!joined.contains("interval="), "interval must not appear when absent from body, got: {joined}");
    }

    #[test]
    fn settings_log_fields_redacts_api_key_plaintext() {
        use crate::settings::Settings;

        let body = serde_json::json!({ "api_key": "supersecretvalue" });
        let persist = Settings {
            api_key: "supersecretvalue".to_string(),
            ..Default::default()
        };

        let fields = settings_log_fields(&body, &persist);
        let joined = fields.join(", ");

        assert!(
            !joined.contains("supersecretvalue"),
            "api_key plaintext must NEVER appear in the log (security: log files can be exfiltrated via the support bundle), got: {joined}"
        );
        assert!(joined.contains("api_key=set"), "expected redacted set marker, got: {joined}");
        assert!(joined.contains("16 chars"), "expected length hint so the user can verify the key, got: {joined}");
    }

    #[test]
    fn settings_log_fields_reports_api_key_cleared_when_empty() {
        use crate::settings::Settings;

        let body = serde_json::json!({ "api_key": "" });
        let persist = Settings {
            api_key: String::new(),
            ..Default::default()
        };

        let fields = settings_log_fields(&body, &persist);
        let joined = fields.join(", ");

        assert!(joined.contains("api_key=cleared"), "empty api_key should be reported as cleared, got: {joined}");
    }

    #[test]
    fn settings_log_fields_uses_persisted_values_not_incoming() {
        use crate::settings::Settings;

        // Simulate: disk already has interval=30, body sends only host
        // and port. The log must show the *persisted* interval (30s, from
        // disk), not 0 from a missing body field. This is the exact
        // scenario the user reported — "interval was saved as 30s but
        // the log said interval=0s".
        let body = serde_json::json!({ "host": "10.0.0.50", "port": 8899 });
        let persist = Settings {
            host: "10.0.0.50".to_string(),
            port: 8899,
            poll_interval: 30, // already on disk from a previous save
            ..Default::default()
        };

        let fields = settings_log_fields(&body, &persist);
        let joined = fields.join(", ");

        // host and port are present, so they should appear.
        assert!(joined.contains("host=10.0.0.50"), "got: {joined}");
        assert!(joined.contains("port=8899"), "got: {joined}");
        // interval_secs was NOT in the body, so it must not be logged
        // (otherwise we'd be back to the misleading default-zeros case).
        assert!(
            !joined.contains("interval="),
            "interval must not be reported when absent from the body, got: {joined}"
        );
    }

    #[test]
    fn settings_log_fields_summarises_complex_fields() {
        use crate::settings::{Settings, TariffConfig, TariffSlot};

        let body = serde_json::json!({
            "import_tariff_config": { "slots": [] },
            "export_tariff_config": { "slots": [] },
            "hidden_panels": ["a", "b", "c"],
        });
        let persist = Settings {
            import_tariff_config: Some(TariffConfig {
                slots: vec![
                    TariffSlot { start: "00:00".into(), end: "07:00".into(), rate: 0.10 },
                    TariffSlot { start: "07:00".into(), end: "23:59".into(), rate: 0.30 },
                ],
            }),
            export_tariff_config: Some(TariffConfig {
                slots: (0..24).map(|i| TariffSlot {
                    start: format!("{i:02}:00"),
                    end: format!("{:02}:00", (i + 1) % 24),
                    rate: 0.20,
                }).collect(),
            }),
            hidden_panels: vec!["x".into(), "y".into(), "z".into()],
            ..Default::default()
        };

        let fields = settings_log_fields(&body, &persist);
        let joined = fields.join(", ");

        assert!(joined.contains("import_tariff_config=2 slots"), "got: {joined}");
        assert!(joined.contains("export_tariff_config=24 slots"), "got: {joined}");
        assert!(joined.contains("hidden_panels=3 entries"), "got: {joined}");
        // Don't dump the full JSON.
        assert!(!joined.contains("rate"), "tariff slot details must not be logged, got: {joined}");
    }

    #[test]
    fn settings_log_fields_empty_body_returns_empty() {
        use crate::settings::Settings;

        let body = serde_json::json!({});
        let persist = Settings::default();

        let fields = settings_log_fields(&body, &persist);
        assert!(
            fields.is_empty(),
            "empty body should produce no log fields, got: {fields:?}"
        );
    }

    // -- End-to-end handler tests: log format + on-disk persistence --

    /// Saving the read-only API key + port must (a) log the new values
    /// (with the key redacted) and (b) persist them to settings.json.
    /// This is the exact scenario from the bug report.
    #[tokio::test]
    async fn update_settings_api_key_save_logs_and_persists_both_fields() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "api_key": "my-secret-token-12345",
                "api_port": 8443,
            });

            let (status, json) =
                update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let msg = response_message(&json);

            // (a) The response/log message must show both fields, with
            // the key redacted.
            assert!(
                msg.contains("api_key=set"),
                "api_key must appear as redacted 'set', got: {msg}"
            );
            assert!(
                msg.contains("21 chars"),
                "api_key length must appear so the user can verify the key, got: {msg}"
            );
            assert!(
                !msg.contains("my-secret-token-12345"),
                "api_key plaintext must NEVER appear in the log, got: {msg}"
            );
            assert!(
                msg.contains("api_port=8443"),
                "api_port must appear in the log, got: {msg}"
            );
            // (b) The connection fields must NOT appear in the log even
            // though the old format always printed them as 0/empty. Use
            // precise token-prefix matches so we don't false-positive on
            // `api_port=`, `http_port=`, `evc_port=` etc.
            assert!(
                !msg.split(',').any(|s| s.trim_start().starts_with("host=")),
                "host must not appear when absent, got: {msg}"
            );
            assert!(
                !msg.split(',').any(|s| s.trim_start().starts_with("port=")),
                "port must not appear when absent, got: {msg}"
            );
            assert!(
                !msg.split(',').any(|s| s.trim_start().starts_with("serial=")),
                "serial must not appear when absent, got: {msg}"
            );
            assert!(
                !msg.split(',').any(|s| s.trim_start().starts_with("interval=")),
                "interval must not appear when absent, got: {msg}"
            );

            // (c) The values must be on disk for the next launch.
            let saved = crate::settings::Settings::load();
            assert_eq!(saved.api_key, "my-secret-token-12345", "api_key must be persisted");
            assert_eq!(saved.api_port, 8443, "api_port must be persisted");
        })
        .await;
    }

    /// Clearing the read-only API key (sending "") must log it as
    /// "cleared" (never the empty string itself, never a stray value)
    /// and persist the cleared state.
    #[tokio::test]
    async fn update_settings_clear_api_key_logs_cleared_and_persists_empty() {
        with_isolated_config_dir_async(|| async {
            // Seed an existing key.
            let mut seed = crate::settings::Settings::load();
            seed.api_key = "old-key".to_string();
            seed.api_port = 7338;
            seed.save().expect("seed save");

            let state = Arc::new(AppState::new());
            let body = serde_json::json!({ "api_key": "" });

            let (status, json) =
                update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let msg = response_message(&json);

            assert!(
                msg.contains("api_key=cleared"),
                "empty key must be reported as cleared, got: {msg}"
            );
            // Persisted state is empty, port preserved.
            let saved = crate::settings::Settings::load();
            assert_eq!(saved.api_key, "", "api_key must be cleared on disk");
            assert_eq!(saved.api_port, 7338, "api_port must be preserved when only key was sent");
        })
        .await;
    }

    /// Saving a tariff config must log the slot count (not the raw JSON)
    /// and persist the parsed config to disk.
    #[tokio::test]
    async fn update_settings_tariff_save_logs_slot_count_and_persists_config() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "import_tariff_config": {
                    "slots": [
                        { "start": "00:00", "end": "07:00", "rate": 0.10 },
                        { "start": "07:00", "end": "23:59", "rate": 0.30 },
                    ],
                },
            });

            let (status, json) =
                update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let msg = response_message(&json);

            assert!(
                msg.contains("import_tariff_config=2 slots"),
                "tariff config must be summarised by slot count, got: {msg}"
            );
            // Connection fields must not pollute a tariff-save log.
            assert!(
                !msg.split(',').any(|s| s.trim_start().starts_with("host=")),
                "got: {msg}"
            );
            assert!(
                !msg.split(',').any(|s| s.trim_start().starts_with("port=")),
                "got: {msg}"
            );
            assert!(
                !msg.split(',').any(|s| s.trim_start().starts_with("interval=")),
                "got: {msg}"
            );

            // Persisted: the parsed config is on disk and intact.
            let saved = crate::settings::Settings::load();
            let cfg = saved
                .import_tariff_config
                .as_ref()
                .expect("import_tariff_config must be persisted");
            assert_eq!(cfg.slots.len(), 2);
            assert_eq!(cfg.slots[0].start, "00:00");
            assert_eq!(cfg.slots[0].end, "07:00");
            assert!((cfg.slots[0].rate - 0.10).abs() < 1e-9);
            assert_eq!(cfg.slots[1].start, "07:00");
            assert_eq!(cfg.slots[1].end, "23:59");
            assert!((cfg.slots[1].rate - 0.30).abs() < 1e-9);
        })
        .await;
    }

    /// Saving a connection (host/port/serial) must log exactly those
    /// three fields, the interval that was already on disk must NOT
    /// appear in the log (the user's complaint was the opposite), and
    /// the persisted interval on disk must be unchanged.
    #[tokio::test]
    async fn update_settings_connection_save_logs_only_three_fields_and_preserves_disk_interval() {
        with_isolated_config_dir_async(|| async {
            // Seed the disk with a known interval (30s) BEFORE the save.
            let mut seed = crate::settings::Settings::load();
            seed.poll_interval = 30;
            seed.host = "old-host".to_string();
            seed.save().expect("seed save");

            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "host": "10.0.0.99",
                "port": 8899,
                "serial": "SN-NEW",
            });

            let (status, json) =
                update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let msg = response_message(&json);

            // (a) The log must show exactly the three connection fields
            // that were in the body.
            assert!(msg.contains("host=10.0.0.99"), "got: {msg}");
            assert!(msg.contains("port=8899"), "got: {msg}");
            assert!(msg.contains("serial=SN-NEW"), "got: {msg}");
            // (b) The interval was NOT in the body, so it must not appear.
            // Previously the log would have said "interval=0s" here,
            // which was the misleading behaviour.
            assert!(!msg.contains("interval="), "interval must not appear when absent from body, got: {msg}");
            // (c) And the disk value must still be 30s — the connection
            // save did not clobber it.
            let saved = crate::settings::Settings::load();
            assert_eq!(saved.poll_interval, 30, "interval on disk must be unchanged");
            assert_eq!(saved.host, "10.0.0.99", "new host must be persisted");
            assert_eq!(saved.port, 8899, "new port must be persisted");
            assert_eq!(saved.serial, "SN-NEW", "new serial must be persisted");
        })
        .await;
    }

    /// Saving the poll interval alone must log exactly the new interval
    /// and persist it. Previously the log would also show
    /// `host=, port=0, serial=` which was misleading.
    #[tokio::test]
    async fn update_settings_interval_save_logs_only_interval_and_persists_new_value() {
        with_isolated_config_dir_async(|| async {
            // Seed a non-default interval on disk first.
            let mut seed = crate::settings::Settings::load();
            seed.poll_interval = 60;
            seed.save().expect("seed save");

            let state = Arc::new(AppState::new());
            let body = serde_json::json!({ "interval_secs": 30 });

            let (status, json) =
                update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let msg = response_message(&json);

            assert!(msg.contains("interval=30s"), "new interval must appear, got: {msg}");
            // The four connection fields are not in the body, so none
            // should appear — previously all four would show as empty/0.
            assert!(!msg.contains("host="), "got: {msg}");
            assert!(!msg.contains("port="), "got: {msg}");
            assert!(!msg.contains("serial="), "got: {msg}");

            let saved = crate::settings::Settings::load();
            assert_eq!(saved.poll_interval, 30, "interval must be persisted to disk");
        })
        .await;
    }

    /// Multi-field save (e.g. EV charger host + port) must log exactly
    /// the fields in the body and persist both to disk.
    #[tokio::test]
    async fn update_settings_evc_save_logs_both_fields_and_persists_both() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "evc_host": "ev-charger.local",
                "evc_port": 5020,
            });

            let (status, json) =
                update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let msg = response_message(&json);

            assert!(msg.contains("evc_host=ev-charger.local"), "got: {msg}");
            assert!(msg.contains("evc_port=5020"), "got: {msg}");
            // The bare `host=`/`port=` connection fields must not appear;
            // `evc_host=` / `evc_port=` are fine. Use precise token-prefix
            // matching to avoid the substring overlap.
            assert!(
                !msg.split(',').any(|s| {
                    let t = s.trim_start();
                    t.starts_with("host=") || t.starts_with("port=")
                }),
                "bare host/port (no evc_ prefix) must not appear, got: {msg}"
            );

            let saved = crate::settings::Settings::load();
            assert_eq!(saved.evc_host, "ev-charger.local");
            assert_eq!(saved.evc_port, 5020);
        })
        .await;
    }

    /// Hidden-panels save must log the entry count and persist the list.
    /// (Logging the full panel list would spam the log every save.)
    #[tokio::test]
    async fn update_settings_hidden_panels_save_logs_count_and_persists_list() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "hidden_panels": ["battery", "history", "inverter"],
            });

            let (status, json) =
                update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let msg = response_message(&json);

            assert!(msg.contains("hidden_panels=3 entries"), "got: {msg}");
            // Don't dump the full list into the log.
            assert!(!msg.contains("battery"), "panel names must not be logged, got: {msg}");

            let saved = crate::settings::Settings::load();
            assert_eq!(saved.hidden_panels, vec!["battery", "history", "inverter"]);
        })
        .await;
    }

    /// Toggles (autostart, auto-discovery, minimal telemetry) must log
    /// the new value (true/false) and persist it. Previously a toggle
    /// save would produce `host=, port=0, serial=, interval=0s` which
    /// made it look like the connection was wiped.
    #[tokio::test]
    async fn update_settings_toggles_log_new_value_and_persist() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            for (field, value, expected_log, expected_disk) in [
                ("autostart_enabled", json!(true), "autostart_enabled=true", true),
                ("autostart_enabled", json!(false), "autostart_enabled=false", false),
                ("disable_auto_discovery", json!(true), "disable_auto_discovery=true", true),
                ("disable_auto_discovery", json!(false), "disable_auto_discovery=false", false),
                ("minimal_telemetry_mode", json!(true), "minimal_telemetry_mode=true", true),
            ] {
                let body = serde_json::json!({ field: value });
                let (status, json) = update_settings(State(state.clone()), Json(body)).await;
                assert_eq!(status, StatusCode::OK);
                let msg = response_message(&json);
                assert!(
                    msg.contains(expected_log),
                    "expected `{expected_log}` in log for field {field}, got: {msg}"
                );
                // The misleading connection defaults must not appear.
                assert!(!msg.contains("host="), "got: {msg}");
                assert!(!msg.contains("interval="), "got: {msg}");
                // Disk value must match.
                let saved = crate::settings::Settings::load();
                let actual = match field {
                    "autostart_enabled" => saved.autostart_enabled,
                    "disable_auto_discovery" => saved.disable_auto_discovery,
                    "minimal_telemetry_mode" => saved.minimal_telemetry_mode,
                    _ => unreachable!(),
                };
                assert_eq!(actual, expected_disk, "{field} must be persisted");
            }
        })
        .await;
    }

    /// Save the HTTP port — must appear in the log and be persisted.
    /// Previously this would also produce a misleading
    /// `host=, port=0, serial=, interval=0s` line.
    #[tokio::test]
    async fn update_settings_http_port_save_logs_and_persists() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({ "http_port": 8080 });
            let (status, json) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let msg = response_message(&json);
            assert!(msg.contains("http_port=8080"), "got: {msg}");
            // Use precise token-prefix matching to avoid the `port=` substring
            // matching `http_port=`.
            assert!(
                !msg.split(',').any(|s| {
                    let t = s.trim_start();
                    t.starts_with("host=") || t.starts_with("port=")
                }),
                "bare host/port (no http_ prefix) must not appear, got: {msg}"
            );

            let saved = crate::settings::Settings::load();
            assert_eq!(saved.http_port, 8080, "http_port must be persisted");
        })
        .await;
    }

    /// Empty body — log should explicitly say "no fields in request body"
    /// and the disk save should still run (auto_connect is set
    /// unconditionally — covered here as a known side effect, not a
    /// behavioural change).
    #[tokio::test]
    async fn update_settings_empty_body_log_says_no_fields_and_disk_save_runs() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({});
            let (status, json) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let msg = response_message(&json);
            assert!(
                msg.contains("no fields"),
                "empty body should produce a 'no fields' marker, got: {msg}"
            );
        })
        .await;
    }

    /// GET /api/evc/status — issue #138. The frontend uses this to seed
    /// `evcEverConnected` on page load when the WS broadcast channel has
    /// already missed the initial `EvcConnected` / `Evc` frame.
    mod evc_status_endpoint {
        use super::*;

        #[tokio::test]
        async fn returns_reachable_false_when_no_snapshot_cached() {
            let state = Arc::new(AppState::new());
            // Fresh process — latest_evc is None, host is empty by default.
            let (status, json) = evc_status(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(json["ok"], serde_json::Value::Bool(true));
            assert_eq!(
                json["reachable"],
                serde_json::Value::Bool(false),
                "no cached snapshot -> reachable=false"
            );
            assert!(
                json["snapshot"].is_null(),
                "no cached snapshot -> snapshot=null, got: {}",
                json["snapshot"]
            );
            assert_eq!(json["evc_host"], serde_json::Value::String(String::new()));
            assert_eq!(json["evc_port"], serde_json::json!(502));
        }

        #[tokio::test]
        async fn returns_configured_host_even_when_unreachable() {
            let state = Arc::new(AppState::new());
            state.settings.lock().await.evc_host = "192.168.1.225".to_string();
            state.settings.lock().await.evc_port = 502;
            let (_status, json) = evc_status(State(state.clone())).await;
            assert_eq!(
                json["evc_host"],
                serde_json::Value::String("192.168.1.225".into())
            );
            assert_eq!(json["evc_port"], serde_json::json!(502));
            assert_eq!(
                json["reachable"],
                serde_json::Value::Bool(false),
                "host is set but never reached -> reachable=false"
            );
            assert!(json["snapshot"].is_null());
        }

        #[tokio::test]
        async fn returns_reachable_true_with_snapshot_when_latest_evc_is_some() {
            // Simulate the EVC poll loop having decoded at least one
            // snapshot since startup.
            let state = Arc::new(AppState::new());
            state.settings.lock().await.evc_host = "192.168.1.225".to_string();
            {
                let mut evc = state.latest_evc.lock().await;
                *evc = Some(crate::evc::EvcSnapshot {
                    charging_state: "Idle".into(),
                    connection_status: "Connected".into(),
                    active_power: 0,
                    current_l1: 0.0,
                    current_l2: 0.0,
                    current_l3: 0.0,
                    voltage_l1: 230.0,
                    voltage_l2: 230.0,
                    voltage_l3: 230.0,
                    meter_energy_kwh: 1234.5,
                    session_energy_kwh: 0.0,
                    session_duration_secs: 0,
                    charge_limit_a: 32.0,
                    serial_number: "GE-EVC-0001".into(),
                });
            }
            let (_status, json) = evc_status(State(state.clone())).await;
            assert_eq!(
                json["reachable"],
                serde_json::Value::Bool(true),
                "cached snapshot -> reachable=true"
            );
            let snap = &json["snapshot"];
            assert!(snap.is_object(), "snapshot should be an object");
            assert_eq!(
                snap["connection_status"],
                serde_json::Value::String("Connected".into())
            );
            assert_eq!(
                snap["serial_number"],
                serde_json::Value::String("GE-EVC-0001".into())
            );
            assert_eq!(snap["meter_energy_kwh"], serde_json::json!(1234.5));
        }
    }
}
