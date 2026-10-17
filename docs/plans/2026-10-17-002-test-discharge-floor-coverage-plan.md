---
title: "test: Close coverage gaps on the Discharge Schedule Minimum SOC guard"
type: test
status: complete
date: 2026-10-17
---

<!-- markdownlint-disable MD025 -->

# Close Coverage Gaps On The Discharge Schedule Minimum SOC Guard

## Goal

Add the tests that are missing around the Discharge Schedule Minimum SOC guard (the "discharge floor", added in v0.74.0, developer mode only). The guard's core state machine is already well covered; this plan only covers what surrounds it. No production behaviour changes, except where a small extraction is needed to make the poll-loop wiring testable (Gap 1).

## What is already covered (not in scope)

- `check_discharge_floor` in `inverter/state_machines.rs`: raise, skip when already above, re-assert, restore, disable mid-window, restart inside and outside a window, midnight wrap, multiple slots, unconfigured slots, three-phase register, idle with nothing to write.
- `server/api.rs`: config round-trip and `floor_soc` validation, config not mutated when the save fails.
- `poll.rs`: `persist_discharge_floor_saved_reserve` concurrent disjoint save.
- `controlPageSaveBeforeLoad.test.tsx`: Save stays off until the config has loaded; saving posts to `/api/discharge-floor`.
- `controlPageAgileScope.test.tsx`: section hidden outside developer mode; save posts the expected payload.

## Gaps

1. The poll loop's call to `check_discharge_floor` (`poll.rs`, "Discharge floor guard" block) has no test. Nothing fails if the call is removed, runs while disconnected, or stops persisting the saved reserve.
2. The `DischargeFloorSection` load-failure message and save-error feedback are untested.
3. Developer-mode gating is only tested in the hidden direction; the shown direction is covered only incidentally by the save tests.
4. There is no HTTP-level test that a saved config reaches the live `discharge_floor_config` the poll loop reads.

`e2e/control.spec.ts` mentions a "discharge floor" but is about a discharge slot's target SOC, so it is not a gap here and needs no change.

## Approach

Work in TDD order per gap: write the failing test, confirm it fails for the right reason, then make it pass. For gaps 2–4 the production code already behaves correctly, so the RED step is proved by temporarily breaking the behaviour (for example removing the `developerMode &&` or the error branch), seeing the test fail, and reverting. No test is weakened to pass.

### Gap 1: poll-loop wiring

The call site is an inline block inside the very large poll loop, so a direct test may not be practical. Plan:

- First check whether an existing poll-loop test harness (mock-TCP, hermetic `AppState`) can drive one poll cycle with a configured floor and an active discharge window. If it can, test through it: assert the HR110 raise is queued, `discharge_floor_saved_reserve` is persisted to a temp config dir, and nothing is queued when the floor is disabled.
- If it cannot, extract the block into a small `apply_discharge_floor(state, snapshot, inverter_minute)` function with no behaviour change, make the loop call it, and test that function against `AppState`.
- Any test that touches settings uses a unique temp dir via `GIVENERGY_LOCAL_CONFIG_DIR` and restores previous env values on teardown. Nothing reads or writes `~/.givenergy-local`.
- Pin the clock by passing `inverter_minute` explicitly; no wall-clock reads.

Decision for Stuart: if extraction is needed, it touches the poll loop, which is high-traffic. I would keep the extraction minimal and verify with the full Rust suite. Say if you would rather skip Gap 1 than refactor.

### Gap 2: frontend failure paths

New cases in `tests/pages/` (alongside `controlPageSaveBeforeLoad.test.tsx`, reusing its API mock pattern):

- GET `/api/discharge-floor` returns not-ok or throws: the section shows its load-failed message and Save stays off.
- POST `/api/discharge-floor` rejects: the error feedback appears and clears after its timeout (fake timers).

### Gap 3: gating

In `controlPageAgileScope.test.tsx` or a new small file, add a developer-mode-on case: the "Discharge Schedule Minimum SOC" heading renders when `developerMode` is true. Keep the existing hidden case.

### Gap 4: API to live config

In `server/api.rs` tests, add a case that POSTs `/api/discharge-floor` and then asserts `state.discharge_floor_config` holds the new values, alongside the existing round-trip test. If the existing round-trip already asserts this, record that and drop the gap.

## Out of scope

- Promoting the section out of developer mode. That is a separate decision and would need the gating tests updated at the same time.
- Any change to the guard's behaviour.

## Verification

From the repo root and `src-tauri/`: `cargo clippy`, `cargo fmt --check`, `cargo test`, `npm run lint`, `npm run lint:md`, `npm run test`. Report any failure with its output. No commit or push until Stuart asks.

## Outcome

- **Gap 1:** The existing mock-TCP harness drives poll cycles but cannot observe register writes, so the block was extracted as `evaluate_discharge_floor` in `poll.rs` with no behaviour change. It is tested directly (raise and persist, restore and clear, disabled, outside a window, persistence failure still returns the writes). `poll_loop_runs_the_discharge_floor_guard` runs the real poll loop against a scripted mock dongle and checks the saved reserve reaches disk; removing the call from the loop makes it fail.
- **Gap 2:** The load-failure message and the locked Save were already covered for this section by the parametrised cases in `controlPageSaveBeforeLoad.test.tsx` (failed load, non-success response). The missing save-error and save-success feedback is now in `controlPageDischargeFloor.test.tsx`, with a pinned clock.
- **Gap 3:** `controlPageDischargeFloor.test.tsx` covers developer mode on and off. Removing the gate fails it.
- **Gap 4:** The handler-level round trip already asserted the live config. A router-level test, `discharge_floor_post_reaches_the_live_config` in `tests/e2e_mock.rs`, now also proves the route is mounted and a rejected post changes nothing.
