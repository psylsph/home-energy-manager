import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

/**
 * Static guard: every recharts `CartesianGrid` consumer in `src/` must
 * spread `getHistoryChartGridProps(…)` rather than the removed
 * `HISTORY_CHART_GRID_PROPS` constant.
 *
 * Background (issue #111):
 *   The original constant was a single shared object, so every chart
 *   rendered with identical grid props. Replacing it with a getter
 *   keyed on the user's `gridLineWeight` preference is what lets the
 *   user actually see a difference between Standard and Subtle. If a
 *   future contributor accidentally re-imports the old constant on one
 *   chart, that chart would silently keep the 2-px look regardless of
 *   the preference and this test would fail at PR time.
 *
 *   We also verify each consumer reads `gridLineWeight` from the store
 *   so the setter actually flows through. Both checks together pin the
 *   end-to-end wiring without rendering React.
 */

const COMPONENTS_DIR = resolve(process.cwd(), 'src/components');
const PAGES_DIR = resolve(process.cwd(), 'src/pages');

const CONSUMER_FILES: { path: string; varName: string }[] = [
  { path: resolve(COMPONENTS_DIR, 'BatterySocChart.tsx'), varName: 'gridLineWeight' },
  { path: resolve(COMPONENTS_DIR, 'SolarPowerChart.tsx'), varName: 'gridLineWeight' },
  { path: resolve(PAGES_DIR, 'HistoryPage.tsx'), varName: 'gridLineWeight' },
  { path: resolve(PAGES_DIR, 'PowerPage.tsx'), varName: 'gridLineWeight' },
];

describe('chart consumers — grid line weight wiring (issue #111)', () => {
  it.each(CONSUMER_FILES)(
    '$path no longer references the removed HISTORY_CHART_GRID_PROPS constant',
    ({ path }) => {
      const source = readFileSync(path, 'utf8');
      // The old constant has been replaced by the preset getter. Any
      // remaining import or spread of it would mean a chart still renders
      // the un-configurable 2-px grid regardless of the user's choice.
      expect(source).not.toMatch(/HISTORY_CHART_GRID_PROPS/);
    },
  );

  it.each(CONSUMER_FILES)(
    '$path imports getHistoryChartGridProps from historyRangeConfig',
    ({ path }) => {
      const source = readFileSync(path, 'utf8');
      // Without the getter in scope, the spread on the CartesianGrid
      // element wouldn't compile — but if a contributor removed the
      // import but kept the spread, TypeScript would catch it. This is
      // belt-and-braces against a `// @ts-expect-error` being added.
      expect(source).toMatch(/getHistoryChartGridProps/);
    },
  );

  it.each(CONSUMER_FILES)(
    '$path spreads the getter onto its <CartesianGrid>',
    ({ path }) => {
      const source = readFileSync(path, 'utf8');
      // The actual wiring: the chart must read the live preference at
      // render time. A static spread (e.g. `HISTORY_CHART_GRID_PROPS`)
      // would compile but render the wrong grid. Match the spread call.
      expect(source).toMatch(/<CartesianGrid\s+\{\.\.\.getHistoryChartGridProps\(/);
    },
  );

  it.each(CONSUMER_FILES)(
    '$path subscribes to gridLineWeight from the store',
    ({ path, varName }) => {
      const source = readFileSync(path, 'utf8');
      // Reading `gridLineWeight` from `useInverterStore` is what closes
      // the loop — without it, the getter call gets `undefined` and the
      // spread fails at runtime.
      expect(source).toMatch(new RegExp(`state\\.${varName}`));
    },
  );

  it('HISTORY_CHART_GRID_PROPS is not re-exported from historyRangeConfig.ts', () => {
    // If the constant still exists as an export, a future contributor
    // could accidentally import it from the lib. The replacement is the
    // preset map + getter; the constant must be gone.
    const libPath = resolve(process.cwd(), 'src/lib/historyRangeConfig.ts');
    const source = readFileSync(libPath, 'utf8');
    expect(source).not.toMatch(/export\s+const\s+HISTORY_CHART_GRID_PROPS/);
  });

  it('historyRangeConfig.ts exports both GridLineWeight and the getter', () => {
    // Conversely, the new public surface must be present so consumers
    // can import it.
    const libPath = resolve(process.cwd(), 'src/lib/historyRangeConfig.ts');
    const source = readFileSync(libPath, 'utf8');
    expect(source).toMatch(/export\s+type\s+GridLineWeight/);
    expect(source).toMatch(/export\s+const\s+HISTORY_CHART_GRID_PRESETS/);
    expect(source).toMatch(/export\s+function\s+getHistoryChartGridProps/);
  });
});
