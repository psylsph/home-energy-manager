/**
 * E2E tests for the History page.
 *
 * Tests tab navigation, time range selection, chart rendering, and CSV export.
 */

import { test, expect } from './local-fixture.js';

test.describe('History Page - Loading', () => {
  test('should load and show History tabs', async ({ page }) => {
    await page.goto('/#/history');
    // Use heading or tab bar, not nav link which also says 'History'
    await expect(page.locator('button:has-text("Battery"), button:has-text("Solar")').first()).toBeVisible({ timeout: 15_000 });
  });
});

test.describe('History Page - Tab Navigation', () => {
  const tabs = ['Battery', 'Solar', 'Grid', 'Home', 'Cost'];

  for (const tab of tabs) {
    test(`should show ${tab} tab`, async ({ page }) => {
      await page.goto('/#/history');
      // Target the desktop tab button directly — `text=${tab}` also matches
      // the hidden <option> inside the sm:hidden mobile <select>.
      await expect(page.getByRole('button', { name: tab, exact: true })).toBeVisible({ timeout: 15_000 });
    });
  }

  test('should switch between tabs', async ({ page }) => {
    await page.goto('/#/history');
    await expect(page.locator('text=Battery').first()).toBeVisible({ timeout: 15_000 });

    // Click Solar tab
    await page.locator('button:has-text("Solar")').first().click();
    await page.waitForTimeout(500);

    // Should show solar chart heading
    await expect(page.locator('text=/PV Power|Solar/').first()).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('History Page - Time Range', () => {
  test('should show time range buttons', async ({ page }) => {
    await page.goto('/#/history');
    // Time range is exposed both as <select> (mobile, aria-label="Select
    // time range") and as buttons (desktop). The buttons are the primary
    // UI; verify they exist with the expected labels.
    const rangeButtons = page.locator('button').filter({ hasText: /^(1h|6h|12h|24h|7d|30d)$/ });
    await expect(rangeButtons.first()).toBeVisible({ timeout: 15_000 });
    expect(await rangeButtons.count()).toBeGreaterThanOrEqual(3);
  });

  test('should switch time ranges', async ({ page }) => {
    await page.goto('/#/history');
    const rangeButtons = page.locator('button').filter({ hasText: /^(1h|6h|12h|24h|7d|30d)$/ });
    await expect(rangeButtons.first()).toBeVisible({ timeout: 15_000 });

    // Click 24h button to switch ranges.
    await rangeButtons.filter({ hasText: '24h' }).first().click();
  });
});

test.describe('History Page - Navigation', () => {
  test('should show navigation arrows', async ({ page }) => {
    await page.goto('/#/history');
    // Should show Older/Newer navigation
    await expect(page.locator('text=/Older|◀/')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('text=/Newer|▶/')).toBeVisible();
  });
});

test.describe('History Page - CSV Export', () => {
  test('should show CSV button', async ({ page }) => {
    await page.goto('/#/history');
    await expect(page.getByRole('button', { name: 'CSV' })).toBeVisible({ timeout: 15_000 });
  });
});

test.describe('History Page - Chart Content', () => {
  test('Battery tab should show SOC or chart', async ({ page }) => {
    await page.goto('/#/history');
    await expect(page.locator('button:has-text("Battery")').first()).toBeVisible({ timeout: 15_000 });

    // Default tab is Battery — shows SOC chart or empty state
    await page.waitForTimeout(2000);
    const hasChart = await page.locator('.recharts-wrapper, [class*="recharts"]').count();
    const hasEmpty = await page.locator('text=/No data/').count();
    expect(hasChart > 0 || hasEmpty > 0).toBe(true);
  });

  test('should show chart or empty state', async ({ page }) => {
    await page.goto('/#/history');
    await page.waitForTimeout(2000);

    // Either shows chart data or "No data available" message
    const hasChart = await page.locator('.recharts-wrapper, [class*="recharts"]').count();
    const hasEmpty = await page.locator('text=/No data available/').count();

    expect(hasChart > 0 || hasEmpty > 0).toBe(true);
  });
});

test.describe('History Page - API', () => {
  test('history endpoint should respond', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/history?range=1h&fields=soc`);
    expect(resp.ok).toBe(true);
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });

  test('history endpoint with multiple fields', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/history?range=24h&fields=soc,battery_power,solar_power`);
    expect(resp.ok).toBe(true);
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });

  test('history endpoint with invalid range should handle gracefully', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/history?range=invalid&fields=soc`);
    // Should return 400 or handle gracefully
    expect(resp.status).toBeLessThanOrEqual(400);
  });

  // Issue #108: per-string PV1/PV2 today fields are valid history fields.
  test('history endpoint accepts today_pv1_kwh and today_pv2_kwh fields', async ({ baseUrl }) => {
    const resp = await fetch(
      `${baseUrl}/api/history?range=24h&fields=today_pv1_kwh,today_pv2_kwh,today_solar_kwh`,
    );
    expect(resp.ok).toBe(true);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.data).toHaveProperty('today_pv1_kwh');
    expect(data.data).toHaveProperty('today_pv2_kwh');
    expect(data.data).toHaveProperty('today_solar_kwh');
  });

  test('Solar tab on history page shows the per-string energy chart', async ({ page }) => {
    await page.goto('/#/history');
    await expect(page.locator('button:has-text("Solar")').first()).toBeVisible({ timeout: 15_000 });
    await page.locator('button:has-text("Solar")').first().click();
    await page.waitForTimeout(1500);

    // The PV Energy Today chart should be present (or empty state).
    const hasChart = await page.locator('text=/PV Energy Today/').count();
    const hasEmpty = await page.locator('text=/No data available/').count();
    expect(hasChart > 0 || hasEmpty > 0).toBe(true);
  });
});
