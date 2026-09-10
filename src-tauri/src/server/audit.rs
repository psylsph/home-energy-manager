//! Bounded SQLite audit trail for security and control events (U3).
//!
//! The audit log records authentication results, authorization denials,
//! credential/configuration changes, rate-limit activations, and external
//! actions with enough context to reconstruct activity — and *without*
//! secrets: actors are identified by a one-way fingerprint of the presented
//! token, never the token itself.
//!
//! Storage is a dedicated SQLite database in the HEM config directory, kept
//! bounded by row count and age (oldest rows are deleted deterministically).
//! Writers decide the failure policy: control mutations fail closed (a
//! command is not queued unless its audit record was durably written) while
//! reads and authentication responses fail open with a local warning.
//!
//! The audit trail is never exposed through either HTTP API.

use std::path::PathBuf;
use std::sync::Mutex;

use chrono::Utc;
use rusqlite::Connection;

/// Audit retention: at most this many rows…
const MAX_ROWS: i64 = 10_000;
/// …and never older than this many days.
const MAX_AGE_DAYS: i64 = 30;

/// One audit event.
#[derive(Debug, Clone)]
pub struct AuditEvent {
    pub kind: &'static str,
    /// One-way fingerprint of the acting credential, if any.
    pub actor: Option<String>,
    /// Trusted source address as observed at the socket (or forwarded by a
    /// trusted proxy).
    pub source: Option<String>,
    pub method: Option<String>,
    pub path: Option<String>,
    /// Sanitized outcome (`ok`, `denied`, `rate_limited`, `error`, …).
    pub outcome: &'static str,
    /// Sanitized detail (e.g. the requested duration). Never raw bodies.
    pub detail: Option<String>,
}

/// Compute the audit fingerprint for a presented bearer token: the first 12
/// hex characters of SHA-256. One-way — the token cannot be recovered, and
/// high-entropy generated tokens make brute force infeasible.
pub fn token_fingerprint(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let hash = hasher.finalize();
    hash.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// A bounded SQLite-backed audit log. Cheap to clone into handlers via
/// `Arc`; the connection is opened lazily so tests and early startup never
/// touch the filesystem until an event is actually recorded.
pub struct AuditLog {
    path: Mutex<Option<PathBuf>>,
    connection: Mutex<Option<Connection>>,
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}

impl AuditLog {
    /// An audit log stored in the current settings directory (`audit.db`).
    /// The path is resolved lazily so config-directory overrides (tests,
    /// portable installs) are honoured on first use.
    pub fn new() -> Self {
        Self {
            path: Mutex::new(None),
            connection: Mutex::new(None),
        }
    }

    /// Re-point an existing audit log at another database (test-only: lets
    /// router tests swap in a broken store to prove fail-closed behaviour).
    #[cfg(test)]
    pub(crate) fn override_path(&self, path: PathBuf) {
        *self.path.lock().unwrap() = Some(path);
        *self.connection.lock().unwrap() = None;
    }

    /// Override the database path (used by tests to isolate writes).
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            path: Mutex::new(Some(path)),
            connection: Mutex::new(None),
        }
    }

    fn with_connection<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut cached = self.connection.lock().unwrap();
        if cached.is_none() {
            let path = {
                let mut configured = self.path.lock().unwrap();
                if configured.is_none() {
                    *configured = Some(crate::settings::Settings::settings_dir().join("audit.db"));
                }
                configured.clone().unwrap()
            };
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("failed to create audit dir: {e}"))?;
            }
            let connection =
                Connection::open(&path).map_err(|e| format!("failed to open audit db: {e}"))?;
            connection
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS audit_events (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        ts TEXT NOT NULL,
                        kind TEXT NOT NULL,
                        actor TEXT,
                        source TEXT,
                        method TEXT,
                        path TEXT,
                        outcome TEXT NOT NULL,
                        detail TEXT
                    );
                    CREATE INDEX IF NOT EXISTS audit_events_ts ON audit_events(ts);",
                )
                .map_err(|e| format!("failed to init audit db: {e}"))?;
            *cached = Some(connection);
        }
        f(cached.as_ref().expect("connection initialized"))
    }

    /// Record one event, applying bounded retention. Returns `Err` when the
    /// event could not be durably written — callers decide fail-open vs
    /// fail-closed.
    pub fn record(&self, event: AuditEvent) -> Result<(), String> {
        self.with_connection(|connection| {
            let ts = Utc::now().to_rfc3339();
            connection
                .execute(
                    "INSERT INTO audit_events (ts, kind, actor, source, method, path, outcome, detail)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        ts,
                        event.kind,
                        event.actor,
                        event.source,
                        event.method,
                        event.path,
                        event.outcome,
                        event.detail,
                    ],
                )
                .map_err(|e| format!("audit insert failed: {e}"))?;
            // Bounded retention: drop rows beyond the count cap or age cap.
            connection
                .execute(
                    "DELETE FROM audit_events WHERE id <= (
                         SELECT id FROM audit_events ORDER BY id DESC LIMIT 1 OFFSET ?1
                     )",
                    rusqlite::params![MAX_ROWS],
                )
                .map_err(|e| format!("audit retention failed: {e}"))?;
            connection
                .execute(
                    "DELETE FROM audit_events WHERE ts < datetime('now', ?1)",
                    rusqlite::params![format!("-{MAX_AGE_DAYS} days")],
                )
                .map_err(|e| format!("audit age retention failed: {e}"))?;
            Ok(())
        })
    }

    /// Number of retained events (tests, diagnostics).
    pub fn count(&self) -> Result<i64, String> {
        self.with_connection(|connection| {
            connection
                .query_row("SELECT COUNT(*) FROM audit_events", [], |row| row.get(0))
                .map_err(|e| format!("audit count failed: {e}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated_audit() -> (std::path::PathBuf, AuditLog) {
        let dir = crate::test_util::make_unique_test_dir("audit");
        let path = dir.join("audit.db");
        (path.clone(), AuditLog::with_path(path))
    }

    fn event(kind: &'static str, outcome: &'static str) -> AuditEvent {
        AuditEvent {
            kind,
            actor: Some(token_fingerprint("some-token")),
            source: Some("127.0.0.1".to_string()),
            method: Some("GET".to_string()),
            path: Some("/api/control/status".to_string()),
            outcome,
            detail: None,
        }
    }

    #[test]
    fn records_events_and_is_queryable() {
        let (_path, audit) = isolated_audit();
        audit.record(event("auth_success", "ok")).unwrap();
        audit.record(event("auth_failure", "denied")).unwrap();
        assert_eq!(audit.count().unwrap(), 2);
    }

    #[test]
    fn fingerprints_are_one_way_and_stable() {
        let a = token_fingerprint("secret-value");
        assert_eq!(a, token_fingerprint("secret-value"));
        assert_ne!(a, token_fingerprint("secret-value-2"));
        assert_eq!(a.len(), 12);
        assert!(!a.contains("secret"));
    }

    #[test]
    fn retention_caps_row_count() {
        let (_path, audit) = isolated_audit();
        for i in 0..(MAX_ROWS + 250) {
            audit
                .record(AuditEvent {
                    kind: "auth_success",
                    actor: None,
                    source: None,
                    method: None,
                    path: None,
                    outcome: "ok",
                    detail: Some(format!("seq-{i}")),
                })
                .unwrap();
        }
        assert!(
            audit.count().unwrap() <= MAX_ROWS,
            "retention must cap row count"
        );
    }
}
