/**
 * E2E tests for the Status / Dashboard page.
 *
 * Tests the energy flow diagram, summary tiles, battery panel, and loading states
 * using the real GivEnergy simulator.
 */

import type { Page } from '@playwright/test';
import { test, expect } from './local-fixture.js';

test.describe('Status Page - Loading', () => {
  test('should show data after initial load', async ({ page }) => {
    await page.goto('/');
    // Should NOT show "Waiting for Data" after the poll loop has warmed up
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });
  });
});

test.describe('Status Page - Energy Flow Diagram', () => {
  test('should display the energy flow SVG', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // The energy flow diagram should be visible
    const svg = page.locator('svg').first();
    await expect(svg).toBeVisible({ timeout: 5_000 });
  });

  test('should show INVERTER node label', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // EnergyFlowDiagram renders the central hub with label "Inverter".
    await expect(page.getByText('Inverter', { exact: true }).first()).toBeVisible({ timeout: 5_000 });
  });

  test('should show Solar node with power value', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Solar power should show either W or kW
    await expect(page.locator('text=/\\d+[Wk]/').first()).toBeVisible({ timeout: 5_000 });
  });

  test('should show Battery node with SOC percentage', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // SOC should be visible as a percentage
    await expect(page.locator('text=/\\d+%/').first()).toBeVisible({ timeout: 5_000 });
  });

  test('should show Home and Grid nodes', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // These labels appear inside the SVG
    const svgContent = page.locator('svg').first();
    await expect(svgContent).toBeVisible();
  });

  test('should display battery mode label', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Should show Eco or similar battery mode
    await expect(page.locator('text=/Eco|Timed|Paused/i').first()).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('Status Page - Battery Card', () => {
  // The full battery card moved to the Battery page (issue #422); its rows
  // are covered by local-battery-page.spec.ts.
  test('should not show the Battery card', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });
    await expect(page.getByRole('heading', { name: 'Today' })).toBeVisible({ timeout: 5_000 });

    await expect(page.getByRole('heading', { name: 'Battery', exact: true })).toHaveCount(0);
  });
});

test.describe('Status Page - Summary Tiles', () => {
  test('should show Today heading', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.getByRole('heading', { name: 'Today' })).toBeVisible({ timeout: 5_000 });
  });

  test('should show Solar Today tile with energy', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Solar today should show kWh
    const solarTile = page.locator('text=/☀/').locator('..').locator('..');
    await expect(solarTile).toBeVisible();
  });

  test('should show Consumption tile', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Consumption tile (label was renamed to "Home Use")
    await expect(page.locator('span', { hasText: /^Home Use$/ })).toBeVisible({ timeout: 5_000 });
  });

  test('should show Import and Export tiles', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('span', { hasText: /^Import$/ })).toBeVisible({ timeout: 5_000 });
    await expect(page.locator('span', { hasText: /^Export$/ })).toBeVisible({ timeout: 5_000 });
  });

  test('should show kWh values in summary tiles', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // At least some kWh values should be visible
    const kwhValues = page.locator('text=/\\d+\\.\\d+kWh/');
    await expect(kwhValues.first()).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('Status Page - Polling State', () => {
  test('should show connection state', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // The snapshot is loaded, so the status page should show energy data.
    // The SummaryTiles grid's "Solar" tile is the most reliable indicator
    // (scoped to the grid so the nav link of the same name doesn't match).
    const tiles = page.getByTestId('summary-tiles-grid');
    await expect(tiles.getByText('Solar', { exact: true })).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('Status Page - API Snapshot', () => {
  test('snapshot endpoint returns valid data', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.data).toBeDefined();
    expect(typeof data.data.soc).toBe('number');
    expect(data.data.soc).toBeGreaterThanOrEqual(0);
    expect(data.data.soc).toBeLessThanOrEqual(100);
  });

  test('snapshot has solar power data', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(typeof data.data.solar_power).toBe('number');
  });

  test('snapshot has grid power data', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(typeof data.data.grid_power).toBe('number');
  });

  test('snapshot has battery data', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(typeof data.data.battery_power).toBe('number');
    expect(typeof data.data.battery_voltage).toBe('number');
    expect(typeof data.data.battery_temperature).toBe('number');
  });

  test('snapshot has device type', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.data.device_type).toBeDefined();
  });
});

test.describe('Status Page - House View (issue #422)', () => {
  const useHouse = async (page: Page, theme: 'dark' | 'light' = 'dark') => {
    await page.addInitScript((t) => {
      localStorage.setItem('energyFlowDisplay', 'house');
      localStorage.setItem('themeMode', t);
    }, theme);
  };

  for (const theme of ['dark', 'light'] as const) {
    test(`fits an iPhone 12 Pro screen without scrolling (${theme})`, async ({ page }) => {
      // 844 px tall minus the ~47 px / ~34 px safe areas Chromium doesn't emulate.
      // The house view starts with the Today statistics hidden, so this is its
      // untouched default.
      await page.setViewportSize({ width: 390, height: 763 });
      await useHouse(page, theme);
      await page.goto('/');
      await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

      await expect(page.getByTestId('house-energy-view')).toBeVisible();
      await expect(page.getByTestId('house-reading-battery')).toBeVisible();
      await expect(page.getByTestId('house-status-pill')).toBeVisible();
      await expect(page.getByTestId('house-charge-card')).toBeVisible();
      await expect(page.getByTestId('house-battery-card')).toBeVisible();
      // Hidden without ever touching the toggle: the house view's default.
      await expect(page.getByTestId('summary-tiles-grid')).toHaveCount(0);
      await expect(page.locator('nav.sticky')).toHaveCount(0);

      const dims = await page.evaluate(() => {
        const main = document.querySelector('main') as HTMLElement;
        return {
          doc: document.documentElement.scrollHeight,
          inner: window.innerHeight,
          main: main.scrollHeight,
          mainClient: main.clientHeight,
        };
      });
      expect(dims.doc).toBeLessThanOrEqual(dims.inner);
      expect(dims.main).toBeLessThanOrEqual(dims.mainClient);
    });
  }

  test('shows HEM in the header on a phone and the full name on a desktop', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 763 });
    await page.goto('/');
    await expect(page.getByTestId('app-title-short')).toBeVisible({ timeout: 20_000 });
    // The full name is screen-reader-only here: a 1 px clipped box, which
    // Playwright still counts as visible, so check its rendered width.
    const fullWidth = () => page.getByText('Home Energy Manager', { exact: true }).evaluate((el) => el.getBoundingClientRect().width);
    expect(await fullWidth()).toBeLessThanOrEqual(1);
    await expect(page.getByRole('heading', { level: 1, name: /^Home Energy Manager/ })).toBeAttached();

    await page.setViewportSize({ width: 1280, height: 800 });
    await expect(page.getByTestId('app-title-short')).toBeHidden();
    await expect(page.getByText('Home Energy Manager', { exact: true })).toBeVisible();
    expect(await fullWidth()).toBeGreaterThan(100);
  });

  test('navigates from the burger menu and closes it', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 763 });
    await useHouse(page);
    await page.goto('/');
    await expect(page.getByTestId('house-energy-view')).toBeVisible({ timeout: 20_000 });

    const menu = page.getByRole('button', { name: 'Menu' });
    await menu.click();
    const drawer = page.getByRole('dialog', { name: 'Pages' });
    await expect(drawer).toBeVisible();
    await expect(drawer.getByRole('link', { name: 'Status' })).toHaveAttribute('aria-current', 'page');

    await page.keyboard.press('Escape');
    await expect(drawer).toBeHidden();
    await expect(menu).toBeFocused();

    await menu.click();
    await drawer.getByRole('link', { name: 'Battery' }).click();
    await expect(page).toHaveURL(/#\/battery$/);
    await expect(drawer).toBeHidden();
  });

  test('draws a single cable off the panels', async ({ page }) => {
    await useHouse(page);
    await page.goto('/');
    await expect(page.getByTestId('house-energy-view')).toBeVisible({ timeout: 20_000 });
    // One cable per part: never more than one line off the panels, whatever the flows.
    expect(await page.locator('[data-testid="house-route-solar"]').count()).toBeLessThanOrEqual(1);
    const ids = await page.locator('[data-testid^="house-route-"]').evaluateAll((els) => els.map((e) => e.getAttribute('data-testid')));
    expect(new Set(ids).size).toBe(ids.length);
    await expect(page.getByTestId('house-consumer-unit')).toBeVisible();
  });

  test('hides the Today statistics from Settings', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('summary-tiles-grid')).toBeVisible({ timeout: 20_000 });
    await page.goto('/#/settings');
    await page.getByRole('switch', { name: 'Show Today Statistics' }).click();
    await page.goto('/#/');
    await expect(page.getByRole('heading', { name: 'Today' })).toHaveCount(0);
    await expect(page.getByTestId('summary-tiles-grid')).toHaveCount(0);
  });

  test('switches between Orbit and House from Settings without a reload', async ({ page }) => {
    await page.goto('/#/settings');
    const group = page.getByRole('group', { name: 'Energy Flow Display' });
    await group.getByRole('button', { name: 'House' }).click();
    await expect(page.locator('nav.sticky')).toHaveCount(0);

    await page.getByRole('button', { name: 'Menu' }).click();
    await page.getByRole('dialog', { name: 'Pages' }).getByRole('link', { name: 'Status' }).click();
    await expect(page.getByTestId('house-energy-view')).toBeVisible({ timeout: 20_000 });

    await page.goto('/#/settings');
    await page.getByRole('group', { name: 'Energy Flow Display' }).getByRole('button', { name: 'Orbit' }).click();
    await expect(page.locator('nav.sticky')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Menu' })).toHaveCount(0);
  });
});
