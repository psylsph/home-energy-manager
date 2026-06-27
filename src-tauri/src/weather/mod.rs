//! Local weather integration.
//!
//! Periodically fetches the current ambient temperature from the free
//! Open-Meteo API (`api.open-meteo.com/v1/forecast`) and backfills
//! historical observations from the Open-Meteo archive
//! (`archive-api.open-meteo.com/v1/archive`). Both endpoints are keyless
//! and free for non-commercial use under CC BY 4.0 — attribution to
//! "Weather data by Open-Meteo.com" is displayed in the Settings page and
//! on the History page (next to the chart per the licence requirement).
//!
//! # Cadence
//!
//! - Live current reading: every 15 minutes (matches Open-Meteo's
//!   own update frequency).
//! - Backfill: one calendar month per day, until `last_backfill_completed`
//!   reaches "yesterday".
//!
//! # Storage
//!
//! Each observation is a row in `history.db::weather_observations` with
//! timestamp, temperature, source (`current` or `backfill`), and the
//! actual grid-cell lat/lon Open-Meteo resolved. `INSERT OR REPLACE`
//! means a live fetch that overlaps with a slow archive backfill
//! supersedes the older value at the same timestamp.

use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike, NaiveDate};

use crate::inverter::poll::AppState;
use crate::settings::{default_open_meteo_base_url, WeatherConfig};

/// HTTP timeout for both live and backfill fetches. 10 s is generous for
/// Open-Meteo's normal p99 — anything longer is treated as a failure and
/// retried on the next tick rather than blocking the loop.
const WEATHER_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the live "now" fetch runs.
const LIVE_FETCH_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// How often the backfill loop checks whether there's more history to pull.
/// Daily is plenty — each tick advances at most one month.
const BACKFILL_TICK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Upper bound on the number of historical chunks we'll fetch in a row
/// before pausing until the next tick. Defensive — a malformed start date
/// shouldn't produce thousands of API calls in one go.
const BACKFILL_MAX_CHUNKS_PER_TICK: u32 = 4;

/// Lower bound on the backfill window: 30 days before `today`. We
/// don't need years of archive data for the dashboard — temperature
/// history is only displayed alongside 30-day inverter history, and
/// capping the window keeps the Open-Meteo archive calls (one per
/// month) cheap.
fn backfill_min_date(today: NaiveDate) -> NaiveDate {
    today
        .checked_sub_signed(chrono::Duration::days(30))
        .unwrap_or(today)
}

// ---------------------------------------------------------------------------
// Runtime state
// ---------------------------------------------------------------------------

/// Snapshot of the weather subsystem's current state, exposed via
/// `GET /api/weather` and updated by `run_weather_loop`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct WeatherState {
    /// Mirror of the persisted config, so the API layer doesn't have to
    /// hit disk on every poll.
    pub config: WeatherConfig,
    /// ISO-8601 UTC timestamp of the most recent successful live fetch.
    #[serde(default)]
    pub last_fetch_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Temperature returned by the most recent successful live fetch.
    #[serde(default)]
    pub last_fetched_temperature_c: Option<f32>,
    /// Grid-cell latitude actually used by Open-Meteo (may differ from
    /// the requested coords by several km).
    #[serde(default)]
    pub grid_cell_latitude: Option<f32>,
    /// Grid-cell longitude actually used by Open-Meteo.
    #[serde(default)]
    pub grid_cell_longitude: Option<f32>,
    /// True while a backfill task is running. The frontend polls this to
    /// render a spinner; the loop also sets it on entry/exit so a
    /// crashed-and-restarted task doesn't run twice.
    #[serde(default)]
    pub backfill_in_progress: bool,
    /// Most recent error message, surfaced to the Settings UI for
    /// debugging. Cleared on the next successful fetch.
    #[serde(default)]
    pub last_error: Option<String>,
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------

/// Dedicated `ureq` agent for Open-Meteo calls. Mirrors the
/// `telegram_agent` pattern in `alerts/mod.rs`: a 10 s global timeout plus
/// `max_idle_connections(0)` to dodge the same "middlebox silently reaps
/// idle pooled socket" pitfall that bit the Telegram client. With
/// pooling disabled every call pays its own TCP+TLS setup, which is
/// negligible against the 15-min cadence.
///
/// Exposed as `pub(crate)` so the API layer can reuse the same agent for
/// the postcode lookup (api.postcodes.io) — same timeout / no-pooling
/// policy applies.
pub(crate) fn weather_agent() -> &'static ureq::Agent {
    use std::sync::OnceLock;
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(WEATHER_HTTP_TIMEOUT))
            .max_idle_connections(0)
            .max_idle_connections_per_host(0)
            .build();
        ureq::Agent::new_with_config(config)
    })
}

fn derive_archive_base_url(forecast_base: &str) -> String {
    // The free non-commercial Open-Meteo API exposes the live forecast at
    // `api.open-meteo.com` and the archive at `archive-api.open-meteo.com`.
    // For a self-hosted instance both URLs are configurable per-instance,
    // so we only apply the swap when the user is on the default endpoint.
    if forecast_base == default_open_meteo_base_url() {
        "https://archive-api.open-meteo.com".to_string()
    } else {
        forecast_base.to_string()
    }
}

/// One observation returned by either the live or the archive endpoint.
#[derive(Debug, Clone)]
struct WeatherObservation {
    timestamp: i64,
    temperature_c: f32,
    grid_lat: Option<f32>,
    grid_lon: Option<f32>,
}

/// Fetch the current ambient temperature from Open-Meteo's forecast endpoint.
async fn fetch_current(
    base_url: &str,
    latitude: f64,
    longitude: f64,
) -> Result<WeatherObservation, String> {
    let url = format!(
        "{base}/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m&timezone=UTC",
        base = base_url.trim_end_matches('/'),
        lat = latitude,
        lon = longitude,
    );

    let result: Result<serde_json::Value, String> = tokio::task::spawn_blocking(move || {
        let mut resp = weather_agent()
            .get(&url)
            .call()
            .map_err(|e| format!("HTTP error: {e}"))?;
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("read error: {e}"))?;
        serde_json::from_str(&body).map_err(|e| format!("JSON error: {e}"))
    })
    .await
    .map_err(|e| format!("spawn_blocking failed: {e}"))?;

    let json = result?;
    let current = json
        .get("current")
        .ok_or_else(|| "missing 'current'".to_string())?;
    let time = current
        .get("time")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'current.time'".to_string())?;
    let temperature = current
        .get("temperature_2m")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| "missing 'current.temperature_2m'".to_string())?;
    let ts = chrono::NaiveDateTime::parse_from_str(time, "%Y-%m-%dT%H:%M")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(time, "%Y-%m-%dT%H:%M:%S"))
        .map_err(|e| format!("invalid time '{time}': {e}"))?
        .and_utc()
        .timestamp();

    // Open-Meteo also echoes the resolved coords at the top level — we
    // persist them so the Settings UI can show what grid cell was used.
    let grid_lat = json
        .get("latitude")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32);
    let grid_lon = json
        .get("longitude")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32);

    Ok(WeatherObservation {
        timestamp: ts,
        temperature_c: temperature as f32,
        grid_lat,
        grid_lon,
    })
}

/// Fetch one calendar month of hourly observations from Open-Meteo's
/// archive endpoint. Returns observations ordered by timestamp ascending.
async fn fetch_archive_month(
    archive_base_url: &str,
    latitude: f64,
    longitude: f64,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<WeatherObservation>, String> {
    let url = format!(
        "{base}/v1/archive?latitude={lat}&longitude={lon}&start_date={s}&end_date={e}&hourly=temperature_2m&timezone=UTC",
        base = archive_base_url.trim_end_matches('/'),
        lat = latitude,
        lon = longitude,
        s = start.format("%Y-%m-%d"),
        e = end.format("%Y-%m-%d"),
    );

    let result: Result<serde_json::Value, String> = tokio::task::spawn_blocking(move || {
        let mut resp = weather_agent()
            .get(&url)
            .call()
            .map_err(|e| format!("HTTP error: {e}"))?;
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("read error: {e}"))?;
        serde_json::from_str(&body).map_err(|e| format!("JSON error: {e}"))
    })
    .await
    .map_err(|e| format!("spawn_blocking failed: {e}"))?;

    let json = result?;
    // Guard against Open-Meteo's own error responses (HTTP 200 with an
    // `error: true` body, e.g. for malformed parameters).
    if json.get("error").and_then(|v| v.as_bool()).unwrap_or(false) {
        let reason = json
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        return Err(format!("Open-Meteo error: {reason}"));
    }

    let hourly = json
        .get("hourly")
        .ok_or_else(|| "missing 'hourly'".to_string())?;
    let times = hourly
        .get("time")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "missing 'hourly.time'".to_string())?;
    let temps = hourly
        .get("temperature_2m")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "missing 'hourly.temperature_2m'".to_string())?;

    if times.len() != temps.len() {
        return Err(format!(
            "hourly.time ({} entries) and hourly.temperature_2m ({} entries) lengths differ",
            times.len(),
            temps.len()
        ));
    }

    let grid_lat = json
        .get("latitude")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32);
    let grid_lon = json
        .get("longitude")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32);

    let mut out = Vec::with_capacity(times.len());
    for (t, temp) in times.iter().zip(temps.iter()) {
        let Some(time_str) = t.as_str() else { continue };
        let Ok(parsed) = chrono::NaiveDateTime::parse_from_str(time_str, "%Y-%m-%dT%H:%M") else {
            continue;
        };
        // Open-Meteo returns UTC-naive timestamps when `timezone=UTC` is
        // passed. The DB stores seconds since epoch.
        let ts = parsed.and_utc().timestamp();
        let Some(temp) = temp.as_f64() else { continue };
        out.push(WeatherObservation {
            timestamp: ts,
            temperature_c: temp as f32,
            grid_lat,
            grid_lon,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Backfill driver
// ---------------------------------------------------------------------------

/// Compute the next calendar-month window to backfill.
///
/// `last_completed` is the last date that's *fully* populated. We advance
/// one month at a time and stop at `today` (UTC). Returns `None` when
/// there's nothing left to do. The returned `end` is *inclusive* — it's
/// passed straight to Open-Meteo's `end_date` query parameter, which
/// must be `<= today` (the archive only contains data up to today).
fn next_backfill_window(
    last_completed: Option<NaiveDate>,
    today: NaiveDate,
) -> Option<(NaiveDate, NaiveDate)> {
    let start = match last_completed {
        // First ever run: backfill from the 30-day-ago lower bound so
        // a stale or empty history DB doesn't trigger years of API
        // calls.
        None => backfill_min_date(today),
        Some(d) => d + chrono::Duration::days(1),
    };
    if start > today {
        return None;
    }
    let start = start.max(backfill_min_date(today));
    // End is the last day of `start`'s month, capped at today.
    // Open-Meteo's `end_date` is inclusive and rejects dates beyond
    // today with HTTP 400. We compute the *last* day of the month by
    // taking the first of the next month and subtracting one.
    let mut year: i32 = start.year();
    let mut month: u32 = start.month();
    if month == 12 {
        year += 1;
        month = 1;
    } else {
        month += 1;
    }
    let month_end = NaiveDate::from_ymd_opt(year, month, 1)
        .and_then(|d| d.pred_opt())
        .unwrap_or(today)
        .min(today);
    Some((start, month_end))
}

/// Run the backfill from `from` up to `today` (UTC), one calendar month
/// per call to `fetch_archive_month`. Updates `state.last_backfill_completed`
/// and `state.last_error` as it goes. Resumable: each tick advances by
/// at most `BACKFILL_MAX_CHUNKS_PER_TICK` months.
async fn run_backfill_tick(state: Arc<AppState>) {
    let today = chrono::Utc::now().date_naive();
    let (config, history_db) = {
        let ws = state.weather.lock().await;
        let history_db = state.history.lock().await.clone();
        (ws.config.clone(), history_db)
    };
    let Some(db) = history_db else {
        return;
    };
    let (Some(lat), Some(lon)) = (config.latitude, config.longitude) else {
        return;
    };

    let archive_base = derive_archive_base_url(&config.open_meteo_base_url);
    let mut last_completed = config.last_backfill_completed;
    let mut chunks_done = 0u32;

    while chunks_done < BACKFILL_MAX_CHUNKS_PER_TICK {
        let window = match next_backfill_window(last_completed, today) {
            Some(w) => w,
            None => break,
        };

        match fetch_archive_month(&archive_base, lat, lon, window.0, window.1).await {
            Ok(obs) => {
                let fetched_at = chrono::Utc::now().timestamp();
                for o in &obs {
                    db.insert_weather(
                        o.timestamp,
                        o.temperature_c,
                        "backfill",
                        o.grid_lat,
                        o.grid_lon,
                        fetched_at,
                    );
                }
                // Advance `last_completed` to the last day we actually
                // fetched. The window's `end` is inclusive, so on success
                // that's just `window.1`. On an empty range we skip the
                // window start and try again next tick.
                let advance_to = if obs.is_empty() { window.0 } else { window.1 };
                last_completed = Some(advance_to);

                {
                    let mut ws = state.weather.lock().await;
                    ws.config.last_backfill_completed = Some(advance_to);
                    ws.last_error = None;
                }
                tracing::info!(
                    start = %window.0,
                    end = %window.1,
                    rows = obs.len(),
                    "weather backfill chunk complete",
                );
            }
            Err(e) => {
                tracing::warn!(
                    start = %window.0,
                    end = %window.1,
                    error = %e,
                    "weather backfill chunk failed; will retry on next tick",
                );
                {
                    let mut ws = state.weather.lock().await;
                    ws.last_error = Some(format!("Backfill failed: {e}"));
                }
                // Don't advance `last_completed` — we'll retry the same
                // window on the next tick. Stop early so we don't hammer
                // a broken endpoint.
                break;
            }
        }
        chunks_done += 1;
    }

    // Persist the new `last_backfill_completed` to settings.json so it
    // survives a crash.
    if last_completed != config.last_backfill_completed {
        let mut settings = crate::settings::Settings::load();
        settings.weather_config.last_backfill_completed = last_completed;
        if let Err(e) = settings.save() {
            tracing::warn!("Failed to persist weather backfill progress: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

/// Run the weather loop forever. Spawned from `lib.rs` after
/// `initialize_app_state`. Idles when weather is disabled or coordinates
/// are missing; otherwise ticks every `LIVE_FETCH_INTERVAL` for the live
/// fetch and once a day for the backfill tick.
pub async fn run_weather_loop(state: Arc<AppState>) {
    tracing::info!("Weather loop starting");
    let mut live_tick = tokio::time::interval(LIVE_FETCH_INTERVAL);
    live_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // First tick fires immediately — let it through so a pre-configured
    // weather setup gets a reading on startup rather than waiting 15 min.
    // If weather is disabled or unconfigured, `run_live_fetch` is a no-op.

    let mut backfill_tick = tokio::time::interval(BACKFILL_TICK_INTERVAL);
    backfill_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    backfill_tick.tick().await;

    loop {
        tokio::select! {
            _ = live_tick.tick() => {
                run_live_fetch(state.clone()).await;
            }
            _ = backfill_tick.tick() => {
                let ws = state.weather.lock().await;
                if ws.config.enabled
                    && ws.config.latitude.is_some()
                    && ws.config.longitude.is_some()
                    && !ws.backfill_in_progress
                {
                    drop(ws);
                    run_backfill_tick(state.clone()).await;
                }
            }
        }
    }
}

async fn run_live_fetch(state: Arc<AppState>) {
    let (config, history_db) = {
        let ws = state.weather.lock().await;
        let history_db = state.history.lock().await.clone();
        (ws.config.clone(), history_db)
    };
    if !config.enabled {
        return;
    }
    let (Some(lat), Some(lon)) = (config.latitude, config.longitude) else {
        return;
    };
    let Some(db) = history_db else {
        return;
    };

    match fetch_current(&config.open_meteo_base_url, lat, lon).await {
        Ok(obs) => {
            let fetched_at = chrono::Utc::now().timestamp();
            db.insert_weather(
                obs.timestamp,
                obs.temperature_c,
                "current",
                obs.grid_lat,
                obs.grid_lon,
                fetched_at,
            );
            let mut ws = state.weather.lock().await;
            ws.last_fetch_at = Some(
                chrono::DateTime::<chrono::Utc>::from_timestamp(obs.timestamp, 0)
                    .unwrap_or_else(chrono::Utc::now),
            );
            ws.last_fetched_temperature_c = Some(obs.temperature_c);
            ws.grid_cell_latitude = obs.grid_lat;
            ws.grid_cell_longitude = obs.grid_lon;
            ws.last_error = None;
        }
        Err(e) => {
            tracing::warn!("Weather live fetch failed: {e}");
            let mut ws = state.weather.lock().await;
            ws.last_error = Some(format!("Live fetch failed: {e}"));
        }
    }
}

/// Public entry point used by `POST /api/weather/backfill` to start a
/// one-shot backfill in the background. The Settings UI doesn't await
/// the future — it polls `GET /api/weather` for progress.
pub fn spawn_backfill(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut ws = state.weather.lock().await;
        if ws.backfill_in_progress {
            // Already running — nothing to do.
            return;
        }
        ws.backfill_in_progress = true;
        drop(ws);

        run_backfill_tick(state.clone()).await;

        let mut ws = state.weather.lock().await;
        ws.backfill_in_progress = false;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    // --- next_backfill_window -------------------------------------------

    #[test]
    fn next_window_first_run_starts_at_min_date() {
        // No prior backfill — start at the 30-day floor, end is the
        // first day of the month after that floor (inclusive).
        let today = d(2025, 6, 15);
        let (start, end) = next_backfill_window(None, today).unwrap();
        assert_eq!(start, d(2025, 5, 16));
        assert_eq!(end, d(2025, 5, 31));
    }

    #[test]
    fn next_window_advances_from_clamped_floor() {
        // Last completed is older than the 30-day floor — the start
        // must be clamped forward to the floor.
        let today = d(2025, 6, 15);
        let (start, end) = next_backfill_window(Some(d(2025, 3, 15)), today).unwrap();
        assert_eq!(start, d(2025, 5, 16));
        assert_eq!(end, d(2025, 5, 31));
    }

    #[test]
    fn next_window_advances_one_day_within_floor() {
        // Last completed is yesterday — next window is just today
        // (the end is inclusive, so end == today).
        let today = d(2025, 6, 15);
        let (start, end) = next_backfill_window(Some(d(2025, 6, 14)), today).unwrap();
        assert_eq!(start, d(2025, 6, 15));
        assert_eq!(end, d(2025, 6, 15));
    }

    #[test]
    fn next_window_wraps_month_boundary() {
        // Last completed is 30 May; start is 31 May (after the 30-day
        // floor so it isn't clamped), end is the last day of May.
        let today = d(2025, 6, 15);
        let (start, end) = next_backfill_window(Some(d(2025, 5, 30)), today).unwrap();
        assert_eq!(start, d(2025, 5, 31));
        assert_eq!(end, d(2025, 5, 31));
    }

    #[test]
    fn next_window_caps_end_at_today() {
        // End must never exceed today — Open-Meteo's archive rejects
        // `end_date` in the future with HTTP 400.
        let today = d(2025, 6, 15);
        let (_, end) = next_backfill_window(Some(d(2025, 5, 1)), today).unwrap();
        assert_eq!(end, d(2025, 5, 31));
        // The single-day case is also bounded by today.
        let (start, end) = next_backfill_window(Some(d(2025, 6, 14)), today).unwrap();
        assert_eq!(start, d(2025, 6, 15));
        assert_eq!(end, today);
    }

    #[test]
    fn next_window_returns_none_when_caught_up() {
        // Already backfilled through today — nothing to do.
        let today = d(2025, 6, 15);
        assert!(next_backfill_window(Some(today), today).is_none());
    }

    #[test]
    fn next_window_clamps_start_below_min_date() {
        // A bogus `last_completed` from before the 30-day window still
        // clamps the start to the floor — we never fetch older data.
        let today = d(2025, 6, 15);
        let (start, _) = next_backfill_window(Some(d(2000, 1, 1)), today).unwrap();
        assert_eq!(start, backfill_min_date(today));
        assert_eq!(start, d(2025, 5, 16));
    }

    // --- derive_archive_base_url ----------------------------------------

    #[test]
    fn archive_url_swaps_default_host() {
        // The free non-commercial default swaps to the archive subdomain.
        assert_eq!(
            derive_archive_base_url("https://api.open-meteo.com"),
            "https://archive-api.open-meteo.com"
        );
    }

    #[test]
    fn archive_url_preserves_custom_host() {
        // A self-hosted instance keeps the same base — both endpoints
        // live on the same host in that deployment shape.
        assert_eq!(
            derive_archive_base_url("https://weather.example.internal"),
            "https://weather.example.internal"
        );
    }

    #[test]
    fn archive_url_preserves_custom_host_with_trailing_slash() {
        // Trailing slashes are left alone here — the caller trims them
        // before building the URL. This just confirms we don't mangle a
        // non-default host.
        assert_eq!(
            derive_archive_base_url("https://weather.example.internal/"),
            "https://weather.example.internal/"
        );
    }
}
