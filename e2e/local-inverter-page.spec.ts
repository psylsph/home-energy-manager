/**
 * E2E tests for the Inverter device info page.
 *
 * Tests device info sections, solar inputs, grid data, and battery details.
 */

import { test, expect } from './local-fixture.js';

test.describe('Inverter Page - Loading', () => {
  test('should load and show inverter data', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });
  });
});

test.describe('Inverter Page - Device Info', () => {
  test('should show Device Info section', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Device Info')).toBeVisible({ timeout: 5_000 });
  });

  test('should show Inverter Type', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Inverter Type')).toBeVisible({ timeout: 5_000 });
  });

  test('should show Device Type Code', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Device Type Code')).toBeVisible({ timeout: 5_000 });
  });

  test('should show Serial Number', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Serial Number')).toBeVisible({ timeout: 5_000 });
  });

  test('should show firmware versions', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=ARM Firmware')).toBeVisible({ timeout: 5_000 });
  });

  test('should show Max Battery Power', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Max Battery Power')).toBeVisible({ timeout: 5_000 });
  });

  test('should show Max AC Output', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=/Max AC|Rated/')).toBeVisible({ timeout: 5_000 });
  });

  test('should show Battery Capacity', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Battery Capacity')).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('Inverter Page - Solar Inputs', () => {
  test('should show Solar Inputs section', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Solar Inputs')).toBeVisible({ timeout: 5_000 });
  });

  test('should show PV1 power', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=PV1 Power')).toBeVisible({ timeout: 5_000 });
  });

  test('should show PV1 voltage and current', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=PV1 Voltage')).toBeVisible({ timeout: 5_000 });
    await expect(page.locator('text=PV1 Current')).toBeVisible();
  });

  test('should show Solar Today', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Solar Today')).toBeVisible({ timeout: 5_000 });
  });

  // Issue #108: per-string PV1/PV2 Today rows in the Solar Inputs section.
  test('should show PV1 Today row', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=PV1 Today')).toBeVisible({ timeout: 5_000 });
  });

  test('snapshot exposes per-string PV today fields (issue #108)', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.data).toHaveProperty('today_pv1_kwh');
    expect(data.data).toHaveProperty('today_pv2_kwh');
  });
});

test.describe('Inverter Page - Grid Section', () => {
  test('should show Grid section', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.getByRole('heading', { name: 'Grid', exact: true })).toBeVisible({ timeout: 5_000 });
  });

  test('should show Grid Power', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Grid Power')).toBeVisible({ timeout: 5_000 });
  });

  test('should show Grid Voltage', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Grid Voltage')).toBeVisible({ timeout: 5_000 });
  });

  test('should show Grid Frequency', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Grid Frequency')).toBeVisible({ timeout: 5_000 });
  });

  test('should show Import/Export Today', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Import Today')).toBeVisible({ timeout: 5_000 });
    await expect(page.locator('text=Export Today')).toBeVisible();
  });
});

test.describe('Inverter Page - Battery Section', () => {
  test('should show Battery section', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.getByRole('heading', { name: 'Battery', exact: true })).toBeVisible({ timeout: 5_000 });
  });

  test('should show SOC and power', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.getByText('SOC', { exact: true })).toBeVisible({ timeout: 5_000 });
    await expect(page.locator('text=Battery Power').first()).toBeVisible();
  });

  test('should show Enable Charge/Discharge status', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.getByText('Enable Charge', { exact: true })).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText('Enable Discharge', { exact: true })).toBeVisible();
  });
});

test.describe('Inverter Page - Features & Status', () => {
  test('should show Features & Status section', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.getByRole('heading', { name: 'Features & Status' })).toBeVisible({ timeout: 5_000 });
  });

  test('should show Auto Winter status', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=/Auto Winter/')).toBeVisible({ timeout: 5_000 });
  });

  test('should show Cosy Mode status', async ({ page }) => {
    await page.goto('/#/inverter');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=/Cosy Mode/')).toBeVisible({ timeout: 5_000 });
  });
});
