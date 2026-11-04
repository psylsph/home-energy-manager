/**
 * Charge cap (issue #426) against the GivEnergy Simulator, one inverter
 * family at a time. Each describe starts its own simulator + headless backend.
 *
 * Paths covered:
 *   - All-in-One (HR 318 mode + HR 319/320 window): holds with the all-day
 *     window, gives charging back to Force Charge, resumes at the resume
 *     level and on switch-off with the old window put back, releases on a
 *     graceful shutdown, and carries on from its record after a hard kill.
 *   - Gen3 hybrid on ARM firmware ≥ 312 (window via the HR 318-320 probe).
 *   - AC-coupled (ARM ≥ 200): HR 318 alone, as GivTCP does there, so the
 *     window is never written; also released on shutdown and after a kill.
 *   - Gen2 hybrid and AC three-phase: refused and hidden. GivTCP, with far
 *     more user feedback, gives these no pause-charge.
 *
 * Prerequisites: npm run build; (cd src-tauri && cargo build --release);
 * the simulator built at ~/repos/givenergy-simulator/target/release/sim-api.
 */

import { test, expect } from './local-fixture.js';
import { spawn, type ChildProcess } from 'child_process';
import * as path from 'path';
import * as fs from 'fs';
import { fileURLToPath } from 'url';
import { writeTestSettings, type TestSettingsFixture } from './test-settings.js';
import { simulatorBinaryPath } from './binary-path.js';
import { attachErrorHandler } from './process-errors.js';
import { stopChildProcess } from './process-lifecycle.js';
import { killPort } from './port-cleanup.mjs';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const SIMULATOR_PATH = simulatorBinaryPath();
const BACKEND_PATH = path.resolve(__dirname, '..', 'src-tauri', 'target', 'release', 'givenergy-local');
const DIST_DIR = path.resolve(__dirname, '..', 'dist');

const CAP_WINDOW = { start: 0, end: 2359 };

interface Snapshot {
  soc: number;
  device_type_code: string;
  battery_power: number;
  battery_pause_mode_raw: number | null;
  battery_pause_slot_start_raw: number | null;
  battery_pause_slot_end_raw: number | null;
  charge_cap_enabled?: boolean;
  charge_cap_soc?: number;
  charge_cap_state?: string;
}

interface Rig {
  httpPort: number;
  base: string;
  /** Stop the backend gracefully (SIGTERM) or hard (SIGKILL), keeping the simulator and settings. */
  stopBackend: (hard?: boolean) => Promise<void>;
  startBackend: () => Promise<void>;
  cleanup: () => Promise<void>;
}

function pipe(child: ChildProcess, label: string) {
  for (const stream of [child.stdout, child.stderr]) {
    stream?.on('data', (d: Buffer) => {
      for (const line of d.toString().trim().split('\n')) {
        if (line.trim()) console.log(`[${label}] ${line}`);
      }
    });
  }
}

async function startRig(inverter: string, basePort: number, soc: number): Promise<Rig> {
  const modbusPort = basePort;
  const httpPort = basePort + 1;
  killPort(modbusPort);
  killPort(httpPort);
  await new Promise((r) => setTimeout(r, 500));

  if (!SIMULATOR_PATH || !fs.existsSync(SIMULATOR_PATH)) {
    throw new Error('Simulator not found; build ~/repos/givenergy-simulator with cargo build --release');
  }
  if (!fs.existsSync(BACKEND_PATH)) throw new Error(`Backend not found at ${BACKEND_PATH}`);
  if (!fs.existsSync(path.join(DIST_DIR, 'index.html'))) throw new Error(`Frontend dist not found at ${DIST_DIR}`);

  const simulator = spawn(
    SIMULATOR_PATH,
    [
      'simulate',
      '--inverter', inverter,
      '--batteries', '1',
      '--battery-size', '9.5',
      '--soc', String(soc),
      '--solar-peak', '5000',
      '--load-profile', 'minimal',
      '--weather', 'clear',
      '--modbus', `127.0.0.1:${modbusPort}`,
    ],
    { stdio: ['ignore', 'pipe', 'pipe'] },
  );
  attachErrorHandler(simulator, `simulator:${inverter}`);
  pipe(simulator, `sim:${inverter}`);
  await new Promise((r) => setTimeout(r, 2000));

  const settings: TestSettingsFixture = await writeTestSettings({
    tag: `charge-cap-${inverter}`,
    port: modbusPort,
    httpPort,
    pollInterval: 2,
    writePacingMs: 25,
  });

  let backend: ChildProcess | null = null;
  const startBackend = async () => {
    backend = spawn(BACKEND_PATH, ['--headless', '--port', String(httpPort), '--dist', DIST_DIR], {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...process.env, ...settings.env },
    });
    attachErrorHandler(backend, `backend:${inverter}`);
    pipe(backend, `backend:${inverter}`);
    const start = Date.now();
    while (Date.now() - start < 30_000) {
      try {
        const resp = await fetch(`http://127.0.0.1:${httpPort}/api/status`);
        if (resp.ok) return;
      } catch {
        // not up yet
      }
      await new Promise((r) => setTimeout(r, 300));
    }
    throw new Error(`Backend did not become ready for ${inverter}`);
  };
  const stopBackend = async (hard = false) => {
    const child = backend;
    backend = null;
    if (!child) return;
    if (hard) {
      child.kill('SIGKILL');
      await new Promise((r) => setTimeout(r, 500));
    } else {
      // The shutdown release waits up to 6 s for its writes.
      await stopChildProcess(child, `backend:${inverter}`, 10_000);
    }
    killPort(httpPort);
  };

  await startBackend();
  return {
    httpPort,
    base: `http://127.0.0.1:${httpPort}`,
    startBackend,
    stopBackend,
    cleanup: async () => {
      await stopBackend();
      await stopChildProcess(simulator, `simulator:${inverter}`, 3000);
      await settings.cleanup();
    },
  };
}

async function getJson<T>(url: string): Promise<T> {
  const resp = await fetch(url);
  return (await resp.json()) as T;
}

async function post(url: string, body?: unknown): Promise<{ status: number; data: Record<string, unknown> }> {
  const resp = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  return { status: resp.status, data: (await resp.json()) as Record<string, unknown> };
}

async function snapshot(base: string): Promise<Snapshot | null> {
  try {
    const res = await getJson<{ ok: boolean; data: Snapshot | null }>(`${base}/api/snapshot`);
    return res.ok ? res.data : null;
  } catch {
    return null;
  }
}

/** Poll the snapshot until `check` passes (it returns a reason while waiting). */
async function waitFor(base: string, what: string, check: (s: Snapshot) => string | null, timeoutMs = 45_000): Promise<Snapshot> {
  const start = Date.now();
  let last = 'no snapshot yet';
  while (Date.now() - start < timeoutMs) {
    const s = await snapshot(base);
    if (s) {
      const why = check(s);
      if (why === null) return s;
      last = why;
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`Timed out waiting for ${what}: ${last}`);
}

const connected = (s: Snapshot) => (s.battery_pause_mode_raw == null ? 'pause registers not read yet' : null);

function capState(state: string, mode: number) {
  return (s: Snapshot) =>
    s.charge_cap_state === state && s.battery_pause_mode_raw === mode
      ? null
      : `state=${s.charge_cap_state} HR318=${s.battery_pause_mode_raw}`;
}

function windowIs(win: { start: number | null; end: number | null }) {
  return (s: Snapshot) =>
    s.battery_pause_slot_start_raw === win.start && s.battery_pause_slot_end_raw === win.end
      ? null
      : `window=${s.battery_pause_slot_start_raw}-${s.battery_pause_slot_end_raw}`;
}

const both = (...checks: ((s: Snapshot) => string | null)[]) => (s: Snapshot) => {
  for (const c of checks) {
    const why = c(s);
    if (why) return why;
  }
  return null;
};

/** A cap below the simulator's SOC (so it holds), with its resume level. */
function capBelow(soc: number) {
  const cap = Math.max(50, Math.min(99, soc - 10));
  return { enabled: true, cap_soc: cap, resume_soc: cap - 5 };
}

/** A cap above the SOC with the resume level above it too, so a hold lets go at once. */
function resumeAbove(soc: number) {
  const cap = Math.min(99, soc + 10);
  return { enabled: true, cap_soc: cap, resume_soc: Math.min(cap - 2, soc + 5) };
}

interface Family {
  inverter: string;
  port: number;
  /** Exercise Force Charge yielding on this family. */
  forceCharge: boolean;
  /** Paused with HR 318 alone: the window is left as it was. */
  modeOnly: boolean;
  /** Exercise the shutdown release and the restart after a hard kill. */
  restarts: boolean;
}

const SUPPORTED: Family[] = [
  { inverter: 'AllInOne6', port: 18960, forceCharge: true, modeOnly: false, restarts: true },
  { inverter: 'Gen3Hybrid', port: 18962, forceCharge: true, modeOnly: false, restarts: false },
  { inverter: 'ACCoupled', port: 18964, forceCharge: true, modeOnly: true, restarts: true },
];

/** Families GivTCP gives no pause-charge: the cap is refused and hidden. */
const UNSUPPORTED: { inverter: string; port: number; why: string }[] = [
  { inverter: 'Gen2Hybrid', port: 18968, why: 'no native pause' },
  { inverter: 'ACCoupledThreePhase6kW', port: 18966, why: 'no pause in GivTCP' },
];

for (const family of SUPPORTED) {
  test.describe(`Charge cap: ${family.inverter}`, () => {
    test.describe.configure({ mode: 'serial' });
    test.setTimeout(240_000);
    let rig: Rig;
    let prior: { start: number | null; end: number | null };
    let soc: number;
    /** The window while the cap holds: its own, or untouched when mode-only. */
    const holdWindow = () => (family.modeOnly ? prior : CAP_WINDOW);

    test.beforeAll(async () => {
      test.setTimeout(120_000);
      rig = await startRig(family.inverter, family.port, 80);
      const first = await waitFor(rig.base, 'the first pause-register read', connected);
      prior = { start: first.battery_pause_slot_start_raw, end: first.battery_pause_slot_end_raw };
      soc = first.soc;
      expect(first.battery_pause_mode_raw, 'the simulator starts unpaused').toBe(0);
    });

    test.afterAll(async () => {
      await rig?.cleanup();
    });

    test('is offered on this inverter', async () => {
      const status = await getJson<Record<string, unknown>>(`${rig.base}/api/charge-cap`);
      expect(status.supported).toBe(true);
      expect(status.enabled).toBe(false);
      expect(status.state).toBe('off');
    });

    test('pauses charging above the cap', async () => {
      const saved = await post(`${rig.base}/api/charge-cap`, capBelow(soc));
      expect(saved.status, JSON.stringify(saved.data)).toBe(200);
      const held = await waitFor(
        rig.base,
        'the cap to hold',
        both(capState('holding', 1), windowIs(holdWindow())),
      );
      expect(held.charge_cap_enabled).toBe(true);
      expect((await getJson<Record<string, unknown>>(`${rig.base}/api/charge-cap`)).holding).toBe(true);
    });

    if (family.forceCharge) {
      test('gives charging back to Force Charge and pauses again afterwards', async () => {
        const started = await post(`${rig.base}/api/control/force-charge`, { minutes: 30 });
        expect(started.status, JSON.stringify(started.data)).toBe(200);
        await waitFor(rig.base, 'the cap to yield', both(capState('yielding', 0), windowIs(prior)));
        // With the pause lifted the simulator really charges.
        await waitFor(rig.base, 'the battery to charge', (s) =>
          s.battery_power < -100 ? null : `battery_power=${s.battery_power}`,
        );

        const stopped = await post(`${rig.base}/api/control/force-charge/stop`);
        expect(stopped.data.ok, JSON.stringify(stopped.data)).toBe(true);
        await waitFor(rig.base, 'the cap to hold again', both(capState('holding', 1), windowIs(holdWindow())), 90_000);
      });
    }

    test('lets go at the resume level and puts the old window back', async () => {
      const now = (await snapshot(rig.base))!;
      const saved = await post(`${rig.base}/api/charge-cap`, resumeAbove(now.soc));
      expect(saved.status, JSON.stringify(saved.data)).toBe(200);
      await waitFor(rig.base, 'the cap to let go', both(capState('waiting', 0), windowIs(prior)));
      const status = await getJson<Record<string, unknown>>(`${rig.base}/api/charge-cap`);
      expect(status.holding, 'the record is gone once the release reads back').toBe(false);
    });

    test('switching off releases a hold', async () => {
      const now = (await snapshot(rig.base))!;
      await post(`${rig.base}/api/charge-cap`, capBelow(now.soc));
      await waitFor(rig.base, 'the cap to hold', capState('holding', 1));
      const off = await post(`${rig.base}/api/charge-cap`, { ...capBelow(now.soc), enabled: false });
      expect(off.status).toBe(200);
      await waitFor(rig.base, 'the release', both(capState('off', 0), windowIs(prior)));
      expect((await snapshot(rig.base))!.charge_cap_enabled).toBe(false);
    });

    test('shows the section on the Control page and the cap line on the battery', async ({ page }) => {
      const now = (await snapshot(rig.base))!;
      const config = capBelow(now.soc);
      await post(`${rig.base}/api/charge-cap`, config);
      await waitFor(rig.base, 'the cap to hold', capState('holding', 1));

      await page.goto(`${rig.base}/#/control`);
      await expect(page.getByRole('heading', { name: 'Charge Cap' })).toBeVisible({ timeout: 20_000 });
      await expect(page.getByTestId('charge-cap-state')).toHaveText(
        `Charging paused at ${config.cap_soc}%. Charges again at ${config.resume_soc}%.`,
      );
      await expect(page.getByTestId('charge-cap-hem-warning')).toBeVisible();
      await expect(page.getByTestId('charge-cap-balance-warning')).toContainText('every 30 days');

      await page.goto(`${rig.base}/#/battery`);
      await expect(page.getByTestId('battery-gauge-cap').first()).toBeAttached({ timeout: 20_000 });

      // Switch it off from the page.
      await page.goto(`${rig.base}/#/control`);
      await page.getByRole('switch', { name: 'Enable Charge Cap' }).click();
      const section = page.getByTestId('charge-cap-section');
      await section.getByRole('button', { name: 'Save' }).click();
      await expect(section.getByText('Saved')).toBeVisible();
      await waitFor(rig.base, 'the release', both(capState('off', 0), windowIs(prior)));
    });

    if (family.restarts) {
      test('a graceful shutdown hands charging back', async () => {
        const now = (await snapshot(rig.base))!;
        await post(`${rig.base}/api/charge-cap`, capBelow(now.soc));
        await waitFor(rig.base, 'the cap to hold', capState('holding', 1));
        await rig.stopBackend();
        // Back up with the cap still on: the first fresh read must show the
        // release landed before the cap pauses again.
        await rig.startBackend();
        const first = await waitFor(rig.base, 'the first read after restart', connected);
        expect(first.battery_pause_mode_raw, 'released on shutdown').toBe(0);
        expect(first.battery_pause_slot_end_raw).toBe(prior.end);
        await waitFor(rig.base, 'the cap to hold again', capState('holding', 1));
      });

      test('after a hard kill mid-hold it carries on from its record', async () => {
        await waitFor(rig.base, 'the cap to hold', capState('holding', 1));
        await rig.stopBackend(true);
        await rig.startBackend();
        // The pause survived the kill, and the record says it's the cap's.
        await waitFor(rig.base, 'the cap to recognise its pause', capState('holding', 1));
        const status = await getJson<Record<string, unknown>>(`${rig.base}/api/charge-cap`);
        expect(status.holding).toBe(true);
        // So it can still let go and put the old window back.
        await post(`${rig.base}/api/charge-cap`, { ...capBelow(80), enabled: false });
        await waitFor(rig.base, 'the release', both(capState('off', 0), windowIs(prior)));
      });
    }
  });
}

for (const family of UNSUPPORTED) {
test.describe(`Charge cap: ${family.inverter} (${family.why})`, () => {
  test.describe.configure({ mode: 'serial' });
  test.setTimeout(120_000);
  let rig: Rig;

  test.beforeAll(async () => {
    test.setTimeout(120_000);
    rig = await startRig(family.inverter, family.port, 80);
    await waitFor(rig.base, 'the first snapshot', (s) => (s.device_type_code ? null : 'no device type yet'));
  });

  test.afterAll(async () => {
    await rig?.cleanup();
  });

  test('refuses to switch on and stays hidden', async ({ page }) => {
    const status = await getJson<Record<string, unknown>>(`${rig.base}/api/charge-cap`);
    expect(status.supported).toBe(false);
    expect(status.state).toBe('unsupported');

    const refused = await post(`${rig.base}/api/charge-cap`, { enabled: true, cap_soc: 70, resume_soc: 65 });
    expect(refused.status).toBe(400);
    expect(String(refused.data.error)).toMatch(/can't pause charging/);

    await page.goto(`${rig.base}/#/control`);
    // The page has loaded its other automations…
    await expect(page.getByRole('heading', { name: 'Auto Winter Mode' })).toBeVisible({ timeout: 20_000 });
    // …but not the cap, and the battery shows no cap line.
    await expect(page.getByRole('heading', { name: 'Charge Cap' })).toHaveCount(0);
    await page.goto(`${rig.base}/#/battery`);
    await expect(page.getByTestId('battery-gauge-cap')).toHaveCount(0);
  });
});
}
