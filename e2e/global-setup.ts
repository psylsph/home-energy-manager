/**
 * Playwright global setup: starts the mock Modbus server and headless backend
 * before any tests run, and tears them down after all tests finish.
 */

import { type FullConfig } from '@playwright/test';
import { ChildProcess, spawn } from 'child_process';
import * as path from 'path';
import * as fs from 'fs';
import * as os from 'os';
import { fileURLToPath } from 'url';
import { startModbusServer, stopModbusServer } from './mock-modbus.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const MODBUS_PORT = 18899;
const HTTP_PORT = 17337;
const DIST_DIR = path.resolve(__dirname, '..', 'dist');
const BINARY_PATH = path.resolve(
  __dirname,
  '..',
  'src-tauri',
  'target',
  'release',
  'givenergy-local',
);

let backendProcess: ChildProcess | null = null;
let configDir: string;

export default async function globalSetup(_config: FullConfig) {
  console.log('[global-setup] Starting E2E test infrastructure...');

  // Kill any leftover processes on our ports from previous test runs
  try {
    const { execSync } = await import('child_process');
    execSync(`fuser -k 18899/tcp 2>/dev/null || true`, { stdio: 'ignore' });
    execSync(`fuser -k 18900/tcp 2>/dev/null || true`, { stdio: 'ignore' });
    execSync(`fuser -k 17337/tcp 2>/dev/null || true`, { stdio: 'ignore' });
    await new Promise((r) => setTimeout(r, 500));
  } catch { /* ignore */ }

  // Verify build artifacts
  if (!fs.existsSync(BINARY_PATH)) {
    throw new Error(
      `Binary not found at ${BINARY_PATH}. Run 'cd src-tauri && cargo build --release' first.`,
    );
  }
  if (!fs.existsSync(path.join(DIST_DIR, 'index.html'))) {
    throw new Error(
      `Frontend dist not found at ${DIST_DIR}. Run 'npm run build' first.`,
    );
  }

  // Start mock Modbus server
  console.log('[global-setup] Starting mock Modbus server on port', MODBUS_PORT);
  await startModbusServer(MODBUS_PORT);

  // Create temp config directory with settings pointing at mock server
  configDir = path.join(os.tmpdir(), `givenergy-e2e-${process.pid}`);
  fs.mkdirSync(configDir, { recursive: true });
  fs.mkdirSync(path.join(configDir, '.givenergy-local'), { recursive: true });

  const settings = {
    host: '127.0.0.1',
    port: MODBUS_PORT,
    serial: 'SA12345678',
    poll_interval: 5,
    http_port: HTTP_PORT,
    auto_connect: true,
    import_tariff: 0.285,
    export_tariff: 0.15,
    auto_winter_enabled: false,
    auto_winter_cold_threshold: 8.0,
    auto_winter_recovery_threshold: 12.0,
    auto_winter_target_soc: 80,
    auto_winter_debounce_readings: 2,
    cosy_enabled: false,
    cosy_slots: [
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
    ],
  };
  fs.writeFileSync(
    path.join(configDir, '.givenergy-local', 'settings.json'),
    JSON.stringify(settings, null, 2),
  );

  // Start the headless backend
  console.log('[global-setup] Starting headless backend on port', HTTP_PORT);
  backendProcess = spawn(
    BINARY_PATH,
    ['--headless', '--port', String(HTTP_PORT), '--dist', DIST_DIR],
    {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: {
        ...process.env,
        HOME: configDir,
        GIVENERGY_LOCAL_CONFIG_DIR: path.join(configDir, '.givenergy-local'),
      },
    },
  );

  const logLine = (prefix: string, data: Buffer) => {
    const lines = data.toString().trim().split('\n');
    for (const line of lines) {
      if (line.trim()) console.log(`[${prefix}] ${line}`);
    }
  };
  backendProcess.stdout?.on('data', (d: Buffer) => logLine('backend:out', d));
  backendProcess.stderr?.on('data', (d: Buffer) => logLine('backend:err', d));

  backendProcess.on('exit', (code) => {
    console.log(`[global-setup] Backend exited with code ${code}`);
  });

  // Wait for the HTTP server to become available
  const maxWait = 20_000;
  const start = Date.now();
  while (Date.now() - start < maxWait) {
    try {
      const resp = await fetch(`http://127.0.0.1:${HTTP_PORT}/api/status`);
      if (resp.ok) {
        console.log('[global-setup] Backend HTTP server ready');
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
  // The poll loop does: connect → 500ms → drain → 3× warmup (500ms each) → first real read
  // Total warmup: ~2.5s + poll interval
  console.log('[global-setup] Waiting for poll loop to complete first reading...');
  await new Promise((r) => setTimeout(r, 8000));

  // Verify we have a snapshot
  try {
    const resp = await fetch(`http://127.0.0.1:${HTTP_PORT}/api/snapshot`);
    const data = await resp.json();
    if (data.ok) {
      console.log(`[global-setup] Snapshot confirmed: SOC=${data.data.soc}%, solar=${data.data.solar_power}W`);
    } else {
      console.warn('[global-setup] Warning: no snapshot yet, tests may be flaky');
    }
  } catch (e) {
    console.warn('[global-setup] Warning: could not verify snapshot:', e);
  }

  console.log('[global-setup] Ready — starting tests');
}

async function cleanup() {
  if (backendProcess) {
    console.log('[global-setup] Stopping backend...');
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
  await stopModbusServer();
  // Small delay to let file handles close
  await new Promise((r) => setTimeout(r, 500));
  if (configDir && fs.existsSync(configDir)) {
    try {
      fs.rmSync(configDir, { recursive: true, force: true });
      console.log('[global-setup] Cleaned up temp dir:', configDir);
    } catch (e) {
      console.warn('[global-setup] Failed to clean temp dir:', e);
    }
  }
}

// Playwright calls globalTeardown if exported
export async function globalTeardown() {
  console.log('[global-teardown] Cleaning up...');
  await cleanup();
  console.log('[global-teardown] Done');
}
