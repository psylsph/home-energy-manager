//! Durable command ledger for external Quick Action mutations (U5).
//!
//! One SQLite-backed table serves two duties: idempotent replay of external
//! mutations (U6's `Idempotency-Key` contract) and honest command state —
//! `accepted → queued → dispatched → readback_confirmed`, with `failed`,
//! `expired` and `unknown` for everything the inverter has not confirmed.
//!
//! Guarantees:
//!
//! * **Reservation before enqueue.** A command row is committed before any
//!   Modbus write is queued; only the reservation's owner may enqueue. A
//!   crash between reservation and dispatch leaves a replayable row — a
//!   retry replays or resumes it instead of enqueueing blindly.
//! * **One active action per kind.** A partial unique index means a second
//!   start for the same action kind cannot create a second owner (a repeat
//!   start must resolve to the existing command or conflict).
//! * **Bounded.** Finished commands are trimmed to a retention window that
//!   comfortably exceeds the 1439-minute maximum action plus a normal retry
//!   window.
//! * **No silent resume.** On startup every still-in-progress command is
//!   marked `unknown` and reported, so a restart can never re-arm an action
//!   and callers never mistake staleness for success.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{extract::State, Json};
use chrono::Utc;
use rusqlite::OptionalExtension;

use crate::inverter::poll::AppState;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

/// How long finished/failed/expired commands are retained (ms): 24 h —
/// comfortably above the 1439-minute maximum action and any sane retry
/// window, while keeping the table bounded.
const RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

/// Reserve outcome for a mutation request.
#[derive(Debug)]
pub enum Reservation {
    /// Freshly reserved — caller may proceed to queue exactly one command.
    Accepted { command_id: String },
    /// The same scope was already used and finished: replay the stored
    /// response verbatim.
    Replayed { response: Value },
    /// The same scope (or the same active action) is still in progress.
    InProgress { command_id: String },
    /// Same idempotency scope with a different request payload.
    Conflict { existing_command_id: String },
}

/// Facts about the inverter's current state, derived from a fresh sanitized
/// readback, used to advance command states.
#[derive(Debug, Clone, Copy)]
pub struct ReadbackEvidence {
    /// Millisecond timestamp of the snapshot the facts came from.
    pub snapshot_ts_ms: i64,
    /// Strict "force-charge predicate" (enable_charge && Eco mode && window
    /// && not paused), as computed for the status endpoint.
    pub charge_active: bool,
    /// Strict force-discharge predicate.
    pub discharge_active: bool,
    /// Current wall-clock milliseconds.
    pub now_ms: i64,
}

/// A row of the ledger (for the status endpoint).
#[derive(Debug, Clone)]
pub struct CommandRecord {
    pub command_id: String,
    pub action: String,
    pub state: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub expires_at_ms: Option<i64>,
    pub detail: Option<String>,
}

/// Bounded SQLite-backed ledger. Lazy connection like [`crate::server::audit::AuditLog`].
pub struct CommandLedger {
    path: Mutex<Option<PathBuf>>,
    connection: Mutex<Option<Connection>>,
}

use std::sync::Mutex;

impl Default for CommandLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandLedger {
    pub fn new() -> Self {
        Self {
            path: Mutex::new(None),
            connection: Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub(crate) fn override_path(&self, path: PathBuf) {
        *self.path.lock().unwrap() = Some(path);
        *self.connection.lock().unwrap() = None;
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
                    *configured = Some(
                        crate::settings::Settings::settings_dir().join("external_commands.db"),
                    );
                }
                configured.clone().unwrap()
            };
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("failed to create commands dir: {e}"))?;
            }
            let connection =
                Connection::open(&path).map_err(|e| format!("failed to open commands db: {e}"))?;
            connection
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS external_commands (
                        id TEXT PRIMARY KEY,
                        scope TEXT NOT NULL,
                        fingerprint TEXT NOT NULL,
                        endpoint TEXT NOT NULL,
                        action TEXT NOT NULL,
                        is_start INTEGER NOT NULL,
                        request_hash TEXT NOT NULL,
                        state TEXT NOT NULL,
                        expires_at_ms INTEGER,
                        recovery TEXT,
                        response TEXT,
                        created_ms INTEGER NOT NULL,
                        updated_ms INTEGER NOT NULL
                    );
                    CREATE UNIQUE INDEX IF NOT EXISTS external_commands_scope
                        ON external_commands(scope);
                    CREATE UNIQUE INDEX IF NOT EXISTS external_commands_active_action
                        ON external_commands(action)
                        WHERE is_start = 1
                          AND state IN ('accepted','queued','dispatched');
                    CREATE INDEX IF NOT EXISTS external_commands_updated
                        ON external_commands(updated_ms);",
                )
                .map_err(|e| format!("failed to init commands db: {e}"))?;
            *cached = Some(connection);
        }
        f(cached.as_ref().expect("connection initialized"))
    }

    /// Reserve a start command. `action` is `force_charge` or
    /// `force_discharge`; `idem_key` scopes replay; the active-action index
    /// guarantees a single live start per action kind.
    pub fn reserve_start(
        &self,
        fingerprint: &str,
        action: &str,
        minutes: u64,
        idem_key: &str,
        now_ms: i64,
    ) -> Result<Reservation, String> {
        let endpoint = format!("/api/control/{action}");
        let request_hash = format!("minutes={minutes}");
        let scope = format!("{fingerprint}:{endpoint}:{idem_key}");
        let _ = endpoint;
        let command_id = new_command_id();
        let expires_at_ms = now_ms + (minutes as i64) * 60_000;
        let inserted = self.with_connection(|connection| {
            let mut stmt = connection
                .prepare(
                    "INSERT OR IGNORE INTO external_commands
                     (id, scope, fingerprint, endpoint, action, is_start, request_hash, state,
                      expires_at_ms, created_ms, updated_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, 'accepted', ?7, ?8, ?8)",
                )
                .map_err(|e| format!("reserve failed: {e}"))?;
            let changed = stmt
                .execute(params![
                    command_id,
                    scope,
                    fingerprint,
                    endpoint,
                    action,
                    request_hash,
                    expires_at_ms,
                    now_ms
                ])
                .map_err(|e| format!("reserve execute failed: {e}"))?;
            Ok(changed)
        })?;
        if inserted == 1 {
            return Ok(Reservation::Accepted { command_id });
        }
        self.resolve_duplicate(&scope, &request_hash, action)
    }

    /// Reserve a stop command. Stops never conflict with the active-action
    /// index (they unwind it); replay/conflict follow the same scope rules.
    pub fn reserve_stop(
        &self,
        fingerprint: &str,
        action: &str,
        idem_key: &str,
        now_ms: i64,
    ) -> Result<Reservation, String> {
        let endpoint = format!("/api/control/{action}/stop");
        let request_hash = "stop".to_string();
        let scope = format!("{fingerprint}:{endpoint}:{idem_key}");
        let command_id = new_command_id();
        let inserted = self.with_connection(|connection| {
            let mut stmt = connection
                .prepare(
                    "INSERT OR IGNORE INTO external_commands
                     (id, scope, fingerprint, endpoint, action, is_start, request_hash, state,
                      created_ms, updated_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, 'accepted', ?7, ?7)",
                )
                .map_err(|e| format!("reserve failed: {e}"))?;
            let changed = stmt
                .execute(params![
                    command_id,
                    scope,
                    fingerprint,
                    endpoint,
                    action,
                    request_hash,
                    now_ms
                ])
                .map_err(|e| format!("reserve execute failed: {e}"))?;
            Ok(changed)
        })?;
        if inserted == 1 {
            return Ok(Reservation::Accepted { command_id });
        }
        self.resolve_duplicate(&scope, &request_hash, action)
    }

    /// Classify a scope collision: replay a finished response, report the
    /// in-progress command, or flag a payload conflict.
    fn resolve_duplicate(
        &self,
        scope: &str,
        request_hash: &str,
        action: &str,
    ) -> Result<Reservation, String> {
        self.with_connection(|connection| {
            let existing = connection
                .query_row(
                    "SELECT id, request_hash, state, response FROM external_commands
                     WHERE scope = ?1",
                    params![scope],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| format!("duplicate lookup failed: {e}"))?;
            if let Some((command_id, existing_hash, _state, response)) = existing {
                if existing_hash != request_hash {
                    return Ok(Reservation::Conflict {
                        existing_command_id: command_id,
                    });
                }
                if let Some(response) = response {
                    let parsed = serde_json::from_str(&response)
                        .map_err(|e| format!("stored response unparsable: {e}"))?;
                    return Ok(Reservation::Replayed { response: parsed });
                }
                return Ok(Reservation::InProgress { command_id });
            }
            // Scope is fresh but a live action of this kind already exists:
            // treat as in-progress so the caller addresses the running
            // command instead of creating a competing one.
            let active = connection
                .query_row(
                    "SELECT id FROM external_commands
                     WHERE action = ?1 AND is_start = 1
                       AND state IN ('accepted','queued','dispatched')",
                    params![action],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|e| format!("active lookup failed: {e}"))?;
            match active {
                Some(command_id) => Ok(Reservation::InProgress { command_id }),
                None => Err(format!(
                    "command scope {scope:?} freed between reserve attempts; retry"
                )),
            }
        })
    }

    /// Transition a command's state. No-op if the command is already in a
    /// terminal state (`readback_confirmed`/`failed`/`expired`/`unknown`
    /// are never overwritten by queue/dispatch bookkeeping).
    pub fn mark_state(&self, command_id: &str, state: &str) -> Result<(), String> {
        self.with_connection(|connection| {
            connection
                .execute(
                    "UPDATE external_commands SET state = ?2, updated_ms = ?3
                     WHERE id = ?1 AND state NOT IN
                       ('readback_confirmed','failed','expired','unknown')",
                    params![command_id, state, Utc::now().timestamp_millis()],
                )
                .map_err(|e| format!("state update failed: {e}"))?;
            Ok(())
        })?;
        self.trim()?;
        Ok(())
    }

    /// Record the recovery baseline captured for a start (serialized revert
    /// snapshot) so a restart can reconcile instead of guessing.
    pub fn record_recovery(&self, command_id: &str, recovery_json: &str) -> Result<(), String> {
        self.with_connection(|connection| {
            connection
                .execute(
                    "UPDATE external_commands SET recovery = ?2 WHERE id = ?1",
                    params![command_id, recovery_json],
                )
                .map_err(|e| format!("recovery write failed: {e}"))?;
            Ok(())
        })
    }

    /// Attach the final response envelope to a command and freeze its state.
    pub fn finish(&self, command_id: &str, state: &str, response_json: &str) -> Result<(), String> {
        self.with_connection(|connection| {
            connection
                .execute(
                    "UPDATE external_commands
                     SET state = ?2, response = ?3, updated_ms = ?4
                     WHERE id = ?1",
                    params![
                        command_id,
                        state,
                        response_json,
                        Utc::now().timestamp_millis()
                    ],
                )
                .map_err(|e| format!("finish failed: {e}"))?;
            Ok(())
        })?;
        self.trim()?;
        Ok(())
    }

    /// Advance in-progress commands from fresh inverter evidence.
    ///
    /// Evidence is causally compared against the command's creation time: a
    /// readback from *before* the command existed can never confirm it.
    /// Returns the number of commands whose state changed.
    pub fn advance_readback(&self, evidence: &ReadbackEvidence) -> Result<usize, String> {
        self.with_connection(|connection| {
            let mut stmt = connection
                .prepare(
                    "SELECT id, action, is_start, state, expires_at_ms, created_ms
                     FROM external_commands
                     WHERE state IN ('accepted','queued','dispatched')",
                )
                .map_err(|e| format!("advance select failed: {e}"))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                })
                .map_err(|e| format!("advance query failed: {e}"))?;
            let commands: Vec<_> = rows
                .filter_map(|row| row.ok())
                .collect::<Vec<_>>()
                .into_iter()
                .collect();
            drop(stmt);
            let mut changed = 0;
            for (id, action, is_start, state, expires_at_ms, created_ms) in commands {
                let causally_fresh = evidence.snapshot_ts_ms > created_ms;
                let active = match action.as_str() {
                    "force_charge" => evidence.charge_active,
                    "force_discharge" => evidence.discharge_active,
                    _ => false,
                };
                let new_state = if is_start == 1 {
                    if causally_fresh && active {
                        Some("readback_confirmed")
                    } else if let Some(expires_at_ms) = expires_at_ms {
                        if evidence.now_ms > expires_at_ms {
                            if causally_fresh && !active {
                                Some("expired")
                            } else if evidence.now_ms > expires_at_ms + 300_000 {
                                // Deadline well passed with still no
                                // confirming evidence: honest unknown.
                                Some("unknown")
                            } else {
                                None
                            }
                        } else if state == "accepted" {
                            // The writes are queued but not yet observed.
                            Some("queued")
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    // Stops confirm when the action is no longer active.
                    if causally_fresh && !active {
                        Some("readback_confirmed")
                    } else {
                        None
                    }
                };
                if let Some(new_state) = new_state {
                    if new_state != state {
                        // updated_ms is real wall-clock (retention trim compares
                        // against it); evidence.now_ms is only for deadlines.
                        connection
                            .execute(
                                "UPDATE external_commands SET state = ?2, updated_ms = ?3
                                 WHERE id = ?1",
                                params![id, new_state, Utc::now().timestamp_millis()],
                            )
                            .map_err(|e| format!("advance update failed: {e}"))?;
                        changed += 1;
                    }
                }
            }
            Ok(changed)
        })?;
        self.trim()?;
        Ok(0)
    }

    /// Mark every `queued` command `dispatched` (the poll loop picked the
    /// pending writes up).
    pub fn mark_dispatched(&self) -> Result<(), String> {
        self.with_connection(|connection| {
            connection
                .execute(
                    "UPDATE external_commands SET state = 'dispatched', updated_ms = ?1
                     WHERE state = 'queued'",
                    params![Utc::now().timestamp_millis()],
                )
                .map_err(|e| format!("dispatch mark failed: {e}"))?;
            Ok(())
        })
    }

    /// Attach a response envelope without changing the lifecycle state (an
    /// accepted command keeps advancing on readback evidence; its stored
    /// response is what replays return).
    pub fn store_response(&self, command_id: &str, response_json: &str) -> Result<(), String> {
        self.with_connection(|connection| {
            connection
                .execute(
                    "UPDATE external_commands SET response = ?2 WHERE id = ?1",
                    params![command_id, response_json],
                )
                .map_err(|e| format!("response store failed: {e}"))?;
            Ok(())
        })
    }

    /// Mark every in-progress command `unknown` (startup reconciliation) and
    /// return how many were stranded.
    pub fn reconcile_startup(&self) -> Result<usize, String> {
        self.with_connection(|connection| {
            let now_ms = Utc::now().timestamp_millis();
            let stranded = connection
                .execute(
                    "UPDATE external_commands SET state = 'unknown', updated_ms = ?1
                     WHERE state IN ('accepted','queued','dispatched')",
                    params![now_ms],
                )
                .map_err(|e| format!("startup reconcile failed: {e}"))?;
            Ok(stranded)
        })
    }

    /// Fetch one command (status endpoint).
    pub fn get(&self, command_id: &str) -> Result<Option<CommandRecord>, String> {
        self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT id, action, state, created_ms, updated_ms, expires_at_ms, recovery
                     FROM external_commands WHERE id = ?1",
                    params![command_id],
                    |row| {
                        Ok(CommandRecord {
                            command_id: row.get(0)?,
                            action: row.get(1)?,
                            state: row.get(2)?,
                            created_ms: row.get(3)?,
                            updated_ms: row.get(4)?,
                            expires_at_ms: row.get(5)?,
                            detail: row.get(6)?,
                        })
                    },
                )
                .optional()
                .map_err(|e| format!("command lookup failed: {e}"))
        })
    }

    /// Whether a start command for `action` is currently active (accepted,
    /// queued or dispatched). Used by recovery stops after permission
    /// revocation.
    pub fn has_active_start(&self, action: &str) -> Result<bool, String> {
        self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM external_commands
                     WHERE action = ?1 AND is_start = 1
                       AND state IN ('accepted','queued','dispatched')",
                    params![action],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count > 0)
                .map_err(|e| format!("active-start lookup failed: {e}"))
        })
    }

    /// Drop finished commands past the retention window.
    fn trim(&self) -> Result<(), String> {
        self.with_connection(|connection| {
            connection
                .execute(
                    "DELETE FROM external_commands
                     WHERE state IN ('readback_confirmed','failed','expired','unknown')
                       AND updated_ms < ?1",
                    params![Utc::now().timestamp_millis() - RETENTION_MS],
                )
                .map_err(|e| format!("ledger trim failed: {e}"))?;
            Ok(())
        })
    }
}

/// GET /api/commands/{command_id} on the authenticated router: the current
/// lifecycle state of an external command, with explicit unknown handling —
/// a missing id is a 404, a ledger failure a 500, and a command stranded by
/// a restart reports `unknown` (never `confirmed`).
pub async fn command_status(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(command_id): axum::extract::Path<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match state.command_ledger.get(&command_id) {
        Ok(Some(record)) => {
            let body = json!({
                "ok": true,
                "data": {
                    "command_id": record.command_id,
                    "action": record.action,
                    "state": record.state,
                    "created_ms": record.created_ms,
                    "updated_ms": record.updated_ms,
                    "expires_at_ms": record.expires_at_ms,
                },
            });
            (axum::http::StatusCode::OK, Json(body)).into_response()
        }
        Ok(None) => (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "Unknown command id"})),
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e})),
        )
            .into_response(),
    }
}

pub(crate) fn new_command_id() -> String {
    // 16 random bytes hex — CSPRNG via getrandom, same source as the API
    // credential; 128 bits of command id is collision-proof for this use.
    use sha2::{Digest, Sha256};
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("OS CSPRNG must be available");
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let hash = hasher.finalize();
    hash.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated_ledger() -> CommandLedger {
        let dir = crate::test_util::make_unique_test_dir("commands");
        let ledger = CommandLedger::new();
        ledger.override_path(dir.join("external_commands.db"));
        ledger
    }

    #[test]
    fn reserve_creates_then_replays_the_same_scope() {
        let ledger = isolated_ledger();
        let first = ledger
            .reserve_start("fp", "force_charge", 30, "key-1", 1_000)
            .unwrap();
        let command_id = match &first {
            Reservation::Accepted { command_id } => command_id.clone(),
            other => panic!("expected accepted, got {other:?}"),
        };
        // Attach a final response so a replay can return it.
        ledger
            .finish(
                &command_id,
                "readback_confirmed",
                r#"{"ok":true,"message":"Force charge enabled","command_id":"X"}"#,
            )
            .unwrap();
        match ledger
            .reserve_start("fp", "force_charge", 30, "key-1", 2_000)
            .unwrap()
        {
            Reservation::Replayed { response } => {
                assert_eq!(response["message"], "Force charge enabled");
            }
            other => panic!("expected replay, got {other:?}"),
        }
    }

    #[test]
    fn same_scope_with_different_payload_conflicts() {
        let ledger = isolated_ledger();
        ledger
            .reserve_start("fp", "force_charge", 30, "key-1", 1_000)
            .unwrap();
        match ledger
            .reserve_start("fp", "force_charge", 90, "key-1", 2_000)
            .unwrap()
        {
            Reservation::Conflict { .. } => {}
            other => panic!("expected conflict, got {other:?}"),
        }
    }

    #[test]
    fn second_active_start_for_a_kind_is_in_progress_not_a_new_command() {
        let ledger = isolated_ledger();
        let first = ledger
            .reserve_start("fp-a", "force_charge", 30, "key-1", 1_000)
            .unwrap();
        let command_id = match first {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        // A different key (and even a different caller) must not create a
        // second live start for the same action.
        match ledger
            .reserve_start("fp-b", "force_charge", 90, "key-2", 2_000)
            .unwrap()
        {
            Reservation::InProgress {
                command_id: existing,
            } => {
                assert_eq!(existing, command_id)
            }
            other => panic!("expected in-progress, got {other:?}"),
        }
    }

    #[test]
    fn readback_confirms_only_causally_newer_evidence() {
        let ledger = isolated_ledger();
        let first = ledger
            .reserve_start("fp", "force_charge", 60, "key-1", 10_000)
            .unwrap();
        let command_id = match first {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&command_id, "queued").unwrap();

        // Stale evidence (from before the command existed) confirms nothing.
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 5_000,
                charge_active: true,
                discharge_active: false,
                now_ms: 11_000,
            })
            .unwrap();
        assert_eq!(ledger.get(&command_id).unwrap().unwrap().state, "queued");

        // Fresh matching evidence confirms.
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 12_000,
                charge_active: true,
                discharge_active: false,
                now_ms: 13_000,
            })
            .unwrap();
        assert_eq!(
            ledger.get(&command_id).unwrap().unwrap().state,
            "readback_confirmed"
        );
    }

    #[test]
    fn expired_deadline_without_evidence_becomes_unknown() {
        let ledger = isolated_ledger();
        let first = ledger
            .reserve_start("fp", "force_discharge", 1, "key-1", 1_000)
            .unwrap();
        let command_id = match first {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        // Deadline is now + 60s. Shortly after the deadline with no fresh
        // matching evidence the command stays as-is…
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 500,
                charge_active: false,
                discharge_active: false,
                now_ms: 70_000,
            })
            .unwrap();
        assert_eq!(ledger.get(&command_id).unwrap().unwrap().state, "accepted");
        // …but well past it (grace elapsed) it becomes unknown.
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 500,
                charge_active: false,
                discharge_active: false,
                now_ms: 70_000 + 300_000,
            })
            .unwrap();
        assert_eq!(ledger.get(&command_id).unwrap().unwrap().state, "unknown");
        // With fresh negative evidence right after the deadline: expired.
        let ledger2 = isolated_ledger();
        let first = ledger2
            .reserve_start("fp", "force_discharge", 1, "key-1", 1_000)
            .unwrap();
        let command_id = match first {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger2
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 62_000,
                charge_active: false,
                discharge_active: false,
                now_ms: 70_000,
            })
            .unwrap();
        assert_eq!(ledger2.get(&command_id).unwrap().unwrap().state, "expired");
    }

    #[test]
    fn startup_reconciliation_marks_in_progress_unknown() {
        let ledger = isolated_ledger();
        let first = ledger
            .reserve_start("fp", "force_charge", 30, "key-1", 1_000)
            .unwrap();
        let command_id = match first {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        assert_eq!(ledger.reconcile_startup().unwrap(), 1);
        assert_eq!(ledger.get(&command_id).unwrap().unwrap().state, "unknown");
        // A finished command is untouched.
        assert_eq!(ledger.reconcile_startup().unwrap(), 0);
    }

    #[test]
    fn active_start_visibility_for_recovery_stops() {
        let ledger = isolated_ledger();
        assert!(!ledger.has_active_start("force_charge").unwrap());
        let first = ledger
            .reserve_start("fp", "force_charge", 30, "key-1", 1_000)
            .unwrap();
        let command_id = match first {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        assert!(ledger.has_active_start("force_charge").unwrap());
        ledger
            .finish(&command_id, "failed", r#"{"ok":false}"#)
            .unwrap();
        assert!(!ledger.has_active_start("force_charge").unwrap());
    }
}
