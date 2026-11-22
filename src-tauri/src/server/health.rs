//! `GET /api/health` — HEM's own health for external monitors (issue #503).
//!
//! Tokenless and read-only like `/api/mini/status`, and just as safe to leave
//! open: the report carries statuses, times and fixed reason codes only (see
//! [`crate::health`]). It never reads the inverter, calls out to the network
//! or queries the database, so a monitor can ask every minute. Each lock is
//! taken on its own and released before the next.
//!
//! 200 for `healthy` and `degraded`, 503 for `unhealthy`, since many simple
//! monitors look only at the status code.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};

use crate::health::{
    build_health, BackupInput, DiskInput, Feature, FetchInput, HealthInputs, HealthReport,
    NotifyInput, OctopusInput, Overall,
};
use crate::inverter::poll::AppState;
use crate::settings::Settings;

pub async fn get_health(State(state): State<Arc<AppState>>) -> Response {
    let report = health_report(&state, chrono::Utc::now().timestamp(), Instant::now()).await;
    let code = if report.status == Overall::Unhealthy {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    let mut response = (code, Json(report)).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Gather the inputs at `now` (unix seconds) / `now_mono` and judge them.
/// Public so tests can inject both clocks.
pub async fn health_report(state: &Arc<AppState>, now: i64, now_mono: Instant) -> HealthReport {
    let (poll_interval_secs, host_configured) = {
        let poll = state.settings.lock().await;
        (poll.interval_secs, !poll.host.trim().is_empty())
    };
    let connection = state.connection_state.lock().await.clone();
    let db = state.history.lock().await.clone();
    let marks = &state.health;

    let settings = Settings::load_async().await;
    // Only what the backup loop has already loaded; until it has (about two
    // minutes after start), the check reads `not_yet`.
    let backup = {
        let guard = state.backup.lock().await;
        let enabled = settings.history_backup.enabled;
        BackupInput {
            enabled,
            enabled_for_s: marks.enabled_for_s(Feature::Backup, enabled, now_mono),
            interval_hours: settings.history_backup.interval_hours,
            last_success_at: guard.status.last_success_at,
            last_attempt_ok: guard.status.last.as_ref().map(|r| r.ok),
        }
    };

    let notifications = {
        let config = state.alert_config.lock().await;
        NotifyInput {
            enabled: config.enabled && crate::alerts::has_delivery_channel(&config),
            channels: crate::alerts::channel_marks(&config),
        }
    };

    let (weather_enabled, weather_last) = {
        let ws = state.weather.lock().await;
        let enabled =
            ws.config.enabled && ws.config.latitude.is_some() && ws.config.longitude.is_some();
        (enabled, ws.last_fetch_at.map(|at| at.timestamp()))
    };
    let weather = FetchInput {
        enabled: weather_enabled,
        enabled_for_s: marks.enabled_for_s(Feature::Weather, weather_enabled, now_mono),
        last_ok: weather_last,
    };
    let solar_forecast = FetchInput {
        enabled: weather_enabled,
        enabled_for_s: marks.enabled_for_s(Feature::SolarForecast, weather_enabled, now_mono),
        last_ok: db.as_ref().and_then(|db| db.forecast_stored_at()),
    };

    let octopus = {
        let configured = crate::octopus::configured(&settings);
        let o = state.octopus.lock().await;
        OctopusInput {
            configured,
            enabled_for_s: marks.enabled_for_s(Feature::Octopus, configured, now_mono),
            last_sync_at: o.last_sync_at.map(|at| at.timestamp()),
            failed: o.last_error.is_some(),
        }
    };

    // The disk holding the history database, else the config folder.
    let disk_dir = db
        .as_ref()
        .and_then(|db| db.path().parent().map(PathBuf::from))
        .unwrap_or_else(Settings::settings_dir);
    let disk = tokio::task::spawn_blocking(move || DiskInput {
        free_bytes: fs4::available_space(&disk_dir).ok(),
    })
    .await
    .unwrap_or_default();

    build_health(&HealthInputs {
        now,
        uptime_s: marks.uptime_s(now_mono),
        poll_interval_secs,
        host_configured,
        connection,
        reading: marks.reading(now_mono),
        poll: marks.poll_state(now_mono),
        history: db.as_ref().map(|db| db.write_marks(now_mono)),
        backup,
        disk,
        notifications,
        weather,
        solar_forecast,
        octopus,
    })
}
