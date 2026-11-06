/**
 * Issue #432: the AIO Hybrid (0x82xx) and the HV Gen3 hybrid (0x81xx) are
 * physically single-phase and use the single-phase schedule registers, as
 * GivTCP does. A charge slot saved from the Control page must actually charge
 * the battery.
 *
 * Before the fix the save wrote HR 1113/1114 (the three-phase slot map) and
 * never touched `enable_charge` (HR 96), so it read back fine and the inverter
 * carried on in Eco for the whole slot. This runs each model through the real
 * simulator and asserts the saved slot shows up in the single-phase registers
 * and the battery charges.
 *
 * Prerequisites: npm run build; (cd src-tauri && cargo build --release);
 * the simulator built at ~/repos/givenergy-simulator/target/release/sim-api
 * (or GIVENERGY_SIMULATOR_BIN pointing at a build from the same branch).
 */

import { test, expect } from './local-fixture.js';
import { spawn, type ChildProcess } from 'child_process';
import * as path from 'path';
import * as fs from 'fs';
import { fileURLToPath } from 'url';
import { writeTestSettings } from './test-settings.js';
import { simulatorBinaryPath } from './binary-path.js';
import { attachErrorHandler } from './process-errors.js';
import { stopChildProcess } from './process-lifecycle.js';
import { killPort } from './port-cleanup.mjs';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const SIMULATOR_PATH = simulatorBinaryPath();
const BACKEND_PATH = path.resolve(__dirname, '..', 'src-tauri', 'target', 'release', 'givenergy-local');
const DIST_DIR = path.resolve(__dirname, '..', 'dist');
const BASE_PORT = 19800;

interface Snapshot {
  timestamp: number;
  device_type: string;
  device_type_code: string;
  enable_charge: boolean;
  charge_slots: Array<{ enabled: boolean; start_hour: number; end_hour: number }>;
  battery_power: number;
}

async function getSnapshot(base: string): Promise<Snapshot | null> {
  try {
    const resp = await fetch(`${base}/api/snapshot`);
    const body = (await resp.json()) as { ok: boolean; data: Snapshot | null };
    return body.ok ? body.data : null;
  } catch {
    return null;
  }
}

function pipe(child: ChildProcess, label: string) {
  for (const stream of [child.stdout, child.stderr]) {
    stream?.on('data', (d: Buffer) => {
      for (const line of d.toString().trim().split('\n')) {
        if (/ERROR|panicked/.test(line)) console.log(`[${label}] ${line}`);
      }
    });
  }
}

const MODELS = ['AIOHybrid6kW', 'Gen3HvHybrid6kW'];

test.describe('single-phase HV models charge in a saved slot', () => {
  test.describe.configure({ mode: 'serial' });

  MODELS.forEach((sim, index) => {
    test(`${sim}: a saved charge slot charges the battery`, async () => {
      test.setTimeout(180_000);
      if (!SIMULATOR_PATH || !fs.existsSync(SIMULATOR_PATH)) {
        throw new Error('Simulator not found; build ~/repos/givenergy-simulator with cargo build --release');
      }
      if (!fs.existsSync(BACKEND_PATH)) throw new Error(`Backend not found at ${BACKEND_PATH}`);
      const modbusPort = BASE_PORT + index * 2;
      const httpPort = modbusPort + 1;
      killPort(modbusPort);
      killPort(httpPort);

      const simulator = spawn(
        SIMULATOR_PATH,
        ['simulate', '--inverter', sim, '--soc', '40', '--load-profile', 'minimal', '--modbus', `127.0.0.1:${modbusPort}`],
        { stdio: ['ignore', 'pipe', 'pipe'] },
      );
      attachErrorHandler(simulator, `simulator:${sim}`);
      pipe(simulator, `sim:${sim}`);
      const settings = await writeTestSettings({ tag: `hv-slot-${sim}`, port: modbusPort, httpPort, pollInterval: 2 });
      let backend: ChildProcess | null = null;
      try {
        await new Promise((r) => setTimeout(r, 1500));
        backend = spawn(BACKEND_PATH, ['--headless', '--port', String(httpPort), '--dist', DIST_DIR], {
          stdio: ['ignore', 'pipe', 'pipe'],
          env: { ...process.env, ...settings.env },
        });
        attachErrorHandler(backend, `backend:${sim}`);
        pipe(backend, `backend:${sim}`);
        const base = `http://127.0.0.1:${httpPort}`;

        // Wait for the first full snapshot.
        let connected = false;
        const connectDeadline = Date.now() + 60_000;
        while (Date.now() < connectDeadline && !connected) {
          const snap = await getSnapshot(base);
          connected = !!snap?.device_type_code;
          if (!connected) await new Promise((r) => setTimeout(r, 500));
        }
        expect(connected, `${sim} never connected`).toBe(true);

        // A window around the inverter's clock, so the slot is active now.
        const now = new Date();
        const start = new Date(now.getTime() - 10 * 60_000);
        const end = new Date(now.getTime() + 60 * 60_000);
        const save = await fetch(`${base}/api/control/charge-slot`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            slot: 1,
            enabled: true,
            start_hour: start.getHours(),
            start_minute: start.getMinutes(),
            end_hour: end.getHours(),
            end_minute: end.getMinutes(),
            target_soc: 80,
          }),
        });
        const saved = (await save.json()) as { ok: boolean; error?: string };
        expect(saved.ok, `charge slot save failed: ${saved.error ?? ''}`).toBe(true);

        // The slot must land in the single-phase map: the decoder reads it back
        // from HR 94/95 with enable_charge (HR 96) on, and the inverter charges.
        let snap: Snapshot | null = null;
        const deadline = Date.now() + 90_000;
        while (Date.now() < deadline) {
          snap = await getSnapshot(base);
          if (snap?.enable_charge && snap.charge_slots[0]?.enabled && snap.battery_power < -100) break;
          await new Promise((r) => setTimeout(r, 1000));
        }
        expect(snap?.device_type, `${sim} detected model`).toBe(
          sim === 'AIOHybrid6kW' ? 'AllInOneHybrid' : 'HybridHvGen3',
        );
        expect(snap?.enable_charge, `${sim}: enable_charge (HR 96) must be on`).toBe(true);
        expect(snap?.charge_slots[0]?.enabled, `${sim}: slot 1 must read back enabled`).toBe(true);
        expect(
          snap?.battery_power ?? 0,
          `${sim}: the battery must be charging (negative = charging)`,
        ).toBeLessThan(-100);
      } finally {
        if (backend) await stopChildProcess(backend, `backend:${sim}`, 10_000);
        await stopChildProcess(simulator, `simulator:${sim}`, 3000);
        await settings.cleanup();
        killPort(modbusPort);
        killPort(httpPort);
      }
    });
  });
});
