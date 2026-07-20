/**
 * Per-spec-file backend lifecycle for the mock-E2E suite.
 *
 * Each spec file starts a FRESH headless backend in `test.beforeAll` and stops
 * it in `test.afterAll`. A brand-new process is the only reliable way to reset
 * backend-internal state that has no admin reset surface: the cached detected
 * device type, an armed Agile/Cosy/auto-winter slot, and the battery-mode
 * state machine. Sharing one backend across the whole run left that state in
 * place and silently broke later spec files — e.g. control.spec.ts passed in
 * isolation but failed in the full suite because earlier files (agile-slot,
 * aio) left the backend mid-armed or device-type-cached.
 *
 * The mock Modbus server is owned by global-setup and lives for the whole run;
 * it's fully resettable via its admin `/reset` endpoint (which restores the
 * Gen3 default register snapshot and clears captured writes), so we reset it
 * to defaults before each backend starts.
 */
import { type ChildProcess, spawn, execSync } from 'child_process';
import * as path from 'path';
import * as fs from 'fs';
import { fileURLToPath } from 'url';
import { writeTestSettings, type TestSettingsFixture } from './test-settings.js';
import { backendExecutableName } from './binary-path.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

// Must match global-setup.ts and fixture.ts.
export const BACKEND_HTTP_PORT = 17337;
const MODBUS_PORT = 18899;
const ADMIN_PORT = 18900;
const DIST_DIR = path.resolve(__dirname, '..', 'dist');
const BINARY_PATH = path.resolve(
  __dirname,
  '..',
  'src-tauri',
  'target',
  'release',
  backendExecutableName(),
);

let backendProcess: ChildProcess | null = null;
let settingsFixture: TestSettingsFixture | null = null;

function killPort(port: number): void {
  try {
    execSync(`fuser -k ${port}/tcp 2>/dev/null || true`, { stdio: 'ignore' });
  } catch {
    /* ignore — best effort */
  }
}

async function stopBackendProcess(): Promise<void> {
  if (backendProcess) {
    console.log('[backend] Stopping...');
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
  // Safety net in case the process handle was lost.
  killPort(BACKEND_HTTP_PORT);
}

/** Restore the mock Modbus server to its Gen3 default snapshot + clear writes. */
async function resetMock(): Promise<void> {
  try {
    await fetch(`http://127.0.0.1:${ADMIN_PORT}/reset`, { method: 'POST' });
  } catch {
    /* mock not up yet — global-setup starts it before any spec file runs */
  }
}

/** Resolve when `probe` returns true, or reject after `timeoutMs`. */
async function waitFor(probe: () => Promise<boolean>, label: string, timeoutMs: number): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      if (await probe()) return;
    } catch {
      /* not ready yet */
    }
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error(`${label} did not become ready within ${timeoutMs}ms`);
}

/**
 * Start a fresh, isolated backend instance for the current spec file.
 * Idempotent — calls stopBackend() first so a stray instance can't linger.
 */
export async function startBackend(): Promise<void> {
  await stopBackend();

  if (!fs.existsSync(BINARY_PATH)) {
    throw new Error(
      `Binary not found at ${BINARY_PATH}. Run 'cd src-tauri && cargo build --release' first.`,
    );
  }

  // Each backend gets its own config/history dir (per-process temp) so SQLite
  // history and settings can't leak between spec files either.
  settingsFixture = await writeTestSettings({
    tag: 'mock',
    port: MODBUS_PORT,
    serial: 'SA12345678',
    httpPort: BACKEND_HTTP_PORT,
    pollInterval: 5,
  });

  // Start each file from the Gen3 default register snapshot.
  await resetMock();

  await launchBackendProcess();
}

async function launchBackendProcess(): Promise<void> {
  if (!settingsFixture) {
    throw new Error('settingsFixture is not initialised');
  }

  console.log('[backend] Starting headless backend on port', BACKEND_HTTP_PORT);
  backendProcess = spawn(
    BINARY_PATH,
    ['--headless', '--port', String(BACKEND_HTTP_PORT), '--dist', DIST_DIR],
    {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: {
        ...process.env,
        ...settingsFixture.env,
        RUST_LOG: process.env.RUST_LOG || 'info',
      },
    },
  );

  const logLine = (prefix: string, data: Buffer): void => {
    for (const line of data.toString().trim().split('\n')) {
      if (line.trim()) console.log(`[${prefix}] ${line}`);
    }
  };
  backendProcess.stdout?.on('data', (d: Buffer) => logLine('backend:out', d));
  backendProcess.stderr?.on('data', (d: Buffer) => logLine('backend:err', d));
  backendProcess.on('exit', (code) => {
    console.log(`[backend] exited with code ${code}`);
  });

  // Wait for the HTTP server, then for the poll loop's first real snapshot.
  // Poll-loop warmup is: connect → 500ms → drain → 3× grace reads → first read.
  await waitFor(
    async () => {
      const r = await fetch(`http://127.0.0.1:${BACKEND_HTTP_PORT}/api/status`);
      return r.ok;
    },
    'Backend HTTP server',
    20_000,
  );
  await waitFor(
    async () => {
      const r = await fetch(`http://127.0.0.1:${BACKEND_HTTP_PORT}/api/snapshot`);
      const data = await r.json();
      return Boolean(data.ok && typeof data.data?.soc === 'number');
    },
    'Backend first snapshot',
    30_000,
  );
  console.log('[backend] Ready');
}

/**
 * Restart the backend process while preserving this spec file's settings dir
 * and the mock Modbus register state. Used to simulate the app crashing while
 * the inverter still has an Agile slot armed.
 */
export async function restartBackendPreservingState(): Promise<void> {
  if (!settingsFixture) {
    throw new Error('Cannot restart before startBackend()');
  }
  await stopBackendProcess();
  await launchBackendProcess();
}

/** Stop the current spec file's backend and remove its temp config dir. */
export async function stopBackend(): Promise<void> {
  await stopBackendProcess();
  if (settingsFixture) {
    await settingsFixture.cleanup();
    settingsFixture = null;
    console.log('[backend] Cleaned up temp settings dir');
  }
}
