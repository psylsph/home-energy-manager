/**
 * Local E2E test fixture: provides the base URL for the headless backend.
 *
 * Unlike the mock-server fixture, this does NOT provide Modbus write capture,
 * because the real simulator doesn't have an admin API. Tests using this fixture
 * verify end-to-end behaviour by reading back state from the REST API or UI.
 */

import { test as base, expect } from '@playwright/test';

const HTTP_PORT = 17337;
export const BASE_URL = `http://127.0.0.1:${HTTP_PORT}`;

export interface LocalFixtures {
  /** Base URL of the HTTP server. */
  baseUrl: string;
}

export const test = base.extend<LocalFixtures>({
  baseUrl: async ({}, use) => {
    await use(BASE_URL);
  },
});

export { expect };
