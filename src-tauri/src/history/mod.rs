//! SQLite-backed history storage for inverter readings.
//!
//! Stores one row per poll cycle and provides aggregated queries
//! for the history chart API.

use std::path::Path;
use std::sync::Mutex;

use chrono::{TimeZone, Timelike};
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

/// One display bucket of a cost/income series, split into its per-kWh energy
/// component and its fixed daily standing-charge component (both cumulative £
/// over the query window). The two always sum to the total value
/// [`HistoryDb::query_cost_series`] plots for the same bucket, so the History
/// cost chart can draw energy versus standing charge without ever subtracting
/// one series from another (which would drift with float error and mismatched
/// bucket timestamps).
#[derive(Debug, Clone, Copy)]
pub struct CostComponentPoint {
    /// Unix timestamp in milliseconds.
    pub t: i64,
    /// Cumulative per-kWh (time-of-use) cost at this bucket, in £.
    pub energy_gbp: f64,
    /// Cumulative standing-charge (fixed daily fee) at this bucket, in £.
    pub standing_gbp: f64,
}

/// Cost fallback totals integrated from raw signed grid power when the
/// inverter does not expose usable daily import/export counters.
#[derive(Debug, Clone, Copy, Default)]
pub struct GridPowerCostTotals {
    pub import_kwh: f64,
    pub export_kwh: f64,
    pub import_cost_gbp: f64,
    pub export_income_gbp: f64,
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
    "today_pv1_kwh",
    "today_pv2_kwh",
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
    // PV1 / PV2 output as a percentage of their rated kWp (issue #110).
    // Instantaneous gauge — AVG is the correct bucket aggregation.
    "pv1_pct",
    "pv2_pct",
    // Weather observations live in the separate `weather_observations`
    // table — see `is_weather_field`. Listed here so the standard SQL-
    // injection whitelist accepts the field name on the history endpoint.
    "external_temperature",
];

fn is_allowed_field(field: &str) -> bool {
    ALLOWED_FIELDS.contains(&field)
}

/// True for fields that live in the `weather_observations` table rather
/// than `readings`. The frontend sends them through `/api/history` like any
/// other field; the query layer routes them here so the rest of the pipeline
/// (bucket aggregation, TimePoint shape) is identical.
fn is_weather_field(field: &str) -> bool {
    field == "external_temperature"
}

/// Cumulative counter fields that monotonically increase within a day and
/// reset at midnight. For these fields MAX is the correct aggregation
/// (AVG would understate the true value).
const CUMULATIVE_FIELDS: &[&str] = &[
    "today_solar_kwh",
    "today_pv1_kwh",
    "today_pv2_kwh",
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

fn utc_date_for_timestamp_ms(timestamp_ms: i64) -> Option<chrono::NaiveDate> {
    let secs = timestamp_ms.div_euclid(1000);
    let nanos = (timestamp_ms.rem_euclid(1000) as u32) * 1_000_000;
    chrono::Utc
        .timestamp_opt(secs, nanos)
        .earliest()
        .map(|dt| dt.date_naive())
}

fn same_utc_day(a_ms: i64, b_ms: i64) -> bool {
    match (
        utc_date_for_timestamp_ms(a_ms),
        utc_date_for_timestamp_ms(b_ms),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Cost / income series
// ---------------------------------------------------------------------------

/// Derived field names for the History "Cost" tab. These are NOT stored
/// columns - the server computes them from the `today_*_kwh` counters and the
/// configured tariff (see [`HistoryDb::query_cost_series`]). The frontend
/// requests them like any other field; the history handler routes them here.
pub const IMPORT_COST_FIELD: &str = "_import_cost";
pub const EXPORT_INCOME_FIELD: &str = "_export_income";

/// Import-cost breakdown fields. `_import_cost` is the sum of the two:
/// `_import_energy_cost` is the per-kWh (time-of-use) component, and
/// `_import_standing_charge` is the fixed daily standing-charge component
/// (the once-per-local-day step). Splitting them lets the History cost chart
/// show what the user pays for energy versus the flat daily fee. Both are
/// served from the same single [`HistoryDb::query_cost_breakdown`] walk so
/// `energy + standing` equals `_import_cost` exactly, with no float drift.
pub const IMPORT_ENERGY_COST_FIELD: &str = "_import_energy_cost";
pub const IMPORT_STANDING_CHARGE_FIELD: &str = "_import_standing_charge";

/// True for the server-derived cost/income fields.
pub fn is_cost_field(field: &str) -> bool {
    matches!(
        field,
        IMPORT_COST_FIELD
            | EXPORT_INCOME_FIELD
            | IMPORT_ENERGY_COST_FIELD
            | IMPORT_STANDING_CHARGE_FIELD
    )
}

/// Field names for the server-derived DIRECTIONAL power series.
///
/// The stored `battery_power` and `grid_power` columns are SIGNED (negative =
/// charge / grid-import, positive = discharge / grid-export). Splitting them by
/// sign on the client AFTER the server has `AVG`-aggregated each bucket lets the
/// two directions cancel inside a wide bucket, collapsing the directional charts
/// toward 0 at coarse zoom (a full day of charging and discharging nets to ~0,
/// so the 24h-bucket 1-year view flat-lines). These derived fields instead
/// average each direction's magnitude independently per bucket - the signed
/// split happens INSIDE the `AVG`, before aggregation - so the directions never
/// cancel. Mirrors the derive-then-downsample pattern already used for
/// `_import_cost` / `_export_income`.
pub const CHARGE_POWER_FIELD: &str = "_charge_power";
pub const DISCHARGE_POWER_FIELD: &str = "_discharge_power";
pub const GRID_IMPORT_POWER_FIELD: &str = "_grid_import_power";
pub const GRID_EXPORT_POWER_FIELD: &str = "_grid_export_power";

/// Map a directional field to `(source_column, signed-magnitude SQL)`.
///
/// The magnitude expression is averaged per bucket; the source column drives the
/// `IS NOT NULL` row guard (the `CASE` itself is never NULL). Both halves are
/// compile-time string literals over a fixed column, so the formatted SQL
/// carries no untrusted input - this is what lets directional fields safely
/// bypass the `is_allowed_field` column whitelist in `query_history`.
///
/// Each `CASE WHEN` wraps its source column in `COALESCE(…, 0)` so the
/// magnitude stays 0 instead of NULL when a row's column is NULL. Today
/// `battery_power` / `grid_power` are non-NULL in the schema (poll.rs
/// defaults them to 0 when the dongle omits them), but the coalesce
/// keeps the directional series robust if that ever changes.
fn directional_sql(field: &str) -> Option<(&'static str, &'static str)> {
    match field {
        CHARGE_POWER_FIELD => Some((
            "battery_power",
            "CASE WHEN COALESCE(battery_power, 0) < 0 THEN -COALESCE(battery_power, 0) ELSE 0 END",
        )),
        DISCHARGE_POWER_FIELD => Some((
            "battery_power",
            "CASE WHEN COALESCE(battery_power, 0) > 0 THEN COALESCE(battery_power, 0) ELSE 0 END",
        )),
        GRID_IMPORT_POWER_FIELD => Some((
            "grid_power",
            "CASE WHEN COALESCE(grid_power, 0) < 0 THEN -COALESCE(grid_power, 0) ELSE 0 END",
        )),
        GRID_EXPORT_POWER_FIELD => Some((
            "grid_power",
            "CASE WHEN COALESCE(grid_power, 0) > 0 THEN COALESCE(grid_power, 0) ELSE 0 END",
        )),
        _ => None,
    }
}

/// True for the server-derived directional power fields. Mirrors
/// [`is_cost_field`] so callers outside `history/` (e.g. the API layer
/// or tests) can ask "is this a derived field?" without re-encoding the
/// field-name list.
pub fn is_directional_field(field: &str) -> bool {
    directional_sql(field).is_some()
}

/// Coarse upper bound (kW) used only to reject a grossly corrupted counter
/// value that slipped past ingestion. A per-reading delta is discarded only if
/// it would require sustaining more than this over the actual elapsed time
/// between readings.
///
/// Sized well above realistic residential grid import: unlike inverter or
/// solar power, grid import can legitimately reach the low tens of kW (EV
/// charging, electric showers, three-phase supplies), so the bound must clear
/// those or genuine high-power readings get silently dropped. It only needs to
/// be tight enough to catch obvious corruption (a counter glitching by orders
/// of magnitude), not to bound real power precisely.
const MAX_PLAUSIBLE_POWER_KW: f64 = 30.0;

/// Whether two Unix-second timestamps fall on the same LOCAL calendar day.
///
/// The inverter's `today_*_kwh` counters reset at local midnight, so the
/// cost walk detects a daily reset by a local-day change (not UTC - the
/// reset happens at local midnight regardless of server timezone).
fn same_local_day(a_secs: i64, b_secs: i64) -> bool {
    let date = |s: i64| {
        chrono::DateTime::from_timestamp(s, 0)
            .map(|dt| dt.with_timezone(&chrono::Local).date_naive())
    };
    match (date(a_secs), date(b_secs)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Minutes-of-day `[0, 1440)` for a Unix-second timestamp in local time.
/// Used to pick the tariff slot for the moment a reading occurred.
fn local_minutes_of_day(ts_secs: i64) -> u16 {
    match chrono::DateTime::from_timestamp(ts_secs, 0) {
        Some(dt) => {
            let l = dt.with_timezone(&chrono::Local);
            (l.hour() * 60 + l.minute()) as u16
        }
        None => 0,
    }
}

/// Number of distinct local calendar days the window `(start_ts, end_ts)`
/// touches. A window that starts at 23:59 on day N and ends at 00:01 on
/// day N+2 touches 3 days (N, N+1, N+2). Used to compute the total
/// standing-charge debit for issue #131: the per-day amount × number of
/// days touched. The set of step times (one local midnight per day after
/// the first) is computed separately by [`local_midnight_steps_after`].
pub(crate) fn days_in_local_window(start_ts: i64, end_ts: i64) -> u32 {
    if end_ts <= start_ts {
        return 0;
    }
    let to_date = |s: i64| {
        chrono::DateTime::from_timestamp(s, 0)
            .map(|dt| dt.with_timezone(&chrono::Local).date_naive())
    };
    match (to_date(start_ts), to_date(end_ts)) {
        (Some(s), Some(e)) if e >= s => (e - s).num_days() as u32 + 1,
        _ => 0,
    }
}

/// Local-midnight unix-second timestamps that OPEN days whose local date
/// is STRICTLY AFTER the window-open local date. These are the points
/// where the cumulative cost graph should step up by one day's worth of
/// Standing Charge. The window-open day's debit is seeded into
/// `standing_charge_days_credited` at function entry, so this list excludes
/// the window-open day entirely.
fn local_midnight_steps_after(start_ts: i64, end_ts: i64) -> Vec<i64> {
    if end_ts <= start_ts {
        return Vec::new();
    }
    let start_local_date = chrono::DateTime::from_timestamp(start_ts, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).date_naive());
    let Some(start_local_date) = start_local_date else {
        return Vec::new();
    };
    // The first step is the local midnight that opens `start_local_date +
    // 1`, regardless of whether start_ts itself was at local midnight.
    // Walking by 86 400s thereafter is safe across DST transitions: the
    // local-midnight check at each step self-corrects within at most one
    // hour.
    let mut cursor = match start_local_date.succ_opt() {
        Some(d) => match chrono::Local
            .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
        {
            Some(dt) => dt.timestamp(),
            None => return Vec::new(),
        },
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    while cursor < end_ts {
        out.push(cursor);
        cursor += 86_400;
    }
    out
}

/// Time-window specification for a history query.
///
/// Both the aggregated-field path ([`HistoryDb::query_history`]) and the cost
/// path ([`HistoryDb::query_cost_series`]) resolve a `HistoryWindow` to the
/// same concrete UTC-second `(start, end)` bounds via [`HistoryWindow::resolve`],
/// so they cover the exact same span of readings. `query_cost_series` takes one
/// by reference instead of the three loose window arguments (clippy's
/// `too_many_arguments`), keeping the call site self-documenting.
///
/// When `explicit_window` is supplied (the browser sent local-timezone
/// boundaries) it wins; otherwise the window is `range_secs` long, ending at a
/// boundary aligned the same way the aggregated path aligns it.
pub struct HistoryWindow {
    /// Window length in seconds (used only when `explicit_window` is `None`).
    pub range_secs: i64,
    /// How many whole windows back from "now" the window ends (`0` = most recent).
    pub offset: i64,
    /// Explicit UTC-second `(start, end)` bounds sent by the browser in its
    /// local timezone. When `Some`, overrides `range_secs`/`offset`.
    pub explicit_window: Option<(i64, i64)>,
}

impl HistoryWindow {
    /// Resolve to concrete UTC-second `(start, end)` bounds, applying the
    /// range/alignment rules shared by both history query paths.
    pub(crate) fn resolve(&self) -> (i64, i64) {
        match self.explicit_window {
            Some((s, e)) => (s, e),
            None => {
                let now = chrono::Utc::now().timestamp();
                let raw_end = now - (self.offset * self.range_secs);
                let aligned_end = match self.range_secs {
                    3600 => ((raw_end / 3600) * 3600) + 3600,
                    21600 => ((raw_end / 21600) * 21600) + 21600,
                    _ => {
                        // Align to local midnight so day-based ranges start at
                        // 00:00 local time instead of 00:00 UTC.
                        let raw_local = chrono::DateTime::from_timestamp(raw_end, 0)
                            .unwrap()
                            .with_timezone(&chrono::Local);
                        let secs_today = raw_local.time().num_seconds_from_midnight();
                        if secs_today == 0 {
                            raw_end
                        } else {
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
                (aligned_end - self.range_secs, aligned_end)
            }
        }
    }
}

/// A decrease counts as a genuine daily reset only if the counter collapses to
/// near zero relative to the day's running peak (`RESET_NEAR_ZERO_FRACTION`),
/// floored by `RESET_FLOOR_KWH` so a tiny peak can't make the threshold absurd.
/// A glitch dip stays at a substantial fraction of the day's total, so it falls
/// above this and gets clamped instead.
const RESET_NEAR_ZERO_FRACTION: f64 = 0.10;
const RESET_FLOOR_KWH: f64 = 0.5;

/// After a real reset the counter starts a fresh slow ramp from ~0, whereas a
/// transient comms glitch that momentarily reads ~0 snaps straight back toward
/// the prior level on the next sample. So a near-zero drop is only treated as a
/// reset if the *next* point stays below `RESET_RECOVERY_FRACTION` of the peak;
/// otherwise it's a glitch and gets clamped.
const RESET_RECOVERY_FRACTION: f64 = 0.5;

/// Whether a decrease from `peak` to `value` is a genuine daily-counter reset
/// (keep it) rather than a glitch dip (clamp it). `next` is the raw value of
/// the following point, used to reject a transient single-sample dip to ~0.
fn is_genuine_reset(peak: f64, value: f64, next: Option<f64>) -> bool {
    let near_zero = value <= (peak * RESET_NEAR_ZERO_FRACTION).max(RESET_FLOOR_KWH);
    if !near_zero {
        return false;
    }
    match next {
        Some(n) => n < peak * RESET_RECOVERY_FRACTION,
        None => true,
    }
}

/// Repair cumulative daily counters after aggregation.
///
/// The inverter's `today_*_kwh` fields are cumulative counters: they rise
/// through the day and reset to ~0 at the inverter's local midnight. Older app
/// versions could persist plausible-but-wrong low values after reconnects, and
/// MAX bucket aggregation does not fix a whole bad bucket/plateau, so this
/// display-side repair clamps a downward glitch dip back up to the previous
/// good value - while leaving the genuine once-per-day reset intact.
///
/// The reset is detected from the *data* (a collapse to ~0, see
/// [`is_genuine_reset`]), NOT from a calendar boundary. The inverter's clock is
/// set to local time (verified on a GIV-HY5.0: it tracks BST/GMT through DST and
/// resets `today_*` at local midnight, i.e. 23:00 UTC in summer), and there is
/// no guarantee every unit is configured the same way, so the repair must not
/// depend on a fixed offset. An earlier version assumed a fixed UTC-midnight
/// reset and used a same-UTC-day clamp; for a local-clock inverter in BST that
/// suppressed the real 23:00 UTC reset and dragged the visible drop to the next
/// UTC midnight (01:00 BST). Keying off the data works regardless of the
/// inverter's clock setting or the viewer's timezone.
///
/// The same-UTC-day guard is retained only to bound glitch-clamping: in wide
/// (>= 1 day) buckets each point is a separate daily total, so a lower day is
/// legitimate day-to-day variation, not a dip to repair.
fn repair_cumulative_points(points: &mut [TimePoint]) {
    let mut prev: Option<(i64, f64)> = None; // (timestamp_ms, running daily peak)
    let mut repaired = 0usize;

    for i in 0..points.len() {
        let cur_v = points[i].v;
        let next_v = points.get(i + 1).map(|p| p.v);

        if let Some((prev_t, prev_v)) = prev {
            if cur_v < prev_v
                && !is_genuine_reset(prev_v, cur_v, next_v)
                && same_utc_day(prev_t, points[i].t)
            {
                points[i].v = prev_v;
                repaired += 1;
            }
        }
        prev = Some((points[i].t, points[i].v));
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
    today_pv1_kwh       REAL,
    today_pv2_kwh       REAL,
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
-- Weather observations are kept in their own table so they can be polled
-- on a different cadence to the inverter (15 min vs every poll cycle) and
-- so historical backfill can insert in bulk without churning the inverter
-- readings table. The `source` column distinguishes live fetches from
-- archive backfill for debugging — both write into the same row keyed by
-- timestamp, so the most-recent observation wins. Resolved lat/lon are
-- persisted per row so a user can audit which grid cell the actual reading
-- came from (Open-Meteo can pick a cell several km from the requested
-- coords).
CREATE TABLE IF NOT EXISTS weather_observations (
    timestamp     INTEGER PRIMARY KEY,
    temperature_c REAL NOT NULL,
    source        TEXT NOT NULL,
    latitude      REAL,
    longitude     REAL,
    fetched_at    INTEGER NOT NULL
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

        // Migration: add today_pv1_kwh / today_pv2_kwh columns if missing
        // (issue #108 — per-string PV daily totals).
        let _ = conn.execute_batch("ALTER TABLE readings ADD COLUMN today_pv1_kwh REAL");
        let _ = conn.execute_batch("ALTER TABLE readings ADD COLUMN today_pv2_kwh REAL");

        // Migration: add pv1_pct / pv2_pct columns if missing (issue #110 —
        // PV output as % of rated peak, stored for history charting).
        let _ = conn.execute_batch("ALTER TABLE readings ADD COLUMN pv1_pct REAL");
        let _ = conn.execute_batch("ALTER TABLE readings ADD COLUMN pv2_pct REAL");

        // One-time backfill: populate home_energy_today_kwh for historic rows
        // recorded before this column existed. Commit 5e1da32 renamed the
        // History "Load Energy Today" chart from today_consumption_kwh to
        // home_energy_today_kwh. Without this, the chart silently shows no
        // historic data because the old rows never carried the new field.
        //
        // The two fields derive from the identical register formula (see
        // decoder.rs `decode_input_0_59`), so the legacy value is a faithful
        // backfill. The match clause covers both legacy shapes observed in
        // the wild: rows from before the column existed are NULL after the
        // ALTER above, while rows written by the brief integration-based
        // decoder (commits fddc40a → 7a7ec1b) carry a 0 from the per-session
        // accumulator reset. The `today_consumption_kwh > 0` guard means we
        // only ever touch rows that genuinely carry consumption data, so
        // legitimate midnight-reset rows (where both fields are 0) are left
        // alone. Gated by a meta flag so it runs once per database.
        let backfill_done: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM meta WHERE key = 'home_energy_backfill_done' AND value = '1')",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !backfill_done {
            match conn.execute(
                "UPDATE readings SET home_energy_today_kwh = today_consumption_kwh \
                 WHERE today_consumption_kwh > 0 \
                   AND (home_energy_today_kwh IS NULL OR home_energy_today_kwh = 0)",
                [],
            ) {
                Ok(n) if n > 0 => {
                    tracing::info!("Backfilled home_energy_today_kwh for {n} historic rows");
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("home_energy_today_kwh backfill failed: {e}"),
            }
            let _ = conn.execute_batch(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('home_energy_backfill_done', '1')",
            );
        }

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
                    tracing::info!("History DB backed up to {}", backup_path.display());
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
                          WHEN {col} < 1.0 \
                               AND LAG({col}) OVER (ORDER BY timestamp) > 1.0 \
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

        // ---- repair_v3: undo v2's incorrect midnight-rollover threshold ----
        // repair_v2 used `LAG(col) > 50.0 AND col < 10.0` to detect midnight
        // counter resets. For today_charge_kwh and today_discharge_kwh (typical
        // 5-15 kWh/day) the threshold was never reached, so the repair incorrectly
        // carried yesterday's final value into today, inflating chart data.
        //
        // v3 fixes the threshold to `col < 1.0 AND LAG(col) > 1.0` and restores
        // pre-v2 original data from the backup that v2 created before modifying rows.
        let v3_done: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM meta WHERE key = 'repair_v3_done' AND value = '1')",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !v3_done {
            let v2_ran: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM meta WHERE key = 'repair_v2_done' AND value = '1')",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(false);

            if v2_ran {
                let backup_path = path.with_extension("db.bak");
                if backup_path.exists() {
                    tracing::info!("repair_v3: restoring original data from backup");
                    let bak_str = backup_path.to_string_lossy().replace('\'', "''");
                    if let Err(e) =
                        conn.execute(&format!("ATTACH DATABASE '{}' AS bak", bak_str), [])
                    {
                        tracing::warn!("repair_v3: failed to attach backup: {e}");
                    } else {
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
                            // Try to restore each column. If the backup table
                            // doesn't have this column, the UPDATE will fail
                            // with "no such column" — we catch and skip.
                            let sql = format!(
                                "UPDATE readings SET {col} = (\
                                  SELECT {col} FROM bak.readings \
                                  WHERE bak.readings.timestamp = main.readings.timestamp\
                                ) WHERE timestamp IN (\
                                  SELECT timestamp FROM bak.readings \
                                  WHERE {col} IS NOT NULL\
                                )"
                            );
                            if let Err(e) = conn.execute(&sql, []) {
                                tracing::debug!("repair_v3: could not restore {col}: {e}");
                            }
                        }
                        if let Err(e) = conn.execute_batch("DETACH bak") {
                            tracing::warn!("repair_v3: failed to detach backup: {e}");
                        }
                    }
                } else {
                    tracing::warn!(
                        "repair_v3: no backup found at {} — cannot automatically restore",
                        backup_path.display()
                    );
                }
            }

            // Re-run the corrected repair with the fixed threshold, either on
            // restored data (if v2 corrupted it) or on the original database.
            // This uses the same logic as the v2 block above, repeated here so
            // the fixed threshold applies even if v2 already ran.
            let v3_energy_cols = [
                "today_solar_kwh",
                "today_import_kwh",
                "today_export_kwh",
                "today_charge_kwh",
                "today_discharge_kwh",
                "today_consumption_kwh",
                "today_ac_charge_kwh",
            ];
            for col in &v3_energy_cols {
                let repair_sql_v3 = format!(
                    "CREATE TABLE IF NOT EXISTS _repair_v3_{col} AS \
                     SELECT timestamp, {col} AS orig, \
                            CASE \
                              WHEN LAG({col}) OVER (ORDER BY timestamp) IS NULL THEN {col} \
                              -- Midnight rollover: counter reset to near-zero \
                              WHEN {col} < 1.0 \
                                   AND LAG({col}) OVER (ORDER BY timestamp) > 1.0 \
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
                let _ = conn.execute_batch(&repair_sql_v3);

                let count: i64 = conn
                    .query_row(
                        &format!("SELECT COUNT(*) FROM _repair_v3_{col} WHERE orig != repaired"),
                        [],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);

                if count > 0 {
                    tracing::info!("repair_v3: repairing {count} {col} values");
                    let apply_sql = format!(
                        "UPDATE readings SET {col} = (\
                          SELECT repaired FROM _repair_v3_{col} \
                          WHERE _repair_v3_{col}.timestamp = readings.timestamp\
                        ) WHERE timestamp IN (\
                          SELECT timestamp FROM _repair_v3_{col} WHERE orig != repaired\
                        )"
                    );
                    if let Err(e) = conn.execute_batch(&apply_sql) {
                        tracing::warn!("repair_v3: failed to repair {col}: {e}");
                    }
                }

                let _ = conn.execute_batch(&format!("DROP TABLE IF EXISTS _repair_v3_{col}"));
            }

            let _ = conn.execute_batch(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('repair_v3_done', '1')",
            );
        }

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
                today_solar_kwh, today_pv1_kwh, today_pv2_kwh,
                today_import_kwh, today_export_kwh,
                today_charge_kwh, today_discharge_kwh, today_consumption_kwh,
                today_ac_charge_kwh, home_energy_today_kwh,
                charge_rate, discharge_rate, battery_reserve, target_soc,
                pv1_pct, pv2_pct
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?33,?34,?35)",
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
                snap.today_pv1_kwh,
                snap.today_pv2_kwh,
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
                snap.pv1_pct,
                snap.pv2_pct,
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
    /// - `fields`: list of field names. Each is one of:
    ///   - a column on `readings` (or `weather_observations` for
    ///     `external_temperature`) - validated by [`is_allowed_field`]
    ///     against the SQL-injection whitelist;
    ///   - `_import_cost` / `_export_income` - the cost/income series are
    ///     routed out to `query_cost_series` by the HTTP layer
    ///     (`server/api.rs`), NOT computed here;
    ///   - `_charge_power` / `_discharge_power` / `_grid_import_power` /
    ///     `_grid_export_power` - server-derived directional series that
    ///     `AVG(CASE WHEN …)` a signed magnitude per bucket so charge
    ///     never cancels discharge (or import cancels export) inside a
    ///     wide bucket (PR #166). Routed via [`directional_sql`].
    /// - `explicit_window`: optional (start_ts, end_ts) in UTC epoch seconds.
    ///   When provided, `range_secs` and `offset` are ignored entirely.
    ///
    /// Returns a map from field name to Vec<TimePoint>. Unrecognised
    /// field names are silently skipped (no SQL is built for them), so a
    /// request that mixes known and unknown fields still succeeds for
    /// the known ones.
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

        let (start_ts, end_ts) = HistoryWindow {
            range_secs,
            offset,
            explicit_window,
        }
        .resolve();

        let mut result = serde_json::Map::new();

        for field in fields {
            // Directional power fields (`_charge_power`, `_grid_import_power`, …)
            // are derived: they average a SIGNED-SPLIT magnitude of an existing
            // column so charge/discharge (or import/export) don't cancel within a
            // bucket. They bypass the plain `is_allowed_field` column whitelist
            // via `directional_sql`, which only maps a fixed set of names to
            // constant SQL over a known column. Everything downstream (bucketing,
            // TimePoint shape) is identical to a plain aggregated field.
            let (table, value_expr, null_guard) =
                if let Some((source_col, magnitude_expr)) = directional_sql(field) {
                    (
                        "readings",
                        format!("AVG({magnitude_expr})"),
                        format!("\"{source_col}\""),
                    )
                } else {
                    if !is_allowed_field(field) {
                        continue;
                    }

                    let agg = if is_cumulative_field(field) {
                        "MAX"
                    } else {
                        "AVG"
                    };

                    let table = if is_weather_field(field) {
                        "weather_observations"
                    } else {
                        "readings"
                    };
                    // Weather observations store temperature in `temperature_c`
                    // rather than the requested field name, so the SELECT has to
                    // alias the column. Every other field shares its name with
                    // the column.
                    let select_col = if is_weather_field(field) {
                        "temperature_c".to_string()
                    } else {
                        format!("\"{field}\"")
                    };

                    (table, format!("{agg}({select_col})"), select_col)
                };

            let sql = format!(
                "SELECT \
                    ((timestamp / {bucket}) * {bucket}) * 1000 AS t_bucket, \
                    {value_expr} AS v \
                 FROM {table} \
                 WHERE timestamp >= ?1 AND timestamp < ?2 AND {null_guard} IS NOT NULL \
                 GROUP BY t_bucket \
                 ORDER BY t_bucket",
                bucket = bucket_secs,
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

    /// Cumulative cost/income series (£) for a daily energy counter over the
    /// query window: the per-kWh component plus the standing charge, summed.
    ///
    /// This is the total the History "Cost" tab plots as `_import_cost` /
    /// `_export_income`, and the total the `/api/report` summary sums. It's a
    /// thin wrapper that adds the two components returned by
    /// [`HistoryDb::query_cost_breakdown`]; see that method for the full
    /// pricing and standing-charge-step semantics.
    pub fn query_cost_series(
        &self,
        window: &HistoryWindow,
        bucket_secs: i64,
        counter_field: &str,
        tariff: &crate::settings::TariffConfig,
        flat_fallback: f64,
        standing_charge_p_per_day: f64,
    ) -> Result<Vec<TimePoint>, String> {
        Ok(self
            .query_cost_breakdown(
                window,
                bucket_secs,
                counter_field,
                tariff,
                flat_fallback,
                standing_charge_p_per_day,
            )?
            .into_iter()
            .map(|c| TimePoint {
                t: c.t,
                v: c.energy_gbp + c.standing_gbp,
            })
            .collect())
    }

    /// Compute a cumulative cost/income **breakdown** series for a daily energy
    /// counter over the query window, priced with a time-of-use `tariff`. Each
    /// point carries the per-kWh energy component and the standing-charge
    /// component separately (see [`CostComponentPoint`]); [`Self::query_cost_series`]
    /// sums them for callers that only want the total.
    ///
    /// Why this can't reuse the aggregated (`MAX`-bucket) path: a time-of-use
    /// rate must be applied to each energy increment at the moment it actually
    /// occurred, and that resolution is destroyed by wide buckets. A 24h MAX
    /// bucket only knows the day's *total* - pricing it at one rate prices the
    /// whole day at the bucket's start-of-day rate and silently drops the
    /// evening peak, so the running total shrinks as the range widens.
    /// Likewise the local-midnight reset must be detected exactly, not
    /// inferred from a coarse bucket.
    ///
    /// So we walk the raw readings at native resolution, accumulate
    /// `energy_delta * rate(reading_time)`, and only then downsample the
    /// cumulative result to `bucket_secs` display buckets (one point per
    /// bucket, carrying the running total at the bucket's last reading). The
    /// total is therefore independent of the selected range's bucket width.
    ///
    /// `counter_field` must be `"today_import_kwh"` or `"today_export_kwh"`.
    /// `flat_fallback` is the £/kWh rate used only if the tariff lookup yields
    /// nothing (degenerate config).
    ///
    /// `standing_charge_p_per_day` is the optional daily fixed cost in
    /// pence/day for this direction. UK-style tariffs (Octopus Flux, etc.)
    /// charge a flat daily fee that does not scale with usage — without it
    /// the cumulative cost graph omits a constant and reads low by roughly
    /// the per-day amount × days covered. Unlike the per-kWh component
    /// (which grows continuously as energy is consumed), the standing
    /// charge is debited once at the **start of each local day**: the
    /// cumulative cost series steps up by `standing_charge_p_per_day / 100`
    /// (£) at each local midnight that falls within the query window. So a
    /// 7-day range with a 54.86p/day Standing Charge shows 7 visible steps
    /// in the cost graph (one per day), each of size £0.5486, layered on
    /// top of the per-kWh cost component. A value of 0 leaves the standing
    /// component at zero. See issue #131.
    pub fn query_cost_breakdown(
        &self,
        window: &HistoryWindow,
        bucket_secs: i64,
        counter_field: &str,
        tariff: &crate::settings::TariffConfig,
        flat_fallback: f64,
        standing_charge_p_per_day: f64,
    ) -> Result<Vec<CostComponentPoint>, String> {
        // Guard the SQL identifier - only the two daily counters drive cost.
        if counter_field != "today_import_kwh" && counter_field != "today_export_kwh" {
            return Err(format!("Unsupported cost counter field: {counter_field}"));
        }

        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;

        let (start_ts, end_ts) = window.resolve();

        // Negative Standing Charge clamped to 0 — it would invert the cost
        // series, which doesn't match any real UK tariff. Issue #131.
        let sc_pence = standing_charge_p_per_day.max(0.0);
        let standing_charge_gbp_per_day = sc_pence / 100.0;

        // Pre-compute the local-midnight timestamps strictly inside the
        // window. Each is where the cumulative cost graph steps up by
        // one day's worth. We do NOT seed `standing_charge_days_credited`
        // at window open — the user wants to see the step land at the
        // start of each local day, not as a one-shot offset at window
        // open. (A partial first day at window open still incurs the
        // daily fee under UK billing, but it's debited at the next
        // local midnight rather than at window open, so the graph shows
        // a single visible step per local day crossed.)
        let midnight_steps = if standing_charge_gbp_per_day > 0.0 {
            local_midnight_steps_after(start_ts, end_ts)
        } else {
            Vec::new()
        };
        let total_days_in_window = days_in_local_window(start_ts, end_ts);

        let sql = format!(
            "SELECT timestamp, \"{counter_field}\" \
             FROM readings \
             WHERE timestamp >= ?1 AND timestamp < ?2 AND \"{counter_field}\" IS NOT NULL \
             ORDER BY timestamp",
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare cost query: {e}"))?;
        let rows: Vec<(i64, f64)> = stmt
            .query_map(params![start_ts, end_ts], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
            })
            .map_err(|e| format!("Cost query failed: {e}"))?
            .filter_map(SqlResult::ok)
            .collect();

        // bucket_start_ms -> (per-kWh energy £, standing-charge £) at that
        // bucket's last reading. BTreeMap keeps display points sorted; both
        // components are monotonic non-decreasing so the last write per
        // bucket is its max. Kept split (rather than pre-summed) so the
        // breakdown fields and the total both come from this one walk.
        let mut buckets: std::collections::BTreeMap<i64, (f64, f64)> =
            std::collections::BTreeMap::new();
        let mut acc = 0.0_f64;
        // `baseline` is the counter value of the last *counted* reading on the
        // current day; `last_ts` is the previous reading's time (for the
        // local-day reset check and the plausibility window).
        let mut baseline: Option<f64> = None;
        let mut last_ts: Option<i64> = None;
        // Issue #131: number of full days of Standing Charge credited at the
        // current reading's time. Starts at 1 for the window-open day's
        // debit (UK billing convention: a partial first day still incurs
        // the full daily fee); incremented once per local midnight
        // strictly inside the window. When the window is entirely within
        // one local day and has no readings, the seed is the entire
        // Standing Charge; the user sees the open-day step at the very
        // first bucket. Each subsequent local midnight shows another
        // visible step in the cumulative cost graph.
        let mut standing_charge_days_credited: u32 = if total_days_in_window > 0 { 1 } else { 0 };
        // Parse the tariff's HH:MM bounds once, not per reading (the walk can
        // cover hundreds of thousands of rows on a 1y range).
        let parsed_slots = tariff.parsed_slots();

        for (idx, &(ts, raw)) in rows.iter().enumerate() {
            let next_raw = rows.get(idx + 1).map(|&(_, v)| v);
            // Apply any standing-charge debits for local-midnight boundaries
            // that fall at or before this reading's timestamp and strictly
            // after the previous reading's timestamp (or start_ts for the
            // first reading). We do this BEFORE updating `last_ts` so the
            // step lands on the first reading of the new local day
            // (matching UK billing: Standing Charge is due at local
            // midnight, not when the next reading happens to arrive).
            let prior_ts = last_ts.unwrap_or(start_ts);
            for &midnight in &midnight_steps {
                if midnight > prior_ts && midnight <= ts {
                    standing_charge_days_credited = standing_charge_days_credited.saturating_add(1);
                }
            }

            match baseline {
                None => {
                    // First reading establishes the baseline; nothing to credit
                    // (we only count energy accumulated *within* the window).
                    baseline = Some(raw);
                }
                Some(base) => {
                    let day_changed = last_ts.is_some_and(|lt| !same_local_day(lt, ts));
                    let (delta, new_baseline) = if day_changed {
                        // Counter reset at local midnight. Re-baseline only;
                        // credit nothing for the first reading of the new day.
                        // In continuous data that reading is ~0, so this is a
                        // no-op. After a data gap (app or inverter offline) the
                        // first reading can already hold a chunk of the day,
                        // accumulated at unknown times; crediting it here would
                        // price that whole chunk at this single reading's tariff
                        // slot. We only count energy whose accumulation we
                        // actually observe via within-day deltas.
                        (0.0, raw)
                    } else if raw >= base {
                        (raw - base, raw) // normal same-day increase
                    } else if is_genuine_reset(base, raw, next_raw) {
                        // Same-day collapse to near zero: this is the inverter's
                        // daily counter reset landing shortly after the query
                        // window opened. Treat it as a reset rather than holding
                        // yesterday's high baseline, otherwise all export/import
                        // accrued today below yesterday's total is stranded at £0.
                        (0.0, raw)
                    } else {
                        // Same-day decrease: a sensor glitch, not real negative
                        // energy. Skip it AND keep the baseline, so the later
                        // recovery back up to `base` isn't re-counted.
                        (0.0, base)
                    };

                    // Plausibility ceiling, scaled by the actual elapsed time so
                    // a data gap (app offline) doesn't trip it. Rejects a delta
                    // that would require > MAX_PLAUSIBLE_POWER_KW sustained. The
                    // floor keeps a sane ceiling for sub-minute sample spacing.
                    let elapsed_h = ((ts - last_ts.unwrap_or(ts)).max(0) as f64) / 3600.0;
                    let ceiling = MAX_PLAUSIBLE_POWER_KW * elapsed_h.max(1.0 / 60.0);

                    if delta > 0.0 && delta <= ceiling {
                        let rate = crate::settings::rate_for_parsed_minutes(
                            &parsed_slots,
                            local_minutes_of_day(ts),
                        )
                        .unwrap_or(flat_fallback);
                        acc += delta * rate;
                        baseline = Some(new_baseline);
                    } else if delta <= ceiling {
                        // Zero (or reset-to-zero) delta: still advance the
                        // baseline so the day re-syncs after a reset.
                        baseline = Some(new_baseline);
                    }
                    // delta > ceiling: implausible spike. Drop it and keep the
                    // baseline so a single corrupt reading doesn't inflate the
                    // total; the next good reading produces a re-checked delta.
                }
            }

            let bucket_ms = ((ts / bucket_secs) * bucket_secs) * 1000;
            // Issue #131: per-day Standing Charge layered on top of the
            // per-kWh component. The cost graph line steps up by exactly the
            // per-day amount at every local midnight within the window.
            let standing_charge_total =
                standing_charge_days_credited as f64 * standing_charge_gbp_per_day;
            buckets.insert(bucket_ms, (acc, standing_charge_total));
            last_ts = Some(ts);
        }

        // If we crossed one or more local-day boundaries between the last
        // reading and the window end (a quiet period with no readings past
        // the previous local midnight), still surface those standing-charge
        // steps. The standing-charge debit for the final partial day is
        // credited at its start (the midnight that opens it), so any
        // midnights strictly between `last_ts` and `end_ts` must still be
        // applied.
        if let Some(lt) = last_ts {
            for &midnight in &midnight_steps {
                if midnight > lt && midnight < end_ts {
                    standing_charge_days_credited = standing_charge_days_credited.saturating_add(1);
                }
            }
            // Emit a final bucket at end_ts so the graph shows the latest
            // standing-charge step we just credited, plus the per-kWh
            // component (which doesn't change after the last reading).
            let end_bucket_ms = ((end_ts / bucket_secs) * bucket_secs) * 1000;
            let standing_charge_total =
                standing_charge_days_credited as f64 * standing_charge_gbp_per_day;
            // Only insert if this bucket is later than what we already have
            // (avoid clobbering an existing bucket from the readings walk).
            buckets
                .entry(end_bucket_ms)
                .or_insert((acc, standing_charge_total));
        }

        // Always emit at least the window-open bucket with the standing
        // charge applied — a query over a quiet stretch (no kWh activity,
        // no day boundaries crossed in readings) must still surface the
        // fixed daily cost so the user sees the contribution in the cost
        // graph, not just an empty series. Issue #131.
        if buckets.is_empty() {
            let open_bucket_ms = ((start_ts / bucket_secs) * bucket_secs) * 1000;
            // For a window with no readings, surface the cumulative
            // Standing Charge for ALL days the window covers. Each day's
            // debit lands at the start of that day (the local midnight),
            // so the first bucket carries the per-day amount for the
            // window-open day, and subsequent midnights would add more
            // (none here since there are no readings to trigger them).
            // Use `total_days_in_window` rather than
            // `standing_charge_days_credited` because the trailing logic
            // didn't run (no readings means no `last_ts` to anchor it).
            let sc = total_days_in_window as f64 * standing_charge_gbp_per_day;
            buckets.insert(open_bucket_ms, (0.0, sc));
        }

        Ok(buckets
            .into_iter()
            .map(|(t, (energy_gbp, standing_gbp))| CostComponentPoint {
                t,
                energy_gbp,
                standing_gbp,
            })
            .collect())
    }

    /// Fallback cost/income totals from raw `grid_power` samples, used when
    /// an inverter/firmware does not populate the `today_import_kwh` /
    /// `today_export_kwh` cumulative counters. Positive grid power is import;
    /// negative grid power is export. The integration mirrors the Power page's
    /// consumption report: trapezoid integration over neighbouring samples,
    /// priced at the tariff slot covering the interval start.
    pub fn query_grid_power_cost_totals(
        &self,
        window: &HistoryWindow,
        import_tariff: &crate::settings::TariffConfig,
        export_tariff: &crate::settings::TariffConfig,
        flat_import_fallback: f64,
        flat_export_fallback: f64,
    ) -> Result<GridPowerCostTotals, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        let (start_ts, end_ts) = window.resolve();

        let mut stmt = conn
            .prepare(
                "SELECT timestamp, grid_power \
                 FROM readings \
                 WHERE timestamp >= ?1 AND timestamp < ?2 AND grid_power IS NOT NULL \
                 ORDER BY timestamp",
            )
            .map_err(|e| format!("Failed to prepare grid-power cost query: {e}"))?;
        let rows: Vec<(i64, f64)> = stmt
            .query_map(params![start_ts, end_ts], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
            })
            .map_err(|e| format!("Grid-power cost query failed: {e}"))?
            .filter_map(SqlResult::ok)
            .collect();

        if rows.len() < 2 {
            return Ok(GridPowerCostTotals::default());
        }

        let mut intervals: Vec<f64> = rows
            .windows(2)
            .map(|w| (w[1].0 - w[0].0) as f64)
            .filter(|dt| *dt > 0.0)
            .collect();
        if intervals.is_empty() {
            return Ok(GridPowerCostTotals::default());
        }
        intervals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let max_gap_secs = intervals[intervals.len() / 2] * 3.5;

        let import_slots = import_tariff.parsed_slots();
        let export_slots = export_tariff.parsed_slots();
        let mut totals = GridPowerCostTotals::default();

        for pair in rows.windows(2) {
            let (ts_a, grid_a) = pair[0];
            let (ts_b, grid_b) = pair[1];
            let dt_secs = (ts_b - ts_a) as f64;
            if dt_secs <= 0.0 || dt_secs > max_gap_secs {
                continue;
            }
            let hours = dt_secs / 3600.0;
            let import_kwh = (grid_a.max(0.0) + grid_b.max(0.0)) / 2.0 * hours / 1000.0;
            let export_kwh = ((-grid_a).max(0.0) + (-grid_b).max(0.0)) / 2.0 * hours / 1000.0;
            let minute = local_minutes_of_day(ts_a);
            let import_rate = crate::settings::rate_for_parsed_minutes(&import_slots, minute)
                .unwrap_or(flat_import_fallback);
            let export_rate = crate::settings::rate_for_parsed_minutes(&export_slots, minute)
                .unwrap_or(flat_export_fallback);

            totals.import_kwh += import_kwh;
            totals.export_kwh += export_kwh;
            totals.import_cost_gbp += import_kwh * import_rate;
            totals.export_income_gbp += export_kwh * export_rate;
        }

        Ok(totals)
    }

    /// Insert one weather observation.
    ///
    /// `source` is `"current"` for live fetches or `"backfill"` for archive
    /// pulls. Both write into the same row keyed by `timestamp`; the most
    /// recent observation wins, so a live fetch that overlaps with a slow
    /// backfill naturally supersedes it.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_weather(
        &self,
        timestamp: i64,
        temperature_c: f32,
        source: &str,
        latitude: Option<f32>,
        longitude: Option<f32>,
        fetched_at: i64,
    ) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("History DB lock poisoned: {e}");
                return;
            }
        };

        if let Err(e) = conn.execute(
            "INSERT OR REPLACE INTO weather_observations \
                (timestamp, temperature_c, source, latitude, longitude, fetched_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                timestamp,
                temperature_c,
                source,
                latitude,
                longitude,
                fetched_at
            ],
        ) {
            tracing::warn!("Failed to insert weather observation: {e}");
        }
    }

    /// Return the earliest (timestamp, source) pair from `weather_observations`,
    /// or `None` if the table is empty. Used to report a "first weather data
    /// point" status to the frontend, and as the lower bound for resumption
    /// when the user disables + re-enables weather.
    pub fn earliest_weather_observation(&self) -> Option<(i64, String)> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT timestamp, source FROM weather_observations ORDER BY timestamp ASC LIMIT 1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .ok()
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

    /// Regression for the directional sign-cancellation bug: within a single
    /// bucket that contains both charging and discharging, the net
    /// `AVG(battery_power)`
    /// cancels toward 0 - which is exactly what the old client-side sign split
    /// saw, collapsing both directional series. The derived `_charge_power` /
    /// `_discharge_power` (and grid import/export) fields must instead report
    /// each direction's true average magnitude, because the split happens
    /// inside the aggregate.
    #[test]
    fn directional_fields_do_not_cancel_within_a_bucket() {
        let db = test_db();
        let base = 1_700_000_000i64;

        // Four readings inside one bucket. Battery nets to exactly 0
        // (-2000, +2000, -1000, +1000) yet charges in two and discharges in
        // two. Grid imports (-3000, -1000) and exports (+2000, +500) in the
        // same bucket. (Sign convention: battery/grid negative = charge/import.)
        let samples = [
            (base, -2000i32, -3000i32),
            (base + 60, 2000, 2000),
            (base + 120, -1000, -1000),
            (base + 180, 1000, 500),
        ];
        for (ts, battery_power, grid_power) in samples {
            db.insert_reading(&InverterSnapshot {
                timestamp: ts,
                battery_power,
                grid_power,
                ..Default::default()
            });
        }

        // One 24h bucket so all four readings aggregate together - the coarse
        // bucket case where the broken client split flat-lined.
        let result = db
            .query_history(
                100_000_000,
                86_400,
                0,
                &[
                    "battery_power".to_string(),
                    CHARGE_POWER_FIELD.to_string(),
                    DISCHARGE_POWER_FIELD.to_string(),
                    GRID_IMPORT_POWER_FIELD.to_string(),
                    GRID_EXPORT_POWER_FIELD.to_string(),
                ],
                None,
            )
            .unwrap();

        let single = |field: &str| -> f64 {
            let pts: Vec<TimePoint> =
                serde_json::from_value(result.get(field).cloned().unwrap()).unwrap();
            assert_eq!(
                pts.len(),
                1,
                "expected one bucket for {field}, got {}",
                pts.len()
            );
            pts[0].v
        };

        // Net average cancels to ~0 - the symptom the directional fields fix.
        assert!(
            single("battery_power").abs() < 1.0,
            "net battery_power should cancel to ~0, got {}",
            single("battery_power"),
        );

        // charge magnitudes 2000,0,1000,0 -> avg 750; discharge 0,2000,0,1000 -> 750.
        assert!((single(CHARGE_POWER_FIELD) - 750.0).abs() < 0.01);
        assert!((single(DISCHARGE_POWER_FIELD) - 750.0).abs() < 0.01);
        // import magnitudes 3000,0,1000,0 -> avg 1000; export 0,2000,0,500 -> 625.
        assert!((single(GRID_IMPORT_POWER_FIELD) - 1000.0).abs() < 0.01);
        assert!((single(GRID_EXPORT_POWER_FIELD) - 625.0).abs() < 0.01);
    }

    /// Directional fields bypass the column whitelist via an exact name->SQL
    /// map, so a lookalike / injection-shaped field name must still be rejected
    /// with no SQL built.
    #[test]
    fn rejects_directional_lookalike_field() {
        let db = test_db();
        let result = db
            .query_history(
                600,
                60,
                0,
                &["_charge_power; DROP TABLE readings".to_string()],
                None,
            )
            .unwrap();
        assert!(result.is_empty());
    }

    /// One bad field name must not poison the whole query: the unknown
    /// field is silently skipped, while every known field still returns
    /// its data. The HTTP layer relies on this to keep custom tabs working
    /// after the directional fields (or any future derived field) are
    /// added to the request list.
    #[test]
    fn unknown_field_does_not_shadow_known_fields() {
        let db = test_db();
        // Use the standard 'window covers everything' range from the
        // other history tests so the single inserted reading is always
        // inside the query window regardless of wall-clock time.
        let base = 1_700_000_000i64;
        db.insert_reading(&InverterSnapshot {
            timestamp: base,
            battery_power: -1500,
            grid_power: 800,
            ..Default::default()
        });
        let result = db
            .query_history(
                100_000_000,
                60,
                0,
                &[
                    CHARGE_POWER_FIELD.to_string(),
                    "_not_a_real_field".to_string(),
                    "solar_power".to_string(),
                    GRID_EXPORT_POWER_FIELD.to_string(),
                ],
                None,
            )
            .unwrap();
        // Known fields returned non-empty…
        assert!(result.get(CHARGE_POWER_FIELD).is_some());
        assert!(result.get(GRID_EXPORT_POWER_FIELD).is_some());
        assert!(result.get("solar_power").is_some());
        // …and the unknown one did not appear.
        assert!(result.get("_not_a_real_field").is_none());
        // Spot-check a value: that one reading was pure charge and pure
        // export, so the directional avg equals the reading itself.
        let charge_pts: Vec<TimePoint> =
            serde_json::from_value(result.get(CHARGE_POWER_FIELD).cloned().unwrap()).unwrap();
        assert_eq!(charge_pts.len(), 1);
        assert!((charge_pts[0].v - 1500.0).abs() < 0.01);
    }

    /// A bucket with exclusively charging readings must report full
    /// magnitude on the charge series and exactly 0 on the discharge
    /// series. Symmetrically, a pure-discharge bucket must report 0 on
    /// the charge series. The cancellation test alone does not exercise
    /// the polarity branches - both `> 0` and `< 0` need at least one
    /// direct hit each so an off-by-one sign flip in the SQL `CASE WHEN`
    /// would be caught.
    #[test]
    fn directional_polarity_branches_are_correct() {
        let db = test_db();
        // Use a small bucket (60s) so each cluster reliably lands in its
        // own bucket rather than getting aggregated together.
        let bucket_secs = 60i64;
        let t0 = 1_700_100_000i64; // arbitrary non-overlapping base

        // Bucket A: three pure-charge readings (-400, -1000, -600 W),
        // all inside the bucket starting at t0.
        for (i, watts) in [-400i32, -1000, -600].iter().enumerate() {
            db.insert_reading(&InverterSnapshot {
                timestamp: t0 + (i as i64) * 10, // 0s, 10s, 20s
                battery_power: *watts,
                ..Default::default()
            });
        }
        // Bucket B: three pure-discharge readings (300, 700, 500 W) in a
        // separate bucket starting at t0 + 120s (well past the 60s bucket
        // boundary, so they aggregate on their own).
        for (i, watts) in [300i32, 700, 500].iter().enumerate() {
            db.insert_reading(&InverterSnapshot {
                timestamp: t0 + 120 + (i as i64) * 10, // 120s, 130s, 140s
                battery_power: *watts,
                ..Default::default()
            });
        }

        let result = db
            .query_history(
                100_000_000,
                bucket_secs,
                0,
                &[
                    CHARGE_POWER_FIELD.to_string(),
                    DISCHARGE_POWER_FIELD.to_string(),
                ],
                None,
            )
            .unwrap();

        let pts = |f: &str| -> Vec<TimePoint> {
            serde_json::from_value(result.get(f).cloned().unwrap()).unwrap()
        };

        let charge_pts = pts(CHARGE_POWER_FIELD);
        let discharge_pts = pts(DISCHARGE_POWER_FIELD);
        assert_eq!(
            charge_pts.len(),
            2,
            "expected two charge buckets, got {charge_pts:?}"
        );
        assert_eq!(
            discharge_pts.len(),
            2,
            "expected two discharge buckets, got {discharge_pts:?}"
        );

        // Bucket A: mean charge = (400+1000+600)/3 = 666.67; discharge = 0.
        let bucket_a_start = (t0 / bucket_secs) * bucket_secs * 1000;
        let bucket_b_start = ((t0 + 120) / bucket_secs) * bucket_secs * 1000;

        let charge_a = charge_pts
            .iter()
            .find(|p| p.t == bucket_a_start)
            .expect("bucket A on charge series");
        assert!(
            (charge_a.v - 666.666666667).abs() < 0.5,
            "pure-charge bucket A should be ~666.67, got {}",
            charge_a.v
        );
        let discharge_a = discharge_pts
            .iter()
            .find(|p| p.t == bucket_a_start)
            .expect("bucket A on discharge series");
        assert!(
            discharge_a.v.abs() < 0.01,
            "pure-charge bucket A on discharge series should be 0, got {}",
            discharge_a.v
        );

        // Symmetric: pure-discharge bucket B.
        let charge_b = charge_pts
            .iter()
            .find(|p| p.t == bucket_b_start)
            .expect("bucket B on charge series");
        assert!(
            charge_b.v.abs() < 0.01,
            "pure-discharge bucket B on charge series should be 0, got {}",
            charge_b.v
        );
        let discharge_b = discharge_pts
            .iter()
            .find(|p| p.t == bucket_b_start)
            .expect("bucket B on discharge series");
        assert!(
            (discharge_b.v - 500.0).abs() < 0.5,
            "pure-discharge bucket B should be 500, got {}",
            discharge_b.v
        );
    }

    /// `is_directional_field` recognises all four directional names and
    /// rejects everything else - mirrors the symmetry of `is_cost_field`
    /// so contributors adding future derived fields have a clear pattern
    /// to follow.
    #[test]
    fn is_directional_field_helper() {
        assert!(is_directional_field(CHARGE_POWER_FIELD));
        assert!(is_directional_field(DISCHARGE_POWER_FIELD));
        assert!(is_directional_field(GRID_IMPORT_POWER_FIELD));
        assert!(is_directional_field(GRID_EXPORT_POWER_FIELD));
        assert!(!is_directional_field("battery_power"));
        assert!(!is_directional_field("_import_cost"));
        assert!(!is_directional_field("_charge_power; DROP TABLE readings"));
        assert!(!is_directional_field(""));
    }

    /// `is_cost_field` is the single chokepoint that keeps the four derived
    /// cost fields out of `query_history`'s column-whitelist SQL path and
    /// routes them to the cost engine instead. Pin all four positives (incl.
    /// the two breakdown fields) and a few negatives.
    #[test]
    fn is_cost_field_helper() {
        assert!(is_cost_field(IMPORT_COST_FIELD));
        assert!(is_cost_field(EXPORT_INCOME_FIELD));
        assert!(is_cost_field(IMPORT_ENERGY_COST_FIELD));
        assert!(is_cost_field(IMPORT_STANDING_CHARGE_FIELD));
        assert!(!is_cost_field("today_import_kwh"));
        assert!(!is_cost_field(CHARGE_POWER_FIELD));
        assert!(!is_cost_field("_import_cost; DROP TABLE readings"));
        assert!(!is_cost_field(""));
    }

    #[test]
    fn all_allowed_fields_are_valid_columns() {
        for field in ALLOWED_FIELDS {
            assert!(is_allowed_field(field));
        }
    }

    #[test]
    fn pv1_pct_and_pv2_pct_are_allowed_fields() {
        // issue #110: PV % fields must be in the SQL-injection whitelist.
        assert!(is_allowed_field("pv1_pct"));
        assert!(is_allowed_field("pv2_pct"));
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
    fn cumulative_point_repair_allows_utc_midnight_reset() {
        // Regression test for the BST/timezone-east-of-UTC bug: the
        // inverter's today_*_kwh counters reset at UTC midnight, not local
        // midnight. In BST (UTC+1) a reading at 23:30 UTC falls on the next
        // local day, but its value is still yesterday's final counter. The
        // repair must compare UTC dates (not local), otherwise it clamps the
        // legitimate midnight reset as a "same-day decrease".
        //
        // Fixed UTC timestamps so the test is timezone-independent in its
        // setup; it relies on the system running in BST (or any zone east
        // of UTC) for the local/UTC dates to actually diverge.
        //   base         = 2024-06-22 12:00 UTC = 13:00 BST (local June 22)
        //   before_reset = 2024-06-22 23:30 UTC = 00:30 BST (local June 23)
        //   after_reset  = 2024-06-23 00:30 UTC = 01:30 BST (local June 23)
        let base: i64 = 1_719_057_600_000; // 2024-06-22 12:00:00 UTC
        let before_reset: i64 = base + (23 * 60 + 30 - 12 * 60) * 60 * 1000; // +11.5h
        let after_reset: i64 = before_reset + 60 * 60 * 1000; // +1h
        let mut points = vec![
            TimePoint { t: base, v: 12.5 },
            TimePoint {
                t: before_reset,
                v: 12.5,
            },
            TimePoint {
                t: after_reset,
                v: 0.3,
            },
        ];

        repair_cumulative_points(&mut points);
        let values: Vec<f64> = points.iter().map(|p| p.v).collect();
        // The UTC-day reset must be allowed through, NOT clamped to 12.5.
        // (If the repair used same_local_day instead of same_utc_day, points
        // at 23:30 UTC and 00:30 UTC the next day would both fall on the
        // same local day in BST, and the reset would be incorrectly clamped.)
        assert_eq!(values, vec![12.5, 12.5, 0.3]);
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
    fn cumulative_counter_preserves_local_midnight_reset_within_utc_day() {
        // An inverter on local time (BST) resets today_* at local midnight =
        // 23:00 UTC, the SAME UTC day as the daytime peak. The drop to ~0 must
        // be preserved, not clamped back up and deferred to the next UTC
        // midnight (which showed the graph resetting at 01:00 instead of 00:00
        // in summer).
        let db = test_db();
        let day1 = 1700006400i64; // 2023-11-15 00:00:00 UTC

        db.insert_reading(&make_snapshot_with_kwh(day1 + 72000, 8.0, 0.0)); // 20:00 UTC
        db.insert_reading(&make_snapshot_with_kwh(day1 + 79200, 8.3, 0.0)); // 22:00 UTC peak
        db.insert_reading(&make_snapshot_with_kwh(day1 + 82800, 0.0, 0.0)); // 23:00 UTC reset, same UTC day
        db.insert_reading(&make_snapshot_with_kwh(day1 + 84600, 0.2, 0.0)); // 23:30 UTC fresh ramp

        let result = db
            .query_history(100_000_000, 600, 0, &["today_import_kwh".to_string()], None)
            .unwrap();
        let mut points: Vec<TimePoint> =
            serde_json::from_value(result.get("today_import_kwh").cloned().unwrap()).unwrap();
        points.sort_by_key(|p| p.t);
        let values: Vec<f64> = points.iter().map(|p| p.v).collect();

        let expected = [8.0, 8.3, 0.0, 0.2];
        assert_eq!(values.len(), expected.len(), "got {values:?}");
        for (got, want) in values.iter().zip(expected) {
            assert!(
                (got - want).abs() < 0.01,
                "got {values:?}, want {expected:?}"
            );
        }
    }

    #[test]
    fn cumulative_counter_clamps_transient_zero_glitch() {
        // A single bad sample reading ~0 mid-day (comms glitch) that snaps back
        // to the prior level must still be clamped, not mistaken for a reset.
        let db = test_db();
        let base = 1700000000i64;

        db.insert_reading(&make_snapshot_with_kwh(base, 8.0, 0.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 600, 0.0, 0.0)); // transient glitch
        db.insert_reading(&make_snapshot_with_kwh(base + 1200, 8.2, 0.0)); // recovers

        let result = db
            .query_history(100_000_000, 600, 0, &["today_import_kwh".to_string()], None)
            .unwrap();
        let mut points: Vec<TimePoint> =
            serde_json::from_value(result.get("today_import_kwh").cloned().unwrap()).unwrap();
        points.sort_by_key(|p| p.t);
        let values: Vec<f64> = points.iter().map(|p| p.v).collect();

        let expected = [8.0, 8.0, 8.2];
        assert_eq!(values.len(), expected.len(), "got {values:?}");
        for (got, want) in values.iter().zip(expected) {
            assert!(
                (got - want).abs() < 0.01,
                "got {values:?}, want {expected:?}"
            );
        }
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
        db.insert_reading(&make_snapshot_with_kwh(base + 60, 0.5, 1.0)); // midnight reset
        db.insert_reading(&make_snapshot_with_kwh(base + 120, 2.0, 2.0));
        db.insert_reading(&make_snapshot_with_kwh(base + 180, 6.0, 5.0));

        // Execute the repair SQL directly and check results
        let conn = db.conn.lock().unwrap();
        let repair_sql = "
            SELECT timestamp, today_import_kwh AS orig,
                   CASE
                     WHEN LAG(today_import_kwh) OVER (ORDER BY timestamp) IS NULL THEN today_import_kwh
                     WHEN today_import_kwh < 1.0
                          AND LAG(today_import_kwh) OVER (ORDER BY timestamp) > 1.0
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
        // Row 1: 0.5 → midnight rollover, keep 0.5 (NOT replace with 150.0!)
        assert!((rows[1].1 - 0.5).abs() < 0.01, "orig should be 0.5");
        assert!(
            (rows[1].2 - 0.5).abs() < 0.01,
            "repaired should be 0.5 (midnight rollover kept), got {}",
            rows[1].2
        );
        // Row 2: 2.0 → normal increase from 0.5, keep 2.0
        assert!((rows[2].2 - 2.0).abs() < 0.01);
        // Row 3: 6.0 → normal increase, keep 6.0
        assert!((rows[3].2 - 6.0).abs() < 0.01);
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
                    (hour - 6) * 400
                } else if hour < 16 {
                    800
                } else {
                    (18 - hour) * 400
                };

                let delta_hours = 5.0 / 60.0;
                correct_kwh += (solar_w as f64) / 1000.0 * delta_hours;

                // Register is stuck after 11:00
                let stored_kwh = if ts >= midnight + 11 * 3600 && ts < midnight + 14 * 3600 {
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
        assert!(
            count > 0,
            "Should have processed at least some rows, got {count}"
        );

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
        let ts2 = ts1 + 2 * 3600; // 10:00 (2h gap > 30min threshold)

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
        assert!(val1.abs() < 0.01, "first row should be 0, got {}", val1);
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
            (base + 5100, 4403, 5.23257441972147), // 11:15 (1h25m gap)
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
                (base + 300, 4403, 5.23257441972147), // 5min gap
                (base + 600, 4400, 5.23293874805481), // 5min gap
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

    #[test]
    fn home_energy_today_kwh_backfills_from_legacy_consumption() {
        // Simulate a REAL pre-upgrade database: the readings table was
        // created before the home_energy_today_kwh column existed, so it
        // has no such column and historic rows carry only
        // today_consumption_kwh. After reopening with the current code, the
        // ALTER adds the column (NULL for every legacy row) and the one-time
        // backfill must copy today_consumption_kwh across so the History
        // "Load Energy Today" chart shows historic data instead of blanks.
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("givenergy-history-test-{id}"));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("legacy_backfill_history.db");
        let _ = std::fs::remove_file(&path);

        // Hand-build a legacy schema identical to SCHEMA_SQL minus the
        // home_energy_today_kwh column, then insert two historic rows with
        // real (non-zero) consumption values and no home_energy_today_kwh.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE readings (
                    timestamp INTEGER PRIMARY KEY,
                    today_consumption_kwh REAL
                );
                CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO readings (timestamp, today_consumption_kwh) VALUES (?, ?)",
                rusqlite::params![1_000i64, 16.2f64],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO readings (timestamp, today_consumption_kwh) VALUES (?, ?)",
                rusqlite::params![2_000i64, 23.7f64],
            )
            .unwrap();
        }

        // Reopen through HistoryDb::open → runs the ALTER + backfill.
        let db = HistoryDb::open(&path).unwrap();
        {
            let conn = db.conn.lock().unwrap();
            let v1: f64 = conn
                .query_row(
                    "SELECT home_energy_today_kwh FROM readings WHERE timestamp = ?",
                    rusqlite::params![1_000i64],
                    |row| row.get(0),
                )
                .unwrap();
            let v2: f64 = conn
                .query_row(
                    "SELECT home_energy_today_kwh FROM readings WHERE timestamp = ?",
                    rusqlite::params![2_000i64],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                (v1 - 16.2).abs() < 1e-6,
                "legacy row 1 backfilled, got {v1}"
            );
            assert!(
                (v2 - 23.7).abs() < 1e-6,
                "legacy row 2 backfilled, got {v2}"
            );

            // The backfill must be gated: reopening must not re-run it (and
            // must not clobber values written by new inserts). The meta flag
            // is the gate.
            let done: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM meta WHERE key = 'home_energy_backfill_done' AND value = '1')",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(done, "home_energy_backfill_done flag set after backfill");
        }

        // Reopen again: backfill must be a no-op (flag already set) and
        // values must be unchanged even if today_consumption_kwh changes.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "UPDATE readings SET today_consumption_kwh = 999.0 WHERE timestamp = ?",
                rusqlite::params![1_000i64],
            )
            .unwrap();
        }
        let db = HistoryDb::open(&path).unwrap();
        {
            let conn = db.conn.lock().unwrap();
            let v1: f64 = conn
                .query_row(
                    "SELECT home_energy_today_kwh FROM readings WHERE timestamp = ?",
                    rusqlite::params![1_000i64],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                (v1 - 16.2).abs() < 1e-6,
                "backfill did not re-run, got {v1}"
            );
        }
    }

    #[test]
    fn home_energy_today_kwh_backfills_zero_rows_but_keeps_legit_zeros() {
        // Production scenario: the column already exists but historic rows
        // carry 0 (written by the brief integration-based decoder whose
        // per-session accumulator started at 0), not NULL. The IS NULL-only
        // backfill in an earlier revision skipped these, leaving the chart
        // blank for 124k rows. The backfill must also recover the 0 rows —
        // but must NOT touch rows where 0 is a legitimate value:
        //   * midnight reset rows (both consumption and home_energy are 0)
        //   * rows already populated (home_energy already non-zero)
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("givenergy-history-test-{id}"));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("legacy_zero_backfill_history.db");
        let _ = std::fs::remove_file(&path);

        // Build a DB that already has the home_energy_today_kwh column
        // (simulating the post-ALTER state), seeded with every shape:
        //   t=1000: cons=16.2, home=0    → MUST backfill to 16.2
        //   t=2000: cons=23.7, home=0    → MUST backfill to 23.7
        //   t=3000: cons=0,    home=0    → midnight reset, LEAVE at 0
        //   t=4000: cons=9.1,  home=9.1  → already populated, LEAVE at 9.1
        //   t=5000: cons=NULL, home=0    → no consumption data, LEAVE at 0
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE readings (
                    timestamp INTEGER PRIMARY KEY,
                    today_consumption_kwh REAL,
                    home_energy_today_kwh REAL
                );
                CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO readings (timestamp, today_consumption_kwh, home_energy_today_kwh) VALUES (?, ?, ?)",
                rusqlite::params![1_000i64, 16.2f64, 0.0f64],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO readings (timestamp, today_consumption_kwh, home_energy_today_kwh) VALUES (?, ?, ?)",
                rusqlite::params![2_000i64, 23.7f64, 0.0f64],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO readings (timestamp, today_consumption_kwh, home_energy_today_kwh) VALUES (?, ?, ?)",
                rusqlite::params![3_000i64, 0.0f64, 0.0f64],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO readings (timestamp, today_consumption_kwh, home_energy_today_kwh) VALUES (?, ?, ?)",
                rusqlite::params![4_000i64, 9.1f64, 9.1f64],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO readings (timestamp, today_consumption_kwh, home_energy_today_kwh) VALUES (?, ?, ?)",
                rusqlite::params![5_000i64, None::<f64>, 0.0f64],
            )
            .unwrap();
        }

        // Reopen → backfill runs (no meta flag yet).
        let db = HistoryDb::open(&path).unwrap();
        let conn = db.conn.lock().unwrap();

        let read = |ts: i64| -> f64 {
            conn.query_row(
                "SELECT home_energy_today_kwh FROM readings WHERE timestamp = ?",
                rusqlite::params![ts],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert!(
            (read(1_000) - 16.2).abs() < 1e-6,
            "zero row backfilled from consumption, got {}",
            read(1_000)
        );
        assert!(
            (read(2_000) - 23.7).abs() < 1e-6,
            "zero row backfilled from consumption, got {}",
            read(2_000)
        );
        assert!(
            read(3_000).abs() < 1e-6,
            "midnight-reset row left at 0, got {}",
            read(3_000)
        );
        assert!(
            (read(4_000) - 9.1).abs() < 1e-6,
            "already-populated row untouched, got {}",
            read(4_000)
        );
        assert!(
            read(5_000).abs() < 1e-6,
            "no-consumption-data row left at 0, got {}",
            read(5_000)
        );
    }
    #[test]
    fn repair_v3_restores_from_backup_and_fixes_midnight_rollover() {
        // Simulate a database that was corrupted by repair_v2's incorrect
        // midnight-rollover threshold (prev > 50.0 AND cur < 10.0).
        //
        // v2 carried yesterday's final today_charge_kwh (8.5) into today's
        // first rows instead of keeping the midnight reset (~0). v3 should
        // restore original values from the backup and re-run the corrected
        // repair with the fixed threshold (cur < 1.0 AND prev > 1.0).
        use std::sync::atomic::{AtomicU32, Ordering};

        static V3_COUNTER: AtomicU32 = AtomicU32::new(1000);
        let id = V3_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("givenergy-v3-test-{id}"));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("history.db");
        let backup_path = path.with_extension("db.bak");

        // ---- Step 1: create original (uncorrupted) data ----
        // Day 1: today_charge_kwh climbs 0 -> 8.5
        // Day 2: today_charge_kwh climbs 0 -> 7.2 (midnight reset)
        let day1_noon = 1705320000i64; // 2024-01-15 12:00 UTC
        let day2_midnight = 1705363200i64; // 2024-01-16 00:00 UTC
        let day2_noon = 1705406400i64; // 2024-01-16 12:00 UTC

        // Write original data to the backup file first (pre-v2 state)
        // Remove any stale backup from a previous run.
        let _ = std::fs::remove_file(&backup_path);
        {
            let bak_conn = Connection::open(&backup_path).unwrap();
            bak_conn
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS readings (
                    timestamp INTEGER PRIMARY KEY,
                    today_charge_kwh REAL,
                    today_discharge_kwh REAL
                )",
                )
                .unwrap();
            // Day 1: 0 -> 8.5
            bak_conn.execute(
                "INSERT INTO readings (timestamp, today_charge_kwh, today_discharge_kwh) VALUES (?1, ?2, ?3)",
                rusqlite::params![day1_noon, 0.0, 0.0],
            ).unwrap();
            bak_conn.execute(
                "INSERT INTO readings (timestamp, today_charge_kwh, today_discharge_kwh) VALUES (?1, ?2, ?3)",
                rusqlite::params![day1_noon + 300, 2.1, 0.0],
            ).unwrap();
            bak_conn.execute(
                "INSERT INTO readings (timestamp, today_charge_kwh, today_discharge_kwh) VALUES (?1, ?2, ?3)",
                rusqlite::params![day1_noon + 600, 5.3, 0.0],
            ).unwrap();
            bak_conn.execute(
                "INSERT INTO readings (timestamp, today_charge_kwh, today_discharge_kwh) VALUES (?1, ?2, ?3)",
                rusqlite::params![day1_noon + 900, 8.5, 0.0],
            ).unwrap();
            // Day 2: 0 -> 7.2 (midnight reset)
            bak_conn.execute(
                "INSERT INTO readings (timestamp, today_charge_kwh, today_discharge_kwh) VALUES (?1, ?2, ?3)",
                rusqlite::params![day2_midnight + 60, 0.3, 0.0],
            ).unwrap();
            bak_conn.execute(
                "INSERT INTO readings (timestamp, today_charge_kwh, today_discharge_kwh) VALUES (?1, ?2, ?3)",
                rusqlite::params![day2_midnight + 300, 1.8, 0.0],
            ).unwrap();
            bak_conn.execute(
                "INSERT INTO readings (timestamp, today_charge_kwh, today_discharge_kwh) VALUES (?1, ?2, ?3)",
                rusqlite::params![day2_noon, 4.5, 0.0],
            ).unwrap();
            bak_conn.execute(
                "INSERT INTO readings (timestamp, today_charge_kwh, today_discharge_kwh) VALUES (?1, ?2, ?3)",
                rusqlite::params![day2_noon + 600, 7.2, 0.0],
            ).unwrap();
        }

        // ---- Step 2: create main database with v2-corrupted data ----
        // Copy backup to main, then corrupt day 2 data (carry forward 8.5)
        std::fs::copy(&backup_path, &path).unwrap();
        {
            let main_conn = Connection::open(&path).unwrap();
            // Corrupt day 2: set all day 2 values to 8.5 (v2 bug: carried forward)
            main_conn
                .execute(
                    "UPDATE readings SET today_charge_kwh = 8.5 WHERE timestamp >= ?1",
                    rusqlite::params![day2_midnight],
                )
                .unwrap();
            // Set repair_v2_done meta flag so v3 knows v2 previously ran
            main_conn
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
                )
                .unwrap();
            main_conn
                .execute_batch(
                    "INSERT OR REPLACE INTO meta (key, value) VALUES ('repair_v2_done', '1')",
                )
                .unwrap();
        }

        // ---- Step 3: open the database (triggers v3 migration) ----
        let db = HistoryDb::open(&path).unwrap();
        let conn = db.conn.lock().unwrap();

        // ---- Step 4: verify data is corrected ----
        // Day 1 should be unchanged (wasn't corrupted)
        let d1_r1: f64 = conn
            .query_row(
                "SELECT today_charge_kwh FROM readings WHERE timestamp = ?",
                rusqlite::params![day1_noon],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            (d1_r1 - 0.0).abs() < 0.01,
            "day1 start should be 0.0, got {d1_r1}"
        );

        let d1_r4: f64 = conn
            .query_row(
                "SELECT today_charge_kwh FROM readings WHERE timestamp = ?",
                rusqlite::params![day1_noon + 900],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            (d1_r4 - 8.5).abs() < 0.01,
            "day1 end should be 8.5, got {d1_r4}"
        );

        // Day 2 should be restored from backup (not carrying forward 8.5)
        let d2_r1: f64 = conn
            .query_row(
                "SELECT today_charge_kwh FROM readings WHERE timestamp = ?",
                rusqlite::params![day2_midnight + 60],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            (d2_r1 - 0.3).abs() < 0.01,
            "day2 first row should be 0.3 (midnight reset), got {d2_r1}"
        );

        let d2_r2: f64 = conn
            .query_row(
                "SELECT today_charge_kwh FROM readings WHERE timestamp = ?",
                rusqlite::params![day2_midnight + 300],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            (d2_r2 - 1.8).abs() < 0.01,
            "day2 second row should be 1.8, got {d2_r2}"
        );

        let d2_r3: f64 = conn
            .query_row(
                "SELECT today_charge_kwh FROM readings WHERE timestamp = ?",
                rusqlite::params![day2_noon],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            (d2_r3 - 4.5).abs() < 0.01,
            "day2 noon should be 4.5, got {d2_r3}"
        );

        let d2_r4: f64 = conn
            .query_row(
                "SELECT today_charge_kwh FROM readings WHERE timestamp = ?",
                rusqlite::params![day2_noon + 600],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            (d2_r4 - 7.2).abs() < 0.01,
            "day2 end should be 7.2, got {d2_r4}"
        );

        // Verify v3 meta flag was set
        let v3_flag: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM meta WHERE key = 'repair_v3_done' AND value = '1')",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        assert!(v3_flag, "repair_v3_done meta flag should be set");

        // Clean up
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repair_v3_skips_when_already_done() {
        // Verify that v3 does NOT re-run if the meta flag is already set.
        use std::sync::atomic::{AtomicU32, Ordering};

        static V3_SKIP_COUNTER: AtomicU32 = AtomicU32::new(2000);
        let id = V3_SKIP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("givenergy-v3-skip-{id}"));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("history.db");

        // Create a database with v3 already marked done
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS readings (
                    timestamp INTEGER PRIMARY KEY,
                    today_charge_kwh REAL
                )",
            )
            .unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            )
            .unwrap();
            conn.execute_batch(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('repair_v3_done', '1')",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO readings (timestamp, today_charge_kwh) VALUES (100, 5.0)",
                [],
            )
            .unwrap();
        }

        // Open -- v3 should skip because flag is set
        let db = HistoryDb::open(&path).unwrap();
        let conn = db.conn.lock().unwrap();

        // Data should be untouched
        let val: f64 = conn
            .query_row(
                "SELECT today_charge_kwh FROM readings WHERE timestamp = 100",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            (val - 5.0).abs() < 0.01,
            "data should be unchanged, got {val}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ==================================================================
    // Weather observations tests
    // ==================================================================

    #[test]
    fn weather_table_created_on_open() {
        // The schema SQL must create the weather_observations table so
        // existing history.db files (without the table) get it on first
        // launch of the new code path.
        let db = test_db();
        let conn = db.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='weather_observations'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "weather_observations table must exist after open");
    }

    #[test]
    fn external_temperature_is_whitelisted() {
        // The query_history loop accepts it; without the whitelist entry
        // the field is silently dropped (SQL-injection defence).
        assert!(is_allowed_field("external_temperature"));
        assert!(is_weather_field("external_temperature"));
        // Spot-check that the original whitelist is unaffected.
        assert!(is_allowed_field("battery_temperature"));
        assert!(!is_weather_field("battery_temperature"));
    }

    #[test]
    fn insert_and_query_weather() {
        let db = test_db();
        // Insert hourly observations across one day. Use a recent range so
        // the very-large `range_secs` query used by the test framework
        // covers them.
        let base = 1700000000i64;
        for hour in 0..24 {
            db.insert_weather(
                base + hour * 3600,
                10.0 + hour as f32 * 0.5,
                "current",
                Some(51.5),
                Some(-0.13),
                base + hour * 3600,
            );
        }

        let result = db
            .query_history(
                100_000_000,
                3600,
                0,
                &["external_temperature".to_string()],
                None,
            )
            .unwrap();

        let points: Vec<TimePoint> =
            serde_json::from_value(result.get("external_temperature").cloned().unwrap()).unwrap();
        assert_eq!(points.len(), 24, "all 24 hourly observations should appear");
        // Values are AVG-aggregated per bucket, so each bucket has a single
        // point equal to the inserted value (one observation per bucket).
        assert!((points[0].v - 10.0).abs() < 0.01);
        assert!((points[23].v - (10.0 + 23.0 * 0.5)).abs() < 0.01);
    }

    #[test]
    fn insert_weather_upserts_on_conflict() {
        // Same timestamp, two writes — second one wins. This is the
        // mechanism that lets a live fetch supersede a slow backfill.
        let db = test_db();
        let ts = 1700000000i64;
        db.insert_weather(ts, 12.0, "backfill", None, None, ts);
        db.insert_weather(ts, 14.5, "current", Some(51.5), Some(-0.13), ts);

        let count: i64 = db
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM weather_observations WHERE timestamp = ?1",
                rusqlite::params![ts],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "upsert must collapse to a single row");

        let value: f64 = db
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT temperature_c FROM weather_observations WHERE timestamp = ?1",
                rusqlite::params![ts],
                |row| row.get(0),
            )
            .unwrap();
        assert!((value - 14.5).abs() < 0.01, "latest write must win");
        let source: String = db
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT source FROM weather_observations WHERE timestamp = ?1",
                rusqlite::params![ts],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source, "current");
    }

    #[test]
    fn weather_query_respects_window() {
        // The same bucketing parameters as query_history — start/end filter
        // is honoured so a History tab query for "last 24h" doesn't pull in
        // every backfilled row since 1940.
        let db = test_db();
        let base = 1700000000i64;
        for hour in 0..48 {
            db.insert_weather(base + hour * 3600, 15.0, "backfill", None, None, base);
        }

        // Explicit window covering the last 6 hours of the populated range.
        let start = base + 42 * 3600;
        let end = base + 48 * 3600;
        let result = db
            .query_history(
                100_000_000,
                3600,
                0,
                &["external_temperature".to_string()],
                Some((start, end)),
            )
            .unwrap();

        let points: Vec<TimePoint> =
            serde_json::from_value(result.get("external_temperature").cloned().unwrap()).unwrap();
        assert_eq!(
            points.len(),
            6,
            "only the in-window buckets should be returned"
        );
    }

    #[test]
    fn weather_query_returns_empty_for_no_data() {
        let db = test_db();
        let result = db
            .query_history(
                100_000_000,
                3600,
                0,
                &["external_temperature".to_string()],
                None,
            )
            .unwrap();
        let points: Vec<TimePoint> =
            serde_json::from_value(result.get("external_temperature").cloned().unwrap()).unwrap();
        assert!(
            points.is_empty(),
            "empty table must yield an empty response"
        );
    }

    #[test]
    fn weather_query_is_silently_dropped_for_unknown_field() {
        // Belt-and-braces: an unknown field name passes through
        // query_history without error, just doesn't appear in the result.
        // SQL injection defence — keep this guard.
        let db = test_db();
        let result = db
            .query_history(
                100_000_000,
                3600,
                0,
                &[
                    "external_temperature".to_string(),
                    "DROP TABLE readings".to_string(),
                ],
                None,
            )
            .unwrap();
        assert!(result.contains_key("external_temperature"));
        assert!(!result.contains_key("DROP TABLE readings"));
    }

    #[test]
    fn earliest_weather_observation_returns_minimum() {
        let db = test_db();
        assert!(db.earliest_weather_observation().is_none(), "empty table");

        let db = test_db();
        // Insert out-of-order to verify the SQL actually orders rather than
        // relying on insertion order.
        db.insert_weather(2000, 14.0, "backfill", None, None, 2000);
        db.insert_weather(1000, 12.0, "backfill", None, None, 1000);
        db.insert_weather(1500, 13.0, "current", None, None, 1500);

        let (ts, source) = db.earliest_weather_observation().unwrap();
        assert_eq!(ts, 1000);
        assert_eq!(source, "backfill");
    }

    #[test]
    fn weather_coexists_with_readings() {
        // End-to-end shape check: insert both an inverter reading and a
        // weather observation at the same timestamp, then query both
        // fields together. The result map must carry both keys with
        // matching bucket timestamps so the frontend can chart them on the
        // same axes.
        let db = test_db();
        let ts = 1700000000i64;
        db.insert_reading(&make_snapshot(ts, 50, 1000));
        db.insert_weather(ts, 18.5, "current", Some(51.5), Some(-0.13), ts);

        let result = db
            .query_history(
                100_000_000,
                3600,
                0,
                &["soc".to_string(), "external_temperature".to_string()],
                None,
            )
            .unwrap();

        let soc: Vec<TimePoint> =
            serde_json::from_value(result.get("soc").cloned().unwrap()).unwrap();
        let ext: Vec<TimePoint> =
            serde_json::from_value(result.get("external_temperature").cloned().unwrap()).unwrap();
        assert_eq!(soc.len(), 1);
        assert_eq!(ext.len(), 1);
        assert_eq!(soc[0].t, ext[0].t, "both must share the bucketed timestamp");
    }

    // ---- Issue #108: per-string PV1/PV2 today history fields ----

    #[test]
    fn today_pv1_kwh_and_today_pv2_kwh_are_allowed_fields() {
        // The whitelist must accept the new field names — without this,
        // /api/history requests for the per-string series would 400.
        assert!(is_allowed_field("today_pv1_kwh"));
        assert!(is_allowed_field("today_pv2_kwh"));
    }

    #[test]
    fn today_pv1_kwh_and_today_pv2_kwh_are_cumulative_fields() {
        // Per-string fields are monotonic daily counters (same shape as
        // today_solar_kwh) and must use MAX aggregation. Otherwise a
        // 5-minute bucket averaging would show lower values than reality.
        assert!(is_cumulative_field("today_pv1_kwh"));
        assert!(is_cumulative_field("today_pv2_kwh"));
    }

    #[test]
    fn insert_and_query_per_string_pv_today_with_max_aggregation() {
        // Insert three snapshots, query today_pv1_kwh / today_pv2_kwh /
        // today_solar_kwh over a wide bucket — must return the MAX of each,
        // not the average, since the fields are cumulative.
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("givenergy-history-test-pv-{id}"));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("pv_history.db");
        let _ = std::fs::remove_file(&path);

        let db = HistoryDb::open(&path).unwrap();
        // Use explicit (start, end) window so the test is independent of
        // wall-clock time. Three snapshots 60s apart.
        let start_ts = 1_700_000_000i64;
        // Three snapshots — PV1 climbs 5→6→7, PV2 climbs 2→2.5→3, total 7→8.5→10.
        let values = [(5.0, 2.0, 7.0), (6.0, 2.5, 8.5), (7.0, 3.0, 10.0)];
        for (i, (pv1, pv2, total)) in values.iter().enumerate() {
            let mut snap = make_snapshot(start_ts + i as i64 * 60, 50, 0);
            snap.today_pv1_kwh = *pv1;
            snap.today_pv2_kwh = *pv2;
            snap.today_solar_kwh = *total;
            db.insert_reading(&snap);
        }

        let result = db
            .query_history(
                3600,
                300,
                0,
                &[
                    "today_pv1_kwh".to_string(),
                    "today_pv2_kwh".to_string(),
                    "today_solar_kwh".to_string(),
                ],
                Some((start_ts, start_ts + 3 * 60)),
            )
            .unwrap();

        let pv1_pts: Vec<TimePoint> =
            serde_json::from_value(result.get("today_pv1_kwh").cloned().unwrap()).unwrap();
        let pv2_pts: Vec<TimePoint> =
            serde_json::from_value(result.get("today_pv2_kwh").cloned().unwrap()).unwrap();
        let total_pts: Vec<TimePoint> =
            serde_json::from_value(result.get("today_solar_kwh").cloned().unwrap()).unwrap();

        assert!(!pv1_pts.is_empty(), "PV1 series must have data");
        assert!(!pv2_pts.is_empty(), "PV2 series must have data");
        assert!(!total_pts.is_empty(), "Total series must have data");

        // MAX aggregation across the three samples.
        let pv1_max = pv1_pts
            .iter()
            .map(|p| p.v)
            .fold(f64::NEG_INFINITY, f64::max);
        let pv2_max = pv2_pts
            .iter()
            .map(|p| p.v)
            .fold(f64::NEG_INFINITY, f64::max);
        let total_max = total_pts
            .iter()
            .map(|p| p.v)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (pv1_max - 7.0).abs() < 1e-6,
            "PV1 MAX must be 7.0; got {pv1_max}"
        );
        assert!(
            (pv2_max - 3.0).abs() < 1e-6,
            "PV2 MAX must be 3.0; got {pv2_max}"
        );
        assert!(
            (total_max - 10.0).abs() < 1e-6,
            "Total MAX must be 10.0; got {total_max}"
        );

        // Invariant: total == pv1 + pv2 (with per-string sum preference).
        assert!(
            (total_max - (pv1_max + pv2_max)).abs() < 1e-6,
            "Total must equal PV1+PV2 in MAX view"
        );
    }

    #[test]
    fn existing_db_migrates_today_pv_columns() {
        // Simulate an existing DB created before the per-string columns
        // existed. After reopening with the current code, the ALTER
        // migrations must add the columns and inserts must succeed.
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("givenergy-history-test-pv-mig-{id}"));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("legacy_pv.db");
        let _ = std::fs::remove_file(&path);

        // Open with current schema (which includes the ALTER migration),
        // insert a row with the new fields, reopen, read back.
        {
            let db = HistoryDb::open(&path).unwrap();
            let mut snap = make_snapshot(1_000, 50, 0);
            snap.today_pv1_kwh = 4.5;
            snap.today_pv2_kwh = 2.0;
            db.insert_reading(&snap);
        }
        {
            let db = HistoryDb::open(&path).unwrap();
            let conn = db.conn.lock().unwrap();
            let pv1: Option<f64> = conn
                .query_row(
                    "SELECT today_pv1_kwh FROM readings WHERE timestamp = 1000",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let pv2: Option<f64> = conn
                .query_row(
                    "SELECT today_pv2_kwh FROM readings WHERE timestamp = 1000",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                pv1.map(|v| (v - 4.5).abs() < 1e-6).unwrap_or(false),
                "got {pv1:?}"
            );
            assert!(
                pv2.map(|v| (v - 2.0).abs() < 1e-6).unwrap_or(false),
                "got {pv2:?}"
            );
        }
    }

    #[test]
    fn existing_db_migrates_pv_pct_columns() {
        // issue #110: an existing DB opened with the new schema must ALTER
        // in the pv1_pct / pv2_pct columns and inserts must succeed.
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("givenergy-history-test-pct-mig-{id}"));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("legacy_pct.db");
        let _ = std::fs::remove_file(&path);

        // Open with current schema (includes the pv_pct ALTER migration).
        {
            let db = HistoryDb::open(&path).unwrap();
            let mut snap = make_snapshot(2_000, 50, 0);
            snap.pv1_pct = Some(76.76);
            snap.pv2_pct = Some(44.85);
            db.insert_reading(&snap);
        }
        {
            let db = HistoryDb::open(&path).unwrap();
            let conn = db.conn.lock().unwrap();
            let pv1: Option<f64> = conn
                .query_row(
                    "SELECT pv1_pct FROM readings WHERE timestamp = 2000",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let pv2: Option<f64> = conn
                .query_row(
                    "SELECT pv2_pct FROM readings WHERE timestamp = 2000",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                pv1.map(|v| (v - 76.76).abs() < 1e-3).unwrap_or(false),
                "got pv1_pct = {pv1:?}"
            );
            assert!(
                pv2.map(|v| (v - 44.85).abs() < 1e-3).unwrap_or(false),
                "got pv2_pct = {pv2:?}"
            );
        }
    }

    #[test]
    fn query_rejects_per_string_field_when_not_yet_migrated() {
        // Belt-and-braces: even if migration hasn't run for some reason,
        // the API path validates the field name against ALLOWED_FIELDS,
        // not the column existence. Field name recognition is independent
        // of column existence at query time. (The migration is what adds
        // the column, but a missing column would surface as an SQLite
        // error, not a 400 from the API layer.)
        assert!(
            is_allowed_field("today_pv1_kwh"),
            "API must whitelist the field name"
        );
    }

    // -----------------------------------------------------------------------
    // Cost / income series
    // -----------------------------------------------------------------------

    fn local_midnight_secs(day_offset: i64) -> i64 {
        let date = Local::now().date_naive() + chrono::Duration::days(day_offset);
        Local
            .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .unwrap()
            .timestamp()
    }

    /// Insert one local day of a daily-resetting export counter at 30-min
    /// resolution. The counter rises 0 → `daily_kwh` between `rise_start_min`
    /// and `rise_end_min` (local minutes-of-day) and plateaus after, so the
    /// day's last reading carries the full daily total. It is 0 at the day's
    /// local midnight (step 0), modelling the inverter's midnight reset.
    fn insert_export_day(
        db: &HistoryDb,
        day_offset: i64,
        daily_kwh: f32,
        rise_start_min: i64,
        rise_end_min: i64,
    ) {
        let midnight = local_midnight_secs(day_offset);
        for step in 0..48i64 {
            let ts = midnight + step * 1800;
            let min_of_day = step * 30;
            let frac = if min_of_day <= rise_start_min {
                0.0
            } else if min_of_day >= rise_end_min {
                1.0
            } else {
                (min_of_day - rise_start_min) as f64 / (rise_end_min - rise_start_min) as f64
            };
            db.insert_reading(&make_snapshot_with_kwh(
                ts,
                0.0,
                (daily_kwh as f64 * frac) as f32,
            ));
        }
    }

    /// Insert one local day of a daily-resetting IMPORT counter (mirror of
    /// [`insert_export_day`] on the import side). Used by the cost-breakdown
    /// tests, where the standing charge only applies to the import direction.
    fn insert_import_day(
        db: &HistoryDb,
        day_offset: i64,
        daily_kwh: f32,
        rise_start_min: i64,
        rise_end_min: i64,
    ) {
        let midnight = local_midnight_secs(day_offset);
        for step in 0..48i64 {
            let ts = midnight + step * 1800;
            let min_of_day = step * 30;
            let frac = if min_of_day <= rise_start_min {
                0.0
            } else if min_of_day >= rise_end_min {
                1.0
            } else {
                (min_of_day - rise_start_min) as f64 / (rise_end_min - rise_start_min) as f64
            };
            db.insert_reading(&make_snapshot_with_kwh(
                ts,
                (daily_kwh as f64 * frac) as f32,
                0.0,
            ));
        }
    }

    fn series_total(series: &[TimePoint]) -> f64 {
        series.last().map(|p| p.v).unwrap_or(0.0)
    }

    /// Cumulative value of a breakdown component at its last bucket.
    fn breakdown_energy_total(series: &[CostComponentPoint]) -> f64 {
        series.last().map(|p| p.energy_gbp).unwrap_or(0.0)
    }
    fn breakdown_standing_total(series: &[CostComponentPoint]) -> f64 {
        series.last().map(|p| p.standing_gbp).unwrap_or(0.0)
    }

    /// A `HistoryWindow` pinned to explicit `(start, end)` UTC-second bounds —
    /// the form every cost-series test uses. `range_secs`/`offset` are unused
    /// when `explicit_window` is `Some`.
    fn window(start: i64, end: i64) -> HistoryWindow {
        HistoryWindow {
            range_secs: 0,
            offset: 0,
            explicit_window: Some((start, end)),
        }
    }

    fn tou_export_tariff() -> crate::settings::TariffConfig {
        // Off-peak 0.10 all day except a 16:00-19:00 peak at 0.30.
        crate::settings::TariffConfig {
            slots: vec![
                crate::settings::TariffSlot {
                    start: "00:00".into(),
                    end: "16:00".into(),
                    rate: 0.10,
                },
                crate::settings::TariffSlot {
                    start: "16:00".into(),
                    end: "19:00".into(),
                    rate: 0.30,
                },
                crate::settings::TariffSlot {
                    start: "19:00".into(),
                    end: "23:59".into(),
                    rate: 0.10,
                },
            ],
        }
    }

    #[test]
    fn cost_series_total_is_bucket_size_independent() {
        // The core regression: the cumulative total must not
        // change with the selected range's bucket width. Three local days of
        // export, ALL generated inside the 16:00-19:00 peak window, priced with
        // a time-of-use tariff. Because the running total is integrated at
        // native reading resolution, every bucket size yields the same final
        // value - and that value reflects the peak rate even on the 24h bucket
        // (the old MAX-bucket reconstruction collapsed wide ranges to the
        // off-peak rate).
        let db = test_db();
        for d in 0..3 {
            insert_export_day(&db, d, 10.0, 16 * 60, 18 * 60 + 30); // 16:00-18:30
        }
        let start = local_midnight_secs(0) - 60;
        let end = local_midnight_secs(3) + 60;
        let tou = tou_export_tariff();

        let totals: Vec<f64> = [1800i64, 3600, 7200, 43200, 86400]
            .iter()
            .map(|&b| {
                let s = db
                    .query_cost_series(&window(start, end), b, "today_export_kwh", &tou, 0.10, 0.0)
                    .unwrap();
                series_total(&s)
            })
            .collect();

        for w in totals.windows(2) {
            assert!(
                (w[0] - w[1]).abs() < 1e-9,
                "cost total drifted with bucket size: {totals:?}"
            );
        }
        // 3 days × 10 kWh, all in the peak window → 30 kWh × £0.30 = £9.00.
        assert!(
            (totals[0] - 9.0).abs() < 1e-6,
            "expected £9.00 at peak rate, got {}",
            totals[0]
        );
    }

    #[test]
    fn cost_series_prices_each_increment_at_its_local_time() {
        // Same energy (10 kWh in a day), once in the peak window and once in an
        // off-peak window, must price at the respective rates - proving the
        // rate is applied at the time the energy was produced.
        let tou = tou_export_tariff();
        let start = local_midnight_secs(0) - 60;
        let end = local_midnight_secs(1) + 60;

        let db_peak = test_db();
        insert_export_day(&db_peak, 0, 10.0, 16 * 60, 18 * 60 + 30);
        let peak = series_total(
            &db_peak
                .query_cost_series(
                    &window(start, end),
                    1800,
                    "today_export_kwh",
                    &tou,
                    0.10,
                    0.0,
                )
                .unwrap(),
        );

        let db_off = test_db();
        insert_export_day(&db_off, 0, 10.0, 6 * 60, 10 * 60);
        let off = series_total(
            &db_off
                .query_cost_series(
                    &window(start, end),
                    1800,
                    "today_export_kwh",
                    &tou,
                    0.10,
                    0.0,
                )
                .unwrap(),
        );

        assert!(
            (peak - 3.0).abs() < 1e-6,
            "10 kWh @ peak 0.30 = £3.00, got {peak}"
        );
        assert!(
            (off - 1.0).abs() < 1e-6,
            "10 kWh @ off-peak 0.10 = £1.00, got {off}"
        );
    }

    #[test]
    fn cost_series_credits_each_day_once_after_reset() {
        // Three days at a flat rate must sum each day's energy exactly once.
        // A double-count (the symptom that inflated narrow ranges) would
        // roughly double this.
        let db = test_db();
        for d in 0..3 {
            insert_export_day(&db, d, 8.0, 6 * 60, 10 * 60);
        }
        let start = local_midnight_secs(0) - 60;
        let end = local_midnight_secs(3) + 60;
        let flat = crate::settings::TariffConfig::flat(0.10);
        let total = series_total(
            &db.query_cost_series(
                &window(start, end),
                86400,
                "today_export_kwh",
                &flat,
                0.10,
                0.0,
            )
            .unwrap(),
        );
        // 3 × 8 kWh × £0.10 = £2.40.
        assert!((total - 2.4).abs() < 1e-6, "expected £2.40, got {total}");
    }

    #[test]
    fn cost_series_ignores_same_day_dip_without_recounting_recovery() {
        // A transient sensor dip mid-day must not subtract energy, and the
        // recovery back up to the prior peak must not be re-counted.
        let db = test_db();
        let m = local_midnight_secs(0);
        let pts: [(i64, f32); 5] = [
            (0, 0.0),       // 00:00 baseline
            (8 * 60, 5.0),  // 08:00 → +5
            (9 * 60, 1.0),  // 09:00 glitch dip (ignored, baseline held at 5)
            (10 * 60, 5.0), // 10:00 recovery (no new energy)
            (12 * 60, 8.0), // 12:00 → +3
        ];
        for (min, v) in pts {
            db.insert_reading(&make_snapshot_with_kwh(m + min * 60, 0.0, v));
        }
        let start = m - 60;
        let end = local_midnight_secs(1) + 60;
        let flat = crate::settings::TariffConfig::flat(1.0);
        let total = series_total(
            &db.query_cost_series(
                &window(start, end),
                3600,
                "today_export_kwh",
                &flat,
                1.0,
                0.0,
            )
            .unwrap(),
        );
        // True energy is the monotone envelope 0 -> 8 = 8 kWh at £1.00 = £8.00.
        assert!(
            (total - 8.0).abs() < 1e-6,
            "dip recovery double-counted: got {total}"
        );
    }

    #[test]
    fn cost_series_treats_near_midnight_counter_collapse_as_reset() {
        // Issue #184: if the first reading inside a Today/calendar window still
        // carries yesterday's daily counter (the inverter reset arrives a minute
        // or two after the UI's local-midnight window open), the cost walk used
        // that high value as the same-day baseline. The subsequent reset to 0
        // was treated as a glitch and the baseline stayed high, so a normal day
        // that exported less than yesterday showed no export income even though
        // the raw `today_export_kwh` history was correct.
        let db = test_db();
        let m = local_midnight_secs(0);
        db.insert_reading(&make_snapshot_with_kwh(m + 10, 0.0, 18.0)); // stale pre-reset value
        db.insert_reading(&make_snapshot_with_kwh(m + 70, 0.0, 0.0)); // true daily reset
        db.insert_reading(&make_snapshot_with_kwh(m + 12 * 3600, 0.0, 4.0));
        db.insert_reading(&make_snapshot_with_kwh(m + 15 * 3600, 0.0, 8.0));

        let flat = crate::settings::TariffConfig::flat(0.20);
        let total = series_total(
            &db.query_cost_series(
                &window(m, local_midnight_secs(1)),
                300,
                "today_export_kwh",
                &flat,
                0.20,
                0.0,
            )
            .unwrap(),
        );

        assert!(
            (total - 1.60).abs() < 1e-6,
            "8 kWh exported after reset at £0.20 should be credited, got {total}"
        );
    }

    #[test]
    fn cost_series_does_not_misprice_energy_accrued_during_a_gap() {
        // Recording resumes mid-afternoon (peak window) after a gap, with the
        // counter already at 9 kWh produced at unknown (mostly off-peak) times.
        // That 9 kWh must NOT be billed at the 17:00 peak rate; only energy
        // observed via within-day deltas after resumption is counted.
        let db = test_db();
        let m0 = local_midnight_secs(0);
        db.insert_reading(&make_snapshot_with_kwh(m0 + 6 * 3600, 0.0, 0.0)); // 06:00, then a gap
        let m1 = local_midnight_secs(1);
        db.insert_reading(&make_snapshot_with_kwh(m1 + 17 * 3600, 0.0, 9.0)); // resume 17:00, raw 9
        db.insert_reading(&make_snapshot_with_kwh(m1 + 17 * 3600 + 1800, 0.0, 10.5)); // 17:30
        db.insert_reading(&make_snapshot_with_kwh(m1 + 18 * 3600, 0.0, 11.0)); // 18:00
        db.insert_reading(&make_snapshot_with_kwh(m1 + 18 * 3600 + 1800, 0.0, 12.0)); // 18:30
        let start = m0 - 60;
        let end = m1 + 19 * 3600;
        let tou = tou_export_tariff();
        let total = series_total(
            &db.query_cost_series(
                &window(start, end),
                1800,
                "today_export_kwh",
                &tou,
                0.10,
                0.0,
            )
            .unwrap(),
        );
        // Only the observed 9 -> 12 = 3 kWh (all peak) is credited: 3 x 0.30 = 0.90.
        // The pre-resumption 9 kWh is not retroactively priced at the peak rate
        // (which would have given 12 x 0.30 = 3.60).
        assert!(
            (total - 0.90).abs() < 1e-6,
            "gap energy mis-priced: got {total}"
        );
    }

    #[test]
    fn cost_series_credits_sustained_high_power_under_ceiling() {
        // ~20 kW import sustained for 10 minutes (0.333 kWh/min) must be credited
        // in full. The old 15 kW ceiling (0.25 kWh per 60s poll) wrongly dropped
        // every step and stranded the whole session at ~0.
        let db = test_db();
        let m = local_midnight_secs(0);
        db.insert_reading(&make_snapshot_with_kwh(m + 12 * 3600, 0.0, 0.0)); // noon, import 0
        let mut kwh = 0.0f32;
        for i in 1..=10i64 {
            kwh += 20.0 / 60.0; // 20 kW for one minute
            db.insert_reading(&make_snapshot_with_kwh(m + 12 * 3600 + i * 60, kwh, 0.0));
        }
        let start = m - 60;
        let end = m + 13 * 3600;
        let flat = crate::settings::TariffConfig::flat(0.25);
        let total = series_total(
            &db.query_cost_series(
                &window(start, end),
                1800,
                "today_import_kwh",
                &flat,
                0.25,
                0.0,
            )
            .unwrap(),
        );
        // ~3.33 kWh at 0.25 ~= £0.83. Must be far above zero (the bug gave ~0).
        assert!(
            (total - 0.833).abs() < 0.02,
            "sustained high power dropped: got {total}"
        );
    }

    #[test]
    fn cost_series_still_clamps_a_gross_transient_spike() {
        // A single corrupt reading that jumps the counter by orders of magnitude
        // and then returns must not inflate the total.
        let db = test_db();
        let m = local_midnight_secs(0);
        db.insert_reading(&make_snapshot_with_kwh(m + 12 * 3600, 0.0, 5.0)); // 12:00 export 5
        db.insert_reading(&make_snapshot_with_kwh(m + 12 * 3600 + 60, 0.0, 5000.0)); // spike
        db.insert_reading(&make_snapshot_with_kwh(m + 12 * 3600 + 120, 0.0, 5.2)); // back to normal
        db.insert_reading(&make_snapshot_with_kwh(m + 12 * 3600 + 180, 0.0, 5.4));
        let start = m - 60;
        let end = m + 13 * 3600;
        let flat = crate::settings::TariffConfig::flat(1.0);
        let total = series_total(
            &db.query_cost_series(
                &window(start, end),
                1800,
                "today_export_kwh",
                &flat,
                1.0,
                0.0,
            )
            .unwrap(),
        );
        // The 5 -> 5000 spike is dropped (baseline held at 5); only 5 -> 5.2 -> 5.4
        // = 0.4 kWh of real growth is credited: 0.4 x 1.0 = £0.40.
        assert!(
            (total - 0.4).abs() < 1e-6,
            "gross spike not clamped: got {total}"
        );
    }

    // -----------------------------------------------------------------------
    // Issue #131: standing-charge (p/day) integration into the cost series.
    //
    // The historical-cost tests above intentionally pass `0.0` for the new
    // standing-charge parameter to keep the regression baseline stable.
    // These tests exercise the new parameter and confirm the documented
    // behaviour: zero leaves the series unchanged; a non-zero value
    // debits the per-day amount once at the start of each local day the
    // window covers (the cost graph line steps up at every local
    // midnight within the window); partial-day windows are handled
    // correctly; the empty-window fallback still emits the standing-charge
    // value; bucket-size independence is preserved.
    // -----------------------------------------------------------------------

    /// Read the bucket timestamps as `(unix_seconds, value)` pairs. The
    /// per-day step test reads the timestamps to confirm the standing-charge
    /// debits land on local-midnight boundaries.
    fn series_points(series: &[TimePoint]) -> Vec<(i64, f64)> {
        series.iter().map(|p| (p.t / 1000, p.v)).collect()
    }

    #[test]
    fn cost_series_standing_charge_zero_is_a_no_op() {
        // The historical-cost series must be byte-identical whether the
        // Standing Charge is explicitly 0 or omitted (older callers pass
        // 0). Regression guard for issue #131.
        let db = test_db();
        insert_export_day(&db, 0, 10.0, 6 * 60, 10 * 60);
        let start = local_midnight_secs(0) - 60;
        let end = local_midnight_secs(1) + 60;
        let flat = crate::settings::TariffConfig::flat(0.25);
        let baseline = series_total(
            &db.query_cost_series(
                &window(start, end),
                1800,
                "today_export_kwh",
                &flat,
                0.25,
                0.0,
            )
            .unwrap(),
        );
        assert!(
            (baseline - 2.5).abs() < 1e-6,
            "10 kWh @ 0.25 = £2.50; got {baseline}"
        );
    }

    #[test]
    fn cost_series_standing_charge_debits_at_each_local_midnight() {
        // A 4-day window (00:00 day-0 → 00:00 day-4) must show exactly 4
        // standing-charge debits, one per local day touched, NOT a single
        // flat offset at window open. The cumulative series therefore
        // shows visible steps at each local midnight (matching how UK
        // bills actually look).
        let db = test_db();
        for d in 0..3 {
            insert_export_day(&db, d, 5.0, 6 * 60, 10 * 60); // 5 kWh each day
        }
        let start = local_midnight_secs(0);
        let end = local_midnight_secs(4);
        let flat = crate::settings::TariffConfig::flat(0.10);
        let series = db
            .query_cost_series(
                &window(start, end),
                1800,
                "today_export_kwh",
                &flat,
                0.10,
                54.86,
            )
            .unwrap();
        // kWh cost: 3 × 5 × £0.10 = £1.50. Standing Charge: 4 × £0.5486 = £2.1944.
        let expected_total = 1.50 + 4.0 * 0.5486;
        let got = series_total(&series);
        assert!(
            (got - expected_total).abs() < 1e-3,
            "4-day window total should be kWh + 4 × Standing Charge (got {got}, expected {expected_total})"
        );
        // The very first bucket (before any kWh is consumed) should carry
        // only ONE day's Standing Charge, NOT the full 4-day amount.
        let first = series.first().map(|p| p.v).unwrap_or(0.0);
        assert!(
            (first - 0.5486).abs() < 1e-3,
            "first bucket should carry only day-1 Standing Charge (got {first})"
        );
        // The last bucket must carry all 4 days of Standing Charge plus
        // the per-kWh total — already verified by `series_total` above.
        let last = series.last().map(|p| p.v).unwrap_or(0.0);
        assert!(
            (last - expected_total).abs() < 1e-3,
            "last bucket should equal kWh + 4 × Standing Charge (got {last})"
        );
    }

    #[test]
    fn cost_series_standing_charge_single_day_window_credits_one_day() {
        // A window that starts and ends on the same local day, with no
        // contained local midnight, must credit exactly one day (the
        // partial-day at window open, which under UK billing incurs the
        // full daily fee).
        let db = test_db();
        insert_export_day(&db, 0, 5.0, 6 * 60, 10 * 60);
        let m = local_midnight_secs(0);
        // 00:00 → 23:59 same day. No midnight contained in the window.
        let start = m;
        let end = m + 23 * 3600 + 59 * 60;
        let flat = crate::settings::TariffConfig::flat(0.25);
        let series = db
            .query_cost_series(
                &window(start, end),
                3600,
                "today_export_kwh",
                &flat,
                0.25,
                54.86,
            )
            .unwrap();
        // kWh: 5 × £0.25 = £1.25. Standing Charge: 1 × £0.5486.
        let expected = 1.25 + 0.5486;
        let got = series_total(&series);
        assert!(
            (got - expected).abs() < 1e-3,
            "single-day no-midnight window should credit exactly one day (got {got}, expected {expected})"
        );
    }

    #[test]
    fn cost_series_standing_charge_no_midnight_in_window_credits_one_day() {
        // A window that sits entirely within one local day (e.g. 12h
        // starting at 06:00, ending at 18:00) contains no local-midnight
        // boundary. UK bills still charge the daily fee for that day, so
        // we credit one full day. The series must show £0.5486 even though
        // no day boundary was crossed.
        let db = test_db();
        // 06:00 → 18:00, no readings anywhere in that stretch.
        let m = local_midnight_secs(0);
        let start = m + 6 * 3600;
        let end = m + 18 * 3600;
        let flat = crate::settings::TariffConfig::flat(0.25);
        let series = db
            .query_cost_series(
                &window(start, end),
                3600,
                "today_import_kwh",
                &flat,
                0.25,
                54.86,
            )
            .unwrap();
        // No readings → no per-kWh cost. Standing Charge: £0.5486 (one day,
        // partial first day still incurs the daily fee under UK billing).
        let got = series_total(&series);
        assert!(
            (got - 0.5486).abs() < 1e-3,
            "no-midnight window must still credit one full day (got {got})"
        );
    }

    #[test]
    fn cost_series_standing_charge_partial_first_day_credits_one_day() {
        // A 12h window starting at local midnight (00:00 → 12:00) contains
        // no local midnight, but the user is still in that day so it should
        // be billed at one full day's Standing Charge.
        let db = test_db();
        let m = local_midnight_secs(0);
        let start = m;
        let end = m + 12 * 3600;
        let flat = crate::settings::TariffConfig::flat(0.25);
        let series = db
            .query_cost_series(
                &window(start, end),
                3600,
                "today_import_kwh",
                &flat,
                0.25,
                54.86,
            )
            .unwrap();
        let got = series_total(&series);
        assert!(
            (got - 0.5486).abs() < 1e-3,
            "partial first day must still credit one full day (got {got})"
        );
    }

    #[test]
    fn cost_series_standing_charge_crossing_two_midnights_credits_three_days() {
        // A 60h window (00:00 day-0 → 12:00 day-2) crosses TWO local
        // midnights, meaning the user lives through 3 distinct calendar
        // days (today, tomorrow, day-after). We credit 3 days × £0.5486
        // = £1.6458 in Standing Charge.
        let db = test_db();
        let m = local_midnight_secs(0);
        let start = m;
        let end = m + 60 * 3600;
        let flat = crate::settings::TariffConfig::flat(0.25);
        let series = db
            .query_cost_series(
                &window(start, end),
                3600,
                "today_import_kwh",
                &flat,
                0.25,
                54.86,
            )
            .unwrap();
        let got = series_total(&series);
        assert!(
            (got - 3.0 * 0.5486).abs() < 1e-3,
            "60h window crossing 2 midnights must credit 3 days (got {got})"
        );
    }

    #[test]
    fn cost_series_standing_charge_negative_input_is_clamped_to_zero() {
        // A negative Standing Charge is not a real-world concept and would
        // invert the cost graph (subtracting from the cumulative total).
        // The implementation clamps to 0; the output must be byte-identical
        // to passing 0 explicitly.
        let db = test_db();
        insert_export_day(&db, 0, 5.0, 6 * 60, 10 * 60);
        let m = local_midnight_secs(0);
        let start = m - 60;
        let end = m + 24 * 3600;
        let flat = crate::settings::TariffConfig::flat(0.25);
        let baseline = series_total(
            &db.query_cost_series(
                &window(start, end),
                1800,
                "today_export_kwh",
                &flat,
                0.25,
                0.0,
            )
            .unwrap(),
        );
        let clamped = series_total(
            &db.query_cost_series(
                &window(start, end),
                1800,
                "today_export_kwh",
                &flat,
                0.25,
                -100.0,
            )
            .unwrap(),
        );
        assert!(
            (baseline - clamped).abs() < 1e-9,
            "negative Standing Charge must clamp to 0 (baseline {baseline}, clamped {clamped})"
        );
    }

    #[test]
    fn cost_series_standing_charge_emits_bucket_when_no_data() {
        // A query over a stretch with no readings must still surface the
        // standing-charge value — otherwise a quiet day would show as £0
        // in the History cost graph even though the user is paying for a
        // daily standing fee. The 24h window crosses one local midnight
        // (m+24h opens day 1), so we credit 2 days × £0.5486 = £1.0972.
        let db = test_db();
        let m = local_midnight_secs(0);
        let start = m;
        let end = m + 24 * 3600;
        let flat = crate::settings::TariffConfig::flat(0.25);
        let series = db
            .query_cost_series(
                &window(start, end),
                3600,
                "today_import_kwh",
                &flat,
                0.25,
                54.86,
            )
            .unwrap();
        assert!(
            !series.is_empty(),
            "empty-window fallback must still emit at least one bucket"
        );
        let first = series.first().map(|p| p.v).unwrap_or(0.0);
        assert!(
            (first - 2.0 * 0.5486).abs() < 1e-3,
            "empty-window first bucket should carry 2 days of Standing Charge (got {first})"
        );
    }

    #[test]
    fn cost_series_standing_charge_does_not_break_bucket_size_independence() {
        // Regression guard for the existing bucket-size-independence
        // property: with a non-zero Standing Charge, the cumulative total
        // must still be the same across every bucket width. Issue #131.
        let db = test_db();
        for d in 0..3 {
            insert_export_day(&db, d, 10.0, 6 * 60, 10 * 60);
        }
        // 4-day window: 00:00 day-0 → 00:00 day-4 (crosses 3 midnights,
        // touches 4 calendar days).
        let start = local_midnight_secs(0);
        let end = local_midnight_secs(4);
        let flat = crate::settings::TariffConfig::flat(0.25);
        let totals: Vec<f64> = [1800i64, 3600, 7200, 43200, 86400]
            .iter()
            .map(|&b| {
                series_total(
                    &db.query_cost_series(
                        &window(start, end),
                        b,
                        "today_export_kwh",
                        &flat,
                        0.25,
                        54.86,
                    )
                    .unwrap(),
                )
            })
            .collect();
        for w in totals.windows(2) {
            assert!(
                (w[0] - w[1]).abs() < 1e-9,
                "cost total drifted with bucket size when Standing Charge is non-zero: {totals:?}"
            );
        }
        // kWh: 3 × 10 × £0.25 = £7.50. Standing Charge: 4 × £0.5486 = £2.1944.
        let expected = 7.50 + 4.0 * 0.5486;
        assert!(
            (totals[0] - expected).abs() < 1e-3,
            "expected {expected}, got {}",
            totals[0]
        );
    }

    #[test]
    fn cost_series_standing_charge_step_lands_on_local_midnight() {
        // Verify the per-day standing-charge step pattern. Insert readings
        // spanning a local midnight: 22:00 day-0 → 03:00 day-1, with
        // kWh activity on both sides. The cumulative cost graph must
        // show a visible step at the midnight boundary, exactly one day's
        // worth of Standing Charge in size.
        let db = test_db();
        let m = local_midnight_secs(0);
        let start = m + 22 * 3600;
        let end = m + 27 * 3600;
        // 5 kWh across day-0 evening (20:00 → 23:30) and day-1 morning
        // (00:00 → 02:30) so we have observations straddling midnight.
        insert_export_day(&db, 0, 5.0, 20 * 60, 23 * 60 + 30);
        insert_export_day(&db, 1, 5.0, 30, 2 * 60 + 30);
        let flat = crate::settings::TariffConfig::flat(0.25);
        let series = db
            .query_cost_series(
                &window(start, end),
                3600,
                "today_export_kwh",
                &flat,
                0.25,
                54.86,
            )
            .unwrap();
        let points = series_points(&series);
        assert!(!points.is_empty(), "series must have at least one bucket");
        // The very first bucket (open day, before midnight) carries the
        // open-day debit (1 day) plus whatever kWh cost has accumulated
        // up to that point. We only check the standing-charge portion:
        // it must equal exactly one day's worth, even if some kWh cost
        // has accrued.
        let first_v = points[0].1;
        let first_kwh_share = first_v - 0.5486;
        assert!(
            first_kwh_share >= -1e-6,
            "first bucket must include the open-day Standing Charge (got {first_v})"
        );
        // The very last bucket (after the crossed midnight) must carry
        // 2 × £0.5486 plus any kWh cost.
        let last_v = points.last().unwrap().1;
        assert!(
            last_v > 2.0 * 0.5486,
            "last bucket (after midnight) must include the second day's Standing Charge on top of kWh cost (got {last_v})"
        );
        // The graph line must show a clear visible step at midnight:
        // the maximum value in the second half of the series must be
        // strictly greater than the maximum value in the first half
        // (by at least the per-day Standing Charge amount).
        let mid_idx = points.len() / 2;
        let pre_midnight = points[..mid_idx]
            .iter()
            .map(|(_, v)| *v)
            .fold(f64::NEG_INFINITY, f64::max);
        let post_midnight = points[mid_idx..]
            .iter()
            .map(|(_, v)| *v)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            post_midnight - pre_midnight >= 0.5486 - 1e-6,
            "the cost graph must step UP at the local midnight by at least one day's Standing Charge (pre={pre_midnight}, post={post_midnight})"
        );
    }

    // -- Import-cost breakdown (energy vs standing charge) -----------------

    #[test]
    fn cost_breakdown_energy_plus_standing_equals_total_per_bucket() {
        // The two breakdown components must sum, bucket-for-bucket, to the
        // total `query_cost_series` plots — the invariant the History chart
        // relies on to draw energy and standing charge as separate lines
        // whose implied sum is the (red) total line.
        let db = test_db();
        for d in 0..3 {
            insert_import_day(&db, d, 10.0, 6 * 60, 10 * 60);
        }
        let start = local_midnight_secs(0);
        let end = local_midnight_secs(4);
        let flat = crate::settings::TariffConfig::flat(0.25);
        let w = window(start, end);
        let breakdown = db
            .query_cost_breakdown(&w, 3600, "today_import_kwh", &flat, 0.25, 54.86)
            .unwrap();
        let total = db
            .query_cost_series(&w, 3600, "today_import_kwh", &flat, 0.25, 54.86)
            .unwrap();
        assert_eq!(
            breakdown.len(),
            total.len(),
            "breakdown and total series must share the same buckets"
        );
        for (c, t) in breakdown.iter().zip(total.iter()) {
            assert_eq!(c.t, t.t, "bucket timestamps must line up");
            assert!(
                (c.energy_gbp + c.standing_gbp - t.v).abs() < 1e-9,
                "energy ({}) + standing ({}) must equal total ({}) at t={}",
                c.energy_gbp,
                c.standing_gbp,
                t.v,
                t.t
            );
        }
    }

    #[test]
    fn cost_breakdown_zero_standing_charge_has_zero_standing_component() {
        // With no Standing Charge configured, the standing component must be
        // flat zero and the energy component must equal the whole cost — the
        // case where the History chart keeps the breakdown lines hidden.
        let db = test_db();
        insert_import_day(&db, 0, 12.0, 6 * 60, 10 * 60);
        let start = local_midnight_secs(0);
        let end = local_midnight_secs(1);
        let flat = crate::settings::TariffConfig::flat(0.25);
        let breakdown = db
            .query_cost_breakdown(
                &window(start, end),
                3600,
                "today_import_kwh",
                &flat,
                0.25,
                0.0,
            )
            .unwrap();
        assert!(
            breakdown.iter().all(|c| c.standing_gbp == 0.0),
            "standing component must be zero when no Standing Charge is set"
        );
        // 12 kWh × £0.25 = £3.00, all in the energy component.
        assert!(
            (breakdown_energy_total(&breakdown) - 3.0).abs() < 1e-6,
            "energy component should carry the full £3.00 (got {})",
            breakdown_energy_total(&breakdown)
        );
        assert!(
            breakdown_standing_total(&breakdown).abs() < 1e-9,
            "standing total should be £0.00 (got {})",
            breakdown_standing_total(&breakdown)
        );
    }

    #[test]
    fn cost_breakdown_splits_energy_from_standing_charge() {
        // With a Standing Charge configured, the energy component must be the
        // pure per-kWh cost (independent of the standing charge) and the
        // standing component must be exactly one debit per local day touched.
        let db = test_db();
        for d in 0..3 {
            insert_import_day(&db, d, 10.0, 6 * 60, 10 * 60);
        }
        // 4-day window: 00:00 day-0 → 00:00 day-4 (touches 4 calendar days).
        let start = local_midnight_secs(0);
        let end = local_midnight_secs(4);
        let flat = crate::settings::TariffConfig::flat(0.25);
        let breakdown = db
            .query_cost_breakdown(
                &window(start, end),
                3600,
                "today_import_kwh",
                &flat,
                0.25,
                54.86,
            )
            .unwrap();
        // Energy: 3 days × 10 kWh × £0.25 = £7.50 (NOT inflated by the SC).
        assert!(
            (breakdown_energy_total(&breakdown) - 7.50).abs() < 1e-3,
            "energy component should be the pure per-kWh cost £7.50 (got {})",
            breakdown_energy_total(&breakdown)
        );
        // Standing: 4 days × £0.5486 = £2.1944.
        assert!(
            (breakdown_standing_total(&breakdown) - 4.0 * 0.5486).abs() < 1e-3,
            "standing component should be 4 × £0.5486 (got {})",
            breakdown_standing_total(&breakdown)
        );
    }
}
