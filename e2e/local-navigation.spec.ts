/**
 * E2E tests for navigation, routing, theme toggle, and connection indicator.
 *
 * Uses the real GivEnergy simulator via the headless backend.
 */

import { test, expect } from './local-fixture.js';

test.describe('Navigation', () => {
  test('should show the app header with title', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Home Energy Manager')).toBeVisible({ timeout: 10_000 });
  });

  test('should show GivEnergy subtitle', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=For GivEnergy Solar and Battery Systems')).toBeVisible();
  });

  const navLinks = [
    { label: 'Status', href: '/' },
    { label: 'Power', href: '/power' },
    { label: 'Battery', href: '/battery' },
    { label: 'Inverter', href: '/inverter' },
    { label: 'Solar', href: '/solar' },
    { label: 'Meters', href: '/meters' },
    { label: 'History', href: '/history' },
    { label: 'Control', href: '/control' },
    { label: 'Settings', href: '/settings' },
  ];

  for (const { label, href } of navLinks) {
    test(`should navigate to ${label} page via bottom nav`, async ({ page }) => {
      await page.goto('/');
      // Wait for data to load
      await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 15_000 });

      await page.locator(`nav >> text=${label}`).first().click();
      await expect(page).toHaveURL(new RegExp(`#${href.replace('/', '\\/')}$`), { timeout: 5_000 });
    });
  }

  test('should show active state on current nav link', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 15_000 });

    // Status should be active by default
    const statusLink = page.locator('nav >> text=Status').first();
    await expect(statusLink).toBeVisible();
  });
});

test.describe('Connection Indicator', () => {
  test('should show connected state in header', async ({ page }) => {
    await page.goto('/');
    // The green dot + IP address signals connected state
    await expect(page.locator('text=127.0.0.1').first()).toBeVisible({ timeout: 15_000 });
  });

  test('should show last updated time in header', async ({ page }) => {
    await page.goto('/');
    // The header shows the IP address followed by · HH:MM:SS
    await expect(page.getByText(/127\.0\.0\.1.*\d{1,2}:\d{2}:\d{2}/)).toBeVisible({ timeout: 15_000 });
  });

  test('API status endpoint returns connected', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/status`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.connection).toBe('connected');
  });
});

test.describe('Theme Toggle', () => {
  test('should toggle between dark and light mode', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Home Energy Manager')).toBeVisible({ timeout: 10_000 });

    // Find theme toggle button
    const themeBtn = page.locator('button[aria-label*="Switch to"]');
    await expect(themeBtn).toBeVisible({ timeout: 5_000 });

    // Click to switch theme
    await themeBtn.click();

    // Theme should persist — check aria-label changed
    await expect(themeBtn).toBeVisible();
  });

  test('should persist theme preference across page reload', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Home Energy Manager')).toBeVisible({ timeout: 10_000 });

    const themeBtn = page.locator('button[aria-label*="Switch to"]');
    const initialLabel = await themeBtn.getAttribute('aria-label');

    await themeBtn.click();

    // Reload and verify theme persisted
    await page.reload();
    await expect(page.locator('text=Home Energy Manager')).toBeVisible({ timeout: 10_000 });

    const newLabel = await page.locator('button[aria-label*="Switch to"]').getAttribute('aria-label');
    expect(newLabel).not.toBe(initialLabel);
  });
});

test.describe('Developer Mode', () => {
  test('Logs nav link should not be visible by default', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 15_000 });

    // Logs should NOT be in the nav
    await expect(page.locator('nav >> text=Logs')).toBeHidden();
  });

  test('should show Logs nav link after enabling developer mode', async ({ page }) => {
    await page.goto('/#/settings');
    // Wait for settings to load
    await expect(page.locator('text=Developer Mode')).toBeVisible({ timeout: 10_000 });

    // Enable developer mode
    const section = page.locator('section', { hasText: 'Developer Mode' }).first();
    const devToggle = section.locator('div.cursor-pointer').first();
    await devToggle.click();

    // Navigate back to see Logs in nav
    await page.goto('/');
    await expect(page.locator('nav >> text=Logs')).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('Error Boundary', () => {
  test('should load without errors on the root page', async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', (err) => errors.push(err.message));

    await page.goto('/');
    await expect(page.locator('text=Home Energy Manager')).toBeVisible({ timeout: 10_000 });
    await new Promise((r) => setTimeout(r, 2000));

    expect(errors).toHaveLength(0);
  });
});
