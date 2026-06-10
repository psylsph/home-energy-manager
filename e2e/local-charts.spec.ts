/**
 * E2E tests for interactive chart legends and shared range selection.
 *
 * Uses the real GivEnergy simulator — exercises the full stack from Modbus
 * protocol to UI rendering. These tests validate:
 *   - Legend toggle buttons render with correct aria attributes
 *   - Range selection persists between Power → History pages
 *   - Range selection survives a page reload
 *   - History page multi-series charts show legend items
 */

import { test, expect } from './local-fixture.js';

test.describe('Charts - Legend Toggles (Simulator)', () => {
  test('Power page legend items are rendered as toggle buttons', async ({ page }) => {
    await page.goto('/#/power');
    await expect(page.getByRole('heading', { name: 'Power Flow' })).toBeVisible({ timeout: 15_000 });

    // Verify legend buttons exist for all expected series
    const legendBtns = page.locator('button[title^="Mute "], button[title^="Show "]');
    await expect(legendBtns.first()).toBeVisible({ timeout: 5_000 });
    const count = await legendBtns.count();
    expect(count).toBeGreaterThanOrEqual(4);

    // Each legend button should have a meaningful title
    const firstTitle = await legendBtns.first().getAttribute('title');
    expect(firstTitle).toMatch(/^(Mute|Show) /);
  });

  test('Power page legend toggle toggles aria-pressed and title', async ({ page }) => {
    await page.goto('/#/power');
    await expect(page.getByRole('heading', { name: 'Power Flow' })).toBeVisible({ timeout: 15_000 });

    const btn = page.getByRole('button', { name: 'Battery SOC' });
    await expect(btn).toBeVisible();

    // Initially pressed (not muted)
    await expect(btn).toHaveAttribute('aria-pressed', 'true');
    const muteTitle = await btn.getAttribute('title');
    expect(muteTitle).toMatch(/^Mute /);

    // Click to mute
    await btn.click();
    await expect(btn).toHaveAttribute('aria-pressed', 'false');
    const showTitle = await btn.getAttribute('title');
    expect(showTitle).toBe(muteTitle?.replace('Mute ', 'Show '));

    // Click again to restore
    await btn.click();
    await expect(btn).toHaveAttribute('aria-pressed', 'true');
    await expect(btn).toHaveAttribute('title', muteTitle ?? '');
  });

  test('History page multi-series chart legend items are toggleable', async ({ page }) => {
    await page.goto('/#/history');
    await expect(page.getByRole('heading', { name: 'Charge / Discharge Power' })).toBeVisible({ timeout: 15_000 });

    // Find a legend button inside a multi-series chart
    const chargeBtn = page.getByRole('button', { name: 'Charge' }).first();
    await expect(chargeBtn).toBeVisible({ timeout: 5_000 });

    await expect(chargeBtn).toHaveAttribute('aria-pressed', 'true');
    const muteTitle = await chargeBtn.getAttribute('title');
    expect(muteTitle).toMatch(/^Mute /);

    await chargeBtn.click();
    await expect(chargeBtn).toHaveAttribute('aria-pressed', 'false');
  });
});

test.describe('Charts - Shared Range (Simulator)', () => {
  test('switching range on Power page persists to History page', async ({ page }) => {
    await page.goto('/#/power');
    await expect(page.getByRole('heading', { name: 'Power Flow' })).toBeVisible({ timeout: 15_000 });

    // Click 6h range
    await page.getByRole('button', { name: '6h', exact: true }).click();
    await expect(page.getByRole('button', { name: '6h', exact: true })).toHaveAttribute('aria-pressed', 'true');

    // Navigate to History — range should still be 6h
    await page.goto('/#/history');
    await expect(page.getByRole('button', { name: '6h', exact: true })).toHaveAttribute('aria-pressed', 'true');

    // Switch to 12h on History
    await page.getByRole('button', { name: '12h', exact: true }).click();
    await expect(page.getByRole('button', { name: '12h', exact: true })).toHaveAttribute('aria-pressed', 'true');

    // Back to Power — should still be 12h
    await page.goto('/#/power');
    await expect(page.getByRole('heading', { name: 'Power Flow' })).toBeVisible({ timeout: 15_000 });
    await expect(page.getByRole('button', { name: '12h', exact: true })).toHaveAttribute('aria-pressed', 'true');
  });

  test('selected range survives a page reload', async ({ page }) => {
    await page.goto('/#/power');
    await expect(page.getByRole('heading', { name: 'Power Flow' })).toBeVisible({ timeout: 15_000 });

    // Clear any cached range and pick one
    await page.evaluate(() => localStorage.removeItem('chartRange'));
    await page.reload();
    await expect(page.getByRole('heading', { name: 'Power Flow' })).toBeVisible({ timeout: 15_000 });

    // Pick a non-default range
    await page.getByRole('button', { name: '6h', exact: true }).click();
    await expect(page.getByRole('button', { name: '6h', exact: true })).toHaveAttribute('aria-pressed', 'true');

    // Reload
    await page.reload();
    await expect(page.getByRole('heading', { name: 'Power Flow' })).toBeVisible({ timeout: 15_000 });

    // Range should survive
    await expect(page.getByRole('button', { name: '6h', exact: true })).toHaveAttribute('aria-pressed', 'true');
  });
});

test.describe('PWA Manifest', () => {
  test('manifest.json is served and is valid JSON', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/manifest.json`);
    expect(resp.ok).toBe(true);
    expect(resp.headers.get('content-type')).toMatch(/json/);

    const manifest = await resp.json();
    expect(manifest.name).toBe('Home Energy Manager');
    expect(manifest.short_name).toBe('Energy Manager');
    expect(manifest.display).toBe('standalone');
    expect(manifest.theme_color).toBe('#0D1117');
  });

  test('manifest.json declares Android icon sizes', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/manifest.json`);
    const manifest = await resp.json();

    expect(Array.isArray(manifest.icons)).toBe(true);
    const sizes = manifest.icons.map((i: { sizes: string }) => i.sizes);
    expect(sizes).toContain('192x192');
    expect(sizes).toContain('512x512');

    // Verify the icon files are actually served
    for (const icon of manifest.icons) {
      const iconResp = await fetch(`${baseUrl}${icon.src}`);
      expect(iconResp.ok).toBe(true);
      expect(iconResp.headers.get('content-type')).toMatch(/png/);
    }
  });

  test('index.html links to manifest', async ({ page }) => {
    await page.goto('/');
    const manifestLink = page.locator('link[rel="manifest"]');
    await expect(manifestLink).toHaveAttribute('href', '/manifest.json');
  });

  test('dist contains apple touch icons', async ({ baseUrl }) => {
    const resp152 = await fetch(`${baseUrl}/apple-touch-icon-152x152.png`);
    expect(resp152.ok).toBe(true);

    const resp180 = await fetch(`${baseUrl}/apple-touch-icon-180x180.png`);
    expect(resp180.ok).toBe(true);
  });
});
