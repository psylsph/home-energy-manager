import type { BatteryHealthDay } from './types';

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

export const HEALTH_RANGES = [
  { label: '3m', days: 90 },
  { label: '6m', days: 180 },
  { label: '1y', days: 365 },
  { label: 'All', days: 3650 },
] as const;
