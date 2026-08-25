//! Solar forecast subsystem (issue #283).
//!
//! Phase 0 — the data foundation: fetch hourly solar radiation forecasts,
//! persist the forward window to `history.db::forecast_values` (so
//! predicted-vs-actual accuracy can be tracked once history accumulates),
//! and self-calibrate the site's performance ratio from trailing
//! generation history. No UI or API surface yet.
//!
//! The periodic fetch rides on the weather loop
//! ([`crate::weather::run_weather_loop`]) because it shares the same
//! Open-Meteo endpoint, agent, config and enable gate.

pub mod calibration;
pub mod solar;

use chrono::{DateTime, Local};

use crate::forecast::solar::SolarForecast;
use crate::history::{ForecastValueRow, HistoryDb};

/// How often the radiation forecast is refreshed. Open-Meteo's European
/// models update hourly, so three hours is plenty for a 48 h planning
/// horizon while staying far inside the free tier's fair-use envelope.
pub const SOLAR_FORECAST_FETCH_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(3 * 60 * 60);

/// Split a fetched forecast at `now`: persist the forward window to
/// `forecast_values` (one row per variable per hour — optional fields are
/// stored only when the provider supplied them) and calibrate the
/// performance ratio from the past window against actual generation.
///
/// Returns the calibrated PR, or `None` when there isn't enough usable
/// history yet. Synchronous and DB-only so the whole behaviour —
/// future/past split, variable persistence, calibration hand-off — is
/// testable without network access.
pub fn store_and_calibrate(
    _db: &HistoryDb,
    _rated_kw: f64,
    _source: &str,
    _now: DateTime<Local>,
    _forecast: &SolarForecast,
) -> Option<f64> {
    // RED stub — Phase 0 implementation lands in the following commit.
    None
}

/// Total rated kWp used by the radiation→power model. Mirrors the
/// frontend convention (`solarArrays.ts::solarOverallPercent`): when
/// CT-meter solar arrays are configured, their measured generation is
/// what `today_solar_kwh` reports, so their capacity is the denominator;
/// otherwise the hybrid's DC strings (pv1 + pv2) are. Arrays with zero
/// rated capacity are ignored — they carry no measurement.
pub(crate) fn effective_solar_kwp(_settings: &crate::settings::Settings) -> f64 {
    // RED stub — Phase 0 implementation lands in the following commit.
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forecast::solar::SolarForecastSample;
    use crate::inverter::model::InverterSnapshot;
    use crate::settings::{Settings, SolarArrayConfig};
    use chrono::TimeZone;
    use std::path::PathBuf;

    fn test_db() -> HistoryDb {
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir: PathBuf = std::env::temp_dir().join(format!(
            "givenergy-forecast-test-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create forecast test dir");
        HistoryDb::open(&dir.join("test_history.db")).expect("open forecast test db")
    }

    fn local_dt(y: i32, m: u32, day: u32, h: u32, min: u32) -> DateTime<Local> {
        Local
            .from_local_datetime(
                &chrono::NaiveDate::from_ymd_opt(y, m, day)
                    .unwrap()
                    .and_hms_opt(h, min, 0)
                    .unwrap(),
            )
            .earliest()
            .unwrap()
    }

    fn sample(ts: i64, g: f32, direct: Option<f32>, cloud: Option<f32>) -> SolarForecastSample {
        SolarForecastSample {
            timestamp: ts,
            shortwave_radiation: g,
            direct_radiation: direct,
            cloud_cover: cloud,
        }
    }

    fn series_len(db: &HistoryDb, variable: &str) -> usize {
        db.query_forecast_series(variable, "open-meteo", 0, i64::MAX)
            .unwrap()
            .len()
    }

    #[test]
    fn stores_only_the_forward_window() {
        let db = test_db();
        let now = local_dt(2025, 6, 15, 12, 0);
        let now_ts = now.timestamp();
        let forecast = SolarForecast {
            samples: vec![
                // Past hours feed calibration only — never stored.
                sample(now_ts - 2 * 3600, 100.0, Some(80.0), Some(50.0)),
                // `now` itself counts as forward (>= now_ts).
                sample(now_ts, 200.0, Some(150.0), Some(10.0)),
                // Optional fields missing → only the shortwave row.
                sample(now_ts + 3600, 210.0, None, None),
                sample(now_ts + 2 * 3600, 220.0, Some(160.0), None),
            ],
            grid_lat: None,
            grid_lon: None,
        };

        let pr = store_and_calibrate(&db, 5.0, "open-meteo", now, &forecast);

        // 3 forward hours of shortwave; direct/cloud only where present.
        assert_eq!(series_len(&db, "shortwave_radiation"), 3);
        assert_eq!(series_len(&db, "direct_radiation"), 2);
        assert_eq!(series_len(&db, "cloud_cover"), 1);
        // The past hour must not appear anywhere.
        let sw = db
            .query_forecast_series("shortwave_radiation", "open-meteo", 0, i64::MAX)
            .unwrap();
        assert!(!sw.iter().any(|p| p.timestamp == now_ts - 2 * 3600));
        // No usable past history → no PR yet.
        assert!(pr.is_none());
    }

    #[test]
    fn calibrates_and_stores_in_one_pass() {
        let db = test_db();
        let now = local_dt(2025, 6, 15, 12, 0);
        let now_ts = now.timestamp();

        // Five complete past days (2025-06-10..14), 12 hourly readings
        // each, counters rising to the day's total — mirroring how the
        // poll loop writes `today_solar_kwh`.
        let rated = 5.0_f64;
        // 700 W/m² × 10 h = 7 kWh/m²/day → expect 35 kWh at PR 1.0.
        // Ratios 0.7..1.0 and 0.6 → median 0.8.
        let kwhs = [24.5_f32, 28.0, 31.5, 35.0, 21.0];
        for (i, &kwh) in kwhs.iter().enumerate() {
            let date = chrono::NaiveDate::from_ymd_opt(2025, 6, 10 + i as u32).unwrap();
            for hour in 7..19u32 {
                let ts = Local
                    .from_local_datetime(&date.and_hms_opt(hour, 0, 0).unwrap())
                    .earliest()
                    .unwrap()
                    .timestamp();
                let fraction = (hour - 6) as f32 / 12.0;
                db.insert_reading(&InverterSnapshot {
                    timestamp: ts,
                    today_solar_kwh: kwh * fraction,
                    ..Default::default()
                });
            }
        }

        let mut samples: Vec<SolarForecastSample> = Vec::new();
        for i in 0..5u32 {
            let date = chrono::NaiveDate::from_ymd_opt(2025, 6, 10 + i).unwrap();
            for hour in 7..17u32 {
                let ts = Local
                    .from_local_datetime(&date.and_hms_opt(hour, 0, 0).unwrap())
                    .earliest()
                    .unwrap()
                    .timestamp();
                samples.push(sample(ts, 700.0, None, None));
            }
        }
        // Plus a forward tail so storage is exercised on the same call.
        samples.push(sample(now_ts + 3600, 400.0, Some(300.0), Some(20.0)));

        let forecast = SolarForecast {
            samples,
            grid_lat: Some(51.5),
            grid_lon: Some(-0.13),
        };
        let pr = store_and_calibrate(&db, rated, "open-meteo", now, &forecast);

        let pr = pr.expect("five complete days should calibrate");
        assert!((pr - 0.8).abs() < 1e-9, "pr = {pr}");
        assert_eq!(series_len(&db, "shortwave_radiation"), 1);
    }

    #[test]
    fn kwp_prefers_meter_arrays_when_configured() {
        let mut settings = Settings::default();
        settings.pv1_rated_kw = 3.0;
        settings.solar_arrays = vec![
            SolarArrayConfig {
                meter_address: 1,
                name: "East roof".into(),
                rated_kw: 6.0,
            },
            SolarArrayConfig {
                meter_address: 2,
                name: String::new(),
                rated_kw: 4.2,
            },
        ];
        assert!((effective_solar_kwp(&settings) - 10.2).abs() < 1e-9);
    }

    #[test]
    fn kwp_falls_back_to_dc_strings() {
        let mut settings = Settings::default();
        settings.pv1_rated_kw = 6.0;
        settings.pv2_rated_kw = 4.2;
        assert!((effective_solar_kwp(&settings) - 10.2).abs() < 1e-9);
    }

    #[test]
    fn kwp_ignores_zero_rated_meter_arrays() {
        // A meter array without a rating carries no measurement — the DC
        // string ratings remain the denominator.
        let mut settings = Settings::default();
        settings.pv1_rated_kw = 5.0;
        settings.solar_arrays = vec![SolarArrayConfig {
            meter_address: 1,
            name: String::new(),
            rated_kw: 0.0,
        }];
        assert!((effective_solar_kwp(&settings) - 5.0).abs() < 1e-9);
    }
}
