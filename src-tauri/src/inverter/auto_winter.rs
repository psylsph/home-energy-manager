//! Auto Winter Mode: keep a cold battery warm with short charge bursts
//! (issue #515).
//!
//! GivEnergy's batteries have no heater; warmth only comes from charging and
//! discharging, and the BMS refuses to charge below 0 °C. When the battery
//! temperature falls below the Cold Threshold this runs a 30 minute charge,
//! then another, until the temperature has stayed above the Recovery
//! Threshold. It never touches the charge target: the standard target is the
//! cap, so a burst stops where the user's own charging would.
//!
//! A burst is charge slot 1 set to now → now + 30 minutes (the inverter only
//! charges inside a slot, and stops by itself when the window ends), with the
//! charge flag on. Slot 1 is borrowed rather than a spare slot because
//! AC-coupled, Gen 1 and Gen 2 inverters have only the one. What it replaced
//! is saved first and put back when the mode finishes.
//!
//! Invariants:
//! - The slot-1 baseline is on disk before the first write, and lives until
//!   the slot and the charge flag read as put back, the user has changed the
//!   slot, or another owner has taken it over with the baseline.
//! - The user wins: a slot 1 that reads as neither HEM's window nor the saved
//!   baseline, on two reads from different frames, was changed by someone
//!   else (the user, the Planner, Cosy). The baseline is dropped, never
//!   written over it, and their setting is what a later burst puts back. A
//!   read mixing the window and the baseline is a write that half landed,
//!   and is sent again.
//! - An owner that takes slot 1 over (Force Charge, the calibration cycle, a
//!   smart-charge top-up) captures the baseline, not HEM's window
//!   ([`with_baseline_slot`]), and the burst is handed to it.
//! - Never charges while another owner holds the battery (Force Charge, a
//!   safety limiter, Adaptive Charge, Cosy/Agile charging, a calibration),
//!   while a charge slot of the user's is already running, in anything but
//!   plain Eco, on a three-phase inverter, or once the battery is at the
//!   level slot 1 stops at.
//! - A single odd temperature reading can't start or stop it: it takes
//!   `debounce_readings` in a row on the same side of a threshold.
//! - A restore that fails is retried each poll, because the baseline stays
//!   until the slot reads as put back. A graceful shutdown puts it back too.

use serde::{Deserialize, Serialize};

use crate::inverter::encoder::{ControlCommand, RegisterWrite};
use crate::inverter::model::{DeviceType, InverterSnapshot, ScheduleSlot};
use crate::inverter::poll::AppState;
use crate::modbus::registers::{HR_CHARGE_TARGET_SOC, HR_ENABLE_CHARGE_TARGET};
use crate::server::api::charge_slot_command_for_device;

/// How long one charge burst runs.
pub const BURST_MINUTES: i64 = 30;
/// Times a burst's window is re-sent when it doesn't read back, before the
/// attempt is given up.
const RESEND_LIMIT: u8 = 3;
/// How long to wait after giving a burst up before trying another.
const RETRY_AFTER_SECS: i64 = 5 * 60;
/// Restore attempts for a pre-#515 saved target before it is dropped.
const LEGACY_ATTEMPTS: u8 = 3;
/// The Cold Threshold slider's range (°C). Above 0 °C because the BMS
/// refuses to charge below it.
pub const COLD_RANGE: (f32, f32) = (2.0, 10.0);
/// The Recovery Threshold slider's range (°C).
pub const RECOVERY_RANGE: (f32, f32) = (3.0, 10.0);
/// The defaults before issue #515, when the mode raised the charge target.
/// With bursts they would grid-charge all day below 8 °C.
const OLD_DEFAULTS: (f32, f32) = (8.0, 12.0);
/// How long shutdown waits for slot 1 to be put back.
pub const SHUTDOWN_RESTORE_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Thresholds saved by an older version, brought into the slider ranges with
/// recovery above cold. The old defaults, which nobody chose, become the new
/// ones; a value the user picked is kept as near as the ranges allow.
pub fn normalise_thresholds(cold: f32, recovery: f32) -> (f32, f32) {
    if !cold.is_finite() || !recovery.is_finite() || (cold, recovery) == OLD_DEFAULTS {
        return (COLD_RANGE.0, RECOVERY_RANGE.0);
    }
    let cold = cold.clamp(COLD_RANGE.0, COLD_RANGE.1);
    let recovery = recovery.clamp(RECOVERY_RANGE.0, RECOVERY_RANGE.1);
    if recovery > cold {
        (cold, recovery)
    } else {
        // Recovery is at least 3, so this stays in range.
        (recovery - 1.0, recovery)
    }
}

/// Configuration for Auto Winter Mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoWinterConfig {
    /// Master toggle - must be on for automatic winter mode to function.
    pub enabled: bool,
    /// Temperature below which charge bursts start (°C).
    pub cold_threshold: f32,
    /// Temperature at or above which they stop (°C).
    pub recovery_threshold: f32,
    /// Number of consecutive cold/warm readings before the state transitions.
    pub debounce_readings: u32,
}

impl Default for AutoWinterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cold_threshold: 2.0,
            recovery_threshold: 3.0,
            debounce_readings: 10,
        }
    }
}

/// State machine for temperature-triggered auto winter mode.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum AutoWinterState {
    /// Awaiting cold temperatures.
    #[default]
    Idle,
    /// Temperature below the Cold Threshold, counting towards debounce.
    ColdPending {
        /// Consecutive polls where temp was below threshold.
        consecutive: u32,
    },
    /// Cold: charge bursts run, one after another, until it has warmed.
    Warming,
    /// Temperature at or above the Recovery Threshold, counting towards stopping.
    WarmPending {
        /// Consecutive polls where temp was at or above the threshold.
        consecutive: u32,
    },
}

/// What charge slot 1 and the charge flag were before the first burst.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoWinterBurst {
    /// Inverter serial the baseline was captured from.
    pub serial: String,
    /// Slot 1 before the first burst, as HHMM (0 and 0 when it was empty).
    pub baseline_start: u16,
    pub baseline_end: u16,
    /// The charge flag (HR 96) before the first burst.
    pub baseline_enable_charge: bool,
    /// The window now on the inverter, as HHMM.
    pub start: u16,
    pub end: u16,
    /// Host clock (Unix seconds) when the window ends.
    pub ends_at: i64,
    /// Times this window has been re-sent for not reading back.
    #[serde(default)]
    pub resends: u8,
    /// The window this one chained from, as HHMM: until the new window reads
    /// back, slot 1 still holds it, and it is HEM's, not someone else's.
    #[serde(default)]
    pub previous: Option<(u16, u16)>,
}

/// A target saved by Auto Winter Mode before issue #515, when it raised the
/// charge target and put the user's back on recovery. A run that was active
/// at upgrade still has to put it back, once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoWinterSaved {
    pub enable_charge_target: bool,
    pub target_soc: u8,
}

/// Everything the mode keeps between polls, under one lock.
#[derive(Debug, Clone, Default)]
pub struct AutoWinterRun {
    /// The burst on the inverter, with the baseline to put back.
    pub burst: Option<AutoWinterBurst>,
    /// A pre-#515 target still to be put back.
    pub legacy: Option<AutoWinterSaved>,
    pub legacy_attempts: u8,
    /// Host clock before which no new burst starts (after one was given up).
    pub retry_after: i64,
    /// Why no burst is running while it is cold, for the log.
    pub waiting: Option<&'static str>,
    /// A read of slot 1 as someone else's, waiting for a second read from a
    /// different frame before the baseline is dropped.
    pub changed_read: Option<ChangedRead>,
    /// Set on shutdown: nothing more is written.
    pub stopped: bool,
}

/// A slot-1 read that matched neither the window nor the baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedRead {
    slot: (u16, u16),
    inverter_time: String,
}

impl ChangedRead {
    fn of(snap: &InverterSnapshot) -> Self {
        Self {
            slot: slot_one(snap),
            inverter_time: snap.inverter_time.clone(),
        }
    }

    /// The same slot, read from a different frame. A dongle repeating a
    /// frozen frame repeats the inverter's clock too; with no clock decoded
    /// the slot alone counts (as `RepairReadMark`, issue #501).
    fn confirms(&self, later: &Self) -> bool {
        self.slot == later.slot
            && (self.inverter_time.is_empty() || self.inverter_time != later.inverter_time)
    }
}

/// What charge slot 1 and the charge flag were before the first burst, for an
/// owner that takes slot 1 over while a burst holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotOneBaseline {
    pub start: u16,
    pub end: u16,
    pub enable_charge: bool,
}

impl SlotOneBaseline {
    /// `snap` with slot 1 and the charge flag read as the baseline.
    pub fn apply(&self, snap: &InverterSnapshot) -> InverterSnapshot {
        let mut snap = snap.clone();
        let configured = (self.start, self.end) != (0, 0);
        let slot = &mut snap.charge_slots[0];
        slot.enabled = configured;
        slot.start_hour = (self.start / 100) as u8;
        slot.start_minute = (self.start % 100) as u8;
        slot.end_hour = (self.end / 100) as u8;
        slot.end_minute = (self.end % 100) as u8;
        snap.enable_charge = self.enable_charge;
        snap
    }
}

/// The baseline under a burst that holds slot 1 now (its window, or part of
/// it, is on the inverter), else `None` and the reading stands.
pub fn baseline_under_burst(
    run: &AutoWinterRun,
    snap: &InverterSnapshot,
) -> Option<SlotOneBaseline> {
    let burst = run.burst.as_ref()?;
    if snap.inverter_serial.is_empty() || burst.serial != snap.inverter_serial {
        return None;
    }
    match slot_reads(snap, burst) {
        SlotReads::Window | SlotReads::Partial => Some(SlotOneBaseline {
            start: burst.baseline_start,
            end: burst.baseline_end,
            enable_charge: burst.baseline_enable_charge,
        }),
        SlotReads::Baseline | SlotReads::Changed => None,
    }
}

/// `snap` with slot 1 and the charge flag read as what a burst replaced, for
/// the baselines other owners capture (Force Charge, the calibration cycle,
/// a smart-charge top-up), so they never put HEM's window back.
pub fn with_baseline_slot(run: &AutoWinterRun, snap: &InverterSnapshot) -> InverterSnapshot {
    match baseline_under_burst(run, snap) {
        Some(baseline) => baseline.apply(snap),
        None => snap.clone(),
    }
}

/// Another owner of the battery that a burst must not run over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoWinterBlocker {
    Calibration,
    ManualForce,
    Safety,
    Adaptive,
    TariffAutomation,
    DispatchTopUp,
}

impl AutoWinterBlocker {
    /// An owner that writes charge slot 1 itself, having captured the
    /// baseline through [`with_baseline_slot`]: a burst is handed to it.
    fn takes_slot_one(self) -> bool {
        matches!(
            self,
            Self::Calibration | Self::ManualForce | Self::DispatchTopUp
        )
    }

    fn code(self) -> &'static str {
        match self {
            Self::Calibration => "a calibration is running",
            Self::ManualForce => "Force Charge is running",
            Self::Safety => "a safety limiter holds the battery",
            Self::Adaptive => "Adaptive Charge is charging",
            Self::TariffAutomation => "Cosy or Agile is charging",
            Self::DispatchTopUp => "a smart-charge top-up is running",
        }
    }
}

/// What the poll loop knows this poll.
#[derive(Debug, Clone, Copy)]
pub struct AutoWinterFacts {
    /// Host clock, Unix seconds.
    pub now: i64,
    /// The inverter's clock, as a minute of the day.
    pub inverter_minute: u16,
    pub blocker: Option<AutoWinterBlocker>,
}

/// One poll's result: register writes to send. A change to
/// `AutoWinterRun::burst` must be saved before they go out.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AutoWinterStep {
    pub writes: Vec<RegisterWrite>,
}

fn hhmm(minute_of_day: i64) -> u16 {
    let minute = minute_of_day.rem_euclid(1440) as u16;
    (minute / 60) * 100 + minute % 60
}

fn slot_hhmm(slot: &ScheduleSlot) -> (u16, u16) {
    if !slot.is_configured() {
        return (0, 0);
    }
    (
        u16::from(slot.start_hour) * 100 + u16::from(slot.start_minute),
        u16::from(slot.end_hour) * 100 + u16::from(slot.end_minute),
    )
}

fn slot_one(snap: &InverterSnapshot) -> (u16, u16) {
    slot_hhmm(&snap.charge_slots[0])
}

fn slot_writes(device: DeviceType, start: u16, end: u16) -> Vec<RegisterWrite> {
    let enabled = (start, end) != (0, 0);
    charge_slot_command_for_device(device, 1, enabled, start, end)
        .and_then(|command| command.encode())
        .unwrap_or_default()
}

fn enable_charge_write(enabled: bool) -> Vec<RegisterWrite> {
    ControlCommand::SetEnableCharge { enabled }
        .encode()
        .unwrap_or_default()
}

/// The writes that put slot 1 and the charge flag back.
fn restore_writes(snap: &InverterSnapshot, burst: &AutoWinterBurst) -> Vec<RegisterWrite> {
    let mut writes = slot_writes(snap.device_type, burst.baseline_start, burst.baseline_end);
    if snap.enable_charge != burst.baseline_enable_charge {
        writes.extend(enable_charge_write(burst.baseline_enable_charge));
    }
    writes
}

/// The level a burst in slot 1 charges to. On 10-slot inverters the firmware
/// stops at the lower of the global target and slot 1's own (HR 242; 0 means
/// none is set).
fn slot_one_stops_at(snap: &InverterSnapshot) -> u8 {
    let own = snap
        .device_type
        .uses_extended_schedule_slots()
        .then_some(snap.raw_charge_slot_1_target_soc)
        .flatten()
        .filter(|raw| (4..=100).contains(raw))
        .map(|raw| raw as u8);
    own.map_or(snap.target_soc, |own| own.min(snap.target_soc))
}

/// Whether a charge burst may charge the battery now.
fn blocked(
    snap: &InverterSnapshot,
    run: &AutoWinterRun,
    facts: &AutoWinterFacts,
) -> Option<&'static str> {
    if let Some(blocker) = facts.blocker {
        return Some(blocker.code());
    }
    // The baseline is only ever written back to the inverter it came from.
    if snap.inverter_serial.is_empty() {
        return Some("the inverter hasn't reported its identity yet");
    }
    // Three-phase arms charging through HR 1112/1123, not the HR 96 a burst
    // writes; until that is built and seen working, they get no bursts.
    if snap.device_type.uses_three_phase_schedule_slots() {
        return Some("three-phase inverters aren't supported yet");
    }
    if snap.battery_power_mode != 1 || snap.enable_discharge {
        return Some("the inverter isn't in plain Eco");
    }
    if snap.soc >= slot_one_stops_at(snap) {
        return Some("the battery is at its charge target");
    }
    // A running slot is charging the battery already. Not while a burst is
    // on the inverter: the running slot is then HEM's own.
    if run.burst.is_none()
        && crate::inverter::state_machines::charge_slot_running(snap, facts.inverter_minute)
    {
        return Some("a charge slot is already charging the battery");
    }
    if facts.now < run.retry_after {
        return Some("the last burst didn't land");
    }
    None
}

/// The next window and the baseline to put back, for a burst starting now.
fn next_burst(
    snap: &InverterSnapshot,
    run: &AutoWinterRun,
    facts: &AutoWinterFacts,
) -> AutoWinterBurst {
    let start_minute = i64::from(facts.inverter_minute);
    let previous = run.burst.as_ref().map(|burst| (burst.start, burst.end));
    let (baseline_start, baseline_end, baseline_enable_charge) = match &run.burst {
        // Chained: the baseline is still the user's, not the last window.
        Some(burst) => (
            burst.baseline_start,
            burst.baseline_end,
            burst.baseline_enable_charge,
        ),
        None => {
            let (start, end) = slot_one(snap);
            (start, end, snap.enable_charge)
        }
    };
    AutoWinterBurst {
        serial: snap.inverter_serial.clone(),
        baseline_start,
        baseline_end,
        baseline_enable_charge,
        start: hhmm(start_minute),
        end: hhmm(start_minute + BURST_MINUTES),
        ends_at: facts.now + BURST_MINUTES * 60,
        resends: 0,
        previous,
    }
}

fn burst_writes(snap: &InverterSnapshot, burst: &AutoWinterBurst) -> Vec<RegisterWrite> {
    // Slot first, then the flag: the inverter has a window before it is told
    // to charge.
    let mut writes = slot_writes(snap.device_type, burst.start, burst.end);
    if !snap.enable_charge {
        writes.extend(enable_charge_write(true));
    }
    writes
}

/// Where slot 1 stands against the burst on the inverter.
#[derive(Debug, PartialEq, Eq)]
enum SlotReads {
    /// Reads as the window HEM wrote.
    Window,
    /// Reads as it was before the first burst.
    Baseline,
    /// One register reads as the window and the other as the baseline: a
    /// write that half landed.
    Partial,
    /// Reads as neither: someone else changed it.
    Changed,
}

fn slot_reads(snap: &InverterSnapshot, burst: &AutoWinterBurst) -> SlotReads {
    let read = slot_one(snap);
    let window = (burst.start, burst.end);
    let baseline = (burst.baseline_start, burst.baseline_end);
    // HEM's own values for each register: this window, the one it chained
    // from (still there until the new one reads back), and the baseline.
    let ours = |value: u16, pick: fn((u16, u16)) -> u16| {
        value == pick(window)
            || value == pick(baseline)
            || burst
                .previous
                .is_some_and(|previous| value == pick(previous))
    };
    if read == window {
        SlotReads::Window
    } else if read == baseline {
        SlotReads::Baseline
    } else if ours(read.0, |pair| pair.0) && ours(read.1, |pair| pair.1) {
        // HR 94 and HR 95 go out one at a time, and a batch stops at the
        // first failed write: the new window half (or not yet) landed.
        SlotReads::Partial
    } else {
        SlotReads::Changed
    }
}

fn note_waiting(run: &mut AutoWinterRun, reason: Option<&'static str>) {
    if run.waiting != reason {
        match reason {
            Some(why) => tracing::info!("Auto winter: not charging, {why}"),
            None => tracing::info!("Auto winter: charging again"),
        }
        run.waiting = reason;
    }
}

/// Put a pre-#515 saved target back, once. Returns the writes for this poll.
fn legacy_step(snap: &InverterSnapshot, run: &mut AutoWinterRun) -> Option<AutoWinterStep> {
    let saved = run.legacy.clone()?;
    if snap.enable_charge_target == saved.enable_charge_target
        && snap.target_soc == saved.target_soc
    {
        tracing::info!("Auto winter: the charge target from before the update is back");
        run.legacy = None;
        return Some(AutoWinterStep::default());
    }
    if run.legacy_attempts >= LEGACY_ATTEMPTS {
        tracing::warn!(
            "Auto winter: couldn't put the charge target back to {}% (target flag {}); check it on the Control page",
            saved.target_soc,
            saved.enable_charge_target
        );
        run.legacy = None;
        return Some(AutoWinterStep::default());
    }
    run.legacy_attempts += 1;
    tracing::info!(
        "Auto winter: putting the charge target from before the update back ({}%)",
        saved.target_soc
    );
    Some(AutoWinterStep {
        writes: vec![
            RegisterWrite {
                address: HR_ENABLE_CHARGE_TARGET,
                value: u16::from(saved.enable_charge_target),
            },
            RegisterWrite {
                address: HR_CHARGE_TARGET_SOC,
                value: u16::from(saved.target_soc),
            },
        ],
    })
}

/// Advance the temperature state machine by one reading.
fn advance_state(temp: f32, config: &AutoWinterConfig, state: &mut AutoWinterState) {
    if !temp.is_finite() {
        return;
    }
    match state {
        AutoWinterState::Idle => {
            if temp < config.cold_threshold {
                tracing::info!(
                    temp,
                    cold = config.cold_threshold,
                    "Auto winter: battery cold - counting"
                );
                *state = AutoWinterState::ColdPending { consecutive: 1 };
            }
        }
        AutoWinterState::ColdPending { consecutive } => {
            if temp < config.cold_threshold {
                *consecutive += 1;
                if *consecutive >= config.debounce_readings {
                    tracing::info!(temp, "Auto winter: battery cold - starting charge bursts");
                    *state = AutoWinterState::Warming;
                }
            } else {
                *state = AutoWinterState::Idle;
            }
        }
        AutoWinterState::Warming => {
            if temp >= config.recovery_threshold {
                *state = AutoWinterState::WarmPending { consecutive: 1 };
            }
        }
        AutoWinterState::WarmPending { consecutive } => {
            if temp >= config.recovery_threshold {
                *consecutive += 1;
                if *consecutive >= config.debounce_readings {
                    tracing::info!(temp, "Auto winter: battery warm - stopping");
                    *state = AutoWinterState::Idle;
                }
            } else {
                *state = AutoWinterState::Warming;
            }
        }
    }
}

/// Evaluate Auto Winter Mode for one poll.
pub(crate) fn step(
    snap: &InverterSnapshot,
    config: &AutoWinterConfig,
    state: &mut AutoWinterState,
    run: &mut AutoWinterRun,
    facts: &AutoWinterFacts,
) -> AutoWinterStep {
    if run.stopped {
        return AutoWinterStep::default();
    }
    if let Some(done) = legacy_step(snap, run) {
        return done;
    }

    if config.enabled {
        advance_state(snap.battery_temperature, config, state);
    } else {
        *state = AutoWinterState::Idle;
    }
    let warming = matches!(
        state,
        AutoWinterState::Warming | AutoWinterState::WarmPending { .. }
    );

    // What is on the inverter from an earlier poll, or an earlier run.
    if let Some(burst) = run.burst.clone() {
        if snap.inverter_serial.is_empty() {
            // No identity yet (the first poll after a restart): not another
            // inverter, so keep the baseline and wait.
            return AutoWinterStep::default();
        }
        if burst.serial != snap.inverter_serial {
            // The baseline belongs to another inverter: never write it here.
            tracing::warn!(
                "Auto winter: the saved charge slot is from another inverter, dropping it"
            );
            run.burst = None;
            run.changed_read = None;
            return AutoWinterStep::default();
        }
        if let Some(owner) = facts.blocker {
            if hand_over(run, owner) {
                return AutoWinterStep::default();
            }
        }
        let reads = slot_reads(snap, &burst);
        if reads != SlotReads::Changed {
            run.changed_read = None;
        }
        let window_live = warming && facts.now < burst.ends_at;
        match reads {
            SlotReads::Changed => {
                let read = ChangedRead::of(snap);
                let confirmed = run
                    .changed_read
                    .as_ref()
                    .is_some_and(|earlier| earlier.confirms(&read));
                if !confirmed {
                    // One read may be a bad frame: wait for a second.
                    run.changed_read = Some(read);
                    return AutoWinterStep::default();
                }
                tracing::info!(
                    "Auto winter: charge slot 1 was changed by someone else, keeping their setting to put back"
                );
                run.burst = None;
                run.changed_read = None;
                // Nothing this poll: the next one starts afresh from
                // whatever slot 1 now holds.
                return AutoWinterStep::default();
            }
            SlotReads::Baseline if !window_live => {
                // Put back (or never taken). The baseline lasts until the
                // charge flag reads as put back too.
                if snap.enable_charge != burst.baseline_enable_charge {
                    return AutoWinterStep {
                        writes: enable_charge_write(burst.baseline_enable_charge),
                    };
                }
                run.burst = None;
            }
            SlotReads::Baseline => {
                // Inside its window and still reading as before: the
                // write didn't land. Send it again, a few times.
                if burst.resends >= RESEND_LIMIT {
                    tracing::warn!(
                        "Auto winter: the charge window isn't reading back, giving up for now"
                    );
                    run.burst = None;
                    run.retry_after = facts.now + RETRY_AFTER_SECS;
                } else if let Some(current) = run.burst.as_mut() {
                    current.resends += 1;
                    let writes = burst_writes(snap, current);
                    return AutoWinterStep { writes };
                }
            }
            SlotReads::Partial if window_live => {
                // Half the window is on the inverter. Send it again; if it
                // never lands, put the user's slot back rather than leave a
                // window nobody will restore.
                if let Some(current) = run.burst.as_mut() {
                    if current.resends >= RESEND_LIMIT {
                        tracing::warn!(
                            "Auto winter: the charge window only half landed, putting slot 1 back"
                        );
                        current.ends_at = facts.now;
                        run.retry_after = facts.now + RETRY_AFTER_SECS;
                        let writes = restore_writes(snap, current);
                        return AutoWinterStep { writes };
                    }
                    current.resends += 1;
                    let writes = burst_writes(snap, current);
                    return AutoWinterStep { writes };
                }
            }
            // A half-landed restore, or a window that has run out: the
            // paths below write slot 1 in full.
            SlotReads::Partial | SlotReads::Window => {}
        }
    }

    // Warm again, or switched off: put slot 1 back.
    if let Some(burst) = run.burst.as_ref() {
        if !warming {
            let writes = restore_writes(snap, burst);
            note_waiting(run, None);
            return AutoWinterStep { writes };
        }
    }

    if !warming {
        run.waiting = None;
        return AutoWinterStep::default();
    }

    // Warming: a new burst once the last one has run out. Once the
    // temperature is back over the threshold (WarmPending) no new one starts.
    let burst_running = run
        .burst
        .as_ref()
        .is_some_and(|burst| facts.now < burst.ends_at);
    if burst_running || matches!(state, AutoWinterState::WarmPending { .. }) {
        if burst_running {
            note_waiting(run, None);
        }
        return AutoWinterStep::default();
    }

    if let Some(why) = blocked(snap, run, facts) {
        note_waiting(run, Some(why));
        // A finished window is left on the inverter until the next burst, so
        // put slot 1 back rather than let it recur tomorrow.
        return match run.burst.as_ref() {
            Some(burst) => AutoWinterStep {
                writes: restore_writes(snap, burst),
            },
            None => AutoWinterStep::default(),
        };
    }

    let next = next_burst(snap, run, facts);
    let writes = burst_writes(snap, &next);
    if writes.is_empty() {
        note_waiting(run, Some("the charge slot couldn't be encoded"));
        return AutoWinterStep::default();
    }
    tracing::info!(
        start = next.start,
        end = next.end,
        temp = snap.battery_temperature,
        "Auto winter: charging for {BURST_MINUTES} minutes to warm the battery"
    );
    note_waiting(run, None);
    run.burst = Some(next);
    AutoWinterStep { writes }
}

/// Hand a burst to an owner that takes slot 1 over (Force Charge, the
/// calibration cycle, a smart-charge top-up): it captured the baseline under
/// the burst through [`with_baseline_slot`] and puts it back itself, so HEM's
/// record goes without a write. Returns whether there was one to hand over.
pub fn hand_over(run: &mut AutoWinterRun, owner: AutoWinterBlocker) -> bool {
    if !owner.takes_slot_one() || run.burst.is_none() {
        return false;
    }
    tracing::info!("Auto winter: handing charge slot 1 over, {}", owner.code());
    run.burst = None;
    run.changed_read = None;
    true
}

/// On a graceful shutdown, put slot 1 back so HEM's window doesn't recur
/// every day while HEM is away. A slot someone else changed is left alone,
/// and with the inverter unreachable the burst stays on disk for the next
/// start to finish.
pub async fn restore_on_shutdown(state: &std::sync::Arc<AppState>, limit: std::time::Duration) {
    let snap = state.latest_snapshot.lock().await.clone();
    let writes = {
        let mut run = state.auto_winter_run.lock().await;
        run.stopped = true;
        let (Some(burst), Some(snap)) = (run.burst.as_ref(), snap.as_ref()) else {
            return;
        };
        if snap.inverter_serial.is_empty() || burst.serial != snap.inverter_serial {
            return;
        }
        match slot_reads(snap, burst) {
            SlotReads::Window | SlotReads::Partial => restore_writes(snap, burst),
            SlotReads::Baseline if snap.enable_charge != burst.baseline_enable_charge => {
                enable_charge_write(burst.baseline_enable_charge)
            }
            // Already back: only the record is left to clear.
            SlotReads::Baseline => Vec::new(),
            // Someone else's now: the next start confirms and drops it.
            SlotReads::Changed => return,
        }
    };
    if !writes.is_empty() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        crate::server::api::queue_writes_with_policy(
            state,
            writes,
            crate::inverter::poll::WriteBatchPolicy::FailFast,
            Some(tx),
            None,
        )
        .await;
        match tokio::time::timeout(limit, rx).await {
            Ok(Ok(crate::inverter::encoder::WriteOutcome::Ok)) => {
                tracing::info!("Auto winter: charge slot 1 put back on shutdown");
            }
            _ => {
                tracing::warn!(
                    "Auto winter: couldn't put charge slot 1 back on shutdown; it is put back when HEM starts again"
                );
                return;
            }
        }
    }
    let mut run = state.auto_winter_run.lock().await;
    match crate::settings::Settings::update_async(|s| s.auto_winter_burst = None).await {
        Ok(()) => run.burst = None,
        Err(e) => tracing::warn!("Auto winter: couldn't clear the saved charge slot: {e}"),
    }
}

/// True while a burst's window is on the inverter and running.
pub fn burst_running(run: &AutoWinterRun, now: i64) -> bool {
    run.burst.as_ref().is_some_and(|burst| now < burst.ends_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modbus::registers::{
        HR_CHARGE_SLOT_1_END, HR_CHARGE_SLOT_1_START, HR_ENABLE_CHARGE,
    };

    const NOW: i64 = 1_800_000_000;

    fn config(debounce: u32) -> AutoWinterConfig {
        AutoWinterConfig {
            enabled: true,
            debounce_readings: debounce,
            ..Default::default()
        }
    }

    fn facts(now: i64, inverter_minute: u16) -> AutoWinterFacts {
        AutoWinterFacts {
            now,
            inverter_minute,
            blocker: None,
        }
    }

    fn slot(start: (u8, u8), end: (u8, u8)) -> ScheduleSlot {
        ScheduleSlot {
            enabled: true,
            start_hour: start.0,
            start_minute: start.1,
            end_hour: end.0,
            end_minute: end.1,
            ..Default::default()
        }
    }

    /// A battery at the reserve, in Eco, with slot 1 set to the user's cheap window.
    fn snap(temp: f32) -> InverterSnapshot {
        let mut snap = InverterSnapshot {
            battery_temperature: temp,
            soc: 10,
            target_soc: 100,
            battery_power_mode: 1,
            enable_discharge: false,
            enable_charge: true,
            inverter_serial: "SN1".into(),
            device_type: DeviceType::ACCoupled,
            ..Default::default()
        };
        snap.charge_slots[0] = slot((23, 30), (5, 30));
        snap
    }

    /// The state after enough cold readings, with no burst yet.
    fn warming() -> (AutoWinterState, AutoWinterRun) {
        (AutoWinterState::Warming, AutoWinterRun::default())
    }

    fn written(step: &AutoWinterStep, address: u16) -> Option<u16> {
        step.writes
            .iter()
            .find(|write| write.address == address)
            .map(|write| write.value)
    }

    #[test]
    fn defaults_are_two_and_three_degrees() {
        let config = AutoWinterConfig::default();
        assert_eq!(config.cold_threshold, 2.0);
        assert_eq!(config.recovery_threshold, 3.0);
        let settings = crate::settings::Settings::default();
        assert_eq!(config.cold_threshold, settings.auto_winter_cold_threshold);
        assert_eq!(
            config.recovery_threshold,
            settings.auto_winter_recovery_threshold
        );
    }

    #[test]
    fn one_cold_reading_does_not_start_anything() {
        let mut state = AutoWinterState::Idle;
        let mut run = AutoWinterRun::default();
        let out = step(
            &snap(1.0),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 600),
        );
        assert!(out.writes.is_empty());
        assert_eq!(state, AutoWinterState::ColdPending { consecutive: 1 });
    }

    #[test]
    fn a_reading_that_is_not_cold_resets_the_count() {
        let mut state = AutoWinterState::ColdPending { consecutive: 2 };
        let mut run = AutoWinterRun::default();
        step(
            &snap(2.5),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 600),
        );
        assert_eq!(state, AutoWinterState::Idle);
    }

    #[test]
    fn a_missing_temperature_changes_nothing() {
        let mut state = AutoWinterState::ColdPending { consecutive: 2 };
        let mut run = AutoWinterRun::default();
        step(
            &snap(f32::NAN),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 600),
        );
        assert_eq!(state, AutoWinterState::ColdPending { consecutive: 2 });
    }

    #[test]
    fn the_debounced_cold_reading_starts_a_thirty_minute_burst_in_slot_one() {
        let mut state = AutoWinterState::ColdPending { consecutive: 2 };
        let mut run = AutoWinterRun::default();
        // 14:10 on the inverter.
        let out = step(
            &snap(1.0),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );

        assert_eq!(state, AutoWinterState::Warming);
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_START), Some(1410));
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_END), Some(1440));
        let burst = run.burst.expect("the baseline is recorded");
        assert_eq!((burst.baseline_start, burst.baseline_end), (2330, 530));
        assert!(burst.baseline_enable_charge);
        assert_eq!(burst.ends_at, NOW + 30 * 60);
    }

    #[test]
    fn a_burst_never_writes_the_charge_target() {
        let (mut state, mut run) = warming();
        let mut snapshot = snap(1.0);
        snapshot.enable_charge = false;
        let out = step(
            &snapshot,
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 600),
        );

        assert!(!out.writes.is_empty());
        for write in &out.writes {
            assert_ne!(
                write.address, HR_ENABLE_CHARGE_TARGET,
                "HR 20 is the user's"
            );
            assert_ne!(write.address, HR_CHARGE_TARGET_SOC, "HR 116 is the user's");
        }
        // The charge flag was off, so it is switched on after the slot.
        assert_eq!(written(&out, HR_ENABLE_CHARGE), Some(1));
        assert!(!run.burst.unwrap().baseline_enable_charge);
    }

    #[test]
    fn a_window_that_crosses_midnight_wraps() {
        let (mut state, mut run) = warming();
        // A daytime slot 1, so nothing is charging at 23:50.
        let mut daytime = snap(1.0);
        daytime.charge_slots[0] = slot((13, 0), (14, 0));
        let out = step(
            &daytime,
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 23 * 60 + 50),
        );
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_START), Some(2350));
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_END), Some(20));
    }

    #[test]
    fn the_next_burst_chains_and_keeps_the_original_baseline() {
        let (mut state, mut run) = warming();
        step(
            &snap(1.0),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );

        // 30 minutes on: slot 1 reads as the first window.
        let mut later = snap(1.2);
        later.charge_slots[0] = slot((14, 10), (14, 40));
        let out = step(
            &later,
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW + 30 * 60, 14 * 60 + 40),
        );

        assert_eq!(written(&out, HR_CHARGE_SLOT_1_START), Some(1440));
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_END), Some(1510));
        let burst = run.burst.unwrap();
        assert_eq!((burst.baseline_start, burst.baseline_end), (2330, 530));
    }

    #[test]
    fn nothing_is_written_while_a_burst_is_still_running() {
        let (mut state, mut run) = warming();
        step(
            &snap(1.0),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );
        let mut later = snap(1.2);
        later.charge_slots[0] = slot((14, 10), (14, 40));
        let out = step(
            &later,
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW + 5 * 60, 14 * 60 + 15),
        );
        assert!(out.writes.is_empty());
        assert!(burst_running(&run, NOW + 5 * 60));
    }

    #[test]
    fn it_stays_out_of_the_way_of_a_charge_slot_that_is_running() {
        let (mut state, mut run) = warming();
        // The user's 23:30-05:30 window is running at 01:00.
        let out = step(
            &snap(1.0),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 60),
        );
        assert!(out.writes.is_empty());
        assert!(run.burst.is_none());
    }

    #[test]
    fn it_does_not_start_over_another_owner_of_the_battery() {
        for blocker in [
            AutoWinterBlocker::Calibration,
            AutoWinterBlocker::ManualForce,
            AutoWinterBlocker::Safety,
            AutoWinterBlocker::Adaptive,
            AutoWinterBlocker::TariffAutomation,
            AutoWinterBlocker::DispatchTopUp,
        ] {
            let (mut state, mut run) = warming();
            let mut f = facts(NOW, 14 * 60 + 10);
            f.blocker = Some(blocker);
            let out = step(&snap(1.0), &config(3), &mut state, &mut run, &f);
            assert!(out.writes.is_empty(), "{blocker:?}");
            assert!(run.burst.is_none(), "{blocker:?}");
        }
    }

    #[test]
    fn it_only_charges_from_plain_eco() {
        let (mut state, mut run) = warming();
        let mut export = snap(1.0);
        export.battery_power_mode = 0;
        export.enable_discharge = true;
        let out = step(
            &export,
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );
        assert!(out.writes.is_empty());
    }

    #[test]
    fn it_does_not_charge_a_battery_that_is_at_its_target() {
        let (mut state, mut run) = warming();
        let mut full = snap(1.0);
        full.soc = 80;
        full.target_soc = 80;
        let out = step(
            &full,
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );
        assert!(out.writes.is_empty(), "the standard target is the cap");
    }

    #[test]
    fn a_finished_window_is_put_back_when_it_cannot_chain() {
        let (mut state, mut run) = warming();
        step(
            &snap(1.0),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );

        // Window over, and the battery has reached its target.
        let mut later = snap(1.0);
        later.charge_slots[0] = slot((14, 10), (14, 40));
        later.soc = 100;
        let out = step(
            &later,
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW + 30 * 60, 14 * 60 + 40),
        );

        assert_eq!(written(&out, HR_CHARGE_SLOT_1_START), Some(2330));
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_END), Some(530));
        assert!(run.burst.is_some(), "kept until the slot reads as put back");
    }

    #[test]
    fn warming_up_puts_slot_one_back() {
        let (mut state, mut run) = warming();
        step(
            &snap(1.0),
            &config(2),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );

        let mut warm = snap(3.5);
        warm.charge_slots[0] = slot((14, 10), (14, 40));
        // First warm reading, then the second confirms.
        let first = step(
            &warm,
            &config(2),
            &mut state,
            &mut run,
            &facts(NOW + 60, 14 * 60 + 11),
        );
        assert!(first.writes.is_empty(), "one warm reading isn't enough");
        let out = step(
            &warm,
            &config(2),
            &mut state,
            &mut run,
            &facts(NOW + 120, 14 * 60 + 12),
        );

        assert_eq!(state, AutoWinterState::Idle);
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_START), Some(2330));
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_END), Some(530));
    }

    #[test]
    fn the_baseline_stays_until_the_slot_reads_as_put_back() {
        let (mut state, mut run) = warming();
        step(
            &snap(1.0),
            &config(1),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );
        let mut warm = snap(3.5);
        warm.charge_slots[0] = slot((14, 10), (14, 40));
        step(
            &warm,
            &config(1),
            &mut state,
            &mut run,
            &facts(NOW + 60, 14 * 60 + 11),
        );
        assert!(run.burst.is_some());

        // The restore didn't land: sent again next poll.
        let again = step(
            &warm,
            &config(1),
            &mut state,
            &mut run,
            &facts(NOW + 80, 14 * 60 + 11),
        );
        assert_eq!(written(&again, HR_CHARGE_SLOT_1_START), Some(2330));

        // Now it reads as the baseline: nothing left to do.
        let mut back = snap(3.5);
        back.charge_slots[0] = slot((23, 30), (5, 30));
        let done = step(
            &back,
            &config(1),
            &mut state,
            &mut run,
            &facts(NOW + 100, 14 * 60 + 12),
        );
        assert!(done.writes.is_empty());
        assert!(run.burst.is_none());
    }

    #[test]
    fn a_slot_someone_else_changed_is_left_alone() {
        let (mut state, mut run) = warming();
        step(
            &snap(1.0),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );

        // The Planner rewrote slot 1 mid-burst, read on two polls.
        for (offset, clock) in [(60, "14:11:00"), (80, "14:11:20")] {
            let mut changed = snap(1.0);
            changed.charge_slots[0] = slot((1, 0), (2, 0));
            changed.inverter_time = clock.into();
            let out = step(
                &changed,
                &config(3),
                &mut state,
                &mut run,
                &facts(NOW + offset, 14 * 60 + 11),
            );
            assert!(out.writes.is_empty(), "never written over");
        }
        assert!(run.burst.is_none(), "the baseline is dropped");
    }

    #[test]
    fn a_window_that_never_lands_is_resent_then_given_up() {
        let (mut state, mut run) = warming();
        step(
            &snap(1.0),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );

        // Slot 1 still reads as the user's own window.
        for attempt in 1..=3 {
            let out = step(
                &snap(1.0),
                &config(3),
                &mut state,
                &mut run,
                &facts(NOW + attempt * 20, 14 * 60 + 10),
            );
            assert_eq!(
                written(&out, HR_CHARGE_SLOT_1_START),
                Some(1410),
                "resend {attempt}"
            );
        }
        let out = step(
            &snap(1.0),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW + 80, 14 * 60 + 11),
        );
        assert!(out.writes.is_empty());
        assert!(run.burst.is_none());
        assert!(run.retry_after > NOW + 80, "waits before trying again");

        let out = step(
            &snap(1.0),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW + 100, 14 * 60 + 12),
        );
        assert!(out.writes.is_empty(), "still waiting");
    }

    #[test]
    fn a_baseline_from_another_inverter_is_never_written() {
        let (mut state, mut run) = warming();
        step(
            &snap(1.0),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );
        let mut other = snap(5.0);
        other.inverter_serial = "SN2".into();
        let out = step(
            &other,
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW + 60, 14 * 60 + 11),
        );
        assert!(out.writes.is_empty());
        assert!(run.burst.is_none());
    }

    #[test]
    fn a_restart_during_a_burst_puts_slot_one_back() {
        // The baseline came back from disk; the state machine starts Idle.
        let mut run = AutoWinterRun {
            burst: Some(AutoWinterBurst {
                serial: "SN1".into(),
                baseline_start: 2330,
                baseline_end: 530,
                baseline_enable_charge: true,
                start: 1410,
                end: 1440,
                ends_at: NOW,
                resends: 0,
                previous: None,
            }),
            ..Default::default()
        };
        let mut state = AutoWinterState::Idle;
        let mut stale = snap(5.0);
        stale.charge_slots[0] = slot((14, 10), (14, 40));
        let out = step(
            &stale,
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW + 3600, 15 * 60),
        );
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_START), Some(2330));
    }

    #[test]
    fn an_empty_slot_one_is_put_back_empty() {
        let (mut state, mut run) = warming();
        let mut empty = snap(1.0);
        empty.charge_slots[0] = ScheduleSlot::default();
        step(
            &empty,
            &config(1),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );
        assert_eq!(
            run.burst
                .as_ref()
                .map(|b| (b.baseline_start, b.baseline_end)),
            Some((0, 0))
        );

        let mut warm = snap(3.5);
        warm.charge_slots[0] = slot((14, 10), (14, 40));
        step(
            &warm,
            &config(1),
            &mut state,
            &mut run,
            &facts(NOW + 60, 14 * 60 + 11),
        );
        let out = step(
            &warm,
            &config(1),
            &mut state,
            &mut run,
            &facts(NOW + 80, 14 * 60 + 11),
        );
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_START), Some(0));
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_END), Some(0));
    }

    #[test]
    fn switching_it_off_puts_slot_one_back() {
        let (mut state, mut run) = warming();
        step(
            &snap(1.0),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );
        let mut off = config(3);
        off.enabled = false;
        let mut running = snap(1.0);
        running.charge_slots[0] = slot((14, 10), (14, 40));
        let out = step(
            &running,
            &off,
            &mut state,
            &mut run,
            &facts(NOW + 60, 14 * 60 + 11),
        );
        assert_eq!(state, AutoWinterState::Idle);
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_START), Some(2330));
    }

    #[test]
    fn a_charge_flag_it_switched_on_is_switched_off_again() {
        let (mut state, mut run) = warming();
        let mut off = snap(1.0);
        off.enable_charge = false;
        step(
            &off,
            &config(1),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );

        // Warm: restore writes the slot and the flag.
        let mut warm = snap(3.5);
        warm.enable_charge = true;
        warm.charge_slots[0] = slot((14, 10), (14, 40));
        step(
            &warm,
            &config(1),
            &mut state,
            &mut run,
            &facts(NOW + 60, 14 * 60 + 11),
        );
        let out = step(
            &warm,
            &config(1),
            &mut state,
            &mut run,
            &facts(NOW + 80, 14 * 60 + 11),
        );
        assert_eq!(written(&out, HR_ENABLE_CHARGE), Some(0));
    }

    /// A burst at 14:10 over the user's 23:30-05:30, already on the run.
    fn started() -> (AutoWinterState, AutoWinterRun) {
        let (mut state, mut run) = warming();
        step(
            &snap(1.0),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );
        assert!(run.burst.is_some());
        (state, run)
    }

    fn reading(temp: f32, slot_one: ScheduleSlot, clock: &str) -> InverterSnapshot {
        let mut snapshot = snap(temp);
        snapshot.charge_slots[0] = slot_one;
        snapshot.inverter_time = clock.into();
        snapshot
    }

    #[test]
    fn a_window_that_half_landed_is_sent_again_and_the_baseline_kept() {
        // HR 94 took 14:10, HR 95 hit exception 67: slot 1 reads 14:10-05:30.
        let (mut state, mut run) = started();
        let out = step(
            &reading(1.0, slot((14, 10), (5, 30)), "14:10:20"),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW + 20, 14 * 60 + 10),
        );
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_START), Some(1410));
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_END), Some(1440));
        let burst = run.burst.expect("the baseline is kept");
        assert_eq!((burst.baseline_start, burst.baseline_end), (2330, 530));
    }

    #[test]
    fn a_restore_that_half_landed_is_sent_again() {
        let (mut state, mut run) = started();
        // Warm (the first reading starts the count).
        step(
            &reading(3.5, slot((14, 10), (14, 40)), "14:19:40"),
            &config(1),
            &mut state,
            &mut run,
            &facts(NOW + 580, 14 * 60 + 19),
        );
        // The restore wrote 23:30 but not 05:30.
        let out = step(
            &reading(3.5, slot((23, 30), (14, 40)), "14:20:00"),
            &config(1),
            &mut state,
            &mut run,
            &facts(NOW + 600, 14 * 60 + 20),
        );
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_START), Some(2330));
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_END), Some(530));
        assert!(run.burst.is_some());
    }

    #[test]
    fn a_window_that_never_fully_lands_is_put_back_not_abandoned() {
        let (mut state, mut run) = started();
        let half = slot((14, 10), (5, 30));
        for attempt in 1..=3 {
            step(
                &reading(1.0, half.clone(), &format!("14:10:{attempt:02}")),
                &config(3),
                &mut state,
                &mut run,
                &facts(NOW + attempt * 20, 14 * 60 + 10),
            );
        }
        let out = step(
            &reading(1.0, half, "14:11:30"),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW + 90, 14 * 60 + 11),
        );
        assert_eq!(
            written(&out, HR_CHARGE_SLOT_1_END),
            Some(530),
            "the user's slot goes back"
        );
        assert!(run.burst.is_some(), "kept until it reads as put back");
        assert!(run.retry_after > NOW + 90);
    }

    /// A burst chained at 14:40 after the 14:10 one.
    fn chained() -> (AutoWinterState, AutoWinterRun) {
        let (mut state, mut run) = started();
        step(
            &reading(1.2, slot((14, 10), (14, 40)), "14:40:00"),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW + 30 * 60, 14 * 60 + 40),
        );
        assert_eq!(
            run.burst.as_ref().map(|b| (b.start, b.end)),
            Some((1440, 1510))
        );
        (state, run)
    }

    #[test]
    fn a_chained_window_that_did_not_land_is_sent_again_not_taken_for_a_change() {
        // The 14:40 writes failed: slot 1 still reads the 14:10 window.
        let (mut state, mut run) = chained();
        for (offset, clock) in [(20, "14:40:20"), (40, "14:40:40")] {
            let out = step(
                &reading(1.2, slot((14, 10), (14, 40)), clock),
                &config(3),
                &mut state,
                &mut run,
                &facts(NOW + 30 * 60 + offset, 14 * 60 + 40),
            );
            assert_eq!(
                written(&out, HR_CHARGE_SLOT_1_START),
                Some(1440),
                "at {offset}"
            );
        }
        let burst = run.burst.as_ref().expect("HEM's own window, not a change");
        assert_eq!((burst.baseline_start, burst.baseline_end), (2330, 530));
        // An owner capturing now gets the user's slot, not the old window.
        let capture = with_baseline_slot(&run, &reading(1.2, slot((14, 10), (14, 40)), ""));
        assert_eq!(slot_one(&capture), (2330, 530));
    }

    #[test]
    fn one_odd_read_of_slot_one_does_not_drop_the_baseline() {
        let (mut state, mut run) = started();
        let out = step(
            &reading(1.0, slot((1, 0), (2, 0)), "14:11:00"),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW + 60, 14 * 60 + 11),
        );
        assert!(out.writes.is_empty());
        assert!(run.burst.is_some(), "one read isn't enough");

        // The next read is HEM's window again: it was a bad read.
        step(
            &reading(1.0, slot((14, 10), (14, 40)), "14:11:20"),
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW + 80, 14 * 60 + 11),
        );
        assert!(run.burst.is_some());
        assert!(run.changed_read.is_none());
    }

    #[test]
    fn a_frozen_frame_repeating_a_change_does_not_confirm_it() {
        let (mut state, mut run) = started();
        for offset in [60, 80] {
            let out = step(
                &reading(1.0, slot((1, 0), (2, 0)), "14:11:00"),
                &config(3),
                &mut state,
                &mut run,
                &facts(NOW + offset, 14 * 60 + 11),
            );
            assert!(out.writes.is_empty(), "at {offset}");
        }
        let burst = run.burst.expect("the same frame twice is one read");
        assert_eq!(
            (burst.baseline_start, burst.baseline_end),
            (2330, 530),
            "still the user's baseline"
        );
    }

    #[test]
    fn a_change_read_twice_from_fresh_frames_drops_the_baseline() {
        let (mut state, mut run) = started();
        for (offset, clock) in [(60, "14:11:00"), (80, "14:11:20")] {
            let out = step(
                &reading(1.0, slot((1, 0), (2, 0)), clock),
                &config(3),
                &mut state,
                &mut run,
                &facts(NOW + offset, 14 * 60 + 11),
            );
            assert!(out.writes.is_empty(), "never written over");
        }
        assert!(run.burst.is_none(), "the user wins");
    }

    #[test]
    fn a_restored_burst_waits_for_the_inverter_identity() {
        let (mut state, mut run) = started();
        let mut nameless = reading(1.0, slot((14, 10), (14, 40)), "14:11:00");
        nameless.inverter_serial = String::new();
        let out = step(
            &nameless,
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW + 60, 14 * 60 + 11),
        );
        assert!(out.writes.is_empty());
        assert!(run.burst.is_some(), "no serial yet isn't another inverter");
    }

    #[test]
    fn the_charge_flag_is_retried_until_it_reads_as_put_back() {
        let (mut state, mut run) = warming();
        let mut off = snap(1.0);
        off.enable_charge = false;
        step(
            &off,
            &config(1),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );
        // Warm: two readings, the second puts slot 1 and the flag back.
        let mut warm = snap(3.5);
        warm.enable_charge = true;
        warm.charge_slots[0] = slot((14, 10), (14, 40));
        for offset in [20, 40] {
            step(
                &warm,
                &config(1),
                &mut state,
                &mut run,
                &facts(NOW + offset, 14 * 60 + 10),
            );
        }
        assert_eq!(state, AutoWinterState::Idle);
        // The slot is back, but the flag write failed.
        let mut back = snap(3.5);
        back.enable_charge = true;
        for offset in [60, 80] {
            let out = step(
                &back,
                &config(1),
                &mut state,
                &mut run,
                &facts(NOW + offset, 14 * 60 + 11),
            );
            assert_eq!(written(&out, HR_ENABLE_CHARGE), Some(0), "at {offset}");
            assert!(run.burst.is_some(), "kept until the flag reads back");
        }
        back.enable_charge = false;
        let out = step(
            &back,
            &config(1),
            &mut state,
            &mut run,
            &facts(NOW + 100, 14 * 60 + 12),
        );
        assert!(out.writes.is_empty());
        assert!(run.burst.is_none());
    }

    #[test]
    fn an_owner_taking_slot_one_over_captures_the_baseline_and_gets_the_burst() {
        let (mut state, mut run) = warming();
        let mut off = snap(1.0);
        off.enable_charge = false;
        step(
            &off,
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );
        let live = reading(1.0, slot((14, 10), (14, 40)), "14:20:00");
        let mut live = live;
        live.enable_charge = true;

        // What Force Charge captures is the user's slot and flag.
        let captured = with_baseline_slot(&run, &live);
        assert_eq!(slot_one(&captured), (2330, 530));
        assert!(!captured.enable_charge);
        assert_eq!(
            captured.charge_slots[0].target_soc,
            live.charge_slots[0].target_soc
        );

        for blocker in [
            AutoWinterBlocker::ManualForce,
            AutoWinterBlocker::DispatchTopUp,
            AutoWinterBlocker::Calibration,
        ] {
            let mut run = run.clone();
            let mut state = state.clone();
            let mut f = facts(NOW + 600, 14 * 60 + 20);
            f.blocker = Some(blocker);
            let out = step(&live, &config(3), &mut state, &mut run, &f);
            assert!(out.writes.is_empty(), "{blocker:?}: never written over");
            assert!(run.burst.is_none(), "{blocker:?}: handed over");
        }
    }

    #[test]
    fn only_owners_that_take_slot_one_get_the_burst() {
        for (owner, takes) in [
            (AutoWinterBlocker::Calibration, true),
            (AutoWinterBlocker::ManualForce, true),
            (AutoWinterBlocker::DispatchTopUp, true),
            (AutoWinterBlocker::Safety, false),
            (AutoWinterBlocker::Adaptive, false),
            (AutoWinterBlocker::TariffAutomation, false),
        ] {
            let (_, mut run) = started();
            assert_eq!(hand_over(&mut run, owner), takes, "{owner:?}");
            assert_eq!(run.burst.is_none(), takes, "{owner:?}");
        }
        assert!(!hand_over(
            &mut AutoWinterRun::default(),
            AutoWinterBlocker::ManualForce
        ));
    }

    #[test]
    fn with_no_burst_on_slot_one_the_reading_stands() {
        let run = AutoWinterRun::default();
        let reading = snap(1.0);
        assert_eq!(slot_one(&with_baseline_slot(&run, &reading)), (2330, 530));
        // A burst whose slot someone else has changed holds nothing.
        let (_, run) = started();
        let changed = reading_with(slot((1, 0), (2, 0)));
        assert_eq!(slot_one(&with_baseline_slot(&run, &changed)), (100, 200));
    }

    fn reading_with(slot_one: ScheduleSlot) -> InverterSnapshot {
        reading(1.0, slot_one, "")
    }

    #[test]
    fn three_phase_inverters_get_no_bursts() {
        let (mut state, mut run) = warming();
        let mut three_phase = snap(1.0);
        three_phase.device_type = DeviceType::ThreePhase;
        three_phase.charge_slots[0] = slot((13, 0), (14, 0));
        let out = step(
            &three_phase,
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );
        assert!(out.writes.is_empty());
        assert!(run.burst.is_none());
    }

    #[test]
    fn slot_ones_own_target_caps_a_burst_on_ten_slot_models() {
        let (mut state, mut run) = warming();
        let mut gen3 = snap(1.0);
        gen3.device_type = DeviceType::Gen3Hybrid;
        gen3.charge_slots[0] = slot((13, 0), (14, 0));
        gen3.soc = 60;
        gen3.target_soc = 100;
        gen3.raw_charge_slot_1_target_soc = Some(30);
        let out = step(
            &gen3,
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );
        assert!(out.writes.is_empty(), "slot 1 stops at 30%");

        // No per-slot target (raw 0): the global target is the cap.
        gen3.raw_charge_slot_1_target_soc = Some(0);
        let out = step(
            &gen3,
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW, 14 * 60 + 10),
        );
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_START), Some(1410));
    }

    #[test]
    fn stopped_writes_nothing() {
        let (mut state, mut run) = started();
        run.stopped = true;
        // Switched off with the window on the inverter: a restore, normally.
        let mut off = config(1);
        off.enabled = false;
        let out = step(
            &reading(3.5, slot((14, 10), (14, 40)), "14:20:00"),
            &off,
            &mut state,
            &mut run,
            &facts(NOW + 600, 14 * 60 + 20),
        );
        assert!(out.writes.is_empty());
    }

    #[test]
    fn thresholds_from_older_versions_are_brought_into_range() {
        assert_eq!(normalise_thresholds(8.0, 12.0), (2.0, 3.0), "old defaults");
        assert_eq!(
            normalise_thresholds(4.0, 6.0),
            (4.0, 6.0),
            "a user's choice"
        );
        assert_eq!(normalise_thresholds(5.0, 12.0), (5.0, 10.0));
        assert_eq!(normalise_thresholds(0.0, 2.0), (2.0, 3.0));
        assert_eq!(normalise_thresholds(12.0, 15.0), (9.0, 10.0));
        assert_eq!(normalise_thresholds(f32::NAN, 3.0), (2.0, 3.0));
    }

    #[test]
    fn a_target_saved_before_the_update_is_put_back_once() {
        let mut state = AutoWinterState::Idle;
        let mut run = AutoWinterRun {
            legacy: Some(AutoWinterSaved {
                enable_charge_target: false,
                target_soc: 100,
            }),
            ..Default::default()
        };
        let mut raised = snap(5.0);
        raised.enable_charge_target = true;
        raised.target_soc = 80;

        let out = step(&raised, &config(3), &mut state, &mut run, &facts(NOW, 600));
        assert_eq!(written(&out, HR_ENABLE_CHARGE_TARGET), Some(0));
        assert_eq!(written(&out, HR_CHARGE_SLOT_1_START), None);
        assert_eq!(written(&out, HR_CHARGE_TARGET_SOC), Some(100));
        assert!(run.legacy.is_some(), "kept until it reads back");

        let mut back = snap(5.0);
        back.enable_charge_target = false;
        back.target_soc = 100;
        let out = step(
            &back,
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW + 60, 600),
        );
        assert!(out.writes.is_empty());
        assert!(run.legacy.is_none());
    }

    #[test]
    fn a_saved_target_that_will_not_stick_is_given_up_on() {
        let mut state = AutoWinterState::Idle;
        let mut run = AutoWinterRun {
            legacy: Some(AutoWinterSaved {
                enable_charge_target: false,
                target_soc: 100,
            }),
            ..Default::default()
        };
        let mut raised = snap(5.0);
        raised.enable_charge_target = true;
        raised.target_soc = 80;
        for attempt in 0..LEGACY_ATTEMPTS {
            let out = step(
                &raised,
                &config(3),
                &mut state,
                &mut run,
                &facts(NOW + i64::from(attempt), 600),
            );
            assert!(!out.writes.is_empty());
        }
        let out = step(
            &raised,
            &config(3),
            &mut state,
            &mut run,
            &facts(NOW + 10, 600),
        );
        assert!(out.writes.is_empty());
        assert!(run.legacy.is_none());
    }
}
