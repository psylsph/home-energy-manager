// @vitest-environment node

import { readFileSync } from 'node:fs';
import { describe, expect, test } from 'vitest';

const ROOT = new URL('../', import.meta.url);
const INFRASTRUCTURE_FILES = [
  'e2e/global-setup.ts',
  'e2e/local-global-setup.ts',
  'e2e/local-adaptive-charge.spec.ts',
  'e2e/local-dongle-misbehaviour.spec.ts',
  'e2e/local-evc-session-energy.spec.ts',
  'e2e/local-inverter-temperature-alert.spec.ts',
  'e2e/local-gateway-test.mjs',
];

describe('E2E infrastructure cleanup', () => {
  test('does not depend on fuser', () => {
    for (const relativePath of INFRASTRUCTURE_FILES) {
      const source = readFileSync(new URL(relativePath, ROOT), 'utf8');
      expect(source, relativePath).not.toMatch(/\bfuser\b/);
    }
  });

  test('both global setups register runner-exit cleanup', () => {
    for (const relativePath of ['e2e/global-setup.ts', 'e2e/local-global-setup.ts']) {
      const source = readFileSync(new URL(relativePath, ROOT), 'utf8');
      expect(source, relativePath).toMatch(/process\.on\(['"]exit['"]/);
    }
  });

  test('standalone gateway setup registers runner-exit cleanup', () => {
    const source = readFileSync(new URL('e2e/local-gateway-test.mjs', ROOT), 'utf8');
    expect(source).toMatch(/process\.on\(['"]exit['"]/);
  });
});
