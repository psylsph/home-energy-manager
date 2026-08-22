/**
 * E2E regression tests for the 0.74.x field report:
 * "charge slot 1 target SOC > 99 fails and the system charges to 100%".
 *
 * Root cause: POST /api/control/charge-slot defaulted an omitted target_soc
 * to 100 ("no limit"), which disarms any armed target — the encoder's
 * target>=100 branch clears HR 20 and never writes HR 116, so an automated
 * re-post of the slot (e.g. after a poll blip) silently turned an armed 99%
 * target into "charge to full".
 *
 * Fixed by preserving the snapshot's armed target (5-99) when target_soc is
 * omitted. These tests drive the real GUI (Chromium) to arm a target, then
 * replay the automated re-post without target_soc and assert the target
 * survives at the Modbus register level.
 *
 * Register reference (Gen3 hybrid mock, extended schedule slots):
 *   HR 20  = enable_charge_target
 *   HR 94  = charge_slot_1_start
 *   HR 95  = charge_slot_1_end
 *   HR 96  = enable_charge
 *   HR 116 = charge_target_soc (global)
 *   HR 242 = charge_slot_1_target_soc (extended per-slot)
 */

import { test, expect } from './fixture.js';
import { startBackend, stopBackend } from './backend.js';
import type { RegisterWrite } from './mock-modbus.js';

// Each spec file runs against a FRESH backend instance so backend-internal
// state (detected device type, armed slots, battery-mode state machine) can't
// leak between spec files. See e2e/backend.ts.
test.beforeAll(async () => {
  await startBackend();
});
test.afterAll(async () => {
  await stopBackend();
});

// ---------------------------------------------------------------------------
// Helpers (same pattern as control.spec.ts)
// ---------------------------------------------------------------------------

function findWrite(writes: RegisterWrite[], address: number): RegisterWrite | undefined {
  return writes.find((w) => w.address === address);
}

/** Wait until a write to `address` with `value` has landed, then drain. */
async function waitForWrite(
  peekWrites: () => Promise<RegisterWrite[]>,
  drainWrites: () => Promise<RegisterWrite[]>,
  address: number,
  value: number,
  timeoutMs = 30_000,
): Promise<RegisterWrite[]> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const writes = await peekWrites();
    if (writes.some((w) => w.address === address && w.value === value)) {
      return drainWrites();
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  return drainWrites();
}

/** Poll the REST snapshot until target_soc reaches `value` (or timeout). */
async function waitForSnapshotTarget(baseUrl: string, value: number, timeoutMs = 25_000): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const resp = await fetch(`${baseUrl}/api/snapshot`);
      const body = await resp.json();
      if (body?.data?.target_soc === value) return;
    } catch { /* backend restarting between polls */ }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`snapshot target_soc never reached ${value} within ${timeoutMs}ms`);
}

/** Set a React-controlled range input value (native setter + input event). */
async function setRangeValue(locator: import('@playwright/test').Locator, value: string): Promise<void> {
  await locator.evaluate((el: HTMLInputElement, v: string) => {
    const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value')?.set;
    if (!setter) throw new Error('no value setter');
    setter.call(el, v);
    el.dispatchEvent(new Event('input', { bubbles: true }));
  }, value);
}

test.describe('Charge slot target SOC regression', () => {
  test('GUI arms 99, automated re-post without target_soc preserves it', async ({
    page,
    baseUrl,
    setHoldingReg,
    peekModbusWrites,
    drainModbusWrites,
  }) => {
    // Seed: slot 1 armed 06:00-10:00 with no explicit target (HR116=100).
    await setHoldingReg(94, 600);   // 06:00
    await setHoldingReg(95, 1000);  // 10:00
    await setHoldingReg(96, 1);     // enable_charge
    await setHoldingReg(116, 100);  // no limit
    await new Promise((r) => setTimeout(r, 6_000)); // let a poll cycle decode it
    await drainModbusWrites();

    // --- GUI: set Target SOC 99 on charge slot 1 and save ---
    await page.goto('/#/control');
    const chargeSection = page.locator('section').filter({
      has: page.getByRole('heading', { name: 'Charge Schedule', exact: true }),
    });
    await expect(chargeSection).toBeVisible({ timeout: 15_000 });
    const slot1Card = chargeSection.locator('div.bg-bg-surface').first();
    const slider = slot1Card.locator('input[type="range"]').first();
    await expect(slider).toBeVisible({ timeout: 10_000 });
    await setRangeValue(slider, '99');
    await slot1Card.getByRole('button', { name: 'Save', exact: true }).click();

    // The save posts the full slot: HR116=99 (global target), HR242=99
    // (per-slot), HR96=1, plus HR94/95 times. Wait on the trailing per-slot
    // write (HR242) so the full set has landed before draining.
    const armWrites = await waitForWrite(peekModbusWrites, drainModbusWrites, 242, 99);
    expect(findWrite(armWrites, 116)!.value).toBe(99);
    expect(findWrite(armWrites, 242)!.value).toBe(99);
    expect(findWrite(armWrites, 96)!.value).toBe(1);

    // Wait until the poll loop has decoded the armed target back into the
    // snapshot — the preserve-on-omit fix reads this snapshot value.
    await waitForSnapshotTarget(baseUrl, 99);
    await drainModbusWrites();

    // --- Automated re-post of the same slot WITHOUT target_soc ---
    // (What the field report caught: schedule re-asserted after a poll blip.)
    const resp = await fetch(`${baseUrl}/api/control/charge-slot`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        slot: 1,
        enabled: true,
        start_hour: 6,
        start_minute: 0,
        end_hour: 10,
        end_minute: 0,
        // target_soc deliberately omitted
      }),
    });
    expect((await resp.json()).ok).toBe(true);

    // The armed 99 must be RE-WRITTEN, not disarmed. Pre-fix behaviour was:
    // default 100 -> "no limit" branch -> HR116 never written -> charge to full.
    // Wait on the trailing HR242 write so the full set has landed.
    const rePostWrites = await waitForWrite(peekModbusWrites, drainModbusWrites, 242, 99);
    expect(findWrite(rePostWrites, 116)!.value).toBe(99);
    expect(findWrite(rePostWrites, 242)!.value).toBe(99);
    expect(findWrite(rePostWrites, 96)!.value).toBe(1);

    // And the GUI still shows 99% after a fresh snapshot round-trip.
    await page.reload();
    await expect(chargeSection).toBeVisible({ timeout: 15_000 });
    await expect(slot1Card.locator('span.font-mono').first()).toHaveText('99%', { timeout: 15_000 });
  });

  test('GUI explicit 100 keeps "no limit" semantics (no HR116 write)', async ({
    page,
    baseUrl,
    peekModbusWrites,
    drainModbusWrites,
  }) => {
    // Continuing state from the previous test: slot 1 armed, target 99.
    await waitForSnapshotTarget(baseUrl, 99);
    await drainModbusWrites();

    // --- GUI: set Target SOC 100 on charge slot 1 and save ---
    await page.goto('/#/control');
    const chargeSection = page.locator('section').filter({
      has: page.getByRole('heading', { name: 'Charge Schedule', exact: true }),
    });
    await expect(chargeSection).toBeVisible({ timeout: 15_000 });
    const slot1Card = chargeSection.locator('div.bg-bg-surface').first();
    const slider = slot1Card.locator('input[type="range"]').first();
    await expect(slider).toBeVisible({ timeout: 10_000 });
    await setRangeValue(slider, '100');
    await slot1Card.getByRole('button', { name: 'Save', exact: true }).click();

    // Explicit 100 = charge to full: the global HR116 must NOT be written
    // (GivTCP-compatible "no limit"), the force flag is cleared and the
    // per-slot target still records 100. Wait on the trailing HR242 write.
    const writes = await waitForWrite(peekModbusWrites, drainModbusWrites, 242, 100);
    expect(findWrite(writes, 116), 'HR116 must not be written for explicit 100').toBeUndefined();
    expect(findWrite(writes, 20)!.value).toBe(0);
    expect(findWrite(writes, 96)!.value).toBe(1);
  });

  test('omitted target_soc with no armed target keeps the old no-limit default', async ({
    baseUrl,
    setHoldingReg,
    peekModbusWrites,
    drainModbusWrites,
  }) => {
    // Reset to "no armed target": HR116 back to 100 and let the poll see it.
    await setHoldingReg(116, 100);
    await waitForSnapshotTarget(baseUrl, 100);
    await drainModbusWrites();

    const resp = await fetch(`${baseUrl}/api/control/charge-slot`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        slot: 1,
        enabled: true,
        start_hour: 6,
        start_minute: 0,
        end_hour: 10,
        end_minute: 0,
        // target_soc omitted, nothing armed -> default no-limit (old behaviour)
      }),
    });
    expect((await resp.json()).ok).toBe(true);

    const writes = await waitForWrite(peekModbusWrites, drainModbusWrites, 96, 1);
    expect(findWrite(writes, 116), 'no armed target: HR116 must stay untouched').toBeUndefined();
    expect(findWrite(writes, 96)!.value).toBe(1);
  });
});
