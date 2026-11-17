//! Dongle factory-reset detection (issue #397).
//!
//! Pen Test Partners found that port-scanning a GivEnergy dongle can crash
//! its Wi-Fi module and reset it to factory defaults, which silently wipes
//! the charge and discharge schedule. With GivEnergy in administration no
//! firmware fix is coming, so HEM keeps the last schedule it saw and, when
//! the inverter comes back from an outage with that schedule gone, says so
//! and offers a one-button restore.
//!
//! Invariants:
//!
//! 1. HEM never probes the dongle's other ports; this only reads what the
//!    normal poll already decoded.
//! 2. Detection never writes. Restoring is the user's button
//!    (`server::schedule_reset`).
//! 3. A schedule cleared on purpose isn't a reset. A change seen while
//!    connected is just adopted as the new last known schedule, and a HEM
//!    write since that copy was last confirmed stops a reconnect from being
//!    read as a reset.
//! 4. A wrong read isn't a reset. Three settled reads in a row must all look
//!    like factory defaults (Eco, nothing armed, no slot), and an all-zero
//!    corrupt read decodes as export mode, which isn't factory Eco.
//! 5. While a reset is flagged the last known schedule isn't overwritten, so
//!    the schedule to restore can't be lost to the empty one.
//!
//! The decision logic lives in [`ScheduleWatch`], a pure state machine over
//! snapshots and clock readings; the poll loop and API handlers drive it
//! behind one lock and persist the [`WatchAction`]s it returns.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::inverter::encoder::RegisterWrite;
use crate::inverter::model::{DeviceType, InverterSnapshot, ScheduleSlot};
use crate::inverter::poll::AppState;
use crate::settings::DischargeSlotBackup;

/// A gap this long (seconds) between good reads counts as a reconnect, so
/// a dongle that reboots faster than HEM notices a disconnect is still
/// caught. Raised with the poll interval so a slow poll isn't one long gap.
pub const MIN_GAP_SECS: i64 = 30;
/// Settled reads that must agree before a change is adopted or a reset
/// is flagged.
pub const CONFIRM_READS: u8 = 3;
/// Alert delivery retries (one every [`ALERT_RETRY_SECS`]) before giving up;
/// the Control banner stays either way.
const ALERT_ATTEMPTS: u8 = 12;
const ALERT_RETRY_SECS: i64 = 300;

/// The schedule the inverter held when HEM last saw it settle: what a
/// factory reset would wipe and a restore puts back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownSchedule {
    /// When this copy was taken (unix seconds); the banner says "from …".
    pub saved_at: i64,
    /// HR 0 device code the schedule was read from; a schedule isn't offered
    /// to a different inverter.
    pub device_type_code: String,
    pub charge_slots: Vec<DischargeSlotBackup>,
    pub discharge_slots: Vec<DischargeSlotBackup>,
    pub enable_charge: bool,
    pub enable_discharge: bool,
    /// HR 27: 0 = export, 1 = eco.
    pub battery_power_mode: u8,
}

/// An unconfigured slot is stored as the default record, so stale times in
/// a slot the inverter ignores can't read as a change.
fn record(slot: &ScheduleSlot) -> DischargeSlotBackup {
    if slot.is_configured() {
        DischargeSlotBackup::from(slot)
    } else {
        DischargeSlotBackup {
            enabled: false,
            start_hour: 0,
            start_minute: 0,
            end_hour: 0,
            end_minute: 0,
            target_soc: 4,
        }
    }
}

impl KnownSchedule {
    pub fn from_snapshot(snap: &InverterSnapshot, saved_at: i64) -> Self {
        let take = |slots: &[ScheduleSlot], max: u8| -> Vec<DischargeSlotBackup> {
            slots.iter().take(usize::from(max)).map(record).collect()
        };
        Self {
            saved_at,
            device_type_code: snap.device_type_code.clone(),
            charge_slots: take(&snap.charge_slots, snap.device_type.max_charge_slots()),
            discharge_slots: take(
                &snap.discharge_slots,
                snap.device_type.max_discharge_slots(),
            ),
            enable_charge: snap.enable_charge,
            enable_discharge: snap.enable_discharge,
            battery_power_mode: snap.battery_power_mode,
        }
    }

    /// Whether two schedules are the same, ignoring when they were taken.
    pub fn same_schedule(&self, other: &Self) -> bool {
        self.device_type_code == other.device_type_code
            && self.charge_slots == other.charge_slots
            && self.discharge_slots == other.discharge_slots
            && self.enable_charge == other.enable_charge
            && self.enable_discharge == other.enable_discharge
            && self.battery_power_mode == other.battery_power_mode
    }

    /// Whether there's anything for a reset to have wiped: a configured slot,
    /// timed discharge armed, or export mode.
    pub fn has_content(&self) -> bool {
        self.charge_slots.iter().any(|s| s.enabled)
            || self.discharge_slots.iter().any(|s| s.enabled)
            || self.enable_discharge
            || self.battery_power_mode != 1
    }
}

/// What the Control page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleResetNotice {
    /// When the reset was confirmed (unix seconds).
    pub detected_at: i64,
}

/// The notice as the snapshot carries it to the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleResetInfo {
    pub detected_at: i64,
    /// When the schedule to restore was taken.
    pub saved_at: i64,
}

/// Whether the inverter reads as factory defaults: no configured charge or
/// discharge slot, timed discharge off, eco mode. `enable_charge` is left
/// out: the firmware's default for it isn't known.
pub fn looks_factory_reset(snap: &InverterSnapshot) -> bool {
    let none = |slots: &[ScheduleSlot], max: u8| {
        !slots
            .iter()
            .take(usize::from(max))
            .any(ScheduleSlot::is_configured)
    };
    none(&snap.charge_slots, snap.device_type.max_charge_slots())
        && none(
            &snap.discharge_slots,
            snap.device_type.max_discharge_slots(),
        )
        && !snap.enable_discharge
        && snap.battery_power_mode == 1
}

/// Models HEM watches and can restore: single-phase batteries it controls.
/// Three-phase keeps its enable bits elsewhere, and the Gateway, EMS and
/// batteryless PV inverters have no schedule HEM may write.
pub fn supports_watch(device: DeviceType) -> bool {
    !device.uses_three_phase_schedule_slots()
        && device.battery_controls_reach_battery()
        && !matches!(
            device,
            DeviceType::PvInverter | DeviceType::Ems | DeviceType::Unknown(_)
        )
}

/// Registers that make up the schedule [`KnownSchedule`] keeps: the mode,
/// the global charge target, both enable flags and every slot and per-slot
/// target.
pub fn touches_schedule(register: u16) -> bool {
    matches!(
        register,
        27 | 59 | 96 | 116 | 31 | 32 | 44 | 45 | 56 | 57 | 94 | 95 | 242..=299
    )
}

/// What the caller must do after [`ScheduleWatch::observe`] or a user action.
#[derive(Debug, Clone, PartialEq)]
pub enum WatchAction {
    /// Save as `Settings::last_known_schedule`.
    SaveSchedule(KnownSchedule),
    /// A reset was confirmed: save the notice and send the alert.
    Detected(ScheduleResetNotice),
    /// The notice no longer applies: clear it from settings.
    ClearNotice,
}

#[derive(Debug, Default)]
struct AlertGate {
    pending: bool,
    attempts: u8,
    next_try: i64,
}

/// The detector. One per [`AppState`], behind a lock, shared by the poll
/// loop and the restore and dismiss handlers.
#[derive(Debug, Default)]
pub struct ScheduleWatch {
    loaded: bool,
    stored: Option<KnownSchedule>,
    notice: Option<ScheduleResetNotice>,
    last_good: Option<i64>,
    /// A gap was seen and no settled read has been compared since.
    after_gap: bool,
    /// HEM wrote to the inverter since the stored copy was last confirmed.
    written: bool,
    candidate: Option<(KnownSchedule, u8)>,
    reset_reads: u8,
    hem_owned: bool,
    alert: AlertGate,
}

impl ScheduleWatch {
    /// Load what settings persisted; later calls do nothing, so the
    /// in-memory copy stays the truth.
    pub fn ensure_loaded(
        &mut self,
        stored: Option<&KnownSchedule>,
        notice: Option<ScheduleResetNotice>,
    ) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        self.stored = stored.cloned();
        self.notice = notice;
    }

    /// HEM wrote to the inverter (any write: the schedule may have moved on
    /// purpose). Until the schedule has settled again, a reconnect isn't
    /// read as a reset.
    pub fn note_write(&mut self) {
        self.written = true;
    }

    /// [`Self::note_write`] for the writes that can move the schedule only:
    /// the clock, rates, reserve and limiter pauses don't count.
    pub fn note_writes(&mut self, writes: &[RegisterWrite]) {
        if writes.iter().any(|w| touches_schedule(w.address)) {
            self.note_write();
        }
    }

    /// Whether something HEM runs (a Force action, calibration, Cosy, Agile,
    /// a smart-charge top-up, Timed Export, a pause) owns the schedule
    /// registers right now. What it writes is temporary, so it's neither
    /// kept as the schedule to restore nor read as a reset. Set by the
    /// caller before each [`Self::observe`].
    pub fn set_hem_owned(&mut self, owned: bool) {
        self.hem_owned = owned;
    }

    pub fn info(&self) -> Option<ScheduleResetInfo> {
        let notice = self.notice?;
        let stored = self.stored.as_ref()?;
        Some(ScheduleResetInfo {
            detected_at: notice.detected_at,
            saved_at: stored.saved_at,
        })
    }

    pub fn stored(&self) -> Option<&KnownSchedule> {
        self.stored.as_ref()
    }

    pub fn notice(&self) -> Option<ScheduleResetNotice> {
        self.notice
    }

    /// The user dismissed the banner: the inverter's current schedule
    /// becomes the one to keep, once it has held for [`CONFIRM_READS`] reads.
    pub fn dismiss(&mut self) -> Vec<WatchAction> {
        if self.notice.take().is_none() {
            return Vec::new();
        }
        self.alert = AlertGate::default();
        self.candidate = None;
        // The user has acted: until what the inverter holds settles as the
        // schedule to keep, a reconnect isn't read as another reset.
        self.after_gap = false;
        self.written = true;
        vec![WatchAction::ClearNotice]
    }

    /// One poll. `settled` is false during the post-connect grace readings,
    /// which only keep the clock. `poll_interval_secs` widens the gap that
    /// counts as a reconnect.
    pub fn observe(
        &mut self,
        snap: &InverterSnapshot,
        now: i64,
        settled: bool,
        poll_interval_secs: u64,
    ) -> Vec<WatchAction> {
        if !supports_watch(snap.device_type) {
            return Vec::new();
        }
        let gap_secs = MIN_GAP_SECS.max(3 * i64::try_from(poll_interval_secs).unwrap_or(0));
        // The first read after HEM starts is a gap too: HEM wasn't watching
        // while it was closed. A clock that stepped back says nothing.
        if self.notice.is_none() && self.last_good.is_none_or(|last| now - last >= gap_secs) {
            self.after_gap = true;
            self.reset_reads = 0;
            self.candidate = None;
        }
        self.last_good = Some(now);
        if !settled {
            return Vec::new();
        }
        if self.hem_owned {
            // Not the user's schedule, and nothing to compare it with.
            self.candidate = None;
            self.reset_reads = 0;
            return Vec::new();
        }

        let current = KnownSchedule::from_snapshot(snap, now);
        let reset_now = looks_factory_reset(snap);

        if self.notice.is_some() {
            return self.observe_flagged(current);
        }

        if self.after_gap {
            let stored_had_content = self
                .stored
                .as_ref()
                .is_some_and(|s| s.has_content() && s.device_type_code == current.device_type_code);
            if reset_now && stored_had_content && !self.written {
                self.reset_reads += 1;
                if self.reset_reads >= CONFIRM_READS {
                    self.after_gap = false;
                    self.reset_reads = 0;
                    let notice = ScheduleResetNotice { detected_at: now };
                    self.notice = Some(notice);
                    self.alert = AlertGate {
                        pending: true,
                        attempts: 0,
                        next_try: now,
                    };
                    return vec![WatchAction::Detected(notice)];
                }
                return Vec::new();
            }
            // Back with a schedule, or one HEM changed: not a reset.
            self.after_gap = false;
            self.reset_reads = 0;
        }

        self.adopt_when_stable(current)
    }

    /// While a reset is flagged the stored schedule is never replaced, so a
    /// restore that lands only in part (a rejected write) can't become the
    /// schedule to keep. The flag ends when the inverter holds the stored
    /// schedule again for [`CONFIRM_READS`] reads (the restore landed), or on
    /// Dismiss.
    fn observe_flagged(&mut self, current: KnownSchedule) -> Vec<WatchAction> {
        let restored = self
            .stored
            .as_ref()
            .is_some_and(|stored| stored.same_schedule(&current));
        if !restored {
            self.candidate = None;
            return Vec::new();
        }
        let reads = match &mut self.candidate {
            Some((_, reads)) => {
                *reads = reads.saturating_add(1);
                *reads
            }
            slot => {
                *slot = Some((current, 1));
                1
            }
        };
        if reads < CONFIRM_READS {
            return Vec::new();
        }
        self.notice = None;
        self.alert = AlertGate::default();
        self.candidate = None;
        self.written = false;
        vec![WatchAction::ClearNotice]
    }

    /// Count consecutive identical reads; at [`CONFIRM_READS`] the schedule
    /// becomes the stored one (when it differs) and the write flag clears.
    fn adopt_when_stable(&mut self, current: KnownSchedule) -> Vec<WatchAction> {
        let reads = match &mut self.candidate {
            Some((seen, reads)) if seen.same_schedule(&current) => {
                *reads = reads.saturating_add(1);
                *reads
            }
            slot => {
                *slot = Some((current.clone(), 1));
                1
            }
        };
        if reads < CONFIRM_READS {
            return Vec::new();
        }
        self.written = false;
        if self
            .stored
            .as_ref()
            .is_some_and(|stored| stored.same_schedule(&current))
        {
            return Vec::new();
        }
        self.stored = Some(current.clone());
        vec![WatchAction::SaveSchedule(current)]
    }

    /// Whether the alert should be sent now. Counts the attempt; call
    /// [`Self::alert_delivered`] when it got through.
    pub fn take_alert_due(&mut self, now: i64) -> bool {
        let gate = &mut self.alert;
        if !gate.pending || self.notice.is_none() || now < gate.next_try {
            return false;
        }
        gate.attempts += 1;
        gate.next_try = now + ALERT_RETRY_SECS;
        if gate.attempts >= ALERT_ATTEMPTS {
            gate.pending = false;
        }
        true
    }

    pub fn alert_delivered(&mut self) {
        self.alert.pending = false;
    }
}

/// Settings the watch persists, applied under the caller's watch lock so a
/// poll and an API call never interleave their saves.
pub async fn persist(actions: Vec<WatchAction>) {
    for action in actions {
        let result = match action {
            WatchAction::SaveSchedule(schedule) => {
                crate::settings::Settings::update_async(move |s| {
                    s.last_known_schedule = Some(schedule);
                })
                .await
            }
            WatchAction::Detected(notice) => {
                crate::settings::Settings::update_async(move |s| {
                    s.schedule_reset_notice = Some(notice);
                })
                .await
            }
            WatchAction::ClearNotice => {
                crate::settings::Settings::update_async(|s| {
                    s.schedule_reset_notice = None;
                })
                .await
            }
        };
        if let Err(e) = result {
            tracing::warn!("Couldn't save the schedule-reset watch state: {e}");
        }
    }
}

/// Load the watch from `settings` on first use and stamp the snapshot with
/// the open notice, if any.
pub async fn stamp_snapshot(
    state: &Arc<AppState>,
    settings: &crate::settings::Settings,
    snapshot: &mut InverterSnapshot,
) {
    let mut watch = state.schedule_watch.lock().await;
    watch.ensure_loaded(
        settings.last_known_schedule.as_ref(),
        settings.schedule_reset_notice,
    );
    snapshot.schedule_reset = watch.info();
}

/// The alert text.
pub fn build_message(saved_at: i64) -> String {
    let from = chrono::DateTime::from_timestamp(saved_at, 0)
        .map(|at| {
            at.with_timezone(&chrono::Local)
                .format("%-d %B %H:%M")
                .to_string()
        })
        .unwrap_or_default();
    format!(
        "⚠️ HEM: the inverter's charge and discharge schedule has gone back to factory \
         defaults after the connection dropped. That's what a dongle reset does. HEM kept the \
         schedule from {from}: open Control in HEM to restore it. Settings → Dongle Security \
         lists how to stop it happening again."
    )
}

/// What, if anything, is running its own schedule on the inverter: a Force
/// action, the battery calibration, a pause or limiter, Cosy, Agile, a
/// smart-charge top-up or the Timed Export schedule. Those windows are
/// HEM's and temporary (HEM sets them up again itself), so they're neither
/// the schedule to keep nor safe to restore over.
pub(crate) async fn hem_owns_schedule(
    state: &Arc<AppState>,
    snapshot: Option<&InverterSnapshot>,
) -> Option<&'static str> {
    if state.force_charge_revert.lock().await.is_some()
        || state.force_discharge_revert.lock().await.is_some()
    {
        return Some("a Force Charge or Force Discharge");
    }
    if state.calibration_run.lock().await.is_some()
        || snapshot.is_some_and(|s| s.battery_calibration_stage != 0)
    {
        return Some("a battery calibration");
    }
    if state.pause_mode_revert.lock().await.is_some()
        || state.load_limiter_state.lock().await.is_actively_pausing()
        || state
            .temperature_limiter_state
            .lock()
            .await
            .is_actively_pausing()
        || state.dispatch_hold_state.lock().await.is_actively_pausing()
    {
        return Some("a pause or limiter");
    }
    if *state.cosy_active.lock().await
        || snapshot.is_some_and(|s| s.cosy_active || s.agile_active || s.dispatch_charge_active)
    {
        return Some("Cosy, Agile or a smart-charge top-up");
    }
    if state.timed_export_config.lock().await.schedule_enabled {
        return Some("the Timed Export schedule");
    }
    None
}

/// One poll's check: feed the settled snapshot, persist what changed and,
/// when a reset was confirmed (or an alert is still owed), pass the message
/// to `send` off the poll's path.
pub(crate) async fn run_watch<F, Fut>(
    state: &Arc<AppState>,
    snapshot: &InverterSnapshot,
    now: i64,
    settled: bool,
    poll_interval_secs: u64,
    send: F,
) where
    F: FnOnce(crate::settings::AlertsConfig, String) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = bool> + Send + 'static,
{
    // Worked out before taking the watch lock: it reads several other locks.
    let hem_owned = hem_owns_schedule(state, Some(snapshot)).await.is_some();
    let (alert_text, config) = {
        let mut watch = state.schedule_watch.lock().await;
        watch.set_hem_owned(hem_owned);
        let actions = watch.observe(snapshot, now, settled, poll_interval_secs);
        if !actions.is_empty() {
            // Held across the save so a restore or dismiss can't interleave.
            persist(actions).await;
        }
        if !watch.alert.pending {
            return;
        }
        let config = state.alert_config.lock().await.clone();
        if !(config.enabled
            && config.schedule_reset_alert_enabled
            && crate::alerts::has_delivery_channel(&config))
        {
            // Nobody to tell yet; the alert stays owed in case alerts are
            // turned on while the banner is still up.
            return;
        }
        if !watch.take_alert_due(now) {
            return;
        }
        let saved_at = watch.stored().map_or(now, |s| s.saved_at);
        (build_message(saved_at), config)
    };
    let state = state.clone();
    tokio::spawn(async move {
        if send(config, alert_text).await {
            state.schedule_watch.lock().await.alert_delivered();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(start: (u8, u8), end: (u8, u8), soc: u8) -> ScheduleSlot {
        ScheduleSlot {
            enabled: true,
            start_hour: start.0,
            start_minute: start.1,
            end_hour: end.0,
            end_minute: end.1,
            target_soc: soc,
        }
    }

    fn base() -> InverterSnapshot {
        InverterSnapshot {
            device_type: DeviceType::Gen3Hybrid,
            device_type_code: "2201".to_string(),
            battery_power_mode: 1,
            ..Default::default()
        }
    }

    /// A snapshot with a night charge slot set up.
    fn scheduled() -> InverterSnapshot {
        let mut snap = base();
        snap.charge_slots[0] = slot((0, 30), (5, 30), 100);
        snap.enable_charge = true;
        snap
    }

    fn watch_with(stored: &InverterSnapshot) -> ScheduleWatch {
        let mut w = ScheduleWatch::default();
        w.ensure_loaded(Some(&KnownSchedule::from_snapshot(stored, 1_000)), None);
        w
    }

    /// Feed `n` settled polls 10 s apart from `from`.
    fn feed(w: &mut ScheduleWatch, snap: &InverterSnapshot, from: i64, n: i64) -> Vec<WatchAction> {
        let mut all = Vec::new();
        for i in 0..n {
            all.extend(w.observe(snap, from + i * 10, true, 10));
        }
        all
    }

    // -- pure helpers --

    #[test]
    fn factory_defaults_means_eco_nothing_armed_and_no_slot() {
        assert!(looks_factory_reset(&base()));
        assert!(!looks_factory_reset(&scheduled()));

        let mut armed = base();
        armed.enable_discharge = true;
        assert!(!looks_factory_reset(&armed));

        let mut export = base();
        export.battery_power_mode = 0;
        assert!(
            !looks_factory_reset(&export),
            "export mode isn't factory Eco"
        );

        let mut discharge = base();
        discharge.discharge_slots[1] = slot((16, 0), (19, 0), 20);
        assert!(!looks_factory_reset(&discharge));
    }

    #[test]
    fn a_partially_cleared_schedule_is_not_factory_defaults() {
        let mut snap = scheduled();
        snap.charge_slots[0] = ScheduleSlot::default();
        snap.discharge_slots[0] = slot((17, 0), (19, 0), 10);
        assert!(!looks_factory_reset(&snap));
    }

    #[test]
    fn slots_beyond_the_models_count_are_ignored() {
        let mut snap = base();
        snap.device_type = DeviceType::Gen1Hybrid; // one charge slot
        snap.charge_slots[4] = slot((1, 0), (2, 0), 100);
        assert!(looks_factory_reset(&snap));
        assert_eq!(KnownSchedule::from_snapshot(&snap, 0).charge_slots.len(), 1);
    }

    #[test]
    fn unconfigured_slot_noise_does_not_change_the_schedule() {
        let a = KnownSchedule::from_snapshot(&scheduled(), 1);
        let mut noisy = scheduled();
        noisy.charge_slots[1].start_hour = 7; // stale time, slot not enabled
        noisy.charge_slots[1].target_soc = 80;
        let b = KnownSchedule::from_snapshot(&noisy, 2);
        assert!(a.same_schedule(&b));
    }

    #[test]
    fn has_content_needs_a_slot_arming_or_export_mode() {
        assert!(!KnownSchedule::from_snapshot(&base(), 0).has_content());
        assert!(KnownSchedule::from_snapshot(&scheduled(), 0).has_content());
        let mut export = base();
        export.battery_power_mode = 0;
        assert!(KnownSchedule::from_snapshot(&export, 0).has_content());
    }

    #[test]
    fn only_single_phase_batteries_are_watched() {
        assert!(supports_watch(DeviceType::Gen3Hybrid));
        assert!(supports_watch(DeviceType::AllInOne6kW));
        assert!(supports_watch(DeviceType::HybridHvGen3));
        for unwatched in [
            DeviceType::ThreePhase,
            DeviceType::ACThreePhase,
            DeviceType::Gateway,
            DeviceType::Ems,
            DeviceType::PvInverter,
            DeviceType::Unknown(0),
        ] {
            assert!(!supports_watch(unwatched), "{unwatched:?}");
        }
    }

    // -- keeping the last known schedule --

    #[test]
    fn a_new_schedule_is_stored_after_three_matching_reads() {
        let mut w = ScheduleWatch::default();
        w.ensure_loaded(None, None);
        let snap = scheduled();
        assert!(
            feed(&mut w, &snap, 1_000, 2).is_empty(),
            "two reads aren't enough"
        );
        let actions = feed(&mut w, &snap, 1_020, 1);
        assert!(matches!(actions.as_slice(), [WatchAction::SaveSchedule(_)]));
        // Stable afterwards: nothing more to save.
        assert!(feed(&mut w, &snap, 1_030, 5).is_empty());
    }

    #[test]
    fn a_flickering_schedule_is_never_stored() {
        let mut w = ScheduleWatch::default();
        w.ensure_loaded(None, None);
        let (a, b) = (scheduled(), base());
        for i in 0..20 {
            let snap = if i % 2 == 0 { &a } else { &b };
            assert!(w.observe(snap, 1_000 + i * 10, true, 10).is_empty());
        }
    }

    #[test]
    fn grace_reads_do_not_count_toward_a_change() {
        let mut w = ScheduleWatch::default();
        w.ensure_loaded(None, None);
        for i in 0..6 {
            assert!(w
                .observe(&scheduled(), 1_000 + i * 10, false, 10)
                .is_empty());
        }
    }

    // -- detecting a reset --

    #[test]
    fn a_reset_after_a_reconnect_is_flagged_on_the_third_read() {
        let mut w = watch_with(&scheduled());
        w.observe(&scheduled(), 2_000, true, 10);
        // 2 minutes with no good read, then three reads of factory defaults.
        let reset = base();
        assert!(w.observe(&reset, 2_130, false, 10).is_empty());
        assert!(w.observe(&reset, 2_140, true, 10).is_empty());
        assert!(w.observe(&reset, 2_150, true, 10).is_empty());
        let actions = w.observe(&reset, 2_160, true, 10);
        assert_eq!(
            actions,
            vec![WatchAction::Detected(ScheduleResetNotice {
                detected_at: 2_160
            })]
        );
        let info = w.info().unwrap();
        assert_eq!(info.saved_at, 1_000);
    }

    #[test]
    fn a_gap_of_thirty_seconds_counts_as_a_reconnect() {
        let mut w = watch_with(&scheduled());
        w.observe(&scheduled(), 2_000, true, 10);
        let reset = base();
        // 30 s later, with no disconnect state change in between.
        assert!(w.observe(&reset, 2_030, true, 10).is_empty());
        assert!(w.observe(&reset, 2_040, true, 10).is_empty());
        assert!(matches!(
            w.observe(&reset, 2_050, true, 10).as_slice(),
            [WatchAction::Detected(_)]
        ));
    }

    #[test]
    fn a_short_gap_is_not_a_reconnect() {
        let mut w = watch_with(&scheduled());
        w.observe(&scheduled(), 2_000, true, 10);
        // 29 s, then the schedule empties: seen arriving, so just adopted.
        let reset = base();
        let actions = feed(&mut w, &reset, 2_029, 3);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, WatchAction::Detected(_))));
        assert!(actions
            .iter()
            .any(|a| matches!(a, WatchAction::SaveSchedule(_))));
    }

    #[test]
    fn a_slow_poll_interval_widens_the_gap() {
        let mut w = watch_with(&scheduled());
        w.observe(&scheduled(), 2_000, true, 60);
        // 60 s polls: a 60 s step is normal, not a reconnect (limit is 180 s).
        let reset = base();
        let mut actions = Vec::new();
        for i in 1..=3 {
            actions.extend(w.observe(&reset, 2_000 + i * 60, true, 60));
        }
        assert!(!actions
            .iter()
            .any(|a| matches!(a, WatchAction::Detected(_))));
    }

    #[test]
    fn a_schedule_cleared_while_connected_is_not_a_reset() {
        let mut w = watch_with(&scheduled());
        w.observe(&scheduled(), 1_990, true, 10);
        let actions = feed(&mut w, &base(), 2_000, 10);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, WatchAction::Detected(_))));
        assert!(w.notice().is_none());
        assert!(
            !w.stored().unwrap().has_content(),
            "the empty schedule is the new one"
        );
    }

    #[test]
    fn a_reconnect_with_the_schedule_intact_is_not_a_reset() {
        let mut w = watch_with(&scheduled());
        w.observe(&scheduled(), 2_000, true, 10);
        let actions = feed(&mut w, &scheduled(), 2_300, 6);
        assert!(actions.is_empty());
        assert!(w.notice().is_none());
    }

    #[test]
    fn a_reset_with_nothing_stored_to_lose_is_ignored() {
        let mut w = watch_with(&base()); // user never had a schedule
        w.observe(&base(), 2_000, true, 10);
        let actions = feed(&mut w, &base(), 2_300, 6);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, WatchAction::Detected(_))));
    }

    #[test]
    fn a_corrupt_all_zero_read_is_not_a_reset() {
        let mut w = watch_with(&scheduled());
        w.observe(&scheduled(), 2_000, true, 10);
        // An all-zero poll decodes as export mode, not factory Eco.
        let mut zeros = base();
        zeros.battery_power_mode = 0;
        let actions = feed(&mut w, &zeros, 2_300, 6);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, WatchAction::Detected(_))));
    }

    #[test]
    fn intermittent_zero_reads_never_confirm() {
        let mut w = watch_with(&scheduled());
        w.observe(&scheduled(), 2_000, true, 10);
        let reset = base();
        // After the gap, factory-looking reads are interrupted by the real
        // schedule: the run of three never completes.
        assert!(w.observe(&reset, 2_300, true, 10).is_empty());
        assert!(w.observe(&reset, 2_310, true, 10).is_empty());
        assert!(w.observe(&scheduled(), 2_320, true, 10).is_empty());
        assert!(w.observe(&reset, 2_330, true, 10).is_empty());
        assert!(w.observe(&reset, 2_340, true, 10).is_empty());
        assert!(w.notice().is_none());
    }

    #[test]
    fn a_hem_write_before_the_reconnect_means_it_was_the_users_change() {
        let mut w = watch_with(&scheduled());
        w.observe(&scheduled(), 2_000, true, 10);
        // The user cleared the schedule through HEM, then the dongle dropped
        // before the change had settled.
        w.note_write();
        let actions = feed(&mut w, &base(), 2_300, 6);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, WatchAction::Detected(_))));
        assert!(!w.stored().unwrap().has_content());
    }

    #[test]
    fn writes_stop_masking_a_reset_once_the_schedule_has_settled() {
        let mut w = watch_with(&scheduled());
        w.note_write();
        // Unchanged by the write (e.g. a rate tweak); three stable reads clear the mark.
        feed(&mut w, &scheduled(), 2_000, 3);
        let actions = feed(&mut w, &base(), 2_300, 3);
        assert!(actions
            .iter()
            .any(|a| matches!(a, WatchAction::Detected(_))));
    }

    #[test]
    fn a_different_inverter_is_not_compared() {
        let mut w = watch_with(&scheduled());
        w.observe(&scheduled(), 2_000, true, 10);
        let mut other = base();
        other.device_type_code = "3001".to_string();
        let actions = feed(&mut w, &other, 2_300, 4);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, WatchAction::Detected(_))));
    }

    #[test]
    fn unwatched_models_are_never_flagged() {
        let mut gw = scheduled();
        gw.device_type = DeviceType::Gateway;
        let mut w = watch_with(&gw);
        w.observe(&gw, 2_000, true, 10);
        let mut reset = base();
        reset.device_type = DeviceType::Gateway;
        assert!(feed(&mut w, &reset, 2_300, 6).is_empty());
    }

    // -- while flagged --

    fn flagged() -> ScheduleWatch {
        let mut w = watch_with(&scheduled());
        w.observe(&scheduled(), 2_000, true, 10);
        feed(&mut w, &base(), 2_300, 3);
        assert!(w.notice().is_some());
        w
    }

    #[test]
    fn the_stored_schedule_is_kept_while_the_reset_is_flagged() {
        let mut w = flagged();
        // Hours of factory defaults must never overwrite it.
        let actions = feed(&mut w, &base(), 3_000, 50);
        assert!(actions.is_empty());
        assert!(w.stored().unwrap().has_content());
        assert!(w.notice().is_some());
    }

    #[test]
    fn the_flag_clears_when_the_restore_lands() {
        let mut w = flagged();
        let actions = feed(&mut w, &scheduled(), 3_000, 3);
        assert!(actions.contains(&WatchAction::ClearNotice));
        assert!(w.notice().is_none());
        assert!(w.stored().unwrap().has_content());
    }

    #[test]
    fn a_partly_applied_restore_neither_clears_the_flag_nor_replaces_the_saved_schedule() {
        // A rejected write leaves some of the schedule on the inverter. That
        // must not become "the schedule to keep" (invariant 5).
        let mut w = flagged();
        let mut partial = base();
        partial.charge_slots[0] = slot((0, 30), (5, 30), 100);
        let actions = feed(&mut w, &partial, 3_000, 10);
        assert!(actions.is_empty(), "{actions:?}");
        assert!(w.notice().is_some());
        assert!(w
            .stored()
            .unwrap()
            .same_schedule(&KnownSchedule::from_snapshot(&scheduled(), 0)));
    }

    #[test]
    fn a_schedule_set_up_by_hand_while_flagged_waits_for_dismiss() {
        let mut w = flagged();
        let mut other = base();
        other.discharge_slots[0] = slot((17, 0), (19, 0), 10);
        assert!(feed(&mut w, &other, 3_000, 10).is_empty());
        assert!(w.notice().is_some());
        w.dismiss();
        let actions = feed(&mut w, &other, 3_200, 3);
        assert!(actions
            .iter()
            .any(|a| matches!(a, WatchAction::SaveSchedule(_))));
    }

    // -- restarts, writes and HEM-owned schedules --

    #[test]
    fn a_reset_while_hem_was_closed_is_flagged_not_adopted() {
        // The stored schedule loads from settings; the first read after
        // startup is factory defaults.
        let mut w = watch_with(&scheduled());
        let actions = feed(&mut w, &base(), 2_000, 3);
        assert!(actions
            .iter()
            .any(|a| matches!(a, WatchAction::Detected(_))));
        assert!(
            w.stored().unwrap().has_content(),
            "the saved schedule survives"
        );
    }

    #[test]
    fn a_restart_with_the_schedule_intact_flags_nothing() {
        let mut w = watch_with(&scheduled());
        assert!(feed(&mut w, &scheduled(), 2_000, 6).is_empty());
    }

    #[test]
    fn only_schedule_writes_mask_a_reset() {
        let write = |address| RegisterWrite { address, value: 1 };
        for unrelated in [35, 110, 111, 163, 318] {
            let mut w = watch_with(&scheduled());
            w.observe(&scheduled(), 2_000, true, 10);
            w.note_writes(&[write(unrelated)]);
            let actions = feed(&mut w, &base(), 2_300, 3);
            assert!(
                actions
                    .iter()
                    .any(|a| matches!(a, WatchAction::Detected(_))),
                "HR {unrelated} shouldn't mask a reset"
            );
        }
        for schedule in [27, 59, 94, 95, 96, 116, 56, 244, 276, 299] {
            let mut w = watch_with(&scheduled());
            w.observe(&scheduled(), 2_000, true, 10);
            w.note_writes(&[write(schedule)]);
            let actions = feed(&mut w, &base(), 2_300, 3);
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, WatchAction::Detected(_))),
                "HR {schedule} is a schedule write"
            );
        }
    }

    #[test]
    fn a_schedule_owned_by_hem_is_neither_kept_nor_read_as_a_reset() {
        // A Force Discharge writes a real slot and arms discharge; if it
        // held for three reads it must not become the schedule to restore.
        let mut w = watch_with(&scheduled());
        w.observe(&scheduled(), 2_000, true, 10);
        let mut forced = base();
        forced.discharge_slots[0] = slot((0, 0), (23, 59), 4);
        forced.enable_discharge = true;
        forced.battery_power_mode = 0;
        w.set_hem_owned(true);
        assert!(feed(&mut w, &forced, 2_010, 10).is_empty());
        assert!(w
            .stored()
            .unwrap()
            .same_schedule(&KnownSchedule::from_snapshot(&scheduled(), 0)));
        // Nor does a reset read during an owned stretch count.
        assert!(feed(&mut w, &base(), 2_300, 5).is_empty());
        assert!(w.notice().is_none());
        // Once HEM lets go, normal watching resumes.
        w.set_hem_owned(false);
        let actions = feed(&mut w, &base(), 2_400, 3);
        assert!(actions
            .iter()
            .any(|a| matches!(a, WatchAction::Detected(_))));
    }

    #[test]
    fn dismissing_keeps_the_old_schedule_until_the_new_one_settles() {
        let mut w = flagged();
        assert_eq!(w.dismiss(), vec![WatchAction::ClearNotice]);
        assert!(w.stored().unwrap().has_content());
        assert!(w.info().is_none());
        // Three reads of factory defaults: now they are the user's choice.
        let actions = feed(&mut w, &base(), 3_000, 3);
        assert!(actions
            .iter()
            .any(|a| matches!(a, WatchAction::SaveSchedule(_))));
        assert!(!w.stored().unwrap().has_content());
        // And not flagged again.
        assert!(!actions
            .iter()
            .any(|a| matches!(a, WatchAction::Detected(_))));
    }

    #[test]
    fn dismissing_with_nothing_flagged_does_nothing() {
        let mut w = watch_with(&scheduled());
        assert!(w.dismiss().is_empty());
    }

    // -- the alert --

    #[test]
    fn the_alert_is_due_once_then_retried_until_delivered() {
        let mut w = flagged();
        assert!(w.take_alert_due(5_000));
        assert!(!w.take_alert_due(5_010), "not again straight away");
        assert!(w.take_alert_due(5_000 + ALERT_RETRY_SECS), "retried");
        w.alert_delivered();
        assert!(!w.take_alert_due(9_000_000));
    }

    #[test]
    fn the_alert_gives_up_after_its_attempts() {
        let mut w = flagged();
        let mut sent = 0;
        for i in 0..40 {
            if w.take_alert_due(5_000 + i * ALERT_RETRY_SECS) {
                sent += 1;
            }
        }
        assert_eq!(sent, i32::from(ALERT_ATTEMPTS));
    }

    #[test]
    fn dismissing_cancels_a_pending_alert() {
        let mut w = flagged();
        w.dismiss();
        assert!(!w.take_alert_due(9_000_000));
    }

    #[test]
    fn the_message_names_the_saved_schedule_and_where_to_restore() {
        let text = build_message(1_700_000_000);
        assert!(text.contains("factory defaults"));
        assert!(text.contains("Control"));
        assert!(text.contains("Dongle Security"));
        assert!(text.contains("November"), "{text}");
    }
}
