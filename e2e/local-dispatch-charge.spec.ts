/**
 * Battery top-up during an Intelligent Octopus Go smart charge (issue #434)
 * against the GivEnergy Simulator's AC-coupled inverter (the 0x3001 family
 * the feature is proven on first).
 *
 * A small mock of Octopus's GraphQL API (`octopus_api_base_url`) lists one
 * smart dispatch that started a minute ago and ends shortly. The test proves:
 *   - with the hold and the top-up on, the battery charges during it (charge
 *     slot 1 over the dispatch, Force Charge to 100%);
 *   - at its end HEM puts back the user's own slot 1, charge enable and
 *     target SOC, and the battery is no longer charged.
 *
 * Prerequisites: npm run build; (cd src-tauri && cargo build --release);
 * the simulator built at ~/repos/givenergy-simulator/target/release/sim-api.
 */

import { test, expect } from './local-fixture.js';
import { spawn, type ChildProcess } from 'child_process';
import * as http from 'http';
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

const MODBUS_PORT = 18980;
const HTTP_PORT = 18981;
const OCTOPUS_PORT = 18982;

/** How long the mock's dispatch has left when the hold is switched on. */
const DISPATCH_LEFT_MS = 150_000;

interface Slot {
  enabled: boolean;
  start_hour: number;
  start_minute: number;
  end_hour: number;
  end_minute: number;
}

interface Snapshot {
  soc: number;
  device_type_code: string;
  battery_power: number;
  enable_charge: boolean;
  target_soc: number;
  battery_reserve: number;
  charge_slots: Slot[];
  dispatch_charge_active?: boolean;
}

interface Dispatches {
  charge_enabled?: boolean;
  charge_state?: string;
  charge_reason?: string | null;
  charge_note?: string | null;
}

/** The mock's planned dispatches, replaced by the test. */
let planned: { start: string; end: string; type: string; energyAddedKwh: number }[] = [];

function startOctopusMock(): Promise<http.Server> {
  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', (chunk: Buffer) => (body += chunk.toString()));
    req.on('end', () => {
      let query = '';
      try {
        query = String((JSON.parse(body) as { query?: string }).query ?? '');
      } catch {
        // not JSON: answered as unknown below
      }
      let data: unknown;
      if (query.includes('obtainKrakenToken')) {
        data = { obtainKrakenToken: { token: 'mock-token' } };
      } else if (query.includes('devices(')) {
        data = {
          devices: [{ id: 'car-1', deviceType: 'ELECTRIC_VEHICLES', status: { current: 'LIVE' } }],
        };
      } else if (query.includes('flexPlannedDispatches')) {
        data = { flexPlannedDispatches: planned };
      }
      res.setHeader('Content-Type', 'application/json');
      if (data === undefined) {
        // Anything else HEM asks (sessions, account data) isn't part of this test.
        res.end(JSON.stringify({ errors: [{ message: 'not mocked' }] }));
      } else {
        res.end(JSON.stringify({ data }));
      }
    });
  });
  return new Promise((resolve) => server.listen(OCTOPUS_PORT, '127.0.0.1', () => resolve(server)));
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

const base = `http://127.0.0.1:${HTTP_PORT}`;

async function getJson<T>(url: string): Promise<T> {
  const resp = await fetch(url);
  return (await resp.json()) as T;
}

async function post(url: string, body: unknown): Promise<{ status: number; data: Record<string, unknown> }> {
  const resp = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  return { status: resp.status, data: (await resp.json()) as Record<string, unknown> };
}

async function snapshot(): Promise<Snapshot | null> {
  try {
    const res = await getJson<{ ok: boolean; data: Snapshot | null }>(`${base}/api/snapshot`);
    return res.ok ? res.data : null;
  } catch {
    return null;
  }
}

/** Poll until `check` passes (it returns a reason while waiting). */
async function waitFor(
  what: string,
  check: (s: Snapshot, d: Dispatches) => string | null,
  timeoutMs: number,
): Promise<{ snap: Snapshot; dispatches: Dispatches }> {
  const start = Date.now();
  let last = 'no snapshot yet';
  while (Date.now() - start < timeoutMs) {
    const snap = await snapshot();
    const dispatches = await getJson<Dispatches>(`${base}/api/octopus/dispatches`).catch(() => ({}));
    if (snap) {
      const why = check(snap, dispatches);
      if (why === null) return { snap, dispatches };
      last = why;
    }
    await new Promise((r) => setTimeout(r, 1000));
  }
  throw new Error(`Timed out waiting for ${what}: ${last}`);
}

const slotText = (s: Slot | undefined) =>
  s ? `${s.enabled ? 'on' : 'off'} ${s.start_hour}:${s.start_minute}-${s.end_hour}:${s.end_minute}` : 'none';

test.describe('Battery top-up during a smart charge: ACCoupled', () => {
  test.describe.configure({ mode: 'serial' });
  test.setTimeout(360_000);

  let simulator: ChildProcess | null = null;
  let backend: ChildProcess | null = null;
  let mock: http.Server | null = null;
  let settings: TestSettingsFixture | null = null;

  test.beforeAll(async () => {
    test.setTimeout(120_000);
    for (const port of [MODBUS_PORT, HTTP_PORT, OCTOPUS_PORT]) killPort(port);
    await new Promise((r) => setTimeout(r, 500));
    if (!SIMULATOR_PATH || !fs.existsSync(SIMULATOR_PATH)) {
      throw new Error('Simulator not found; build ~/repos/givenergy-simulator with cargo build --release');
    }
    if (!fs.existsSync(BACKEND_PATH)) throw new Error(`Backend not found at ${BACKEND_PATH}`);
    if (!fs.existsSync(path.join(DIST_DIR, 'index.html'))) throw new Error(`Frontend dist not found at ${DIST_DIR}`);

    mock = await startOctopusMock();
    simulator = spawn(
      SIMULATOR_PATH,
      [
        'simulate',
        '--inverter', 'ACCoupled',
        '--batteries', '1',
        '--battery-size', '9.5',
        '--soc', '40',
        '--solar-peak', '0',
        '--load-profile', 'minimal',
        '--weather', 'clear',
        '--modbus', `127.0.0.1:${MODBUS_PORT}`,
      ],
      { stdio: ['ignore', 'pipe', 'pipe'] },
    );
    attachErrorHandler(simulator, 'simulator');
    pipe(simulator, 'sim');
    await new Promise((r) => setTimeout(r, 2000));

    settings = await writeTestSettings({
      tag: 'dispatch-charge',
      port: MODBUS_PORT,
      httpPort: HTTP_PORT,
      pollInterval: 2,
      writePacingMs: 25,
    });
    // Point the Octopus integration at the mock. The hold and top-up are
    // switched on by the test, through the API the Octopus page uses.
    const saved = JSON.parse(fs.readFileSync(settings.settingsPath, 'utf8')) as Record<string, unknown>;
    Object.assign(saved, {
      octopus_api_key: 'sk_test_mock',
      octopus_account_number: 'A-MOCK0001',
      octopus_api_base_url: `http://127.0.0.1:${OCTOPUS_PORT}`,
      octopus_dispatch_hold: false,
      octopus_dispatch_charge: false,
    });
    fs.writeFileSync(settings.settingsPath, JSON.stringify(saved, null, 2));

    backend = spawn(BACKEND_PATH, ['--headless', '--port', String(HTTP_PORT), '--dist', DIST_DIR], {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...process.env, ...settings.env },
    });
    attachErrorHandler(backend, 'backend');
    pipe(backend, 'backend');
    await waitFor('the first snapshot', (s) => (s.device_type_code ? null : 'not connected'), 60_000);
  });

  test.afterAll(async () => {
    if (backend) await stopChildProcess(backend, 'backend', 10_000);
    killPort(HTTP_PORT);
    if (simulator) await stopChildProcess(simulator, 'simulator', 3000);
    await new Promise<void>((resolve) => (mock ? mock.close(() => resolve()) : resolve()));
    await settings?.cleanup();
  });

  test('charges the battery during the dispatch and puts the settings back after it', async () => {
    // The user's own slot 1, well away from now, with an 80% target: what
    // the restore must put back.
    const later = new Date(Date.now() + 6 * 3600_000);
    const laterEnd = new Date(later.getTime() + 3600_000);
    const own = await post(`${base}/api/control/charge-slot`, {
      slot: 1,
      enabled: true,
      start_hour: later.getHours(),
      start_minute: 0,
      end_hour: laterEnd.getHours(),
      end_minute: 0,
      target_soc: 80,
    });
    expect(own.data.ok, JSON.stringify(own.data)).toBe(true);
    const { snap: before } = await waitFor(
      'the own slot to read back',
      (s) =>
        s.charge_slots[0]?.enabled && s.charge_slots[0].start_hour === later.getHours() && s.target_soc === 80
          ? null
          : `slot 1 ${slotText(s.charge_slots[0])}, target ${s.target_soc}`,
      45_000,
    );

    console.log(`[test] before: SOC ${before.soc}, battery ${before.battery_power} W, enable ${before.enable_charge}, reserve ${before.battery_reserve}`);
    // The top-up is refused until the hold is on.
    const early = await post(`${base}/api/octopus/dispatches/charge`, { enabled: true });
    expect(early.status).toBe(409);

    const dispatchEnd = Date.now() + DISPATCH_LEFT_MS;
    planned = [
      {
        start: new Date(Date.now() - 60_000).toISOString(),
        end: new Date(dispatchEnd).toISOString(),
        type: 'SMART',
        energyAddedKwh: -2,
      },
    ];
    const hold = await post(`${base}/api/octopus/dispatches/hold`, { enabled: true });
    expect(hold.status, JSON.stringify(hold.data)).toBe(200);
    const charge = await post(`${base}/api/octopus/dispatches/charge`, { enabled: true });
    expect(charge.status, JSON.stringify(charge.data)).toBe(200);

    const end = new Date(dispatchEnd);
    const { snap: during, dispatches } = await waitFor(
      'the top-up to charge',
      (s, d) => {
        if (!s.dispatch_charge_active) {
          return `not active at SOC ${s.soc}, battery ${s.battery_power} W (${d.charge_state} ${d.charge_reason ?? ''} ${d.charge_note ?? ''})`;
        }
        if (!s.enable_charge) return 'charge enable not on';
        if (s.target_soc !== 100) return `target ${s.target_soc}`;
        const slot = s.charge_slots[0];
        // Slot 1 over the dispatch, ending at its end on the inverter's
        // clock (a minute either way for the clock and the minute boundary).
        const endMinute = end.getHours() * 60 + end.getMinutes();
        const slotEnd = (slot?.end_hour ?? 0) * 60 + (slot?.end_minute ?? 0);
        const apart = Math.abs(slotEnd - endMinute) % 1440;
        if (!slot?.enabled || Math.min(apart, 1440 - apart) > 1) return `slot 1 ${slotText(slot)}`;
        if (s.battery_power > -100) return `battery ${s.battery_power} W`;
        return null;
      },
      Math.max(DISPATCH_LEFT_MS - 30_000, 60_000),
    );
    expect(dispatches.charge_state).toBe('charging');
    expect(during.battery_power, 'negative = charging').toBeLessThan(-100);

    // After the dispatch: HEM's restore, confirmed by a read.
    planned = [];
    const { snap: after, dispatches: done } = await waitFor(
      'the settings to come back',
      (s, d) => {
        if (Date.now() < dispatchEnd) return 'the dispatch is still running';
        if (s.dispatch_charge_active) return 'still active';
        if (d.charge_state !== 'idle') return `state ${d.charge_state}`;
        if (slotText(s.charge_slots[0]) !== slotText(before.charge_slots[0])) {
          return `slot 1 ${slotText(s.charge_slots[0])}`;
        }
        if (s.target_soc !== before.target_soc) return `target ${s.target_soc}`;
        if (s.enable_charge !== before.enable_charge) return `enable ${s.enable_charge}`;
        return null;
      },
      DISPATCH_LEFT_MS + 120_000,
    );
    expect(done.charge_enabled).toBe(true);
    expect(after.battery_reserve, 'the hold put the reserve back').toBe(before.battery_reserve);
    const { snap: settled } = await waitFor(
      'the battery to stop charging',
      (s) => (s.battery_power < -100 ? `battery ${s.battery_power} W` : null),
      30_000,
    );
    expect(settled.battery_power).toBeGreaterThanOrEqual(-100);
  });
});
