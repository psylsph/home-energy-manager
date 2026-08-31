/**
 * Process-level restart recovery for a Timed Export stop left pending.
 *
 * The unit suite covers the restore-from-marker logic directly; this spec
 * proves the whole chain with a real backend process: a Stop whose disarm
 * batch is rejected leaves the durable stop-pending marker armed with the
 * registers still exporting — and a hard process kill + restart must resume
 * the exit and settle on Eco with the schedule still disabled (CODE_REVIEW.md
 * BLOCKER: the restart used to boot the machine as `Off`, misread the armed
 * registers as another controller's schedule, and never repair).
 *
 * The write-rejection hook on the mock (`/reject-writes`) plays the refusing
 * inverter: it rejects the Stop's disarm batch, the API failure path arms
 * `Exiting` + the marker, and the process is killed before any poll cycle can
 * complete the exit. After the restart the rejection is cleared so the
 * restored machine can converge.
 */

import { test, expect } from './fixture.js';
import { startBackend, stopBackend, restartBackendPreservingState } from './backend.js';

const HR_DISCHARGE_SLOT_1_START = 56;
const HR_DISCHARGE_SLOT_1_END = 57;

const wait = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** HHMM-encode (now + offsetMinutes), wrapping past midnight. */
function hhmmOffset(offsetMinutes: number): number {
  const now = new Date();
  const total = now.getHours() * 60 + now.getMinutes() + offsetMinutes;
  const wrapped = ((total % 1440) + 1440) % 1440;
  const hours = Math.floor(wrapped / 60);
  const minutes = wrapped % 60;
  return hours * 100 + minutes;
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

async function getTimedExport(baseUrl: string): Promise<Record<string, any>> {
  const response = await fetch(`${baseUrl}/api/timed-export`);
  const body = await response.json();
  if (!body.ok) throw new Error(`timed-export unavailable: ${JSON.stringify(body)}`);
  return body.data as Record<string, any>;
}

async function waitForSnapshot(
  baseUrl: string,
  predicate: (snapshot: Record<string, any>) => boolean,
  label: string,
  timeoutMs = 20_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      const response = await fetch(`${baseUrl}/api/snapshot`);
      const body = await response.json();
      if (body.ok && predicate(body.data as Record<string, any>)) return;
    } catch {
      /* backend restarting — retry */
    }
    if (Date.now() >= deadline) {
      throw new Error(`timed out waiting for ${label}`);
    }
    await wait(300);
  }
}

test.describe('Timed Export stop pending across a backend restart', () => {
  test.beforeAll(async () => {
    await startBackend();
  });

  test.afterAll(async () => {
    await stopBackend();
  });

  test('a rejected Stop disarms after restart via the stop-pending marker', async ({
    baseUrl,
    drainModbusWrites,
    setHoldingReg,
    setRejectWrites,
  }) => {
    test.setTimeout(240_000);

    // --- Arm Timed Export inside a window that covers "now" ---
    await setHoldingReg(HR_DISCHARGE_SLOT_1_START, hhmmOffset(-90));
    await setHoldingReg(HR_DISCHARGE_SLOT_1_END, hhmmOffset(90));
    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.discharge_slots?.[0]?.enabled === true,
      'in-window discharge slot',
    );
    const arm = await postJson(baseUrl, '/api/control/discharge-slot', {
      slot: 1,
      enabled: true,
      start_hour: Math.floor(hhmmOffset(-90) / 100),
      start_minute: hhmmOffset(-90) % 100,
      end_hour: Math.floor(hhmmOffset(90) / 100),
      end_minute: hhmmOffset(90) % 100,
      target_soc: 20,
    });
    expect(arm.status).toBe(200);
    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.enable_discharge === true && snapshot.battery_power_mode === 0,
      'armed Timed Export snapshot',
    );
    await drainModbusWrites();

    // --- Stop against a refusing inverter: the disarm batch is rejected ---
    // Enough rejections to cover the Stop's retry ladder AND the poll loop's
    // repair attempts until the process is killed, so the exit is genuinely
    // still pending when the backend dies.
    await setRejectWrites(60);
    const disable = await postJson(baseUrl, '/api/control/timed-export', { enabled: false });
    expect(disable.status).toBe(502);
    expect(disable.body.ok).toBe(false);

    // The stop persisted the disabled schedule before its writes failed.
    const stopped = await getTimedExport(baseUrl);
    expect(stopped.schedule_enabled).toBe(false);

    // --- Kill the process mid-recovery and restart it ---
    await restartBackendPreservingState();

    // The restart must have restored the reconciler from the stop-pending
    // marker (Exiting, not Off): the schedule stays disabled and the desired
    // slots are retained as data.
    const reloaded = await getTimedExport(baseUrl);
    expect(reloaded.schedule_enabled).toBe(false);
    expect(reloaded.slots?.length).toBeGreaterThan(0);

    // --- Let the inverter accept writes again: the pending exit resumes ---
    await setRejectWrites(0);
    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.enable_discharge === false && snapshot.battery_power_mode === 1,
      'Eco after the restarted backend resumes the pending exit',
      90_000,
    );

    // And the machine settles on Off once Eco is confirmed.
    let settled = false;
    const deadline = Date.now() + 30_000;
    while (Date.now() < deadline && !settled) {
      const schedule = await getTimedExport(baseUrl);
      settled = schedule.machine_state === 'Off';
      if (!settled) await wait(500);
    }
    expect(settled).toBe(true);
  });
});
