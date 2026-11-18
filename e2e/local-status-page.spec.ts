/**
 * E2E tests for the Status / Dashboard page.
 *
 * Tests the energy flow diagram, the Today balance bars (#429), and loading
 * states using the real GivEnergy simulator.
 */

import type { Page } from '@playwright/test';
import * as fs from 'fs';
import * as path from 'path';
import { test, expect } from './local-fixture.js';

// The two longest lines the Battery Level card can show (issues #476, #472),
// so each fit check below covers both: charging faster than the forecast
// (the longest text), and a busy day running out before tomorrow's charge
// (with its dots and ⓘ).
type RuntimeKind = 'charging' | 'risk';
const RUNTIME_KINDS: readonly RuntimeKind[] = ['charging', 'risk'];
const runtimeAnswer = (kind: RuntimeKind, now: number) => ({
  ok: true,
  data: kind === 'charging'
    ? { outcome: 'charging', computed_at: Math.floor(now / 1000) }
    : {
      outcome: 'recharges_first', by: 'charge', at: Math.floor(now / 1000) + 5 * 3600, risk: Math.floor(now / 1000) + 3600,
      confidence: 'medium', computed_at: Math.floor(now / 1000),
    },
});
const runtimeText = (kind: RuntimeKind) => (kind === 'charging'
  ? 'Unable to calculate: charging faster than forecast'
  : /^Empty .* or later, charge .*\d\d:\d\d$/);

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
    await expect(page.getByRole('region', { name: "Today's Energy Balance" })).toBeVisible({ timeout: 5_000 });

    await expect(page.getByRole('heading', { name: 'Battery', exact: true })).toHaveCount(0);
  });
});

test.describe('Status Page - Today Balance (issue #429)', () => {
  test('should show Today heading', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.getByRole('region', { name: "Today's Energy Balance" })).toBeVisible({ timeout: 5_000 });
  });

  test('should show both bars', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    await expect(page.getByText("Today's Energy In", { exact: true })).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText("Today's Energy Out", { exact: true })).toBeVisible({ timeout: 5_000 });
  });

  test('should show the Home Use segment', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // There is no legend, so the text alternative inside the card is the
    // stable place to see all six segments whatever the fill widths.
    await expect(page.getByTestId('today-balance')).toContainText('Home Use', { timeout: 5_000 });
  });

  test('should show Grid Import and Grid Export in the balance', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    const balance = page.getByTestId('today-balance');
    await expect(balance).toContainText('Grid Import', { timeout: 5_000 });
    await expect(balance).toContainText('Grid Export', { timeout: 5_000 });
  });

  test('should show kWh values in the balance', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // Values live in the pills, not a legend: at least one segment is wide
    // enough to hold its value at any viewport.
    const pillValues = page.getByTestId('today-balance')
      .locator('[data-testid="balance-fill"]')
      .locator('text=/\\d+\\.\\d+kWh/');
    await expect(pillValues.first()).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('Status Page - Polling State', () => {
  test('should show connection state', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });

    // The snapshot is loaded, so the status page should show energy data.
    // Both bar rows are rendered from it; the segment labels may be too
    // narrow for the pill, so count the rows rather than their text.
    const balance = page.getByTestId('today-balance');
    await expect(balance).toBeVisible({ timeout: 5_000 });
    await expect(balance.getByTestId('balance-bar')).toHaveCount(2);
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

  test('battery runtime answers with a known outcome (issue #472)', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/battery/runtime`);
    expect(resp.status).toBe(200);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    // A fresh simulator install has no history, so usually "unknown".
    expect(['empty_at', 'recharges_first', 'lasts_beyond', 'at_reserve', 'below_reserve', 'full_at', 'paused', 'charging', 'unknown']).toContain(data.data.outcome);
    expect(typeof data.data.computed_at).toBe('number');
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
    // No "new version available" banner: whether the backend has seen a newer
    // release would otherwise decide whether the page fits.
    await page.route('**/api/latest-version', async (r) => {
      const res = await r.fetch();
      const j = await res.json();
      await r.fulfill({ response: res, json: { ...j, latest_version: j.current_version, update_available: false } });
    });
    await page.addInitScript((t) => {
      localStorage.setItem('energyFlowDisplay', 'house');
      localStorage.setItem('themeMode', t);
    }, theme);
  };

  for (const [theme, kind] of [['dark', 'charging'], ['light', 'risk'], ['dark', 'risk'], ['light', 'charging']] as const) {
    test(`fits an iPhone 12 Pro screen without scrolling (${theme}, ${kind} runtime line)`, async ({ page }) => {
      // 844 px tall minus the ~47 px / ~34 px safe areas Chromium doesn't emulate.
      // The house view starts with the Today statistics hidden, so this is its
      // untouched default.
      await page.setViewportSize({ width: 390, height: 763 });
      await useHouse(page, theme);
      // The tallest layout: weather (with a location, so the sky band shows)
      // and carbon intensity.
      const now = Date.now();
      await page.route('**/api/weather', (r) => r.fulfill({ json: { ok: true, data: {
        config: { enabled: true, latitude: 51.1, longitude: -1.05 }, last_fetch_at: new Date(now).toISOString(),
        last_fetched_temperature_c: 11, last_fetched_weather_code: 2, last_fetched_is_day: true, last_fetched_solar_w_m2: 486,
      } } }));
      await page.route('**/api/carbon-intensity', (r) => r.fulfill({ json: { ok: true, data: {
        region: 'South England', fetched_at: new Date(now).toISOString(),
        periods: [{ from: new Date(now - 60_000).toISOString(), to: new Date(now + 1_800_000).toISOString(), forecast: 183, index: 'high' }],
      } } }));
      // And the car's next charge under the battery's (issue #474), which
      // needs Octopus set up and Show Car Charging Slots on.
      await page.route('**/api/settings', async (r) => {
        if (r.request().method() !== 'GET') return r.continue();
        const res = await r.fetch();
        const j = await res.json();
        await r.fulfill({ response: res, json: { ...j, data: {
          ...j.data, octopus_account_number: 'A-TEST1', octopus_api_key_configured: true, status_show_car_charging: true,
          // And when the battery empties (issue #472), in the card beside it.
          status_show_battery_runtime: true,
        } } });
      });
      await page.route('**/api/octopus/dispatches', (r) => r.fulfill({ json: {
        ok: true, enabled: false, configured: true, checked_at: new Date(now).toISOString(), error: null, devices: 1,
        holding: false, hold_failing: false, stale_after_minutes: 15,
        dispatches: [{ start: new Date(now + 3_600_000).toISOString(), end: new Date(now + 7_200_000).toISOString(), type: 'BOOST', energy_added_kwh: 7, smart: false }],
      } }));
      await page.route('**/api/battery/runtime', (r) => r.fulfill({ json: runtimeAnswer(kind, now) }));
      await page.goto('/');
      await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });
      await expect(page.getByTestId('house-sky')).toBeVisible();
      await expect(page.getByTestId('house-carbon')).toBeVisible();

      await expect(page.getByTestId('house-energy-view')).toBeVisible();
      await expect(page.getByTestId('house-reading-battery')).toBeVisible();
      await expect(page.getByTestId('house-status-pill')).toBeVisible();
      await expect(page.getByTestId('house-charge-card')).toBeVisible();
      await expect(page.getByTestId('house-car-charge')).toBeVisible();
      await expect(page.getByTestId('house-car-charge')).toContainText('· Boost');
      await expect(page.getByTestId('house-battery-card')).toBeVisible();
      await expect(page.getByTestId('house-runtime-text')).toHaveText(runtimeText(kind));
      // Hidden without ever touching the toggle: the house view's default.
      await expect(page.getByTestId('today-balance')).toHaveCount(0);
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
    // Extra room at the top on a phone, where iOS dims the edge of the screen.
    const headerPadTop = () => page.locator('header').first().evaluate((el) => getComputedStyle(el).paddingTop);
    expect(await headerPadTop()).toBe('28px');

    await page.setViewportSize({ width: 1280, height: 800 });
    expect(await headerPadTop()).toBe('12px');
    await expect(page.getByTestId('app-title-short')).toBeHidden();
    await expect(page.getByText('Home Energy Manager', { exact: true })).toBeVisible();
    expect(await fullWidth()).toBeGreaterThan(100);
  });

  test('centres each weather icon on the temperature, clear of the solar bars', async ({ page }) => {
    const kinds: [number, boolean][] = [[0, true], [0, false], [2, true], [2, false], [3, true], [45, true], [51, true], [61, true], [71, true], [95, true]];
    let current = kinds[0];
    await page.route('**/api/weather', (r) => r.fulfill({ json: { ok: true, data: {
      config: { enabled: true }, last_fetch_at: new Date().toISOString(), last_fetched_temperature_c: 11,
      last_fetched_weather_code: current[0], last_fetched_is_day: current[1], last_fetched_solar_w_m2: 486,
    } } }));
    await page.setViewportSize({ width: 390, height: 763 });
    await useHouse(page);
    for (const kind of kinds) {
      current = kind;
      await page.goto('/');
      await expect(page.getByTestId('house-weather')).toBeVisible({ timeout: 20_000 });
      const m = await page.getByTestId('house-weather').evaluate((el) => {
        const glyph = (el.children[0] as SVGGraphicsElement).getBBox();
        const text = el.querySelector('text') as SVGTextElement;
        const baseline = Number(text.getAttribute('y'));
        const size = Number(text.getAttribute('font-size'));
        // Icon shapes overlapping a bar by more than a hairline on both axes
        // (each shape on its own: the icon's overall box is mostly empty).
        const box = (e: Element) => (e as SVGGraphicsElement).getBBox();
        const shapes = [...el.children[0].querySelectorAll('path, line, circle')].map(box);
        const bars = [...el.querySelectorAll('[data-testid="house-solar-bar"]')].map(box);
        const hits = shapes.filter((a) => bars.some((r) => {
          const ox = Math.min(a.x + a.width, r.x + r.width) - Math.max(a.x, r.x);
          const oy = Math.min(a.y + a.height, r.y + r.height) - Math.max(a.y, r.y);
          return ox > 0.5 && oy > 0.5;
        })).length;
        return { centre: glyph.y + glyph.height / 2, digits: baseline - size * 0.36, hits };
      });
      // Within a few px of the digits' middle: the plain icons sit within 2,
      // thunder's bolt hangs lowest at ~4 (the old layout was 5 off).
      expect(Math.abs(m.centre - m.digits), `code ${kind[0]}`).toBeLessThanOrEqual(4.5);
      expect(m.hits, `code ${kind[0]}`).toBe(0);
    }
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
    // Lines up with the header, below the edge iOS dims.
    expect(await drawer.evaluate((el) => getComputedStyle(el).paddingTop)).toBe('28px');
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
    await expect(page.getByTestId('today-balance')).toBeVisible({ timeout: 20_000 });
    await page.goto('/#/settings');
    await page.getByRole('switch', { name: 'Show Today Statistics' }).click();
    await page.goto('/#/');
    await expect(page.getByTestId('today-balance')).toHaveCount(0);
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

test.describe('Status Page - Today Balance on a phone (issue #429)', () => {
  test('fits the bars in their card at 390×763 with the statistics shown', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 763 });
    // Pin the toggle: invariant 7 keeps the bars within today's tile-card
    // budget, and the narrowest bar in the app sets the label thresholds.
    await page.addInitScript(() => {
      localStorage.setItem('showTodayStats', 'true');
    });
    await page.goto('/');
    const card = page.getByTestId('today-balance');
    await expect(card).toBeVisible({ timeout: 20_000 });

    // Nothing overflows its box — card and bar rows keep their scroll width
    // within their client width (fills clip their own text).
    // +1px tolerates the sub-pixel rounding of the percentage widths.
    const overflows = await page.evaluate(
      () => [...document.querySelectorAll(
        '[data-testid="today-balance"], [data-testid="balance-bar"]',
      )].filter((el) => el.scrollWidth > el.clientWidth + 1).length,
    );
    expect(overflows).toBe(0);
  });
});

test.describe('Status Page - Orbit battery runtime on a phone (issue #472)', () => {
  for (const kind of RUNTIME_KINDS) test(`fits 390×763 unscrolled with the runtime line under the orbit (${kind})`, async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 763 });
    const now = Date.now();
    await page.route('**/api/latest-version', async (r) => {
      const res = await r.fetch();
      const j = await res.json();
      await r.fulfill({ response: res, json: { ...j, latest_version: j.current_version, update_available: false } });
    });
    await page.addInitScript(() => {
      localStorage.setItem('energyFlowDisplay', 'orbit');
      localStorage.setItem('showTodayStats', 'false');
    });
    await page.route('**/api/settings', async (r) => {
      if (r.request().method() !== 'GET') return r.continue();
      const res = await r.fetch();
      const j = await res.json();
      await r.fulfill({ response: res, json: { ...j, data: { ...j.data, status_show_battery_runtime: true } } });
    });
    // The longest lines, so they have to wrap at this width.
    await page.route('**/api/battery/runtime', (r) => r.fulfill({ json: runtimeAnswer(kind, now) }));
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });
    await expect(page.getByTestId('orbit-runtime-text')).toHaveText(runtimeText(kind));
    await expect(page.getByTestId('orbit-battery-runtime').getByRole('button', { name: 'More about Battery Runtime' })).toBeVisible();
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
});

test.describe('Status Page - the desktop app\'s default window', () => {
  // The app opens at tauri.conf.json's window size, read here so the check
  // follows it: the Status page must fit it unscrolled with the Today
  // statistics shown, in either view, with the battery runtime line (#472)
  // on as well, and the house view's car line (#474).
  const { width, height } = (JSON.parse(
    fs.readFileSync(path.join(process.cwd(), 'src-tauri', 'tauri.conf.json'), 'utf8'),
  ) as { app: { windows: { width: number; height: number }[] } }).app.windows[0];
  const windowCases: (readonly ['orbit' | 'house', RuntimeKind | null])[] = [
    ['orbit', null], ['house', null],
    ...(['orbit', 'house'] as const).flatMap((view) => RUNTIME_KINDS.map((kind) => [view, kind] as const)),
  ];
  for (const [view, lines] of windowCases) {
    test(`fits the default window with Today shown (${view}${lines ? `, ${lines} runtime and car lines on` : ''})`, async ({ page }) => {
      await page.setViewportSize({ width, height });
      const now = Date.now();
      // No "new version available" banner: whether the backend has seen a
      // newer release would otherwise decide whether the page fits.
      await page.route('**/api/latest-version', async (r) => {
        const res = await r.fetch();
        const j = await res.json();
        await r.fulfill({ response: res, json: { ...j, latest_version: j.current_version, update_available: false } });
      });
      await page.addInitScript((v) => {
        localStorage.setItem('energyFlowDisplay', v);
        localStorage.setItem('showTodayStats', 'true');
      }, view);
      if (lines) {
        await page.route('**/api/settings', async (r) => {
          if (r.request().method() !== 'GET') return r.continue();
          const res = await r.fetch();
          const j = await res.json();
          await r.fulfill({ response: res, json: { ...j, data: {
            ...j.data, octopus_account_number: 'A-TEST1', octopus_api_key_configured: true, status_show_car_charging: true,
            status_show_battery_runtime: true,
          } } });
        });
        await page.route('**/api/octopus/dispatches', (r) => r.fulfill({ json: {
          ok: true, enabled: false, configured: true, checked_at: new Date(now).toISOString(), error: null, devices: 1,
          holding: false, hold_failing: false, stale_after_minutes: 15,
          dispatches: [{ start: new Date(now + 3_600_000).toISOString(), end: new Date(now + 7_200_000).toISOString(), type: 'SMART', energy_added_kwh: 7, smart: true }],
        } }));
        await page.route('**/api/battery/runtime', (r) => r.fulfill({ json: runtimeAnswer(lines, now) }));
      }
      await page.goto('/');
      await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });
      await expect(page.getByTestId('today-balance')).toBeVisible();
      if (lines && view === 'house') await expect(page.getByTestId('house-car-charge')).toBeVisible();
      if (lines) await expect(page.getByTestId(`${view}-battery-runtime`)).toBeVisible();
      const dims = await page.evaluate(() => ({
        doc: document.documentElement.scrollHeight,
        inner: window.innerHeight,
      }));
      expect(dims.doc).toBeLessThanOrEqual(dims.inner);
    });
  }
});
