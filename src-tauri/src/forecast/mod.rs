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
pub mod consumption;
pub mod planner;
pub mod refresh;
pub mod simulate;
pub mod solar;

use chrono::{DateTime, Datelike, Local, Timelike};

use crate::forecast::calibration::calibrate_performance_ratio_detailed;
use crate::forecast::consumption::build_consumption_profile;
use crate::forecast::simulate::{simulate_battery, SimHourInput, SimulationParams};
use crate::forecast::solar::{OpenMeteoSolarProvider, SolarForecast, SolarForecastProvider};
use crate::history::{ForecastValueRow, HistoryDb};
use crate::inverter::model::InverterSnapshot;
use crate::settings::Settings;

/// How often the radiation forecast is refreshed. Open-Meteo's European
/// models update hourly, so three hours is plenty for a 72 h planning
/// horizon while staying far inside the free tier's fair-use envelope.
pub const SOLAR_FORECAST_FETCH_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(3 * 60 * 60);

/// How far back the consumption profile fit reads counter samples for.
const CONSUMPTION_WINDOW_DAYS: i64 = 28;

/// Model-uncertainty band around the solar prediction while measured
/// forecast-error quantiles don't exist yet (Phase 4 replaces this with
/// real error statistics from `forecast_values`).
const SOLAR_BAND_FACTOR: f64 = 0.2;

/// Performance ratio used while calibration hasn't accumulated enough
/// days — the payload reports the `calibrating` status alongside so the
/// UI can label these numbers preliminary.
const FALLBACK_PERFORMANCE_RATIO: f64 = 0.75;

/// `meta` keys persisted by [`store_and_calibrate`].
pub const META_FORECAST_PR: &str = "forecast_pr";
pub const META_FORECAST_PR_DAYS: &str = "forecast_pr_days";
pub const META_FORECAST_CALIBRATED_AT: &str = "forecast_calibrated_at";

// ---------------------------------------------------------------------------
// GET /api/forecast payload
// ---------------------------------------------------------------------------

/// One forward solar-prediction hour (kWh plus the model band).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ForecastSolarHour {
    pub timestamp: i64,
    pub kwh: f64,
    pub band_low: f64,
    pub band_high: f64,
}

/// One hour-of-day consumption statistic.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ForecastConsumptionHour {
    pub hour: u8,
    pub kwh: f64,
    pub p25: f64,
    pub p75: f64,
}

fn consumption_series(
    profile: &crate::forecast::consumption::ConsumptionProfile,
    is_weekend: bool,
) -> Vec<ForecastConsumptionHour> {
    (0..24u8)
        .map(|hour| {
            let band = profile.band_for_day_type(hour, is_weekend);
            ForecastConsumptionHour {
                hour,
                kwh: band.median,
                p25: band.p25,
                p75: band.p75,
            }
        })
        .collect()
}

/// Battery projection over the forward window.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ForecastBattery {
    pub capacity_kwh: f64,
    pub start_soc_pct: f64,
    pub reserve_soc_pct: f64,
    /// SOC at the end of each forward hour.
    pub hours: Vec<(i64, f64)>,
    pub end_soc_pct: f64,
}

/// The complete `GET /api/forecast` data block. `status` carries
/// human-resolvable degradation codes (empty = fully calibrated):
/// `weather_disabled`, `no_coordinates`, `no_forecast_data`,
/// `calibrating`, `insufficient_consumption_history`, `no_snapshot`,
/// `no_battery_capacity`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ForecastPayload {
    pub generated_at: i64,
    pub status: Vec<String>,
    pub performance_ratio: Option<f64>,
    pub performance_ratio_days: Option<u32>,
    /// Forward solar hours from now, ascending.
    pub solar: Vec<ForecastSolarHour>,
    pub solar_today_remaining_kwh: f64,
    pub solar_tomorrow_kwh: f64,
    /// The weekday hour-of-day statistics (median + p25/p75), using the
    /// all-days nearest-neighbour fallback when weekday history is missing.
    pub consumption_weekday: Vec<ForecastConsumptionHour>,
    /// The weekend hour-of-day statistics (median + p25/p75), using the
    /// all-days nearest-neighbour fallback when weekend history is missing.
    pub consumption_weekend: Vec<ForecastConsumptionHour>,
    pub consumption_days_observed: u32,
    pub consumption_sufficient: bool,
    pub consumption_tomorrow_kwh: f64,
    pub battery: Option<ForecastBattery>,
    pub import_tomorrow_kwh: f64,
    pub export_tomorrow_kwh: f64,
}

/// Everything [`build_forecast_payload`] needs, so the assembly is a pure
/// function of local state (testable without network or live poll loop).
pub struct ForecastInputs<'a> {
    pub db: Option<&'a HistoryDb>,
    pub settings: &'a Settings,
    pub snapshot: Option<&'a InverterSnapshot>,
    pub weather_enabled: bool,
    pub weather_coords: Option<(f64, f64)>,
    pub now: DateTime<Local>,
}

/// Assemble the forecast payload from local state: stored forward
/// radiation, calibrated PR, fitted consumption profile, current snapshot
/// and settings. Every degradation path yields partial data plus a
/// status code — never zeros pretending to be a prediction.
pub fn build_forecast_payload(inputs: &ForecastInputs) -> ForecastPayload {
    let now = inputs.now;
    let now_ts = now.timestamp();
    let today = now.date_naive();
    let tomorrow = today
        .checked_add_signed(chrono::Duration::days(1))
        .unwrap_or(today);
    let mut status: Vec<String> = Vec::new();

    // --- gating ----------------------------------------------------------
    if !inputs.weather_enabled {
        status.push("weather_disabled".to_string());
    } else if inputs.weather_coords.is_none() {
        status.push("no_coordinates".to_string());
    }

    // --- performance ratio ----------------------------------------------
    let pr_stored = inputs
        .db
        .and_then(|db| db.get_meta_value(META_FORECAST_PR))
        .and_then(|v| v.parse::<f64>().ok());
    let pr_days = inputs
        .db
        .and_then(|db| db.get_meta_value(META_FORECAST_PR_DAYS))
        .and_then(|v| v.parse::<u32>().ok());
    if pr_stored.is_none() {
        status.push("calibrating".to_string());
    }
    let pr = pr_stored.unwrap_or(FALLBACK_PERFORMANCE_RATIO);

    // --- forward solar ---------------------------------------------------
    let kwp = effective_solar_kwp(inputs.settings);
    let mut solar: Vec<ForecastSolarHour> = Vec::new();
    let mut solar_today_remaining_kwh = 0.0;
    let mut solar_tomorrow_kwh = 0.0;
    if let Some(db) = inputs.db {
        let series = db
            .query_forecast_series(
                "shortwave_radiation",
                crate::forecast::solar::OpenMeteoSolarProvider::SOURCE,
                now_ts,
                now_ts + 72 * 3600,
            )
            .unwrap_or_default();
        for p in series {
            let Some(utc) = chrono::DateTime::from_timestamp(p.timestamp, 0) else {
                continue;
            };
            let local = utc.with_timezone(&chrono::Local);
            let kwh = p.value / 1000.0 * kwp * pr;
            match local.date_naive() {
                d if d == today => solar_today_remaining_kwh += kwh,
                d if d == tomorrow => solar_tomorrow_kwh += kwh,
                _ => {}
            }
            solar.push(ForecastSolarHour {
                timestamp: p.timestamp,
                kwh,
                band_low: kwh * (1.0 - SOLAR_BAND_FACTOR),
                band_high: kwh * (1.0 + SOLAR_BAND_FACTOR),
            });
        }
    }
    if solar.is_empty() {
        status.push("no_forecast_data".to_string());
    }

    // --- consumption profile ----------------------------------------------
    let rows = inputs
        .db
        .map(|db| {
            db.consumption_counter_rows_since(now_ts - CONSUMPTION_WINDOW_DAYS * 86_400)
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let profile = build_consumption_profile(&rows);
    if !profile.sufficient() {
        status.push("insufficient_consumption_history".to_string());
    }
    let consumption_weekday = consumption_series(&profile, false);
    let consumption_weekend = consumption_series(&profile, true);
    let tomorrow_is_weekend = tomorrow.weekday().number_from_monday() >= 6;
    let consumption_tomorrow_kwh: f64 = if tomorrow_is_weekend {
        consumption_weekend.iter().map(|c| c.kwh).sum()
    } else {
        consumption_weekday.iter().map(|c| c.kwh).sum()
    };

    // --- battery projection -----------------------------------------------
    let mut battery = None;
    let mut import_tomorrow_kwh = 0.0;
    let mut export_tomorrow_kwh = 0.0;
    match inputs.snapshot {
        None => status.push("no_snapshot".to_string()),
        Some(snap) => {
            let (charge_kw, discharge_kw) = battery_rate_limits_kw(snap);
            if snap.battery_capacity_kwh <= 0.0 || charge_kw <= 0.0 || discharge_kw <= 0.0 {
                status.push("no_battery_capacity".to_string());
            } else {
                let sim_hours: Vec<SimHourInput> = solar
                    .iter()
                    .map(|s| {
                        let (hour, is_weekend) = chrono::DateTime::from_timestamp(s.timestamp, 0)
                            .map(|dt| {
                                let local = dt.with_timezone(&chrono::Local);
                                (
                                    local.hour() as u8,
                                    local.weekday().number_from_monday() >= 6,
                                )
                            })
                            .unwrap_or((0, false));
                        SimHourInput {
                            timestamp: s.timestamp,
                            solar_kwh: s.kwh,
                            consumption_kwh: profile.median_kwh_for_day_type(hour, is_weekend),
                        }
                    })
                    .collect();
                let params = SimulationParams {
                    capacity_kwh: snap.battery_capacity_kwh as f64,
                    start_soc_pct: snap.soc as f64,
                    reserve_soc_pct: snap.battery_reserve as f64,
                    max_charge_kw: charge_kw,
                    max_discharge_kw: discharge_kw,
                    charge_efficiency: inputs.settings.forecast_charge_efficiency,
                    discharge_efficiency: inputs.settings.forecast_discharge_efficiency,
                };
                let sim = simulate_battery(&sim_hours, &params);
                for h in &sim.hours {
                    let is_tomorrow = chrono::DateTime::from_timestamp(h.timestamp, 0)
                        .map(|dt| dt.with_timezone(&chrono::Local).date_naive() == tomorrow)
                        .unwrap_or(false);
                    if is_tomorrow {
                        import_tomorrow_kwh += h.import_kwh;
                        export_tomorrow_kwh += h.export_kwh;
                    }
                }
                let end_soc_pct = sim
                    .hours
                    .last()
                    .map(|h| h.soc_pct)
                    .unwrap_or(snap.soc as f64);
                battery = Some(ForecastBattery {
                    capacity_kwh: params.capacity_kwh,
                    start_soc_pct: params.start_soc_pct,
                    reserve_soc_pct: params.reserve_soc_pct,
                    hours: sim.hours.iter().map(|h| (h.timestamp, h.soc_pct)).collect(),
                    end_soc_pct,
                });
            }
        }
    }

    ForecastPayload {
        generated_at: now_ts,
        status,
        performance_ratio: pr_stored,
        performance_ratio_days: pr_days,
        solar,
        solar_today_remaining_kwh,
        solar_tomorrow_kwh,
        consumption_weekday,
        consumption_weekend,
        consumption_days_observed: profile.days_observed,
        consumption_sufficient: profile.sufficient(),
        consumption_tomorrow_kwh,
        battery,
        import_tomorrow_kwh,
        export_tomorrow_kwh,
    }
}

/// Battery AC rate limits in kW derived from snapshot registers, using
/// the same model classification the Control page applies: direct-limit
/// families store 1–100%, DC hybrids store 0–50 which the UI doubles.
/// Returns (0, 0) when the max battery power is unknown.
pub(crate) fn battery_rate_limits_kw(snapshot: &InverterSnapshot) -> (f64, f64) {
    if snapshot.max_battery_power_w == 0 {
        return (0.0, 0.0);
    }
    let max_kw = snapshot.max_battery_power_w as f64 / 1000.0;
    // DC hybrids store 0–50 which the UI doubles for display; direct-limit
    // families store 1–100 natively.
    let scale = if snapshot.device_type.uses_direct_charge_limit() {
        1.0
    } else {
        2.0
    };
    // A 0 rate register means "unset/unknown", not "disabled" — the AC
    // config / limit blocks are optional and read zero right after connect
    // (and on simulators). Fall back to the hardware maximum so the
    // projection still runs; a genuine user-configured 0 is rare and the
    // forecast is an estimate capped at the physical limit either way.
    let pct_or_max = |raw: u8| {
        if raw == 0 {
            100.0
        } else {
            (raw as f64 * scale).clamp(0.0, 100.0)
        }
    };
    let charge_pct = pct_or_max(snapshot.charge_rate);
    let discharge_pct = pct_or_max(snapshot.discharge_rate);
    (charge_pct / 100.0 * max_kw, discharge_pct / 100.0 * max_kw)
}

/// How far back the calibration reads actual generation for. One day
/// beyond the 14 modelled past days so a fetch early in the local day
/// still sees a full trailing window.
const CALIBRATION_LOOKBACK_DAYS: i64 = 15;

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
    db: &HistoryDb,
    rated_kw: f64,
    source: &str,
    now: DateTime<Local>,
    forecast: &SolarForecast,
) -> Option<f64> {
    let now_ts = now.timestamp();
    let mut rows: Vec<ForecastValueRow> = Vec::new();
    let mut past: Vec<crate::forecast::solar::SolarForecastSample> = Vec::new();

    for s in &forecast.samples {
        if s.timestamp >= now_ts {
            rows.push(ForecastValueRow {
                timestamp: s.timestamp,
                variable: "shortwave_radiation".to_string(),
                value: s.shortwave_radiation as f64,
                source: source.to_string(),
                fetched_at: now_ts,
            });
            if let Some(v) = s.direct_radiation {
                rows.push(ForecastValueRow {
                    timestamp: s.timestamp,
                    variable: "direct_radiation".to_string(),
                    value: v as f64,
                    source: source.to_string(),
                    fetched_at: now_ts,
                });
            }
            if let Some(v) = s.cloud_cover {
                rows.push(ForecastValueRow {
                    timestamp: s.timestamp,
                    variable: "cloud_cover".to_string(),
                    value: v as f64,
                    source: source.to_string(),
                    fetched_at: now_ts,
                });
            }
        } else {
            past.push(s.clone());
        }
    }

    if !rows.is_empty() {
        if let Err(e) = db.insert_forecast_values(&rows) {
            tracing::warn!("Failed to store solar forecast: {e}");
        }
    }

    let since = now_ts - CALIBRATION_LOOKBACK_DAYS * 86_400;
    let totals = db.daily_solar_totals_since(since).unwrap_or_default();
    let fit = calibrate_performance_ratio_detailed(&past, &totals, rated_kw, now.date_naive());
    // Persist the calibration so `GET /api/forecast` can serve the PR
    // without recalibrating. Written only on success — a failed attempt
    // keeps the previous fit (and its day count / timestamp) until the
    // next successful one supersedes it.
    if let Some(pr) = fit.pr {
        if let Err(e) = db.set_meta_value(META_FORECAST_PR, &format!("{pr:.4}")) {
            tracing::warn!("Failed to persist forecast PR: {e}");
        }
        let _ = db.set_meta_value(META_FORECAST_PR_DAYS, &fit.usable_days.to_string());
        let _ = db.set_meta_value(META_FORECAST_CALIBRATED_AT, &now_ts.to_string());
    }
    fit.pr
}

/// Total rated kWp used by the radiation→power model. Mirrors the
/// frontend convention (`solarArrays.ts::solarOverallPercent`): when
/// CT-meter solar arrays are configured, their measured generation is
/// what `today_solar_kwh` reports, so their capacity is the denominator;
/// otherwise the hybrid's DC strings (pv1 + pv2) are. Arrays with zero
/// rated capacity are ignored — they carry no measurement.
pub(crate) fn effective_solar_kwp(settings: &crate::settings::Settings) -> f64 {
    let meter_total: f64 = settings
        .solar_arrays
        .iter()
        .map(|a| a.rated_kw)
        .filter(|kw| *kw > 0.0)
        .sum();
    if meter_total > 0.0 {
        meter_total
    } else {
        settings.pv1_rated_kw + settings.pv2_rated_kw
    }
}

/// One solar forecast fetch-and-store cycle, invoked by the weather loop
/// every [`SOLAR_FORECAST_FETCH_INTERVAL`]. Gated on the same weather
/// enable flag and coordinates; failures log a warning and retry on the
/// next tick.
pub async fn run_solar_forecast_fetch(state: std::sync::Arc<crate::inverter::poll::AppState>) {
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

    let rated_kw = effective_solar_kwp(&crate::settings::Settings::load());
    let provider = OpenMeteoSolarProvider::new(&config.open_meteo_base_url);
    match provider.fetch(lat, lon).await {
        Ok(forecast) => {
            let now = Local::now();
            let sample_count = forecast.samples.len();
            match store_and_calibrate(
                &db,
                rated_kw,
                OpenMeteoSolarProvider::SOURCE,
                now,
                &forecast,
            ) {
                Some(pr) => tracing::info!(
                    pr,
                    samples = sample_count,
                    "solar forecast stored and performance ratio calibrated"
                ),
                None => tracing::info!(
                    samples = sample_count,
                    "solar forecast stored; not enough history to calibrate performance ratio yet"
                ),
            }
        }
        Err(e) => {
            tracing::warn!("Solar forecast fetch failed: {e}");
        }
    }
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
        let settings = Settings {
            pv1_rated_kw: 3.0,
            solar_arrays: vec![
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
            ],
            ..Settings::default()
        };
        assert!((effective_solar_kwp(&settings) - 10.2).abs() < 1e-9);
    }

    #[test]
    fn kwp_falls_back_to_dc_strings() {
        let settings = Settings {
            pv1_rated_kw: 6.0,
            pv2_rated_kw: 4.2,
            ..Settings::default()
        };
        assert!((effective_solar_kwp(&settings) - 10.2).abs() < 1e-9);
    }

    #[test]
    fn kwp_ignores_zero_rated_meter_arrays() {
        // A meter array without a rating carries no measurement — the DC
        // string ratings remain the denominator.
        let settings = Settings {
            pv1_rated_kw: 5.0,
            solar_arrays: vec![SolarArrayConfig {
                meter_address: 1,
                name: String::new(),
                rated_kw: 0.0,
            }],
            ..Settings::default()
        };
        assert!((effective_solar_kwp(&settings) - 5.0).abs() < 1e-9);
    }

    // --- Phase 1: rate limits, meta persistence, payload assembly ------

    fn rate_snapshot() -> InverterSnapshot {
        InverterSnapshot {
            max_battery_power_w: 5000,
            charge_rate: 50,
            discharge_rate: 50,
            ..Default::default()
        }
    }

    #[test]
    fn rate_limits_fall_back_to_hardware_max_when_unset() {
        // A 0 rate register means "unset/unknown", not "disabled": the AC
        // config block is optional and often reads zero (right after
        // connect, or on simulators). Found via the real simulator —
        // the projection must still run, capped at the hardware max.
        use crate::inverter::model::DeviceType;
        let mut snap = rate_snapshot();
        snap.charge_rate = 0;
        snap.discharge_rate = 0;
        snap.device_type = DeviceType::ACCoupled;
        assert_eq!(battery_rate_limits_kw(&snap), (5.0, 5.0));
        snap.device_type = DeviceType::Gen3Hybrid;
        assert_eq!(battery_rate_limits_kw(&snap), (5.0, 5.0));
    }

    #[test]
    fn payload_projects_battery_despite_zero_rate_registers() {
        // The exact simulator scenario: AC-coupled snapshot with capacity
        // and max power known but rate registers reading zero. The payload
        // must include the battery projection (not no_battery_capacity).
        let db = test_db();
        let now = local_dt(2025, 6, 15, 12, 0);
        seed_full_forecast_state(&db, now);
        let mut snap = battery_snapshot();
        snap.charge_rate = 0;
        snap.discharge_rate = 0;
        let settings = five_kwp_settings();
        let payload =
            build_forecast_payload(&full_forecast_inputs(&db, Some(&snap), now, &settings));
        assert!(!payload.status.contains(&"no_battery_capacity".to_string()));
        let battery = payload.battery.expect("projection must run");
        assert!((battery.end_soc_pct - 100.0).abs() < 1e-9);
    }

    #[test]
    fn rate_limits_double_dc_hybrid_and_use_direct_for_ac() {
        use crate::inverter::model::DeviceType;
        let mut snap = rate_snapshot();
        // DC hybrid raw register 0-50 → display 100% → full 5 kW.
        assert_eq!(battery_rate_limits_kw(&snap), (5.0, 5.0));
        // Direct-limit family: raw 50 is already a percentage → 2.5 kW.
        snap.device_type = DeviceType::ACCoupled;
        assert_eq!(battery_rate_limits_kw(&snap), (2.5, 2.5));
        // Unknown max power → no simulation basis.
        snap.max_battery_power_w = 0;
        assert_eq!(battery_rate_limits_kw(&snap), (0.0, 0.0));
    }

    #[test]
    fn store_and_calibrate_persists_pr_to_meta() {
        let db = test_db();
        let now = local_dt(2025, 6, 15, 12, 0);
        let _now_ts = now.timestamp();
        let rated = 5.0_f64;
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
        let forecast = SolarForecast {
            samples,
            grid_lat: None,
            grid_lon: None,
        };
        let pr = store_and_calibrate(&db, rated, "open-meteo", now, &forecast);
        assert!(pr.is_some(), "five complete days should calibrate");
        // The PR and usable-day count survive to meta so GET /api/forecast
        // can serve them without recalibrating from scratch.
        assert_eq!(
            db.get_meta_value(META_FORECAST_PR)
                .and_then(|v| v.parse::<f64>().ok()),
            Some(0.8)
        );
        assert_eq!(
            db.get_meta_value(META_FORECAST_PR_DAYS)
                .and_then(|v| v.parse::<u32>().ok()),
            Some(5)
        );
        assert!(db.get_meta_value(META_FORECAST_CALIBRATED_AT).is_some());
    }

    /// Seed the full data set a healthy deployment accumulates: 72 h of
    /// forward radiation (constant 500 W/m²), a calibrated PR of 0.8 in
    /// meta, seven days of hourly home-consumption counters, and a
    /// connected snapshot with battery detail.
    fn seed_full_forecast_state(db: &HistoryDb, now: DateTime<Local>) {
        let now_ts = now.timestamp();
        // Align the forward window to the local hour so "today remaining"
        // counting is exact under test.
        let hour_start = now_ts - (now_ts.rem_euclid(3600));
        for h in 0..72i64 {
            let ts = hour_start + h * 3600;
            db.insert_forecast_values(&[ForecastValueRow {
                timestamp: ts,
                variable: "shortwave_radiation".to_string(),
                value: 500.0,
                source: "open-meteo".to_string(),
                fetched_at: now_ts,
            }])
            .unwrap();
        }
        db.set_meta_value(META_FORECAST_PR, "0.8").unwrap();
        db.set_meta_value(META_FORECAST_PR_DAYS, "12").unwrap();
        db.set_meta_value(META_FORECAST_CALIBRATED_AT, &now_ts.to_string())
            .unwrap();

        // Seven days of hourly home-energy counters, +0.5 kWh every hour,
        // all 24 hours covered: today-7 through yesterday inclusive.
        let mut date = now.date_naive() - chrono::Duration::days(7);
        while date < now.date_naive() {
            let mut counter = 0.0_f64;
            for hour in 0..24u32 {
                let ts = Local
                    .from_local_datetime(&date.and_hms_opt(hour, 0, 0).unwrap())
                    .earliest()
                    .unwrap()
                    .timestamp();
                db.insert_reading(&InverterSnapshot {
                    timestamp: ts,
                    home_energy_today_kwh: counter as f32,
                    ..Default::default()
                });
                counter += 0.5;
            }
            date = date.succ_opt().unwrap();
        }
    }

    fn full_forecast_inputs<'a>(
        db: &'a HistoryDb,
        snapshot: Option<&'a InverterSnapshot>,
        now: DateTime<Local>,
        settings: &'a Settings,
    ) -> ForecastInputs<'a> {
        ForecastInputs {
            db: Some(db),
            settings,
            snapshot,
            weather_enabled: true,
            weather_coords: Some((51.5, -0.13)),
            now,
        }
    }

    fn five_kwp_settings() -> Settings {
        Settings {
            pv1_rated_kw: 5.0,
            ..Settings::default()
        }
    }

    fn battery_snapshot() -> InverterSnapshot {
        InverterSnapshot {
            soc: 50,
            battery_capacity_kwh: 10.0,
            max_battery_power_w: 5000,
            charge_rate: 50,
            discharge_rate: 50,
            battery_reserve: 10,
            ..Default::default()
        }
    }

    #[test]
    fn payload_builds_full_forecast() {
        let db = test_db();
        let now = local_dt(2025, 6, 15, 12, 0);
        seed_full_forecast_state(&db, now);
        let snap = battery_snapshot();
        let settings = five_kwp_settings();
        let payload =
            build_forecast_payload(&full_forecast_inputs(&db, Some(&snap), now, &settings));

        assert!(payload.status.is_empty(), "status = {:?}", payload.status);
        assert_eq!(payload.performance_ratio, Some(0.8));
        assert_eq!(payload.performance_ratio_days, Some(12));
        // 500 W/m² × 5 kWp × 0.8 = 2 kWh/h → tomorrow = 24 × 2 = 48.
        assert!(
            (payload.solar_tomorrow_kwh - 48.0).abs() < 0.01,
            "tomorrow = {}",
            payload.solar_tomorrow_kwh
        );
        assert!(payload.solar_today_remaining_kwh > 0.0);
        assert_eq!(payload.consumption_weekday.len(), 24);
        assert_eq!(payload.consumption_weekend.len(), 24);
        assert!(payload.consumption_sufficient);
        assert_eq!(payload.consumption_days_observed, 7);
        assert!((payload.consumption_tomorrow_kwh - 12.0).abs() < 0.01);

        // Battery: surplus 1.5 kWh/h charges the 10 kWh pack from 50% to
        // full within the first hours; every later hour exports 1.5.
        let battery = payload.battery.as_ref().expect("battery projection");
        assert!((battery.start_soc_pct - 50.0).abs() < 1e-9);
        assert!((battery.reserve_soc_pct - 10.0).abs() < 1e-9);
        assert!((battery.end_soc_pct - 100.0).abs() < 1e-9);
        assert!(payload.import_tomorrow_kwh.abs() < 1e-9);
        assert!((payload.export_tomorrow_kwh - 36.0).abs() < 0.1);
    }

    #[test]
    fn payload_reports_every_degradation_when_empty() {
        let db = test_db();
        let now = local_dt(2025, 6, 15, 12, 0);
        let settings = Settings::default();
        let inputs = ForecastInputs {
            db: Some(&db),
            settings: &settings,
            snapshot: None,
            weather_enabled: false,
            weather_coords: None,
            now,
        };
        let payload = build_forecast_payload(&inputs);
        for code in [
            "weather_disabled",
            "no_forecast_data",
            "calibrating",
            "insufficient_consumption_history",
            "no_snapshot",
        ] {
            assert!(payload.status.contains(&code.to_string()), "{code} missing");
        }
        assert!(payload.battery.is_none());
        assert!(payload.solar.is_empty());
    }

    #[test]
    fn payload_flags_calibrating_and_uses_fallback_pr() {
        let db = test_db();
        let now = local_dt(2025, 6, 15, 12, 0);
        // Radiation stored but no PR in meta (fresh install).
        let now_ts = now.timestamp();
        for h in 0..72i64 {
            db.insert_forecast_values(&[ForecastValueRow {
                timestamp: now_ts + h * 3600,
                variable: "shortwave_radiation".to_string(),
                value: 500.0,
                source: "open-meteo".to_string(),
                fetched_at: now_ts,
            }])
            .unwrap();
        }
        let snap = battery_snapshot();
        let settings = five_kwp_settings();
        let payload =
            build_forecast_payload(&full_forecast_inputs(&db, Some(&snap), now, &settings));
        assert!(payload.status.contains(&"calibrating".to_string()));
        // Preliminary numbers still served, using the fallback PR.
        assert!((payload.solar_tomorrow_kwh - 45.0).abs() < 0.01);
    }

    #[test]
    fn payload_without_battery_capacity_omits_projection() {
        let db = test_db();
        let now = local_dt(2025, 6, 15, 12, 0);
        seed_full_forecast_state(&db, now);
        let mut snap = battery_snapshot();
        snap.battery_capacity_kwh = 0.0;
        let settings = five_kwp_settings();
        let payload =
            build_forecast_payload(&full_forecast_inputs(&db, Some(&snap), now, &settings));
        assert!(payload.status.contains(&"no_battery_capacity".to_string()));
        assert!(payload.battery.is_none());
        // Solar and consumption still served.
        assert!((payload.solar_tomorrow_kwh - 48.0).abs() < 0.01);
    }
}
