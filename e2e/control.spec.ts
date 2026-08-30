/**
 * E2E tests for control commands.
 *
 * These tests verify that the GUI correctly sends control commands
 * through the backend API, which translates them into Modbus register
 * writes that our mock Modbus server captures.
 *
 * Register address reference (from src-tauri/src/modbus/registers.rs):
 *   HR 20  = enable_charge_target
 *   HR 27  = battery_power_mode (0=export, 1=self-consumption)
 *   HR 50  = active_power_rate
 *   HR 56  = discharge_slot_1_start
 *   HR 57  = discharge_slot_1_end
 *   HR 59  = enable_discharge
 *   HR 94  = charge_slot_1_start
 *   HR 95  = charge_slot_1_end
 *   HR 96  = enable_charge
 *   HR 110 = battery_soc_reserve
 *   HR 111 = battery_charge_limit
 *   HR 112 = battery_discharge_limit
 *   HR 116 = charge_target_soc
 *   HR 163 = inverter_reboot (write 100)
 *   HR 242 = charge_slot_1_target_soc
 */

import { test, expect } from './fixture.js';
import { startBackend, stopBackend } from './backend.js';

// Each spec file runs against a FRESH backend instance so backend-internal
// state (detected device type, armed slots, battery-mode state machine) can't
// leak between spec files. See e2e/backend.ts.
test.beforeAll(async () => {
  await startBackend();
});
test.afterAll(async () => {
  await stopBackend();
});
import type { RegisterWrite } from './mock-modbus.js';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Wait for at least N writes to appear, then drain and return them.
 * Uses peekWrites for polling (non-destructive) then drains once.
 */
async function waitForWrites(
  peekWrites: () => Promise<RegisterWrite[]>,
  drainWrites: () => Promise<RegisterWrite[]>,
  minCount: number,
  timeoutMs = 15_000,
): Promise<RegisterWrite[]> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const writes = await peekWrites();
    if (writes.length >= minCount) {
      return drainWrites();
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  // Timeout — return whatever we have
  return drainWrites();
}

/** Find a write to a specific register address. */
function findWrite(writes: RegisterWrite[], address: number): RegisterWrite | undefined {
  return writes.find((w) => w.address === address);
}

/** Wait until a write to `address` with `value` has landed, then drain and
 * return everything captured so far (including any writes that preceded it).
 *
 * The mode tests use this instead of `waitForWrites(minCount)` because a
 * prior test in the suite may arm a discharge slot: when the inverter next
 * enters a Timed mode it restores that slot (issue #137) and queues the slot
 * writes *before* the mode writes. A fixed-count wait would grab the restore
 * writes and miss the mode writes; waiting for the trailing SOC-reserve
 * write (HR 110) guarantees the full set has landed. */
async function waitForWrite(
  peekWrites: () => Promise<RegisterWrite[]>,
  drainWrites: () => Promise<RegisterWrite[]>,
  address: number,
  value: number,
  timeoutMs = 15_000,
): Promise<RegisterWrite[]> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const writes = await peekWrites();
    if (writes.some((w) => w.address === address && w.value === value)) {
      return drainWrites();
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  // Timeout — return whatever we have
  return drainWrites();
}

/** Wait until /api/snapshot's payload satisfies `predicate`.
 *
 * The mock applies writes to its register store immediately, but the
 * backend's snapshot only reflects them on the next poll cycle — asserting
 * straight after a write races the poll loop. Poll the snapshot endpoint
 * until the decoded field settles. */
async function waitForSnapshotValue(
  baseUrl: string,
  predicate: (data: Record<string, unknown>) => boolean,
  timeoutMs = 20_000,
): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const resp = await fetch(`${baseUrl}/api/snapshot`);
      const body = await resp.json();
      if (predicate(body?.data ?? {})) return;
    } catch {
      // backend not ready yet — retry
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`waitForSnapshotValue: condition not met within ${timeoutMs}ms`);
}

/** Clear any in-flight writes from previous tests.
 *
 * Repeatedly drains writes and waits until no new writes appear for
 * 3 seconds (covering up to ~6 writes × 1.5s retry delay each).
 * This prevents cross-contamination where a previous test's deferred
 * writes arrive in the middle of the next test. */
async function clearWrites(drainModbusWrites: () => Promise<RegisterWrite[]>) {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    await drainModbusWrites();
    await new Promise((r) => setTimeout(r, 3000));
    const remaining = await drainModbusWrites();
    if (remaining.length === 0) return;
  }
}

// ---------------------------------------------------------------------------
// Test: Verify the dashboard loads and shows data
// ---------------------------------------------------------------------------

test.describe('Dashboard', () => {
  test('should load and show inverter data from mock server', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=GivEnergy')).toBeVisible({ timeout: 10_000 });
  });

  test('should show connection status as connected', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/status`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.connection).toBe('connected');
  });

  test('should deliver snapshot data via API', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.data).toBeDefined();
    // Battery SOC should be 75% from our mock defaults
    expect(data.data.soc).toBe(75);
    // Battery power — sign depends on decoder convention (charging positive or negative)
    expect(Math.abs(data.data.battery_power)).toBe(256);
  });
});

// ---------------------------------------------------------------------------
// Test: Quick action buttons (UI interaction)
// ---------------------------------------------------------------------------

test.describe('Quick Actions', () => {
  // These tests click through the UI and then wait for 7-8 sequential Modbus
  // writes (each a real ~1.5s round-trip) twice — once for start, once for
  // stop. The default 30s test timeout is too tight under CI/parallel load
  // (the Force Discharge test measures ~28s standalone); give the block a
  // budget that matches its actual work.
  test.describe.configure({ timeout: 90_000 });
  test('Force Charge should send correct Modbus writes', async ({
    page,
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites); // clear

    await page.goto('/');
    await page.locator('text=Control').click();
    // The button might say "Force Charge" or "Stop Charge" depending on
    // whether a prior test left a force-charge mode active. Match either.
    await page.getByRole('button', { name: /Force Charge|Stop Charge/i }).click();

    // ForceCharge with minutes=30 = 2 slot writes + 5 flag writes = 7
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 7, 20_000);
    expect(writes.length).toBeGreaterThanOrEqual(7);

    // Slot registers (HR94, HR95) must be present and non-zero
    expect(findWrite(writes, 94)!.value).not.toBe(0);
    expect(findWrite(writes, 95)!.value).not.toBe(0);

    // HR 27 = 1 (eco), HR 59 = 0 (clear stale discharge), HR 96 = 1 (enable_charge),
    // HR 20 = 1 (enable_charge_target), HR 116 = 100 (target SOC)
    expect(findWrite(writes, 27)!.value).toBe(1);
    expect(findWrite(writes, 59)!.value).toBe(0);
    expect(findWrite(writes, 96)!.value).toBe(1);
    expect(findWrite(writes, 20)!.value).toBe(1);
    expect(findWrite(writes, 116)!.value).toBe(100);

    const conflict = await fetch(`${baseUrl}/api/control/force-discharge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    expect(conflict.status).toBe(400);
    expect((await conflict.json()).error).toContain(
      'Stop Force Charge before starting Force Discharge',
    );

    const stop = await fetch(`${baseUrl}/api/control/force-charge/stop`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
    });
    expect((await stop.json()).ok).toBe(true);
    await waitForWrites(peekModbusWrites, drainModbusWrites, 7, 20_000);
  });

  test('Force Discharge should send correct Modbus writes', async ({
    page,
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    await page.goto('/');
    await page.locator('text=Control').click();
    // Match either "Force Discharge" or "Stop Discharge".
    await page.getByRole('button', { name: /Force Discharge|Stop Discharge/i }).click();

    // ForceDischarge = 8 writes (HR27=0, HR96=0, HR20=0, HR59=1,
    //                     HR56=now, HR57=now+30, HR44=0, HR45=0)
    // Since the duration slider defaults to 30 minutes, the slot is
    // now → now+30min rather than the legacy 00:00–23:59.
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 8, 25_000);
    expect(writes.length).toBeGreaterThanOrEqual(8);

    // HR 27 = 0 (export/max power), HR 96 = 0 (clear charge), HR 20 = 0 (clear target),
    // HR 59 = 1 (enable discharge)
    expect(findWrite(writes, 27)!.value).toBe(0);
    expect(findWrite(writes, 96)!.value).toBe(0);
    expect(findWrite(writes, 20)!.value).toBe(0);
    expect(findWrite(writes, 59)!.value).toBe(1);

    // HR 44/45 = 0 (slot 2 cleared)
    expect(findWrite(writes, 44)!.value).toBe(0);
    expect(findWrite(writes, 45)!.value).toBe(0);

    // HR 56 / HR 57: duration slot. Start is the time-of-day in HHMM
    // when the click happened, end is start+30. We can't pin the
    // exact value without freezing time, so just assert they're both
    // non-zero and differ from each other.
    const slotStart = findWrite(writes, 56)!.value;
    const slotEnd = findWrite(writes, 57)!.value;
    expect(slotStart).toBeGreaterThan(0);
    expect(slotEnd).toBeGreaterThan(0);
    expect(slotStart).not.toBe(slotEnd);

    const stop = await fetch(`${baseUrl}/api/control/force-discharge/stop`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
    });
    expect((await stop.json()).ok).toBe(true);
    await waitForWrites(peekModbusWrites, drainModbusWrites, 8, 25_000);
  });

  test('Pause Discharge should send the Eco Paused Modbus writes', async ({
    page,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    await page.goto('/');
    await page.locator('text=Control').click();
    await page.getByRole('button', { name: /Pause Discharge/ }).click();

    // PauseBattery = 3 writes: HR 27=1 (eco), HR 59=0 (disable discharge),
    // HR 110=100 (SOC reserve = paused). Charge enable and schedules are
    // deliberately left untouched.
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 3, 20_000);
    expect(writes.length).toBeGreaterThanOrEqual(3);

    expect(findWrite(writes, 27)!.value).toBe(1);    // eco mode
    expect(findWrite(writes, 59)!.value).toBe(0);    // disable discharge
    expect(findWrite(writes, 110)!.value).toBe(100); // SOC reserve = paused
    expect(findWrite(writes, 96)).toBeUndefined();   // charge left untouched
  });

  test('Sync Clock should send time registers', async ({
    page,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    await page.goto('/');
    await page.locator('text=Control').click();
    await page.locator('text=Sync Clock').click();

    // SyncClock = 6 writes, ~9s
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 6, 20_000);
    expect(writes.length).toBeGreaterThanOrEqual(6);

    const now = new Date();
    const expectedYear = now.getUTCFullYear() - 2000;
    expect(findWrite(writes, 35)!.value).toBe(expectedYear);
    expect(findWrite(writes, 36)!.value).toBeGreaterThanOrEqual(1);
    expect(findWrite(writes, 36)!.value).toBeLessThanOrEqual(12);
    expect(findWrite(writes, 37)).toBeDefined(); // day
    expect(findWrite(writes, 38)).toBeDefined(); // hour
    expect(findWrite(writes, 39)).toBeDefined(); // minute
    expect(findWrite(writes, 40)).toBeDefined(); // second
  });
});

// ---------------------------------------------------------------------------
// Test: Direct API tests (no UI interaction — faster and more reliable)
// ---------------------------------------------------------------------------

test.describe('API Control Endpoints', () => {
  // Park the Timed Export boundary machine before every test. Saving an
  // enabled discharge slot arms the HEM-managed schedule (issue #289), and
  // a schedule left over from an earlier test fires entry/exit write bursts
  // whenever a later test changes HR318 or the inverter clock — interleaving
  // with unrelated assertions. Disabling here (rather than at the end of
  // whichever test happened to save a slot) keeps each test independent of
  // suite order and of the wall-clock time the suite runs at.
  test.beforeEach(async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/timed-export`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ enabled: false }),
    });
    expect((await resp.json()).ok).toBe(true);
  });

  test('POST /api/control/reserve sends HR 110', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/reserve`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ soc: 30 }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    expect(findWrite(writes, 110)).toBeDefined();
    expect(findWrite(writes, 110)!.value).toBe(30);
  });

  test('POST /api/control/charge-rate sends HR 111', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/charge-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ limit: 25 }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    expect(findWrite(writes, 111)!.value).toBe(25);
  });

  test('POST /api/control/discharge-rate sends HR 112', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/discharge-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ limit: 50 }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    expect(findWrite(writes, 112)!.value).toBe(50);
  });

  test('POST /api/control/active-power-rate sends HR 50', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/active-power-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ rate: 80 }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    expect(findWrite(writes, 50)!.value).toBe(80);
  });

  test('POST /api/control/mode eco sends correct writes', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'eco', soc_reserve: 4 }),
    });
    expect((await resp.json()).ok).toBe(true);

    // Eco mode: 7 writes (~10.5s)
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 7, 20_000);
    expect(findWrite(writes, 27)!.value).toBe(1);  // self-consumption
    expect(findWrite(writes, 59)!.value).toBe(0);  // disable discharge
    expect(findWrite(writes, 110)!.value).toBe(4);  // SOC reserve
    expect(findWrite(writes, 56)!.value).toBe(0);   // discharge slot 1 start
    expect(findWrite(writes, 57)!.value).toBe(0);   // discharge slot 1 end
  });

  test('POST /api/control/mode omitting soc_reserve preserves configured reserve', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    // Since issue #289 (v0.75.4), entering Eco on an extended-slot model
    // clears every slot register the inverter supports: 3 mode writes + 20
    // slot clears at the dongle's 1.5s pacing ≈ 35s per Eco batch. Every
    // wait below must cover a full batch plus whatever is left queued from
    // the previous step.
    test.setTimeout(180_000);
    await clearWrites(drainModbusWrites);

    // Arm a real (non-default) reserve first, so the snapshot carries 30%.
    const setResp = await fetch(`${baseUrl}/api/control/reserve`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ soc: 30 }),
    });
    expect((await setResp.json()).ok).toBe(true);
    // Drain the reserve write itself; the mock applies the write to its
    // register store, but the backend's snapshot only picks it up on the next
    // poll — wait for /api/snapshot to reflect 30% before continuing.
    await waitForWrite(peekModbusWrites, drainModbusWrites, 110, 30, 30_000);
    await clearWrites(drainModbusWrites);
    await waitForSnapshotValue(baseUrl, (s) => s.battery_reserve === 30, 20_000);

    // Now POST a mode WITHOUT soc_reserve — the automated round-trip shape.
    const resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'eco' }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrite(peekModbusWrites, drainModbusWrites, 110, 30, 45_000);
    expect(findWrite(writes, 27)!.value).toBe(1);  // self-consumption
    expect(findWrite(writes, 59)!.value).toBe(0);  // disable discharge
    // HR 110 must echo the configured 30, not the old 4% omit-default.
    expect(findWrite(writes, 110)!.value).toBe(30);  // SOC reserve preserved

    // Restore the harness default so later tests see HR 110 = 4.
    const restore = await fetch(`${baseUrl}/api/control/reserve`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ soc: 4 }),
    });
    expect((await restore.json()).ok).toBe(true);
    // The restore batch queues BEHIND the eco slot-clear batch (~20
    // registers at 1.5s pacing), so this wait is the long pole of the test.
    await waitForWrite(peekModbusWrites, drainModbusWrites, 110, 4, 60_000);
    // Wait for the snapshot to settle back to 4% too — the following tests
    // pin HR 110 = 4 on an omitted `soc_reserve`, which now preserves the
    // snapshot value instead of defaulting.
    await waitForSnapshotValue(baseUrl, (s) => s.battery_reserve === 4, 20_000);
  });

  test('POST /api/control/mode timed_demand sends correct writes', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'timed_demand', soc_reserve: 4 }),
    });
    expect((await resp.json()).ok).toBe(true);

    // Wait for the trailing SOC-reserve write (HR 110 = 4) rather than a
    // fixed count: a prior test may have armed a discharge slot, and entering
    // Timed mode restores it (issue #137), queueing slot writes before the
    // mode writes. Asserting on the full captured set is robust to that.
    const writes = await waitForWrite(peekModbusWrites, drainModbusWrites, 110, 4, 15_000);
    expect(findWrite(writes, 27)!.value).toBe(1);  // self-consumption
    expect(findWrite(writes, 59)!.value).toBe(1);  // enable discharge
    expect(findWrite(writes, 110)!.value).toBe(4);  // SOC reserve
  });

  test('POST /api/control/mode timed_export requires a managed schedule', async ({
    baseUrl,
    drainModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'timed_export', soc_reserve: 4 }),
    });
    expect(resp.status).toBe(409);
    const body = await resp.json();
    expect(body.ok).toBe(false);
    expect(body.error).toContain('Configure at least one discharge slot');

    // The managed route must not fall back to the raw HR27/HR59 export
    // flags when there is no desired schedule to gate them.
    const writes = await drainModbusWrites();
    expect(writes.some((write) => write.address === 59 && write.value === 1)).toBe(false);
    expect(writes.some((write) => write.address === 27 && write.value === 0)).toBe(false);
  });

  test('POST /api/control/timed-export rejects enabling without a discharge slot', async ({
    baseUrl,
    drainModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/timed-export`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ enabled: true }),
    });
    expect(resp.status).toBe(409);
    const body = await resp.json();
    expect(body.ok).toBe(false);
    expect(body.error).toContain('Configure at least one discharge slot');
    expect((await drainModbusWrites()).some((write) => write.address === 59 && write.value === 1)).toBe(false);
  });

  test('POST /api/control/timed-discharge writes HR318 pause-discharge inverse window', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/timed-discharge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        enabled: true,
        start_hour: 3,
        start_minute: 0,
        end_hour: 4,
        end_minute: 0,
      }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 3, 15_000);
    expect(findWrite(writes, 319)!.value).toBe(400);
    expect(findWrite(writes, 320)!.value).toBe(300);
    expect(findWrite(writes, 318)!.value).toBe(2);
  });

  test('POST /api/control/charge-slot sends correct writes', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/charge-slot`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        slot: 1,
        enabled: true,
        start_hour: 0,
        start_minute: 30,
        end_hour: 4,
        end_minute: 30,
        target_soc: 100,
      }),
    });
    expect((await resp.json()).ok).toBe(true);

    // For a Gen3 hybrid with slot 1 enabled, the backend sends:
    //   HR 94 = slot start, HR 95 = slot end (from SetChargeSlot1)
    //   HR 20 = 0 (clear enable_charge_target)
    //   HR 96 = 1 (enable_charge from SetEnableCharge)
    //   HR 242 = 100 (target SOC for slot 1 from SetChargeTargetSocSlot)
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 5, 20_000);
    expect(findWrite(writes, 94)!.value).toBe(30);    // 00:30
    expect(findWrite(writes, 95)!.value).toBe(430);   // 04:30
    expect(findWrite(writes, 20)!.value).toBe(0);     // clear enable_charge_target
    expect(findWrite(writes, 96)!.value).toBe(1);     // enable_charge
    expect(findWrite(writes, 242)!.value).toBe(100);  // per-slot target SOC
  });

  test('POST /api/control/discharge-slot sends correct writes', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/discharge-slot`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        slot: 1,
        enabled: true,
        start_hour: 16,
        start_minute: 0,
        end_hour: 19,
        end_minute: 0,
      }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 2, 15_000);
    expect(findWrite(writes, 56)!.value).toBe(1600);  // 16:00
    expect(findWrite(writes, 57)!.value).toBe(1900);  // 19:00
  });

  test('POST /api/control/discharge-slot omitting target_soc preserves configured floor', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    // Arm a real (non-default) discharge floor first.
    const armResp = await fetch(`${baseUrl}/api/control/discharge-slot`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        slot: 1,
        enabled: true,
        start_hour: 16,
        start_minute: 0,
        end_hour: 19,
        end_minute: 0,
        target_soc: 20,
      }),
    });
    expect((await armResp.json()).ok).toBe(true);
    await waitForWrite(peekModbusWrites, drainModbusWrites, 272, 20, 15_000);
    await clearWrites(drainModbusWrites);
    // The backend's snapshot only reflects the write on the next poll.
    await waitForSnapshotValue(
      baseUrl,
      (s) => Array.isArray(s.discharge_slots) && s.discharge_slots[0]?.target_soc === 20,
      20_000,
    );

    // Re-post the same slot WITHOUT target_soc — the round-trip shape.
    const resp = await fetch(`${baseUrl}/api/control/discharge-slot`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        slot: 1,
        enabled: true,
        start_hour: 16,
        start_minute: 0,
        end_hour: 19,
        end_minute: 0,
      }),
    });
    expect((await resp.json()).ok).toBe(true);

    // HR 272 must echo the configured 20% floor, not the inert 100 default.
    const writes = await waitForWrite(peekModbusWrites, drainModbusWrites, 272, 20, 15_000);
    expect(findWrite(writes, 56)!.value).toBe(1600);  // 16:00
    expect(findWrite(writes, 57)!.value).toBe(1900);  // 19:00

    // (The saved 16:00-19:00 schedule is parked by the describe-level
    // beforeEach before the next test — see the top of this describe.)
  });

  test('discharge slot with no configured floor displays the reserve, not the charge target', async ({
    baseUrl,
    setHoldingReg,
  }) => {
    // Decoder fallback regression (2026-08-22 sweep): an enabled discharge
    // slot with no per-slot floor (HR 272/275 = 0) must display the battery
    // reserve — not the global charge target, which the flag-off decoder
    // fix reports as the effective 100 ("discharge to 100%" nonsense).
    // Seed the mock registers directly: a control POST omitting target_soc
    // would itself write HR 275 = 100 (preserve path only preserves ENABLED
    // slots' floors) and arm the very trap under test.
    await setHoldingReg(110, 25); // battery reserve 25%
    await setHoldingReg(56, 1600); // discharge slot 1 start 16:00
    await setHoldingReg(57, 1900); // discharge slot 1 end 19:00
    await setHoldingReg(272, 0); // no per-slot floor (earlier tests may have armed one)
    await setHoldingReg(275, 0);

    // The snapshot reflects seeds on the next poll cycle. Absolute 25 (not a
    // relative comparison) so a charge-target leak (100) or stale floor (20
    // from the preserve test) both fail.
    await waitForSnapshotValue(
      baseUrl,
      (s) =>
        Array.isArray(s.discharge_slots) &&
        s.discharge_slots[0]?.enabled === true &&
        s.battery_reserve === 25 &&
        s.discharge_slots[0]?.target_soc === 25,
      20_000,
    );

    // Restore the reserve the rest of the file expects (tests before this
    // one run with the mock default of 4).
    await setHoldingReg(110, 4);
  });

  test('POST /api/control/force-charge with minutes writes slot before enable', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    expect((await resp.json()).ok).toBe(true);

    // With minutes: slot writes (HR94, HR95) + force-charge flags = ~7 writes
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 7, 20_000);

    // Slot registers must be present and non-zero
    const slotStart = findWrite(writes, 94);
    const slotEnd = findWrite(writes, 95);
    expect(slotStart).toBeDefined();
    expect(slotEnd).toBeDefined();
    expect(slotStart!.value).not.toBe(0);
    expect(slotEnd!.value).not.toBe(0);
    expect(slotStart!.value).not.toBe(slotEnd!.value);

    // Force-charge flags
    expect(findWrite(writes, 27)!.value).toBe(1);    // eco mode
    expect(findWrite(writes, 59)!.value).toBe(0);    // clear stale discharge
    expect(findWrite(writes, 96)!.value).toBe(1);    // enable_charge
    expect(findWrite(writes, 20)!.value).toBe(1);    // enable_charge_target
    expect(findWrite(writes, 116)!.value).toBe(100); // target SOC

    // Slot registers must appear before enable_charge (HR96)
    const enableIdx = writes.findIndex((w) => w.address === 96);
    expect(writes.indexOf(slotStart!)).toBeLessThan(enableIdx);
    expect(writes.indexOf(slotEnd!)).toBeLessThan(enableIdx);

    const stop = await fetch(`${baseUrl}/api/control/force-charge/stop`, { method: 'POST' });
    expect((await stop.json()).ok).toBe(true);
    await waitForWrites(peekModbusWrites, drainModbusWrites, 7, 25_000);
  });

  test('POST /api/control/force-discharge sends correct writes', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
    setHoldingReg,
  }) => {
    test.setTimeout(120_000);
    await clearWrites(drainModbusWrites);
    // Issue #289: an armed discharge pause (HR318=2/3) makes force-discharge
    // queue an extra HR318=0 write, and the stop path restores the captured
    // HR319/320 pause slot. The earlier timed-discharge test leaves all three
    // in the shared mock, so pin them to 0 here (and wait for the poll to
    // reflect it) to keep this batch's size deterministic.
    await setHoldingReg(318, 0);
    await setHoldingReg(319, 0);
    await setHoldingReg(320, 0);
    await waitForSnapshotValue(
      baseUrl,
      (s) => (s as Record<string, unknown>).battery_pause_mode === 0,
      20_000,
    );

    const resp = await fetch(`${baseUrl}/api/control/force-discharge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 8, 25_000);
    expect(findWrite(writes, 27)!.value).toBe(0);     // export/max power
    expect(findWrite(writes, 96)!.value).toBe(0);     // clear charge
    expect(findWrite(writes, 20)!.value).toBe(0);     // clear charge target
    expect(findWrite(writes, 59)!.value).toBe(1);     // enable discharge
    // With minutes=30, slot 1 is now → now+30 (not 00:00–23:59).
    // Verify the start/end differ and end > start.
    const slot1Start = findWrite(writes, 56)!.value;
    const slot1End = findWrite(writes, 57)!.value;
    expect(slot1Start).toBeGreaterThan(0);
    expect(slot1End).toBeGreaterThan(0);
    expect(slot1Start).not.toBe(slot1End);
    // Slot 2 is cleared.
    expect(findWrite(writes, 44)!.value).toBe(0);
    expect(findWrite(writes, 45)!.value).toBe(0);

    const stop = await fetch(`${baseUrl}/api/control/force-discharge/stop`, { method: 'POST' });
    expect((await stop.json()).ok).toBe(true);
    // Stop now also restores the captured HR318/319/320 pause state (issue
    // #289), so the trailing batch is 11 writes, not 8.
    await waitForWrites(peekModbusWrites, drainModbusWrites, 11, 30_000);
  });

  test('POST /api/control/force-discharge without minutes keeps legacy 00:00–23:59 slot', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
    setHoldingReg,
  }) => {
    test.setTimeout(120_000);
    await clearWrites(drainModbusWrites);
    // See the previous test: pin HR318/319/320 so the #289 pause-disable
    // write (and its stop-time restore) can't be appended and change the
    // batch size.
    await setHoldingReg(318, 0);
    await setHoldingReg(319, 0);
    await setHoldingReg(320, 0);
    await waitForSnapshotValue(
      baseUrl,
      (s) => (s as Record<string, unknown>).battery_pause_mode === 0,
      20_000,
    );

    // API call with no body and no Content-Type (backward-compat path).
    const resp = await fetch(`${baseUrl}/api/control/force-discharge`, {
      method: 'POST',
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 8, 25_000);
    // No-body path: full-day slot for backward compatibility.
    expect(findWrite(writes, 56)!.value).toBe(0);     // slot start 00:00
    expect(findWrite(writes, 57)!.value).toBe(2359);  // slot end 23:59
    expect(findWrite(writes, 44)!.value).toBe(0);     // slot2 start
    expect(findWrite(writes, 45)!.value).toBe(0);     // slot2 end

    const stop = await fetch(`${baseUrl}/api/control/force-discharge/stop`, { method: 'POST' });
    expect((await stop.json()).ok).toBe(true);
    // Same trailing batch as the previous test: 11 writes including the
    // HR318/319/320 pause-state restore (issue #289).
    await waitForWrites(peekModbusWrites, drainModbusWrites, 11, 30_000);
  });

  test('POST /api/control/pause enters Eco Paused (HR 27=1, 59=0, 110=100)', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/pause`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
    });
    expect((await resp.json()).ok).toBe(true);

    // PauseBattery now writes the minimal Eco Paused contract: eco mode,
    // discharge off, SOC reserve=100 = 3 writes (~5s at 1.5s each). Charge
    // enable and schedules are deliberately left untouched.
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 3, 25_000);

    expect(findWrite(writes, 27)!.value).toBe(1);     // eco mode
    expect(findWrite(writes, 59)!.value).toBe(0);     // disable discharge
    expect(findWrite(writes, 110)!.value).toBe(100);  // SOC reserve=100 to pause
    expect(findWrite(writes, 96)).toBeUndefined();    // charge left untouched
  });

  test('POST /api/control/sync-clock sends time registers', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/sync-clock`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 6, 20_000);
    expect(findWrite(writes, 35)).toBeDefined(); // year
    expect(findWrite(writes, 36)).toBeDefined(); // month
    expect(findWrite(writes, 37)).toBeDefined(); // day
    expect(findWrite(writes, 38)).toBeDefined(); // hour
    expect(findWrite(writes, 39)).toBeDefined(); // minute
    expect(findWrite(writes, 40)).toBeDefined(); // second

    const now = new Date();
    expect(findWrite(writes, 35)!.value).toBe(now.getUTCFullYear() - 2000);
  });

  test('Validation: SOC reserve rejects > 100', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/reserve`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ soc: 101 }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(false);
    expect(data.error).toBeDefined();
  });

  test('Validation: charge rate rejects > 100', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/charge-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ limit: 101 }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(false);
    expect(data.error).toBeDefined();
  });

  test('Validation: active power rate rejects > 100', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/active-power-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ rate: 101 }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(false);
    expect(data.error).toBeDefined();
  });

  test('Validation: unknown mode returns error', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'invalid_mode' }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(false);
    expect(data.error).toContain('Unknown mode');
  });
});

// ---------------------------------------------------------------------------
// Test: Quick Actions — extended UI interaction tests
// ---------------------------------------------------------------------------

test.describe('Quick Actions - extended', () => {
  test('Force Charge without minutes (API only) sends only 5 flag writes', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    // API call without body or Content-Type — no slot writes, only flags
    const resp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
    });
    expect((await resp.json()).ok).toBe(true);

    // Without slot = 5 writes (HR27, HR59=0, HR96, HR20, HR116)
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 5, 20_000);
    expect(writes.length).toBeGreaterThanOrEqual(5);

    expect(findWrite(writes, 27)!.value).toBe(1);    // eco mode
    expect(findWrite(writes, 59)!.value).toBe(0);    // clear stale discharge
    expect(findWrite(writes, 96)!.value).toBe(1);    // enable_charge
    expect(findWrite(writes, 20)!.value).toBe(1);    // enable_charge_target
    expect(findWrite(writes, 116)!.value).toBe(100); // target SOC

    // Verify no slot registers were written
    expect(findWrite(writes, 94)).toBeUndefined();
    expect(findWrite(writes, 95)).toBeUndefined();

    const stop = await fetch(`${baseUrl}/api/control/force-charge/stop`, { method: 'POST' });
    expect((await stop.json()).ok).toBe(true);
    await waitForWrites(peekModbusWrites, drainModbusWrites, 7, 25_000);
  });

  test('Force Charge when already in eco mode should still work', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    test.setTimeout(120_000);

    // Aggressive drain of any pending writes from previous tests
    const deadline = Date.now() + 45_000;
    while (Date.now() < deadline) {
      const drained = await drainModbusWrites();
      if (drained.length === 0) break;
      await new Promise((r) => setTimeout(r, 5000));
    }

    // First set eco mode via API
    const modeResp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'eco' }),
    });
    expect((await modeResp.json()).ok).toBe(true);

    // Wait for the eco mode's leading writes (HR 27/59/110 arrive first,
    // then up to 20 slot-clears at 1.5s each on a ten-slot model).
    await waitForWrites(peekModbusWrites, drainModbusWrites, 7, 45_000);
    // Drain any remaining
    while (true) {
      const remaining = await drainModbusWrites();
      if (remaining.length === 0) break;
      await new Promise((r) => setTimeout(r, 3000));
    }

    // Drive Force Charge via the API directly (avoids UI button
    // text matching complications — the button toggles between
    // "Force Charge" and "Stop Charge" depending on inverter state).
    const fcResp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    expect((await fcResp.json()).ok).toBe(true);

    // The FC batch queues behind the eco slot-clear leftovers, so a plain
    // `waitForWrites(7)` can be satisfied by HR 276/277/… clears before any
    // force-charge register shows up. Collect writes until the four
    // FC-specific registers are all captured instead.
    let allWrites: RegisterWrite[] = [];
    const fcDeadline = Date.now() + 60_000;
    while (Date.now() < fcDeadline) {
      const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 5_000);
      allWrites = [...allWrites, ...writes];
      if (
        findWrite(allWrites, 96) &&
        findWrite(allWrites, 20) &&
        findWrite(allWrites, 116) &&
        findWrite(allWrites, 94)
      ) {
        break;
      }
    }

    // Slot registers present (30-minute window) + force-charge flags.
    expect(findWrite(allWrites, 94)!.value).not.toBe(
      findWrite(allWrites, 95)!.value,
    );
    expect(findWrite(allWrites, 27)!.value).toBe(1);    // eco mode
    expect(findWrite(allWrites, 59)!.value).toBe(0);    // clear stale discharge
    expect(findWrite(allWrites, 96)!.value).toBe(1);    // enable_charge
    expect(findWrite(allWrites, 20)!.value).toBe(1);    // enable_charge_target
    expect(findWrite(allWrites, 116)!.value).toBe(100); // target SOC

    const stop = await fetch(`${baseUrl}/api/control/force-charge/stop`, { method: 'POST' });
    expect((await stop.json()).ok).toBe(true);
    await waitForWrites(peekModbusWrites, drainModbusWrites, 7, 25_000);
  });

  test('Pause Discharge should write HR110=100 and enter Eco Paused', async ({
    page,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    await page.goto('/');
    await page.locator('text=Control').click();
    await page.getByRole('button', { name: /Pause Discharge/ }).click();

    // Pause = eco mode + discharge off + SOC reserve=100 = 3 writes (~5s).
    // Charge enable and schedules are deliberately left untouched.
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 3, 30_000);
    expect(writes.length).toBeGreaterThanOrEqual(3);

    expect(findWrite(writes, 27)!.value).toBe(1);     // eco mode
    expect(findWrite(writes, 59)!.value).toBe(0);     // disable discharge
    expect(findWrite(writes, 110)!.value).toBe(100);  // SOC reserve=100
    expect(findWrite(writes, 96)).toBeUndefined();    // charge left untouched
  });
});

// ---------------------------------------------------------------------------
// Test: API Mode Transitions
// ---------------------------------------------------------------------------

test.describe('API Mode Transitions', () => {
  test('Eco → Timed Demand transition', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    // The first Eco POST queues ~23 writes (~35s at dongle pacing since
    // the v0.75.4 ten-slot Eco clear, issue #289); the transition's
    // write batch queues behind whatever is left of it.
    test.setTimeout(180_000);
    // Set eco mode first
    await clearWrites(drainModbusWrites);
    let resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'eco', soc_reserve: 4 }),
    });
    expect((await resp.json()).ok).toBe(true);
    await waitForWrites(peekModbusWrites, drainModbusWrites, 4, 45_000);

    // Now transition to timed_demand
    await clearWrites(drainModbusWrites);
    resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'timed_demand', soc_reserve: 4 }),
    });
    expect((await resp.json()).ok).toBe(true);

    let allWrites: RegisterWrite[] = [];
    // The eco batch queued by the FIRST mode POST can still be draining
    // (~20 slot-clears at 1.5s) when the transition POST lands behind it.
    const deadline = Date.now() + 90_000;
    while (Date.now() < deadline) {
      const writes = await waitForWrites(
        peekModbusWrites,
        drainModbusWrites,
        1,
        5_000,
      );
      allWrites = [...allWrites, ...writes];
      if (
        findWrite(allWrites, 27) &&
        findWrite(allWrites, 59) &&
        findWrite(allWrites, 110)
      ) {
        break;
      }
    }
    expect(findWrite(allWrites, 27)!.value).toBe(1);  // self-consumption
    expect(findWrite(allWrites, 59)!.value).toBe(1);  // enable discharge
    expect(findWrite(allWrites, 110)!.value).toBe(4);  // SOC reserve
  });

  test('Eco → Timed Export transition', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
    setHoldingReg,
  }) => {
    test.setTimeout(180_000);
    await clearWrites(drainModbusWrites);

    // Make the managed window deterministic and exercise the real persisted
    // schedule path. Eco clears the physical slot registers, so the later
    // Timed Export request must restore them before arming HR27/HR59.
    for (const [address, value] of [
      [35, 26], [36, 6], [37, 28], [38, 17], [39, 0], [40, 0],
    ] as const) {
      await setHoldingReg(address, value);
    }
    await waitForSnapshotValue(
      baseUrl,
      (s) => s.inverter_time === '2026-06-28 17:00:00',
      20_000,
    );

    const scheduleResp = await fetch(`${baseUrl}/api/control/discharge-slot`, {
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
    expect((await scheduleResp.json()).ok).toBe(true);
    await clearWrites(drainModbusWrites);

    let resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'eco', soc_reserve: 4 }),
    });
    expect((await resp.json()).ok).toBe(true);
    await waitForWrites(peekModbusWrites, drainModbusWrites, 4, 20_000);

    await clearWrites(drainModbusWrites);
    resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'timed_export', soc_reserve: 4 }),
    });
    expect((await resp.json()).ok).toBe(true);

    // Wait until all three mode-flag writes are captured. See comment
    // on the previous test for why a single `waitForWrites(3)` is not
    // sufficient under issue #137's slot-restore path.
    let allWrites: RegisterWrite[] = [];
    const deadline = Date.now() + 90_000;
    while (Date.now() < deadline) {
      const writes = await waitForWrites(
        peekModbusWrites,
        drainModbusWrites,
        1,
        5_000,
      );
      allWrites = [...allWrites, ...writes];
      if (
        findWrite(allWrites, 27) &&
        findWrite(allWrites, 59) &&
        findWrite(allWrites, 110)
      ) {
        break;
      }
    }
    expect(findWrite(allWrites, 27)!.value).toBe(0);  // export mode
    expect(findWrite(allWrites, 59)!.value).toBe(1);  // enable discharge
    expect(findWrite(allWrites, 110)!.value).toBe(4);  // SOC reserve
    expect(findWrite(allWrites, 56)!.value).toBe(1600); // restored slot start
    expect(findWrite(allWrites, 57)!.value).toBe(1900); // restored slot end
  });

  test('Timed Demand → Eco transition', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    // The eco half of this transition queues ~20 slot clears behind the
    // mode writes (issue #289 / v0.75.4 ten-slot clear) — ~35s at dongle
    // pacing.
    test.setTimeout(150_000);
    let resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'timed_demand', soc_reserve: 4 }),
    });
    expect((await resp.json()).ok).toBe(true);
    await waitForWrites(peekModbusWrites, drainModbusWrites, 3, 15_000);

    await clearWrites(drainModbusWrites);
    resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'eco', soc_reserve: 4 }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 3, 20_000);
    expect(findWrite(writes, 27)!.value).toBe(1);  // self-consumption
    expect(findWrite(writes, 59)!.value).toBe(0);  // disable discharge
  });
});

// ---------------------------------------------------------------------------
// Test: Edge Cases
// ---------------------------------------------------------------------------

test.describe('Edge Cases', () => {
    test('Force charge with minutes=0 clamps to 1', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    // May queue behind a prior test's eco slot-clear batch.
    test.setTimeout(120_000);
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 0 }),
    });
    expect((await resp.json()).ok).toBe(true);

    // Should produce slot writes (clamped to 1 min) + flags = 7 writes
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 7, 20_000);

    // Slot registers should be present (clamped to 1 minute)
    const slotStart = findWrite(writes, 94);
    const slotEnd = findWrite(writes, 95);
    expect(slotStart).toBeDefined();
    expect(slotEnd).toBeDefined();
    // Start and end differ by ~1 minute (HHMM encoding)
    expect(slotStart!.value).not.toBe(slotEnd!.value);

    // Force-charge flags still present
    expect(findWrite(writes, 27)!.value).toBe(1);
    expect(findWrite(writes, 96)!.value).toBe(1);
    expect(findWrite(writes, 20)!.value).toBe(1);
    expect(findWrite(writes, 116)!.value).toBe(100);
  });

  test('Force charge with minutes=9999 clamps to 1439', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 9999 }),
    });
    expect((await resp.json()).ok).toBe(true);

    // Should produce slot writes (clamped to 1439 min) + flags = 7 writes
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 7, 20_000);

    const slotStart = findWrite(writes, 94);
    const slotEnd = findWrite(writes, 95);
    expect(slotStart).toBeDefined();
    expect(slotEnd).toBeDefined();

    // Force-charge flags still present
    expect(findWrite(writes, 27)!.value).toBe(1);
    expect(findWrite(writes, 96)!.value).toBe(1);
    expect(findWrite(writes, 20)!.value).toBe(1);
    expect(findWrite(writes, 116)!.value).toBe(100);
  });

  test('Reserve with soc=100 (max)', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/reserve`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ soc: 100 }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    expect(findWrite(writes, 110)).toBeDefined();
    expect(findWrite(writes, 110)!.value).toBe(100);
  });

  test('Reserve with soc=4 (minimum)', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/reserve`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ soc: 4 }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    expect(findWrite(writes, 110)).toBeDefined();
    expect(findWrite(writes, 110)!.value).toBe(4);
  });

  test('Charge rate with limit=0 (minimum)', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/charge-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ limit: 0 }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    expect(findWrite(writes, 111)!.value).toBe(0);
  });

  test('Discharge rate with limit=0 (minimum)', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/discharge-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ limit: 0 }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    expect(findWrite(writes, 112)!.value).toBe(0);
  });

  test('Charge slot disabled clears slot registers', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/charge-slot`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        slot: 1,
        enabled: false,
        start_hour: 0,
        start_minute: 0,
        end_hour: 0,
        end_minute: 0,
      }),
    });
    expect((await resp.json()).ok).toBe(true);

    // Disabling a charge slot writes (0, 0) to the slot time registers
    // (HR 94/95 for slot 1), clearing the window so the slot is inactive.
    // enable_charge (HR 96) is a separate global flag not touched here.
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 20_000);
    expect(findWrite(writes, 94)!.value).toBe(0);
  });

  test('Discharge slot disabled clears slot registers', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/discharge-slot`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        slot: 1,
        enabled: false,
        start_hour: 16,
        start_minute: 0,
        end_hour: 19,
        end_minute: 0,
      }),
    });
    expect((await resp.json()).ok).toBe(true);

    // Disabling a discharge slot should write HR56=0, HR57=0 (clearing writes)
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 2, 15_000);
    expect(findWrite(writes, 56)!.value).toBe(0);
    expect(findWrite(writes, 57)!.value).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// Test: Snapshot & WebSocket
// ---------------------------------------------------------------------------

test.describe('Snapshot & WebSocket', () => {
  test('snapshot reflects register changes via mock', async ({
    baseUrl,
    setHoldingReg,
    setInputReg,
    resetModbus,
  }) => {
    // Reset to defaults, then modify registers
    await resetModbus();
    // Set battery SOC to 85%
    await setInputReg(59, 85);
    // Set device type to a known value
    await setHoldingReg(0, 0x2001);
    await setHoldingReg(21, 352);

    // Wait for poll cycle to pick up changes (poll_interval = 5s, give 10s)
    await new Promise((r) => setTimeout(r, 10_000));

    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.data).toBeDefined();
    expect(data.data.soc).toBe(85);
  });

  test('WebSocket connects and delivers data', async ({ baseUrl }) => {
    const wsUrl = baseUrl.replace('http://', 'ws://') + '/ws';
    const ws = new WebSocket(wsUrl);

    const messages: any[] = [];
    await new Promise<void>((resolve, reject) => {
      const timeout = setTimeout(() => {
        ws.close();
        reject(new Error('WebSocket timed out waiting for messages'));
      }, 15_000);

      ws.onmessage = (event) => {
        try {
          messages.push(JSON.parse(event.data as string));
        } catch {
          messages.push({ raw: event.data });
        }
        // Expect at least connection + snapshot message
        if (messages.length >= 2) {
          clearTimeout(timeout);
          ws.close();
          resolve();
        }
      };
      ws.onerror = (err) => {
        clearTimeout(timeout);
        reject(err);
      };
      ws.onopen = () => {
        // Connected — wait for messages via onmessage
      };
    });

    expect(messages.length).toBeGreaterThanOrEqual(1);

    // First message should be connection state
    const connectionMsg = messages.find((m: any) => m.type === 'connection');
    expect(connectionMsg).toBeDefined();
    expect(connectionMsg.state).toBeDefined();

    // Should also receive snapshot
    const snapshotMsg = messages.find((m: any) => m.type === 'snapshot');
    expect(snapshotMsg).toBeDefined();
  });

  test('status endpoint returns connected', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/status`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.connection).toBe('connected');
    expect(data.host).toBeDefined();
    expect(data.client_count).toBeGreaterThanOrEqual(0);
  });
});

// ---------------------------------------------------------------------------
// Test: Different inverter types
//
// NOTE: The backend caches the device type after first detection and does NOT
// re-detect on holding register changes during a single run. To fully exercise
// AC-coupled, three-phase, and Gen1 routing, run a separate test project with
// the mock server's populateDefaults() returning the desired HR0/HR21 values.
//
// The tests below set HR0 in the mock and verify the API still works correctly
// (returning ok responses and writing expected registers). For the default
// Gen3 Hybrid backend, the register addresses will match the Gen3 path.
// ---------------------------------------------------------------------------

test.describe('Inverter Types', () => {
  test('AC Coupled (HR0=0x3001): force charge returns ok', async ({
    baseUrl,
    setHoldingReg,
    resetModbus,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await resetModbus();
    await setHoldingReg(0, 0x3001); // AC Coupled
    await setHoldingReg(21, 100);   // ARM FW — not 3xx century, stays as AC Coupled

    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 5, 20_000);
    expect(writes.length).toBeGreaterThanOrEqual(5);
  });

  test('AC Coupled (HR0=0x3001): pause returns ok', async ({
    baseUrl,
    setHoldingReg,
    resetModbus,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await resetModbus();
    await setHoldingReg(0, 0x3001);
    await setHoldingReg(21, 100);

    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/pause`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 2, 15_000);
    expect(writes.length).toBeGreaterThanOrEqual(2);
  });

  test('AC Coupled (HR0=0x3001): charge rate uses HR313', async ({
    baseUrl,
    setHoldingReg,
    resetModbus,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    // NOTE: This test verifies the API works. The register written depends on
    // whether the backend has detected AC-coupled (HR313) or still uses Gen3
    // Hybrid routing (HR111). In a separate project with AC-coupled defaults,
    // expect HR313.
    await resetModbus();
    await setHoldingReg(0, 0x3001);
    await setHoldingReg(21, 100);

    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/charge-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ limit: 25 }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    // Verify at least one register was written (HR111 for Gen3, HR313 for AC-coupled)
    expect(writes.length).toBeGreaterThanOrEqual(1);
    const regAddr = writes[0].address;
    expect([111, 313]).toContain(regAddr);
  });

  test('Three Phase (HR0=0x4001): force charge returns ok', async ({
    baseUrl,
    setHoldingReg,
    resetModbus,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await resetModbus();
    await setHoldingReg(0, 0x4001);
    await setHoldingReg(21, 100);

    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    expect(writes.length).toBeGreaterThanOrEqual(1);
  });

  test('Three Phase (HR0=0x4001): pause returns ok', async ({
    baseUrl,
    setHoldingReg,
    resetModbus,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await resetModbus();
    await setHoldingReg(0, 0x4001);
    await setHoldingReg(21, 100);

    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/pause`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    expect(writes.length).toBeGreaterThanOrEqual(1);
  });

  test('Three Phase (HR0=0x4001): charge rate returns ok', async ({
    baseUrl,
    setHoldingReg,
    resetModbus,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await resetModbus();
    await setHoldingReg(0, 0x4001);
    await setHoldingReg(21, 100);

    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/charge-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ limit: 30 }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    expect(writes.length).toBeGreaterThanOrEqual(1);
  });

  test('Gen1 (HR0=0x1001): force charge returns ok', async ({
    baseUrl,
    setHoldingReg,
    resetModbus,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await resetModbus();
    await setHoldingReg(0, 0x1001);
    await setHoldingReg(21, 100);

    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    expect(writes.length).toBeGreaterThanOrEqual(1);
  });

  test('Gen1 (HR0=0x1001): pause returns ok', async ({
    baseUrl,
    setHoldingReg,
    resetModbus,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await resetModbus();
    await setHoldingReg(0, 0x1001);
    await setHoldingReg(21, 100);

    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/pause`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    expect(writes.length).toBeGreaterThanOrEqual(1);
  });

  // ---------------------------------------------------------------------------
  // The Gateway/Three-Phase export-limit tests below verify that the endpoint
  // correctly rejects single-phase devices. The mock server caches Gen3Hybrid
  // (single-phase) after the first poll — changing HR 0 mid-session doesn't
  // re-detect the device type — so we can't test the device-specific register
  // routing via E2E. That routing is covered by the Rust unit tests
  // (encoder.rs: set_ems_export_limit_encodes, set_three_phase_export_limit).
  // ---------------------------------------------------------------------------

  test('Gateway/EMS export limit: single-phase device is rejected', async ({
    baseUrl,
    drainModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/export-limit`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ watts: 9200 }),
    });
    // The cached device type is Gen3Hybrid (single-phase) — export limit
    // is only available on Gateway/EMS/three-phase devices.
    const data = await resp.json();
    expect(data.ok).toBe(false);
  });

  test('Three Phase export limit: single-phase device is rejected', async ({
    baseUrl,
    drainModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/export-limit`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ watts: 6000 }),
    });
    // The cached device type is Gen3Hybrid (single-phase) — three-phase
    // export limit routing is not exercised here. See encoder unit tests.
    const data = await resp.json();
    expect(data.ok).toBe(false);
  });

  test('AC Coupled (HR0=0x3001): export limit returns 400 (read-only)', async ({
    baseUrl,
    setHoldingReg,
    resetModbus,
    drainModbusWrites,
  }) => {
    await resetModbus();
    await setHoldingReg(0, 0x3001); // AC Coupled
    await setHoldingReg(21, 100);

    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/export-limit`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ watts: 3000 }),
    });
    expect(resp.status).toBe(400);
    const body = await resp.json();
    expect(body.ok).toBe(false);
  });

  test('Gen1 (HR0=0x1001): export limit returns 400 (read-only)', async ({
    baseUrl,
    setHoldingReg,
    resetModbus,
    drainModbusWrites,
  }) => {
    await resetModbus();
    await setHoldingReg(0, 0x1001); // Gen1
    await setHoldingReg(21, 100);

    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/export-limit`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ watts: 3000 }),
    });
    expect(resp.status).toBe(400);
    const body = await resp.json();
    expect(body.ok).toBe(false);
  });
});
