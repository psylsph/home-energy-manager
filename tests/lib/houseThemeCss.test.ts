import { describe, it, expect } from 'vitest';
import { readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';

/**
 * House view theme variables (issue #422): every `--app-house-*` colour the
 * illustration uses must be declared in both the dark and the light theme,
 * so light mode never falls back to an unstyled (black) fill.
 */

const css = readFileSync(resolve(process.cwd(), 'src/index.css'), 'utf8');
const houseDir = resolve(process.cwd(), 'src/components/house');
const source = readdirSync(houseDir)
  .filter((f) => f.endsWith('.tsx'))
  .map((f) => readFileSync(resolve(houseDir, f), 'utf8'))
  .join('\n');

function block(theme: 'dark' | 'light'): string {
  const m = css.match(new RegExp(`\\[data-theme="${theme}"\\][^}]+\\}`, 's'));
  expect(m, theme).not.toBeNull();
  return m![0];
}

function declared(theme: 'dark' | 'light'): Map<string, string> {
  const out = new Map<string, string>();
  for (const m of block(theme).matchAll(/--app-house-([a-z-]+):\s*([^;]+);/g)) out.set(m[1], m[2].trim());
  return out;
}

/** Names used as `v('name')` or `var(--app-house-name…)` in the house components. */
function used(): Set<string> {
  const names = new Set<string>();
  for (const m of source.matchAll(/\bv\('([a-z-]+)'\)/g)) names.add(m[1]);
  for (const m of source.matchAll(/var\(--app-house-([a-z-]+)/g)) names.add(m[1]);
  return names;
}

describe('src/index.css — house view theme variables', () => {
  it('finds the variables the house components use', () => {
    expect(used().size).toBeGreaterThan(15);
  });

  it.each(['dark', 'light'] as const)('declares every used variable in the %s theme', (theme) => {
    const d = declared(theme);
    for (const name of used()) expect(d.has(name), `--app-house-${name} in ${theme}`).toBe(true);
  });

  it('declares the same set in both themes', () => {
    expect([...declared('dark').keys()].sort()).toEqual([...declared('light').keys()].sort());
  });

  it('gives the light theme its own values', () => {
    const dark = declared('dark');
    const light = declared('light');
    expect(light.get('plot-top')).not.toBe(dark.get('plot-top'));
    expect(light.get('roof')).not.toBe(dark.get('roof'));
  });
});
