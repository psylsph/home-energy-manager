import type { HistoryRange } from './types';

export const HISTORY_RANGES: { key: HistoryRange; label: string }[] = [
  { key: '1h', label: '1h' },
  { key: '6h', label: '6h' },
  { key: '12h', label: '12h' },
  { key: '24h', label: '24h' },
  { key: 'today', label: 'Today' },
  { key: '7d', label: '7d' },
  { key: '30d', label: '30d' },
  { key: 'month', label: 'Month' },
  { key: '6m', label: '6m' },
  { key: '1y', label: '1y' },
];

/**
 * Recharts `CartesianGrid` props presets for the live history charts.
 *
 * Two weights are exposed so users bothered by chunky grid lines (issue #111)
 * can opt into a hairline preset. The `standard` preset keeps the original
 * look byte-for-byte (`strokeWidth: 2`, `'4 4'` dash, current colour) so
 * existing users see no visual change unless they opt in.
 */
export type GridLineWeight = 'standard' | 'subtle';

export const HISTORY_CHART_GRID_PRESETS: Record<GridLineWeight, {
  strokeDasharray: string;
  stroke: string;
  strokeWidth: number;
}> = {
  standard: {
    strokeDasharray: '4 4',
    stroke: 'var(--color-grid-stroke)',
    strokeWidth: 2,
  },
  subtle: {
    strokeDasharray: '3 4',
    stroke: 'var(--color-grid-stroke-subtle)',
    strokeWidth: 1,
  },
};

/**
 * Return the active `CartesianGrid` props for the given weight preset.
 *
 * Spreads directly onto a `<CartesianGrid {...props} />` element. The stroke
 * value is a CSS variable string resolved by the SVG at render time — Recharts
 * forwards it unchanged through to the underlying `<line stroke="…">`.
 */
export function getHistoryChartGridProps(weight: GridLineWeight): {
  strokeDasharray: string;
  stroke: string;
  strokeWidth: number;
} {
  return HISTORY_CHART_GRID_PRESETS[weight];
}

export const HISTORY_RANGE_MS: Partial<Record<HistoryRange, number>> = {
  '1h': 3600000,
  '6h': 21600000,
  '12h': 43200000,
  '24h': 86400000,
  '7d': 604800000,
  '30d': 2592000000,
  '6m': 15552000000,
  '1y': 31536000000,
};

/**
 * History aggregation bucket size (seconds) for a range.
 *
 * Mirrors the backend's range→bucket mapping in
 * `src-tauri/src/server/api.rs` (`get_history`). The Cost-tab spike ceiling
 * scales by this value so legitimate per-bucket energy is not discarded on
 * wider ranges (issue #133). If the backend mapping changes, update both.
 */
export function rangeToBucketSecs(range: HistoryRange): number {
  switch (range) {
    case '1h':
      return 30;
    case '6h':
      return 60;
    case '12h':
      return 120;
    case '24h':
      return 300;
    case 'today':
      return 300;
    case '7d':
      return 1800;
    case '30d':
      return 7200;
    case 'month':
      return 3600;
    case '6m':
      return 43200;
    case '1y':
      return 86400;
  }
}

export function isRollingHistoryRange(range: HistoryRange): boolean {
  return range !== 'month' && range !== 'today';
}

export function shouldRefreshHistoryRange(range: HistoryRange, offset: number = 0): boolean {
  return isRollingHistoryRange(range) || (range === 'today' && offset === 0);
}

export function shouldTrimHistoryRangeLeadingGap(range: HistoryRange): boolean {
  return range === '1h' || range === '6h';
}

export function getTodayBoundaryMs(offset: number, nowMs: number = Date.now()): [number, number] {
  const now = new Date(nowMs);
  const start = new Date(now.getFullYear(), now.getMonth(), now.getDate() - offset, 0, 0, 0, 0).getTime();
  const end = new Date(now.getFullYear(), now.getMonth(), now.getDate() - offset + 1, 0, 0, 0, 0).getTime();
  return [start, end];
}

export function getMonthBoundaryMs(offset: number, nowMs: number = Date.now()): [number, number] {
  const now = new Date(nowMs);
  const totalMonths = now.getFullYear() * 12 + now.getMonth() - offset;
  const targetYear = Math.floor(totalMonths / 12);
  const targetMonth = totalMonths % 12;
  return [
    new Date(targetYear, targetMonth, 1, 0, 0, 0, 0).getTime(),
    new Date(targetYear, targetMonth + 1, 1, 0, 0, 0, 0).getTime(),
  ];
}

export function getHistoryRangeDomain(
  range: HistoryRange,
  offset: number = 0,
  nowMs: number = Date.now(),
): [number, number] {
  if (range === 'today') {
    return getTodayBoundaryMs(offset, nowMs);
  }

  if (range === 'month') {
    return getMonthBoundaryMs(offset, nowMs);
  }

  const windowMs = HISTORY_RANGE_MS[range] ?? HISTORY_RANGE_MS['24h'] ?? 86400000;
  const end = nowMs - offset * windowMs;
  return [end - windowMs, end];
}

export function getHistoryXAxisTicks(
  range: HistoryRange,
  domain: [number, number],
): number[] | undefined {
  const startMs = domain[0];
  const endMs = domain[1];
  const spanMs = endMs - startMs;
  const startDate = new Date(startMs);

  if (range === '1h') {
    const ticks: number[] = [];
    const cursor = new Date(startDate);
    cursor.setMinutes(0, 0, 0);
    while (cursor.getTime() < startMs) cursor.setMinutes(cursor.getMinutes() + 10);
    while (cursor.getTime() < endMs) {
      ticks.push(cursor.getTime());
      cursor.setMinutes(cursor.getMinutes() + 10);
    }
    return ticks;
  }

  if (range === '6h') {
    const ticks: number[] = [];
    const cursor = new Date(startDate);
    cursor.setMinutes(0, 0, 0);
    while (cursor.getTime() < startMs) cursor.setHours(cursor.getHours() + 1);
    while (cursor.getTime() < endMs) {
      ticks.push(cursor.getTime());
      cursor.setHours(cursor.getHours() + 1);
    }
    return ticks;
  }

  if (range === '12h') {
    const ticks: number[] = [];
    const cursor = new Date(startDate);
    cursor.setMinutes(0, 0, 0);
    while (cursor.getTime() < startMs) cursor.setHours(cursor.getHours() + 2);
    while (cursor.getTime() < endMs) {
      ticks.push(cursor.getTime());
      cursor.setHours(cursor.getHours() + 2);
    }
    return ticks;
  }

  if (range === '24h' || range === 'today') {
    const ticks: number[] = [];
    const cursor = new Date(startDate);
    cursor.setMinutes(0, 0, 0);
    while (cursor.getTime() < startMs) cursor.setHours(cursor.getHours() + 3);
    while (cursor.getTime() < endMs) {
      ticks.push(cursor.getTime());
      cursor.setHours(cursor.getHours() + 3);
    }
    return ticks;
  }

  if (range === '6m' || range === '1y') {
    const approxMonths = Math.max(1, Math.round(spanMs / (30 * 86400000)));
    const stepMonths = Math.max(1, Math.floor(approxMonths / 6));
    const ticks: number[] = [];
    const cursor = new Date(startDate.getFullYear(), startDate.getMonth(), 1);
    while (cursor.getTime() < startMs) cursor.setMonth(cursor.getMonth() + stepMonths);
    while (cursor.getTime() < endMs) {
      ticks.push(cursor.getTime());
      cursor.setMonth(cursor.getMonth() + stepMonths);
    }
    return ticks;
  }

  const totalDays = spanMs / 86400000;
  const step = Math.max(1, Math.floor(totalDays / 6));
  const ticks: number[] = [];
  const cursor = new Date(startDate.getFullYear(), startDate.getMonth(), startDate.getDate());
  while (cursor.getTime() < startMs) cursor.setDate(cursor.getDate() + step);
  while (cursor.getTime() < endMs) {
    ticks.push(cursor.getTime());
    cursor.setDate(cursor.getDate() + step);
  }
  return ticks;
}

export function formatHistoryXAxisTick(ts: number, range: HistoryRange): string {
  const d = new Date(ts);
  if (range === '1h' || range === '6h' || range === '12h' || range === '24h' || range === 'today') {
    return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }
  if (range === '7d' || range === '30d') {
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
  }
  if (range === 'month') {
    return String(d.getDate());
  }
  return d.toLocaleDateString([], { month: 'short', year: 'numeric' });
}

export function getHistoryXAxisMinTickGap(range: HistoryRange): number {
  return range === '1h' || range === '6h' || range === '12h' || range === '24h' || range === 'today'
    ? 30
    : 40;
}

export function trimDomainStartToFirstDataPoint<T extends { t: number }>(
  domain: [number, number],
  series: Record<string, T[]>,
  minGapMs: number = 60000,
): [number, number] {
  let firstTs = Infinity;
  for (const points of Object.values(series)) {
    if (points.length > 0 && points[0].t < firstTs) {
      firstTs = points[0].t;
    }
  }

  if (firstTs === Infinity || firstTs >= domain[1]) {
    return domain;
  }

  return firstTs - domain[0] > minGapMs ? [firstTs, domain[1]] : domain;
}

// ---------------------------------------------------------------------------
// Period date picker helpers
// ---------------------------------------------------------------------------
//
// The Older/Newer switcher steps the selected period back/forward by one
// window. For calendar-aligned ranges ("today", "month") a period maps to an
// exact calendar day/month, so we expose a native date/month picker so users
// can jump straight to a date. Rolling ranges (1h, 6h, 24h, 7d, …) are
// anchored at "now" and step by a fixed window size, so a picked date has no
// unambiguous mapping — those keep the textual label.

/** Format a Date as a local `YYYY-MM-DD` string (no timezone conversion). */
function toLocalDateString(d: Date): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}

/** Format a Date as a local `YYYY-MM` string (no timezone conversion). */
function toLocalMonthString(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`;
}

/**
 * Whether the period switcher should render a native date/month picker for the
 * given range. Only calendar-aligned ranges ("today", "month") map cleanly to
 * a picked date; rolling ranges keep the textual label.
 */
export function supportsHistoryDate(range: HistoryRange): boolean {
  return range === 'today' || range === 'month';
}

/** Native input type to render for the picker: `'month'` for month ranges, else `'date'`. */
export function historyPickerInputType(range: HistoryRange): 'month' | 'date' {
  return range === 'month' ? 'month' : 'date';
}

/** The picker value (`YYYY-MM-DD` or `YYYY-MM`) for the current offset. */
export function getHistoryPickerValue(range: HistoryRange, offset: number, nowMs: number = Date.now()): string {
  const now = new Date(nowMs);
  if (range === 'month') {
    let m = now.getMonth() - offset;
    let y = now.getFullYear();
    while (m < 0) {
      m += 12;
      y -= 1;
    }
    return toLocalMonthString(new Date(y, m, 1));
  }
  // 'today' — calendar day
  return toLocalDateString(new Date(now.getFullYear(), now.getMonth(), now.getDate() - offset));
}

/** Newest selectable value (today's day, or the current month). */
export function getHistoryPickerMax(range: HistoryRange, nowMs: number = Date.now()): string {
  const now = new Date(nowMs);
  return range === 'month' ? toLocalMonthString(now) : toLocalDateString(now);
}

/** Convert a picked picker value back to a non-negative offset. */
export function historyPickerValueToOffset(range: HistoryRange, value: string, nowMs: number = Date.now()): number {
  if (range === 'month') {
    const [yStr, mStr] = value.split('-');
    const now = new Date(nowMs);
    const months = (now.getFullYear() - Number(yStr)) * 12 + (now.getMonth() - (Number(mStr) - 1));
    return Math.max(0, months);
  }
  // 'today'
  const now = new Date(nowMs);
  const todayMidnight = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const [yStr, mStr, dStr] = value.split('-');
  const pickedMidnight = new Date(Number(yStr), Number(mStr) - 1, Number(dStr));
  const days = Math.round((todayMidnight.getTime() - pickedMidnight.getTime()) / 86400000);
  return Math.max(0, days);
}
