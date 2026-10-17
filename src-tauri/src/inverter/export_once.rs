//! One-off Timed Export ("Export Once").
//!
//! Timed Export slots are daily `HH:MM` windows with no date, so they cannot
//! express a single dated export. An [`ExportOnceEvent`] books one: shortly
//! before it starts, the window is written into a slot the user picked (the
//! slot's previous content is captured first), and when it ends the slot is
//! restored. Applying and reverting go through the same Timed Export
//! slot-save / stop handlers the Control page uses, so Cosy and Agile see an
//! ordinary enabled export window and defer to it as they always do.
//!
//! This module holds the pure parts — the model, validation and the planner
//! that decides what to do at a given instant. The async executor that runs
//! the plan against the inverter is added alongside.

use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike as _};
use serde::{Deserialize, Serialize};

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{json, Value};

use crate::inverter::model::{DeviceType, ScheduleSlot};
use crate::inverter::poll::{AppState, ConnectionState};
use crate::server::api::{set_discharge_slot_locked, set_timed_export_locked};
use crate::settings::Settings;

/// Wire/storage format for event instants (inverter-local, minute precision).
pub const TIME_FORMAT: &str = "%Y-%m-%d %H:%M";
/// How long before its start an event is written into its slot. Gives the
/// slot-save handler time to retry a busy dongle before the window opens.
pub const APPLY_LEAD_MINUTES: i64 = 10;
/// Furthest ahead an event may be booked.
pub const MAX_BOOKING_AHEAD_DAYS: i64 = 14;
/// Failed apply attempts before an event gives up and reverts. Attempts are
/// spaced [`RETRY_SPACING_MINUTES`] apart, so six cover most of the lead window.
pub const MAX_APPLY_ATTEMPTS: u8 = 6;
/// Minimum gap between attempts to apply or revert the same event.
const RETRY_SPACING_MINUTES: i64 = 1;
/// Source tag for events created on the Control page.
pub const SOURCE_MANUAL: &str = "manual";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportOnceState {
    /// Booked; the slot has not been touched.
    #[default]
    Pending,
    /// The slot's previous content is captured and the event owns the slot
    /// until it is reverted.
    Active,
    /// Finished and the slot restored (or deliberately left to the user).
    Done,
    Cancelled,
    /// Never ran, or could not be applied. See `note`.
    Failed,
}

impl ExportOnceState {
    /// Whether the event still occupies its slot's time range.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Pending | Self::Active)
    }
}

/// What the slot held, and whether the schedule was armed, before an event
/// took it over.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportOnceOriginal {
    pub slot: ScheduleSlot,
    pub schedule_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportOnceEvent {
    pub id: String,
    /// 1-based discharge slot used for the override.
    pub slot: u8,
    /// `YYYY-MM-DD HH:MM`, inverter-local.
    pub start: String,
    pub end: String,
    /// Discharge floor written with the slot (4-99).
    pub target_soc: u8,
    /// [`SOURCE_MANUAL`], or e.g. `saving_session:<id>`.
    pub source: String,
    #[serde(default)]
    pub state: ExportOnceState,
    /// Captured when the event goes `Active`.
    #[serde(default)]
    pub original: Option<ExportOnceOriginal>,
    /// Whether the event window has actually been written to the slot.
    #[serde(default)]
    pub applied: bool,
    #[serde(default)]
    pub attempts: u8,
    /// Human-readable reason for `Failed`, or a note on how it ended.
    #[serde(default)]
    pub note: Option<String>,
}

impl ExportOnceEvent {
    pub fn start_at(&self) -> Option<NaiveDateTime> {
        parse_local(&self.start)
    }

    pub fn end_at(&self) -> Option<NaiveDateTime> {
        parse_local(&self.end)
    }
}

/// A request to book a new event (the POST body).
#[derive(Debug, Clone, Deserialize)]
pub struct NewEventRequest {
    pub slot: u8,
    /// `YYYY-MM-DD`
    pub date: String,
    /// `HH:MM`
    pub start: String,
    pub end: String,
    pub target_soc: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedAction {
    /// Write the event window into its slot (capturing the original first
    /// if that has not happened yet).
    Apply(String),
    /// Restore the slot and finish the event.
    Revert(String),
    /// The event's window passed without it ever being applied.
    Expire(String),
}

pub fn parse_local(value: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(value, TIME_FORMAT).ok()
}

pub fn format_local(value: NaiveDateTime) -> String {
    value.format(TIME_FORMAT).to_string()
}

/// Decide what to do at `now`. Pure: the caller passes the inverter clock.
pub fn plan(events: &[ExportOnceEvent], now: NaiveDateTime) -> Vec<PlannedAction> {
    let lead = Duration::minutes(APPLY_LEAD_MINUTES);
    let mut actions = Vec::new();
    for event in events {
        match event.state {
            ExportOnceState::Pending => {
                let (Some(start), Some(end)) = (event.start_at(), event.end_at()) else {
                    actions.push(PlannedAction::Expire(event.id.clone()));
                    continue;
                };
                if end <= now {
                    actions.push(PlannedAction::Expire(event.id.clone()));
                } else if now >= start - lead {
                    // A slot carries one override at a time: a back-to-back
                    // event waits until the previous one has reverted.
                    let slot_busy = events.iter().any(|other| {
                        other.id != event.id
                            && other.slot == event.slot
                            && other.state == ExportOnceState::Active
                    });
                    if !slot_busy {
                        actions.push(PlannedAction::Apply(event.id.clone()));
                    }
                }
            }
            ExportOnceState::Active => match event.end_at() {
                Some(end) if end > now => {
                    if !event.applied {
                        if event.attempts >= MAX_APPLY_ATTEMPTS {
                            actions.push(PlannedAction::Revert(event.id.clone()));
                        } else {
                            actions.push(PlannedAction::Apply(event.id.clone()));
                        }
                    }
                }
                _ => actions.push(PlannedAction::Revert(event.id.clone())),
            },
            ExportOnceState::Done | ExportOnceState::Cancelled | ExportOnceState::Failed => {}
        }
    }
    actions
}

/// Validate a booking and build the pending event.
pub fn validate_new(
    request: &NewEventRequest,
    existing: &[ExportOnceEvent],
    now: NaiveDateTime,
    max_slots: u8,
    source: &str,
    id: String,
) -> Result<ExportOnceEvent, String> {
    if request.slot < 1 || request.slot > max_slots {
        return Err(format!(
            "Slot must be between 1 and {max_slots} on this inverter"
        ));
    }
    if !(4..=99).contains(&request.target_soc) {
        return Err("Discharge floor must be between 4 and 99".to_string());
    }
    let date = NaiveDate::parse_from_str(&request.date, "%Y-%m-%d")
        .map_err(|_| "Date must look like 2026-10-17".to_string())?;
    let start_time = NaiveTime::parse_from_str(&request.start, "%H:%M")
        .map_err(|_| "Start time must look like 18:00".to_string())?;
    let end_time = NaiveTime::parse_from_str(&request.end, "%H:%M")
        .map_err(|_| "End time must look like 19:00".to_string())?;
    if end_time <= start_time {
        return Err("End must be after start (windows cannot cross midnight)".to_string());
    }
    let start = date.and_time(start_time);
    let end = date.and_time(end_time);
    if end <= now {
        return Err("That window has already ended".to_string());
    }
    if date > now.date() + Duration::days(MAX_BOOKING_AHEAD_DAYS) {
        return Err(format!(
            "Exports can be booked at most {MAX_BOOKING_AHEAD_DAYS} days ahead"
        ));
    }
    let clash = existing.iter().filter(|e| e.state.is_live()).any(|other| {
        other.slot == request.slot
            && match (other.start_at(), other.end_at()) {
                (Some(other_start), Some(other_end)) => start < other_end && other_start < end,
                _ => false,
            }
    });
    if clash {
        return Err(format!(
            "Slot {} already has an export booked that overlaps this window",
            request.slot
        ));
    }
    Ok(ExportOnceEvent {
        id,
        slot: request.slot,
        start: format_local(start),
        end: format_local(end),
        target_soc: request.target_soc,
        source: source.to_string(),
        state: ExportOnceState::Pending,
        original: None,
        applied: false,
        attempts: 0,
        note: None,
    })
}

/// Whether `slot` still holds exactly the window `event` wrote. When it does
/// not, the user edited the slot during the event and their edit wins.
pub fn slot_matches_event(slot: &ScheduleSlot, event: &ExportOnceEvent) -> bool {
    let (Some(start), Some(end)) = (event.start_at(), event.end_at()) else {
        return false;
    };
    slot.enabled
        && slot.target_soc == event.target_soc
        && slot.start_hour as u32 == start.hour()
        && slot.start_minute as u32 == start.minute()
        && slot.end_hour as u32 == end.hour()
        && slot.end_minute as u32 == end.minute()
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

/// How often the background loop looks for work.
const TICK_SECS: u64 = 15;
/// Finished events kept for the Control page's history list.
const KEEP_FINISHED: usize = 20;

fn disabled_slot() -> ScheduleSlot {
    ScheduleSlot {
        enabled: false,
        start_hour: 0,
        start_minute: 0,
        end_hour: 0,
        end_minute: 0,
        target_soc: 4,
    }
}

/// The slot content that realises an event.
fn event_slot(event: &ExportOnceEvent) -> Option<ScheduleSlot> {
    let (start, end) = (event.start_at()?, event.end_at()?);
    Some(ScheduleSlot {
        enabled: true,
        start_hour: start.hour() as u8,
        start_minute: start.minute() as u8,
        end_hour: end.hour() as u8,
        end_minute: end.minute() as u8,
        target_soc: event.target_soc,
    })
}

/// Body for the Timed Export slot-save handler.
fn slot_body(slot_number: u8, slot: &ScheduleSlot) -> Value {
    json!({
        "slot": slot_number,
        "enabled": slot.enabled,
        "start_hour": slot.start_hour,
        "start_minute": slot.start_minute,
        "end_hour": slot.end_hour,
        "end_minute": slot.end_minute,
        "target_soc": slot.target_soc.clamp(4, 100),
    })
}

/// Whether two slots would behave identically. A disabled slot's leftover
/// times are irrelevant.
fn same_content(a: &ScheduleSlot, b: &ScheduleSlot) -> bool {
    if !a.enabled || !b.enabled {
        return a.enabled == b.enabled;
    }
    a.start_hour == b.start_hour
        && a.start_minute == b.start_minute
        && a.end_hour == b.end_hour
        && a.end_minute == b.end_minute
        && a.target_soc == b.target_soc
}

/// The user's desired content for a slot (0-based `index`), using the same
/// source the slot-save handler merges into: the persisted schedule, or the
/// live registers when none is persisted yet.
async fn desired_slot_at(state: &Arc<AppState>, index: usize) -> ScheduleSlot {
    let persisted = state.timed_export_config.lock().await.slots.clone();
    let base = if persisted.iter().any(ScheduleSlot::is_configured) {
        persisted
    } else {
        state
            .latest_snapshot
            .lock()
            .await
            .as_ref()
            .map(|snapshot| snapshot.discharge_slots.to_vec())
            .unwrap_or_default()
    };
    base.get(index).cloned().unwrap_or_else(disabled_slot)
}

/// The inverter's local clock, falling back to the host's.
pub async fn inverter_now(state: &Arc<AppState>) -> NaiveDateTime {
    let reported = state
        .latest_snapshot
        .lock()
        .await
        .as_ref()
        .map(|snapshot| snapshot.inverter_time.clone())
        .unwrap_or_default();
    NaiveDateTime::parse_from_str(&reported, "%Y-%m-%d %H:%M:%S")
        .unwrap_or_else(|_| chrono::Local::now().naive_local())
}

/// The slot content a just-finished event on the same slot put back, when
/// `current` still shows that event's own window.
///
/// With nothing persisted for the slot, `desired_slot_at` reads the live
/// registers, and right after a revert the poll loop may not have read the
/// restored slot back yet. Capturing that stale read would make the next
/// back-to-back event restore the previous event's window as an enabled daily
/// slot. The match is limited to events that ended in the last few minutes so
/// a slot the user set later is never mistaken for it.
fn restored_by_recent_event(
    events: &[ExportOnceEvent],
    this: &ExportOnceEvent,
    current: &ScheduleSlot,
    now: NaiveDateTime,
) -> Option<ScheduleSlot> {
    const STALE_READ_MINUTES: i64 = 10;
    if !current.enabled {
        return None;
    }
    events
        .iter()
        .filter(|other| {
            other.id != this.id
                && other.slot == this.slot
                && other.applied
                && matches!(
                    other.state,
                    ExportOnceState::Done | ExportOnceState::Cancelled | ExportOnceState::Failed
                )
        })
        .filter(|other| {
            other
                .end_at()
                .is_some_and(|end| end > now - Duration::minutes(STALE_READ_MINUTES))
        })
        .find(|other| {
            event_slot(other).is_some_and(|window| {
                window.start_hour == current.start_hour
                    && window.start_minute == current.start_minute
                    && window.end_hour == current.end_hour
                    && window.end_minute == current.end_minute
            })
        })
        .and_then(|other| {
            other
                .original
                .as_ref()
                .map(|original| original.slot.clone())
        })
}

/// Every stored event, oldest first.
pub async fn list_events() -> Vec<ExportOnceEvent> {
    Settings::load_async().await.export_once_events
}

async fn load_event(id: &str) -> Option<ExportOnceEvent> {
    list_events().await.into_iter().find(|event| event.id == id)
}

async fn update_event<F>(id: &str, change: F)
where
    F: FnOnce(&mut ExportOnceEvent) + Send + 'static,
{
    let id = id.to_string();
    let result = Settings::update_async(move |settings| {
        if let Some(event) = settings.export_once_events.iter_mut().find(|e| e.id == id) {
            change(event);
        }
    })
    .await;
    if let Err(error) = result {
        tracing::error!("Export Once: could not save event state: {error}");
    }
}

/// Keep every live event and only the most recent finished ones.
fn prune_finished(events: &mut Vec<ExportOnceEvent>) {
    let mut finished_seen = 0;
    let mut keep: Vec<bool> = events
        .iter()
        .rev()
        .map(|event| {
            if event.state.is_live() {
                true
            } else {
                finished_seen += 1;
                finished_seen <= KEEP_FINISHED
            }
        })
        .collect();
    keep.reverse();
    let mut flags = keep.into_iter();
    events.retain(|_| flags.next().unwrap_or(true));
}

async fn finish(id: &str, state: ExportOnceState, note: Option<String>) {
    update_event(id, move |event| {
        event.state = state;
        if note.is_some() {
            event.note = note;
        }
    })
    .await;
    let _ =
        Settings::update_async(|settings| prune_finished(&mut settings.export_once_events)).await;
}

/// Exports booked for Octopus Saving Sessions carry this `source` prefix.
const SESSION_SOURCE_PREFIX: &str = "saving_session:";

/// Tell the user how a Saving Session export is going. Exports the user
/// booked by hand are theirs to watch and send nothing. The send is detached
/// so a slow alert channel never holds up the inverter work.
fn notify_session_export(
    state: &Arc<AppState>,
    event: &ExportOnceEvent,
    headline: &str,
    detail: String,
) {
    if !event.source.starts_with(SESSION_SOURCE_PREFIX) {
        return;
    }
    let state = state.clone();
    let text = format!("⚡ HEM — {headline}\n{detail}");
    tokio::spawn(async move {
        let _ = crate::alerts::send_text_notification(&state, &text, "saving-session-export").await;
    });
}

fn response_error(body: &Value) -> String {
    body["error"]
        .as_str()
        .unwrap_or("the inverter rejected the change")
        .to_string()
}

/// Book a new event. Validated against the events already stored, inside
/// the settings transaction, so two bookings cannot both pass the overlap
/// check.
pub async fn create_event(
    state: &Arc<AppState>,
    request: &NewEventRequest,
    source: &str,
) -> Result<ExportOnceEvent, String> {
    let now = inverter_now(state).await;
    let max_slots = state
        .latest_snapshot
        .lock()
        .await
        .as_ref()
        .map_or(DeviceType::Gen2Hybrid, |snapshot| snapshot.device_type)
        .max_discharge_slots();
    let request = request.clone();
    let source = source.to_string();
    tokio::task::spawn_blocking(move || {
        Settings::try_update(|settings| {
            let mut id = format!("eo-{}", chrono::Utc::now().timestamp_millis());
            let mut suffix = 1;
            while settings.export_once_events.iter().any(|e| e.id == id) {
                id = format!("eo-{}-{suffix}", chrono::Utc::now().timestamp_millis());
                suffix += 1;
            }
            let event = validate_new(
                &request,
                &settings.export_once_events,
                now,
                max_slots,
                &source,
                id,
            )?;
            settings.export_once_events.push(event.clone());
            prune_finished(&mut settings.export_once_events);
            Ok(event)
        })
    })
    .await
    .map_err(|error| format!("Saving the export failed: {error}"))??
}

/// Cancel an event. A pending one is marked cancelled; an active one is
/// reverted first.
pub async fn cancel_event(state: &Arc<AppState>, id: &str) -> Result<(), String> {
    let _guard = state.timed_export_action_lock.lock().await;
    let event = load_event(id)
        .await
        .ok_or_else(|| "No such export".to_string())?;
    match event.state {
        ExportOnceState::Pending => {
            finish(id, ExportOnceState::Cancelled, None).await;
            Ok(())
        }
        ExportOnceState::Active => revert_locked(state, id, true).await,
        _ => Err("That export has already finished".to_string()),
    }
}

/// Whether a recent failure means this event should wait before another try.
async fn backed_off(state: &Arc<AppState>, id: &str, now: NaiveDateTime) -> bool {
    state
        .export_once_backoff
        .lock()
        .await
        .get(id)
        .is_some_and(|last| now < *last + Duration::minutes(RETRY_SPACING_MINUTES))
}

async fn note_attempt(state: &Arc<AppState>, id: &str, now: NaiveDateTime, succeeded: bool) {
    let mut backoff = state.export_once_backoff.lock().await;
    if succeeded {
        backoff.remove(id);
    } else {
        backoff.insert(id.to_string(), now);
    }
}

/// Write an event's window into its slot, capturing the slot's previous
/// content first. Runs under the action lock so the Timed Export machine
/// never sees a half-applied slot. Returns false when the inverter refused
/// the change and another attempt is due.
async fn apply_event(state: &Arc<AppState>, id: &str) -> bool {
    let _guard = state.timed_export_action_lock.lock().await;
    let Some(event) = load_event(id).await else {
        return true;
    };
    let Some(window) = event_slot(&event) else {
        let note = "The export's times could not be read".to_string();
        finish(id, ExportOnceState::Failed, Some(note.clone())).await;
        notify_session_export(state, &event, "Saving Session export did not run", note);
        return true;
    };
    match event.state {
        ExportOnceState::Pending => {
            let current = desired_slot_at(state, usize::from(event.slot) - 1).await;
            let now = inverter_now(state).await;
            let restored = restored_by_recent_event(&list_events().await, &event, &current, now);
            let original = ExportOnceOriginal {
                slot: restored.unwrap_or(current),
                schedule_enabled: state.timed_export_config.lock().await.schedule_enabled,
            };
            // Persisted before the slot is touched: a crash mid-apply must
            // still know what to put back.
            update_event(id, move |e| {
                e.state = ExportOnceState::Active;
                e.original = Some(original);
                e.attempts = 1;
            })
            .await;
        }
        ExportOnceState::Active if !event.applied => {
            update_event(id, |e| e.attempts = e.attempts.saturating_add(1)).await;
        }
        _ => return true,
    }
    tracing::info!(
        "Export Once: applying {} to slot {} ({} - {})",
        event.id,
        event.slot,
        event.start,
        event.end
    );
    let (status, body) =
        set_discharge_slot_locked(state.clone(), slot_body(event.slot, &window)).await;
    if status == StatusCode::OK {
        update_event(id, |e| {
            e.applied = true;
            e.note = None;
        })
        .await;
        notify_session_export(
            state,
            &event,
            "Saving Session export started",
            format!(
                "Slot {} is exporting until {}. It goes back to its previous setting afterwards.",
                event.slot,
                event.end.get(11..16).unwrap_or("the end of the session")
            ),
        );
    } else {
        let reason = response_error(&body.0);
        tracing::warn!("Export Once: applying {} failed: {reason}", event.id);
        update_event(id, move |e| e.note = Some(reason)).await;
    }
    status == StatusCode::OK
}

/// Put an active event's slot back and finish the event.
async fn revert_locked(state: &Arc<AppState>, id: &str, cancelling: bool) -> Result<(), String> {
    let Some(event) = load_event(id).await else {
        return Err("No such export".to_string());
    };
    if event.state != ExportOnceState::Active {
        return Ok(());
    }
    let Some(original) = event.original.clone() else {
        let note = "The slot's previous setting was never recorded".to_string();
        finish(id, ExportOnceState::Failed, Some(note.clone())).await;
        notify_session_export(state, &event, "Saving Session export did not run", note);
        return Ok(());
    };
    let current = desired_slot_at(state, usize::from(event.slot) - 1).await;
    let at_original = same_content(&current, &original.slot);
    if event.applied && !at_original && !slot_matches_event(&current, &event) {
        // Neither the event's window nor what was there before: the user
        // changed the slot. Leave their setting alone.
        finish(
            id,
            ExportOnceState::Done,
            Some("The slot was edited during the export, so it was left as you set it".to_string()),
        )
        .await;
        return Ok(());
    }
    let schedule_enabled_before = state.timed_export_config.lock().await.schedule_enabled;
    if !at_original {
        tracing::info!(
            "Export Once: restoring slot {} after {}",
            event.slot,
            event.id
        );
        let (status, body) =
            set_discharge_slot_locked(state.clone(), slot_body(event.slot, &original.slot)).await;
        if status != StatusCode::OK {
            let reason = response_error(&body.0);
            tracing::warn!(
                "Export Once: restoring slot {} failed: {reason}",
                event.slot
            );
            update_event(id, {
                let reason = reason.clone();
                move |e| e.note = Some(format!("Restoring the slot failed: {reason}"))
            })
            .await;
            return Err(reason);
        }
    }
    // Restoring an enabled slot re-arms the schedule. Put the master flag
    // back to what it was, unless the user had already stopped export
    // during the event. (If this stop fails and the revert is retried, the
    // user's mid-event stop can no longer be told apart, so the schedule
    // stays as it was before the event — acceptable for that corner.)
    let want_enabled = original.schedule_enabled && schedule_enabled_before;
    let enabled_now = state.timed_export_config.lock().await.schedule_enabled;
    if enabled_now && !want_enabled {
        let (status, body) = set_timed_export_locked(state.clone(), false, None).await;
        if status != StatusCode::OK {
            let reason = response_error(&body.0);
            tracing::warn!(
                "Export Once: stopping export after {} failed: {reason}",
                event.id
            );
            update_event(id, {
                let reason = reason.clone();
                move |e| e.note = Some(format!("Stopping export failed: {reason}"))
            })
            .await;
            return Err(reason);
        }
    }
    if cancelling {
        finish(id, ExportOnceState::Cancelled, None).await;
    } else if event.applied {
        finish(id, ExportOnceState::Done, None).await;
        notify_session_export(
            state,
            &event,
            "Saving Session export finished",
            format!("Slot {} is back to how it was.", event.slot),
        );
    } else {
        let note = event
            .note
            .clone()
            .map(|reason| format!("Could not be applied: {reason}"))
            .unwrap_or_else(|| "Could not be applied".to_string());
        finish(id, ExportOnceState::Failed, Some(note.clone())).await;
        notify_session_export(state, &event, "Saving Session export did not run", note);
    }
    Ok(())
}

/// Drop an applied event whose slot the user has since edited, so the
/// scheduled revert cannot overwrite their change.
/// Returns whether any event was released.
async fn release_edited_events(
    state: &Arc<AppState>,
    now: NaiveDateTime,
    events: &[ExportOnceEvent],
) -> bool {
    let mut released = false;
    for event in events {
        let still_running = event.state == ExportOnceState::Active
            && event.applied
            && event.end_at().is_some_and(|end| end > now);
        if !still_running {
            continue;
        }
        let _guard = state.timed_export_action_lock.lock().await;
        let current = desired_slot_at(state, usize::from(event.slot) - 1).await;
        if !slot_matches_event(&current, event) {
            tracing::info!(
                "Export Once: slot {} was edited during {}; leaving it as set",
                event.slot,
                event.id
            );
            finish(
                &event.id,
                ExportOnceState::Done,
                Some(
                    "The slot was edited during the export, so it was left as you set it"
                        .to_string(),
                ),
            )
            .await;
            released = true;
        }
    }
    released
}

/// Run one pass: apply, revert and expire whatever is due.
pub async fn tick(state: &Arc<AppState>) {
    if state.latest_snapshot.lock().await.is_none()
        || *state.connection_state.lock().await != ConnectionState::Connected
    {
        return;
    }
    // Most households never book an export: do nothing, and read the settings
    // file once, rather than re-reading it for every step below.
    let events = list_events().await;
    if !events.iter().any(|event| event.state.is_live()) {
        return;
    }
    let now = inverter_now(state).await;
    let events = if release_edited_events(state, now, &events).await {
        list_events().await
    } else {
        events
    };
    for action in plan(&events, now) {
        match action {
            PlannedAction::Apply(id) => {
                if backed_off(state, &id, now).await {
                    continue;
                }
                let applied = apply_event(state, &id).await;
                note_attempt(state, &id, now, applied).await;
            }
            PlannedAction::Revert(id) => {
                // A revert that keeps failing must keep being tried (the slot
                // must not be left holding the window), but not every tick:
                // each attempt holds the Timed Export action lock.
                if backed_off(state, &id, now).await {
                    continue;
                }
                let reverted = {
                    let _guard = state.timed_export_action_lock.lock().await;
                    revert_locked(state, &id, false).await.is_ok()
                };
                note_attempt(state, &id, now, reverted).await;
            }
            PlannedAction::Expire(id) => {
                let note = "Missed: HEM did not apply this export before its window ended (the app or the inverter connection may have been down)".to_string();
                let event = load_event(&id).await;
                finish(&id, ExportOnceState::Failed, Some(note.clone())).await;
                if let Some(event) = event {
                    notify_session_export(state, &event, "Saving Session export did not run", note);
                }
            }
        }
    }
}

/// Background loop. Runs as its own task, not inside the poll loop: the
/// slot-save handlers wait for the poll loop to drain their writes, so
/// calling them from the poll loop would wait on itself.
pub async fn run_export_once_loop(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(TICK_SECS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        tick(&state).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> NaiveDateTime {
        parse_local(value).expect("test instant")
    }

    fn event(id: &str, slot: u8, start: &str, end: &str) -> ExportOnceEvent {
        ExportOnceEvent {
            id: id.to_string(),
            slot,
            start: start.to_string(),
            end: end.to_string(),
            target_soc: 20,
            source: SOURCE_MANUAL.to_string(),
            state: ExportOnceState::Pending,
            original: None,
            applied: false,
            attempts: 0,
            note: None,
        }
    }

    fn active(id: &str, slot: u8, start: &str, end: &str, applied: bool) -> ExportOnceEvent {
        ExportOnceEvent {
            state: ExportOnceState::Active,
            original: Some(ExportOnceOriginal {
                slot: ScheduleSlot {
                    enabled: false,
                    start_hour: 0,
                    start_minute: 0,
                    end_hour: 0,
                    end_minute: 0,
                    target_soc: 4,
                },
                schedule_enabled: false,
            }),
            applied,
            ..event(id, slot, start, end)
        }
    }

    fn request(slot: u8, date: &str, start: &str, end: &str, soc: u8) -> NewEventRequest {
        NewEventRequest {
            slot,
            date: date.to_string(),
            start: start.to_string(),
            end: end.to_string(),
            target_soc: soc,
        }
    }

    const NOW: &str = "2026-10-17 12:00";

    // ---- planner ----

    #[test]
    fn pending_event_far_in_the_future_is_left_alone() {
        let events = [event("a", 1, "2026-10-17 18:00", "2026-10-17 19:00")];
        assert!(plan(&events, at(NOW)).is_empty());
    }

    #[test]
    fn pending_event_is_applied_once_inside_the_lead() {
        let events = [event("a", 1, "2026-10-17 18:00", "2026-10-17 19:00")];
        assert!(plan(&events, at("2026-10-17 17:49")).is_empty());
        assert_eq!(
            plan(&events, at("2026-10-17 17:50")),
            vec![PlannedAction::Apply("a".to_string())]
        );
    }

    #[test]
    fn pending_event_already_in_its_window_is_applied() {
        let events = [event("a", 1, "2026-10-17 18:00", "2026-10-17 19:00")];
        assert_eq!(
            plan(&events, at("2026-10-17 18:30")),
            vec![PlannedAction::Apply("a".to_string())]
        );
    }

    #[test]
    fn pending_event_that_ended_unapplied_expires() {
        let events = [event("a", 1, "2026-10-17 18:00", "2026-10-17 19:00")];
        assert_eq!(
            plan(&events, at("2026-10-17 19:00")),
            vec![PlannedAction::Expire("a".to_string())]
        );
    }

    #[test]
    fn applied_active_event_waits_until_its_end_then_reverts() {
        let events = [active("a", 1, "2026-10-17 18:00", "2026-10-17 19:00", true)];
        assert!(plan(&events, at("2026-10-17 18:59")).is_empty());
        assert_eq!(
            plan(&events, at("2026-10-17 19:00")),
            vec![PlannedAction::Revert("a".to_string())]
        );
    }

    #[test]
    fn active_event_whose_apply_failed_is_retried_inside_its_window() {
        let mut ev = active("a", 1, "2026-10-17 18:00", "2026-10-17 19:00", false);
        ev.attempts = 1;
        assert_eq!(
            plan(&[ev], at("2026-10-17 18:10")),
            vec![PlannedAction::Apply("a".to_string())]
        );
    }

    #[test]
    fn active_event_that_never_applied_gives_up_after_the_attempt_limit() {
        let mut ev = active("a", 1, "2026-10-17 18:00", "2026-10-17 19:00", false);
        ev.attempts = MAX_APPLY_ATTEMPTS;
        assert_eq!(
            plan(&[ev], at("2026-10-17 18:10")),
            vec![PlannedAction::Revert("a".to_string())]
        );
    }

    #[test]
    fn active_event_past_its_end_reverts_even_if_it_never_applied() {
        let ev = active("a", 1, "2026-10-17 18:00", "2026-10-17 19:00", false);
        assert_eq!(
            plan(&[ev], at("2026-10-17 19:05")),
            vec![PlannedAction::Revert("a".to_string())]
        );
    }

    #[test]
    fn finished_events_are_ignored() {
        for state in [
            ExportOnceState::Done,
            ExportOnceState::Cancelled,
            ExportOnceState::Failed,
        ] {
            let mut ev = event("a", 1, "2026-10-17 11:00", "2026-10-17 13:00");
            ev.state = state;
            assert!(plan(&[ev], at(NOW)).is_empty(), "{state:?}");
        }
    }

    #[test]
    fn a_pending_event_waits_for_the_active_event_on_its_slot() {
        let events = [
            active("a", 1, "2026-10-17 18:00", "2026-10-17 19:00", true),
            event("b", 1, "2026-10-17 19:00", "2026-10-17 20:00"),
        ];
        assert!(plan(&events, at("2026-10-17 18:55")).is_empty());
        // Back to back: A reverts first; B applies on a later tick.
        assert_eq!(
            plan(&events, at("2026-10-17 19:00")),
            vec![PlannedAction::Revert("a".to_string())]
        );
    }

    #[test]
    fn events_on_different_slots_are_planned_independently() {
        let events = [
            active("a", 1, "2026-10-17 18:00", "2026-10-17 19:00", true),
            event("b", 2, "2026-10-17 18:00", "2026-10-17 19:00"),
        ];
        assert_eq!(
            plan(&events, at("2026-10-17 18:00")),
            vec![PlannedAction::Apply("b".to_string())]
        );
    }

    #[test]
    fn an_unparseable_event_is_expired_not_applied() {
        let ev = event("a", 1, "garbage", "2026-10-17 19:00");
        assert_eq!(
            plan(&[ev], at(NOW)),
            vec![PlannedAction::Expire("a".to_string())]
        );
    }

    // ---- validation ----

    fn build(
        req: &NewEventRequest,
        existing: &[ExportOnceEvent],
    ) -> Result<ExportOnceEvent, String> {
        validate_new(req, existing, at(NOW), 2, SOURCE_MANUAL, "new".to_string())
    }

    #[test]
    fn a_valid_booking_becomes_a_pending_event() {
        let ev = build(&request(2, "2026-10-17", "18:00", "19:00", 25), &[]).unwrap();
        assert_eq!(ev.id, "new");
        assert_eq!(ev.slot, 2);
        assert_eq!(ev.start, "2026-10-17 18:00");
        assert_eq!(ev.end, "2026-10-17 19:00");
        assert_eq!(ev.target_soc, 25);
        assert_eq!(ev.source, SOURCE_MANUAL);
        assert_eq!(ev.state, ExportOnceState::Pending);
        assert!(!ev.applied);
        assert!(ev.original.is_none());
    }

    #[test]
    fn a_booking_must_name_a_slot_the_model_has() {
        assert!(build(&request(0, "2026-10-17", "18:00", "19:00", 25), &[]).is_err());
        assert!(build(&request(3, "2026-10-17", "18:00", "19:00", 25), &[]).is_err());
    }

    #[test]
    fn a_booking_must_end_after_it_starts() {
        assert!(build(&request(1, "2026-10-17", "19:00", "19:00", 25), &[]).is_err());
        assert!(build(&request(1, "2026-10-17", "19:00", "18:00", 25), &[]).is_err());
    }

    #[test]
    fn a_booking_that_has_already_ended_is_rejected() {
        assert!(build(&request(1, "2026-10-17", "10:00", "11:00", 25), &[]).is_err());
        assert!(build(&request(1, "2026-10-16", "18:00", "19:00", 25), &[]).is_err());
    }

    #[test]
    fn a_booking_that_is_under_way_is_accepted() {
        assert!(build(&request(1, "2026-10-17", "11:30", "13:00", 25), &[]).is_ok());
    }

    #[test]
    fn a_booking_too_far_ahead_is_rejected() {
        assert!(build(&request(1, "2026-10-31", "18:00", "19:00", 25), &[]).is_ok());
        assert!(build(&request(1, "2026-11-01", "18:00", "19:00", 25), &[]).is_err());
    }

    #[test]
    fn the_discharge_floor_must_be_in_range() {
        assert!(build(&request(1, "2026-10-17", "18:00", "19:00", 3), &[]).is_err());
        assert!(build(&request(1, "2026-10-17", "18:00", "19:00", 100), &[]).is_err());
        assert!(build(&request(1, "2026-10-17", "18:00", "19:00", 4), &[]).is_ok());
        assert!(build(&request(1, "2026-10-17", "18:00", "19:00", 99), &[]).is_ok());
    }

    #[test]
    fn malformed_dates_and_times_are_rejected() {
        assert!(build(&request(1, "17/10/2026", "18:00", "19:00", 25), &[]).is_err());
        assert!(build(&request(1, "2026-10-17", "6pm", "19:00", 25), &[]).is_err());
        assert!(build(&request(1, "2026-10-17", "18:00", "25:00", 25), &[]).is_err());
    }

    #[test]
    fn overlapping_a_live_event_on_the_same_slot_is_rejected() {
        let existing = [event("a", 1, "2026-10-17 18:00", "2026-10-17 19:00")];
        assert!(build(&request(1, "2026-10-17", "18:30", "19:30", 25), &existing).is_err());
        assert!(build(&request(1, "2026-10-17", "17:00", "18:01", 25), &existing).is_err());
    }

    #[test]
    fn touching_or_other_slot_events_do_not_conflict() {
        let existing = [event("a", 1, "2026-10-17 18:00", "2026-10-17 19:00")];
        assert!(build(&request(1, "2026-10-17", "19:00", "20:00", 25), &existing).is_ok());
        assert!(build(&request(2, "2026-10-17", "18:30", "19:30", 25), &existing).is_ok());
    }

    #[test]
    fn finished_events_do_not_block_a_rebooking() {
        let mut done = event("a", 1, "2026-10-17 18:00", "2026-10-17 19:00");
        done.state = ExportOnceState::Done;
        assert!(build(&request(1, "2026-10-17", "18:00", "19:00", 25), &[done]).is_ok());
    }

    // ---- user-edit detection ----

    fn slot(h1: u8, m1: u8, h2: u8, m2: u8, enabled: bool) -> ScheduleSlot {
        ScheduleSlot {
            enabled,
            start_hour: h1,
            start_minute: m1,
            end_hour: h2,
            end_minute: m2,
            target_soc: 20,
        }
    }

    #[test]
    fn a_slot_holding_the_event_window_matches() {
        let ev = event("a", 1, "2026-10-17 18:00", "2026-10-17 19:30");
        assert!(slot_matches_event(&slot(18, 0, 19, 30, true), &ev));
    }

    #[test]
    fn an_edited_or_disabled_slot_no_longer_matches() {
        let ev = event("a", 1, "2026-10-17 18:00", "2026-10-17 19:30");
        assert!(!slot_matches_event(&slot(18, 0, 20, 0, true), &ev));
        assert!(!slot_matches_event(&slot(17, 0, 19, 30, true), &ev));
        assert!(!slot_matches_event(&slot(18, 0, 19, 30, false), &ev));
    }

    // ---- executor (real slot-save / stop handlers, write queue acked) ----

    use crate::inverter::encoder::WriteOutcome;
    use crate::inverter::model::{DeviceType, InverterSnapshot};
    use crate::test_util::with_isolated_config_dir_async;

    fn sched(h1: u8, m1: u8, h2: u8, m2: u8, soc: u8) -> ScheduleSlot {
        ScheduleSlot {
            enabled: true,
            start_hour: h1,
            start_minute: m1,
            end_hour: h2,
            end_minute: m2,
            target_soc: soc,
        }
    }

    async fn make_state() -> Arc<AppState> {
        let state = Arc::new(AppState::new());
        let snapshot = InverterSnapshot {
            device_type: DeviceType::Gen3Hybrid,
            inverter_serial: "HEM-TEST-001".to_string(),
            inverter_time: "2026-10-17 12:00:00".to_string(),
            ..Default::default()
        };
        *state.latest_snapshot.lock().await = Some(snapshot);
        *state.connection_state.lock().await = crate::inverter::poll::ConnectionState::Connected;
        state
    }

    async fn set_clock(state: &Arc<AppState>, local: &str) {
        let mut snapshot = state.latest_snapshot.lock().await;
        snapshot.as_mut().unwrap().inverter_time = format!("{local}:00");
    }

    /// Seed the user's desired Timed Export schedule (mirror + disk).
    async fn seed_schedule(state: &Arc<AppState>, enabled: bool, slots: Vec<ScheduleSlot>) {
        {
            let mut config = state.timed_export_config.lock().await;
            config.schedule_enabled = enabled;
            config.slots = slots.clone();
        }
        crate::settings::Settings::update(|s| {
            s.timed_export_schedule_enabled = enabled;
            s.timed_export_slots = slots;
        })
        .unwrap();
    }

    fn store(events: Vec<ExportOnceEvent>) {
        crate::settings::Settings::update(|s| s.export_once_events = events).unwrap();
    }

    fn stored(id: &str) -> ExportOnceEvent {
        crate::settings::Settings::load()
            .export_once_events
            .into_iter()
            .find(|e| e.id == id)
            .expect("event stored")
    }

    /// Run `fut` to completion while acking every completion-backed write
    /// batch with `outcome` — standing in for the poll loop.
    async fn drive<F>(state: &Arc<AppState>, fut: F, outcome: WriteOutcome) -> F::Output
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let handle = tokio::spawn(fut);
        while !handle.is_finished() {
            {
                let mut pending = state.pending_writes.lock().await;
                for batch in pending.iter_mut() {
                    if let Some(tx) = batch.completion.take() {
                        let _ = tx.send(outcome.clone());
                    }
                }
            }
            tokio::task::yield_now().await;
        }
        handle.await.unwrap()
    }

    async fn run_tick(state: &Arc<AppState>) {
        let s = state.clone();
        drive(state, async move { tick(&s).await }, WriteOutcome::Ok).await;
    }

    async fn desired_slot(state: &Arc<AppState>, index: usize) -> ScheduleSlot {
        state.timed_export_config.lock().await.slots[index].clone()
    }

    #[tokio::test]
    async fn tick_applies_the_event_into_the_chosen_slot_and_captures_the_original() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            seed_schedule(
                &state,
                true,
                vec![sched(6, 0, 7, 0, 40), sched(22, 0, 23, 0, 30)],
            )
            .await;
            store(vec![event("e1", 2, "2026-10-17 18:00", "2026-10-17 19:00")]);

            set_clock(&state, "2026-10-17 12:00").await;
            run_tick(&state).await;
            assert_eq!(stored("e1").state, ExportOnceState::Pending, "too early");

            set_clock(&state, "2026-10-17 17:55").await;
            run_tick(&state).await;

            let ev = stored("e1");
            assert_eq!(ev.state, ExportOnceState::Active);
            assert!(ev.applied);
            let original = ev.original.expect("original captured");
            assert!(original.schedule_enabled);
            assert_eq!(original.slot.start_hour, 22);
            assert_eq!(original.slot.target_soc, 30);

            let slot = desired_slot(&state, 1).await;
            assert!(slot_matches_event(&slot, &stored("e1")));
            assert_eq!(slot.target_soc, 20);
            // The other slot is untouched.
            assert_eq!(desired_slot(&state, 0).await.start_hour, 6);
        })
        .await;
    }

    #[tokio::test]
    async fn tick_restores_the_original_slot_when_the_event_ends() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            seed_schedule(
                &state,
                true,
                vec![sched(6, 0, 7, 0, 40), sched(22, 0, 23, 0, 30)],
            )
            .await;
            store(vec![event("e1", 2, "2026-10-17 18:00", "2026-10-17 19:00")]);
            set_clock(&state, "2026-10-17 17:55").await;
            run_tick(&state).await;

            set_clock(&state, "2026-10-17 18:30").await;
            run_tick(&state).await;
            assert_eq!(stored("e1").state, ExportOnceState::Active, "still running");

            set_clock(&state, "2026-10-17 19:00").await;
            run_tick(&state).await;

            assert_eq!(stored("e1").state, ExportOnceState::Done);
            let slot = desired_slot(&state, 1).await;
            assert!(slot.enabled);
            assert_eq!(
                (slot.start_hour, slot.end_hour, slot.target_soc),
                (22, 23, 30)
            );
            assert!(state.timed_export_config.lock().await.schedule_enabled);
            let on_disk = crate::settings::Settings::load();
            assert_eq!(on_disk.timed_export_slots[1].start_hour, 22);
        })
        .await;
    }

    #[tokio::test]
    async fn reverting_clears_the_slot_and_stops_export_when_nothing_was_there_before() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            seed_schedule(&state, false, vec![]).await;
            store(vec![event("e1", 1, "2026-10-17 18:00", "2026-10-17 19:00")]);
            set_clock(&state, "2026-10-17 17:55").await;
            run_tick(&state).await;
            assert!(state.timed_export_config.lock().await.schedule_enabled);

            set_clock(&state, "2026-10-17 19:01").await;
            run_tick(&state).await;

            assert_eq!(stored("e1").state, ExportOnceState::Done);
            let config = state.timed_export_config.lock().await.clone();
            assert!(!config.schedule_enabled, "export must not repeat tomorrow");
            assert!(
                !config.slots.iter().any(ScheduleSlot::is_configured),
                "the event window must not be left in the slot"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn reverting_stops_export_when_the_schedule_was_off_but_the_slot_was_set() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            seed_schedule(&state, false, vec![sched(22, 0, 23, 0, 30)]).await;
            store(vec![event("e1", 1, "2026-10-17 18:00", "2026-10-17 19:00")]);
            set_clock(&state, "2026-10-17 17:55").await;
            run_tick(&state).await;
            set_clock(&state, "2026-10-17 19:01").await;
            run_tick(&state).await;

            assert_eq!(stored("e1").state, ExportOnceState::Done);
            let config = state.timed_export_config.lock().await.clone();
            assert!(!config.schedule_enabled);
            assert_eq!(config.slots[0].start_hour, 22, "their saved slot is back");
        })
        .await;
    }

    #[tokio::test]
    async fn a_user_edit_of_the_slot_during_the_event_wins() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            seed_schedule(&state, true, vec![sched(22, 0, 23, 0, 30)]).await;
            store(vec![event("e1", 1, "2026-10-17 18:00", "2026-10-17 19:00")]);
            set_clock(&state, "2026-10-17 17:55").await;
            run_tick(&state).await;

            // The user saves the same slot from the Control page.
            let s = state.clone();
            let (status, _) = drive(
                &state,
                async move {
                    crate::server::api::set_discharge_slot(
                        axum::extract::State(s),
                        axum::response::Json(serde_json::json!({
                            "slot": 1, "enabled": true,
                            "start_hour": 20, "start_minute": 0,
                            "end_hour": 21, "end_minute": 0, "target_soc": 50
                        })),
                    )
                    .await
                },
                WriteOutcome::Ok,
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::OK);

            set_clock(&state, "2026-10-17 18:10").await;
            run_tick(&state).await;
            assert_eq!(stored("e1").state, ExportOnceState::Done);
            assert!(stored("e1").note.is_some());

            // And the end of the original window must not undo their edit.
            set_clock(&state, "2026-10-17 19:05").await;
            run_tick(&state).await;
            let slot = desired_slot(&state, 0).await;
            assert_eq!(
                (slot.start_hour, slot.end_hour, slot.target_soc),
                (20, 21, 50)
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_rejected_apply_is_retried_then_gives_up_and_restores_the_slot() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            seed_schedule(&state, true, vec![sched(22, 0, 23, 0, 30)]).await;
            store(vec![event("e1", 1, "2026-10-17 18:00", "2026-10-17 19:00")]);

            for attempt in 1..=MAX_APPLY_ATTEMPTS {
                // Attempts are spaced a minute apart across the lead window.
                set_clock(
                    &state,
                    &format!("2026-10-17 17:{:02}", 49 + u32::from(attempt)),
                )
                .await;
                let s = state.clone();
                drive(
                    &state,
                    async move { tick(&s).await },
                    WriteOutcome::Failed {
                        address: 56,
                        value: 0,
                        error: "boom".to_string(),
                    },
                )
                .await;
                let ev = stored("e1");
                assert_eq!(ev.state, ExportOnceState::Active);
                assert!(!ev.applied);
                assert_eq!(ev.attempts, attempt);
            }

            // Out of attempts: a minute after the last try, the next pass
            // reverts and reports failure.
            set_clock(&state, "2026-10-17 17:56").await;
            run_tick(&state).await;
            let ev = stored("e1");
            assert_eq!(ev.state, ExportOnceState::Failed);
            assert!(ev.note.is_some());
            let slot = desired_slot(&state, 0).await;
            assert_eq!((slot.start_hour, slot.end_hour), (22, 23));
        })
        .await;
    }

    /// Like `drive`, but also counts the write batches the poll loop would have
    /// had to send, so a test can tell an attempt from a skipped one.
    async fn drive_counting<F>(
        state: &Arc<AppState>,
        fut: F,
        outcome: WriteOutcome,
    ) -> (F::Output, usize)
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let handle = tokio::spawn(fut);
        let mut batches = 0;
        while !handle.is_finished() {
            {
                let mut pending = state.pending_writes.lock().await;
                for batch in pending.iter_mut() {
                    if let Some(tx) = batch.completion.take() {
                        batches += 1;
                        let _ = tx.send(outcome.clone());
                    }
                }
            }
            tokio::task::yield_now().await;
        }
        (handle.await.unwrap(), batches)
    }

    fn failing() -> WriteOutcome {
        WriteOutcome::Failed {
            address: 56,
            value: 0,
            error: "dongle busy".to_string(),
        }
    }

    #[tokio::test]
    async fn a_rejected_apply_is_not_retried_again_within_the_minute() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            seed_schedule(&state, true, vec![sched(22, 0, 23, 0, 30)]).await;
            store(vec![event("e1", 1, "2026-10-17 18:00", "2026-10-17 19:00")]);

            set_clock(&state, "2026-10-17 17:50").await;
            let s = state.clone();
            let (_, first) = drive_counting(&state, async move { tick(&s).await }, failing()).await;
            assert!(first > 0, "the first attempt must reach the inverter");
            assert_eq!(stored("e1").attempts, 1);

            // Same minute: no new attempt, so the dongle gets a breather.
            let s = state.clone();
            let (_, again) = drive_counting(&state, async move { tick(&s).await }, failing()).await;
            assert_eq!(again, 0);
            assert_eq!(stored("e1").attempts, 1);

            // A minute later it tries again.
            set_clock(&state, "2026-10-17 17:51").await;
            let s = state.clone();
            let (_, later) = drive_counting(&state, async move { tick(&s).await }, failing()).await;
            assert!(later > 0);
            assert_eq!(stored("e1").attempts, 2);
        })
        .await;
    }

    #[tokio::test]
    async fn a_brief_outage_does_not_use_up_every_apply_attempt() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            seed_schedule(&state, true, vec![sched(22, 0, 23, 0, 30)]).await;
            store(vec![event("e1", 1, "2026-10-17 18:00", "2026-10-17 19:00")]);

            // The dongle is down for the first two minutes of the lead window.
            for minute in 50..=51 {
                set_clock(&state, &format!("2026-10-17 17:{minute}")).await;
                let s = state.clone();
                drive(&state, async move { tick(&s).await }, failing()).await;
            }
            // Same tick, many times within those minutes would have burned them.
            assert_eq!(stored("e1").state, ExportOnceState::Active);

            set_clock(&state, "2026-10-17 17:52").await;
            run_tick(&state).await;
            let ev = stored("e1");
            assert!(ev.applied, "it still gets applied once the dongle is back");
            assert_eq!(ev.state, ExportOnceState::Active);
        })
        .await;
    }

    #[tokio::test]
    async fn a_failed_revert_is_retried_each_minute_not_every_tick() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            seed_schedule(&state, true, vec![sched(22, 0, 23, 0, 30)]).await;
            store(vec![event("e1", 1, "2026-10-17 18:00", "2026-10-17 19:00")]);
            set_clock(&state, "2026-10-17 18:30").await;
            run_tick(&state).await;
            assert!(stored("e1").applied);

            // The window has closed but the inverter will not take the restore.
            set_clock(&state, "2026-10-17 19:01").await;
            let s = state.clone();
            let (_, first) = drive_counting(&state, async move { tick(&s).await }, failing()).await;
            assert!(first > 0);
            assert_eq!(
                stored("e1").state,
                ExportOnceState::Active,
                "still owns the slot"
            );

            let s = state.clone();
            let (_, again) = drive_counting(&state, async move { tick(&s).await }, failing()).await;
            assert_eq!(again, 0, "no hammering within the minute");

            // It keeps trying (the slot must not be left holding the window),
            // and finishes as soon as the inverter cooperates.
            set_clock(&state, "2026-10-17 19:02").await;
            run_tick(&state).await;
            assert_eq!(stored("e1").state, ExportOnceState::Done);
            assert_eq!(desired_slot(&state, 0).await.start_hour, 22);
        })
        .await;
    }

    #[tokio::test]
    async fn an_unapplied_event_whose_window_passed_is_marked_failed_without_writes() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            seed_schedule(&state, true, vec![sched(22, 0, 23, 0, 30)]).await;
            store(vec![event("e1", 1, "2026-10-17 18:00", "2026-10-17 19:00")]);
            set_clock(&state, "2026-10-17 20:00").await;
            run_tick(&state).await;

            let ev = stored("e1");
            assert_eq!(ev.state, ExportOnceState::Failed);
            assert!(ev.note.unwrap().starts_with("Missed"));
            assert!(state.pending_writes.lock().await.is_empty());
            assert_eq!(desired_slot(&state, 0).await.start_hour, 22);
        })
        .await;
    }

    #[tokio::test]
    async fn a_restart_during_an_event_still_reverts_it() {
        with_isolated_config_dir_async(|| async {
            // Process one applies the event, then dies.
            let state = make_state().await;
            seed_schedule(&state, true, vec![sched(22, 0, 23, 0, 30)]).await;
            store(vec![event("e1", 1, "2026-10-17 18:00", "2026-10-17 19:00")]);
            set_clock(&state, "2026-10-17 18:30").await;
            run_tick(&state).await;
            assert!(stored("e1").applied);
            drop(state);

            // Process two boots from disk after the window has closed.
            let state = make_state().await;
            set_clock(&state, "2026-10-17 19:30").await;
            run_tick(&state).await;

            assert_eq!(stored("e1").state, ExportOnceState::Done);
            let slot = desired_slot(&state, 0).await;
            assert_eq!(
                (slot.start_hour, slot.end_hour, slot.target_soc),
                (22, 23, 30)
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_back_to_back_export_does_not_take_the_previous_window_as_the_original() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            seed_schedule(&state, false, vec![]).await;
            store(vec![
                event("a", 1, "2026-10-17 18:00", "2026-10-17 19:00"),
                event("b", 1, "2026-10-17 19:00", "2026-10-17 20:00"),
            ]);

            set_clock(&state, "2026-10-17 17:55").await;
            run_tick(&state).await;
            set_clock(&state, "2026-10-17 19:01").await;
            run_tick(&state).await;
            assert_eq!(stored("a").state, ExportOnceState::Done);

            // The poll loop has not read the restored slot back yet: the live
            // registers still show A's window.
            {
                let mut snapshot = state.latest_snapshot.lock().await;
                snapshot.as_mut().unwrap().discharge_slots[0] = ScheduleSlot {
                    enabled: true,
                    start_hour: 18,
                    start_minute: 0,
                    end_hour: 19,
                    end_minute: 0,
                    target_soc: 20,
                };
            }
            set_clock(&state, "2026-10-17 19:02").await;
            run_tick(&state).await;
            let original = stored("b").original.expect("B captured an original");
            assert!(
                !original.slot.enabled,
                "B must restore an empty slot, not A's window: {:?}",
                original.slot
            );

            // So when B ends nothing is left to repeat tomorrow.
            set_clock(&state, "2026-10-17 20:01").await;
            run_tick(&state).await;
            assert_eq!(stored("b").state, ExportOnceState::Done);
            let config = state.timed_export_config.lock().await.clone();
            assert!(!config.schedule_enabled);
            assert!(!config.slots.iter().any(ScheduleSlot::is_configured));
        })
        .await;
    }

    #[tokio::test]
    async fn cancelling_an_active_event_reverts_it_immediately() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            seed_schedule(&state, true, vec![sched(22, 0, 23, 0, 30)]).await;
            store(vec![event("e1", 1, "2026-10-17 18:00", "2026-10-17 19:00")]);
            set_clock(&state, "2026-10-17 18:30").await;
            run_tick(&state).await;

            let s = state.clone();
            let result = drive(
                &state,
                async move { cancel_event(&s, "e1").await },
                WriteOutcome::Ok,
            )
            .await;
            assert!(result.is_ok());
            assert_eq!(stored("e1").state, ExportOnceState::Cancelled);
            assert_eq!(desired_slot(&state, 0).await.start_hour, 22);
        })
        .await;
    }

    #[tokio::test]
    async fn cancelling_a_pending_event_touches_nothing() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            seed_schedule(&state, true, vec![sched(22, 0, 23, 0, 30)]).await;
            store(vec![event("e1", 1, "2026-10-17 18:00", "2026-10-17 19:00")]);
            cancel_event(&state, "e1").await.unwrap();
            assert_eq!(stored("e1").state, ExportOnceState::Cancelled);
            assert!(state.pending_writes.lock().await.is_empty());
            assert!(
                cancel_event(&state, "e1").await.is_err(),
                "already finished"
            );
            assert!(cancel_event(&state, "nope").await.is_err(), "unknown id");
        })
        .await;
    }

    #[tokio::test]
    async fn create_event_validates_and_stores_against_the_inverter_clock() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            let ok = create_event(
                &state,
                &request(2, "2026-10-17", "18:00", "19:00", 25),
                SOURCE_MANUAL,
            )
            .await
            .unwrap();
            assert_eq!(list_events().await.len(), 1);
            assert_eq!(list_events().await[0].id, ok.id);

            // Overlap on the same slot is rejected and not stored.
            assert!(create_event(
                &state,
                &request(2, "2026-10-17", "18:30", "19:30", 25),
                SOURCE_MANUAL
            )
            .await
            .is_err());
            // A slot the model lacks (Gen3 hybrid has 10, so use 11).
            assert!(create_event(
                &state,
                &request(11, "2026-10-17", "18:00", "19:00", 25),
                SOURCE_MANUAL
            )
            .await
            .is_err());
            assert_eq!(list_events().await.len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn applying_an_event_does_not_lose_a_concurrent_edit_to_another_slot() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            seed_schedule(
                &state,
                true,
                vec![sched(6, 0, 7, 0, 40), sched(22, 0, 23, 0, 30)],
            )
            .await;
            store(vec![event("e1", 2, "2026-10-17 18:00", "2026-10-17 19:00")]);
            set_clock(&state, "2026-10-17 17:55").await;

            let s1 = state.clone();
            let s2 = state.clone();
            let apply = tokio::spawn(async move { tick(&s1).await });
            let edit = tokio::spawn(async move {
                crate::server::api::set_discharge_slot(
                    axum::extract::State(s2),
                    axum::response::Json(serde_json::json!({
                        "slot": 1, "enabled": true,
                        "start_hour": 5, "start_minute": 0,
                        "end_hour": 6, "end_minute": 0, "target_soc": 55
                    })),
                )
                .await
            });
            while !(apply.is_finished() && edit.is_finished()) {
                {
                    let mut pending = state.pending_writes.lock().await;
                    for batch in pending.iter_mut() {
                        if let Some(tx) = batch.completion.take() {
                            let _ = tx.send(WriteOutcome::Ok);
                        }
                    }
                }
                tokio::task::yield_now().await;
            }
            apply.await.unwrap();
            let (status, _) = edit.await.unwrap();
            assert_eq!(status, axum::http::StatusCode::OK);

            let config = state.timed_export_config.lock().await.clone();
            assert_eq!(config.slots[0].start_hour, 5, "the user's edit survived");
            assert!(
                slot_matches_event(&config.slots[1], &stored("e1")),
                "the override landed"
            );
            let disk = crate::settings::Settings::load();
            assert_eq!(disk.timed_export_slots[0].start_hour, 5);
            assert_eq!(disk.timed_export_slots[1].start_hour, 18);
        })
        .await;
    }

    // ---- alerts for Saving Session exports ----

    struct NtfyMock {
        base: String,
        bodies: Arc<std::sync::Mutex<Vec<String>>>,
        _shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl NtfyMock {
        async fn spawn() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = bodies.clone();
            let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
                let seen = seen.clone();
                async move {
                    let bytes = axum::body::to_bytes(req.into_body(), 1 << 20)
                        .await
                        .unwrap();
                    seen.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&bytes).to_string());
                    "ok"
                }
            });
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = rx.await;
                    })
                    .await;
            });
            Self {
                base: format!("http://{addr}"),
                bodies,
                _shutdown: Some(tx),
            }
        }

        /// Wait for the detached alert tasks to land, then return every body.
        async fn alerts_after(&self, expected: usize) -> Vec<String> {
            for _ in 0..250 {
                if self.bodies.lock().unwrap().len() >= expected {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            // Give an unexpected extra alert a moment to show up too.
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            self.bodies.lock().unwrap().clone()
        }
    }

    impl Drop for NtfyMock {
        fn drop(&mut self) {
            if let Some(tx) = self._shutdown.take() {
                let _ = tx.send(());
            }
        }
    }

    async fn alerts_to(state: &Arc<AppState>, mock: &NtfyMock) {
        *state.alert_config.lock().await = crate::settings::AlertsConfig {
            enabled: true,
            ntfy_topic: "hem".to_string(),
            ntfy_server: mock.base.clone(),
            ..Default::default()
        };
    }

    fn session_export(id: &str, slot: u8) -> ExportOnceEvent {
        ExportOnceEvent {
            source: "saving_session:9".to_string(),
            ..event(id, slot, "2026-10-17 18:00", "2026-10-17 19:00")
        }
    }

    #[tokio::test]
    async fn a_saving_session_export_says_when_it_starts_and_when_it_finishes() {
        with_isolated_config_dir_async(|| async {
            let mock = NtfyMock::spawn().await;
            let state = make_state().await;
            alerts_to(&state, &mock).await;
            seed_schedule(&state, true, vec![sched(22, 0, 23, 0, 30)]).await;
            store(vec![session_export("e1", 1)]);

            set_clock(&state, "2026-10-17 17:55").await;
            run_tick(&state).await;
            let alerts = mock.alerts_after(1).await;
            assert_eq!(alerts.len(), 1, "{alerts:?}");
            assert!(alerts[0].contains("export started"), "{}", alerts[0]);
            assert!(
                alerts[0].contains("Slot 1") && alerts[0].contains("19:00"),
                "{}",
                alerts[0]
            );

            set_clock(&state, "2026-10-17 19:01").await;
            run_tick(&state).await;
            let alerts = mock.alerts_after(2).await;
            assert_eq!(alerts.len(), 2, "{alerts:?}");
            assert!(alerts[1].contains("export finished"), "{}", alerts[1]);
        })
        .await;
    }

    #[tokio::test]
    async fn a_saving_session_export_that_never_ran_says_so() {
        with_isolated_config_dir_async(|| async {
            let mock = NtfyMock::spawn().await;
            let state = make_state().await;
            alerts_to(&state, &mock).await;
            seed_schedule(&state, true, vec![sched(22, 0, 23, 0, 30)]).await;
            store(vec![session_export("e1", 1)]);

            set_clock(&state, "2026-10-17 20:00").await;
            run_tick(&state).await;
            let alerts = mock.alerts_after(1).await;
            assert_eq!(alerts.len(), 1, "{alerts:?}");
            assert!(alerts[0].contains("did not run"), "{}", alerts[0]);
            assert!(alerts[0].contains("Missed"), "{}", alerts[0]);
        })
        .await;
    }

    #[tokio::test]
    async fn an_export_booked_by_hand_sends_no_alerts() {
        with_isolated_config_dir_async(|| async {
            let mock = NtfyMock::spawn().await;
            let state = make_state().await;
            alerts_to(&state, &mock).await;
            seed_schedule(&state, true, vec![sched(22, 0, 23, 0, 30)]).await;
            store(vec![event("e1", 1, "2026-10-17 18:00", "2026-10-17 19:00")]);

            set_clock(&state, "2026-10-17 17:55").await;
            run_tick(&state).await;
            set_clock(&state, "2026-10-17 19:01").await;
            run_tick(&state).await;
            assert!(mock.alerts_after(1).await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn cancelling_a_saving_session_export_by_hand_sends_no_alert() {
        with_isolated_config_dir_async(|| async {
            let mock = NtfyMock::spawn().await;
            let state = make_state().await;
            alerts_to(&state, &mock).await;
            seed_schedule(&state, true, vec![sched(22, 0, 23, 0, 30)]).await;
            store(vec![session_export("e1", 1)]);
            cancel_event(&state, "e1").await.unwrap();
            assert!(mock.alerts_after(1).await.is_empty());
        })
        .await;
    }

    // ---- interaction with Cosy (the same ownership check the poll loop uses) ----

    use crate::inverter::cosy::{plan_cosy_step, CosyInputs, CosyStep};
    use crate::settings::CosySlot;

    fn cosy_slot(start: (u8, u8), end: (u8, u8)) -> CosySlot {
        CosySlot {
            enabled: true,
            start_hour: start.0,
            start_minute: start.1,
            end_hour: end.0,
            end_minute: end.1,
            target_soc: 100,
        }
    }

    /// Whether the Export Once schedule, as the poll loop sees it right now,
    /// claims the discharge controls at `minute`.
    async fn export_claims(state: &Arc<AppState>, minute: u16) -> bool {
        let config = state.timed_export_config.lock().await.clone();
        let machine = state.timed_export_state.lock().await.clone();
        let snapshot = state.latest_snapshot.lock().await.clone().unwrap();
        crate::inverter::state_machines::timed_export_claims_discharge(
            &config, &machine, &snapshot, minute,
        )
    }

    fn cosy_step(slots: &[CosySlot], minute: u16, active: bool, export_owns: bool) -> CosyStep {
        plan_cosy_step(&CosyInputs {
            enabled: true,
            slots,
            now_minutes: minute,
            cosy_active: active,
            last_preloaded: None,
            timed_export_owns_discharge: export_owns,
            device_type: DeviceType::Gen3Hybrid,
        })
    }

    #[tokio::test]
    async fn cosy_waits_for_an_export_once_window_and_resumes_when_it_ends() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            seed_schedule(&state, false, vec![]).await;
            store(vec![event("e1", 1, "2026-10-17 18:00", "2026-10-17 19:00")]);
            // A Cosy charge slot that starts inside the export and outlasts it.
            let cosy = [cosy_slot((18, 30), (19, 30))];

            set_clock(&state, "2026-10-17 17:55").await;
            run_tick(&state).await;
            assert!(
                !export_claims(&state, 17 * 60 + 55).await,
                "not before its window"
            );

            // Inside the window the export holds the controls; Cosy does not
            // start charging over it.
            assert!(export_claims(&state, 18 * 60 + 30).await);
            assert!(matches!(
                cosy_step(&cosy, 18 * 60 + 30, false, true),
                CosyStep::Idle
            ));

            // The export ends and the slot is put back; Cosy's slot is still
            // open, so it now starts.
            set_clock(&state, "2026-10-17 19:01").await;
            run_tick(&state).await;
            assert_eq!(stored("e1").state, ExportOnceState::Done);
            assert!(
                !export_claims(&state, 19 * 60 + 1).await,
                "released after the revert"
            );
            assert!(matches!(
                cosy_step(&cosy, 19 * 60 + 1, false, false),
                CosyStep::Enter { .. }
            ));
        })
        .await;
    }

    #[tokio::test]
    async fn a_cosy_slot_ending_inside_an_export_once_window_does_not_cancel_it() {
        with_isolated_config_dir_async(|| async {
            let state = make_state().await;
            seed_schedule(&state, false, vec![]).await;
            store(vec![event("e1", 1, "2026-10-17 18:00", "2026-10-17 19:00")]);
            // Cosy is charging 17:00-18:30 when the export opens at 18:00.
            let cosy = [cosy_slot((17, 0), (18, 30))];

            set_clock(&state, "2026-10-17 18:20").await;
            run_tick(&state).await;
            assert!(export_claims(&state, 18 * 60 + 30).await);

            let step = cosy_step(&cosy, 18 * 60 + 31, true, true);
            let CosyStep::Exit { writes, .. } = step else {
                panic!("Cosy should still wind down its own charging, got {step:?}");
            };
            let regs: Vec<(u16, u16)> = writes.iter().map(|w| (w.address, w.value)).collect();
            assert!(regs.contains(&(96, 0)), "charge is switched off: {regs:?}");
            assert!(
                !regs.contains(&(27, 1)) && !regs.contains(&(59, 0)),
                "Cosy must not restore Eco or clear the discharge flag mid-export: {regs:?}"
            );
        })
        .await;
    }
}
