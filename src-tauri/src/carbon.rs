//! Grid carbon intensity for the house view (issue #401).
//!
//! The National Energy System Operator's free Carbon Intensity API
//! (<https://carbonintensity.org.uk/>) publishes a half-hourly forecast of
//! grid carbon intensity (gCO₂/kWh) for the next 24 hours for each GB
//! distribution region. `GET /api/carbon-intensity` returns that
//! forecast; the frontend picks the current period and the lowest one ahead.
//!
//! Fetched on demand only, so nothing leaves the machine unless the house view
//! is open and weather is on. Invariants:
//!
//! - Shown alongside the weather, so it's off until the weather integration
//!   is turned on — the user's opt-in to location-based outside requests.
//! - Region comes from the weather postcode's outward code (`RG10` of
//!   `RG10 9XX`). Without one nothing is shown — weather may be running on
//!   typed-in coordinates anywhere — and neither is it for a postcode that
//!   isn't a GB one (an Eircode, a typo) or a Northern Ireland one (a separate
//!   grid the API doesn't cover): never another region's figures.
//! - A forecast for one area is never served for another: changing the
//!   postcode drops the cached one.
//! - At most one fetch is in flight, and an attempt counts from when it
//!   starts, so one cut short by a client disconnect still waits out the
//!   retry gap; a good forecast is reused for 30 minutes
//!   and a failure is retried no sooner than 5 minutes later, so a down API
//!   costs one request per 5 minutes however many screens are open.
//! - A failed fetch never fails the endpoint: it answers with the last good
//!   forecast for the same area, or `null`.
//! - Ages use the wall clock (the forecast is stamped with it), and a stamp in
//!   the future counts as stale, so a clock jump either way refetches; the
//!   retry backoff uses the monotonic clock.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};

use crate::inverter::poll::AppState;

pub const DEFAULT_CARBON_API_BASE: &str = "https://api.carbonintensity.org.uk";

/// How long a good forecast is reused. The API updates its forecast every
/// half hour.
const FRESH_FOR: Duration = Duration::from_secs(30 * 60);
/// Minimum gap between attempts after a failure.
const RETRY_AFTER: Duration = Duration::from_secs(5 * 60);

/// One half-hour forecast period.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CarbonPeriod {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// Forecast intensity, gCO₂/kWh.
    pub forecast: u32,
    /// The API's band: `very low`, `low`, `moderate`, `high` or `very high`.
    pub index: String,
}

/// What `GET /api/carbon-intensity` returns as `data`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CarbonForecast {
    /// Region name, e.g. `South England`, when the API gives one.
    pub region: Option<String>,
    pub fetched_at: DateTime<Utc>,
    pub periods: Vec<CarbonPeriod>,
}

/// Which forecast to fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarbonArea {
    /// A GB outward code, e.g. `RG10`.
    Outcode(String),
    /// No postcode, or not one the GB grid covers: show nothing.
    Unavailable,
}

impl CarbonArea {
    /// The area for the weather config: nothing while weather is off.
    pub fn for_weather(config: &crate::settings::WeatherConfig) -> Self {
        if config.enabled {
            Self::from_postcode(&config.postcode)
        } else {
            Self::Unavailable
        }
    }

    /// The area for the weather postcode.
    pub fn from_postcode(postcode: &str) -> Self {
        let compact: String = postcode
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_uppercase();
        // Northern Ireland (BT) is on the all-island grid, not GB's.
        if compact.starts_with("BT") || !compact.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Self::Unavailable;
        }
        let bytes = compact.as_bytes();
        // A full postcode ends with the inward code: a digit and two letters.
        let full = compact.len() >= 5
            && bytes[bytes.len() - 3].is_ascii_digit()
            && bytes[bytes.len() - 2].is_ascii_alphabetic()
            && bytes[bytes.len() - 1].is_ascii_alphabetic();
        let outcode = if full {
            &compact[..compact.len() - 3]
        } else {
            compact.as_str()
        };
        if is_outcode(outcode) {
            Self::Outcode(outcode.to_string())
        } else {
            Self::Unavailable
        }
    }
}

/// A GB outward code: 2–4 characters, one or two letters then a digit
/// (`M1`, `RG10`, `EC1A`, `SW1W`).
fn is_outcode(s: &str) -> bool {
    let b = s.as_bytes();
    if !(2..=4).contains(&b.len()) || !b[0].is_ascii_alphabetic() {
        return false;
    }
    let letters = b.iter().take_while(|c| c.is_ascii_alphabetic()).count();
    letters <= 2 && b.get(letters).is_some_and(|c| c.is_ascii_digit())
}

/// The 24-hour forecast URL, starting half an hour before `now` so the
/// response always includes the current period.
pub fn forecast_url(base: &str, area: &CarbonArea, now: DateTime<Utc>) -> Option<String> {
    let base = base.trim_end_matches('/');
    let from = (now - chrono::Duration::minutes(30)).format("%Y-%m-%dT%H:%MZ");
    match area {
        CarbonArea::Outcode(oc) => Some(format!(
            "{base}/regional/intensity/{from}/fw24h/postcode/{oc}"
        )),
        CarbonArea::Unavailable => None,
    }
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    // The API writes `2026-11-01T13:00Z` (no seconds), which isn't RFC 3339.
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%MZ")
        .ok()
        .map(|t| t.and_utc())
        .or_else(|| {
            DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|t| t.with_timezone(&Utc))
        })
}

/// Parse a regional forecast response. Periods without a valid
/// time or forecast are skipped; a response with none left is an error.
pub fn parse_forecast(body: &Value, fetched_at: DateTime<Utc>) -> Result<CarbonForecast, String> {
    if let Some(msg) = body["error"]["message"].as_str() {
        return Err(format!("API error: {msg}"));
    }
    let data = &body["data"];
    // `data` is the region, with its periods under `data.data`.
    let region = data["shortname"].as_str().map(str::to_string);
    let rows = data["data"]
        .as_array()
        .ok_or_else(|| "missing periods".to_string())?;
    let mut periods: Vec<CarbonPeriod> = rows
        .iter()
        .filter_map(|r| {
            let forecast = r["intensity"]["forecast"].as_u64()?;
            Some(CarbonPeriod {
                from: parse_time(r["from"].as_str()?)?,
                to: parse_time(r["to"].as_str()?)?,
                forecast: u32::try_from(forecast).ok()?,
                index: r["intensity"]["index"].as_str().unwrap_or("").to_string(),
            })
        })
        .filter(|p| p.to > p.from)
        .collect();
    if periods.is_empty() {
        return Err("no forecast periods".to_string());
    }
    periods.sort_by_key(|p| p.from);
    Ok(CarbonForecast {
        region,
        fetched_at,
        periods,
    })
}

/// Fetch and parse `url`. Blocking: run it on a blocking thread.
fn fetch_blocking(url: &str, now: DateTime<Utc>) -> Result<CarbonForecast, String> {
    let mut resp = crate::weather::weather_agent()
        .get(url)
        .header("Accept", "application/json")
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .map_err(|e| format!("HTTP error: {e}"))?;
    let status = resp.status();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("read error: {e}"))?;
    let json: Value =
        serde_json::from_str(&body).map_err(|e| format!("HTTP {status}, JSON error: {e}"))?;
    // An unknown postcode comes back as 400 with an `error` body.
    let parsed = parse_forecast(&json, now);
    if !status.is_success() {
        return Err(parsed.err().unwrap_or_else(|| format!("HTTP {status}")));
    }
    parsed
}

/// The last forecast and when it was last tried, for one area.
#[derive(Debug)]
pub struct CarbonCache {
    /// API base URL; tests point it at a local server.
    pub base_url: String,
    area: Option<CarbonArea>,
    forecast: Option<CarbonForecast>,
    last_failure: Option<Instant>,
}

impl Default for CarbonCache {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_CARBON_API_BASE.to_string(),
            area: None,
            forecast: None,
            last_failure: None,
        }
    }
}

impl CarbonCache {
    /// The forecast for `area`, fetching through `fetch` when the cached one
    /// is missing or stale and no recent attempt failed.
    pub async fn get<F, Fut>(
        &mut self,
        area: CarbonArea,
        now: DateTime<Utc>,
        now_mono: Instant,
        fetch: F,
    ) -> Option<CarbonForecast>
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = Result<CarbonForecast, String>>,
    {
        if self.area.as_ref() != Some(&area) {
            self.area = Some(area.clone());
            self.forecast = None;
            self.last_failure = None;
        }
        let url = forecast_url(&self.base_url, &area, now)?;
        let fresh = self.forecast.as_ref().is_some_and(|f| {
            (now - f.fetched_at)
                .to_std()
                .is_ok_and(|age| age < FRESH_FOR)
        });
        let backing_off = self
            .last_failure
            .is_some_and(|t| now_mono.saturating_duration_since(t) < RETRY_AFTER);
        if !fresh && !backing_off {
            // Counted as failed until it succeeds: if the request is dropped
            // mid-fetch (client disconnect), the next one still waits.
            self.last_failure = Some(now_mono);
            match fetch(url).await {
                Ok(forecast) => {
                    self.forecast = Some(forecast);
                    self.last_failure = None;
                }
                Err(e) => tracing::debug!("carbon intensity fetch failed: {e}"),
            }
        }
        self.forecast.clone()
    }
}

/// GET /api/carbon-intensity — the 24-hour grid carbon intensity forecast
/// for the weather postcode's region, or `null` when weather is off or
/// there's no forecast.
pub async fn get_carbon_intensity(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let area = CarbonArea::for_weather(&state.weather.lock().await.config);
    // Held across the fetch so concurrent requests share one; nothing else
    // takes this lock.
    let mut cache = state.carbon.lock().await;
    let data = cache
        .get(area, Utc::now(), Instant::now(), |url| async move {
            let now = Utc::now();
            tokio::task::spawn_blocking(move || fetch_blocking(&url, now))
                .await
                .map_err(|e| format!("spawn_blocking failed: {e}"))?
        })
        .await;
    (StatusCode::OK, Json(json!({ "ok": true, "data": data })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io::{Read, Write};

    fn t(s: &str) -> DateTime<Utc> {
        parse_time(s).unwrap()
    }

    fn forecast_at(fetched_at: &str, value: u32) -> CarbonForecast {
        CarbonForecast {
            region: None,
            fetched_at: t(fetched_at),
            periods: vec![CarbonPeriod {
                from: t("2026-11-01T13:00Z"),
                to: t("2026-11-01T13:30Z"),
                forecast: value,
                index: "moderate".into(),
            }],
        }
    }

    fn rg10() -> CarbonArea {
        CarbonArea::Outcode("RG10".into())
    }

    // ---- area from postcode --------------------------------------------

    #[test]
    fn a_full_postcode_gives_its_outward_code() {
        for (pc, oc) in [
            ("RG10 9XX", "RG10"),
            ("rg109xx", "RG10"),
            ("M1 1AE", "M1"),
            ("EC1A 1BB", "EC1A"),
            ("SW1W 0NY", "SW1W"),
            (" b33  8th ", "B33"),
        ] {
            assert_eq!(
                CarbonArea::from_postcode(pc),
                CarbonArea::Outcode(oc.into()),
                "{pc}"
            );
        }
    }

    #[test]
    fn an_outward_code_alone_is_used_as_is() {
        assert_eq!(
            CarbonArea::from_postcode("RG10"),
            CarbonArea::Outcode("RG10".into())
        );
        assert_eq!(
            CarbonArea::from_postcode("m1"),
            CarbonArea::Outcode("M1".into())
        );
    }

    #[test]
    fn a_postcode_that_is_not_a_gb_one_shows_nothing() {
        for pc in [
            "D02 X285", "12345", "RG10/9XX", "ABCDE", "R", "RG1000", "ÄB1 2CD",
        ] {
            assert_eq!(
                CarbonArea::from_postcode(pc),
                CarbonArea::Unavailable,
                "{pc}"
            );
        }
    }

    #[test]
    fn nothing_is_fetched_while_weather_is_off() {
        let mut config = crate::settings::WeatherConfig {
            postcode: "RG10 9XX".into(),
            ..Default::default()
        };
        assert_eq!(CarbonArea::for_weather(&config), CarbonArea::Unavailable);
        config.enabled = true;
        assert_eq!(
            CarbonArea::for_weather(&config),
            CarbonArea::Outcode("RG10".into())
        );
        config.postcode.clear();
        assert_eq!(CarbonArea::for_weather(&config), CarbonArea::Unavailable);
    }

    #[test]
    fn coordinates_without_a_postcode_show_nothing() {
        // Weather can run on typed-in coordinates anywhere; only a GB
        // postcode says the GB grid is the right one.
        let config = crate::settings::WeatherConfig {
            enabled: true,
            latitude: Some(53.35),
            longitude: Some(-6.26),
            ..Default::default()
        };
        assert_eq!(CarbonArea::for_weather(&config), CarbonArea::Unavailable);
        assert_eq!(CarbonArea::from_postcode(""), CarbonArea::Unavailable);
        assert_eq!(CarbonArea::from_postcode("   "), CarbonArea::Unavailable);
    }

    #[test]
    fn northern_ireland_postcodes_show_nothing() {
        // A separate grid the API doesn't cover: never asked for.
        for pc in ["BT1 1AA", "bt47 6ab", "BT7"] {
            assert_eq!(
                CarbonArea::from_postcode(pc),
                CarbonArea::Unavailable,
                "{pc}"
            );
        }
    }

    // ---- URL -----------------------------------------------------------

    #[test]
    fn urls_start_half_an_hour_back_for_the_area() {
        let now = t("2026-11-01T13:47Z");
        assert_eq!(
            forecast_url("https://x/", &CarbonArea::Outcode("RG10".into()), now).unwrap(),
            "https://x/regional/intensity/2026-11-01T13:17Z/fw24h/postcode/RG10"
        );
        assert_eq!(
            forecast_url("https://x", &CarbonArea::Unavailable, now),
            None
        );
    }

    // ---- parsing -------------------------------------------------------

    #[test]
    fn periods_are_put_in_time_order_and_a_missing_region_is_none() {
        let body = json!({"data": {"data": [
            {"from": "2026-11-01T13:30Z", "to": "2026-11-01T14:00Z",
             "intensity": {"forecast": 168, "index": "moderate"}},
            {"from": "2026-11-01T13:00Z", "to": "2026-11-01T13:30Z",
             "intensity": {"forecast": 161, "index": "moderate"}},
        ]}});
        let f = parse_forecast(&body, t("2026-11-01T13:10Z")).unwrap();
        assert_eq!(f.region, None);
        assert_eq!(f.periods.len(), 2);
        assert_eq!(f.periods[0].from, t("2026-11-01T13:00Z"));
        assert_eq!(f.periods[0].forecast, 161);
        assert_eq!(f.periods[1].forecast, 168);
    }

    #[test]
    fn a_regional_response_carries_the_region_name() {
        let body = json!({"data": {
            "regionid": 12, "shortname": "South England", "postcode": "RG10",
            "data": [{"from": "2026-11-01T13:00Z", "to": "2026-11-01T13:30Z",
                      "intensity": {"forecast": 183, "index": "high"},
                      "generationmix": []}]
        }});
        let f = parse_forecast(&body, t("2026-11-01T13:10Z")).unwrap();
        assert_eq!(f.region.as_deref(), Some("South England"));
        assert_eq!(f.periods[0].index, "high");
    }

    #[test]
    fn bad_periods_are_skipped_and_none_left_is_an_error() {
        let body = json!({"data": {"data": [
            {"from": "garbage", "to": "2026-11-01T13:30Z", "intensity": {"forecast": 1}},
            {"from": "2026-11-01T13:00Z", "to": "2026-11-01T13:30Z", "intensity": {"forecast": -5}},
            {"from": "2026-11-01T13:30Z", "to": "2026-11-01T13:00Z", "intensity": {"forecast": 9}},
            {"from": "2026-11-01T14:00Z", "to": "2026-11-01T14:30Z", "intensity": {"forecast": 120}},
        ]}});
        let f = parse_forecast(&body, t("2026-11-01T13:10Z")).unwrap();
        assert_eq!(f.periods.len(), 1);
        assert_eq!(f.periods[0].forecast, 120);
        assert_eq!(f.periods[0].index, "", "a missing index is blank");

        let none = json!({"data": {"data": [{"from": "x", "to": "y", "intensity": {}}]}});
        assert!(parse_forecast(&none, t("2026-11-01T13:10Z")).is_err());
        assert!(parse_forecast(&json!({"data": []}), t("2026-11-01T13:10Z")).is_err());
        assert!(parse_forecast(&json!({}), t("2026-11-01T13:10Z")).is_err());
    }

    #[test]
    fn an_error_body_is_reported() {
        let body = json!({"error": {"code": "400 Bad Request", "message": "No postcode match can be found."}});
        let err = parse_forecast(&body, t("2026-11-01T13:10Z")).unwrap_err();
        assert!(err.contains("No postcode match"), "{err}");
    }

    // ---- cache ---------------------------------------------------------

    /// Run `cache.get` with a fetcher that records its calls and answers `result`.
    fn get(
        cache: &mut CarbonCache,
        area: CarbonArea,
        now: &str,
        mono: Instant,
        calls: &Cell<u32>,
        result: Result<CarbonForecast, String>,
    ) -> Option<CarbonForecast> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(cache.get(area, t(now), mono, |_url| {
            calls.set(calls.get() + 1);
            async move { result }
        }))
    }

    #[test]
    fn a_fresh_forecast_is_reused_for_half_an_hour() {
        let mut cache = CarbonCache::default();
        let calls = Cell::new(0);
        let m0 = Instant::now();
        let a = rg10();
        let got = get(
            &mut cache,
            a.clone(),
            "2026-11-01T13:00Z",
            m0,
            &calls,
            Ok(forecast_at("2026-11-01T13:00Z", 100)),
        );
        assert_eq!(got.unwrap().periods[0].forecast, 100);
        let got = get(
            &mut cache,
            a.clone(),
            "2026-11-01T13:29Z",
            m0,
            &calls,
            Ok(forecast_at("2026-11-01T13:29Z", 200)),
        );
        assert_eq!(got.unwrap().periods[0].forecast, 100, "still cached");
        assert_eq!(calls.get(), 1);
        let got = get(
            &mut cache,
            a,
            "2026-11-01T13:30Z",
            m0,
            &calls,
            Ok(forecast_at("2026-11-01T13:30Z", 300)),
        );
        assert_eq!(
            got.unwrap().periods[0].forecast,
            300,
            "refetched once stale"
        );
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn a_failure_keeps_the_last_forecast_and_waits_before_retrying() {
        let mut cache = CarbonCache::default();
        let calls = Cell::new(0);
        let m0 = Instant::now();
        let a = rg10();
        get(
            &mut cache,
            a.clone(),
            "2026-11-01T13:00Z",
            m0,
            &calls,
            Ok(forecast_at("2026-11-01T13:00Z", 100)),
        );
        // Stale: tries, fails, keeps serving the old one.
        let got = get(
            &mut cache,
            a.clone(),
            "2026-11-01T13:40Z",
            m0,
            &calls,
            Err("down".into()),
        );
        assert_eq!(got.unwrap().periods[0].forecast, 100);
        assert_eq!(calls.get(), 2);
        // Within 5 minutes of the failure: no new attempt.
        let got = get(
            &mut cache,
            a.clone(),
            "2026-11-01T13:44Z",
            m0 + Duration::from_secs(299),
            &calls,
            Ok(forecast_at("2026-11-01T13:44Z", 200)),
        );
        assert_eq!(got.unwrap().periods[0].forecast, 100);
        assert_eq!(calls.get(), 2);
        // After it: retries and succeeds.
        let got = get(
            &mut cache,
            a,
            "2026-11-01T13:45Z",
            m0 + Duration::from_secs(300),
            &calls,
            Ok(forecast_at("2026-11-01T13:45Z", 200)),
        );
        assert_eq!(got.unwrap().periods[0].forecast, 200);
        assert_eq!(calls.get(), 3);
    }

    #[tokio::test]
    async fn a_fetch_cut_short_by_a_disconnect_still_waits_before_retrying() {
        let mut cache = CarbonCache::default();
        let calls = Cell::new(0);
        let m0 = Instant::now();
        let a = CarbonArea::Outcode("RG10".into());
        // The client goes away mid-fetch: Axum drops the handler future.
        let cut = tokio::time::timeout(
            Duration::from_millis(10),
            cache.get(a.clone(), t("2026-11-01T13:00Z"), m0, |_url| {
                calls.set(calls.get() + 1);
                std::future::pending::<Result<CarbonForecast, String>>()
            }),
        )
        .await;
        assert!(cut.is_err());
        // The next request, a minute later, mustn't start a second fetch.
        let got = cache
            .get(
                a,
                t("2026-11-01T13:01Z"),
                m0 + Duration::from_secs(60),
                |_url| {
                    calls.set(calls.get() + 1);
                    async { Ok(forecast_at("2026-11-01T13:01Z", 100)) }
                },
            )
            .await;
        assert_eq!(got, None);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn a_first_failure_answers_nothing() {
        let mut cache = CarbonCache::default();
        let calls = Cell::new(0);
        let got = get(
            &mut cache,
            rg10(),
            "2026-11-01T13:00Z",
            Instant::now(),
            &calls,
            Err("down".into()),
        );
        assert_eq!(got, None);
    }

    #[test]
    fn changing_area_never_serves_the_old_forecast_and_skips_the_backoff() {
        let mut cache = CarbonCache::default();
        let calls = Cell::new(0);
        let m0 = Instant::now();
        get(
            &mut cache,
            CarbonArea::Outcode("RG10".into()),
            "2026-11-01T13:00Z",
            m0,
            &calls,
            Ok(forecast_at("2026-11-01T13:00Z", 100)),
        );
        // New postcode whose first fetch fails: nothing, not RG10's figures.
        let got = get(
            &mut cache,
            CarbonArea::Outcode("M1".into()),
            "2026-11-01T13:01Z",
            m0,
            &calls,
            Err("down".into()),
        );
        assert_eq!(got, None);
        // Back to RG10 within the backoff: the area changed, so it fetches.
        let got = get(
            &mut cache,
            CarbonArea::Outcode("RG10".into()),
            "2026-11-01T13:02Z",
            m0,
            &calls,
            Ok(forecast_at("2026-11-01T13:02Z", 150)),
        );
        assert_eq!(got.unwrap().periods[0].forecast, 150);
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn an_unavailable_area_never_fetches() {
        let mut cache = CarbonCache::default();
        let calls = Cell::new(0);
        get(
            &mut cache,
            rg10(),
            "2026-11-01T13:00Z",
            Instant::now(),
            &calls,
            Ok(forecast_at("2026-11-01T13:00Z", 100)),
        );
        let got = get(
            &mut cache,
            CarbonArea::Unavailable,
            "2026-11-01T13:01Z",
            Instant::now(),
            &calls,
            Ok(forecast_at("2026-11-01T13:01Z", 1)),
        );
        assert_eq!(
            got, None,
            "the last area's forecast isn't served once there is none"
        );
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn a_clock_jump_either_way_refetches() {
        let m0 = Instant::now();
        // Backwards: the cached stamp is now in the future.
        let mut cache = CarbonCache::default();
        let calls = Cell::new(0);
        get(
            &mut cache,
            rg10(),
            "2026-11-01T13:00Z",
            m0,
            &calls,
            Ok(forecast_at("2026-11-01T13:00Z", 100)),
        );
        let got = get(
            &mut cache,
            rg10(),
            "2026-11-01T12:00Z",
            m0,
            &calls,
            Ok(forecast_at("2026-11-01T12:00Z", 200)),
        );
        assert_eq!(got.unwrap().periods[0].forecast, 200);
        // Forwards by a day.
        let got = get(
            &mut cache,
            rg10(),
            "2026-11-02T12:00Z",
            m0,
            &calls,
            Ok(forecast_at("2026-11-02T12:00Z", 300)),
        );
        assert_eq!(got.unwrap().periods[0].forecast, 300);
        assert_eq!(calls.get(), 3);
    }

    // ---- handler -------------------------------------------------------

    /// A local API that counts connections and answers each after `delay`.
    async fn counting_api(delay: Duration) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = stream.read(&mut buf).await;
                    tokio::time::sleep(delay).await;
                    let now = Utc::now();
                    let from = now.format("%Y-%m-%dT%H:%MZ");
                    let to = (now + chrono::Duration::minutes(30)).format("%Y-%m-%dT%H:%MZ");
                    let payload = json!({"data": {"shortname": "South England", "data": [
                        {"from": from.to_string(), "to": to.to_string(), "intensity": {"forecast": 150, "index": "moderate"}}
                    ]}}).to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        (base, hits)
    }

    async fn state_with(base: &str, enabled: bool, postcode: &str) -> Arc<AppState> {
        let state = Arc::new(AppState::new());
        state.carbon.lock().await.base_url = base.to_string();
        let mut weather = state.weather.lock().await;
        weather.config.enabled = enabled;
        weather.config.postcode = postcode.to_string();
        drop(weather);
        state
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_endpoint_answers_null_without_asking_the_api_while_weather_is_off() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let (base, hits) = counting_api(Duration::ZERO).await;
            for (enabled, postcode) in [(false, "RG10 9XX"), (true, ""), (true, "BT1 1AA")] {
                let state = state_with(&base, enabled, postcode).await;
                let (status, Json(body)) = get_carbon_intensity(State(state)).await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(
                    body,
                    json!({"ok": true, "data": null}),
                    "{enabled} {postcode}"
                );
            }
            assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_requests_share_one_fetch() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let (base, hits) = counting_api(Duration::from_millis(200)).await;
            let state = state_with(&base, true, "RG10 9XX").await;
            let requests: Vec<_> = (0..5)
                .map(|_| tokio::spawn(get_carbon_intensity(State(state.clone()))))
                .collect();
            for r in requests {
                let (_, Json(body)) = r.await.unwrap();
                assert_eq!(body["data"]["region"], "South England");
                assert_eq!(body["data"]["periods"][0]["forecast"], 150);
            }
            assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        })
        .await;
    }

    // ---- fetch (local HTTP server, hermetic) -----------------------------

    fn serve_once(status: &str, payload: &str) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        );
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });
        (url, handle)
    }

    #[test]
    fn a_regional_forecast_is_fetched_from_the_api() {
        let payload = json!({"data": {"shortname": "South England", "data": [
            {"from": "2026-11-01T13:00Z", "to": "2026-11-01T13:30Z", "intensity": {"forecast": 183, "index": "high"}}
        ]}}).to_string();
        let (base, server) = serve_once("200 OK", &payload);
        let now = t("2026-11-01T13:10Z");
        let url = forecast_url(&base, &CarbonArea::Outcode("RG10".into()), now).unwrap();
        let f = fetch_blocking(&url, now).unwrap();
        let request = server.join().unwrap();
        assert!(
            request.starts_with("GET /regional/intensity/2026-11-01T12:40Z/fw24h/postcode/RG10 "),
            "{request}"
        );
        assert_eq!(f.region.as_deref(), Some("South England"));
        assert_eq!(f.fetched_at, now);
    }

    #[test]
    fn an_unknown_postcode_reports_the_api_message() {
        let payload = json!({"error": {"code": "400 Bad Request", "message": "No postcode match can be found."}}).to_string();
        let (base, server) = serve_once("400 Bad Request", &payload);
        let now = t("2026-11-01T13:10Z");
        let url = forecast_url(&base, &CarbonArea::Outcode("BT1".into()), now).unwrap();
        let err = fetch_blocking(&url, now).unwrap_err();
        server.join().unwrap();
        assert!(err.contains("No postcode match"), "{err}");
    }

    #[test]
    fn a_server_error_with_a_forecast_body_is_still_an_error() {
        let payload = json!({"data": {"data": [
            {"from": "2026-11-01T13:00Z", "to": "2026-11-01T13:30Z", "intensity": {"forecast": 1, "index": "low"}}
        ]}}).to_string();
        let (base, server) = serve_once("500 Internal Server Error", &payload);
        let now = t("2026-11-01T13:10Z");
        let url = forecast_url(&base, &rg10(), now).unwrap();
        let err = fetch_blocking(&url, now).unwrap_err();
        server.join().unwrap();
        assert!(err.contains("500"), "{err}");
    }

    #[test]
    fn a_non_json_body_is_an_error() {
        let (base, server) = serve_once("502 Bad Gateway", "<html>oops</html>");
        let now = t("2026-11-01T13:10Z");
        let url = forecast_url(&base, &rg10(), now).unwrap();
        let err = fetch_blocking(&url, now).unwrap_err();
        server.join().unwrap();
        assert!(err.contains("JSON error"), "{err}");
    }
}
