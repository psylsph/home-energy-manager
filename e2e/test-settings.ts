/**
 * Shared helper for E2E test config files.
 *
 * Writes a complete, test-appropriate `settings.json` to a per-process temp
 * directory and returns the path + env vars needed to point a spawned
 * backend at it. Centralised here so every E2E suite uses the same
 * isolation contract:
 *
 *   1. **Never touch the production config** (`~/.givenergy-local/settings.json`).
 *      Every settings file lives under `os.tmpdir()` and is removed on teardown.
 *   2. **Set both HOME and GIVENERGY_LOCAL_CONFIG_DIR** on the spawned backend.
 *      `HOME` covers the dirs-crate path; `GIVENERGY_LOCAL_CONFIG_DIR` is the
 *      explicit override honoured first by `Settings::settings_dir()`. Setting
 *      both is defense-in-depth so the backend cannot accidentally fall back
 *      to the user's real config if one env var is dropped on some platform.
 *   3. **Set `disable_auto_discovery: true`** so a misconfigured inverter
 *      host doesn't trigger a LAN scan during tests (which would slow the
 *      suite and potentially hit real production devices on the test box).
 *   4. **Use telemetry defaults that are safe for offline / local-only
 *      runs** — alerts disabled (no real Telegram/ntfy/Pushover sends),
 *      weather disabled (no Open-Meteo backfill writes to a per-process
 *      SQLite history that gets thrown away anyway), and `agile_api_base_url`
 *      set to the real Octopus endpoint so suites that DO exercise Agile
 *      don't have to override the URL themselves unless they want a mock.
 *      Tests that need a mock Octopus server (e.g. agile-slot.spec.ts)
 *      POST their own `api_base_url` via `/api/agile` per-test.
 *
 * Each call returns a unique temp dir so concurrent suites (e.g. the
 * misbehaviour tests, which spin up multiple backends) don't collide.
 */

import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

export interface TestSettingsOverrides {
  /** Inverter Modbus host (default: '127.0.0.1' — the simulator). */
  host?: string;
  /** Inverter Modbus port. Caller is expected to know the simulator's port. */
  port: number;
  /** Inverter serial number (default: empty). */
  serial?: string;
  /** HTTP server port. Caller is expected to know its assigned port. */
  httpPort: number;
  /** Poll interval in seconds (default: 5). */
  pollInterval?: number;
  /** Optional override tag appended to the temp dir name (helps debugging). */
  tag?: string;
}

export interface TestSettingsFixture {
  /** Absolute path to the temp directory (no trailing `.givenergy-local`). */
  configDir: string;
  /** Absolute path to the written settings.json file. */
  settingsPath: string;
  /** Env-var map to merge into the spawned backend's environment. */
  env: Record<string, string>;
  /** Async cleanup function — removes the temp dir. Idempotent. */
  cleanup: () => Promise<void>;
}

/**
 * Write a settings.json suitable for E2E tests at the given location and
 * return the path + env vars. The file is fully populated so the backend
 * never falls back to defaults (which might differ from production
 * defaults in ways that break a specific test).
 *
 * Every field that production callers might rely on is set explicitly:
 *   - Inverter host/port/serial: pointed at the supplied (host, port)
 *   - `auto_connect: true` so the poll loop starts immediately
 *   - `disable_auto_discovery: true` so the backend doesn't scan the LAN
 *     if the simulator is slow to come up
 *   - `agile_enabled: false` and `cosy_enabled: false` so the slot/state
 *     machine engines don't pre-arm slots before the test gets control
 *   - `alerts_config.enabled: false` so the background evaluator never
 *     tries to send a real Telegram/ntfy/Pushover push notification
 *   - `weather_config.enabled: false` so no Open-Meteo requests are
 *     fired (the suite's per-process SQLite history.db gets discarded)
 *   - Three all-disabled `cosy_slots` (matches the production schema
 *     — the Vec<CosySlot> must have exactly 3 entries once enabled)
 *
 * Tariff fields are set to the project's documented defaults so any
 * cost-calculation assertions that hit them are stable.
 */
export async function writeTestSettings(
  overrides: TestSettingsOverrides,
): Promise<TestSettingsFixture> {
  const dirTag = overrides.tag ?? 'default';
  // Combine a monotonic counter with the caller-supplied tag so two
  // back-to-back calls (faster than Date.now()'s millisecond resolution)
  // still produce distinct dirs. The counter is process-local so it
  // can't leak across test runs, and using a counter instead of a
  // nanosecond timestamp keeps dir names short and human-readable.
  const sequence = ++writeTestSettingsSeq;
  const configDir = path.join(
    os.tmpdir(),
    `givenergy-e2e-${dirTag}-${process.pid}-${sequence}`,
  );
  const dataDir = path.join(configDir, '.givenergy-local');
  fs.mkdirSync(dataDir, { recursive: true });

  const settings = {
    // -- Inverter connection --
    host: overrides.host ?? '127.0.0.1',
    port: overrides.port,
    serial: overrides.serial ?? '',
    poll_interval: overrides.pollInterval ?? 5,
    http_port: overrides.httpPort,
    auto_connect: true,
    // CRITICAL: never let the test backend scan the user's LAN if the
    // configured simulator host is slow to accept. Without this the
    // discovery module would walk every /24 on the test box's interfaces.
    disable_auto_discovery: true,

    // -- Tariffs --
    import_tariff: 0.285,
    export_tariff: 0.15,
    import_standing_charge_p_per_day: 0.0,
    import_tariff_config: {
      slots: [
        { start: '00:00', end: '00:30', rate: 0.285 },
        { start: '00:30', end: '05:30', rate: 0.09 },
        { start: '05:30', end: '23:59', rate: 0.285 },
      ],
    },
    export_tariff_config: {
      slots: [{ start: '00:00', end: '23:59', rate: 0.15 }],
    },

    // -- Auto winter / load limiter (off — we test these explicitly) --
    auto_winter_enabled: false,
    auto_winter_cold_threshold: 8.0,
    auto_winter_recovery_threshold: 12.0,
    auto_winter_target_soc: 80,
    auto_winter_debounce_readings: 10,
    auto_winter_saved_enable_target: null,
    auto_winter_saved_target_soc: null,
    load_limiter_enabled: false,
    load_limiter_threshold_w: 3000,
    load_limiter_trigger_delay_minutes: 5,
    load_limiter_start_hour: 0,
    load_limiter_start_minute: 0,
    load_limiter_end_hour: 0,
    load_limiter_end_minute: 0,
    load_limiter_active_persisted: false,
    load_limiter_saved_reserve: null,

    // -- Agile Octopus (off; tests that need it POST their own scope + URL) --
    agile_enabled: false,
    agile_scope: 'off',
    agile_region: 'A',
    agile_charge_threshold: 10.0,
    agile_discharge_threshold: 30.0,
    agile_api_base_url: '',
    agile_state_persisted: '',

    // -- Cosy (off; 3 dummy slots so the Vec<CosySlot> schema is honoured) --
    cosy_enabled: false,
    cosy_slots: [
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
    ],
    cosy_active_persisted: false,

    // -- EV charger (off; tests that need it set their own host) --
    evc_host: '',
    evc_port: 502,

    // -- Telemetry: alerts OFF so no real Telegram/ntfy/Pushover sends --
    alerts_config: {
      enabled: false,
      telegram_bot_token: '',
      telegram_chat_id: '',
      cooldown_minutes: 30,
      batt_temp_min: 8.0,
      batt_temp_max: 50.0,
      soc_min: 4,
      soc_max: 100,
      grid_offline_enabled: true,
      connection_lost_enabled: true,
      battery_over_temp_enabled: true,
      solar_clipping_enabled: false,
      solar_clipping_ceiling_w: 0,
      ntfy_topic: '',
      ntfy_server: 'https://ntfy.sh',
      pushover_app_token: '',
      pushover_user_key: '',
      daily_report_enabled: false,
      daily_report_hour: 8,
      daily_report_minute: 0,
    },

    // -- Telemetry: weather OFF so no Open-Meteo requests fire and no
    //    backfill writes land in the per-process history.db (which gets
    //    thrown away on teardown anyway). --
    weather_config: {
      enabled: false,
      postcode: '',
      latitude: null,
      longitude: null,
      last_backfill_completed: '',
      open_meteo_base_url: 'https://api.open-meteo.com',
    },

    // -- Misc --
    hidden_panels: [],
    autostart_enabled: false,
    api_key: '',
    api_port: 0,
    discharge_slots_backup: null,
  };

  const settingsPath = path.join(dataDir, 'settings.json');
  fs.writeFileSync(settingsPath, JSON.stringify(settings, null, 2));

  // Defense-in-depth: set BOTH HOME and GIVENERGY_LOCAL_CONFIG_DIR. The
  // backend's Settings::settings_dir() checks GIVENERGY_LOCAL_CONFIG_DIR
  // first; HOME is what `dirs::home_dir()` consults on Linux/macOS as a
  // fallback. Setting both means neither lookup can accidentally land
  // on the user's real ~/.givenergy-local/ directory.
  const env: Record<string, string> = {
    HOME: configDir,
    GIVENERGY_LOCAL_CONFIG_DIR: dataDir,
  };

  let cleaned = false;
  const cleanup = async () => {
    if (cleaned) return;
    cleaned = true;
    try {
      await fs.promises.rm(configDir, { recursive: true, force: true });
    } catch {
      /* best-effort; the OS will sweep /tmp eventually */
    }
  };

  return { configDir, settingsPath, env, cleanup };
}

/**
 * Process-local monotonic counter used to disambiguate temp dirs
 * created within a single process. `Date.now()` only has millisecond
 * resolution so two back-to-back calls would otherwise collide —
 * this counter increments per call so the dirs are always unique.
 */
let writeTestSettingsSeq = 0;