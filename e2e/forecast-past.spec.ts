import type { Page } from '@playwright/test';
import { test, expect } from './fixture.js';
import { startBackend, stopBackend } from './backend.js';

// The Planner's charts in a real browser with real Recharts (issue #364).
//
// The page tests mock Recharts, so a chart element bound to an axis the chart
// does not have passes them and still crashes the page in production with a
// bare "Invariant failed". Here every chart tab is drawn for every past range.
//
// The forecast endpoints are served from fixtures via `page.route`: the mock
// backend has no Open-Meteo data, and the planner needs none to be drawn.

test.beforeAll(async () => {
  await startBackend();
});
test.afterAll(async () => {
  await stopBackend();
});

const HOUR = 3600;

function forecastFixtures(nowSec: number) {
  const hour = Math.floor(nowSec / HOUR) * HOUR;
  const solar = (from: number, count: number) =>
    Array.from({ length: count }, (_, i) => ({
      timestamp: from + i * HOUR,
      kwh: 1 + (i % 4),
      band_low: 0.5 + (i % 4),
      band_high: 1.5 + (i % 4),
    }));
  const battery = (from: number, count: number) => ({
    capacity_kwh: 9.5,
    start_soc_pct: 60,
    reserve_soc_pct: 4,
    hours: Array.from({ length: count }, (_, i) => [from + i * HOUR, 40 + (i % 30)]),
    end_soc_pct: 55,
    with_current_schedule: null,
  });
  const consumption = Array.from({ length: 24 }, (_, h) => ({
    hour: h,
    kwh: 0.3 + (h % 6) * 0.1,
    p25: 0.2 + (h % 6) * 0.1,
    p75: 0.5 + (h % 6) * 0.1,
  }));
  const forecast = {
    generated_at: nowSec,
    forecast_fetched_at: nowSec - HOUR,
    forecast_age_seconds: HOUR,
    status: [],
    performance_ratio: 0.8,
    performance_ratio_days: 12,
    solar: solar(hour + HOUR, 36),
    solar_today_remaining_kwh: 10,
    solar_tomorrow_kwh: 18.4,
    consumption_weekday: consumption,
    consumption_weekend: consumption,
    consumption_days_observed: 14,
    consumption_sufficient: true,
    consumption_tomorrow_kwh: 11.2,
    battery: battery(hour + HOUR, 36),
    import_tomorrow_kwh: 1.1,
    export_tomorrow_kwh: 7.2,
  };
  const plan = {
    recommendation: { kind: 'no_plan', reason: 'no battery projection available' },
    apply: null,
  };
  // One stored decision a day for the last 90 days, each covering that day.
  const snapshots = Array.from({ length: 90 }, (_, d) => {
    const recordedAt = hour - (d + 1) * 24 * HOUR;
    return {
      recorded_at: recordedAt,
      kind: 'before_window',
      strategy: 'min_soc',
      plan: {
        recommendation: {
          kind: 'no_charge_needed',
          min_soc_pct: 20,
          observed_min_soc_pct: 55,
          current_soc_pct: 60,
          rationale: 'Sunny day — the battery fills from solar.',
        },
        apply: null,
      },
      forecast: {
        generated_at: recordedAt,
        solar: solar(recordedAt, 24),
        battery: battery(recordedAt, 24),
      },
    };
  }).reverse();
  return { forecast, plan, snapshots };
}

async function servePlannerFixtures(page: Page) {
  const nowSec = Math.floor(Date.now() / 1000);
  const { forecast, plan, snapshots } = forecastFixtures(nowSec);
  const json = (data: unknown) => ({
    status: 200,
    contentType: 'application/json',
    body: JSON.stringify({ ok: true, data }),
  });
  await page.route('**/api/forecast', (route) => route.fulfill(json(forecast)));
  await page.route('**/api/forecast/plan', (route) => route.fulfill(json(plan)));
  await page.route('**/api/forecast/plan-history?*', (route) => {
    const url = new URL(route.request().url());
    const from = Number(url.searchParams.get('from'));
    const to = Number(url.searchParams.get('to'));
    return route.fulfill(
      json({ snapshots: snapshots.filter((s) => s.recorded_at >= from && s.recorded_at < to) }),
    );
  });
  await page.route('**/api/history?*', (route) => {
    const url = new URL(route.request().url());
    const start = Number(url.searchParams.get('start_ms'));
    const end = Number(url.searchParams.get('end_ms'));
    const points = (value: (i: number) => number) => {
      const out: { t: number; v: number }[] = [];
      for (let t = start, i = 0; t < end; t += 1_800_000, i++) out.push({ t, v: value(i) });
      return out;
    };
    return route.fulfill(
      json({
        soc: points((i) => 30 + (i % 50)),
        solar_power: points((i) => 500 * (i % 8)),
      }),
    );
  });
}

const RANGES = ['24 hours', '3 days', '7 days', '30 days', '90 days'];
const TABS = ['battery', 'solar', 'consumption'];

test.describe('Planner past view', () => {
  test('every chart draws for every past range without crashing the page', async ({ page }) => {
    const pageErrors: string[] = [];
    page.on('pageerror', (err) => pageErrors.push(err.message));
    await servePlannerFixtures(page);

    await page.goto('/#/forecast');
    const pastControl = page.getByTestId('forecast-past-range');
    await expect(pastControl).toBeVisible();

    for (const range of RANGES) {
      // Home use has no past view, so the range control is only on the others.
      await page.getByTestId('forecast-chart-tab-battery').click();
      await pastControl.getByRole('button', { name: range }).click();
      for (const tab of TABS) {
        await page.getByTestId(`forecast-chart-tab-${tab}`).click();
        await expect(page.locator(`#forecast-chart-panel-${tab} .recharts-surface`).first()).toBeVisible();
        await expect(page.getByText('Something went wrong'), `${range} / ${tab}`).toHaveCount(0);
      }
    }
    expect(pageErrors).toEqual([]);
  });
});
