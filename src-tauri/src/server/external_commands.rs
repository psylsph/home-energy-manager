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

/// How long finished/failed/expired commands are retained (ms): 24 h. That is
/// deliberately just above the 1439-minute (23 h 59 m) maximum action, so a
/// terminal row — and the durable ownership it carries — expires right after
/// the longest action could still be running, while the table stays bounded.
pub(crate) const RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

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
pub struct ReadbackEvidence<'a> {
    /// Millisecond timestamp of the snapshot the facts came from.
    pub snapshot_ts_ms: i64,
    /// Strict "force-charge predicate" (enable_charge && Eco mode && window
    /// && not paused), as computed for the status endpoint. `None` means the
    /// predicate could not be evaluated (no inverter clock, corrupt slots) —
    /// distinct from `Some(false)`, which is a real observation that the
    /// action is not running.
    pub charge_active: Option<bool>,
    /// Strict force-discharge predicate; see [`Self::charge_active`].
    pub discharge_active: Option<bool>,
    /// Raw HR318 mode from the same fresh readback, when available.
    pub pause_mode: Option<u16>,
    /// Raw HR319/320 pause window from the same fresh readback.
    pub pause_slot_start: Option<u16>,
    pub pause_slot_end: Option<u16>,
    /// Timestamp of the complete HR318-320 read. It must equal
    /// `snapshot_ts_ms`; carried-forward values are never confirmation.
    pub pause_registers_observed_at_ms: Option<i64>,
    /// Inverter identity attached to the same snapshot.
    pub device_type: crate::inverter::model::DeviceType,
    pub inverter_serial: &'a str,
    pub firmware_version: &'a str,
    /// Complete decoded snapshot used for exact Force restoration matching.
    pub snapshot: &'a crate::inverter::model::InverterSnapshot,
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
                        recovery_pending INTEGER NOT NULL DEFAULT 0,
                        active INTEGER NOT NULL DEFAULT 0,
                        response TEXT,
                        created_ms INTEGER NOT NULL,
                        updated_ms INTEGER NOT NULL
                    );
                    CREATE UNIQUE INDEX IF NOT EXISTS external_commands_scope
                        ON external_commands(scope);
                    CREATE INDEX IF NOT EXISTS external_commands_updated
                        ON external_commands(updated_ms);",
                )
                .map_err(|e| format!("failed to init commands db: {e}"))?;
            let columns: Vec<String> = connection
                .prepare("PRAGMA table_info(external_commands)")
                .and_then(|mut stmt| {
                    stmt.query_map([], |row| row.get::<_, String>(1))
                        .map(|rows| rows.flatten().collect())
                })
                .map_err(|e| format!("failed to inspect commands db schema: {e}"))?;
            // The migration runs as ONE transaction and is versioned, so an
            // interrupted upgrade cannot leave the database half-migrated. With
            // the previous column-presence gating, a crash after ADD COLUMN
            // active but before the backfill skipped the backfill forever, and
            // every legacy in-flight row silently lost its durable recovery.
            let schema_version: i64 = connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .map_err(|e| format!("failed to read commands db version: {e}"))?;
            if !columns.iter().any(|name| name == "recovery") {
                return Err(
                    "external_commands table is missing its recovery column; cannot migrate"
                        .to_string(),
                );
            }
            connection
                .execute_batch("BEGIN IMMEDIATE")
                .map_err(|e| format!("failed to start commands migration: {e}"))?;
            let migrate = || -> Result<(), String> {
                if !columns.iter().any(|name| name == "recovery_pending") {
                    connection
                        .execute(
                            "ALTER TABLE external_commands
                             ADD COLUMN recovery_pending INTEGER NOT NULL DEFAULT 0",
                            [],
                        )
                        .map_err(|e| format!("failed to migrate commands db: {e}"))?;
                }
                // Column presence is checked in addition to the version
                // marker: a database that reports version 1 without the column
                // (hand-edited, or a future partial migration) must still get
                // the ALTER rather than failing the collapse below forever.
                let added_active = !columns.iter().any(|name| name == "active");
                if added_active {
                    connection
                        .execute(
                            "ALTER TABLE external_commands
                             ADD COLUMN active INTEGER NOT NULL DEFAULT 0",
                            [],
                        )
                        .map_err(|e| format!("failed to migrate commands db: {e}"))?;
                }
                if schema_version < 1 || added_active {
                    connection
                        .execute(
                            "UPDATE external_commands
                             SET active = 1
                             WHERE is_start = 1 AND recovery IS NOT NULL
                               AND state IN ('accepted','queued','dispatched','readback_confirmed')",
                            [],
                        )
                        .map_err(|e| format!("failed to migrate active commands: {e}"))?;
                }
                // Collapse duplicate active owners before the unique index
                // demands one. A pre-diff database can legitimately hold several
                // `readback_confirmed` starts for the same action (the old index
                // excluded that state) and the backfill above marks them all
                // active, so index creation would fail — taking every ledger call
                // with it. Rows are deactivated, never deleted: the scope row must
                // still replay its stored response for a retried Idempotency-Key.
                //
                // Ordering keeps exactly one owner per action:
                //   1. an in-flight command (accepted/queued/dispatched) always
                //      wins — it is the command actually running, and its own
                //      recovery payload is what its caller is waiting on;
                //   2. among finished rows the OLDEST created_ms wins, because a
                //      later start captured its revert only after the earlier force
                //      writes, so the earliest capture is the one that provably
                //      restores the user's own pre-force schedule.
                // In a mixed legacy shape (a live row plus older confirmed rows)
                // this deliberately prefers the live row's baseline even though it
                // may already be contaminated: liveness is the stronger
                // requirement, and every loser keeps its stored response for
                // idempotent replay. The CTE is MATERIALIZED so the winner set is
                // fixed before any row is updated.
                connection
                    .execute_batch(
                        "WITH duplicate_active AS MATERIALIZED (
                         SELECT id FROM (
                             SELECT id,
                                    ROW_NUMBER() OVER (
                                        PARTITION BY action
                                        ORDER BY (state = 'readback_confirmed') ASC,
                                                 created_ms ASC,
                                                 id ASC
                                    ) AS rank
                             FROM external_commands
                             WHERE is_start = 1 AND active = 1
                               AND state IN ('accepted','queued','dispatched','readback_confirmed')
                         )
                         WHERE rank > 1
                     )
                     UPDATE external_commands SET active = 0
                      WHERE id IN (SELECT id FROM duplicate_active);",
                    )
                    .map_err(|e| format!("failed to migrate duplicate active commands: {e}"))?;
                connection
                    .execute_batch(
                        "DROP INDEX IF EXISTS external_commands_active_action;
                         CREATE UNIQUE INDEX external_commands_active_action
                             ON external_commands(action)
                             WHERE is_start = 1 AND active = 1
                               AND state IN ('accepted','queued','dispatched','readback_confirmed');",
                    )
                    .map_err(|e| format!("failed to init active command index: {e}"))?;
                Ok(())
            };
            if let Err(error) = migrate() {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error);
            }
            connection
                .execute_batch("PRAGMA user_version = 1; COMMIT")
                .map_err(|e| format!("failed to commit commands migration: {e}"))?;
            *cached = Some(connection);
        }
        f(cached.as_ref().expect("connection initialized"))
    }

    /// Reserve a start command. `action` identifies a force or native pause
    /// operation; `idem_key` scopes replay; the active-action index
    /// guarantees a single live start per action kind.
    pub fn reserve_start(
        &self,
        fingerprint: &str,
        action: &str,
        minutes: u64,
        idem_key: &str,
        now_ms: i64,
    ) -> Result<Reservation, String> {
        self.reserve_start_request(
            fingerprint,
            &format!("/api/control/{action}"),
            action,
            &format!("minutes={minutes}"),
            minutes,
            idem_key,
            now_ms,
        )
    }

    /// Reserve a native-pause start. All modes share the public endpoint
    /// scope, while the request hash includes the canonical mode so reusing an
    /// idempotency key with a different mode is a conflict.
    pub fn reserve_pause_start(
        &self,
        fingerprint: &str,
        action: &str,
        mode: &str,
        minutes: u64,
        idem_key: &str,
        now_ms: i64,
    ) -> Result<Reservation, String> {
        self.reserve_start_request(
            fingerprint,
            "/api/control/pause-mode",
            action,
            &format!("mode={mode};minutes={minutes}"),
            minutes,
            idem_key,
            now_ms,
        )
    }

    /// Allow clippy::too_many_arguments — the parameters mirror the ledger
    /// row's columns one-for-one; a params struct would be pure indirection.
    #[allow(clippy::too_many_arguments)]
    fn reserve_start_request(
        &self,
        fingerprint: &str,
        endpoint: &str,
        action: &str,
        request_hash: &str,
        minutes: u64,
        idem_key: &str,
        now_ms: i64,
    ) -> Result<Reservation, String> {
        // A restart may encounter a terminal owner whose retention window
        // elapsed while the process was down. Trim before the partial unique
        // active-action index can turn that stale row into a permanent
        // InProgress response.
        self.trim_at(now_ms)?;
        let scope = format!("{fingerprint}:{endpoint}:{idem_key}");
        let command_id = new_command_id();
        let expires_at_ms = now_ms + (minutes as i64) * 60_000;
        let inserted = self.with_connection(|connection| {
            let mut stmt = connection
                .prepare(
                    "INSERT OR IGNORE INTO external_commands
                     (id, scope, fingerprint, endpoint, action, is_start, request_hash, state,
                      expires_at_ms, active, created_ms, updated_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, 'accepted', ?7, 1, ?8, ?8)",
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
        self.resolve_duplicate(&scope, request_hash, action)
    }

    /// Check whether this exact idempotency scope already has a command,
    /// without reserving a new command. This preflight lets API handlers
    /// replay an accepted response before a changing inverter-state guard.
    pub fn lookup_start(
        &self,
        fingerprint: &str,
        action: &str,
        minutes: u64,
        idem_key: &str,
    ) -> Result<Option<Reservation>, String> {
        self.lookup_scope(
            &format!("{fingerprint}:/api/control/{action}:{idem_key}"),
            &format!("minutes={minutes}"),
        )
    }

    pub fn lookup_pause_start(
        &self,
        fingerprint: &str,
        mode: &str,
        minutes: u64,
        idem_key: &str,
    ) -> Result<Option<Reservation>, String> {
        self.lookup_scope(
            &format!("{fingerprint}:/api/control/pause-mode:{idem_key}"),
            &format!("mode={mode};minutes={minutes}"),
        )
    }

    pub fn lookup_stop(
        &self,
        fingerprint: &str,
        action: &str,
        idem_key: &str,
    ) -> Result<Option<Reservation>, String> {
        self.lookup_scope(
            &format!("{fingerprint}:/api/control/{action}/stop:{idem_key}"),
            "stop",
        )
    }

    /// Check whether an idempotency scope exists without comparing its body.
    /// Permission middleware uses this only to let an already-known request
    /// reach the handler, where the full request hash is still validated before
    /// replay or conflict handling.
    pub fn has_idempotency_scope(
        &self,
        fingerprint: &str,
        endpoint: &str,
        idem_key: &str,
    ) -> Result<bool, String> {
        self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT 1 FROM external_commands WHERE scope = ?1 LIMIT 1",
                    params![format!("{fingerprint}:{endpoint}:{idem_key}")],
                    |_| Ok(()),
                )
                .optional()
                .map(|value| value.is_some())
                .map_err(|e| format!("idempotency scope lookup failed: {e}"))
        })
    }

    fn lookup_scope(&self, scope: &str, request_hash: &str) -> Result<Option<Reservation>, String> {
        self.with_connection(|connection| {
            let existing = connection
                .query_row(
                    "SELECT id, request_hash, response FROM external_commands
                     WHERE scope = ?1",
                    params![scope],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| format!("idempotency lookup failed: {e}"))?;
            let Some((command_id, existing_hash, response)) = existing else {
                return Ok(None);
            };
            if existing_hash != request_hash {
                return Ok(Some(Reservation::Conflict {
                    existing_command_id: command_id,
                }));
            }
            if let Some(response) = response {
                let parsed = serde_json::from_str(&response)
                    .map_err(|e| format!("stored response unparsable: {e}"))?;
                return Ok(Some(Reservation::Replayed { response: parsed }));
            }
            Ok(Some(Reservation::InProgress { command_id }))
        })
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
                      active, created_ms, updated_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, 'accepted', 0, ?7, ?7)",
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
                       AND active = 1
                       AND state IN ('accepted','queued','dispatched','readback_confirmed')",
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
        let mode_only_pause = serde_json::from_str::<Value>(recovery_json)
            .ok()
            .and_then(|recovery| {
                let device_type = serde_json::from_value::<crate::inverter::model::DeviceType>(
                    recovery.get("device_type")?.clone(),
                )
                .ok()?;
                let firmware = recovery
                    .get("firmware_version")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<u16>().ok())
                    .unwrap_or(0);
                Some(matches!(
                    device_type.pause_register_support(firmware),
                    crate::inverter::model::PauseRegisterSupport::ModeOnly
                ))
            })
            .unwrap_or(false);
        self.with_connection(|connection| {
            connection
                .execute(
                    "UPDATE external_commands
                     SET recovery = ?2,
                         recovery_pending = CASE
                             WHEN ?3 AND is_start = 1
                               AND action IN ('pause_charge','pause_discharge','pause_both')
                             THEN 1 ELSE recovery_pending END
                     WHERE id = ?1",
                    params![command_id, recovery_json, mode_only_pause],
                )
                .map_err(|e| format!("recovery write failed: {e}"))?;
            Ok(())
        })
    }

    /// Test-only visibility for the `recovery_pending` flag: it decides whether
    /// a failed command keeps its ownership and survives the retention trim.
    #[cfg(test)]
    pub(crate) fn recovery_pending_for_test(&self, command_id: &str) -> Result<bool, String> {
        self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT recovery_pending FROM external_commands WHERE id = ?1",
                    params![command_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(|e| format!("recovery pending read failed: {e}"))
                .map(|value| value.unwrap_or(0) != 0)
        })
    }

    /// Mark a persisted baseline as needing restoration after a failed or
    /// interrupted mutation. The row remains recoverable even after its
    /// command reaches a terminal failure state.
    pub fn mark_recovery_pending(&self, command_id: &str) -> Result<(), String> {
        self.with_connection(|connection| {
            connection
                .execute(
                    "UPDATE external_commands SET recovery_pending = 1 WHERE id = ?1",
                    params![command_id],
                )
                .map_err(|e| format!("recovery pending update failed: {e}"))?;
            Ok(())
        })
    }

    /// Keep the active start row of `action` owned while a restoration is still
    /// being retried. Without this the retention-based ownership expiry would
    /// release (and later delete) a row whose rollback is genuinely owed, so a
    /// restart could no longer hydrate the baseline even though the inverter is
    /// still in the forced state.
    pub fn hold_restoration_ownership(&self, action: &str) -> Result<(), String> {
        self.with_connection(|connection| {
            connection
                .execute(
                    "UPDATE external_commands SET recovery_pending = 1
                     WHERE action = ?1 AND is_start = 1 AND active = 1",
                    params![action],
                )
                .map_err(|e| format!("restoration hold failed: {e}"))?;
            Ok(())
        })
    }

    /// Remove a baseline after a fresh exact restoration readback confirms it.
    pub fn clear_recovery(&self, command_id: &str) -> Result<(), String> {
        self.with_connection(|connection| {
            connection
                .execute(
                    "UPDATE external_commands
                     SET recovery = NULL, recovery_pending = 0, active = 0
                     WHERE id = ?1",
                    params![command_id],
                )
                .map_err(|e| format!("recovery clear failed: {e}"))?;
            Ok(())
        })
    }

    /// Attach the final response envelope to a command and freeze its state.
    ///
    /// A failed command also releases its active slot, unless a restoration is
    /// still pending (`recovery_pending = 1`, set when a rejected write left a
    /// rollback to retry). The release is what lets the row leave the bounded
    /// ledger: `trim` refuses any row that still owns its action, so an
    /// unowned failure would otherwise be retained forever. (An in-flight row
    /// is never counted as active purely by this column — the live states and
    /// the unique index decide that.) `readback_confirmed` rows keep their slot
    /// because their restoration may still be outstanding.
    pub fn finish(&self, command_id: &str, state: &str, response_json: &str) -> Result<(), String> {
        self.with_connection(|connection| {
            connection
                .execute(
                    "UPDATE external_commands
                     SET state = ?2,
                         response = ?3,
                         updated_ms = ?4,
                         active = CASE
                             WHEN ?2 = 'failed' AND recovery_pending = 0 THEN 0
                             ELSE active
                         END
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
    pub fn advance_readback(&self, evidence: &ReadbackEvidence<'_>) -> Result<usize, String> {
        let changed = self.with_connection(|connection| {
            let mut stmt = connection
                .prepare(
                    "SELECT id, action, is_start, state, expires_at_ms, created_ms, recovery
                     FROM external_commands
                     WHERE state IN ('accepted','queued','dispatched','readback_confirmed')",
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
                        row.get::<_, Option<String>>(6)?,
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
            for (id, action, is_start, state, expires_at_ms, created_ms, recovery) in commands {
                let causally_fresh = evidence.snapshot_ts_ms > created_ms;
                let pause_target = recovery
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<Value>(json).ok());
                let pause_registers_match = |mode: u16, start: u16, end: u16| {
                    let Some(target) = pause_target.as_ref() else {
                        return false;
                    };
                    let expected_device = target
                        .get("device_type")
                        .cloned()
                        .and_then(|value| {
                            serde_json::from_value::<crate::inverter::model::DeviceType>(value).ok()
                        });
                    let expected_serial = target.get("inverter_serial").and_then(Value::as_str);
                    let firmware = target
                        .get("firmware_version")
                        .and_then(Value::as_str)
                        .and_then(|value| value.parse::<u16>().ok())
                        .unwrap_or(0);
                    let slots_match = match expected_device {
                        Some(device) => match device.pause_register_support(firmware) {
                            crate::inverter::model::PauseRegisterSupport::ModeAndWindow => {
                                evidence.pause_slot_start == Some(start)
                                    && evidence.pause_slot_end == Some(end)
                            }
                            crate::inverter::model::PauseRegisterSupport::ModeOnly => true,
                            crate::inverter::model::PauseRegisterSupport::Unsupported => false,
                        },
                        None => false,
                    };
                    // Firmware updates do not identify a different inverter;
                    // the raw pause registers plus serial/device identity are
                    // the stable restoration evidence. Legacy AC3 has no safe
                    // HR319/320 path, so HR318 is the complete evidence set.
                    evidence.pause_registers_observed_at_ms == Some(evidence.snapshot_ts_ms)
                        && expected_device == Some(evidence.device_type)
                        && expected_serial == Some(evidence.inverter_serial)
                        && evidence.pause_mode == Some(mode)
                        && slots_match
                };
                let active: Option<bool> = match action.as_str() {
                    "force_charge" => evidence.charge_active,
                    "force_discharge" => evidence.discharge_active,
                    "pause_charge" | "pause_discharge" | "pause_both" => Some(
                        pause_target
                            .as_ref()
                            .and_then(|target| {
                                Some((
                                    target.get("requested_mode")?.as_u64()? as u16,
                                    target.get("requested_slot_start")?.as_u64()? as u16,
                                    target.get("requested_slot_end")?.as_u64()? as u16,
                                ))
                            })
                            .is_some_and(|(mode, start, end)| {
                                pause_registers_match(mode, start, end)
                            }),
                    ),
                    "pause_mode" => Some(
                        pause_target
                            .as_ref()
                            .and_then(|target| {
                                Some((
                                    target.get("battery_pause_mode")?.as_u64()? as u16,
                                    target.get("battery_pause_slot_start")?.as_u64()? as u16,
                                    target.get("battery_pause_slot_end")?.as_u64()? as u16,
                                ))
                            })
                            .is_some_and(|(mode, start, end)| {
                                pause_registers_match(mode, start, end)
                            }),
                    ),
                    _ => Some(false),
                };
                // `Some(false)` is an observation; `None` is not. Only an
                // observation may complete or release an action, so a cycle
                // with no inverter clock (a zeroed HR35-40 block — the
                // supported EmptyData/Intermittent dongle behaviour) cannot
                // expire a start or confirm a fallback stop.
                let stop_satisfied = match action.as_str() {
                    "pause_mode" => active == Some(true),
                    "force_charge" => recovery
                        .as_deref()
                        .and_then(|json| {
                            serde_json::from_str::<crate::inverter::poll::ForceChargeRevert>(json)
                                .ok()
                        })
                        .map_or(active == Some(false), |revert| {
                            revert_identity_matches(
                                evidence,
                                &revert.device_type,
                                &revert.inverter_serial,
                            ) && {
                                let writes = crate::server::api::build_force_charge_stop_writes(
                                    evidence.device_type,
                                    &revert,
                                );
                                crate::server::api::snapshot_matches_writes(evidence.snapshot, &writes)
                            }
                        }),
                    "force_discharge" => recovery
                        .as_deref()
                        .and_then(|json| {
                            serde_json::from_str::<crate::inverter::poll::ForceDischargeRevert>(
                                json,
                            )
                            .ok()
                        })
                        .map_or(
                            // After restart there is no captured baseline. The
                            // fallback stop is only confirmed once a fresh
                            // snapshot proves the inverter is back in Eco and
                            // the discharge enable is actually clear; the
                            // window predicate alone becomes false naturally
                            // when an expired slot is no longer active.
                            !evidence.snapshot.enable_discharge
                                && evidence.snapshot.battery_power_mode == 1,
                            |revert| {
                                revert_identity_matches(
                                    evidence,
                                    &revert.device_type,
                                    &revert.inverter_serial,
                                ) && {
                                    let writes = crate::server::api::build_force_discharge_stop_writes(
                                        evidence.device_type,
                                        evidence.firmware_version,
                                        &revert,
                                    );
                                    !writes.is_empty()
                                        && crate::server::api::snapshot_matches_writes(
                                            evidence.snapshot,
                                            &writes,
                                        )
                                }
                            },
                        ),
                    _ => active == Some(false),
                };
                // A Force start may only be confirmed or expired by the
                // inverter that supplied its persisted recovery baseline. The
                // baseline can be absent briefly while an external start is
                // being recorded; that is intentionally not confirmation.
                let force_start_identity_matches = match action.as_str() {
                    "force_charge" | "force_discharge" => recovery
                        .as_deref()
                        .and_then(|json| serde_json::from_str::<Value>(json).ok())
                        .and_then(|revert| {
                            let device_type = revert
                                .get("device_type")
                                .cloned()
                                .and_then(|value| serde_json::from_value(value).ok())?;
                            let serial = revert.get("inverter_serial")?.as_str()?;
                            Some(revert_identity_matches(evidence, &device_type, serial))
                        })
                        .unwrap_or(false),
                    _ => true,
                };
                let new_state = if is_start == 1 {
                    if causally_fresh
                        && active == Some(true)
                        && force_start_identity_matches
                    {
                        Some("readback_confirmed")
                    } else if let Some(expires_at_ms) = expires_at_ms {
                        if evidence.now_ms > expires_at_ms {
                            if causally_fresh
                                && active == Some(false)
                                && force_start_identity_matches
                            {
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
                } else if causally_fresh && stop_satisfied {
                    // Stops confirm when a fresh readback proves the captured
                    // baseline was restored. A safe Eco/disarmed predicate is
                    // used only when no baseline is available to compare.
                    Some("readback_confirmed")
                } else if evidence.now_ms > created_ms.saturating_add(RETENTION_MS) {
                    // A stop whose restoration never round-trips must not stay
                    // in flight forever: past the retention window it becomes
                    // honestly `unknown` ("go look"), which also lets the
                    // bounded ledger trim it. Each retry with a fresh key would
                    // otherwise add another permanent row.
                    Some("unknown")
                } else {
                    None
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
                        if is_start == 0 && new_state == "readback_confirmed" {
                            if action == "pause_mode" {
                                connection
                                    .execute(
                                        "UPDATE external_commands SET active = 0
                                         WHERE is_start = 1 AND active = 1
                                           AND recovery_pending = 0
                                           AND action IN ('pause_charge','pause_discharge','pause_both')",
                                        [],
                                    )
                                    .map_err(|e| {
                                        format!("pause start deactivation failed: {e}")
                                    })?;
                            } else {
                                connection
                                    .execute(
                                        "UPDATE external_commands SET active = 0
                                         WHERE is_start = 1 AND active = 1
                                           AND recovery_pending = 0 AND action = ?1",
                                        params![action],
                                    )
                                    .map_err(|e| format!("start deactivation failed: {e}"))?;
                            }
                        }
                        if is_start == 1 && new_state == "expired" {
                            // The action's window has ended with fresh evidence
                            // that it is no longer running: release the active
                            // slot so new starts are admitted and the row can
                            // be trimmed once no restoration is pending.
                            connection
                                .execute(
                                    "UPDATE external_commands
                                     SET active = 0
                                     WHERE id = ?1 AND recovery_pending = 0",
                                    params![id],
                                )
                                .map_err(|e| format!("expired deactivation failed: {e}"))?;
                        }
                        changed += 1;
                    }
                }
            }
            Ok(changed)
        })?;
        self.trim()?;
        Ok(changed)
    }

    /// Mark only the command ids whose batches were actually extracted by
    /// the poll loop. Deferred owner batches remain honestly `queued`.
    pub fn mark_dispatched(&self, command_ids: &[String]) -> Result<(), String> {
        if command_ids.is_empty() {
            return Ok(());
        }
        self.with_connection(|connection| {
            let now = Utc::now().timestamp_millis();
            for command_id in command_ids {
                connection
                    .execute(
                        "UPDATE external_commands SET state = 'dispatched', updated_ms = ?2
                         WHERE id = ?1 AND state = 'queued'",
                        params![command_id, now],
                    )
                    .map_err(|e| format!("dispatch mark failed: {e}"))?;
            }
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

    /// Return the durable baseline owned by an active start of `action`.
    /// Recovery stops use this after restart when no in-memory revert exists,
    /// so a fallback stop is still tied to the original inverter identity.
    pub fn active_recovery_for_action(&self, action: &str) -> Result<Option<String>, String> {
        self.trim()?;
        self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT recovery FROM external_commands
                     WHERE action = ?1 AND is_start = 1 AND active = 1
                       AND recovery IS NOT NULL
                     ORDER BY created_ms ASC
                     LIMIT 1",
                    params![action],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|e| format!("active recovery lookup failed: {e}"))
        })
    }

    /// Return active command recovery payloads before startup reconciliation.
    /// These payloads enable an explicit recovery Stop without ever silently
    /// re-arming the inverter. A failed command with a pending compensation is
    /// included as well.
    ///
    /// Terminal states are bounded by the retention window: a baseline whose
    /// owning session ended days ago must not be re-hydrated at boot, because
    /// a later Stop would then apply it over whatever the user has armed since
    /// (and report success). In-flight commands and rows with a restoration
    /// still pending are always surfaced.
    pub fn active_recoveries(&self) -> Result<Vec<(String, String)>, String> {
        self.active_recoveries_at(Utc::now().timestamp_millis())
    }

    fn active_recoveries_at(&self, now_ms: i64) -> Result<Vec<(String, String)>, String> {
        self.trim_at(now_ms)?;
        let cutoff = now_ms - RETENTION_MS;
        self.with_connection(|connection| {
            let mut stmt = connection
                .prepare(
                    "SELECT action, recovery FROM external_commands
                     WHERE is_start = 1
                       AND active = 1
                       AND recovery IS NOT NULL
                       AND (state IN ('accepted','queued','dispatched')
                            OR recovery_pending = 1
                            OR (state IN ('readback_confirmed','unknown')
                                AND updated_ms >= ?1))",
                )
                .map_err(|e| format!("recovery lookup failed: {e}"))?;
            let rows = stmt
                .query_map(params![cutoff], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|e| format!("recovery query failed: {e}"))?;
            Ok(rows.filter_map(Result::ok).collect())
        })
    }

    /// Clear all durable command state for the local E2E harness. The endpoint
    /// exposing this is only armed by `--e2e-admin`, so production instances do
    /// not call this destructive operation.
    pub fn reset_for_e2e(&self) -> Result<(), String> {
        self.with_connection(|connection| {
            connection
                .execute("DELETE FROM external_commands", [])
                .map_err(|e| format!("E2E command reset failed: {e}"))?;
            Ok(())
        })
    }

    /// Mark a command whose writes were still queued for the old TCP session
    /// as failed. Reconnect deliberately drops such batches rather than
    /// replaying them against an unidentified inverter, so the ledger must
    /// release the idempotency slot and recovery ownership at the same time.
    pub fn fail_queued_command(&self, command_id: &str, reason: &str) -> Result<(), String> {
        let response = json!({
            "status": 502,
            "body": {"ok": false, "error": reason}
        })
        .to_string();
        self.with_connection(|connection| {
            connection
                .execute(
                    "UPDATE external_commands
                     SET state = 'failed',
                         active = CASE WHEN is_start = 1 THEN 0 ELSE active END,
                         recovery = CASE WHEN is_start = 1 THEN NULL ELSE recovery END,
                         recovery_pending = CASE WHEN is_start = 1 THEN 0 ELSE recovery_pending END,
                         response = ?2, updated_ms = ?3
                     WHERE id = ?1
                       AND state IN ('accepted','queued','dispatched','unknown')",
                    params![command_id, response, Utc::now().timestamp_millis()],
                )
                .map_err(|e| format!("queued command failure update failed: {e}"))?;
            Ok(())
        })
    }

    /// Release durable ownership after exact restoration readback. Recovery
    /// payloads are cleared only for the active start of this action.
    pub fn complete_restoration(&self, action: &str) -> Result<(), String> {
        self.with_connection(|connection| {
            connection
                .execute(
                    "UPDATE external_commands
                     SET active = 0, recovery = NULL, recovery_pending = 0,
                         updated_ms = ?2
                     WHERE action = ?1 AND is_start = 1 AND active = 1",
                    params![action, Utc::now().timestamp_millis()],
                )
                .map_err(|e| format!("restoration completion failed: {e}"))?;
            Ok(())
        })
    }

    /// Whether another battery-control start is active, excluding the command
    /// currently being prepared after its idempotency reservation.
    pub fn has_active_battery_control_except(&self, command_id: &str) -> Result<bool, String> {
        self.with_connection(|connection| {
            let exists = connection
                .query_row(
                    "SELECT 1 FROM external_commands
                     WHERE id != ?1 AND is_start = 1
                       AND active = 1
                       AND state IN ('accepted','queued','dispatched','readback_confirmed')
                       AND action IN ('force_charge','force_discharge',
                                      'pause_charge','pause_discharge','pause_both')
                     LIMIT 1",
                    params![command_id],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|e| format!("active control lookup failed: {e}"))?;
            Ok(exists.is_some())
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
                       AND active = 1
                       AND state IN ('accepted','queued','dispatched','readback_confirmed')",
                    params![action],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count > 0)
                .map_err(|e| format!("active-start lookup failed: {e}"))
        })
    }

    /// Whether an active native pause start owns the pause-control domain.
    pub fn has_active_pause_control(&self) -> Result<bool, String> {
        self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM external_commands
                     WHERE is_start = 1 AND active = 1
                       AND state IN ('accepted','queued','dispatched','readback_confirmed')
                       AND action IN ('pause_charge','pause_discharge','pause_both')",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count > 0)
                .map_err(|e| format!("active-pause lookup failed: {e}"))
        })
    }

    /// Whether an active command owns the shared battery-control domain.
    pub fn has_active_battery_control(&self) -> Result<bool, String> {
        self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM external_commands
                     WHERE is_start = 1
                       AND active = 1
                       AND state IN ('accepted','queued','dispatched','readback_confirmed')
                       AND action IN ('force_charge','force_discharge',
                                      'pause_charge','pause_discharge','pause_both')",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count > 0)
                .map_err(|e| format!("active battery-control lookup failed: {e}"))
        })
    }

    /// Drop finished commands past the retention window.
    ///
    /// Durable ownership of a terminal row expires first: a stranded
    /// (`unknown`) or confirmed-but-unrestored row that nobody has touched for
    /// the whole retention window is no longer a plausible owner, so its
    /// active slot is released and the row becomes trimmable. Rows with a
    /// restoration still pending (`recovery_pending = 1`) and in-flight rows
    /// keep their ownership indefinitely.
    fn trim(&self) -> Result<(), String> {
        self.trim_at(Utc::now().timestamp_millis())
    }

    fn trim_at(&self, now_ms: i64) -> Result<(), String> {
        self.with_connection(|connection| {
            let cutoff = now_ms - RETENTION_MS;
            connection
                .execute(
                    "UPDATE external_commands SET active = 0
                     WHERE is_start = 1 AND active = 1 AND recovery_pending = 0
                       AND state IN ('readback_confirmed','unknown','expired','failed')
                       AND updated_ms < ?1",
                    params![cutoff],
                )
                .map_err(|e| format!("ledger ownership expiry failed: {e}"))?;
            connection
                .execute(
                    "DELETE FROM external_commands
                     WHERE state IN ('readback_confirmed','failed','expired','unknown')
                       AND active = 0 AND recovery_pending = 0
                       AND updated_ms < ?1",
                    params![cutoff],
                )
                .map_err(|e| format!("ledger trim failed: {e}"))?;
            Ok(())
        })
    }
}

/// Whether a Force readback may be confirmed by this evidence. Reverts
/// captured before the identity fields were introduced carry no inverter
/// identity: a foreign inverter must never confirm them. The identity fields
/// are copied from the same complete snapshot that produced the evidence, so
/// this check remains independent of any test-only snapshot projection.
fn revert_identity_matches(
    evidence: &ReadbackEvidence<'_>,
    device_type: &crate::inverter::model::DeviceType,
    inverter_serial: &str,
) -> bool {
    crate::inverter::poll::force_baseline_identity_matches(
        evidence.device_type,
        evidence.inverter_serial,
        *device_type,
        inverter_serial,
    )
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
    getrandom::fill(&mut bytes).expect("OS CSPRNG must be available");
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let hash = hasher.finalize();
    hash.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXED_NOW_MS: i64 = 1_700_000_000_000;

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

    /// A failed start must release its active slot. Otherwise one refused
    /// command holds its action kind forever (every later start resolves to
    /// `InProgress`) and the row is excluded from the retention trim, growing
    /// the table without bound. A failure that still owns a pending
    /// restoration keeps its slot until that restoration is cleared.
    #[test]
    fn failed_start_releases_its_active_slot_unless_a_restoration_is_pending() {
        let ledger = isolated_ledger();
        let start = |key: &str| match ledger
            .reserve_start("fp", "force_charge", 30, key, 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };

        // Plain failure: the slot is released and a new start is admitted.
        let failed = start("failed-key");
        ledger.finish(&failed, "failed", r#"{"ok":false}"#).unwrap();
        assert!(
            !ledger.has_active_start("force_charge").unwrap(),
            "a failed start must not keep holding the action kind"
        );
        let next = match ledger
            .reserve_start("fp", "force_charge", 30, "next-key", 2_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("a new start must be admitted after the failure, got {other:?}"),
        };
        // Release it as well so the next scenario starts from a clean slate.
        ledger.finish(&next, "failed", r#"{"ok":false}"#).unwrap();

        // Failure with a pending restoration: the recovery must stay available
        // (so a restart can still finish the rollback) and the row must stay
        // out of the trim. It no longer owns the action kind — the in-memory
        // revert is what guards a competing start — so later starts are still
        // admitted.
        let pending = start("pending-key");
        ledger
            .record_recovery(&pending, r#"{"restoring":true}"#)
            .unwrap();
        ledger.mark_recovery_pending(&pending).unwrap();
        ledger
            .finish(&pending, "failed", r#"{"ok":false}"#)
            .unwrap();
        assert!(
            !ledger.has_active_start("force_charge").unwrap(),
            "a failed command must not block later starts"
        );
        assert_eq!(
            ledger.active_recoveries().unwrap().len(),
            1,
            "the pending restoration must stay recoverable after a restart"
        );

        // Age every failure past retention. The released ones are trimmed; the
        // pending restoration is deliberately withheld from the trim.
        ledger
            .with_connection(|connection| {
                connection
                    .execute(
                        "UPDATE external_commands SET updated_ms = 0 WHERE id IN (?1, ?2, ?3)",
                        params![failed, next, pending],
                    )
                    .map_err(|e| format!("age failed rows: {e}"))?;
                Ok(())
            })
            .unwrap();
        ledger.mark_state(&failed, "failed").unwrap();
        assert!(
            ledger.get(&failed).unwrap().is_none() && ledger.get(&next).unwrap().is_none(),
            "released failures must be trimmable once retention passes"
        );
        assert!(
            ledger.get(&pending).unwrap().is_some(),
            "a pending restoration must survive the trim"
        );

        // Confirming the restoration releases the row for the trim too.
        ledger.clear_recovery(&pending).unwrap();
        ledger.mark_state(&failed, "failed").unwrap();
        assert!(ledger.get(&pending).unwrap().is_none());
        assert!(ledger.active_recoveries().unwrap().is_empty());
    }

    /// Durable ownership of a stranded action expires with the retention
    /// window. Otherwise a baseline captured in a session that ended days ago
    /// is re-hydrated at every boot (the row is immortal because trim excludes
    /// active rows) and a later Stop applies it, overwriting whatever schedule
    /// the user has armed since — and reporting success.
    #[test]
    fn stranded_ownership_expires_with_the_retention_window() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_start("fp", "force_charge", 30, "stranded-key", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&command_id, "queued").unwrap();
        ledger
            .record_recovery(&command_id, r#"{"baseline":"stranded"}"#)
            .unwrap();

        // A restart strands the command: it becomes `unknown`, and its
        // baseline is hydrated so an explicit Stop can still recover it.
        ledger.reconcile_startup().unwrap();
        assert_eq!(ledger.get(&command_id).unwrap().unwrap().state, "unknown");
        assert_eq!(
            ledger.active_recoveries().unwrap().len(),
            1,
            "a freshly stranded action must stay recoverable"
        );

        // Days pass (retention is measured on the row's last update).
        ledger
            .with_connection(|connection| {
                connection
                    .execute(
                        "UPDATE external_commands SET updated_ms = 0 WHERE id = ?1",
                        params![command_id],
                    )
                    .map_err(|e| format!("age stranded row: {e}"))?;
                Ok(())
            })
            .unwrap();

        assert!(
            ledger.active_recoveries().unwrap().is_empty(),
            "a baseline past the retention window must not be hydrated"
        );
        assert!(
            matches!(
                ledger
                    .reserve_start("fp-new", "force_charge", 30, "after-retention", 2_000)
                    .unwrap(),
                Reservation::Accepted { .. }
            ),
            "an aged confirmed owner must not block a new start after restart"
        );
        ledger.mark_state(&command_id, "queued").unwrap();
        assert!(
            ledger.get(&command_id).unwrap().is_none(),
            "an expired strand must leave the bounded ledger"
        );
    }

    /// An upgrade interrupted after `ALTER TABLE ... ADD COLUMN active` but
    /// before the ownership backfill must still backfill on the next open. The
    /// migration is versioned rather than gated on column presence, so a crash
    /// cannot silently drop the durable recovery of commands that were live at
    /// the upgrade.
    #[test]
    fn interrupted_migration_still_backfills_legacy_in_flight_owners() {
        let dir = crate::test_util::make_unique_test_dir("commands-interrupted");
        let path = dir.join("external_commands.db");
        let now = FIXED_NOW_MS;
        {
            // Legacy schema plus the new columns, as a crashed upgrade leaves
            // it, with `user_version` still 0.
            let partial = Connection::open(&path).unwrap();
            partial
                .execute_batch(
                    "CREATE TABLE external_commands (
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
                        recovery_pending INTEGER NOT NULL DEFAULT 0,
                        active INTEGER NOT NULL DEFAULT 0,
                        response TEXT,
                        created_ms INTEGER NOT NULL,
                        updated_ms INTEGER NOT NULL
                     );
                     CREATE UNIQUE INDEX external_commands_scope
                         ON external_commands(scope);",
                )
                .unwrap();
            partial
                .execute(
                    "INSERT INTO external_commands
                     (id, scope, fingerprint, endpoint, action, is_start, request_hash, state,
                      recovery, active, created_ms, updated_ms)
                     VALUES ('cmd-live', 'fp:/api/control/force_charge:live', 'fp',
                             '/api/control/force_charge', 'force_charge', 1, 'minutes=30',
                             'dispatched', '{\"baseline\":\"live\"}', 0, ?1, ?1)",
                    params![now - 5_000],
                )
                .unwrap();
        }

        let ledger = CommandLedger::new();
        ledger.override_path(path);
        assert_eq!(
            ledger.active_recoveries().unwrap(),
            vec![(
                "force_charge".to_string(),
                r#"{"baseline":"live"}"#.to_string()
            )],
            "an interrupted migration must still surface the live command's baseline"
        );
    }

    #[test]
    fn schema_v1_without_active_column_backfills_legacy_owners() {
        let dir = crate::test_util::make_unique_test_dir("commands-v1-no-active");
        let path = dir.join("external_commands.db");
        let now = 1_700_000_000_000_i64;
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE external_commands (
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
                    recovery_pending INTEGER NOT NULL DEFAULT 0,
                    response TEXT,
                    created_ms INTEGER NOT NULL,
                    updated_ms INTEGER NOT NULL
                 );
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO external_commands
                 (id, scope, fingerprint, endpoint, action, is_start, request_hash,
                  state, recovery, created_ms, updated_ms)
                 VALUES ('cmd-v1', 'fp:/api/control/force_charge:v1', 'fp',
                         '/api/control/force_charge', 'force_charge', 1, 'minutes=30',
                         'queued', '{\"baseline\":\"v1\"}', ?1, ?1)",
                params![now],
            )
            .unwrap();
        drop(connection);

        let ledger = CommandLedger::new();
        ledger.override_path(path);
        assert_eq!(
            ledger.active_recoveries().unwrap(),
            vec![(
                "force_charge".to_string(),
                r#"{"baseline":"v1"}"#.to_string()
            )]
        );
    }

    /// While a restoration is still in flight the durable row must stay owned:
    /// otherwise the retention expiry releases it, a restart cannot hydrate the
    /// baseline, and the inverter is left in the forced state with no recovery.
    #[test]
    fn restoration_in_flight_keeps_durable_ownership_past_retention() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_start("fp", "force_charge", 30, "hold-key", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&command_id, "queued").unwrap();
        ledger
            .record_recovery(&command_id, r#"{"baseline":"held"}"#)
            .unwrap();
        // Stranded by a restart, then aged past the ownership window.
        ledger.reconcile_startup().unwrap();
        ledger
            .with_connection(|connection| {
                connection
                    .execute(
                        "UPDATE external_commands SET updated_ms = 0 WHERE id = ?1",
                        params![command_id],
                    )
                    .map_err(|e| format!("age held row: {e}"))?;
                Ok(())
            })
            .unwrap();

        // An automatic restoration attempt holds the row, so the trim may not
        // release it...
        ledger.hold_restoration_ownership("force_charge").unwrap();
        ledger.mark_state(&command_id, "queued").unwrap();
        assert!(
            ledger.get(&command_id).unwrap().is_some(),
            "a pending restoration must keep the durable row"
        );
        assert_eq!(
            ledger.active_recoveries().unwrap().len(),
            1,
            "and it must still be hydratable after a restart"
        );

        // ...until the restoration is confirmed.
        ledger.clear_recovery(&command_id).unwrap();
        ledger.mark_state(&command_id, "queued").unwrap();
        assert!(ledger.get(&command_id).unwrap().is_none());
    }

    /// A stop whose restoration never round-trips must not stay in flight
    /// forever: past the retention window it becomes honestly `unknown` and is
    /// trimmed, so repeated retries with fresh keys cannot grow the ledger
    /// without bound.
    #[test]
    fn unconfirmed_stop_becomes_unknown_past_retention() {
        let ledger = isolated_ledger();
        let stop_id = match ledger
            .reserve_stop("fp", "force_charge", "stop-key", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&stop_id, "dispatched").unwrap();

        // Well past retention, and the action is still running so the stop
        // cannot confirm.
        let late = 1_000 + RETENTION_MS + 60_000;
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: late,
                charge_active: Some(true),
                discharge_active: Some(false),
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "SN1",
                firmware_version: "400",
                snapshot: &Default::default(),
                now_ms: late,
            })
            .unwrap();
        assert_eq!(ledger.get(&stop_id).unwrap().unwrap().state, "unknown");

        ledger
            .with_connection(|connection| {
                connection
                    .execute(
                        "UPDATE external_commands SET updated_ms = 0 WHERE id = ?1",
                        params![stop_id],
                    )
                    .map_err(|e| format!("age stop row: {e}"))?;
                Ok(())
            })
            .unwrap();
        ledger.mark_state(&stop_id, "queued").unwrap();
        assert!(
            ledger.get(&stop_id).unwrap().is_none(),
            "an unconfirmed stop must leave the bounded ledger once aged"
        );
    }

    /// A pre-existing ledger database can hold several start rows for the
    /// same action: the legacy partial unique index only covered
    /// accepted/queued/dispatched, so two finished `readback_confirmed`
    /// starts (each with its own recovery baseline, which the old code never
    /// cleared) legitimately coexisted. The new index also covers
    /// `readback_confirmed`, so migration must collapse those duplicates
    /// deterministically — otherwise index creation fails, every ledger call
    /// errors, and authenticated control is dead until the file is deleted.
    #[test]
    fn legacy_database_with_duplicate_confirmed_starts_migrates() {
        let dir = crate::test_util::make_unique_test_dir("commands-legacy");
        let path = dir.join("external_commands.db");
        {
            // The exact pre-diff schema and the duplicate rows it permitted.
            let legacy = Connection::open(&path).unwrap();
            legacy
                .execute_batch(
                    "CREATE TABLE external_commands (
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
                     CREATE UNIQUE INDEX external_commands_scope
                         ON external_commands(scope);
                     CREATE UNIQUE INDEX external_commands_active_action
                         ON external_commands(action)
                         WHERE is_start = 1
                           AND state IN ('accepted','queued','dispatched');",
                )
                .unwrap();
            for (id, created_ms) in [
                ("cmd-older", FIXED_NOW_MS - 5_000),
                ("cmd-newer", FIXED_NOW_MS - 4_000),
            ] {
                legacy
                    .execute(
                        "INSERT INTO external_commands
                         (id, scope, fingerprint, endpoint, action, is_start, request_hash, state,
                          recovery, created_ms, updated_ms)
                         VALUES (?1, ?2, 'fp', '/api/control/force_charge', 'force_charge', 1,
                                 'minutes=30', 'readback_confirmed', ?3, ?4, ?4)",
                        params![
                            id,
                            format!("fp:/api/control/force_charge:{id}"),
                            format!("{{\"baseline\":\"{id}\"}}"),
                            created_ms
                        ],
                    )
                    .unwrap();
            }
        }

        let ledger = CommandLedger::new();
        ledger.override_path(path);

        // Opening the migrated ledger must succeed and keep exactly one owner.
        assert!(
            ledger.has_active_start("force_charge").unwrap(),
            "the migrated ledger must still report an active start"
        );
        let recoveries = ledger.active_recoveries_at(FIXED_NOW_MS).unwrap();
        assert_eq!(
            recoveries.len(),
            1,
            "duplicate confirmed starts must collapse to one active owner"
        );
        // The oldest baseline predates every later force write, so it is the
        // only captured state that actually restores the user's schedule.
        assert_eq!(recoveries[0].1, r#"{"baseline":"cmd-older"}"#);

        // The surviving owner still serialises new starts and can be released.
        assert!(matches!(
            ledger
                .reserve_start("fp", "force_charge", 30, "fresh-key", 3_000)
                .unwrap(),
            Reservation::InProgress { .. }
        ));
        ledger.complete_restoration("force_charge").unwrap();
        assert!(!ledger.has_active_start("force_charge").unwrap());
        // A released ledger admits a brand-new start (index intact).
        assert!(matches!(
            ledger
                .reserve_start("fp", "force_charge", 30, "next-key", 4_000)
                .unwrap(),
            Reservation::Accepted { .. }
        ));
    }

    /// The dedupe's other branch: a legacy database can hold one in-flight
    /// start plus older finished ones (the old index only covered
    /// accepted/queued/dispatched). The in-flight command keeps ownership
    /// because it is the one actually running, while the finished row loses
    /// its active slot yet stays replayable.
    #[test]
    fn legacy_mixed_in_flight_and_finished_starts_keep_the_live_owner() {
        let dir = crate::test_util::make_unique_test_dir("commands-legacy-mixed");
        let path = dir.join("external_commands.db");
        let now = FIXED_NOW_MS;
        {
            let legacy = Connection::open(&path).unwrap();
            legacy
                .execute_batch(
                    "CREATE TABLE external_commands (
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
                     CREATE UNIQUE INDEX external_commands_scope
                         ON external_commands(scope);
                     CREATE UNIQUE INDEX external_commands_active_action
                         ON external_commands(action)
                         WHERE is_start = 1
                           AND state IN ('accepted','queued','dispatched');",
                )
                .unwrap();
            // Finished first, then the in-flight one (recovery payloads differ
            // so the survivor is unambiguous).
            legacy
                .execute(
                    "INSERT INTO external_commands
                     (id, scope, fingerprint, endpoint, action, is_start, request_hash, state,
                      recovery, response, created_ms, updated_ms)
                     VALUES ('cmd-finished', 'fp:/api/control/force_charge:finished', 'fp', '/api/control/force_charge',
                             'force_charge', 1, 'minutes=30', 'readback_confirmed',
                             '{\"baseline\":\"oldest\"}', '{\"status\":200,\"body\":{\"ok\":true}}',
                             ?1, ?1)",
                    params![now - 6_000],
                )
                .unwrap();
            legacy
                .execute(
                    "INSERT INTO external_commands
                     (id, scope, fingerprint, endpoint, action, is_start, request_hash, state,
                      recovery, response, created_ms, updated_ms)
                     VALUES ('cmd-live', 'fp:/api/control/force_charge:live', 'fp', '/api/control/force_charge',
                             'force_charge', 1, 'minutes=30', 'dispatched',
                             '{\"baseline\":\"live\"}', NULL, ?1, ?1)",
                    params![now - 5_000],
                )
                .unwrap();
        }

        let ledger = CommandLedger::new();
        ledger.override_path(path);

        // The in-flight command keeps the action; the finished row is
        // deactivated but must still replay its stored response.
        assert_eq!(
            ledger.active_recoveries_at(FIXED_NOW_MS).unwrap(),
            vec![(
                "force_charge".to_string(),
                r#"{"baseline":"live"}"#.to_string()
            )]
        );
        assert!(ledger.has_active_start("force_charge").unwrap());
        match ledger
            .lookup_start("fp", "force_charge", 30, "finished")
            .unwrap()
        {
            Some(Reservation::Replayed { response }) => {
                assert_eq!(response["status"], 200);
                assert_eq!(response["body"]["ok"], true);
            }
            other => panic!("the deactivated loser must still replay, got {other:?}"),
        }
    }

    #[test]
    fn active_recovery_includes_readback_confirmed_start() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_start("fp", "force_charge", 30, "key-confirmed", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger
            .record_recovery(&command_id, r#"{"baseline":true}"#)
            .unwrap();
        ledger
            .finish(&command_id, "readback_confirmed", r#"{"ok":true}"#)
            .unwrap();
        assert_eq!(
            ledger.active_recovery_for_action("force_charge").unwrap(),
            Some(r#"{"baseline":true}"#.to_string())
        );

        let recoveries = ledger.active_recoveries().unwrap();
        assert_eq!(
            recoveries,
            vec![("force_charge".into(), r#"{"baseline":true}"#.into())]
        );
    }

    #[test]
    fn e2e_reset_clears_durable_command_ownership() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_start("fp", "force_charge", 30, "reset-key", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger
            .record_recovery(&command_id, r#"{"baseline":true}"#)
            .unwrap();

        ledger.reset_for_e2e().unwrap();

        assert!(ledger.get(&command_id).unwrap().is_none());
        assert!(!ledger.has_active_start("force_charge").unwrap());
        assert!(ledger.active_recoveries().unwrap().is_empty());
    }

    #[test]
    fn failed_command_with_pending_recovery_is_restored() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_start("fp", "pause_both", 30, "key-pending", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger
            .record_recovery(&command_id, r#"{"restoring":true}"#)
            .unwrap();
        ledger.mark_recovery_pending(&command_id).unwrap();
        ledger
            .finish(&command_id, "failed", r#"{"ok":false}"#)
            .unwrap();

        assert_eq!(
            ledger.active_recoveries().unwrap(),
            vec![("pause_both".into(), r#"{"restoring":true}"#.into())]
        );
        ledger.clear_recovery(&command_id).unwrap();
        assert!(ledger.active_recoveries().unwrap().is_empty());
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
        ledger
            .record_recovery(
                &command_id,
                r#"{"device_type":"ACCoupled","inverter_serial":"TEST"}"#,
            )
            .unwrap();

        // Stale evidence (from before the command existed) confirms nothing.
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 5_000,
                charge_active: Some(true),
                discharge_active: Some(false),
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "TEST",
                firmware_version: "400",
                snapshot: &Default::default(),
                now_ms: 11_000,
            })
            .unwrap();
        assert_eq!(ledger.get(&command_id).unwrap().unwrap().state, "queued");

        // Fresh matching evidence confirms.
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 12_000,
                charge_active: Some(true),
                discharge_active: Some(false),
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "TEST",
                firmware_version: "400",
                snapshot: &Default::default(),
                now_ms: 13_000,
            })
            .unwrap();
        assert_eq!(
            ledger.get(&command_id).unwrap().unwrap().state,
            "readback_confirmed"
        );
    }

    /// Build pause-register evidence with the given identity and freshness.
    /// Allow clippy::too_many_arguments — a test-evidence builder where the
    /// named parameters read exactly like the ReadbackEvidence fields they
    /// populate.
    #[allow(clippy::too_many_arguments)]
    fn pause_evidence(
        snapshot_ts_ms: i64,
        observed_at_ms: Option<i64>,
        serial: &str,
        firmware: &str,
        device: crate::inverter::model::DeviceType,
        mode: u16,
        start: u16,
        end: u16,
    ) -> ReadbackEvidence<'static> {
        ReadbackEvidence {
            snapshot_ts_ms,
            charge_active: Some(false),
            discharge_active: Some(false),
            pause_mode: Some(mode),
            pause_slot_start: Some(start),
            pause_slot_end: Some(end),
            pause_registers_observed_at_ms: observed_at_ms,
            device_type: device,
            inverter_serial: Box::leak(serial.to_string().into_boxed_str()),
            firmware_version: Box::leak(firmware.to_string().into_boxed_str()),
            snapshot: Box::leak(Box::default()),
            now_ms: snapshot_ts_ms,
        }
    }

    const PAUSE_TARGET: &str = r#"{"requested_mode":3,"requested_slot_start":1234,"requested_slot_end":1304,
        "device_type":"AllInOne3_6kW","inverter_serial":"AIO-TEST","firmware_version":"400"}"#;

    /// Foreign inverter identity must never confirm a pause start, even with
    /// otherwise exact fresh register values.
    #[test]
    fn pause_readback_rejects_foreign_inverter_identity() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_start("fp", "pause_both", 30, "foreign-key", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&command_id, "queued").unwrap();
        ledger.record_recovery(&command_id, PAUSE_TARGET).unwrap();

        let (serial, firmware) = ("OTHER-SN", "400");
        ledger
            .advance_readback(&pause_evidence(
                3_000,
                Some(3_000),
                serial,
                firmware,
                crate::inverter::model::DeviceType::AllInOne3_6kW,
                3,
                1234,
                1304,
            ))
            .unwrap();
        assert_eq!(
            ledger.get(&command_id).unwrap().unwrap().state,
            "queued",
            "identity {serial}/{firmware} must not confirm"
        );
        ledger
            .advance_readback(&pause_evidence(
                3_000,
                Some(3_000),
                "AIO-TEST",
                "400",
                crate::inverter::model::DeviceType::Gen2Hybrid,
                3,
                1234,
                1304,
            ))
            .unwrap();
        assert_eq!(
            ledger.get(&command_id).unwrap().unwrap().state,
            "queued",
            "a different device type must not confirm"
        );

        ledger
            .advance_readback(&pause_evidence(
                4_000,
                Some(4_000),
                "AIO-TEST",
                "400",
                crate::inverter::model::DeviceType::AllInOne3_6kW,
                3,
                1234,
                1304,
            ))
            .unwrap();
        assert_eq!(
            ledger.get(&command_id).unwrap().unwrap().state,
            "readback_confirmed"
        );
    }

    /// Firmware updates do not change inverter identity, so exact fresh pause
    /// readback remains valid when HR21 changes.
    #[test]
    fn pause_readback_allows_firmware_change() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_start("fp", "pause_both", 30, "firmware-key", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&command_id, "queued").unwrap();
        ledger.record_recovery(&command_id, PAUSE_TARGET).unwrap();
        ledger
            .advance_readback(&pause_evidence(
                3_000,
                Some(3_000),
                "AIO-TEST",
                "301",
                crate::inverter::model::DeviceType::AllInOne3_6kW,
                3,
                1234,
                1304,
            ))
            .unwrap();
        assert_eq!(
            ledger.get(&command_id).unwrap().unwrap().state,
            "readback_confirmed"
        );
    }

    /// Carried-forward HR318-320 raw values keep their original observation
    /// timestamp, so evidence whose observed-at differs from the snapshot
    /// timestamp can never confirm a start or clear a restoration.
    #[test]
    fn pause_readback_rejects_carried_forward_register_values() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_start("fp", "pause_both", 30, "carry-key", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&command_id, "queued").unwrap();
        ledger.record_recovery(&command_id, PAUSE_TARGET).unwrap();

        // Exact values, same identity, but observed one snapshot earlier.
        ledger
            .advance_readback(&pause_evidence(
                3_000,
                Some(2_000),
                "AIO-TEST",
                "400",
                crate::inverter::model::DeviceType::AllInOne3_6kW,
                3,
                1234,
                1304,
            ))
            .unwrap();
        assert_eq!(ledger.get(&command_id).unwrap().unwrap().state, "queued");

        // A pause stop clearing the baseline is bound by the same rule.
        let stop_id = match ledger
            .reserve_stop("fp", "pause_mode", "carry-stop-key", 4_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger
            .record_recovery(
                &stop_id,
                r#"{"battery_pause_mode":0,"battery_pause_slot_start":0,"battery_pause_slot_end":0,
                    "device_type":"AllInOne3_6kW","inverter_serial":"AIO-TEST","firmware_version":"400"}"#,
            )
            .unwrap();
        ledger
            .advance_readback(&pause_evidence(
                5_000,
                Some(4_000),
                "AIO-TEST",
                "400",
                crate::inverter::model::DeviceType::AllInOne3_6kW,
                0,
                0,
                0,
            ))
            .unwrap();
        assert_eq!(ledger.get(&stop_id).unwrap().unwrap().state, "accepted");
    }

    /// Force stop rows confirm only when a causally fresh snapshot decodes
    /// exactly the restoration writes; a foreign inverter's snapshot must
    /// never confirm, while legacy rows without captured identity keep the
    /// historical exact-write-only behaviour.
    #[test]
    fn force_stop_readback_requires_exact_writes_and_matching_identity() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_stop("fp", "force_charge", "force-stop-key", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&command_id, "queued").unwrap();
        ledger
            .record_recovery(
                &command_id,
                r#"{"started_at_ms":500,"enable_charge":true,"enable_charge_target":true,
                    "enable_discharge":true,"target_soc":60,"battery_power_mode":1,
                    "charge_slot_1_start":[2,0],"charge_slot_1_end":[4,0],
                    "three_phase_force_charge_enable":null,"three_phase_ac_charge_enable":null,
                    "battery_pause_mode":null,
                    "device_type":"Gen2Hybrid","inverter_serial":"SN1","firmware_version":"400"}"#,
            )
            .unwrap();

        // The decoded snapshot the restored registers would produce.
        let restored = crate::inverter::model::InverterSnapshot {
            device_type: crate::inverter::model::DeviceType::Gen2Hybrid,
            enable_charge: true,
            enable_charge_target: true,
            enable_discharge: true,
            target_soc: 60,
            battery_power_mode: 1,
            charge_slots: [
                crate::inverter::model::ScheduleSlot {
                    enabled: true,
                    start_hour: 2,
                    start_minute: 0,
                    end_hour: 4,
                    end_minute: 0,
                    target_soc: 60,
                },
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
            ],
            ..Default::default()
        };

        // Exact registers but foreign serial: no confirmation.
        let mut foreign = restored.clone();
        foreign.inverter_serial = "OTHER".into();
        foreign.timestamp = 2;
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 2_000,
                charge_active: Some(false),
                discharge_active: Some(false),
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: foreign.device_type,
                inverter_serial: &foreign.inverter_serial,
                firmware_version: "400",
                snapshot: &foreign,
                now_ms: 2_500,
            })
            .unwrap();
        assert_eq!(ledger.get(&command_id).unwrap().unwrap().state, "queued");

        // Same registers, same inverter, causally fresh: confirmed.
        let mut own = restored;
        own.inverter_serial = "SN1".into();
        own.timestamp = 3;
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 3_000,
                charge_active: Some(false),
                discharge_active: Some(false),
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: own.device_type,
                inverter_serial: &own.inverter_serial,
                firmware_version: "400",
                snapshot: &own,
                now_ms: 3_500,
            })
            .unwrap();
        assert_eq!(
            ledger.get(&command_id).unwrap().unwrap().state,
            "readback_confirmed"
        );
    }

    /// Draining one owner's batch must mark only that batch's command as
    /// dispatched; other queued commands stay honestly `queued`.
    #[test]
    fn mark_dispatched_only_advances_the_drained_batch() {
        let ledger = isolated_ledger();
        let charge_id = match ledger
            .reserve_start("fp-a", "force_charge", 30, "batch-key-a", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        let pause_id = match ledger
            .reserve_start("fp-b", "pause_charge", 30, "batch-key-b", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&charge_id, "queued").unwrap();
        ledger.mark_state(&pause_id, "queued").unwrap();

        ledger
            .mark_dispatched(std::slice::from_ref(&charge_id))
            .unwrap();
        assert_eq!(ledger.get(&charge_id).unwrap().unwrap().state, "dispatched");
        assert_eq!(ledger.get(&pause_id).unwrap().unwrap().state, "queued");
    }

    /// Reporter timeline from issue #301 (AC-coupled AC3, external API):
    /// a Force Charge start is confirmed, then a Stop is sent — both *after* the
    /// window expired and *while it is still running* — and HEM must confirm it
    /// once a fresh readback decodes the restored baseline. The released build
    /// only checked the loose `enable_charge && eco` predicate, so a user whose
    /// normal charge schedule is armed never saw a stop confirm.
    #[test]
    fn force_charge_stop_confirms_mid_window_and_after_expiry() {
        // Pre-force state captured on the AC3: armed charge schedule, eco mode,
        // target armed flag off (so the decoder normalises it to 100).
        let revert = json!({
            "started_at_ms": 1_784_000_000_000i64,
            "force_charge_slot_end_ms": 1_784_000_600_000i64,
            "enable_charge": true,
            "enable_charge_target": false,
            "device_type": "ACCoupled",
            "inverter_serial": "SA12345678",
            "firmware_version": "449",
            "enable_discharge": false,
            "target_soc": 100,
            "battery_power_mode": 1,
            "charge_rate": 50,
            "charge_slot_1_start": [2, 0],
            "charge_slot_1_end": [5, 0],
            "three_phase_force_charge_enable": null,
            "three_phase_ac_charge_enable": null,
            "battery_pause_mode": 0
        })
        .to_string();

        // The decoded snapshot after the restoration writes land: the user's own
        // schedule back in slot 1, eco, charge armed, target normalised to 100.
        let restored = || {
            let mut snapshot = crate::inverter::model::InverterSnapshot {
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "SA12345678".into(),
                firmware_version: "449".into(),
                enable_charge: true,
                enable_charge_target: false,
                enable_discharge: false,
                target_soc: 100,
                battery_power_mode: 1,
                ..Default::default()
            };
            snapshot.charge_slots[0] = crate::inverter::model::ScheduleSlot {
                enabled: true,
                start_hour: 2,
                start_minute: 0,
                end_hour: 5,
                end_minute: 0,
                target_soc: 100,
            };
            snapshot
        };

        for (label, stop_at_ms, window_still_live) in [
            (
                "stop sent while the window is still running",
                1_784_000_200_000i64,
                true,
            ),
            (
                "stop sent after the window expired",
                1_784_000_900_000i64,
                false,
            ),
        ] {
            let ledger = isolated_ledger();
            let start = match ledger
                .reserve_start("fp", "force_charge", 10, "report-start", 1_784_000_000_000)
                .unwrap()
            {
                Reservation::Accepted { command_id } => command_id,
                other => panic!("expected accepted, got {other:?}"),
            };
            ledger.mark_state(&start, "queued").unwrap();
            ledger.record_recovery(&start, &revert).unwrap();

            // The start confirms while its window is live.
            let mut live = restored();
            live.timestamp = 1_784_000_010;
            ledger
                .advance_readback(&ReadbackEvidence {
                    snapshot_ts_ms: 1_784_000_010_000,
                    charge_active: Some(true),
                    discharge_active: Some(false),
                    pause_mode: None,
                    pause_slot_start: None,
                    pause_slot_end: None,
                    pause_registers_observed_at_ms: None,
                    device_type: crate::inverter::model::DeviceType::ACCoupled,
                    inverter_serial: "SA12345678",
                    firmware_version: "449",
                    snapshot: &live,
                    now_ms: 1_784_000_010_000,
                })
                .unwrap();
            assert_eq!(
                ledger.get(&start).unwrap().unwrap().state,
                "readback_confirmed",
                "{label}: the start must confirm"
            );

            // The stop is accepted and dispatched.
            let stop = match ledger
                .reserve_stop("fp", "force_charge", "report-stop", stop_at_ms)
                .unwrap()
            {
                Reservation::Accepted { command_id } => command_id,
                other => panic!("expected accepted, got {other:?}"),
            };
            ledger.mark_state(&stop, "queued").unwrap();
            ledger.record_recovery(&stop, &revert).unwrap();
            ledger.mark_state(&stop, "dispatched").unwrap();

            // A later readback decodes the restored baseline. Note the force
            // predicate may still be "live" in the first case, so confirmation
            // must come from the exact write match, not from `!active`.
            let mut after = restored();
            after.timestamp = (stop_at_ms / 1000) + 5;
            ledger
                .advance_readback(&ReadbackEvidence {
                    snapshot_ts_ms: after.timestamp * 1000,
                    charge_active: Some(window_still_live),
                    discharge_active: Some(false),
                    pause_mode: None,
                    pause_slot_start: None,
                    pause_slot_end: None,
                    pause_registers_observed_at_ms: None,
                    device_type: crate::inverter::model::DeviceType::ACCoupled,
                    inverter_serial: "SA12345678",
                    firmware_version: "449",
                    snapshot: &after,
                    now_ms: after.timestamp * 1000,
                })
                .unwrap();
            assert_eq!(
                ledger.get(&stop).unwrap().unwrap().state,
                "readback_confirmed",
                "{label}: the stop must confirm from the restored baseline"
            );
        }
    }

    /// A recovery baseline from before v0.83.3 has no inverter identity. A
    /// readable current snapshot must not confirm the old start until an
    /// explicit Stop migrates the baseline and establishes ownership.
    #[test]
    fn legacy_force_start_readback_requires_explicit_migration() {
        let ledger = isolated_ledger();
        let start = match ledger
            .reserve_start("fp", "force_charge", 10, "legacy-start", 1_784_000_000_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&start, "queued").unwrap();
        ledger
            .record_recovery(
                &start,
                r#"{
                    "started_at_ms":1784000000000,
                    "force_charge_slot_end_ms":1784000600000,
                    "enable_charge":false,
                    "enable_discharge":false,
                    "target_soc":80,
                    "battery_power_mode":1,
                    "charge_rate":30,
                    "charge_slot_1_start":[2,0],
                    "charge_slot_1_end":[4,0],
                    "three_phase_force_charge_enable":null,
                    "three_phase_ac_charge_enable":null,
                    "battery_pause_mode":0
                }"#,
            )
            .unwrap();
        let snapshot = crate::inverter::model::InverterSnapshot {
            device_type: crate::inverter::model::DeviceType::ACCoupled,
            inverter_serial: "SA12345678".into(),
            enable_charge: true,
            battery_power_mode: 1,
            ..Default::default()
        };
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 1_784_000_010_000,
                charge_active: Some(true),
                discharge_active: Some(false),
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "SA12345678",
                firmware_version: "449",
                snapshot: &snapshot,
                now_ms: 1_784_000_010_000,
            })
            .unwrap();
        assert_eq!(
            ledger.get(&start).unwrap().unwrap().state,
            "queued",
            "legacy identity-less recovery must not confirm on readable readback"
        );
    }

    /// A cycle whose force predicate could not be evaluated (no inverter clock,
    /// corrupt slots) must not complete or release a start. `expired` is
    /// documented as "elapsed with confirming readback", and a zeroed HR35-40
    /// block is the supported EmptyData/Intermittent dongle behaviour — not
    /// evidence that the action stopped. Treating it as `false` would release
    /// the active slot while the force action may still be physically running,
    /// letting a new start capture the forced state as its "pre" baseline.
    #[test]
    fn clockless_evidence_cannot_expire_or_release_a_force_start() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_start("fp", "force_charge", 4, "clockless-key", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&command_id, "queued").unwrap();
        ledger
            .record_recovery(
                &command_id,
                r#"{"baseline":"clockless","device_type":"ACCoupled","inverter_serial":"SN1"}"#,
            )
            .unwrap();

        // Deadline long past, but the clock could not be read: no transition,
        // and the recovery stays available.
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 400_000,
                charge_active: None,
                discharge_active: None,
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "SN1",
                firmware_version: "400",
                snapshot: &Default::default(),
                now_ms: 400_000,
            })
            .unwrap();
        assert_eq!(ledger.get(&command_id).unwrap().unwrap().state, "queued");
        assert_eq!(ledger.active_recoveries().unwrap().len(), 1);
        assert!(
            matches!(
                ledger
                    .reserve_start("fp", "force_charge", 10, "clockless-2", 400_001)
                    .unwrap(),
                Reservation::InProgress { .. }
            ),
            "a clockless cycle must not free the action for a new start"
        );

        // Far past the deadline the row becomes honestly `unknown`, which keeps
        // ownership (and the recovery payload) rather than releasing it. An
        // `unknown` row is deliberately never advanced again — recovery runs
        // through an explicit Stop — so this is its terminal state.
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 900_000,
                charge_active: None,
                discharge_active: None,
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "SN1",
                firmware_version: "400",
                snapshot: &Default::default(),
                now_ms: 900_000,
            })
            .unwrap();
        assert_eq!(ledger.get(&command_id).unwrap().unwrap().state, "unknown");
        assert_eq!(ledger.active_recoveries().unwrap().len(), 1);

        // A clockless cycle in a fresh ledger also does not expire, while a
        // real "not running" observation does release the action.
        let fresh = isolated_ledger();
        let second = match fresh
            .reserve_start("fp", "force_charge", 4, "clockless-fresh", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        fresh.mark_state(&second, "queued").unwrap();
        fresh
            .record_recovery(
                &second,
                r#"{"baseline":"fresh","device_type":"ACCoupled","inverter_serial":"SN1"}"#,
            )
            .unwrap();
        fresh
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 1_000_000,
                charge_active: Some(false),
                discharge_active: Some(false),
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "SN1",
                firmware_version: "400",
                snapshot: &Default::default(),
                now_ms: 1_000_000,
            })
            .unwrap();
        assert_eq!(fresh.get(&second).unwrap().unwrap().state, "expired");
        assert!(
            fresh.active_recoveries().unwrap().is_empty(),
            "a real observation releases ownership"
        );
        assert!(
            matches!(
                fresh
                    .reserve_start("fp", "force_charge", 10, "fresh-next", 1_000_001)
                    .unwrap(),
                Reservation::Accepted { .. }
            ),
            "a real observation does release the action"
        );
    }

    /// Issue #301 field report: a force-charge start whose window ended (the
    /// inverter returned to its armed schedule in eco) must reach `expired`
    /// and release the active slot, and a stop issued AFTER the window ended
    /// must confirm from that same evidence instead of staying `dispatched`
    /// forever. The strict window-aware predicate supplies the evidence; this
    /// test pins the ledger transitions that were stuck before it.
    #[test]
    fn expired_force_window_releases_start_and_confirms_late_stop() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_start("fp", "force_charge", 4, "window-key", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&command_id, "queued").unwrap();
        ledger
            .record_recovery(
                &command_id,
                r#"{"device_type":"ACCoupled","inverter_serial":"SN1"}"#,
            )
            .unwrap();

        // While the window is live the start confirms from fresh evidence.
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 2_000,
                charge_active: Some(true),
                discharge_active: Some(false),
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "SN1",
                firmware_version: "400",
                snapshot: &Default::default(),
                now_ms: 2_500,
            })
            .unwrap();
        assert_eq!(
            ledger.get(&command_id).unwrap().unwrap().state,
            "readback_confirmed"
        );
        assert!(ledger.has_active_start("force_charge").unwrap());

        // The window ends (expires_at = 1_000 + 240_000). A minute later the
        // user sends a stop; it dispatches and then sees fresh evidence of a
        // NOT-active force charge (armed schedule in eco, window over).
        let stop_id = match ledger
            .reserve_stop("fp", "force_charge", "window-stop-key", 250_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&stop_id, "dispatched").unwrap();
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 251_000,
                charge_active: Some(false),
                discharge_active: Some(false),
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "SN1",
                firmware_version: "400",
                snapshot: &Default::default(),
                now_ms: 251_500,
            })
            .unwrap();
        // The late stop reaches its terminal state (this was the stuck
        // `dispatched` in the field report) and releases the start.
        assert_eq!(
            ledger.get(&stop_id).unwrap().unwrap().state,
            "readback_confirmed"
        );
        assert!(!ledger.has_active_start("force_charge").unwrap());

        // The start itself, past its deadline with fresh not-active evidence,
        // expires and frees the active slot for a brand-new start.
        let ledger2 = isolated_ledger();
        let second = match ledger2
            .reserve_start("fp", "force_charge", 4, "window-key-2", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger2.mark_state(&second, "queued").unwrap();
        ledger2
            .record_recovery(
                &second,
                r#"{"device_type":"ACCoupled","inverter_serial":"SN1"}"#,
            )
            .unwrap();
        ledger2
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 251_000,
                charge_active: Some(false),
                discharge_active: Some(false),
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "SN1",
                firmware_version: "400",
                snapshot: &Default::default(),
                now_ms: 251_500,
            })
            .unwrap();
        assert_eq!(ledger2.get(&second).unwrap().unwrap().state, "expired");
        assert!(!ledger2.has_active_start("force_charge").unwrap());
        // A new start is admitted after the release.
        assert!(matches!(
            ledger2
                .reserve_start("fp", "force_charge", 10, "window-key-3", 300_000)
                .unwrap(),
            Reservation::Accepted { .. }
        ));
    }

    /// A start that expires while compensation is pending must keep durable
    /// ownership, so a restart can still hydrate its recovery baseline.
    #[test]
    fn expired_start_with_pending_recovery_keeps_active_ownership() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_start("fp", "force_charge", 1, "pending-expiry", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&command_id, "queued").unwrap();
        ledger
            .record_recovery(
                &command_id,
                r#"{"device_type":"ACCoupled","inverter_serial":"SN1"}"#,
            )
            .unwrap();
        ledger.mark_recovery_pending(&command_id).unwrap();

        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 70_000,
                charge_active: Some(false),
                discharge_active: Some(false),
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "SN1",
                firmware_version: "400",
                snapshot: &Default::default(),
                now_ms: 70_000,
            })
            .unwrap();

        assert_eq!(ledger.get(&command_id).unwrap().unwrap().state, "expired");
        assert!(!ledger.has_active_start("force_charge").unwrap());
        assert_eq!(ledger.active_recoveries().unwrap().len(), 1);
    }

    /// A confirmed Force start still has to leave the active ledger once its
    /// finite window ends and fresh evidence says it is no longer active.
    #[test]
    fn confirmed_force_start_expires_after_its_window() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_start("fp", "force_charge", 1, "confirmed-expiry", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&command_id, "queued").unwrap();
        ledger
            .record_recovery(
                &command_id,
                r#"{"device_type":"ACCoupled","inverter_serial":"SN1"}"#,
            )
            .unwrap();
        let evidence = |timestamp_ms: i64, active: Option<bool>, now_ms: i64| {
            ledger
                .advance_readback(&ReadbackEvidence {
                    snapshot_ts_ms: timestamp_ms,
                    charge_active: active,
                    discharge_active: Some(false),
                    pause_mode: None,
                    pause_slot_start: None,
                    pause_slot_end: None,
                    pause_registers_observed_at_ms: None,
                    device_type: crate::inverter::model::DeviceType::ACCoupled,
                    inverter_serial: "SN1",
                    firmware_version: "400",
                    snapshot: &Default::default(),
                    now_ms,
                })
                .unwrap();
        };
        evidence(2_000, Some(true), 2_000);
        assert_eq!(
            ledger.get(&command_id).unwrap().unwrap().state,
            "readback_confirmed"
        );
        evidence(70_000, Some(false), 70_000);
        assert_eq!(ledger.get(&command_id).unwrap().unwrap().state, "expired");
        assert!(!ledger.has_active_start("force_charge").unwrap());
    }

    /// A restart fallback Stop Discharge must not confirm merely because the
    /// finite slot expired. The actual enable flag and Eco mode must be seen
    /// in a fresh snapshot.
    #[test]
    fn restart_force_discharge_stop_requires_safe_flag_readback() {
        let ledger = isolated_ledger();
        let start_id = match ledger
            .reserve_start("fp", "force_discharge", 30, "restart-discharge", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&start_id, "queued").unwrap();
        let stop_id = match ledger
            .reserve_stop("fp", "force_discharge", "restart-discharge-stop", 2_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&stop_id, "dispatched").unwrap();

        let mut still_armed = crate::inverter::model::InverterSnapshot {
            enable_discharge: true,
            battery_power_mode: 1,
            timestamp: 3,
            ..Default::default()
        };
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 3_000,
                charge_active: Some(false),
                discharge_active: Some(false),
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "SN1",
                firmware_version: "400",
                snapshot: &still_armed,
                now_ms: 3_000,
            })
            .unwrap();
        assert_eq!(ledger.get(&stop_id).unwrap().unwrap().state, "dispatched");

        still_armed.enable_discharge = false;
        still_armed.timestamp = 4;
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 4_000,
                charge_active: Some(false),
                discharge_active: Some(false),
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "SN1",
                firmware_version: "400",
                snapshot: &still_armed,
                now_ms: 4_000,
            })
            .unwrap();
        assert_eq!(
            ledger.get(&stop_id).unwrap().unwrap().state,
            "readback_confirmed"
        );
    }

    #[test]
    fn pause_readback_requires_exact_mode_and_window() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_start("fp", "pause_both", 30, "pause-key-1", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&command_id, "queued").unwrap();
        ledger
            .record_recovery(
                &command_id,
                r#"{"requested_mode":3,"requested_slot_start":1234,"requested_slot_end":1304,
                    "device_type":"AllInOne3_6kW","inverter_serial":"AIO-TEST","firmware_version":"400"}"#,
            )
            .unwrap();
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 2_000,
                charge_active: Some(false),
                discharge_active: Some(false),
                pause_mode: Some(3),
                pause_slot_start: Some(1234),
                pause_slot_end: Some(1305),
                pause_registers_observed_at_ms: Some(2_000),
                device_type: crate::inverter::model::DeviceType::AllInOne3_6kW,
                inverter_serial: "AIO-TEST",
                firmware_version: "400",
                snapshot: &Default::default(),
                now_ms: 2_000,
            })
            .unwrap();
        assert_eq!(ledger.get(&command_id).unwrap().unwrap().state, "queued");
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 3_000,
                charge_active: Some(false),
                discharge_active: Some(false),
                pause_mode: Some(3),
                pause_slot_start: Some(1234),
                pause_slot_end: Some(1304),
                pause_registers_observed_at_ms: Some(3_000),
                device_type: crate::inverter::model::DeviceType::AllInOne3_6kW,
                inverter_serial: "AIO-TEST",
                firmware_version: "400",
                snapshot: &Default::default(),
                now_ms: 3_000,
            })
            .unwrap();
        assert_eq!(
            ledger.get(&command_id).unwrap().unwrap().state,
            "readback_confirmed"
        );

        let stop_id = match ledger
            .reserve_stop("fp", "pause_mode", "pause-stop-key-1", 4_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger
            .record_recovery(
                &stop_id,
                r#"{"battery_pause_mode":0,"battery_pause_slot_start":0,"battery_pause_slot_end":0,
                    "device_type":"AllInOne3_6kW","inverter_serial":"AIO-TEST","firmware_version":"400"}"#,
            )
            .unwrap();
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 5_000,
                charge_active: Some(false),
                discharge_active: Some(false),
                pause_mode: Some(0),
                pause_slot_start: Some(1),
                pause_slot_end: Some(0),
                pause_registers_observed_at_ms: Some(5_000),
                device_type: crate::inverter::model::DeviceType::AllInOne3_6kW,
                inverter_serial: "AIO-TEST",
                firmware_version: "400",
                snapshot: &Default::default(),
                now_ms: 5_000,
            })
            .unwrap();
        assert_eq!(ledger.get(&stop_id).unwrap().unwrap().state, "accepted");
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 6_000,
                charge_active: Some(false),
                discharge_active: Some(false),
                pause_mode: Some(0),
                pause_slot_start: Some(0),
                pause_slot_end: Some(0),
                pause_registers_observed_at_ms: Some(6_000),
                device_type: crate::inverter::model::DeviceType::AllInOne3_6kW,
                inverter_serial: "AIO-TEST",
                firmware_version: "400",
                snapshot: &Default::default(),
                now_ms: 6_000,
            })
            .unwrap();
        assert_eq!(
            ledger.get(&stop_id).unwrap().unwrap().state,
            "readback_confirmed"
        );
        assert!(!ledger.has_active_start("pause_both").unwrap());
        assert!(ledger.active_recoveries().unwrap().is_empty());
    }

    #[test]
    fn ac3_pause_readback_requires_only_hr318() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_start("fp", "pause_discharge", 30, "ac3-pause-key-1", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&command_id, "queued").unwrap();
        ledger
            .record_recovery(
                &command_id,
                r#"{"requested_mode":2,"requested_slot_start":0,"requested_slot_end":0,
                    "device_type":"ACCoupled","inverter_serial":"AC3-TEST","firmware_version":"400"}"#,
            )
            .unwrap();
        ledger
            .advance_readback(&pause_evidence(
                2_000,
                Some(2_000),
                "AC3-TEST",
                "400",
                crate::inverter::model::DeviceType::ACCoupled,
                2,
                2461,
                9999,
            ))
            .unwrap();
        assert_eq!(
            ledger.get(&command_id).unwrap().unwrap().state,
            "readback_confirmed"
        );

        let stop_id = match ledger
            .reserve_stop("fp", "pause_mode", "ac3-pause-key-2", 3_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger
            .record_recovery(
                &stop_id,
                r#"{"battery_pause_mode":0,"battery_pause_slot_start":0,"battery_pause_slot_end":0,
                    "device_type":"ACCoupled","inverter_serial":"AC3-TEST","firmware_version":"400"}"#,
            )
            .unwrap();
        ledger
            .advance_readback(&pause_evidence(
                4_000,
                Some(4_000),
                "AC3-TEST",
                "400",
                crate::inverter::model::DeviceType::ACCoupled,
                0,
                1234,
                9999,
            ))
            .unwrap();
        assert_eq!(
            ledger.get(&stop_id).unwrap().unwrap().state,
            "readback_confirmed"
        );
    }

    #[test]
    fn ac3_mode_only_recovery_survives_long_restart_until_restored() {
        let ledger = isolated_ledger();
        let started_at = FIXED_NOW_MS;
        let command_id = match ledger
            .reserve_pause_start(
                "fp",
                "pause_discharge",
                "discharge",
                30,
                "ac3-long-restart",
                started_at,
            )
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        let recovery = r#"{"device_type":"ACCoupled","firmware_version":"400","inverter_serial":"AC3-TEST","requested_mode":2,"requested_slot_start":0,"requested_slot_end":0}"#;
        ledger.record_recovery(&command_id, recovery).unwrap();
        ledger.mark_state(&command_id, "queued").unwrap();
        ledger
            .advance_readback(&pause_evidence(
                started_at + 1_000,
                Some(started_at + 1_000),
                "AC3-TEST",
                "400",
                crate::inverter::model::DeviceType::ACCoupled,
                2,
                0,
                0,
            ))
            .unwrap();
        assert_eq!(
            ledger.get(&command_id).unwrap().unwrap().state,
            "readback_confirmed"
        );
        ledger
            .with_connection(|connection| {
                connection
                    .execute(
                        "UPDATE external_commands SET updated_ms = ?2 WHERE id = ?1",
                        params![command_id, started_at + 1_000],
                    )
                    .map_err(|error| error.to_string())?;
                Ok(())
            })
            .unwrap();
        let after_restart = started_at + RETENTION_MS + 60_000;
        assert_eq!(
            ledger.active_recoveries_at(after_restart).unwrap(),
            vec![("pause_discharge".to_string(), recovery.to_string())]
        );
        assert!(ledger.recovery_pending_for_test(&command_id).unwrap());

        ledger.clear_recovery(&command_id).unwrap();
        assert!(ledger
            .active_recoveries_at(after_restart)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn pause_readback_rejects_hr318_for_unconfirmed_ac_coupled() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_start("fp", "pause_discharge", 30, "ac3-unsupported-key", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&command_id, "queued").unwrap();
        ledger
            .record_recovery(
                &command_id,
                r#"{"requested_mode":2,"requested_slot_start":0,"requested_slot_end":0,
                    "device_type":"ACCoupledMk2","inverter_serial":"AC3-TEST","firmware_version":"400"}"#,
            )
            .unwrap();
        ledger
            .advance_readback(&pause_evidence(
                2_000,
                Some(2_000),
                "AC3-TEST",
                "400",
                crate::inverter::model::DeviceType::ACCoupledMk2,
                2,
                2461,
                9999,
            ))
            .unwrap();
        assert_eq!(ledger.get(&command_id).unwrap().unwrap().state, "queued");
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
                charge_active: Some(false),
                discharge_active: Some(false),
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "TEST",
                firmware_version: "400",
                snapshot: &Default::default(),
                now_ms: 70_000,
            })
            .unwrap();
        assert_eq!(ledger.get(&command_id).unwrap().unwrap().state, "accepted");
        // …but well past it (grace elapsed) it becomes unknown.
        ledger
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 500,
                charge_active: Some(false),
                discharge_active: Some(false),
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "TEST",
                firmware_version: "400",
                snapshot: &Default::default(),
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
            .record_recovery(
                &command_id,
                r#"{"device_type":"ACCoupled","inverter_serial":"TEST"}"#,
            )
            .unwrap();
        ledger2
            .advance_readback(&ReadbackEvidence {
                snapshot_ts_ms: 62_000,
                charge_active: Some(false),
                discharge_active: Some(false),
                pause_mode: None,
                pause_slot_start: None,
                pause_slot_end: None,
                pause_registers_observed_at_ms: None,
                device_type: crate::inverter::model::DeviceType::ACCoupled,
                inverter_serial: "TEST",
                firmware_version: "400",
                snapshot: &Default::default(),
                now_ms: 70_000,
            })
            .unwrap();
        assert_eq!(ledger2.get(&command_id).unwrap().unwrap().state, "expired");
    }

    #[test]
    fn active_recovery_is_available_before_startup_reconciliation() {
        let ledger = isolated_ledger();
        let command_id = match ledger
            .reserve_start("fp", "pause_charge", 30, "key-1", 1_000)
            .unwrap()
        {
            Reservation::Accepted { command_id } => command_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        ledger.mark_state(&command_id, "queued").unwrap();
        ledger
            .record_recovery(&command_id, r#"{"requested_mode":1}"#)
            .unwrap();
        assert_eq!(
            ledger.active_recoveries().unwrap(),
            vec![(
                "pause_charge".to_string(),
                r#"{"requested_mode":1}"#.to_string()
            )]
        );
        assert_eq!(ledger.reconcile_startup().unwrap(), 1);
        assert_eq!(
            ledger.active_recoveries().unwrap(),
            vec![(
                "pause_charge".to_string(),
                r#"{"requested_mode":1}"#.to_string()
            )]
        );
        assert_eq!(ledger.get(&command_id).unwrap().unwrap().state, "unknown");
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
