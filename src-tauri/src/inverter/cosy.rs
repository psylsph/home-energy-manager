//! Cosy charging mode (Octopus Cosy tariff slots).
//!
//! The schedule is written into the inverter's own charge slot 1 registers so
//! the inverter follows it independently:
//!
//! - **In a slot**: write the current slot's times, enable charge and the target
//!   SOC.
//! - **Not in a slot**: preload the NEXT upcoming slot's times with charge
//!   disabled so the inverter has the schedule ready, or clear the registers if
//!   there is none. If the app crashes the inverter already holds the correct
//!   schedule.
//!
//! [`plan_cosy_step`] decides what this cycle does and is pure;
//! [`run_cosy_step`] applies it (arbitration, writes, state updates) over any
//! [`RegisterWriteExecutor`], so it is testable without a socket.

use std::sync::Arc;
use std::time::Duration;

use crate::inverter::encoder::RegisterWrite;
use crate::inverter::model::DeviceType;
use crate::inverter::poll::AppState;
use crate::inverter::state_machines::{
    clear_cosy_slot_registers, cosy_slot_register_writes, execute_register_writes,
    persist_cosy_active, DischargeControlArbiter, DischargeControlOwner, RegisterWriteExecutor,
};
use crate::modbus::registers::{
    HR_3PH_AC_CHARGE_ENABLE, HR_3PH_FORCE_CHARGE_ENABLE, HR_3PH_FORCE_DISCHARGE_ENABLE,
    HR_BATTERY_POWER_MODE, HR_ENABLE_CHARGE, HR_ENABLE_CHARGE_TARGET, HR_ENABLE_DISCHARGE,
};
use crate::settings::{find_next_cosy_slot, CosySlot};

/// Gap between register writes to the inverter (real dongles need ~1.5 s).
pub(crate) const COSY_WRITE_GAP: Duration = Duration::from_millis(1500);

/// Everything a cycle's decision depends on.
pub(crate) struct CosyInputs<'a> {
    pub enabled: bool,
    pub slots: &'a [CosySlot],
    pub now_minutes: u16,
    /// A cosy slot is currently being driven (the flag the previous cycles set).
    pub cosy_active: bool,
    /// Index of the slot already preloaded into the inverter, if any.
    pub last_preloaded: Option<usize>,
    /// Timed Export owns the discharge-control registers this cycle.
    pub timed_export_owns_discharge: bool,
    pub device_type: DeviceType,
}

/// What this cycle should do.
#[derive(Debug)]
pub(crate) enum CosyStep {
    /// Nothing to do.
    Idle,
    /// A slot has just started: write its times and enable charging.
    Enter { writes: Vec<RegisterWrite> },
    /// The slot has ended (or Cosy was switched off while active): disable
    /// charging and preload the next slot, or clear the registers.
    Exit {
        writes: Vec<RegisterWrite>,
        /// The slot to remember as preloaded once the exit succeeds.
        next_preloaded: Option<usize>,
    },
    /// Idle, and the next upcoming slot changed: load it with charge disabled.
    Preload {
        slot_index: usize,
        writes: Vec<RegisterWrite>,
    },
    /// Idle with no upcoming slot, but one was previously preloaded: clear it.
    ClearPreload { writes: Vec<RegisterWrite> },
}

fn describe(slot: &CosySlot) -> String {
    format!(
        "{}:{:02}-{}:{:02}",
        slot.start_hour, slot.start_minute, slot.end_hour, slot.end_minute
    )
}

/// The slot containing `now_minutes`. With Cosy disabled this is always `None`,
/// so a lingering `cosy_active` flag is cleared on the next poll (otherwise the
/// inverter stays force-charging after switching away from Cosy mode).
fn current_slot<'a>(i: &CosyInputs<'a>) -> Option<(usize, &'a CosySlot)> {
    if !i.enabled {
        return None;
    }
    i.slots
        .iter()
        .enumerate()
        .find(|(_, s)| s.enabled && s.contains_minutes(i.now_minutes))
}

/// Decide this cycle's step. Pure apart from logging.
pub(crate) fn plan_cosy_step(i: &CosyInputs<'_>) -> CosyStep {
    let current = current_slot(i);
    let in_slot = current.is_some();

    if let (Some((slot_idx, slot)), false) = (current, i.cosy_active) {
        tracing::info!(
            "Cosy: entering slot {} ({}), target SOC {}%",
            slot_idx,
            describe(slot),
            slot.target_soc
        );
        return CosyStep::Enter {
            writes: cosy_slot_register_writes(slot, i.device_type, true),
        };
    }

    if i.cosy_active && !in_slot {
        return plan_exit(i);
    }

    if !in_slot && !i.cosy_active {
        return plan_idle_preload(i);
    }

    // Already in an active cosy slot.
    CosyStep::Idle
}

fn plan_exit(i: &CosyInputs<'_>) -> CosyStep {
    tracing::info!("Cosy: exiting slot, restoring Eco mode");
    // First, disable charge and the charge target.
    let mut writes = vec![
        RegisterWrite {
            address: HR_ENABLE_CHARGE,
            value: 0,
        },
        RegisterWrite {
            address: HR_ENABLE_CHARGE_TARGET,
            value: 0,
        },
    ];
    // Three-phase models also clear their force flags, except the discharge
    // flag, which belongs to the higher-priority Timed Export machine while it
    // owns the current window (issue #289: scheduled Timed Export outranks
    // Timed Charge).
    if i.device_type.uses_three_phase_schedule_slots() {
        writes.push(RegisterWrite {
            address: HR_3PH_FORCE_CHARGE_ENABLE,
            value: 0,
        });
        writes.push(RegisterWrite {
            address: HR_3PH_AC_CHARGE_ENABLE,
            value: 0,
        });
        if !i.timed_export_owns_discharge {
            writes.push(RegisterWrite {
                address: HR_3PH_FORCE_DISCHARGE_ENABLE,
                value: 0,
            });
        }
    }
    // Restore Eco and clear enable_discharge to match CosyExit, but only when
    // Timed Export does not own the discharge-control registers this cycle. A
    // lower-priority automation must not cancel an active export window.
    if !i.timed_export_owns_discharge {
        writes.push(RegisterWrite {
            address: HR_BATTERY_POWER_MODE,
            value: 1,
        });
        writes.push(RegisterWrite {
            address: HR_ENABLE_DISCHARGE,
            value: 0,
        });
    } else {
        tracing::debug!("Cosy: deferring Eco restore — Timed Export owns the current window");
    }

    // Then preload the next slot (charge disabled so the inverter doesn't act on
    // it yet), or clear the registers. Cosy switched off while active: clear.
    let next = if i.enabled {
        find_next_cosy_slot(i.now_minutes, i.slots)
    } else {
        None
    };
    let next_preloaded = next.map(|(idx, _, _)| idx);
    match next {
        Some((idx, slot, minutes_until)) => {
            tracing::info!(
                "Cosy: preloading next slot {} ({}) in {} min",
                idx,
                describe(slot),
                minutes_until
            );
            writes.extend(cosy_slot_register_writes(slot, i.device_type, false));
        }
        None => {
            if i.enabled {
                tracing::info!("Cosy: no upcoming slot - clearing charge slot registers");
            }
            writes.extend(clear_cosy_slot_registers(i.device_type));
        }
    }
    CosyStep::Exit {
        writes,
        next_preloaded,
    }
}

fn plan_idle_preload(i: &CosyInputs<'_>) -> CosyStep {
    if !i.enabled {
        return CosyStep::Idle;
    }
    // Only re-writes when the "next upcoming slot" index changes (e.g. after a
    // slot ends, or on the first poll after connect).
    let next = find_next_cosy_slot(i.now_minutes, i.slots);
    let next_idx = next.map(|(idx, _, _)| idx);
    if next_idx == i.last_preloaded {
        return CosyStep::Idle;
    }
    match next {
        Some((idx, slot, minutes_until)) => {
            tracing::info!(
                "Cosy: preloading next slot {} ({}) in {} min",
                idx,
                describe(slot),
                minutes_until
            );
            CosyStep::Preload {
                slot_index: idx,
                writes: cosy_slot_register_writes(slot, i.device_type, false),
            }
        }
        // No upcoming slot: clear the registers (only reached when one had been
        // preloaded, since `next_idx == None != last_preloaded`).
        None => {
            tracing::info!("Cosy: no upcoming slot - clearing charge slot registers");
            CosyStep::ClearPreload {
                writes: clear_cosy_slot_registers(i.device_type),
            }
        }
    }
}

/// Apply a step: take the discharge-control domain, write, and on success
/// update the shared `cosy_active` flag (persisting it) and the preloaded-slot
/// tracker. A failed write leaves the state untouched so the next cycle retries.
pub(crate) async fn run_cosy_step<W: RegisterWriteExecutor>(
    step: CosyStep,
    state: &Arc<AppState>,
    writer: &mut W,
    arbiter: &mut DischargeControlArbiter,
    last_preloaded: &mut Option<usize>,
    write_gap: Duration,
) {
    match step {
        CosyStep::Idle => {}
        CosyStep::Enter { writes } => {
            if !request_writes(&writes, arbiter) {
                return;
            }
            if execute_register_writes(writer, &writes, "Cosy enter", write_gap).await {
                *state.cosy_active.lock().await = true;
                persist_cosy_active(true);
                // The preloaded slot is now the active one.
                *last_preloaded = None;
            } else {
                tracing::warn!("Cosy: enter writes failed - will retry on next poll");
            }
        }
        CosyStep::Exit {
            writes,
            next_preloaded,
        } => {
            if !arbiter.request(DischargeControlOwner::TimedCharge) {
                return;
            }
            if execute_register_writes(writer, &writes, "Cosy exit", write_gap).await {
                *state.cosy_active.lock().await = false;
                persist_cosy_active(false);
                *last_preloaded = next_preloaded;
            } else {
                tracing::warn!("Cosy: exit writes failed - will retry on next poll");
            }
        }
        CosyStep::Preload { slot_index, writes } => {
            if request_writes(&writes, arbiter)
                && execute_register_writes(writer, &writes, "Cosy preload", write_gap).await
            {
                *last_preloaded = Some(slot_index);
            }
        }
        CosyStep::ClearPreload { writes } => {
            if arbiter.request(DischargeControlOwner::TimedCharge)
                && execute_register_writes(writer, &writes, "Cosy clear", write_gap).await
            {
                *last_preloaded = None;
            }
        }
    }
}

/// Request the discharge-control domain for a batch. An empty batch (a slot
/// whose times could not be encoded) never claims it.
fn request_writes(writes: &[RegisterWrite], arbiter: &mut DischargeControlArbiter) -> bool {
    !writes.is_empty() && arbiter.request(DischargeControlOwner::TimedCharge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::with_isolated_config_dir_async;

    fn slot(sh: u8, sm: u8, eh: u8, em: u8, enabled: bool) -> CosySlot {
        CosySlot {
            enabled,
            start_hour: sh,
            start_minute: sm,
            end_hour: eh,
            end_minute: em,
            target_soc: 90,
        }
    }

    /// The three Cosy windows: 04:00-07:00, 13:00-16:00, 22:00-00:00 (the last
    /// crossing midnight).
    fn cosy_slots() -> Vec<CosySlot> {
        vec![
            slot(4, 0, 7, 0, true),
            slot(13, 0, 16, 0, true),
            slot(22, 0, 0, 0, true),
        ]
    }

    fn at(h: u16, m: u16) -> u16 {
        h * 60 + m
    }

    struct Scenario {
        slots: Vec<CosySlot>,
        enabled: bool,
        now: u16,
        active: bool,
        preloaded: Option<usize>,
        te_owns: bool,
        device: DeviceType,
    }

    impl Scenario {
        fn new(now: u16) -> Self {
            Self {
                slots: cosy_slots(),
                enabled: true,
                now,
                active: false,
                preloaded: None,
                te_owns: false,
                device: DeviceType::Gen2Hybrid,
            }
        }
        fn plan(&self) -> CosyStep {
            plan_cosy_step(&CosyInputs {
                enabled: self.enabled,
                slots: &self.slots,
                now_minutes: self.now,
                cosy_active: self.active,
                last_preloaded: self.preloaded,
                timed_export_owns_discharge: self.te_owns,
                device_type: self.device,
            })
        }
    }

    fn regs(writes: &[RegisterWrite]) -> Vec<(u16, u16)> {
        writes.iter().map(|w| (w.address, w.value)).collect()
    }

    // ---- entering ------------------------------------------------------------

    #[test]
    fn entering_a_slot_writes_its_times_and_enables_charging() {
        let step = Scenario::new(at(4, 30)).plan();
        let CosyStep::Enter { writes } = step else {
            panic!("expected Enter, got {step:?}");
        };
        let regs = regs(&writes);
        assert!(regs.contains(&(94, 400)), "start: {regs:?}");
        assert!(regs.contains(&(95, 700)), "end: {regs:?}");
        assert!(regs.contains(&(96, 1)), "enable charge: {regs:?}");
        assert!(regs.contains(&(20, 1)), "enable charge target: {regs:?}");
    }

    #[test]
    fn the_slot_start_minute_is_inclusive_and_the_end_minute_exclusive() {
        assert!(matches!(
            Scenario::new(at(4, 0)).plan(),
            CosyStep::Enter { .. }
        ));
        // 07:00 is outside 04:00-07:00 and the next slot is hours away.
        assert!(matches!(
            Scenario::new(at(7, 0)).plan(),
            CosyStep::Preload { .. }
        ));
    }

    #[test]
    fn a_slot_crossing_midnight_is_entered_on_both_sides_of_it() {
        for minute in [at(22, 0), at(23, 59), at(0, 0)] {
            let mut s = Scenario::new(minute);
            s.slots = vec![slot(22, 0, 5, 0, true)];
            assert!(
                matches!(s.plan(), CosyStep::Enter { .. }),
                "minute {minute}"
            );
        }
        let mut s = Scenario::new(at(5, 0));
        s.slots = vec![slot(22, 0, 5, 0, true)];
        assert!(
            !matches!(s.plan(), CosyStep::Enter { .. }),
            "05:00 is the end"
        );
    }

    #[test]
    fn a_disabled_slot_is_never_entered() {
        let mut s = Scenario::new(at(4, 30));
        s.slots = vec![slot(4, 0, 7, 0, false)];
        assert!(matches!(s.plan(), CosyStep::Idle));
    }

    #[test]
    fn a_zero_length_slot_is_never_entered() {
        // Review H9: a slot left at 00:00-00:00 must not charge all day.
        let mut s = Scenario::new(at(12, 0));
        s.slots = vec![slot(0, 0, 0, 0, true)];
        assert!(matches!(s.plan(), CosyStep::Idle));
    }

    #[test]
    fn with_cosy_off_a_slot_is_not_entered() {
        let mut s = Scenario::new(at(4, 30));
        s.enabled = false;
        assert!(matches!(s.plan(), CosyStep::Idle));
    }

    #[test]
    fn an_unencodable_slot_yields_an_empty_enter_batch() {
        let mut s = Scenario::new(at(1, 0));
        s.slots = vec![slot(25, 0, 2, 0, true)];
        let step = s.plan();
        let CosyStep::Enter { writes } = step else {
            panic!("expected Enter, got {step:?}");
        };
        assert!(writes.is_empty());
    }

    #[test]
    fn an_active_slot_with_the_clock_still_inside_it_does_nothing() {
        let mut s = Scenario::new(at(5, 0));
        s.active = true;
        assert!(matches!(s.plan(), CosyStep::Idle));
    }

    // ---- exiting -------------------------------------------------------------

    fn exit_of(s: &Scenario) -> (Vec<(u16, u16)>, Option<usize>) {
        match s.plan() {
            CosyStep::Exit {
                writes,
                next_preloaded,
            } => (regs(&writes), next_preloaded),
            other => panic!("expected Exit, got {other:?}"),
        }
    }

    #[test]
    fn leaving_a_slot_disables_charging_restores_eco_and_preloads_the_next_slot() {
        let mut s = Scenario::new(at(8, 0));
        s.active = true;
        let (writes, next) = exit_of(&s);
        assert!(writes.contains(&(96, 0)), "enable charge off: {writes:?}");
        assert!(writes.contains(&(20, 0)), "charge target off: {writes:?}");
        assert!(writes.contains(&(27, 1)), "Eco: {writes:?}");
        assert!(writes.contains(&(59, 0)), "discharge off: {writes:?}");
        assert!(writes.contains(&(94, 1300)) && writes.contains(&(95, 1600)));
        assert!(
            !writes.contains(&(96, 1)),
            "the preloaded slot must not enable charging: {writes:?}"
        );
        assert_eq!(next, Some(1));
    }

    #[test]
    fn exit_orders_the_disables_before_the_preload() {
        let mut s = Scenario::new(at(8, 0));
        s.active = true;
        let (writes, _) = exit_of(&s);
        let pos = |r: (u16, u16)| writes.iter().position(|w| *w == r).unwrap();
        assert!(pos((96, 0)) < pos((94, 1300)), "{writes:?}");
    }

    #[test]
    fn timed_export_keeps_the_eco_restore_and_discharge_flag_to_itself() {
        let mut s = Scenario::new(at(8, 0));
        s.active = true;
        s.te_owns = true;
        let (writes, _) = exit_of(&s);
        assert!(writes.contains(&(96, 0)), "charge is still disabled");
        assert!(
            !writes.contains(&(27, 1)) && !writes.contains(&(59, 0)),
            "must not cancel an active export window: {writes:?}"
        );
    }

    #[test]
    fn three_phase_exit_clears_the_force_flags() {
        let mut s = Scenario::new(at(8, 0));
        s.active = true;
        s.device = DeviceType::ThreePhase;
        let (writes, _) = exit_of(&s);
        assert!(writes.contains(&(1123, 0)), "force charge: {writes:?}");
        assert!(writes.contains(&(1112, 0)), "AC charge: {writes:?}");
        assert!(writes.contains(&(1122, 0)), "force discharge: {writes:?}");
        assert!(
            writes.contains(&(1113, 1300)),
            "three-phase slot regs: {writes:?}"
        );
    }

    #[test]
    fn three_phase_exit_leaves_the_discharge_flag_when_timed_export_owns_it() {
        let mut s = Scenario::new(at(8, 0));
        s.active = true;
        s.device = DeviceType::ThreePhase;
        s.te_owns = true;
        let (writes, _) = exit_of(&s);
        assert!(writes.contains(&(1123, 0)) && writes.contains(&(1112, 0)));
        assert!(!writes.contains(&(1122, 0)), "{writes:?}");
    }

    #[test]
    fn exit_with_no_upcoming_slot_clears_the_charge_slot_registers() {
        let mut s = Scenario::new(at(8, 0));
        s.active = true;
        s.slots = vec![slot(4, 0, 7, 0, false)];
        let (writes, next) = exit_of(&s);
        assert!(
            writes.contains(&(94, 0)) && writes.contains(&(95, 0)),
            "{writes:?}"
        );
        assert_eq!(next, None);
    }

    #[test]
    fn switching_cosy_off_while_active_exits_and_clears_rather_than_preloads() {
        let mut s = Scenario::new(at(8, 0));
        s.active = true;
        s.enabled = false;
        let (writes, next) = exit_of(&s);
        assert!(writes.contains(&(96, 0)));
        assert!(writes.contains(&(94, 0)) && writes.contains(&(95, 0)));
        assert!(
            !writes.contains(&(94, 1300)),
            "must not preload: {writes:?}"
        );
        assert_eq!(next, None);
    }

    #[test]
    fn switching_cosy_off_mid_slot_still_exits() {
        // The clock is inside the slot, but disabled Cosy counts as "no slot".
        let mut s = Scenario::new(at(5, 0));
        s.active = true;
        s.enabled = false;
        assert!(matches!(s.plan(), CosyStep::Exit { .. }));
    }

    // ---- idle preload -----------------------------------------------------------

    #[test]
    fn idle_preloads_the_next_slot_once() {
        let mut s = Scenario::new(at(8, 0));
        let step = s.plan();
        let CosyStep::Preload { slot_index, writes } = step else {
            panic!("expected Preload, got {step:?}");
        };
        assert_eq!(slot_index, 1);
        let writes = regs(&writes);
        assert!(writes.contains(&(94, 1300)) && writes.contains(&(95, 1600)));
        assert!(!writes.contains(&(96, 1)), "charge stays off: {writes:?}");

        // Once preloaded, the same next slot is not rewritten.
        s.preloaded = Some(1);
        assert!(matches!(s.plan(), CosyStep::Idle));
    }

    #[test]
    fn idle_repreloads_when_the_next_slot_changes() {
        let mut s = Scenario::new(at(8, 0));
        s.preloaded = Some(0); // the morning slot was loaded; now 13:00 is next
        assert!(matches!(s.plan(), CosyStep::Preload { slot_index: 1, .. }));
    }

    #[test]
    fn the_next_slot_wraps_past_midnight() {
        // 23:59 is in the 22:00-00:00 slot; at 00:00 the next one is 04:00.
        let s = Scenario::new(at(0, 0));
        assert!(matches!(s.plan(), CosyStep::Preload { slot_index: 0, .. }));
    }

    #[test]
    fn idle_does_nothing_when_cosy_is_off() {
        let mut s = Scenario::new(at(8, 0));
        s.enabled = false;
        assert!(matches!(s.plan(), CosyStep::Idle));
    }

    #[test]
    fn idle_with_no_slots_and_nothing_preloaded_does_nothing() {
        let mut s = Scenario::new(at(8, 0));
        s.slots = Vec::new();
        assert!(matches!(s.plan(), CosyStep::Idle));
    }

    #[test]
    fn idle_clears_a_preloaded_slot_that_no_longer_exists() {
        let mut s = Scenario::new(at(8, 0));
        s.slots = vec![slot(4, 0, 7, 0, false)];
        s.preloaded = Some(1);
        let step = s.plan();
        let CosyStep::ClearPreload { writes } = step else {
            panic!("expected ClearPreload, got {step:?}");
        };
        let writes = regs(&writes);
        assert!(writes.contains(&(94, 0)) && writes.contains(&(95, 0)));
    }

    // ---- run_cosy_step ------------------------------------------------------------

    /// Records every write; fails the write at index `fail_at`, if set.
    #[derive(Default)]
    struct Recorder {
        sent: Vec<(u16, u16)>,
        fail_at: Option<usize>,
    }

    impl RegisterWriteExecutor for Recorder {
        async fn write_register(&mut self, write: &RegisterWrite) -> Result<(), String> {
            if self.fail_at == Some(self.sent.len()) {
                return Err("dongle busy".into());
            }
            self.sent.push((write.address, write.value));
            Ok(())
        }
    }

    struct Rig {
        state: Arc<AppState>,
        writer: Recorder,
        arbiter: DischargeControlArbiter,
        preloaded: Option<usize>,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                state: Arc::new(AppState::new()),
                writer: Recorder::default(),
                arbiter: DischargeControlArbiter::default(),
                preloaded: None,
            }
        }
        async fn run(&mut self, step: CosyStep) {
            run_cosy_step(
                step,
                &self.state,
                &mut self.writer,
                &mut self.arbiter,
                &mut self.preloaded,
                Duration::ZERO,
            )
            .await;
        }
        async fn active(&self) -> bool {
            *self.state.cosy_active.lock().await
        }
        /// The planner's step for `minute`, given the state so far.
        async fn plan_at(&self, minute: u16) -> CosyStep {
            let mut s = Scenario::new(minute);
            s.active = self.active().await;
            s.preloaded = self.preloaded;
            s.plan()
        }
    }

    fn enter_step() -> CosyStep {
        Scenario::new(at(4, 30)).plan()
    }

    fn exit_step() -> CosyStep {
        let mut s = Scenario::new(at(8, 0));
        s.active = true;
        s.plan()
    }

    #[tokio::test]
    async fn a_successful_enter_marks_cosy_active_and_persists_it() {
        with_isolated_config_dir_async(|| async {
            let mut rig = Rig::new();
            rig.preloaded = Some(0);
            rig.run(enter_step()).await;
            assert!(rig.active().await);
            assert!(
                crate::settings::Settings::load().cosy_active_persisted,
                "must survive a restart"
            );
            assert_eq!(
                rig.preloaded, None,
                "the preloaded slot is now the active one"
            );
            assert!(rig.writer.sent.contains(&(96, 1)));
            assert_eq!(
                rig.arbiter.selected_owner(),
                Some(DischargeControlOwner::TimedCharge)
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_failed_enter_changes_no_state_so_the_next_poll_retries() {
        with_isolated_config_dir_async(|| async {
            let mut rig = Rig::new();
            rig.preloaded = Some(0);
            rig.writer.fail_at = Some(2);
            rig.run(enter_step()).await;
            assert!(!rig.active().await);
            assert!(!crate::settings::Settings::load().cosy_active_persisted);
            assert_eq!(rig.preloaded, Some(0));
            assert_eq!(rig.writer.sent.len(), 2, "stops at the first failure");
        })
        .await;
    }

    #[tokio::test]
    async fn an_unencodable_slot_neither_writes_nor_claims_the_domain() {
        with_isolated_config_dir_async(|| async {
            let mut rig = Rig::new();
            let mut s = Scenario::new(at(1, 0));
            s.slots = vec![slot(25, 0, 2, 0, true)];
            rig.run(s.plan()).await;
            assert!(rig.writer.sent.is_empty());
            assert!(!rig.active().await);
            assert_eq!(rig.arbiter.selected_owner(), None);
        })
        .await;
    }

    #[tokio::test]
    async fn a_higher_priority_owner_blocks_every_kind_of_cosy_write() {
        with_isolated_config_dir_async(|| async {
            for step in [enter_step(), exit_step()] {
                let mut rig = Rig::new();
                rig.arbiter.request(DischargeControlOwner::Safety);
                rig.run(step).await;
                assert!(rig.writer.sent.is_empty());
                assert!(!rig.active().await);
            }
            let mut rig = Rig::new();
            rig.arbiter.request(DischargeControlOwner::Safety);
            rig.run(Scenario::new(at(8, 0)).plan()).await; // Preload
            assert!(rig.writer.sent.is_empty());
            assert_eq!(rig.preloaded, None);
        })
        .await;
    }

    #[tokio::test]
    async fn a_successful_exit_clears_the_flag_and_records_the_next_preload() {
        with_isolated_config_dir_async(|| async {
            let mut rig = Rig::new();
            *rig.state.cosy_active.lock().await = true;
            crate::inverter::state_machines::persist_cosy_active(true);
            rig.run(exit_step()).await;
            assert!(!rig.active().await);
            assert!(!crate::settings::Settings::load().cosy_active_persisted);
            assert_eq!(rig.preloaded, Some(1));
            assert!(rig.writer.sent.contains(&(96, 0)));
        })
        .await;
    }

    #[tokio::test]
    async fn a_failed_exit_leaves_cosy_active_so_the_next_poll_retries() {
        with_isolated_config_dir_async(|| async {
            let mut rig = Rig::new();
            *rig.state.cosy_active.lock().await = true;
            rig.preloaded = Some(0);
            rig.writer.fail_at = Some(1);
            rig.run(exit_step()).await;
            assert!(rig.active().await, "must still be flagged active");
            assert_eq!(rig.preloaded, Some(0));
        })
        .await;
    }

    #[tokio::test]
    async fn a_preload_records_the_slot_only_once_the_writes_land() {
        with_isolated_config_dir_async(|| async {
            let mut ok = Rig::new();
            ok.run(Scenario::new(at(8, 0)).plan()).await;
            assert_eq!(ok.preloaded, Some(1));
            assert!(!ok.active().await, "a preload never activates charging");

            let mut failed = Rig::new();
            failed.writer.fail_at = Some(0);
            failed.run(Scenario::new(at(8, 0)).plan()).await;
            assert_eq!(failed.preloaded, None);
        })
        .await;
    }

    #[tokio::test]
    async fn clearing_a_preload_forgets_it_only_once_the_writes_land() {
        with_isolated_config_dir_async(|| async {
            let step = || {
                let mut s = Scenario::new(at(8, 0));
                s.slots = vec![slot(4, 0, 7, 0, false)];
                s.preloaded = Some(1);
                s.plan()
            };
            let mut ok = Rig::new();
            ok.preloaded = Some(1);
            ok.run(step()).await;
            assert_eq!(ok.preloaded, None);

            let mut failed = Rig::new();
            failed.preloaded = Some(1);
            failed.writer.fail_at = Some(0);
            failed.run(step()).await;
            assert_eq!(failed.preloaded, Some(1));
        })
        .await;
    }

    #[tokio::test]
    async fn an_idle_step_touches_nothing() {
        with_isolated_config_dir_async(|| async {
            let mut rig = Rig::new();
            rig.preloaded = Some(1);
            rig.run(CosyStep::Idle).await;
            assert!(rig.writer.sent.is_empty());
            assert_eq!(rig.preloaded, Some(1));
            assert_eq!(rig.arbiter.selected_owner(), None);
        })
        .await;
    }

    #[tokio::test]
    async fn a_full_day_walks_through_preload_enter_exit_and_preload() {
        // Drive the planner and applier together across the day, feeding the
        // state each step produces into the next, as the poll loop does.
        with_isolated_config_dir_async(|| async {
            let mut rig = Rig::new();
            // 03:00: the morning slot gets preloaded.
            let step = rig.plan_at(at(3, 0)).await;
            rig.run(step).await;
            assert_eq!(rig.preloaded, Some(0));
            // 03:01: nothing more to do.
            let step = rig.plan_at(at(3, 1)).await;
            assert!(matches!(step, CosyStep::Idle));
            // 04:00: the slot starts.
            let step = rig.plan_at(at(4, 0)).await;
            rig.run(step).await;
            assert!(rig.active().await);
            // 05:00: inside the slot, nothing to do.
            let step = rig.plan_at(at(5, 0)).await;
            assert!(matches!(step, CosyStep::Idle));
            // 07:00: the slot ends and the 13:00 slot is preloaded.
            let step = rig.plan_at(at(7, 0)).await;
            rig.run(step).await;
            assert!(!rig.active().await);
            assert_eq!(rig.preloaded, Some(1));
            // 07:01: settled.
            let step = rig.plan_at(at(7, 1)).await;
            assert!(matches!(step, CosyStep::Idle));
        })
        .await;
    }
}
