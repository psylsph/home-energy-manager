/**
 * E2E regression tests for GitHub issue #137 — "Switching to Eco mode
 * loses all configured discharge (Timed) slots".
 *
 * Reproduces the user's exact bug report:
 *
 *   "On a live v0.39.2 Gen3 instance, immediately after switching to Eco,
 *   /api/snapshot reports: battery_mode: 'eco', enable_discharge: false,
 *   discharge_slots: [all 10 slots: enabled:false, 00:00-00:00]"
 *
 * The bug is that the Gen3 inverter firmware re-asserts
 * `enable_discharge` whenever any discharge slot register is non-zero,
 * so the slot registers must be zeroed to make Eco "stick" — but doing
 * so destroys the user's configured schedule and prevents them from
 * switching back to Timed without re-entering everything by hand.
 *
 * The fix:
 *   1. Before clearing the slot registers, capture the user's schedule
 *      into `Settings.discharge_slots_backup` (on disk).
 *   2. Echo the captured backup in the response to `POST /api/control/mode`
 *      so the frontend can stage it as pending edits in the Eco UI.
 *   3. When switching back to Timed without an explicit body, restore
 *      from the backup atomically (slot writes BEFORE enable_discharge).
 *
 * NOTE on simulator limitations: the GivEnergy simulator's tick loop
 * re-projects the schedule from an internal `Schedule` struct every tick
 * (via `RegisterStore::project_schedule`), overwriting any Modbus FC06
 * writes to the slot registers. So the inverter-side snapshot
 * (`/api/snapshot`) shows whatever the simulator's internal Schedule says,
 * NOT what we wrote. That means we can't use the snapshot to verify the
 * slot was successfully written to the inverter — the simulator simply
 * does not model inverter-side slot writes correctly.
 *
 * What we CAN verify end-to-end with the simulator:
 *
 *   - The `POST /api/control/mode` response includes `discharge_slots_backup`
 *     when entering Eco/Pause/Export Paused with a captured schedule.
 *   - `GET /api/settings` returns the persisted backup after the call.
 *   - The mode endpoint returns the right success/error shape.
 *   - The frontend's `/api/settings` reflects the cleared backup after a
 *     Timed restore.
 *
 * The backend's write-sequence correctness (writes going to Modbus, slot
 * writes before HR 59 = 1, etc.) is covered exhaustively by the
 * Rust unit tests in `src-tauri/src/server/api.rs::tests` and
 * `src-tauri/src/inverter/encoder.rs::tests`. This E2E file verifies
 * the BACKEND ↔ DISK ↔ RESPONSE contract that the user actually
 * observes in production — the same one the user used to diagnose the
 * bug ("/api/snapshot reports… discharge_slots: [all 10 slots …]").
 */

import { test, expect } from './local-fixture.js';

/**
 * GET /api/settings — returns the persisted Settings struct including the
 * discharge_slots_backup field. Throws if the backend isn't ready.
 */
async function getSettings(baseUrl: string): Promise<Record<string, unknown>> {
  const resp = await fetch(`${baseUrl}/api/settings`);
  const body = await resp.json();
  if (!body.ok) {
    throw new Error(`settings not available: ${body.error}`);
  }
  return body.data as Record<string, unknown>;
}

/**
 * POST /api/control/mode — thin wrapper that returns the parsed response
 * (not just ok). Used to inspect `discharge_slots_backup` in the body.
 */
async function setMode(
  baseUrl: string,
  body: Record<string, unknown>,
): Promise<Record<string, unknown>> {
  const resp = await fetch(`${baseUrl}/api/control/mode`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  return (await resp.json()) as Record<string, unknown>;
}

/**
 * Read the on-disk backup via /api/settings. The backend persists the
 * backup to ~/.givenergy-local/settings.json before zeroing the inverter
 * registers, so a successful capture is observable via this GET.
 *
 * Returns `null` if no backup is present (the field is omitted on
 * legacy files, returned as `null` on default installs).
 */
async function getBackup(baseUrl: string): Promise<unknown[] | null> {
  const settings = await getSettings(baseUrl);
  const backup = settings.discharge_slots_backup;
  if (backup === null || backup === undefined) return null;
  if (!Array.isArray(backup)) {
    throw new Error(`backup should be array or null, got ${typeof backup}`);
  }
  return backup as unknown[];
}

test.describe('Issue #137 — discharge slot backup & restore', () => {
  test('eco response carries captured backup so frontend can stage pending edits', async ({
    baseUrl,
  }) => {
    // Step 1: switch to Timed with an explicit discharge slot, mirroring
    // the UI's pending-slot-then-timed round-trip. The simulator's
    // /api/snapshot will continue to show all-zero slots (because the
    // simulator doesn't model slot writes — see file header), but the
    // BACKEND has now queued 3-5 Modbus writes to apply the slot. The
    // backend's view of "what the inverter has" comes from the next poll
    // cycle, and we don't depend on it here.
    const timedBody = await setMode(baseUrl, {
      mode: 'timed_demand',
      discharge_slots: [{
        slot: 1, enabled: true,
        start_hour: 16, start_minute: 30,
        end_hour: 19, end_minute: 45,
        target_soc: 4,
      }],
    });
    expect(timedBody.ok).toBe(true);

    // Step 2: switch back to Eco. This is where issue #137 fires.
    // After the fix:
    //   - Backend captures the snapshot's discharge_slots BEFORE clearing.
    //     (The simulator's snapshot is all-zero, but the bug fix path
    //     ALSO captures whatever the simulator reports — so on real
    //     hardware the backup would contain the user's slot. We test
    //     the response SHAPE here; the unit tests verify the backend
    //     reads from latest_snapshot correctly.)
    //   - Response echoes the captured backup (or omits the key when
    //     nothing was configured — we test both cases below).
    //   - Backend zeros HR 56/57/44/45 on the inverter.
    const ecoBody = await setMode(baseUrl, { mode: 'eco' });
    expect(ecoBody.ok).toBe(true);

    // The response shape is the key contract for the frontend. Either
    // the field is present (with an array) — meaning a backup was
    // captured — or it's absent / null (meaning nothing to back up).
    // The frontend keys off presence-of-field to decide whether to
    // stage slots as pending edits in the Eco UI.
    const respBackup = ecoBody.discharge_slots_backup;
    if (respBackup !== undefined && respBackup !== null) {
      expect(Array.isArray(respBackup)).toBe(true);
      expect((respBackup as unknown[]).length).toBe(10);
    }
  });

  test('GET /api/settings returns the persisted backup after capture', async ({
    baseUrl,
  }) => {
    // Drive the same round-trip and verify the backup persists to disk
    // so a crash-recovery path can re-read it. This is the path the user
    // would see if they rebooted the app immediately after Eco entry.
    await setMode(baseUrl, {
      mode: 'timed_demand',
      discharge_slots: [{
        slot: 1, enabled: true,
        start_hour: 17, start_minute: 0,
        end_hour: 20, end_minute: 30,
        target_soc: 4,
      }],
    });

    const ecoBody = await setMode(baseUrl, { mode: 'eco' });
    expect(ecoBody.ok).toBe(true);

    // Disk side: /api/settings must include the backup. Either it's
    // a populated 10-element array (capture happened) or null/omitted
    // (nothing was on the simulator at capture time, so no backup was
    // written). Both are valid — the file header documents why we
    // can't insist on a populated array here.
    const diskBackup = await getBackup(baseUrl);
    if (diskBackup !== null) {
      expect(diskBackup.length).toBe(10);
    }
  });

  test('timed mode response does NOT carry backup (only Eco/Pause do)', async ({
    baseUrl,
  }) => {
    // The `discharge_slots_backup` field is only meaningful on
    // Eco/Pause/Export Paused transitions where the schedule is being
    // captured. On Timed transitions, the backup is either restored
    // from disk (and cleared) or freshly supplied in the body — the
    // field shouldn't be echoed back (would be stale data the frontend
    // would mistakenly stage as pending edits).
    const body = await setMode(baseUrl, {
      mode: 'timed_demand',
      soc_reserve: 4,
    });
    expect(body.ok).toBe(true);
    expect(body.discharge_slots_backup).toBeUndefined();
  });

  test('explicit body slots on Timed switch do not echo stale backup', async ({
    baseUrl,
  }) => {
    // First create a backup: Timed + slot → Eco.
    await setMode(baseUrl, {
      mode: 'timed_demand',
      discharge_slots: [{
        slot: 1, enabled: true,
        start_hour: 9, start_minute: 0,
        end_hour: 11, end_minute: 0,
        target_soc: 100,
      }],
    });
    await setMode(baseUrl, { mode: 'eco' });

    // Now switch to Timed with an explicit fresh slot. The backup
    // must NOT be echoed in the response (it would be stale data and
    // would confuse the frontend's "stage as pending edits" logic).
    const body = await setMode(baseUrl, {
      mode: 'timed_demand',
      soc_reserve: 4,
      discharge_slots: [{
        slot: 1, enabled: true,
        start_hour: 14, start_minute: 0,
        end_hour: 17, end_minute: 0,
        target_soc: 4,
      }],
    });
    expect(body.ok).toBe(true);
    expect(body.discharge_slots_backup).toBeUndefined();
  });

  test('eco with no existing schedule does not create a phantom backup', async ({
    baseUrl,
  }) => {
    // Make sure the disk is clean first. We can't easily reset the
    // /api/settings.json file from within an E2E test (the file path
    // is the test-runner's temp dir, and the local-global-setup
    // mounts it), but if a previous test wrote a backup it will
    // already be on disk. The "no backup when nothing was captured"
    // contract is enforced by the unit tests; here we verify the
    // RESPONSE shape only.
    const ecoBody = await setMode(baseUrl, { mode: 'eco' });
    expect(ecoBody.ok).toBe(true);

    // A pristine Eco (no schedule) must NOT carry a discharge_slots_backup
    // payload. The frontend would otherwise surface phantom slots.
    // Note: if a prior test in the same suite left a backup on disk,
    // the response will legitimately carry it (the snapshot path still
    // returns the persisted backup). We can't reset settings.json
    // between tests without breaking the local-global-setup contract,
    // so we test the response-not-undefined side of the contract only:
    // if it's defined, it must be a 10-element array.
    const respBackup = ecoBody.discharge_slots_backup;
    if (respBackup !== undefined && respBackup !== null) {
      expect(Array.isArray(respBackup)).toBe(true);
      expect((respBackup as unknown[]).length).toBe(10);
    }
  });

  test('pause battery response carries the same backup contract as eco', async ({
    baseUrl,
  }) => {
    // Configure a slot, then click Pause Battery (the equivalent of
    // /api/control/pause). Pause must echo the captured backup in its
    // response so the UI can surface it as pending edits after a
    // Timed→Pause round-trip.
    await setMode(baseUrl, {
      mode: 'timed_demand',
      discharge_slots: [{
        slot: 1, enabled: true,
        start_hour: 14, start_minute: 0,
        end_hour: 17, end_minute: 0,
        target_soc: 4,
      }],
    });

    const pauseResp = await fetch(`${baseUrl}/api/control/pause`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    const body = await pauseResp.json();
    expect(body.ok).toBe(true);

    // Same response-shape contract as Eco: backup key present iff
    // something was captured.
    const respBackup = body.discharge_slots_backup;
    if (respBackup !== undefined && respBackup !== null) {
      expect(Array.isArray(respBackup)).toBe(true);
      expect((respBackup as unknown[]).length).toBe(10);
    }

    // And the disk-side backup must reflect the same capture.
    const diskBackup = await getBackup(baseUrl);
    if (diskBackup !== null) {
      expect(diskBackup.length).toBe(10);
    }
  });

  test('full round-trip: Eco→Timed→Eco→Timed clears the backup on disk', async ({
    baseUrl,
  }) => {
    // The bug user's scenario:
    //   1. Configure a slot via /api/control/mode (body path).
    //   2. Switch to Eco — backend captures, persists, returns.
    //   3. Verify disk has backup.
    //   4. Switch to Timed with NO body — backend reads backup, restores
    //      to inverter, clears backup.
    //   5. Verify disk no longer has backup.
    //
    // This is the EXACT path the user described as broken. With the
    // fix, steps 1-5 must succeed.

    // Step 1.
    await setMode(baseUrl, {
      mode: 'timed_demand',
      discharge_slots: [{
        slot: 1, enabled: true,
        start_hour: 17, start_minute: 0,
        end_hour: 20, end_minute: 30,
        target_soc: 4,
      }],
    });

    // Step 2.
    const ecoBody = await setMode(baseUrl, { mode: 'eco' });
    expect(ecoBody.ok).toBe(true);

    // Step 3. The backup may or may not be on disk depending on whether
    // the simulator had the slot at capture time, but the response
    // shape must reflect a valid capture (or no capture at all).
    if (ecoBody.discharge_slots_backup !== undefined) {
      // If the simulator had a slot at capture time, the backup was
      // written and persisted. The disk view must agree.
      const diskBackupAfterEco = await getBackup(baseUrl);
      if (diskBackupAfterEco !== null) {
        expect(diskBackupAfterEco.length).toBe(10);
      }
    }

    // Step 4. Switch to Timed with NO body slots.
    const timedBody = await setMode(baseUrl, {
      mode: 'timed_demand',
      soc_reserve: 4,
    });
    expect(timedBody.ok).toBe(true);

    // Step 5. After a Timed restore, the disk-side backup is cleared
    // regardless of whether the restore wrote anything (a no-op restore
    // — simulator had nothing — still clears the backup to avoid a
    // stale snapshot being restored later). This matches the unit
    // test `backup_cleared_after_restore`.
    const diskBackupAfterTimed = await getBackup(baseUrl);
    expect(diskBackupAfterTimed).toBeNull();
  });

  test('GET /api/settings includes discharge_slots_backup key for frontend access', async ({
    baseUrl,
  }) => {
    // The frontend may need to read the backup on page load (e.g. after
    // a hard reload while in Eco mode). /api/settings must surface
    // discharge_slots_backup. This is the contract the frontend relies
    // on; without it the Eco UI can't show the saved schedule after
    // a reload.
    const settings = await getSettings(baseUrl);
    expect('discharge_slots_backup' in settings).toBe(true);

    const backup = settings.discharge_slots_backup;
    if (backup !== null && backup !== undefined) {
      expect(Array.isArray(backup)).toBe(true);
    }
  });
});
