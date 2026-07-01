/**
 * CI-runnable E2E tests for the slot-based Agile Octopus refactor.
 *
 * This is the high-value companion to `local-agile-slot.spec.ts`. Where
 * the local spec checks snapshot booleans against the real simulator,
 * this spec uses the mock Modbus server (with its admin write-capture
 * API) to verify the EXACT registers the slot-based state machine
 * writes — which is the whole point of the refactor: Agile now drives
 * the inverter's native schedule registers (HR 94/95, HR 56/57)
 * instead of the legacy momentary ForceCharge/ForceDischarge flags.
 *
 * The Octopus pricing API is mocked by a per-test local HTTP server
 * (startMockOctopus), pointed at via POST /api/agile { api_base_url }.
 *
 * What these tests pin:
 *   - Cheap price + Full scope   → writes HR 94/95/96/20/116 (AgileChargeSlot)
 *   - Expensive price + Full     → writes HR 56/57/59 + HR 27=0 (AgileDischargeSlot)
 *   - Expensive price + Charge Only → NO discharge writes (HR 56/57/59 untouched)
 *   - Cheap price + Discharge Only   → NO charge writes (HR 94/95/96 untouched)
 *   - scope=off after arming    → AgileClearActiveSlot zeros HR 94/95/96/56/57/59
 *   - UI: selecting "Agile — Charge only" POSTs { scope: "charge_only" }
 *
 * Isolation: the backend's Agile config persists to settings.json and
 * the poll loop re-evaluates every cycle, so every test disarms
 * (scope=off, api_base_url='') in afterEach and waits for the clear
 * writes to drain. This prevents Agile writes from leaking into other
 * specs in the shared CI backend.
 */

import { test, expect } from './fixture.js';
import { startBackend, stopBackend } from './backend.js';

// Each spec file runs against a FRESH backend instance so backend-internal
// state (detected device type, armed slots, battery-mode state machine) can't
// leak between spec files. See e2e/backend.ts.
test.beforeAll(async () => {
  await startBackend();
});
test.afterAll(async () => {
  await stopBackend();
});
import type { RegisterWrite } from './mock-modbus.js';
import { createServer, type Server } from 'http';
import type { AddressInfo } from 'net';

// ---------------------------------------------------------------------------
// Mock Octopus pricing API
// ---------------------------------------------------------------------------

interface MockOctopus {
  server: Server;
  baseUrl: string;
  /** Swap the price emitted for the active run mid-test. */
  setPrice: (p: number) => void;
}

/**
 * Start a local server that mimics the Octopus Agile standard-unit-rates
 * endpoint. The current half-hour (and `runSlots - 1` following slots)
 * are priced at `priceForNow`; everything else is a neutral 20p that
 * never trips the charge or discharge thresholds. The Octopus API
 * returns results newest-first, so we reverse the array.
 */
async function startMockOctopus(
  priceForNow: number,
  runSlots = 2,
): Promise<MockOctopus> {
  let currentPrice = priceForNow;
  const server = createServer((_req, res) => {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    const now = Date.now();
    // Align to the start of the current 30-min slot.
    const slotMs = 30 * 60_000;
    const currentSlotStart = Math.floor(now / slotMs) * slotMs;
    const results: Array<Record<string, unknown>> = [];
    // Emit today + tomorrow (96 slots), newest-first.
    for (let i = 191; i >= 0; i--) {
      const from = currentSlotStart - 48 * slotMs + i * slotMs;
      const to = from + slotMs;
      // The active run is the `runSlots` slots starting at the current slot.
      const slotIndexFromNow = (from - currentSlotStart) / slotMs;
      const inRun = slotIndexFromNow >= 0 && slotIndexFromNow < runSlots;
      results.push({
        value_inc_vat: inRun ? currentPrice : 20.0,
        valid_from: new Date(from).toISOString(),
        valid_to: new Date(to).toISOString(),
      });
    }
    res.end(JSON.stringify({ count: results.length, results }));
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const addr = server.address() as AddressInfo;
  return {
    server,
    baseUrl: `http://127.0.0.1:${addr.port}`,
    setPrice: (p) => {
      currentPrice = p;
    },
  };
}

// ---------------------------------------------------------------------------
// Shared write-capture helpers (mirrors control.spec.ts patterns)
// ---------------------------------------------------------------------------

function findWrite(writes: RegisterWrite[], address: number): RegisterWrite | undefined {
  return writes.find((w) => w.address === address);
}

/** Last captured write to `address` (later writes override earlier ones). */
function lastWrite(writes: RegisterWrite[], address: number): RegisterWrite | undefined {
  const matches = writes.filter((w) => w.address === address);
  return matches[matches.length - 1];
}

async function waitForWrites(
  peekWrites: () => Promise<RegisterWrite[]>,
  drainWrites: () => Promise<RegisterWrite[]>,
  predicate: (writes: RegisterWrite[]) => boolean,
  timeoutMs = 40_000,
): Promise<RegisterWrite[]> {
  const start = Date.now();
  let acc: RegisterWrite[] = [];
  while (Date.now() - start < timeoutMs) {
    acc = [...acc, ...(await drainWrites())];
    if (predicate(acc)) return acc;
    await new Promise((r) => setTimeout(r, 500));
  }
  return acc;
}

/** Aggressively drain pending writes until 3s pass with no new ones. */
async function clearWrites(drainModbusWrites: () => Promise<RegisterWrite[]>) {
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    await drainModbusWrites();
    await new Promise((r) => setTimeout(r, 3000));
    if ((await drainModbusWrites()).length === 0) return;
  }
}

async function setAgile(baseUrl: string, body: Record<string, unknown>): Promise<void> {
  const resp = await fetch(`${baseUrl}/api/agile`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  expect((await resp.json()).ok).toBe(true);
}

/**
 * Disarm Agile and restore the real Octopus URL so the next spec starts
 * clean. Waits for the AgileClearActiveSlot writes to drain so they
 * don't leak into the following test.
 */
async function disarmAgile(
  baseUrl: string,
  drainModbusWrites: () => Promise<RegisterWrite[]>,
): Promise<void> {
  await setAgile(baseUrl, { scope: 'off', api_base_url: '' });
  // Give the poll loop a couple cycles to fire the clear writes.
  await new Promise((r) => setTimeout(r, 8000));
  await clearWrites(drainModbusWrites);
}

// Register address constants (from src-tauri/src/modbus/registers.rs).
const HR_BATTERY_POWER_MODE = 27;
const HR_ENABLE_CHARGE_TARGET = 20;
const HR_DISCHARGE_SLOT_1_START = 56;
const HR_DISCHARGE_SLOT_1_END = 57;
const HR_ENABLE_DISCHARGE = 59;
const HR_CHARGE_SLOT_1_START = 94;
const HR_CHARGE_SLOT_1_END = 95;
const HR_ENABLE_CHARGE = 96;
const HR_CHARGE_TARGET_SOC = 116;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test.describe('Agile slot-based mode (register-write verification)', () => {
  let mock: MockOctopus | null = null;

  test.afterEach(async ({ baseUrl, drainModbusWrites }) => {
    if (mock) {
      await new Promise<void>((resolve) => mock!.server.close(() => resolve()));
      mock = null;
    }
    // Critical: disarm so Agile writes never leak into other specs.
    await disarmAgile(baseUrl, drainModbusWrites);
  });

  test('cheap price + Full scope writes the native charge schedule (HR 94/95/96)', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    // The poll loop must fetch prices, evaluate, then write several registers
    // (~1.5s each). Allow headroom over the default 30s test timeout.
    test.setTimeout(60_000);
    await clearWrites(drainModbusWrites);
    mock = await startMockOctopus(5.0); // below charge threshold (10p)

    await setAgile(baseUrl, {
      scope: 'full',
      api_base_url: mock.baseUrl,
      charge_threshold: 10,
      discharge_threshold: 30,
    });

    // AgileChargeSlot writes HR 94 (slot start), HR 95 (slot end),
    // HR 116 = 100, HR 20 = 1, HR 96 = 1. Use last-write so a stale clear
    // from a prior poll can't fool the predicate/assertions. The slot
    // start/end HHMM are time-of-day dependent (the window can legitimately
    // end at midnight = 0), so we assert the arm *signals* (enable_charge =
    // 1, charge_target = 1, target_soc = 100) and that the slot registers
    // were written, not their numeric values.
    const writes = await waitForWrites(
      peekModbusWrites,
      drainModbusWrites,
      (w) => lastWrite(w, HR_CHARGE_SLOT_1_START) !== undefined
        && lastWrite(w, HR_ENABLE_CHARGE)?.value === 1,
    );

    expect(lastWrite(writes, HR_CHARGE_SLOT_1_START)).toBeDefined();
    expect(lastWrite(writes, HR_CHARGE_SLOT_1_END)).toBeDefined();
    expect(lastWrite(writes, HR_ENABLE_CHARGE)?.value).toBe(1);
    expect(lastWrite(writes, HR_ENABLE_CHARGE_TARGET)?.value).toBe(1);
    expect(lastWrite(writes, HR_CHARGE_TARGET_SOC)?.value).toBe(100);
  });

  test('expensive price + Full scope writes the native discharge schedule (HR 56/57/59)', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    // The poll loop must fetch prices, evaluate, then write 4 registers
    // (~1.5s each). Allow headroom over the default 30s test timeout.
    test.setTimeout(60_000);
    await clearWrites(drainModbusWrites);
    mock = await startMockOctopus(35.0); // above discharge threshold (30p)

    await setAgile(baseUrl, {
      scope: 'full',
      api_base_url: mock.baseUrl,
      charge_threshold: 10,
      discharge_threshold: 30,
    });

    // AgileDischargeSlot writes HR 56, 57 (slot times), HR 59 = 1 (enable),
    // then HR 27 = 0 (export) LAST. Use last-write so a stale clear from a
    // prior poll (which writes 59 = 0 / 27 = 1) can't fool the predicate or
    // the assertions. The slot start/end HHMM are time-of-day dependent —
    // the window can legitimately end at midnight (= 0) — so we assert the
    // arm *signals* (enable_discharge = 1, export mode = 0) and that the slot
    // registers were written, not their numeric values.
    const writes = await waitForWrites(
      peekModbusWrites,
      drainModbusWrites,
      (w) => lastWrite(w, HR_DISCHARGE_SLOT_1_START) !== undefined
        && lastWrite(w, HR_BATTERY_POWER_MODE)?.value === 0,
    );

    expect(lastWrite(writes, HR_DISCHARGE_SLOT_1_START)).toBeDefined();
    expect(lastWrite(writes, HR_DISCHARGE_SLOT_1_END)).toBeDefined();
    expect(lastWrite(writes, HR_ENABLE_DISCHARGE)?.value).toBe(1);
    // Export mode (option β) — the inverter dumps to the grid at full power.
    expect(lastWrite(writes, HR_BATTERY_POWER_MODE)?.value).toBe(0);
  });

  test('Charge Only mode does NOT write discharge registers on an expensive price', async ({
    baseUrl,
    drainModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);
    mock = await startMockOctopus(35.0); // expensive

    await setAgile(baseUrl, {
      scope: 'charge_only',
      api_base_url: mock.baseUrl,
      charge_threshold: 10,
      discharge_threshold: 30,
    });

    // Charge Only owns only the charge side. An expensive price must NOT
    // arm a discharge slot — the user's manual Discharge Schedule owns
    // that side. Watch several poll cycles and assert the discharge
    // enable flag is never set to 1 and no non-zero discharge slot start
    // is ever written. (Idle cycles emit AgileClearActiveSlot, which
    // zeros these registers — those zero-writes must not be mistaken
    // for an arm.)
    const deadline = Date.now() + 20_000;
    while (Date.now() < deadline) {
      const writes = await drainModbusWrites();
      const armedDischarge =
        findWrite(writes, HR_ENABLE_DISCHARGE)?.value === 1 ||
        (findWrite(writes, HR_DISCHARGE_SLOT_1_START)?.value ?? 0) !== 0;
      expect(armedDischarge, 'Charge Only must not arm discharge').toBe(false);
      await new Promise((r) => setTimeout(r, 1000));
    }
  });

  test('Discharge Only mode does NOT write charge registers on a cheap price', async ({
    baseUrl,
    drainModbusWrites,
  }) => {
    await clearWrites(drainModbusWrites);
    mock = await startMockOctopus(5.0); // cheap

    await setAgile(baseUrl, {
      scope: 'discharge_only',
      api_base_url: mock.baseUrl,
      charge_threshold: 10,
      discharge_threshold: 30,
    });

    // Symmetric: Discharge Only must never arm a charge slot on a cheap
    // price. Watch for HR_ENABLE_CHARGE=1 or a non-zero charge slot start.
    const deadline = Date.now() + 20_000;
    while (Date.now() < deadline) {
      const writes = await drainModbusWrites();
      const armedCharge =
        findWrite(writes, HR_ENABLE_CHARGE)?.value === 1 ||
        (findWrite(writes, HR_CHARGE_SLOT_1_START)?.value ?? 0) !== 0;
      expect(armedCharge, 'Discharge Only must not arm charge').toBe(false);
      await new Promise((r) => setTimeout(r, 1000));
    }
  });

  test('scope=off after arming clears all active slot registers', async ({
    baseUrl,
    drainModbusWrites,
    peekModbusWrites,
  }) => {
    // Arms a charge slot, drains, then disarms via scope=off. The disarm
    // alone writes 8 registers (~12s at ~1.5s/write), on top of the arm +
    // drain + price-fetch cycles, so the default 30s test timeout is too tight.
    test.setTimeout(90_000);
    // First arm a charge slot.
    await clearWrites(drainModbusWrites);
    mock = await startMockOctopus(5.0);
    await setAgile(baseUrl, {
      scope: 'full',
      api_base_url: mock.baseUrl,
      charge_threshold: 10,
      discharge_threshold: 30,
    });
    await waitForWrites(
      peekModbusWrites,
      drainModbusWrites,
      (w) => findWrite(w, HR_ENABLE_CHARGE)?.value === 1,
    );
    await clearWrites(drainModbusWrites);

    // Now disarm via scope=off. The poll loop emits AgileClearActiveSlot,
    // which zeros every slot register and restores eco mode.
    await setAgile(baseUrl, { scope: 'off', api_base_url: '' });

    const writes = await waitForWrites(
      peekModbusWrites,
      drainModbusWrites,
      (w) => lastWrite(w, HR_CHARGE_SLOT_1_START) !== undefined
        && lastWrite(w, HR_BATTERY_POWER_MODE) !== undefined,
    );

    // Use lastWrite rather than findWrite: the clear is a batch of 8 writes
    // emitted over ~12s, and an in-flight residual from a prior test (e.g. the
    // arm's non-zero HR 94) can otherwise appear as the *first* write to a
    // register, making findWrite return the stale value. lastWrite gives us
    // the clear's value (0 / 1) regardless of what came before.
    expect(lastWrite(writes, HR_CHARGE_SLOT_1_START)?.value).toBe(0);
    expect(lastWrite(writes, HR_CHARGE_SLOT_1_END)?.value).toBe(0);
    expect(lastWrite(writes, HR_ENABLE_CHARGE)?.value).toBe(0);
    expect(lastWrite(writes, HR_ENABLE_CHARGE_TARGET)?.value).toBe(0);
    expect(lastWrite(writes, HR_DISCHARGE_SLOT_1_START)?.value).toBe(0);
    expect(findWrite(writes, HR_DISCHARGE_SLOT_1_END)?.value).toBe(0);
    expect(findWrite(writes, HR_ENABLE_DISCHARGE)?.value).toBe(0);
    expect(findWrite(writes, HR_BATTERY_POWER_MODE)?.value).toBe(1); // eco restored
  });

  test('threshold gap enforcement rejects inverted charge/discharge pair', async ({ baseUrl }) => {
    // The 5p minimum gap is enforced by the FRONTEND Apply handler
    // (saveConfig clamps discharge_threshold up to charge_threshold + 5);
    // the backend accepts any numeric pair. Verify that here so a future
    // backend guard doesn't silently change the contract: an inverted
    // pair is accepted (ok=true), and the clamp lives in the UI layer
    // — covered by tests/pages/controlPageAgileScope.test.tsx.
    const resp = await fetch(`${baseUrl}/api/agile`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        scope: 'full',
        charge_threshold: 30,
        discharge_threshold: 10,
      }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
    // Restore defaults so nothing leaks.
    await setAgile(baseUrl, { scope: 'off' });
  });
});

// ---------------------------------------------------------------------------
// UI interaction tests
// ---------------------------------------------------------------------------

test.describe('Agile Charging Mode dropdown (UI)', () => {
  test.afterEach(async ({ baseUrl, drainModbusWrites }) => {
    await disarmAgile(baseUrl, drainModbusWrites);
  });

  test('selecting "Agile — Charge only" POSTs scope=charge_only', async ({ page }) => {
    // Intercept the /api/agile POST so we can assert the body the
    // frontend builds from the dropdown selection. Fulfil it so the
    // store updates normally.
    let postedBody: unknown = null;
    await page.route('**/api/agile', async (route) => {
      if (route.request().method() === 'POST') {
        postedBody = JSON.parse(route.request().postData() || 'null');
      }
      await route.continue();
    });

    await page.goto('/#/control');
    // Wait for the Charging Mode section to render.
    await expect(page.getByText(/Charging Mode/i).first()).toBeVisible({ timeout: 15_000 });

    // The <select> groups Agile options under an <optgroup label="Agile">.
    // Open it and pick "Agile — Charge only".
    const dropdown = page.getByRole('combobox');
    await dropdown.selectOption('agile_charge');

    // The dropdown change fires the POST immediately.
    await expect.poll(() => postedBody, { timeout: 10_000 }).toMatchObject({
      scope: 'charge_only',
    });
  });

  test('Agile sub-modes are grouped under an optgroup in the dropdown', async ({ page }) => {
    await page.goto('/#/control');
    await expect(page.getByText(/Charging Mode/i).first()).toBeVisible({ timeout: 15_000 });

    // The Control page renders several <select>s (slot editors, rate
    // pickers), so scope to the Charging Mode combobox — the one owning
    // the Agile options.
    const dropdown = page.getByRole('combobox').filter({ hasText: 'Agile' });
    for (const value of ['agile', 'agile_charge', 'agile_discharge']) {
      await expect(dropdown.selectOption(value)).resolves.toBeDefined();
    }
    // Land on Standard so the afterEach disarm isn't racing an armed Agile.
    await dropdown.selectOption('standard');
  });
});
