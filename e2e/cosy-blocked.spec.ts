/**
 * Browser and full backend/Modbus coverage for a Cosy (and Agile) schedule
 * that is held back by another owner of the inverter (issue #366).
 *
 * What these tests pin, end to end against the real backend and the mock
 * Modbus server:
 *   - A manual Force action is a hard override: while a Cosy slot is running,
 *     the snapshot reports the block, the app-wide banner appears on every
 *     page, the Control and Inverter pages say "on hold", and exactly ONE
 *     notification reaches the configured channel (not one per poll).
 *   - The block clearing resumes Cosy (its charge slot is written) and the
 *     banner and notices go away.
 *   - A block that is not near a slot gets the on-page notice but neither the
 *     banner nor a notification.
 *   - A deliberate Timed Demand or an armed export (soft owners) no longer
 *     stops Cosy setting the night's charge slot - the original bug.
 *   - Agile reports when a hard override stops a charge it wanted.
 *
 * Notifications are captured by a local HTTP server standing in for ntfy.
 */

import { createServer, type IncomingMessage, type Server } from 'http';
import type { AddressInfo } from 'net';
import { test, expect } from './fixture.js';
import { startBackend, stopBackend } from './backend.js';
import type { RegisterWrite } from './mock-modbus.js';

// Input register 59: battery state of charge (%).
const IR_BATTERY_PERCENT = 59;
const HR_BATTERY_POWER_MODE = 27;
const HR_DISCHARGE_SLOT_1_START = 56;
const HR_DISCHARGE_SLOT_1_END = 57;
const HR_ENABLE_DISCHARGE = 59;
const HR_CHARGE_SLOT_1_START = 94;
const HR_CHARGE_SLOT_1_END = 95;
const HR_ENABLE_CHARGE = 96;

const wait = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

// ---------------------------------------------------------------------------
// Local stand-ins for the outside world
// ---------------------------------------------------------------------------

interface NtfyCapture {
  server: Server;
  baseUrl: string;
  bodies: string[];
  topics: string[];
}

/**
 * The Cosy notifications received. The mock inverter has no battery on the
 * BMS address, so unrelated alerts (battery connection lost) can arrive on
 * the same channel; only the Cosy ones are under test.
 */
const cosyAlerts = (ntfy: NtfyCapture) =>
  ntfy.bodies.filter((body) => body.includes('Cosy Charging Held Back'));

/** A local ntfy: records every POSTed notification body and its topic. */
async function startNtfyCapture(): Promise<NtfyCapture> {
  const bodies: string[] = [];
  const topics: string[] = [];
  const server = createServer((req: IncomingMessage, res) => {
    const chunks: Buffer[] = [];
    req.on('data', (chunk: Buffer) => chunks.push(chunk));
    req.on('end', () => {
      topics.push((req.url ?? '').replace(/^\//, ''));
      bodies.push(Buffer.concat(chunks).toString('utf8'));
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end('{}');
    });
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const addr = server.address() as AddressInfo;
  return { server, baseUrl: `http://127.0.0.1:${addr.port}`, bodies, topics };
}

/** Mock Octopus Agile rates: the current half-hours cost `price`, the rest 20p. */
async function startMockOctopus(price: number): Promise<{ server: Server; baseUrl: string }> {
  const server = createServer((_req, res) => {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    const slotMs = 30 * 60_000;
    const currentSlotStart = Math.floor(Date.now() / slotMs) * slotMs;
    const results: Array<Record<string, unknown>> = [];
    for (let i = 191; i >= 0; i--) {
      const from = currentSlotStart - 48 * slotMs + i * slotMs;
      const index = (from - currentSlotStart) / slotMs;
      results.push({
        value_inc_vat: index >= 0 && index < 2 ? price : 20.0,
        valid_from: new Date(from).toISOString(),
        valid_to: new Date(from + slotMs).toISOString(),
      });
    }
    res.end(JSON.stringify({ count: results.length, results }));
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const addr = server.address() as AddressInfo;
  return { server, baseUrl: `http://127.0.0.1:${addr.port}` };
}

const closeServer = (server: Server) =>
  new Promise<void>((resolve) => server.close(() => resolve()));

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async function postJson(baseUrl: string, path: string, body: unknown): Promise<any> {
  const response = await fetch(`${baseUrl}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  return response.json().catch(() => ({}));
}

async function snapshot(baseUrl: string): Promise<Record<string, any>> {
  const response = await fetch(`${baseUrl}/api/snapshot`);
  const payload = await response.json();
  return payload.data;
}

async function waitForSnapshot(
  baseUrl: string,
  predicate: (value: Record<string, any>) => boolean,
  label: string,
  timeoutMs = 45_000,
) {
  await expect
    .poll(async () => predicate(await snapshot(baseUrl)), {
      message: label,
      timeout: timeoutMs,
      intervals: [500, 1_000, 2_000],
    })
    .toBe(true);
}

/**
 * Wait until the backend has completed `count` more poll cycles, judged by
 * the snapshot timestamp advancing. Used before asserting that something did
 * NOT happen, so the assertion rests on observed polls, not a guessed delay.
 */
async function waitForPolls(baseUrl: string, count: number, timeoutMs = 60_000) {
  const seen = new Set<unknown>([(await snapshot(baseUrl)).timestamp]);
  await expect
    .poll(
      async () => {
        seen.add((await snapshot(baseUrl)).timestamp);
        return seen.size - 1;
      },
      { message: `${count} more poll cycles`, timeout: timeoutMs, intervals: [500, 1_000] },
    )
    .toBeGreaterThanOrEqual(count);
}

/** Minutes since midnight, `offset` minutes from now, wrapped to a day. */
function minuteOffset(offset: number): { hour: number; minute: number } {
  const now = new Date();
  const total = (((now.getHours() * 60 + now.getMinutes() + offset) % 1440) + 1440) % 1440;
  return { hour: Math.floor(total / 60), minute: total % 60 };
}

/** One enabled Cosy slot from `fromOffset` to `toOffset` minutes from now. */
function cosySlot(fromOffset: number, toOffset: number) {
  const start = minuteOffset(fromOffset);
  const end = minuteOffset(toOffset);
  return {
    enabled: true,
    start_hour: start.hour,
    start_minute: start.minute,
    end_hour: end.hour,
    end_minute: end.minute,
    target_soc: 90,
  };
}

const idleSlot = {
  enabled: false,
  start_hour: 0,
  start_minute: 0,
  end_hour: 0,
  end_minute: 0,
  target_soc: 100,
};

async function enableCosy(baseUrl: string, slot: ReturnType<typeof cosySlot>) {
  const result = await postJson(baseUrl, '/api/cosy', {
    enabled: true,
    slots: [slot, idleSlot, idleSlot],
  });
  expect(result.ok).toBe(true);
}

async function startForceCharge(baseUrl: string) {
  const result = await postJson(baseUrl, '/api/control/force-charge', { minutes: 60 });
  expect(result.ok).toBe(true);
}

async function startForceDischarge(baseUrl: string) {
  const result = await postJson(baseUrl, '/api/control/force-discharge', { minutes: 60 });
  expect(result.ok).toBe(true);
}

async function configureAlerts(baseUrl: string, ntfy: NtfyCapture, enabled = true) {
  const result = await postJson(baseUrl, '/api/alerts', {
    enabled,
    ntfy_topic: 'hem-e2e',
    ntfy_server: ntfy.baseUrl,
    cooldown_minutes: 1,
  });
  expect(result.ok).toBe(true);
}


function hasWrite(writes: RegisterWrite[], address: number, value: number) {
  return writes.some((write) => write.address === address && write.value === value);
}

async function drainUntil(
  drain: () => Promise<RegisterWrite[]>,
  predicate: (writes: RegisterWrite[]) => boolean,
  timeoutMs = 60_000,
): Promise<RegisterWrite[]> {
  const deadline = Date.now() + timeoutMs;
  let all: RegisterWrite[] = [];
  while (Date.now() < deadline) {
    all = [...all, ...(await drain())];
    if (predicate(all)) return all;
    await wait(500);
  }
  return all;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test.beforeAll(async () => {
  await startBackend();
});
test.afterAll(async () => {
  await stopBackend();
});

test.describe.serial('Cosy held back by another owner', () => {
  test.setTimeout(150_000);
  let ntfy: NtfyCapture;
  let octopus: { server: Server; baseUrl: string } | null = null;

  test.beforeEach(async () => {
    ntfy = await startNtfyCapture();
  });

  test.afterEach(async ({ baseUrl, drainModbusWrites }) => {
    // Leave nothing armed for the next test: stop the Force action, switch
    // Cosy and Agile off, silence alerts, and let the clear writes drain.
    await fetch(`${baseUrl}/api/control/force-charge/stop`, { method: 'POST' }).catch(() => {});
    await fetch(`${baseUrl}/api/control/force-discharge/stop`, { method: 'POST' }).catch(() => {});
    await postJson(baseUrl, '/api/cosy', { enabled: false, slots: [idleSlot, idleSlot, idleSlot] });
    await postJson(baseUrl, '/api/agile', { scope: 'off', api_base_url: '' });
    await postJson(baseUrl, '/api/alerts', { enabled: false });
    if (octopus) {
      await closeServer(octopus.server);
      octopus = null;
    }
    await closeServer(ntfy.server);
    await wait(6_000);
    await drainModbusWrites();
  });

  test('a Force action during a Cosy slot raises the banner, the notices and one notification', async ({
    page,
    baseUrl,
  }) => {
    await configureAlerts(baseUrl, ntfy);
    await startForceCharge(baseUrl);
    await enableCosy(baseUrl, cosySlot(-90, 90));

    // The backend reports the block and that a slot is at risk.
    await waitForSnapshot(
      baseUrl,
      (s) => s.cosy_blocked_by === 'force' && s.cosy_slot_at_risk === true,
      'snapshot reports Cosy blocked by a Force action with a slot at risk',
    );

    // The app-wide banner is on every page.
    for (const route of ['/#/', '/#/battery', '/#/control']) {
      await page.goto(route);
      const banner = page.getByRole('alert').filter({ hasText: 'Cosy charging is being held back' });
      await expect(banner).toBeVisible({ timeout: 15_000 });
      await expect(banner).toContainText('Force Charge or Force Discharge');
    }

    // The Control page says Cosy is on hold.
    await page.goto('/#/control');
    await expect(
      page.getByRole('status').filter({ hasText: 'Cosy is on hold' }),
    ).toBeVisible({ timeout: 15_000 });

    // The Inverter page marks the Cosy row.
    await page.goto('/#/inverter');
    const cosyRow = page.locator('span', { hasText: /^Cosy Mode$/ }).locator('xpath=following-sibling::span[1]');
    await expect(cosyRow).toHaveText('On hold', { timeout: 15_000 });

    // Exactly one notification, however many polls the block lasts.
    await expect.poll(() => cosyAlerts(ntfy).length, { timeout: 30_000 }).toBeGreaterThanOrEqual(1);
    await waitForPolls(baseUrl, 3);
    expect(cosyAlerts(ntfy)).toHaveLength(1);
    expect(new Set(ntfy.topics)).toEqual(new Set(['hem-e2e']));
    const text = cosyAlerts(ntfy)[0];
    expect(text).toContain('Cosy');
    expect(text).toContain('manual Force');
    expect(text).toMatch(/under way/);
    expect(text).toContain('90%');
  });

  test('clearing the block resumes Cosy and removes the banner and notices', async ({
    page,
    baseUrl,
    drainModbusWrites,
  }) => {
    await configureAlerts(baseUrl, ntfy);
    await startForceCharge(baseUrl);
    await enableCosy(baseUrl, cosySlot(-90, 90));
    await waitForSnapshot(
      baseUrl,
      (s) => s.cosy_slot_at_risk === true,
      'Cosy slot at risk while Force is running',
    );
    await page.goto('/#/control');
    await expect(
      page.getByRole('alert').filter({ hasText: 'Cosy charging is being held back' }),
    ).toBeVisible({ timeout: 15_000 });

    await drainModbusWrites();
    const stop = await postJson(baseUrl, '/api/control/force-charge/stop', {});
    expect(stop.ok).toBe(true);

    await waitForSnapshot(
      baseUrl,
      (s) => !s.cosy_blocked_by && s.cosy_slot_at_risk === false,
      'block clears once the Force action stops',
    );
    await expect(
      page.getByRole('alert').filter({ hasText: 'Cosy charging is being held back' }),
    ).toHaveCount(0, { timeout: 15_000 });
    await expect(page.getByText('Cosy is on hold')).toHaveCount(0);

    // Cosy resumes: it writes its slot and enables charging.
    const writes = await drainUntil(
      drainModbusWrites,
      (all) => all.some((w) => w.address === HR_CHARGE_SLOT_1_START)
        && hasWrite(all, HR_ENABLE_CHARGE, 1),
    );
    expect(writes.some((w) => w.address === HR_CHARGE_SLOT_1_START)).toBe(true);
    expect(writes.some((w) => w.address === HR_CHARGE_SLOT_1_END)).toBe(true);
    expect(hasWrite(writes, HR_ENABLE_CHARGE, 1)).toBe(true);
  });

  test('a block far from any slot shows the page notice but no banner and no notification', async ({
    page,
    baseUrl,
  }) => {
    await configureAlerts(baseUrl, ntfy);
    await startForceCharge(baseUrl);
    await enableCosy(baseUrl, cosySlot(180, 240));

    await waitForSnapshot(
      baseUrl,
      (s) => s.cosy_blocked_by === 'force' && s.cosy_slot_at_risk === false,
      'blocked but no slot at risk',
    );
    await page.goto('/#/control');
    await expect(
      page.getByRole('status').filter({ hasText: 'Cosy is on hold' }),
    ).toBeVisible({ timeout: 15_000 });
    await expect(
      page.getByRole('alert').filter({ hasText: 'Cosy charging is being held back' }),
    ).toHaveCount(0);

    await waitForPolls(baseUrl, 3);
    expect(cosyAlerts(ntfy)).toHaveLength(0);
  });

  test('a battery already at the slot target raises no banner and no notification', async ({
    page,
    baseUrl,
    setInputReg,
  }) => {
    // Nothing is lost if Cosy cannot charge a battery that is already full.
    //
    // Deterministic order: the full battery must be what the backend reports
    // BEFORE Cosy, alerts and the blocker exist, or the first poll after
    // Cosy is enabled could still see the old 75% and (correctly) alert. A
    // Force DISCHARGE is the blocker: the sanitizer rejects SOC=100 while the
    // battery is fast-charging, which would keep the old reading alive.
    await setInputReg(IR_BATTERY_PERCENT, 100);
    await waitForSnapshot(baseUrl, (s) => s.soc === 100, 'backend reports a full battery');

    await configureAlerts(baseUrl, ntfy);
    await startForceDischarge(baseUrl);
    await enableCosy(baseUrl, cosySlot(-90, 90));

    await waitForSnapshot(
      baseUrl,
      (s) => s.cosy_blocked_by === 'force' && s.soc === 100,
      'Cosy is blocked while the battery stays full',
    );
    await waitForPolls(baseUrl, 3);
    const state = await snapshot(baseUrl);
    expect(state.cosy_slot_at_risk).toBe(false);

    await page.goto('/#/control');
    await expect(
      page.getByRole('status').filter({ hasText: 'Cosy is on hold' }),
    ).toBeVisible({ timeout: 15_000 });
    await expect(
      page.getByRole('alert').filter({ hasText: 'Cosy charging is being held back' }),
    ).toHaveCount(0);
    expect(cosyAlerts(ntfy)).toHaveLength(0);
  });

  test('a slot about to start raises the alert before the window opens', async ({
    page,
    baseUrl,
  }) => {
    await configureAlerts(baseUrl, ntfy);
    await startForceCharge(baseUrl);
    await enableCosy(baseUrl, cosySlot(5, 120));

    await waitForSnapshot(
      baseUrl,
      (s) => s.cosy_blocked_by === 'force' && s.cosy_slot_at_risk === true,
      'a slot starting in five minutes is at risk',
    );
    await page.goto('/#/status');
    await expect(
      page.getByRole('alert').filter({ hasText: 'Cosy charging is being held back' }),
    ).toBeVisible({ timeout: 15_000 });
    await expect.poll(() => cosyAlerts(ntfy).length, { timeout: 30_000 }).toBe(1);
    expect(cosyAlerts(ntfy)[0]).toMatch(/about to start/);
  });

  test('with alerts switched off the banner still shows but nothing is sent', async ({
    page,
    baseUrl,
  }) => {
    await configureAlerts(baseUrl, ntfy, false);
    await startForceCharge(baseUrl);
    await enableCosy(baseUrl, cosySlot(-90, 90));

    await waitForSnapshot(baseUrl, (s) => s.cosy_slot_at_risk === true, 'slot at risk');
    await page.goto('/#/battery');
    await expect(
      page.getByRole('alert').filter({ hasText: 'Cosy charging is being held back' }),
    ).toBeVisible({ timeout: 15_000 });
    await waitForPolls(baseUrl, 3);
    expect(cosyAlerts(ntfy)).toHaveLength(0);
  });

  // The reporter's night: an export or discharge schedule set up outside HEM
  // (the GivEnergy portal). Neither shape may stop Cosy setting its slot.
  //   Timed Demand: Eco work mode (HR27=1) with discharge enabled (HR59=1) -
  //     the poll loop treats it as a manually chosen mode.
  //   Timed Export: export work mode (HR27=0) with discharge enabled.
  // A discharge slot is set too: HR59 with no slot is the armed-but-slotless
  // state the backend repairs on its own, which is not what a user has.
  for (const shape of [
    { name: 'Timed Demand', workMode: 1, battery_mode: 'timed_demand' },
    { name: 'a Timed Export set up outside HEM', workMode: 0, battery_mode: 'timed_export' },
  ]) {
    test(`${shape.name} no longer stops Cosy setting its charge slot`, async ({
      page,
      baseUrl,
      setHoldingReg,
      drainModbusWrites,
    }) => {
      await configureAlerts(baseUrl, ntfy);
      await setHoldingReg(HR_DISCHARGE_SLOT_1_START, 1600);
      await setHoldingReg(HR_DISCHARGE_SLOT_1_END, 1900);
      await setHoldingReg(HR_BATTERY_POWER_MODE, shape.workMode);
      await setHoldingReg(HR_ENABLE_DISCHARGE, 1);
      await waitForSnapshot(
        baseUrl,
        (s) => s.battery_mode === shape.battery_mode,
        `inverter reads as ${shape.battery_mode}`,
      );
      await drainModbusWrites();

      await enableCosy(baseUrl, cosySlot(-90, 90));

      const writes = await drainUntil(
        drainModbusWrites,
        (all) => all.some((w) => w.address === HR_CHARGE_SLOT_1_START)
          && hasWrite(all, HR_ENABLE_CHARGE, 1),
      );
      expect(hasWrite(writes, HR_ENABLE_CHARGE, 1)).toBe(true);
      await waitForSnapshot(baseUrl, (s) => s.cosy_active === true, 'Cosy is charging');

      const state = await snapshot(baseUrl);
      expect(state.cosy_blocked_by ?? '').toBe('');
      expect(state.cosy_slot_at_risk).toBe(false);

      // No warning anywhere.
      await page.goto('/#/control');
      await expect(page.getByText('Cosy charging is being held back')).toHaveCount(0);
      await expect(page.getByText('Cosy is on hold')).toHaveCount(0);
      expect(cosyAlerts(ntfy)).toHaveLength(0);
    });
  }

  test('Agile reports a charge that a Force action stopped it making', async ({
    page,
    baseUrl,
  }) => {
    octopus = await startMockOctopus(5.0); // well below the 10p charge threshold
    await startForceCharge(baseUrl);
    const agile = await postJson(baseUrl, '/api/agile', {
      scope: 'full',
      api_base_url: octopus.baseUrl,
      charge_threshold: 10,
      discharge_threshold: 30,
    });
    expect(agile.ok).toBe(true);

    await waitForSnapshot(
      baseUrl,
      (s) => s.agile_blocked_by === 'force',
      'Agile reports it is held back by a Force action',
    );

    await page.goto('/#/control');
    await expect(
      page.getByRole('status').filter({ hasText: 'Agile is on hold' }),
    ).toBeVisible({ timeout: 15_000 });
    await page.goto('/#/inverter');
    const agileRow = page.locator('span', { hasText: /^Agile Mode$/ }).locator('xpath=following-sibling::span[1]');
    await expect(agileRow).toHaveText('On hold', { timeout: 15_000 });
  });
});
