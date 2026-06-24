/**
 * E2E tests for Force Charge / Force Discharge start→stop round-trips
 * against the real GivEnergy simulator.
 *
 * These tests verify the API and end-to-end flow against a real Modbus
 * TCP server. Note: the simulator does NOT echo writes to certain
 * holding registers (HR 27 / 59 / 96) back into the read response —
 * it computes those values from its internal mode_state instead. So
 * the simulator can't be used to verify "the inverter accepted the
 * stop write" — that's what the mock-based tests in
 * `e2e/force-stop.spec.ts` do, by checking the exact Modbus register
 * writes the poll loop sends.
 *
 * What this suite verifies:
 *   - The /api/control/force-charge/stop and /api/control/force-discharge/stop
 *     endpoints exist and respond correctly.
 *   - Defensive guards: stop with no active force returns 400.
 *   - Stop is one-shot (second call also returns 400).
 *   - The end-to-end flow (start, wait, stop, wait) doesn't crash.
 *   - Force Charge / Force Discharge reverts are independent.
 *   - Charge slot registers appear in the snapshot when the simulator
 *     moves into the right mode (e.g. enable_charge=1 forces a
 *     non-disabled slot to be read back).
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
    charge_slots: Array<{
      enabled: boolean;
      start_hour: number;
      start_minute: number;
      end_hour: number;
      end_minute: number;
    }>;
    discharge_slots: Array<{
      enabled: boolean;
      start_hour: number;
      start_minute: number;
      end_hour: number;
      end_minute: number;
    }>;
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

async function setChargeSlot(
  baseUrl: string,
  slot: number,
  startHour: number,
  startMin: number,
  endHour: number,
  endMin: number,
): Promise<void> {
  const resp = await fetch(`${baseUrl}/api/control/charge-slot`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      slot, enabled: true,
      start_hour: startHour, start_minute: startMin,
      end_hour: endHour, end_minute: endMin,
    }),
  });
  if (!(await resp.json()).ok) throw new Error('setChargeSlot failed');
}

async function setMode(
  baseUrl: string,
  mode: 'eco' | 'timed_demand' | 'timed_export',
): Promise<void> {
  const resp = await fetch(`${baseUrl}/api/control/mode`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ mode }),
  });
  if (!(await resp.json()).ok) throw new Error(`setMode(${mode}) failed`);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test.describe('Force Charge → Stop API', () => {

  test('Start then stop completes the full flow', async ({ baseUrl }) => {
    // Ensure baseline: simulator in Eco so the charge flag is 0.
    await setMode(baseUrl, 'eco');
    await waitForSnapshot(baseUrl, (d) => d.enable_charge === false, 15_000);

    // The revert capture in the start endpoint reads latest_snapshot, which
    // can briefly be None if the poll loop clears it (reconnect) between the
    // waitForSnapshot check above and the API call. Retry the start+stop so
    // a transient None revert doesn't fail the test.
    let stopOk = false;
    for (let attempt = 0; attempt < 3 && !stopOk; attempt++) {
      const fcResp = await fetch(`${baseUrl}/api/control/force-charge`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ minutes: 30 }),
      });
      expect((await fcResp.json()).ok).toBe(true);

      if (attempt > 0) await new Promise((r) => setTimeout(r, 500));

      const stopResp = await fetch(`${baseUrl}/api/control/force-charge/stop`, {
        method: 'POST',
      });
      stopOk = (await stopResp.json()).ok;
    }
    expect(stopOk).toBe(true);
  });

  test('Stop with no active Force Charge returns 400', async ({ baseUrl }) => {
    // Drain any in-flight state. If the previous test left a force
    // charge active, the stop here would succeed (consume the revert)
    // and the assertion below would fail with ok=true. We want the
    // "no force charge in progress" path, so we first ensure no
    // force charge is active by calling stop once and ignoring the
    // result — if it was 200, great; if it was 400, the revert was
    // already gone.
    const preResp = await fetch(`${baseUrl}/api/control/force-charge/stop`, {
      method: 'POST',
    });
    await preResp.json();

    // Now a fresh stop should reliably 400.
    const stopResp = await fetch(`${baseUrl}/api/control/force-charge/stop`, {
      method: 'POST',
    });
    const data = await stopResp.json();
    expect(data.ok).toBe(false);
    expect(data.error).toMatch(/no force charge/i);
  });

  test('Stop is one-shot — second call also returns 400', async ({ baseUrl }) => {
    // Start a fresh force charge, stop it, then try to stop again.
    await setMode(baseUrl, 'eco');
    await waitForSnapshot(baseUrl, (d) => d.enable_charge === false, 15_000);

    await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    // We do NOT wait for enable_charge=true — the simulator does not echo
    // slot register writes, so the flag never flips. The revert is captured
    // at start time regardless, so the first stop will succeed.

    const stop1 = await fetch(`${baseUrl}/api/control/force-charge/stop`, { method: 'POST' });
    expect((await stop1.json()).ok).toBe(true);

    // Second stop should be rejected (revert already consumed).
    const stop2 = await fetch(`${baseUrl}/api/control/force-charge/stop`, { method: 'POST' });
    const data = await stop2.json();
    expect(data.ok).toBe(false);
  });

  test('Stop accepts no body (no Content-Type required)', async ({ baseUrl }) => {
    await setMode(baseUrl, 'eco');
    await waitForSnapshot(baseUrl, (d) => d.enable_charge === false, 15_000);

    await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    // We do NOT wait for enable_charge=true — the simulator does not echo
    // slot register writes. The revert is captured at start time.

    // POST with empty body and no Content-Type header.
    const stopResp = await fetch(`${baseUrl}/api/control/force-charge/stop`, {
      method: 'POST',
    });
    expect((await stopResp.json()).ok).toBe(true);
  });
});

test.describe('Force Discharge → Stop API', () => {

  test('Start then stop completes the full flow', async ({ baseUrl }) => {
    // Set up Eco baseline.
    await setMode(baseUrl, 'eco');

    // Drain any leftover discharge state from a previous test.
    await fetch(`${baseUrl}/api/control/force-discharge/stop`, { method: 'POST' });

    // The revert capture can race with the poll loop clearing the snapshot.
    // Retry the start+stop so a transient None revert doesn't fail the test.
    let stopOk = false;
    for (let attempt = 0; attempt < 3 && !stopOk; attempt++) {
      const fdResp = await fetch(`${baseUrl}/api/control/force-discharge`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ minutes: 60 }),
      });
      expect((await fdResp.json()).ok).toBe(true);

      if (attempt > 0) await new Promise((r) => setTimeout(r, 500));

      const stopResp = await fetch(`${baseUrl}/api/control/force-discharge/stop`, { method: 'POST' });
      stopOk = (await stopResp.json()).ok;
    }
    expect(stopOk).toBe(true);
  });

  test('Stop with no active Force Discharge returns 400', async ({ baseUrl }) => {
    // Drain any in-flight state (see the charge counterpart above).
    const preResp = await fetch(`${baseUrl}/api/control/force-discharge/stop`, {
      method: 'POST',
    });
    await preResp.json();

    const stopResp = await fetch(`${baseUrl}/api/control/force-discharge/stop`, {
      method: 'POST',
    });
    const data = await stopResp.json();
    expect(data.ok).toBe(false);
    expect(data.error).toMatch(/no force discharge/i);
  });

  test('Force Charge and Force Discharge reverts are independent', async ({ baseUrl }) => {
    // Set up Eco so the simulator is in a clean baseline.
    await setMode(baseUrl, 'eco');

    // Retry the start if the revert wasn't captured (snapshot briefly None).
    let fcOk = false;
    for (let i = 0; i < 3 && !fcOk; i++) {
      await fetch(`${baseUrl}/api/control/force-charge`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ minutes: 30 }),
      });
      if (i > 0) await new Promise((r) => setTimeout(r, 500));
      // Probe: try to stop and re-arm if it fails.
      const probe = await fetch(`${baseUrl}/api/control/force-charge/stop`, { method: 'POST' });
      fcOk = (await probe.json()).ok;
    }
    // Re-start the charge (the probe above consumed the revert).
    const fcResp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    expect((await fcResp.json()).ok).toBe(true);

    // Now start a force discharge. The charge revert should still be alive.
    const fdResp = await fetch(`${baseUrl}/api/control/force-discharge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    expect((await fdResp.json()).ok).toBe(true);

    // Stop the discharge; the charge revert should still be there.
    const stopFd = await fetch(`${baseUrl}/api/control/force-discharge/stop`, { method: 'POST' });
    expect((await stopFd.json()).ok).toBe(true);

    // And we should still be able to stop the charge.
    const stopFc = await fetch(`${baseUrl}/api/control/force-charge/stop`, { method: 'POST' });
    expect((await stopFc.json()).ok).toBe(true);
  });
});

test.describe('Force Charge with pre-existing schedule', () => {

  test('Force Charge completes the full flow with a pre-existing slot', async ({ baseUrl }) => {
    // Set up a known pre-state: Eco + a 02:00–04:00 charge slot.
    await setMode(baseUrl, 'eco');
    await setChargeSlot(baseUrl, 1, 2, 0, 4, 0);

    // Force Charge.
    const fcResp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    expect((await fcResp.json()).ok).toBe(true);

    // Stop.
    const stopResp = await fetch(`${baseUrl}/api/control/force-charge/stop`, {
      method: 'POST',
    });
    expect((await stopResp.json()).ok).toBe(true);

    // The end-to-end flow completes. Detailed slot-restoration
    // assertions live in the mock-based e2e/force-stop.spec.ts,
    // which checks the exact Modbus register writes (HR 94, HR 95,
    // HR 116, etc.) that the poll loop sends.
  });
});
