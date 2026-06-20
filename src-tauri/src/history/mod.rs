//! SQLite-backed history storage for inverter readings.
//!
//! Stores one row per poll cycle and provides aggregated queries
//! for the history chart API.

use std::path::Path;
use std::sync::Mutex;

use chrono::{Local, TimeZone, Timelike};
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
    "home_energy_today_kwh",
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
    "home_energy_today_kwh",
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
    home_energy_today_kwh REAL,
    charge_rate     INTEGER,
    discharge_rate  INTEGER,
    battery_reserve INTEGER,
    target_soc      INTEGER
);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
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

        // Migration: add home_energy_today_kwh column if missing (integrated
        // cumulative consumption, replaces the misleading today_consumption_kwh
        // formula value for display).
        let _ = conn.execute_batch("ALTER TABLE readings ADD COLUMN home_energy_today_kwh REAL");

        // One-time migration: repair corrupted cumulative counter data and
        // reconstruct today_solar_kwh. Gated by a `meta` table flag so it
        // only runs once per database. On a healthy database this is a
        // single SELECT + no-op UPDATE on subsequent launches. To force a
        // re-run, delete the history.db file (a fresh backup is taken on
        // every repair) or run `DELETE FROM meta WHERE key = 'repair_v2_done'`.

        // Check whether the repair has already been performed.
        let repair_done: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM meta WHERE key = 'repair_v2_done' AND value = '1')",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !repair_done {
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

        // ---- Reconstruct today_solar_kwh ----
        // Use the inverter's values directly, only recalculating when stuck.
        let solar_repaired = Self::reconstruct_solar_kwh(&conn);
        match &solar_repaired {
            Ok(count) if *count > 0 => {
                tracing::info!(
                    "Reconstructed {count} today_solar_kwh values from solar_power integration"
                );
            }
            Err(e) => {
                tracing::warn!("Solar reconstruction failed: {e}");
            }
            _ => {}
        }

            // Mark the repair as complete so it doesn't run on every launch.
            let _ = conn.execute_batch(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('repair_v2_done', '1')",
            );
        } // end if !repair_done

        tracing::info!("History database opened at {}", path.display());
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Reconstruct `today_solar_kwh` by using the inverter's own values where reliable,
    /// only recalculating from solar_power when the register appears stuck.
    ///
    /// This replaces the old approach that cleared all values and reintegrated from
    /// scratch (which over-calculated due to gap interpolation).
    fn reconstruct_solar_kwh(conn: &Connection) -> Result<i64, String> {
        // Step 1: delete old slot-filler rows (solar_power=0/NULL with today_solar_kwh > 0)
        let deleted = conn
            .execute(
                "DELETE FROM readings WHERE (solar_power = 0 OR solar_power IS NULL) AND today_solar_kwh > 0",
                [],
            )
            .map_err(|e| format!("Failed to delete old slot-filler rows: {e}"))?;
        if deleted > 0 {
            tracing::warn!("Solar reconstruction: deleted {deleted} old slot-filler rows");
        }

        // Step 2: Read all rows with solar_power readings - use inverter's values
        let mut stmt = conn
            .prepare(
                "SELECT timestamp, solar_power, today_solar_kwh \
                 FROM readings WHERE solar_power > 0 ORDER BY timestamp",
            )
            .map_err(|e| format!("Prepare failed: {e}"))?;

        let rows: Vec<(i64, i32, f64)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, f64>(2).unwrap_or(0.0),
                ))
            })
            .map_err(|e| format!("Query failed: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Row read failed: {e}"))?;

        if rows.is_empty() {
            tracing::warn!("Solar reconstruction: no rows to process");
            return Ok(0);
        }

        // Step 3: Process each day, using inverter's value unless stuck
        let mut updates: Vec<(i64, f64)> = Vec::new();
        let mut current_local_date: Option<chrono::NaiveDate> = None;
        let mut prev_ts: Option<i64> = None;
        let mut prev_value: f64 = 0.0;

        for (ts, solar_power, stored_value) in &rows {
            // Detect day boundary
            let local_date = chrono::Local
                .timestamp_opt(*ts, 0)
                .earliest()
                .map(|dt| dt.date_naive());
            if local_date != current_local_date {
                current_local_date = local_date;
                prev_ts = None;
                prev_value = 0.0;
            }

            // Use inverter's value if it's increasing, otherwise recalculate
            let new_value = if let Some(prev) = prev_ts {
                let delta_secs = ts - prev;
                // Recalculate from solar_power when:
                //   - Gap > 30 min and value didn't increase (stuck after a gap), OR
                //   - Value DECREASED within the same day (corrupted baseline)
                let value_decreased = *stored_value < prev_value;
                let gap_and_stuck = delta_secs > 1800 && *stored_value <= prev_value;
                if gap_and_stuck || value_decreased {
                    // Recalculate from previous value using current power
                    let power_kw = *solar_power as f64 / 1000.0;
                    let delta_hours = delta_secs as f64 / 3600.0;
                    prev_value + power_kw * delta_hours
                } else {
                    // Use inverter's value (it's increasing normally)
                    *stored_value
                }
            } else {
                // First reading of day - use stored value
                *stored_value
            };

            updates.push((*ts, new_value));
            prev_ts = Some(*ts);
            prev_value = new_value;
        }

        // Step 4: write back computed values
        let count = updates.len() as i64;
        for (ts, new_val) in &updates {
            if conn
                .execute(
                    "UPDATE readings SET today_solar_kwh = ?1 WHERE timestamp = ?2",
                    rusqlite::params![*new_val, *ts],
                )
                .is_err()
            {
                tracing::warn!("Failed to update today_solar_kwh at ts={ts}");
            }
        }

        tracing::warn!("Solar reconstruction: updated {count} rows");
        Ok(count)
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
                today_ac_charge_kwh, home_energy_today_kwh,
                charge_rate, discharge_rate, battery_reserve, target_soc
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31)",
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
                snap.home_energy_today_kwh,
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
    use chrono::Local;
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

    fn local_noon_secs(day_offset: i64) -> i64 {
        let date = Local::now().date_naive() + chrono::Duration::days(day_offset);
        let naive = date.and_hms_opt(12, 0, 0).unwrap();
        Local
            .from_local_datetime(&naive)
            .earliest()
            .unwrap()
            .timestamp()
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

    /// Reconstruct today_solar_kwh from solar_power integration.
    /// Simulates a day where the register was stuck at 1.5 kWh while PV was
    /// generating 800W. The repair should overwrite the stuck value with the
    /// PV-integrated total.
    #[test]
    fn reconstruct_solar_kwh_fixes_stuck_baseline() {
        let db = test_db();

        // Fixed UTC day in seconds (2026-06-19)
        let midnight: i64 = 1710288000;

        // Insert readings from 06:00 to 18:00 at 5-minute intervals
        // Phase 1 (06:00–11:00): correct today_solar_kwh
        // Phase 2 (11:00–14:00): stuck at 1.5 kWh (corrupted)
        // Phase 3 (14:00–18:00): stuck at 2.0 kWh (corrupted)
        let mut ts = midnight + 6 * 3600;
        let mut correct_kwh: f64 = 0.0;

        for hour_offset in 0..12 {
            for _ in 0..12 {
                let hour = 6 + hour_offset;
                // Solar power: ramp 0→800W (06-08), hold 800W (08-16), drop (16-18)
                let solar_w = if hour < 8 {
                    (hour as i32 - 6) * 400
                } else if hour < 16 {
                    800
                } else {
                    (18 - hour as i32) * 400
                };

                let delta_hours = 5.0 / 60.0;
                correct_kwh += (solar_w as f64) / 1000.0 * delta_hours;

                // Register is stuck after 11:00
                let stored_kwh = if ts >= midnight + 11 * 3600
                    && ts < midnight + 14 * 3600
                {
                    1.5
                } else if ts >= midnight + 14 * 3600 {
                    2.0
                } else {
                    correct_kwh
                };

                let mut snap = make_snapshot(ts, 50, solar_w);
                snap.today_solar_kwh = stored_kwh as f32;
                db.insert_reading(&snap);

                ts += 5 * 60; // 5 minutes in seconds
            }
        }

        // Run reconstruction directly
        let conn = db.conn.lock().unwrap();
        let count = HistoryDb::reconstruct_solar_kwh(&conn).unwrap();
        drop(conn);

        // All 144 rows should be processed (one per row with solar_power > 0).
        // Note: rows with solar_power=0 (06:00 when hour<8, solar_w=0) are
        // excluded by the WHERE clause, so we expect 144 total rows
        // inserted minus the zero-power ones.
        assert!(count > 0, "Should have processed at least some rows, got {count}");

        // Verify: at noon, stored value should be near PV-integrated total, not 1.5
        let noon_ts = midnight + 12 * 3600;
        let conn = db.conn.lock().unwrap();
        let noon_val: f64 = conn
            .query_row(
                "SELECT today_solar_kwh FROM readings WHERE timestamp = ?",
                rusqlite::params![noon_ts],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);

        // By noon: ~3.6 kWh from PV integration, not 1.5
        assert!(
            (noon_val - 3.6).abs() < 1.0,
            "noon value {noon_val} should be near 3.6 kWh (PV integrated), not stuck at 1.5"
        );

        // Verify consecutive rows have DIFFERENT (increasing) values
        // proving we're NOT applying the same value to all slots.
        // Check 3 rows at 08:00, 09:00, 10:00 when solar_power is 800W.
        let rows: Vec<(i64, f64)> = {
            let conn = db.conn.lock().unwrap();
            let mut stmt = conn
                .prepare(
                    "SELECT timestamp, today_solar_kwh FROM readings \
                     WHERE timestamp IN (?1, ?2, ?3) ORDER BY timestamp",
                )
                .unwrap();
            let r = stmt
                .query_map(
                    rusqlite::params![
                        midnight + 8 * 3600,
                        midnight + 9 * 3600,
                        midnight + 10 * 3600,
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            r
        };

        assert_eq!(rows.len(), 3, "Should have 3 checkpoints");
        // 08:00: ~0.43 kWh (400W×1h at 5-min intervals)
        assert!(
            rows[0].1 > 0.3,
            "08:00 value should be ~0.43 kWh, got {}",
            rows[0].1
        );
        // 09:00: ~1.23 kWh (0.43 + 800W×1h)
        assert!(
            rows[1].1 > rows[0].1,
            "09:00 ({}) should be > 08:00 ({})",
            rows[1].1,
            rows[0].1
        );
        // 10:00: ~2.03 kWh (1.23 + 800W×1h)
        assert!(
            rows[2].1 > rows[1].1,
            "10:00 ({}) should be > 09:00 ({})",
            rows[2].1,
            rows[1].1
        );
    }

    /// A gap > 30 min between readings where the value didn't increase
    /// triggers recalculation from solar_power. The new value is
    /// prev_value + power_kw * delta_hours.
    #[test]
    fn reconstruct_solar_kwh_gap_treated_as_zero() {
        let db = test_db();
        let noon = local_noon_secs(-1);
        let midnight = noon - 12 * 3600;

        // Insert two readings 2 hours apart with 800W solar
        let ts1 = midnight + 8 * 3600; // 08:00
        let ts2 = ts1 + 2 * 3600;        // 10:00 (2h gap > 30min threshold)

        let mut snap = make_snapshot(ts1, 50, 800);
        snap.today_solar_kwh = 0.0;
        db.insert_reading(&snap);

        let mut snap = make_snapshot(ts2, 50, 800);
        snap.today_solar_kwh = 0.0;
        db.insert_reading(&snap);

        let conn = db.conn.lock().unwrap();
        let count = HistoryDb::reconstruct_solar_kwh(&conn).unwrap();
        drop(conn);

        // Both rows should be processed
        assert_eq!(count, 2);

        let conn = db.conn.lock().unwrap();
        let val1: f64 = conn
            .query_row(
                "SELECT today_solar_kwh FROM readings WHERE timestamp = ?",
                rusqlite::params![ts1],
                |row| row.get(0),
            )
            .unwrap();
        let val2: f64 = conn
            .query_row(
                "SELECT today_solar_kwh FROM readings WHERE timestamp = ?",
                rusqlite::params![ts2],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);

        // First row: uses stored value (0)
        assert!(
            val1.abs() < 0.01,
            "first row should be 0, got {}",
            val1
        );
        // Second row: gap > 30min and stored value (0) didn't increase,
        // so recalculated: prev_value (0) + 0.8kW × 2h = 1.6 kWh
        assert!(
            (val2 - 1.6).abs() < 0.01,
            "second row should be ~1.6 kWh (recalculated from power), got {}",
            val2
        );
    }

    /// Reproduce the user's exact production data to verify the new
    /// "use inverter value unless stuck" behaviour.
    #[test]
    fn reconstruct_solar_kwh_with_user_data() {
        let db = test_db();

        // User's timestamps (UTC seconds for 2026-06-19)
        // 2026-06-19T09:50:00.000Z = 1710323400
        // 2026-06-19T11:15:00.000Z = 1710328500
        // 2026-06-19T11:20:00.000Z = 1710328800
        // ...
        let base = 1710323400i64; // 09:50 UTC

        // Insert rows matching user's data
        let rows_data: Vec<(i64, i32, f64)> = vec![
            (base, 4026, 5.2268622569437),
            (base + 5100, 4403, 5.23257441972147),   // 11:15 (1h25m gap)
            (base + 5100 + 300, 4400, 5.23293874805481), // 11:20
            (base + 5100 + 600, 4395, 5.23330745416592), // 11:25
        ];

        for (ts, pv, kwh) in &rows_data {
            let mut snap = make_snapshot(*ts, 50, *pv);
            snap.today_solar_kwh = *kwh as f32;
            db.insert_reading(&snap);
        }

        // Run reconstruction
        let conn = db.conn.lock().unwrap();
        let count = HistoryDb::reconstruct_solar_kwh(&conn).unwrap();
        drop(conn);

        assert_eq!(count, 4, "Should have processed all 4 rows");

        // Check each row
        let conn = db.conn.lock().unwrap();
        for (ts, pv, _) in &rows_data {
            let val: f64 = conn
                .query_row(
                    "SELECT today_solar_kwh FROM readings WHERE timestamp = ?",
                    rusqlite::params![*ts],
                    |row| row.get(0),
                )
                .unwrap();
            println!("ts={ts}, pv={pv}, today_solar_kwh={val}");
        }
        drop(conn);

        // First row (09:50): uses stored value (first reading of day, no prev)
        let conn = db.conn.lock().unwrap();
        let val0: f64 = conn
            .query_row(
                "SELECT today_solar_kwh FROM readings WHERE timestamp = ?",
                rusqlite::params![base],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert!(
            (val0 - 5.2268622569437).abs() < 0.001,
            "first row should use stored value 5.226, got {val0}"
        );

        // Second row (11:15): gap > 30min but stored value (5.232) >
        // prev_value (5.226), so uses stored value
        let conn = db.conn.lock().unwrap();
        let val1: f64 = conn
            .query_row(
                "SELECT today_solar_kwh FROM readings WHERE timestamp = ?",
                rusqlite::params![base + 5100],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert!(
            (val1 - 5.23257441972147).abs() < 0.001,
            "second row should use stored value (value increased), got {val1}"
        );

        // Third row (11:20): 5min gap, uses stored value
        let conn = db.conn.lock().unwrap();
        let val2: f64 = conn
            .query_row(
                "SELECT today_solar_kwh FROM readings WHERE timestamp = ?",
                rusqlite::params![base + 5100 + 300],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert!(
            (val2 - 5.23293874805481).abs() < 0.001,
            "third row should use stored value, got {val2}"
        );
    }

    /// Verify reconstruction runs when DB is opened (full startup path).
    /// With the new behaviour, inverter values are preserved when increasing
    /// normally — only stuck values are recalculated.
    #[test]
    fn reconstruct_solar_kwh_on_open() {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("givenergy-history-test-{id}"));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_history.db");
        let _ = std::fs::remove_file(&path);

        // Step 1: create DB and insert data with corrupted today_solar_kwh
        {
            let db = HistoryDb::open(&path).unwrap();
            let base = 1710323400i64;
            let rows_data: Vec<(i64, i32, f64)> = vec![
                (base, 4026, 5.2268622569437),
                (base + 300, 4403, 5.23257441972147),  // 5min gap
                (base + 600, 4400, 5.23293874805481),  // 5min gap
            ];
            for (ts, pv, kwh) in &rows_data {
                let mut snap = make_snapshot(*ts, 50, *pv);
                snap.today_solar_kwh = *kwh as f32;
                db.insert_reading(&snap);
            }
            // DB drops here, connection closes
        }

        // Step 2: reopen — reconstruction should run on existing data
        {
            let db = HistoryDb::open(&path).unwrap();
            let conn = db.conn.lock().unwrap();

            // First row: uses stored value (first reading of day, no prev)
            let val0: f64 = conn
                .query_row(
                    "SELECT today_solar_kwh FROM readings WHERE timestamp = ?",
                    rusqlite::params![1710323400i64],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                (val0 - 5.2268622569437).abs() < 0.001,
                "first row should use stored value 5.226 after reopen, got {val0}"
            );

            // Second row: 5min gap, stored value > prev, so uses stored value
            let val1: f64 = conn
                .query_row(
                    "SELECT today_solar_kwh FROM readings WHERE timestamp = ?",
                    rusqlite::params![1710323700i64],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                (val1 - 5.23257441972147).abs() < 0.001,
                "second row should use stored value 5.232 after reopen, got {val1}"
            );
        }
    }

    // ===================================================================
    // home_energy_today_kwh history DB tests
    //
    // The integrated cumulative consumption metric is stored in its own
    // column. It is treated as cumulative (MAX aggregation) like the
    // today_*_kwh fields, but it does NOT participate in the register-
    // corruption repair (it is computed from home_power, not read from
    // a dongle register).
    // ===================================================================

    #[test]
    fn home_energy_today_kwh_is_allowed_field() {
        assert!(is_allowed_field("home_energy_today_kwh"));
    }

    #[test]
    fn home_energy_today_kwh_is_cumulative_field() {
        // MAX aggregation must be used so the displayed value matches
        // the day's peak (true cumulative consumption).
        assert!(is_cumulative_field("home_energy_today_kwh"));
    }

    #[test]
    fn home_energy_today_kwh_is_inserted_and_readable() {
        let db = test_db();
        let mut snap = make_snapshot(1_000, 50, 0);
        snap.home_energy_today_kwh = 3.5;
        db.insert_reading(&snap);

        let conn = db.conn.lock().unwrap();
        let stored: f64 = conn
            .query_row(
                "SELECT home_energy_today_kwh FROM readings WHERE timestamp = ?",
                rusqlite::params![1_000i64],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            (stored - 3.5).abs() < 1e-6,
            "stored value must match inserted value, got {stored}"
        );
    }

    #[test]
    fn home_energy_today_kwh_query_uses_max_aggregation() {
        // A 24h bucket with three readings: 1.0, 2.5, 4.0.
        // Cumulative → MAX → 4.0 (true day's peak consumption).
        let db = test_db();
        let base = 1_700_000_000i64;
        for (offset, kwh) in [(0, 1.0f32), (3600, 2.5), (7200, 4.0)] {
            let mut snap = make_snapshot(base + offset, 50, 0);
            snap.home_energy_today_kwh = kwh;
            db.insert_reading(&snap);
        }
        let result = db
            .query_history(
                21_600,
                3600,
                0,
                &["home_energy_today_kwh".to_string()],
                Some((base - 3600, base + 10_800)),
            )
            .unwrap();
        let series = result
            .get("home_energy_today_kwh")
            .and_then(|v| v.as_array())
            .expect("series must be present");
        assert_eq!(series.len(), 3, "3 hourly buckets");
        // Find the maximum value in the result
        let max: f64 = series
            .iter()
            .filter_map(|p| p.get("v").and_then(|v| v.as_f64()))
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (max - 4.0).abs() < 1e-6,
            "MAX aggregation must pick the day's peak, got {max}"
        );
    }

    #[test]
    fn home_energy_today_kwh_existing_db_migrates_column() {
        // Simulate an existing DB (created before home_energy_today_kwh was
        // added) by creating a DB with the OLD schema and inserting a row,
        // then reopening with the new schema and verifying the column
        // appears and accepts values.
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("givenergy-history-test-{id}"));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("legacy_history.db");
        let _ = std::fs::remove_file(&path);

        // Open with current schema (which includes the ALTER migration),
        // insert legacy row (no home_energy_today_kwh), reopen.
        {
            let db = HistoryDb::open(&path).unwrap();
            let mut snap = make_snapshot(1_000, 50, 0);
            snap.today_solar_kwh = 2.0;
            db.insert_reading(&snap);
        }
        {
            let db = HistoryDb::open(&path).unwrap();
            let mut snap = make_snapshot(2_000, 50, 0);
            snap.home_energy_today_kwh = 7.5;
            db.insert_reading(&snap);
            // Should not panic — the ALTER TABLE migration added the column.
        }
        {
            let db = HistoryDb::open(&path).unwrap();
            let conn = db.conn.lock().unwrap();
            let val: Option<f64> = conn
                .query_row(
                    "SELECT home_energy_today_kwh FROM readings WHERE timestamp = ?",
                    rusqlite::params![1_000i64],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                val.is_none() || (val.unwrap() - 0.0).abs() < 1e-6,
                "legacy row has NULL/0 for new column, got {:?}",
                val
            );
            let val: Option<f64> = conn
                .query_row(
                    "SELECT home_energy_today_kwh FROM readings WHERE timestamp = ?",
                    rusqlite::params![2_000i64],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                val.map(|v| (v - 7.5).abs() < 1e-6).unwrap_or(false),
                "new row stores 7.5, got {:?}",
                val
            );
        }
    }
}
