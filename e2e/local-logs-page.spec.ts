/**
 * E2E tests for the Developer Logs page.
 *
 * Tests log viewing, capture level control, filtering, and auto-scroll.
 * This page is only visible when developer mode is enabled.
 */

import { test, expect } from './local-fixture.js';

/** Enable developer mode via the Settings page. */
async function enableDeveloperMode(page: import('@playwright/test').Page) {
  await page.goto('/#/settings');
  await expect(page.locator('text=Developer Mode')).toBeVisible({ timeout: 10_000 });

  // Check if already enabled (Logs link visible)
  await page.goto('/');
  const logsVisible = await page.locator('nav >> text=Logs').isVisible().catch(() => false);
  if (!logsVisible) {
    await page.goto('/#/settings');
    // Toggle is a <div> (not <button>), target via cursor-pointer class
    const toggle = page.locator('section:has-text("Developer Mode") div.cursor-pointer').first();
    await toggle.click();
    await page.waitForTimeout(500);
  }
}

test.describe('Logs Page - Visibility', () => {
  test('should not be accessible without developer mode', async ({ page }) => {
    // Logs nav should not be visible by default
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 15_000 });
    await expect(page.locator('nav >> text=Logs')).toBeHidden();
  });

  test('should be accessible after enabling developer mode', async ({ page }) => {
    await enableDeveloperMode(page);

    // Navigate to logs page directly
    await page.goto('/#/logs');
    await expect(page.locator('text=Developer Logs')).toBeVisible({ timeout: 10_000 });
  });
});

test.describe('Logs Page - Controls', () => {
  test('should show capture level buttons', async ({ page }) => {
    await enableDeveloperMode(page);
    await page.goto('/#/logs');

    // Capture-level buttons (ERROR, WARN, INFO, DEBUG, TRACE) — but log
    // entries also contain level labels, so use button role + exact match
    // to disambiguate. There should be exactly one button per level.
    for (const level of ['ERROR', 'WARN', 'INFO', 'DEBUG', 'TRACE']) {
      await expect(
        page.getByRole('button', { name: level, exact: true })
      ).toHaveCount(1, { timeout: 10_000 });
    }
  });

  test('should show filter input', async ({ page }) => {
    await enableDeveloperMode(page);
    await page.goto('/#/logs');

    await expect(page.locator('input[placeholder="Filter logs\u2026"]')).toBeVisible({ timeout: 10_000 });
  });

  test('should show Refresh button', async ({ page }) => {
    await enableDeveloperMode(page);
    await page.goto('/#/logs');

    await expect(page.locator('text=Refresh')).toBeVisible({ timeout: 10_000 });
  });

  test('should show line count', async ({ page }) => {
    await enableDeveloperMode(page);
    await page.goto('/#/logs');

    // Should show "X/Y lines" format
    await expect(page.locator('text=/\\d+\\/\\d+ lines/')).toBeVisible({ timeout: 10_000 });
  });
});

test.describe('Logs Page - Capture Level', () => {
  test('should switch capture level via API', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/log-level`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ level: 'INFO' }),
    });
    expect(resp.ok).toBe(true);
  });

  test('should read current capture level', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/log-level`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.level).toBeDefined();
  });

  test('should change capture level to DEBUG', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/log-level`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ level: 'DEBUG' }),
    });
    expect(resp.ok).toBe(true);

    // Verify it stuck
    const check = await fetch(`${baseUrl}/api/log-level`);
    const data = await check.json();
    expect(data.level).toBe('DEBUG');
  });
});

test.describe('Logs Page - Log Content', () => {
  test('should fetch logs via API', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/logs`);
    const data = await resp.json();
    // Response may be { ok: true, logs: [...] } or { ok: true, lines: [...] }
    expect(data.ok).toBe(true);
    const logs = data.logs ?? data.lines ?? [];
    expect(Array.isArray(logs)).toBe(true);
  });

  test('should show empty state when no logs match filter', async ({ page }) => {
    await enableDeveloperMode(page);
    await page.goto('/#/logs');

    // Type a very specific filter that won't match anything
    const filterInput = page.locator('input[placeholder="Filter logs\u2026"]');
    await filterInput.fill('ZZZZZZ_NO_MATCH_ZZZZZZ');
    await page.waitForTimeout(500);

    // Should show no-match message
    await expect(page.locator('text=/No logs match/')).toBeVisible({ timeout: 5_000 });
  });
});
