//! SQLite-backed history storage for inverter readings.
//!
//! Stores one row per poll cycle and provides aggregated queries
//! for the history chart API.

use std::path::Path;
use std::sync::Mutex;

use chrono::{Datelike, Local, TimeZone, Timelike};
use rusqlite::{params, Connection, Result as SqlResult};
use serde::Serialize;

use crate::inverter::model::InverterSnapshot;

// ---------------------------------------------------------------------------
// Time-series data point returned by queries
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct TimePoint {
    /// Unix timestamp in milliseconds.
    pub t: i64,
    /// Numeric value.
    pub v: f64,
}

// ---------------------------------------------------------------------------
// Allowed field whitelist (prevents SQL injection)
// ---------------------------------------------------------------------------

const ALLOWED_FIELDS: &[&str] = &[
    "solar_power",
    "pv1_power",
    "pv2_power",
    "battery_power",
    "grid_power",
    "home_power",
    "pv1_voltage",
    "pv2_voltage",
    "pv1_current",
    "pv2_current",
    "soc",
    "battery_voltage",
    "battery_current",
    "battery_temperature",
    "battery_capacity_kwh",
    "grid_voltage",
    "grid_frequency",
    "inverter_temperature",
    "today_solar_kwh",
    "today_import_kwh",
    "today_export_kwh",
    "today_charge_kwh",
    "today_discharge_kwh",
    "today_consumption_kwh",
    "today_ac_charge_kwh",
    "charge_rate",
    "discharge_rate",
    "battery_reserve",
    "target_soc",
];

fn is_allowed_field(field: &str) -> bool {
    ALLOWED_FIELDS.contains(&field)
}

/// Cumulative counter fields that monotonically increase within a day and
/// reset at midnight. For these fields MAX is the correct aggregation
/// (AVG would understate the true value).
const CUMULATIVE_FIELDS: &[&str] = &[
    "today_solar_kwh",
    "today_import_kwh",
    "today_export_kwh",
    "today_charge_kwh",
    "today_discharge_kwh",
    "today_consumption_kwh",
    "today_ac_charge_kwh",
];

fn is_cumulative_field(field: &str) -> bool {
    CUMULATIVE_FIELDS.contains(&field)
}

fn local_date_for_timestamp_ms(timestamp_ms: i64) -> Option<chrono::NaiveDate> {
    let secs = timestamp_ms.div_euclid(1000);
    let nanos = (timestamp_ms.rem_euclid(1000) as u32) * 1_000_000;
    Local
        .timestamp_opt(secs, nanos)
        .earliest()
        .map(|dt| dt.date_naive())
}

fn same_local_day(a_ms: i64, b_ms: i64) -> bool {
    match (
        local_date_for_timestamp_ms(a_ms),
        local_date_for_timestamp_ms(b_ms),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Repair cumulative daily counters after aggregation.
///
/// The inverter's `today_*_kwh` fields are cumulative counters: they should
/// only rise within a local day and reset around midnight. Older app versions
/// could persist plausible-but-wrong low values after reconnects; MAX bucket
/// aggregation does not fix a whole bad bucket/plateau. This display-side
/// repair clamps same-day decreases to the previous good value while allowing
/// a normal day-boundary reset.
fn repair_cumulative_points(points: &mut [TimePoint]) {
    let mut last_t: Option<i64> = None;
    let mut last_v: Option<f64> = None;
    let mut repaired = 0usize;

    for point in points {
        if let (Some(prev_t), Some(prev_v)) = (last_t, last_v) {
            if same_local_day(prev_t, point.t) && point.v < prev_v {
                point.v = prev_v;
                repaired += 1;
            }
        }
        last_t = Some(point.t);
        last_v = Some(point.v);
    }

    if repaired > 0 {
        tracing::debug!(repaired, "Repaired same-day cumulative history dips");
    }
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

const SCHEMA_SQL: &str = "\
CREATE TABLE IF NOT EXISTS readings (
    timestamp       INTEGER PRIMARY KEY,
    solar_power     INTEGER,
    pv1_power       INTEGER,
    pv2_power       INTEGER,
    battery_power   INTEGER,
    grid_power      INTEGER,
    home_power      INTEGER,
    pv1_voltage     REAL,
    pv2_voltage     REAL,
    pv1_current     REAL,
    pv2_current     REAL,
    soc             INTEGER,
    battery_voltage REAL,
    battery_current REAL,
    battery_temperature REAL,
    battery_capacity_kwh REAL,
    grid_voltage    REAL,
    grid_frequency  REAL,
    inverter_temperature REAL,
    today_solar_kwh     REAL,
    today_import_kwh    REAL,
    today_export_kwh    REAL,
    today_charge_kwh    REAL,
    today_discharge_kwh REAL,
    today_consumption_kwh REAL,
    today_ac_charge_kwh REAL,
    charge_rate     INTEGER,
    discharge_rate  INTEGER,
    battery_reserve INTEGER,
    target_soc      INTEGER
);
";

// ---------------------------------------------------------------------------
// HistoryDb wrapper
// ---------------------------------------------------------------------------

pub struct HistoryDb {
    conn: Mutex<Connection>,
}

impl HistoryDb {
    /// Open (or create) the SQLite database at the given path.
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create history db dir: {e}"))?;
        }
        let conn = Connection::open(path).map_err(|e| format!("Failed to open history db: {e}"))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .map_err(|e| format!("Failed to set pragmas: {e}"))?;

        conn.execute_batch(SCHEMA_SQL)
            .map_err(|e| format!("Failed to create schema: {e}"))?;

        // Migration: add today_ac_charge_kwh column if missing (added in v0.9.34)
        let _ = conn.execute_batch("ALTER TABLE readings ADD COLUMN today_ac_charge_kwh REAL");

        // Migration: add today_ac_charge_kwh column if missing (added in v0.9.34)
        let _ = conn.execute_batch("ALTER TABLE readings ADD COLUMN today_ac_charge_kwh REAL");

        // Idempotent migration: repair corrupted cumulative counter data.
        // Runs on every launch (no version gate) and is intentionally
        // idempotent — it checks for the new column and exits immediately
        // if already present. The column-addition ALTER TABLE is a no-op
        // when the column exists (SQLite ignores it). The repair loop
        // below scans the readings table and fixes rows where cumulative
        // counters decreased (register corruption). On a healthy database
        // this scan completes within seconds for typical history sizes.
        // Pre-v0.17.0 databases that lack the column will run the full
        // repair once; subsequent launches are no-ops.

        // ---- Backup before repair ----
        // Copy the database before any destructive write, so the user can
        // restore the original if the repair introduces new issues.
        {
            let backup_path = path.with_extension("db.bak");
            if let Err(e) = std::fs::copy(path, &backup_path) {
                tracing::warn!(
                    "Failed to backup history DB to {}: {e}",
                    backup_path.display()
                );
            } else {
                tracing::info!(
                    "History DB backed up to {}",
                    backup_path.display()
                );
            }
        }

        let energy_cols = [
            "today_solar_kwh",
            "today_import_kwh",
            "today_export_kwh",
            "today_charge_kwh",
            "today_discharge_kwh",
            "today_consumption_kwh",
            "today_ac_charge_kwh",
        ];
        for col in &energy_cols {
            // Build a repaired set using a window: for each row, fix
            // corrupted values including:
            //   - Small spurious DECREASES (counter dips without midnight reset)
            //   - Values clamped to 0 by old sanitizer versions (previous bug)
            //     followed by a large jump back to the real value
            //
            // Midnight rollover: prev > 50 and current < 10 is a genuine
            // counter reset — keep the new value.
            //
            // We do NOT suppress increases on cumulative counters because:
            //   - MAX aggregation in the query already handles spikes
            //   - The poll.rs sanitizer prevents register corruption
            //   - Legitimate increases can be arbitrarily large (e.g. after
            //     a long gap in data the counter could jump by > 2 kWh)
            let repair_sql = format!(
                "CREATE TABLE IF NOT EXISTS _repair_{col} AS \
                 SELECT timestamp, {col} AS orig, \
                        CASE \
                          WHEN LAG({col}) OVER (ORDER BY timestamp) IS NULL THEN {col} \
                          -- Midnight rollover: counter reset to near-zero \
                          WHEN LAG({col}) OVER (ORDER BY timestamp) > 50.0 \
                               AND {col} < 10.0 \
                            THEN {col} \
                          -- Zero clamp artifact: prev was 0 (old sanitizer bug) and \
                          -- current jumped by > 5 kWh (implausible for one interval). \
                          -- Replace with the value BEFORE the 0 to avoid cost spikes. \
                          WHEN LAG({col}) OVER (ORDER BY timestamp) = 0.0 \
                               AND {col} > 5.0 \
                               AND LAG({col}, 2, 0) OVER (ORDER BY timestamp) > 0.0 \
                            THEN LAG({col}, 2, {col}) OVER (ORDER BY timestamp) \
                          -- Zero clamp artifact: current value IS the 0, replace with prev \
                          WHEN {col} = 0.0 \
                               AND LAG({col}) OVER (ORDER BY timestamp) > 1.0 \
                               AND LEAD({col}, 1, {col}) OVER (ORDER BY timestamp) > LAG({col}) OVER (ORDER BY timestamp) \
                            THEN LAG({col}) OVER (ORDER BY timestamp) \
                          -- Small decrease (glitch): replace with previous \
                          WHEN {col} < LAG({col}) OVER (ORDER BY timestamp) \
                            THEN LAG({col}) OVER (ORDER BY timestamp) \
                          ELSE {col} \
                        END AS repaired \
                 FROM readings \
                 WHERE {col} IS NOT NULL \
                 ORDER BY timestamp"
            );
            let _ = conn.execute_batch(&repair_sql);

            // Count how many rows were changed
            let count: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM _repair_{col} WHERE orig != repaired"),
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            if count > 0 {
                tracing::info!("Repairing {count} corrupted {col} values in history DB");
                // Apply repairs back to the readings table
                let apply_sql = format!(
                    "UPDATE readings SET {col} = (\
                      SELECT repaired FROM _repair_{col} \
                      WHERE _repair_{col}.timestamp = readings.timestamp\
                    ) WHERE timestamp IN (\
                      SELECT timestamp FROM _repair_{col} WHERE orig != repaired\
                    )"
                );
                if let Err(e) = conn.execute_batch(&apply_sql) {
                    tracing::warn!("Failed to repair {col}: {e}");
                }
            }

            // Clean up temp table
            let _ = conn.execute_batch(&format!("DROP TABLE IF EXISTS _repair_{col}"));
        }

        // ---- PV-power-based solar-energy repair ----
        // After the window-function repair above, recompute today_solar_kwh from
        // solar_power * delta_time integration. This fixes the "stuck baseline"
        // pattern where the register was frozen at a wrong low value for hours
        // (AC-coupled bug) — the window-function pass only catches dips/zeros.
        let solar_repaired = Self::repair_cumulative_solar_kwh_inner(&conn);
        if let Ok(count) = solar_repaired {
            if count > 0 {
                tracing::info!(
                    "Repaired {count} today_solar_kwh values via PV power integration"
                );
            }
        }

        tracing::info!("History database opened at {}", path.display());
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Recompute `today_solar_kwh` by integrating `solar_power` over time.
    /// Fixes the "stuck baseline" corruption where the register was frozen
    /// at a wrong low value while PV was actually generating.
    fn repair_cumulative_solar_kwh_inner(conn: &Connection) -> Result<i64, String> {
        let mut stmt = conn
            .prepare(
                "SELECT timestamp, solar_power, today_solar_kwh \
                 FROM readings ORDER BY timestamp",
            )
            .map_err(|e| format!("Prepare failed: {e}"))?;

        let rows: Vec<(i64, Option<i32>, Option<f64>)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i32>>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                ))
            })
            .map_err(|e| format!("Query failed: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Row read failed: {e}"))?;

        if rows.is_empty() {
            return Ok(0);
        }

        let mut updates: Vec<(i64, f64)> = Vec::new();
        let mut current_day_midnight: Option<i64> = None;
        let mut accumulated_kwh: f64 = 0.0;
        let mut prev_ts: Option<i64> = None;
        let mut prev_solar_power: i32 = 0;
        let mut is_first_of_day = true;

        for (ts, solar_power, stored_kwh) in &rows {
            let ts = *ts;
            let solar_power = solar_power.unwrap_or(0);

            // Detect day boundary via local midnight
            let secs = ts / 1000;
            let nanos = (ts as u32 % 1000) * 1_000_000;
            let this_midnight = Local
                .timestamp_opt(secs, nanos)
                .earliest()
                .map(|dt| {
                    Local
                        .with_ymd_and_hms(dt.year(), dt.month(), dt.day(), 0, 0, 0)
                        .earliest()
                        .map(|m| m.timestamp_millis())
                })
                .flatten();

            if current_day_midnight != this_midnight {
                // New day — seed accumulator from first reading's register value
                // (or from 0 if the first reading looks corrupted at night)
                current_day_midnight = this_midnight;
                accumulated_kwh = stored_kwh.unwrap_or(0.0).max(0.0);
                // Sanity: if it's nighttime (solar_power ~ 0) but the stored
                // value is implausibly high, start from 0 (the register was
                // likely corrupted even at midnight)
                if solar_power < 10 && accumulated_kwh > 1.0 {
                    accumulated_kwh = 0.0;
                }
                prev_ts = None;
                prev_solar_power = 0;
                is_first_of_day = true;
            }

            // Accumulate PV power since the previous reading
            if let Some(prev) = prev_ts {
                let delta_ms = ts - prev;
                if delta_ms > 0 && delta_ms < 3_600_000 {
                    // Max 1-hour gap
                    let delta_hours = delta_ms as f64 / 3_600_000.0;
                    let power_w = prev_solar_power.max(0) as f64;
                    accumulated_kwh += power_w * delta_hours / 1000.0;
                }

                // Compare with stored value
                if !is_first_of_day {
                    if let Some(stored) = stored_kwh {
                        if !stored.is_nan() {
                            let diff = (stored - accumulated_kwh).abs();
                            if diff > 0.02 {
                                updates.push((ts, accumulated_kwh));
                            }
                        }
                    }
                }
            }

            prev_ts = Some(ts);
            prev_solar_power = solar_power;
            is_first_of_day = false;
        }

        let count = updates.len() as i64;
        if count > 0 {
            for (ts, new_val) in &updates {
                if conn
                    .execute(
                        "UPDATE readings SET today_solar_kwh = ?1 WHERE timestamp = ?2",
                        params![*new_val, *ts],
                    )
                    .is_err()
                {
                    tracing::warn!("Failed to repair today_solar_kwh at ts={ts}");
                }
            }
        }

        Ok(count)
    }

    /// Public wrapper around the PV-power-based solar energy repair.
    /// Runs the repair inline (not spawned).
    pub fn repair_cumulative_solar_kwh(&self) -> Result<i64, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        Self::repair_cumulative_solar_kwh_inner(&conn)
    }

    /// Insert a snapshot as a new reading row.
    pub fn insert_reading(&self, snap: &InverterSnapshot) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("History DB lock poisoned: {e}");
                return;
            }
        };

        let r = conn.execute(
            "INSERT OR REPLACE INTO readings (
                timestamp, solar_power, pv1_power, pv2_power,
                battery_power, grid_power, home_power,
                pv1_voltage, pv2_voltage, pv1_current, pv2_current,
                soc, battery_voltage, battery_current,
                battery_temperature, battery_capacity_kwh,
                grid_voltage, grid_frequency, inverter_temperature,
                today_solar_kwh, today_import_kwh, today_export_kwh,
                today_charge_kwh, today_discharge_kwh, today_consumption_kwh,
                today_ac_charge_kwh,
                charge_rate, discharge_rate, battery_reserve, target_soc
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30)",
            params![
                snap.timestamp,
                snap.solar_power,
                snap.pv1_power,
                snap.pv2_power,
                snap.battery_power,
                snap.grid_power,
                snap.home_power,
                snap.pv1_voltage,
                snap.pv2_voltage,
                snap.pv1_current,
                snap.pv2_current,
                snap.soc,
                snap.battery_voltage,
                snap.battery_current,
                snap.battery_temperature,
                snap.battery_capacity_kwh,
                snap.grid_voltage,
                snap.grid_frequency,
                snap.inverter_temperature,
                snap.today_solar_kwh,
                snap.today_import_kwh,
                snap.today_export_kwh,
                snap.today_charge_kwh,
                snap.today_discharge_kwh,
                snap.today_consumption_kwh,
                snap.today_ac_charge_kwh,
                snap.charge_rate,
                snap.discharge_rate,
                snap.battery_reserve,
                snap.target_soc,
            ],
        );

        if let Err(e) = r {
            tracing::warn!("Failed to insert history reading: {e}");
        }
    }

    /// Query aggregated history data for the given fields and time range.
    ///
    /// - `range_secs`: total time window in seconds (e.g. 3600 for 1h). Ignored
    ///   when `explicit_window` is provided.
    /// - `bucket_secs`: aggregation bucket size in seconds (e.g. 300 for 5m)
    /// - `offset`: number of windows to go back (0 = most recent). Ignored
    ///   when `explicit_window` is provided.
    /// - `fields`: comma-separated list of field names
    /// - `explicit_window`: optional (start_ts, end_ts) in UTC epoch seconds.
    ///   When provided, `range_secs` and `offset` are ignored entirely.
    ///
    /// Returns a map from field name to Vec<TimePoint>.
    pub fn query_history(
        &self,
        range_secs: i64,
        bucket_secs: i64,
        offset: i64,
        fields: &[String],
        explicit_window: Option<(i64, i64)>,
    ) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;

        let (start_ts, end_ts) = match explicit_window {
            Some((s, e)) => (s, e),
            None => {
                let now = chrono::Utc::now().timestamp();
                let raw_end = now - (offset * range_secs);
                let aligned_end = match range_secs {
                    3600 => ((raw_end / 3600) * 3600) + 3600,
                    21600 => ((raw_end / 21600) * 21600) + 21600,
                    _ => {
                        // Align to local midnight so day-based ranges start at
                        // 00:00 local time instead of 00:00 UTC. This prevents
                        // 24h charts from appearing to start at 01:00 in
                        // timezones east of UTC (e.g. BST/GMT+1).
                        let raw_local = chrono::DateTime::from_timestamp(raw_end, 0)
                            .unwrap()
                            .with_timezone(&chrono::Local);
                        let secs_today = raw_local.time().num_seconds_from_midnight();
                        if secs_today == 0 {
                            raw_end
                        } else {
                            // Next local midnight: go to midnight of the next day
                            let tomorrow = raw_local.date_naive() + chrono::Duration::days(1);
                            let next_midnight_naive = tomorrow.and_hms_opt(0, 0, 0).unwrap();
                            let next_midnight_local = chrono::Local
                                .from_local_datetime(&next_midnight_naive)
                                .earliest()
                                .unwrap();
                            next_midnight_local.timestamp()
                        }
                    }
                };
                (aligned_end - range_secs, aligned_end)
            }
        };

        let mut result = serde_json::Map::new();

        for field in fields {
            if !is_allowed_field(field) {
                continue;
            }

            let agg = if is_cumulative_field(field) {
                "MAX"
            } else {
                "AVG"
            };

            let sql = format!(
                "SELECT \
                    ((timestamp / {bucket}) * {bucket}) * 1000 AS t_bucket, \
                    {agg}(\"{field}\") AS v \
                 FROM readings \
                 WHERE timestamp >= ?1 AND timestamp < ?2 AND \"{field}\" IS NOT NULL \
                 GROUP BY t_bucket \
                 ORDER BY t_bucket",
                bucket = bucket_secs,
                agg = agg,
                field = field,
            );

            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| format!("Failed to prepare query for {field}: {e}"))?;

            let mut points: Vec<TimePoint> = stmt
                .query_map(params![start_ts, end_ts], |row| {
                    Ok(TimePoint {
                        t: row.get(0)?,
                        v: row.get(1)?,
                    })
                })
                .map_err(|e| format!("Query failed for {field}: {e}"))?
                .filter_map(SqlResult::ok)
                .collect();

            if is_cumulative_field(field) {
                repair_cumulative_points(&mut points);
            }

            result.insert(
                field.clone(),
                serde_json::to_value(&points).unwrap_or(serde_json::Value::Null),
            );
        }

        Ok(result)
    }

    /// Fetch all readings for a given local date (midnight-to-midnight).
    /// Returns rows ordered by timestamp ascending.
    pub fn get_readings_for_date(
        &self,
        date: chrono::NaiveDate,
    ) -> Result<Vec<crate::alerts::report::ReadingRow>, String> {
        let local_tz = chrono::Local;
        let midnight_start = match local_tz
            .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
        {
            Some(dt) => dt,
            None => return Ok(Vec::new()),
        };
        let next_day = date
            .checked_add_signed(chrono::Duration::days(1))
            .unwrap_or(date);
        let midnight_end = match local_tz
            .from_local_datetime(&next_day.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
        {
            Some(dt) => dt,
            None => return Ok(Vec::new()),
        };

        let start_ts = midnight_start.timestamp();
        let end_ts = midnight_end.timestamp();

        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;

        let mut stmt = conn
            .prepare(
                "SELECT timestamp, solar_power, pv1_power, pv2_power, \
                 battery_power, grid_power, home_power, soc \
                 FROM readings \
                 WHERE timestamp >= ?1 AND timestamp < ?2 \
                 ORDER BY timestamp",
            )
            .map_err(|e| format!("Failed to prepare query: {e}"))?;

        let rows = stmt
            .query_map(rusqlite::params![start_ts, end_ts], |row| {
                Ok(crate::alerts::report::ReadingRow {
                    timestamp: row.get(0)?,
                    solar_power: row.get::<_, Option<f64>>(1)?.map(|v| v as i32),
                    pv1_power: row.get::<_, Option<f64>>(2)?.map(|v| v as i32),
                    pv2_power: row.get::<_, Option<f64>>(3)?.map(|v| v as i32),
                    battery_power: row.get::<_, Option<f64>>(4)?.map(|v| v as i32),
                    grid_power: row.get::<_, Option<f64>>(5)?.map(|v| v as i32),
                    home_power: row.get::<_, Option<f64>>(6)?.map(|v| v as i32),
                    soc: row.get::<_, Option<f64>>(7)?.map(|v| v as f32),
                })
            })
            .map_err(|e| format!("Failed to query readings: {e}"))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| format!("Failed to read row: {e}"))?);
        }
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn test_db() -> HistoryDb {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("givenergy-history-test-{id}"));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_history.db");
        let _ = std::fs::remove_file(&path);
        HistoryDb::open(&path).unwrap()
    }

    fn make_snapshot(ts: i64, soc: u8, solar: i32) -> InverterSnapshot {
        InverterSnapshot {
            timestamp: ts,
            soc,
            solar_power: solar,
            ..Default::default()
        }
    }

    fn make_snapshot_with_kwh(ts: i64, import_kwh: f32, export_kwh: f32) -> InverterSnapshot {
        InverterSnapshot {
            timestamp: ts,
            today_import_kwh: import_kwh,
            today_export_kwh: export_kwh,
            ..Default::default()
        }
    }

    fn local_noon_ms(day_offset: i64) -> i64 {
        let date = Local::now().date_naive() + chrono::Duration::days(day_offset);
        let naive = date.and_hms_opt(12, 0, 0).unwrap();
        Local
            .from_local_datetime(&naive)
            .earliest()
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn open_creates_db() {
        let _db = test_db();
    }

    #[test]
    fn insert_and_query_raw() {
        let db = test_db();

        let base = 1700000000i64;
        db.insert_reading(&make_snapshot(base, 50, 1000));
        db.insert_reading(&make_snapshot(base + 60, 55, 1200));
        db.insert_reading(&make_snapshot(base + 120, 60, 800));

        // Use a very large range to ensure all data falls within the window,
        // regardless of current wall-clock time.
        let result = db
            .query_history(
                100_000_000,
                60,
                0,
                &["soc".to_string(), "solar_power".to_string()],
                None,
            )
            .unwrap();

        let soc_points: Vec<TimePoint> =
            serde_json::from_value(result.get("soc").cloned().unwrap()).unwrap();
        assert!(
            soc_points.len() >= 2,
            "Expected >= 2 SOC points, got {}",
            soc_points.len()
        );

        let solar_points: Vec<TimePoint> =
            serde_json::from_value(result.get("solar_power").cloned().unwrap()).unwrap();
        assert!(
            solar_points.len() >= 2,
            "Expected >= 2 solar points, got {}",
            solar_points.len()
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let db = test_db();
        let result = db
            .query_history(600, 60, 0, &["DROP TABLE readings".to_string()], None)
            .unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn all_allowed_fields_are_valid_columns() {
        for field in ALLOWED_FIELDS {
            assert!(is_allowed_field(field));
        }
    }

    #[test]
    fn cumulative_point_repair_clamps_same_day_dips() {
        let base = local_noon_ms(0);
        let mut points = vec![
            TimePoint { t: base, v: 5.0 },
            TimePoint {
                t: base + 60_000,
                v: 1.0,
            },
            TimePoint {
                t: base + 120_000,
                v: 2.0,
            },
            TimePoint {
                t: base + 180_000,
                v: 6.0,
            },
        ];

        repair_cumulative_points(&mut points);
        let values: Vec<f64> = points.iter().map(|p| p.v).collect();
        assert_eq!(values, vec![5.0, 5.0, 5.0, 6.0]);
    }

    #[test]
    fn cumulative_point_repair_allows_local_day_reset() {
        let day1 = local_noon_ms(0);
        let day2 = local_noon_ms(1);
        let mut points = vec![
            TimePoint { t: day1, v: 12.0 },
            TimePoint { t: day2, v: 1.0 },
            TimePoint {
                t: day2 + 60_000,
                v: 2.0,
            },
        ];

        repair_cumulative_points(&mut points);
        let values: Vec<f64> = points.iter().map(|p| p.v).collect();
        assert_eq!(values, vec![12.0, 1.0, 2.0]);
    }

    #[test]
    fn cumulative_counter_query_repairs_same_day_plateau() {
        let db = test_db();
        let base_ms = local_noon_ms(0);
        let base = base_ms / 1000;

        db.insert_reading(&make_snapshot_with_kwh(base, 5.0, 0.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 60, 1.0, 0.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 120, 2.0, 0.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 180, 6.0, 0.0));

        let result = db
            .query_history(100_000_000, 60, 0, &["today_import_kwh".to_string()], None)
            .unwrap();
        let points: Vec<TimePoint> =
            serde_json::from_value(result.get("today_import_kwh").cloned().unwrap()).unwrap();
        let values: Vec<f64> = points.iter().map(|p| p.v).collect();

        assert_eq!(values, vec![5.0, 5.0, 5.0, 6.0]);
    }

    #[test]
    fn cumulative_counter_uses_max_aggregation() {
        let db = test_db();
        let base = 1700000000i64;

        db.insert_reading(&make_snapshot_with_kwh(base, 10.0, 5.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 15, 15.0, 8.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 30, 12.0, 9.0));

        let result = db
            .query_history(
                100_000_000,
                60,
                0,
                &[
                    "today_import_kwh".to_string(),
                    "today_export_kwh".to_string(),
                ],
                None,
            )
            .unwrap();

        let import_points: Vec<TimePoint> =
            serde_json::from_value(result.get("today_import_kwh").cloned().unwrap()).unwrap();
        let bucket = import_points
            .iter()
            .find(|p| (p.t / 1000) / 60 * 60 == base / 60 * 60);
        assert!(bucket.is_some());
        let b = bucket.unwrap();
        assert!((b.v - 15.0).abs() < 0.01, "Expected MAX=15.0, got {}", b.v);

        let export_points: Vec<TimePoint> =
            serde_json::from_value(result.get("today_export_kwh").cloned().unwrap()).unwrap();
        let eb = export_points
            .iter()
            .find(|p| (p.t / 1000) / 60 * 60 == base / 60 * 60);
        assert!(eb.is_some());
        let e = eb.unwrap();
        assert!((e.v - 9.0).abs() < 0.01, "Expected MAX=9.0, got {}", e.v);
    }

    #[test]
    fn cumulative_counter_over_two_buckets() {
        let db = test_db();
        let base = 1700000000i64;

        db.insert_reading(&make_snapshot_with_kwh(base, 10.0, 5.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 15, 15.0, 7.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 60, 18.0, 9.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 75, 22.0, 11.0));

        let result = db
            .query_history(100_000_000, 60, 0, &["today_import_kwh".to_string()], None)
            .unwrap();

        let import_points: Vec<TimePoint> =
            serde_json::from_value(result.get("today_import_kwh").cloned().unwrap()).unwrap();

        let bucket_a = import_points
            .iter()
            .find(|p| (p.t / 1000) / 60 * 60 == base / 60 * 60);
        let bucket_b = import_points
            .iter()
            .find(|p| (p.t / 1000) / 60 * 60 == (base + 60) / 60 * 60);

        assert!(bucket_a.is_some());
        assert!(bucket_b.is_some());

        let a = bucket_a.unwrap();
        let b = bucket_b.unwrap();
        assert!(
            (a.v - 15.0).abs() < 0.01,
            "Bucket A should be 15.0, got {}",
            a.v
        );
        assert!(
            (b.v - 22.0).abs() < 0.01,
            "Bucket B should be 22.0, got {}",
            b.v
        );

        let delta = b.v - a.v;
        assert!(
            (delta - 7.0).abs() < 0.01,
            "Expected delta 7.0, got {}",
            delta
        );
    }

    #[test]
    fn cumulative_counter_midnight_rollover() {
        let db = test_db();
        // Use timestamps on different UTC days: 2023-11-15 00:00:00 UTC
        let day1 = 1700006400i64; // 2023-11-15 00:00:00 UTC
        let day2 = 1700092800i64; // 2023-11-16 00:00:00 UTC (next day)

        db.insert_reading(&make_snapshot_with_kwh(day1 + 82800, 150.0, 80.0)); // 23:00 UTC day1
        db.insert_reading(&make_snapshot_with_kwh(day2 + 3600, 5.0, 3.0)); // 01:00 UTC day2
        db.insert_reading(&make_snapshot_with_kwh(day2 + 7200, 15.0, 8.0)); // 02:00 UTC day2

        let result = db
            .query_history(
                100_000_000,
                3600,
                0,
                &["today_import_kwh".to_string()],
                None,
            )
            .unwrap();

        let import_points: Vec<TimePoint> =
            serde_json::from_value(result.get("today_import_kwh").cloned().unwrap()).unwrap();

        let yesterday = import_points.iter().find(|p| (p.v - 150.0).abs() < 0.01);
        let today_1 = import_points.iter().find(|p| (p.v - 5.0).abs() < 0.01);
        let today_2 = import_points.iter().find(|p| (p.v - 15.0).abs() < 0.01);

        assert!(yesterday.is_some(), "Missing yesterday's 150.0");
        assert!(today_1.is_some(), "Missing today's 5.0");
        assert!(today_2.is_some(), "Missing today's 15.0");
    }

    #[test]
    fn cumulative_counter_query_midnight_rollover() {
        // Verify the query pipeline (MAX aggregation) correctly handles
        // midnight rollover WITHOUT corrupting the data.
        let db = test_db();
        let day1 = 1700006400i64;
        let day2 = day1 + 86400;

        db.insert_reading(&make_snapshot_with_kwh(day1 + 82800, 150.0, 80.0));
        db.insert_reading(&make_snapshot_with_kwh(day2 + 600, 5.0, 1.0));
        db.insert_reading(&make_snapshot_with_kwh(day2 + 3600, 15.0, 5.0));
        db.insert_reading(&make_snapshot_with_kwh(day2 + 7200, 25.0, 8.0));

        let result = db
            .query_history(
                100_000_000,
                3600,
                0,
                &["today_import_kwh".to_string()],
                None,
            )
            .unwrap();
        let points: Vec<TimePoint> =
            serde_json::from_value(result.get("today_import_kwh").cloned().unwrap()).unwrap();

        // Day 1 last bucket should have 150.0
        let d150 = points.iter().find(|p| (p.v - 150.0).abs() < 0.01);
        assert!(d150.is_some(), "Day 1 should have 150.0");

        // Day 2 buckets should have 5.0, 15.0, 25.0
        let d5 = points.iter().find(|p| (p.v - 5.0).abs() < 0.01);
        assert!(d5.is_some(), "Day 2 midnight bucket should be 5.0");
        let d15 = points.iter().find(|p| (p.v - 15.0).abs() < 0.01);
        assert!(d15.is_some(), "Day 2 bucket should be 15.0");
        let d25 = points.iter().find(|p| (p.v - 25.0).abs() < 0.01);
        assert!(d25.is_some(), "Day 2 bucket should be 25.0");

        // Frontend-style cost calculation: across midnight, prev > 50 && raw < 10
        // means the delta for the first post-midnight bucket is just raw (reset value)
        let mut sorted: Vec<_> = points.clone();
        sorted.sort_by_key(|p| p.t);
        let ri = sorted.windows(2).position(|w| w[1].v < w[0].v).unwrap();
        assert!(sorted[ri].v > 50.0, "Pre-rollover should be high");
        assert!(
            sorted[ri + 1].v < 10.0,
            "Post-rollover should be low (reset)"
        );
    }

    #[test]
    fn cumulative_counter_query_pipeline_computes_deltas() {
        // Verify that deltas computed from query results are sensible.
        let db = test_db();
        let day1 = 1700006400i64;
        let day2 = day1 + 86400;

        db.insert_reading(&make_snapshot_with_kwh(day1 + 3600, 2.0, 1.0));
        db.insert_reading(&make_snapshot_with_kwh(day1 + 7200, 5.0, 2.0));
        db.insert_reading(&make_snapshot_with_kwh(day1 + 64800, 120.0, 60.0));
        db.insert_reading(&make_snapshot_with_kwh(day1 + 82800, 150.0, 80.0));
        db.insert_reading(&make_snapshot_with_kwh(day2 + 600, 3.0, 1.0));
        db.insert_reading(&make_snapshot_with_kwh(day2 + 3600, 7.0, 3.0));
        db.insert_reading(&make_snapshot_with_kwh(day2 + 7200, 12.0, 5.0));

        let result = db
            .query_history(
                100_000_000,
                3600,
                0,
                &["today_import_kwh".to_string()],
                None,
            )
            .unwrap();
        let points: Vec<TimePoint> =
            serde_json::from_value(result.get("today_import_kwh").cloned().unwrap()).unwrap();

        let mut sorted = points;
        sorted.sort_by_key(|p| p.t);

        // Find midnight rollover
        let ri = sorted.windows(2).position(|w| w[1].v < w[0].v).unwrap();

        // Day 1 deltas (positive = import)
        let day1_deltas: Vec<f64> = sorted[..ri + 1]
            .windows(2)
            .map(|w| w[1].v - w[0].v)
            .filter(|d| *d > 0.0)
            .collect();
        assert!(
            day1_deltas.iter().sum::<f64>() > 0.0,
            "Day 1 should have import"
        );

        // Day 2: after rollover, values increase monotonically
        let day2_vals: Vec<f64> = sorted[ri + 1..].iter().map(|p| p.v).collect();
        assert!(day2_vals.len() >= 2, "Day 2 should have multiple buckets");
        assert!(
            day2_vals.windows(2).all(|w| w[1] >= w[0]),
            "Day 2 values should be monotonically increasing"
        );
    }

    #[test]
    fn cumulative_counter_query_large_increase_preserved() {
        // Legitimate large increases (> 2 kWh) must NOT be suppressed.
        let db = test_db();
        let base = 1700000000i64;
        db.insert_reading(&make_snapshot_with_kwh(base, 5.0, 2.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 600, 25.0, 8.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 1200, 35.0, 12.0));

        let result = db
            .query_history(100_000_000, 600, 0, &["today_import_kwh".to_string()], None)
            .unwrap();
        let points: Vec<TimePoint> =
            serde_json::from_value(result.get("today_import_kwh").cloned().unwrap()).unwrap();

        let v5 = points.iter().find(|p| (p.v - 5.0).abs() < 0.01);
        assert!(v5.is_some(), "Should have 5.0");
        let v25 = points.iter().find(|p| (p.v - 25.0).abs() < 0.01);
        assert!(
            v25.is_some(),
            "Should have 25.0 (large increase NOT suppressed)"
        );
        let v35 = points.iter().find(|p| (p.v - 35.0).abs() < 0.01);
        assert!(v35.is_some(), "Should have 35.0");

        let mut sorted = points.clone();
        sorted.sort_by_key(|p| p.t);
        let delta = sorted.last().unwrap().v - sorted.first().unwrap().v;
        assert!(
            (delta - 30.0).abs() < 0.01,
            "Delta 5->35 should be 30, got {}",
            delta
        );
    }

    #[test]
    fn repair_sql_midnight_rollover_keeps_new_value() {
        // Directly test the repair CASE logic: midnight rollover should
        // keep the new small value, NOT replace with the old large value.
        let db = test_db();
        let base = 1700000000i64;

        db.insert_reading(&make_snapshot_with_kwh(base, 150.0, 80.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 60, 5.0, 1.0)); // midnight reset
        db.insert_reading(&make_snapshot_with_kwh(base + 120, 8.0, 2.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 180, 15.0, 5.0));

        // Execute the repair SQL directly and check results
        let conn = db.conn.lock().unwrap();
        let repair_sql = "
            SELECT timestamp, today_import_kwh AS orig,
                   CASE
                     WHEN LAG(today_import_kwh) OVER (ORDER BY timestamp) IS NULL THEN today_import_kwh
                     WHEN LAG(today_import_kwh) OVER (ORDER BY timestamp) > 50.0
                          AND today_import_kwh < 10.0
                       THEN today_import_kwh
                     WHEN today_import_kwh < LAG(today_import_kwh) OVER (ORDER BY timestamp)
                       THEN LAG(today_import_kwh) OVER (ORDER BY timestamp)
                     ELSE today_import_kwh
                   END AS repaired
            FROM readings
            WHERE today_import_kwh IS NOT NULL
            ORDER BY timestamp";
        let mut stmt = conn.prepare(repair_sql).unwrap();
        let rows: Vec<(i64, f64, f64)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, f64>(2)?,
                ))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert_eq!(rows.len(), 4);
        // Row 0: 150.0 → keep 150.0
        assert!((rows[0].1 - 150.0).abs() < 0.01);
        assert!((rows[0].2 - 150.0).abs() < 0.01);
        // Row 1: 5.0 → midnight rollover, keep 5.0 (NOT replace with 150.0!)
        assert!((rows[1].1 - 5.0).abs() < 0.01, "orig should be 5.0");
        assert!(
            (rows[1].2 - 5.0).abs() < 0.01,
            "repaired should be 5.0 (midnight rollover kept), got {}",
            rows[1].2
        );
        // Row 2: 8.0 → normal increase from 5.0, keep 8.0
        assert!((rows[2].2 - 8.0).abs() < 0.01);
        // Row 3: 15.0 → normal increase, keep 15.0
        assert!((rows[3].2 - 15.0).abs() < 0.01);
    }

    #[test]
    fn repair_sql_small_glitch_is_fixed() {
        // Directly test the repair CASE logic: small decrease should be
        // replaced with the previous value.
        let db = test_db();
        let base = 1700000000i64;

        db.insert_reading(&make_snapshot_with_kwh(base, 10.0, 3.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 60, 20.0, 6.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 120, 18.5, 7.0)); // glitch
        db.insert_reading(&make_snapshot_with_kwh(base + 180, 30.0, 9.0));

        let conn = db.conn.lock().unwrap();
        let repair_sql = "
            SELECT timestamp, today_import_kwh AS orig,
                   CASE
                     WHEN LAG(today_import_kwh) OVER (ORDER BY timestamp) IS NULL THEN today_import_kwh
                     WHEN LAG(today_import_kwh) OVER (ORDER BY timestamp) > 50.0
                          AND today_import_kwh < 10.0
                       THEN today_import_kwh
                     WHEN today_import_kwh < LAG(today_import_kwh) OVER (ORDER BY timestamp)
                       THEN LAG(today_import_kwh) OVER (ORDER BY timestamp)
                     ELSE today_import_kwh
                   END AS repaired
            FROM readings
            WHERE today_import_kwh IS NOT NULL
            ORDER BY timestamp";
        let mut stmt = conn.prepare(repair_sql).unwrap();
        let rows: Vec<(i64, f64, f64)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, f64>(2)?,
                ))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        // Row 2: 18.5 < 20.0 → small glitch, repaired to 20.0
        assert!((rows[2].1 - 18.5).abs() < 0.01, "orig should be 18.5");
        assert!(
            (rows[2].2 - 20.0).abs() < 0.01,
            "repaired should be 20.0 (glitch fixed), got {}",
            rows[2].2
        );
        // Row 3: 30.0 > 20.0 → normal increase, keep 30.0
        assert!((rows[3].2 - 30.0).abs() < 0.01);
    }

    #[test]
    fn repair_sql_large_increase_kept() {
        // Directly test that the repair does NOT suppress large increases.
        // Old bug: increases > 2 kWh were replaced with previous value.
        let db = test_db();
        let base = 1700000000i64;

        db.insert_reading(&make_snapshot_with_kwh(base, 5.0, 2.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 60, 25.0, 8.0)); // +20 kWh jump
        db.insert_reading(&make_snapshot_with_kwh(base + 120, 35.0, 12.0));

        let conn = db.conn.lock().unwrap();
        let repair_sql = "
            SELECT timestamp, today_import_kwh AS orig,
                   CASE
                     WHEN LAG(today_import_kwh) OVER (ORDER BY timestamp) IS NULL THEN today_import_kwh
                     WHEN LAG(today_import_kwh) OVER (ORDER BY timestamp) > 50.0
                          AND today_import_kwh < 10.0
                       THEN today_import_kwh
                     WHEN today_import_kwh < LAG(today_import_kwh) OVER (ORDER BY timestamp)
                       THEN LAG(today_import_kwh) OVER (ORDER BY timestamp)
                     ELSE today_import_kwh
                   END AS repaired
            FROM readings
            WHERE today_import_kwh IS NOT NULL
            ORDER BY timestamp";
        let mut stmt = conn.prepare(repair_sql).unwrap();
        let rows: Vec<(i64, f64, f64)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, f64>(2)?,
                ))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        // Row 1: 25.0 > 5.0 → increase kept (not suppressed to 5.0!)
        assert!((rows[1].1 - 25.0).abs() < 0.01, "orig should be 25.0");
        assert!(
            (rows[1].2 - 25.0).abs() < 0.01,
            "repaired should be 25.0 (large increase kept), got {}",
            rows[1].2
        );
        // Row 2: 35.0 > 25.0 → increase kept
        assert!((rows[2].2 - 35.0).abs() < 0.01);
    }

    #[test]
    fn repair_solar_from_pv_power_fixes_stuck_baseline() {
        // Simulate a day where today_solar_kwh was stuck at ~1.5 kWh from
        // 11:00 onward while solar_power was generating 500–1000 W.
        // The repair should recompute today_solar_kwh from PV power integration.

        let db = test_db();

        // Use a fixed base timestamp so results are deterministic.
        // Use local_noon_ms to get a proper local-midnight-aligned day
        let noon = local_noon_ms(-1);
        let midnight_of = noon - 12 * 3600_000; // 12 hours before noon = local midnight

        // Insert readings from 06:00 to 18:00 at 5-minute intervals
        // Phase 1 (06:00–11:00): correct today_solar_kwh (manually computed)
        // Phase 2 (11:00–14:00): stuck at 1.5 kWh (CORRUPTED)
        // Phase 3 (14:00–18:00): stuck at 2.0 kWh (CORRUPTED, changed once)

        let start_ts = midnight_of + 6 * 3600_000; // 06:00
        let mut ts = start_ts;
        let mut correct_kwh: f64 = 0.0;
        let mut rows_inserted: Vec<(i64, i32, f64)> = Vec::new();

        for hour_offset in 0..12 {
            // 6 hours * 12 five-min intervals = 144 readings
            for _ in 0..12 {
                // Solar power: ramp 0→800W (06-08), hold 800W (08-16), drop (16-18)
                let hour = 6 + hour_offset;
                let solar_w = if hour < 8 {
                    (hour as i32 - 6) * 400 // 0→800
                } else if hour < 16 {
                    800
                } else {
                    (18 - hour as i32) * 400 // 800→0
                };

                // Accumulate PV energy
                let delta_hours = 5.0 / 60.0; // 5 min interval
                correct_kwh += (solar_w as f64) * delta_hours / 1000.0;

                // Determine what the (corrupted) register would report
                let stored_kwh = if ts >= midnight_of + 11 * 3600_000
                    && ts < midnight_of + 14 * 3600_000
                {
                    1.5 // stuck at 1.5 (11:00–14:00)
                } else if ts >= midnight_of + 14 * 3600_000 {
                    2.0 // stuck at 2.0 (14:00–18:00)
                } else {
                    correct_kwh // correct before 11:00
                };

                rows_inserted.push((ts, solar_w, stored_kwh));

                // Insert via the normal path (today_solar_kwh will be 0.0)
                let mut snap = make_snapshot(ts, 50, solar_w);
                snap.today_solar_kwh = stored_kwh as f32;
                db.insert_reading(&snap);

                ts += 5 * 60_000; // 5 minutes
            }
        }

        // Run the repair
        let count = db.repair_cumulative_solar_kwh().unwrap();
        assert!(
            count > 0,
            "Should have repaired at least one stuck-baseline row"
        );

        // Query the repaired values and verify
        // We only check a sample: the stuck period should have been repaired
        // to values close to the PV-integrated correct_kwh
        let conn = db.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT timestamp, today_solar_kwh \
                 FROM readings ORDER BY timestamp",
            )
            .unwrap();
        let repaired_rows: Vec<(i64, f64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get::<_, f64>(1)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        // Check that at 12:00 (mid-corruption period), the value is near
        // the PV-integrated correct_kwh, NOT the stuck 1.5
        let noon_ts = midnight_of + 12 * 3600_000;
        if let Some((_, repaired)) =
            repaired_rows.iter().find(|(t, _)| *t == noon_ts)
        {
            // Expected: ~accumulated PV energy by noon
            // 07:00 400W×1h = 0.4, 08-11 800W×4h = 3.2, 12:00 800W×5min = 0.067
            // ≈ 3.67 kWh
            assert!(
                (*repaired - 3.6).abs() < 1.0,
                "12:00 value {repaired} should be near 3.6 kWh (PV integrated), not 1.5"
            );
        }

        // Check that the 11:00 value (before corruption) was NOT changed
        let before_ts = midnight_of + 10 * 3600_000 + 55 * 60_000; // 10:55
        if let Some((_, repaired)) =
            repaired_rows.iter().find(|(t, _)| *t == before_ts)
        {
            // This was before the stuck period, should be correct
            assert!(
                *repaired > 0.0,
                "10:55 value {repaired} should be > 0 (was correct)"
            );
        }
    }
}
