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
use crate::forecast::dispatch_load::{apply_dispatch_hours, DispatchHour, DispatchWindow};
use crate::forecast::simulate::{
    modelled_reserve, simulate_battery, SimHourInput, SimulationParams,
};
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
/// SOC more than this under the reserve is below it: any whole point under,
/// with the same float tolerance as [`AT_RESERVE_PCT`]. The house view's
/// flash (`houseFlow.ts::BELOW_RESERVE_POINTS`) waits for two points under,
/// so a battery parked a point low doesn't flash.
const BELOW_RESERVE_PCT: f64 = 0.5;

/// Where the SOC sits against the reserve: -1 below it, 0 at it, 1 above.
/// "At" runs a point above the reserve, where a cold battery really stops
/// (issue #479); "below" is measured from the reserve itself. The
/// endpoint's cache keys on it, so a one-point move across an edge gets a
/// fresh answer.
pub fn reserve_band(soc: f64, reserve: f64) -> i8 {
    if soc < reserve - BELOW_RESERVE_PCT {
        -1
    } else if soc <= modelled_reserve(reserve) + AT_RESERVE_PCT {
        0
    } else {
        1
    }
}
/// A rise of more than this in an hour means the battery is being refilled.
const RISE_PCT: f64 = 0.5;
/// Battery power below this (W, negative = charging) means charging now.
const CHARGING_W: f64 = -100.0;
/// Battery power above this (W, positive = discharging) means discharging now.
const DISCHARGING_W: f64 = 100.0;

/// Whether the battery is charging now, by its power (W, negative =
/// charging). The endpoint's cache keys on the same test.
pub fn is_charging(battery_power_w: f64) -> bool {
    battery_power_w.is_finite() && battery_power_w < CHARGING_W
}
/// Whether the battery is discharging now, by its power (W, positive =
/// discharging).
pub fn is_discharging(battery_power_w: f64) -> bool {
    battery_power_w.is_finite() && battery_power_w > DISCHARGING_W
}

/// The battery's direction now: -1 charging, 1 discharging, 0 idle. The
/// endpoint's cache keys on it, so a flip gets a fresh answer.
pub fn battery_direction(battery_power_w: f64) -> i8 {
    if is_charging(battery_power_w) {
        -1
    } else if is_discharging(battery_power_w) {
        1
    } else {
        0
    }
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
    /// The planned charges themselves, so a top-up is said to start when its
    /// dispatch does, not at the start of the hour it falls in.
    pub dispatch_windows: Vec<DispatchWindow>,
    pub dispatch_net_of_profile: bool,
    /// Capacity, the live SOC as the start, the reserve, rates, efficiencies.
    /// Unlike the payload's params, `reserve_soc_pct` here is the inverter's
    /// own reserve: the pause test and "Below Reserve" read it as it is, and
    /// the runtime applies `modelled_reserve` itself (issue #479), so don't
    /// build it from modelled params or the margin is counted twice.
    pub params: SimulationParams,
    pub schedule: CurrentSchedule,
    /// Battery power now, W (negative = charging).
    pub battery_power_w: f64,
    /// Solar power now, W. Lifts the current hour's median and optimistic
    /// solar when the sun is beating the forecast (issue #476).
    pub live_solar_w: f64,
    /// House load now, W. Lifts the current hour's load when the house is
    /// drawing more than the profile for the hour (issue #472).
    pub live_home_w: f64,
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
    /// Refilled before it empties: by a charge slot or by solar, from `at`
    /// (a charge's own start, exact; solar's, rounded). `risk` is when a
    /// busy day (p75 load, low sun) would run out first, when it would
    /// (issue #472), with its own `confidence`.
    RechargesFirst {
        at: i64,
        by: RechargeSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        risk: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        confidence: Option<Confidence>,
    },
    /// Neither empties nor refills within the horizon.
    LastsBeyond { hours: i64 },
    /// Already at the reserve.
    AtReserve,
    /// Under the reserve (after an outage or standby losses, or a raised
    /// floor such as a Saving Session's): the inverter won't discharge it.
    BelowReserve,
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

/// Round to the nearest [`ROUND_SECS`], but never back before `now_ts`: a
/// time that has passed is no answer to "how long", so one that would round
/// into the past rounds up instead.
pub fn round_not_before(ts: i64, now_ts: i64) -> i64 {
    let rounded = round_to_half_hour(ts);
    if rounded >= now_ts {
        rounded
    } else {
        (ts.max(now_ts) + ROUND_SECS - 1).div_euclid(ROUND_SECS) * ROUND_SECS
    }
}

/// The hourly inputs for day `which` (0 pessimistic, 1 median, 2
/// optimistic), the current hour cut to what's left of it.
///
/// For the rest of the current hour the median and optimistic days take the
/// live solar where it beats the forecast (issue #476): a sunny morning on a
/// dull forecast otherwise can't see the charge it's making. The pessimistic
/// day keeps the forecast, since the sun may not last the hour.
///
/// The live load does the same for the house (issue #472): every day takes
/// the greater of the profile and the live load for the rest of the hour,
/// since a heavy load now is the one thing known for sure. And while the
/// battery is discharging it keeps draining at least at the rate it is
/// draining now: the sun isn't beating the house, whatever the forecast says.
fn sim_hours(inputs: &RuntimeInputs, which: usize) -> Vec<SimHourInput> {
    let live_kwh = (which > 0 && inputs.live_solar_w.is_finite() && inputs.live_solar_w > 0.0)
        .then(|| inputs.live_solar_w / 1000.0);
    // The live load includes a car that's charging, which a planned car
    // charge adds again from the dispatch overlay: leave it to the overlay.
    let first_ts = inputs.hours.first().map(|h| h.timestamp);
    let car_planned_now = inputs
        .dispatch_hours
        .iter()
        .any(|d| Some(d.timestamp) == first_ts && d.ev_kwh > 0.0);
    let live_load_kwh =
        (inputs.live_home_w.is_finite() && inputs.live_home_w > 0.0 && !car_planned_now)
            .then(|| inputs.live_home_w / 1000.0);
    let discharge_kw =
        is_discharging(inputs.battery_power_w).then(|| inputs.battery_power_w / 1000.0);
    let mut hours: Vec<SimHourInput> = inputs
        .hours
        .iter()
        .enumerate()
        .map(|(i, hour)| {
            let solar_kwh = match live_kwh {
                Some(live) if i == 0 => hour.solar[which].max(live),
                _ => hour.solar[which],
            };
            let mut consumption_kwh = hour.consumption[which];
            if i == 0 {
                if let Some(live) = live_load_kwh {
                    consumption_kwh = consumption_kwh.max(live);
                }
                // Draining now: at least at the rate it's draining, whatever
                // the forecast sun says (Force Discharge included).
                if let Some(kw) = discharge_kw {
                    consumption_kwh = consumption_kwh.max(solar_kwh + kw);
                }
            }
            SimHourInput {
                timestamp: hour.timestamp,
                solar_kwh,
                consumption_kwh,
                ..Default::default()
            }
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

/// When the charge slot that refills the battery in the hour at `ts` started:
/// within that hour, or, when it already ran into it from earlier hours
/// that didn't lift the battery (ten minutes of a 23:50 slot don't), the
/// start of that run. `None` when no slot overlaps the hour. `floor_ts` is
/// as far back as to look (the hour now is in).
fn charge_start_for_hour(schedule: &CurrentSchedule, ts: i64, floor_ts: i64) -> Option<i64> {
    let mut best: Option<i64> = None;
    for w in &schedule.charge_windows {
        let segments = |hour: i64| window_overlap_segments(hour, w.start_min, w.end_min, &Local);
        let Some(mut start) = segments(ts)
            .into_iter()
            .map(|(start, _)| (ts, start))
            .min_by(|a, b| a.1.total_cmp(&b.1))
        else {
            continue;
        };
        // Started at the top of the hour: it may have been running already.
        let mut hour = ts;
        while start.1 <= 0.0 && hour > floor_ts {
            let prev = hour - HOUR_SECS;
            match segments(prev)
                .into_iter()
                .filter(|(_, end)| *end >= 1.0)
                .map(|(s, _)| s)
                .min_by(f64::total_cmp)
            {
                Some(s) => {
                    start = (prev, s);
                    hour = prev;
                }
                None => break,
            }
        }
        let at = start.0 + (start.1 * HOUR_SECS as f64).round() as i64;
        best = Some(best.map_or(at, |b| b.min(at)));
    }
    best
}

/// When a held smart dispatch starts inside the hour at `ts`, which is when
/// a #434 top-up starts charging; `None` when the windows don't say.
fn dispatch_hold_start_in_hour(windows: &[DispatchWindow], ts: i64) -> Option<i64> {
    windows
        .iter()
        .filter(|w| w.held && w.end > ts && w.start < ts + HOUR_SECS)
        .map(|w| w.start.max(ts))
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
    // Where the battery stops: a point above the reserve (issue #479).
    let reserve = modelled_reserve(inputs.params.reserve_soc_pct);
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
            let floor = inputs.now_ts - inputs.now_ts.rem_euclid(HOUR_SECS);
            return Some(match charge_start_for_hour(&inputs.schedule, ts, floor) {
                Some(at) => Event::Recharge(at.max(inputs.now_ts), RechargeSource::Charge),
                None if top_up => Event::Recharge(
                    dispatch_hold_start_in_hour(&inputs.dispatch_windows, ts)
                        .unwrap_or(ts)
                        .max(inputs.now_ts),
                    RechargeSource::Charge,
                ),
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
                at: round_not_before(at, inputs.now_ts),
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
    // reserve alone. Under a floor below 100 the battery is held where it
    // is: at its reserve, or below it once a point under.
    if reserve >= 100.0 || inputs.discharge_paused {
        return RuntimeOutcome::Paused;
    }
    match reserve_band(soc, reserve) {
        -1 => return RuntimeOutcome::BelowReserve,
        0 => return RuntimeOutcome::AtReserve,
        _ => {}
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
        Event::Recharge(at, by) => {
            // A charge's start is a fact of the schedule, said as it is; a
            // solar refill is a prediction, rounded.
            let at = match by {
                RechargeSource::Charge => at.max(inputs.now_ts),
                RechargeSource::Solar => round_not_before(at, inputs.now_ts),
            };
            // The median day reaches the charge, but a busy one may not.
            let risk = empty(first_event(inputs, 0, topping_up))
                .map(|t| round_not_before(t, inputs.now_ts))
                .filter(|risk| *risk < at);
            RuntimeOutcome::RechargesFirst {
                at,
                by,
                risk,
                confidence: risk.map(|risk| confidence_for(Some(at - risk), inputs)),
            }
        }
        Event::Nothing => RuntimeOutcome::LastsBeyond {
            hours: RUNTIME_HORIZON_HOURS,
        },
        Event::Empty(at) => {
            let earliest = empty(first_event(inputs, 0, topping_up));
            let latest = empty(first_event(inputs, 2, topping_up));
            let spread = earliest.zip(latest).map(|(e, l)| (l - e).abs());
            let earliest = earliest.map(|t| round_not_before(t, inputs.now_ts));
            let latest = latest.map(|t| round_not_before(t, inputs.now_ts));
            RuntimeOutcome::EmptyAt {
                at: clamp_into_range(round_not_before(at, inputs.now_ts), earliest, latest),
                earliest,
                latest,
                confidence: confidence_for(spread, inputs),
            }
        }
    }
}

/// The confidence for a spread (seconds): between a busy and a quiet day's
/// empty times, or between a busy day's and the refill that comes first.
/// `None` (one of the days never empties) is low.
fn confidence_for(spread: Option<i64>, inputs: &RuntimeInputs) -> Confidence {
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
    confidence
}

/// The most likely time kept inside its own range, so the line never says
/// "most likely 22:00" for a range that starts at 22:30. A missing end (that
/// day doesn't empty in the horizon) leaves that side open.
pub(crate) fn clamp_into_range(at: i64, earliest: Option<i64>, latest: Option<i64>) -> i64 {
    let at = earliest.map_or(at, |e| at.max(e));
    latest.map_or(at, |l| at.min(l.max(earliest.unwrap_or(l))))
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
            dispatch_windows: Vec::new(),
            dispatch_net_of_profile: false,
            params: params(start_soc),
            schedule: CurrentSchedule::default(),
            battery_power_w: 500.0,
            live_solar_w: 0.0,
            live_home_w: 0.0,
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
        // To the 11% floor, 0.8 kWh an hour from 50%: 4 h 52 → 05:00; 0.9: 4 h 20 → 04:30.
        let (at, ..) = empty_at(&battery_runtime(&inputs(T0, T0, 50.0, [0.9, 0.9, 0.9])));
        assert_eq!(at, T0 + 4 * HOUR_SECS + 1800);
    }

    #[test]
    fn the_spread_of_the_profile_sets_the_confidence() {
        // To the 11% floor (3.9 kWh): p75 1.25 → 3 h 07; median 3 h 54;
        // p25 0.75 → 5 h 12: two hours apart.
        let (at, earliest, latest, confidence) =
            empty_at(&battery_runtime(&inputs(T0, T0, 50.0, [1.25, 1.0, 0.75])));
        assert_eq!(at, T0 + 4 * HOUR_SECS);
        assert_eq!(earliest, Some(T0 + 3 * HOUR_SECS));
        assert_eq!(latest, Some(T0 + 5 * HOUR_SECS));
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
                by: RechargeSource::Solar,
                risk: None,
                confidence: None,
            }
        );
    }

    /// Local 20:00 on a Monday, with a 23:30 charge slot: the evening the
    /// "may run out first" tests ask about.
    fn evening_with_a_slot(load: [f64; 3]) -> (RuntimeInputs, i64) {
        let at = |h, m| {
            Local
                .with_ymd_and_hms(2025, 11, 10, h, m, 0)
                .earliest()
                .unwrap()
                .timestamp()
        };
        let start = at(20, 0);
        let mut i = inputs(start, start, 50.0, load);
        i.schedule = CurrentSchedule {
            charge_windows: vec![ScheduledWindow {
                start_min: 23 * 60 + 30,
                end_min: 5 * 60 + 30,
                rate_kw: 3.0,
                target_soc_pct: 100.0,
            }],
            discharge_windows: Vec::new(),
        };
        (i, at(23, 30))
    }

    #[test]
    fn a_busy_evening_that_runs_out_before_the_charge_is_flagged() {
        // Issue #472, Brian's 17 November: the median day reaches the 23:30
        // charge, but to the 11% floor (3.9 kWh) a 1.5 kWh-an-hour evening
        // is empty 2 h 36 in, 22:36 → 22:30, and the line said "Lasts until
        // the charge" with no doubt.
        let (i, slot) = evening_with_a_slot([1.5, 1.0, 0.8]);
        match battery_runtime(&i) {
            RuntimeOutcome::RechargesFirst {
                at,
                by: RechargeSource::Charge,
                risk: Some(risk),
                confidence: Some(confidence),
            } => {
                assert_eq!(at, slot);
                assert_eq!(risk, slot - HOUR_SECS);
                // An hour before the charge: high.
                assert_eq!(confidence, Confidence::High);
            }
            other => panic!("expected a flagged RechargesFirst, got {other:?}"),
        }
    }

    #[test]
    fn a_busy_evening_that_lasts_to_the_charge_too_is_not_flagged() {
        // p75 3.9/1.05 = 3 h 43 → past 23:30, so nothing to warn about.
        let (i, slot) = evening_with_a_slot([1.05, 1.0, 0.8]);
        assert_eq!(
            battery_runtime(&i),
            RuntimeOutcome::RechargesFirst {
                at: slot,
                by: RechargeSource::Charge,
                risk: None,
                confidence: None,
            }
        );
    }

    #[test]
    fn the_risk_confidence_follows_the_gap_to_the_charge_and_the_history() {
        // Empty 20:00 + 3.9/3.0 h = 21:18 → 21:30; the charge is 2 h later: medium.
        let (i, slot) = evening_with_a_slot([3.0, 1.0, 0.8]);
        match battery_runtime(&i) {
            RuntimeOutcome::RechargesFirst {
                risk: Some(risk),
                confidence: Some(Confidence::Medium),
                ..
            } => assert_eq!(slot - risk, 2 * HOUR_SECS),
            other => panic!("{other:?}"),
        }
        // Far away: low; a thin history: always low.
        // (A battery already a point off its floor, so a busy evening empties
        // it at once, three and a half hours before the charge.)
        let (mut i, _) = evening_with_a_slot([8.0, 0.01, 0.01]);
        i.params.start_soc_pct = 12.0;
        i.battery_power_w = 0.0;
        assert!(matches!(
            battery_runtime(&i),
            RuntimeOutcome::RechargesFirst {
                confidence: Some(Confidence::Low),
                ..
            }
        ));
        let (mut thin, _) = evening_with_a_slot([1.5, 1.0, 0.8]);
        thin.profile_days = 5;
        assert!(matches!(
            battery_runtime(&thin),
            RuntimeOutcome::RechargesFirst {
                confidence: Some(Confidence::Low),
                ..
            }
        ));
    }

    #[test]
    fn a_risk_that_rounds_onto_the_charge_is_not_flagged() {
        // Empty at 23:20 rounds to 23:30, the charge itself: no warning.
        let (i, _) = evening_with_a_slot([1.13, 1.0, 0.8]);
        match battery_runtime(&i) {
            RuntimeOutcome::RechargesFirst { risk, .. } => assert_eq!(risk, None),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_top_up_is_said_to_start_with_its_dispatch_not_its_hour() {
        // A held dispatch starting 25 minutes into the hour charges from
        // then; a charge's start is said as it is, not rounded to the hour
        // it falls in or to the half hour.
        let mut i = inputs(T0, T0, 50.0, [1.0, 1.0, 1.0]);
        let hour = T0 + 2 * HOUR_SECS;
        i.dispatch_hours = vec![DispatchHour {
            timestamp: hour,
            ev_kwh: 0.0,
            held_fraction: 0.6,
            charge_to_soc: Some(100.0),
        }];
        i.dispatch_windows = vec![DispatchWindow {
            start: hour + 25 * 60,
            end: hour + HOUR_SECS + 25 * 60,
            held: true,
            smart: true,
            kwh: None,
        }];
        match battery_runtime(&i) {
            RuntimeOutcome::RechargesFirst {
                at,
                by: RechargeSource::Charge,
                ..
            } => assert_eq!(at, hour + 25 * 60),
            other => panic!("{other:?}"),
        }
        // A dispatch that began earlier, or no window to go on: the hour.
        i.dispatch_windows[0].start = hour - 10 * 60;
        assert!(matches!(
            battery_runtime(&i),
            RuntimeOutcome::RechargesFirst { at, .. } if at == hour
        ));
        i.dispatch_windows.clear();
        assert!(matches!(
            battery_runtime(&i),
            RuntimeOutcome::RechargesFirst { at, .. } if at == hour
        ));
    }

    #[test]
    fn a_charge_slot_starting_off_the_half_hour_is_said_as_it_is() {
        let (mut i, _) = evening_with_a_slot([1.0, 1.0, 1.0]);
        i.schedule.charge_windows[0].start_min = 23 * 60 + 15;
        let start = i.now_ts;
        let at = Local
            .with_ymd_and_hms(2025, 11, 10, 23, 15, 0)
            .earliest()
            .unwrap()
            .timestamp();
        assert!(at > start);
        assert!(matches!(
            battery_runtime(&i),
            RuntimeOutcome::RechargesFirst { at: got, .. } if got == at
        ));
    }

    #[test]
    fn a_charge_slot_starting_late_in_an_hour_is_said_at_its_start() {
        // A 23:50 slot whose ten minutes don't lift the 23:00 hour (the load
        // takes more than they give) is first seen rising in the 00:00 hour,
        // but it started at 23:50.
        let (mut i, _) = evening_with_a_slot([0.6, 0.6, 0.6]);
        i.battery_power_w = 0.0;
        i.schedule.charge_windows[0].start_min = 23 * 60 + 50;
        let at = Local
            .with_ymd_and_hms(2025, 11, 10, 23, 50, 0)
            .earliest()
            .unwrap()
            .timestamp();
        match battery_runtime(&i) {
            RuntimeOutcome::RechargesFirst { at: got, .. } => assert_eq!(got, at),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_most_likely_time_stays_inside_its_range() {
        assert_eq!(clamp_into_range(100, Some(90), Some(120)), 100);
        assert_eq!(clamp_into_range(80, Some(90), Some(120)), 90);
        assert_eq!(clamp_into_range(130, Some(90), Some(120)), 120);
        // An end that never gets there leaves that side open.
        assert_eq!(clamp_into_range(130, Some(90), None), 130);
        assert_eq!(clamp_into_range(80, None, Some(120)), 80);
        assert_eq!(clamp_into_range(80, None, None), 80);
        // Crossed ends: the earlier bound wins rather than panicking.
        assert_eq!(clamp_into_range(100, Some(120), Some(90)), 120);
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
                by: RechargeSource::Charge,
                risk: None,
                confidence: None,
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
                by: RechargeSource::Charge,
                risk: None,
                confidence: None,
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
    fn a_heavy_load_now_is_not_mistaken_for_solar_refilling_it() {
        // Issue #472, BrianUK6 on 16 November: 10:43, 71%, discharging
        // 667 W, 1.3 kW of sun against a 2.0 kW house, on a forecast of
        // 0.6 kWh for the hour and a profile of 0.8. Counting the live sun
        // but not the live load said "Lasts until solar at 10:30".
        let start = T0 + 10 * HOUR_SECS;
        let now = start + 43 * 60;
        let mut brian = inputs(now, start, 71.0, [0.8, 0.8, 0.8]);
        brian.hours[0].solar = [0.6, 0.6, 0.6];
        brian.battery_power_w = 667.0;
        brian.live_solar_w = 1300.0;
        brian.live_home_w = 2000.0;
        let (at, ..) = empty_at(&battery_runtime(&brian));
        assert!(at > now + 2 * HOUR_SECS, "{at}");
    }

    #[test]
    fn the_current_hour_never_rises_while_the_battery_is_discharging() {
        // The forecast expects 3 kWh of sun this hour but the battery is
        // discharging: the sun isn't there, whatever the forecast says.
        let now = T0 + 1800;
        let mut i = inputs(now, T0, 50.0, [1.0, 1.0, 1.0]);
        i.hours[0].solar = [3.0, 3.0, 3.0];
        i.battery_power_w = 500.0;
        assert!(matches!(
            battery_runtime(&i),
            RuntimeOutcome::EmptyAt { .. }
        ));
        // Idle, the forecast stands.
        i.battery_power_w = 0.0;
        assert!(matches!(
            battery_runtime(&i),
            RuntimeOutcome::RechargesFirst { .. }
        ));
    }

    #[test]
    fn live_load_lifts_only_the_rest_of_this_hour_and_never_lowers_the_profile() {
        let now = T0 + 1800;
        let base = inputs(now, T0, 50.0, [1.0, 1.0, 1.0]);
        let (base_at, ..) = empty_at(&battery_runtime(&base));
        // A lighter load now than the profile changes nothing.
        let mut light = base.clone();
        light.live_home_w = 300.0;
        assert_eq!(battery_runtime(&light), battery_runtime(&base));
        // A heavier one empties it sooner, but only by the rest of the hour.
        let mut heavy = base.clone();
        heavy.live_home_w = 3000.0;
        let (heavy_at, ..) = empty_at(&battery_runtime(&heavy));
        assert!(heavy_at < base_at, "{heavy_at} vs {base_at}");
        assert!(base_at - heavy_at <= HOUR_SECS, "{heavy_at} vs {base_at}");
        // Not a power: ignored.
        for junk in [f64::NAN, f64::INFINITY, -1500.0] {
            let mut i = base.clone();
            i.live_home_w = junk;
            assert_eq!(battery_runtime(&i), battery_runtime(&base), "{junk}");
        }
    }

    #[test]
    fn a_time_given_is_never_in_the_past() {
        // Sun refilling from this hour's start, asked at 10:44 and 10:14.
        for minute in [14, 44] {
            let now = T0 + 10 * HOUR_SECS + minute * 60;
            let mut i = inputs(now, T0 + 10 * HOUR_SECS, 50.0, [1.0, 1.0, 1.0]);
            i.hours[0].solar = [3.0, 3.0, 3.0];
            i.battery_power_w = 0.0;
            match battery_runtime(&i) {
                RuntimeOutcome::RechargesFirst { at, .. } => {
                    assert!(at >= now, "minute {minute}: {at} < {now}");
                    assert_eq!(at % ROUND_SECS, 0);
                }
                other => panic!("expected RechargesFirst, got {other:?}"),
            }
        }
        // An empty a few minutes away rounds up, not back.
        let now = T0 + 4 * HOUR_SECS + 20 * 60;
        // (12% against a 10% reserve: the floor is 11%, a point away.)
        let mut i = inputs(now, T0 + 4 * HOUR_SECS, 12.0, [1.0, 1.0, 1.0]);
        i.params.reserve_soc_pct = 10.0;
        let (at, earliest, latest, _) = empty_at(&battery_runtime(&i));
        assert!(at >= now, "{at}");
        assert!(earliest.unwrap() >= now && latest.unwrap() >= now);
    }

    #[test]
    fn a_discharging_battery_keeps_draining_at_its_rate_for_the_rest_of_the_hour() {
        // 3 kWh of forecast sun, a 1 kWh profile, discharging 2 kW: the rest
        // of the hour drains, it doesn't hold flat.
        let now = T0;
        let mut i = inputs(now, T0, 50.0, [1.0, 1.0, 1.0]);
        i.hours[0].solar = [3.0, 3.0, 3.0];
        i.battery_power_w = 2000.0;
        let (with, ..) = empty_at(&battery_runtime(&i));
        let mut flat = i.clone();
        flat.hours[0].solar = [0.0; 3];
        flat.battery_power_w = 0.0;
        let (without, ..) = empty_at(&battery_runtime(&flat));
        // The hour drained 2 kWh against the profile's 1: sooner, not later.
        assert!(with < without, "{with} vs {without}");
    }

    #[test]
    fn a_car_charge_in_the_live_load_is_not_counted_twice() {
        // The dispatch overlay already adds the car for this hour.
        let now = T0 + 1800;
        let mut i = inputs(now, T0, 50.0, [1.0, 1.0, 1.0]);
        i.dispatch_hours = vec![DispatchHour {
            timestamp: T0,
            ev_kwh: 3.0,
            held_fraction: 0.0,
            charge_to_soc: None,
        }];
        let before = battery_runtime(&i);
        i.live_home_w = 9000.0;
        assert_eq!(battery_runtime(&i), before);
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
    fn a_point_under_a_low_reserve_is_below_it() {
        // 3% against a 4% reserve reads "Below Reserve"; 4% reads "At Reserve".
        let mut under = inputs(T0, T0, 3.0, [1.0, 1.0, 1.0]);
        under.params.reserve_soc_pct = 4.0;
        assert_eq!(battery_runtime(&under), RuntimeOutcome::BelowReserve);
        let mut at = inputs(T0, T0, 4.0, [1.0, 1.0, 1.0]);
        at.params.reserve_soc_pct = 4.0;
        assert_eq!(battery_runtime(&at), RuntimeOutcome::AtReserve);
        assert_eq!(reserve_band(3.0, 4.0), -1);
        assert_eq!(reserve_band(4.0, 4.0), 0);
    }

    #[test]
    fn a_point_above_the_reserve_counts_as_the_reserve() {
        // Issue #479: a cold battery stops a point early, so 5% against a 4%
        // reserve is "At Reserve"; 6% still has a point to give; a point
        // under the real reserve is still "Below Reserve".
        let with_soc = |soc: f64| {
            let mut i = inputs(T0, T0, soc, [1.0, 1.0, 1.0]);
            i.params.reserve_soc_pct = 4.0;
            battery_runtime(&i)
        };
        assert_eq!(with_soc(3.0), RuntimeOutcome::BelowReserve);
        assert_eq!(with_soc(4.0), RuntimeOutcome::AtReserve);
        assert_eq!(with_soc(5.0), RuntimeOutcome::AtReserve);
        assert!(matches!(with_soc(6.0), RuntimeOutcome::EmptyAt { .. }));
        assert_eq!(reserve_band(3.0, 4.0), -1);
        assert_eq!(reserve_band(5.0, 4.0), 0);
        assert_eq!(reserve_band(6.0, 4.0), 1);
    }

    #[test]
    fn the_empty_time_is_when_it_reaches_the_floor_a_point_up() {
        // 10 kWh at 50% to the 10% reserve is 4 kWh; at 0.2 kWh an hour that
        // is 20 h. The floor is 11%, so 3.9 kWh: 19 h 30. (Idle, so the
        // first hour isn't drained at the live rate.)
        let mut i = inputs(T0, T0, 50.0, [0.2, 0.2, 0.2]);
        i.battery_power_w = 0.0;
        let (at, ..) = empty_at(&battery_runtime(&i));
        assert_eq!(at, T0 + 19 * HOUR_SECS + 1800);
    }

    #[test]
    fn a_reserve_of_99_is_not_read_as_a_pause() {
        // The margin caps at 100, which must not turn a real reserve of 99
        // into HEM's reserve-100 pause.
        let mut near = inputs(T0, T0, 60.0, [1.0, 1.0, 1.0]);
        near.params.reserve_soc_pct = 99.0;
        assert_eq!(battery_runtime(&near), RuntimeOutcome::BelowReserve);
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
        // A raised floor below 100 (a Saving Session) holds it where it is,
        // under the reserve the inverter sees: not a pause.
        let mut floor = inputs(T0, T0, 30.0, [1.0, 1.0, 1.0]);
        floor.params.reserve_soc_pct = 45.0;
        assert_eq!(battery_runtime(&floor), RuntimeOutcome::BelowReserve);
        // Under its reserve (an outage, standby losses), even by a point:
        // says so, not "at its reserve", and not paused.
        for soc in [9.0, 17.0, 18.0, 19.0, 19.4] {
            let mut under = inputs(T0, T0, soc, [1.0, 1.0, 1.0]);
            under.params.reserve_soc_pct = 20.0;
            assert_eq!(
                battery_runtime(&under),
                RuntimeOutcome::BelowReserve,
                "{soc}"
            );
        }
        // At it, or within the float tolerance either side: at its reserve.
        for soc in [19.5, 20.0, 20.4] {
            let mut at = inputs(T0, T0, soc, [1.0, 1.0, 1.0]);
            at.params.reserve_soc_pct = 20.0;
            assert_eq!(battery_runtime(&at), RuntimeOutcome::AtReserve, "{soc}");
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
            risk: None,
            confidence: None,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "outcome": "recharges_first", "at": 5, "by": "charge" })
        );
        let json = serde_json::to_value(RuntimeOutcome::RechargesFirst {
            at: 5,
            by: RechargeSource::Charge,
            risk: Some(3),
            confidence: Some(Confidence::Low),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "outcome": "recharges_first", "at": 5, "by": "charge", "risk": 3, "confidence": "low" })
        );
    }
}
