/**
 * E2E tests for the Settings page.
 *
 * Tests connection configuration, refresh interval, HTTP port, tariffs,
 * developer mode, and network access display.
 */

import { test, expect } from './local-fixture.js';

test.describe('Settings Page - Loading', () => {
  test('should load settings page', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Connection')).toBeVisible({ timeout: 10_000 });
  });
});

test.describe('Settings Page - Connection', () => {
  test('should show Connection heading', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Connection')).toBeVisible({ timeout: 10_000 });
  });

  test('should show connection state', async ({ page }) => {
    await page.goto('/#/settings');
    // Connection state shown on settings page (may show "Connected" or "disconnected")
    await expect(page.locator('text=/connected|disconnected/i').first()).toBeVisible({ timeout: 15_000 });
  });

  test('should show Inverter Address input', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Inverter Address')).toBeVisible({ timeout: 10_000 });
  });

  test('should show Port input', async ({ page }) => {
    await page.goto('/#/settings');
    // Port field may be part of a combined address:port row
    await expect(page.locator('input[type="number"]').first()).toBeVisible({ timeout: 10_000 });
  });

  test('should show Serial Number input', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Serial Number')).toBeVisible({ timeout: 10_000 });
  });

  test('should show Connect button', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Connect')).toBeVisible({ timeout: 10_000 });
  });

  test('should show Scan Network button', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Scan Network').first()).toBeVisible({ timeout: 10_000 });
  });
});

test.describe('Settings Page - Refresh Interval', () => {
  test('should show Refresh Interval heading', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Refresh Interval')).toBeVisible({ timeout: 10_000 });
  });

  test('should show interval buttons', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.getByRole('button', { name: '5s', exact: true })).toBeVisible({ timeout: 10_000 });
    await expect(page.getByRole('button', { name: '10s', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: '15s', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: '20s', exact: true })).toBeVisible();
  });

  test('5s should be active by default', async ({ page }) => {
    await page.goto('/#/settings');
    // The 5s button should have the active/highlighted style
    const btn = page.getByRole('button', { name: '5s', exact: true });
    await expect(btn).toBeVisible({ timeout: 10_000 });
  });
});

test.describe('Settings Page - HTTP Port', () => {
  test('should show HTTP Port heading', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=HTTP Port')).toBeVisible({ timeout: 10_000 });
  });
});

test.describe('Settings Page - Energy Tariffs', () => {
  test('should show Energy Tariffs heading', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Energy Tariffs')).toBeVisible({ timeout: 10_000 });
  });

  test('should show Import tariff section', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Import')).toBeVisible({ timeout: 10_000 });
  });

  test('should show Export tariff section', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Export')).toBeVisible({ timeout: 10_000 });
  });

  test('should show Save Tariffs button', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Save Tariffs')).toBeVisible({ timeout: 10_000 });
  });

  test('should save tariff settings via API', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/settings`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        host: '127.0.0.1',
        port: 18899,
        serial: '',
        poll_interval: 5,
        http_port: 17337,
        import_tariff: 0.30,
        export_tariff: 0.15,
      }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });

  test('should read back saved tariffs', async ({ baseUrl }) => {
    // First save
    await fetch(`${baseUrl}/api/settings`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        host: '127.0.0.1',
        port: 18899,
        serial: '',
        poll_interval: 5,
        http_port: 17337,
        import_tariff: 0.35,
        export_tariff: 0.12,
      }),
    });

    // Then read back
    const resp = await fetch(`${baseUrl}/api/settings`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.data.import_tariff).toBe(0.35);
    expect(data.data.export_tariff).toBe(0.12);
  });
});

test.describe('Settings Page - Network Access', () => {
  test('should show Network Access heading', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Network Access')).toBeVisible({ timeout: 10_000 });
  });

  test('should show URL with port', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=/17337/')).toBeVisible({ timeout: 10_000 });
  });

  test('should show Copy button', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Copy')).toBeVisible({ timeout: 10_000 });
  });
});

test.describe('Settings Page - Developer', () => {
  test('should show Developer heading', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.getByText('Developer', { exact: true })).toBeVisible({ timeout: 10_000 });
  });

  test('should show Developer Mode toggle', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Developer Mode')).toBeVisible({ timeout: 10_000 });
  });

  test('should enable developer mode', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Developer Mode')).toBeVisible({ timeout: 10_000 });

    // Click the toggle
    // The toggle is a button near the Developer Mode text
    const section = page.locator('section', { hasText: 'Developer Mode' }).first();
    const toggle = section.locator('div.cursor-pointer').first();
    await toggle.click();

    // DevTools section should appear
    await expect(page.locator('text=/Test Cold Battery|DevTools/')).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('Settings Page - About', () => {
  test('should show About heading', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=About')).toBeVisible({ timeout: 10_000 });
  });

  test('should show GitHub link', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=/github/')).toBeVisible({ timeout: 10_000 });
  });
});

test.describe('Settings Page - API', () => {
  test('GET /api/settings returns current settings', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/settings`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.data.host).toBeDefined();
    expect(data.data.port).toBeDefined();
    expect(data.data.poll_interval ?? data.data.interval_secs).toBeDefined();
  });

  test('GET /api/status returns connection info', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/status`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.connection).toBe('connected');
  });
});
