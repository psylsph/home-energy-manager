//! Full-charge reminder (issue #426).
//!
//! The cells of a battery balance at the top of charge, and the BMS
//! re-anchors its SOC there, so a battery that never reaches 100% slowly
//! drifts. That happens with the charge cap, and to solar-only users in
//! winter. Once a day after [`CHECK_HOUR`] this checks the history for the
//! last reading at 100% and notifies when it's more than
//! `full_charge_alert_days` ago (or, with none in the history, when the
//! history is that old). It says so once, then again only after another
//! interval with still no full charge. Read-only.
//!
//! The decision is a pure function; the runner does the I/O and takes the
//! sender as a parameter so tests never touch the network.

use std::future::Future;
use std::sync::Arc;

use chrono::{DateTime, Local, NaiveDate, Timelike};
use serde::{Deserialize, Serialize};

use crate::inverter::poll::AppState;
use crate::settings::AlertsConfig;

/// Local hour from which the daily check runs.
pub const CHECK_HOUR: u32 = 7;
/// How long the check waits before trying again after it couldn't finish.
pub const RETRY_SECS: i64 = 600;
/// `meta` key holding [`FullChargeAlertState`].
pub const META_KEY: &str = "full_charge_alert";

const DAY_SECS: i64 = 86_400;

/// In-memory gate, so most polls skip the check without touching the DB.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FullChargeCheck {
    checked_day: Option<NaiveDate>,
    retry_at: Option<i64>,
}

/// Persisted in `meta`, so a restart doesn't repeat the day's check or the
/// message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FullChargeAlertState {
    #[serde(default)]
    pub last_checked_day: Option<NaiveDate>,
    /// When the reminder was last delivered (unix seconds).
    #[serde(default)]
    pub alerted_at: Option<i64>,
}

/// Whether the reminder is on and has somewhere to go.
fn alert_on(config: &AlertsConfig) -> bool {
    config.enabled
        && config.full_charge_alert_enabled
        && crate::alerts::has_delivery_channel(config)
}

/// Whether the daily check should run now.
pub fn check_due(now: DateTime<Local>, last_checked: Option<NaiveDate>) -> bool {
    now.hour() >= CHECK_HOUR && last_checked != Some(now.date_naive())
}

fn retry_waiting(retry_at: Option<i64>, now_ts: i64) -> bool {
    retry_at.is_some_and(|at| (1..=RETRY_SECS).contains(&(at - now_ts)))
}

/// Whole days without a full charge, when that's worth a message now.
///
/// Counts from the last reading at 100%, or from the oldest reading when
/// there's none (so a new install isn't told off on day one). After a
/// message, the next waits another `days` unless a full charge came since.
pub fn overdue_days(
    last_full: Option<i64>,
    earliest: Option<i64>,
    alerted_at: Option<i64>,
    now_ts: i64,
    days: u32,
) -> Option<i64> {
    let since = last_full.or(earliest)?;
    let window = i64::from(days) * DAY_SECS;
    let elapsed = now_ts - since;
    if elapsed < window {
        return None;
    }
    // A message already sent about this stretch, and not long enough ago
    // (a clock moved back counts as not long enough).
    if alerted_at.is_some_and(|at| at >= since && now_ts - at < window) {
        return None;
    }
    Some(elapsed / DAY_SECS)
}

/// The message for [`overdue_days`].
pub fn build_message(days: i64, last_full: Option<i64>) -> String {
    let when = last_full
        .and_then(|ts| DateTime::from_timestamp(ts, 0))
        .map(|at| at.with_timezone(&Local).format("%-d %B").to_string());
    let lead = match when {
        Some(date) => format!(
            "🔋 HEM: the battery hasn't been charged to 100% for {days} days (last on {date})."
        ),
        None => format!(
            "🔋 HEM: the battery hasn't been charged to 100% in the {days} days HEM has recorded."
        ),
    };
    format!(
        "{lead} A full charge now and then lets the cells balance and keeps the SOC reading \
         accurate. If you use the charge cap, switch it off for a day or let a Timed Charge \
         run to 100%."
    )
}

/// Cheap in-memory test the poll loop makes before spawning
/// [`run_check`], so most polls spawn nothing.
pub(crate) async fn check_pending(state: &Arc<AppState>, now: DateTime<Local>) -> bool {
    if !alert_on(&*state.alert_config.lock().await) {
        return false;
    }
    let gate = state.full_charge_check.lock().await;
    check_due(now, gate.checked_day) && !retry_waiting(gate.retry_at, now.timestamp())
}

/// The daily check: due once a day after [`CHECK_HOUR`] while the reminder
/// is on; sends through `send` (which reports whether it was delivered) and
/// stores the outcome in `meta`.
pub(crate) async fn run_check<F, Fut>(state: &Arc<AppState>, now: DateTime<Local>, send: F)
where
    F: FnOnce(AlertsConfig, String) -> Fut,
    Fut: Future<Output = bool>,
{
    let config = state.alert_config.lock().await.clone();
    if !alert_on(&config) {
        return;
    }
    let today = now.date_naive();
    let now_ts = now.timestamp();
    let previous = {
        // Claim today's check before any await, so overlapping callers
        // can't both run it.
        let mut gate = state.full_charge_check.lock().await;
        if !check_due(now, gate.checked_day) || retry_waiting(gate.retry_at, now_ts) {
            return;
        }
        gate.checked_day.replace(today)
    };
    // Hand the claim back when the check couldn't finish, and try again
    // after RETRY_SECS rather than on every poll.
    let release = || async {
        let mut gate = state.full_charge_check.lock().await;
        if gate.checked_day == Some(today) {
            gate.checked_day = previous;
        }
        gate.retry_at = Some(now_ts + RETRY_SECS);
    };

    // No battery, nothing to charge. A Gateway counts as having one: it
    // reports its AIOs' SOC. Before the first snapshot it isn't known yet:
    // try again later.
    let batteryless = state.latest_snapshot.lock().await.as_ref().map(|snap| {
        snap.device_type.is_batteryless()
            && snap.device_type != crate::inverter::model::DeviceType::Gateway
    });
    match batteryless {
        Some(true) => return,
        Some(false) => {}
        None => {
            release().await;
            return;
        }
    }
    let Some(db) = state.history.lock().await.clone() else {
        release().await;
        return;
    };

    let read_db = db.clone();
    let loaded = tokio::task::spawn_blocking(move || {
        let last_full = read_db.last_full_charge_at()?;
        let earliest = read_db.earliest_reading_at()?;
        let stored = read_db
            .get_meta_value(META_KEY)
            .and_then(|raw| serde_json::from_str::<FullChargeAlertState>(&raw).ok())
            .unwrap_or_default();
        Ok::<_, String>((last_full, earliest, stored))
    })
    .await
    .map_err(|e| format!("full charge check worker failed: {e}"))
    .and_then(|r| r);
    let (last_full, earliest, stored) = match loaded {
        Ok(loaded) => loaded,
        Err(e) => {
            tracing::warn!("Full charge check failed, will retry: {e}");
            release().await;
            return;
        }
    };
    // Already done today, by this process or a previous one.
    if stored.last_checked_day == Some(today) {
        return;
    }

    let due = overdue_days(
        last_full,
        earliest,
        stored.alerted_at,
        now_ts,
        config.full_charge_alert_days,
    );
    let delivered = match due {
        Some(days) => {
            tracing::warn!("Battery hasn't reached 100% for {days} days");
            send(config, build_message(days, last_full)).await
        }
        None => false,
    };
    if due.is_some() && !delivered {
        tracing::warn!("Full charge reminder not delivered, will retry");
        release().await;
        return;
    }
    let next = FullChargeAlertState {
        last_checked_day: Some(today),
        alerted_at: if delivered {
            Some(now_ts)
        } else {
            stored.alerted_at
        },
    };
    let raw = match serde_json::to_string(&next) {
        Ok(raw) => raw,
        Err(e) => {
            tracing::warn!("Failed to encode the full charge alert state: {e}");
            return;
        }
    };
    match tokio::task::spawn_blocking(move || db.set_meta_value(META_KEY, &raw)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("Failed to store the full charge alert state: {e}"),
        Err(e) => tracing::warn!("Full charge alert state worker failed: {e}"),
    }
    state.full_charge_check.lock().await.retry_at = None;
}

/// The last reading at 100% for the Control page, or `None` when there's
/// none or the history isn't open.
pub(crate) async fn last_full_charge_at(state: &Arc<AppState>) -> Option<i64> {
    let db = state.history.lock().await.clone()?;
    tokio::task::spawn_blocking(move || db.last_full_charge_at())
        .await
        .ok()?
        .unwrap_or_else(|e| {
            tracing::warn!("{e}");
            None
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::HistoryDb;
    use crate::inverter::model::{DeviceType, InverterSnapshot};
    use crate::test_util::with_isolated_config_dir_async;
    use chrono::TimeZone;

    const DAY: i64 = DAY_SECS;

    fn at(y: i32, m: u32, d: u32, h: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(y, m, d, h, 0, 0)
            .single()
            .expect("unambiguous local time")
    }

    // ---- decision -------------------------------------------------------

    #[test]
    fn overdue_only_after_the_interval() {
        let now = 100 * DAY;
        assert_eq!(
            overdue_days(Some(now - 29 * DAY), None, None, now, 30),
            None
        );
        assert_eq!(
            overdue_days(Some(now - 30 * DAY), None, None, now, 30),
            Some(30)
        );
        assert_eq!(
            overdue_days(Some(now - 45 * DAY), Some(0), None, now, 30),
            Some(45)
        );
    }

    #[test]
    fn with_no_full_charge_it_counts_from_the_start_of_the_history() {
        let now = 100 * DAY;
        assert_eq!(
            overdue_days(None, Some(now - 10 * DAY), None, now, 30),
            None
        );
        assert_eq!(
            overdue_days(None, Some(now - 31 * DAY), None, now, 30),
            Some(31)
        );
        assert_eq!(overdue_days(None, None, None, now, 30), None, "no history");
    }

    #[test]
    fn says_it_once_per_interval() {
        let now = 100 * DAY;
        let last = Some(now - 40 * DAY);
        // Told 5 days ago about this same stretch: not again yet.
        assert_eq!(overdue_days(last, None, Some(now - 5 * DAY), now, 30), None);
        // Told 30 days ago: again.
        assert_eq!(
            overdue_days(last, None, Some(now - 30 * DAY), now, 30),
            Some(40)
        );
        // Told before the last full charge: that was another stretch.
        let recent = Some(now - 31 * DAY);
        assert_eq!(
            overdue_days(recent, None, Some(now - 35 * DAY), now, 30),
            Some(31)
        );
        // A clock moved back behind the last message doesn't repeat it.
        assert_eq!(overdue_days(last, None, Some(now + DAY), now, 30), None);
    }

    #[test]
    fn due_once_a_day_from_seven() {
        assert!(!check_due(at(2026, 11, 3, 6), None));
        assert!(check_due(at(2026, 11, 3, 7), None));
        assert!(!check_due(
            at(2026, 11, 3, 9),
            Some(at(2026, 11, 3, 7).date_naive())
        ));
        assert!(check_due(
            at(2026, 11, 4, 7),
            Some(at(2026, 11, 3, 7).date_naive())
        ));
    }

    #[test]
    fn the_message_names_the_days_and_the_last_full_charge() {
        let last = at(2026, 9, 20, 12).timestamp();
        let text = build_message(44, Some(last));
        assert!(
            text.contains("for 44 days (last on 20 September)"),
            "{text}"
        );
        assert!(text.contains("charge cap"), "{text}");
        let text = build_message(31, None);
        assert!(text.contains("in the 31 days HEM has recorded"), "{text}");
    }

    // ---- runner ---------------------------------------------------------

    fn channel_config() -> AlertsConfig {
        AlertsConfig {
            enabled: true,
            ntfy_topic: "hem-test".into(),
            ..Default::default()
        }
    }

    fn open_history() -> Arc<HistoryDb> {
        let path = crate::test_util::make_unique_test_dir("full-charge-alert").join("h.db");
        Arc::new(HistoryDb::open(&path).unwrap())
    }

    fn reading(db: &HistoryDb, ts: i64, soc: u8) {
        db.insert_reading(&InverterSnapshot {
            timestamp: ts,
            soc,
            ..Default::default()
        });
    }

    async fn state_with(
        config: AlertsConfig,
        db: Option<Arc<HistoryDb>>,
        device_type: Option<DeviceType>,
    ) -> Arc<AppState> {
        let state = Arc::new(AppState::new());
        *state.alert_config.lock().await = config;
        *state.history.lock().await = db;
        *state.latest_snapshot.lock().await = device_type.map(|device_type| InverterSnapshot {
            device_type,
            ..Default::default()
        });
        state
    }

    type Sent = Arc<std::sync::Mutex<Vec<String>>>;

    async fn check(state: &Arc<AppState>, now: DateTime<Local>, delivered: bool) -> Vec<String> {
        let sent: Sent = Arc::default();
        let log = sent.clone();
        run_check(state, now, move |_, text| {
            log.lock().unwrap().push(text);
            async move { delivered }
        })
        .await;
        let out = sent.lock().unwrap().clone();
        out
    }

    fn stored(db: &HistoryDb) -> Option<FullChargeAlertState> {
        db.get_meta_value(META_KEY)
            .map(|raw| serde_json::from_str(&raw).unwrap())
    }

    #[tokio::test]
    async fn an_overdue_full_charge_is_announced_once_and_survives_a_restart() {
        with_isolated_config_dir_async(|| async {
            let now = at(2026, 11, 3, 8);
            let db = open_history();
            reading(&db, now.timestamp() - 60 * DAY, 80);
            reading(&db, now.timestamp() - 40 * DAY, 100);
            reading(&db, now.timestamp() - DAY, 90);
            let state = state_with(
                channel_config(),
                Some(db.clone()),
                Some(DeviceType::AllInOne6kW),
            )
            .await;

            let sent = check(&state, now, true).await;
            assert_eq!(sent.len(), 1);
            assert!(sent[0].contains("for 40 days"), "{}", sent[0]);
            assert_eq!(
                stored(&db),
                Some(FullChargeAlertState {
                    last_checked_day: Some(now.date_naive()),
                    alerted_at: Some(now.timestamp()),
                })
            );
            // Not again today, nor the next day, nor after a restart.
            assert!(check(&state, now, true).await.is_empty());
            let restarted = state_with(
                channel_config(),
                Some(db.clone()),
                Some(DeviceType::AllInOne6kW),
            )
            .await;
            let tomorrow = at(2026, 11, 4, 8);
            assert!(check(&restarted, tomorrow, true).await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_recent_full_charge_sends_nothing_but_records_the_check() {
        with_isolated_config_dir_async(|| async {
            let now = at(2026, 11, 3, 8);
            let db = open_history();
            reading(&db, now.timestamp() - 60 * DAY, 80);
            reading(&db, now.timestamp() - 3 * DAY, 100);
            let state = state_with(
                channel_config(),
                Some(db.clone()),
                Some(DeviceType::AllInOne6kW),
            )
            .await;
            assert!(check(&state, now, true).await.is_empty());
            assert_eq!(
                stored(&db).unwrap().last_checked_day,
                Some(now.date_naive())
            );
        })
        .await;
    }

    #[tokio::test]
    async fn an_undelivered_reminder_is_retried_later() {
        with_isolated_config_dir_async(|| async {
            let now = at(2026, 11, 3, 8);
            let db = open_history();
            reading(&db, now.timestamp() - 40 * DAY, 100);
            let state = state_with(
                channel_config(),
                Some(db.clone()),
                Some(DeviceType::AllInOne6kW),
            )
            .await;
            assert_eq!(check(&state, now, false).await.len(), 1);
            assert_eq!(stored(&db), None, "nothing recorded");
            assert!(
                !check_pending(&state, now).await,
                "waits out the retry delay"
            );
            let later = now + chrono::Duration::seconds(RETRY_SECS + 1);
            assert!(check_pending(&state, later).await);
            assert_eq!(check(&state, later, true).await.len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn a_batteryless_inverter_is_never_told() {
        with_isolated_config_dir_async(|| async {
            let now = at(2026, 11, 3, 8);
            let db = open_history();
            reading(&db, now.timestamp() - 90 * DAY, 0);
            let state = state_with(
                channel_config(),
                Some(db.clone()),
                Some(DeviceType::PvInverter),
            )
            .await;
            assert!(check(&state, now, true).await.is_empty());
        })
        .await;
    }

    /// A Gateway reports its AIOs' SOC, and the #426 reporter is behind one.
    #[tokio::test]
    async fn a_gateway_install_is_told_too() {
        with_isolated_config_dir_async(|| async {
            let now = at(2026, 11, 3, 8);
            let db = open_history();
            reading(&db, now.timestamp() - 40 * DAY, 100);
            let state = state_with(
                channel_config(),
                Some(db.clone()),
                Some(DeviceType::Gateway),
            )
            .await;
            assert_eq!(check(&state, now, true).await.len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn waits_for_a_snapshot_and_the_history() {
        with_isolated_config_dir_async(|| async {
            let now = at(2026, 11, 3, 8);
            let db = open_history();
            reading(&db, now.timestamp() - 40 * DAY, 100);
            let no_snapshot = state_with(channel_config(), Some(db.clone()), None).await;
            assert!(check(&no_snapshot, now, true).await.is_empty());
            assert!(!check_pending(&no_snapshot, now).await, "retries later");

            let no_history =
                state_with(channel_config(), None, Some(DeviceType::AllInOne6kW)).await;
            assert!(check(&no_history, now, true).await.is_empty());
            assert!(!check_pending(&no_history, now).await, "retries later");
        })
        .await;
    }

    #[tokio::test]
    async fn nothing_runs_while_it_is_off_or_has_nowhere_to_go() {
        with_isolated_config_dir_async(|| async {
            let now = at(2026, 11, 3, 8);
            let db = open_history();
            reading(&db, now.timestamp() - 40 * DAY, 100);
            let off = AlertsConfig {
                full_charge_alert_enabled: false,
                ..channel_config()
            };
            let state = state_with(off, Some(db.clone()), Some(DeviceType::AllInOne6kW)).await;
            assert!(!check_pending(&state, now).await);
            assert!(check(&state, now, true).await.is_empty());
            *state.alert_config.lock().await = AlertsConfig {
                ntfy_topic: String::new(),
                ..channel_config()
            };
            assert!(!check_pending(&state, now).await);
            assert_eq!(stored(&db), None);
        })
        .await;
    }

    #[tokio::test]
    async fn the_interval_follows_the_setting() {
        with_isolated_config_dir_async(|| async {
            let now = at(2026, 11, 3, 8);
            let db = open_history();
            reading(&db, now.timestamp() - 10 * DAY, 100);
            let weekly = AlertsConfig {
                full_charge_alert_days: 7,
                ..channel_config()
            };
            let state = state_with(weekly, Some(db.clone()), Some(DeviceType::AllInOne6kW)).await;
            assert_eq!(check(&state, now, true).await.len(), 1);
        })
        .await;
    }
}
