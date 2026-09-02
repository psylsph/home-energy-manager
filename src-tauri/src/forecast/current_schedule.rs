//! What-if SOC projection under the inverter's CURRENTLY configured
//! schedule (issue #297).
//!
//! The Phase-1 projection ([`crate::forecast::simulate`]) models pure Eco
//! behaviour — no timed windows at all — and the planner's
//! `with_charge_series` models the *recommended* slot being adopted.
//! Neither answers "what happens tonight if I change nothing?". This
//! module re-runs the same eco simulation with the snapshot's enabled
//! charge/discharge slots injected as timed windows: charge slots drive
//! the battery towards their target SOC at the configured charge rate;
//! discharge slots drain towards their floor at the configured discharge
//! rate.
//!
//! Modelling notes:
//! - Charge windows take priority when they overlap a discharge window
//!   (the inverter cannot charge and export-discharge the same hour).
//! - A charge slot stops injecting once the running SOC reaches the
//!   slot's target, matching the inverter's behaviour.
//! - A discharge slot's floor (its per-slot target SOC, or the battery
//!   reserve when unset — the decoder stamps an effective value into
//!   every enabled slot) also bounds household drain for that hour,
//!   because the underlying per-hour simulation is given the raised
//!   floor. Household demand outside slot hours keeps draining to the
//!   global reserve as in Eco.

use crate::forecast::simulate::{simulate_battery, SimHourInput, SimulationParams};
use crate::inverter::model::InverterSnapshot;
use chrono::Timelike;

/// One timed window: an enabled inverter slot, resolved to wall-clock
/// minutes and kW.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledWindow {
    /// Window start, minutes of day (0..=1439).
    pub start_min: u16,
    /// Window end, minutes of day (0..=1439). `<= start_min` means the
    /// slot wraps midnight (e.g. 23:30–06:00).
    pub end_min: u16,
    /// Power limit in kW while the window is active (AC side).
    pub rate_kw: f64,
    /// SOC the window drives towards, %: a charge slot stops at this
    /// target; a discharge slot stops at this floor.
    pub target_soc_pct: f64,
}

/// The snapshot's effective schedule, split by direction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CurrentSchedule {
    pub charge_windows: Vec<ScheduledWindow>,
    pub discharge_windows: Vec<ScheduledWindow>,
}

impl CurrentSchedule {
    /// Extract the enabled slots from the snapshot, gated on the
    /// inverter's timed-charge / timed-export arm flags — a slot the
    /// inverter will not honour must not shape the projection. The rate
    /// limits are the model-aware values from
    /// [`crate::forecast::battery_rate_limits_kw`] (the rate a slot
    /// actually charges/discharges at).
    pub fn from_snapshot(
        snapshot: &InverterSnapshot,
        charge_limit_kw: f64,
        discharge_limit_kw: f64,
    ) -> Self {
        let _ = (snapshot, charge_limit_kw, discharge_limit_kw);
        Self::default()
    }

    /// True when no slot would run — the projection must then be
    /// omitted (it would duplicate the Eco line).
    pub fn is_empty(&self) -> bool {
        self.charge_windows.is_empty() && self.discharge_windows.is_empty()
    }
}

/// Fraction of an hour (0.0–1.0) the hour bucket starting at `timestamp`
/// spends inside `window`. Midnight-wrapping windows are handled by
/// splitting them at 24:00.
fn window_overlap_hours(timestamp: i64, window: &ScheduledWindow) -> f64 {
    let Some(dt) = chrono::DateTime::from_timestamp(timestamp, 0) else {
        return 0.0;
    };
    let local = dt.with_timezone(&chrono::Local);
    let hour_start = local.hour() as u16 * 60 + local.minute() as u16;
    let hour_end = hour_start.saturating_add(60);
    let overlap = |start: u16, end: u16| hour_end.min(end).saturating_sub(hour_start.max(start));
    let overlap_min = if window.end_min > window.start_min {
        overlap(window.start_min, window.end_min)
    } else if window.end_min < window.start_min {
        overlap(window.start_min, 1440) + overlap(0, window.end_min)
    } else {
        0
    };
    f64::from(overlap_min) / 60.0
}

/// Run the eco simulation with the schedule's timed windows applied to
/// every occurrence across the horizon. Returns `(timestamp, SOC at the
/// end of the hour)` pairs, ascending — empty when the parameters are
/// invalid (no capacity, unknown rates, out-of-range start SOC) or the
/// hour series is empty.
pub fn simulate_current_schedule(
    hours: &[SimHourInput],
    params: &SimulationParams,
    schedule: &CurrentSchedule,
) -> Vec<(i64, f64)> {
    let _ = (hours, params, schedule);
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::{InverterSnapshot, ScheduleSlot};
    use chrono::TimeZone;

    fn params() -> SimulationParams {
        SimulationParams {
            capacity_kwh: 10.0,
            start_soc_pct: 30.0,
            reserve_soc_pct: 10.0,
            max_charge_kw: 3.0,
            max_discharge_kw: 3.0,
            charge_efficiency: 0.9,
            discharge_efficiency: 0.95,
        }
    }

    fn slot(start_h: u8, start_m: u8, end_h: u8, end_m: u8, target: u8) -> ScheduleSlot {
        ScheduleSlot {
            enabled: true,
            start_hour: start_h,
            start_minute: start_m,
            end_hour: end_h,
            end_minute: end_m,
            target_soc: target,
        }
    }

    fn schedule_snapshot(
        enable_charge: bool,
        enable_discharge: bool,
        charge: [ScheduleSlot; 10],
        discharge: [ScheduleSlot; 10],
    ) -> InverterSnapshot {
        InverterSnapshot {
            enable_charge,
            enable_discharge,
            charge_slots: charge,
            discharge_slots: discharge,
            ..Default::default()
        }
    }

    fn disabled_slot() -> ScheduleSlot {
        ScheduleSlot {
            enabled: false,
            start_hour: 1,
            start_minute: 0,
            end_hour: 2,
            end_minute: 0,
            target_soc: 100,
        }
    }

    fn all_disabled() -> [ScheduleSlot; 10] {
        std::array::from_fn(|_| ScheduleSlot::default())
    }

    fn hour_series(day_start_ts: i64, hours: usize, solar: f64, cons: f64) -> Vec<SimHourInput> {
        (0..hours)
            .map(|h| SimHourInput {
                timestamp: day_start_ts + h as i64 * 3600,
                solar_kwh: solar,
                consumption_kwh: cons,
            })
            .collect()
    }

    // --- from_snapshot gating -------------------------------------------

    #[test]
    fn enabled_charge_slot_becomes_a_charge_window() {
        let mut charge = all_disabled();
        charge[0] = slot(2, 0, 5, 0, 65);
        let snap = schedule_snapshot(true, false, charge, all_disabled());
        let schedule = CurrentSchedule::from_snapshot(&snap, 3.0, 3.0);
        assert_eq!(
            schedule.charge_windows,
            vec![ScheduledWindow {
                start_min: 120,
                end_min: 300,
                rate_kw: 3.0,
                target_soc_pct: 65.0,
            }]
        );
        assert!(schedule.discharge_windows.is_empty());
        assert!(!schedule.is_empty());
    }

    #[test]
    fn enabled_discharge_slot_becomes_a_discharge_window() {
        let mut discharge = all_disabled();
        discharge[0] = slot(16, 30, 18, 0, 20);
        let snap = schedule_snapshot(false, true, all_disabled(), discharge);
        let schedule = CurrentSchedule::from_snapshot(&snap, 3.0, 2.5);
        assert_eq!(
            schedule.discharge_windows,
            vec![ScheduledWindow {
                start_min: 990,
                end_min: 1080,
                rate_kw: 2.5,
                target_soc_pct: 20.0,
            }]
        );
        assert!(schedule.charge_windows.is_empty());
    }

    #[test]
    fn disabled_slots_and_unset_flags_are_ignored() {
        let mut charge = all_disabled();
        charge[0] = slot(2, 0, 5, 0, 65);
        charge[1] = disabled_slot();
        let mut discharge = all_disabled();
        discharge[0] = slot(16, 0, 18, 0, 20);
        // Charge flag off: the charge slot must not run.
        let snap = schedule_snapshot(false, true, charge.clone(), discharge.clone());
        let schedule = CurrentSchedule::from_snapshot(&snap, 3.0, 3.0);
        assert!(schedule.charge_windows.is_empty());
        assert_eq!(schedule.discharge_windows.len(), 1);
        // Discharge flag off: the discharge slot must not run either.
        let snap = schedule_snapshot(true, false, charge, discharge);
        let schedule = CurrentSchedule::from_snapshot(&snap, 3.0, 3.0);
        assert_eq!(schedule.charge_windows.len(), 1);
        assert!(schedule.discharge_windows.is_empty());
    }

    #[test]
    fn snapshot_without_any_slots_is_empty() {
        let snap = schedule_snapshot(true, true, all_disabled(), all_disabled());
        assert!(CurrentSchedule::from_snapshot(&snap, 3.0, 3.0).is_empty());
    }

    // --- simulation ------------------------------------------------------

    #[test]
    fn empty_schedule_simulates_nothing() {
        let out = simulate_current_schedule(
            &hour_series(0, 3, 0.0, 0.5),
            &params(),
            &CurrentSchedule::default(),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn invalid_params_simulate_nothing() {
        let schedule = CurrentSchedule {
            charge_windows: vec![ScheduledWindow {
                start_min: 0,
                end_min: 300,
                rate_kw: 3.0,
                target_soc_pct: 65.0,
            }],
            discharge_windows: Vec::new(),
        };
        let mut p = params();
        p.capacity_kwh = 0.0;
        let out = simulate_current_schedule(&hour_series(0, 3, 0.0, 0.5), &p, &schedule);
        assert!(out.is_empty());
    }

    #[test]
    fn charge_slot_stops_at_target_not_full() {
        // 10 kWh pack at 30%, slot 00:00–05:00 charging at 3 kW to 65%.
        // Hour 1 charges the full 3 kW (2.7 kWh stored → 57%); hour 2
        // tops up the remaining 8 pp; hours 3–5 hold at exactly 65%.
        let day = chrono::Local
            .with_ymd_and_hms(2025, 6, 15, 0, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        let schedule = CurrentSchedule {
            charge_windows: vec![ScheduledWindow {
                start_min: 0,
                end_min: 300,
                rate_kw: 3.0,
                target_soc_pct: 65.0,
            }],
            discharge_windows: Vec::new(),
        };
        let out = simulate_current_schedule(
            &hour_series(day, 8, 0.0, 0.0),
            &params(),
            &schedule,
        );
        assert_eq!(out.len(), 8);
        assert!((out[0].1 - 57.0).abs() < 1e-6, "hour 1 = {}", out[0].1);
        assert!((out[1].1 - 65.0).abs() < 1e-6, "hour 2 = {}", out[1].1);
        for (ts, soc) in out.iter().skip(2) {
            assert!(
                (soc - 65.0).abs() < 1e-6,
                "hour {} must hold 65%, got {soc}",
                (ts - day) / 3600
            );
        }
    }

    #[test]
    fn charge_slot_never_exceeds_target_even_with_solar_surplus() {
        // Slot target 65% while the sun would push the battery higher:
        // outside the slot hours Eco charges freely, but DURING the slot
        // the injection must not lift SOC past the target on its own.
        let day = chrono::Local
            .with_ymd_and_hms(2025, 6, 15, 0, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        let schedule = CurrentSchedule {
            charge_windows: vec![ScheduledWindow {
                start_min: 0,
                end_min: 120,
                rate_kw: 1.0,
                target_soc_pct: 40.0,
            }],
            discharge_windows: Vec::new(),
        };
        // 2 kWh/h surplus from 08:00 would fill the pack by Eco anyway —
        // only the in-slot hours must respect the target.
        let mut hours = hour_series(day, 10, 0.0, 0.0);
        for h in hours.iter_mut().skip(8) {
            h.solar_kwh = 2.0;
        }
        let out = simulate_current_schedule(&hours, &params(), &schedule);
        // In-slot: hour 1 charges 0.9 kWh stored (30→39%), hour 2 the
        // remaining 1 pp → 40%.
        assert!((out[0].1 - 39.0).abs() < 1e-6, "hour 1 = {}", out[0].1);
        assert!((out[1].1 - 40.0).abs() < 1e-6, "hour 2 = {}", out[1].1);
        // Eco resumes after the slot and surplus fills the rest.
        assert!(out[9].1 > 40.0);
    }

    #[test]
    fn discharge_slot_drains_to_floor_and_holds() {
        // 10 kWh pack at 80%, reserve 10%, slot 00:00–03:00 exporting at
        // 2 kW down to a 30% floor (above the reserve). The pack must
        // bottom out at exactly 30% — not at the 10% reserve the Eco
        // projection would drain to — and hold there.
        let day = chrono::Local
            .with_ymd_and_hms(2025, 6, 15, 0, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        let mut p = params();
        p.start_soc_pct = 80.0;
        let schedule = CurrentSchedule {
            charge_windows: Vec::new(),
            discharge_windows: vec![ScheduledWindow {
                start_min: 0,
                end_min: 180,
                rate_kw: 2.0,
                target_soc_pct: 30.0,
            }],
        };
        // 0.2 kWh/h household load on top of the export drain.
        let out = simulate_current_schedule(&hour_series(day, 6, 0.0, 0.2), &p, &schedule);
        assert_eq!(out.len(), 6);
        for (ts, soc) in out.iter() {
            assert!(
                *soc >= 30.0 - 1e-6,
                "hour {} below slot floor: {soc}",
                (ts - day) / 3600
            );
        }
        assert!(
            (out[5].1 - 30.0).abs() < 1e-6,
            "must settle at the 30% floor, got {}",
            out[5].1
        );
    }

    #[test]
    fn wrapping_slot_overlaps_after_midnight() {
        // 23:30–06:00 slot: the 01:00 hour must be fully inside it, the
        // 07:00 hour fully outside.
        let day = chrono::Local
            .with_ymd_and_hms(2025, 6, 15, 0, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        let window = ScheduledWindow {
            start_min: 23 * 60 + 30,
            end_min: 6 * 60,
            rate_kw: 3.0,
            target_soc_pct: 65.0,
        };
        let at = |h: u32| {
            window_overlap_hours(
                day + i64::from(h) * 3600,
                &window,
            )
        };
        assert!((at(23) - 0.5).abs() < 1e-9, "23:00 overlap = {}", at(23));
        assert!((at(1) - 1.0).abs() < 1e-9, "01:00 overlap = {}", at(1));
        assert!((at(6) - 0.0).abs() < 1e-9, "06:00 overlap = {}", at(6));
        assert!(window_overlap_hours(day + 3600, &window) > 0.0);
    }

    #[test]
    fn charge_priority_when_slots_overlap() {
        // A charge and a discharge window covering the same hour: the
        // inverter charges; the projection must not do both.
        let day = chrono::Local
            .with_ymd_and_hms(2025, 6, 15, 0, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        let schedule = CurrentSchedule {
            charge_windows: vec![ScheduledWindow {
                start_min: 0,
                end_min: 120,
                rate_kw: 3.0,
                target_soc_pct: 65.0,
            }],
            discharge_windows: vec![ScheduledWindow {
                start_min: 0,
                end_min: 120,
                rate_kw: 3.0,
                target_soc_pct: 10.0,
            }],
        };
        let out = simulate_current_schedule(&hour_series(day, 2, 0.0, 0.0), &params(), &schedule);
        // Hour 1 must be a charge hour (SOC rises), never a net drain.
        assert!(out[0].1 > params().start_soc_pct, "hour 1 = {}", out[0].1);
    }

    #[test]
    fn sim_degrades_to_eco_outside_windows() {
        // Outside slot hours the projection must equal the plain Eco
        // simulation hour-for-hour: the schedule line only diverges
        // where a window is active.
        let day = chrono::Local
            .with_ymd_and_hms(2025, 6, 15, 0, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        let schedule = CurrentSchedule {
            charge_windows: vec![ScheduledWindow {
                start_min: 300,
                end_min: 360,
                rate_kw: 3.0,
                target_soc_pct: 65.0,
            }],
            discharge_windows: Vec::new(),
        };
        let hours = hour_series(day, 4, 0.0, 0.5);
        let out = simulate_current_schedule(&hours, &params(), &schedule);
        let eco = simulate_battery(&hours, &params());
        for (i, h) in eco.hours.iter().enumerate().take(3) {
            assert!(
                (out[i].1 - h.soc_pct).abs() < 1e-9,
                "hour {i} diverges: schedule {} vs eco {}",
                out[i].1,
                h.soc_pct
            );
        }
    }
}
