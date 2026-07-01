/**
 * Playwright global setup for the mock-E2E suite.
 *
 * Owns the ONE long-lived process for the whole run: the mock Modbus server.
 * The mock is fully resettable via its admin `/reset` endpoint (restores the
 * Gen3 default register snapshot and clears captured writes), so it's safe to
 * share across spec files.
 *
 * The headless BACKEND is intentionally NOT started here. Each spec file
 * starts/stops its own backend in `test.beforeAll`/`test.afterAll`
 * (e2e/backend.ts) so backend-internal state with no reset surface — cached
 * device type, armed Agile/Cosy/auto-winter slots, the battery-mode state
 * machine — can't leak between spec files.
 */
import type { FullConfig } from '@playwright/test';
import * as path from 'path';
import * as fs from 'fs';
import { fileURLToPath } from 'url';
import { execSync } from 'child_process';
import { startModbusServer, stopModbusServer } from './mock-modbus.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const MODBUS_PORT = 18899;
const ADMIN_PORT = 18900;
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

export default async function globalSetup(_config: FullConfig): Promise<() => Promise<void>> {
  console.log('[global-setup] Starting E2E infrastructure...');

  // Kill leftover processes on our ports from previous runs.
  for (const port of [MODBUS_PORT, ADMIN_PORT, HTTP_PORT]) {
    try {
      execSync(`fuser -k ${port}/tcp 2>/dev/null || true`, { stdio: 'ignore' });
    } catch {
      /* ignore */
    }
  }
  await new Promise((r) => setTimeout(r, 500));

  // Fail fast if build artifacts are missing (saves a confusing timeout later).
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

  console.log('[global-setup] Starting mock Modbus server on port', MODBUS_PORT);
  await startModbusServer(MODBUS_PORT);
  console.log('[global-setup] Ready — backend is started per spec file');

  // Teardown: stop the mock. Each spec file's backend is stopped by its own
  // afterAll. Returning the teardown guarantees it runs on success, failure,
  // and globalTimeout abort.
  return async () => {
    console.log('[global-setup] Stopping mock Modbus server...');
    await stopModbusServer();
    // Safety net: free the backend/mock ports in case a spec file's afterAll
    // didn't run (e.g. the suite was aborted mid-file).
    for (const port of [HTTP_PORT, MODBUS_PORT]) {
      try {
        execSync(`fuser -k ${port}/tcp 2>/dev/null || true`, { stdio: 'ignore' });
      } catch {
        /* ignore */
      }
    }
    console.log('[global-setup] Done');
  };
}
