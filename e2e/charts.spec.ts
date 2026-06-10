import type { Locator, Page } from '@playwright/test';
import { test, expect } from './fixture.js';

async function waitForHistoryData(baseUrl: string): Promise<void> {
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    const params = new URLSearchParams({
      range: '24h',
      fields: 'battery_power,today_charge_kwh,today_discharge_kwh',
      rolling: 'true',
    });
    const resp = await fetch(`${baseUrl}/api/history?${params}`);
    const body = await resp.json();
    const data = body.data as Record<string, { t: number; v: number }[]> | undefined;
    if (data && Object.values(data).some((points) => points.length > 0)) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error('Timed out waiting for history data');
}

async function expectRangeSelected(page: Page, label: string) {
  await expect(page.getByRole('button', { name: label })).toHaveAttribute('aria-pressed', 'true');
}

async function expectLegendToggle(button: Locator) {
  await expect(button).toHaveAttribute('aria-pressed', 'true');
  const muteTitle = await button.getAttribute('title');
  expect(muteTitle).toMatch(/^Mute /);

  await button.click();
  await expect(button).toHaveAttribute('aria-pressed', 'false');
  const showTitle = await button.getAttribute('title');
  expect(showTitle).toBe(muteTitle?.replace('Mute ', 'Show '));

  await button.click();
  await expect(button).toHaveAttribute('aria-pressed', 'true');
  await expect(button).toHaveAttribute('title', muteTitle ?? '');
}

test.describe('Chart ranges', () => {
  test('selected time range is shared between Power and History pages', async ({ page }) => {
    await page.goto('/#/power');
    await expect(page.getByRole('heading', { name: 'Power Flow' })).toBeVisible();

    await page.getByRole('button', { name: '6h' }).click();
    await expectRangeSelected(page, '6h');

    await page.goto('/#/history');
    await expectRangeSelected(page, '6h');

    await page.getByRole('button', { name: '12h' }).click();
    await expectRangeSelected(page, '12h');

    await page.goto('/#/power');
    await expect(page.getByRole('heading', { name: 'Power Flow' })).toBeVisible();
    await expectRangeSelected(page, '12h');
  });

  test('selected time range survives a page reload', async ({ page }) => {
    await page.goto('/#/power');
    await page.evaluate(() => localStorage.removeItem('chartRange'));
    await page.reload();
    await expect(page.getByRole('heading', { name: 'Power Flow' })).toBeVisible();

    await page.getByRole('button', { name: '6h' }).click();
    await expectRangeSelected(page, '6h');

    await page.reload();
    await expect(page.getByRole('heading', { name: 'Power Flow' })).toBeVisible();
    await expectRangeSelected(page, '6h');
  });
});

test.describe('Chart legends', () => {
  test('Power page legend items can be muted and restored', async ({ page }) => {
    await page.goto('/#/power');
    await expect(page.getByRole('heading', { name: 'Power Flow' })).toBeVisible();

    await expectLegendToggle(page.getByRole('button', { name: 'Battery SOC' }));
  });

  test('History page multi-series legends can be muted and restored', async ({ page, baseUrl }) => {
    await waitForHistoryData(baseUrl);

    await page.goto('/#/history');
    await expect(page.getByRole('heading', { name: 'Charge / Discharge Power' })).toBeVisible();

    await expectLegendToggle(page.getByRole('button', { name: 'Charge' }).first());
  });
});
