//! Octopus Saving Sessions (read-only).
//!
//! Reads the account's joined Saving Sessions from Octopus's GraphQL API so
//! HEM can tell the user about them. The API is not part of Octopus's
//! documented REST surface, so everything here fails soft: a broken or
//! changed response shows "unavailable" on the Octopus page and never
//! touches billing sync.
//!
//! Only `TURN_DOWN` events (the Saving Session / Power Down event) count.
//! `joinedEvents` carries no event type, so each joined id is looked up in
//! the offered `events` list; anything whose type cannot be established is
//! ignored. A joined Weekend Happy Hour (cheap import, no reward) must never
//! be mistaken for a session.
//!
//! Joining is opt-in and separate from everything above. With auto-join on,
//! upcoming `TURN_DOWN` events the account has not joined are joined through
//! the same API. Every join is attempted at most once (the id is persisted
//! before the request is sent), is gated on the account already being enrolled
//! in the campaign, and is confirmed by re-reading the joined list before
//! anything - an alert or an export - relies on it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use chrono::{DateTime, Duration, Utc};
use chrono_tz::Europe::London;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::alerts::AlertDelivery;
use crate::inverter::export_once::{self, NewEventRequest};
use crate::inverter::poll::AppState;
use crate::settings::Settings;

/// The only event type that is a Saving Session.
pub const SESSION_EVENT_TYPE: &str = "TURN_DOWN";
/// How long before a session starts the reminder alert is sent.
pub const REMINDER_LEAD_MINUTES: i64 = 30;
/// Alert-id history kept in settings.
const KEEP_ALERT_IDS: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionsMode {
    /// No requests are made to Octopus.
    #[default]
    Off,
    /// Check for joined sessions and send alerts.
    Alert,
    /// As `Alert`, and book a one-off export for every joined session.
    Export,
}

/// A joined Saving Session that has not finished yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    pub id: i64,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub reward_octopoints_per_kwh: Option<i64>,
}

/// An upcoming Saving Session Octopus is offering that the account has not
/// joined, with the code the join request needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Joinable {
    pub id: i64,
    #[serde(skip)]
    pub code: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub reward_octopoints_per_kwh: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// Whether the account has joined the Octoplus campaign, if reported.
    pub enrolled: Option<bool>,
    pub sessions: Vec<Session>,
    /// Offered sessions that have not started and are not joined yet.
    pub joinable: Vec<Joinable>,
}

/// Turn the combined `savingSessions` response into upcoming joined
/// sessions. Pure: `now` is passed in.
pub fn parse_response(response: &Value, now: DateTime<Utc>) -> Result<Parsed, String> {
    if let Some(errors) = response.get("errors").and_then(Value::as_array) {
        if !errors.is_empty() {
            let message = errors
                .iter()
                .filter_map(|error| error["message"].as_str())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(format!("Octopus returned an error: {message}"));
        }
    }
    let unexpected = || "Octopus returned an unexpected Saving Sessions response".to_string();
    let root = response
        .pointer("/data/savingSessions")
        .filter(|value| value.is_object())
        .ok_or_else(unexpected)?;
    let account = root
        .get("account")
        .filter(|value| value.is_object())
        .ok_or_else(unexpected)?;
    let events = root
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(unexpected)?;
    let joined = account
        .get("joinedEvents")
        .and_then(Value::as_array)
        .ok_or_else(unexpected)?;

    // id -> (event type, reward). `joinedEvents` has no type of its own.
    let offered: HashMap<i64, (&str, Option<i64>)> = events
        .iter()
        .filter_map(|event| {
            Some((
                event["id"].as_i64()?,
                (
                    event["eventType"].as_str()?,
                    event["rewardPerKwhInOctoPoints"].as_i64(),
                ),
            ))
        })
        .collect();

    let mut sessions: Vec<Session> = joined
        .iter()
        .filter_map(|entry| {
            let id = entry["eventId"].as_i64()?;
            let start = DateTime::parse_from_rfc3339(entry["startAt"].as_str()?).ok()?;
            let end = DateTime::parse_from_rfc3339(entry["endAt"].as_str()?).ok()?;
            let (kind, reward) = offered.get(&id)?;
            if *kind != SESSION_EVENT_TYPE {
                return None;
            }
            let (start, end) = (start.with_timezone(&Utc), end.with_timezone(&Utc));
            (end > start && end > now).then_some(Session {
                id,
                start,
                end,
                reward_octopoints_per_kwh: *reward,
            })
        })
        .collect();
    sessions.sort_by_key(|session| (session.start, session.id));

    let joined_ids: HashSet<i64> = joined
        .iter()
        .filter_map(|entry| entry["eventId"].as_i64())
        .collect();
    let mut joinable: Vec<Joinable> = events
        .iter()
        .filter_map(|event| {
            let id = event["id"].as_i64()?;
            if event["eventType"].as_str()? != SESSION_EVENT_TYPE
                || event["status"].as_str()? != "UPCOMING"
                || joined_ids.contains(&id)
            {
                return None;
            }
            let code = event["code"].as_str().filter(|code| !code.is_empty())?;
            let start = DateTime::parse_from_rfc3339(event["startAt"].as_str()?)
                .ok()?
                .with_timezone(&Utc);
            let end = DateTime::parse_from_rfc3339(event["endAt"].as_str()?)
                .ok()?
                .with_timezone(&Utc);
            // A session that has begun cannot be usefully joined.
            (end > start && start > now).then(|| Joinable {
                id,
                code: code.to_string(),
                start,
                end,
                reward_octopoints_per_kwh: event["rewardPerKwhInOctoPoints"].as_i64(),
            })
        })
        .collect();
    joinable.sort_by_key(|offer| (offer.start, offer.id));
    Ok(Parsed {
        enrolled: account["hasJoinedCampaign"].as_bool(),
        sessions,
        joinable,
    })
}

/// Which alerts are due for `sessions` given what has already been sent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DueAlerts {
    /// Sessions not yet announced.
    pub announce: Vec<Session>,
    /// Sessions inside the reminder window that have not had a reminder.
    pub remind: Vec<Session>,
}

pub fn within_reminder_window(session: &Session, now: DateTime<Utc>) -> bool {
    now >= session.start - Duration::minutes(REMINDER_LEAD_MINUTES) && now < session.start
}

pub fn due_alerts(
    sessions: &[Session],
    announced: &[i64],
    reminded: &[i64],
    now: DateTime<Utc>,
) -> DueAlerts {
    DueAlerts {
        announce: sessions
            .iter()
            .filter(|session| !announced.contains(&session.id))
            .cloned()
            .collect(),
        // A session being announced right now needs no separate reminder:
        // the caller records that announcement as covering the window.
        remind: sessions
            .iter()
            .filter(|session| {
                announced.contains(&session.id)
                    && !reminded.contains(&session.id)
                    && within_reminder_window(session, now)
            })
            .cloned()
            .collect(),
    }
}

/// `Mon 19 Oct, 18:00–19:00 (UK time)`.
fn session_window_text(start: DateTime<Utc>, end: DateTime<Utc>) -> String {
    let start = start.with_timezone(&London);
    let end = end.with_timezone(&London);
    format!(
        "{}, {}–{} (UK time)",
        start.format("%a %-d %b"),
        start.format("%H:%M"),
        end.format("%H:%M"),
    )
}

/// Notification text. Times are shown in UK time, which is what Octopus
/// sessions are announced in.
pub fn alert_text(session: &Session, reminder: bool) -> String {
    let headline = if reminder {
        format!("⚡ HEM — Octopus Saving Session starts in about {REMINDER_LEAD_MINUTES} minutes")
    } else {
        "⚡ HEM — Octopus Saving Session joined".to_string()
    };
    let mut text = format!(
        "{headline}\n{}",
        session_window_text(session.start, session.end)
    );
    if let Some(reward) = session.reward_octopoints_per_kwh {
        text.push_str(&format!("\nReward: {reward} OctoPoints per kWh"));
    }
    text
}

/// Append `id`, keeping only the most recent entries.
pub fn remember(ids: &mut Vec<i64>, id: i64) {
    if ids.contains(&id) {
        return;
    }
    ids.push(id);
    if ids.len() > KEEP_ALERT_IDS {
        ids.remove(0);
    }
}

/// `source` tag on the one-off exports booked for Saving Sessions.
const EXPORT_SOURCE_PREFIX: &str = "saving_session:";

fn export_source(session_id: i64) -> String {
    format!("{EXPORT_SOURCE_PREFIX}{session_id}")
}

/// The Control-page booking that realises `session`. Times are UK local,
/// which is how Octopus announces sessions; the inverter clock is expected to
/// agree (the same assumption every Timed Export window makes).
pub fn export_request(
    session: &Session,
    slot: u8,
    floor_soc: u8,
) -> Result<NewEventRequest, String> {
    let start = session.start.with_timezone(&London);
    let end = session.end.with_timezone(&London);
    if start.date_naive() != end.date_naive() {
        return Err("The session crosses midnight, which a one-off export cannot".to_string());
    }
    Ok(NewEventRequest {
        slot,
        date: start.format("%Y-%m-%d").to_string(),
        start: start.format("%H:%M").to_string(),
        end: end.format("%H:%M").to_string(),
        target_soc: floor_soc,
    })
}

// ---------------------------------------------------------------------------
// Octopus GraphQL client
// ---------------------------------------------------------------------------

const OFFICIAL_AUTH_URL: &str = "https://api.octopus.energy/v1/graphql/";
const OFFICIAL_QUERY_URL: &str = "https://api.backend.octopus.energy/v1/graphql/";
const HTTP_TIMEOUT: StdDuration = StdDuration::from_secs(20);
const POLL_INTERVAL: StdDuration = StdDuration::from_secs(10 * 60);

const TOKEN_MUTATION: &str =
    "mutation($key: String!) { obtainKrakenToken(input: { APIKey: $key }) { token } }";
const SESSIONS_QUERY: &str = "query($account: String!) { savingSessions { \
    events { id code startAt endAt status eventType rewardPerKwhInOctoPoints } \
    account(accountNumber: $account) { hasJoinedCampaign joinedEvents { eventId startAt endAt } } } }";

/// What the last check found.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SessionsState {
    /// When Octopus last answered successfully.
    pub checked_at: Option<DateTime<Utc>>,
    /// Why the latest check failed, if it did. The sessions below are then
    /// the last good ones.
    pub error: Option<String>,
    pub enrolled: Option<bool>,
    pub sessions: Vec<Session>,
    /// Sessions whose export could not be booked at the last check, and why.
    pub booking_errors: HashMap<i64, String>,
    /// Offered sessions the account has not joined.
    pub joinable: Vec<Joinable>,
    /// Sessions HEM tried to join and could not, and why. Kept while the
    /// session is still on offer, since a failed join is never retried.
    pub join_errors: HashMap<i64, String>,
}

/// Where the token exchange and the sessions query go. A configured
/// `octopus_api_base_url` (used by tests) serves both.
fn endpoints(settings: &Settings) -> (String, String) {
    let base = settings.octopus_api_base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        (
            OFFICIAL_AUTH_URL.to_string(),
            OFFICIAL_QUERY_URL.to_string(),
        )
    } else {
        let url = format!("{base}/v1/graphql/");
        (url.clone(), url)
    }
}

fn post_graphql(url: &str, token: Option<&str>, body: &Value) -> Result<Value, String> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .max_idle_connections(0)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut request = agent.post(url);
    if let Some(token) = token {
        request = request.header("Authorization", token);
    }
    // Errors are described without the request body: it carries the API key.
    let mut response = request.send_json(body).map_err(|error| match error {
        ureq::Error::StatusCode(code) => format!("Octopus answered HTTP {code}"),
        other => format!("Octopus request failed: {other}"),
    })?;
    response
        .body_mut()
        .read_json::<Value>()
        .map_err(|error| format!("Octopus sent an unreadable response: {error}"))
}

/// Exchange the API key for a short-lived token.
fn obtain_token(settings: &Settings) -> Result<String, String> {
    let (auth_url, _) = endpoints(settings);
    let key = settings.octopus_api_key.trim();
    let token_response = post_graphql(
        &auth_url,
        None,
        &json!({ "query": TOKEN_MUTATION, "variables": { "key": key } }),
    )?;
    if let Some(message) = token_response
        .get("errors")
        .and_then(Value::as_array)
        .and_then(|errors| errors.first())
        .and_then(|error| error["message"].as_str())
    {
        return Err(format!("Octopus did not accept the API key: {message}"));
    }
    token_response
        .pointer("/data/obtainKrakenToken/token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Octopus did not return an access token".to_string())
}

fn query_sessions(settings: &Settings, token: &str, now: DateTime<Utc>) -> Result<Parsed, String> {
    let (_, query_url) = endpoints(settings);
    let response = post_graphql(
        &query_url,
        Some(token),
        &json!({
            "query": SESSIONS_QUERY,
            "variables": { "account": settings.octopus_account_number.trim() },
        }),
    )?;
    parse_response(&response, now)
}

/// Exchange the API key for a short-lived token and read the sessions. The
/// token comes back too, so a join in the same check does not need another.
fn fetch_blocking(settings: &Settings, now: DateTime<Utc>) -> Result<(Parsed, String), String> {
    let token = obtain_token(settings)?;
    let parsed = query_sessions(settings, &token, now)?;
    Ok((parsed, token))
}

/// The join request for one offered session. Values go in as JSON string
/// literals, which GraphQL accepts, so neither can break out of the query.
fn join_mutation(account: &str, code: &str) -> String {
    let quote = |value: &str| serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        "mutation {{ joinSavingSessionsEvent(input: {{ accountNumber: {}, eventCode: {} }}) \
         {{ possibleErrors {{ message }} }} }}",
        quote(account),
        quote(code),
    )
}

/// Whether Octopus refused a join request. Success here only means "no
/// refusal": the caller still confirms against the joined list.
fn join_outcome(response: &Value) -> Result<(), String> {
    let messages = |list: Option<&Value>| -> Vec<String> {
        list.and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["message"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut problems = messages(response.get("errors"));
    problems.extend(messages(
        response.pointer("/data/joinSavingSessionsEvent/possibleErrors"),
    ));
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Octopus would not join it: {}",
            problems.join("; ")
        ))
    }
}

/// What one round of join requests achieved.
struct JoinPass {
    /// Per requested session, whether Octopus refused it.
    requests: Vec<Result<(), String>>,
    /// The joined list read again afterwards, which is what decides success.
    after: Result<Parsed, String>,
}

/// How many times the joined list is read to confirm a join, and how long to
/// wait between reads. Octopus can take a moment to show a session it has just
/// accepted; reading again is safe where sending again is not.
const CONFIRM_READS: usize = 3;
#[cfg(not(test))]
const CONFIRM_DELAY: StdDuration = StdDuration::from_secs(2);
#[cfg(test)]
const CONFIRM_DELAY: StdDuration = StdDuration::from_millis(5);

/// Send a join request for each of `targets`, then read the sessions again
/// until each accepted one shows up (or the reads run out).
fn join_blocking(
    settings: &Settings,
    token: &str,
    targets: &[Joinable],
    now: DateTime<Utc>,
) -> JoinPass {
    let (_, query_url) = endpoints(settings);
    let account = settings.octopus_account_number.trim();
    let requests: Vec<Result<(), String>> = targets
        .iter()
        .map(|target| {
            post_graphql(
                &query_url,
                Some(token),
                &json!({ "query": join_mutation(account, &target.code) }),
            )
            .and_then(|response| join_outcome(&response))
        })
        .collect();
    let mut after = query_sessions(settings, token, now);
    for _ in 1..CONFIRM_READS {
        let all_visible = after.as_ref().is_ok_and(|parsed| {
            requests
                .iter()
                .zip(targets)
                .filter(|(outcome, _)| outcome.is_ok())
                .all(|(_, target)| {
                    parsed
                        .sessions
                        .iter()
                        .any(|session| session.id == target.id)
                })
        });
        if all_visible {
            break;
        }
        std::thread::sleep(CONFIRM_DELAY);
        after = query_sessions(settings, token, now);
    }
    JoinPass { requests, after }
}

// ---------------------------------------------------------------------------
// Check loop
// ---------------------------------------------------------------------------

/// One pass: fetch, publish, book exports when asked to, and send any alerts
/// that are due.
pub async fn check_once(state: &Arc<AppState>, now: DateTime<Utc>) {
    let settings = Settings::load_async().await;
    if !session_checks_wanted(state, &settings).await {
        return;
    }
    let for_fetch = settings.clone();
    let fetched = tokio::task::spawn_blocking(move || fetch_blocking(&for_fetch, now))
        .await
        .map_err(|error| format!("Saving Sessions check failed: {error}"))
        .and_then(|result| result);
    let (parsed, token) = match fetched {
        Ok(fetched) => fetched,
        Err(error) => {
            // A failed check changes nothing: bookings stay as they were.
            let mut current = state.octopus_sessions.lock().await;
            if current.error.as_deref() != Some(error.as_str()) {
                tracing::warn!("Saving Sessions: {error}");
            }
            current.error = Some(error);
            return;
        }
    };
    // The user may have switched mode or skipped a session while Octopus was
    // answering (it can take a while). Act on what they want now, not on what
    // they wanted when the check began.
    let settings = Settings::load_async().await;
    if !session_checks_wanted(state, &settings).await {
        return;
    }
    let (parsed, joined_by_hem) = auto_join(state, &settings, parsed, &token, now).await;
    let floor_applies = max_discharge_slots(state).await > 2;
    let outcomes = if settings.octopus_sessions_mode == SessionsMode::Export {
        cancel_withdrawn_exports(state, &settings, &parsed.sessions).await;
        let previous_errors = state.octopus_sessions.lock().await.booking_errors.clone();
        book_sessions(state, &settings, &parsed.sessions, &previous_errors).await
    } else {
        HashMap::new()
    };
    {
        let mut current = state.octopus_sessions.lock().await;
        current.checked_at = Some(now);
        current.error = None;
        current.enrolled = parsed.enrolled;
        current.sessions = parsed.sessions.clone();
        current
            .join_errors
            .retain(|id, _| parsed.joinable.iter().any(|offer| offer.id == *id));
        // An attempt is remembered across restarts but its reason is not.
        // A session still on offer after an attempt did not get joined, and
        // must not silently drop off the page.
        for offer in &parsed.joinable {
            if settings.octopus_sessions_join_attempted.contains(&offer.id) {
                current.join_errors.entry(offer.id).or_insert_with(|| {
                    "HEM tried to join this earlier and it is still not joined. HEM will not \
                     try again; check the Octopus app"
                        .to_string()
                });
            }
        }
        current.joinable = parsed.joinable.clone();
        current.booking_errors = outcomes
            .iter()
            .filter_map(|(id, result)| result.as_ref().err().map(|error| (*id, error.clone())))
            .collect();
    }
    send_due_alerts(
        state,
        &settings,
        &parsed.sessions,
        now,
        &outcomes,
        floor_applies,
        &joined_by_hem,
    )
    .await;
}

/// Most join requests sent in one check. Offers are few; this bounds how much
/// of the account one pass can change if Octopus ever lists a great many.
const MAX_JOINS_PER_CHECK: usize = 5;

/// Join offered sessions when the user has opted in, and return what Octopus
/// now reports plus the ids HEM joined and confirmed.
///
/// Safeguards: only an account already enrolled in the campaign is touched
/// (HEM never enrols one); a session the user skipped is left alone; each
/// session is attempted once, the id saved just before its request is sent
/// (after the token is in hand, so a failed login costs nothing); and a join
/// counts only once the joined list shows it. A failure is recorded and
/// alerted, never retried.
async fn auto_join(
    state: &Arc<AppState>,
    settings: &Settings,
    parsed: Parsed,
    token: &str,
    now: DateTime<Utc>,
) -> (Parsed, HashSet<i64>) {
    let mut joined_by_hem = HashSet::new();
    if !settings.octopus_sessions_auto_join || parsed.enrolled != Some(true) {
        return (parsed, joined_by_hem);
    }
    let targets: Vec<Joinable> = parsed
        .joinable
        .iter()
        .filter(|offer| {
            !settings.octopus_sessions_join_attempted.contains(&offer.id)
                && !settings.octopus_sessions_skipped.contains(&offer.id)
        })
        .take(MAX_JOINS_PER_CHECK)
        .cloned()
        .collect();
    if targets.is_empty() {
        return (parsed, joined_by_hem);
    }

    // Persist the attempt first. If it cannot be saved, a crash could repeat
    // the request on restart, so nothing is sent.
    let ids: Vec<i64> = targets.iter().map(|offer| offer.id).collect();
    if let Err(error) = Settings::update_async(move |settings| {
        for id in ids {
            remember(&mut settings.octopus_sessions_join_attempted, id);
        }
    })
    .await
    {
        tracing::error!("Saving Sessions: not joining, could not record the attempt: {error}");
        return (parsed, joined_by_hem);
    }

    let for_join = settings.clone();
    let for_targets = targets.clone();
    let for_token = token.to_string();
    let pass = tokio::task::spawn_blocking(move || {
        join_blocking(&for_join, &for_token, &for_targets, now)
    })
    .await;
    let Ok(pass) = pass else {
        // The attempt is on record, so it is not repeated; say so.
        let mut current = state.octopus_sessions.lock().await;
        for offer in &targets {
            current.join_errors.insert(
                offer.id,
                "The join request did not complete, so it was not retried".to_string(),
            );
        }
        return (parsed, joined_by_hem);
    };

    let confirmed: HashSet<i64> = pass
        .after
        .as_ref()
        .map(|after| after.sessions.iter().map(|session| session.id).collect())
        .unwrap_or_default();
    let mut failures = Vec::new();
    for (offer, request) in targets.iter().zip(pass.requests.iter()) {
        if confirmed.contains(&offer.id) {
            // Credit HEM only when Octopus took HEM's request. If it refused
            // (say the user joined in the app a moment earlier) the session is
            // joined all the same, but not by HEM.
            if request.is_ok() {
                joined_by_hem.insert(offer.id);
            }
            continue;
        }
        // `unconfirmed`: Octopus took the request but the account does not show
        // the session (yet). That is not the same as a refusal, and the message
        // must not claim a failure that the next check may disprove.
        let (reason, unconfirmed) = match (request, &pass.after) {
            (Err(error), _) => (error.clone(), false),
            (Ok(()), Ok(_)) => (
                "Octopus accepted the request but the session is not showing on the account \
                 yet. HEM will not send it again; if it appears you will get the usual \
                 message, otherwise join it in the Octopus app"
                    .to_string(),
                true,
            ),
            (Ok(()), Err(error)) => (
                format!("The request was sent but could not be confirmed: {error}"),
                true,
            ),
        };
        tracing::warn!(
            "Saving Sessions: could not join session {}: {reason}",
            offer.id
        );
        failures.push((offer.clone(), reason, unconfirmed));
    }
    {
        let mut current = state.octopus_sessions.lock().await;
        for (offer, reason, _) in &failures {
            current.join_errors.insert(offer.id, reason.clone());
        }
    }
    if !failures.is_empty() {
        // One message however many sessions failed: a login or account
        // problem fails them all, and five notifications at once is noise.
        let headline = if failures.iter().all(|(_, _, unconfirmed)| *unconfirmed) {
            "⚡ HEM — Octopus Saving Session join not confirmed"
        } else {
            "⚡ HEM — could not join an Octopus Saving Session"
        };
        let mut text = headline.to_string();
        for (offer, reason, _) in &failures {
            text.push_str(&format!(
                "\n{}: {reason}",
                session_window_text(offer.start, offer.end)
            ));
        }
        crate::alerts::send_text_notification(state, &text, "saving-session").await;
    }
    // Carry on with what Octopus reports now; if it could not be read again,
    // with what it reported before, and the next check picks up any join.
    (pass.after.unwrap_or(parsed), joined_by_hem)
}

/// Whether to check for sessions at all. Also enforces that nothing booked for
/// a session outlives Activate Export (or the Octopus credentials it needs),
/// however the setting was changed, and clears what the page shows when the
/// feature is off.
async fn session_checks_wanted(state: &Arc<AppState>, settings: &Settings) -> bool {
    let configured = crate::octopus::configured(settings);
    if settings.octopus_sessions_mode != SessionsMode::Export || !configured {
        cancel_session_exports(state, None, true).await;
    }
    if settings.octopus_sessions_mode == SessionsMode::Off || !configured {
        *state.octopus_sessions.lock().await = SessionsState::default();
        return false;
    }
    true
}

/// Turn each joined session that is not skipped or already booked into a
/// one-off export, the same way the Control page does. Returns what happened
/// for every session attempted this pass.
async fn book_sessions(
    state: &Arc<AppState>,
    settings: &Settings,
    sessions: &[Session],
    previous_errors: &HashMap<i64, String>,
) -> HashMap<i64, Result<(), String>> {
    let mut outcomes = HashMap::new();
    for session in sessions {
        if settings.octopus_sessions_skipped.contains(&session.id)
            || settings.octopus_sessions_booked.contains(&session.id)
        {
            continue;
        }
        let result = match export_request(
            session,
            settings.octopus_sessions_export_slot,
            settings.octopus_sessions_export_floor_soc,
        ) {
            Ok(request) => export_once::create_event(state, &request, &export_source(session.id))
                .await
                .map(|_| ()),
            Err(error) => Err(error),
        };
        match &result {
            Ok(()) => record_booked(session.id).await,
            Err(error) => {
                // The same failure is retried every check; say it once.
                if previous_errors.get(&session.id) != Some(error) {
                    tracing::warn!(
                        "Saving Sessions: could not book session {}: {error}",
                        session.id
                    );
                }
            }
        }
        outcomes.insert(session.id, result);
    }
    outcomes
}

fn session_id_of(event: &export_once::ExportOnceEvent) -> Option<i64> {
    event
        .source
        .strip_prefix(EXPORT_SOURCE_PREFIX)?
        .parse()
        .ok()
}

/// Cancel the exports of sessions (matching `only`, or all) and forget that
/// they were booked, so they can be booked again later. A running export is
/// stopped and its slot put back only when `include_running` is set.
async fn cancel_session_exports(
    state: &Arc<AppState>,
    only: Option<i64>,
    include_running: bool,
) -> Vec<i64> {
    let mut cancelled = Vec::new();
    for event in export_once::list_events().await {
        let Some(id) = session_id_of(&event) else {
            continue;
        };
        if only.is_some_and(|wanted| wanted != id) {
            continue;
        }
        let eligible = event.state == export_once::ExportOnceState::Pending
            || (include_running && event.state == export_once::ExportOnceState::Active);
        if eligible && export_once::cancel_event(state, &event.id).await.is_ok() {
            cancelled.push(id);
        }
    }
    if !cancelled.is_empty() {
        let ids = cancelled.clone();
        let _ = Settings::update_async(move |settings| {
            settings
                .octopus_sessions_booked
                .retain(|id| !ids.contains(id));
        })
        .await;
    }
    cancelled
}

/// A pending export whose session Octopus no longer lists (or that the user
/// skipped) must not fire. A running one is left to finish and revert.
async fn cancel_withdrawn_exports(
    state: &Arc<AppState>,
    settings: &Settings,
    sessions: &[Session],
) {
    let live: HashSet<i64> = sessions
        .iter()
        .map(|session| session.id)
        .filter(|id| !settings.octopus_sessions_skipped.contains(id))
        .collect();
    for event in export_once::list_events().await {
        if event.state != export_once::ExportOnceState::Pending {
            continue;
        }
        if let Some(id) = session_id_of(&event) {
            if !live.contains(&id) {
                tracing::info!(
                    "Saving Sessions: session {id} is no longer listed, cancelling its export"
                );
                // Also forget the booking, so a session that Octopus lists
                // again is booked again rather than staying cancelled.
                cancel_session_exports(state, Some(id), false).await;
            }
        }
    }
}

/// What the alert should tell the user about the export for this session.
fn export_note(
    settings: &Settings,
    session_id: i64,
    outcomes: &HashMap<i64, Result<(), String>>,
    floor_applies: bool,
) -> Option<String> {
    if settings.octopus_sessions_mode != SessionsMode::Export {
        return None;
    }
    if settings.octopus_sessions_skipped.contains(&session_id) {
        return Some("Skipped, so HEM will not export.".to_string());
    }
    // Only models with extended slots hold a per-slot floor; elsewhere the
    // export runs to the battery's own reserve, so do not promise a level.
    let will_export = || {
        if floor_applies {
            format!(
                "HEM will export during it: slot {}, down to {}%.",
                settings.octopus_sessions_export_slot, settings.octopus_sessions_export_floor_soc
            )
        } else {
            format!(
                "HEM will export during it: slot {}.",
                settings.octopus_sessions_export_slot
            )
        }
    };
    match outcomes.get(&session_id) {
        Some(Ok(())) => Some(will_export()),
        Some(Err(error)) => Some(format!("HEM could not book an export: {error}")),
        None => settings
            .octopus_sessions_booked
            .contains(&session_id)
            .then(will_export),
    }
}

async fn send_due_alerts(
    state: &Arc<AppState>,
    settings: &Settings,
    sessions: &[Session],
    now: DateTime<Utc>,
    outcomes: &HashMap<i64, Result<(), String>>,
    floor_applies: bool,
    joined_by_hem: &HashSet<i64>,
) {
    let due = due_alerts(
        sessions,
        &settings.octopus_sessions_announced,
        &settings.octopus_sessions_reminded,
        now,
    );
    for session in due.announce {
        let mut text = alert_text(&session, false);
        if joined_by_hem.contains(&session.id) {
            text.push_str("\nHEM joined this session for you.");
        }
        if let Some(note) = export_note(settings, session.id, outcomes, floor_applies) {
            text.push('\n');
            text.push_str(&note);
        }
        let delivery = crate::alerts::send_text_notification(state, &text, "saving-session").await;
        if delivery == AlertDelivery::Delivered {
            // The announcement already says it is imminent when it lands
            // inside the reminder window, so no second message follows.
            let covers_reminder = within_reminder_window(&session, now);
            record(session.id, true, covers_reminder).await;
        }
    }
    for session in due.remind {
        let delivery = crate::alerts::send_text_notification(
            state,
            &alert_text(&session, true),
            "saving-session",
        )
        .await;
        if delivery == AlertDelivery::Delivered {
            record(session.id, false, true).await;
        }
    }
}

async fn record(id: i64, announced: bool, reminded: bool) {
    let result = Settings::update_async(move |settings| {
        if announced {
            remember(&mut settings.octopus_sessions_announced, id);
        }
        if reminded {
            remember(&mut settings.octopus_sessions_reminded, id);
        }
    })
    .await;
    if let Err(error) = result {
        tracing::error!("Saving Sessions: could not record sent alert: {error}");
    }
}

async fn record_booked(id: i64) {
    let result = Settings::update_async(move |settings| {
        remember(&mut settings.octopus_sessions_booked, id);
    })
    .await;
    if let Err(error) = result {
        tracing::error!("Saving Sessions: could not record the booking: {error}");
    }
}

/// Background loop. Wakes early when the mode changes.
pub async fn run_sessions_loop(state: Arc<AppState>) {
    loop {
        check_once(&state, Utc::now()).await;
        tokio::select! {
            () = tokio::time::sleep(POLL_INTERVAL) => {}
            () = state.octopus_sessions_wake.notified() => {}
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

async fn max_discharge_slots(state: &Arc<AppState>) -> u8 {
    state
        .latest_snapshot
        .lock()
        .await
        .as_ref()
        .map_or(crate::inverter::model::DeviceType::Gen2Hybrid, |snapshot| {
            snapshot.device_type
        })
        .max_discharge_slots()
}

/// How the export for one session stands, for the Octopus page.
fn export_status(settings: &Settings, current: &SessionsState, session: &Session) -> Option<Value> {
    if settings.octopus_sessions_mode != SessionsMode::Export {
        return None;
    }
    if settings.octopus_sessions_skipped.contains(&session.id) {
        return Some(json!({ "state": "skipped" }));
    }
    let source = export_source(session.id);
    if let Some(event) = settings
        .export_once_events
        .iter()
        .rev()
        .find(|event| event.source == source)
    {
        use export_once::ExportOnceState as S;
        let state = match event.state {
            S::Pending => "booked",
            S::Active => "running",
            S::Done => "done",
            S::Cancelled => "cancelled",
            S::Failed => "failed",
        };
        return Some(json!({ "state": state, "detail": event.note }));
    }
    current
        .booking_errors
        .get(&session.id)
        .map(|error| json!({ "state": "error", "detail": error }))
}

/// GET /api/octopus/sessions
pub async fn get_sessions(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let settings = Settings::load_async().await;
    let current = state.octopus_sessions.lock().await.clone();
    let alerts_ready = {
        let config = state.alert_config.lock().await;
        config.enabled && crate::alerts::has_delivery_channel(&config)
    };
    let sessions: Vec<Value> = current
        .sessions
        .iter()
        .map(|session| {
            json!({
                "id": session.id,
                "start": session.start,
                "end": session.end,
                "reward_octopoints_per_kwh": session.reward_octopoints_per_kwh,
                "announced": settings.octopus_sessions_announced.contains(&session.id),
                "skipped": settings.octopus_sessions_skipped.contains(&session.id),
                "export": export_status(&settings, &current, session),
            })
        })
        .collect();
    let join_failures: Vec<Value> = if settings.octopus_sessions_auto_join {
        current
            .joinable
            .iter()
            .filter_map(|offer| {
                let error = current.join_errors.get(&offer.id)?;
                Some(json!({
                    "id": offer.id,
                    "start": offer.start,
                    "end": offer.end,
                    "error": error,
                }))
            })
            .collect()
    } else {
        Vec::new()
    };
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "mode": settings.octopus_sessions_mode,
            "configured": crate::octopus::configured(&settings),
            "alerts_ready": alerts_ready,
            "export_slot": settings.octopus_sessions_export_slot,
            "export_floor_soc": settings.octopus_sessions_export_floor_soc,
            "max_discharge_slots": max_discharge_slots(&state).await,
            "checked_at": current.checked_at,
            "error": current.error,
            "enrolled": current.enrolled,
            "auto_join": settings.octopus_sessions_auto_join,
            "join_failures": join_failures,
            "sessions": sessions,
        })),
    )
}

#[derive(Deserialize)]
pub struct ModeRequest {
    mode: SessionsMode,
    #[serde(default)]
    export_slot: Option<u8>,
    #[serde(default)]
    export_floor_soc: Option<u8>,
    /// Join new sessions automatically. Left as it is when absent.
    #[serde(default)]
    auto_join: Option<bool>,
}

fn rejected(message: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "ok": false, "error": message })),
    )
}

/// POST /api/octopus/sessions/mode — body `{ "mode": "off" | "alert" | "export",
/// "export_slot": 1, "export_floor_soc": 20, "auto_join": false }` (the last
/// three optional).
pub async fn set_sessions_mode(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ModeRequest>,
) -> (StatusCode, Json<Value>) {
    let mode = body.mode;
    let max_slots = max_discharge_slots(&state).await;
    if let Some(slot) = body.export_slot {
        if !(1..=max_slots).contains(&slot) {
            return rejected(format!(
                "Slot must be between 1 and {max_slots} on this inverter"
            ));
        }
    }
    if let Some(floor) = body.export_floor_soc {
        if !(4..=99).contains(&floor) {
            return rejected("The export floor must be between 4 and 99%".to_string());
        }
    }
    let (export_slot, export_floor_soc) = (body.export_slot, body.export_floor_soc);
    let auto_join = body.auto_join;
    let before = Settings::load_async().await;
    if let Err(error) = Settings::update_async(move |settings| {
        settings.octopus_sessions_mode = mode;
        if let Some(slot) = export_slot {
            settings.octopus_sessions_export_slot = slot;
        }
        if let Some(floor) = export_floor_soc {
            settings.octopus_sessions_export_floor_soc = floor;
        }
        if let Some(on) = auto_join {
            settings.octopus_sessions_auto_join = on;
        }
    })
    .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": format!("Could not save the setting: {error}") })),
        );
    }
    let export_settings_changed = export_slot
        .is_some_and(|s| s != before.octopus_sessions_export_slot)
        || export_floor_soc.is_some_and(|f| f != before.octopus_sessions_export_floor_soc);
    if mode != SessionsMode::Export {
        // Leaving export mode: nothing booked for sessions may fire, and a
        // running one is stopped and its slot put back.
        cancel_session_exports(&state, None, true).await;
    } else if export_settings_changed {
        // The slot or floor changed: pending exports are re-booked with the
        // new values at the next check. A running one finishes as it is.
        cancel_session_exports(&state, None, false).await;
    }
    if mode == SessionsMode::Off {
        *state.octopus_sessions.lock().await = SessionsState::default();
    }
    state.octopus_sessions_wake.notify_one();
    (StatusCode::OK, Json(json!({ "ok": true, "mode": mode })))
}

#[derive(Deserialize)]
pub struct SkipRequest {
    id: i64,
    skip: bool,
}

/// POST /api/octopus/sessions/skip — body `{ "id": 7244, "skip": true }`.
/// Skipping cancels the pending export for that session; un-skipping lets the
/// next check book it again.
pub async fn skip_session(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SkipRequest>,
) -> (StatusCode, Json<Value>) {
    let (id, skip) = (body.id, body.skip);
    if let Err(error) = Settings::update_async(move |settings| {
        if skip {
            remember(&mut settings.octopus_sessions_skipped, id);
        } else {
            settings
                .octopus_sessions_skipped
                .retain(|skipped| *skipped != id);
            settings
                .octopus_sessions_booked
                .retain(|booked| *booked != id);
        }
    })
    .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": format!("Could not save the setting: {error}") })),
        );
    }
    if skip {
        cancel_session_exports(&state, Some(id), false).await;
    } else {
        state.octopus_sessions_wake.notify_one();
    }
    (StatusCode::OK, Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn utc(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    const NOW: &str = "2026-10-17T12:00:00+00:00";

    fn response(joined: Value, events: Value) -> Value {
        json!({ "data": { "savingSessions": {
            "events": events,
            "account": {
                "hasJoinedCampaign": true,
                "canJoinCampaign": false,
                "joinedEvents": joined
            }
        }}})
    }

    fn event(id: i64, kind: &str, start: &str, end: &str, reward: i64) -> Value {
        json!({ "id": id, "code": format!("code-{id}"), "eventType": kind, "startAt": start,
                "endAt": end, "status": "UPCOMING", "rewardPerKwhInOctoPoints": reward })
    }

    fn joined(id: i64, start: &str, end: &str) -> Value {
        json!({ "eventId": id, "startAt": start, "endAt": end })
    }

    fn session(id: i64, start: &str, end: &str) -> Session {
        Session {
            id,
            start: utc(start),
            end: utc(end),
            reward_octopoints_per_kwh: Some(73),
        }
    }

    // ---- parsing ----

    #[test]
    fn a_joined_saving_session_is_returned_with_its_reward() {
        let r = response(
            json!([joined(
                1,
                "2026-10-19T17:00:00+00:00",
                "2026-10-19T18:00:00+00:00"
            )]),
            json!([event(
                1,
                "TURN_DOWN",
                "2026-10-19T17:00:00+00:00",
                "2026-10-19T18:00:00+00:00",
                73
            )]),
        );
        let parsed = parse_response(&r, utc(NOW)).unwrap();
        assert_eq!(parsed.enrolled, Some(true));
        assert_eq!(
            parsed.sessions,
            vec![session(
                1,
                "2026-10-19T17:00:00+00:00",
                "2026-10-19T18:00:00+00:00"
            )]
        );
    }

    #[test]
    fn a_joined_weekend_happy_hour_is_not_a_saving_session() {
        // The account that motivated this: its only upcoming joined event
        // was a Weekend Happy Hour (cheap import, reward 0).
        let r = response(
            json!([joined(
                7244,
                "2026-10-18T10:00:00+00:00",
                "2026-10-18T11:00:00+00:00"
            )]),
            json!([event(
                7244,
                "WEEKEND_HAPPY_HOUR",
                "2026-10-18T10:00:00+00:00",
                "2026-10-18T11:00:00+00:00",
                0
            )]),
        );
        assert!(parse_response(&r, utc(NOW)).unwrap().sessions.is_empty());
    }

    #[test]
    fn turn_up_events_and_unknown_types_are_ignored() {
        let r = response(
            json!([
                joined(1, "2026-10-19T17:00:00+00:00", "2026-10-19T18:00:00+00:00"),
                joined(2, "2026-10-20T17:00:00+00:00", "2026-10-20T18:00:00+00:00"),
                joined(3, "2026-10-21T17:00:00+00:00", "2026-10-21T18:00:00+00:00")
            ]),
            json!([
                event(
                    1,
                    "TURN_UP",
                    "2026-10-19T17:00:00+00:00",
                    "2026-10-19T18:00:00+00:00",
                    10
                ),
                event(
                    2,
                    "SOMETHING_NEW",
                    "2026-10-20T17:00:00+00:00",
                    "2026-10-20T18:00:00+00:00",
                    10
                )
            ]),
        );
        // id 3 is joined but absent from the offered list, so its type is unknown.
        assert!(parse_response(&r, utc(NOW)).unwrap().sessions.is_empty());
    }

    #[test]
    fn finished_and_running_sessions_are_handled_by_end_time() {
        let r = response(
            json!([
                joined(1, "2026-10-10T17:00:00+00:00", "2026-10-10T18:00:00+00:00"),
                joined(2, "2026-10-17T11:30:00+00:00", "2026-10-17T12:30:00+00:00"),
                joined(3, "2026-10-17T12:00:00+00:00", "2026-10-17T12:00:00+00:00")
            ]),
            json!([
                event(
                    1,
                    "TURN_DOWN",
                    "2026-10-10T17:00:00+00:00",
                    "2026-10-10T18:00:00+00:00",
                    70
                ),
                event(
                    2,
                    "TURN_DOWN",
                    "2026-10-17T11:30:00+00:00",
                    "2026-10-17T12:30:00+00:00",
                    70
                ),
                event(
                    3,
                    "TURN_DOWN",
                    "2026-10-17T12:00:00+00:00",
                    "2026-10-17T12:00:00+00:00",
                    70
                )
            ]),
        );
        let ids: Vec<i64> = parse_response(&r, utc(NOW))
            .unwrap()
            .sessions
            .iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, vec![2], "running stays, finished and zero-length go");
    }

    #[test]
    fn sessions_come_back_in_start_order_with_offsets_normalised_to_utc() {
        let r = response(
            json!([
                joined(2, "2026-10-21T18:00:00+01:00", "2026-10-21T19:00:00+01:00"),
                joined(1, "2026-10-19T17:00:00+00:00", "2026-10-19T18:00:00+00:00")
            ]),
            json!([
                event(
                    1,
                    "TURN_DOWN",
                    "2026-10-19T17:00:00+00:00",
                    "2026-10-19T18:00:00+00:00",
                    70
                ),
                event(
                    2,
                    "TURN_DOWN",
                    "2026-10-21T18:00:00+01:00",
                    "2026-10-21T19:00:00+01:00",
                    70
                )
            ]),
        );
        let sessions = parse_response(&r, utc(NOW)).unwrap().sessions;
        assert_eq!(
            sessions.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(sessions[1].start, utc("2026-10-21T17:00:00+00:00"));
    }

    #[test]
    fn malformed_joined_entries_are_skipped_not_fatal() {
        let r = response(
            json!([
                { "eventId": "x", "startAt": "nope", "endAt": null },
                joined(1, "2026-10-19T17:00:00+00:00", "2026-10-19T18:00:00+00:00")
            ]),
            json!([event(
                1,
                "TURN_DOWN",
                "2026-10-19T17:00:00+00:00",
                "2026-10-19T18:00:00+00:00",
                70
            )]),
        );
        assert_eq!(parse_response(&r, utc(NOW)).unwrap().sessions.len(), 1);
    }

    #[test]
    fn a_missing_reward_is_none() {
        let mut ev = event(
            1,
            "TURN_DOWN",
            "2026-10-19T17:00:00+00:00",
            "2026-10-19T18:00:00+00:00",
            70,
        );
        ev.as_object_mut()
            .unwrap()
            .remove("rewardPerKwhInOctoPoints");
        let r = response(
            json!([joined(
                1,
                "2026-10-19T17:00:00+00:00",
                "2026-10-19T18:00:00+00:00"
            )]),
            json!([ev]),
        );
        assert_eq!(
            parse_response(&r, utc(NOW)).unwrap().sessions[0].reward_octopoints_per_kwh,
            None
        );
    }

    #[test]
    fn graphql_errors_and_unexpected_shapes_are_errors() {
        let err = json!({ "errors": [{ "message": "Unauthorized" }] });
        assert!(parse_response(&err, utc(NOW))
            .unwrap_err()
            .contains("Unauthorized"));
        assert!(parse_response(&json!({ "data": null }), utc(NOW)).is_err());
        assert!(parse_response(
            &json!({ "data": { "savingSessions": { "events": [] } } }),
            utc(NOW)
        )
        .is_err());
        assert!(parse_response(&json!("garbage"), utc(NOW)).is_err());
    }

    #[test]
    fn enrolment_is_unknown_when_not_reported() {
        let r = json!({ "data": { "savingSessions": { "events": [], "account": { "joinedEvents": [] } } } });
        assert_eq!(parse_response(&r, utc(NOW)).unwrap().enrolled, None);
    }

    // ---- alerts ----

    #[test]
    fn a_new_session_is_announced_once() {
        let s = session(1, "2026-10-19T17:00:00+00:00", "2026-10-19T18:00:00+00:00");
        let due = due_alerts(std::slice::from_ref(&s), &[], &[], utc(NOW));
        assert_eq!(due.announce, vec![s.clone()]);
        assert!(due.remind.is_empty(), "far from the start");
        let due = due_alerts(std::slice::from_ref(&s), &[1], &[], utc(NOW));
        assert!(due.announce.is_empty());
    }

    #[test]
    fn a_reminder_is_due_only_inside_the_lead_window() {
        let s = session(1, "2026-10-17T12:30:00+00:00", "2026-10-17T13:30:00+00:00");
        assert!(due_alerts(
            std::slice::from_ref(&s),
            &[1],
            &[],
            utc("2026-10-17T11:59:00+00:00")
        )
        .remind
        .is_empty());
        assert_eq!(
            due_alerts(
                std::slice::from_ref(&s),
                &[1],
                &[],
                utc("2026-10-17T12:00:00+00:00")
            )
            .remind,
            vec![s.clone()]
        );
        assert!(due_alerts(
            std::slice::from_ref(&s),
            &[1],
            &[1],
            utc("2026-10-17T12:10:00+00:00")
        )
        .remind
        .is_empty());
        // Once it has started there is nothing to remind about.
        assert!(due_alerts(
            std::slice::from_ref(&s),
            &[1],
            &[],
            utc("2026-10-17T12:30:00+00:00")
        )
        .remind
        .is_empty());
    }

    #[test]
    fn the_window_boundaries_are_exact() {
        let s = session(1, "2026-10-17T12:30:00+00:00", "2026-10-17T13:30:00+00:00");
        assert!(!within_reminder_window(
            &s,
            utc("2026-10-17T11:59:59+00:00")
        ));
        assert!(within_reminder_window(&s, utc("2026-10-17T12:00:00+00:00")));
        assert!(within_reminder_window(&s, utc("2026-10-17T12:29:59+00:00")));
        assert!(!within_reminder_window(
            &s,
            utc("2026-10-17T12:30:00+00:00")
        ));
    }

    #[test]
    fn alert_text_uses_uk_time_and_names_the_reward() {
        // 17:00 UTC on 19 Oct is 18:00 BST.
        let s = session(1, "2026-10-19T17:00:00+00:00", "2026-10-19T18:00:00+00:00");
        let text = alert_text(&s, false);
        assert!(text.contains("Mon 19 Oct"), "{text}");
        assert!(text.contains("18:00–19:00"), "{text}");
        assert!(text.contains("73"), "{text}");
        assert!(alert_text(&s, true).to_lowercase().contains("starts in"));
        // After the clocks go back, UTC and UK time agree.
        let winter = session(2, "2026-11-02T17:00:00+00:00", "2026-11-02T18:00:00+00:00");
        assert!(alert_text(&winter, false).contains("17:00–18:00"));
    }

    #[test]
    fn alert_text_copes_with_no_reward() {
        let mut s = session(1, "2026-10-19T17:00:00+00:00", "2026-10-19T18:00:00+00:00");
        s.reward_octopoints_per_kwh = None;
        assert!(!alert_text(&s, false).contains("None"));
    }

    #[test]
    fn remembered_ids_are_bounded_and_deduplicated() {
        let mut ids = Vec::new();
        for id in 0..80 {
            remember(&mut ids, id);
        }
        assert_eq!(ids.len(), 50);
        assert_eq!(ids.first(), Some(&30));
        assert_eq!(ids.last(), Some(&79));
        remember(&mut ids, 79);
        assert_eq!(ids.len(), 50, "no duplicate");
    }

    // ---- check loop against a mock Octopus (also the ntfy server) ----

    use crate::inverter::poll::AppState;
    use crate::settings::{AlertsConfig, Settings};
    use crate::test_util::with_isolated_config_dir_async;
    use axum::extract::Request;
    use axum::http::StatusCode;

    const KEY: &str = "sk_live_test_SECRETKEY";

    #[derive(Clone, Debug)]
    struct Seen {
        path: String,
        auth: Option<String>,
        body: String,
    }

    struct MockOctopus {
        base: String,
        seen: Arc<std::sync::Mutex<Vec<Seen>>>,
        token_body: Arc<std::sync::Mutex<String>>,
        sessions_body: Arc<std::sync::Mutex<String>>,
        /// Milliseconds the sessions query waits before answering.
        sessions_delay_ms: Arc<std::sync::atomic::AtomicU64>,
        /// What the join mutation answers.
        join_body: Arc<std::sync::Mutex<String>>,
        /// If set: the sessions answer to switch to once a join has been seen,
        /// how many reads after the join still get the old answer, and
        /// whether a join has been seen yet.
        after_join: Arc<std::sync::Mutex<Option<(String, u64, bool)>>>,
        _shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    }

    /// The first field a join request selects that Octopus's
    /// `JoinSavingSessionsEvent` lacks. Its only fields, read from the live
    /// schema, are `joinedEventCodes` and `joinedEvent`.
    fn unknown_join_field(body: &str) -> Option<String> {
        let query: Value = serde_json::from_str(body).ok()?;
        let query = query["query"].as_str()?;
        let selection = query.split(')').nth(1)?;
        selection
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .find(|word| !word.is_empty() && !matches!(*word, "joinedEventCodes" | "joinedEvent"))
            .map(str::to_string)
    }

    impl MockOctopus {
        async fn spawn(sessions: Value) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            let token_body = Arc::new(std::sync::Mutex::new(
                json!({ "data": { "obtainKrakenToken": { "token": "jwt-abc" } } }).to_string(),
            ));
            let sessions_body = Arc::new(std::sync::Mutex::new(sessions.to_string()));
            let sessions_delay_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let join_body = Arc::new(std::sync::Mutex::new(
                json!({ "data": { "joinSavingSessionsEvent": { "joinedEventCodes": [] } } })
                    .to_string(),
            ));
            let after_join: Arc<std::sync::Mutex<Option<(String, u64, bool)>>> =
                Arc::new(std::sync::Mutex::new(None));
            let (s2, t2, b2, d2, j2, a2) = (
                seen.clone(),
                token_body.clone(),
                sessions_body.clone(),
                sessions_delay_ms.clone(),
                join_body.clone(),
                after_join.clone(),
            );
            let app = axum::Router::new().fallback(move |req: Request| {
                let (s2, t2, b2, d2, j2, a2) = (
                    s2.clone(),
                    t2.clone(),
                    b2.clone(),
                    d2.clone(),
                    j2.clone(),
                    a2.clone(),
                );
                async move {
                    let path = req.uri().path().to_string();
                    let auth = req
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    let bytes = axum::body::to_bytes(req.into_body(), 1 << 20)
                        .await
                        .unwrap();
                    let body = String::from_utf8_lossy(&bytes).to_string();
                    s2.lock().unwrap().push(Seen {
                        path: path.clone(),
                        auth,
                        body: body.clone(),
                    });
                    if body.contains("obtainKrakenToken") {
                        (StatusCode::OK, t2.lock().unwrap().clone())
                    } else if body.contains("joinSavingSessionsEvent") {
                        // Like Octopus, refuse the whole request if it asks
                        // for a field the reply type does not have.
                        if let Some(field) = unknown_join_field(&body) {
                            let error = json!({ "errors": [{ "message": format!(
                                "Cannot query field '{field}' on type 'JoinSavingSessionsEvent'."
                            ) }] });
                            return (StatusCode::OK, error.to_string());
                        }
                        if let Some(pending) = a2.lock().unwrap().as_mut() {
                            pending.2 = true;
                        }
                        (StatusCode::OK, j2.lock().unwrap().clone())
                    } else if body.contains("savingSessions") {
                        {
                            let mut pending = a2.lock().unwrap();
                            if let Some((next, stale, joined)) = pending.as_mut() {
                                if *joined && *stale > 0 {
                                    *stale -= 1;
                                } else if *joined {
                                    *b2.lock().unwrap() = next.clone();
                                    *pending = None;
                                }
                            }
                        }
                        let delay = d2.load(std::sync::atomic::Ordering::Relaxed);
                        if delay > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        }
                        (StatusCode::OK, b2.lock().unwrap().clone())
                    } else if path == "/hem" {
                        (StatusCode::OK, "ok".to_string())
                    } else {
                        (StatusCode::NOT_FOUND, "no route".to_string())
                    }
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
                seen,
                token_body,
                sessions_body,
                sessions_delay_ms,
                join_body,
                after_join,
                _shutdown: Some(tx),
            }
        }

        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }

        fn alerts(&self) -> Vec<Seen> {
            self.seen()
                .into_iter()
                .filter(|s| s.path == "/hem")
                .collect()
        }

        /// Join requests the mock has received.
        fn joins(&self) -> Vec<Seen> {
            self.seen()
                .into_iter()
                .filter(|s| s.body.contains("joinSavingSessionsEvent"))
                .collect()
        }

        fn set_sessions(&self, body: Value) {
            *self.sessions_body.lock().unwrap() = body.to_string();
        }

        /// What the join mutation answers.
        fn set_join_answer(&self, body: Value) {
            *self.join_body.lock().unwrap() = body.to_string();
        }

        /// The sessions answer once a join request has been seen.
        fn set_sessions_after_join(&self, body: Value) {
            self.set_sessions_after_join_lagging(body, 0);
        }

        /// As above, but the first `stale_reads` reads after the join still
        /// get the old answer, as when Octopus is slow to show a new join.
        fn set_sessions_after_join_lagging(&self, body: Value, stale_reads: u64) {
            *self.after_join.lock().unwrap() = Some((body.to_string(), stale_reads, false));
        }
    }

    impl Drop for MockOctopus {
        fn drop(&mut self) {
            if let Some(tx) = self._shutdown.take() {
                let _ = tx.send(());
            }
        }
    }

    fn one_session_response() -> Value {
        response(
            json!([joined(
                9,
                "2026-10-19T17:00:00+00:00",
                "2026-10-19T18:00:00+00:00"
            )]),
            json!([event(
                9,
                "TURN_DOWN",
                "2026-10-19T17:00:00+00:00",
                "2026-10-19T18:00:00+00:00",
                73
            )]),
        )
    }

    async fn configure(mock: &MockOctopus, mode: SessionsMode) -> Arc<AppState> {
        Settings::update(|s| {
            s.octopus_api_key = KEY.to_string();
            s.octopus_account_number = "A-TEST1".to_string();
            s.octopus_api_base_url = mock.base.clone();
            s.octopus_sessions_mode = mode;
        })
        .unwrap();
        let state = Arc::new(AppState::new());
        *state.alert_config.lock().await = AlertsConfig {
            enabled: true,
            ntfy_topic: "hem".to_string(),
            ntfy_server: mock.base.clone(),
            ..Default::default()
        };
        state
    }

    #[tokio::test]
    async fn the_key_goes_only_to_the_token_exchange_and_the_token_to_the_query() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;

            let graphql: Vec<Seen> = mock
                .seen()
                .into_iter()
                .filter(|s| s.path == "/v1/graphql/")
                .collect();
            assert_eq!(graphql.len(), 2);
            assert!(
                graphql[0].body.contains(KEY),
                "token exchange carries the key"
            );
            assert_eq!(graphql[0].auth, None);
            assert_eq!(graphql[1].auth.as_deref(), Some("jwt-abc"));
            assert!(
                !graphql[1].body.contains(KEY),
                "the query must not carry the key"
            );
            assert!(graphql[1].body.contains("A-TEST1"));
            let sessions = state.octopus_sessions.lock().await.clone();
            assert_eq!(sessions.sessions.len(), 1);
            assert_eq!(sessions.enrolled, Some(true));
            assert_eq!(sessions.error, None);
            assert_eq!(sessions.checked_at, Some(utc(NOW)));
        })
        .await;
    }

    #[tokio::test]
    async fn off_mode_makes_no_requests_and_clears_the_list() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;
            assert_eq!(state.octopus_sessions.lock().await.sessions.len(), 1);
            let before = mock.seen().len();

            Settings::update(|s| s.octopus_sessions_mode = SessionsMode::Off).unwrap();
            check_once(&state, utc(NOW)).await;
            assert_eq!(mock.seen().len(), before, "no further requests");
            assert!(state.octopus_sessions.lock().await.sessions.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn an_account_without_an_account_number_is_never_contacted() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure(&mock, SessionsMode::Alert).await;
            Settings::update(|s| s.octopus_account_number = "  ".to_string()).unwrap();
            check_once(&state, utc(NOW)).await;
            assert!(mock.seen().is_empty());
            let (_, Json(body)) = get_sessions(State(state)).await;
            assert_eq!(body["configured"], false);
        })
        .await;
    }

    #[tokio::test]
    async fn an_account_without_octopus_credentials_is_never_contacted() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure(&mock, SessionsMode::Alert).await;
            Settings::update(|s| s.octopus_api_key.clear()).unwrap();
            check_once(&state, utc(NOW)).await;
            assert!(mock.seen().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_session_is_announced_once_and_survives_a_restart() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;
            assert_eq!(mock.alerts().len(), 1);
            assert!(mock.alerts()[0].body.contains("Mon 19 Oct, 18:00–19:00"));
            check_once(&state, utc("2026-10-17T12:10:00+00:00")).await;
            assert_eq!(mock.alerts().len(), 1, "not repeated");

            // A fresh process reads the announced ids from disk.
            let restarted = Arc::new(AppState::new());
            *restarted.alert_config.lock().await = state.alert_config.lock().await.clone();
            check_once(&restarted, utc("2026-10-17T12:20:00+00:00")).await;
            assert_eq!(mock.alerts().len(), 1, "still not repeated");
        })
        .await;
    }

    #[tokio::test]
    async fn a_joined_weekend_happy_hour_raises_no_alert() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(response(
                json!([joined(
                    7244,
                    "2026-10-18T10:00:00+00:00",
                    "2026-10-18T11:00:00+00:00"
                )]),
                json!([event(
                    7244,
                    "WEEKEND_HAPPY_HOUR",
                    "2026-10-18T10:00:00+00:00",
                    "2026-10-18T11:00:00+00:00",
                    0
                )]),
            ))
            .await;
            let state = configure(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;
            assert!(mock.alerts().is_empty());
            assert!(state.octopus_sessions.lock().await.sessions.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn alerts_that_could_not_be_sent_do_not_use_up_the_announcement() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure(&mock, SessionsMode::Alert).await;
            state.alert_config.lock().await.enabled = false;
            check_once(&state, utc(NOW)).await;
            assert!(mock.alerts().is_empty());
            assert!(Settings::load().octopus_sessions_announced.is_empty());

            // The user switches alerts on later: they still hear about it.
            state.alert_config.lock().await.enabled = true;
            check_once(&state, utc("2026-10-17T12:10:00+00:00")).await;
            assert_eq!(mock.alerts().len(), 1);
            assert_eq!(Settings::load().octopus_sessions_announced, vec![9]);
        })
        .await;
    }

    #[tokio::test]
    async fn a_reminder_follows_the_announcement_once() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;
            assert_eq!(mock.alerts().len(), 1);

            // 17:00 UTC start: the reminder window opens at 16:30.
            check_once(&state, utc("2026-10-19T16:29:00+00:00")).await;
            assert_eq!(mock.alerts().len(), 1);
            check_once(&state, utc("2026-10-19T16:31:00+00:00")).await;
            assert_eq!(mock.alerts().len(), 2);
            assert!(mock.alerts()[1].body.to_lowercase().contains("starts in"));
            check_once(&state, utc("2026-10-19T16:41:00+00:00")).await;
            assert_eq!(mock.alerts().len(), 2, "one reminder only");
        })
        .await;
    }

    #[tokio::test]
    async fn a_session_first_seen_inside_the_reminder_window_gets_one_alert() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure(&mock, SessionsMode::Alert).await;
            check_once(&state, utc("2026-10-19T16:40:00+00:00")).await;
            check_once(&state, utc("2026-10-19T16:50:00+00:00")).await;
            assert_eq!(mock.alerts().len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn a_failing_check_keeps_the_last_good_list_and_never_leaks_the_key() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;

            mock.set_sessions(json!({ "errors": [{ "message": "Internal error" }] }));
            check_once(&state, utc("2026-10-17T12:10:00+00:00")).await;
            let current = state.octopus_sessions.lock().await.clone();
            assert_eq!(current.sessions.len(), 1, "last good list kept");
            let error = current.error.expect("error surfaced");
            assert!(error.contains("Internal error"));
            assert!(!error.contains(KEY));
            assert_eq!(current.checked_at, Some(utc(NOW)), "still the last success");

            mock.set_sessions(one_session_response());
            check_once(&state, utc("2026-10-17T12:20:00+00:00")).await;
            assert_eq!(state.octopus_sessions.lock().await.error, None);
        })
        .await;
    }

    #[tokio::test]
    async fn a_rejected_key_stops_before_the_sessions_query() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            *mock.token_body.lock().unwrap() =
                json!({ "errors": [{ "message": "Invalid API key" }] }).to_string();
            let state = configure(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;

            assert_eq!(mock.seen().len(), 1, "only the token exchange");
            let error = state.octopus_sessions.lock().await.error.clone().unwrap();
            assert!(error.contains("Invalid API key"));
            assert!(!error.contains(KEY));
        })
        .await;
    }

    #[tokio::test]
    async fn an_unreachable_octopus_is_reported_not_fatal() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure(&mock, SessionsMode::Alert).await;
            Settings::update(|s| s.octopus_api_base_url = "http://127.0.0.1:1".to_string())
                .unwrap();
            check_once(&state, utc(NOW)).await;
            let error = state.octopus_sessions.lock().await.error.clone().unwrap();
            assert!(error.contains("request failed"), "{error}");
            assert!(!error.contains(KEY));
        })
        .await;
    }

    #[tokio::test]
    async fn the_handlers_report_state_and_change_the_mode() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure(&mock, SessionsMode::Off).await;

            let (status, Json(body)) = get_sessions(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["mode"], "off");
            assert_eq!(body["configured"], true);
            assert_eq!(body["alerts_ready"], true);
            assert!(body["sessions"].as_array().unwrap().is_empty());

            let (status, _) = set_sessions_mode(
                State(state.clone()),
                Json(ModeRequest {
                    mode: SessionsMode::Alert,
                    export_slot: None,
                    export_floor_soc: None,
                    auto_join: None,
                }),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(Settings::load().octopus_sessions_mode, SessionsMode::Alert);
            // The loop is woken so the change takes effect straight away.
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                state.octopus_sessions_wake.notified(),
            )
            .await
            .expect("wake permit stored");

            check_once(&state, utc(NOW)).await;
            let (_, Json(body)) = get_sessions(State(state.clone())).await;
            assert_eq!(body["mode"], "alert");
            assert_eq!(body["sessions"][0]["id"], 9);
            assert_eq!(body["sessions"][0]["announced"], true);
            assert_eq!(body["sessions"][0]["reward_octopoints_per_kwh"], 73);

            let (_, _) = set_sessions_mode(
                State(state.clone()),
                Json(ModeRequest {
                    mode: SessionsMode::Off,
                    export_slot: None,
                    export_floor_soc: None,
                    auto_join: None,
                }),
            )
            .await;
            let (_, Json(body)) = get_sessions(State(state)).await;
            assert!(
                body["sessions"].as_array().unwrap().is_empty(),
                "turning it off clears the list"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn alerts_are_not_ready_without_a_channel() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure(&mock, SessionsMode::Alert).await;
            *state.alert_config.lock().await = AlertsConfig::default();
            let (_, Json(body)) = get_sessions(State(state)).await;
            assert_eq!(body["alerts_ready"], false);
        })
        .await;
    }

    // ---- Activate export (phase 3) ----

    use crate::inverter::export_once::{list_events, ExportOnceState};
    use crate::inverter::model::{DeviceType, InverterSnapshot};

    #[test]
    fn a_session_becomes_an_export_request_in_uk_time() {
        // 17:00 UTC on 19 Oct is 18:00 BST.
        let s = session(9, "2026-10-19T17:00:00+00:00", "2026-10-19T18:00:00+00:00");
        let request = export_request(&s, 2, 25).unwrap();
        assert_eq!(request.slot, 2);
        assert_eq!(request.date, "2026-10-19");
        assert_eq!(request.start, "18:00");
        assert_eq!(request.end, "19:00");
        assert_eq!(request.target_soc, 25);

        // After the clocks go back UTC and UK time agree.
        let winter = session(10, "2026-11-02T17:00:00+00:00", "2026-11-02T18:30:00+00:00");
        let request = export_request(&winter, 1, 20).unwrap();
        assert_eq!(
            (request.start.as_str(), request.end.as_str()),
            ("17:00", "18:30")
        );
    }

    #[test]
    fn a_session_that_crosses_uk_midnight_cannot_be_booked() {
        // 22:30 UTC in summer is 23:30 BST; it ends at 00:30 the next day.
        let s = session(11, "2026-07-01T22:30:00+00:00", "2026-07-01T23:30:00+00:00");
        let error = export_request(&s, 1, 20).unwrap_err();
        assert!(error.contains("midnight"), "{error}");
    }

    const EXPORT_NOW: &str = "2026-10-17T12:00:00+00:00";

    /// Export mode with an inverter whose clock is pinned to `NOW`.
    async fn configure_export(mock: &MockOctopus, device: DeviceType) -> Arc<AppState> {
        let state = configure(mock, SessionsMode::Export).await;
        *state.latest_snapshot.lock().await = Some(InverterSnapshot {
            device_type: device,
            inverter_time: "2026-10-17 12:00:00".to_string(),
            ..Default::default()
        });
        state
    }

    fn session_events() -> Vec<crate::inverter::export_once::ExportOnceEvent> {
        crate::settings::Settings::load()
            .export_once_events
            .into_iter()
            .filter(|e| e.source.starts_with("saving_session:"))
            .collect()
    }

    #[tokio::test]
    async fn export_mode_books_each_joined_session_as_a_one_off_export() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            Settings::update(|s| {
                s.octopus_sessions_export_slot = 2;
                s.octopus_sessions_export_floor_soc = 30;
            })
            .unwrap();
            check_once(&state, utc(EXPORT_NOW)).await;

            let events = session_events();
            assert_eq!(events.len(), 1);
            let e = &events[0];
            assert_eq!(e.source, "saving_session:9");
            assert_eq!((e.slot, e.target_soc), (2, 30));
            assert_eq!(
                (e.start.as_str(), e.end.as_str()),
                ("2026-10-19 18:00", "2026-10-19 19:00")
            );
            assert_eq!(e.state, ExportOnceState::Pending);
        })
        .await;
    }

    #[tokio::test]
    async fn alert_only_mode_books_nothing() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            Settings::update(|s| s.octopus_sessions_mode = SessionsMode::Alert).unwrap();
            check_once(&state, utc(EXPORT_NOW)).await;
            assert!(list_events().await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_weekend_happy_hour_is_never_booked() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(response(
                json!([joined(
                    7244,
                    "2026-10-18T10:00:00+00:00",
                    "2026-10-18T11:00:00+00:00"
                )]),
                json!([event(
                    7244,
                    "WEEKEND_HAPPY_HOUR",
                    "2026-10-18T10:00:00+00:00",
                    "2026-10-18T11:00:00+00:00",
                    0
                )]),
            ))
            .await;
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            check_once(&state, utc(EXPORT_NOW)).await;
            assert!(list_events().await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_session_is_booked_once_even_after_a_restart() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            check_once(&state, utc(EXPORT_NOW)).await;
            check_once(&state, utc("2026-10-17T12:10:00+00:00")).await;
            assert_eq!(session_events().len(), 1);

            let restarted = Arc::new(AppState::new());
            *restarted.latest_snapshot.lock().await = state.latest_snapshot.lock().await.clone();
            check_once(&restarted, utc("2026-10-17T12:20:00+00:00")).await;
            assert_eq!(
                session_events().len(),
                1,
                "no second booking after a restart"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn skipping_cancels_the_pending_export_and_unskipping_books_it_again() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            check_once(&state, utc(EXPORT_NOW)).await;
            assert_eq!(session_events()[0].state, ExportOnceState::Pending);

            let (status, _) = skip_session(
                State(state.clone()),
                Json(SkipRequest { id: 9, skip: true }),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(session_events()[0].state, ExportOnceState::Cancelled);
            check_once(&state, utc("2026-10-17T12:10:00+00:00")).await;
            assert_eq!(
                session_events().len(),
                1,
                "a skipped session stays unbooked"
            );
            let (_, Json(body)) = get_sessions(State(state.clone())).await;
            assert_eq!(body["sessions"][0]["skipped"], true);
            assert_eq!(body["sessions"][0]["export"]["state"], "skipped");

            let (status, _) = skip_session(
                State(state.clone()),
                Json(SkipRequest { id: 9, skip: false }),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            check_once(&state, utc("2026-10-17T12:20:00+00:00")).await;
            let events = session_events();
            assert_eq!(events.len(), 2);
            assert_eq!(events[1].state, ExportOnceState::Pending);
        })
        .await;
    }

    #[tokio::test]
    async fn a_session_that_disappears_cancels_its_pending_export_but_not_a_running_one() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(response(
                json!([
                    joined(9, "2026-10-19T17:00:00+00:00", "2026-10-19T18:00:00+00:00"),
                    joined(12, "2026-10-20T17:00:00+00:00", "2026-10-20T18:00:00+00:00")
                ]),
                json!([
                    event(
                        9,
                        "TURN_DOWN",
                        "2026-10-19T17:00:00+00:00",
                        "2026-10-19T18:00:00+00:00",
                        73
                    ),
                    event(
                        12,
                        "TURN_DOWN",
                        "2026-10-20T17:00:00+00:00",
                        "2026-10-20T18:00:00+00:00",
                        73
                    )
                ]),
            ))
            .await;
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            check_once(&state, utc(EXPORT_NOW)).await;
            assert_eq!(session_events().len(), 2);
            // Session 12's export is already running.
            Settings::update(|s| {
                for e in s
                    .export_once_events
                    .iter_mut()
                    .filter(|e| e.source == "saving_session:12")
                {
                    e.state = ExportOnceState::Active;
                }
            })
            .unwrap();

            // Octopus withdraws both.
            mock.set_sessions(response(json!([]), json!([])));
            check_once(&state, utc("2026-10-17T12:10:00+00:00")).await;
            let by_source = |src: &str| {
                session_events()
                    .into_iter()
                    .find(|e| e.source == src)
                    .unwrap()
                    .state
            };
            assert_eq!(by_source("saving_session:9"), ExportOnceState::Cancelled);
            assert_eq!(by_source("saving_session:12"), ExportOnceState::Active);
        })
        .await;
    }

    #[tokio::test]
    async fn a_withdrawn_session_that_comes_back_is_booked_again() {
        // Octopus can drop a session from one answer and list it again in the
        // next. The cancelled export must not leave it marked as handled.
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            check_once(&state, utc(EXPORT_NOW)).await;
            assert_eq!(session_events()[0].state, ExportOnceState::Pending);

            mock.set_sessions(response(json!([]), json!([])));
            check_once(&state, utc("2026-10-17T12:10:00+00:00")).await;
            assert_eq!(session_events()[0].state, ExportOnceState::Cancelled);

            mock.set_sessions(one_session_response());
            check_once(&state, utc("2026-10-17T12:20:00+00:00")).await;
            let events = session_events();
            assert_eq!(events.len(), 2, "booked again");
            assert_eq!(events[1].state, ExportOnceState::Pending);
        })
        .await;
    }

    #[tokio::test]
    async fn clearing_the_account_number_cancels_pending_session_exports() {
        // The Settings text promises that clearing it "switches it off".
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            check_once(&state, utc(EXPORT_NOW)).await;
            assert_eq!(session_events()[0].state, ExportOnceState::Pending);

            Settings::update(|s| s.octopus_account_number.clear()).unwrap();
            check_once(&state, utc("2026-10-17T12:10:00+00:00")).await;
            assert_eq!(session_events()[0].state, ExportOnceState::Cancelled);
            assert!(Settings::load().octopus_sessions_booked.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_pending_session_export_never_outlives_export_mode() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            check_once(&state, utc(EXPORT_NOW)).await;
            // Switched away without going through the mode endpoint (another
            // window, a hand-edited file): the next check still cancels.
            Settings::update(|s| s.octopus_sessions_mode = SessionsMode::Alert).unwrap();
            check_once(&state, utc("2026-10-17T12:10:00+00:00")).await;
            assert_eq!(session_events()[0].state, ExportOnceState::Cancelled);
        })
        .await;
    }

    #[tokio::test]
    async fn switching_off_while_octopus_is_answering_stops_the_booking() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            mock.sessions_delay_ms
                .store(400, std::sync::atomic::Ordering::Relaxed);
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;

            let checking = {
                let state = state.clone();
                tokio::spawn(async move { check_once(&state, utc(EXPORT_NOW)).await })
            };
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            Settings::update(|s| s.octopus_sessions_mode = SessionsMode::Off).unwrap();
            checking.await.unwrap();
            assert!(
                session_events().is_empty(),
                "booked after the user switched off"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn skipping_while_octopus_is_answering_stops_that_booking() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            mock.sessions_delay_ms
                .store(400, std::sync::atomic::Ordering::Relaxed);
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;

            let checking = {
                let state = state.clone();
                tokio::spawn(async move { check_once(&state, utc(EXPORT_NOW)).await })
            };
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            let (status, _) = skip_session(
                State(state.clone()),
                Json(SkipRequest { id: 9, skip: true }),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            checking.await.unwrap();
            assert!(
                session_events().is_empty(),
                "booked a session the user skipped"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn the_announcement_only_promises_a_floor_where_the_inverter_honours_one() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            // A Gen2 hybrid has no per-slot floor register.
            let state = configure_export(&mock, DeviceType::Gen2Hybrid).await;
            Settings::update(|s| s.octopus_sessions_export_floor_soc = 35).unwrap();
            check_once(&state, utc(EXPORT_NOW)).await;
            let alert = &mock.alerts()[0].body;
            assert!(
                alert.contains("HEM will export") && alert.contains("slot 1"),
                "{alert}"
            );
            assert!(
                !alert.contains("down to") && !alert.contains("35%"),
                "{alert}"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_failed_check_does_not_cancel_anything() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            check_once(&state, utc(EXPORT_NOW)).await;
            mock.set_sessions(json!({ "errors": [{ "message": "Internal error" }] }));
            check_once(&state, utc("2026-10-17T12:10:00+00:00")).await;
            assert_eq!(session_events()[0].state, ExportOnceState::Pending);
        })
        .await;
    }

    #[tokio::test]
    async fn a_booking_that_cannot_be_made_is_reported_and_retried() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            // A Gen2 hybrid has two slots; ask for a third.
            let state = configure_export(&mock, DeviceType::Gen2Hybrid).await;
            Settings::update(|s| s.octopus_sessions_export_slot = 3).unwrap();
            check_once(&state, utc(EXPORT_NOW)).await;

            assert!(session_events().is_empty());
            let (_, Json(body)) = get_sessions(State(state.clone())).await;
            assert_eq!(body["sessions"][0]["export"]["state"], "error");
            assert!(body["sessions"][0]["export"]["detail"]
                .as_str()
                .unwrap()
                .contains("Slot"));
            let alert = &mock.alerts()[0].body;
            assert!(alert.contains("could not book"), "{alert}");

            // Fix the slot and the next check books it.
            Settings::update(|s| s.octopus_sessions_export_slot = 1).unwrap();
            check_once(&state, utc("2026-10-17T12:10:00+00:00")).await;
            assert_eq!(session_events().len(), 1);
            let (_, Json(body)) = get_sessions(State(state)).await;
            assert_eq!(body["sessions"][0]["export"]["state"], "booked");
        })
        .await;
    }

    #[tokio::test]
    async fn a_session_that_crosses_midnight_is_reported_not_booked() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(response(
                json!([joined(
                    11,
                    "2026-10-20T22:30:00+00:00",
                    "2026-10-20T23:30:00+00:00"
                )]),
                json!([event(
                    11,
                    "TURN_DOWN",
                    "2026-10-20T22:30:00+00:00",
                    "2026-10-20T23:30:00+00:00",
                    70
                )]),
            ))
            .await;
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            check_once(&state, utc(EXPORT_NOW)).await;
            assert!(session_events().is_empty());
            let (_, Json(body)) = get_sessions(State(state)).await;
            assert!(body["sessions"][0]["export"]["detail"]
                .as_str()
                .unwrap()
                .contains("midnight"));
        })
        .await;
    }

    #[tokio::test]
    async fn a_clashing_manual_export_is_reported_and_left_alone() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            // The user already booked slot 1 across the same hour.
            crate::inverter::export_once::create_event(
                &state,
                &crate::inverter::export_once::NewEventRequest {
                    slot: 1,
                    date: "2026-10-19".to_string(),
                    start: "17:30".to_string(),
                    end: "19:30".to_string(),
                    target_soc: 20,
                },
                crate::inverter::export_once::SOURCE_MANUAL,
            )
            .await
            .unwrap();
            check_once(&state, utc(EXPORT_NOW)).await;
            assert!(session_events().is_empty());
            let (_, Json(body)) = get_sessions(State(state)).await;
            assert!(body["sessions"][0]["export"]["detail"]
                .as_str()
                .unwrap()
                .contains("overlaps"));
            assert_eq!(
                list_events().await.len(),
                1,
                "the manual booking is untouched"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn leaving_export_mode_cancels_pending_session_exports() {
        with_isolated_config_dir_async(|| async {
            for leave_to in [SessionsMode::Alert, SessionsMode::Off] {
                let mock = MockOctopus::spawn(one_session_response()).await;
                let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
                check_once(&state, utc(EXPORT_NOW)).await;
                assert_eq!(
                    session_events().last().unwrap().state,
                    ExportOnceState::Pending
                );

                let (status, _) = set_sessions_mode(
                    State(state.clone()),
                    Json(ModeRequest {
                        mode: leave_to,
                        export_slot: None,
                        export_floor_soc: None,
                        auto_join: None,
                    }),
                )
                .await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(
                    session_events().last().unwrap().state,
                    ExportOnceState::Cancelled
                );
                // Reset for the next lap.
                Settings::update(|s| {
                    s.export_once_events.clear();
                    s.octopus_sessions_booked.clear();
                })
                .unwrap();
            }
        })
        .await;
    }

    #[tokio::test]
    async fn changing_the_slot_rebooks_pending_exports_in_the_new_slot() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            check_once(&state, utc(EXPORT_NOW)).await;
            assert_eq!(session_events()[0].slot, 1);

            let (status, _) = set_sessions_mode(
                State(state.clone()),
                Json(ModeRequest {
                    mode: SessionsMode::Export,
                    export_slot: Some(3),
                    export_floor_soc: Some(40),
                    auto_join: None,
                }),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(session_events()[0].state, ExportOnceState::Cancelled);
            check_once(&state, utc("2026-10-17T12:10:00+00:00")).await;
            let events = session_events();
            assert_eq!(events.len(), 2);
            assert_eq!((events[1].slot, events[1].target_soc), (3, 40));
            assert_eq!(events[1].state, ExportOnceState::Pending);
        })
        .await;
    }

    #[tokio::test]
    async fn the_mode_request_validates_slot_and_floor() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure_export(&mock, DeviceType::Gen2Hybrid).await;
            for (slot, floor) in [
                (Some(0), None),
                (Some(3), None),
                (None, Some(3)),
                (None, Some(100)),
            ] {
                let (status, body) = set_sessions_mode(
                    State(state.clone()),
                    Json(ModeRequest {
                        mode: SessionsMode::Export,
                        export_slot: slot,
                        export_floor_soc: floor,
                        auto_join: None,
                    }),
                )
                .await;
                assert_eq!(status, StatusCode::BAD_REQUEST, "{slot:?} {floor:?}");
                assert_eq!(body["ok"], false);
            }
            // Nothing was saved by the rejected requests.
            let saved = Settings::load();
            assert_eq!(saved.octopus_sessions_export_slot, 1);
            assert_eq!(saved.octopus_sessions_export_floor_soc, 20);
        })
        .await;
    }

    #[tokio::test]
    async fn the_announcement_says_what_hem_will_do() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            Settings::update(|s| {
                s.octopus_sessions_export_slot = 2;
                s.octopus_sessions_export_floor_soc = 35;
            })
            .unwrap();
            check_once(&state, utc(EXPORT_NOW)).await;
            let alert = &mock.alerts()[0].body;
            assert!(alert.contains("HEM will export"), "{alert}");
            assert!(alert.contains("slot 2") && alert.contains("35%"), "{alert}");
        })
        .await;
    }

    #[tokio::test]
    async fn get_sessions_reports_the_export_settings_and_per_session_status() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            check_once(&state, utc(EXPORT_NOW)).await;
            let (_, Json(body)) = get_sessions(State(state)).await;
            assert_eq!(body["mode"], "export");
            assert_eq!(body["export_slot"], 1);
            assert_eq!(body["export_floor_soc"], 20);
            assert_eq!(body["max_discharge_slots"], 10);
            assert_eq!(body["sessions"][0]["export"]["state"], "booked");
            assert_eq!(body["sessions"][0]["skipped"], false);
        })
        .await;
    }

    // ---- auto-join ----

    fn unjoined_response() -> Value {
        response(
            json!([]),
            json!([event(
                9,
                "TURN_DOWN",
                "2026-10-19T17:00:00+00:00",
                "2026-10-19T18:00:00+00:00",
                73
            )]),
        )
    }

    async fn configure_auto_join(mock: &MockOctopus, mode: SessionsMode) -> Arc<AppState> {
        let state = configure(mock, mode).await;
        Settings::update(|s| s.octopus_sessions_auto_join = true).unwrap();
        state
    }

    #[test]
    fn an_offered_session_the_account_has_not_joined_is_joinable() {
        let r = parse_response(&unjoined_response(), utc(NOW)).unwrap();
        assert!(r.sessions.is_empty());
        assert_eq!(r.joinable.len(), 1);
        let offer = &r.joinable[0];
        assert_eq!((offer.id, offer.code.as_str()), (9, "code-9"));
        assert_eq!(offer.start, utc("2026-10-19T17:00:00+00:00"));
        assert_eq!(offer.reward_octopoints_per_kwh, Some(73));
    }

    #[test]
    fn only_upcoming_unjoined_saving_sessions_with_a_code_are_joinable() {
        let mut no_code = event(
            5,
            "TURN_DOWN",
            "2026-10-20T17:00:00+00:00",
            "2026-10-20T18:00:00+00:00",
            73,
        );
        no_code.as_object_mut().unwrap().remove("code");
        let mut done = event(
            6,
            "TURN_DOWN",
            "2026-10-21T17:00:00+00:00",
            "2026-10-21T18:00:00+00:00",
            73,
        );
        done["status"] = json!("DONE");
        let r = parse_response(
            &response(
                json!([joined(
                    1,
                    "2026-10-19T17:00:00+00:00",
                    "2026-10-19T18:00:00+00:00"
                )]),
                json!([
                    // Already joined.
                    event(
                        1,
                        "TURN_DOWN",
                        "2026-10-19T17:00:00+00:00",
                        "2026-10-19T18:00:00+00:00",
                        73
                    ),
                    // Not a Saving Session.
                    event(
                        2,
                        "WEEKEND_HAPPY_HOUR",
                        "2026-10-24T10:00:00+00:00",
                        "2026-10-24T11:00:00+00:00",
                        0
                    ),
                    event(
                        3,
                        "TURN_UP",
                        "2026-10-24T10:00:00+00:00",
                        "2026-10-24T11:00:00+00:00",
                        0
                    ),
                    // Already started.
                    event(
                        4,
                        "TURN_DOWN",
                        "2026-10-17T11:00:00+00:00",
                        "2026-10-17T13:00:00+00:00",
                        73
                    ),
                    no_code,
                    done,
                    // The one that qualifies.
                    event(
                        7,
                        "TURN_DOWN",
                        "2026-10-22T17:00:00+00:00",
                        "2026-10-22T18:00:00+00:00",
                        73
                    ),
                ]),
            ),
            utc(NOW),
        )
        .unwrap();
        assert_eq!(r.joinable.iter().map(|o| o.id).collect::<Vec<_>>(), vec![7]);
    }

    #[test]
    fn the_join_request_keeps_hostile_values_inside_their_string_literals() {
        let m = join_mutation("A-1\"2", "co\"de}{ \\ x");
        assert_eq!(
            m,
            "mutation { joinSavingSessionsEvent(input: { accountNumber: \"A-1\\\"2\", \
             eventCode: \"co\\\"de}{ \\\\ x\" }) { joinedEventCodes } }"
        );
    }

    #[test]
    fn a_refused_join_is_reported_with_octopus_own_words() {
        assert_eq!(
            join_outcome(
                &json!({ "data": { "joinSavingSessionsEvent": { "joinedEventCodes": ["c"] } } })
            ),
            Ok(())
        );
        assert_eq!(
            join_outcome(
                &json!({ "data": { "joinSavingSessionsEvent": { "joinedEventCodes": [] } } })
            ),
            Ok(())
        );
        assert_eq!(
            join_outcome(&json!({ "data": { "joinSavingSessionsEvent": null },
                "errors": [{ "message": "Event has ended" }, { "message": "Try later" }] })),
            Err("Octopus would not join it: Event has ended; Try later".to_string())
        );
        assert_eq!(
            join_outcome(&json!({ "errors": [{ "message": "Cannot query field" }] })),
            Err("Octopus would not join it: Cannot query field".to_string())
        );
    }

    #[test]
    fn the_join_request_selects_only_fields_octopus_has() {
        let body = json!({ "query": join_mutation("A-1", "code") }).to_string();
        assert_eq!(unknown_join_field(&body), None);
        let stale = json!({ "query": "mutation { joinSavingSessionsEvent(input: {}) \
            { possibleErrors { message } } }" })
        .to_string();
        assert_eq!(
            unknown_join_field(&stale),
            Some("possibleErrors".to_string())
        );
    }

    #[tokio::test]
    async fn auto_join_is_off_by_default_and_joins_nothing() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(unjoined_response()).await;
            let state = configure(&mock, SessionsMode::Alert).await;
            assert!(!Settings::load().octopus_sessions_auto_join);
            check_once(&state, utc(NOW)).await;
            assert!(mock.joins().is_empty());
            assert!(Settings::load().octopus_sessions_join_attempted.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn an_offered_session_is_joined_and_confirmed_before_it_is_announced() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(unjoined_response()).await;
            mock.set_sessions_after_join(one_session_response());
            let state = configure_auto_join(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;

            let joins = mock.joins();
            assert_eq!(joins.len(), 1);
            assert_eq!(joins[0].auth.as_deref(), Some("jwt-abc"));
            assert!(joins[0].body.contains("A-TEST1"));
            assert!(joins[0].body.contains("code-9"));
            assert!(!joins[0].body.contains(KEY), "the key never rides along");

            assert_eq!(Settings::load().octopus_sessions_join_attempted, vec![9]);
            assert_eq!(state.octopus_sessions.lock().await.sessions.len(), 1);
            let alerts = mock.alerts();
            assert_eq!(alerts.len(), 1);
            assert!(alerts[0].body.contains("HEM joined this session for you."));
            assert!(state.octopus_sessions.lock().await.join_errors.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn the_join_is_confirmed_by_a_second_read_not_by_the_reply() {
        with_isolated_config_dir_async(|| async {
            // Octopus says fine, but the account still lists nothing joined.
            let mock = MockOctopus::spawn(unjoined_response()).await;
            let state = configure_auto_join(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;

            assert_eq!(mock.joins().len(), 1);
            let current = state.octopus_sessions.lock().await.clone();
            assert!(current.sessions.is_empty());
            assert!(current.join_errors[&9].contains("not showing on the account yet"));
            let alerts = mock.alerts();
            assert_eq!(alerts.len(), 1);
            assert!(alerts[0].body.contains("join not confirmed"));
            assert!(!alerts[0].body.contains("could not join"));
            assert!(!alerts[0].body.contains("joined this session for you"));
        })
        .await;
    }

    #[tokio::test]
    async fn a_refused_join_is_recorded_alerted_once_and_never_retried() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(unjoined_response()).await;
            mock.set_join_answer(json!({ "data": { "joinSavingSessionsEvent": null },
                "errors": [{ "message": "Event has ended" }] }));
            let state = configure_auto_join(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;
            check_once(&state, utc("2026-10-17T12:10:00+00:00")).await;

            assert_eq!(mock.joins().len(), 1, "not retried");
            let alerts = mock.alerts();
            assert_eq!(alerts.len(), 1, "alerted once");
            assert!(alerts[0].body.contains("Event has ended"));
            assert!(alerts[0].body.contains("Mon 19 Oct, 18:00–19:00"));

            let (_, Json(body)) = get_sessions(State(state)).await;
            assert_eq!(body["auto_join"], true);
            assert_eq!(body["join_failures"][0]["id"], 9);
            assert!(body["join_failures"][0]["error"]
                .as_str()
                .unwrap()
                .contains("Event has ended"));
        })
        .await;
    }

    #[tokio::test]
    async fn an_attempted_join_is_not_repeated_after_a_restart() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(unjoined_response()).await;
            let state = configure_auto_join(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;
            assert_eq!(mock.joins().len(), 1);

            let restarted = Arc::new(AppState::new());
            *restarted.alert_config.lock().await = state.alert_config.lock().await.clone();
            check_once(&restarted, utc("2026-10-17T12:20:00+00:00")).await;
            assert_eq!(mock.joins().len(), 1, "still one request");
        })
        .await;
    }

    #[tokio::test]
    async fn an_account_not_enrolled_in_the_campaign_is_never_joined() {
        with_isolated_config_dir_async(|| async {
            let mut body = unjoined_response();
            body["data"]["savingSessions"]["account"]["hasJoinedCampaign"] = json!(false);
            body["data"]["savingSessions"]["account"]["canJoinCampaign"] = json!(true);
            let mock = MockOctopus::spawn(body).await;
            let state = configure_auto_join(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;
            assert!(mock.joins().is_empty());
            assert!(Settings::load().octopus_sessions_join_attempted.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn off_mode_never_joins_even_with_auto_join_set() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(unjoined_response()).await;
            let state = configure_auto_join(&mock, SessionsMode::Off).await;
            check_once(&state, utc(NOW)).await;
            assert!(mock.seen().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn offers_that_are_not_saving_sessions_are_not_joined() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(response(
                json!([]),
                json!([event(
                    7244,
                    "WEEKEND_HAPPY_HOUR",
                    "2026-10-24T10:00:00+00:00",
                    "2026-10-24T11:00:00+00:00",
                    0
                )]),
            ))
            .await;
            let state = configure_auto_join(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;
            assert!(mock.joins().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn one_check_joins_a_bounded_number_and_the_rest_follow() {
        with_isolated_config_dir_async(|| async {
            let offers: Vec<Value> = (1..=7)
                .map(|n| {
                    event(
                        n,
                        "TURN_DOWN",
                        &format!("2026-10-{:02}T17:00:00+00:00", 18 + n),
                        &format!("2026-10-{:02}T18:00:00+00:00", 18 + n),
                        73,
                    )
                })
                .collect();
            let mock = MockOctopus::spawn(response(json!([]), json!(offers))).await;
            let state = configure_auto_join(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;
            assert_eq!(mock.joins().len(), MAX_JOINS_PER_CHECK);
            check_once(&state, utc("2026-10-17T12:10:00+00:00")).await;
            assert_eq!(mock.joins().len(), 7, "the remaining two, none repeated");
        })
        .await;
    }

    #[tokio::test]
    async fn a_session_joined_in_export_mode_is_booked_in_the_same_check() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(unjoined_response()).await;
            mock.set_sessions_after_join(one_session_response());
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            Settings::update(|s| s.octopus_sessions_auto_join = true).unwrap();
            check_once(&state, utc(EXPORT_NOW)).await;
            let events = session_events();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].source, "saving_session:9");
        })
        .await;
    }

    #[tokio::test]
    async fn an_unconfirmed_join_books_no_export() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(unjoined_response()).await;
            let state = configure_export(&mock, DeviceType::Gen3Hybrid).await;
            Settings::update(|s| s.octopus_sessions_auto_join = true).unwrap();
            check_once(&state, utc(EXPORT_NOW)).await;
            assert!(session_events().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn the_mode_endpoint_sets_auto_join_and_leaves_it_alone_when_absent() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_session_response()).await;
            let state = configure(&mock, SessionsMode::Alert).await;
            let request = |auto_join| {
                Json(ModeRequest {
                    mode: SessionsMode::Alert,
                    export_slot: None,
                    export_floor_soc: None,
                    auto_join,
                })
            };
            let (status, _) = set_sessions_mode(State(state.clone()), request(Some(true))).await;
            assert_eq!(status, StatusCode::OK);
            assert!(Settings::load().octopus_sessions_auto_join);
            let _ = set_sessions_mode(State(state.clone()), request(None)).await;
            assert!(
                Settings::load().octopus_sessions_auto_join,
                "absent means unchanged"
            );
            let _ = set_sessions_mode(State(state), request(Some(false))).await;
            assert!(!Settings::load().octopus_sessions_auto_join);
        })
        .await;
    }

    #[tokio::test]
    async fn a_join_that_octopus_is_slow_to_show_is_confirmed_by_reading_again() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(unjoined_response()).await;
            mock.set_sessions_after_join_lagging(one_session_response(), 1);
            let state = configure_auto_join(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;

            assert_eq!(mock.joins().len(), 1, "read again, not sent again");
            let current = state.octopus_sessions.lock().await.clone();
            assert_eq!(current.sessions.len(), 1);
            assert!(current.join_errors.is_empty());
            let alerts = mock.alerts();
            assert_eq!(alerts.len(), 1, "no false failure alert");
            assert!(alerts[0].body.contains("HEM joined this session for you."));
        })
        .await;
    }

    #[tokio::test]
    async fn a_session_joined_elsewhere_is_not_credited_to_hem() {
        with_isolated_config_dir_async(|| async {
            // HEM's request is refused (the user got there first in the app),
            // and the account shows the session joined all the same.
            let mock = MockOctopus::spawn(unjoined_response()).await;
            mock.set_join_answer(json!({ "data": { "joinSavingSessionsEvent": null },
                "errors": [{ "message": "Already joined" }] }));
            mock.set_sessions_after_join(one_session_response());
            let state = configure_auto_join(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;

            let alerts = mock.alerts();
            assert_eq!(alerts.len(), 1);
            assert!(!alerts[0].body.contains("HEM joined this session for you"));
            assert!(alerts[0].body.contains("Saving Session joined"));
            assert!(state.octopus_sessions.lock().await.join_errors.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_session_the_user_skipped_is_not_joined() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(unjoined_response()).await;
            let state = configure_auto_join(&mock, SessionsMode::Alert).await;
            Settings::update(|s| s.octopus_sessions_skipped = vec![9]).unwrap();
            check_once(&state, utc(NOW)).await;
            assert!(mock.joins().is_empty());
            assert!(Settings::load().octopus_sessions_join_attempted.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_login_that_fails_joins_nothing_and_uses_up_no_attempt() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(unjoined_response()).await;
            *mock.token_body.lock().unwrap() =
                json!({ "errors": [{ "message": "Authentication failed" }] }).to_string();
            let state = configure_auto_join(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;
            assert!(mock.joins().is_empty());
            assert!(Settings::load().octopus_sessions_join_attempted.is_empty());

            // Once the login works again the session is still joined.
            *mock.token_body.lock().unwrap() =
                json!({ "data": { "obtainKrakenToken": { "token": "jwt-abc" } } }).to_string();
            check_once(&state, utc("2026-10-17T12:10:00+00:00")).await;
            assert_eq!(mock.joins().len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn a_failed_join_is_still_shown_after_a_restart() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(unjoined_response()).await;
            let state = configure_auto_join(&mock, SessionsMode::Alert).await;
            // A previous run tried this session; its reason was only in memory.
            Settings::update(|s| s.octopus_sessions_join_attempted = vec![9]).unwrap();
            check_once(&state, utc(NOW)).await;

            assert!(mock.joins().is_empty(), "never retried");
            let (_, Json(body)) = get_sessions(State(state)).await;
            assert_eq!(body["join_failures"][0]["id"], 9);
            assert!(body["join_failures"][0]["error"]
                .as_str()
                .unwrap()
                .contains("tried to join this earlier"));
        })
        .await;
    }

    #[tokio::test]
    async fn several_refused_joins_send_one_message() {
        with_isolated_config_dir_async(|| async {
            let offers: Vec<Value> = (1..=3)
                .map(|n| {
                    event(
                        n,
                        "TURN_DOWN",
                        &format!("2026-10-{:02}T17:00:00+00:00", 18 + n),
                        &format!("2026-10-{:02}T18:00:00+00:00", 18 + n),
                        73,
                    )
                })
                .collect();
            let mock = MockOctopus::spawn(response(json!([]), json!(offers))).await;
            mock.set_join_answer(json!({ "data": { "joinSavingSessionsEvent": null },
                "errors": [{ "message": "Account locked" }] }));
            let state = configure_auto_join(&mock, SessionsMode::Alert).await;
            check_once(&state, utc(NOW)).await;

            assert_eq!(mock.joins().len(), 3);
            let alerts = mock.alerts();
            assert_eq!(alerts.len(), 1, "one message for all three");
            assert_eq!(alerts[0].body.matches("Account locked").count(), 3);
            assert!(alerts[0].body.contains("could not join"));
        })
        .await;
    }
}
