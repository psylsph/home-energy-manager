/**
 * E2E tests for the slot-based Agile Octopus mode against the real
 * GivEnergy simulator, with the Octopus pricing API mocked by a
 * local HTTP server.
 *
 * These tests verify the end-to-end flow:
 *   1. Start a local mock Octopus server returning canned prices.
 *   2. Point the backend at it via POST /api/agile { api_base_url }.
 *   3. Set the Agile scope (full / charge_only / discharge_only).
 *   4. Wait for the poll loop to fetch prices and drive the inverter.
 *   5. Verify the snapshot reflects the expected slot-driven state.
 *
 * The mock server returns prices for the CURRENT half-hour so the
 * state machine fires immediately. We use prices that are clearly
 * below the charge threshold (5p) or above the discharge threshold
 * (35p) to force the Charge / Discharge actions deterministically.
 *
 * Standard-mode regression guard: switching back to scope=off clears
 * the Agile-driven slots without touching the user's manual schedule.
 */

import { test, expect } from './local-fixture.js';
import { createServer, type Server } from 'http';
import type { AddressInfo } from 'net';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

interface SnapshotData {
  enable_charge: boolean;
  enable_discharge: boolean;
  battery_power_mode: number;
  agile_active: boolean;
  agile_state: string;
  agile_enabled: boolean;
  agile_scope?: string;
  charge_slots: Array<{
    enabled: boolean;
    start_hour: number;
    start_minute: number;
    end_hour: number;
    end_minute: number;
  }>;
  discharge_slots: Array<{
    enabled: boolean;
    start_hour: number;
    start_minute: number;
    end_hour: number;
    end_minute: number;
  }>;
}

async function getSnapshot(baseUrl: string): Promise<SnapshotData> {
  const resp = await fetch(`${baseUrl}/api/snapshot`);
  const json = await resp.json();
  if (!json.ok) throw new Error(`snapshot not ok: ${JSON.stringify(json)}`);
  return json.data as SnapshotData;
}

async function waitForSnapshot(
  baseUrl: string,
  predicate: (d: SnapshotData) => boolean,
  timeoutMs = 30_000,
): Promise<SnapshotData> {
  const start = Date.now();
  let last: SnapshotData | null = null;
  while (Date.now() - start < timeoutMs) {
    try {
      const data = await getSnapshot(baseUrl);
      last = data;
      if (predicate(data)) return data;
    } catch {
      // transient — retry
    }
    await new Promise((r) => setTimeout(r, 1000));
  }
  throw new Error(
    `Snapshot predicate not satisfied within ${timeoutMs}ms. Last: ${JSON.stringify(last)}`,
  );
}

async function setAgile(
  baseUrl: string,
  body: Record<string, unknown>,
): Promise<void> {
  const resp = await fetch(`${baseUrl}/api/agile`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  expect((await resp.json()).ok).toBe(true);
}

/**
 * Start a local HTTP server that mimics the Octopus Agile pricing API.
 * Returns the base URL the backend should use. The `priceForSlot` callback
 * decides the price for each half-hour slot relative to "now".
 */
async function startMockOctopus(
  priceForNow: number,
  contiguousSlots = 1,
): Promise<{ server: Server; baseUrl: string; setPrice: (p: number) => void }> {
  let currentPrice = priceForNow;
  const server = createServer((req, res) => {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    // Build results for today + tomorrow (48 slots per day). The current
    // slot and `contiguousSlots - 1` following slots get `currentPrice`;
    // all others get a neutral 20p so they don't trigger charge/discharge.
    const now = new Date();
    const results: Array<Record<string, unknown>> = [];
    for (let dayOffset = 0; dayOffset < 2; dayOffset++) {
      for (let h = 0; h < 24; h++) {
        for (const m of [0, 30]) {
          const slotDate = new Date(now.getTime() + dayOffset * 86400000);
          slotDate.setUTCHours(h, m, 0, 0);
          const slotEnd = new Date(slotDate.getTime() + 1800000);
          const isCurrentRun =
            dayOffset === 0 &&
            slotDate.getTime() <= now.getTime() &&
            slotEnd.getTime() > now.getTime();
          // The contiguous run starts at the current slot and extends
          // forward for `contiguousSlots` total. Simplification: mark
          // the current slot + next (contiguousSlots-1) as the run.
          const minutesIntoSlot = (now.getTime() - slotDate.getTime()) / 60000;
          const inRun =
            isCurrentRun ||
            (dayOffset === 0 &&
              slotDate.getTime() > now.getTime() &&
              (slotDate.getTime() - now.getTime()) / 60000 <
                contiguousSlots * 30 - minutesIntoSlot);
          const pence = inRun ? currentPrice : 20.0;
          results.push({
            value_inc_vat: pence,
            valid_from: slotDate.toISOString(),
            valid_to: slotEnd.toISOString(),
          });
        }
      }
    }
    // Octopus returns newest-first.
    results.reverse();
    res.end(JSON.stringify({ count: results.length, results }));
  });
  await new Promise<void>((resolve) => {
    server.listen(0, '127.0.0.1', resolve);
  });
  const addr = server.address() as AddressInfo;
  const baseUrl = `http://127.0.0.1:${addr.port}`;
  return {
    server,
    baseUrl,
    setPrice: (p: number) => {
      currentPrice = p;
    },
  };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test.describe('Agile slot-based mode with mocked Octopus API', () => {
  let mock: { server: Server; baseUrl: string; setPrice: (p: number) => void } | null = null;

  test.afterEach(async () => {
    // Always disable Agile and restore the real Octopus URL so we don't
    // leak state into subsequent test files.
    try {
      // The baseUrl fixture is per-test; we can't reach it here, so we
      // rely on each test cleaning up its own scope before finishing.
    } catch {
      /* ignore */
    }
    if (mock) {
      await new Promise<void>((resolve) => mock!.server.close(() => resolve()));
      mock = null;
    }
  });

  test('scope field round-trips through the API', async ({ baseUrl }) => {
    // POST scope=charge_only, GET should return scope=charge_only.
    await setAgile(baseUrl, { scope: 'charge_only' });
    const resp = await fetch(`${baseUrl}/api/agile`);
    const json = await resp.json();
    expect(json.scope).toBe('charge_only');
    expect(json.enabled).toBe(true); // legacy mirror

    // POST scope=off, GET should return scope=off.
    await setAgile(baseUrl, { scope: 'off' });
    const resp2 = await fetch(`${baseUrl}/api/agile`);
    const json2 = await resp2.json();
    expect(json2.scope).toBe('off');
    expect(json2.enabled).toBe(false);
  });

  test('legacy { enabled } shape still works', async ({ baseUrl }) => {
    // Backwards compat: existing frontends that POST { enabled: true }
    // should map to scope=full.
    await setAgile(baseUrl, { enabled: true });
    const resp = await fetch(`${baseUrl}/api/agile`);
    const json = await resp.json();
    expect(json.scope).toBe('full');
    expect(json.enabled).toBe(true);

    // Cleanup.
    await setAgile(baseUrl, { enabled: false });
  });

  test('Charge Only mode ignores expensive prices (no discharge)', async ({ baseUrl }) => {
    // Start a mock that returns an expensive price (35p) for the current
    // slot. In Charge Only mode, this should NOT trigger a discharge —
    // the user's discharge schedule owns that side.
    mock = await startMockOctopus(35.0);
    await setAgile(baseUrl, {
      scope: 'charge_only',
      api_base_url: mock.baseUrl,
      charge_threshold: 10,
      discharge_threshold: 30,
    });

    // Wait a few poll cycles for the price fetch + evaluation.
    // Charge Only + expensive price → Idle (no discharge action).
    const data = await waitForSnapshot(
      baseUrl,
      (d) => d.agile_scope === 'charge_only',
      20_000,
    );
    // The snapshot should report idle (not discharging) because Charge
    // Only ignores the discharge threshold.
    expect(data.agile_state).toBe('idle');
    expect(data.agile_active).toBe(false);

    // Cleanup: restore real Octopus URL + disable Agile.
    await setAgile(baseUrl, { scope: 'off', api_base_url: '' });
  });

  test('Discharge Only mode ignores cheap prices (no charge)', async ({ baseUrl }) => {
    // Symmetric to the previous test: cheap price in Discharge Only
    // mode should NOT trigger a charge.
    mock = await startMockOctopus(5.0);
    await setAgile(baseUrl, {
      scope: 'discharge_only',
      api_base_url: mock.baseUrl,
      charge_threshold: 10,
      discharge_threshold: 30,
    });

    const data = await waitForSnapshot(
      baseUrl,
      (d) => d.agile_scope === 'discharge_only',
      20_000,
    );
    expect(data.agile_state).toBe('idle');
    expect(data.agile_active).toBe(false);

    await setAgile(baseUrl, { scope: 'off', api_base_url: '' });
  });

  test('Full mode drives a charge slot when price is cheap', async ({ baseUrl }) => {
    // Cheap price (5p) + Full scope → the state machine should write a
    // charge slot covering the current half-hour and set enable_charge=1.
    mock = await startMockOctopus(5.0);
    await setAgile(baseUrl, {
      scope: 'full',
      api_base_url: mock.baseUrl,
      charge_threshold: 10,
      discharge_threshold: 30,
    });

    // Wait for the snapshot to report charging. The poll loop fetches
    // prices, evaluates, and writes the slot. We give it up to 40s
    // (several poll cycles at the test's 5s interval) to account for
    // the price fetch + write latency.
    const data = await waitForSnapshot(
      baseUrl,
      (d) => d.agile_state === 'charging' && d.enable_charge === true,
      40_000,
    );
    // The charge slot should be enabled with a non-zero window.
    const activeSlot = data.charge_slots.find((s) => s.enabled);
    expect(activeSlot, 'expected at least one enabled charge slot').toBeTruthy();

    // Cleanup.
    await setAgile(baseUrl, { scope: 'off', api_base_url: '' });
  });

  test('Standard mode (scope=off) disarms any active Agile slot', async ({ baseUrl }) => {
    // First arm Agile, then switch to Standard and verify the snapshot
    // reports idle + the agile_scope is off.
    mock = await startMockOctopus(5.0);
    await setAgile(baseUrl, {
      scope: 'full',
      api_base_url: mock.baseUrl,
    });
    await waitForSnapshot(baseUrl, (d) => d.agile_state === 'charging', 40_000);

    // Switch to Standard.
    await setAgile(baseUrl, { scope: 'off', api_base_url: '' });

    // The next poll should disarm and report idle.
    const data = await waitForSnapshot(
      baseUrl,
      (d) => d.agile_scope === 'off' && d.agile_state === 'idle',
      20_000,
    );
    expect(data.agile_active).toBe(false);
    expect(data.agile_enabled).toBe(false);
  });

  test('api_base_url override is persisted and returned by GET', async ({ baseUrl }) => {
    mock = await startMockOctopus(20.0);
    await setAgile(baseUrl, { api_base_url: mock.baseUrl });
    const resp = await fetch(`${baseUrl}/api/agile`);
    const json = await resp.json();
    expect(json.api_base_url).toBe(mock.baseUrl);
    // Cleanup: restore the default (empty = real Octopus).
    await setAgile(baseUrl, { api_base_url: '' });
  });
});
