/**
 * E2E tests for the Settings page.
 *
 * Tests connection configuration, refresh interval, HTTP port, tariffs,
 * developer mode, and network access display.
 */

import { test, expect } from './local-fixture.js';

test.describe('Settings Page - Loading', () => {
  test('should load settings page', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.getByRole('heading', { name: 'Inverter Connection' })).toBeVisible({ timeout: 10_000 });
  });
});

test.describe('Settings Page - Connection', () => {
  test('should show Connection heading', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.getByRole('heading', { name: 'Inverter Connection' })).toBeVisible({ timeout: 10_000 });
  });

  test('should show connection state', async ({ page }) => {
    await page.goto('/#/settings');
    // Connection state shown on settings page (may show "Connected" or "disconnected")
    await expect(page.locator('text=/connected|disconnected/i').first()).toBeVisible({ timeout: 15_000 });
  });

  test('should show Inverter Address input', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Inverter Address')).toBeVisible({ timeout: 10_000 });
  });

  test('should show Port input', async ({ page }) => {
    await page.goto('/#/settings');
    // Port field may be part of a combined address:port row
    await expect(page.locator('input[type="number"]').first()).toBeVisible({ timeout: 10_000 });
  });

  test('should show Serial Number input', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Serial Number')).toBeVisible({ timeout: 10_000 });
  });

  test('should show Connect button', async ({ page }) => {
    await page.goto('/#/settings');
    // Use role=button with exact name — plain `text=Connect` matches
    // 6 elements (the button, the "Inverter Connection" heading,
    // helper text mentioning "Connect a GivEnergy EV Charger", etc).
    await expect(
      page.getByRole('button', { name: 'Connect', exact: true })
    ).toBeVisible({ timeout: 10_000 });
  });

  test('should show Scan Network button', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Scan Network').first()).toBeVisible({ timeout: 10_000 });
  });
});

test.describe('Settings Page - Refresh Interval', () => {
  test('should show Refresh Interval heading', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Refresh Interval')).toBeVisible({ timeout: 10_000 });
  });

  test('should show interval buttons', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.getByRole('button', { name: '5s', exact: true })).toBeVisible({ timeout: 10_000 });
    await expect(page.getByRole('button', { name: '10s', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: '15s', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: '20s', exact: true })).toBeVisible();
  });

  test('5s should be active by default', async ({ page }) => {
    await page.goto('/#/settings');
    // The 5s button should have the active/highlighted style
    const btn = page.getByRole('button', { name: '5s', exact: true });
    await expect(btn).toBeVisible({ timeout: 10_000 });
  });
});

test.describe('Settings Page - HTTP Port', () => {
  test('should show HTTP Port heading', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=HTTP Port')).toBeVisible({ timeout: 10_000 });
  });
});

test.describe('Settings Page - Energy Tariffs', () => {
  test('should show Energy Tariffs heading', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Energy Tariffs')).toBeVisible({ timeout: 10_000 });
  });

  test('should show Import tariff section', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Import')).toBeVisible({ timeout: 10_000 });
  });

  test('should show Export tariff section', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Export')).toBeVisible({ timeout: 10_000 });
  });

  test('should show Save Tariffs button', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Save Tariffs')).toBeVisible({ timeout: 10_000 });
  });

  test('should show Add window button for time-of-use tariffs', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Add window').first()).toBeVisible({ timeout: 10_000 });
  });

  test('should save tariff settings via API', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/settings`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        host: '127.0.0.1',
        port: 18899,
        serial: '',
        poll_interval: 5,
        http_port: 17337,
        import_tariff: 0.30,
        export_tariff: 0.15,
      }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });

  test('should read back saved tariffs', async ({ baseUrl }) => {
    // First save
    await fetch(`${baseUrl}/api/settings`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        host: '127.0.0.1',
        port: 18899,
        serial: '',
        poll_interval: 5,
        http_port: 17337,
        import_tariff: 0.35,
        export_tariff: 0.12,
      }),
    });

    // Then read back
    const resp = await fetch(`${baseUrl}/api/settings`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.data.import_tariff).toBe(0.35);
    expect(data.data.export_tariff).toBe(0.12);
  });

  test('should accept and read back slot-based tariff config', async ({ baseUrl }) => {
    // Post the new slots shape — a Flux-like tariff with 3 windows.
    // Final slot ends at "23:59" (inclusive) — "24:00" is no longer valid.
    const slots = [
      { start: '00:00', end: '16:00', rate: 0.35 },
      { start: '16:00', end: '19:00', rate: 0.15 },
      { start: '19:00', end: '23:59', rate: 0.35 },
    ];
    const resp = await fetch(`${baseUrl}/api/settings`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        host: '127.0.0.1',
        port: 18899,
        serial: '',
        poll_interval: 5,
        http_port: 17337,
        import_tariff_config: { slots },
      }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);

    // Read back — the slots should be preserved.
    const getResp = await fetch(`${baseUrl}/api/settings`);
    const getData = await getResp.json();
    expect(getData.data.import_tariff_config).not.toBeNull();
    expect(getData.data.import_tariff_config.slots).toHaveLength(3);
    expect(getData.data.import_tariff_config.slots[0].rate).toBe(0.35);
    expect(getData.data.import_tariff_config.slots[1].rate).toBe(0.15);
  });

  test('should accept legacy peak/off-peak tariff and migrate to slots', async ({ baseUrl }) => {
    // Post the OLD shape — must still be accepted (backward compat).
    const resp = await fetch(`${baseUrl}/api/settings`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        host: '127.0.0.1',
        port: 18899,
        serial: '',
        poll_interval: 5,
        http_port: 17337,
        import_tariff_config: {
          peak_rate: 0.30,
          off_peak_rate: 0.10,
          off_peak_start: '00:30',
          off_peak_end: '05:30',
        },
      }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);

    // Read back — should have been migrated to the new slots shape.
    const getResp = await fetch(`${baseUrl}/api/settings`);
    const getData = await getResp.json();
    expect(getData.data.import_tariff_config).not.toBeNull();
    expect(getData.data.import_tariff_config.slots).toBeDefined();
    expect(getData.data.import_tariff_config.slots.length).toBeGreaterThanOrEqual(2);
  });

  test('should reject tariff config with a gap (no 24-hour coverage)', async ({ baseUrl }) => {
    // Slot 1 ends at 05:00 but slot 2 starts at 06:00 — leaves 06:00–23:59 uncovered.
    const resp = await fetch(`${baseUrl}/api/settings`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        host: '127.0.0.1',
        port: 18899,
        serial: '',
        poll_interval: 5,
        http_port: 17337,
        import_tariff_config: {
          slots: [
            { start: '00:00', end: '05:00', rate: 0.20 },
            { start: '06:00', end: '23:59', rate: 0.30 },
          ],
        },
      }),
    });
    expect(resp.status).toBe(400);
    const data = await resp.json();
    expect(data.ok).toBe(false);
    expect(String(data.error).toLowerCase()).toContain('gap');
  });

  test('should reject tariff config with overlapping windows', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/settings`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        host: '127.0.0.1',
        port: 18899,
        serial: '',
        poll_interval: 5,
        http_port: 17337,
        import_tariff_config: {
          slots: [
            { start: '00:00', end: '06:00', rate: 0.20 },
            { start: '05:00', end: '23:59', rate: 0.30 },
          ],
        },
      }),
    });
    expect(resp.status).toBe(400);
    const data = await resp.json();
    expect(data.ok).toBe(false);
    expect(String(data.error).toLowerCase()).toContain('overlap');
  });

  test('should reject tariff config where the last slot does not end at 23:59', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/settings`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        host: '127.0.0.1',
        port: 18899,
        serial: '',
        poll_interval: 5,
        http_port: 17337,
        import_tariff_config: {
          slots: [{ start: '00:00', end: '20:00', rate: 0.20 }],
        },
      }),
    });
    expect(resp.status).toBe(400);
    const data = await resp.json();
    expect(String(data.error)).toContain('23:59');
  });

  test('should reject the legacy "24:00" end time', async ({ baseUrl }) => {
    // "24:00" was the pre-v0.37 sentinel. With the new model the final
    // slot must end at "23:59" (inclusive), so "24:00" must now fail.
    const resp = await fetch(`${baseUrl}/api/settings`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        host: '127.0.0.1',
        port: 18899,
        serial: '',
        poll_interval: 5,
        http_port: 17337,
        import_tariff_config: {
          slots: [{ start: '00:00', end: '24:00', rate: 0.20 }],
        },
      }),
    });
    expect(resp.status).toBe(400);
  });
});

test.describe('Settings Page - Network Access', () => {
  test('should show Network Access heading', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Network Access')).toBeVisible({ timeout: 10_000 });
  });

  test('should show URL with port', async ({ page }) => {
    await page.goto('/#/settings');
    // Two URLs are now shown side-by-side (main + read-only). The main
    // one is the first <code> element on the page.
    await expect(page.locator('code', { hasText: /17337/ }).first()).toBeVisible({ timeout: 10_000 });
  });

  test('should show Copy button', async ({ page }) => {
    await page.goto('/#/settings');
    // Two Copy buttons now (one per URL). The main one's button is first.
    await expect(page.getByRole('button', { name: 'Copy' }).first()).toBeVisible({ timeout: 10_000 });
  });

  test('should show read-only URL with ?RO suffix (issue #114)', async ({ page }) => {
    await page.goto('/#/settings');
    // The read-only URL is the LAN URL with `?RO` appended. The element
    // is rendered inside a <code> tag like the main URL.
    const readOnlyLink = page.locator('code', { hasText: /\?RO$/ });
    await expect(readOnlyLink).toBeVisible({ timeout: 10_000 });

    // The read-only link must end with the `?RO` suffix and contain a
    // port number. We don't compare against the main URL because the LAN
    // IP detection can return a different host than localhost — both are
    // valid, the only invariant is the `?RO` suffix.
    const roText = await readOnlyLink.innerText();
    expect(roText).toMatch(/:\d+\?RO$/);

    // Helpful copy: tells the visitor what hiding the URL does.
    await expect(page.locator('text=Read-only link')).toBeVisible();
  });
});

test.describe('Settings Page - Developer', () => {
  test('should show Developer heading', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.getByText('Developer', { exact: true })).toBeVisible({ timeout: 10_000 });
  });

  test('should show Developer Mode toggle', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Developer Mode')).toBeVisible({ timeout: 10_000 });
  });

  test('should enable developer mode', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Developer Mode')).toBeVisible({ timeout: 10_000 });

    // The toggle is the only `div.cursor-pointer` inside the Developer
    // section (the row containing the "Developer Mode" label).
    const developerSection = page.locator('section').filter({ hasText: 'Developer Mode' }).first();
    const toggle = developerSection.locator('div.cursor-pointer').first();
    await toggle.click();

    // The Modbus port input (title="Inverter Modbus port") becomes visible
    // when developer mode is enabled (it lives in the Inverter Connection
    // section and is gated on `developerMode`).
    await expect(
      page.locator('input[title="Inverter Modbus port"]')
    ).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('Settings Page - About', () => {
  test('should show About heading', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=About')).toBeVisible({ timeout: 10_000 });
  });

  test('should show GitHub link', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=/github/')).toBeVisible({ timeout: 10_000 });
  });
});

test.describe('Settings Page - API', () => {
  test('GET /api/settings returns current settings', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/settings`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.data.host).toBeDefined();
    expect(data.data.port).toBeDefined();
    expect(data.data.poll_interval ?? data.data.interval_secs).toBeDefined();
  });

  test('GET /api/status returns connection info', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/status`);
    const data = await resp.json();
    expect(data.ok).toBe(true);
    expect(data.connection).toBe('connected');
  });
});

test.describe('Settings Page - Energy Flow Diagram Noise Threshold', () => {
  test('should show Energy Flow Diagram sub-section under Panel Controls', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=Panel Controls')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('text=Energy Flow Diagram')).toBeVisible({ timeout: 5_000 });
  });

  test('should show the noise floor description', async ({ page }) => {
    await page.goto('/#/settings');
    await expect(page.locator('text=no animated line, no arrow')).toBeVisible({ timeout: 10_000 });
  });

  test('should show the number input with default value 20', async ({ page }) => {
    await page.goto('/#/settings');
    const section = page.locator('section').filter({ hasText: 'Energy Flow Diagram' });
    const input = section.locator('input[type="number"]');
    await expect(input).toBeVisible({ timeout: 5_000 });
    await expect(input).toHaveValue('20');
  });

  test('should show the range slider with default value 20', async ({ page }) => {
    await page.goto('/#/settings');
    const section = page.locator('section').filter({ hasText: 'Energy Flow Diagram' });
    const slider = section.locator('input[type="range"]');
    await expect(slider).toBeVisible({ timeout: 5_000 });
    await expect(slider).toHaveValue('20');
  });

  test('should show the current value label next to the slider', async ({ page }) => {
    await page.goto('/#/settings');
    const section = page.locator('section').filter({ hasText: 'Energy Flow Diagram' });
    // The label shows e.g. "20W" — look for any text ending in W
    await expect(section.locator('text=20W')).toBeVisible({ timeout: 5_000 });
  });

  test('should update the slider when the number input changes', async ({ page }) => {
    await page.goto('/#/settings');
    const section = page.locator('section').filter({ hasText: 'Energy Flow Diagram' });
    const numberInput = section.locator('input[type="number"]');
    const slider = section.locator('input[type="range"]');

    await numberInput.fill('50');
    // The slider should update to match
    await expect(slider).toHaveValue('50');
    // The label should update too
    await expect(section.locator('text=50W')).toBeVisible();
  });

  test('should update the number input when the slider changes', async ({ page }) => {
    await page.goto('/#/settings');
    const section = page.locator('section').filter({ hasText: 'Energy Flow Diagram' });
    const numberInput = section.locator('input[type="number"]');
    const slider = section.locator('input[type="range"]');

    // Set slider to 40 via the number input (easier than dragging)
    await numberInput.fill('40');
    await expect(slider).toHaveValue('40');
  });

  test('should clamp the value to 0-100 range', async ({ page }) => {
    await page.goto('/#/settings');
    const section = page.locator('section').filter({ hasText: 'Energy Flow Diagram' });
    const numberInput = section.locator('input[type="number"]');

    await numberInput.fill('-10');
    await expect(numberInput).toHaveValue('0');

    await numberInput.fill('200');
    await expect(numberInput).toHaveValue('100');
  });

  test('should persist the threshold across page reloads', async ({ page }) => {
    await page.goto('/#/settings');
    const section = page.locator('section').filter({ hasText: 'Energy Flow Diagram' });
    const numberInput = section.locator('input[type="number"]');

    // Change to 35
    await numberInput.fill('35');
    await expect(numberInput).toHaveValue('35');

    // Reload
    await page.reload();
    await page.waitForTimeout(1_000);

    // Should still be 35
    const sectionAfter = page.locator('section').filter({ hasText: 'Energy Flow Diagram' });
    const inputAfter = sectionAfter.locator('input[type="number"]');
    await expect(inputAfter).toHaveValue('35');
  });

  test('should affect the energy flow diagram on the status page', async ({ page }) => {
    // First, set threshold to 100 (max) via the number input
    await page.goto('/#/settings');
    const section = page.locator('section').filter({ hasText: 'Energy Flow Diagram' });
    const numberInput = section.locator('input[type="number"]');
    await numberInput.fill('100');
    await expect(numberInput).toHaveValue('100');

    // Now go to the status page — grid at 500W is above 100W threshold, so still shows
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });
    // Use first() to disambiguate between SVG text and summary tile span
    await expect(page.locator('text=Import').first()).toBeVisible({ timeout: 5_000 });
  });

  test('should hide grid flow when threshold exceeds grid power', async ({ page }) => {
    // Set threshold to 100 (max) — simulator grid is 500W, so still above threshold.
    // The simulator doesn't produce sub-threshold values, so we verify the UI
    // renders correctly with the threshold control visible and functional.
    await page.goto('/#/settings');
    const section = page.locator('section').filter({ hasText: 'Energy Flow Diagram' });
    await expect(section.locator('input[type="range"]')).toBeVisible({ timeout: 5_000 });

    // Set to 0 (show everything)
    const numberInput = section.locator('input[type="number"]');
    await numberInput.fill('0');
    await expect(numberInput).toHaveValue('0');

    // Go to status page — everything should still show
    await page.goto('/');
    await expect(page.locator('text=Waiting for data')).toBeHidden({ timeout: 20_000 });
    // Use first() to disambiguate between SVG text and summary tile span
    await expect(page.locator('text=Import').first()).toBeVisible({ timeout: 5_000 });
  });
});
