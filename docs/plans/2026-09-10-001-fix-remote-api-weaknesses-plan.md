---
title: "fix: Harden the authenticated remote-control API"
type: fix
status: active
date: 2026-09-10
---

<!-- markdownlint-disable MD025 -->

# Harden the Authenticated Remote-Control API

## Overview

Harden the separate authenticated API used by external integrations without changing the main dashboard/API server's deliberately permissive bind, CORS, or authentication behavior. The work is delivered in dependency-ordered phases: establish a threat model and secure configuration contract, protect credentials and listener lifecycle, harden the HTTP boundary, minimise exposed data, then add durable command identity and readback state.

The existing Quick Action handlers remain the source of inverter-write behavior. The external API becomes a safer adapter around them rather than a second control implementation.

---

## Problem Frame

The authenticated API currently uses a shared bearer key stored in plaintext settings and backups, binds to all interfaces, permits wildcard CORS, has no failed-authentication throttling or audit trail, exposes the full inverter snapshot, and returns acceptance of queued writes without a durable command identity. Key and port changes are read from disk by authentication but do not reconfigure the already-running listener.

These weaknesses matter because the API can change real battery operation. A leaked key can read detailed household energy data and, when the global control toggle is enabled, invoke every external Quick Action. A timed-out write can also leave an integration unable to distinguish rejection from an accepted command.

The existing trust boundary is valuable and must remain: a separate port, authentication on every external route, read-only defaults for starts, explicit start permission plus owned-action recovery stops, strict external JSON validation, and delegation to the existing Quick Action handlers.

---

## Requirements Trace

- **R1. Separate-boundary preservation:** Do not alter the main server's `0.0.0.0` bind, wildcard CORS, or normal tokenless dashboard/API routes; R9 is the narrow exception for security-sensitive authenticated-API configuration writes.
- **R2. Credential protection:** Generate or accept only strong API credentials, compare them safely, avoid plaintext persistence/logging, support migration, and provide explicit rotation/revocation.
- **R3. Exposure control:** Make the authenticated listener's bind address and CORS policy explicit, safe by default for new installations, and reconfigurable without stale listeners.
- **R4. Abuse resistance:** Add bounded request bodies, failed-authentication throttling, targeted control limits, and useful `429` responses without throttling normal dashboard traffic.
- **R5. Least data exposure:** Return an allow-listed external snapshot rather than the complete internal `InverterSnapshot`.
- **R6. Honest command state:** Give every external mutation a stable command identity and distinguish accepted/queued, readback-confirmed, failed, expired, and unknown states.
- **R7. Safe retries:** Add idempotency handling so duplicate or timed-out external requests cannot silently enqueue repeated starts or replace restore state.
- **R8. Operational evidence:** Record security and control events without secrets, retain bounded audit data, and document deployment, rotation, recovery, and emergency limitations.
- **R9. Configuration-plane protection:** Do not leave API credentials, control permission, bind address, or CORS policy writable through the tokenless network settings control plane; preserve the main server's general compatibility surface while moving these fields behind a local administration boundary.
- **R10. Recovery safety:** Revoking start permission must not leave an accepted force action without a documented, authorized stop/recovery path; restart and uncertain queue states must not silently re-arm or strand control.

---

## Scope Boundaries

- The main dashboard/API server remains bound to `0.0.0.0` with permissive CORS, as required by `AGENTS.md`; this plan does not add authentication, custom headers, or a global bind restriction to that server.
- Security-sensitive authenticated-API configuration fields are removed from the tokenless remote settings write path. Desktop configuration uses the local Tauri/UI path, and headless/local administration uses a loopback-only control path or equivalent local configuration mechanism; ordinary dashboard settings and compatibility routes remain unchanged.
- This plan does not add general user accounts, OAuth, cloud identity, or multi-tenant authorization.
- This plan does not make HEM a safety-certified controller or replace inverter protections and physical emergency controls.
- TLS termination remains the responsibility of a trusted reverse proxy or VPN rather than adding a new certificate-management subsystem to HEM.
- Existing Quick Action register encoding and model-specific inverter behavior remain in the existing handlers.
- Historical full snapshots are not made available through the authenticated API merely for compatibility; integrations needing detailed data must use the documented safe projection or an explicitly reviewed future interface.

### Deferred to Follow-Up Work

- Native OS keychain integration after the cross-platform file-backed credential migration is stable.
- Multiple independently revocable client tokens with separate read/control scopes.
- mTLS or in-process TLS for deployments that cannot use a trusted proxy/VPN.
- Tamper-evident or remote audit-log forwarding.

---

## Context & Research

### Relevant Code and Patterns

- `src-tauri/src/server/mod.rs`: authenticated router, bearer middleware, CORS, listener startup, existing auth tests, and middleware ordering.
- `src-tauri/src/server/external_control.rs`: strict duration validation, permission middleware, external action adapters, and integration tests.
- `src-tauri/src/server/api.rs`: settings persistence endpoint, full snapshot serialization, Quick Action handlers, redacted settings logging, and existing action tests.
- `src-tauri/src/server/control_status.rs`: cached status construction, freshness rules, Quick Action phases, and conditions.
- `src-tauri/src/settings/mod.rs`: settings defaults, atomic read-modify-write, backups, corruption quarantine, and isolated configuration tests.
- `src-tauri/src/inverter/poll.rs`: queued writes, dispatch/readback flow, Force Discharge expiry restoration, and shared application state.
- `src-tauri/src/lib.rs`: Tauri and headless startup paths, which currently spawn the authenticated listener without a retained lifecycle handle.
- `src-tauri/tests/e2e_mock.rs` and `src-tauri/tests/headless_smoke.rs`: existing in-process and real-process test patterns.
- `src/pages/SettingsPage.tsx` and `src/lib/types.ts`: current API key, port, and control-permission configuration UI and payload shape.
- `REMOTE_CONTROL.md` and `README.md`: public API contract, usage examples, weaknesses, and operational guidance.

### Institutional Learnings

- Keep the authenticated API narrow and layered; settings and WebSocket routes must remain absent.
- Secret values must never enter logs or support output; metadata-only settings responses are an established pattern.
- Settings changes must use the existing transactional update semantics and preserve concurrent unrelated fields.
- Quick Action responses represent acceptance/queueing, not inverter completion; duplicate starts can replace restore state.
- Real listener lifecycle tests are needed in addition to `Router::oneshot()` tests.
- `docs/solutions/` is not present in this repository; relevant historical patterns were recovered from source and git history.

### External References

- [RFC 6750](https://www.rfc-editor.org/rfc/rfc6750.html): protect bearer tokens in transit and do not place them in URLs.
- [OWASP REST Security Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/REST_Security_Cheat_Sheet.html): disable unnecessary CORS and constrain origins when browser access is required.
- [OWASP Logging Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Logging_Cheat_Sheet.html): log authentication, authorization, configuration, and high-risk events without secrets.
- [RFC 6585](https://www.rfc-editor.org/rfc/rfc6585.html): `429 Too Many Requests` and `Retry-After` behavior.
- [Axum 0.8 middleware documentation](https://docs.rs/axum/0.8.9/axum/middleware/): middleware ordering and route coverage.
- [Tower HTTP 0.7 request limits](https://docs.rs/tower-http/0.7.1/tower_http/limit/): bounded request bodies.
- [Axum graceful shutdown](https://docs.rs/axum/0.8.9/axum/serve/struct.Serve.html): listener shutdown and request draining.
- [NIST SP 800-57](https://csrc.nist.gov/pubs/sp/800/57/pt2/r1/final): risk-based key lifecycle and rotation guidance.

---

## Key Technical Decisions

- **Harden only the separate API.** The main router's compatibility behavior is an explicit invariant, not a finding to fix.
- **Use generated high-entropy credentials as the preferred path.** Generate at least 32 random bytes with the platform CSPRNG, encode them for copying, store a versioned SHA-256 verifier plus non-secret metadata, and make all newly generated credentials satisfy the strong policy. A legacy plaintext key is accepted only during the one-time migration state: the first successful authenticated request or explicit local rotation must atomically replace it with the verifier and scrub HEM-owned plaintext artifacts; if migration cannot commit, fail closed for subsequent API use and require local recovery. Verifier-over-legacy precedence and old-key invalidation are explicit.
- **Use a file-backed verifier first, with restrictive permissions and deterministic migration cleanup.** This is cross-platform and works in desktop and headless deployments. A migration must handle `settings.json`, `.bak`, `.tmp`, and `.corrupt` artifacts explicitly; “best effort” cleanup is not the security boundary. OS keychain support remains deferred.
- **Protect the configuration control plane separately.** The main server remains permissive for normal dashboard compatibility, but network callers cannot change API credentials, control permission, bind address, or CORS policy through the tokenless settings route. Desktop IPC/local UI and loopback-only headless administration are the supported paths.
- **Make listener exposure explicit.** Add a configurable bind address and CORS origin allow-list for the separate API. New installations default to loopback/no CORS; legacy settings without an explicit bind marker retain all-interface behavior until the owner acknowledges or changes it, avoiding silent migration.
- **Use targeted throttling rather than a global router limiter.** The authenticated API gets per-source failed-auth and control-route limits; the main UI/WebSocket traffic is untouched. Axum 0.8.9, Tower 0.5.3, and Tower HTTP 0.7.1 are the current locked versions; `tower-governor` is a compatible option for peer-IP `429` handling, while application state still owns command-specific limits.
- **Return a safe snapshot projection by default.** The authenticated `/api/snapshot` contract becomes an explicit DTO allow-list; internal `get_snapshot()` remains unchanged for the main server.
- **Preserve response compatibility while adding command state.** Continue returning a successful acceptance response where possible, but add `command_id` and an explicit initial state. Add a separate authenticated command-status endpoint rather than pretending HTTP `200` means inverter confirmation.
- **Use one durable command/idempotency ledger.** Reserve the idempotency key and command record before enqueueing, enforce a uniqueness constraint, and couple replay/conflict handling to the existing active-action compare-and-swap. Same key plus same request returns the original result; same key with a different request is rejected; semantically duplicate starts from different keys do not replace restore state.
- **Separate start permission from recovery stop semantics.** The global control toggle gates new external starts. An authenticated stop may unwind an already-owned external action even after start permission is revoked, with idempotent command status and readback verification; a read-only credential must not gain arbitrary control over unrelated UI actions. This is safer than leaving an accepted action running with no remote recovery path.

---

## Open Questions

### Resolved During Planning

- **Which API is in scope?** Only the separate authenticated listener plus the narrow local protection needed to prevent tokenless remote settings writes from changing that listener's security configuration; the main server's general bind, CORS, and dashboard behavior remain unchanged.
- **Should the work be comprehensive?** Yes, it is divided into compatibility-aware phases so foundational security changes land before command-contract changes.
- **Should HEM add in-process TLS?** No; the plan hardens bind/proxy boundaries and documents trusted TLS termination instead.
- **Should the full internal snapshot remain public to authenticated clients?** No; use an allow-listed external projection.

### Deferred to Implementation

- Exact cross-platform filesystem permission calls for Windows, macOS, and Unix, validated against the project's supported packaging targets.
- The final verifier algorithm parameters for generated tokens and the bounded compatibility window for legacy weak keys; new credentials must be generated high-entropy tokens rather than unconstrained user text.
- The exact UI for local desktop administration versus loopback/headless administration, while the security rule remains fixed: remote tokenless clients cannot change API security fields.
- The exact audit retention limit is implementation-owned but must be concrete before U3 is complete; use one bounded SQLite-backed audit/command store unless repository constraints prove otherwise.
- The exact recovery-stop authorization and restart reconciliation details must be finalized before U5; the plan requires a path that cannot strand an accepted action when start permission is revoked.

---

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

```mermaid
flowchart LR
    C[External client] --> P[Trusted HTTPS proxy or VPN]
    P --> L[Authenticated API listener]
    L --> M[CORS / body limit / auth / rate limits / audit]
    M --> R[Separate authenticated router]
    R --> S[Safe snapshot + command status]
    R --> I[Idempotency registry]
    I --> Q[Existing Quick Action handlers]
    Q --> W[Pending Modbus write queue]
    W --> B[Poll loop and inverter readback]
    B --> I
    B --> S
```

The listener manager owns bind/rebind/shutdown signals and reports startup failures. Authentication resolves a token identifier from the verifier without exposing the token. Mutating routes pass through idempotency and command registration before delegating to the existing handlers. The poll loop or status evaluator advances command records only when dispatch/readback evidence exists; uncertain connectivity produces `unknown`, not false confirmation.

---

## Implementation Units

- [ ] U1. **Secure API credential lifecycle and settings migration**

**Goal:** Replace plaintext API-key persistence with a generated high-entropy credential/verifier model, protect the security-sensitive settings control plane, and provide explicit rotation/revocation without losing existing installations.

**Requirements:** R2, R8, R9

**Dependencies:** None

**Files:**

- Modify: `src-tauri/src/settings/mod.rs`
- Modify: `src-tauri/src/server/api.rs`
- Modify: `src-tauri/src/server/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src/pages/SettingsPage.tsx`
- Modify: `src/lib/types.ts`
- Test: `src-tauri/src/settings/mod.rs`
- Test: `src-tauri/src/server/api.rs`
- Test: `tests/pages/settingsPageHandlers.test.tsx`

**Approach:**

- Add versioned authenticated-API credential fields while retaining a bounded migration read path for legacy plaintext `api_key` settings. Define verifier-over-legacy precedence, the explicit migration/rotation trigger, and behavior for weak legacy keys that cannot meet the new policy.
- Generate credentials using a CSPRNG and store only a concrete verifier, a non-secret last-four/fingerprint value, and configuration metadata. Prefer generated tokens over accepting arbitrary user text.
- Make the UI's primary action “generate/rotate and copy once”; do not echo the stored secret from `GET /api/settings`.
- Implement deterministic HEM-owned artifact cleanup for `settings.json`, `.bak`, `.tmp`, and `.corrupt` during migration, with explicit failure handling and no claim of secure deletion of arbitrary user backups.
- Set restrictive permissions on the settings directory, settings file, temporary file, and backup across supported platforms.
- Remove API key, API port, control permission, bind, and CORS mutations from the tokenless remote settings path. Route desktop changes through local Tauri/UI administration and headless changes through a loopback-only/local mechanism; reject sensitive-field writes from non-local `ConnectInfo` without changing ordinary dashboard settings behavior.
- Keep settings updates atomic through the existing `Settings::update`/`try_update` patterns; failed validation or persistence must not partially change key, port, bind, CORS, or control settings.

**Execution note:** Start with failing persistence and API contract tests for legacy migration, rejected weak keys, rotation, and secret redaction.

**Patterns to follow:** Existing metadata-only `get_settings()` responses, `settings_log_fields()` redaction tests, atomic settings save/backup tests, and isolated config-directory helpers.

**Test scenarios:**

- **Happy path:** Generate a credential, save it, authenticate with the displayed value, and confirm `GET /api/settings` exposes only metadata.

- **Migration:** The first successful request using a legacy plaintext key atomically migrates it to the verifier, invalidates the plaintext representation, and refuses API service if cleanup/persistence cannot commit.
- **Edge case:** Load a legacy plaintext-key settings file, authenticate with the legacy value during the compatibility window, rotate it, and confirm verifier precedence, old-key rejection, and removal of the old value from HEM-owned settings artifacts.
- **Error path:** Reject empty, too-short, placeholder, whitespace-containing, or malformed new credentials without changing the previously committed settings; define the response for a weak legacy key rather than treating it as a new credential.
- **Error path:** Inject a save or artifact-cleanup failure while rotating key/port/control fields and confirm the transaction leaves a clear old-or-new state without ambiguous mixed credentials.
- **Security boundary:** A remote `POST /api/settings` cannot change API security fields, while the local desktop/headless administration path can; ordinary non-security settings retain their existing compatibility behavior.
- **Concurrency:** Concurrent unrelated settings updates preserve both fields and never expose a plaintext secret in logs or serialized API responses.
- **Integration:** The UI shows a new generated key only during the rotation flow and warns that losing it requires another rotation.

**Verification:** Existing installations have an explicit, bounded migration path; new generated credentials authenticate; old credentials are invalidated at rotation; no credential material appears in settings responses, logs, HEM-owned backups, or support-facing serialization; remote tokenless settings calls cannot alter the authenticated API security posture; failed saves leave configuration unchanged.

---

- [ ] U2. **Authenticated listener exposure and lifecycle manager**

**Goal:** Make the separate listener's bind address and CORS policy explicit, safely configurable, and correctly started, stopped, and rebound when API settings change.

**Requirements:** R1, R3, R8, R9

**Dependencies:** U1

**Files:**

- Modify: `src-tauri/src/settings/mod.rs`
- Modify: `src-tauri/src/server/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/server/api.rs`
- Modify: `src/pages/SettingsPage.tsx`
- Modify: `src/lib/types.ts`
- Create: `src-tauri/src/server/authenticated_lifecycle.rs`
- Test: `src-tauri/src/server/mod.rs`
- Test: `src-tauri/src/server/authenticated_lifecycle.rs`
- Test: `src-tauri/tests/authenticated_api.rs`
- Test: `tests/pages/settingsPageHandlers.test.tsx`

**Approach:**

- Add persisted bind-address and allowed-origin configuration for the authenticated API only. Use an explicit migration marker/optional field so legacy settings without a bind value remain distinguishable from an explicit loopback choice; new installations use loopback and no CORS.
- Introduce an owned lifecycle manager in `AppState`/startup orchestration with cancellation, retained task ownership, and join coordination in both Tauri and headless paths. Credential rotation, key clear, port/bind/CORS changes, and disable must all reach this manager.
- Use a candidate-bind/swap transaction: validate and pre-bind the candidate listener before committing the new settings, or roll back the persisted settings if binding fails. Never leave `GET /api/settings`, runtime listener state, and persisted configuration describing different active endpoints.
- Use graceful shutdown for the authenticated listener and a bounded drain period. Explicitly coordinate Tauri exit and headless shutdown; do not broaden this unit into changing the main server's compatibility behavior.
- Surface bind/rebind failures to application state/UI and logs rather than silently leaving a stale listener running. Reuse the existing bound-channel pattern from `start_server_with_frontend_on_port()`.
- Build the authenticated listener with `into_make_service_with_connect_info::<SocketAddr>()` so source-based limits/audit records have a real peer address. Accept forwarded identity only from explicitly configured trusted proxy peers; reject or ignore forwarded headers from direct clients.
- Apply CORS only to the authenticated router, with no headers by default and exact configured origins when browser integrations are deliberately enabled. Keep settings and WebSocket routes absent.

**Patterns to follow:** `start_server_with_frontend_on_port()` bind reporting, existing settings restart messaging, authenticated router separation, and real process cleanup in `src-tauri/tests/headless_smoke.rs`.

**Test scenarios:**

- **Happy path:** New default settings bind only to loopback; an explicitly configured interface binds as requested and the exact origin receives the configured CORS headers.
- **Edge case:** Clear the key, set port 0, change the port, and change the bind address; each operation closes the prior listener and leaves no stale port open.
- **Error path:** Rebind to an occupied port reports failure and rolls back the candidate settings, leaving the previous healthy listener and persisted configuration authoritative; no ambiguous “new settings but old listener” state is allowed.
- **Error path:** Unlisted origins, malformed origins, and requests without an `Origin` header receive the documented behavior without wildcard reflection.
- **Integration:** A real ephemeral TCP client can observe startup, authenticated request handling, graceful shutdown, and port replacement; teardown always releases the socket.
- **Regression:** Main-router bind/CORS behavior and all existing main API routes remain unchanged.

**Verification:** Listener state matches persisted configuration after save/reload; no stale listener survives disable/rebind or credential rotation; bind failures roll back deterministically; direct non-loopback exposure is explicit; peer/trusted-proxy identity is fail-closed; the main server tests remain green and unchanged in behavior.

---

- [ ] U3. **Authentication, request limits, and audit boundary**

**Goal:** Harden the authenticated router against token comparison leaks, brute-force attempts, oversized input, and unaudited security/control events.

**Requirements:** R2, R4, R8

**Dependencies:** U1, U2

**Files:**

- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/src/server/mod.rs`
- Modify: `src-tauri/src/server/external_control.rs`
- Modify: `src-tauri/src/history/mod.rs`
- Modify: `src-tauri/src/inverter/poll.rs`
- Test: `src-tauri/src/server/mod.rs`
- Test: `src-tauri/src/server/external_control.rs`
- Test: `src-tauri/tests/authenticated_api.rs`

**Approach:**

- Compare credential verifiers in constant time and normalize authentication failures to a bounded, non-secret response.
- Add per-source failed-auth throttling and targeted control-route throttling with concrete bounded defaults: 10 failed credentials per source per minute, 120 reads per source per minute, 2 starts per credential identity per minute, and 10 owned-action stops per identity per minute. Return `429` with `Retry-After`, cap limiter cardinality at a fixed bounded size with idle eviction, and do not apply the limiter to the main router or normal dashboard polling. Keep separate budgets so stop/recovery is not starved by reads.
- Add a small route-specific JSON body limit for authenticated control requests, preserving strict `deny_unknown_fields` validation, and apply a bounded request timeout.
- Use constant-time verifier comparison and ensure malformed/duplicate/query-string credentials produce equivalent non-secret responses.
- Add a bounded SQLite-backed audit event store with explicit retention/count cleanup: retain at most 30 days and 10,000 rows, deleting oldest records deterministically. The audit primitive records authentication successes/failures, authorization failures, key/configuration changes, rate-limit events, starts/stops, listener transitions, and command outcome changes. Store only token ID/fingerprint, trusted source metadata, sanitized duration, route, result, and correlation ID.
- Ensure client address extraction uses actual `ConnectInfo<SocketAddr>` or explicitly configured trusted proxy peers; reject/ignore forwarded headers from untrusted direct clients and test spoofing.
- Keep audit output out of both API routers and define failure behavior: a failed audit write for a control mutation fails closed before enqueueing; read/authentication responses remain available but emit a bounded local warning and never claim that the event was durably audited.

**Patterns to follow:** Existing redacted settings logging, `into_make_service_with_connect_info` on the main server, authenticated middleware ordering, isolated SQLite/history tests, and `tower-http`/Tower version constraints documented by the research. Use `tower-governor` only if its bounded peer-IP behavior and middleware ordering fit the router; otherwise keep the limiter as a small application-owned component.

**Test scenarios:**

- **Happy path:** Valid credentials pass constant-time verification and produce a redacted audit event with a stable token identifier.
- **Error path:** Missing, malformed, duplicate, query-string, and wrong credentials return the same non-secret authentication response and increment the source limiter.
- **Edge case:** The configured failed-auth threshold returns `429` with `Retry-After`, then recovers after the window; legitimate authenticated reads remain available.
- **Edge case:** Limiter state remains bounded when many source addresses or malformed forwarded headers are presented; direct clients cannot spoof proxy identity.
- **Error path:** Oversized, slow, or unknown-body control requests are rejected before action state changes.
- **Integration:** Audit records include method, route, source, result, correlation ID, and sanitized duration but never the Authorization header, token, full snapshot, or arbitrary request body.
- **Failure path:** Audit-store write failure is surfaced according to the chosen fail-open/fail-closed policy and never silently changes the HTTP command outcome.
- **Regression:** Permission-denied writes remain `403`, route/method restrictions remain intact, and no audit endpoint is exposed through the authenticated router.

**Verification:** Brute-force attempts are bounded, limiter memory is bounded, control input is size/time-limited, trusted source identity is enforceable, every security-sensitive event is observable without secret leakage, audit retention is deterministic, and normal dashboard traffic is unaffected.

---

- [ ] U4. **Least-data external snapshot contract**

**Goal:** Stop exposing the complete internal inverter snapshot through the external API while providing the fields integrations actually need.

**Requirements:** R5, R8

**Dependencies:** U2

**Files:**

- Create: `src-tauri/src/server/external_snapshot.rs`
- Modify: `src-tauri/src/server/mod.rs`
- Modify: `src-tauri/src/server/api.rs`
- Modify: `REMOTE_CONTROL.md`
- Modify: `README.md`
- Test: `src-tauri/src/server/external_snapshot.rs`
- Test: `src-tauri/src/server/mod.rs`
- Test: `src-tauri/tests/authenticated_api.rs`

**Approach:**

- Define an explicit allow-list DTO for `/api/snapshot`, limited to integration-safe operating measurements and connection/freshness fields. Defer URL/version negotiation until a second incompatible contract is actually needed.
- Keep the main `get_snapshot()` serializer unchanged so the dashboard and existing internal consumers retain their current contract.
- Omit serials, firmware details, detailed battery-module/BMS data, meter identifiers, and other fields not required by the documented remote-control workflows.
- Add an allow-list regression test that fails when a future internal snapshot field is accidentally exposed.
- Update examples and field documentation to distinguish the safe external snapshot from the internal full snapshot.

**Patterns to follow:** `control_status::build_status()` as a deliberately shaped external response and the existing authenticated router route allow-list.

**Test scenarios:**

- **Happy path:** Authenticated `/api/snapshot` returns the documented DTO with expected measurements and freshness fields, classified as safe integration data and marked `Cache-Control: no-store`.
- **Edge case:** Empty, stale, disconnected, unknown-device, and future-dated snapshots preserve safe null/unavailable semantics.
- **Regression:** Internal `InverterSnapshot` fields added or populated in fixtures do not appear unless explicitly added to the external DTO.
- **Integration:** The authenticated router exposes only the safe projection while the main router continues returning its existing full snapshot; newly added internal fields and cache headers cannot expand the external exposure accidentally.

**Verification:** External consumers receive enough documented data for status/control verification but cannot use this API to retrieve the full internal household/inverter record.

---

- [ ] U5. **Durable command identity and readback state**

**Goal:** Give external callers a reliable way to track a command from acceptance through inverter readback without misrepresenting queued work as complete.

**Requirements:** R6, R8, R10

**Dependencies:** U3

**Files:**

- Create: `src-tauri/src/server/external_commands.rs`
- Modify: `src-tauri/src/server/mod.rs`
- Modify: `src-tauri/src/server/external_control.rs`
- Modify: `src-tauri/src/server/control_status.rs`
- Modify: `src-tauri/src/history/mod.rs`
- Modify: `src-tauri/src/inverter/poll.rs`
- Test: `src-tauri/src/server/external_commands.rs`
- Test: `src-tauri/src/server/external_control.rs`
- Test: `src-tauri/src/server/control_status.rs`
- Test: `src-tauri/src/inverter/poll.rs`
- Test: `src-tauri/tests/authenticated_api.rs`

**Approach:**

- Create one durable SQLite-backed command/idempotency ledger with unique `(credential identity, endpoint, idempotency key)` ownership, request fingerprint, command ID, action kind, expiry, response, and lifecycle state. U6 consumes this ledger rather than creating a second persistence model.
- Reserve the ledger row transactionally before enqueueing a write; only the reservation owner may enqueue. A crash after reservation but before dispatch must never cause an automatic duplicate enqueue on retry.
- Enumerate all four mutation routes explicitly: `force-charge`, `force-charge/stop`, `force-discharge`, and `force-discharge/stop`. Each gets command registration, idempotency/replay behavior, state tracking, and route-by-route tests.
- Add an authenticated `GET` command-status route returning explicit states: `accepted`, `queued`, `dispatched`, `readback_confirmed`, `failed`, `expired`, and `unknown`.
- Persist action ownership, deadline, write phase, and the serializable pre-action recovery baseline before enqueueing. On restart, never re-arm automatically; after a fresh read, either issue and verify a recovery stop/revert using the persisted baseline or mark the action `unknown` and expose a local/operator recovery warning when no safe baseline exists.
- Connect command records to the existing Quick Action restore state and poll-loop write/readback flow without duplicating register encoding. Use a monotonic poll sequence/connection epoch and dispatch watermark so readback is causally newer than the command, not merely newer than process start.
- Define state transitions precisely: `expired` requires a passed deadline plus fresh readback confirming the action is no longer active; deadline expiry without confirming evidence is `unknown`; write/validation failures are `failed`.
- Enforce an active-action compare-and-swap independent of idempotency keys: semantically duplicate starts return the existing command, conflicting starts return `409`, and stops target the active command/recovery lease explicitly.
- Permit an authenticated stop for an already-owned external action after start permission is revoked, while preventing that path from stopping unrelated local/UI actions. Stops remain idempotent and require readback confirmation.

**Patterns to follow:** `control_status` injected-time state construction, `AppState` mutex ownership, pending-write queue behavior, and existing Quick Action mutual-exclusion tests.

**Test scenarios:**

- **Happy path:** Each of the four mutation routes returns one command ID, transitions through accepted/queued to readback-confirmed, and remains queryable.
- **Happy path:** Repeated stop for the same owned action is idempotent; a revoked start permission still permits recovery of that owned action.
- **Error path:** Opposite-direction conflict, missing snapshot, write failure, stale readback, and inverter disconnect produce explicit failed/unknown states rather than false success.
- **Edge case:** HEM crashes before enqueue, after enqueue, and after dispatch; restart never blindly re-enqueues, and fresh-read recovery either safely stops/reverts or reports unknown with operator visibility.
- **Edge case:** Deadline passes with and without confirming readback; only the former becomes expired.
- **Concurrency:** Concurrent UI and external starts remain mutually exclusive; semantically duplicate starts with different keys cannot replace restore state; only the ledger reservation owner advances the queue.
- **Integration:** Poll-loop dispatch and subsequent snapshot/status evaluation update the same command record using a causal poll/connection watermark and preserve existing Quick Action behavior.

**Verification:** Integrations can poll a command ID and make a safe decision without parsing human summaries or assuming that HTTP acceptance equals inverter application; restart, expiry, revocation, duplicate, and recovery paths cannot silently strand or duplicate a force action.

---

- [ ] U6. **External mutation replay contract and operational documentation**

**Goal:** Expose the durable ledger's replay/conflict behavior through all external mutation routes and align public guidance with the hardened contract.

**Requirements:** R6, R7, R8, R10

**Dependencies:** U5

**Files:**

- Modify: `src-tauri/src/server/external_control.rs`
- Modify: `src-tauri/src/server/mod.rs`
- Modify: `src-tauri/src/server/external_commands.rs`
- Modify: `REMOTE_CONTROL.md`
- Modify: `README.md`
- Test: `src-tauri/src/server/external_control.rs`
- Test: `src-tauri/src/server/external_commands.rs`
- Test: `src-tauri/tests/authenticated_api.rs`

**Approach:**

- Require an `Idempotency-Key` on all four named mutation routes, with a documented UUID/length policy and a canonical fingerprint covering method, route, and normalized JSON body.
- Use the U5 ledger's unique reservation and replay result. A repeated key with the same endpoint and payload replays the original response without queuing another write; a mismatched payload returns `409`; a concurrent reservation returns the existing command state.
- Ensure semantically duplicate starts with different keys are handled by U5's active-action compare-and-swap rather than relying on idempotency keys alone.
- Keep ledger retention longer than the 1439-minute maximum action and normal retry window, with bounded cleanup and restart persistence.
- Update curl, JavaScript, and Python examples to send keys, poll command status, and distinguish safe same-key retries from unsafe new-key retries. Do not recommend automatic retries without reusing the identical key and payload.
- Document the protected local configuration path, safe bind/CORS defaults, proxy/VPN boundary, generated-key rotation, `429` behavior, safe snapshot fields, command states, idempotency rules, recovery-stop semantics, and the unchanged physical-emergency limitation.

**Execution note:** Add characterization tests around current external responses before changing the mutation contract, then extend them for replay/conflict behavior.

**Patterns to follow:** Existing external-control integration tests, SQLite/history migration patterns, and the guide's current bounded verification sequence.

**Test scenarios:**

- **Happy path:** Each of the four mutation routes with a new key reserves exactly one command and returns its command ID; replaying the same request returns the same response and does not enqueue another write.
- **Error path:** Reusing a key with a different duration, endpoint, or start/stop action returns `409` and leaves command state unchanged.
- **Concurrency:** Two simultaneous identical requests produce one durable reservation and equivalent responses even across a transaction boundary; a simultaneous mismatched request is rejected.
- **Edge case:** A process crash after reservation but before queue enqueue leaves a replayable `unknown`/in-progress record and never allows a retry to enqueue blindly; records survive restart.
- **Edge case:** Expired ledger records are cleaned up only after the documented retention period, which exceeds the maximum action/retry window.
- **Error path:** Missing/invalid idempotency keys, rate-limited clients, and timed-out network clients receive actionable errors without claiming inverter completion; documentation says only identical same-key retries are safe.
- **Integration:** All three client examples use the new contract and the documentation accurately explains when a command is accepted, confirmed, failed, unknown, or recoverable after start-permission revocation.

**Verification:** A lost response can be retried safely only with the same key and identical payload, duplicate starts never reset a live action even with different keys, recovery stops remain available for owned actions after start revocation, and public documentation matches the implemented API behavior.

---

## System-Wide Impact

- **Interaction graph:** Local desktop/headless administration → protected settings update → persisted credential/network configuration → authenticated listener manager → middleware → external router → durable command/idempotency ledger → existing Quick Action handlers → pending-write queue → poll loop/readback → command/status responses.
- **Error propagation:** Validation and policy failures remain structured `4xx` responses; remote tokenless writes to security fields are rejected without changing ordinary dashboard settings; bind/rebind failures roll back candidate configuration and update UI/log state; write/readback uncertainty becomes command `unknown`; persistence failures preserve the prior committed configuration.
- **State lifecycle risks:** Listener replacement, credential rotation, idempotency reservations, command records, queue enqueue, restart recovery, and volatile Quick Action restore state must not race. The existing `Settings::update` transaction and `force_action_lock` remain authoritative boundaries, with the durable ledger reservation preceding queue ownership.
- **API surface parity:** Update `REMOTE_CONTROL.md`, `README.md`, Settings UI copy, TypeScript response types, curl/JavaScript/Python examples, and any integration fixtures together.
- **Integration coverage:** Router tests alone cannot prove listener rebinding, socket release, proxy-boundary behavior, or restart persistence. Add real ephemeral TCP lifecycle tests and a bounded headless-process scenario.
- **Unchanged invariants:** Main server bind/CORS/routes, inverter register encoders, strict duration validation, default-off external writes, mutual exclusion, and status freshness semantics remain intact unless explicitly extended by the command-state unit.

---

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Tokenless main settings writes can change the authenticated API security posture | Keep the main server generally permissive, but reject API security-field mutations from non-local peers and provide local desktop/headless administration; test both paths. |
| Credential migration locks out existing integrations or leaves old secrets in backups | Define a bounded legacy exception, verifier precedence, migration completion marker, and deterministic cleanup/rollback for HEM-owned backup/temp/corrupt artifacts. |
| Changing bind/CORS defaults breaks remote clients | Preserve explicit legacy all-interface mode during migration, surface the new setting clearly, and update the guide with LAN/VPN/proxy examples. |
| Listener reconfiguration races with in-flight requests or process shutdown | Use one owned lifecycle manager, candidate-bind/swap rollback, cancellation/join coordination, bounded graceful drain, retained `ConnectInfo`, and real-socket tests. |
| Rate limiting blocks legitimate clients behind NAT/VPN or trusts spoofed proxy headers | Apply concrete bounded limits only to the separate API, use actual peer identity unless an explicitly configured trusted proxy matches, separate reads/stops/starts, and test cardinality/forwarded-header behavior. |
| Snapshot minimization breaks undocumented consumers | Publish the new allow-list, add contract fixtures, and provide a deliberate migration path rather than silently exposing the old full serializer. |
| Command state claims confirmation without reliable inverter evidence | Advance state only from queue/poll/readback evidence; use `unknown` for disconnects and preserve existing safety conditions. |
| Idempotency persistence grows without bound or interacts badly with restore state | Use one ledger with a unique reservation transaction before queue enqueue, active-action compare-and-swap, bounded retention, canonical fingerprints, and crash/restart/concurrency tests. |
| Security logging leaks household data or credentials | Centralize redaction, never log Authorization/request bodies, use token IDs, and add regression tests against serialized logs/support output. |

---

## Documentation / Operational Notes

- Update `REMOTE_CONTROL.md` as each phase changes the contract; document that security fields are configured locally, that rotation/rebind is transactional, and remove statements that restart is always required once listener lifecycle reconfiguration is implemented.
- Add a short rotation/revocation runbook covering generated-key copy, client update, old-key invalidation, backup protection, and recovery after a lost key.
- Document the recommended deployment topology: HTTPS client → trusted proxy/VPN → loopback HEM listener. State that direct HTTP/LAN exposure remains an explicit compatibility mode, not an encrypted channel.
- Document CORS as disabled by default and explain exact-origin configuration for browser integrations.
- Document `429`, `Retry-After`, required idempotency keys, command polling, and the distinction between accepted, confirmed, failed, and unknown.
- Keep the emergency warning prominent: this is not an emergency-stop or safety-certified interface; use inverter/manufacturer physical controls for emergencies.
- Re-check packaged desktop, headless, Docker, IPv4, and IPv6 startup paths before release.

---

## Phased Delivery

### Phase 1 — Credential and exposure foundations

Deliver U1 and U2. This removes plaintext credential persistence for newly generated keys, makes exposure explicit, and ensures settings changes cannot leave stale listeners behind.

### Phase 2 — Boundary hardening and least data

Deliver U3 and U4 in parallel after U2. This constrains abuse and data exposure while preserving the existing Quick Action behavior and main server contract.

### Phase 3 — Reliable control semantics

Deliver U5 and U6. U5 establishes the single durable command/idempotency ledger and recovery semantics; U6 exposes the replay contract and updates clients/documentation.

---

## Success Metrics

- New credentials are generated with high entropy, are not persisted or logged in plaintext, and can be rotated without restarting solely to revoke the old credential.
- New installations do not expose the authenticated listener beyond loopback unless the owner explicitly configures another bind address.
- Wildcard CORS is absent from the authenticated API by default; configured browser origins are exact matches.
- Repeated invalid authentication attempts receive bounded responses and do not affect the main UI/API.
- Authenticated snapshot responses contain only documented allow-listed fields.
- Every external mutation has a command ID, and status never reports inverter confirmation without corresponding readback evidence.
- Retrying a timed-out mutation with the same idempotency key never queues a duplicate command.
- Security/control events are diagnosable without secrets or unnecessary household data.
- The full project verification suite remains clean, including isolated settings tests and real listener/process lifecycle coverage.

---

## Sources & References

- `REMOTE_CONTROL.md`
- `src-tauri/src/server/mod.rs`
- `src-tauri/src/server/external_control.rs`
- `src-tauri/src/server/control_status.rs`
- `src-tauri/src/server/api.rs`
- `src-tauri/src/settings/mod.rs`
- `src-tauri/src/inverter/poll.rs`
- `src-tauri/src/lib.rs`
- `src-tauri/tests/e2e_mock.rs`
- `src-tauri/tests/headless_smoke.rs`
- `src/pages/SettingsPage.tsx`
- `tests/pages/settingsPageHandlers.test.tsx`
- [RFC 6750 — OAuth 2.0 Bearer Token Usage](https://www.rfc-editor.org/rfc/rfc6750.html)
- [RFC 6585 — Additional HTTP Status Codes](https://www.rfc-editor.org/rfc/rfc6585.html)
- [OWASP REST Security Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/REST_Security_Cheat_Sheet.html)
- [OWASP Logging Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Logging_Cheat_Sheet.html)
- [Axum 0.8.9 middleware](https://docs.rs/axum/0.8.9/axum/middleware/)
- [Tower HTTP 0.7.1 limits](https://docs.rs/tower-http/0.7.1/tower_http/limit/)
