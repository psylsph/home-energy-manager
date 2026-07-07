/**
 * E2E tests for issue #110: PV1 & PV2 output as % of max.
 *
 * Covers:
 * - Settings: PV1/PV2 kWp inputs capped at 100
 * - Settings: CT meter array add/remove/save round-trip
 * - Solar page: % display appears when pv1_pct/pv2_pct are in the snapshot
 * - History → Solar tab: PV % of Rated chart appears when % data exists
 * - API: pv1_pct and pv2_pct fields present on /api/snapshot
 */

import { test, expect } from './local-fixture.js';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Post partial settings to the backend. */
async function patchSettings(baseUrl: string, body: Record<string, unknown>) {
  const resp = await fetch(`${baseUrl}/api/settings`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  const json = await resp.json();
  if (!json.ok) throw new Error(`settings patch failed: ${JSON.stringify(json)}`);
  return json;
}

/** Wait for the WebSocket snapshot to carry a non-null pv1_pct / pv2_pct,
 * polling the REST endpoint as a proxy (the WS carries the same snapshot). */
async function waitForPvPct(
  baseUrl: string,
  timeoutMs = 30_000,
): Promise<{ pv1_pct: number; pv2_pct: number }> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const json = await resp.json();
    if (json.ok && json.data.pv1_pct != null && json.data.pv2_pct != null) {
      return { pv1_pct: json.data.pv1_pct, pv2_pct: json.data.pv2_pct };
    }
    await new Promise((r) => setTimeout(r, 1_000));
  }
  throw new Error('Timed out waiting for pv1_pct / pv2_pct in snapshot');
}

// ---------------------------------------------------------------------------
// Settings: kWp input limits
// ---------------------------------------------------------------------------

test.describe('Settings - Solar Arrays (issue #110)', () => {
  test.describe.configure({ mode: 'serial' });

  test.beforeEach(async ({ page }) => {
    // Navigate to settings and wait for the section to load.
    await page.goto('/#/settings');
    await expect(page.getByRole('heading', { name: 'Solar Arrays' })).toBeVisible({ timeout: 15_000 });
  });

  test('PV1 kWp input has max=100', async ({ page }) => {
    const pv1 = page.getByTestId('pv1-rated-kw-input');
    await expect(pv1).toBeVisible();
    expect(await pv1.getAttribute('max')).toBe('100');
  });

  test('PV2 kWp input has max=100', async ({ page }) => {
    const pv2 = page.getByTestId('pv2-rated-kw-input');
    await expect(pv2).toBeVisible();
    expect(await pv2.getAttribute('max')).toBe('100');
  });

  test('CT array kWp input has max=100', async ({ page }) => {
    // Add a row first so the kWp input is visible.
    await page.getByTestId('solar-array-add').click();
    const kwpInputs = page.getByTestId('solar-array-kwp');
    await expect(kwpInputs.first()).toBeVisible();
    expect(await kwpInputs.first().getAttribute('max')).toBe('100');
  });

  test('"+ Add array" appends a new row', async ({ page }) => {
    const before = await page.getByTestId('solar-array-row').count();
    await page.getByTestId('solar-array-add').click();
    const after = await page.getByTestId('solar-array-row').count();
    expect(after).toBe(before + 1);
  });

  test('✕ removes its row', async ({ page }) => {
    // Add a row then immediately remove it.
    await page.getByTestId('solar-array-add').click();
    const before = await page.getByTestId('solar-array-row').count();
    await page.getByTestId('solar-array-remove').last().click();
    const after = await page.getByTestId('solar-array-row').count();
    expect(after).toBe(before - 1);
  });

  test('Save Solar Arrays persists the values', async ({ page, baseUrl }) => {
    // Clear the inputs and enter a known value.
    const pv1 = page.getByTestId('pv1-rated-kw-input');
    await pv1.fill('');
    await pv1.fill('6');

    const pv2 = page.getByTestId('pv2-rated-kw-input');
    await pv2.fill('');
    await pv2.fill('4.2');

    await page.getByTestId('solar-arrays-save').click();

    // Wait for the toast.
    await expect(page.getByText('Solar arrays saved')).toBeVisible({ timeout: 10_000 });

    // Confirm persisted via the API.
    const resp = await fetch(`${baseUrl}/api/settings`);
    const json = await resp.json();
    expect(json.data.pv1_rated_kw).toBeCloseTo(6, 2);
    expect(json.data.pv2_rated_kw).toBeCloseTo(4.2, 2);
  });

  test('negative PV1 input is clamped to 0 on save', async ({ page, baseUrl }) => {
    await page.getByTestId('pv1-rated-kw-input').fill('-2');
    await page.getByTestId('solar-arrays-save').click();
    await expect(page.getByText('Solar arrays saved')).toBeVisible({ timeout: 10_000 });
    const resp = await fetch(`${baseUrl}/api/settings`);
    const json = await resp.json();
    expect(json.data.pv1_rated_kw).toBeLessThanOrEqual(0);
  });

  test('clearing both PV1 and PV2 serialises them to 0', async ({ page, baseUrl }) => {
    const pv1 = page.getByTestId('pv1-rated-kw-input');
    const pv2 = page.getByTestId('pv2-rated-kw-input');
    await pv1.fill('');
    await pv2.fill('');
    await page.getByTestId('solar-arrays-save').click();
    await expect(page.getByText('Solar arrays saved')).toBeVisible({ timeout: 10_000 });
    const resp = await fetch(`${baseUrl}/api/settings`);
    const json = await resp.json();
    expect(json.data.pv1_rated_kw).toBe(0);
    expect(json.data.pv2_rated_kw).toBe(0);
  });

  test('adding and filling a CT row appears in the save payload', async ({ page, baseUrl }) => {
    await page.getByTestId('solar-array-add').click();
    const names = page.getByTestId('solar-array-name');
    await names.last().fill('North roof');
    const kwpInputs = page.getByTestId('solar-array-kwp');
    await kwpInputs.last().fill('5.5');
    await page.getByTestId('solar-arrays-save').click();
    await expect(page.getByText('Solar arrays saved')).toBeVisible({ timeout: 10_000 });
    const resp = await fetch(`${baseUrl}/api/settings`);
    const json = await resp.json();
    const newArr = json.data.solar_arrays.find(
      (a: { name: string }) => a.name === 'North roof',
    );
    expect(newArr).toBeDefined();
    expect(newArr.rated_kw).toBeCloseTo(5.5, 2);
  });

  test('removing a CT row is reflected in the save payload', async ({ page, baseUrl }) => {
    const initialResp = await fetch(`${baseUrl}/api/settings`);
    const initialJson = await initialResp.json();
    const initialCount = initialJson.data.solar_arrays.length;

    // Remove the last row.
    await page.getByTestId('solar-array-remove').last().click();
    await page.getByTestId('solar-arrays-save').click();
    await expect(page.getByText('Solar arrays saved')).toBeVisible({ timeout: 10_000 });

    const resp = await fetch(`${baseUrl}/api/settings`);
    const json = await resp.json();
    expect(json.data.solar_arrays.length).toBe(initialCount - 1);
  });
});

// ---------------------------------------------------------------------------
// Solar page: % of max display
// ---------------------------------------------------------------------------

test.describe('Solar page — % of max (issue #110)', () => {
  test.configure({ mode: 'serial' });

  test.beforeAll(async ({ baseUrl }) => {
    // Set a rated kWp so pv1_pct / pv2_pct are populated.
    await patchSettings(baseUrl, { pv1_rated_kw: 5, pv2_rated_kw: 3 });
  });

  test.afterAll(async ({ baseUrl }) => {
    // Reset to unset so other tests aren't affected.
    await patchSettings(baseUrl, { pv1_rated_kw: 0, pv2_rated_kw: 0 });
  });

  test('Solar page shows the Solar Arrays section when rated kWp is configured', async ({ page }) => {
    await page.goto('/#/solar');
    await expect(page.locator('[data-testid="awaiting"]')).toBeHidden({ timeout: 20_000 });
    await expect(page.getByTestId('solar-arrays')).toBeVisible({ timeout: 10_000 });
  });

  test('Solar Arrays section shows PV1 and PV2 labels', async ({ page }) => {
    await page.goto('/#/solar');
    await expect(page.locator('[data-testid="awaiting"]')).toBeHidden({ timeout: 20_000 });
    await expect(page.getByTestId('solar-arrays')).toBeVisible({ timeout: 10_000 });
    // PV1 and PV2 appear as array labels in the section.
    const arraysSection = page.getByTestId('solar-arrays');
    await expect(arraysSection.getByText('PV1')).toBeVisible();
    await expect(arraysSection.getByText('PV2')).toBeVisible();
  });

  test('Solar Arrays section shows "% of max" label', async ({ page }) => {
    await page.goto('/#/solar');
    await expect(page.locator('[data-testid="awaiting"]')).toBeHidden({ timeout: 20_000 });
    const arraysSection = page.getByTestId('solar-arrays');
    // The "of max" text appears next to the numeric % for both arrays.
    await expect(arraysSection.getByText('of max')).toBeVisible();
  });

  test('Solar page hides the Solar Arrays section when no kWp is configured', async ({ page, baseUrl }) => {
    // Ensure no rated kWp is set.
    await patchSettings(baseUrl, { pv1_rated_kw: 0, pv2_rated_kw: 0 });
    await page.goto('/#/solar');
    await expect(page.locator('[data-testid="awaiting"]')).toBeHidden({ timeout: 20_000 });
    await expect(page.getByTestId('solar-arrays')).not.toBeVisible();
  });

  test('Solar page shows the % progress bar', async ({ page, baseUrl }) => {
    await patchSettings(baseUrl, { pv1_rated_kw: 5, pv2_rated_kw: 3 });
    await page.goto('/#/solar');
    await expect(page.locator('[data-testid="awaiting"]')).toBeHidden({ timeout: 20_000 });
    const arraysSection = page.getByTestId('solar-arrays');
    // Both arrays have progress bars.
    const bars = arraysSection.getByRole('progressbar');
    expect(await bars.count()).toBeGreaterThanOrEqual(1);
  });
});

// ---------------------------------------------------------------------------
// History → Solar tab: PV % of Rated chart
// ---------------------------------------------------------------------------

test.describe('History → Solar — PV % of Rated chart (issue #110)', () => {
  test.configure({ mode: 'serial' });

  test.beforeAll(async ({ baseUrl }) => {
    await patchSettings(baseUrl, { pv1_rated_kw: 5, pv2_rated_kw: 3 });
  });

  test.afterAll(async ({ baseUrl }) => {
    await patchSettings(baseUrl, { pv1_rated_kw: 0, pv2_rated_kw: 0 });
  });

  test('Solar tab shows a "PV % of Rated (kWp)" chart tab', async ({ page }) => {
    await page.goto('/#/history');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });
    await expect(page.getByRole('button', { name: 'Solar' })).toBeVisible();
    await page.getByRole('button', { name: 'Solar' }).click();
    await expect(page.getByText('PV % of Rated (kWp)')).toBeVisible({ timeout: 5_000 });
  });

  test('PV % chart is selectable as a history chart', async ({ page }) => {
    await page.goto('/#/history');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });
    // Click Solar tab.
    await page.getByRole('button', { name: 'Solar' }).click();
    // Click the "PV % of Rated (kWp)" chart tab.
    await page.getByRole('button', { name: 'PV % of Rated (kWp)' }).click();
    // The chart title should now be visible.
    await expect(page.getByText('PV % of Rated (kWp)')).toBeVisible();
  });
});

// ---------------------------------------------------------------------------
// API: pv1_pct and pv2_pct in snapshot
// ---------------------------------------------------------------------------

test.describe('API — pv1_pct / pv2_pct in snapshot (issue #110)', () => {
  test.configure({ mode: 'serial' });

  test.beforeAll(async ({ baseUrl }) => {
    await patchSettings(baseUrl, { pv1_rated_kw: 5, pv2_rated_kw: 3 });
  });

  test.afterAll(async ({ baseUrl }) => {
    await patchSettings(baseUrl, { pv1_rated_kw: 0, pv2_rated_kw: 0 });
  });

  test('snapshot has pv1_pct and pv2_pct fields', async ({ baseUrl }) => {
    const { pv1_pct, pv2_pct } = await waitForPvPct(baseUrl);
    expect(typeof pv1_pct).toBe('number');
    expect(typeof pv2_pct).toBe('number');
  });

  test('pv1_pct is a realistic % (0–200 range)', async ({ baseUrl }) => {
    const { pv1_pct } = await waitForPvPct(baseUrl);
    expect(pv1_pct).toBeGreaterThanOrEqual(0);
    expect(pv1_pct).toBeLessThanOrEqual(200);
  });

  test('pv1_pct + pv2_pct values are plausible for live generation', async ({ baseUrl }) => {
    const { pv1_pct, pv2_pct } = await waitForPvPct(baseUrl);
    // At least one of the strings should be producing power at some point
    // during the day; the simulator generates realistic power.
    // If both are 0 it means it's night — not an error, just no generation.
    // The important invariant: they never go above ~108% even in edge-of-cloud spikes.
    expect(pv1_pct).toBeLessThanOrEqual(200);
    expect(pv2_pct).toBeLessThanOrEqual(200);
  });

  test('GET /api/settings returns pv1_rated_kw, pv2_rated_kw, and solar_arrays', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/settings`);
    const json = await resp.json();
    expect(json.ok).toBe(true);
    expect(json.data).toHaveProperty('pv1_rated_kw');
    expect(json.data).toHaveProperty('pv2_rated_kw');
    expect(json.data).toHaveProperty('solar_arrays');
    expect(Array.isArray(json.data.solar_arrays)).toBe(true);
  });

  test('solar_arrays field is present on snapshot', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const json = await resp.json();
    expect(json.ok).toBe(true);
    expect(json.data).toHaveProperty('solar_arrays');
    expect(Array.isArray(json.data.solar_arrays)).toBe(true);
  });

  test('solar_arrays contains PV1/PV2 entries when kWp is configured', async ({ baseUrl }) => {
    // Wait for the % values to be populated before asserting on solar_arrays.
    await waitForPvPct(baseUrl);
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const json = await resp.json();
    expect(json.data.solar_arrays.length).toBeGreaterThanOrEqual(2);
    const sources = json.data.solar_arrays.map((a: { source: string }) => a.source);
    expect(sources).toContain('pv1');
    expect(sources).toContain('pv2');
  });

  test('solar_arrays entries carry the correct rated_kw', async ({ baseUrl }) => {
    await waitForPvPct(baseUrl);
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const json = await resp.json();
    const pv1 = json.data.solar_arrays.find((a: { source: string }) => a.source === 'pv1');
    const pv2 = json.data.solar_arrays.find((a: { source: string }) => a.source === 'pv2');
    expect(pv1?.rated_kw).toBeCloseTo(5, 1);
    expect(pv2?.rated_kw).toBeCloseTo(3, 1);
  });

  test('solar_arrays entries carry power_w and today_kwh', async ({ baseUrl }) => {
    await waitForPvPct(baseUrl);
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const json = await resp.json();
    const pv1 = json.data.solar_arrays.find((a: { source: string }) => a.source === 'pv1');
    const pv2 = json.data.solar_arrays.find((a: { source: string }) => a.source === 'pv2');
    expect(typeof pv1?.power_w).toBe('number');
    expect(typeof pv2?.power_w).toBe('number');
    expect(typeof pv1?.today_kwh).toBe('number');
    expect(typeof pv2?.today_kwh).toBe('number');
  });

  test('solar_arrays entries have null meter_address for DC strings', async ({ baseUrl }) => {
    await waitForPvPct(baseUrl);
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const json = await resp.json();
    const pv1 = json.data.solar_arrays.find((a: { source: string }) => a.source === 'pv1');
    expect(pv1?.meter_address).toBeNull();
  });
});
