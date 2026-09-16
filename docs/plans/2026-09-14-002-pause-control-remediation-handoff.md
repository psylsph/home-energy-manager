# Pause control remediation handoff

## Objective

Finish the safety review remediation for authenticated finite native battery Pause controls and the related authenticated Force Charge / Force Discharge restoration paths.

The branch is `feat/external-battery-pause`. The review merge base is:

```text
f60d9ade9f882f3ea7333b73535ee6b268753e93
```

The owner explicitly asked to implement fixes first and add regression tests afterward (no TDD for this remediation pass).

Do not alter unrelated `INSTALL.md` work if it reappears. Do not change the main server's `0.0.0.0` bind or permissive CORS behavior.

## Working tree

At handoff, these files are modified:

```text
src-tauri/src/inverter/poll.rs
src-tauri/src/inverter/state_machines.rs
src-tauri/src/lib.rs
src-tauri/src/server/api.rs
src-tauri/src/server/external_commands.rs
src-tauri/src/server/external_control.rs
docs/plans/2026-09-14-002-pause-control-remediation-handoff.md (new)
```

`git diff --check` passes.

The latest compile check passes:

```bash
cargo fmt --manifest-path src-tauri/Cargo.toml
cd src-tauri && cargo check --tests
```

No full test suite has been run since the in-progress remediation edits. The new behavior still needs regression coverage and existing tests will probably need expectation updates.

## Fixes already implemented in this remediation pass

### Pause deadlock

`run_pause_start` and `run_pause_stop` now drop `force_action_lock` immediately after queueing their fail-fast write batch and before awaiting completion. This removes the lock/wait cycle with poll-loop snapshot publication.

Rollback is queued only after that guard has been dropped.

### Batch-specific dispatch state

`PendingWriteBatch` now has:

```rust
pub command_id: Option<String>
```

Authenticated pause start/stop batches carry their ledger command ID. Poll-loop dispatch collects IDs only from the batches actually extracted for that pass and calls:

```rust
CommandLedger::mark_dispatched(&command_ids)
```

Deferred owner batches no longer cause unrelated commands to be marked dispatched.

### Pause evidence provenance

`InverterSnapshot` has a non-serialized `battery_pause_registers_observed_at` timestamp. The decoder stamps it when HR318–320 are freshly decoded; sanitizer carry-forward copies raw values without copying that timestamp.

`ReadbackEvidence` now includes:

- pause-register observation timestamp,
- device type,
- inverter serial,
- firmware version,
- a reference to the complete decoded snapshot.

Pause start and stop confirmation require:

- `pause_registers_observed_at_ms == snapshot_ts_ms`,
- exact raw HR318/319/320 values,
- identity matching the stored pause recovery target.

Pause restoration clearing also requires the raw observation timestamp to equal the current snapshot timestamp and the inverter identity to match.

### Pause lifecycle and idempotency

Pause starts now use a stable endpoint scope (`/api/control/pause-mode`) and a request hash over canonical mode plus minutes. Reusing an idempotency key with another mode or duration should conflict.

`clear_recovery` now also sets `active = 0`.

`complete_restoration(action)` was added to atomically clear active ownership/recovery for an action after exact restoration readback.

Ledger trimming now excludes active or recovery-pending terminal rows.

The schema migration now marks start rows with recovery active for `accepted`, `queued`, `dispatched`, and `readback_confirmed` states.

Several pause failure paths now persist the exact response envelope returned to the caller, including the final fail-fast write error. The remaining response-envelope audit is listed below.

### Force restoration ownership (partially complete)

Core Force Charge and Force Discharge Stop no longer consume their revert with `.take()` before restoration. They clone the revert and retain it until confirmation.

`ForceRestorationRequest` records:

- request timestamp,
- device type,
- inverter serial,
- firmware version.

Poll publication calls `clear_confirmed_force_restorations`, which requires a causally newer snapshot, matching identity, and a decoded snapshot matching every generated restoration write before clearing the revert and durable ledger ownership.

Force Discharge automatic expiry now retains its revert and retries restoration rather than consuming it once. It uses the same exact stop-write builder as explicit Stop.

External Force Stop now persists its exact revert baseline on the stop command before calling the core stop path.

Core Force starts now reject an active native Pause, complementing Pause's rejection of active Force controls.

## Outstanding implementation work

### 1. Finish exact terminal response replay

Audit every ledger reservation path in `src-tauri/src/server/external_control.rs` after a command ID has been accepted.

Remaining suspicious sites found by grep:

```text
run_start around lines 477 and 484
run_stop around line 913
```

These still call `mark_state(..., "failed")` and return a response without storing the exact terminal envelope. Replace them with the same exact-envelope behavior used by `finish_response` / `store_envelope`.

Also inspect the non-2xx `finish(...)` calls around the end of `run_start` and `run_stop`. Ensure the envelope body is byte-for-byte equivalent at the JSON value level to what the current request returns, including `code`, formatted error text, and `command_id` where present.

For pause start/stop, verify every post-reservation exit stores an envelope. In particular, review ignored errors while serializing or updating a restoration recovery payload.

### 2. Validate and refine Force exact matching

The new helper is:

```rust
server::api::snapshot_matches_writes
```

Review its register-to-snapshot mapping carefully against decoder semantics for:

- `HR_ENABLE_CHARGE`,
- `HR_ENABLE_CHARGE_TARGET`,
- single-phase charge/discharge slots,
- three-phase force and AC charge flags,
- target SOC,
- battery power mode,
- HR318–320 raw pause values.

Do not release Force ownership based on normalized or unrelated fields. If a generated restoration write cannot be proven from the decoded snapshot, add the required raw provenance field rather than weakening the match.

Check whether `snapshot.timestamp * 1000 > requested_at_ms` is reliable at one-second timestamp resolution. A request late in a second should deliberately require the next second's fresh poll; tests should pin this behavior.

### 3. Make Force restart recovery complete

The retained Force reverts currently live in memory and the original start command's recovery row. Verify restart hydration in `restore_external_recoveries` and ensure a restart during restoration retains enough information to retry and confirm safely.

Potential gap: `ForceRestorationRequest` itself is not durable. After restart, hydration restores the start revert but not an explicit “restoration in progress” timestamp. Decide whether the poll loop or authenticated Stop must reconstruct this state. Preserve the policy that restart never auto-resumes a Force action, but restoration must remain available to an authenticated Stop.

Confirm that readback-confirmed Force starts remain in `active_recoveries()` and hydrate before polling.

### 4. Review Force Stop readback lifecycle

`advance_readback` now parses Force stop recovery and calls the exact write matcher. Verify:

- stop rows with an exact captured baseline confirm only on exact fresh restoration;
- fallback stop rows without a baseline retain the intended behavior for stopping an externally-created physical Force state;
- confirming a stop deactivates the owning start row and clears only the correct recovery;
- an old/foreign snapshot cannot confirm a Force stop;
- command deadline handling does not turn a still-owned, retryable restoration into an unrecoverable state.

The Force recovery payload itself does not currently contain inverter identity. The in-memory `ForceRestorationRequest` does. Consider persisting identity with the stop recovery envelope or extending the Force revert structs so restart/readback confirmation cannot accept a foreign inverter snapshot.

### 5. Retry explicit Force restoration after failed batches

Core Force Stop uses the normal fire-and-forget queue rather than a fail-fast completion channel. Ownership is retained, but explicit Stop does not yet automatically enqueue another batch after a Modbus failure. Verify poll-loop behavior and add a bounded retry path if necessary.

The required invariant is: a failed or interrupted restoration must retain ownership and retry safely until exact fresh readback succeeds, or remain recoverable by another authenticated Stop.

### 6. Remove or justify test-only dead-code annotations

Because Force Discharge expiry now uses `build_force_discharge_stop_writes`, these state-machine helpers became production-unused and were temporarily marked `#[cfg(test)]`:

```text
encode_hhmm_or_clear
push_pause_restore_writes
build_force_discharge_auto_revert_writes
```

Decide whether to delete the obsolete implementation/tests, retain it as a tested helper used by production, or keep it explicitly test-only with a clear reason. Avoid dead-code warnings under Clippy.

### 7. Update existing tests for retained Force ownership

Tests that expected Force Stop or expiry to immediately consume the revert will fail. Update them to assert:

1. the baseline remains after queueing/restoration writes;
2. stale or mismatched snapshots do not clear it;
3. a causally fresh exact snapshot clears it;
4. ledger ownership/recovery is cleared only at that point.

The test cleanup helper in `server/api.rs` still uses `.take()` intentionally to reset test state; it should also clear the new Force restoration trackers.

### 8. Fix pause readback test fixtures

`external_commands.rs` test evidence initializers were bulk-filled so compilation succeeds, but the pause recovery JSON in existing tests does not yet include the target identity fields required by the new matcher.

For example, `pause_readback_requires_exact_mode_and_window` currently records only:

```json
{"requested_mode":3,"requested_slot_start":1234,"requested_slot_end":1304}
```

Add `device_type`, `inverter_serial`, and `firmware_version` matching the evidence. Also fix the observation timestamp in the “fresh exact” cases: it must equal `snapshot_ts_ms`, not remain at `2000` when the snapshot is `3000`, `5000`, or `6000`.

The temporary `snapshot: &Default::default()` values are sufficient only for non-Force tests. Exact Force tests need purpose-built snapshots reflecting the expected restoration writes.

## Required regression tests

Add deterministic tests after completing implementation, covering every review finding.

### Concurrent deadlock regression

Create a genuinely concurrent test where a pause request queues writes and waits while poll publication/drain needs `force_action_lock`. Prove both tasks complete under a short deterministic timeout. A sequential test is insufficient.

### Fresh same-inverter pause evidence

Cover:

- carried-forward HR318–320 cannot confirm start;
- carried-forward HR318–320 cannot clear restoration;
- foreign serial, firmware, or device type cannot confirm;
- exact same-inverter freshly observed raw values confirm;
- stale snapshot timestamps cannot confirm.

### Pause expiry ownership

After exact expiry restoration readback:

- the start row becomes inactive,
- recovery is cleared,
- a new pause can be reserved/started,
- the completed row is replayable but no longer blocks battery controls.

### Pause idempotency and exact replay

Cover:

- aliases canonicalize (`pause_charge` and `charge` are equivalent);
- same key + same canonical mode + same duration replays;
- same key + different mode conflicts;
- same key + different duration conflicts;
- failed pause start replay returns exactly the original status/body;
- failed pause stop replay returns exactly the original status/body;
- mutable connection/capability/rate-limit state changing after the first result does not alter replay.

### Batch-specific dispatch

Queue at least two external command batches under different owners, drain only the admitted owner, and prove only that batch's command ID transitions to `dispatched`.

### Force restoration

For both Force Charge and Force Discharge:

- explicit Stop retains baseline before readback;
- failed/interrupted restoration retains ownership;
- stale evidence does not clear;
- partial/mismatched decoded values do not clear;
- foreign identity does not clear;
- exact fresh restoration clears memory and durable ownership;
- a second authenticated Stop can retry retained restoration;
- restart hydration preserves recovery after a failed restore.

For Force Discharge expiry, test failed first restoration and successful retry.

## Suggested next commands

Start with focused tests and Clippy while iterating:

```bash
cargo fmt --manifest-path src-tauri/Cargo.toml
cd src-tauri
cargo test external_commands
cargo test pause
cargo test force_charge
cargo test force_discharge
cargo clippy --all-targets -- -D warnings
```

Then run the complete project validation in the required order:

```bash
cd src-tauri && cargo fmt --check
cd src-tauri && cargo clippy --all-targets -- -D warnings
npm run lint
npm run lint:md
npm run build
cd src-tauri && cargo test
npm run test
npm run test:e2e
docker build .
npm run check:versions
git diff --check
```

If environment constraints prevent Playwright or Docker, report the exact blocker; do not claim those checks passed.

## Final review checklist

Before declaring merge-ready:

- no action lock is held while awaiting a queued write completion;
- every confirmation uses causally fresh, same-inverter evidence;
- exact raw pause provenance is never carried forward as fresh;
- each drained batch updates only its own ledger command;
- every accepted idempotency reservation has an exact replayable terminal envelope;
- active/recovery-pending rows cannot be trimmed;
- Pause and Force starts are mutually exclusive in memory and durable ownership;
- restoration ownership survives failure and restart until exact readback;
- successful restoration atomically releases the owning start;
- full Rust/frontend/docs/version/diff validation passes.
