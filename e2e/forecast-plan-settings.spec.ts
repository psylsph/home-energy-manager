import type { APIRequestContext, Locator, Page } from '@playwright/test';
import { test, expect } from './fixture.js';
import { startBackend, stopBackend } from './backend.js';
import { servePlannerFixtures } from './forecast-fixtures.js';

// Every Plan Settings control on the Planner page, against the real
// backend's /api/settings, in Chromium and Firefox (playwright.config.ts).
//
// Issue #391: the number fields saved only on blur, so a value changed
// without the field losing focus never reached the backend and the page
// showed the old value on return. Firefox's spin arrows change the value
// without focusing the field at all, so there every arrow-button edit was
// lost. Most tests here therefore never blur the field: leaving the page is
// a hash change, the way the back button or a keyboard shortcut leaves it.

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
  forecast_plan_strategy: 'min_soc',
  forecast_plan_auto_apply_enabled: false,
  forecast_plan_auto_apply_lead_minutes: 30,
  forecast_plan_auto_refresh: false,
};

async function savedSettings(request: APIRequestContext): Promise<Record<string, unknown>> {
  const res = await request.get('/api/settings');
  return ((await res.json()) as { data: Record<string, unknown> }).data;
}

const saved = (request: APIRequestContext, key: string) =>
  expect.poll(async () => (await savedSettings(request))[key]);

async function openPlanSettings(page: Page) {
  await servePlannerFixtures(page);
  await page.goto('/#/forecast');
  await page.getByText('Plan Settings').click();
  const minSoc = page.getByTestId('forecast-min-soc-input');
  await expect(minSoc).toBeEnabled();
  return minSoc;
}

// Leave the Planner without moving focus, the way the browser's back button
// or a keyboard shortcut does.
async function leaveWithoutBlur(page: Page) {
  await page.evaluate(() => {
    window.location.hash = '#/status';
  });
  await expect(page.getByTestId('forecast-min-soc-input')).toHaveCount(0);
}

async function returnToPlanSettings(page: Page) {
  await page.getByRole('link', { name: /Planner/ }).first().click();
  await page.getByText('Plan Settings').click();
}

// Click a number field's up arrow with the mouse. The arrows sit inside the
// field's 12px side padding, in the top half of its content box.
async function clickUpArrow(page: Page, field: Locator, times: number) {
  const box = await field.boundingBox();
  if (!box) throw new Error('number field has no bounding box');
  for (let i = 0; i < times; i++) {
    await page.mouse.click(box.x + box.width - 18, box.y + 12);
  }
}

test.describe('Planner settings (issue #391)', () => {
  test.beforeEach(async ({ request }) => {
    await request.post('/api/settings', { data: DEFAULTS });
  });
  test.afterEach(async ({ request }) => {
    await request.post('/api/settings', { data: DEFAULTS });
  });

  test.describe('Minimum Battery Level', () => {
    test('survives leaving the page without a blur', async ({ page, request }) => {
      const minSoc = await openPlanSettings(page);
      await expect(minSoc).toHaveValue('20');
      await minSoc.fill('35');
      await leaveWithoutBlur(page);

      await saved(request, 'forecast_min_soc_pct').toBe(35);
      await returnToPlanSettings(page);
      await expect(page.getByTestId('forecast-min-soc-input')).toHaveValue('35');
    });

    test('mouse clicks on the spin arrows save, then a nav click away keeps them', async ({
      page,
      request,
    }) => {
      // The reporter's exact flow, which lost the edit in Firefox.
      const minSoc = await openPlanSettings(page);
      await expect(minSoc).toHaveValue('20');
      await clickUpArrow(page, minSoc, 3);
      await expect(minSoc).toHaveValue('23');
      await page.getByRole('link', { name: /Status/ }).first().click();
      await expect(minSoc).toHaveCount(0);

      await saved(request, 'forecast_min_soc_pct').toBe(23);
      await returnToPlanSettings(page);
      await expect(page.getByTestId('forecast-min-soc-input')).toHaveValue('23');
    });

    test('arrow-key changes save after a pause and survive a reload', async ({ page, request }) => {
      const minSoc = await openPlanSettings(page);
      await expect(minSoc).toHaveValue('20');
      await minSoc.focus();
      for (let i = 0; i < 3; i++) await minSoc.press('ArrowUp');
      await expect(minSoc).toHaveValue('23');
      await expect(minSoc).toBeFocused();

      await saved(request, 'forecast_min_soc_pct').toBe(23);
      await page.reload();
      await page.getByText('Plan Settings').click();
      await expect(page.getByTestId('forecast-min-soc-input')).toHaveValue('23');
    });

    test('Enter saves straight away and keeps the field focused', async ({ page, request }) => {
      const minSoc = await openPlanSettings(page);
      await minSoc.fill('45');
      await minSoc.press('Enter');
      await saved(request, 'forecast_min_soc_pct').toBe(45);
      await expect(minSoc).toBeFocused();
      await expect(minSoc).toBeEnabled();
    });

    test('a field cleared and abandoned keeps the saved value', async ({ page, request }) => {
      const minSoc = await openPlanSettings(page);
      await minSoc.fill('');
      // Well past the save delay: an empty field must never auto-save or
      // complain while the user is still typing.
      await page.waitForTimeout(1500);
      await expect(page.getByTestId('forecast-min-soc-error')).toHaveCount(0);
      await leaveWithoutBlur(page);

      await returnToPlanSettings(page);
      await expect(page.getByTestId('forecast-min-soc-input')).toHaveValue('20');
      expect((await savedSettings(request)).forecast_min_soc_pct).toBe(20);
    });
  });

  test.describe('Battery efficiencies', () => {
    test('Enter saves the charge efficiency', async ({ page, request }) => {
      await openPlanSettings(page);
      const charge = page.getByTestId('forecast-charge-eff-input');
      await expect(charge).toHaveValue('90');
      await charge.fill('85');
      await charge.press('Enter');

      await saved(request, 'forecast_charge_efficiency').toBe(0.85);
      await expect(charge).toBeFocused();
    });

    test('the discharge efficiency survives leaving the page without a blur', async ({
      page,
      request,
    }) => {
      await openPlanSettings(page);
      const discharge = page.getByTestId('forecast-discharge-eff-input');
      await expect(discharge).toHaveValue('95');
      await discharge.fill('92');
      await leaveWithoutBlur(page);

      await saved(request, 'forecast_discharge_efficiency').toBe(0.92);
      expect((await savedSettings(request)).forecast_charge_efficiency).toBe(0.9);
      await returnToPlanSettings(page);
      await expect(page.getByTestId('forecast-discharge-eff-input')).toHaveValue('92');
    });

    test('spin-arrow clicks on the charge efficiency save after a pause', async ({
      page,
      request,
    }) => {
      await openPlanSettings(page);
      const charge = page.getByTestId('forecast-charge-eff-input');
      await expect(charge).toHaveValue('90');
      await clickUpArrow(page, charge, 2);
      await expect(charge).toHaveValue('92');
      await saved(request, 'forecast_charge_efficiency').toBe(0.92);
    });
  });

  test.describe('Automatic planning', () => {
    test.beforeEach(async ({ request }) => {
      await request.post('/api/settings', {
        data: { forecast_plan_auto_apply_enabled: true },
      });
    });

    test('an edited lead time saves after a pause and keeps auto-apply on', async ({
      page,
      request,
    }) => {
      await openPlanSettings(page);
      const lead = page.getByTestId('forecast-auto-apply-lead');
      await expect(lead).toHaveValue('30');
      await lead.fill('45');

      await saved(request, 'forecast_plan_auto_apply_lead_minutes').toBe(45);
      expect((await savedSettings(request)).forecast_plan_auto_apply_enabled).toBe(true);
      await expect(lead).toBeFocused();
    });

    test('an edited lead time survives leaving the page without a blur', async ({
      page,
      request,
    }) => {
      await openPlanSettings(page);
      const lead = page.getByTestId('forecast-auto-apply-lead');
      await lead.fill('60');
      await leaveWithoutBlur(page);

      await saved(request, 'forecast_plan_auto_apply_lead_minutes').toBe(60);
      await returnToPlanSettings(page);
      await expect(page.getByTestId('forecast-auto-apply-lead')).toHaveValue('60');
    });

    test('unticking straight after editing the lead time turns auto-apply off', async ({
      page,
      request,
    }) => {
      // The lead edit must not switch the trigger back on behind the untick.
      await openPlanSettings(page);
      const toggle = page.getByTestId('forecast-auto-apply-toggle');
      await expect(toggle).toBeChecked();
      await page.getByTestId('forecast-auto-apply-lead').fill('50');
      await toggle.click();

      await expect(toggle).not.toBeChecked();
      await saved(request, 'forecast_plan_auto_apply_enabled').toBe(false);
      // Past the save delay: nothing may re-enable it afterwards.
      await page.waitForTimeout(1500);
      const after = await savedSettings(request);
      expect(after.forecast_plan_auto_apply_enabled).toBe(false);
      expect(after.forecast_plan_auto_apply_lead_minutes).toBe(50);
      await page.reload();
      await page.getByText('Plan Settings').click();
      await expect(page.getByTestId('forecast-auto-apply-toggle')).not.toBeChecked();
    });

    test('the checkbox setting survives a reload', async ({ page, request }) => {
      await openPlanSettings(page);
      const toggle = page.getByTestId('forecast-auto-apply-toggle');
      await expect(toggle).toBeChecked();
      await toggle.click();
      await saved(request, 'forecast_plan_auto_apply_enabled').toBe(false);
      await page.reload();
      await page.getByText('Plan Settings').click();
      await expect(page.getByTestId('forecast-auto-apply-toggle')).not.toBeChecked();
    });
  });

  test('the charging strategy survives leaving the page', async ({ page, request }) => {
    await openPlanSettings(page);
    const strategy = page.getByTestId('forecast-plan-strategy');
    await expect(strategy).toHaveValue('min_soc');
    await strategy.selectOption('cheapest');
    await saved(request, 'forecast_plan_strategy').toBe('cheapest');
    await leaveWithoutBlur(page);

    await returnToPlanSettings(page);
    await expect(page.getByTestId('forecast-plan-strategy')).toHaveValue('cheapest');
  });
});
