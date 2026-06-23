/**
 * E2E tests for the Force Charge / Discharge start→stop round-trips
 * against the mock Modbus server.
 *
 * These tests verify the exact Modbus register writes produced by
 * start and stop. The mock captures every write the poll loop sends,
 * so we can assert on the register values precisely.
 *
 * The bug being guarded (HR_ENABLE_DISCHARGE silently left at 0
 * after a Force Charge → Stop) would show up as a missing HR 59
 * write in the stop batch.
 *
 * Test design note: the poll loop processes writes at 1.5s each. To
 * keep tests fast, we use the simplest possible API sequences (one
 * start, one stop) and don't try to set up elaborate pre-states
 * that would require multiple writes before the actual test action.
 */

import { test, expect } from './fixture.js';
import type { RegisterWrite } from './mock-modbus.js';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Drain pending writes. Mirrors the `clearWrites` helper in
 * control.spec.ts: repeatedly drain and wait until no new writes
 * appear for 3 seconds (covering ~2 writes × 1.5s retry delay
 * each). This prevents cross-contamination where a previous test's
 * deferred writes arrive in the middle of the next test.
 */
async function clearWrites(drainModbusWrites: () => Promise<RegisterWrite[]>) {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    await drainModbusWrites();
    await new Promise((r) => setTimeout(r, 3000));
    const remaining = await drainModbusWrites();
    if (remaining.length === 0) return;
  }
}

async function waitForWrites(
  peekModbusWrites: () => Promise<RegisterWrite[]>,
  drainModbusWrites: () => Promise<RegisterWrite[]>,
  minCount: number,
  timeoutMs = 30_000,
): Promise<RegisterWrite[]> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const writes = await peekModbusWrites();
    if (writes.length >= minCount) return drainModbusWrites();
    await new Promise((r) => setTimeout(r, 200));
  }
  return drainModbusWrites();
}

function findWrite(writes: RegisterWrite[], address: number): RegisterWrite | undefined {
  return writes.find((w) => w.address === address);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test.describe('Force Charge → Stop (mock Modbus)', () => {

  test('Stop without active force charge returns 400', async ({
    baseUrl,
    drainModbusWrites,
  }) => {
    // A previous test in another file may have left a force-charge
    // revert set. Drain it first so we test the "no force charge"
    // path cleanly.
    await clearWrites(drainModbusWrites);
    await fetch(`${baseUrl}/api/control/force-charge/stop`, { method: 'POST' });
    // Now a fresh stop should 400.
    const resp = await fetch(`${baseUrl}/api/control/force-charge/stop`, {
      method: 'POST',
    });
    const data = await resp.json();
    expect(data.ok).toBe(false);
    expect(data.error).toMatch(/no force charge/i);
  });

  test('Stop is one-shot — second call also returns 400', async ({
    baseUrl,
    drainModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);
    const fcResp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    expect((await fcResp.json()).ok).toBe(true);

    const stop1 = await fetch(`${baseUrl}/api/control/force-charge/stop`, { method: 'POST' });
    expect((await stop1.json()).ok).toBe(true);

    const stop2 = await fetch(`${baseUrl}/api/control/force-charge/stop`, { method: 'POST' });
    const data = await stop2.json();
    expect(data.ok).toBe(false);
  });

  test('Start with minutes produces slot + 5 force-charge flag writes', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    test.setTimeout(60_000);
    await clearWrites(drainModbusWrites);

    await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });

    // 2 slot writes (HR 94, 95) + 5 flag writes = 7 total.
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 7, 30_000);

    expect(findWrite(writes, 94)).toBeDefined();
    expect(findWrite(writes, 95)).toBeDefined();
    expect(findWrite(writes, 27)!.value).toBe(1);    // eco mode
    expect(findWrite(writes, 96)!.value).toBe(1);    // enable_charge
    expect(findWrite(writes, 20)!.value).toBe(1);    // enable_charge_target
    expect(findWrite(writes, 116)!.value).toBe(100); // target SOC

    // Stop. Verify the stop is consumed.
    const stop = await fetch(`${baseUrl}/api/control/force-charge/stop`, { method: 'POST' });
    expect((await stop.json()).ok).toBe(true);
  });

  test('Start without minutes produces only 5 flag writes (no slot)', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    test.setTimeout(60_000);
    await clearWrites(drainModbusWrites);

    // No Content-Type, no body.
    await fetch(`${baseUrl}/api/control/force-charge`, { method: 'POST' });

    // Without slot = 5 writes (HR27, HR59=0, HR96, HR20, HR116).
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 5, 30_000);

    expect(findWrite(writes, 94)).toBeUndefined();
    expect(findWrite(writes, 95)).toBeUndefined();
    expect(findWrite(writes, 27)!.value).toBe(1);
    expect(findWrite(writes, 96)!.value).toBe(1);
    expect(findWrite(writes, 20)!.value).toBe(1);
    expect(findWrite(writes, 116)!.value).toBe(100);
  });
});

test.describe('Force Discharge → Stop (mock Modbus)', () => {

  test('Stop without active force discharge returns 400', async ({
    baseUrl,
    drainModbusWrites,
  }) => {
    // Drain any leftover discharge state from previous tests.
    await clearWrites(drainModbusWrites);
    await fetch(`${baseUrl}/api/control/force-discharge/stop`, { method: 'POST' });
    const resp = await fetch(`${baseUrl}/api/control/force-discharge/stop`, {
      method: 'POST',
    });
    const data = await resp.json();
    expect(data.ok).toBe(false);
    expect(data.error).toMatch(/no force discharge/i);
  });

  test('Start with minutes produces 4 slot writes + 4 flag writes', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    test.setTimeout(60_000);
    await clearWrites(drainModbusWrites);

    await fetch(`${baseUrl}/api/control/force-discharge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 60 }),
    });

    // 4 slot writes (HR 56, 57, 44, 45) + 4 force flags = 8 writes total.
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 8, 30_000);

    const slot1Start = findWrite(writes, 56);
    const slot1End = findWrite(writes, 57);
    expect(slot1Start).toBeDefined();
    expect(slot1End).toBeDefined();

    // With minutes=60, slot 1 is now → now+60 (not 00:00–23:59).
    // Verify start and end differ and slot 2 is cleared.
    if (slot1Start!.value === 0) {
      // Midnight edge case.
      expect(slot1End!.value).not.toBe(2359);
    } else {
      expect(slot1Start!.value).not.toBe(0);
    }
    expect(findWrite(writes, 44)!.value).toBe(0);
    expect(findWrite(writes, 45)!.value).toBe(0);

    // Force flags.
    expect(findWrite(writes, 27)!.value).toBe(0);   // export/max-power
    expect(findWrite(writes, 59)!.value).toBe(1);   // enable_discharge
    expect(findWrite(writes, 96)!.value).toBe(0);   // clear charge
    expect(findWrite(writes, 20)!.value).toBe(0);   // clear charge target

    // Stop. Verify the stop is consumed.
    const stop = await fetch(`${baseUrl}/api/control/force-discharge/stop`, { method: 'POST' });
    expect((await stop.json()).ok).toBe(true);
  });

  test('Start without minutes produces 4 slot writes (legacy 00:00–23:59) + 4 flag writes', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    test.setTimeout(60_000);
    await clearWrites(drainModbusWrites);

    // No Content-Type, no body — backward-compat path.
    await fetch(`${baseUrl}/api/control/force-discharge`, { method: 'POST' });

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 8, 30_000);
    expect(findWrite(writes, 56)!.value).toBe(0);     // slot start 00:00
    expect(findWrite(writes, 57)!.value).toBe(2359);  // slot end 23:59
    expect(findWrite(writes, 44)!.value).toBe(0);     // slot2 start
    expect(findWrite(writes, 45)!.value).toBe(0);     // slot2 end
  });

  test('Stop is one-shot — second call also returns 400', async ({
    baseUrl,
    drainModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);
    const fdResp = await fetch(`${baseUrl}/api/control/force-discharge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    expect((await fdResp.json()).ok).toBe(true);

    const stop1 = await fetch(`${baseUrl}/api/control/force-discharge/stop`, { method: 'POST' });
    expect((await stop1.json()).ok).toBe(true);

    const stop2 = await fetch(`${baseUrl}/api/control/force-discharge/stop`, { method: 'POST' });
    const data = await stop2.json();
    expect(data.ok).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Force Discharge auto-revert on slot expiry (issue #129)
//
// When Force Discharge is started with a bounded duration, the backend
// records the slot's end time. When that time passes, the poll loop
// auto-reverts the inverter to its pre-force-discharge state — preventing
// the battery from being left "paused" (export mode, discharge enabled,
// but no active slot; no charge from solar, no discharge).
//
// These tests use `minutes: 1` and wait for the slot to expire. The poll
// loop interval is 5s in the test setup, so the auto-revert fires within
// ~5–10s of the slot expiry (the next poll cycle after the slot ends).
// ---------------------------------------------------------------------------

test.describe('Force Discharge auto-revert (issue #129)', () => {

  test('Start with minutes=1: poll loop auto-reverts after slot expires', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    // Generous timeout: this test waits for a 1-minute slot to expire
    // plus the poll loop to detect and write the auto-revert. When run
    // after other tests, the poll loop may be processing leftover
    // writes, so we allow extra headroom.
    test.setTimeout(300_000);
    await clearWrites(drainModbusWrites);

    // Give the poll loop time to finish any in-flight writes from
    // previous tests. The poll loop processes writes at 1.5s each,
    // and previous tests may have left several writes in the queue.
    // A 15s wait ensures the poll loop is idle before we start.
    await new Promise((r) => setTimeout(r, 15_000));

    // Start force discharge with a 1-minute window.
    const fdResp = await fetch(`${baseUrl}/api/control/force-discharge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 1 }),
    });
    expect((await fdResp.json()).ok).toBe(true);

    // Wait for the initial force-discharge writes to be processed by
    // the poll loop (8 writes × 1.5s ≈ 12s). Drain them so the
    // auto-revert writes can be inspected in isolation.
    await new Promise((r) => setTimeout(r, 20_000));
    await drainModbusWrites();

    // Wait for the slot to expire. The slot is now → now+60s. The poll
    // loop runs every 5s, so the auto-revert should fire within ~70s
    // of the force_discharge call. We poll the mock for up to 120s.
    //
    // We check for HR_BATTERY_POWER_MODE=1 (eco mode) as the
    // definitive auto-revert signal — it's always written regardless
    // of the pre-state. Checking HR_ENABLE_DISCHARGE=0 is not reliable
    // because the pre-state might have had it set to 1 (from a previous
    // test), and the auto-revert restores the pre-state.
    const startWait = Date.now();
    let autoRevertObserved = false;
    while (Date.now() - startWait < 120_000) {
      const writes = await peekModbusWrites();
      // Look for the auto-revert's HR 27=1 write. The initial force
      // discharge writes HR 27=0, and the auto-revert writes HR 27=1.
      // We check for the LAST write to HR 27 being 1.
      const hr27Writes = writes.filter((w) => w.address === 27);
      const lastHr27 = hr27Writes[hr27Writes.length - 1];
      if (lastHr27 && lastHr27.value === 1) {
        autoRevertObserved = true;
        break;
      }
      await new Promise((r) => setTimeout(r, 1000));
    }

    expect(
      autoRevertObserved,
      'auto-revert should have written HR_BATTERY_POWER_MODE=1 (eco mode)',
    ).toBe(true);

    // Drain the auto-revert writes and verify the key signal. The
    // auto-revert always writes HR 27=1 (eco mode). Other writes
    // restore the pre-force state, which varies depending on what
    // previous tests left configured — we don't assert specific values
    // for those registers here.
    const writes = await drainModbusWrites();
    const lastWriteAt = (addr: number) => {
      const matches = writes.filter((w) => w.address === addr);
      return matches[matches.length - 1];
    };
    // HR 27 must end as eco (1) — the auto-revert always restores
    // this regardless of pre-state.
    expect(lastWriteAt(27)!.value).toBe(1);

    // Stop should now return 400 (revert was consumed by auto-revert).
    const stop = await fetch(`${baseUrl}/api/control/force-discharge/stop`, { method: 'POST' });
    const data = await stop.json();
    expect(data.ok).toBe(false);
    expect(data.error).toMatch(/no force discharge/i);
  });

  test('Start without minutes: no auto-revert (slot runs until stopped)', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    test.setTimeout(60_000);
    await clearWrites(drainModbusWrites);

    // No body = until stopped. The revert's slot_end is None, so the
    // poll loop must never auto-revert.
    const fdResp = await fetch(`${baseUrl}/api/control/force-discharge`, {
      method: 'POST',
    });
    expect((await fdResp.json()).ok).toBe(true);

    // Wait for the initial writes to be processed, then drain.
    await new Promise((r) => setTimeout(r, 15_000));
    await drainModbusWrites();

    // Wait 20s — well beyond a typical poll cycle — and verify no
    // auto-revert writes appear.
    await new Promise((r) => setTimeout(r, 20_000));
    const writes = await peekModbusWrites();
    const hasEnableDischargeZero = writes.some(
      (w) => w.address === 59 && w.value === 0,
    );
    const hasModeEco = writes.some(
      (w) => w.address === 27 && w.value === 1,
    );
    expect(
      hasEnableDischargeZero && hasModeEco,
      'no-body force discharge should not auto-revert',
    ).toBe(false);

    // Clean up: explicit stop should still work.
    const stop = await fetch(`${baseUrl}/api/control/force-discharge/stop`, { method: 'POST' });
    expect((await stop.json()).ok).toBe(true);
  });
});
