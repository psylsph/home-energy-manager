//! Nightly auto-refresh of the Forecast plan's charge slot.
//!
//! The plan is deliberately sized for ONE charge cycle (see
//! [`crate::forecast::planner`]): it is computed from the live SOC and the
//! current forecast, and the "assuming charge" trajectory stops at the next
//! cheap period because that cycle is the NEXT plan's business. The
//! inverter, however, treats an applied charge slot as a nightly recurring
//! schedule — left alone it would repeat yesterday's duration forever,
//! drifting away from what the battery actually needs.
//!
//! When `forecast_plan_auto_refresh` is enabled, this module lets the poll
//! loop close that gap: shortly before each cheap period it re-computes the
//! plan from the freshest live SOC and forecast, then either rewrites
//! charge slot 1 with the newly sized window or — when the fresh plan needs
//! no charge — clears the slot and disarms Timed Charge so the inverter
//! returns to Eco. Enabling the setting hands charge slot 1 to the planner;
//! hand-configured slots on the Control page are never touched while it is
//! off.

use crate::forecast::planner::{ChargeWindow, PlanRecommendation};
use crate::settings::TariffConfig;
use chrono::{DateTime, Local, NaiveDate, Timelike};

/// How long before the cheap period's start the refresh fires. The slot
/// must be (re)written before the window begins; half an hour keeps the
/// SOC the plan reads fresh while leaving a comfortable margin for the
/// handful of register writes.
pub const PLAN_REFRESH_LEAD_MINUTES: u16 = 30;

/// What the refresh decided to do for this cheap period.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanRefreshAction {
    /// Nothing to do (disabled, already refreshed today, or no plan).
    None,
    /// Rewrite charge slot 1 with the plan's window and arm Timed Charge.
    /// `start_hhmm`/`end_hhmm` are the encoded HHMM register values.
    WriteSlot { start_hhmm: u16, end_hhmm: u16 },
    /// The fresh plan needs no charge: clear slot 1 and return to Eco so
    /// the previously applied slot cannot keep charging nightly.
    ClearSlot,
}

/// Cheap gate run every poll: true when the auto-refresh is enabled, has
/// not fired yet today, and the tariff's cheapest window starts within the
/// lead window. Only tariff arithmetic — the expensive plan computation
/// runs after this returns true.
pub fn plan_refresh_due(
    now: DateTime<Local>,
    last_refresh_date: Option<NaiveDate>,
    tariff: Option<&TariffConfig>,
) -> bool {
    if last_refresh_date == Some(now.date_naive()) {
        return false;
    }
    let Some(tariff) = tariff else {
        return false;
    };
    let now_min = now.hour() as u16 * 60 + now.minute() as u16;
    let Some(window) = crate::forecast::planner::cheapest_import_window(tariff, now_min, 30) else {
        return false;
    };
    // Minutes until the selected occurrence starts, wrapping at midnight.
    let until_start = (window.start_min + 1440 - now_min) % 1440;
    until_start <= PLAN_REFRESH_LEAD_MINUTES
}

/// Map a freshly computed plan to the refresh action. The window's
/// wall-clock bounds use the same 23:59 clamp as the Apply payload so the
/// inverter's slot register stays enabled for end-of-day windows.
pub fn plan_refresh_action(rec: &PlanRecommendation) -> PlanRefreshAction {
    match rec {
        PlanRecommendation::Charge { window, .. } => {
            let (start_h, start_m, end_h, end_m) = plan_slot_hhmm(window);
            PlanRefreshAction::WriteSlot {
                start_hhmm: encode_hhmm(start_h, start_m),
                end_hhmm: encode_hhmm(end_h, end_m),
            }
        }
        PlanRecommendation::NoChargeNeeded { .. } => PlanRefreshAction::ClearSlot,
        PlanRecommendation::NoPlan { .. } => PlanRefreshAction::None,
    }
}

/// The (start hour, start minute, end hour, end minute) to write to an
/// inverter charge slot for this window. Ends at or past 23:59 — and any
/// midnight-wrapped end (00:00) on a window that starts later in the day —
/// are clamped to 23:59 so the slot register's disabled encoding
/// (00:00–00:00), and the ambiguous midnight end, can never be produced.
pub fn plan_slot_hhmm(window: &ChargeWindow) -> (u16, u16, u16, u16) {
    let end_min = if window.end_min >= 23 * 60 + 59 || (window.end_min == 0 && window.start_min > 0)
    {
        23 * 60 + 59
    } else {
        window.end_min
    };
    (
        window.start_min / 60,
        window.start_min % 60,
        end_min / 60,
        end_min % 60,
    )
}

/// Encode hour/minute into the inverter's HHMM register value.
fn encode_hhmm(hour: u16, minute: u16) -> u16 {
    hour * 100 + minute
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forecast::planner::{plan_overnight_charge, PlanInputs};
    use crate::forecast::simulate::{SimHourInput, SimulationParams};
    use crate::settings::TariffSlot;
    use chrono::TimeZone;

    fn tariff(slots: &[(&str, &str, f64)]) -> TariffConfig {
        TariffConfig {
            slots: slots
                .iter()
                .map(|(s, e, r)| TariffSlot {
                    start: s.to_string(),
                    end: e.to_string(),
                    rate: *r,
                })
                .collect(),
        }
    }

    fn flux_tariff() -> TariffConfig {
        tariff(&[
            ("00:00", "02:00", 0.26),
            ("02:00", "05:00", 0.09),
            ("05:00", "16:00", 0.26),
            ("16:00", "21:00", 0.35),
            ("21:00", "23:59", 0.26),
        ])
    }

    fn local_dt(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local
            .from_local_datetime(
                &chrono::NaiveDate::from_ymd_opt(y, m, d)
                    .unwrap()
                    .and_hms_opt(h, min, 0)
                    .unwrap(),
            )
            .earliest()
            .unwrap()
    }

    #[test]
    fn refresh_due_only_inside_the_lead_window() {
        let flux = flux_tariff();
        let day = local_dt(2026, 8, 31, 1, 40); // 20 min before 02:00
        assert!(plan_refresh_due(day, None, Some(&flux)));
        // Boundary: exactly the lead interval before the start still fires.
        let day = local_dt(2026, 8, 31, 1, 30);
        assert!(plan_refresh_due(day, None, Some(&flux)));
        // One minute past the boundary: no.
        let day = local_dt(2026, 8, 31, 1, 29);
        assert!(!plan_refresh_due(day, None, Some(&flux)));
        // Inside the window itself (until_start == 0) still counts: the
        // inverter charges the remainder of the slot.
        let day = local_dt(2026, 8, 31, 2, 0);
        assert!(plan_refresh_due(day, None, Some(&flux)));
        // Mid-afternoon, hours away from the window: no.
        let day = local_dt(2026, 8, 31, 15, 0);
        assert!(!plan_refresh_due(day, None, Some(&flux)));
    }

    #[test]
    fn refresh_fires_once_per_day() {
        let flux = flux_tariff();
        let first = local_dt(2026, 8, 31, 1, 40);
        assert!(plan_refresh_due(first, None, Some(&flux)));
        // Same day, still inside the lead window: already done.
        let again = local_dt(2026, 8, 31, 1, 55);
        assert!(!plan_refresh_due(
            again,
            Some(first.date_naive()),
            Some(&flux)
        ));
        // Next day inside the lead window: due again.
        let next_day = local_dt(2026, 9, 1, 1, 40);
        assert!(plan_refresh_due(
            next_day,
            Some(first.date_naive()),
            Some(&flux)
        ));
    }

    #[test]
    fn refresh_needs_a_tariff() {
        let day = local_dt(2026, 8, 31, 1, 40);
        assert!(!plan_refresh_due(day, None, None));
    }

    #[test]
    fn wrapping_window_due_right_before_its_start() {
        // Cheap period 23:30–05:30 (spans midnight): due at 23:10.
        let wrap = tariff(&[
            ("00:00", "05:30", 0.07),
            ("05:30", "23:30", 0.30),
            ("23:30", "23:59", 0.07),
        ]);
        let day = local_dt(2026, 8, 31, 23, 10);
        assert!(plan_refresh_due(day, None, Some(&wrap)));
        // ...but not at 22:00, 90 minutes early.
        let day = local_dt(2026, 8, 31, 22, 0);
        assert!(!plan_refresh_due(day, None, Some(&wrap)));
    }

    #[test]
    fn charge_plan_maps_to_a_slot_write() {
        let window = ChargeWindow {
            start_min: 2 * 60,
            end_min: 4 * 60 + 45,
            tomorrow: true,
            rate: 0.09,
        };
        let rec = PlanRecommendation::Charge {
            window,
            kwh: 8.25,
            min_soc_pct: 20.0,
            observed_min_soc_pct: 10.0,
            after_min_soc_pct: 20.3,
            charge_target_soc_pct: 100.0,
            current_soc_pct: 20.0,
            rationale: String::new(),
            with_charge_series: Vec::new(),
            import_tomorrow_with_charge_kwh: 0.0,
            export_tomorrow_with_charge_kwh: 0.0,
        };
        assert_eq!(
            plan_refresh_action(&rec),
            PlanRefreshAction::WriteSlot {
                start_hhmm: 200,
                end_hhmm: 445,
            }
        );
    }

    #[test]
    fn end_of_day_window_clamps_to_2359() {
        // A window ending at 23:59+ must not encode 23:60+ or 00:00 (the
        // disabled slot encoding) — clamp to the last minute of the day.
        let window = ChargeWindow {
            start_min: 22 * 60,
            end_min: 23 * 60 + 59,
            tomorrow: false,
            rate: 0.09,
        };
        let rec = PlanRecommendation::Charge {
            window,
            kwh: 1.0,
            min_soc_pct: 20.0,
            observed_min_soc_pct: 10.0,
            after_min_soc_pct: 21.0,
            charge_target_soc_pct: 100.0,
            current_soc_pct: 20.0,
            rationale: String::new(),
            with_charge_series: Vec::new(),
            import_tomorrow_with_charge_kwh: 0.0,
            export_tomorrow_with_charge_kwh: 0.0,
        };
        assert_eq!(
            plan_refresh_action(&rec),
            PlanRefreshAction::WriteSlot {
                start_hhmm: 2200,
                end_hhmm: 2359,
            }
        );
    }

    #[test]
    fn midnight_end_window_clamps_to_2359() {
        // A wrap window shortened so its end lands exactly on midnight
        // (23:30 + 30 min) must not write end 00:00 — with a non-zero
        // start that register pair is ambiguous on the inverter and at
        // worst decodes as a disabled slot. Clamp to 23:59 so the slot
        // stays enabled for the minutes that matter.
        let window = ChargeWindow {
            start_min: 23 * 60 + 30,
            end_min: 0,
            tomorrow: false,
            rate: 0.07,
        };
        let rec = PlanRecommendation::Charge {
            window,
            kwh: 1.25,
            min_soc_pct: 20.0,
            observed_min_soc_pct: 10.0,
            after_min_soc_pct: 20.5,
            charge_target_soc_pct: 100.0,
            current_soc_pct: 15.0,
            rationale: String::new(),
            with_charge_series: Vec::new(),
            import_tomorrow_with_charge_kwh: 0.0,
            export_tomorrow_with_charge_kwh: 0.0,
        };
        assert_eq!(
            plan_refresh_action(&rec),
            PlanRefreshAction::WriteSlot {
                start_hhmm: 2330,
                end_hhmm: 2359,
            }
        );
    }

    #[test]
    fn no_charge_plan_clears_the_slot() {
        let rec = PlanRecommendation::NoChargeNeeded {
            current_soc_pct: 80.0,
            min_soc_pct: 20.0,
            observed_min_soc_pct: 45.0,
        };
        assert_eq!(plan_refresh_action(&rec), PlanRefreshAction::ClearSlot);
        let rec = PlanRecommendation::NoPlan {
            reason: "no tariff".to_string(),
        };
        assert_eq!(plan_refresh_action(&rec), PlanRefreshAction::None);
    }

    // --- changed SOC ⇒ changed slot duration ------------------------------

    fn params() -> SimulationParams {
        SimulationParams {
            capacity_kwh: 10.0,
            start_soc_pct: 30.0,
            reserve_soc_pct: 10.0,
            max_charge_kw: 2.5,
            max_discharge_kw: 2.5,
            charge_efficiency: 0.9,
            discharge_efficiency: 0.95,
        }
    }

    /// 48-hour series starting at midnight on the day AFTER the planning
    /// moment — the forward-only shape the live refresh sees when it fires
    /// in the lead window. `cons_kwh` is flat across all hours and there
    /// is no solar, so the battery's level at the window start is the
    /// `start_soc` minus barely an hour of drain: the SOC the refresh
    /// reads is what the plan sizes against. Returns the params with the
    /// SOC applied — the same params the what-if re-simulations must use.
    fn forward_48h(
        start_soc: f64,
        cons_kwh: f64,
        p: &SimulationParams,
    ) -> (
        crate::forecast::simulate::SimulationOutput,
        Vec<SimHourInput>,
        SimulationParams,
    ) {
        let base = chrono::NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
        let mut hours: Vec<SimHourInput> = Vec::new();
        for d in 0..2i64 {
            for h in 0..24u32 {
                let day = base + chrono::Duration::days(d);
                let ts = Local
                    .from_local_datetime(&day.and_hms_opt(h, 0, 0).unwrap())
                    .earliest()
                    .unwrap()
                    .timestamp();
                hours.push(SimHourInput {
                    timestamp: ts,
                    solar_kwh: 0.0,
                    consumption_kwh: cons_kwh,
                });
            }
        }
        let mut p2 = *p;
        p2.start_soc_pct = start_soc;
        let sim = crate::forecast::simulate::simulate_battery(&hours, &p2);
        (sim, hours, p2)
    }

    /// Plan at 22:00 on 2026-08-31 for a forward series starting 09-01,
    /// from the given live SOC.
    fn plan_at(start_soc: f64, cons_kwh: f64) -> PlanRecommendation {
        let p = params();
        let (sim, sim_hours, p) = forward_48h(start_soc, cons_kwh, &p);
        let inputs = PlanInputs {
            simulation: &sim,
            sim_hours: Some(&sim_hours),
            params: &p,
            import_tariff: Some(&flux_tariff()),
            target_soc_pct: 20.0,
            consumption_tomorrow_kwh: 12.0,
            consumption_sufficient: true,
            now_ts: local_dt(2026, 8, 31, 22, 0).timestamp(),
            current_soc_pct: start_soc,
        };
        plan_overnight_charge(&inputs)
    }

    fn slot_duration(action: &PlanRefreshAction) -> u16 {
        let PlanRefreshAction::WriteSlot {
            start_hhmm,
            end_hhmm,
        } = action
        else {
            panic!("expected WriteSlot, got {action:?}");
        };
        let start = (start_hhmm / 100) * 60 + (start_hhmm % 100);
        let end = (end_hhmm / 100) * 60 + (end_hhmm % 100);
        end - start
    }

    /// The review's scheduling coverage: the auto-refresh re-decides the
    /// slot from the live SOC, so a changed next-day SOC must produce a
    /// changed charge duration — a lower battery needs a longer slot, and
    /// the second night's rewrite replaces the first night's duration
    /// instead of repeating it.
    #[test]
    fn changed_soc_changes_the_refreshed_slot_duration() {
        let low = plan_refresh_action(&plan_at(12.0, 0.12));
        let high = plan_refresh_action(&plan_at(46.0, 0.12));
        let low_min = slot_duration(&low);
        let high_min = slot_duration(&high);
        assert!(
            low_min > high_min,
            "lower SOC must size a longer slot: {low_min} vs {high_min}"
        );
        // The write pair differs, so the second night's rewrite genuinely
        // replaces the first night's slot rather than re-writing it.
        assert_ne!(low, high);
    }

    /// A healthy battery needs no charge at all: the refresh must clear
    /// the slot instead of leaving the previous night's schedule running.
    /// The same light drain at a low SOC still produces a slot, so the
    /// contrast comes from the SOC, not the load shape.
    #[test]
    fn healthy_soc_clears_the_slot() {
        assert_eq!(
            plan_refresh_action(&plan_at(80.0, 0.05)),
            PlanRefreshAction::ClearSlot
        );
        assert!(matches!(
            plan_refresh_action(&plan_at(15.0, 0.05)),
            PlanRefreshAction::WriteSlot { .. }
        ));
    }
}
