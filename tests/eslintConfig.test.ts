// @vitest-environment node

import { readFileSync } from 'node:fs';
import { describe, expect, test } from 'vitest';

describe('ESLint configuration', () => {
  test('applies the Node E2E rules to JavaScript module helpers', () => {
    const config = readFileSync(new URL('../eslint.config.js', import.meta.url), 'utf8');

    expect(config).toMatch(/e2e\/\*\*\/\*\.mjs/);
  });
});
