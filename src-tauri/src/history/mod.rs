//! SQLite-backed history storage for inverter readings.
//!
//! Stores one row per poll cycle and provides aggregated queries
//! for the history chart API.

use std::path::Path;
use std::sync::Mutex;

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
];

fn is_cumulative_field(field: &str) -> bool {
    CUMULATIVE_FIELDS.contains(&field)
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
        let conn = Connection::open(path)
            .map_err(|e| format!("Failed to open history db: {e}"))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .map_err(|e| format!("Failed to set pragmas: {e}"))?;

        conn.execute_batch(SCHEMA_SQL)
            .map_err(|e| format!("Failed to create schema: {e}"))?;

        // Migration: repair corrupted cumulative counter data.
        // For each today_*_kwh column, fix rows where the value decreased
        // from the previous row (counters are monotonically increasing)
        // or jumped up by more than 2 kWh (implausible between polls).
        // Skip if previous value is <= 5 (near midnight reset).
        let energy_cols = [
            "today_solar_kwh",
            "today_import_kwh",
            "today_export_kwh",
            "today_charge_kwh",
            "today_discharge_kwh",
            "today_consumption_kwh",
        ];
        for col in &energy_cols {
            // Build a repaired set using a window: for each row, if the value
            // decreased from the previous good value (and prev > 5, i.e. not
            // midnight reset), or jumped up by > 2, replace with previous.
            let repair_sql = format!(
                "CREATE TABLE IF NOT EXISTS _repair_{col} AS \
                 SELECT timestamp, {col} AS orig, \
                        CASE \
                          WHEN LAG({col}) OVER (ORDER BY timestamp) IS NULL THEN {col} \
                          WHEN LAG({col}) OVER (ORDER BY timestamp) > 5.0 \
                               AND {col} < LAG({col}) OVER (ORDER BY timestamp) \
                            THEN LAG({col}) OVER (ORDER BY timestamp) \
                          WHEN {col} > LAG({col}) OVER (ORDER BY timestamp) + 2.0 \
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

        tracing::info!("History database opened at {}", path.display());
        Ok(Self {
            conn: Mutex::new(conn),
        })
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
                charge_rate, discharge_rate, battery_reserve, target_soc
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29)",
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
    /// - `range_secs`: total time window in seconds (e.g. 3600 for 1h)
    /// - `bucket_secs`: aggregation bucket size in seconds (e.g. 300 for 5m)
    /// - `offset`: number of windows to go back (0 = most recent)
    /// - `fields`: comma-separated list of field names
    ///
    /// Returns a map from field name to Vec<TimePoint>.
    pub fn query_history(
        &self,
        range_secs: i64,
        bucket_secs: i64,
        offset: i64,
        fields: &[String],
    ) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;

        let now = chrono::Utc::now().timestamp();
        let raw_end = now - (offset * range_secs);
        let aligned_end = match range_secs {
            3600 => ((raw_end / 3600) * 3600) + 3600,
            21600 => ((raw_end / 21600) * 21600) + 21600,
            _ => ((raw_end / 86400) * 86400) + 86400,
        };
        let start_ts = aligned_end - range_secs;
        let end_ts = aligned_end;

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

            let points: Vec<TimePoint> = stmt
                .query_map(params![start_ts, end_ts], |row| {
                    Ok(TimePoint {
                        t: row.get(0)?,
                        v: row.get(1)?,
                    })
                })
                .map_err(|e| format!("Query failed for {field}: {e}"))?
                .filter_map(SqlResult::ok)
                .collect();

            result.insert(
                field.clone(),
                serde_json::to_value(&points).unwrap_or(serde_json::Value::Null),
            );
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
        let s = InverterSnapshot {
            timestamp: ts,
            soc,
            solar_power: solar,
            ..Default::default()
        };
        s
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
            .query_history(100_000_000, 60, 0, &["soc".to_string(), "solar_power".to_string()])
            .unwrap();

        let soc_points: Vec<TimePoint> =
            serde_json::from_value(result.get("soc").cloned().unwrap()).unwrap();
        assert!(soc_points.len() >= 2, "Expected >= 2 SOC points, got {}", soc_points.len());

        let solar_points: Vec<TimePoint> =
            serde_json::from_value(result.get("solar_power").cloned().unwrap()).unwrap();
        assert!(solar_points.len() >= 2, "Expected >= 2 solar points, got {}", solar_points.len());
    }

    #[test]
    fn rejects_unknown_fields() {
        let db = test_db();
        let result = db
            .query_history(600, 60, 0, &["DROP TABLE readings".to_string()])
            .unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn all_allowed_fields_are_valid_columns() {
        for field in ALLOWED_FIELDS {
            assert!(is_allowed_field(field));
        }
    }
}
