#!/usr/bin/env node
/**
 * A mock of Octopus's GraphQL API serving planned Intelligent Octopus Go
 * charges you choose, for checking the car line in the Status page's Next
 * Charge card (issue #474) and anything else that reads the dispatch list.
 *
 *   npm run mock:octopus -- --with-hem --slot 23:30-05:30@12 --slot 13:00-13:30:boost
 *
 * Slots are local times, `HH:MM-HH:MM`, with optional `@kWh` energy and
 * `:boost` for a boost (otherwise smart). An end before the start runs past
 * midnight; a slot whose end has passed today is moved to tomorrow.
 * `--running MIN` adds a smart charge that started 10 minutes ago and runs
 * MIN more minutes. `--file slots.json` reads a JSON array of slot strings
 * (or `{start, end, type, energyAddedKwh}` objects) on every request, so you
 * can edit it while the mock runs; HEM asks every 3 minutes, and Settings →
 * Panel Controls → Show Car Charging Slots switching on asks at once.
 *
 * `--with-hem` also starts the GivEnergy simulator and a headless HEM in a
 * throwaway config folder, already pointed at the mock with the car line on
 * and the battery hold off, and prints the address to open. Your real
 * install and its settings are never touched. Needs `npm run build`,
 * `cargo build --release` and the simulator at
 * ~/repos/givenergy-simulator/target/release/sim-api.
 *
 * Without `--with-hem`, point a HEM at the mock yourself by setting
 * `octopus_api_base_url` in a settings.json it isn't running from. Don't do
 * that on your real install with the battery hold on: HEM would really pause
 * the battery for the made-up charges.
 */

import { spawn } from 'node:child_process';
import * as fs from 'node:fs';
import * as http from 'node:http';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';

const MINUTE = 60_000;

/**
 * One slot string as an Octopus dispatch, against `now` in local time.
 * Returns `null` for anything it can't read.
 */
export function parseSlot(text, now = new Date()) {
  const m = /^(\d{1,2}):(\d{2})-(\d{1,2}):(\d{2})(?:@([\d.]+))?(?::(boost|smart))?$/i.exec(String(text).trim());
  if (!m) return null;
  const [sh, sm, eh, em] = [m[1], m[2], m[3], m[4]].map(Number);
  if (sh > 23 || eh > 23 || sm > 59 || em > 59) return null;
  const start = new Date(now);
  start.setHours(sh, sm, 0, 0);
  const end = new Date(now);
  end.setHours(eh, em, 0, 0);
  if (end <= start) end.setDate(end.getDate() + 1);
  // Over already: tomorrow's.
  if (end <= now) {
    start.setDate(start.getDate() + 1);
    end.setDate(end.getDate() + 1);
  }
  const kwh = m[5] === undefined ? null : Number(m[5]);
  return dispatch(start, end, (m[6] ?? 'smart').toUpperCase(), kwh);
}

/** A charge running now: started ten minutes ago, `minutes` more to go. */
export function runningSlot(minutes, now = new Date()) {
  return dispatch(new Date(now.getTime() - 10 * MINUTE), new Date(now.getTime() + minutes * MINUTE), 'SMART', null);
}

function dispatch(start, end, type, kwh) {
  // Octopus signs the energy as an import (negative).
  return { start: start.toISOString(), end: end.toISOString(), type, energyAddedKwh: kwh == null ? null : -kwh };
}

/** A `--file` entry: a slot string, or a dispatch object passed through. */
export function fileEntry(entry, now = new Date()) {
  if (typeof entry === 'string') return parseSlot(entry, now);
  if (entry && typeof entry.start === 'string' && typeof entry.end === 'string') return entry;
  return null;
}

/** The GraphQL answer for one request body, or `null` for "not mocked". */
export function answer(body, dispatches) {
  let query = '';
  try {
    query = String(JSON.parse(body).query ?? '');
  } catch {
    return null;
  }
  if (query.includes('obtainKrakenToken')) return { obtainKrakenToken: { token: 'mock-token' } };
  if (query.includes('devices(')) {
    return { devices: [{ id: 'mock-car', deviceType: 'ELECTRIC_VEHICLES', status: { current: 'LIVE' } }] };
  }
  if (query.includes('flexPlannedDispatches')) return { flexPlannedDispatches: dispatches };
  return null;
}

function parseArgs(argv) {
  const opts = { port: 8099, http: 7340, modbus: 5503, soc: 60, slots: [], running: [], file: null, withHem: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const next = () => {
      const v = argv[++i];
      if (v === undefined) throw new Error(`${a} needs a value`);
      return v;
    };
    if (a === '--port') opts.port = Number(next());
    else if (a === '--http') opts.http = Number(next());
    else if (a === '--modbus') opts.modbus = Number(next());
    else if (a === '--soc') opts.soc = Number(next());
    else if (a === '--slot') opts.slots.push(next());
    else if (a === '--running') opts.running.push(Number(next()));
    else if (a === '--file') opts.file = path.resolve(next());
    else if (a === '--with-hem') opts.withHem = true;
    else if (a === '--help' || a === '-h') opts.help = true;
    else throw new Error(`Unknown option ${a}`);
  }
  return opts;
}

function currentDispatches(opts) {
  const now = new Date();
  const list = [
    ...opts.slots.map((s) => parseSlot(s, now)),
    ...opts.running.map((m) => runningSlot(m, now)),
  ];
  if (opts.file) {
    try {
      const entries = JSON.parse(fs.readFileSync(opts.file, 'utf8'));
      if (Array.isArray(entries)) list.push(...entries.map((e) => fileEntry(e, now)));
      else console.warn(`[mock-octopus] ${opts.file} isn't a JSON array; ignoring it`);
    } catch (e) {
      console.warn(`[mock-octopus] can't read ${opts.file}: ${e.message}`);
    }
  }
  return list.filter(Boolean);
}

function startMock(opts) {
  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', (chunk) => (body += chunk));
    req.on('end', () => {
      res.setHeader('Content-Type', 'application/json');
      const data = req.method === 'POST' && req.url?.startsWith('/v1/graphql') ? answer(body, currentDispatches(opts)) : null;
      if (data === null) {
        // Account data, tariffs, Saving Sessions: not part of this mock.
        res.end(JSON.stringify({ errors: [{ message: 'not mocked' }] }));
        return;
      }
      if (data.flexPlannedDispatches) {
        console.log(`[mock-octopus] ${new Date().toLocaleTimeString()} served ${data.flexPlannedDispatches.length} dispatch(es)`);
      }
      res.end(JSON.stringify({ data }));
    });
  });
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(opts.port, '127.0.0.1', () => resolve(server));
  });
}

/** The throwaway settings.json for `--with-hem` (the same complete shape the e2e suites write). */
function hemSettings(opts) {
  return {
    host: '127.0.0.1', port: opts.modbus, serial: '', poll_interval: 5, write_pacing_ms: 25, http_port: opts.http,
    auto_connect: true, disable_auto_discovery: true,
    import_tariff: 0.285, export_tariff: 0.15, import_standing_charge_p_per_day: 0.0,
    import_tariff_config: { slots: [
      { start: '00:00', end: '00:30', rate: 0.285 },
      { start: '00:30', end: '05:30', rate: 0.09 },
      { start: '05:30', end: '23:59', rate: 0.285 },
    ] },
    export_tariff_config: { slots: [{ start: '00:00', end: '23:59', rate: 0.15 }] },
    auto_winter_enabled: false, auto_winter_cold_threshold: 8.0, auto_winter_recovery_threshold: 12.0,
    auto_winter_target_soc: 80, auto_winter_debounce_readings: 10, auto_winter_saved_enable_target: null,
    auto_winter_saved_target_soc: null,
    load_limiter_enabled: false, load_limiter_threshold_w: 3000, load_limiter_trigger_delay_minutes: 5,
    load_limiter_start_hour: 0, load_limiter_start_minute: 0, load_limiter_end_hour: 0, load_limiter_end_minute: 0,
    load_limiter_active_persisted: false, load_limiter_saved_reserve: null,
    agile_enabled: false, agile_scope: 'off', agile_region: 'A', agile_charge_threshold: 10.0,
    agile_discharge_threshold: 30.0, agile_api_base_url: '', agile_state_persisted: '',
    cosy_enabled: false,
    cosy_slots: [0, 1, 2].map(() => ({ enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 })),
    cosy_active_persisted: false,
    evc_host: '', evc_port: 502,
    alerts_config: {
      enabled: false, telegram_bot_token: '', telegram_chat_id: '', cooldown_minutes: 30, batt_temp_min: 8.0,
      batt_temp_max: 50.0, soc_min: 4, soc_max: 100, grid_offline_enabled: true, connection_lost_enabled: true,
      battery_over_temp_enabled: true, solar_clipping_enabled: false, solar_clipping_ceiling_w: 0, ntfy_topic: '',
      ntfy_server: 'https://ntfy.sh', pushover_app_token: '', pushover_user_key: '', daily_report_enabled: false,
      daily_report_hour: 8, daily_report_minute: 0,
    },
    weather_config: {
      enabled: false, postcode: '', latitude: null, longitude: null, last_backfill_completed: null,
      open_meteo_base_url: 'https://api.open-meteo.com',
    },
    hidden_panels: [], autostart_enabled: false, api_key: '', api_port: 0, discharge_slots_backup: null,
    // The point of it: Octopus is the mock, the car line is on, the hold is
    // off (so nothing pauses the simulated battery unless you turn it on).
    octopus_api_key: 'sk_test_mock',
    octopus_account_number: 'A-MOCK0001',
    octopus_api_base_url: `http://127.0.0.1:${opts.port}`,
    octopus_dispatch_hold: false,
    octopus_dispatch_charge: false,
    status_show_car_charging: true,
  };
}

function startHem(opts) {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
  const sim = path.join(os.homedir(), 'repos/givenergy-simulator/target/release/sim-api');
  const backend = path.join(root, 'src-tauri/target/release/givenergy-local');
  const dist = path.join(root, 'dist');
  for (const [what, file] of [['simulator', sim], ['backend (cargo build --release)', backend], ['frontend (npm run build)', path.join(dist, 'index.html')]]) {
    if (!fs.existsSync(file)) throw new Error(`No ${what} at ${file}`);
  }
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'hem-mock-octopus-'));
  const configDir = path.join(home, '.givenergy-local');
  fs.mkdirSync(configDir, { recursive: true });
  fs.writeFileSync(path.join(configDir, 'settings.json'), JSON.stringify(hemSettings(opts), null, 2));
  const children = [];
  const quiet = (child, label) => child.stderr?.on('data', (d) => {
    const text = d.toString();
    if (/error|panic/i.test(text)) process.stderr.write(`[${label}] ${text}`);
  });
  const simulator = spawn(sim, ['simulate', '--inverter', 'Gen3Hybrid', '--batteries', '2', '--battery-size', '9.5',
    '--soc', String(opts.soc), '--solar-peak', '4000', '--load-profile', 'family', '--weather', 'clear',
    '--modbus', `127.0.0.1:${opts.modbus}`], { stdio: ['ignore', 'ignore', 'pipe'] });
  quiet(simulator, 'sim');
  children.push(simulator);
  const hem = spawn(backend, ['--headless', '--port', String(opts.http), '--dist', dist], {
    stdio: ['ignore', 'ignore', 'pipe'],
    // Both, so it can never fall back to your real ~/.givenergy-local.
    env: { ...process.env, HOME: home, GIVENERGY_LOCAL_CONFIG_DIR: configDir },
  });
  quiet(hem, 'hem');
  children.push(hem);
  const stop = () => {
    for (const child of children) child.kill();
    fs.rmSync(home, { recursive: true, force: true });
  };
  return { stop, children };
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  if (opts.help) {
    console.log(fs.readFileSync(fileURLToPath(import.meta.url), 'utf8').split('*/')[0].replace(/^#!.*\n\/\*\*\n/, '').replace(/^ \* ?/gm, ''));
    return;
  }
  for (const s of opts.slots) if (!parseSlot(s)) throw new Error(`Can't read slot "${s}" (expected HH:MM-HH:MM[@kWh][:boost])`);
  const server = await startMock(opts);
  console.log(`[mock-octopus] Octopus mock on http://127.0.0.1:${opts.port}`);
  for (const d of currentDispatches(opts)) {
    console.log(`  ${d.type.padEnd(5)} ${new Date(d.start).toLocaleString()} → ${new Date(d.end).toLocaleTimeString()}${d.energyAddedKwh == null ? '' : ` (${Math.abs(d.energyAddedKwh)} kWh)`}`);
  }
  let hem = null;
  if (opts.withHem) {
    hem = startHem(opts);
    for (const child of hem.children) {
      child.on('exit', (code) => {
        if (code) console.error(`[mock-octopus] a child exited with code ${code}; stopping`);
        shutdown();
      });
    }
    console.log(`[mock-octopus] HEM (simulator, throwaway settings) on http://127.0.0.1:${opts.http}`);
    console.log('  Choose the House view in Settings → Panel Controls; the car line is already switched on.');
  } else {
    console.log(`  Point a test HEM at it with "octopus_api_base_url": "http://127.0.0.1:${opts.port}" in its settings.json.`);
    console.log('  Not your real install with the battery hold on: it would pause the battery for these charges.');
  }
  console.log('  Ctrl-C to stop.');
  let stopping = false;
  function shutdown() {
    if (stopping) return;
    stopping = true;
    hem?.stop();
    server.close(() => process.exit(0));
  }
  process.on('SIGINT', shutdown);
  process.on('SIGTERM', shutdown);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((e) => {
    console.error(`[mock-octopus] ${e.message}`);
    process.exit(1);
  });
}
