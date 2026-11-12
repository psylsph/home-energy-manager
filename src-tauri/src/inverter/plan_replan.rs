//! Re-plan charge slot 1 when Octopus moves a dispatch (issue #404).
//!
//! Auto-apply and the nightly refresh write charge slot 1 once per cheap
//! window. Octopus keeps moving Intelligent Octopus Go smart charges after
//! that, and the plan models them (#406, #434), so a plan sized against the
//! old list goes stale. This keeps the slot matched to the list until the
//! window it was written for ends.
//!
//! Invariants (docs/issue-404-replan-plan.md):
//! - Only acts on a cycle a machine owner (auto-apply or the refresh) wrote,
//!   and only while that owner is still on.
//! - The same gates as the daily write (a Gateway, Adaptive Charge owning
//!   the rate, the poll sitting out), plus a running top-up (#434), which
//!   writes charge slot 1 too. A gated change stays pending until the gate
//!   lifts.
//! - A stale or missing list never replans; a plan made without a fresh
//!   list counts the first fresh one as a change.
//! - Writes only when the slot materially changes; one computation per
//!   change of list, worked or not.
//! - Never writes into another window than the cycle's, and stops at its end.
//! - Inside the window it plans only near the top of an hour, where the
//!   forecast's hours start at the current one.
//! - In memory only: a restart never writes on its own.
//! - Once charge slot 1 reads back as something other than what the plan
//!   wrote on [`TAKEN_OVER_READS`] reads in a row (the user changed it, or
//!   the write never landed), the cycle ends: a replan never overwrites
//!   someone else's slot.

use chrono::{DateTime, Local, NaiveDate, Timelike};

use crate::inverter::encoder::RegisterWrite;
use crate::inverter::forecast_plan::{PlanTrigger, SlotPlan};
use crate::settings::TariffConfig;

/// A smaller move than this is logged, not written (agreed with Stuart,
/// 10 November 2026): Octopus reshuffles by minutes all evening.
pub const MATERIAL_MOVE_MINUTES: u16 = 15;

/// Inside the window, a pending change is planned only in the first this
/// many minutes of an hour: the forecast is built from the hour's start,
/// so the minutes since then are counted as still to come.
pub const IN_WINDOW_REPLAN_MINUTES: u32 = 10;

/// Reads in a row that must show another slot before the cycle lets go, so
/// one stale or garbled dongle read doesn't end it.
pub const TAKEN_OVER_READS: u8 = 2;

/// Minutes past midnight for an HHMM register value.
pub(crate) fn hhmm_minutes(hhmm: u16) -> u16 {
    (hhmm / 100) * 60 + hhmm % 100
}

/// The local instant `minutes` past midnight on `date` (1440 is the next
/// midnight), resolved the way the planner dates window occurrences.
pub(crate) fn local_ts(date: NaiveDate, minutes: u32) -> Option<i64> {
    use chrono::TimeZone;
    let (date, minutes) = if minutes >= 1440 {
        (date.succ_opt()?, minutes - 1440)
    } else {
        (date, minutes)
    };
    let naive = date.and_hms_opt(minutes / 60, minutes % 60, 0)?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .or_else(|| Local.from_local_datetime(&naive).single())
        .map(|dt| dt.timestamp())
}

/// What charge slot 1 was set to, in the terms that decide a rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotSummary {
    Charge {
        start_hhmm: u16,
        end_hhmm: u16,
    },
    Hold {
        start_hhmm: u16,
        end_hhmm: u16,
        target_soc: u8,
    },
    Clear,
}

impl SlotSummary {
    fn start_hhmm(self) -> Option<u16> {
        match self {
            Self::Charge { start_hhmm, .. } | Self::Hold { start_hhmm, .. } => Some(start_hhmm),
            Self::Clear => None,
        }
    }
}

/// What a slot plan would queue: its summary, the writes that set the
/// slot's times, and whether it is for the next day's window. None for a
/// plan that writes nothing.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct QueuedSlot {
    pub summary: SlotSummary,
    pub slot_writes: Vec<RegisterWrite>,
    pub tomorrow: bool,
}

impl QueuedSlot {
    pub(crate) fn of(plan: &SlotPlan) -> Option<Self> {
        let (summary, writes, tomorrow) = match plan {
            SlotPlan::Write {
                start_hhmm,
                end_hhmm,
                tomorrow,
                writes: Ok(writes),
                ..
            } => (
                SlotSummary::Charge {
                    start_hhmm: *start_hhmm,
                    end_hhmm: *end_hhmm,
                },
                writes,
                *tomorrow,
            ),
            SlotPlan::WriteHold {
                start_hhmm,
                end_hhmm,
                target_soc,
                tomorrow,
                writes: Ok(writes),
                ..
            } => (
                SlotSummary::Hold {
                    start_hhmm: *start_hhmm,
                    end_hhmm: *end_hhmm,
                    target_soc: *target_soc,
                },
                writes,
                *tomorrow,
            ),
            SlotPlan::Clear { writes: Ok(writes) } => (SlotSummary::Clear, writes, false),
            _ => return None,
        };
        Some(Self {
            summary,
            slot_writes: slot_time_writes(writes),
            tomorrow,
        })
    }
}

/// The slot one machine write set for one cheap-window occurrence.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlanCycle {
    /// Which owner wrote it: decides whether a replan notifies.
    pub trigger: PlanTrigger,
    /// When it was written (Unix seconds); a clock stepped back before this
    /// ends the cycle.
    pub written_at: i64,
    /// The cheap-window occurrence it is for (Unix seconds).
    pub window_start: i64,
    pub window_end: i64,
    /// The tariff window's start, as the slot writes it.
    pub window_start_hhmm: u16,
    pub slot: SlotSummary,
    /// The dispatch list revision the plan was made with; none when it had
    /// no fresh list, so the first fresh one is a change.
    pub dispatch_revision: Option<u64>,
    /// The slot's start and end writes, to tell whether it still reads back.
    pub slot_writes: Vec<RegisterWrite>,
    /// Reads in a row that showed another slot.
    pub mismatched_reads: u8,
}

impl PlanCycle {
    /// When a slot ending at `end_hhmm` ends: the first such time from the
    /// window's start, in local time (a clock-change night included).
    pub(crate) fn slot_end_ts(&self, end_hhmm: u16) -> i64 {
        let start_min = u32::from(hhmm_minutes(self.window_start_hhmm));
        let end_min = u32::from(hhmm_minutes(end_hhmm));
        let minutes = if end_min >= start_min {
            end_min
        } else {
            end_min + 1440
        };
        chrono::DateTime::from_timestamp(self.window_start, 0)
            .and_then(|dt| local_ts(dt.with_timezone(&Local).date_naive(), minutes))
            .unwrap_or(self.window_start + i64::from(minutes - start_min) * 60)
    }

    /// Whether `new`, written at `now`, ends a charge that is running: the
    /// window has started and it charges nothing more.
    pub(crate) fn stops_charging(&self, new: SlotSummary, now: i64) -> bool {
        now >= self.window_start && self.done_charging(new, now)
    }

    /// Whether `slot` charges nothing from `now` on: cleared, or a charge
    /// whose end has passed. A hold runs to the window's end.
    pub(crate) fn done_charging(&self, slot: SlotSummary, now: i64) -> bool {
        match slot {
            SlotSummary::Clear => true,
            SlotSummary::Charge { end_hhmm, .. } => self.slot_end_ts(end_hhmm) <= now,
            SlotSummary::Hold { .. } => false,
        }
    }

    /// The part of a plan's `kwh` still to charge from `now`: a charge slot
    /// sized from the window's start has charged the rest already.
    pub(crate) fn kwh_from(&self, slot: SlotSummary, kwh: f64, now: i64) -> f64 {
        let SlotSummary::Charge { end_hhmm, .. } = slot else {
            return kwh;
        };
        let length = self.slot_end_ts(end_hhmm) - self.window_start;
        if length <= 0 {
            return 0.0;
        }
        let left = (self.slot_end_ts(end_hhmm) - now.max(self.window_start)).clamp(0, length);
        kwh * left as f64 / length as f64
    }
}

/// The writes that set charge slot 1's times, out of a plan's batch.
pub(crate) fn slot_time_writes(writes: &[RegisterWrite]) -> Vec<RegisterWrite> {
    use crate::modbus::registers::*;
    writes
        .iter()
        .filter(|write| {
            matches!(
                write.address,
                HR_CHARGE_SLOT_1_START
                    | HR_CHARGE_SLOT_1_END
                    | HR_3PH_CHARGE_SLOT_1_START
                    | HR_3PH_CHARGE_SLOT_1_END
            )
        })
        .cloned()
        .collect()
}

/// The reads in a row that showed another slot, after this poll's read:
/// `matches` is none when the read can't tell (writes still queued, or a
/// read from before the plan's writes).
pub(crate) fn mismatch_count(previous: u8, matches: Option<bool>) -> u8 {
    match matches {
        None => previous,
        Some(true) => 0,
        Some(false) => previous.saturating_add(1),
    }
}

/// The next occurrence of the planned cheap window from `now` (the cheapest,
/// or with every cheap window planned the next of them, issue #453), as the
/// daily write's due-check picks it: `(start, end)` in Unix seconds and the
/// start as HHMM. Both ends are resolved in local time, so a clock-change
/// night has its real length. A window ending at 23:59 ends at midnight.
pub(crate) fn cycle_window(
    tariff: &TariffConfig,
    now: DateTime<Local>,
    every_cheap_window: bool,
) -> Option<(i64, i64, u16)> {
    let now_min = now.hour() as u16 * 60 + now.minute() as u16;
    let window = crate::forecast::planner::plan_window(tariff, now_min, every_cheap_window)?;
    let today = now.date_naive();
    let start_date = if window.start_min >= now_min {
        today
    } else {
        today.succ_opt()?
    };
    let start_min = u32::from(window.start_min);
    let end_min = match window.end_min {
        1439 => 1440,
        end => u32::from(end),
    };
    let end_min = if end_min > start_min {
        end_min
    } else {
        end_min + 1440
    };
    let start = local_ts(start_date, start_min)?;
    let end = local_ts(start_date, end_min)?;
    let hhmm = (window.start_min / 60) * 100 + window.start_min % 60;
    Some((start, end, hhmm))
}

/// Minutes between two HHMM times on a 24 h clock, the short way round.
fn minutes_apart(a: u16, b: u16) -> u16 {
    let diff = (hhmm_minutes(a) + 1440 - hhmm_minutes(b)) % 1440;
    diff.min(1440 - diff)
}

/// Whether `new` is worth a rewrite over `old`: another action, another
/// hold target, or an end of the slot moved by [`MATERIAL_MOVE_MINUTES`].
pub(crate) fn materially_differs(old: SlotSummary, new: SlotSummary) -> bool {
    let moved = |a: u16, b: u16| minutes_apart(a, b) >= MATERIAL_MOVE_MINUTES;
    match (old, new) {
        (SlotSummary::Clear, SlotSummary::Clear) => false,
        (
            SlotSummary::Charge {
                start_hhmm: s1,
                end_hhmm: e1,
            },
            SlotSummary::Charge {
                start_hhmm: s2,
                end_hhmm: e2,
            },
        ) => moved(s1, s2) || moved(e1, e2),
        (
            SlotSummary::Hold {
                start_hhmm: s1,
                end_hhmm: e1,
                target_soc: t1,
            },
            SlotSummary::Hold {
                start_hhmm: s2,
                end_hhmm: e2,
                target_soc: t2,
            },
        ) => t1 != t2 || moved(s1, s2) || moved(e1, e2),
        _ => true,
    }
}

/// What the poll loop knows that decides whether to replan.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReplanFacts {
    pub now: i64,
    /// Local minutes past the hour at `now`.
    pub minute_of_hour: u32,
    /// The cycle's owner is still the one writing charge slot 1.
    pub owner_on: bool,
    /// Reads in a row that showed another slot than the plan wrote
    /// ([`mismatch_count`]): [`TAKEN_OVER_READS`] of them end the cycle,
    /// and fewer hold a replan back until a read settles it.
    pub mismatched_reads: u8,
    /// Battery writes reach the battery (not a Gateway).
    pub reaches_battery: bool,
    pub adaptive_owns_rate: bool,
    pub sit_out: bool,
    /// A top-up during a smart charge is running (#434).
    pub top_up_running: bool,
    /// The dispatch list is wanted and fresh.
    pub list_fresh: bool,
    pub revision: u64,
}

/// The decision before any computation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReplanStep {
    /// Nothing to do this poll.
    Idle,
    /// The cycle is over (or its owner let go): forget it.
    EndCycle,
    /// A change is pending but can't be acted on yet.
    Wait(&'static str),
    /// Compute a fresh plan: of the next window, or (`running`, its start)
    /// of the rest of the one under way.
    Compute { running: Option<i64> },
}

pub(crate) fn replan_step(cycle: &PlanCycle, facts: &ReplanFacts) -> ReplanStep {
    if facts.now < cycle.written_at || facts.now >= cycle.window_end || !facts.owner_on {
        return ReplanStep::EndCycle;
    }
    // A top-up writes its own slot 1 and puts the plan's back afterwards,
    // so while it runs the slot says nothing about who owns it.
    if facts.top_up_running {
        return ReplanStep::Wait("a top-up during a smart charge is running");
    }
    if facts.mismatched_reads >= TAKEN_OVER_READS {
        return ReplanStep::EndCycle;
    }
    if cycle.dispatch_revision == Some(facts.revision) {
        return ReplanStep::Idle;
    }
    if facts.mismatched_reads > 0 {
        return ReplanStep::Wait("charge slot 1 didn't read back as the plan set it");
    }
    if !facts.reaches_battery {
        return ReplanStep::Wait("battery controls don't reach the battery");
    }
    if facts.adaptive_owns_rate {
        return ReplanStep::Wait("Adaptive Charge owns the charge rate");
    }
    if facts.sit_out {
        return ReplanStep::Wait("the poll is sitting out");
    }
    if !facts.list_fresh {
        return ReplanStep::Wait("the dispatch list is stale");
    }
    if facts.now < cycle.window_start {
        return ReplanStep::Compute { running: None };
    }
    if facts.minute_of_hour >= IN_WINDOW_REPLAN_MINUTES {
        return ReplanStep::Wait("waiting for the top of the hour");
    }
    ReplanStep::Compute {
        running: Some(cycle.window_start),
    }
}

/// What a fresh plan means for the cycle.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ReplanOutcome {
    /// Write it: the slot changed enough.
    Rewrite(QueuedSlot),
    /// Leave the slot; the reason is logged.
    Keep(&'static str),
}

pub(crate) fn replan_outcome(cycle: &PlanCycle, plan: &SlotPlan, now: i64) -> ReplanOutcome {
    let Some(queued) = QueuedSlot::of(plan) else {
        return ReplanOutcome::Keep("the fresh plan has nothing to write");
    };
    let new = queued.summary;
    // The fresh plan is for the cycle's window: it starts there and isn't
    // flagged as the next day's. Anything else is not this cycle's.
    if queued.tomorrow
        || new
            .start_hhmm()
            .is_some_and(|start| start != cycle.window_start_hhmm)
    {
        return ReplanOutcome::Keep("the fresh plan is for another window");
    }
    if cycle.done_charging(cycle.slot, now) && cycle.done_charging(new, now) {
        return ReplanOutcome::Keep("the battery has already stopped charging");
    }
    if !materially_differs(cycle.slot, new) {
        return ReplanOutcome::Keep("the slot barely moved");
    }
    ReplanOutcome::Rewrite(queued)
}

fn describe(slot: SlotSummary) -> String {
    let hhmm = |v: u16| format!("{:02}:{:02}", v / 100, v % 100);
    match slot {
        SlotSummary::Charge {
            start_hhmm,
            end_hhmm,
        } => format!("{}–{}", hhmm(start_hhmm), hhmm(end_hhmm)),
        SlotSummary::Hold {
            start_hhmm,
            end_hhmm,
            target_soc,
        } => format!("to {target_soc}% {}–{}", hhmm(start_hhmm), hhmm(end_hhmm)),
        SlotSummary::Clear => "no charge".to_string(),
    }
}

/// The message for a replan that rewrote the slot. `kwh` is what is still
/// to charge ([`PlanCycle::kwh_from`]). `stops_now`: inside the window, the
/// new slot charges nothing more ([`PlanCycle::stops_charging`]), so the
/// charge that is running ends.
pub(crate) fn replan_message(
    old: SlotSummary,
    new: SlotSummary,
    kwh: f64,
    stops_now: bool,
) -> String {
    let was = describe(old);
    if stops_now {
        return format!(
            "📋 Octopus moved the car's smart charge, so the battery has enough: charging \
             stops now (was {was})."
        );
    }
    match new {
        SlotSummary::Clear => format!(
            "📋 Octopus moved the car's smart charge, so the battery no longer needs charging \
             tonight: charge slot 1 cleared (was {was})."
        ),
        SlotSummary::Charge { .. } | SlotSummary::Hold { .. } => format!(
            "📋 Octopus moved the car's smart charge, so HEM changed the battery charge to {} \
             (was {was}), about {kwh:.1} kWh.",
            describe(new)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::TariffSlot;
    use chrono::TimeZone;

    fn local_on(day: u32, h: u32, m: u32) -> DateTime<Local> {
        Local
            .from_local_datetime(
                &NaiveDate::from_ymd_opt(2026, 11, day)
                    .unwrap()
                    .and_hms_opt(h, m, 20)
                    .unwrap(),
            )
            .earliest()
            .unwrap()
    }

    fn local(h: u32, m: u32) -> DateTime<Local> {
        local_on(10, h, m)
    }

    /// Intelligent Octopus Go: 23:30–05:30 off-peak, written across midnight.
    fn iog() -> TariffConfig {
        let slot = |s: &str, e: &str, r: f64| TariffSlot {
            start: s.into(),
            end: e.into(),
            rate: r,
        };
        TariffConfig {
            slots: vec![
                slot("00:00", "05:30", 0.07),
                slot("05:30", "23:30", 0.30),
                slot("23:30", "23:59", 0.07),
            ],
        }
    }

    fn charge(start_hhmm: u16, end_hhmm: u16) -> SlotSummary {
        SlotSummary::Charge {
            start_hhmm,
            end_hhmm,
        }
    }

    fn cycle() -> PlanCycle {
        let (window_start, window_end, window_start_hhmm) =
            cycle_window(&iog(), local(23, 0), false).unwrap();
        PlanCycle {
            trigger: PlanTrigger::AutoApply,
            written_at: local(23, 0).timestamp(),
            window_start,
            window_end,
            window_start_hhmm,
            slot: charge(2330, 340),
            dispatch_revision: Some(4),
            slot_writes: Vec::new(),
            mismatched_reads: 0,
        }
    }

    /// 23:10, before the window.
    fn before() -> i64 {
        local(23, 10).timestamp()
    }

    /// 01:00 the next morning, an hour and a half into the window.
    fn in_window() -> i64 {
        local_on(11, 1, 0).timestamp()
    }

    fn facts_at(now: i64, revision: u64) -> ReplanFacts {
        let minute_of_hour = chrono::DateTime::from_timestamp(now, 0)
            .unwrap()
            .with_timezone(&Local)
            .minute();
        ReplanFacts {
            now,
            minute_of_hour,
            owner_on: true,
            mismatched_reads: 0,
            reaches_battery: true,
            adaptive_owns_rate: false,
            sit_out: false,
            top_up_running: false,
            list_fresh: true,
            revision,
        }
    }

    fn facts(now: DateTime<Local>, revision: u64) -> ReplanFacts {
        facts_at(now.timestamp(), revision)
    }

    fn write(start_hhmm: u16, end_hhmm: u16) -> SlotPlan {
        SlotPlan::Write {
            start_hhmm,
            end_hhmm,
            kwh: 5.0,
            tomorrow: false,
            writes: Ok(Vec::new()),
        }
    }

    fn rewritten(outcome: ReplanOutcome) -> SlotSummary {
        match outcome {
            ReplanOutcome::Rewrite(queued) => queued.summary,
            other => panic!("expected a rewrite, got {other:?}"),
        }
    }

    // ---- cycle_window ----

    #[test]
    fn the_cycle_is_tonights_wrapping_window_in_local_time() {
        let (start, end, hhmm) = cycle_window(&iog(), local(23, 0), false).unwrap();
        assert_eq!(hhmm, 2330);
        assert_eq!(start, local(23, 30).timestamp() - 20);
        // 05:30 the next morning as the local clock reads it, whatever the
        // night's length (a clock change makes it 5 or 7 hours).
        assert_eq!(end, local_on(11, 5, 30).timestamp() - 20);
    }

    #[test]
    fn a_window_ending_at_23_59_ends_at_midnight() {
        let slot = |s: &str, e: &str, r: f64| TariffSlot {
            start: s.into(),
            end: e.into(),
            rate: r,
        };
        let tariff = TariffConfig {
            slots: vec![slot("00:00", "22:00", 0.30), slot("22:00", "23:59", 0.07)],
        };
        let (start, end, hhmm) = cycle_window(&tariff, local(21, 45), false).unwrap();
        assert_eq!(hhmm, 2200);
        assert_eq!(start, local(22, 0).timestamp() - 20);
        assert_eq!(end, local_on(11, 0, 0).timestamp() - 20);
    }

    #[test]
    fn the_slot_ends_on_the_cycles_local_timeline() {
        let cycle = cycle();
        assert_eq!(cycle.slot_end_ts(10), local_on(11, 0, 10).timestamp() - 20);
        assert_eq!(cycle.slot_end_ts(2330), cycle.window_start);
        assert_eq!(cycle.slot_end_ts(530), cycle.window_end);
    }

    // ---- materiality ----

    #[test]
    fn a_move_under_fifteen_minutes_is_not_material() {
        assert!(!materially_differs(charge(2330, 340), charge(2330, 354)));
        assert!(materially_differs(charge(2330, 340), charge(2330, 355)));
        assert!(materially_differs(charge(2330, 340), charge(2330, 325)));
        // Across midnight, the short way round.
        assert!(!materially_differs(charge(2330, 2355), charge(2330, 5)));
    }

    #[test]
    fn another_action_or_hold_target_is_material() {
        let hold = |t| SlotSummary::Hold {
            start_hhmm: 2330,
            end_hhmm: 530,
            target_soc: t,
        };
        assert!(materially_differs(charge(2330, 340), SlotSummary::Clear));
        assert!(materially_differs(SlotSummary::Clear, charge(2330, 340)));
        assert!(materially_differs(charge(2330, 530), hold(80)));
        assert!(materially_differs(hold(80), hold(85)));
        assert!(!materially_differs(hold(80), hold(80)));
        assert!(!materially_differs(SlotSummary::Clear, SlotSummary::Clear));
    }

    // ---- replan_step ----

    #[test]
    fn an_unchanged_list_does_nothing() {
        assert_eq!(
            replan_step(&cycle(), &facts(local(23, 10), 4)),
            ReplanStep::Idle
        );
    }

    #[test]
    fn a_changed_list_before_the_window_plans_the_next_window() {
        assert_eq!(
            replan_step(&cycle(), &facts(local(23, 10), 5)),
            ReplanStep::Compute { running: None }
        );
    }

    #[test]
    fn a_plan_made_without_a_fresh_list_takes_the_first_fresh_one_as_a_change() {
        let unlisted = PlanCycle {
            dispatch_revision: None,
            ..cycle()
        };
        // The same revision the stale list had: still a change.
        assert_eq!(
            replan_step(&unlisted, &facts(local(23, 10), 4)),
            ReplanStep::Compute { running: None }
        );
    }

    #[test]
    fn inside_the_window_it_plans_the_rest_near_the_top_of_an_hour() {
        let cycle = cycle();
        assert_eq!(
            replan_step(&cycle, &facts_at(in_window() + 5 * 60, 5)),
            ReplanStep::Compute {
                running: Some(cycle.window_start)
            }
        );
        // Mid-hour the forecast couldn't see the minutes to the next hour:
        // the change waits for it, still pending.
        assert!(matches!(
            replan_step(&cycle, &facts_at(in_window() + 25 * 60, 5)),
            ReplanStep::Wait(_)
        ));
        // From 23:30 to midnight is mid-hour too.
        assert!(matches!(
            replan_step(&cycle, &facts(local(23, 45), 5)),
            ReplanStep::Wait(_)
        ));
    }

    #[test]
    fn the_cycle_ends_with_its_window_its_owner_or_a_clock_stepped_back() {
        let mut at_end = facts(local(23, 10), 5);
        at_end.now = cycle().window_end;
        assert_eq!(replan_step(&cycle(), &at_end), ReplanStep::EndCycle);

        let mut owner_off = facts(local(23, 10), 5);
        owner_off.owner_on = false;
        assert_eq!(replan_step(&cycle(), &owner_off), ReplanStep::EndCycle);

        assert_eq!(
            replan_step(&cycle(), &facts(local(22, 0), 5)),
            ReplanStep::EndCycle
        );

        // Even with no change of list: someone else's slot is never touched.
        let mut taken = facts(local(23, 10), 4);
        taken.mismatched_reads = TAKEN_OVER_READS;
        assert_eq!(replan_step(&cycle(), &taken), ReplanStep::EndCycle);

        // Except during a top-up, which has its own slot 1 on the inverter
        // until it puts the plan's back.
        taken.top_up_running = true;
        assert!(matches!(replan_step(&cycle(), &taken), ReplanStep::Wait(_)));
    }

    #[test]
    fn every_gate_holds_the_change_back_without_dropping_it() {
        let gates: [fn(&mut ReplanFacts); 5] = [
            |f| f.reaches_battery = false,
            |f| f.adaptive_owns_rate = true,
            |f| f.sit_out = true,
            |f| f.top_up_running = true,
            |f| f.list_fresh = false,
        ];
        for gate in gates {
            let mut gated = facts(local(23, 10), 5);
            gate(&mut gated);
            assert!(matches!(replan_step(&cycle(), &gated), ReplanStep::Wait(_)));
        }
    }

    // ---- reading the slot back ----

    #[test]
    fn it_takes_reads_in_a_row_to_decide_the_slot_was_taken_over() {
        let mut count = 0;
        count = mismatch_count(count, Some(false));
        assert!(count < TAKEN_OVER_READS, "one stale read isn't enough");
        count = mismatch_count(count, None);
        assert_eq!(count, 1, "a read that can't tell changes nothing");
        count = mismatch_count(count, Some(true));
        assert_eq!(count, 0, "a matching read starts again");
        for _ in 0..TAKEN_OVER_READS {
            count = mismatch_count(count, Some(false));
        }
        assert!(count >= TAKEN_OVER_READS);
    }

    #[test]
    fn one_read_of_another_slot_holds_a_replan_back_without_ending_the_cycle() {
        let mut once = facts(local(23, 10), 5);
        once.mismatched_reads = 1;
        assert!(matches!(replan_step(&cycle(), &once), ReplanStep::Wait(_)));
    }

    #[test]
    fn only_the_slot_times_decide_whether_it_was_taken_over() {
        let w = |address, value| RegisterWrite { address, value };
        let batch = [w(94, 2330), w(95, 340), w(20, 0), w(96, 1), w(111, 50)];
        assert_eq!(slot_time_writes(&batch), vec![w(94, 2330), w(95, 340)]);
        let three_phase = [w(1113, 2330), w(1114, 340), w(1110, 100)];
        assert_eq!(
            slot_time_writes(&three_phase),
            vec![w(1113, 2330), w(1114, 340)]
        );
    }

    // ---- replan_outcome ----

    #[test]
    fn a_moved_slot_is_rewritten_and_a_barely_moved_one_kept() {
        assert_eq!(
            rewritten(replan_outcome(&cycle(), &write(2330, 210), before())),
            charge(2330, 210)
        );
        assert!(matches!(
            replan_outcome(&cycle(), &write(2330, 350), before()),
            ReplanOutcome::Keep(_)
        ));
    }

    #[test]
    fn no_charge_needed_now_clears_the_slot() {
        let clear = SlotPlan::Clear {
            writes: Ok(Vec::new()),
        };
        assert_eq!(
            rewritten(replan_outcome(&cycle(), &clear, before())),
            SlotSummary::Clear
        );
    }

    #[test]
    fn a_plan_with_nothing_to_write_or_for_another_window_is_kept() {
        let no_plan = SlotPlan::NoPlan {
            reason: "no forecast".into(),
        };
        let unencodable = SlotPlan::Write {
            start_hhmm: 2330,
            end_hhmm: 210,
            kwh: 5.0,
            tomorrow: false,
            writes: Err("no".into()),
        };
        let tomorrow = SlotPlan::Write {
            tomorrow: true,
            start_hhmm: 2330,
            end_hhmm: 210,
            kwh: 5.0,
            writes: Ok(Vec::new()),
        };
        for plan in [no_plan, unencodable, write(200, 400), tomorrow] {
            assert!(matches!(
                replan_outcome(&cycle(), &plan, before()),
                ReplanOutcome::Keep(_)
            ));
        }
    }

    #[test]
    fn a_running_slot_cut_short_of_now_stops_the_charge() {
        let cycle = cycle();
        let new = rewritten(replan_outcome(&cycle, &write(2330, 10), in_window()));
        assert_eq!(new, charge(2330, 10));
        assert!(cycle.stops_charging(new, in_window()));
        assert!(cycle.stops_charging(SlotSummary::Clear, in_window()));
        assert!(!cycle.stops_charging(SlotSummary::Clear, before()));
    }

    #[test]
    fn a_charge_already_over_is_not_rewritten_for_another_ending_in_the_past() {
        let stopped = PlanCycle {
            slot: charge(2330, 0),
            ..cycle()
        };
        let clear = SlotPlan::Clear {
            writes: Ok(Vec::new()),
        };
        for plan in [write(2330, 40), clear] {
            assert!(matches!(
                replan_outcome(&stopped, &plan, in_window()),
                ReplanOutcome::Keep(_)
            ));
        }
        // But a charge still to come is.
        assert_eq!(
            rewritten(replan_outcome(&stopped, &write(2330, 300), in_window())),
            charge(2330, 300)
        );
    }

    // ---- message ----

    #[test]
    fn the_energy_told_is_what_is_still_to_charge() {
        let cycle = cycle();
        // 23:30–03:30 is four hours; at 01:00 two and a half are left.
        let slot = charge(2330, 330);
        assert!((cycle.kwh_from(slot, 8.0, before()) - 8.0).abs() < 1e-9);
        // (`in_window` is 20 seconds past the minute.)
        assert!((cycle.kwh_from(slot, 8.0, in_window()) - 5.0).abs() < 0.02);
        assert_eq!(cycle.kwh_from(charge(2330, 0), 1.0, in_window()), 0.0);
    }

    #[test]
    fn the_message_says_what_changed() {
        assert_eq!(
            replan_message(charge(2330, 340), charge(2330, 210), 4.2, false),
            "📋 Octopus moved the car's smart charge, so HEM changed the battery charge to \
             23:30–02:10 (was 23:30–03:40), about 4.2 kWh."
        );
        assert_eq!(
            replan_message(charge(2330, 340), SlotSummary::Clear, 0.0, false),
            "📋 Octopus moved the car's smart charge, so the battery no longer needs charging \
             tonight: charge slot 1 cleared (was 23:30–03:40)."
        );
        assert_eq!(
            replan_message(charge(2330, 340), SlotSummary::Clear, 0.0, true),
            "📋 Octopus moved the car's smart charge, so the battery has enough: charging \
             stops now (was 23:30–03:40)."
        );
        assert!(
            replan_message(SlotSummary::Clear, charge(2330, 100), 3.0, false)
                .contains("(was no charge)")
        );
    }

    #[test]
    fn the_cycle_is_for_the_window_the_plan_picks() {
        // Issue #453: with every cheap window planned, the cycle is the next
        // cheap window, as the planner plans it, not the longest one.
        let slot = |s: &str, e: &str, r: f64| crate::settings::TariffSlot {
            start: s.into(),
            end: e.into(),
            rate: r,
        };
        let tariff = TariffConfig {
            slots: vec![
                slot("00:00", "01:00", 0.10),
                slot("01:00", "13:00", 0.27),
                slot("13:00", "14:00", 0.10),
                slot("14:00", "22:00", 0.27),
                slot("22:00", "23:59", 0.10),
            ],
        };
        let now = local(12, 30);
        let (start, end, hhmm) = cycle_window(&tariff, now, true).unwrap();
        assert_eq!(hhmm, 1300);
        assert_eq!(end - start, 3600);
        let (_, _, hhmm) = cycle_window(&tariff, now, false).unwrap();
        assert_eq!(hhmm, 2200);
    }
}
