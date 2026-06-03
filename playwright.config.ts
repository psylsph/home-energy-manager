import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './e2e',
  fullyParallel: false,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  retries: 0,
  reporter: 'list',
  globalSetup: './e2e/global-setup.ts',
  globalTimeout: 300_000,
  use: {
    headless: true,
    browserName: 'chromium',
    channel: 'chrome',
    viewport: { width: 1280, height: 900 },
    actionTimeout: 5_000,
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
