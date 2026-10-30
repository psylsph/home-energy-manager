//! Battery health notifications (issue #393).
//!
//! Two kinds, both read-only:
//!
//! - **SOC jumps and stuck SOC**, from [`SocWatch`] on every poll. Every
//!   confirmed event is counted in `battery_health_daily`; the user is told
//!   when `soc_anomaly_alert_enabled` is on, at most one jump message per
//!   battery every [`JUMP_NOTIFY_INTERVAL_SECS`] (stuck is already once per
//!   episode).
//! - **Cell spread widening**, from [`spread_trend`] once a day after
//!   [`SPREAD_CHECK_HOUR`]. What was last sent per module is kept in the
//!   history database's `meta` table, so a restart does not repeat it; a
//!   module still flagged is re-sent only when its spread has grown another
//!   [`SPREAD_REALERT_GROWTH_MV`] or [`SPREAD_REALERT_DAYS`] have passed.
//!
//! The decisions are pure functions; the runners do the I/O and take the
//! sender as a parameter so tests never touch the network.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::Arc;

use chrono::{DateTime, Local, NaiveDate, Timelike};
use serde::{Deserialize, Serialize};

use crate::history::battery_health_trend::{
    spread_trend, SpreadStatus, SpreadTrend, BASELINE_FROM_DAYS_AGO,
};
use crate::inverter::model::InverterSnapshot;
use crate::inverter::poll::AppState;
use crate::inverter::soc_watch::{SocEvent, SocEventKind, SocSource, SocWatch};
use crate::settings::AlertsConfig;

/// Shortest gap between two jump messages about the same battery.
pub const JUMP_NOTIFY_INTERVAL_SECS: i64 = 6 * 3600;
/// Local hour from which the daily spread check runs.
pub const SPREAD_CHECK_HOUR: u32 = 7;
/// Growth past the last alerted spread that is worth another message, mV.
pub const SPREAD_REALERT_GROWTH_MV: f64 = 15.0;
/// Days after which a module still flagged is mentioned again.
pub const SPREAD_REALERT_DAYS: i64 = 30;
/// `meta` key holding [`SpreadAlertState`].
pub const SPREAD_ALERT_META_KEY: &str = "battery_health_alerts";

/// In-memory battery health alert state, one per process.
#[derive(Debug, Default)]
pub struct BatteryHealthAlerts {
    soc_watch: SocWatch,
    /// SOC messages that could not be delivered, awaiting a resend.
    soc_outbox: SocOutbox,
    /// When a jump message about each battery was last sent (unix seconds).
    jump_last_sent: HashMap<SocSource, i64>,
    /// The day the spread check last ran, so most polls skip it cheaply.
    spread_checked_day: Option<NaiveDate>,
    /// When the spread check may try again after it could not finish (unix
    /// seconds); see [`SPREAD_RETRY_SECS`].
    spread_retry_at: Option<i64>,
}

impl BatteryHealthAlerts {
    /// The events worth a message now: every stuck episode, and jumps no
    /// sooner than [`JUMP_NOTIFY_INTERVAL_SECS`] after the last one for the
    /// same battery. Marks the jumps it returns as sent.
    pub fn soc_events_to_announce(&mut self, events: &[SocEvent], now_ts: i64) -> Vec<SocEvent> {
        let _ = (events, now_ts);
        Vec::new()
}
}

/// Gap between attempts to resend an undelivered SOC message.
pub const SOC_RESEND_SECS: i64 = 300;
/// An undelivered SOC message older than this is dropped (it is still
/// counted on the Battery page).
pub const SOC_RESEND_MAX_AGE_SECS: i64 = 3600;

/// SOC messages whose delivery failed, resent every [`SOC_RESEND_SECS`] for
/// up to [`SOC_RESEND_MAX_AGE_SECS`]. Resending the message, rather than
/// re-raising the event, keeps each event counted once.
#[derive(Debug, Default)]
pub struct SocOutbox {
    /// (failed at, text), oldest first.
    failed: Vec<(i64, String)>,
    retry_at: Option<i64>,
}

impl SocOutbox {
    /// Hold `messages` (each with when it first failed) for a resend.
    pub fn add_failed(&mut self, messages: Vec<(i64, String)>, now_ts: i64) {
        let _ = (messages, now_ts);
}

    /// The held messages due for a resend now, taken out of the box (put
    /// them back with [`Self::add_failed`] if the resend fails). Messages
    /// past their age limit are dropped.
    pub fn take_due(&mut self, now_ts: i64) -> Vec<(i64, String)> {
        let _ = now_ts;
        Vec::new()
}
}

/// How long the spread check waits before trying again after it could not
/// finish (no database yet, a failed read, or an undelivered message).
pub const SPREAD_RETRY_SECS: i64 = 600;

/// Whether the spread check is still waiting out a retry delay that ends at
/// `retry_at`. A clock moved back past the delay does not extend it.
pub fn retry_waiting(retry_at: Option<i64>, now_ts: i64) -> bool {
    let _ = (retry_at, now_ts);
    false
}

/// What was last announced about one module's spread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpreadAlerted {
    pub mv: f64,
    pub day: NaiveDate,
}

/// Persisted spread alert state (`meta` key [`SPREAD_ALERT_META_KEY`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SpreadAlertState {
    #[serde(default)]
    pub last_checked_day: Option<NaiveDate>,
    #[serde(default)]
    pub alerted: BTreeMap<i64, SpreadAlerted>,
}

/// Whether the daily spread check should run now.
pub fn spread_check_due(now: DateTime<Local>, last_checked: Option<NaiveDate>) -> bool {
    let _ = (now, last_checked);
    false
}

/// The flagged modules worth a message today.
pub fn spread_alerts_due(
    trends: &[SpreadTrend],
    state: &SpreadAlertState,
    today: NaiveDate,
) -> Vec<SpreadTrend> {
    let _ = (trends, state, today);
    Vec::new()
}

/// The state to store after a check on `today` that announced `sent`.
/// A module back to normal is forgotten, so widening again later is news;
/// `sent` is only remembered, and the day only marked checked, if it was
/// `delivered`, so a failed send is retried.
pub fn spread_state_after(
    state: &SpreadAlertState,
    trends: &[SpreadTrend],
    sent: &[SpreadTrend],
    delivered: bool,
    today: NaiveDate,
) -> SpreadAlertState {
    let _ = (trends, sent, delivered, today);
    state.clone()
}

fn battery_label(source: SocSource) -> String {
    match source {
        SocSource::Module(index) => format!("Battery #{}", index + 1),
        SocSource::System => "The battery".to_string(),
    }
}

/// Notification text for confirmed SOC jumps and stuck episodes.
pub fn build_soc_events_message(
    events: &[SocEvent],
    snap: &InverterSnapshot,
    now: DateTime<Local>,
) -> String {
    let _ = (events, snap, now);
    String::new()
}

/// Notification text for modules whose cell spread is widening or high.
pub fn build_spread_message(trends: &[SpreadTrend], now: DateTime<Local>) -> String {
    let _ = (trends, now);
    String::new()
}

fn soc_alerts_on(config: &AlertsConfig) -> bool {
    config.enabled
        && config.soc_anomaly_alert_enabled
        && crate::alerts::has_delivery_channel(config)
}

fn spread_alerts_on(config: &AlertsConfig) -> bool {
    config.enabled
        && config.cell_spread_alert_enabled
        && crate::alerts::has_delivery_channel(config)
}

/// One poll's SOC watch: feed the sanitized snapshot, count what it confirms
/// in the battery health summary, and pass a message to `send` when the
/// alert is on. `settled` is false during the post-connect grace period.
pub(crate) async fn run_soc_watch<F, Fut>(
    state: &Arc<AppState>,
    snap: &InverterSnapshot,
    now: DateTime<Local>,
    settled: bool,
    poll_interval_secs: u64,
    send: F,
) -> Vec<tokio::task::JoinHandle<()>>
where
    F: Fn(AlertsConfig, String) -> Fut,
    Fut: Future<Output = bool> + Send + 'static,
{
    let _ = (state, snap, now, settled, poll_interval_secs, send);
    Vec::new()
}

/// Cheap in-memory test the poll loop makes before spawning
/// [`run_spread_check`], so most polls spawn nothing.
pub(crate) async fn spread_check_pending(state: &Arc<AppState>, now: DateTime<Local>) -> bool {
    let _ = (state, now);
    false
}

/// The daily spread check: due once a day after [`SPREAD_CHECK_HOUR`] while
/// the alert is on; announces modules from [`spread_alerts_due`] through
/// `send` (which reports whether anything was delivered) and stores the
/// outcome in `meta`.
pub(crate) async fn run_spread_check<F, Fut>(state: &Arc<AppState>, now: DateTime<Local>, send: F)
where
    F: FnOnce(AlertsConfig, String) -> Fut,
    Fut: Future<Output = bool>,
{
    let _ = (state, now, send);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::battery_health::BatteryHealthRow;
    use crate::history::HistoryDb;
    use crate::inverter::model::BatteryModule;
    use crate::test_util::with_isolated_config_dir_async;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(y, m, d, h, min, 0)
            .single()
            .expect("unambiguous local time")
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn jump(source: SocSource) -> SocEvent {
        SocEvent {
            source,
            kind: SocEventKind::Jump { from: 62, to: 18 },
        }
    }

    fn stuck(source: SocSource) -> SocEvent {
        SocEvent {
            source,
            kind: SocEventKind::Stuck {
                soc: 47,
                energy_kwh: 1.62,
            },
        }
    }

    fn trend(module_index: i64, status: SpreadStatus, recent: f64) -> SpreadTrend {
        SpreadTrend {
            module_index,
            status,
            recent_mv: Some(recent),
            baseline_mv: Some(30.0),
            recent_days: 7,
            recent_min_days: 4,
        }
    }

    // ---- SOC event rate limit -------------------------------------------

    #[test]
    fn stuck_episodes_are_always_announced() {
        let mut a = BatteryHealthAlerts::default();
        let m0 = SocSource::Module(0);
        assert_eq!(a.soc_events_to_announce(&[stuck(m0)], 0), vec![stuck(m0)]);
        assert_eq!(a.soc_events_to_announce(&[stuck(m0)], 60), vec![stuck(m0)]);
    }

    #[test]
    fn jumps_are_announced_at_most_every_six_hours_per_battery() {
        let mut a = BatteryHealthAlerts::default();
        let (m0, m1) = (SocSource::Module(0), SocSource::Module(1));
        assert_eq!(a.soc_events_to_announce(&[jump(m0)], 1000), vec![jump(m0)]);
        assert!(a
            .soc_events_to_announce(&[jump(m0)], 1000 + 3600)
            .is_empty());
        assert_eq!(
            a.soc_events_to_announce(&[jump(m1)], 1000 + 3600),
            vec![jump(m1)],
            "another battery has its own limit"
        );
        assert_eq!(
            a.soc_events_to_announce(&[jump(m0)], 1000 + JUMP_NOTIFY_INTERVAL_SECS),
            vec![jump(m0)]
        );
    }

    #[test]
    fn two_jumps_for_one_battery_in_one_poll_send_one() {
        let mut a = BatteryHealthAlerts::default();
        let m0 = SocSource::Module(0);
        assert_eq!(
            a.soc_events_to_announce(&[jump(m0), jump(m0)], 0),
            vec![jump(m0)]
        );
    }

    #[test]
    fn a_clock_moved_back_does_not_silence_jumps_for_ever() {
        let mut a = BatteryHealthAlerts::default();
        let m0 = SocSource::Module(0);
        a.soc_events_to_announce(&[jump(m0)], 1_000_000);
        assert_eq!(
            a.soc_events_to_announce(&[jump(m0)], 1_000_000 - 86_400),
            vec![jump(m0)]
        );
    }

    // ---- failure log rate limit ------------------------------------------

    #[test]
    fn a_retry_waits_out_its_delay() {
        assert!(!retry_waiting(None, 1000));
        assert!(retry_waiting(Some(1000 + SPREAD_RETRY_SECS), 1000));
        assert!(retry_waiting(
            Some(1000 + SPREAD_RETRY_SECS),
            1000 + SPREAD_RETRY_SECS - 1
        ));
        assert!(!retry_waiting(
            Some(1000 + SPREAD_RETRY_SECS),
            1000 + SPREAD_RETRY_SECS
        ));
        assert!(
            !retry_waiting(Some(1000 + SPREAD_RETRY_SECS), 1000 - 3600),
            "a clock moved back does not stretch the delay"
        );
    }

    // ---- SOC resend -----------------------------------------------------

    #[test]
    fn an_undelivered_message_waits_then_comes_out_once() {
        let mut o = SocOutbox::default();
        o.add_failed(vec![(1000, "a".into())], 1000);
        assert!(o.take_due(1000 + SOC_RESEND_SECS - 1).is_empty());
        assert_eq!(
            o.take_due(1000 + SOC_RESEND_SECS),
            vec![(1000, "a".to_string())]
        );
        assert!(
            o.take_due(1000 + 2 * SOC_RESEND_SECS).is_empty(),
            "taken out"
        );
    }

    #[test]
    fn a_failed_resend_keeps_the_original_failure_time_and_order() {
        let mut o = SocOutbox::default();
        o.add_failed(vec![(1000, "a".into())], 1000);
        let taken = o.take_due(1300);
        o.add_failed(vec![(1250, "b".into())], 1250); // a new failure meanwhile
        o.add_failed(taken, 1300); // the resend failed too
        assert_eq!(
            o.take_due(1300 + SOC_RESEND_SECS),
            vec![(1000, "a".to_string()), (1250, "b".to_string())]
        );
    }

    #[test]
    fn a_message_past_its_age_limit_is_dropped() {
        let mut o = SocOutbox::default();
        o.add_failed(vec![(1000, "old".into())], 1000);
        o.add_failed(
            vec![(1000 + SOC_RESEND_MAX_AGE_SECS, "new".into())],
            1000 + SOC_RESEND_MAX_AGE_SECS,
        );
        let due = o.take_due(1000 + SOC_RESEND_MAX_AGE_SECS + SOC_RESEND_SECS);
        assert_eq!(
            due,
            vec![(1000 + SOC_RESEND_MAX_AGE_SECS, "new".to_string())]
        );
    }

    #[test]
    fn a_clock_moved_back_does_not_stall_the_resend() {
        let mut o = SocOutbox::default();
        o.add_failed(vec![(10_000, "a".into())], 10_000);
        assert_eq!(o.take_due(10_000 - 86_400).len(), 1);
    }

    // ---- SOC message ----------------------------------------------------

    #[test]
    fn soc_message_names_the_battery_and_the_numbers() {
        let snap = InverterSnapshot {
            battery_modules: vec![BatteryModule {
                index: 1,
                bms_firmware: 3015,
                ..BatteryModule::default()
            }],
            ..InverterSnapshot::default()
        };
        let text = build_soc_events_message(
            &[jump(SocSource::Module(1)), stuck(SocSource::System)],
            &snap,
            at(2026, 10, 30, 9, 15),
        );
        assert!(text.contains("2026-10-30 09:15"), "{text}");
        assert!(text.contains("Battery #2"), "{text}");
        assert!(text.contains("62%") && text.contains("18%"), "{text}");
        assert!(text.contains("3015"), "{text}");
        assert!(text.contains("The battery"), "{text}");
        assert!(text.contains("47%") && text.contains("1.6 kWh"), "{text}");
        assert!(text.contains("discharged"), "{text}");
        // HEM has no calibration button for this firmware, so say how.
        assert!(text.contains("reserve to 4%"), "{text}");
        assert!(
            text.contains("Force Discharge") && text.contains("Force Charge"),
            "{text}"
        );
    }

    #[test]
    fn stuck_while_charging_says_charged() {
        let event = SocEvent {
            source: SocSource::Module(0),
            kind: SocEventKind::Stuck {
                soc: 30,
                energy_kwh: -2.0,
            },
        };
        let text = build_soc_events_message(
            &[event],
            &InverterSnapshot::default(),
            at(2026, 10, 30, 9, 0),
        );
        assert!(
            text.contains("2.0 kWh") && text.contains("charged"),
            "{text}"
        );
        assert!(!text.contains("discharged"), "{text}");
    }

    // ---- spread check timing --------------------------------------------

    #[test]
    fn the_spread_check_runs_once_a_day_from_seven() {
        assert!(!spread_check_due(at(2026, 10, 30, 6, 59), None));
        assert!(spread_check_due(at(2026, 10, 30, 7, 0), None));
        assert!(spread_check_due(
            at(2026, 10, 30, 7, 0),
            Some(date(2026, 10, 29))
        ));
        assert!(!spread_check_due(
            at(2026, 10, 30, 23, 0),
            Some(date(2026, 10, 30))
        ));
    }

    #[test]
    fn a_check_stamped_in_the_future_does_not_block_it() {
        // A clock that ran ahead once must not silence the check until then.
        assert!(spread_check_due(
            at(2026, 10, 30, 8, 0),
            Some(date(2026, 12, 1))
        ));
    }

    // ---- which spreads to announce --------------------------------------

    #[test]
    fn a_newly_flagged_module_is_announced() {
        let trends = [
            trend(0, SpreadStatus::Ok, 30.0),
            trend(1, SpreadStatus::Widening, 70.0),
            trend(2, SpreadStatus::Insufficient, 90.0),
        ];
        let due = spread_alerts_due(&trends, &SpreadAlertState::default(), date(2026, 10, 30));
        assert_eq!(due, vec![trends[1].clone()]);
    }

    #[test]
    fn an_announced_module_waits_for_growth_or_a_month() {
        let mut state = SpreadAlertState::default();
        state.alerted.insert(
            1,
            SpreadAlerted {
                mv: 70.0,
                day: date(2026, 10, 1),
            },
        );
        let today = date(2026, 10, 20);
        let same = [trend(1, SpreadStatus::Widening, 84.0)];
        assert!(spread_alerts_due(&same, &state, today).is_empty());
        let grown = [trend(1, SpreadStatus::Widening, 85.0)];
        assert_eq!(spread_alerts_due(&grown, &state, today), grown.to_vec());
        let month_on = date(2026, 10, 31);
        assert_eq!(spread_alerts_due(&same, &state, month_on), same.to_vec());
    }

    #[test]
    fn state_after_a_delivered_check_remembers_what_was_sent() {
        let today = date(2026, 10, 30);
        let trends = [trend(1, SpreadStatus::High, 160.0)];
        let after = spread_state_after(&SpreadAlertState::default(), &trends, &trends, true, today);
        assert_eq!(after.last_checked_day, Some(today));
        assert_eq!(
            after.alerted.get(&1),
            Some(&SpreadAlerted {
                mv: 160.0,
                day: today
            })
        );
    }

    #[test]
    fn a_failed_send_is_not_remembered_and_the_day_stays_open() {
        let today = date(2026, 10, 30);
        let trends = [trend(1, SpreadStatus::High, 160.0)];
        let after =
            spread_state_after(&SpreadAlertState::default(), &trends, &trends, false, today);
        assert_eq!(
            after.last_checked_day, None,
            "the day is not done until it is delivered"
        );
        assert!(after.alerted.is_empty());
    }

    #[test]
    fn a_module_back_to_normal_is_forgotten_but_insufficient_data_is_not() {
        let mut state = SpreadAlertState::default();
        for i in [0, 1] {
            state.alerted.insert(
                i,
                SpreadAlerted {
                    mv: 70.0,
                    day: date(2026, 10, 1),
                },
            );
        }
        let trends = [
            trend(0, SpreadStatus::Ok, 40.0),
            trend(1, SpreadStatus::Insufficient, 70.0),
        ];
        let after = spread_state_after(&state, &trends, &[], false, date(2026, 10, 30));
        assert!(!after.alerted.contains_key(&0));
        assert!(after.alerted.contains_key(&1));
    }

    #[test]
    fn spread_message_names_modules_and_figures() {
        let mut high = trend(2, SpreadStatus::High, 162.4);
        high.baseline_mv = None;
        let text = build_spread_message(
            &[trend(0, SpreadStatus::Widening, 71.6), high],
            at(2026, 10, 30, 7, 0),
        );
        assert!(text.contains("Battery #1"), "{text}");
        assert!(text.contains("30 mV") && text.contains("72 mV"), "{text}");
        assert!(
            text.contains("Battery #3") && text.contains("162 mV"),
            "{text}"
        );
    }

    // ---- runners --------------------------------------------------------

    fn channel_config() -> AlertsConfig {
        AlertsConfig {
            enabled: true,
            ntfy_topic: "hem-test".into(),
            cell_spread_alert_enabled: true,
            soc_anomaly_alert_enabled: true,
            ..Default::default()
        }
    }

    fn open_history() -> Arc<HistoryDb> {
        let path = crate::test_util::make_unique_test_dir("battery-health-alerts").join("h.db");
        Arc::new(HistoryDb::open(&path).unwrap())
    }

    async fn state_with(config: AlertsConfig, db: Option<Arc<HistoryDb>>) -> Arc<AppState> {
        let state = Arc::new(AppState::new());
        *state.alert_config.lock().await = config;
        *state.history.lock().await = db;
        state
    }

    fn module_snap(soc: u8) -> InverterSnapshot {
        InverterSnapshot {
            soc,
            battery_capacity_kwh: 9.5,
            battery_modules: vec![BatteryModule {
                index: 0,
                soc,
                cell_voltages: vec![3.30, 3.31],
                ..BatteryModule::default()
            }],
            ..InverterSnapshot::default()
        }
    }

    type Sent = Arc<std::sync::Mutex<Vec<String>>>;

    /// Polls a 50 -> 20 step a minute apart from 12:00, returning what was sent.
    async fn run_step(state: &Arc<AppState>) -> Sent {
        run_socs(state, &[50, 50, 20, 20, 20], 0, true).await
    }

    /// Polls `socs` a minute apart from 12:`from_minute`; `delivered` is what
    /// the sender reports. Waits for each delivery to finish.
    async fn run_socs(
        state: &Arc<AppState>,
        socs: &[u8],
        from_minute: u32,
        delivered: bool,
    ) -> Sent {
        let sent: Sent = Arc::default();
        for (i, &soc) in socs.iter().enumerate() {
            let log = sent.clone();
            let mut snap = InverterSnapshot {
                home_power: 500 + i as i32, // live data wobbles
                ..module_snap(soc)
            };
            snap.battery_modules[0].voltage = 51.0 + i as f32 / 100.0;
            let handles = run_soc_watch(
                state,
                &snap,
                at(2026, 10, 30, 12, from_minute + i as u32),
                true,
                60,
                move |_, text| {
                    log.lock().unwrap().push(text);
                    async move { delivered }
                },
            )
            .await;
            for handle in handles {
                handle.await.unwrap();
            }
        }
        sent
    }

    #[tokio::test]
    async fn an_undelivered_soc_message_is_resent_on_a_later_poll() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(channel_config(), Some(open_history())).await;
            // The jump confirms at 12:04 and its message fails.
            let first = run_socs(&state, &[50, 50, 20, 20, 20], 0, false).await;
            assert_eq!(first.lock().unwrap().len(), 1);
            let text = first.lock().unwrap()[0].clone();

            // No new events; nothing is resent before 12:09.
            assert!(run_socs(&state, &[20, 20, 20], 6, true)
                .await
                .lock()
                .unwrap()
                .is_empty());
            // At 12:09 it goes again, delivered this time, then not again.
            let resent = run_socs(&state, &[20, 20, 20], 9, true).await;
            assert_eq!(*resent.lock().unwrap(), vec![text]);
            let db = open_db(&state).await;
            assert_eq!(rows(&db)[0].soc_jumps, 1, "counted once");
        })
        .await;
    }

    async fn open_db(state: &Arc<AppState>) -> Arc<HistoryDb> {
        state.history.lock().await.clone().unwrap()
    }

    #[tokio::test]
    async fn after_a_delivered_jump_message_the_next_jump_waits() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(channel_config(), Some(open_history())).await;
            assert_eq!(
                run_socs(&state, &[50, 50, 20, 20, 20], 0, true)
                    .await
                    .lock()
                    .unwrap()
                    .len(),
                1
            );
            let again = run_socs(&state, &[20, 70, 70, 70], 10, true).await;
            assert!(again.lock().unwrap().is_empty());
        })
        .await;
    }

    fn rows(db: &HistoryDb) -> Vec<BatteryHealthRow> {
        db.flush_battery_health();
        db.query_battery_health(date(2026, 1, 1), date(2027, 1, 1))
            .unwrap()
    }

    #[tokio::test]
    async fn a_soc_jump_is_counted_and_announced() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            let state = state_with(channel_config(), Some(db.clone())).await;
            let sent = run_step(&state).await;
            assert_eq!(sent.lock().unwrap().len(), 1);
            assert!(sent.lock().unwrap()[0].contains("50%"));
            assert_eq!(rows(&db)[0].soc_jumps, 1);
        })
        .await;
    }

    #[tokio::test]
    async fn with_the_alert_off_a_soc_jump_is_still_counted_but_not_sent() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            let config = AlertsConfig {
                soc_anomaly_alert_enabled: false,
                ..channel_config()
            };
            let state = state_with(config, Some(db.clone())).await;
            assert!(run_step(&state).await.lock().unwrap().is_empty());
            assert_eq!(rows(&db)[0].soc_jumps, 1);
        })
        .await;
    }

    #[tokio::test]
    async fn without_a_channel_a_soc_jump_is_not_sent() {
        with_isolated_config_dir_async(|| async {
            let config = AlertsConfig {
                ntfy_topic: String::new(),
                ..channel_config()
            };
            let state = state_with(config, Some(open_history())).await;
            assert!(run_step(&state).await.lock().unwrap().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn without_a_history_database_a_soc_jump_is_still_sent() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(channel_config(), None).await;
            assert_eq!(run_step(&state).await.lock().unwrap().len(), 1);
        })
        .await;
    }

    /// 7 recent days at `recent` mV and 10 baseline days at 30 mV near full,
    /// for module 0, ending on 2026-10-30.
    fn seed_spread(db: &HistoryDb, recent: f32) {
        let seed = |day: u32, month: u32, spread_mv: f32| {
            let lo = 3.40_f32;
            let snap = InverterSnapshot {
                soc: 98,
                battery_modules: vec![BatteryModule {
                    index: 0,
                    soc: 98,
                    cell_voltages: vec![lo, lo + spread_mv / 1000.0],
                    ..BatteryModule::default()
                }],
                ..InverterSnapshot::default()
            };
            db.observe_battery_health(&snap, at(2026, month, day, 12, 0));
            db.flush_battery_health();
        };
        for d in 20..=29 {
            seed(d, 9, 30.0);
        }
        for d in 24..=30 {
            seed(d, 10, recent);
        }
    }

    async fn check(state: &Arc<AppState>, now: DateTime<Local>, delivered: bool) -> Sent {
        let sent: Sent = Arc::default();
        let log = sent.clone();
        run_spread_check(state, now, move |_, text| {
            log.lock().unwrap().push(text);
            async move { delivered }
        })
        .await;
        sent
    }

    fn stored(db: &HistoryDb) -> SpreadAlertState {
        serde_json::from_str(&db.get_meta_value(SPREAD_ALERT_META_KEY).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn a_widening_spread_is_announced_once_and_survives_a_restart() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            seed_spread(&db, 80.0);
            let state = state_with(channel_config(), Some(db.clone())).await;

            assert_eq!(
                check(&state, at(2026, 10, 30, 7, 5), true)
                    .await
                    .lock()
                    .unwrap()
                    .len(),
                1
            );
            assert!(check(&state, at(2026, 10, 30, 7, 6), true)
                .await
                .lock()
                .unwrap()
                .is_empty());
            assert_eq!(
                stored(&db).alerted.get(&0).map(|a| a.mv.round()),
                Some(80.0)
            );

            // A restarted process (fresh in-memory state) the same day and
            // the next day does not repeat it.
            let restarted = state_with(channel_config(), Some(db.clone())).await;
            assert!(check(&restarted, at(2026, 10, 30, 9, 0), true)
                .await
                .lock()
                .unwrap()
                .is_empty());
            assert!(check(&restarted, at(2026, 10, 31, 7, 0), true)
                .await
                .lock()
                .unwrap()
                .is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_failed_spread_send_is_retried_ten_minutes_later() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            seed_spread(&db, 80.0);
            let state = state_with(channel_config(), Some(db.clone())).await;
            let sends = |state: Arc<AppState>, h: u32, m: u32, ok: bool| async move {
                check(&state, at(2026, 10, 30, h, m), ok)
                    .await
                    .lock()
                    .unwrap()
                    .len()
            };
            assert_eq!(sends(state.clone(), 7, 5, false).await, 1);
            assert_eq!(stored(&db).last_checked_day, None);
            assert!(!spread_check_pending(&state, at(2026, 10, 30, 7, 10)).await);
            assert_eq!(sends(state.clone(), 7, 10, true).await, 0, "still waiting");
            assert_eq!(sends(state.clone(), 7, 15, true).await, 1);
            assert_eq!(
                sends(state.clone(), 8, 0, true).await,
                0,
                "done for the day"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_check_before_the_database_is_open_runs_again_once_it_is() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(channel_config(), None).await;
            assert!(check(&state, at(2026, 10, 30, 7, 5), true)
                .await
                .lock()
                .unwrap()
                .is_empty());
            // Not on every poll: after the retry delay.
            assert!(!spread_check_pending(&state, at(2026, 10, 30, 7, 6)).await);
            assert!(spread_check_pending(&state, at(2026, 10, 30, 7, 15)).await);

            let db = open_history();
            seed_spread(&db, 80.0);
            *state.history.lock().await = Some(db);
            assert_eq!(
                check(&state, at(2026, 10, 30, 7, 15), true)
                    .await
                    .lock()
                    .unwrap()
                    .len(),
                1
            );
        })
        .await;
    }

    #[tokio::test]
    async fn the_poll_loop_gate_is_closed_once_the_day_is_checked() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            seed_spread(&db, 32.0);
            let state = state_with(channel_config(), Some(db)).await;
            assert!(!spread_check_pending(&state, at(2026, 10, 30, 6, 59)).await);
            assert!(spread_check_pending(&state, at(2026, 10, 30, 7, 0)).await);
            check(&state, at(2026, 10, 30, 7, 0), true).await;
            assert!(!spread_check_pending(&state, at(2026, 10, 30, 7, 1)).await);
            assert!(spread_check_pending(&state, at(2026, 10, 31, 7, 0)).await);
        })
        .await;
    }

    #[tokio::test]
    async fn the_poll_loop_gate_stays_closed_while_the_alert_is_off() {
        // Otherwise every poll after 07:00 would spawn a check that does nothing.
        with_isolated_config_dir_async(|| async {
            let off = AlertsConfig {
                cell_spread_alert_enabled: false,
                ..channel_config()
            };
            let state = state_with(off, Some(open_history())).await;
            assert!(!spread_check_pending(&state, at(2026, 10, 30, 7, 0)).await);
            let no_channel = AlertsConfig {
                ntfy_topic: String::new(),
                ..channel_config()
            };
            *state.alert_config.lock().await = no_channel;
            assert!(!spread_check_pending(&state, at(2026, 10, 30, 7, 0)).await);
        })
        .await;
    }

    #[tokio::test]
    async fn a_healthy_spread_sends_nothing_but_records_the_check() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            seed_spread(&db, 32.0);
            let state = state_with(channel_config(), Some(db.clone())).await;
            assert!(check(&state, at(2026, 10, 30, 7, 5), true)
                .await
                .lock()
                .unwrap()
                .is_empty());
            assert_eq!(stored(&db).last_checked_day, Some(date(2026, 10, 30)));
        })
        .await;
    }

    #[tokio::test]
    async fn with_the_spread_alert_off_nothing_runs() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            seed_spread(&db, 80.0);
            let config = AlertsConfig {
                cell_spread_alert_enabled: false,
                ..channel_config()
            };
            let state = state_with(config, Some(db.clone())).await;
            assert!(check(&state, at(2026, 10, 30, 7, 5), true)
                .await
                .lock()
                .unwrap()
                .is_empty());
            assert_eq!(db.get_meta_value(SPREAD_ALERT_META_KEY), None);
        })
        .await;
    }

    #[tokio::test]
    async fn corrupt_stored_state_is_treated_as_empty() {
        with_isolated_config_dir_async(|| async {
            let db = open_history();
            seed_spread(&db, 80.0);
            db.set_meta_value(SPREAD_ALERT_META_KEY, "{not json")
                .unwrap();
            let state = state_with(channel_config(), Some(db.clone())).await;
            assert_eq!(
                check(&state, at(2026, 10, 30, 7, 5), true)
                    .await
                    .lock()
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(stored(&db).last_checked_day, Some(date(2026, 10, 30)));
        })
        .await;
    }
}
