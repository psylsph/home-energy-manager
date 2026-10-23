import { test, expect } from './fixture.js';
import { startBackend, stopBackend } from './backend.js';
import { servePlannerFixtures } from './forecast-fixtures.js';

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

const RANGES = ['24 Hours', '3 Days', '7 Days', '30 Days', '90 Days'];
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
        await expect(page.getByText('Something Went Wrong'), `${range} / ${tab}`).toHaveCount(0);
      }
    }
    expect(pageErrors).toEqual([]);
  });
});
