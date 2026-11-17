//! HTTP surface for the dongle factory-reset notice (issue #397). The
//! detection lives in `inverter::schedule_reset`; these handlers only turn
//! the user's button presses into writes or a dismissal. Neither probes the
//! dongle: the restore goes through the normal write queue on port 8899.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde_json::{json, Value};

use crate::inverter::encoder::{ControlCommand, RegisterWrite};
use crate::inverter::model::DeviceType;
use crate::inverter::poll::{AppState, WriteBatchPolicy};
use crate::inverter::schedule_reset::{persist, supports_watch, KnownSchedule};
use crate::modbus::registers::encode_hhmm;
use crate::server::api::{
    await_write_outcome_with_timeout, build_charge_slot_writes, discharge_slot_command_for_device,
    queue_writes_with_completion_budget,
};

fn refuse(status: StatusCode, error: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "ok": false, "error": error })))
}

/// The writes that put `schedule` back on an inverter that lost it.
///
/// Slots go first and the arming flags last, so the inverter never sees
/// timed discharge on without its window. Only configured slots are written
/// (the rest are already zero after a reset), and the reserve, power limits
/// and rates are left alone: HEM doesn't keep them, and a limiter, pause or
/// Adaptive Charge may be holding them right now.
pub(crate) fn restore_writes(
    device_type: DeviceType,
    schedule: &KnownSchedule,
) -> Result<Vec<RegisterWrite>, String> {
    if !supports_watch(device_type) {
        return Err("This inverter's schedule can't be restored from HEM.".to_string());
    }
    let mut writes: Vec<RegisterWrite> = Vec::new();
    let mut charge_slots = 0;
    for (index, slot) in schedule.charge_slots.iter().enumerate() {
        if !slot.enabled {
            continue;
        }
        let number = (index + 1) as u8;
        let start = encode_hhmm(slot.start_hour, slot.start_minute)
            .ok_or_else(|| format!("Charge slot {number} has an invalid start time"))?;
        let end = encode_hhmm(slot.end_hour, slot.end_minute)
            .ok_or_else(|| format!("Charge slot {number} has an invalid end time"))?;
        writes.extend(build_charge_slot_writes(
            device_type,
            number,
            true,
            start,
            end,
            slot.target_soc.clamp(4, 100),
            None,
        )?);
        charge_slots += 1;
    }
    // The slot writes arm charging; put the saved flag back if it was off.
    if charge_slots > 0 && !schedule.enable_charge {
        writes.extend(ControlCommand::SetEnableCharge { enabled: false }.encode()?);
    }
    for (index, slot) in schedule.discharge_slots.iter().enumerate() {
        if !slot.enabled {
            continue;
        }
        let number = (index + 1) as u8;
        let start = encode_hhmm(slot.start_hour, slot.start_minute)
            .ok_or_else(|| format!("Discharge slot {number} has an invalid start time"))?;
        let end = encode_hhmm(slot.end_hour, slot.end_minute)
            .ok_or_else(|| format!("Discharge slot {number} has an invalid end time"))?;
        writes.extend(
            discharge_slot_command_for_device(device_type, number, true, start, end)?.encode()?,
        );
        if device_type.uses_extended_schedule_slots() {
            writes.extend(
                ControlCommand::SetDischargeTargetSocSlot {
                    slot: number,
                    soc: u16::from(slot.target_soc.clamp(4, 100)),
                }
                .encode()?,
            );
        }
    }
    if schedule.battery_power_mode == 0 {
        writes.extend(ControlCommand::SetBatteryPowerMode { mode: 0 }.encode()?);
    }
    if schedule.enable_discharge {
        writes.extend(ControlCommand::SetEnableDischarge { enabled: true }.encode()?);
    }
    if writes.is_empty() {
        return Err("The saved schedule has nothing to restore.".to_string());
    }
    Ok(writes)
}

/// `POST /api/control/schedule-reset/restore`: write the last known schedule
/// back. The notice clears once the inverter reads back that schedule (or
/// any other that has held), not on this reply.
pub async fn restore(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    // The watch lock is released before waiting: the poll loop takes it to
    // drain these very writes.
    let schedule = {
        let watch = state.schedule_watch.lock().await;
        if watch.notice().is_none() {
            return refuse(StatusCode::CONFLICT, "No schedule reset to restore.");
        }
        match watch.stored() {
            Some(schedule) => schedule.clone(),
            None => return refuse(StatusCode::CONFLICT, "No saved schedule to restore."),
        }
    };
    let (device_type, device_code) = {
        let snapshot = state.latest_snapshot.lock().await;
        match snapshot.as_ref() {
            Some(s) => (s.device_type, s.device_type_code.clone()),
            None => {
                return refuse(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Not connected to the inverter.",
                )
            }
        }
    };
    if device_code != schedule.device_type_code {
        return refuse(
            StatusCode::CONFLICT,
            "The saved schedule is from a different inverter.",
        );
    }
    // Same lock the Timed Export routes take, so a restore can't interleave
    // with one of them.
    let _timed_export_action = state.timed_export_action_lock.lock().await;
    let snapshot = state.latest_snapshot.lock().await.clone();
    if let Some(owner) =
        crate::inverter::schedule_reset::hem_owns_schedule(&state, snapshot.as_ref()).await
    {
        return refuse(
            StatusCode::CONFLICT,
            &format!(
                "HEM can't restore the schedule while {owner} is running. Try again when it has finished."
            ),
        );
    }
    let writes = match restore_writes(device_type, &schedule) {
        Ok(writes) => writes,
        Err(error) => return refuse(StatusCode::BAD_REQUEST, &error),
    };
    tracing::info!(
        "Restoring the schedule saved before the reset: {} write(s)",
        writes.len()
    );
    let (rx, budget) =
        queue_writes_with_completion_budget(&state, writes, WriteBatchPolicy::FailFast, None).await;
    match await_write_outcome_with_timeout(rx, budget).await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "message": "Schedule restore sent" })),
        ),
        Err(error) => refuse(
            StatusCode::BAD_GATEWAY,
            &format!("The schedule could not be restored: {error}. Try again."),
        ),
    }
}

/// `POST /api/control/schedule-reset/dismiss`: the inverter's schedule is
/// what the user wants; stop showing the notice.
pub async fn dismiss(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let mut watch = state.schedule_watch.lock().await;
    let actions = watch.dismiss();
    // Saved under the watch lock so a poll can't interleave its own save.
    persist(actions).await;
    (
        StatusCode::OK,
        Json(json!({ "ok": true, "message": "Reset notice dismissed" })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::{InverterSnapshot, ScheduleSlot};
    use crate::inverter::schedule_reset::ScheduleResetNotice;
    use crate::test_util::with_isolated_config_dir_async;

    fn slot(start: (u8, u8), end: (u8, u8), soc: u8) -> ScheduleSlot {
        ScheduleSlot {
            enabled: true,
            start_hour: start.0,
            start_minute: start.1,
            end_hour: end.0,
            end_minute: end.1,
            target_soc: soc,
        }
    }

    fn snapshot(device: DeviceType) -> InverterSnapshot {
        let mut snap = InverterSnapshot {
            device_type: device,
            device_type_code: "2201".to_string(),
            battery_power_mode: 1,
            ..Default::default()
        };
        snap.charge_slots[0] = slot((0, 30), (5, 30), 80);
        snap.enable_charge = true;
        snap
    }

    fn registers(writes: &[RegisterWrite]) -> Vec<u16> {
        writes.iter().map(|w| w.address).collect()
    }

    fn position(writes: &[RegisterWrite], register: u16) -> Option<usize> {
        writes.iter().position(|w| w.address == register)
    }

    #[test]
    fn a_charge_schedule_restores_its_slot_and_target() {
        let schedule = KnownSchedule::from_snapshot(&snapshot(DeviceType::Gen3Hybrid), 1);
        let writes = restore_writes(DeviceType::Gen3Hybrid, &schedule).unwrap();
        let regs = registers(&writes);
        assert!(
            regs.contains(&94) && regs.contains(&95),
            "slot 1 times {regs:?}"
        );
        assert!(regs.contains(&242), "per-slot target {regs:?}");
        assert!(regs.contains(&116), "global target {regs:?}");
        let target = writes.iter().find(|w| w.address == 116).unwrap();
        assert_eq!(target.value, 80);
        assert!(!regs.contains(&59), "discharge isn't armed: {regs:?}");
    }

    #[test]
    fn discharge_slots_are_written_before_discharge_is_armed() {
        let mut snap = snapshot(DeviceType::Gen3Hybrid);
        snap.discharge_slots[0] = slot((16, 0), (19, 0), 20);
        snap.discharge_slots[1] = slot((21, 0), (22, 0), 30);
        snap.enable_discharge = true;
        snap.battery_power_mode = 0;
        let writes = restore_writes(
            DeviceType::Gen3Hybrid,
            &KnownSchedule::from_snapshot(&snap, 1),
        )
        .unwrap();
        let arm = position(&writes, 59).expect("HR 59 written");
        let mode = position(&writes, 27).expect("export mode restored");
        for slot_register in [56, 57, 44, 45] {
            let at = position(&writes, slot_register).unwrap_or_else(|| panic!("{slot_register}"));
            assert!(
                at < arm,
                "slot register {slot_register} after the arming write"
            );
        }
        assert!(mode < arm, "mode before arming");
        assert_eq!(writes[arm].value, 1);
        assert_eq!(arm, writes.len() - 1, "arming is the last write");
    }

    #[test]
    fn a_saved_off_charge_flag_is_put_back_off() {
        let mut snap = snapshot(DeviceType::Gen3Hybrid);
        snap.enable_charge = false;
        let writes = restore_writes(
            DeviceType::Gen3Hybrid,
            &KnownSchedule::from_snapshot(&snap, 1),
        )
        .unwrap();
        let last_flag = writes.iter().rev().find(|w| w.address == 96).unwrap();
        assert_eq!(last_flag.value, 0);
    }

    #[test]
    fn eco_with_nothing_set_up_has_nothing_to_restore() {
        let mut snap = snapshot(DeviceType::Gen3Hybrid);
        snap.charge_slots[0] = ScheduleSlot::default();
        let schedule = KnownSchedule::from_snapshot(&snap, 1);
        assert!(restore_writes(DeviceType::Gen3Hybrid, &schedule).is_err());
    }

    #[test]
    fn it_never_writes_reserve_or_power_limits() {
        let mut snap = snapshot(DeviceType::Gen3Hybrid);
        snap.discharge_slots[0] = slot((16, 0), (19, 0), 20);
        snap.enable_discharge = true;
        let writes = restore_writes(
            DeviceType::Gen3Hybrid,
            &KnownSchedule::from_snapshot(&snap, 1),
        )
        .unwrap();
        for register in [110, 111, 112, 313, 314, 318, 319, 320] {
            assert!(
                !registers(&writes).contains(&register),
                "HR {register} must be left to the limiters"
            );
        }
    }

    #[test]
    fn unsupported_models_are_refused() {
        let schedule = KnownSchedule::from_snapshot(&snapshot(DeviceType::Gen3Hybrid), 1);
        for device in [DeviceType::ThreePhase, DeviceType::Gateway] {
            assert!(restore_writes(device, &schedule).is_err(), "{device:?}");
        }
    }

    async fn state_with_reset(device_code: &str) -> Arc<AppState> {
        let state = Arc::new(AppState::new());
        let snap = snapshot(DeviceType::Gen3Hybrid);
        let mut stored = KnownSchedule::from_snapshot(&snap, 1_000);
        stored.device_type_code = device_code.to_string();
        state.schedule_watch.lock().await.ensure_loaded(
            Some(&stored),
            Some(ScheduleResetNotice { detected_at: 2_000 }),
        );
        *state.latest_snapshot.lock().await = Some(snap);
        state
    }

    #[tokio::test]
    async fn restore_queues_the_saved_schedule() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_reset("2201").await;
            // The poll loop isn't running; resolve the batch like it would.
            let pending = state.clone();
            let drain = tokio::spawn(async move {
                loop {
                    let batch = pending.pending_writes.lock().await.pop();
                    if let Some(batch) = batch {
                        assert_eq!(batch.policy, WriteBatchPolicy::FailFast);
                        if let Some(tx) = batch.completion {
                            let _ = tx.send(crate::inverter::encoder::WriteOutcome::Ok);
                        }
                        return batch.writes.len();
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            });
            let (status, Json(body)) = restore(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert!(drain.await.unwrap() > 0);
        })
        .await;
    }

    #[tokio::test]
    async fn restore_reports_a_rejected_write() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_reset("2201").await;
            let pending = state.clone();
            tokio::spawn(async move {
                loop {
                    let batch = pending.pending_writes.lock().await.pop();
                    if let Some(batch) = batch {
                        if let Some(tx) = batch.completion {
                            let _ = tx.send(crate::inverter::encoder::WriteOutcome::Failed {
                                address: 94,
                                value: 30,
                                error: "exception 67".to_string(),
                            });
                        }
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            });
            let (status, Json(body)) = restore(State(state)).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
            assert!(body["error"]
                .as_str()
                .unwrap()
                .contains("could not be restored"));
        })
        .await;
    }

    #[tokio::test]
    async fn restore_waits_while_hem_is_running_its_own_schedule() {
        // A restore would arm discharge windows another state machine owns.
        with_isolated_config_dir_async(|| async {
            for owner in ["cosy", "timed_export"] {
                let state = state_with_reset("2201").await;
                match owner {
                    "cosy" => *state.cosy_active.lock().await = true,
                    _ => state.timed_export_config.lock().await.schedule_enabled = true,
                }
                let (status, Json(body)) = restore(State(state.clone())).await;
                assert_eq!(status, StatusCode::CONFLICT, "{owner}: {body}");
                assert!(body["error"].as_str().unwrap().contains("can't restore"));
                assert!(state.pending_writes.lock().await.is_empty(), "{owner}");
            }
        })
        .await;
    }

    #[tokio::test]
    async fn restore_with_no_reset_flagged_writes_nothing() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            state.schedule_watch.lock().await.ensure_loaded(None, None);
            let (status, _) = restore(State(state.clone())).await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert!(state.pending_writes.lock().await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn restore_refuses_another_inverters_schedule() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_reset("3001").await;
            let (status, _) = restore(State(state.clone())).await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert!(state.pending_writes.lock().await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn restore_without_a_connection_writes_nothing() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_reset("2201").await;
            *state.latest_snapshot.lock().await = None;
            let (status, _) = restore(State(state.clone())).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(state.pending_writes.lock().await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn dismiss_clears_the_notice_in_memory_and_on_disk() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_reset("2201").await;
            crate::settings::Settings::update_async(|s| {
                s.schedule_reset_notice = Some(ScheduleResetNotice { detected_at: 2_000 });
            })
            .await
            .unwrap();
            let (status, _) = dismiss(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);
            assert!(state.schedule_watch.lock().await.notice().is_none());
            assert!(crate::settings::Settings::load()
                .schedule_reset_notice
                .is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn an_alert_owed_while_alerts_are_off_is_sent_once_they_are_on() {
        with_isolated_config_dir_async(|| async {
            let state = state_with_reset("2201").await;
            // A reset flagged while no alert channel is set up.
            {
                let mut watch = state.schedule_watch.lock().await;
                let reset = snapshot(DeviceType::Gen3Hybrid);
                *watch = crate::inverter::schedule_reset::ScheduleWatch::default();
                watch.ensure_loaded(Some(&KnownSchedule::from_snapshot(&reset, 50)), None);
                let mut empty = reset.clone();
                empty.charge_slots[0] = ScheduleSlot::default();
                watch.observe(&reset, 100, true, 10);
                for i in 0..3 {
                    watch.observe(&empty, 400 + i * 10, true, 10);
                }
                assert!(watch.notice().is_some());
            }
            let sent = Arc::new(std::sync::atomic::AtomicU32::new(0));
            let poll = |at: i64, sent: Arc<std::sync::atomic::AtomicU32>| {
                let state = state.clone();
                async move {
                    let mut empty = snapshot(DeviceType::Gen3Hybrid);
                    empty.charge_slots[0] = ScheduleSlot::default();
                    crate::inverter::schedule_reset::run_watch(&state, &empty, at, true, 10, {
                        move |_, _| async move {
                            sent.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            true
                        }
                    })
                    .await;
                    tokio::task::yield_now().await;
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            };
            poll(500, sent.clone()).await;
            assert_eq!(
                sent.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "nobody to tell"
            );

            *state.alert_config.lock().await = crate::settings::AlertsConfig {
                enabled: true,
                ntfy_topic: "hem-test".into(),
                ..Default::default()
            };
            poll(510, sent.clone()).await;
            assert_eq!(
                sent.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "sent once on"
            );
            poll(520, sent.clone()).await;
            assert_eq!(
                sent.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "delivered: not again"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_poll_and_a_dismiss_racing_each_lose_neither_update() {
        // The poll saves a newly settled schedule while the user dismisses:
        // both must land, whatever the order.
        with_isolated_config_dir_async(|| async {
            let state = state_with_reset("2201").await;
            crate::settings::Settings::update_async(|s| {
                s.schedule_reset_notice = Some(ScheduleResetNotice { detected_at: 2_000 });
            })
            .await
            .unwrap();
            let mut changed = snapshot(DeviceType::Gen3Hybrid);
            changed.charge_slots[0] = slot((1, 0), (4, 0), 90);
            let polls = {
                let state = state.clone();
                let changed = changed.clone();
                tokio::spawn(async move {
                    for i in 0..3 {
                        crate::inverter::schedule_reset::run_watch(
                            &state,
                            &changed,
                            3_000 + i * 10,
                            true,
                            10,
                            |_, _| async { true },
                        )
                        .await;
                    }
                })
            };
            let dismissal = tokio::spawn(dismiss(State(state.clone())));
            polls.await.unwrap();
            let _ = dismissal.await.unwrap();
            // A couple more polls so the schedule settles whichever came first.
            for i in 3..6 {
                crate::inverter::schedule_reset::run_watch(
                    &state,
                    &changed,
                    3_000 + i * 10,
                    true,
                    10,
                    |_, _| async { true },
                )
                .await;
            }
            let settings = crate::settings::Settings::load();
            assert!(settings.schedule_reset_notice.is_none(), "dismissal kept");
            let kept = settings.last_known_schedule.expect("schedule kept");
            assert_eq!(kept.charge_slots[0].start_hour, 1, "the new schedule saved");
        })
        .await;
    }
}
