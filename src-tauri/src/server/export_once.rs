//! HTTP surface for one-off Timed Export ("Export Once"). The booking logic
//! lives in `inverter::export_once`; these handlers only translate.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::inverter::export_once::{
    cancel_event, create_event, inverter_now, list_events, NewEventRequest, SOURCE_MANUAL,
};
use crate::inverter::poll::AppState;

/// GET /api/export-once — the booked events plus the inverter-local "now"
/// the UI should default dates from (the browser may be in another timezone).
pub async fn get_export_once(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let now = inverter_now(&state).await;
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "now": crate::inverter::export_once::format_local(now),
            "events": list_events().await,
        })),
    )
}

/// POST /api/export-once — book an event.
///
/// Body: `{ "slot": 1, "date": "2026-10-17", "start": "18:00",
/// "end": "19:00", "target_soc": 20 }`.
pub async fn create_export_once(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let request: NewEventRequest = match serde_json::from_value(body) {
        Ok(request) => request,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": format!("Invalid export request: {error}") })),
            );
        }
    };
    match create_event(&state, &request, SOURCE_MANUAL).await {
        Ok(event) => (StatusCode::OK, Json(json!({ "ok": true, "event": event }))),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": error })),
        ),
    }
}

#[derive(Deserialize)]
pub struct CancelRequest {
    id: String,
}

/// POST /api/export-once/cancel — cancel a booked event; an active one is
/// reverted before the response returns.
pub async fn cancel_export_once(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CancelRequest>,
) -> (StatusCode, Json<Value>) {
    match cancel_event(&state, &body.id).await {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": error })),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::{DeviceType, InverterSnapshot};
    use crate::test_util::with_isolated_config_dir_async;

    async fn make_state() -> Arc<AppState> {
        let state = Arc::new(AppState::new());
        *state.latest_snapshot.lock().await = Some(InverterSnapshot {
            device_type: DeviceType::Gen2Hybrid,
            inverter_time: "2026-10-17 12:00:00".to_string(),
            ..Default::default()
        });
        state
    }

    fn booking(slot: u64, start: &str, end: &str) -> Value {
        json!({
            "slot": slot, "date": "2026-10-17",
            "start": start, "end": end, "target_soc": 20
        })
    }

    #[tokio::test]
    async fn booking_then_listing_then_cancelling_round_trips() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;

            let (status, Json(created)) =
                create_export_once(State(state.clone()), Json(booking(1, "18:00", "19:00"))).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(created["event"]["state"], "pending");
            let id = created["event"]["id"].as_str().unwrap().to_string();

            let (status, Json(listed)) = get_export_once(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(listed["now"], "2026-10-17 12:00");
            assert_eq!(listed["events"].as_array().unwrap().len(), 1);

            let (status, _) =
                cancel_export_once(State(state.clone()), Json(CancelRequest { id: id.clone() }))
                    .await;
            assert_eq!(status, StatusCode::OK);
            let (_, Json(listed)) = get_export_once(State(state.clone())).await;
            assert_eq!(listed["events"][0]["state"], "cancelled");

            // Cancelling twice is a clear error, not a silent success.
            let (status, Json(again)) =
                cancel_export_once(State(state), Json(CancelRequest { id })).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(again["ok"], false);
        })
        .await;
    }

    #[tokio::test]
    async fn invalid_bookings_are_rejected_with_a_message() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            // Gen2 hybrid has two discharge slots.
            let (status, Json(body)) =
                create_export_once(State(state.clone()), Json(booking(3, "18:00", "19:00"))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(body["error"].as_str().unwrap().contains("Slot"));

            let (status, _) =
                create_export_once(State(state.clone()), Json(booking(1, "19:00", "18:00"))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);

            // Missing fields never reach the booking logic.
            let (status, Json(body)) =
                create_export_once(State(state.clone()), Json(json!({ "slot": 1 }))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(body["error"]
                .as_str()
                .unwrap()
                .contains("Invalid export request"));

            let (_, Json(listed)) = get_export_once(State(state)).await;
            assert!(listed["events"].as_array().unwrap().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn cancelling_an_unknown_id_is_an_error() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            let (status, Json(body)) = cancel_export_once(
                State(state),
                Json(CancelRequest {
                    id: "nope".to_string(),
                }),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(body["ok"], false);
        })
        .await;
    }
}
