---
title: "feat: Add authenticated battery pause control"
type: feat
status: active
date: 2026-09-14
---

<!-- markdownlint-disable MD025 -->

# Add Authenticated Battery Pause Control

## Goal

Add finite Pause Charge, Pause Discharge, and Pause Both actions to HEM's authenticated external API.

The API must not be AC3-specific. It should work on every inverter model for which HEM can safely perform the requested operation. Every authenticated control mutation—including the four existing Force Start/Stop routes—must return `422 unsupported_control` before command reservation, state capture, or writes when that operation is unsafe or unconfirmed. Read-only authenticated API access remains available on every model.

Native pause is an inverter/plant-level control. A second daisy-chained battery or multiple batteries behind a Gateway are controlled together; there is no battery selector.

## Design Principles

- Reuse HEM's existing model-aware encoders, command ledger, write queue, action lock, expiry handling, and readback confirmation.
- Keep one central capability decision per operation and model/firmware combination.
- Prefer a clear refusal over attempting an unconfirmed register write.
- Preserve and restore the exact previous inverter settings.
- Keep the existing dashboard Eco Pause and Timed Discharge behavior unchanged.
- Do not create a general lease framework for this feature.

## API

### Start

```http
POST /api/control/pause-mode
Authorization: Bearer <api-key>
Idempotency-Key: <unique-key>
Content-Type: application/json

{"mode":"charge","minutes":60}
```

`mode` is one of:

- `charge` — native mode 1
- `discharge` — native mode 2
- `both` — native mode 3

`minutes` is an integer from 1 through 1439. A finite duration avoids pretending that HR319/320 can represent an indefinite, gap-free pause.

Only one Force or pause action may own battery control at a time. A new start with a different idempotency key is refused while another action is active; an identical retry with the same key replays its original result.

### Stop

```http
POST /api/control/pause-mode/stop
Authorization: Bearer <api-key>
Idempotency-Key: <unique-key>
```

Stop restores the raw HR318/319/320 values captured before Start. Every Stop passes through the same capability gate as Start. An inactive Stop is a successful no-op only when native pause control is supported on the connected inverter; otherwise it returns `422 unsupported_control`.

As with the existing Force Stop routes, an authenticated Stop may recover a HEM-owned pause after `api_control_enabled` is turned off. Recovery still requires capability and inverter-identity checks. If no owned action exists, the normal permission check applies.

### Responses

Use the existing external-control response shape:

```json
{"ok":true,"message":"Battery pause queued","command_id":"..."}
```

Acceptance means queued, not confirmed. Clients poll `GET /api/commands/{command_id}` or `GET /api/control/status` for readback.

Errors retain `ok: false` and `error`, with an additive stable `code`. Important results are:

| HTTP | Code | Meaning |
|---|---|---|
| 400 | `invalid_request` | Invalid body, duration, or idempotency key |
| 401 | `unauthorized` | Missing or wrong bearer |
| 403 | `control_disabled` | Writes are disabled and no owned action needs recovery |
| 409 | `control_conflict` | Another Force or pause action is active |
| 422 | `unsupported_control` | This operation is unsafe or unconfirmed on the connected inverter |
| 503 | `state_unavailable` | A fresh connected snapshot, inverter clock, or restore baseline is unavailable |

Existing authenticated API failures keep their current HTTP behavior. Adding `code` must not remove fields existing clients use. `422 unsupported_control` applies consistently to all six authenticated mutation routes: Force Charge Start/Stop, Force Discharge Start/Stop, and native Pause Start/Stop. Authenticated read routes report capabilities instead of returning `422`.

## Model Capabilities

Create one model-aware capability source used by every authenticated mutation route, including the four Force routes already implemented. Each route maps to an underlying operation before it can reserve a command or call the existing handler. The capability source reports:

- Force Charge support
- Force Discharge support
- native Pause Charge support
- native Pause Discharge support
- native Pause Both support

Refactor the existing `supports_timed_discharge(arm_fw)` boundary into a shared native-pause capability source so Timed Discharge and external pause cannot maintain conflicting model lists. Distinguish HR318 mode support from the full HR318-320 window: mode support remains explicit, and proof of one pause mode does not automatically prove modes 1 and 3.

The confirmed native-pause families are:

- **Mode and window:** AC Three Phase; All-in-One 3.6 kW, 5 kW, and 6 kW; Gen3 Hybrid with ARM firmware 312 or newer.
- **Mode only:** legacy AC-coupled AC3 (`0x3001`), where HR318 works but HR319/320 are ignored or rejected. The authenticated API owns the finite timer for this path; dashboard Timed Discharge remains unavailable because it needs the inverter-side window.

Enable each pause mode only on model/firmware rows where that mode is confirmed. Derive Force support from the model-routed writes HEM already knows how to encode safely, then explicitly refuse families without a valid write and readback path. Every unsupported Start or Stop returns `422 unsupported_control`. Future inverter support requires an explicit capability entry and tests, not handler changes.

Expose the result through `GET /api/control/status`, for example:

```json
{
  "control_capabilities": {
    "force_charge": true,
    "force_discharge": true,
    "pause_modes": ["charge", "discharge", "both"]
  }
}
```

## Safe Control Flow

### Capture

Before queueing Start:

1. Authenticate, authorize, and validate the request.
2. Resolve a connected, fresh model/firmware snapshot and run the shared capability gate. Return `422 unsupported_control` if the known inverter does not support the operation, or `503 state_unavailable` if support cannot be determined.
3. Reserve the idempotent command.
4. For full-window models, require a valid inverter clock; do not fall back to host time. AC3 mode-only pauses use HEM's finite timer and do not need the inverter clock.
5. Require a successful fresh read of HR318. Full-window models additionally require HR319-320.
6. Capture the raw registers supported by that capability plus inverter serial, device type, firmware, and read generation.
7. Persist that restore point before queueing writes.

The snapshot must retain raw pause-register values. The decoded `ScheduleSlot` is lossy and cannot be used as the restore point. Carried-forward register values cannot capture or confirm an action.

### Write

For full-window models, calculate a finite pause window from the inverter's current minute and duration, including midnight wrap. Queue one fail-fast owned batch in this order:

1. HR319 pause start
2. HR320 pause end
3. HR318 pause mode

For AC3 mode-only control, queue only HR318 and let HEM's duration timer trigger restoration. The batch carries its command ID and raw rollback values. If a full-window slot write fails after an earlier write succeeds, the poll loop restores the captured raw values and never attempts HR318. Failed rollback remains recoverable through Stop; it must not discard the restore point.

### Restore

Stop and expiry restore the captured registers supported by the capability, writing HR319/320 first and HR318 last when a window exists. AC3 restores only HR318. Ownership is released only after a newer successful read confirms every write in the selected path.

Persist the pause restore data using the existing command-ledger recovery mechanism. Keep an active or recovery-required command exempt from normal terminal-row trimming until restoration is confirmed. On restart, restore only after a fresh snapshot confirms the same inverter identity.

### Deferred review note

The owner accepted the low likelihood of a same-serial model or pause-capability change during an action. Three review findings remain intentionally deferred: Pause Start can use a newer snapshot than its capability check; explicit Stop and failed-start rollback derive writes from the captured model; and restoration readback can clear ownership without checking the current model's pause capability. The automatic expiry writer already checks the current capability. Revisit these paths if the AC3 user reports a pause that will not stop, an unexpected register-write failure, or recovery ownership clearing incorrectly. No change is planned solely for these model-change scenarios.

### Arbitration

Reuse the existing battery-action lock and owner model:

- Reject Pause Start while any Force or pause action is queued, dispatching, or active.
- Reject Force Start while native pause is queued, dispatching, or active.
- Prevent Timed Export and other automation from writing overlapping battery-mode registers while pause owns the domain.
- Hold the action lock while the poll loop extracts and dispatches an owned batch, closing the gap where a batch has left the queue but has not reached the inverter.
- Mark dispatch/readback by command ID, not by updating every queued command.

## Implementation

### 1. Capabilities and raw readback

Update:

- `src-tauri/src/inverter/model.rs`
- `src-tauri/src/inverter/decoder.rs`
- `src-tauri/src/inverter/encoder.rs`
- `src-tauri/src/inverter/poll.rs`
- `src-tauri/src/server/control_status.rs`

Add the central operation matrix, exhaustive model tests, raw HR318-320 readback/provenance, and status capability output. Correct the stale HR318 mode comment in `encoder.rs`.

### 2. Pause action

Update:

- `src-tauri/src/server/api.rs`
- `src-tauri/src/inverter/state_machines.rs`
- `src-tauri/src/inverter/poll.rs`
- `src-tauri/src/server/external_commands.rs`

Add the finite pause encoder, `PauseModeRevert`, fail-fast rollback metadata, expiry, exact restoration, restart recovery, and command-specific readback.

### 3. Authenticated routes

Update:

- `src-tauri/src/server/mod.rs`
- `src-tauri/src/server/external_control.rs`

Add Start and Stop using the existing auth, permission, audit, rate-limit, idempotency, and command-response helpers. Add one shared pre-reservation capability guard to all six authenticated mutation routes, including existing Force Stops, and include `Idempotency-Key` in authenticated CORS preflight coverage.

### 4. Documentation

Update `REMOTE_CONTROL.md` and `README.md` with routes, supported modes, duration, status polling, errors, retries, restoration, and aggregate multi-battery behavior.

## Required Tests

Use TDD, including a genuinely concurrent regression test for the queue race.

- Every `DeviceType` has explicit support results for Force Charge, Force Discharge, and each native pause mode; every one of the six mutation routes is table-tested against those results.
- Timed Discharge and external pause share one HR318-320 capability boundary.
- Unsupported existing Force and new Pause Start/Stop requests return `422 unsupported_control` before command reservation, state capture, or writes.
- Invalid mode, duration, JSON, auth, permission, and idempotency requests are rejected.
- Missing/malformed inverter time returns `503`; host time is never used.
- Mode 1, 2, and 3 produce the expected HR318 write where supported.
- Normal and midnight-wrap windows produce the expected HR319/320 values.
- Failure of HR319 or HR320 prevents HR318 and exercises successful and failed rollback.
- Stop and expiry restore disabled sentinels and pre-existing Timed Discharge values exactly.
- Stale, carried-forward, or identity-mismatched reads never confirm or restore.
- Same-key retry replays; a new start while active returns `409` without replacing the restore point.
- Inactive Stop returns `200` only on a supported inverter and `422` otherwise; permission-revoked recovery, expiry, trimming, reconnect, and restart are covered.
- A barrier test races an extracted Force batch against Pause Start and proves that only one owner can write.
- Existing Force, Eco Pause, Timed Discharge, main-server CORS, and aggregate multi-battery behavior remain unchanged.

## Verification

```bash
cd src-tauri
cargo fmt --check
cargo clippy
cargo test
cd ..
npm run lint
npm run lint:md
npm run build
npm run test
```

Use the simulator for normal/wrap timing, multiple-battery aggregate behavior, and unsupported-operation refusal. Require real-device evidence before enabling a new model/mode capability.

## References

- GitHub issue [#301](https://github.com/psylsph/home-energy-manager/issues/301)
- `src-tauri/src/server/external_control.rs`
- `src-tauri/src/server/external_commands.rs`
- `src-tauri/src/inverter/model.rs`
- `src-tauri/src/inverter/encoder.rs`
- `src-tauri/src/inverter/poll.rs`
- `REMOTE_CONTROL.md`
