//! Authenticated Octopus Energy customer-consumption integration (issue #212).
//!
//! Supplier intervals are stored separately from inverter history because they
//! arrive late and may be corrected. Gas values are deliberately preserved in
//! the units returned by Octopus: SMETS1 commonly reports kWh, while SMETS2 may
//! report cubic metres.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use base64::Engine;
use chrono::{DateTime, Utc};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::history::{HistoryDb, OctopusConsumptionRow};
use crate::inverter::poll::AppState;
use crate::settings::Settings;

const OFFICIAL_BASE_URL: &str = "https://api.octopus.energy";
const RECENT_INITIAL_DAYS: i64 = 90;
const RECENT_REFRESH_DAYS: i64 = 7;
const BACKFILL_CHUNK_DAYS: i64 = 90;
const SYNC_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OctopusState {
    pub syncing: bool,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub backfill_complete: bool,
    pub discovered_streams: usize,
    pub imported_intervals: u64,
}

#[derive(Debug, Deserialize)]
struct AccountResponse {
    #[serde(default)]
    properties: Vec<Property>,
}

#[derive(Debug, Deserialize)]
struct Property {
    moved_in_at: Option<String>,
    moved_out_at: Option<String>,
    #[serde(default)]
    electricity_meter_points: Vec<ElectricityPoint>,
    #[serde(default)]
    gas_meter_points: Vec<GasPoint>,
}

#[derive(Debug, Deserialize)]
struct ElectricityPoint {
    mpan: String,
    #[serde(default)]
    is_export: bool,
    #[serde(default)]
    meters: Vec<Meter>,
    #[serde(default)]
    agreements: Vec<Agreement>,
}

#[derive(Debug, Deserialize)]
struct GasPoint {
    mprn: String,
    #[serde(default)]
    meters: Vec<Meter>,
    #[serde(default)]
    agreements: Vec<Agreement>,
}

#[derive(Debug, Deserialize)]
struct Meter {
    serial_number: String,
}

#[derive(Debug, Deserialize)]
struct Agreement {
    valid_from: String,
}

#[derive(Debug, Clone, PartialEq)]
struct Stream {
    kind: String,
    meter_point: String,
    serial: String,
    earliest: i64,
}

impl Stream {
    fn key(&self) -> String {
        format!("{}:{}:{}", self.kind, self.meter_point, self.serial)
    }

    fn path(&self) -> String {
        if self.kind == "gas" {
            format!(
                "/v1/gas-meter-points/{}/meters/{}/consumption/",
                encode(&self.meter_point),
                encode(&self.serial)
            )
        } else {
            format!(
                "/v1/electricity-meter-points/{}/meters/{}/consumption/",
                encode(&self.meter_point),
                encode(&self.serial)
            )
        }
    }
}

#[derive(Debug, Deserialize)]
struct ConsumptionPage {
    next: Option<String>,
    #[serde(default)]
    results: Vec<ConsumptionResult>,
}

#[derive(Debug, Deserialize)]
struct ConsumptionResult {
    consumption: f64,
    interval_start: String,
    interval_end: String,
}

fn encode(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn configured(settings: &Settings) -> bool {
    settings.octopus_enabled
        && !settings.octopus_api_key.trim().is_empty()
        && !settings.octopus_account_number.trim().is_empty()
}

fn base_url(settings: &Settings) -> String {
    let configured = settings.octopus_api_base_url.trim().trim_end_matches('/');
    if configured.is_empty() {
        OFFICIAL_BASE_URL.to_string()
    } else {
        configured.to_string()
    }
}

fn parse_timestamp(value: &str) -> Result<i64, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.timestamp())
        .map_err(|e| format!("invalid Octopus timestamp '{value}': {e}"))
}

fn discover_streams(account: AccountResponse, now: i64) -> Vec<Stream> {
    let mut streams = Vec::new();
    for property in account
        .properties
        .into_iter()
        .filter(|p| p.moved_out_at.is_none())
    {
        let property_start = property
            .moved_in_at
            .as_deref()
            .and_then(|v| parse_timestamp(v).ok())
            .unwrap_or(now - 365 * 86400);
        for point in property.electricity_meter_points {
            let earliest = point
                .agreements
                .iter()
                .filter_map(|a| parse_timestamp(&a.valid_from).ok())
                .min()
                .unwrap_or(property_start);
            let kind = if point.is_export {
                "electricity_export"
            } else {
                "electricity_import"
            };
            for meter in point.meters {
                streams.push(Stream {
                    kind: kind.to_string(),
                    meter_point: point.mpan.clone(),
                    serial: meter.serial_number,
                    earliest,
                });
            }
        }
        for point in property.gas_meter_points {
            let earliest = point
                .agreements
                .iter()
                .filter_map(|a| parse_timestamp(&a.valid_from).ok())
                .min()
                .unwrap_or(property_start);
            for meter in point.meters {
                streams.push(Stream {
                    kind: "gas".to_string(),
                    meter_point: point.mprn.clone(),
                    serial: meter.serial_number,
                    earliest,
                });
            }
        }
    }
    streams
}

fn auth_header(api_key: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{api_key}:"));
    format!("Basic {encoded}")
}

fn validated_next_url(
    next: Option<String>,
    allowed_prefix: &str,
) -> Result<Option<String>, String> {
    match next {
        Some(url) if url.starts_with(allowed_prefix) => Ok(Some(url)),
        Some(_) => Err("Octopus pagination URL changed API origin".to_string()),
        None => Ok(None),
    }
}

fn http_get_json(url: String, api_key: String) -> Result<Value, String> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .max_idle_connections(0)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = agent
        .get(&url)
        .header("Authorization", &auth_header(&api_key))
        .call()
        .map_err(|e| format!("Octopus request failed: {e}"))?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("failed to read Octopus response: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("invalid Octopus response: {e}"))
}

async fn get_json(url: String, api_key: String) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || http_get_json(url, api_key))
        .await
        .map_err(|e| format!("Octopus request task failed: {e}"))?
}

async fn fetch_account(settings: &Settings) -> Result<AccountResponse, String> {
    let url = format!(
        "{}/v1/accounts/{}/",
        base_url(settings),
        encode(settings.octopus_account_number.trim())
    );
    serde_json::from_value(get_json(url, settings.octopus_api_key.clone()).await?)
        .map_err(|e| format!("invalid Octopus account response: {e}"))
}

async fn fetch_window(
    settings: &Settings,
    stream: &Stream,
    start: i64,
    end: i64,
) -> Result<Vec<OctopusConsumptionRow>, String> {
    let from = DateTime::<Utc>::from_timestamp(start, 0).ok_or("invalid sync start")?;
    let to = DateTime::<Utc>::from_timestamp(end, 0).ok_or("invalid sync end")?;
    let api_base = base_url(settings);
    let allowed_page_prefix = format!("{api_base}/");
    let mut url = format!(
        "{}{}?period_from={}&period_to={}&order_by=period&page_size=1000",
        api_base,
        stream.path(),
        encode(&from.to_rfc3339()),
        encode(&to.to_rfc3339())
    );
    let mut rows = Vec::new();
    loop {
        let page: ConsumptionPage =
            serde_json::from_value(get_json(url, settings.octopus_api_key.clone()).await?)
                .map_err(|e| format!("invalid Octopus consumption response: {e}"))?;
        for item in page.results {
            if !item.consumption.is_finite() || item.consumption < 0.0 {
                continue;
            }
            rows.push(OctopusConsumptionRow {
                meter_kind: stream.kind.clone(),
                meter_point: stream.meter_point.clone(),
                meter_serial: stream.serial.clone(),
                interval_start: parse_timestamp(&item.interval_start)?,
                interval_end: parse_timestamp(&item.interval_end)?,
                consumption: item.consumption,
            });
        }
        match validated_next_url(page.next, &allowed_page_prefix)? {
            Some(next) => url = next,
            None => break,
        }
    }
    Ok(rows)
}

async fn sync_recent(
    settings: &Settings,
    db: &HistoryDb,
    stream: &Stream,
    now: i64,
) -> Result<u64, String> {
    let cursor = db.octopus_sync_cursor(&stream.key());
    let days = if cursor.is_some() {
        RECENT_REFRESH_DAYS
    } else {
        RECENT_INITIAL_DAYS
    };
    let recent_start = (now - days * 86400).max(stream.earliest);
    let rows = fetch_window(settings, stream, recent_start, now).await?;
    let imported = db.upsert_octopus_consumption(&rows, Utc::now().timestamp())? as u64;
    if cursor.is_none() {
        db.set_octopus_sync_cursor(&stream.key(), recent_start, recent_start <= stream.earliest)?;
    }
    Ok(imported)
}

async fn backfill_stream(
    settings: &Settings,
    db: &HistoryDb,
    stream: &Stream,
) -> Result<(u64, bool), String> {
    let key = stream.key();
    let Some((mut before, mut complete)) = db.octopus_sync_cursor(&key) else {
        return Err(format!("missing sync cursor for {key}"));
    };
    let mut imported = 0u64;
    while !complete && before > stream.earliest {
        let start = (before - BACKFILL_CHUNK_DAYS * 86400).max(stream.earliest);
        let rows = fetch_window(settings, stream, start, before).await?;
        imported += db.upsert_octopus_consumption(&rows, Utc::now().timestamp())? as u64;
        before = start;
        complete = before <= stream.earliest;
        db.set_octopus_sync_cursor(&key, before, complete)?;
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    Ok((imported, complete))
}

async fn run_sync(state: Arc<AppState>) -> Result<(), String> {
    let settings = Settings::load();
    if !configured(&settings) {
        return Err("Octopus integration is not fully configured".to_string());
    }
    {
        let mut status = state.octopus.lock().await;
        if status.syncing {
            return Err("Octopus sync is already running".to_string());
        }
        status.syncing = true;
        status.last_error = None;
    }

    let result = async {
        let db = state
            .history
            .lock()
            .await
            .clone()
            .ok_or("history database is unavailable")?;
        let now = Utc::now().timestamp();
        let streams = discover_streams(fetch_account(&settings).await?, now);
        if streams.is_empty() {
            return Err("Octopus account has no active electricity or gas meters".to_string());
        }
        let mut imported = 0u64;
        // Fetch the recent window for every stream first. Users therefore see
        // import, export and gas graphs quickly even when a multi-year
        // backfill takes much longer to finish.
        for stream in &streams {
            imported += sync_recent(&settings, &db, stream, now).await?;
        }
        let mut all_complete = true;
        for stream in &streams {
            let (count, complete) = backfill_stream(&settings, &db, stream).await?;
            imported += count;
            all_complete &= complete;
        }
        Ok::<_, String>((streams.len(), imported, all_complete))
    }
    .await;

    let mut status = state.octopus.lock().await;
    status.syncing = false;
    match result {
        Ok((streams, imported, complete)) => {
            status.last_sync_at = Some(Utc::now());
            status.last_error = None;
            status.discovered_streams = streams;
            status.imported_intervals = status.imported_intervals.saturating_add(imported);
            status.backfill_complete = complete;
            Ok(())
        }
        Err(error) => {
            status.last_error = Some(error.clone());
            Err(error)
        }
    }
}

pub async fn run_octopus_loop(state: Arc<AppState>) {
    tracing::info!("Octopus consumption loop starting");
    let mut tick = tokio::time::interval(SYNC_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if configured(&Settings::load()) {
            if let Err(error) = run_sync(state.clone()).await {
                tracing::warn!("Octopus sync failed: {error}");
            }
        }
    }
}

pub async fn get_status(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let settings = Settings::load();
    let status = state.octopus.lock().await.clone();
    let bounds = state
        .history
        .lock()
        .await
        .clone()
        .and_then(|db| db.octopus_bounds());
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "configured": configured(&settings),
            "data": status,
            "bounds": bounds.map(|(start, end)| [start * 1000, end * 1000]),
            "gas_unit_note": "Gas values are shown in the units reported by Octopus (kWh for many SMETS1 meters; m³ may be returned for SMETS2)."
        })),
    )
}

pub async fn start_sync(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    if !configured(&Settings::load()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "Octopus integration is not fully configured"})),
        );
    }
    if state.octopus.lock().await.syncing {
        return (
            StatusCode::CONFLICT,
            Json(json!({"ok": false, "error": "Octopus sync is already running"})),
        );
    }
    tokio::spawn(async move {
        if let Err(error) = run_sync(state).await {
            tracing::warn!("Manual Octopus sync failed: {error}");
        }
    });
    (
        StatusCode::ACCEPTED,
        Json(json!({"ok": true, "message": "Octopus sync started"})),
    )
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    range: Option<String>,
    offset: Option<i64>,
}

pub async fn get_history(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HistoryQuery>,
) -> (StatusCode, Json<Value>) {
    if !configured(&Settings::load()) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "Octopus integration is not configured"})),
        );
    }
    let now = Utc::now().timestamp();
    let offset = query.offset.unwrap_or(0).max(0);
    let (span, bucket) = match query.range.as_deref().unwrap_or("30d") {
        "7d" => (7 * 86400, 1800),
        "30d" => (30 * 86400, 6 * 3600),
        "6m" => (180 * 86400, 86400),
        "1y" => (365 * 86400, 86400),
        "all" => {
            let bounds = state
                .history
                .lock()
                .await
                .clone()
                .and_then(|db| db.octopus_bounds());
            let start = bounds.map(|b| b.0).unwrap_or(now - 365 * 86400);
            (now - start, 30 * 86400)
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": "Invalid range. Use 7d, 30d, 6m, 1y, or all"})),
            )
        }
    };
    let end = now - offset * span;
    let start = end - span;
    let db = state.history.lock().await.clone();
    let Some(db) = db else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": "History database is unavailable"})),
        );
    };
    match tokio::task::spawn_blocking(move || db.query_octopus_consumption(start, end, bucket))
        .await
    {
        Ok(Ok(data)) => (StatusCode::OK, Json(json!({"ok": true, "data": data}))),
        Ok(Err(error)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": error})),
        ),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": error.to_string()})),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_requires_opt_in_and_both_credentials() {
        let mut settings = Settings::default();
        settings.octopus_enabled = true;
        settings.octopus_api_key = "sk_live".into();
        assert!(!configured(&settings));
        settings.octopus_account_number = "A-1234".into();
        assert!(configured(&settings));
    }

    #[test]
    fn discovers_active_import_export_and_gas_streams() {
        let account: AccountResponse = serde_json::from_value(json!({
            "properties": [{
                "moved_in_at": "2024-01-01T00:00:00Z",
                "moved_out_at": null,
                "electricity_meter_points": [
                    {"mpan":"111", "is_export":false, "meters":[{"serial_number":"IMP"}], "agreements":[]},
                    {"mpan":"222", "is_export":true, "meters":[{"serial_number":"EXP"}], "agreements":[]}
                ],
                "gas_meter_points": [
                    {"mprn":"333", "meters":[{"serial_number":"GAS"}], "agreements":[]}
                ]
            }, {
                "moved_in_at": "2020-01-01T00:00:00Z",
                "moved_out_at": "2023-01-01T00:00:00Z",
                "electricity_meter_points": [{"mpan":"old", "meters":[{"serial_number":"OLD"}]}]
            }]
        })).unwrap();
        let streams = discover_streams(account, 0);
        assert_eq!(streams.len(), 3);
        assert!(streams.iter().any(|s| s.kind == "electricity_import"));
        assert!(streams.iter().any(|s| s.kind == "electricity_export"));
        assert!(streams.iter().any(|s| s.kind == "gas"));
        assert!(!streams.iter().any(|s| s.meter_point == "old"));
    }

    #[test]
    fn basic_auth_uses_key_as_username_and_blank_password() {
        assert_eq!(auth_header("abc"), "Basic YWJjOg==");
    }

    #[test]
    fn pagination_rejects_a_different_origin_before_reusing_credentials() {
        assert!(validated_next_url(
            Some("https://evil.example/steal".to_string()),
            "https://api.octopus.energy/"
        )
        .is_err());
        assert_eq!(
            validated_next_url(
                Some("https://api.octopus.energy/v1/next".to_string()),
                "https://api.octopus.energy/"
            )
            .unwrap(),
            Some("https://api.octopus.energy/v1/next".to_string())
        );
    }

    #[test]
    fn parses_dst_offset_timestamps_as_absolute_instants() {
        assert_eq!(
            parse_timestamp("2024-03-31T02:00:00+01:00").unwrap(),
            parse_timestamp("2024-03-31T01:00:00Z").unwrap()
        );
    }
}
