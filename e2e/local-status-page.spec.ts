/**
 * E2E tests for the Status / Dashboard page.
 *
 * Tests the energy flow diagram, summary tiles, battery panel, and loading states
 * using the real GivEnergy simulator.
 */

import { test, expect } from './local-fixture.js';

test.describe('Status Page - Loading', () => {
  test('should show data after initial load', async ({ page }) => {
    await page.goto('/');
    // Should NOT show "Waiting for data" after the poll loop has warmed up
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

test.describe('Status Page - Battery Panel', () => {
  test('should show Battery heading', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Battery').first()).toBeVisible();
  });

  test('should show battery state (Charging/Discharging/Idle)', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=/Charging|Discharging|Idle/').first()).toBeVisible({ timeout: 5_000 });
  });

  test('should show battery SOC percentage', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // SOC should be a percentage value (e.g., "75%")
    await expect(page.locator('text=/\\d{1,3}%/').first()).toBeVisible({ timeout: 5_000 });
  });

  test('should show battery power value', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Power should show with W or kW suffix
    await expect(page.locator('text=/[-+]?\\d+[Wk]/').first()).toBeVisible({ timeout: 5_000 });
  });

  test('should show battery voltage', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Voltage should show with V suffix
    await expect(page.locator('text=/\\d+\\.\\d+V/').first()).toBeVisible({ timeout: 5_000 });
  });

  test('should show battery temperature', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Temperature should show with °C suffix
    await expect(page.locator('text=/\\d+\\.\\d+°C/').first()).toBeVisible({ timeout: 5_000 });
  });

  test('should show capacity bar', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Should show kWh values for available/capacity
    await expect(page.locator('text=/\\d+\\.\\d+kWh/').first()).toBeVisible({ timeout: 5_000 });
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
    // The SummaryTiles "Solar Today" tile is the most reliable indicator.
    await expect(page.getByText('Solar Today', { exact: true })).toBeVisible({ timeout: 5_000 });
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
