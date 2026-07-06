/**
 * E2E tests for the EV charger session-energy display (issue #189).
 *
 * These tests drive the full pipeline end-to-end with the real GivEnergy
 * simulator's new EVC CLI flags:
 *
 *   sim `--evc-session-energy 12.7`  →  HR 72 = 127 (kWh×10) on the wire
 *   →  backend decodes ÷10 = 12.7 kWh  →  SessionLatch  →  /api/evc/status
 *   →  WebSocket / REST  →  diagram renders `7.7kW(12.7kWh)`
 *
 * They guard the regression where the simulator forgot the ×10 wire scale
 * (so HR 72 read 10× too small and stayed flat-zero below 1 kWh), and pin
 * the SessionLatch behaviour: the kWh counts up while charging, then
 * latches at the final value after the session ends and only resets on the
 * next cable plug-in.
 *
 * Each describe block spins up its own simulator + backend so the EVC
 * state never collides with the shared global-setup instance.
 *
 * Prerequisites:
 *   1. Build frontend: npm run build
 *   2. Build backend: cd src-tauri && cargo build --release
 *   3. Build simulator: cd ~/repos/givenergy-simulator && cargo build --release
 */

import { test, expect } from './local-fixture.js';
import { execSync, spawn, type ChildProcess } from 'child_process';
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

/** Shared inverter sim args (identical to the global-setup baseline). */
const INVERTER_ARGS = [
  '--inverter', 'Gen3Hybrid',
  '--batteries', '2',
  '--battery-size', '9.5',
  '--soc', '75',
  '--solar-peak', '5000',
  '--load-profile', 'family',
  '--weather', 'clear',
];

function verifyArtifacts() {
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
    throw new Error(`Frontend dist not found at ${DIST_DIR}. Run 'npm run build' first.`);
  }
}

function killPort(port: number) {
  try {
    execSync(`fuser -k ${port}/tcp 2>/dev/null || true`, { stdio: 'ignore' });
  } catch { /* ignore */ }
}

/** Spawn a simulator with the given EVC args; returns the process handle. */
function spawnSim(
  modbusPort: number,
  evcPort: number,
  evcArgs: string[],
  logTag: string,
): ChildProcess {
  const args = [
    'simulate',
    ...INVERTER_ARGS,
    '--modbus', `127.0.0.1:${modbusPort}`,
    '--evc-enabled',
    '--evc-port', String(evcPort),
    ...evcArgs,
  ];
  const sim = spawn(SIMULATOR_PATH, args, { stdio: ['ignore', 'pipe', 'pipe'] });
  const log = (d: Buffer) => {
    for (const line of d.toString().trim().split('\n')) {
      if (line.trim()) console.log(`[sim:${logTag}] ${line}`);
    }
  };
  sim.stdout?.on('data', log);
  sim.stderr?.on('data', log);
  return sim;
}

/** Kill a child process gracefully (SIGTERM → SIGKILL after 3s). */
async function killProc(proc: ChildProcess | null, tag: string) {
  if (!proc || proc.exitCode !== null) return;
  console.log(`[cleanup] Stopping ${tag}...`);
  proc.kill('SIGTERM');
  await new Promise<void>((resolve) => {
    const t = setTimeout(() => { proc.kill('SIGKILL'); resolve(); }, 3000);
    proc.on('exit', () => { clearTimeout(t); resolve(); });
  });
}

interface EvcInfra {
  httpPort: number;
  evcPort: number;
  /** Kill the current sim (backend will start reconnecting). */
  stopSim: () => Promise<void>;
  /** Spawn a fresh sim on the same ports with new EVC args. */
  startSim: (evcArgs: string[], tag: string) => ChildProcess;
  cleanup: () => Promise<void>;
}

/**
 * Boot a simulator (with EVC) and a backend pointed at it. Returns handles
 * that let a test restart the sim mid-run (for the latch test) plus a
 * full cleanup.
 */
async function startEvcInfra(
  evcArgs: string[],
  modbusPort: number,
  httpPort: number,
  evcPort: number,
  tag: string,
): Promise<EvcInfra> {
  verifyArtifacts();
  killPort(modbusPort); killPort(httpPort); killPort(evcPort);
  await new Promise((r) => setTimeout(r, 500));

  let sim: ChildProcess | null = spawnSim(modbusPort, evcPort, evcArgs, tag);
  await new Promise((r) => setTimeout(r, 2000));

  const settingsFixture: TestSettingsFixture = await writeTestSettings({
    tag,
    port: modbusPort,
    httpPort,
    pollInterval: 2,
    evcHost: '127.0.0.1',
    evcPort,
  });

  const backend = spawn(
    BACKEND_PATH,
    ['--headless', '--port', String(httpPort), '--dist', DIST_DIR],
    { stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, ...settingsFixture.env } },
  );
  const blog = (d: Buffer) => {
    for (const line of d.toString().trim().split('\n')) {
      if (line.trim()) console.log(`[backend:${tag}] ${line}`);
    }
  };
  backend.stdout?.on('data', blog);
  backend.stderr?.on('data', blog);

  // Wait for the HTTP server.
  const start = Date.now();
  while (Date.now() - start < 30_000) {
    try {
      const r = await fetch(`http://127.0.0.1:${httpPort}/api/status`);
      if (r.ok) break;
    } catch { /* not ready */ }
    await new Promise((r) => setTimeout(r, 500));
  }
  if (Date.now() - start >= 30_000) {
    await killProc(backend, `backend:${tag}`);
    await killProc(sim, `sim:${tag}`);
    await settingsFixture.cleanup();
    throw new Error(`Backend did not become ready for ${tag}`);
  }

  const stopSim = async () => { await killProc(sim, `sim:${tag}`); sim = null; };
  const startSim = (args: string[], t: string) => {
    sim = spawnSim(modbusPort, evcPort, args, t);
    return sim;
  };
  const cleanup = async () => {
    await killProc(backend, `backend:${tag}`);
    await killProc(sim, `sim:${tag}`);
    await new Promise((r) => setTimeout(r, 500));
    await settingsFixture.cleanup();
  };

  return { httpPort, evcPort, stopSim, startSim, cleanup };
}

/**
 * Poll /api/evc/status until the EVC is reachable, then return the parsed
 * snapshot. Throws if it doesn't come up within the timeout — the EVC poll
 * loop runs every 10 s, so allow generous time.
 */
async function waitForEvcSnapshot(httpPort: number, timeoutMs = 40_000): Promise<{
  charging_state: string;
  connection_status: string;
  active_power: number;
  session_energy_kwh: number;
}> {
  const start = Date.now();
  let lastErr: string | null = null;
  while (Date.now() - start < timeoutMs) {
    try {
      const r = await fetch(`http://127.0.0.1:${httpPort}/api/evc/status`);
      const data = await r.json();
      if (data.ok && data.reachable && data.snapshot) return data.snapshot;
      lastErr = `reachable=${data.reachable}`;
    } catch (e) { lastErr = String(e); }
    await new Promise((r) => setTimeout(r, 1000));
  }
  throw new Error(`EVC snapshot never became reachable within ${timeoutMs}ms (${lastErr})`);
}

// ===========================================================================
// Seeded session energy flows through + increments while charging
// ===========================================================================

test.describe('EVC session energy — seeded value flows through', () => {
  let infra: EvcInfra;

  test.beforeAll(async () => {
    // Seed 12.7 kWh, cable in, charging at ~7.7 kW (32 A × ~240 V).
    infra = await startEvcInfra(
      ['--evc-cable', '--evc-start', '--evc-session-energy', '12.7'],
      18930, 18931, 18932, 'evc-seed',
    );
  });
  test.afterAll(async () => { await infra.cleanup(); });

  test('decodes the seeded HR 72 value (kWh×10 wire scale) at full magnitude', async () => {
    // The regression: the sim used to omit the ×10 wire scale, so 12.7 kWh
    // arrived as 0.1 (truncated `as u16` of 1.2... → 1 → ÷10). With the
    // fix, HR 72 = 127 → decode = 12.7. HR 72 wire resolution is 0.1 kWh
    // and f32(12.7) is 12.6999..., so assert a band around the seed that's
    // far above the broken ~0.1–1.3 kWh decode.
    const snap = await waitForEvcSnapshot(infra.httpPort);
    expect(snap.charging_state).toBe('Charging');
    expect(snap.connection_status).toBe('Connected');
    expect(snap.session_energy_kwh).toBeGreaterThan(12.5);
    expect(snap.session_energy_kwh).toBeLessThan(14);
  });

  test('increments as the charge continues (crosses a 0.1 kWh wire boundary)', async () => {
    // HR 72 is encoded as kWh×10 in a u16, so the wire resolution is 0.1 kWh.
    // At ~7.7 kW that's one tick roughly every 47 s — the value only visibly
    // advances when it crosses a 0.1 boundary. Grab an early reading, wait
    // long enough to cross at least one, and confirm it moved strictly up.
    test.setTimeout(90_000);
    const first = await waitForEvcSnapshot(infra.httpPort);
    await new Promise((r) => setTimeout(r, 65_000));
    const second = await waitForEvcSnapshot(infra.httpPort);
    expect(second.session_energy_kwh).toBeGreaterThan(first.session_energy_kwh);
  });
});

// ===========================================================================
// Session energy — UI renders the inline `kW(kWh)` value
// ===========================================================================

test.describe('EVC session energy — diagram display', () => {
  let infra: EvcInfra;

  test.beforeAll(async () => {
    infra = await startEvcInfra(
      ['--evc-cable', '--evc-start', '--evc-session-energy', '23'],
      18940, 18941, 18942, 'evc-ui',
    );
  });
  test.afterAll(async () => { await infra.cleanup(); });

  test('renders the EV node value as `kW(kWh)` on the status page', async ({ page }) => {
    // Wait for the backend to have an EVC snapshot before loading the UI,
    // so the WS seed / REST fetch lands immediately.
    await waitForEvcSnapshot(infra.httpPort);
    await page.goto(`http://127.0.0.1:${infra.httpPort}/`);
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // The EV node value is the power with the session energy in parens,
    // e.g. `7.7kW(23kWh)`. 23 kWh crosses the integer threshold so no dp.
    await expect(page.locator('text=/\\d+(\\.\\d+)?kW\\(\\d+(\\.\\d+)?kWh\\)/').first())
      .toBeVisible({ timeout: 20_000 });
  });
});

// ===========================================================================
// Session energy — latch holds the value after the charge stops
// ===========================================================================

test.describe('EVC session energy — latched after charge stops', () => {
  let infra: EvcInfra;

  test.beforeAll(async () => {
    // Phase A: charging, seeded high so the held value is unmistakable.
    infra = await startEvcInfra(
      ['--evc-cable', '--evc-start', '--evc-session-energy', '40'],
      18950, 18951, 18952, 'evc-latch',
    );
  });
  test.afterAll(async () => { await infra.cleanup(); });

  test('keeps showing the completed session total after charging stops', async () => {
    test.setTimeout(120_000); // generous: sim restart + EVC reconnect backoff

    // 1. Confirm a charging reading well above zero.
    const whileCharging = await waitForEvcSnapshot(infra.httpPort);
    expect(whileCharging.session_energy_kwh).toBeGreaterThan(10);
    const peak = whileCharging.session_energy_kwh;

    // 2. Stop the sim and restart it on the SAME ports with the cable
    //    still plugged in but NOT charging (no --evc-start, no seed →
    //    charging_state leaves "Charging", HR 72 reads 0). The backend's
    //    SessionLatch must hold the last peak rather than dropping to 0.
    await infra.stopSim();
    await new Promise((r) => setTimeout(r, 1000));
    infra.startSim(['--evc-cable'], 'evc-latch-idle');

    // 3. Wait for the backend to reconnect and poll the now-idle charger.
    //    The reconnect backoff is ~10 s + a 10 s poll, so allow ample time.
    const afterStop = await waitForEvcSnapshot(infra.httpPort, 90_000);

    // The latch must hold: the displayed energy did NOT reset to 0 when
    // the session ended (cable stayed connected → no reset transition).
    expect(afterStop.session_energy_kwh).toBeGreaterThan(peak * 0.5);
    expect(afterStop.session_energy_kwh).toBeGreaterThan(10);
  });
});
