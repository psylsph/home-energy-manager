//! Minute-bucketed history rows (issue #379).
//!
//! The poll loop can run faster than once a minute for a livelier display,
//! but nothing reads history at sub-minute resolution. [`MinuteAccumulator`]
//! folds every poll in a wall-clock minute into one `readings` row, so the
//! database grows with time rather than with the poll rate. Each minute is
//! written twice whatever the poll rate: at its first poll, so new data shows
//! at once, and when it ends (the first poll of a later minute arrives), with
//! the full aggregate in place of the first row. The minute still open is
//! written on shutdown; a hard crash or power cut loses at most that minute's
//! later polls. Polls a minute or more apart are written once each.
//!
//! Instantaneous readings (power, voltage, current, temperature, frequency,
//! panel output %) are averaged across the minute's polls, so a short peak is
//! neither lost nor over-represented. Everything else — SOC, rates, limits
//! and the cumulative `today_*` counters — is the latest poll's, and the row
//! carries the latest poll's own timestamp, so counters are stamped when they
//! were read. The writer replaces only rows it inserted itself for the same
//! minute, and never overwrites a row already there; existing history is
//! never touched.
//!
//! Known trade-off: when polling faster than once a minute and the inverter's
//! clock runs ahead of the host's, its midnight counter reset can land inside
//! the day's last minute, whose row then holds the reset value. Up to a
//! minute of that day's energy is lost; "Sync clock" avoids it.

use std::collections::VecDeque;

use crate::inverter::model::InverterSnapshot;

/// Seconds per history bucket.
pub const BUCKET_SECS: i64 = 60;

/// Recent minutes kept open, so a clock corrected back a few seconds into a
/// minute already written continues that minute's aggregate (and rewrites its
/// row) rather than starting it again from a single poll.
const OPEN_BUCKETS: usize = 3;

const AVERAGED_POWER: [fn(&mut InverterSnapshot) -> &mut i32; 6] = [
    |s| &mut s.solar_power,
    |s| &mut s.pv1_power,
    |s| &mut s.pv2_power,
    |s| &mut s.battery_power,
    |s| &mut s.grid_power,
    |s| &mut s.home_power,
];

const AVERAGED_READINGS: [fn(&mut InverterSnapshot) -> &mut f32; 10] = [
    |s| &mut s.pv1_voltage,
    |s| &mut s.pv2_voltage,
    |s| &mut s.pv1_current,
    |s| &mut s.pv2_current,
    |s| &mut s.battery_voltage,
    |s| &mut s.battery_current,
    |s| &mut s.battery_temperature,
    |s| &mut s.grid_voltage,
    |s| &mut s.grid_frequency,
    |s| &mut s.inverter_temperature,
];

const AVERAGED_PERCENT: [fn(&mut InverterSnapshot) -> &mut Option<f64>; 2] =
    [|s| &mut s.pv1_pct, |s| &mut s.pv2_pct];

#[derive(Debug)]
struct Bucket {
    start: i64,
    polls: u32,
    power_sums: [i64; AVERAGED_POWER.len()],
    reading_sums: [f64; AVERAGED_READINGS.len()],
    /// Sum and count per percentage; polls without a rated array are skipped.
    percent_sums: [(f64, u32); AVERAGED_PERCENT.len()],
    /// The minute's row as of its latest poll, until it is written.
    pending: Option<InverterSnapshot>,
}

impl Bucket {
    fn new(start: i64) -> Self {
        Self {
            start,
            polls: 0,
            power_sums: [0; AVERAGED_POWER.len()],
            reading_sums: [0.0; AVERAGED_READINGS.len()],
            percent_sums: [(0.0, 0); AVERAGED_PERCENT.len()],
            pending: None,
        }
    }

    /// Fold `snap` into the sums, then rewrite its averaged fields in place so
    /// it becomes the minute's row; every other field is left as this, the
    /// latest, poll's.
    fn fold_into_row(&mut self, snap: &mut InverterSnapshot) {
        self.polls += 1;
        let polls = f64::from(self.polls);
        for (sum, field) in self.power_sums.iter_mut().zip(AVERAGED_POWER) {
            let value = field(snap);
            *sum += i64::from(*value);
            *value = (*sum as f64 / polls).round() as i32;
        }
        for (sum, field) in self.reading_sums.iter_mut().zip(AVERAGED_READINGS) {
            let value = field(snap);
            *sum += f64::from(*value);
            *value = (*sum / polls) as f32;
        }
        for ((sum, count), field) in self.percent_sums.iter_mut().zip(AVERAGED_PERCENT) {
            let value = field(snap);
            if let Some(v) = *value {
                *sum += v;
                *count += 1;
            }
            *value = (*count > 0).then(|| *sum / f64::from(*count));
        }
    }

    /// Hand over the pending row, if any, to be written.
    fn emit(&mut self) -> Option<MinuteRow> {
        let row = self.pending.take()?;
        Some(MinuteRow {
            start: self.start,
            row,
        })
    }
}

/// A minute's row, ready to write.
#[derive(Debug, Clone)]
pub struct MinuteRow {
    /// Start of the wall-clock minute; the writer replaces the rows it
    /// inserted for this minute earlier, and nothing else.
    pub start: i64,
    /// The row, stamped with the latest poll's time.
    pub row: InverterSnapshot,
}

/// Folds polls into one row per wall-clock minute. Polls must be pushed in
/// the order they were taken.
#[derive(Debug, Default)]
pub struct MinuteAccumulator {
    /// The most recently used minutes, oldest first, at most [`OPEN_BUCKETS`].
    buckets: VecDeque<Bucket>,
}

impl MinuteAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold `snap` into its minute and return the rows that are now ready:
    /// every other open minute with unwritten polls (normally just the
    /// previous minute, once this poll shows it has ended), then this minute's
    /// row if `snap` is its first poll. Later polls of the minute wait for a
    /// later minute's poll, or [`Self::close_open`].
    ///
    /// A poll for a minute still open (reached again after a small backwards
    /// clock correction) continues it; any other minute starts afresh, so
    /// after a large backwards step the minute is rebuilt from the correctly
    /// stamped polls.
    pub fn push(&mut self, mut snap: InverterSnapshot) -> Vec<MinuteRow> {
        let mut ready = Vec::new();
        let start = snap.timestamp.div_euclid(BUCKET_SECS) * BUCKET_SECS;
        let mut first_poll = false;
        let index = match self.buckets.iter().position(|b| b.start == start) {
            Some(index) => index,
            None => {
                first_poll = true;
                if self.buckets.len() == OPEN_BUCKETS {
                    if let Some(row) = self.buckets.pop_front().and_then(|mut b| b.emit()) {
                        ready.push(row);
                    }
                }
                self.buckets.push_back(Bucket::new(start));
                self.buckets.len() - 1
            }
        };
        let bucket = &mut self.buckets[index];
        bucket.fold_into_row(&mut snap);
        bucket.pending = Some(snap);
        for (i, bucket) in self.buckets.iter_mut().enumerate() {
            if i != index {
                ready.extend(bucket.emit());
            }
        }
        if first_poll {
            ready.extend(self.buckets[index].emit());
        }
        ready
    }

    /// Rows for every minute still open, for shutdown.
    pub fn close_open(&mut self) -> Vec<MinuteRow> {
        self.buckets.iter_mut().filter_map(Bucket::emit).collect()
    }
}

/// Minute rows waiting to be written, in poll order.
#[derive(Debug, Default)]
pub struct PollRows {
    accumulator: MinuteAccumulator,
    queued: Vec<MinuteRow>,
}

impl PollRows {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold `snap` in; returns whether any rows became ready to write.
    pub fn push(&mut self, snap: InverterSnapshot) -> bool {
        let ready = self.accumulator.push(snap);
        let any = !ready.is_empty();
        self.queued.extend(ready);
        any
    }

    /// Queue the rows of every minute still open (shutdown).
    pub fn close_open(&mut self) {
        let rows = self.accumulator.close_open();
        self.queued.extend(rows);
    }

    /// Take every queued row, oldest first.
    pub fn take(&mut self) -> Vec<MinuteRow> {
        std::mem::take(&mut self.queued)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-21 12:00:00 UTC — minute aligned.
    const NOON: i64 = 1_792_584_000;
    /// 2026-10-22 00:00:00 UTC — a midnight boundary.
    const MIDNIGHT: i64 = 1_792_627_200;

    fn snap_at(timestamp: i64, home_power: i32, today_import_kwh: f32) -> InverterSnapshot {
        InverterSnapshot {
            timestamp,
            soc: 50,
            home_power,
            today_import_kwh,
            ..InverterSnapshot::default()
        }
    }

    /// Queue `polls`, close the open minute (as shutdown does) and return
    /// what the database ends up holding: each minute's last row (a later
    /// row replaces the earlier ones for its minute), in order.
    fn final_rows(polls: &[InverterSnapshot]) -> Vec<InverterSnapshot> {
        let mut rows = PollRows::new();
        for poll in polls {
            rows.push(poll.clone());
        }
        rows.close_open();
        let mut stored: Vec<MinuteRow> = Vec::new();
        for row in rows.take() {
            match stored.iter_mut().find(|r| r.start == row.start) {
                Some(existing) => *existing = row,
                None => stored.push(row),
            }
        }
        stored.into_iter().map(|r| r.row).collect()
    }

    #[test]
    fn constants_are_minute_aligned() {
        assert_eq!(NOON % BUCKET_SECS, 0);
        assert_eq!(MIDNIGHT % 86_400, 0);
    }

    #[test]
    fn ten_second_polls_write_one_row_per_minute_stamped_with_its_latest_poll() {
        let polls: Vec<_> = (0..18)
            .map(|i| snap_at(NOON + 3 + i * 10, 1000, i as f32 * 0.01))
            .collect();
        let rows = final_rows(&polls);
        let stamps: Vec<i64> = rows.iter().map(|r| r.timestamp).collect();
        assert_eq!(stamps, vec![NOON + 53, NOON + 113, NOON + 173]);
    }

    #[test]
    fn fifteen_second_polls_average_power_and_keep_the_latest_counter() {
        let mut polls = vec![
            snap_at(NOON + 1, 100, 1.00),
            snap_at(NOON + 16, 200, 1.01),
            snap_at(NOON + 31, 300, 1.02),
            snap_at(NOON + 46, 400, 1.03),
        ];
        polls[0].grid_voltage = 240.0;
        polls[1].grid_voltage = 244.0;
        polls[2].grid_voltage = 246.0;
        polls[3].grid_voltage = 250.0;
        polls[3].soc = 53;
        polls[3].target_soc = 90;
        let rows = final_rows(&polls);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.timestamp, NOON + 46);
        assert_eq!(row.home_power, 250);
        assert!((row.grid_voltage - 245.0).abs() < 1e-4);
        assert!((row.today_import_kwh - 1.03).abs() < 1e-6);
        assert_eq!(row.soc, 53);
        assert_eq!(row.target_soc, 90);
    }

    #[test]
    fn every_averaged_field_is_averaged() {
        let mut a = snap_at(NOON + 5, 0, 0.0);
        let mut b = snap_at(NOON + 35, 0, 0.0);
        for (s, v) in [(&mut a, 1.0_f32), (&mut b, 3.0_f32)] {
            let w = v as i32 * 100;
            s.solar_power = w;
            s.pv1_power = w;
            s.pv2_power = w;
            s.battery_power = -w;
            s.grid_power = w;
            s.home_power = w;
            s.pv1_voltage = v;
            s.pv2_voltage = v;
            s.pv1_current = v;
            s.pv2_current = v;
            s.battery_voltage = v;
            s.battery_current = v;
            s.battery_temperature = v;
            s.grid_voltage = v;
            s.grid_frequency = v;
            s.inverter_temperature = v;
            s.pv1_pct = Some(v as f64);
            s.pv2_pct = Some(v as f64);
        }
        let row = final_rows(&[a, b]).remove(0);
        for w in [
            row.solar_power,
            row.pv1_power,
            row.pv2_power,
            -row.battery_power,
            row.grid_power,
            row.home_power,
        ] {
            assert_eq!(w, 200);
        }
        for v in [
            row.pv1_voltage,
            row.pv2_voltage,
            row.pv1_current,
            row.pv2_current,
            row.battery_voltage,
            row.battery_current,
            row.battery_temperature,
            row.grid_voltage,
            row.grid_frequency,
            row.inverter_temperature,
        ] {
            assert!((v - 2.0).abs() < 1e-6, "got {v}");
        }
        assert_eq!(row.pv1_pct, Some(2.0));
        assert_eq!(row.pv2_pct, Some(2.0));
    }

    #[test]
    fn negative_power_averages_round_half_away_from_zero() {
        let rows = final_rows(&[snap_at(NOON + 1, -1, 0.0), snap_at(NOON + 2, -2, 0.0)]);
        assert_eq!(rows[0].home_power, -2);
    }

    #[test]
    fn panel_percentage_averages_only_polls_that_had_one() {
        let mut a = snap_at(NOON + 1, 0, 0.0);
        let b = snap_at(NOON + 20, 0, 0.0);
        let mut c = snap_at(NOON + 40, 0, 0.0);
        a.pv1_pct = Some(10.0);
        c.pv1_pct = Some(30.0);
        let row = final_rows(&[a, b, c]).remove(0);
        assert_eq!(row.pv1_pct, Some(20.0));
        assert_eq!(row.pv2_pct, None);
    }

    #[test]
    fn midnight_does_not_carry_a_pre_midnight_counter_or_power_into_the_new_day() {
        let rows = final_rows(&[
            snap_at(MIDNIGHT - 20, 4000, 12.40),
            snap_at(MIDNIGHT - 5, 4000, 12.45),
            snap_at(MIDNIGHT + 5, 200, 0.01),
            snap_at(MIDNIGHT + 20, 400, 0.02),
        ]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].timestamp, MIDNIGHT - 5);
        assert!((rows[0].today_import_kwh - 12.45).abs() < 1e-6);
        assert_eq!(rows[1].timestamp, MIDNIGHT + 20);
        assert!((rows[1].today_import_kwh - 0.02).abs() < 1e-6);
        assert_eq!(rows[1].home_power, 300);
    }

    #[test]
    fn a_counter_reset_inside_the_last_minute_takes_the_latest_value() {
        // The documented trade-off: an inverter clock running ahead resets at
        // 23:59:40 host time, so the 23:59 row holds the reset value and the
        // energy since the 23:58 row is not in that day's total.
        let rows = final_rows(&[
            snap_at(MIDNIGHT - 50, 0, 12.40),
            snap_at(MIDNIGHT - 20, 0, 0.00),
            snap_at(MIDNIGHT + 10, 0, 0.01),
        ]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].timestamp, MIDNIGHT - 20);
        assert_eq!(rows[0].today_import_kwh, 0.0);
        assert!((rows[1].today_import_kwh - 0.01).abs() < 1e-6);
    }

    #[test]
    fn every_counter_takes_the_latest_value() {
        let mut a = snap_at(NOON + 5, 0, 0.0);
        let mut b = snap_at(NOON + 35, 0, 0.0);
        for (s, v) in [(&mut a, 5.0_f32), (&mut b, 4.5_f32)] {
            s.today_solar_kwh = v;
            s.today_pv1_kwh = v;
            s.today_pv2_kwh = v;
            s.today_import_kwh = v;
            s.today_export_kwh = v;
            s.today_charge_kwh = v;
            s.today_discharge_kwh = v;
            s.today_consumption_kwh = v;
            s.today_ac_charge_kwh = v;
            s.home_energy_today_kwh = v;
        }
        let row = final_rows(&[a, b]).remove(0);
        for v in [
            row.today_solar_kwh,
            row.today_pv1_kwh,
            row.today_pv2_kwh,
            row.today_import_kwh,
            row.today_export_kwh,
            row.today_charge_kwh,
            row.today_discharge_kwh,
            row.today_consumption_kwh,
            row.today_ac_charge_kwh,
            row.home_energy_today_kwh,
        ] {
            assert_eq!(v, 4.5);
        }
    }

    #[test]
    fn a_counter_reset_inside_the_midnight_minute_takes_the_new_days_value() {
        // Inverter clock running behind: it resets at 00:00:30 system time.
        // The latest value is the new day's, so yesterday's total stays out.
        let rows = final_rows(&[
            snap_at(MIDNIGHT + 10, 0, 12.45),
            snap_at(MIDNIGHT + 40, 0, 0.01),
        ]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].timestamp, MIDNIGHT + 40);
        assert!((rows[0].today_import_kwh - 0.01).abs() < 1e-6);
    }

    #[test]
    fn a_reset_more_than_a_minute_late_still_takes_the_new_days_value() {
        // Inverter clock 90 s behind: the reset lands at 00:01:30.
        let rows = final_rows(&[
            snap_at(MIDNIGHT + 70, 0, 12.45),
            snap_at(MIDNIGHT + 95, 0, 0.01),
        ]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].timestamp, MIDNIGHT + 95);
        assert!((rows[0].today_import_kwh - 0.01).abs() < 1e-6);
    }

    #[test]
    fn a_brief_upward_glitch_does_not_become_the_minutes_value() {
        // One corrupt read during the sanitizer's grace period, then sane
        // reads: the minute keeps the latest, not the glitch.
        let rows = final_rows(&[
            snap_at(NOON + 5, 0, 8.1),
            snap_at(NOON + 15, 0, 150.0),
            snap_at(NOON + 25, 0, 8.2),
        ]);
        assert!((rows[0].today_import_kwh - 8.2).abs() < 1e-6);
    }

    #[test]
    fn sixty_second_polls_write_each_poll_unaveraged() {
        let polls: Vec<_> = (0..4)
            .map(|i| snap_at(NOON + 7 + i * 60, 100 * (i as i32 + 1), i as f32))
            .collect();
        let rows = final_rows(&polls);
        assert_eq!(rows.len(), 4);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(row.timestamp, NOON + 7 + i as i64 * 60);
            assert_eq!(row.home_power, 100 * (i as i32 + 1));
            assert_eq!(row.today_import_kwh, i as f32);
        }
    }

    #[test]
    fn hundred_and_twenty_second_polls_write_each_poll_unaveraged() {
        let polls: Vec<_> = (0..3)
            .map(|i| snap_at(NOON + 30 + i * 120, 500 + i as i32, 0.0))
            .collect();
        let rows = final_rows(&polls);
        let got: Vec<(i64, i32)> = rows.iter().map(|r| (r.timestamp, r.home_power)).collect();
        assert_eq!(
            got,
            vec![(NOON + 30, 500), (NOON + 150, 501), (NOON + 270, 502)]
        );
    }

    #[test]
    fn a_clock_step_back_starts_a_new_minute_instead_of_dropping_polls() {
        // A host clock corrected backwards by NTP must not stop history. The
        // earlier minute's row is rebuilt from the correctly-stamped polls and
        // replaces the one written under the wrong time.
        let rows = final_rows(&[
            snap_at(NOON + 7_200 + 10, 900, 2.0),
            snap_at(NOON + 10, 100, 1.0),
            snap_at(NOON + 20, 300, 1.1),
        ]);
        let got: Vec<(i64, i32)> = rows.iter().map(|r| (r.timestamp, r.home_power)).collect();
        assert_eq!(got, vec![(NOON + 7_210, 900), (NOON + 20, 200)]);
    }

    #[test]
    fn a_small_clock_correction_continues_the_minute_already_written() {
        // Six polls fill 12:00, one lands at 12:01:02, then NTP steps the
        // clock back to 12:00:58 and polling carries on.
        let mut polls: Vec<_> = (0..6)
            .map(|i| snap_at(NOON + 5 + i * 10, 100, 1.0))
            .collect();
        polls.push(snap_at(NOON + 62, 700, 1.1));
        polls.push(snap_at(NOON + 58, 800, 1.2));
        polls.push(snap_at(NOON + 68, 900, 1.3));
        let mut rows = PollRows::new();
        for poll in polls {
            rows.push(poll);
        }
        rows.close_open();
        // Last queued row per minute, as the writer leaves the database.
        let mut stored = std::collections::BTreeMap::new();
        for row in rows.take() {
            stored.insert(row.start, row.row);
        }
        // 12:00 averages all seven of its polls, not just the late one.
        assert_eq!(stored[&NOON].home_power, (6 * 100 + 800) / 7);
        assert_eq!(stored[&NOON].timestamp, NOON + 58);
        // 12:01 averages both of its polls.
        assert_eq!(stored[&(NOON + 60)].home_power, 800);
        assert!((stored[&(NOON + 60)].today_import_kwh - 1.3).abs() < 1e-6);
    }

    #[test]
    fn a_minute_is_written_at_its_first_poll_and_when_it_ends() {
        let mut rows = PollRows::new();
        for i in 0..15 {
            rows.push(snap_at(NOON + i * 10, 100, i as f32));
        }
        // (minute, row timestamp) in write order: each minute's first poll
        // at once, its aggregate when it ends; 12:02 still open.
        let written: Vec<(i64, i64)> = rows
            .take()
            .into_iter()
            .map(|r| (r.start, r.row.timestamp))
            .collect();
        assert_eq!(
            written,
            vec![
                (NOON, NOON),
                (NOON, NOON + 50),
                (NOON + 60, NOON + 60),
                (NOON + 60, NOON + 110),
                (NOON + 120, NOON + 120),
            ]
        );
        assert!(
            rows.take().is_empty(),
            "nothing new until another minute ends"
        );
        rows.close_open();
        let open: Vec<(i64, i64)> = rows
            .take()
            .into_iter()
            .map(|r| (r.start, r.row.timestamp))
            .collect();
        assert_eq!(open, vec![(NOON + 120, NOON + 140)]);
        rows.close_open();
        assert!(rows.take().is_empty(), "closing twice writes nothing more");
    }

    #[test]
    fn push_reports_whether_rows_became_ready() {
        let mut rows = PollRows::new();
        assert!(
            rows.push(snap_at(NOON + 5, 0, 0.0)),
            "a minute's first poll"
        );
        assert!(!rows.push(snap_at(NOON + 15, 0, 0.0)), "mid-minute");
        assert!(rows.push(snap_at(NOON + 65, 0, 0.0)), "the minute ended");
    }

    #[test]
    fn polls_a_minute_apart_are_written_once_each() {
        let mut rows = PollRows::new();
        for i in 0..3 {
            rows.push(snap_at(NOON + 7 + i * 60, 0, 0.0));
        }
        rows.close_open();
        let written: Vec<i64> = rows.take().iter().map(|r| r.row.timestamp).collect();
        assert_eq!(written, vec![NOON + 7, NOON + 67, NOON + 127]);
    }

    #[test]
    fn a_minute_reopened_by_a_clock_correction_is_written_again() {
        let mut acc = MinuteAccumulator::new();
        assert_eq!(acc.push(snap_at(NOON + 5, 0, 0.0)).len(), 1);
        let next_first = acc.push(snap_at(NOON + 65, 0, 0.0));
        assert_eq!(next_first.len(), 1);
        assert_eq!(next_first[0].start, NOON + 60);
        // Clock steps back into 12:00: nothing pending elsewhere to write.
        assert!(acc.push(snap_at(NOON + 58, 0, 0.0)).is_empty());
        // Back to 12:01: 12:00 is written again, with both its polls.
        let rewritten = acc.push(snap_at(NOON + 70, 0, 0.0));
        assert_eq!(rewritten.len(), 1);
        assert_eq!(rewritten[0].start, NOON);
        assert_eq!(rewritten[0].row.timestamp, NOON + 58);
    }

    #[test]
    fn an_open_minute_pushed_out_by_newer_ones_is_still_written() {
        let mut acc = MinuteAccumulator::new();
        for minute in 0..3 {
            acc.push(snap_at(NOON + minute * 60 + 5, 100, 0.0));
        }
        // Clock back into 12:00 (still open, now pending again)...
        acc.push(snap_at(NOON + 30, 300, 0.0));
        // ...then a jump far ahead pushes 12:00 out of the open minutes.
        let ready = acc.push(snap_at(NOON + 600, 0, 0.0));
        let noon = ready
            .iter()
            .find(|r| r.start == NOON)
            .expect("12:00 written");
        assert_eq!(noon.row.home_power, 200, "both 12:00 polls averaged");
        assert_eq!(acc.buckets.len(), OPEN_BUCKETS);
    }

    #[test]
    fn open_minutes_are_bounded() {
        let mut acc = MinuteAccumulator::new();
        for i in 0..100 {
            acc.push(snap_at(NOON + i * 60, 0, 0.0));
        }
        assert_eq!(acc.buckets.len(), OPEN_BUCKETS);
    }

    #[test]
    fn row_keeps_non_averaged_fields_from_the_latest_poll() {
        let mut a = snap_at(NOON + 1, 0, 0.0);
        let mut b = snap_at(NOON + 30, 0, 0.0);
        a.battery_capacity_kwh = 9.5;
        a.charge_rate = 10;
        a.battery_reserve = 4;
        b.battery_capacity_kwh = 9.6;
        b.charge_rate = 50;
        b.battery_reserve = 20;
        b.discharge_rate = 25;
        let row = final_rows(&[a, b]).remove(0);
        assert_eq!(row.battery_capacity_kwh, 9.6);
        assert_eq!(row.charge_rate, 50);
        assert_eq!(row.battery_reserve, 20);
        assert_eq!(row.discharge_rate, 25);
    }
}
