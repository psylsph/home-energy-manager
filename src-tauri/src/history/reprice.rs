//! Re-pricing and backfilling the rates stamped on history rows (issue #455).
//!
//! Rows carry the rates in force when they were recorded
//! ([`super::RateStamp`]). Two jobs change that, both in chunks so the history
//! lock is never held for long, and each chunk one transaction so a crash
//! leaves every row either old or new:
//!
//! * **Re-price** rewrites the stamps of every row in a range from the
//!   settings given. It is the only path that overwrites a recorded value, and
//!   it only ever runs because the user asked for it.
//! * **Backfill** fills the rates a row does not have (`NULL`) from the
//!   supplier's dated rates in `octopus_tariff_prices`, which gives the exact
//!   historic rate rather than today's. It never changes a recorded value.
//!
//! Neither job touches rows newer than [`job_end`]: the live writer may still
//! rewrite the minutes it has open, and it stamps them itself.

use std::collections::HashMap;

use chrono::Timelike;
use rusqlite::params;

use super::{HistoryDb, RateStamp};
use crate::settings::Settings;

/// Rows rewritten per transaction.
pub const CHUNK_ROWS: usize = 5_000;

/// How far behind "now" a job stops. The live writer keeps three minutes open
/// and may rewrite their rows, so anything newer is left to it.
pub const LIVE_MARGIN_SECS: i64 = 4 * 60;

/// The exclusive upper bound (Unix seconds) of the rows a job started at
/// `now_ts` may change.
pub fn job_end(now_ts: i64) -> i64 {
    (now_ts - LIVE_MARGIN_SECS).div_euclid(60) * 60
}

/// What one chunk did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkOutcome {
    /// Rows the chunk looked at.
    pub rows: usize,
    /// Where the next chunk starts, or `None` when the range is finished.
    pub next: Option<i64>,
}

fn next_cursor(timestamps: &[i64], limit: usize) -> Option<i64> {
    if timestamps.len() < limit {
        return None;
    }
    timestamps.last().map(|t| t + 1)
}

impl HistoryDb {
    /// How many rows lie in `[from_ts, end_ts)`, for a job's progress.
    pub fn count_rows_between(&self, from_ts: i64, end_ts: i64) -> Result<u64, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        conn.query_row(
            "SELECT COUNT(*) FROM readings WHERE timestamp >= ?1 AND timestamp < ?2",
            params![from_ts, end_ts],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n.max(0) as u64)
        .map_err(|e| format!("Failed to count history rows: {e}"))
    }

    /// How many rows in `[from_ts, end_ts)` lack at least one rate, which is
    /// the work a backfill has to look at.
    pub fn count_rows_missing_rates_between(
        &self,
        from_ts: i64,
        end_ts: i64,
    ) -> Result<u64, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        conn.query_row(
            "SELECT COUNT(*) FROM readings WHERE timestamp >= ?1 AND timestamp < ?2 \
             AND (import_rate IS NULL OR export_rate IS NULL OR standing_charge_p_per_day IS NULL)",
            params![from_ts, end_ts],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n.max(0) as u64)
        .map_err(|e| format!("Failed to count history rows: {e}"))
    }

    /// Overwrite the stamped rates of up to `limit` rows in `[cursor, end)`
    /// with those `settings` give at each row's time, in the host's zone.
    pub fn reprice_chunk(
        &self,
        settings: &Settings,
        cursor: i64,
        end: i64,
        limit: usize,
    ) -> Result<ChunkOutcome, String> {
        self.reprice_chunk_in(settings, cursor, end, limit, &chrono::Local)
    }

    pub(crate) fn reprice_chunk_in<TZ: chrono::TimeZone>(
        &self,
        settings: &Settings,
        cursor: i64,
        end: i64,
        limit: usize,
        tz: &TZ,
    ) -> Result<ChunkOutcome, String> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        let tx = conn
            .transaction()
            .map_err(|e| format!("Failed to start re-price: {e}"))?;
        let timestamps = timestamps_in(&tx, cursor, end, limit, false)?;
        {
            let mut update = tx
                .prepare(
                    "UPDATE readings SET import_rate = ?1, export_rate = ?2, \
                     standing_charge_p_per_day = ?3 WHERE timestamp = ?4",
                )
                .map_err(|e| format!("Failed to prepare re-price: {e}"))?;
            for &ts in &timestamps {
                let stamp = RateStamp::resolve_in(settings, ts, tz);
                update
                    .execute(params![
                        stamp.import_rate,
                        stamp.export_rate,
                        stamp.standing_charge_p_per_day,
                        ts
                    ])
                    .map_err(|e| format!("Failed to re-price a history row: {e}"))?;
            }
        }
        tx.commit()
            .map_err(|e| format!("Failed to commit re-price: {e}"))?;
        Ok(ChunkOutcome {
            rows: timestamps.len(),
            next: next_cursor(&timestamps, limit),
        })
    }

    /// Fill the rates that up to `limit` rows in `[cursor, end)` lack (`NULL`)
    /// from the supplier's rates in `book`. A recorded value, zero included,
    /// is never changed; a rate the book cannot give stays `NULL`.
    pub fn backfill_chunk(
        &self,
        book: &PriceBook,
        cursor: i64,
        end: i64,
        limit: usize,
    ) -> Result<ChunkOutcome, String> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        let tx = conn
            .transaction()
            .map_err(|e| format!("Failed to start backfill: {e}"))?;
        let timestamps = timestamps_in(&tx, cursor, end, limit, true)?;
        {
            let mut update = tx
                .prepare(
                    "UPDATE readings SET \
                     import_rate = COALESCE(import_rate, ?1), \
                     export_rate = COALESCE(export_rate, ?2), \
                     standing_charge_p_per_day = COALESCE(standing_charge_p_per_day, ?3) \
                     WHERE timestamp = ?4",
                )
                .map_err(|e| format!("Failed to prepare backfill: {e}"))?;
            for &ts in &timestamps {
                let stamp = book.stamp_at(ts);
                // Nothing the supplier's rates can give this row: leave it, so
                // a run over history they don't cover writes nothing.
                if stamp == RateStamp::default() {
                    continue;
                }
                update
                    .execute(params![
                        stamp.import_rate,
                        stamp.export_rate,
                        stamp.standing_charge_p_per_day,
                        ts
                    ])
                    .map_err(|e| format!("Failed to backfill a history row: {e}"))?;
            }
        }
        tx.commit()
            .map_err(|e| format!("Failed to commit backfill: {e}"))?;
        Ok(ChunkOutcome {
            rows: timestamps.len(),
            next: next_cursor(&timestamps, limit),
        })
    }

    /// The supplier's electricity rates, for [`Self::backfill_chunk`]. The
    /// night window (minutes of the UK day) splits Economy 7 day and night
    /// rates, as the Octopus billing does.
    pub fn load_price_book(
        &self,
        night_start_minutes: u16,
        night_end_minutes: u16,
    ) -> Result<PriceBook, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        let mut stmt = conn
            .prepare(
                "SELECT meter_kind, meter_point, valid_from, valid_to, value_inc_vat, rate_type, \
                        tariff_code \
                 FROM octopus_tariff_prices \
                 WHERE meter_kind IN ('electricity_import', 'electricity_export') \
                 ORDER BY valid_from",
            )
            .map_err(|e| format!("Failed to prepare the Octopus rates query: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(|e| format!("Octopus rates query failed: {e}"))?;
        let mut book = PriceBook::new(night_start_minutes, night_end_minutes);
        for row in rows {
            let (kind, point, valid_from, valid_to, pence, rate_type, code) =
                row.map_err(|e| format!("Octopus rates query failed: {e}"))?;
            book.add_coded(
                &kind, &rate_type, &point, &code, valid_from, valid_to, pence,
            );
        }
        Ok(book)
    }
}

impl HistoryDb {
    /// The supplier's rate records for one meter kind (`electricity_import`,
    /// `electricity_export` or `gas`) that end after `since_ts` or never,
    /// for [`crate::octopus_tariff`] to derive Energy Tariffs from.
    pub fn octopus_rate_records(
        &self,
        meter_kind: &str,
        since_ts: i64,
    ) -> Result<Vec<crate::octopus_tariff::RateRecord>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        let mut stmt = conn
            .prepare(
                "SELECT meter_point, tariff_code, rate_type, valid_from, valid_to, value_inc_vat \
                 FROM octopus_tariff_prices \
                 WHERE meter_kind = ?1 AND (valid_to IS NULL OR valid_to > ?2) \
                 ORDER BY valid_from",
            )
            .map_err(|e| format!("Failed to prepare the Octopus rates query: {e}"))?;
        let rows = stmt
            .query_map(params![meter_kind, since_ts], |row| {
                Ok(crate::octopus_tariff::RateRecord {
                    meter_point: row.get(0)?,
                    tariff_code: row.get(1)?,
                    rate_type: row.get(2)?,
                    valid_from: row.get(3)?,
                    valid_to: row.get(4)?,
                    pence: row.get(5)?,
                })
            })
            .map_err(|e| format!("Octopus rates query failed: {e}"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| format!("Octopus rates query failed: {e}"))
    }
}

/// Up to `limit` row timestamps in `[cursor, end)`, oldest first, optionally
/// only rows lacking a rate.
fn timestamps_in(
    conn: &rusqlite::Connection,
    cursor: i64,
    end: i64,
    limit: usize,
    only_missing: bool,
) -> Result<Vec<i64>, String> {
    let filter = if only_missing {
        " AND (import_rate IS NULL OR export_rate IS NULL OR standing_charge_p_per_day IS NULL)"
    } else {
        ""
    };
    let sql = format!(
        "SELECT timestamp FROM readings WHERE timestamp >= ?1 AND timestamp < ?2{filter} \
         ORDER BY timestamp LIMIT ?3"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare the history row query: {e}"))?;
    let rows = stmt
        .query_map(params![cursor, end, limit as i64], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|e| format!("History row query failed: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("History row query failed: {e}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Part {
    Standard,
    Day,
    Night,
    Standing,
}

impl Part {
    pub(crate) fn parse(rate_type: &str) -> Option<Self> {
        match rate_type {
            "standard" => Some(Self::Standard),
            "day" => Some(Self::Day),
            "night" => Some(Self::Night),
            "standing" => Some(Self::Standing),
            _ => None,
        }
    }
}

/// One dated rate record.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Span {
    pub(crate) valid_from: i64,
    pub(crate) valid_to: Option<i64>,
    /// Pence per kWh, or per day for a standing charge.
    pub(crate) pence: f64,
    /// For a day or night rate: the hours (minutes of the UK day) the night
    /// rate applies, which depend on the tariff.
    pub(crate) night: (u16, u16),
}

/// What a rate lookup found.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Found {
    Nothing,
    /// The rate (pence) and the night hours of the record it came from.
    One(f64, (u16, u16)),
    /// More than one meter point had a rate at that moment: unknown which is
    /// the house's, so nothing is stamped.
    Ambiguous,
}

impl Found {
    fn pence(self) -> Option<f64> {
        match self {
            Found::One(pence, _) => Some(pence),
            _ => None,
        }
    }
}

/// How many overlapping records are looked through for one covering a moment.
const OVERLAP_LOOKBACK: usize = 8;

/// The newest record covering `ts` in `spans` (sorted by `valid_from`): the
/// last that has started and not yet ended, like the Octopus billing's
/// "newest active period". Looks back through a few overlapping records only.
pub(crate) fn newest_covering(spans: &[Span], ts: i64) -> Option<&Span> {
    let upto = spans.partition_point(|s| s.valid_from <= ts);
    spans[..upto]
        .iter()
        .rev()
        .take(OVERLAP_LOOKBACK)
        .find(|s| s.valid_to.is_none_or(|to| to > ts))
}

/// The supplier's dated rates, indexed for looking up the rate at an instant.
#[derive(Debug, Clone)]
pub struct PriceBook {
    /// `(is_export, part, meter point)` to that series, oldest first.
    series: HashMap<(bool, Part, String), Vec<Span>>,
    night_start: u16,
    night_end: u16,
}

impl PriceBook {
    pub fn new(night_start_minutes: u16, night_end_minutes: u16) -> Self {
        Self {
            series: HashMap::new(),
            night_start: night_start_minutes,
            night_end: night_end_minutes,
        }
    }

    /// [`Self::add`] for a record of a known tariff code, which decides the
    /// hours a day and night rate split at.
    #[allow(clippy::too_many_arguments)]
    pub fn add_coded(
        &mut self,
        meter_kind: &str,
        rate_type: &str,
        meter_point: &str,
        tariff_code: &str,
        valid_from: i64,
        valid_to: Option<i64>,
        pence: f64,
    ) {
        let night = crate::octopus_tariff::night_window_for(
            tariff_code,
            (self.night_start, self.night_end),
        );
        self.add_with_hours(
            meter_kind,
            rate_type,
            meter_point,
            night,
            valid_from,
            valid_to,
            pence,
        );
    }

    /// Add one rate record; unknown meter kinds and rate types, and rates that
    /// aren't finite non-negative numbers, are ignored.
    pub fn add(
        &mut self,
        meter_kind: &str,
        rate_type: &str,
        meter_point: &str,
        valid_from: i64,
        valid_to: Option<i64>,
        pence: f64,
    ) {
        let night = (self.night_start, self.night_end);
        self.add_with_hours(
            meter_kind,
            rate_type,
            meter_point,
            night,
            valid_from,
            valid_to,
            pence,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn add_with_hours(
        &mut self,
        meter_kind: &str,
        rate_type: &str,
        meter_point: &str,
        night: (u16, u16),
        valid_from: i64,
        valid_to: Option<i64>,
        pence: f64,
    ) {
        let is_export = match meter_kind {
            "electricity_import" => false,
            "electricity_export" => true,
            _ => return,
        };
        let Some(part) = Part::parse(rate_type) else {
            return;
        };
        if !pence.is_finite() || pence < 0.0 {
            return;
        }
        let spans = self
            .series
            .entry((is_export, part, meter_point.to_string()))
            .or_default();
        // Kept sorted by insertion, not by re-sorting the whole series on
        // every record (quadratic over a year of half-hourly rates). Records
        // normally arrive in order, so this is an append; equal start times
        // keep their arrival order.
        let at = spans.partition_point(|s| s.valid_from <= valid_from);
        spans.insert(
            at,
            Span {
                valid_from,
                valid_to,
                pence,
                night,
            },
        );
    }

    /// Whether `ts` falls in the night hours `hours` of the UK day.
    fn night_at(ts: i64, hours: (u16, u16)) -> bool {
        let Some(utc) = chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0) else {
            return false;
        };
        let local = utc.with_timezone(&chrono_tz::Europe::London);
        let minute = (local.hour() * 60 + local.minute()) as u16;
        crate::octopus_tariff::in_night_window(minute, hours)
    }

    /// The newest record covering `ts` for each meter point that has one,
    /// like the Octopus billing's "newest active period".
    fn find(&self, is_export: bool, part: Part, ts: i64) -> Found {
        let mut found: Option<(f64, (u16, u16))> = None;
        let mut points = 0;
        for ((export, p, _), spans) in &self.series {
            if *export != is_export || *p != part {
                continue;
            }
            if let Some(span) = newest_covering(spans, ts) {
                points += 1;
                found = Some((span.pence, span.night));
            }
        }
        match (points, found) {
            (0, _) | (_, None) => Found::Nothing,
            (1, Some((pence, night))) => Found::One(pence, night),
            _ => Found::Ambiguous,
        }
    }

    /// The unit rate for a direction at `ts`, in £/kWh: the standard rate,
    /// else the Economy 7 day or night rate for that time.
    fn unit_rate(&self, is_export: bool, ts: i64) -> Option<f64> {
        match self.find(is_export, Part::Standard, ts) {
            Found::One(pence, _) => return Some(pence / 100.0),
            Found::Ambiguous => return None,
            Found::Nothing => {}
        }
        // Day and night: the hours belong to the tariff whose rates cover
        // this moment, so an agreement change switches them with it.
        let hours = [Part::Day, Part::Night].into_iter().find_map(|part| {
            match self.find(is_export, part, ts) {
                Found::One(_, hours) => Some(Some(hours)),
                Found::Ambiguous => Some(None),
                Found::Nothing => None,
            }
        })??;
        let part = if Self::night_at(ts, hours) {
            Part::Night
        } else {
            Part::Day
        };
        self.find(is_export, part, ts)
            .pence()
            .map(|pence| pence / 100.0)
    }

    /// The rates the supplier charged at `ts`; each one the book cannot give
    /// is left `None`. Only the import side has a standing charge.
    pub fn stamp_at(&self, ts: i64) -> RateStamp {
        RateStamp {
            import_rate: self.unit_rate(false, ts),
            export_rate: self.unit_rate(true, ts),
            standing_charge_p_per_day: self.find(false, Part::Standing, ts).pence(),
        }
    }

    /// Whether the book holds no rates at all.
    pub fn is_empty(&self) -> bool {
        self.series.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::InverterSnapshot;
    use crate::settings::{TariffConfig, TariffSlot};

    /// 2026-10-21 12:00:00 UTC (BST, so 13:00 in London).
    const NOON: i64 = 1_792_584_000;

    fn test_db() -> HistoryDb {
        let dir = std::env::temp_dir().join(format!(
            "givenergy-reprice-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        HistoryDb::open(&dir.join("history.db")).unwrap()
    }

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// One row per minute from `NOON`, `n` of them, with no stamps.
    fn rows(db: &HistoryDb, n: i64) {
        for i in 0..n {
            db.insert_reading(&InverterSnapshot {
                timestamp: NOON + i * 60,
                soc: 50,
                ..InverterSnapshot::default()
            });
        }
    }

    type Stamps = Vec<(i64, Option<f64>, Option<f64>, Option<f64>)>;

    fn stamps(db: &HistoryDb) -> Stamps {
        let conn = db.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT timestamp, import_rate, export_rate, standing_charge_p_per_day \
                 FROM readings ORDER BY timestamp",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn set_stamp(db: &HistoryDb, ts: i64, import: Option<f64>, export: Option<f64>) {
        db.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE readings SET import_rate = ?1, export_rate = ?2 WHERE timestamp = ?3",
                params![import, export, ts],
            )
            .unwrap();
    }

    fn flat_settings(import: f64, export: f64, standing: f64) -> Settings {
        Settings {
            import_tariff: import,
            export_tariff: export,
            import_standing_charge_p_per_day: standing,
            import_tariff_config: None,
            export_tariff_config: None,
            ..Settings::default()
        }
    }

    // ---- re-price ----

    #[test]
    fn repricing_rewrites_every_row_in_the_range_from_the_settings_given() {
        let db = test_db();
        rows(&db, 4);
        set_stamp(&db, NOON, Some(0.99), Some(0.99));
        let outcome = db
            .reprice_chunk_in(
                &flat_settings(0.30, 0.15, 54.86),
                NOON,
                NOON + 4 * 60,
                100,
                &chrono::Utc,
            )
            .unwrap();
        assert_eq!(
            outcome,
            ChunkOutcome {
                rows: 4,
                next: None
            }
        );
        for (_, import, export, standing) in stamps(&db) {
            assert_eq!(
                (import, export, standing),
                (Some(0.30), Some(0.15), Some(54.86))
            );
        }
    }

    #[test]
    fn repricing_never_touches_rows_before_the_start_or_from_the_end() {
        let db = test_db();
        rows(&db, 6);
        for i in 0..6 {
            set_stamp(&db, NOON + i * 60, Some(0.99), Some(0.99));
        }
        db.reprice_chunk_in(
            &flat_settings(0.30, 0.15, 0.0),
            NOON + 2 * 60,
            NOON + 5 * 60,
            100,
            &chrono::Utc,
        )
        .unwrap();
        let imports: Vec<Option<f64>> = stamps(&db).iter().map(|r| r.1).collect();
        assert_eq!(
            imports,
            vec![
                Some(0.99),
                Some(0.99),
                Some(0.30),
                Some(0.30),
                Some(0.30),
                Some(0.99)
            ]
        );
    }

    #[test]
    fn repricing_in_chunks_matches_one_pass_and_resumes_from_the_cursor() {
        let one = test_db();
        let chunked = test_db();
        rows(&one, 10);
        rows(&chunked, 10);
        let settings = flat_settings(0.25, 0.10, 40.0);
        one.reprice_chunk_in(&settings, NOON, NOON + 10 * 60, 100, &chrono::Utc)
            .unwrap();

        let mut cursor = NOON;
        let mut chunks = 0;
        loop {
            let outcome = chunked
                .reprice_chunk_in(&settings, cursor, NOON + 10 * 60, 3, &chrono::Utc)
                .unwrap();
            chunks += 1;
            match outcome.next {
                Some(next) => cursor = next,
                None => break,
            }
        }
        assert_eq!(chunks, 4, "3 + 3 + 3 + 1 rows");
        assert_eq!(stamps(&one), stamps(&chunked));
    }

    #[test]
    fn a_stopped_job_leaves_whole_chunks_only() {
        // Two chunks of three, then the job is abandoned: the first six rows
        // are new, the rest exactly as they were.
        let db = test_db();
        rows(&db, 10);
        for i in 0..10 {
            set_stamp(&db, NOON + i * 60, Some(0.99), Some(0.99));
        }
        let settings = flat_settings(0.30, 0.15, 0.0);
        let mut cursor = NOON;
        for _ in 0..2 {
            cursor = db
                .reprice_chunk_in(&settings, cursor, NOON + 10 * 60, 3, &chrono::Utc)
                .unwrap()
                .next
                .unwrap();
        }
        let imports: Vec<Option<f64>> = stamps(&db).iter().map(|r| r.1).collect();
        assert_eq!(&imports[..6], &[Some(0.30); 6]);
        assert_eq!(&imports[6..], &[Some(0.99); 4]);
    }

    #[test]
    fn repricing_is_idempotent() {
        let db = test_db();
        rows(&db, 5);
        let settings = flat_settings(0.30, 0.15, 10.0);
        let run = |db: &HistoryDb| {
            db.reprice_chunk_in(&settings, NOON, NOON + 5 * 60, 100, &chrono::Utc)
                .unwrap()
        };
        run(&db);
        let first = stamps(&db);
        run(&db);
        assert_eq!(stamps(&db), first);
    }

    #[test]
    fn repricing_follows_a_time_of_use_tariff_by_each_rows_minute() {
        let db = test_db();
        rows(&db, 3);
        let settings = Settings {
            import_tariff_config: Some(TariffConfig {
                slots: vec![
                    TariffSlot {
                        start: "00:00".into(),
                        end: "12:01".into(),
                        rate: 0.07,
                    },
                    TariffSlot {
                        start: "12:01".into(),
                        end: "23:59".into(),
                        rate: 0.30,
                    },
                ],
            }),
            ..flat_settings(0.30, 0.15, 0.0)
        };
        db.reprice_chunk_in(&settings, NOON, NOON + 3 * 60, 100, &chrono::Utc)
            .unwrap();
        let imports: Vec<Option<f64>> = stamps(&db).iter().map(|r| r.1).collect();
        assert_eq!(
            imports,
            vec![Some(0.07), Some(0.30), Some(0.30)],
            "12:00 is in [00:00, 12:01); 12:01 and 12:02 are not"
        );
    }

    #[test]
    fn a_rate_the_settings_cannot_give_is_cleared_not_guessed() {
        let db = test_db();
        rows(&db, 1);
        set_stamp(&db, NOON, Some(0.99), Some(0.99));
        let settings = Settings {
            export_tariff: f64::NAN,
            ..flat_settings(0.30, 0.15, 0.0)
        };
        db.reprice_chunk_in(&settings, NOON, NOON + 60, 100, &chrono::Utc)
            .unwrap();
        let row = &stamps(&db)[0];
        assert_eq!((row.1, row.2), (Some(0.30), None));
    }

    #[test]
    fn job_end_stops_short_of_the_minutes_the_live_writer_may_rewrite() {
        // 12:07:30 minus four minutes, to the minute: 12:03.
        assert_eq!(job_end(NOON + 7 * 60 + 30), NOON + 3 * 60);
        assert_eq!(job_end(NOON + 4 * 60), NOON);
    }

    #[test]
    fn counting_rows_gives_a_jobs_total() {
        let db = test_db();
        rows(&db, 7);
        assert_eq!(db.count_rows_between(NOON, NOON + 5 * 60).unwrap(), 5);
        assert_eq!(
            db.count_rows_between(NOON + 100 * 60, NOON + 200 * 60)
                .unwrap(),
            0
        );
    }

    // ---- backfill ----

    fn book() -> PriceBook {
        // Night window 00:30–07:30, as the default.
        PriceBook::new(30, 450)
    }

    #[test]
    fn backfill_gives_the_rate_in_force_at_each_row_not_todays() {
        // A price change at NOON + 2 min: rows before are 20p, after 25p.
        let mut b = book();
        b.add(
            "electricity_import",
            "standard",
            "mp1",
            NOON - 86_400,
            Some(NOON + 120),
            20.0,
        );
        b.add(
            "electricity_import",
            "standard",
            "mp1",
            NOON + 120,
            None,
            25.0,
        );
        let db = test_db();
        rows(&db, 4);
        db.backfill_chunk(&b, NOON, NOON + 4 * 60, 100).unwrap();
        let imports: Vec<Option<f64>> = stamps(&db).iter().map(|r| r.1).collect();
        assert_eq!(
            imports,
            vec![Some(0.20), Some(0.20), Some(0.25), Some(0.25)]
        );
    }

    #[test]
    fn backfill_converts_pence_to_pounds_and_keeps_the_standing_charge_in_pence() {
        let mut b = book();
        b.add("electricity_import", "standard", "mp1", 0, None, 28.5);
        b.add("electricity_import", "standing", "mp1", 0, None, 54.86);
        b.add("electricity_export", "standard", "mp2", 0, None, 15.0);
        let db = test_db();
        rows(&db, 1);
        db.backfill_chunk(&b, NOON, NOON + 60, 100).unwrap();
        assert_eq!(
            stamps(&db),
            vec![(NOON, Some(0.285), Some(0.15), Some(54.86))]
        );
    }

    #[test]
    fn backfill_never_changes_a_recorded_value_and_zero_counts_as_recorded() {
        let mut b = book();
        b.add("electricity_import", "standard", "mp1", 0, None, 20.0);
        b.add("electricity_export", "standard", "mp2", 0, None, 15.0);
        let db = test_db();
        rows(&db, 3);
        set_stamp(&db, NOON, Some(0.99), Some(0.0)); // both recorded
        set_stamp(&db, NOON + 60, Some(0.99), None); // import recorded only
        db.backfill_chunk(&b, NOON, NOON + 3 * 60, 100).unwrap();
        let got = stamps(&db);
        assert_eq!((got[0].1, got[0].2), (Some(0.99), Some(0.0)));
        assert_eq!((got[1].1, got[1].2), (Some(0.99), Some(0.15)));
        assert_eq!((got[2].1, got[2].2), (Some(0.20), Some(0.15)));
    }

    #[test]
    fn rows_outside_any_known_rate_stay_unrecorded() {
        let mut b = book();
        b.add(
            "electricity_import",
            "standard",
            "mp1",
            NOON + 60,
            Some(NOON + 120),
            20.0,
        );
        let db = test_db();
        rows(&db, 3);
        db.backfill_chunk(&b, NOON, NOON + 3 * 60, 100).unwrap();
        let imports: Vec<Option<f64>> = stamps(&db).iter().map(|r| r.1).collect();
        assert_eq!(imports, vec![None, Some(0.20), None]);
    }

    #[test]
    fn economy_7_rows_take_the_day_or_night_rate_for_their_uk_time() {
        // 2026-01-15 (GMT): 00:15 is before the 00:30 night start, 03:00 is
        // night, 12:00 is day.
        let jan15 = chrono::DateTime::parse_from_rfc3339("2026-01-15T00:00:00Z")
            .unwrap()
            .timestamp();
        let mut b = book();
        b.add("electricity_import", "day", "mp1", 0, None, 30.0);
        b.add("electricity_import", "night", "mp1", 0, None, 10.0);
        let db = test_db();
        for offset in [15 * 60, 3 * 3600, 12 * 3600] {
            db.insert_reading(&InverterSnapshot {
                timestamp: jan15 + offset,
                soc: 50,
                ..InverterSnapshot::default()
            });
        }
        db.backfill_chunk(&b, jan15, jan15 + 86_400, 100).unwrap();
        let imports: Vec<Option<f64>> = stamps(&db).iter().map(|r| r.1).collect();
        assert_eq!(imports, vec![Some(0.30), Some(0.10), Some(0.30)]);
    }

    #[test]
    fn economy_7_night_follows_london_summer_time() {
        // In BST 23:45 UTC is 00:45 UK time, which is in the 00:30 night.
        let june = chrono::DateTime::parse_from_rfc3339("2026-06-15T23:45:00Z")
            .unwrap()
            .timestamp();
        let mut b = book();
        b.add("electricity_import", "day", "mp1", 0, None, 30.0);
        b.add("electricity_import", "night", "mp1", 0, None, 10.0);
        assert_eq!(b.stamp_at(june).import_rate, Some(0.10));
        assert_eq!(b.stamp_at(june - 3600).import_rate, Some(0.30));
    }

    #[test]
    fn intelligent_go_rows_take_the_night_rate_between_2330_and_0530() {
        // 2026-01-15 GMT. The Economy 7 hours (00:30-07:30) must not apply:
        // 23:45 and 05:15 are night, 06:00 is day.
        let jan15 = chrono::DateTime::parse_from_rfc3339("2026-01-15T00:00:00Z")
            .unwrap()
            .timestamp();
        let mut b = book();
        let code = "E-1R-IOG-SMB-VAR-24-10-29-H";
        b.add_coded("electricity_import", "day", "mp1", code, 0, None, 36.0);
        b.add_coded("electricity_import", "night", "mp1", code, 0, None, 6.6);
        let rate = |minute: i64| b.stamp_at(jan15 + minute * 60).import_rate;
        assert_eq!(rate(23 * 60 + 45), Some(0.066));
        assert_eq!(rate(5 * 60 + 15), Some(0.066));
        assert_eq!(rate(6 * 60), Some(0.36));
        assert_eq!(rate(23 * 60 + 29), Some(0.36), "night starts at 23:30");
        assert_eq!(rate(5 * 60 + 30), Some(0.36), "night ends at 05:30");
    }

    #[test]
    fn economy_7_rows_keep_the_households_hours_when_the_code_is_known() {
        let jan15 = chrono::DateTime::parse_from_rfc3339("2026-01-15T00:00:00Z")
            .unwrap()
            .timestamp();
        let mut b = book();
        let code = "E-2R-VAR-22-11-01-H";
        b.add_coded("electricity_import", "day", "mp1", code, 0, None, 30.0);
        b.add_coded("electricity_import", "night", "mp1", code, 0, None, 10.0);
        assert_eq!(b.stamp_at(jan15 + 3 * 3600).import_rate, Some(0.10));
        assert_eq!(
            b.stamp_at(jan15 + 23 * 3600 + 45 * 60).import_rate,
            Some(0.30)
        );
    }

    #[test]
    fn an_agreement_change_switches_the_hours_with_the_tariff() {
        // Economy 7 until a cut-over, Intelligent Go after it.
        let jan15 = chrono::DateTime::parse_from_rfc3339("2026-01-15T00:00:00Z")
            .unwrap()
            .timestamp();
        let cut = jan15 + 86_400;
        let mut b = book();
        let e7 = "E-2R-VAR-22-11-01-H";
        let iog = "E-1R-IOG-SMB-VAR-24-10-29-H";
        b.add_coded("electricity_import", "day", "mp1", e7, 0, Some(cut), 30.0);
        b.add_coded("electricity_import", "night", "mp1", e7, 0, Some(cut), 10.0);
        b.add_coded("electricity_import", "day", "mp1", iog, cut, None, 36.0);
        b.add_coded("electricity_import", "night", "mp1", iog, cut, None, 6.6);
        // 23:45 on the 15th: Economy 7 day. 23:45 on the 16th: Go night.
        assert_eq!(
            b.stamp_at(jan15 + 23 * 3600 + 45 * 60).import_rate,
            Some(0.30)
        );
        assert_eq!(
            b.stamp_at(cut + 23 * 3600 + 45 * 60).import_rate,
            Some(0.066)
        );
    }

    #[test]
    fn two_meter_points_covering_a_row_leave_it_unrecorded() {
        let mut b = book();
        b.add("electricity_import", "standard", "mp1", 0, None, 20.0);
        b.add("electricity_import", "standard", "mp2", 0, None, 22.0);
        assert_eq!(b.stamp_at(NOON).import_rate, None);
    }

    #[test]
    fn records_added_in_any_order_resolve_the_same() {
        let mut ordered = book();
        let mut shuffled = book();
        let records: Vec<(i64, Option<i64>, f64)> = (0..50)
            .map(|i| (i * 1800, Some((i + 1) * 1800), 10.0 + i as f64))
            .collect();
        for &(from, to, pence) in &records {
            ordered.add("electricity_import", "standard", "mp1", from, to, pence);
        }
        // Back to front, then the middle out.
        for &(from, to, pence) in records.iter().rev() {
            shuffled.add("electricity_import", "standard", "mp1", from, to, pence);
        }
        for ts in [0, 1799, 1800, 40_000, 49 * 1800 + 5, 50 * 1800] {
            assert_eq!(ordered.stamp_at(ts), shuffled.stamp_at(ts), "at {ts}");
        }
        assert_eq!(shuffled.stamp_at(3 * 1800 + 1).import_rate, Some(0.13));
    }

    #[test]
    fn overlapping_records_for_one_meter_point_use_the_newest() {
        let mut b = book();
        b.add(
            "electricity_import",
            "standard",
            "mp1",
            0,
            Some(NOON + 3600),
            20.0,
        );
        b.add(
            "electricity_import",
            "standard",
            "mp1",
            NOON - 60,
            None,
            25.0,
        );
        assert_eq!(b.stamp_at(NOON).import_rate, Some(0.25));
    }

    #[test]
    fn a_long_standing_charge_is_found_past_many_newer_short_records() {
        let mut b = book();
        b.add("electricity_import", "standing", "mp1", 0, None, 50.0);
        for i in 0..20 {
            b.add(
                "electricity_import",
                "standard",
                "mp1",
                i * 1800,
                Some((i + 1) * 1800),
                20.0,
            );
        }
        assert_eq!(b.stamp_at(NOON).standing_charge_p_per_day, Some(50.0));
    }

    #[test]
    fn invalid_supplier_rates_and_other_meters_are_ignored() {
        let mut b = book();
        b.add("gas", "standard", "g1", 0, None, 10.0);
        b.add("electricity_import", "weird", "mp1", 0, None, 10.0);
        b.add("electricity_import", "standard", "mp1", 0, None, f64::NAN);
        b.add("electricity_import", "standard", "mp1", 0, None, -5.0);
        assert!(b.is_empty());
        assert_eq!(b.stamp_at(NOON), RateStamp::default());
    }

    #[test]
    fn backfill_in_chunks_resumes_and_finishes_even_when_nothing_can_be_filled() {
        // Nothing is covered, so every row stays NULL; the cursor still moves
        // past them and the job ends.
        let b = book();
        let db = test_db();
        rows(&db, 7);
        let mut cursor = NOON;
        let mut chunks = 0;
        loop {
            let outcome = db.backfill_chunk(&b, cursor, NOON + 7 * 60, 3).unwrap();
            chunks += 1;
            assert!(chunks < 10, "the cursor must advance");
            match outcome.next {
                Some(next) => cursor = next,
                None => break,
            }
        }
        assert_eq!(chunks, 3, "3 + 3 + 1 rows");
        assert!(stamps(&db).iter().all(|r| r.1.is_none()));
    }

    #[test]
    fn backfill_writes_nothing_for_rows_the_rates_cannot_fill() {
        // A household with no export meter, or history older than the
        // downloaded rates, would otherwise rewrite every such row on every
        // run. Any UPDATE aborts here, so one being attempted fails the job.
        let mut b = book();
        b.add(
            "electricity_import",
            "standard",
            "mp1",
            NOON + 10 * 60,
            None,
            20.0,
        );
        let db = test_db();
        rows(&db, 5);
        db.conn
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER no_updates BEFORE UPDATE ON readings \
                 BEGIN SELECT RAISE(ABORT, 'update attempted'); END;",
            )
            .unwrap();
        let outcome = db
            .backfill_chunk(&b, NOON, NOON + 5 * 60, 100)
            .expect("no row has a rate to fill, so none should be written");
        assert_eq!(outcome.rows, 5, "the rows are still passed over");
    }

    #[test]
    fn backfill_skips_fully_recorded_rows_so_a_rerun_does_no_work() {
        let mut b = book();
        b.add("electricity_import", "standard", "mp1", 0, None, 20.0);
        b.add("electricity_import", "standing", "mp1", 0, None, 50.0);
        b.add("electricity_export", "standard", "mp2", 0, None, 15.0);
        let db = test_db();
        rows(&db, 5);
        let first = db.backfill_chunk(&b, NOON, NOON + 5 * 60, 100).unwrap();
        assert_eq!(first.rows, 5);
        let second = db.backfill_chunk(&b, NOON, NOON + 5 * 60, 100).unwrap();
        assert_eq!(second.rows, 0);
    }

    #[test]
    fn the_price_book_loads_from_the_database() {
        let db = test_db();
        db.upsert_octopus_tariff_prices(&[
            super::super::OctopusTariffPriceRow {
                meter_kind: "electricity_import".into(),
                meter_point: "mp1".into(),
                tariff_code: "E-1R-X".into(),
                valid_from: 0,
                valid_to: None,
                value_inc_vat: 24.5,
                rate_type: "standard".into(),
            },
            super::super::OctopusTariffPriceRow {
                meter_kind: "gas".into(),
                meter_point: "g1".into(),
                tariff_code: "G-1R-X".into(),
                valid_from: 0,
                valid_to: None,
                value_inc_vat: 6.0,
                rate_type: "standard".into(),
            },
        ])
        .unwrap();
        let book = db.load_price_book(30, 450).unwrap();
        assert_eq!(book.stamp_at(NOON).import_rate, Some(0.245));
        assert_eq!(book.stamp_at(NOON).export_rate, None);
    }
}
