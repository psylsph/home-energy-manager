//! Background jobs that change the rates stamped on history rows (issue #455):
//! re-pricing History from a date with the current tariff, and filling the
//! rates rows lack from the supplier's dated Octopus rates.
//!
//! One job runs at a time. It works in chunks on the blocking pool (see
//! `history::reprice`), reports progress to the Settings page through
//! `GET /api/history/reprice`, and runs to completion on the server whether or
//! not the client that started it stays connected.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value};

use crate::history::reprice::{job_end, CHUNK_ROWS};
use crate::inverter::poll::AppState;
use crate::settings::Settings;

/// `meta` key holding where an interrupted backfill resumes.
pub const BACKFILL_CURSOR_KEY: &str = "tariff_backfill_cursor";

/// Pause between chunks, so the poll loop's history writes get the lock.
const CHUNK_PAUSE: Duration = Duration::from_millis(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    /// Rewrite the rates of every row from a date with the current tariff.
    Reprice,
    /// Fill missing rates from the supplier's dated rates.
    Backfill,
}

/// How the latest job ended.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobRecord {
    pub kind: JobKind,
    /// Unix seconds when it ended.
    pub at: i64,
    pub ok: bool,
    /// History rows it looked at.
    pub rows: u64,
    pub error: Option<String>,
}

/// Shared between the job task and the HTTP handlers via
/// [`AppState::rates_job`].
#[derive(Debug, Default)]
pub struct RatesJobState {
    /// A job is in progress; a second request is refused.
    pub running: bool,
    pub kind: Option<JobKind>,
    pub done: u64,
    pub total: u64,
    pub last: Option<JobRecord>,
    /// Signalled each time a job ends, so tests can await one without polling
    /// a clock.
    pub finished: Arc<tokio::sync::Notify>,
}

impl RatesJobState {
    fn status(&self) -> Value {
        json!({
            "running": self.running,
            "kind": self.kind,
            "done": self.done,
            "total": self.total,
            "last": self.last,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StartError {
    /// Another job is running.
    Busy,
    /// A history backup is running; a re-price mid-backup would be copied
    /// half-done.
    BackupRunning,
    /// There is no history database.
    NoHistory,
}

impl StartError {
    fn message(&self) -> &'static str {
        match self {
            StartError::Busy => "A rates job is already running",
            StartError::BackupRunning => "A history backup is running; try again when it finishes",
            StartError::NoHistory => "History is not available",
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            StartError::Busy | StartError::BackupRunning => StatusCode::CONFLICT,
            StartError::NoHistory => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

/// A claimed job. Released explicitly when it ends, or on drop if anything in
/// between panicked, so a bug can never block every later job.
struct Claim {
    state: Option<Arc<AppState>>,
}

impl Claim {
    async fn release_state(state: &Arc<AppState>) {
        let mut guard = state.rates_job.lock().await;
        guard.running = false;
        guard.kind = None;
        guard.finished.notify_one();
    }

    async fn release(mut self) {
        if let Some(state) = self.state.take() {
            Self::release_state(&state).await;
        }
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move { Self::release_state(&state).await });
            }
        }
    }
}

/// Claim the job slot, refusing while another job or a backup runs.
async fn claim(state: &Arc<AppState>, kind: JobKind) -> Result<Claim, StartError> {
    if state.history.lock().await.is_none() {
        return Err(StartError::NoHistory);
    }
    // Both are held while deciding, in the order `backup::try_claim` takes
    // them, so a backup and a job can never both be let in.
    let backup = state.backup.lock().await;
    if backup.running {
        return Err(StartError::BackupRunning);
    }
    let mut guard = state.rates_job.lock().await;
    if guard.running {
        return Err(StartError::Busy);
    }
    guard.running = true;
    guard.kind = Some(kind);
    guard.done = 0;
    guard.total = 0;
    drop(guard);
    drop(backup);
    Ok(Claim::new(state.clone()))
}

impl Claim {
    fn new(state: Arc<AppState>) -> Self {
        Self { state: Some(state) }
    }
}

/// Start a job in the background. The slot is claimed before this returns, so
/// a status fetch straight after sees it running. `from_ts` is where a
/// re-price starts (Unix seconds); a backfill ignores it and resumes from its
/// saved cursor.
pub async fn start(
    state: &Arc<AppState>,
    kind: JobKind,
    from_ts: i64,
    now_ts: i64,
) -> Result<(), StartError> {
    let claim = claim(state, kind).await?;
    let state = state.clone();
    tokio::spawn(async move {
        let result = run(&state, kind, from_ts, now_ts).await;
        let (rows, error) = match result {
            Ok(rows) => (rows, None),
            Err(error) => {
                tracing::warn!("Rates job ({kind:?}) failed: {error}");
                (state.rates_job.lock().await.done, Some(error))
            }
        };
        {
            let mut guard = state.rates_job.lock().await;
            guard.last = Some(JobRecord {
                kind,
                at: chrono::Utc::now().timestamp(),
                ok: error.is_none(),
                rows,
                error,
            });
        }
        claim.release().await;
    });
    Ok(())
}

/// The job itself: chunk after chunk until the range is done. Returns the
/// number of rows looked at.
async fn run(
    state: &Arc<AppState>,
    kind: JobKind,
    from_ts: i64,
    now_ts: i64,
) -> Result<u64, String> {
    let db = state
        .history
        .lock()
        .await
        .clone()
        .ok_or_else(|| StartError::NoHistory.message().to_string())?;
    let end = job_end(now_ts);
    let settings = Settings::load_async().await;

    // The smart charges that ran in the range, from the dispatch log (issue
    // #434): their half-hours stay at the off-peak rate the live writer
    // stamped. The log is never pruned, so it holds every smart charge the
    // live writer stamped.
    let smart = match kind {
        JobKind::Reprice => {
            let db2 = db.clone();
            tokio::task::spawn_blocking(move || logged_smart_half_hours(&db2, from_ts))
                .await
                .map_err(|e| format!("Rates job task failed: {e}"))??
        }
        JobKind::Backfill => crate::history::OffPeakHalfHours::default(),
    };
    let smart = Arc::new(smart);

    let (mut cursor, book) = match kind {
        JobKind::Reprice => (from_ts, None),
        JobKind::Backfill => {
            let resume = db
                .get_meta_value(BACKFILL_CURSOR_KEY)
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            let (night_start, night_end) = crate::octopus_tariff::economy7_window(&settings);
            let db2 = db.clone();
            let book =
                tokio::task::spawn_blocking(move || db2.load_price_book(night_start, night_end))
                    .await
                    .map_err(|e| format!("Rates job task failed: {e}"))??;
            (resume, Some(Arc::new(book)))
        }
    };

    let total = {
        let db = db.clone();
        let start = cursor;
        tokio::task::spawn_blocking(move || match kind {
            // A backfill only visits rows lacking a rate, so that is its total.
            JobKind::Backfill => db.count_rows_missing_rates_between(start, end),
            JobKind::Reprice => db.count_rows_between(start, end),
        })
        .await
        .map_err(|e| format!("Rates job task failed: {e}"))??
    };
    state.rates_job.lock().await.total = total;

    let settings = Arc::new(settings);
    let mut looked_at = 0_u64;
    loop {
        let outcome = {
            let db = db.clone();
            let settings = settings.clone();
            let book = book.clone();
            let smart = smart.clone();
            let from = cursor;
            tokio::task::spawn_blocking(move || match book {
                Some(book) => db.backfill_chunk(&book, from, end, CHUNK_ROWS),
                None => db.reprice_chunk(&settings, &smart, from, end, CHUNK_ROWS),
            })
            .await
            .map_err(|e| format!("Rates job task failed: {e}"))??
        };
        looked_at += outcome.rows as u64;
        state.rates_job.lock().await.done = looked_at;
        match outcome.next {
            Some(next) => {
                cursor = next;
                if kind == JobKind::Backfill {
                    // Saved after the chunk commits, so a restart resumes at a
                    // chunk boundary; rows already filled are skipped anyway.
                    db.set_meta_value(BACKFILL_CURSOR_KEY, &cursor.to_string())?;
                }
                tokio::time::sleep(CHUNK_PAUSE).await;
            }
            None => break,
        }
    }
    if kind == JobKind::Backfill {
        // Finished: the next run starts from the beginning, so rows the
        // supplier's later rates now cover get picked up.
        db.set_meta_value(BACKFILL_CURSOR_KEY, "")?;
    }
    Ok(looked_at)
}

/// The half-hours of the smart charges the dispatch log says ran after
/// `from_ts`. Boosts are billed as normal and left out.
fn logged_smart_half_hours(
    db: &crate::history::HistoryDb,
    from_ts: i64,
) -> Result<crate::history::OffPeakHalfHours, String> {
    let logged =
        db.logged_dispatches_since(from_ts, crate::octopus_dispatches::LOG_SEEN_GAP_SECS)?;
    Ok(crate::history::OffPeakHalfHours::from_spans(
        logged
            .iter()
            .filter(|d| crate::octopus_dispatches::is_smart_kind(d.kind.as_deref()))
            .map(|d| (d.start, d.end)),
    ))
}

fn error_response(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"ok": false, "error": message})))
}

fn accepted() -> (StatusCode, Json<Value>) {
    (
        StatusCode::ACCEPTED,
        Json(json!({"ok": true, "data": {"running": true}})),
    )
}

/// `GET /api/history/reprice` — the running (or latest) rates job.
pub async fn get_reprice(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let guard = state.rates_job.lock().await;
    (
        StatusCode::OK,
        Json(json!({"ok": true, "data": guard.status()})),
    )
}

/// `POST /api/history/reprice` with `{"from": "YYYY-MM-DD"}` — rewrite the
/// rates stamped on every History row from that local date with the tariff
/// saved now. Never runs unless asked; it is the only thing that changes a
/// recorded rate.
pub async fn post_reprice(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let Some(from) = body.get("from").and_then(Value::as_str) else {
        return error_response(StatusCode::BAD_REQUEST, "A start date is required");
    };
    let Ok(date) = chrono::NaiveDate::parse_from_str(from, "%Y-%m-%d") else {
        return error_response(StatusCode::BAD_REQUEST, "The start date must be YYYY-MM-DD");
    };
    let from_ts = match crate::history::local_midnight_timestamp_in(date, &chrono::Local) {
        Ok(ts) => ts,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error),
    };
    let now_ts = chrono::Utc::now().timestamp();
    if from_ts >= job_end(now_ts) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "Choose a date that has already started",
        );
    }
    match start(&state, JobKind::Reprice, from_ts, now_ts).await {
        Ok(()) => accepted(),
        Err(error) => error_response(error.status(), error.message()),
    }
}

/// `POST /api/octopus/backfill-rates` — fill the rates History rows lack from
/// the Octopus rates HEM has downloaded. Changes only rows with no recorded
/// rate.
pub async fn post_backfill(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    match start(&state, JobKind::Backfill, 0, chrono::Utc::now().timestamp()).await {
        Ok(()) => accepted(),
        Err(error) => error_response(error.status(), error.message()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::HistoryDb;
    use crate::inverter::model::InverterSnapshot;

    /// 2026-10-21 12:00:00 UTC.
    const NOON: i64 = 1_792_584_000;

    async fn state_with_rows(n: i64) -> Arc<AppState> {
        let dir = Settings::settings_dir();
        let db = HistoryDb::open(&dir.join("history.db")).unwrap();
        for i in 0..n {
            db.insert_reading(&InverterSnapshot {
                timestamp: NOON + i * 60,
                soc: 50,
                ..InverterSnapshot::default()
            });
        }
        let state = Arc::new(AppState::new());
        *state.history.lock().await = Some(Arc::new(db));
        state
    }

    async fn finished(state: &Arc<AppState>) -> JobRecord {
        let finished = state.rates_job.lock().await.finished.clone();
        tokio::time::timeout(Duration::from_secs(60), finished.notified())
            .await
            .expect("job finished");
        let guard = state.rates_job.lock().await;
        assert!(!guard.running);
        guard.last.clone().expect("a result")
    }

    fn history_of(state: &Arc<AppState>) -> Arc<HistoryDb> {
        state
            .history
            .try_lock()
            .expect("history lock free")
            .clone()
            .expect("history")
    }

    fn import_rates(state: &Arc<AppState>) -> Vec<Option<f64>> {
        history_of(state).import_rates_for_test()
    }

    #[tokio::test]
    async fn a_reprice_rewrites_rows_from_the_date_with_the_saved_tariff() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            Settings::update(|s| {
                s.import_tariff = 0.42;
                s.import_tariff_config = None;
            })
            .unwrap();
            let state = state_with_rows(5).await;
            // The clock is an hour after the last row: all five are in range.
            start(&state, JobKind::Reprice, NOON, NOON + 3600)
                .await
                .unwrap();
            let record = finished(&state).await;
            assert!(record.ok, "{record:?}");
            assert_eq!(record.kind, JobKind::Reprice);
            assert_eq!(record.rows, 5);
            assert_eq!(import_rates(&state), vec![Some(0.42); 5]);
        })
        .await;
    }

    #[tokio::test]
    async fn a_reprice_keeps_the_logged_smart_charges_at_the_off_peak_rate() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let slot = |start: &str, end: &str, rate: f64| crate::settings::TariffSlot {
                start: start.into(),
                end: end.into(),
                rate,
            };
            Settings::update(|s| {
                s.import_tariff_config = Some(crate::settings::TariffConfig {
                    slots: vec![slot("00:00", "05:30", 0.07), slot("05:30", "23:59", 0.30)],
                });
            })
            .unwrap();
            // Rows 12:00–12:39; a smart charge ran 12:05–12:20 and a boost
            // 12:35–12:50. Only the smart one's half-hour is off-peak.
            let state = state_with_rows(40).await;
            let sighting =
                |start: i64, end: i64, kind: &str| crate::history::dispatch_log::DispatchSighting {
                    device: "car".into(),
                    start,
                    end,
                    kind: Some(kind.into()),
                    energy_kwh: None,
                };
            history_of(&state)
                .log_dispatches(
                    &[
                        sighting(NOON + 300, NOON + 1200, "SMART"),
                        sighting(NOON + 2100, NOON + 3000, "BOOST"),
                    ],
                    NOON + 2200,
                )
                .unwrap();
            start(&state, JobKind::Reprice, NOON, NOON + 3600 * 2)
                .await
                .unwrap();
            let record = finished(&state).await;
            assert!(record.ok, "{record:?}");
            let rates = import_rates(&state);
            assert_eq!(rates[..30], vec![Some(0.07); 30]);
            assert_eq!(rates[30..], vec![Some(0.30); 10]);
        })
        .await;
    }

    #[tokio::test]
    async fn a_reprice_leaves_the_minutes_the_live_writer_may_rewrite() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            Settings::update(|s| {
                s.import_tariff = 0.42;
                s.import_tariff_config = None;
            })
            .unwrap();
            let state = state_with_rows(10).await;
            // Now is 12:07: rows from 12:03 on are within the live margin.
            start(&state, JobKind::Reprice, NOON, NOON + 7 * 60)
                .await
                .unwrap();
            let record = finished(&state).await;
            assert_eq!(record.rows, 3, "12:00, 12:01 and 12:02 only");
            let rates = import_rates(&state);
            assert_eq!(&rates[..3], &[Some(0.42); 3]);
            assert!(rates[3..].iter().all(Option::is_none));
        })
        .await;
    }

    #[tokio::test]
    async fn a_second_job_is_refused_while_one_runs() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rows(1).await;
            // Hold the slot as a running job would.
            state.rates_job.lock().await.running = true;
            let error = start(&state, JobKind::Reprice, NOON, NOON + 3600)
                .await
                .unwrap_err();
            assert_eq!(error, StartError::Busy);
            assert_eq!(error.status(), StatusCode::CONFLICT);
            let error = start(&state, JobKind::Backfill, 0, NOON + 3600)
                .await
                .unwrap_err();
            assert_eq!(error, StartError::Busy);
        })
        .await;
    }

    #[tokio::test]
    async fn a_job_is_refused_while_a_backup_runs() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rows(1).await;
            state.backup.lock().await.running = true;
            let error = start(&state, JobKind::Reprice, NOON, NOON + 3600)
                .await
                .unwrap_err();
            assert_eq!(error, StartError::BackupRunning);
            assert!(
                !state.rates_job.lock().await.running,
                "a refused job must not hold the slot"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_job_without_a_history_database_is_refused() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let error = start(&state, JobKind::Reprice, NOON, NOON + 3600)
                .await
                .unwrap_err();
            assert_eq!(error, StartError::NoHistory);
        })
        .await;
    }

    #[tokio::test]
    async fn the_slot_is_free_again_after_a_job_so_the_next_can_run() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rows(2).await;
            start(&state, JobKind::Reprice, NOON, NOON + 3600)
                .await
                .unwrap();
            finished(&state).await;
            start(&state, JobKind::Reprice, NOON, NOON + 3600)
                .await
                .unwrap();
            let second = finished(&state).await;
            assert!(second.ok);
        })
        .await;
    }

    #[tokio::test]
    async fn a_failed_job_is_recorded_and_frees_the_slot() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rows(2).await;
            // Poison the history connection: the first chunk fails.
            let db = history_of(&state);
            let poisoner = db.clone();
            let _ = std::thread::spawn(move || poisoner.poison_connection_for_test()).join();
            start(&state, JobKind::Reprice, NOON, NOON + 3600)
                .await
                .unwrap();
            let record = finished(&state).await;
            assert!(!record.ok);
            assert!(record.error.as_deref().unwrap_or("").contains("poisoned"));
            assert!(!state.rates_job.lock().await.running);
        })
        .await;
    }

    #[tokio::test]
    async fn a_backfill_fills_missing_rates_from_the_downloaded_octopus_rates() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rows(3).await;
            let db = history_of(&state);
            db.upsert_octopus_tariff_prices(&[crate::history::OctopusTariffPriceRow {
                meter_kind: "electricity_import".into(),
                meter_point: "mp1".into(),
                tariff_code: "E-1R-X".into(),
                valid_from: 0,
                valid_to: None,
                value_inc_vat: 24.5,
                rate_type: "standard".into(),
            }])
            .unwrap();
            drop(db);
            start(&state, JobKind::Backfill, 0, NOON + 3600)
                .await
                .unwrap();
            let record = finished(&state).await;
            assert!(record.ok, "{record:?}");
            assert_eq!(record.kind, JobKind::Backfill);
            assert_eq!(import_rates(&state), vec![Some(0.245); 3]);
            let db = history_of(&state);
            assert_eq!(
                db.get_meta_value(BACKFILL_CURSOR_KEY).as_deref(),
                Some(""),
                "a finished run leaves no cursor"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn backfill_progress_counts_only_the_rows_that_need_filling() {
        // Rows that already have every rate are never visited, so the total
        // must not include them or the progress could never reach it.
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rows(6).await;
            let db = history_of(&state);
            db.upsert_octopus_tariff_prices(&[crate::history::OctopusTariffPriceRow {
                meter_kind: "electricity_import".into(),
                meter_point: "mp1".into(),
                tariff_code: "E-1R-X".into(),
                valid_from: 0,
                valid_to: None,
                value_inc_vat: 20.0,
                rate_type: "standard".into(),
            }])
            .unwrap();
            // Four rows are fully recorded already.
            let recorded = Settings {
                import_tariff: 0.5,
                export_tariff: 0.1,
                import_standing_charge_p_per_day: 10.0,
                import_tariff_config: None,
                export_tariff_config: None,
                ..Settings::default()
            };
            db.reprice_chunk(
                &recorded,
                &crate::history::OffPeakHalfHours::default(),
                NOON,
                NOON + 4 * 60,
                100,
            )
            .unwrap();
            drop(db);
            start(&state, JobKind::Backfill, 0, NOON + 3600)
                .await
                .unwrap();
            let record = finished(&state).await;
            assert_eq!(record.rows, 2);
            assert_eq!(
                state.rates_job.lock().await.total,
                2,
                "only the rows still missing a rate"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn an_interrupted_backfill_resumes_from_its_saved_cursor() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rows(4).await;
            let db = history_of(&state);
            db.upsert_octopus_tariff_prices(&[crate::history::OctopusTariffPriceRow {
                meter_kind: "electricity_import".into(),
                meter_point: "mp1".into(),
                tariff_code: "E-1R-X".into(),
                valid_from: 0,
                valid_to: None,
                value_inc_vat: 20.0,
                rate_type: "standard".into(),
            }])
            .unwrap();
            // A previous run got as far as the third row.
            db.set_meta_value(BACKFILL_CURSOR_KEY, &(NOON + 2 * 60).to_string())
                .unwrap();
            drop(db);
            start(&state, JobKind::Backfill, 0, NOON + 3600)
                .await
                .unwrap();
            let record = finished(&state).await;
            assert_eq!(record.rows, 2, "only the rows from the cursor on");
            assert_eq!(
                import_rates(&state),
                vec![None, None, Some(0.20), Some(0.20)]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_backfill_with_no_downloaded_rates_succeeds_and_changes_nothing() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rows(3).await;
            start(&state, JobKind::Backfill, 0, NOON + 3600)
                .await
                .unwrap();
            let record = finished(&state).await;
            assert!(record.ok);
            assert!(import_rates(&state).iter().all(Option::is_none));
        })
        .await;
    }

    #[tokio::test]
    async fn the_status_reports_progress_and_the_last_result() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rows(2).await;
            let (status, Json(body)) = get_reprice(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["data"]["running"], false);
            assert!(body["data"]["last"].is_null());

            start(&state, JobKind::Reprice, NOON, NOON + 3600)
                .await
                .unwrap();
            finished(&state).await;
            let (_, Json(body)) = get_reprice(State(state.clone())).await;
            assert_eq!(body["data"]["running"], false);
            assert_eq!(body["data"]["last"]["kind"], "reprice");
            assert_eq!(body["data"]["last"]["ok"], true);
            assert_eq!(body["data"]["last"]["rows"], 2);
            assert_eq!(body["data"]["total"], 2);
        })
        .await;
    }

    #[tokio::test]
    async fn the_reprice_endpoint_rejects_a_missing_or_bad_or_future_date() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rows(1).await;
            for (body, why) in [
                (json!({}), "missing"),
                (json!({"from": 5}), "not a string"),
                (json!({"from": "yesterday"}), "not a date"),
                (json!({"from": "2026-13-45"}), "impossible date"),
                (json!({"from": "2999-01-01"}), "in the future"),
            ] {
                let (status, Json(reply)) = post_reprice(State(state.clone()), Json(body)).await;
                assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {reply}");
                assert_eq!(reply["ok"], false);
            }
            assert!(
                !state.rates_job.lock().await.running,
                "a rejected request must not start a job"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn the_reprice_endpoint_accepts_a_past_date_and_runs_the_job() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rows(1).await;
            let (status, Json(reply)) =
                post_reprice(State(state.clone()), Json(json!({"from": "2026-10-01"}))).await;
            assert_eq!(status, StatusCode::ACCEPTED, "{reply}");
            assert_eq!(reply["data"]["running"], true);
            let record = finished(&state).await;
            assert!(record.ok);
            let (status, _) =
                post_reprice(State(state.clone()), Json(json!({"from": "2026-10-01"}))).await;
            assert_eq!(status, StatusCode::ACCEPTED, "the slot was released");
            finished(&state).await;
        })
        .await;
    }

    #[tokio::test]
    async fn the_endpoints_answer_conflict_while_a_job_runs() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rows(1).await;
            state.rates_job.lock().await.running = true;
            let (status, _) =
                post_reprice(State(state.clone()), Json(json!({"from": "2026-10-01"}))).await;
            assert_eq!(status, StatusCode::CONFLICT);
            let (status, _) = post_backfill(State(state.clone())).await;
            assert_eq!(status, StatusCode::CONFLICT);
        })
        .await;
    }
}
