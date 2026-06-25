/**
 * E2E tests for the History page Cost tab (issue #133) and Y-axis rendering.
 *
 * Covers three concerns that the pure-data unit tests can't:
 *
 *  1. Issue #133 follow-up — the Cost-tab cost computation must NOT collapse as
 *     the range widens. The bug lived in the frontend preprocess (midnight
 *     rollover detection) interacting with backend bucketing, so we feed a
 *     daily-resetting counter series via route interception at the 6m range
 *     (12 h buckets) and assert the rendered £ total is substantial, not ~£0.
 *
 *  2. £ Y-axis labels must not be clipped off the left edge of the chart card
 *     (the £ glyph is wider than % / W / °C labels).
 *
 *  3. Regression — non-currency charts (Solar etc.) still render a chart with
 *     a visible Y axis after the £-axis fix, so the fix didn't perturb their
 *     layout. This guards the exact mistake an earlier attempt made (adding a
 *     YAxis `width` prop broke every chart's layout).
 *
 * History data is injected via `page.route` so these tests don't depend on
 * real recorded history or the simulator — they exercise the frontend
 * preprocess + chart rendering in isolation.
 */

import { test, expect } from './fixture.js';

// ---------------------------------------------------------------------------
// Data helpers
// ---------------------------------------------------------------------------

/** Local-midnight epoch ms for the calendar day containing `ms`. */
function startOfLocalDay(ms: number): number {
  const d = new Date(ms);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

type Point = { t: number; v: number };

/**
 * Build a daily-resetting cumulative-counter series (the `today_*_kwh`
 * shape): each local day the counter ramps morning→evening then resets at
 * the next local midnight. Two 12 h buckets per day, matching the 6m range's
 * backend bucket size (43200 s).
 *
 * This is the data shape that exposed issue #133 at wide ranges: the morning
 * MAX sits above 5 kWh, so the old value-heuristic rollover detector
 * (`prev > 5 && raw < 5`) failed to recognise the midnight reset and zeroed
 * every following day's energy.
 */
function dailyResetSeries(days: number, morning: number, evening: number): Point[] {
  const pts: Point[] = [];
  const todayMidnight = startOfLocalDay(Date.now());
  for (let d = days - 1; d >= 0; d--) {
    const midnight = todayMidnight - d * 86_400_000;
    pts.push({ t: midnight, v: morning }); // 00:00 bucket
    pts.push({ t: midnight + 12 * 3_600_000, v: evening }); // 12:00 bucket
  }
  return pts;
}

/** Intercept `/api/history` and serve canned per-field series from `map`. */
async function mockHistory(
  page: import('@playwright/test').Page,
  map: Record<string, Point[]>,
): Promise<void> {
  await page.route('**/api/history*', async (route) => {
    const url = new URL(route.request().url());
    const fields = url.searchParams.get('fields')?.split(',') ?? [];
    const data: Record<string, Point[]> = {};
    for (const f of fields) {
      if (map[f]) data[f] = map[f];
    }
    await route.fulfill({ json: { ok: true, data } });
  });
}

/** All Y-axis tick value texts on the page (scoped to the Y axis group). */
async function yAxisTickTexts(page: import('@playwright/test').Page): Promise<string[]> {
  return page.locator('.recharts-yAxis text').allTextContents();
}

/** Largest numeric value among Y-axis tick labels (parsed, units stripped). */
async function yAxisMaxValue(page: import('@playwright/test').Page): Promise<number> {
  const texts = await yAxisTickTexts(page);
  const vals = texts
    .map((s) => Number(s.replace(/[^0-9.-]/g, '')))
    .filter((n) => Number.isFinite(n));
  return vals.length ? Math.max(...vals) : NaN;
}

/**
 * Assert every Y-axis tick on the given chart renders inside the chart's SVG
 * viewport — i.e. none is pushed past the SVG's left edge, which clips the
 * leading glyph (e.g. the £ symbol). The SVG (`.recharts-wrapper`) is the
 * real clipping boundary: a tick rendered at negative SVG-x is cut off.
 * (Checking against the outer card is too loose — the tick can sit inside
 * the card's padding yet still be outside the SVG.)
 */
async function expectYAxisTicksInsideChart(
  page: import('@playwright/test').Page,
  cardHeading: string,
): Promise<void> {
  const card = page.locator('.bg-bg-elevated', { hasText: cardHeading }).first();
  await expect(card).toBeVisible({ timeout: 10_000 });
  // The SVG wrapper is the chart's clipping boundary.
  const wrapper = card.locator('.recharts-wrapper').first();
  await expect(wrapper).toBeVisible({ timeout: 10_000 });
  const wrapBox = await wrapper.boundingBox();
  expect(wrapBox).not.toBeNull();
  const ticks = card.locator('.recharts-yAxis text');
  const count = await ticks.count();
  expect(count).toBeGreaterThan(0);
  for (let i = 0; i < count; i++) {
    const box = await ticks.nth(i).boundingBox();
    expect(box).not.toBeNull();
    // 1px tolerance for sub-pixel rounding. The tick's left edge must not be
    // left of the SVG viewport (where it would be clipped).
    expect(box!.x).toBeGreaterThanOrEqual(wrapBox!.x - 1);
  }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test.describe('History Cost tab — issue #133 (totals collapse at wide ranges)', () => {
  test('export income and import cost stay substantial at the 6m range', async ({ page }) => {
    // 10 days of daily-resetting counters, 12 h buckets (6m range bucket size).
    // Export ramps 6→12 kWh/day; import 1→2 kWh/day. With the fix, export
    // income ≈ £15 and import cost ≈ £5.4. Under the old rollover bug both
    // collapsed to roughly a single day (~£0.9 / £0.28).
    await mockHistory(page, {
      today_export_kwh: dailyResetSeries(10, 6, 12),
      today_import_kwh: dailyResetSeries(10, 1, 2),
    });

    await page.goto('/#/history');
    await page.getByRole('button', { name: 'Cost', exact: true }).click();
    // 6m uses 12 h buckets — the width that exposed the bug.
    await page.getByRole('button', { name: '6m', exact: true }).click();
    await expect(page.getByText('Import Cost & Export Income')).toBeVisible({ timeout: 10_000 });
    // Give the route-mocked fetch + preprocess + render a beat to settle.
    await expect(page.locator('.recharts-wrapper').first()).toBeVisible({ timeout: 10_000 });

    const maxTick = await yAxisMaxValue(page);
    // Substantial: the two series peak near £15 (export) / £5.4 (import); the
    // Y-axis max tick should be well above £3. The collapsed bug produced < £1.
    expect(maxTick).toBeGreaterThan(3);
  });

  test('cost chart renders data points (preprocess wiring intact)', async ({ page }) => {
    await mockHistory(page, {
      today_export_kwh: dailyResetSeries(5, 6, 12),
      today_import_kwh: dailyResetSeries(5, 1, 2),
    });
    await page.goto('/#/history');
    await page.getByRole('button', { name: 'Cost', exact: true }).click();
    await expect(page.locator('.recharts-wrapper').first()).toBeVisible({ timeout: 10_000 });
    // An area chart with real data renders at least one <path> (the area).
    const paths = page.locator('.recharts-area');
    await expect(paths.first()).toBeVisible({ timeout: 10_000 });
  });
});

test.describe('History Y-axis label rendering', () => {
  test('£ labels are not clipped off the left of the Cost chart', async ({ page }) => {
    await mockHistory(page, {
      today_export_kwh: dailyResetSeries(10, 6, 12),
      today_import_kwh: dailyResetSeries(10, 1, 2),
    });
    await page.goto('/#/history');
    await page.getByRole('button', { name: 'Cost', exact: true }).click();
    await page.getByRole('button', { name: '6m', exact: true }).click();
    await expect(page.getByText('Import Cost & Export Income')).toBeVisible({ timeout: 10_000 });

    // At least one £ tick should be rendered.
    const ticks = page.locator('.recharts-yAxis text');
    await expect(ticks.first()).toBeVisible({ timeout: 10_000 });
    const texts = await ticks.allTextContents();
    expect(texts.some((t) => t.includes('£'))).toBe(true);

    // None of them may be clipped past the SVG's left edge.
    await expectYAxisTicksInsideChart(page, 'Import Cost & Export Income');
  });

  test('regression: non-currency Solar chart still renders a Y axis', async ({ page }) => {
    // Feed simple PV data so the chart renders without depending on live
    // history. The point is to confirm the £-axis fix did not perturb the
    // layout of a normal (non-£) chart — the exact mistake an earlier attempt
    // made by adding a YAxis `width` prop globally.
    const now = Date.now();
    const pv1: Point[] = Array.from({ length: 24 }, (_, i) => ({
      t: now - (23 - i) * 3_600_000,
      v: Math.round(1000 * Math.sin((i / 24) * Math.PI) ** 2),
    }));
    await mockHistory(page, { pv1_power: pv1 });

    await page.goto('/#/history');
    await page.getByRole('button', { name: 'Solar', exact: true }).click();
    await expect(page.locator('.recharts-wrapper').first()).toBeVisible({ timeout: 10_000 });

    const ticks = page.locator('.recharts-yAxis text');
    await expect(ticks.first()).toBeVisible({ timeout: 10_000 });
    const count = await ticks.count();
    expect(count).toBeGreaterThan(0);
    // And its ticks must also sit inside the SVG (no clipping regression).
    const cardHeadingText = await page
      .locator('.bg-bg-elevated h3')
      .first()
      .textContent();
    expect(cardHeadingText).toBeTruthy();
    await expectYAxisTicksInsideChart(page, cardHeadingText!);
  });
});
