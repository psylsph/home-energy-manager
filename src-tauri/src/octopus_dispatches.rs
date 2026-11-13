//! Intelligent Octopus Go smart-charging dispatches (issue #394).
//!
//! On Intelligent Octopus Go, Octopus schedules the car's charging and can
//! start it outside the 23:30–05:30 window. The house load jumps, the battery
//! covers it, and the car ends up charging from the battery instead of the
//! cheap grid. With the hold on, HEM reads the account's planned dispatches
//! and pauses battery discharge while a smart one is running.
//!
//! Everything here fails soft. A failed or stale read means "no dispatch is
//! known", and the battery behaves as normal; HEM never guesses. Boost
//! (bump) charges are billed at the peak rate, so the battery covering one
//! is what the user wants and they are never held.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::history::dispatch_log::DispatchSighting;
use crate::history::OffPeakHalfHours;
use crate::inverter::poll::AppState;
use crate::octopus_sessions::{endpoints, obtain_token, post_graphql};
use crate::settings::Settings;

/// How often dispatches are read. Octopus allows about five dispatch queries
/// a minute; this stays far below that.
const POLL_INTERVAL: StdDuration = StdDuration::from_secs(3 * 60);

/// How long after its last sighting a logged dispatch counts as still
/// running: one check, as one that vanished was cancelled before the next.
pub const LOG_SEEN_GAP_SECS: i64 = POLL_INTERVAL.as_secs() as i64;

/// A dispatch list older than this is not trusted to hold the battery.
pub const STALE_AFTER_MINUTES: i64 = 15;
/// How often the account's car and charger list is re-read.
const DEVICE_REFRESH_HOURS: i64 = 6;
/// How long an access token is reused. Octopus tokens last an hour.
const TOKEN_REUSE_MINUTES: i64 = 50;
/// The longest a restart keeps a hold it found active while waiting for the
/// first dispatch check, so a check that never finishes cannot hold the
/// battery for ever.
const RESTART_HOLD_MINUTES: i64 = 5;

const DEVICES_QUERY: &str = "query($account: String!) { devices(accountNumber: $account) { \
    id deviceType status { current } } }";
const DISPATCHES_QUERY: &str =
    "query($device: String!) { flexPlannedDispatches(deviceId: $device) { \
    start end type energyAddedKwh } }";

/// One planned dispatch for the car or charger.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Dispatch {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// Octopus's dispatch type (`SMART`, `BOOST`), when given.
    pub kind: Option<String>,
    pub energy_added_kwh: Option<f64>,
}

impl Dispatch {
    /// Whether this is an Octopus-scheduled smart charge, billed at the
    /// off-peak rate. An unlabelled dispatch counts as smart, as it does for
    /// Octopus; a boost or anything unrecognised does not.
    pub fn is_smart(&self) -> bool {
        is_smart_kind(self.kind.as_deref())
    }

    /// Whether this dispatch holds the battery at `time`: a smart one that
    /// has started and not yet ended. The poll loop's hold and the planner's
    /// model of it (issue #406) both use this, so they can't disagree.
    pub fn holds_at(&self, time: DateTime<Utc>) -> bool {
        self.is_smart() && self.start <= time && time < self.end
    }
}

/// What the planner gets from the dispatch list (issue #406). The default
/// is no dispatches, as with the hold off.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlanningDispatches {
    /// Dispatches not yet over, from a list fresh enough to trust.
    pub dispatches: Vec<Dispatch>,
    /// When that list was read; none unless it is fresh.
    pub checked_at: Option<DateTime<Utc>>,
    /// The hold is on but the list is missing or too old to plan with.
    pub stale: bool,
    /// Whether the poll loop would hold the battery for these dispatches.
    /// It doesn't during a calibration run, so the car runs from the
    /// battery then.
    pub hold_applies: bool,
    /// The SOC the battery is topped up to during a held dispatch (issue
    /// #434), when that is on and the hold applies.
    pub charge_to_soc: Option<f64>,
    /// [`DispatchesState::revision`] when this was read (issue #404).
    pub revision: u64,
}

/// An Octopus access token. Its `Debug` output is redacted so logging the
/// state never leaks it.
#[derive(Clone)]
struct Token(String);

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(redacted)")
    }
}

/// [`Dispatch::is_smart`] for a bare type, as the dispatch log stores it.
pub fn is_smart_kind(kind: Option<&str>) -> bool {
    kind.is_none_or(|kind| matches!(kind.to_ascii_lowercase().as_str(), "smart" | "smart-charge"))
}

/// How long a smart charge that has left the list is remembered for live
/// rate stamping: past the end of the half-hour it finished in.
const RAN_SMART_KEEP_SECS: i64 = 2 * 3600;

/// What the last check found.
#[derive(Debug, Clone, Default)]
pub struct DispatchesState {
    /// When Octopus last answered successfully.
    pub checked_at: Option<DateTime<Utc>>,
    /// Why the latest check failed, if it did. The dispatches below are
    /// then the last good ones, trusted until they go stale.
    pub error: Option<String>,
    /// Smart-charging cars and chargers on the account.
    pub devices: Vec<String>,
    devices_checked_at: Option<DateTime<Utc>>,
    pub dispatches: Vec<Dispatch>,
    /// Reused between checks, with when it was obtained.
    token: Option<(Token, DateTime<Utc>)>,
    /// Bumped when the hold is switched, so a check that was already asking
    /// Octopus cannot bring back a list the user has just turned off.
    generation: u64,
    /// After a restart mid-hold: keep holding until this time or until the
    /// first check finishes, whichever is first.
    restart_hold_until: Option<DateTime<Utc>>,
    /// Why the last dispatch log write failed, so it's reported once.
    pub log_error: Option<String>,
    /// Smart charges that have run and since left the list, `(start, end)`
    /// Unix seconds, cut to the check that stopped listing them. Octopus
    /// drops a dispatch once it's over, but the rest of its half-hour is
    /// still billed off-peak (issue #434).
    ran_smart: Vec<(i64, i64)>,
    /// Moves whenever the planned charges change (issue #404): on the first
    /// good list, when a charge's start, end or type changes, and when the
    /// hold is switched. Never reused, so a plan that recorded one can tell
    /// whether the list it was made with is still the list.
    revision: u64,
}

impl DispatchesState {
    /// Whether a smart dispatch is running now, by a list fresh enough to
    /// trust. Unknown means no.
    pub fn hold_now(&self, now: DateTime<Utc>) -> bool {
        self.fresh(now)
            && self
                .dispatches
                .iter()
                .any(|dispatch| dispatch.holds_at(now))
    }

    /// The half-hours billed off-peak by a smart charge, for stamping the
    /// minute recorded at `now` (issue #434): the fresh list's smart
    /// dispatches, and those that ran and have since left it. A stale list
    /// counts for nothing, so an unknown means the normal rate.
    pub fn off_peak_half_hours(&self, now: DateTime<Utc>) -> OffPeakHalfHours {
        let listed = self
            .dispatches
            .iter()
            .filter(|_| self.fresh(now))
            .filter(|dispatch| dispatch.is_smart())
            .map(|dispatch| (dispatch.start.timestamp(), dispatch.end.timestamp()));
        OffPeakHalfHours::from_spans(listed.chain(self.ran_smart.iter().copied()))
    }

    /// Take a fresh list checked at `now`, remembering the smart charges
    /// the old one had running that the new one no longer lists: each ran
    /// until its planned end or `now`, whichever is first.
    fn replace_list(&mut self, fresh: Vec<Dispatch>, now: DateTime<Utc>) {
        let now_ts = now.timestamp();
        for old in &self.dispatches {
            let gone = !fresh
                .iter()
                .any(|new| new.start == old.start && new.is_smart() == old.is_smart());
            if gone && old.is_smart() && old.start <= now {
                let end = old.end.min(now).timestamp();
                if end > old.start.timestamp() {
                    self.ran_smart.push((old.start.timestamp(), end));
                }
            }
        }
        // Absolute age, so a clock stepped either way drops them.
        self.ran_smart
            .retain(|(_, end)| (now_ts - end).abs() <= RAN_SMART_KEEP_SECS);
        self.dispatches = fresh;
    }

    /// Whether the list is recent enough to act on. Absolute age: a check
    /// stamped in the future (the clock stepped back) says nothing about now
    /// either.
    pub(crate) fn fresh(&self, now: DateTime<Utc>) -> bool {
        self.checked_at
            .is_some_and(|checked| (now - checked).abs() <= Duration::minutes(STALE_AFTER_MINUTES))
    }

    /// After a restart that found a hold active, keep holding until the first
    /// check has finished (whether it worked or failed): without a list yet,
    /// the hold would restore Eco and re-pause up to a check interval later.
    pub fn hold_until_first_check(&mut self, now: DateTime<Utc>) {
        self.restart_hold_until = Some(now + Duration::minutes(RESTART_HOLD_MINUTES));
    }

    /// Whether the hold is being kept for a restart that is still waiting for
    /// its first check. Absolute distance: a clock stepped far back must not
    /// stretch the wait.
    pub fn holding_for_restart(&self, now: DateTime<Utc>) -> bool {
        self.restart_hold_until.is_some_and(|until| {
            now < until && (until - now) <= Duration::minutes(RESTART_HOLD_MINUTES)
        })
    }

    /// The dispatches the planner may use (issue #406): the ones not yet
    /// over, from a fresh list, while the hold is wanted. Otherwise none;
    /// `stale` says a wanted list couldn't be trusted. The hold sits out a
    /// calibration run, so `calibrating` clears `hold_applies`.
    pub fn for_planning(
        &self,
        now: DateTime<Utc>,
        wanted: bool,
        calibrating: bool,
    ) -> PlanningDispatches {
        if !wanted {
            return PlanningDispatches {
                revision: self.revision,
                ..PlanningDispatches::default()
            };
        }
        if !self.fresh(now) {
            return PlanningDispatches {
                stale: true,
                revision: self.revision,
                ..PlanningDispatches::default()
            };
        }
        PlanningDispatches {
            charge_to_soc: None,
            dispatches: self
                .dispatches
                .iter()
                .filter(|dispatch| dispatch.end > now)
                .cloned()
                .collect(),
            checked_at: self.checked_at,
            stale: false,
            hold_applies: !calibrating,
            revision: self.revision,
        }
    }

    /// Forget everything, as when the hold is turned off.
    pub(crate) fn reset(&mut self) {
        *self = Self {
            generation: self.generation.wrapping_add(1),
            revision: self.revision.wrapping_add(1),
            ..Self::default()
        };
    }
}

/// Smart-charging devices from the `devices` response: live cars and chargers.
pub fn parse_devices(response: &Value) -> Result<Vec<String>, String> {
    graphql_errors(response)?;
    let devices = response
        .pointer("/data/devices")
        .and_then(Value::as_array)
        .ok_or_else(|| "Octopus returned an unexpected device list".to_string())?;
    Ok(devices
        .iter()
        .filter(|device| {
            device["deviceType"].as_str() == Some("ELECTRIC_VEHICLES")
                && device.pointer("/status/current").and_then(Value::as_str) == Some("LIVE")
        })
        .filter_map(|device| device["id"].as_str().map(str::to_string))
        .collect())
}

/// Planned dispatches from the `flexPlannedDispatches` response. An error or
/// a malformed entry fails the whole read: a partial list could miss the
/// dispatch that is running.
pub fn parse_dispatches(response: &Value) -> Result<Vec<Dispatch>, String> {
    graphql_errors(response)?;
    let unexpected = || "Octopus returned an unexpected dispatch list".to_string();
    let data = response
        .get("data")
        .filter(|data| data.get("flexPlannedDispatches").is_some())
        .ok_or_else(unexpected)?;
    let items = match &data["flexPlannedDispatches"] {
        Value::Null => return Ok(Vec::new()),
        Value::Array(items) => items,
        _ => return Err(unexpected()),
    };
    items
        .iter()
        .map(|item| {
            let time = |field: &str| {
                item[field]
                    .as_str()
                    .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
                    .map(|time| time.with_timezone(&Utc))
            };
            let (start, end) = match (time("start"), time("end")) {
                (Some(start), Some(end)) if start < end => (start, end),
                _ => return Err(unexpected()),
            };
            // Octopus signs the energy the car draws as negative (an import),
            // so the energy added is its size.
            let energy_added_kwh = match &item["energyAddedKwh"] {
                Value::Number(number) => number.as_f64(),
                Value::String(text) => text.trim().parse::<f64>().ok(),
                _ => None,
            }
            .map(f64::abs);
            Ok(Dispatch {
                start,
                end,
                kind: item["type"].as_str().map(str::to_string),
                energy_added_kwh,
            })
        })
        .collect()
}

/// A GraphQL error response as one message, with Octopus's error codes so a
/// known failure such as KT-CT-4340 can be recognised on the page.
fn graphql_errors(response: &Value) -> Result<(), String> {
    let Some(errors) = response
        .get("errors")
        .and_then(Value::as_array)
        .filter(|errors| !errors.is_empty())
    else {
        return Ok(());
    };
    let message = errors
        .iter()
        .map(|error| {
            let text = error["message"].as_str().unwrap_or("unknown error");
            match error
                .pointer("/extensions/errorCode")
                .and_then(Value::as_str)
            {
                Some(code) => format!("{text} ({code})"),
                None => text.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    Err(format!("Octopus returned an error: {message}"))
}

// ---------------------------------------------------------------------------
// Octopus requests
// ---------------------------------------------------------------------------

/// What one check needs from the last one.
struct Known {
    token: Option<String>,
    devices: Option<Vec<String>>,
    /// The last device list, used when a due refresh fails. Empty when
    /// there is none to fall back on.
    previous_devices: Vec<String>,
}

/// What one check found. `token` is set when a new one was obtained and
/// `devices` when the list was re-read.
struct Fetched {
    token: Option<String>,
    devices: Option<Vec<String>>,
    /// With the car or charger each is for.
    dispatches: Vec<(String, Dispatch)>,
}

/// Where the device and dispatch queries go: the public Kraken API, the same
/// place the token comes from. The backend API Saving Sessions uses does not
/// have `devices` or `flexPlannedDispatches`.
fn query_url(settings: &Settings) -> String {
    endpoints(settings).0
}

/// Octopus's "Unable to fetch planned dispatches": seen failing about one
/// read in six and working on the next (10 November 2026).
const PASSING_DISPATCH_ERROR: &str = "KT-CT-4340";

/// How long to wait before the one retry of [`PASSING_DISPATCH_ERROR`].
#[cfg(not(test))]
const DISPATCH_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(test)]
const DISPATCH_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(10);

/// One car or charger's planned dispatches, retried once after
/// [`PASSING_DISPATCH_ERROR`]. Nothing else is retried: a rate limit or a
/// refused token only gets worse with another request. Blocking.
fn read_device_dispatches(
    query_url: &str,
    token: &str,
    device: &str,
) -> Result<Vec<Dispatch>, String> {
    let read = || {
        post_graphql(
            query_url,
            Some(token),
            &json!({ "query": DISPATCHES_QUERY, "variables": { "device": device } }),
        )
        .and_then(|response| parse_dispatches(&response))
    };
    match read() {
        Err(error) if error.contains(PASSING_DISPATCH_ERROR) => {
            tracing::debug!("Intelligent Octopus dispatches: {error}; retrying once");
            std::thread::sleep(DISPATCH_RETRY_DELAY);
            read()
        }
        result => result,
    }
}

fn fetch_blocking(settings: &Settings, known: Known) -> Result<Fetched, String> {
    let query_url = query_url(settings);
    let (token, new_token) = match known.token {
        Some(token) => (token, None),
        None => {
            let token = obtain_token(settings)?;
            (token.clone(), Some(token))
        }
    };
    let (devices, new_devices) = match known.devices {
        Some(devices) => (devices, None),
        None => {
            let read = post_graphql(
                &query_url,
                Some(&token),
                &json!({
                    "query": DEVICES_QUERY,
                    "variables": { "account": settings.octopus_account_number.trim() },
                }),
            )
            .and_then(|response| parse_devices(&response));
            match read {
                Ok(devices) => (devices.clone(), Some(devices)),
                // The cars on an account rarely change, so a failed
                // refresh (Octopus's KT-CT-4360, say) keeps the last list
                // rather than stopping the hold. It is retried next check.
                Err(error) if !known.previous_devices.is_empty() => {
                    tracing::debug!("Intelligent Octopus devices: {error}; using the last list");
                    (known.previous_devices, None)
                }
                Err(error) => return Err(error),
            }
        }
    };
    let mut dispatches = Vec::new();
    for device in &devices {
        dispatches.extend(
            read_device_dispatches(&query_url, &token, device)?
                .into_iter()
                .map(|dispatch| (device.clone(), dispatch)),
        );
    }
    dispatches.sort_by_key(|(_, dispatch)| dispatch.start);
    Ok(Fetched {
        token: new_token,
        devices: new_devices,
        dispatches,
    })
}

/// Whether the hold should be checking at all.
pub(crate) fn wanted(settings: &Settings) -> bool {
    settings.octopus_dispatch_hold && crate::octopus::configured(settings)
}

/// [`DispatchesState::for_planning`] for the live state and settings.
pub async fn planning_dispatches(
    state: &AppState,
    settings: &Settings,
    now: DateTime<Utc>,
) -> PlanningDispatches {
    // The poll loop's hold sits out a calibration run.
    let calibrating = state.calibration_run.lock().await.is_some();
    let mut planning =
        state
            .octopus_dispatches
            .lock()
            .await
            .for_planning(now, wanted(settings), calibrating);
    planning.charge_to_soc = planning_charge_to_soc(&planning, settings);
    planning
}

/// The SOC the Planner should expect a held dispatch to top the battery up
/// to (issue #434): only with the top-up on and the hold applying, and not
/// while Cosy or Agile automation owns the charge slots, which keeps the
/// top-up from ever starting. Owners that come and go (Adaptive Charge's
/// period, a charge slot of the user's, Force Charge) aren't predicted: a
/// slot running then charges the battery itself.
pub fn planning_charge_to_soc(planning: &PlanningDispatches, settings: &Settings) -> Option<f64> {
    (planning.hold_applies
        && settings.octopus_dispatch_charge
        && !crate::inverter::dispatch_charge::tariff_automation_owns_slots(settings))
    .then(|| {
        let cap = &settings.charge_cap;
        f64::from(crate::inverter::dispatch_charge::target_soc(
            cap.enabled.then_some(cap.cap_soc),
        ))
    })
}

/// The log line for a freshly read dispatch list (issue #431), so a log
/// shows what Octopus had planned whenever the hold did or didn't start.
/// `previous` is the last good list, or none on the first good check, which
/// always logs a baseline. Charges that have already ended by `now` count on
/// neither side: Octopus dropping a finished one (or listing it one check
/// longer) is not a change, so a steady list stays quiet. After that it logs
/// only when a charge's start, end or type changes: the order and the energy
/// figure (which Octopus updates as the car charges) don't count.
fn planned_change<Tz: chrono::TimeZone>(
    previous: Option<&[Dispatch]>,
    fresh: &[Dispatch],
    now: DateTime<Utc>,
    tz: &Tz,
) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    let key = |list: &[Dispatch]| {
        let mut key: Vec<_> = list
            .iter()
            .filter(|d| d.end > now)
            .map(|d| (d.start, d.end, d.kind.clone()))
            .collect();
        key.sort();
        key
    };
    let heading = match previous {
        None => "Intelligent Octopus planned charges",
        Some(previous) if key(previous) == key(fresh) => return None,
        Some(_) => "Intelligent Octopus planned charges changed",
    };
    let mut sorted: Vec<&Dispatch> = fresh.iter().filter(|d| d.end > now).collect();
    sorted.sort_by_key(|d| (d.start, d.end));
    let described: Vec<String> = sorted
        .into_iter()
        .map(|d| {
            let kind = match d.kind.as_deref() {
                None => "smart (unlabelled)".to_string(),
                Some(kind) => kind.to_ascii_lowercase(),
            };
            let start = d.start.with_timezone(tz);
            let end = d.end.with_timezone(tz);
            let end_format = if end.date_naive() == start.date_naive() {
                "%H:%M"
            } else {
                "%-d %b %H:%M"
            };
            let energy = d
                .energy_added_kwh
                .map(|kwh| format!(" ({kwh:.1} kWh)"))
                .unwrap_or_default();
            format!(
                "{kind} {}\u{2013}{}{energy}",
                start.format("%-d %b %H:%M"),
                end.format(end_format)
            )
        })
        .collect();
    let body = if described.is_empty() {
        "none".to_string()
    } else {
        described.join("; ")
    };
    Some(format!("{heading}: {body}"))
}

/// A change in a charge's planned energy that the planner should see
/// (issue #404): the car's load is sized from it. Smaller changes, as
/// Octopus updates the figure while the car charges, aren't.
const PLAN_ENERGY_CHANGE_KWH: f64 = 1.0;

/// Whether the fresh list changes what the planner models: the first good
/// list, a charge's start, end or type ([`planned_change`]'s key), or its
/// planned energy by [`PLAN_ENERGY_CHANGE_KWH`]. Charges already over count
/// on neither side.
fn plan_changed(previous: Option<&[Dispatch]>, fresh: &[Dispatch], now: DateTime<Utc>) -> bool {
    let Some(previous) = previous else {
        return true;
    };
    let key = |list: &[Dispatch]| {
        let mut key: Vec<_> = list
            .iter()
            .filter(|d| d.end > now)
            .map(|d| (d.start, d.end, d.kind.clone(), d.energy_added_kwh))
            .collect();
        key.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
        key
    };
    let (before, after) = (key(previous), key(fresh));
    before.len() != after.len()
        || before.iter().zip(&after).any(|(old, new)| {
            (old.0, old.1, &old.2) != (new.0, new.1, &new.2)
                || match (old.3, new.3) {
                    (Some(a), Some(b)) => (a - b).abs() >= PLAN_ENERGY_CHANGE_KWH,
                    (None, None) => false,
                    _ => true,
                }
        })
}

/// One pass: read the dispatches and publish them.
pub async fn check_once(state: &Arc<AppState>, now: DateTime<Utc>) {
    // Read the generation before the settings: a switch after this point
    // bumps it, and the answer below is then dropped.
    let (generation, known) = {
        let current = state.octopus_dispatches.lock().await;
        let token = current
            .token
            .as_ref()
            .filter(|(_, at)| (now - *at).abs() < Duration::minutes(TOKEN_REUSE_MINUTES))
            .map(|(token, _)| token.0.clone());
        let devices = current
            .devices_checked_at
            .filter(|at| (now - *at).abs() < Duration::hours(DEVICE_REFRESH_HOURS))
            .map(|_| current.devices.clone());
        let previous_devices = if devices.is_none() {
            current.devices.clone()
        } else {
            Vec::new()
        };
        (
            current.generation,
            Known {
                token,
                devices,
                previous_devices,
            },
        )
    };
    let settings = Settings::load_async().await;
    if !wanted(&settings) {
        let mut current = state.octopus_dispatches.lock().await;
        // A hold that is off has nothing to wait for after a restart.
        current.restart_hold_until = None;
        if current.checked_at.is_some() || current.error.is_some() || current.token.is_some() {
            current.reset();
        }
        return;
    }
    let fetched = tokio::task::spawn_blocking(move || fetch_blocking(&settings, known))
        .await
        .map_err(|error| format!("Dispatch check failed: {error}"))
        .and_then(|result| result);

    let mut current = state.octopus_dispatches.lock().await;
    if current.generation != generation {
        return;
    }
    // The first check has finished, worked or not: from here the list (or
    // its absence) decides, as it does in normal running.
    current.restart_hold_until = None;
    let mut sightings = None;
    match fetched {
        Ok(fetched) => {
            sightings = Some(
                fetched
                    .dispatches
                    .iter()
                    .map(|(device, dispatch)| DispatchSighting {
                        device: device.clone(),
                        start: dispatch.start.timestamp(),
                        end: dispatch.end.timestamp(),
                        kind: dispatch.kind.clone(),
                        energy_kwh: dispatch.energy_added_kwh,
                    })
                    .collect::<Vec<_>>(),
            );
            if let Some(token) = fetched.token {
                current.token = Some((Token(token), now));
            }
            if let Some(devices) = fetched.devices {
                current.devices = devices;
                current.devices_checked_at = Some(now);
            }
            let fresh: Vec<Dispatch> = fetched
                .dispatches
                .into_iter()
                .map(|(_, dispatch)| dispatch)
                .collect();
            // A baseline on the first good check since start-up or since the
            // hold was switched on (both leave `checked_at` empty), then only
            // the changes.
            let previous = current.checked_at.map(|_| current.dispatches.as_slice());
            if let Some(line) = planned_change(previous, &fresh, now, &chrono::Local) {
                tracing::info!("{line}");
            }
            if plan_changed(previous, &fresh, now) {
                current.revision = current.revision.wrapping_add(1);
            }
            current.replace_list(fresh, now);
            current.checked_at = Some(now);
            current.error = None;
        }
        Err(error) => {
            if current.error.as_deref() != Some(error.as_str()) {
                tracing::warn!("Intelligent Octopus dispatches: {error}");
            }
            // The token may be what failed; get a fresh one next time.
            current.token = None;
            current.error = Some(error);
        }
    }
    // The hold is decided; the log is written after, off the lock.
    drop(current);
    let Some(sightings) = sightings else {
        return;
    };
    let seen_at = now.timestamp();
    write_log(state, move |db| db.log_dispatches(&sightings, seen_at)).await;
}

/// Run a dispatch log write on the blocking pool, reporting a failure once
/// per change of error. It never affects the hold. Returns whether it was
/// written; with no history database there's nothing to write.
async fn write_log<F>(state: &AppState, write: F) -> bool
where
    F: FnOnce(&crate::history::HistoryDb) -> Result<(), String> + Send + 'static,
{
    let Some(db) = state.history.lock().await.clone() else {
        return false;
    };
    let result = tokio::task::spawn_blocking(move || write(&db))
        .await
        .map_err(|error| format!("dispatch log worker failed: {error}"))
        .and_then(|result| result);
    let mut current = state.octopus_dispatches.lock().await;
    match result {
        Ok(()) => {
            current.log_error = None;
            true
        }
        Err(error) => {
            if current.log_error.as_deref() != Some(error.as_str()) {
                tracing::warn!("Intelligent Octopus dispatch log: {error}");
            }
            current.log_error = Some(error);
            false
        }
    }
}

/// Background loop. Wakes early when the hold is switched.
pub async fn run_dispatches_loop(state: Arc<AppState>) {
    loop {
        check_once(&state, Utc::now()).await;
        tokio::select! {
            () = tokio::time::sleep(POLL_INTERVAL) => {}
            () = state.octopus_dispatches_wake.notified() => {}
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

/// GET /api/octopus/dispatches
pub async fn get_dispatches(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let settings = Settings::load_async().await;
    let current = state.octopus_dispatches.lock().await.clone();
    // Only while holding: a restore still being retried is not a charge.
    let hold_state = state.dispatch_hold_state.lock().await.clone();
    let holding = hold_state == crate::inverter::poll::DispatchHoldState::Holding;
    // The inverter isn't taking the pause (issue #431), or an AIO hold has
    // had no usable reading to save and so never started.
    let hold_failing = {
        let retry = state.dispatch_hold_retry.lock().await;
        (holding && retry.failing())
            || (hold_state == crate::inverter::poll::DispatchHoldState::Idle
                && retry.cannot_start())
    };
    // A smart charge the hold leaves alone because a charge slot is charging
    // the battery, as the last poll decided.
    let hold_charge_slot = !holding
        && state
            .latest_snapshot
            .lock()
            .await
            .as_ref()
            .is_some_and(|snap| snap.dispatch_hold_charge_slot);
    let charge = state.dispatch_charge.lock().await.clone();
    // The run's own target while it runs; otherwise what one would use now.
    let charge_target_soc = charge.run.as_ref().map_or_else(
        || {
            let cap = &settings.charge_cap;
            crate::inverter::dispatch_charge::target_soc(cap.enabled.then_some(cap.cap_soc))
        },
        |run| run.target_soc,
    );
    let now = Utc::now();
    let dispatches: Vec<Value> = current
        .dispatches
        .iter()
        .filter(|dispatch| dispatch.end > now)
        .map(|dispatch| {
            json!({
                "start": dispatch.start,
                "end": dispatch.end,
                "type": dispatch.kind,
                "energy_added_kwh": dispatch.energy_added_kwh,
                "smart": dispatch.is_smart(),
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "enabled": settings.octopus_dispatch_hold,
            "configured": crate::octopus::configured(&settings),
            "checked_at": current.checked_at,
            "error": current.error,
            "devices": current.devices.len(),
            "holding": holding,
            "hold_failing": hold_failing,
            "hold_charge_slot": hold_charge_slot,
            "stale_after_minutes": STALE_AFTER_MINUTES,
            "dispatches": dispatches,
            "charge_enabled": settings.octopus_dispatch_charge,
            "charging": charge.charging(),
            "charge_state": charge.state_code(),
            "charge_reason": charge.reason.map(|reason| reason.code()),
            "charge_note": charge.reason.map(|reason| reason.message(charge_target_soc)),
            "charge_target_soc": charge_target_soc,
        })),
    )
}

/// Unknown fields are refused so a misspelled one is not silently dropped.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldRequest {
    enabled: bool,
}

/// POST /api/octopus/dispatches/hold — body `{ "enabled": true }`.
pub async fn set_hold(
    State(state): State<Arc<AppState>>,
    Json(body): Json<HoldRequest>,
) -> (StatusCode, Json<Value>) {
    let enabled = body.enabled;
    if let Err(error) = Settings::update_async(move |settings| {
        settings.octopus_dispatch_hold = enabled;
    })
    .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": format!("Could not save the setting: {error}") })),
        );
    }
    // Either way the old list goes: off must stop holding at the next poll,
    // and on starts from a fresh read.
    state.octopus_dispatches.lock().await.reset();
    state.octopus_dispatches_wake.notify_one();
    if !enabled {
        // Car charging isn't logged while the hold is off, so the log no
        // longer covers its window.
        write_log(&state, |db| db.forget_dispatch_log_since()).await;
    }
    (
        StatusCode::OK,
        Json(json!({ "ok": true, "enabled": enabled })),
    )
}

/// POST /api/octopus/dispatches/charge — body `{ "enabled": true }`. Tops
/// the battery up during smart charging (issue #434); refused while the hold
/// is off, since charging with discharge live would feed the car.
pub async fn set_charge(
    State(state): State<Arc<AppState>>,
    Json(body): Json<HoldRequest>,
) -> (StatusCode, Json<Value>) {
    let enabled = body.enabled;
    let saved = Settings::update_async(move |settings| {
        if enabled && !settings.octopus_dispatch_hold {
            return Err("Turn on the smart charging hold first");
        }
        settings.octopus_dispatch_charge = enabled;
        Ok(())
    })
    .await;
    match saved {
        Ok(Ok(())) => {}
        Ok(Err(refused)) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "ok": false, "error": refused })),
            )
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    json!({ "ok": false, "error": format!("Could not save the setting: {error}") }),
                ),
            )
        }
    }
    // Off is acted on by the next poll, which restores a running top-up.
    state.write_notify.notify_one();
    (
        StatusCode::OK,
        Json(json!({ "ok": true, "enabled": enabled })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::with_isolated_config_dir_async;
    use axum::extract::Request;

    fn utc(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    const NOW: &str = "2026-10-17T12:00:00+00:00";
    const KEY: &str = "sk_live_test_SECRETKEY";
    const CAR: &str = "00000000-0000-0000-0000-00000000c4r0";

    fn devices_response() -> Value {
        json!({ "data": { "devices": [
            { "id": CAR, "deviceType": "ELECTRIC_VEHICLES", "status": { "current": "LIVE" } },
            { "id": "battery", "deviceType": "BATTERIES", "status": { "current": "LIVE" } },
            { "id": "old-car", "deviceType": "ELECTRIC_VEHICLES", "status": { "current": "RETIRED" } },
        ] } })
    }

    fn dispatch(start: &str, end: &str, kind: Value, energy: Value) -> Value {
        json!({ "start": start, "end": end, "type": kind, "energyAddedKwh": energy })
    }

    fn dispatches_response(items: Value) -> Value {
        json!({ "data": { "flexPlannedDispatches": items } })
    }

    fn smart(start: &str, end: &str) -> Dispatch {
        Dispatch {
            start: utc(start),
            end: utc(end),
            kind: Some("SMART".to_string()),
            energy_added_kwh: None,
        }
    }

    fn checked(at: &str, dispatches: Vec<Dispatch>) -> DispatchesState {
        DispatchesState {
            checked_at: Some(utc(at)),
            dispatches,
            ..Default::default()
        }
    }

    // ---- parsing ----

    #[test]
    fn devices_keep_live_cars_and_chargers_only() {
        assert_eq!(
            parse_devices(&devices_response()),
            Ok(vec![CAR.to_string()])
        );
        assert_eq!(
            parse_devices(&json!({ "data": { "devices": [] } })),
            Ok(Vec::new())
        );
    }

    #[test]
    fn device_errors_and_unexpected_shapes_are_errors() {
        let error = parse_devices(&json!({
            "errors": [{ "message": "Unauthorized", "extensions": { "errorCode": "KT-CT-1139" } }]
        }))
        .unwrap_err();
        assert!(error.contains("Unauthorized"), "{error}");
        assert!(error.contains("KT-CT-1139"), "{error}");
        assert!(parse_devices(&json!({ "data": { "devices": null } })).is_err());
        assert!(parse_devices(&json!({})).is_err());
    }

    #[test]
    fn dispatches_read_times_type_and_energy() {
        let parsed = parse_dispatches(&dispatches_response(json!([
            dispatch(
                "2026-10-17T13:00:00+00:00",
                "2026-10-17T13:30:00+00:00",
                json!("SMART"),
                json!(3.5)
            ),
            // Octopus has sent the energy as a decimal string.
            dispatch(
                "2026-10-17T14:00:00+01:00",
                "2026-10-17T14:30:00+01:00",
                json!("BOOST"),
                json!("1.25")
            ),
            dispatch(
                "2026-10-17T15:00:00+00:00",
                "2026-10-17T15:30:00+00:00",
                Value::Null,
                Value::Null
            ),
        ])))
        .unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].start, utc("2026-10-17T13:00:00+00:00"));
        assert_eq!(parsed[0].end, utc("2026-10-17T13:30:00+00:00"));
        assert_eq!(parsed[0].kind.as_deref(), Some("SMART"));
        assert_eq!(parsed[0].energy_added_kwh, Some(3.5));
        assert_eq!(
            parsed[1].start,
            utc("2026-10-17T13:00:00+00:00"),
            "offset honoured"
        );
        assert_eq!(parsed[1].energy_added_kwh, Some(1.25));
        assert_eq!(parsed[2].kind, None);
        assert_eq!(parsed[2].energy_added_kwh, None);
    }

    #[test]
    fn energy_octopus_sends_as_a_negative_import_is_read_as_the_energy_added() {
        // A live account's list (2026-11-01): Octopus signs the energy the
        // car draws as negative. 3.642 kWh in half an hour is a 7.3 kW
        // charger, so the size is right and only the sign is backwards.
        let parsed = parse_dispatches(&dispatches_response(json!([
            dispatch(
                "2026-11-01T23:00:00+00:00",
                "2026-11-01T23:30:00+00:00",
                json!("SMART"),
                json!("-3.642")
            ),
            dispatch(
                "2026-11-02T03:30:00+00:00",
                "2026-11-02T04:30:00+00:00",
                json!("SMART"),
                json!(-7.284)
            ),
        ])))
        .unwrap();
        assert_eq!(parsed[0].energy_added_kwh, Some(3.642));
        assert_eq!(parsed[1].energy_added_kwh, Some(7.284));
    }

    #[test]
    fn a_failed_dispatch_read_is_an_error_not_an_empty_list() {
        // Octopus's intermittent KT-CT-4340 failure.
        let error = parse_dispatches(&json!({
            "errors": [{
                "message": "Unable to fetch planned dispatches.",
                "extensions": { "errorCode": "KT-CT-4340" }
            }],
            "data": { "flexPlannedDispatches": null }
        }))
        .unwrap_err();
        assert!(error.contains("KT-CT-4340"), "{error}");
    }

    #[test]
    fn no_dispatches_is_empty_but_a_malformed_list_is_an_error() {
        assert_eq!(
            parse_dispatches(&dispatches_response(Value::Null)),
            Ok(Vec::new())
        );
        assert_eq!(
            parse_dispatches(&dispatches_response(json!([]))),
            Ok(Vec::new())
        );
        assert!(parse_dispatches(&json!({ "data": {} })).is_err());
        assert!(parse_dispatches(&dispatches_response(json!({}))).is_err());
        assert!(parse_dispatches(&dispatches_response(json!([dispatch(
            "not a time",
            "2026-10-17T13:30:00+00:00",
            json!("SMART"),
            Value::Null
        )])))
        .is_err());
        // An end before its start is not a usable window.
        assert!(parse_dispatches(&dispatches_response(json!([dispatch(
            "2026-10-17T14:00:00+00:00",
            "2026-10-17T13:30:00+00:00",
            json!("SMART"),
            Value::Null
        )])))
        .is_err());
    }

    #[test]
    fn smart_and_unlabelled_dispatches_are_held_boosts_are_not() {
        let mut d = smart("2026-10-17T13:00:00+00:00", "2026-10-17T13:30:00+00:00");
        assert!(d.is_smart());
        d.kind = Some("smart-charge".to_string());
        assert!(d.is_smart());
        d.kind = None;
        assert!(d.is_smart());
        d.kind = Some("BOOST".to_string());
        assert!(!d.is_smart());
        d.kind = Some("bump-charge".to_string());
        assert!(!d.is_smart());
        d.kind = Some("SOMETHING_NEW".to_string());
        assert!(!d.is_smart());
    }

    // ---- hold decision ----

    #[test]
    fn holds_only_inside_a_smart_dispatch() {
        let state = checked(
            "2026-10-17T12:58:00+00:00",
            vec![smart(
                "2026-10-17T13:00:00+00:00",
                "2026-10-17T13:30:00+00:00",
            )],
        );
        assert!(!state.hold_now(utc("2026-10-17T12:59:59+00:00")));
        assert!(state.hold_now(utc("2026-10-17T13:00:00+00:00")));
        assert!(state.hold_now(utc("2026-10-17T13:10:00+00:00")));
        assert!(
            !state.hold_now(utc("2026-10-17T13:30:00+00:00")),
            "end is exclusive"
        );
    }

    #[test]
    fn a_boost_is_never_held() {
        let mut boost = smart("2026-10-17T13:00:00+00:00", "2026-10-17T13:30:00+00:00");
        boost.kind = Some("BOOST".to_string());
        let state = checked("2026-10-17T13:00:00+00:00", vec![boost]);
        assert!(!state.hold_now(utc("2026-10-17T13:10:00+00:00")));
    }

    #[test]
    fn a_dispatch_holds_from_its_start_to_just_before_its_end_unless_a_boost() {
        // The planner (issue #406) asks this of each dispatch on its own,
        // apart from the list's freshness.
        let dispatch = smart("2026-10-17T13:00:00+00:00", "2026-10-17T13:30:00+00:00");
        assert!(!dispatch.holds_at(utc("2026-10-17T12:59:59+00:00")));
        assert!(dispatch.holds_at(utc("2026-10-17T13:00:00+00:00")));
        assert!(dispatch.holds_at(utc("2026-10-17T13:29:59+00:00")));
        assert!(!dispatch.holds_at(utc("2026-10-17T13:30:00+00:00")));

        let unlabelled = Dispatch {
            kind: None,
            ..dispatch.clone()
        };
        assert!(unlabelled.holds_at(utc("2026-10-17T13:10:00+00:00")));
        let boost = Dispatch {
            kind: Some("BOOST".to_string()),
            ..dispatch
        };
        assert!(!boost.holds_at(utc("2026-10-17T13:10:00+00:00")));
    }

    #[test]
    fn the_planner_gets_nothing_while_the_hold_is_off() {
        let state = checked(
            "2026-10-17T13:00:00+00:00",
            vec![smart(
                "2026-10-17T19:00:00+00:00",
                "2026-10-17T20:00:00+00:00",
            )],
        );
        assert_eq!(
            state.for_planning(utc("2026-10-17T13:05:00+00:00"), false, false),
            PlanningDispatches::default()
        );
    }

    #[test]
    fn the_planner_gets_the_dispatches_not_yet_over_from_a_fresh_list() {
        let over = smart("2026-10-17T11:00:00+00:00", "2026-10-17T13:00:00+00:00");
        let running = smart("2026-10-17T12:30:00+00:00", "2026-10-17T13:30:00+00:00");
        let later = smart("2026-10-17T19:00:00+00:00", "2026-10-17T20:00:00+00:00");
        let state = checked(
            "2026-10-17T13:00:00+00:00",
            vec![over, running.clone(), later.clone()],
        );
        let got = state.for_planning(utc("2026-10-17T13:05:00+00:00"), true, false);
        assert_eq!(got.dispatches, vec![running, later]);
        assert_eq!(got.checked_at, Some(utc("2026-10-17T13:00:00+00:00")));
        assert!(!got.stale);
    }

    #[test]
    fn the_planner_is_told_the_hold_sits_out_a_calibration_run() {
        let later = smart("2026-10-17T19:00:00+00:00", "2026-10-17T20:00:00+00:00");
        let state = checked("2026-10-17T13:00:00+00:00", vec![later.clone()]);
        let now = utc("2026-10-17T13:05:00+00:00");
        assert!(state.for_planning(now, true, false).hold_applies);
        let calibrating = state.for_planning(now, true, true);
        assert!(!calibrating.hold_applies);
        assert_eq!(calibrating.dispatches, vec![later], "the car still charges");
    }

    #[test]
    fn a_wanted_list_that_is_missing_or_old_is_stale_and_gives_nothing() {
        let later = smart("2026-10-17T19:00:00+00:00", "2026-10-17T20:00:00+00:00");
        let never = DispatchesState {
            dispatches: vec![later.clone()],
            ..Default::default()
        };
        let got = never.for_planning(utc("2026-10-17T13:00:00+00:00"), true, false);
        assert!(got.stale && got.dispatches.is_empty() && got.checked_at.is_none());

        let state = checked("2026-10-17T13:00:00+00:00", vec![later]);
        for now in ["2026-10-17T13:15:01+00:00", "2026-10-17T12:44:59+00:00"] {
            let got = state.for_planning(utc(now), true, false);
            assert!(got.stale, "{now}");
            assert!(got.dispatches.is_empty(), "{now}");
            assert_eq!(got.checked_at, None, "{now}");
        }
    }

    #[test]
    fn a_list_is_fresh_for_fifteen_minutes_either_side_of_its_check() {
        let state = checked("2026-10-17T13:00:00+00:00", Vec::new());
        assert!(state.fresh(utc("2026-10-17T13:15:00+00:00")));
        assert!(!state.fresh(utc("2026-10-17T13:15:01+00:00")));
        assert!(state.fresh(utc("2026-10-17T12:45:00+00:00")));
        assert!(
            !state.fresh(utc("2026-10-17T12:44:59+00:00")),
            "a check stamped in the future is no evidence about now"
        );
        assert!(!DispatchesState::default().fresh(utc("2026-10-17T13:00:00+00:00")));
    }

    #[test]
    fn an_unknown_or_stale_list_holds_nothing() {
        let window = smart("2026-10-17T13:00:00+00:00", "2026-10-17T15:00:00+00:00");
        let never_checked = DispatchesState {
            dispatches: vec![window.clone()],
            ..Default::default()
        };
        assert!(!never_checked.hold_now(utc("2026-10-17T13:10:00+00:00")));

        let state = checked("2026-10-17T13:00:00+00:00", vec![window]);
        assert!(
            state.hold_now(utc("2026-10-17T13:15:00+00:00")),
            "15 min old is fresh"
        );
        assert!(
            !state.hold_now(utc("2026-10-17T13:15:01+00:00")),
            "older is stale"
        );
        // The clock stepped back: a check stamped well in the future is not
        // evidence about now.
        assert!(!state.hold_now(utc("2026-10-17T12:40:00+00:00")));
    }

    #[test]
    fn a_failed_check_keeps_trusting_the_last_good_list_until_it_is_stale() {
        let mut state = checked(
            "2026-10-17T13:00:00+00:00",
            vec![smart(
                "2026-10-17T13:00:00+00:00",
                "2026-10-17T14:00:00+00:00",
            )],
        );
        state.error = Some("Octopus returned an error: KT-CT-4340".to_string());
        assert!(state.hold_now(utc("2026-10-17T13:06:00+00:00")));
        assert!(!state.hold_now(utc("2026-10-17T13:20:00+00:00")));
    }

    // ---- check loop against a mock Octopus ----

    #[derive(Clone, Debug)]
    struct Seen {
        auth: Option<String>,
        body: String,
    }

    struct MockOctopus {
        base: String,
        seen: Arc<std::sync::Mutex<Vec<Seen>>>,
        devices_body: Arc<std::sync::Mutex<String>>,
        dispatches_body: Arc<std::sync::Mutex<String>>,
        /// Milliseconds the dispatch query waits before answering.
        dispatches_delay_ms: Arc<std::sync::atomic::AtomicU64>,
        /// One-off answers to the next dispatch queries, before the usual one.
        dispatches_queued: Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
        _shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl MockOctopus {
        async fn spawn(dispatches: Value) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            let devices_body = Arc::new(std::sync::Mutex::new(devices_response().to_string()));
            let dispatches_body = Arc::new(std::sync::Mutex::new(dispatches.to_string()));
            let dispatches_delay_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let dispatches_queued = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::<
                String,
            >::new()));
            let (s2, v2, b2, d2, q2) = (
                seen.clone(),
                devices_body.clone(),
                dispatches_body.clone(),
                dispatches_delay_ms.clone(),
                dispatches_queued.clone(),
            );
            let app = axum::Router::new().fallback(move |req: Request| {
                let (s2, v2, b2, d2, q2) =
                    (s2.clone(), v2.clone(), b2.clone(), d2.clone(), q2.clone());
                async move {
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
                        auth,
                        body: body.clone(),
                    });
                    if body.contains("obtainKrakenToken") {
                        let token =
                            json!({ "data": { "obtainKrakenToken": { "token": "jwt-abc" } } });
                        (StatusCode::OK, token.to_string())
                    } else if body.contains("flexPlannedDispatches") {
                        let delay = d2.load(std::sync::atomic::Ordering::Relaxed);
                        if delay > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        }
                        let queued: Option<String> = q2.lock().unwrap().pop_front();
                        (
                            StatusCode::OK,
                            queued.unwrap_or_else(|| b2.lock().unwrap().clone()),
                        )
                    } else if body.contains("devices") {
                        (StatusCode::OK, v2.lock().unwrap().clone())
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
                devices_body,
                dispatches_body,
                dispatches_delay_ms,
                dispatches_queued,
                _shutdown: Some(tx),
            }
        }

        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }

        fn count(&self, needle: &str) -> usize {
            self.seen()
                .iter()
                .filter(|seen| seen.body.contains(needle))
                .count()
        }

        fn set_dispatches(&self, body: Value) {
            *self.dispatches_body.lock().unwrap() = body.to_string();
        }

        /// Answer the next dispatch queries with these, in order.
        fn queue_dispatches(&self, bodies: &[Value]) {
            self.dispatches_queued
                .lock()
                .unwrap()
                .extend(bodies.iter().map(Value::to_string));
        }

        fn set_devices(&self, body: Value) {
            *self.devices_body.lock().unwrap() = body.to_string();
        }
    }

    impl Drop for MockOctopus {
        fn drop(&mut self) {
            if let Some(tx) = self._shutdown.take() {
                let _ = tx.send(());
            }
        }
    }

    fn one_dispatch() -> Value {
        dispatches_response(json!([dispatch(
            "2026-10-17T12:00:00+00:00",
            "2026-10-17T13:00:00+00:00",
            json!("SMART"),
            json!(4.2)
        )]))
    }

    async fn configure(mock: &MockOctopus, hold: bool) -> Arc<AppState> {
        Settings::update(|s| {
            s.octopus_api_key = KEY.to_string();
            s.octopus_account_number = "A-TEST1".to_string();
            s.octopus_api_base_url = mock.base.clone();
            s.octopus_dispatch_hold = hold;
        })
        .unwrap();
        Arc::new(AppState::new())
    }

    #[tokio::test]
    async fn a_check_reads_the_devices_then_their_dispatches() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            check_once(&state, utc(NOW)).await;

            let seen = mock.seen();
            assert_eq!(seen.len(), 3, "token, devices, one car's dispatches");
            assert!(seen[0].body.contains("obtainKrakenToken"));
            assert!(seen[0].body.contains(KEY));
            assert_eq!(seen[0].auth, None);
            assert!(seen[1].body.contains("A-TEST1"));
            assert_eq!(seen[1].auth.as_deref(), Some("jwt-abc"));
            assert!(seen[2].body.contains(CAR));
            assert!(!seen[2].body.contains("old-car"));
            assert!(!seen[1].body.contains(KEY) && !seen[2].body.contains(KEY));

            let current = state.octopus_dispatches.lock().await.clone();
            assert_eq!(current.checked_at, Some(utc(NOW)));
            assert_eq!(current.error, None);
            assert_eq!(current.devices, vec![CAR.to_string()]);
            assert_eq!(current.dispatches.len(), 1);
            assert!(current.hold_now(utc("2026-10-17T12:10:00+00:00")));
        })
        .await;
    }

    #[tokio::test]
    async fn later_checks_reuse_the_token_and_the_device_list() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            check_once(&state, utc(NOW)).await;
            check_once(&state, utc("2026-10-17T12:03:00+00:00")).await;
            assert_eq!(mock.count("obtainKrakenToken"), 1);
            assert_eq!(mock.count("devices("), 1);
            assert_eq!(mock.count("flexPlannedDispatches"), 2);

            // An hour on the token is renewed; six hours on, so is the list.
            check_once(&state, utc("2026-10-17T13:00:00+00:00")).await;
            assert_eq!(mock.count("obtainKrakenToken"), 2);
            assert_eq!(mock.count("devices("), 1);
            check_once(&state, utc("2026-10-17T18:00:00+00:00")).await;
            assert_eq!(mock.count("devices("), 2);
        })
        .await;
    }

    /// The list's revision (issue #404) moves exactly when what Octopus
    /// plans changes, so the replan can tell "changed since the plan" from
    /// one number, read with the list it describes.
    #[tokio::test]
    async fn the_revision_moves_only_when_the_planned_charges_change() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            let revision = |state: Arc<AppState>, at: &'static str| async move {
                planning_dispatches(&state, &Settings::load(), utc(at))
                    .await
                    .revision
            };
            let before = revision(state.clone(), NOW).await;

            // The first good list is a change: the plan had none.
            check_once(&state, utc(NOW)).await;
            let first = revision(state.clone(), NOW).await;
            assert_ne!(first, before);

            // The same charge, its energy updated a little as the car
            // charges: no change.
            mock.set_dispatches(dispatches_response(json!([dispatch(
                "2026-10-17T12:00:00+00:00",
                "2026-10-17T13:00:00+00:00",
                json!("SMART"),
                json!(4.9)
            )])));
            check_once(&state, utc("2026-10-17T12:03:00+00:00")).await;
            assert_eq!(
                revision(state.clone(), "2026-10-17T12:03:00+00:00").await,
                first
            );

            // Planned energy up by a kWh or more at the same times: the car's
            // load the plan models changed.
            mock.set_dispatches(dispatches_response(json!([dispatch(
                "2026-10-17T12:00:00+00:00",
                "2026-10-17T13:00:00+00:00",
                json!("SMART"),
                json!(9.0)
            )])));
            check_once(&state, utc("2026-10-17T12:04:00+00:00")).await;
            let more_energy = revision(state.clone(), "2026-10-17T12:04:00+00:00").await;
            assert_ne!(more_energy, first);
            let first = more_energy;

            // A failed check is not a change either.
            mock.set_dispatches(json!({ "errors": [{ "message": "boom" }] }));
            check_once(&state, utc("2026-10-17T12:06:00+00:00")).await;
            assert!(state.octopus_dispatches.lock().await.error.is_some());
            assert_eq!(
                revision(state.clone(), "2026-10-17T12:06:00+00:00").await,
                first
            );

            // Octopus moves the charge: a change.
            mock.set_dispatches(dispatches_response(json!([dispatch(
                "2026-10-17T14:00:00+00:00",
                "2026-10-17T15:00:00+00:00",
                json!("SMART"),
                json!(4.2)
            )])));
            check_once(&state, utc("2026-10-17T12:09:00+00:00")).await;
            let moved = revision(state.clone(), "2026-10-17T12:09:00+00:00").await;
            assert_ne!(moved, first);

            // Switching the hold off and on starts a new list, which must
            // not reuse an old number a plan may have recorded.
            state.octopus_dispatches.lock().await.reset();
            let after_reset = revision(state.clone(), "2026-10-17T12:10:00+00:00").await;
            assert_ne!(after_reset, moved);
            check_once(&state, utc("2026-10-17T12:12:00+00:00")).await;
            let relisted = revision(state.clone(), "2026-10-17T12:12:00+00:00").await;
            assert!(relisted != moved && relisted != first && relisted != after_reset);
        })
        .await;
    }

    fn graphql_error(code: &str, message: &str) -> Value {
        json!({
            "errors": [{ "message": message, "extensions": { "errorCode": code } }],
            "data": { "flexPlannedDispatches": null }
        })
    }

    /// Seen live (10 November 2026): about one dispatch read in six fails
    /// with KT-CT-4340 and the next works, so one quick retry saves the
    /// check. On a fresh start that's the difference between a list and
    /// three minutes of "out of date".
    #[tokio::test]
    async fn a_passing_dispatch_read_failure_is_retried_once() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            mock.queue_dispatches(&[graphql_error(
                "KT-CT-4340",
                "Unable to fetch planned dispatches.",
            )]);
            check_once(&state, utc(NOW)).await;
            assert_eq!(mock.count("flexPlannedDispatches"), 2);
            let current = state.octopus_dispatches.lock().await.clone();
            assert_eq!(current.error, None);
            assert_eq!(current.checked_at, Some(utc(NOW)));
            assert_eq!(current.dispatches.len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn a_dispatch_read_failing_twice_is_reported_without_more_retries() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            let flake = graphql_error("KT-CT-4340", "Unable to fetch planned dispatches.");
            mock.queue_dispatches(&[flake.clone(), flake]);
            check_once(&state, utc(NOW)).await;
            assert_eq!(mock.count("flexPlannedDispatches"), 2);
            let error = state.octopus_dispatches.lock().await.error.clone();
            assert!(error.is_some_and(|e| e.contains("KT-CT-4340")));
        })
        .await;
    }

    #[tokio::test]
    async fn other_dispatch_read_errors_are_not_retried() {
        // A rate limit (seen live after a burst of reads) only gets worse
        // with another request.
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            mock.queue_dispatches(&[graphql_error("KT-CT-1199", "Too many requests.")]);
            check_once(&state, utc(NOW)).await;
            assert_eq!(mock.count("flexPlannedDispatches"), 1);
            let error = state.octopus_dispatches.lock().await.error.clone();
            assert!(error.is_some_and(|e| e.contains("KT-CT-1199")));
        })
        .await;
    }

    fn device_status_error() -> Value {
        // Seen live after switching the hold off and on (2026-11-01).
        json!({
            "errors": [{ "message": "Device status could not be fetched.",
                         "extensions": { "errorCode": "KT-CT-4360" } }],
            "data": { "devices": null }
        })
    }

    #[tokio::test]
    async fn a_failed_device_refresh_keeps_reading_dispatches_for_the_known_devices() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            check_once(&state, utc(NOW)).await;

            // Six hours on the list is re-read, and Octopus fails it. The
            // cars on an account rarely change, so the old list still reads
            // the dispatches and the hold keeps working.
            mock.set_devices(device_status_error());
            let later = utc("2026-10-17T18:00:00+00:00");
            check_once(&state, later).await;
            assert_eq!(mock.count("flexPlannedDispatches"), 2);
            let current = state.octopus_dispatches.lock().await.clone();
            assert_eq!(current.error, None);
            assert_eq!(current.checked_at, Some(later));
            assert_eq!(current.devices, vec![CAR.to_string()]);

            // The refresh is retried at the next check until it works.
            check_once(&state, utc("2026-10-17T18:03:00+00:00")).await;
            assert_eq!(mock.count("devices("), 3);
            mock.set_devices(devices_response());
            check_once(&state, utc("2026-10-17T18:06:00+00:00")).await;
            check_once(&state, utc("2026-10-17T18:09:00+00:00")).await;
            assert_eq!(mock.count("devices("), 4, "a good refresh is reused");
        })
        .await;
    }

    #[tokio::test]
    async fn a_failed_first_device_read_is_reported() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            mock.set_devices(device_status_error());
            let state = configure(&mock, true).await;
            check_once(&state, utc(NOW)).await;
            let current = state.octopus_dispatches.lock().await.clone();
            assert!(current.error.as_deref().unwrap().contains("KT-CT-4360"));
            assert_eq!(current.checked_at, None);
            assert_eq!(mock.count("flexPlannedDispatches"), 0);
        })
        .await;
    }

    #[tokio::test]
    async fn a_failed_check_reports_the_error_and_keeps_the_last_good_list() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            check_once(&state, utc(NOW)).await;

            mock.set_dispatches(json!({
                "errors": [{ "message": "Unable to fetch planned dispatches.",
                             "extensions": { "errorCode": "KT-CT-4340" } }],
                "data": { "flexPlannedDispatches": null }
            }));
            check_once(&state, utc("2026-10-17T12:03:00+00:00")).await;
            let current = state.octopus_dispatches.lock().await.clone();
            assert!(current.error.as_deref().unwrap().contains("KT-CT-4340"));
            assert_eq!(current.checked_at, Some(utc(NOW)), "last success kept");
            assert_eq!(current.dispatches.len(), 1);
            assert!(current.hold_now(utc("2026-10-17T12:05:00+00:00")));
            assert!(!current.hold_now(utc("2026-10-17T12:16:00+00:00")));

            // A failure drops the cached token: it may be what went wrong.
            check_once(&state, utc("2026-10-17T12:06:00+00:00")).await;
            assert_eq!(mock.count("obtainKrakenToken"), 2);

            mock.set_dispatches(one_dispatch());
            check_once(&state, utc("2026-10-17T12:09:00+00:00")).await;
            assert_eq!(state.octopus_dispatches.lock().await.error, None);
        })
        .await;
    }

    #[tokio::test]
    async fn an_unreachable_octopus_is_reported_without_the_key() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            Settings::update(|s| s.octopus_api_base_url = "http://127.0.0.1:1".to_string())
                .unwrap();
            check_once(&state, utc(NOW)).await;
            let current = state.octopus_dispatches.lock().await.clone();
            let error = current.error.clone().unwrap();
            assert!(error.contains("request failed"), "{error}");
            assert!(!error.contains(KEY));
            assert!(!current.hold_now(utc(NOW)));
        })
        .await;
    }

    #[tokio::test]
    async fn an_account_without_a_smart_charging_car_holds_nothing() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            *mock.devices_body.lock().unwrap() = json!({ "data": { "devices": [] } }).to_string();
            let state = configure(&mock, true).await;
            check_once(&state, utc(NOW)).await;
            assert_eq!(mock.count("flexPlannedDispatches"), 0);
            let current = state.octopus_dispatches.lock().await.clone();
            assert_eq!(current.checked_at, Some(utc(NOW)));
            assert!(current.devices.is_empty());
            assert!(current.dispatches.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn the_hold_off_makes_no_requests_and_forgets_the_list() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            check_once(&state, utc(NOW)).await;
            let before = mock.seen().len();

            Settings::update(|s| s.octopus_dispatch_hold = false).unwrap();
            check_once(&state, utc("2026-10-17T12:03:00+00:00")).await;
            assert_eq!(mock.seen().len(), before, "no further requests");
            let current = state.octopus_dispatches.lock().await.clone();
            assert!(current.dispatches.is_empty());
            assert_eq!(current.checked_at, None);
        })
        .await;
    }

    #[tokio::test]
    async fn turning_the_hold_off_during_a_slow_check_discards_its_answer() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            mock.dispatches_delay_ms
                .store(400, std::sync::atomic::Ordering::Relaxed);
            let state = configure(&mock, true).await;

            let checking = tokio::spawn({
                let state = state.clone();
                async move { check_once(&state, utc(NOW)).await }
            });
            // Wait until the check is waiting on the dispatch query.
            for _ in 0..200 {
                if mock.count("flexPlannedDispatches") > 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            assert_eq!(mock.count("flexPlannedDispatches"), 1);
            let (status, _) =
                set_hold(State(state.clone()), Json(HoldRequest { enabled: false })).await;
            assert_eq!(status, StatusCode::OK);
            checking.await.unwrap();

            let current = state.octopus_dispatches.lock().await.clone();
            assert!(current.dispatches.is_empty(), "the late answer is dropped");
            assert!(!current.hold_now(utc(NOW)));
        })
        .await;
    }

    // ---- the dispatch log (issue #406) ----

    fn log_db(name: &str) -> (Arc<crate::history::HistoryDb>, std::path::PathBuf) {
        let path = crate::test_util::make_unique_test_dir(name).join("h.db");
        (
            Arc::new(crate::history::HistoryDb::open(&path).unwrap()),
            path,
        )
    }

    fn logged(db: &crate::history::HistoryDb) -> Vec<crate::history::dispatch_log::LoggedDispatch> {
        db.logged_dispatches_since(0, LOG_SEEN_GAP_SECS).unwrap()
    }

    #[tokio::test]
    async fn a_check_logs_the_dispatches_that_have_started() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(dispatches_response(json!([
                dispatch(
                    "2026-10-17T11:30:00+00:00",
                    "2026-10-17T12:30:00+00:00",
                    json!("SMART"),
                    json!("-6.0")
                ),
                dispatch(
                    "2026-10-17T14:00:00+00:00",
                    "2026-10-17T15:00:00+00:00",
                    json!("SMART"),
                    json!("-7.0")
                ),
            ])))
            .await;
            let state = configure(&mock, true).await;
            let (db, _path) = log_db("dispatch-log-check");
            *state.history.lock().await = Some(db.clone());
            check_once(&state, utc(NOW)).await;
            assert_eq!(mock.seen().len(), 3, "no extra Octopus requests");

            // The running one, so far: 11:30 to one check after 12:00.
            let start = utc("2026-10-17T11:30:00+00:00").timestamp();
            let ran_until = utc("2026-10-17T12:03:00+00:00").timestamp();
            let rows = logged(&db);
            assert_eq!(rows.len(), 1, "the 14:00 one hasn't started: {rows:?}");
            assert_eq!(rows[0].device, CAR);
            assert_eq!((rows[0].start, rows[0].end), (start, ran_until));
            assert_eq!(rows[0].kind.as_deref(), Some("SMART"));
            assert_eq!(rows[0].energy_kwh, Some(6.0 * 33.0 / 60.0));
            assert_eq!(db.dispatch_log_since(), Some(utc(NOW).timestamp()));

            // The next check carries it on.
            check_once(&state, utc("2026-10-17T12:15:00+00:00")).await;
            let rows = logged(&db);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].end, utc("2026-10-17T12:18:00+00:00").timestamp());
        })
        .await;
    }

    #[tokio::test]
    async fn a_log_that_cannot_be_written_leaves_the_hold_alone() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            let (db, path) = log_db("dispatch-log-broken");
            rusqlite::Connection::open(&path)
                .unwrap()
                .execute_batch("DROP TABLE IF EXISTS octopus_dispatch_log")
                .unwrap();
            *state.history.lock().await = Some(db);
            for at in [NOW, "2026-10-17T12:03:00+00:00"] {
                check_once(&state, utc(at)).await;
                let current = state.octopus_dispatches.lock().await.clone();
                assert_eq!(current.checked_at, Some(utc(at)));
                assert_eq!(current.error, None);
                assert_eq!(current.dispatches.len(), 1);
                assert!(current.hold_now(utc(at)));
                let error = current.log_error.clone().unwrap();
                assert!(error.contains("octopus_dispatch_log"), "{error}");
            }
        })
        .await;
    }

    #[tokio::test]
    async fn turning_the_hold_off_forgets_when_the_log_started() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            let (db, _path) = log_db("dispatch-log-off");
            *state.history.lock().await = Some(db.clone());
            check_once(&state, utc(NOW)).await;
            assert!(db.dispatch_log_since().is_some());

            // Car charging while the hold is off isn't logged, so the log
            // no longer covers its window.
            let (status, _) =
                set_hold(State(state.clone()), Json(HoldRequest { enabled: false })).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(db.dispatch_log_since(), None);
            assert_eq!(logged(&db).len(), 1, "what ran stays logged");
            check_once(&state, utc("2026-10-17T12:03:00+00:00")).await;
            assert_eq!(db.dispatch_log_since(), None);

            let (status, _) =
                set_hold(State(state.clone()), Json(HoldRequest { enabled: true })).await;
            assert_eq!(status, StatusCode::OK);
            check_once(&state, utc("2026-10-17T12:06:00+00:00")).await;
            assert_eq!(
                db.dispatch_log_since(),
                Some(utc("2026-10-17T12:06:00+00:00").timestamp())
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_gap_in_the_checks_starts_the_log_again() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            let (db, _path) = log_db("dispatch-log-gap");
            *state.history.lock().await = Some(db.clone());
            check_once(&state, utc(NOW)).await;
            check_once(&state, utc("2026-10-17T12:03:00+00:00")).await;
            assert_eq!(db.dispatch_log_since(), Some(utc(NOW).timestamp()));

            // Octopus fails for three-quarters of an hour: a car could have
            // charged unseen in that time.
            mock.set_dispatches(json!({
                "errors": [{ "message": "Unable to fetch planned dispatches.",
                             "extensions": { "errorCode": "KT-CT-4340" } }],
                "data": { "flexPlannedDispatches": null }
            }));
            for minute in (6..=45).step_by(3) {
                let at = utc(NOW) + Duration::minutes(minute);
                check_once(&state, at).await;
            }
            assert_eq!(db.dispatch_log_since(), Some(utc(NOW).timestamp()));
            mock.set_dispatches(one_dispatch());
            let back = utc("2026-10-17T12:48:00+00:00");
            check_once(&state, back).await;
            assert_eq!(db.dispatch_log_since(), Some(back.timestamp()));

            // The hold switched off in the settings file, not through the
            // page, for an hour: also a gap.
            Settings::update(|s| s.octopus_dispatch_hold = false).unwrap();
            check_once(&state, utc("2026-10-17T12:51:00+00:00")).await;
            Settings::update(|s| s.octopus_dispatch_hold = true).unwrap();
            let on = utc("2026-10-17T13:51:00+00:00");
            check_once(&state, on).await;
            assert_eq!(db.dispatch_log_since(), Some(on.timestamp()));
        })
        .await;
    }

    #[tokio::test]
    async fn octopus_failing_from_start_up_starts_the_log_again() {
        // A restarted HEM whose first checks fail for six hours records the
        // house all that time; a car could have charged unseen in it.
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let (db, path) = log_db("dispatch-log-start-up");
            {
                let state = configure(&mock, true).await;
                *state.history.lock().await = Some(db.clone());
                check_once(&state, utc(NOW)).await;
            }
            mock.set_dispatches(json!({
                "errors": [{ "message": "Unable to fetch planned dispatches.",
                             "extensions": { "errorCode": "KT-CT-4340" } }],
                "data": { "flexPlannedDispatches": null }
            }));
            drop(db);
            let db = Arc::new(crate::history::HistoryDb::open(&path).unwrap());
            let state = configure(&mock, true).await;
            *state.history.lock().await = Some(db.clone());
            let restarted = utc(NOW) + Duration::minutes(10);
            for minute in (0..=360).step_by(3) {
                check_once(&state, restarted + Duration::minutes(minute)).await;
            }
            assert_eq!(db.dispatch_log_since(), Some(utc(NOW).timestamp()));
            mock.set_dispatches(one_dispatch());
            let back = restarted + Duration::minutes(363);
            check_once(&state, back).await;
            assert_eq!(db.dispatch_log_since(), Some(back.timestamp()));
        })
        .await;
    }

    #[tokio::test]
    async fn an_answer_dropped_by_a_hold_switch_is_not_logged() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            mock.dispatches_delay_ms
                .store(400, std::sync::atomic::Ordering::Relaxed);
            let state = configure(&mock, true).await;
            let (db, _path) = log_db("dispatch-log-dropped");
            *state.history.lock().await = Some(db.clone());

            let checking = tokio::spawn({
                let state = state.clone();
                async move { check_once(&state, utc(NOW)).await }
            });
            for _ in 0..200 {
                if mock.count("flexPlannedDispatches") > 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            let (status, _) =
                set_hold(State(state.clone()), Json(HoldRequest { enabled: false })).await;
            assert_eq!(status, StatusCode::OK);
            checking.await.unwrap();
            assert!(logged(&db).is_empty());
            assert_eq!(db.dispatch_log_since(), None);
        })
        .await;
    }

    #[tokio::test]
    async fn the_handlers_report_the_list_and_switch_the_hold() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, false).await;

            let (status, Json(body)) = get_dispatches(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["ok"], true);
            assert_eq!(body["enabled"], false);
            assert_eq!(body["configured"], true);
            assert_eq!(body["holding"], false);
            assert!(body["dispatches"].as_array().unwrap().is_empty());

            let (status, Json(reply)) =
                set_hold(State(state.clone()), Json(HoldRequest { enabled: true })).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(reply["enabled"], true);
            assert!(Settings::load().octopus_dispatch_hold);
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                state.octopus_dispatches_wake.notified(),
            )
            .await
            .expect("the loop is woken so the change applies straight away");

            mock.set_dispatches(dispatches_response(json!([
                // Already over: not shown.
                dispatch(
                    "2000-01-01T00:00:00+00:00",
                    "2000-01-01T01:00:00+00:00",
                    json!("SMART"),
                    Value::Null
                ),
                dispatch(
                    "2099-01-01T01:00:00+00:00",
                    "2099-01-01T02:00:00+00:00",
                    json!("BOOST"),
                    json!(2.0)
                ),
                dispatch(
                    "2099-01-01T00:00:00+00:00",
                    "2099-01-01T00:30:00+00:00",
                    json!("SMART"),
                    json!(1.5)
                ),
            ])));
            check_once(&state, Utc::now()).await;
            *state.dispatch_hold_state.lock().await =
                crate::inverter::poll::DispatchHoldState::Holding;
            let (_, Json(body)) = get_dispatches(State(state.clone())).await;
            assert_eq!(body["enabled"], true);
            assert_eq!(body["devices"], 1);
            assert_eq!(body["holding"], true);
            assert!(body["checked_at"].is_string());
            assert_eq!(body["error"], Value::Null);
            let listed = body["dispatches"].as_array().unwrap();
            assert_eq!(listed.len(), 2);
            assert_eq!(
                listed[0]["start"], "2099-01-01T00:00:00Z",
                "sorted by start"
            );
            assert_eq!(listed[0]["smart"], true);
            assert_eq!(listed[0]["energy_added_kwh"], 1.5);
            assert_eq!(listed[1]["type"], "BOOST");
            assert_eq!(listed[1]["smart"], false);

            let (status, _) =
                set_hold(State(state.clone()), Json(HoldRequest { enabled: false })).await;
            assert_eq!(status, StatusCode::OK);
            assert!(!Settings::load().octopus_dispatch_hold);
            let (_, Json(body)) = get_dispatches(State(state.clone())).await;
            assert!(body["dispatches"].as_array().unwrap().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn the_page_says_holding_only_while_the_hold_is_holding() {
        use crate::inverter::poll::DispatchHoldState;
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            for (hold, expected) in [
                (DispatchHoldState::Idle, false),
                (DispatchHoldState::Holding, true),
                // Releasing is the restore being retried: the charge has
                // ended, so the page must not say the car is being held for.
                (DispatchHoldState::Releasing, false),
            ] {
                *state.dispatch_hold_state.lock().await = hold.clone();
                let (_, Json(body)) = get_dispatches(State(state.clone())).await;
                assert_eq!(body["holding"], expected, "{hold:?}");
            }
        })
        .await;
    }

    #[tokio::test]
    async fn the_page_says_when_the_hold_leaves_it_to_a_charge_slot() {
        use crate::inverter::poll::DispatchHoldState;
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let (_, Json(body)) = get_dispatches(State(state.clone())).await;
            assert_eq!(body["hold_charge_slot"], false, "no snapshot yet");

            *state.latest_snapshot.lock().await = Some(crate::inverter::model::InverterSnapshot {
                dispatch_hold_charge_slot: true,
                ..Default::default()
            });
            let (_, Json(body)) = get_dispatches(State(state.clone())).await;
            assert_eq!(body["hold_charge_slot"], true);
            assert_eq!(body["holding"], false);

            // Holding wins: the page never says both.
            *state.dispatch_hold_state.lock().await = DispatchHoldState::Holding;
            let (_, Json(body)) = get_dispatches(State(state.clone())).await;
            assert_eq!(body["hold_charge_slot"], false);
            assert_eq!(body["holding"], true);
        })
        .await;
    }

    #[tokio::test]
    async fn the_page_says_when_the_hold_cannot_pause_the_battery() {
        // #431: the AIO wouldn't take the pause, and nothing told the user.
        use crate::inverter::poll::{DispatchHoldRetry, DispatchHoldState};
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let (_, Json(body)) = get_dispatches(State(state.clone())).await;
            assert_eq!(body["hold_failing"], false);

            *state.dispatch_hold_retry.lock().await = DispatchHoldRetry {
                failing: true,
                ..Default::default()
            };
            for (hold, expected) in [
                (DispatchHoldState::Holding, true),
                (DispatchHoldState::Releasing, false),
                (DispatchHoldState::Idle, false),
            ] {
                *state.dispatch_hold_state.lock().await = hold.clone();
                let (_, Json(body)) = get_dispatches(State(state.clone())).await;
                assert_eq!(body["hold_failing"], expected, "{hold:?}");
            }
        })
        .await;
    }

    #[tokio::test]
    async fn the_page_says_when_an_aio_hold_cannot_start_for_want_of_a_reading() {
        // Third code review #4: the hold couldn't save HR 318-320, so it never
        // started; it stays Idle but must not look like nothing is happening.
        use crate::inverter::poll::{DispatchHoldRetry, DispatchHoldState};
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            *state.dispatch_hold_state.lock().await = DispatchHoldState::Idle;
            *state.dispatch_hold_retry.lock().await = DispatchHoldRetry {
                failing: true,
                unread_since: Some(1_800_000_000),
                ..Default::default()
            };
            let (_, Json(body)) = get_dispatches(State(state.clone())).await;
            assert_eq!(body["hold_failing"], true);
            assert_eq!(body["holding"], false);
        })
        .await;
    }

    // ---- A restart mid-hold (review of v0.94.0, finding 4) ----

    #[test]
    fn a_restart_keeps_holding_until_the_first_check_or_the_cap() {
        let mut state = DispatchesState::default();
        assert!(!state.holding_for_restart(utc(NOW)), "nothing pending");

        state.hold_until_first_check(utc(NOW));
        assert!(state.holding_for_restart(utc(NOW)));
        assert!(state.holding_for_restart(utc("2026-10-17T12:04:00+00:00")));
        // A check that never finishes (a stuck task) cannot hold the battery
        // for ever.
        assert!(!state.holding_for_restart(utc("2026-10-17T12:06:00+00:00")));
        // A clock that stepped far back does not extend the hold either.
        assert!(!state.holding_for_restart(utc("2026-10-17T10:00:00+00:00")));
    }

    #[test]
    fn turning_the_hold_off_ends_the_restart_hold() {
        let mut state = DispatchesState::default();
        state.hold_until_first_check(utc(NOW));
        state.reset();
        assert!(!state.holding_for_restart(utc(NOW)));
    }

    #[tokio::test]
    async fn a_good_check_ends_the_restart_hold() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            state
                .octopus_dispatches
                .lock()
                .await
                .hold_until_first_check(utc(NOW));
            check_once(&state, utc(NOW)).await;
            let current = state.octopus_dispatches.lock().await.clone();
            assert!(!current.holding_for_restart(utc(NOW)));
            assert!(current.hold_now(utc(NOW)), "the list now decides");
        })
        .await;
    }

    #[tokio::test]
    async fn a_failed_check_also_ends_the_restart_hold() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            *mock.devices_body.lock().unwrap() =
                json!({ "errors": [{ "message": "KT-CT-4340" }] }).to_string();
            let state = configure(&mock, true).await;
            state
                .octopus_dispatches
                .lock()
                .await
                .hold_until_first_check(utc(NOW));
            check_once(&state, utc(NOW)).await;
            let current = state.octopus_dispatches.lock().await.clone();
            assert!(current.error.is_some(), "the check did fail");
            assert!(!current.holding_for_restart(utc(NOW)));
            assert!(!current.hold_now(utc(NOW)));
        })
        .await;
    }

    #[tokio::test]
    async fn a_hold_switched_off_does_not_wait_for_a_check() {
        with_isolated_config_dir_async(|| async {
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, false).await;
            state
                .octopus_dispatches
                .lock()
                .await
                .hold_until_first_check(utc(NOW));
            check_once(&state, utc(NOW)).await;
            assert!(!state
                .octopus_dispatches
                .lock()
                .await
                .holding_for_restart(utc(NOW)));
            assert_eq!(mock.seen().len(), 0, "and no request is made");
        })
        .await;
    }

    // ---- logging what Octopus planned (issue #431) ----

    fn planned(start: &str, end: &str, kind: Option<&str>, energy: Option<f64>) -> Dispatch {
        Dispatch {
            start: utc(start),
            end: utc(end),
            kind: kind.map(str::to_string),
            energy_added_kwh: energy,
        }
    }

    /// A time before every dispatch in these tests.
    fn early() -> DateTime<Utc> {
        utc("2026-01-01T00:00:00Z")
    }

    #[test]
    fn a_charge_that_finishes_and_leaves_the_list_is_not_a_change() {
        let done = planned(
            "2026-11-09T23:30:00Z",
            "2026-11-10T05:30:00Z",
            Some("SMART"),
            None,
        );
        let next = planned(
            "2026-11-10T23:30:00Z",
            "2026-11-11T05:30:00Z",
            Some("SMART"),
            None,
        );
        let after = utc("2026-11-10T05:33:00Z");
        // Octopus drops it once it's over, or lists it one check longer.
        assert_eq!(
            planned_change(Some(std::slice::from_ref(&done)), &[], after, &Utc),
            None
        );
        assert_eq!(
            planned_change(
                Some(&[done.clone(), next.clone()]),
                std::slice::from_ref(&next),
                after,
                &Utc
            ),
            None
        );
        assert_eq!(
            planned_change(
                Some(std::slice::from_ref(&done)),
                std::slice::from_ref(&done),
                after,
                &Utc
            ),
            None
        );
        // A charge still running counts, and the baseline leaves out what's over.
        let running = utc("2026-11-10T01:00:00Z");
        assert!(planned_change(Some(std::slice::from_ref(&done)), &[], running, &Utc).is_some());
        assert_eq!(
            planned_change(None, &[done, next], after, &Utc).as_deref(),
            Some("Intelligent Octopus planned charges: smart 10 Nov 23:30\u{2013}11 Nov 05:30")
        );
    }

    #[test]
    fn the_first_good_check_always_logs_what_is_planned() {
        assert_eq!(
            planned_change(None, &[], early(), &Utc).as_deref(),
            Some("Intelligent Octopus planned charges: none")
        );
        let list = [
            planned(
                "2026-11-09T23:30:00Z",
                "2026-11-10T05:30:00Z",
                Some("SMART"),
                Some(31.5),
            ),
            planned(
                "2026-11-09T18:00:00Z",
                "2026-11-09T18:30:00Z",
                Some("BOOST"),
                None,
            ),
        ];
        assert_eq!(
            planned_change(None, &list, early(), &Utc).as_deref(),
            Some(
                "Intelligent Octopus planned charges: boost 9 Nov 18:00\u{2013}18:30; \
                 smart 9 Nov 23:30\u{2013}10 Nov 05:30 (31.5 kWh)"
            ),
            "in start order, with the end's date when it falls on a later day"
        );
    }

    #[test]
    fn an_unlabelled_dispatch_is_logged_as_smart_like_the_hold_treats_it() {
        let list = [planned(
            "2026-11-09T01:00:00Z",
            "2026-11-09T02:00:00Z",
            None,
            None,
        )];
        assert_eq!(
            planned_change(None, &list, early(), &Utc).as_deref(),
            Some(
                "Intelligent Octopus planned charges: smart (unlabelled) 9 Nov 01:00\u{2013}02:00"
            )
        );
    }

    #[test]
    fn an_unchanged_list_logs_nothing_even_reordered_or_with_new_energy() {
        let a = planned(
            "2026-11-09T23:30:00Z",
            "2026-11-10T01:00:00Z",
            Some("SMART"),
            Some(5.0),
        );
        let b = planned(
            "2026-11-10T02:00:00Z",
            "2026-11-10T03:00:00Z",
            Some("SMART"),
            None,
        );
        let before = [a.clone(), b.clone()];
        let after = [
            b,
            Dispatch {
                energy_added_kwh: Some(7.5),
                ..a
            },
        ];
        assert_eq!(planned_change(Some(&before), &after, early(), &Utc), None);
        assert_eq!(planned_change(Some(&[]), &[], early(), &Utc), None);
    }

    #[test]
    fn a_moved_added_dropped_or_relabelled_dispatch_is_logged_as_a_change() {
        let base = planned(
            "2026-11-09T23:30:00Z",
            "2026-11-10T01:00:00Z",
            Some("SMART"),
            None,
        );
        let moved = Dispatch {
            end: utc("2026-11-10T01:30:00Z"),
            ..base.clone()
        };
        let relabelled = Dispatch {
            kind: Some("BOOST".to_string()),
            ..base.clone()
        };
        let extra = planned(
            "2026-11-10T03:00:00Z",
            "2026-11-10T04:00:00Z",
            Some("SMART"),
            None,
        );
        for fresh in [
            vec![moved],
            vec![relabelled],
            vec![base.clone(), extra],
            vec![],
        ] {
            let line = planned_change(Some(std::slice::from_ref(&base)), &fresh, early(), &Utc)
                .unwrap_or_else(|| panic!("{fresh:?} must be logged"));
            assert!(
                line.starts_with("Intelligent Octopus planned charges changed: "),
                "{line}"
            );
        }
        assert_eq!(
            planned_change(Some(std::slice::from_ref(&base)), &[], early(), &Utc).as_deref(),
            Some("Intelligent Octopus planned charges changed: none")
        );
    }

    #[test]
    fn times_are_logged_in_the_given_time_zone() {
        let bst = chrono::FixedOffset::east_opt(3600).unwrap();
        let list = [planned(
            "2026-06-09T23:30:00Z",
            "2026-06-10T00:30:00Z",
            Some("SMART"),
            None,
        )];
        assert_eq!(
            planned_change(None, &list, early(), &bst).as_deref(),
            Some("Intelligent Octopus planned charges: smart 10 Jun 00:30\u{2013}01:30")
        );
    }

    fn capture_logs() -> (
        std::sync::Arc<crate::server::logs::LogRing>,
        tracing::dispatcher::DefaultGuard,
    ) {
        use tracing_subscriber::layer::SubscriberExt;
        let ring = std::sync::Arc::new(crate::server::logs::LogRing::new(200));
        let subscriber = tracing_subscriber::Registry::default()
            .with(crate::server::logs::LogCaptureLayer::new(ring.clone()));
        let guard =
            tracing::dispatcher::set_default(&tracing::dispatcher::Dispatch::new(subscriber));
        (ring, guard)
    }

    fn planned_lines(ring: &crate::server::logs::LogRing) -> Vec<String> {
        ring.read_all()
            .into_iter()
            .filter(|line| line.contains("Intelligent Octopus planned charges"))
            .collect()
    }

    #[tokio::test]
    async fn checks_log_the_plan_once_then_only_its_changes_and_a_failure_adds_none() {
        with_isolated_config_dir_async(|| async {
            let (ring, _guard) = capture_logs();
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;

            check_once(&state, utc(NOW)).await;
            check_once(&state, utc("2026-10-17T12:03:00+00:00")).await;
            assert_eq!(planned_lines(&ring).len(), 1, "{:?}", planned_lines(&ring));
            assert!(!planned_lines(&ring)[0].contains("changed"));

            // A failed check keeps the last list and logs no plan.
            mock.set_dispatches(json!({ "errors": [{ "message": "boom" }] }));
            check_once(&state, utc("2026-10-17T12:06:00+00:00")).await;
            assert_eq!(planned_lines(&ring).len(), 1);
            // Recovering with the same list is not a change either.
            mock.set_dispatches(one_dispatch());
            check_once(&state, utc("2026-10-17T12:09:00+00:00")).await;
            assert_eq!(planned_lines(&ring).len(), 1);

            mock.set_dispatches(dispatches_response(json!([])));
            check_once(&state, utc("2026-10-17T12:12:00+00:00")).await;
            let lines = planned_lines(&ring);
            assert_eq!(lines.len(), 2, "{lines:?}");
            assert!(lines[1].contains("changed: none"), "{lines:?}");
            assert!(!lines
                .iter()
                .any(|l| l.contains(KEY) || l.contains("jwt-abc")));
        })
        .await;
    }

    #[tokio::test]
    async fn switching_the_hold_back_on_logs_a_fresh_baseline() {
        with_isolated_config_dir_async(|| async {
            let (ring, _guard) = capture_logs();
            let mock = MockOctopus::spawn(one_dispatch()).await;
            let state = configure(&mock, true).await;
            check_once(&state, utc(NOW)).await;
            Settings::update(|s| s.octopus_dispatch_hold = false).unwrap();
            check_once(&state, utc("2026-10-17T12:03:00+00:00")).await;
            Settings::update(|s| s.octopus_dispatch_hold = true).unwrap();
            check_once(&state, utc("2026-10-17T12:06:00+00:00")).await;
            let lines = planned_lines(&ring);
            assert_eq!(lines.len(), 2, "{lines:?}");
            assert!(lines.iter().all(|l| !l.contains("changed")), "{lines:?}");
        })
        .await;
    }

    #[test]
    fn dispatches_are_read_from_the_kraken_api_not_the_backend() {
        // `devices` and `flexPlannedDispatches` exist only on the public
        // Kraken API; the backend API Saving Sessions uses refuses them with
        // "Cannot query field 'devices' on type 'Query'".
        let mut settings = Settings::default();
        assert_eq!(
            query_url(&settings),
            "https://api.oegb-kraken.energy/v1/graphql/"
        );
        settings.octopus_api_base_url = "http://127.0.0.1:9/".to_string();
        assert_eq!(query_url(&settings), "http://127.0.0.1:9/v1/graphql/");
    }

    #[test]
    fn the_state_never_prints_the_token() {
        let state = DispatchesState {
            token: Some((Token("jwt-secret".to_string()), utc(NOW))),
            ..Default::default()
        };
        assert!(!format!("{state:?}").contains("jwt-secret"));
    }

    #[test]
    fn the_hold_request_refuses_unknown_fields() {
        assert!(serde_json::from_value::<HoldRequest>(json!({ "enabled": true })).is_ok());
        assert!(serde_json::from_value::<HoldRequest>(json!({ "enable": true })).is_err());
    }
}

#[cfg(test)]
mod charge_tests {
    use super::*;
    use crate::test_util::with_isolated_config_dir_async;

    #[test]
    fn the_planner_expects_a_top_up_only_when_it_is_on_and_the_hold_applies() {
        let applies = PlanningDispatches {
            hold_applies: true,
            ..Default::default()
        };
        let mut settings = Settings {
            octopus_dispatch_charge: true,
            ..Default::default()
        };
        assert_eq!(planning_charge_to_soc(&applies, &settings), Some(100.0));
        settings.charge_cap.enabled = true;
        settings.charge_cap.cap_soc = 85;
        assert_eq!(planning_charge_to_soc(&applies, &settings), Some(85.0));
        assert_eq!(
            planning_charge_to_soc(&PlanningDispatches::default(), &settings),
            None,
            "the hold doesn't apply (calibrating, or the list is stale)"
        );
        settings.octopus_dispatch_charge = false;
        assert_eq!(planning_charge_to_soc(&applies, &settings), None);
    }

    #[test]
    fn the_planner_expects_no_top_up_while_cosy_or_agile_owns_the_slots() {
        let applies = PlanningDispatches {
            hold_applies: true,
            ..Default::default()
        };
        let cosy = Settings {
            octopus_dispatch_charge: true,
            cosy_enabled: true,
            ..Default::default()
        };
        assert_eq!(planning_charge_to_soc(&applies, &cosy), None);
        let agile = Settings {
            octopus_dispatch_charge: true,
            agile_scope: crate::settings::AgileScope::ChargeOnly,
            ..Default::default()
        };
        assert_eq!(planning_charge_to_soc(&applies, &agile), None);
    }

    fn at(hhmm: &str) -> DateTime<Utc> {
        format!("2026-11-10T{hhmm}:00Z").parse().unwrap()
    }

    fn listed(start: &str, end: &str, kind: Option<&str>) -> Dispatch {
        Dispatch {
            start: at(start),
            end: at(end),
            kind: kind.map(str::to_string),
            energy_added_kwh: None,
        }
    }

    fn checked(list: Vec<Dispatch>, now: DateTime<Utc>) -> DispatchesState {
        let mut state = DispatchesState::default();
        state.replace_list(list, now);
        state.checked_at = Some(now);
        state
    }

    #[test]
    fn a_listed_smart_charge_makes_its_half_hours_off_peak_but_a_boost_does_not() {
        let state = checked(
            vec![
                listed("14:10", "14:40", Some("SMART")),
                listed("16:00", "16:30", Some("BOOST")),
            ],
            at("14:05"),
        );
        let now = at("14:05");
        let smart = state.off_peak_half_hours(now);
        assert!(smart.contains(at("14:00").timestamp()));
        assert!(smart.contains(at("14:59").timestamp()));
        assert!(!smart.contains(at("16:10").timestamp()));
    }

    #[test]
    fn a_stale_list_bills_nothing_off_peak() {
        let state = checked(vec![listed("14:00", "15:00", None)], at("13:00"));
        let later = at("14:30");
        assert!(!state.fresh(later));
        assert!(state.off_peak_half_hours(later).is_empty());
    }

    #[test]
    fn a_smart_charge_that_left_the_list_still_covers_the_rest_of_its_half_hour() {
        let mut state = checked(vec![listed("14:00", "14:10", Some("SMART"))], at("14:03"));
        // Octopus drops it once it's over; the 14:00 half-hour stays off-peak.
        state.replace_list(Vec::new(), at("14:12"));
        state.checked_at = Some(at("14:12"));
        let smart = state.off_peak_half_hours(at("14:20"));
        assert!(smart.contains(at("14:20").timestamp()));
        assert!(!smart.contains(at("14:30").timestamp()));
    }

    #[test]
    fn a_cancelled_smart_charge_counts_only_until_the_check_that_dropped_it() {
        let mut state = checked(vec![listed("14:00", "16:00", Some("SMART"))], at("14:05"));
        state.replace_list(Vec::new(), at("14:35"));
        state.checked_at = Some(at("14:35"));
        let smart = state.off_peak_half_hours(at("14:40"));
        assert!(smart.contains(at("14:40").timestamp()));
        assert!(
            !smart.contains(at("15:00").timestamp()),
            "cancelled before 15:00"
        );
        // One that hadn't started yet when it was dropped never ran.
        let mut state = checked(vec![listed("15:00", "16:00", None)], at("14:05"));
        state.replace_list(Vec::new(), at("14:35"));
        state.checked_at = Some(at("14:35"));
        assert!(state.off_peak_half_hours(at("15:10")).is_empty());
    }

    #[test]
    fn a_remembered_smart_charge_is_forgotten_after_two_hours() {
        let mut state = checked(vec![listed("14:00", "14:10", None)], at("14:03"));
        state.replace_list(Vec::new(), at("14:12"));
        assert_eq!(state.ran_smart.len(), 1);
        state.replace_list(Vec::new(), at("16:11"));
        assert!(state.ran_smart.is_empty());
    }

    #[tokio::test]
    async fn the_top_up_is_refused_while_the_hold_is_off() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let (status, Json(body)) =
                set_charge(State(state.clone()), Json(HoldRequest { enabled: true })).await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert!(body["error"].as_str().unwrap().contains("hold"));
            assert!(!Settings::load().octopus_dispatch_charge);

            // Turning it off is always allowed.
            let (status, _) =
                set_charge(State(state.clone()), Json(HoldRequest { enabled: false })).await;
            assert_eq!(status, StatusCode::OK);
        })
        .await;
    }

    #[tokio::test]
    async fn the_top_up_round_trips_with_the_hold_on() {
        with_isolated_config_dir_async(|| async {
            Settings::update(|s| s.octopus_dispatch_hold = true).unwrap();
            let state = Arc::new(AppState::new());
            let (status, _) =
                set_charge(State(state.clone()), Json(HoldRequest { enabled: true })).await;
            assert_eq!(status, StatusCode::OK);
            assert!(Settings::load().octopus_dispatch_charge);

            let (_, Json(body)) = get_dispatches(State(state.clone())).await;
            assert_eq!(body["charge_enabled"], true);
            assert_eq!(body["charging"], false);
            assert_eq!(body["charge_state"], "idle");
            assert_eq!(body["charge_reason"], Value::Null);
            assert_eq!(body["charge_target_soc"], 100);
        })
        .await;
    }

    #[tokio::test]
    async fn the_page_says_why_it_is_sitting_out_and_the_cap_sets_the_target() {
        with_isolated_config_dir_async(|| async {
            Settings::update(|s| {
                s.octopus_dispatch_hold = true;
                s.octopus_dispatch_charge = true;
                s.charge_cap.enabled = true;
                s.charge_cap.cap_soc = 90;
            })
            .unwrap();
            let state = Arc::new(AppState::new());
            state.dispatch_charge.lock().await.reason =
                Some(crate::inverter::dispatch_charge::Reason::ManualForce);
            let (_, Json(body)) = get_dispatches(State(state.clone())).await;
            assert_eq!(body["charge_state"], "standing_aside");
            assert_eq!(body["charge_reason"], "manual_force");
            assert_eq!(body["charge_note"], "Force Charge is running");
            assert_eq!(body["charge_target_soc"], 90);
        })
        .await;
    }
}
