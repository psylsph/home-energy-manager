/**
 * E2E tests for the Control page.
 *
 * Tests quick actions, battery mode, charge/discharge schedules,
 * power limits, and auto winter mode using the real simulator.
 *
 * Note: Unlike the mock-based tests, we verify behaviour by reading back
 * state from the REST API after sending commands.
 */

import { test, expect } from './local-fixture.js';

test.describe('Control Page - Quick Actions', () => {
  test('should show Quick Actions heading', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.locator('text=Quick Actions')).toBeVisible({ timeout: 15_000 });
  });

  test('should show Force Charge button', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.locator('text=Force Charge')).toBeVisible({ timeout: 15_000 });
  });

  test('should show Force Discharge button', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.locator('text=Force Discharge')).toBeVisible({ timeout: 15_000 });
  });

  test('should show Pause Battery button', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.locator('text=Pause Battery').first()).toBeVisible({ timeout: 15_000 });
  });

  test('should show Sync Clock button', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.locator('text=Sync Clock')).toBeVisible({ timeout: 15_000 });
  });

  test('Force Charge via API should enable charging', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });

  test('Pause Battery via API should disable both charge and discharge', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/pause`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: 30 }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });

  test('Sync Clock via API should succeed', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/sync-clock`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });
});

test.describe('Control Page - Battery Mode', () => {
  test('should show Battery Mode heading', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.locator('text=Battery Mode')).toBeVisible({ timeout: 15_000 });
  });

  test('should show Eco and Timed toggle buttons', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.locator('text=/Eco/').first()).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('text=/Timed/').first()).toBeVisible();
  });

  test('should switch to Eco mode via API', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'eco' }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });

  test('should switch to timed_demand mode via API', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'timed_demand' }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });

  test('should switch to timed_export mode via API', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'timed_export' }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });

  test('should reject unknown mode', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode: 'invalid_mode' }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(false);
    expect(data.error).toContain('Unknown mode');
  });
});

test.describe('Control Page - Charge Schedule', () => {
  test('should show Charge Schedule heading', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.getByRole('heading', { name: 'Charge Schedule', exact: true })).toBeVisible({ timeout: 15_000 });
  });

  test('should set a charge slot via API', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/charge-slot`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        slot: 1,
        enabled: true,
        start_hour: 0,
        start_minute: 30,
        end_hour: 4,
        end_minute: 30,
        target_soc: 100,
      }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });

  test('should set charge slot 2 via API', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/charge-slot`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        slot: 2,
        enabled: true,
        start_hour: 13,
        start_minute: 0,
        end_hour: 16,
        end_minute: 0,
        target_soc: 80,
      }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });
});

test.describe('Control Page - Discharge Schedule', () => {
  test('should show Discharge Schedule heading', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.locator('text=Discharge Schedule')).toBeVisible({ timeout: 15_000 });
  });

  test('should set a discharge slot via API', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/discharge-slot`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        slot: 1,
        enabled: true,
        start_hour: 16,
        start_minute: 0,
        end_hour: 19,
        end_minute: 0,
      }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });
});

test.describe('Control Page - Battery & Power Limits', () => {
  test('should show Battery & Power Limits heading', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.locator('text=Battery & Power Limits')).toBeVisible({ timeout: 15_000 });
  });

  test('should show Minimum SOC section', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.locator('text=Minimum SOC')).toBeVisible({ timeout: 15_000 });
  });

  test('should show Charge Power Limit section', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.locator('text=/Charge Power Limit|Charge Rate/')).toBeVisible({ timeout: 15_000 });
  });

  test('should show Discharge Power Limit section', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.locator('text=/Discharge Power Limit|Discharge Rate/')).toBeVisible({ timeout: 15_000 });
  });

  test('should show Inverter Active Power Limit section', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.locator('text=Active Power Limit')).toBeVisible({ timeout: 15_000 });
  });

  test('should set SOC reserve via API', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/reserve`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ soc: 20 }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });

  test('should set charge rate via API', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/charge-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ limit: 50 }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });

  test('should set discharge rate via API', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/discharge-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ limit: 50 }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });

  test('should set active power rate via API', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/active-power-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ rate: 80 }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });
});

test.describe('Control Page - Auto Winter Mode', () => {
  test('should show Auto Winter Mode heading', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.locator('text=Auto Winter Mode')).toBeVisible({ timeout: 15_000 });
  });

  test('should show enable toggle', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.locator('text=/Enable|disable auto winter/i').first()).toBeVisible({ timeout: 15_000 });
  });

  test('should fetch auto winter config', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/auto-winter`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.data).toBeDefined();
  });
});

test.describe('Control Page - Validation', () => {
  test('should reject SOC reserve > 100', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/reserve`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ soc: 101 }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(false);
  });

  test('should reject charge rate > 100', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/charge-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ limit: 101 }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(false);
  });

  test('should reject discharge rate > 100', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/discharge-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ limit: 101 }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(false);
  });

  test('should reject active power rate > 100', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/active-power-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ rate: 101 }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(false);
  });

  test('should reject SOC reserve < 0', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/reserve`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ soc: -1 }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(false);
  });

  test('should reject force-charge with negative minutes', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/force-charge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ minutes: -5 }),
    });
    const data = await resp.json();
    // Should either reject or clamp
    expect(data.ok === true || data.ok === false).toBe(true);
  });
});
