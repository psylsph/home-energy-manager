//! "Export finished" notification (issue #499).
//!
//! Detection reads telemetry only, so every way of starting an export counts:
//! Export Once, a Timed Export window, Force Discharge, Agile, the calibration
//! cycle, or the GivEnergy app. An export is the battery discharging at more
//! than half the inverter's AC rating while the grid takes power; it starts
//! once that has held for a minute and ends after ten minutes without it, so
//! a dip (a load spike, back-to-back slots) doesn't split one export in two.
//! The figures come from the last exporting poll, not from ten minutes later.
//!
//! A Saving Session export is announced by its own messages
//! (`inverter::export_once`), which carry the same figures, so this alert
//! stays quiet for one whose "finished" message had them. An export running
//! into or out of a session is split there, so the rest is still announced.
//! Off by default; read-only.
//!
//! The decisions are a pure state machine ([`ExportSession::step`]) driven by
//! an injected clock; the runner does the I/O and takes the sender as a
//! parameter so tests never touch the network.

use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

/// The battery must export this long before it counts as an export.
pub const START_AFTER_SECS: i64 = 60;
/// ...and stop exporting this long before the export is over.
pub const END_AFTER_SECS: i64 = 600;
/// A gap between polls longer than this and three poll intervals (or a
/// clock that steps back) ends the export at the last poll seen.
pub const MAX_GAP_SECS: i64 = 600;
/// The grid must take at least this much, so the battery covering a heavy
/// home load in Eco is not an export.
pub const EXPORT_MIN_W: f64 = 100.0;
/// A fall in the day's export counter this small is the dongle's rounding.
const COUNTER_JITTER_KWH: f64 = 0.15;
/// An export that moved the battery less than a point and the grid less than
/// this is too small to tell anyone about.
const MIN_REPORT_KWH: f64 = 0.1;

/// One poll's readings.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportSample {
    /// W, + = discharging (HEM's convention on every model).
    pub battery_w: f64,
    /// W, + = exporting.
    pub grid_w: f64,
    pub soc: u8,
    /// The inverter's day export counter.
    pub today_export_kwh: f64,
    pub battery_capacity_kwh: f64,
    /// The inverter's AC rating; 0 when unknown.
    pub max_ac_power_w: f64,
    /// The time on the inverter's clock: its counter resets by its midnight,
    /// so a reset is told apart from a corrupt read, and Saving Session
    /// windows are in its local time.
    pub local_now: NaiveDateTime,
    /// How often the poll loop reads, so slow polling isn't taken for a gap.
    pub poll_interval_secs: i64,
    /// The HEM feature driving the battery now, if any.
    pub owner: Option<String>,
    /// The Saving Session export running now, by its booking's id.
    pub saving_session: Option<String>,
}

impl ExportSample {
    pub(crate) fn exporting(&self) -> bool {
        let finite = self.battery_w.is_finite() && self.grid_w.is_finite();
        finite
            && self.max_ac_power_w > 0.0
            && self.battery_w > self.max_ac_power_w / 2.0
            && self.grid_w > EXPORT_MIN_W
    }
}

/// A finished export, as announced.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportEvent {
    pub started_at: i64,
    pub ended_at: i64,
    pub start_soc: u8,
    pub end_soc: u8,
    /// To the grid over the export (solar included); `None` when the counter
    /// went backwards other than at midnight.
    pub grid_kwh: Option<f64>,
    pub battery_capacity_kwh: f64,
    pub owner: Option<String>,
    /// The Saving Session this export was, whose own message may carry the
    /// figures.
    pub saving_session: Option<String>,
}

/// The day's export counter followed across polls, joining the two days
/// when midnight resets it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridCounter {
    /// Exported since the first exporting poll.
    pub kwh: f64,
    /// The last reading followed (the highest, so jitter is not counted).
    pub last: f64,
    /// The day of the counter being followed: the export's first day, or the
    /// day of the reset since.
    pub day: NaiveDate,
    /// The counter went backwards other than at midnight: no figure.
    pub broken: bool,
}

impl GridCounter {
    fn new(sample: &ExportSample) -> Self {
        Self {
            kwh: 0.0,
            last: sample.today_export_kwh,
            day: sample.local_now.date(),
            broken: !sample.today_export_kwh.is_finite(),
        }
    }

    fn follow(&mut self, sample: &ExportSample) {
        let now = sample.today_export_kwh;
        if self.broken || !now.is_finite() {
            self.broken = true;
            return;
        }
        if now >= self.last {
            self.kwh += now - self.last;
            self.last = now;
        } else if sample.local_now.date() > self.day {
            // Midnight on the inverter's clock: the counter started again
            // from zero.
            self.kwh += now;
            self.last = now;
            self.day = sample.local_now.date();
        } else if self.last - now >= COUNTER_JITTER_KWH {
            self.broken = true;
        }
    }

    fn figure(&self) -> Option<f64> {
        (!self.broken).then_some(self.kwh)
    }
}

/// The last exporting poll: where the figures end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    pub at: i64,
    pub soc: u8,
    pub grid_kwh: Option<f64>,
    pub battery_capacity_kwh: f64,
}

/// Exporting, for less than [`START_AFTER_SECS`] so far.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    pub since: i64,
    /// SOC of the exporting polls in its first minute (at most three).
    pub socs: Vec<u8>,
    pub grid: GridCounter,
    pub owner: Option<String>,
    pub saving_session: Option<String>,
}

/// An export that has started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenExport {
    pub started_at: i64,
    pub start_soc: u8,
    pub grid: GridCounter,
    pub last: Mark,
    pub owner: Option<String>,
    pub saving_session: Option<String>,
    /// When the battery stopped exporting, while it's open.
    pub idle_since: Option<i64>,
}

/// The state machine, persisted in `meta` so a restart mid-export resumes
/// and still announces it once.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExportSession {
    #[serde(default)]
    pub pending: Option<Pending>,
    #[serde(default)]
    pub open: Option<OpenExport>,
    #[serde(default)]
    pub last_seen: Option<i64>,
}

fn median(values: &[u8]) -> u8 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted.get(sorted.len() / 2).copied().unwrap_or(0)
}

impl ExportSession {
    /// Feed one poll at `now` (Unix seconds). Returns the export that ended,
    /// if one did and it was big enough to report.
    pub fn step(&mut self, now: i64, sample: &ExportSample) -> Vec<ExportEvent> {
        let mut events = Vec::new();
        if let Some(last) = self.last_seen {
            if now == last {
                return events;
            }
            let max_gap = MAX_GAP_SECS.max(sample.poll_interval_secs.saturating_mul(3));
            if now < last || now - last > max_gap {
                // The clock stepped back or polls stopped: what happened in
                // between is unknown, so the export ended when last seen.
                events.extend(self.close());
                self.pending = None;
            }
        }
        self.last_seen = Some(now);
        let exporting = sample.exporting();

        // A Saving Session starting or ending splits the export: the session's
        // own message covers only the session.
        if self
            .open
            .as_ref()
            .is_some_and(|open| open.saving_session != sample.saving_session)
        {
            events.extend(self.close());
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.saving_session != sample.saving_session)
        {
            self.pending = None;
        }

        if let Some(open) = self.open.as_mut() {
            open.grid.follow(sample);
            if exporting {
                open.idle_since = None;
                open.last = Mark {
                    at: now,
                    soc: sample.soc,
                    grid_kwh: open.grid.figure(),
                    battery_capacity_kwh: sample.battery_capacity_kwh,
                };
            } else {
                let idle_since = *open.idle_since.get_or_insert(now);
                if now - idle_since >= END_AFTER_SECS {
                    events.extend(self.close());
                }
            }
            return events;
        }

        if !exporting {
            self.pending = None;
            return events;
        }
        let pending = self.pending.get_or_insert_with(|| Pending {
            since: now,
            socs: Vec::new(),
            grid: GridCounter::new(sample),
            owner: None,
            saving_session: sample.saving_session.clone(),
        });
        pending.grid.follow(sample);
        if now - pending.since < START_AFTER_SECS && pending.socs.len() < 3 {
            pending.socs.push(sample.soc);
        }
        if pending.owner.is_none() {
            pending.owner = sample.owner.clone();
        }
        if now - pending.since >= START_AFTER_SECS {
            let pending = self.pending.take().expect("pending was just set");
            let start_soc = if pending.socs.len() >= 3 {
                median(&pending.socs)
            } else {
                pending.socs.first().copied().unwrap_or(sample.soc)
            };
            self.open = Some(OpenExport {
                started_at: pending.since,
                start_soc,
                last: Mark {
                    at: now,
                    soc: sample.soc,
                    grid_kwh: pending.grid.figure(),
                    battery_capacity_kwh: sample.battery_capacity_kwh,
                },
                grid: pending.grid,
                owner: pending.owner,
                saving_session: pending.saving_session,
                idle_since: None,
            });
        }
        events
    }

    /// End the open export at its last exporting poll, if it is worth telling.
    fn close(&mut self) -> Option<ExportEvent> {
        let open = self.open.take()?;
        let event = ExportEvent {
            started_at: open.started_at,
            ended_at: open.last.at,
            start_soc: open.start_soc,
            end_soc: open.last.soc,
            grid_kwh: open.last.grid_kwh,
            battery_capacity_kwh: open.last.battery_capacity_kwh,
            owner: open.owner,
            saving_session: open.saving_session,
        };
        let points = event.start_soc.saturating_sub(event.end_soc);
        let grid = event.grid_kwh.unwrap_or(0.0);
        (points >= 1 || grid >= MIN_REPORT_KWH).then_some(event)
    }
}

/// "82% → 24% (58%, about 5.4 kWh), 4.9 kWh to the grid": the figures
/// shared with the Saving Session "finished" message, each caller putting
/// "battery" in front its own way.
pub fn figures_text(
    start_soc: u8,
    end_soc: u8,
    capacity_kwh: f64,
    grid_kwh: Option<f64>,
) -> String {
    let points = start_soc.saturating_sub(end_soc);
    let energy = if capacity_kwh.is_finite() && capacity_kwh > 0.0 {
        format!(
            ", about {:.1} kWh",
            f64::from(points) / 100.0 * capacity_kwh
        )
    } else {
        String::new()
    };
    let grid = grid_kwh
        .filter(|kwh| kwh.is_finite() && *kwh >= 0.0)
        .map(|kwh| format!(", {kwh:.1} kWh to the grid"))
        .unwrap_or_default();
    format!("{start_soc}% → {end_soc}% ({points}%{energy}){grid}")
}

/// The message for a finished export.
pub fn event_message(event: &ExportEvent) -> String {
    use crate::alerts::car_charging::{clock, duration};
    let owner = event
        .owner
        .as_deref()
        .map(|o| format!(" · {o}"))
        .unwrap_or_default();
    format!(
        "🔋 Export finished {}–{} ({}): battery {}{owner}",
        clock(event.started_at),
        clock(event.ended_at),
        duration(event.ended_at - event.started_at),
        figures_text(
            event.start_soc,
            event.end_soc,
            event.battery_capacity_kwh,
            event.grid_kwh
        ),
    )
}

/// The Saving Session export running at `now` (inverter-local), by its
/// booking's id: applied, live and inside its window.
pub fn saving_session_running(
    events: &[crate::inverter::export_once::ExportOnceEvent],
    now: chrono::NaiveDateTime,
) -> Option<String> {
    events
        .iter()
        .find(|e| {
            e.running_at(now)
                && e.source
                    .starts_with(crate::inverter::export_once::SESSION_SOURCE_PREFIX)
        })
        .map(|e| e.id.clone())
}

/// Whether a Saving Session's own "finished" message has the figures, or
/// still can: it finished normally and read them in time, or it is still
/// live and not yet past the time for them. A session cancelled, edited,
/// failed or reverted late leaves the export to this alert.
fn session_reports(
    id: &str,
    events: &[crate::inverter::export_once::ExportOnceEvent],
    now: NaiveDateTime,
) -> bool {
    use crate::inverter::export_once::{ExportOnceState, FIGURES_WITHIN_MINUTES};
    let in_time = |e: &crate::inverter::export_once::ExportOnceEvent| {
        e.end_at()
            .is_some_and(|end| now - end <= chrono::Duration::minutes(FIGURES_WITHIN_MINUTES))
    };
    events.iter().any(|e| {
        e.id == id
            && ((e.state == ExportOnceState::Active && in_time(e))
                || (e.state == ExportOnceState::Done && e.figures_reported))
    })
}

/// One poll's readings for the alert, on the inverter's clock. The owner is
/// left for the caller to name, and only while exporting, since working it
/// out takes several locks.
pub(crate) fn export_sample(
    snapshot: &crate::inverter::model::InverterSnapshot,
    events: &[crate::inverter::export_once::ExportOnceEvent],
    poll_interval_secs: u64,
) -> ExportSample {
    let now = crate::inverter::export_once::snapshot_local_now(snapshot);
    ExportSample {
        battery_w: f64::from(snapshot.battery_power),
        grid_w: f64::from(snapshot.grid_power),
        soc: snapshot.soc,
        today_export_kwh: f64::from(snapshot.today_export_kwh),
        battery_capacity_kwh: f64::from(snapshot.battery_capacity_kwh),
        max_ac_power_w: f64::from(snapshot.max_ac_power_w),
        local_now: now,
        poll_interval_secs: i64::try_from(poll_interval_secs).unwrap_or(i64::MAX),
        owner: None,
        saving_session: saving_session_running(events, now),
    }
}

/// The HEM feature driving the battery now, named for the message. With
/// none, the GivEnergy app or cloud may have started the export, so no name.
pub(crate) async fn current_owner(
    state: &std::sync::Arc<crate::inverter::poll::AppState>,
    snapshot: &crate::inverter::model::InverterSnapshot,
    events: &[crate::inverter::export_once::ExportOnceEvent],
) -> Option<String> {
    let now = crate::inverter::export_once::snapshot_local_now(snapshot);
    let owner = if state.calibration_run.lock().await.is_some() {
        "Battery Calibration"
    } else if state.force_discharge_revert.lock().await.is_some() {
        "Force Discharge"
    } else if events.iter().any(|e| e.running_at(now)) {
        "Export Once"
    } else if snapshot.agile_active {
        "Agile"
    } else if timed_export_window_running(state, snapshot).await {
        "Timed Export"
    } else {
        return None;
    };
    Some(owner.to_string())
}

/// Whether one of HEM's Timed Export windows is running now: the schedule
/// merely being on doesn't make an export outside its windows HEM's.
async fn timed_export_window_running(
    state: &std::sync::Arc<crate::inverter::poll::AppState>,
    snapshot: &crate::inverter::model::InverterSnapshot,
) -> bool {
    use crate::inverter::state_machines::{
        authoritative_minute_of_day, timed_export_claims_discharge,
    };
    use chrono::Timelike;
    let config = state.timed_export_config.lock().await.clone();
    if !config.schedule_enabled {
        return false;
    }
    let machine = state.timed_export_state.lock().await.clone();
    let now = chrono::Local::now();
    let host_minute = (now.hour() * 60 + now.minute()) as u16;
    let minute = authoritative_minute_of_day(snapshot, host_minute);
    timed_export_claims_discharge(&config, &machine, snapshot, minute)
}

/// `meta` key holding the persisted [`ExportSession`].
pub const META_KEY: &str = "export_summary_session";

/// The session plus what the runner keeps around it.
pub type ExportSessionRunner = crate::alerts::session_runner::SessionRunner<ExportSession>;

impl crate::alerts::session_runner::AlertSession for ExportSession {
    const META_KEY: &'static str = META_KEY;
    const WHAT: &'static str = "Export summary";
    /// An export starting, opening, going idle or closing.
    type Shape = (bool, bool, bool);
    fn shape(&self) -> Self::Shape {
        (
            self.pending.is_some(),
            self.open.is_some(),
            self.open.as_ref().is_some_and(|o| o.idle_since.is_some()),
        )
    }
}

fn alert_on(config: &crate::settings::AlertsConfig) -> bool {
    config.enabled
        && config.export_summary_alert_enabled
        && crate::alerts::has_delivery_channel(config)
}

/// Whether the alert would send anything, so the poll loop can skip working
/// out the sample while it's off. The runner checks again.
pub(crate) async fn is_on(state: &std::sync::Arc<crate::inverter::poll::AppState>) -> bool {
    alert_on(&*state.alert_config.lock().await)
}

/// One poll's export session: feed the sample and announce an export that
/// ended through `send` (which reports whether it was delivered), unless a
/// Saving Session in `bookings` announces it with the figures. `sample` is
/// `None` while the alert is off (the caller skips building it); with the
/// alert off or no channel, nothing is sent and no state is kept.
pub(crate) async fn run_export_session<F, Fut>(
    state: &std::sync::Arc<crate::inverter::poll::AppState>,
    sample: Option<ExportSample>,
    bookings: &[crate::inverter::export_once::ExportOnceEvent],
    now_ts: i64,
    send: F,
) -> Vec<tokio::task::JoinHandle<()>>
where
    F: Fn(crate::settings::AlertsConfig, String) -> Fut,
    Fut: std::future::Future<Output = bool> + Send + 'static,
{
    let config = state.alert_config.lock().await.clone();
    let db = state.history.lock().await.clone();
    let on = alert_on(&config) && sample.is_some();
    crate::alerts::session_runner::run_session(
        &state.export_session,
        db,
        config,
        on,
        now_ts,
        |session: &mut ExportSession| {
            let Some(sample) = sample else {
                return (false, Vec::new());
            };
            let events = session.step(now_ts, &sample);
            let messages = events
                .iter()
                .filter(|e| {
                    e.saving_session
                        .as_deref()
                        .is_none_or(|id| !session_reports(id, bookings, sample.local_now))
                })
                .map(event_message)
                .collect();
            (!events.is_empty(), messages)
        },
        send,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 5 kW inverter: the threshold is 2.5 kW.
    const AC: f64 = 5_000.0;

    /// Noon on the inverter's clock, 2026-11-`d`.
    fn day(d: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, d)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    }

    fn sample(battery_w: f64, grid_w: f64, soc: u8, export: f64) -> ExportSample {
        ExportSample {
            battery_w,
            grid_w,
            soc,
            today_export_kwh: export,
            battery_capacity_kwh: 9.5,
            max_ac_power_w: AC,
            local_now: day(20),
            poll_interval_secs: 60,
            owner: None,
            saving_session: None,
        }
    }

    fn exporting(soc: u8, export: f64) -> ExportSample {
        sample(3_000.0, 2_800.0, soc, export)
    }

    fn idle(soc: u8, export: f64) -> ExportSample {
        sample(200.0, -50.0, soc, export)
    }

    /// Polls every 60 s from `from` to `to` (inclusive) with `f(t)`.
    fn feed(
        s: &mut ExportSession,
        from: i64,
        to: i64,
        f: impl Fn(i64) -> ExportSample,
    ) -> Vec<ExportEvent> {
        let mut events = Vec::new();
        let mut t = from;
        while t <= to {
            events.extend(s.step(t, &f(t)));
            t += 60;
        }
        events
    }

    /// An export that lowers SOC a point and adds 0.05 kWh each minute.
    fn falling(from_soc: u8, t: i64) -> ExportSample {
        let minutes = (t / 60) as u8;
        exporting(from_soc - minutes, 1.0 + 0.05 * f64::from(minutes))
    }

    #[test]
    fn it_starts_only_after_a_minute_of_export() {
        let mut s = ExportSession::default();
        assert!(s.step(0, &exporting(80, 1.0)).is_empty());
        assert!(s.step(59, &exporting(80, 1.0)).is_empty());
        assert!(s.open.is_none(), "59 s is not enough");
        assert!(s.step(70, &idle(80, 1.0)).is_empty());
        assert!(s.pending.is_none() && s.open.is_none());

        assert!(s.step(120, &exporting(80, 1.0)).is_empty());
        assert!(s.step(180, &exporting(80, 1.0)).is_empty());
        assert_eq!(s.open.as_ref().map(|o| o.started_at), Some(120));
    }

    #[test]
    fn a_short_export_never_announces() {
        let mut s = ExportSession::default();
        let mut events = feed(&mut s, 0, 0, |_| exporting(80, 1.0));
        events.extend(s.step(30, &exporting(78, 1.5)));
        events.extend(feed(&mut s, 60, 1200, |_| idle(78, 1.5)));
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn the_battery_covering_the_house_is_not_an_export() {
        // Eco covering a 3 kW load: the battery is over the threshold but the
        // grid is importing.
        let mut s = ExportSession::default();
        let events = feed(&mut s, 0, 1800, |_| sample(3_000.0, -40.0, 80, 1.0));
        assert!(events.is_empty());
        assert!(s.pending.is_none() && s.open.is_none());
    }

    #[test]
    fn solar_export_alone_is_not_an_export() {
        let mut s = ExportSession::default();
        let events = feed(&mut s, 0, 1800, |_| sample(0.0, 5_000.0, 100, 1.0));
        assert!(events.is_empty());
        assert!(s.open.is_none());
    }

    #[test]
    fn a_rate_at_or_under_half_the_inverter_is_not_an_export() {
        let mut s = ExportSession::default();
        assert!(feed(&mut s, 0, 1800, |_| sample(2_500.0, 2_400.0, 80, 1.0)).is_empty());
        assert!(s.open.is_none());
    }

    #[test]
    fn an_unknown_inverter_rating_never_starts() {
        let mut s = ExportSession::default();
        let events = feed(&mut s, 0, 1800, |_| ExportSample {
            max_ac_power_w: 0.0,
            ..exporting(80, 1.0)
        });
        assert!(events.is_empty());
        assert!(s.pending.is_none() && s.open.is_none());
    }

    #[test]
    fn corrupt_powers_are_not_an_export() {
        for (battery, grid) in [(f64::NAN, 2_800.0), (3_000.0, f64::INFINITY)] {
            let mut s = ExportSession::default();
            assert!(feed(&mut s, 0, 600, |_| sample(battery, grid, 80, 1.0)).is_empty());
            assert!(s.pending.is_none());
        }
    }

    #[test]
    fn nine_minutes_under_keeps_one_export_and_ten_end_it_at_the_last_exporting_poll() {
        let mut s = ExportSession::default();
        assert!(feed(&mut s, 0, 600, |t| falling(80, t)).is_empty());
        // 80 - 10 = 70 at 600 s, 1.5 kWh.
        assert!(feed(&mut s, 660, 660 + 480, |_| idle(70, 1.5)).is_empty());
        // Back to exporting within the ten minutes: the same export.
        assert!(feed(&mut s, 1200, 1500, |t| exporting(
            70 - ((t - 1140) / 60) as u8,
            1.5 + 0.05 * ((t - 1140) / 60) as f64
        ))
        .is_empty());
        // Last exporting poll at 1500: SOC 64, 1.8 kWh.
        assert!(feed(&mut s, 1560, 1560 + 540, |_| idle(60, 1.9)).is_empty());
        let events = s.step(1560 + 600, &idle(60, 1.9));
        let [event] = events.as_slice() else {
            panic!("{events:?}")
        };
        assert_eq!(
            (
                event.started_at,
                event.ended_at,
                event.start_soc,
                event.end_soc
            ),
            (0, 1500, 80, 64),
            "figures from the last exporting poll"
        );
        // 1.0 → 1.8: the 1.9 read after it isn't counted.
        assert!((event.grid_kwh.unwrap() - 0.8).abs() < 1e-9, "{event:?}");
        assert!(event.saving_session.is_none() && event.owner.is_none());
    }

    #[test]
    fn the_start_soc_is_the_median_of_the_first_three_polls() {
        let mut s = ExportSession::default();
        // A jump to 95 on the first poll, then 81, 80.
        s.step(0, &exporting(95, 1.0));
        s.step(20, &exporting(81, 1.0));
        s.step(40, &exporting(80, 1.0));
        s.step(60, &exporting(80, 1.0));
        assert_eq!(s.open.as_ref().unwrap().start_soc, 81);
    }

    #[test]
    fn a_long_gap_closes_at_the_last_poll_seen() {
        let mut s = ExportSession::default();
        feed(&mut s, 0, 600, |t| falling(80, t));
        let events = s.step(600 + 601, &exporting(50, 3.0));
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].ended_at, events[0].end_soc), (600, 70));
        assert!((events[0].grid_kwh.unwrap() - 0.5).abs() < 1e-9);
        // What follows is a new run, from scratch.
        assert_eq!(s.pending.as_ref().map(|p| p.since), Some(1201));
    }

    #[test]
    fn a_clock_stepping_back_closes_it_too() {
        let mut s = ExportSession::default();
        feed(&mut s, 0, 600, |t| falling(80, t));
        let events = s.step(300, &exporting(70, 1.5));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].ended_at, 600);
    }

    #[test]
    fn a_repeated_timestamp_changes_nothing() {
        let mut s = ExportSession::default();
        feed(&mut s, 0, 300, |t| falling(80, t));
        let before = s.clone();
        assert!(s.step(300, &idle(10, 9.0)).is_empty());
        assert_eq!(s, before);
    }

    #[test]
    fn midnight_joins_both_days_of_the_export_counter() {
        let mut s = ExportSession::default();
        // 23:50 to 00:20: 4.0 → 4.5 before midnight, then 0.0 → 1.0.
        let at = |t: i64| {
            if t < 600 {
                ExportSample {
                    local_now: day(19),
                    ..exporting(80 - (t / 60) as u8, 4.0 + 0.05 * (t / 60) as f64)
                }
            } else {
                ExportSample {
                    local_now: day(20),
                    ..exporting(80 - (t / 60) as u8, 0.05 * ((t - 600) / 60) as f64)
                }
            }
        };
        feed(&mut s, 0, 1800, at);
        let events = feed(&mut s, 1860, 1860 + 600, |_| ExportSample {
            local_now: day(20),
            ..idle(50, 1.0)
        });
        assert_eq!(events.len(), 1);
        // 0.45 before (4.0 → 4.45), 1.0 after (0.0 → 1.0).
        let grid = events[0].grid_kwh.unwrap();
        assert!((grid - 1.45).abs() < 1e-9, "{grid}");
    }

    #[test]
    fn the_counter_resetting_at_the_inverters_midnight_joins_the_two_days() {
        // The day comes from the inverter's clock, which its counter resets
        // by, so a host clock running ahead or behind can't break the join.
        let mut s = ExportSession::default();
        let at = |t: i64| {
            let local_now = if t < 780 { day(19) } else { day(20) };
            let minutes = (t / 60) as u8;
            let export = if t < 780 {
                4.0 + 0.05 * f64::from(minutes)
            } else {
                0.05 * ((t - 780) / 60) as f64
            };
            ExportSample {
                local_now,
                ..exporting(80 - minutes, export)
            }
        };
        feed(&mut s, 0, 1800, at);
        let events = feed(&mut s, 1860, 1860 + 600, |_| ExportSample {
            local_now: day(20),
            ..idle(50, 1.0)
        });
        // 4.0 → 4.6 before the reset (0..720 s), 0.0 → 0.85 after.
        let grid = events[0].grid_kwh.expect("joined, not dropped");
        assert!((grid - 1.45).abs() < 1e-9, "{grid}");
    }

    #[test]
    fn a_second_fall_on_the_new_day_is_not_another_midnight() {
        let mut s = ExportSession::default();
        let first = ExportSample {
            local_now: day(19),
            ..exporting(80, 4.0)
        };
        s.step(0, &first);
        s.step(60, &first);
        s.step(
            120,
            &ExportSample {
                local_now: day(20),
                ..exporting(79, 0.2)
            },
        );
        s.step(
            180,
            &ExportSample {
                local_now: day(20),
                ..exporting(78, 1.0)
            },
        );
        s.step(
            240,
            &ExportSample {
                local_now: day(20),
                ..exporting(77, 0.1)
            },
        );
        let events = feed(&mut s, 300, 1000, |_| ExportSample {
            local_now: day(20),
            ..idle(77, 0.2)
        });
        assert_eq!(events[0].grid_kwh, None);
    }

    #[test]
    fn a_counter_going_backwards_on_the_same_day_leaves_the_grid_figure_out() {
        let mut s = ExportSession::default();
        feed(&mut s, 0, 300, |t| falling(80, t));
        s.step(360, &exporting(74, 0.2));
        feed(&mut s, 420, 600, |t| falling(80, t));
        let events = feed(&mut s, 660, 1300, |_| idle(70, 1.5));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].grid_kwh, None);
        assert!(!event_message(&events[0]).contains("to the grid"));
    }

    #[test]
    fn jitter_in_the_counter_is_not_a_reset() {
        let mut s = ExportSession::default();
        feed(&mut s, 0, 300, |t| falling(80, t));
        // 1.25 → 1.2: dongle rounding.
        s.step(330, &exporting(75, 1.2));
        feed(&mut s, 360, 600, |t| falling(80, t));
        let events = feed(&mut s, 660, 1300, |_| idle(70, 1.5));
        assert!((events[0].grid_kwh.unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn an_export_that_barely_moved_anything_is_dropped() {
        let mut s = ExportSession::default();
        let events = feed(&mut s, 0, 300, |_| exporting(80, 1.0));
        assert!(events.is_empty());
        assert!(feed(&mut s, 360, 1000, |_| idle(80, 1.05)).is_empty());
        assert!(s.open.is_none());
    }

    #[test]
    fn an_export_next_to_a_saving_session_is_announced_on_its_own() {
        // A Timed Export or Force Discharge running into a Saving Session is
        // its own export: the session's message covers only the session.
        let session = |t| ExportSample {
            saving_session: Some("s1".into()),
            ..falling(80, t)
        };
        let mut s = ExportSession::default();
        let mut events = feed(&mut s, 0, 300, |t| falling(80, t));
        events.extend(feed(&mut s, 360, 600, session));
        events.extend(feed(&mut s, 660, 1300, |_| idle(70, 1.5)));
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0].saving_session, None);
        assert_eq!((events[0].start_soc, events[0].end_soc), (80, 75));
        assert_eq!(events[1].saving_session.as_deref(), Some("s1"));
        assert_eq!((events[1].start_soc, events[1].end_soc), (74, 70));
    }

    #[test]
    fn slow_polling_still_announces_an_export() {
        // Polling every 15 minutes: each gap is longer than ten minutes but
        // only one poll interval, so the export carries on.
        let slow = |s: ExportSample| ExportSample {
            poll_interval_secs: 900,
            ..s
        };
        let mut s = ExportSession::default();
        let mut events = s.step(0, &slow(exporting(80, 1.0)));
        events.extend(s.step(900, &slow(exporting(60, 2.0))));
        assert!(s.open.is_some());
        for t in [1800, 2700, 3600] {
            events.extend(s.step(t, &slow(idle(60, 2.0))));
        }
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!((events[0].start_soc, events[0].end_soc), (80, 60));
        // A gap of more than three polls still ends it where it was last seen.
        let mut gapped = ExportSession::default();
        gapped.step(0, &slow(exporting(80, 1.0)));
        gapped.step(900, &slow(exporting(60, 2.0)));
        let ended = gapped.step(900 + 2701, &slow(exporting(40, 3.0)));
        assert_eq!(ended.len(), 1, "{ended:?}");
        assert_eq!(ended[0].end_soc, 60);
    }

    #[test]
    fn the_owner_is_the_first_named_while_it_starts() {
        let mut s = ExportSession::default();
        s.step(0, &exporting(80, 1.0));
        s.step(
            30,
            &ExportSample {
                owner: Some("Force Discharge".into()),
                ..exporting(80, 1.0)
            },
        );
        s.step(
            60,
            &ExportSample {
                owner: Some("Timed Export".into()),
                ..exporting(79, 1.1)
            },
        );
        assert_eq!(
            s.open.as_ref().unwrap().owner.as_deref(),
            Some("Force Discharge")
        );
    }

    #[test]
    fn a_gateway_snapshot_counts_once_the_decoder_has_flipped_its_sign() {
        // The decoder negates the Gateway's battery figure, so a discharge
        // arrives positive like every other model's; a 12 kW plant needs 6 kW.
        let gateway = |battery_w: f64| ExportSample {
            max_ac_power_w: 12_000.0,
            ..sample(battery_w, 6_000.0, 80, 1.0)
        };
        let mut s = ExportSession::default();
        feed(&mut s, 0, 120, |_| gateway(7_000.0));
        assert!(s.open.is_some());
        let mut charging = ExportSession::default();
        feed(&mut charging, 0, 120, |_| gateway(-7_000.0));
        assert!(charging.pending.is_none() && charging.open.is_none());
    }

    #[test]
    fn it_resumes_from_its_saved_state() {
        let mut live = ExportSession::default();
        feed(&mut live, 0, 300, |t| falling(80, t));
        let mut restored: ExportSession =
            serde_json::from_str(&serde_json::to_string(&live).unwrap()).unwrap();
        assert_eq!(restored, live);
        let a = feed(&mut live, 360, 1000, |_| idle(75, 1.25));
        let b = feed(&mut restored, 360, 1000, |_| idle(75, 1.25));
        assert_eq!(a, b);
        assert_eq!(a.len(), 1);
        let empty: ExportSession = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, ExportSession::default());
    }

    #[test]
    fn the_message_reads_well() {
        let event = ExportEvent {
            started_at: 0,
            ended_at: 2 * 3600 + 5 * 60,
            start_soc: 82,
            end_soc: 24,
            grid_kwh: Some(4.93),
            battery_capacity_kwh: 9.5,
            owner: Some("Force Discharge".into()),
            saving_session: None,
        };
        let text = event_message(&event);
        assert!(text.starts_with("🔋 Export finished "), "{text}");
        assert!(
            text.ends_with(
                " (2 h 5 m): battery 82% → 24% (58%, about 5.5 kWh), 4.9 kWh to the grid · Force Discharge"
            ),
            "{text}"
        );
        let bare = event_message(&ExportEvent {
            battery_capacity_kwh: 0.0,
            owner: None,
            ..event
        });
        assert!(
            bare.ends_with("battery 82% → 24% (58%), 4.9 kWh to the grid"),
            "{bare}"
        );
    }

    // ---- the runner ----

    use crate::history::HistoryDb;
    use crate::inverter::poll::AppState;
    use crate::settings::AlertsConfig;
    use crate::test_util::with_isolated_config_dir_async;
    use std::sync::{Arc, Mutex};

    fn on_config() -> AlertsConfig {
        AlertsConfig {
            enabled: true,
            export_summary_alert_enabled: true,
            ntfy_topic: "hem-test".into(),
            ..Default::default()
        }
    }

    fn open_history() -> Arc<HistoryDb> {
        let path = crate::test_util::make_unique_test_dir("export-alert").join("h.db");
        Arc::new(HistoryDb::open(&path).unwrap())
    }

    async fn state_with(config: AlertsConfig, db: Option<Arc<HistoryDb>>) -> Arc<AppState> {
        let state = Arc::new(AppState::new());
        *state.alert_config.lock().await = config;
        *state.history.lock().await = db;
        state
    }

    type Sent = Arc<Mutex<Vec<String>>>;

    use crate::inverter::export_once::{ExportOnceEvent, ExportOnceState};

    fn booking(source: &str, start: &str, end: &str, state: ExportOnceState) -> ExportOnceEvent {
        ExportOnceEvent {
            id: "s1".into(),
            slot: 1,
            start: start.into(),
            end: end.into(),
            target_soc: 20,
            source: source.into(),
            state,
            original: None,
            applied: true,
            attempts: 1,
            floor_refused: false,
            reserve_attempts: 0,
            note: None,
            start_soc: None,
            start_export_kwh: None,
            figures_reported: false,
        }
    }

    async fn poll(state: &Arc<AppState>, s: ExportSample, now: i64, deliver: bool) -> Vec<String> {
        poll_with(state, s, now, deliver, &[]).await
    }

    async fn poll_with(
        state: &Arc<AppState>,
        s: ExportSample,
        now: i64,
        deliver: bool,
        bookings: &[ExportOnceEvent],
    ) -> Vec<String> {
        let sent: Sent = Arc::default();
        let log = sent.clone();
        let sample = is_on(state).await.then_some(s);
        let handles = run_export_session(state, sample, bookings, now, move |_cfg, text| {
            let log = log.clone();
            async move {
                log.lock().unwrap().push(text);
                deliver
            }
        })
        .await;
        for h in handles {
            h.await.unwrap();
        }
        let out = sent.lock().unwrap().clone();
        out
    }

    async fn run(
        state: &Arc<AppState>,
        from: i64,
        to: i64,
        f: impl Fn(i64) -> ExportSample,
        deliver: bool,
    ) -> Vec<String> {
        let mut out = Vec::new();
        let mut t = from;
        while t <= to {
            out.extend(poll(state, f(t), t, deliver).await);
            t += 60;
        }
        out
    }

    fn stored(db: &HistoryDb) -> Option<ExportSession> {
        db.get_meta_value(META_KEY)
            .and_then(|json| serde_json::from_str(&json).ok())
    }

    #[tokio::test]
    async fn it_announces_an_export_once_when_it_ends() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(on_config(), Some(open_history())).await;
            assert!(run(&state, 0, 600, |t| falling(80, t), true)
                .await
                .is_empty());
            let sent = run(&state, 660, 1400, |_| idle(70, 1.5), true).await;
            assert_eq!(sent.len(), 1, "{sent:?}");
            assert!(
                sent[0].contains(
                    "(10 m): battery 80% → 70% (10%, about 1.0 kWh), 0.5 kWh to the grid"
                ),
                "{sent:?}"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn nothing_is_sent_or_kept_while_it_is_off_or_has_no_channel() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            for cfg in [
                AlertsConfig {
                    export_summary_alert_enabled: false,
                    ..on_config()
                },
                AlertsConfig {
                    enabled: false,
                    ..on_config()
                },
                AlertsConfig {
                    ntfy_topic: String::new(),
                    ..on_config()
                },
            ] {
                let state = state_with(cfg, Some(db.clone())).await;
                assert!(run(&state, 0, 600, |t| falling(80, t), true)
                    .await
                    .is_empty());
                assert!(run(&state, 660, 1400, |_| idle(70, 1.5), true)
                    .await
                    .is_empty());
                assert_eq!(stored(&db), None);
            }
        })
        .await;
    }

    #[tokio::test]
    async fn turning_it_off_clears_the_saved_session() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            let state = state_with(on_config(), Some(db.clone())).await;
            run(&state, 0, 300, |t| falling(80, t), true).await;
            assert!(stored(&db).unwrap().open.is_some());
            state.alert_config.lock().await.export_summary_alert_enabled = false;
            poll(&state, idle(75, 1.25), 360, true).await;
            assert_eq!(stored(&db), Some(ExportSession::default()));
            // Back on: the old export is gone, so its end sends nothing.
            state.alert_config.lock().await.export_summary_alert_enabled = true;
            assert!(run(&state, 420, 1200, |_| idle(75, 1.25), true)
                .await
                .is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_restart_mid_export_announces_it_once() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            let first = state_with(on_config(), Some(db.clone())).await;
            run(&first, 0, 600, |t| falling(80, t), true).await;
            let second = state_with(on_config(), Some(db.clone())).await;
            let sent = run(&second, 660, 1400, |_| idle(70, 1.5), true).await;
            assert_eq!(sent.len(), 1, "{sent:?}");
            assert!(sent[0].contains("80% → 70%"), "{sent:?}");
            // And a third start doesn't repeat it.
            let third = state_with(on_config(), Some(db.clone())).await;
            assert!(run(&third, 1460, 2000, |_| idle(70, 1.5), true)
                .await
                .is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn it_waits_for_the_database_before_starting() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            let first = state_with(on_config(), Some(db.clone())).await;
            run(&first, 0, 300, |t| falling(80, t), true).await;
            let restarted = state_with(on_config(), None).await;
            run(&restarted, 360, 600, |t| falling(80, t), true).await;
            assert_eq!(
                restarted.export_session.lock().await.session,
                ExportSession::default()
            );
            *restarted.history.lock().await = Some(db.clone());
            run(&restarted, 660, 900, |t| falling(80, t), true).await;
            assert_eq!(stored(&db).unwrap().open.unwrap().started_at, 0);
        })
        .await;
    }

    #[tokio::test]
    async fn an_undelivered_message_is_resent_and_not_twice() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(on_config(), Some(open_history())).await;
            run(&state, 0, 600, |t| falling(80, t), true).await;
            // Ends at 660 + 600 = 1260; the channel refuses it.
            let refused = run(&state, 660, 1260, |_| idle(70, 1.5), false).await;
            assert_eq!(refused.len(), 1, "tried once: {refused:?}");
            // Not again before five minutes.
            assert!(run(&state, 1320, 1500, |_| idle(70, 1.5), true)
                .await
                .is_empty());
            let resent = poll(&state, idle(70, 1.5), 1260 + 301, true).await;
            assert_eq!(resent.len(), 1, "{resent:?}");
            assert!(resent[0].starts_with("🔋 Export finished"));
            assert!(run(&state, 1620, 2400, |_| idle(70, 1.5), true)
                .await
                .is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_saving_session_export_is_left_to_its_own_message_only_when_that_has_the_figures() {
        // The session's "finished" message carries the figures only when it
        // finished normally and read them in time; otherwise this alert does.
        let session = |state, figures_reported| ExportOnceEvent {
            figures_reported,
            ..booking(
                "saving_session:9",
                "2026-11-20 17:00",
                "2026-11-20 18:00",
                state,
            )
        };
        let cases = [
            (session(ExportOnceState::Active, false), false),
            (session(ExportOnceState::Done, true), false),
            (session(ExportOnceState::Done, false), true),
            (session(ExportOnceState::Cancelled, false), true),
            (session(ExportOnceState::Failed, false), true),
            // Still live a day after its end (HEM closed through the revert):
            // its message won't have the figures, so this alert does.
            (
                ExportOnceEvent {
                    start: "2026-11-19 17:00".into(),
                    end: "2026-11-19 18:00".into(),
                    ..session(ExportOnceState::Active, false)
                },
                true,
            ),
        ];
        for (booked, announced) in cases {
            with_isolated_config_dir_async(|| async {
                let state = state_with(on_config(), Some(open_history())).await;
                let bookings = std::slice::from_ref(&booked);
                let mut sent = Vec::new();
                let mut t = 0;
                while t <= 1400 {
                    let sample = if t <= 600 {
                        ExportSample {
                            saving_session: Some("s1".into()),
                            ..falling(80, t)
                        }
                    } else {
                        idle(70, 1.5)
                    };
                    sent.extend(poll_with(&state, sample, t, true, bookings).await);
                    t += 60;
                }
                assert_eq!(sent.len(), usize::from(announced), "{booked:?}: {sent:?}");
            })
            .await;
        }
    }

    #[tokio::test]
    async fn the_owner_names_only_what_is_driving_the_battery_now() {
        use crate::inverter::model::InverterSnapshot;
        with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let snapshot = InverterSnapshot {
                inverter_time: "2026-11-20 12:00:00".into(),
                ..Default::default()
            };
            assert_eq!(current_owner(&state, &snapshot, &[]).await, None);

            // The Timed Export schedule on, but no window now: the GivEnergy
            // app may have started this export, so no name.
            state.timed_export_config.lock().await.schedule_enabled = true;
            assert_eq!(current_owner(&state, &snapshot, &[]).await, None);

            let agile = InverterSnapshot {
                agile_active: true,
                ..snapshot.clone()
            };
            assert_eq!(
                current_owner(&state, &agile, &[]).await.as_deref(),
                Some("Agile")
            );

            let booked = booking(
                "manual",
                "2026-11-20 11:00",
                "2026-11-20 13:00",
                ExportOnceState::Active,
            );
            assert_eq!(
                current_owner(&state, &agile, std::slice::from_ref(&booked))
                    .await
                    .as_deref(),
                Some("Export Once")
            );
            // Applied in its lead time, before the window opens: not yet HEM's.
            let ahead = booking(
                "manual",
                "2026-11-20 12:05",
                "2026-11-20 13:00",
                ExportOnceState::Active,
            );
            assert_eq!(current_owner(&state, &snapshot, &[ahead]).await, None);
            let unapplied = ExportOnceEvent {
                applied: false,
                ..booked
            };
            assert_eq!(current_owner(&state, &snapshot, &[unapplied]).await, None);
        })
        .await;
    }

    #[test]
    fn a_saving_session_is_running_only_while_applied_active_and_inside_its_window() {
        let at = |hhmm: &str| {
            crate::inverter::export_once::parse_local(&format!("2026-11-20 {hhmm}")).unwrap()
        };
        let event = |source: &str, state, applied| ExportOnceEvent {
            applied,
            ..booking(source, "2026-11-20 17:00", "2026-11-20 18:00", state)
        };
        let running = event("saving_session:9", ExportOnceState::Active, true);
        assert_eq!(
            saving_session_running(std::slice::from_ref(&running), at("17:30")).as_deref(),
            Some("s1")
        );
        // Applied in the lead time, and once the window has ended.
        assert_eq!(
            saving_session_running(std::slice::from_ref(&running), at("16:55")),
            None
        );
        assert_eq!(
            saving_session_running(std::slice::from_ref(&running), at("18:00")),
            None
        );
        for other in [
            event("saving_session:9", ExportOnceState::Active, false),
            event("saving_session:9", ExportOnceState::Done, true),
            event("manual", ExportOnceState::Active, true),
        ] {
            assert_eq!(saving_session_running(&[other], at("17:30")), None);
        }
    }

    #[test]
    fn the_sample_reads_the_inverters_clock() {
        // The day's export counter resets on the inverter's midnight, so the
        // day comes from its clock, not the host's.
        use crate::inverter::model::InverterSnapshot;
        let snapshot = InverterSnapshot {
            inverter_time: "2026-11-21 00:02:00".into(),
            battery_power: 3_000,
            grid_power: 2_800,
            soc: 70,
            today_export_kwh: 0.1,
            battery_capacity_kwh: 9.5,
            max_ac_power_w: 5_000,
            ..Default::default()
        };
        let session = booking(
            "saving_session:9",
            "2026-11-21 00:00",
            "2026-11-21 01:00",
            ExportOnceState::Active,
        );
        let sample = export_sample(&snapshot, std::slice::from_ref(&session), 900);
        assert_eq!(
            sample.local_now,
            NaiveDate::from_ymd_opt(2026, 11, 21)
                .unwrap()
                .and_hms_opt(0, 2, 0)
                .unwrap()
        );
        assert_eq!(sample.poll_interval_secs, 900);
        assert_eq!(sample.saving_session.as_deref(), Some("s1"));
        assert!(sample.exporting() && sample.owner.is_none());
        assert_eq!(
            (sample.soc, sample.today_export_kwh),
            (70, f64::from(0.1_f32))
        );
    }
}
