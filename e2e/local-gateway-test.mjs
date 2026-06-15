/**
 * Local-only Gateway integration test.
 *
 * Starts the GivEnergy Simulator with Gateway12kW profile and the headless
 * backend, then verifies Gateway-specific data via the REST API.
 *
 * MUST NOT BE RUN ON GITHUB PIPELINE (per AGENTS.md).
 *
 * Usage: node e2e/local-gateway-test.mjs
 */

import { spawn, execSync } from 'child_process';
import * as path from 'path';
import * as fs from 'fs';
import * as os from 'os';
import { fileURLToPath } from 'url';
import { setTimeout as sleep } from 'timers/promises';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const MODBUS_PORT = 18999;
const HTTP_PORT = 17347;
const HOME_DIR = os.homedir();

const SIMULATOR_PATH = path.resolve(
  HOME_DIR, 'repos', 'givenergy-simulator', 'target', 'release', 'sim-api',
);
const BACKEND_PATH = path.resolve(
  __dirname, '..', 'src-tauri', 'target', 'release', 'givenergy-local',
);
const DIST_DIR = path.resolve(__dirname, '..', 'dist');

function killPort(port) {
  try {
    execSync(`fuser -k ${port}/tcp 2>/dev/null || true`, { stdio: 'ignore' });
  } catch { /* ignore */ }
}

async function startSimulator() {
  console.log(`[gateway-test] Starting simulator on port ${MODBUS_PORT}...`);
  const proc = spawn(SIMULATOR_PATH, [
    'simulate',
    '--inverter', 'Gateway12kW',
    '--batteries', '1',
    '--battery-size', '13.5',
    '--soc', '65',
    '--solar-peak', '4000',
    '--load-profile', 'family',
    '--weather', 'clear',
    '--tick-interval', '5',
    '--modbus', `127.0.0.1:${MODBUS_PORT}`,
  ], { stdio: ['ignore', 'pipe', 'pipe'] });
  proc.stdout.on('data', d => process.stdout.write(`[sim] ${d}`));
  proc.stderr.on('data', d => process.stderr.write(`[sim:err] ${d}`));
  proc.on('exit', code => console.log(`[gateway-test] Simulator exited (${code})`));
  await sleep(2000);
  return proc;
}

async function startBackend() {
  console.log(`[gateway-test] Starting backend on port ${HTTP_PORT}...`);
  const configDir = fs.mkdtempSync(path.join(os.tmpdir(), 'givenergy-gateway-test-'));
  const dataDir = path.join(configDir, '.givenergy-local');
  fs.mkdirSync(dataDir, { recursive: true });

  const settings = {
    host: '127.0.0.1', port: MODBUS_PORT, serial: '', poll_interval: 5,
    http_port: HTTP_PORT, auto_connect: true,
    import_tariff: 0.285, export_tariff: 0.15,
    auto_winter_enabled: false, auto_winter_cold_threshold: 8.0,
    auto_winter_recovery_threshold: 12.0, auto_winter_target_soc: 80,
    auto_winter_debounce_readings: 2,
    cosy_enabled: false, developer_mode: true,
    cosy_slots: Array.from({ length: 3 }, () => ({
      enabled: false, start_hour: 0, start_minute: 0,
      end_hour: 0, end_minute: 0, target_soc: 100,
    })),
  };
  fs.writeFileSync(path.join(dataDir, 'settings.json'), JSON.stringify(settings, null, 2));

  const proc = spawn(BACKEND_PATH, ['--headless', '--port', String(HTTP_PORT), '--dist', DIST_DIR], {
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, HOME: configDir, RUST_LOG: 'warn' },
  });
  proc.stdout.on('data', d => process.stdout.write(`[backend] ${d}`));
  proc.stderr.on('data', d => process.stderr.write(`[backend:err] ${d}`));
  proc.on('exit', code => console.log(`[gateway-test] Backend exited (${code})`));
  await sleep(3000);
  return { proc, configDir };
}

async function fetchJson(url) {
  const resp = await fetch(url);
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
  return resp.json();
}

async function waitForConnection(baseUrl, maxRetries = 20) {
  for (let i = 0; i < maxRetries; i++) {
    try {
      const status = await fetchJson(`${baseUrl}/api/status`);
      if (status.connected) return status;
    } catch { /* retry */ }
    await sleep(1000);
  }
  throw new Error('Timed out waiting for connection');
}

async function waitForSnapshot(baseUrl, maxRetries = 15) {
  for (let i = 0; i < maxRetries; i++) {
    try {
      const snap = await fetchJson(`${baseUrl}/api/snapshot`);
      if (snap && snap.device_type_code) return snap;
    } catch { /* retry */ }
    await sleep(1000);
  }
  throw new Error('Timed out waiting for snapshot');
}

async function main() {
  let passed = 0, failed = 0;
  function assert(label, condition, detail = '') {
    if (condition) { console.log(`  ✅ ${label}`); passed++; }
    else { console.log(`  ❌ ${label}${detail ? ': ' + detail : ''}`); failed++; }
  }

  console.log('================================================');
  console.log('  Gateway Integration Test');
  console.log('================================================\n');

  killPort(MODBUS_PORT); killPort(HTTP_PORT);
  await sleep(500);

  const sim = await startSimulator();
  const { proc: backend, configDir } = await startBackend();

  try {
    const baseUrl = `http://127.0.0.1:${HTTP_PORT}`;
    await waitForConnection(baseUrl);
    const snap = await waitForSnapshot(baseUrl);

    assert('Snapshot received', !!snap);
    assert('Device type code is 7001', snap.device_type_code === '7001', `Got ${snap.device_type_code}`);
    assert('Device display is Gateway', snap.device_type_display === 'Gateway', `Got ${snap.device_type_display}`);
    assert('Software version present', /^GA\d{6}$/.test(snap.gateway_software_version ?? ''), `Got ${snap.gateway_software_version}`);
    assert('V1 firmware (simulator)', snap.gateway_is_v2 === false, `Got ${snap.gateway_is_v2}`);
    assert('Parallel AIO count = 1', snap.parallel_aio_count === 1, `Got ${snap.parallel_aio_count}`);
    assert('Parallel AIO online = 1', snap.parallel_aio_online === 1, `Got ${snap.parallel_aio_online}`);
    assert('AIO1 SOC in range', (snap.per_aio_soc?.[0] ?? 0) > 0 && snap.per_aio_soc[0] <= 100, `Got ${snap.per_aio_soc?.[0]}`);
    assert('Battery capacity ~13.5 kWh', Math.abs(snap.battery_capacity_kwh - 13.5) < 0.1, `Got ${snap.battery_capacity_kwh}`);
    assert('Max battery power = 6000W', snap.max_battery_power_w === 6000, `Got ${snap.max_battery_power_w}`);
    assert('First inverter serial present', !!snap.first_inverter_serial);
    assert('Solar power >= 0', snap.solar_power >= 0, `Got ${snap.solar_power}`);
    assert('Home power >= 0', snap.home_power >= 0, `Got ${snap.home_power}`);
    assert('Grid frequency is NaN', Number.isNaN(snap.grid_frequency), `Got ${snap.grid_frequency}`);
    assert('No faults', Array.isArray(snap.gateway_fault_codes) && snap.gateway_fault_codes.length === 0);
    assert('Max charge slots >= 2', snap.max_charge_slots >= 2, `Got ${snap.max_charge_slots}`);

    // API endpoints respond
    const rateResp = await fetchJson(`${baseUrl}/api/control/charge-rate`);
    assert('Charge rate API responds', rateResp.ok !== false);

    const now = Math.floor(Date.now() / 1000);
    const history = await fetchJson(`${baseUrl}/api/history?range=today&t=${now}`);
    assert('History API responds', !!history);

    console.log(`\n================================================`);
    console.log(`  Results: ${passed} passed, ${failed} failed`);
    console.log(`================================================\n`);
  } finally {
    backend.kill('SIGTERM'); sim.kill('SIGTERM');
    await sleep(1000);
    backend.kill('SIGKILL'); sim.kill('SIGKILL');
    try { fs.rmSync(configDir, { recursive: true }); } catch {}
    killPort(MODBUS_PORT); killPort(HTTP_PORT);
  }
  process.exit(failed > 0 ? 1 : 0);
}

main().catch(err => { console.error('[gateway-test] Fatal:', err); process.exit(1); });
