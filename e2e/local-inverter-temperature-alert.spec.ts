/**
 * Local-only regression test for the inverter temperature alert banner.
 *
 * Uses the simulator's `--inverter-temperature` override so the backend sees
 * a real high-temperature snapshot instead of a mocked frontend store.
 */

import { test, expect } from './local-fixture.js';
import { spawn, type ChildProcess } from 'child_process';
import * as path from 'path';
import * as fs from 'fs';
import { fileURLToPath } from 'url';
import { writeTestSettings, type TestSettingsFixture } from './test-settings.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const SIMULATOR_PATH = path.resolve(
  __dirname, '..', '..', 'givenergy-simulator', 'target', 'release', 'sim-api',
);
const BACKEND_PATH = path.resolve(
  __dirname, '..', 'src-tauri', 'target', 'release', 'givenergy-local',
);
const DIST_DIR = path.resolve(__dirname, '..', 'dist');

async function startHighTemperatureInfrastructure(): Promise<{ baseUrl: string; cleanup: () => Promise<void> }> {
  const modbusPort = 9910;
  const httpPort = 9911;
  let settingsFixture: TestSettingsFixture | null = null;
  let simulator: ChildProcess | null = null;
  let backend: ChildProcess | null = null;

  if (!fs.existsSync(SIMULATOR_PATH)) throw new Error(`Simulator not found at ${SIMULATOR_PATH}`);
  if (!fs.existsSync(BACKEND_PATH)) throw new Error(`Backend not found at ${BACKEND_PATH}`);
  if (!fs.existsSync(path.join(DIST_DIR, 'index.html'))) throw new Error(`Frontend dist not found at ${DIST_DIR}`);

  const { execSync } = await import('child_process');
  execSync(`fuser -k ${modbusPort}/tcp 2>/dev/null || true`, { stdio: 'ignore' });
  execSync(`fuser -k ${httpPort}/tcp 2>/dev/null || true`, { stdio: 'ignore' });
  await new Promise((r) => setTimeout(r, 500));

  simulator = spawn(
    SIMULATOR_PATH,
    [
      'simulate',
      '--inverter', 'Gen3Hybrid',
      '--batteries', '1',
      '--battery-size', '9.5',
      '--soc', '75',
      '--solar-peak', '5000',
      '--load-profile', 'family',
      '--weather', 'clear',
      '--inverter-temperature', '70',
      '--modbus', `127.0.0.1:${modbusPort}`,
    ],
    { stdio: ['ignore', 'pipe', 'pipe'] },
  );

  simulator.stdout?.on('data', (d: Buffer) => console.log(`[sim:temp] ${d.toString().trim()}`));
  simulator.stderr?.on('data', (d: Buffer) => console.log(`[sim:temp:err] ${d.toString().trim()}`));
  await new Promise((r) => setTimeout(r, 2000));

  settingsFixture = await writeTestSettings({
    tag: 'inverter-temperature-alert',
    port: modbusPort,
    httpPort,
    pollInterval: 2,
  });

  backend = spawn(
    BACKEND_PATH,
    ['--headless', '--port', String(httpPort), '--dist', DIST_DIR],
    {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...process.env, ...settingsFixture.env },
    },
  );

  backend.stdout?.on('data', (d: Buffer) => console.log(`[backend:temp] ${d.toString().trim()}`));
  backend.stderr?.on('data', (d: Buffer) => console.log(`[backend:temp:err] ${d.toString().trim()}`));

  const start = Date.now();
  while (Date.now() - start < 30_000) {
    try {
      const resp = await fetch(`http://127.0.0.1:${httpPort}/api/status`);
      if (resp.ok) break;
    } catch { /* wait */ }
    await new Promise((r) => setTimeout(r, 500));
  }

  const snapshotStart = Date.now();
  while (Date.now() - snapshotStart < 20_000) {
    try {
      const resp = await fetch(`http://127.0.0.1:${httpPort}/api/snapshot`);
      const data = await resp.json() as { ok?: boolean; data?: { inverter_temperature?: number } };
      if (data.ok && (data.data?.inverter_temperature ?? 0) > 60) break;
    } catch { /* wait */ }
    await new Promise((r) => setTimeout(r, 500));
  }

  const cleanup = async () => {
    backend?.kill('SIGTERM');
    simulator?.kill('SIGTERM');
    await new Promise((r) => setTimeout(r, 500));
    backend?.kill('SIGKILL');
    simulator?.kill('SIGKILL');
    await settingsFixture?.cleanup();
  };

  return { baseUrl: `http://127.0.0.1:${httpPort}`, cleanup };
}

test.describe('local inverter temperature alert banner', () => {
  test('shows the high inverter temperature banner from simulator telemetry', async ({ page }) => {
    const infra = await startHighTemperatureInfrastructure();
    try {
      await page.goto(infra.baseUrl);
      await expect(page.getByText('Inverter temperature high')).toBeVisible({ timeout: 15_000 });
      await expect(page.getByText(/Inverter temperature above 60°C/)).toBeVisible();
    } finally {
      await infra.cleanup();
    }
  });
});
