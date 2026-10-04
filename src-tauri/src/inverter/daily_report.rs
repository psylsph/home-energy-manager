//! The scheduled daily consumption report (Telegram).
//!
//! Once a day, after the configured time, the report for the previous day is
//! built from the history database and sent. The poll loop calls
//! [`run_daily_report`] every cycle; this module decides whether it is due
//! ([`daily_report_due`], pure), builds the report ([`build_daily_report`]) and
//! hands it to a sender. The sender is a parameter so tests never touch the
//! network.

use std::sync::Arc;

use chrono::{DateTime, Duration, Local, NaiveDate, Timelike};

use crate::history::HistoryDb;
use crate::inverter::poll::AppState;
use crate::settings::{AlertsConfig, Settings};

/// A built report, ready to send.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DailyReport {
    /// Plain summary shown above the attachment. Uses `<b>`/`<i>` tags on
    /// purpose, so it is sent with HTML parse mode.
    pub caption: String,
    pub filename: String,
    /// The full HTML report.
    pub body: String,
}

/// The date to report on, if a report is due now.
///
/// Due once the local time has reached the configured hour and minute and
/// nothing has been sent today. The report covers the previous calendar day.
pub(crate) fn daily_report_due(
    now: DateTime<Local>,
    last_sent: Option<NaiveDate>,
    hour: u8,
    minute: u8,
) -> Option<NaiveDate> {
    let today = now.date_naive();
    // No baseline yet (the app just started): never due. The caller records
    // today as the baseline (see `run_daily_report`), so a restart does not
    // fire a report but tomorrow's still goes out.
    let sent_date = last_sent?;
    if sent_date >= today {
        return None;
    }
    let minutes_since_midnight = now.hour() * 60 + now.minute();
    let send_minutes = hour as u32 * 60 + minute as u32;
    if minutes_since_midnight < send_minutes {
        return None;
    }
    Some(today.checked_sub_signed(Duration::days(1)).unwrap_or(today))
}

/// The `last_report_date` to record the first time the report is seen enabled.
pub(crate) fn first_cycle_baseline(now: DateTime<Local>, _hour: u8, _minute: u8) -> NaiveDate {
    now.date_naive()
}

/// Build the report for `report_date` from stored readings. `Ok(None)` means
/// there was too little data for a report. Blocking (SQLite).
pub(crate) fn build_daily_report(
    db: &HistoryDb,
    report_date: NaiveDate,
    settings: &Settings,
) -> Result<Option<DailyReport>, String> {
    let rows = db.get_readings_for_date(report_date)?;
    let date_str = report_date.format("%A %d %B %Y").to_string();
    let Some(body) = crate::alerts::report::generate_daily_report_html(&rows, &date_str) else {
        return Ok(None);
    };
    let caption = crate::alerts::report::generate_daily_summary_text(&rows, &date_str, settings)
        .unwrap_or_default();
    Ok(Some(DailyReport {
        caption,
        filename: format!("hem-report-{report_date}.html"),
        body,
    }))
}

/// Send the report to Telegram on a blocking thread, logging the outcome.
pub(crate) fn send_daily_report_telegram(config: &AlertsConfig, report: DailyReport) {
    let token = config.telegram_bot_token.clone();
    let chat_id = config.telegram_chat_id.clone();
    tokio::task::spawn_blocking(move || {
        // The caption uses intentional <b>/<i> tags from
        // generate_daily_summary_text, so keep HTML parse_mode here (unlike the
        // support-bundle caption, which is plain text).
        match crate::alerts::send_telegram_document(
            &token,
            &chat_id,
            &report.caption,
            &report.filename,
            report.body.as_bytes(),
            Some("HTML"),
        ) {
            Ok(()) => tracing::warn!("Daily report sent"),
            Err(e) => tracing::warn!("Failed to send daily report: {e}"),
        }
    });
}

/// Run one poll cycle's worth of the daily report: if enabled and due, build
/// the previous day's report and pass it to `send`.
///
/// The first enabled cycle after start only records today as the baseline (no
/// report on startup); the first report is sent the following day.
///
/// `last_report_date` is advanced when a report was sent or when there was too
/// little data to make one. It is left alone when there is no history database
/// or the query failed, so those retry on a later cycle.
pub(crate) async fn run_daily_report(
    state: &Arc<AppState>,
    now: DateTime<Local>,
    send: impl FnOnce(&AlertsConfig, DailyReport),
) {
    let config = state.alert_config.lock().await.clone();
    if !(config.daily_report_enabled && config.enabled) {
        return;
    }
    let today = now.date_naive();
    let mut last_sent = state.last_report_date.lock().await;
    if last_sent.is_none() {
        // First enabled cycle since start: don't send on startup, but record
        // today as the baseline. Without this `last_report_date` would stay
        // None forever and the report would never be sent.
        *last_sent = Some(today);
        return;
    }
    let Some(report_date) = daily_report_due(
        now,
        *last_sent,
        config.daily_report_hour,
        config.daily_report_minute,
    ) else {
        return;
    };
    let Some(db) = state.history.lock().await.clone() else {
        return;
    };
    let report = tokio::task::spawn_blocking(move || {
        build_daily_report(&db, report_date, &Settings::load())
    })
    .await
    .map_err(|error| format!("daily report worker failed: {error}"))
    .and_then(|result| result);

    match report {
        Ok(Some(report)) => {
            send(&config, report);
            *last_sent = Some(today);
        }
        Ok(None) => {
            tracing::debug!("Daily report: insufficient data for {report_date}");
            *last_sent = Some(today);
        }
        Err(e) => {
            tracing::warn!("Failed to query history for daily report: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::InverterSnapshot;
    use crate::test_util::with_isolated_config_dir_async;
    use chrono::TimeZone;

    fn local_dt(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local
            .from_local_datetime(
                &NaiveDate::from_ymd_opt(y, m, d)
                    .unwrap()
                    .and_hms_opt(h, min, 0)
                    .unwrap(),
            )
            .earliest()
            .unwrap()
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    // ---- daily_report_due ----------------------------------------------

    #[test]
    fn a_report_is_due_once_the_send_time_has_passed() {
        let due = daily_report_due(local_dt(2026, 8, 31, 9, 0), Some(date(2026, 8, 30)), 8, 0);
        assert_eq!(due, Some(date(2026, 8, 30)), "reports on the previous day");
    }

    #[test]
    fn a_report_waits_until_the_send_time() {
        let sent = Some(date(2026, 8, 30));
        assert_eq!(
            daily_report_due(local_dt(2026, 8, 31, 7, 59), sent, 8, 0),
            None
        );
        assert_eq!(
            daily_report_due(local_dt(2026, 8, 31, 8, 0), sent, 8, 0),
            Some(date(2026, 8, 30)),
            "the configured minute itself is due"
        );
    }

    #[test]
    fn the_send_time_has_minute_resolution() {
        let sent = Some(date(2026, 8, 30));
        assert_eq!(
            daily_report_due(local_dt(2026, 8, 31, 8, 29), sent, 8, 30),
            None
        );
        assert!(daily_report_due(local_dt(2026, 8, 31, 8, 30), sent, 8, 30).is_some());
    }

    #[test]
    fn nothing_is_due_once_todays_report_has_gone() {
        let now = local_dt(2026, 8, 31, 20, 0);
        assert_eq!(daily_report_due(now, Some(date(2026, 8, 31)), 8, 0), None);
    }

    #[test]
    fn a_last_sent_date_in_the_future_never_fires() {
        // Clock stepped backwards: do not send a second report today.
        let now = local_dt(2026, 8, 31, 20, 0);
        assert_eq!(daily_report_due(now, Some(date(2026, 9, 2)), 8, 0), None);
    }

    #[test]
    fn without_a_baseline_nothing_is_due() {
        // Seeding the baseline is run_daily_report's job, not the due-check's.
        assert_eq!(
            daily_report_due(local_dt(2026, 8, 31, 9, 0), None, 8, 0),
            None
        );
    }

    #[test]
    fn a_missed_day_still_reports_only_yesterday() {
        // Offline for several days: one report, for yesterday, not a backlog.
        let due = daily_report_due(local_dt(2026, 8, 31, 9, 0), Some(date(2026, 8, 25)), 8, 0);
        assert_eq!(due, Some(date(2026, 8, 30)));
    }

    #[test]
    fn the_report_date_crosses_month_and_year_boundaries() {
        assert_eq!(
            daily_report_due(local_dt(2026, 9, 1, 9, 0), Some(date(2026, 8, 31)), 8, 0),
            Some(date(2026, 8, 31))
        );
        assert_eq!(
            daily_report_due(local_dt(2027, 1, 1, 9, 0), Some(date(2026, 12, 31)), 8, 0),
            Some(date(2026, 12, 31))
        );
    }

    // ---- build_daily_report / run_daily_report ----------------------------

    fn open_history() -> Arc<HistoryDb> {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("givenergy-daily-report-test-{id}/history.db"));
        Arc::new(HistoryDb::open(&path).unwrap())
    }

    /// Readings every five minutes from 06:00 to 18:00 local on `day`.
    fn insert_a_day(db: &HistoryDb, day: NaiveDate) {
        let start = Local
            .from_local_datetime(&day.and_hms_opt(6, 0, 0).unwrap())
            .earliest()
            .unwrap()
            .timestamp();
        for n in 0..=144 {
            let hour_fraction = n as f32 / 144.0;
            db.insert_reading(&InverterSnapshot {
                timestamp: start + n * 300,
                solar_power: (4000.0 * (hour_fraction * std::f32::consts::PI).sin()) as i32,
                home_power: 600,
                grid_power: -100,
                battery_power: 0,
                soc: 50,
                ..Default::default()
            });
        }
    }

    fn enabled_config() -> AlertsConfig {
        AlertsConfig {
            enabled: true,
            daily_report_enabled: true,
            daily_report_hour: 8,
            daily_report_minute: 0,
            ..Default::default()
        }
    }

    async fn state_with(config: AlertsConfig, last_sent: Option<NaiveDate>) -> Arc<AppState> {
        let state = Arc::new(AppState::new());
        *state.alert_config.lock().await = config;
        *state.last_report_date.lock().await = last_sent;
        state
    }

    type Sent = Arc<std::sync::Mutex<Vec<DailyReport>>>;

    async fn run(state: &Arc<AppState>, now: DateTime<Local>) -> Sent {
        let sent: Sent = Arc::default();
        let log = sent.clone();
        run_daily_report(state, now, move |_cfg, report| {
            log.lock().unwrap().push(report)
        })
        .await;
        sent
    }

    #[test]
    fn a_day_with_readings_builds_a_report() {
        let db = open_history();
        let day = date(2026, 8, 30);
        insert_a_day(&db, day);
        let report = build_daily_report(&db, day, &Settings::default())
            .unwrap()
            .expect("a full day of readings should build a report");
        assert_eq!(report.filename, "hem-report-2026-08-30.html");
        assert!(
            report.body.contains("Sunday 30 August 2026"),
            "{}",
            &report.body[..200.min(report.body.len())]
        );
        assert!(!report.caption.is_empty());
    }

    #[test]
    fn a_day_without_enough_readings_builds_nothing() {
        let db = open_history();
        assert_eq!(
            build_daily_report(&db, date(2026, 8, 30), &Settings::default()).unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn a_due_report_is_built_and_sent_once() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(enabled_config(), Some(date(2026, 8, 30))).await;
            let db = open_history();
            insert_a_day(&db, date(2026, 8, 30));
            *state.history.lock().await = Some(db);
            let now = local_dt(2026, 8, 31, 9, 0);

            let sent = run(&state, now).await;
            let filenames: Vec<String> = sent
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.filename.clone())
                .collect();
            assert_eq!(filenames, vec!["hem-report-2026-08-30.html".to_string()]);
            assert_eq!(
                *state.last_report_date.lock().await,
                Some(date(2026, 8, 31))
            );

            // The next cycle the same day must not send it again.
            let again = run(&state, local_dt(2026, 8, 31, 9, 0)).await;
            assert!(again.lock().unwrap().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn too_little_data_skips_the_report_but_still_advances_the_day() {
        // Otherwise an empty day would be re-queried on every poll.
        with_isolated_config_dir_async(|| async {
            let state = state_with(enabled_config(), Some(date(2026, 8, 30))).await;
            *state.history.lock().await = Some(open_history());
            let sent = run(&state, local_dt(2026, 8, 31, 9, 0)).await;
            assert!(sent.lock().unwrap().is_empty());
            assert_eq!(
                *state.last_report_date.lock().await,
                Some(date(2026, 8, 31))
            );
        })
        .await;
    }

    #[tokio::test]
    async fn without_a_history_database_nothing_is_sent_and_it_retries_later() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(enabled_config(), Some(date(2026, 8, 30))).await;
            let sent = run(&state, local_dt(2026, 8, 31, 9, 0)).await;
            assert!(sent.lock().unwrap().is_empty());
            assert_eq!(
                *state.last_report_date.lock().await,
                Some(date(2026, 8, 30)),
                "the day must stay open for a retry"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn nothing_is_sent_before_the_send_time() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(enabled_config(), Some(date(2026, 8, 30))).await;
            let db = open_history();
            insert_a_day(&db, date(2026, 8, 30));
            *state.history.lock().await = Some(db);
            let sent = run(&state, local_dt(2026, 8, 31, 7, 0)).await;
            assert!(sent.lock().unwrap().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_disabled_report_is_never_built_or_sent() {
        for (daily, master) in [(false, true), (true, false), (false, false)] {
            with_isolated_config_dir_async(|| async move {
                let mut config = enabled_config();
                config.daily_report_enabled = daily;
                config.enabled = master;
                let state = state_with(config, Some(date(2026, 8, 30))).await;
                let db = open_history();
                insert_a_day(&db, date(2026, 8, 30));
                *state.history.lock().await = Some(db);
                let sent = run(&state, local_dt(2026, 8, 31, 9, 0)).await;
                assert!(
                    sent.lock().unwrap().is_empty(),
                    "daily={daily} master={master}"
                );
                assert_eq!(
                    *state.last_report_date.lock().await,
                    None,
                    "a disabled report forgets its baseline (daily={daily} master={master})"
                );
            })
            .await;
        }
    }

    // ---- the report must actually start ----------------------------------

    #[tokio::test]
    async fn a_fresh_start_does_not_send_but_arms_tomorrows_report() {
        // Starting the app must not fire a report on every restart, but it must
        // record today as the baseline so tomorrow's report can go out.
        with_isolated_config_dir_async(|| async {
            let state = state_with(enabled_config(), None).await;
            let db = open_history();
            insert_a_day(&db, date(2026, 8, 30));
            *state.history.lock().await = Some(db);

            let sent = run(&state, local_dt(2026, 8, 31, 9, 0)).await;
            assert!(sent.lock().unwrap().is_empty(), "no report on startup");
            assert_eq!(
                *state.last_report_date.lock().await,
                Some(date(2026, 8, 31)),
                "today must be recorded as the baseline"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn the_first_report_goes_out_the_morning_after_a_fresh_start() {
        // Regression: last_report_date starts as None and was only ever set
        // inside the branch that required it to already be set, so the
        // scheduled report never sent at all.
        with_isolated_config_dir_async(|| async {
            let state = state_with(enabled_config(), None).await;
            let db = open_history();
            insert_a_day(&db, date(2026, 8, 31));
            *state.history.lock().await = Some(db);

            // Day 1: the app starts after the send time. Nothing goes out.
            let day1 = run(&state, local_dt(2026, 8, 31, 9, 0)).await;
            assert!(day1.lock().unwrap().is_empty());

            // Day 2, before the send time: still nothing.
            let early = run(&state, local_dt(2026, 9, 1, 7, 0)).await;
            assert!(early.lock().unwrap().is_empty());

            // Day 2, after the send time: yesterday's report is sent.
            let day2 = run(&state, local_dt(2026, 9, 1, 9, 0)).await;
            let filenames: Vec<String> = day2
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.filename.clone())
                .collect();
            assert_eq!(filenames, vec!["hem-report-2026-08-31.html".to_string()]);
            assert_eq!(*state.last_report_date.lock().await, Some(date(2026, 9, 1)));
        })
        .await;
    }

    #[tokio::test]
    async fn a_disabled_report_does_not_arm_the_baseline() {
        // Only an enabled report records a baseline, so enabling it later starts
        // from that moment rather than inheriting a stale date.
        with_isolated_config_dir_async(|| async {
            let mut config = enabled_config();
            config.daily_report_enabled = false;
            let state = state_with(config, None).await;
            run(&state, local_dt(2026, 8, 31, 9, 0)).await;
            assert_eq!(*state.last_report_date.lock().await, None);
        })
        .await;
    }

    // ---- baseline when the report is first enabled ------------------------

    #[test]
    fn the_baseline_is_yesterday_before_the_send_time_and_today_after() {
        // Before the send time today's report is still to come, so yesterday is
        // the baseline; at or after it, today's has gone (or is being skipped).
        let day = date(2026, 8, 31);
        assert_eq!(
            first_cycle_baseline(local_dt(2026, 8, 31, 6, 0), 8, 0),
            date(2026, 8, 30)
        );
        assert_eq!(
            first_cycle_baseline(local_dt(2026, 8, 31, 7, 59), 8, 0),
            date(2026, 8, 30)
        );
        assert_eq!(first_cycle_baseline(local_dt(2026, 8, 31, 8, 0), 8, 0), day);
        assert_eq!(
            first_cycle_baseline(local_dt(2026, 8, 31, 20, 0), 8, 0),
            day
        );
    }

    #[test]
    fn the_baseline_crosses_a_month_boundary() {
        assert_eq!(
            first_cycle_baseline(local_dt(2026, 9, 1, 6, 0), 8, 0),
            date(2026, 8, 31)
        );
    }

    #[tokio::test]
    async fn enabling_the_report_before_the_send_time_still_sends_this_morning() {
        with_isolated_config_dir_async(|| async {
            let state = state_with(enabled_config(), None).await;
            let db = open_history();
            insert_a_day(&db, date(2026, 8, 30));
            *state.history.lock().await = Some(db);

            // 06:00: enabled, nothing sent yet.
            let early = run(&state, local_dt(2026, 8, 31, 6, 0)).await;
            assert!(early.lock().unwrap().is_empty());
            assert_eq!(
                *state.last_report_date.lock().await,
                Some(date(2026, 8, 30))
            );

            // 09:00: yesterday's report goes out, as it would have anyway.
            let later = run(&state, local_dt(2026, 8, 31, 9, 0)).await;
            let names: Vec<String> = later
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.filename.clone())
                .collect();
            assert_eq!(names, vec!["hem-report-2026-08-30.html".to_string()]);
        })
        .await;
    }

    #[tokio::test]
    async fn disabling_the_report_forgets_the_baseline_so_re_enabling_does_not_fire_at_once() {
        with_isolated_config_dir_async(|| async {
            // Sent on the 25th, then disabled for days.
            let mut config = enabled_config();
            config.daily_report_enabled = false;
            let state = state_with(config, Some(date(2026, 8, 25))).await;
            let db = open_history();
            insert_a_day(&db, date(2026, 8, 30));
            *state.history.lock().await = Some(db);
            run(&state, local_dt(2026, 8, 31, 9, 0)).await;
            assert_eq!(
                *state.last_report_date.lock().await,
                None,
                "disabled: baseline forgotten"
            );

            // Re-enabled after the send time: baseline today, nothing sent at once.
            *state.alert_config.lock().await = enabled_config();
            let sent = run(&state, local_dt(2026, 8, 31, 15, 0)).await;
            assert!(sent.lock().unwrap().is_empty());
            assert_eq!(
                *state.last_report_date.lock().await,
                Some(date(2026, 8, 31))
            );
        })
        .await;
    }
}
