/**
 * The GivTCP model contract over the wire: every simulator model in
 * tests/fixtures/givtcp-model-contract.json, through a real headless backend.
 *
 * The unit tests check HEM's and the simulator's rules against the contract
 * one side at a time; this checks the two together, so a model the simulator
 * presents differently from what HEM expects (another device code, firmware
 * that detects as another generation, a pause block HEM can't read) fails
 * here instead of on someone's inverter.
 *
 * Per model it checks HEM detects the contract's model, offers the charge cap
 * exactly where the contract says, and reads the pause registers wherever it
 * uses them.
 *
 * The simulator runs each model on its own firmware, so each is checked
 * against the contract row for that firmware: the highest row at or below
 * it, or the model's only row when the contract has just one.
 *
 * Prerequisites: npm run build; (cd src-tauri && cargo build --release);
 * the simulator built at ~/repos/givenergy-simulator/target/release/sim-api.
 */

import { test, expect } from './local-fixture.js';
import { spawn, type ChildProcess } from 'child_process';
import * as path from 'path';
import * as fs from 'fs';
import { fileURLToPath } from 'url';
import contract from '../tests/fixtures/givtcp-model-contract.json' with { type: 'json' };
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
const BASE_PORT = 19700;

interface Deviation {
  value: string | boolean;
  issue: number;
  why: string;
}

interface Row {
  code: string;
  arm_fw: number;
  hem: string;
  sim: string | null;
  givtcp: { pause: string; three_phase: boolean };
  hem_deviations?: Record<string, Deviation>;
}

interface Snapshot {
  device_type: string;
  device_type_code: string;
  firmware_version: string;
  battery_pause_mode_raw: number | null;
}

const rows = (contract as { rows: Row[] }).rows;

/** What HEM does for a field: GivTCP's value unless the contract records a deviation. */
function hem(row: Row, field: 'pause' | 'three_phase' | 'charge_cap'): string | boolean {
  const deviation = row.hem_deviations?.[field];
  if (deviation) return deviation.value;
  if (field === 'charge_cap') return row.givtcp.pause !== 'none';
  return row.givtcp[field];
}

/** The contract row for this simulator model running this firmware. */
function rowFor(sim: string, armFw: number): Row {
  const candidates = rows.filter((r) => r.sim === sim).sort((a, b) => a.arm_fw - b.arm_fw);
  if (candidates.length === 1) return candidates[0];
  const atOrBelow = candidates.filter((r) => r.arm_fw <= armFw);
  if (atOrBelow.length === 0) throw new Error(`no contract row for ${sim} on ARM ${armFw}`);
  return atOrBelow[atOrBelow.length - 1];
}

const models = [...new Set(rows.map((r) => r.sim).filter((s): s is string => s !== null))];

function pipe(child: ChildProcess, label: string) {
  for (const stream of [child.stdout, child.stderr]) {
    stream?.on('data', (d: Buffer) => {
      for (const line of d.toString().trim().split('\n')) {
        if (/ERROR|panicked/.test(line)) console.log(`[${label}] ${line}`);
      }
    });
  }
}

async function getJson<T>(url: string): Promise<T> {
  const resp = await fetch(url);
  return (await resp.json()) as T;
}

test.describe('GivTCP model contract against the simulator', () => {
  test.describe.configure({ mode: 'serial' });

  models.forEach((sim, index) => {
    test(`${sim}: HEM detects it and offers what the contract says`, async () => {
      test.setTimeout(90_000);
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
        ['simulate', '--inverter', sim, '--soc', '60', '--load-profile', 'minimal', '--modbus', `127.0.0.1:${modbusPort}`],
        { stdio: ['ignore', 'pipe', 'pipe'] },
      );
      attachErrorHandler(simulator, `simulator:${sim}`);
      pipe(simulator, `sim:${sim}`);
      const settings = await writeTestSettings({ tag: `contract-${sim}`, port: modbusPort, httpPort, pollInterval: 2 });
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

        // Two polls after connecting, so the optional blocks (the pause
        // registers among them) have had a read.
        let snapshot: Snapshot | null = null;
        let polls = 0;
        let lastTimestamp: unknown = null;
        const deadline = Date.now() + 60_000;
        while (Date.now() < deadline && polls < 2) {
          try {
            const res = await getJson<{ ok: boolean; data: (Snapshot & { timestamp: unknown }) | null }>(
              `${base}/api/snapshot`,
            );
            if (res.ok && res.data && res.data.device_type_code && res.data.timestamp !== lastTimestamp) {
              lastTimestamp = res.data.timestamp;
              snapshot = res.data;
              polls += 1;
            }
          } catch {
            // not up yet
          }
          await new Promise((r) => setTimeout(r, 500));
        }
        expect(snapshot, `${sim} never connected`).not.toBeNull();
        const armFw = parseInt(snapshot!.firmware_version, 10);
        const row = rowFor(sim, armFw);
        const label = `${sim} (0x${snapshot!.device_type_code}, ARM ${armFw}) against row 0x${row.code} ARM ${row.arm_fw}`;

        expect(snapshot!.device_type, `${label}: detected model`).toBe(row.hem);
        expect(snapshot!.device_type_code.slice(0, 2), `${label}: device code family`).toBe(row.code.slice(0, 2));

        const cap = await getJson<{ supported: boolean }>(`${base}/api/charge-cap`);
        expect(cap.supported, `${label}: charge cap offered`).toBe(hem(row, 'charge_cap'));

        if (hem(row, 'pause') !== 'none') {
          expect(snapshot!.battery_pause_mode_raw, `${label}: HEM reads the pause mode it uses`).not.toBeNull();
        }
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
