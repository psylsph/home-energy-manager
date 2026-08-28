//! Solar radiation forecast — data-source layer (issue #283, Phase 0).
//!
//! Fetches hourly solar radiation variables from the same free Open-Meteo
//! forecast endpoint the weather module already uses (`/v1/forecast`, CC-BY
//! 4.0 — attribution is already displayed for weather). The request asks
//! for `past_days` of modelled radiation alongside the forward forecast so
//! the performance-ratio calibration in [`crate::forecast::calibration`]
//! can compare delivered radiation against the site's actual generation
//! without a second API surface.
//!
//! Providers sit behind [`SolarForecastProvider`] so a future source (e.g.
//! a Solcast hobbyist account) can slot in without touching callers.

use crate::weather::weather_agent;

/// One hourly sample of the solar-relevant variables.
///
/// Timestamps are epoch seconds on UTC hour boundaries (the fetch passes
/// `timezone=UTC`), matching the storage convention of
/// `history.db::weather_observations`.
#[derive(Debug, Clone, PartialEq)]
pub struct SolarForecastSample {
    pub timestamp: i64,
    /// Global horizontal irradiance, W/m². Always present — rows without
    /// it are skipped at parse time since power cannot be modelled from
    /// them.
    pub shortwave_radiation: f32,
    /// Direct beam irradiance, W/m². Optional at the source; `None` when
    /// the provider omits or nulls the value.
    pub direct_radiation: Option<f32>,
    /// Cloud cover, %. Optional at the source, like `direct_radiation`.
    pub cloud_cover: Option<f32>,
}

/// A provider's response: hourly samples ordered by timestamp ascending,
/// plus the grid cell the provider actually resolved (may differ from the
/// requested coordinates by several km, as with weather).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SolarForecast {
    pub samples: Vec<SolarForecastSample>,
    pub grid_lat: Option<f32>,
    pub grid_lon: Option<f32>,
}

/// How many complete past days of modelled radiation to request so the
/// performance-ratio calibration has history to learn from.
pub const PAST_DAYS: u32 = 14;
/// How many forward days to request — covers the 48 h planning horizon
/// with a day of slack at hourly resolution.
pub const FORECAST_DAYS: u32 = 3;

/// Builds the Open-Meteo URL for the hourly solar forecast request.
///
/// Pure so the parameter table (variables, past/forward day counts,
/// UTC times) is unit-testable without network access.
pub fn solar_forecast_url(base_url: &str, latitude: f64, longitude: f64) -> String {
    format!(
        "{base}/v1/forecast?latitude={lat}&longitude={lon}\
         &hourly=shortwave_radiation,direct_radiation,cloud_cover\
         &past_days={PAST_DAYS}&forecast_days={FORECAST_DAYS}&timezone=UTC",
        base = base_url.trim_end_matches('/'),
        lat = latitude,
        lon = longitude,
    )
}

/// Parse Open-Meteo's hourly forecast response into a [`SolarForecast`].
///
/// Pure (operates only on already-fetched JSON) so the full parsing table —
/// error bodies, missing fields, length mismatches, null rows — is
/// unit-tested without any network, mirroring the weather module's
/// `parse_archive_response` tests.
pub fn parse_solar_forecast_response(json: &serde_json::Value) -> Result<SolarForecast, String> {
    // Guard against Open-Meteo's own error responses (HTTP 200 with an
    // `error: true` body, e.g. for malformed parameters) — same shape
    // the weather module handles.
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
    let shortwave = hourly
        .get("shortwave_radiation")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "missing 'hourly.shortwave_radiation'".to_string())?;
    if times.len() != shortwave.len() {
        return Err(format!(
            "hourly.time ({} entries) and hourly.shortwave_radiation ({} entries) lengths differ",
            times.len(),
            shortwave.len()
        ));
    }

    // Optional variables may be absent entirely; when present their
    // length must match the time axis.
    let optional_series = |name: &str| -> Result<Option<&Vec<serde_json::Value>>, String> {
        match hourly.get(name).and_then(|v| v.as_array()) {
            None => Ok(None),
            Some(values) if values.len() == times.len() => Ok(Some(values)),
            Some(values) => Err(format!(
                "hourly.time ({} entries) and hourly.{name} ({} entries) lengths differ",
                times.len(),
                values.len()
            )),
        }
    };
    let direct = optional_series("direct_radiation")?;
    let cloud = optional_series("cloud_cover")?;

    let grid_lat = json
        .get("latitude")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32);
    let grid_lon = json
        .get("longitude")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32);

    let mut samples = Vec::with_capacity(times.len());
    for (i, t) in times.iter().enumerate() {
        let Some(time_str) = t.as_str() else { continue };
        let Ok(parsed) = chrono::NaiveDateTime::parse_from_str(time_str, "%Y-%m-%dT%H:%M") else {
            continue;
        };
        let Some(g) = shortwave[i].as_f64() else {
            continue;
        };
        samples.push(SolarForecastSample {
            timestamp: parsed.and_utc().timestamp(),
            shortwave_radiation: g as f32,
            direct_radiation: direct.and_then(|v| v[i].as_f64()).map(|v| v as f32),
            cloud_cover: cloud.and_then(|v| v[i].as_f64()).map(|v| v as f32),
        });
    }
    Ok(SolarForecast {
        samples,
        grid_lat,
        grid_lon,
    })
}

/// A source of hourly solar radiation forecasts. Phase 0 ships a single
/// Open-Meteo implementation; the trait exists so an alternative (e.g. a
/// Solcast hobbyist account) can slot in without touching callers.
///
/// `async fn` in the trait is deliberate: providers are used by static
/// dispatch inside the already-`Send` weather-loop task, so the missing
/// auto-trait bounds the lint warns about don't apply here.
#[allow(async_fn_in_trait)] // rustc lint, not clippy
pub trait SolarForecastProvider {
    /// Fetch hourly samples covering the provider's configured past and
    /// forward windows, ordered by timestamp ascending.
    async fn fetch(&self, latitude: f64, longitude: f64) -> Result<SolarForecast, String>;
}

/// The free Open-Meteo forecast endpoint — keyless, CC-BY 4.0, sharing
/// the weather module's HTTP agent (same 10 s timeout / no-pooling
/// policy, same base-URL override for self-hosted instances).
pub struct OpenMeteoSolarProvider {
    base_url: String,
}

impl OpenMeteoSolarProvider {
    /// Source name persisted into `forecast_values.source`.
    pub const SOURCE: &str = "open-meteo";

    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
        }
    }
}

impl SolarForecastProvider for OpenMeteoSolarProvider {
    async fn fetch(&self, latitude: f64, longitude: f64) -> Result<SolarForecast, String> {
        let url = solar_forecast_url(&self.base_url, latitude, longitude);
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

        parse_solar_forecast_response(&result?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_contains_solar_params() {
        let url = solar_forecast_url("https://api.open-meteo.com", 51.5, -0.13);
        assert!(
            url.starts_with("https://api.open-meteo.com/v1/forecast?"),
            "{url}"
        );
        assert!(url.contains("latitude=51.5"), "{url}");
        assert!(url.contains("longitude=-0.13"), "{url}");
        assert!(
            url.contains("hourly=shortwave_radiation,direct_radiation,cloud_cover"),
            "{url}"
        );
        // 14 modelled past days feed the performance-ratio calibration;
        // 3 forward days cover the 48 h planning horizon with slack.
        assert!(url.contains("past_days=14"), "{url}");
        assert!(url.contains("forecast_days=3"), "{url}");
        // UTC keeps the timestamp parse tz-free like the weather module.
        assert!(url.contains("timezone=UTC"), "{url}");
    }

    #[test]
    fn url_trims_trailing_slash_from_base() {
        let url = solar_forecast_url("https://api.open-meteo.com/", 1.0, 2.0);
        assert!(!url.contains("//v1"), "{url}");
    }

    fn json_with(hourly: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "latitude": 51.25,
            "longitude": -0.5,
            "hourly": hourly
        })
    }

    #[test]
    fn parse_extracts_all_variables_and_grid_coords() {
        let json = json_with(serde_json::json!({
            "time": ["2025-06-21T00:00", "2025-06-21T01:00", "2025-06-21T02:00"],
            "shortwave_radiation": [0.0, 12.5, 45.0],
            "direct_radiation": [0.0, 8.0, 30.0],
            "cloud_cover": [100.0, 93.0, 80.0]
        }));
        let fc = parse_solar_forecast_response(&json).unwrap();
        assert_eq!(fc.samples.len(), 3);
        assert_eq!(fc.grid_lat, Some(51.25));
        assert_eq!(fc.grid_lon, Some(-0.5));
        // Ascending, one hour apart.
        assert_eq!(fc.samples[1].timestamp - fc.samples[0].timestamp, 3600);
        assert_eq!(fc.samples[1].shortwave_radiation, 12.5);
        assert_eq!(fc.samples[2].direct_radiation, Some(30.0));
        assert_eq!(fc.samples[0].cloud_cover, Some(100.0));
    }

    #[test]
    fn parse_tolerates_missing_optional_arrays() {
        // A provider response without the optional variables still yields
        // usable shortwave samples with None optionals.
        let json = json_with(serde_json::json!({
            "time": ["2025-06-21T00:00", "2025-06-21T01:00"],
            "shortwave_radiation": [5.0, 10.0]
        }));
        let fc = parse_solar_forecast_response(&json).unwrap();
        assert_eq!(fc.samples.len(), 2);
        assert_eq!(fc.samples[0].direct_radiation, None);
        assert_eq!(fc.samples[0].cloud_cover, None);
    }

    #[test]
    fn parse_skips_null_shortwave_rows() {
        let json = json_with(serde_json::json!({
            "time": ["2025-06-21T00:00", "2025-06-21T01:00", "2025-06-21T02:00"],
            "shortwave_radiation": [5.0, null, 15.0],
            "cloud_cover": [50.0, 40.0, 30.0]
        }));
        let fc = parse_solar_forecast_response(&json).unwrap();
        // The null-radiation hour is dropped entirely; no partial sample.
        assert_eq!(fc.samples.len(), 2);
        assert_eq!(fc.samples[0].shortwave_radiation, 5.0);
        assert_eq!(fc.samples[1].shortwave_radiation, 15.0);
        assert_eq!(fc.samples[1].cloud_cover, Some(30.0));
    }

    #[test]
    fn parse_surfaces_open_meteo_error_body() {
        // Open-Meteo returns HTTP 200 with `error: true` for bad params —
        // same shape the weather module handles.
        let json = serde_json::json!({ "error": true, "reason": "bad params" });
        let err = parse_solar_forecast_response(&json).unwrap_err();
        assert!(err.contains("bad params"), "{err}");
    }

    #[test]
    fn parse_rejects_missing_hourly_and_time() {
        assert!(parse_solar_forecast_response(&serde_json::json!({})).is_err());
        assert!(parse_solar_forecast_response(&json_with(serde_json::json!({
            "shortwave_radiation": [5.0]
        })))
        .is_err());
    }

    #[test]
    fn parse_rejects_length_mismatch() {
        let mismatched = json_with(serde_json::json!({
            "time": ["2025-06-21T00:00", "2025-06-21T01:00"],
            "shortwave_radiation": [5.0],
            "direct_radiation": [1.0, 2.0, 3.0]
        }));
        let err = parse_solar_forecast_response(&mismatched).unwrap_err();
        assert!(err.contains("lengths differ"), "{err}");
    }

    #[test]
    fn parse_skips_malformed_timestamps() {
        // A bad time row is dropped, not fatal — matches weather parsing.
        let json = json_with(serde_json::json!({
            "time": ["2025-06-21T00:00", "not-a-time"],
            "shortwave_radiation": [5.0, 10.0]
        }));
        let fc = parse_solar_forecast_response(&json).unwrap();
        assert_eq!(fc.samples.len(), 1);
        assert_eq!(fc.samples[0].shortwave_radiation, 5.0);
    }
}
