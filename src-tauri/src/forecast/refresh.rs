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

/// The charge-slot target SOC the auto-refresh writes. Always 100 by
/// design (planner v2): the slot's DURATION is the control variable, and
/// the target stays at 100 so the inverter never stops early at an SOC
/// threshold — the rate-limit register (HR 111 / HR 313 / HR 1110) is
/// what actually governs the charge.
pub const SLOT_TARGET_SOC_NONE: u8 = 100;

/// The charge-rate percent the auto-refresh (and the Apply payload)
/// writes: the inverter charges at its maximum for the planned duration.
/// Half-scale for DC hybrids (raw HR 111 = 50), direct 1–100 for
/// AC-coupled / three-phase (HR 313 / HR 1110 = 100).
pub const PLAN_CHARGE_RATE_PERCENT: u16 = 100;

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

/// The poll loop's full gate: the refresh fires only when the plan is due
/// AND Adaptive Charge does not own the charge rate. Adaptive Charge and
/// the auto-refresh both write the charge-limit register; when Adaptive
/// owns it, the refresh must not clobber its writes (CODE_REVIEW.md
/// Major 3). Kept as a separate pure helper so the interaction is
/// testable — `plan_refresh_due` itself is deliberately unaware of
/// Adaptive Charge.
pub fn plan_refresh_due_with_adaptive(
    now: DateTime<Local>,
    last_refresh_date: Option<NaiveDate>,
    tariff: Option<&TariffConfig>,
    adaptive_owns_rate: bool,
) -> bool {
    !adaptive_owns_rate && plan_refresh_due(now, last_refresh_date, tariff)
}

/// Hard upper bound for the user-configured auto-apply lead time,
/// enforced by `POST /api/settings`. Two hours comfortably covers any
/// sane "warn me before the cheap window" preference without letting a
/// fat-fingered 9999 fire the trigger all day.
pub const PLAN_AUTO_APPLY_MAX_LEAD_MINUTES: u16 = 120;

/// Why the auto-apply gate stood down — or that it's due. Returned by
/// [`plan_auto_apply_due`] so the poll loop can log (and once per day
/// notify) the reason instead of silently skipping on every poll. This
/// is the `Due { reason } | NotDue { reason }` shape CODE_REVIEW.md's
/// open SUGGESTION asks for; `plan_refresh_due` can be retro-fitted to
/// it later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanApplyDecision {
    /// Fire the auto-apply this poll.
    Due { reason: &'static str },
    /// Stand down; `reason` says why.
    NotDue { reason: &'static str },
}

/// The auto-apply trigger gate: due when the tariff's cheapest charging
/// window starts within `lead_minutes` of now and the trigger has not
/// fired yet today. Pure tariff arithmetic — the expensive plan
/// computation runs only on `Due`. The lead time is the user-configured
/// value (0–120 min); 0 fires at the window's own start, which is safe
/// because the slot write lands via the shared write pump and the
/// inverter charges the remainder of the slot either way.
pub fn plan_auto_apply_due(
    now: DateTime<Local>,
    last_apply_date: Option<NaiveDate>,
    tariff: Option<&TariffConfig>,
    lead_minutes: u16,
) -> PlanApplyDecision {
    if last_apply_date == Some(now.date_naive()) {
        return PlanApplyDecision::NotDue {
            reason: "already applied today",
        };
    }
    let Some(tariff) = tariff else {
        return PlanApplyDecision::NotDue {
            reason: "no tariff configured",
        };
    };
    let now_min = now.hour() as u16 * 60 + now.minute() as u16;
    let Some(window) = crate::forecast::planner::cheapest_import_window(tariff, now_min, 30) else {
        return PlanApplyDecision::NotDue {
            reason: "no upcoming cheap window",
        };
    };
    // Minutes until the selected occurrence starts, wrapping at midnight.
    let until_start = (window.start_min + 1440 - now_min) % 1440;
    if until_start <= lead_minutes {
        PlanApplyDecision::Due {
            reason: "inside the lead window",
        }
    } else {
        PlanApplyDecision::NotDue {
            reason: "outside the lead window",
        }
    }
}

/// The poll loop's full auto-apply gate: the same composition as
/// [`plan_refresh_due_with_adaptive`] — Adaptive Charge owning the
/// charge-limit register suppresses the apply (both write the charge
/// rate), with a distinct reason so the poll loop can warn/notify about
/// it once per day.
pub fn plan_auto_apply_decision_with_adaptive(
    now: DateTime<Local>,
    last_apply_date: Option<NaiveDate>,
    tariff: Option<&TariffConfig>,
    lead_minutes: u16,
    adaptive_owns_rate: bool,
) -> PlanApplyDecision {
    if adaptive_owns_rate {
        return PlanApplyDecision::NotDue {
            reason: "Adaptive Charge owns the charge rate",
        };
    }
    plan_auto_apply_due(now, last_apply_date, tariff, lead_minutes)
}

/// Whether the poll loop should emit the once-per-day "Adaptive Charge
/// owns the charge rate" warning for the auto-apply: only when the apply
/// would actually be due — not already applied today, a tariff configured,
/// and inside the lead window. Pure helper so the poll loop's warning gate
/// is testable, mirroring how the auto-refresh block gates its equivalent
/// warning on [`plan_refresh_due`]. Without the would-be-due check the
/// warning would fire daily even with no cheap window near (or no tariff
/// at all), training users to ignore the channel.
pub fn plan_auto_apply_adaptive_warning_due(
    now: DateTime<Local>,
    last_apply_date: Option<NaiveDate>,
    tariff: Option<&TariffConfig>,
    lead_minutes: u16,
) -> bool {
    matches!(
        plan_auto_apply_due(now, last_apply_date, tariff, lead_minutes),
        PlanApplyDecision::Due { .. }
    )
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
    fn adaptive_owns_rate_suppresses_the_refresh() {
        let flux = flux_tariff();
        let day = local_dt(2026, 8, 31, 1, 40); // 20 min before 02:00
                                                // Due without Adaptive: fires.
        assert!(plan_refresh_due_with_adaptive(
            day,
            None,
            Some(&flux),
            false
        ));
        // Same moment with Adaptive owning the rate: suppressed.
        assert!(!plan_refresh_due_with_adaptive(
            day,
            None,
            Some(&flux),
            true
        ));
        // Adaptive alone never makes an otherwise-undue refresh fire.
        let afternoon = local_dt(2026, 8, 31, 15, 0);
        assert!(!plan_refresh_due_with_adaptive(
            afternoon,
            None,
            Some(&flux),
            false
        ));
        assert!(!plan_refresh_due_with_adaptive(
            afternoon,
            None,
            Some(&flux),
            true
        ));
    }

    #[test]
    fn adaptive_warning_only_when_the_apply_would_actually_be_due() {
        let flux = flux_tariff();
        let day = local_dt(2026, 8, 31, 1, 40); // 20 min before 02:00
                                                // Inside the lead window and not applied yet: the warning fires...
        assert!(plan_auto_apply_adaptive_warning_due(
            day,
            None,
            Some(&flux),
            30
        ));
        // ...but not once today's apply has already happened — nothing is
        // being skipped, so a warning would be noise.
        assert!(!plan_auto_apply_adaptive_warning_due(
            day,
            Some(day.date_naive()),
            Some(&flux),
            30
        ));
        // Mid-afternoon with no cheap window near: no daily false alarm.
        let afternoon = local_dt(2026, 8, 31, 15, 0);
        assert!(!plan_auto_apply_adaptive_warning_due(
            afternoon,
            None,
            Some(&flux),
            30
        ));
        // No tariff configured: nothing to warn about either.
        assert!(!plan_auto_apply_adaptive_warning_due(day, None, None, 30));
    }

    #[test]
    fn adaptive_warning_respects_the_configured_lead_time() {
        let flux = flux_tariff();
        // 90 min before the 02:00 window: outside a 30-minute lead, inside
        // a 120-minute one — the gate must follow the user's setting.
        let early = local_dt(2026, 8, 31, 0, 30);
        assert!(!plan_auto_apply_adaptive_warning_due(
            early,
            None,
            Some(&flux),
            30
        ));
        assert!(plan_auto_apply_adaptive_warning_due(
            early,
            None,
            Some(&flux),
            120
        ));
    }

    #[test]
    fn auto_apply_due_only_inside_the_configured_lead_window() {
        let flux = flux_tariff();
        // 20 min before the 02:00 cheap window: due with the default lead.
        let day = local_dt(2026, 8, 31, 1, 40);
        assert!(matches!(
            plan_auto_apply_due(day, None, Some(&flux), 30),
            PlanApplyDecision::Due { .. }
        ));
        // 90 min before: outside a 30-minute lead...
        let early = local_dt(2026, 8, 31, 0, 30);
        assert!(matches!(
            plan_auto_apply_due(early, None, Some(&flux), 30),
            PlanApplyDecision::NotDue { .. }
        ));
        // ...but inside a 120-minute one.
        assert!(matches!(
            plan_auto_apply_due(early, None, Some(&flux), 120),
            PlanApplyDecision::Due { .. }
        ));
    }

    #[test]
    fn auto_apply_boundary_exactly_the_lead_before_fires() {
        // Exactly the lead interval before the start still fires — same
        // boundary contract as the nightly refresh.
        let flux = flux_tariff();
        let day = local_dt(2026, 8, 31, 1, 30);
        assert!(matches!(
            plan_auto_apply_due(day, None, Some(&flux), 30),
            PlanApplyDecision::Due { .. }
        ));
    }

    #[test]
    fn auto_apply_zero_lead_fires_at_the_window_start() {
        let flux = flux_tariff();
        let start = local_dt(2026, 8, 31, 2, 0);
        assert!(matches!(
            plan_auto_apply_due(start, None, Some(&flux), 0),
            PlanApplyDecision::Due { .. }
        ));
        // One minute before with a zero lead: not yet.
        let just_before = local_dt(2026, 8, 31, 1, 59);
        assert!(matches!(
            plan_auto_apply_due(just_before, None, Some(&flux), 0),
            PlanApplyDecision::NotDue { .. }
        ));
    }

    #[test]
    fn auto_apply_wraps_midnight_to_tomorrows_window() {
        // Cheap window 00:15–05:00: a 30-minute lead must fire at 23:45
        // the previous evening, not wrap past the window entirely.
        let t = tariff(&[("00:15", "05:00", 0.09), ("05:00", "23:59", 0.30)]);
        let evening = local_dt(2026, 8, 31, 23, 45);
        assert!(matches!(
            plan_auto_apply_due(evening, None, Some(&t), 30),
            PlanApplyDecision::Due { .. }
        ));
        // 31 minutes before: not yet.
        let earlier = local_dt(2026, 8, 31, 23, 14);
        assert!(matches!(
            plan_auto_apply_due(earlier, None, Some(&t), 30),
            PlanApplyDecision::NotDue { .. }
        ));
    }

    #[test]
    fn auto_apply_fires_once_per_day() {
        let flux = flux_tariff();
        let day = local_dt(2026, 8, 31, 1, 40);
        assert!(matches!(
            plan_auto_apply_due(day, None, Some(&flux), 30),
            PlanApplyDecision::Due { .. }
        ));
        // Same poll cycle shape after the latch: already applied today.
        assert_eq!(
            plan_auto_apply_due(day, Some(day.date_naive()), Some(&flux), 30),
            PlanApplyDecision::NotDue {
                reason: "already applied today"
            }
        );
    }

    #[test]
    fn auto_apply_needs_a_tariff() {
        let day = local_dt(2026, 8, 31, 1, 40);
        assert_eq!(
            plan_auto_apply_due(day, None, None, 30),
            PlanApplyDecision::NotDue {
                reason: "no tariff configured"
            }
        );
    }

    #[test]
    fn auto_apply_decision_suppressed_when_adaptive_owns_the_rate() {
        let flux = flux_tariff();
        let day = local_dt(2026, 8, 31, 1, 40); // would be due
        assert_eq!(
            plan_auto_apply_decision_with_adaptive(day, None, Some(&flux), 30, true),
            PlanApplyDecision::NotDue {
                reason: "Adaptive Charge owns the charge rate"
            }
        );
        // Without Adaptive, the same moment is due.
        assert!(matches!(
            plan_auto_apply_decision_with_adaptive(day, None, Some(&flux), 30, false),
            PlanApplyDecision::Due { .. }
        ));
        // Adaptive suppression short-circuits before the due arithmetic, so
        // the poll loop must consult `plan_auto_apply_adaptive_warning_due`
        // before warning — an out-of-lead-window moment reports the adaptive
        // reason while the warning gate correctly says not to warn.
        let afternoon = local_dt(2026, 8, 31, 15, 0);
        assert_eq!(
            plan_auto_apply_decision_with_adaptive(afternoon, None, Some(&flux), 30, true),
            PlanApplyDecision::NotDue {
                reason: "Adaptive Charge owns the charge rate"
            }
        );
        assert!(!plan_auto_apply_adaptive_warning_due(
            afternoon,
            None,
            Some(&flux),
            30
        ));
    }

    #[test]
    fn plan_refresh_due_itself_is_unaware_of_adaptive() {
        // The Adaptive gate lives in `plan_refresh_due_with_adaptive`, not
        // in `plan_refresh_due` — the poll loop composes them. This pins
        // that separation so a future refactor can't silently move the
        // gate into the helper and change the poll-loop behaviour.
        let flux = flux_tariff();
        let day = local_dt(2026, 8, 31, 1, 40);
        assert!(plan_refresh_due(day, None, Some(&flux)));
    }

    #[test]
    fn refresh_due_with_no_prior_refresh_fires_in_the_lead_window() {
        // `last = None` (fresh start inside the lead window) must fire —
        // the idempotent-rewrite case after a restart.
        let flux = flux_tariff();
        let day = local_dt(2026, 8, 31, 1, 40);
        assert!(plan_refresh_due(day, None, Some(&flux)));
        assert!(plan_refresh_due_with_adaptive(
            day,
            None,
            Some(&flux),
            false
        ));
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
