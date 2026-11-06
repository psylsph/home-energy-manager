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
pub mod current_schedule;
pub mod dispatch_load;
pub mod planner;
pub mod refresh;
pub mod simulate;
pub mod solar;
pub(crate) mod time_windows;

use chrono::{DateTime, Datelike, Local, Timelike};

use crate::forecast::calibration::calibrate_performance_ratio_detailed;
use crate::forecast::consumption::ConsumptionProfile;
use crate::forecast::consumption::{build_consumption_profile_excluding, ConsumptionExclusions};
use crate::forecast::current_schedule::{simulate_current_schedule, CurrentSchedule};
use crate::forecast::dispatch_load::{
    apply_dispatch_hours, believable_energy, believable_kwh, dispatch_overlay, DispatchHour,
    DispatchWindow,
};
use crate::forecast::simulate::{simulate_battery, SimHourInput, SimulationParams};
use crate::forecast::solar::{OpenMeteoSolarProvider, SolarForecast, SolarForecastProvider};
use crate::history::dispatch_log::LoggedDispatch;
use crate::history::{ForecastValueRow, HistoryDb};
use crate::inverter::model::InverterSnapshot;
use crate::octopus_dispatches::PlanningDispatches;
use crate::settings::Settings;

/// How often the radiation forecast is refreshed. Open-Meteo's European
/// models update hourly, so three hours is plenty for a 72 h planning
/// horizon while staying far inside the free tier's fair-use envelope.
pub const SOLAR_FORECAST_FETCH_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(3 * 60 * 60);

/// A forecast is allowed to survive one missed three-hour refresh, but an
/// older projection must not be used to recommend a battery-control action.
pub const SOLAR_FORECAST_MAX_AGE_SECS: i64 = 6 * 60 * 60;

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
    /// SOC projection with the inverter's CURRENTLY enabled schedule
    /// applied (issue #297) — charge slots driving towards their target
    /// at the configured rate, discharge slots draining to their floor.
    /// `None` when no slot would run (the line would merely duplicate
    /// the Eco projection above).
    pub with_current_schedule: Option<Vec<(i64, f64)>>,
}

/// The complete `GET /api/forecast` data block. `status` carries
/// human-resolvable degradation codes (empty = fully calibrated):
/// `weather_disabled`, `no_coordinates`, `no_forecast_data`,
/// `calibrating`, `insufficient_consumption_history`, `no_snapshot`,
/// `no_battery_capacity`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ForecastPayload {
    pub generated_at: i64,
    /// Fetch time of the newest stored solar forecast sample.
    pub forecast_fetched_at: Option<i64>,
    /// Signed age of the newest forecast fetch relative to `generated_at`;
    /// negative values indicate a clock-skewed future timestamp.
    pub forecast_age_seconds: Option<i64>,
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
    /// EV charging left out of the profile's window (issue #400), kWh:
    /// from the charger's recorded power, and above the load threshold.
    pub consumption_excluded_ev_kwh: f64,
    pub consumption_excluded_above_kwh: f64,
    /// Car charging the Octopus dispatch log took out (issue #406), kWh.
    pub consumption_excluded_dispatch_kwh: f64,
    pub battery: Option<ForecastBattery>,
    pub import_tomorrow_kwh: f64,
    pub export_tomorrow_kwh: f64,
    /// Planned car charging (issue #406): the forecast hours with a car or
    /// a dispatch hold, ascending. The plan re-simulates from these.
    pub ev_dispatch_hours: Vec<DispatchHour>,
    /// The car's planned energy tomorrow, kWh.
    pub ev_dispatch_tomorrow_kwh: f64,
    /// When the dispatch list behind `ev_dispatch_hours` was read.
    pub dispatches_checked_at: Option<i64>,
    /// The profile may still hold part of the car (only the threshold trims
    /// it), so each hour adds just the car energy above its own load.
    pub ev_dispatch_net_of_profile: bool,
    /// The planned car charges in the forecast horizon, by start.
    pub ev_dispatch_windows: Vec<DispatchWindow>,
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
    /// The account's planned car charging, when the dispatch hold is on
    /// (no dispatches otherwise).
    pub dispatches: &'a PlanningDispatches,
}

fn current_local_hour_start_ts(now: DateTime<Local>) -> i64 {
    (now - chrono::Duration::minutes(i64::from(now.minute()))
        - chrono::Duration::seconds(i64::from(now.second()))
        - chrono::Duration::nanoseconds(i64::from(now.nanosecond())))
    .timestamp()
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
    // Forecast samples are hourly interval starts. Include the current
    // interval in the remaining-energy total so it can be prorated, while
    // keeping `solar` itself strictly forward-looking.
    let forecast_start_ts = current_local_hour_start_ts(now);
    let forecast_fetched_at = inputs.db.and_then(|db| {
        db.latest_forecast_fetched_at(
            "shortwave_radiation",
            crate::forecast::solar::OpenMeteoSolarProvider::SOURCE,
            forecast_start_ts,
            now_ts + 72 * 3600,
        )
        .ok()
        .flatten()
    });
    let forecast_age_seconds = forecast_fetched_at.map(|fetched_at| now_ts - fetched_at);
    if let Some(db) = inputs.db {
        let series = db
            .query_forecast_series(
                "shortwave_radiation",
                crate::forecast::solar::OpenMeteoSolarProvider::SOURCE,
                forecast_start_ts,
                now_ts + 72 * 3600,
            )
            .unwrap_or_default();
        for p in series {
            let Some(utc) = chrono::DateTime::from_timestamp(p.timestamp, 0) else {
                continue;
            };
            let local = utc.with_timezone(&chrono::Local);
            let kwh = p.value / 1000.0 * kwp * pr;
            let remaining_fraction = if p.timestamp < now_ts {
                p.timestamp
                    .saturating_add(3600)
                    .saturating_sub(now_ts)
                    .clamp(0, 3600) as f64
                    / 3600.0
            } else {
                1.0
            };
            match local.date_naive() {
                d if d == today => solar_today_remaining_kwh += kwh * remaining_fraction,
                d if d == tomorrow => solar_tomorrow_kwh += kwh,
                _ => {}
            }
            if p.timestamp >= now_ts {
                solar.push(ForecastSolarHour {
                    timestamp: p.timestamp,
                    kwh,
                    band_low: kwh * (1.0 - SOLAR_BAND_FACTOR),
                    band_high: kwh * (1.0 + SOLAR_BAND_FACTOR),
                });
            }
        }
    }
    if solar.is_empty() {
        status.push("no_forecast_data".to_string());
    }
    match forecast_age_seconds {
        Some(age) if age < 0 => status.push("forecast_data_future".to_string()),
        Some(age) if age > SOLAR_FORECAST_MAX_AGE_SECS => status.push("forecast_stale".to_string()),
        _ => {}
    }

    // --- consumption profile ----------------------------------------------
    let since_ts = now_ts - CONSUMPTION_WINDOW_DAYS * 86_400;
    let rows = inputs
        .db
        .map(|db| {
            db.consumption_counter_rows_since(since_ts)
                .unwrap_or_default()
        })
        .unwrap_or_default();
    // EV charging is left out (issue #400): the charger's recorded power
    // where HEM has it, else the Octopus dispatches that ran (issue #406),
    // then load above the user's threshold.
    let ev_minutes = inputs
        .db
        .map(|db| db.evc_power_minutes_since(since_ts).unwrap_or_default())
        .unwrap_or_default();
    let logged_dispatches = inputs
        .db
        .map(|db| {
            db.logged_dispatches_since(since_ts, crate::octopus_dispatches::LOG_SEEN_GAP_SECS)
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let profile = build_consumption_profile_excluding(
        &rows,
        &ConsumptionExclusions {
            ev_minutes: &ev_minutes,
            dispatches: &logged_dispatches,
            load_above_kw: inputs.settings.forecast_exclude_load_above_kw,
        },
    );
    let dispatch_log_since = inputs.db.and_then(|db| db.dispatch_log_since());
    if !profile.sufficient() {
        status.push("insufficient_consumption_history".to_string());
    }
    // --- planned car charging (issue #406) --------------------------------
    // The car's load goes in only as far as the profile is free of it, so
    // it is never counted twice; the hold goes in regardless.
    let car = car_in_profile(
        inputs.settings,
        &profile,
        dispatch_log_since,
        &logged_dispatches,
    );
    let planning = inputs.dispatches;
    if planning.stale {
        status.push("dispatches_stale".to_string());
    }
    let hour_starts: Vec<i64> = solar.iter().map(|hour| hour.timestamp).collect();
    let mut overlay = dispatch_overlay(&hour_starts, &planning.dispatches, |_| {
        car != CarInProfile::Unknown
    });
    if !planning.hold_applies {
        // The poll loop won't hold the battery (a calibration run), so a
        // smart charge runs from it like a boost.
        for hour in &mut overlay.hours {
            hour.held_fraction = 0.0;
        }
    }
    let ev_dispatch_net_of_profile = car == CarInProfile::Trimmed;
    if overlay.energy_unknown {
        status.push("dispatch_energy_unknown".to_string());
    }
    if overlay.load_not_added {
        status.push("dispatch_load_not_added".to_string());
    }
    let ev_dispatch_hours: Vec<DispatchHour> = overlay
        .hours
        .into_iter()
        .filter(|hour| hour.ev_kwh > 0.0 || hour.held_fraction > 0.0)
        .collect();
    let ev_dispatch_tomorrow_kwh: f64 = ev_dispatch_hours
        .iter()
        .filter(|hour| {
            chrono::DateTime::from_timestamp(hour.timestamp, 0)
                .is_some_and(|dt| dt.with_timezone(&chrono::Local).date_naive() == tomorrow)
        })
        .map(|hour| hour.ev_kwh)
        .sum();
    let dispatches_checked_at = planning.checked_at.map(|at| at.timestamp());
    // The same 72 h the forecast covers, solar data or not.
    let horizon_end = now_ts + 72 * 3600;
    let mut ev_dispatch_windows: Vec<DispatchWindow> = planning
        .dispatches
        .iter()
        .filter(|d| {
            let (start, end) = (d.start.timestamp(), d.end.timestamp());
            end > now_ts && end > start && start < horizon_end
        })
        .map(|d| DispatchWindow {
            start: d.start.timestamp(),
            end: d.end.timestamp(),
            held: d.is_smart() && planning.hold_applies,
            kwh: believable_energy(d),
        })
        .collect();
    ev_dispatch_windows.sort_by_key(|w| (w.start, w.end));

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
            if !snap.battery_capacity_kwh.is_finite()
                || !inputs.settings.forecast_charge_efficiency.is_finite()
                || !inputs.settings.forecast_discharge_efficiency.is_finite()
            {
                status.push("invalid_battery_configuration".to_string());
            } else if snap.battery_capacity_kwh <= 0.0 || charge_kw <= 0.0 || discharge_kw <= 0.0 {
                status.push("no_battery_capacity".to_string());
            } else {
                let mut sim_hours: Vec<SimHourInput> = solar
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
                            ..Default::default()
                        }
                    })
                    .collect();
                apply_dispatch_hours(
                    &mut sim_hours,
                    &ev_dispatch_hours,
                    ev_dispatch_net_of_profile,
                );
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
                let schedule = CurrentSchedule::from_snapshot(snap, charge_kw, discharge_kw);
                let with_current_schedule = if schedule.is_empty() {
                    None
                } else {
                    Some(simulate_current_schedule(&sim_hours, &params, &schedule))
                };
                battery = Some(ForecastBattery {
                    capacity_kwh: params.capacity_kwh,
                    start_soc_pct: params.start_soc_pct,
                    reserve_soc_pct: params.reserve_soc_pct,
                    hours: sim.hours.iter().map(|h| (h.timestamp, h.soc_pct)).collect(),
                    end_soc_pct,
                    with_current_schedule,
                });
            }
        }
    }

    ForecastPayload {
        generated_at: now_ts,
        forecast_fetched_at,
        forecast_age_seconds,
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
        consumption_excluded_ev_kwh: profile.excluded_ev_kwh,
        consumption_excluded_above_kwh: profile.excluded_above_kwh,
        consumption_excluded_dispatch_kwh: profile.excluded_dispatch_kwh,
        battery,
        import_tomorrow_kwh,
        export_tomorrow_kwh,
        ev_dispatch_hours,
        ev_dispatch_tomorrow_kwh,
        dispatches_checked_at,
        ev_dispatch_net_of_profile,
        ev_dispatch_windows,
    }
}

/// How much of the car the consumption profile still holds (issue #406),
/// which decides how planned car charging is added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CarInProfile {
    /// The GivEnergy charger recorded (nearly) all of the history, or the
    /// dispatch log covers all of it, so the car was taken out: add its
    /// planned energy in full.
    Removed,
    /// Only the "leave out load above" threshold trims it, which leaves up
    /// to the threshold of the car in each heavy hour: add just the car
    /// energy above each hour's own load.
    Trimmed,
    /// Nothing takes it out: add none of it.
    Unknown,
}

/// The share of the history the charger must have recorded for the car to
/// count as taken out. The profile is a median, so a missed day or two
/// can't bring the car back into it.
const CHARGER_COVERAGE_FOR_CAR_FREE: f64 = 0.95;

/// `dispatch_log_since` is when the dispatch log started without a break:
/// once that's no later than the history's start, and every dispatch
/// `logged` in the history had an energy figure to take out, the car is out.
fn car_in_profile(
    settings: &Settings,
    profile: &ConsumptionProfile,
    dispatch_log_since: Option<i64>,
    logged: &[LoggedDispatch],
) -> CarInProfile {
    let threshold = settings
        .forecast_exclude_load_above_kw
        .is_some_and(|kw| kw.is_finite() && kw > 0.0);
    let logged_throughout = match (dispatch_log_since, profile.history_start) {
        (Some(since), Some(start)) => {
            since <= start
                && logged
                    .iter()
                    .filter(|d| d.end > start)
                    // Nothing added leaves nothing in the history either.
                    .all(|d| {
                        d.energy_kwh == Some(0.0)
                            || believable_kwh(d.start, d.end, d.energy_kwh).is_some()
                    })
        }
        _ => false,
    };
    if profile.charger_covered_fraction >= CHARGER_COVERAGE_FOR_CAR_FREE || logged_throughout {
        CarInProfile::Removed
    } else if threshold {
        CarInProfile::Trimmed
    } else {
        CarInProfile::Unknown
    }
}

/// Battery AC rate limits in kW derived from snapshot registers, using
/// the same conversion the Control page applies (`inverter::power_limit`):
/// direct-limit families store 1–100% of the inverter's maximum, DC hybrids
/// store 0–50 as a percentage of battery capacity.
/// Returns (0, 0) when the max battery power is unknown.
pub(crate) fn battery_rate_limits_kw(snapshot: &InverterSnapshot) -> (f64, f64) {
    if snapshot.max_battery_power_w == 0 {
        return (0.0, 0.0);
    }
    // The raw register means different things per family: direct banks hold a
    // percentage of the inverter's maximum, while the DC-hybrid HR 111/112 holds
    // a percentage of battery capacity (GivTCP read.py), so the power it allows
    // depends on the pack size. See `inverter::power_limit`.
    let scale = crate::inverter::power_limit::PowerLimitScale::from_snapshot(snapshot);
    // A 0 rate register means "unset/unknown", not "disabled" — the AC
    // config / limit blocks are optional and read zero right after connect
    // (and on simulators). Fall back to the hardware maximum so the
    // projection still runs; a genuine user-configured 0 is rare and the
    // forecast is an estimate capped at the physical limit either way.
    let limit_kw = |raw: u8| {
        if raw == 0 {
            return snapshot.max_battery_power_w as f64 / 1000.0;
        }
        scale.raw_to_watts(u16::from(raw)) / 1000.0
    };
    (
        limit_kw(snapshot.charge_rate),
        limit_kw(snapshot.discharge_rate),
    )
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
    run_solar_forecast_fetch_at(state, Local::now()).await;
}

/// Testable implementation of [`run_solar_forecast_fetch`]. The fetch time is
/// supplied by the caller so tests can pin the split between past calibration
/// samples and forward rows without depending on the wall clock.
async fn run_solar_forecast_fetch_at(
    state: std::sync::Arc<crate::inverter::poll::AppState>,
    now: DateTime<Local>,
) {
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

    let settings = crate::settings::Settings::load_async().await;
    let rated_kw = effective_solar_kwp(&settings);
    let provider = OpenMeteoSolarProvider::new(&config.open_meteo_base_url);
    match provider.fetch(lat, lon).await {
        Ok(forecast) => {
            let sample_count = forecast.samples.len();
            let stored = tokio::task::spawn_blocking(move || {
                store_and_calibrate(
                    &db,
                    rated_kw,
                    OpenMeteoSolarProvider::SOURCE,
                    now,
                    &forecast,
                )
            })
            .await;
            match stored {
                Ok(Some(pr)) => tracing::info!(
                    pr,
                    samples = sample_count,
                    "solar forecast stored and performance ratio calibrated"
                ),
                Ok(None) => tracing::info!(
                    samples = sample_count,
                    "solar forecast stored; not enough history to calibrate performance ratio yet"
                ),
                Err(error) => tracing::warn!("Solar forecast database worker failed: {error}"),
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
    use crate::octopus_dispatches::{Dispatch, PlanningDispatches};
    use crate::settings::{Settings, SolarArrayConfig, WeatherConfig};
    use chrono::TimeZone;
    use std::path::PathBuf;
    use std::sync::Arc;

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

    fn forecast_response_body(samples: &[SolarForecastSample]) -> String {
        let times: Vec<String> = samples
            .iter()
            .map(|sample| {
                DateTime::<chrono::Utc>::from_timestamp(sample.timestamp, 0)
                    .expect("fixture timestamp must be valid")
                    .format("%Y-%m-%dT%H:%M")
                    .to_string()
            })
            .collect();
        let shortwave: Vec<f32> = samples
            .iter()
            .map(|sample| sample.shortwave_radiation)
            .collect();
        serde_json::json!({
            "latitude": 51.5,
            "longitude": -0.13,
            "hourly": {
                "time": times,
                "shortwave_radiation": shortwave,
            }
        })
        .to_string()
    }

    async fn spawn_forecast_http_server(
        responses: Vec<String>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for body in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 4096];
                let _request_len = stream.read(&mut request).await.unwrap();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (format!("http://{address}"), server)
    }

    #[tokio::test]
    async fn run_solar_forecast_fetch_returns_before_network_when_gated() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            use tokio::net::TcpListener;

            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let now = local_dt(2025, 6, 15, 12, 0);

            for config in [
                WeatherConfig {
                    enabled: false,
                    latitude: Some(51.5),
                    longitude: Some(-0.13),
                    open_meteo_base_url: base_url.clone(),
                    ..WeatherConfig::default()
                },
                WeatherConfig {
                    enabled: true,
                    latitude: None,
                    longitude: Some(-0.13),
                    open_meteo_base_url: base_url.clone(),
                    ..WeatherConfig::default()
                },
                WeatherConfig {
                    enabled: true,
                    latitude: Some(51.5),
                    longitude: Some(-0.13),
                    open_meteo_base_url: base_url.clone(),
                    ..WeatherConfig::default()
                },
            ] {
                let state = Arc::new(crate::inverter::poll::AppState::new());
                state.weather.lock().await.config = config;
                // The first two cases return before checking the database;
                // the third has coordinates but no HistoryDb. None may open
                // a network connection in any of the three cases.
                run_solar_forecast_fetch_at(state, now).await;
            }

            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                    .await
                    .is_err(),
                "disabled, unconfigured, and database-less cycles must not contact Open-Meteo"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn run_solar_forecast_fetch_retries_next_tick_and_uses_calibration_input() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let now = local_dt(2025, 6, 15, 12, 0);
            let now_ts = now.timestamp();
            let db = Arc::new(test_db());
            let rated_kw = 5.0_f64;
            let actual_kwh = [24.5_f32, 28.0, 31.5, 35.0, 21.0];

            // Five complete days of actual counter readings provide the
            // calibration input consumed by the successful fetch.
            for (day_index, &kwh) in actual_kwh.iter().enumerate() {
                let date = chrono::NaiveDate::from_ymd_opt(2025, 6, 10 + day_index as u32).unwrap();
                for hour in 7..19u32 {
                    let timestamp = Local
                        .from_local_datetime(&date.and_hms_opt(hour, 0, 0).unwrap())
                        .earliest()
                        .unwrap()
                        .timestamp();
                    let fraction = (hour - 6) as f32 / 12.0;
                    db.insert_reading(&InverterSnapshot {
                        timestamp,
                        today_solar_kwh: kwh * fraction,
                        ..Default::default()
                    });
                }
            }

            let mut samples = Vec::new();
            for day_index in 0..5u32 {
                let date = chrono::NaiveDate::from_ymd_opt(2025, 6, 10 + day_index).unwrap();
                for hour in 7..17u32 {
                    let timestamp = Local
                        .from_local_datetime(&date.and_hms_opt(hour, 0, 0).unwrap())
                        .earliest()
                        .unwrap()
                        .timestamp();
                    samples.push(sample(timestamp, 700.0, None, None));
                }
            }
            samples.push(sample(now_ts + 3600, 400.0, None, None));
            let successful_body = forecast_response_body(&samples);
            let (base_url, server) = spawn_forecast_http_server(vec![
                serde_json::json!({ "error": true, "reason": "temporary upstream failure" })
                    .to_string(),
                successful_body,
            ])
            .await;

            let settings = Settings {
                pv1_rated_kw: rated_kw,
                ..Settings::default()
            };
            settings.save().unwrap();

            let state = Arc::new(crate::inverter::poll::AppState::new());
            *state.history.lock().await = Some(db.clone());
            state.weather.lock().await.config = WeatherConfig {
                enabled: true,
                latitude: Some(51.5),
                longitude: Some(-0.13),
                open_meteo_base_url: base_url,
                ..WeatherConfig::default()
            };

            // A failed fetch is not persisted; the next weather-loop tick
            // retries the same provider and can succeed.
            run_solar_forecast_fetch_at(state.clone(), now).await;
            assert_eq!(series_len(db.as_ref(), "shortwave_radiation"), 0);
            assert!(db.get_meta_value(META_FORECAST_PR).is_none());

            run_solar_forecast_fetch_at(state, now).await;
            assert_eq!(series_len(db.as_ref(), "shortwave_radiation"), 1);
            assert_eq!(
                db.get_meta_value(META_FORECAST_PR)
                    .and_then(|value| value.parse::<f64>().ok()),
                Some(0.8)
            );
            assert_eq!(
                db.get_meta_value(META_FORECAST_PR_DAYS)
                    .and_then(|value| value.parse::<u32>().ok()),
                Some(5)
            );

            server.await.unwrap();
        })
        .await;
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
    fn rate_limits_cover_three_phase_and_hv_families() {
        use crate::inverter::model::DeviceType;
        let mut snap = rate_snapshot();

        // Three-phase control banks store a direct 1–100 percentage, so raw
        // 50 is half of the hardware maximum.
        for device_type in [DeviceType::ThreePhase, DeviceType::ACThreePhase] {
            snap.device_type = device_type;
            assert_eq!(
                battery_rate_limits_kw(&snap),
                (2.5, 2.5),
                "{device_type:?} uses direct three-phase limits"
            );
        }

        // The residential and HV AIO variants (and the single-phase HV Gen3)
        // are HV batteries but stay on the single-phase HR111/112 0-50 scale,
        // as GivTCP reads and writes them (issue #432).
        for device_type in [
            DeviceType::AllInOne6kW,
            DeviceType::AllInOne3_6kW,
            DeviceType::AllInOne5kW,
            DeviceType::HybridHvGen3,
            DeviceType::AllInOneHybrid,
        ] {
            snap.device_type = device_type;
            assert_eq!(
                battery_rate_limits_kw(&snap),
                (5.0, 5.0),
                "{device_type:?} uses the DC-hybrid half scale"
            );
        }
    }

    /// Issue #346 regression, reported on a Gen1 Hybrid: a 2600 W inverter
    /// with HR 111 = 33 (displayed 66%) must project 66% of 2600 W, not the
    /// full rating, and the Gateway's direct HR 313/314 must not be doubled.
    #[test]
    fn rate_limits_for_the_gen1_hybrid_reporter_and_the_gateway() {
        use crate::inverter::model::DeviceType;
        let mut snap = rate_snapshot();
        snap.device_type = DeviceType::Gen1Hybrid;
        snap.max_battery_power_w = 2600;
        snap.charge_rate = 33;
        snap.discharge_rate = 25;
        let (charge, discharge) = battery_rate_limits_kw(&snap);
        assert!((charge - 1.716).abs() < 1e-9, "66% of 2.6 kW, got {charge}");
        assert!(
            (discharge - 1.3).abs() < 1e-9,
            "50% of 2.6 kW, got {discharge}"
        );

        // Every even display percentage round-trips through the half scale.
        for raw in 1..=50u8 {
            snap.charge_rate = raw;
            let (charge, _) = battery_rate_limits_kw(&snap);
            let expected = raw as f64 * 2.0 / 100.0 * 2.6;
            assert!((charge - expected).abs() < 1e-9, "raw {raw}");
        }

        snap.device_type = DeviceType::Gateway;
        snap.max_battery_power_w = 6000;
        snap.charge_rate = 66;
        snap.discharge_rate = 66;
        let (charge, _) = battery_rate_limits_kw(&snap);
        assert!((charge - 3.96).abs() < 1e-9, "Gateway HR 313 is direct");
    }

    /// Issue #346: the DC-hybrid HR 111/112 register is a percentage of battery
    /// *capacity* (GivTCP read.py: `min(reg / 100 * capacity_w, inverter_max)`),
    /// so the power the forecast assumes must follow the pack size. A 9.5 kWh
    /// Gen1 behind a 2.6 kW inverter with the reporter's old register 31 was
    /// never throttled at all.
    #[test]
    fn rate_limits_follow_the_pack_size_on_the_half_scale_register() {
        use crate::inverter::model::DeviceType;
        let mut snap = rate_snapshot();
        snap.device_type = DeviceType::Gen1Hybrid;
        snap.max_battery_power_w = 2600;
        snap.battery_capacity_kwh = 9.5;

        // Register 17 = 17% of 9500 Wh = 1615 W.
        snap.charge_rate = 17;
        snap.discharge_rate = 10;
        let (charge, discharge) = battery_rate_limits_kw(&snap);
        assert!((charge - 1.615).abs() < 1e-9, "got {charge}");
        assert!((discharge - 0.95).abs() < 1e-9, "got {discharge}");

        // 31 and the factory default 50 both exceed the inverter's rating.
        for raw in [28, 31, 50] {
            snap.charge_rate = raw;
            let (charge, _) = battery_rate_limits_kw(&snap);
            assert!((charge - 2.6).abs() < 1e-9, "raw {raw}: {charge}");
        }

        // A small pack where capacity / 2 is the maximum keeps the doubling.
        snap.battery_capacity_kwh = 5.12;
        snap.max_battery_power_w = 2560;
        snap.charge_rate = 25;
        let (charge, _) = battery_rate_limits_kw(&snap);
        // 5.12 is not exact in f32, hence the looser tolerance.
        assert!((charge - 1.28).abs() < 1e-6, "got {charge}");
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
        seed_consumption_history(db, now);
    }

    /// Seven days of hourly home-energy counters, +0.5 kWh every hour,
    /// all 24 hours covered: today-7 through yesterday inclusive.
    fn seed_consumption_history(db: &HistoryDb, now: DateTime<Local>) {
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

    /// No dispatches, borrowed for as long as the inputs need.
    static NO_DISPATCHES: PlanningDispatches = PlanningDispatches {
        dispatches: Vec::new(),
        checked_at: None,
        stale: false,
        hold_applies: false,
    };

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
            dispatches: &NO_DISPATCHES,
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

    /// Issue #400: the charger's recorded power comes out of the profile,
    /// and the threshold clips load where there's no charger data.
    #[test]
    fn ev_charging_is_left_out_of_the_consumption_prediction() {
        let db = test_db();
        let now = local_dt(2025, 1, 15, 20, 0);
        seed_consumption_history(&db, now);
        let settings = five_kwp_settings();
        let payload = build_forecast_payload(&full_forecast_inputs(&db, None, now, &settings));
        assert!((payload.consumption_tomorrow_kwh - 12.0).abs() < 1e-6);
        assert_eq!(payload.consumption_excluded_ev_kwh, 0.0);
        assert_eq!(payload.consumption_excluded_above_kwh, 0.0);
        assert_eq!(payload.consumption_excluded_dispatch_kwh, 0.0);

        // The charger drew 300 W through 18:00–19:00 every day.
        let mut date = now.date_naive() - chrono::Duration::days(7);
        while date < now.date_naive() {
            let start = Local
                .from_local_datetime(&date.and_hms_opt(18, 0, 0).unwrap())
                .earliest()
                .unwrap()
                .timestamp();
            for m in 0..60 {
                db.record_evc_power_minute(&crate::history::evc_power::EvcMinuteRow {
                    timestamp: start + m * 60,
                    power_w: 300.0,
                    samples: 6,
                })
                .unwrap();
            }
            date = date.succ_opt().unwrap();
        }
        let payload = build_forecast_payload(&full_forecast_inputs(&db, None, now, &settings));
        assert!((payload.consumption_tomorrow_kwh - 11.7).abs() < 1e-6);
        assert!((payload.consumption_excluded_ev_kwh - 7.0 * 0.3).abs() < 1e-6);

        // Load above 0.4 kW is clipped in every other hour; 18:00 has the
        // charger's own figure, so it isn't clipped as well.
        let settings = Settings {
            forecast_exclude_load_above_kw: Some(0.4),
            ..five_kwp_settings()
        };
        let payload = build_forecast_payload(&full_forecast_inputs(&db, None, now, &settings));
        assert!((payload.consumption_tomorrow_kwh - (0.2 + 23.0 * 0.4)).abs() < 1e-6);
        assert!(payload.consumption_excluded_above_kwh > 0.0);
    }

    /// Issue #359: the plan the API serves and the automation applies
    /// follows the configured strategy.
    #[test]
    fn plan_follows_the_configured_strategy() {
        use crate::forecast::planner::PlanRecommendation;
        let db = test_db();
        let now = local_dt(2025, 1, 15, 20, 0);
        seed_full_forecast_state(&db, now);
        // Overcast: no solar at all, so the battery runs down overnight.
        let now_ts = now.timestamp();
        let hour_start = now_ts - now_ts.rem_euclid(3600);
        for h in 0..72i64 {
            db.insert_forecast_values(&[ForecastValueRow {
                timestamp: hour_start + h * 3600,
                variable: "shortwave_radiation".to_string(),
                value: 0.0,
                source: "open-meteo".to_string(),
                fetched_at: now_ts,
            }])
            .unwrap();
        }
        let slot = |start: &str, end: &str, rate: f64| crate::settings::TariffSlot {
            start: start.to_string(),
            end: end.to_string(),
            rate,
        };
        let mut settings = five_kwp_settings();
        settings.import_tariff_config = Some(crate::settings::TariffConfig {
            slots: vec![slot("00:00", "05:00", 0.07), slot("05:00", "23:59", 0.27)],
        });
        let snap = battery_snapshot();
        let payload =
            build_forecast_payload(&full_forecast_inputs(&db, Some(&snap), now, &settings));

        let default_plan = crate::server::api::compute_plan_recommendation(
            &payload,
            &settings,
            Some(&snap),
            now_ts,
        );
        assert!(
            matches!(default_plan, PlanRecommendation::Charge { .. }),
            "{default_plan:?}"
        );

        settings.forecast_plan_strategy = crate::settings::PlanStrategy::HoldWindow;
        let hold_plan = crate::server::api::compute_plan_recommendation(
            &payload,
            &settings,
            Some(&snap),
            now_ts,
        );
        assert!(
            matches!(hold_plan, PlanRecommendation::ChargeAndHold { .. }),
            "{hold_plan:?}"
        );

        settings.forecast_plan_strategy = crate::settings::PlanStrategy::Cheapest;
        let cheapest_plan = crate::server::api::compute_plan_recommendation(
            &payload,
            &settings,
            Some(&snap),
            now_ts,
        );
        assert!(
            matches!(
                cheapest_plan,
                PlanRecommendation::ChargeAndHold {
                    strategy: crate::settings::PlanStrategy::Cheapest,
                    ..
                }
            ),
            "{cheapest_plan:?}"
        );
    }

    /// Issue #359 review: the export advice must see a hold plan's charge,
    /// or it can sell into the very hours the battery is being charged.
    #[test]
    fn export_advice_respects_a_hold_plan_charge() {
        use crate::forecast::planner::ExportAdvice;
        let db = test_db();
        let now = local_dt(2025, 1, 15, 20, 0);
        seed_full_forecast_state(&db, now);
        let now_ts = now.timestamp();
        let hour_start = now_ts - now_ts.rem_euclid(3600);
        for h in 0..72i64 {
            db.insert_forecast_values(&[ForecastValueRow {
                timestamp: hour_start + h * 3600,
                variable: "shortwave_radiation".to_string(),
                value: 0.0,
                source: "open-meteo".to_string(),
                fetched_at: now_ts,
            }])
            .unwrap();
        }
        let slot = |start: &str, end: &str, rate: f64| crate::settings::TariffSlot {
            start: start.to_string(),
            end: end.to_string(),
            rate,
        };
        let mut settings = five_kwp_settings();
        settings.import_tariff_config = Some(crate::settings::TariffConfig {
            slots: vec![slot("00:00", "05:00", 0.07), slot("05:00", "23:59", 0.27)],
        });
        // The best export window, 23:00–01:00, runs into the cheap charging
        // hours that start at midnight.
        settings.export_tariff_config = Some(crate::settings::TariffConfig {
            slots: vec![
                slot("00:00", "01:00", 0.40),
                slot("01:00", "23:00", 0.05),
                slot("23:00", "23:59", 0.40),
            ],
        });
        let snap = battery_snapshot();
        let payload =
            build_forecast_payload(&full_forecast_inputs(&db, Some(&snap), now, &settings));
        for strategy in [
            crate::settings::PlanStrategy::MinSoc,
            crate::settings::PlanStrategy::HoldWindow,
        ] {
            settings.forecast_plan_strategy = strategy;
            let (_, export) =
                crate::server::api::compute_full_plan(&payload, &settings, Some(&snap), now_ts);
            match export {
                Some(ExportAdvice::NoExport { reason }) => assert!(
                    reason.contains("overlaps the planned charge"),
                    "{strategy:?}: {reason}"
                ),
                other => panic!("{strategy:?}: expected an overlap refusal, got {other:?}"),
            }
        }
    }

    /// Issue #359: the cheapest strategy is fed the forecast's low solar band
    /// and the export tariff through the API path. With the sun doubtful it
    /// charges further than the same night's hold plan, and a rich export
    /// tariff changes the answer again.
    #[test]
    fn cheapest_strategy_is_fed_the_low_solar_band_and_the_export_tariff() {
        use crate::forecast::planner::PlanRecommendation;
        let target = |plan: PlanRecommendation| match plan {
            PlanRecommendation::ChargeAndHold { target_soc_pct, .. } => target_soc_pct,
            other => panic!("expected ChargeAndHold, got {other:?}"),
        };
        let now = local_dt(2025, 1, 15, 20, 0);
        let now_ts = now.timestamp();
        let hour_start = now_ts - now_ts.rem_euclid(3600);
        let db = test_db();
        seed_full_forecast_state(&db, now);
        // Real but modest winter sun by day, none at night.
        for h in 0..72i64 {
            let ts = hour_start + h * 3600;
            let local_hour = chrono::DateTime::from_timestamp(ts, 0)
                .unwrap()
                .with_timezone(&Local)
                .hour();
            db.insert_forecast_values(&[ForecastValueRow {
                timestamp: ts,
                variable: "shortwave_radiation".to_string(),
                value: if (9..=15).contains(&local_hour) {
                    300.0
                } else {
                    0.0
                },
                source: "open-meteo".to_string(),
                fetched_at: now_ts,
            }])
            .unwrap();
        }
        let slot = |start: &str, end: &str, rate: f64| crate::settings::TariffSlot {
            start: start.to_string(),
            end: end.to_string(),
            rate,
        };
        let mut settings = five_kwp_settings();
        settings.import_tariff_config = Some(crate::settings::TariffConfig {
            slots: vec![slot("00:00", "05:00", 0.07), slot("05:00", "23:59", 0.27)],
        });
        let snap = battery_snapshot();
        let payload =
            build_forecast_payload(&full_forecast_inputs(&db, Some(&snap), now, &settings));
        assert!(
            payload.solar.iter().any(|s| s.band_low < s.kwh),
            "the fixture needs a low band below the median"
        );

        settings.forecast_plan_strategy = crate::settings::PlanStrategy::HoldWindow;
        let hold = target(crate::server::api::compute_plan_recommendation(
            &payload,
            &settings,
            Some(&snap),
            now_ts,
        ));
        settings.forecast_plan_strategy = crate::settings::PlanStrategy::Cheapest;
        let unpaid = target(crate::server::api::compute_plan_recommendation(
            &payload,
            &settings,
            Some(&snap),
            now_ts,
        ));
        assert!(unpaid > hold, "cheapest {unpaid} vs hold {hold}");

        settings.export_tariff_config = Some(crate::settings::TariffConfig {
            slots: vec![slot("00:00", "23:59", 0.60)],
        });
        let paid = target(crate::server::api::compute_plan_recommendation(
            &payload,
            &settings,
            Some(&snap),
            now_ts,
        ));
        assert_ne!(paid, unpaid, "the export tariff must reach the planner");
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
    fn payload_prorates_the_current_solar_hour() {
        let db = test_db();
        let hour_start = local_dt(2025, 6, 15, 12, 0);
        let now = hour_start + chrono::Duration::minutes(30);
        db.insert_forecast_values(&[ForecastValueRow {
            timestamp: hour_start.timestamp(),
            variable: "shortwave_radiation".to_string(),
            value: 1000.0,
            source: "open-meteo".to_string(),
            fetched_at: now.timestamp(),
        }])
        .unwrap();
        db.set_meta_value(META_FORECAST_PR, "1.0").unwrap();

        let settings = Settings {
            pv1_rated_kw: 1.0,
            ..Settings::default()
        };
        let payload = build_forecast_payload(&full_forecast_inputs(&db, None, now, &settings));

        // The current forecast hour is 1 kWh in total, but only its final
        // 30 minutes remain.
        assert!((payload.solar_today_remaining_kwh - 0.5).abs() < 1e-9);
    }

    #[test]
    fn payload_reports_forecast_freshness_and_suppresses_stale_plan() {
        let db = test_db();
        let now = local_dt(2025, 6, 15, 12, 0);
        let now_ts = now.timestamp();
        let settings = five_kwp_settings();

        db.insert_forecast_values(&[ForecastValueRow {
            timestamp: now_ts + 3600,
            variable: "shortwave_radiation".to_string(),
            value: 500.0,
            source: "open-meteo".to_string(),
            fetched_at: now_ts - 3600,
        }])
        .unwrap();
        let fresh = build_forecast_payload(&full_forecast_inputs(&db, None, now, &settings));
        assert_eq!(fresh.forecast_fetched_at, Some(now_ts - 3600));
        assert_eq!(fresh.forecast_age_seconds, Some(3600));
        assert!(!fresh.status.contains(&"forecast_stale".to_string()));
        assert!(!fresh.status.contains(&"forecast_data_future".to_string()));

        let stale_at = now_ts - SOLAR_FORECAST_MAX_AGE_SECS - 1;
        db.insert_forecast_values(&[ForecastValueRow {
            timestamp: now_ts + 3600,
            variable: "shortwave_radiation".to_string(),
            value: 500.0,
            source: "open-meteo".to_string(),
            fetched_at: stale_at,
        }])
        .unwrap();
        let stale = build_forecast_payload(&full_forecast_inputs(&db, None, now, &settings));
        assert_eq!(stale.forecast_fetched_at, Some(stale_at));
        assert_eq!(
            stale.forecast_age_seconds,
            Some(SOLAR_FORECAST_MAX_AGE_SECS + 1)
        );
        assert!(stale.status.contains(&"forecast_stale".to_string()));

        let future_at = now_ts + 3600;
        db.insert_forecast_values(&[ForecastValueRow {
            timestamp: now_ts + 3600,
            variable: "shortwave_radiation".to_string(),
            value: 500.0,
            source: "open-meteo".to_string(),
            fetched_at: future_at,
        }])
        .unwrap();
        let future = build_forecast_payload(&full_forecast_inputs(&db, None, now, &settings));
        assert_eq!(future.forecast_fetched_at, Some(future_at));
        assert_eq!(future.forecast_age_seconds, Some(-3600));
        assert!(future.status.contains(&"forecast_data_future".to_string()));

        let mut full_settings = five_kwp_settings();
        full_settings.import_tariff_config = Some(crate::settings::TariffConfig {
            slots: vec![
                crate::settings::TariffSlot {
                    start: "00:00".to_string(),
                    end: "02:00".to_string(),
                    rate: 0.10,
                },
                crate::settings::TariffSlot {
                    start: "02:00".to_string(),
                    end: "23:59".to_string(),
                    rate: 0.25,
                },
            ],
        });
        seed_full_forecast_state(&db, now);
        let current = build_forecast_payload(&full_forecast_inputs(
            &db,
            Some(&battery_snapshot()),
            now,
            &full_settings,
        ));
        assert!(!matches!(
            crate::server::api::compute_plan_recommendation(
                &current,
                &full_settings,
                Some(&battery_snapshot()),
                now_ts,
            ),
            crate::forecast::planner::PlanRecommendation::NoPlan { .. }
        ));

        let stale_at = now_ts - SOLAR_FORECAST_MAX_AGE_SECS - 1;
        let hour_start = now_ts - now_ts.rem_euclid(3600);
        for h in 0..72i64 {
            db.insert_forecast_values(&[ForecastValueRow {
                timestamp: hour_start + h * 3600,
                variable: "shortwave_radiation".to_string(),
                value: 500.0,
                source: "open-meteo".to_string(),
                fetched_at: stale_at,
            }])
            .unwrap();
        }
        let stale_full = build_forecast_payload(&full_forecast_inputs(
            &db,
            Some(&battery_snapshot()),
            now,
            &full_settings,
        ));
        assert!(stale_full.status.contains(&"forecast_stale".to_string()));
        assert!(matches!(
            crate::server::api::compute_plan_recommendation(
                &stale_full,
                &full_settings,
                Some(&battery_snapshot()),
                now_ts,
            ),
            crate::forecast::planner::PlanRecommendation::NoPlan { .. }
        ));
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
            dispatches: &PlanningDispatches::default(),
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

    #[test]
    fn payload_omits_projection_for_non_finite_battery_inputs() {
        let db = test_db();
        let now = local_dt(2025, 6, 15, 12, 0);
        seed_full_forecast_state(&db, now);
        let settings = five_kwp_settings();

        let mut snap = battery_snapshot();
        snap.battery_capacity_kwh = f32::NAN;
        let payload =
            build_forecast_payload(&full_forecast_inputs(&db, Some(&snap), now, &settings));
        assert!(payload
            .status
            .contains(&"invalid_battery_configuration".to_string()));
        assert!(payload.battery.is_none());

        let mut snap = battery_snapshot();
        let mut settings = five_kwp_settings();
        settings.forecast_charge_efficiency = f64::NAN;
        let payload =
            build_forecast_payload(&full_forecast_inputs(&db, Some(&snap), now, &settings));
        assert!(payload
            .status
            .contains(&"invalid_battery_configuration".to_string()));
        assert!(payload.battery.is_none());

        snap = battery_snapshot();
        settings = five_kwp_settings();
        settings.forecast_discharge_efficiency = f64::NAN;
        let payload =
            build_forecast_payload(&full_forecast_inputs(&db, Some(&snap), now, &settings));
        assert!(payload
            .status
            .contains(&"invalid_battery_configuration".to_string()));
        assert!(payload.battery.is_none());
    }

    #[test]
    fn payload_omits_current_schedule_when_no_slot_enabled() {
        // Without enabled slots the schedule projection would duplicate
        // the Eco line — the payload must leave the field empty so the
        // UI hides it (issue #297).
        let db = test_db();
        let now = local_dt(2025, 6, 15, 12, 0);
        seed_full_forecast_state(&db, now);
        let snap = battery_snapshot();
        let settings = five_kwp_settings();
        let payload =
            build_forecast_payload(&full_forecast_inputs(&db, Some(&snap), now, &settings));
        let battery = payload.battery.as_ref().expect("battery projection");
        assert!(battery.with_current_schedule.is_none());
    }

    #[test]
    fn payload_includes_current_schedule_diverging_from_eco_overnight() {
        // An enabled overnight charge slot (23:00–06:00 → 65%): the Eco
        // projection keeps draining toward the reserve overnight while
        // the current-schedule projection charges to and holds 65% —
        // visibly higher at 02:00 (issue #297). Solar is limited to
        // 12:00–14:00 so the night is genuinely dark.
        let db = test_db();
        let now = local_dt(2025, 6, 15, 12, 0);
        seed_consumption_history(&db, now);
        let now_ts = now.timestamp();
        let hour_start = now_ts - now_ts.rem_euclid(3600);
        for h in 0..72i64 {
            let ts = hour_start + h * 3600;
            let local_hour = chrono::DateTime::from_timestamp(ts, 0)
                .map(|dt| dt.with_timezone(&chrono::Local).hour())
                .unwrap_or(0);
            db.insert_forecast_values(&[ForecastValueRow {
                timestamp: ts,
                variable: "shortwave_radiation".to_string(),
                value: if (12..=14).contains(&local_hour) {
                    500.0
                } else {
                    0.0
                },
                source: "open-meteo".to_string(),
                fetched_at: now_ts,
            }])
            .unwrap();
        }
        db.set_meta_value(META_FORECAST_PR, "0.8").unwrap();
        db.set_meta_value(META_FORECAST_PR_DAYS, "12").unwrap();
        let mut snap = battery_snapshot();
        snap.enable_charge = true;
        snap.charge_slots[0] = crate::inverter::model::ScheduleSlot {
            enabled: true,
            start_hour: 23,
            start_minute: 0,
            end_hour: 6,
            end_minute: 0,
            target_soc: 65,
        };
        let settings = five_kwp_settings();
        let payload =
            build_forecast_payload(&full_forecast_inputs(&db, Some(&snap), now, &settings));
        let battery = payload.battery.as_ref().expect("battery projection");
        let schedule_series = battery
            .with_current_schedule
            .as_ref()
            .expect("enabled charge slot must yield a schedule projection");
        assert_eq!(schedule_series.len(), battery.hours.len());
        // Compare at 02:00 tomorrow, mid-slot after the overnight drain.
        let tomorrow_2am = Local
            .with_ymd_and_hms(2025, 6, 16, 2, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        let schedule_soc = schedule_series
            .iter()
            .find(|(ts, _)| *ts == tomorrow_2am)
            .map(|(_, soc)| *soc)
            .expect("02:00 must be in the forward horizon");
        let eco_soc = battery
            .hours
            .iter()
            .find(|(ts, _)| *ts == tomorrow_2am)
            .map(|(_, soc)| *soc)
            .expect("02:00 must be in the forward horizon");
        assert!(
            schedule_soc > eco_soc,
            "schedule projection ({schedule_soc}) must hold above eco ({eco_soc}) overnight"
        );
        assert!(
            schedule_soc >= 65.0 - 1e-6,
            "slot target held at 02:00: {schedule_soc}"
        );
    }

    // ---- planned car charging (issue #406) ----

    /// No sun at all for 72 h, so the battery covers the house (0.5 kWh an
    /// hour) from a full start and a hold visibly keeps energy in it.
    fn seed_dark_forecast_state(db: &HistoryDb, now: DateTime<Local>) {
        let now_ts = now.timestamp();
        let hour_start = now_ts - now_ts.rem_euclid(3600);
        for h in 0..72i64 {
            db.insert_forecast_values(&[ForecastValueRow {
                timestamp: hour_start + h * 3600,
                variable: "shortwave_radiation".to_string(),
                value: 0.0,
                source: "open-meteo".to_string(),
                fetched_at: now_ts,
            }])
            .unwrap();
        }
        db.set_meta_value(META_FORECAST_PR, "0.8").unwrap();
        db.set_meta_value(META_FORECAST_PR_DAYS, "12").unwrap();
        seed_consumption_history(db, now);
    }

    fn car_now() -> DateTime<Local> {
        local_dt(2025, 6, 15, 12, 0)
    }

    fn local_ts(day: u32, hour: u32) -> i64 {
        local_dt(2025, 6, day, hour, 0).timestamp()
    }

    fn car_dispatch(start: i64, end: i64, kind: &str, kwh: Option<f64>) -> Dispatch {
        Dispatch {
            start: chrono::DateTime::from_timestamp(start, 0).unwrap(),
            end: chrono::DateTime::from_timestamp(end, 0).unwrap(),
            kind: Some(kind.to_string()),
            energy_added_kwh: kwh,
        }
    }

    fn fresh(dispatches: Vec<Dispatch>) -> PlanningDispatches {
        PlanningDispatches {
            dispatches,
            checked_at: Some(car_now().with_timezone(&chrono::Utc)),
            stale: false,
            hold_applies: true,
        }
    }

    /// Settings with the "leave out load above" threshold set, which trims
    /// the car out of the history only above the threshold.
    fn threshold_settings() -> Settings {
        Settings {
            forecast_exclude_load_above_kw: Some(3.0),
            ..five_kwp_settings()
        }
    }

    /// The seeded history and a GivEnergy charger recording all of it, so
    /// #400 took the car out and the profile is free of it.
    fn seed_car_free_state(db: &HistoryDb, now: DateTime<Local>) -> Settings {
        seed_dark_forecast_state(db, now);
        let from = (now.date_naive() - chrono::Duration::days(7))
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(Local)
            .earliest()
            .unwrap()
            .timestamp();
        record_charger_minutes(db, from, now.timestamp());
        five_kwp_settings()
    }

    fn record_charger_minutes(db: &HistoryDb, from: i64, to: i64) {
        let rows: Vec<_> = (from..to)
            .step_by(60)
            .map(|timestamp| crate::history::evc_power::EvcMinuteRow {
                timestamp,
                power_w: 0.0,
                samples: 1,
            })
            .collect();
        for row in &rows {
            db.record_evc_power_minute(row).unwrap();
        }
    }

    fn full_battery() -> InverterSnapshot {
        InverterSnapshot {
            soc: 100,
            ..battery_snapshot()
        }
    }

    fn car_payload(
        db: &HistoryDb,
        settings: &Settings,
        dispatches: &PlanningDispatches,
    ) -> ForecastPayload {
        let snap = full_battery();
        build_forecast_payload(&ForecastInputs {
            dispatches,
            ..full_forecast_inputs(db, Some(&snap), car_now(), settings)
        })
    }

    /// SOC at the end of the hour starting at `ts`.
    fn soc_after(payload: &ForecastPayload, ts: i64) -> f64 {
        payload
            .battery
            .as_ref()
            .expect("projection")
            .hours
            .iter()
            .find(|(hour, _)| *hour == ts)
            .map(|(_, soc)| *soc)
            .expect("hour in the projection")
    }

    #[test]
    fn a_held_evening_dispatch_keeps_the_house_off_the_battery() {
        let db = test_db();
        let settings = seed_car_free_state(&db, car_now());
        let without = car_payload(&db, &settings, &PlanningDispatches::default());
        let tonight = fresh(vec![car_dispatch(
            local_ts(15, 19),
            local_ts(15, 21),
            "SMART",
            Some(7.0),
        )]);
        let with = car_payload(&db, &settings, &tonight);

        // Two held hours of 0.5 kWh house load come from the grid, not the
        // battery; the car itself never touches it.
        let saved = 2.0 * 0.5 / 0.95 / 10.0 * 100.0;
        let after = local_ts(15, 20);
        assert!(
            (soc_after(&with, after) - soc_after(&without, after) - saved).abs() < 1e-6,
            "with {} without {}",
            soc_after(&with, after),
            soc_after(&without, after)
        );
        assert_eq!(
            with.ev_dispatch_hours,
            vec![
                DispatchHour {
                    timestamp: local_ts(15, 19),
                    ev_kwh: 3.5,
                    held_fraction: 1.0
                },
                DispatchHour {
                    timestamp: local_ts(15, 20),
                    ev_kwh: 3.5,
                    held_fraction: 1.0
                },
            ]
        );
        assert_eq!(with.dispatches_checked_at, Some(car_now().timestamp()));
        assert!(without.ev_dispatch_hours.is_empty());
    }

    #[test]
    fn the_payload_lists_the_planned_charges_as_octopus_gave_them() {
        let db = test_db();
        let settings = seed_car_free_state(&db, car_now());
        let half = 1800;
        let list = fresh(vec![
            // Already over: not listed.
            car_dispatch(local_ts(15, 9), local_ts(15, 10), "SMART", Some(7.0)),
            car_dispatch(
                local_ts(15, 20) + half,
                local_ts(15, 21),
                "BOOST",
                Some(3.5),
            ),
            car_dispatch(local_ts(15, 19) + half, local_ts(15, 20), "SMART", None),
            // Beyond the forecast horizon: not listed.
            car_dispatch(local_ts(20, 1), local_ts(20, 2), "SMART", Some(7.0)),
        ]);
        let payload = car_payload(&db, &settings, &list);
        assert_eq!(
            payload.ev_dispatch_windows,
            vec![
                DispatchWindow {
                    start: local_ts(15, 19) + half,
                    end: local_ts(15, 20),
                    held: true,
                    kwh: None,
                },
                DispatchWindow {
                    start: local_ts(15, 20) + half,
                    end: local_ts(15, 21),
                    held: false,
                    kwh: Some(3.5),
                },
            ]
        );

        // During a calibration run nothing is held.
        let unheld = PlanningDispatches {
            hold_applies: false,
            ..list
        };
        let payload = car_payload(&db, &settings, &unheld);
        assert!(payload.ev_dispatch_windows.iter().all(|w| !w.held));
        assert!(car_payload(&db, &settings, &PlanningDispatches::default())
            .ev_dispatch_windows
            .is_empty());
    }

    #[test]
    fn the_planned_charges_are_listed_without_solar_data() {
        // Weather off: no forecast hours, but the charges are still known.
        let db = test_db();
        seed_consumption_history(&db, car_now());
        let list = fresh(vec![car_dispatch(
            local_ts(15, 19),
            local_ts(15, 20),
            "SMART",
            Some(7.0),
        )]);
        let payload = car_payload(&db, &five_kwp_settings(), &list);
        assert!(payload.solar.is_empty());
        assert_eq!(
            payload.ev_dispatch_windows,
            vec![DispatchWindow {
                start: local_ts(15, 19),
                end: local_ts(15, 20),
                held: true,
                kwh: Some(7.0),
            }]
        );
    }

    #[test]
    fn a_boost_runs_from_the_battery() {
        let db = test_db();
        let settings = seed_car_free_state(&db, car_now());
        let without = car_payload(&db, &settings, &PlanningDispatches::default());
        let boost = fresh(vec![car_dispatch(
            local_ts(15, 19),
            local_ts(15, 20),
            "BOOST",
            Some(3.0),
        )]);
        let with = car_payload(&db, &settings, &boost);
        let drawn = 3.0 / 0.95 / 10.0 * 100.0;
        let after = local_ts(15, 19);
        assert!(
            (soc_after(&without, after) - soc_after(&with, after) - drawn).abs() < 1e-6,
            "with {} without {}",
            soc_after(&with, after),
            soc_after(&without, after)
        );
    }

    #[test]
    fn a_stale_list_changes_nothing_but_the_status() {
        let db = test_db();
        let settings = seed_car_free_state(&db, car_now());
        let without = car_payload(&db, &settings, &PlanningDispatches::default());
        // As `for_planning` gives it: stale, so no list and no time.
        let stale = PlanningDispatches {
            stale: true,
            ..PlanningDispatches::default()
        };
        let mut with = car_payload(&db, &settings, &stale);
        assert!(with.status.contains(&"dispatches_stale".to_string()));
        with.status.retain(|code| code != "dispatches_stale");
        assert_eq!(with, without);
    }

    #[test]
    fn a_history_that_may_hold_the_car_gets_the_hold_but_not_the_load() {
        let db = test_db();
        seed_dark_forecast_state(&db, car_now());
        let tonight = fresh(vec![car_dispatch(
            local_ts(15, 19),
            local_ts(15, 21),
            "SMART",
            Some(7.0),
        )]);
        // No GivEnergy charger history and no threshold.
        let payload = car_payload(&db, &five_kwp_settings(), &tonight);
        assert!(payload
            .status
            .contains(&"dispatch_load_not_added".to_string()));
        let hours: Vec<(f64, f64)> = payload
            .ev_dispatch_hours
            .iter()
            .map(|h| (h.ev_kwh, h.held_fraction))
            .collect();
        assert_eq!(hours, vec![(0.0, 1.0), (0.0, 1.0)]);
    }

    fn record_charger_minute(db: &HistoryDb, timestamp: i64) {
        db.record_evc_power_minute(&crate::history::evc_power::EvcMinuteRow {
            timestamp,
            power_w: 0.0,
            samples: 1,
        })
        .unwrap();
    }

    #[test]
    fn charger_history_covering_the_profile_lets_the_car_load_in() {
        // Only a week of history, all of it recorded by the charger: a new
        // install's profile is free of the car, without waiting four weeks.
        let db = test_db();
        let settings = seed_car_free_state(&db, car_now());
        let tonight = fresh(vec![car_dispatch(
            local_ts(15, 19),
            local_ts(15, 20),
            "SMART",
            Some(7.0),
        )]);
        let payload = car_payload(&db, &settings, &tonight);
        assert!(!payload
            .status
            .contains(&"dispatch_load_not_added".to_string()));
        assert!(!payload.ev_dispatch_net_of_profile);
        assert_eq!(
            payload.ev_dispatch_hours.first().map(|h| h.ev_kwh),
            Some(7.0)
        );
    }

    #[test]
    fn a_charger_that_stopped_recording_keeps_the_car_load_out() {
        // Recorded at the start of the history, then offline for the rest:
        // those nights still hold the car.
        let db = test_db();
        seed_dark_forecast_state(&db, car_now());
        let from = car_now().timestamp() - 7 * 86_400;
        record_charger_minutes(&db, from, from + 86_400);
        let tonight = fresh(vec![car_dispatch(
            local_ts(15, 19),
            local_ts(15, 20),
            "SMART",
            Some(7.0),
        )]);
        let payload = car_payload(&db, &five_kwp_settings(), &tonight);
        assert!(payload
            .status
            .contains(&"dispatch_load_not_added".to_string()));
        assert_eq!(
            payload.ev_dispatch_hours.first().map(|h| h.ev_kwh),
            Some(0.0)
        );
    }

    #[test]
    fn with_only_the_threshold_the_car_adds_what_the_profile_lacks() {
        // The threshold leaves up to its limit of the car in each heavy
        // hour, so only the car energy above the profile's hour goes in:
        // a 3 kWh boost on a 0.5 kWh hour adds 2.5, never the car twice.
        let db = test_db();
        seed_dark_forecast_state(&db, car_now());
        let settings = threshold_settings();
        let without = car_payload(&db, &settings, &PlanningDispatches::default());
        let boost = fresh(vec![car_dispatch(
            local_ts(15, 19),
            local_ts(15, 20),
            "BOOST",
            Some(3.0),
        )]);
        let with = car_payload(&db, &settings, &boost);
        assert!(!with.status.contains(&"dispatch_load_not_added".to_string()));
        assert!(with.ev_dispatch_net_of_profile);
        let drawn = 2.5 / 0.95 / 10.0 * 100.0;
        let after = local_ts(15, 19);
        assert!(
            (soc_after(&without, after) - soc_after(&with, after) - drawn).abs() < 1e-6,
            "with {} without {}",
            soc_after(&with, after),
            soc_after(&without, after)
        );
        // The chart still shows what Octopus plans for the car.
        assert_eq!(with.ev_dispatch_hours.first().map(|h| h.ev_kwh), Some(3.0));
    }

    #[test]
    fn a_smart_dispatch_the_hold_will_not_apply_to_runs_from_the_battery() {
        // During a calibration run the poll loop doesn't hold the battery,
        // so the car's smart charge comes out of it like a boost.
        let db = test_db();
        let settings = seed_car_free_state(&db, car_now());
        let without = car_payload(&db, &settings, &PlanningDispatches::default());
        let unheld = PlanningDispatches {
            hold_applies: false,
            ..fresh(vec![car_dispatch(
                local_ts(15, 19),
                local_ts(15, 20),
                "SMART",
                Some(3.0),
            )])
        };
        let with = car_payload(&db, &settings, &unheld);
        assert_eq!(
            with.ev_dispatch_hours
                .iter()
                .map(|h| (h.ev_kwh, h.held_fraction))
                .collect::<Vec<_>>(),
            vec![(3.0, 0.0)]
        );
        let drawn = 3.0 / 0.95 / 10.0 * 100.0;
        let after = local_ts(15, 19);
        assert!(
            (soc_after(&without, after) - soc_after(&with, after) - drawn).abs() < 1e-6,
            "with {} without {}",
            soc_after(&with, after),
            soc_after(&without, after)
        );
    }

    #[test]
    fn charger_history_only_from_recently_keeps_the_car_load_out() {
        let db = test_db();
        seed_dark_forecast_state(&db, car_now());
        record_charger_minute(&db, car_now().timestamp() - 2 * 86_400);
        let tonight = fresh(vec![car_dispatch(
            local_ts(15, 19),
            local_ts(15, 20),
            "SMART",
            Some(7.0),
        )]);
        let payload = car_payload(&db, &five_kwp_settings(), &tonight);
        assert!(payload
            .status
            .contains(&"dispatch_load_not_added".to_string()));
        assert_eq!(
            payload.ev_dispatch_hours.first().map(|h| h.ev_kwh),
            Some(0.0)
        );
    }

    /// Log a dispatch as the hold's poller would, seen until its end.
    fn log_dispatch(db: &HistoryDb, start: i64, end: i64, kwh: impl Into<Option<f64>>) {
        db.log_dispatches(
            &[crate::history::dispatch_log::DispatchSighting {
                device: "car".to_string(),
                start,
                end,
                kind: Some("SMART".to_string()),
                energy_kwh: kwh.into(),
            }],
            end,
        )
        .unwrap();
    }

    fn set_dispatch_log_since(db: &HistoryDb, since: i64) {
        db.set_meta_value(
            crate::history::dispatch_log::DISPATCH_LOG_SINCE_KEY,
            &since.to_string(),
        )
        .unwrap();
    }

    /// Without a GivEnergy charger, the dispatches that ran come out of the
    /// consumption prediction.
    #[test]
    fn logged_dispatches_are_left_out_of_the_consumption_prediction() {
        let db = test_db();
        let now = local_dt(2025, 1, 15, 20, 0);
        seed_consumption_history(&db, now);
        let mut date = now.date_naive() - chrono::Duration::days(7);
        while date < now.date_naive() {
            let start = Local
                .from_local_datetime(&date.and_hms_opt(18, 0, 0).unwrap())
                .earliest()
                .unwrap()
                .timestamp();
            log_dispatch(&db, start, start + 3600, 0.3);
            date = date.succ_opt().unwrap();
        }
        let settings = five_kwp_settings();
        let payload = build_forecast_payload(&full_forecast_inputs(&db, None, now, &settings));
        assert!((payload.consumption_tomorrow_kwh - 11.7).abs() < 1e-6);
        assert!((payload.consumption_excluded_dispatch_kwh - 7.0 * 0.3).abs() < 1e-6);
        assert_eq!(payload.consumption_excluded_ev_kwh, 0.0);
    }

    #[test]
    fn a_dispatch_log_covering_the_history_lets_the_car_load_in() {
        let db = test_db();
        seed_dark_forecast_state(&db, car_now());
        set_dispatch_log_since(&db, car_now().timestamp() - 8 * 86_400);
        let tonight = fresh(vec![car_dispatch(
            local_ts(15, 19),
            local_ts(15, 20),
            "SMART",
            Some(7.0),
        )]);
        // With the threshold set too, the log still means the car is out.
        for settings in [five_kwp_settings(), threshold_settings()] {
            let payload = car_payload(&db, &settings, &tonight);
            assert!(!payload
                .status
                .contains(&"dispatch_load_not_added".to_string()));
            assert!(!payload.ev_dispatch_net_of_profile);
            assert_eq!(
                payload.ev_dispatch_hours.first().map(|h| h.ev_kwh),
                Some(7.0)
            );
        }
    }

    #[test]
    fn a_dispatch_log_without_energy_figures_keeps_the_car_load_out() {
        // Octopus gave no energy for a dispatch three nights ago, so
        // nothing came out of that night and the car is still in it.
        let db = test_db();
        seed_dark_forecast_state(&db, car_now());
        let night = local_ts(12, 19);
        log_dispatch(&db, night, night + 3600, None);
        set_dispatch_log_since(&db, car_now().timestamp() - 8 * 86_400);
        let tonight = fresh(vec![car_dispatch(
            local_ts(15, 19),
            local_ts(15, 20),
            "SMART",
            Some(7.0),
        )]);
        let payload = car_payload(&db, &five_kwp_settings(), &tonight);
        assert!(payload
            .status
            .contains(&"dispatch_load_not_added".to_string()));

        // A dispatch that added nothing leaves nothing behind.
        let db = test_db();
        seed_dark_forecast_state(&db, car_now());
        log_dispatch(&db, night, night + 3600, 0.0);
        set_dispatch_log_since(&db, car_now().timestamp() - 8 * 86_400);
        let payload = car_payload(&db, &five_kwp_settings(), &tonight);
        assert!(!payload
            .status
            .contains(&"dispatch_load_not_added".to_string()));
    }

    #[test]
    fn a_dispatch_log_younger_than_the_history_keeps_the_car_load_out() {
        // The log restarted two days ago (a gap in the checks), so the
        // nights before it may still hold the car.
        let db = test_db();
        seed_dark_forecast_state(&db, car_now());
        set_dispatch_log_since(&db, car_now().timestamp() - 2 * 86_400);
        let tonight = fresh(vec![car_dispatch(
            local_ts(15, 19),
            local_ts(15, 20),
            "SMART",
            Some(7.0),
        )]);
        let payload = car_payload(&db, &five_kwp_settings(), &tonight);
        assert!(payload
            .status
            .contains(&"dispatch_load_not_added".to_string()));
        assert_eq!(
            payload.ev_dispatch_hours.first().map(|h| h.ev_kwh),
            Some(0.0)
        );
    }

    #[test]
    fn a_dispatch_without_believable_energy_is_reported() {
        let db = test_db();
        let settings = seed_car_free_state(&db, car_now());
        let tonight = fresh(vec![car_dispatch(
            local_ts(15, 19),
            local_ts(15, 20),
            "SMART",
            None,
        )]);
        let payload = car_payload(&db, &settings, &tonight);
        assert!(payload
            .status
            .contains(&"dispatch_energy_unknown".to_string()));
        assert_eq!(
            payload.ev_dispatch_hours.first().map(|h| h.held_fraction),
            Some(1.0)
        );
    }

    #[test]
    fn tomorrows_car_energy_counts_only_tomorrows_hours() {
        let db = test_db();
        let settings = seed_car_free_state(&db, car_now());
        let both = fresh(vec![
            car_dispatch(local_ts(15, 22), local_ts(15, 23), "SMART", Some(5.0)),
            car_dispatch(local_ts(16, 1), local_ts(16, 3), "SMART", Some(7.0)),
        ]);
        let payload = car_payload(&db, &settings, &both);
        assert!((payload.ev_dispatch_tomorrow_kwh - 7.0).abs() < 1e-9);
    }
}
