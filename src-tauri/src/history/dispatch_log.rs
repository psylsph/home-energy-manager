//! Intelligent Octopus dispatches that actually ran (issue #406).
//!
//! Households without a GivEnergy charger have the car's charging in their
//! usage history. The dispatch list says when Octopus charged the car and
//! how much, so the hold's poller logs every dispatch once it has started,
//! and the consumption profile can take that energy back out.
//!
//! A dispatch is one row per car and start time, updated each time the list
//! repeats it. Octopus can cancel a dispatch mid-run, after which it just
//! disappears from the list, so it counts as running only until shortly
//! after it was last seen; one that left the list after its planned end ran
//! in full.
//!
//! The log only helps once it covers the whole profile window, so it keeps
//! when it started without a break. Any gap in the writes restarts that:
//! Octopus failing, the hold off, a database error, a clock that steps, or
//! HEM being closed (the profile bridges short closures, and a dispatch
//! that ran while HEM was closed was never seen).

use rusqlite::{params, OptionalExtension};

use super::HistoryDb;

/// When the log started without a break, Unix seconds.
pub const DISPATCH_LOG_SINCE_KEY: &str = "octopus_dispatch_log_since";

/// When the log was last written, Unix seconds.
pub const DISPATCH_LOG_LAST_WRITE_KEY: &str = "octopus_dispatch_log_last_write";

/// How long logged dispatches are kept: the 28-day profile window with a
/// week's margin.
pub const DISPATCH_LOG_KEEP_DAYS: i64 = 35;

/// The longest time between two writes that still counts as unbroken.
/// Octopus dispatches last at least half an hour, so none can start and end
/// unseen inside a shorter gap.
pub const MAX_LOG_GAP_SECS: i64 = 30 * 60;

/// One dispatch as a check saw it, ready to log.
#[derive(Debug, Clone, PartialEq)]
pub struct DispatchSighting {
    pub device: String,
    /// Planned start and end, Unix seconds.
    pub start: i64,
    pub end: i64,
    pub kind: Option<String>,
    pub energy_kwh: Option<f64>,
}

/// A logged dispatch, cut to the time it really ran.
#[derive(Debug, Clone, PartialEq)]
pub struct LoggedDispatch {
    pub device: String,
    pub start: i64,
    /// The planned end, or sooner if it was cancelled or moved.
    pub end: i64,
    pub kind: Option<String>,
    /// The planned energy, pro rata to the time it ran.
    pub energy_kwh: Option<f64>,
}

impl HistoryDb {
    /// Log what a check at `seen_at` found, in one transaction: the
    /// dispatches that have started, and when the log started and was last
    /// written.
    pub fn log_dispatches(
        &self,
        sightings: &[DispatchSighting],
        seen_at: i64,
    ) -> Result<(), String> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        let fail = |e: rusqlite::Error| format!("Failed to log Octopus dispatches: {e}");
        let tx = conn.transaction().map_err(fail)?;
        let previous_write = tx
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM meta WHERE key = ?1",
                params![DISPATCH_LOG_LAST_WRITE_KEY],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(fail)?;
        let unbroken = previous_write
            .filter(|previous| (0..=MAX_LOG_GAP_SECS).contains(&(seen_at - previous)));
        if let Some(previous) = unbroken {
            // Seen at the last check and past its planned end now: it ran
            // to the end rather than being cancelled in the last moments.
            tx.execute(
                "UPDATE octopus_dispatch_log SET last_seen_at = end \
                 WHERE last_seen_at >= ?1 AND end <= ?2 AND end > last_seen_at",
                params![previous, seen_at],
            )
            .map_err(fail)?;
            // Pruned only on an unbroken run, so a clock that is wrong at
            // start-up can't delete the log.
            tx.execute(
                "DELETE FROM octopus_dispatch_log WHERE end < ?1",
                params![seen_at - DISPATCH_LOG_KEEP_DAYS * 86_400],
            )
            .map_err(fail)?;
        }
        {
            // A clock stepped back can't make a dispatch seen earlier than
            // it was, so `last_seen_at` only moves forward.
            let mut upsert = tx
                .prepare(
                    "INSERT INTO octopus_dispatch_log \
                       (device, start, end, kind, energy_kwh, last_seen_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                     ON CONFLICT(device, start) DO UPDATE SET \
                       end = excluded.end, kind = excluded.kind, \
                       energy_kwh = excluded.energy_kwh, \
                       last_seen_at = MAX(last_seen_at, excluded.last_seen_at)",
                )
                .map_err(fail)?;
            for sighting in sightings.iter().filter(|s| s.start <= seen_at) {
                upsert
                    .execute(params![
                        sighting.device,
                        sighting.start,
                        sighting.end,
                        sighting.kind,
                        sighting.energy_kwh,
                        seen_at,
                    ])
                    .map_err(fail)?;
            }
        }
        if unbroken.is_none() {
            // The first write, a gap or a step in the clock: start again.
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                params![DISPATCH_LOG_SINCE_KEY, seen_at.to_string()],
            )
            .map_err(fail)?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![DISPATCH_LOG_LAST_WRITE_KEY, seen_at.to_string()],
        )
        .map_err(fail)?;
        tx.commit().map_err(fail)
    }

    /// Logged dispatches still running after `since_ts`, oldest first. One
    /// counts until `seen_gap_secs` after it was last seen, or its planned
    /// end if that's sooner, and a car's dispatch ends where its next one
    /// starts (Octopus moved it).
    pub fn logged_dispatches_since(
        &self,
        since_ts: i64,
        seen_gap_secs: i64,
    ) -> Result<Vec<LoggedDispatch>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        let fail = |e: rusqlite::Error| format!("Failed to read Octopus dispatches: {e}");
        let mut stmt = conn
            .prepare(
                "SELECT device, start, end, kind, energy_kwh, last_seen_at \
                 FROM octopus_dispatch_log ORDER BY device ASC, start ASC",
            )
            .map_err(fail)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<f64>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(fail)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(fail)?;
        drop(stmt);
        drop(conn);

        let mut logged = Vec::new();
        for (i, (device, start, planned_end, kind, energy_kwh, last_seen_at)) in
            rows.iter().enumerate()
        {
            let mut end = (*planned_end).min(last_seen_at.saturating_add(seen_gap_secs));
            if let Some((next_device, next_start, ..)) = rows.get(i + 1) {
                if next_device == device {
                    end = end.min(*next_start);
                }
            }
            if end <= since_ts || end <= *start {
                continue;
            }
            // Octopus's dispatches always end after they start; a row that
            // doesn't has no energy to share out.
            let energy_kwh = energy_kwh
                .filter(|_| *planned_end > *start)
                .map(|kwh| kwh * (end - start) as f64 / (planned_end - start) as f64);
            logged.push(LoggedDispatch {
                device: device.clone(),
                start: *start,
                end,
                kind: kind.clone(),
                energy_kwh,
            });
        }
        logged.sort_by(|a, b| (a.start, &a.device).cmp(&(b.start, &b.device)));
        Ok(logged)
    }

    /// When the log started without a break, if it has.
    pub fn dispatch_log_since(&self) -> Option<i64> {
        self.get_meta_value(DISPATCH_LOG_SINCE_KEY)?.parse().ok()
    }

    /// Forget when the log started and was last written, as when the hold
    /// is turned off: the next write starts it again.
    pub fn forget_dispatch_log_since(&self) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        conn.execute(
            "DELETE FROM meta WHERE key IN (?1, ?2)",
            params![DISPATCH_LOG_SINCE_KEY, DISPATCH_LOG_LAST_WRITE_KEY],
        )
        .map_err(|e| format!("Failed to reset the Octopus dispatch log: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 1_790_000_000;
    const GAP: i64 = 180;

    fn test_db() -> (HistoryDb, std::path::PathBuf) {
        let path = crate::test_util::make_unique_test_dir("dispatch-log").join("h.db");
        (HistoryDb::open(&path).unwrap(), path)
    }

    fn sighting(device: &str, start: i64, end: i64, kwh: Option<f64>) -> DispatchSighting {
        DispatchSighting {
            device: device.to_string(),
            start,
            end,
            kind: Some("SMART".to_string()),
            energy_kwh: kwh,
        }
    }

    fn logged(device: &str, start: i64, end: i64, kwh: Option<f64>) -> LoggedDispatch {
        LoggedDispatch {
            device: device.to_string(),
            start,
            end,
            kind: Some("SMART".to_string()),
            energy_kwh: kwh,
        }
    }

    /// Checks every three minutes from `from` to `to`, all seeing
    /// `sightings`. Returns the last check's time.
    fn checks(db: &HistoryDb, sightings: &[DispatchSighting], from: i64, to: i64) -> i64 {
        let mut at = from;
        let mut last = from;
        while at <= to {
            db.log_dispatches(sightings, at).unwrap();
            last = at;
            at += GAP;
        }
        last
    }

    #[test]
    fn a_database_from_before_the_log_gets_the_table() {
        let (db, path) = test_db();
        drop(db);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE IF EXISTS octopus_dispatch_log")
            .unwrap();
        drop(conn);

        let db = HistoryDb::open(&path).unwrap();
        db.log_dispatches(&[sighting("car", T, T + 3600, Some(7.0))], T + 3600)
            .unwrap();
        assert_eq!(
            db.logged_dispatches_since(0, GAP).unwrap(),
            vec![logged("car", T, T + 3600, Some(7.0))]
        );
    }

    #[test]
    fn a_dispatch_the_list_repeats_is_one_row_with_its_latest_shape() {
        let (db, _path) = test_db();
        checks(&db, &[sighting("car", T, T + 3600, Some(7.0))], T, T + 1800);
        // Octopus stretches it and raises the energy part way through.
        checks(
            &db,
            &[sighting("car", T, T + 5400, Some(10.5))],
            T + 1980,
            T + 5400,
        );
        assert_eq!(
            db.logged_dispatches_since(0, GAP).unwrap(),
            vec![logged("car", T, T + 5400, Some(10.5))]
        );
    }

    #[test]
    fn each_car_has_its_own_row_for_the_same_slot() {
        let (db, _path) = test_db();
        db.log_dispatches(
            &[
                sighting("car-b", T, T + 3600, Some(3.5)),
                sighting("car-a", T, T + 3600, Some(7.0)),
            ],
            T + 3600,
        )
        .unwrap();
        assert_eq!(
            db.logged_dispatches_since(0, GAP).unwrap(),
            vec![
                logged("car-a", T, T + 3600, Some(7.0)),
                logged("car-b", T, T + 3600, Some(3.5)),
            ]
        );
    }

    #[test]
    fn a_dispatch_that_has_not_started_is_not_logged() {
        let (db, _path) = test_db();
        // Planned for later, then moved before it began: never logged.
        db.log_dispatches(&[sighting("car", T + 3600, T + 7200, Some(7.0))], T)
            .unwrap();
        db.log_dispatches(&[sighting("car", T + 7200, T + 10800, Some(7.0))], T + 600)
            .unwrap();
        assert!(db.logged_dispatches_since(0, GAP).unwrap().is_empty());
        assert_eq!(db.dispatch_log_since(), Some(T), "the log is running");
    }

    #[test]
    fn a_dispatch_cancelled_mid_run_counts_until_just_after_it_was_last_seen() {
        let (db, _path) = test_db();
        // Seen from its start to 15 minutes in, then gone from the list.
        checks(&db, &[sighting("car", T, T + 3600, Some(8.0))], T, T + 900);
        checks(&db, &[], T + 1080, T + 3900);
        // 15 minutes plus one check interval of the planned hour.
        assert_eq!(
            db.logged_dispatches_since(0, GAP).unwrap(),
            vec![logged("car", T, T + 1080, Some(8.0 * 1080.0 / 3600.0))]
        );
    }

    #[test]
    fn a_dispatch_gone_from_the_list_after_its_planned_end_ran_in_full() {
        let (db, _path) = test_db();
        // Last seen 6 minutes before its end; the next check, after the
        // end, no longer lists it.
        let last = checks(&db, &[sighting("car", T, T + 3600, Some(8.0))], T, T + 3240);
        assert_eq!(last, T + 3240);
        checks(&db, &[], T + 3700, T + 3700);
        assert_eq!(
            db.logged_dispatches_since(0, GAP).unwrap(),
            vec![logged("car", T, T + 3600, Some(8.0))]
        );
    }

    #[test]
    fn a_dispatch_last_seen_before_a_gap_is_not_assumed_to_have_run_in_full() {
        let (db, _path) = test_db();
        db.log_dispatches(&[sighting("car", T, T + 3600, Some(8.0))], T + 900)
            .unwrap();
        // The next good check is an hour later: it may have been cancelled.
        db.log_dispatches(&[], T + 4500).unwrap();
        assert_eq!(
            db.logged_dispatches_since(0, GAP).unwrap(),
            vec![logged("car", T, T + 1080, Some(8.0 * 1080.0 / 3600.0))]
        );
    }

    #[test]
    fn a_running_dispatch_octopus_moves_ends_where_the_new_one_starts() {
        let (db, _path) = test_db();
        checks(&db, &[sighting("car", T, T + 7200, Some(8.0))], T, T + 1800);
        // Re-planned half an hour in to 00:30-02:30.
        checks(
            &db,
            &[sighting("car", T + 1800, T + 9000, Some(8.0))],
            T + 1980,
            T + 9000,
        );
        assert_eq!(
            db.logged_dispatches_since(0, GAP).unwrap(),
            vec![
                logged("car", T, T + 1800, Some(8.0 * 1800.0 / 7200.0)),
                logged("car", T + 1800, T + 9000, Some(8.0)),
            ]
        );
    }

    #[test]
    fn a_dispatch_never_runs_past_its_planned_end_and_a_backwards_clock_cannot_shorten_it() {
        let (db, _path) = test_db();
        db.log_dispatches(&[sighting("car", T, T + 3600, None)], T + 3590)
            .unwrap();
        // The clock steps back: the dispatch was still seen at T + 3590.
        db.log_dispatches(&[sighting("car", T, T + 3600, None)], T + 60)
            .unwrap();
        assert_eq!(
            db.logged_dispatches_since(0, GAP).unwrap(),
            vec![logged("car", T, T + 3600, None)]
        );
    }

    #[test]
    fn only_dispatches_still_running_after_the_start_are_read() {
        let (db, _path) = test_db();
        db.log_dispatches(&[sighting("car", T, T + 3600, Some(7.0))], T + 3600)
            .unwrap();
        db.log_dispatches(&[sighting("car", T + 7200, T + 9000, Some(3.5))], T + 9000)
            .unwrap();
        let read = db.logged_dispatches_since(T + 3600, GAP).unwrap();
        assert_eq!(read, vec![logged("car", T + 7200, T + 9000, Some(3.5))]);
        assert_eq!(db.logged_dispatches_since(T + 3599, GAP).unwrap().len(), 2);
    }

    #[test]
    fn dispatches_older_than_the_keep_window_are_dropped_on_an_unbroken_run() {
        let (db, _path) = test_db();
        db.log_dispatches(&[sighting("car", T, T + 3600, Some(7.0))], T + 3600)
            .unwrap();
        let keep = DISPATCH_LOG_KEEP_DAYS * 86_400;
        // A write after a jump (a clock wrong at start-up) keeps it.
        db.log_dispatches(&[], T + 3600 + keep - GAP).unwrap();
        assert_eq!(db.logged_dispatches_since(0, GAP).unwrap().len(), 1);
        // Unbroken from there: dropped once it ended more than the window ago.
        db.log_dispatches(&[], T + 3600 + keep).unwrap();
        assert_eq!(db.logged_dispatches_since(0, GAP).unwrap().len(), 1);
        db.log_dispatches(&[], T + 3601 + keep).unwrap();
        assert!(db.logged_dispatches_since(0, GAP).unwrap().is_empty());
    }

    #[test]
    fn the_log_keeps_its_start_while_the_checks_are_unbroken() {
        let (db, _path) = test_db();
        assert_eq!(db.dispatch_log_since(), None);
        checks(&db, &[], T, T + 86_400);
        assert_eq!(db.dispatch_log_since(), Some(T));
        // Half an hour between writes is still unbroken.
        db.log_dispatches(&[], T + 86_400 + MAX_LOG_GAP_SECS)
            .unwrap();
        assert_eq!(db.dispatch_log_since(), Some(T));
    }

    #[test]
    fn a_gap_in_the_checks_starts_the_log_again() {
        let (db, _path) = test_db();
        let last = checks(&db, &[], T, T + 3600);
        // Octopus failed for over half an hour, the hold was off, or HEM
        // was closed.
        let after = last + MAX_LOG_GAP_SECS + 1;
        db.log_dispatches(&[], after).unwrap();
        assert_eq!(db.dispatch_log_since(), Some(after));
    }

    #[test]
    fn a_clock_step_starts_the_log_again() {
        let (db, _path) = test_db();
        let last = checks(&db, &[], T, T + 600);
        // Stepped back a minute.
        db.log_dispatches(&[], last - 60).unwrap();
        assert_eq!(db.dispatch_log_since(), Some(last - 60));
        // Stepped forward a month.
        db.log_dispatches(&[], T + 30 * 86_400).unwrap();
        assert_eq!(db.dispatch_log_since(), Some(T + 30 * 86_400));
    }

    #[test]
    fn a_quick_restart_keeps_the_log_and_a_longer_closure_starts_it_again() {
        // The last write is in the database, so a restarted HEM knows how
        // long it was gone.
        let (db, path) = test_db();
        let last = checks(&db, &[], T, T + 600);
        drop(db);
        let db = HistoryDb::open(&path).unwrap();
        let restarted = last + MAX_LOG_GAP_SECS;
        db.log_dispatches(&[], restarted).unwrap();
        assert_eq!(db.dispatch_log_since(), Some(T));
        // Closed for an hour and a half: the profile still bridges it, and a
        // dispatch could have run unseen.
        drop(db);
        let db = HistoryDb::open(&path).unwrap();
        let back = restarted + 5400;
        db.log_dispatches(&[], back).unwrap();
        assert_eq!(db.dispatch_log_since(), Some(back));
    }

    #[test]
    fn forgetting_the_start_keeps_the_rows() {
        let (db, _path) = test_db();
        db.log_dispatches(&[sighting("car", T, T + 60, None)], T + 600)
            .unwrap();
        db.forget_dispatch_log_since().unwrap();
        assert_eq!(db.dispatch_log_since(), None);
        assert_eq!(db.logged_dispatches_since(0, GAP).unwrap().len(), 1);
        // Back on within half an hour still starts again.
        db.log_dispatches(&[], T + 900).unwrap();
        assert_eq!(db.dispatch_log_since(), Some(T + 900));
    }

    #[test]
    fn a_poisoned_lock_is_an_error_not_a_panic() {
        let (db, _path) = test_db();
        let db = std::sync::Arc::new(db);
        let poisoner = db.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.conn.lock().unwrap();
            panic!("poison the connection lock");
        })
        .join();
        for result in [
            db.log_dispatches(&[], T),
            db.logged_dispatches_since(0, GAP).map(|_| ()),
            db.forget_dispatch_log_since(),
        ] {
            assert!(result.unwrap_err().contains("lock poisoned"));
        }
    }

    #[test]
    fn a_broken_database_is_reported_by_each_call() {
        let drop_table = |db: &HistoryDb, table: &str| {
            db.conn
                .lock()
                .unwrap()
                .execute_batch(&format!("DROP TABLE {table}"))
                .unwrap();
        };
        let (db, _path) = test_db();
        drop_table(&db, "octopus_dispatch_log");
        let error = db
            .log_dispatches(&[sighting("car", T, T + 60, None)], T + 60)
            .unwrap_err();
        assert!(
            error.starts_with("Failed to log Octopus dispatches"),
            "{error}"
        );
        let error = db.logged_dispatches_since(0, GAP).unwrap_err();
        assert!(
            error.starts_with("Failed to read Octopus dispatches"),
            "{error}"
        );

        let (db, _path) = test_db();
        drop_table(&db, "meta");
        let error = db.forget_dispatch_log_since().unwrap_err();
        assert!(
            error.starts_with("Failed to reset the Octopus dispatch log"),
            "{error}"
        );
        let error = db.log_dispatches(&[], T).unwrap_err();
        assert!(
            error.starts_with("Failed to log Octopus dispatches"),
            "{error}"
        );
    }

    #[test]
    fn a_row_that_cannot_be_read_is_reported() {
        let (db, _path) = test_db();
        db.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO octopus_dispatch_log (device, start, end, last_seen_at) \
                 VALUES ('car', 1, 2, 'later')",
                [],
            )
            .unwrap();
        let error = db.logged_dispatches_since(0, GAP).unwrap_err();
        assert!(
            error.starts_with("Failed to read Octopus dispatches"),
            "{error}"
        );
    }

    /// A check's write races profile reads on the blocking pool: a reader
    /// sees all of a check's rows or none, never half of them.
    #[test]
    fn a_reader_never_sees_half_of_a_check() {
        let (db, _path) = test_db();
        let db = std::sync::Arc::new(db);
        let writer = {
            let db = db.clone();
            std::thread::spawn(move || {
                for round in 1..=300 {
                    let kwh = Some(round as f64);
                    let at = T + 3600 + round;
                    db.log_dispatches(
                        &[
                            sighting("car-a", T, T + 3600, kwh),
                            sighting("car-b", T, T + 3600, kwh),
                        ],
                        at,
                    )
                    .unwrap();
                }
            })
        };
        while !writer.is_finished() {
            let rows = db.logged_dispatches_since(0, GAP).unwrap();
            if !rows.is_empty() {
                assert_eq!(rows.len(), 2, "both cars or neither: {rows:?}");
                assert_eq!(rows[0].energy_kwh, rows[1].energy_kwh, "{rows:?}");
            }
        }
        writer.join().unwrap();
        assert_eq!(
            db.logged_dispatches_since(0, GAP).unwrap()[0].energy_kwh,
            Some(300.0)
        );
    }
}
