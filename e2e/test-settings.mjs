/**
 * E2E test settings helper — Node.js (.mjs) variant.
 *
 * Mirrors `e2e/test-settings.ts` for Node-only test scripts that can't
 * load TypeScript directly (notably `e2e/local-gateway-test.mjs`).
 *
 * Writes a complete, test-appropriate `settings.json` to a per-process
 * temp directory and returns the path + env vars needed to point a
 * spawned backend at it. Centralised so the same isolation contract is
 * honoured by every E2E entry point:
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
 *      empty so suites that exercise Agile must supply their own mock URL.
 */

import { spawn } from 'child_process';
import { promises as fsp, existsSync, mkdirSync, rmSync, writeFileSync } from 'fs';
import { tmpdir } from 'os';
import * as path from 'path';

/**
 * @typedef {Object} TestSettingsOverrides
 * @property {string} [host]         Inverter Modbus host (default '127.0.0.1')
 * @property {number} port           Inverter Modbus port
 * @property {string} [serial]       Inverter serial number (default empty)
 * @property {number} httpPort       HTTP server port
 * @property {number} [pollInterval] Poll interval seconds (default 5)
 * @property {string} [tag]          Optional override tag appended to temp dir name
 */

/**
 * @typedef {Object} TestSettingsFixture
 * @property {string} configDir      Absolute path to the temp directory
 * @property {string} settingsPath   Absolute path to the written settings.json
 * @property {Record<string,string>} env Env-var map to merge into spawned child env
 * @property {() => Promise<void>} cleanup Async cleanup; idempotent
 */

/**
 * @param {TestSettingsOverrides} overrides
 * @returns {Promise<TestSettingsFixture>}
 */
export async function writeTestSettings(overrides) {
  const dirTag = overrides.tag ?? 'default';
  // Combine a monotonic counter with the caller-supplied tag so two
  // back-to-back calls (faster than Date.now()'s millisecond resolution)
  // still produce distinct dirs. The counter is process-local so it
  // can't leak across test runs, and using a counter instead of a
  // nanosecond timestamp keeps dir names short and human-readable.
  const sequence = ++writeTestSettingsSeq;
  const configDir = path.join(
    tmpdir(),
    `givenergy-e2e-${dirTag}-${process.pid}-${sequence}`,
  );
  const dataDir = path.join(configDir, '.givenergy-local');
  mkdirSync(dataDir, { recursive: true });

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
  writeFileSync(settingsPath, JSON.stringify(settings, null, 2));

  // Defense-in-depth: set BOTH HOME and GIVENERGY_LOCAL_CONFIG_DIR. The
  // backend's Settings::settings_dir() checks GIVENERGY_LOCAL_CONFIG_DIR
  // first; HOME is what `dirs::home_dir()` consults on Linux/macOS as a
  // fallback. Setting both means neither lookup can accidentally land
  // on the user's real ~/.givenergy-local/ directory.
  const env = {
    HOME: configDir,
    GIVENERGY_LOCAL_CONFIG_DIR: dataDir,
  };

  let cleaned = false;
  const cleanup = async () => {
    if (cleaned) return;
    cleaned = true;
    try {
      await fsp.rm(configDir, { recursive: true, force: true });
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