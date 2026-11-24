//! What a planned hold charge actually reached (issue #507).
//!
//! The Planner writes a target SOC to charge slot 1 and the inverter stops
//! there; nothing used to look at the result. [`PlanOutcomeWatch`] follows the
//! battery through the slot's window and reports the level it reached when
//! the window ends. It is pure: the poll loop feeds it one reading a poll and
//! logs, and optionally notifies, what it returns. It never writes anything.
//!
//! Invariants:
//! - A window is only watched while charge slot 1 reads back exactly as the
//!   Planner last wrote it with a hold target below 100 ([`PlannedSlot`], in
//!   memory), enabled and running. A user's own schedule, saved over the
//!   plan, is never reported on.
//! - One report per window occurrence, however many polls it took.
//! - The reported end level is the last reading taken *inside* the window, so
//!   a battery that drains once the window is over never counts against it.
//! - A report needs readings that cover the end: a gap longer than
//!   [`MAX_GAP_SECS`] inside the window, a clock stepped back, or a first
//!   poll after the end that comes more than [`MAX_GAP_SECS`] late means no
//!   report for that occurrence rather than a guess. (A gap inside the window
//!   only costs the peak and the reached time: the end level is the last
//!   reading, so the watch carries on as if it had joined late.)
//! - A window HEM only joined part-way through (a restart) still reports the
//!   level at its end, but not when the target was reached, which it can't
//!   know; nor is a battery that began at or above the target said to have
//!   reached it.
//! - A slot or target that changes mid-window ends the watch on the old plan;
//!   a replan to a new target is watched afresh.
//! - In memory only: a restart forgets a run in progress.

use std::sync::Arc;

use chrono::{DateTime, Local};

use crate::inverter::model::{InverterSnapshot, ScheduleSlot};
use crate::inverter::plan_replan::local_ts;
use crate::inverter::poll::AppState;
use crate::settings::Settings;

/// A gap between readings longer than this, inside the window or around its
/// end, leaves too little to report.
pub const MAX_GAP_SECS: i64 = 600;

/// The battery counts as having reached its target when it ends the window
/// within this many points of it: the BMS rounds, and the inverter stops
/// within a point or so of the register.
pub const SHORT_POINTS: u8 = 2;

/// The slot the Planner last wrote with a hold target (HHMM, as the inverter
/// reads it back).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedSlot {
    pub start_hhmm: u16,
    pub end_hhmm: u16,
    pub target: u8,
}

/// The Planner's running hold window, as the inverter reads back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveWindow {
    pub start_ts: i64,
    pub end_ts: i64,
    /// The target the Planner armed.
    pub target: u8,
}

/// One poll's readings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub ts: i64,
    pub soc: u8,
    /// The target SOC the inverter reported at this poll.
    pub reported_target: u8,
}

/// How a watched window ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub target: u8,
    /// The last reading inside the window.
    pub end_soc: u8,
    /// The highest reading inside the window.
    pub peak_soc: u8,
    /// When the battery first read the target, if HEM saw it from the start.
    pub reached_ts: Option<i64>,
    /// The target the inverter reported on the last reading in the window.
    pub reported_target: u8,
    pub window_end_ts: i64,
    /// HEM joined the window part-way through.
    pub joined_late: bool,
}

impl Report {
    /// Whether the battery ended the window clearly below its target.
    pub fn fell_short(&self) -> bool {
        u16::from(self.end_soc) + u16::from(SHORT_POINTS) < u16::from(self.target)
    }
}

impl Report {
    /// The log and notification text.
    pub fn message(&self) -> String {
        let end = hhmm(self.window_end_ts);
        if !self.fell_short() {
            return match self.reached_ts {
                Some(ts) => format!(
                    "🔋 Planned charge finished — reached the {}% target at {} and was at {}% \
                     when the window ended at {end}.",
                    self.target,
                    hhmm(ts),
                    self.end_soc
                ),
                None => format!(
                    "🔋 Planned charge finished — the battery was at {}% when the window ended \
                     at {end} (target {}%).",
                    self.end_soc, self.target
                ),
            };
        }
        let mut text = format!(
            "⚠️ Planned charge fell short — the battery was at {}% when the window ended at \
             {end}, below the {}% target.",
            self.end_soc, self.target
        );
        if self.peak_soc >= self.target {
            text.push_str(&format!(
                " It reached {}% and then fell back to {}% before the window ended.",
                self.peak_soc, self.end_soc
            ));
        }
        if self.reported_target == self.target {
            text.push_str(&format!(
                " The inverter was still reporting the {}% target.",
                self.target
            ));
        } else {
            text.push_str(&format!(
                " The inverter was reporting a target of {}%.",
                self.reported_target
            ));
        }
        text
    }
}

/// `HH:MM` local time of a Unix timestamp.
fn hhmm(ts: i64) -> String {
    DateTime::from_timestamp(ts, 0)
        .map(|utc| utc.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

/// The Planner's window running at `now`, if any: its planned target is below
/// 100 and `slot` (charge slot 1 as read back) is enabled, has the times the
/// Planner wrote and covers `now`.
pub fn active_window(
    planned: Option<PlannedSlot>,
    slot: &ScheduleSlot,
    now: DateTime<Local>,
) -> Option<ActiveWindow> {
    let planned = planned.filter(|planned| planned.target < 100)?;
    let target = planned.target;
    let slot_hhmm = |hour: u8, minute: u8| u16::from(hour) * 100 + u16::from(minute);
    if !slot.enabled
        || slot_hhmm(slot.start_hour, slot.start_minute) != planned.start_hhmm
        || slot_hhmm(slot.end_hour, slot.end_minute) != planned.end_hhmm
    {
        return None;
    }
    let start = u32::from(slot.start_hour) * 60 + u32::from(slot.start_minute);
    let mut end = u32::from(slot.end_hour) * 60 + u32::from(slot.end_minute);
    if start == end {
        return None;
    }
    // The planner clamps a window ending at midnight to 23:59.
    if end == 23 * 60 + 59 {
        end = 24 * 60;
    }
    if end < start {
        end += 1440;
    }
    let now_ts = now.timestamp();
    let today = now.date_naive();
    // A window that crossed midnight opened yesterday.
    [today.pred_opt()?, today].into_iter().find_map(|date| {
        let start_ts = local_ts(date, start)?;
        let end_ts = local_ts(date, end)?;
        (start_ts <= now_ts && now_ts < end_ts).then_some(ActiveWindow {
            start_ts,
            end_ts,
            target,
        })
    })
}

/// Follows one window at a time from the first reading inside it.
#[derive(Debug, Default)]
pub struct PlanOutcomeWatch {
    /// The slot the Planner last wrote with a hold target. In memory only,
    /// so the watch never trusts a persisted target that something else may
    /// have outlived.
    planned: Option<PlannedSlot>,
    run: Option<Run>,
    /// The end of the last occurrence reported or given up on, so it is
    /// neither reported twice nor restarted from the middle.
    done_end: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
struct Run {
    window: ActiveWindow,
    peak_soc: u8,
    reached_ts: Option<i64>,
    last_ts: i64,
    last_soc: u8,
    reported_target: u8,
    joined_late: bool,
    /// The first reading was already at or above the target, so nothing was
    /// charged to reach it.
    started_at_target: bool,
}

impl PlanOutcomeWatch {
    /// The slot the Planner last wrote with a hold target.
    pub fn planned(&self) -> Option<PlannedSlot> {
        self.planned
    }

    /// Record the slot the Planner just wrote, or `None` when it wrote
    /// anything else (a full charge, a cleared slot).
    pub fn set_planned(&mut self, planned: Option<PlannedSlot>) {
        self.planned = planned;
    }

    /// Take one poll: `window` is the Planner's window running at the
    /// reading, none outside it. Returns the report when this reading is the
    /// first after a watched window's end.
    pub fn observe(&mut self, window: Option<ActiveWindow>, reading: Reading) -> Option<Report> {
        let mut report = None;
        if let Some(run) = self.run.take() {
            if reading.ts < run.last_ts {
                // The clock stepped back: nothing about this window can be
                // trusted any more.
                self.done_end = Some(run.window.end_ts);
                return None;
            }
            let running = reading.ts < run.window.end_ts;
            match window {
                Some(current) if running && current == run.window => {
                    // A gap loses the peak and when the target was reached,
                    // but the end level only needs the last reading.
                    self.run = Some(if reading.ts - run.last_ts > MAX_GAP_SECS {
                        run.restarted(reading)
                    } else {
                        run.with(reading)
                    });
                    return None;
                }
                // Replaced or cleared before the end: nothing to report on.
                _ if running => {}
                // The window has ended, whatever the slot reads now (it may
                // already be tomorrow's, or the next window's).
                _ => {
                    self.done_end = Some(run.window.end_ts);
                    let covered = run.window.end_ts - run.last_ts <= MAX_GAP_SECS
                        && reading.ts - run.window.end_ts <= MAX_GAP_SECS;
                    report = covered.then(|| run.report());
                }
            }
        }
        if let Some(window) = window.filter(|w| self.done_end != Some(w.end_ts)) {
            self.run = Some(Run::start(window, reading));
        }
        report
    }
}

impl Run {
    /// A run from the first reading inside `window`.
    fn start(window: ActiveWindow, reading: Reading) -> Self {
        Self {
            window,
            peak_soc: reading.soc,
            reached_ts: None,
            last_ts: reading.ts,
            last_soc: reading.soc,
            reported_target: reading.reported_target,
            joined_late: reading.ts - window.start_ts > MAX_GAP_SECS,
            started_at_target: reading.soc >= window.target,
        }
        .with_reached(reading)
    }

    /// The run after a gap in the readings: the same window, seen afresh.
    fn restarted(self, reading: Reading) -> Self {
        Self {
            joined_late: true,
            ..Self::start(self.window, reading)
        }
    }

    /// The run after one more reading inside its window.
    fn with(mut self, reading: Reading) -> Self {
        self.peak_soc = self.peak_soc.max(reading.soc);
        self.last_ts = reading.ts;
        self.last_soc = reading.soc;
        self.reported_target = reading.reported_target;
        self.with_reached(reading)
    }

    /// Note the first reading at the target. HEM can only say when from a
    /// window it watched from its start, and only when the battery had to
    /// charge to get there.
    fn with_reached(mut self, reading: Reading) -> Self {
        if self.reached_ts.is_none()
            && !self.joined_late
            && !self.started_at_target
            && reading.soc >= self.window.target
        {
            self.reached_ts = Some(reading.ts);
        }
        self
    }

    fn report(self) -> Report {
        Report {
            target: self.window.target,
            end_soc: self.last_soc,
            peak_soc: self.peak_soc,
            reached_ts: self.reached_ts,
            reported_target: self.reported_target,
            window_end_ts: self.window.end_ts,
            joined_late: self.joined_late,
        }
    }
}

/// Record the slot the Planner just wrote (`None` when it wrote anything
/// else, a full charge or a cleared slot).
pub(crate) async fn note_planned_slot(state: &Arc<AppState>, planned: Option<PlannedSlot>) {
    state.plan_outcome.lock().await.set_planned(planned);
}

/// Whether a report is also sent as a notification: the plan notifications'
/// own rule, only the auto-apply notifies (through the master alerts toggle).
fn notifies(settings: &Settings) -> bool {
    settings.forecast_plan_auto_apply_enabled
}

/// The target SOC the inverter reports for charge slot 1: the slot's own on
/// models with the extended schedule block, the global one elsewhere.
fn reported_target(snapshot: &InverterSnapshot) -> u8 {
    if snapshot.device_type.uses_extended_schedule_slots() {
        snapshot.charge_slots[0].target_soc
    } else {
        snapshot.target_soc
    }
}

/// Feed this poll to the watch, and log (and, with auto-apply on, notify)
/// how a planned window ended. A Gateway keeps its own copy of the schedule,
/// so what it reads back says nothing about the battery's charge.
pub(crate) async fn run_plan_outcome(
    state: &Arc<AppState>,
    settings: &Settings,
    snapshot: &InverterSnapshot,
    now: DateTime<Local>,
) -> Option<Report> {
    if !snapshot.device_type.battery_controls_reach_battery() {
        return None;
    }
    let reading = Reading {
        ts: now.timestamp(),
        soc: snapshot.soc,
        reported_target: reported_target(snapshot),
    };
    let report = {
        let mut watch = state.plan_outcome.lock().await;
        let window = active_window(watch.planned(), &snapshot.charge_slots[0], now);
        watch.observe(window, reading)?
    };
    if report.fell_short() {
        tracing::warn!(
            target_soc = report.target,
            end_soc = report.end_soc,
            peak_soc = report.peak_soc,
            reported_target = report.reported_target,
            "{}",
            report.message()
        );
    } else {
        tracing::info!(
            target_soc = report.target,
            end_soc = report.end_soc,
            peak_soc = report.peak_soc,
            "{}",
            report.message()
        );
    }
    if notifies(settings) {
        crate::alerts::send_plan_notification(state, &report.message()).await;
    }
    Some(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 24).unwrap()
    }

    /// Local time `h:m` on the test day (hours past 24 run into the next).
    fn at(h: u32, m: u32) -> i64 {
        local_ts(day(), h * 60 + m).unwrap()
    }

    fn local(h: u32, m: u32) -> DateTime<Local> {
        Local
            .timestamp_opt(at(h, m), 0)
            .earliest()
            .expect("a valid local time")
    }

    fn window(target: u8) -> ActiveWindow {
        ActiveWindow {
            start_ts: at(2, 0),
            end_ts: at(5, 0),
            target,
        }
    }

    fn read(ts: i64, soc: u8, reported_target: u8) -> Reading {
        Reading {
            ts,
            soc,
            reported_target,
        }
    }

    /// Feed readings every five minutes from `from` to the window's end,
    /// with the SOC given by `soc(minutes since the window opened)`.
    fn run_window(
        watch: &mut PlanOutcomeWatch,
        w: ActiveWindow,
        from: i64,
        soc: impl Fn(i64) -> u8,
        reported_target: u8,
    ) {
        let mut ts = from;
        while ts < w.end_ts {
            let report = watch.observe(
                Some(w),
                read(ts, soc((ts - w.start_ts) / 60), reported_target),
            );
            assert_eq!(report, None, "no report inside the window");
            ts += 300;
        }
    }

    fn planned_at(start_hhmm: u16, end_hhmm: u16, target: u8) -> PlannedSlot {
        PlannedSlot {
            start_hhmm,
            end_hhmm,
            target,
        }
    }

    /// The 02:00-05:00 slot the tests' `window` runs in.
    fn planned(target: u8) -> PlannedSlot {
        planned_at(200, 500, target)
    }

    fn slot(enabled: bool, start: (u8, u8), end: (u8, u8)) -> ScheduleSlot {
        ScheduleSlot {
            enabled,
            start_hour: start.0,
            start_minute: start.1,
            end_hour: end.0,
            end_minute: end.1,
            target_soc: 65,
        }
    }

    // ---- active_window ----------------------------------------------------

    #[test]
    fn a_running_hold_slot_is_the_active_window() {
        let w =
            active_window(Some(planned(65)), &slot(true, (2, 0), (5, 0)), local(3, 10)).unwrap();
        assert_eq!(w, window(65));
    }

    #[test]
    fn the_window_is_half_open() {
        let s = slot(true, (2, 0), (5, 0));
        assert!(active_window(Some(planned(65)), &s, local(2, 0)).is_some());
        assert!(active_window(Some(planned(65)), &s, local(4, 59)).is_some());
        assert_eq!(active_window(Some(planned(65)), &s, local(5, 0)), None);
        assert_eq!(active_window(Some(planned(65)), &s, local(1, 59)), None);
    }

    #[test]
    fn no_planned_slot_or_a_full_target_is_not_the_planners_hold() {
        let s = slot(true, (2, 0), (5, 0));
        assert_eq!(active_window(None, &s, local(3, 0)), None);
        assert_eq!(active_window(Some(planned(100)), &s, local(3, 0)), None);
    }

    #[test]
    fn a_slot_with_other_times_than_the_planners_is_not_its_window() {
        // The user saved their own slot 1 after the Planner's plan: the
        // Planner's target is still armed, but the slot isn't its own.
        let own = slot(true, (0, 30), (4, 30));
        assert_eq!(active_window(Some(planned(65)), &own, local(3, 0)), None);
        let later_start = slot(true, (2, 30), (5, 0));
        assert_eq!(
            active_window(Some(planned(65)), &later_start, local(3, 0)),
            None
        );
    }

    #[test]
    fn a_disabled_or_empty_slot_is_not_a_window() {
        assert_eq!(
            active_window(Some(planned(65)), &slot(false, (2, 0), (5, 0)), local(3, 0)),
            None
        );
        assert_eq!(
            active_window(
                Some(planned_at(0, 0, 65)),
                &slot(true, (0, 0), (0, 0)),
                local(3, 0)
            ),
            None
        );
    }

    #[test]
    fn a_window_across_midnight_is_found_on_both_sides() {
        let s = slot(true, (23, 30), (5, 30));
        let before = active_window(Some(planned_at(2330, 530, 70)), &s, local(23, 45)).unwrap();
        let after = active_window(Some(planned_at(2330, 530, 70)), &s, local(1, 0)).unwrap();
        assert_eq!(before.end_ts, at(5, 30) + 24 * 3600);
        assert_eq!(after.start_ts, at(23, 30) - 24 * 3600);
        assert_eq!(after.end_ts, at(5, 30));
        assert_eq!(
            active_window(Some(planned_at(2330, 530, 70)), &s, local(12, 0)),
            None
        );
    }

    #[test]
    fn a_window_ending_at_2359_ends_at_midnight() {
        let w = active_window(
            Some(planned_at(2300, 2359, 70)),
            &slot(true, (23, 0), (23, 59)),
            local(23, 30),
        )
        .unwrap();
        assert_eq!(w.end_ts, at(24, 0));
    }

    // ---- reporting ----------------------------------------------------------

    #[test]
    fn a_charge_that_reaches_its_target_reports_when() {
        let mut watch = PlanOutcomeWatch::default();
        let w = window(65);
        // 40% rising a point a minute from 02:00: 65% at 02:25.
        run_window(&mut watch, w, w.start_ts, |m| (40 + m).min(65) as u8, 65);
        let report = watch
            .observe(None, read(at(5, 0), 64, 65))
            .expect("the first poll after the end reports");
        assert_eq!(report.target, 65);
        assert_eq!(report.end_soc, 65);
        assert_eq!(report.peak_soc, 65);
        assert_eq!(report.reached_ts, Some(at(2, 25)));
        assert_eq!(report.window_end_ts, at(5, 0));
        assert!(!report.joined_late);
        assert!(!report.fell_short());
    }

    #[test]
    fn a_charge_that_stops_short_reports_the_target_the_inverter_held() {
        let mut watch = PlanOutcomeWatch::default();
        let w = window(65);
        run_window(&mut watch, w, w.start_ts, |m| (30 + m).min(41) as u8, 100);
        let report = watch.observe(None, read(at(5, 1), 41, 100)).unwrap();
        assert_eq!(report.end_soc, 41);
        assert_eq!(report.peak_soc, 41);
        assert_eq!(report.reached_ts, None);
        assert_eq!(report.reported_target, 100);
        assert!(report.fell_short());
    }

    #[test]
    fn the_end_level_is_the_last_reading_inside_the_window() {
        let mut watch = PlanOutcomeWatch::default();
        let w = window(65);
        run_window(&mut watch, w, w.start_ts, |_| 65, 65);
        // The house drains the battery once the window is over.
        let report = watch.observe(None, read(at(5, 2), 40, 65)).unwrap();
        assert_eq!(report.end_soc, 65);
        assert!(!report.fell_short());
    }

    #[test]
    fn a_battery_that_drains_while_held_is_short_at_the_end() {
        let mut watch = PlanOutcomeWatch::default();
        let w = window(65);
        run_window(
            &mut watch,
            w,
            w.start_ts,
            |m| if m < 60 { 65 } else { 50 },
            65,
        );
        let report = watch.observe(None, read(at(5, 0), 50, 65)).unwrap();
        assert_eq!(report.peak_soc, 65);
        assert_eq!(report.end_soc, 50);
        assert!(report.fell_short());
    }

    #[test]
    fn two_points_under_is_still_reached_three_is_short() {
        let report = |end_soc| Report {
            target: 65,
            end_soc,
            peak_soc: end_soc,
            reached_ts: None,
            reported_target: 65,
            window_end_ts: 0,
            joined_late: false,
        };
        assert!(!report(63).fell_short());
        assert!(report(62).fell_short());
        assert!(!report(70).fell_short());
    }

    #[test]
    fn a_battery_already_above_target_is_not_said_to_have_reached_it() {
        let mut watch = PlanOutcomeWatch::default();
        let w = window(65);
        run_window(&mut watch, w, w.start_ts, |_| 80, 65);
        let report = watch.observe(None, read(at(5, 0), 79, 65)).unwrap();
        assert_eq!(report.reached_ts, None);
        assert!(!report.joined_late);
        assert!(report
            .message()
            .contains("was at 80% when the window ended"));
        assert!(!report.message().contains("reached the"));
        assert_eq!(report.peak_soc, 80);
        assert!(!report.fell_short());
    }

    #[test]
    fn each_window_reports_once() {
        let mut watch = PlanOutcomeWatch::default();
        let w = window(65);
        run_window(&mut watch, w, w.start_ts, |_| 65, 65);
        assert!(watch.observe(None, read(at(5, 0), 65, 65)).is_some());
        assert_eq!(watch.observe(None, read(at(5, 5), 64, 65)), None);
        assert_eq!(watch.observe(None, read(at(5, 10), 63, 65)), None);
    }

    #[test]
    fn the_next_night_reports_again() {
        let mut watch = PlanOutcomeWatch::default();
        let w = window(65);
        run_window(&mut watch, w, w.start_ts, |_| 65, 65);
        assert!(watch.observe(None, read(at(5, 0), 65, 65)).is_some());
        let next = ActiveWindow {
            start_ts: w.start_ts + 86_400,
            end_ts: w.end_ts + 86_400,
            target: 70,
        };
        run_window(&mut watch, next, next.start_ts, |_| 70, 70);
        let report = watch.observe(None, read(next.end_ts, 70, 70)).unwrap();
        assert_eq!(report.target, 70);
    }

    // ---- readings that can't be trusted to cover the window -----------------

    #[test]
    fn a_gap_inside_the_window_still_reports_the_end_level_without_a_time() {
        let mut watch = PlanOutcomeWatch::default();
        let w = window(65);
        assert_eq!(watch.observe(Some(w), read(at(2, 0), 60, 65)), None);
        assert_eq!(watch.observe(Some(w), read(at(2, 5), 62, 65)), None);
        // Readings stop for twenty-five minutes; the target is reached in it.
        assert_eq!(watch.observe(Some(w), read(at(2, 30), 65, 65)), None);
        let mut ts = at(2, 35);
        while ts < w.end_ts {
            assert_eq!(watch.observe(Some(w), read(ts, 65, 65)), None);
            ts += 300;
        }
        let report = watch.observe(None, read(at(5, 0), 65, 65)).unwrap();
        assert!(report.joined_late);
        assert_eq!(report.reached_ts, None);
        assert_eq!(report.end_soc, 65);
    }

    #[test]
    fn a_gap_that_runs_to_the_end_still_gives_no_report() {
        let mut watch = PlanOutcomeWatch::default();
        let w = window(65);
        assert_eq!(watch.observe(Some(w), read(at(2, 0), 60, 65)), None);
        assert_eq!(watch.observe(Some(w), read(at(2, 30), 65, 65)), None);
        assert_eq!(watch.observe(None, read(at(5, 0), 65, 65)), None);
    }

    #[test]
    fn a_poll_that_comes_long_after_the_end_does_not_report() {
        let mut watch = PlanOutcomeWatch::default();
        let w = window(65);
        run_window(&mut watch, w, w.start_ts, |_| 65, 65);
        assert_eq!(watch.observe(None, read(at(8, 0), 40, 65)), None);
    }

    #[test]
    fn readings_that_stopped_before_the_end_do_not_report() {
        let mut watch = PlanOutcomeWatch::default();
        let w = window(65);
        assert_eq!(watch.observe(Some(w), read(at(2, 0), 65, 65)), None);
        assert_eq!(watch.observe(Some(w), read(at(3, 0), 65, 65)), None);
        // The dongle was away from 03:00 to the end of the window.
        assert_eq!(watch.observe(None, read(at(5, 1), 65, 65)), None);
    }

    #[test]
    fn a_clock_stepped_back_gives_up_the_window() {
        let mut watch = PlanOutcomeWatch::default();
        let w = window(65);
        assert_eq!(watch.observe(Some(w), read(at(3, 0), 65, 65)), None);
        assert_eq!(watch.observe(Some(w), read(at(2, 30), 65, 65)), None);
        assert_eq!(watch.observe(Some(w), read(at(4, 55), 65, 65)), None);
        assert_eq!(watch.observe(None, read(at(5, 0), 65, 65)), None);
    }

    #[test]
    fn joining_part_way_reports_the_end_level_but_not_when_it_was_reached() {
        let mut watch = PlanOutcomeWatch::default();
        let w = window(65);
        // HEM restarted at 03:30: the battery was already at target.
        run_window(&mut watch, w, at(3, 30), |_| 65, 65);
        let report = watch.observe(None, read(at(5, 0), 65, 65)).unwrap();
        assert!(report.joined_late);
        assert_eq!(report.reached_ts, None);
        assert_eq!(report.end_soc, 65);
    }

    // ---- the plan changing under the watch ----------------------------------

    #[test]
    fn a_slot_cleared_before_the_end_is_not_reported() {
        let mut watch = PlanOutcomeWatch::default();
        let w = window(65);
        assert_eq!(watch.observe(Some(w), read(at(2, 0), 50, 65)), None);
        assert_eq!(watch.observe(Some(w), read(at(3, 0), 55, 65)), None);
        // The user cleared the slot at 03:05; the window is no longer running.
        assert_eq!(watch.observe(None, read(at(3, 5), 55, 65)), None);
        assert_eq!(watch.observe(None, read(at(5, 0), 55, 65)), None);
    }

    #[test]
    fn a_replan_to_a_new_target_is_judged_against_the_new_target() {
        let mut watch = PlanOutcomeWatch::default();
        let old = window(65);
        assert_eq!(watch.observe(Some(old), read(at(2, 0), 50, 65)), None);
        assert_eq!(watch.observe(Some(old), read(at(2, 5), 55, 65)), None);
        let new = window(80);
        assert_eq!(watch.observe(Some(new), read(at(2, 10), 60, 80)), None);
        let mut ts = at(2, 15);
        while ts < new.end_ts {
            assert_eq!(watch.observe(Some(new), read(ts, 80, 80)), None);
            ts += 300;
        }
        let report = watch.observe(None, read(at(5, 0), 80, 80)).unwrap();
        assert_eq!(report.target, 80);
        assert_eq!(report.end_soc, 80);
        assert!(!report.fell_short());
    }

    #[test]
    fn a_new_window_already_running_at_the_end_still_reports_the_old_one() {
        // Slot 1 was rewritten to the next window as the first ended.
        let mut watch = PlanOutcomeWatch::default();
        let w = window(65);
        run_window(&mut watch, w, w.start_ts, |_| 65, 65);
        let next = ActiveWindow {
            start_ts: at(5, 0),
            end_ts: at(7, 0),
            target: 80,
        };
        let report = watch
            .observe(Some(next), read(at(5, 0), 64, 80))
            .expect("the old window is reported as the new one begins");
        assert_eq!(report.target, 65);
        assert_eq!(report.end_soc, 65);
        // And the new window is watched from its first reading.
        let mut ts = at(5, 5);
        while ts < next.end_ts {
            assert_eq!(watch.observe(Some(next), read(ts, 80, 80)), None);
            ts += 300;
        }
        let second = watch.observe(None, read(at(7, 0), 80, 80)).unwrap();
        assert_eq!(second.target, 80);
        assert!(!second.joined_late);
    }

    #[test]
    fn a_new_slot_the_next_poll_after_the_end_still_reports_the_old_window() {
        // At 05:00 the auto-apply may already have written tomorrow's slot,
        // so the first poll after the end sees no running window.
        let mut watch = PlanOutcomeWatch::default();
        let w = window(65);
        run_window(&mut watch, w, w.start_ts, |_| 65, 65);
        assert!(watch.observe(None, read(at(5, 0), 65, 100)).is_some());
    }

    // ---- the words ------------------------------------------------------------

    fn report_at(target: u8, end_soc: u8, peak_soc: u8, reached: Option<i64>) -> Report {
        Report {
            target,
            end_soc,
            peak_soc,
            reached_ts: reached,
            reported_target: target,
            window_end_ts: at(5, 0),
            joined_late: false,
        }
    }

    #[test]
    fn a_reached_target_says_when_and_how_it_finished() {
        assert_eq!(
            report_at(65, 65, 65, Some(at(2, 25))).message(),
            "🔋 Planned charge finished — reached the 65% target at 02:25 and was at 65% \
             when the window ended at 05:00."
        );
    }

    #[test]
    fn a_level_within_tolerance_is_reported_without_a_time() {
        assert_eq!(
            report_at(65, 64, 64, None).message(),
            "🔋 Planned charge finished — the battery was at 64% when the window ended \
             at 05:00 (target 65%)."
        );
    }

    #[test]
    fn a_late_join_does_not_claim_a_time() {
        let mut report = report_at(65, 65, 65, None);
        report.joined_late = true;
        let text = report.message();
        assert!(text.contains("was at 65% when the window ended at 05:00"));
        assert!(!text.contains("reached the"));
    }

    #[test]
    fn a_shortfall_warns_and_names_the_target_the_inverter_held() {
        let mut report = report_at(65, 41, 41, None);
        report.reported_target = 100;
        assert_eq!(
            report.message(),
            "⚠️ Planned charge fell short — the battery was at 41% when the window ended \
             at 05:00, below the 65% target. The inverter was reporting a target of 100%."
        );
    }

    #[test]
    fn a_shortfall_with_the_target_in_place_says_so() {
        let text = report_at(65, 41, 41, None).message();
        assert!(text.contains("The inverter was still reporting the 65% target."));
    }

    #[test]
    fn a_battery_that_reached_the_target_then_fell_back_says_so() {
        let text = report_at(65, 50, 65, Some(at(3, 0))).message();
        assert!(text.starts_with("⚠️ Planned charge fell short"));
        assert!(text.contains("It reached 65% and then fell back to 50% before the window ended."));
    }

    // ---- the poll-loop glue -------------------------------------------------

    use crate::inverter::model::DeviceType;
    use crate::test_util::with_isolated_config_dir_async;

    fn gen2_charging(soc: u8, target_soc: u8) -> InverterSnapshot {
        let mut snapshot = InverterSnapshot {
            device_type: DeviceType::Gen2Hybrid,
            soc,
            target_soc,
            ..Default::default()
        };
        snapshot.charge_slots[0] = slot(true, (2, 0), (5, 0));
        snapshot
    }

    /// A state whose Planner has planned the 02:00-05:00 slot to `target`.
    async fn planning(target: u8) -> Arc<AppState> {
        let state = Arc::new(AppState::new());
        note_planned_slot(&state, Some(planned(target))).await;
        state
    }

    #[tokio::test]
    async fn the_runner_reports_a_window_from_polls_to_the_end() {
        with_isolated_config_dir_async(|| async {
            let state = planning(65).await;
            let settings = Settings::default();
            let mut minute = 0;
            while minute < 180 {
                let now = local(2 + minute / 60, minute % 60);
                let snapshot = gen2_charging(41, 100);
                assert_eq!(
                    run_plan_outcome(&state, &settings, &snapshot, now).await,
                    None
                );
                minute += 5;
            }
            let report = run_plan_outcome(&state, &settings, &gen2_charging(41, 100), local(5, 0))
                .await
                .expect("the poll after the end reports");
            assert!(report.fell_short());
            assert_eq!(report.end_soc, 41);
            assert_eq!(report.reported_target, 100);
        })
        .await;
    }

    #[tokio::test]
    async fn a_gateway_is_never_reported_on() {
        with_isolated_config_dir_async(|| async {
            let state = planning(65).await;
            let settings = Settings::default();
            let mut gateway = gen2_charging(41, 65);
            gateway.device_type = DeviceType::Gateway;
            for minute in (0..180).step_by(5) {
                let now = local(2 + minute / 60, minute % 60);
                assert_eq!(
                    run_plan_outcome(&state, &settings, &gateway, now).await,
                    None
                );
            }
            assert_eq!(
                run_plan_outcome(&state, &settings, &gateway, local(5, 0)).await,
                None
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_users_own_slot_is_never_reported_on() {
        with_isolated_config_dir_async(|| async {
            // The Planner's 02:00-05:00 plan is still recorded, but the user
            // has since saved their own 00:30-04:30 slot over it.
            let state = planning(65).await;
            let settings = Settings::default();
            let mut own = gen2_charging(41, 100);
            own.charge_slots[0] = slot(true, (0, 30), (4, 30));
            for minute in (30..270).step_by(5) {
                let now = local(minute / 60, minute % 60);
                assert_eq!(run_plan_outcome(&state, &settings, &own, now).await, None);
            }
            assert_eq!(
                run_plan_outcome(&state, &settings, &own, local(4, 30)).await,
                None
            );
        })
        .await;
    }

    #[tokio::test]
    async fn nothing_planned_means_nothing_reported() {
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let settings = Settings::default();
            for minute in (0..180).step_by(5) {
                let now = local(2 + minute / 60, minute % 60);
                let snapshot = gen2_charging(41, 65);
                assert_eq!(
                    run_plan_outcome(&state, &settings, &snapshot, now).await,
                    None
                );
            }
            assert_eq!(
                run_plan_outcome(&state, &settings, &gen2_charging(41, 65), local(5, 0)).await,
                None
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_later_plan_for_another_slot_ends_the_watch() {
        with_isolated_config_dir_async(|| async {
            let state = planning(65).await;
            let settings = Settings::default();
            for minute in (0..60).step_by(5) {
                let now = local(2, minute);
                assert_eq!(
                    run_plan_outcome(&state, &settings, &gen2_charging(41, 100), now).await,
                    None
                );
            }
            // A full-charge plan (or a cleared slot) replaces the hold.
            note_planned_slot(&state, None).await;
            assert_eq!(
                run_plan_outcome(&state, &settings, &gen2_charging(41, 100), local(5, 0)).await,
                None
            );
        })
        .await;
    }

    #[test]
    fn only_auto_apply_notifies() {
        let on = Settings {
            forecast_plan_auto_apply_enabled: true,
            ..Settings::default()
        };
        assert!(notifies(&on));
        assert!(!notifies(&Settings::default()));
    }

    #[test]
    fn the_reported_target_is_the_slots_own_where_the_model_has_one() {
        let mut gen3 = gen2_charging(50, 100);
        gen3.device_type = DeviceType::Gen3Hybrid;
        gen3.charge_slots[0].target_soc = 65;
        assert_eq!(reported_target(&gen3), 65);
        assert_eq!(reported_target(&gen2_charging(50, 90)), 90);
    }
}
