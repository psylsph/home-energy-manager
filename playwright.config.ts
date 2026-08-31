import { defineConfig } from '@playwright/test';

const envExecutablePath = (globalThis as typeof globalThis & {
  process?: { env?: Record<string, string | undefined> };
}).process?.env?.PLAYWRIGHT_EXECUTABLE_PATH;

export default defineConfig({
  testDir: './e2e',
  testMatch: [
    '**/control.spec.ts',
    '**/adaptive-charge.spec.ts',
    '**/force-stop.spec.ts',
    '**/aio.spec.ts',
    '**/charts.spec.ts',
    '**/history-cost.spec.ts',
    '**/agile-slot.spec.ts',
    '**/pv2-after-sunset.spec.ts',
    '**/timed-export-slot-guard.spec.ts',
    '**/timed-export-stop-restart.spec.ts',
    '**/websocket-stale-timeout.spec.ts',
    '**/charge-slot-target-soc.spec.ts',
  ],
  fullyParallel: false,
  workers: 1,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  retries: 0,
  reporter: 'list',
  globalSetup: './e2e/global-setup.ts',
  // The suite is fully serial against one shared mock server (workers: 1)
  // and every Modbus register write is a real ~1.5s round-trip. Each test
  // also pays the per-test harness reset (fixture.ts: backend state reset +
  // one poll-wait, ~5s typical) for spec-order independence, so the full run
  // comfortably exceeds 40 minutes. Raise the ceiling so the global timeout
  // never aborts in-flight tests (which then report bogus failures).
  globalTimeout: 3_300_000,
  use: {
    headless: true,
    browserName: 'chromium',
    // CI uses Playwright's managed Chromium; local Debian installs can opt
    // into the system browser with PLAYWRIGHT_EXECUTABLE_PATH.
    launchOptions: {
      executablePath: envExecutablePath,
    },
    viewport: { width: 1280, height: 900 },
    actionTimeout: 10_000,
    navigationTimeout: 10_000,
    baseURL: 'http://127.0.0.1:17337',
  },
  projects: [
    {
      name: 'e2e',
      testDir: './e2e',
    },
  ],
});
