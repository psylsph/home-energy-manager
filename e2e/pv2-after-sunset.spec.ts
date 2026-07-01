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

async function waitForSnapshot(
  baseUrl: string,
  predicate: (snap: Record<string, unknown>) => boolean,
): Promise<Record<string, unknown>> {
  const deadline = Date.now() + 20_000;
  let last: Record<string, unknown> | null = null;
  while (Date.now() < deadline) {
    const resp = await fetch(`${baseUrl}/api/snapshot`);
    const body = await resp.json() as { ok: boolean; data?: Record<string, unknown> };
    if (body.ok && body.data) {
      last = body.data;
      if (predicate(body.data)) return body.data;
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error(`Timed out waiting for matching snapshot; last=${JSON.stringify(last)}`);
}

test.describe('PV2 daily energy after sunset', () => {
  test('keeps PV2 Energy Today when PV2 voltage has fallen to zero', async ({
    baseUrl,
    resetModbus,
    setInputReg,
  }) => {
    await resetModbus();

    // Model the report in issue #165: after sunset PV2 voltage/current are 0,
    // but the daily PV2 register still contains the day's total and the
    // aggregate PV daily register corroborates PV1+PV2.
    await setInputReg(2, 0); // PV2 voltage
    await setInputReg(9, 0); // PV2 current
    await setInputReg(17, 150); // PV1 today = 15.0 kWh
    await setInputReg(19, 147); // PV2 today = 14.7 kWh
    await setInputReg(44, 300); // aggregate today ~= 30.0 kWh

    const snap = await waitForSnapshot(baseUrl, (s) => (
      Math.abs(Number(s.today_pv1_kwh) - 15.0) < 0.01
      && Math.abs(Number(s.today_pv2_kwh) - 14.7) < 0.01
      && Math.abs(Number(s.today_solar_kwh) - 29.7) < 0.01
    ));

    expect(Number(snap.today_pv2_kwh)).toBeCloseTo(14.7, 1);
    expect(Number(snap.today_solar_kwh)).toBeCloseTo(29.7, 1);
  });
});
