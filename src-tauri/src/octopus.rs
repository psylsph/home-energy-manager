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

use crate::history::{HistoryDb, OctopusConsumptionRow, OctopusTariffPriceRow};
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
    pub tariff_prices: usize,
    pub last_tariff_error: Option<String>,
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

#[derive(Debug, Clone, Deserialize)]
struct Agreement {
    tariff_code: String,
    valid_from: String,
    valid_to: Option<String>,
}

#[derive(Debug, Clone)]
struct Stream {
    kind: String,
    meter_point: String,
    serial: String,
    earliest: i64,
    agreements: Vec<Agreement>,
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

#[derive(Debug, Deserialize)]
struct PricePage {
    next: Option<String>,
    #[serde(default)]
    results: Vec<PriceResult>,
}

#[derive(Debug, Clone, Deserialize)]
struct PriceResult {
    value_inc_vat: f64,
    valid_from: String,
    valid_to: Option<String>,
    payment_method: Option<String>,
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
                    agreements: point.agreements.clone(),
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
                    agreements: point.agreements.clone(),
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

fn tariff_product_code(tariff_code: &str) -> Result<&str, String> {
    let rest = ["E-1R-", "E-2R-", "G-1R-"]
        .iter()
        .find_map(|prefix| tariff_code.strip_prefix(prefix))
        .ok_or_else(|| format!("unsupported Octopus tariff code '{tariff_code}'"))?;
    rest.rsplit_once('-')
        .map(|(product, _region)| product)
        .filter(|product| !product.is_empty())
        .ok_or_else(|| format!("invalid Octopus tariff code '{tariff_code}'"))
}

fn hhmm_minutes(value: &str) -> Option<u16> {
    let (hour, minute) = value.split_once(':')?;
    let hour = hour.parse::<u16>().ok()?;
    let minute = minute.parse::<u16>().ok()?;
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

fn tariff_rate_types(tariff_code: &str) -> &'static [&'static str] {
    if tariff_code.starts_with("E-2R-") {
        &["day", "night", "standing"]
    } else {
        &["standard", "standing"]
    }
}

fn payment_priority(method: Option<&str>) -> u8 {
    match method {
        Some("DIRECT_DEBIT") => 3,
        None => 2,
        _ => 1,
    }
}

async fn fetch_tariff_prices(
    settings: &Settings,
    stream: &Stream,
    agreement: &Agreement,
    rate_type: &str,
    now: i64,
    refresh_from: Option<i64>,
) -> Result<Vec<OctopusTariffPriceRow>, String> {
    let agreement_start = parse_timestamp(&agreement.valid_from)?;
    let agreement_end = agreement
        .valid_to
        .as_deref()
        .map(parse_timestamp)
        .transpose()?;
    let request_end = agreement_end.unwrap_or(now + 86400);
    let request_start = refresh_from
        .map(|from| from.max(agreement_start))
        .unwrap_or(agreement_start);
    if request_end <= request_start {
        return Ok(Vec::new());
    }
    let product = tariff_product_code(&agreement.tariff_code)?;
    let tariff_family = if stream.kind == "gas" {
        "gas-tariffs"
    } else {
        "electricity-tariffs"
    };
    let endpoint = match rate_type {
        "standard" => "standard-unit-rates",
        "day" => "day-unit-rates",
        "night" => "night-unit-rates",
        "standing" => "standing-charges",
        _ => return Err(format!("unsupported Octopus rate type '{rate_type}'")),
    };
    let api_base = base_url(settings);
    let allowed_page_prefix = format!("{api_base}/");
    let from = DateTime::<Utc>::from_timestamp(request_start, 0)
        .ok_or("invalid tariff agreement start")?;
    let to =
        DateTime::<Utc>::from_timestamp(request_end, 0).ok_or("invalid tariff agreement end")?;
    let mut url = format!(
        "{api_base}/v1/products/{}/{}/{}/{}/?period_from={}&period_to={}&page_size=1500",
        encode(product),
        tariff_family,
        encode(&agreement.tariff_code),
        endpoint,
        encode(&from.to_rfc3339()),
        encode(&to.to_rfc3339()),
    );
    let mut selected: std::collections::HashMap<(i64, Option<i64>), (u8, PriceResult)> =
        std::collections::HashMap::new();
    loop {
        let page: PricePage =
            serde_json::from_value(get_json(url, settings.octopus_api_key.clone()).await?)
                .map_err(|e| format!("invalid Octopus tariff response: {e}"))?;
        for price in page.results {
            // Agile rates can legitimately be negative; reject only non-finite
            // corruption, not a real paid-to-consume interval.
            if !price.value_inc_vat.is_finite() {
                continue;
            }
            let from = parse_timestamp(&price.valid_from)?;
            let to = price.valid_to.as_deref().map(parse_timestamp).transpose()?;
            let priority = payment_priority(price.payment_method.as_deref());
            let key = (from, to);
            if selected
                .get(&key)
                .is_none_or(|(current, _)| priority > *current)
            {
                selected.insert(key, (priority, price));
            }
        }
        match validated_next_url(page.next, &allowed_page_prefix)? {
            Some(next) => url = next,
            None => break,
        }
    }

    let mut rows = Vec::new();
    for ((price_start, price_end), (_, price)) in selected {
        let valid_from = price_start.max(agreement_start);
        let valid_to = match (price_end, agreement_end) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        if valid_to.is_some_and(|end| end <= valid_from) {
            continue;
        }
        rows.push(OctopusTariffPriceRow {
            meter_kind: stream.kind.clone(),
            meter_point: stream.meter_point.clone(),
            tariff_code: agreement.tariff_code.clone(),
            valid_from,
            valid_to,
            value_inc_vat: price.value_inc_vat,
            rate_type: rate_type.to_string(),
        });
    }
    Ok(rows)
}

async fn sync_tariffs(
    settings: &Settings,
    db: &HistoryDb,
    streams: &[Stream],
    now: i64,
) -> (usize, Option<String>) {
    let mut seen = std::collections::HashSet::new();
    let mut stored = 0usize;
    let mut errors = Vec::new();
    for stream in streams {
        for agreement in &stream.agreements {
            let key = format!(
                "{}:{}:{}:{}",
                stream.kind, stream.meter_point, agreement.tariff_code, agreement.valid_from
            );
            if !seen.insert(key) {
                continue;
            }
            for rate_type in tariff_rate_types(&agreement.tariff_code) {
                let already_imported = db.has_octopus_tariff_prices(
                    &stream.kind,
                    &stream.meter_point,
                    &agreement.tariff_code,
                    rate_type,
                );
                let refresh_from = already_imported.then_some(now - RECENT_REFRESH_DAYS * 86400);
                match fetch_tariff_prices(settings, stream, agreement, rate_type, now, refresh_from)
                    .await
                {
                    Ok(rows) => match db.upsert_octopus_tariff_prices(&rows) {
                        Ok(count) => stored += count,
                        Err(error) => errors.push(error),
                    },
                    Err(error) => errors.push(format!("{}: {error}", agreement.tariff_code)),
                }
            }
        }
    }
    let error = if errors.is_empty() {
        None
    } else {
        let total = errors.len();
        let preview = errors.into_iter().take(3).collect::<Vec<_>>().join("; ");
        Some(if total > 3 {
            format!("{preview}; and {} more tariff error(s)", total - 3)
        } else {
            preview
        })
    };
    (stored, error)
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
        let (tariff_prices, tariff_error) = sync_tariffs(&settings, &db, &streams, now).await;
        let mut all_complete = true;
        for stream in &streams {
            let (count, complete) = backfill_stream(&settings, &db, stream).await?;
            imported += count;
            all_complete &= complete;
        }
        Ok::<_, String>((
            streams.len(),
            imported,
            all_complete,
            tariff_prices,
            tariff_error,
        ))
    }
    .await;

    let mut status = state.octopus.lock().await;
    status.syncing = false;
    match result {
        Ok((streams, imported, complete, tariff_prices, tariff_error)) => {
            status.last_sync_at = Some(Utc::now());
            status.last_error = None;
            status.discovered_streams = streams;
            status.imported_intervals = status.imported_intervals.saturating_add(imported);
            status.backfill_complete = complete;
            status.tariff_prices = tariff_prices;
            status.last_tariff_error = tariff_error;
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

pub async fn get_comparison(
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
    let bounds = state
        .history
        .lock()
        .await
        .clone()
        .and_then(|db| db.octopus_bounds());
    let span = match query.range.as_deref().unwrap_or("30d") {
        "7d" => 7 * 86400,
        "30d" => 30 * 86400,
        "6m" => 180 * 86400,
        "1y" => 365 * 86400,
        "all" => now - bounds.map(|b| b.0).unwrap_or(now - 365 * 86400),
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
    match tokio::task::spawn_blocking(move || db.query_octopus_comparison(start, end)).await {
        Ok(Ok(report)) => (StatusCode::OK, Json(json!({"ok": true, "data": report}))),
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

pub async fn get_summary(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HistoryQuery>,
) -> (StatusCode, Json<Value>) {
    let settings = Settings::load();
    if !configured(&settings) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "Octopus integration is not configured"})),
        );
    }
    let now = Utc::now().timestamp();
    let offset = query.offset.unwrap_or(0).max(0);
    let bounds = state
        .history
        .lock()
        .await
        .clone()
        .and_then(|db| db.octopus_bounds());
    let span = match query.range.as_deref().unwrap_or("30d") {
        "7d" => 7 * 86400,
        "30d" => 30 * 86400,
        "6m" => 180 * 86400,
        "1y" => 365 * 86400,
        "all" => now - bounds.map(|b| b.0).unwrap_or(now - 365 * 86400),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": "Invalid range. Use 7d, 30d, 6m, 1y, or all"})),
            )
        }
    };
    let end = now - offset * span;
    let start = end - span;
    let gas_is_kwh = settings.octopus_gas_unit == "kwh";
    let economy7_start = hhmm_minutes(&settings.octopus_economy7_start).unwrap_or(30);
    let economy7_end = hhmm_minutes(&settings.octopus_economy7_end).unwrap_or(450);
    let db = state.history.lock().await.clone();
    let Some(db) = db else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": "History database is unavailable"})),
        );
    };
    match tokio::task::spawn_blocking(move || {
        db.query_octopus_billing(start, end, gas_is_kwh, economy7_start, economy7_end)
    })
    .await
    {
        Ok(Ok(report)) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "data": report,
                "gas_unit": settings.octopus_gas_unit,
                "estimated": true
            })),
        ),
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
        let mut settings = Settings {
            octopus_enabled: true,
            octopus_api_key: "sk_live".into(),
            ..Settings::default()
        };
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
    fn tariff_product_is_derived_from_import_export_and_gas_codes() {
        assert_eq!(
            tariff_product_code("E-1R-AGILE-24-10-01-A").unwrap(),
            "AGILE-24-10-01"
        );
        assert_eq!(
            tariff_product_code("G-1R-VAR-22-11-01-N").unwrap(),
            "VAR-22-11-01"
        );
        assert_eq!(
            tariff_product_code("E-2R-GO-VAR-22-10-14-C").unwrap(),
            "GO-VAR-22-10-14"
        );
        assert!(tariff_product_code("not-a-tariff").is_err());
    }

    #[test]
    fn economy7_tariffs_request_separate_day_and_night_feeds() {
        assert_eq!(
            tariff_rate_types("E-2R-VAR-22-11-01-H"),
            &["day", "night", "standing"]
        );
        assert_eq!(
            tariff_rate_types("E-1R-AGILE-24-10-01-H"),
            &["standard", "standing"]
        );
    }

    #[test]
    fn parses_economy7_clock_times() {
        assert_eq!(hhmm_minutes("00:30"), Some(30));
        assert_eq!(hhmm_minutes("07:30"), Some(450));
        assert_eq!(hhmm_minutes("24:00"), None);
        assert_eq!(hhmm_minutes("bad"), None);
    }

    #[test]
    fn direct_debit_price_has_priority_over_other_payment_methods() {
        assert!(payment_priority(Some("DIRECT_DEBIT")) > payment_priority(None));
        assert!(payment_priority(None) > payment_priority(Some("NON_DIRECT_DEBIT")));
    }

    #[tokio::test]
    async fn comparison_endpoint_returns_report_when_configured() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let settings = Settings {
                octopus_enabled: true,
                octopus_api_key: "sk_test".to_string(),
                octopus_account_number: "A-TEST".to_string(),
                ..Settings::default()
            };
            settings.save().unwrap();
            let state = Arc::new(AppState::new());
            let path = std::env::temp_dir().join(format!(
                "givenergy-octopus-comparison-{}.db",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&path);
            let db = Arc::new(HistoryDb::open(&path).unwrap());
            *state.history.lock().await = Some(db);

            let (status, body) = get_comparison(
                State(state),
                Query(HistoryQuery {
                    range: Some("7d".to_string()),
                    offset: None,
                }),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["ok"], true);
            assert!(matches!(
                body["data"]["days"].as_array().unwrap().len(),
                7 | 8
            ));
            assert_eq!(body["data"]["import_stream_available"], false);
        })
        .await;
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
