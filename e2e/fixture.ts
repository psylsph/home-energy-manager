/**
 * Test fixture: provides helpers for interacting with the mock Modbus server
 * via its HTTP admin API (started by global-setup).
 */

import { test as base } from '@playwright/test';

// ---------------------------------------------------------------------------
// Configuration — must match global-setup.ts and mock-modbus.ts
// ---------------------------------------------------------------------------

const HTTP_PORT = 17337;
const ADMIN_PORT = 18900;
const ADMIN_BASE = `http://127.0.0.1:${ADMIN_PORT}`;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface RegisterWrite {
  address: number;
  value: number;
}

// ---------------------------------------------------------------------------
// Admin API helpers
// ---------------------------------------------------------------------------

async function adminGet(path: string): Promise<any> {
  const resp = await fetch(`${ADMIN_BASE}${path}`);
  return resp.json();
}

async function adminPost(path: string, body?: unknown): Promise<any> {
  const resp = await fetch(`${ADMIN_BASE}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: body ? JSON.stringify(body) : undefined,
  });
  return resp.json();
}

// ---------------------------------------------------------------------------
// Fixture types
// ---------------------------------------------------------------------------

export interface ModbusFixtures {
  /** Drain all captured Modbus register writes and return them. */
  drainModbusWrites: () => Promise<RegisterWrite[]>;
  /** Peek at captured writes without clearing. */
  peekModbusWrites: () => Promise<RegisterWrite[]>;
  /** Set a holding register value in the mock server. */
  setHoldingReg: (addr: number, value: number) => Promise<void>;
  /** Set an input register value in the mock server. */
  setInputReg: (addr: number, value: number) => Promise<void>;
  /** Reset all register state and captured writes. */
  resetModbus: () => Promise<void>;
  /** Base URL of the HTTP server. */
  baseUrl: string;
}

// ---------------------------------------------------------------------------
// Extended test fixture
// ---------------------------------------------------------------------------

export const test = base.extend<ModbusFixtures>({
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
