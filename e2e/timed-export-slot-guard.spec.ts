/**
 * E2E coverage for the Timed Export/discharge-slot invariant.
 *
 * The mock Modbus server is intentional here: unlike the real simulator it
 * lets us inject the exact inverter states that matter (including HR59=1
 * with no discharge slot) and preserves FC06 writes for read-back assertions.
 */

import { test, expect } from './fixture.js';
import { startBackend, stopBackend } from './backend.js';
import type { RegisterWrite } from './mock-modbus.js';

const HR_BATTERY_POWER_MODE = 27;
const HR_DISCHARGE_SLOT_1_START = 56;
const HR_DISCHARGE_SLOT_1_END = 57;
const HR_ENABLE_DISCHARGE = 59;

const wait = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

async function getSnapshot(baseUrl: string): Promise<Record<string, any>> {
  const response = await fetch(`${baseUrl}/api/snapshot`);
  const body = await response.json();
  if (!body.ok) throw new Error(`snapshot unavailable: ${JSON.stringify(body)}`);
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

async function waitForSnapshot(
  baseUrl: string,
  predicate: (snapshot: Record<string, any>) => boolean,
  label: string,
  timeoutMs = 20_000,
): Promise<Record<string, any>> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const snapshot = await getSnapshot(baseUrl);
      if (predicate(snapshot)) return snapshot;
    } catch {
      // The backend may be between poll cycles.
    }
    await wait(250);
  }
  throw new Error(`${label} was not observed within ${timeoutMs}ms`);
}

async function waitForWrite(
  peekWrites: () => Promise<RegisterWrite[]>,
  address: number,
  value: number,
  timeoutMs = 20_000,
): Promise<RegisterWrite[]> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const writes = await peekWrites();
    if (writes.some((write) => write.address === address && write.value === value)) {
      return writes;
    }
    await wait(250);
  }
  return peekWrites();
}

/** Wait until every (address, value) pair has landed in the write log.
 *
 * The poll-loop heal and the last-slot-disable path write HR59=0 and then
 * HR27=1 with a ~1.5s gap; waiting on the first alone would return a partial
 * batch. This waits until the trailing write is observed too. */
async function waitForAllWrites(
  peekWrites: () => Promise<RegisterWrite[]>,
  pairs: Array<[number, number]>,
  timeoutMs = 20_000,
): Promise<RegisterWrite[]> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const writes = await peekWrites();
    if (pairs.every(([address, value]) =>
      writes.some((write) => write.address === address && write.value === value),
    )) {
      return writes;
    }
    await wait(250);
  }
  return peekWrites();
}

function writeIndex(writes: RegisterWrite[], address: number, value: number): number {
  return writes.findIndex((write) => write.address === address && write.value === value);
}

async function resetToEco(
  baseUrl: string,
  resetModbus: () => Promise<void>,
  drainModbusWrites: () => Promise<RegisterWrite[]>,
): Promise<void> {
  await resetModbus();
  await waitForSnapshot(
    baseUrl,
    (snapshot) => snapshot.enable_discharge === false,
    'clean Eco snapshot',
  );
  await drainModbusWrites();
}

test.beforeAll(async () => {
  await startBackend();
});

test.afterAll(async () => {
  await stopBackend();
});

test.describe('Timed Export/discharge-slot state alignment', () => {
  test('rejects enabling Timed Export when no discharge slot exists', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
  }) => {
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);

    const result = await postJson(baseUrl, '/api/control/timed-export', { enabled: true });
    expect(result.status).toBe(409);
    expect(result.body.ok).toBe(false);
    expect(result.body.error).toContain('Configure at least one discharge slot');

    const writes = await drainModbusWrites();
    expect(writes.some((write) => write.address === HR_ENABLE_DISCHARGE && write.value === 1)).toBe(false);
  });

  test('arms Timed Export only after a configured slot is observed and preserves order', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
    setHoldingReg,
    peekModbusWrites,
  }) => {
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);
    await setHoldingReg(HR_DISCHARGE_SLOT_1_START, 1600);
    await setHoldingReg(HR_DISCHARGE_SLOT_1_END, 1900);
    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.discharge_slots?.[0]?.enabled === true,
      'configured discharge slot',
    );
    await drainModbusWrites();

    const result = await postJson(baseUrl, '/api/control/timed-export', { enabled: true });
    expect(result.status).toBe(200);
    expect(result.body.ok).toBe(true);

    const writes = await waitForWrite(peekModbusWrites, HR_ENABLE_DISCHARGE, 1);
    const hr27 = writeIndex(writes, HR_BATTERY_POWER_MODE, 0);
    const hr59 = writeIndex(writes, HR_ENABLE_DISCHARGE, 1);
    expect(hr27).toBeGreaterThanOrEqual(0);
    expect(hr59).toBeGreaterThan(hr27);

    const armed = await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.enable_discharge === true && snapshot.discharge_slots?.[0]?.enabled === true,
      'armed Timed Export snapshot',
    );
    expect(armed.battery_power_mode).toBe(0);
  });

  test('automatically repairs an externally-created HR59/no-slot state', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
    setHoldingReg,
    peekModbusWrites,
  }) => {
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);
    await setHoldingReg(HR_BATTERY_POWER_MODE, 0);
    await setHoldingReg(HR_ENABLE_DISCHARGE, 1);
    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.enable_discharge === true && snapshot.discharge_slots?.every((slot: any) => !slot.enabled),
      'inconsistent HR59/no-slot snapshot',
    );

    const writes = await waitForAllWrites(peekModbusWrites, [
      [HR_ENABLE_DISCHARGE, 0],
      [HR_BATTERY_POWER_MODE, 1],
    ]);
    expect(writes.some((write) => write.address === HR_BATTERY_POWER_MODE && write.value === 1)).toBe(true);
    expect(writes.some((write) => write.address === HR_ENABLE_DISCHARGE && write.value === 0)).toBe(true);

    const repaired = await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.enable_discharge === false && snapshot.battery_power_mode === 1,
      'repaired Eco snapshot',
    );
    expect(repaired.discharge_slots?.every((slot: any) => !slot.enabled)).toBe(true);
  });

  test('disabling the last slot returns an armed inverter to Eco', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
    setHoldingReg,
    peekModbusWrites,
  }) => {
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);
    await setHoldingReg(HR_DISCHARGE_SLOT_1_START, 1600);
    await setHoldingReg(HR_DISCHARGE_SLOT_1_END, 1900);
    await setHoldingReg(HR_BATTERY_POWER_MODE, 0);
    await setHoldingReg(HR_ENABLE_DISCHARGE, 1);
    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.enable_discharge === true && snapshot.discharge_slots?.[0]?.enabled === true,
      'armed snapshot with one slot',
    );
    await drainModbusWrites();

    const result = await postJson(baseUrl, '/api/control/discharge-slot', {
      slot: 1,
      enabled: false,
      start_hour: 16,
      start_minute: 0,
      end_hour: 19,
      end_minute: 0,
    });
    expect(result.status).toBe(200);

    const writes = await waitForAllWrites(peekModbusWrites, [
      [HR_DISCHARGE_SLOT_1_START, 0],
      [HR_DISCHARGE_SLOT_1_END, 0],
      [HR_ENABLE_DISCHARGE, 0],
      [HR_BATTERY_POWER_MODE, 1],
    ]);
    expect(writes.some((write) => write.address === HR_DISCHARGE_SLOT_1_START && write.value === 0)).toBe(true);
    expect(writes.some((write) => write.address === HR_DISCHARGE_SLOT_1_END && write.value === 0)).toBe(true);
    expect(writes.some((write) => write.address === HR_BATTERY_POWER_MODE && write.value === 1)).toBe(true);

    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.enable_discharge === false && snapshot.battery_power_mode === 1,
      'Eco snapshot after last-slot disable',
    );
  });

  test('does not repair a valid armed state while a slot remains configured', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
    setHoldingReg,
  }) => {
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);
    await setHoldingReg(HR_DISCHARGE_SLOT_1_START, 1600);
    await setHoldingReg(HR_DISCHARGE_SLOT_1_END, 1900);
    await setHoldingReg(HR_BATTERY_POWER_MODE, 0);
    await setHoldingReg(HR_ENABLE_DISCHARGE, 1);
    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.enable_discharge === true && snapshot.discharge_slots?.[0]?.enabled === true,
      'valid armed snapshot',
    );
    await drainModbusWrites();

    await wait(11_000);
    const writes = await drainModbusWrites();
    expect(writes.some((write) => write.address === HR_ENABLE_DISCHARGE && write.value === 0)).toBe(false);
    expect(writes.some((write) => write.address === HR_BATTERY_POWER_MODE && write.value === 1)).toBe(false);
  });

  test('locks the real control page when the polled inverter has no slot', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
    page,
  }) => {
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);
    await page.goto('/#/control');
    const button = page.getByRole('button', { name: /Timed Export/ });
    await expect(button).toBeVisible();
    await expect(button).toBeDisabled();
    await expect(
      page.getByText('Configure at least one discharge slot before enabling Timed Export.'),
    ).toBeVisible();
  });
});
