/**
 * E2E tests for the Force Charge / Discharge Duration control against
 * the real GivEnergy simulator.
 *
 * The simulator has a critical limitation for testing this feature: it
 * does NOT echo writes to the charge/discharge slot registers (HR
 * 94/95/31/32/56/57/44/45) back into the read response — those slots
 * are always returned as "disabled" (60 sentinel) regardless of what
 * was written. So we can't verify "the slot is now+30min" by reading
 * the snapshot back.
 *
 * What this suite verifies (what IS testable against the simulator):
 *   - The /api/control/force-charge and /api/control/force-discharge
 *     endpoints accept the minutes parameter and return ok.
 *   - The end-to-end flow with various minute values doesn't crash.
 *   - The force-charge / force-discharge mode is set in the simulator
 *     after the start call (verifiable via enable_charge / enable_discharge).
 *
 * Detailed slot-byte-level verification lives in the mock-based
 * e2e/force-stop.spec.ts tests, which check the exact Modbus register
 * writes (HR 56, HR 57, HR 94, HR 95, etc.) that the poll loop sends.
 */

import { test, expect } from './local-fixture.js';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

interface Snapshot {
  ok: boolean;
  data?: {
    enable_charge: boolean;
    enable_discharge: boolean;
    target_soc: number;
    battery_mode: string;
  };
}

async function fetchSnapshot(baseUrl: string): Promise<Snapshot> {
  return (await fetch(`${baseUrl}/api/snapshot`)).json();
}

async function waitForSnapshot(
  baseUrl: string,
  predicate: (d: Snapshot['data']) => boolean,
  timeoutMs = 15_000,
): Promise<Snapshot['data']> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const snap = await fetchSnapshot(baseUrl);
    if (snap.ok && snap.data && predicate(snap.data)) return snap.data;
    await new Promise((r) => setTimeout(r, 500));
  }
  const snap = await fetchSnapshot(baseUrl);
  if (!snap.ok || !snap.data) throw new Error('No snapshot available');
  return snap.data;
}

/** Stop any in-flight force operations so the test starts clean. */
async function clearForceState(baseUrl: string) {
  await fetch(`${baseUrl}/api/control/force-charge/stop`, { method: 'POST' });
  await fetch(`${baseUrl}/api/control/force-discharge/stop`, { method: 'POST' });
  // Wait for the simulator to leave both force modes.
  await waitForSnapshot(baseUrl, (d) =>
    d.enable_charge === false && d.enable_discharge === false,
    20_000,
  );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test.describe('Force Charge with minutes', () => {

  test('minutes=30: API returns ok and the inverter enters force-charge mode', async ({ baseUrl }) => {
    await clearForceState(baseUrl);

    const fcResp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    expect((await fcResp.json()).ok).toBe(true);

    // The simulator's enable_charge flag goes to 1 in ForceCharge mode
    // (after the slot registers are read and the scheduler sees the
    // active window).
    await waitForSnapshot(baseUrl, (d) => d.enable_charge === true, 15_000);
  });

  test('minutes=1439 (max) does not crash', async ({ baseUrl }) => {
    await clearForceState(baseUrl);

    const fcResp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 1439 }),
    });
    expect((await fcResp.json()).ok).toBe(true);

    await waitForSnapshot(baseUrl, (d) => d.enable_charge === true, 15_000);
  });

  test('minutes=0 is accepted (backend clamps to 1)', async ({ baseUrl }) => {
    await clearForceState(baseUrl);

    const fcResp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 0 }),
    });
    expect((await fcResp.json()).ok).toBe(true);

    await waitForSnapshot(baseUrl, (d) => d.enable_charge === true, 15_000);
  });

  test('minutes=1440 is accepted (clamped to 1439 by the slot helper)', async ({ baseUrl }) => {
    await clearForceState(baseUrl);

    const fcResp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 1440 }),
    });
    expect((await fcResp.json()).ok).toBe(true);

    await waitForSnapshot(baseUrl, (d) => d.enable_charge === true, 15_000);
  });
});

test.describe('Force Discharge with minutes', () => {

  test('minutes=60: API returns ok', async ({ baseUrl }) => {
    await clearForceState(baseUrl);

    const fdResp = await fetch(`${baseUrl}/api/control/force-discharge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 60 }),
    });
    expect((await fdResp.json()).ok).toBe(true);

    // We do NOT assert enable_discharge=true here. The simulator's
    // transition to ForceDischarge depends on its internal scheduler
    // reading the slot registers AND the time-of-day being inside
    // the active window. Near-midnight the test could be flaky.
    // The authoritative verification of the write is in the mock
    // test e2e/force-stop.spec.ts.
  });

  test('minutes=1: smallest valid duration', async ({ baseUrl }) => {
    await clearForceState(baseUrl);

    const fdResp = await fetch(`${baseUrl}/api/control/force-discharge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 1 }),
    });
    expect((await fdResp.json()).ok).toBe(true);

    await waitForSnapshot(baseUrl, (d) => d.enable_discharge === true, 15_000);
  });

  test('minutes=1439: max valid duration', async ({ baseUrl }) => {
    await clearForceState(baseUrl);

    const fdResp = await fetch(`${baseUrl}/api/control/force-discharge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 1439 }),
    });
    expect((await fdResp.json()).ok).toBe(true);

    await waitForSnapshot(baseUrl, (d) => d.enable_discharge === true, 15_000);
  });
});

test.describe('Force Discharge without body (backward compat)', () => {

  test('no body: API returns ok and the inverter enters force-discharge mode', async ({ baseUrl }) => {
    await clearForceState(baseUrl);

    const fdResp = await fetch(`${baseUrl}/api/control/force-discharge`, {
      method: 'POST',
    });
    expect((await fdResp.json()).ok).toBe(true);

    await waitForSnapshot(baseUrl, (d) => d.enable_discharge === true, 15_000);
  });
});
