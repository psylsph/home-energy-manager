// @vitest-environment node

import { readFileSync } from 'node:fs';
import { describe, expect, test } from 'vitest';

const ROOT = new URL('../', import.meta.url);

describe('E2E harness reset timeout', () => {
  test('extends the Playwright test budget by the reset deadline', () => {
    const source = readFileSync(new URL('e2e/fixture.ts', ROOT), 'utf8');

    expect(source).toMatch(/HARNESS_RESET_TIMEOUT_MS\s*=\s*60_000/);
    expect(source).toMatch(
      /testInfo\.setTimeout\(testInfo\.timeout\s*\+\s*HARNESS_RESET_TIMEOUT_MS\)/,
    );
  });
});
