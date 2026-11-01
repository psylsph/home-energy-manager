//! Battery state-of-charge projection (issue #283, Phase 1).
//!
//! Deterministic hourly step simulation of Eco-mode behaviour: predicted
//! solar feeds home load first; surplus charges the battery (bounded by
//! the configured rate and scaled by charge efficiency) until full, the
//! remainder exports; deficit discharges the battery (bounded by rate,
//! scaled by discharge efficiency) down to the reserve SOC, the remainder
//! imports. No timed-charge/discharge windows — the planner (Phase 2)
//! builds on top of this primitive. Planned car charging adds to the load,
//! and a dispatch hold stops discharge for its share of the hour (#406).

/// Per-hour inputs the simulator consumes (all kWh, one-hour buckets).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SimHourInput {
    pub timestamp: i64,
    /// Predicted solar generation for the hour.
    pub solar_kwh: f64,
    /// Predicted home consumption for the hour.
    pub consumption_kwh: f64,
    /// Planned car charging on top of `consumption_kwh`, from the account's
    /// Intelligent Octopus dispatches (issue #406).
    pub ev_kwh: f64,
    /// Share of the hour (0–1) a smart dispatch holds battery discharge for
    /// (issue #394), spread evenly across the hour.
    pub held_fraction: f64,
}

impl SimHourInput {
    /// The part of this hour that is `fraction` of it long: energies scale,
    /// the timestamp and the held share stay. Every split-hour simulation
    /// builds its segments through this, so none of them drops a field.
    pub(crate) fn segment(&self, fraction: f64) -> Self {
        Self {
            solar_kwh: self.solar_kwh * fraction,
            consumption_kwh: self.consumption_kwh * fraction,
            ev_kwh: self.ev_kwh * fraction,
            ..*self
        }
    }

    /// The car's planned energy; anything that isn't a finite, positive
    /// figure counts as no car.
    pub(crate) fn car_kwh(&self) -> f64 {
        if self.ev_kwh.is_finite() && self.ev_kwh > 0.0 {
            self.ev_kwh
        } else {
            0.0
        }
    }

    /// The share of the hour the battery may discharge in: all of it,
    /// less the time a dispatch holds it. A held share that isn't a finite
    /// number counts as no hold.
    pub(crate) fn discharge_share(&self) -> f64 {
        if self.held_fraction.is_finite() {
            1.0 - self.held_fraction.clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    /// Everything drawn in the hour: the house and the car.
    pub(crate) fn load_kwh(&self) -> f64 {
        self.consumption_kwh + self.car_kwh()
    }

    /// Load solar doesn't cover.
    pub(crate) fn net_load_kwh(&self) -> f64 {
        (self.load_kwh() - self.solar_kwh).max(0.0)
    }

    /// Solar left after the load.
    pub(crate) fn surplus_kwh(&self) -> f64 {
        (self.solar_kwh - self.load_kwh()).max(0.0)
    }

    /// The `fraction` of this hour split into the time the battery may
    /// discharge (`held_fraction` 0) and the time a dispatch holds it
    /// (`held_fraction` 1), each with its share of the energy and its
    /// length as a fraction of the hour. Empty parts are left out.
    ///
    /// Simulating the parts one after the other is exact, where scaling a
    /// discharge by the held share is not once a caller adds energy of its
    /// own (a forced export would be cut twice). The free part runs first:
    /// where in the hour the hold falls isn't known.
    pub(crate) fn held_parts(&self, fraction: f64) -> impl Iterator<Item = (Self, f64)> {
        let free = self.discharge_share();
        let hour = *self;
        [(free, 0.0), (1.0 - free, 1.0)]
            .into_iter()
            .filter(|(share, _)| *share > 0.0)
            .map(move |(share, held)| {
                let part = fraction * share;
                (
                    Self {
                        held_fraction: held,
                        ..hour.segment(part)
                    },
                    part,
                )
            })
    }
}

/// Everything the simulation needs besides the hour series.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimulationParams {
    /// Usable battery capacity, kWh.
    pub capacity_kwh: f64,
    /// Starting SOC, %.
    pub start_soc_pct: f64,
    /// Reserve SOC floor the simulation will not discharge below, %.
    pub reserve_soc_pct: f64,
    /// Max AC charge power, kW.
    pub max_charge_kw: f64,
    /// Max AC discharge power, kW.
    pub max_discharge_kw: f64,
    /// AC→battery efficiency, 0–1 (default 0.9).
    pub charge_efficiency: f64,
    /// Battery→AC efficiency, 0–1 (default 0.95).
    pub discharge_efficiency: f64,
}

/// Result for one simulated hour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimHourResult {
    pub timestamp: i64,
    /// SOC at the END of the hour, %.
    pub soc_pct: f64,
    /// Grid import (AC kWh drawn from the grid).
    pub import_kwh: f64,
    /// Grid export (AC kWh pushed to the grid).
    pub export_kwh: f64,
    /// AC kWh used to charge the battery this hour.
    pub charge_kwh: f64,
    /// AC kWh delivered by discharging this hour.
    pub discharge_kwh: f64,
}

/// Whole-simulation output.
#[derive(Debug, Clone, PartialEq)]
pub struct SimulationOutput {
    pub hours: Vec<SimHourResult>,
    pub total_import_kwh: f64,
    pub total_export_kwh: f64,
}

/// Run the hourly simulation. Hours are processed in the order given —
/// callers pass ascending timestamps. A non-positive capacity or an
/// out-of-range start SOC yields no simulation (empty output), matching
/// the API's "no battery" degradation path.
pub fn simulate_battery(hours: &[SimHourInput], params: &SimulationParams) -> SimulationOutput {
    // Guards: no capacity / unknown max rates / out-of-range start SOC
    // mean we cannot project — the API reports `no_battery_capacity` /
    // empty output instead of guessing.
    if !params.capacity_kwh.is_finite()
        || !params.start_soc_pct.is_finite()
        || !params.reserve_soc_pct.is_finite()
        || !params.max_charge_kw.is_finite()
        || !params.max_discharge_kw.is_finite()
        || !params.charge_efficiency.is_finite()
        || !params.discharge_efficiency.is_finite()
        || params.capacity_kwh <= 0.0
        || params.max_charge_kw <= 0.0
        || params.max_discharge_kw <= 0.0
        || !(0.0..=100.0).contains(&params.start_soc_pct)
    {
        return SimulationOutput {
            hours: Vec::new(),
            total_import_kwh: 0.0,
            total_export_kwh: 0.0,
        };
    }

    let eta_c = params.charge_efficiency.clamp(0.01, 1.0);
    let eta_d = params.discharge_efficiency.clamp(0.01, 1.0);
    let capacity = params.capacity_kwh;
    let mut stored = params.start_soc_pct / 100.0 * capacity;
    let reserve_stored = params.reserve_soc_pct.clamp(0.0, 100.0) / 100.0 * capacity;

    let mut out = SimulationOutput {
        hours: Vec::with_capacity(hours.len()),
        total_import_kwh: 0.0,
        total_export_kwh: 0.0,
    };

    for h in hours {
        let (mut charge, mut discharge, mut import, mut export) = (0.0, 0.0, 0.0, 0.0);
        // A dispatch hold pauses discharge for its share of the hour; the
        // grid supplies the house and the car through it. Rate limits apply
        // to each part for its length.
        for (part, length) in h.held_parts(1.0) {
            let net = part.solar_kwh - part.load_kwh();
            if net >= 0.0 {
                let room = capacity - stored;
                let charge_ac = net.min(params.max_charge_kw * length).min(room / eta_c);
                stored += charge_ac * eta_c;
                charge += charge_ac;
                export += net - charge_ac;
            } else {
                let need = -net;
                let discharge_ac = if part.discharge_share() > 0.0 {
                    let available_dc = (stored - reserve_stored).max(0.0);
                    need.min(params.max_discharge_kw * length)
                        .min(available_dc * eta_d)
                } else {
                    0.0
                };
                stored -= discharge_ac / eta_d;
                discharge += discharge_ac;
                import += need - discharge_ac;
            }
        }

        // Numerical safety: never report SOC outside [0, 100].
        stored = stored.clamp(0.0, capacity);
        out.hours.push(SimHourResult {
            timestamp: h.timestamp,
            soc_pct: stored / capacity * 100.0,
            import_kwh: import.max(0.0),
            export_kwh: export.max(0.0),
            charge_kwh: charge.max(0.0),
            discharge_kwh: discharge.max(0.0),
        });
        out.total_import_kwh += import.max(0.0);
        out.total_export_kwh += export.max(0.0);
    }

    out
}

/// Simulate one real-time segment of an hourly input. The input energy must
/// already be scaled to the segment's fraction of an hour; this helper scales
/// the power limits, allowing a timed window to be composed with ordinary Eco
/// behaviour without dropping the part of the bucket outside that window.
pub(crate) fn simulate_battery_segment(
    hour: SimHourInput,
    params: &SimulationParams,
    fraction: f64,
) -> Option<SimHourResult> {
    if !fraction.is_finite() || fraction <= 0.0 || fraction > 1.0 {
        return None;
    }
    let segment_params = SimulationParams {
        start_soc_pct: params.start_soc_pct,
        max_charge_kw: params.max_charge_kw * fraction,
        max_discharge_kw: params.max_discharge_kw * fraction,
        ..*params
    };
    simulate_battery(&[hour], &segment_params)
        .hours
        .into_iter()
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(capacity: f64, start: f64, reserve: f64) -> SimulationParams {
        SimulationParams {
            capacity_kwh: capacity,
            start_soc_pct: start,
            reserve_soc_pct: reserve,
            max_charge_kw: 5.0,
            max_discharge_kw: 5.0,
            charge_efficiency: 0.9,
            discharge_efficiency: 0.95,
        }
    }

    fn hour(ts: i64, solar: f64, cons: f64) -> SimHourInput {
        SimHourInput {
            timestamp: ts,
            solar_kwh: solar,
            consumption_kwh: cons,
            ..Default::default()
        }
    }

    #[test]
    fn a_segment_scales_every_energy_and_keeps_the_held_share() {
        let full = SimHourInput {
            timestamp: 3600,
            solar_kwh: 2.0,
            consumption_kwh: 1.0,
            ev_kwh: 3.5,
            held_fraction: 0.5,
        };
        assert_eq!(
            full.segment(0.25),
            SimHourInput {
                timestamp: 3600,
                solar_kwh: 0.5,
                consumption_kwh: 0.25,
                ev_kwh: 0.875,
                held_fraction: 0.5,
            }
        );
        assert_eq!(full.segment(1.0), full);
    }

    fn with_car(ts: i64, solar: f64, cons: f64, ev: f64, held: f64) -> SimHourInput {
        SimHourInput {
            ev_kwh: ev,
            held_fraction: held,
            ..hour(ts, solar, cons)
        }
    }

    // ---- planned car charging (issue #406) ----

    #[test]
    fn car_charging_adds_to_the_load_the_battery_covers() {
        let out = simulate_battery(
            &[with_car(0, 0.0, 1.0, 2.0, 0.0)],
            &params(10.0, 80.0, 10.0),
        );
        let h = out.hours[0];
        assert!((h.discharge_kwh - 3.0).abs() < 1e-9, "{h:?}");
        assert_eq!(h.import_kwh, 0.0);
        assert!((h.soc_pct - (80.0 - 3.0 / 0.95 / 10.0 * 100.0)).abs() < 1e-9);
    }

    #[test]
    fn a_held_hour_leaves_the_battery_alone_and_imports_the_whole_deficit() {
        let out = simulate_battery(
            &[with_car(0, 0.0, 1.0, 3.0, 1.0)],
            &params(10.0, 80.0, 10.0),
        );
        let h = out.hours[0];
        assert_eq!(h.discharge_kwh, 0.0);
        assert!((h.import_kwh - 4.0).abs() < 1e-9, "{h:?}");
        assert!((h.soc_pct - 80.0).abs() < 1e-9);
    }

    #[test]
    fn a_half_held_hour_discharges_for_half_the_deficit() {
        let out = simulate_battery(
            &[with_car(0, 0.0, 2.0, 0.0, 0.5)],
            &params(10.0, 80.0, 10.0),
        );
        let h = out.hours[0];
        assert!((h.discharge_kwh - 1.0).abs() < 1e-9, "{h:?}");
        assert!((h.import_kwh - 1.0).abs() < 1e-9, "{h:?}");
    }

    #[test]
    fn a_held_hour_still_charges_from_the_solar_the_car_leaves() {
        // The hold pauses discharge only: 4 kWh of solar less 2 kWh of house
        // and car leaves 2 kWh to charge.
        let out = simulate_battery(
            &[with_car(0, 4.0, 0.5, 1.5, 1.0)],
            &params(10.0, 50.0, 10.0),
        );
        let h = out.hours[0];
        assert!((h.charge_kwh - 2.0).abs() < 1e-9, "{h:?}");
        assert_eq!(h.import_kwh, 0.0);
        assert_eq!(h.export_kwh, 0.0);
    }

    #[test]
    fn nonsense_car_figures_count_as_no_car_and_no_hold() {
        let p = params(10.0, 80.0, 10.0);
        let plain = simulate_battery(&[hour(0, 0.0, 1.0)], &p).hours[0];
        for (ev, held) in [
            (f64::NAN, 0.0),
            (-2.0, 0.0),
            (f64::INFINITY, 0.0),
            (0.0, f64::NAN),
            (0.0, -1.0),
        ] {
            let got = simulate_battery(&[with_car(0, 0.0, 1.0, ev, held)], &p).hours[0];
            assert_eq!(got, plain, "ev {ev} held {held}");
        }
        let over = simulate_battery(&[with_car(0, 0.0, 1.0, 0.0, 1.5)], &p).hours[0];
        let full = simulate_battery(&[with_car(0, 0.0, 1.0, 0.0, 1.0)], &p).hours[0];
        assert_eq!(over, full, "a share above 1 is the whole hour");
    }

    #[test]
    fn a_segment_of_a_held_hour_is_held_too() {
        let held = with_car(0, 0.0, 2.0, 2.0, 1.0);
        let res = simulate_battery_segment(held.segment(0.5), &params(10.0, 80.0, 10.0), 0.5)
            .expect("simulates");
        assert_eq!(res.discharge_kwh, 0.0);
        assert!((res.import_kwh - 2.0).abs() < 1e-9, "{res:?}");
    }

    #[test]
    fn surplus_charges_with_efficiency_and_caps_at_full() {
        // 10 kWh battery at 50%: 5 kWh room, surplus 3 kWh → all 3 kWh AC
        // drawn, 2.7 kWh stored → SOC 77%.
        let out = simulate_battery(&[hour(0, 3.0, 0.0)], &params(10.0, 50.0, 10.0));
        assert_eq!(out.hours.len(), 1);
        assert!((out.hours[0].charge_kwh - 3.0).abs() < 1e-9);
        assert!((out.hours[0].soc_pct - 77.0).abs() < 1e-9);
        assert!((out.hours[0].export_kwh - 0.0).abs() < 1e-9);
        assert!((out.hours[0].import_kwh - 0.0).abs() < 1e-9);
    }

    #[test]
    fn surplus_beyond_room_and_rate_exports_the_rest() {
        // 10 kWh at 95%: room 0.5 kWh → AC charge 0.5/0.9 ≈ 0.5556; solar
        // surplus 4 kWh → export ≈ 3.444, SOC pinned at 100.
        let out = simulate_battery(&[hour(0, 4.0, 0.0)], &params(10.0, 95.0, 10.0));
        let h = &out.hours[0];
        assert!((h.soc_pct - 100.0).abs() < 1e-9);
        assert!((h.charge_kwh - 0.5 / 0.9).abs() < 1e-9);
        assert!((h.export_kwh - (4.0 - 0.5 / 0.9)).abs() < 1e-9);
    }

    #[test]
    fn charge_rate_caps_throughput() {
        // Rate capped at 1 kW: surplus 5 kWh → charge 1 kWh AC, 4 kWh export.
        let mut p = params(10.0, 10.0, 10.0);
        p.max_charge_kw = 1.0;
        let out = simulate_battery(&[hour(0, 5.0, 0.0)], &p);
        let h = &out.hours[0];
        assert!((h.charge_kwh - 1.0).abs() < 1e-9);
        assert!((h.export_kwh - 4.0).abs() < 1e-9);
    }

    #[test]
    fn deficit_discharges_down_to_reserve() {
        // 10 kWh at 50%, reserve 10% → 4 kWh usable stored → 3.8 kWh AC
        // deliverable. Need 2 kWh → fully covered, SOC 50 - 2/0.95/10*100
        // = 28.947%.
        let out = simulate_battery(&[hour(0, 0.0, 2.0)], &params(10.0, 50.0, 10.0));
        let h = &out.hours[0];
        assert!((h.discharge_kwh - 2.0).abs() < 1e-9);
        assert!((h.soc_pct - (50.0 - 2.0 / 0.95 / 10.0 * 100.0)).abs() < 1e-9);
        assert!((h.import_kwh - 0.0).abs() < 1e-9);
    }

    #[test]
    fn deficit_beyond_reserve_imports_the_rest() {
        // At reserve: no discharge allowed, all need imports.
        let out = simulate_battery(&[hour(0, 0.0, 3.0)], &params(10.0, 10.0, 10.0));
        let h = &out.hours[0];
        assert!((h.discharge_kwh - 0.0).abs() < 1e-9);
        assert!((h.import_kwh - 3.0).abs() < 1e-9);
        assert!((h.soc_pct - 10.0).abs() < 1e-9);
    }

    #[test]
    fn partially_dischargeable_deficit_splits_discharge_and_import() {
        // 10 kWh at 50%, reserve 10%: 4 kWh stored → 3.8 kWh AC. Need
        // 5 kWh, discharge rate 5 kW → discharge 3.8, import 1.2.
        let out = simulate_battery(&[hour(0, 0.0, 5.0)], &params(10.0, 50.0, 10.0));
        let h = &out.hours[0];
        assert!((h.discharge_kwh - 3.8).abs() < 1e-9);
        assert!((h.import_kwh - 1.2).abs() < 1e-9);
        assert!((h.soc_pct - 10.0).abs() < 1e-9);
    }

    #[test]
    fn discharge_rate_caps_throughput() {
        // Need 5 kWh but rate 1 kW → discharge 1, import 4.
        let mut p = params(10.0, 50.0, 10.0);
        p.max_discharge_kw = 1.0;
        let out = simulate_battery(&[hour(0, 0.0, 5.0)], &p);
        let h = &out.hours[0];
        assert!((h.discharge_kwh - 1.0).abs() < 1e-9);
        assert!((h.import_kwh - 4.0).abs() < 1e-9);
    }

    #[test]
    fn soc_carries_across_hours_and_totals_sum() {
        // Two surplus hours then one deficit hour across a 10 kWh pack.
        let out = simulate_battery(
            &[
                hour(0, 2.0, 0.0),
                hour(3600, 2.0, 0.0),
                hour(7200, 0.0, 1.0),
            ],
            &params(10.0, 20.0, 10.0),
        );
        assert_eq!(out.hours.len(), 3);
        // After 2h: 20% + 2×1.8 stored = 56%.
        assert!((out.hours[1].soc_pct - 56.0).abs() < 1e-9);
        // Third hour discharges 1 kWh AC → stored drops 1/0.95.
        assert!((out.hours[2].soc_pct - (56.0 - 1.0 / 0.95 / 10.0 * 100.0)).abs() < 1e-9);
        assert!((out.total_export_kwh - 0.0).abs() < 1e-9);
        assert!((out.total_import_kwh - 0.0).abs() < 1e-9);
    }

    #[test]
    fn invalid_capacity_or_soc_yields_empty_output() {
        let p = params(10.0, 50.0, 10.0);
        // Valid params simulate normally.
        assert_eq!(simulate_battery(&[hour(0, 1.0, 1.0)], &p).hours.len(), 1);

        let mut zero_cap = p;
        zero_cap.capacity_kwh = 0.0;
        assert!(simulate_battery(&[hour(0, 1.0, 1.0)], &zero_cap)
            .hours
            .is_empty());

        let mut over = p;
        over.start_soc_pct = 101.0;
        assert!(simulate_battery(&[hour(0, 1.0, 1.0)], &over)
            .hours
            .is_empty());
    }

    #[test]
    fn non_finite_capacity_or_efficiency_yields_empty_output() {
        let input = [hour(0, 1.0, 1.0)];

        let mut non_finite_capacity = params(10.0, 50.0, 10.0);
        non_finite_capacity.capacity_kwh = f64::NAN;
        assert!(simulate_battery(&input, &non_finite_capacity)
            .hours
            .is_empty());

        let mut non_finite_charge_efficiency = params(10.0, 50.0, 10.0);
        non_finite_charge_efficiency.charge_efficiency = f64::NAN;
        assert!(simulate_battery(&input, &non_finite_charge_efficiency)
            .hours
            .is_empty());

        let mut non_finite_discharge_efficiency = params(10.0, 50.0, 10.0);
        non_finite_discharge_efficiency.discharge_efficiency = f64::NAN;
        assert!(simulate_battery(&input, &non_finite_discharge_efficiency)
            .hours
            .is_empty());
    }
}
