import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const source = readFileSync(resolve(process.cwd(), 'src/index.css'), 'utf8');

function themeBlock(selector: string): string {
  const match = source.match(new RegExp(`${selector}[^}]+}`, 's'));
  expect(match).not.toBeNull();
  return match![0];
}

function cssHex(block: string, variable: string): string {
  const match = block.match(new RegExp(`${variable}:\\s*(#[0-9A-Fa-f]{6});`));
  expect(match).not.toBeNull();
  return match![1];
}

function relativeLuminance(hex: string): number {
  const channels = [1, 3, 5].map((offset) => Number.parseInt(hex.slice(offset, offset + 2), 16) / 255);
  const [red, green, blue] = channels.map((channel) =>
    channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4,
  );
  return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
}

function contrastRatio(first: string, second: string): number {
  const [lighter, darker] = [relativeLuminance(first), relativeLuminance(second)].sort((a, b) => b - a);
  return (lighter + 0.05) / (darker + 0.05);
}

describe('shared interaction accent', () => {
  it('exposes dedicated accent and foreground Tailwind tokens', () => {
    expect(source).toMatch(/--color-accent:\s*var\(--app-accent\);/);
    expect(source).toMatch(/--color-on-accent:\s*var\(--app-on-accent\);/);
  });

  it.each([
    { selector: ':root,\\s*\\[data-theme="dark"\\]', theme: 'dark' },
    { selector: '\\[data-theme="light"\\]', theme: 'light' },
  ])('keeps solid accent controls readable in the $theme theme', ({ selector }) => {
    const block = themeBlock(selector);
    const accent = cssHex(block, '--app-accent');
    const foreground = cssHex(block, '--app-on-accent');

    expect(contrastRatio(accent, foreground)).toBeGreaterThanOrEqual(4.5);
  });

  it('uses the interaction accent for range controls rather than the battery colour', () => {
    expect(source).toMatch(/::-webkit-slider-thumb[^}]+background:\s*var\(--app-accent\);/s);
    expect(source).toMatch(/::-moz-range-thumb[^}]+background:\s*var\(--app-accent\);/s);
  });
});
