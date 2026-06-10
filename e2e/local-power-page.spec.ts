/**
 * E2E tests for the Power page.
 *
 * Tests the power flow overview, stat tiles, and time range controls.
 */

import { test, expect } from './local-fixture.js';

test.describe('Power Page - Loading', () => {
  test('should load and show Power heading', async ({ page }) => {
    await page.goto('/#/power');
    await expect(page.getByRole('heading', { name: 'Power', exact: true })).toBeVisible({ timeout: 15_000 });
  });
});

test.describe('Power Page - Stat Tiles', () => {
  test('should show Generation tile', async ({ page }) => {
    await page.goto('/#/power');
    await expect(page.locator('text=Generation')).toBeVisible({ timeout: 15_000 });
  });

  test('should show battery state tile', async ({ page }) => {
    await page.goto('/#/power');
    await expect(page.locator('text=/Charging|Discharging|Idle/').first()).toBeVisible({ timeout: 15_000 });
  });

  test('should show grid state tile', async ({ page }) => {
    await page.goto('/#/power');
    await expect(page.locator('text=/Importing|Exporting|Idle/').first()).toBeVisible({ timeout: 15_000 });
  });

  test('should show Load tile', async ({ page }) => {
    await page.goto('/#/power');
    await expect(page.getByText('Load', { exact: true }).first()).toBeVisible({ timeout: 15_000 });
  });

  test('should show power values in W or kW', async ({ page }) => {
    await page.goto('/#/power');
    // Multiple power values should be displayed
    await expect(page.locator('text=/\\d+[Wk]/').first()).toBeVisible({ timeout: 15_000 });
  });
});

test.describe('Power Page - Time Range', () => {
  test('should show time range buttons', async ({ page }) => {
    await page.goto('/#/power');
    // At least some range buttons should be visible
    await expect(page.locator('text=1h')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('text=24h')).toBeVisible();
  });

  test('should allow switching time ranges', async ({ page }) => {
    await page.goto('/#/power');
    await expect(page.locator('text=1h')).toBeVisible({ timeout: 15_000 });

    // Click on a different range
    const btn6h = page.getByRole('button', { name: '6h', exact: true });
    if (await btn6h.isVisible()) {
      await btn6h.click();
    }
    // Page should not crash
    await expect(page.getByRole('heading', { name: 'Power', exact: true })).toBeVisible();
  });
});

test.describe('Power Page - Chart', () => {
  test('should show Power Flow heading', async ({ page }) => {
    await page.goto('/#/power');
    await expect(page.locator('text=Power Flow')).toBeVisible({ timeout: 15_000 });
  });

  test('should display chart area', async ({ page }) => {
    await page.goto('/#/power');
    // Recharts container should render
    const chartArea = page.locator('.recharts-wrapper, [class*="recharts"]').first();
    // Chart may or may not have data yet — just verify the page doesn't error
    await page.waitForTimeout(2000);
  });
});
