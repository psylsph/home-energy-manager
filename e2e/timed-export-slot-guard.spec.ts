/**
 * E2E coverage for the Timed Export/discharge-slot invariant.
 *
 * The mock Modbus server is intentional here: unlike the real simulator it
 * lets us inject the exact inverter states that matter (including HR59=1
 * with no discharge slot) and preserves FC06 writes for read-back assertions.
 * Its `/rearm-hr59` admin toggle additionally emulates Gen3 Hybrid firmware
 * that re-asserts HR59=1 while discharge slots remain programmed — the
 * behaviour issue #289's clear/restore fallback defends against.
 */

import { test, expect } from './fixture.js';
import { startBackend, stopBackend, restartBackendPreservingState } from './backend.js';
import type { RegisterWrite } from './mock-modbus.js';

const HR_BATTERY_POWER_MODE = 27;
const HR_DISCHARGE_SLOT_1_START = 56;
const HR_DISCHARGE_SLOT_1_END = 57;
const HR_ENABLE_DISCHARGE = 59;
const HR_BATTERY_PAUSE_MODE = 318;
const HR_BATTERY_PAUSE_SLOT_START = 319;
const HR_BATTERY_PAUSE_SLOT_END = 320;

const wait = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** HHMM-encode (now + offsetMinutes), wrapping past midnight. */
function hhmmOffset(offsetMinutes: number): number {
  const now = new Date();
  const total = now.getHours() * 60 + now.getMinutes() + offsetMinutes;
  const wrapped = ((total % 1440) + 1440) % 1440;
  const hours = Math.floor(wrapped / 60);
  const minutes = wrapped % 60;
  return hours * 100 + minutes;
}

async function getSnapshot(baseUrl: string): Promise<Record<string, any>> {
  const response = await fetch(`${baseUrl}/api/snapshot`);
  const body = await response.json();
  if (!body.ok) throw new Error(`snapshot unavailable: ${JSON.stringify(body)}`);
  return body.data as Record<string, any>;
}

async function postJson(
  baseUrl: string,
  path: string,
  body: Record<string, unknown>,
): Promise<{ status: number; body: Record<string, any> }> {
  const response = await fetch(`${baseUrl}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  return { status: response.status, body: await response.json() };
}

async function waitForSnapshot(
  baseUrl: string,
  predicate: (snapshot: Record<string, any>) => boolean,
  label: string,
  timeoutMs = 20_000,
): Promise<Record<string, any>> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const snapshot = await getSnapshot(baseUrl);
      if (predicate(snapshot)) return snapshot;
    } catch {
      // The backend may be between poll cycles.
    }
    await wait(250);
  }
  throw new Error(`${label} was not observed within ${timeoutMs}ms`);
}

async function waitForWrite(
  peekWrites: () => Promise<RegisterWrite[]>,
  address: number,
  value: number,
  timeoutMs = 20_000,
): Promise<RegisterWrite[]> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const writes = await peekWrites();
    if (writes.some((write) => write.address === address && write.value === value)) {
      return writes;
    }
    await wait(250);
  }
  return peekWrites();
}

/** Wait until every (address, value) pair has landed in the write log.
 *
 * The poll-loop heal and the last-slot-disable path write HR59=0 and then
 * HR27=1 with a ~1.5s gap; waiting on the first alone would return a partial
 * batch. This waits until the trailing write is observed too. */
async function waitForAllWrites(
  peekWrites: () => Promise<RegisterWrite[]>,
  pairs: Array<[number, number]>,
  timeoutMs = 20_000,
): Promise<RegisterWrite[]> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const writes = await peekWrites();
    if (pairs.every(([address, value]) =>
      writes.some((write) => write.address === address && write.value === value),
    )) {
      return writes;
    }
    await wait(250);
  }
  return peekWrites();
}

function writeIndex(writes: RegisterWrite[], address: number, value: number): number {
  return writes.findIndex((write) => write.address === address && write.value === value);
}

async function resetToEco(
  baseUrl: string,
  resetModbus: () => Promise<void>,
  drainModbusWrites: () => Promise<RegisterWrite[]>,
): Promise<void> {
  const resetAt = Date.now();
  await resetModbus();
  // First wait for a snapshot captured AFTER the reset. A stale pre-reset
  // snapshot can satisfy the Eco check below (the previous test may have
  // ended with enable_discharge=false), and acting on it races the poll
  // loop — e.g. an enable POST computing its arm decision against the
  // previous test's slot registers.
  await waitForSnapshot(
    baseUrl,
    (snapshot) => {
      const ts = snapshot.timestamp as number | undefined;
      return typeof ts === 'number' && ts * 1000 > resetAt;
    },
    'post-reset poll',
  );
  await waitForSnapshot(
    baseUrl,
    (snapshot) =>
      snapshot.enable_discharge === false
      && snapshot.discharge_slots?.every((slot: { enabled: boolean }) => !slot.enabled) === true,
    'clean Eco snapshot',
  );
  await drainModbusWrites();
}

test.beforeAll(async () => {
  await startBackend();
});

test.afterAll(async () => {
  await stopBackend();
});

test.describe('Timed Export/discharge-slot state alignment', () => {
  test('rejects enabling Timed Export when no discharge slot exists', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
  }) => {
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);

    const result = await postJson(baseUrl, '/api/control/timed-export', { enabled: true });
    expect(result.status).toBe(409);
    expect(result.body.ok).toBe(false);
    expect(result.body.error).toContain('Configure at least one discharge slot');

    const writes = await drainModbusWrites();
    expect(writes.some((write) => write.address === HR_ENABLE_DISCHARGE && write.value === 1)).toBe(false);
  });

  test('arms Timed Export when the configured slot contains now and preserves order', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
    setHoldingReg,
    peekModbusWrites,
  }) => {
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);
    // A window that always contains "now" (±90 min, midnight-wrapping) so the
    // arming decision is deterministic regardless of when the test runs.
    // Issue #289: entry writes are only queued when the current time is inside
    // an enabled window — a fixed 16:00-19:00 slot would arm only between 16:00
    // and 19:00 local time and fail (or flakily pass) outside it.
    await setHoldingReg(HR_DISCHARGE_SLOT_1_START, hhmmOffset(-90));
    await setHoldingReg(HR_DISCHARGE_SLOT_1_END, hhmmOffset(90));
    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.discharge_slots?.[0]?.enabled === true,
      'configured discharge slot',
    );
    await drainModbusWrites();

    const result = await postJson(baseUrl, '/api/control/timed-export', { enabled: true });
    expect(result.status).toBe(200);
    expect(result.body.ok).toBe(true);

    const writes = await waitForWrite(peekModbusWrites, HR_ENABLE_DISCHARGE, 1);
    const hr27 = writeIndex(writes, HR_BATTERY_POWER_MODE, 0);
    const hr59 = writeIndex(writes, HR_ENABLE_DISCHARGE, 1);
    expect(hr27).toBeGreaterThanOrEqual(0);
    expect(hr59).toBeGreaterThan(hr27);

    const armed = await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.enable_discharge === true && snapshot.discharge_slots?.[0]?.enabled === true,
      'armed Timed Export snapshot',
    );
    expect(armed.battery_power_mode).toBe(0);
  });

  test('saving a future Timed Export slot does NOT immediately write HR27=0/HR59=1', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
    setHoldingReg,
  }) => {
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);
    // A window that always lies in the future (+90..+180 min) so Eco stays
    // the baseline and no HR27=0/HR59=1 entry writes are queued (issue #289).
    await setHoldingReg(HR_DISCHARGE_SLOT_1_START, hhmmOffset(90));
    await setHoldingReg(HR_DISCHARGE_SLOT_1_END, hhmmOffset(180));
    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.discharge_slots?.[0]?.enabled === true,
      'configured discharge slot',
    );
    await drainModbusWrites();

    const result = await postJson(baseUrl, '/api/control/timed-export', { enabled: true });
    expect(result.status).toBe(200);
    expect(result.body.ok).toBe(true);

    // Give the poll loop a couple of cycles; the entry writes must never land.
    await wait(11_000);
    const writes = await drainModbusWrites();
    expect(writes.some((write) => write.address === HR_BATTERY_POWER_MODE && write.value === 0)).toBe(false);
    expect(writes.some((write) => write.address === HR_ENABLE_DISCHARGE && write.value === 1)).toBe(false);

    // The desired schedule is persisted for the boundary state machine.
    const snapshot = await waitForSnapshot(
      baseUrl,
      (s) => s.enable_discharge === false && s.battery_power_mode === 1,
      'Eco baseline with future slot',
    );
    expect(snapshot.discharge_slots?.[0]?.enabled).toBe(true);
  });

  test('automatically repairs an externally-created HR59/no-slot state', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
    setHoldingReg,
    peekModbusWrites,
  }) => {
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);
    await setHoldingReg(HR_BATTERY_POWER_MODE, 0);
    await setHoldingReg(HR_ENABLE_DISCHARGE, 1);
    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.enable_discharge === true && snapshot.discharge_slots?.every((slot: any) => !slot.enabled),
      'inconsistent HR59/no-slot snapshot',
    );

    const writes = await waitForAllWrites(peekModbusWrites, [
      [HR_ENABLE_DISCHARGE, 0],
      [HR_BATTERY_POWER_MODE, 1],
    ]);
    expect(writes.some((write) => write.address === HR_BATTERY_POWER_MODE && write.value === 1)).toBe(true);
    expect(writes.some((write) => write.address === HR_ENABLE_DISCHARGE && write.value === 0)).toBe(true);

    const repaired = await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.enable_discharge === false && snapshot.battery_power_mode === 1,
      'repaired Eco snapshot',
    );
    expect(repaired.discharge_slots?.every((slot: any) => !slot.enabled)).toBe(true);
  });

  test('disabling the last slot returns an armed inverter to Eco', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
    setHoldingReg,
    peekModbusWrites,
  }) => {
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);
    await setHoldingReg(HR_DISCHARGE_SLOT_1_START, 1600);
    await setHoldingReg(HR_DISCHARGE_SLOT_1_END, 1900);
    await setHoldingReg(HR_BATTERY_POWER_MODE, 0);
    await setHoldingReg(HR_ENABLE_DISCHARGE, 1);
    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.enable_discharge === true && snapshot.discharge_slots?.[0]?.enabled === true,
      'armed snapshot with one slot',
    );
    await drainModbusWrites();

    const result = await postJson(baseUrl, '/api/control/discharge-slot', {
      slot: 1,
      enabled: false,
      start_hour: 16,
      start_minute: 0,
      end_hour: 19,
      end_minute: 0,
    });
    expect(result.status).toBe(200);

    const writes = await waitForAllWrites(peekModbusWrites, [
      [HR_DISCHARGE_SLOT_1_START, 0],
      [HR_DISCHARGE_SLOT_1_END, 0],
      [HR_ENABLE_DISCHARGE, 0],
      [HR_BATTERY_POWER_MODE, 1],
    ]);
    expect(writes.some((write) => write.address === HR_DISCHARGE_SLOT_1_START && write.value === 0)).toBe(true);
    expect(writes.some((write) => write.address === HR_DISCHARGE_SLOT_1_END && write.value === 0)).toBe(true);
    expect(writes.some((write) => write.address === HR_BATTERY_POWER_MODE && write.value === 1)).toBe(true);

    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.enable_discharge === false && snapshot.battery_power_mode === 1,
      'Eco snapshot after last-slot disable',
    );
  });

  test('does not repair a valid armed state while a slot remains configured', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
    setHoldingReg,
  }) => {
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);
    await setHoldingReg(HR_DISCHARGE_SLOT_1_START, 1600);
    await setHoldingReg(HR_DISCHARGE_SLOT_1_END, 1900);
    await setHoldingReg(HR_BATTERY_POWER_MODE, 0);
    await setHoldingReg(HR_ENABLE_DISCHARGE, 1);
    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.enable_discharge === true && snapshot.discharge_slots?.[0]?.enabled === true,
      'valid armed snapshot',
    );
    await drainModbusWrites();

    await wait(11_000);
    const writes = await drainModbusWrites();
    expect(writes.some((write) => write.address === HR_ENABLE_DISCHARGE && write.value === 0)).toBe(false);
    expect(writes.some((write) => write.address === HR_BATTERY_POWER_MODE && write.value === 1)).toBe(false);
  });

  test('locks the real control page when the polled inverter has no slot', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
    page,
  }) => {
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);
    await page.goto('/#/control');
    const button = page.getByRole('button', { name: /Timed Export/ });
    await expect(button).toBeVisible();
    await expect(button).toBeDisabled();
    await expect(
      page.getByText('Configure at least one discharge slot before enabling Timed Export.'),
    ).toBeVisible();
  });

  test('HR318 pause blocks entry even inside the export window', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
    setHoldingReg,
  }) => {
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);
    // Export window covering "now" (±90 min) so in-window entry WOULD fire —
    // were it not for the pause.
    await setHoldingReg(HR_DISCHARGE_SLOT_1_START, hhmmOffset(-90));
    await setHoldingReg(HR_DISCHARGE_SLOT_1_END, hhmmOffset(90));
    // Pause Discharge (HR318=2) with a pause window covering "now" as well.
    await setHoldingReg(HR_BATTERY_PAUSE_MODE, 2);
    await setHoldingReg(HR_BATTERY_PAUSE_SLOT_START, hhmmOffset(-90));
    await setHoldingReg(HR_BATTERY_PAUSE_SLOT_END, hhmmOffset(90));
    await waitForSnapshot(
      baseUrl,
      (snapshot) =>
        snapshot.discharge_slots?.[0]?.enabled === true
        && snapshot.battery_pause_mode === 2,
      'in-window slot + active pause',
    );
    await drainModbusWrites();

    const result = await postJson(baseUrl, '/api/control/timed-export', { enabled: true });
    expect(result.status).toBe(200);
    expect(result.body.ok).toBe(true);

    // The boundary machine must NOT write entry (HR27=0/HR59=1) while the
    // pause is blocking discharge — issue #289: pause takes precedence.
    await wait(11_000);
    const writes = await drainModbusWrites();
    expect(writes.some((write) => write.address === HR_BATTERY_POWER_MODE && write.value === 0)).toBe(false);
    expect(writes.some((write) => write.address === HR_ENABLE_DISCHARGE && write.value === 1)).toBe(false);
  });

  test('the desired schedule survives a backend restart', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
    setHoldingReg,
  }) => {
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);
    // A future window so arming never happens; only the schedule is persisted.
    await setHoldingReg(HR_DISCHARGE_SLOT_1_START, hhmmOffset(90));
    await setHoldingReg(HR_DISCHARGE_SLOT_1_END, hhmmOffset(180));
    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.discharge_slots?.[0]?.enabled === true,
      'configured discharge slot',
    );
    await drainModbusWrites();

    const result = await postJson(baseUrl, '/api/control/timed-export', { enabled: true });
    expect(result.status).toBe(200);

    const schedule = await getTimedExport(baseUrl);
    expect(schedule.schedule_enabled).toBe(true);

    // Restart the backend preserving settings: the persisted schedule must
    // reload into the poll-loop config and stay visible (issue #289).
    await restartBackendPreservingState();

    const reloaded = await getTimedExport(baseUrl);
    expect(reloaded.schedule_enabled).toBe(true);
  });

  test('Stop disarms re-arming firmware because the fallback clears slots before HR59=0', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
    setHoldingReg,
    setHr59Rearm,
  }) => {
    test.setTimeout(240_000);
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);
    // Emulate Gen3 firmware that re-asserts HR59=1 while discharge slot
    // registers are non-zero (issue #289) — and leave it enabled for the
    // whole test: Stop must still win.
    await setHr59Rearm(true);

    // --- Classify the firmware: three outside-window HR59=1 polls ---
    // A future window: the schedule is enabled but "now" is outside it, so
    // an HR59=1 readback can only come from the (emulated) firmware.
    await setHoldingReg(HR_DISCHARGE_SLOT_1_START, hhmmOffset(90));
    await setHoldingReg(HR_DISCHARGE_SLOT_1_END, hhmmOffset(180));
    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.discharge_slots?.[0]?.enabled === true,
      'future discharge slot',
    );
    await drainModbusWrites();
    const enable = await postJson(baseUrl, '/api/control/timed-export', { enabled: true });
    expect(enable.status).toBe(200);
    await drainModbusWrites();
    await setHoldingReg(HR_ENABLE_DISCHARGE, 1);
    // Three consecutive qualifying polls at poll_interval=5s, plus margin.
    await wait(23_000);
    const classified = await getTimedExport(baseUrl);
    expect(classified.device_rearm_confirmed).toBe(true);
    await drainModbusWrites();

    // --- Arm inside a window that covers "now" ---
    await setHoldingReg(HR_DISCHARGE_SLOT_1_START, hhmmOffset(-90));
    await setHoldingReg(HR_DISCHARGE_SLOT_1_END, hhmmOffset(90));
    await waitForSnapshot(
      baseUrl,
      (snapshot) =>
        snapshot.discharge_slots?.[0]?.enabled === true
        && snapshot.discharge_slots?.[0]?.start_hour === Math.floor(hhmmOffset(-90) / 100),
      'in-window discharge slot',
    );
    const arm = await postJson(baseUrl, '/api/control/discharge-slot', {
      slot: 1,
      enabled: true,
      start_hour: Math.floor(hhmmOffset(-90) / 100),
      start_minute: hhmmOffset(-90) % 100,
      end_hour: Math.floor(hhmmOffset(90) / 100),
      end_minute: hhmmOffset(90) % 100,
      target_soc: 20,
    });
    expect(arm.status).toBe(200);
    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.enable_discharge === true && snapshot.battery_power_mode === 0,
      'armed Timed Export snapshot',
    );
    await drainModbusWrites();

    // --- Stop: the fallback must clear the slots BEFORE the HR59=0 disarm,
    // or the emulated firmware would undo the disarm and the inverter would
    // keep exporting forever. ---
    const disable = await postJson(baseUrl, '/api/control/timed-export', { enabled: false });
    expect(disable.status).toBe(200);

    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.enable_discharge === false && snapshot.battery_power_mode === 1,
      'Eco after Stop against re-arming firmware',
      45_000,
    );

    // And it stays there — with the slots cleared the firmware has nothing
    // to re-arm HR59 from.
    await wait(11_000);
    const settled = await getSnapshot(baseUrl);
    expect(settled.enable_discharge).toBe(false);
    expect(settled.battery_power_mode).toBe(1);
  });

  test('re-arming firmware is classified after three outside-window HR59=1 polls and the learned fallback survives a restart', async ({
    baseUrl,
    resetModbus,
    drainModbusWrites,
    setHoldingReg,
  }) => {
    test.setTimeout(180_000);
    await resetToEco(baseUrl, resetModbus, drainModbusWrites);
    // A future window: the schedule is enabled but "now" is outside it, so
    // an HR59=1 readback can only come from the (emulated) firmware.
    await setHoldingReg(HR_DISCHARGE_SLOT_1_START, hhmmOffset(90));
    await setHoldingReg(HR_DISCHARGE_SLOT_1_END, hhmmOffset(180));
    await waitForSnapshot(
      baseUrl,
      (snapshot) => snapshot.discharge_slots?.[0]?.enabled === true,
      'configured discharge slot',
    );
    await drainModbusWrites();

    const enable = await postJson(baseUrl, '/api/control/timed-export', { enabled: true });
    expect(enable.status).toBe(200);
    await drainModbusWrites();

    // Firmware re-arms HR59 outside the window (the observable the detector
    // classifies on; the mock's write interception needs a client HR59=0
    // write, so seed the readback directly).
    await setHoldingReg(HR_ENABLE_DISCHARGE, 1);

    // Three consecutive qualifying polls at poll_interval=5s, plus margin.
    await wait(23_000);

    const schedule = await getTimedExport(baseUrl);
    expect(schedule.device_rearm_confirmed).toBe(true);
    // The desired schedule must not be lost or hidden by the fallback.
    expect(schedule.schedule_enabled).toBe(true);
    const savedSlot = (schedule.slots as Array<Record<string, unknown>>).find(
      (slot) => (slot as { enabled: boolean }).enabled,
    );
    expect(savedSlot).toBeDefined();

    // The learned classification persists (timed_export_slots_require_clear).
    await restartBackendPreservingState();
    const reloaded = await getTimedExport(baseUrl);
    expect(reloaded.device_rearm_confirmed).toBe(true);
  });
});

/** GET /api/timed-export — the HEM-managed schedule. */
async function getTimedExport(baseUrl: string): Promise<Record<string, any>> {
  const response = await fetch(`${baseUrl}/api/timed-export`);
  const body = await response.json();
  if (!body.ok) throw new Error(`timed-export unavailable: ${JSON.stringify(body)}`);
  return body.data as Record<string, any>;
}
