//! Battery SOC that jumps in steps or sticks at one value (issue #393).
//!
//! BMS firmware 3007–3022 is reported to mis-estimate state of charge: the
//! SOC drops or rises by many points at once, or freezes while the battery
//! keeps charging or discharging. [`SocWatch`] spots both from the live
//! polls. It is pure; the poll loop records and announces what it returns.
//!
//! - **Jump**: the SOC moves further than any real charge rate allows
//!   ([`jump_allowance`]) and stays at the new level for
//!   [`JUMP_CONFIRM_POLLS`] polls, each read within a real charge rate of
//!   the one before (the battery may keep charging or discharging) and
//!   still a jump away from the old value. A one-poll corrupt read springs
//!   back and is ignored.
//! - **Stuck**: the SOC stays on one value while the net energy through the
//!   battery reaches [`STUCK_CAPACITY_FRACTION`] of its capacity. Sitting at
//!   100% while topping up, or at the reserve while the inverter holds it,
//!   does not count.
//!
//! A poll whose readings all repeat the previous poll's exactly is a dongle
//! serving stale data (live power always wobbles by a few watts): it moves
//! the clock on but neither adds energy nor confirms a jump.
//!
//! Each module with its own SOC is watched separately; when no module reports
//! one, the system SOC is watched instead. Anything that breaks continuity
//! (a gap longer than [`MAX_GAP_SECS`] and three poll intervals, the clock moving backwards, the
//! post-connect grace period, an inverter calibration in progress) starts the
//! watch afresh rather than comparing across it.

use std::collections::HashMap;

use serde::Serialize;

use crate::inverter::model::InverterSnapshot;

/// Polls the new value must be read on (including the first) before a step
/// counts as a jump.
pub const JUMP_CONFIRM_POLLS: u8 = 3;
/// A step this many points or fewer is never a jump, whatever the interval.
pub const JUMP_MIN_POINTS: f64 = 5.0;
/// The fastest plausible SOC movement, in points per minute. 1C is about
/// 1.7 points a minute; residential batteries are limited well below that.
pub const MAX_POINTS_PER_MINUTE: f64 = 2.0;
/// Net energy, as a fraction of battery capacity, that must pass while the
/// SOC holds one value before it counts as stuck.
pub const STUCK_CAPACITY_FRACTION: f64 = 0.15;
/// A gap between polls longer than this restarts the watch.
pub const MAX_GAP_SECS: i64 = 600;

/// Which SOC an event is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case", tag = "source", content = "index")]
pub enum SocSource {
    /// A battery module slot (0-based), from its own BMS SOC.
    Module(usize),
    /// The inverter's system SOC, used when no module reports its own.
    System,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SocEventKind {
    /// The SOC stepped from `from` to `to` and stayed there.
    Jump { from: u8, to: u8 },
    /// The SOC held `soc` while `energy_kwh` (signed, + = discharged) passed.
    Stuck { soc: u8, energy_kwh: f64 },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SocEvent {
    pub source: SocSource,
    #[serde(flatten)]
    pub kind: SocEventKind,
}

/// The largest SOC change, in points, that real charging or discharging can
/// produce in `elapsed_secs`.
pub fn jump_allowance(elapsed_secs: i64) -> f64 {
    JUMP_MIN_POINTS + MAX_POINTS_PER_MINUTE * (elapsed_secs.max(0) as f64 / 60.0)
}

/// Live SOC watch. Keep one per inverter connection and feed it every
/// sanitized snapshot.
#[derive(Debug, Default)]
pub struct SocWatch {
    tracks: HashMap<SocSource, Track>,
    /// Each source's [`fingerprint`] at the previous poll.
    last_fingerprints: HashMap<SocSource, Vec<u32>>,
    /// The configured poll interval, seconds (0 until set).
    poll_interval_secs: i64,
}

/// The live readings behind one SOC that cannot all repeat exactly while
/// they are really being read: the whole snapshot for the system SOC, the
/// module's own BMS figures for a module. An exact repeat is a dongle serving
/// stale data, or the sanitizer carrying a silent module forward.
fn fingerprint(snap: &InverterSnapshot, source: SocSource) -> Vec<u32> {
    let module = match source {
        SocSource::Module(index) => snap.battery_modules.iter().find(|m| m.index == index),
        SocSource::System => None,
    };
    // A module with no readings of its own besides its SOC is judged on the
    // whole snapshot, or it would look like a repeat on every poll.
    let own = module.filter(|m| {
        m.voltage != 0.0 || m.current != 0.0 || m.temperature != 0.0 || !m.cell_voltages.is_empty()
    });
    match own {
        Some(m) => {
            let mut fp = vec![
                u32::from(m.soc),
                m.voltage.to_bits(),
                m.current.to_bits(),
                m.temperature.to_bits(),
            ];
            fp.extend(m.cell_voltages.iter().map(|v| v.to_bits()));
            fp
        }
        None => {
            let mut fp = vec![
                u32::from(snap.soc),
                snap.battery_power as u32,
                snap.grid_power as u32,
                snap.home_power as u32,
                snap.solar_power as u32,
                snap.battery_voltage.to_bits(),
            ];
            fp.extend(snap.battery_modules.iter().map(|m| u32::from(m.soc)));
            fp
        }
    }
}

/// One SOC's watch since it last started afresh.
#[derive(Debug)]
struct Track {
    /// The previous poll, unix seconds.
    last_ts: i64,
    /// The accepted SOC, and when it was last read.
    soc: u8,
    soc_ts: i64,
    /// A step awaiting confirmation.
    pending: Option<Pending>,
    /// Net energy (kWh, + = discharged) since `soc` was accepted.
    energy_kwh: f64,
    stuck_reported: bool,
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    /// The value first stepped to (reported).
    to: u8,
    /// The latest read since, and when.
    last: u8,
    last_ts: i64,
    /// Polls that have read the new level, the first included.
    reads: u8,
    /// The allowance the step exceeded when first read. Later reads must
    /// stay this far from the old value; the allowance is not re-measured,
    /// or it would grow with every poll until it swallowed the step.
    threshold: f64,
}

impl Track {
    fn new(soc: u8, ts: i64) -> Self {
        Self {
            last_ts: ts,
            soc,
            soc_ts: ts,
            pending: None,
            energy_kwh: 0.0,
            stuck_reported: false,
        }
    }

    fn accept(&mut self, soc: u8, ts: i64) {
        if soc != self.soc {
            self.energy_kwh = 0.0;
            self.stuck_reported = false;
        }
        self.soc = soc;
        self.soc_ts = ts;
    }

    fn step(
        &mut self,
        soc: u8,
        ts: i64,
        snap: &InverterSnapshot,
        repeat: bool,
        max_gap: i64,
    ) -> Vec<SocEventKind> {
        let elapsed = ts - self.last_ts;
        if !(0..=max_gap).contains(&elapsed) {
            *self = Self::new(soc, ts);
            return Vec::new();
        }
        self.last_ts = ts;
        if repeat {
            return Vec::new();
        }
        let mut events = Vec::new();

        let allowance = jump_allowance(ts - self.soc_ts);
        let from_accepted = f64::from(soc.abs_diff(self.soc));
        match self.pending {
            Some(p)
                if f64::from(soc.abs_diff(p.last)) <= jump_allowance(ts - p.last_ts)
                    && from_accepted > p.threshold =>
            {
                if p.reads + 1 >= JUMP_CONFIRM_POLLS {
                    events.push(SocEventKind::Jump {
                        from: self.soc,
                        to: p.to,
                    });
                    self.pending = None;
                    self.accept(soc, ts);
                } else {
                    self.pending = Some(Pending {
                        last: soc,
                        last_ts: ts,
                        reads: p.reads + 1,
                        ..p
                    });
                }
            }
            _ => {
                // No step pending, or the reading left the new level (a
                // corrupt read springing back, or a different step).
                self.pending = None;
                if from_accepted > allowance {
                    self.pending = Some(Pending {
                        to: soc,
                        last: soc,
                        last_ts: ts,
                        reads: 1,
                        threshold: allowance,
                    });
                } else {
                    self.accept(soc, ts);
                }
            }
        }

        // Stuck is judged on the accepted SOC, so a corrupt read awaiting
        // confirmation does not restart the energy count. Energy while a step
        // is pending is not counted against the old value either: if the
        // step confirms, that energy belongs to the new one.
        let capacity = f64::from(snap.battery_capacity_kwh);
        if self.pending.is_none() && capacity > 0.0 && capacity.is_finite() {
            let charging = snap.battery_power < 0;
            let discharging = snap.battery_power > 0;
            // At full and at the reserve the system SOC counts too: modules in
            // one pack read a few points apart while the inverter tops up or
            // holds the reserve.
            let resting = (self.soc.max(snap.soc) >= 100 && charging)
                || (self.soc.min(snap.soc) <= snap.battery_reserve.max(4) && discharging);
            if !resting {
                self.energy_kwh += f64::from(snap.battery_power) * elapsed as f64 / 3_600_000.0;
            }
            if !self.stuck_reported && self.energy_kwh.abs() >= STUCK_CAPACITY_FRACTION * capacity {
                self.stuck_reported = true;
                events.push(SocEventKind::Stuck {
                    soc: self.soc,
                    energy_kwh: self.energy_kwh,
                });
            }
        }
        events
    }
}

impl SocWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// The configured poll interval. A gap only restarts the watch when it
    /// is longer than [`MAX_GAP_SECS`] and three polls, so slow polling is
    /// still watched.
    pub fn set_poll_interval(&mut self, secs: u64) {
        self.poll_interval_secs = i64::try_from(secs).unwrap_or(i64::MAX);
    }

    fn max_gap_secs(&self) -> i64 {
        MAX_GAP_SECS.max(self.poll_interval_secs.saturating_mul(3))
    }

    /// Fold in one sanitized snapshot taken at `now_ts` (unix seconds).
    /// `settled` is false during the post-connect grace period, when values
    /// are not yet trusted. Returns the jumps and stuck episodes confirmed by
    /// this poll; each is reported once.
    pub fn observe(
        &mut self,
        snap: &InverterSnapshot,
        now_ts: i64,
        settled: bool,
    ) -> Vec<SocEvent> {
        // An HR 29 calibration drives the SOC to its limits on purpose.
        let calibrating =
            snap.supports_battery_calibration && (1..=6).contains(&snap.battery_calibration_stage);
        if !settled || calibrating {
            self.tracks.clear();
            self.last_fingerprints.clear();
            return Vec::new();
        }

        // A module SOC of 0 is a failed read (the BMS never reports a flat
        // module as 0 while it is answering), so it is skipped.
        let mut readings: Vec<(SocSource, u8)> = snap
            .battery_modules
            .iter()
            .filter(|m| (1..=100).contains(&m.soc))
            .map(|m| (SocSource::Module(m.index), m.soc))
            .collect();
        if readings.is_empty() && snap.soc <= 100 {
            readings.push((SocSource::System, snap.soc));
        }

        let mut events = Vec::new();
        let max_gap = self.max_gap_secs();
        for (source, soc) in readings {
            let fp = fingerprint(snap, source);
            let repeat = self.last_fingerprints.get(&source) == Some(&fp);
            self.last_fingerprints.insert(source, fp);
            match self.tracks.get_mut(&source) {
                Some(track) => events.extend(
                    track
                        .step(soc, now_ts, snap, repeat, max_gap)
                        .into_iter()
                        .map(|kind| SocEvent { source, kind }),
                ),
                None => {
                    self.tracks.insert(source, Track::new(soc, now_ts));
                }
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::BatteryModule;

    const T0: i64 = 1_790_000_000;

    /// A single-module system at `soc`, moving `battery_power` W
    /// (+ = discharging), 9.5 kWh, 4% reserve.
    fn snap(soc: u8, battery_power: i32) -> InverterSnapshot {
        InverterSnapshot {
            soc,
            battery_power,
            battery_capacity_kwh: 9.5,
            battery_reserve: 4,
            battery_modules: vec![BatteryModule {
                index: 0,
                soc,
                ..BatteryModule::default()
            }],
            ..InverterSnapshot::default()
        }
    }

    /// Feed `socs` at `interval` seconds apart, starting at T0, with the given
    /// battery power; return every event.
    fn run(watch: &mut SocWatch, socs: &[u8], interval: i64, power: i32) -> Vec<SocEvent> {
        socs.iter()
            .enumerate()
            .flat_map(|(i, &soc)| {
                watch.observe(&live(soc, power, i), T0 + i as i64 * interval, true)
            })
            .collect()
    }

    /// [`snap`] with the house load wobbling a few watts, as live data does.
    fn live(soc: u8, battery_power: i32, i: usize) -> InverterSnapshot {
        let mut s = InverterSnapshot {
            home_power: 500 + (i % 7) as i32,
            ..snap(soc, battery_power)
        };
        for m in &mut s.battery_modules {
            m.voltage = 51.0 + (i % 7) as f32 / 100.0;
        }
        s
    }

    fn jump(from: u8, to: u8) -> SocEvent {
        SocEvent {
            source: SocSource::Module(0),
            kind: SocEventKind::Jump { from, to },
        }
    }

    fn is_stuck(e: &SocEvent) -> bool {
        matches!(e.kind, SocEventKind::Stuck { .. })
    }

    // ---- jumps ---------------------------------------------------------

    #[test]
    fn allowance_grows_with_time() {
        assert_eq!(jump_allowance(0), 5.0);
        assert_eq!(jump_allowance(60), 7.0);
        assert_eq!(
            jump_allowance(-60),
            5.0,
            "a negative interval is not a credit"
        );
    }

    #[test]
    fn full_rate_discharge_is_not_a_jump_at_60s_polling() {
        // 5 kW from 9.5 kWh is about 0.9 points a minute; rounding can show 2.
        let socs: Vec<u8> = (0..60).map(|i| 90 - (i * 9 / 10) as u8).collect();
        let mut w = SocWatch::new();
        assert!(run(&mut w, &socs, 60, 5000).is_empty());
    }

    #[test]
    fn full_rate_discharge_is_not_a_jump_at_10s_polling() {
        let socs: Vec<u8> = (0..300).map(|i| 90 - (i / 6) as u8).collect();
        let mut w = SocWatch::new();
        assert!(run(&mut w, &socs, 10, 5000).is_empty());
    }

    #[test]
    fn a_one_poll_blip_is_not_a_jump() {
        let mut w = SocWatch::new();
        assert!(run(&mut w, &[50, 50, 20, 50, 50, 50, 50], 60, 0).is_empty());
    }

    #[test]
    fn a_two_poll_blip_is_not_a_jump() {
        let mut w = SocWatch::new();
        assert!(run(&mut w, &[50, 50, 20, 20, 50, 50, 50], 60, 0).is_empty());
    }

    #[test]
    fn a_held_step_is_reported_once_on_the_confirming_poll() {
        let mut w = SocWatch::new();
        let socs = [50, 50, 20, 20, 20, 20, 20, 20];
        let per_poll: Vec<Vec<SocEvent>> = socs
            .iter()
            .enumerate()
            .map(|(i, &soc)| w.observe(&live(soc, 0, i), T0 + i as i64 * 60, true))
            .collect();
        assert_eq!(per_poll[4], vec![jump(50, 20)], "third read of 20 confirms");
        let total: usize = per_poll.iter().map(Vec::len).sum();
        assert_eq!(total, 1);
    }

    #[test]
    fn a_step_up_is_a_jump_too() {
        let mut w = SocWatch::new();
        let events = run(&mut w, &[30, 30, 60, 60, 60], 60, 0);
        assert_eq!(events, vec![jump(30, 60)]);
    }

    #[test]
    fn a_step_that_wobbles_by_one_point_still_confirms() {
        let mut w = SocWatch::new();
        let events = run(&mut w, &[50, 20, 21, 20], 60, 0);
        assert_eq!(events, vec![jump(50, 20)]);
    }

    #[test]
    fn a_small_step_is_not_a_jump() {
        let mut w = SocWatch::new();
        assert!(run(&mut w, &[50, 45, 45, 45, 45], 60, 0).is_empty());
    }

    #[test]
    fn the_watch_continues_from_the_new_value_after_a_jump() {
        let mut w = SocWatch::new();
        let events = run(&mut w, &[80, 40, 40, 40, 10, 10, 10], 60, 0);
        assert_eq!(events, vec![jump(80, 40), jump(40, 10)]);
    }

    #[test]
    fn a_long_gap_starts_afresh() {
        let mut w = SocWatch::new();
        w.observe(&snap(50, 0), T0, true);
        for i in 0..5 {
            let events = w.observe(&snap(20, 0), T0 + MAX_GAP_SECS + 60 + i * 60, true);
            assert!(events.is_empty(), "reconnect after a gap is not a jump");
        }
    }

    #[test]
    fn the_clock_moving_backwards_starts_afresh() {
        let mut w = SocWatch::new();
        w.observe(&snap(50, 0), T0, true);
        for i in 0..5 {
            assert!(w.observe(&snap(20, 0), T0 - 3600 + i * 60, true).is_empty());
        }
    }

    #[test]
    fn grace_period_readings_are_not_compared_across() {
        let mut w = SocWatch::new();
        w.observe(&snap(50, 0), T0, true);
        w.observe(&snap(20, 0), T0 + 60, false);
        for i in 2..6 {
            assert!(w.observe(&snap(20, 0), T0 + i * 60, true).is_empty());
        }
    }

    #[test]
    fn an_inverter_calibration_suppresses_the_watch() {
        // HR 29 calibration drives the SOC to its limits on purpose.
        let mut w = SocWatch::new();
        let calibrating = |soc| InverterSnapshot {
            supports_battery_calibration: true,
            battery_calibration_stage: 2,
            ..snap(soc, 0)
        };
        w.observe(&snap(50, 0), T0, true);
        for i in 1..5 {
            assert!(w.observe(&calibrating(4), T0 + i * 60, true).is_empty());
        }
        // After calibration the new value is the baseline, not a jump.
        for i in 5..9 {
            assert!(w.observe(&snap(4, 0), T0 + i * 60, true).is_empty());
        }
    }

    #[test]
    fn a_module_reading_zero_is_a_failed_read_not_a_jump() {
        let mut w = SocWatch::new();
        let mut events = run(&mut w, &[50, 50], 60, 0);
        for i in 2..6 {
            let mut s = snap(50, 0);
            s.battery_modules[0].soc = 0;
            events.extend(w.observe(&s, T0 + i * 60, true));
        }
        assert!(events.is_empty());
    }

    #[test]
    fn each_module_is_watched_separately() {
        let two = |a: u8, b: u8| InverterSnapshot {
            battery_modules: vec![
                BatteryModule {
                    index: 0,
                    soc: a,
                    ..BatteryModule::default()
                },
                BatteryModule {
                    index: 1,
                    soc: b,
                    ..BatteryModule::default()
                },
            ],
            ..snap(50, 0)
        };
        let mut w = SocWatch::new();
        let events: Vec<_> = [(50, 50), (50, 50), (50, 15), (50, 15), (50, 15)]
            .iter()
            .enumerate()
            .flat_map(|(i, &(a, b))| {
                let mut s = InverterSnapshot {
                    home_power: 500 + i as i32,
                    ..two(a, b)
                };
                for m in &mut s.battery_modules {
                    m.voltage = 51.0 + i as f32 / 100.0; // live readings move
                }
                w.observe(&s, T0 + i as i64 * 60, true)
            })
            .collect();
        assert_eq!(
            events,
            vec![SocEvent {
                source: SocSource::Module(1),
                kind: SocEventKind::Jump { from: 50, to: 15 },
            }]
        );
    }

    #[test]
    fn the_system_soc_is_watched_when_no_module_reports_one() {
        let mut w = SocWatch::new();
        let events: Vec<_> = [70u8, 70, 30, 30, 30]
            .iter()
            .enumerate()
            .flat_map(|(i, &soc)| {
                let mut s = live(soc, 0, i);
                s.battery_modules[0].soc = 0; // HV BMU modules carry no SOC
                w.observe(&s, T0 + i as i64 * 60, true)
            })
            .collect();
        assert_eq!(
            events,
            vec![SocEvent {
                source: SocSource::System,
                kind: SocEventKind::Jump { from: 70, to: 30 },
            }]
        );
    }

    // ---- stuck ---------------------------------------------------------

    /// 15% of 9.5 kWh is 1.425 kWh: 43 minutes at 2 kW.
    const STUCK_MINUTES_AT_2KW: usize = 43;

    #[test]
    fn soc_held_while_energy_flows_is_stuck_once() {
        let mut w = SocWatch::new();
        let socs = vec![60u8; STUCK_MINUTES_AT_2KW + 30];
        let events = run(&mut w, &socs, 60, 2000);
        assert_eq!(events.len(), 1, "{events:?}");
        match &events[0].kind {
            SocEventKind::Stuck { soc, energy_kwh } => {
                assert_eq!(*soc, 60);
                assert!(
                    *energy_kwh >= 0.15 * 9.5 && *energy_kwh < 1.5,
                    "{energy_kwh}"
                );
            }
            other => panic!("expected stuck, got {other:?}"),
        }
        assert_eq!(events[0].source, SocSource::Module(0));
    }

    #[test]
    fn soc_held_just_short_of_the_threshold_is_not_stuck() {
        let mut w = SocWatch::new();
        let socs = vec![60u8; STUCK_MINUTES_AT_2KW - 1];
        assert!(run(&mut w, &socs, 60, 2000).is_empty());
    }

    #[test]
    fn stuck_while_charging_counts_too() {
        let mut w = SocWatch::new();
        let events = run(&mut w, &[40u8; STUCK_MINUTES_AT_2KW + 2], 60, -2000);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0].kind,
            SocEventKind::Stuck { energy_kwh, .. } if energy_kwh < 0.0
        ));
    }

    #[test]
    fn a_moving_soc_is_never_stuck() {
        // One point every 3 minutes: about right for 2 kW on 9.5 kWh.
        let socs: Vec<u8> = (0..120).map(|i| 90 - (i / 3) as u8).collect();
        let mut w = SocWatch::new();
        assert!(run(&mut w, &socs, 60, 2000).is_empty());
    }

    #[test]
    fn energy_in_and_out_cancels() {
        let mut w = SocWatch::new();
        let mut events = Vec::new();
        for i in 0..200 {
            let power = if i % 2 == 0 { 2000 } else { -2000 };
            events.extend(w.observe(&snap(60, power), T0 + i * 60, true));
        }
        assert!(events.is_empty());
    }

    #[test]
    fn full_while_charging_is_not_stuck() {
        let mut w = SocWatch::new();
        assert!(run(&mut w, &[100u8; 200], 60, -2000).is_empty());
    }

    #[test]
    fn at_the_reserve_while_discharging_is_not_stuck() {
        let mut w = SocWatch::new();
        assert!(run(&mut w, &[4u8; 200], 60, 2000).is_empty());
    }

    #[test]
    fn full_while_discharging_is_stuck() {
        // Leaving 100% is exactly when a frozen SOC shows.
        let mut w = SocWatch::new();
        assert_eq!(
            run(&mut w, &[100u8; STUCK_MINUTES_AT_2KW + 2], 60, 2000).len(),
            1
        );
    }

    #[test]
    fn unknown_capacity_never_reports_stuck() {
        let mut w = SocWatch::new();
        let mut events = Vec::new();
        for i in 0..200 {
            let mut s = snap(60, 2000);
            s.battery_capacity_kwh = 0.0;
            events.extend(w.observe(&s, T0 + i * 60, true));
        }
        assert!(events.is_empty());
    }

    #[test]
    fn a_gap_restarts_the_energy_count() {
        let mut w = SocWatch::new();
        let half = STUCK_MINUTES_AT_2KW / 2 + 5;
        let mut events = Vec::new();
        for i in 0..half as i64 {
            events.extend(w.observe(&snap(60, 2000), T0 + i * 60, true));
        }
        let resume = T0 + half as i64 * 60 + MAX_GAP_SECS + 60;
        for i in 0..half as i64 {
            events.extend(w.observe(&snap(60, 2000), resume + i * 60, true));
        }
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn a_corrupt_read_mid_episode_does_not_restart_the_count() {
        let mut w = SocWatch::new();
        let mut socs = vec![60u8; STUCK_MINUTES_AT_2KW + 2];
        socs[20] = 5; // one bad module read, springs straight back
        let events = run(&mut w, &socs, 60, 2000);
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(is_stuck(&events[0]));
    }

    #[test]
    fn a_jump_while_the_battery_keeps_moving_still_confirms() {
        // 5.2 kWh at 3.6 kW is over a point a minute: after the step the
        // SOC keeps falling, so the reads never sit within a point of 20.
        let mut w = SocWatch::new();
        let mut events = Vec::new();
        for (i, soc) in [60u8, 60, 20, 19, 17, 16, 15, 14, 12, 11, 10, 9]
            .into_iter()
            .enumerate()
        {
            let s = InverterSnapshot {
                battery_capacity_kwh: 5.2,
                ..live(soc, 3600, i)
            };
            events.extend(w.observe(&s, T0 + i as i64 * 60, true));
        }
        assert_eq!(events, vec![jump(60, 20)], "and no false stuck report");
    }

    #[test]
    fn energy_during_an_unconfirmed_step_is_not_counted_as_stuck() {
        // 41 minutes at 60% (just short), then two reads of a step that
        // springs back: counting those two minutes would tip it over.
        let mut socs = vec![60u8; STUCK_MINUTES_AT_2KW - 1];
        socs.extend([20, 20, 60]);
        let mut w = SocWatch::new();
        let events = run(&mut w, &socs, 60, 2000);
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn a_module_above_the_reserve_while_the_system_sits_at_it_is_resting() {
        let mut w = SocWatch::new();
        let mut events = Vec::new();
        for i in 0..200 {
            let mut s = live(7, 2000, i);
            s.soc = 4; // the system is at its 4% reserve
            events.extend(w.observe(&s, T0 + i as i64 * 60, true));
        }
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn an_eight_point_step_at_60s_polling_is_a_jump() {
        // The allowance must not keep growing while the step is checked.
        let mut w = SocWatch::new();
        assert_eq!(
            run(&mut w, &[50, 50, 42, 42, 42], 60, 0),
            vec![jump(50, 42)]
        );
    }

    #[test]
    fn a_held_step_at_slow_polling_is_a_jump() {
        let mut w = SocWatch::new();
        assert_eq!(
            run(&mut w, &[50, 50, 20, 20, 20], 300, 0),
            vec![jump(50, 20)]
        );
    }

    #[test]
    fn a_module_below_full_while_the_system_reads_full_is_resting() {
        // Top balancing: the pack reads 100% and keeps charging gently while
        // one module reads a point or two lower.
        let mut w = SocWatch::new();
        let mut events = Vec::new();
        for i in 0..200 {
            let mut s = live(98, -2000, i);
            s.soc = 100;
            events.extend(w.observe(&s, T0 + i as i64 * 60, true));
        }
        assert!(events.is_empty(), "{events:?}");
    }

    /// Two modules: #1 live and discharging normally, #2 carried forward by
    /// the sanitizer (its BMS stopped answering) at 60%, then answering
    /// again at 30%.
    fn carried_forward(i: usize, second_soc: u8, second_live: bool) -> InverterSnapshot {
        let first_soc = 90 - (i / 3) as u8;
        let second = BatteryModule {
            index: 1,
            soc: second_soc,
            voltage: if second_live {
                51.0 + i as f32 / 100.0
            } else {
                52.0
            },
            cell_voltages: vec![
                3.30,
                if second_live {
                    3.30 + i as f32 / 10_000.0
                } else {
                    3.31
                },
            ],
            ..BatteryModule::default()
        };
        InverterSnapshot {
            battery_modules: vec![
                BatteryModule {
                    index: 0,
                    soc: first_soc,
                    voltage: 51.0 + i as f32 / 100.0,
                    ..BatteryModule::default()
                },
                second,
            ],
            ..live(first_soc, 2000, i)
        }
    }

    #[test]
    fn a_module_carried_forward_is_neither_stuck_nor_jumping_on_its_return() {
        let mut w = SocWatch::new();
        let mut events = Vec::new();
        for i in 0..3 {
            events.extend(w.observe(&carried_forward(i, 60, true), T0 + i as i64 * 60, true));
        }
        for i in 3..120 {
            events.extend(w.observe(&carried_forward(i, 60, false), T0 + i as i64 * 60, true));
        }
        for i in 120..126 {
            events.extend(w.observe(&carried_forward(i, 30, true), T0 + i as i64 * 60, true));
        }
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn a_module_with_no_readings_of_its_own_is_judged_on_the_snapshot() {
        // A module reporting only its SOC must not look like a repeat on
        // every poll, or a held step on it could never confirm.
        let mut w = SocWatch::new();
        let mut events = Vec::new();
        for (i, soc) in [50u8, 50, 20, 20, 20].into_iter().enumerate() {
            let mut s = live(soc, 0, i);
            s.battery_modules[0].voltage = 0.0;
            events.extend(w.observe(&s, T0 + i as i64 * 60, true));
        }
        assert_eq!(events, vec![jump(50, 20)]);
    }

    #[test]
    fn slow_polling_is_still_watched() {
        let mut w = SocWatch::new();
        w.set_poll_interval(900);
        assert_eq!(
            run(&mut w, &[80, 80, 30, 30, 30], 900, 0),
            vec![jump(80, 30)]
        );

        let mut w = SocWatch::new();
        w.set_poll_interval(900);
        let events = run(&mut w, &[60u8; 6], 900, 2000);
        assert_eq!(
            events.iter().filter(|e| is_stuck(e)).count(),
            1,
            "{events:?}"
        );
    }

    #[test]
    fn slow_polling_still_restarts_after_a_real_gap() {
        let mut w = SocWatch::new();
        w.set_poll_interval(900);
        w.observe(&live(80, 0, 0), T0, true);
        let resume = T0 + 4 * 900;
        let mut events = Vec::new();
        for i in 0..4 {
            events.extend(w.observe(&live(30, 0, i + 1), resume + i as i64 * 900, true));
        }
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn a_dongle_repeating_the_same_snapshot_is_not_stuck() {
        // The StaleData dongle fault: every value frozen, power included.
        let mut w = SocWatch::new();
        let mut events = Vec::new();
        for i in 0..200 {
            events.extend(w.observe(&snap(60, 2000), T0 + i * 60, true));
        }
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn repeated_snapshots_do_not_confirm_a_jump() {
        // One corrupt read, then the dongle repeats it.
        let mut w = SocWatch::new();
        let mut events = run(&mut w, &[50, 50], 60, 0);
        for i in 2..8 {
            events.extend(w.observe(&snap(20, 0), T0 + i * 60, true));
        }
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn a_soc_change_ends_the_episode_and_a_new_one_can_follow() {
        let mut w = SocWatch::new();
        let mut socs = vec![60u8; STUCK_MINUTES_AT_2KW + 2];
        socs.extend(vec![59u8; STUCK_MINUTES_AT_2KW + 2]);
        let events = run(&mut w, &socs, 60, 2000);
        assert_eq!(events.iter().filter(|e| is_stuck(e)).count(), 2);
    }
}
