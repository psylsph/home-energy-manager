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

/// Builds the Open-Meteo URL for the hourly solar forecast request.
///
/// Pure so the parameter table (variables, past/forward day counts,
/// UTC times) is unit-testable without network access.
pub fn solar_forecast_url(_base_url: &str, _latitude: f64, _longitude: f64) -> String {
    // RED stub — Phase 0 implementation lands in the following commit.
    String::new()
}

/// Parse Open-Meteo's hourly forecast response into a [`SolarForecast`].
///
/// Pure (operates only on already-fetched JSON) so the full parsing table —
/// error bodies, missing fields, length mismatches, null rows — is
/// unit-tested without any network, mirroring the weather module's
/// `parse_archive_response` tests.
pub fn parse_solar_forecast_response(_json: &serde_json::Value) -> Result<SolarForecast, String> {
    // RED stub — Phase 0 implementation lands in the following commit.
    Err("not implemented".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_contains_solar_params() {
        let url = solar_forecast_url("https://api.open-meteo.com", 51.5, -0.13);
        assert!(url.starts_with("https://api.open-meteo.com/v1/forecast?"), "{url}");
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
