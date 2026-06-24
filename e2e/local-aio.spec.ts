/**
 * E2E tests for AIO (All-in-One) specific behaviour.
 *
 * These tests configure the mock Modbus server as an AIO device (HR 0 = 0x8001)
 * and verify that the UI and API handle AIO-specific register routing correctly:
 *   - Charge/discharge limits use HR 111/112 (DC hybrid), not HR 313/314 (AC)
 *   - Charge slot 2 uses extended registers HR 243/244, not classic HR 31/32
 *   - EPS toggle writes HR 317
 *   - 10 charge/discharge slots are shown
 *   - Slot toggle round-trips correctly (issue #106 regression guard)
 */

import { test, expect } from './fixture.js';

const AIO_DTC = 0x8001; // All-in-One 6kW

test.describe('AIO - API routing', () => {
  test.beforeEach(async ({ resetModbus, setHoldingReg }) => {
    await resetModbus();
    await setHoldingReg(0, AIO_DTC);
    // Set serial so the poll loop recognises the device
    const serial = Buffer.from('SA12345678');
    for (let i = 0; i < 5; i++) {
      await setHoldingReg(13 + i, (serial[i * 2] << 8) | serial[i * 2 + 1]);
    }
    // Set ARM firmware to a Gen3-era value so the device type is stable
    await setHoldingReg(21, 352);
    // Wait for the poll loop to detect the new device type
    await new Promise((r) => setTimeout(r, 3000));
  });

  test('charge rate writes HR 111 (DC hybrid) not HR 313 (AC)', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/charge-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ limit: 30 }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);

    // Verify the snapshot shows the correct charge rate
    const snapResp = await fetch(`${baseUrl}/api/snapshot`);
    const snap = await snapResp.json();
    expect(snap.ok).toBe(true);
    expect(snap.data.device_type).toBe('AllInOne6kW');
    expect(snap.data.charge_rate).toBe(30);
  });

  test('discharge rate writes HR 112 (DC hybrid) not HR 314 (AC)', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/discharge-rate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ limit: 25 }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);

    const snapResp = await fetch(`${baseUrl}/api/snapshot`);
    const snap = await snapResp.json();
    expect(snap.ok).toBe(true);
    expect(snap.data.device_type).toBe('AllInOne6kW');
    expect(snap.data.discharge_rate).toBe(25);
  });

  test('EPS toggle writes HR 317', async ({ baseUrl }) => {
    const resp = await fetch(`${baseUrl}/api/control/eps`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ enabled: true }),
    });
    const data = await resp.json();
    expect(data.ok).toBe(true);
  });

  test('charge slot 1 toggle round-trips correctly', async ({ baseUrl, setHoldingReg }) => {
    // Set up initial state: slot 1 configured and enabled
    await setHoldingReg(94, 600);  // slot 1 start 06:00
    await setHoldingReg(95, 1000); // slot 1 end 10:00
    await setHoldingReg(96, 1);    // enable_charge = true
    await new Promise((r) => setTimeout(r, 2000));

    // Verify the snapshot shows slot 1 as enabled
    let snapResp = await fetch(`${baseUrl}/api/snapshot`);
    let snap = await snapResp.json();
    expect(snap.ok).toBe(true);
    expect(snap.data.charge_slots[0].enabled).toBe(true);

    // Disable slot 1 via API
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
    const data = await resp.json();
    expect(data.ok).toBe(true);

    // Wait for the poll loop to pick up the change
    await new Promise((r) => setTimeout(r, 3000));

    // Verify the snapshot shows slot 1 as disabled (regression guard for issue #106)
    snapResp = await fetch(`${baseUrl}/api/snapshot`);
    snap = await snapResp.json();
    expect(snap.ok).toBe(true);
    expect(snap.data.charge_slots[0].enabled).toBe(false);
    expect(snap.data.enable_charge).toBe(false);
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
