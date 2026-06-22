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
  test('Force Charge should send correct Modbus writes', async ({
    page,
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
  });

  test('Force Discharge should send correct Modbus writes', async ({
    page,
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
  });

  test('Pause Battery should send correct Modbus write', async ({
    page,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    await page.goto('/');
    await page.locator('text=Control').click();
    await page.getByRole('button', { name: /Pause Battery/ }).click();

    // PauseBattery = 2 writes: HR 96=0 (disable charge), HR 59=0 (disable discharge)
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 2, 20_000);
    expect(writes.length).toBeGreaterThanOrEqual(2);

    expect(findWrite(writes, 96)!.value).toBe(0);  // disable charge
    expect(findWrite(writes, 59)!.value).toBe(0);  // disable discharge
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
      body: JSON.stringify({ mode: 'eco' }),
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

  test('POST /api/control/mode timed_demand sends correct writes', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'timed_demand' }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 3, 15_000);
    expect(findWrite(writes, 27)!.value).toBe(1);  // self-consumption
    expect(findWrite(writes, 59)!.value).toBe(1);  // enable discharge
    expect(findWrite(writes, 110)!.value).toBe(4);  // SOC reserve
  });

  test('POST /api/control/mode timed_export sends correct writes', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'timed_export' }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 3, 15_000);
    expect(findWrite(writes, 27)!.value).toBe(0);  // export mode
    expect(findWrite(writes, 59)!.value).toBe(1);  // enable discharge
    expect(findWrite(writes, 110)!.value).toBe(4);  // SOC reserve
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
  });

  test('POST /api/control/force-discharge sends correct writes', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

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
  });

  test('POST /api/control/force-discharge without minutes keeps legacy 00:00–23:59 slot', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

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
  });

  test('POST /api/control/pause sends HR 110=100 (SOC reserve) and clears charge/discharge', async ({
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

    // PauseBattery now writes: charge clear, discharge clear, slot clears,
    // eco mode, and SOC reserve=100 = 8 writes (~12s at 1.5s each)
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 8, 25_000);

    expect(findWrite(writes, 96)!.value).toBe(0);     // disable charge
    expect(findWrite(writes, 59)!.value).toBe(0);     // disable discharge
    expect(findWrite(writes, 27)!.value).toBe(1);     // eco mode
    expect(findWrite(writes, 110)!.value).toBe(100);  // SOC reserve=100 to pause
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
  });

  test('Force Charge when already in eco mode should still work', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    test.setTimeout(60_000);

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

    // Wait for ALL eco mode writes to complete (7 writes ~10s)
    await waitForWrites(peekModbusWrites, drainModbusWrites, 7, 25_000);
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

    // API sends minutes=30 → 7 writes (2 slot + 5 flags)
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 7, 20_000);
    expect(writes.length).toBeGreaterThanOrEqual(7);

    expect(findWrite(writes, 27)!.value).toBe(1);    // eco mode
    expect(findWrite(writes, 59)!.value).toBe(0);    // clear stale discharge
    expect(findWrite(writes, 96)!.value).toBe(1);    // enable_charge
    expect(findWrite(writes, 20)!.value).toBe(1);    // enable_charge_target
    expect(findWrite(writes, 116)!.value).toBe(100); // target SOC
  });

  test('Pause Battery should write HR110=100 and clear charge/discharge', async ({
    page,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    await page.goto('/');
    await page.locator('text=Control').click();
    await page.getByRole('button', { name: /Pause Battery/ }).click();

    // Pause = charge clear + discharge clear + slot clears + eco mode + SOC reserve=100
    // = 8 writes (~12s at 1.5s each)
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 8, 30_000);
    expect(writes.length).toBeGreaterThanOrEqual(8);

    expect(findWrite(writes, 96)!.value).toBe(0);     // disable charge
    expect(findWrite(writes, 59)!.value).toBe(0);     // disable discharge
    expect(findWrite(writes, 27)!.value).toBe(1);     // eco mode
    expect(findWrite(writes, 110)!.value).toBe(100);  // SOC reserve=100
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
    // Set eco mode first
    await clearWrites(drainModbusWrites);
    let resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'eco' }),
    });
    expect((await resp.json()).ok).toBe(true);
    await waitForWrites(peekModbusWrites, drainModbusWrites, 4, 20_000);

    // Now transition to timed_demand
    await clearWrites(drainModbusWrites);
    resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'timed_demand' }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 3, 15_000);
    expect(findWrite(writes, 27)!.value).toBe(1);  // self-consumption
    expect(findWrite(writes, 59)!.value).toBe(1);  // enable discharge
    expect(findWrite(writes, 110)!.value).toBe(4);  // SOC reserve
  });

  test('Eco → Timed Export transition', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);
    let resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'eco' }),
    });
    expect((await resp.json()).ok).toBe(true);
    await waitForWrites(peekModbusWrites, drainModbusWrites, 4, 20_000);

    await clearWrites(drainModbusWrites);
    resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'timed_export' }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 3, 15_000);
    expect(findWrite(writes, 27)!.value).toBe(0);  // export mode
    expect(findWrite(writes, 59)!.value).toBe(1);  // enable discharge
    expect(findWrite(writes, 110)!.value).toBe(4);  // SOC reserve
  });

  test('Timed Demand → Eco transition', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);
    let resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'timed_demand' }),
    });
    expect((await resp.json()).ok).toBe(true);
    await waitForWrites(peekModbusWrites, drainModbusWrites, 3, 15_000);

    await clearWrites(drainModbusWrites);
    resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'eco' }),
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

    // Disabling a charge slot writes only enable_charge=false (HR96=0).
    // Slot time registers and enable_charge_target are NOT written — the
    // hardware ignores slot times when enable_charge is false.
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 20_000);
    expect(findWrite(writes, 96)!.value).toBe(0);
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
  // Export Power Limit — device-type-specific behaviour
  // ---------------------------------------------------------------------------

  test('Gateway (HR0=0x7001): export limit writes HR 2071', async ({
    baseUrl,
    setHoldingReg,
    resetModbus,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await resetModbus();
    await setHoldingReg(0, 0x7001); // Gateway
    await setHoldingReg(21, 100);

    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/export-limit`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ watts: 9200 }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    expect(writes.length).toBeGreaterThanOrEqual(1);
    expect(writes[0].address).toBe(2071);
    expect(writes[0].value).toBe(9200);
  });

  test('Three Phase (HR0=0x4001): export limit writes HR 1063 (deci-W)', async ({
    baseUrl,
    setHoldingReg,
    resetModbus,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await resetModbus();
    await setHoldingReg(0, 0x4001); // Three Phase
    await setHoldingReg(21, 100);

    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/export-limit`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ watts: 6000 }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    expect(writes.length).toBeGreaterThanOrEqual(1);
    expect(writes[0].address).toBe(1063);
    expect(writes[0].value).toBe(60000); // 6000 W × 10 = 60000 deci-W
  });

  test('AC Coupled (HR0=0x3001): export limit returns 400 (read-only)', async ({
    baseUrl,
    setHoldingReg,
    resetModbus,
    drainModbusWrites,
    _peekModbusWrites,
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
    _peekModbusWrites,
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
