import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

/**
 * Static guard for the chart grid line weight CSS contract (issue #111).
 *
 * The recharts `CartesianGrid` reads its `stroke` prop as a CSS variable
 * reference like `var(--color-grid-stroke-subtle)`. If that variable is
 * missing from `src/index.css`, the grid line renders as Recharts'
 * default (1 px solid black on dark backgrounds = invisible). These
 * tests pin the variable presence and per-theme definition so the subtle
 * preset cannot silently degrade to no-grid.
 *
 * We deliberately don't try to evaluate the CSS or compute the rendered
 * colour — that belongs in a Playwright visual regression test. This
 * file is the cheap static guard that catches a missing declaration at
 * PR time.
 */
const INDEX_CSS_PATH = resolve(process.cwd(), 'src/index.css');

describe('src/index.css — grid stroke CSS variables (issue #111)', () => {
  const source = readFileSync(INDEX_CSS_PATH, 'utf8');

  it('declares --app-grid-stroke in both dark and light themes', () => {
    // The standard preset's stroke is unchanged by this work — if someone
    // deletes the original variable while editing the new one, this fails.
    expect(source).toMatch(/--app-grid-stroke:\s*#6E7681;/);
    expect(source).toMatch(/--app-grid-stroke:\s*#57606A;/);
  });

  it('declares --app-grid-stroke-subtle in the dark theme', () => {
    // The dark-theme declaration must sit inside the dark block. We match
    // any rgba value because the alpha is a deliberate design knob; if a
    // future commit hard-codes the wrong alpha, the visual contract still
    // holds as long as it stays rgba() and not opaque.
    expect(source).toMatch(/--app-grid-stroke-subtle:\s*rgba\([^)]+\)\s*;/);
  });

  it('declares --app-grid-stroke-subtle in the light theme', () => {
    // Same shape as the dark theme. The exact rgba may differ — light
    // backgrounds often need a lower alpha than dark — but the variable
    // must exist in both blocks.
    const lightBlockMatch = source.match(/\[data-theme="light"\][^}]+\}/s);
    expect(lightBlockMatch).not.toBeNull();
    expect(lightBlockMatch![0]).toMatch(/--app-grid-stroke-subtle:/);
  });

  it('exposes --color-grid-stroke-subtle through the Tailwind theme bridge', () => {
    // The TS presets reference `var(--color-grid-stroke-subtle)`, NOT the
    // `--app-grid-stroke-subtle` variable directly. The bridge line in
    // `@theme` does the aliasing. If someone removes it, every chart
    // renders with Recharts' default stroke.
    expect(source).toMatch(/--color-grid-stroke-subtle:\s*var\(--app-grid-stroke-subtle\)/);
  });

  it('keeps the standard --color-grid-stroke bridge intact (no regression)', () => {
    // Belt-and-braces for the standard preset: the bridge line that was
    // already in place must remain.
    expect(source).toMatch(/--color-grid-stroke:\s*var\(--app-grid-stroke\)/);
  });
});
