/**
 * Real-simulator coverage for the HEM-managed Timed Export schedule
 * (issue #289).
 *
 * The simulator re-projects HR27/HR59 and the slot registers from its own
 * internal (empty) schedule every tick, so register-level entry/exit writes
 * cannot be confirmed against it — those are covered deterministically by the
 * mock-Modbus suite. What the simulator CAN prove is the #289 persistence
 * contract: saving a desired slot stores the schedule in HEM (settings +
 * `/api/timed-export`), leaves Eco as the baseline outside the window, and the
 * Control page shows the Configured state rather than the "exporting now"
 * banner. All windows are computed relative to the current time so the
 * expected states never depend on when the test runs.
 */

import { test, expect } from './local-fixture.js';

/** HHMM-encode (now + offsetMinutes), wrapping past midnight. */
function hhmmOffset(offsetMinutes: number): number {
  const now = new Date();
  const total = now.getHours() * 60 + now.getMinutes() + offsetMinutes;
  const wrapped = ((total % 1440) + 1440) % 1440;
  const hours = Math.floor(wrapped / 60);
  const minutes = wrapped % 60;
  return hours * 100 + minutes;
}

async function getSnapshot(baseUrl: string): Promise<Record<string, any>> {
  const response = await fetch(`${baseUrl}/api/snapshot`);
  const body = await response.json();
  if (!body.ok) throw new Error(`snapshot unavailable: ${JSON.stringify(body)}`);
  return body.data as Record<string, any>;
}

async function getTimedExport(baseUrl: string): Promise<Record<string, any>> {
  const response = await fetch(`${baseUrl}/api/timed-export`);
  const body = await response.json();
  if (!body.ok) throw new Error(`timed-export unavailable: ${JSON.stringify(body)}`);
  return body.data as Record<string, any>;
}

async function postJson(
  baseUrl: string,
  path: string,
  body: Record<string, unknown>,
): Promise<{ status: number; body: Record<string, any> }> {
  const response = await fetch(`${baseUrl}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  return { status: response.status, body: await response.json() };
}

/**
 * Wait until the polled snapshot reports the clean Eco state (no discharge
 * slots configured, discharge not armed) AND the HEM schedule is disabled, so
 * no earlier test leaves schedule state behind for the next one.
 */
async function resetToCleanState(baseUrl: string): Promise<void> {
  // Canonical clean slate: clear the desired schedule, any #137 backup,
  // force reverts and queued writes left by earlier specs (harness-only
  // endpoint, armed by --e2e-admin in the local global setup).
  await fetch(`${baseUrl}/api/test/reset`, { method: 'POST' });

  await postJson(baseUrl, '/api/control/timed-export', { enabled: false });

  await expect
    .poll(
      async () => {
        const schedule = await getTimedExport(baseUrl);
        return schedule.schedule_enabled === false;
      },
      { timeout: 30_000, intervals: [1_000] },
    )
    .toBe(true);

  // Precondition: the poll must be broadcasting fresh snapshots (not stuck
  // draining a deep write queue), otherwise the disable writes below would
  // sit behind the backlog indefinitely.
  await expect
    .poll(
      async () => {
        const snapshot = await getSnapshot(baseUrl);
        const ts = snapshot.timestamp as number | undefined;
        return typeof ts === 'number' && Math.abs(Date.now() / 1000 - ts) < 15;
      },
      { timeout: 60_000, intervals: [1_000] },
    )
    .toBe(true);

  await expect
    .poll(
      async () => {
        const snapshot = await getSnapshot(baseUrl);
        return snapshot.enable_discharge === false && snapshot.battery_power_mode === 1;
      },
      { timeout: 30_000, intervals: [1_000] },
    )
    .toBe(true);
}

test.describe('Real simulator — HEM-managed Timed Export schedule', () => {
  test.describe.configure({ timeout: 120_000 });

  test('saving a future slot persists the schedule without arming the inverter', async ({ baseUrl }) => {
    await resetToCleanState(baseUrl);

    // A window that always lies in the future (+90..+180 min) so Eco must
    // remain the baseline for the whole test — deterministic regardless of
    // wall clock.
    const result = await postJson(baseUrl, '/api/control/discharge-slot', {
      slot: 1,
      enabled: true,
      start_hour: Math.floor(hhmmOffset(90) / 100),
      start_minute: hhmmOffset(90) % 100,
      end_hour: Math.floor(hhmmOffset(180) / 100),
      end_minute: hhmmOffset(180) % 100,
      target_soc: 20,
    });
    expect(result.status).toBe(200);
    expect(result.body.ok).toBe(true);

    // The desired schedule is persisted and exposed by the backend.
    const schedule = await getTimedExport(baseUrl);
    expect(schedule.schedule_enabled).toBe(true);
    const savedSlot = (schedule.slots as Array<Record<string, unknown>>).find(
      (slot) => (slot as { enabled: boolean }).enabled,
    );
    expect(savedSlot).toBeDefined();

    // Eco remains the baseline: no export arming, reserve untouched.
    await expect
      .poll(
        async () => {
          const snapshot = await getSnapshot(baseUrl);
          return snapshot.enable_discharge === false && snapshot.battery_power_mode === 1;
        },
        { timeout: 30_000, intervals: [1_000] },
      )
      .toBe(true);
  });

  test('disabling the schedule returns the backend to Off', async ({ baseUrl }) => {
    await resetToCleanState(baseUrl);

    // Save a future slot, then disable the whole schedule.
    await postJson(baseUrl, '/api/control/discharge-slot', {
      slot: 1,
      enabled: true,
      start_hour: Math.floor(hhmmOffset(90) / 100),
      start_minute: hhmmOffset(90) % 100,
      end_hour: Math.floor(hhmmOffset(180) / 100),
      end_minute: hhmmOffset(180) % 100,
      target_soc: 20,
    });

    const disable = await postJson(baseUrl, '/api/control/timed-export', { enabled: false });
    expect(disable.status).toBe(200);
    expect(disable.body.ok).toBe(true);

    await expect
      .poll(
        async () => {
          const schedule = await getTimedExport(baseUrl);
          return schedule.schedule_enabled === false;
        },
        { timeout: 30_000, intervals: [1_000] },
      )
      .toBe(true);
  });

  test('the control page shows Configured for a future slot, not the exporting banner', async ({
    baseUrl,
    page,
  }) => {
    await resetToCleanState(baseUrl);

    await postJson(baseUrl, '/api/control/discharge-slot', {
      slot: 1,
      enabled: true,
      start_hour: Math.floor(hhmmOffset(90) / 100),
      start_minute: hhmmOffset(90) % 100,
      end_hour: Math.floor(hhmmOffset(180) / 100),
      end_minute: hhmmOffset(180) % 100,
      target_soc: 20,
    });

    await page.goto('/#/control');

    // The schedule card reads Configured (waiting for the window) — pinned
    // via the configured-only "Next export starts at" line — never the amber
    // exporting banner for a future slot. (The quick-action button carries a
    // copy of the state label, so a bare getByText would be ambiguous.)
    await expect(page.getByText(/Next export starts at/)).toBeVisible({ timeout: 15_000 });
    await expect(
      page.getByText('Timed Export is exporting now.'),
    ).toHaveCount(0);

    // The Timed Export quick action is armed by the persisted schedule.
    const timedExport = page.getByRole('button', { name: /Timed Export/ });
    await expect(timedExport).toBeVisible();
    await expect(timedExport).toBeEnabled();
  });
});
