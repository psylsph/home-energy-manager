/**
 * Spread Charge write-and-restore (issue #508) against the real GivEnergy
 * Simulator and the real backend.
 *
 * With the setting on, a plan writes its whole cheap window at the full rate
 * and arms a lower charge limit; the poll loop lowers the limit once charge
 * slot 1 is running and puts it back at the window's end. This suite proves,
 * in the simulator's physics, the things unit tests can only assert about
 * HEM's own idea of the register:
 *   - nothing is lowered before the slot runs;
 *   - once it runs the battery really charges at the lowered power, the
 *     baseline is on disk, and the Control page says when the limit returns;
 *   - at the window's end the limit is back and the saved run is gone;
 *   - the user's own change to the limit wins;
 *   - Force Charge puts the limit back at once, and its own restore leaves
 *     the user's value rather than the lowered one;
 *   - a restart mid-window carries the run on and still restores.
 *
 * The Planner's own advice (forecast, tariff) needs data the simulator
 * doesn't have, so the window is armed through the same endpoint the
 * Planner's Apply button posts to once its slot writes succeed.
 *
 * Prerequisites: npm run build; (cd src-tauri && cargo build --release);
 * the simulator built at ~/repos/givenergy-simulator/target/release/sim-api.
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

/** What the plan lowers the limit to, as a share of the inverter's maximum. */
const LOWERED_PERCENT = 30;
/** Measured charge power may differ from the target by about one register
 *  step plus the simulator's own rounding. */
const TOLERANCE_W = 250;

interface Slot {
  enabled: boolean;
  start_hour: number;
  start_minute: number;
  end_hour: number;
  end_minute: number;
}

interface Snap {
  charge_rate: number;
  battery_power: number;
  max_battery_power_w: number;
  soc: number;
  charge_slots: Slot[];
  plan_charge_limit_back_at?: number | null;
}

interface SavedRun {
  phase: string;
  baseline_raw: number;
  lowered_raw: number;
}

interface Suite {
  label: string;
  inverter: string;
  modbusPort: number;
  httpPort: number;
  /** The write that puts the limit somewhere the user chose, in raw units. */
  userLimitRaw: number;
  /** Only the first suite runs the whole set; the second proves the other
   *  register bank lowers and restores. */
  full: boolean;
  /** Run the simulator and backend in a time zone where it is about 23:56
   *  now, so a window can be followed across midnight in a few minutes. */
  midnight?: boolean;
}

const SUITES: Suite[] = [
  {
    label: 'hybrid (HR111, percent of capacity)',
    inverter: 'Gen1Hybrid',
    modbusPort: 19941,
    httpPort: 18391,
    userLimitRaw: 40,
    full: true,
  },
  {
    label: 'AC-coupled (HR313, percent of maximum)',
    inverter: 'ACCoupled',
    modbusPort: 19942,
    httpPort: 18392,
    userLimitRaw: 80,
    full: false,
  },
  {
    label: 'hybrid, across midnight (simulated clock)',
    inverter: 'Gen1Hybrid',
    modbusPort: 19943,
    httpPort: 18393,
    userLimitRaw: 40,
    full: false,
    midnight: true,
  },
];

/**
 * A POSIX time zone `HEM` that is `offsetMinutes` east of UTC, which the
 * simulator (`chrono::Local`) and the backend both honour from `TZ`.
 */
function posixZone(offsetMinutes: number): string {
  const magnitude = Math.abs(offsetMinutes);
  const hours = Math.floor(magnitude / 60);
  const minutes = String(magnitude % 60).padStart(2, '0');
  // POSIX writes the offset west-positive, so east of UTC is negative.
  return `HEM${offsetMinutes > 0 ? '-' : '+'}${hours}:${minutes}`;
}

/** The offset (minutes east of UTC, within +-12h) that makes it `hhmm` now. */
function offsetForLocalTime(hour: number, minute: number): number {
  const now = new Date();
  const utcMinutes = now.getUTCHours() * 60 + now.getUTCMinutes();
  const offset = (((hour * 60 + minute - utcMinutes) % 1440) + 1440) % 1440;
  return offset > 720 ? offset - 1440 : offset;
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

function attachLogs(label: string, proc: ChildProcess) {
  for (const stream of [proc.stdout, proc.stderr]) {
    stream?.on('data', (data: Buffer) => {
      for (const line of data.toString().trim().split('\n')) {
        if (line.trim()) console.log(`[${label}] ${line}`);
      }
    });
  }
}

/** Local time of day for a Date, as the slot endpoints take it. */
const clock = (date: Date) => ({ hour: date.getHours(), minute: date.getMinutes() });

/** The next whole minute at least `seconds` from now. */
function minuteBoundary(seconds: number): Date {
  const at = new Date(Date.now() + seconds * 1000);
  at.setSeconds(0, 0);
  if (at.getTime() < Date.now() + seconds * 1000) at.setMinutes(at.getMinutes() + 1);
  return at;
}

for (const suite of SUITES) {
  test.describe.serial(`Spread Charge on a simulated ${suite.label}`, () => {
    const base = `http://127.0.0.1:${suite.httpPort}`;
    let simulator: ChildProcess | null = null;
    let backend: ChildProcess | null = null;
    let fixture: TestSettingsFixture | null = null;
    let baselineRaw = 0;
    let maxW = 0;
    // Set for the midnight suite: the simulated local clock is host UTC plus this.
    let zoneOffsetMs = 0;
    let zone: string | null = null;

    async function api(method: 'GET' | 'POST', pathname: string, body?: unknown) {
      const response = await fetch(`${base}${pathname}`, {
        method,
        headers: { 'Content-Type': 'application/json' },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      return { status: response.status, json: (await response.json()) as Record<string, unknown> };
    }

    async function snap(): Promise<Snap | null> {
      try {
        const res = await api('GET', '/api/snapshot');
        return (res.json.data as Snap | null) ?? null;
      } catch {
        return null;
      }
    }

    async function waitFor(what: string, check: (s: Snap) => string | null, timeoutMs: number) {
      const started = Date.now();
      let last = 'no snapshot yet';
      while (Date.now() - started < timeoutMs) {
        const current = await snap();
        if (current) {
          const why = check(current);
          if (why === null) return current;
          last = why;
        }
        await sleep(1000);
      }
      throw new Error(`Timed out waiting for ${what}: ${last}`);
    }

    /** The run HEM saved to settings.json, if any. */
    function savedRun(): SavedRun | null {
      const saved = JSON.parse(fs.readFileSync(fixture!.settingsPath, 'utf8')) as {
        plan_charge_rate_run?: SavedRun | null;
      };
      return saved.plan_charge_rate_run ?? null;
    }

    function startBackend() {
      backend = spawn(BACKEND_PATH, ['--headless', '--port', String(suite.httpPort), '--dist', DIST_DIR], {
        stdio: ['ignore', 'pipe', 'pipe'],
        env: { ...process.env, ...fixture!.env, ...(zone ? { TZ: zone } : {}) },
      });
      attachErrorHandler(backend, `${suite.inverter} backend`);
      attachLogs(`spread-${suite.inverter}-backend`, backend);
    }

    /**
     * What the Planner's Apply does for a spread plan: the slot over the
     * window at the full rate, the master charge switch, then the window
     * armed. Returns the window. With `startsInSeconds` the slot starts at the
     * next whole minute after that and lasts `lastsSeconds`; without it the
     * slot is already running and ends at the first whole minute at least
     * `lastsSeconds` away.
     */
    async function applySpreadWindow(lastsSeconds: number, startsInSeconds?: number) {
      const start = startsInSeconds === undefined
        ? new Date(minuteBoundary(0).getTime() - 120_000)
        : minuteBoundary(startsInSeconds);
      const end = startsInSeconds === undefined
        ? minuteBoundary(lastsSeconds)
        : new Date(start.getTime() + lastsSeconds * 1000);
      const slot = await api('POST', '/api/control/charge-slot', {
        slot: 1,
        enabled: true,
        start_hour: clock(start).hour,
        start_minute: clock(start).minute,
        end_hour: clock(end).hour,
        end_minute: clock(end).minute,
        target_soc: 100,
      });
      expect(slot.json.ok, JSON.stringify(slot.json)).toBe(true);
      const timed = await api('POST', '/api/control/timed-charge', { enabled: true });
      expect(timed.json.ok, JSON.stringify(timed.json)).toBe(true);
      const armed = await api('POST', '/api/forecast/plan/arm-spread', {
        window_start: start.toISOString(),
        window_end: end.toISOString(),
        lowered_percent: LOWERED_PERCENT,
      });
      expect(armed.status, JSON.stringify(armed.json)).toBe(200);
      return { start, end };
    }

    const lowered = (s: Snap) => (s.charge_rate < baselineRaw ? null : `limit ${s.charge_rate}, baseline ${baselineRaw}`);
    const restored = (s: Snap) =>
      s.charge_rate !== baselineRaw
        ? `limit ${s.charge_rate}, baseline ${baselineRaw}`
        : s.plan_charge_limit_back_at
          ? 'still reporting a lowered limit'
          : null;

    test.beforeAll(async () => {
      test.setTimeout(120_000);
      for (const binary of [SIMULATOR_PATH, BACKEND_PATH]) {
        if (!binary || !fs.existsSync(binary)) throw new Error(`Required release binary not found: ${binary}`);
      }
      if (!fs.existsSync(path.join(DIST_DIR, 'index.html'))) {
        throw new Error(`Frontend build not found in ${DIST_DIR}; run npm run build first`);
      }
      for (const port of [suite.modbusPort, suite.httpPort]) killPort(port);
      if (suite.midnight) {
        // About 23:55:30 where the simulator and backend run, so the window
        // below crosses midnight a few minutes into the test.
        const offset = offsetForLocalTime(23, 55);
        zone = posixZone(offset);
        zoneOffsetMs = offset * 60_000;
        console.log(`[test] simulated clock: TZ=${zone}, now ${new Date(Date.now() + zoneOffsetMs).toISOString().slice(11, 19)}`);
      }

      simulator = spawn(SIMULATOR_PATH, [
        'simulate',
        '--inverter', suite.inverter,
        '--batteries', '1',
        '--battery-size', '9.5',
        '--soc', '30',
        '--solar-peak', '0',
        '--load-level', '300',
        '--modbus', `127.0.0.1:${suite.modbusPort}`,
      ], { stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, ...(zone ? { TZ: zone } : {}) } });
      attachErrorHandler(simulator, `${suite.inverter} simulator`);
      attachLogs(`spread-${suite.inverter}-sim`, simulator);
      await sleep(2000);

      fixture = await writeTestSettings({
        tag: `spread-charge-${suite.inverter}`,
        port: suite.modbusPort,
        httpPort: suite.httpPort,
        pollInterval: 2,
        writePacingMs: 25,
      });
      const saved = JSON.parse(fs.readFileSync(fixture.settingsPath, 'utf8')) as Record<string, unknown>;
      saved.forecast_plan_spread_charge = true;
      fs.writeFileSync(fixture.settingsPath, JSON.stringify(saved, null, 2));

      startBackend();
      const first = await waitFor(
        'the first snapshot',
        (s) => (s.max_battery_power_w > 0 && s.soc > 0 ? null : 'not connected'),
        60_000,
      );
      baselineRaw = first.charge_rate;
      maxW = first.max_battery_power_w;
      console.log(`[test] ${suite.inverter}: baseline limit ${baselineRaw}, maximum ${maxW} W`);
    });

    test.afterAll(async () => {
      try { await api('POST', '/api/control/force-charge/stop'); } catch { /* not running */ }
      await stopChildProcess(backend, `${suite.inverter} backend`, 8_000);
      await stopChildProcess(simulator, `${suite.inverter} simulator`, 5_000);
      backend = null;
      simulator = null;
      if (fixture) await fixture.cleanup();
      fixture = null;
    });

    /** Put the inverter back to a known state between tests. */
    test.afterEach(async () => {
      await api('POST', '/api/control/force-charge/stop').catch(() => undefined);
      await api('POST', '/api/control/charge-slot', {
        slot: 1, enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100,
      }).catch(() => undefined);
      await waitFor('no run left', () => (savedRun() ? 'a run is still saved' : null), 60_000);
      if ((await snap())?.charge_rate !== baselineRaw) {
        await api('POST', '/api/control/charge-rate', { limit: baselineRaw });
        await waitFor('the baseline back', (s) => (s.charge_rate === baselineRaw ? null : `limit ${s.charge_rate}`), 40_000);
      }
    });

    test('lowers the limit only once the slot runs, charges at that power and puts it back at the end', async ({ page }) => {
      test.skip(!!suite.midnight, 'this one follows the host clock; the midnight suite has its own');
      test.setTimeout(420_000);
      const { start, end } = await applySpreadWindow(120, 50);

      // The slot hasn't started: nothing is lowered, and nothing is on disk.
      await sleep(8_000);
      expect(Date.now()).toBeLessThan(start.getTime());
      expect((await snap())?.charge_rate, 'the limit stays at full power before the window').toBe(baselineRaw);
      expect(savedRun()).toBeNull();

      // The slot runs: the limit is lowered, the baseline is on disk first.
      const during = await waitFor('the limit to be lowered', lowered, start.getTime() - Date.now() + 60_000);
      expect(during.plan_charge_limit_back_at).toBe(Math.floor(end.getTime() / 1000));
      const run = savedRun();
      expect(run, 'the baseline is saved').not.toBeNull();
      expect(run!.baseline_raw).toBe(baselineRaw);
      expect(run!.lowered_raw).toBe(during.charge_rate);

      // The simulator's physics: the battery really charges at the lowered power.
      await waitFor(
        'the charge power to follow the limit',
        (s) => {
          const watts = -s.battery_power;
          const target = (LOWERED_PERCENT / 100) * maxW;
          return Math.abs(watts - target) <= TOLERANCE_W ? null : `charging at ${Math.round(watts)} W, want ${Math.round(target)} W`;
        },
        60_000,
      );

      // The Control page says when it returns.
      await page.goto(`${base}/#/control`);
      const back = `${String(end.getHours()).padStart(2, '0')}:${String(end.getMinutes()).padStart(2, '0')}`;
      await expect(page.getByText(`Spread Charge holds it there until ${back}.`)).toBeVisible({ timeout: 20_000 });
      await expect(page.getByTestId('charge-slot-limit-shortfall')).toHaveCount(0);

      // The window ends: the limit is back, confirmed by a read, and the run is gone.
      await waitFor(
        'the limit to come back at the window end',
        (s) => (Date.now() < end.getTime() ? 'the window is still open' : restored(s)),
        180_000,
      );
      await waitFor('the saved run to clear', () => (savedRun() ? 'still saved' : null), 30_000);
    });

    if (suite.midnight) {
      test('a window that crosses midnight stays lowered through the date change and is restored at its end', async () => {
        test.setTimeout(600_000);
        // The simulated local clock: host UTC shifted by the zone's offset.
        const simClock = (date: Date) => new Date(date.getTime() + zoneOffsetMs);
        const now = new Date();
        const simNow = simClock(now);
        expect(simNow.getUTCHours(), 'the test starts shortly before midnight').toBe(23);
        // The slot began two minutes ago and ends two minutes after midnight.
        const start = new Date(Math.floor(now.getTime() / 60_000) * 60_000 - 120_000);
        const nextMidnightSim = Math.floor(simNow.getTime() / 86_400_000 + 1) * 86_400_000;
        const end = new Date(nextMidnightSim - zoneOffsetMs + 120_000);
        const hm = (date: Date) => ({ hour: simClock(date).getUTCHours(), minute: simClock(date).getUTCMinutes() });
        expect(hm(start).hour, 'the slot starts before midnight').toBe(23);
        expect(hm(end), 'and ends after it').toEqual({ hour: 0, minute: 2 });

        const slot = await api('POST', '/api/control/charge-slot', {
          slot: 1,
          enabled: true,
          start_hour: hm(start).hour,
          start_minute: hm(start).minute,
          end_hour: hm(end).hour,
          end_minute: hm(end).minute,
          target_soc: 100,
        });
        expect(slot.json.ok, JSON.stringify(slot.json)).toBe(true);
        expect((await api('POST', '/api/control/timed-charge', { enabled: true })).json.ok).toBe(true);
        const armed = await api('POST', '/api/forecast/plan/arm-spread', {
          window_start: start.toISOString(),
          window_end: end.toISOString(),
          lowered_percent: LOWERED_PERCENT,
        });
        expect(armed.status, JSON.stringify(armed.json)).toBe(200);

        // Before midnight: lowered, and the battery charges at the lowered power.
        const during = await waitFor('the limit to be lowered', lowered, 60_000);
        expect(during.plan_charge_limit_back_at).toBe(Math.floor(end.getTime() / 1000));
        expect(simClock(new Date()).getUTCHours(), 'still before midnight').toBe(23);

        // Through midnight: still lowered, still charging at that power, and
        // the date change does not read as the window ending.
        await waitFor(
          'the simulated clock to pass midnight',
          () => (simClock(new Date()).getUTCHours() === 0 ? null : 'not yet midnight'),
          300_000,
        );
        await sleep(6_000);
        const past = (await snap())!;
        expect(past.charge_rate, 'lowered after midnight').toBeLessThan(baselineRaw);
        expect(past.plan_charge_limit_back_at).toBe(Math.floor(end.getTime() / 1000));
        const target = (LOWERED_PERCENT / 100) * maxW;
        expect(Math.abs(-past.battery_power - target), 'charging at the lowered power').toBeLessThanOrEqual(TOLERANCE_W);

        // The window's end, after midnight: the limit is back.
        await waitFor(
          'the limit to come back at the window end',
          (s) => (Date.now() < end.getTime() ? 'the window is still open' : restored(s)),
          240_000,
        );
        await waitFor('the saved run to clear', () => (savedRun() ? 'still saved' : null), 30_000);
      });
      return;
    }

    if (!suite.full) return;

    test('a change to the limit made during the window is the user\'s, and stays', async () => {
      test.setTimeout(300_000);
      const { end } = await applySpreadWindow(150);
      await waitFor('the limit to be lowered', lowered, 90_000);

      const write = await api('POST', '/api/control/charge-rate', { limit: suite.userLimitRaw });
      expect(write.json.ok, JSON.stringify(write.json)).toBe(true);
      await waitFor('the run to end', (s) => (savedRun() || s.plan_charge_limit_back_at ? 'run still held' : null), 60_000);

      // Past the window end HEM must not write the baseline over their choice.
      await sleep(Math.max(end.getTime() - Date.now(), 0) + 15_000);
      expect((await snap())?.charge_rate, 'their value stands').toBe(suite.userLimitRaw);
    });

    test('Force Charge gets full power at once, and afterwards the limit is the user\'s, not the lowered one', async () => {
      test.setTimeout(300_000);
      await applySpreadWindow(180);
      await waitFor('the limit to be lowered', lowered, 90_000);

      const force = await api('POST', '/api/control/force-charge', { minutes: 30 });
      expect(force.json.ok, JSON.stringify(force.json)).toBe(true);
      await waitFor('the limit to go back for Force Charge', restored, 60_000);
      await waitFor('the saved run to clear', () => (savedRun() ? 'still saved' : null), 30_000);

      // Stopping Force Charge must leave the user's limit in place, not the
      // lowered one the plan had set.
      const stop = await api('POST', '/api/control/force-charge/stop');
      expect(stop.json.ok, JSON.stringify(stop.json)).toBe(true);
      await sleep(10_000);
      await waitFor('the limit to stay at the baseline', restored, 40_000);
    });

    test('a graceful stop puts the limit back, and a restart finishes nothing it should not', async () => {
      test.setTimeout(300_000);
      await applySpreadWindow(240);
      await waitFor('the limit to be lowered', lowered, 90_000);
      expect(savedRun()).not.toBeNull();

      await stopChildProcess(backend, `${suite.inverter} backend`, 15_000);
      backend = null;
      expect(savedRun(), 'the shutdown restore cleared the saved run').toBeNull();

      startBackend();
      await waitFor('HEM to reconnect', (s) => (s.max_battery_power_w > 0 ? null : 'not connected'), 60_000);
      await waitFor('the baseline on the inverter', restored, 40_000);
    });

    test('a crash mid-window leaves the run on disk, and the restarted backend restores it at the end', async () => {
      test.setTimeout(420_000);
      const { end } = await applySpreadWindow(150);
      await waitFor('the limit to be lowered', lowered, 90_000);

      // No shutdown handler runs.
      backend!.kill('SIGKILL');
      await new Promise((resolve) => backend!.once('exit', resolve));
      backend = null;
      expect(savedRun(), 'the baseline survives the crash').not.toBeNull();

      startBackend();
      await waitFor('HEM to reconnect', (s) => (s.max_battery_power_w > 0 ? null : 'not connected'), 60_000);
      // Still the window: the run is carried on, the limit stays lowered.
      if (Date.now() < end.getTime() - 20_000) {
        const midway = await snap();
        expect(midway?.charge_rate).toBeLessThan(baselineRaw);
      }
      await waitFor(
        'the restore at the window end',
        (s) => (Date.now() < end.getTime() ? 'the window is still open' : restored(s)),
        240_000,
      );
      await waitFor('the saved run to clear', () => (savedRun() ? 'still saved' : null), 30_000);
    });
  });
}
