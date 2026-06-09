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

export const HISTORY_CHART_GRID_PROPS = {
  strokeDasharray: '4 4',
  stroke: 'var(--color-grid-stroke)',
  strokeWidth: 2,
} as const;

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
