import type { APIRequestContext, Page } from '@playwright/test';
import { test, expect } from './fixture.js';
import { startBackend, stopBackend } from './backend.js';
import { servePlannerFixtures } from './forecast-fixtures.js';

// Issue #391: the Plan Settings number fields saved only on blur, so a value
// changed without the field losing focus — spin arrows, then leaving the
// page — never reached the backend, and the page showed the old value on
// return. These run against the real backend's /api/settings and never blur
// the field: leaving the page is a hash change, not a click elsewhere.

test.beforeAll(async () => {
  await startBackend();
});
test.afterAll(async () => {
  await stopBackend();
});

const DEFAULTS = {
  forecast_min_soc_pct: 20,
  forecast_charge_efficiency: 0.9,
  forecast_discharge_efficiency: 0.95,
};

async function savedSettings(request: APIRequestContext): Promise<Record<string, unknown>> {
  const res = await request.get('/api/settings');
  return ((await res.json()) as { data: Record<string, unknown> }).data;
}

async function openPlanSettings(page: Page) {
  await servePlannerFixtures(page);
  await page.goto('/#/forecast');
  await page.getByText('Plan Settings').click();
  const minSoc = page.getByTestId('forecast-min-soc-input');
  await expect(minSoc).toBeEnabled();
  return minSoc;
}

// Leave the Forecast page without moving focus, the way the browser's back
// button or a keyboard shortcut does.
async function leaveWithoutBlur(page: Page) {
  await page.evaluate(() => {
    window.location.hash = '#/status';
  });
  await expect(page.getByTestId('forecast-min-soc-input')).toHaveCount(0);
}

test.describe('Planner settings save without a blur (issue #391)', () => {
  test.beforeEach(async ({ request }) => {
    await request.post('/api/settings', { data: DEFAULTS });
  });
  test.afterEach(async ({ request }) => {
    await request.post('/api/settings', { data: DEFAULTS });
  });

  test('an edited minimum battery level survives leaving the page', async ({ page, request }) => {
    const minSoc = await openPlanSettings(page);
    await expect(minSoc).toHaveValue('20');
    await minSoc.fill('35');
    await leaveWithoutBlur(page);

    await expect.poll(async () => (await savedSettings(request)).forecast_min_soc_pct).toBe(35);
    await page.evaluate(() => {
      window.location.hash = '#/forecast';
    });
    await page.getByText('Plan Settings').click();
    await expect(page.getByTestId('forecast-min-soc-input')).toHaveValue('35');
  });

  test('spin-arrow changes save after a pause and survive a reload', async ({ page, request }) => {
    const minSoc = await openPlanSettings(page);
    await expect(minSoc).toHaveValue('20');
    await minSoc.focus();
    for (let i = 0; i < 3; i++) await minSoc.press('ArrowUp');
    await expect(minSoc).toHaveValue('23');
    await expect(minSoc).toBeFocused();

    await expect.poll(async () => (await savedSettings(request)).forecast_min_soc_pct).toBe(23);
    await page.reload();
    await page.getByText('Plan Settings').click();
    await expect(page.getByTestId('forecast-min-soc-input')).toHaveValue('23');
  });

  test('Enter saves the battery efficiencies', async ({ page, request }) => {
    await openPlanSettings(page);
    const charge = page.getByTestId('forecast-charge-eff-input');
    await expect(charge).toHaveValue('90');
    await charge.fill('85');
    await charge.press('Enter');

    await expect
      .poll(async () => (await savedSettings(request)).forecast_charge_efficiency)
      .toBe(0.85);
    await expect(charge).toBeFocused();
  });
});
