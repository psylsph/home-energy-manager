---
title: "feat: One-off Timed Export and Octopus Saving Sessions"
type: feat
status: active
date: 2026-10-17
---

<!-- markdownlint-disable MD025 -->

# One-off Timed Export and Octopus Saving Sessions

## Background

Issue #366: a user sets a one-off evening export in the GivEnergy portal while HEM runs Cosy. HEM does not know the portal export is deliberate. It reads the slot back from the inverter, Cosy's slot exit then clears Eco/`enable_discharge`, and the export is cancelled (or Cosy stands down). The user wants one-off exports without leaving HEM.

HEM's Timed Export slots are daily `HH:MM` windows with no date, so they cannot express a one-off. When HEM owns the export (an enabled schedule that is in its window), Cosy already defers its Eco/discharge writes and will not start a charge slot inside the window.

## Goal

1. **Export Once**: let the user book a dated export window on the Control page that is applied to a slot they pick, then reverts that slot to its previous setting automatically.
2. **Saving Sessions**: read the user's joined Octopus Saving Sessions, alert about them, and optionally create the same one-off exports programmatically.
3. **Auto-join** (last): optionally join newly offered sessions.

## Design Principles

- One backend path. Octopus automation calls the same function the Control page uses, so a session export is indistinguishable from a manual one.
- Reuse the existing Timed Export handlers (slot save, stop), their rollback/compensation logic, and the `timed_export_action_lock`. No second export mechanism.
- Never apply the override at booking time. Slots repeat daily, so applying a future event early would export today.
- The revert must survive a restart.
- The user's own edit wins. If they save the overridden slot during the event, the revert is dropped.
- The Octopus Saving Sessions API is unofficial. Billing features must never depend on it.
- Automatic account writes (joining sessions) are opt-in, separate from export, and last.

## Phase 1: Export Once (manual)

Implemented. The apply/revert executor runs as its own background task, not inside the poll loop, because the slot-save and stop handlers wait for the poll loop to drain their writes.

### Data model

`Settings::export_once_events: Vec<ExportOnceEvent>` (`#[serde(default)]`):

| Field | Meaning |
|---|---|
| `id` | Unique id (UUID-like string) |
| `slot` | 1-based discharge slot the override uses |
| `start`, `end` | `YYYY-MM-DD HH:MM` on the inverter's clock |
| `target_soc` | Discharge floor written with the slot (4-99) |
| `source` | `manual` now; `saving_session:<octopus id>` later |
| `state` | `pending`, `active`, `done`, `cancelled`, `failed` |
| `original` | Captured when applied: the slot's previous content and whether the schedule was enabled |

### Lifecycle

A poll-loop tick (`inverter/export_once.rs`) takes the inverter clock and evaluates a pure planner:

- A `pending` event whose start is within the apply lead (10 minutes) and whose end is in the future becomes `active`: capture the slot's current desired content and the schedule flag, persist them, then save the event window into the slot through the Timed Export slot-save path.
- An `active` event past its end is reverted: restore the captured slot content, and stop the schedule if it was off before the override. The event becomes `done`.
- A `pending` event whose end has passed without being applied becomes `failed` with a reason (HEM was off or the inverter was unreachable).
- Cancelling a `pending` event just marks it cancelled. Cancelling an `active` event reverts immediately.
- If the user saves the overridden slot while the event is active, the event is dropped with no revert.
- Startup: any `active` event is reconciled by the same tick, so a crash mid-override still reverts.

Apply and revert each run under one hold of `timed_export_action_lock`, using the locked inner forms of the slot-save and stop handlers, so the poll loop's Timed Export machine never observes a half-applied state.

### Validation

- The slot must exist on the detected model; a model with one eligible slot needs no picker.
- End after start, same calendar day (cross-midnight is rejected in this phase), event must end in the future, start within 14 days.
- No overlap with another non-finished event using the same slot.
- The slot picker shows what each slot holds now ("Slot 2 (now 22:00-23:00)"), since that is what comes back afterwards. A booking that overlaps the slot's own daily window is allowed: replacing it for the event is often the point.

### API

- `GET /api/export-once` - events plus the inverter-local "now".
- `POST /api/export-once` - create (`slot`, `date`, `start`, `end`, `target_soc`).
- `DELETE /api/export-once/{id}` - cancel (reverts if active).

### UI

A card under Timed Export on the Control page: slot picker (using the page's existing slot labels), date, start, end, target SOC, a list of upcoming/active events with Cancel, and a line on the slot while an override is active ("Temporary export until 19:00, then reverts to ...").

### Cosy

An active override is an ordinary enabled Timed Export slot inside its window, so Cosy defers exactly as it does for a manual export. Tests cover a Cosy slot running when the event starts and an event ending inside a Cosy window.

## Phase 2: Saving Sessions reader and alerts

Implemented as `octopus_sessions.rs` with a panel at the top of the Octopus page. The Octopus integration has no on/off switch of its own: saving the account number and API key switches it on (and the meter import with it), and clearing the account number switches it off. A separate "My Home Has Gas" setting (on by default) stops gas being fetched and hides it from the page and the CSV/PDF exports for electricity-only homes. The mode is Off or Alert only for now; Activate export arrives with phase 3. Alerts go through `alerts::send_text_notification`, are deduplicated by Octopus event id (persisted, so a restart never repeats one), are not marked as sent when delivery was skipped or failed, and a session first seen inside the 30 minute window gets a single alert.

New `octopus_sessions.rs`. Exchange the existing Octopus API key for a short-lived token, then query joined Saving Sessions from the Octopus backend GraphQL endpoint. Poll every 10-15 minutes. Only joined events count. The endpoint is unofficial: failures surface as "Saving Sessions unavailable" on the Octopus page and never affect billing features. A base-URL override keeps tests hermetic.

Octopus page settings (top section): mode `Off` / `Alert only` / `Activate export`, export slot, SOC floor. Alerts dedupe by Octopus event id (one on detection, one before start) and go through the existing Telegram/ntfy/Pushover delivery. The UI warns when no alert channel is configured.

Verified against a real account on 2026-10-17 (read-only):

- Token: `mutation { obtainKrakenToken(input:{APIKey:"..."}) { token } }` on `https://api.octopus.energy/v1/graphql/`. The token is sent as `Authorization: <token>` (no prefix) to `https://api.backend.octopus.energy/v1/graphql/`.
- Joined sessions: `savingSessions { account(accountNumber:"A-...") { canJoinCampaign hasJoinedCampaign joinedEvents { eventId startAt endAt } } }`. It returns the account's whole history (58 entries on the test account), so filter to future ones.
- Offered events: `savingSessions { eventCount events { id startAt endAt status eventType rewardPerKwhInOctoPoints capacityStatus } }`. It returns the whole history too (109 entries); `status` is `UPCOMING` or `DONE`. `code` (needed to join) and `targetRegion` (needs a sub-selection) also exist.
- `joinedEvents` carries no `eventType`. Cross-reference `events` by id.
- `eventType` values seen: `TURN_DOWN` (the Saving Session / Power Down event), `WEEKEND_HAPPY_HOUR` (cheap import, reward 0) and `TURN_UP`. **Only `TURN_DOWN` may trigger an export.** On the test account the only upcoming joined event was a Weekend Happy Hour, so a naive "export for every joined event" would have exported during a cheap-import window.
- All times are UTC with an explicit offset. Convert to the inverter's local time before booking. Sessions are not always evenings (a Sunday 10:00 UTC one exists).

## Phase 3: Automatic export

Implemented. The mode adds `Activate Export`, with a slot and a stop-at level chosen on the Octopus page. Each joined `TURN_DOWN` session is turned into a one-off export through the same booking function as the Control page (source `saving_session:<id>`, times converted from UTC to UK time). A session is booked once; skipping cancels its pending export and un-skipping books it again. A session Octopus stops listing cancels its pending export (a running one is left to finish and revert), a failed check changes nothing, and leaving the mode or changing the slot or level cancels pending exports so nothing fires with stale settings. The announcement alert says what HEM will do, and these exports also alert when they start, finish or cannot run. Sessions that cross UK midnight cannot be booked and are reported on the page.

In `Activate export` mode each joined session is turned into an `ExportOnceEvent` with source `saving_session:<id>` through the Phase 1 create function. Per-session skip. Upstream cancellation removes a pending event. Alerts for start, revert and failures.

## Phase 4: Auto-join

Implemented. A separate opt-in checkbox on the Octopus page (`octopus_sessions_auto_join`, off by default) that works in Alert only and Activate export modes. Each upcoming, unjoined `TURN_DOWN` event is joined with `joinSavingSessionsEvent`, at most five per check. Safeguards:

- The account must already report `hasJoinedCampaign`; HEM never enrols one.
- The id is saved to `octopus_sessions_join_attempted` once the token is in hand and just before the request is sent (a failed login costs nothing), and is never retried, so a crash or restart cannot fire the mutation twice. A failed join is alerted once and listed on the page.
- The reply to the mutation is not trusted. The joined list is read again (up to three reads, since Octopus can be slow to show a join), and only a session that appears there is announced as joined or booked for export. A session is credited to HEM only if Octopus took HEM's request.
- A session the user skipped is not joined. A refused or unconfirmed join stays listed on the page, including after a restart.

**Not yet verified against a real account.** The mutation name and its `possibleErrors` reply are taken from community clients, not captured from this account (joining is a write, unlike the Phase 2 reads). Because success is decided by the second read, a wrong shape fails soft: the session is reported as not joined and never retried. Check it once against a real offered session before relying on it.

## Test plan

- Planner: pure function with an injected clock; apply lead, revert, expired pending, cancel, overlap, cross-midnight rejection.
- Executor: apply and revert through the real handlers against a mock write queue, including a rejected slot write (no revert record left dangling) and a restart while active.
- Concurrency: the tick applying an override while the user saves a different slot, and the user saving the overridden slot during the event.
- Cosy overlap, both directions.
- API: create, validate, list, cancel.
- Frontend: form validation, event list, override notice.
- Phase 2+: mock Octopus server for success, schema change, auth failure, upstream cancellation, restart mid-join.
- Every test that touches settings uses a temporary `GIVENERGY_LOCAL_CONFIG_DIR`.

## Open questions

- Re-arm firmware (Gen3 variants that re-assert `enable_discharge` when any slot is non-zero) clears slots outside the window; the override must go through the same restore path. Covered by reusing the slot-save handler.
- Whether Weekend Happy Hour or other event types should be ignored explicitly (yes: only Saving Session / Power Down types).
- Octopus terms for automated joining (Phase 4). Not checked.
