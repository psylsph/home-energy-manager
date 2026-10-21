//! Automatic `history.db` backups (issue #379).
//!
//! Each backup is a `VACUUM INTO` copy taken on its own read-only
//! connection, so it never holds the [`super::HistoryDb`] connection lock
//! and poll inserts carry on while it runs. The copy is written to a hidden
//! temporary file, checked with `PRAGMA integrity_check`, and only then
//! renamed to its date-stamped name, so a partial or failed backup never
//! replaces or displaces a good one. Old backups are rotated out only after
//! a new one succeeds.

use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone};
use serde::{Deserialize, Serialize};

use crate::settings::HistoryBackupConfig;

/// `history-20261021-120000.db`: sorts chronologically by name.
const HISTORY_PREFIX: &str = "history-";
const HISTORY_SUFFIX: &str = ".db";
const SETTINGS_PREFIX: &str = "settings-";
const SETTINGS_SUFFIX: &str = ".json";
const STAMP_FORMAT: &str = "%Y%m%d-%H%M%S";

/// How long to wait before retrying a failed scheduled backup.
pub const RETRY_SECS: i64 = 3600;

/// What a finished backup produced.
#[derive(Debug, Clone, PartialEq)]
pub struct BackupOutcome {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub settings_path: Option<PathBuf>,
}

/// The result of one backup attempt, shown in Settings and persisted so the
/// schedule survives restarts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupRecord {
    /// Unix seconds when the attempt started.
    pub at: i64,
    pub ok: bool,
    /// `"scheduled"` or `"manual"`.
    pub trigger: String,
    pub path: Option<String>,
    pub size_bytes: Option<u64>,
    pub settings_path: Option<String>,
    pub error: Option<String>,
}

/// `backups/` in the config directory.
pub fn default_destination(config_dir: &Path) -> PathBuf {
    config_dir.join("backups")
}

/// Check a user-entered destination before it is saved: empty (the default)
/// or an absolute path to an existing directory.
pub fn validate_destination(destination: &str) -> Result<(), String> {
    let destination = destination.trim();
    if destination.is_empty() {
        return Ok(());
    }
    check_chosen_directory(Path::new(destination))
}

fn check_chosen_directory(dir: &Path) -> Result<(), String> {
    if !dir.is_absolute() {
        return Err(format!(
            "Backup folder must be a full path, not {}",
            dir.display()
        ));
    }
    match std::fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(format!("Backup folder {} is not a folder", dir.display())),
        Err(_) => Err(format!(
            "Backup folder {} does not exist (is the drive or share mounted?)",
            dir.display()
        )),
    }
}

/// Refuse a backup folder inside the folder HEM serves its web pages from,
/// where anyone who can reach HEM could download the backups (and any
/// `settings.json` copies). `served` that doesn't exist can't be served.
pub fn check_not_served(dir: &Path, served: &Path) -> Result<(), String> {
    let (Ok(real_dir), Ok(real_served)) = (dir.canonicalize(), served.canonicalize()) else {
        return Ok(());
    };
    if real_dir.starts_with(&real_served) {
        return Err(format!(
            "Backup folder {} is inside {}, the folder HEM serves its web pages from, \
             where anyone who can reach HEM could download the backups",
            dir.display(),
            served.display()
        ));
    }
    Ok(())
}

/// The directory backups go to. The default is created on demand; a
/// directory the user chose must already exist, so an unmounted NAS share
/// fails instead of quietly filling the local disk under the mount point.
pub fn resolve_destination(
    config: &HistoryBackupConfig,
    config_dir: &Path,
) -> Result<PathBuf, String> {
    let destination = config.destination.trim();
    if destination.is_empty() {
        let dir = default_destination(config_dir);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("failed to create backup folder {}: {e}", dir.display()))?;
        return Ok(dir);
    }
    let dir = PathBuf::from(destination);
    check_chosen_directory(&dir)?;
    Ok(dir)
}

/// Whether a scheduled backup should run at `now` (unix seconds).
pub fn backup_due(
    config: &HistoryBackupConfig,
    last_success_at: Option<i64>,
    last_scheduled_attempt_at: Option<i64>,
    now: i64,
) -> bool {
    next_due_at(config, last_success_at, last_scheduled_attempt_at, now)
        .is_some_and(|due| due <= now)
}

/// When the next scheduled backup will run, never earlier than `now`; `None`
/// when backups are off.
pub fn next_due_at(
    config: &HistoryBackupConfig,
    last_success_at: Option<i64>,
    last_scheduled_attempt_at: Option<i64>,
    now: i64,
) -> Option<i64> {
    if !config.enabled {
        return None;
    }
    let interval = i64::from(
        config
            .interval_hours
            .clamp(1, crate::settings::HISTORY_BACKUP_INTERVAL_HOURS_MAX),
    ) * 3600;
    // Due one interval after the last success, and no sooner than the retry
    // wait (an hour) after the last scheduled attempt. A stored time ahead of
    // `now` (the clock stepped back) counts as elapsed, so a bad clock can't
    // stall the schedule.
    let after = |at: Option<i64>, wait: i64| at.filter(|&t| t <= now).map(|t| t + wait);
    let due = [
        after(last_success_at, interval),
        after(last_scheduled_attempt_at, RETRY_SECS.min(interval)),
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(now);
    Some(due.max(now))
}

/// Take a backup of the database at `db_path`.
pub fn run_backup<TZ: TimeZone>(
    db_path: &Path,
    settings_path: &Path,
    config: &HistoryBackupConfig,
    config_dir: &Path,
    now: &DateTime<TZ>,
) -> Result<BackupOutcome, String> {
    run_backup_with(
        db_path,
        settings_path,
        config,
        config_dir,
        now,
        super::verify_integrity,
        |dir: &Path| fs4::available_space(dir),
    )
}

fn run_backup_with<TZ: TimeZone>(
    db_path: &Path,
    settings_path: &Path,
    config: &HistoryBackupConfig,
    config_dir: &Path,
    now: &DateTime<TZ>,
    verify: impl FnOnce(&Path) -> Result<(), String>,
    free_space: impl FnOnce(&Path) -> std::io::Result<u64>,
) -> Result<BackupOutcome, String> {
    let dir = resolve_destination(config, config_dir)?;
    // UTC, so names never repeat or sort out of order when the clocks go back.
    let stamp = now
        .with_timezone(&chrono::Utc)
        .format(STAMP_FORMAT)
        .to_string();
    let path = dir.join(format!("{HISTORY_PREFIX}{stamp}{HISTORY_SUFFIX}"));
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    remove_stale_temporaries(&dir);
    check_free_space(db_path, &dir, free_space)?;
    let nonce = super::next_repair_nonce();
    // Hidden, and not matching the backup pattern, so rotation and anyone
    // browsing the folder never mistake it for a finished backup.
    let temporary = dir.join(format!(
        ".{HISTORY_PREFIX}{stamp}{HISTORY_SUFFIX}.tmp-{nonce}"
    ));

    // A separate read-only connection: the copy reads a consistent snapshot
    // (WAL included) while the poll loop keeps writing on its own.
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("failed to open history database: {e}"))?;
    conn.busy_timeout(std::time::Duration::from_secs(10))
        .map_err(|e| format!("failed to configure history database: {e}"))?;
    super::write_verified_copy(&conn, &temporary, &path, verify)?;
    drop(conn);
    let size_bytes = std::fs::metadata(&path)
        .map_err(|e| format!("failed to read backup size: {e}"))?
        .len();

    let settings_copy = if config.include_settings {
        let copy = dir.join(format!("{SETTINGS_PREFIX}{stamp}{SETTINGS_SUFFIX}"));
        let temporary = dir.join(format!(
            ".{SETTINGS_PREFIX}{stamp}{SETTINGS_SUFFIX}.tmp-{nonce}"
        ));
        let copied = std::fs::copy(settings_path, &temporary)
            .and_then(|_| std::fs::rename(&temporary, &copy));
        if let Err(e) = copied {
            // All or nothing: a history copy left behind here would be
            // retried hourly and never rotated.
            let _ = std::fs::remove_file(&temporary);
            let _ = std::fs::remove_file(&path);
            return Err(format!("failed to copy settings.json: {e}"));
        }
        Some(copy)
    } else {
        None
    };

    match rotate(&dir, config.keep, &stamp) {
        Ok(deleted) if !deleted.is_empty() => {
            tracing::info!(
                "History backup rotation removed {} old file(s)",
                deleted.len()
            );
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("History backup rotation failed: {e}"),
    }

    Ok(BackupOutcome {
        path,
        size_bytes,
        settings_path: settings_copy,
    })
}

/// Delete hidden temporaries left by a backup that was interrupted (HEM
/// killed mid-copy). Each process names its temporaries afresh, so nothing
/// else would ever remove them. Only one backup runs at a time, so none of
/// these can belong to a backup in progress.
fn remove_stale_temporaries(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let ours = [
            (HISTORY_PREFIX, HISTORY_SUFFIX),
            (SETTINGS_PREFIX, SETTINGS_SUFFIX),
        ]
        .iter()
        .any(|(prefix, suffix)| {
            name.strip_prefix('.')
                .and_then(|rest| rest.strip_prefix(prefix))
                .and_then(|rest| rest.split_once(&format!("{suffix}.tmp-")[..]))
                .is_some_and(|(stamp, _)| {
                    chrono::NaiveDateTime::parse_from_str(stamp, STAMP_FORMAT).is_ok()
                })
        });
        if ours {
            let path = entry.path();
            match std::fs::remove_file(&path) {
                Ok(()) => tracing::info!("Removed interrupted backup file {}", path.display()),
                Err(e) => tracing::warn!("Could not remove {}: {e}", path.display()),
            }
        }
    }
}

/// Refuse to start a backup that might fill the disk: running out of space
/// mid-copy on the disk that holds `history.db` would fail live history
/// writes too. Needs the database's size (WAL included) plus half again; a
/// filesystem that can't report free space doesn't block the backup.
fn check_free_space(
    db_path: &Path,
    dir: &Path,
    free_space: impl FnOnce(&Path) -> std::io::Result<u64>,
) -> Result<(), String> {
    let len = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let size = len(db_path) + len(&db_path.with_extension("db-wal"));
    let needed = size.saturating_add(size / 2);
    match free_space(dir) {
        Ok(free) if free < needed => Err(format!(
            "not enough free space in {}: needs {}, {} available",
            dir.display(),
            megabytes(needed),
            megabytes(free)
        )),
        Ok(_) => Ok(()),
        Err(e) => {
            tracing::warn!("Could not check free space in {}: {e}", dir.display());
            Ok(())
        }
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

/// Delete all but the newest `keep` history backups (and settings copies)
/// in `dir`. Only files whose names HEM itself writes are touched.
///
/// `current_stamp` is the backup just taken. A file stamped later than it
/// means one of the two clocks was wrong — a backup taken while the clock ran
/// ahead, or (just as likely) a real backup and a clock that is now behind.
/// Such files are neither deleted nor counted, so a wrong clock can never
/// destroy a good backup; once the clock passes them they rotate normally.
pub fn rotate(dir: &Path, keep: u32, current_stamp: &str) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("failed to list backup folder {}: {e}", dir.display()))?;
    let names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    let keep = keep.max(1) as usize;
    let mut deleted = Vec::new();
    for (prefix, suffix) in [
        (HISTORY_PREFIX, HISTORY_SUFFIX),
        (SETTINGS_PREFIX, SETTINGS_SUFFIX),
    ] {
        let mut ours: Vec<(&str, &String)> = names
            .iter()
            .filter_map(|name| {
                let stamp = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
                chrono::NaiveDateTime::parse_from_str(stamp, STAMP_FORMAT)
                    .ok()
                    .map(|_| (stamp, name))
            })
            .collect();
        // Stamps sort chronologically as text; newest first.
        ours.retain(|&(stamp, _)| stamp <= current_stamp);
        ours.sort_unstable_by_key(|&(stamp, _)| std::cmp::Reverse(stamp));
        for (_, name) in ours.into_iter().skip(keep) {
            let path = dir.join(name);
            std::fs::remove_file(&path)
                .map_err(|e| format!("failed to remove old backup {}: {e}", path.display()))?;
            deleted.push(path);
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::InverterSnapshot;

    fn temp_dir(label: &str) -> PathBuf {
        crate::test_util::make_unique_test_dir(label)
    }

    /// A real history database with one reading in it.
    fn seeded_db(dir: &Path) -> PathBuf {
        let path = dir.join("history.db");
        let db = super::super::HistoryDb::open(&path).unwrap();
        db.insert_reading(&InverterSnapshot {
            timestamp: 1_792_584_000,
            soc: 50,
            home_power: 1234,
            ..InverterSnapshot::default()
        });
        path
    }

    fn at(secs: i64) -> DateTime<chrono::Utc> {
        chrono::Utc.timestamp_opt(secs, 0).unwrap()
    }

    /// 2026-10-21 12:00:00 UTC.
    const NOON: i64 = 1_792_584_000;

    fn config_into(dest: &Path, keep: u32) -> HistoryBackupConfig {
        HistoryBackupConfig {
            destination: dest.to_string_lossy().into_owned(),
            keep,
            ..HistoryBackupConfig::default()
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_backup_is_a_verified_date_stamped_copy_with_the_data() {
        let dir = temp_dir("backup-basic");
        let db = seeded_db(&dir);
        let dest = dir.join("out");
        std::fs::create_dir(&dest).unwrap();
        let outcome = run_backup(
            &db,
            &dir.join("settings.json"),
            &config_into(&dest, 7),
            &dir,
            &at(NOON),
        )
        .unwrap();
        assert_eq!(outcome.path, dest.join("history-20261021-120000.db"));
        assert_eq!(outcome.settings_path, None);
        assert_eq!(
            outcome.size_bytes,
            std::fs::metadata(&outcome.path).unwrap().len()
        );
        let copy = rusqlite::Connection::open(&outcome.path).unwrap();
        let power: i64 = copy
            .query_row("SELECT home_power FROM readings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(power, 1234);
        assert_eq!(names(&dest), vec!["history-20261021-120000.db"]);
    }

    #[test]
    fn a_backup_includes_rows_still_in_the_wal() {
        let dir = temp_dir("backup-wal");
        let path = dir.join("history.db");
        // Keep the writer open so its rows stay in the WAL, as they do while
        // HEM is running.
        let db = super::super::HistoryDb::open(&path).unwrap();
        for i in 0..50 {
            db.insert_reading(&InverterSnapshot {
                timestamp: NOON + i * 60,
                soc: 50,
                ..InverterSnapshot::default()
            });
        }
        let dest = dir.join("out");
        std::fs::create_dir(&dest).unwrap();
        let outcome = run_backup(
            &path,
            &dir.join("settings.json"),
            &config_into(&dest, 7),
            &dir,
            &at(NOON),
        )
        .unwrap();
        let copy = rusqlite::Connection::open(&outcome.path).unwrap();
        let rows: i64 = copy
            .query_row("SELECT COUNT(*) FROM readings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 50);
    }

    #[test]
    fn the_default_destination_is_created_in_the_config_dir() {
        let dir = temp_dir("backup-default");
        let db = seeded_db(&dir);
        let outcome = run_backup(
            &db,
            &dir.join("settings.json"),
            &HistoryBackupConfig::default(),
            &dir,
            &at(NOON),
        )
        .unwrap();
        assert_eq!(outcome.path.parent().unwrap(), dir.join("backups"));
    }

    #[test]
    fn settings_are_copied_only_when_asked() {
        let dir = temp_dir("backup-settings");
        let db = seeded_db(&dir);
        let settings = dir.join("settings.json");
        std::fs::write(&settings, br#"{"host":"10.0.0.2"}"#).unwrap();
        let dest = dir.join("out");
        std::fs::create_dir(&dest).unwrap();
        let mut config = config_into(&dest, 7);
        config.include_settings = true;
        let outcome = run_backup(&db, &settings, &config, &dir, &at(NOON)).unwrap();
        let copied = outcome.settings_path.unwrap();
        assert_eq!(copied, dest.join("settings-20261021-120000.json"));
        assert_eq!(
            std::fs::read(&copied).unwrap(),
            br#"{"host":"10.0.0.2"}"#.to_vec()
        );
    }

    #[test]
    fn rotation_keeps_exactly_the_newest_n() {
        let dir = temp_dir("backup-rotate");
        let db = seeded_db(&dir);
        let settings = dir.join("settings.json");
        std::fs::write(&settings, b"{}").unwrap();
        let dest = dir.join("out");
        std::fs::create_dir(&dest).unwrap();
        // Files HEM didn't write are never touched.
        std::fs::write(dest.join("notes.txt"), b"mine").unwrap();
        std::fs::write(dest.join("history-manual-copy.db"), b"mine").unwrap();
        let mut config = config_into(&dest, 3);
        config.include_settings = true;
        for day in 0..5 {
            run_backup(&db, &settings, &config, &dir, &at(NOON + day * 86_400)).unwrap();
        }
        assert_eq!(
            names(&dest),
            vec![
                "history-20261023-120000.db",
                "history-20261024-120000.db",
                "history-20261025-120000.db",
                "history-manual-copy.db",
                "notes.txt",
                "settings-20261023-120000.json",
                "settings-20261024-120000.json",
                "settings-20261025-120000.json",
            ]
        );
    }

    #[test]
    fn a_future_stamped_backup_never_displaces_the_one_just_taken() {
        // Taken once while the clock was a year ahead (a Pi without RTC).
        let dir = temp_dir("backup-future");
        let db = seeded_db(&dir);
        let dest = dir.join("out");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("history-20271021-120000.db"), b"future").unwrap();
        std::fs::write(dest.join("history-20261001-120000.db"), b"older").unwrap();
        let config = config_into(&dest, 2);
        let outcome = run_backup(&db, &dir.join("s.json"), &config, &dir, &at(NOON)).unwrap();
        assert!(outcome.path.exists());
        // Later-stamped files are kept and not counted: keep=2 holds both
        // of the others.
        assert_eq!(
            names(&dest),
            vec![
                "history-20261001-120000.db",
                "history-20261021-120000.db",
                "history-20271021-120000.db"
            ]
        );
        let keep_one = config_into(&dest, 1);
        let outcome =
            run_backup(&db, &dir.join("s.json"), &keep_one, &dir, &at(NOON + 60)).unwrap();
        assert!(outcome.path.exists());
        assert_eq!(
            names(&dest),
            vec!["history-20261021-120100.db", "history-20271021-120000.db"]
        );
    }

    #[test]
    fn an_interrupted_backups_temporary_files_are_cleaned_up() {
        // HEM was killed mid-copy last time: its hidden temporaries stay
        // behind under a name no later process would reuse.
        let dir = temp_dir("backup-stale-temp");
        let db = seeded_db(&dir);
        let dest = dir.join("out");
        std::fs::create_dir(&dest).unwrap();
        for name in [
            ".history-20261020-120000.db.tmp-7",
            ".settings-20261020-120000.json.tmp-7",
            ".history-notes",
            ".hidden-user-file",
        ] {
            std::fs::write(dest.join(name), b"x").unwrap();
        }
        run_backup(
            &db,
            &dir.join("s.json"),
            &config_into(&dest, 7),
            &dir,
            &at(NOON),
        )
        .unwrap();
        assert_eq!(
            names(&dest),
            vec![
                ".hidden-user-file",
                ".history-notes",
                "history-20261021-120000.db"
            ]
        );
    }

    #[test]
    fn a_folder_inside_the_served_web_folder_is_refused() {
        let dir = temp_dir("backup-served");
        let served = dir.join("dist");
        std::fs::create_dir_all(served.join("assets")).unwrap();
        let sibling = dir.join("backups");
        std::fs::create_dir(&sibling).unwrap();
        for inside in [
            served.clone(),
            served.join("assets"),
            served.join("assets/../assets"),
        ] {
            let err = check_not_served(&inside, &served).unwrap_err();
            assert!(err.contains("web pages"), "{err}");
        }
        assert!(check_not_served(&sibling, &served).is_ok());
        assert!(
            check_not_served(&dir, &served).is_ok(),
            "a parent is not served"
        );
        assert!(check_not_served(&sibling, &dir.join("no-such-dist")).is_ok());
    }

    #[test]
    fn a_backup_taken_while_the_clock_is_behind_deletes_no_real_backup() {
        // A Pi comes up with an old fake-hwclock time and backs up before NTP
        // syncs: its backup is stamped before the real ones, which rotation
        // must keep rather than treat as clock errors.
        let dir = temp_dir("backup-clock-behind");
        let db = seeded_db(&dir);
        let dest = dir.join("out");
        std::fs::create_dir(&dest).unwrap();
        for stamp in ["20261019-120000", "20261020-120000", "20261021-120000"] {
            std::fs::write(dest.join(format!("history-{stamp}.db")), b"real").unwrap();
        }
        let behind = 1_767_225_600; // 2026-01-01 00:00:00 UTC
        run_backup(
            &db,
            &dir.join("s.json"),
            &config_into(&dest, 3),
            &dir,
            &at(behind),
        )
        .unwrap();
        assert_eq!(
            names(&dest),
            vec![
                "history-20260101-000000.db",
                "history-20261019-120000.db",
                "history-20261020-120000.db",
                "history-20261021-120000.db",
            ]
        );
    }

    #[test]
    fn rotation_reports_what_it_deleted() {
        let dir = temp_dir("backup-rotate-report");
        for stamp in ["20261020-120000", "20261021-120000", "20261022-120000"] {
            std::fs::write(dir.join(format!("history-{stamp}.db")), b"x").unwrap();
        }
        let deleted = rotate(&dir, 1, "20261022-120000").unwrap();
        assert_eq!(
            deleted,
            vec![
                dir.join("history-20261021-120000.db"),
                dir.join("history-20261020-120000.db"),
            ]
        );
    }

    #[test]
    fn a_failed_integrity_check_leaves_the_previous_backup_intact() {
        let dir = temp_dir("backup-integrity");
        let db = seeded_db(&dir);
        let dest = dir.join("out");
        std::fs::create_dir(&dest).unwrap();
        let config = config_into(&dest, 1);
        let settings = dir.join("settings.json");
        let good = run_backup(&db, &settings, &config, &dir, &at(NOON)).unwrap();
        let good_bytes = std::fs::read(&good.path).unwrap();

        let err = run_backup_with(
            &db,
            &settings,
            &config,
            &dir,
            &at(NOON + 86_400),
            |_| Err("backup integrity check returned corrupt".to_string()),
            |_| Ok(u64::MAX),
        )
        .unwrap_err();
        assert!(err.contains("integrity"), "{err}");
        // The good backup survives (keep=1 rotation must not have run), and
        // no temporary or partial file is left behind.
        assert_eq!(names(&dest), vec!["history-20261021-120000.db"]);
        assert_eq!(std::fs::read(&good.path).unwrap(), good_bytes);
    }

    #[test]
    fn too_little_free_space_skips_the_backup_and_keeps_the_last_one() {
        let dir = temp_dir("backup-space");
        let db = seeded_db(&dir);
        let dest = dir.join("out");
        std::fs::create_dir(&dest).unwrap();
        let config = config_into(&dest, 1);
        let settings = dir.join("settings.json");
        run_backup(&db, &settings, &config, &dir, &at(NOON)).unwrap();
        let db_len = std::fs::metadata(&db).unwrap().len();

        // Exactly the database's size is not enough: a half-size margin is kept.
        let err = run_backup_with(
            &db,
            &settings,
            &config,
            &dir,
            &at(NOON + 86_400),
            crate::history::verify_integrity,
            |_| Ok(db_len),
        )
        .unwrap_err();
        assert!(err.contains("free space"), "{err}");
        assert_eq!(names(&dest), vec!["history-20261021-120000.db"]);

        // Enough room: the backup goes ahead.
        run_backup_with(
            &db,
            &settings,
            &config,
            &dir,
            &at(NOON + 86_400),
            crate::history::verify_integrity,
            |_| Ok(db_len * 2),
        )
        .unwrap();
        assert_eq!(names(&dest), vec!["history-20261022-120000.db"]);
    }

    #[test]
    fn a_failed_free_space_lookup_does_not_block_the_backup() {
        let dir = temp_dir("backup-space-unknown");
        let db = seeded_db(&dir);
        let outcome = run_backup_with(
            &db,
            &dir.join("settings.json"),
            &HistoryBackupConfig::default(),
            &dir,
            &at(NOON),
            crate::history::verify_integrity,
            |_| Err(std::io::Error::other("statvfs unsupported")),
        );
        assert!(outcome.is_ok(), "{outcome:?}");
    }

    #[test]
    fn a_missing_destination_is_an_error_and_is_not_created() {
        let dir = temp_dir("backup-missing");
        let db = seeded_db(&dir);
        let dest = dir.join("nas-not-mounted");
        let err = run_backup(
            &db,
            &dir.join("settings.json"),
            &config_into(&dest, 7),
            &dir,
            &at(NOON),
        )
        .unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
        assert!(!dest.exists());
    }

    #[cfg(unix)]
    #[test]
    fn an_unwritable_destination_is_an_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("backup-readonly");
        let db = seeded_db(&dir);
        let dest = dir.join("readonly");
        std::fs::create_dir(&dest).unwrap();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::write(dest.join("probe"), b"").is_ok() {
            // Running as root: permissions don't apply.
            eprintln!("skipping: destination is writable despite mode 0555 (root?)");
            return;
        }
        let err = run_backup(
            &db,
            &dir.join("settings.json"),
            &config_into(&dest, 7),
            &dir,
            &at(NOON),
        )
        .unwrap_err();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(err.contains("failed to create backup"), "{err}");
        assert!(names(&dest).is_empty());
    }

    #[test]
    fn a_missing_settings_file_fails_the_backup_without_rotating() {
        let dir = temp_dir("backup-settings-missing");
        let db = seeded_db(&dir);
        let dest = dir.join("out");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("history-20200101-000000.db"), b"old").unwrap();
        let mut config = config_into(&dest, 1);
        config.include_settings = true;
        let err = run_backup(&db, &dir.join("absent.json"), &config, &dir, &at(NOON)).unwrap_err();
        assert!(err.contains("settings"), "{err}");
        // All or nothing: the new history copy is removed too, so hourly
        // retries can't pile up copies that rotation never sees.
        assert_eq!(names(&dest), vec!["history-20200101-000000.db"]);
    }

    #[test]
    fn file_names_are_stamped_in_utc() {
        // Local time would repeat an hour when the clocks go back, so names
        // could collide or sort out of order.
        let dir = temp_dir("backup-utc");
        let db = seeded_db(&dir);
        let bst = chrono::FixedOffset::east_opt(3600).unwrap();
        let outcome = run_backup(
            &db,
            &dir.join("settings.json"),
            &HistoryBackupConfig::default(),
            &dir,
            &bst.timestamp_opt(NOON, 0).unwrap(),
        )
        .unwrap();
        assert_eq!(
            outcome.path.file_name().unwrap(),
            "history-20261021-120000.db"
        );
    }

    #[test]
    fn next_due_is_never_in_the_past_and_follows_retries() {
        let daily = HistoryBackupConfig::default();
        let now = NOON + 1000;
        // Never backed up: due now.
        assert_eq!(next_due_at(&daily, None, None, now), Some(now));
        // After a success: one interval later.
        assert_eq!(
            next_due_at(&daily, Some(NOON), Some(NOON), now),
            Some(NOON + 86_400)
        );
        // After a failure past the due time: an hour after that attempt.
        let failed = NOON + 86_400;
        assert_eq!(
            next_due_at(&daily, Some(NOON), Some(failed), failed + 60),
            Some(failed + RETRY_SECS)
        );
        // Overdue (missed while HEM was off): due now, not in the past.
        assert_eq!(
            next_due_at(&daily, Some(NOON), Some(NOON), NOON + 200_000),
            Some(NOON + 200_000)
        );
        // Off: never.
        let off = HistoryBackupConfig {
            enabled: false,
            ..daily
        };
        assert_eq!(next_due_at(&off, Some(NOON), Some(NOON), now), None);
    }

    #[test]
    fn next_due_and_backup_due_agree() {
        let config = HistoryBackupConfig {
            interval_hours: 6,
            ..HistoryBackupConfig::default()
        };
        for (success, attempt) in [
            (None, None),
            (Some(NOON), Some(NOON)),
            (Some(NOON), Some(NOON + 30_000)),
        ] {
            for now in (NOON..NOON + 90_000).step_by(600) {
                let due = next_due_at(&config, success, attempt, now).unwrap();
                assert_eq!(
                    backup_due(&config, success, attempt, now),
                    due <= now,
                    "{success:?} {attempt:?} at {now}"
                );
            }
        }
    }

    #[test]
    fn destinations_must_be_empty_or_an_existing_absolute_directory() {
        let dir = temp_dir("backup-validate");
        assert!(validate_destination("").is_ok());
        assert!(validate_destination("   ").is_ok());
        assert!(validate_destination(&dir.to_string_lossy()).is_ok());
        assert!(validate_destination("relative/path")
            .unwrap_err()
            .contains("full path"));
        assert!(validate_destination(&dir.join("absent").to_string_lossy())
            .unwrap_err()
            .contains("does not exist"));
        let file = dir.join("file");
        std::fs::write(&file, b"").unwrap();
        assert!(validate_destination(&file.to_string_lossy())
            .unwrap_err()
            .contains("not a folder"));
    }

    #[test]
    fn backups_fall_due_on_the_interval_and_retry_failures_hourly() {
        let daily = HistoryBackupConfig::default();
        // Never backed up: due straight away.
        assert!(backup_due(&daily, None, None, NOON));
        // Backed up an hour ago: not due until a day has passed.
        assert!(!backup_due(&daily, Some(NOON), Some(NOON), NOON + 3600));
        assert!(!backup_due(&daily, Some(NOON), Some(NOON), NOON + 86_399));
        assert!(backup_due(&daily, Some(NOON), Some(NOON), NOON + 86_400));
        // A failed attempt after the due time waits an hour before retrying.
        let failed = NOON + 86_400;
        assert!(!backup_due(&daily, Some(NOON), Some(failed), failed + 3599));
        assert!(backup_due(&daily, Some(NOON), Some(failed), failed + 3600));
        // A clock stepped back behind the last backup doesn't stall the schedule.
        assert!(backup_due(&daily, Some(NOON), Some(NOON), NOON - 60));
        // Disabled: never due.
        let off = HistoryBackupConfig {
            enabled: false,
            ..daily.clone()
        };
        assert!(!backup_due(&off, None, None, NOON));
        // Hourly: the retry wait never exceeds the interval.
        let hourly = HistoryBackupConfig {
            interval_hours: 1,
            ..daily
        };
        assert!(backup_due(&hourly, Some(NOON), Some(NOON), NOON + 3600));
    }
}
