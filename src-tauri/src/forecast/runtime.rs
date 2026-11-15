//! How long the battery will last (issue #472): the time the projection
//! reaches the inverter's battery reserve, for the house view's Battery
//! Level card.
//!
//! It runs the Planner's own simulation (the same consumption profile,
//! solar forecast, dispatch overlay and current schedule, taken from the
//! forecast payload), from the live SOC at the current minute, three times:
//! the median day, a pessimistic one (p75 consumption, the low solar band)
//! and an optimistic one (p25 consumption, the high band). The median gives
//! the time; the spread between the other two gives the confidence.
//!
//! Kept trade-off: the current hour is pro-rated by its remaining share of
//! energy, but a schedule window that starts or ends inside it is still
//! judged over the whole hour, as the hourly simulation does elsewhere.

use chrono::{DateTime, Datelike, Local, Timelike};

use crate::forecast::current_schedule::{simulate_current_schedule, CurrentSchedule};
use crate::forecast::dispatch_load::{apply_dispatch_hours, DispatchHour};
use crate::forecast::simulate::{simulate_battery, SimHourInput, SimulationParams};
use crate::forecast::time_windows::window_overlap_segments;
use crate::forecast::{ForecastConsumptionHour, ForecastPayload};

const HOUR_SECS: i64 = 3600;
/// How far ahead to look: "how long will it last" is about today and tonight.
pub const RUNTIME_HORIZON_HOURS: i64 = 24;
/// The answer is rounded to this, as Brian asked: a minute-exact time would
/// claim more than an hourly profile can say.
pub const ROUND_SECS: i64 = 30 * 60;
/// SOC within this of the reserve counts as there (the simulation stops at
/// the reserve exactly; float noise aside).
const AT_RESERVE_PCT: f64 = 0.5;
/// A rise of more than this in an hour means the battery is being refilled.
const RISE_PCT: f64 = 0.5;
/// Battery power below this (W, negative = charging) means charging now.
const CHARGING_W: f64 = -100.0;

/// Whether the battery is charging now, by its power (W, negative =
/// charging). The endpoint's cache keys on the same test.
pub fn is_charging(battery_power_w: f64) -> bool {
    battery_power_w.is_finite() && battery_power_w < CHARGING_W
}
/// Under this much history the profile's spread can't be trusted.
const CONFIDENT_PROFILE_DAYS: u32 = 14;
/// Spreads for the confidence tiers.
const HIGH_SPREAD_SECS: i64 = HOUR_SECS;
const MEDIUM_SPREAD_SECS: i64 = 3 * HOUR_SECS;

/// One forward hour's inputs, for each of the three days.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RuntimeHour {
    /// Start of the hour, unix seconds.
    pub timestamp: i64,
    /// Solar: low band, central, high band (kWh). Zero where unknown.
    pub solar: [f64; 3],
    /// House load: p75, median, p25 (kWh), pessimistic first.
    pub consumption: [f64; 3],
}

/// Everything [`battery_runtime`] needs, so it's a pure function.
#[derive(Debug, Clone)]
pub struct RuntimeInputs {
    pub now_ts: i64,
    /// Hourly, ascending, the first being the hour `now_ts` is in.
    pub hours: Vec<RuntimeHour>,
    /// Planned car charging and dispatch holds, by hour start.
    pub dispatch_hours: Vec<DispatchHour>,
    pub dispatch_net_of_profile: bool,
    /// Capacity, the live SOC as the start, the reserve, rates, efficiencies.
    pub params: SimulationParams,
    pub schedule: CurrentSchedule,
    /// Battery power now, W (negative = charging).
    pub battery_power_w: f64,
    /// Solar power now, W. Lifts the current hour's median and optimistic
    /// solar when the sun is beating the forecast (issue #476).
    pub live_solar_w: f64,
    /// The inverter's own discharge pause (HR 318 mode 2 or 3) covers now.
    pub discharge_paused: bool,
    pub profile_days: u32,
    /// Whether the solar figures come from a forecast (false: taken as 0).
    pub solar_known: bool,
}

/// What the Battery Level card says.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum RuntimeOutcome {
    /// Reaches the reserve at `at` (rounded), likely between `earliest`
    /// and `latest` (either `None` when that day doesn't empty in the
    /// horizon).
    EmptyAt {
        at: i64,
        earliest: Option<i64>,
        latest: Option<i64>,
        confidence: Confidence,
    },
    /// Refilled before it empties: by a charge slot or by solar, from `at`.
    RechargesFirst { at: i64, by: RechargeSource },
    /// Neither empties nor refills within the horizon.
    LastsBeyond { hours: i64 },
    /// Already at the reserve.
    AtReserve,
    /// Discharge is paused (HEM's reserve pauses write 100; the inverter's
    /// own pause, HR 318): it won't drain until that ends, and when that is
    /// isn't known.
    Paused,
    /// Charging now, reaching `soc` (the running charge slot's target, or
    /// 100%) at `at`, rounded.
    FullAt { at: i64, soc: u8 },
    /// Charging now, but the projection doesn't carry it to its target (a
    /// Force Charge, say, which the forecast doesn't model): no time.
    Charging,
    /// The simulation refused its inputs (a corrupt SOC, no hours).
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RechargeSource {
    Charge,
    Solar,
}

/// What one simulated day does first.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Event {
    Empty(i64),
    Recharge(i64, RechargeSource),
    Nothing,
}

/// Round to the nearest [`ROUND_SECS`].
pub fn round_to_half_hour(ts: i64) -> i64 {
    (ts + ROUND_SECS / 2).div_euclid(ROUND_SECS) * ROUND_SECS
}

/// The hourly inputs for day `which` (0 pessimistic, 1 median, 2
/// optimistic), the current hour cut to what's left of it.
///
/// For the rest of the current hour the median and optimistic days take the
/// live solar where it beats the forecast (issue #476): a sunny morning on a
/// dull forecast otherwise can't see the charge it's making. The pessimistic
/// day keeps the forecast, since the sun may not last the hour.
fn sim_hours(inputs: &RuntimeInputs, which: usize) -> Vec<SimHourInput> {
    let live_kwh = (which > 0 && inputs.live_solar_w.is_finite() && inputs.live_solar_w > 0.0)
        .then(|| inputs.live_solar_w / 1000.0);
    let mut hours: Vec<SimHourInput> = inputs
        .hours
        .iter()
        .enumerate()
        .map(|(i, hour)| SimHourInput {
            timestamp: hour.timestamp,
            solar_kwh: match live_kwh {
                Some(live) if i == 0 => hour.solar[which].max(live),
                _ => hour.solar[which],
            },
            consumption_kwh: hour.consumption[which],
            ..Default::default()
        })
        .collect();
    apply_dispatch_hours(
        &mut hours,
        &inputs.dispatch_hours,
        inputs.dispatch_net_of_profile,
    );
    if let Some(first) = hours.first_mut() {
        let left = (first.timestamp + HOUR_SECS - inputs.now_ts).clamp(0, HOUR_SECS);
        *first = first.segment(left as f64 / HOUR_SECS as f64);
    }
    hours
}

/// When a charge slot starts refilling within the hour at `ts`, if one does.
fn charge_start_in_hour(schedule: &CurrentSchedule, ts: i64) -> Option<i64> {
    schedule
        .charge_windows
        .iter()
        .flat_map(|w| window_overlap_segments(ts, w.start_min, w.end_min, &Local))
        .map(|(start, _)| ts + (start * HOUR_SECS as f64).round() as i64)
        .min()
}

/// The SOC the battery is charging towards now: the running charge slot's
/// target, else full.
fn charge_target_now(schedule: &CurrentSchedule, now_ts: i64) -> f64 {
    let hour = now_ts - now_ts.rem_euclid(HOUR_SECS);
    let into = (now_ts - hour) as f64 / HOUR_SECS as f64;
    schedule
        .charge_windows
        .iter()
        .find(|w| {
            window_overlap_segments(hour, w.start_min, w.end_min, &Local)
                .iter()
                .any(|(start, end)| *start <= into && into < *end)
        })
        .map_or(100.0, |w| w.target_soc_pct)
}

fn soc_now(inputs: &RuntimeInputs) -> f64 {
    inputs.params.start_soc_pct
}

/// While charging: when the median day reaches `target`, or `None` if it
/// stops rising first (or the simulation refuses its inputs).
fn full_at(inputs: &RuntimeInputs, target: f64) -> Option<i64> {
    let hours = sim_hours(inputs, 1);
    // The simulation stops at 100%, which would put the crossing at the end
    // of the hour that fills it. A battery twice the size from half the SOC,
    // with every percentage halved, moves the same energy without a ceiling
    // before the target; so all SOCs below are halves.
    let half = |pct: f64| pct / 2.0;
    let params = SimulationParams {
        capacity_kwh: inputs.params.capacity_kwh * 2.0,
        start_soc_pct: half(inputs.params.start_soc_pct),
        reserve_soc_pct: half(inputs.params.reserve_soc_pct),
        ..inputs.params
    };
    let mut schedule = inputs.schedule.clone();
    for w in schedule
        .charge_windows
        .iter_mut()
        .chain(schedule.discharge_windows.iter_mut())
    {
        w.target_soc_pct = half(w.target_soc_pct);
    }
    let target = half(target);
    let series: Vec<(i64, f64)> = if schedule.is_empty() {
        simulate_battery(&hours, &params)
            .hours
            .iter()
            .map(|h| (h.timestamp, h.soc_pct))
            .collect()
    } else {
        simulate_current_schedule(&hours, &params, &schedule)
    };
    let (mut prev_t, mut prev_soc) = (inputs.now_ts, params.start_soc_pct);
    for (ts, soc) in series {
        let end = ts + HOUR_SECS;
        if soc >= target - AT_RESERVE_PCT / 2.0 {
            let rise = soc - prev_soc;
            let share = if rise > 0.0 {
                ((target - prev_soc) / rise).clamp(0.0, 1.0)
            } else {
                0.0
            };
            return Some(prev_t + (share * (end - prev_t) as f64).round() as i64);
        }
        if soc <= prev_soc {
            return None;
        }
        (prev_t, prev_soc) = (end, soc);
    }
    None
}

/// Day `which` simulated with no floor (see [`first_event`]), `(hour
/// start, SOC at its end)`; empty when the simulation refused its inputs.
fn unfloored_series(inputs: &RuntimeInputs, which: usize) -> Vec<(i64, f64)> {
    let hours = sim_hours(inputs, which);
    // Simulated with no floor, so the hour that reaches the reserve shows
    // how far past it the load would take the battery, and the crossing can
    // be placed inside the hour. Up to the crossing it's the same path.
    let unfloored = SimulationParams {
        reserve_soc_pct: 0.0,
        ..inputs.params
    };
    let series: Vec<(i64, f64)> = if inputs.schedule.is_empty() {
        simulate_battery(&hours, &unfloored)
            .hours
            .iter()
            .map(|h| (h.timestamp, h.soc_pct))
            .collect()
    } else {
        simulate_current_schedule(&hours, &unfloored, &inputs.schedule)
    };
    series
}

/// Simulate one day and find what happens first: the reserve, or a refill.
/// `None` when the simulation refused its inputs. With `topping_up`, the
/// rise the battery is in now is part of the day, not a refill: refills
/// count only once it has started to fall.
fn first_event(inputs: &RuntimeInputs, which: usize, topping_up: bool) -> Option<Event> {
    let series = unfloored_series(inputs, which);
    if series.is_empty() {
        return None;
    }
    let reserve = inputs.params.reserve_soc_pct;
    let (mut prev_t, mut prev_soc) = (inputs.now_ts, inputs.params.start_soc_pct);
    let mut rising_now = topping_up;
    for (ts, soc) in series {
        let end = ts + HOUR_SECS;
        if soc <= reserve + AT_RESERVE_PCT / 10.0 {
            let drop = prev_soc - soc;
            let share = if drop > 0.0 {
                ((prev_soc - reserve) / drop).clamp(0.0, 1.0)
            } else {
                0.0
            };
            return Some(Event::Empty(
                prev_t + (share * (end - prev_t) as f64).round() as i64,
            ));
        }
        if rising_now && soc >= prev_soc {
            (prev_t, prev_soc) = (end, soc);
            continue;
        }
        rising_now = false;
        if soc > prev_soc + RISE_PCT {
            // A #434 top-up during a held dispatch charges from the grid too.
            let top_up = inputs
                .dispatch_hours
                .iter()
                .any(|d| d.timestamp == ts && d.charge_to_soc.is_some() && d.held_fraction > 0.0);
            return Some(match charge_start_in_hour(&inputs.schedule, ts) {
                Some(at) => Event::Recharge(at.max(inputs.now_ts), RechargeSource::Charge),
                None if top_up => Event::Recharge(ts.max(inputs.now_ts), RechargeSource::Charge),
                None => Event::Recharge(ts.max(inputs.now_ts), RechargeSource::Solar),
            });
        }
        (prev_t, prev_soc) = (end, soc);
    }
    Some(Event::Nothing)
}

/// How long the battery lasts. See the module docs.
pub fn battery_runtime(inputs: &RuntimeInputs) -> RuntimeOutcome {
    if is_charging(inputs.battery_power_w) {
        // Already past a running slot's target (the sun, say): heading on
        // towards full.
        let target = Some(charge_target_now(&inputs.schedule, inputs.now_ts).clamp(0.0, 100.0))
            .filter(|target| *target > soc_now(inputs) + AT_RESERVE_PCT)
            .unwrap_or(100.0);
        if let Some(at) = full_at(inputs, target) {
            return RuntimeOutcome::FullAt {
                at: round_to_half_hour(at),
                soc: target.round() as u8,
            };
        }
        // Not to full: if the forecast at least sees this charge (an
        // afternoon top-up off the last of the sun), say when the evening
        // takes it to the reserve. If it doesn't (a Force Charge, which it
        // doesn't model), there's nothing to go on.
        let forecast_rises = unfloored_series(inputs, 1)
            .first()
            .is_some_and(|(_, soc)| *soc > inputs.params.start_soc_pct);
        if !forecast_rises {
            return RuntimeOutcome::Charging;
        }
        return empty_outcome(inputs, true);
    }
    let (soc, reserve) = (inputs.params.start_soc_pct, inputs.params.reserve_soc_pct);
    // HEM's reserve pauses write 100; the inverter's own pause leaves the
    // reserve alone. A floor below 100 holds it at its reserve instead.
    if reserve >= 100.0 || inputs.discharge_paused {
        return RuntimeOutcome::Paused;
    }
    if soc <= reserve + AT_RESERVE_PCT {
        return RuntimeOutcome::AtReserve;
    }
    empty_outcome(inputs, false)
}

/// What the battery does next on the median day, with the other two days
/// for the confidence. See [`first_event`] for `topping_up`.
fn empty_outcome(inputs: &RuntimeInputs, topping_up: bool) -> RuntimeOutcome {
    let empty = |event: Option<Event>| match event {
        Some(Event::Empty(at)) => Some(at),
        _ => None,
    };
    let Some(central) = first_event(inputs, 1, topping_up) else {
        return RuntimeOutcome::Unknown;
    };
    match central {
        Event::Recharge(at, by) => RuntimeOutcome::RechargesFirst {
            at: round_to_half_hour(at),
            by,
        },
        Event::Nothing => RuntimeOutcome::LastsBeyond {
            hours: RUNTIME_HORIZON_HOURS,
        },
        Event::Empty(at) => {
            let earliest = empty(first_event(inputs, 0, topping_up));
            let latest = empty(first_event(inputs, 2, topping_up));
            let spread = earliest.zip(latest).map(|(e, l)| (l - e).abs());
            let mut confidence = match spread {
                Some(s) if s <= HIGH_SPREAD_SECS => Confidence::High,
                Some(s) if s <= MEDIUM_SPREAD_SECS => Confidence::Medium,
                _ => Confidence::Low,
            };
            if inputs.profile_days < CONFIDENT_PROFILE_DAYS {
                confidence = Confidence::Low;
            } else if !inputs.solar_known && confidence == Confidence::High {
                confidence = Confidence::Medium;
            }
            RuntimeOutcome::EmptyAt {
                at: round_to_half_hour(at),
                earliest: earliest.map(round_to_half_hour),
                latest: latest.map(round_to_half_hour),
                confidence,
            }
        }
    }
}

fn band_for(series: &[ForecastConsumptionHour], hour: u32) -> Option<&ForecastConsumptionHour> {
    series.iter().find(|c| c.hour == hour as u8)
}

/// The hourly inputs from a forecast payload built at `now` (with the
/// current hour included): the forecast's solar where it has it, else 0,
/// and the profile's band for each hour's local hour and day type.
pub fn runtime_hours(payload: &ForecastPayload, now: DateTime<Local>) -> Vec<RuntimeHour> {
    let now_ts = now.timestamp();
    let first = now_ts - now_ts.rem_euclid(HOUR_SECS);
    (0..=RUNTIME_HORIZON_HOURS)
        .map(|i| first + i * HOUR_SECS)
        .filter(|ts| *ts < now_ts + RUNTIME_HORIZON_HOURS * HOUR_SECS)
        .map(|ts| {
            let local = DateTime::from_timestamp(ts, 0)
                .map(|dt| dt.with_timezone(&Local))
                .unwrap_or(now);
            let weekend = local.weekday().number_from_monday() >= 6;
            let series = if weekend {
                &payload.consumption_weekend
            } else {
                &payload.consumption_weekday
            };
            let consumption =
                band_for(series, local.hour()).map_or([0.0; 3], |c| [c.p75, c.kwh, c.p25]);
            let solar = payload
                .solar
                .iter()
                .find(|s| s.timestamp == ts)
                .map_or([0.0; 3], |s| [s.band_low, s.kwh, s.band_high]);
            RuntimeHour {
                timestamp: ts,
                solar,
                consumption,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forecast::current_schedule::ScheduledWindow;
    use chrono::TimeZone;

    /// Midnight UTC on a Monday; nothing here depends on the time zone
    /// unless a schedule is involved.
    const T0: i64 = 1_762_732_800; // 2025-11-10T00:00:00Z

    fn params(start_soc: f64) -> SimulationParams {
        SimulationParams {
            capacity_kwh: 10.0,
            start_soc_pct: start_soc,
            reserve_soc_pct: 10.0,
            max_charge_kw: 5.0,
            max_discharge_kw: 5.0,
            charge_efficiency: 1.0,
            discharge_efficiency: 1.0,
        }
    }

    /// 24 hours from `start`: no solar, the given load band every hour.
    fn inputs(now_ts: i64, start: i64, start_soc: f64, load: [f64; 3]) -> RuntimeInputs {
        RuntimeInputs {
            now_ts,
            hours: (0..24)
                .map(|i| RuntimeHour {
                    timestamp: start + i * HOUR_SECS,
                    solar: [0.0; 3],
                    consumption: load,
                })
                .collect(),
            dispatch_hours: Vec::new(),
            dispatch_net_of_profile: false,
            params: params(start_soc),
            schedule: CurrentSchedule::default(),
            battery_power_w: 500.0,
            live_solar_w: 0.0,
            discharge_paused: false,
            profile_days: 30,
            solar_known: true,
        }
    }

    fn empty_at(outcome: &RuntimeOutcome) -> (i64, Option<i64>, Option<i64>, Confidence) {
        match outcome {
            RuntimeOutcome::EmptyAt {
                at,
                earliest,
                latest,
                confidence,
            } => (*at, *earliest, *latest, *confidence),
            other => panic!("expected EmptyAt, got {other:?}"),
        }
    }

    #[test]
    fn empties_when_the_load_brings_it_to_the_reserve_from_the_current_minute() {
        // 50% of 10 kWh to the 10% reserve is 4 kWh; at 1 kWh an hour, from
        // half past, that's 04:30. The first half-hour only drains half.
        let now = T0 + 1800;
        let (at, earliest, latest, confidence) =
            empty_at(&battery_runtime(&inputs(now, T0, 50.0, [1.0, 1.0, 1.0])));
        assert_eq!(at, T0 + 4 * HOUR_SECS + 1800);
        assert_eq!((earliest, latest), (Some(at), Some(at)));
        assert_eq!(confidence, Confidence::High);
    }

    #[test]
    fn rounds_to_the_nearest_half_hour() {
        assert_eq!(round_to_half_hour(T0 + 14 * 60), T0);
        assert_eq!(round_to_half_hour(T0 + 15 * 60), T0 + 1800);
        assert_eq!(round_to_half_hour(T0 + 44 * 60), T0 + 1800);
        assert_eq!(round_to_half_hour(T0 + 46 * 60), T0 + 3600);
        // 0.8 kWh an hour from 50%: 5 h exactly → 05:00; 0.9: 4 h 27 → 04:30.
        let (at, ..) = empty_at(&battery_runtime(&inputs(T0, T0, 50.0, [0.9, 0.9, 0.9])));
        assert_eq!(at, T0 + 4 * HOUR_SECS + 1800);
    }

    #[test]
    fn the_spread_of_the_profile_sets_the_confidence() {
        // p75 1.25 → 3 h 12; p25 0.75 → 5 h 20: over two hours apart.
        let (at, earliest, latest, confidence) =
            empty_at(&battery_runtime(&inputs(T0, T0, 50.0, [1.25, 1.0, 0.75])));
        assert_eq!(at, T0 + 4 * HOUR_SECS);
        assert_eq!(earliest, Some(T0 + 3 * HOUR_SECS));
        assert_eq!(latest, Some(T0 + 5 * HOUR_SECS + 1800));
        assert_eq!(confidence, Confidence::Medium);
        // Far apart: low.
        let (.., confidence) = empty_at(&battery_runtime(&inputs(T0, T0, 50.0, [2.0, 1.0, 0.4])));
        assert_eq!(confidence, Confidence::Low);
        // Close together: high.
        let (.., confidence) = empty_at(&battery_runtime(&inputs(T0, T0, 50.0, [1.05, 1.0, 0.95])));
        assert_eq!(confidence, Confidence::High);
    }

    #[test]
    fn an_optimistic_day_that_never_empties_means_low_confidence() {
        let mut i = inputs(T0, T0, 50.0, [1.0, 0.5, 0.0]);
        i.hours.truncate(9);
        let (_, _, latest, confidence) = empty_at(&battery_runtime(&i));
        assert_eq!(latest, None);
        assert_eq!(confidence, Confidence::Low);
    }

    #[test]
    fn a_thin_history_or_unknown_solar_lowers_the_confidence() {
        let mut thin = inputs(T0, T0, 50.0, [1.0, 1.0, 1.0]);
        thin.profile_days = 10;
        assert_eq!(empty_at(&battery_runtime(&thin)).3, Confidence::Low);
        let mut no_solar = inputs(T0, T0, 50.0, [1.0, 1.0, 1.0]);
        no_solar.solar_known = false;
        assert_eq!(empty_at(&battery_runtime(&no_solar)).3, Confidence::Medium);
    }

    #[test]
    fn solar_that_refills_it_first_is_said_instead() {
        let mut i = inputs(T0, T0, 30.0, [1.0, 1.0, 1.0]);
        // From the second hour, 3 kWh of sun against 1 kWh of load.
        for hour in &mut i.hours[1..] {
            hour.solar = [3.0; 3];
        }
        assert_eq!(
            battery_runtime(&i),
            RuntimeOutcome::RechargesFirst {
                at: T0 + HOUR_SECS,
                by: RechargeSource::Solar
            }
        );
    }

    #[test]
    fn a_charge_slot_that_refills_it_first_is_said_with_its_start() {
        // Local times, because the schedule is on the inverter's clock.
        let start = Local
            .with_ymd_and_hms(2025, 11, 10, 20, 0, 0)
            .earliest()
            .unwrap()
            .timestamp();
        let mut i = inputs(start, start, 50.0, [1.0, 1.0, 1.0]);
        i.schedule = CurrentSchedule {
            charge_windows: vec![ScheduledWindow {
                start_min: 23 * 60 + 30,
                end_min: 5 * 60 + 30,
                rate_kw: 3.0,
                target_soc_pct: 100.0,
            }],
            discharge_windows: Vec::new(),
        };
        let slot = Local
            .with_ymd_and_hms(2025, 11, 10, 23, 30, 0)
            .earliest()
            .unwrap()
            .timestamp();
        assert_eq!(
            battery_runtime(&i),
            RuntimeOutcome::RechargesFirst {
                at: slot,
                by: RechargeSource::Charge
            }
        );
    }

    #[test]
    fn a_smart_charge_top_up_is_a_charge_not_solar() {
        // Issue #434: during a held dispatch HEM tops the battery up. That
        // refill comes from the grid at night, so it's a charge.
        let mut i = inputs(T0, T0, 50.0, [1.0, 1.0, 1.0]);
        i.dispatch_hours = vec![DispatchHour {
            timestamp: T0 + 2 * HOUR_SECS,
            ev_kwh: 0.0,
            held_fraction: 1.0,
            charge_to_soc: Some(100.0),
        }];
        assert_eq!(
            battery_runtime(&i),
            RuntimeOutcome::RechargesFirst {
                at: T0 + 2 * HOUR_SECS,
                by: RechargeSource::Charge
            }
        );
    }

    #[test]
    fn past_a_running_slot_target_it_charges_on_towards_full() {
        // An 80% slot is running but the sun has already taken it to 85%:
        // it isn't heading for 80 any more.
        let start = Local
            .with_ymd_and_hms(2025, 11, 10, 1, 0, 0)
            .earliest()
            .unwrap()
            .timestamp();
        let mut i = inputs(start, start, 85.0, [1.0, 1.0, 1.0]);
        i.battery_power_w = -500.0;
        i.schedule = CurrentSchedule {
            charge_windows: vec![ScheduledWindow {
                start_min: 30,
                end_min: 5 * 60 + 30,
                rate_kw: 3.0,
                target_soc_pct: 80.0,
            }],
            discharge_windows: Vec::new(),
        };
        for hour in &mut i.hours {
            hour.solar = [3.0; 3];
        }
        match battery_runtime(&i) {
            RuntimeOutcome::FullAt { soc, at } => {
                assert_eq!(soc, 100);
                assert!(at > start, "not the current minute");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_held_dispatch_keeps_the_battery_for_its_hours() {
        // Two held hours early on push the empty time back by two hours.
        let mut i = inputs(T0, T0, 50.0, [1.0, 1.0, 1.0]);
        i.dispatch_hours = vec![
            DispatchHour {
                timestamp: T0 + HOUR_SECS,
                ev_kwh: 0.0,
                held_fraction: 1.0,
                charge_to_soc: None,
            },
            DispatchHour {
                timestamp: T0 + 2 * HOUR_SECS,
                ev_kwh: 0.0,
                held_fraction: 1.0,
                charge_to_soc: None,
            },
        ];
        assert_eq!(empty_at(&battery_runtime(&i)).0, T0 + 6 * HOUR_SECS);
    }

    #[test]
    fn lasts_beyond_the_horizon_when_nothing_happens() {
        let i = inputs(T0, T0, 90.0, [0.1, 0.1, 0.1]);
        assert_eq!(
            battery_runtime(&i),
            RuntimeOutcome::LastsBeyond {
                hours: RUNTIME_HORIZON_HOURS
            }
        );
    }

    #[test]
    fn while_charging_on_solar_it_says_when_it_is_full() {
        // 3 kWh of sun against 1 kWh of load: 2 kWh an hour into a 10 kWh
        // battery, 50% to 100% in two and a half hours.
        let mut i = inputs(T0, T0, 50.0, [1.0, 1.0, 1.0]);
        i.battery_power_w = -2000.0;
        for hour in &mut i.hours {
            hour.solar = [3.0; 3];
        }
        assert_eq!(
            battery_runtime(&i),
            RuntimeOutcome::FullAt {
                at: T0 + 2 * HOUR_SECS + 1800,
                soc: 100
            }
        );
    }

    #[test]
    fn while_a_charge_slot_runs_it_says_when_it_reaches_the_slot_target() {
        let start = Local
            .with_ymd_and_hms(2025, 11, 10, 1, 0, 0)
            .earliest()
            .unwrap()
            .timestamp();
        let mut i = inputs(start, start, 50.0, [1.0, 1.0, 1.0]);
        i.battery_power_w = -3000.0;
        i.schedule = CurrentSchedule {
            charge_windows: vec![ScheduledWindow {
                start_min: 30,
                end_min: 5 * 60 + 30,
                rate_kw: 3.0,
                target_soc_pct: 80.0,
            }],
            discharge_windows: Vec::new(),
        };
        // 3 kWh an hour into 10 kWh: 50% to 80% in an hour.
        assert_eq!(
            battery_runtime(&i),
            RuntimeOutcome::FullAt {
                at: start + HOUR_SECS,
                soc: 80
            }
        );
    }

    #[test]
    fn an_afternoon_top_up_that_will_not_fill_it_says_when_it_empties() {
        // Stuart's 14 November: charging off the last of the sun, which
        // lifts it a little before it falls away; the answer is when the
        // evening takes it to the reserve, not nothing and not a "refill".
        let mut i = inputs(T0, T0, 50.0, [1.0, 1.0, 1.0]);
        i.battery_power_w = -500.0;
        i.hours[0].solar = [1.5; 3];
        i.hours[1].solar = [1.2; 3];
        // +5, +2, then 1 kWh an hour down from 57%: 47 points to the
        // reserve takes 4.7 hours from 02:00.
        let (at, ..) = empty_at(&battery_runtime(&i));
        assert_eq!(at, T0 + 6 * HOUR_SECS + 1800);
    }

    #[test]
    fn live_sun_above_a_dull_forecast_carries_the_rest_of_this_hour() {
        // Issue #476: 1.4 kW of sun at half past while the forecast expects
        // none. Charging at 800 W, the forecast alone can't see the charge
        // and gives no time; the live reading for the rest of the hour can.
        let now = T0 + 1800;
        let mut dull = inputs(now, T0, 50.0, [0.6, 0.6, 0.6]);
        dull.battery_power_w = -800.0;
        assert_eq!(battery_runtime(&dull), RuntimeOutcome::Charging);

        let mut sunny = dull.clone();
        sunny.live_solar_w = 1400.0;
        let (at, earliest, ..) = empty_at(&battery_runtime(&sunny));
        // Later than the same day without the half-hour of sun.
        let mut no_sun_now = sunny.clone();
        no_sun_now.battery_power_w = 0.0;
        no_sun_now.live_solar_w = 0.0;
        let (without, ..) = empty_at(&battery_runtime(&no_sun_now));
        assert!(at > without, "{at} vs {without}");
        // The pessimistic day keeps the forecast: the sun may not last.
        assert!(earliest.is_some_and(|e| e < at), "{earliest:?} vs {at}");
    }

    #[test]
    fn live_sun_never_lowers_a_brighter_forecast_or_reaches_past_this_hour() {
        let mut forecast = inputs(T0, T0, 50.0, [1.0, 1.0, 1.0]);
        forecast.battery_power_w = -2000.0;
        for hour in &mut forecast.hours {
            hour.solar = [3.0; 3];
        }
        let mut dimmer_now = forecast.clone();
        dimmer_now.live_solar_w = 500.0;
        assert_eq!(battery_runtime(&dimmer_now), battery_runtime(&forecast));

        // Bright now, dark forecast from the next hour: only this hour gains.
        let mut one_hour = inputs(T0, T0, 50.0, [1.0, 1.0, 1.0]);
        one_hour.live_solar_w = 3000.0;
        one_hour.battery_power_w = -2000.0;
        let mut forecast_says_so = inputs(T0, T0, 50.0, [1.0, 1.0, 1.0]);
        forecast_says_so.battery_power_w = -2000.0;
        forecast_says_so.hours[0].solar = [0.0, 3.0, 3.0];
        assert_eq!(
            battery_runtime(&one_hour),
            battery_runtime(&forecast_says_so)
        );
    }

    #[test]
    fn a_reading_that_is_not_a_power_is_ignored() {
        for junk in [f64::NAN, f64::INFINITY, -1500.0] {
            let mut i = inputs(T0 + 1800, T0, 50.0, [0.6, 0.6, 0.6]);
            i.battery_power_w = -800.0;
            i.live_solar_w = junk;
            assert_eq!(battery_runtime(&i), RuntimeOutcome::Charging, "{junk}");
        }
    }

    #[test]
    fn says_nothing_while_charging_when_the_projection_does_not_charge() {
        // Charging now (a Force Charge, say), but nothing in the forecast
        // keeps it going: no time rather than a guess.
        let mut charging = inputs(T0, T0, 50.0, [1.0, 1.0, 1.0]);
        charging.battery_power_w = -1500.0;
        assert_eq!(battery_runtime(&charging), RuntimeOutcome::Charging);
    }

    #[test]
    fn at_reserve_when_there() {
        let at_reserve = inputs(T0, T0, 10.3, [1.0, 1.0, 1.0]);
        assert_eq!(battery_runtime(&at_reserve), RuntimeOutcome::AtReserve);
    }

    #[test]
    fn a_reserve_held_above_the_battery_is_a_pause_not_empty() {
        // HEM's pauses (the dispatch hold, the limiters, the Pause button)
        // write the reserve to 100; a Saving Session floor can hold it above
        // the battery too. Nobody can say when that ends, so no time.
        let mut paused = inputs(T0, T0, 60.0, [1.0, 1.0, 1.0]);
        paused.params.reserve_soc_pct = 100.0;
        assert_eq!(battery_runtime(&paused), RuntimeOutcome::Paused);
        let mut full = inputs(T0, T0, 100.0, [1.0, 1.0, 1.0]);
        full.params.reserve_soc_pct = 100.0;
        assert_eq!(battery_runtime(&full), RuntimeOutcome::Paused);
        // The inverter's own discharge pause (HR 318), which leaves the
        // reserve alone: an AIO dispatch hold, Timed Discharge, the Pause button.
        let mut native = inputs(T0, T0, 60.0, [1.0, 1.0, 1.0]);
        native.discharge_paused = true;
        assert_eq!(battery_runtime(&native), RuntimeOutcome::Paused);
        // A raised floor below 100 (a Saving Session) holds it where it is:
        // at its reserve, as the inverter sees it.
        let mut floor = inputs(T0, T0, 30.0, [1.0, 1.0, 1.0]);
        floor.params.reserve_soc_pct = 45.0;
        assert_eq!(battery_runtime(&floor), RuntimeOutcome::AtReserve);
        // Below its reserve after an outage or standby losses: at it, not paused.
        for soc in [9.0, 17.0] {
            let mut under = inputs(T0, T0, soc, [1.0, 1.0, 1.0]);
            under.params.reserve_soc_pct = 20.0;
            assert_eq!(battery_runtime(&under), RuntimeOutcome::AtReserve, "{soc}");
        }
    }

    #[test]
    fn a_simulation_that_refuses_its_inputs_is_unknown_not_lasting() {
        // A corrupt SOC the simulator won't start from: no projection at
        // all, which must not read as "lasts over 24 hours".
        let corrupt = inputs(T0, T0, 150.0, [1.0, 1.0, 1.0]);
        assert_eq!(battery_runtime(&corrupt), RuntimeOutcome::Unknown);
        let mut no_hours = inputs(T0, T0, 50.0, [1.0, 1.0, 1.0]);
        no_hours.hours.clear();
        assert_eq!(battery_runtime(&no_hours), RuntimeOutcome::Unknown);
    }

    #[test]
    fn charging_is_judged_by_one_threshold() {
        assert!(is_charging(-150.0));
        assert!(!is_charging(-50.0));
        assert!(!is_charging(f64::NAN));
    }

    #[test]
    fn serialises_with_the_outcome_as_a_tag() {
        let json = serde_json::to_value(RuntimeOutcome::EmptyAt {
            at: 10,
            earliest: None,
            latest: Some(20),
            confidence: Confidence::Medium,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "outcome": "empty_at", "at": 10, "earliest": null, "latest": 20, "confidence": "medium" })
        );
        let json = serde_json::to_value(RuntimeOutcome::RechargesFirst {
            at: 5,
            by: RechargeSource::Charge,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "outcome": "recharges_first", "at": 5, "by": "charge" })
        );
    }
}
