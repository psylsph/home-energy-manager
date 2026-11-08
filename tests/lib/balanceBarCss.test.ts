import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

// Balance-bar fills and their in-fill text (issue #429): every pair must
// clear 4.5:1 in both themes, and light mode uses white text on every fill.
const source = readFileSync(resolve(process.cwd(), 'src/index.css'), 'utf8');

function themeBlock(selector: string): string {
  const match = source.match(new RegExp(`${selector}[^}]+}`, 's'));
  expect(match).not.toBeNull();
  return match![0];
}

/** Resolves a variable to a hex literal, following one `var(--x)` hop. */
function cssHex(block: string, variable: string): string {
  const match = block.match(new RegExp(`${variable}:\\s*([^;]+);`));
  expect(match, variable).not.toBeNull();
  const value = match![1].trim();
  const ref = value.match(/^var\((--[\w-]+)\)$/);
  if (ref) return cssHex(block, ref[1]);
  expect(value).toMatch(/^#[0-9A-Fa-f]{6}$/);
  return value;
}

function luminance(hex: string): number {
  const [r, g, b] = [1, 3, 5]
    .map((o) => Number.parseInt(hex.slice(o, o + 2), 16) / 255)
    .map((c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

const SEGMENTS = ['solar', 'grid', 'battery', 'home', 'export'];
const THEMES = [
  { theme: 'dark', selector: ':root,\\s*\\[data-theme="dark"\\]' },
  { theme: 'light', selector: '\\[data-theme="light"\\]' },
];

describe('balance-bar colours', () => {
  it.each(THEMES)('keeps every fill readable in the $theme theme', ({ selector }) => {
    const block = themeBlock(selector);
    for (const seg of SEGMENTS) {
      const fill = cssHex(block, `--app-fill-${seg}`);
      const text = cssHex(block, `--app-on-${seg}`);
      expect(contrast(fill, text), seg).toBeGreaterThanOrEqual(4.5);
    }
  });

  it('uses the primary text colour on every fill in the dark theme', () => {
    const block = themeBlock(THEMES[0].selector);
    const primary = cssHex(block, '--app-text-primary').toUpperCase();
    for (const seg of [...SEGMENTS, 'car']) {
      expect(cssHex(block, `--app-on-${seg}`).toUpperCase(), seg).toBe(primary);
    }
  });

  it('keeps the car fill readable in both themes', () => {
    for (const { selector } of THEMES) {
      const block = themeBlock(selector);
      expect(
        contrast(cssHex(block, '--app-fill-car'), cssHex(block, '--app-on-car')),
      ).toBeGreaterThanOrEqual(4.5);
    }
  });

  it('puts white text on every fill in the light theme', () => {
    const block = themeBlock('\\[data-theme="light"\\]');
    for (const seg of SEGMENTS) {
      expect(cssHex(block, `--app-on-${seg}`).toUpperCase(), seg).toBe('#FFFFFF');
    }
  });
});
