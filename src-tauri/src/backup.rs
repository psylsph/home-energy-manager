//! Scheduled and on-demand `history.db` backups (issue #379).
//!
//! The file work lives in [`crate::history::backup`]; this module owns the
//! schedule, the status shown in Settings, the failure alert and the HTTP
//! endpoints. Status is persisted in the history database's `meta` table so
//! the schedule survives restarts: a restart never triggers an early backup,
//! and a missed one runs soon after start.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Local, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::history::backup::{BackupOutcome, BackupRecord};
use crate::inverter::poll::AppState;
use crate::settings::{HistoryBackupConfig, Settings};

const STATUS_META_KEY: &str = "history_backup_status";

/// Let the inverter connection and first polls settle before backing up.
const INITIAL_DELAY: Duration = Duration::from_secs(120);

/// How often the loop checks whether a backup is due.
const CHECK_INTERVAL: Duration = Duration::from_secs(600);

/// Persisted backup history.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BackupStatus {
    /// The latest attempt of either kind, shown in Settings.
    pub last: Option<BackupRecord>,
    pub last_success_at: Option<i64>,
    /// The latest scheduled attempt. The retry wait and the alert de-dup use
    /// only this, so a manual "Back up now" never delays the schedule or
    /// re-arms an alert for the same outage.
    #[serde(default)]
    pub last_scheduled: Option<BackupRecord>,
    /// The current run of scheduled failures has been alerted (an alert was
    /// delivered). Cleared by a scheduled success.
    #[serde(default)]
    pub failure_alerted: bool,
}

/// Shared between the loop and the HTTP handlers via [`AppState::backup`].
#[derive(Debug, Default)]
pub struct BackupState {
    /// A backup is in progress; a second request is refused.
    pub running: bool,
    pub status: BackupStatus,
    /// `status` has been read back from the database.
    loaded: bool,
    /// Signalled each time a backup finishes, so tests can await one
    /// without polling a clock.
    pub done: Arc<tokio::sync::Notify>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trigger {
    Scheduled,
    Manual,
}

impl Trigger {
    fn as_str(self) -> &'static str {
        match self {
            Trigger::Scheduled => "scheduled",
            Trigger::Manual => "manual",
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum RunError {
    AlreadyRunning,
    Failed(Box<BackupRecord>),
}

/// Take a backup now, record the result and alert on a new scheduled failure.
pub(crate) async fn run_backup_now(
    state: &Arc<AppState>,
    trigger: Trigger,
    now: DateTime<Local>,
) -> Result<BackupRecord, RunError> {
    if !try_claim(state).await {
        return Err(RunError::AlreadyRunning);
    }
    run_claimed(state, trigger, now).await
}

/// Whether `failure_alerted` should be set after a scheduled attempt.
fn alerted_after(was_alerted: bool, ok: bool, delivered: bool) -> bool {
    !ok && (was_alerted || delivered)
}

/// Mark a backup as running, unless one already is.
async fn try_claim(state: &Arc<AppState>) -> bool {
    let mut guard = state.backup.lock().await;
    !std::mem::replace(&mut guard.running, true)
}

/// A successful [`try_claim`]. Released (`running` cleared, `done` signalled)
/// explicitly at the end of a backup, or on drop if anything in between
/// panicked, so a bug can never block every later backup.
struct Claim {
    state: Option<Arc<AppState>>,
}

impl Claim {
    fn new(state: Arc<AppState>) -> Self {
        Self { state: Some(state) }
    }

    async fn release(mut self) {
        if let Some(state) = self.state.take() {
            finish(&state).await;
        }
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move { finish(&state).await });
            }
        }
    }
}

/// Clear `running` and signal that a backup finished.
async fn finish(state: &Arc<AppState>) {
    let mut guard = state.backup.lock().await;
    guard.running = false;
    guard.done.notify_one();
}

/// The backup itself, after [`try_claim`] succeeded; always clears `running`.
async fn run_claimed(
    state: &Arc<AppState>,
    trigger: Trigger,
    now: DateTime<Local>,
) -> Result<BackupRecord, RunError> {
    run_claimed_with(state, trigger, now, backup_work(state.clone(), now)).await
}

/// Run `work` (the file side of a backup) on its own task, then record the
/// result. A panic in `work` is recorded, saved, retried and alerted like any
/// other failed attempt.
async fn run_claimed_with<F>(
    state: &Arc<AppState>,
    trigger: Trigger,
    now: DateTime<Local>,
    work: F,
) -> Result<BackupRecord, RunError>
where
    F: std::future::Future<Output = Result<BackupOutcome, String>> + Send + 'static,
{
    let claim = Claim::new(state.clone());
    ensure_loaded(state).await;
    let result = tokio::spawn(work)
        .await
        .unwrap_or_else(|e| Err(format!("backup task failed: {e}")));
    let display = |p: &std::path::Path| p.to_string_lossy().into_owned();
    let record = match &result {
        Ok(outcome) => {
            tracing::info!(
                "History backup written to {} ({} bytes)",
                outcome.path.display(),
                outcome.size_bytes
            );
            BackupRecord {
                at: now.timestamp(),
                ok: true,
                trigger: trigger.as_str().to_string(),
                path: Some(display(&outcome.path)),
                size_bytes: Some(outcome.size_bytes),
                settings_path: outcome.settings_path.as_deref().map(display),
                error: None,
            }
        }
        Err(error) => {
            tracing::warn!("History backup failed: {error}");
            BackupRecord {
                at: now.timestamp(),
                ok: false,
                trigger: trigger.as_str().to_string(),
                path: None,
                size_bytes: None,
                settings_path: None,
                error: Some(error.clone()),
            }
        }
    };
    conclude(state, trigger, &now, &record).await;
    claim.release().await;
    if record.ok {
        Ok(record)
    } else {
        Err(RunError::Failed(Box::new(record)))
    }
}

/// The file side of a backup: check the folder, then copy, verify, rotate.
async fn backup_work(state: Arc<AppState>, now: DateTime<Local>) -> Result<BackupOutcome, String> {
    let config = Settings::load_async().await.history_backup;
    let Some(db) = state.history.lock().await.clone() else {
        return Err("History database is not open".to_string());
    };
    let served = state
        .served_dir
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let settings_path = Settings::settings_path();
    let config_dir = Settings::settings_dir();
    tokio::task::spawn_blocking(move || {
        // Checked here too: the folder may have been saved before the web
        // folder was known.
        if let Some(served) = &served {
            let dir = crate::history::backup::resolve_destination(&config, &config_dir)?;
            crate::history::backup::check_not_served(&dir, served)?;
        }
        crate::history::backup::run_backup(db.path(), &settings_path, &config, &config_dir, &now)
    })
    .await
    .unwrap_or_else(|e| Err(format!("backup task failed: {e}")))
}

/// Record an attempt in the status, alert on the first scheduled failure of
/// an outage, and save the status.
async fn conclude(
    state: &Arc<AppState>,
    trigger: Trigger,
    now: &DateTime<Local>,
    record: &BackupRecord,
) {
    let (was_alerted, status) = {
        let mut guard = state.backup.lock().await;
        guard.status.last = Some(record.clone());
        if trigger == Trigger::Scheduled {
            guard.status.last_scheduled = Some(record.clone());
        }
        if record.ok {
            guard.status.last_success_at = Some(record.at);
        }
        (guard.status.failure_alerted, guard.status.clone())
    };

    // Only the first scheduled failure of an outage alerts, and only an
    // alert that was delivered counts, so alerts that were off or failing
    // when the outage began still get one through later. "Back up now"
    // results are shown on screen and never alert.
    let mut delivered = false;
    if trigger == Trigger::Scheduled && !record.ok && !was_alerted {
        let schedule = Settings::load_async().await.history_backup;
        let error = record.error.as_deref().unwrap_or("unknown error");
        let next = crate::history::backup::next_due_at(
            &schedule,
            status.last_success_at,
            status.last_scheduled.as_ref().map(|r| r.at),
            now.timestamp(),
        )
        .and_then(|t| Local.timestamp_opt(t, 0).single());
        let text = build_backup_failed_message(error, now, next.as_ref());
        delivered = matches!(
            crate::alerts::send_text_notification(state, &text, "backup-failed").await,
            crate::alerts::AlertDelivery::Delivered
        );
    }

    let status = {
        let mut guard = state.backup.lock().await;
        if trigger == Trigger::Scheduled {
            guard.status.failure_alerted = alerted_after(was_alerted, record.ok, delivered);
        }
        guard.status.clone()
    };
    let db = state.history.lock().await.clone();
    if let Some(db) = db {
        persist(db, &status).await;
    }
}

/// `at` is the attempt's time (the record's `at`); `next` is when the
/// schedule will try again, or `None` when automatic backups are off.
fn build_backup_failed_message(
    error: &str,
    at: &DateTime<Local>,
    next: Option<&DateTime<Local>>,
) -> String {
    let error = crate::alerts::report::escape_html(error);
    let mut msg = format!(
        "💾 HEM History Backup Failed — {}\n",
        at.format("%Y-%m-%d %H:%M:%S")
    );
    msg.push_str("━━━━━━━━━━━━━━━━━━━━━━━━\n");
    msg.push_str(&format!("{error}\nThe previous backup is untouched. "));
    match next {
        Some(next) => msg.push_str(&format!("Next attempt {}.", next.format("%Y-%m-%d %H:%M"))),
        None => msg.push_str("Automatic backups are off."),
    }
    msg
}

/// Read the persisted status back once per process.
async fn ensure_loaded(state: &Arc<AppState>) {
    if state.backup.lock().await.loaded {
        return;
    }
    let Some(db) = state.history.lock().await.clone() else {
        return;
    };
    let stored = tokio::task::spawn_blocking(move || db.get_meta_value(STATUS_META_KEY))
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<BackupStatus>(&raw).ok());
    let mut guard = state.backup.lock().await;
    if !guard.loaded {
        if let Some(stored) = stored {
            // A result recorded before the database opened is newer than
            // anything stored, so in-memory fields win where present.
            let status = &mut guard.status;
            // The alert state belongs with the scheduled attempts it came
            // from: kept from memory only if this process already made one.
            if status.last_scheduled.is_none() {
                status.failure_alerted = stored.failure_alerted;
            }
            status.last = status.last.take().or(stored.last);
            status.last_scheduled = status.last_scheduled.take().or(stored.last_scheduled);
            status.last_success_at = status.last_success_at.max(stored.last_success_at);
        }
        guard.loaded = true;
    }
}

async fn persist(db: Arc<crate::history::HistoryDb>, status: &BackupStatus) {
    let Ok(raw) = serde_json::to_string(status) else {
        return;
    };
    let saved = tokio::task::spawn_blocking(move || db.set_meta_value(STATUS_META_KEY, &raw))
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    if let Err(e) = saved {
        tracing::warn!("Failed to save history backup status: {e}");
    }
}

/// Run a scheduled backup if one is due at `now`. Returns the attempt's
/// record, or `None` when nothing was due (or one is already running).
pub(crate) async fn run_scheduled_backup_if_due(
    state: &Arc<AppState>,
    now: DateTime<Local>,
) -> Option<BackupRecord> {
    let config = Settings::load_async().await.history_backup;
    if !config.enabled {
        return None;
    }
    ensure_loaded(state).await;
    let (running, last_success_at, last_scheduled_attempt_at) = {
        let guard = state.backup.lock().await;
        (
            guard.running,
            guard.status.last_success_at,
            guard.status.last_scheduled.as_ref().map(|r| r.at),
        )
    };
    if running
        || !crate::history::backup::backup_due(
            &config,
            last_success_at,
            last_scheduled_attempt_at,
            now.timestamp(),
        )
    {
        return None;
    }
    match run_backup_now(state, Trigger::Scheduled, now).await {
        Ok(record) => Some(record),
        Err(RunError::Failed(record)) => Some(*record),
        Err(RunError::AlreadyRunning) => None,
    }
}

/// Background loop. Spawned from the Tauri and headless startup paths in
/// [`crate::lib`]; not spawned in tests.
pub async fn run_backup_loop(state: Arc<AppState>) {
    tokio::time::sleep(INITIAL_DELAY).await;
    let mut tick = tokio::time::interval(CHECK_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        run_scheduled_backup_if_due(&state, Local::now()).await;
    }
}

/// `GET /api/backup` — config, defaults and the last result.
pub async fn get_backup(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    get_backup_at(&state, chrono::Utc::now().timestamp()).await
}

/// [`get_backup`] at an injected `now` (unix seconds).
async fn get_backup_at(state: &Arc<AppState>, now: i64) -> (StatusCode, Json<Value>) {
    ensure_loaded(state).await;
    let config = Settings::load_async().await.history_backup;
    let guard = state.backup.lock().await;
    let next_due_at = crate::history::backup::next_due_at(
        &config,
        guard.status.last_success_at,
        guard.status.last_scheduled.as_ref().map(|r| r.at),
        now,
    );
    let default_destination =
        crate::history::backup::default_destination(&Settings::settings_dir());
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "data": {
                "config": config,
                "default_destination": default_destination.to_string_lossy(),
                "running": guard.running,
                "last": guard.status.last,
                "last_success_at": guard.status.last_success_at,
                "next_due_at": next_due_at,
            },
        })),
    )
}

/// The fields one `POST /api/backup` changes.
#[derive(Debug, Default)]
struct ConfigPatch {
    enabled: Option<bool>,
    destination: Option<String>,
    keep: Option<u32>,
    interval_hours: Option<u32>,
    include_settings: Option<bool>,
}

impl ConfigPatch {
    fn parse(body: &Value) -> Result<Self, String> {
        let flag = |key: &str| -> Result<Option<bool>, String> {
            body.get(key)
                .map(|v| v.as_bool().ok_or(format!("{key} must be true or false")))
                .transpose()
        };
        let number = |key: &str, max: u32| -> Result<Option<u32>, String> {
            body.get(key)
                .map(|v| {
                    v.as_u64()
                        .filter(|n| (1..=u64::from(max)).contains(n))
                        .map(|n| n as u32)
                        .ok_or(format!("{key} must be a whole number from 1 to {max}"))
                })
                .transpose()
        };
        let destination = body
            .get("destination")
            .map(|v| {
                v.as_str()
                    .map(|s| s.trim().to_string())
                    .ok_or("destination must be text".to_string())
            })
            .transpose()?;
        Ok(Self {
            enabled: flag("enabled")?,
            destination,
            keep: number("keep", crate::settings::HISTORY_BACKUP_KEEP_MAX)?,
            interval_hours: number(
                "interval_hours",
                crate::settings::HISTORY_BACKUP_INTERVAL_HOURS_MAX,
            )?,
            include_settings: flag("include_settings")?,
        })
    }

    fn apply(self, config: &mut HistoryBackupConfig) {
        if let Some(enabled) = self.enabled {
            config.enabled = enabled;
        }
        if let Some(destination) = self.destination {
            config.destination = destination;
        }
        if let Some(keep) = self.keep {
            config.keep = keep;
        }
        if let Some(interval_hours) = self.interval_hours {
            config.interval_hours = interval_hours;
        }
        if let Some(include_settings) = self.include_settings {
            config.include_settings = include_settings;
        }
    }
}

fn bad_request(error: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"ok": false, "error": error})),
    )
}

/// `POST /api/backup` — save any of `enabled`, `destination`, `keep`,
/// `interval_hours`, `include_settings`.
pub async fn set_backup(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let patch = match ConfigPatch::parse(&body) {
        Ok(patch) => patch,
        Err(e) => return bad_request(&e),
    };
    let stored_destination = Settings::load_async().await.history_backup.destination;
    // Only a changed folder is checked, so a share that is offline right now
    // doesn't block changing the interval or how many backups to keep.
    if let Some(destination) = patch
        .destination
        .clone()
        .filter(|destination| *destination != stored_destination)
    {
        let served = state
            .served_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // A stalled network mount can block the metadata call.
        let checked = tokio::task::spawn_blocking(move || {
            crate::history::backup::validate_destination(&destination)?;
            match &served {
                Some(served) if !destination.is_empty() => {
                    crate::history::backup::check_not_served(
                        std::path::Path::new(&destination),
                        served,
                    )
                }
                _ => Ok(()),
            }
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        if let Err(e) = checked {
            return bad_request(&e);
        }
    }
    let saved = Settings::try_update_async(move |settings| {
        patch.apply(&mut settings.history_backup);
        Ok(settings.history_backup.clone())
    })
    .await;
    match saved {
        Ok(Ok(config)) => (
            StatusCode::OK,
            Json(json!({"ok": true, "data": {"config": config}})),
        ),
        Ok(Err(e)) | Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e})),
        ),
    }
}

/// `POST /api/backup/run` — "Back up now".
pub async fn post_backup_run(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    // Claimed before replying, so a status fetch straight after sees it
    // running. The backup runs on its own task and the reply doesn't wait: a
    // large database on a network share can outlast a proxy's timeout, and a
    // client that disconnects must not abandon it. The panel polls
    // `GET /api/backup` for the result.
    if !try_claim(&state).await {
        return (
            StatusCode::CONFLICT,
            Json(json!({"ok": false, "error": "A backup is already running"})),
        );
    }
    tokio::spawn(async move {
        let _ = run_claimed(&state, Trigger::Manual, Local::now()).await;
    });
    (
        StatusCode::ACCEPTED,
        Json(json!({"ok": true, "data": {"running": true}})),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// 2026-10-21 12:00:00 UTC.
    const NOON: i64 = 1_792_584_000;
    const RETRY_SECS_FOR_TEST: i64 = crate::history::backup::RETRY_SECS;

    fn local(secs: i64) -> DateTime<Local> {
        Local.timestamp_opt(secs, 0).single().unwrap()
    }

    async fn state_with_history() -> Arc<AppState> {
        let dir = Settings::settings_dir();
        let db = crate::history::HistoryDb::open(&dir.join("history.db")).unwrap();
        let state = Arc::new(AppState::new());
        *state.history.lock().await = Some(Arc::new(db));
        state
    }

    fn backups_in(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with("history-"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn record(ok: bool) -> BackupRecord {
        BackupRecord {
            at: NOON,
            ok,
            trigger: "scheduled".into(),
            path: None,
            size_bytes: None,
            settings_path: None,
            error: (!ok).then(|| "boom".into()),
        }
    }

    #[tokio::test]
    async fn the_schedule_fires_on_the_injected_clock() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let dest = Settings::settings_dir().join("backups");

            let first = run_scheduled_backup_if_due(&state, local(NOON))
                .await
                .unwrap();
            assert!(first.ok, "{first:?}");
            assert_eq!(first.trigger, "scheduled");
            assert!(first.size_bytes.unwrap() > 0);
            assert_eq!(backups_in(&dest).len(), 1);

            assert!(run_scheduled_backup_if_due(&state, local(NOON + 3600))
                .await
                .is_none());
            assert!(run_scheduled_backup_if_due(&state, local(NOON + 86_399))
                .await
                .is_none());
            let next = run_scheduled_backup_if_due(&state, local(NOON + 86_400))
                .await
                .unwrap();
            assert!(next.ok);
            assert_eq!(backups_in(&dest).len(), 2);
        })
        .await;
    }

    #[tokio::test]
    async fn nothing_runs_when_backups_are_turned_off() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            Settings::update(|s| s.history_backup.enabled = false).unwrap();
            let state = state_with_history().await;
            assert!(run_scheduled_backup_if_due(&state, local(NOON))
                .await
                .is_none());
            assert!(backups_in(&Settings::settings_dir().join("backups")).is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn the_schedule_survives_a_restart() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            run_scheduled_backup_if_due(&state, local(NOON))
                .await
                .unwrap();
            drop(state);

            // A fresh process reads the last success back from the database
            // and does not back up again early.
            let restarted = state_with_history().await;
            assert!(run_scheduled_backup_if_due(&restarted, local(NOON + 600))
                .await
                .is_none());
            let status = restarted.backup.lock().await.status.clone();
            assert_eq!(status.last_success_at, Some(NOON));
        })
        .await;
    }

    #[tokio::test]
    async fn a_failed_backup_is_recorded_and_retried_after_an_hour() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            run_scheduled_backup_if_due(&state, local(NOON))
                .await
                .unwrap();

            let missing = Settings::settings_dir().join("nas");
            Settings::update(|s| s.history_backup.destination = missing.to_string_lossy().into())
                .unwrap();
            let due = NOON + 86_400;
            let failed = run_scheduled_backup_if_due(&state, local(due))
                .await
                .unwrap();
            assert!(!failed.ok);
            assert!(failed.error.as_deref().unwrap().contains("does not exist"));

            let status = state.backup.lock().await.status.clone();
            assert_eq!(
                status.last_success_at,
                Some(NOON),
                "last good backup is kept"
            );
            assert_eq!(status.last.as_ref(), Some(&failed));

            assert!(run_scheduled_backup_if_due(&state, local(due + 1800))
                .await
                .is_none());
            std::fs::create_dir(&missing).unwrap();
            let retried = run_scheduled_backup_if_due(&state, local(due + 3600))
                .await
                .unwrap();
            assert!(retried.ok, "{retried:?}");
            assert_eq!(backups_in(&missing).len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn a_backup_without_a_history_database_fails_cleanly() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let err = run_backup_now(&state, Trigger::Manual, local(NOON))
                .await
                .unwrap_err();
            let RunError::Failed(record) = err else {
                panic!("expected a failure record");
            };
            assert!(record.error.unwrap().contains("not open"));
            assert!(!state.backup.lock().await.running);
        })
        .await;
    }

    #[tokio::test]
    async fn a_second_backup_is_refused_while_one_is_running() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            state.backup.lock().await.running = true;
            assert_eq!(
                run_backup_now(&state, Trigger::Manual, local(NOON)).await,
                Err(RunError::AlreadyRunning)
            );
            assert!(run_scheduled_backup_if_due(&state, local(NOON))
                .await
                .is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn a_manual_attempt_neither_delays_the_schedule_nor_repeats_the_alert() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let missing = Settings::settings_dir().join("nas");
            Settings::update(|s| s.history_backup.destination = missing.to_string_lossy().into())
                .unwrap();
            // 02:00 scheduled attempt fails (and alerts).
            let failed = run_scheduled_backup_if_due(&state, local(NOON))
                .await
                .unwrap();
            assert!(!failed.ok);
            // 02:30 the user tries "Back up now"; it fails too.
            run_backup_now(&state, Trigger::Manual, local(NOON + 1800))
                .await
                .unwrap_err();
            {
                let guard = state.backup.lock().await;
                assert_eq!(guard.status.last.as_ref().unwrap().trigger, "manual");
                assert_eq!(guard.status.last_scheduled.as_ref(), Some(&failed));
            }
            // The retry is still due an hour after the scheduled attempt.
            let retry = run_scheduled_backup_if_due(&state, local(NOON + 3600)).await;
            retry.expect("retry runs at 03:00, not 03:30");
        })
        .await;
    }

    #[tokio::test]
    async fn a_manual_attempt_leaves_the_alert_state_alone() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            Settings::update(|s| s.history_backup.destination = "/no/such/hem/nas".into()).unwrap();
            // An outage already alerted...
            state.backup.lock().await.status.failure_alerted = true;
            // ...a failed "Back up now" neither clears nor re-arms it.
            run_backup_now(&state, Trigger::Manual, local(NOON))
                .await
                .unwrap_err();
            assert!(state.backup.lock().await.status.failure_alerted);
        })
        .await;
    }

    #[test]
    fn the_failure_alert_uses_the_attempt_time_and_says_what_happens_next() {
        let at = local(NOON);
        let next = local(NOON + 3600);
        let text = build_backup_failed_message("disk full", &at, Some(&next));
        assert!(
            text.contains(&at.format("%Y-%m-%d %H:%M").to_string()),
            "{text}"
        );
        assert!(text.contains("disk full"));
        assert!(
            text.contains(&format!("Next attempt {}", next.format("%Y-%m-%d %H:%M"))),
            "{text}"
        );
        let off = build_backup_failed_message("disk full", &at, None);
        assert!(off.contains("Automatic backups are off"), "{off}");
        assert!(!off.contains("Next attempt"));
        // The error comes from the filesystem and is sent as HTML.
        assert!(build_backup_failed_message("<b>", &at, None).contains("&lt;b&gt;"));
    }

    #[tokio::test]
    async fn get_reports_config_defaults_and_last_result() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let (code, Json(body)) = get_backup_at(&state, NOON).await;
            assert_eq!(code, StatusCode::OK);
            let data = &body["data"];
            assert_eq!(data["config"]["enabled"], true);
            assert_eq!(data["config"]["keep"], 3);
            assert_eq!(data["config"]["interval_hours"], 24);
            assert_eq!(data["config"]["include_settings"], false);
            assert_eq!(
                data["default_destination"],
                Settings::settings_dir()
                    .join("backups")
                    .to_string_lossy()
                    .as_ref()
            );
            assert_eq!(data["running"], false);
            assert!(data["last"].is_null());
            assert_eq!(
                data["next_due_at"].as_i64(),
                Some(NOON),
                "never backed up: due now"
            );

            run_backup_now(&state, Trigger::Manual, local(NOON))
                .await
                .unwrap();
            let (_, Json(body)) = get_backup_at(&state, NOON + 60).await;
            assert_eq!(body["data"]["last"]["ok"], true);
            assert_eq!(body["data"]["last_success_at"], NOON);
            assert_eq!(body["data"]["next_due_at"], NOON + 86_400);
        })
        .await;
    }

    #[tokio::test]
    async fn set_saves_valid_fields_and_leaves_others_alone() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let dest = Settings::settings_dir();
            let (code, Json(body)) = set_backup(
                State(Arc::new(AppState::new())),
                Json(json!({
                    "destination": dest.to_string_lossy(),
                    "keep": 14,
                    "include_settings": true,
                })),
            )
            .await;
            assert_eq!(code, StatusCode::OK, "{body}");
            let saved = Settings::load().history_backup;
            assert_eq!(
                saved,
                HistoryBackupConfig {
                    destination: dest.to_string_lossy().into(),
                    keep: 14,
                    include_settings: true,
                    ..HistoryBackupConfig::default()
                }
            );
            assert_eq!(body["data"]["config"]["keep"], 14);

            let (code, _) = set_backup(
                State(Arc::new(AppState::new())),
                Json(json!({"enabled": false, "interval_hours": 6, "destination": "  "})),
            )
            .await;
            assert_eq!(code, StatusCode::OK);
            let saved = Settings::load().history_backup;
            assert!(!saved.enabled);
            assert_eq!(saved.interval_hours, 6);
            assert_eq!(saved.destination, "", "blank means the default folder");
            assert_eq!(saved.keep, 14);
        })
        .await;
    }

    #[tokio::test]
    async fn an_unchanged_unavailable_folder_does_not_block_other_changes() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            // The NAS is offline right now; the user only changes "keep".
            Settings::update(|s| s.history_backup.destination = "/no/such/hem/nas".into()).unwrap();
            let (code, Json(body)) = set_backup(
                State(Arc::new(AppState::new())),
                Json(json!({"destination": "/no/such/hem/nas", "keep": 3})),
            )
            .await;
            assert_eq!(code, StatusCode::OK, "{body}");
            assert_eq!(Settings::load().history_backup.keep, 3);
        })
        .await;
    }

    #[tokio::test]
    async fn a_result_recorded_before_the_database_opens_is_not_overwritten() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            // An older status is stored in the database...
            let dir = Settings::settings_dir();
            let db = Arc::new(crate::history::HistoryDb::open(&dir.join("history.db")).unwrap());
            let stored = BackupStatus {
                last: Some(record(true)),
                last_success_at: Some(NOON),
                last_scheduled: Some(record(true)),
                failure_alerted: false,
            };
            db.set_meta_value(STATUS_META_KEY, &serde_json::to_string(&stored).unwrap())
                .unwrap();
            // ...but this process tried a backup before the database opened.
            let state = Arc::new(AppState::new());
            let early = run_backup_now(&state, Trigger::Manual, local(NOON + 600))
                .await
                .unwrap_err();
            let RunError::Failed(early) = early else {
                panic!("expected a failure");
            };
            *state.history.lock().await = Some(db);
            let (_, Json(body)) = get_backup_at(&state, NOON + 600).await;
            assert_eq!(body["data"]["last"]["at"], NOON + 600, "newer result kept");
            assert_eq!(body["data"]["last"]["ok"], false);
            assert_eq!(
                body["data"]["last_success_at"], NOON,
                "stored success merged in"
            );
            assert_eq!(
                state.backup.lock().await.status.last.as_ref(),
                Some(&*early)
            );
        })
        .await;
    }

    #[tokio::test]
    async fn set_rejects_bad_values_without_saving_anything() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            for (body, message) in [
                (json!({"keep": 0}), "keep"),
                (json!({"keep": 366}), "keep"),
                (json!({"keep": "7"}), "keep"),
                (json!({"interval_hours": 0}), "interval"),
                (json!({"interval_hours": 169}), "interval"),
                (json!({"enabled": "yes"}), "enabled"),
                (json!({"include_settings": 1}), "include_settings"),
                (json!({"destination": 5}), "destination"),
                (json!({"destination": "relative/dir"}), "full path"),
                (
                    json!({"keep": 3, "destination": "/no/such/hem/dir"}),
                    "does not exist",
                ),
            ] {
                let (code, Json(reply)) =
                    set_backup(State(Arc::new(AppState::new())), Json(body.clone())).await;
                assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
                let error = reply["error"].as_str().unwrap();
                assert!(error.contains(message), "{body}: {error}");
            }
            assert_eq!(
                Settings::load().history_backup,
                HistoryBackupConfig::default()
            );
        })
        .await;
    }

    /// Wait for a backup started by `POST /api/backup/run` to finish. The
    /// done signal is a stored permit, so it can't be missed; the timeout is
    /// only a hang guard, never part of the assertion.
    async fn finished(state: &Arc<AppState>) -> BackupRecord {
        let done = state.backup.lock().await.done.clone();
        tokio::time::timeout(Duration::from_secs(60), done.notified())
            .await
            .expect("backup finished");
        let guard = state.backup.lock().await;
        assert!(!guard.running);
        guard.status.last.clone().expect("a result")
    }

    #[tokio::test]
    async fn a_panicking_scheduled_backup_is_recorded_like_any_failure() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            assert!(try_claim(&state).await);
            let result = run_claimed_with(&state, Trigger::Scheduled, local(NOON), async {
                panic!("bug in a backup")
            })
            .await;
            assert!(matches!(result, Err(RunError::Failed(_))));
            {
                let guard = state.backup.lock().await;
                assert!(!guard.running, "claim released");
                let last = guard.status.last_scheduled.as_ref().expect("recorded");
                assert_eq!(last.at, NOON);
                assert!(last
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("backup task failed"));
            }
            // Retried after the retry wait, not on the next 10-minute tick...
            assert!(run_scheduled_backup_if_due(&state, local(NOON + 600))
                .await
                .is_none());
            // ...and saved, so a restart doesn't forget it.
            let restarted = state_with_history().await;
            let (_, Json(body)) = get_backup_at(&restarted, NOON + 600).await;
            assert_eq!(body["data"]["last"]["at"], NOON);
            assert_eq!(body["data"]["next_due_at"], NOON + RETRY_SECS_FOR_TEST);
        })
        .await;
    }

    #[tokio::test]
    async fn a_claim_dropped_without_finishing_still_releases() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let done = state.backup.lock().await.done.clone();
            assert!(try_claim(&state).await);
            // As if the bookkeeping after a backup panicked mid-way.
            drop(Claim::new(state.clone()));
            tokio::time::timeout(Duration::from_secs(60), done.notified())
                .await
                .expect("released");
            assert!(!state.backup.lock().await.running);
        })
        .await;
    }

    #[tokio::test]
    async fn the_alert_state_survives_a_restart() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            {
                let db = state.history.lock().await.clone().unwrap();
                let stored = BackupStatus {
                    last: Some(record(false)),
                    last_success_at: None,
                    last_scheduled: Some(record(false)),
                    failure_alerted: true,
                };
                db.set_meta_value(STATUS_META_KEY, &serde_json::to_string(&stored).unwrap())
                    .unwrap();
            }
            ensure_loaded(&state).await;
            assert!(state.backup.lock().await.status.failure_alerted);
        })
        .await;
    }

    #[test]
    fn a_failure_counts_as_alerted_only_once_an_alert_is_delivered() {
        // First failure, alert delivered.
        assert!(alerted_after(false, false, true));
        // First failure, alerts off or every channel failed: try next time.
        assert!(!alerted_after(false, false, false));
        // Already alerted for this outage.
        assert!(alerted_after(true, false, false));
        // A success ends the outage.
        assert!(!alerted_after(true, true, false));
    }

    #[tokio::test]
    async fn a_failure_with_alerts_off_will_alert_once_they_work() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_history().await; // alerts off by default
            let missing = Settings::settings_dir().join("nas");
            Settings::update(|s| s.history_backup.destination = missing.to_string_lossy().into())
                .unwrap();
            run_scheduled_backup_if_due(&state, local(NOON))
                .await
                .unwrap();
            assert!(!state.backup.lock().await.status.failure_alerted);
        })
        .await;
    }

    #[tokio::test]
    async fn a_folder_inside_the_served_web_folder_is_refused() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            let served = Settings::settings_dir().join("dist");
            std::fs::create_dir_all(served.join("assets")).unwrap();
            *state.served_dir.lock().unwrap() = Some(served.clone());

            let (code, Json(body)) = set_backup(
                State(state.clone()),
                Json(json!({"destination": served.join("assets").to_string_lossy()})),
            )
            .await;
            assert_eq!(code, StatusCode::BAD_REQUEST);
            assert!(body["error"].as_str().unwrap().contains("web pages"));

            // Saved before the web folder was known: the backup itself refuses.
            Settings::update(|s| s.history_backup.destination = served.to_string_lossy().into())
                .unwrap();
            let RunError::Failed(record) = run_backup_now(&state, Trigger::Manual, local(NOON))
                .await
                .unwrap_err()
            else {
                panic!("expected a refusal");
            };
            assert!(record.error.unwrap().contains("web pages"));
            assert!(!std::fs::read_dir(&served).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("history-")));
        })
        .await;
    }

    #[tokio::test]
    async fn back_up_now_starts_in_the_background_and_refuses_a_second() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_history().await;
            // Hold the history handle so the started backup can't progress
            // (it waits for it first), making the second request's timing
            // deterministic.
            let history = state.history.lock().await;
            let (code, Json(body)) = post_backup_run(State(state.clone())).await;
            assert_eq!(code, StatusCode::ACCEPTED, "{body}");
            assert_eq!(body["data"]["running"], true);
            assert!(
                state.backup.lock().await.running,
                "claimed before the reply"
            );
            let (code, _) = post_backup_run(State(state.clone())).await;
            assert_eq!(code, StatusCode::CONFLICT);
            drop(history);
            let record = finished(&state).await;
            assert!(record.ok, "{record:?}");
            assert_eq!(record.trigger, "manual");

            state.backup.lock().await.running = true;
            let (code, _) = post_backup_run(State(state.clone())).await;
            assert_eq!(code, StatusCode::CONFLICT);
            state.backup.lock().await.running = false;

            Settings::update(|s| s.history_backup.destination = "/no/such/hem/dir".into()).unwrap();
            let (code, _) = post_backup_run(State(state.clone())).await;
            assert_eq!(code, StatusCode::ACCEPTED);
            let record = finished(&state).await;
            assert!(!record.ok);
            assert!(record.error.unwrap().contains("does not exist"));
        })
        .await;
    }
}
