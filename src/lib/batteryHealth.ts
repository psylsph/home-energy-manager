import type { BatteryHealthDay, BatteryHealthTrend } from './types';

// Pure helpers for the Battery Health panel (issue #393): the live cell
// spread, and the pivot from one-row-per-module-per-day into the
// one-row-per-day-with-a-column-per-module shape Recharts draws.

/** A cell reading outside this range is a failed read, not a cell (mirrors the recorder). */
const CELL_VOLTS_MIN = 0.5;
const CELL_VOLTS_MAX = 4.5;

/** Live max-minus-min cell voltage in mV, or null when it can't be trusted (fewer than 2 cells, or any failed read). */
export function cellSpreadMv(cells: readonly number[] | undefined): number | null {
  if (!cells || cells.length < 2) return null;
  if (cells.some((v) => !Number.isFinite(v) || v < CELL_VOLTS_MIN || v > CELL_VOLTS_MAX)) return null;
  return (Math.max(...cells) - Math.min(...cells)) * 1000;
}

export type HealthMetric = 'full_spread_mv' | 'max_spread_mv' | 'capacity_pct' | 'cycles' | 'max_temp_c';

/** Battery capacity as a percentage of design capacity, or null when either is unknown. */
export function capacityPercent(row: Pick<BatteryHealthDay, 'capacity_ah' | 'design_capacity_ah'>): number | null {
  if (!row.capacity_ah || !row.design_capacity_ah) return null;
  return (row.capacity_ah / row.design_capacity_ah) * 100;
}

export function metricValue(row: BatteryHealthDay, metric: HealthMetric): number | null {
  return metric === 'capacity_pct' ? capacityPercent(row) : row[metric];
}

export interface HealthModule {
  /** Series key for chart rows: one per module slot. */
  key: string;
  /** Slot number shown to the user (1-based). */
  label: string;
}

/** Modules are told apart by slot, not serial: a corrupt serial read must not split a battery into several lines. */
export function moduleKey(moduleIndex: number): string {
  return `m${moduleIndex}`;
}

/** The module slots present in `rows`, in slot order. */
export function healthModules(rows: readonly BatteryHealthDay[]): HealthModule[] {
  const slots = [...new Set(rows.map((row) => row.module_index))].sort((a, b) => a - b);
  return slots.map((slot) => ({ key: moduleKey(slot), label: `#${slot + 1}` }));
}

export interface HealthChartRow {
  /** Local noon of the day, ms — a numeric x so gaps between recorded days stay to scale. */
  t: number;
  day: string;
  /** One column per module slot ({@link moduleKey}); absent when that module had no value that day. */
  [key: string]: number | string | null;
}

/** Local noon of a `YYYY-MM-DD` day in ms, or NaN for a malformed day. */
export function dayToMs(day: string): number {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(day);
  if (!m) return NaN;
  return new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]), 12).getTime();
}

const DAY_MS = 86_400_000;

/** Longest span, in days, labelled on whole days rather than months: covers the 3m range. */
const DAY_TICKS_MAX_SPAN = 100;

export type HealthTickUnit = 'day' | 'month';

/**
 * X-axis ticks for a daily health chart. Up to about three months they fall on
 * local midday (as `dayToMs`) of whole days, every day while that fits in
 * `maxTicks`, otherwise an even step counted back so the newest day is always
 * labelled. Longer spans use month starts, labelled with the year, so a year
 * or more never repeats a label. Charts pass these with `interval={0}` so no
 * label is dropped.
 */
export function healthAxis(
  times: readonly number[],
  maxTicks = 4,
): { ticks: number[]; unit: HealthTickUnit } {
  const limit = Math.max(2, Math.floor(maxTicks));
  const valid = times.filter((t) => Number.isFinite(t));
  if (valid.length === 0) return { ticks: [], unit: 'day' };
  const first = Math.min(...valid);
  const lastMs = Math.max(...valid);
  const last = new Date(lastMs);
  // Rounded, so a clock change (a 23 or 25 hour day) still counts as one day.
  const spanDays = Math.round((lastMs - first) / DAY_MS);
  const ticks: number[] = [];

  if (spanDays <= DAY_TICKS_MAX_SPAN) {
    const step = spanDays < limit ? 1 : Math.ceil(spanDays / (limit - 1));
    for (let back = 0; back <= spanDays; back += step) {
      ticks.push(new Date(last.getFullYear(), last.getMonth(), last.getDate() - back, 12).getTime());
    }
    return { ticks: ticks.reverse(), unit: 'day' };
  }

  // Month starts inside the data, counted back from the newest one. A first
  // day after the 1st starts the count at the next month, so the step is
  // sized to the labels that can actually be drawn.
  const firstDate = new Date(first);
  const firstMonth = firstDate.getFullYear() * 12 + firstDate.getMonth() + (firstDate.getDate() > 1 ? 1 : 0);
  const months = last.getFullYear() * 12 + last.getMonth() - firstMonth;
  const step = Math.ceil((months + 1) / limit);
  for (let back = 0; back <= months; back += step) {
    ticks.push(new Date(last.getFullYear(), last.getMonth() - back, 1, 12).getTime());
  }
  return { ticks: ticks.reverse(), unit: 'month' };
}

/** Axis label for a tick from `healthAxis`. */
export function formatHealthTick(ms: number, unit: HealthTickUnit): string {
  return unit === 'month'
    ? new Date(ms).toLocaleDateString(undefined, { month: 'short', year: 'numeric' })
    : new Date(ms).toLocaleDateString(undefined, { day: 'numeric', month: 'short' });
}

/** Pivot daily rows into chart rows for `metric`, oldest day first; days with no value for any module are dropped. */
export function pivotHealth(rows: readonly BatteryHealthDay[], metric: HealthMetric): HealthChartRow[] {
  const byDay = new Map<string, HealthChartRow>();
  for (const row of rows) {
    const value = metricValue(row, metric);
    if (value === null || !Number.isFinite(value)) continue;
    const t = dayToMs(row.day);
    if (Number.isNaN(t)) continue;
    let entry = byDay.get(row.day);
    if (!entry) {
      entry = { t, day: row.day };
      byDay.set(row.day, entry);
    }
    entry[moduleKey(row.module_index)] = value;
  }
  return [...byDay.values()].sort((a, b) => a.t - b.t);
}

/** Whether any row has a value for `metric`. */
export function hasMetric(rows: readonly BatteryHealthDay[], metric: HealthMetric): boolean {
  return rows.some((row) => {
    const v = metricValue(row, metric);
    return v !== null && Number.isFinite(v);
  });
}

export interface ModuleHealthSummary {
  key: string;
  label: string;
  /** The module's most recent day. */
  day: string;
  /** Latest cell spread: near full when the module has ever been, else the day's worst. */
  spreadMv: number | null;
  spreadKind: 'full' | 'worst' | null;
  /** The day `spreadMv` was recorded on. */
  spreadDay: string | null;
  cycles: number | null;
  capacityPct: number | null;
  bmsFirmware: number | null;
}

/** The newest row (rows are oldest first per module) that has a value for `pick`. */
function latestWith<T>(
  rows: readonly BatteryHealthDay[],
  pick: (row: BatteryHealthDay) => T | null,
): { value: T; day: string } | null {
  for (let i = rows.length - 1; i >= 0; i -= 1) {
    const value = pick(rows[i]);
    if (value !== null) return { value, day: rows[i].day };
  }
  return null;
}

/**
 * Each module's current figures. Every figure comes from the latest day that
 * has it, so a partial day (no near-full reading yet, no capacity read yet)
 * neither blanks a figure nor swaps one measure for another.
 */
export function latestPerModule(rows: readonly BatteryHealthDay[]): ModuleHealthSummary[] {
  const byModule = new Map<number, BatteryHealthDay[]>();
  for (const row of [...rows].sort((a, b) => a.day.localeCompare(b.day))) {
    const list = byModule.get(row.module_index);
    if (list) list.push(row);
    else byModule.set(row.module_index, [row]);
  }
  return healthModules(rows).map(({ key, label }) => {
    const mine = byModule.get(Number(key.slice(1)))!;
    const full = latestWith(mine, (r) => r.full_spread_mv);
    const spread = full ?? latestWith(mine, (r) => r.max_spread_mv);
    return {
      key,
      label,
      day: mine[mine.length - 1].day,
      spreadMv: spread?.value ?? null,
      spreadKind: spread ? (full ? 'full' : 'worst') : null,
      spreadDay: spread?.day ?? null,
      cycles: latestWith(mine, (r) => r.cycles)?.value ?? null,
      capacityPct: latestWith(mine, capacityPercent)?.value ?? null,
      bmsFirmware: latestWith(mine, (r) => r.bms_firmware)?.value ?? null,
    };
  });
}

export interface TrendView {
  tone: 'warn' | 'ok' | 'muted';
  text: string;
}

/** One line on a module's cell spread trend, or null when the backend sent none for it. */
export function describeTrend(trend: BatteryHealthTrend | undefined): TrendView | null {
  if (!trend) return null;
  const mv = (v: number | null) => (v === null ? '—' : Math.round(v).toString());
  switch (trend.status) {
    case 'widening':
      return { tone: 'warn', text: `Widening: ${mv(trend.baseline_mv)} → ${mv(trend.recent_mv)} mV near full` };
    case 'high':
      return { tone: 'warn', text: `High: ${mv(trend.recent_mv)} mV near full` };
    case 'ok':
      return { tone: 'ok', text: `Steady: ${mv(trend.recent_mv)} mV near full` };
    default:
      return trend.recent_days < trend.recent_min_days
        ? {
            tone: 'muted',
            text: `Needs more near-full days (${trend.recent_days} of ${trend.recent_min_days} this week)`,
          }
        : { tone: 'muted', text: 'Building a baseline: about a month of near-full days' };
  }
}

export interface SocEventDay {
  day: string;
  module_index: number;
  jumps: number;
  stuck: number;
}

/** Days with SOC jumps or stuck episodes, newest first, then by module slot. */
export function socEventDays(rows: readonly BatteryHealthDay[]): SocEventDay[] {
  return rows
    .map((row) => ({
      day: row.day,
      module_index: row.module_index,
      jumps: row.soc_jumps ?? 0,
      stuck: row.soc_stuck ?? 0,
    }))
    .filter((e) => e.jumps > 0 || e.stuck > 0)
    .sort((a, b) => b.day.localeCompare(a.day) || a.module_index - b.module_index);
}

function times(n: number): string {
  return n === 1 ? 'once' : n === 2 ? 'twice' : `${n} times`;
}

/** "SOC jumped twice and stuck once", for one {@link SocEventDay}. */
export function describeSocEvents(event: Pick<SocEventDay, 'jumps' | 'stuck'>): string {
  const parts = [];
  if (event.jumps > 0) parts.push(`jumped ${times(event.jumps)}`);
  if (event.stuck > 0) parts.push(`stuck ${times(event.stuck)}`);
  return `SOC ${parts.join(' and ')}`;
}

export const HEALTH_RANGES = [
  { label: '3m', days: 90 },
  { label: '6m', days: 180 },
  { label: '1y', days: 365 },
  { label: 'All', days: 3650 },
] as const;
