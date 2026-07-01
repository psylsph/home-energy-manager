import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './e2e',
  testMatch: [
    '**/control.spec.ts',
    '**/force-stop.spec.ts',
    '**/aio.spec.ts',
    '**/charts.spec.ts',
    '**/history-cost.spec.ts',
    '**/agile-slot.spec.ts',
    '**/pv2-after-sunset.spec.ts',
  ],
  fullyParallel: false,
  workers: 1,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  retries: 0,
  reporter: 'list',
  globalSetup: './e2e/global-setup.ts',
  // The suite is fully serial against one shared backend (workers: 1) and
  // every Modbus register write is a real ~1.5s round-trip, so the full run
  // comfortably exceeds 20 minutes. Raise the ceiling so the global timeout
  // never aborts in-flight tests (which then report bogus "0 writes" failures).
  globalTimeout: 2_400_000,
  use: {
    headless: true,
    browserName: 'chromium',
    channel: 'chrome',
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
