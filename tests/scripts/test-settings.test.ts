import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { mkdtempSync, existsSync, readFileSync, rmSync, mkdirSync, writeFileSync } from 'fs';
import { homedir, tmpdir } from 'os';
import * as path from 'path';
import { writeTestSettings } from '../../e2e/test-settings.js';

/**
 * The E2E test settings helper is the single source of truth for
 * "what does a test settings.json look like". If it ever drifts from
 * the isolation contract documented in e2e/test-settings.ts, a real
 * e2e run could write to the user's production ~/.givenergy-local/.
 *
 * These tests pin the contract at the unit-test level so a regression
 * surfaces here instead of as a quietly-clobbered production config.
 *
 * Pattern: this file imports the .ts source via the same alias vitest
 * would resolve, which means a syntax error in the helper fails the
 * whole unit-test run before any e2e suite starts.
 */

describe('e2e/test-settings.ts — production isolation contract', () => {
  let originalHome: string | undefined;
  let originalConfigDir: string | undefined;

  beforeEach(() => {
    // Snapshot the real env so we can prove the helper never reads them.
    originalHome = process.env.HOME;
    originalConfigDir = process.env.GIVENERGY_LOCAL_CONFIG_DIR;
  });

  afterEach(() => {
    if (originalHome === undefined) delete process.env.HOME;
    else process.env.HOME = originalHome;
    if (originalConfigDir === undefined) delete process.env.GIVENERGY_LOCAL_CONFIG_DIR;
    else process.env.GIVENERGY_LOCAL_CONFIG_DIR = originalConfigDir;
  });

  it('writes a settings.json under a per-process temp directory', async () => {
    const fixture = await writeTestSettings({
      tag: 'unit',
      port: 18899,
      httpPort: 17337,
    });

    try {
      // The settings file must exist on disk.
      expect(existsSync(fixture.settingsPath)).toBe(true);

      // The settings path must be inside the OS temp directory and must not
      // equal the live config path. On Windows, %TEMP% normally lives under
      // the user's profile, so merely asserting that it is outside HOME would
      // reject a correctly isolated fixture.
      const settingsAbs = path.resolve(fixture.settingsPath);
      const tempAbs = path.resolve(tmpdir());
      const liveConfigDir = path.resolve(
        originalConfigDir ?? path.join(originalHome ?? homedir(), '.givenergy-local'),
      );
      expect(settingsAbs.startsWith(tempAbs + path.sep)).toBe(true);
      expect(settingsAbs).toBe(
        path.resolve(fixture.configDir, '.givenergy-local', 'settings.json'),
      );
      expect(settingsAbs).not.toBe(path.join(liveConfigDir, 'settings.json'));
    } finally {
      await fixture.cleanup();
    }
  });

  it('returns env vars that point at the temp dir, not at the real one', async () => {
    const fixture = await writeTestSettings({
      tag: 'unit',
      port: 18899,
      httpPort: 17337,
    });

    try {
      // Both HOME and GIVENERGY_LOCAL_CONFIG_DIR must be set so the
      // backend can't fall through to the user's real config.
      expect(fixture.env.HOME).toBeDefined();
      expect(fixture.env.GIVENERGY_LOCAL_CONFIG_DIR).toBeDefined();

      // They must match the same temp directory the file was written to.
      expect(path.resolve(fixture.env.HOME)).toBe(path.resolve(fixture.configDir));
      expect(path.resolve(fixture.env.GIVENERGY_LOCAL_CONFIG_DIR)).toBe(
        path.resolve(path.join(fixture.configDir, '.givenergy-local')),
      );

      // Neither env value may point at the real production config dir.
      const realHome = originalHome ?? '';
      expect(fixture.env.HOME).not.toBe(realHome);
      expect(fixture.env.GIVENERGY_LOCAL_CONFIG_DIR).not.toBe(
        path.join(realHome, '.givenergy-local'),
      );
    } finally {
      await fixture.cleanup();
    }
  });

  it('writes a complete settings.json with every required field set', async () => {
    const fixture = await writeTestSettings({
      tag: 'unit',
      port: 18900,
      httpPort: 17338,
      serial: 'TEST-SERIAL',
      pollInterval: 3,
    });

    try {
      const parsed = JSON.parse(readFileSync(fixture.settingsPath, 'utf8'));

      // Inverter connection — must match what the caller asked for.
      expect(parsed.host).toBe('127.0.0.1');
      expect(parsed.port).toBe(18900);
      expect(parsed.serial).toBe('TEST-SERIAL');
      expect(parsed.poll_interval).toBe(3);
      expect(parsed.http_port).toBe(17338);
      expect(parsed.auto_connect).toBe(true);

      // CRITICAL: disable_auto_discovery must be true so the test backend
      // can't scan the user's LAN if the simulator host is slow to accept.
      // Without this, a flaky test environment could trigger a full subnet
      // discovery walk on the developer's machine.
      expect(parsed.disable_auto_discovery).toBe(true);

      // Telemetry: alerts OFF so no real Telegram/ntfy/Pushover sends.
      // A test run must never contact a real push-notification endpoint.
      expect(parsed.alerts_config.enabled).toBe(false);
      expect(parsed.alerts_config.telegram_bot_token).toBe('');
      expect(parsed.alerts_config.telegram_chat_id).toBe('');
      expect(parsed.alerts_config.ntfy_topic).toBe('');
      expect(parsed.alerts_config.pushover_app_token).toBe('');
      expect(parsed.alerts_config.pushover_user_key).toBe('');
      expect(parsed.alerts_config.daily_report_enabled).toBe(false);

      // Telemetry: weather OFF so no Open-Meteo requests fire.
      // The history.db written by tests is per-process and gets thrown
      // away — any Open-Meteo backfill would be wasted bandwidth.
      expect(parsed.weather_config.enabled).toBe(false);
      expect(parsed.weather_config.postcode).toBe('');

      // Scheduling engines must be off so tests get a clean slate.
      expect(parsed.agile_enabled).toBe(false);
      expect(parsed.agile_scope).toBe('off');
      expect(parsed.cosy_enabled).toBe(false);

      // Tariff config must be a valid slot list (mirrors the validation
      // contract in src-tauri/src/settings/mod.rs::TariffConfig::validate).
      expect(Array.isArray(parsed.import_tariff_config.slots)).toBe(true);
      expect(parsed.import_tariff_config.slots.length).toBeGreaterThan(0);
      expect(Array.isArray(parsed.export_tariff_config.slots)).toBe(true);

      // Cosy always has 3 slots so the Vec<CosySlot> schema is honoured.
      expect(Array.isArray(parsed.cosy_slots)).toBe(true);
      expect(parsed.cosy_slots.length).toBe(3);
    } finally {
      await fixture.cleanup();
    }
  });

  it('uses a unique temp dir per call so concurrent suites do not collide', async () => {
    // Two writeTestSettings calls in quick succession must produce
    // different temp dirs. The helper appends process.pid + Date.now()
    // to the dir name, so this is a property of the implementation
    // that we pin so a future "simplification" (e.g. dropping the
    // timestamp) gets caught here.
    const a = await writeTestSettings({ tag: 'concurrent', port: 18901, httpPort: 17339 });
    const b = await writeTestSettings({ tag: 'concurrent', port: 18902, httpPort: 17340 });

    try {
      expect(path.resolve(a.configDir)).not.toBe(path.resolve(b.configDir));
      expect(existsSync(a.settingsPath)).toBe(true);
      expect(existsSync(b.settingsPath)).toBe(true);
    } finally {
      await Promise.all([a.cleanup(), b.cleanup()]);
    }
  });

  it('cleanup removes the temp dir (idempotent)', async () => {
    const fixture = await writeTestSettings({
      tag: 'cleanup',
      port: 18903,
      httpPort: 17341,
    });
    expect(existsSync(fixture.configDir)).toBe(true);

    await fixture.cleanup();
    expect(existsSync(fixture.configDir)).toBe(false);

    // Calling cleanup twice must not throw — tests may run cleanup in
    // a finally block plus a global teardown, and the double-call must
    // be safe.
    await expect(fixture.cleanup()).resolves.toBeUndefined();
  });

  it('never touches the user\'s real ~/.givenergy-local/', async () => {
    // Stage a fake production config under a temp HOME so we can detect
    // any accidental writes to it. The helper is invoked with the real
    // env still in scope; if it ever read HOME instead of the override
    // env var, the staged file would survive.
    const stagedHome = mkdtempSync(path.join(tmpdir(), 'givenergy-prod-home-'));
    const stagedConfig = path.join(stagedHome, '.givenergy-local');
    const stagedSettings = path.join(stagedConfig, 'settings.json');
    mkdirSync(stagedConfig, { recursive: true });
    writeFileSync(
      stagedSettings,
      JSON.stringify({ host: 'PRODUCTION-DETECT-ME', port: 9999 }),
    );

    const previousHome = process.env.HOME;
    process.env.HOME = stagedHome;

    try {
      const fixture = await writeTestSettings({
        tag: 'no-touch',
        port: 18904,
        httpPort: 17342,
      });

      try {
        // The fake production settings must be UNCHANGED — the helper
        // must have written to a different temp dir, not the staged one.
        const after = JSON.parse(readFileSync(stagedSettings, 'utf8'));
        expect(after.host).toBe('PRODUCTION-DETECT-ME');
        expect(after.port).toBe(9999);

        // And the helper's settings file must NOT live under stagedHome.
        const settingsAbs = path.resolve(fixture.settingsPath);
        const homeAbs = path.resolve(stagedHome);
        expect(settingsAbs.startsWith(homeAbs + path.sep)).toBe(false);
      } finally {
        await fixture.cleanup();
      }
    } finally {
      if (previousHome === undefined) delete process.env.HOME;
      else process.env.HOME = previousHome;
      rmSync(stagedHome, { recursive: true, force: true });
    }
  });
});