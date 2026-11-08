//! `/api/charge-cap`: read and save the optional charge cap (issue #426).
//! The cap itself runs in the poll loop (`inverter::charge_cap`); saving only
//! changes its config, which the next poll acts on.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, Json};
use serde_json::{json, Value};

use crate::inverter::charge_cap::{
    self, ChargeCapConfig, ChargeCapShared, MAX_CAP_SOC, MIN_CAP_SOC, MIN_GAP, MIN_RESUME_SOC,
};
use crate::inverter::poll::{AppState, PollMessage};

fn reply(status: StatusCode, error: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "ok": false, "error": error })))
}

/// Whether the connected inverter can run the cap; `None` before the first
/// snapshot.
async fn supported(state: &AppState) -> Option<bool> {
    state
        .latest_snapshot
        .lock()
        .await
        .as_ref()
        .map(charge_cap::snapshot_supports)
}

fn view(shared: &ChargeCapShared, supported: Option<bool>) -> Value {
    let state = charge_cap::shown_state(shared, supported.unwrap_or(true));
    json!({
        "ok": true,
        "enabled": shared.config.enabled,
        "cap_soc": shared.config.cap_soc,
        "resume_soc": shared.config.resume_soc,
        "state": state.api_name(),
        "supported": supported.unwrap_or(false),
        "connected": supported.is_some(),
        "holding": shared.hold.is_some(),
        "min_cap_soc": MIN_CAP_SOC,
        "max_cap_soc": MAX_CAP_SOC,
        "min_resume_soc": MIN_RESUME_SOC,
        "min_gap": MIN_GAP,
    })
}

/// GET /api/charge-cap — the config, what the cap is doing, and whether this
/// inverter supports it.
pub async fn get_charge_cap(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let supported = supported(&state).await;
    let shared = state.charge_cap.lock().await.clone();
    let mut body = view(&shared, supported);
    // For the reminder to let the battery reach 100% now and then.
    body["last_full_charge_at"] =
        json!(crate::alerts::full_charge::last_full_charge_at(&state).await);
    body["full_charge_days"] = json!(state.alert_config.lock().await.full_charge_alert_days);
    (StatusCode::OK, Json(body))
}

/// The POST body. Every field is required: `ChargeCapConfig`'s defaults
/// are for reading settings, and would turn a partial body into a silent
/// switch-off.
#[derive(serde::Deserialize)]
struct ChargeCapBody {
    enabled: bool,
    cap_soc: u8,
    resume_soc: u8,
}

/// POST /api/charge-cap — save `{ enabled, cap_soc, resume_soc }`.
///
/// Switching it on is refused on an inverter that can't pause charging.
/// Switching it off is always allowed: the next poll hands back any pause
/// the cap holds.
pub async fn set_charge_cap(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let config = match serde_json::from_value::<ChargeCapBody>(body) {
        Ok(body) => ChargeCapConfig {
            enabled: body.enabled,
            cap_soc: body.cap_soc,
            resume_soc: body.resume_soc,
        },
        Err(e) => return reply(StatusCode::BAD_REQUEST, &format!("Invalid charge cap: {e}")),
    };
    if let Err(e) = config.validate() {
        return reply(StatusCode::BAD_REQUEST, &e);
    }
    let supported = supported(&state).await;
    if config.enabled && supported == Some(false) {
        return reply(
            StatusCode::BAD_REQUEST,
            "This inverter can't pause charging, so the charge cap isn't available",
        );
    }

    // Saved under the cap's lock, so a poll never acts on a config that
    // isn't on disk, and two saves land in the order they took the lock.
    let shared = {
        let mut guard = state.charge_cap.lock().await;
        let saved = config.clone();
        if let Err(e) = crate::settings::Settings::update_async(move |s| s.charge_cap = saved).await
        {
            return reply(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Failed to save the charge cap: {e}"),
            );
        }
        guard.config = config;
        guard.clone()
    };
    tracing::info!(
        enabled = shared.config.enabled,
        cap_soc = shared.config.cap_soc,
        resume_soc = shared.config.resume_soc,
        "Charge cap saved"
    );

    // Show the change on the current snapshot now rather than after the
    // next poll (the gauge's cap line follows `charge_cap_enabled`).
    {
        let mut latest = state.latest_snapshot.lock().await;
        if let Some(snap) = latest.as_mut() {
            charge_cap::stamp_snapshot(snap, &shared);
            let _ = state.tx.send(PollMessage::Snapshot(Box::new(snap.clone())));
        }
    }
    (StatusCode::OK, Json(view(&shared, supported)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::charge_cap::{ChargeCapHold, ChargeCapState};
    use crate::inverter::model::{DeviceType, InverterSnapshot};
    use crate::test_util::with_isolated_config_dir_async;

    async fn state_with(device_type: Option<DeviceType>) -> Arc<AppState> {
        let state = Arc::new(AppState::new());
        if let Some(device_type) = device_type {
            *state.latest_snapshot.lock().await = Some(InverterSnapshot {
                device_type,
                inverter_serial: "SA1234".into(),
                device_type_code: "8001".into(),
                firmware_version: "300".into(),
                soc: 60,
                ..Default::default()
            });
        }
        state
    }

    async fn get(state: &Arc<AppState>) -> Value {
        let (code, Json(value)) = get_charge_cap(State(state.clone())).await;
        assert_eq!(code, StatusCode::OK);
        value
    }

    async fn post(state: &Arc<AppState>, body: Value) -> (StatusCode, Value) {
        let (code, Json(value)) = set_charge_cap(State(state.clone()), Json(body)).await;
        (code, value)
    }

    #[tokio::test]
    async fn a_partial_body_is_refused_rather_than_filled_with_defaults() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(Some(DeviceType::AllInOne6kW)).await;
            let (code, _) = post(
                &state,
                json!({ "enabled": true, "cap_soc": 90, "resume_soc": 85 }),
            )
            .await;
            assert_eq!(code, StatusCode::OK);
            let (code, body) = post(&state, json!({ "cap_soc": 80 })).await;
            assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
            let config = state.charge_cap.lock().await.config.clone();
            assert!(config.enabled, "still on");
            assert_eq!((config.cap_soc, config.resume_soc), (90, 85));
        })
        .await;
    }

    #[tokio::test]
    async fn starts_off_and_reports_support() {
        with_isolated_config_dir_async(|| async {
            let aio = state_with(Some(DeviceType::AllInOne6kW)).await;
            let body = get(&aio).await;
            assert_eq!(body["enabled"], false);
            assert_eq!(body["cap_soc"], 95);
            assert_eq!(body["resume_soc"], 92);
            assert_eq!(body["state"], "off");
            assert_eq!(body["supported"], true);
            assert_eq!(body["connected"], true);

            let gateway = state_with(Some(DeviceType::Gateway)).await;
            let body = get(&gateway).await;
            assert_eq!(body["supported"], false);
            assert_eq!(body["state"], "unsupported");

            let offline = state_with(None).await;
            let body = get(&offline).await;
            assert_eq!(body["connected"], false);
            assert_eq!(body["supported"], false);
        })
        .await;
    }

    #[tokio::test]
    async fn saving_persists_and_restamps_the_snapshot() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(Some(DeviceType::AllInOne6kW)).await;
            let mut rx = state.tx.subscribe();
            let (code, body) = post(
                &state,
                json!({ "enabled": true, "cap_soc": 90, "resume_soc": 85 }),
            )
            .await;
            assert_eq!(code, StatusCode::OK, "{body}");
            assert_eq!(body["enabled"], true);
            assert_eq!(body["cap_soc"], 90);

            let saved = crate::settings::Settings::load().charge_cap;
            assert_eq!(
                saved,
                ChargeCapConfig {
                    enabled: true,
                    cap_soc: 90,
                    resume_soc: 85
                }
            );
            assert_eq!(state.charge_cap.lock().await.config, saved);
            let snap = state.latest_snapshot.lock().await.clone().unwrap();
            assert!(snap.charge_cap_enabled);
            assert_eq!(snap.charge_cap_soc, 90);
            assert!(matches!(rx.try_recv(), Ok(PollMessage::Snapshot(s)) if s.charge_cap_enabled));
            assert!(
                state.pending_writes.lock().await.is_empty(),
                "the poll loop does the writing"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn invalid_levels_are_rejected_and_nothing_is_saved() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(Some(DeviceType::AllInOne6kW)).await;
            for body in [
                json!({ "enabled": true, "cap_soc": 100, "resume_soc": 90 }),
                json!({ "enabled": true, "cap_soc": 95, "resume_soc": 94 }),
                json!({ "enabled": true, "cap_soc": 95, "resume_soc": 30 }),
                json!({ "enabled": "yes", "cap_soc": 95, "resume_soc": 90 }),
                json!({ "enabled": true, "cap_soc": -1, "resume_soc": 90 }),
            ] {
                let (code, reply) = post(&state, body.clone()).await;
                assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
                assert_eq!(reply["ok"], false);
            }
            assert_eq!(
                crate::settings::Settings::load().charge_cap,
                ChargeCapConfig::default()
            );
        })
        .await;
    }

    #[tokio::test]
    async fn cannot_be_switched_on_where_charging_cannot_be_paused() {
        with_isolated_config_dir_async(|| async {
            for device in [DeviceType::Gateway, DeviceType::Gen4Hybrid] {
                let state = state_with(Some(device)).await;
                let (code, _) = post(
                    &state,
                    json!({ "enabled": true, "cap_soc": 95, "resume_soc": 92 }),
                )
                .await;
                assert_eq!(code, StatusCode::BAD_REQUEST, "{device:?}");
                // Switching off is always allowed.
                let (code, _) = post(
                    &state,
                    json!({ "enabled": false, "cap_soc": 95, "resume_soc": 92 }),
                )
                .await;
                assert_eq!(code, StatusCode::OK, "{device:?}");
            }
        })
        .await;
    }

    #[tokio::test]
    async fn can_be_set_up_before_the_inverter_is_connected() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(None).await;
            let (code, _) = post(
                &state,
                json!({ "enabled": true, "cap_soc": 95, "resume_soc": 92 }),
            )
            .await;
            assert_eq!(code, StatusCode::OK);
        })
        .await;
    }

    #[tokio::test]
    async fn switching_off_while_holding_shows_releasing_until_the_poll_lets_go() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(Some(DeviceType::AllInOne6kW)).await;
            {
                let mut cap = state.charge_cap.lock().await;
                cap.config.enabled = true;
                cap.runtime.state = ChargeCapState::Holding;
                cap.hold = Some(ChargeCapHold {
                    inverter_serial: "SA1234".into(),
                    device_type_code: "8001".into(),
                    prior_slot_start: 0,
                    prior_slot_end: 0,
                    mode_only: false,
                });
            }
            let (code, body) = post(
                &state,
                json!({ "enabled": false, "cap_soc": 95, "resume_soc": 92 }),
            )
            .await;
            assert_eq!(code, StatusCode::OK);
            assert_eq!(body["holding"], true, "the pause is still on the inverter");
            assert_eq!(body["state"], "releasing", "not \"holding\" once it's off");
            assert!(
                state.charge_cap.lock().await.hold.is_some(),
                "only the poll loop drops the record"
            );
        })
        .await;
    }
}
