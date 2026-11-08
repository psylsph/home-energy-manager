//! "Car charging started / finished" notification (issue #454).
//!
//! A session starts once the car has drawn power for three minutes running
//! and ends once it has drawn none for ten, so a pause between Octopus slots
//! or a taper under the threshold neither ends it nor starts a second one.
//! The car's power is the polled GivEnergy charger's, else the estimate from
//! home load above the user's threshold (`history::car_charging`), which must
//! be set for the alert to do anything. Off by default; read-only.
//!
//! The decisions are a pure state machine ([`CarSession::step`]) driven by
//! an injected clock; the runner does the I/O and takes the sender as a
//! parameter so tests never touch the network.

use serde::{Deserialize, Serialize};

/// The car must draw power this long before a session starts.
pub const START_AFTER_SECS: i64 = 180;
/// ...and draw none this long before it ends.
pub const END_AFTER_SECS: i64 = 600;
/// A gap between polls longer than this (or a clock that steps back) ends
/// the session at the last poll seen, without claiming the missing energy.
pub const MAX_GAP_SECS: i64 = 600;
/// At most one "started" message in this long, so a charger that keeps
/// stopping and starting doesn't send one each time.
pub const START_MESSAGE_MIN_GAP_SECS: i64 = 1800;

/// What a step tells the runner to announce.
#[derive(Debug, Clone, PartialEq)]
pub enum CarEvent {
    /// A session began at `at`.
    Started { at: i64 },
    /// A session has ended.
    Finished {
        started_at: i64,
        ended_at: i64,
        kwh: f64,
    },
}

/// A session that has started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenSession {
    pub started_at: i64,
    pub kwh: f64,
}

/// The state machine, persisted in `meta` so a restart mid-charge resumes
/// instead of announcing again.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CarSession {
    /// When the current run of car power began, before it counts as a session.
    #[serde(default)]
    pub pending_since: Option<i64>,
    #[serde(default)]
    pub pending_kwh: f64,
    #[serde(default)]
    pub open: Option<OpenSession>,
    /// When the car stopped drawing power while a session is open.
    #[serde(default)]
    pub idle_since: Option<i64>,
    #[serde(default)]
    pub last_seen: Option<i64>,
    #[serde(default)]
    pub last_power_w: f64,
    #[serde(default)]
    pub last_started_notified: Option<i64>,
}

impl CarSession {
    /// Feed one poll: `car_w` is the car's power at `now` (Unix seconds).
    ///
    /// Energy is the previous poll's power held until this one, so a gap is
    /// never bridged: a long one ends the session first. A reading that is
    /// not a finite non-negative number counts as no power.
    pub fn step(&mut self, now: i64, car_w: f64) -> Vec<CarEvent> {
        let car_w = if car_w.is_finite() && car_w > 0.0 {
            car_w
        } else {
            0.0
        };
        let mut events = Vec::new();

        if let Some(last) = self.last_seen {
            if now == last {
                return events;
            }
            if now < last || now - last > MAX_GAP_SECS {
                // The clock stepped back or polls stopped: the session ended
                // when we last saw it, and what happened since is unknown.
                if let Some(event) = self.close(last) {
                    events.push(event);
                }
                self.pending_since = None;
                self.pending_kwh = 0.0;
                self.idle_since = None;
                self.last_seen = None;
                self.last_power_w = 0.0;
            }
        }

        // The previous poll's power, held over the time since it.
        let credited = self.last_seen.map_or(0.0, |last| {
            self.last_power_w * (now - last) as f64 / 3_600_000.0
        });

        if car_w > 0.0 {
            self.idle_since = None;
            if let Some(open) = self.open.as_mut() {
                open.kwh += credited;
            } else {
                let since = *self.pending_since.get_or_insert(now);
                self.pending_kwh += credited;
                if now - since >= START_AFTER_SECS {
                    // Only the "started" message is rate-limited; the
                    // "finished" energy summary always goes out.
                    let announce = self
                        .last_started_notified
                        .is_none_or(|at| now < at || now - at >= START_MESSAGE_MIN_GAP_SECS);
                    self.open = Some(OpenSession {
                        started_at: since,
                        kwh: self.pending_kwh,
                    });
                    self.pending_since = None;
                    self.pending_kwh = 0.0;
                    if announce {
                        self.last_started_notified = Some(now);
                        events.push(CarEvent::Started { at: since });
                    }
                }
            }
        } else {
            self.pending_since = None;
            self.pending_kwh = 0.0;
            if let Some(open) = self.open.as_mut() {
                open.kwh += credited;
                let idle_since = *self.idle_since.get_or_insert(now);
                if now - idle_since >= END_AFTER_SECS {
                    if let Some(event) = self.close(idle_since) {
                        events.push(event);
                    }
                    self.idle_since = None;
                }
            }
        }

        self.last_seen = Some(now);
        self.last_power_w = car_w;
        events
    }

    /// End the open session at `ended_at`, if there is one.
    fn close(&mut self, ended_at: i64) -> Option<CarEvent> {
        let open = self.open.take()?;
        self.idle_since = None;
        Some(CarEvent::Finished {
            started_at: open.started_at,
            ended_at,
            kwh: open.kwh,
        })
    }
}

/// `meta` key holding the persisted [`CarSession`].
pub const META_KEY: &str = "car_charging_alert";
/// How often the session's progress is saved when nothing else changed, so a
/// restart resumes close to where it was without a write every poll.
const PERSIST_EVERY_SECS: i64 = 60;
/// A polled charger drawing no more than this is idle (it reads its own
/// standby draw), so a charger that never quite reads zero still ends.
pub const MEASURED_FLOOR_W: f64 = 100.0;

/// The car's power for this poll: a polled charger's own figure when HEM
/// reads one, else the estimate from home load above the threshold.
pub fn car_input_w(
    cfg: &crate::history::car_charging::CarChargingConfig,
    home_w: f64,
    grid_v: f64,
    polled_charger_w: Option<i32>,
) -> f64 {
    match polled_charger_w {
        Some(w) if f64::from(w) > MEASURED_FLOOR_W => f64::from(w),
        Some(_) => 0.0,
        None => crate::history::car_charging::car_power_w(home_w, Some(grid_v), cfg),
    }
}

/// The session plus what the runner keeps around it.
#[derive(Debug, Default)]
pub struct CarSessionRunner {
    loaded: bool,
    session: CarSession,
    persisted_at: Option<i64>,
    outbox: crate::alerts::battery_health::SocOutbox,
}

fn alert_on(config: &crate::settings::AlertsConfig) -> bool {
    config.enabled
        && config.car_charging_alert_enabled
        && crate::alerts::has_delivery_channel(config)
}

fn clock(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

fn duration(secs: i64) -> String {
    let minutes = (secs.max(0) + 30) / 60;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} m"),
        (h, m) => format!("{h} h {m} m"),
    }
}

/// The message for an event. `estimated` says the power was worked out from
/// the home load rather than read from a charger.
pub fn event_message(event: &CarEvent, estimated: bool) -> String {
    let note = if estimated { " (estimated)" } else { "" };
    match event {
        CarEvent::Started { at } => format!("🚗 Car charging started at {}{note}", clock(*at)),
        CarEvent::Finished {
            started_at,
            ended_at,
            kwh,
        } => format!(
            "🚗 Car charging finished: {kwh:.1} kWh in {} ({}–{}){note}",
            duration(ended_at - started_at),
            clock(*started_at),
            clock(*ended_at),
        ),
    }
}

/// One poll's car session: feed the car's power, announce what it confirms
/// through `send` (which reports whether the message was delivered), keep an
/// undelivered one for a resend, and save the session so a restart resumes.
///
/// `car_w` is `None` when the feature has nothing to go on (no car-charging
/// threshold set; a polled charger only replaces the estimate, it does not
/// switch the alert on by itself); with the alert off, no channel left or no input,
/// nothing is sent and no state is kept. The config is read once, so a
/// concurrent settings save cannot split one poll across two states.
pub(crate) async fn run_car_session<F, Fut>(
    state: &std::sync::Arc<crate::inverter::poll::AppState>,
    car_w: Option<f64>,
    estimated: bool,
    now_ts: i64,
    send: F,
) -> Vec<tokio::task::JoinHandle<()>>
where
    F: Fn(crate::settings::AlertsConfig, String) -> Fut,
    Fut: std::future::Future<Output = bool> + Send + 'static,
{
    let config = state.alert_config.lock().await.clone();
    let db = state.history.lock().await.clone();
    let on = alert_on(&config) && car_w.is_some();

    let (messages, resend, to_save) = {
        let mut runner = state.car_session.lock().await;
        if on && !runner.loaded && db.is_none() {
            // The saved session can't be read yet, so don't start a new one
            // over it: wait for the history database.
            return Vec::new();
        }
        if !runner.loaded {
            runner.loaded = true;
            if let Some(saved) = db
                .as_ref()
                .and_then(|db| db.get_meta_value(META_KEY))
                .and_then(|json| serde_json::from_str::<CarSession>(&json).ok())
            {
                runner.session = saved;
            }
        }
        if !on {
            // Switched off, or nothing to go on: drop the session, the held
            // messages and the saved copy.
            let had_state =
                runner.session != CarSession::default() || runner.persisted_at.is_some();
            runner.session = CarSession::default();
            runner.outbox = Default::default();
            runner.persisted_at = None;
            let clear = had_state
                .then(|| serde_json::to_string(&CarSession::default()).ok())
                .flatten();
            drop(runner);
            if let (Some(db), Some(json)) = (db, clear) {
                let _ =
                    tokio::task::spawn_blocking(move || db.set_meta_value(META_KEY, &json)).await;
            }
            return Vec::new();
        }
        let before = runner.session.clone();
        let events = runner.session.step(now_ts, car_w.unwrap_or(0.0));
        let resend = runner.outbox.take_due(now_ts);
        // Save on a change of shape (a session opening or closing, a run
        // starting or ending) and otherwise every minute.
        let shape = |s: &CarSession| {
            (
                s.pending_since.is_some(),
                s.open.is_some(),
                s.idle_since.is_some(),
            )
        };
        let due = runner
            .persisted_at
            .is_none_or(|at| now_ts < at || now_ts - at >= PERSIST_EVERY_SECS);
        let save = !events.is_empty() || shape(&before) != shape(&runner.session) || due;
        let to_save = save.then(|| {
            runner.persisted_at = Some(now_ts);
            serde_json::to_string(&runner.session).ok()
        });
        let messages: Vec<String> = events.iter().map(|e| event_message(e, estimated)).collect();
        (messages, resend, to_save.flatten())
    };

    if let (Some(db), Some(json)) = (db, to_save) {
        let _ = tokio::task::spawn_blocking(move || db.set_meta_value(META_KEY, &json)).await;
    }

    // Each message goes out on its own task; one that isn't delivered is held
    // for a resend, carrying when it first failed.
    let mut handles = Vec::new();
    let mut deliver = |texts: Vec<(i64, String)>| {
        let text = texts
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let delivery = send(config.clone(), text);
        let state = state.clone();
        handles.push(tokio::spawn(async move {
            if !delivery.await {
                tracing::warn!("Car charging message not delivered, will resend");
                state
                    .car_session
                    .lock()
                    .await
                    .outbox
                    .add_failed(texts, now_ts);
            }
        }));
    };
    if !resend.is_empty() {
        deliver(resend);
    }
    for message in messages {
        deliver(vec![(now_ts, message)]);
    }
    handles
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f64 = 7_360.0;

    /// Feed a power every 60 s from `from` to `to` (inclusive), collecting events.
    fn feed(s: &mut CarSession, from: i64, to: i64, power: f64) -> Vec<CarEvent> {
        let mut events = Vec::new();
        let mut t = from;
        while t <= to {
            events.extend(s.step(t, power));
            t += 60;
        }
        events
    }

    #[test]
    fn a_session_starts_after_three_minutes_and_not_two() {
        let mut s = CarSession::default();
        assert!(
            feed(&mut s, 0, 120, W).is_empty(),
            "two minutes is not enough"
        );
        assert_eq!(s.step(179, W), vec![]);
        assert_eq!(s.step(180, W), vec![CarEvent::Started { at: 0 }]);
        // Nothing more while it carries on.
        assert!(feed(&mut s, 240, 1200, W).is_empty());
    }

    #[test]
    fn a_short_run_is_forgotten() {
        let mut s = CarSession::default();
        assert!(feed(&mut s, 0, 120, W).is_empty());
        assert!(s.step(180, 0.0).is_empty());
        // The clock starts again: three more minutes are needed.
        assert!(feed(&mut s, 240, 360, W).is_empty());
        assert_eq!(s.step(420, W), vec![CarEvent::Started { at: 240 }]);
    }

    #[test]
    fn nine_idle_minutes_keep_the_session_and_ten_end_it() {
        let mut s = CarSession::default();
        let started = feed(&mut s, 0, 240, W);
        assert_eq!(started, vec![CarEvent::Started { at: 0 }]);
        // Idle from 300 (the first zero reading).
        assert!(feed(&mut s, 300, 300 + 540, 0.0).is_empty(), "nine minutes");
        let events = s.step(300 + 600, 0.0);
        assert_eq!(events.len(), 1);
        match &events[0] {
            CarEvent::Finished {
                started_at,
                ended_at,
                ..
            } => {
                assert_eq!(*started_at, 0);
                assert_eq!(*ended_at, 300, "it ended when the power stopped");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_finished_energy_is_the_integral_of_what_was_drawn() {
        let mut s = CarSession::default();
        feed(&mut s, 0, 240, W);
        let events = feed(&mut s, 300, 300 + 600, 0.0);
        let CarEvent::Finished { kwh, .. } = events.last().unwrap() else {
            panic!()
        };
        // 7.36 kW from 0 to 300 s.
        assert!((kwh - 7.36 * 300.0 / 3600.0).abs() < 1e-9, "{kwh}");
    }

    #[test]
    fn a_pause_and_a_restart_is_one_session() {
        let mut s = CarSession::default();
        assert_eq!(feed(&mut s, 0, 240, W), vec![CarEvent::Started { at: 0 }]);
        assert!(
            feed(&mut s, 300, 300 + 300, 0.0).is_empty(),
            "five idle minutes"
        );
        let resumed = feed(&mut s, 660, 900, W);
        assert!(resumed.is_empty(), "no second start");
        let events = feed(&mut s, 960, 960 + 600, 0.0);
        let CarEvent::Finished {
            started_at, kwh, ..
        } = events.last().unwrap()
        else {
            panic!()
        };
        assert_eq!(*started_at, 0);
        // 0..300 and 660..960 at 7.36 kW; the idle minutes between add nothing.
        assert!(
            (kwh - 7.36 * (300.0 + 300.0) / 3600.0).abs() < 1e-9,
            "{kwh}"
        );
    }

    #[test]
    fn a_long_gap_ends_the_session_at_the_last_poll_without_the_missing_energy() {
        let mut s = CarSession::default();
        feed(&mut s, 0, 240, W);
        let events = s.step(240 + 601, W);
        let [CarEvent::Finished {
            started_at,
            ended_at,
            kwh,
        }] = events.as_slice()
        else {
            panic!("{events:?}")
        };
        assert_eq!((*started_at, *ended_at), (0, 240));
        assert!(
            (kwh - 7.36 * 240.0 / 3600.0).abs() < 1e-9,
            "no energy for the gap: {kwh}"
        );
        // The new reading begins a fresh run; no energy claimed for the gap.
        assert_eq!(s.pending_since, Some(240 + 601));
        assert_eq!(s.pending_kwh, 0.0);
    }

    #[test]
    fn a_gap_of_exactly_ten_minutes_is_still_a_continuing_session() {
        let mut s = CarSession::default();
        feed(&mut s, 0, 240, W);
        assert!(s.step(240 + 600, W).is_empty());
    }

    #[test]
    fn a_clock_that_steps_back_ends_the_session_too() {
        let mut s = CarSession::default();
        feed(&mut s, 0, 240, W);
        let events = s.step(100, W);
        assert!(matches!(
            events.as_slice(),
            [CarEvent::Finished { ended_at: 240, .. }]
        ));
    }

    #[test]
    fn a_repeated_timestamp_changes_nothing() {
        let mut s = CarSession::default();
        feed(&mut s, 0, 240, W);
        let before = s.clone();
        assert!(s.step(240, W).is_empty());
        assert_eq!(s, before);
    }

    #[test]
    fn a_second_session_inside_half_an_hour_of_the_last_start_skips_only_the_start_message() {
        let mut s = CarSession::default();
        assert_eq!(feed(&mut s, 0, 240, W), vec![CarEvent::Started { at: 0 }]);
        // Ends at 300 (10 idle minutes seen by 900).
        let events = feed(&mut s, 300, 900, 0.0);
        assert!(matches!(events.last(), Some(CarEvent::Finished { .. })));
        // Charging again from 1000: opens at 1180, only 1180 s after the first start message.
        let events = feed(&mut s, 1000, 1300, W);
        assert!(events.is_empty(), "no second start message: {events:?}");
        // Its end is still announced: the rate limit is for "started"
        // messages, and the energy summary is what the user is waiting for.
        let events = feed(&mut s, 1360, 1360 + 700, 0.0);
        assert!(
            matches!(
                events.as_slice(),
                [CarEvent::Finished {
                    started_at: 1000,
                    ..
                }]
            ),
            "finish still sent: {events:?}"
        );
    }

    #[test]
    fn a_session_half_an_hour_after_the_last_start_is_announced_again() {
        let mut s = CarSession::default();
        feed(&mut s, 0, 240, W);
        feed(&mut s, 300, 900, 0.0);
        let events = feed(&mut s, 2000, 2200, W);
        assert_eq!(events, vec![CarEvent::Started { at: 2000 }]);
    }

    #[test]
    fn a_stopped_session_resumes_from_its_saved_state() {
        let mut live = CarSession::default();
        feed(&mut live, 0, 540, W);
        let saved = serde_json::to_string(&live).unwrap();
        let mut restored: CarSession = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored, live);
        let a = feed(&mut live, 600, 600 + 900, 0.0);
        let b = feed(&mut restored, 600, 600 + 900, 0.0);
        assert_eq!(a, b);
        assert!(matches!(a.last(), Some(CarEvent::Finished { .. })));
    }

    #[test]
    fn a_state_from_an_older_file_with_missing_fields_loads() {
        let s: CarSession = serde_json::from_str("{}").unwrap();
        assert_eq!(s, CarSession::default());
    }

    #[test]
    fn corrupt_power_counts_as_none() {
        for bad in [f64::NAN, f64::INFINITY, -50.0] {
            let mut s = CarSession::default();
            assert!(feed(&mut s, 0, 600, bad).is_empty());
            assert_eq!(s.pending_since, None);
        }
    }

    // ---- the runner ----

    use crate::history::car_charging::CarChargingConfig;
    use crate::history::HistoryDb;
    use crate::inverter::poll::AppState;
    use crate::settings::AlertsConfig;
    use crate::test_util::with_isolated_config_dir_async;
    use std::sync::{Arc, Mutex};

    fn on_config() -> AlertsConfig {
        AlertsConfig {
            enabled: true,
            car_charging_alert_enabled: true,
            ntfy_topic: "hem-test".into(),
            ..Default::default()
        }
    }

    fn open_history() -> Arc<HistoryDb> {
        let path = crate::test_util::make_unique_test_dir("car-alert").join("h.db");
        Arc::new(HistoryDb::open(&path).unwrap())
    }

    async fn state_with(config: AlertsConfig, db: Option<Arc<HistoryDb>>) -> Arc<AppState> {
        let state = Arc::new(AppState::new());
        *state.alert_config.lock().await = config;
        *state.history.lock().await = db;
        state
    }

    type Sent = Arc<Mutex<Vec<String>>>;

    /// One poll at `now`, collecting what was sent; `deliver` says whether
    /// the (fake) channel accepts it.
    async fn poll(state: &Arc<AppState>, car_w: Option<f64>, now: i64, deliver: bool) -> Sent {
        let sent: Sent = Arc::default();
        let log = sent.clone();
        let handles = run_car_session(state, car_w, true, now, move |_cfg, text| {
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
        sent
    }

    /// Polls every 60 s from `from` to `to`, returning every message sent.
    async fn run_minutes(state: &Arc<AppState>, from: i64, to: i64, power: f64) -> Vec<String> {
        let mut out = Vec::new();
        let mut t = from;
        while t <= to {
            out.extend(
                poll(state, Some(power), t, true)
                    .await
                    .lock()
                    .unwrap()
                    .clone(),
            );
            t += 60;
        }
        out
    }

    fn stored(db: &HistoryDb) -> Option<CarSession> {
        db.get_meta_value(META_KEY)
            .and_then(|json| serde_json::from_str(&json).ok())
    }

    #[tokio::test]
    async fn it_announces_the_start_once_and_the_finish_with_the_energy() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            let state = state_with(on_config(), Some(db)).await;
            let started = run_minutes(&state, 0, 600, W).await;
            assert_eq!(started.len(), 1, "{started:?}");
            assert!(
                started[0].starts_with("🚗 Car charging started at"),
                "{started:?}"
            );
            assert!(started[0].ends_with("(estimated)"));
            let finished = run_minutes(&state, 660, 660 + 700, 0.0).await;
            assert_eq!(finished.len(), 1, "{finished:?}");
            // 7.36 kW from 0 to 660 s.
            assert!(
                finished[0].contains("finished: 1.3 kWh in 11 m"),
                "{finished:?}"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn nothing_is_sent_or_kept_while_the_alert_is_off() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            let off = AlertsConfig {
                car_charging_alert_enabled: false,
                ..on_config()
            };
            let state = state_with(off, Some(db.clone())).await;
            assert!(run_minutes(&state, 0, 900, W).await.is_empty());
            assert_eq!(stored(&db), None);

            // Master switch off, or nowhere to send, is the same.
            for cfg in [
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
                assert!(run_minutes(&state, 0, 900, W).await.is_empty());
                assert_eq!(stored(&db), None);
            }
        })
        .await;
    }

    #[tokio::test]
    async fn nothing_is_sent_without_an_input() {
        // No threshold set: the caller passes no power.
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            let state = state_with(on_config(), Some(db.clone())).await;
            for t in (0..900).step_by(60) {
                assert!(poll(&state, None, t, true).await.lock().unwrap().is_empty());
            }
            assert_eq!(stored(&db), None);
        })
        .await;
    }

    #[tokio::test]
    async fn turning_it_off_drops_the_session_and_on_again_starts_fresh() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            let state = state_with(on_config(), Some(db.clone())).await;
            assert_eq!(run_minutes(&state, 0, 300, W).await.len(), 1);
            assert!(stored(&db).unwrap().open.is_some());

            *state.alert_config.lock().await = AlertsConfig {
                car_charging_alert_enabled: false,
                ..on_config()
            };
            assert!(run_minutes(&state, 360, 420, W).await.is_empty());
            assert_eq!(stored(&db), Some(CarSession::default()), "nothing kept");

            *state.alert_config.lock().await = on_config();
            // The session is new: three minutes must pass again, and as it is
            // inside half an hour of the last start message, stays quiet.
            assert!(poll(&state, Some(W), 480, true)
                .await
                .lock()
                .unwrap()
                .is_empty());
            assert!(stored(&db).unwrap().open.is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn an_undelivered_message_is_resent_every_five_minutes_and_not_twice() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            let state = state_with(on_config(), Some(db)).await;
            // The start is due at 180 s but the channel refuses it.
            for t in (0..180).step_by(60) {
                poll(&state, Some(W), t, false).await;
            }
            let first = poll(&state, Some(W), 180, false).await;
            assert_eq!(first.lock().unwrap().len(), 1, "tried once");
            // Before five minutes: not retried.
            let soon = poll(&state, Some(W), 240, true).await;
            assert!(soon.lock().unwrap().is_empty());
            // After five minutes: resent, and this time it is delivered.
            let again = poll(&state, Some(W), 180 + 301, true).await;
            let texts = again.lock().unwrap().clone();
            assert_eq!(texts.len(), 1, "{texts:?}");
            assert!(texts[0].starts_with("🚗 Car charging started"));
            // Delivered, so it is not sent a third time.
            let later = poll(&state, Some(W), 180 + 700, true).await;
            assert!(later.lock().unwrap().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_message_past_its_hour_is_given_up() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            let state = state_with(on_config(), Some(db)).await;
            for t in (0..=180).step_by(60) {
                poll(&state, Some(W), t, false).await;
            }
            // Over an hour later (a continuing session, polls every ten minutes).
            let mut t = 180;
            let mut last = Vec::new();
            while t < 180 + 3700 {
                t += 600;
                last = poll(&state, Some(W), t, false)
                    .await
                    .lock()
                    .unwrap()
                    .clone();
            }
            assert!(last.is_empty(), "given up after an hour: {last:?}");
        })
        .await;
    }

    #[tokio::test]
    async fn a_restart_mid_charge_resumes_without_announcing_again() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            let first = state_with(on_config(), Some(db.clone())).await;
            assert_eq!(run_minutes(&first, 0, 600, W).await.len(), 1);

            // A new process: fresh state, same database.
            let second = state_with(on_config(), Some(db.clone())).await;
            let more = run_minutes(&second, 660, 900, W).await;
            assert!(more.is_empty(), "no second start message: {more:?}");
            let finished = run_minutes(&second, 960, 960 + 700, 0.0).await;
            assert_eq!(finished.len(), 1);
            assert!(finished[0].contains("finished"), "{finished:?}");
        })
        .await;
    }

    #[tokio::test]
    async fn a_restart_after_a_long_gap_ends_the_session_where_it_was_last_seen() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            let first = state_with(on_config(), Some(db.clone())).await;
            run_minutes(&first, 0, 600, W).await;
            let saved = stored(&db).unwrap();
            assert!(saved.open.is_some());

            // HEM was closed for hours.
            let second = state_with(on_config(), Some(db.clone())).await;
            let sent = poll(&second, Some(0.0), 600 + 5 * 3600, true).await;
            let texts = sent.lock().unwrap().clone();
            assert_eq!(texts.len(), 1, "{texts:?}");
            assert!(texts[0].contains("finished"), "{texts:?}");
            // It claims the session up to the last poll seen, not the gap.
            assert!(
                texts[0].contains("1.2 kWh") || texts[0].contains("1.3 kWh"),
                "{texts:?}"
            );
            assert!(stored(&db).unwrap().open.is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn a_corrupt_saved_state_starts_fresh() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            db.set_meta_value(META_KEY, "not json").unwrap();
            let state = state_with(on_config(), Some(db)).await;
            assert_eq!(run_minutes(&state, 0, 300, W).await.len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn it_waits_for_the_database_so_a_saved_session_is_never_missed() {
        // At startup the history may not be open on the first polls. Starting
        // a fresh session then would announce a charge the saved state says
        // was already announced.
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            let first = state_with(on_config(), Some(db.clone())).await;
            assert_eq!(run_minutes(&first, 0, 600, W).await.len(), 1);

            let restarted = state_with(on_config(), None).await;
            assert!(run_minutes(&restarted, 660, 900, W).await.is_empty());
            assert_eq!(
                restarted.car_session.lock().await.session,
                CarSession::default(),
                "nothing advanced without the database"
            );
            // The database opens: the saved session is picked up and carries on.
            *restarted.history.lock().await = Some(db.clone());
            let more = run_minutes(&restarted, 960, 1200, W).await;
            assert!(more.is_empty(), "no second start message: {more:?}");
            assert!(stored(&db).unwrap().open.is_some());
        })
        .await;
    }

    #[test]
    fn a_polled_chargers_figure_wins_over_the_estimate() {
        let cfg = CarChargingConfig {
            threshold_w: 3000.0,
            amps: 32.0,
        };
        // The charger says 6.5 kW; the estimate would be 7.36 kW.
        assert_eq!(car_input_w(&cfg, 9000.0, 230.0, Some(6500)), 6500.0);
        // The charger says idle although the load is high: it is the house.
        assert_eq!(car_input_w(&cfg, 9000.0, 230.0, Some(0)), 0.0);
        assert_eq!(
            car_input_w(&cfg, 9000.0, 230.0, Some(80)),
            0.0,
            "standby draw"
        );
        // No charger: the estimate.
        assert!((car_input_w(&cfg, 9000.0, 230.0, None) - 7360.0).abs() < 1e-9);
        assert_eq!(car_input_w(&cfg, 800.0, 230.0, None), 0.0);
    }

    #[test]
    fn the_messages_read_well() {
        let finished = event_message(
            &CarEvent::Finished {
                started_at: 0,
                ended_at: 3 * 3600 + 20 * 60,
                kwh: 11.43,
            },
            false,
        );
        assert!(
            finished.starts_with("🚗 Car charging finished: 11.4 kWh in 3 h 20 m ("),
            "{finished}"
        );
        assert!(!finished.contains("estimated"));
        let short = event_message(
            &CarEvent::Finished {
                started_at: 0,
                ended_at: 25 * 60,
                kwh: 2.0,
            },
            true,
        );
        assert!(short.contains("in 25 m"), "{short}");
    }
}
