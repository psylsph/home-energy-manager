/**
 * Test fixture: provides helpers for interacting with the mock Modbus server
 * via its HTTP admin API (started by global-setup).
 */

import { test as base } from '@playwright/test';
import { requireOkJson } from './admin-responses.js';

// ---------------------------------------------------------------------------
// Configuration — must match global-setup.ts and mock-modbus.ts
// ---------------------------------------------------------------------------

const HTTP_PORT = 17337;
const ADMIN_PORT = 18900;
const ADMIN_BASE = `http://127.0.0.1:${ADMIN_PORT}`;
const HARNESS_RESET_TIMEOUT_MS = 60_000;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface RegisterWrite {
  address: number;
  value: number;
}

// ---------------------------------------------------------------------------
// Harness reset (runs before every test — see the auto fixture below)
// ---------------------------------------------------------------------------

async function adminGet(path: string): Promise<any> {
  const resp = await fetch(`${ADMIN_BASE}${path}`);
  return requireOkJson(resp, `GET ${path}`);
}

async function adminPost(path: string, body?: unknown): Promise<any> {
  const resp = await fetch(`${ADMIN_BASE}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: body ? JSON.stringify(body) : undefined,
  });
  return requireOkJson(resp, `POST ${path}`);
}

/**
 * Reset the whole harness (mock Modbus server + backend schedule ownership)
 * to a clean pre-test state.
 *
 * Resetting only the mock's registers is NOT enough: the backend's Timed
 * Export schedule (`timed_export_schedule_enabled`, persisted in the spec
 * file's settings dir, including the desired slots and the learned HR59
 * re-arm fallback) and its boundary state machine live in the backend
 * process and survive a register reset. A test that arms Timed Export and
 * leaves it armed therefore makes the reconciler treat the next mock reset
 * as "registers changed externally" and re-arm export into the new test,
 * and a later enable silently reuses the previous test's schedule slots —
 * the mock-reset-only per-file reset leaked exactly this way (observed in
 * timed-export-slot-guard.spec.ts).
 *
 * Order matters: the backend state is reset FIRST (POST /api/test/reset,
 * armed by the harness's `--e2e-admin` flag — see server::api::test_reset),
 * so from that moment no automation owns the discharge registers and the
 * register reset cannot be fought. Only then are the registers reset, one
 * post-reset poll awaited (so a test that acts immediately cannot compute a
 * decision from a stale pre-reset snapshot), and the captured writes
 * drained so per-test write assertions start empty.
 */
export async function resetHarness(baseUrl: string): Promise<void> {
  // 1. Backend-owned schedule/machine/force-revert state → clean. Only an
  // actual connection failure is tolerated (specs without a backend): a
  // 404 or 500 means the reset did NOT happen, and continuing would leak
  // desired slots or machine ownership into the next test — exactly the
  // order-dependence this fixture exists to prevent.
  try {
    const resp = await fetch(`${baseUrl}/api/test/reset`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({}),
    });
    const body: unknown = await resp.json().catch(() => null);
    if (!resp.ok || (body as { ok?: unknown } | null)?.ok !== true) {
      throw new Error(
        `harness reset: /api/test/reset returned HTTP ${resp.status}: ${JSON.stringify(body)}`,
      );
    }
  } catch (error) {
    // fetch signals a connection failure as TypeError; anything else is our
    // own assertion and must propagate.
    if (!(error instanceof TypeError)) {
      throw error;
    }
    /* backend not running — nothing to reset */
  }

  // 2. Mock registers → Gen3 default snapshot; captured writes and the
  // HR59 re-arm emulation cleared.
  await adminPost('/reset');

  // 3. Wait for one poll captured after the reset. The ceiling must cover
  // the worst case where the poll loop is still draining the previous
  // test's queued write burst (~35s at dongle pacing for a full ten-slot
  // clear) before its next read; the usual warm-backend case returns after
  // one poll.
  const resetAt = Date.now();
  const deadline = Date.now() + HARNESS_RESET_TIMEOUT_MS;
  for (;;) {
    let fresh = false;
    try {
      const resp = await fetch(`${baseUrl}/api/snapshot`);
      const body = await resp.json();
      const ts = body?.data?.timestamp;
      fresh = Boolean(
        body?.ok
          && typeof body?.data?.soc === 'number'
          && typeof ts === 'number'
          && ts * 1000 > resetAt
          && body.data.enable_discharge === false,
      );
    } catch {
      /* backend not running — nothing to wait for */
    }
    if (fresh || Date.now() >= deadline) {
      if (!fresh) {
        throw new Error('harness reset: no post-reset Eco snapshot within 60s');
      }
      break;
    }
    await new Promise((r) => setTimeout(r, 300));
  }

  // 4. Start every test with an empty captured-writes list.
  await adminPost('/writes/drain');
}

// ---------------------------------------------------------------------------
// Fixture types
// ---------------------------------------------------------------------------

export interface ModbusFixtures {
  /** Automatic per-test harness reset (auto fixture — do not use directly;
   *  call {@link resetHarness} for a mid-test reset). */
  harnessReset: void;
  /** Drain all captured Modbus register writes and return them. */
  drainModbusWrites: () => Promise<RegisterWrite[]>;
  /** Peek at captured writes without clearing. */
  peekModbusWrites: () => Promise<RegisterWrite[]>;
  /** Set a holding register value in the mock server. */
  setHoldingReg: (addr: number, value: number) => Promise<void>;
  /** Set an input register value in the mock server. */
  setInputReg: (addr: number, value: number) => Promise<void>;
  /**
   * Toggle the mock server's HR59 re-arm firmware emulation (issue #289):
   * when enabled, a client write of HR59=0 is re-asserted back to 1
   * (whenever any discharge slot register is non-zero) after the next
   * register read has observed the written 0 — mimicking Gen3 Hybrid
   * firmware that re-arms discharge while slots are populated, on its own
   * cycle rather than atomically inside the write.
   */
  setHr59Rearm: (enabled: boolean) => Promise<void>;
  /**
   * Make the mock inverter reject the next `count` FC06 writes with a
   * Modbus exception (code 4) instead of applying them — emulating a
   * device that refuses register writes. `count: 0` clears the emulation.
   */
  setRejectWrites: (count: number) => Promise<void>;
  /** Reset all register state and captured writes. */
  resetModbus: () => Promise<void>;
  /** Base URL of the HTTP server. */
  baseUrl: string;
}

// ---------------------------------------------------------------------------
// Extended test fixture
// ---------------------------------------------------------------------------

export const test = base.extend<ModbusFixtures>({
  // Per-test harness reset. Runs before every test in every spec that uses
  // this fixture, so tests within one spec file cannot leak mock register
  // state or backend schedule ownership into each other (each spec file
  // shares one backend + one mock server; only the FILE boundary got a
  // reset before, and the 10 full-suite failures were all within-file
  // leakage). Tests that want the stronger registered guarantee mid-test
  // can call `resetHarness(baseUrl)` again.
  harnessReset: [
    async ({ baseUrl }, use, testInfo) => {
      // The reset deadline is intentionally longer than Playwright's default
      // 30-second test timeout because a previous test may leave a large
      // write queue to drain. Reserve that time before the fixture starts so
      // a stalled reset reports its own error instead of aborting the test
      // and skipping the fixture's cleanup checks.
      testInfo.setTimeout(testInfo.timeout + HARNESS_RESET_TIMEOUT_MS);
      await resetHarness(baseUrl);
      let testFailed = false;
      let testError: unknown;
      try {
        await use();
      } catch (error) {
        testFailed = true;
        testError = error;
      }

      const protocolErrors = await adminGet('/protocol-errors');
      if (testFailed) {
        throw testError;
      }
      if (protocolErrors?.ok !== true || !Array.isArray(protocolErrors.errors)) {
        throw new Error(`harness protocol-error endpoint returned an invalid response: ${JSON.stringify(protocolErrors)}`);
      }
      if (protocolErrors.errors.length > 0) {
        throw new Error(`mock Modbus protocol violation: ${protocolErrors.errors.join('; ')}`);
      }
    },
    { auto: true },
  ],
  drainModbusWrites: async ({}, use) => {
    await use(async () => {
      const data = await adminPost('/writes/drain');
      return data.writes as RegisterWrite[];
    });
  },
  peekModbusWrites: async ({}, use) => {
    await use(async () => {
      const data = await adminGet('/writes');
      return data.writes as RegisterWrite[];
    });
  },
  setHoldingReg: async ({}, use) => {
    await use(async (addr, value) => {
      await adminPost('/holding-reg', { address: addr, value });
    });
  },
  setInputReg: async ({}, use) => {
    await use(async (addr, value) => {
      await adminPost('/input-reg', { address: addr, value });
    });
  },
  setHr59Rearm: async ({}, use) => {
    await use(async (enabled) => {
      await adminPost('/rearm-hr59', { enabled });
    });
  },
  setRejectWrites: async ({}, use) => {
    await use(async (count) => {
      const data = await adminPost('/reject-writes', { count });
      if (data?.ok !== true) {
        throw new Error(`reject-writes failed: ${JSON.stringify(data)}`);
      }
    });
  },
  resetModbus: async ({}, use) => {
    await use(async () => {
      await adminPost('/reset');
    });
  },
  baseUrl: async ({}, use) => {
    await use(`http://127.0.0.1:${HTTP_PORT}`);
  },
});

export { expect } from '@playwright/test';
