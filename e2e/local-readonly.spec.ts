/**
 * E2E tests for the ?RO (read-only) URL parameter (issue #114).
 *
 * The param hides the Control and Settings nav links so a household-shared
 * dashboard link can't be used to accidentally change settings. Once the
 * param is visited, the read-only flag is pinned in localStorage so the
 * link is sticky in that browser across reloads.
 *
 * Uses the real GivEnergy simulator via the headless backend.
 */

import { test, expect } from './local-fixture.js';

test.describe('Read-only mode (?RO URL param, issue #114)', () => {
  test('shows Control and Settings nav links on the default URL', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Home Energy Manager')).toBeVisible({ timeout: 10_000 });

    const nav = page.locator('nav');
    await expect(nav.getByText('Control', { exact: true })).toBeVisible();
    await expect(nav.getByText('Settings', { exact: true })).toBeVisible();
  });

  test('hides Control and Settings nav links when ?RO is in the URL', async ({ page }) => {
    await page.goto('/?RO');
    await expect(page.locator('text=Home Energy Manager')).toBeVisible({ timeout: 10_000 });

    // The two protected tabs disappear from the bottom bar.
    const nav = page.locator('nav');
    await expect(nav.getByText('Control', { exact: true })).toBeHidden();
    await expect(nav.getByText('Settings', { exact: true })).toBeHidden();

    // Other nav links are still present.
    await expect(nav.getByText('Status', { exact: true })).toBeVisible();
    await expect(nav.getByText('Battery', { exact: true })).toBeVisible();
  });

  test('persists read-only mode in localStorage after the first ?RO visit', async ({ page }) => {
    await page.goto('/?RO');
    await expect(page.locator('text=Home Energy Manager')).toBeVisible({ timeout: 10_000 });

    // The flag is pinned, so even navigating away from ?RO keeps RO mode.
    const stored = await page.evaluate(() => localStorage.getItem('readOnly'));
    expect(stored).toBe('true');
  });

  test('sticks to read-only mode in subsequent visits (no ?RO needed)', async ({ page }) => {
    // First visit with the param — the flag is persisted.
    await page.goto('/?RO');
    await expect(page.locator('text=Home Energy Manager')).toBeVisible({ timeout: 10_000 });
    const nav = page.locator('nav');
    await expect(nav.getByText('Control', { exact: true })).toBeHidden();

    // Now navigate to a different route (no ?RO) — the flag is sticky.
    await page.goto('/#/battery');
    await expect(nav.getByText('Control', { exact: true })).toBeHidden();
    await expect(nav.getByText('Settings', { exact: true })).toBeHidden();

    // And on full reload, the same: flag survives.
    await page.reload();
    await expect(page.locator('text=Home Energy Manager')).toBeVisible({ timeout: 10_000 });
    await expect(nav.getByText('Control', { exact: true })).toBeHidden();
    await expect(nav.getByText('Settings', { exact: true })).toBeHidden();
  });
});
