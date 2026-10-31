//! EV charger power per minute (issue #400).
//!
//! The consumption profile takes the car's charging out of home load, so the
//! planner doesn't size every night's charge for an evening the car happened
//! to be plugged in. The charger's polls fold into one average per wall-clock
//! minute in `evc_power_minutes`; a minute that is written twice (the clock
//! stepped back into it) merges by sample count, so no polls are lost.
//!
//! The charger's own lifetime meter (HR 29) isn't used: it counts in 0.1 kWh
//! and rolls over at 6553.5 kWh.

use rusqlite::params;

use super::minute::BUCKET_SECS;
use super::HistoryDb;
use crate::forecast::consumption::EvPowerMinute;

/// The most a home charger can draw (22 kW three-phase, with margin). A
/// larger or negative reading is a corrupt read and is not recorded.
pub const MAX_CHARGER_POWER_W: i32 = 23_000;

/// One finished minute, ready to store.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EvcMinuteRow {
    /// Start of the minute, Unix seconds.
    pub timestamp: i64,
    pub power_w: f64,
    pub samples: u32,
}

/// Folds charger polls into per-minute averages.
#[derive(Debug, Default)]
pub struct EvcMinuteAccumulator {
    open: Option<EvcMinuteRow>,
}

impl EvcMinuteAccumulator {
    /// Add a poll. Returns the previous minute once a poll lands in another
    /// one; a reading out of range is ignored.
    pub fn push(&mut self, timestamp: i64, power_w: i32) -> Option<EvcMinuteRow> {
        if !(0..=MAX_CHARGER_POWER_W).contains(&power_w) {
            return None;
        }
        let minute = timestamp.div_euclid(BUCKET_SECS) * BUCKET_SECS;
        match self.open.as_mut() {
            Some(open) if open.timestamp == minute => {
                let n = open.samples as f64;
                open.power_w = (open.power_w * n + power_w as f64) / (n + 1.0);
                open.samples += 1;
                None
            }
            _ => self.open.replace(EvcMinuteRow {
                timestamp: minute,
                power_w: power_w as f64,
                samples: 1,
            }),
        }
    }
}

impl HistoryDb {
    /// Store a finished minute, merging with a row already stored for it.
    pub fn record_evc_power_minute(&self, row: &EvcMinuteRow) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        conn.execute(
            "INSERT INTO evc_power_minutes (timestamp, power_w, samples) VALUES (?1, ?2, ?3) \
             ON CONFLICT(timestamp) DO UPDATE SET \
               power_w = (power_w * samples + excluded.power_w * excluded.samples) \
                         / (samples + excluded.samples), \
               samples = samples + excluded.samples",
            params![row.timestamp, row.power_w, row.samples],
        )
        .map_err(|e| format!("Failed to store EV charger power: {e}"))?;
        Ok(())
    }

    /// Charger minutes starting at or after `since_ts`, oldest first.
    pub fn evc_power_minutes_since(&self, since_ts: i64) -> Result<Vec<EvPowerMinute>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        let mut stmt = conn
            .prepare(
                "SELECT timestamp, power_w FROM evc_power_minutes \
                 WHERE timestamp >= ?1 ORDER BY timestamp ASC",
            )
            .map_err(|e| format!("Failed to prepare EV charger power query: {e}"))?;
        let rows = stmt
            .query_map(params![since_ts], |row| {
                Ok(EvPowerMinute {
                    timestamp: row.get(0)?,
                    power_w: row.get(1)?,
                })
            })
            .map_err(|e| format!("EV charger power query failed: {e}"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| format!("Failed to read EV charger power: {e}"))?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> HistoryDb {
        HistoryDb::open(&crate::test_util::make_unique_test_dir("evc-power").join("h.db")).unwrap()
    }

    const T: i64 = 1_790_000_040; // a minute boundary

    #[test]
    fn polls_in_one_minute_average_and_the_minute_is_written_when_the_next_starts() {
        let mut acc = EvcMinuteAccumulator::default();
        assert_eq!(acc.push(T, 7_000), None);
        assert_eq!(acc.push(T + 10, 7_200), None);
        assert_eq!(acc.push(T + 59, 6_800), None);
        let done = acc.push(T + 60, 0).expect("minute finished");
        assert_eq!(done.timestamp, T);
        assert_eq!(done.samples, 3);
        assert!((done.power_w - 7_000.0).abs() < 1e-9);
    }

    #[test]
    fn minutes_start_on_the_minute() {
        let mut acc = EvcMinuteAccumulator::default();
        acc.push(T + 25, 1_000);
        assert_eq!(acc.push(T + 85, 1_000).unwrap().timestamp, T);
    }

    #[test]
    fn corrupt_readings_are_ignored() {
        let mut acc = EvcMinuteAccumulator::default();
        acc.push(T, 7_000);
        assert_eq!(acc.push(T + 10, -5), None);
        assert_eq!(acc.push(T + 20, 65_535), None);
        assert_eq!(acc.push(T + 70, 0).unwrap().power_w, 7_000.0);
    }

    #[test]
    fn a_clock_step_back_writes_the_open_minute_and_starts_another() {
        let mut acc = EvcMinuteAccumulator::default();
        acc.push(T + 120, 3_000);
        let done = acc.push(T, 1_000).unwrap();
        assert_eq!(done.timestamp, T + 120);
    }

    #[test]
    fn stored_minutes_read_back_in_order_from_a_start() {
        let db = test_db();
        for (i, w) in [(2, 300.0), (0, 7_000.0), (1, 6_000.0)] {
            db.record_evc_power_minute(&EvcMinuteRow {
                timestamp: T + i * 60,
                power_w: w,
                samples: 6,
            })
            .unwrap();
        }
        let rows = db.evc_power_minutes_since(T + 60).unwrap();
        assert_eq!(
            rows,
            vec![
                EvPowerMinute {
                    timestamp: T + 60,
                    power_w: 6_000.0
                },
                EvPowerMinute {
                    timestamp: T + 120,
                    power_w: 300.0
                },
            ]
        );
    }

    #[test]
    fn a_minute_written_twice_keeps_every_poll() {
        let db = test_db();
        let first = EvcMinuteRow {
            timestamp: T,
            power_w: 6_000.0,
            samples: 3,
        };
        let again = EvcMinuteRow {
            timestamp: T,
            power_w: 0.0,
            samples: 1,
        };
        db.record_evc_power_minute(&first).unwrap();
        db.record_evc_power_minute(&again).unwrap();
        let rows = db.evc_power_minutes_since(T).unwrap();
        assert_eq!(rows.len(), 1);
        assert!((rows[0].power_w - 4_500.0).abs() < 1e-9);
    }

    #[test]
    fn an_existing_database_gains_the_table() {
        let path = crate::test_util::make_unique_test_dir("evc-power-legacy").join("h.db");
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE readings (timestamp INTEGER PRIMARY KEY)")
            .unwrap();
        let db = HistoryDb::open(&path).unwrap();
        assert_eq!(db.evc_power_minutes_since(0).unwrap(), Vec::new());
    }
}
