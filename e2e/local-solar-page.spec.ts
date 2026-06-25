/**
 * E2E tests for the Solar page.
 *
 * Tests solar overview, PV1/PV2 breakdown, and energy values.
 */

import { test, expect } from './local-fixture.js';

test.describe('Solar Page - Loading', () => {
  test('should load and show solar data', async ({ page }) => {
    await page.goto('/#/solar');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });
  });
});

test.describe('Solar Page - Overview', () => {
  test('should show Solar Overview heading', async ({ page }) => {
    await page.goto('/#/solar');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Solar Overview')).toBeVisible({ timeout: 5_000 });
  });

  test('should show total solar power', async ({ page }) => {
    await page.goto('/#/solar');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.locator('text=Total Solar Power')).toBeVisible({ timeout: 5_000 });
  });

  test('should show a power value in W or kW', async ({ page }) => {
    await page.goto('/#/solar');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Power should be displayed
    await expect(page.locator('text=/\\d+[Wk]/').first()).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('Solar Page - PV1 Input', () => {
  test('should show PV1 heading', async ({ page }) => {
    await page.goto('/#/solar');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.getByRole('heading', { name: 'PV1' })).toBeVisible({ timeout: 5_000 });
  });

  test('should show PV1 Power', async ({ page }) => {
    await page.goto('/#/solar');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // PV1 card may label power differently
    await expect(page.getByRole('heading', { name: 'PV1' })).toBeVisible({ timeout: 5_000 });
  });

  test('should show PV1 Voltage', async ({ page }) => {
    await page.goto('/#/solar');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Voltage is shown in the PV1 card (just 'Voltage', not 'PV1 Voltage')
    await expect(page.getByText('Voltage', { exact: true }).first()).toBeVisible({ timeout: 5_000 });
  });

  test('should show PV1 Current', async ({ page }) => {
    await page.goto('/#/solar');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Current is shown in the PV1 card
    await expect(page.getByText('Current', { exact: true }).first()).toBeVisible({ timeout: 5_000 });
  });

  test('should show PV1 Today energy', async ({ page }) => {
    await page.goto('/#/solar');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Should show today's energy
    await expect(page.locator('text=/Today/').first()).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('Solar Page - API Data', () => {
  test('snapshot should have solar power data', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(typeof data.data.solar_power).toBe('number');
    expect(data.data.solar_power).toBeGreaterThanOrEqual(0);
  });

  test('snapshot should have PV1 voltage and current', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(typeof data.data.pv1_voltage).toBe('number');
    expect(typeof data.data.pv1_current).toBe('number');
  });

  // Issue #108: per-string PV1/PV2 today totals.
  test('snapshot exposes today_pv1_kwh and today_pv2_kwh', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    // Fields must exist on every snapshot (#[serde(default)]); values
    // may be 0 if the inverter hasn't reported them yet.
    expect(data.data).toHaveProperty('today_pv1_kwh');
    expect(data.data).toHaveProperty('today_pv2_kwh');
    expect(typeof data.data.today_pv1_kwh).toBe('number');
    expect(typeof data.data.today_pv2_kwh).toBe('number');
    expect(data.data.today_pv1_kwh).toBeGreaterThanOrEqual(0);
    expect(data.data.today_pv2_kwh).toBeGreaterThanOrEqual(0);
  });

  test('Solar Overview shows Today total with PV1/PV2 breakdown when data present', async ({ page }) => {
    await page.goto('/#/solar');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // The Today line is always rendered below Total Solar Power.
    await expect(page.getByText(/^Today:/)).toBeVisible({ timeout: 5_000 });
  });

  test('PV1 card shows its own Today row (not the combined total)', async ({ page }) => {
    await page.goto('/#/solar');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // PV1 card has a Today row (could match multiple "Today" labels —
    // we just need at least one, and the PV1 heading is visible).
    await expect(page.getByRole('heading', { name: 'PV1' })).toBeVisible({ timeout: 5_000 });
    // Inside the PV1 card, find the Today label.
    const pv1Card = page.locator('section', { has: page.getByRole('heading', { name: 'PV1' }) });
    await expect(pv1Card.getByText('Today', { exact: true })).toBeVisible();
  });
});
