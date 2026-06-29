/**
 * Local E2E global setup: starts the GivEnergy simulator and the headless backend
 * before any tests run, and tears them down after all tests finish.
 *
 * Uses the real sim-api binary instead of the mock Modbus server — this exercises
 * the full Modbus protocol stack with realistic register layouts.
 */

import { type FullConfig } from '@playwright/test';
import { ChildProcess, execSync, spawn } from 'child_process';
import * as path from 'path';
import * as fs from 'fs';
import { fileURLToPath } from 'url';
import { writeTestSettings, type TestSettingsFixture } from './test-settings.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const MODBUS_PORT = 18899;
const HTTP_PORT = 17337;
const DIST_DIR = path.resolve(__dirname, '..', 'dist');
const BACKEND_PATH = path.resolve(
  __dirname,
  '..',
  'src-tauri',
  'target',
  'release',
  'givenergy-local',
);
const SIMULATOR_PATH = path.resolve(
  __dirname,
  '..',
  '..',
  'givenergy-simulator',
  'target',
  'release',
  'sim-api',
);

let simulatorProcess: ChildProcess | null = null;
let backendProcess: ChildProcess | null = null;
let settingsFixture: TestSettingsFixture | null = null;

/**
 * Kill any leftover processes on our ports. Called on setup and on
 * unexpected exit to prevent orphaned backends from blocking the ports.
 */
function killLeftoverProcesses() {
  try {
    execSync(`fuser -k 18899/tcp 2>/dev/null || true`, { stdio: 'ignore' });
    execSync(`fuser -k 17337/tcp 2>/dev/null || true`, { stdio: 'ignore' });
  } catch { /* ignore */ }
}

// Guard against orphaned processes when the test runner is killed
// (Ctrl+C, crash, OOM, etc.) before globalTeardown runs. Without this,
// the backend and simulator keep running and block ports 17337/18899
// until manually killed — which breaks subsequent test runs.
process.on('exit', () => {
  if (backendProcess || simulatorProcess) {
    if (backendProcess) backendProcess.kill('SIGKILL');
    if (simulatorProcess) simulatorProcess.kill('SIGKILL');
  }
  killLeftoverProcesses();
});
process.on('SIGINT', () => { process.exit(2); });
process.on('SIGTERM', () => { process.exit(15); });

export default async function globalSetup(_config: FullConfig) {
  console.log('[local-setup] Starting local E2E test infrastructure...');

  // Kill any leftover processes on our ports from previous test runs
  killLeftoverProcesses();
  await new Promise((r) => setTimeout(r, 500));

  // Verify build artifacts
  if (!fs.existsSync(SIMULATOR_PATH)) {
    throw new Error(
      `Simulator not found at ${SIMULATOR_PATH}. Build it first:\n` +
      `  cd ~/repos/givenergy-simulator && cargo build --release`,
    );
  }
  if (!fs.existsSync(BACKEND_PATH)) {
    throw new Error(
      `Backend not found at ${BACKEND_PATH}. Build it first:\n` +
      `  cd src-tauri && cargo build --release`,
    );
  }
  if (!fs.existsSync(path.join(DIST_DIR, 'index.html'))) {
    throw new Error(
      `Frontend dist not found at ${DIST_DIR}. Run 'npm run build' first.`,
    );
  }

  // Start the GivEnergy simulator (Modbus TCP server)
  console.log('[local-setup] Starting simulator on port', MODBUS_PORT);
  simulatorProcess = spawn(
    SIMULATOR_PATH,
    [
      'simulate',
      '--inverter', 'Gen3Hybrid',
      '--batteries', '2',
      '--battery-size', '9.5',
      '--soc', '75',
      '--solar-peak', '5000',
      '--load-profile', 'family',
      '--weather', 'clear',
      '--modbus', `127.0.0.1:${MODBUS_PORT}`,
    ],
    { stdio: ['ignore', 'pipe', 'pipe'] },
  );

  const logLine = (prefix: string, data: Buffer) => {
    const lines = data.toString().trim().split('\n');
    for (const line of lines) {
      if (line.trim()) console.log(`[${prefix}] ${line}`);
    }
  };
  simulatorProcess.stdout?.on('data', (d: Buffer) => logLine('sim:out', d));
  simulatorProcess.stderr?.on('data', (d: Buffer) => logLine('sim:err', d));
  simulatorProcess.on('exit', (code) => {
    console.log(`[local-setup] Simulator exited with code ${code}`);
  });

  // Wait for simulator to start
  await new Promise((r) => setTimeout(r, 2000));

  // Write a test settings.json in a per-process temp dir via the shared
  // helper — same isolation contract as the mock-modbus suite, so neither
  // global setup touches the user's real ~/.givenergy-local/.
  settingsFixture = await writeTestSettings({
    tag: 'sim',
    port: MODBUS_PORT,
    httpPort: HTTP_PORT,
    pollInterval: 5,
  });

  // Start the headless backend
  console.log('[local-setup] Starting headless backend on port', HTTP_PORT);
  backendProcess = spawn(
    BACKEND_PATH,
    ['--headless', '--port', String(HTTP_PORT), '--dist', DIST_DIR],
    {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: {
        ...process.env,
        ...settingsFixture.env,
      },
    },
  );

  backendProcess.stdout?.on('data', (d: Buffer) => logLine('backend:out', d));
  backendProcess.stderr?.on('data', (d: Buffer) => logLine('backend:err', d));
  backendProcess.on('exit', (code) => {
    console.log(`[local-setup] Backend exited with code ${code}`);
  });

  // Wait for the HTTP server to become available
  const maxWait = 30_000;
  const start = Date.now();
  while (Date.now() - start < maxWait) {
    try {
      const resp = await fetch(`http://127.0.0.1:${HTTP_PORT}/api/status`);
      if (resp.ok) {
        console.log('[local-setup] Backend HTTP server ready');
        break;
      }
    } catch {
      // Not ready yet
    }
    await new Promise((r) => setTimeout(r, 500));
  }

  if (Date.now() - start >= maxWait) {
    await cleanup();
    throw new Error('Backend HTTP server did not become ready in time');
  }

  // Wait for the poll loop to connect and get first readings
  // The poll loop: connect → 500ms → drain → 3× warmup (500ms each) → first real read
  console.log('[local-setup] Waiting for poll loop to complete first reading...');
  await new Promise((r) => setTimeout(r, 10_000));

  // Verify we have a snapshot
  try {
    const resp = await fetch(`http://127.0.0.1:${HTTP_PORT}/api/snapshot`);
    const data = await resp.json();
    if (data.ok) {
      console.log(
        `[local-setup] Snapshot confirmed: SOC=${data.data.soc}%, solar=${data.data.solar_power}W`,
      );
    } else {
      console.warn('[local-setup] Warning: no snapshot yet, tests may be flaky');
    }
  } catch (e) {
    console.warn('[local-setup] Warning: could not verify snapshot:', e);
  }

  console.log('[local-setup] Ready — starting tests');
}

async function cleanup() {
  if (backendProcess) {
    console.log('[local-setup] Stopping backend...');
    backendProcess.kill('SIGTERM');
    await new Promise<void>((resolve) => {
      const timeout = setTimeout(() => {
        backendProcess?.kill('SIGKILL');
        resolve();
      }, 5000);
      backendProcess?.on('exit', () => {
        clearTimeout(timeout);
        resolve();
      });
    });
    backendProcess = null;
  }
  if (simulatorProcess) {
    console.log('[local-setup] Stopping simulator...');
    simulatorProcess.kill('SIGTERM');
    await new Promise<void>((resolve) => {
      const timeout = setTimeout(() => {
        simulatorProcess?.kill('SIGKILL');
        resolve();
      }, 5000);
      simulatorProcess?.on('exit', () => {
        clearTimeout(timeout);
        resolve();
      });
    });
    simulatorProcess = null;
  }
  // Small delay to let file handles close
  await new Promise((r) => setTimeout(r, 500));
  if (settingsFixture) {
    await settingsFixture.cleanup();
    settingsFixture = null;
    console.log('[local-setup] Cleaned up temp settings dir');
  }
}

export async function globalTeardown() {
  console.log('[local-teardown] Cleaning up...');
  await cleanup();
  console.log('[local-teardown] Done');
}
