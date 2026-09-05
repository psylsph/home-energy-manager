// @vitest-environment node

import { fileURLToPath } from 'node:url';
import { loadESLint } from 'eslint';
import { describe, expect, test } from 'vitest';

describe('ESLint configuration', () => {
  test('applies the Node E2E rules to JavaScript module helpers', async () => {
    const ESLint = await loadESLint();
    const eslint = new ESLint({
      overrideConfigFile: fileURLToPath(new URL('../eslint.config.js', import.meta.url)),
    });

    const [nodeResult] = await eslint.lintText('process.exitCode = process.env.NODE_ENV ? 0 : 1;\n', {
      filePath: 'e2e/helper.mjs',
    });
    const [browserResult] = await eslint.lintText('window.location.href;\n', {
      filePath: 'e2e/helper.mjs',
    });

    expect(nodeResult.errorCount).toBe(0);
    expect(browserResult.messages.some((message) => message.ruleId === 'no-undef')).toBe(true);
  });
});
