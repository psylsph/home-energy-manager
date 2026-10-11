import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

// Dependabot alert #44 (GHSA-238p-pmpm-9mq7): katex < 0.18.2 has a prototype
// pollution issue. katex only reaches us through the dev-only markdownlint
// chain (markdownlint-cli → markdownlint → micromark-extension-math), so the
// fix is an npm override plus a lockfile that resolves a patched version.
// This guards against the lock quietly drifting back to the vulnerable release.

const root = resolve(__dirname, '..', '..');

function parseSemver(version: string): [number, number, number] {
  const [major, minor, patch] = version.split('.').map((part) => Number.parseInt(part, 10));
  return [major ?? 0, minor ?? 0, patch ?? 0];
}

function atLeast(version: string, floor: string): boolean {
  const a = parseSemver(version);
  const b = parseSemver(floor);
  for (let i = 0; i < 3; i++) {
    if (a[i] !== b[i]) return a[i]! > b[i]!;
  }
  return true;
}

describe('katex (Dependabot #44)', () => {
  it('pins katex to the patched release through an override', () => {
    const pkg = JSON.parse(readFileSync(resolve(root, 'package.json'), 'utf8')) as {
      overrides?: Record<string, string>;
    };
    expect(pkg.overrides?.katex).toBe('^0.18.2');
  });

  it('resolves katex to 0.18.2 or later in the lockfile', () => {
    const lock = JSON.parse(readFileSync(resolve(root, 'package-lock.json'), 'utf8')) as {
      packages: Record<string, { version?: string }>;
    };
    const version = lock.packages['node_modules/katex']?.version;
    expect(version).toBeDefined();
    expect(atLeast(version!, '0.18.2')).toBe(true);
  });

  it('treats an older katex as vulnerable', () => {
    expect(atLeast('0.16.47', '0.18.2')).toBe(false);
    expect(atLeast('0.18.1', '0.18.2')).toBe(false);
    expect(atLeast('0.18.2', '0.18.2')).toBe(true);
  });
});
