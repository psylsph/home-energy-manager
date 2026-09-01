//! REST API routes and handlers.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use chrono::{Datelike, Duration as ChronoDuration, Local, TimeZone, Timelike};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::inverter::encoder::{ControlCommand, RegisterWrite, WriteOutcome};
use crate::inverter::model::{DeviceType, InverterSnapshot};
use crate::inverter::poll::{
    stamp_solar_array_fields, AppState, ConnectionState, ForceChargeRevert, ForceDischargeRevert,
    PendingWriteBatch, PollMessage, PollSettings, WriteBatchPolicy,
};
use crate::inverter::state_machines::DischargeControlOwner;
use crate::modbus::registers::encode_hhmm;
use crate::settings::TariffConfig;

// ---------------------------------------------------------------------------
// Helper: standard JSON response
// ---------------------------------------------------------------------------

fn ok_response(message: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({ "ok": true, "message": message })),
    )
}

/// Like [`ok_response`] but includes `discharge_slots_backup` so the
/// frontend can stage the just-captured schedule as pending edits and
/// show the saved slots in the Eco-mode UI after an Eco→Timed→Eco
/// round-trip. The field is omitted when `backup` is `None` (nothing
/// was captured — see `capture_discharge_schedule_backup`). See #137.
fn ok_response_with_backup(
    message: &str,
    backup: Option<&[crate::settings::DischargeSlotBackup]>,
) -> (StatusCode, Json<Value>) {
    let mut body = json!({ "ok": true, "message": message });
    if let Some(b) = backup {
        body["discharge_slots_backup"] = json!(b);
    }
    (StatusCode::OK, Json(body))
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

/// ARM firmware version (HR 21) from the latest snapshot, parsed to a u16.
///
/// Used where a capability decision depends on firmware — notably
/// `DeviceType::supports_timed_discharge`, which enables the Gen3 Hybrid
/// pause-register probe only at ARM fw >= 312. Returns 0 when no snapshot is
/// available or the firmware string is empty/unparseable, which safely
/// evaluates as "below threshold" so the feature stays hidden until a real
/// reading arrives.
async fn latest_arm_fw(state: &Arc<AppState>) -> u16 {
    state
        .latest_snapshot
        .lock()
        .await
        .as_ref()
        .and_then(|s| s.firmware_version.parse::<u16>().ok())
        .unwrap_or(0)
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
    // When disabled, clear the slot times (write 0/0). The master
    // enable_discharge flag is managed by the caller: an enabled save arms
    // Timed Export right after the slot writes, and disabling the last armed
    // slot returns the inverter to Eco (both in `set_discharge_slot`).
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
/// Produce whitelist-validated register writes that clear **every**
/// discharge slot the device supports by setting them to 00:00–00:00
/// (disabled) — classic slots 1–2 (HR 44-45/56-57, or HR 1118-1121 on
/// three-phase) plus the extended slots 3–10 bank (HR 276-298) on
/// Gen3/AIO/HV/three-phase families.
///
/// Clearing all of them matters: Gen3-family firmware re-asserts
/// `enable_discharge` whenever ANY discharge slot register is non-zero
/// (issue #248), so leaving extended slots armed keeps bouncing the
/// inverter back out of Eco (issue #289).
///
/// Routes through the encoder's `SetDischargeSlot*` commands so every target
/// address is checked against `SAFE_WRITE_REGS`. Use this
/// instead of constructing raw `RegisterWrite` structs, which would bypass
/// the encoder's whitelist validation (the security invariant that *all*
/// register writes must be validated by the encoder).
fn clear_discharge_slot_writes(device_type: DeviceType) -> Vec<RegisterWrite> {
    let mut out = Vec::new();
    for slot in 1..=device_type.max_discharge_slots() {
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

/// Build the writes needed to restore a backed-up discharge schedule to the
/// inverter. Mirrors the body-path logic in `set_mode` (`is_timed` branch):
/// slot writes go FIRST so the inverter never sees `enable_discharge=1`
/// without slot constraints. Returns `None` if the backup is empty / absent
/// so callers can skip the restore step cleanly.
///
/// Each slot's `start_hour:start_minute` and `end_hour:end_minute` are
/// packed to the `HHMM` wire format expected by the encoder. Per-slot
/// discharge target SOCs are written for extended-slot models (Gen3, AIO,
/// HV Gen3) so the restore round-trips everything the snapshot had, not
/// just the slot times.
///
/// Performance: with a 10-element backup array, an early version emitted
/// 2 slot-register writes per element (20 writes) plus per-slot target-SOC
/// writes for extended-slot models. With the backend's 1.5-second-per-write
/// inter-write delay, a full restore of an unmodified backup took ~36
/// seconds — long enough to time out downstream tests. The current
/// implementation SKIPS slots that are not actually configured (i.e.
/// `enabled: false` AND all times zero): the inverter already holds zero
/// in those registers, so writing zero again is wasted Modbus traffic.
/// The user's only "configured" slots are restored, which is what the
/// user actually lost when Eco cleared them.
///
/// See issue #137.
fn restore_discharge_slot_writes(
    device_type: DeviceType,
    backup: &[crate::settings::DischargeSlotBackup],
) -> Option<Vec<RegisterWrite>> {
    if backup.is_empty() {
        return None;
    }
    let mut slot_writes: Vec<RegisterWrite> = Vec::new();
    for (idx, slot) in backup.iter().enumerate() {
        // Skip unconfigured slots — they're already zero in the
        // inverter's registers (Eco's slot-clear left them that way),
        // and writing zero again costs 1.5s of Modbus time per slot for
        // no observable change. Only configured slots (enabled, OR
        // non-zero times) need restoring.
        let is_configured = slot.enabled
            || slot.start_hour != 0
            || slot.start_minute != 0
            || slot.end_hour != 0
            || slot.end_minute != 0;
        if !is_configured {
            continue;
        }

        let slot_num = (idx + 1) as u8;
        let start = encode_hhmm(slot.start_hour, slot.start_minute);
        let end = encode_hhmm(slot.end_hour, slot.end_minute);

        let cmd = match discharge_slot_command_for_device(
            device_type,
            slot_num,
            slot.enabled,
            start,
            end,
        ) {
            Ok(cmd) => cmd,
            Err(e) => {
                tracing::warn!("Skipping backed-up discharge slot {}: {}", slot_num, e);
                continue;
            }
        };

        match cmd.encode() {
            Ok(mut w) => {
                // Per-slot discharge target SOC for extended-slot models.
                // Non-extended models key off the global target, which the
                // Timed mode encoder doesn't touch, so this stays a no-op
                // there — same pattern the existing `is_timed` body path
                // uses (api.rs:1083-1100).
                if slot.enabled && slot.target_soc > 0 && device_type.uses_extended_schedule_slots()
                {
                    if let Ok(target_writes) = (ControlCommand::SetDischargeTargetSocSlot {
                        slot: slot_num,
                        soc: slot.target_soc as u16,
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
                    "Failed to encode backed-up discharge slot {}: {}",
                    slot_num,
                    e
                );
            }
        }
    }
    if slot_writes.is_empty() {
        return None;
    }
    Some(slot_writes)
}

/// Capture the current discharge schedule (from `latest_snapshot`) into
/// `Settings.discharge_slots_backup` so a subsequent Eco→Timed round-trip
/// can restore the user's configured slots. Persists to disk via
/// `Settings::save()` — a backup is only useful if it survives a crash,
/// and the file write is the same atomic-rename path every other settings
/// mutation uses.
///
/// Returns the captured backup (so the API can echo it back to the
/// frontend, which surfaces the slots as pending edits in the Eco-mode
/// UI) or `None` if no slot is actually configured (no enabled slot with
/// non-zero start or end). When `None`, the existing backup field is
/// left untouched so a stale snapshot from earlier in the day doesn't get
/// restored by accident on the next Timed toggle.
///
/// See issue #137.
async fn capture_discharge_schedule_backup(
    state: &AppState,
) -> Option<Vec<crate::settings::DischargeSlotBackup>> {
    // Snapshot read: take the entire `discharge_slots` array so the backup
    // covers all 10 slots (Gen3 extended). The clear path zeroes every
    // slot the model supports (issue #289), so a restore from a partial
    // backup would silently drop slots 3–10.
    let slots = {
        let snap = state.latest_snapshot.lock().await;
        let snap = snap.as_ref()?;
        snap.discharge_slots.to_vec()
    };

    let has_any_configured_slot = slots
        .iter()
        .any(crate::inverter::model::ScheduleSlot::is_configured);
    if !has_any_configured_slot {
        // Nothing worth backing up — leave the existing backup field alone
        // so a stale snapshot from earlier in the day doesn't get restored
        // by accident on the next Timed toggle.
        return None;
    }

    let backup: Vec<crate::settings::DischargeSlotBackup> = slots
        .iter()
        .map(crate::settings::DischargeSlotBackup::from)
        .collect();

    // Transactional save (review #1 follow-up): holds the settings lock
    // across the read-modify-write so a concurrent writer can't clobber
    // this update.
    if let Err(e) = crate::settings::Settings::update(|s| {
        s.discharge_slots_backup = Some(backup.clone());
    }) {
        tracing::warn!(
            "Failed to persist discharge-slot backup, schedule may not round-trip on next Timed toggle: {e}"
        );
        return None;
    }
    Some(backup)
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

/// Merge an edited slot into the desired schedule at its actual slot index.
///
/// The persisted desired schedule is stored as an array whose position maps
/// to the slot number, so an edit beyond the current length must be placed
/// at its index (padding with default slots) — an append would land on the
/// wrong physical slot (reachable via the legacy-backup path where
/// `discharge_slots_backup` is shorter than the edited slot index;
/// CODE_REVIEW.md finding 2).
fn merge_desired_slot(
    desired: &mut Vec<crate::inverter::model::ScheduleSlot>,
    slot_number: u8,
    edited: crate::inverter::model::ScheduleSlot,
) {
    let slot_index = slot_number as usize - 1;
    if slot_index >= desired.len() {
        // Pad with default (unconfigured) slots so the edit lands at its
        // own index instead of shifting down onto the wrong slot.
        desired.resize(
            slot_index + 1,
            crate::inverter::model::ScheduleSlot::default(),
        );
    }
    desired[slot_index] = edited;
}

/// Queue register writes for execution by the poll loop.
///
/// `pub(crate)` so the poll loop's Forecast plan auto-refresh can route
/// its slot writes through the same pump as every other control path
/// instead of writing inline (CODE_REVIEW.md Major 1: inline writes with
/// inter-write sleeps stall the read/broadcast path).
pub(crate) async fn queue_writes(state: &Arc<AppState>, writes: Vec<RegisterWrite>) {
    queue_writes_with_policy(state, writes, WriteBatchPolicy::ContinueOnError, None, None).await;
}

/// Queue a batch that participates in the shared discharge-control arbiter.
/// Lower-priority automation batches remain queued while a higher-priority
/// owner is active, rather than overwriting that owner's registers.
async fn queue_owned_writes(
    state: &Arc<AppState>,
    writes: Vec<RegisterWrite>,
    owner: DischargeControlOwner,
) {
    queue_writes_with_policy(
        state,
        writes,
        WriteBatchPolicy::ContinueOnError,
        None,
        Some(owner),
    )
    .await;
}

/// Completion-aware variant for an owned discharge-control batch.
async fn queue_owned_writes_with_completion(
    state: &Arc<AppState>,
    writes: Vec<RegisterWrite>,
    owner: DischargeControlOwner,
) -> tokio::sync::oneshot::Receiver<WriteOutcome> {
    queue_writes_with_policy_completion(
        state,
        writes,
        WriteBatchPolicy::ContinueOnError,
        Some(owner),
    )
    .await
}

/// Transactional Timed Export write. If the API request times out, the poll
/// loop cancels any unstarted/remaining writes instead of applying them after
/// the handler has returned an error and declined to persist the schedule.
async fn queue_owned_writes_transactional(
    state: &Arc<AppState>,
    writes: Vec<RegisterWrite>,
    owner: DischargeControlOwner,
) -> tokio::sync::oneshot::Receiver<WriteOutcome> {
    queue_writes_with_policy_completion(
        state,
        writes,
        WriteBatchPolicy::FailFastTransactional,
        Some(owner),
    )
    .await
}

/// Completion-backed fail-fast writes for the Timed Export slot/target
/// registers, tagged with [`DischargeControlOwner::ManualMode`] ownership.
///
/// CODE_REVIEW.md finding 4: an ownerless batch is always admitted by the
/// poll loop, so a slot edit saved while Force Discharge ran would overwrite
/// the temporary force-discharge slot — and stopping Force Discharge would
/// then restore the older captured slot, silently losing the user's edit.
/// ManualMode ownership defers the batch behind ManualForce until Force
/// Discharge has released the registers (the awaited completion then times
/// out transactionally, so the edit fails cleanly and is retried). The
/// existing manual-selection exception keeps these batches admitted while a
/// register-derived ExplicitPause owns the cycle — slot/target values may be
/// prepared while HR318 owns a pause; only the later HR27/discharge-enable
/// arm transition must wait for that owner to release.
async fn queue_writes_transactional(
    state: &Arc<AppState>,
    writes: Vec<RegisterWrite>,
) -> tokio::sync::oneshot::Receiver<WriteOutcome> {
    queue_writes_with_policy_completion(
        state,
        writes,
        WriteBatchPolicy::FailFastTransactional,
        Some(DischargeControlOwner::ManualMode),
    )
    .await
}

async fn queue_writes_with_policy(
    state: &Arc<AppState>,
    writes: Vec<RegisterWrite>,
    policy: WriteBatchPolicy,
    completion: Option<tokio::sync::oneshot::Sender<WriteOutcome>>,
    owner: Option<DischargeControlOwner>,
) {
    let mut pw = state.pending_writes.lock().await;
    tracing::info!("Queued {} register write(s)", writes.len());
    pw.push(PendingWriteBatch {
        writes,
        completion,
        policy,
        owner,
    });
    drop(pw);
    // Wake the poll loop immediately so writes are applied without
    // waiting for the next read cycle or sleep interval.
    state.write_notify.notify_one();
}

async fn queue_writes_with_policy_completion(
    state: &Arc<AppState>,
    writes: Vec<RegisterWrite>,
    policy: WriteBatchPolicy,
    owner: Option<DischargeControlOwner>,
) -> tokio::sync::oneshot::Receiver<WriteOutcome> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    queue_writes_with_policy(state, writes, policy, Some(tx), owner).await;
    rx
}

/// How long to wait for the poll loop to finish executing a write batch
/// before returning to the caller. Comfortably covers the normal pause batch
/// (~3 writes plus the 1.5 s inter-write gap) and a few retries on a glitchy
/// register, while staying well under the frontend's 30 s confirmation
/// timeout. On timeout the writes are still queued and will complete; we
/// return success so the normal snapshot-confirmation path covers the
/// slow-but-progressing case.
const WRITE_COMPLETION_TIMEOUT: Duration = Duration::from_secs(15);

/// Completion timeout scaled to batch size: the base covers ownership
/// deferrals and per-register retries, and each queued write adds its 1.5 s
/// inter-write pacing so a full slot-clear batch (up to 10 slots × 2
/// registers on extended models) cannot spuriously time out.
fn batch_completion_timeout(write_count: usize) -> Duration {
    WRITE_COMPLETION_TIMEOUT + Duration::from_millis(write_count as u64 * 1500)
}

/// Await the outcome of a write batch queued via
/// [`queue_writes_with_completion`]. Returns `Ok(())` when every register was
/// accepted, or `Err(message)` naming the first register the inverter
/// rejected. A timeout is treated as success (see [`WRITE_COMPLETION_TIMEOUT`]).
async fn await_write_outcome(
    rx: tokio::sync::oneshot::Receiver<WriteOutcome>,
) -> Result<(), String> {
    await_write_outcome_with_timeout(rx, WRITE_COMPLETION_TIMEOUT).await
}

/// Testable implementation of [`await_write_outcome`] with an injectable
/// timeout. Production callers use [`WRITE_COMPLETION_TIMEOUT`]; tests can
/// exercise the slow-but-progressing fallback without waiting 15 seconds.
async fn await_write_outcome_with_timeout(
    rx: tokio::sync::oneshot::Receiver<WriteOutcome>,
    timeout: Duration,
) -> Result<(), String> {
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(WriteOutcome::Ok)) => Ok(()),
        Ok(Ok(WriteOutcome::Failed {
            address,
            value,
            error,
        })) => Err(format!(
            "the inverter rejected the write to register {address} (value {value}): {error}"
        )),
        Ok(Err(_)) => Err("the write batch was dropped before completing".to_string()),
        Err(_) => {
            tracing::debug!(
                "Write batch still in progress after {:?} — leaving confirmation to the poll loop",
                timeout
            );
            Ok(())
        }
    }
}

/// Await an outcome that gates a settings transaction. Unlike ordinary UI
/// confirmation, a timeout is an error: callers must not persist or report a
/// schedule whose required physical writes are still deferred. Dropping the
/// receiver also tells a `FailFastTransactional` batch to cancel.
async fn await_required_write_outcome(
    rx: tokio::sync::oneshot::Receiver<WriteOutcome>,
) -> Result<(), String> {
    await_required_write_outcome_with_timeout(rx, WRITE_COMPLETION_TIMEOUT).await
}

/// Await a required multi-register batch with enough time for the poll loop's
/// mandatory inter-write pacing. Extended Timed Export schedules can contain
/// dozens of slot and target-SOC writes, so the fixed base timeout is only
/// suitable for short mode-transition batches.
async fn await_required_batch_write_outcome(
    rx: tokio::sync::oneshot::Receiver<WriteOutcome>,
    write_count: usize,
) -> Result<(), String> {
    await_required_write_outcome_with_timeout(rx, batch_completion_timeout(write_count)).await
}

async fn await_required_write_outcome_with_timeout(
    rx: tokio::sync::oneshot::Receiver<WriteOutcome>,
    timeout: Duration,
) -> Result<(), String> {
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(WriteOutcome::Ok)) => Ok(()),
        Ok(Ok(WriteOutcome::Failed {
            address,
            value,
            error,
        })) => Err(format!(
            "the inverter rejected the write to register {address} (value {value}): {error}"
        )),
        Ok(Err(_)) => Err("the write batch was dropped before completing".to_string()),
        Err(_) => Err(format!(
            "the required inverter write did not complete within {} seconds",
            timeout.as_secs()
        )),
    }
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
        HR_CHARGE_SLOT_1_END, HR_CHARGE_SLOT_1_START, HR_CHARGE_TARGET_SOC, HR_ENABLE_CHARGE,
        HR_ENABLE_CHARGE_TARGET, HR_ENABLE_DISCHARGE,
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
            value: if revert.three_phase_force_charge_enable.unwrap_or(false) {
                1
            } else {
                0
            },
        });
        writes.push(RegisterWrite {
            address: HR_3PH_AC_CHARGE_ENABLE,
            value: if revert.three_phase_ac_charge_enable.unwrap_or(false) {
                1
            } else {
                0
            },
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
            (
                Some((slot.start_hour, slot.start_minute)),
                Some((slot.end_hour, slot.end_minute)),
            )
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
        // HR318/319/320 pre-state (issue #289): the force action may
        // temporarily disable pause mode so discharge can run; Stop
        // Discharge / auto-revert restores the exact prior configuration.
        battery_pause_mode: snap.battery_pause_mode,
        battery_pause_slot: snap.battery_pause_slot.clone(),
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
        HR_DISCHARGE_SLOT_2_START, HR_ENABLE_CHARGE, HR_ENABLE_CHARGE_TARGET, HR_ENABLE_DISCHARGE,
    };
    let mut writes = Vec::new();

    if device_type.uses_three_phase_schedule_slots() {
        // Three-phase path: restore the force-discharge and force-charge
        // enable flags, then return to eco mode. The poll loop will resync
        // the slot registers from the HR 1080-1124 block.
        writes.push(RegisterWrite {
            address: HR_3PH_FORCE_DISCHARGE_ENABLE,
            value: if revert.three_phase_force_discharge_enable.unwrap_or(false) {
                1
            } else {
                0
            },
        });
        writes.push(RegisterWrite {
            address: HR_3PH_FORCE_CHARGE_ENABLE,
            value: if revert.three_phase_force_charge_enable.unwrap_or(false) {
                1
            } else {
                0
            },
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

    // Restore the pre-force HR318/319/320 pause configuration (issue #289)
    // on EVERY device family — including three-phase / AC-three-phase, whose
    // enable/slot restoration was routed through the three-phase register
    // bank above but whose pause schedule lives in the common holding block
    // and was captured + temporarily disabled just the same. Slot values
    // precede the pause mode so an enabled mode never coexists with stale
    // window data.
    crate::inverter::state_machines::push_pause_restore_writes(
        &mut writes,
        Some(revert.battery_pause_mode),
        Some(&revert.battery_pause_slot),
    );

    writes
}

/// Build a minimal safe Stop Discharge sequence after an app restart.
///
/// The full stop path normally restores a `ForceDischargeRevert` captured
/// when Force Discharge was started. That snapshot is volatile and is lost
/// when the app restarts. If the inverter is still armed for discharge after
/// the restart, the Quick Actions button still renders as "Stop Discharge",
/// but the old no-revert path returned 400 and queued no register writes at
/// all. In that restart-recovery case we cannot restore the user's previous
/// schedule, but we can safely stop the active discharge: clear the discharge
/// enable/force flag and return to self-consumption.
fn build_force_discharge_restart_stop_writes(
    device_type: DeviceType,
    snapshot: &InverterSnapshot,
) -> Vec<RegisterWrite> {
    use crate::modbus::registers::{
        HR_3PH_FORCE_DISCHARGE_ENABLE, HR_BATTERY_POWER_MODE, HR_ENABLE_DISCHARGE,
    };

    if !snapshot.enable_discharge {
        return Vec::new();
    }

    let mut writes = Vec::new();
    if device_type.uses_three_phase_schedule_slots() {
        writes.push(RegisterWrite {
            address: HR_3PH_FORCE_DISCHARGE_ENABLE,
            value: 0,
        });
    } else {
        writes.push(RegisterWrite {
            address: HR_ENABLE_DISCHARGE,
            value: 0,
        });
    }
    writes.push(RegisterWrite {
        address: HR_BATTERY_POWER_MODE,
        value: 1,
    });
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
    let cs_val = state.connected_since.lock().ok().and_then(|guard| *guard);
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
            // Issue #131: surface the import-side Standing Charge so the
            // Settings page can hydrate its p/day input on load.
            "import_standing_charge_p_per_day": settings.import_standing_charge_p_per_day,
            "import_tariff_config": settings.import_tariff_config,
            "export_tariff_config": settings.export_tariff_config,
            "octopus_enabled": settings.octopus_enabled,
            "octopus_account_number": settings.octopus_account_number,
            "octopus_api_key_configured": !settings.octopus_api_key.is_empty(),
            "octopus_gas_unit": settings.octopus_gas_unit,
            "octopus_economy7_start": settings.octopus_economy7_start,
            "octopus_economy7_end": settings.octopus_economy7_end,
            // The Octopus API key is intentionally never returned.
            "hidden_panels": settings.hidden_panels,
            "evc_host": settings.evc_host,
            "evc_port": settings.evc_port,
            "disable_auto_discovery": settings.disable_auto_discovery,
            "autostart_enabled": settings.autostart_enabled,
            // "New version available" banner gate.
            "check_for_updates": settings.check_for_updates,
            // Issue #217: tray window preferences. Both default to false so
            // an older settings.json simply reports them as off.
            "minimise_to_tray": settings.minimise_to_tray,
            "start_minimised": settings.start_minimised,
            "api_key": settings.api_key,
            "api_port": settings.api_port,
            // Issue #137: surface the discharge-slot backup so the frontend
            // can stage it as pending edits after a Timed→Eco round-trip.
            // `None` on first install / before any capture; otherwise the
            // 10-element Vec captured on the most recent Eco/Pause entry.
            "discharge_slots_backup": settings.discharge_slots_backup,
            // Issue #110: solar array capacities for "% of max" display.
            // DC-string ratings (hybrid) + CT-meter labels (AC-coupled).
            "pv1_rated_kw": settings.pv1_rated_kw,
            "pv2_rated_kw": settings.pv2_rated_kw,
            "solar_arrays": settings.solar_arrays,
            // Issue #283: forecast efficiencies consumed by the SOC
            // projection on the Forecast page.
            "forecast_charge_efficiency": settings.forecast_charge_efficiency,
            "forecast_discharge_efficiency": settings.forecast_discharge_efficiency,
            // Min SOC the planner keeps the battery above across the
            // forward forecast window (issue #283, planner v2).
            "forecast_min_soc_pct": settings.forecast_min_soc_pct,
            // Nightly auto-refresh of the Forecast plan's charge slot:
            // re-sizes slot 1 from the live SOC before each cheap period.
            "forecast_plan_auto_refresh": settings.forecast_plan_auto_refresh,
            // Auto-apply of the Forecast plan: applies the calculated
            // charging plan the configured number of minutes before the
            // cheap charging tariff window and notifies the user.
            "forecast_plan_auto_apply_enabled": settings.forecast_plan_auto_apply_enabled,
            "forecast_plan_auto_apply_lead_minutes": settings.forecast_plan_auto_apply_lead_minutes,
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

    // Validate the tariff config fields if present. We do NOT fall back to a
    // pre-lock disk default here: the closure only writes a config when the
    // body actually supplied it, and Settings::update loads the freshest
    // disk state under the lock, so omitting the field preserves whatever is
    // on disk. Falling back to a pre-lock read and writing it back
    // unconditionally would revert a concurrent writer's update to the same
    // field (the clobber class fixed for the scalar import/export tariff).
    // (The old pre-lock default read was only to keep file I/O off the Tokio
    // worker; that concern is moot now the closure owns the disk read.)
    let import_tariff_config =
        match parse_and_validate_tariff(body.get("import_tariff_config"), "import_tariff_config") {
            Ok(v) => v,
            Err(e) => return error_response(&e),
        };
    let export_tariff_config =
        match parse_and_validate_tariff(body.get("export_tariff_config"), "export_tariff_config") {
            Ok(v) => v,
            Err(e) => return error_response(&e),
        };

    // Transactional save (review #1 third cycle): wrap the entire
    // field-build + save in Settings::update so the read-modify-write
    // happens under the settings lock. A concurrent update_settings call
    // racing on disjoint fields (host vs import_tariff, etc.) used to
    // clobber each other's field because both callers loaded the full
    // Settings struct, mutated their own field, and saved back the
    // complete struct — the second save overwrote the first. Now the
    // closure mutates the just-loaded `s` in-place under the lock, so
    // concurrent callers serialise on the settings lock and both
    // disjoint updates land.
    //
    // Validation runs inside the closure and can REJECT the whole write:
    // `try_update` discards the partially-mutated settings on Err, so a
    // 400-rejected request leaves disk completely untouched (all-or-
    // nothing). The closure returns the post-mutation snapshot for the
    // log/response block below — captured under the lock, so it cannot
    // race with a concurrent writer the way a post-save load() would.
    let save_result = crate::settings::Settings::try_update(|persist| {
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
        // Tariff fields: only write the field if it was actually present
        // in the request body. The old code merged `body.get(...).unwrap_or(disk_default)`
        // which unconditionally re-wrote the field with the disk-default
        // value, clobbering a concurrent writer's update to the same field.
        // The new conditional write is functionally identical when the
        // caller doesn't supply the field (the disk value is preserved in
        // both cases) but doesn't clobber a concurrent caller who DID
        // supply a different value for the same field.
        if let Some(v) = body.get("import_tariff").and_then(|v| v.as_f64()) {
            persist.import_tariff = v;
        }
        if let Some(v) = body.get("export_tariff").and_then(|v| v.as_f64()) {
            persist.export_tariff = v;
        }
        // Issue #131: Standing Charge is pence/day; we accept any non-negative
        // number. Negative values are clamped to 0 — a Standing Charge that
        // *credits* the customer doesn't exist in any real UK tariff and would
        // let a UI bug silently invert the cost graph.
        if let Some(sc) = body
            .get("import_standing_charge_p_per_day")
            .and_then(|v| v.as_f64())
        {
            persist.import_standing_charge_p_per_day = sc.max(0.0);
        }
        if let Some(ref cfg) = import_tariff_config {
            persist.import_tariff_config = Some(cfg.clone());
        }
        if let Some(ref cfg) = export_tariff_config {
            persist.export_tariff_config = Some(cfg.clone());
        }
        if let Some(hp) = body.get("http_port").and_then(|v| v.as_u64()) {
            persist.http_port = hp.min(u16::MAX as u64) as u16;
        }
        if let Some(enabled) = body.get("octopus_enabled").and_then(|v| v.as_bool()) {
            persist.octopus_enabled = enabled;
        }
        if let Some(account) = body.get("octopus_account_number").and_then(|v| v.as_str()) {
            persist.octopus_account_number = account.trim().to_string();
        }
        if let Some(key) = body.get("octopus_api_key").and_then(|v| v.as_str()) {
            persist.octopus_api_key = key.trim().to_string();
        }
        if let Some(unit) = body.get("octopus_gas_unit").and_then(|v| v.as_str()) {
            if !matches!(unit, "unknown" | "kwh" | "m3") {
                return Err("octopus_gas_unit must be unknown, kwh, or m3".to_string());
            }
            persist.octopus_gas_unit = unit.to_string();
        }
        let valid_hhmm = |value: &str| {
            value.split_once(':').is_some_and(|(hour, minute)| {
                hour.len() == 2
                    && minute.len() == 2
                    && hour.parse::<u8>().is_ok_and(|h| h < 24)
                    && minute.parse::<u8>().is_ok_and(|m| m < 60)
            })
        };
        if let Some(value) = body.get("octopus_economy7_start").and_then(|v| v.as_str()) {
            if !valid_hhmm(value) {
                return Err("octopus_economy7_start must be a valid HH:MM time".to_string());
            }
            persist.octopus_economy7_start = value.to_string();
        }
        if let Some(value) = body.get("octopus_economy7_end").and_then(|v| v.as_str()) {
            if !valid_hhmm(value) {
                return Err("octopus_economy7_end must be a valid HH:MM time".to_string());
            }
            persist.octopus_economy7_end = value.to_string();
        }
        if persist.octopus_economy7_start == persist.octopus_economy7_end {
            return Err("Octopus Economy 7 start and end times must differ".to_string());
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
        // "New version available" banner gate. Disabling stops the background
        // loop's GitHub fetches and makes the banner hide on next poll.
        if let Some(c) = body.get("check_for_updates").and_then(|v| v.as_bool()) {
            persist.check_for_updates = c;
        }
        // Persist the user's autostart preference. The actual platform
        // autostart entry is driven from the frontend via the
        // @tauri-apps/plugin-autostart JS bindings (so the toast and
        // toggling happen client-side), and the startup self-heal path in
        // lib.rs re-applies it after a crash/restart. See issue #117.
        if let Some(a) = body.get("autostart_enabled").and_then(|v| v.as_bool()) {
            persist.autostart_enabled = a;
        }
        // Issue #217: tray window preferences. `minimise_to_tray` takes effect
        // immediately (the close handler reads it live on each window close);
        // `start_minimised` takes effect on the next launch.
        if let Some(m) = body.get("minimise_to_tray").and_then(|v| v.as_bool()) {
            persist.minimise_to_tray = m;
        }
        if let Some(s) = body.get("start_minimised").and_then(|v| v.as_bool()) {
            persist.start_minimised = s;
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
        // Issue #110: solar array capacities for "% of max" display. Negative
        // ratings are clamped to 0 (a negative array size is nonsensical and
        // would invert the % display). Meter addresses are validated at
        // compute time (only 1-8 are honoured), so we accept any u8 here and
        // let the poll loop drop bogus entries.
        if let Some(kw) = body.get("pv1_rated_kw").and_then(|v| v.as_f64()) {
            persist.pv1_rated_kw = kw.max(0.0);
        }
        if let Some(kw) = body.get("pv2_rated_kw").and_then(|v| v.as_f64()) {
            persist.pv2_rated_kw = kw.max(0.0);
        }
        // Issue #283: forecast battery efficiencies for the SOC projection.
        // Range-validated so a typo can't silently zero the projection's
        // energy accounting.
        if let Some(v) = body
            .get("forecast_charge_efficiency")
            .and_then(|v| v.as_f64())
        {
            if !(0.5..=1.0).contains(&v) {
                return Err("Charge efficiency must be between 0.5 and 1.0".to_string());
            }
            persist.forecast_charge_efficiency = v;
        }
        if let Some(v) = body
            .get("forecast_discharge_efficiency")
            .and_then(|v| v.as_f64())
        {
            if !(0.5..=1.0).contains(&v) {
                return Err("Discharge efficiency must be between 0.5 and 1.0".to_string());
            }
            persist.forecast_discharge_efficiency = v;
        }
        if let Some(v) = body.get("forecast_min_soc_pct").and_then(|v| v.as_f64()) {
            if !(0.0..=100.0).contains(&v) {
                return Err("Min SOC must be between 0 and 100 percent".to_string());
            }
            persist.forecast_min_soc_pct = v;
        }
        if let Some(v) = body
            .get("forecast_plan_auto_refresh")
            .and_then(|v| v.as_bool())
        {
            persist.forecast_plan_auto_refresh = v;
        }
        if let Some(v) = body
            .get("forecast_plan_auto_apply_enabled")
            .and_then(|v| v.as_bool())
        {
            persist.forecast_plan_auto_apply_enabled = v;
        }
        if let Some(v) = body
            .get("forecast_plan_auto_apply_lead_minutes")
            .and_then(|v| v.as_u64())
        {
            if v > crate::forecast::refresh::PLAN_AUTO_APPLY_MAX_LEAD_MINUTES as u64 {
                return Err(format!(
                    "Auto-apply lead time must be between 0 and {} minutes",
                    crate::forecast::refresh::PLAN_AUTO_APPLY_MAX_LEAD_MINUTES
                ));
            }
            persist.forecast_plan_auto_apply_lead_minutes = v as u16;
        }
        if let Some(arrays) = body.get("solar_arrays").and_then(|v| v.as_array()) {
            let parsed: Vec<crate::settings::SolarArrayConfig> = arrays
                .iter()
                .filter_map(|v| {
                    serde_json::from_value::<crate::settings::SolarArrayConfig>(v.clone()).ok()
                })
                .collect();
            persist.solar_arrays = parsed;
        }
        // Capture the post-mutation settings for the log message below.
        // We must clone here because the closure's `persist` is dropped
        // when the lock is released at the end of Settings::update.
        Ok(persist.clone())
    });
    // Outer Err = lock/save failure (500); inner Err = validation
    // rejection (400, nothing persisted); Ok = saved, snapshot returned.
    let persist_for_log = match save_result {
        Err(e) => return server_error(&format!("Failed to save settings: {e}")),
        Ok(Err(msg)) => return error_response(&msg),
        Ok(Ok(snapshot)) => snapshot,
    };

    // Build the log/response message from the just-persisted settings so
    // it reflects the values actually written to disk (not the raw
    // request body). The snapshot was captured inside the closure above
    // — no post-save Settings::load() that could race with a concurrent
    // writer (review #1 follow-up).
    let fields = settings_log_fields(&body, &persist_for_log);
    let msg = if fields.is_empty() {
        "Settings updated: (no fields in request body)".to_string()
    } else {
        format!("Settings updated: {}", fields.join(", "))
    };
    tracing::info!("{}", msg);

    // Issue #110 / local-E2E "solar arrays never appear": when the save
    // touched any of the solar-array settings, re-stamp the kWp-derived
    // fields (`solar_arrays`, `pv1_pct`, `pv2_pct`) on the CURRENT
    // snapshot right away. The poll loop normally does this every cycle,
    // but after a control-page change it can spend minutes draining queued
    // register writes (1.5 s per write) before its next read — without
    // this re-stamp (plus broadcast), the Solar page's Solar Arrays card
    // lags the save by that whole drain window.
    //
    // NOTE: this key set must cover every settings field that
    // `stamp_solar_array_fields` / `compute_solar_arrays` reads — today
    // `pv1_rated_kw`, `pv2_rated_kw`, and the `solar_arrays` CT-meter list.
    // Adding a new solar-derived setting means extending BOTH this
    // condition and the poll loop's stamping, or saves of the new field
    // won't restamp the current snapshot.
    if body.get("pv1_rated_kw").is_some()
        || body.get("pv2_rated_kw").is_some()
        || body.get("solar_arrays").is_some()
    {
        let mut snapshot = state.latest_snapshot.lock().await;
        if let Some(snap) = snapshot.as_mut() {
            stamp_solar_array_fields(snap, &persist_for_log);
            let _ = state.tx.send(PollMessage::Snapshot(Box::new(snap.clone())));
        }
    }

    let response = ok_response(&msg);

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
    // Sync EVC settings + auto-discovery flag from persisted config to in-memory PollSettings.
    // Uses the closure-captured snapshot rather than re-reading disk: the
    // snapshot is exactly what this request just wrote, so the in-memory
    // mirror can't pick up a concurrent writer's EVC values mid-request.
    settings.evc_host = persist_for_log.evc_host.clone();
    settings.evc_port = persist_for_log.evc_port;
    settings.disable_auto_discovery = persist_for_log.disable_auto_discovery;

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
    if is_present("import_standing_charge_p_per_day") {
        out.push(format!(
            "import_standing_charge={}p/day",
            persist.import_standing_charge_p_per_day
        ));
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
    if is_present("octopus_enabled") {
        out.push(format!("octopus_enabled={}", persist.octopus_enabled));
    }
    if is_present("octopus_account_number") {
        out.push(format!(
            "octopus_account={}",
            persist.octopus_account_number
        ));
    }
    if is_present("octopus_api_key") {
        out.push(if persist.octopus_api_key.is_empty() {
            "octopus_api_key=cleared".to_string()
        } else {
            format!(
                "octopus_api_key=set ({} chars)",
                persist.octopus_api_key.len()
            )
        });
    }
    if is_present("octopus_gas_unit") {
        out.push(format!("octopus_gas_unit={}", persist.octopus_gas_unit));
    }
    if is_present("octopus_economy7_start") || is_present("octopus_economy7_end") {
        out.push(format!(
            "octopus_economy7_window={}-{}",
            persist.octopus_economy7_start, persist.octopus_economy7_end
        ));
    }
    if is_present("hidden_panels") {
        out.push(format!(
            "hidden_panels={} entries",
            persist.hidden_panels.len()
        ));
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
    if is_present("check_for_updates") {
        out.push(format!("check_for_updates={}", persist.check_for_updates));
    }
    if is_present("autostart_enabled") {
        out.push(format!("autostart_enabled={}", persist.autostart_enabled));
    }
    if is_present("minimise_to_tray") {
        out.push(format!("minimise_to_tray={}", persist.minimise_to_tray));
    }
    if is_present("start_minimised") {
        out.push(format!("start_minimised={}", persist.start_minimised));
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
        interval_secs,           // caller will merge: 0 means "keep existing"
        version: 0,              // not set by the API; caller bumps it
        evc_host: String::new(), // merged from disk settings separately
        evc_port: 502,
        disable_auto_discovery,
    })
}

/// Parse and validate an optional tariff config from a request body field.
///
/// Returns `Ok(None)` both when the key is absent and when it is
/// explicitly `null` — the caller (`update_settings`' closure) treats both
/// as "preserve whatever is on disk" and only writes `Some(cfg)`. Note
/// this means there is currently no way to CLEAR a tariff config via the
/// API; the settings UI always sends a full config, so a clear path has
/// never been needed. Distinguish `null` from absent here (and write
/// `None` in the closure) if a clear path is ever added. Returns `Err(msg)`
/// when the field is present but malformed or fails validation — the
/// message is surfaced to the user as a 400 response.
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
    // An omitted `soc_reserve` must preserve the snapshot's configured
    // reserve rather than silently resetting it to the 4% default — the same
    // omit-default clobber class as the 0.74.10 charge-slot `target_soc` fix.
    // An explicit value always wins.
    let soc_reserve = match body["soc_reserve"].as_u64() {
        Some(v) => v as u16,
        None => state
            .latest_snapshot
            .lock()
            .await
            .as_ref()
            .map(|s| s.battery_reserve as u16)
            // Default only when nothing is connected/configured yet.
            .filter(|r| *r >= 4)
            .unwrap_or(4),
    };

    let is_timed = mode_str == "timed_demand" || mode_str == "timed_export";

    // Issue #289 / code-review finding: `mode=timed_export` must not bypass
    // the HEM-managed schedule. The raw `SetTimedExportMode` command writes
    // HR27=0/HR59=1 immediately and leaves them armed — recreating the
    // all-day maximum-power mode the redesign removes, with no persisted
    // schedule and no window boundaries. Route every Timed Export entry
    // point through the managed implementation instead: it persists the
    // desired schedule and arms export only inside the configured windows.
    if mode_str == "timed_export" {
        tracing::info!("set_mode(timed_export): routing through the managed Timed Export schedule");
        let mut managed_body = serde_json::json!({ "enabled": true });
        if let Some(soc) = body["soc_reserve"].as_u64() {
            managed_body["soc_reserve"] = serde_json::json!(soc);
        }
        return set_timed_export(State(state), Json(managed_body)).await;
    }

    // Serialise the legacy mode routes against every other Timed Export
    // mutation: the `timed_demand` and Eco-family branches below disable the
    // managed schedule and queue discharge-register writes, and without the
    // action lock a concurrent Timed Export enable or slot save could
    // persist its stale enabled schedule after this request returned —
    // re-arming export and silently undoing the user's manual selection.
    // Acquired *after* the `timed_export` routing above: that branch
    // delegates to `set_timed_export`, which takes the lock itself, and
    // `tokio::sync::Mutex` is not re-entrant.
    let _timed_export_action = state.timed_export_action_lock.lock().await;

    // A raw Timed Demand selection is an explicit manual mode, not an
    // instruction to keep the HEM-managed Timed Export schedule alive. Stop
    // that lower-priority automation before the mode batch is queued; the
    // desired export slots remain persisted for a later explicit re-enable.
    if mode_str == "timed_demand" {
        let slots = state.timed_export_config.lock().await.slots.clone();
        // Durable stop/exit-pending marker (CODE_REVIEW.md BLOCKER), moved in
        // the SAME settings transaction as the disable: if the process dies
        // before the mode batch lands, the restart resumes the exit — and a
        // crash can never leave the marker and the schedule disagreeing.
        if let Err(error) =
            persist_timed_export_schedule_with_stop_pending(&state, false, slots, true).await
        {
            return error_response(&format!(
                "Could not save the Timed Export schedule: {error}"
            ));
        }
        *state.timed_export_state.lock().await =
            crate::inverter::state_machines::TimedExportState::Off;
    }

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

            // Captured backup (if any) for the response payload. Captured
            // only when entering Eco/Pause/Export Paused with a configured
            // schedule (see issue #137); the frontend uses it to surface
            // the slots as pending edits so the Eco-mode UI shows the
            // user's saved schedule after an Eco→Timed→Eco round-trip.
            let mut captured_backup: Option<Vec<crate::settings::DischargeSlotBackup>> = None;
            // Tracks whether the queued write batch includes a slot-restore
            // from the #137 backup. When true, the handler must await
            // write confirmation before clearing the backup from disk —
            // otherwise a rejected register write silently destroys the
            // user's schedule (the bug class 2877466 fixed for
            // set_timed_export but missed here).
            let mut restoring_backup = false;
            let owner = match mode_str {
                "eco_paused" | "export_paused" => DischargeControlOwner::ExplicitPause,
                _ => DischargeControlOwner::ManualMode,
            };

            // When switching to Eco / Pause / Export Paused, the Gen3
            // inverter firmware re-asserts `enable_discharge` whenever any
            // discharge slot register is non-zero, so the slot registers
            // must be zeroed to make Eco "stick". Doing so erases the
            // user's configured schedule — issue #137 — so snapshot the
            // current schedule into `Settings` *before* clearing it, and
            // restore it on the way back to Timed (see the `is_timed`
            // branch below).
            if mode_str == "eco" || mode_str == "eco_paused" || mode_str == "export_paused" {
                // The user is explicitly choosing an Eco-family baseline:
                // the managed Timed Export schedule must stop re-entering
                // its windows, or the poll loop would re-arm export at the
                // next boundary and silently undo the mode change
                // (code-review finding: ownership coordination). The
                // *desired* slots stay persisted so a later Timed Export
                // enable restores the schedule without a legacy backup.
                {
                    let slots = state.timed_export_config.lock().await.slots.clone();
                    // Durable stop/exit-pending marker moved atomically with
                    // the disable — same restart-recovery rationale as the
                    // timed_demand branch above.
                    if let Err(error) =
                        persist_timed_export_schedule_with_stop_pending(&state, false, slots, true)
                            .await
                    {
                        return error_response(&format!(
                            "Could not save the Timed Export schedule: {error}"
                        ));
                    }
                    *state.timed_export_state.lock().await =
                        crate::inverter::state_machines::TimedExportState::Off;
                }

                // Back up the user's existing schedule (issue #137). No-op
                // when nothing is configured, so a phantom backup can't
                // later "restore" an empty schedule and unlock the Timed
                // button spuriously.
                captured_backup = capture_discharge_schedule_backup(&state).await;

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
            //
            // If the body doesn't carry discharge_slots but a backup exists
            // from a prior Eco entry (issue #137), restore from the backup
            // instead. The body always wins: an explicit fresh schedule
            // overrides any stale snapshot from earlier in the day.
            if is_timed {
                let device_type = latest_device_type(&state).await;
                if let Some(slots) = body["discharge_slots"].as_array() {
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
                } else {
                    // No explicit body slots: try the backup (issue #137).
                    //
                    // The backup is consumed ONLY after the inverter
                    // confirms the slot-restore writes (mirrors the
                    // 2877466 pattern in `set_timed_export`). Until then
                    // the backup must stay on disk so a rejected write
                    // doesn't silently destroy the user's schedule.
                    let settings = crate::settings::Settings::load();
                    if let Some(backup) = settings.discharge_slots_backup.as_ref() {
                        if let Some(slot_writes) =
                            restore_discharge_slot_writes(device_type, backup)
                        {
                            // Slot writes go FIRST so they're on the inverter
                            // before HR59=1 is set — same invariant as the
                            // body path above.
                            let mut combined = slot_writes;
                            combined.append(&mut writes);
                            writes = combined;
                            restoring_backup = true;
                        }
                    }
                }
            }

            if restoring_backup {
                // Await confirmation before consuming the #137 backup
                // (same invariant as set_timed_export, see 2877466).
                let rx = queue_writes_with_policy_completion(
                    &state,
                    writes,
                    WriteBatchPolicy::ContinueOnError,
                    Some(owner),
                )
                .await;
                match await_write_outcome(rx).await {
                    Ok(()) => {
                        // Transactional save (review #1 follow-up): the
                        // backup clear happens under the settings lock so
                        // a concurrent writer can't sneak a backup-reseed
                        // in between our read and our save. Matches the
                        // pattern in set_timed_export.
                        if let Err(e) = crate::settings::Settings::update(|s| {
                            s.discharge_slots_backup = None;
                        }) {
                            tracing::warn!(
                                "Failed to clear discharge-slot backup after set_mode restore: {e}"
                            );
                        }
                        ok_response_with_backup(
                            &format!("Mode set to {mode_str}"),
                            captured_backup.as_deref(),
                        )
                    }
                    Err(msg) => {
                        tracing::warn!("set_mode slot restore rejected: {msg}");
                        (
                            StatusCode::BAD_GATEWAY,
                            Json(json!({
                                "ok": false,
                                "error": format!(
                                    "Mode could not be set to {mode_str} — restoring the saved discharge schedule failed ({msg}). The schedule backup has been kept; please try again."
                                )
                            })),
                        )
                    }
                }
            } else {
                queue_writes_with_policy(
                    &state,
                    writes,
                    WriteBatchPolicy::ContinueOnError,
                    None,
                    Some(owner),
                )
                .await;
                ok_response_with_backup(
                    &format!("Mode set to {mode_str}"),
                    captured_backup.as_deref(),
                )
            }
        }
        Err(e) => error_response(&format!("Validation error: {e}")),
    }
}

/// POST /api/control/eco — toggle Eco / self-consumption.
///
/// Body: `{ "enabled": true }`. When enabling, this is treated the same as
/// `set_mode({ mode: "eco" })`: it captures the current discharge schedule
/// into a backup (issue #137), clears HR59, zeroes all discharge slot
/// registers (Gen3 firmware re-asserts `enable_discharge` when slots are
/// non-zero), and finally restores self-consumption (HR27=1). When
/// disabling, it writes HR27=0 only.
pub async fn set_eco(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let enabled = body["enabled"].as_bool().unwrap_or(true);

    if !enabled {
        let cmd = ControlCommand::SetBatteryPowerMode { mode: 0 };
        match cmd.encode() {
            Ok(writes) => {
                tracing::info!("SetEco encoded: {:?}", writes);
                queue_owned_writes(&state, writes, DischargeControlOwner::ManualMode).await;
                ok_response("Eco disabled")
            }
            Err(e) => error_response(&format!("Validation error: {}", e)),
        }
    } else {
        apply_eco_enable(&state).await
    }
}

/// Eco-enable logic for the independent `set_eco` toggle. Captures the
/// discharge schedule backup, clears HR59, zeroes discharge slot registers,
/// and writes HR27=1. Mirrors the Eco path in `set_mode` (which uses
/// `SetEcoMode { soc_reserve }` instead, adding an SOC-reserve write), but
/// does not touch the SOC reserve — toggling Eco independently must not
/// reset the user's configured reserve.
///
/// Order matters: HR59 is cleared *before* the slot registers so the
/// inverter never briefly sees an active export schedule with live slots
/// while being returned to Eco. Slots are then zeroed because Gen3 firmware
/// re-asserts `enable_discharge` (HR59=1) whenever any discharge slot is
/// non-zero — without clearing them, Eco does not stick (issue #248).
async fn apply_eco_enable(state: &Arc<AppState>) -> (StatusCode, Json<Value>) {
    // Serialise against every other Timed Export mutation so the schedule
    // cannot be re-enabled (or have its slots edited) between our disable
    // persistence and the queued Eco writes.
    let _timed_export_action = state.timed_export_action_lock.lock().await;
    // The frontend expects enabling Eco to disable the managed Timed
    // Export schedule (issue #289 ownership model): Eco is the baseline the
    // user just chose, so the poll loop must stop re-entering export
    // windows at the next boundary. The *desired* slots stay persisted so a
    // later Timed Export enable restores the schedule directly.
    {
        let slots = state.timed_export_config.lock().await.slots.clone();
        // Durable stop/exit-pending marker (CODE_REVIEW.md BLOCKER), moved in
        // the SAME settings transaction as the disable: if the process dies
        // before the Eco batch lands, the restart resumes the exit.
        if let Err(error) =
            persist_timed_export_schedule_with_stop_pending(state, false, slots, true).await
        {
            return error_response(&format!(
                "Could not save the Timed Export schedule: {error}"
            ));
        }
        *state.timed_export_state.lock().await =
            crate::inverter::state_machines::TimedExportState::Off;
    }

    let captured_backup = capture_discharge_schedule_backup(state).await;

    let mut writes = Vec::new();
    // Clear HR59 (disable discharge) before anything else.
    match (ControlCommand::SetEnableDischarge { enabled: false }).encode() {
        Ok(mut w) => writes.append(&mut w),
        Err(e) => return error_response(&format!("Validation error: {}", e)),
    }
    // Zero all discharge slot registers (Gen3 firmware safety — see above).
    let device_type = latest_device_type(state).await;
    writes.extend(clear_discharge_slot_writes(device_type));
    // Finally restore self-consumption (HR27=1).
    match (ControlCommand::SetBatteryPowerMode { mode: 1 }).encode() {
        Ok(mut w) => writes.append(&mut w),
        Err(e) => return error_response(&format!("Validation error: {}", e)),
    }

    tracing::info!("SetEco encoded: {:?}", writes);
    queue_owned_writes(state, writes, DischargeControlOwner::ManualMode).await;
    ok_response_with_backup("Eco enabled", captured_backup.as_deref())
}

/// POST /api/control/timed-charge — toggle scheduled charge (HR96)
/// independently of Eco / export / pause controls.
///
/// Body: `{ "enabled": true }`.
pub async fn set_timed_charge(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let enabled = body["enabled"].as_bool().unwrap_or(true);
    let cmd = ControlCommand::SetEnableCharge { enabled };
    match cmd.encode() {
        Ok(writes) => {
            tracing::info!("SetTimedCharge encoded: {:?}", writes);
            // HR96 controls charging only; it is independent of the shared
            // discharge-control register domain and must not be deferred by
            // a ManualMode/Timed Export owner.
            queue_writes(&state, writes).await;
            ok_response(if enabled {
                "Timed Charge enabled"
            } else {
                "Timed Charge disabled"
            })
        }
        Err(e) => error_response(&format!("Validation error: {}", e)),
    }
}

/// GET /api/timed-export — the HEM-managed Timed Export schedule state
/// (issue #289).
///
/// Returns the persisted desired schedule (enabled flag + slots), the
/// boundary state-machine state, whether the device has been
/// classified as HR59 re-arming firmware, and the backend's own window
/// evaluation (inverter-local minute-of-day, in-window flag, HR318 pause
/// gate) so the UI does not have to re-derive it from a browser clock that
/// may disagree with the inverter.
///
/// Lock discipline: each mutex is acquired under its own short-lived guard
/// and released before the next is taken. Holding the config and state
/// guards simultaneously (in the reverse order from the poll loop) is what
/// deadlocked this endpoint against the poll loop previously.
pub async fn get_timed_export(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let config = state.timed_export_config.lock().await.clone();
    let machine_state = state.timed_export_state.lock().await.clone();
    let snapshot = state.latest_snapshot.lock().await.clone();

    // Evaluate the window on the inverter's clock when available; expose the
    // fallback so the UI can flag degraded timing accuracy.
    let (inverter_minute, timing_fallback) = match snapshot.as_ref() {
        Some(snap) => match crate::inverter::state_machines::inverter_minute_of_day(snap) {
            Some(minute) => (Some(minute), false),
            None => (Some(host_minute_of_day()), true),
        },
        None => (None, false),
    };
    let (in_window, hr318_blocking) = match (snapshot.as_ref(), inverter_minute) {
        (Some(snap), Some(minute)) => (
            crate::inverter::state_machines::export_window_contains(&config.slots, minute),
            crate::inverter::state_machines::hr318_blocks_discharge(snap, minute),
        ),
        _ => (false, false),
    };

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "data": {
                "schedule_enabled": config.schedule_enabled,
                "slots": config.slots,
                "machine_state": machine_state,
                // Kept for backwards compatibility with older frontends:
                // the Debug-formatted state string.
                "state": format!("{:?}", machine_state),
                "device_rearm_confirmed": config.device_rearm_confirmed,
                "inverter_minute_of_day": inverter_minute,
                "in_window": in_window,
                "hr318_blocking": hr318_blocking,
                "timing_fallback": timing_fallback,
            }
        })),
    )
}

/// Host-local minute-of-day (fallback when the inverter clock is absent).
fn host_minute_of_day() -> u16 {
    use chrono::Timelike as _;
    let now = chrono::Local::now();
    now.hour() as u16 * 60 + now.minute() as u16
}

/// Persist a Timed Export schedule and optionally consume the legacy slot
/// backup in one settings transaction. The backup must be cleared before an
/// in-window schedule is armed: otherwise a later backup-clear failure would
/// return an error after the inverter had already started exporting.
///
/// `stop_pending` moves the durable stop/exit-pending marker in the SAME
/// transaction (CODE_REVIEW.md follow-up): the marker must never lag the
/// schedule change it describes. `Some(true)` belongs to paths that disable
/// the schedule while their disarm writes are still outstanding (a crash
/// then restores the reconciler into `Exiting` at startup); `Some(false)`
/// belongs to commits that retire a pending exit (an explicit re-enable);
/// `None` leaves the marker exactly as it is.
async fn persist_timed_export_schedule_with_backup_clear(
    state: &Arc<AppState>,
    schedule_enabled: bool,
    slots: Vec<crate::inverter::model::ScheduleSlot>,
    clear_backup: bool,
    stop_pending: Option<bool>,
) -> Result<(), String> {
    crate::settings::Settings::update(|s| {
        s.timed_export_schedule_enabled = schedule_enabled;
        s.timed_export_slots = slots.clone();
        if clear_backup {
            s.discharge_slots_backup = None;
        }
        if let Some(pending) = stop_pending {
            s.timed_export_stop_pending = pending;
        }
    })
    .map_err(|error| error.to_string())?;
    if let Some(pending) = stop_pending {
        state
            .timed_export_stop_pending
            .store(pending, std::sync::atomic::Ordering::Release);
    }
    let mut config = state.timed_export_config.lock().await;
    config.schedule_enabled = schedule_enabled;
    config.slots = slots;
    drop(config);
    crate::inverter::poll::reset_timed_export_rearm_detector(state).await;
    Ok(())
}

/// Persist a schedule change together with the durable stop/exit-pending
/// marker in a single settings transaction — the disable and enable paths'
/// entry point (see [`persist_timed_export_schedule_with_backup_clear`]).
async fn persist_timed_export_schedule_with_stop_pending(
    state: &Arc<AppState>,
    schedule_enabled: bool,
    slots: Vec<crate::inverter::model::ScheduleSlot>,
    stop_pending: bool,
) -> Result<(), String> {
    persist_timed_export_schedule_with_backup_clear(
        state,
        schedule_enabled,
        slots,
        false,
        Some(stop_pending),
    )
    .await
}

/// CODE_REVIEW.md BLOCKER: after a partially-applied slot mutation, drive
/// the physical discharge slot registers back to the prior physical
/// schedule with an awaited compensating batch.
///
/// A fail-fast rejection mid-batch leaves the earlier registers of the
/// batch physically written — e.g. an accepted slot start with a rejected
/// slot end turns a short window into an overnight one — so rolling back
/// only the persisted desired state is not enough. If the inverter rejects
/// the compensation too, the physical schedule can no longer be reasoned
/// about: disarm to the Eco baseline (ManualMode owner, so a pause cannot
/// starve it) and park the machine in a terminal `Error` so the UI says
/// the inverter's schedule may not match the saved one, instead of
/// quietly reconciling against corrupted registers.
///
/// Returns `Err(description)` when the physical registers could not be
/// restored; `Ok(())` when the compensating batch was accepted (or there
/// was nothing to restore).
async fn compensate_discharge_slots(
    state: &Arc<AppState>,
    device_type: DeviceType,
    prior_slots: &[crate::inverter::model::ScheduleSlot],
    slot: u8,
) -> Result<(), String> {
    let writes = crate::inverter::state_machines::build_timed_export_slot_compensation_writes(
        device_type,
        prior_slots,
    );
    let count = writes.len();
    tracing::warn!(
        "Timed Export slot {slot}: queueing {count} compensation write(s) to restore the prior physical schedule"
    );
    let rx = queue_writes_transactional(state, writes).await;
    if await_required_batch_write_outcome(rx, count).await.is_ok() {
        return Ok(());
    }
    tracing::error!(
        "Timed Export slot {slot}: compensation writes were rejected — the physical slot \
         registers could not be restored"
    );
    // Best-effort disarm so a corrupted physical window cannot keep
    // exporting against unexpected times.
    let rx = queue_owned_writes_transactional(
        state,
        crate::inverter::state_machines::build_timed_export_disable_writes(device_type),
        DischargeControlOwner::ManualMode,
    )
    .await;
    if let Err(disarm_msg) = await_required_write_outcome(rx).await {
        tracing::error!(
            "Timed Export slot {slot}: disarm after failed compensation also failed: {disarm_msg}"
        );
    }
    let reason = format!(
        "Discharge slot {slot} could not be restored after a failed save — the inverter's \
         physical schedule may not match the saved schedule. Disarm was attempted; re-save \
         the schedule to recover."
    );
    *state.timed_export_state.lock().await =
        crate::inverter::state_machines::TimedExportState::Error { reason };
    Err("the inverter also rejected the compensation writes".to_string())
}

/// Put the boundary reconciler back into a live state after the API changes
/// the desired schedule. In particular, `Error` is terminal inside
/// `check_timed_export`, so leaving it untouched would make a corrected slot
/// edit inert forever.
async fn reset_timed_export_machine_after_schedule_change(
    state: &Arc<AppState>,
    schedule_enabled: bool,
    in_window: bool,
) {
    let next = if !schedule_enabled {
        crate::inverter::state_machines::TimedExportState::Off
    } else if in_window {
        crate::inverter::state_machines::TimedExportState::Entering {
            polls_waiting: 0,
            retries: 0,
        }
    } else {
        crate::inverter::state_machines::TimedExportState::Configured
    };
    *state.timed_export_state.lock().await = next;
}

/// Whether "now" (inverter-local when the clock registers are readable)
/// falls inside an enabled desired slot and HR318 is not blocking discharge
/// — the condition for entering Timed Export immediately rather than
/// leaving the boundary transition to the poll-loop state machine
/// (issue #289).
async fn should_arm_timed_export_now(
    state: &Arc<AppState>,
    desired_slots: &[crate::inverter::model::ScheduleSlot],
) -> bool {
    let snapshot = state.latest_snapshot.lock().await;
    let Some(snapshot) = snapshot.as_ref() else {
        return false;
    };
    let minute_of_day = crate::inverter::state_machines::inverter_minute_of_day(snapshot)
        .unwrap_or_else(host_minute_of_day);
    let in_window =
        crate::inverter::state_machines::export_window_contains(desired_slots, minute_of_day);
    if !in_window {
        return false;
    }
    !crate::inverter::state_machines::hr318_blocks_discharge(snapshot, minute_of_day)
}

/// POST /api/control/timed-export — toggle scheduled DC export (HR59)
/// independently of the Eco switch.
///
/// Body: `{ "enabled": true }`. Issue #289 semantics: enabling arms the
/// **HEM-managed schedule** — the desired slots are persisted and the
/// poll-loop boundary state machine enters maximum-power export
/// (HR27=0 + the model-routed discharge-enable register) only inside the
/// configured windows, keeping Eco as the baseline outside them.
///
/// Schedule source of truth (code-review finding): the persisted desired
/// slots win. Enable prefers, in order: the persisted `timed_export_slots`,
/// the legacy issue-#137 discharge-slot backup, then the live snapshot
/// slots. Editing under the re-arm fallback (physical registers cleared)
/// can therefore never lose the schedule or be rejected with "configure at
/// least one slot" while a valid schedule is persisted.
///
/// Arming is ordered and fail-fast: slot/target and optional reserve writes
/// complete first, the desired schedule is persisted, and the export arm
/// (HR27=0, then enable=1) is the final fallible phase. A rejected setup write
/// therefore cannot leave maximum-power export armed, while an arm failure
/// retains a reconcilable desired schedule for automatic retry.
pub async fn set_timed_export(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let enabled = body["enabled"].as_bool().unwrap_or(true);
    // Serialise the whole enable or disable against every other Timed
    // Export mutation (slot edits, backup-restore, test reset) so the
    // poll loop cannot interleave between a clone of the desired schedule
    // and the persistence of the post-write schedule. Held until the
    // response is built — this is a request-scoped lock, not a global
    // one, so a long-running poll cycle is never blocked.
    let _timed_export_action = state.timed_export_action_lock.lock().await;

    if enabled {
        // An explicit Arm supersedes any pending stop/exit — but the marker
        // is retired at COMMIT time (the same settings transaction that
        // persists the enabled schedule), not up-front: if the enable fails
        // while the registers were armed by a failed stop, the marker must
        // still say an exit is pending, or a crash would boot the machine
        // as `Off` and suppress every repair.
        // Without a snapshot the device type — and therefore the register
        // layout the restore writes target — is unknown. Refuse rather than
        // guess (a wrong guess could restore the backup into the wrong
        // registers on non-Gen2Hybrid models).
        let device_type = {
            let snapshot = state.latest_snapshot.lock().await;
            match snapshot.as_ref() {
                Some(snapshot) => snapshot.device_type,
                None => {
                    return (
                        StatusCode::CONFLICT,
                        Json(json!({
                            "ok": false,
                            "error": "No inverter data yet — wait for the first poll before enabling Timed Export"
                        })),
                    );
                }
            }
        };

        // ---- Resolve the desired schedule (persisted slots first) ----
        let mut restoring_backup = false;
        let mut desired_slots: Vec<crate::inverter::model::ScheduleSlot> = {
            let config = state.timed_export_config.lock().await;
            config.slots.clone()
        };
        if !desired_slots
            .iter()
            .any(crate::inverter::model::ScheduleSlot::is_configured)
        {
            // No persisted desired schedule: fall back to the live snapshot
            // slots (the pre-#289 source), then the legacy issue-#137 backup.
            let live_slots: Vec<crate::inverter::model::ScheduleSlot> = {
                let snapshot = state.latest_snapshot.lock().await;
                snapshot
                    .as_ref()
                    .map(|s| s.discharge_slots.to_vec())
                    .unwrap_or_default()
            };
            if live_slots
                .iter()
                .any(crate::inverter::model::ScheduleSlot::is_configured)
            {
                desired_slots = live_slots;
            } else {
                let settings = crate::settings::Settings::load();
                match settings.discharge_slots_backup.clone() {
                    Some(backup) => {
                        if restore_discharge_slot_writes(device_type, &backup).is_none() {
                            return (
                                StatusCode::CONFLICT,
                                Json(json!({
                                    "ok": false,
                                    "error": "Configure at least one discharge slot before enabling Timed Export"
                                })),
                            );
                        }
                        restoring_backup = true;
                        // The backup becomes the desired schedule: mirror it
                        // into the persisted HEM schedule so the boundary
                        // state machine owns it from here on.
                        desired_slots = backup.into_iter().map(Into::into).collect();
                    }
                    None => {
                        return (
                            StatusCode::CONFLICT,
                            Json(json!({
                                "ok": false,
                                "error": "Configure at least one discharge slot before enabling Timed Export"
                            })),
                        );
                    }
                }
            }
        }

        // The desired schedule is only committed to disk after every
        // required physical write succeeded (code-review finding:
        // persisting `schedule_enabled=true` before restore writes are
        // confirmed left the schedule enabled with no physical slot).
        // ---- Phase 2: arm export when "now" is inside an enabled window ----
        // Issue #289: maximum-power writes (HR27=0 then the model-routed
        // enable register =1) are only queued when "now" is inside an
        // enabled window and HR318 is not blocking; otherwise Eco stays the
        // baseline and the poll-loop state machine enters at the window
        // boundary. The enable register is model-routed: HR1122 on
        // three-phase schedule devices, HR59 elsewhere.
        //
        // Normal firmware keeps desired slots populated, including for a
        // future window. Re-arm firmware is the inverse: physical slots must
        // stay clear outside the window and are restored only immediately
        // before arming, otherwise firmware reasserts discharge by itself.
        let arm_now = should_arm_timed_export_now(&state, &desired_slots).await;
        let rearm_confirmed = device_rearm_confirmed(&state).await;
        let slot_writes = crate::inverter::state_machines::build_timed_export_slot_restore_writes(
            device_type,
            &desired_slots,
        );
        let reserve_writes = if let Some(soc) = body["soc_reserve"].as_u64() {
            match reserve_writes_for_device(device_type, soc as u16) {
                Ok(writes) => writes,
                Err(error) => return error_response(&format!("Validation error: {error}")),
            }
        } else {
            Vec::new()
        };
        if (!rearm_confirmed || arm_now) && !slot_writes.is_empty() {
            let write_count = slot_writes.len();
            let rx = queue_writes_transactional(&state, slot_writes).await;
            if let Err(msg) = await_required_batch_write_outcome(rx, write_count).await {
                tracing::warn!("Timed Export slot restore rejected: {msg}");
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({
                        "ok": false,
                        "error": format!(
                            "Timed Export could not be enabled — restoring the saved discharge schedule failed ({msg}). Please try again."
                        )
                    })),
                );
            }
        }
        if !reserve_writes.is_empty() {
            // The reserve is part of setup, not a post-arm auxiliary write.
            // Complete it before persistence and before maximum-power export
            // can be armed, so a rejected reserve can never leave the inverter
            // exporting while this endpoint reports failure.
            tracing::info!(
                "SetTimedExport reserve writes encoded: {:?}",
                reserve_writes
            );
            let rx = queue_owned_writes_transactional(
                &state,
                reserve_writes,
                DischargeControlOwner::TimedExport,
            )
            .await;
            if let Err(msg) = await_required_write_outcome(rx).await {
                tracing::warn!("Timed Export reserve write rejected: {msg}");
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({
                        "ok": false,
                        "error": format!(
                            "Timed Export could not be enabled — the battery reserve write failed ({msg})."
                        )
                    })),
                );
            }
        }

        if !arm_now {
            let eco_confirmed =
                state
                    .latest_snapshot
                    .lock()
                    .await
                    .as_ref()
                    .is_some_and(|snapshot| {
                        snapshot.battery_power_mode == 1 && !snapshot.enable_discharge
                    });
            if !eco_confirmed {
                let eco_writes =
                    crate::inverter::state_machines::build_timed_export_disable_writes(device_type);
                // User-issued Eco restore (the user just enabled a future
                // window): queue as ManualMode so a register-derived
                // ExplicitPause cannot starve it (see the stop path).
                let rx = queue_owned_writes_transactional(
                    &state,
                    eco_writes,
                    DischargeControlOwner::ManualMode,
                )
                .await;
                if let Err(msg) = await_required_write_outcome(rx).await {
                    tracing::warn!("Timed Export Eco baseline restore rejected: {msg}");
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({
                            "ok": false,
                            "error": format!(
                                "Timed Export could not be enabled — restoring the Eco baseline failed ({msg})."
                            )
                        })),
                    );
                }
            }
        }

        // This is the last fallible settings operation on the in-window path.
        // Once it succeeds the export arm is the final action, so no later
        // reserve or persistence error can strand maximum-power export behind
        // an error response. Consume a legacy backup and retire the
        // stop/exit-pending marker in the same transaction.
        if let Err(error) = persist_timed_export_schedule_with_backup_clear(
            &state,
            true,
            desired_slots,
            restoring_backup,
            Some(false),
        )
        .await
        {
            return error_response(&format!(
                "Could not save the Timed Export schedule: {error}"
            ));
        }
        reset_timed_export_machine_after_schedule_change(&state, true, arm_now).await;

        if arm_now {
            let mut arm_writes = vec![RegisterWrite {
                address: crate::modbus::registers::HR_BATTERY_POWER_MODE,
                value: 0,
            }];
            arm_writes.push(
                crate::inverter::state_machines::timed_export_discharge_enable_write(
                    device_type,
                    true,
                ),
            );
            tracing::info!("SetTimedExport arm encoded: {:?}", arm_writes);
            let rx = queue_owned_writes_transactional(
                &state,
                arm_writes,
                DischargeControlOwner::TimedExport,
            )
            .await;
            if let Err(msg) = await_required_write_outcome(rx).await {
                tracing::warn!("Timed Export arm rejected: {msg}");
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({
                        "ok": false,
                        "error": format!(
                            "Timed Export could not be enabled — the export arm failed ({msg}). Arming will be retried automatically."
                        )
                    })),
                );
            }
        }
        ok_response("Timed Export enabled")
    } else {
        // Stopping Timed Export must return the inverter to self-consumption
        // (HR27=1), not just clear the schedule flag — and must disarm the
        // HEM-managed schedule so the poll loop stops re-entering windows.
        // When the firmware is classified as HR59 re-arming (issue #289),
        // the physical slot registers must also be cleared BEFORE the
        // disarm — otherwise the firmware re-asserts HR59=1 and Stop can
        // never take effect. The desired schedule stays persisted (with
        // schedule_enabled=false) so a later enable restores it.
        let device_type = latest_device_type(&state).await;
        let rearm_confirmed = state
            .timed_export_config
            .lock()
            .await
            .device_rearm_confirmed;
        // Transactional stop (code-review finding 1): enable is fully
        // transactional, so Stop must not be fire-and-forget. A rejected or
        // indefinitely-deferred disarm otherwise leaves the inverter
        // exporting on its own armed schedule while the response — and the
        // UI — claim success. The desired Off state is persisted first (so
        // retry converges), then both phases are awaited and a failure
        // returns an actionable error.
        // The action lock is acquired at the top of `set_timed_export`,
        // so do not re-acquire it here: `tokio::sync::Mutex` is not
        // re-entrant and the handler would deadlock itself (issue observed
        // with --nocapture eprintln probes showing the disable branch
        // never reached the persist step).
        let slot_clear_writes = if rearm_confirmed {
            crate::inverter::state_machines::build_timed_export_slot_clear_writes(device_type)
        } else {
            Vec::new()
        };
        let disarm_writes =
            crate::inverter::state_machines::build_timed_export_disable_writes(device_type);
        let slots = state.timed_export_config.lock().await.slots.clone();
        // Durable stop/exit-pending marker, moved in the SAME settings
        // transaction as the disable (CODE_REVIEW.md BLOCKER): a crash
        // before the disarm completes restores the reconciler into
        // `Exiting` on restart, and the marker can never be observed
        // disagreeing with the schedule it belongs to.
        if let Err(error) =
            persist_timed_export_schedule_with_stop_pending(&state, false, slots, true).await
        {
            return error_response(&format!(
                "Could not save the Timed Export schedule: {error}"
            ));
        }
        // The machine goes straight to Off; the reconciler's Off-repair
        // covers a failed disarm on later polls.
        *state.timed_export_state.lock().await =
            crate::inverter::state_machines::TimedExportState::Off;

        if !slot_clear_writes.is_empty() {
            tracing::info!("SetTimedExport slot-clear encoded: {:?}", slot_clear_writes);
            let rx = queue_writes_transactional(&state, slot_clear_writes.clone()).await;
            if let Err(msg) = await_required_write_outcome_with_timeout(
                rx,
                batch_completion_timeout(slot_clear_writes.len()),
            )
            .await
            {
                tracing::warn!("Timed Export stop failed clearing slots: {msg}");
                // CODE_REVIEW.md finding 2: the writes failed after the
                // schedule was persisted disabled, so the reconciler must
                // keep repairing. Arm the machine into Exiting — the
                // physical slots are residue of our own incomplete stop,
                // not an externally-owned schedule, so the disabled-schedule
                // quiet branch must not swallow the retry.
                *state.timed_export_state.lock().await =
                    crate::inverter::state_machines::TimedExportState::Exiting {
                        polls_waiting: 0,
                        retries: 0,
                    };
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({
                        "ok": false,
                        "error": format!(
                            "Timed Export could not be stopped yet ({msg}). \
                             The schedule is disabled — retry the stop to also disarm the inverter."
                        )
                    })),
                );
            }
        }
        // Always issue the disarm writes — the previous "skip if Eco
        // already confirmed" short-circuit could race a poll-cycle
        // transition that re-arms HR27=0/enable=1 immediately after this
        // endpoint returns: by the time Stop returned 200 OK the inverter
        // would already be exporting again. The disarm is idempotent (HR27
        // and the model-routed enable register are both safe to rewrite)
        // and the failure path is unchanged.
        tracing::info!("SetTimedExport encoded: {:?}", disarm_writes);
        // A user-issued stop is a manual baseline selection (HR27=1 is
        // the manual Eco write), NOT an automation step: it must not
        // defer behind a register-derived ExplicitPause owner. The
        // admission matrix (poll.rs) guarantees ManualMode batches
        // drain within one poll cycle against every derived state.
        // Entering export (the arm batch) keeps the TimedExport owner
        // so an explicit pause still blocks it (issue #289).
        let rx = queue_owned_writes_transactional(
            &state,
            disarm_writes,
            DischargeControlOwner::ManualMode,
        )
        .await;
        if let Err(msg) = await_required_write_outcome(rx).await {
            tracing::warn!("Timed Export stop failed disarming: {msg}");
            // CODE_REVIEW.md finding 2: the disarm failed after the
            // schedule was persisted disabled — arm the reconciler into
            // Exiting so the poll loop keeps retrying the disarm even
            // though the physical slots remain configured.
            *state.timed_export_state.lock().await =
                crate::inverter::state_machines::TimedExportState::Exiting {
                    polls_waiting: 0,
                    retries: 0,
                };
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "ok": false,
                    "error": format!(
                        "Timed Export could not be stopped yet ({msg}). \
                         The schedule is disabled — retry the stop to also disarm the inverter."
                    )
                })),
            );
        }
        ok_response("Timed Export disabled")
    }
}

/// Whether the device has been classified as HR59 re-arming firmware
/// (slots must be restored before arming).
async fn device_rearm_confirmed(state: &Arc<AppState>) -> bool {
    state
        .timed_export_config
        .lock()
        .await
        .device_rearm_confirmed
}

/// POST /api/test/reset — E2E-harness-only state reset.
///
/// Registered on every router but answers 404 unless the process was started
/// with `--e2e-admin`, so production installs expose nothing.
///
/// The Playwright suite shares one backend process per spec file and one
/// mock Modbus server across the whole run. Resetting the mock's registers
/// cannot reach backend-owned state: the persisted Timed Export schedule
/// (enabled flag, desired slots, learned HR59 re-arm fallback, legacy slot
/// backup), the in-memory config mirror, the boundary state machine, and the
/// captured Force Charge/Discharge restore snapshots. Without this reset a
/// later test's enable silently reuses the previous test's schedule slots,
/// and the reconciler re-arms export into a test that just reset the
/// registers — the failure class behind the full-suite E2E order dependence.
pub async fn test_reset(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    if !state.e2e_admin.load(std::sync::atomic::Ordering::Acquire) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "error": "Not found" })),
        );
    }

    // Hold the Timed Export action lock for the whole reset so an in-flight
    // mutation cannot reapply a schedule (or rearm the detector) between
    // our settings persistence and the in-memory mirror update — the
    // resetting test is the canonical source of truth until the next
    // mutation completes.
    let _timed_export_action = state.timed_export_action_lock.lock().await;

    if let Err(error) = crate::settings::Settings::update(|s| {
        s.timed_export_schedule_enabled = false;
        s.timed_export_slots = Vec::new();
        s.timed_export_slots_require_clear = false;
        s.discharge_slots_backup = None;
        s.timed_export_stop_pending = false;
    }) {
        return error_response(&format!("Could not reset settings: {error}"));
    }
    state
        .timed_export_stop_pending
        .store(false, std::sync::atomic::Ordering::Release);

    {
        let mut config = state.timed_export_config.lock().await;
        config.schedule_enabled = false;
        config.slots.clear();
        config.device_rearm_confirmed = false;
    }
    *state.timed_export_state.lock().await = crate::inverter::state_machines::TimedExportState::Off;
    crate::inverter::poll::reset_timed_export_rearm_detector(&state).await;
    state.force_charge_revert.lock().await.take();
    state.force_discharge_revert.lock().await.take();
    // Drop any still-queued register writes (e.g. a previous test's mode-switch
    // slot-clear burst). The poll loop drains the queue BEFORE its next read,
    // so a multi-second burst would otherwise starve the harness of a fresh
    // snapshot for tens of seconds after the reset. An in-flight batch (already
    // taken by the poll loop) is unaffected; awaiting endpoints of dropped
    // batches see the normal dropped-channel cancellation path.
    state.pending_writes.lock().await.clear();

    tracing::info!("E2E harness reset applied (test-only endpoint)");
    (StatusCode::OK, Json(json!({ "ok": true })))
}

/// POST /api/control/timed-discharge — configure/toggle the portal-style
/// single-slot Timed Discharge mechanism.
///
/// GivEnergy Cloud implements this with the battery pause registers: HR318=2
/// (`Pause Discharge`) and HR319/320 set to the inverse of the desired demand
/// window. For a user slot 03:00-04:00 we write a pause window 04:00-03:00,
/// so the battery only covers demand inside the visible slot.
pub async fn set_timed_discharge(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    // The portal-style Timed Discharge feature is implemented with the
    // battery pause registers HR318 (battery_pause_mode) and HR319/320
    // (battery_pause_slot). Two paths reach them:
    //  - AC-three-phase / residential All-in-One models expose confirmed
    //    writable HR319/320 Timed Discharge slots. AC-coupled models expose
    //    the AC-config block but field logs show those slot writes fail.
    //  - Gen3 Hybrid (ARM fw >= 312) reaches them via a targeted 3-register
    //    probe — the full HR 300-359 block times out on this family (#162).
    // On every other family the registers are absent: the write times out or
    // is silently dropped and `battery_pause_mode` stays 0 forever, so the
    // toggle could never reflect an enabled state. Refuse up front with a
    // clear error so the UI can hide the control.
    let device_type = latest_device_type(&state).await;
    let arm_fw = latest_arm_fw(&state).await;
    if !device_type.supports_timed_discharge(arm_fw) {
        return error_response(&format!(
            "Timed Discharge is not supported on {} inverters",
            device_type.display_name()
        ));
    }

    let enabled = body["enabled"].as_bool().unwrap_or(true);
    let mut writes = Vec::new();

    if enabled {
        let start_hour = body["start_hour"].as_u64().unwrap_or(0) as u8;
        let start_minute = body["start_minute"].as_u64().unwrap_or(0) as u8;
        let end_hour = body["end_hour"].as_u64().unwrap_or(0) as u8;
        let end_minute = body["end_minute"].as_u64().unwrap_or(0) as u8;
        if start_hour > 23 || end_hour > 23 {
            return error_response("Hour must be 0-23");
        }
        if start_minute > 59 || end_minute > 59 {
            return error_response("Minute must be 0-59");
        }
        let pause_start = encode_hhmm(end_hour, end_minute);
        let pause_end = encode_hhmm(start_hour, start_minute);
        for cmd in [
            ControlCommand::SetPauseSlot {
                start: pause_start,
                end: pause_end,
            },
            ControlCommand::SetPauseMode { mode: 2 },
        ] {
            match cmd.encode() {
                Ok(mut w) => writes.append(&mut w),
                Err(e) => return error_response(&format!("Validation error: {}", e)),
            }
        }
    } else {
        for cmd in [
            ControlCommand::SetPauseMode { mode: 0 },
            ControlCommand::SetPauseSlot { start: 0, end: 0 },
        ] {
            match cmd.encode() {
                Ok(mut w) => writes.append(&mut w),
                Err(e) => return error_response(&format!("Validation error: {}", e)),
            }
        }
    }

    tracing::info!("SetTimedDischarge encoded: {:?}", writes);
    queue_owned_writes(&state, writes, DischargeControlOwner::ExplicitPause).await;
    ok_response(if enabled {
        "Timed Discharge enabled"
    } else {
        "Timed Discharge disabled"
    })
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

    // Duration-based forecast plans use time as the control variable: the
    // inverter must charge at the requested rate for the slot duration,
    // rather than stopping early at an SOC target. The percentage is in the
    // same display scale as the Control page (100 = maximum).
    let requested_charge_rate_pct = match body.get("charge_rate_percent") {
        None => None,
        Some(value) => match value.as_u64() {
            Some(rate) if rate <= 100 => Some(rate as u16),
            _ => return error_response("charge_rate_percent must be 0-100"),
        },
    };
    if enabled && requested_charge_rate_pct.is_some() {
        let settings = crate::settings::Settings::load();
        if settings.adaptive_charge_enabled || settings.adaptive_charge_saved_limit.is_some() {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "ok": false,
                    "error": "Charge rate is controlled by Adaptive Charge",
                })),
            );
        }
    }

    let start_hour = body["start_hour"].as_u64().unwrap_or(0) as u8;
    let start_minute = body["start_minute"].as_u64().unwrap_or(0) as u8;
    let end_hour = body["end_hour"].as_u64().unwrap_or(0) as u8;
    let end_minute = body["end_minute"].as_u64().unwrap_or(0) as u8;
    // Target SOC semantics: an explicit value always wins. When omitted, do
    // NOT silently default to 100 ("no limit") — that disarms any armed
    // charge target via the `target_soc >= 100` branch below (HR 20 cleared,
    // HR 116 inert), so an automated re-post without `target_soc` would turn
    // an armed 99% target into "charge to full". Instead preserve the
    // currently armed target from the latest snapshot. Only 5-99 is
    // unambiguous: the decoder clamps HR 116 to [4,100] with 4 meaning
    // "unset/minimum" (raw 0 → 4), and a decoded 100 already means no-limit,
    // which is the old default anyway.
    let target_soc = match body["target_soc"].as_u64() {
        Some(explicit) => explicit.clamp(4, 100) as u8,
        None => {
            let armed = state
                .latest_snapshot
                .lock()
                .await
                .as_ref()
                .map(|s| s.target_soc)
                .filter(|t| (5..=99).contains(t))
                .unwrap_or(100);
            if armed != 100 {
                tracing::info!(
                    armed,
                    "Charge slot POST omitted target_soc - preserving armed target"
                );
            }
            armed
        }
    };

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

    match build_charge_slot_writes(
        device_type,
        slot,
        enabled,
        start,
        end,
        target_soc,
        requested_charge_rate_pct,
    ) {
        Ok(writes) => {
            tracing::info!("SetChargeSlot {} encoded: {:?}", slot, writes);
            queue_writes(&state, writes).await;
            ok_response(&format!("Charge slot {} configured", slot))
        }
        Err(e) => error_response(&e),
    }
}

/// Build the register writes for a charge-slot save: the slot window
/// itself plus, when enabling, the charge-target / enable-charge /
/// rate-limit extras the Control endpoints have always applied. Shared by
/// `POST /api/control/charge-slot` and the Forecast plan's nightly
/// auto-refresh so the hand-applied and machine-refreshed slots can never
/// diverge in wire shape.
pub(crate) fn build_charge_slot_writes(
    device_type: DeviceType,
    slot: u8,
    enabled: bool,
    start: u16,
    end: u16,
    target_soc: u8,
    requested_charge_rate_pct: Option<u16>,
) -> Result<Vec<RegisterWrite>, String> {
    let mut writes = charge_slot_command_for_device(device_type, slot, enabled, start, end)?
        .encode()
        .map_err(|e| format!("Validation error: {}", e))?;
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
            if let Ok(enable_writes) = (ControlCommand::SetEnableCharge { enabled: true }).encode()
            {
                writes.extend(enable_writes);
            }
        }
        if let Some(rate_pct) = requested_charge_rate_pct {
            let raw_limit = if device_type.uses_direct_charge_limit() {
                rate_pct
            } else {
                (rate_pct as f64 / 2.0).round() as u16
            };
            let rate_command = if device_type.uses_three_phase_schedule_slots() {
                ControlCommand::SetThreePhaseChargeLimit { limit: raw_limit }
            } else if device_type.uses_direct_charge_limit() {
                ControlCommand::SetAcChargeLimit { limit: raw_limit }
            } else {
                ControlCommand::SetChargeLimit { limit: raw_limit }
            };
            match rate_command.encode() {
                Ok(rate_writes) => writes.extend(rate_writes),
                Err(e) => return Err(format!("Validation error: {}", e)),
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

    Ok(writes)
}

/// POST /api/control/discharge-slot — configure a discharge schedule slot.
///
/// Body: `{"slot": 1, "start_hour": 16, "start_minute": 0, "end_hour": 19, "end_minute": 0,
///         "enabled": true}`
///
/// If `enabled` is false, the slot times are set to 0 (per givenergy-modbus reference).
/// Saving an enabled slot also arms Timed Export after the slot and target-SOC
/// writes, matching the Timed Discharge editor's save-and-enable behaviour.
/// Disabling the last configured slot while Timed Export is armed returns the
/// inverter to Eco, so HR59/HR27 cannot be left in the invalid no-window state.
pub async fn set_discharge_slot(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let slot_raw = match body["slot"].as_u64() {
        Some(s) => s,
        None => return error_response("Missing 'slot' field (1-2)"),
    };
    // Serialise against every other Timed Export mutation so two concurrent
    // slot saves cannot both load the desired vector, race through the
    // Modbus write queue, and have the second persistence overwrite the
    // first edit (the lost-update class the in-process concurrent-update
    // settings tests guard against).
    let _timed_export_action = state.timed_export_action_lock.lock().await;
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
    // An omitted `target_soc` must preserve the per-slot discharge floor
    // rather than silently writing the inert 100 ("never discharge")
    // default — same omit-default clobber class as the charge-slot fix.
    // The persisted desired slot is consulted first (source of truth —
    // under the re-arm fallback the physical registers are zero), then
    // the live snapshot. Only a real configured floor (4-99) is preserved;
    // an explicit value always wins.
    let target_soc = match body["target_soc"].as_u64() {
        Some(v) => v as u8,
        None => {
            let desired = state
                .timed_export_config
                .lock()
                .await
                .slots
                .get(slot as usize - 1)
                .cloned();
            let live = state
                .latest_snapshot
                .lock()
                .await
                .as_ref()
                .and_then(|s| s.discharge_slots.get(slot as usize - 1))
                .cloned();
            desired
                .or(live)
                .filter(|slot| slot.enabled)
                .map(|slot| slot.target_soc)
                .filter(|t| (4..100).contains(t))
                .unwrap_or(100)
        }
    };

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
    if enabled && start == end {
        return error_response("Start and end times must differ for an enabled Timed Export slot");
    }

    let should_return_to_eco = if enabled {
        false
    } else {
        // Whether this disable leaves no configured slot anywhere: evaluate
        // against the *desired* schedule after the edit (the persisted slots
        // are the source of truth — under the re-arm fallback the physical
        // registers are intentionally zero and must not mask a configured
        // desired slot), combined with the live armed readback.
        let desired_base: Vec<crate::inverter::model::ScheduleSlot> = {
            let config_slots = state.timed_export_config.lock().await.slots.clone();
            if config_slots
                .iter()
                .any(crate::inverter::model::ScheduleSlot::is_configured)
            {
                config_slots
            } else {
                let snapshot = state.latest_snapshot.lock().await;
                snapshot
                    .as_ref()
                    .map(|s| s.discharge_slots.to_vec())
                    .unwrap_or_default()
            }
        };
        let snapshot = state.latest_snapshot.lock().await;
        snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.enable_discharge && {
                let mut desired_after = desired_base.clone();
                merge_desired_slot(
                    &mut desired_after,
                    slot,
                    crate::inverter::model::ScheduleSlot {
                        enabled,
                        start_hour,
                        start_minute,
                        end_hour,
                        end_minute,
                        target_soc,
                    },
                );
                !desired_after
                    .iter()
                    .any(crate::inverter::model::ScheduleSlot::is_configured)
            }
        })
    };

    let cmd = match discharge_slot_command_for_device(device_type, slot, enabled, start, end) {
        Ok(cmd) => cmd,
        Err(e) => return error_response(&e),
    };

    match cmd.encode() {
        Ok(mut writes) => {
            // Build the desired schedule after this save from the PERSISTED
            // desired slots (issue #289 / code-review finding): rebuilding
            // from the physical snapshot under the re-arm fallback — where
            // the registers are intentionally zero — would silently discard
            // every other configured desired slot. Merge this edit in by
            // its actual slot index. When no desired schedule has been
            // persisted yet (pre-#289 install, or slots configured by
            // another client), seed from the live snapshot instead.
            let mut desired_slots: Vec<crate::inverter::model::ScheduleSlot> = {
                let config_slots = state.timed_export_config.lock().await.slots.clone();
                if config_slots
                    .iter()
                    .any(crate::inverter::model::ScheduleSlot::is_configured)
                {
                    config_slots
                } else {
                    let snapshot = state.latest_snapshot.lock().await;
                    snapshot
                        .as_ref()
                        .map(|s| s.discharge_slots.to_vec())
                        .unwrap_or_default()
                }
            };
            let edited = crate::inverter::model::ScheduleSlot {
                enabled,
                start_hour,
                start_minute,
                end_hour,
                end_minute,
                target_soc,
            };
            merge_desired_slot(&mut desired_slots, slot, edited);

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

            if enabled {
                // Issue #289: two-phase, fail-fast slot-then-arm. The slot
                // and target writes are queued first and awaited; only when
                // the inverter accepts them is the schedule persisted, then
                // the export entry writes (HR27=0 followed by the model-routed
                // enable register) are queued. A rejected slot write therefore
                // can never leave maximum-power export armed without a valid
                // gating window or commit a schedule the inverter rejected.
                //
                // The schedule IS committed before the arm attempt: a
                // rejected or deferred arm write must retain the desired
                // schedule (so the boundary state machine retries it and a
                // manual retry needs no re-entry) while the response still
                // reports an actionable failure rather than claiming
                // physical activation (CODE_REVIEW.md).
                //
                let arm_now = should_arm_timed_export_now(&state, &desired_slots).await;
                let rearm_confirmed = device_rearm_confirmed(&state).await;

                // CODE_REVIEW.md BLOCKER: the last decoded physical slot
                // registers — the target state for the compensating batch if
                // a later write or persistence step fails.
                let prior_physical: Vec<crate::inverter::model::ScheduleSlot> = {
                    let snapshot = state.latest_snapshot.lock().await;
                    snapshot
                        .as_ref()
                        .map(|s| s.discharge_slots.to_vec())
                        .unwrap_or_default()
                };

                // Normal firmware keeps future slots populated. Re-arm
                // firmware must keep every physical slot clear until the
                // window opens, then restore the complete desired schedule
                // immediately before arming.
                let physical_slot_writes = if rearm_confirmed {
                    crate::inverter::state_machines::build_timed_export_slot_restore_writes(
                        device_type,
                        &desired_slots,
                    )
                } else {
                    writes
                };
                if (!rearm_confirmed || arm_now) && !physical_slot_writes.is_empty() {
                    let write_count = physical_slot_writes.len();
                    let rx = queue_writes_transactional(&state, physical_slot_writes).await;
                    if let Err(msg) = await_required_batch_write_outcome(rx, write_count).await {
                        tracing::warn!("Timed Export slot {slot} write failed: {msg}");
                        // The batch was rejected mid-way: earlier registers
                        // are already physically written (e.g. a new slot
                        // start with the old slot end — an unintended
                        // window). Compensate the physical state before
                        // reporting failure; the desired schedule was never
                        // persisted, so nothing else needs rolling back.
                        if compensate_discharge_slots(&state, device_type, &prior_physical, slot)
                            .await
                            .is_ok()
                        {
                            return (
                                StatusCode::BAD_GATEWAY,
                                Json(json!({
                                    "ok": false,
                                    "error": format!(
                                        "Timed Export slot {slot} could not be saved ({msg}). Export was not armed."
                                    )
                                })),
                            );
                        }
                        return (
                            StatusCode::BAD_GATEWAY,
                            Json(json!({
                                "ok": false,
                                "error": format!(
                                    "Timed Export slot {slot} could not be saved ({msg}), and restoring the previous physical schedule failed. Timed Export has been disarmed and flagged an error — re-save the schedule to recover."
                                )
                            })),
                        );
                    }
                }

                // Commit the desired schedule before attempting the arm
                // transition. On arm failure the schedule stays retained for
                // retry — the poll-loop state machine reconciles toward it —
                // and the machine state (exposed via GET /api/timed-export)
                // is what distinguishes pending/armed/active, never this
                // response's success flag alone. The (re)commit also retires
                // any stale stop/exit-pending marker in the same transaction.
                if let Err(error) = persist_timed_export_schedule_with_stop_pending(
                    &state,
                    true,
                    desired_slots.clone(),
                    false,
                )
                .await
                {
                    tracing::error!(
                        "Timed Export slot {slot} save could not be persisted after the physical writes landed: {error}"
                    );
                    // The physical registers already hold the new slot but
                    // HEM cannot record it as desired — compensate so the
                    // inverter matches the (unchanged) saved schedule.
                    if compensate_discharge_slots(&state, device_type, &prior_physical, slot)
                        .await
                        .is_ok()
                    {
                        return error_response(&format!(
                            "Could not save the Timed Export schedule: {error}"
                        ));
                    }
                    return error_response(&format!(
                        "Could not save the Timed Export schedule: {error}. Restoring the previous physical schedule also failed — Timed Export has been disarmed and flagged an error; re-save the schedule to recover."
                    ));
                }
                reset_timed_export_machine_after_schedule_change(&state, true, arm_now).await;

                if arm_now {
                    let mut arm_writes = vec![RegisterWrite {
                        address: crate::modbus::registers::HR_BATTERY_POWER_MODE,
                        value: 0,
                    }];
                    arm_writes.push(
                        crate::inverter::state_machines::timed_export_discharge_enable_write(
                            device_type,
                            true,
                        ),
                    );
                    tracing::info!("SetDischargeSlot {} arm encoded: {:?}", slot, arm_writes);
                    let rx = queue_owned_writes_transactional(
                        &state,
                        arm_writes,
                        DischargeControlOwner::TimedExport,
                    )
                    .await;
                    if let Err(msg) = await_required_write_outcome(rx).await {
                        tracing::warn!("Timed Export arm failed after saving slot {slot}: {msg}");
                        return (
                            StatusCode::BAD_GATEWAY,
                            Json(json!({
                                "ok": false,
                                "error": format!(
                                    "Discharge slot {slot} was saved and retained, but Timed Export could not be armed yet ({msg}). Arming will be retried automatically."
                                )
                            })),
                        );
                    }
                } else {
                    // A future slot uses Eco as its baseline. This also makes
                    // an explicit user Timed Export request replace an
                    // unclaimed Timed Demand/ManualMode owner immediately.
                    let eco_confirmed =
                        state
                            .latest_snapshot
                            .lock()
                            .await
                            .as_ref()
                            .is_some_and(|snapshot| {
                                snapshot.battery_power_mode == 1 && !snapshot.enable_discharge
                            });
                    if !eco_confirmed {
                        // User-issued Eco restore: queue as ManualMode so a
                        // register-derived ExplicitPause cannot starve it
                        // (see the stop path and the poll.rs admission
                        // matrix).
                        let rx = queue_owned_writes_transactional(
                            &state,
                            crate::inverter::state_machines::build_timed_export_disable_writes(
                                device_type,
                            ),
                            DischargeControlOwner::ManualMode,
                        )
                        .await;
                        if let Err(msg) = await_required_write_outcome(rx).await {
                            tracing::warn!(
                                "Timed Export Eco baseline restore failed after saving slot {slot}: {msg}"
                            );
                            return (
                                StatusCode::BAD_GATEWAY,
                                Json(json!({
                                    "ok": false,
                                    "error": format!(
                                        "Discharge slot {slot} was saved and retained, but the Eco baseline could not be restored yet ({msg}). Export was not armed."
                                    )
                                })),
                            );
                        }
                    }
                }
            } else {
                // A disable is transactional too. The previous order —
                // physical clear, then disarm, then persist — left the
                // desired schedule stale on a failed disarm or a failed
                // persistence call: the inverter's physical slot was gone
                // but the user-visible schedule still pointed at an
                // enabled window, so the next poll could re-arm export
                // without any safety window. Persist first (the schedule
                // is now durable as "this slot is disabled"), then issue
                // the physical writes; if the physical step fails, the
                // prior PHYSICAL registers are compensated (CODE_REVIEW.md
                // BLOCKER — a fail-fast rejection mid-batch leaves the
                // earlier registers written) and the pre-edit desired
                // state is restored so the poll loop never sees a
                // mismatched pair.
                //
                // CODE_REVIEW.md BLOCKER: the post-edit master flag is NOT
                // re-derived from "any configured slot remains". A stopped
                // schedule with two retained slots must stay stopped when
                // one slot is disabled — deriving `enabled` from the
                // surviving slot silently re-armed the schedule and let it
                // trigger maximum-power export without the user pressing
                // Arm.
                let pre_edit_desired: Vec<crate::inverter::model::ScheduleSlot> =
                    state.timed_export_config.lock().await.slots.clone();
                let pre_edit_enabled = state.timed_export_config.lock().await.schedule_enabled;
                let pre_edit_stop_pending = state
                    .timed_export_stop_pending
                    .load(std::sync::atomic::Ordering::Acquire);
                // CODE_REVIEW.md BLOCKER: the last decoded physical slot
                // registers — the target state for the compensating batch
                // if a later write or persistence step fails.
                let prior_physical: Vec<crate::inverter::model::ScheduleSlot> = {
                    let snapshot = state.latest_snapshot.lock().await;
                    snapshot
                        .as_ref()
                        .map(|s| s.discharge_slots.to_vec())
                        .unwrap_or_default()
                };
                let schedule_enabled = pre_edit_enabled
                    && desired_slots
                        .iter()
                        .any(crate::inverter::model::ScheduleSlot::is_configured);
                let in_window =
                    schedule_enabled && should_arm_timed_export_now(&state, &desired_slots).await;

                // The marker moves atomically with the schedule change: when
                // the edit turns the master schedule off, the exit is marked
                // durably pending in the SAME transaction as the disable, so
                // a crash before the physical clear/disarm completes resumes
                // the exit on restart — and the marker can never be observed
                // disagreeing with the schedule it belongs to.
                if let Err(error) = persist_timed_export_schedule_with_stop_pending(
                    &state,
                    schedule_enabled,
                    desired_slots.clone(),
                    !schedule_enabled,
                )
                .await
                {
                    return error_response(&format!(
                        "Could not save the Timed Export schedule: {error}"
                    ));
                }
                // Stage the machine state to reflect the desired disable so
                // a poll-cycle arrival between persistence and the physical
                // writes observes the post-edit state (and any subsequent
                // failure rolls it back via `reset_timed_export_machine_after_schedule_change`).
                reset_timed_export_machine_after_schedule_change(
                    &state,
                    schedule_enabled,
                    in_window,
                )
                .await;

                let write_count = writes.len();
                tracing::info!("SetDischargeSlot {} clear encoded: {:?}", slot, writes);
                let rx = queue_writes_transactional(&state, writes).await;
                if let Err(msg) = await_required_batch_write_outcome(rx, write_count).await {
                    tracing::warn!("Timed Export slot {slot} clear failed: {msg}");
                    // The batch was rejected mid-way: some of its registers
                    // are already physically written, so first compensate the
                    // inverter's physical state, then roll back the desired
                    // schedule. The action lock guarantees no other mutation
                    // interfered while we held it.
                    let compensation =
                        compensate_discharge_slots(&state, device_type, &prior_physical, slot)
                            .await;
                    if compensation.is_ok() {
                        let pre_in_window = pre_edit_enabled
                            && should_arm_timed_export_now(&state, &pre_edit_desired).await;
                        if let Err(persist_error) = persist_timed_export_schedule_with_stop_pending(
                            &state,
                            pre_edit_enabled,
                            pre_edit_desired,
                            pre_edit_stop_pending,
                        )
                        .await
                        {
                            tracing::error!(
                                "Failed to roll back Timed Export schedule after slot {slot} clear failure: {persist_error}"
                            );
                        }
                        reset_timed_export_machine_after_schedule_change(
                            &state,
                            pre_edit_enabled,
                            pre_in_window,
                        )
                        .await;
                        return (
                            StatusCode::BAD_GATEWAY,
                            Json(json!({
                                "ok": false,
                                "error": format!(
                                    "Discharge slot {slot} could not be disabled ({msg}). The saved schedule was left unchanged."
                                )
                            })),
                        );
                    }
                    // Compensation failed: the registers cannot be trusted
                    // and the machine is parked in a terminal `Error` (with a
                    // best-effort disarm already issued). Do not claim the
                    // schedule is unchanged — the physical state may differ.
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({
                            "ok": false,
                            "error": format!(
                                "Discharge slot {slot} could not be disabled ({msg}), and restoring the previous physical schedule failed. Timed Export has been disarmed and flagged an error — re-save the schedule to recover."
                            )
                        })),
                    );
                }

                if should_return_to_eco {
                    // The final desired slot was active. Await the same
                    // model-routed disarm used by Stop before persisting Off.
                    // This is a user-issued baseline selection, so ManualMode
                    // ownership must not be starved by an explicit pause.
                    let disarm_writes =
                        crate::inverter::state_machines::build_timed_export_disable_writes(
                            device_type,
                        );
                    tracing::info!(
                        "SetDischargeSlot {} disarm encoded: {:?}",
                        slot,
                        disarm_writes
                    );
                    let rx = queue_owned_writes_transactional(
                        &state,
                        disarm_writes,
                        DischargeControlOwner::ManualMode,
                    )
                    .await;
                    if let Err(msg) = await_required_write_outcome(rx).await {
                        tracing::warn!(
                            "Timed Export disarm failed while disabling final slot {slot}: {msg}"
                        );
                        // The physical clear already succeeded, so the
                        // inverter is now armed with NO slot — the invalid
                        // paused state. Rolling the desired schedule back
                        // to enabled would make the reconciler treat the
                        // armed registers as a confirmed Active window and
                        // leave the battery paused until the window ends.
                        // Keep the (already persisted) disabled desired
                        // state and arm Exiting{0,0} instead: the disabled
                        // branch re-issues the exit writes immediately
                        // (fresh Exiting + NoneIssued, the same repair a
                        // failed Stop gets), the shared ladder bounds the
                        // retries, and a confirmed Eco settles into Off.
                        *state.timed_export_state.lock().await =
                            crate::inverter::state_machines::TimedExportState::Exiting {
                                polls_waiting: 0,
                                retries: 0,
                            };
                        return (
                            StatusCode::BAD_GATEWAY,
                            Json(json!({
                                "ok": false,
                                "error": format!(
                                    "Discharge slot {slot} was cleared, but Timed Export could not be disarmed yet ({msg}). The schedule is disabled — the poll loop keeps retrying the disarm."
                                )
                            })),
                        );
                    }
                }
            }

            // Return the updated desired schedule so the UI can update its
            // local copy immediately — under the re-arm fallback the
            // physical readback intentionally stays zero, so waiting for
            // the next poll would show an empty editor.
            let schedule_json = {
                let config = state.timed_export_config.lock().await;
                json!({
                    "schedule_enabled": config.schedule_enabled,
                    "slots": config.slots,
                })
            };
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "message": format!("Discharge slot {} configured", slot),
                    "schedule": schedule_json,
                })),
            )
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
            queue_owned_writes(&state, writes, DischargeControlOwner::ManualMode).await;
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
    let settings = crate::settings::Settings::load();
    if settings.adaptive_charge_enabled || settings.adaptive_charge_saved_limit.is_some() {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "ok": false,
                "error": if settings.adaptive_charge_enabled {
                    "Charge rate is controlled by Adaptive Charge"
                } else {
                    "Adaptive Charge is restoring the previous charge rate"
                }
            })),
        );
    }
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
            queue_owned_writes(&state, writes, DischargeControlOwner::ManualMode).await;
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
            queue_owned_writes(&state, writes, DischargeControlOwner::ManualMode).await;
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
    // with a confirmed safe write path (see DeviceType::supports_eps). Refuse
    // every unverified register write up front with a clear error.
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
        || matches!(device_type, DeviceType::Ems | DeviceType::EmsCommercial)
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

/// POST /api/control/pause — pause battery discharge using Eco Paused.
///
/// Eco Paused is the standard Eco/self-consumption mode with discharge
/// disabled and the SOC reserve set to 100%. Solar and scheduled grid charging
/// remain available. Keep this deliberately small: it should not clear the
/// user's charge/discharge schedules or other slots.
pub async fn pause_battery(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    // Refuse to pause while clearly offline. Writes queued while Disconnected
    // would sit idle until the next reconnect (and likely time out the
    // frontend's confirmation), so surface a precise error immediately
    // instead of a silent ~15 s wait. A transient Reconnecting blip is
    // allowed — the write queues and runs as soon as the link comes back.
    if matches!(
        state.connection_state.lock().await.clone(),
        ConnectionState::Disconnected
    ) {
        return error_response(
            "Cannot pause discharge: the inverter is not connected. Please retry once the connection is restored.",
        );
    }
    let device_type = latest_device_type(&state).await;
    let reserve_before_pause = state
        .latest_snapshot
        .lock()
        .await
        .as_ref()
        .map(|s| s.battery_reserve as u16)
        .filter(|reserve| (4..100).contains(reserve));
    if let Some(reserve) = reserve_before_pause {
        let mut saved = state.load_limiter_saved.lock().await;
        *saved = Some(crate::inverter::poll::LoadLimiterSaved { reserve });

        // Transactional save (review #1 follow-up): single-field update.
        if let Err(e) = crate::settings::Settings::update(|s| {
            s.load_limiter_saved_reserve = Some(reserve);
        }) {
            tracing::warn!("Failed to persist pause reserve for manual unpause: {e}");
        }
    }

    let mut writes = Vec::new();

    for cmd in [
        ControlCommand::SetBatteryPowerMode { mode: 1 },
        ControlCommand::SetEnableDischarge { enabled: false },
    ] {
        match cmd.encode() {
            Ok(mut w) => writes.append(&mut w),
            Err(e) => return error_response(&format!("Validation error: {}", e)),
        }
    }
    match reserve_writes_for_device(device_type, 100) {
        Ok(mut w) => writes.append(&mut w),
        Err(e) => return error_response(&format!("Validation error: {}", e)),
    }

    tracing::info!("PauseBattery encoded: {:?}", writes);
    let rx =
        queue_owned_writes_with_completion(&state, writes, DischargeControlOwner::ExplicitPause)
            .await;
    match await_write_outcome(rx).await {
        Ok(()) => ok_response("Battery paused"),
        Err(msg) => {
            tracing::warn!("Pause discharge rejected: {msg}");
            error_response(&format!(
                "Pause discharge could not be fully applied ({msg}). Discharge may be only partially paused; please try again."
            ))
        }
    }
}

/// POST /api/control/unpause — resume battery discharge in Eco mode.
///
/// Eco Paused is represented as self-consumption mode with discharge disabled
/// and SOC reserve at 100%. To unpause, keep the safe Eco/discharge-disabled
/// flags but restore the reserve below 100% so the battery can discharge to
/// serve house load again. If a limiter captured a previous reserve, use it;
/// otherwise fall back to the app's normal 4% reserve. A manual unpause while
/// either limiter owns the pause disables all active pause limiters, making
/// the override lasting instead of immediately starting another countdown.
pub async fn unpause_battery(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    // See pause_battery: refuse while clearly offline so the caller gets an
    // immediate, precise error instead of a silent timeout. No side effects
    // have run yet on refusal, so the limiter state is left untouched.
    if matches!(
        state.connection_state.lock().await.clone(),
        ConnectionState::Disconnected
    ) {
        return error_response(
            "Cannot resume discharge: the inverter is not connected. Please retry once the connection is restored.",
        );
    }
    let device_type = latest_device_type(&state).await;
    // Follow the poll loop's load config → load state → temperature config →
    // temperature state → shared saved reserve lock order.
    let (restore_reserve, disabled_load_limiter, disabled_temperature_limiter) = {
        let mut load_config = state.load_limiter_config.lock().await;
        let mut load_state = state.load_limiter_state.lock().await;
        let mut temperature_config = state.temperature_limiter_config.lock().await;
        let mut temperature_state = state.temperature_limiter_state.lock().await;
        let mut saved = state.load_limiter_saved.lock().await;
        let disabled_load_limiter = load_state.is_actively_pausing();
        let disabled_temperature_limiter = temperature_state.is_actively_pausing();
        if disabled_load_limiter {
            load_config.enabled = false;
        }
        if disabled_temperature_limiter {
            temperature_config.enabled = false;
        }
        *load_state = crate::inverter::poll::LoadLimiterState::Idle;
        *temperature_state = crate::inverter::poll::TemperatureLimiterState::Idle;
        let restore_reserve = saved.take().map(|s| s.reserve).unwrap_or(4).clamp(4, 99);
        (
            restore_reserve,
            disabled_load_limiter,
            disabled_temperature_limiter,
        )
    };

    // Transactional save (review #1 follow-up): the five-field reset
    // happens under the settings lock so a concurrent writer can't
    // resurrect any of the flags between our read and our save.
    if let Err(e) = crate::settings::Settings::update(|settings| {
        if disabled_load_limiter {
            settings.load_limiter_enabled = false;
        }
        if disabled_temperature_limiter {
            settings.temperature_limiter_enabled = false;
        }
        settings.load_limiter_active_persisted = false;
        settings.temperature_limiter_active_persisted = false;
        settings.load_limiter_saved_reserve = None;
    }) {
        tracing::warn!("Failed to persist load limiter reset during manual unpause: {e}");
    }

    let mut writes = Vec::new();
    for cmd in [
        ControlCommand::SetBatteryPowerMode { mode: 1 },
        ControlCommand::SetEnableDischarge { enabled: false },
    ] {
        match cmd.encode() {
            Ok(mut w) => writes.append(&mut w),
            Err(e) => return error_response(&format!("Validation error: {}", e)),
        }
    }
    match reserve_writes_for_device(device_type, restore_reserve) {
        Ok(mut w) => writes.append(&mut w),
        Err(e) => return error_response(&format!("Validation error: {}", e)),
    }

    tracing::info!(
        restore_reserve,
        disabled_load_limiter,
        disabled_temperature_limiter,
        "UnpauseBattery encoded: {:?}",
        writes
    );
    let rx =
        queue_owned_writes_with_completion(&state, writes, DischargeControlOwner::ExplicitPause)
            .await;
    match await_write_outcome(rx).await {
        Ok(()) => {}
        Err(msg) => {
            tracing::warn!("Resume discharge rejected: {msg}");
            let detail = if disabled_load_limiter || disabled_temperature_limiter {
                format!(" Active limiter(s) were disabled, but the inverter rejected the reserve restore ({msg}); please try again.")
            } else {
                format!(" The inverter rejected the reserve restore ({msg}); please try again.")
            };
            return error_response(&format!(
                "Resume discharge could not be fully applied.{detail}"
            ));
        }
    }
    let message = if disabled_load_limiter || disabled_temperature_limiter {
        format!(
            "Active limiter(s) disabled and battery unpaused (reserve restored to {restore_reserve}%)"
        )
    } else {
        format!("Battery unpaused (reserve restored to {restore_reserve}%)")
    };
    ok_response(&message)
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
/// Returns HTTP 400 while a HEM-owned Force Discharge is in progress; callers
/// must stop it first so the two restore snapshots cannot overlap.
pub async fn force_charge(
    State(state): State<Arc<AppState>>,
    body: Option<Json<serde_json::Value>>,
) -> (StatusCode, Json<Value>) {
    let _force_action_guard = state.force_action_lock.lock().await;
    if state.force_discharge_revert.lock().await.is_some() {
        return error_response("Stop Force Discharge before starting Force Charge");
    }

    let device_type = latest_device_type(&state).await;
    let is_three_phase = device_type.uses_three_phase_schedule_slots();
    let mut writes = Vec::new();

    // Capture the pre-force-charge state BEFORE queuing the force-charge
    // writes, so the stop endpoint can restore the inverter to this point.
    // This is the Rust equivalent of GivTCP's `revert` dict (write.py:1148).
    let revert = capture_force_charge_revert(&state, device_type).await;

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
            *state.force_charge_revert.lock().await = revert;
            tracing::info!("ForceCharge encoded: {:?}", writes);
            queue_owned_writes(&state, writes, DischargeControlOwner::ManualForce).await;
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
pub async fn force_charge_stop(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let _force_action_guard = state.force_action_lock.lock().await;
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
    queue_owned_writes(&state, writes, DischargeControlOwner::ManualForce).await;
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
/// Returns HTTP 400 while a HEM-owned Force Charge is in progress; callers
/// must stop it first so the two restore snapshots cannot overlap.
pub async fn force_discharge(
    State(state): State<Arc<AppState>>,
    body: Option<Json<serde_json::Value>>,
) -> (StatusCode, Json<Value>) {
    let _force_action_guard = state.force_action_lock.lock().await;
    if state.force_charge_revert.lock().await.is_some() {
        return error_response("Stop Force Charge before starting Force Discharge");
    }

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
            r.force_discharge_slot_end_ms = Some(expiry.timestamp_millis());
        }
        match force_discharge_slot_writes(device_type, minutes) {
            Ok(mut slot_writes) => writes.append(&mut slot_writes),
            Err(e) => return error_response(&format!("Failed to encode discharge slot: {}", e)),
        }
    }

    // Issue #289: an armed discharge pause (HR318=2/3) would block the
    // forced discharge. The pre-action pause configuration is already
    // captured in `revert`; temporarily disable pause mode so discharge
    // can run. Stop Discharge / auto-revert restores the exact prior
    // HR318/319/320 state. Mode 1 (pause charging) does not block
    // discharge and is left alone.
    if revert.as_ref().is_some_and(|r| {
        crate::inverter::state_machines::should_disable_pause_for_force_discharge(
            r.battery_pause_mode,
        )
    }) {
        writes.push(RegisterWrite {
            address: crate::modbus::registers::HR_BATTERY_PAUSE_MODE,
            value: 0,
        });
    }

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
                // isn't overwritten by 00:00–23:59. The single-phase
                // `ForceDischarge` encoder writes 4 slot registers that
                // must be stripped here:
                //   HR_DISCHARGE_SLOT_1_START/END, HR_DISCHARGE_SLOT_2_START/END
                //
                // The three-phase branch is a defensive no-op today:
                // `ThreePhaseForceDischarge::encode()` emits only the
                // power-mode and force-enable flags — no slot registers —
                // so there is nothing to strip. It's kept so that if the
                // encoder ever gains slot writes (mirroring the
                // single-phase layout), the duration slot is still
                // preserved instead of clobbered by a default.
                use crate::modbus::registers::{
                    HR_3PH_DISCHARGE_SLOT_1_END, HR_3PH_DISCHARGE_SLOT_1_START,
                    HR_3PH_DISCHARGE_SLOT_2_END, HR_3PH_DISCHARGE_SLOT_2_START,
                    HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_END,
                    HR_DISCHARGE_SLOT_2_START,
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
            *state.force_discharge_revert.lock().await = revert;
            tracing::info!("ForceDischarge encoded: {:?}", writes);
            queue_owned_writes(&state, writes, DischargeControlOwner::ManualForce).await;
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
/// If the app restarted while Force Discharge was active, the captured
/// revert snapshot is gone. In that case, fall back to a minimal safe stop
/// based on the latest inverter snapshot instead of returning 400 and
/// sending no writes.
pub async fn force_discharge_stop(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let _force_action_guard = state.force_action_lock.lock().await;
    let device_type = latest_device_type(&state).await;
    let revert = state.force_discharge_revert.lock().await.take();

    let writes = if let Some(revert) = revert {
        build_force_discharge_stop_writes(device_type, &revert)
    } else {
        let snapshot = state.latest_snapshot.lock().await;
        let Some(snapshot) = snapshot.as_ref() else {
            return error_response("No force discharge in progress to stop");
        };
        let writes = build_force_discharge_restart_stop_writes(device_type, snapshot);
        if writes.is_empty() {
            return error_response("No force discharge in progress to stop");
        }
        writes
    };

    if writes.is_empty() {
        return error_response("No restore writes generated for this device");
    }
    tracing::info!("ForceDischarge stop encoded: {:?}", writes);
    queue_owned_writes(&state, writes, DischargeControlOwner::ManualForce).await;
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

#[derive(Debug, Clone, Deserialize)]
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

/// Resolve History query parameters to the exact database window shared by
/// the period-summary endpoint and the chart API contract. Explicit browser
/// boundaries win so Today/Month follow the viewer's local calendar even when
/// the backend runs in UTC (Docker/NAS deployments).
fn resolve_history_summary_window(
    params: &HistoryQuery,
) -> Result<(crate::history::HistoryWindow, i64), String> {
    let range_str = params.range.as_deref().unwrap_or("24h");
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
        "month" => (0, 3600),
        _ => {
            return Err(
                "Invalid range. Use: 1h, 6h, 12h, 24h, today, 7d, 30d, 6m, 1y, month".to_string(),
            )
        }
    };

    let explicit_window = if let (Some(start_ms), Some(end_ms)) = (params.start_ms, params.end_ms) {
        if start_ms >= end_ms {
            return Err("Invalid history window: start_ms must be before end_ms".to_string());
        }
        Some((start_ms.div_euclid(1000), (end_ms + 999).div_euclid(1000)))
    } else if rolling && range_str != "month" && range_str != "today" {
        let end_ts = chrono::Utc::now().timestamp() - offset * range_secs;
        Some((end_ts - range_secs, end_ts))
    } else if range_str == "today" {
        let now = chrono::Local::now();
        let start_date = now.date_naive() - chrono::Duration::days(offset);
        let start_local = chrono::Local
            .from_local_datetime(&start_date.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .unwrap();
        let end_date = start_date.succ_opt().unwrap();
        let end_local = chrono::Local
            .from_local_datetime(&end_date.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .unwrap();
        Some((start_local.timestamp(), end_local.timestamp()))
    } else if range_str == "month" {
        let now = chrono::Local::now();
        let total_months = now.year() * 12 + now.month() as i32 - 1 - offset as i32;
        let target_year = total_months.div_euclid(12);
        let target_month = (total_months.rem_euclid(12) + 1) as u32;
        let start_local = chrono::Local
            .from_local_datetime(
                &chrono::NaiveDate::from_ymd_opt(target_year, target_month, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            )
            .earliest()
            .unwrap();
        let (next_year, next_month) = if target_month == 12 {
            (target_year + 1, 1)
        } else {
            (target_year, target_month + 1)
        };
        let end_local = chrono::Local
            .from_local_datetime(
                &chrono::NaiveDate::from_ymd_opt(next_year, next_month, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            )
            .earliest()
            .unwrap();
        Some((start_local.timestamp(), end_local.timestamp()))
    } else {
        None
    };

    Ok((
        crate::history::HistoryWindow {
            range_secs,
            offset,
            explicit_window,
        },
        bucket_secs,
    ))
}

/// GET /api/history — aggregated time-series data for charts.
///
/// Query params: `range`, `fields`, `offset`, `rolling`, optional `start_ms`/`end_ms`.
/// Returns `{ok: true, data: {field: [{t, v}, ...]}}`.
pub async fn get_history(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HistoryQuery>,
) -> (StatusCode, Json<Value>) {
    let fields_str = params.fields.as_deref().unwrap_or("soc");
    // Single source of truth for range/bucket/window resolution — shared
    // with the summary and report endpoints so the three cannot drift
    // (review finding #9; the DST/standing-charge bug was previously fixed
    // in parallel in multiple copies of this logic).
    let (window, bucket_secs) = match resolve_history_summary_window(&params) {
        Ok(resolved) => resolved,
        Err(msg) => return error_response(&msg),
    };
    let range_secs = window.range_secs;

    let explicit_window = window.explicit_window;

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
    let want_import_cost = fields
        .iter()
        .any(|f| f == crate::history::IMPORT_COST_FIELD);
    // Import-cost breakdown: the per-kWh energy component and the fixed daily
    // standing-charge component, split out of the same total so the History
    // chart can show them as separate lines.
    let want_import_energy = fields
        .iter()
        .any(|f| f == crate::history::IMPORT_ENERGY_COST_FIELD);
    let want_import_standing = fields
        .iter()
        .any(|f| f == crate::history::IMPORT_STANDING_CHARGE_FIELD);
    let want_any_import_cost = want_import_cost || want_import_energy || want_import_standing;
    let want_export_income = fields
        .iter()
        .any(|f| f == crate::history::EXPORT_INCOME_FIELD);
    let normal_fields: Vec<String> = fields
        .iter()
        .filter(|f| !crate::history::is_cost_field(f))
        .cloned()
        .collect();

    // Resolve tariff configs only when a cost field is requested, falling back
    // to a flat single-slot config built from the legacy scalar rate.
    // Issue #131: also carry the import-side Standing Charge (pence/day)
    // so the cost series includes the daily fixed component.
    let cost_cfgs = if want_any_import_cost || want_export_income {
        let s = crate::settings::Settings::load();
        let import_cfg = s
            .import_tariff_config
            .clone()
            .unwrap_or_else(|| crate::settings::TariffConfig::flat(s.import_tariff));
        let export_cfg = s
            .export_tariff_config
            .clone()
            .unwrap_or_else(|| crate::settings::TariffConfig::flat(s.export_tariff));
        Some((
            import_cfg,
            export_cfg,
            s.import_tariff,
            s.export_tariff,
            s.import_standing_charge_p_per_day,
        ))
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
                        offset: params.offset.unwrap_or(0),
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

                    if let Some((
                        import_cfg,
                        export_cfg,
                        flat_import,
                        flat_export,
                        import_standing_charge_p_per_day,
                    )) = &cost_cfgs
                    {
                        if want_any_import_cost {
                            // One walk yields both components; the total, the
                            // per-kWh energy series and the standing-charge
                            // series are all cut from it so `energy + standing`
                            // matches the total exactly. Issue #131: the
                            // import direction carries the daily standing
                            // charge; export doesn't (UK SEG has no standing
                            // fee on exports), so it's served separately below.
                            let breakdown = db.query_cost_breakdown(
                                &window,
                                bucket_secs,
                                "today_import_kwh",
                                import_cfg,
                                *flat_import,
                                *import_standing_charge_p_per_day,
                            )?;
                            if want_import_cost {
                                let series: Vec<crate::history::TimePoint> = breakdown
                                    .iter()
                                    .map(|c| crate::history::TimePoint {
                                        t: c.t,
                                        v: c.energy_gbp + c.standing_gbp,
                                    })
                                    .collect();
                                data.insert(
                                    crate::history::IMPORT_COST_FIELD.to_string(),
                                    serde_json::to_value(&series).unwrap_or(Value::Null),
                                );
                            }
                            if want_import_energy {
                                let series: Vec<crate::history::TimePoint> = breakdown
                                    .iter()
                                    .map(|c| crate::history::TimePoint {
                                        t: c.t,
                                        v: c.energy_gbp,
                                    })
                                    .collect();
                                data.insert(
                                    crate::history::IMPORT_ENERGY_COST_FIELD.to_string(),
                                    serde_json::to_value(&series).unwrap_or(Value::Null),
                                );
                            }
                            if want_import_standing {
                                let series: Vec<crate::history::TimePoint> = breakdown
                                    .iter()
                                    .map(|c| crate::history::TimePoint {
                                        t: c.t,
                                        v: c.standing_gbp,
                                    })
                                    .collect();
                                data.insert(
                                    crate::history::IMPORT_STANDING_CHARGE_FIELD.to_string(),
                                    serde_json::to_value(&series).unwrap_or(Value::Null),
                                );
                            }
                        }
                        if want_export_income {
                            let series = db.query_cost_series(
                                &window,
                                bucket_secs,
                                "today_export_kwh",
                                export_cfg,
                                *flat_export,
                                // Issue #131: no export-side Standing Charge
                                // today (UI has no input for it).
                                0.0,
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
// History period summary (issue #237)
// ---------------------------------------------------------------------------

/// GET /api/history/summary — additive energy and cost totals for the exact
/// History range currently selected by the user.
///
/// Query params mirror `/api/history`: `range`, `offset`, `rolling`, and the
/// optional browser-local `start_ms`/`end_ms` boundaries. The response is
/// `{ok: true, data: {...}}`; energy values are kWh and money values are GBP.
pub async fn get_history_summary(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HistoryQuery>,
) -> (StatusCode, Json<Value>) {
    let (window, bucket_secs) = match resolve_history_summary_window(&params) {
        Ok(value) => value,
        Err(error) => return error_response(&error),
    };
    let (start_ts, end_ts) = window.resolve();

    let settings = crate::settings::Settings::load();
    let import_tariff = settings
        .import_tariff_config
        .clone()
        .unwrap_or_else(|| crate::settings::TariffConfig::flat(settings.import_tariff));
    let export_tariff = settings
        .export_tariff_config
        .clone()
        .unwrap_or_else(|| crate::settings::TariffConfig::flat(settings.export_tariff));
    let standing_charge_p_per_day = settings.import_standing_charge_p_per_day.max(0.0);
    let flat_import = settings.import_tariff;
    let flat_export = settings.export_tariff;

    let history_db = state.history.lock().await.clone();
    let Some(db) = history_db else {
        return server_error("History database not available");
    };

    let result = tokio::task::spawn_blocking(move || {
        let energy = db.query_energy_summary(&window)?;
        let import_series = db.query_cost_series(
            &window,
            bucket_secs,
            "today_import_kwh",
            &import_tariff,
            flat_import,
            standing_charge_p_per_day,
        )?;
        let export_series = db.query_cost_series(
            &window,
            bucket_secs,
            "today_export_kwh",
            &export_tariff,
            flat_export,
            0.0,
        )?;
        let days_in_range = crate::history::days_in_local_window(start_ts, end_ts, &chrono::Local);
        let standing_charge_gbp = days_in_range as f64 * standing_charge_p_per_day / 100.0;
        // Both the box and the chart's _import_cost / _export_income
        // series trust the daily counter alone. The chart never applied a
        // grid-power trapezoid fallback, and neither does the box — when
        // the counter is empty the box shows the standing-charge-only
        // figure (issue #269 comment 5407621722). See api269_empty_counter_trusts_counter_no_fallback.
        let import_cost_gbp = import_series
            .last()
            .map(|point| point.v)
            .unwrap_or(standing_charge_gbp);
        let export_income_gbp = export_series.last().map(|point| point.v).unwrap_or(0.0);
        let net_cost_gbp = import_cost_gbp - export_income_gbp;

        Ok::<_, String>(json!({
            "ok": true,
            "data": {
                "solar_generated_kwh": energy.solar_generated_kwh,
                "battery_charged_kwh": energy.battery_charged_kwh,
                "battery_discharged_kwh": energy.battery_discharged_kwh,
                "grid_imported_kwh": energy.grid_imported_kwh,
                "grid_exported_kwh": energy.grid_exported_kwh,
                "home_consumed_kwh": energy.home_consumed_kwh,
                "import_cost_gbp": import_cost_gbp,
                "export_income_gbp": export_income_gbp,
                "net_cost_gbp": net_cost_gbp
            }
        }))
    })
    .await;

    match result {
        Ok(Ok(json)) => (StatusCode::OK, Json(json)),
        Ok(Err(error)) => error_response(&error),
        Err(error) => error_response(&format!("History summary query join error: {error}")),
    }
}

// ---------------------------------------------------------------------------
// Consumption report cost endpoint (issue #131)
//
// The Power page's Consumption Report button needs per-window cost totals
// matching the same range / offset the user is looking at in the graphs.
// The cost is built server-side from the same `today_import_kwh` /
// `today_export_kwh` counters and the configured tariffs the History page
// uses, plus the Standing Charge applied once per local day covered by
// the window (matching the per-day step pattern in the History cost
// graph).
// ---------------------------------------------------------------------------

/// GET /api/report — cost totals for the Power page Consumption Report.
///
/// Query params mirror `/api/history`: `range`, `offset`, `rolling`,
/// `start_ms`, `end_ms`. Returns a flat JSON object:
///   `{ ok, import_cost_gbp, export_income_gbp, net_cost_gbp,
///      standing_charge_gbp, days_in_range }`
/// where every cost figure is the cumulative £ total over the queried
/// window (per-kWh component + standing-charge step sum), and
/// `days_in_range` is the count of distinct local calendar days the
/// window touches (used by the frontend for the footnote line and for
/// deriving the per-day average).
pub async fn get_report(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HistoryQuery>,
) -> (StatusCode, Json<Value>) {
    // Single source of truth for range/bucket/window resolution, shared with
    // the history chart and summary endpoints (review finding #9).
    let (window, bucket_secs) = match resolve_history_summary_window(&params) {
        Ok(resolved) => resolved,
        Err(msg) => return error_response(&msg),
    };

    let (start_ts, end_ts) = window.resolve();

    let s = crate::settings::Settings::load();
    let import_tariff = s
        .import_tariff_config
        .clone()
        .unwrap_or_else(|| crate::settings::TariffConfig::flat(s.import_tariff));
    let export_tariff = s
        .export_tariff_config
        .clone()
        .unwrap_or_else(|| crate::settings::TariffConfig::flat(s.export_tariff));
    let standing_charge_p_per_day = s.import_standing_charge_p_per_day.max(0.0);
    let flat_import = s.import_tariff;
    let flat_export = s.export_tariff;

    let history_db = state.history.lock().await.clone();
    let Some(db) = history_db else {
        return server_error("History database not available");
    };

    // Run the cost integration on a blocking thread — same SQLite mutex
    // pattern as `/api/history`. We need both the import and export cost
    // series; the Standing Charge is the same on both (UK bills charge
    // standing fees on the import side only, never on exports).
    let result = tokio::task::spawn_blocking(move || {
        let import_series = db.query_cost_series(
            &window,
            bucket_secs,
            "today_import_kwh",
            &import_tariff,
            flat_import,
            standing_charge_p_per_day,
        )?;
        let export_series = db.query_cost_series(
            &window,
            bucket_secs,
            "today_export_kwh",
            &export_tariff,
            flat_export,
            // Issue #131: no export-side Standing Charge today (UI has no
            // input for it).
            0.0,
        )?;
        let days_in_range = crate::history::days_in_local_window(start_ts, end_ts, &chrono::Local);
        let standing_charge_gbp = days_in_range as f64 * standing_charge_p_per_day / 100.0;
        // Both the report and the History chart's _import_cost / _export_income
        // series trust the daily counter alone — no grid-power trapezoid
        // fallback. When the counter is empty the report shows the
        // standing-charge-only figure, matching the chart (issue #269
        // comment 5407621722). The report still surfaces the standing-
        // charge component separately so the frontend can footnote the
        // "kWh + standing charge = total" breakdown.
        let import_cost_gbp = import_series
            .last()
            .map(|p| p.v)
            .unwrap_or(standing_charge_p_per_day / 100.0);
        let export_income_gbp = export_series.last().map(|p| p.v).unwrap_or(0.0);
        let net_cost_gbp = import_cost_gbp - export_income_gbp;
        Ok::<_, String>(serde_json::json!({
            "ok": true,
            "import_cost_gbp": import_cost_gbp,
            "export_income_gbp": export_income_gbp,
            "net_cost_gbp": net_cost_gbp,
            "standing_charge_gbp": standing_charge_gbp,
            "days_in_range": days_in_range,
            "standing_charge_p_per_day": standing_charge_p_per_day,
        }))
    })
    .await;

    match result {
        Ok(Ok(json)) => (StatusCode::OK, Json(json)),
        Ok(Err(e)) => error_response(&e),
        Err(e) => error_response(&format!("Report query join error: {e}")),
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

    // Transactional save (review #1): the `Settings::update` helper holds
    // the settings lock across the read-modify-write so a concurrent
    // settings-save from another handler can't clobber this one's fields.
    // Drop the in-memory config lock first — Settings::update takes its own
    // settings lock; holding both at once would risk a deadlock if another
    // path ever needed the two in the same order.
    let snapshot = config.clone();
    drop(config);
    if let Err(e) = crate::settings::Settings::update(|app_settings| {
        app_settings.auto_winter_enabled = snapshot.enabled;
        app_settings.auto_winter_cold_threshold = snapshot.cold_threshold;
        app_settings.auto_winter_recovery_threshold = snapshot.recovery_threshold;
        app_settings.auto_winter_target_soc = snapshot.target_soc;
        app_settings.auto_winter_debounce_readings = snapshot.debounce_readings;
    }) {
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
        config.batt_temp_max = v.clamp(0.0, 120.0) as f32;
    }
    if let Some(v) = body.get("inverter_temp_min").and_then(|v| v.as_f64()) {
        config.inverter_temp_min = v.clamp(0.0, 120.0) as f32;
    }
    if let Some(v) = body.get("inverter_temp_max").and_then(|v| v.as_f64()) {
        config.inverter_temp_max = v.clamp(0.0, 120.0) as f32;
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
    if let Some(v) = body.get("inverter_trip_enabled").and_then(|v| v.as_bool()) {
        config.inverter_trip_enabled = v;
    }
    if let Some(v) = body
        .get("connection_lost_enabled")
        .and_then(|v| v.as_bool())
    {
        config.connection_lost_enabled = v;
    }
    if let Some(v) = body
        .get("battery_over_temp_enabled")
        .and_then(|v| v.as_bool())
    {
        config.battery_over_temp_enabled = v;
    }
    if let Some(v) = body
        .get("battery_connection_lost_enabled")
        .and_then(|v| v.as_bool())
    {
        config.battery_connection_lost_enabled = v;
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

    // Transactional save (review #1): see `set_auto_winter` for the
    // reasoning. Drop the in-memory config lock before taking the settings
    // lock to keep the lock order consistent across handlers.
    let snapshot = config.clone();
    drop(config);
    if let Err(e) = crate::settings::Settings::update(|app_settings| {
        app_settings.alerts_config = snapshot;
    }) {
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
pub async fn post_reconnect(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
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
// Unified charging mode + Adaptive Charge endpoints (issue #234)
// ---------------------------------------------------------------------------

fn parse_cosy_slots(value: Option<&serde_json::Value>) -> Option<Vec<crate::settings::CosySlot>> {
    value?.as_array().map(|slots| {
        slots
            .iter()
            .map(|slot| crate::settings::CosySlot {
                enabled: slot["enabled"].as_bool().unwrap_or(false),
                start_hour: slot["start_hour"]
                    .as_u64()
                    .map(|value| value.min(23))
                    .unwrap_or(0) as u8,
                start_minute: slot["start_minute"]
                    .as_u64()
                    .map(|value| value.min(59))
                    .unwrap_or(0) as u8,
                end_hour: slot["end_hour"]
                    .as_u64()
                    .map(|value| value.min(23))
                    .unwrap_or(0) as u8,
                end_minute: slot["end_minute"]
                    .as_u64()
                    .map(|value| value.min(59))
                    .unwrap_or(0) as u8,
                target_soc: slot["target_soc"].as_u64().unwrap_or(100).clamp(4, 100) as u8,
            })
            .collect()
    })
}

fn parse_charging_mode(value: &str) -> Option<crate::settings::ChargingMode> {
    use crate::settings::ChargingMode;
    match value {
        "standard" => Some(ChargingMode::Standard),
        "cosy" => Some(ChargingMode::Cosy),
        "agile" => Some(ChargingMode::Agile),
        "agile_charge" => Some(ChargingMode::AgileCharge),
        "agile_discharge" => Some(ChargingMode::AgileDischarge),
        "adaptive" => Some(ChargingMode::Adaptive),
        _ => None,
    }
}

fn apply_charging_mode(
    settings: &mut crate::settings::Settings,
    mode: crate::settings::ChargingMode,
) {
    use crate::settings::{AgileScope, ChargingMode};
    settings.cosy_enabled = matches!(mode, ChargingMode::Cosy);
    settings.adaptive_charge_enabled = matches!(mode, ChargingMode::Adaptive);
    settings.agile_scope = match mode {
        ChargingMode::Agile => AgileScope::Full,
        ChargingMode::AgileCharge => AgileScope::ChargeOnly,
        ChargingMode::AgileDischarge => AgileScope::DischargeOnly,
        _ => AgileScope::Off,
    };
    settings.agile_enabled = settings.agile_scope.is_enabled();
    if !settings.cosy_enabled {
        settings.cosy_active_persisted = false;
    }
}

pub async fn get_charging_mode(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let settings = crate::settings::Settings::load();
    let adaptive_state = state.adaptive_charge_state.lock().await.clone();
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "mode": crate::settings::charging_mode_for_settings(&settings),
            "adaptive_state": adaptive_state.api_name(),
            "active_period": adaptive_state.active_period(),
        })),
    )
}

pub async fn set_charging_mode(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let Some(mode_name) = body.get("mode").and_then(|value| value.as_str()) else {
        return error_response("Missing 'mode' field");
    };
    let Some(mode) = parse_charging_mode(mode_name) else {
        return error_response("Unknown charging mode");
    };

    // Preflight: validate the existing adaptive config + check the live
    // snapshot for the Adaptive register. The validation/snapshot read
    // happens outside the settings lock; the actual mutation happens
    // inside the closure under the lock (review #1 follow-up).
    if mode == crate::settings::ChargingMode::Adaptive {
        let pre_settings = crate::settings::Settings::load();
        if let Err(e) = pre_settings.adaptive_charge_config.validate() {
            return error_response(&format!("Invalid Adaptive Charge config: {e}"));
        }
        drop(pre_settings);
        let snapshot_for_preflight = state.latest_snapshot.lock().await.clone();
        let Some(snapshot_for_preflight) = snapshot_for_preflight else {
            return error_response("Connect to the inverter before enabling Adaptive Charge");
        };
        if crate::inverter::state_machines::adaptive_charge_register(
            snapshot_for_preflight.device_type,
        )
        .is_none()
        {
            return error_response("Adaptive Charge is not supported by this inverter");
        }
    }

    let cosy_slots = parse_cosy_slots(body.get("cosy_slots"));
    let mut previous_scope = crate::settings::AgileScope::Off;
    // Snapshot the fields `queue_cached_agile_action_for_settings` reads
    // so we can feed it the just-written state without re-reading disk
    // (review #1 follow-up: a post-save Settings::load() would race with
    // a concurrent writer, defeating the point of Settings::update).
    let mut post_snapshot = crate::settings::Settings::default();
    let save_result = crate::settings::Settings::update(|settings| {
        previous_scope = crate::settings::agile_scope_for_settings(settings);
        apply_charging_mode(settings, mode);
        if let Some(slots) = cosy_slots.clone() {
            settings.cosy_slots = slots;
        }
        post_snapshot.cosy_enabled = settings.cosy_enabled;
        post_snapshot.agile_scope = settings.agile_scope;
        post_snapshot.agile_enabled = settings.agile_enabled;
        post_snapshot.agile_charge_threshold = settings.agile_charge_threshold;
        post_snapshot.agile_discharge_threshold = settings.agile_discharge_threshold;
    });
    if let Err(e) = save_result {
        return server_error(&format!("Failed to save charging mode: {e}"));
    }

    if previous_scope != crate::settings::AgileScope::Off
        && !matches!(
            mode,
            crate::settings::ChargingMode::Agile
                | crate::settings::ChargingMode::AgileCharge
                | crate::settings::ChargingMode::AgileDischarge
        )
    {
        let device_type = latest_device_type(&state).await;
        let command = if device_type.uses_three_phase_schedule_slots() {
            ControlCommand::ThreePhaseAgileClearActiveSlot
        } else {
            ControlCommand::AgileClearActiveSlot
        };
        if let Ok(writes) = command.encode() {
            queue_owned_writes(&state, writes, DischargeControlOwner::Agile).await;
        }
    }

    if matches!(
        mode,
        crate::settings::ChargingMode::Agile
            | crate::settings::ChargingMode::AgileCharge
            | crate::settings::ChargingMode::AgileDischarge
    ) {
        queue_cached_agile_action_for_settings(&state, &post_snapshot).await;
    }
    state.write_notify.notify_one();
    ok_response("Charging mode updated")
}

pub async fn get_adaptive_charge(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let settings = crate::settings::Settings::load();
    let runtime = state.adaptive_charge_state.lock().await.clone();
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "data": {
                "enabled": settings.adaptive_charge_enabled,
                "config": settings.adaptive_charge_config,
                "state": runtime.api_name(),
                "active_period": runtime.active_period(),
            }
        })),
    )
}

pub async fn set_adaptive_charge(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let config_value = body.get("config").cloned().unwrap_or(body);
    let config: crate::settings::AdaptiveChargeConfig = match serde_json::from_value(config_value) {
        Ok(config) => config,
        Err(e) => return error_response(&format!("Invalid Adaptive Charge config: {e}")),
    };
    if let Err(e) = config.validate() {
        return error_response(&e);
    }

    // Transactional save (review #1 follow-up): single-field update.
    if let Err(e) = crate::settings::Settings::update(|s| {
        s.adaptive_charge_config = config;
    }) {
        return server_error(&format!("Failed to save Adaptive Charge config: {e}"));
    }
    state.write_notify.notify_one();
    ok_response("Adaptive Charge config updated")
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
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let enabled = body["enabled"].as_bool().unwrap_or(false);
    let slots = parse_cosy_slots(body.get("slots"));

    // Transactional save (review #1 follow-up): capture the pre-change
    // agile scope inside the closure so a concurrent writer can't sneak a
    // scope change in between our read and our save (the previous
    // load-then-save pattern could see a stale scope and then clobber a
    // concurrent writer's update). Slots parsing happens outside the
    // closure so we only re-run the (cheap) JSON validation, not the
    // load-and-save I/O.
    let mut previous_agile_scope = None;
    let save_result = crate::settings::Settings::update(|app_settings| {
        previous_agile_scope = Some(crate::settings::agile_scope_for_settings(app_settings));
        app_settings.cosy_enabled = enabled;
        if enabled {
            app_settings.adaptive_charge_enabled = false;
            app_settings.agile_scope = crate::settings::AgileScope::Off;
            app_settings.agile_enabled = false;
        }
        if let Some(slots) = slots.clone() {
            app_settings.cosy_slots = slots;
        }
    });
    let previous_agile_scope = previous_agile_scope.expect("closure always runs");
    if let Err(e) = save_result {
        tracing::warn!("Failed to persist cosy config: {e}");
        return server_error(&format!("Failed to save: {e}"));
    }

    if enabled && previous_agile_scope != crate::settings::AgileScope::Off {
        let device_type = latest_device_type(&state).await;
        let cmd = if device_type.uses_three_phase_schedule_slots() {
            ControlCommand::ThreePhaseAgileClearActiveSlot
        } else {
            ControlCommand::AgileClearActiveSlot
        };
        if let Ok(writes) = cmd.encode() {
            tracing::info!(
                "Cosy switched on — disabling Agile and clearing its armed slot ({} register writes)",
                writes.len()
            );
            queue_owned_writes(&state, writes, DischargeControlOwner::Agile).await;
            state.write_notify.notify_one();
        }
    }

    ok_response("Cosy config updated")
}

// ---------------------------------------------------------------------------
// Agile Octopus endpoints
// ---------------------------------------------------------------------------

/// GET /api/agile — get Agile Octopus config.
async fn queue_cached_agile_action_for_settings(
    state: &Arc<AppState>,
    settings: &crate::settings::Settings,
) {
    use crate::inverter::state_machines::{evaluate_agile_slot, should_write_agile_action};

    if settings.cosy_enabled {
        return;
    }
    let scope = crate::settings::agile_scope_for_settings(settings);
    if scope == crate::settings::AgileScope::Off {
        return;
    }

    let now_ts = chrono::Utc::now().timestamp();
    let cache_snapshot = state.cached_agile_prices.lock().await.clone();
    let price = cache_snapshot
        .iter()
        .find(|s| now_ts >= s.valid_from && now_ts < s.valid_to)
        .map(|s| s.pence);
    let Some(price) = price else {
        // No current cached price — region/API-base changes deliberately clear
        // the cache and rely on the poll loop to fetch fresh prices.
        return;
    };

    let cosy_active = *state.cosy_active.lock().await;
    let auto_winter_active = state
        .latest_snapshot
        .lock()
        .await
        .as_ref()
        .map(|s| s.auto_winter_active)
        .unwrap_or(false);
    let action = evaluate_agile_slot(
        scope,
        Some(price),
        settings.agile_charge_threshold,
        settings.agile_discharge_threshold,
        &cache_snapshot,
        now_ts,
        cosy_active,
        auto_winter_active,
        &chrono::Local,
    );
    if !should_write_agile_action(scope, &action) {
        return;
    }

    let use_3ph = latest_device_type(state)
        .await
        .uses_three_phase_schedule_slots();
    let cmd = match action {
        crate::inverter::state_machines::AgileSlotAction::Charge {
            start_hhmm,
            end_hhmm,
            target_soc,
        } => {
            if use_3ph {
                ControlCommand::ThreePhaseAgileChargeSlot {
                    start_hhmm,
                    end_hhmm,
                    target_soc,
                }
            } else {
                ControlCommand::AgileChargeSlot {
                    start_hhmm,
                    end_hhmm,
                    target_soc,
                }
            }
        }
        crate::inverter::state_machines::AgileSlotAction::Discharge {
            start_hhmm,
            end_hhmm,
        } => {
            if use_3ph {
                ControlCommand::ThreePhaseAgileDischargeSlot {
                    start_hhmm,
                    end_hhmm,
                }
            } else {
                ControlCommand::AgileDischargeSlot {
                    start_hhmm,
                    end_hhmm,
                }
            }
        }
        crate::inverter::state_machines::AgileSlotAction::Idle => {
            if use_3ph {
                ControlCommand::ThreePhaseAgileClearActiveSlot
            } else {
                ControlCommand::AgileClearActiveSlot
            }
        }
        crate::inverter::state_machines::AgileSlotAction::Defer => return,
    };

    if let Ok(writes) = cmd.encode() {
        tracing::info!(
            "Agile settings save queued current-slot action ({} register writes)",
            writes.len()
        );
        queue_owned_writes(state, writes, DischargeControlOwner::Agile).await;
    }
}

pub async fn get_agile(State(_state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let settings = crate::settings::Settings::load();
    let scope = if settings.cosy_enabled {
        crate::settings::AgileScope::Off
    } else {
        crate::settings::agile_scope_for_settings(&settings)
    };
    (
        StatusCode::OK,
        Json(json!({
        "ok": true,
        "enabled": scope.is_enabled(),
        "scope": scope,
        "region": settings.agile_region,
        "charge_threshold": settings.agile_charge_threshold,
        "discharge_threshold": settings.agile_discharge_threshold,
        "api_base_url": settings.agile_api_base_url,
        })),
    )
}

/// POST /api/agile — update Agile Octopus config.
///
/// Accepts either:
///   - `{ enabled: bool, ... }` — legacy shape; `enabled=true` maps to
///     `AgileScope::Full`, `enabled=false` maps to `AgileScope::Off`.
///   - `{ scope: "off"|"full"|"charge_only"|"discharge_only", ... }` —
///     new explicit shape. Takes precedence over `enabled` when both
///     are provided.
///
/// Both shapes are accepted on the same endpoint so existing frontends
/// keep working unchanged. New frontends should send `scope` explicitly.
pub async fn set_agile(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    use crate::settings::AgileScope;

    // Compute scope-update semantics up front so the closure can apply them
    // transactionally (review #1 follow-up). The closure runs once under the
    // settings lock; it captures the pre-change scope inside the lock so a
    // concurrent writer can't sneak a scope change in between our read and
    // our save (the previous load-then-save pattern saw a stale scope and
    // could then clobber a concurrent writer's update).
    let explicit_scope = body
        .get("scope")
        .and_then(|v| v.as_str())
        .and_then(parse_agile_scope);
    let has_agile_partial_fields = body.get("charge_threshold").is_some()
        || body.get("discharge_threshold").is_some()
        || body.get("region").is_some()
        || body.get("api_base_url").is_some();
    let legacy_scope_toggle =
        explicit_scope.is_none() && body.get("enabled").is_some() && !has_agile_partial_fields;
    let scope_update_requested = explicit_scope.is_some() || legacy_scope_toggle;
    let legacy_enabled = body["enabled"].as_bool();
    let new_scope_from_legacy_toggle = legacy_scope_toggle.then(|| {
        if legacy_enabled.unwrap_or(false) {
            AgileScope::Full
        } else {
            AgileScope::Off
        }
    });
    let region = body["region"].as_str().map(|s| s.to_string());
    let charge_threshold = body["charge_threshold"].as_f64();
    let discharge_threshold = body["discharge_threshold"].as_f64();
    let api_base_url = body["api_base_url"].as_str().map(|s| s.to_string());

    // Capture the post-mutation scope, thresholds, and cosy flag inside
    // the closure so the helper below can read them without re-reading
    // disk. Re-reading disk after Settings::update would race with any
    // concurrent writer that mutated the same fields between our save
    // and our re-read, defeating the whole point of the transactional
    // helper. The closure is the only safe place to snapshot these.
    let mut snapshot = crate::settings::Settings::default();
    let mut price_source_changed_in_closure = false;
    let save_result = crate::settings::Settings::update(|app_settings| {
        let current_scope = match explicit_scope {
            Some(scope) => scope,
            None => match new_scope_from_legacy_toggle {
                Some(scope) => scope,
                None => crate::settings::agile_scope_for_settings(app_settings),
            },
        };
        app_settings.agile_scope = current_scope;
        app_settings.agile_enabled = current_scope != AgileScope::Off;
        if scope_update_requested && current_scope != AgileScope::Off {
            app_settings.cosy_enabled = false;
            app_settings.cosy_active_persisted = false;
            app_settings.adaptive_charge_enabled = false;
        }

        if let Some(r) = region.clone() {
            app_settings.agile_region = r;
            price_source_changed_in_closure = true;
        }
        if let Some(v) = charge_threshold {
            app_settings.agile_charge_threshold = v;
        }
        if let Some(v) = discharge_threshold {
            app_settings.agile_discharge_threshold = v;
        }
        if let Some(u) = api_base_url.clone() {
            app_settings.agile_api_base_url = u;
            price_source_changed_in_closure = true;
        }
        // Snapshot the fields the queue helper actually reads, scoped to
        // just the post-mutation state — no full clone of the Settings
        // struct, no race window for a concurrent writer.
        snapshot.cosy_enabled = app_settings.cosy_enabled;
        snapshot.agile_scope = app_settings.agile_scope;
        snapshot.agile_enabled = app_settings.agile_enabled;
        snapshot.agile_charge_threshold = app_settings.agile_charge_threshold;
        snapshot.agile_discharge_threshold = app_settings.agile_discharge_threshold;
    });
    if let Err(e) = save_result {
        tracing::warn!("Failed to persist agile config: {e}");
        return server_error(&format!("Failed to save: {e}"));
    }

    let price_source_changed = body.get("region").is_some()
        || body.get("api_base_url").is_some()
        || price_source_changed_in_closure;
    if price_source_changed {
        state.cached_agile_prices.lock().await.clear();
        tracing::info!("Agile price cleared after region/API-base change");
    } else {
        // Threshold-only changes can change the live 30-minute decision using
        // the currently cached price. Queue that action immediately so the
        // inverter starts/stops now, not at app restart or the next normal poll.
        // `snapshot` was captured inside the Settings::update closure above
        // — no disk re-read, no concurrent-writer race.
        queue_cached_agile_action_for_settings(&state, &snapshot).await;
    }

    // Wake the poll loop after any Agile settings save. The poll loop already
    // re-reads settings from disk every cycle, so scope/region/base-url changes
    // propagate on the next poll without a reconnect; the price cache clear
    // above forces a fresh fetch for the new source.
    state.write_notify.notify_one();

    // When the user switches Agile off, clear any slot Agile had armed so the
    // inverter stops acting on Agile's behalf — e.g. stops exporting to the
    // grid on a discharge slot. Done here, on the explicit user action, so it
    // fires deterministically once instead of relying on the poll loop (which
    // deliberately leaves the slot alone while scope is Off to preserve a
    // manual schedule the user might arm afterwards). AgileClearActiveSlot is
    // idempotent, so re-POSTing "off" just re-writes the same clear.
    //
    // Always clear: the snapshot's register flags can lag the inverter by a
    // poll cycle (the write that armed a slot may not be reflected yet), so a
    // "is anything armed?" check risks skipping the clear right after an
    // arm — leaving the sim/inverter with enable_charge stuck on. The 8
    // writes are idempotent zeros and drain once.
    if scope_update_requested && snapshot.agile_scope == AgileScope::Off {
        let device_type = latest_device_type(&state).await;
        let cmd = if device_type.uses_three_phase_schedule_slots() {
            ControlCommand::ThreePhaseAgileClearActiveSlot
        } else {
            ControlCommand::AgileClearActiveSlot
        };
        if let Ok(writes) = cmd.encode() {
            tracing::info!(
                "Agile switched off — clearing its armed slot ({} register writes)",
                writes.len()
            );
            queue_owned_writes(&state, writes, DischargeControlOwner::Agile).await;
        }
    }

    ok_response("Agile config updated")
}

/// Parse an `AgileScope` from a string. Accepts the snake_case variants
/// serialised in `AgileScope`'s serde representation. Returns `None` for
/// unknown values so the API caller can fall back to the legacy boolean
/// without erroring.
fn parse_agile_scope(s: &str) -> Option<crate::settings::AgileScope> {
    use crate::settings::AgileScope;
    match s {
        "off" => Some(AgileScope::Off),
        "full" => Some(AgileScope::Full),
        "charge_only" => Some(AgileScope::ChargeOnly),
        "discharge_only" => Some(AgileScope::DischargeOnly),
        _ => None,
    }
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

    // Transactional save (review #1 follow-up): see `set_alerts` for the
    // pattern. Snapshot the merged config, drop the in-memory lock, then
    // let Settings::update serialise the read-modify-write against any
    // other handler's save.
    let snapshot = config.clone();
    drop(config);
    if let Err(e) = crate::settings::Settings::update(|app_settings| {
        app_settings.load_limiter_enabled = snapshot.enabled;
        app_settings.load_limiter_threshold_w = snapshot.threshold_w;
        app_settings.load_limiter_trigger_delay_minutes = snapshot.trigger_delay_minutes;
        app_settings.load_limiter_start_hour = snapshot.start_hour;
        app_settings.load_limiter_start_minute = snapshot.start_minute;
        app_settings.load_limiter_end_hour = snapshot.end_hour;
        app_settings.load_limiter_end_minute = snapshot.end_minute;
    }) {
        tracing::warn!("Failed to persist load limiter config: {e}");
        return server_error(&format!("Failed to save: {e}"));
    }

    ok_response("Load limiter config updated")
}

// ---------------------------------------------------------------------------
// Discharge floor guard (developer mode only) endpoints
// ---------------------------------------------------------------------------

/// GET /api/discharge-floor — current config and runtime state.
pub async fn get_discharge_floor(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let config = state.discharge_floor_config.lock().await.clone();
    let df_state = state.discharge_floor_state.lock().await.clone();
    (
        StatusCode::OK,
        Json(json!({
        "ok": true,
        "data": {
            "config": config,
            "state": df_state,
        }
        })),
    )
}

/// POST /api/discharge-floor — update the discharge floor guard config.
///
/// Body fields are optional — only provided fields are updated.
/// Fields: `enabled` (bool), `floor_soc` (4-100).
pub async fn set_discharge_floor(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let mut candidate = state.discharge_floor_config.lock().await.clone();

    if let Some(v) = body.get("enabled").and_then(|v| v.as_bool()) {
        candidate.enabled = v;
    }
    if let Some(v) = body.get("floor_soc").and_then(|v| v.as_u64()) {
        if !(4..=100).contains(&v) {
            return error_response("floor_soc must be 4-100");
        }
        candidate.floor_soc = v as u8;
    }

    tracing::info!("Discharge floor guard config updated: {:?}", candidate);

    // Transactional save (review #1 follow-up): the beb781b "save before
    // swapping live state" ordering is preserved — Settings::update gives
    // the same atomicity guarantee against concurrent writers as it does
    // for the poll-path writers in 78ba6fd, and the live config swap
    // stays after the persist so a failed save leaves live state
    // untouched (the existing
    // `discharge_floor_config_not_mutated_when_save_fails` test pins
    // this property).
    let snapshot = candidate;
    if let Err(e) = crate::settings::Settings::update(|app_settings| {
        app_settings.discharge_floor_enabled = snapshot.enabled;
        app_settings.discharge_floor_soc = snapshot.floor_soc;
    }) {
        tracing::warn!("Failed to persist discharge floor config: {e}");
        return server_error(&format!("Failed to save: {e}"));
    }

    *state.discharge_floor_config.lock().await = snapshot;
    ok_response("Discharge floor guard config updated")
}

// ---------------------------------------------------------------------------
// Inverter temperature limiter endpoints
// ---------------------------------------------------------------------------

/// GET /api/temperature-limiter — current config and runtime state.
pub async fn get_temperature_limiter(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<Value>) {
    let config = state.temperature_limiter_config.lock().await.clone();
    let limiter_state = state.temperature_limiter_state.lock().await.clone();
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "data": { "config": config, "state": limiter_state }
        })),
    )
}

/// POST /api/temperature-limiter — update temperature protection settings.
pub async fn set_temperature_limiter(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    let mut candidate = state.temperature_limiter_config.lock().await.clone();
    if let Some(value) = body.get("enabled").and_then(|value| value.as_bool()) {
        candidate.enabled = value;
    }
    if let Some(value) = body.get("high_threshold").and_then(|value| value.as_f64()) {
        candidate.high_threshold = value as f32;
    }
    if let Some(value) = body
        .get("recovery_threshold")
        .and_then(|value| value.as_f64())
    {
        candidate.recovery_threshold = value as f32;
    }
    if let Some(value) = body
        .get("confirmation_readings")
        .and_then(|value| value.as_u64())
    {
        if value > 30 {
            return error_response("Confirmation readings must be between 1 and 30");
        }
        candidate.confirmation_readings = value as u32;
    }
    if let Err(error) = candidate.validate() {
        return error_response(&error);
    }

    // Transactional save (review #1 follow-up): the old pattern mutated
    // in-memory first then saved, so a failed save left live and persisted
    // state diverged. Now Settings::update holds the lock across the
    // read-modify-write (giving the same atomicity guarantee as the poll
    // path writers in 78ba6fd). The live config swap is intentionally
    // after the persist so a failed save leaves the live state untouched
    // — the existing discharge-floor test
    // `discharge_floor_config_not_mutated_when_save_fails` already pins
    // this property for the parallel case, and the same pattern is
    // upheld here.
    let snapshot = candidate.clone();
    if let Err(error) = crate::settings::Settings::update(|app_settings| {
        app_settings.temperature_limiter_enabled = snapshot.enabled;
        app_settings.temperature_limiter_high_threshold = snapshot.high_threshold;
        app_settings.temperature_limiter_recovery_threshold = snapshot.recovery_threshold;
        app_settings.temperature_limiter_confirmation_readings = snapshot.confirmation_readings;
    }) {
        return server_error(&format!("Failed to save: {error}"));
    }

    *state.temperature_limiter_config.lock().await = snapshot;
    ok_response("Temperature limiter config updated")
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
        percent_encoding::utf8_percent_encode(trimmed, percent_encoding::NON_ALPHANUMERIC,)
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

/// GET /api/forecast — the assembled prediction payload (issue #283).
///
/// Computes from entirely local state (stored forecast values, meta PR,
/// history counters, current snapshot, settings) — no network access on
/// the request path, so the endpoint is cheap to poll and hermetic under
/// test. Degradations are reported as status codes in the payload.
pub async fn get_forecast(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let snapshot = state.latest_snapshot.lock().await.clone();
    let history = state.history.lock().await.clone();
    let (weather_enabled, coords) = {
        let ws = state.weather.lock().await;
        (
            ws.config.enabled,
            ws.config.latitude.zip(ws.config.longitude),
        )
    };
    let settings = crate::settings::Settings::load();
    let payload = crate::forecast::build_forecast_payload(&crate::forecast::ForecastInputs {
        db: history.as_deref(),
        settings: &settings,
        snapshot: snapshot.as_ref(),
        weather_enabled,
        weather_coords: coords,
        now: chrono::Local::now(),
    });
    (StatusCode::OK, Json(json!({ "ok": true, "data": payload })))
}

/// GET /api/forecast/plan — tariff-aware overnight charge recommendation
/// (issue #283, Phase 2). Same inputs as `/api/forecast` plus the user's
/// import-tariff config; never writes any register — Apply happens only
/// from the UI through the existing control endpoints.
pub async fn get_forecast_plan(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    use chrono::Local;

    let snapshot = state.latest_snapshot.lock().await.clone();
    let history = state.history.lock().await.clone();
    let (weather_enabled, coords) = {
        let ws = state.weather.lock().await;
        (
            ws.config.enabled,
            ws.config.latitude.zip(ws.config.longitude),
        )
    };
    let settings = crate::settings::Settings::load();
    let forecast = crate::forecast::build_forecast_payload(&crate::forecast::ForecastInputs {
        db: history.as_deref(),
        settings: &settings,
        snapshot: snapshot.as_ref(),
        weather_enabled,
        weather_coords: coords,
        now: Local::now(),
    });

    let (charge, export_advice) = compute_full_plan(
        &forecast,
        &settings,
        snapshot.as_ref(),
        Local::now().timestamp(),
    );
    let rec_value = plan_to_json_value(&charge);
    let apply = rec_value
        .get("apply")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let recommendation = {
        let mut r = rec_value.as_object().cloned().unwrap_or_default();
        r.remove("apply");
        serde_json::Value::Object(r)
    };
    // The export advice has no Apply path yet (read-only v1) — when it
    // stands down with the charge plan it serialises as null so the UI
    // hides the card entirely.
    let export = export_advice
        .as_ref()
        .map(export_advice_to_json)
        .unwrap_or(serde_json::Value::Null);
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "data": {
                "recommendation": recommendation,
                "apply": apply,
                "export": export,
            }
        })),
    )
}

/// Compute the typed plan recommendation from a forecast payload, plus
/// the export advice that rides the same trajectory. Shared by the API
/// handler (which serialises both) and the poll loop's nightly
/// auto-refresh (which acts on the charge side), so the applied slot,
/// the displayed plan, and the export card can never come from
/// different computations. The export half is `None` exactly when the
/// charge half is `NoPlan` — the same gates (projection, history,
/// import tariff) underpin both.
pub(crate) fn compute_full_plan(
    forecast: &crate::forecast::ForecastPayload,
    settings: &crate::settings::Settings,
    snapshot: Option<&crate::inverter::model::InverterSnapshot>,
    now_ts: i64,
) -> (
    crate::forecast::planner::PlanRecommendation,
    Option<crate::forecast::planner::ExportAdvice>,
) {
    use crate::forecast::planner::{ExportAdvice, ExportPlanInputs, PlanRecommendation};
    let (charge, export_ctx) = compute_charge_plan_inner(forecast, settings, snapshot, now_ts);
    // NoPlan means one of the shared gates failed (projection, history,
    // import tariff) — the export advice stands down with them.
    if matches!(charge, PlanRecommendation::NoPlan { .. }) {
        return (charge, None);
    }
    let Some(ExportContext {
        sim_hours,
        params,
        import_tariff,
        floor_pct: min_soc_pct,
    }) = export_ctx
    else {
        return (charge, None);
    };
    // Without an export tariff there is no window to sell into — say so
    // rather than hiding the card, so the user knows what to configure.
    let Some(export_tariff) = settings.export_tariff_config.as_ref() else {
        return (
            charge,
            Some(ExportAdvice::NoExport {
                reason: "no export tariff configured — set your export rates in Settings so the planner can find your best window".to_string(),
            }),
        );
    };
    let charge_window = match &charge {
        crate::forecast::planner::PlanRecommendation::Charge { window, .. } => Some(window),
        _ => None,
    };
    let export = crate::forecast::planner::plan_export_window(&ExportPlanInputs {
        sim_hours: &sim_hours,
        params: &params,
        export_tariff,
        import_tariff,
        charge_window,
        floor_pct: min_soc_pct,
        now_ts,
    });
    (charge, Some(export))
}

/// Compute the typed plan recommendation from a forecast payload. Shared
/// by the API handler (which serialises it) and the poll loop's nightly
/// auto-refresh (which acts on it), so the applied slot and the displayed
/// plan can never come from different computations.
pub(crate) fn compute_plan_recommendation(
    forecast: &crate::forecast::ForecastPayload,
    settings: &crate::settings::Settings,
    snapshot: Option<&crate::inverter::model::InverterSnapshot>,
    now_ts: i64,
) -> crate::forecast::planner::PlanRecommendation {
    compute_full_plan(forecast, settings, snapshot, now_ts).0
}

/// The context the export advice needs to ride the charge plan's
/// trajectory: the reconstructed hourly inputs, the simulation params,
/// the import tariff, and the user's SOC floor.
struct ExportContext<'a> {
    sim_hours: Vec<crate::forecast::simulate::SimHourInput>,
    params: crate::forecast::simulate::SimulationParams,
    import_tariff: &'a crate::settings::TariffConfig,
    floor_pct: f64,
}

/// The charge-plan computation proper, returning the context the export
/// advice needs to ride the same trajectory. `None` when the charge
/// gates failed (no projection / no import tariff) — the export advice
/// stands down with them.
fn compute_charge_plan_inner<'a>(
    forecast: &crate::forecast::ForecastPayload,
    settings: &'a crate::settings::Settings,
    snapshot: Option<&crate::inverter::model::InverterSnapshot>,
    now_ts: i64,
) -> (
    crate::forecast::planner::PlanRecommendation,
    Option<ExportContext<'a>>,
) {
    use crate::forecast::simulate::{
        SimHourInput, SimHourResult, SimulationOutput, SimulationParams,
    };
    let import_tariff = settings.import_tariff_config.as_ref();

    // No battery projection -> straight NoPlan (and no export advice).
    let Some(battery) = &forecast.battery else {
        return (
            crate::forecast::planner::PlanRecommendation::NoPlan {
            reason: "no battery projection available — connect to the inverter and wait for forecast data".to_string(),
        },
            None,
        );
    };

    // Reserve floor + capacity from the snapshot/battery block; rate caps
    // and efficiencies from settings. We use the snapshot for live
    // capacity when present, otherwise the battery block's capacity_kwh.
    let (capacity_kwh, start_soc, reserve_soc) = match snapshot {
        Some(s) if s.battery_capacity_kwh > 0.0 => (
            s.battery_capacity_kwh as f64,
            s.soc as f64,
            s.battery_reserve as f64,
        ),
        _ => (
            battery.capacity_kwh,
            battery.start_soc_pct,
            battery.reserve_soc_pct,
        ),
    };

    // Minimum SOC across the forward window (planner v2 objective).
    let min_soc_pct = settings.forecast_min_soc_pct.clamp(0.0, 100.0);

    // Rate caps: the same model-aware limits the forecast projection
    // used, so the planner's what-if re-simulation and its
    // "deliverable in the window" clamp match the physics of the
    // original projection. Falls back to a 1C upper bound when the
    // live registers are unavailable.
    let (configured_charge_kw, max_discharge_kw) = snapshot
        .map(crate::forecast::battery_rate_limits_kw)
        .filter(|(c, d)| *c > 0.0 && *d > 0.0)
        .unwrap_or((battery.capacity_kwh, battery.capacity_kwh));
    // The applied forecast schedule explicitly sets the charge limit to
    // 100%, so duration must be calculated against the hardware maximum even
    // when the inverter is currently configured below that rate.
    let max_charge_kw = snapshot
        .map(|s| s.max_battery_power_w as f64 / 1000.0)
        .filter(|rate| *rate > 0.0)
        .unwrap_or(configured_charge_kw);
    let eta_c = settings.forecast_charge_efficiency;
    let eta_d = settings.forecast_discharge_efficiency;

    let hours: Vec<SimHourResult> = battery
        .hours
        .iter()
        .map(|(ts, soc)| SimHourResult {
            timestamp: *ts,
            soc_pct: *soc,
            import_kwh: 0.0,
            export_kwh: 0.0,
            charge_kwh: 0.0,
            discharge_kwh: 0.0,
        })
        .collect();

    // Reconstruct the original hourly inputs so the planner can re-run
    // the simulation with the proposed charge applied. Without this the
    // planner over-estimates `after_min_soc_pct` whenever the forecast
    // trough falls AFTER the charge window — intermediate discharge
    // consumes the kWh before the trough is reached, leaving SOC
    // untouched. The forecast payload exposes `solar` (per-timestamp)
    // and the weekday/weekend consumption profiles (per-hour-of-day); pair them back up so the
    // re-simulation sees the same hourly inputs the original did.
    let sim_hours: Vec<SimHourInput> = forecast
        .solar
        .iter()
        .map(|s| {
            let local = chrono::DateTime::from_timestamp(s.timestamp, 0)
                .map(|dt| dt.with_timezone(&chrono::Local));
            let local_hour = local.map(|dt| dt.hour() as usize).unwrap_or(0);
            let is_weekend = local
                .map(|dt| dt.weekday().number_from_monday() >= 6)
                .unwrap_or(false);
            let profile = if is_weekend {
                &forecast.consumption_weekend
            } else {
                &forecast.consumption_weekday
            };
            let cons = profile.get(local_hour).map(|c| c.kwh).unwrap_or(0.0);
            SimHourInput {
                timestamp: s.timestamp,
                solar_kwh: s.kwh,
                consumption_kwh: cons,
            }
        })
        .collect();

    let params = SimulationParams {
        capacity_kwh,
        start_soc_pct: start_soc,
        reserve_soc_pct: reserve_soc,
        max_charge_kw,
        max_discharge_kw,
        charge_efficiency: eta_c,
        discharge_efficiency: eta_d,
    };
    let sim = SimulationOutput {
        hours,
        total_import_kwh: forecast.import_tomorrow_kwh,
        total_export_kwh: forecast.export_tomorrow_kwh,
    };
    let inputs = crate::forecast::planner::PlanInputs {
        simulation: &sim,
        sim_hours: Some(&sim_hours),
        params: &params,
        import_tariff,
        target_soc_pct: min_soc_pct,
        consumption_tomorrow_kwh: forecast.consumption_tomorrow_kwh,
        consumption_sufficient: forecast.consumption_sufficient,
        now_ts,
        current_soc_pct: snapshot.map(|s| s.soc as f64).unwrap_or(0.0),
    };
    let charge = crate::forecast::planner::plan_overnight_charge(&inputs);
    // The export advice rides the same trajectory: hand over the hourly
    // inputs, params, tariff, and floor. `None` when there is no import
    // tariff — the charge planner has already NoPlan'd on that gate.
    let ctx = import_tariff.map(|t| ExportContext {
        sim_hours,
        params,
        import_tariff: t,
        floor_pct: min_soc_pct,
    });
    (charge, ctx)
}

/// Serialise a `PlanRecommendation` into the JSON shape the UI/Apply
/// path expects.
fn plan_to_json_value(rec: &crate::forecast::planner::PlanRecommendation) -> serde_json::Value {
    use crate::forecast::planner::PlanRecommendation;
    match rec {
        PlanRecommendation::NoChargeNeeded {
            current_soc_pct,
            min_soc_pct,
            observed_min_soc_pct,
        } => serde_json::json!({
            "kind": "no_charge_needed",
            "min_soc_pct": min_soc_pct,
            "observed_min_soc_pct": observed_min_soc_pct,
            "current_soc_pct": current_soc_pct,
            "rationale": format!(
                "Battery is at {:.0}% now and the forecast trough stays above your {:.0}% minimum — nothing to schedule.",
                current_soc_pct,
                min_soc_pct
            ),
            "apply": null,
        }),
        PlanRecommendation::Charge {
            window,
            kwh,
            min_soc_pct,
            observed_min_soc_pct,
            after_min_soc_pct,
            current_soc_pct,
            rationale,
            with_charge_series,
            import_tomorrow_with_charge_kwh,
            export_tomorrow_with_charge_kwh,
        } => {
            // The slot's wall-clock bounds, with the same 23:59 clamp the
            // nightly auto-refresh uses (a 00:00 end would decode as a
            // disabled slot).
            let (start_h, start_m, end_h, end_m) = crate::forecast::refresh::plan_slot_hhmm(window);
            // The slot's charge target is always 100 by design in the v2
            // one-cycle sizing (CODE_REVIEW.md Minor 6): the DURATION is
            // the control variable, and the rate-limit register (HR 111 /
            // HR 313 / HR 1110) governs the charge. A slot targeted at the
            // min-soc floor would stop charging as soon as it's touched,
            // and the rest of the day's drain would pull the battery right
            // back below the floor (the exact "still drops to 4%" failure
            // the planner exists to prevent).
            let slot_target_soc = 100u64;
            // The "if we follow the plan" trajectory, in the same
            // `[timestamp_unix, soc_pct]` shape as the forecast's
            // `battery.hours` so the Forecast tab's Battery projection
            // chart can draw it as a dashed overlay line without
            // coordinate work.
            serde_json::json!({
                "kind": "charge",
                "window": {
                    "start": format!("{:02}:{:02}", start_h, start_m),
                    "end": format!("{:02}:{:02}", end_h, end_m),
                    "rate": window.rate,
                    "tomorrow": window.tomorrow,
                },
                "kwh": kwh,
                "min_soc_pct": min_soc_pct,
                "observed_min_soc_pct": observed_min_soc_pct,
                "after_min_soc_pct": after_min_soc_pct,
                "current_soc_pct": current_soc_pct,
                "rationale": rationale,
                "with_charge_series": with_charge_series,
                // Tomorrow's import/export under the plan — the same
                // numbers the Tomorrow tiles show, so the tiles and the
                // plan card can never disagree (the live report was an
                // Expected Import tile at 0.3 kWh against a 6.2 kWh
                // charge plan because the tile read the uncharged
                // simulation).
                "import_tomorrow_with_charge_kwh": import_tomorrow_with_charge_kwh,
                "export_tomorrow_with_charge_kwh": export_tomorrow_with_charge_kwh,
                "apply": {
                    "charge_slot": {
                        "slot": 1,
                        "enabled": true,
                        "start_hour": start_h,
                        "start_minute": start_m,
                        "end_hour": end_h,
                        "end_minute": end_m,
                        "target_soc": slot_target_soc,
                        "charge_rate_percent": crate::forecast::refresh::PLAN_CHARGE_RATE_PERCENT,
                    },
                    "timed_charge": { "enabled": true },
                },
            })
        }
        PlanRecommendation::NoPlan { reason } => serde_json::json!({
            "kind": "no_plan",
            "reason": reason,
            "apply": null,
        }),
    }
}

/// Serialise the export advice (issue #283 stage 2) into the JSON shape
/// the UI expects. Deliberately no `apply` payload — export advice is
/// read-only in v1; scheduling a discharge slot comes later.
fn export_advice_to_json(advice: &crate::forecast::planner::ExportAdvice) -> serde_json::Value {
    use crate::forecast::planner::ExportAdvice;
    match advice {
        ExportAdvice::Export {
            window,
            kwh,
            min_soc_pct,
            after_min_soc_pct,
            earning,
            rationale,
            with_export_series,
        } => {
            // Same wall-clock clamp as the charge window: a 00:00 end
            // would decode as a disabled slot on the inverter.
            let (start_h, start_m, end_h, end_m) = crate::forecast::refresh::plan_slot_hhmm(window);
            serde_json::json!({
                "kind": "export",
                "window": {
                    "start": format!("{:02}:{:02}", start_h, start_m),
                    "end": format!("{:02}:{:02}", end_h, end_m),
                    "rate": window.rate,
                    "tomorrow": window.tomorrow,
                },
                "kwh": kwh,
                "min_soc_pct": min_soc_pct,
                "after_min_soc_pct": after_min_soc_pct,
                "earning": earning,
                "rationale": rationale,
                "with_export_series": with_export_series,
            })
        }
        ExportAdvice::NoExport { reason } => serde_json::json!({
            "kind": "no_export",
            "reason": reason,
        }),
    }
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
    if !ws.config.postcode.is_empty()
        && (ws.config.latitude.is_none() || ws.config.longitude.is_none())
    {
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
    // Transactional save (review #1 follow-up): the whole
    // read-modify-write happens under the settings lock.
    let snapshot = config_clone;
    if let Err(e) = crate::settings::Settings::update(|app_settings| {
        app_settings.weather_config = snapshot.clone();
    }) {
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

    /// Construct a minimal `AppState` for use in endpoint tests that
    /// don't exercise the poll loop or websocket layer. The settings
    /// are loaded fresh per call so each test sees its own isolated
    /// config dir (see `with_isolated_config_dir_async`).
    fn test_state() -> Arc<crate::AppState> {
        Arc::new(crate::AppState::new())
    }

    fn slot(enabled: bool, start_hour: u8, end_hour: u8) -> crate::inverter::model::ScheduleSlot {
        crate::inverter::model::ScheduleSlot {
            enabled,
            start_hour,
            start_minute: 0,
            end_hour,
            end_minute: 0,
            target_soc: 100,
        }
    }

    /// CODE_REVIEW.md finding 2: the persisted desired schedule maps array
    /// position → slot number, so an edit beyond the current length must be
    /// padded to its index — appending shifts the edit onto the wrong
    /// physical slot (reachable via the short legacy backup path).
    #[test]
    fn merge_desired_slot_places_edits_at_their_slot_index() {
        let mut desired = vec![slot(true, 16, 19)];

        // Slot 3 (index 2) edited while only slot 1 is persisted.
        merge_desired_slot(&mut desired, 3, slot(true, 20, 22));

        assert_eq!(desired.len(), 3);
        // Index 0 untouched.
        assert_eq!((desired[0].start_hour, desired[0].end_hour), (16, 19));
        // Index 1 must be an unconfigured pad, NOT the slot-3 edit.
        assert!(!desired[1].enabled);
        // The edit lands at index 2.
        assert_eq!((desired[2].start_hour, desired[2].end_hour), (20, 22));
        assert!(desired[2].enabled);
    }

    #[test]
    fn merge_desired_slot_replaces_in_place_when_index_exists() {
        let mut desired = vec![slot(true, 16, 19), slot(true, 20, 22)];
        merge_desired_slot(&mut desired, 2, slot(false, 0, 0));
        assert_eq!(desired.len(), 2);
        assert!(!desired[1].enabled);
        assert_eq!((desired[0].start_hour, desired[0].end_hour), (16, 19));
    }

    #[tokio::test]
    async fn discharge_floor_config_round_trips_and_validates_floor_soc() {
        with_isolated_config_dir_async(|| async {
            let state = test_state();

            // Enable with a valid floor.
            let (status, body) = set_discharge_floor(
                State(state.clone()),
                Json(json!({ "enabled": true, "floor_soc": 50 })),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["ok"], true);
            {
                let config = state.discharge_floor_config.lock().await;
                assert!(config.enabled);
                assert_eq!(config.floor_soc, 50);
            }
            // Persisted to settings.json.
            let settings = crate::settings::Settings::load();
            assert!(settings.discharge_floor_enabled);
            assert_eq!(settings.discharge_floor_soc, 50);

            // Out-of-range floor is rejected.
            let (status, body) =
                set_discharge_floor(State(state.clone()), Json(json!({ "floor_soc": 3 }))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(body["ok"], false);
            let (status, _) =
                set_discharge_floor(State(state.clone()), Json(json!({ "floor_soc": 101 }))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);

            // GET returns the live config and state.
            let (status, body) = get_discharge_floor(State(state)).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["data"]["config"]["enabled"], true);
            assert_eq!(body["data"]["config"]["floor_soc"], 50);
            assert_eq!(body["data"]["state"], "Idle");
        })
        .await;
    }

    #[tokio::test]
    async fn discharge_floor_config_not_mutated_when_save_fails() {
        with_isolated_config_dir_async(|| async {
            let state = test_state();
            let config_before = state.discharge_floor_config.lock().await.clone();

            // Make the settings save fail by making the config dir read-only
            // (the temp-file write inside `Settings::save` needs write access).
            let dir = crate::settings::Settings::settings_dir();
            let perms = std::fs::metadata(&dir)
                .expect("isolated config dir exists")
                .permissions();
            let mut readonly = perms.clone();
            std::os::unix::fs::PermissionsExt::set_mode(&mut readonly, 0o500);
            std::fs::set_permissions(&dir, readonly).expect("make config dir read-only");

            let (status, body) = set_discharge_floor(
                State(state.clone()),
                Json(json!({ "enabled": true, "floor_soc": 50 })),
            )
            .await;

            // Restore write access so the isolation guard can clean up.
            std::fs::set_permissions(&dir, perms).expect("restore config dir permissions");

            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(body["ok"], false);
            // The live config must NOT have diverged from the persisted settings.
            let config_after = state.discharge_floor_config.lock().await.clone();
            assert_eq!(config_after.enabled, config_before.enabled);
            assert_eq!(config_after.floor_soc, config_before.floor_soc);
            let settings = crate::settings::Settings::load();
            assert_eq!(settings.discharge_floor_enabled, config_before.enabled);
            assert_eq!(settings.discharge_floor_soc, config_before.floor_soc);
        })
        .await;
    }

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
            HR_3PH_DISCHARGE_SLOT_2_END, HR_3PH_DISCHARGE_SLOT_2_START, HR_DISCHARGE_SLOT_10_END,
            HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_END,
            HR_DISCHARGE_SLOT_2_START, HR_DISCHARGE_SLOT_3_START, SAFE_WRITE_REGS,
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

        // Three-phase: slots 1–2 via HR 1118-1121 plus the extended
        // slots 3–10 bank (HR 276-298) — 10 slots x start/end.
        let writes = clear_discharge_slot_writes(DeviceType::ThreePhase);
        assert_eq!(writes.len(), 20, "three-phase clears 10 slots x start/end");
        for w in &writes {
            assert_eq!(w.value, 0);
            assert!(SAFE_WRITE_REGS.contains(&w.address));
        }
        let addrs: Vec<u16> = writes.iter().map(|w| w.address).collect();
        assert!(addrs.contains(&HR_3PH_DISCHARGE_SLOT_1_START));
        assert!(addrs.contains(&HR_3PH_DISCHARGE_SLOT_1_END));
        assert!(addrs.contains(&HR_3PH_DISCHARGE_SLOT_2_START));
        assert!(addrs.contains(&HR_3PH_DISCHARGE_SLOT_2_END));
        assert!(addrs.contains(&HR_DISCHARGE_SLOT_3_START));
        assert!(addrs.contains(&HR_DISCHARGE_SLOT_10_END));
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
            // Keep API time-window tests hermetic. Production code uses the
            // inverter clock when present, so tests should exercise that path
            // instead of depending on the host wall clock.
            inverter_time: "2026-06-28 17:00:00".to_string(),
            ..Default::default()
        };
        *state.latest_snapshot.lock().await = Some(snapshot);
        // Tests using this helper set a snapshot, implying a successful poll —
        // model a connected inverter so the pause/unpause connection guard
        // (issue #245) doesn't short-circuit these exercises.
        *state.connection_state.lock().await = ConnectionState::Connected;
        state
    }

    /// Like [`make_state_with_device`] but also sets the ARM firmware version
    /// string, so firmware-gated capability checks (e.g. Gen3 Hybrid's
    /// ARM fw >= 312 threshold for Timed Discharge) can be exercised.
    async fn make_state_with_device_and_fw(device_type: DeviceType, arm_fw: u16) -> Arc<AppState> {
        let state = Arc::new(AppState::new());
        let snapshot = crate::inverter::model::InverterSnapshot {
            device_type,
            firmware_version: arm_fw.to_string(),
            inverter_time: "2026-06-28 17:00:00".to_string(),
            ..Default::default()
        };
        *state.latest_snapshot.lock().await = Some(snapshot);
        *state.connection_state.lock().await = ConnectionState::Connected;
        state
    }

    /// Drain the pending-writes queue and flatten the batches into one vec.
    async fn drain_pending_writes(state: &Arc<AppState>) -> Vec<RegisterWrite> {
        let mut pw = state.pending_writes.lock().await;
        let batches = std::mem::take(&mut *pw);
        drop(pw);
        batches.into_iter().flat_map(|b| b.writes).collect()
    }

    /// Fixed window containing the fixed 17:00 inverter clock above. Issue
    /// #289 made immediate export arming conditional on the save happening
    /// inside the slot window; keeping both values fixed prevents these API
    /// tests from changing behaviour with the time of day or CI timezone.
    fn window_containing_now() -> (u8, u8, u8, u8) {
        (16, 0, 19, 0)
    }

    /// Run [`pause_battery`] against `state` while driving the write-
    /// completion channel that the real poll loop would resolve. The handler
    /// now awaits write confirmation (so it can surface register-rejection
    /// errors); without a poll loop in unit tests it would otherwise block
    /// for `WRITE_COMPLETION_TIMEOUT`. This signals `WriteOutcome::Ok` the
    /// moment the batch lands, leaving the batch in the queue so the test's
    /// subsequent [`drain_pending_writes`] still observes the queued writes.
    /// Returns the same `(StatusCode, Json<Value>)` as `pause_battery` so it
    /// is a drop-in replacement at each call site.
    async fn drive_pause_battery_completion(state: &Arc<AppState>) -> (StatusCode, Json<Value>) {
        drive_pause_battery_completion_with(state, WriteOutcome::Ok).await
    }

    /// Like [`drive_pause_battery_completion`] but lets the test choose the
    /// outcome the poll loop would report (e.g. a rejected register), so the
    /// handler's error-surfacing path can be exercised.
    async fn drive_pause_battery_completion_with(
        state: &Arc<AppState>,
        outcome: WriteOutcome,
    ) -> (StatusCode, Json<Value>) {
        let handle = tokio::spawn(pause_battery(State(state.clone())));
        loop {
            if handle.is_finished() {
                // Handler returned without queuing (validation-error path).
                break;
            }
            let mut pw = state.pending_writes.lock().await;
            if let Some(batch) = pw.iter_mut().find(|batch| batch.completion.is_some()) {
                if let Some(tx) = batch.completion.take() {
                    let _ = tx.send(outcome.clone());
                    drop(pw);
                    break;
                }
            }
            drop(pw);
            tokio::task::yield_now().await;
        }
        handle.await.unwrap()
    }

    /// Like [`drive_pause_battery_completion`] but for [`unpause_battery`]. The
    /// handler now also awaits write confirmation (issue #245 follow-up) so
    /// it can surface register-rejection errors the same way pause does.
    async fn drive_unpause_battery_completion(state: &Arc<AppState>) -> (StatusCode, Json<Value>) {
        drive_unpause_battery_completion_with(state, WriteOutcome::Ok).await
    }

    /// Like [`drive_pause_battery_completion_with`] but for [`unpause_battery`].
    async fn drive_unpause_battery_completion_with(
        state: &Arc<AppState>,
        outcome: WriteOutcome,
    ) -> (StatusCode, Json<Value>) {
        let handle = tokio::spawn(unpause_battery(State(state.clone())));
        loop {
            if handle.is_finished() {
                // Handler returned without queuing (validation-error path or
                // connection-refused guard).
                break;
            }
            let mut pw = state.pending_writes.lock().await;
            if let Some(batch) = pw.iter_mut().find(|batch| batch.completion.is_some()) {
                if let Some(tx) = batch.completion.take() {
                    let _ = tx.send(outcome.clone());
                    drop(pw);
                    break;
                }
            }
            drop(pw);
            tokio::task::yield_now().await;
        }
        handle.await.unwrap()
    }

    /// Like [`drive_pause_battery_completion_with`] but for
    /// [`set_timed_export`]. The handler awaits write confirmation when a
    /// discharge-slot backup is restored, so the test must drive the
    /// completion channel the poll loop would normally resolve.
    async fn drive_set_timed_export_completion_with(
        state: &Arc<AppState>,
        body: serde_json::Value,
        outcome: WriteOutcome,
    ) -> (StatusCode, Json<Value>) {
        let handle = tokio::spawn(set_timed_export(State(state.clone()), Json(body)));
        let mut completion_seen = false;
        loop {
            if handle.is_finished() {
                break;
            }
            let mut pw = state.pending_writes.lock().await;
            if let Some(batch) = pw.iter_mut().find(|batch| batch.completion.is_some()) {
                if let Some(tx) = batch.completion.take() {
                    let _ = tx.send(outcome.clone());
                    completion_seen = true;
                }
            }
            drop(pw);
            if completion_seen {
                // A successful first phase can cause the handler to enqueue
                // and await a second completion-backed phase (the export-arm
                // writes). Keep polling until the handler actually returns.
                completion_seen = false;
            }
            tokio::task::yield_now().await;
        }
        handle.await.unwrap()
    }

    /// Drive a handler with a distinct outcome for each completion-backed
    /// batch. This keeps the two-phase Timed Export tests deterministic when
    /// the slot phase succeeds but the later arm phase fails.
    async fn drive_set_timed_export_completion_sequence(
        state: &Arc<AppState>,
        body: serde_json::Value,
        outcomes: &[WriteOutcome],
    ) -> (StatusCode, Json<Value>) {
        let handle = tokio::spawn(set_timed_export(State(state.clone()), Json(body)));
        let mut next_outcome = 0;
        loop {
            if handle.is_finished() {
                break;
            }
            let mut pw = state.pending_writes.lock().await;
            if let Some(batch) = pw.iter_mut().find(|batch| batch.completion.is_some()) {
                if let Some(tx) = batch.completion.take() {
                    let outcome = outcomes
                        .get(next_outcome)
                        .or_else(|| outcomes.last())
                        .cloned()
                        .unwrap_or(WriteOutcome::Ok);
                    next_outcome += 1;
                    let _ = tx.send(outcome);
                }
            }
            drop(pw);
            tokio::task::yield_now().await;
        }
        handle.await.unwrap()
    }

    /// Like [`drive_set_timed_export_completion_with`] but for
    /// [`set_discharge_slot`]. The arming path awaits the slot-write batch
    /// before queueing the export-arm writes, so the test must resolve the
    /// completion channel the poll loop would normally drive.
    async fn drive_set_discharge_slot_completion_with(
        state: &Arc<AppState>,
        body: serde_json::Value,
        outcome: WriteOutcome,
    ) -> (StatusCode, Json<Value>) {
        let handle = tokio::spawn(set_discharge_slot(State(state.clone()), Json(body)));
        let mut completion_seen = false;
        loop {
            if handle.is_finished() {
                break;
            }
            let mut pw = state.pending_writes.lock().await;
            if let Some(batch) = pw.iter_mut().find(|batch| batch.completion.is_some()) {
                if let Some(tx) = batch.completion.take() {
                    let _ = tx.send(outcome.clone());
                    completion_seen = true;
                }
            }
            drop(pw);
            if completion_seen {
                // Continue for the second, arm-phase completion when the
                // save is inside the current window.
                completion_seen = false;
            }
            tokio::task::yield_now().await;
        }
        handle.await.unwrap()
    }

    /// Like [`drive_set_discharge_slot_completion_with`] but with a distinct
    /// outcome per completion-backed batch (last outcome repeats). Required
    /// for the compensation flows: a rejected save batch is followed by a
    /// compensating restore batch whose outcome the test controls
    /// separately.
    async fn drive_set_discharge_slot_completion_sequence(
        state: &Arc<AppState>,
        body: serde_json::Value,
        outcomes: &[WriteOutcome],
    ) -> (StatusCode, Json<Value>) {
        let handle = tokio::spawn(set_discharge_slot(State(state.clone()), Json(body)));
        let mut next_outcome = 0;
        loop {
            if handle.is_finished() {
                break;
            }
            let mut pw = state.pending_writes.lock().await;
            if let Some(batch) = pw.iter_mut().find(|batch| batch.completion.is_some()) {
                if let Some(tx) = batch.completion.take() {
                    let outcome = outcomes
                        .get(next_outcome)
                        .or_else(|| outcomes.last())
                        .cloned()
                        .unwrap_or(WriteOutcome::Ok);
                    next_outcome += 1;
                    let _ = tx.send(outcome);
                }
            }
            drop(pw);
            tokio::task::yield_now().await;
        }
        handle.await.unwrap()
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
                HR_3PH_CHARGE_SLOT_1_START, HR_3PH_FORCE_CHARGE_ENABLE, HR_CHARGE_SLOT_1_END,
                HR_CHARGE_SLOT_1_START, HR_CHARGE_TARGET_SOC, HR_ENABLE_CHARGE,
            };
            let state = make_state_with_device(DeviceType::Gateway).await;

            let (status, body) = force_charge(
                State(state.clone()),
                Some(Json(serde_json::json!({"minutes": 30}))),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body.0["ok"], serde_json::Value::Bool(true));

            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            // Single-phase charge registers must be written.
            let slot_start = writes.iter().find(|w| w.address == HR_CHARGE_SLOT_1_START);
            assert!(
                slot_start.is_some(),
                "Gateway must write HR 94 (charge slot 1 start)"
            );
            let _ = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_SLOT_1_END)
                .expect("Gateway must write HR 95 (charge slot 1 end)");
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_ENABLE_CHARGE)
                    .map(|w| w.value),
                Some(1),
                "Gateway must set HR 96 (enable_charge)",
            );
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_CHARGE_TARGET_SOC)
                    .map(|w| w.value),
                Some(100),
                "Gateway must write HR 116 (charge target SOC)",
            );
            // The three-phase control bank must NOT be touched.
            assert!(
                writes
                    .iter()
                    .all(|w| w.address != HR_3PH_CHARGE_SLOT_1_START),
                "Gateway must NOT write HR 1113 (three-phase charge slot)",
            );
            assert!(
                writes
                    .iter()
                    .all(|w| w.address != HR_3PH_FORCE_CHARGE_ENABLE),
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
                HR_3PH_FORCE_DISCHARGE_ENABLE, HR_DISCHARGE_SLOT_1_START, HR_ENABLE_DISCHARGE,
            };
            let state = make_state_with_device(DeviceType::Gateway).await;

            let (status, body) = force_discharge(
                State(state.clone()),
                Some(Json(serde_json::json!({"minutes": 30}))),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body.0["ok"], serde_json::Value::Bool(true));

            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_1_START),
                "Gateway must write HR 56 (discharge slot 1 start)",
            );
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_ENABLE_DISCHARGE)
                    .map(|w| w.value),
                Some(1),
                "Gateway must set HR 59 (enable_discharge)",
            );
            assert!(
                writes
                    .iter()
                    .all(|w| w.address != HR_3PH_FORCE_DISCHARGE_ENABLE),
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
            assert!(
                per_slot.is_some(),
                "extended-slot model must write per-slot HR 242"
            );
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
            assert!(
                global.is_none(),
                "HR 116 must NOT be written when target_soc=100"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn charge_slot_can_request_maximum_rate_for_duration_based_plans() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_BATTERY_CHARGE_LIMIT;
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 2, "start_minute": 0,
                "end_hour": 3, "end_minute": 36,
                "enabled": true,
                "target_soc": 100,
                "charge_rate_percent": 100,
            });

            let (status, _) = set_charge_slot(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let writes = drain_pending_writes(&state).await;
            let rate = writes
                .iter()
                .find(|w| w.address == HR_BATTERY_CHARGE_LIMIT)
                .expect("duration-based plan must set the DC charge limit");
            assert_eq!(rate.value, 50, "100% display rate maps to raw HR111=50");
        })
        .await;
    }

    /// CODE_REVIEW.md Minor 1: the `charge_rate_percent` branch must pick
    /// the right register per device family. AC-coupled uses HR 313 with a
    /// direct 1–100 scale (no half-scale division).
    #[tokio::test]
    async fn charge_slot_maximum_rate_writes_ac_charge_limit_for_ac_coupled() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_AC_BATTERY_CHARGE_LIMIT;
            let state = make_state_with_device(DeviceType::ACCoupled).await;
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 2, "start_minute": 0,
                "end_hour": 3, "end_minute": 36,
                "enabled": true,
                "target_soc": 100,
                "charge_rate_percent": 100,
            });

            let (status, _) = set_charge_slot(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let writes = drain_pending_writes(&state).await;
            let rate = writes
                .iter()
                .find(|w| w.address == HR_AC_BATTERY_CHARGE_LIMIT)
                .expect("AC-coupled must set the AC charge limit (HR 313)");
            assert_eq!(rate.value, 100, "AC-coupled HR313 is a direct 1-100 scale");
        })
        .await;
    }

    /// CODE_REVIEW.md Minor 1: three-phase uses HR 1110 with a direct
    /// 1–100 scale.
    #[tokio::test]
    async fn charge_slot_maximum_rate_writes_three_phase_charge_limit() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_3PH_BATTERY_CHARGE_LIMIT;
            let state = make_state_with_device(DeviceType::ThreePhase).await;
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 2, "start_minute": 0,
                "end_hour": 3, "end_minute": 36,
                "enabled": true,
                "target_soc": 100,
                "charge_rate_percent": 100,
            });

            let (status, _) = set_charge_slot(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let writes = drain_pending_writes(&state).await;
            let rate = writes
                .iter()
                .find(|w| w.address == HR_3PH_BATTERY_CHARGE_LIMIT)
                .expect("three-phase must set the three-phase charge limit (HR 1110)");
            assert_eq!(
                rate.value, 100,
                "three-phase HR1110 is a direct 1-100 scale"
            );
        })
        .await;
    }

    /// CODE_REVIEW.md Major 2: the auto-refresh's exact write-batch shape
    /// (`build_charge_slot_writes` with `SLOT_TARGET_SOC_NONE` + a
    /// `PLAN_CHARGE_RATE_PERCENT` request) must re-assert the charge-limit
    /// register on EVERY refresh — the "100% for the planned duration"
    /// contract depends on the rate being rewritten each cycle, not just
    /// the first night. Flipping the limit between refreshes is therefore
    /// corrected by the next refresh.
    #[test]
    fn forecast_refresh_batch_reasserts_the_rate_limit() {
        use crate::forecast::refresh::{PLAN_CHARGE_RATE_PERCENT, SLOT_TARGET_SOC_NONE};
        use crate::modbus::registers::HR_BATTERY_CHARGE_LIMIT;
        // The exact call the poll loop makes for a WriteSlot action.
        let writes = crate::server::api::build_charge_slot_writes(
            DeviceType::Gen2Hybrid,
            1,
            true,
            200,
            336,
            SLOT_TARGET_SOC_NONE,
            Some(PLAN_CHARGE_RATE_PERCENT),
        )
        .expect("refresh slot writes must encode");
        let rate = writes
            .iter()
            .find(|w| w.address == HR_BATTERY_CHARGE_LIMIT)
            .expect("every refresh batch must re-assert the charge limit");
        assert_eq!(rate.value, 50, "100% display rate maps to raw HR111=50");
        // And the ClearSlot shape must NOT touch the rate register — it
        // only clears the slot window and disarms.
        let clear = crate::server::api::build_charge_slot_writes(
            DeviceType::Gen2Hybrid,
            1,
            false,
            0,
            0,
            SLOT_TARGET_SOC_NONE,
            None,
        )
        .expect("refresh clear writes must encode");
        assert!(
            !clear.iter().any(|w| w.address == HR_BATTERY_CHARGE_LIMIT),
            "clearing the slot must not touch the charge-limit register"
        );
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
            use crate::modbus::registers::{HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START};
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 16, "start_minute": 0,
                "end_hour": 19, "end_minute": 0,
                "enabled": true,
            });
            let _ = drive_set_discharge_slot_completion_with(&state, body, WriteOutcome::Ok).await;
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
            use crate::modbus::registers::{HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START};
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 16, "start_minute": 0,
                "end_hour": 19, "end_minute": 0,
                "enabled": false,
            });
            let _ = drive_set_discharge_slot_completion_with(&state, body, WriteOutcome::Ok).await;
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
                HR_CHARGE_SLOT_1_START, HR_CHARGE_TARGET_SOC, HR_ENABLE_CHARGE,
                HR_ENABLE_CHARGE_TARGET,
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
                HR_CHARGE_SLOT_1_START, HR_CHARGE_TARGET_SOC, HR_ENABLE_CHARGE,
                HR_ENABLE_CHARGE_TARGET,
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
            assert!(
                global.is_none(),
                "HR 116 must NOT be written when target_soc=100"
            );
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

    /// AC-coupled: a charge-slot POST that omits `target_soc` must NOT
    /// silently default to 100 ("no limit") and disarm an armed target.
    ///
    /// Regression test for the 0.74.x field report: an automated re-post of
    /// the slot (e.g. schedule round-trip) without `target_soc` in the body
    /// cleared HR 20 and left HR 116 inert, so an armed 99% target became
    /// "charge to full" — the battery charged to 100% overnight. The armed
    /// target from the latest snapshot must be preserved instead.
    #[tokio::test]
    async fn charge_slot_omitted_target_soc_preserves_armed_target() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_CHARGE_SLOT_1_START, HR_CHARGE_TARGET_SOC, HR_ENABLE_CHARGE_TARGET,
            };
            // Seed a connected AC-coupled inverter with HR 116 armed at 99%.
            let state = Arc::new(AppState::new());
            {
                let mut snap = state.latest_snapshot.lock().await;
                let s = snap.get_or_insert_with(Default::default);
                s.device_type = DeviceType::ACCoupled;
                s.target_soc = 99;
            }
            *state.connection_state.lock().await = ConnectionState::Connected;

            // POST the slot WITHOUT `target_soc` — the automated round-trip.
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 23, "start_minute": 30,
                "end_hour": 5, "end_minute": 30,
                "enabled": true,
            });
            let _ = set_charge_slot(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);

            let start = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_SLOT_1_START)
                .expect("charge slot must write HR 94");
            assert_eq!(start.value, 2330);

            let target = writes
                .iter()
                .find(|w| w.address == HR_CHARGE_TARGET_SOC)
                .expect("omitted target_soc must preserve the armed HR 116 target");
            assert_eq!(
                target.value, 99,
                "armed 99% target must be re-written, not clobbered to no-limit"
            );
            let flag = writes
                .iter()
                .find(|w| w.address == HR_ENABLE_CHARGE_TARGET)
                .expect("charge target flag must be armed (HR 20 = 1)");
            assert_eq!(flag.value, 1);
        })
        .await;
    }

    /// An omitted `soc_reserve` must NOT reset the user's configured reserve.
    ///
    /// Same bug class as the 0.74.10 charge-slot fix: `set_mode` defaulted an
    /// omitted `soc_reserve` to 4% and every mode command writes HR 110
    /// unconditionally, so any client re-posting a mode without the field
    /// silently dragged the battery reserve down to 4%. The snapshot's
    /// configured reserve must be preserved instead; an explicit value still
    /// wins (pinned by `set_eco_mode_only_emits_whitelisted_writes`).
    #[tokio::test]
    async fn set_mode_omitted_soc_reserve_preserves_configured_reserve() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_BATTERY_SOC_RESERVE;
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            {
                let mut snap = state.latest_snapshot.lock().await;
                let s = snap.get_or_insert_with(Default::default);
                s.device_type = DeviceType::Gen2Hybrid;
                s.battery_reserve = 30;
            }

            // POST the mode WITHOUT `soc_reserve` — the round-trip shape.
            let body = serde_json::json!({ "mode": "eco" });
            let _ = set_mode(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);

            let reserve = writes
                .iter()
                .find(|w| w.address == HR_BATTERY_SOC_RESERVE)
                .expect("mode command must write the SOC reserve (HR 110)");
            assert_eq!(
                reserve.value, 30,
                "omitted soc_reserve must preserve the configured 30% reserve, not reset it to 4%"
            );
        })
        .await;
    }

    /// An omitted `target_soc` on a discharge slot POST must NOT reset the
    /// slot's configured per-slot discharge floor to the inert 100 default.
    ///
    /// Same omit-default clobber class as the charge-slot fix: on
    /// extended-slot models (Gen3/AIO) the handler defaulted an omitted
    /// `target_soc` to 100 and wrote it to HR 272/275 — a slot that "never
    /// discharges". A client re-posting the slot without the field silently
    /// neutered the user's configured floor. The snapshot's per-slot target
    /// must be preserved instead; an explicit value always wins.
    #[tokio::test]
    async fn discharge_slot_omitted_target_soc_preserves_configured_floor() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_DISCHARGE_SLOT_1_START;
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            {
                let mut snap = state.latest_snapshot.lock().await;
                let s = snap.get_or_insert_with(Default::default);
                s.device_type = DeviceType::Gen3Hybrid;
                s.discharge_slots[0] = crate::inverter::model::ScheduleSlot {
                    enabled: true,
                    start_hour: 16,
                    start_minute: 0,
                    end_hour: 19,
                    end_minute: 0,
                    target_soc: 20,
                };
            }

            // POST the slot WITHOUT `target_soc` — the round-trip shape.
            let body = serde_json::json!({
                "slot": 1,
                "start_hour": 16, "start_minute": 0,
                "end_hour": 19, "end_minute": 0,
                "enabled": true,
            });
            let _ = drive_set_discharge_slot_completion_with(&state, body, WriteOutcome::Ok).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);

            let start = writes
                .iter()
                .find(|w| w.address == HR_DISCHARGE_SLOT_1_START)
                .expect("discharge slot must write HR 44");
            assert_eq!(start.value, 1600);

            // HR 272 (slot 1 discharge floor) must echo the configured 20%,
            // not the old inert 100% omit-default.
            let floor = writes
                .iter()
                .find(|w| w.address == 272)
                .expect("extended-slot models must write the per-slot discharge floor (HR 272)");
            assert_eq!(
                floor.value, 20,
                "omitted target_soc must preserve the configured 20% floor, not reset it to 100"
            );
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

    // -- Split control register contracts (issue #131 follow-up / portal parity) ----

    /// Enabling Eco via the independent toggle must mirror `set_mode({ mode:
    /// "eco" })`: clear HR59, zero discharge slot registers (Gen3 firmware
    /// re-asserts HR59 when slots are non-zero), then restore self-consumption
    /// (HR27=1). The HR59 clear must come *before* the slot clears, and HR27
    /// must come *after*, so the inverter never sees an armed export schedule
    /// during the transition.
    #[tokio::test]
    async fn set_eco_enable_clears_slots_and_hr59_before_hr27() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START,
                HR_DISCHARGE_SLOT_2_END, HR_DISCHARGE_SLOT_2_START, HR_ENABLE_DISCHARGE,
            };
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            state
                .latest_snapshot
                .lock()
                .await
                .as_mut()
                .unwrap()
                .enable_discharge = true;
            let body = serde_json::json!({ "enabled": true });
            let _ = set_eco(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);

            // HR59 clear must be the first write.
            assert_eq!(writes[0].address, HR_ENABLE_DISCHARGE);
            assert_eq!(writes[0].value, 0);

            // HR27=1 (self-consumption) must be the last write.
            let hr27 = writes
                .iter()
                .find(|w| w.address == HR_BATTERY_POWER_MODE)
                .expect("must write HR27");
            assert_eq!(hr27.value, 1);
            assert_eq!(
                writes.last().unwrap().address,
                HR_BATTERY_POWER_MODE,
                "HR27 must come after slot clears"
            );

            // All four discharge slot registers must be zeroed.
            for reg in [
                HR_DISCHARGE_SLOT_1_START,
                HR_DISCHARGE_SLOT_1_END,
                HR_DISCHARGE_SLOT_2_START,
                HR_DISCHARGE_SLOT_2_END,
            ] {
                let w = writes
                    .iter()
                    .find(|w| w.address == reg)
                    .unwrap_or_else(|| panic!("eco enable must clear slot register {reg}"));
                assert_eq!(w.value, 0);
            }
        })
        .await;
    }

    /// Enabling Eco with a configured discharge schedule must back up the
    /// slots so the user can restore them on the next Timed Export toggle
    /// (issue #137 parity with `set_mode`).
    #[tokio::test]
    async fn set_eco_enable_backs_up_configured_slots() {
        with_isolated_config_dir_async(|| async {
            use crate::inverter::model::ScheduleSlot;
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            {
                let mut snap = state.latest_snapshot.lock().await;
                let snap = snap.as_mut().unwrap();
                snap.enable_discharge = true;
                snap.discharge_slots[0] = ScheduleSlot {
                    enabled: true,
                    start_hour: 5,
                    start_minute: 0,
                    end_hour: 7,
                    end_minute: 0,
                    target_soc: 10,
                };
            }
            let body = serde_json::json!({ "enabled": true });
            let _ = set_eco(State(state.clone()), Json(body)).await;

            let backup = crate::settings::Settings::load()
                .discharge_slots_backup
                .expect("eco enable must back up configured slots");
            let slot0 = &backup[0];
            assert!(slot0.enabled);
            assert_eq!(slot0.start_hour, 5);
            assert_eq!(slot0.end_hour, 7);
        })
        .await;
    }

    #[tokio::test]
    async fn set_eco_disable_writes_hr27_only() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_BATTERY_POWER_MODE;
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let body = serde_json::json!({ "enabled": false });
            let _ = set_eco(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert_eq!(writes.len(), 1);
            assert_eq!(writes[0].address, HR_BATTERY_POWER_MODE);
            assert_eq!(writes[0].value, 0);
        })
        .await;
    }

    #[tokio::test]
    async fn timed_charge_toggle_writes_hr96_only() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_ENABLE_CHARGE;
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let body = serde_json::json!({ "enabled": true });
            let _ = set_timed_charge(State(state.clone()), Json(body)).await;
            let batches = {
                let mut pending = state.pending_writes.lock().await;
                std::mem::take(&mut *pending)
            };
            assert_eq!(batches.len(), 1);
            assert_eq!(
                batches[0].owner, None,
                "HR96 is outside discharge ownership"
            );
            let writes: Vec<RegisterWrite> =
                batches.into_iter().flat_map(|batch| batch.writes).collect();
            assert_all_whitelisted(&writes);
            assert_eq!(writes.len(), 1);
            assert_eq!(writes[0].address, HR_ENABLE_CHARGE);
            assert_eq!(writes[0].value, 1);
        })
        .await;
    }

    #[tokio::test]
    async fn timed_export_legacy_setting_disables_eco_for_full_export() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_BATTERY_POWER_MODE, HR_ENABLE_DISCHARGE};
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            seed_gen3_timed_pre_state(&state).await;
            // Issue #289: arming is conditional on "now" being inside the
            // slot window, so replace slot 1 with a window that always
            // contains the test's wall clock.
            let (sh, sm, eh, em) = window_containing_now();
            {
                let mut guard = state.latest_snapshot.lock().await;
                guard.as_mut().unwrap().discharge_slots[0] = crate::inverter::model::ScheduleSlot {
                    enabled: true,
                    start_hour: sh,
                    start_minute: sm,
                    end_hour: eh,
                    end_minute: em,
                    target_soc: 4,
                };
            }
            let body = serde_json::json!({ "enabled": true });
            let (status, response) =
                drive_set_timed_export_completion_sequence(&state, body, &[WriteOutcome::Ok]).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(response.0["ok"], json!(true));
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_BATTERY_POWER_MODE)
                    .map(|w| w.value),
                Some(0)
            );
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_ENABLE_DISCHARGE)
                    .map(|w| w.value),
                Some(1)
            );
        })
        .await;
    }

    #[tokio::test]
    async fn timed_export_arm_rejection_does_not_commit_schedule() {
        // The slot phase and the export-arm phase are separate. If the arm
        // phase is rejected, the endpoint must report the failure and leave
        // the managed schedule disabled so a later poll cannot treat a
        // partially written inverter as an active HEM schedule.
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_BATTERY_POWER_MODE;
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            seed_gen3_timed_pre_state(&state).await;

            let (status, response) = drive_set_timed_export_completion_sequence(
                &state,
                serde_json::json!({ "enabled": true }),
                &[WriteOutcome::Failed {
                    address: HR_BATTERY_POWER_MODE,
                    value: 0,
                    error: "rejected by inverter".to_string(),
                }],
            )
            .await;

            assert_eq!(status, StatusCode::BAD_GATEWAY);
            assert_eq!(response.0["ok"], json!(false));
            assert!(!state.timed_export_config.lock().await.schedule_enabled);
            assert!(!crate::settings::Settings::load().timed_export_schedule_enabled);
        })
        .await;
    }

    #[tokio::test]
    async fn timed_export_disable_clears_hr59_and_restores_eco_hr27() {
        // Stopping Timed Export must do BOTH: clear the schedule flag
        // (HR59=0) and return the inverter to self-consumption (HR27=1).
        // The Timed Export enable path is the only thing that ever
        // writes HR27=0, so its disable path is also the only thing
        // that writes HR27=1 again. Forgetting HR27=1 here made the
        // Stop button a no-op: the schedule flag flipped off in the
        // UI, but the inverter kept force-exporting to grid.
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_BATTERY_POWER_MODE, HR_ENABLE_DISCHARGE};
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            // Physically exporting (HR27=0/HR59=1): the disarm writes are
            // required. (Against an already-in-Eco snapshot the stop is
            // idempotent and skips them — covered in e2e_mock.)
            {
                let mut snap = state.latest_snapshot.lock().await;
                if let Some(s) = snap.as_mut() {
                    s.battery_power_mode = 0;
                    s.enable_discharge = true;
                }
            }
            let body = serde_json::json!({ "enabled": false });
            // The stop path awaits its disarm batch (code-review finding 1),
            // so the test must drive the completion channel the poll loop
            // would normally resolve.
            let _ = drive_set_timed_export_completion_with(&state, body, WriteOutcome::Ok).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert_eq!(writes.len(), 2);
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_ENABLE_DISCHARGE)
                    .map(|w| w.value),
                Some(0),
                "Stop must clear the schedule flag (HR59=0)"
            );
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_BATTERY_POWER_MODE)
                    .map(|w| w.value),
                Some(1),
                "Stop must return the inverter to self-consumption (HR27=1)"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn timed_export_stop_disarm_failure_leaves_retryable_exit_state() {
        // CODE_REVIEW.md finding 2: a stop whose disarm batch fails must
        // not strand the export-armed registers behind a quiet machine. The
        // handler already persisted schedule_enabled=false and set Off;
        // when the awaited disarm fails it must arm the reconciler into
        // Exiting so the poll loop keeps repairing (distinguished from an
        // externally-owned schedule, which lands Off and stays quiet).
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            // Physically exporting (HR27=0/HR59=1) with a populated physical
            // slot — the residue shape the reconciler must keep repairing.
            {
                let mut snap = state.latest_snapshot.lock().await;
                if let Some(s) = snap.as_mut() {
                    s.battery_power_mode = 0;
                    s.enable_discharge = true;
                    s.discharge_slots[0] = crate::inverter::model::ScheduleSlot {
                        enabled: true,
                        start_hour: 16,
                        start_minute: 0,
                        end_hour: 19,
                        end_minute: 0,
                        target_soc: 4,
                    };
                }
            }
            let (status, _) = drive_set_timed_export_completion_with(
                &state,
                serde_json::json!({ "enabled": false }),
                WriteOutcome::Failed {
                    address: 59,
                    value: 0,
                    error: "dongle busy".to_string(),
                },
            )
            .await;
            assert_eq!(status, StatusCode::BAD_GATEWAY);
            assert!(
                !crate::settings::Settings::load().timed_export_schedule_enabled,
                "the stop still persists the disabled schedule for retry"
            );
            assert!(
                matches!(
                    *state.timed_export_state.lock().await,
                    crate::inverter::state_machines::TimedExportState::Exiting { .. }
                ),
                "a failed disarm must leave the machine in Exiting so the poll loop repairs it"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn timed_export_slot_writes_queue_with_manual_mode_ownership() {
        // CODE_REVIEW.md finding 4: Timed Export slot restore/configuration
        // writes used to queue ownerless, and ownerless batches are always
        // admitted — so a slot edit saved while Force Discharge ran would
        // overwrite the temporary force-discharge slot, and stopping Force
        // Discharge would then restore the older captured slot, losing the
        // user's edit. Slot writes carry ownership that a Force Discharge
        // (ManualForce) outranks and defers.
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            {
                let mut snap = state.latest_snapshot.lock().await;
                if let Some(s) = snap.as_mut() {
                    s.discharge_slots[0] = crate::inverter::model::ScheduleSlot {
                        enabled: true,
                        start_hour: 16,
                        start_minute: 0,
                        end_hour: 19,
                        end_minute: 0,
                        target_soc: 20,
                    };
                }
            }
            let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
            let captured_clone = captured.clone();
            let state_for_handler = state.clone();
            let handle = tokio::spawn(async move {
                set_timed_export(State(state_for_handler), Json(json!({ "enabled": true }))).await
            });
            loop {
                if handle.is_finished() {
                    break;
                }
                let mut pw = state.pending_writes.lock().await;
                if let Some(batch) = pw.iter_mut().find(|batch| batch.completion.is_some()) {
                    captured_clone.lock().unwrap().push(batch.owner);
                    if let Some(tx) = batch.completion.take() {
                        let _ = tx.send(WriteOutcome::Ok);
                    }
                }
                drop(pw);
                tokio::task::yield_now().await;
            }
            let (status, _) = handle.await.unwrap();
            assert_eq!(status, StatusCode::OK);
            let owners = captured.lock().unwrap().clone();
            assert!(!owners.is_empty(), "the slot restore batch must be queued");
            assert_eq!(
                owners[0],
                Some(DischargeControlOwner::ManualMode),
                "slot writes must carry ManualMode ownership so Force Discharge defers them"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn discharge_slot_save_queues_writes_with_manual_mode_ownership() {
        // Same ownership contract for the slot editor endpoint (finding 4):
        // the slot/target batch is ManualMode-owned (deferred behind Force
        // Discharge, admitted during an HR318 pause via the manual-selection
        // exception), while the export-arm batch keeps the TimedExport owner
        // so an explicit pause still blocks it (issue #289).
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
            let captured_clone = captured.clone();
            let state_for_handler = state.clone();
            let handle = tokio::spawn(async move {
                set_discharge_slot(
                    State(state_for_handler),
                    Json(json!({
                        "slot": 1,
                        "start_hour": 16, "start_minute": 0,
                        "end_hour": 19, "end_minute": 0,
                        "enabled": true,
                    })),
                )
                .await
            });
            loop {
                if handle.is_finished() {
                    break;
                }
                let mut pw = state.pending_writes.lock().await;
                if let Some(batch) = pw.iter_mut().find(|batch| batch.completion.is_some()) {
                    captured_clone.lock().unwrap().push(batch.owner);
                    if let Some(tx) = batch.completion.take() {
                        let _ = tx.send(WriteOutcome::Ok);
                    }
                }
                drop(pw);
                tokio::task::yield_now().await;
            }
            let (status, _) = handle.await.unwrap();
            assert_eq!(status, StatusCode::OK);
            let owners = captured.lock().unwrap().clone();
            assert_eq!(
                owners.len(),
                2,
                "an in-window save queues the slot batch then the arm batch: {owners:?}"
            );
            assert_eq!(
                owners[0],
                Some(DischargeControlOwner::ManualMode),
                "slot writes must carry ManualMode ownership so Force Discharge defers them"
            );
            assert_eq!(
                owners[1],
                Some(DischargeControlOwner::TimedExport),
                "the export arm keeps the TimedExport owner (pause precedence)"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn timed_export_disable_with_rearm_fallback_clears_slots_before_disarm() {
        // Issue #289: once the firmware is classified as HR59 re-arming,
        // Stop must clear the physical slot registers BEFORE the HR59=0
        // disarm — the firmware re-asserts HR59=1 while any discharge slot
        // register is non-zero, so a bare disarm is immediately undone and
        // Timed Export can never be stopped.
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START,
                HR_ENABLE_DISCHARGE,
            };
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            // Classify the firmware as re-arming (as the poll-loop detector
            // would after three qualifying polls).
            state
                .timed_export_config
                .lock()
                .await
                .device_rearm_confirmed = true;
            // Physically exporting (HR27=0/HR59=1): the disarm batch is
            // required (an already-in-Eco inverter would skip it).
            {
                let mut snap = state.latest_snapshot.lock().await;
                if let Some(s) = snap.as_mut() {
                    s.battery_power_mode = 0;
                    s.enable_discharge = true;
                }
            }

            let body = serde_json::json!({ "enabled": false });
            // Drive both completion-backed phases (slot clears, then the
            // awaited disarm batch) the way the poll loop would.
            let _ = drive_set_timed_export_completion_with(&state, body, WriteOutcome::Ok).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);

            let hr59_idx = writes
                .iter()
                .position(|w| w.address == HR_ENABLE_DISCHARGE)
                .expect("HR59=0 write present");
            let hr27_idx = writes
                .iter()
                .position(|w| w.address == HR_BATTERY_POWER_MODE)
                .expect("HR27=1 write present");
            assert!(hr59_idx < hr27_idx, "HR59=0 must precede HR27=1");
            let slot_clear_idx = writes
                .iter()
                .position(|w| w.address == HR_DISCHARGE_SLOT_1_START)
                .expect("slot clear write present when the fallback is active");
            assert!(
                slot_clear_idx < hr59_idx,
                "slot clears must precede the HR59=0 disarm so re-arm firmware cannot undo it"
            );
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_DISCHARGE_SLOT_1_START)
                    .map(|w| w.value),
                Some(0)
            );
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_DISCHARGE_SLOT_1_END)
                    .map(|w| w.value),
                Some(0)
            );
        })
        .await;
    }

    /// BLOCKER: Stop must always issue the disarm writes — even when the
    /// snapshot already shows the Eco baseline. Skipping the disarm left a
    /// window where a poll-cycle transition could re-arm export after the
    /// endpoint returned 200 OK.
    #[tokio::test]
    async fn stopping_timed_export_always_issues_disarm_even_when_eco_already_confirmed() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_BATTERY_POWER_MODE, HR_ENABLE_DISCHARGE};
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            {
                let mut config = state.timed_export_config.lock().await;
                config.schedule_enabled = true;
                config.slots = vec![crate::inverter::model::ScheduleSlot {
                    enabled: true,
                    start_hour: 16,
                    start_minute: 0,
                    end_hour: 19,
                    end_minute: 0,
                    target_soc: 100,
                }];
            }
            // Snapshot reads Eco (HR27=1, HR59=0). The previous
            // short-circuit would skip the disarm batch entirely.
            {
                let mut snap = state.latest_snapshot.lock().await;
                if let Some(s) = snap.as_mut() {
                    s.battery_power_mode = 1;
                    s.enable_discharge = false;
                }
            }

            let (status, _) = drive_set_timed_export_completion_with(
                &state,
                serde_json::json!({ "enabled": false }),
                WriteOutcome::Ok,
            )
            .await;
            assert_eq!(status, StatusCode::OK);

            let writes = drain_pending_writes(&state).await;
            let has_enable_discharge = writes
                .iter()
                .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0);
            let has_battery_power_mode = writes
                .iter()
                .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1);
            assert!(
                has_enable_discharge,
                "Stop must always write HR59=0 to defeat a later re-arm"
            );
            assert!(
                has_battery_power_mode,
                "Stop must always write HR27=1 to defeat a later re-arm"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn timed_export_enable_rejects_empty_schedule_without_writes() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let (status, response) = set_timed_export(
                State(state.clone()),
                Json(serde_json::json!({ "enabled": true })),
            )
            .await;

            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(response.0["ok"], json!(false));
            assert!(response.0["error"].as_str().unwrap().contains("Configure"));
            assert!(drain_pending_writes(&state).await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn timed_export_enable_restores_backup_before_arming_hr59() {
        with_isolated_config_dir_async(|| async {
            use crate::settings::{DischargeSlotBackup, Settings};
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            // Issue #289: HR59=1 is only queued when the restored window
            // contains "now", so use a window that always does.
            let (sh, sm, eh, em) = window_containing_now();
            let mut settings = Settings::load();
            settings.discharge_slots_backup = Some(vec![DischargeSlotBackup {
                enabled: true,
                start_hour: sh,
                start_minute: sm,
                end_hour: eh,
                end_minute: em,
                target_soc: 4,
            }]);
            settings.save().unwrap();

            // The restore path awaits write confirmation; drive the
            // completion channel the poll loop would normally resolve.
            let (status, _) = drive_set_timed_export_completion_with(
                &state,
                serde_json::json!({ "enabled": true }),
                WriteOutcome::Ok,
            )
            .await;
            assert_eq!(status, StatusCode::OK);

            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            let hr59_index = writes
                .iter()
                .position(|write| write.address == crate::modbus::registers::HR_ENABLE_DISCHARGE)
                .expect("Timed Export must arm HR59");
            let slot_start_index = writes
                .iter()
                .position(|write| {
                    write.address == crate::modbus::registers::HR_DISCHARGE_SLOT_1_START
                })
                .expect("backup slot must be restored");
            assert!(slot_start_index < hr59_index, "slot must precede HR59=1");
            assert_eq!(writes[hr59_index].value, 1);
            assert!(Settings::load().discharge_slots_backup.is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn timed_export_restore_rejection_keeps_backup_and_errors() {
        // Review finding #7: the restored backup discharge-slot writes were
        // queued fire-and-forget while the #137 backup was consumed and
        // persist-cleared up front. If the inverter rejects the slot-restore
        // writes the user's discharge schedule backup is already gone. The
        // backup must survive a rejection and the endpoint must surface the
        // failure instead of reporting success.
        with_isolated_config_dir_async(|| async {
            use crate::settings::{DischargeSlotBackup, Settings};
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let mut settings = Settings::load();
            settings.discharge_slots_backup = Some(vec![DischargeSlotBackup {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 4,
            }]);
            settings.save().unwrap();

            let (status, response) = drive_set_timed_export_completion_with(
                &state,
                serde_json::json!({ "enabled": true }),
                WriteOutcome::Failed {
                    address: crate::modbus::registers::HR_DISCHARGE_SLOT_1_START,
                    value: 960,
                    error: "rejected by inverter".to_string(),
                },
            )
            .await;

            assert_eq!(
                status,
                StatusCode::BAD_GATEWAY,
                "rejected restore writes must surface an error"
            );
            assert_eq!(response.0["ok"], json!(false));
            let restored = Settings::load()
                .discharge_slots_backup
                .expect("backup must survive a rejected slot restore");
            assert_eq!(restored[0].start_hour, 16);
            assert_eq!(restored[0].end_hour, 19);
        })
        .await;
    }

    #[tokio::test]
    async fn timed_export_restore_success_consumes_backup() {
        // Happy-path counterpart: when the inverter accepts the restore
        // writes the backup is consumed and persist-cleared exactly once.
        with_isolated_config_dir_async(|| async {
            use crate::settings::{DischargeSlotBackup, Settings};
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let mut settings = Settings::load();
            settings.discharge_slots_backup = Some(vec![DischargeSlotBackup {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 4,
            }]);
            settings.save().unwrap();

            let (status, _) = drive_set_timed_export_completion_with(
                &state,
                serde_json::json!({ "enabled": true }),
                WriteOutcome::Ok,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(Settings::load().discharge_slots_backup.is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn timed_export_enable_without_snapshot_returns_error() {
        // Without a snapshot the register layout (device type) is unknown, so
        // restoring the backup could target the wrong registers. The endpoint
        // must refuse instead of silently assuming Gen2Hybrid.
        with_isolated_config_dir_async(|| async {
            use crate::settings::{DischargeSlotBackup, Settings};
            let state = Arc::new(AppState::new());
            let mut settings = Settings::load();
            settings.discharge_slots_backup = Some(vec![DischargeSlotBackup {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 4,
            }]);
            settings.save().unwrap();

            let (status, response) = set_timed_export(
                State(state.clone()),
                Json(serde_json::json!({ "enabled": true })),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(response.0["ok"], json!(false));
            assert!(response.0["error"]
                .as_str()
                .unwrap()
                .contains("inverter data"));
            assert!(drain_pending_writes(&state).await.is_empty());
            // The backup must not be consumed by the refused operation.
            assert!(Settings::load().discharge_slots_backup.is_some());
        })
        .await;
    }

    #[tokio::test]
    async fn saving_enabled_discharge_slot_arms_timed_export_after_slot_writes() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START,
                HR_DISCHARGE_TARGET_SOC_1, HR_ENABLE_DISCHARGE,
            };
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            // Issue #289: the arming pair is only queued when the saved
            // window contains "now" — use one that always does.
            let (sh, sm, eh, em) = window_containing_now();

            let (status, _) = drive_set_discharge_slot_completion_with(
                &state,
                serde_json::json!({
                    "slot": 1,
                    "enabled": true,
                    "start_hour": sh,
                    "start_minute": sm,
                    "end_hour": eh,
                    "end_minute": em,
                    "target_soc": 40
                }),
                WriteOutcome::Ok,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            // Two-phase fail-fast arming: the slot/target writes form their
            // own batch, separate from the export-arm writes, so a rejected
            // slot write can never be followed by the arming pair.
            let batches = {
                let mut pw = state.pending_writes.lock().await;
                std::mem::take(&mut *pw)
            };
            assert_eq!(
                batches.len(),
                2,
                "slot writes and arm writes must be separate phases"
            );
            assert_eq!(batches[0].policy, WriteBatchPolicy::FailFastTransactional);
            assert_eq!(batches[1].policy, WriteBatchPolicy::FailFastTransactional);
            let writes: Vec<RegisterWrite> = batches.into_iter().flat_map(|b| b.writes).collect();
            assert_all_whitelisted(&writes);
            assert_eq!(
                writes
                    .iter()
                    .map(|write| (write.address, write.value))
                    .collect::<Vec<_>>(),
                vec![
                    (HR_DISCHARGE_SLOT_1_START, encode_hhmm(sh, sm)),
                    (HR_DISCHARGE_SLOT_1_END, encode_hhmm(eh, em)),
                    (HR_DISCHARGE_TARGET_SOC_1, 40),
                    (HR_BATTERY_POWER_MODE, 0),
                    (HR_ENABLE_DISCHARGE, 1),
                ]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn saving_enabled_zero_duration_discharge_slot_does_not_arm_timed_export() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;

            let (status, response) = set_discharge_slot(
                State(state.clone()),
                Json(serde_json::json!({
                    "slot": 1,
                    "enabled": true,
                    "start_hour": 16,
                    "end_hour": 16,
                    "target_soc": 40
                })),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(response.0["error"]
                .as_str()
                .is_some_and(|error| error.contains("differ")));
            assert!(drain_pending_writes(&state).await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn saving_extended_discharge_slot_3_arms_timed_export() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_DISCHARGE_SLOT_3_END, HR_DISCHARGE_SLOT_3_START,
                HR_DISCHARGE_TARGET_SOC_3, HR_ENABLE_DISCHARGE,
            };
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let (sh, sm, eh, em) = window_containing_now();

            let (status, _) = drive_set_discharge_slot_completion_with(
                &state,
                serde_json::json!({
                    "slot": 3,
                    "enabled": true,
                    "start_hour": sh,
                    "start_minute": sm,
                    "end_hour": eh,
                    "end_minute": em,
                    "target_soc": 10
                }),
                WriteOutcome::Ok,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            // The extended slot + per-slot floor must land before the arming
            // pair — the same ordering guarantee the slot-1 test pins.
            assert_eq!(
                writes
                    .iter()
                    .map(|write| (write.address, write.value))
                    .collect::<Vec<_>>(),
                vec![
                    (HR_DISCHARGE_SLOT_3_START, encode_hhmm(sh, sm)),
                    (HR_DISCHARGE_SLOT_3_END, encode_hhmm(eh, em)),
                    (HR_DISCHARGE_TARGET_SOC_3, 10),
                    (HR_BATTERY_POWER_MODE, 0),
                    (HR_ENABLE_DISCHARGE, 1),
                ]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn saving_three_phase_discharge_slot_arms_timed_export() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_3PH_DISCHARGE_SLOT_1_END, HR_3PH_DISCHARGE_SLOT_1_START,
                HR_3PH_FORCE_DISCHARGE_ENABLE, HR_BATTERY_POWER_MODE, HR_DISCHARGE_TARGET_SOC_1,
            };
            let state = make_state_with_device(DeviceType::ThreePhase).await;
            let (sh, sm, eh, em) = window_containing_now();

            let (status, _) = drive_set_discharge_slot_completion_with(
                &state,
                serde_json::json!({
                    "slot": 1,
                    "enabled": true,
                    "start_hour": sh,
                    "start_minute": sm,
                    "end_hour": eh,
                    "end_minute": em,
                    "target_soc": 40
                }),
                WriteOutcome::Ok,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            // Three-phase slot writes land in the HR 1080-1124 bank, and the
            // arming pair must use the three-phase force-discharge enable
            // (HR1122) — three-phase firmware derives `enable_discharge`
            // from HR1122, so writing HR59 arms nothing and the machine
            // would sit in Entering for the whole window (code-review
            // finding: model-incorrect enable routing).
            assert_eq!(
                writes
                    .iter()
                    .map(|write| (write.address, write.value))
                    .collect::<Vec<_>>(),
                vec![
                    (HR_3PH_DISCHARGE_SLOT_1_START, encode_hhmm(sh, sm)),
                    (HR_3PH_DISCHARGE_SLOT_1_END, encode_hhmm(eh, em)),
                    (HR_DISCHARGE_TARGET_SOC_1, 40),
                    (HR_BATTERY_POWER_MODE, 0),
                    (HR_3PH_FORCE_DISCHARGE_ENABLE, 1),
                ]
            );
            assert!(!writes
                .iter()
                .any(|w| w.address == crate::modbus::registers::HR_ENABLE_DISCHARGE));
        })
        .await;
    }

    #[tokio::test]
    async fn disabling_a_slot_while_timed_export_is_disarmed_writes_no_mode_registers() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START,
                HR_ENABLE_DISCHARGE,
            };
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                let snapshot = snapshot.as_mut().unwrap();
                snapshot.enable_discharge = false;
                snapshot.discharge_slots[0] = crate::inverter::model::ScheduleSlot {
                    enabled: true,
                    start_hour: 16,
                    start_minute: 0,
                    end_hour: 19,
                    end_minute: 0,
                    target_soc: 4,
                };
            }

            let (status, _) = drive_set_discharge_slot_completion_with(
                &state,
                serde_json::json!({
                    "slot": 1,
                    "enabled": false,
                    "start_hour": 16,
                    "end_hour": 19
                }),
                WriteOutcome::Ok,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            // Disarming is not this path's job: only the slot clears go out,
            // so a disabled save can never flip the inverter's mode by itself.
            assert_eq!(
                writes
                    .iter()
                    .map(|write| (write.address, write.value))
                    .collect::<Vec<_>>(),
                vec![(HR_DISCHARGE_SLOT_1_START, 0), (HR_DISCHARGE_SLOT_1_END, 0)]
            );
            assert!(!writes.iter().any(|w| w.address == HR_BATTERY_POWER_MODE));
            assert!(!writes.iter().any(|w| w.address == HR_ENABLE_DISCHARGE));
        })
        .await;
    }

    #[tokio::test]
    async fn editing_persisted_slot_under_fallback_preserves_other_slots() {
        // Code-review finding: editing one slot under the re-arm fallback
        // (physical registers cleared) must not silently drop every other
        // configured desired slot. The persisted schedule is the source of
        // truth and the edit merges into it by slot index.
        use crate::inverter::model::ScheduleSlot;
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            // Seed two configured persisted slots and mark the fallback active.
            {
                let mut config = state.timed_export_config.lock().await;
                config.slots = vec![
                    ScheduleSlot {
                        enabled: true,
                        start_hour: 6,
                        start_minute: 0,
                        end_hour: 9,
                        end_minute: 0,
                        target_soc: 50,
                    },
                    ScheduleSlot {
                        enabled: true,
                        start_hour: 16,
                        start_minute: 0,
                        end_hour: 19,
                        end_minute: 0,
                        target_soc: 4,
                    },
                ];
                config.schedule_enabled = true;
                config.device_rearm_confirmed = true;
            }
            *state.timed_export_state.lock().await =
                crate::inverter::state_machines::TimedExportState::Error {
                    reason: "old schedule failed".to_string(),
                };
            // The physical registers are intentionally zeroed by the fallback.
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                for slot in snapshot.as_mut().unwrap().discharge_slots.iter_mut() {
                    *slot = ScheduleSlot::default();
                }
            }

            let (status, response) = drive_set_discharge_slot_completion_with(
                &state,
                serde_json::json!({
                    "slot": 1,
                    "enabled": true,
                    "start_hour": 7,
                    "end_hour": 8,
                    "target_soc": 30
                }),
                WriteOutcome::Ok,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            // Slot 2's persisted entry must survive the edit even though the
            // physical registers show it cleared.
            let config = state.timed_export_config.lock().await;
            assert_eq!(config.slots.len(), 2);
            assert_eq!(config.slots[0].start_hour, 7);
            assert_eq!(config.slots[0].target_soc, 30);
            assert_eq!(config.slots[1].start_hour, 16, "slot 2 must be preserved");
            assert!(response.0["schedule"]["slots"].is_array());
            drop(config);
            assert!(matches!(
                *state.timed_export_state.lock().await,
                crate::inverter::state_machines::TimedExportState::Entering { .. }
            ));
        })
        .await;
    }

    #[tokio::test]
    async fn re_enabling_fallback_schedule_uses_persisted_desired_slots() {
        // The persisted desired slots are the source of truth. Re-enabling
        // a fallback schedule (physical slots zeroed, no legacy backup)
        // must NOT be rejected with "configure at least one slot" — the
        // persisted slots are the schedule.
        use crate::inverter::model::ScheduleSlot;
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            // Persisted desired schedule exists and is disabled; physical slots
            // are cleared (fallback active). No legacy #137 backup exists in
            // the fresh isolated config directory.
            {
                let mut config = state.timed_export_config.lock().await;
                config.slots = vec![ScheduleSlot {
                    enabled: true,
                    start_hour: 16,
                    start_minute: 0,
                    end_hour: 19,
                    end_minute: 0,
                    target_soc: 4,
                }];
                config.schedule_enabled = false;
                config.device_rearm_confirmed = true;
            }
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                for slot in snapshot.as_mut().unwrap().discharge_slots.iter_mut() {
                    *slot = ScheduleSlot::default();
                }
            }

            let (status, response) = drive_set_timed_export_completion_with(
                &state,
                serde_json::json!({ "enabled": true }),
                WriteOutcome::Ok,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "got {response:?}");
            assert!(state.timed_export_config.lock().await.schedule_enabled);
        })
        .await;
    }

    #[tokio::test]
    async fn rejected_slot_write_prevents_export_arm() {
        // Code-review blocker: a failed slot write must prevent the
        // export-arm writes. The endpoint must report the failure and not
        // queue HR27=0/HR59=1.
        use crate::inverter::model::ScheduleSlot;
        use crate::modbus::registers::{
            HR_BATTERY_POWER_MODE, HR_DISCHARGE_SLOT_1_START, HR_ENABLE_DISCHARGE,
        };
        let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
        // Persist a desired schedule covering now so arm_now fires.
        let (sh, sm, eh, em) = window_containing_now();
        state.timed_export_config.lock().await.slots = vec![ScheduleSlot {
            enabled: true,
            start_hour: sh,
            start_minute: sm,
            end_hour: eh,
            end_minute: em,
            target_soc: 4,
        }];

        let (status, response) = drive_set_discharge_slot_completion_sequence(
            &state,
            serde_json::json!({
                "slot": 1,
                "enabled": true,
                "start_hour": sh,
                "start_minute": sm,
                "end_hour": eh,
                "end_minute": em,
                "target_soc": 40
            }),
            // The slot write batch is rejected; the compensating restore is
            // accepted, so no disarm is needed.
            &[
                WriteOutcome::Failed {
                    address: HR_DISCHARGE_SLOT_1_START,
                    value: encode_hhmm(sh, sm),
                    error: "code 0".to_string(),
                },
                WriteOutcome::Ok,
            ],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(response.0["error"]
            .as_str()
            .is_some_and(|e| e.contains("Timed Export slot 1 could not be saved")));
        // The export-arm writes must never have been queued.
        let batches = {
            let mut pw = state.pending_writes.lock().await;
            std::mem::take(&mut *pw)
        };
        let writes: Vec<RegisterWrite> = batches.into_iter().flat_map(|b| b.writes).collect();
        for w in &writes {
            assert_ne!(
                w.address, HR_BATTERY_POWER_MODE,
                "export-arm mode write must not be queued after a slot-write failure"
            );
            assert_ne!(
                w.address, HR_ENABLE_DISCHARGE,
                "export-arm enable write must not be queued after a slot-write failure"
            );
        }
    }

    #[tokio::test]
    async fn rejected_future_slot_write_does_not_commit_schedule() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_DISCHARGE_SLOT_1_START;
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;

            let (status, response) = drive_set_discharge_slot_completion_with(
                &state,
                serde_json::json!({
                    "slot": 1,
                    "enabled": true,
                    "start_hour": 18,
                    "start_minute": 0,
                    "end_hour": 20,
                    "end_minute": 0,
                    "target_soc": 40
                }),
                WriteOutcome::Failed {
                    address: HR_DISCHARGE_SLOT_1_START,
                    value: encode_hhmm(18, 0),
                    error: "code 0".to_string(),
                },
            )
            .await;

            assert_eq!(status, StatusCode::BAD_GATEWAY, "got {response:?}");
            assert!(!state.timed_export_config.lock().await.schedule_enabled);
            assert!(!crate::settings::Settings::load().timed_export_schedule_enabled);
        })
        .await;
    }

    #[tokio::test]
    async fn timed_export_slot_save_is_not_deferred_by_active_timed_discharge_pause() {
        with_isolated_config_dir_async(|| async {
            use crate::inverter::model::ScheduleSlot;

            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                let snapshot = snapshot.as_mut().expect("seeded snapshot");
                snapshot.battery_power_mode = 1;
                snapshot.enable_discharge = false;
                snapshot.battery_pause_mode = 2;
                snapshot.battery_pause_slot = ScheduleSlot {
                    enabled: true,
                    start_hour: 16,
                    start_minute: 0,
                    end_hour: 19,
                    end_minute: 0,
                    target_soc: 4,
                };
            }

            let handle = tokio::spawn(set_discharge_slot(
                State(state.clone()),
                Json(serde_json::json!({
                    "slot": 1,
                    "enabled": true,
                    "start_hour": 18,
                    "start_minute": 0,
                    "end_hour": 20,
                    "end_minute": 0,
                    "target_soc": 40
                })),
            ));

            loop {
                let mut pending = state.pending_writes.lock().await;
                if let Some(batch) = pending.iter_mut().find(|batch| batch.completion.is_some()) {
                    // CODE_REVIEW.md finding 4 retags these batches from
                    // ownerless to ManualMode (so a Force Discharge defers
                    // them). The property this test guards is unchanged: the
                    // manual-selection exception keeps a ManualMode batch
                    // eligible while a register-derived HR318 pause owns the
                    // cycle.
                    assert_eq!(
                        batch.owner,
                        Some(DischargeControlOwner::ManualMode),
                        "slot configuration stays ManualMode-owned and eligible while HR318 owns pause control"
                    );
                    let completion = batch.completion.take().expect("completion sender");
                    drop(pending);
                    completion.send(WriteOutcome::Ok).expect("handler waiting");
                    break;
                }
                drop(pending);
                tokio::task::yield_now().await;
            }

            let (status, response) = handle.await.expect("handler task");
            assert_eq!(status, StatusCode::OK, "got {response:?}");
            assert!(state.timed_export_config.lock().await.schedule_enabled);
        })
        .await;
    }

    #[tokio::test]
    async fn future_timed_export_replaces_manual_timed_demand_with_eco_baseline() {
        with_isolated_config_dir_async(|| async {
            use crate::inverter::model::{BatteryMode, ScheduleSlot};
            use crate::modbus::registers::{HR_BATTERY_POWER_MODE, HR_ENABLE_DISCHARGE};

            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                let snapshot = snapshot.as_mut().expect("seeded snapshot");
                snapshot.battery_mode = BatteryMode::TimedDemand;
                snapshot.battery_power_mode = 0;
                snapshot.enable_discharge = true;
            }
            state.timed_export_config.lock().await.slots = vec![ScheduleSlot {
                enabled: true,
                start_hour: 18,
                start_minute: 0,
                end_hour: 20,
                end_minute: 0,
                target_soc: 4,
            }];

            let (status, response) = drive_set_timed_export_completion_with(
                &state,
                serde_json::json!({ "enabled": true }),
                WriteOutcome::Ok,
            )
            .await;

            assert_eq!(status, StatusCode::OK, "got {response:?}");
            let writes = drain_pending_writes(&state).await;
            assert!(writes
                .iter()
                .any(|write| { write.address == HR_ENABLE_DISCHARGE && write.value == 0 }));
            assert!(writes
                .iter()
                .any(|write| { write.address == HR_BATTERY_POWER_MODE && write.value == 1 }));
            assert!(state.timed_export_config.lock().await.schedule_enabled);
        })
        .await;
    }

    #[tokio::test]
    async fn enabling_future_fallback_schedule_keeps_physical_slots_cleared() {
        with_isolated_config_dir_async(|| async {
            use crate::inverter::model::ScheduleSlot;
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            {
                let mut config = state.timed_export_config.lock().await;
                config.slots = vec![ScheduleSlot {
                    enabled: true,
                    start_hour: 18,
                    start_minute: 0,
                    end_hour: 20,
                    end_minute: 0,
                    target_soc: 4,
                }];
                config.device_rearm_confirmed = true;
            }

            let (status, response) = drive_set_timed_export_completion_with(
                &state,
                serde_json::json!({ "enabled": true }),
                WriteOutcome::Ok,
            )
            .await;

            assert_eq!(status, StatusCode::OK, "got {response:?}");
            let writes = drain_pending_writes(&state).await;
            assert!(writes
                .iter()
                .all(|write| !matches!(write.address, 56 | 57 | 272)));
            assert!(state.timed_export_config.lock().await.schedule_enabled);
        })
        .await;
    }

    #[tokio::test]
    async fn editing_future_fallback_slot_keeps_physical_slots_cleared() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            state
                .timed_export_config
                .lock()
                .await
                .device_rearm_confirmed = true;

            let (status, response) = drive_set_discharge_slot_completion_with(
                &state,
                serde_json::json!({
                    "slot": 1,
                    "enabled": true,
                    "start_hour": 18,
                    "start_minute": 0,
                    "end_hour": 20,
                    "end_minute": 0,
                    "target_soc": 40
                }),
                WriteOutcome::Ok,
            )
            .await;

            assert_eq!(status, StatusCode::OK, "got {response:?}");
            let writes = drain_pending_writes(&state).await;
            assert!(writes
                .iter()
                .all(|write| !matches!(write.address, 56 | 57 | 272)));
            let config = state.timed_export_config.lock().await;
            assert!(config.schedule_enabled);
            assert_eq!(config.slots[0].start_hour, 18);
        })
        .await;
    }

    #[tokio::test]
    async fn get_timed_export_exposes_evaluated_window_and_machine_state() {
        // The endpoint must include the backend's evaluated window fields
        // (so the UI does not have to re-derive them from a clock that
        // may disagree with the inverter) and serialise the machine state
        // as a structured value rather than the Debug-only string.
        use crate::inverter::model::ScheduleSlot;
        use crate::inverter::state_machines::TimedExportState;
        let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
        let (sh, sm, eh, em) = window_containing_now();
        {
            let mut config = state.timed_export_config.lock().await;
            config.schedule_enabled = true;
            config.slots = vec![ScheduleSlot {
                enabled: true,
                start_hour: sh,
                start_minute: sm,
                end_hour: eh,
                end_minute: em,
                target_soc: 4,
            }];
        }
        *state.timed_export_state.lock().await = TimedExportState::Active;
        let (status, response) = get_timed_export(State(state.clone())).await;
        assert_eq!(status, StatusCode::OK);
        let data = response.0["data"].as_object().unwrap();
        assert!(data.contains_key("machine_state"));
        assert!(data.contains_key("inverter_minute_of_day"));
        assert!(data.contains_key("in_window"));
        assert!(data.contains_key("hr318_blocking"));
        assert!(data.contains_key("timing_fallback"));
        // Locking the mutex directly would block this test, so just check
        // the structured value: unit variants serialise as JSON strings.
        assert_eq!(data["schedule_enabled"], serde_json::json!(true));
        assert!(data["slots"].as_array().unwrap().len() == 1);
    }

    #[tokio::test]
    async fn set_eco_enable_disables_managed_timed_export_schedule() {
        with_isolated_config_dir_async(|| async {
            // Issue #289 ownership: the frontend expects Eco-enable to disable
            // the managed Timed Export schedule. Eco must not silently leave a
            // scheduled re-arm pending for the next window boundary.
            use crate::inverter::model::ScheduleSlot;
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            state.timed_export_config.lock().await.slots = vec![ScheduleSlot {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 4,
            }];
            state.timed_export_config.lock().await.schedule_enabled = true;
            *state.timed_export_state.lock().await =
                crate::inverter::state_machines::TimedExportState::Active;

            let (status, _) = set_eco(
                State(state.clone()),
                Json(serde_json::json!({ "enabled": true })),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let config = state.timed_export_config.lock().await;
            assert!(
                !config.schedule_enabled,
                "Eco enable must disable managed TE"
            );
            // The desired slots must stay persisted so a later re-enable
            // restores the schedule directly (no legacy backup needed).
            assert_eq!(config.slots.len(), 1);
            let disk = crate::settings::Settings::load();
            assert!(!disk.timed_export_schedule_enabled);
        })
        .await;
    }

    #[tokio::test]
    async fn set_mode_eco_disables_managed_timed_export_schedule() {
        with_isolated_config_dir_async(|| async {
            use crate::inverter::model::ScheduleSlot;
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            state.timed_export_config.lock().await.slots = vec![ScheduleSlot {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 4,
            }];
            state.timed_export_config.lock().await.schedule_enabled = true;

            let (status, _) = set_mode(
                State(state.clone()),
                Json(serde_json::json!({ "mode": "eco", "soc_reserve": 10 })),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                !state.timed_export_config.lock().await.schedule_enabled,
                "Eco-mode change must disable the managed TE schedule"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn disabling_last_discharge_slot_returns_timed_export_to_eco() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_BATTERY_POWER_MODE, HR_ENABLE_DISCHARGE};
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            seed_gen3_timed_pre_state(&state).await;
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                let snapshot = snapshot.as_mut().unwrap();
                snapshot.discharge_slots[1] = Default::default();
            }
            let desired = state
                .latest_snapshot
                .lock()
                .await
                .as_ref()
                .unwrap()
                .discharge_slots
                .to_vec();
            {
                let mut config = state.timed_export_config.lock().await;
                config.schedule_enabled = true;
                config.slots = desired.clone();
            }
            crate::settings::Settings::update(|settings| {
                settings.timed_export_schedule_enabled = true;
                settings.timed_export_slots = desired;
            })
            .unwrap();
            *state.timed_export_state.lock().await =
                crate::inverter::state_machines::TimedExportState::Error {
                    reason: "previous boundary failure".to_string(),
                };

            let (status, _) = drive_set_discharge_slot_completion_with(
                &state,
                serde_json::json!({
                    "slot": 1,
                    "enabled": false,
                    "start_hour": 16,
                    "end_hour": 19
                }),
                WriteOutcome::Ok,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert_eq!(
                writes.last().map(|write| (write.address, write.value)),
                Some((HR_BATTERY_POWER_MODE, 1))
            );
            assert_eq!(
                writes
                    .iter()
                    .find(|write| write.address == HR_ENABLE_DISCHARGE)
                    .map(|write| write.value),
                Some(0)
            );
            assert_eq!(
                *state.timed_export_state.lock().await,
                crate::inverter::state_machines::TimedExportState::Off,
                "a successful final-slot edit must clear terminal Error state"
            );
            assert!(!crate::settings::Settings::load().timed_export_schedule_enabled);
        })
        .await;
    }

    #[tokio::test]
    async fn rejected_last_slot_clear_keeps_enabled_schedule_and_reports_failure() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_DISCHARGE_SLOT_1_START;

            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            seed_gen3_timed_pre_state(&state).await;
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                snapshot.as_mut().unwrap().discharge_slots[1] = Default::default();
            }
            let desired = state
                .latest_snapshot
                .lock()
                .await
                .as_ref()
                .unwrap()
                .discharge_slots
                .to_vec();
            {
                let mut config = state.timed_export_config.lock().await;
                config.schedule_enabled = true;
                config.slots = desired.clone();
            }
            crate::settings::Settings::update(|settings| {
                settings.timed_export_schedule_enabled = true;
                settings.timed_export_slots = desired;
            })
            .unwrap();

            // The clear batch is rejected; the compensating restore of the
            // prior physical slot succeeds, so the desired schedule rolls
            // back to enabled.
            let (status, response) = drive_set_discharge_slot_completion_sequence(
                &state,
                serde_json::json!({
                    "slot": 1,
                    "enabled": false,
                    "start_hour": 16,
                    "end_hour": 19
                }),
                &[
                    WriteOutcome::Failed {
                        address: HR_DISCHARGE_SLOT_1_START,
                        value: 0,
                        error: "rejected by inverter".to_string(),
                    },
                    WriteOutcome::Ok,
                ],
            )
            .await;

            assert_eq!(status, StatusCode::BAD_GATEWAY, "got {response:?}");
            assert!(state.timed_export_config.lock().await.schedule_enabled);
            assert!(crate::settings::Settings::load().timed_export_schedule_enabled);
        })
        .await;
    }

    /// BLOCKER: a failed physical-slot clear must roll back the persisted
    /// desired schedule (and the in-memory mirror). Without the rollback,
    /// the inverter still holds the user's slot but HEM believes the
    /// schedule is disabled — the reconciler sees an armed physical slot
    /// against a disabled desired and may re-arm export with no safety
    /// window. The rollback is durable (settings.json) and the live
    /// mirror is restored before the handler returns.
    #[tokio::test]
    async fn rejected_disable_rolls_back_persisted_and_in_memory_schedule() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_DISCHARGE_SLOT_1_START;

            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            // Seed an enabled 16:00-19:00 slot on slot 1.
            let slot = crate::inverter::model::ScheduleSlot {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 50,
            };
            {
                let mut config = state.timed_export_config.lock().await;
                config.schedule_enabled = true;
                config.slots = vec![slot.clone()];
            }
            crate::settings::Settings::update(|settings| {
                settings.timed_export_schedule_enabled = true;
                settings.timed_export_slots = vec![slot];
            })
            .unwrap();
            // Mirror the same slot into the live snapshot so the physical
            // write is what gets queued (and rejected).
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                snapshot.as_mut().unwrap().discharge_slots[0] =
                    crate::inverter::model::ScheduleSlot {
                        enabled: true,
                        start_hour: 16,
                        start_minute: 0,
                        end_hour: 19,
                        end_minute: 0,
                        target_soc: 50,
                    };
            }

            // The clear batch is rejected; the compensating restore succeeds,
            // so the pre-edit schedule is restored.
            let (status, _) = drive_set_discharge_slot_completion_sequence(
                &state,
                serde_json::json!({
                    "slot": 1,
                    "enabled": false,
                    "start_hour": 16,
                    "end_hour": 19
                }),
                &[
                    WriteOutcome::Failed {
                        address: HR_DISCHARGE_SLOT_1_START,
                        value: 0,
                        error: "rejected by inverter".to_string(),
                    },
                    WriteOutcome::Ok,
                ],
            )
            .await;
            assert_eq!(status, StatusCode::BAD_GATEWAY);

            // Pre-edit desired schedule (enabled + slot 1 at 16:00-19:00) must
            // be back in the in-memory mirror.
            let config = state.timed_export_config.lock().await;
            assert!(config.schedule_enabled, "schedule must remain enabled");
            assert_eq!(config.slots.len(), 1);
            assert!(config.slots[0].enabled);
            assert_eq!(config.slots[0].start_hour, 16);
            assert_eq!(config.slots[0].end_hour, 19);
            drop(config);

            // And on disk.
            let on_disk = crate::settings::Settings::load();
            assert!(on_disk.timed_export_schedule_enabled);
            assert_eq!(on_disk.timed_export_slots.len(), 1);
            assert!(on_disk.timed_export_slots[0].enabled);
            assert_eq!(on_disk.timed_export_slots[0].start_hour, 16);
            assert_eq!(on_disk.timed_export_slots[0].end_hour, 19);
        })
        .await;
    }

    /// BLOCKER: when the final active slot's clear succeeds but the disarm
    /// writes fail, the disable must STICK — the physical slot is already
    /// gone, so rolling the desired schedule back to enabled would make the
    /// reconciler treat the still-armed registers as a confirmed Active
    /// window and leave the battery in the invalid armed-without-slot pause
    /// until the window ends. Instead the (already persisted) disabled
    /// desired state is kept and the machine is armed into `Exiting{0,0}`
    /// so the poll loop re-issues the exit writes immediately (the same
    /// repair path a failed Stop uses), bounded by the shared retry ladder.
    #[tokio::test]
    async fn rejected_final_slot_disarm_keeps_disabled_schedule_repairable() {
        with_isolated_config_dir_async(|| async {
            use crate::inverter::state_machines::TimedExportState;
            use crate::modbus::registers::HR_BATTERY_POWER_MODE;

            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let slot = crate::inverter::model::ScheduleSlot {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 0,
                target_soc: 50,
            };
            {
                let mut config = state.timed_export_config.lock().await;
                config.schedule_enabled = true;
                config.slots = vec![slot.clone()];
            }
            crate::settings::Settings::update(|settings| {
                settings.timed_export_schedule_enabled = true;
                settings.timed_export_slots = vec![slot];
            })
            .unwrap();
            // Snapshot must report the slot configured + discharge-enabled so
            // the disable path classifies the save as `should_return_to_eco`.
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                let snap = snapshot.as_mut().unwrap();
                snap.discharge_slots[0] = crate::inverter::model::ScheduleSlot {
                    enabled: true,
                    start_hour: 16,
                    start_minute: 0,
                    end_hour: 19,
                    end_minute: 0,
                    target_soc: 50,
                };
                snap.battery_power_mode = 0;
                snap.enable_discharge = true;
            }

            // Drive two completion-backed batches: the slot-clear succeeds,
            // the disarm fails. This is the exact race the original code
            // mishandled by leaving the schedule persisted disabled while
            // the inverter was still armed.
            let handle = tokio::spawn(set_discharge_slot(
                State(state.clone()),
                Json(serde_json::json!({
                    "slot": 1,
                    "enabled": false,
                    "start_hour": 16,
                    "end_hour": 19
                })),
            ));
            let mut seen = 0usize;
            loop {
                if handle.is_finished() {
                    break;
                }
                let mut pw = state.pending_writes.lock().await;
                if let Some(batch) = pw.iter_mut().find(|batch| batch.completion.is_some()) {
                    if let Some(tx) = batch.completion.take() {
                        let outcome = if seen == 0 {
                            WriteOutcome::Ok
                        } else {
                            WriteOutcome::Failed {
                                address: HR_BATTERY_POWER_MODE,
                                value: 1,
                                error: "rejected by inverter".to_string(),
                            }
                        };
                        seen += 1;
                        let _ = tx.send(outcome);
                    }
                }
                drop(pw);
                tokio::task::yield_now().await;
            }
            let (status, _) = handle.await.unwrap();
            assert_eq!(status, StatusCode::BAD_GATEWAY);

            // The disable must stick: schedule disabled in the live mirror…
            let config = state.timed_export_config.lock().await;
            assert!(
                !config.schedule_enabled,
                "disable must stick after a successful physical clear"
            );
            assert!(!config.slots.is_empty());
            assert!(!config.slots[0].enabled);
            drop(config);

            // …on disk…
            let on_disk = crate::settings::Settings::load();
            assert!(!on_disk.timed_export_schedule_enabled);
            assert!(!on_disk.timed_export_slots[0].enabled);

            // …and the machine must be armed into Exiting{0,0} so the next
            // poll re-issues the exit writes instead of going quiet.
            assert!(
                matches!(
                    *state.timed_export_state.lock().await,
                    TimedExportState::Exiting {
                        polls_waiting: 0,
                        retries: 0
                    }
                ),
                "machine must be armed into a fresh Exiting for poll-loop repair"
            );
        })
        .await;
    }

    /// BLOCKER: the action lock must serialise concurrent slot saves so the
    /// second handler cannot overwrite the first handler's desired slot in
    /// the in-memory mirror and on disk.
    #[tokio::test]
    async fn concurrent_set_discharge_slot_preserves_both_edits() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;

            // Seed a two-slot enabled schedule so both handlers can run
            // against distinct slot indices in parallel.
            let seed_slots = vec![
                crate::inverter::model::ScheduleSlot {
                    enabled: true,
                    start_hour: 16,
                    start_minute: 0,
                    end_hour: 17,
                    end_minute: 0,
                    target_soc: 100,
                },
                crate::inverter::model::ScheduleSlot {
                    enabled: true,
                    start_hour: 18,
                    start_minute: 0,
                    end_hour: 19,
                    end_minute: 0,
                    target_soc: 100,
                },
            ];
            {
                let mut config = state.timed_export_config.lock().await;
                config.schedule_enabled = true;
                config.slots = seed_slots.clone();
            }
            crate::settings::Settings::update(|settings| {
                settings.timed_export_schedule_enabled = true;
                settings.timed_export_slots = seed_slots;
            })
            .unwrap();
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                snapshot.as_mut().unwrap().discharge_slots[0].enabled = false;
                snapshot.as_mut().unwrap().discharge_slots[1].enabled = false;
            }

            let slot1 = tokio::spawn(set_discharge_slot(
                State(state.clone()),
                Json(serde_json::json!({
                    "slot": 1,
                    "enabled": true,
                    "start_hour": 6,
                    "start_minute": 0,
                    "end_hour": 7,
                    "end_minute": 0,
                    "target_soc": 50
                })),
            ));
            let slot2 = tokio::spawn(set_discharge_slot(
                State(state.clone()),
                Json(serde_json::json!({
                    "slot": 2,
                    "enabled": true,
                    "start_hour": 20,
                    "start_minute": 0,
                    "end_hour": 21,
                    "end_minute": 0,
                    "target_soc": 50
                })),
            ));

            // Drive completion channels: each save can queue up to two
            // completion-backed phases (slot writes, optional arm). Resolve
            // every one with Ok until the handler returns.
            for handle in [&slot1, &slot2] {
                loop {
                    if handle.is_finished() {
                        break;
                    }
                    let mut pw = state.pending_writes.lock().await;
                    if let Some(batch) = pw.iter_mut().find(|batch| batch.completion.is_some()) {
                        if let Some(tx) = batch.completion.take() {
                            let _ = tx.send(WriteOutcome::Ok);
                        }
                    }
                    drop(pw);
                    tokio::task::yield_now().await;
                }
            }

            let (status1, _) = slot1.await.unwrap();
            let (status2, _) = slot2.await.unwrap();
            assert_eq!(status1, StatusCode::OK);
            assert_eq!(status2, StatusCode::OK);

            // Both edits must be reflected on disk.
            let on_disk = crate::settings::Settings::load();
            assert!(on_disk.timed_export_schedule_enabled);
            assert_eq!(on_disk.timed_export_slots.len(), 2);
            assert_eq!(on_disk.timed_export_slots[0].start_hour, 6);
            assert_eq!(on_disk.timed_export_slots[0].end_hour, 7);
            assert_eq!(on_disk.timed_export_slots[1].start_hour, 20);
            assert_eq!(on_disk.timed_export_slots[1].end_hour, 21);

            // And in the live mirror.
            let mirror = state.timed_export_config.lock().await.slots.clone();
            assert_eq!(mirror.len(), 2);
            assert_eq!(mirror[0].start_hour, 6);
            assert_eq!(mirror[0].end_hour, 7);
            assert_eq!(mirror[1].start_hour, 20);
            assert_eq!(mirror[1].end_hour, 21);
        })
        .await;
    }

    // -----------------------------------------------------------------------
    // CODE_REVIEW.md BLOCKER 1: durable stop/exit-pending marker
    // -----------------------------------------------------------------------

    /// A crash between persisting `schedule_enabled = false` and the
    /// confirmed disarm must not boot the machine as `Off`. With the durable
    /// stop-pending marker set, the `AppState` constructors restore
    /// `Exiting{0,0}`, and the reconciler re-issues the exit writes even
    /// though the registers are still armed and the physical slots are still
    /// populated — the shape the external-owner suppression used to swallow.
    #[tokio::test]
    async fn restart_with_stop_pending_restores_exiting_and_repairs_armed_registers() {
        with_isolated_config_dir_async(|| async {
            use crate::inverter::state_machines::{
                check_timed_export, TimedExportState, TimedExportWriteOutcome,
            };
            // Persisted state at crash time: schedule disabled, desired slots
            // retained, exit still pending.
            crate::settings::Settings::update(|s| {
                s.timed_export_schedule_enabled = false;
                s.timed_export_slots = vec![slot(true, 16, 19)];
                s.timed_export_stop_pending = true;
            })
            .unwrap();

            // "Restart": a fresh AppState loads the marker.
            let state = Arc::new(crate::AppState::new());
            assert!(matches!(
                *state.timed_export_state.lock().await,
                TimedExportState::Exiting {
                    polls_waiting: 0,
                    retries: 0
                }
            ));
            assert!(state
                .timed_export_stop_pending
                .load(std::sync::atomic::Ordering::Acquire));
            {
                let config = state.timed_export_config.lock().await;
                assert!(!config.schedule_enabled);
                assert_eq!(config.slots.len(), 1);
                assert!(config.slots[0].enabled);
                assert!(config.stop_pending);
            }

            // Armed registers + populated physical slots: the shape that used
            // to be misread as another controller's schedule.
            let mut armed_snapshot = crate::inverter::model::InverterSnapshot {
                device_type: DeviceType::Gen3Hybrid,
                battery_power_mode: 0,
                enable_discharge: true,
                inverter_time: "2026-06-28 17:00:00".to_string(),
                ..Default::default()
            };
            armed_snapshot.discharge_slots[0] = slot(true, 16, 19);

            // Restart-restored `Exiting{0,0}` with no batch in flight must
            // re-issue the exit writes immediately.
            let config = state.timed_export_config.lock().await.clone();
            let mut machine = state.timed_export_state.lock().await.clone();
            let decision = check_timed_export(
                &armed_snapshot,
                &config,
                &mut machine,
                12 * 60,
                DeviceType::Gen3Hybrid,
                TimedExportWriteOutcome::NoneIssued,
                false,
            );
            assert!(
                !decision.writes.is_empty(),
                "a pending exit must be repaired, not suppressed"
            );
            assert!(matches!(machine, TimedExportState::Exiting { .. }));

            // The same armed shape with the machine parked on `Off` (what the
            // Stop path sets right after persisting the disable) must also
            // repair via the marker instead of going quiet.
            let mut machine = TimedExportState::Off;
            let decision = check_timed_export(
                &armed_snapshot,
                &config,
                &mut machine,
                12 * 60,
                DeviceType::Gen3Hybrid,
                TimedExportWriteOutcome::NoneIssued,
                false,
            );
            assert!(
                !decision.writes.is_empty(),
                "the stop-pending marker must bypass the external-owner suppression"
            );
            drop(config);

            // Once the readback confirms the Eco baseline, the machine
            // settles on `Off` — the point at which the poll loop clears the
            // durable marker.
            let mut eco_snapshot = armed_snapshot.clone();
            eco_snapshot.battery_power_mode = 1;
            eco_snapshot.enable_discharge = false;
            let config = state.timed_export_config.lock().await.clone();
            let mut machine = TimedExportState::Exiting {
                polls_waiting: 1,
                retries: 0,
            };
            let decision = check_timed_export(
                &eco_snapshot,
                &config,
                &mut machine,
                12 * 60,
                DeviceType::Gen3Hybrid,
                TimedExportWriteOutcome::Succeeded,
                false,
            );
            drop(config);
            assert!(
                matches!(machine, TimedExportState::Off),
                "confirmed Eco must settle the pending exit: {:?}",
                decision.log_message
            );
            crate::inverter::poll::clear_timed_export_stop_pending_if_settled(
                &state,
                &machine,
                &eco_snapshot,
            )
            .await;
            assert!(!state
                .timed_export_stop_pending
                .load(std::sync::atomic::Ordering::Acquire));
            assert!(!crate::settings::Settings::load().timed_export_stop_pending);

            // …and it must NOT clear while the readback is still armed.
            crate::settings::Settings::update(|s| s.timed_export_stop_pending = true).unwrap();
            state
                .timed_export_stop_pending
                .store(true, std::sync::atomic::Ordering::Release);
            crate::inverter::poll::clear_timed_export_stop_pending_if_settled(
                &state,
                &TimedExportState::Off,
                &armed_snapshot,
            )
            .await;
            assert!(
                state
                    .timed_export_stop_pending
                    .load(std::sync::atomic::Ordering::Acquire),
                "the marker must survive while the registers are still armed"
            );
            assert!(crate::settings::Settings::load().timed_export_stop_pending);
        })
        .await;
    }

    /// CODE_REVIEW.md BLOCKER: disabling one slot of a STOPPED multi-slot
    /// schedule must not re-derive the master flag from the surviving slot.
    /// The pre-edit master flag wins: the schedule stays off and the
    /// surviving slot is retained as desired-state data only.
    #[tokio::test]
    async fn disabling_slot_of_stopped_schedule_keeps_master_off() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let seed = vec![slot(true, 16, 19), slot(true, 20, 22)];
            {
                let mut config = state.timed_export_config.lock().await;
                config.schedule_enabled = false; // stopped
                config.slots = seed.clone();
            }
            crate::settings::Settings::update(|s| {
                s.timed_export_schedule_enabled = false;
                s.timed_export_slots = seed.clone();
            })
            .unwrap();
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                snapshot.as_mut().unwrap().discharge_slots[0] = seed[0].clone();
                snapshot.as_mut().unwrap().discharge_slots[1] = seed[1].clone();
            }

            let (status, _) = drive_set_discharge_slot_completion_with(
                &state,
                serde_json::json!({
                    "slot": 2,
                    "enabled": false,
                    "start_hour": 20,
                    "end_hour": 22
                }),
                WriteOutcome::Ok,
            )
            .await;
            assert_eq!(status, StatusCode::OK);

            let on_disk = crate::settings::Settings::load();
            assert!(
                !on_disk.timed_export_schedule_enabled,
                "a slot disable must not silently re-enable a stopped schedule"
            );
            assert!(!on_disk.timed_export_slots[1].enabled);
            assert!(
                on_disk.timed_export_slots[0].enabled,
                "the surviving slot is retained as desired-state data"
            );
            let mirror = state.timed_export_config.lock().await;
            assert!(!mirror.schedule_enabled);
            drop(mirror);
            // The edit turned the (already off) schedule's exit into a
            // durable pending one: the marker must be set so a crash before
            // any armed registers are repaired still resumes the exit.
            assert!(state
                .timed_export_stop_pending
                .load(std::sync::atomic::Ordering::Acquire));
        })
        .await;
    }

    /// CODE_REVIEW.md BLOCKER: a clear batch rejected after its first
    /// register landed must be compensated PHYSICALLY (the prior slot
    /// registers rewritten) before the desired schedule is rolled back —
    /// rolling back only the persisted state used to leave the inverter's
    /// window corrupted. The failing register here is the second of the
    /// batch (slot end), i.e. the slot start was already accepted.
    #[tokio::test]
    async fn rejected_second_clear_register_compensates_prior_physical_slot() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                encode_hhmm, HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START,
            };

            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let saved = slot(true, 16, 19);
            {
                let mut config = state.timed_export_config.lock().await;
                config.schedule_enabled = true;
                config.slots = vec![saved.clone()];
            }
            crate::settings::Settings::update(|s| {
                s.timed_export_schedule_enabled = true;
                s.timed_export_slots = vec![saved.clone()];
            })
            .unwrap();
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                snapshot.as_mut().unwrap().discharge_slots[0] = saved.clone();
            }

            // First completion-backed batch: the clear — its second register
            // (slot end) is rejected after the first (slot start) was
            // accepted. Second batch: the compensating restore — accepted.
            let (status, _) = drive_set_discharge_slot_completion_sequence(
                &state,
                serde_json::json!({
                    "slot": 1,
                    "enabled": false,
                    "start_hour": 16,
                    "end_hour": 19
                }),
                &[
                    WriteOutcome::Failed {
                        address: HR_DISCHARGE_SLOT_1_END,
                        value: 0,
                        error: "rejected by inverter".to_string(),
                    },
                    WriteOutcome::Ok,
                ],
            )
            .await;
            assert_eq!(status, StatusCode::BAD_GATEWAY);

            let writes = drain_pending_writes(&state).await;
            // The original clear attempt…
            let clear_start = writes
                .iter()
                .find(|w| w.address == HR_DISCHARGE_SLOT_1_START && w.value == 0);
            // …and the compensating restore of the prior physical slot.
            let compensated_start = writes
                .iter()
                .any(|w| w.address == HR_DISCHARGE_SLOT_1_START && w.value == encode_hhmm(16, 0));
            let compensated_end = writes
                .iter()
                .any(|w| w.address == HR_DISCHARGE_SLOT_1_END && w.value == encode_hhmm(19, 0));
            assert!(
                clear_start.is_some(),
                "the clear batch must have been queued: {writes:?}"
            );
            assert!(
                compensated_start && compensated_end,
                "the prior physical slot must be rewritten by the compensation batch: {writes:?}"
            );

            // The pre-edit desired schedule is restored on disk and in the
            // live mirror.
            let on_disk = crate::settings::Settings::load();
            assert!(on_disk.timed_export_schedule_enabled);
            assert!(on_disk.timed_export_slots[0].enabled);
            assert_eq!(
                on_disk.timed_export_slots[0].start_hour, 16,
                "the saved schedule must be left unchanged"
            );
            let mirror = state.timed_export_config.lock().await;
            assert!(mirror.schedule_enabled);
            assert!(mirror.slots[0].enabled);
        })
        .await;
    }

    /// CODE_REVIEW.md BLOCKER: when even the compensating batch is rejected,
    /// the registers cannot be trusted — the handler must disarm (best
    /// effort) and park the machine in a terminal `Error` instead of
    /// claiming the schedule was left unchanged.
    #[tokio::test]
    async fn failed_compensation_disarms_and_surfaces_terminal_error() {
        with_isolated_config_dir_async(|| async {
            use crate::inverter::state_machines::TimedExportState;
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_DISCHARGE_SLOT_1_END, HR_ENABLE_DISCHARGE,
            };

            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let saved = slot(true, 16, 19);
            {
                let mut config = state.timed_export_config.lock().await;
                config.schedule_enabled = true;
                config.slots = vec![saved.clone()];
            }
            crate::settings::Settings::update(|s| {
                s.timed_export_schedule_enabled = true;
                s.timed_export_slots = vec![saved.clone()];
            })
            .unwrap();
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                snapshot.as_mut().unwrap().discharge_slots[0] = saved.clone();
            }

            // 1. the clear batch fails mid-way, 2. the compensation batch is
            // rejected too, 3. the disarm batch is accepted.
            let (status, _) = drive_set_discharge_slot_completion_sequence(
                &state,
                serde_json::json!({
                    "slot": 1,
                    "enabled": false,
                    "start_hour": 16,
                    "end_hour": 19
                }),
                &[
                    WriteOutcome::Failed {
                        address: HR_DISCHARGE_SLOT_1_END,
                        value: 0,
                        error: "rejected by inverter".to_string(),
                    },
                    WriteOutcome::Failed {
                        address: HR_DISCHARGE_SLOT_1_END,
                        value: 0,
                        error: "rejected by inverter".to_string(),
                    },
                    WriteOutcome::Ok,
                ],
            )
            .await;
            assert_eq!(status, StatusCode::BAD_GATEWAY);

            let writes = drain_pending_writes(&state).await;
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1),
                "a best-effort disarm (HR27=1) must be issued: {writes:?}"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0),
                "a best-effort disarm (enable=0) must be issued: {writes:?}"
            );
            assert!(
                matches!(
                    *state.timed_export_state.lock().await,
                    TimedExportState::Error { .. }
                ),
                "the machine must surface a terminal error when compensation fails"
            );
        })
        .await;
    }

    /// CODE_REVIEW.md BLOCKER: an enabled-slot save whose physical batch is
    /// rejected after the first register (new slot start accepted, slot end
    /// rejected) must compensate the prior physical slot; the desired
    /// schedule was never committed, so it stays unchanged.
    #[tokio::test]
    async fn rejected_second_save_register_compensates_prior_physical_slot() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                encode_hhmm, HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START,
            };

            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let saved = slot(true, 16, 17);
            {
                let mut config = state.timed_export_config.lock().await;
                config.schedule_enabled = true;
                config.slots = vec![saved.clone()];
            }
            crate::settings::Settings::update(|s| {
                s.timed_export_schedule_enabled = true;
                s.timed_export_slots = vec![saved.clone()];
            })
            .unwrap();
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                snapshot.as_mut().unwrap().discharge_slots[0] = saved.clone();
            }

            // The save batch (new 18:00 start accepted, new 19:00 end
            // rejected), then the compensating restore of 16:00–17:00.
            let (status, _) = drive_set_discharge_slot_completion_sequence(
                &state,
                serde_json::json!({
                    "slot": 1,
                    "enabled": true,
                    "start_hour": 18,
                    "end_hour": 19
                }),
                &[
                    WriteOutcome::Failed {
                        address: HR_DISCHARGE_SLOT_1_END,
                        value: 1900,
                        error: "rejected by inverter".to_string(),
                    },
                    WriteOutcome::Ok,
                ],
            )
            .await;
            assert_eq!(status, StatusCode::BAD_GATEWAY);

            let writes = drain_pending_writes(&state).await;
            let attempted_new_start = writes
                .iter()
                .any(|w| w.address == HR_DISCHARGE_SLOT_1_START && w.value == encode_hhmm(18, 0));
            let compensated_start = writes
                .iter()
                .any(|w| w.address == HR_DISCHARGE_SLOT_1_START && w.value == encode_hhmm(16, 0));
            let compensated_end = writes
                .iter()
                .any(|w| w.address == HR_DISCHARGE_SLOT_1_END && w.value == encode_hhmm(17, 0));
            assert!(
                attempted_new_start,
                "the new slot start must have been queued: {writes:?}"
            );
            assert!(
                compensated_start && compensated_end,
                "the prior physical slot 16:00-17:00 must be restored: {writes:?}"
            );

            // The desired schedule was never committed and stays unchanged.
            let on_disk = crate::settings::Settings::load();
            assert!(on_disk.timed_export_schedule_enabled);
            assert_eq!(on_disk.timed_export_slots[0].start_hour, 16);
            assert_eq!(on_disk.timed_export_slots[0].end_hour, 17);
            let mirror = state.timed_export_config.lock().await;
            assert_eq!(mirror.slots[0].start_hour, 16);
            assert_eq!(mirror.slots[0].end_hour, 17);
        })
        .await;
    }

    /// CODE_REVIEW.md follow-up: the enabled-slot save's persist-failure
    /// branch. The physical batch landed but the schedule could not be
    /// persisted (injected `Settings::update` failure) — the handler must
    /// compensate the physical registers back to the prior slot and leave
    /// the desired schedule unchanged.
    #[tokio::test]
    async fn enabled_save_persist_failure_compensates_physical_slot() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                encode_hhmm, HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START,
            };
            use crate::settings::InjectUpdateFailures;

            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let saved = slot(true, 16, 17);
            {
                let mut config = state.timed_export_config.lock().await;
                config.schedule_enabled = true;
                config.slots = vec![saved.clone()];
            }
            crate::settings::Settings::update(|s| {
                s.timed_export_schedule_enabled = true;
                s.timed_export_slots = vec![saved.clone()];
            })
            .unwrap();
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                snapshot.as_mut().unwrap().discharge_slots[0] = saved.clone();
            }

            // The physical save batch succeeds; the FIRST settings update
            // inside the handler — the schedule persist — fails via
            // injection; the compensating restore succeeds.
            let _injection = InjectUpdateFailures::arm(1);
            let (status, _) = drive_set_discharge_slot_completion_sequence(
                &state,
                serde_json::json!({
                    "slot": 1,
                    "enabled": true,
                    "start_hour": 18,
                    "end_hour": 19
                }),
                &[WriteOutcome::Ok, WriteOutcome::Ok],
            )
            .await;
            drop(_injection);
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(
                InjectUpdateFailures::remaining(),
                0,
                "the injection must have been consumed by the persist attempt"
            );

            let writes = drain_pending_writes(&state).await;
            let attempted_new_start = writes
                .iter()
                .any(|w| w.address == HR_DISCHARGE_SLOT_1_START && w.value == encode_hhmm(18, 0));
            let compensated_start = writes
                .iter()
                .any(|w| w.address == HR_DISCHARGE_SLOT_1_START && w.value == encode_hhmm(16, 0));
            let compensated_end = writes
                .iter()
                .any(|w| w.address == HR_DISCHARGE_SLOT_1_END && w.value == encode_hhmm(17, 0));
            assert!(
                attempted_new_start,
                "the new slot start must have landed: {writes:?}"
            );
            assert!(
                compensated_start && compensated_end,
                "the prior physical slot must be restored after the persist failure: {writes:?}"
            );

            // The desired schedule was never committed and stays unchanged.
            let on_disk = crate::settings::Settings::load();
            assert!(on_disk.timed_export_schedule_enabled);
            assert_eq!(on_disk.timed_export_slots[0].start_hour, 16);
            assert_eq!(on_disk.timed_export_slots[0].end_hour, 17);
            let mirror = state.timed_export_config.lock().await;
            assert_eq!(mirror.slots[0].start_hour, 16);
            assert_eq!(mirror.slots[0].end_hour, 17);
        })
        .await;
    }

    /// CODE_REVIEW.md follow-up: when the schedule persist fails AND the
    /// compensating restore is rejected too, the registers cannot be
    /// trusted — the handler disarms (best effort) and parks the machine in
    /// a terminal `Error`.
    #[tokio::test]
    async fn enabled_save_persist_and_compensation_failure_parks_terminal_error() {
        with_isolated_config_dir_async(|| async {
            use crate::inverter::state_machines::TimedExportState;
            use crate::modbus::registers::{HR_BATTERY_POWER_MODE, HR_DISCHARGE_SLOT_1_END};
            use crate::settings::InjectUpdateFailures;

            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let saved = slot(true, 16, 17);
            {
                let mut config = state.timed_export_config.lock().await;
                config.schedule_enabled = true;
                config.slots = vec![saved.clone()];
            }
            crate::settings::Settings::update(|s| {
                s.timed_export_schedule_enabled = true;
                s.timed_export_slots = vec![saved.clone()];
            })
            .unwrap();
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                snapshot.as_mut().unwrap().discharge_slots[0] = saved.clone();
            }

            // 1. the physical save batch succeeds, 2. the persist fails
            // (injected), 3. the compensation batch is rejected, 4. the
            // disarm batch is accepted.
            let _injection = InjectUpdateFailures::arm(1);
            let (status, _) = drive_set_discharge_slot_completion_sequence(
                &state,
                serde_json::json!({
                    "slot": 1,
                    "enabled": true,
                    "start_hour": 18,
                    "end_hour": 19
                }),
                &[
                    WriteOutcome::Ok,
                    WriteOutcome::Failed {
                        address: HR_DISCHARGE_SLOT_1_END,
                        value: 1700,
                        error: "rejected by inverter".to_string(),
                    },
                    WriteOutcome::Ok,
                ],
            )
            .await;
            drop(_injection);
            assert_eq!(status, StatusCode::BAD_REQUEST);

            let writes = drain_pending_writes(&state).await;
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1),
                "a best-effort disarm (HR27=1) must be issued: {writes:?}"
            );
            assert!(
                matches!(
                    *state.timed_export_state.lock().await,
                    TimedExportState::Error { .. }
                ),
                "the machine must surface a terminal error when compensation fails"
            );
        })
        .await;
    }

    /// CODE_REVIEW.md MAJOR: the legacy Eco route must serialise on the
    /// Timed Export action lock — while the lock is held, Eco must not
    /// disable the managed schedule out from under a concurrent mutation.
    #[tokio::test]
    async fn set_mode_eco_waits_for_timed_export_action_lock() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let seed = vec![slot(true, 16, 19), slot(true, 20, 22)];
            {
                let mut config = state.timed_export_config.lock().await;
                config.schedule_enabled = true;
                config.slots = seed.clone();
            }
            crate::settings::Settings::update(|s| {
                s.timed_export_schedule_enabled = true;
                s.timed_export_slots = seed;
            })
            .unwrap();

            // Hold the same action lock the Timed Export mutations hold.
            let guard = state.timed_export_action_lock.lock().await;
            let handle = tokio::spawn(set_mode(
                State(state.clone()),
                Json(serde_json::json!({ "mode": "eco" })),
            ));
            for _ in 0..200 {
                if handle.is_finished() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(
                crate::settings::Settings::load().timed_export_schedule_enabled,
                "Eco must not disable the managed schedule while another mutation holds the action lock"
            );
            drop(guard);

            let (status, _) = handle.await.unwrap();
            assert_eq!(status, StatusCode::OK);
            assert!(
                !crate::settings::Settings::load().timed_export_schedule_enabled,
                "Eco disables the managed schedule once it owns the lock"
            );
            assert!(matches!(
                *state.timed_export_state.lock().await,
                crate::inverter::state_machines::TimedExportState::Off
            ));
        })
        .await;
    }

    #[tokio::test]
    async fn disabling_non_last_discharge_slot_keeps_timed_export_armed() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_BATTERY_POWER_MODE, HR_ENABLE_DISCHARGE};
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            seed_gen3_timed_pre_state(&state).await;

            let (status, _) = drive_set_discharge_slot_completion_with(
                &state,
                serde_json::json!({
                    "slot": 1,
                    "enabled": false,
                    "start_hour": 16,
                    "end_hour": 19
                }),
                WriteOutcome::Ok,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(!writes
                .iter()
                .any(|write| write.address == HR_ENABLE_DISCHARGE));
            assert!(!writes
                .iter()
                .any(|write| write.address == HR_BATTERY_POWER_MODE));
        })
        .await;
    }

    #[tokio::test]
    async fn timed_discharge_writes_pause_discharge_inverse_window() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_PAUSE_MODE, HR_BATTERY_PAUSE_SLOT_1_END, HR_BATTERY_PAUSE_SLOT_1_START,
            };
            // Residential All-in-One exposes confirmed writable pause slot
            // registers (HR318-320); AC-coupled is gated out because field
            // logs show HR319/320 reject writes with exception 1.
            let state = make_state_with_device(DeviceType::AllInOne6kW).await;
            let body = serde_json::json!({
                "enabled": true,
                "start_hour": 3,
                "start_minute": 0,
                "end_hour": 4,
                "end_minute": 0,
            });
            let _ = set_timed_discharge(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_BATTERY_PAUSE_SLOT_1_START)
                    .map(|w| w.value),
                Some(400)
            );
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_BATTERY_PAUSE_SLOT_1_END)
                    .map(|w| w.value),
                Some(300)
            );
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_BATTERY_PAUSE_MODE)
                    .map(|w| w.value),
                Some(2)
            );
        })
        .await;
    }

    // -- Timed Discharge device gating ------------------------------------
    //
    // The pause registers (HR318-320) are only safely writable for
    // AC-three-phase / residential All-in-One models, plus Gen3 Hybrid via
    // the targeted firmware-gated probe. AC-coupled models expose HR318 but
    // field logs show HR319/320 reject slot writes with exception 1. Every
    // other family is refused with HTTP 400 and queues NO writes.
    #[tokio::test]
    async fn timed_discharge_refused_on_gen1_hybrid() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen1Hybrid).await;
            let body = serde_json::json!({
                "enabled": true,
                "start_hour": 3,
                "end_hour": 4,
            });
            let (status, res) = set_timed_discharge(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(res.get("ok"), Some(&serde_json::Value::Bool(false)));
            // No pause-register writes must be queued on an unsupported device.
            assert!(drain_pending_writes(&state).await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn timed_discharge_refused_on_gen3_hybrid_after_ac_config_block_removal() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let body = serde_json::json!({ "enabled": true });
            let (status, _res) = set_timed_discharge(State(state.clone()), Json(body)).await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "Gen3 Hybrid must refuse now that it no longer polls HR 300-359"
            );
            assert!(drain_pending_writes(&state).await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn timed_discharge_refused_on_ac_coupled_three_phase_gateway_ems_and_pv() {
        with_isolated_config_dir_async(|| async {
            for dt in [
                DeviceType::ACCoupled,
                DeviceType::ACCoupledMk2,
                DeviceType::ThreePhase,
                DeviceType::HybridHvGen3,
                DeviceType::AllInOneHybrid,
                DeviceType::AioCommercial,
                DeviceType::Gateway,
                DeviceType::Ems,
                DeviceType::EmsCommercial,
                DeviceType::PvInverter,
            ] {
                let state = make_state_with_device(dt).await;
                let body = serde_json::json!({ "enabled": true });
                let (status, _res) =
                    set_timed_discharge(State(state.clone()), Json(body.clone())).await;
                assert_eq!(
                    status,
                    StatusCode::BAD_REQUEST,
                    "{dt:?} must refuse Timed Discharge"
                );
                assert!(
                    drain_pending_writes(&state).await.is_empty(),
                    "{dt:?} must queue no pause-register writes"
                );
            }
        })
        .await;
    }

    #[tokio::test]
    async fn timed_discharge_accepted_on_every_supported_device() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_BATTERY_PAUSE_MODE;
            // AC-three-phase / residential All-in-One expose confirmed
            // HR318-320 Timed Discharge slot support and must accept writes.
            for dt in [
                DeviceType::ACThreePhase,
                DeviceType::AllInOne6kW,
                DeviceType::AllInOne3_6kW,
                DeviceType::AllInOne5kW,
            ] {
                let state = make_state_with_device(dt).await;
                let body = serde_json::json!({
                    "enabled": true,
                    "start_hour": 3,
                    "end_hour": 4,
                });
                let (status, _res) =
                    set_timed_discharge(State(state.clone()), Json(body.clone())).await;
                assert_eq!(status, StatusCode::OK, "{dt:?} must accept Timed Discharge");
                let writes = drain_pending_writes(&state).await;
                assert_all_whitelisted(&writes);
                assert_eq!(
                    writes
                        .iter()
                        .find(|w| w.address == HR_BATTERY_PAUSE_MODE)
                        .map(|w| w.value),
                    Some(2),
                    "{dt:?} must arm pause-discharge (HR318=2)"
                );
            }
        })
        .await;
    }

    #[tokio::test]
    async fn timed_discharge_gen3_hybrid_gated_on_arm_firmware_312() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_BATTERY_PAUSE_MODE;
            // Gen3 Hybrid reaches the pause registers via the targeted
            // HR 318-320 probe, enabled only at ARM fw >= 312.
            for fw in [0u16, 300, 311] {
                let state = make_state_with_device_and_fw(DeviceType::Gen3Hybrid, fw).await;
                let (status, _res) = set_timed_discharge(
                    State(state.clone()),
                    Json(serde_json::json!({ "enabled": true })),
                )
                .await;
                assert_eq!(
                    status,
                    StatusCode::BAD_REQUEST,
                    "Gen3 fw {fw} must refuse Timed Discharge"
                );
                assert!(drain_pending_writes(&state).await.is_empty());
            }
            for fw in [312u16, 318, 399] {
                let state = make_state_with_device_and_fw(DeviceType::Gen3Hybrid, fw).await;
                let (status, _res) = set_timed_discharge(
                    State(state.clone()),
                    Json(serde_json::json!({
                        "enabled": true,
                        "start_hour": 3,
                        "end_hour": 4,
                    })),
                )
                .await;
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "Gen3 fw {fw} must accept Timed Discharge"
                );
                let writes = drain_pending_writes(&state).await;
                assert_all_whitelisted(&writes);
                assert_eq!(
                    writes
                        .iter()
                        .find(|w| w.address == HR_BATTERY_PAUSE_MODE)
                        .map(|w| w.value),
                    Some(2),
                    "Gen3 fw {fw} must arm pause-discharge (HR318=2)"
                );
            }
        })
        .await;
    }

    // -- set_mode register contract (issue #156) --------------------------
    //
    // The frontend fix for issue #156 will surface Timed Export as its own
    // selectable mode. The backend already routes `set_mode("timed_export")`
    // → `SetTimedExportMode`, but no test asserts the resulting register
    // writes — so a regression in the API routing or the encoder would slip
    // through silently. These tests pin the three-register contract for each
    // mode exactly as GivTCP's `set_mode_storage` / `set_mode_dynamic`
    // define it (see commands.py), with HR(27) as the sole demand/export
    // distinguisher.
    #[tokio::test]
    async fn set_mode_timed_demand_writes_match_demand_registers() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_BATTERY_SOC_RESERVE, HR_ENABLE_DISCHARGE,
            };
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let body = serde_json::json!({ "mode": "timed_demand", "soc_reserve": 12 });
            let _ = set_mode(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_BATTERY_POWER_MODE)
                    .expect("timed_demand must write HR27")
                    .value,
                1,
                "Timed Demand keeps match-demand (HR27=1) — battery covers home only"
            );
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_ENABLE_DISCHARGE)
                    .expect("timed_demand must write HR59")
                    .value,
                1,
                "Timed Demand arms the schedule (HR59=1)"
            );
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_BATTERY_SOC_RESERVE)
                    .expect("timed_demand must write HR110")
                    .value,
                12
            );
        })
        .await;
    }

    #[tokio::test]
    async fn set_mode_timed_export_routes_through_managed_schedule() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_BATTERY_SOC_RESERVE, HR_DISCHARGE_SLOT_1_START,
                HR_ENABLE_DISCHARGE,
            };
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;

            // With no schedule (persisted, backup, or live slots) the managed
            // path refuses: raw HR27=0/HR59=1 all-day export must not be
            // reachable via the mode endpoint (code-review finding: bypass
            // paths recreated the all-day maximum-power mode).
            let (status, response) = set_mode(
                State(state.clone()),
                Json(serde_json::json!({ "mode": "timed_export", "soc_reserve": 20 })),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert!(response.0["error"]
                .as_str()
                .is_some_and(|e| e.contains("at least one discharge slot")));
            assert!(drain_pending_writes(&state).await.is_empty());

            // With a persisted desired schedule whose window contains the
            // (snapshot-clock) present, the managed path arms export:
            // HR27=0, HR59=1, plus the requested SOC reserve.
            let (sh, sm, eh, em) = window_containing_now();
            state.timed_export_config.lock().await.slots.push(
                crate::inverter::model::ScheduleSlot {
                    enabled: true,
                    start_hour: sh,
                    start_minute: sm,
                    end_hour: eh,
                    end_minute: em,
                    target_soc: 4,
                },
            );
            let body = serde_json::json!({ "mode": "timed_export", "soc_reserve": 20 });
            let (status, response) = drive_set_timed_export_completion_sequence(
                &state,
                body,
                &[WriteOutcome::Ok, WriteOutcome::Ok],
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(response.0["ok"], json!(true));
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            let slot_index = writes
                .iter()
                .position(|w| w.address == HR_DISCHARGE_SLOT_1_START)
                .expect("persisted slot must be restored before arming");
            let mode_index = writes
                .iter()
                .position(|w| w.address == HR_BATTERY_POWER_MODE)
                .expect("timed_export must write HR27");
            assert!(
                slot_index < mode_index,
                "slot restore must precede export arm"
            );
            assert_eq!(writes[slot_index].value, 1600);
            // HR27=0 is the entire point of Timed Export: surplus battery
            // power exports to grid instead of only topping up home demand.
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_BATTERY_POWER_MODE)
                    .expect("timed_export must write HR27")
                    .value,
                0,
                "Timed Export uses max-power (HR27=0) — battery exports surplus to grid"
            );
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_ENABLE_DISCHARGE)
                    .expect("timed_export must write HR59")
                    .value,
                1,
                "Timed Export arms the schedule (HR59=1)"
            );
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_BATTERY_SOC_RESERVE)
                    .expect("timed_export must write HR110")
                    .value,
                20
            );
            // The managed schedule must be persisted as enabled so the poll
            // loop owns the window boundaries from here on.
            assert!(state.timed_export_config.lock().await.schedule_enabled);
            assert!(crate::settings::Settings::load().timed_export_schedule_enabled);
        })
        .await;
    }

    #[tokio::test]
    async fn set_mode_export_paused_writes_paused_registers() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_BATTERY_SOC_RESERVE, HR_ENABLE_DISCHARGE,
            };
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let body = serde_json::json!({ "mode": "export_paused", "soc_reserve": 4 });
            let _ = set_mode(State(state.clone()), Json(body)).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_BATTERY_POWER_MODE)
                    .expect("export_paused must write HR27")
                    .value,
                0,
                "Export Paused stays in the export family (HR27=0)"
            );
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_ENABLE_DISCHARGE)
                    .expect("export_paused must write HR59")
                    .value,
                0,
                "Export Paused disarms the schedule (HR59=0)"
            );
            assert_eq!(
                writes
                    .iter()
                    .find(|w| w.address == HR_BATTERY_SOC_RESERVE)
                    .expect("export_paused must write HR110")
                    .value,
                4
            );
        })
        .await;
    }

    /// Regression test for `set_mode` consuming the #137 discharge-slot
    /// backup BEFORE confirming the inverter accepted the slot-restore
    /// writes. Same bug class as 2877466 fixed for `set_timed_export`,
    /// which the author missed in the original sweep. Currently RED on
    /// master: `set_mode` calls `queue_writes` (no completion channel)
    /// AND `settings.save()` to clear the backup before any write
    /// confirmation could possibly arrive, so a rejected register write
    /// silently destroys the user's schedule.
    ///
    /// After the fix (queue_writes_with_completion + await_write_outcome),
    /// the backup survives a rejected write and the handler returns a
    /// 502 with the user's schedule intact for retry.
    #[tokio::test]
    async fn set_mode_timed_restore_keeps_backup_on_rejected_write() {
        use crate::settings::{DischargeSlotBackup, Settings};
        with_isolated_config_dir_async(|| async {
            // Seed a non-empty backup that the inverter would restore on
            // a Timed-mode entry without body slots.
            let mut settings = Settings::load();
            settings.discharge_slots_backup = Some(vec![DischargeSlotBackup {
                enabled: true,
                start_hour: 16,
                start_minute: 0,
                end_hour: 19,
                end_minute: 30,
                target_soc: 50,
            }]);
            settings.save().unwrap();

            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let body = serde_json::json!({
                "mode": "timed_demand",
                "soc_reserve": 12,
                // NOTE: no `discharge_slots` field — forces the backup-restore path.
            });

            // Drive the handler in a separate task so we can simulate the
            // poll loop rejecting the slot-restore write.
            let handle = tokio::spawn(set_mode(State(state.clone()), Json(body)));
            // Try to drive the completion channel for the slot-restore
            // write batch. With the current bug, set_mode uses
            // `queue_writes` (no completion channel), so the handler
            // completes immediately and we exit the loop. The backup is
            // then gone from disk (RED).
            let mut drained = false;
            for _ in 0..100 {
                if handle.is_finished() {
                    break;
                }
                let mut pw = state.pending_writes.lock().await;
                if let Some(batch) = pw.first_mut() {
                    if let Some(tx) = batch.completion.take() {
                        let _ = tx.send(WriteOutcome::Failed {
                            address: 0,
                            value: 0,
                            error: "simulated reject".to_string(),
                        });
                        drained = true;
                        break;
                    }
                }
                drop(pw);
                tokio::task::yield_now().await;
            }
            let (status, _) = handle.await.unwrap();

            // RED on master: backup was cleared by settings.save() inside
            // set_mode BEFORE the write was confirmed. GREEN after fix:
            // backup survives because Settings::update only runs on Ok(()).
            assert!(
                Settings::load()
                    .discharge_slots_backup
                    .as_ref()
                    .map(|v| !v.is_empty())
                    .unwrap_or(false),
                "discharge_slots_backup must survive a rejected slot-restore write so the user can retry"
            );
            // RED on master: handler returned 200 because it never
            // awaited confirmation. GREEN after fix: returns 502.
            assert_eq!(
                status,
                StatusCode::BAD_GATEWAY,
                "handler must surface rejected slot-restore as 502 (BAD_GATEWAY), not 200"
            );
            // Sanity: we successfully drove the completion channel
            // (test would be meaningless if it didn't engage the new code).
            assert!(
                drained,
                "test couldn't drive the completion channel — set_mode may not use queue_writes_with_completion yet"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn pause_battery_single_phase_uses_minimal_eco_paused_writes() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_BATTERY_SOC_RESERVE, HR_DISCHARGE_SLOT_1_START,
                HR_DISCHARGE_SLOT_2_START, HR_ENABLE_CHARGE, HR_ENABLE_DISCHARGE,
            };
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let _ = drive_pause_battery_completion(&state).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert_eq!(
                writes.len(),
                3,
                "pause should only toggle Eco Paused registers"
            );
            assert!(writes
                .iter()
                .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1));
            assert!(writes
                .iter()
                .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0));
            assert!(writes
                .iter()
                .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 100));
            assert!(!writes.iter().any(|w| w.address == HR_ENABLE_CHARGE));
            assert!(!writes
                .iter()
                .any(|w| w.address == HR_DISCHARGE_SLOT_1_START));
            assert!(!writes
                .iter()
                .any(|w| w.address == HR_DISCHARGE_SLOT_2_START));
        })
        .await;
    }

    #[tokio::test]
    async fn pause_battery_saves_previous_reserve_for_manual_unpause() {
        with_isolated_config_dir_async(|| async {
            use crate::inverter::poll::LoadLimiterSaved;
            use crate::modbus::registers::HR_BATTERY_SOC_RESERVE;

            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            {
                let mut snap = state.latest_snapshot.lock().await;
                snap.as_mut().expect("snapshot seeded").battery_reserve = 27;
            }

            let _ = drive_pause_battery_completion(&state).await;
            let _ = drain_pending_writes(&state).await;
            assert_eq!(
                *state.load_limiter_saved.lock().await,
                Some(LoadLimiterSaved { reserve: 27 })
            );
            assert_eq!(
                crate::settings::Settings::load().load_limiter_saved_reserve,
                Some(27)
            );

            let _ = drive_unpause_battery_completion(&state).await;
            let writes = drain_pending_writes(&state).await;
            assert!(writes
                .iter()
                .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 27));
        })
        .await;
    }

    #[tokio::test]
    async fn unpause_battery_restores_saved_load_limiter_reserve() {
        with_isolated_config_dir_async(|| async {
            use crate::inverter::poll::{LoadLimiterSaved, LoadLimiterState};
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_BATTERY_SOC_RESERVE, HR_ENABLE_DISCHARGE,
            };

            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            state.load_limiter_config.lock().await.enabled = true;
            *state.load_limiter_state.lock().await = LoadLimiterState::Paused;
            *state.load_limiter_saved.lock().await = Some(LoadLimiterSaved { reserve: 23 });
            let mut settings = crate::settings::Settings::load();
            settings.load_limiter_enabled = true;
            settings.load_limiter_active_persisted = true;
            settings.load_limiter_saved_reserve = Some(23);
            settings.save().expect("seed load limiter settings");

            let _ = drive_unpause_battery_completion(&state).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert_eq!(
                *state.load_limiter_state.lock().await,
                LoadLimiterState::Idle
            );
            assert!(state.load_limiter_saved.lock().await.is_none());
            assert!(!state.load_limiter_config.lock().await.enabled);
            let persisted = crate::settings::Settings::load();
            assert!(!persisted.load_limiter_enabled);
            assert!(!persisted.load_limiter_active_persisted);
            assert!(persisted.load_limiter_saved_reserve.is_none());
            assert!(writes
                .iter()
                .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1));
            assert!(writes
                .iter()
                .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0));
            assert!(writes
                .iter()
                .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 23));
        })
        .await;
    }

    #[tokio::test]
    async fn unpause_battery_disables_active_temperature_limiter() {
        with_isolated_config_dir_async(|| async {
            use crate::inverter::poll::{LoadLimiterSaved, TemperatureLimiterState};

            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            state.temperature_limiter_config.lock().await.enabled = true;
            *state.temperature_limiter_state.lock().await = TemperatureLimiterState::Paused;
            *state.load_limiter_saved.lock().await = Some(LoadLimiterSaved { reserve: 18 });
            let mut settings = crate::settings::Settings::load();
            settings.temperature_limiter_enabled = true;
            settings.temperature_limiter_active_persisted = true;
            settings.load_limiter_saved_reserve = Some(18);
            settings.save().expect("seed temperature limiter settings");

            let _ = drive_unpause_battery_completion(&state).await;
            assert!(!state.temperature_limiter_config.lock().await.enabled);
            assert_eq!(
                *state.temperature_limiter_state.lock().await,
                TemperatureLimiterState::Idle
            );
            let persisted = crate::settings::Settings::load();
            assert!(!persisted.temperature_limiter_enabled);
            assert!(!persisted.temperature_limiter_active_persisted);
        })
        .await;
    }

    #[tokio::test]
    async fn temperature_limiter_config_validates_and_persists() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "enabled": true,
                "high_threshold": 65.0,
                "recovery_threshold": 57.0,
                "confirmation_readings": 4
            });
            let (status, _) = set_temperature_limiter(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let config = state.temperature_limiter_config.lock().await.clone();
            assert!(config.enabled);
            assert_eq!(config.high_threshold, 65.0);
            assert_eq!(config.recovery_threshold, 57.0);
            assert_eq!(config.confirmation_readings, 4);
            let persisted = crate::settings::Settings::load();
            assert!(persisted.temperature_limiter_enabled);
            assert_eq!(persisted.temperature_limiter_high_threshold, 65.0);

            let invalid = serde_json::json!({
                "high_threshold": 55.0,
                "recovery_threshold": 55.0
            });
            let (status, _) = set_temperature_limiter(State(state.clone()), Json(invalid)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(
                state.temperature_limiter_config.lock().await.high_threshold,
                65.0
            );
        })
        .await;
    }

    #[tokio::test]
    async fn unpause_battery_falls_back_to_reserve_4() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_BATTERY_SOC_RESERVE;

            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let _ = drive_unpause_battery_completion(&state).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(writes
                .iter()
                .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 4));
        })
        .await;
    }

    #[tokio::test]
    async fn pause_battery_three_phase_uses_minimal_eco_paused_writes() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_3PH_AC_CHARGE_ENABLE, HR_3PH_BATTERY_SOC_RESERVE, HR_3PH_DISCHARGE_SLOT_1_START,
                HR_3PH_FORCE_CHARGE_ENABLE, HR_3PH_FORCE_DISCHARGE_ENABLE, HR_BATTERY_POWER_MODE,
                HR_ENABLE_DISCHARGE,
            };
            let state = make_state_with_device(DeviceType::ThreePhase).await;
            let _ = drive_pause_battery_completion(&state).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert_eq!(
                writes.len(),
                3,
                "pause should not clear schedules or force flags"
            );
            assert!(writes
                .iter()
                .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1));
            assert!(writes
                .iter()
                .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0));
            assert!(writes
                .iter()
                .any(|w| w.address == HR_3PH_BATTERY_SOC_RESERVE && w.value == 100));
            assert!(!writes
                .iter()
                .any(|w| w.address == HR_3PH_DISCHARGE_SLOT_1_START));
            assert!(!writes
                .iter()
                .any(|w| w.address == HR_3PH_FORCE_CHARGE_ENABLE));
            assert!(!writes
                .iter()
                .any(|w| w.address == HR_3PH_FORCE_DISCHARGE_ENABLE));
            assert!(!writes.iter().any(|w| w.address == HR_3PH_AC_CHARGE_ENABLE));
        })
        .await;
    }

    // -----------------------------------------------------------------------
    // Issue #137: switching to Eco (or Pause / Export Paused) currently
    // clears the discharge slot registers via the encoder, leaving the
    // user's previously-configured schedule irretrievable on the inverter
    // AND locking the Timed toggle out because `discharge_slots` is now
    // empty on the wire. The fix is to capture the discharge schedule
    // from `latest_snapshot` *before* clearing it, persist it on
    // `Settings`, and restore it atomically (before HR_ENABLE_DISCHARGE=1)
    // when the user switches back to Timed without an explicit body of
    // discharge_slots. These tests lock in the contract for that fix.
    //
    // They reference `crate::settings::Settings::discharge_slots_backup`,
    // which the fix is expected to add to `Settings` (mirroring the
    // existing `cosy_active_persisted` / `auto_winter_saved_target_soc`
    // pattern). The field must survive across restarts — read/write via
    // `Settings::load()` / `Settings::save()` — so a crash-recovery path
    // can re-read from the backup.
    // -----------------------------------------------------------------------

    /// Seed `latest_snapshot` with a Gen3 inverter carrying two enabled
    /// discharge slots and `enable_discharge=true` (i.e. the user was
    /// happily running Timed Demand with a real schedule). Helper used by
    /// the backup-and-restore tests below.
    async fn seed_gen3_timed_pre_state(state: &Arc<AppState>) {
        use crate::inverter::model::ScheduleSlot;
        let mut snap = crate::inverter::model::InverterSnapshot {
            device_type: DeviceType::Gen3Hybrid,
            enable_discharge: true,
            battery_power_mode: 1, // eco / self-consumption (Timed Demand)
            inverter_time: "2026-06-28 17:00:00".to_string(),
            max_discharge_slots: 10,
            charge_slots: Default::default(),
            discharge_slots: Default::default(),
            ..Default::default()
        };
        snap.discharge_slots[0] = ScheduleSlot {
            enabled: true,
            start_hour: 16,
            start_minute: 0,
            end_hour: 19,
            end_minute: 30,
            target_soc: 4,
        };
        snap.discharge_slots[1] = ScheduleSlot {
            enabled: true,
            start_hour: 21,
            start_minute: 0,
            end_hour: 23,
            end_minute: 0,
            target_soc: 4,
        };
        *state.latest_snapshot.lock().await = Some(snap);
    }

    /// Switching to `eco` must snapshot the user's existing discharge
    /// schedule into `Settings` *before* `clear_discharge_slot_writes`
    /// zeroes the slot registers on the inverter. This is the fix for
    /// the data-loss half of issue #137: without the backup, the schedule
    /// is unrecoverable and the Timed toggle becomes un-selectable.
    ///
    /// Mirrors how `cosy_active_persisted`, `auto_winter_saved_target_soc`,
    /// `load_limiter_saved_reserve`, etc. round-trip through
    /// `crate::settings::Settings::load()` / `Settings::save()`.
    #[tokio::test]
    async fn set_eco_backs_up_discharge_slots_before_clearing() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_END,
                HR_DISCHARGE_SLOT_2_START,
            };
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            seed_gen3_timed_pre_state(&state).await;

            let body = serde_json::json!({ "mode": "eco", "soc_reserve": 10 });
            let (status, response) = set_mode(State(state.clone()), Json(body)).await;

            // Response contract: 200 OK with the captured backup echoed so
            // the frontend can stage it as pending edits in the Eco UI.
            assert_eq!(status, StatusCode::OK);
            assert_eq!(response.0["ok"], serde_json::Value::Bool(true));
            let resp_backup = response.0["discharge_slots_backup"].as_array().expect(
                "eco response must include discharge_slots_backup when a schedule was captured",
            );
            assert_eq!(resp_backup.len(), 10, "response backup covers all 10 slots");
            assert_eq!(resp_backup[0]["start_hour"], 16);
            assert_eq!(resp_backup[0]["end_hour"], 19);
            assert_eq!(resp_backup[0]["end_minute"], 30);
            assert_eq!(resp_backup[1]["start_hour"], 21);

            // Existing contract: the four classic slot registers are zeroed.
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            for reg in [
                HR_DISCHARGE_SLOT_1_START,
                HR_DISCHARGE_SLOT_1_END,
                HR_DISCHARGE_SLOT_2_START,
                HR_DISCHARGE_SLOT_2_END,
            ] {
                let w = writes.iter().find(|w| w.address == reg);
                assert!(w.is_some(), "eco must still clear slot {}", reg);
                assert_eq!(w.unwrap().value, 0);
            }

            // New contract: the schedule has been captured into Settings.
            // The Settings struct lives on disk (not on AppState), so read
            // it back the same way the implementation will write it.
            let disk = crate::settings::Settings::load();
            let backup = disk
                .discharge_slots_backup
                .clone()
                .expect("eco must back up the previous discharge schedule");
            assert_eq!(backup.len(), 10, "backup must cover all 10 slots");
            assert_eq!(backup[0].start_hour, 16);
            assert_eq!(backup[0].start_minute, 0);
            assert_eq!(backup[0].end_hour, 19);
            assert_eq!(backup[0].end_minute, 30);
            assert!(backup[0].enabled);
            assert_eq!(backup[1].start_hour, 21);
            assert_eq!(backup[1].end_hour, 23);
            assert!(backup[1].enabled);
            // Unused slots must round-trip as disabled + 00:00–00:00 so a
            // restore path doesn't accidentally enable phantom slots.
            assert!(!backup[2].enabled);
            assert_eq!(backup[2].start_hour, 0);
            assert_eq!(backup[2].end_hour, 0);
        })
        .await;
    }

    /// After a backup, switching back to Timed (with no `discharge_slots`
    /// in the body) must atomically re-write the backed-up slots to the
    /// inverter BEFORE `HR_ENABLE_DISCHARGE=1`. This is the fix for the
    /// lock-out half of issue #137: with this, the user's schedule comes
    /// back and the next snapshot reports it as configured.
    #[tokio::test]
    async fn set_timed_restores_backed_up_slots_atomically() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_END,
                HR_DISCHARGE_SLOT_2_START, HR_ENABLE_DISCHARGE,
            };
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            seed_gen3_timed_pre_state(&state).await;

            // Step 1: Eco captures the schedule and clears the inverter.
            let _ = set_mode(
                State(state.clone()),
                Json(serde_json::json!({ "mode": "eco", "soc_reserve": 10 })),
            )
            .await;
            drain_pending_writes(&state).await; // drop the eco batch

            // Step 2: Timed Demand with NO body slots must restore from backup.
            let _ = set_mode(
                State(state.clone()),
                Json(serde_json::json!({ "mode": "timed_demand", "soc_reserve": 4 })),
            )
            .await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);

            // The slot registers must carry the backed-up values back.
            let s1 = writes
                .iter()
                .find(|w| w.address == HR_DISCHARGE_SLOT_1_START)
                .expect("restore must rewrite HR 56");
            assert_eq!(s1.value, 1600);
            let e1 = writes
                .iter()
                .find(|w| w.address == HR_DISCHARGE_SLOT_1_END)
                .expect("restore must rewrite HR 57");
            assert_eq!(e1.value, 1930);
            let s2 = writes
                .iter()
                .find(|w| w.address == HR_DISCHARGE_SLOT_2_START)
                .expect("restore must rewrite HR 44");
            assert_eq!(s2.value, 2100);
            let e2 = writes
                .iter()
                .find(|w| w.address == HR_DISCHARGE_SLOT_2_END)
                .expect("restore must rewrite HR 45");
            assert_eq!(e2.value, 2300);

            // Order matters: slots MUST appear before HR_ENABLE_DISCHARGE=1
            // so the inverter never asserts the master flag without slot
            // constraints (the same invariant `is_timed` already enforces
            // for the explicit `discharge_slots` body path).
            let pos_slot = writes
                .iter()
                .position(|w| w.address == HR_DISCHARGE_SLOT_1_START)
                .expect("slot 1 start must be in the batch");
            let pos_enable = writes
                .iter()
                .position(|w| w.address == HR_ENABLE_DISCHARGE)
                .expect("timed must set HR_ENABLE_DISCHARGE");
            assert!(
                pos_slot < pos_enable,
                "slot writes (pos {}) must precede HR_ENABLE_DISCHARGE=1 (pos {})",
                pos_slot,
                pos_enable
            );
            let enable = writes
                .iter()
                .find(|w| w.address == HR_ENABLE_DISCHARGE)
                .unwrap();
            assert_eq!(enable.value, 1);
        })
        .await;
    }

    /// Restore must skip unconfigured slots — slots with `enabled: false`
    /// and all-zero times. Writing zero to a register that's already
    /// zero is wasted Modbus traffic (~1.5s per write at the dongle's
    /// rate limit) and would balloon an Eco→Timed restore of a
    /// 10-slot Gen3 to ~30s when only slot 1 was actually configured
    /// by the user. Issue #137 fix for the downstream E2E
    /// timeout (`Eco → Timed Demand transition`).
    #[tokio::test]
    async fn restore_skips_unconfigured_slots() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_END,
                HR_DISCHARGE_SLOT_2_START,
            };
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            seed_gen3_timed_pre_state(&state).await;

            // Eco captures. The 10-element backup now contains 2 enabled
            // slots (indices 0-1) and 8 all-zero, disabled slots (2-9).
            let _ = set_mode(
                State(state.clone()),
                Json(serde_json::json!({ "mode": "eco", "soc_reserve": 10 })),
            )
            .await;
            drain_pending_writes(&state).await;

            // Sanity: disk has the full 10-element backup (the capture
            // step doesn't filter, only the restore step does — mirroring
            // how the snapshot is reported to the UI verbatim).
            let backup = crate::settings::Settings::load()
                .discharge_slots_backup
                .expect("eco must capture backup");
            assert_eq!(backup.len(), 10);
            assert!(!backup[2].enabled && backup[2].start_hour == 0);

            // Timed Demand restores only the configured slots.
            let _ = set_mode(
                State(state.clone()),
                Json(serde_json::json!({ "mode": "timed_demand", "soc_reserve": 4 })),
            )
            .await;
            let writes = drain_pending_writes(&state).await;

            // Configured slots 1-2 must be written.
            assert!(writes
                .iter()
                .any(|w| w.address == HR_DISCHARGE_SLOT_1_START));
            assert!(writes.iter().any(|w| w.address == HR_DISCHARGE_SLOT_1_END));
            assert!(writes
                .iter()
                .any(|w| w.address == HR_DISCHARGE_SLOT_2_START));
            assert!(writes.iter().any(|w| w.address == HR_DISCHARGE_SLOT_2_END));

            // Unconfigured slots must NOT be written — they would
            // re-zero already-zero registers, costing 1.5s per slot ×
            // 8 slots = 12s of wasted Modbus time.
            for addr in 60..=70u16 {
                // HR 60-70 are mid-poll-block; slot writes go to
                // HR 56/57/44/45 (slots 1-2) and HR 276+ (slots 3-10).
                // We check slot 3+ via the slot_num lookup. Quickest
                // way is to confirm there are no extended-slot writes
                // (HR 276+) since they're for slots 3-10.
                let _ = addr; // placeholder; real assertion below
            }
            let extended_slot_writes: Vec<u16> = writes
                .iter()
                .map(|w| w.address)
                .filter(|a| *a >= 276 && *a <= 298)
                .collect();
            assert!(
                extended_slot_writes.is_empty(),
                "restore must skip slots 3-10 when they're all-zero; got writes to {:?}",
                extended_slot_writes
            );
        })
        .await;
    }

    /// The backup must be cleared once it has been consumed by a restore,
    /// so the next Eco entry captures the *new* (post-Timed) state rather
    /// than silently restoring a stale snapshot from earlier in the day.
    #[tokio::test]
    async fn backup_cleared_after_restore() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            seed_gen3_timed_pre_state(&state).await;

            // Eco captures.
            let _ = set_mode(
                State(state.clone()),
                Json(serde_json::json!({ "mode": "eco", "soc_reserve": 10 })),
            )
            .await;
            assert!(
                crate::settings::Settings::load()
                    .discharge_slots_backup
                    .is_some(),
                "precondition: backup is populated after Eco"
            );
            drain_pending_writes(&state).await;

            // Timed restores and consumes the backup.
            let _ = set_mode(
                State(state.clone()),
                Json(serde_json::json!({ "mode": "timed_demand", "soc_reserve": 4 })),
            )
            .await;
            assert!(
                crate::settings::Settings::load()
                    .discharge_slots_backup
                    .is_none(),
                "backup must be cleared after a successful restore"
            );
        })
        .await;
    }

    /// When the user explicitly posts a `discharge_slots` array alongside
    /// the mode (the existing frontend round-trip via `pendingDischargeSlots`),
    /// the explicit payload must win over the backup. The backup must NOT
    /// be used AND must NOT be cleared (a subsequent explicit save or a
    /// crash-recovery path that re-reads from backup must still work).
    #[tokio::test]
    async fn set_timed_with_body_slots_does_not_use_backup() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START};
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            seed_gen3_timed_pre_state(&state).await;

            // Eco populates the backup with 16:00–19:30.
            let _ = set_mode(
                State(state.clone()),
                Json(serde_json::json!({ "mode": "eco", "soc_reserve": 10 })),
            )
            .await;
            drain_pending_writes(&state).await;

            // User edits a different schedule in the UI and posts it
            // explicitly. Backend must use 09:00–11:00, not the backup.
            let _ = set_mode(
                State(state.clone()),
                Json(serde_json::json!({
                    "mode": "timed_demand",
                    "soc_reserve": 4,
                    "discharge_slots": [{
                        "slot": 1, "enabled": true,
                        "start_hour": 9, "start_minute": 0,
                        "end_hour": 11, "end_minute": 0,
                        "target_soc": 100,
                    }],
                })),
            )
            .await;
            let writes = drain_pending_writes(&state).await;
            let s1 = writes
                .iter()
                .find(|w| w.address == HR_DISCHARGE_SLOT_1_START)
                .expect("slot 1 start must be in the batch");
            let e1 = writes
                .iter()
                .find(|w| w.address == HR_DISCHARGE_SLOT_1_END)
                .expect("slot 1 end must be in the batch");
            assert_eq!(
                s1.value, 900,
                "explicit body must win over backup (got backup's 16:00)"
            );
            assert_eq!(
                e1.value, 1100,
                "explicit body must win over backup (got backup's 19:30)"
            );
        })
        .await;
    }

    /// Pause Discharge no longer clears discharge slots, so it must not create
    /// a backup. The user's schedule should remain untouched on the inverter.
    #[tokio::test]
    async fn pause_battery_does_not_back_up_discharge_slots() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            seed_gen3_timed_pre_state(&state).await;

            let _ = drive_pause_battery_completion(&state).await;

            assert!(
                crate::settings::Settings::load()
                    .discharge_slots_backup
                    .is_none(),
                "pause should not create a discharge schedule backup because it no longer clears slots"
            );
        })
        .await;
    }

    /// Gen3 inverters support 10 discharge slots (HR 56–57, 44–45, and
    /// HR 276–298 for slots 3–10). `clear_discharge_slot_writes` zeroes
    /// every slot the model supports, so the backup MUST cover all 10
    /// slots — a partial backup would make the restore round-trip
    /// silently drop slots 3–10 (issue #289).
    #[tokio::test]
    async fn backup_covers_extended_slots_on_gen3() {
        with_isolated_config_dir_async(|| async {
            use crate::inverter::model::ScheduleSlot;
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;

            // Seed a schedule that uses slots 1, 2, AND 5.
            let mut snap = crate::inverter::model::InverterSnapshot {
                device_type: DeviceType::Gen3Hybrid,
                enable_discharge: true,
                max_discharge_slots: 10,
                charge_slots: Default::default(),
                discharge_slots: Default::default(),
                ..Default::default()
            };
            snap.discharge_slots[0] = ScheduleSlot {
                enabled: true,
                start_hour: 5,
                start_minute: 0,
                end_hour: 7,
                end_minute: 0,
                target_soc: 4,
            };
            snap.discharge_slots[1] = ScheduleSlot {
                enabled: true,
                start_hour: 11,
                start_minute: 0,
                end_hour: 13,
                end_minute: 0,
                target_soc: 4,
            };
            snap.discharge_slots[4] = ScheduleSlot {
                enabled: true,
                start_hour: 17,
                start_minute: 0,
                end_hour: 20,
                end_minute: 0,
                target_soc: 4,
            };
            *state.latest_snapshot.lock().await = Some(snap);

            let _ = set_mode(
                State(state.clone()),
                Json(serde_json::json!({ "mode": "eco", "soc_reserve": 10 })),
            )
            .await;

            let backup = crate::settings::Settings::load()
                .discharge_slots_backup
                .clone()
                .expect("eco must back up slots even on Gen3 10-slot models");
            assert_eq!(backup.len(), 10);
            assert_eq!(backup[0].start_hour, 5);
            assert_eq!(backup[1].start_hour, 11);
            assert_eq!(
                backup[4].start_hour, 17,
                "slot 5 lives in HR 276 and must round-trip through the backup"
            );
            assert_eq!(backup[4].end_hour, 20);
            assert!(backup[4].enabled);
        })
        .await;
    }

    /// When the user starts with NO discharge schedule configured and
    /// switches to Eco, the backup must end up as `None` (or empty) so
    /// that a subsequent Timed toggle doesn't restore a phantom schedule.
    /// Without this guard, a no-op Eco would still unlock the Timed
    /// button via a stale `None→Some` transition.
    #[tokio::test]
    async fn eco_with_no_existing_schedule_does_not_create_backup() {
        with_isolated_config_dir_async(|| async {
            // Default snapshot: discharge_slots all disabled, start==end==0.
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;

            let (status, response) = set_mode(
                State(state.clone()),
                Json(serde_json::json!({ "mode": "eco", "soc_reserve": 4 })),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(response.0["ok"], serde_json::Value::Bool(true));
            // No schedule was captured → response must NOT carry a
            // discharge_slots_backup key (or it must be null). The frontend
            // uses presence-of-field as the signal to surface slots as
            // pending edits; a stale/empty field would re-create the bug.
            assert!(
                response.0.get("discharge_slots_backup").is_none()
                    || response.0["discharge_slots_backup"].is_null(),
                "eco with empty schedule must not echo discharge_slots_backup in response"
            );

            let backup = crate::settings::Settings::load()
                .discharge_slots_backup
                .clone();
            // Either None, or Some(vec with no enabled slot) — both are
            // acceptable representations of "nothing to back up". A future
            // Timed toggle must not see this as a reason to enable slots.
            match backup {
                None => { /* fine — no backup created */ }
                Some(v) => assert!(
                    v.iter().all(|s| !s.enabled),
                    "backup from an empty schedule must contain only disabled slots"
                ),
            }
        })
        .await;
    }

    /// Pause Discharge should not echo `discharge_slots_backup`: it does not
    /// clear slots anymore, so there is nothing for the frontend to stage.
    #[tokio::test]
    async fn pause_battery_response_does_not_echo_backup() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            seed_gen3_timed_pre_state(&state).await;

            let (status, response) = drive_pause_battery_completion(&state).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(response.0["ok"], serde_json::Value::Bool(true));
            assert!(response.0.get("discharge_slots_backup").is_none());
        })
        .await;
    }

    /// Timed mode responses must NOT include `discharge_slots_backup` —
    /// it's only meaningful on the Eco/Pause transitions where the
    /// schedule is being captured (not on Timed, where it's either
    /// restored from a backup or written fresh from the body).
    #[tokio::test]
    async fn set_timed_response_does_not_echo_backup() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            seed_gen3_timed_pre_state(&state).await;

            let (status, response) = set_mode(
                State(state.clone()),
                Json(serde_json::json!({ "mode": "timed_demand", "soc_reserve": 4 })),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(response.0["ok"], serde_json::Value::Bool(true));
            assert!(
                response.0.get("discharge_slots_backup").is_none(),
                "timed response must not include discharge_slots_backup (it would be stale data)"
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
    async fn pause_battery_uses_consistent_device_type_for_reserve_register() {
        // Regression guard for the pause_battery routing: the reserve register
        // must come from the same locked device-type view as the power-mode
        // writes. The action no longer clears schedule slots.
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_3PH_BATTERY_SOC_RESERVE, HR_BATTERY_POWER_MODE, HR_BATTERY_SOC_RESERVE,
                HR_DISCHARGE_SLOT_1_START,
            };
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let _ = drive_pause_battery_completion(&state).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(writes
                .iter()
                .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 100));
            assert!(writes
                .iter()
                .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1));
            assert!(!writes
                .iter()
                .any(|w| w.address == HR_DISCHARGE_SLOT_1_START));

            let state = make_state_with_device(DeviceType::ThreePhase).await;
            let _ = drive_pause_battery_completion(&state).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert!(writes
                .iter()
                .any(|w| w.address == HR_3PH_BATTERY_SOC_RESERVE && w.value == 100));
            assert!(!writes.iter().any(|w| w.address == HR_BATTERY_SOC_RESERVE));
            assert!(writes
                .iter()
                .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1));
        })
        .await;
    }

    // -----------------------------------------------------------------------
    // Additional device-type routing tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn pause_battery_gen2_hybrid_uses_minimal_eco_paused_writes() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_BATTERY_SOC_RESERVE, HR_DISCHARGE_SLOT_1_START,
                HR_ENABLE_DISCHARGE,
            };
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let _ = drive_pause_battery_completion(&state).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert_eq!(writes.len(), 3);
            assert!(writes
                .iter()
                .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1));
            assert!(writes
                .iter()
                .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0));
            assert!(writes
                .iter()
                .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 100));
            assert!(!writes
                .iter()
                .any(|w| w.address == HR_DISCHARGE_SLOT_1_START));
        })
        .await;
    }

    #[tokio::test]
    async fn pause_battery_ac_coupled_uses_minimal_eco_paused_writes() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_BATTERY_POWER_MODE, HR_BATTERY_SOC_RESERVE, HR_DISCHARGE_SLOT_1_START,
                HR_ENABLE_DISCHARGE,
            };
            let state = make_state_with_device(DeviceType::ACCoupled).await;
            let _ = drive_pause_battery_completion(&state).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert_eq!(writes.len(), 3);
            assert!(writes
                .iter()
                .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1));
            assert!(writes
                .iter()
                .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0));
            assert!(writes
                .iter()
                .any(|w| w.address == HR_BATTERY_SOC_RESERVE && w.value == 100));
            assert!(!writes
                .iter()
                .any(|w| w.address == HR_DISCHARGE_SLOT_1_START));
        })
        .await;
    }

    #[tokio::test]
    async fn pause_battery_returns_ok_when_writes_are_accepted() {
        // Issue #245: when every register in the pause batch is accepted by
        // the inverter, the endpoint must return 200 with the same success
        // message as before the write-completion plumbing was added.
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let (status, response) = drive_pause_battery_completion(&state).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(response["ok"], json!(true));
            assert_eq!(response["message"], json!("Battery paused"));
        })
        .await;
    }

    #[tokio::test]
    async fn pause_battery_surfaces_register_rejection_error() {
        // Issue #245: when the inverter rejects one of the pause writes (here
        // the SOC-reserve register HR 110, exactly the failure seen in the
        // user's logs), the endpoint must return 400 naming the failing
        // register and the exception — instead of leaving the UI to time out
        // after 30 s with a generic "did not confirm" message.
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_BATTERY_SOC_RESERVE;
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let (status, response) = drive_pause_battery_completion_with(
                &state,
                WriteOutcome::Failed {
                    address: HR_BATTERY_SOC_RESERVE,
                    value: 100,
                    error: "Modbus exception: function 0x86, code 0".to_string(),
                },
            )
            .await;

            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "a rejected write must surface as a 400, got {status}"
            );
            assert_eq!(response["ok"], json!(false));
            let error = response["error"].as_str().expect("error message present");
            assert!(
                error.contains("110"),
                "error must name the failing register (110), got: {error}"
            );
            assert!(
                error.contains("code 0"),
                "error must include the exception detail, got: {error}"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn pause_battery_refuses_while_disconnected() {
        // Issue #245 follow-up: when the backend has no inverter connection,
        // pause must fail fast with a precise error instead of queueing
        // writes that would sit idle (and time out the frontend's
        // confirmation). No write should be queued.
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            *state.connection_state.lock().await = ConnectionState::Disconnected;
            let (status, response) = pause_battery(State(state.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(response["ok"], json!(false));
            let error = response["error"].as_str().expect("error message present");
            assert!(
                error.contains("not connected"),
                "error must explain the cause, got: {error}"
            );
            // Refusal happens before any write is queued.
            assert!(
                drain_pending_writes(&state).await.is_empty(),
                "no writes should be queued while disconnected"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn pause_battery_allows_reconnecting_state() {
        // The connection guard refuses only Disconnected. A transient
        // Reconnecting blip must still proceed — the write queues and runs
        // once the link comes back, so the user isn't blocked by a brief
        // dongle drop right when they hit Pause.
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            *state.connection_state.lock().await = ConnectionState::Reconnecting;
            let (status, response) = drive_pause_battery_completion(&state).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(response["ok"], json!(true));
            assert!(
                !drain_pending_writes(&state).await.is_empty(),
                "writes should still be queued while Reconnecting"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn await_write_outcome_returns_error_when_channel_dropped() {
        // If the poll loop is dropped (or the batch removed) before sending an
        // outcome, the receiver resolves with a closed-channel error.
        // await_write_outcome must surface that as an Err rather than hanging
        // or silently treating it as success.
        let (tx, rx) = tokio::sync::oneshot::channel::<WriteOutcome>();
        drop(tx);
        let result = await_write_outcome(rx).await;
        let msg = result.expect_err("a dropped sender must surface as an error");
        assert!(
            msg.contains("dropped"),
            "error should explain the batch was dropped, got: {msg}"
        );
    }

    #[tokio::test]
    async fn await_write_outcome_treats_timeout_as_pending_success() {
        // A slow-but-progressing poll loop must not produce a false rejection;
        // the frontend's snapshot confirmation remains responsible for the
        // eventual result after the API wait expires.
        let (_tx, rx) = tokio::sync::oneshot::channel::<WriteOutcome>();
        let result = await_write_outcome_with_timeout(rx, Duration::from_millis(1)).await;
        assert_eq!(result, Ok(()));
    }

    #[tokio::test]
    async fn required_write_outcome_treats_timeout_as_error() {
        let (_tx, rx) = tokio::sync::oneshot::channel::<WriteOutcome>();
        let result = await_required_write_outcome_with_timeout(rx, Duration::from_millis(1)).await;
        let error = result.expect_err("a transactional write timeout must reject the save");
        assert!(
            error.contains("did not complete"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn extended_slot_restore_timeout_covers_mandatory_write_gaps() {
        let slots = vec![
            crate::inverter::model::ScheduleSlot {
                enabled: true,
                start_hour: 1,
                start_minute: 0,
                end_hour: 2,
                end_minute: 0,
                target_soc: 20,
            };
            4
        ];
        let writes = crate::inverter::state_machines::build_timed_export_slot_restore_writes(
            DeviceType::Gen3Hybrid,
            &slots,
        );
        assert_eq!(writes.len(), 12, "four slots restore start/end/target");
        let timeout = batch_completion_timeout(writes.len());
        let mandatory_gaps = Duration::from_millis((writes.len() as u64 - 1) * 1500);
        assert!(
            timeout > mandatory_gaps,
            "transaction timeout {timeout:?} must exceed the {mandatory_gaps:?} pacing floor"
        );
        assert_eq!(timeout, Duration::from_secs(33));
    }

    #[tokio::test]
    async fn unpause_battery_refuses_while_disconnected() {
        // Symmetric guard on unpause: fail fast while offline, before any
        // limiter state is mutated.
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            *state.connection_state.lock().await = ConnectionState::Disconnected;
            let (status, response) = unpause_battery(State(state.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(response["ok"], json!(false));
            let error = response["error"].as_str().expect("error message present");
            assert!(
                error.contains("not connected"),
                "error must explain the cause, got: {error}"
            );
            assert!(
                drain_pending_writes(&state).await.is_empty(),
                "no writes should be queued while disconnected"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn unpause_battery_returns_ok_when_writes_are_accepted() {
        // Symmetric to pause: when every register in the unpause batch is
        // accepted, the endpoint returns 200 with the resume message.
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let (status, response) = drive_unpause_battery_completion(&state).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(response["ok"], json!(true));
            assert!(
                response["message"].as_str().unwrap().contains("unpaused"),
                "success message should mention unpaused, got: {}",
                response["message"]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn unpause_battery_surfaces_register_rejection_error() {
        // Issue #245 follow-up: when the inverter rejects the reserve-restore
        // write, unpause must surface a precise 400 (the limiter was already
        // disabled as intended) rather than reporting success the UI can't
        // confirm.
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::HR_BATTERY_SOC_RESERVE;
            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            let (status, response) = drive_unpause_battery_completion_with(
                &state,
                WriteOutcome::Failed {
                    address: HR_BATTERY_SOC_RESERVE,
                    value: 4,
                    error: "Modbus exception: function 0x86, code 0".to_string(),
                },
            )
            .await;

            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "a rejected write must surface as a 400, got {status}"
            );
            assert_eq!(response["ok"], json!(false));
            let error = response["error"].as_str().expect("error message present");
            assert!(
                error.contains("110"),
                "error must name the failing register (110), got: {error}"
            );
            assert!(
                error.contains("code 0"),
                "error must include the exception detail, got: {error}"
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
    async fn timed_export_three_phase_reserve_lands_before_export_arm() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_3PH_BATTERY_SOC_RESERVE, HR_BATTERY_POWER_MODE, HR_BATTERY_SOC_RESERVE,
            };

            let state = make_state_with_device(DeviceType::ThreePhase).await;
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                snapshot.as_mut().unwrap().discharge_slots[0] =
                    crate::inverter::model::ScheduleSlot {
                        enabled: true,
                        start_hour: 16,
                        start_minute: 0,
                        end_hour: 19,
                        end_minute: 0,
                        target_soc: 20,
                    };
            }

            let (status, response) = drive_set_timed_export_completion_with(
                &state,
                json!({ "enabled": true, "soc_reserve": 30 }),
                WriteOutcome::Ok,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "got {response:?}");

            let writes = drain_pending_writes(&state).await;
            let reserve_index = writes
                .iter()
                .position(|write| write.address == HR_3PH_BATTERY_SOC_RESERVE)
                .expect("three-phase reserve must target HR1109");
            let arm_index = writes
                .iter()
                .position(|write| write.address == HR_BATTERY_POWER_MODE && write.value == 0)
                .expect("Timed Export arm must write HR27=0");
            assert!(
                reserve_index < arm_index,
                "reserve setup must complete before maximum-power export is armed"
            );
            assert!(
                !writes
                    .iter()
                    .any(|write| write.address == HR_BATTERY_SOC_RESERVE),
                "three-phase Timed Export must never route reserve to HR110"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn timed_export_persistence_failure_happens_before_export_arm() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_BATTERY_POWER_MODE, HR_ENABLE_DISCHARGE};

            let state = make_state_with_device(DeviceType::Gen3Hybrid).await;
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                snapshot.as_mut().unwrap().discharge_slots[0] =
                    crate::inverter::model::ScheduleSlot {
                        enabled: true,
                        start_hour: 16,
                        start_minute: 0,
                        end_hour: 19,
                        end_minute: 0,
                        target_soc: 20,
                    };
            }

            let dir = crate::settings::Settings::settings_dir();
            let permissions = std::fs::metadata(&dir).unwrap().permissions();
            let mut readonly = permissions.clone();
            std::os::unix::fs::PermissionsExt::set_mode(&mut readonly, 0o500);
            std::fs::set_permissions(&dir, readonly).unwrap();

            let (status, response) = drive_set_timed_export_completion_with(
                &state,
                json!({ "enabled": true }),
                WriteOutcome::Ok,
            )
            .await;
            std::fs::set_permissions(&dir, permissions).unwrap();

            assert_eq!(status, StatusCode::BAD_REQUEST, "got {response:?}");
            let writes = drain_pending_writes(&state).await;
            assert!(
                !writes
                    .iter()
                    .any(
                        |write| (write.address == HR_BATTERY_POWER_MODE && write.value == 0)
                            || (write.address == HR_ENABLE_DISCHARGE && write.value == 1)
                    ),
                "persistence must fail before any maximum-power arm writes: {writes:?}"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn clear_discharge_slot_writes_three_phase() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_3PH_DISCHARGE_SLOT_1_END, HR_3PH_DISCHARGE_SLOT_1_START,
                HR_3PH_DISCHARGE_SLOT_2_END, HR_3PH_DISCHARGE_SLOT_2_START,
                HR_DISCHARGE_SLOT_10_END, HR_DISCHARGE_SLOT_3_START, SAFE_WRITE_REGS,
            };

            let writes = clear_discharge_slot_writes(DeviceType::ThreePhase);
            assert_eq!(writes.len(), 20, "three-phase clears 10 slots x start/end");
            for w in &writes {
                assert_eq!(w.value, 0);
                assert!(SAFE_WRITE_REGS.contains(&w.address));
            }
            let addrs: Vec<u16> = writes.iter().map(|w| w.address).collect();
            assert!(addrs.contains(&HR_3PH_DISCHARGE_SLOT_1_START));
            assert!(addrs.contains(&HR_3PH_DISCHARGE_SLOT_1_END));
            assert!(addrs.contains(&HR_3PH_DISCHARGE_SLOT_2_START));
            assert!(addrs.contains(&HR_3PH_DISCHARGE_SLOT_2_END));
            // Extended slots 3–10 must be cleared too (issue #289).
            assert!(addrs.contains(&HR_DISCHARGE_SLOT_3_START));
            assert!(addrs.contains(&HR_DISCHARGE_SLOT_10_END));
        })
        .await;
    }

    /// Issue #289: Gen3-family firmware re-asserts `enable_discharge`
    /// whenever ANY discharge slot register is non-zero (the #248 quirk),
    /// and Gen3 supports ten discharge slots — slots 3–10 live in the
    /// extended HR 276–298 bank. Clearing only the classic 1–2 pair leaves
    /// those extended slots armed, so the firmware keeps bouncing the
    /// inverter out of Eco. The clear must cover every slot the model
    /// supports (classic 1–2 registers + the extended 3–10 bank).
    #[test]
    fn clear_discharge_slots_covers_extended_slots_on_gen3() {
        use crate::modbus::registers::{
            HR_DISCHARGE_SLOT_10_END, HR_DISCHARGE_SLOT_10_START, HR_DISCHARGE_SLOT_3_END,
            HR_DISCHARGE_SLOT_3_START, SAFE_WRITE_REGS,
        };

        let writes = clear_discharge_slot_writes(DeviceType::Gen3Hybrid);
        assert_eq!(writes.len(), 20, "Gen3 clears 10 slots x start/end");
        for w in &writes {
            assert_eq!(w.value, 0);
            assert!(
                SAFE_WRITE_REGS.contains(&w.address),
                "address {} must be whitelisted",
                w.address
            );
        }
        // Extended slots 3–10 (HR 276–298) must be zeroed, not just the
        // classic 1–2 pair.
        for reg in [
            HR_DISCHARGE_SLOT_3_START,
            HR_DISCHARGE_SLOT_3_END,
            HR_DISCHARGE_SLOT_10_START,
            HR_DISCHARGE_SLOT_10_END,
        ] {
            assert!(
                writes.iter().any(|w| w.address == reg && w.value == 0),
                "Gen3 clear must zero extended slot register {reg}"
            );
        }

        // Three-phase models route slots 1–2 through HR 1118-1121 and
        // 3–10 through the same extended bank — they must clear all ten
        // as well.
        let writes = clear_discharge_slot_writes(DeviceType::ThreePhase);
        assert_eq!(writes.len(), 20, "three-phase clears 10 slots x start/end");
        assert!(
            writes
                .iter()
                .any(|w| w.address == HR_DISCHARGE_SLOT_10_END && w.value == 0),
            "three-phase clear must zero extended slot registers"
        );
    }

    /// The clear loop's slot range comes from
    /// `DeviceType::max_discharge_slots()`, which returns 1 for
    /// AC-coupled and Gen1 (matching the givenergy-modbus reference —
    /// those devices expose a single discharge slot). The clear must
    /// therefore write ONLY slot 1's pair (HR 56-57) — exactly two
    /// writes, with slot 2's registers (HR 44-45) absent. Pinning this
    /// matters in both directions: a future change to the model's slot
    /// count must not silently grow or shrink the clear set for
    /// single-slot devices.
    #[test]
    fn clear_discharge_slots_on_single_slot_devices_writes_only_slot_1() {
        use crate::modbus::registers::{
            HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_START,
            SAFE_WRITE_REGS,
        };

        for dt in [
            DeviceType::ACCoupled,
            DeviceType::ACCoupledMk2,
            DeviceType::Gen1Hybrid,
        ] {
            let writes = clear_discharge_slot_writes(dt);
            assert_eq!(
                writes.len(),
                2,
                "{dt:?} has one discharge slot: clear is 1 slot x start/end"
            );
            let addrs: Vec<u16> = writes.iter().map(|w| w.address).collect();
            assert_eq!(
                addrs,
                vec![HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_1_END]
            );
            for w in &writes {
                assert_eq!(w.value, 0);
                assert!(
                    SAFE_WRITE_REGS.contains(&w.address),
                    "address {} must be whitelisted",
                    w.address
                );
            }
            // Slot 2 must NOT be touched on single-slot devices.
            assert!(
                !writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_2_START),
                "{dt:?} clear must not write slot 2 registers"
            );
        }
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
                let (status, payload) = set_eps(State(state.clone()), Json(body)).await;
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
                    dt, payload
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
                let (status, payload) = set_eps(State(state.clone()), Json(body)).await;
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
                payload["error"].as_str().unwrap_or("").contains("Missing"),
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
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 1),
                "stop should restore enable_discharge to its pre-force value (1) — \
                 ForceCharge start clears it, and not restoring it leaves the \
                 user's discharge schedule silently disabled"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_CHARGE && w.value == 1),
                "stop should restore enable_charge to its pre-force value (1)"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_CHARGE_TARGET && w.value == 1),
                "stop should restore enable_charge_target to match enable_charge"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_CHARGE_TARGET_SOC && w.value == 60),
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
            assert!(
                revert.enable_discharge,
                "pre-state had enable_discharge=true"
            );
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
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 1),
                "stop should restore enable_discharge to its pre-force value (1)"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_CHARGE && w.value == 1),
                "stop should restore enable_charge to its pre-force value (1)"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_CHARGE_TARGET && w.value == 1),
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
            use crate::modbus::registers::{
                HR_3PH_FORCE_CHARGE_ENABLE, HR_3PH_FORCE_DISCHARGE_ENABLE,
            };

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
    async fn force_discharge_stop_after_restart_queues_minimal_single_phase_stop() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_BATTERY_POWER_MODE, HR_ENABLE_DISCHARGE};

            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                let snapshot = snapshot.as_mut().expect("snapshot seeded");
                snapshot.enable_discharge = true;
            }
            assert!(
                state.force_discharge_revert.lock().await.is_none(),
                "restart scenario has no in-memory revert"
            );

            let (status, payload) = force_discharge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK, "stop should succeed: {:?}", payload);
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert_eq!(writes.len(), 2);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_ENABLE_DISCHARGE && w.value == 0),
                "restart fallback must clear HR59"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1),
                "restart fallback must return to eco/self-consumption"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_discharge_stop_after_restart_queues_minimal_three_phase_stop() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{HR_3PH_FORCE_DISCHARGE_ENABLE, HR_BATTERY_POWER_MODE};

            let state = make_state_with_device(DeviceType::ThreePhase).await;
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                let snapshot = snapshot.as_mut().expect("snapshot seeded");
                snapshot.enable_discharge = true;
            }
            assert!(
                state.force_discharge_revert.lock().await.is_none(),
                "restart scenario has no in-memory revert"
            );

            let (status, payload) = force_discharge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK, "stop should succeed: {:?}", payload);
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);
            assert_eq!(writes.len(), 2);
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_3PH_FORCE_DISCHARGE_ENABLE && w.value == 0),
                "restart fallback must clear the three-phase force-discharge flag"
            );
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_BATTERY_POWER_MODE && w.value == 1),
                "restart fallback must return to eco/self-consumption"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_discharge_stop_is_one_shot_per_revert_once_snapshot_is_inactive() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            seed_discharging_pre_state(&state).await;
            let _ = force_discharge(State(state.clone()), None).await;
            let _ = drain_pending_writes(&state).await;

            let (status, _) = force_discharge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);
            let _ = drain_pending_writes(&state).await;

            // The restart fallback intentionally allows Stop Discharge to
            // work when there is no in-memory revert but the latest snapshot
            // still shows discharge armed. Once a post-stop poll has observed
            // HR59=0, a second stop should return to the old no-op/error path.
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                let snapshot = snapshot.as_mut().expect("snapshot seeded");
                snapshot.enable_discharge = false;
            }

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
            let _ =
                force_discharge(State(state.clone()), Some(Json(json!({ "minutes": 60 })))).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);

            // Slot 1 start must be "now" (we can't pin the exact value
            // without freezing Local::now, so just assert the writes are
            // present and not the encoder's default of 0/2359).
            assert!(
                writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_1_START),
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

    /// The minutes path appends the duration slot writes *before* running
    /// `ForceDischarge::encode()`, whose own slot writes (the 00:00/23:59
    /// full-day default for slot 1, plus a cleared slot 2) would otherwise
    /// land later in the queue. The poll loop drains writes and applies them
    /// sequentially with no deduplication (last write to a given address
    /// wins on the wire — see `poll.rs`), so those encoder defaults would
    /// clobber the duration slot. The handler's strip removes them.
    ///
    /// The sibling test above reads slot 1 via `Iterator::find`, which
    /// returns the *first* match (the duration value), so it still passes
    /// if the strip is deleted. This test pins the strip itself: each slot
    /// register must appear exactly once. We count rather than check values
    /// both to be value-agnostic about the regression and to sidestep the
    /// midnight / 23:59 edge cases where a duration value can legitimately
    /// equal the encoder default.
    #[tokio::test]
    async fn force_discharge_with_minutes_strips_encoder_slot_writes() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_DISCHARGE_SLOT_1_END, HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_END,
                HR_DISCHARGE_SLOT_2_START,
            };

            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            let _ =
                force_discharge(State(state.clone()), Some(Json(json!({ "minutes": 60 })))).await;
            let writes = drain_pending_writes(&state).await;
            assert_all_whitelisted(&writes);

            for addr in [
                HR_DISCHARGE_SLOT_1_START,
                HR_DISCHARGE_SLOT_1_END,
                HR_DISCHARGE_SLOT_2_START,
                HR_DISCHARGE_SLOT_2_END,
            ] {
                let count = writes.iter().filter(|w| w.address == addr).count();
                assert_eq!(
                    count, 1,
                    "slot register {addr} must appear exactly once after the strip; \
                     a count of 2 means ForceDischarge::encode()'s full-day slot writes \
                     were not dropped and would overwrite the duration slot on the wire"
                );
            }
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
            let _ =
                force_discharge(State(state.clone()), Some(Json(json!({ "minutes": 30 })))).await;
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

            let _ =
                force_discharge(State(state.clone()), Some(Json(json!({ "minutes": 30 })))).await;
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
            let _ =
                force_discharge(State(state.clone()), Some(Json(json!({ "minutes": 60 })))).await;
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
                !writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_1_START),
                "3PH force discharge must not write classic HR 56"
            );
            assert!(
                !writes.iter().any(|w| w.address == HR_DISCHARGE_SLOT_1_END),
                "3PH force discharge must not write classic HR 57"
            );
            assert!(
                !writes
                    .iter()
                    .any(|w| w.address == HR_DISCHARGE_SLOT_2_START),
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

    /// Three-phase mirror of `force_discharge_with_minutes_writes_slot_before_enable`:
    /// the 3PH discharge slot registers must be written before the
    /// `HR_3PH_FORCE_DISCHARGE_ENABLE` flag is armed, so the inverter
    /// has a slot to gate once force-discharge turns on.
    #[tokio::test]
    async fn force_discharge_with_minutes_writes_slot_before_enable_three_phase() {
        with_isolated_config_dir_async(|| async {
            use crate::modbus::registers::{
                HR_3PH_DISCHARGE_SLOT_1_END, HR_3PH_DISCHARGE_SLOT_1_START,
                HR_3PH_FORCE_DISCHARGE_ENABLE,
            };

            let state = make_state_with_device(DeviceType::ThreePhase).await;
            let _ =
                force_discharge(State(state.clone()), Some(Json(json!({ "minutes": 30 })))).await;
            let writes = drain_pending_writes(&state).await;

            let start_idx = writes
                .iter()
                .position(|w| w.address == HR_3PH_DISCHARGE_SLOT_1_START)
                .expect("3PH force discharge with minutes must write slot 1 start (HR 1118)");
            let end_idx = writes
                .iter()
                .position(|w| w.address == HR_3PH_DISCHARGE_SLOT_1_END)
                .expect("3PH force discharge with minutes must write slot 1 end (HR 1119)");
            let enable_idx = writes
                .iter()
                .position(|w| w.address == HR_3PH_FORCE_DISCHARGE_ENABLE)
                .expect("3PH force discharge with minutes must arm force-discharge flag");

            assert!(
                start_idx < enable_idx,
                "3PH slot 1 start must be written before HR_3PH_FORCE_DISCHARGE_ENABLE=1"
            );
            assert!(
                end_idx < enable_idx,
                "3PH slot 1 end must be written before HR_3PH_FORCE_DISCHARGE_ENABLE=1"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn force_discharge_is_rejected_until_force_charge_is_stopped() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            seed_charging_pre_state(&state).await;

            let (status, _) =
                force_charge(State(state.clone()), Some(Json(json!({ "minutes": 30 })))).await;
            assert_eq!(status, StatusCode::OK);
            let _ = drain_pending_writes(&state).await;

            let (status, body) = force_discharge(State(state.clone()), None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(
                body.0["error"],
                "Stop Force Charge before starting Force Discharge"
            );
            assert!(state.force_discharge_revert.lock().await.is_none());
            assert!(drain_pending_writes(&state).await.is_empty());

            let (status, _) = force_charge_stop(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);
            let _ = drain_pending_writes(&state).await;

            let (status, _) = force_discharge(State(state.clone()), None).await;
            assert_eq!(status, StatusCode::OK);
            assert!(state.force_discharge_revert.lock().await.is_some());
        })
        .await;
    }

    #[tokio::test]
    async fn force_charge_is_rejected_until_force_discharge_is_stopped() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            seed_charging_pre_state(&state).await;

            let (status, _) = force_discharge(State(state.clone()), None).await;
            assert_eq!(status, StatusCode::OK);
            let _ = drain_pending_writes(&state).await;

            let (status, body) =
                force_charge(State(state.clone()), Some(Json(json!({ "minutes": 30 })))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(
                body.0["error"],
                "Stop Force Discharge before starting Force Charge"
            );
            assert!(state.force_charge_revert.lock().await.is_none());
            assert!(drain_pending_writes(&state).await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn concurrent_opposite_force_starts_only_arm_one_action() {
        with_isolated_config_dir_async(|| async {
            let state = make_state_with_device(DeviceType::Gen2Hybrid).await;
            seed_charging_pre_state(&state).await;

            let (charge, discharge) = tokio::join!(
                force_charge(State(state.clone()), Some(Json(json!({ "minutes": 30 })))),
                force_discharge(State(state.clone()), None),
            );
            let statuses = [charge.0, discharge.0];
            assert_eq!(
                statuses
                    .iter()
                    .filter(|status| **status == StatusCode::OK)
                    .count(),
                1
            );
            assert_eq!(
                statuses
                    .iter()
                    .filter(|status| **status == StatusCode::BAD_REQUEST)
                    .count(),
                1
            );
            assert_ne!(
                state.force_charge_revert.lock().await.is_some(),
                state.force_discharge_revert.lock().await.is_some(),
                "exactly one force action should own a revert"
            );
            assert_eq!(state.pending_writes.lock().await.len(), 1);
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

    #[tokio::test]
    async fn set_alerts_persists_inverter_trip_toggle() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({ "inverter_trip_enabled": true });
            let (status, _) = set_alerts(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let cfg = state.alert_config.lock().await.clone();
            assert!(cfg.inverter_trip_enabled);

            let reloaded = crate::settings::Settings::load();
            assert!(reloaded.alerts_config.inverter_trip_enabled);
        })
        .await;
    }

    #[tokio::test]
    async fn set_alerts_persists_inverter_temperature_bounds() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "inverter_temp_min": 7.5,
                "inverter_temp_max": 62.0,
            });
            let (status, _) = set_alerts(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let cfg = state.alert_config.lock().await.clone();
            assert_eq!(cfg.inverter_temp_min, 7.5);
            assert_eq!(cfg.inverter_temp_max, 62.0);

            let reloaded = crate::settings::Settings::load();
            assert_eq!(reloaded.alerts_config.inverter_temp_min, 7.5);
            assert_eq!(reloaded.alerts_config.inverter_temp_max, 62.0);
        })
        .await;
    }

    #[tokio::test]
    async fn set_alerts_clamps_inverter_temperature_bounds() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "inverter_temp_min": -10.0,
                "inverter_temp_max": 999.0,
            });
            let (status, _) = set_alerts(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let cfg = state.alert_config.lock().await.clone();
            assert_eq!(cfg.inverter_temp_min, 0.0);
            assert_eq!(cfg.inverter_temp_max, 120.0);
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

            // POST an unrelated field with no Pushover or inverter-temp keys in the body.
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
            assert!(!cfg.inverter_trip_enabled);
            assert_eq!(cfg.inverter_temp_min, 8.0);
            assert_eq!(cfg.inverter_temp_max, 60.0);
        })
        .await;
    }

    /// Regression test for review finding #1 (week of 2026-08-22):
    /// two API handlers that each do `Settings::load` → mutate one field →
    /// `Settings::save` lose one of the two updates when they race on the
    /// same baseline (the second save overwrites the first's field). The
    /// poll-path writers were already converted to the transactional
    /// `Settings::update` helper in 78ba6fd; this pins the same property for
    /// the API handlers — `set_alerts` and `set_auto_winter` touch
    /// disjoint fields, so both updates must survive.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_set_alerts_and_set_auto_winter_preserve_both_updates() {
        use std::sync::Arc as StdArc;
        with_isolated_config_dir_async(|| async {
            // Seed the settings file so both handlers load a non-default
            // baseline. Use distinct sentinel values for the two fields we
            // expect to mutate, plus a few unrelated fields we expect to
            // remain untouched (guards against accidental clobber).
            let mut baseline = crate::settings::Settings::load();
            baseline.auto_winter_enabled = false;
            baseline.auto_winter_target_soc = 80;
            baseline.auto_winter_cold_threshold = 5.0;
            baseline.alerts_config.enabled = false;
            baseline.alerts_config.cooldown_minutes = 30;
            baseline.alerts_config.inverter_trip_enabled = false;
            baseline.save().unwrap();

            let state = Arc::new(AppState::new());
            // The handlers under test use the in-memory config on AppState
            // for field reads but write the *persisted* settings file. Seed
            // AppState's mirror so the body fields actually apply (the
            // handlers read current values from AppState before merging the
            // body — without seeding, both bodies merge over defaults).
            {
                let mut aw = state.auto_winter_config.lock().await;
                aw.enabled = false;
                aw.target_soc = 80;
                aw.cold_threshold = 5.0;
            }
            {
                let mut cfg = state.alert_config.lock().await;
                cfg.enabled = false;
                cfg.cooldown_minutes = 30;
                cfg.inverter_trip_enabled = false;
            }

            let barrier = StdArc::new(std::sync::Barrier::new(3));

            // set_auto_winter: bump target_soc to 50 + flip enabled to true.
            // The barrier must be hit BEFORE the handler runs — otherwise
            // both handlers would complete sequentially and never race.
            let state_aw = state.clone();
            let barrier_aw = barrier.clone();
            let h_aw = tokio::spawn(async move {
                barrier_aw.wait();
                set_auto_winter(
                    State(state_aw.clone()),
                    Json(serde_json::json!({
                        "enabled": true,
                        "target_soc": 50,
                    })),
                )
                .await
            });

            // set_alerts: flip enabled to true and change cooldown.
            let state_al = state.clone();
            let barrier_al = barrier.clone();
            let h_al = tokio::spawn(async move {
                barrier_al.wait();
                set_alerts(
                    State(state_al.clone()),
                    Json(serde_json::json!({
                        "enabled": true,
                        "cooldown_minutes": 17,
                    })),
                )
                .await
            });

            // Release both handlers together so they race on the same
            // settings baseline.
            barrier.wait();
            let (status_aw, _) = h_aw.await.unwrap();
            let (status_al, _) = h_al.await.unwrap();
            assert_eq!(status_aw, StatusCode::OK);
            assert_eq!(status_al, StatusCode::OK);

            // Both updates must survive. A lost-update bug shows up as one
            // field here holding the baseline value (e.g. target_soc=80
            // instead of 50) — the second save overwrote the first's field.
            let saved = crate::settings::Settings::load();
            assert!(
                saved.auto_winter_enabled,
                "auto_winter_enabled must be true after concurrent set_auto_winter"
            );
            assert_eq!(
                saved.auto_winter_target_soc, 50,
                "auto_winter_target_soc must reflect the concurrent update (not be clobbered by set_alerts' save)"
            );
            assert!(
                saved.alerts_config.enabled,
                "alerts_config.enabled must be true after concurrent set_alerts"
            );
            assert_eq!(
                saved.alerts_config.cooldown_minutes, 17,
                "alerts_config.cooldown_minutes must reflect the concurrent update (not be clobbered by set_auto_winter's save)"
            );
            // Untouched fields must remain at baseline.
            assert_eq!(saved.auto_winter_cold_threshold, 5.0);
            assert!(!saved.alerts_config.inverter_trip_enabled);
        })
        .await;
    }

    /// Companion regression to
    /// `concurrent_set_alerts_and_set_auto_winter_preserve_both_updates`:
    /// `set_load_limiter` and `set_temperature_limiter` are a different
    /// pair of API handlers that each `Settings::load` → mutate one field →
    /// `Settings::save` disjoint fields. The same lost-update class would
    /// apply if either handler is still on the old pattern.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_set_load_limiter_and_set_temperature_limiter_preserve_both_updates() {
        use std::sync::Arc as StdArc;
        with_isolated_config_dir_async(|| async {
            // Seed baseline settings with non-default sentinels on every
            // field we touch — the failure mode shows up as a sentinel
            // surviving instead of the new value (or vice-versa).
            let mut baseline = crate::settings::Settings::load();
            baseline.load_limiter_enabled = false;
            baseline.load_limiter_threshold_w = 2000;
            baseline.load_limiter_start_hour = 7;
            baseline.load_limiter_start_minute = 30;
            baseline.temperature_limiter_enabled = false;
            baseline.temperature_limiter_high_threshold = 50.0;
            baseline.temperature_limiter_recovery_threshold = 40.0;
            baseline.save().unwrap();

            let state = Arc::new(AppState::new());
            // Mirror the baseline into the AppState configs the handlers
            // merge over (the handlers start from state.<config>.lock() and
            // apply body fields on top — without the mirror, the body
            // values would land on top of defaults rather than the seeded
            // baseline).
            {
                let mut ll = state.load_limiter_config.lock().await;
                ll.enabled = false;
                ll.threshold_w = 2000;
                ll.start_hour = 7;
                ll.start_minute = 30;
            }
            {
                let mut tl = state.temperature_limiter_config.lock().await;
                tl.enabled = false;
                tl.high_threshold = 50.0;
                tl.recovery_threshold = 40.0;
            }

            let barrier = StdArc::new(std::sync::Barrier::new(3));

            // set_load_limiter: bump threshold_w to 4000 + flip enabled.
            let state_ll = state.clone();
            let barrier_ll = barrier.clone();
            let h_ll = tokio::spawn(async move {
                barrier_ll.wait();
                set_load_limiter(
                    State(state_ll.clone()),
                    Json(serde_json::json!({
                        "enabled": true,
                        "threshold_w": 4000,
                    })),
                )
                .await
            });

            // set_temperature_limiter: bump high_threshold to 55.0 + flip
            // enabled. The high_threshold and load_limiter_threshold_w
            // are independent settings, so both updates must survive.
            let state_tl = state.clone();
            let barrier_tl = barrier.clone();
            let h_tl = tokio::spawn(async move {
                barrier_tl.wait();
                set_temperature_limiter(
                    State(state_tl.clone()),
                    Json(serde_json::json!({
                        "enabled": true,
                        "high_threshold": 55.0,
                    })),
                )
                .await
            });

            barrier.wait();
            let (status_ll, _) = h_ll.await.unwrap();
            let (status_tl, _) = h_tl.await.unwrap();
            assert_eq!(status_ll, StatusCode::OK);
            assert_eq!(status_tl, StatusCode::OK);

            let saved = crate::settings::Settings::load();
            assert!(
                saved.load_limiter_enabled,
                "load_limiter_enabled must be true after concurrent set_load_limiter"
            );
            assert_eq!(
                saved.load_limiter_threshold_w, 4000,
                "load_limiter_threshold_w must reflect the concurrent update"
            );
            assert!(
                saved.temperature_limiter_enabled,
                "temperature_limiter_enabled must be true after concurrent set_temperature_limiter"
            );
            assert!(
                (saved.temperature_limiter_high_threshold - 55.0).abs() < 0.01,
                "temperature_limiter_high_threshold must reflect the concurrent update"
            );
            // Sentinels for fields neither body supplied must remain at
            // the seeded baseline (rules out accidental clobber).
            assert_eq!(saved.load_limiter_start_hour, 7);
            assert_eq!(saved.load_limiter_start_minute, 30);
            assert!(
                (saved.temperature_limiter_recovery_threshold - 40.0).abs() < 0.01,
                "untouched temperature recovery_threshold must survive"
            );
        })
        .await;
    }

    /// Regression test for review #1 (third cycle): `update_settings`
    /// (POST /api/settings) is the big multi-field handler — it reads the
    /// current settings to apply per-field defaults, then writes the
    /// complete settings struct. Two concurrent calls each updating a
    /// disjoint field must both survive on disk.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_update_settings_calls_preserve_disjoint_field_updates() {
        use std::sync::Arc as StdArc;
        with_isolated_config_dir_async(|| async {
            // Seed baseline settings with non-default sentinels on the
            // fields we expect to be updated + a few untouched sentinels.
            let mut baseline = crate::settings::Settings::load();
            baseline.host = "0.0.0.0".to_string();
            baseline.port = 8899;
            baseline.import_tariff = 0.25;
            baseline.export_tariff = 0.15;
            baseline.poll_interval = 15;
            baseline.http_port = 7337;
            baseline.save().unwrap();

            let state = Arc::new(AppState::new());
            // Mirror the baseline into the in-memory PollSettings so the
            // handler's defaults read matches the seeded disk state.
            {
                let mut s = state.settings.lock().await;
                s.host = baseline.host.clone();
                s.port = baseline.port;
                s.interval_secs = baseline.poll_interval;
            }

            let barrier = StdArc::new(std::sync::Barrier::new(3));

            // Caller A: change host only. Without transactional save, the
            // caller-B save would overwrite the host change (because A
            // saves the full Settings struct including whatever default
            // import_tariff it loaded from disk before B's update landed).
            let state_a = state.clone();
            let barrier_a = barrier.clone();
            let h_a = tokio::spawn(async move {
                barrier_a.wait();
                update_settings(
                    State(state_a.clone()),
                    Json(serde_json::json!({
                        "host": "192.168.1.10",
                        // serial must be present (or explicit empty) so
                        // the handler doesn't reuse the baseline serial.
                        "serial": "TEST-A",
                    })),
                )
                .await
            });

            // Caller B: change import_tariff only.
            let state_b = state.clone();
            let barrier_b = barrier.clone();
            let h_b = tokio::spawn(async move {
                barrier_b.wait();
                update_settings(
                    State(state_b.clone()),
                    Json(serde_json::json!({
                        "import_tariff": 0.42,
                    })),
                )
                .await
            });

            barrier.wait();
            let (status_a, _) = h_a.await.unwrap();
            let (status_b, _) = h_b.await.unwrap();
            assert_eq!(status_a, StatusCode::OK);
            assert_eq!(status_b, StatusCode::OK);

            let saved = crate::settings::Settings::load();
            // Both updates must survive. A lost-update bug shows up as one
            // field holding the baseline (host=0.0.0.0 or import_tariff=0.25).
            assert_eq!(
                saved.host, "192.168.1.10",
                "host must reflect caller-A's update"
            );
            assert!(
                (saved.import_tariff - 0.42).abs() < 0.0001,
                "import_tariff must reflect caller-B's update"
            );
            // Untouched fields must remain at baseline.
            assert_eq!(saved.port, 8899);
            assert!((saved.export_tariff - 0.15).abs() < 0.0001);
            assert_eq!(saved.poll_interval, 15);
            assert_eq!(saved.http_port, 7337);
        })
        .await;
    }

    /// Regression test for the review finding: `update_settings` captures the
    /// on-disk `import_tariff_config` / `export_tariff_config` default BEFORE
    /// acquiring the settings lock (to keep file I/O off the Tokio worker),
    /// then the closure writes it back UNCONDITIONALLY when the body omits
    /// the field. A concurrent `update_settings` that supplies a NEW tariff
    /// config in that window is reverted to the stale pre-lock default — the
    /// same clobber class the sweep fixed for the scalar `import_tariff` /
    /// `export_tariff` fields (which were made conditional). Host-only
    /// (no tariff field) races config-write and must NOT revert it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 32)]
    async fn concurrent_update_settings_cannot_revert_stale_tariff_config() {
        use std::sync::Arc as StdArc;
        with_isolated_config_dir_async(|| async {
            // Seed a pre-existing tariff config so the host-only writers'
            // pre-lock default read sees it (Some) and thus, under the buggy
            // code, unconditionally re-writes it (reverting any concurrent
            // config update).
            let mut baseline = crate::settings::Settings::load();
            baseline.import_tariff_config = Some(crate::settings::TariffConfig {
                slots: vec![
                    crate::settings::TariffSlot {
                        start: "00:00".to_string(),
                        end: "23:59".to_string(),
                        rate: 0.10,
                    },
                ],
            });
            baseline.save().unwrap();

            let state = Arc::new(AppState::new());

            // Many host-only writers (no tariff field) racing a single config
            // writer. With the buggy code each host-only writer collapses
            // `import_tariff_config` to its pre-lock default and writes it
            // back unconditionally, so if any of them runs its save AFTER the
            // config writer commits, the config is reverted. The many writers
            // widen the window so the interleaving is hit reliably.
            const N_HOST_WRITERS: usize = 16;
            let barrier = StdArc::new(std::sync::Barrier::new(N_HOST_WRITERS + 1));

            let mut host_handles = Vec::new();
            for i in 0..N_HOST_WRITERS {
                let state_i = state.clone();
                let barrier_i = barrier.clone();
                host_handles.push(tokio::spawn(async move {
                    barrier_i.wait();
                    update_settings(
                        State(state_i.clone()),
                        Json(serde_json::json!({
                            "host": format!("192.168.1.{i}"),
                            "serial": format!("TEST-{i}"),
                        })),
                    )
                    .await
                }));
            }

            // The single config writer. Its new value must survive all the
            // concurrent host-only saves.
            let state_c = state.clone();
            let barrier_c = barrier.clone();
            let h_c = tokio::spawn(async move {
                barrier_c.wait();
                update_settings(
                    State(state_c.clone()),
                    Json(serde_json::json!({
                        "import_tariff_config": {
                            "slots": [
                                { "start": "00:00", "end": "23:59", "rate": 0.55 },
                            ],
                        },
                    })),
                )
                .await
            });

            // main does NOT wait on the barrier (it awaits the handles below);
            // the barrier releases all spawned writers together.
            let mut statuses_ok = true;
            for h in host_handles {
                let (status, _) = h.await.unwrap();
                statuses_ok &= status == StatusCode::OK;
            }
            let (status_c, _) = h_c.await.unwrap();
            assert!(statuses_ok, "all host-only writers must succeed");
            assert_eq!(status_c, StatusCode::OK);

            let saved = crate::settings::Settings::load();
            let saved_rate = saved
                .import_tariff_config
                .as_ref()
                .and_then(|c| c.slots.first())
                .map(|s| s.rate)
                .unwrap_or(f64::NAN);
            assert!(
                (saved_rate - 0.55).abs() < 0.0001,
                "import_tariff_config must survive concurrent host-only saves (got rate {saved_rate}) — a host-only writer's stale pre-lock default reverted the config writer's update"
            );
        })
        .await;
    }

    /// `update_settings` must validate the ENTIRE request before persisting
    /// anything. The current `Settings::update` conversion (5ed53be) moved
    /// validation inside the mutate closure, which cannot abort the save —
    /// so a request with a valid `host` plus an invalid `octopus_gas_unit`
    /// returns 400 but has ALREADY written the host change to disk.
    /// A rejected request must be all-or-nothing: 400 and no mutation.
    /// Issue #283: forecast efficiencies are range-validated on save —
    /// an out-of-range value must 400 and leave disk untouched, an
    /// in-range value round-trips.
    #[tokio::test]
    async fn update_settings_validates_forecast_efficiencies() {
        with_isolated_config_dir_async(|| async {
            let state = test_state();

            let (status, body) = update_settings(
                State(state.clone()),
                Json(json!({ "forecast_charge_efficiency": 1.5 })),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(body["error"]
                .as_str()
                .unwrap()
                .contains("Charge efficiency"));
            // Disk untouched — still the default.
            assert!(
                (crate::settings::Settings::load().forecast_charge_efficiency - 0.9).abs() < 1e-9
            );

            let (status, _) = update_settings(
                State(state.clone()),
                Json(json!({ "forecast_discharge_efficiency": 0.4 })),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);

            let (status, _) = update_settings(
                State(state.clone()),
                Json(json!({
                    "forecast_charge_efficiency": 0.85,
                    "forecast_discharge_efficiency": 0.92
                })),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let s = crate::settings::Settings::load();
            assert!((s.forecast_charge_efficiency - 0.85).abs() < 1e-9);
            assert!((s.forecast_discharge_efficiency - 0.92).abs() < 1e-9);
        })
        .await;
    }

    /// Issue #283 planner v2: the settings endpoint accepts and validates
    /// `forecast_min_soc_pct` (0..=100, the SOC floor for the forecast
    /// window). Out-of-range values are rejected and leave disk untouched.
    #[tokio::test]
    async fn update_settings_validates_forecast_min_soc_pct() {
        with_isolated_config_dir_async(|| async {
            let state = test_state();

            let (status, body) = update_settings(
                State(state.clone()),
                Json(json!({ "forecast_min_soc_pct": 101.0 })),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(body["error"].as_str().unwrap().contains("Min SOC"));
            // Disk untouched — still the default (20).
            assert!((crate::settings::Settings::load().forecast_min_soc_pct - 20.0).abs() < 1e-9);

            let (status, _) = update_settings(
                State(state.clone()),
                Json(json!({ "forecast_min_soc_pct": -1.0 })),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);

            let (status, _) = update_settings(
                State(state.clone()),
                Json(json!({ "forecast_min_soc_pct": 30.0 })),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!((crate::settings::Settings::load().forecast_min_soc_pct - 30.0).abs() < 1e-9);
        })
        .await;
    }

    #[tokio::test]
    async fn update_settings_rejected_request_leaves_disk_untouched() {
        with_isolated_config_dir_async(|| async {
            // Seed a known baseline host so we can detect any partial write.
            let mut baseline = crate::settings::Settings::load();
            baseline.host = "192.168.1.99".to_string();
            baseline.save().unwrap();

            let state = Arc::new(AppState::new());
            {
                let mut s = state.settings.lock().await;
                s.host = baseline.host.clone();
            }

            // Valid host + invalid octopus_gas_unit in one request. The gas
            // unit validation fails, so the whole request must be rejected.
            let (status, _) = update_settings(
                State(state.clone()),
                Json(serde_json::json!({
                    "host": "10.0.0.5",
                    "octopus_gas_unit": "cubic_furlongs",
                })),
            )
            .await;

            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "invalid octopus_gas_unit must be rejected with 400"
            );
            let saved = crate::settings::Settings::load();
            assert_eq!(
                saved.host, "192.168.1.99",
                "a 400-rejected request must not partially persist the valid fields — disk untouched"
            );
            assert_ne!(
                saved.octopus_gas_unit, "cubic_furlongs",
                "the invalid value itself must not be persisted"
            );
            assert_eq!(
                saved.octopus_gas_unit, "unknown",
                "octopus_gas_unit stays at its default"
            );
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
    async fn octopus_settings_persist_but_secret_is_never_returned() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "octopus_enabled": true,
                "octopus_account_number": " A-1234ABCD ",
                "octopus_api_key": " sk_secret_value ",
                "octopus_gas_unit": "kwh",
                "octopus_economy7_start": "01:00",
                "octopus_economy7_end": "08:00",
            });
            let (status, _) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let saved = crate::settings::Settings::load();
            assert!(saved.octopus_enabled);
            assert_eq!(saved.octopus_account_number, "A-1234ABCD");
            assert_eq!(saved.octopus_api_key, "sk_secret_value");
            assert_eq!(saved.octopus_gas_unit, "kwh");
            assert_eq!(saved.octopus_economy7_start, "01:00");
            assert_eq!(saved.octopus_economy7_end, "08:00");

            let (_, response) = get_settings(State(state)).await;
            assert_eq!(response["data"]["octopus_enabled"], true);
            assert_eq!(response["data"]["octopus_api_key_configured"], true);
            assert_eq!(response["data"]["octopus_gas_unit"], "kwh");
            assert_eq!(response["data"]["octopus_economy7_start"], "01:00");
            assert_eq!(response["data"]["octopus_economy7_end"], "08:00");
            assert!(response["data"].get("octopus_api_key").is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn octopus_settings_reject_unknown_gas_unit() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let (status, body) = update_settings(
                State(state),
                Json(serde_json::json!({"octopus_gas_unit": "therms"})),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(body["error"].as_str().unwrap().contains("octopus_gas_unit"));
        })
        .await;
    }

    #[tokio::test]
    async fn octopus_settings_reject_invalid_or_empty_economy7_windows() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let (status, body) = update_settings(
                State(state.clone()),
                Json(serde_json::json!({"octopus_economy7_start": "24:00"})),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(body["error"].as_str().unwrap().contains("valid HH:MM"));

            let (status, body) = update_settings(
                State(state),
                Json(serde_json::json!({
                    "octopus_economy7_start": "07:30",
                    "octopus_economy7_end": "07:30"
                })),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(body["error"].as_str().unwrap().contains("must differ"));
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
    // Issue #131: Standing Charge (pence/day) round-trip through the
    // settings API. The SettingsPage posts a numeric pence/day value;
    // the backend must persist it on disk and return it via GET so the
    // frontend can rehydrate the input. Negative values must be clamped
    // to 0 to prevent a UI typo from silently inverting the cost graph.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn update_settings_persists_standing_charge() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "import_standing_charge_p_per_day": 54.86,
            });
            let (status, _) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let saved = crate::settings::Settings::load();
            assert!(
                (saved.import_standing_charge_p_per_day - 54.86).abs() < 1e-9,
                "Standing Charge must persist as the requested pence/day; got {}",
                saved.import_standing_charge_p_per_day
            );

            // Round-trip through GET so the Settings page can rehydrate.
            let (_, get_body) = get_settings(State(state)).await;
            assert_eq!(
                get_body["data"]["import_standing_charge_p_per_day"],
                serde_json::json!(54.86)
            );
        })
        .await;
    }

    #[tokio::test]
    async fn update_settings_standing_charge_zero_clears_existing_value() {
        with_isolated_config_dir_async(|| async {
            // Seed an existing Standing Charge on disk.
            let mut s = crate::settings::Settings::load();
            s.import_standing_charge_p_per_day = 54.86;
            s.save().expect("settings save");

            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "import_standing_charge_p_per_day": 0.0,
            });
            let (status, _) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let saved = crate::settings::Settings::load();
            assert_eq!(
                saved.import_standing_charge_p_per_day, 0.0,
                "explicit 0 must clear the Standing Charge"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn update_settings_standing_charge_negative_is_clamped_to_zero() {
        // Issue #131: a negative Standing Charge would invert the cost
        // series (subtracting from the cumulative total). The backend
        // clamps any negative input to 0 so a UI typo can't silently
        // produce a misleading bill total.
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let body = serde_json::json!({
                "import_standing_charge_p_per_day": -100.0,
            });
            let (status, _) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let saved = crate::settings::Settings::load();
            assert_eq!(
                saved.import_standing_charge_p_per_day, 0.0,
                "negative Standing Charge must be clamped to 0"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn update_settings_omitted_standing_charge_preserves_disk_value() {
        // The SettingsPage POST always sends every tariff field. We must
        // not regress: an admin tool that PATCHes only some fields should
        // leave the Standing Charge untouched.
        with_isolated_config_dir_async(|| async {
            let mut s = crate::settings::Settings::load();
            s.import_standing_charge_p_per_day = 54.86;
            s.save().expect("settings save");

            let state = Arc::new(AppState::new());
            // Empty body — no fields at all. The handler must skip the
            // Standing Charge branch entirely.
            let body = serde_json::json!({});
            let (status, _) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let saved = crate::settings::Settings::load();
            assert!(
                (saved.import_standing_charge_p_per_day - 54.86).abs() < 1e-9,
                "omitted Standing Charge must preserve the on-disk value"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn get_settings_returns_standing_charge_for_legacy_settings_json() {
        // Back-compat: a settings.json written before this field existed
        // has no `import_standing_charge_p_per_day` key. The Settings
        // struct defaults to 0 (via `#[serde(default)]`), and the GET
        // endpoint must surface that default so the frontend can render
        // a blank input rather than "NaN" or "undefined".
        with_isolated_config_dir_async(|| async {
            // Write a settings.json without the standing-charge field at
            // all, mimicking a pre-#131 install.
            let dir = crate::settings::Settings::settings_dir();
            std::fs::create_dir_all(&dir).expect("settings dir");
            let path = dir.join("settings.json");
            let body = serde_json::json!({
                "host": "10.0.0.1",
                "port": 8899,
                "serial": "LEGACY123",
                "poll_interval": 20,
                "http_port": 7337,
                "auto_connect": true,
                "import_tariff": 0.25,
                "export_tariff": 0.15,
            });
            std::fs::write(&path, serde_json::to_vec_pretty(&body).unwrap())
                .expect("write settings");

            let state = Arc::new(AppState::new());
            let (_, get_body) = get_settings(State(state)).await;
            assert_eq!(
                get_body["data"]["import_standing_charge_p_per_day"],
                serde_json::json!(0.0),
                "legacy settings.json must yield 0 for the new field"
            );
        })
        .await;
    }

    // -----------------------------------------------------------------------
    // Issue #131: GET /api/report — cost totals for the Power page
    // Consumption Report, scoped to the same range / offset as the graph.
    //
    // These tests verify the endpoint integrates against the same
    // `today_*_kwh` counters and tariff config the History cost graph uses
    // (so totals on the Power page match what the user sees on the History
    // page), AND adds the import-side Standing Charge once per local day
    // the window covers (matching the per-day step pattern).
    // -----------------------------------------------------------------------

    /// Build an `AppState` with a fresh HistoryDb wired in, plus a
    /// settings file with the supplied import tariff + Standing Charge.
    /// Returns the state and the temp DB path so tests can clean up.
    ///
    /// Must be called from inside a `#[tokio::test]` runtime. The test
    /// runtime blocks on the install via a oneshot channel rather than
    /// `block_on`, since `block_on` is illegal from inside the running
    /// runtime that `#[tokio::test]` provides.
    fn build_report_test_state(
        import_tariff_rate: f64,
        export_tariff_rate: f64,
        standing_charge_p_per_day: f64,
    ) -> std::sync::Arc<AppState> {
        let mut s = crate::settings::Settings::load();
        s.import_tariff = import_tariff_rate;
        s.export_tariff = export_tariff_rate;
        s.import_tariff_config = Some(crate::settings::TariffConfig::flat(import_tariff_rate));
        s.export_tariff_config = Some(crate::settings::TariffConfig::flat(export_tariff_rate));
        s.import_standing_charge_p_per_day = standing_charge_p_per_day;
        s.save().expect("save settings");

        let tmp_path = std::env::temp_dir().join(format!(
            "givenergy-test-history-{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = crate::history::HistoryDb::open(&tmp_path).expect("open history db");
        let db_arc = std::sync::Arc::new(db);

        let state = std::sync::Arc::new(AppState::new());
        // AppState::history uses `tokio::sync::Mutex`, so installing the
        // db requires a running tokio runtime. We're already inside a
        // `#[tokio::test]` runtime, but `block_on` is illegal from the
        // worker thread. Solution: spawn a fresh thread with its own
        // single-threaded runtime and have it install the db. The fresh
        // runtime has no parent, so `block_on` works there. We block
        // synchronously on a `std::sync::mpsc` channel.
        let state_for_install = state.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("install runtime");
            rt.block_on(async move {
                let mut guard = state_for_install.history.lock().await;
                *guard = Some(db_arc);
            });
            let _ = tx.send(());
        });
        rx.recv().expect("install thread");

        let _ = tmp_path; // caller cleans up the temp db file
        state
    }

    /// Convenience: insert a daily-resetting counter ramp matching what
    /// `insert_export_day` in the history test module does. Mirrors the
    /// 30-min granularity so the cost integration has data to walk.
    fn seed_today_import_kwh(db: &crate::history::HistoryDb, day_offset: i64, daily_kwh: f32) {
        let date = chrono::Local::now().date_naive() + chrono::Duration::days(day_offset);
        let midnight = chrono::Local
            .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .unwrap()
            .timestamp();
        for step in 0..48i64 {
            let ts = midnight + step * 1800;
            // Linear ramp from 0 → daily_kwh across the day. Step 0 is
            // just after midnight (counter has been reset) and step 47
            // is just before the next midnight (counter holds the day's
            // total).
            let frac: f32 = step as f32 / 47.0;
            let snap = crate::inverter::model::InverterSnapshot {
                timestamp: ts,
                today_import_kwh: daily_kwh * frac,
                ..Default::default()
            };
            db.insert_reading(&snap);
        }
    }

    fn seed_grid_power_readings(db: &crate::history::HistoryDb, start_ts: i64, grid_power_w: i32) {
        for step in 0..=2i64 {
            db.insert_reading(&crate::inverter::model::InverterSnapshot {
                timestamp: start_ts + step * 3600,
                grid_power: grid_power_w,
                // Reproduce firmware that reports live grid power but leaves
                // the daily import/export counters at zero.
                today_import_kwh: 0.0,
                today_export_kwh: 0.0,
                ..Default::default()
            });
        }
    }

    #[tokio::test]
    async fn get_history_one_hour_keeps_sparse_startup_samples_unique() {
        // Issue #216 was visible immediately after startup, when the selected
        // hour contained only a few recent buckets. The API should return that
        // sparse data once, without padding the empty leading window or
        // duplicating timestamps; the frontend owns the fixed one-hour axis.
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.0, 0.0, 0.0);
            let start_ts = 1_700_000_000i64;
            let end_ts = start_ts + 3600;
            let db_guard = state.history.lock().await;
            let db = db_guard.as_ref().unwrap();

            db.insert_reading(&crate::inverter::model::InverterSnapshot {
                timestamp: start_ts - 1,
                soc: 10,
                ..Default::default()
            });
            for (index, seconds_before_end) in [40i64, 25, 10].into_iter().enumerate() {
                db.insert_reading(&crate::inverter::model::InverterSnapshot {
                    timestamp: end_ts - seconds_before_end,
                    soc: 50 + index as u8,
                    ..Default::default()
                });
            }
            drop(db_guard);

            let (status, Json(json)) = get_history(
                State(state),
                Query(HistoryQuery {
                    range: Some("1h".to_string()),
                    fields: Some("soc".to_string()),
                    offset: Some(0),
                    rolling: Some(true),
                    start_ms: Some(start_ts * 1000),
                    end_ms: Some(end_ts * 1000),
                }),
            )
            .await;

            assert_eq!(status, StatusCode::OK);
            let points = json["data"]["soc"].as_array().unwrap();
            assert!(
                !points.is_empty(),
                "recent startup samples must be returned"
            );
            assert!(
                points.len() <= 2,
                "30-second buckets must stay sparse: {points:?}"
            );

            let timestamps: Vec<i64> = points
                .iter()
                .map(|point| point["t"].as_i64().unwrap())
                .collect();
            let unique: std::collections::HashSet<i64> = timestamps.iter().copied().collect();
            assert_eq!(
                unique.len(),
                timestamps.len(),
                "bucket timestamps must be unique"
            );
            assert!(timestamps.iter().all(|t| *t >= start_ts * 1000));
        })
        .await;
    }

    #[tokio::test]
    async fn get_history_summary_returns_energy_for_the_explicit_window() {
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.25, 0.15, 0.0);
            let start = 1_700_300_000i64;
            {
                let guard = state.history.lock().await;
                let db = guard.as_ref().unwrap();
                for (offset, solar, imported) in [
                    (-60, 4.75, 2.75),
                    (0, 5.0, 3.0),
                    (60, 5.25, 3.25),
                    (120, 5.5, 3.5),
                ] {
                    db.insert_reading(&crate::inverter::model::InverterSnapshot {
                        timestamp: start + offset,
                        today_solar_kwh: solar,
                        today_import_kwh: imported,
                        home_energy_today_kwh: solar / 2.0,
                        ..Default::default()
                    });
                }
            }

            let (status, Json(json)) = get_history_summary(
                State(state),
                Query(HistoryQuery {
                    range: Some("1h".to_string()),
                    fields: None,
                    offset: Some(0),
                    rolling: Some(true),
                    start_ms: Some(start * 1000),
                    end_ms: Some((start + 121) * 1000),
                }),
            )
            .await;

            assert_eq!(status, StatusCode::OK);
            assert!((json["data"]["solar_generated_kwh"].as_f64().unwrap() - 0.5).abs() < 1e-6);
            assert!((json["data"]["grid_imported_kwh"].as_f64().unwrap() - 0.5).abs() < 1e-6);
            assert!((json["data"]["home_consumed_kwh"].as_f64().unwrap() - 0.25).abs() < 1e-6);
            assert!(json["data"]["import_cost_gbp"].is_number());
            assert!(json["data"]["net_cost_gbp"].is_number());
        })
        .await;
    }

    #[tokio::test]
    async fn get_history_summary_rejects_an_inverted_explicit_window() {
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.0, 0.0, 0.0);
            let (status, Json(json)) = get_history_summary(
                State(state),
                Query(HistoryQuery {
                    range: Some("today".to_string()),
                    fields: None,
                    offset: Some(0),
                    rolling: Some(false),
                    start_ms: Some(2_000),
                    end_ms: Some(1_000),
                }),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(
                json["error"],
                "Invalid history window: start_ms must be before end_ms"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn get_report_returns_zero_costs_for_window_with_no_readings() {
        // Empty history → import_cost == Standing Charge (the seeded "no
        // data" bucket), export_income == 0. The endpoint must not 500.
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.25, 0.15, 54.86);
            let (status, json) = get_report(
                State(state.clone()),
                Query(HistoryQuery {
                    range: Some("24h".to_string()),
                    fields: None,
                    offset: Some(0),
                    rolling: Some(false),
                    start_ms: None,
                    end_ms: None,
                }),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            // Non-rolling 24h aligns to [local midnight, local midnight) —
            // exactly one calendar day (issue #269), so Standing Charge is
            // one day's 54.86p. No import kWh data, so import_cost_gbp
            // equals standing_charge_gbp exactly.
            let standing = json["standing_charge_gbp"].as_f64().unwrap();
            let import_cost = json["import_cost_gbp"].as_f64().unwrap();
            let export_income = json["export_income_gbp"].as_f64().unwrap();
            let net_cost = json["net_cost_gbp"].as_f64().unwrap();
            let days = json["days_in_range"].as_u64().unwrap();
            assert!(
                (standing - 0.5486).abs() < 1e-3,
                "24h aligned window must credit 1 day of Standing Charge (£0.5486); got {standing}"
            );
            assert!(
                (import_cost - 0.5486).abs() < 1e-3,
                "import cost must equal Standing Charge when there are no kWh readings; got {import_cost}"
            );
            assert!(
                export_income.abs() < 1e-6,
                "export income must be zero with no readings; got {export_income}"
            );
            assert!(
                (net_cost - 0.5486).abs() < 1e-3,
                "net cost must equal Standing Charge (no kWh to offset); got {net_cost}"
            );
            assert_eq!(days, 1, "24h aligned window must report 1 distinct local day");

            // Clean up the temp db.
        })
        .await;
    }

    #[tokio::test]
    async fn get_report_combines_kwh_cost_and_standing_charge() {
        // 1 day of 5 kWh import at £0.25 plus 54.86p/day Standing Charge.
        // import_cost = 5 × 0.25 + 0.5486 = 1.7986.
        // days_in_range = 2 (24h crosses a midnight).
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.25, 0.15, 54.86);
            seed_today_import_kwh(
                state
                    .history
                    .lock()
                    .await
                    .as_ref()
                    .unwrap()
                    .as_ref(),
                0,
                5.0,
            );

            let (status, json) = get_report(
                State(state.clone()),
                Query(HistoryQuery {
                    range: Some("24h".to_string()),
                    fields: None,
                    offset: Some(0),
                    rolling: Some(false),
                    start_ms: None,
                    end_ms: None,
                }),
            )
            .await;
            assert_eq!(status, StatusCode::OK);

            let import_cost = json["import_cost_gbp"].as_f64().unwrap();
            // The exact value depends on the cost-integration walk
            // including the daily counter reset, but it must include
            // BOTH the kWh component AND the Standing Charge. We allow
            // a generous tolerance because the ramp shape varies.
            assert!(
                import_cost > 1.25 + 0.5486 - 0.1,
                "import cost must include both kWh cost (~£1.25) and Standing Charge (£0.5486); got {import_cost}"
            );
            assert!(
                import_cost < 1.25 + 1.10 + 0.1,
                "import cost must not be wildly inflated; got {import_cost}"
            );

        })
        .await;
    }

    #[tokio::test]
    async fn get_report_no_fallback_trusts_counter_when_import_counter_is_zero() {
        // Issue #269 (Pete's £3.86 noise case, comment 5407621722):
        // the report used to fall back to a grid-power trapezoid when
        // the daily counter was empty, but on a working inverter that
        // means CT-clamp noise gets priced as import. The fix trusts
        // the counter — an empty counter produces £0.00 import cost in
        // the report, matching the chart. This loses the (rare)
        // "broken counter firmware" safety net; users with that
        // firmware would now see a £0.00 report while their kWh tiles
        // show real energy, and the chart stays at standing-only.
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.25, 0.15, 0.0);
            let start = chrono::Local
                .from_local_datetime(
                    &chrono::Local::now()
                        .date_naive()
                        .and_hms_opt(10, 0, 0)
                        .unwrap(),
                )
                .earliest()
                .unwrap()
                .timestamp();
            seed_grid_power_readings(
                state.history.lock().await.as_ref().unwrap().as_ref(),
                start,
                1000,
            );

            let (status, json) = get_report(
                State(state.clone()),
                Query(HistoryQuery {
                    range: Some("24h".to_string()),
                    fields: None,
                    offset: Some(0),
                    rolling: Some(false),
                    start_ms: Some(start * 1000),
                    end_ms: Some((start + 2 * 3600 + 1) * 1000),
                }),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let import_cost = json["import_cost_gbp"].as_f64().unwrap();
            assert!(
                import_cost.abs() < 1e-9,
                "empty import counter must produce £0.00 cost (no trapezoid fallback); got {import_cost} ({json:?})"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn get_report_no_fallback_trusts_counter_when_export_counter_is_zero() {
        // Symmetric to the import side: an empty export counter
        // produces £0.00 export income, even though grid_power shows
        // real export. Matches the chart's contract.
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.25, 0.15, 0.0);
            let start = chrono::Local
                .from_local_datetime(
                    &chrono::Local::now()
                        .date_naive()
                        .and_hms_opt(12, 0, 0)
                        .unwrap(),
                )
                .earliest()
                .unwrap()
                .timestamp();
            seed_grid_power_readings(
                state.history.lock().await.as_ref().unwrap().as_ref(),
                start,
                -1000,
            );

            let (status, json) = get_report(
                State(state.clone()),
                Query(HistoryQuery {
                    range: Some("24h".to_string()),
                    fields: None,
                    offset: Some(0),
                    rolling: Some(false),
                    start_ms: Some(start * 1000),
                    end_ms: Some((start + 2 * 3600 + 1) * 1000),
                }),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let export_income = json["export_income_gbp"].as_f64().unwrap();
            assert!(
                export_income.abs() < 1e-9,
                "empty export counter must produce £0.00 income (no trapezoid fallback); got {export_income} ({json:?})"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn get_report_standing_charge_scales_with_days_in_range() {
        // 7-day window: Standing Charge = 7 × 54.86p = £3.8402 (plus 8
        // days touched at the calendar boundaries, depending on exact
        // local time the test runs). The key invariant is that more
        // days → higher Standing Charge.
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.25, 0.15, 54.86);
            let (status_24h, json_24h) = get_report(
                State(state.clone()),
                Query(HistoryQuery {
                    range: Some("24h".to_string()),
                    fields: None,
                    offset: Some(0),
                    rolling: Some(false),
                    start_ms: None,
                    end_ms: None,
                }),
            )
            .await;
            let (status_7d, json_7d) = get_report(
                State(state.clone()),
                Query(HistoryQuery {
                    range: Some("7d".to_string()),
                    fields: None,
                    offset: Some(0),
                    rolling: Some(false),
                    start_ms: None,
                    end_ms: None,
                }),
            )
            .await;
            assert_eq!(status_24h, StatusCode::OK);
            assert_eq!(status_7d, StatusCode::OK);

            let sc_24h = json_24h["standing_charge_gbp"].as_f64().unwrap();
            let sc_7d = json_7d["standing_charge_gbp"].as_f64().unwrap();
            assert!(
                sc_7d > sc_24h,
                "7-day window must have a larger Standing Charge than 24h (24h={sc_24h}, 7d={sc_7d})"
            );
            // 7-day window touches 7-8 distinct local days; 24h touches
            // 2. The 7-day Standing Charge must be roughly 3.5-4× the
            // 24h one (allowing for the boundary day count difference).
            assert!(
                sc_7d > sc_24h * 3.0,
                "7-day Standing Charge must be substantially larger than 24h (24h={sc_24h}, 7d={sc_7d})"
            );

        })
        .await;
    }

    #[tokio::test]
    async fn get_report_zero_standing_charge_omits_per_day_addition() {
        // When the user has not configured a Standing Charge, the cost
        // totals reflect the per-kWh component only. The standing_charge
        // field must be 0, not silently summed from defaults.
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.25, 0.15, 0.0);
            let (status, json) = get_report(
                State(state.clone()),
                Query(HistoryQuery {
                    range: Some("24h".to_string()),
                    fields: None,
                    offset: Some(0),
                    rolling: Some(false),
                    start_ms: None,
                    end_ms: None,
                }),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                json["standing_charge_gbp"].as_f64().unwrap(),
                0.0,
                "no configured Standing Charge → £0 Standing Charge"
            );
            assert_eq!(json["standing_charge_p_per_day"].as_f64().unwrap(), 0.0);
        })
        .await;
    }

    #[tokio::test]
    async fn get_report_rejects_invalid_range() {
        // Bad range → 400, not 500.
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.25, 0.15, 54.86);
            let (status, _json) = get_report(
                State(state.clone()),
                Query(HistoryQuery {
                    range: Some("not-a-range".to_string()),
                    fields: None,
                    offset: Some(0),
                    rolling: Some(false),
                    start_ms: None,
                    end_ms: None,
                }),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        })
        .await;
    }

    #[tokio::test]
    async fn window_resolution_is_shared_across_history_chart_summary_and_report() {
        // Review finding #9: the range->bucket table and window-resolution
        // logic existed in three near-verbatim copies (get_history,
        // get_history_summary via resolve_history_summary_window, and
        // get_report). This test pins the shared contract: all three
        // endpoints resolve the SAME (range_secs, bucket_secs, explicit
        // window) for identical query params — including rejecting the
        // same invalid input with the same behaviour. If any endpoint
        // drifts (as the report copy already had, with a different
        // inverted-window error message), this fails.
        for range in [
            "1h", "6h", "12h", "24h", "today", "7d", "30d", "6m", "1y", "month",
        ] {
            let _ = range; // keep range bound for clarity of the table below
            let params = HistoryQuery {
                range: Some(range.to_string()),
                offset: Some(0),
                rolling: Some(false),
                start_ms: None,
                end_ms: None,
                fields: None,
            };
            let (w, bucket) = resolve_history_summary_window(&params).expect(range);
            // The shared table: range_secs and bucket_secs per range key.
            let (want_range, want_bucket) = match range {
                "1h" => (3600, 30),
                "6h" => (3600 * 6, 60),
                "12h" => (3600 * 12, 120),
                "24h" | "today" => (86400, 300),
                "7d" => (86400 * 7, 1800),
                "30d" => (86400 * 30, 7200),
                "6m" => (86400 * 180, 43200),
                "1y" => (86400 * 365, 86400),
                "month" => (0, 3600),
                _ => unreachable!(),
            };
            assert_eq!(w.range_secs, want_range, "range {range}");
            assert_eq!(bucket, want_bucket, "range {range}");
        }
        // Inverted explicit window rejected everywhere with the same shape.
        let bad = HistoryQuery {
            range: Some("24h".to_string()),
            offset: Some(0),
            rolling: Some(false),
            start_ms: Some(2000),
            end_ms: Some(1000),
            fields: None,
        };
        assert!(resolve_history_summary_window(&bad).is_err());
    }

    // -----------------------------------------------------------------------
    // Issue #269 exhaustive: period-totals box vs chart series equivalence
    // through the real endpoints, for every scenario combination. The box
    // comes from get_history_summary; the chart's final value comes from
    // get_history's `_import_cost` series. Both trust the daily counter
    // alone and must agree exactly. The earlier grid-power trapezoid
    // fallback fired when the counter was empty and inflated the box
    // above the chart by trapezoid-noise amounts (Pete's £3.86 box vs
    // £0.5735 chart, issue #269 comment 5407621722); it is removed.
    // -----------------------------------------------------------------------

    /// Local-midnight unix seconds (local time) for a day offset.
    fn api_test_midnight(day_offset: i64) -> i64 {
        let date = chrono::Local::now().date_naive() + chrono::Duration::days(day_offset);
        chrono::Local
            .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .unwrap()
            .timestamp()
    }

    /// Insert a full local day of both counters ramping 06:00→18:00 and a
    /// flat `grid_power` (import-positive) across the same span, so both
    /// the counter path and the fallback path have data.
    async fn api_seed_day(
        state: &std::sync::Arc<AppState>,
        day_offset: i64,
        import_kwh: f64,
        export_kwh: f64,
        grid_power_w: i32,
    ) {
        let guard = state.history.lock().await;
        let db = guard.as_ref().unwrap();
        let midnight = api_test_midnight(day_offset);
        for step in 0..48i64 {
            let ts = midnight + step * 1800;
            let min_of_day = step * 30;
            let frac = if min_of_day <= 6 * 60 {
                0.0
            } else if min_of_day >= 18 * 60 {
                1.0
            } else {
                (min_of_day - 6 * 60) as f64 / (18 * 60 - 6 * 60) as f64
            };
            db.insert_reading(&crate::inverter::model::InverterSnapshot {
                timestamp: ts,
                today_import_kwh: (import_kwh * frac) as f32,
                today_export_kwh: (export_kwh * frac) as f32,
                grid_power: if grid_power_w == 0 {
                    0
                } else {
                    ((grid_power_w as f64 * frac) as i32).max(0)
                },
                ..Default::default()
            });
        }
    }

    /// Chart `_import_cost` final value and box `import_cost_gbp` for a
    /// window, via the real endpoints (start_ms/end_ms form so the window
    /// is exactly [midnight N, midnight N+1)).
    async fn box_and_chart_totals(
        state: &std::sync::Arc<AppState>,
        start: i64,
        end: i64,
    ) -> ((f64, f64), f64) {
        let (status, Json(json)) = get_history_summary(
            State(state.clone()),
            Query(HistoryQuery {
                range: Some("today".to_string()),
                fields: None,
                offset: Some(0),
                rolling: Some(false),
                start_ms: Some(start * 1000),
                end_ms: Some(end * 1000),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "summary endpoint failed");
        let import_cost = json["data"]["import_cost_gbp"].as_f64().unwrap();
        let export_income = json["data"]["export_income_gbp"].as_f64().unwrap();

        let (status, Json(chart)) = get_history(
            State(state.clone()),
            Query(HistoryQuery {
                range: Some("today".to_string()),
                fields: Some("_import_cost".to_string()),
                offset: Some(0),
                rolling: Some(false),
                start_ms: Some(start * 1000),
                end_ms: Some(end * 1000),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "history endpoint failed");
        let points = chart["data"]["_import_cost"].as_array().unwrap();
        let chart_total = points.last().and_then(|p| p["v"].as_f64()).unwrap_or(0.0);
        ((import_cost, export_income), chart_total)
    }

    #[tokio::test]
    async fn api269_box_matches_chart_import_only() {
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.30, 0.15, 57.35);
            api_seed_day(&state, 0, 10.0, 0.0, 2000).await;
            let (box_vals, chart) =
                box_and_chart_totals(&state, api_test_midnight(0), api_test_midnight(1)).await;
            // Chart: 10 kWh × £0.30 + 57.35p standing = £3.5735.
            assert!((chart - 3.5735).abs() < 0.02, "chart {chart} != ~£3.5735");
            assert!(
                (box_vals.0 - chart).abs() < 0.02,
                "box {box_vals:?} != chart {chart}: import-only day must agree"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn api269_box_matches_chart_export_only() {
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.30, 0.15, 26.02);
            api_seed_day(&state, 0, 0.0, 20.0, -1500).await;
            let (box_vals, chart) =
                box_and_chart_totals(&state, api_test_midnight(0), api_test_midnight(1)).await;
            // The `_import_cost` chart on an export-only day is standing
            // only (£0.2602) — the export income shows on the box side.
            assert!(
                (chart - 0.2602).abs() < 0.02,
                "import-cost chart {chart} != standing-only £0.2602"
            );
            assert!(
                (box_vals.0 - chart).abs() < 0.02,
                "box {box_vals:?} != chart {chart}: export-only day must agree"
            );
            // Export income matches on both sides too.
            assert!(
                (box_vals.1 - 3.0).abs() < 0.02,
                "export income {} != ~£3.00",
                box_vals.1
            );
        })
        .await;
    }

    #[tokio::test]
    async fn api269_box_matches_chart_both_counters() {
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.30, 0.15, 57.35);
            api_seed_day(&state, 0, 10.0, 20.0, 0).await;
            let (box_vals, chart) =
                box_and_chart_totals(&state, api_test_midnight(0), api_test_midnight(1)).await;
            assert!(
                (box_vals.0 - chart).abs() < 0.02,
                "box {box_vals:?} != chart {chart}: mixed day must agree"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn api269_box_matches_chart_zero_counters_zero_grid() {
        // Genuinely nothing happening: standing charge only, both sides.
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.30, 0.15, 57.35);
            api_seed_day(&state, 0, 0.0, 0.0, 0).await;
            let (box_vals, chart) =
                box_and_chart_totals(&state, api_test_midnight(0), api_test_midnight(1)).await;
            assert!(
                (chart - 0.5735).abs() < 0.02,
                "chart {chart} != standing-only £0.5735"
            );
            assert!(
                (box_vals.0 - chart).abs() < 0.02,
                "box {box_vals:?} != chart {chart}: quiet day must agree"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn api269_counter_populated_fallback_must_not_fire() {
        // Pete's exact case shape: counter has real data, so the box must
        // use the counter path even though grid_power suggests more import
        // than the counter admits. The fallback is for EMPTY counters only.
        // If it fired here, the box would exceed the chart by the
        // trapezoid-minus-counter difference — the £2.86-vs-£1.14 bug.
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.30, 0.15, 57.35);
            // Counter says 4 kWh; grid_power trapezoid would say ~5.8 kWh.
            api_seed_day(&state, 0, 4.0, 0.0, 2400).await;
            let (box_vals, chart) =
                box_and_chart_totals(&state, api_test_midnight(0), api_test_midnight(1)).await;
            let expected_chart = 4.0 * 0.30 + 0.5735;
            assert!(
                (chart - expected_chart).abs() < 0.02,
                "chart {chart} != counter-based £{expected_chart}"
            );
            assert!(
                (box_vals.0 - chart).abs() < 0.02,
                "box {box_vals:?} != chart {chart}: fallback must not fire when counter is populated"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn api269_empty_counter_trusts_counter_no_fallback() {
        // Empty daily counter + sustained grid_power used to trigger the
        // trapezoid fallback and inflate the box above the chart by
        // whatever the trapezoid integrated. The fix trusts the counter;
        // the chart already did. They must match exactly on a quiet day.
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.30, 0.15, 57.35);
            api_seed_day(&state, 0, 0.0, 0.0, 2000).await;
            let (box_vals, chart) =
                box_and_chart_totals(&state, api_test_midnight(0), api_test_midnight(1)).await;
            // Chart: no counter deltas → standing only.
            assert!(
                (chart - 0.5735).abs() < 0.02,
                "chart {chart} != standing-only £0.5735"
            );
            // Box: counter path — must match chart exactly. (Old code
            // added the trapezoid's import_kwh here, taking box above
            // chart by £7+ on this scenario.)
            assert!(
                (box_vals.0 - chart).abs() < 0.02,
                "box {box_vals:?} != chart {chart}: empty-counter trapezoid fallback must not fire"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn api269_pete_noise_no_fallback_on_empty_counter() {
        // Pete's exact scenario (issue #269 comment 5407621722, screenshot
        // 24 Aug): the inverter genuinely imported nothing today (counter
        // stays 0) while exporting solar (counter ramps) — grid_power
        // shows the export AND small positive CT-clamp noise throughout
        // the day. The trapezoid would integrate the noise into a
        // non-trivial "import" cost, the box would show that spurious
        // £3-odd figure, and the chart would still show standing-only.
        // Trusting the counter makes box == chart.
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.30, 0.15, 57.35);
            let guard = state.history.lock().await;
            let db = guard.as_ref().unwrap();
            let midnight = api_test_midnight(0);
            for step in 0..48i64 {
                let ts = midnight + step * 1800;
                let min_of_day = step * 30;
                let export_frac = if min_of_day < 15 * 60 {
                    0.0
                } else if min_of_day >= 18 * 60 {
                    1.0
                } else {
                    (min_of_day - 15 * 60) as f64 / (18 * 60 - 15 * 60) as f64
                };
                // Export counter ramps 15:00→18:00 to 14.8 kWh
                // (£2.22 at £0.15/kWh — matches the screenshot's export
                // income box). Import counter stays at 0 throughout.
                // grid_power is +5 W sensor noise all day, dropping to
                // −1500 W during the export window.
                let export_kwh = 14.8 * export_frac;
                let grid_power_w: i32 = if export_frac > 0.0 {
                    -1500 + 5
                } else {
                    5
                };
                db.insert_reading(&crate::inverter::model::InverterSnapshot {
                    timestamp: ts,
                    today_import_kwh: 0.0,
                    today_export_kwh: export_kwh as f32,
                    grid_power: grid_power_w,
                    ..Default::default()
                });
            }
            drop(guard);

            let (box_vals, chart) =
                box_and_chart_totals(&state, api_test_midnight(0), api_test_midnight(1)).await;
            // Chart: no import counter deltas → standing only.
            assert!(
                (chart - 0.5735).abs() < 0.02,
                "chart {chart} != standing-only £0.5735"
            );
            // Box must equal chart exactly — trapezoid noise must not
            // inflate the import cost.
            assert!(
                (box_vals.0 - chart).abs() < 0.02,
                "box import {box_vals:?} != chart {chart}: empty-counter trapezoid fallback must not fire on noise"
            );
            // The export side should also match the chart's £2.22 export
            // income (sanity check that the box-vs-chart contract holds
            // for the populated direction too).
            let chart_export = chart_export_total(&state, api_test_midnight(0), api_test_midnight(1)).await;
            assert!(
                (box_vals.1 - chart_export).abs() < 0.02,
                "box export {box_vals:?} != chart {chart_export}: populated-counter direction must agree"
            );
        })
        .await;
    }

    /// Last `_export_income` point in the today window — used by the
    /// Pete-noise test to assert the populated direction agrees too.
    async fn chart_export_total(state: &std::sync::Arc<AppState>, start: i64, end: i64) -> f64 {
        let (status, Json(chart)) = get_history(
            State(state.clone()),
            Query(HistoryQuery {
                range: Some("today".to_string()),
                fields: Some("_export_income".to_string()),
                offset: Some(0),
                rolling: Some(false),
                start_ms: Some(start * 1000),
                end_ms: Some(end * 1000),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "history endpoint failed");
        let points = chart["data"]["_export_income"].as_array().unwrap();
        points.last().and_then(|p| p["v"].as_f64()).unwrap_or(0.0)
    }

    #[tokio::test]
    async fn api269_export_larger_than_import_net_credit() {
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.30, 0.15, 57.35);
            api_seed_day(&state, 0, 5.0, 25.0, 0).await;
            let (box_vals, chart) =
                box_and_chart_totals(&state, api_test_midnight(0), api_test_midnight(1)).await;
            // Import: 5 × 0.30 + 0.5735 = £2.0735; income 25 × 0.15 = £3.75.
            assert!((chart - 2.0735).abs() < 0.02, "chart {chart} != ~£2.0735");
            assert!(
                (box_vals.0 - chart).abs() < 0.02,
                "box {box_vals:?} != chart {chart}"
            );
            assert!(
                (box_vals.1 - 3.75).abs() < 0.05,
                "export income {} != ~£3.75",
                box_vals.1
            );
        })
        .await;
    }

    #[tokio::test]
    async fn api269_report_endpoint_agrees_across_ranges() {
        // /api/report must agree with the summary box for every range it
        // shares. This pins the "days_in_range" fix at the third consumer.
        with_isolated_config_dir_async(|| async {
            let state = build_report_test_state(0.30, 0.15, 57.35);
            api_seed_day(&state, 0, 10.0, 0.0, 1000).await;
            api_seed_day(&state, -1, 8.0, 0.0, 1000).await;
            for (range, expected_days) in [("today", 1u64), ("24h", 1), ("7d", 7)] {
                let (status, json) = get_report(
                    State(state.clone()),
                    Query(HistoryQuery {
                        range: Some(range.to_string()),
                        fields: None,
                        offset: Some(0),
                        rolling: Some(false),
                        start_ms: None,
                        end_ms: None,
                    }),
                )
                .await;
                assert_eq!(status, StatusCode::OK);
                let days = json["days_in_range"].as_u64().unwrap();
                let standing = json["standing_charge_gbp"].as_f64().unwrap();
                assert_eq!(
                    days, expected_days,
                    "{range}: got {days} days, expected {expected_days}"
                );
                assert!(
                    (standing - 0.5735 * expected_days as f64).abs() < 1e-3,
                    "{range}: standing {standing} != {expected_days} × £0.5735"
                );
            }
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

        assert_eq!(
            fields.len(),
            2,
            "only the two fields in the body should appear, got: {joined}"
        );
        assert!(
            joined.contains("api_key="),
            "expected api_key entry, got: {joined}"
        );
        assert!(
            joined.contains("api_port=7338"),
            "expected api_port entry, got: {joined}"
        );
        // The bug was: these connection fields appeared as empty/0 in the
        // log even when the body didn't carry them. They must NOT appear.
        // Use precise matches to avoid false positives from `api_port=`
        // (which contains the substring `port=`).
        assert!(
            !joined.contains("host="),
            "host must not appear when absent from body, got: {joined}"
        );
        assert!(
            !joined
                .split(',')
                .any(|s| s.trim_start().starts_with("port=")),
            "port must not appear when absent from body, got: {joined}"
        );
        assert!(
            !joined.contains("serial="),
            "serial must not appear when absent from body, got: {joined}"
        );
        assert!(
            !joined.contains("interval="),
            "interval must not appear when absent from body, got: {joined}"
        );
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
        assert!(
            joined.contains("api_key=set"),
            "expected redacted set marker, got: {joined}"
        );
        assert!(
            joined.contains("16 chars"),
            "expected length hint so the user can verify the key, got: {joined}"
        );
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

        assert!(
            joined.contains("api_key=cleared"),
            "empty api_key should be reported as cleared, got: {joined}"
        );
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
                    TariffSlot {
                        start: "00:00".into(),
                        end: "07:00".into(),
                        rate: 0.10,
                    },
                    TariffSlot {
                        start: "07:00".into(),
                        end: "23:59".into(),
                        rate: 0.30,
                    },
                ],
            }),
            export_tariff_config: Some(TariffConfig {
                slots: (0..24)
                    .map(|i| TariffSlot {
                        start: format!("{i:02}:00"),
                        end: format!("{:02}:00", (i + 1) % 24),
                        rate: 0.20,
                    })
                    .collect(),
            }),
            hidden_panels: vec!["x".into(), "y".into(), "z".into()],
            ..Default::default()
        };

        let fields = settings_log_fields(&body, &persist);
        let joined = fields.join(", ");

        assert!(
            joined.contains("import_tariff_config=2 slots"),
            "got: {joined}"
        );
        assert!(
            joined.contains("export_tariff_config=24 slots"),
            "got: {joined}"
        );
        assert!(joined.contains("hidden_panels=3 entries"), "got: {joined}");
        // Don't dump the full JSON.
        assert!(
            !joined.contains("rate"),
            "tariff slot details must not be logged, got: {joined}"
        );
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

            let (status, json) = update_settings(State(state.clone()), Json(body)).await;
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
                !msg.split(',')
                    .any(|s| s.trim_start().starts_with("serial=")),
                "serial must not appear when absent, got: {msg}"
            );
            assert!(
                !msg.split(',')
                    .any(|s| s.trim_start().starts_with("interval=")),
                "interval must not appear when absent, got: {msg}"
            );

            // (c) The values must be on disk for the next launch.
            let saved = crate::settings::Settings::load();
            assert_eq!(
                saved.api_key, "my-secret-token-12345",
                "api_key must be persisted"
            );
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

            let (status, json) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let msg = response_message(&json);

            assert!(
                msg.contains("api_key=cleared"),
                "empty key must be reported as cleared, got: {msg}"
            );
            // Persisted state is empty, port preserved.
            let saved = crate::settings::Settings::load();
            assert_eq!(saved.api_key, "", "api_key must be cleared on disk");
            assert_eq!(
                saved.api_port, 7338,
                "api_port must be preserved when only key was sent"
            );
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

            let (status, json) = update_settings(State(state.clone()), Json(body)).await;
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
                !msg.split(',')
                    .any(|s| s.trim_start().starts_with("interval=")),
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

            let (status, json) = update_settings(State(state.clone()), Json(body)).await;
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
            assert!(
                !msg.contains("interval="),
                "interval must not appear when absent from body, got: {msg}"
            );
            // (c) And the disk value must still be 30s — the connection
            // save did not clobber it.
            let saved = crate::settings::Settings::load();
            assert_eq!(
                saved.poll_interval, 30,
                "interval on disk must be unchanged"
            );
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

            let (status, json) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let msg = response_message(&json);

            assert!(
                msg.contains("interval=30s"),
                "new interval must appear, got: {msg}"
            );
            // The four connection fields are not in the body, so none
            // should appear — previously all four would show as empty/0.
            assert!(!msg.contains("host="), "got: {msg}");
            assert!(!msg.contains("port="), "got: {msg}");
            assert!(!msg.contains("serial="), "got: {msg}");

            let saved = crate::settings::Settings::load();
            assert_eq!(
                saved.poll_interval, 30,
                "interval must be persisted to disk"
            );
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

            let (status, json) = update_settings(State(state.clone()), Json(body)).await;
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

            let (status, json) = update_settings(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            let msg = response_message(&json);

            assert!(msg.contains("hidden_panels=3 entries"), "got: {msg}");
            // Don't dump the full list into the log.
            assert!(
                !msg.contains("battery"),
                "panel names must not be logged, got: {msg}"
            );

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
                (
                    "autostart_enabled",
                    json!(true),
                    "autostart_enabled=true",
                    true,
                ),
                (
                    "autostart_enabled",
                    json!(false),
                    "autostart_enabled=false",
                    false,
                ),
                (
                    "disable_auto_discovery",
                    json!(true),
                    "disable_auto_discovery=true",
                    true,
                ),
                (
                    "disable_auto_discovery",
                    json!(false),
                    "disable_auto_discovery=false",
                    false,
                ),
                (
                    "minimise_to_tray",
                    json!(true),
                    "minimise_to_tray=true",
                    true,
                ),
                (
                    "minimise_to_tray",
                    json!(false),
                    "minimise_to_tray=false",
                    false,
                ),
                ("start_minimised", json!(true), "start_minimised=true", true),
                (
                    "start_minimised",
                    json!(false),
                    "start_minimised=false",
                    false,
                ),
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
                    "minimise_to_tray" => saved.minimise_to_tray,
                    "start_minimised" => saved.start_minimised,
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
            crate::test_util::with_isolated_config_dir_async(|| async {
                let state = Arc::new(AppState::new());
                // Fresh isolated config — latest_evc is None and host is empty.
                let (status, json) = evc_status(State(state.clone())).await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(json["ok"], serde_json::Value::Bool(true));
                assert_eq!(json["reachable"], serde_json::Value::Bool(false));
                assert!(json["snapshot"].is_null());
                assert_eq!(json["evc_host"], serde_json::Value::String(String::new()));
                assert_eq!(json["evc_port"], serde_json::json!(502));
            })
            .await;
        }

        #[tokio::test]
        async fn returns_configured_host_even_when_unreachable() {
            let state = crate::test_util::with_isolated_config_dir(|| Arc::new(AppState::new()));
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
            let state = crate::test_util::with_isolated_config_dir(|| Arc::new(AppState::new()));
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

    // ==================================================================
    // Agile scope API tests
    // ==================================================================
    //
    // The new `scope` field is the explicit, additive replacement for the
    // legacy `enabled` boolean. Both shapes must be accepted on POST and
    // GET must return both fields so existing frontends keep working.

    use crate::settings::AgileScope;

    #[tokio::test]
    async fn adaptive_charging_mode_disables_cosy_and_agile() {
        with_isolated_config_dir_async(|| async {
            let state = test_state();
            *state.latest_snapshot.lock().await = Some(InverterSnapshot {
                device_type: DeviceType::Gen3Hybrid,
                device_type_code: "2001".to_string(),
                inverter_serial: "CE234".to_string(),
                charge_rate: 50,
                ..Default::default()
            });
            let mut settings = crate::settings::Settings::default();
            settings.cosy_enabled = true;
            settings.agile_enabled = true;
            settings.agile_scope = crate::settings::AgileScope::Full;
            settings.save().unwrap();

            let (status, _) = set_charging_mode(
                State(state),
                Json(json!({
                    "mode": "adaptive",
                    "cosy_slots": [{
                        "enabled": true,
                        "start_hour": 1,
                        "start_minute": 0,
                        "end_hour": 2,
                        "end_minute": 0,
                        "target_soc": 1000
                    }]
                })),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let saved = crate::settings::Settings::load();
            assert!(saved.adaptive_charge_enabled);
            assert!(!saved.cosy_enabled);
            assert_eq!(saved.agile_scope, crate::settings::AgileScope::Off);
            assert_eq!(saved.cosy_slots[0].target_soc, 100);
        })
        .await;
    }

    #[tokio::test]
    async fn adaptive_config_rejects_overlap_without_persisting() {
        with_isolated_config_dir_async(|| async {
            let state = test_state();
            let body = json!({
                "config": {
                    "confirmation_readings": 2,
                    "periods": [
                        {
                            "enabled": true, "all_day": false,
                            "start_hour": 8, "start_minute": 0,
                            "end_hour": 12, "end_minute": 0,
                            "low_soc": 30, "recovery_soc": 40,
                            "preferred_rate_percent": 40, "recovery_rate_percent": 100
                        },
                        {
                            "enabled": true, "all_day": false,
                            "start_hour": 11, "start_minute": 0,
                            "end_hour": 14, "end_minute": 0,
                            "low_soc": 30, "recovery_soc": 40,
                            "preferred_rate_percent": 40, "recovery_rate_percent": 100
                        }
                    ]
                }
            });
            let (status, _) = set_adaptive_charge(State(state), Json(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(
                crate::settings::Settings::load().adaptive_charge_config,
                crate::settings::AdaptiveChargeConfig::default()
            );
        })
        .await;
    }

    #[tokio::test]
    async fn manual_charge_rate_conflicts_while_adaptive_is_enabled() {
        with_isolated_config_dir_async(|| async {
            let state = test_state();
            let settings = crate::settings::Settings {
                adaptive_charge_enabled: true,
                ..Default::default()
            };
            settings.save().unwrap();

            let (status, Json(body)) =
                set_charge_rate(State(state.clone()), Json(json!({ "limit": 25 }))).await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(body["ok"], false);
            assert!(state.pending_writes.lock().await.is_empty());

            let restoring = crate::settings::Settings {
                adaptive_charge_saved_limit: Some(crate::settings::AdaptiveChargeSavedLimit {
                    inverter_serial: "CE234".to_string(),
                    device_type_code: "2001".to_string(),
                    register_address: crate::modbus::registers::HR_BATTERY_CHARGE_LIMIT,
                    raw_value: 50,
                }),
                ..Default::default()
            };
            restoring.save().unwrap();
            let (status, Json(body)) =
                set_charge_rate(State(state), Json(json!({ "limit": 25 }))).await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(
                body["error"],
                "Adaptive Charge is restoring the previous charge rate"
            );
        })
        .await;
    }

    #[test]
    fn parse_agile_scope_accepts_all_four_variants() {
        assert_eq!(parse_agile_scope("off"), Some(AgileScope::Off));
        assert_eq!(parse_agile_scope("full"), Some(AgileScope::Full));
        assert_eq!(
            parse_agile_scope("charge_only"),
            Some(AgileScope::ChargeOnly)
        );
        assert_eq!(
            parse_agile_scope("discharge_only"),
            Some(AgileScope::DischargeOnly)
        );
    }

    #[test]
    fn parse_agile_scope_rejects_unknown_values() {
        // Unknown values return None so the caller falls back to the
        // legacy boolean path. We don't error on the wire — a typo in
        // the front-end shouldn't break the user's existing settings.
        assert_eq!(parse_agile_scope(""), None);
        assert_eq!(parse_agile_scope("Full"), None); // case-sensitive
        assert_eq!(parse_agile_scope("CHARGE_ONLY"), None);
        assert_eq!(parse_agile_scope("charge"), None);
        assert_eq!(parse_agile_scope("invalid"), None);
    }

    #[tokio::test]
    async fn get_agile_returns_scope_field() {
        // Round-trip: write a known scope, read it back via GET, verify
        // both `enabled` (legacy mirror) and `scope` (new explicit) are
        // present. This pins the wire shape so future field renames
        // can't silently break the front-end.
        crate::test_util::with_isolated_config_dir_async(async || {
            let mut s = crate::settings::Settings::load();
            s.agile_scope = AgileScope::ChargeOnly;
            s.agile_enabled = true;
            s.save().unwrap();

            let (status, Json(body)) = get_agile(State(test_state())).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["ok"], serde_json::json!(true));
            assert_eq!(body["enabled"], serde_json::json!(true)); // legacy mirror
            assert_eq!(body["scope"], serde_json::json!("charge_only"));
        })
        .await;
    }

    #[tokio::test]
    async fn get_agile_off_scope_reports_disabled() {
        crate::test_util::with_isolated_config_dir_async(async || {
            let mut s = crate::settings::Settings::load();
            s.agile_scope = AgileScope::Off;
            s.agile_enabled = false;
            s.save().unwrap();

            let (_status, Json(body)) = get_agile(State(test_state())).await;
            assert_eq!(body["enabled"], serde_json::json!(false));
            assert_eq!(body["scope"], serde_json::json!("off"));
        })
        .await;
    }

    #[tokio::test]
    async fn get_agile_reports_off_when_cosy_is_enabled() {
        crate::test_util::with_isolated_config_dir_async(async || {
            let mut s = crate::settings::Settings::load();
            s.cosy_enabled = true;
            s.agile_scope = AgileScope::Full;
            s.agile_enabled = true;
            s.save().unwrap();

            let (_status, Json(body)) = get_agile(State(test_state())).await;
            assert_eq!(body["enabled"], serde_json::json!(false));
            assert_eq!(body["scope"], serde_json::json!("off"));
        })
        .await;
    }

    #[tokio::test]
    async fn set_cosy_enabled_disables_agile_and_queues_clear() {
        crate::test_util::with_isolated_config_dir_async(async || {
            let mut s = crate::settings::Settings::load();
            s.agile_scope = AgileScope::Full;
            s.agile_enabled = true;
            s.save().unwrap();

            let state = test_state();
            let body = serde_json::json!({ "enabled": true, "slots": [] });
            let (status, _) = set_cosy(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let s = crate::settings::Settings::load();
            assert!(s.cosy_enabled);
            assert_eq!(s.agile_scope, AgileScope::Off);
            assert!(!s.agile_enabled);

            let writes: Vec<crate::inverter::encoder::RegisterWrite> = state
                .pending_writes
                .lock()
                .await
                .iter()
                .flat_map(|b| b.writes.iter().cloned())
                .collect();
            let addresses: Vec<u16> = writes.iter().map(|w| w.address).collect();
            assert!(addresses.contains(&96), "clear must disable charge (HR 96)");
            assert!(
                addresses.contains(&59),
                "clear must disable discharge (HR 59)"
            );
            assert!(
                addresses.contains(&27),
                "clear must restore eco mode (HR 27)"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn set_agile_with_explicit_scope_persists() {
        crate::test_util::with_isolated_config_dir_async(async || {
            // Send the new explicit shape; verify it lands in settings.
            let body = serde_json::json!({
                "scope": "charge_only",
                "region": "C",
                "charge_threshold": 8.5,
                "discharge_threshold": 25.0,
            });
            let (status, _) = set_agile(State(test_state()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let s = crate::settings::Settings::load();
            assert_eq!(s.agile_scope, AgileScope::ChargeOnly);
            assert!(s.agile_enabled); // legacy mirror updated
            assert_eq!(s.agile_region, "C");
            assert!((s.agile_charge_threshold - 8.5).abs() < 1e-9);
            assert!((s.agile_discharge_threshold - 25.0).abs() < 1e-9);
        })
        .await;
    }

    #[tokio::test]
    async fn set_agile_enabled_disables_cosy() {
        crate::test_util::with_isolated_config_dir_async(async || {
            let mut s = crate::settings::Settings::load();
            s.cosy_enabled = true;
            s.cosy_active_persisted = true;
            s.save().unwrap();

            let body = serde_json::json!({ "scope": "full" });
            let (status, _) = set_agile(State(test_state()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let s = crate::settings::Settings::load();
            assert!(!s.cosy_enabled);
            assert!(!s.cosy_active_persisted);
            assert_eq!(s.agile_scope, AgileScope::Full);
            assert!(s.agile_enabled);
        })
        .await;
    }

    #[tokio::test]
    async fn set_agile_scope_off_clears_agile_armed_slot() {
        // Switching Agile off must queue AgileClearActiveSlot writes so the
        // inverter stops acting on Agile's behalf (e.g. stops exporting to the
        // grid on a discharge slot) — issue: scope=off previously left the
        // armed slot in place.
        crate::test_util::with_isolated_config_dir_async(async || {
            let state = test_state();
            let body = serde_json::json!({ "scope": "off" });
            let (status, _) = set_agile(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let writes: Vec<crate::inverter::encoder::RegisterWrite> = state
                .pending_writes
                .lock()
                .await
                .iter()
                .flat_map(|b| b.writes.iter().cloned())
                .collect();
            assert!(!writes.is_empty(), "scope=off must queue clear writes");
            let addresses: Vec<u16> = writes.iter().map(|w| w.address).collect();
            // AgileClearActiveSlot zeroes enable_charge (HR 96) +
            // enable_discharge (HR 59) and restores eco mode (HR 27 = 1).
            assert!(addresses.contains(&96), "clear must disable charge (HR 96)");
            assert!(
                addresses.contains(&59),
                "clear must disable discharge (HR 59)"
            );
            assert!(
                addresses.contains(&27),
                "clear must restore eco mode (HR 27)"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn set_agile_with_legacy_enabled_still_works() {
        // Backwards compat: a frontend that POSTs a bare `{ enabled: true }`
        // (no other Agile fields) is treated as a legacy mode toggle and
        // flips the scope to Full. This is the original back-compat path;
        // see `set_agile_hybrid_enabled_and_partial_preserves_scope` for
        // the hybrid case where `enabled` is sent alongside other fields.
        crate::test_util::with_isolated_config_dir_async(async || {
            let body = serde_json::json!({ "enabled": true });
            let (status, _) = set_agile(State(test_state()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let s = crate::settings::Settings::load();
            assert_eq!(s.agile_scope, AgileScope::Full);
            assert!(s.agile_enabled);
        })
        .await;
    }

    #[tokio::test]
    async fn set_agile_explicit_scope_overrides_legacy_enabled() {
        // If both fields are sent, the explicit scope wins. A front-end
        // that sends `{ enabled: false, scope: "discharge_only" }` is
        // explicitly asking for DischargeOnly — respect that, not the
        // legacy bool.
        crate::test_util::with_isolated_config_dir_async(async || {
            let body = serde_json::json!({
                "enabled": false,
                "scope": "discharge_only",
            });
            let (status, _) = set_agile(State(test_state()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let s = crate::settings::Settings::load();
            assert_eq!(s.agile_scope, AgileScope::DischargeOnly);
            assert!(s.agile_enabled); // mirror updated to match scope
        })
        .await;
    }

    #[tokio::test]
    async fn set_agile_threshold_only_does_not_reset_thresholds_to_defaults() {
        // The original bug: the Control page's mode "Apply" button POSTs
        // `{ scope: "off" }` (no thresholds), and the old set_agile did
        // `body["discharge_threshold"].as_f64().unwrap_or(30.0)` which
        // silently reset the user's saved threshold to 30p every time
        // they hit Apply. This test pins the fix: a scope-only POST must
        // leave the saved thresholds exactly as they were.
        crate::test_util::with_isolated_config_dir_async(async || {
            // Seed: scope=Full with non-default thresholds.
            let seed = serde_json::json!({
                "scope": "full",
                "region": "H",
                "charge_threshold": 8.5,
                "discharge_threshold": 25.0,
            });
            let _ = set_agile(State(test_state()), Json(seed)).await;
            let seeded = crate::settings::Settings::load();
            assert!((seeded.agile_charge_threshold - 8.5).abs() < 1e-9);
            assert!((seeded.agile_discharge_threshold - 25.0).abs() < 1e-9);

            // Now POST a scope-only update — the mode "Apply" shape.
            let body = serde_json::json!({ "scope": "off" });
            let (status, _) = set_agile(State(test_state()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let s = crate::settings::Settings::load();
            assert_eq!(s.agile_scope, AgileScope::Off);
            // The thresholds must NOT have been reset to the defaults (10 / 30).
            assert!(
                (s.agile_charge_threshold - 8.5).abs() < 1e-9,
                "charge threshold was reset to {} instead of staying at 8.5",
                s.agile_charge_threshold
            );
            assert!(
                (s.agile_discharge_threshold - 25.0).abs() < 1e-9,
                "discharge threshold was reset to {} instead of staying at 25.0",
                s.agile_discharge_threshold
            );
        })
        .await;
    }

    #[tokio::test]
    async fn set_agile_threshold_only_preserves_scope() {
        // The companion fix on the front-end side: the threshold "Save"
        // button used to POST `enabled: true`, which the backend read as
        // a legacy mode toggle and flipped the scope back to Full even
        // when the user was adjusting a slider from the Standard page.
        // A threshold-only POST (no `scope`, no `enabled`) must keep the
        // current scope exactly as it is and only update the thresholds.
        crate::test_util::with_isolated_config_dir_async(async || {
            // Seed: scope=Full.
            let seed = serde_json::json!({
                "scope": "full",
                "charge_threshold": 10.0,
                "discharge_threshold": 30.0,
            });
            let _ = set_agile(State(test_state()), Json(seed)).await;

            // Threshold-only PATCH.
            let body = serde_json::json!({
                "charge_threshold": 12.0,
                "discharge_threshold": 28.0,
            });
            let (status, _) = set_agile(State(test_state()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let s = crate::settings::Settings::load();
            assert_eq!(
                s.agile_scope,
                AgileScope::Full,
                "threshold-only POST must not change scope"
            );
            assert!((s.agile_charge_threshold - 12.0).abs() < 1e-9);
            assert!((s.agile_discharge_threshold - 28.0).abs() < 1e-9);
        })
        .await;
    }

    #[tokio::test]
    async fn set_agile_threshold_only_queues_current_slot_action() {
        // Threshold changes can change the current 30-minute decision from
        // charge/discharge ↔ hold. When we already have the current price in
        // cache, set_agile should queue the new action immediately rather than
        // waiting for a full poll/restart. This reproduces the real bug:
        // 35p was discharging at threshold 30, then becomes hold at threshold
        // 40, so the save must queue AgileClearActiveSlot.
        crate::test_util::with_isolated_config_dir_async(async || {
            let state = test_state();
            let notify = state.write_notify.clone();
            let notified = notify.notified();

            let seed = serde_json::json!({
                "scope": "full",
                "charge_threshold": 10.0,
                "discharge_threshold": 30.0,
            });
            let (status, _) = set_agile(State(state.clone()), Json(seed)).await;
            assert_eq!(status, StatusCode::OK);
            state.pending_writes.lock().await.clear();

            let now = chrono::Utc::now().timestamp();
            state.cached_agile_prices.lock().await.push(
                crate::inverter::state_machines::PriceSlot {
                    pence: 35.0,
                    valid_from: now - 60,
                    valid_to: now + 1800,
                },
            );

            let body = serde_json::json!({
                "charge_threshold": 10.0,
                "discharge_threshold": 40.0,
            });
            let (status, _) = set_agile(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            tokio::time::timeout(std::time::Duration::from_millis(50), notified)
                .await
                .expect("threshold-only save should wake the poll loop");

            let writes: Vec<crate::inverter::encoder::RegisterWrite> = state
                .pending_writes
                .lock()
                .await
                .iter()
                .flat_map(|b| b.writes.iter().cloned())
                .collect();
            let addresses: Vec<u16> = writes.iter().map(|w| w.address).collect();
            assert!(
                addresses.contains(&59),
                "clear should disable discharge (HR 59)"
            );
            assert!(
                addresses.contains(&56),
                "clear should zero discharge slot start (HR 56)"
            );
            assert!(
                addresses.contains(&57),
                "clear should zero discharge slot end (HR 57)"
            );
            assert!(
                addresses.contains(&27),
                "clear should restore eco mode (HR 27)"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn set_agile_region_change_clears_cached_prices() {
        // Region/location changes must force a refetch. The price cache is
        // not keyed by region; if we leave the old region's current slot in
        // place, the poll loop sees a cache hit and keeps actioning the old
        // region until restart.
        crate::test_util::with_isolated_config_dir_async(async || {
            let state = test_state();
            state.cached_agile_prices.lock().await.push(
                crate::inverter::state_machines::PriceSlot {
                    pence: 35.0,
                    valid_from: 0,
                    valid_to: 1800,
                },
            );

            let body = serde_json::json!({ "region": "A" });
            let (status, _) = set_agile(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                state.cached_agile_prices.lock().await.is_empty(),
                "region change must clear cached prices so the next poll refetches"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn set_agile_api_base_change_clears_cached_prices() {
        crate::test_util::with_isolated_config_dir_async(async || {
            let state = test_state();
            state.cached_agile_prices.lock().await.push(
                crate::inverter::state_machines::PriceSlot {
                    pence: 35.0,
                    valid_from: 0,
                    valid_to: 1800,
                },
            );

            let body = serde_json::json!({ "api_base_url": "http://127.0.0.1:12345" });
            let (status, _) = set_agile(State(state.clone()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                state.cached_agile_prices.lock().await.is_empty(),
                "API-base change must clear cached prices so tests/self-hosted mirrors refetch"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn set_agile_hybrid_enabled_and_partial_preserves_scope() {
        // Hybrid body: `enabled` sent alongside a partial field (region).
        // Previously the backend treated the legacy `enabled` flag as
        // authoritative even when partial fields were present, which
        // meant a body like `{ enabled: true, region: "A" }` would flip
        // scope to Full. Now a hybrid body is treated as a partial
        // update: partial fields are applied, scope is preserved, and
        // the legacy `enabled` flag is ignored for scope purposes.
        crate::test_util::with_isolated_config_dir_async(async || {
            // Fresh isolated dir → scope defaults to Off.
            let body = serde_json::json!({
                "enabled": true,
                "region": "A",
            });
            let (status, _) = set_agile(State(test_state()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let s = crate::settings::Settings::load();
            assert_eq!(
                s.agile_scope,
                AgileScope::Off,
                "hybrid enabled+partial body must not flip scope to Full"
            );
            assert_eq!(s.agile_region, "A", "region should still be updated");
        })
        .await;
    }

    #[tokio::test]
    async fn set_agile_empty_body_preserves_scope() {
        // Defensive: an empty POST body must not silently turn Agile off.
        // The old code derived scope from the absent `enabled` flag
        // (defaulting to false), so `POST /api/agile {}` flipped scope
        // to Off. Now an empty body leaves scope alone.
        crate::test_util::with_isolated_config_dir_async(async || {
            // Seed: scope=Full.
            let seed = serde_json::json!({ "scope": "full" });
            let _ = set_agile(State(test_state()), Json(seed)).await;

            let body = serde_json::json!({});
            let (status, _) = set_agile(State(test_state()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let s = crate::settings::Settings::load();
            assert_eq!(
                s.agile_scope,
                AgileScope::Full,
                "empty POST must not change scope"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn set_agile_unknown_scope_falls_back_to_legacy() {
        // Unknown scope string — don't error, just fall back to the
        // boolean. The user shouldn't have their settings wiped because
        // of a typo or a down-level front-end.
        crate::test_util::with_isolated_config_dir_async(async || {
            let body = serde_json::json!({
                "scope": "turbo",
                "enabled": true,
            });
            let (status, _) = set_agile(State(test_state()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let s = crate::settings::Settings::load();
            assert_eq!(s.agile_scope, AgileScope::Full);
        })
        .await;
    }

    #[tokio::test]
    async fn set_cosy_clamps_target_soc_to_battery_safe_band() {
        // `cosy_slot_register_writes` writes `slot.target_soc` directly to
        // HR_CHARGE_TARGET_SOC / HR_CHARGE_TARGET_SOC_1, bypassing the
        // encoder's validate_range. So the clamp at the POST /api/cosy
        // boundary is the only guard keeping a forged value out of the
        // register write. Each slot exercises one boundary case:
        //   0   → 4   (decoder reads 0 as "no per-slot target"; clamp to floor)
        //   3   → 4   (below floor)
        //   60  → 60  (in range, untouched)
        //   100 → 100 (ceiling, untouched)
        //   150 → 100 (above ceiling)
        //   1000 → 100 (must clamp on u64 BEFORE `as u8`, else truncates to 232)
        //   absent → 100 (default)
        crate::test_util::with_isolated_config_dir_async(async || {
            let body = serde_json::json!({
                "enabled": true,
                "slots": [
                    { "enabled": true, "start_hour": 4, "start_minute": 0,
                      "end_hour": 7, "end_minute": 0, "target_soc": 0 },
                    { "enabled": true, "start_hour": 13, "start_minute": 0,
                      "end_hour": 16, "end_minute": 0, "target_soc": 3 },
                    { "enabled": true, "start_hour": 22, "start_minute": 0,
                      "end_hour": 23, "end_minute": 0, "target_soc": 60 },
                    { "enabled": true, "start_hour": 1, "start_minute": 0,
                      "end_hour": 2, "end_minute": 0, "target_soc": 100 },
                    { "enabled": true, "start_hour": 2, "start_minute": 0,
                      "end_hour": 3, "end_minute": 0, "target_soc": 150 },
                    { "enabled": true, "start_hour": 3, "start_minute": 0,
                      "end_hour": 4, "end_minute": 0, "target_soc": 1000 },
                    { "enabled": false, "start_hour": 0, "start_minute": 0,
                      "end_hour": 0, "end_minute": 0 }
                ]
            });
            let (status, _) = set_cosy(State(test_state()), Json(body)).await;
            assert_eq!(status, StatusCode::OK);

            let s = crate::settings::Settings::load();
            assert_eq!(s.cosy_slots.len(), 7, "all posted slots must persist");
            let got: Vec<u8> = s.cosy_slots.iter().map(|slot| slot.target_soc).collect();
            assert_eq!(
                got,
                vec![4, 4, 60, 100, 100, 100, 100],
                "target_soc must be clamped to [4, 100] after truncation-safe coerce"
            );
        })
        .await;
    }
}
