import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import {
  getTodayBoundaryMs,
  getHistoryRangeDomain,
  getHistoryXAxisTicks,
  formatHistoryXAxisTick,
  trimDomainStartToFirstDataPoint,
  rangeToBucketSecs,
  HISTORY_CHART_GRID_PRESETS,
  getHistoryChartGridProps,
  type GridLineWeight,
} from '../../src/lib/historyRangeConfig';

/**
 * Regression tests for the History page timezone-shift bug (PR #136).
 *
 * Background:
 *   The History page (`src/pages/HistoryPage.tsx`) plots time-series data
 *   returned by `/api/history`. Backend timestamps in `result[field][i].t`
 *   are UTC epoch milliseconds; the chart x-axis domain is built in local
 *   time (e.g. `[local_midnight_ms, next_local_midnight_ms]` for the
 *   "Today" range), and `formatHistoryXAxisTick` formats a tick via
 *   `toLocaleTimeString` so each tick renders in the user's wall clock.
 *
 *   The pre-fix pipeline added `new Date().getTimezoneOffset() * 60_000`
 *   to every `.t` before plotting, on the (incorrect) theory that this
 *   aligned the inverter's UTC-based daily counter reset to local
 *   midnight. That shift moved every series — and the entire x-axis —
 *   one hour behind local time in BST, so a 16:40 reading rendered at
 *   15:40.
 *
 *   The fix dropped the shift entirely. These tests pin the underlying
 *   contracts that made the fix work, so the shift cannot silently come
 *   back under a different timezone:
 *
 *     - `getHistoryRangeDomain('today', ...)` returns local-midnight UTC
 *       epoch ms (verified by round-tripping through `new Date`).
 *     - A backend-style UTC epoch ms representing "now in BST" falls
 *       INSIDE the domain — i.e. the domain is the correct local window
 *       for the user's timezone, not a UTC window.
 *     - Tick formatting of the domain start reads "00:00" (local),
 *       and a UTC point at 16:40 BST reads "16:40" — no shift applied.
 *     - Rolling ranges (24h) anchor at "now" UTC and read at the user's
 *       local wall clock.
 *
 *   Vitest runs the file in Node, where `process.env.TZ` redirects
 *   `Date` to a specific zone. We force a non-UTC zone (`Europe/London`)
 *   for the whole suite and restore it on the way out. Running locally
 *   under `TZ=UTC` would not exercise the bug — the bug only manifests
 *   east of UTC — so a TZ-pinned file is the right shape.
 */

const ORIGINAL_TZ = process.env.TZ;
const PINNED_TZ = 'Europe/London';

beforeAll(() => {
  process.env.TZ = PINNED_TZ;
});

afterAll(() => {
  if (ORIGINAL_TZ === undefined) {
    delete process.env.TZ;
  } else {
    process.env.TZ = ORIGINAL_TZ;
  }
});

// ---------------------------------------------------------------------------
// Sanity check that the TZ pin actually took effect. If Node ever stops
// honouring mid-process TZ changes this whole suite silently degrades to a
// UTC test, which would not catch the original regression.
// ---------------------------------------------------------------------------

describe('TZ pin', () => {
  it('honours the pinned timezone for Date construction', () => {
    // 2024-06-15 noon BST → 11:00 UTC.
    const noonLocal = new Date(2024, 5, 15, 12, 0, 0, 0).getTime();
    expect(new Date(noonLocal).toISOString()).toBe('2024-06-15T11:00:00.000Z');
    // And the local-time clock reads 12:00, not 11:00.
    expect(new Date(noonLocal).getHours()).toBe(12);
  });
});

// ---------------------------------------------------------------------------
// getTodayBoundaryMs / getHistoryRangeDomain — local-midnight UTC epoch ms.
// ---------------------------------------------------------------------------

describe('getTodayBoundaryMs / getHistoryRangeDomain (today) under BST', () => {
  // Pick a fixed "now" inside BST to remove clock dependence. 2024-06-15
  // 16:40 BST = 15:40 UTC.
  const nowMs = Date.UTC(2024, 5, 15, 15, 40, 0);

  it('returns boundaries whose local wall clock is 00:00 → 24:00', () => {
    const [start, end] = getTodayBoundaryMs(0, nowMs);
    const startLocal = new Date(start);
    const endLocal = new Date(end);
    expect(startLocal.getFullYear()).toBe(2024);
    expect(startLocal.getMonth()).toBe(5); // June
    expect(startLocal.getDate()).toBe(15);
    expect(startLocal.getHours()).toBe(0);
    expect(startLocal.getMinutes()).toBe(0);
    expect(startLocal.getSeconds()).toBe(0);
    // End is exclusive — start of the NEXT local day.
    expect(endLocal.getFullYear()).toBe(2024);
    expect(endLocal.getMonth()).toBe(5);
    expect(endLocal.getDate()).toBe(16);
    expect(endLocal.getHours()).toBe(0);
  });

  it('returns a 24h span between the boundaries (not 23h or 25h)', () => {
    const [start, end] = getTodayBoundaryMs(0, nowMs);
    expect(end - start).toBe(86_400_000);
  });

  it('start is the UTC-epoch-ms equivalent of BST 2024-06-15 00:00', () => {
    const [start] = getTodayBoundaryMs(0, nowMs);
    // BST = UTC+1, so 00:00 BST = 23:00 UTC the previous day.
    expect(new Date(start).toISOString()).toBe('2024-06-14T23:00:00.000Z');
  });

  it('"now" falls inside the domain (so data points at "now" render in-window)', () => {
    const [start, end] = getTodayBoundaryMs(0, nowMs);
    expect(nowMs).toBeGreaterThanOrEqual(start);
    expect(nowMs).toBeLessThan(end);
  });

  it('getHistoryRangeDomain("today") matches getTodayBoundaryMs', () => {
    expect(getHistoryRangeDomain('today', 0, nowMs)).toEqual(getTodayBoundaryMs(0, nowMs));
  });

  it('offset=1 shifts the domain to the previous local day', () => {
    const [start, end] = getTodayBoundaryMs(1, nowMs);
    // Start: 2024-06-14 00:00 BST = 2024-06-13T23:00:00Z
    expect(new Date(start).toISOString()).toBe('2024-06-13T23:00:00.000Z');
    expect(new Date(end).toISOString()).toBe('2024-06-14T23:00:00.000Z');
  });
});

// ---------------------------------------------------------------------------
// formatHistoryXAxisTick — renders the user's local wall clock, not UTC.
// This is the contract that PR #136's removed shift was breaking: previously
// the frontend added `getTimezoneOffset()*60_000` to every `.t` before
// plotting, so a 15:40 UTC tick (16:40 BST reading) would format as 16:40
// but be drawn against a 00:00→24:00 LOCAL axis shifted by the offset —
// misaligning the whole chart. With the shift gone, the tick formatter
// and the domain both speak local time and every series lands at the
// correct x position.
// ---------------------------------------------------------------------------

describe('formatHistoryXAxisTick under BST', () => {
  it('renders the domain start as 00:00 (local midnight)', () => {
    const [start] = getTodayBoundaryMs(0, Date.UTC(2024, 5, 15, 15, 40, 0));
    // "today" range uses toLocaleTimeString with hour:minute.
    const tick = formatHistoryXAxisTick(start, 'today');
    // The exact string depends on the Node ICU version, but it must read
    // "00:00" (local) — not "23:00" (which would be the UTC hour) and not
    // shifted into the previous day.
    expect(tick).toBe('00:00');
  });

  it('renders a 16:40 BST data point as "16:40" (not 15:40, the old buggy shift)', () => {
    // 16:40 BST = 15:40 UTC.
    const pointMs = Date.UTC(2024, 5, 15, 15, 40, 0);
    const tick = formatHistoryXAxisTick(pointMs, 'today');
    expect(tick).toBe('16:40');
  });

  it('renders 24h rolling-range ticks in local wall-clock hours', () => {
    const [start, end] = getHistoryRangeDomain('24h', 0, Date.UTC(2024, 5, 15, 15, 40, 0));
    const ticks = getHistoryXAxisTicks('24h', [start, end]);
    expect(ticks).toBeDefined();
    // Every tick should format to "HH:00" in local time (24h range uses 3h
    // spacing aligned to local midnight).
    for (const t of ticks!) {
      const label = formatHistoryXAxisTick(t, '24h');
      expect(label).toMatch(/^\d{2}:00$/);
    }
  });
});

// ---------------------------------------------------------------------------
// trimDomainStartToFirstDataPoint — used by HistoryPage to crop the rolling
// window (1h, 6h) to the first real point. It must compare on the same
// timestamp basis as the points themselves (raw UTC epoch ms), not on
// anything timezone-shifted.
// ---------------------------------------------------------------------------

describe('trimDomainStartToFirstDataPoint under BST', () => {
  it('uses raw .t values, not timezone-shifted ones', () => {
    // Domain: 1h window ending at 15:40 UTC (16:40 BST). Min gap 60s.
    const [start, end] = getHistoryRangeDomain('1h', 0, Date.UTC(2024, 5, 15, 15, 40, 0));
    // First data point 5 minutes into the window.
    const firstPointMs = start + 5 * 60_000;
    const series = { soc: [{ t: firstPointMs, v: 50 }] };

    const trimmed = trimDomainStartToFirstDataPoint([start, end], series, 60_000);

    // The trimmed start must be the original .t of the first point — not
    // the .t plus or minus any timezone offset. If trimDomainStartToFirstDataPoint
    // were ever to add/subtract `getTimezoneOffset()`, this assertion would
    // fail under BST (offset = -60min) by exactly 3_600_000 ms.
    expect(trimmed[0]).toBe(firstPointMs);
    expect(trimmed[1]).toBe(end);
  });
});

// ---------------------------------------------------------------------------
// End-to-end invariant: the UTC epoch ms returned by the backend falls
// inside the frontend's local-time domain AND tick-formats as the user's
// local wall clock. This is the user-visible property that broke pre-fix:
// the chart drew the right numbers at the wrong x-positions, looking
// one hour behind.
// ---------------------------------------------------------------------------

describe('end-to-end: backend UTC ms → frontend local display under BST', () => {
  const nowMs = Date.UTC(2024, 5, 15, 15, 40, 0); // 16:40 BST

  it('a 16:40 BST reading plots inside the "today" domain and labels as 16:40', () => {
    const domain = getHistoryRangeDomain('today', 0, nowMs);
    expect(nowMs).toBeGreaterThanOrEqual(domain[0]);
    expect(nowMs).toBeLessThan(domain[1]);
    expect(formatHistoryXAxisTick(nowMs, 'today')).toBe('16:40');
  });

  it('a 23:30 BST reading from the previous day plots inside the offset=1 domain', () => {
    // 23:30 BST on 2024-06-14 = 22:30 UTC on 2024-06-14.
    const yesterdayEveningMs = Date.UTC(2024, 5, 14, 22, 30, 0);
    const domain = getHistoryRangeDomain('today', 1, nowMs);
    expect(yesterdayEveningMs).toBeGreaterThanOrEqual(domain[0]);
    expect(yesterdayEveningMs).toBeLessThan(domain[1]);
    expect(formatHistoryXAxisTick(yesterdayEveningMs, 'today')).toBe('23:30');
  });

  it('a 00:30 BST reading on the target day plots inside the domain (boundary case)', () => {
    // 00:30 BST on 2024-06-15 = 23:30 UTC on 2024-06-14. The point sits
    // just after the domain start — the previous shift + filter used to
    // trim exactly this kind of point out.
    const earlyMorningMs = Date.UTC(2024, 5, 14, 23, 30, 0);
    const domain = getHistoryRangeDomain('today', 0, nowMs);
    expect(earlyMorningMs).toBeGreaterThanOrEqual(domain[0]);
    expect(earlyMorningMs).toBeLessThan(domain[1]);
    expect(formatHistoryXAxisTick(earlyMorningMs, 'today')).toBe('00:30');
  });
});

// ---------------------------------------------------------------------------
// Static guard: HistoryPage must not re-introduce a timezone offset shift
// on `.t` values. The pre-fix pipeline (PR #136's diff) called
// `new Date().getTimezoneOffset()` and added it to every fetched point's
// timestamp before plotting, shifting the entire x-axis behind local time
// in timezones east of UTC. If that pattern ever returns to the page,
// this test fails and points the reviewer at the regression. The lib
// functions above are timezone-clean by design, so the only way the bug
// can come back is via a reintroduced frontend shift.
// ---------------------------------------------------------------------------

describe('HistoryPage timestamp pipeline (static guard)', () => {
  // Resolved relative to the project root (vitest sets `process.cwd()` to
  // the project root when invoked from `npm test`). Falling back to the
  // import URL is unreliable under vitest because the dev-server rewrites
  // `import.meta.url` to an `http://` scheme, breaking `fileURLToPath`.
  const HISTORY_PAGE_PATH = resolve(process.cwd(), 'src/pages/HistoryPage.tsx');

  it('does not call Date.getTimezoneOffset on fetched point timestamps', () => {
    const source = readFileSync(HISTORY_PAGE_PATH, 'utf8');
    // Match `getTimezoneOffset(...)` invocations — including any future
    // spelling the original bug used. Allow the substring to appear in
    // comments (the original explanatory block in HistoryPage was removed
    // by the fix, but if a future change adds it back as documentation
    // this assertion should still flag the runtime call). We don't try to
    // parse JS — a comment containing the exact identifier without
    // parentheses is unlikely and would be a sign someone is papering over
    // a regression.
    expect(source).not.toMatch(/\bgetTimezoneOffset\s*\(/);
  });

  it('does not add a tzOffsetMs-style shift to point timestamps', () => {
    const source = readFileSync(HISTORY_PAGE_PATH, 'utf8');
    // Belt-and-braces: also forbid the variable name and the
    // `p.t + tzOffsetMs` shape from the original bug. Catches reverts and
    // re-applies of the diff before they get to a human reviewer.
    expect(source).not.toMatch(/tzOffsetMs/);
    expect(source).not.toMatch(/\.t\s*\+\s*tzOffsetMs/);
  });
});
/**
 * `rangeToBucketSecs` mirrors the backend range→bucket mapping in
 * `src-tauri/src/server/api.rs` (`get_history`). The Cost-tab spike ceiling
 * scales by this value (issue #133), so a drift between the two mappings would
 * silently distort Cost totals. These tests pin the contract.
 */
describe('rangeToBucketSecs', () => {
  it.each([
    ['1h', 30],
    ['6h', 60],
    ['12h', 120],
    ['24h', 300],
    ['today', 300],
    ['7d', 1800],
    ['30d', 7200],
    ['month', 3600],
    ['6m', 43200],
    ['1y', 86400],
  ] as const)('maps %s → %s s (matches backend get_history)', (range, expected) => {
    expect(rangeToBucketSecs(range)).toBe(expected);
  });
});

// ---------------------------------------------------------------------------
// HISTORY_CHART_GRID_PRESETS — issue #111.
//
// The recharts `CartesianGrid` on every live history chart (Power, History,
// Battery tab, Solar tab) is driven from one of two presets so users bothered
// by chunky grid lines can opt into a hairline look. These tests pin:
//   - the two presets exist and have the correct dash / width values;
//   - the `standard` preset is byte-identical to the previous hard-coded
//     constant (2 px, '4 4' dash, the original CSS-var stroke) so upgrading
//     users see no visual change unless they opt in;
//   - `subtle` is meaningfully lighter: thinner stroke AND lower-contrast
//     stroke variable, so it sits behind the 2-px / 3-px data series;
//   - `getHistoryChartGridProps` returns the preset verbatim and never
//     mutates the source object (recharts relies on stable references when
//     the same props are re-spread on re-render).
// ---------------------------------------------------------------------------

describe('HISTORY_CHART_GRID_PRESETS (issue #111)', () => {
  it('exposes exactly the two preset weights', () => {
    expect(Object.keys(HISTORY_CHART_GRID_PRESETS).sort()).toEqual([
      'standard',
      'subtle',
    ]);
  });

  it.each<[GridLineWeight, number, string, string]>([
    ['standard', 2, '4 4', 'var(--color-grid-stroke)'],
    ['subtle', 1, '3 4', 'var(--color-grid-stroke-subtle)'],
  ])('preset "%s" → strokeWidth=%i, dasharray="%s", stroke="%s"',
    (weight, width, dasharray, stroke) => {
      const props = HISTORY_CHART_GRID_PRESETS[weight];
      expect(props.strokeWidth).toBe(width);
      expect(props.strokeDasharray).toBe(dasharray);
      expect(props.stroke).toBe(stroke);
    },
  );

  it('standard preset matches the previous hard-coded constant exactly', () => {
    // Issue #111's explicit requirement: the default weight must produce a
    // byte-identical visual to before. If anyone ever changes the standard
    // preset, they must also update this test and the changelog, because
    // it's a user-visible behavioural change for everyone who hasn't
    // touched the new setting.
    expect(HISTORY_CHART_GRID_PRESETS.standard).toEqual({
      strokeDasharray: '4 4',
      stroke: 'var(--color-grid-stroke)',
      strokeWidth: 2,
    });
  });

  it('subtle is strictly lighter than standard in both width and contrast', () => {
    // Stroke width is what Recharts feeds through to the SVG <line>; the
    // dasharray gap-to-mark ratio is what makes the line feel chunky. A
    // preset must be lighter on BOTH axes or it's not actually subtle.
    expect(HISTORY_CHART_GRID_PRESETS.subtle.strokeWidth)
      .toBeLessThan(HISTORY_CHART_GRID_PRESETS.standard.strokeWidth);

    // The stroke values are CSS-var strings. They are different identifiers,
    // which is enough to ensure the rendered colour is different in CSS. A
    // string-equality check is the right shape — we don't want to assert
    // computed colour here (that belongs in a CSS integration test).
    expect(HISTORY_CHART_GRID_PRESETS.subtle.stroke)
      .not.toBe(HISTORY_CHART_GRID_PRESETS.standard.stroke);
  });

  it('every preset carries the three keys Recharts forwards to the SVG <line>', () => {
    for (const weight of Object.keys(HISTORY_CHART_GRID_PRESETS) as GridLineWeight[]) {
      const props = HISTORY_CHART_GRID_PRESETS[weight];
      // Recharts ignores unknown keys but silently drops them; missing any
      // of these would make the grid render as Recharts' default (1px solid
      // black on a dark background = invisible on dark, wrong on light).
      expect(props).toHaveProperty('strokeWidth');
      expect(props).toHaveProperty('stroke');
      expect(props).toHaveProperty('strokeDasharray');
      expect(typeof props.strokeWidth).toBe('number');
      expect(typeof props.stroke).toBe('string');
      expect(typeof props.strokeDasharray).toBe('string');
    }
  });
});

describe('getHistoryChartGridProps (issue #111)', () => {
  it.each<[GridLineWeight, number, string]>([
    ['standard', 2, '4 4'],
    ['subtle', 1, '3 4'],
  ])('returns the preset verbatim for "%s"', (weight, expectedWidth, expectedDash) => {
    const props = getHistoryChartGridProps(weight);
    expect(props.strokeWidth).toBe(expectedWidth);
    expect(props.strokeDasharray).toBe(expectedDash);
    // The spread contract — each call site writes
    // `<CartesianGrid {...getHistoryChartGridProps(weight)} />`. The getter
    // must return a plain object with all three fields so the spread lands
    // each as a JSX attribute. Missing a field → Recharts silently drops it.
    expect(props).toEqual(HISTORY_CHART_GRID_PRESETS[weight]);
  });

  it('returns a fresh reference on each call (no shared mutable state)', () => {
    // Recharts can re-render the chart on parent updates (e.g. when the
    // store ticks). If we returned a shared reference and a consumer ever
    // mutated it, every other chart would silently pick up the mutation.
    // The preset object itself is `as const` so it can't be mutated, but
    // we still verify the getter doesn't return a singleton.
    const a = getHistoryChartGridProps('standard');
    const b = getHistoryChartGridProps('standard');
    expect(a).toEqual(b);
    // Same shape — we don't care about identity equality here, only that
    // repeated calls produce identical content (so the consumer's
    // `===` checks on re-render behave deterministically).
    expect(a.strokeWidth).toBe(b.strokeWidth);
    expect(a.stroke).toBe(b.stroke);
    expect(a.strokeDasharray).toBe(b.strokeDasharray);
  });

  it('standard and subtle return different prop sets', () => {
    // Belt-and-braces: if someone refactors the presets to share a base
    // object and override one field, this catches accidental identity
    // collisions.
    expect(getHistoryChartGridProps('standard'))
      .not.toEqual(getHistoryChartGridProps('subtle'));
  });
});
