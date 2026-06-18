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
      await expect(page.locator(`text=${tab}`).first()).toBeVisible({ timeout: 15_000 });
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
    await expect(page.locator('button:has-text("1h")').first()).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('text=24h').first()).toBeVisible();
    await expect(page.locator('text=7d').first()).toBeVisible();
  });

  test('should switch time ranges', async ({ page }) => {
    await page.goto('/#/history');
    await expect(page.locator('button:has-text("1h")').first()).toBeVisible({ timeout: 15_000 });

    await page.locator('button:has-text("24h")').click();
    await page.waitForTimeout(500);
    // Should not crash
    await expect(page.locator('text=1h')).toBeVisible();
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
});
