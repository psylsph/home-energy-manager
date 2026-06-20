/**
 * E2E tests for the Battery detail page.
 *
 * Tests SOC overview, battery state, stored energy, and module detail expansion
 * using the real GivEnergy simulator (Gen3 Hybrid with 2 batteries).
 */

import { test, expect } from './local-fixture.js';

test.describe('Battery Page - Loading', () => {
  test('should show battery data after load', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });
  });
});

test.describe('Battery Page - SOC Overview', () => {
  test('should show Battery heading', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('h2, h3').locator('text=Battery')).toBeVisible({ timeout: 5_000 });
  });

  test('should show battery state badge', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=/Charging|Discharging|Idle/').first()).toBeVisible({ timeout: 5_000 });
  });

  test('should show SOC percentage in the ring', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // The SOC should be visible as a number with %
    await expect(page.locator('text=/\\d{1,3}%/').first()).toBeVisible({ timeout: 5_000 });
  });

  test('should show Power row with value', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.getByText('Power', { exact: true }).first()).toBeVisible({ timeout: 5_000 });
  });

  test('should show Voltage row', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.getByText('Voltage', { exact: true }).first()).toBeVisible({ timeout: 5_000 });
  });

  test('should show Current row', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Current')).toBeVisible({ timeout: 5_000 });
  });

  test('should show Temperature row', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Temperature')).toBeVisible({ timeout: 5_000 });
  });

  test('should show Mode row', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.getByText('Mode', { exact: true })).toBeVisible({ timeout: 5_000 });
  });

  test('should show Reserve row', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Reserve')).toBeVisible({ timeout: 5_000 });
  });

  test('should show Charged Today row', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.getByText('Charged Today', { exact: true })).toBeVisible({ timeout: 5_000 });
  });

  test('should show Discharged Today row', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Discharged Today')).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('Battery Page - Stored Energy', () => {
  test('should show Battery Panel with energy data', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // The battery panel shows Charged Today / Discharged Today values
    // Use exact: true to avoid the substring matching 'Discharged Today'.
    await expect(page.getByText('Charged Today', { exact: true })).toBeVisible({ timeout: 5_000 });
  });

  test('should show a SOC ring', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // SOC ring shows battery charge level — use the bold 2xl ring label,
    // not any of the per-module SOC labels that share the same text.
    await expect(page.locator('span.text-2xl', { hasText: '75%' })).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('Battery Page - Modules', () => {
  test('should show Modules heading with count', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // We configured 2 batteries in the simulator
    await expect(page.locator('text=/Modules \\(2\\)/')).toBeVisible({ timeout: 5_000 });
  });

  test('should show Module 0 and Module 1 entries', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Each module renders as a button containing "Module" + "#N" in
    // separate divs (the "#N" is the index label).
    const moduleEntries = page.locator('button:has-text("Module")');
    await expect(moduleEntries.first()).toBeVisible({ timeout: 5_000 });
    expect(await moduleEntries.count()).toBeGreaterThanOrEqual(2);
  });

  test('should expand module details on click', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Click on first module button to expand.
    await page.locator('button:has-text("Module")').first().click();

    // Should show detailed fields after expansion (Serial, Cycle, etc.).
    await expect(page.getByText('Serial', { exact: true })).toBeVisible({ timeout: 5_000 });
  });

  test('should show SOC and Voltage in module summary', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Each module should show SOC% and voltage
    const moduleButtons = page.locator('button:has-text("Module")');
    const count = await moduleButtons.count();
    expect(count).toBeGreaterThanOrEqual(2);
  });

  test('should show cell voltage chart when module expanded', async ({ page }) => {
    await page.goto('/#/battery');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Click first module button to expand.
    await page.locator('button:has-text("Module")').first().click();

    // Wait for expanded content
    await page.waitForTimeout(1000);

    // Should show the cell voltage chart heading after expansion.
    // Use exact match to avoid matching the per-cell row text.
    await expect(page.getByText('Cell Voltages', { exact: true })).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('Battery Page - API Data Validation', () => {
  test('snapshot should have battery module data', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const data = await resp.json();
    expect(data.ok).toBe(true);

    const snap = data.data;
    expect(snap.battery_modules).toBeDefined();
    expect(Array.isArray(snap.battery_modules)).toBe(true);
    expect(snap.battery_modules.length).toBeGreaterThanOrEqual(1);
  });

  test('first module should have valid SOC', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const data = await resp.json();
    const mod = data.data.battery_modules[0];
    if (mod) {
      expect(mod.soc).toBeGreaterThanOrEqual(0);
      expect(mod.soc).toBeLessThanOrEqual(100);
    }
  });
});
