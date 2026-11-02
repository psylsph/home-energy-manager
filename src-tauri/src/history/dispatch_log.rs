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
//! when it started without a break. A gap in the checks (Octopus failing,
//! the hold off, a database error) or a clock that steps restarts that.
//! HEM being closed is not a gap: no usage is recorded then either.

use super::HistoryDb;

/// When the log started without a break, Unix seconds.
pub const DISPATCH_LOG_SINCE_KEY: &str = "octopus_dispatch_log_since";

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
    /// dispatches that have started, and when the log started.
    /// `previous_write` is this process's last successful write; `None` for
    /// the first, which allows for HEM having been closed.
    pub fn log_dispatches(
        &self,
        _sightings: &[DispatchSighting],
        _seen_at: i64,
        _previous_write: Option<i64>,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Logged dispatches still running after `since_ts`, oldest first.
    pub fn logged_dispatches_since(
        &self,
        _since_ts: i64,
        _seen_gap_secs: i64,
    ) -> Result<Vec<LoggedDispatch>, String> {
        Ok(Vec::new())
    }

    /// When the log started without a break, if it has.
    pub fn dispatch_log_since(&self) -> Option<i64> {
        None
    }

    /// Forget when the log started, as when the hold is turned off.
    pub fn forget_dispatch_log_since(&self) -> Result<(), String> {
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
    /// `sightings`, as one unbroken run after `previous`.
    fn checks(
        db: &HistoryDb,
        sightings: &[DispatchSighting],
        from: i64,
        to: i64,
        mut previous: Option<i64>,
    ) -> Option<i64> {
        let mut at = from;
        while at <= to {
            db.log_dispatches(sightings, at, previous).unwrap();
            previous = Some(at);
            at += GAP;
        }
        previous
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
        db.log_dispatches(&[sighting("car", T, T + 3600, Some(7.0))], T + 3600, None)
            .unwrap();
        assert_eq!(
            db.logged_dispatches_since(0, GAP).unwrap(),
            vec![logged("car", T, T + 3600, Some(7.0))]
        );
    }

    #[test]
    fn a_dispatch_the_list_repeats_is_one_row_with_its_latest_shape() {
        let (db, _path) = test_db();
        let last = checks(
            &db,
            &[sighting("car", T, T + 3600, Some(7.0))],
            T,
            T + 1800,
            None,
        );
        // Octopus stretches it and raises the energy part way through.
        checks(
            &db,
            &[sighting("car", T, T + 5400, Some(10.5))],
            T + 1980,
            T + 5400,
            last,
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
            None,
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
        db.log_dispatches(&[sighting("car", T + 3600, T + 7200, Some(7.0))], T, None)
            .unwrap();
        db.log_dispatches(
            &[sighting("car", T + 7200, T + 10800, Some(7.0))],
            T + 600,
            Some(T),
        )
        .unwrap();
        assert!(db.logged_dispatches_since(0, GAP).unwrap().is_empty());
        assert_eq!(db.dispatch_log_since(), Some(T), "the log is running");
    }

    #[test]
    fn a_dispatch_cancelled_mid_run_counts_until_just_after_it_was_last_seen() {
        let (db, _path) = test_db();
        // Seen from its start to 15 minutes in, then gone from the list.
        let last = checks(
            &db,
            &[sighting("car", T, T + 3600, Some(8.0))],
            T,
            T + 900,
            None,
        );
        checks(&db, &[], T + 1080, T + 3900, last);
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
        let last = checks(
            &db,
            &[sighting("car", T, T + 3600, Some(8.0))],
            T,
            T + 3240,
            None,
        );
        assert_eq!(last, Some(T + 3240));
        checks(&db, &[], T + 3700, T + 3700, last);
        assert_eq!(
            db.logged_dispatches_since(0, GAP).unwrap(),
            vec![logged("car", T, T + 3600, Some(8.0))]
        );
    }

    #[test]
    fn a_dispatch_last_seen_before_a_gap_is_not_assumed_to_have_run_in_full() {
        let (db, _path) = test_db();
        db.log_dispatches(&[sighting("car", T, T + 3600, Some(8.0))], T + 900, None)
            .unwrap();
        // The next good check is an hour later: it may have been cancelled.
        db.log_dispatches(&[], T + 4500, Some(T + 900)).unwrap();
        assert_eq!(
            db.logged_dispatches_since(0, GAP).unwrap(),
            vec![logged("car", T, T + 1080, Some(8.0 * 1080.0 / 3600.0))]
        );
    }

    #[test]
    fn a_running_dispatch_octopus_moves_ends_where_the_new_one_starts() {
        let (db, _path) = test_db();
        let last = checks(
            &db,
            &[sighting("car", T, T + 7200, Some(8.0))],
            T,
            T + 1800,
            None,
        );
        // Re-planned half an hour in to 00:30-02:30.
        checks(
            &db,
            &[sighting("car", T + 1800, T + 9000, Some(8.0))],
            T + 1980,
            T + 9000,
            last,
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
        db.log_dispatches(&[sighting("car", T, T + 3600, None)], T + 3590, None)
            .unwrap();
        // The clock steps back: the dispatch was still seen at T + 3590.
        db.log_dispatches(
            &[sighting("car", T, T + 3600, None)],
            T + 60,
            Some(T + 3590),
        )
        .unwrap();
        assert_eq!(
            db.logged_dispatches_since(0, GAP).unwrap(),
            vec![logged("car", T, T + 3600, None)]
        );
    }

    #[test]
    fn only_dispatches_still_running_after_the_start_are_read() {
        let (db, _path) = test_db();
        db.log_dispatches(&[sighting("car", T, T + 3600, Some(7.0))], T + 3600, None)
            .unwrap();
        db.log_dispatches(
            &[sighting("car", T + 7200, T + 9000, Some(3.5))],
            T + 9000,
            None,
        )
        .unwrap();
        let read = db.logged_dispatches_since(T + 3600, GAP).unwrap();
        assert_eq!(read, vec![logged("car", T + 7200, T + 9000, Some(3.5))]);
        assert_eq!(db.logged_dispatches_since(T + 3599, GAP).unwrap().len(), 2);
    }

    #[test]
    fn dispatches_older_than_the_keep_window_are_dropped_on_an_unbroken_run() {
        let (db, _path) = test_db();
        db.log_dispatches(&[sighting("car", T, T + 3600, Some(7.0))], T + 3600, None)
            .unwrap();
        let keep = DISPATCH_LOG_KEEP_DAYS * 86_400;
        // A first write with the clock far ahead (wrong at start-up) keeps it.
        db.log_dispatches(&[], T + 3601 + keep, None).unwrap();
        assert_eq!(db.logged_dispatches_since(0, GAP).unwrap().len(), 1);

        db.log_dispatches(&[], T + 3600 + keep, Some(T + 3600 + keep - GAP))
            .unwrap();
        assert_eq!(db.logged_dispatches_since(0, GAP).unwrap().len(), 1);
        db.log_dispatches(&[], T + 3601 + keep, Some(T + 3601 + keep - GAP))
            .unwrap();
        assert!(db.logged_dispatches_since(0, GAP).unwrap().is_empty());
    }

    #[test]
    fn the_log_keeps_its_start_while_the_checks_are_unbroken() {
        let (db, _path) = test_db();
        assert_eq!(db.dispatch_log_since(), None);
        checks(&db, &[], T, T + 86_400, None);
        assert_eq!(db.dispatch_log_since(), Some(T));
        // Half an hour between writes is still unbroken.
        db.log_dispatches(&[], T + 86_400 + MAX_LOG_GAP_SECS, Some(T + 86_400))
            .unwrap();
        assert_eq!(db.dispatch_log_since(), Some(T));
    }

    #[test]
    fn a_gap_in_the_checks_starts_the_log_again() {
        let (db, _path) = test_db();
        let last = checks(&db, &[], T, T + 3600, None).unwrap();
        // Octopus failed for over half an hour, or the hold was off.
        let after = last + MAX_LOG_GAP_SECS + 1;
        db.log_dispatches(&[], after, Some(last)).unwrap();
        assert_eq!(db.dispatch_log_since(), Some(after));
    }

    #[test]
    fn a_clock_step_starts_the_log_again() {
        let (db, _path) = test_db();
        checks(&db, &[], T, T + 600, None);
        // Stepped back a minute.
        db.log_dispatches(&[], T + 540, Some(T + 600)).unwrap();
        assert_eq!(db.dispatch_log_since(), Some(T + 540));
        // Stepped forward a month.
        db.log_dispatches(&[], T + 30 * 86_400, Some(T + 540))
            .unwrap();
        assert_eq!(db.dispatch_log_since(), Some(T + 30 * 86_400));
    }

    #[test]
    fn hem_being_closed_is_not_a_gap_unless_the_clock_went_back() {
        let (db, _path) = test_db();
        checks(&db, &[], T, T + 600, None);
        // Closed overnight: the first write of the new run keeps the start.
        db.log_dispatches(&[], T + 43_200, None).unwrap();
        assert_eq!(db.dispatch_log_since(), Some(T));
        // A start recorded with a clock that was ahead is brought back.
        db.log_dispatches(&[], T - 3600, None).unwrap();
        assert_eq!(db.dispatch_log_since(), Some(T - 3600));
    }

    #[test]
    fn forgetting_the_start_keeps_the_rows() {
        let (db, _path) = test_db();
        db.log_dispatches(&[sighting("car", T, T + 60, None)], T + 600, None)
            .unwrap();
        db.forget_dispatch_log_since().unwrap();
        assert_eq!(db.dispatch_log_since(), None);
        assert_eq!(db.logged_dispatches_since(0, GAP).unwrap().len(), 1);
        db.log_dispatches(&[], T + 900, Some(T + 600)).unwrap();
        assert_eq!(db.dispatch_log_since(), Some(T + 900));
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
                let mut previous = None;
                for round in 1..=300 {
                    let kwh = Some(round as f64);
                    let at = T + 3600 + round;
                    db.log_dispatches(
                        &[
                            sighting("car-a", T, T + 3600, kwh),
                            sighting("car-b", T, T + 3600, kwh),
                        ],
                        at,
                        previous,
                    )
                    .unwrap();
                    previous = Some(at);
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
