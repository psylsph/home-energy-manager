/**
 * Adaptive Charge tests against the real GivEnergy Simulator and backend.
 *
 * This suite owns isolated simulator/backend processes because it needs a
 * deterministic low-SOC plant. It verifies the complete browser → HTTP → poll
 * loop → proprietary Modbus → simulator physics path.
 */

import { test, expect } from '@playwright/test';
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
const MODBUS_PORT = 19899;
const HTTP_PORT = 18347;
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

let simulator: ChildProcess | null = null;
let backend: ChildProcess | null = null;
let settingsFixture: TestSettingsFixture | null = null;

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

async function stopProcess(process: ChildProcess | null) {
  await stopChildProcess(process, 'adaptive process', 5_000);
}

function attachLogs(label: string, process: ChildProcess) {
  process.stdout?.on('data', (data: Buffer) => {
    const text = data.toString().trim();
    if (text) console.log(`[${label}] ${text}`);
  });
  process.stderr?.on('data', (data: Buffer) => {
    const text = data.toString().trim();
    if (text) console.log(`[${label}:err] ${text}`);
  });
}

test.describe.serial('Adaptive Charge with real simulator', () => {
  test.beforeAll(async () => {
    test.setTimeout(60_000);

    for (const binary of [SIMULATOR_PATH, BACKEND_PATH]) {
      if (!binary || !fs.existsSync(binary)) {
        throw new Error(`Required release binary not found: ${binary}`);
      }
    }
    for (const port of [MODBUS_PORT, HTTP_PORT]) {
      killPort(port);
    }

    simulator = spawn(SIMULATOR_PATH, [
      'simulate',
      '--inverter', 'Gen3Hybrid',
      '--batteries', '1',
      '--battery-size', '9.5',
      '--soc', '27',
      '--solar-peak', '0',
      '--load-level', '1500',
      '--modbus', `127.0.0.1:${MODBUS_PORT}`,
      '--evc-port', '19920',
    ], { stdio: ['ignore', 'pipe', 'pipe'] });
    attachErrorHandler(simulator, 'adaptive simulator');
    attachLogs('adaptive-sim', simulator);

    settingsFixture = await writeTestSettings({
      tag: 'adaptive-real-sim',
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
    attachErrorHandler(backend, 'adaptive backend');
    attachLogs('adaptive-backend', backend);

    await waitForSnapshot((value) => value.soc === 27, 45_000);
  });

  test.afterAll(async () => {
    try { await postJson('/api/control/force-charge/stop'); } catch { /* already stopped */ }
    try { await postJson('/api/charging-mode', { mode: 'standard' }); } catch { /* backend gone */ }
    await stopProcess(backend);
    await stopProcess(simulator);
    backend = null;
    simulator = null;
    if (settingsFixture) await settingsFixture.cleanup();
    settingsFixture = null;
  });

  test('enters recovery and writes the full DC-hybrid limit through Modbus', async ({ page }) => {
    test.setTimeout(70_000);

    const baseline = await postJson('/api/control/charge-rate', { limit: 30 });
    expect(baseline.ok).toBe(true);
    await waitForSnapshot((value) => value.charge_rate === 30);

    const config = await postJson('/api/adaptive-charge', {
      config: {
        confirmation_readings: 1,
        periods: [{
          enabled: true,
          all_day: true,
          start_hour: 0,
          start_minute: 0,
          end_hour: 0,
          end_minute: 0,
          low_soc: 30,
          recovery_soc: 60,
          preferred_rate_percent: 50,
          recovery_rate_percent: 100,
        }],
      },
    });
    expect(config.ok).toBe(true);

    await page.goto(`${BASE_URL}/#/control`);
    const section = page.locator('section').filter({ hasText: 'Charging Mode' }).first();
    await section.locator('select').first().selectOption('adaptive');

    await waitForSnapshot((value) =>
      value.adaptive_charge_state === 'recovery'
      && value.adaptive_charge_desired_rate_percent === 100
      && value.charge_rate === 50,
    );

    await expect(page.getByText('Low-SOC recovery active')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByText('100%', { exact: true }).first()).toBeVisible();
  });

  test('recovery permits full 3.6 kW charging in simulator physics', async () => {
    test.setTimeout(50_000);

    const response = await postJson('/api/control/force-charge', { minutes: 30 });
    expect(response.ok).toBe(true);

    await waitForSnapshot((value) =>
      value.adaptive_charge_state === 'recovery'
      && typeof value.battery_power === 'number'
      && value.battery_power <= -3_300,
    );

    const value = await getSnapshot();
    expect(value.charge_rate).toBe(50); // HR111 raw maximum
    expect(value.adaptive_charge_desired_rate_percent).toBe(100);
    expect(value.battery_power).toBeLessThanOrEqual(-3_300);
    expect(value.grid_power).toBeLessThan(-4_500);
  });

  test('rejects competing manual rate writes while Adaptive owns HR111', async () => {
    const response = await postJson('/api/control/charge-rate', { limit: 10 });
    expect(response.status).toBe(409);
    expect((await response.json()).error).toContain('Adaptive Charge');
    expect((await getSnapshot()).charge_rate).toBe(50);
  });

  test('restores the exact pre-Adaptive raw limit when returning to Standard', async ({ page }) => {
    test.setTimeout(60_000);

    const stop = await postJson('/api/control/force-charge/stop');
    expect(stop.ok).toBe(true);

    await page.goto(`${BASE_URL}/#/control`);
    const section = page.locator('section').filter({ hasText: 'Charging Mode' }).first();
    await section.locator('select').first().selectOption('standard');

    await waitForSnapshot((value) =>
      value.adaptive_charge_state === 'inactive'
      && value.adaptive_charge_enabled === false
      && value.charge_rate === 30,
    );

    const modeResponse = await fetch(`${BASE_URL}/api/charging-mode`);
    expect((await modeResponse.json()).mode).toBe('standard');
  });

  test('preferred rate applies the half-power DC limit in simulator physics', async () => {
    test.setTimeout(80_000);

    const config = await postJson('/api/adaptive-charge', {
      config: {
        confirmation_readings: 1,
        periods: [{
          enabled: true,
          all_day: true,
          start_hour: 0,
          start_minute: 0,
          end_hour: 0,
          end_minute: 0,
          low_soc: 20,
          recovery_soc: 40,
          preferred_rate_percent: 50,
          recovery_rate_percent: 100,
        }],
      },
    });
    expect(config.ok).toBe(true);
    expect((await postJson('/api/charging-mode', { mode: 'adaptive' })).ok).toBe(true);

    await waitForSnapshot((value) =>
      value.adaptive_charge_state === 'preferred'
      && value.adaptive_charge_desired_rate_percent === 50
      && value.charge_rate === 25,
    );

    expect((await postJson('/api/control/force-charge', { minutes: 30 })).ok).toBe(true);
    await waitForSnapshot((value) =>
      typeof value.battery_power === 'number'
      && value.battery_power <= -1_600
      && value.battery_power >= -2_000,
    );

    const value = await getSnapshot();
    expect(value.charge_rate).toBe(25); // normalized 50% -> HR111 raw 25
    expect(value.battery_power).toBeLessThanOrEqual(-1_600);
    expect(value.battery_power).toBeGreaterThanOrEqual(-2_000);

    expect((await postJson('/api/control/force-charge/stop')).ok).toBe(true);
    expect((await postJson('/api/charging-mode', { mode: 'standard' })).ok).toBe(true);
    await waitForSnapshot((current) =>
      current.adaptive_charge_state === 'inactive' && current.charge_rate === 30,
    );
  });
});
