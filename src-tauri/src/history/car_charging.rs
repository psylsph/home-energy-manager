//! Estimating the car's charging from home power (issue #454).
//!
//! In a house where nothing but the car pushes home load past a known level,
//! any minute above that level is the car charging, drawing a fixed block of
//! `charger amps × grid voltage`. This module is the single definition of
//! that rule: History, the Power page, Today, the forecast and the report
//! endpoints share its SQL, so they agree to the minute. The Telegram `/today`
//! summary and the daily report integrate the same per-row figure with their
//! own trapezoid and gap rule (`alerts::report`), so their kWh and cost can
//! differ slightly from the Power page's for the same day.

use rusqlite::{params, Connection};

use super::TimePoint;
use crate::settings::Settings;

/// Charger current used until the user sets one (a UK 7 kW charge point).
pub const DEFAULT_CHARGER_AMPS: u16 = 32;
/// Lowest and highest charger current a user can set.
pub const MIN_CHARGER_AMPS: u16 = 6;
pub const MAX_CHARGER_AMPS: u16 = 80;
/// Lowest and highest car charging threshold the API accepts, kW. A saved
/// value outside them (a hand-edited file) turns the feature off.
pub const MIN_THRESHOLD_KW: f64 = 1.0;
pub const MAX_THRESHOLD_KW: f64 = 25.0;
/// Voltage assumed when a minute has no usable grid voltage.
pub const FALLBACK_VOLTAGE_V: f64 = 230.0;
/// The sanitizer's grid voltage range; a reading outside it isn't trusted.
const VOLTAGE_RANGE_V: std::ops::RangeInclusive<f64> = 180.0..=280.0;

/// Derived History series (issue #454): the car's power and the home power
/// with the car taken out. Like the directional series they are averaged per
/// bucket after the per-minute rule, so a wide bucket never loses the car.
pub const CAR_POWER_FIELD: &str = "_car_power";
pub const HOME_EXCL_CAR_POWER_FIELD: &str = "_home_excl_car_power";
/// Derived cumulative energy series for the History energy chart, shaped like
/// the inverter's own `today_*` counters: kWh since local midnight, reset
/// each day. There is no car counter, so the car's is integrated from its
/// per-minute power; the house's is the home counter less the car's.
pub const CAR_ENERGY_FIELD: &str = "_car_energy_today";
pub const HOME_EXCL_CAR_ENERGY_FIELD: &str = "_home_excl_car_energy_today";

/// The rule's two inputs, taken once from the settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CarChargingConfig {
    /// Home power above this, in watts, is the car.
    pub threshold_w: f64,
    /// Charger current in amps.
    pub amps: f64,
}

impl CarChargingConfig {
    /// `None` when no threshold is set, which turns the feature off.
    pub fn from_settings(settings: &Settings) -> Option<Self> {
        let kw = settings.forecast_exclude_load_above_kw?;
        // The same range the API accepts; a hand-edited file outside it
        // turns the feature off rather than inventing a car.
        if !kw.is_finite() || !(MIN_THRESHOLD_KW..=MAX_THRESHOLD_KW).contains(&kw) {
            return None;
        }
        let amps = settings.car_charger_current_a;
        let amps = if (MIN_CHARGER_AMPS..=MAX_CHARGER_AMPS).contains(&amps) {
            amps
        } else {
            DEFAULT_CHARGER_AMPS
        };
        Some(Self {
            threshold_w: kw * 1000.0,
            amps: f64::from(amps),
        })
    }
}

/// The grid voltage to use for a minute.
pub fn effective_voltage(grid_v: Option<f64>) -> f64 {
    match grid_v {
        Some(v) if v.is_finite() && VOLTAGE_RANGE_V.contains(&v) => v,
        _ => FALLBACK_VOLTAGE_V,
    }
}

/// The car's power for a minute with home power `home_w` and grid voltage
/// `grid_v`. Zero at or below the threshold; above it, the charger's block
/// capped at the home power so the house never goes below zero.
pub fn car_power_w(home_w: f64, grid_v: Option<f64>, cfg: &CarChargingConfig) -> f64 {
    // `!(x > t)` also rejects NaN, so a corrupt reading is never a car.
    if !home_w.is_finite()
        || home_w.partial_cmp(&cfg.threshold_w) != Some(std::cmp::Ordering::Greater)
    {
        return 0.0;
    }
    (cfg.amps * effective_voltage(grid_v)).min(home_w)
}

/// SQL for one minute's car power, over `readings r` left-joined to
/// `evc_power_minutes e`: the charger's measured power where it recorded the
/// minute, else the block. `?3` is the threshold in watts and `?4` the amps.
/// It is the arithmetic of [`car_power_w`] (a test feeds both the same rows),
/// clamped to `[0, home_power]` so the house never goes negative. A NULL
/// `home_power` gives NULL, and the callers drop such rows.
const CAR_POWER_SQL: &str = "MAX(0, MIN(COALESCE(e.power_w, \
    CASE WHEN r.home_power > ?3 THEN ?4 * \
        (CASE WHEN r.grid_voltage BETWEEN 180 AND 280 THEN r.grid_voltage ELSE 230 END) \
    ELSE 0 END), r.home_power))";

/// The per-minute value expression for a derived car field, or `None` for any
/// other name. Both are constant strings, so no input reaches the SQL.
fn series_sql(field: &str) -> Option<String> {
    match field {
        CAR_POWER_FIELD => Some(CAR_POWER_SQL.to_string()),
        HOME_EXCL_CAR_POWER_FIELD => Some(format!("(r.home_power - {CAR_POWER_SQL})")),
        _ => None,
    }
}

/// The daily report's row query with the car's power as a ninth column
/// (issue #454): `?1`/`?2` the day, `?3`/`?4` the threshold and amps. A row
/// with no home power has no car figure.
pub(super) fn report_rows_sql() -> String {
    format!(
        "SELECT r.timestamp, r.solar_power, r.pv1_power, r.pv2_power, \
         r.battery_power, r.grid_power, r.home_power, r.soc, \
         CASE WHEN r.home_power IS NULL THEN NULL ELSE {CAR_POWER_SQL} END \
         FROM readings r \
         LEFT JOIN evc_power_minutes e ON e.timestamp = r.timestamp - (r.timestamp % 60) \
         WHERE r.timestamp >= ?1 AND r.timestamp < ?2 \
         ORDER BY r.timestamp"
    )
}

/// True for the derived car fields.
pub fn is_car_field(field: &str) -> bool {
    series_sql(field).is_some()
}

/// One derived car series over `[start_ts, end_ts)`, averaged per bucket.
/// `None` when `field` isn't a car field.
pub(super) fn query_series(
    conn: &Connection,
    field: &str,
    bucket_secs: i64,
    start_ts: i64,
    end_ts: i64,
    cfg: &CarChargingConfig,
) -> Option<Result<Vec<TimePoint>, String>> {
    let expr = series_sql(field)?;
    let sql = format!(
        "SELECT ((r.timestamp / {bucket_secs}) * {bucket_secs}) * 1000 AS t_bucket, \
                AVG({expr}) AS v \
         FROM readings r \
         LEFT JOIN evc_power_minutes e ON e.timestamp = r.timestamp - (r.timestamp % 60) \
         WHERE r.timestamp >= ?1 AND r.timestamp < ?2 AND r.home_power IS NOT NULL \
         GROUP BY t_bucket ORDER BY t_bucket"
    );
    let run = || -> rusqlite::Result<Vec<TimePoint>> {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![start_ts, end_ts, cfg.threshold_w, cfg.amps],
            |row| {
                Ok(TimePoint {
                    t: row.get(0)?,
                    v: row.get(1)?,
                })
            },
        )?;
        rows.collect()
    };
    Some(run().map_err(|e| format!("Query failed for {field}: {e}")))
}

/// True for the derived car energy fields.
pub fn is_car_energy_field(field: &str) -> bool {
    matches!(field, CAR_ENERGY_FIELD | HOME_EXCL_CAR_ENERGY_FIELD)
}

/// One minute-table reading with the car's power worked out for it.
struct CarRow {
    timestamp: i64,
    /// The car's power, W (`CAR_POWER_SQL`).
    car_w: f64,
    /// Grid power, W (positive when exporting); 0 when the reading has none.
    grid_w: f64,
    /// The inverter's cumulative home counter, kWh.
    home_counter_kwh: Option<f64>,
    /// The import rate recorded with the reading, £/kWh (issue #455); `None`
    /// for a row from before rates were recorded.
    import_rate: Option<f64>,
}

/// The seconds a reading earns over the previous one, or `None` when the
/// pair is a step back or a gap too long to bridge. The one rule every car
/// energy and cost figure integrates by, so they cannot drift apart.
fn credited_secs(prev_ts: i64, ts: i64) -> Option<i64> {
    let dt = ts - prev_ts;
    (dt > 0 && dt <= MAX_INTEGRATION_GAP_SECS).then_some(dt)
}

/// Every reading with a home power in `[from_ts, to_ts)`, oldest first, with
/// the car's power from the charger's own minute where HEM recorded one and
/// else the estimate. The one query behind the car's energy, cost and series.
fn car_rows(
    conn: &Connection,
    from_ts: i64,
    to_ts: i64,
    cfg: &CarChargingConfig,
) -> Result<Vec<CarRow>, String> {
    let sql = format!(
        "SELECT r.timestamp, {CAR_POWER_SQL}, COALESCE(r.grid_power, 0), \
                r.home_energy_today_kwh, r.import_rate \
         FROM readings r \
         LEFT JOIN evc_power_minutes e ON e.timestamp = r.timestamp - (r.timestamp % 60) \
         WHERE r.timestamp >= ?1 AND r.timestamp < ?2 AND r.home_power IS NOT NULL \
         ORDER BY r.timestamp"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare car energy query: {e}"))?;
    let rows = stmt
        .query_map(params![from_ts, to_ts, cfg.threshold_w, cfg.amps], |row| {
            Ok(CarRow {
                timestamp: row.get(0)?,
                car_w: row.get(1)?,
                grid_w: row.get(2)?,
                home_counter_kwh: row.get(3)?,
                import_rate: row
                    .get::<_, Option<f64>>(4)?
                    .filter(|r| r.is_finite() && *r >= 0.0),
            })
        })
        .map_err(|e| format!("Car energy query failed: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("Car energy query failed: {e}"))
}

/// A gap between two readings longer than this is not bridged when
/// integrating, matching `power_totals_in_window`.
const MAX_INTEGRATION_GAP_SECS: i64 = 300;

/// Per bucket: its start, the running car kWh at its last reading, and the
/// largest home counter value in it.
pub(super) type EnergyWalk = Vec<(i64, f64, Option<f64>)>;

/// The car's cumulative energy since local midnight (or the house's: the
/// home counter less the car), one point per bucket.
///
/// The walk starts at the local midnight of the window's first day, so a
/// window that opens mid-charge still reports the day's running total, as the
/// inverter's own `today_*` counters do. Each interval is credited with the
/// earlier reading's car power (see [`credited_secs`]); a step back in time
/// adds nothing; a new local day starts again from zero.
///
/// The one pass over the minute rows behind both car energy series. A caller
/// that wants the car's and the house's takes it once and cuts both from it
/// with [`energy_points`], instead of reading the same rows twice.
pub(super) fn energy_walk<TZ: chrono::TimeZone>(
    conn: &Connection,
    bucket_secs: i64,
    start_ts: i64,
    end_ts: i64,
    cfg: &CarChargingConfig,
    tz: &TZ,
) -> Result<EnergyWalk, String> {
    let walk_from = chrono::DateTime::from_timestamp(start_ts, 0)
        .and_then(|dt| {
            super::local_midnight_timestamp_in(dt.with_timezone(tz).date_naive(), tz).ok()
        })
        .map_or(start_ts, |midnight| midnight.min(start_ts));

    let mut buckets: EnergyWalk = Vec::new();
    let mut running_kwh = 0.0_f64;
    let mut prev: Option<(i64, f64)> = None;
    for row in car_rows(conn, walk_from, end_ts, cfg)? {
        let (ts, car_w, counter) = (row.timestamp, row.car_w, row.home_counter_kwh);
        if let Some((prev_ts, prev_car_w)) = prev {
            if !super::same_local_day(prev_ts, ts, tz) {
                running_kwh = 0.0;
            } else if let Some(dt) = credited_secs(prev_ts, ts) {
                running_kwh += prev_car_w * dt as f64 / 3_600_000.0;
            }
        }
        prev = Some((ts, car_w));
        if ts < start_ts {
            continue;
        }
        let bucket = ts.div_euclid(bucket_secs) * bucket_secs;
        match buckets.last_mut() {
            Some(last) if last.0 == bucket => {
                last.1 = running_kwh;
                last.2 = match (last.2, counter) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
            }
            _ => buckets.push((bucket, running_kwh, counter)),
        }
    }
    Ok(buckets)
}

/// One car energy series cut from a walk; `None` for any other field.
pub(super) fn energy_points(walk: &EnergyWalk, field: &str) -> Option<Vec<TimePoint>> {
    if !is_car_energy_field(field) {
        return None;
    }
    let house = field == HOME_EXCL_CAR_ENERGY_FIELD;
    Some(
        walk.iter()
            .filter_map(|&(bucket, car_kwh, counter)| {
                let v = if house {
                    (counter? - car_kwh).max(0.0)
                } else {
                    car_kwh
                };
                Some(TimePoint {
                    t: bucket * 1000,
                    v,
                })
            })
            .collect(),
    )
}

impl super::HistoryDb {
    /// The car's energy over a window, in kWh: each interval between two
    /// readings is credited with the earlier one's car power, a gap over five
    /// minutes or a step back adds nothing, and the window's edges are not
    /// padded, so the figure covers exactly the readings inside it.
    pub fn car_kwh_in_window(
        &self,
        window: &super::HistoryWindow,
        cfg: &CarChargingConfig,
    ) -> Result<f64, String> {
        let (start_ts, end_ts) = window.resolve()?;
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        let mut kwh = 0.0;
        let mut prev: Option<(i64, f64)> = None;
        for row in car_rows(&conn, start_ts, end_ts, cfg)? {
            if let Some((prev_ts, prev_w)) = prev {
                if let Some(dt) = credited_secs(prev_ts, row.timestamp) {
                    kwh += prev_w * dt as f64 / 3_600_000.0;
                }
            }
            prev = Some((row.timestamp, row.car_w));
        }
        Ok(kwh)
    }
}

/// One credited interval of the cost walk.
struct CostStep {
    /// The reading that ends the interval, Unix seconds.
    ends_at: i64,
    car_kwh: f64,
    grid_kwh: f64,
    cost_gbp: f64,
}

/// What the car's charging cost over a window.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CarCost {
    /// The car's energy, kWh (the same figure as [`HistoryDb::car_kwh_in_window`], from the same rows and gap rule).
    pub kwh: f64,
    /// The part of it that came from the grid, kWh.
    pub grid_kwh: f64,
    /// `grid_kwh` priced at the import rate when it was drawn, £.
    pub cost_gbp: f64,
}

impl super::HistoryDb {
    /// The car's charging cost over a window.
    ///
    /// Each interval between two readings takes the earlier reading's car
    /// power and grid import. The car is assumed to draw from the grid first,
    /// so the grid share of an interval is the smaller of the car's energy
    /// and the grid import; whatever solar or the battery supplied beyond
    /// that is free. The grid share is priced at the import rate for the
    /// local time of that reading, so a night-rate charge is priced at the
    /// night rate whatever the bucket size. Gaps over five minutes and steps
    /// back in time add nothing. The standing charge is not part of it.
    ///
    /// An interval uses the import rate recorded with its earlier reading
    /// (issue #455) when there is one, so editing the tariff later doesn't
    /// reprice past charges; `tariff` prices readings with none recorded.
    pub fn car_cost_in_window(
        &self,
        window: &super::HistoryWindow,
        cfg: &CarChargingConfig,
        tariff: &crate::settings::TariffConfig,
        flat_fallback: f64,
    ) -> Result<CarCost, String> {
        self.car_cost_in_window_in(window, cfg, tariff, flat_fallback, &chrono::Local)
    }

    pub(super) fn car_cost_in_window_in<TZ: chrono::TimeZone>(
        &self,
        window: &super::HistoryWindow,
        cfg: &CarChargingConfig,
        tariff: &crate::settings::TariffConfig,
        flat_fallback: f64,
        tz: &TZ,
    ) -> Result<CarCost, String> {
        let mut out = CarCost::default();
        for step in self.car_cost_steps(window, cfg, tariff, flat_fallback, tz)? {
            out.kwh += step.car_kwh;
            out.grid_kwh += step.grid_kwh;
            out.cost_gbp += step.cost_gbp;
        }
        Ok(out)
    }

    /// Every credited interval in the window, in time order, each stamped
    /// with the reading that ends it. The one walk behind the window total
    /// and the running series, so they cannot disagree.
    fn car_cost_steps<TZ: chrono::TimeZone>(
        &self,
        window: &super::HistoryWindow,
        cfg: &CarChargingConfig,
        tariff: &crate::settings::TariffConfig,
        flat_fallback: f64,
        tz: &TZ,
    ) -> Result<Vec<CostStep>, String> {
        let (start_ts, end_ts) = window.resolve()?;
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        let mut steps = Vec::new();
        let mut prev: Option<(i64, f64, f64, Option<f64>)> = None;
        for row in car_rows(&conn, start_ts, end_ts, cfg)? {
            let (ts, car_w, grid_w) = (row.timestamp, row.car_w, row.grid_w);
            if let Some((prev_ts, prev_car_w, prev_grid_w, prev_rate)) = prev {
                if let Some(dt) = credited_secs(prev_ts, ts) {
                    let hours = dt as f64 / 3600.0;
                    let car_kwh = prev_car_w * hours / 1000.0;
                    // Grid power is positive when exporting; only import counts.
                    let import_kwh = (-prev_grid_w).max(0.0) * hours / 1000.0;
                    let grid_kwh = car_kwh.min(import_kwh);
                    // The rate recorded with the earlier reading, else the
                    // current tariff's for its time.
                    let rate = prev_rate.unwrap_or_else(|| {
                        tariff
                            .rate_for_minutes(super::local_minutes_of_day(prev_ts, tz))
                            .unwrap_or(flat_fallback)
                    });
                    steps.push(CostStep {
                        ends_at: ts,
                        car_kwh,
                        grid_kwh,
                        cost_gbp: grid_kwh * rate,
                    });
                }
            }
            prev = Some((ts, car_w, grid_w, row.import_rate));
        }
        Ok(steps)
    }

    /// The car's running cost across the window, one point per bucket: the
    /// total so far at the bucket's last reading, so the last point equals
    /// [`Self::car_cost_in_window`] whatever the bucket size. Buckets without
    /// a reading are omitted, like the other cost series.
    pub fn query_car_cost_series(
        &self,
        window: &super::HistoryWindow,
        bucket_secs: i64,
        cfg: &CarChargingConfig,
        tariff: &crate::settings::TariffConfig,
        flat_fallback: f64,
    ) -> Result<Vec<TimePoint>, String> {
        self.query_car_cost_series_in(
            window,
            bucket_secs,
            cfg,
            tariff,
            flat_fallback,
            &chrono::Local,
        )
    }

    pub(super) fn query_car_cost_series_in<TZ: chrono::TimeZone>(
        &self,
        window: &super::HistoryWindow,
        bucket_secs: i64,
        cfg: &CarChargingConfig,
        tariff: &crate::settings::TariffConfig,
        flat_fallback: f64,
        tz: &TZ,
    ) -> Result<Vec<TimePoint>, String> {
        super::validate_bucket_secs(bucket_secs)?;
        let mut running = 0.0;
        let mut points: Vec<TimePoint> = Vec::new();
        for step in self.car_cost_steps(window, cfg, tariff, flat_fallback, tz)? {
            running += step.cost_gbp;
            let t = step.ends_at.div_euclid(bucket_secs) * bucket_secs * 1000;
            match points.last_mut() {
                Some(last) if last.t == t => last.v = running,
                _ => points.push(TimePoint { t, v: running }),
            }
        }
        Ok(points)
    }
}

impl super::HistoryDb {
    /// The car's energy since local midnight, up to `now_ts` (Unix seconds).
    /// The Status page's Today bars split it out of Home Use (issue #454).
    pub fn car_kwh_today(&self, cfg: &CarChargingConfig, now_ts: i64) -> Result<f64, String> {
        self.car_kwh_today_in(cfg, now_ts, &chrono::Local)
    }

    pub(super) fn car_kwh_today_in<TZ: chrono::TimeZone>(
        &self,
        cfg: &CarChargingConfig,
        now_ts: i64,
        tz: &TZ,
    ) -> Result<f64, String> {
        use chrono::Timelike;
        let now = chrono::DateTime::from_timestamp(now_ts, 0)
            .ok_or_else(|| format!("Invalid timestamp: {now_ts}"))?
            .with_timezone(tz);
        // A zone with no midnight that day (a DST jump at 00:00) falls back to
        // counting the seconds elapsed since the start of the local day.
        let midnight = super::local_midnight_timestamp_in(now.date_naive(), tz)
            .unwrap_or_else(|_| now_ts - i64::from(now.num_seconds_from_midnight()));
        // The window's end is exclusive; +1 takes in the reading at `now`.
        let window = super::HistoryWindow {
            range_secs: 0,
            offset: 0,
            explicit_window: Some((midnight, now_ts + 1)),
        };
        self.car_kwh_in_window(&window, cfg)
    }
}

impl super::HistoryDb {
    /// The estimated car, one entry per wall-clock minute since `since_ts`
    /// whose home power was over the threshold and which the GivEnergy charger
    /// did not record (a measured minute uses its own figure): the average of
    /// the minute's block, in watts, sorted by minute. For the forecast's
    /// consumption profile (issue #454), which takes the same rule as History
    /// and the reports.
    pub fn car_block_minutes_since(
        &self,
        cfg: &CarChargingConfig,
        since_ts: i64,
    ) -> Result<Vec<crate::forecast::consumption::EvPowerMinute>, String> {
        use crate::forecast::consumption::EvPowerMinute;
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        // Minutes the charger recorded are left out (`e.timestamp IS NULL`),
        // so `CAR_POWER_SQL`'s measured branch never applies and it is the
        // block alone. `?2` is the unbounded end.
        let sql = format!(
            "SELECT r.timestamp - (r.timestamp % 60) AS minute, AVG({CAR_POWER_SQL}) AS w \
             FROM readings r \
             LEFT JOIN evc_power_minutes e ON e.timestamp = r.timestamp - (r.timestamp % 60) \
             WHERE r.timestamp >= ?1 AND r.timestamp < ?2 \
               AND e.timestamp IS NULL AND r.home_power IS NOT NULL \
             GROUP BY minute HAVING w > 0 \
             ORDER BY minute"
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare car block query: {e}"))?;
        let rows = stmt
            .query_map(
                params![since_ts, i64::MAX, cfg.threshold_w, cfg.amps],
                |row| {
                    Ok(EvPowerMinute {
                        timestamp: row.get(0)?,
                        power_w: row.get(1)?,
                    })
                },
            )
            .map_err(|e| format!("Car block query failed: {e}"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| format!("Failed to read car block minutes: {e}"))
    }

    /// [`Self::car_block_minutes_since`] for the forecast, which carries on
    /// without the exclusion when the read fails. That leaves the car in the
    /// predicted house load, so the failure is logged rather than lost.
    pub fn car_block_minutes_or_warn(
        &self,
        cfg: &CarChargingConfig,
        since_ts: i64,
    ) -> Vec<crate::forecast::consumption::EvPowerMinute> {
        self.car_block_minutes_since(cfg, since_ts)
            .unwrap_or_else(|e| {
                tracing::warn!("Forecast: car charging not left out of the load profile: {e}");
                Vec::new()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{HistoryDb, TimePoint};
    use crate::inverter::model::InverterSnapshot;

    fn cfg() -> CarChargingConfig {
        CarChargingConfig {
            threshold_w: 3000.0,
            amps: 32.0,
        }
    }

    #[test]
    fn at_or_below_threshold_is_not_the_car() {
        assert_eq!(car_power_w(3000.0, Some(230.0), &cfg()), 0.0);
        assert_eq!(car_power_w(2999.0, Some(230.0), &cfg()), 0.0);
        assert_eq!(car_power_w(0.0, Some(230.0), &cfg()), 0.0);
    }

    #[test]
    fn above_threshold_is_amps_times_voltage() {
        let car = car_power_w(9000.0, Some(231.0), &cfg());
        assert!((car - 32.0 * 231.0).abs() < 1e-9);
    }

    #[test]
    fn car_is_capped_at_home_power() {
        // A slow start: the minute's mean is just over the threshold.
        let car = car_power_w(3100.0, Some(230.0), &cfg());
        assert_eq!(car, 3100.0);
    }

    #[test]
    fn sixteen_amps_gives_a_smaller_block() {
        let c = CarChargingConfig {
            threshold_w: 3000.0,
            amps: 16.0,
        };
        let car = car_power_w(9000.0, Some(230.0), &c);
        assert!((car - 16.0 * 230.0).abs() < 1e-9);
    }

    #[test]
    fn missing_or_out_of_range_voltage_falls_back() {
        assert_eq!(effective_voltage(None), 230.0);
        assert_eq!(effective_voltage(Some(0.0)), 230.0);
        assert_eq!(effective_voltage(Some(179.9)), 230.0);
        assert_eq!(effective_voltage(Some(280.1)), 230.0);
        assert_eq!(effective_voltage(Some(f64::NAN)), 230.0);
        assert_eq!(effective_voltage(Some(f64::INFINITY)), 230.0);
        assert_eq!(effective_voltage(Some(180.0)), 180.0);
        assert_eq!(effective_voltage(Some(280.0)), 280.0);
        assert_eq!(effective_voltage(Some(241.5)), 241.5);
        let car = car_power_w(9000.0, None, &cfg());
        assert!((car - 32.0 * 230.0).abs() < 1e-9);
    }

    #[test]
    fn non_finite_or_negative_home_power_is_not_the_car() {
        for h in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -500.0] {
            assert_eq!(car_power_w(h, Some(230.0), &cfg()), 0.0, "{h}");
        }
    }

    #[test]
    fn car_plus_house_is_always_home() {
        for h in [
            0.0, 1.0, 2999.0, 3000.0, 3001.0, 4000.0, 7360.0, 7361.0, 15000.0,
        ] {
            for v in [None, Some(210.0), Some(230.0), Some(250.0)] {
                let car = car_power_w(h, v, &cfg());
                assert!(car >= 0.0 && car <= h, "h={h} v={v:?} car={car}");
            }
        }
    }

    #[test]
    fn config_is_none_without_a_threshold() {
        let s = Settings::default();
        assert_eq!(s.forecast_exclude_load_above_kw, None);
        assert_eq!(CarChargingConfig::from_settings(&s), None);
    }

    #[test]
    fn config_reads_threshold_in_watts_and_amps() {
        let s = Settings {
            forecast_exclude_load_above_kw: Some(3.5),
            car_charger_current_a: 16,
            ..Settings::default()
        };
        assert_eq!(
            CarChargingConfig::from_settings(&s),
            Some(CarChargingConfig {
                threshold_w: 3500.0,
                amps: 16.0
            })
        );
    }

    #[test]
    fn config_ignores_an_invalid_threshold() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 0.99, 25.01] {
            let s = Settings {
                forecast_exclude_load_above_kw: Some(bad),
                ..Settings::default()
            };
            assert_eq!(CarChargingConfig::from_settings(&s), None, "{bad}");
        }
    }

    #[test]
    fn config_falls_back_to_default_amps_when_the_file_is_out_of_range() {
        for bad in [0u16, 5, 81, u16::MAX] {
            let s = Settings {
                forecast_exclude_load_above_kw: Some(3.0),
                car_charger_current_a: bad,
                ..Settings::default()
            };
            let c = CarChargingConfig::from_settings(&s).unwrap();
            assert_eq!(c.amps, f64::from(DEFAULT_CHARGER_AMPS), "{bad}");
        }
    }

    #[test]
    fn default_amps_is_32_and_a_settings_file_without_the_key_loads() {
        assert_eq!(Settings::default().car_charger_current_a, 32);
        let mut file = serde_json::to_value(Settings::default()).unwrap();
        file.as_object_mut()
            .unwrap()
            .remove("car_charger_current_a");
        let s: Settings = serde_json::from_value(file).unwrap();
        assert_eq!(s.car_charger_current_a, 32);
    }

    // ---- the History series (issue #454 phase 2) ----

    const T0: i64 = 1_790_000_040; // a minute boundary

    fn db() -> HistoryDb {
        HistoryDb::open(&crate::test_util::make_unique_test_dir("car-charging").join("h.db"))
            .unwrap()
    }

    fn put(db: &HistoryDb, minute: i64, home_w: i32, grid_v: f32) {
        let snap = InverterSnapshot {
            timestamp: T0 + minute * 60,
            home_power: home_w,
            grid_voltage: grid_v,
            ..InverterSnapshot::default()
        };
        db.insert_reading(&snap);
    }

    fn put_evc(db: &HistoryDb, minute: i64, power_w: f64) {
        db.record_evc_power_minute(&crate::history::evc_power::EvcMinuteRow {
            timestamp: T0 + minute * 60,
            power_w,
            samples: 6,
        })
        .unwrap();
    }

    fn series(
        db: &HistoryDb,
        bucket: i64,
        fields: &[&str],
        car: Option<&CarChargingConfig>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let fields: Vec<String> = fields.iter().map(|f| f.to_string()).collect();
        db.query_history_with_car(0, bucket, 0, &fields, Some((T0, T0 + 24 * 3600)), car)
            .unwrap()
    }

    fn points(map: &serde_json::Map<String, serde_json::Value>, field: &str) -> Vec<TimePoint> {
        serde_json::from_value(map[field].clone()).unwrap()
    }

    #[test]
    fn sql_matches_the_rust_rule_for_every_minute() {
        let db = db();
        // (home W, grid V, EVC W)
        let rows: [(i32, f32, Option<f64>); 9] = [
            (800, 231.0, None),
            (3000, 231.0, None),
            (3001, 231.0, None),
            (9500, 241.5, None),
            (9500, 0.0, None),
            (9500, 300.0, None),
            (9500, 100.0, None),
            (9500, 231.0, Some(7000.0)),
            (1500, 231.0, Some(0.0)),
        ];
        for (i, (h, v, evc)) in rows.iter().enumerate() {
            put(&db, i as i64, *h, *v);
            if let Some(e) = evc {
                put_evc(&db, i as i64, *e);
            }
        }
        let c = cfg();
        let map = series(
            &db,
            60,
            &[CAR_POWER_FIELD, HOME_EXCL_CAR_POWER_FIELD],
            Some(&c),
        );
        let car = points(&map, CAR_POWER_FIELD);
        let house = points(&map, HOME_EXCL_CAR_POWER_FIELD);
        assert_eq!(car.len(), rows.len());
        for (i, (h, v, evc)) in rows.iter().enumerate() {
            let want = match evc {
                Some(e) => e.min(f64::from(*h)),
                None => car_power_w(f64::from(*h), Some(f64::from(*v)), &c),
            };
            assert_eq!(car[i].t, (T0 + i as i64 * 60) * 1000);
            assert!(
                (car[i].v - want).abs() < 1e-3,
                "minute {i}: {} vs {want}",
                car[i].v
            );
            assert!(
                (house[i].v - (f64::from(*h) - want)).abs() < 1e-3,
                "minute {i}"
            );
        }
    }

    #[test]
    fn car_and_house_add_up_to_home_in_wide_buckets() {
        let db = db();
        for m in 0..60 {
            put(
                &db,
                m,
                if m % 3 == 0 { 9000 } else { 700 + m as i32 },
                232.0,
            );
        }
        let c = cfg();
        let map = series(
            &db,
            3600,
            &["home_power", CAR_POWER_FIELD, HOME_EXCL_CAR_POWER_FIELD],
            Some(&c),
        );
        let home = points(&map, "home_power");
        let car = points(&map, CAR_POWER_FIELD);
        let house = points(&map, HOME_EXCL_CAR_POWER_FIELD);
        assert_eq!(
            (home.len(), car.len(), house.len()),
            (home.len(), home.len(), home.len())
        );
        for i in 0..home.len() {
            assert!((car[i].v + house[i].v - home[i].v).abs() < 1e-6);
            assert!(car[i].v >= 0.0 && house[i].v >= 0.0);
        }
        assert!(car.iter().any(|p| p.v > 0.0));
    }

    #[test]
    fn measured_charger_power_is_capped_at_home_power() {
        let db = db();
        put(&db, 0, 2000, 231.0);
        put_evc(&db, 0, 7000.0);
        let c = cfg();
        let map = series(
            &db,
            60,
            &[CAR_POWER_FIELD, HOME_EXCL_CAR_POWER_FIELD],
            Some(&c),
        );
        assert_eq!(points(&map, CAR_POWER_FIELD)[0].v, 2000.0);
        assert_eq!(points(&map, HOME_EXCL_CAR_POWER_FIELD)[0].v, 0.0);
    }

    #[test]
    fn without_a_config_the_car_series_are_absent() {
        let db = db();
        put(&db, 0, 9000, 231.0);
        let map = series(
            &db,
            60,
            &["home_power", CAR_POWER_FIELD, HOME_EXCL_CAR_POWER_FIELD],
            None,
        );
        assert!(map.contains_key("home_power"));
        assert!(!map.contains_key(CAR_POWER_FIELD));
        assert!(!map.contains_key(HOME_EXCL_CAR_POWER_FIELD));
    }

    #[test]
    fn a_minute_without_home_power_is_unknown_not_zero() {
        let db = db();
        put(&db, 0, 9000, 231.0);
        put(&db, 1, 9000, 231.0);
        db.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE readings SET home_power = NULL WHERE timestamp = ?1",
                [T0 + 60],
            )
            .unwrap();
        let c = cfg();
        let map = series(
            &db,
            60,
            &[CAR_POWER_FIELD, HOME_EXCL_CAR_POWER_FIELD],
            Some(&c),
        );
        assert_eq!(points(&map, CAR_POWER_FIELD).len(), 1);
        assert_eq!(points(&map, HOME_EXCL_CAR_POWER_FIELD).len(), 1);
    }

    #[test]
    fn reading_the_series_never_changes_history() {
        let db = db();
        for m in 0..5 {
            put(&db, m, 9000, 231.0);
        }
        let dump = |db: &HistoryDb| -> Vec<(i64, i64)> {
            let conn = db.conn.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT timestamp, home_power FROM readings ORDER BY timestamp")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        let before = dump(&db);
        let c = cfg();
        series(
            &db,
            60,
            &[CAR_POWER_FIELD, HOME_EXCL_CAR_POWER_FIELD],
            Some(&c),
        );
        assert_eq!(dump(&db), before);
    }

    #[test]
    fn a_changed_threshold_rederives_the_past() {
        let db = db();
        put(&db, 0, 4000, 231.0);
        let low = CarChargingConfig {
            threshold_w: 3000.0,
            amps: 32.0,
        };
        let high = CarChargingConfig {
            threshold_w: 5000.0,
            amps: 32.0,
        };
        let car = |c: &CarChargingConfig| {
            points(
                &series(&db, 60, &[CAR_POWER_FIELD], Some(c)),
                CAR_POWER_FIELD,
            )[0]
            .v
        };
        assert_eq!(car(&low), 4000.0);
        assert_eq!(car(&high), 0.0);
    }

    // ---- the cumulative energy series ----

    const MIDNIGHT: i64 = 1_789_948_800; // 2026-09-21 00:00 UTC

    fn utc() -> chrono::Utc {
        chrono::Utc
    }

    fn put_at(db: &HistoryDb, ts: i64, home_w: i32, counter_kwh: f32) {
        db.insert_reading(&InverterSnapshot {
            timestamp: ts,
            home_power: home_w,
            grid_voltage: 230.0,
            home_energy_today_kwh: counter_kwh,
            ..InverterSnapshot::default()
        });
    }

    /// One car energy series cut from a walk; `None` for any other field.
    fn query_energy_series<TZ: chrono::TimeZone>(
        conn: &Connection,
        field: &str,
        bucket_secs: i64,
        start_ts: i64,
        end_ts: i64,
        cfg: &CarChargingConfig,
        tz: &TZ,
    ) -> Option<Result<Vec<TimePoint>, String>> {
        if !is_car_energy_field(field) {
            return None;
        }
        Some(
            energy_walk(conn, bucket_secs, start_ts, end_ts, cfg, tz)
                .map(|walk| energy_points(&walk, field).unwrap_or_default()),
        )
    }

    fn energy(db: &HistoryDb, field: &str, bucket: i64, start: i64, end: i64) -> Vec<TimePoint> {
        let conn = db.conn.lock().unwrap();
        query_energy_series(&conn, field, bucket, start, end, &cfg(), &utc())
            .expect("a car energy field")
            .unwrap()
    }

    #[test]
    fn car_energy_integrates_the_block_over_the_minutes() {
        let db = db();
        // 61 readings, 60 intervals of a 32 A x 230 V block.
        for m in 0..=60 {
            put_at(&db, MIDNIGHT + 36_000 + m * 60, 9000, 0.0);
        }
        let pts = energy(&db, CAR_ENERGY_FIELD, 3600, MIDNIGHT, MIDNIGHT + 86_400);
        let last = pts.last().unwrap();
        assert!((last.v - 7.36).abs() < 1e-6, "{}", last.v);
        assert_eq!(last.t, (MIDNIGHT + 39_600) * 1000);
        assert!((pts[0].v - 7.36 * 59.0 / 60.0).abs() < 1e-6);
    }

    #[test]
    fn a_window_starting_mid_day_still_counts_from_midnight() {
        let db = db();
        for m in 0..=60 {
            put_at(&db, MIDNIGHT + 36_000 + m * 60, 9000, 0.0);
        }
        // The window opens after the charge has run for 30 minutes.
        let start = MIDNIGHT + 36_000 + 30 * 60;
        let pts = energy(&db, CAR_ENERGY_FIELD, 60, start, MIDNIGHT + 86_400);
        assert_eq!(pts[0].t, start * 1000);
        assert!((pts[0].v - 7.36 * 30.0 / 60.0).abs() < 1e-6, "{}", pts[0].v);
    }

    #[test]
    fn car_energy_resets_at_local_midnight() {
        let db = db();
        for m in 0..=20 {
            put_at(&db, MIDNIGHT + 86_400 - 600 + m * 60, 9000, 0.0);
        }
        let pts = energy(&db, CAR_ENERGY_FIELD, 60, MIDNIGHT, MIDNIGHT + 2 * 86_400);
        let before = pts
            .iter()
            .rev()
            .find(|p| p.t < (MIDNIGHT + 86_400) * 1000)
            .unwrap();
        let after = pts.last().unwrap();
        assert!(before.v > 0.9, "{}", before.v);
        // Without the reset the 20 minutes would total 2.45 kWh.
        assert!(after.v < 1.5, "after midnight restarts from 0: {}", after.v);
        assert!((after.v - 7.36 * 10.0 / 60.0).abs() < 0.13, "{}", after.v);
    }

    #[test]
    fn a_long_gap_is_not_bridged() {
        let db = db();
        put_at(&db, MIDNIGHT + 36_000, 9000, 0.0);
        put_at(&db, MIDNIGHT + 36_000 + 3600, 9000, 0.0);
        put_at(&db, MIDNIGHT + 36_000 + 3660, 9000, 0.0);
        let pts = energy(&db, CAR_ENERGY_FIELD, 3600, MIDNIGHT, MIDNIGHT + 86_400);
        // Only the last minute counts.
        assert!((pts.last().unwrap().v - 7.36 / 60.0).abs() < 1e-6);
    }

    #[test]
    fn house_energy_is_the_counter_less_the_car_and_never_negative() {
        let db = db();
        for m in 0..=60 {
            // The counter climbs 0.15 kWh a minute: 9 kWh over the hour.
            put_at(&db, MIDNIGHT + 36_000 + m * 60, 9000, 0.15 * m as f32);
        }
        let car = energy(&db, CAR_ENERGY_FIELD, 3600, MIDNIGHT, MIDNIGHT + 86_400);
        let house = energy(
            &db,
            HOME_EXCL_CAR_ENERGY_FIELD,
            3600,
            MIDNIGHT,
            MIDNIGHT + 86_400,
        );
        assert_eq!(car.len(), house.len());
        assert!((house[1].v - (9.0 - 7.36)).abs() < 1e-3, "{}", house[1].v);

        // A counter that lags the car floors at zero.
        let db2 = self::db();
        for m in 0..=60 {
            put_at(&db2, MIDNIGHT + 36_000 + m * 60, 9000, 0.01 * m as f32);
        }
        let house = energy(
            &db2,
            HOME_EXCL_CAR_ENERGY_FIELD,
            3600,
            MIDNIGHT,
            MIDNIGHT + 86_400,
        );
        assert!(house.iter().all(|p| p.v == 0.0));
    }

    #[test]
    fn non_energy_fields_are_not_claimed() {
        let db = db();
        let conn = db.conn.lock().unwrap();
        assert!(query_energy_series(&conn, "home_power", 60, 0, 1, &cfg(), &utc()).is_none());
        assert!(!is_car_energy_field(CAR_POWER_FIELD));
    }

    #[test]
    fn car_kwh_in_window_covers_only_readings_inside_it() {
        let db = db();
        for m in 0..=60 {
            put_at(&db, MIDNIGHT + 36_000 + m * 60, 9000, 0.0);
        }
        let all = crate::history::HistoryWindow {
            range_secs: 0,
            offset: 0,
            explicit_window: Some((MIDNIGHT, MIDNIGHT + 86_400)),
        };
        assert!((db.car_kwh_in_window(&all, &cfg()).unwrap() - 7.36).abs() < 1e-6);

        // Half an hour of it.
        let half = crate::history::HistoryWindow {
            explicit_window: Some((MIDNIGHT + 36_000, MIDNIGHT + 36_000 + 30 * 60)),
            ..all
        };
        assert!((db.car_kwh_in_window(&half, &cfg()).unwrap() - 7.36 * 29.0 / 60.0).abs() < 1e-6);
    }

    #[test]
    fn car_kwh_in_window_skips_gaps_and_is_zero_with_no_car() {
        let db = db();
        put_at(&db, MIDNIGHT + 36_000, 9000, 0.0);
        put_at(&db, MIDNIGHT + 40_000, 9000, 0.0);
        put_at(&db, MIDNIGHT + 40_060, 800, 0.0);
        let w = crate::history::HistoryWindow {
            range_secs: 0,
            offset: 0,
            explicit_window: Some((MIDNIGHT, MIDNIGHT + 86_400)),
        };
        // The gap adds nothing; the last minute starts at 9 kW (the car).
        assert!((db.car_kwh_in_window(&w, &cfg()).unwrap() - 7.36 / 60.0).abs() < 1e-6);
        let empty = crate::history::HistoryWindow {
            explicit_window: Some((MIDNIGHT + 80_000, MIDNIGHT + 81_000)),
            ..w
        };
        assert_eq!(db.car_kwh_in_window(&empty, &cfg()).unwrap(), 0.0);
    }

    /// The same cases the TypeScript mirror (`src/lib/carCharging.ts`) is
    /// tested against, so the two implementations cannot drift.
    #[test]
    fn matches_the_shared_fixture() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/car-charging-rule.json"
        ))
        .unwrap();
        let cases = fixture["cases"].as_array().unwrap();
        assert!(cases.len() >= 10);
        for case in cases {
            let cfg = CarChargingConfig {
                threshold_w: case["threshold_w"].as_f64().unwrap(),
                amps: case["amps"].as_f64().unwrap(),
            };
            let got = car_power_w(
                case["home_w"].as_f64().unwrap(),
                case["grid_v"].as_f64(),
                &cfg,
            );
            let want = case["expected_w"].as_f64().unwrap();
            assert!(
                (got - want).abs() < 1e-9,
                "{}: {got} vs {want}",
                case["name"]
            );
        }
    }

    // ---- the car's cost ----

    fn put_grid(db: &HistoryDb, ts: i64, home_w: i32, grid_w: i32) {
        db.insert_reading(&InverterSnapshot {
            timestamp: ts,
            home_power: home_w,
            grid_power: grid_w,
            grid_voltage: 230.0,
            ..InverterSnapshot::default()
        });
    }

    fn day() -> crate::history::HistoryWindow {
        crate::history::HistoryWindow {
            range_secs: 0,
            offset: 0,
            explicit_window: Some((MIDNIGHT, MIDNIGHT + 86_400)),
        }
    }

    fn flat(rate: f64) -> crate::settings::TariffConfig {
        crate::settings::TariffConfig::flat(rate)
    }

    fn cost(db: &HistoryDb, tariff: &crate::settings::TariffConfig) -> CarCost {
        db.car_cost_in_window_in(&day(), &cfg(), tariff, 0.30, &utc())
            .unwrap()
    }

    #[test]
    fn a_charge_wholly_from_the_grid_costs_its_energy_at_the_rate() {
        let db = db();
        for m in 0..=60 {
            put_grid(&db, MIDNIGHT + 3600 + m * 60, 9000, -9000);
        }
        let c = cost(&db, &flat(0.075));
        assert!((c.kwh - 7.36).abs() < 1e-6, "{c:?}");
        assert!((c.grid_kwh - 7.36).abs() < 1e-6);
        assert!((c.cost_gbp - 7.36 * 0.075).abs() < 1e-6);
    }

    fn stamp_import_rate(db: &HistoryDb, from_ts: i64, to_ts: i64, rate: f64) {
        db.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE readings SET import_rate = ?1 WHERE timestamp >= ?2 AND timestamp <= ?3",
                params![rate, from_ts, to_ts],
            )
            .unwrap();
    }

    #[test]
    fn a_charge_is_priced_at_the_rate_recorded_when_it_ran() {
        // The tariff is 0.30 now, but the charge ran at the 0.075 recorded
        // on its rows.
        let db = db();
        for m in 0..=60 {
            put_grid(&db, MIDNIGHT + 3600 + m * 60, 9000, -9000);
        }
        stamp_import_rate(&db, MIDNIGHT, MIDNIGHT + 86_400, 0.075);
        let c = cost(&db, &flat(0.30));
        assert!((c.cost_gbp - 7.36 * 0.075).abs() < 1e-6, "{c:?}");
    }

    #[test]
    fn rows_without_a_recorded_rate_follow_the_current_tariff() {
        // The first half hour was recorded at 0.10; the rest has no stamp.
        let db = db();
        for m in 0..=60 {
            put_grid(&db, MIDNIGHT + 3600 + m * 60, 9000, -9000);
        }
        stamp_import_rate(&db, MIDNIGHT, MIDNIGHT + 3600 + 29 * 60, 0.10);
        let c = cost(&db, &flat(0.30));
        let per_minute_kwh = 7.36 / 60.0;
        let expected = 30.0 * per_minute_kwh * 0.10 + 30.0 * per_minute_kwh * 0.30;
        assert!((c.cost_gbp - expected).abs() < 1e-6, "{c:?}");
    }

    #[test]
    fn a_recorded_zero_rate_makes_the_charge_free() {
        let db = db();
        for m in 0..=60 {
            put_grid(&db, MIDNIGHT + 3600 + m * 60, 9000, -9000);
        }
        stamp_import_rate(&db, MIDNIGHT, MIDNIGHT + 86_400, 0.0);
        assert_eq!(cost(&db, &flat(0.30)).cost_gbp, 0.0);
    }

    #[test]
    fn the_car_series_ends_at_the_window_total_with_recorded_rates() {
        let db = db();
        for m in 0..=60 {
            put_grid(&db, MIDNIGHT + 3600 + m * 60, 9000, -9000);
        }
        stamp_import_rate(&db, MIDNIGHT, MIDNIGHT + 3600 + 29 * 60, 0.10);
        let tariff = flat(0.30);
        let total = cost(&db, &tariff).cost_gbp;
        let series = db
            .query_car_cost_series_in(&day(), 3600, &cfg(), &tariff, 0.30, &utc())
            .unwrap();
        assert!((series.last().unwrap().v - total).abs() < 1e-9);
    }

    #[test]
    fn energy_from_solar_or_the_battery_is_free() {
        let db = db();
        // The car runs entirely from the battery: no grid import.
        for m in 0..=60 {
            put_grid(&db, MIDNIGHT + 3600 + m * 60, 9000, 0);
        }
        let c = cost(&db, &flat(0.30));
        assert!((c.kwh - 7.36).abs() < 1e-6);
        assert_eq!(c.grid_kwh, 0.0);
        assert_eq!(c.cost_gbp, 0.0);
    }

    #[test]
    fn only_the_grid_share_of_the_car_is_priced() {
        let db = db();
        // 3 kW import against a 7.36 kW car: 3 kWh an hour from the grid.
        for m in 0..=60 {
            put_grid(&db, MIDNIGHT + 3600 + m * 60, 9000, -3000);
        }
        let c = cost(&db, &flat(0.20));
        assert!((c.grid_kwh - 3.0).abs() < 1e-6, "{c:?}");
        assert!((c.cost_gbp - 0.60).abs() < 1e-6);
    }

    #[test]
    fn grid_import_beyond_the_car_is_the_houses() {
        let db = db();
        // 9 kW import, 7.36 kW car: the car's share is capped at its own energy.
        for m in 0..=60 {
            put_grid(&db, MIDNIGHT + 3600 + m * 60, 9000, -9000);
        }
        assert!((cost(&db, &flat(0.10)).grid_kwh - 7.36).abs() < 1e-6);
    }

    #[test]
    fn exporting_never_makes_the_car_cost_money_or_credit() {
        let db = db();
        for m in 0..=60 {
            put_grid(&db, MIDNIGHT + 3600 + m * 60, 9000, 2000);
        }
        let c = cost(&db, &flat(0.30));
        assert_eq!(c.grid_kwh, 0.0);
        assert_eq!(c.cost_gbp, 0.0);
    }

    #[test]
    fn each_interval_is_priced_at_the_rate_of_its_time() {
        let db = db();
        let tou = crate::settings::TariffConfig {
            slots: vec![
                crate::settings::TariffSlot {
                    start: "00:00".into(),
                    end: "01:00".into(),
                    rate: 0.10,
                },
                crate::settings::TariffSlot {
                    start: "01:00".into(),
                    end: "23:59".into(),
                    rate: 0.30,
                },
            ],
        };
        // 00:30 to 01:30, half in each rate.
        for m in 0..=60 {
            put_grid(&db, MIDNIGHT + 1800 + m * 60, 9000, -9000);
        }
        let c = cost(&db, &tou);
        let expected = 3.68 * 0.10 + 3.68 * 0.30;
        assert!((c.cost_gbp - expected).abs() < 1e-6, "{c:?}");
    }

    #[test]
    fn gaps_and_the_window_edge_add_nothing() {
        let db = db();
        put_grid(&db, MIDNIGHT + 3600, 9000, -9000);
        put_grid(&db, MIDNIGHT + 3600 + 3000, 9000, -9000); // 50 minutes later
        let c = cost(&db, &flat(0.10));
        assert_eq!(c.kwh, 0.0);
        assert_eq!(c.cost_gbp, 0.0);
    }

    #[test]
    fn no_car_means_no_cost() {
        let db = db();
        for m in 0..=60 {
            put_grid(&db, MIDNIGHT + 3600 + m * 60, 800, -800);
        }
        assert_eq!(cost(&db, &flat(0.30)), CarCost::default());
    }

    #[test]
    fn a_polled_chargers_measured_power_sets_the_car_energy() {
        let db = db();
        for m in 0..=10 {
            put_grid(&db, MIDNIGHT + 3600 + m * 60, 9000, -9000);
            put_evc(&db, (MIDNIGHT + 3600 - T0) / 60 + m, 3000.0);
        }
        let c = cost(&db, &flat(0.10));
        // 3 kW for 10 minutes = 0.5 kWh, not the 7.36 kW block.
        assert!((c.kwh - 0.5).abs() < 1e-6, "{c:?}");
    }

    // ---- the cost series ----

    fn mixed_day(db: &HistoryDb) {
        // Two charges at different import levels, one across a rate change.
        for m in 0..=90 {
            put_grid(db, MIDNIGHT + 1800 + m * 60, 9000, -9000);
        }
        for m in 0..=30 {
            put_grid(db, MIDNIGHT + 20_000 + m * 60, 9000, -4000);
        }
        for m in 0..=20 {
            put_grid(db, MIDNIGHT + 40_000 + m * 60, 800, -800);
        }
    }

    fn tou() -> crate::settings::TariffConfig {
        crate::settings::TariffConfig {
            slots: vec![
                crate::settings::TariffSlot {
                    start: "00:00".into(),
                    end: "01:00".into(),
                    rate: 0.10,
                },
                crate::settings::TariffSlot {
                    start: "01:00".into(),
                    end: "23:59".into(),
                    rate: 0.30,
                },
            ],
        }
    }

    fn series_for(db: &HistoryDb, bucket: i64) -> Vec<TimePoint> {
        db.query_car_cost_series_in(&day(), bucket, &cfg(), &tou(), 0.30, &utc())
            .unwrap()
    }

    #[test]
    fn the_last_cost_point_is_the_window_total_for_any_bucket_size() {
        let db = db();
        mixed_day(&db);
        let total = db
            .car_cost_in_window_in(&day(), &cfg(), &tou(), 0.30, &utc())
            .unwrap()
            .cost_gbp;
        assert!(total > 1.0, "the fixture should charge for real: {total}");
        for bucket in [60, 300, 900, 3600, 86_400] {
            let pts = series_for(&db, bucket);
            assert!(
                (pts.last().unwrap().v - total).abs() < 1e-9,
                "bucket {bucket}: {pts:?}"
            );
        }
    }

    #[test]
    fn the_cost_series_never_falls_and_starts_at_zero_before_any_car() {
        let db = db();
        mixed_day(&db);
        let pts = series_for(&db, 300);
        assert!(pts.windows(2).all(|w| w[1].v >= w[0].v - 1e-12));
        assert!(pts.iter().all(|p| p.v >= 0.0));
        assert!(pts.last().unwrap().v > pts.first().unwrap().v);
    }

    #[test]
    fn the_cost_series_has_one_point_per_bucket_with_readings() {
        let db = db();
        mixed_day(&db);
        let pts = series_for(&db, 3600);
        let times: Vec<i64> = pts.iter().map(|p| p.t).collect();
        let mut sorted = times.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(times, sorted);
        assert!(!times.is_empty());
        assert!(times.iter().all(|t| t % 3_600_000 == 0));
    }

    #[test]
    fn a_window_without_a_car_has_a_flat_zero_cost_series() {
        let db = db();
        for m in 0..=60 {
            put_grid(&db, MIDNIGHT + 3600 + m * 60, 800, -800);
        }
        let pts = series_for(&db, 600);
        assert!(!pts.is_empty());
        assert!(pts.iter().all(|p| p.v == 0.0));
    }

    #[test]
    fn the_cost_series_rejects_a_bad_bucket_size() {
        let db = db();
        assert!(db
            .query_car_cost_series_in(&day(), 0, &cfg(), &tou(), 0.30, &utc())
            .is_err());
    }

    // ---- today ----

    #[test]
    fn today_counts_from_local_midnight_to_now_only() {
        let db = db();
        // Yesterday evening: a charge that must not count.
        for m in 0..=30 {
            put_at(&db, MIDNIGHT - 3600 + m * 60, 9000, 0.0);
        }
        // This morning 02:00-03:00 (UTC midnight), one hour of the block.
        for m in 0..=60 {
            put_at(&db, MIDNIGHT + 7200 + m * 60, 9000, 0.0);
        }
        // Later today, after "now": must not count.
        for m in 0..=30 {
            put_at(&db, MIDNIGHT + 50_000 + m * 60, 9000, 0.0);
        }
        let now = MIDNIGHT + 7200 + 30 * 60; // 02:30
        let kwh = db.car_kwh_today_in(&cfg(), now, &utc()).unwrap();
        assert!((kwh - 7.36 * 30.0 / 60.0).abs() < 1e-6, "{kwh}");
    }

    #[test]
    fn today_includes_the_reading_at_now() {
        let db = db();
        for m in 0..=10 {
            put_at(&db, MIDNIGHT + 3600 + m * 60, 9000, 0.0);
        }
        let kwh = db
            .car_kwh_today_in(&cfg(), MIDNIGHT + 3600 + 600, &utc())
            .unwrap();
        assert!((kwh - 7.36 * 10.0 / 60.0).abs() < 1e-6, "{kwh}");
    }

    #[test]
    fn today_is_zero_with_no_readings_or_just_after_midnight() {
        let db = db();
        assert_eq!(
            db.car_kwh_today_in(&cfg(), MIDNIGHT + 600, &utc()).unwrap(),
            0.0
        );
        assert_eq!(db.car_kwh_today_in(&cfg(), MIDNIGHT, &utc()).unwrap(), 0.0);
    }

    #[test]
    fn today_follows_the_timezone_midnight_not_utc() {
        let db = db();
        // 23:30 UTC the day before is 00:30 the next day at UTC+1: it counts there.
        let plus_one = chrono::FixedOffset::east_opt(3600).unwrap();
        for m in 0..=30 {
            put_at(&db, MIDNIGHT - 1800 + m * 60, 9000, 0.0);
        }
        let now = MIDNIGHT + 1800; // 01:30 UTC, 02:30 at UTC+1
        let kwh_local = db.car_kwh_today_in(&cfg(), now, &plus_one).unwrap();
        let kwh_utc = db.car_kwh_today_in(&cfg(), now, &utc()).unwrap();
        assert!((kwh_local - 7.36 * 30.0 / 60.0).abs() < 1e-6, "{kwh_local}");
        assert_eq!(kwh_utc, 0.0);
    }

    // ---- block minutes for the forecast ----

    fn kwh_of(minutes: &[crate::forecast::consumption::EvPowerMinute]) -> f64 {
        minutes.iter().map(|m| m.power_w * 60.0 / 3_600_000.0).sum()
    }

    #[test]
    fn block_minutes_are_the_minutes_over_the_threshold() {
        let db = db();
        put_at(&db, MIDNIGHT + 60, 800, 0.0);
        put_at(&db, MIDNIGHT + 120, 9000, 0.0);
        put_at(&db, MIDNIGHT + 180, 3000, 0.0); // at the threshold: not the car
        put_at(&db, MIDNIGHT + 240, 9000, 0.0);
        let m = db.car_block_minutes_since(&cfg(), 0).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].timestamp, MIDNIGHT + 120);
        assert_eq!(m[1].timestamp, MIDNIGHT + 240);
        assert!(m.iter().all(|x| (x.power_w - 7360.0).abs() < 1e-6));
    }

    #[test]
    fn a_minute_the_charger_recorded_uses_its_own_figure_so_has_no_block() {
        let db = db();
        put_at(&db, MIDNIGHT + 60, 9000, 0.0);
        put_at(&db, MIDNIGHT + 120, 9000, 0.0);
        put_evc(&db, (MIDNIGHT + 60 - T0) / 60, 0.0); // polled and idle
        let m = db.car_block_minutes_since(&cfg(), 0).unwrap();
        assert_eq!(
            m.iter().map(|x| x.timestamp).collect::<Vec<_>>(),
            vec![MIDNIGHT + 120]
        );
    }

    #[test]
    fn several_rows_in_a_minute_average_and_cover_the_minute_once() {
        let db = db();
        // 15-second rows: three over the threshold, one under.
        put_at(&db, MIDNIGHT + 600, 9000, 0.0);
        put_at(&db, MIDNIGHT + 615, 9000, 0.0);
        put_at(&db, MIDNIGHT + 630, 9000, 0.0);
        put_at(&db, MIDNIGHT + 645, 800, 0.0);
        let m = db.car_block_minutes_since(&cfg(), 0).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].timestamp, MIDNIGHT + 600);
        assert!((m[0].power_w - 7360.0 * 3.0 / 4.0).abs() < 1e-6);
    }

    #[test]
    fn only_readings_since_the_start_are_used() {
        let db = db();
        put_at(&db, MIDNIGHT + 60, 9000, 0.0);
        put_at(&db, MIDNIGHT + 6000, 9000, 0.0);
        let m = db.car_block_minutes_since(&cfg(), MIDNIGHT + 3600).unwrap();
        assert_eq!(
            m.iter().map(|x| x.timestamp).collect::<Vec<_>>(),
            vec![MIDNIGHT + 6000]
        );
    }

    #[test]
    fn block_minutes_give_the_same_energy_as_the_other_views() {
        // One rule: the profile's car kWh equals History's and Today's for
        // the same minute rows.
        let db = db();
        for m in 0..=90 {
            put_at(
                &db,
                MIDNIGHT + 3600 + m * 60,
                if m % 7 == 0 { 800 } else { 9000 },
                0.0,
            );
        }
        let minutes = db.car_block_minutes_since(&cfg(), 0).unwrap();
        let today = db
            .car_kwh_today_in(&cfg(), MIDNIGHT + 86_000, &utc())
            .unwrap();
        // Each minute row spans its own minute in the profile and runs to the
        // next row in the interval walk; they agree on a steady minute cadence
        // except for the last row's trailing minute.
        let last = MIDNIGHT + 3600 + 90 * 60;
        let trailing = minutes
            .iter()
            .find(|m| m.timestamp == last)
            .map_or(0.0, |m| m.power_w * 60.0 / 3_600_000.0);
        assert!(
            (kwh_of(&minutes) - trailing - today).abs() < 1e-6,
            "{} {} {}",
            kwh_of(&minutes),
            trailing,
            today
        );
    }

    #[test]
    fn a_missing_home_power_is_not_a_car_minute() {
        let db = db();
        put_at(&db, MIDNIGHT + 60, 9000, 0.0);
        db.conn
            .lock()
            .unwrap()
            .execute("UPDATE readings SET home_power = NULL", [])
            .unwrap();
        assert!(db.car_block_minutes_since(&cfg(), 0).unwrap().is_empty());
    }

    // ---- the daily report's rows ----

    #[test]
    fn report_rows_carry_the_car_power_with_the_charger_winning() {
        let db = db();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();
        let base = crate::history::local_midnight_timestamp(date).unwrap() + 3 * 3600;
        // base is a minute boundary in every whole-minute zone offset.
        for i in 0..3 {
            put_at(&db, base + i * 60, 9000, 0.0);
        }
        db.record_evc_power_minute(&crate::history::evc_power::EvcMinuteRow {
            timestamp: base + 60,
            power_w: 3000.0,
            samples: 6,
        })
        .unwrap();
        let rows = db
            .get_readings_for_date_with_car(date, Some(&cfg()))
            .unwrap();
        let car: Vec<Option<f64>> = rows.iter().map(|r| r.car_power).collect();
        assert_eq!(car, vec![Some(7360.0), Some(3000.0), Some(7360.0)]);
    }

    #[test]
    fn report_rows_have_no_car_power_without_a_config() {
        let db = db();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();
        let base = crate::history::local_midnight_timestamp(date).unwrap() + 3 * 3600;
        put_at(&db, base, 9000, 0.0);
        let rows = db.get_readings_for_date_with_car(date, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].car_power, None);
        let plain = db.get_readings_for_date(date).unwrap();
        assert_eq!(plain[0].car_power, None);
    }

    #[test]
    fn report_rows_with_no_home_power_have_no_car() {
        let db = db();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();
        let base = crate::history::local_midnight_timestamp(date).unwrap() + 3 * 3600;
        put_at(&db, base, 9000, 0.0);
        db.conn
            .lock()
            .unwrap()
            .execute("UPDATE readings SET home_power = NULL", [])
            .unwrap();
        let rows = db
            .get_readings_for_date_with_car(date, Some(&cfg()))
            .unwrap();
        assert_eq!(rows[0].home_power, None);
        assert_eq!(rows[0].car_power, None);
    }

    #[test]
    fn asking_for_both_energy_series_at_once_gives_the_same_as_asking_separately() {
        let db = db();
        for m in 0..=60 {
            put_at(&db, MIDNIGHT + 36_000 + m * 60, 9000, 0.15 * m as f32);
        }
        let ask = |fields: &[&str]| {
            let fields: Vec<String> = fields.iter().map(|f| f.to_string()).collect();
            db.query_history_with_car(
                0,
                3600,
                0,
                &fields,
                Some((MIDNIGHT, MIDNIGHT + 86_400)),
                Some(&cfg()),
            )
            .unwrap()
        };
        let both = ask(&[CAR_ENERGY_FIELD, HOME_EXCL_CAR_ENERGY_FIELD]);
        for field in [CAR_ENERGY_FIELD, HOME_EXCL_CAR_ENERGY_FIELD] {
            let alone = ask(&[field]);
            assert_eq!(both[field], alone[field], "{field}");
            assert!(!points(&both, field).is_empty());
        }
    }

    #[test]
    fn a_failed_block_query_is_logged_and_gives_no_minutes() {
        use tracing_subscriber::layer::SubscriberExt;
        let db = std::sync::Arc::new(db());
        let poisoner = db.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.conn.lock().unwrap();
            panic!("poison the connection lock");
        })
        .join();

        let ring = std::sync::Arc::new(crate::server::logs::LogRing::new(20));
        let subscriber = tracing_subscriber::Registry::default()
            .with(crate::server::logs::LogCaptureLayer::new(ring.clone()));
        let _guard =
            tracing::dispatcher::set_default(&tracing::dispatcher::Dispatch::new(subscriber));

        assert!(db.car_block_minutes_or_warn(&cfg(), 0).is_empty());
        let lines = ring.read_all();
        assert!(
            lines.iter().any(|l| l.contains("lock poisoned")),
            "the failure must be logged, got: {lines:?}"
        );
    }

    #[test]
    fn a_working_block_query_gives_its_minutes_through_the_wrapper() {
        let db = db();
        for m in 0..3 {
            put(&db, m, 9000, 230.0);
        }
        let direct = db.car_block_minutes_since(&cfg(), T0 - 60).unwrap();
        assert!(!direct.is_empty());
        assert_eq!(db.car_block_minutes_or_warn(&cfg(), T0 - 60), direct);
    }
}
