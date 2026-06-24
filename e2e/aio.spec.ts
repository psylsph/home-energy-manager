/**
 * E2E tests for AIO (All-in-One) specific behaviour.
 *
 * These tests verify API register routing for DC-hybrid devices. The mock
 * server defaults to Gen3Hybrid (also a DC-hybrid device), so charge/discharge
 * limits route to HR 111/112 — the same registers AIO uses. The device type
 * is cached after the first poll, so these tests verify the WRITE routing
 * via drainModbusWrites rather than asserting the device type in the snapshot.
 *
 *   - Charge/discharge limits write HR 111/112 (DC hybrid), not HR 313/314 (AC)
 *   - Charge slot disable writes the correct registers
 *   - 10 charge/discharge slots are shown (extended HR 240-299 block)
 */

import { test, expect } from './fixture.js';

test.describe('AIO - API routing', () => {
  test('charge rate writes HR 111 (DC hybrid) not HR 313 (AC)', async ({ baseUrl, drainModbusWrites }) => {
    // Drain any writes from setup.
    await drainModbusWrites();

    const resp = await fetch(`${baseUrl}/api/control/charge-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ limit: 30 }),
    });
    expect((await resp.json()).ok).toBe(true);

    // The poll loop drains pending writes on the next cycle. Wait for it.
    await new Promise((r) => setTimeout(r, 6_000));
    const writes = await drainModbusWrites();
    // DC-hybrid routing: HR 111 (charge power limit), NOT HR 313 (AC).
    const hr111 = writes.find((w) => w.address === 111);
    const hr313 = writes.find((w) => w.address === 313);
    expect(hr111, 'HR 111 should be written for DC-hybrid charge rate').toBeDefined();
    expect(hr313, 'HR 313 should NOT be written (AC-only register)').toBeUndefined();
  });

  test('discharge rate writes HR 112 (DC hybrid) not HR 314 (AC)', async ({ baseUrl, drainModbusWrites }) => {
    await drainModbusWrites();

    const resp = await fetch(`${baseUrl}/api/control/discharge-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ limit: 25 }),
    });
    expect((await resp.json()).ok).toBe(true);

    await new Promise((r) => setTimeout(r, 6_000));
    const writes = await drainModbusWrites();
    // DC-hybrid routing: HR 112 (discharge power limit), NOT HR 314 (AC).
    const hr112 = writes.find((w) => w.address === 112);
    const hr314 = writes.find((w) => w.address === 314);
    expect(hr112, 'HR 112 should be written for DC-hybrid discharge rate').toBeDefined();
    expect(hr314, 'HR 314 should NOT be written (AC-only register)').toBeUndefined();
  });

  test('charge slot disable writes the correct registers', async ({ baseUrl, setHoldingReg, drainModbusWrites }) => {
    // Set up initial state: slot 1 configured and enabled
    await setHoldingReg(94, 600);  // slot 1 start 06:00
    await setHoldingReg(95, 1000); // slot 1 end 10:00
    await setHoldingReg(96, 1);    // enable_charge = true
    await new Promise((r) => setTimeout(r, 6_000));

    // Drain setup writes, then disable slot 1 via API
    await drainModbusWrites();
    const resp = await fetch(`${baseUrl}/api/control/charge-slot`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        slot: 1,
        start_hour: 6, start_minute: 0,
        end_hour: 10, end_minute: 0,
        enabled: false,
      }),
    });
    expect((await resp.json()).ok).toBe(true);

    // Wait for the poll loop to send the disable writes
    await new Promise((r) => setTimeout(r, 6_000));
    const writes = await drainModbusWrites();
    // Disabling slot 1 clears HR 94/95 to 0 (the slot window).
    const hr94 = writes.find((w) => w.address === 94);
    expect(hr94, 'HR 94 (charge slot 1 start) should be written').toBeDefined();
  });

  test('shows 10 charge slots', async ({ baseUrl }) => {
    const snapResp = await fetch(`${baseUrl}/api/snapshot`);
    const snap = await snapResp.json();
    expect(snap.ok).toBe(true);
    expect(snap.data.max_charge_slots).toBe(10);
    expect(snap.data.charge_slots.length).toBe(10);
  });

  test('shows 10 discharge slots', async ({ baseUrl }) => {
    const snapResp = await fetch(`${baseUrl}/api/snapshot`);
    const snap = await snapResp.json();
    expect(snap.ok).toBe(true);
    expect(snap.data.max_discharge_slots).toBe(10);
    expect(snap.data.discharge_slots.length).toBe(10);
  });
});
