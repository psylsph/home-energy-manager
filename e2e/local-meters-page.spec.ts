/**
 * E2E tests for the Meters page (External CT Meters).
 *
 * Tests meter configuration display and meter card rendering.
 */

import { test, expect } from './local-fixture.js';

test.describe('Meters Page - Loading', () => {
  test('should load and show meters heading', async ({ page }) => {
    await page.goto('/#/meters');
    await expect(page.getByRole('heading', { name: 'External CT Meters' })).toBeVisible({ timeout: 15_000 });
  });
});

test.describe('Meters Page - CT Configuration', () => {
  test('should show CT Clamp Configuration or "No meters" message', async ({ page }) => {
    await page.goto('/#/meters');
    // Wait for the page to settle (poll loop produces snapshot, meters card
    // and/or empty-state message then renders).
    await expect(page.getByText('CT Clamp Configuration').or(
      page.getByText(/No external CT meters detected|Connect to an inverter/)
    )).toBeVisible({ timeout: 15_000 });
  });
});

test.describe('Meters Page - API Data', () => {
  test('snapshot should have meters array', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    // meters may be empty or have entries depending on simulator config
    expect(Array.isArray(data.data.meters)).toBe(true);
  });
});
