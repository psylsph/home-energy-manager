/**
 * Real-simulator coverage for the Timed Export/discharge-slot guard.
 *
 * The simulator starts with an empty schedule and re-projects its internal
 * schedule into HR 59 and the slot registers every tick. It therefore cannot
 * hold the invalid HR59=1/no-slot state or preserve a slot written by HEM.
 * These tests cover the realistic protocol/API/UI contract that the simulator
 * can represent; the mock-Modbus suite covers injected state alignment.
 */

import { test, expect } from './local-fixture.js';

async function postTimedExport(baseUrl: string, enabled: boolean) {
  const response = await fetch(`${baseUrl}/api/control/timed-export`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ enabled }),
  });
  return { status: response.status, body: await response.json() } as {
    status: number;
    body: { ok: boolean; error?: string };
  };
}

async function getSnapshot(baseUrl: string): Promise<Record<string, any>> {
  const response = await fetch(`${baseUrl}/api/snapshot`);
  const body = await response.json();
  if (!body.ok) throw new Error(`snapshot unavailable: ${JSON.stringify(body)}`);
  return body.data as Record<string, any>;
}

/** Wait until the polled snapshot reports the clean Eco state (no discharge
 *  slots configured, discharge not armed). The shared simulator's schedule
 *  can be polluted by earlier specs, and the poll loop drains queued writes
 *  one batch per cycle, so a long queue starves new writes — first wait for
 *  a FRESH snapshot (proves the poll is broadcasting and the backlog has
 *  drained), then post Eco and wait for the snapshot to confirm it. */
async function resetToEco(baseUrl: string): Promise<void> {
  // Canonical clean slate first: clear the desired schedule, any #137
  // backup, force reverts and queued writes that earlier specs left behind
  // (harness-only endpoint, armed by --e2e-admin in the local global setup).
  // Without this the poll-loop reconciler re-applies a leaked desired
  // schedule after the Eco reset below and the "no slot" invariants flake.
  const reset = await fetch(`${baseUrl}/api/test/reset`, { method: 'POST' });
  if (reset.ok) {
    // The Eco write below must reach the simulator as a fresh batch.
    await new Promise((r) => setTimeout(r, 500));
  }

  // Precondition: the poll must be broadcasting fresh snapshots (not stuck
  // draining a deep write queue), otherwise the Eco writes below would sit
  // behind the backlog indefinitely. Use expect.poll (idiomatic Playwright)
  // rather than a hand-rolled Date.now() deadline loop — the latter is a
  // CI flake candidate because it burns real wall-clock time and has no
  // backoff.
  await expect
    .poll(
      async () => {
        const snapshot = await getSnapshot(baseUrl);
        const ts = snapshot.timestamp as number | undefined;
        return typeof ts === 'number' && Math.abs(Date.now() / 1000 - ts) < 15;
      },
      { timeout: 120_000, intervals: [1_000] },
    )
    .toBe(true);

  const response = await fetch(`${baseUrl}/api/control/eco`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ enabled: true }),
  });
  expect(response.ok).toBe(true);

  await expect
    .poll(
      async () => {
        const snapshot = await getSnapshot(baseUrl);
        const slots = (snapshot.discharge_slots ?? []) as Array<{
          enabled: boolean;
          start_hour: number;
          start_minute: number;
          end_hour: number;
          end_minute: number;
        }>;
        const noneConfigured = slots.every(
          (s) =>
            !s.enabled &&
            s.start_hour === 0 &&
            s.start_minute === 0 &&
            s.end_hour === 0 &&
            s.end_minute === 0,
        );
        return snapshot.enable_discharge === false && noneConfigured;
      },
      { timeout: 90_000, intervals: [1_000] },
    )
    .toBe(true);
}

test.describe('Real simulator — Timed Export/discharge-slot guard', () => {
  // resetToEco waits for the polled snapshot to reflect the clean state
  // (the eco writes drain over ~10s and earlier specs can leave the shared
  // simulator's schedule dirty), so the default 30s test timeout is too
  // tight.
  test.describe.configure({ timeout: 120_000 });
  test('rejects Timed Export when the simulator reports no configured slot', async ({ baseUrl }) => {
    await resetToEco(baseUrl);

    // Read the snapshot BEFORE the POST — the backend's enable check reads
    // the same latest_snapshot, so this is deterministic (no broadcast can
    // land between the read and the POST).
    const snapshotBefore = await getSnapshot(baseUrl);
    const hasConfiguredSlot = ((snapshotBefore.discharge_slots ?? []) as Array<{ enabled: boolean }>)
      .some((s) => s.enabled);

    // The backend also restores a persisted discharge_slots_backup (issue
    // #137) when no live slot exists — an earlier spec's Eco entry can have
    // captured one. Account for it so the expectation matches the backend's
    // actual arm decision.
    const settingsResp = await fetch(`${baseUrl}/api/settings`);
    const settings = (await settingsResp.json()).data as {
      discharge_slots_backup?: unknown[] | null;
    };
    const hasBackup =
      Array.isArray(settings.discharge_slots_backup) &&
      settings.discharge_slots_backup.length > 0;

    const result = await postTimedExport(baseUrl, true);

    // The invariant: Timed Export may only arm when a discharge slot is
    // configured (live or restored from the #137 backup). If neither
    // exists, the backend must refuse.
    if (hasConfiguredSlot || hasBackup) {
      expect(result.status).toBe(200);
      expect(result.body.ok).toBe(true);
    } else {
      expect(result.status).toBe(409);
      expect(result.body.ok).toBe(false);
      expect(result.body.error).toContain('Configure at least one discharge slot');
    }
  });

  test('keeps the browser control aligned with the simulator no-slot state', async ({
    baseUrl,
    page,
  }) => {
    await resetToEco(baseUrl);
    await page.goto('/#/control');

    const timedExport = page.getByRole('button', { name: /Timed Export/ });
    await expect(timedExport).toBeVisible({ timeout: 15_000 });
    await expect(timedExport).toBeDisabled();
    await expect(
      page.getByText('Configure at least one discharge slot before enabling Timed Export.'),
    ).toBeVisible();
  });

  test('a slot write alone cannot make the simulator report Timed Export armed', async ({ baseUrl }) => {
    await resetToEco(baseUrl);
    const slotResponse = await fetch(`${baseUrl}/api/control/discharge-slot`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        slot: 1,
        enabled: true,
        start_hour: 16,
        start_minute: 0,
        end_hour: 19,
        end_minute: 0,
        target_soc: 4,
      }),
    });
    expect(slotResponse.ok).toBe(true);

    // The simulator may re-project its own empty Schedule after the FC06
    // write (pacing-dependent), so decide from the polled state exactly like
    // the backend does: HEM may only arm when a configured slot is visible,
    // and must refuse when there is none.
    await new Promise((resolve) => setTimeout(resolve, 7_000));
    const snapshot = await getSnapshot(baseUrl);
    const slotVisible = ((snapshot.discharge_slots ?? []) as Array<{ enabled: boolean }>).some(
      (s) => s.enabled,
    );
    const result = await postTimedExport(baseUrl, true);
    if (slotVisible) {
      expect(result.status).toBe(200);
      expect(result.body.ok).toBe(true);
    } else {
      expect(result.status).toBe(409);
      expect(result.body.ok).toBe(false);
    }
  });
});
