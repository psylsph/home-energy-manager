/**
 * Charge Power Limit slider against the real GivEnergy Simulator and backend.
 *
 * Issue #346: a Gen1 Hybrid user set the Battery Charge Power Limit to 62% and
 * the battery still charged at the inverter's maximum, because the slider's
 * "% of the inverter's maximum" was written to HR111 as if the register were a
 * percentage of the maximum. The register is a percentage of battery
 * *capacity* (GivTCP `write.py`: `watts / (capacity / 2) * 50`), so the
 * conversion must go through the pack size.
 *
 * Every other charge-limit test stops at "the right register was written" and
 * asserts against HEM's own idea of what that register means, which is how the
 * original fix shipped a correct-looking label over unchanged behaviour. This
 * suite measures the one thing that matters: the power the battery actually
 * charges at, in the simulator's physics, after the limit is set through the UI.
 *
 * The pack (9.5 kWh) is deliberately larger than twice the inverter's battery
 * rating, the case where "percent of maximum" and "percent of capacity" differ.
 */

import { test, expect } from '@playwright/test';
import type { Page } from '@playwright/test';
import { spawn } from 'child_process';
import type { ChildProcess } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import { fileURLToPath } from 'url';
import { writeTestSettings } from './test-settings.js';
import type { TestSettingsFixture } from './test-settings.js';
import { simulatorBinaryPath } from './binary-path.js';
import { attachErrorHandler } from './process-errors.js';
import { stopChildProcess } from './process-lifecycle.js';
import { killPort } from './port-cleanup.mjs';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const MODBUS_PORT = 19911;
const HTTP_PORT = 18357;
const EVC_PORT = 19921;
const BASE_URL = `http://127.0.0.1:${HTTP_PORT}`;
const DIST_DIR = path.resolve(__dirname, '..', 'dist');
const BACKEND_PATH = path.resolve(
  __dirname,
  '..',
  'src-tauri',
  'target',
  'release',
  process.platform === 'win32' ? 'givenergy-local.exe' : 'givenergy-local',
);
const SIMULATOR_PATH = simulatorBinaryPath();

/** Measured charge power may differ from the target by about one register step
 *  (1% of a 9.5 kWh pack = 95 W) plus the simulator's own rounding. */
const TOLERANCE_W = 150;

let simulator: ChildProcess | null = null;
let backend: ChildProcess | null = null;
let settingsFixture: TestSettingsFixture | null = null;
let maxBatteryPowerW = 0;

function postJson(pathname: string, body?: unknown) {
  return fetch(`${BASE_URL}${pathname}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
}

async function getSnapshot() {
  const response = await fetch(`${BASE_URL}/api/snapshot`);
  const payload = await response.json();
  return payload.data as Record<string, unknown>;
}

async function waitForSnapshot(
  predicate: (value: Record<string, unknown>) => boolean,
  timeoutMs = 40_000,
) {
  await expect.poll(async () => {
    try {
      return predicate(await getSnapshot());
    } catch {
      return false;
    }
  }, { timeout: timeoutMs, intervals: [500, 1_000, 2_000] }).toBe(true);
}

function attachLogs(label: string, proc: ChildProcess) {
  proc.stdout?.on('data', (data: Buffer) => {
    const text = data.toString().trim();
    if (text) console.log(`[${label}] ${text}`);
  });
  proc.stderr?.on('data', (data: Buffer) => {
    const text = data.toString().trim();
    if (text) console.log(`[${label}:err] ${text}`);
  });
}

/** Drag the Battery Charge Power Limit slider to `percent` and press its Save. */
async function setChargeLimitInUi(page: Page, percent: number) {
  await page.goto(`${BASE_URL}/#/control`);
  const row = page
    .getByText('Battery Charge Power Limit', { exact: true })
    .locator('xpath=ancestor::div[contains(@class,"space-y-1")][1]');
  await expect(row).toBeVisible({ timeout: 20_000 });
  await row.locator('input[type="range"]').fill(String(percent));
  await row.getByRole('button', { name: 'Save' }).click();
  // Save stays pending (with the "Applying changes" banner) until the inverter
  // reads the value back, so it re-enabling with no alert proves the UI saw the
  // limit land, not just that the request was queued.
  await expect(row.getByRole('button', { name: 'Save' })).toBeEnabled({ timeout: 30_000 });
  await expect(page.getByRole('alert')).toHaveCount(0);
}

/**
 * Set the limit through the UI, then (re)start a forced grid charge so the
 * battery is limited only by that setting. The limit is written while nothing
 * owns the inverter: manual writes are deliberately deferred behind an active
 * Force Charge, so changing it mid-charge would wait for the charge to end.
 */
async function chargeWithLimit(page: Page, percent: number) {
  // Stopping when no charge is running is refused, which is fine here.
  await postJson('/api/control/force-charge/stop');
  await waitForSnapshot((value) => typeof value.battery_power === 'number' && value.battery_power > -200);
  await setChargeLimitInUi(page, percent);
  expect((await postJson('/api/control/force-charge', { minutes: 60 })).ok).toBe(true);
}

/** The power (W) the battery is charging at right now, as a positive number. */
async function chargingWatts(): Promise<number> {
  const value = await getSnapshot();
  return typeof value.battery_power === 'number' ? -value.battery_power : 0;
}

/** Wait for the measured charge power to settle within `tolerance` of `target`. */
async function expectChargingNear(target: number, tolerance = TOLERANCE_W) {
  let lastMeasured = Number.NaN;
  try {
    await expect.poll(
      async () => {
        lastMeasured = await chargingWatts();
        return Math.abs(lastMeasured - target) <= tolerance;
      },
      { timeout: 45_000, intervals: [1_000, 2_000] },
    ).toBe(true);
  } catch (error) {
    throw new Error(
      `battery charge power should settle within ${tolerance} W of ${Math.round(target)} W, `
      + `last measured ${Math.round(lastMeasured)} W (max ${maxBatteryPowerW} W)`,
      { cause: error },
    );
  }
}

test.describe.serial('Charge Power Limit slider sets the real charge power', () => {
  test.beforeAll(async () => {
    test.setTimeout(90_000);

    for (const binary of [SIMULATOR_PATH, BACKEND_PATH]) {
      if (!binary || !fs.existsSync(binary)) {
        throw new Error(`Required release binary not found: ${binary}`);
      }
    }
    if (!fs.existsSync(path.join(DIST_DIR, 'index.html'))) {
      throw new Error(`Frontend build not found in ${DIST_DIR}; run npm run build first`);
    }
    for (const port of [MODBUS_PORT, HTTP_PORT]) {
      killPort(port);
    }

    simulator = spawn(SIMULATOR_PATH, [
      'simulate',
      '--inverter', 'Gen1Hybrid',
      '--batteries', '1',
      '--battery-size', '9.5',
      '--soc', '30',
      '--solar-peak', '0',
      '--load-level', '500',
      '--modbus', `127.0.0.1:${MODBUS_PORT}`,
      '--evc-port', String(EVC_PORT),
    ], { stdio: ['ignore', 'pipe', 'pipe'] });
    attachErrorHandler(simulator, 'charge-limit simulator');
    attachLogs('charge-limit-sim', simulator);

    settingsFixture = await writeTestSettings({
      tag: 'charge-limit-power',
      port: MODBUS_PORT,
      httpPort: HTTP_PORT,
      pollInterval: 2,
      writePacingMs: 25,
    });

    backend = spawn(
      BACKEND_PATH,
      ['--headless', '--port', String(HTTP_PORT), '--dist', DIST_DIR],
      {
        stdio: ['ignore', 'pipe', 'pipe'],
        env: { ...process.env, ...settingsFixture.env },
      },
    );
    attachErrorHandler(backend, 'charge-limit backend');
    attachLogs('charge-limit-backend', backend);

    await waitForSnapshot(
      (value) => value.soc === 30 && typeof value.max_battery_power_w === 'number'
        && value.max_battery_power_w > 0,
      60_000,
    );
    maxBatteryPowerW = (await getSnapshot()).max_battery_power_w as number;
  });

  test.afterAll(async () => {
    try { await postJson('/api/control/force-charge/stop'); } catch { /* already stopped */ }
    await stopChildProcess(backend, 'charge-limit backend', 5_000);
    await stopChildProcess(simulator, 'charge-limit simulator', 5_000);
    backend = null;
    simulator = null;
    if (settingsFixture) await settingsFixture.cleanup();
    settingsFixture = null;
  });

  test('60% of the inverter maximum charges at 60% of the maximum, not at full power', async ({ page }) => {
    test.setTimeout(120_000);

    await chargeWithLimit(page, 60);

    await expectChargingNear(0.6 * maxBatteryPowerW);
  });

  test('a lower limit lowers the real charge power', async ({ page }) => {
    test.setTimeout(120_000);

    await chargeWithLimit(page, 30);

    await expectChargingNear(0.3 * maxBatteryPowerW);
  });

  test('100% lets the battery charge at the inverter maximum', async ({ page }) => {
    test.setTimeout(120_000);

    await chargeWithLimit(page, 100);

    // The simulator's Gen1 hardware ceiling is 2.5 kW; the backend's stated
    // maximum may be a touch higher, so only require "essentially full power".
    await expect.poll(
      async () => (await chargingWatts()) >= 0.9 * Math.min(maxBatteryPowerW, 2_500),
      { timeout: 45_000, intervals: [1_000, 2_000] },
    ).toBe(true);
  });
});
