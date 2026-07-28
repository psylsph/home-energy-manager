/**
 * Browser and full backend/Modbus coverage for Adaptive Charge.
 *
 * The mock register server is intentionally used here so SOC can be moved
 * across both hysteresis thresholds without waiting for battery physics.
 */

import { test, expect } from './fixture.js';
import { startBackend, stopBackend } from './backend.js';
import type { RegisterWrite } from './mock-modbus.js';

test.beforeAll(async () => {
  await startBackend();
});

test.afterAll(async () => {
  await stopBackend();
});

async function postJson(baseUrl: string, path: string, body: unknown) {
  return fetch(`${baseUrl}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
}

async function snapshot(baseUrl: string) {
  const response = await fetch(`${baseUrl}/api/snapshot`);
  const payload = await response.json();
  return payload.data;
}

async function waitForSnapshot(
  baseUrl: string,
  predicate: (value: Record<string, unknown>) => boolean,
  timeoutMs = 35_000,
) {
  await expect.poll(async () => {
    const value = await snapshot(baseUrl);
    return predicate(value);
  }, { timeout: timeoutMs, intervals: [500, 1_000, 2_000] }).toBe(true);
}

function hasWrite(writes: RegisterWrite[], address: number, value: number) {
  return writes.some((write) => write.address === address && write.value === value);
}

test.describe.serial('Adaptive Charge', () => {
  test('browser exposes configuration, validation, and mode switching', async ({ page, baseUrl }) => {
    await page.goto('/#/control');

    const section = page.locator('section').filter({ hasText: 'Charging Mode' }).first();
    const modeSelect = section.locator('select').first();
    await expect(modeSelect).toBeVisible({ timeout: 15_000 });
    await modeSelect.selectOption('adaptive');

    await expect(page.getByText('Preferred charge rate').first()).toBeVisible();
    await expect(page.getByText('Recovery charge rate').first()).toBeVisible();
    await expect(page.getByRole('button', { name: 'Add period' })).toBeVisible();

    await page.getByLabel('Low SOC period 1').fill('90');
    await page.getByLabel('Recovery SOC period 1').fill('80');
    await page.getByRole('button', { name: 'Save Adaptive Charge' }).click();
    await expect(page.getByText('Period 1: Recovery SOC must be above Low SOC.')).toBeVisible();

    await page.getByLabel('Low SOC period 1').fill('30');
    await page.getByLabel('Recovery SOC period 1').fill('60');
    await modeSelect.selectOption('standard');
    await expect.poll(async () => {
      const response = await fetch(`${baseUrl}/api/charging-mode`);
      return (await response.json()).mode;
    }).toBe('standard');
    // If the default period happened to be active at the host's local time,
    // wait for its captured baseline to finish restoring before the next test.
    await waitForSnapshot(baseUrl, (value) => value.adaptive_charge_state === 'inactive');
  });

  test('drives preferred/recovery hysteresis, blocks manual writes, and restores baseline', async ({
    baseUrl,
    setInputReg,
    drainModbusWrites,
  }) => {
    test.setTimeout(100_000);

    // Establish a non-default manual baseline so restoration is observable.
    const baseline = await postJson(baseUrl, '/api/control/charge-rate', { limit: 35 });
    expect(baseline.ok).toBe(true);
    await waitForSnapshot(baseUrl, (value) => value.charge_rate === 35);
    await drainModbusWrites();

    const configResponse = await postJson(baseUrl, '/api/adaptive-charge', {
      config: {
        confirmation_readings: 2,
        periods: [{
          enabled: true,
          all_day: true,
          start_hour: 0,
          start_minute: 0,
          end_hour: 0,
          end_minute: 0,
          low_soc: 70,
          recovery_soc: 80,
          preferred_rate_percent: 40,
          recovery_rate_percent: 100,
        }],
      },
    });
    expect(configResponse.ok).toBe(true);

    const enableResponse = await postJson(baseUrl, '/api/charging-mode', { mode: 'adaptive' });
    expect(enableResponse.ok).toBe(true);
    await waitForSnapshot(
      baseUrl,
      (value) => value.adaptive_charge_state === 'preferred' && value.charge_rate === 20,
    );

    // IR59 is battery SOC on the single-phase register map. Two readings below
    // Low SOC are required before the controller enters recovery.
    await setInputReg(59, 69);
    await waitForSnapshot(
      baseUrl,
      (value) => value.adaptive_charge_state === 'recovery' && value.charge_rate === 50,
    );

    const conflict = await postJson(baseUrl, '/api/control/charge-rate', { limit: 10 });
    expect(conflict.status).toBe(409);
    expect((await conflict.json()).error).toContain('Adaptive Charge');

    // Recovery remains latched through the hysteresis band and releases only
    // after the configured confirmation count at Recovery SOC.
    await setInputReg(59, 75);
    await new Promise((resolve) => setTimeout(resolve, 6_000));
    expect((await snapshot(baseUrl)).adaptive_charge_state).toBe('recovery');

    await setInputReg(59, 81);
    await waitForSnapshot(
      baseUrl,
      (value) => value.adaptive_charge_state === 'preferred' && value.charge_rate === 20,
    );

    const disableResponse = await postJson(baseUrl, '/api/charging-mode', { mode: 'standard' });
    expect(disableResponse.ok).toBe(true);
    await waitForSnapshot(
      baseUrl,
      (value) => value.adaptive_charge_state === 'inactive' && value.charge_rate === 35,
    );

    const writes = await drainModbusWrites();
    expect(hasWrite(writes, 111, 20)).toBe(true); // preferred 40% -> DC raw 20
    expect(hasWrite(writes, 111, 50)).toBe(true); // recovery 100% -> DC raw 50
    expect(hasWrite(writes, 111, 35)).toBe(true); // exact manual baseline restored
  });
});
