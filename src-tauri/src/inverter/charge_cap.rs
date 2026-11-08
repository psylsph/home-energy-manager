//! Optional charge cap (issue #426).
//!
//! Stops the battery charging once its SOC reaches a chosen level, so the
//! surplus goes to export, and lets it charge again once the SOC has dropped
//! to a lower resume level. The inverter firmware has no such setting (its
//! target SOC only stops a timed grid charge), so HEM enforces it from the
//! poll loop with the native battery pause (HR 318 mode 1, "pause charge").
//!
//! Only models GivTCP pauses that way are supported (`supports`): the
//! single-phase All-in-Ones, the Gen2 and Gen3 hybrids, the HV Gen3 and the
//! AIO Hybrid (all with the pause window), the Gen1 from ARM firmware 187
//! and the AC-coupled models (0x3001 and the Mk2) from ARM firmware 200.
//! The Gen1 and AC take the mode alone, so the cap pauses and releases them
//! with HR 318 only. Everything else, the Gateway included, hides the option.
//! The cap only starts from a fresh read of HR 318, so a Gen1 that can't be
//! read back (the old 4xx firmware) offers it but never holds charging, and
//! shows `Unreadable` so the Control page says why.
//!
//! HR 318 is shared with the user's own pause, Timed Discharge, Force
//! Discharge's restore and the calibration cycle, several of which save it and
//! put it back later. So the cap:
//!
//! - only starts from a freshly read mode 0, never over another pause;
//! - keeps a persisted [`ChargeCapHold`] record from before its first write
//!   until a fresh read shows mode 0 while no other owner could still put its
//!   pause back. While the record exists, a mode-1 pause with the cap's
//!   window on this inverter is the cap's to release;
//! - lets go whenever anything else wants to charge (a running charge slot,
//!   Force Charge, calibration), when it's switched off, or once the SOC has
//!   fallen to the resume level — except while another owner has saved
//!   HR 318 (calibration, Force Discharge's restore, a remote pause): that
//!   owner lifts and restores the pause itself, and the cap writes nothing
//!   until it has finished.
//!
//! It only stops charging: discharge, export, rates, slots and reserve are
//! never touched. And it only works while HEM is running.

use serde::{Deserialize, Serialize};

use crate::inverter::encoder::RegisterWrite;
use crate::inverter::model::{DeviceType, InverterSnapshot};
use crate::modbus::registers::{
    HR_BATTERY_PAUSE_MODE, HR_BATTERY_PAUSE_SLOT_1_END, HR_BATTERY_PAUSE_SLOT_1_START,
};

/// HR 318 value that pauses charging only.
pub const PAUSE_CHARGE: u16 = 1;
/// HR 318 value with no pause.
pub const PAUSE_NONE: u16 = 0;
/// Start of the all-day pause window the cap writes to HR 319.
pub const CAP_WINDOW_START: u16 = 0;
/// End of the all-day pause window the cap writes to HR 320.
pub const CAP_WINDOW_END: u16 = 2359;
/// Consecutive readings at or above the cap before the cap starts holding,
/// so one corrupt SOC read can't pause charging.
pub const ENGAGE_READINGS: u32 = 2;
/// Consecutive readings from a different inverter before a hold record is
/// dropped, so one misread serial can't orphan the cap's pause.
pub const FOREIGN_READINGS: u32 = 3;
/// Lowest cap the user can choose.
pub const MIN_CAP_SOC: u8 = 50;
/// Highest cap the user can choose (100 would be no cap).
pub const MAX_CAP_SOC: u8 = 99;
/// Lowest resume level the user can choose.
pub const MIN_RESUME_SOC: u8 = 40;
/// The resume level must sit at least this far below the cap.
pub const MIN_GAP: u8 = 2;

/// User settings for the charge cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChargeCapConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Stop charging at this SOC.
    #[serde(default = "default_cap_soc")]
    pub cap_soc: u8,
    /// Charge again once the SOC has fallen to this level.
    #[serde(default = "default_resume_soc")]
    pub resume_soc: u8,
}

fn default_cap_soc() -> u8 {
    95
}

fn default_resume_soc() -> u8 {
    92
}

impl Default for ChargeCapConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cap_soc: default_cap_soc(),
            resume_soc: default_resume_soc(),
        }
    }
}

impl ChargeCapConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(MIN_CAP_SOC..=MAX_CAP_SOC).contains(&self.cap_soc) {
            return Err(format!(
                "The charge cap must be between {MIN_CAP_SOC}% and {MAX_CAP_SOC}%"
            ));
        }
        if self.resume_soc < MIN_RESUME_SOC {
            return Err(format!(
                "The resume level must be at least {MIN_RESUME_SOC}%"
            ));
        }
        if self.resume_soc.saturating_add(MIN_GAP) > self.cap_soc {
            return Err(format!(
                "The resume level must be at least {MIN_GAP}% below the charge cap"
            ));
        }
        Ok(())
    }
}

/// Persisted proof that the cap started a pause on this inverter, and the
/// pause window it found there. Written before the cap's first pause write
/// and kept until a fresh read confirms the pause is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChargeCapHold {
    pub inverter_serial: String,
    pub device_type_code: String,
    /// HR 319 before the cap's write.
    pub prior_slot_start: u16,
    /// HR 320 before the cap's write.
    pub prior_slot_end: u16,
    /// The pause was set with HR 318 alone (the AC-coupled models, which
    /// take no pause window), so the window is neither written nor read.
    #[serde(default)]
    pub mode_only: bool,
}

/// What the cap is doing, for the API and the Control page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChargeCapState {
    /// Switched off and holding nothing.
    #[default]
    Off,
    /// This inverter can't pause charging the way the cap needs.
    Unsupported,
    /// On, below the cap, charging normally.
    Waiting,
    /// At or above the cap, waiting for a second reading.
    Pending,
    /// Charging is paused.
    Holding,
    /// The cap's pause is being lifted.
    Releasing,
    /// Something else is charging the battery (a charge slot, Force Charge,
    /// calibration); the cap stands aside until it finishes.
    Yielding,
    /// Another pause (the user's own, Timed Discharge, or a remote command)
    /// owns the register; the cap won't overwrite it.
    Blocked,
    /// The inverter's pause can't be read back (the Gen1's old 4xx
    /// firmware), and the cap only starts from a fresh read, so it never will.
    Unreadable,
}

impl ChargeCapState {
    pub fn api_name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Unsupported => "unsupported",
            Self::Waiting => "waiting",
            Self::Pending => "pending",
            Self::Holding => "holding",
            Self::Releasing => "releasing",
            Self::Yielding => "yielding",
            Self::Blocked => "blocked",
            Self::Unreadable => "unreadable",
        }
    }
}

/// In-memory state carried between polls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChargeCapRuntime {
    pub state: ChargeCapState,
    /// Consecutive readings at or above the cap.
    pub readings_at_cap: u32,
    /// Snapshot timestamp of the poll that issued the last write. No write is
    /// repeated until a snapshot read after it shows whether it landed.
    pub last_write_at: Option<i64>,
    /// The cap level the current hold paused at, so raising the cap above
    /// the SOC lets the battery charge to it. `None` until known (a restart
    /// mid-hold adopts the configured cap).
    pub held_at_cap: Option<u8>,
    /// Consecutive readings from an inverter other than the record's.
    pub foreign_readings: u32,
}

/// Facts about the rest of the system the poll loop supplies each poll.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChargeCapContext {
    /// Something wants the battery charging now: a charge slot is running
    /// (Timed Charge, Force Charge, Cosy, Agile, the Planner) or the
    /// calibration cycle is active.
    pub wants_charge: bool,
    /// Another owner has saved HR 318 and may put the cap's pause back later
    /// (a remote pause, Force Discharge's restore, calibration, or the
    /// dispatch hold's native pause on an AIO).
    pub other_pause_owner: bool,
}

/// Everything the poll loop and the API share, under one lock.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChargeCapShared {
    pub config: ChargeCapConfig,
    pub runtime: ChargeCapRuntime,
    /// Mirrors `Settings::charge_cap_hold`.
    pub hold: Option<ChargeCapHold>,
    /// Set at shutdown once the release is queued: the cap writes nothing more.
    pub stopped: bool,
}

/// Run one poll's step: evaluate, persist the hold record when it changed,
/// and return the writes to issue now.
///
/// A new or replaced record must be on disk before its pause is written, so
/// a restart can always tell the cap's pause from anyone else's: when that
/// save fails, nothing changes and nothing is written this poll. A record
/// that only goes away is dropped in memory even if the save fails (a stale
/// one on disk is dropped again at the next fresh read after a restart).
pub async fn advance(
    state: &std::sync::Arc<crate::inverter::poll::AppState>,
    snapshot: &InverterSnapshot,
    context: ChargeCapContext,
) -> Vec<RegisterWrite> {
    advance_with(state, snapshot, context, |hold| async move {
        crate::settings::Settings::update_async(move |s| s.charge_cap_hold = hold).await
    })
    .await
}

async fn advance_with<F, Fut>(
    state: &std::sync::Arc<crate::inverter::poll::AppState>,
    snapshot: &InverterSnapshot,
    context: ChargeCapContext,
    persist: F,
) -> Vec<RegisterWrite>
where
    F: FnOnce(Option<ChargeCapHold>) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let mut guard = state.charge_cap.lock().await;
    if guard.stopped {
        return Vec::new();
    }
    let mut runtime = guard.runtime.clone();
    let mut hold = guard.hold.clone();
    let writes = evaluate(snapshot, &guard.config, context, &mut runtime, &mut hold);
    if hold != guard.hold {
        if let Err(e) = persist(hold.clone()).await {
            if hold.is_some() {
                tracing::warn!("Charge cap: couldn't save its hold record, not pausing: {e}");
                return Vec::new();
            }
            tracing::warn!("Charge cap: couldn't clear its hold record: {e}");
        }
    }
    if runtime.state != guard.runtime.state {
        tracing::info!(
            from = guard.runtime.state.api_name(),
            to = runtime.state.api_name(),
            soc = snapshot.soc,
            "Charge cap"
        );
    }
    guard.runtime = runtime;
    guard.hold = hold;
    writes
}

/// How long shutdown waits for the cap's release to be written: room for
/// the poll loop to finish its cycle and send three paced writes, while
/// staying, with the history flush beside it, inside Docker's 10 s stop.
pub const SHUTDOWN_RELEASE_LIMIT: std::time::Duration = std::time::Duration::from_secs(8);

/// On a graceful shutdown, hand charging back if the cap is holding, so the
/// battery isn't left paused while HEM is away. Stops the cap first so a poll
/// still running can't pause it again. Once the inverter has taken the
/// release the record is dropped, so a pause someone sets while HEM is away
/// is never mistaken for the cap's. Bounded by `limit`: with the inverter
/// unreachable the pause stays, and the record (kept on disk) lets the next
/// start finish the job.
pub async fn release_on_shutdown(
    state: &std::sync::Arc<crate::inverter::poll::AppState>,
    limit: std::time::Duration,
) {
    let (writes, record) = {
        let mut guard = state.charge_cap.lock().await;
        guard.stopped = true;
        // Holding, or a release not yet confirmed: the pause is the cap's.
        // Any other state means another owner has the register, or nothing
        // has been read since a restart, so it's left alone.
        match (&guard.hold, guard.runtime.state) {
            (Some(record), ChargeCapState::Holding | ChargeCapState::Releasing) => {
                (release_writes(record), record.clone())
            }
            _ => return,
        }
    };
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
            tracing::info!("Charge cap: charging handed back on shutdown");
            let mut guard = state.charge_cap.lock().await;
            if guard.hold.as_ref() == Some(&record) {
                match crate::settings::Settings::update_async(|s| s.charge_cap_hold = None).await {
                    Ok(()) => guard.hold = None,
                    Err(e) => tracing::warn!("Charge cap: couldn't clear its hold record: {e}"),
                }
            }
        }
        _ => tracing::warn!(
            "Charge cap: couldn't hand charging back on shutdown; the battery stays \
             paused until HEM is running again"
        ),
    }
}

/// The facts the poll loop gathers for [`evaluate`].
pub fn context_for(
    snapshot: &InverterSnapshot,
    inverter_minute: u16,
    calibrating: bool,
    force_charge: bool,
    force_discharge_revert: bool,
    pause_mode_revert: bool,
    dispatch_native_pause: bool,
) -> ChargeCapContext {
    let charge_slot_running = snapshot.enable_charge
        && crate::inverter::state_machines::export_window_contains(
            &snapshot.charge_slots,
            inverter_minute,
        );
    ChargeCapContext {
        wants_charge: calibrating || force_charge || charge_slot_running,
        other_pause_owner: calibrating
            || force_discharge_revert
            || pause_mode_revert
            || dispatch_native_pause,
    }
}

/// Fill in the snapshot's charge cap fields from the shared state.
pub fn stamp_snapshot(snapshot: &mut InverterSnapshot, shared: &ChargeCapShared) {
    let supported = snapshot_supports(snapshot);
    snapshot.charge_cap_enabled = shared.config.enabled && supported;
    snapshot.charge_cap_soc = shared.config.cap_soc;
    snapshot.charge_cap_state = shown_state(shared, supported).api_name().to_string();
}

/// The state to show: the runtime's, except that a cap switched off or an
/// unsupported inverter shows as such straight away, before the next poll.
pub fn shown_state(shared: &ChargeCapShared, supported: bool) -> ChargeCapState {
    if shared.hold.is_none() {
        if !supported {
            return ChargeCapState::Unsupported;
        }
        if !shared.config.enabled {
            return ChargeCapState::Off;
        }
    }
    let wants_hold = shared.config.enabled && supported;
    match shared.runtime.state {
        // Switched off (or no longer supported) while holding: the next poll
        // hands charging back, so say so now rather than "holding".
        ChargeCapState::Holding if !wants_hold => ChargeCapState::Releasing,
        // Started with a record but nothing read yet (a restart mid-hold):
        // the cap's pause is most likely still on, so don't say "off".
        ChargeCapState::Off if shared.hold.is_some() => {
            if wants_hold {
                ChargeCapState::Holding
            } else {
                ChargeCapState::Releasing
            }
        }
        state => state,
    }
}

/// Whether this inverter can run the charge cap: the models where GivTCP,
/// with far more user feedback than HEM, pauses charging. Its single-phase
/// AC takes the mode alone from ARM firmware 200, and its three-phase
/// models have no pause at all. The Gateway does read the registers but
/// keeps them to itself (issue #428).
pub fn supports(device: DeviceType, arm_fw: u16) -> bool {
    match device {
        // The AC three-phase has a pause for Timed Discharge, but GivTCP gives
        // it none and nothing here has run on one, so it hides the cap.
        DeviceType::ACThreePhase => false,
        _ => device.supports_pause_mode(arm_fw),
    }
}

/// Whether the cap pauses this model with HR 318 alone: HEM's pause table
/// gives it the mode but no window (the AC-coupled).
fn pauses_with_mode_alone(device: DeviceType, arm_fw: u16) -> bool {
    device.pause_register_support(arm_fw) == crate::inverter::model::PauseRegisterSupport::ModeOnly
}

/// Whether the snapshot's inverter supports the cap.
pub fn snapshot_supports(snapshot: &InverterSnapshot) -> bool {
    supports(snapshot.device_type, arm_fw(snapshot))
}

fn arm_fw(snapshot: &InverterSnapshot) -> u16 {
    snapshot.firmware_version.parse::<u16>().unwrap_or(0)
}

/// Evaluate the cap for one poll. Updates `runtime` and `hold` in place and
/// returns the writes to issue (empty for none). The caller persists `hold`
/// when it changes, before issuing the writes.
pub fn evaluate(
    snapshot: &InverterSnapshot,
    config: &ChargeCapConfig,
    context: ChargeCapContext,
    runtime: &mut ChargeCapRuntime,
    hold: &mut Option<ChargeCapHold>,
) -> Vec<RegisterWrite> {
    // A record captured on another inverter is never acted on, and is
    // dropped once that inverter has been seen for sure: a single misread
    // serial mustn't leave the cap's pause with no owner.
    if let Some(record) = hold.as_ref() {
        if hold_matches_inverter(record, snapshot) {
            runtime.foreign_readings = 0;
        } else {
            if snapshot.inverter_serial.is_empty() {
                return Vec::new();
            }
            runtime.foreign_readings = runtime.foreign_readings.saturating_add(1);
            if runtime.foreign_readings < FOREIGN_READINGS {
                return Vec::new();
            }
            runtime.foreign_readings = 0;
            runtime.held_at_cap = None;
            *hold = None;
        }
    }
    let supported = snapshot_supports(snapshot);
    if hold.is_none() && (!supported || !config.enabled) {
        runtime.state = if supported {
            ChargeCapState::Off
        } else {
            ChargeCapState::Unsupported
        };
        runtime.readings_at_cap = 0;
        return Vec::new();
    }

    // Only a fresh read taken after the last write says anything about the
    // register; anything else leaves the state as it was.
    let Some(mode) = fresh_pause_mode(snapshot) else {
        // With no cap pause to release, say why nothing will start rather
        // than sit at "charging normally". A record keeps its state: the
        // pause may still be on, and a read will settle it.
        if hold.is_none() && supported && config.enabled && snapshot.battery_pause_read_failing {
            runtime.state = ChargeCapState::Unreadable;
            runtime.readings_at_cap = 0;
        }
        return Vec::new();
    };
    if snapshot.soc > 100 || runtime.last_write_at == Some(snapshot.timestamp) {
        return Vec::new();
    }
    let window = (
        snapshot.battery_pause_slot_start_raw,
        snapshot.battery_pause_slot_end_raw,
    );

    if let Some(record) = hold.clone() {
        // A cap raised above the SOC since the pause began lets it charge.
        let raised = runtime
            .held_at_cap
            .is_some_and(|held| config.cap_soc > held && snapshot.soc < config.cap_soc);
        let keep_holding = supported
            && config.enabled
            && !context.wants_charge
            && !raised
            && snapshot.soc > config.resume_soc;
        match classify(&record, mode, window) {
            Register::CapPause => {
                if keep_holding {
                    runtime.state = ChargeCapState::Holding;
                    runtime.held_at_cap = Some(config.cap_soc);
                    return Vec::new();
                }
                // Another owner has saved this pause and lifts or restores
                // it itself (calibration lifts the mode alone): putting the
                // old window back now would turn its later restore into a
                // pause that isn't the cap's.
                if context.other_pause_owner {
                    runtime.state = standing_aside(context);
                    return Vec::new();
                }
                return release(snapshot, &record, runtime);
            }
            Register::HalfWritten => {
                if context.other_pause_owner {
                    runtime.state = standing_aside(context);
                    return Vec::new();
                }
                if keep_holding {
                    runtime.state = ChargeCapState::Holding;
                    runtime.held_at_cap = Some(config.cap_soc);
                    runtime.last_write_at = Some(snapshot.timestamp);
                    return hold_writes(record.mode_only);
                }
                return release(snapshot, &record, runtime);
            }
            Register::Clear => {
                if context.other_pause_owner {
                    runtime.state = standing_aside(context);
                    return Vec::new();
                }
                // The cap's pause is gone and nobody can put it back. A new
                // hold needs its own readings at the cap, as after a release.
                *hold = None;
                runtime.held_at_cap = None;
                runtime.readings_at_cap = 0;
            }
            Register::Other => {
                runtime.state = ChargeCapState::Blocked;
                runtime.readings_at_cap = 0;
                return Vec::new();
            }
        }
    }

    // No pause of the cap's own on the inverter.
    if !supported {
        runtime.state = ChargeCapState::Unsupported;
        runtime.readings_at_cap = 0;
        return Vec::new();
    }
    if !config.enabled {
        runtime.state = ChargeCapState::Off;
        runtime.readings_at_cap = 0;
        return Vec::new();
    }
    if mode != PAUSE_NONE || context.other_pause_owner {
        runtime.state = ChargeCapState::Blocked;
        runtime.readings_at_cap = 0;
        return Vec::new();
    }
    if context.wants_charge {
        runtime.state = ChargeCapState::Yielding;
        runtime.readings_at_cap = 0;
        return Vec::new();
    }
    if snapshot.soc < config.cap_soc {
        runtime.state = ChargeCapState::Waiting;
        runtime.readings_at_cap = 0;
        return Vec::new();
    }
    // A record needs an inverter to belong to: saved under an empty serial
    // it would later look foreign and be dropped with the cap's pause still
    // on (seventh code review #8).
    if snapshot.inverter_serial.is_empty() {
        runtime.state = ChargeCapState::Pending;
        runtime.readings_at_cap = 0;
        return Vec::new();
    }
    runtime.readings_at_cap = runtime.readings_at_cap.saturating_add(1);
    if runtime.readings_at_cap < ENGAGE_READINGS {
        runtime.state = ChargeCapState::Pending;
        return Vec::new();
    }
    let mode_only = pauses_with_mode_alone(snapshot.device_type, arm_fw(snapshot));
    let (prior_slot_start, prior_slot_end) = if mode_only {
        (0, 0)
    } else {
        // Without the old window there'd be nothing to put back.
        let (Some(start), Some(end)) = window else {
            runtime.state = ChargeCapState::Pending;
            return Vec::new();
        };
        (start, end)
    };
    *hold = Some(ChargeCapHold {
        inverter_serial: snapshot.inverter_serial.clone(),
        device_type_code: snapshot.device_type_code.clone(),
        prior_slot_start,
        prior_slot_end,
        mode_only,
    });
    runtime.state = ChargeCapState::Holding;
    runtime.held_at_cap = Some(config.cap_soc);
    runtime.last_write_at = Some(snapshot.timestamp);
    hold_writes(mode_only)
}

/// What the pause registers show, seen from a hold record.
enum Register {
    /// Mode 1 with the cap's window.
    CapPause,
    /// Mode 0 with a window made only of the cap's and the old window's
    /// halves, not yet the old one: a hold or a release only partly landed,
    /// or another owner put back the mode alone.
    HalfWritten,
    /// Mode 0 with the old window back, or a window that is neither: nothing
    /// of the cap's is left to undo.
    Clear,
    /// Someone else's pause.
    Other,
}

fn classify(record: &ChargeCapHold, mode: u16, window: (Option<u16>, Option<u16>)) -> Register {
    // Paused by the mode alone: whatever the window reads isn't the cap's.
    if record.mode_only {
        return match mode {
            PAUSE_CHARGE => Register::CapPause,
            PAUSE_NONE => Register::Clear,
            _ => Register::Other,
        };
    }
    let (Some(start), Some(end)) = window else {
        return Register::Other;
    };
    let prior = (record.prior_slot_start, record.prior_slot_end);
    let cap = (CAP_WINDOW_START, CAP_WINDOW_END);
    let halves_of_ours = (start == cap.0 || start == prior.0) && (end == cap.1 || end == prior.1);
    match mode {
        PAUSE_CHARGE if (start, end) == cap => Register::CapPause,
        PAUSE_NONE if (start, end) != prior && halves_of_ours => Register::HalfWritten,
        PAUSE_NONE => Register::Clear,
        _ => Register::Other,
    }
}

fn standing_aside(context: ChargeCapContext) -> ChargeCapState {
    if context.wants_charge {
        ChargeCapState::Yielding
    } else {
        ChargeCapState::Blocked
    }
}

fn release(
    snapshot: &InverterSnapshot,
    record: &ChargeCapHold,
    runtime: &mut ChargeCapRuntime,
) -> Vec<RegisterWrite> {
    runtime.state = ChargeCapState::Releasing;
    runtime.readings_at_cap = 0;
    runtime.held_at_cap = None;
    runtime.last_write_at = Some(snapshot.timestamp);
    release_writes(record)
}

/// Writes that hand the register back: mode 0 first, then the old window.
/// Once the mode is 0 nothing is paused whatever the window says, so a
/// release that only partly lands reads back as the cap's own half-done
/// work and is finished on a later read. (A hold goes the other way round,
/// window before mode, so no pause ever runs with the wrong window.) A
/// pause set by the mode alone is released by the mode alone.
pub fn release_writes(record: &ChargeCapHold) -> Vec<RegisterWrite> {
    let mode = RegisterWrite {
        address: HR_BATTERY_PAUSE_MODE,
        value: PAUSE_NONE,
    };
    if record.mode_only {
        return vec![mode];
    }
    vec![
        mode,
        RegisterWrite {
            address: HR_BATTERY_PAUSE_SLOT_1_START,
            value: record.prior_slot_start,
        },
        RegisterWrite {
            address: HR_BATTERY_PAUSE_SLOT_1_END,
            value: record.prior_slot_end,
        },
    ]
}

fn hold_writes(mode_only: bool) -> Vec<RegisterWrite> {
    let mode = RegisterWrite {
        address: HR_BATTERY_PAUSE_MODE,
        value: PAUSE_CHARGE,
    };
    if mode_only {
        return vec![mode];
    }
    vec![
        RegisterWrite {
            address: HR_BATTERY_PAUSE_SLOT_1_START,
            value: CAP_WINDOW_START,
        },
        RegisterWrite {
            address: HR_BATTERY_PAUSE_SLOT_1_END,
            value: CAP_WINDOW_END,
        },
        mode,
    ]
}

/// Whether a hold record was captured on the snapshot's inverter. The serial
/// identifies it, as for the Force baselines and the dispatch hold's capture:
/// a same-serial code change is still this inverter and must keep its release.
pub fn hold_matches_inverter(hold: &ChargeCapHold, snapshot: &InverterSnapshot) -> bool {
    hold.inverter_serial == snapshot.inverter_serial
}

/// HR 318 from this poll's own read, or `None` when it's stale or unread.
fn fresh_pause_mode(snapshot: &InverterSnapshot) -> Option<u16> {
    if snapshot.battery_pause_registers_observed_at != Some(snapshot.timestamp) {
        return None;
    }
    snapshot.battery_pause_mode_raw
}

#[cfg(test)]
mod tests {
    use super::*;

    const TS: i64 = 1_800_000_000;

    fn enabled() -> ChargeCapConfig {
        ChargeCapConfig {
            enabled: true,
            cap_soc: 95,
            resume_soc: 92,
        }
    }

    /// A single-phase AIO (pause mode plus window) with a fresh pause read.
    fn aio(soc: u8, mode: u16, start: u16, end: u16) -> InverterSnapshot {
        let mut snap = InverterSnapshot {
            device_type: DeviceType::AllInOne6kW,
            device_type_code: "8001".into(),
            inverter_serial: "SA1234".into(),
            firmware_version: "300".into(),
            soc,
            timestamp: TS,
            ..Default::default()
        };
        snap.battery_pause_mode_raw = Some(mode);
        snap.battery_pause_slot_start_raw = Some(start);
        snap.battery_pause_slot_end_raw = Some(end);
        snap.battery_pause_registers_observed_at = Some(TS);
        snap
    }

    /// An AC-coupled AC3: HR 318 only, no window.
    fn ac_coupled(soc: u8, mode: u16) -> InverterSnapshot {
        let mut snap = InverterSnapshot {
            device_type: DeviceType::ACCoupled,
            device_type_code: "3001".into(),
            inverter_serial: "CE5678".into(),
            firmware_version: "200".into(),
            soc,
            timestamp: TS,
            ..Default::default()
        };
        snap.battery_pause_mode_raw = Some(mode);
        snap.battery_pause_registers_observed_at = Some(TS);
        snap
    }

    fn later(mut snap: InverterSnapshot, secs: i64) -> InverterSnapshot {
        snap.timestamp += secs;
        snap.battery_pause_registers_observed_at = Some(snap.timestamp);
        snap
    }

    fn aio_hold(start: u16, end: u16) -> ChargeCapHold {
        ChargeCapHold {
            inverter_serial: "SA1234".into(),
            device_type_code: "8001".into(),
            prior_slot_start: start,
            prior_slot_end: end,
            mode_only: false,
        }
    }

    fn ac_hold() -> ChargeCapHold {
        ChargeCapHold {
            inverter_serial: "CE5678".into(),
            device_type_code: "3001".into(),
            prior_slot_start: 0,
            prior_slot_end: 0,
            mode_only: true,
        }
    }

    fn w(address: u16, value: u16) -> RegisterWrite {
        RegisterWrite { address, value }
    }

    fn hold_writes() -> Vec<RegisterWrite> {
        vec![
            w(HR_BATTERY_PAUSE_SLOT_1_START, CAP_WINDOW_START),
            w(HR_BATTERY_PAUSE_SLOT_1_END, CAP_WINDOW_END),
            w(HR_BATTERY_PAUSE_MODE, PAUSE_CHARGE),
        ]
    }

    fn run(
        snap: &InverterSnapshot,
        config: &ChargeCapConfig,
        context: ChargeCapContext,
        runtime: &mut ChargeCapRuntime,
        hold: &mut Option<ChargeCapHold>,
    ) -> Vec<RegisterWrite> {
        evaluate(snap, config, context, runtime, hold)
    }

    /// Engage the cap on an AIO whose pause window was `prior`.
    fn engaged(prior: (u16, u16)) -> (ChargeCapRuntime, Option<ChargeCapHold>) {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        let snap = aio(96, 0, prior.0, prior.1);
        run(
            &snap,
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        let writes = run(
            &later(snap, 10),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, hold_writes());
        (runtime, hold)
    }

    // -- Pause that can't be read back (issue #433) ---------------------

    /// A Gen1 whose HR 318 read keeps failing (the old 4xx firmware): no
    /// fresh pause read, and the poll loop has stopped retrying every poll.
    fn unreadable_gen1(soc: u8) -> InverterSnapshot {
        let mut snap = InverterSnapshot {
            device_type: DeviceType::Gen1Hybrid,
            device_type_code: "2001".into(),
            inverter_serial: "CE9999".into(),
            firmware_version: "252".into(),
            soc,
            timestamp: TS,
            ..Default::default()
        };
        snap.battery_pause_read_failing = true;
        snap
    }

    #[test]
    fn an_unreadable_pause_says_so_instead_of_waiting() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        let writes = run(
            &unreadable_gen1(99),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty(), "the cap never starts blind");
        assert!(hold.is_none());
        assert_eq!(runtime.state, ChargeCapState::Unreadable);
        assert_eq!(runtime.state.api_name(), "unreadable");
    }

    #[test]
    fn a_cap_that_is_off_never_says_it_cant_read_the_pause() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        let off = ChargeCapConfig {
            enabled: false,
            ..enabled()
        };
        run(
            &unreadable_gen1(99),
            &off,
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(runtime.state, ChargeCapState::Off);

        // Switched on, then off again before the next poll: shown off.
        let shared = ChargeCapShared {
            config: off,
            runtime: ChargeCapRuntime {
                state: ChargeCapState::Unreadable,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            shown_state(&shared, true),
            ChargeCapState::Off,
            "a cap that is off never says it can't read the pause"
        );
    }

    #[test]
    fn a_pause_that_reads_again_lets_the_cap_start() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        run(
            &unreadable_gen1(90),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(runtime.state, ChargeCapState::Unreadable);

        let mut readable = unreadable_gen1(90);
        readable.battery_pause_read_failing = false;
        readable.battery_pause_mode_raw = Some(PAUSE_NONE);
        readable.battery_pause_registers_observed_at = Some(TS);
        run(
            &readable,
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(runtime.state, ChargeCapState::Waiting);
    }

    #[test]
    fn an_unreadable_pause_does_not_hide_a_hold_that_is_still_on() {
        // The record says the cap's pause may be on the inverter, so the
        // state stays as it was until a read can release it.
        let mut runtime = ChargeCapRuntime {
            state: ChargeCapState::Holding,
            ..Default::default()
        };
        let mut hold = Some(ChargeCapHold {
            inverter_serial: "CE9999".into(),
            device_type_code: "2001".into(),
            prior_slot_start: 0,
            prior_slot_end: 0,
            mode_only: true,
        });
        let writes = run(
            &unreadable_gen1(96),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert!(hold.is_some());
        assert_eq!(runtime.state, ChargeCapState::Holding);
    }

    #[test]
    fn unreadable_stays_until_a_read_arrives_or_the_cap_is_off() {
        // A reconnect restarts the poll loop's failure count, so the flag
        // drops while the pause is still unread. The cap keeps saying it
        // can't read until a read lands, rather than flipping to "waiting"
        // on a register it has never seen.
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        run(
            &unreadable_gen1(90),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        let mut reconnected = unreadable_gen1(90);
        reconnected.battery_pause_read_failing = false;
        let writes = run(
            &reconnected,
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(runtime.state, ChargeCapState::Unreadable);

        // Switching the cap off clears it at once.
        let off = ChargeCapConfig {
            enabled: false,
            ..enabled()
        };
        run(
            &reconnected,
            &off,
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(runtime.state, ChargeCapState::Off);
    }

    // -- Support --------------------------------------------------------

    #[test]
    fn supports_the_models_that_pause_charging_natively() {
        assert!(supports(DeviceType::AllInOne6kW, 0));
        assert!(supports(DeviceType::AllInOne3_6kW, 0));
        assert!(supports(DeviceType::AllInOne5kW, 0));
        // The single-phase HV models, on GivTCP's pause list (issue #432).
        assert!(supports(DeviceType::HybridHvGen3, 612));
        assert!(supports(DeviceType::AllInOneHybrid, 612));
    }

    /// Aligned with GivTCP, whose users have tested the pause far more
    /// widely: the single-phase AC (and its Mk2) and the Gen1 take the pause
    /// mode alone from their ARM floors, and the three-phase models have no
    /// pause at all.
    #[test]
    fn supports_the_mode_only_models_from_their_arm_floors() {
        for (device, floor) in [
            (DeviceType::ACCoupled, 200),
            (DeviceType::ACCoupledMk2, 200),
            (DeviceType::Gen1Hybrid, 187),
        ] {
            assert!(supports(device, floor), "{device:?}");
            assert!(supports(device, 999), "{device:?}");
            assert!(!supports(device, floor - 1), "{device:?}");
            assert!(!supports(device, 0), "{device:?}");
            assert!(pauses_with_mode_alone(device, floor), "{device:?}");
        }
        assert!(!supports(DeviceType::ACThreePhase, 999));
    }

    /// The Gen2 and the Gen3 pause with the window on every firmware, as in
    /// GivTCP (issue #433).
    #[test]
    fn supports_the_gen2_and_gen3_on_every_firmware() {
        for device in [DeviceType::Gen2Hybrid, DeviceType::Gen3Hybrid] {
            for firmware in [0, 311, 312, 852] {
                assert!(supports(device, firmware), "{device:?} ARM {firmware}");
                assert!(!pauses_with_mode_alone(device, firmware));
            }
        }
    }

    #[test]
    fn hides_the_cap_where_the_pause_is_missing() {
        for device in [
            DeviceType::Gateway,
            DeviceType::ThreePhase,
            DeviceType::Ems,
            DeviceType::PvInverter,
            DeviceType::Gen4Hybrid,
            DeviceType::Unknown(0x4101),
        ] {
            assert!(!supports(device, 999), "{device:?}");
        }
    }

    // -- Validation -----------------------------------------------------

    #[test]
    fn default_config_is_off_and_valid() {
        let config = ChargeCapConfig::default();
        assert!(!config.enabled);
        assert_eq!((config.cap_soc, config.resume_soc), (95, 92));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validation_rejects_out_of_range_levels() {
        let bad = |cap_soc, resume_soc| {
            ChargeCapConfig {
                enabled: true,
                cap_soc,
                resume_soc,
            }
            .validate()
            .is_err()
        };
        assert!(bad(49, 40));
        assert!(bad(100, 90));
        assert!(bad(95, 39));
        assert!(bad(95, 94));
        assert!(bad(95, 95));
        assert!(bad(95, 96));
        assert!(!bad(95, 93));
        assert!(!bad(50, 40));
        assert!(!bad(99, 97));
    }

    #[test]
    fn config_missing_fields_take_defaults() {
        let config: ChargeCapConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config, ChargeCapConfig::default());
    }

    // -- Engaging -------------------------------------------------------

    #[test]
    fn off_and_holding_nothing_does_nothing() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        let writes = run(
            &aio(100, 0, 0, 0),
            &ChargeCapConfig::default(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(runtime.state, ChargeCapState::Off);
        assert!(hold.is_none());
    }

    #[test]
    fn below_the_cap_waits() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        let writes = run(
            &aio(94, 0, 0, 0),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(runtime.state, ChargeCapState::Waiting);
    }

    #[test]
    fn pauses_charging_after_two_readings_at_the_cap_and_records_the_old_window() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        let snap = aio(95, 0, 1600, 1900);
        let first = run(
            &snap,
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(first.is_empty(), "one reading is not enough");
        assert_eq!(runtime.state, ChargeCapState::Pending);
        assert!(hold.is_none());

        let second = run(
            &later(snap, 10),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(second, hold_writes());
        assert_eq!(runtime.state, ChargeCapState::Holding);
        assert_eq!(hold, Some(aio_hold(1600, 1900)));
    }

    #[test]
    fn a_reading_below_the_cap_resets_the_count() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        let snap = aio(95, 0, 0, 0);
        run(
            &snap,
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        run(
            &later(aio(94, 0, 0, 0), 10),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        let writes = run(
            &later(snap, 20),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(runtime.state, ChargeCapState::Pending);
    }

    /// Two reads at the cap on an AC-coupled inverter: the cap pauses with
    /// HR 318 alone and records that it did.
    fn ac_engaged() -> (ChargeCapRuntime, Option<ChargeCapHold>) {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        run(
            &ac_coupled(97, 0),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        let writes = run(
            &later(ac_coupled(97, 0), 10),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, vec![w(HR_BATTERY_PAUSE_MODE, PAUSE_CHARGE)]);
        (runtime, hold)
    }

    #[test]
    fn an_ac_coupled_inverter_is_paused_with_the_mode_alone() {
        let (runtime, hold) = ac_engaged();
        assert_eq!(hold, Some(ac_hold()));
        assert_eq!(runtime.state, ChargeCapState::Holding);
    }

    #[test]
    fn an_ac_coupled_pause_is_held_whatever_the_window_reads() {
        let (mut runtime, mut hold) = ac_engaged();
        let mut snap = later(ac_coupled(96, PAUSE_CHARGE), 20);
        snap.battery_pause_slot_start_raw = Some(1234);
        snap.battery_pause_slot_end_raw = Some(4321);
        let writes = run(
            &snap,
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(runtime.state, ChargeCapState::Holding);
        assert_eq!(hold, Some(ac_hold()));
    }

    #[test]
    fn an_ac_coupled_pause_is_released_with_the_mode_alone() {
        let (mut runtime, mut hold) = ac_engaged();
        let writes = run(
            &later(ac_coupled(92, PAUSE_CHARGE), 20),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, vec![w(HR_BATTERY_PAUSE_MODE, PAUSE_NONE)]);
        assert_eq!(runtime.state, ChargeCapState::Releasing);
        let writes = run(
            &later(ac_coupled(91, PAUSE_NONE), 40),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(hold, None, "mode 0 read back: the release is done");
    }

    #[test]
    fn an_ac_coupled_pause_of_someone_elses_blocks_the_cap() {
        let (mut runtime, mut hold) = ac_engaged();
        let writes = run(
            &later(ac_coupled(97, 2), 20),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(runtime.state, ChargeCapState::Blocked);
        assert_eq!(
            hold,
            Some(ac_hold()),
            "kept for when the cap's pause is back"
        );
    }

    #[test]
    fn an_ac_coupled_inverter_below_arm_200_is_never_paused() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        let mut snap = ac_coupled(97, 0);
        snap.firmware_version = "199".into();
        for step in 0..3 {
            let writes = run(
                &later(snap.clone(), step * 10),
                &enabled(),
                Default::default(),
                &mut runtime,
                &mut hold,
            );
            assert!(writes.is_empty(), "step {step}");
        }
        assert_eq!(hold, None);
        assert_eq!(runtime.state, ChargeCapState::Unsupported);
    }

    #[test]
    fn a_mode_only_hold_is_released_with_the_mode_alone() {
        assert_eq!(
            super::release_writes(&ac_hold()),
            vec![w(HR_BATTERY_PAUSE_MODE, PAUSE_NONE)]
        );
    }

    #[test]
    fn never_pauses_over_another_pause() {
        for mode in [1, 2, 3] {
            let mut runtime = ChargeCapRuntime::default();
            let mut hold = None;
            let snap = aio(99, mode, 1600, 1900);
            run(
                &snap,
                &enabled(),
                Default::default(),
                &mut runtime,
                &mut hold,
            );
            let writes = run(
                &later(snap, 10),
                &enabled(),
                Default::default(),
                &mut runtime,
                &mut hold,
            );
            assert!(writes.is_empty(), "mode {mode}");
            assert_eq!(runtime.state, ChargeCapState::Blocked);
            assert!(hold.is_none());
        }
    }

    #[test]
    fn does_not_pause_while_something_wants_to_charge() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        let context = ChargeCapContext {
            wants_charge: true,
            other_pause_owner: false,
        };
        let snap = aio(99, 0, 0, 0);
        run(&snap, &enabled(), context, &mut runtime, &mut hold);
        let writes = run(
            &later(snap, 10),
            &enabled(),
            context,
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(runtime.state, ChargeCapState::Yielding);
    }

    #[test]
    fn stale_pause_registers_never_cause_a_write() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        let mut snap = aio(99, 0, 0, 0);
        snap.battery_pause_registers_observed_at = Some(TS - 10);
        run(
            &snap,
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        let mut next = snap.clone();
        next.timestamp += 10;
        let writes = run(
            &next,
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert!(hold.is_none());
    }

    #[test]
    fn unread_pause_registers_never_cause_a_write() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        let mut snap = aio(99, 0, 0, 0);
        snap.battery_pause_mode_raw = None;
        run(
            &snap,
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        let writes = run(
            &later(snap, 10),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
    }

    #[test]
    fn an_unsupported_inverter_is_never_written() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        let mut snap = aio(99, 0, 0, 0);
        snap.device_type = DeviceType::Gateway;
        run(
            &snap,
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        let writes = run(
            &later(snap, 10),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(runtime.state, ChargeCapState::Unsupported);
    }

    // -- Holding --------------------------------------------------------

    #[test]
    fn holding_reads_back_without_writing_again() {
        let (mut runtime, mut hold) = engaged((0, 0));
        let writes = run(
            &later(aio(96, 1, CAP_WINDOW_START, CAP_WINDOW_END), 20),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(runtime.state, ChargeCapState::Holding);
    }

    #[test]
    fn a_hold_write_that_did_not_land_is_retried_on_a_later_read() {
        let (mut runtime, mut hold) = engaged((0, 0));
        // The very poll that wrote (TS + 10) can't confirm it.
        let same = run(
            &later(aio(96, 0, 0, 0), 10),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(same.is_empty(), "no repeat before a later read");
        // A write that didn't land reads like a pause someone else cleared,
        // so the cap starts again from fresh readings at the cap.
        let first = run(
            &later(aio(96, 0, 0, 0), 20),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(first.is_empty(), "one fresh reading isn't enough");
        let retry = run(
            &later(aio(96, 0, 0, 0), 30),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(retry, hold_writes());
        assert_eq!(hold, Some(aio_hold(0, 0)), "the old window is kept");
    }

    #[test]
    fn a_clock_stepped_back_still_counts_as_a_later_read() {
        let (mut runtime, mut hold) = engaged((1600, 1900));
        // NTP pulled the clock back an hour; this read still follows the write.
        let mut held = aio(90, 1, CAP_WINDOW_START, CAP_WINDOW_END);
        held.timestamp = TS - 3600;
        held.battery_pause_registers_observed_at = Some(held.timestamp);
        let writes = run(
            &held,
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, release_writes(1600, 1900));
    }

    #[test]
    fn stays_paused_between_the_resume_level_and_the_cap() {
        let (mut runtime, mut hold) = engaged((0, 0));
        let writes = run(
            &later(aio(93, 1, CAP_WINDOW_START, CAP_WINDOW_END), 20),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(runtime.state, ChargeCapState::Holding);
    }

    // -- Releasing ------------------------------------------------------

    /// Mode first: once it's 0 nothing is paused, whatever the window says,
    /// so a release that only partly lands still reads as the cap's own
    /// half-done work and is finished on a later read.
    fn release_writes(start: u16, end: u16) -> Vec<RegisterWrite> {
        vec![
            w(HR_BATTERY_PAUSE_MODE, PAUSE_NONE),
            w(HR_BATTERY_PAUSE_SLOT_1_START, start),
            w(HR_BATTERY_PAUSE_SLOT_1_END, end),
        ]
    }

    #[test]
    fn a_release_whose_window_only_half_landed_is_finished() {
        let (mut runtime, mut hold) = engaged((1600, 1900));
        // Mode 0 and the start landed; the end is still the cap's.
        let writes = run(
            &later(aio(92, 0, 1600, CAP_WINDOW_END), 30),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, release_writes(1600, 1900));
        assert_eq!(hold, Some(aio_hold(1600, 1900)), "the old window is kept");
    }

    #[test]
    fn an_engage_whose_window_only_half_landed_keeps_the_old_window() {
        let (mut runtime, mut hold) = engaged((1600, 1900));
        // The start landed, the end and the mode didn't.
        let writes = run(
            &later(aio(96, 0, CAP_WINDOW_START, 1900), 20),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, hold_writes(), "the hold is finished");
        assert_eq!(hold, Some(aio_hold(1600, 1900)), "the old window is kept");
    }

    #[test]
    fn mode_zero_with_a_window_that_is_neither_the_caps_nor_the_old_one_is_clear() {
        let (mut runtime, mut hold) = engaged((1600, 1900));
        // Someone set their own window since; nothing of the cap's is left.
        let writes = run(
            &later(aio(92, 0, 1000, 1100), 30),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert!(hold.is_none());
    }

    #[test]
    fn raising_the_cap_while_holding_lets_the_battery_charge_to_it() {
        let (mut runtime, mut hold) = engaged((0, 0));
        let raised = ChargeCapConfig {
            cap_soc: 99,
            ..enabled()
        };
        let writes = run(
            &later(aio(95, 1, CAP_WINDOW_START, CAP_WINDOW_END), 20),
            &raised,
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, release_writes(0, 0));
    }

    #[test]
    fn lowering_the_cap_while_holding_keeps_holding() {
        let (mut runtime, mut hold) = engaged((0, 0));
        let lowered = ChargeCapConfig {
            cap_soc: 90,
            resume_soc: 85,
            enabled: true,
        };
        let writes = run(
            &later(aio(95, 1, CAP_WINDOW_START, CAP_WINDOW_END), 20),
            &lowered,
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(runtime.state, ChargeCapState::Holding);
        // Raising it back above the SOC now lets it charge.
        let writes = run(
            &later(aio(93, 1, CAP_WINDOW_START, CAP_WINDOW_END), 30),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, release_writes(0, 0));
    }

    #[test]
    fn a_one_off_serial_misread_keeps_the_record() {
        let (mut runtime, mut hold) = engaged((1600, 1900));
        let garbled = InverterSnapshot {
            inverter_serial: "S\u{1}1234".into(),
            ..later(aio(96, 1, CAP_WINDOW_START, CAP_WINDOW_END), 20)
        };
        let writes = run(
            &garbled,
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(hold, Some(aio_hold(1600, 1900)));
        let writes = run(
            &later(aio(96, 1, CAP_WINDOW_START, CAP_WINDOW_END), 30),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(runtime.state, ChargeCapState::Holding);
        assert!(hold.is_some());
    }

    #[test]
    fn resumes_charging_at_the_resume_level_and_puts_the_window_back() {
        let (mut runtime, mut hold) = engaged((1600, 1900));
        let writes = run(
            &later(aio(92, 1, CAP_WINDOW_START, CAP_WINDOW_END), 20),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, release_writes(1600, 1900));
        assert_eq!(runtime.state, ChargeCapState::Releasing);
        assert!(hold.is_some(), "kept until read-back confirms");
    }

    #[test]
    fn a_confirmed_release_drops_the_record() {
        let (mut runtime, mut hold) = engaged((1600, 1900));
        run(
            &later(aio(92, 1, CAP_WINDOW_START, CAP_WINDOW_END), 20),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        let writes = run(
            &later(aio(92, 0, 1600, 1900), 30),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert!(hold.is_none());
        assert_eq!(runtime.state, ChargeCapState::Waiting);
    }

    #[test]
    fn a_release_that_left_the_cap_window_puts_the_old_window_back() {
        let (mut runtime, mut hold) = engaged((1600, 1900));
        // Mode 0 landed, the window didn't.
        let writes = run(
            &later(aio(92, 0, CAP_WINDOW_START, CAP_WINDOW_END), 30),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, release_writes(1600, 1900));
        assert!(hold.is_some());
    }

    #[test]
    fn switching_off_releases_the_cap_pause() {
        let (mut runtime, mut hold) = engaged((0, 0));
        let writes = run(
            &later(aio(99, 1, CAP_WINDOW_START, CAP_WINDOW_END), 20),
            &ChargeCapConfig::default(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, release_writes(0, 0));
        assert_eq!(runtime.state, ChargeCapState::Releasing);
    }

    #[test]
    fn lets_go_when_a_charge_slot_or_calibration_wants_to_charge() {
        let (mut runtime, mut hold) = engaged((0, 0));
        let context = ChargeCapContext {
            wants_charge: true,
            other_pause_owner: false,
        };
        let writes = run(
            &later(aio(99, 1, CAP_WINDOW_START, CAP_WINDOW_END), 20),
            &enabled(),
            context,
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, release_writes(0, 0));
        let confirmed = run(
            &later(aio(99, 0, 0, 0), 30),
            &enabled(),
            context,
            &mut runtime,
            &mut hold,
        );
        assert!(confirmed.is_empty());
        assert!(hold.is_none());
        assert_eq!(runtime.state, ChargeCapState::Yielding);
    }

    #[test]
    fn pauses_again_once_the_charge_slot_ends_if_still_at_the_cap() {
        let mut runtime = ChargeCapRuntime {
            state: ChargeCapState::Yielding,
            ..Default::default()
        };
        let mut hold = None;
        let snap = aio(99, 0, 0, 0);
        run(
            &snap,
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        let writes = run(
            &later(snap, 10),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, hold_writes());
    }

    #[test]
    fn a_failed_release_is_retried_on_a_later_read() {
        let (mut runtime, mut hold) = engaged((0, 0));
        let held = aio(92, 1, CAP_WINDOW_START, CAP_WINDOW_END);
        run(
            &later(held.clone(), 20),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        let retry = run(
            &later(held, 30),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(retry, release_writes(0, 0));
    }

    // -- Sharing HR 318 -------------------------------------------------

    #[test]
    fn stands_aside_while_another_pause_owns_the_register_and_keeps_its_record() {
        let (mut runtime, mut hold) = engaged((0, 0));
        // A remote pause-discharge replaced the cap's pause.
        let writes = run(
            &later(aio(99, 2, 1700, 1800), 20),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(runtime.state, ChargeCapState::Blocked);
        assert!(hold.is_some());
    }

    #[test]
    fn releases_its_pause_when_another_owner_puts_it_back_later() {
        let (mut runtime, mut hold) = engaged((0, 0));
        run(
            &later(aio(99, 2, 1700, 1800), 20),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        // The other owner restored what it saved: the cap's own pause.
        let writes = run(
            &later(aio(80, 1, CAP_WINDOW_START, CAP_WINDOW_END), 30),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, release_writes(0, 0));
    }

    #[test]
    fn keeps_its_record_through_mode_zero_while_another_owner_may_restore_it() {
        let (mut runtime, mut hold) = engaged((0, 0));
        // Calibration lifted the pause and will put the cap's back at the end.
        let context = ChargeCapContext {
            wants_charge: true,
            other_pause_owner: true,
        };
        let writes = run(
            &later(aio(50, 0, CAP_WINDOW_START, CAP_WINDOW_END), 20),
            &enabled(),
            context,
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty(), "the other owner restores the window");
        assert!(hold.is_some());
    }

    #[test]
    fn a_calibration_lifts_and_restores_the_caps_pause_without_the_cap_rewriting_it() {
        // Calibration captures HR 318 (mode only), lifts it, and writes the
        // captured mode back at the end. If the cap put its old window back
        // meanwhile, that restore would leave mode 1 with the old window:
        // not the cap's pause any more, so it would never be released.
        let (mut runtime, mut hold) = engaged((1600, 1900));
        let calibrating = ChargeCapContext {
            wants_charge: true,
            other_pause_owner: true,
        };
        let held = |soc, secs| later(aio(soc, 1, CAP_WINDOW_START, CAP_WINDOW_END), secs);
        let lifted = |soc, secs| later(aio(soc, 0, CAP_WINDOW_START, CAP_WINDOW_END), secs);

        let writes = run(
            &held(96, 20),
            &enabled(),
            calibrating,
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty(), "calibration lifts the pause itself");
        assert_eq!(runtime.state, ChargeCapState::Yielding);
        let writes = run(
            &lifted(50, 30),
            &enabled(),
            calibrating,
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert!(hold.is_some());

        // Restored at the end with the cap's window intact: the cap's own
        // pause again, released at the resume level as usual.
        let writes = run(
            &held(90, 40),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, release_writes(1600, 1900));
    }

    #[test]
    fn force_charge_alone_still_makes_the_cap_let_go() {
        // Force Charge doesn't touch HR 318, so nobody else would lift it.
        let (mut runtime, mut hold) = engaged((0, 0));
        let force_charge = ChargeCapContext {
            wants_charge: true,
            other_pause_owner: false,
        };
        let writes = run(
            &later(aio(96, 1, CAP_WINDOW_START, CAP_WINDOW_END), 20),
            &enabled(),
            force_charge,
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, release_writes(0, 0));
    }

    #[test]
    fn without_a_record_a_matching_pause_is_not_the_caps() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        let writes = run(
            &aio(80, 1, CAP_WINDOW_START, CAP_WINDOW_END),
            &ChargeCapConfig::default(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(
            writes.is_empty(),
            "the user's own all-day pause is left alone"
        );
    }

    #[test]
    fn a_record_from_another_inverter_is_dropped_and_never_written() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = Some(ChargeCapHold {
            inverter_serial: "OTHER".into(),
            ..aio_hold(1600, 1900)
        });
        for step in 0..FOREIGN_READINGS as i64 {
            let writes = run(
                &later(aio(80, 1, CAP_WINDOW_START, CAP_WINDOW_END), step * 10),
                &enabled(),
                Default::default(),
                &mut runtime,
                &mut hold,
            );
            assert!(writes.is_empty(), "step {step}");
            if step + 1 < FOREIGN_READINGS as i64 {
                assert!(
                    hold.is_some(),
                    "kept through a possible misread at step {step}"
                );
            }
        }
        assert!(
            hold.is_none(),
            "dropped once another inverter is seen for sure"
        );
    }

    #[test]
    fn the_cap_never_engages_on_an_inverter_it_cannot_name() {
        // Seventh code review #8: a record saved under an empty serial was
        // later dropped as another inverter's, with the cap's pause still on.
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        for step in 0..(ENGAGE_READINGS as i64 + 2) {
            let mut snap = later(aio(96, 0, 1600, 1900), step * 10);
            snap.inverter_serial.clear();
            let writes = run(
                &snap,
                &enabled(),
                Default::default(),
                &mut runtime,
                &mut hold,
            );
            assert!(writes.is_empty(), "step {step}");
        }
        assert!(hold.is_none());
        assert_eq!(runtime.state, ChargeCapState::Pending);
        // Named again: it engages as normal.
        let named = |secs| later(aio(96, 0, 1600, 1900), secs);
        run(
            &named(100),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        let writes = run(
            &named(110),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(!writes.is_empty());
        assert_eq!(
            hold.as_ref().map(|h| h.inverter_serial.as_str()),
            Some("SA1234")
        );
    }

    #[test]
    fn a_record_survives_a_model_code_change_on_the_same_inverter() {
        // Second code review: the serial identifies the inverter, as for the
        // dispatch hold's capture and the Force baselines. A same-serial code
        // change used to drop the record and leave charging paused for good.
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = Some(aio_hold(1600, 1900));
        for step in 0..(FOREIGN_READINGS as i64 + 2) {
            let mut snap = later(aio(80, 1, CAP_WINDOW_START, CAP_WINDOW_END), step * 10);
            snap.device_type_code = "8002".into();
            run(
                &snap,
                &enabled(),
                Default::default(),
                &mut runtime,
                &mut hold,
            );
        }
        assert!(hold.is_some(), "kept for the same serial");
    }

    #[test]
    fn a_release_is_confirmed_when_the_old_window_was_already_all_day() {
        let (mut runtime, mut hold) = engaged((CAP_WINDOW_START, CAP_WINDOW_END));
        run(
            &later(aio(92, 1, CAP_WINDOW_START, CAP_WINDOW_END), 20),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        let writes = run(
            &later(aio(92, 0, CAP_WINDOW_START, CAP_WINDOW_END), 30),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty(), "nothing left to put back");
        assert!(hold.is_none());
    }

    #[test]
    fn does_not_start_while_another_owner_has_saved_the_register() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        let context = ChargeCapContext {
            wants_charge: false,
            other_pause_owner: true,
        };
        let snap = aio(99, 0, 0, 0);
        run(&snap, &enabled(), context, &mut runtime, &mut hold);
        let writes = run(
            &later(snap, 10),
            &enabled(),
            context,
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(runtime.state, ChargeCapState::Blocked);
        assert!(hold.is_none());
    }

    #[test]
    fn does_not_start_on_a_window_model_without_the_old_window() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = None;
        let mut snap = aio(99, 0, 0, 0);
        snap.battery_pause_slot_end_raw = None;
        run(
            &snap,
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        let writes = run(
            &later(snap, 10),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert!(hold.is_none());
    }

    #[test]
    fn an_invalid_soc_never_causes_a_write() {
        let (mut runtime, mut hold) = engaged((0, 0));
        let mut snap = later(aio(92, 1, CAP_WINDOW_START, CAP_WINDOW_END), 20);
        snap.soc = 255;
        let writes = run(
            &snap,
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert_eq!(runtime.state, ChargeCapState::Holding);
    }

    #[test]
    fn switching_off_mid_hold_on_a_stale_read_waits_for_a_fresh_one() {
        let (mut runtime, mut hold) = engaged((0, 0));
        let mut snap = later(aio(99, 1, CAP_WINDOW_START, CAP_WINDOW_END), 20);
        snap.battery_pause_registers_observed_at = Some(TS);
        let writes = run(
            &snap,
            &ChargeCapConfig::default(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty());
        assert!(hold.is_some(), "the record outlives a stale read");
    }

    #[test]
    fn a_restart_mid_hold_carries_on_from_the_record() {
        let mut runtime = ChargeCapRuntime::default();
        let mut hold = Some(aio_hold(1600, 1900));
        let writes = run(
            &aio(90, 1, CAP_WINDOW_START, CAP_WINDOW_END),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, release_writes(1600, 1900));
    }

    #[test]
    fn a_pause_someone_else_cleared_needs_fresh_readings_before_pausing_again() {
        let (mut runtime, mut hold) = engaged((1600, 1900));
        // Another tool puts the old window back and clears the mode: the
        // hold is over, and one reading at the cap mustn't start a new one.
        let writes = run(
            &later(aio(97, 0, 1600, 1900), 20),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert!(writes.is_empty(), "one reading isn't enough");
        assert_eq!(hold, None);
        assert_eq!(runtime.state, ChargeCapState::Pending);
        let writes = run(
            &later(aio(97, 0, 1600, 1900), 30),
            &enabled(),
            Default::default(),
            &mut runtime,
            &mut hold,
        );
        assert_eq!(writes, hold_writes(), "the second reading engages");
    }

    #[test]
    fn a_record_with_nothing_read_yet_shows_as_holding_not_off() {
        let shared = |on: bool| ChargeCapShared {
            config: ChargeCapConfig {
                enabled: on,
                ..enabled()
            },
            hold: Some(aio_hold(1600, 1900)),
            ..Default::default()
        };
        assert_eq!(shown_state(&shared(true), true), ChargeCapState::Holding);
        assert_eq!(shown_state(&shared(false), true), ChargeCapState::Releasing);
        // Without a record, a cap just switched on is waiting for its first read.
        let fresh = ChargeCapShared {
            config: enabled(),
            ..Default::default()
        };
        assert_eq!(shown_state(&fresh, true), ChargeCapState::Off);
    }

    // -- Poll-loop wrapper ----------------------------------------------

    use crate::inverter::model::ScheduleSlot;
    use crate::inverter::poll::AppState;
    use std::sync::Arc;

    async fn enabled_state() -> Arc<AppState> {
        let state = Arc::new(AppState::new());
        state.charge_cap.lock().await.config = enabled();
        state
    }

    #[tokio::test]
    async fn the_hold_record_is_saved_before_the_pause_is_written() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = enabled_state().await;
            let snap = aio(96, 0, 1600, 1900);
            assert!(advance(&state, &snap, Default::default()).await.is_empty());
            let writes = advance(&state, &later(snap, 10), Default::default()).await;
            assert_eq!(writes, hold_writes());
            assert_eq!(
                crate::settings::Settings::load().charge_cap_hold,
                Some(aio_hold(1600, 1900))
            );
            // And it's gone from disk once the release is confirmed.
            let released = advance(
                &state,
                &later(aio(90, 1, CAP_WINDOW_START, CAP_WINDOW_END), 20),
                Default::default(),
            )
            .await;
            assert_eq!(released, release_writes(1600, 1900));
            advance(
                &state,
                &later(aio(90, 0, 1600, 1900), 30),
                Default::default(),
            )
            .await;
            assert_eq!(crate::settings::Settings::load().charge_cap_hold, None);
        })
        .await;
    }

    #[tokio::test]
    async fn no_pause_is_written_when_the_record_cannot_be_saved() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = enabled_state().await;
            let snap = aio(96, 0, 1600, 1900);
            let ok = |_| async { Ok(()) };
            advance_with(&state, &snap, Default::default(), ok).await;
            let before = state.charge_cap.lock().await.clone();
            let writes = advance_with(&state, &later(snap, 10), Default::default(), |_| async {
                Err("disk full".to_string())
            })
            .await;
            assert!(writes.is_empty());
            assert_eq!(*state.charge_cap.lock().await, before, "nothing changed");
        })
        .await;
    }

    #[tokio::test]
    async fn a_record_that_cannot_be_cleared_is_still_dropped_in_memory() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = enabled_state().await;
            {
                let mut cap = state.charge_cap.lock().await;
                cap.hold = Some(aio_hold(1600, 1900));
                cap.runtime.state = ChargeCapState::Releasing;
            }
            let writes = advance_with(
                &state,
                &aio(90, 0, 1600, 1900),
                Default::default(),
                |_| async { Err("read-only".to_string()) },
            )
            .await;
            assert!(writes.is_empty());
            let cap = state.charge_cap.lock().await;
            assert_eq!(cap.hold, None);
            assert_eq!(cap.runtime.state, ChargeCapState::Waiting);
        })
        .await;
    }

    #[tokio::test]
    async fn a_stopped_cap_writes_nothing() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = enabled_state().await;
            state.charge_cap.lock().await.stopped = true;
            let snap = aio(99, 0, 0, 0);
            let ok = |_| async { Ok(()) };
            advance_with(&state, &snap, Default::default(), ok).await;
            let writes = advance_with(&state, &later(snap, 10), Default::default(), ok).await;
            assert!(writes.is_empty());
        })
        .await;
    }

    fn slot(start: (u8, u8), end: (u8, u8)) -> ScheduleSlot {
        ScheduleSlot {
            enabled: true,
            start_hour: start.0,
            start_minute: start.1,
            end_hour: end.0,
            end_minute: end.1,
            target_soc: 100,
        }
    }

    #[test]
    fn a_running_charge_slot_wants_charge() {
        let mut snap = aio(96, 0, 0, 0);
        snap.charge_slots[0] = slot((23, 30), (5, 30));
        snap.enable_charge = true;
        let at = |minute| context_for(&snap, minute, false, false, false, false, false);
        assert!(at(0).wants_charge, "overnight slot after midnight");
        assert!(at(23 * 60 + 45).wants_charge);
        assert!(!at(12 * 60).wants_charge, "outside the slot");
        assert!(!at(0).other_pause_owner);

        snap.enable_charge = false;
        assert!(
            !context_for(&snap, 0, false, false, false, false, false).wants_charge,
            "a slot with charging disabled doesn't charge"
        );
    }

    #[test]
    fn force_charge_and_calibration_want_charge_and_pause_owners_are_seen() {
        let snap = aio(96, 0, 0, 0);
        assert!(context_for(&snap, 0, false, true, false, false, false).wants_charge);
        let calibrating = context_for(&snap, 0, true, false, false, false, false);
        assert!(calibrating.wants_charge && calibrating.other_pause_owner);
        assert!(context_for(&snap, 0, false, false, true, false, false).other_pause_owner);
        assert!(context_for(&snap, 0, false, false, false, true, false).other_pause_owner);
        // The dispatch hold's native pause on an AIO (issue #431).
        let held = context_for(&snap, 0, false, false, false, false, true);
        assert!(held.other_pause_owner && !held.wants_charge);
        assert_eq!(
            context_for(&snap, 0, false, false, false, false, false),
            ChargeCapContext::default()
        );
    }

    #[test]
    fn the_snapshot_shows_the_cap_only_where_it_can_run() {
        let shared = ChargeCapShared {
            config: enabled(),
            ..Default::default()
        };
        let mut snap = aio(90, 0, 0, 0);
        stamp_snapshot(&mut snap, &shared);
        assert!(snap.charge_cap_enabled);
        assert_eq!(snap.charge_cap_soc, 95);

        let mut gateway = aio(90, 0, 0, 0);
        gateway.device_type = DeviceType::Gateway;
        stamp_snapshot(&mut gateway, &shared);
        assert!(!gateway.charge_cap_enabled);
        assert_eq!(gateway.charge_cap_state, "unsupported");

        let off = ChargeCapShared {
            runtime: ChargeCapRuntime {
                state: ChargeCapState::Waiting,
                ..Default::default()
            },
            ..Default::default()
        };
        stamp_snapshot(&mut snap, &off);
        assert!(!snap.charge_cap_enabled);
        assert_eq!(
            snap.charge_cap_state, "off",
            "shown off before the next poll"
        );
    }

    #[tokio::test]
    async fn shutdown_hands_charging_back_while_holding() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = enabled_state().await;
            {
                let mut cap = state.charge_cap.lock().await;
                cap.hold = Some(aio_hold(1600, 1900));
                cap.runtime.state = ChargeCapState::Holding;
            }
            crate::settings::Settings::update_async(|s| {
                s.charge_cap_hold = Some(aio_hold(1600, 1900))
            })
            .await
            .unwrap();
            // Stand in for the poll loop: complete the batch once it's queued.
            let pump = {
                let state = state.clone();
                tokio::spawn(async move {
                    loop {
                        let batch = state.pending_writes.lock().await.pop();
                        if let Some(batch) = batch {
                            let _ = batch
                                .completion
                                .expect("completion")
                                .send(crate::inverter::encoder::WriteOutcome::Ok);
                            return batch.writes;
                        }
                        tokio::task::yield_now().await;
                    }
                })
            };
            release_on_shutdown(&state, std::time::Duration::from_secs(5)).await;
            assert_eq!(pump.await.unwrap(), release_writes(1600, 1900));
            let cap = state.charge_cap.lock().await;
            assert!(cap.stopped);
            // The inverter took the release: a pause set while HEM is away
            // mustn't be taken for the cap's on the next start.
            assert_eq!(cap.hold, None);
            assert_eq!(crate::settings::Settings::load().charge_cap_hold, None);
        })
        .await;
    }

    #[tokio::test]
    async fn shutdown_finishes_a_release_still_in_progress() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = enabled_state().await;
            {
                let mut cap = state.charge_cap.lock().await;
                cap.hold = Some(aio_hold(1600, 1900));
                cap.runtime.state = ChargeCapState::Releasing;
            }
            release_on_shutdown(&state, std::time::Duration::from_millis(20)).await;
            let queued = state.pending_writes.lock().await.pop().map(|b| b.writes);
            assert_eq!(queued, Some(release_writes(1600, 1900)));
        })
        .await;
    }

    #[tokio::test]
    async fn shutdown_gives_up_on_an_unreachable_inverter_and_keeps_the_record() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = enabled_state().await;
            {
                let mut cap = state.charge_cap.lock().await;
                cap.hold = Some(aio_hold(1600, 1900));
                cap.runtime.state = ChargeCapState::Holding;
            }
            release_on_shutdown(&state, std::time::Duration::from_millis(20)).await;
            let cap = state.charge_cap.lock().await;
            assert!(cap.stopped);
            assert_eq!(cap.hold, Some(aio_hold(1600, 1900)));
        })
        .await;
    }

    #[tokio::test]
    async fn shutdown_leaves_a_pause_alone_unless_the_cap_holds_it() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            for runtime_state in [ChargeCapState::Blocked, ChargeCapState::Waiting] {
                let state = enabled_state().await;
                {
                    let mut cap = state.charge_cap.lock().await;
                    cap.hold = Some(aio_hold(1600, 1900));
                    cap.runtime.state = runtime_state;
                }
                release_on_shutdown(&state, std::time::Duration::from_millis(20)).await;
                assert!(
                    state.pending_writes.lock().await.is_empty(),
                    "{runtime_state:?}"
                );
            }
            let state = enabled_state().await;
            release_on_shutdown(&state, std::time::Duration::from_millis(20)).await;
            assert!(state.pending_writes.lock().await.is_empty(), "no record");
        })
        .await;
    }
}
