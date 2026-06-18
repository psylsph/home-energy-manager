import { defineConfig } from '@playwright/test';

/**
 * Local-only Playwright config for tests that require the real GivEnergy simulator.
 *
 * These tests are NOT run on GitHub CI. Run locally with:
 *   npm run test:e2e:local
 *
 * Prerequisites:
 *   1. Build frontend: npm run build
 *   2. Build backend: cd src-tauri && cargo build --release
 *   3. Build simulator: cd ~/repos/givenergy-simulator && cargo build --release
 */
export default defineConfig({
  testDir: './e2e',
  testMatch: '**/local-*.spec.ts',
  fullyParallel: false,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  retries: 0,
  reporter: 'list',
  globalSetup: './e2e/local-global-setup.ts',
  globalTimeout: 600_000,
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
      name: 'local-e2e',
      testDir: './e2e',
    },
  ],
});
