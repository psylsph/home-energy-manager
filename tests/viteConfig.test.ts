// @vitest-environment node

import { readFileSync } from 'node:fs';
import { describe, expect, test } from 'vitest';
import config, { resolveAppVersion } from '../vite.config';

interface PackageManifest {
  version: string;
}

describe('Vite app version', () => {
  test('uses package.json when npm does not provide a package version', () => {
    const packageJson = JSON.parse(
      readFileSync(new URL('../package.json', import.meta.url), 'utf8'),
    ) as PackageManifest;
    const define = config.define as Record<string, string>;

    expect(define.__APP_VERSION__).toBe(JSON.stringify(packageJson.version));
    expect(resolveAppVersion(undefined, packageJson.version)).toBe(packageJson.version);
  });
});
