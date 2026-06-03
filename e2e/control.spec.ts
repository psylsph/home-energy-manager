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

/** Clear any in-flight writes from previous tests. */
async function clearWrites(drainModbusWrites: () => Promise<RegisterWrite[]>) {
  // Wait for any in-flight register writes (1.5s per write) to complete
  await new Promise((r) => setTimeout(r, 2000));
  await drainModbusWrites();
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
    await page.locator('text=Force Charge').click();

    // ForceCharge = 4 writes, each with 1.5s delay = ~6s total
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 4, 20_000);
    expect(writes.length).toBeGreaterThanOrEqual(4);

    // HR 27 = 1 (eco), HR 96 = 1 (enable_charge), HR 20 = 1 (enable_charge_target), HR 116 = 100
    expect(findWrite(writes, 27)!.value).toBe(1);
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
    await page.locator('text=Force Discharge').click();

    // ForceDischarge = 3 writes, ~4.5s
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 3, 20_000);
    expect(writes.length).toBeGreaterThanOrEqual(3);

    // HR 59 = 1, HR 56 = 0, HR 57 = 2359
    expect(findWrite(writes, 59)!.value).toBe(1);
    expect(findWrite(writes, 56)!.value).toBe(0);
    expect(findWrite(writes, 57)!.value).toBe(2359);
  });

  test('Pause Battery should send correct Modbus write', async ({
    page,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    await page.goto('/');
    await page.locator('text=Control').click();
    await page.locator('text=Pause Battery').click();

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 20_000);
    expect(writes.length).toBeGreaterThanOrEqual(1);

    // HR 110 = 100
    expect(findWrite(writes, 110)!.value).toBe(100);
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

    // charge slot 1: HR 94 + HR 95 + enable_charge
    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 3, 15_000);
    expect(findWrite(writes, 94)!.value).toBe(30);   // 00:30
    expect(findWrite(writes, 95)!.value).toBe(430);   // 04:30
    expect(findWrite(writes, 96)!.value).toBe(1);     // enable_charge
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

  test('POST /api/control/force-charge sends correct writes', async ({
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

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 4, 15_000);
    expect(findWrite(writes, 27)!.value).toBe(1);    // eco mode
    expect(findWrite(writes, 96)!.value).toBe(1);    // enable_charge
    expect(findWrite(writes, 20)!.value).toBe(1);    // enable_charge_target
    expect(findWrite(writes, 116)!.value).toBe(100); // target SOC
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

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 3, 15_000);
    expect(findWrite(writes, 59)!.value).toBe(1);     // enable discharge
    expect(findWrite(writes, 56)!.value).toBe(0);     // slot start 00:00
    expect(findWrite(writes, 57)!.value).toBe(2359);  // slot end 23:59
  });

  test('POST /api/control/pause sends HR 110=100', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);

    const resp = await fetch(`${baseUrl}/api/control/pause`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrites(peekModbusWrites, drainModbusWrites, 1, 15_000);
    expect(findWrite(writes, 110)!.value).toBe(100);
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
