import { describe, it, expect } from 'vitest';
import {
  computeImportCost,
  computeExportIncome,
  computeCombinedCost,
} from '../../src/lib/costChart';
import { flatTariffConfig } from '../../src/lib/tariff';
import type { TariffConfig } from '../../src/lib/types';

/**
 * Tests for `computeImportCost` — the pure-data helper that powers the
 * "Import Cost" series on the History page's Cost tab.
 *
 * The function takes a row per bucketed timestamp and adds a `_import_cost`
 * field equal to the cumulative import cost in £, computed from
 * `today_import_kwh` deltas and the import tariff config. Missing data is
 * represented as `NaN` so the chart leaves a gap rather than drawing a
 * misleading zero.
 *
 * Important: the first row with a value initialises the baseline (`prev`)
 * but produces no delta — cost only starts accumulating from the second row
 * onward. This matches the original inline preprocess in HistoryPage.tsx.
 */

const FLAT_15P = flatTariffConfig(0.15);
const FLAT_10P = flatTariffConfig(0.10);

describe('computeImportCost', () => {
  it('accumulates cost from monotonic import counter deltas', () => {
    // Row 0: prev=0, no delta → £0.00
    // Row 1: delta=1 kWh at 15p → £0.15
    // Row 2: delta=2 kWh at 15p → £0.45 total
    const rows = [
      { t: 1000, today_import_kwh: 0 },
      { t: 2000, today_import_kwh: 1 },
      { t: 3000, today_import_kwh: 3 },
    ];
    const result = computeImportCost(rows, FLAT_15P);
    expect(result[0]._import_cost).toBeCloseTo(0, 5);
    expect(result[1]._import_cost).toBeCloseTo(0.15, 5);
    expect(result[2]._import_cost).toBeCloseTo(0.45, 5);
  });

  it('handles midnight rollover (counter reset to near-zero)', () => {
    // Row 0: prev=12, no delta → £0.00
    // Row 1: prev=12, raw=2, rollover detected, delta=2 kWh at 15p → £0.30
    const rows = [
      { t: 1000, today_import_kwh: 12 },
      { t: 2000, today_import_kwh: 2 },
    ];
    const result = computeImportCost(rows, FLAT_15P);
    expect(result[1]._import_cost).toBeCloseTo(0.30, 5);
  });

  it('skips small counter dips (data glitch)', () => {
    // Row 0: prev=5, no delta → £0.00
    // Row 1: raw=4.9 < prev=5, not a rollover (prev not > 5), delta stays 0
    const rows = [
      { t: 1000, today_import_kwh: 5 },
      { t: 2000, today_import_kwh: 4.9 },
    ];
    const result = computeImportCost(rows, FLAT_15P);
    expect(result[1]._import_cost).toBeCloseTo(0, 5);
  });

  it('clamps spikes > 2 kWh to zero and does not advance prev', () => {
    // Row 0: prev=5, no delta → £0.00
    // Row 1: delta=45 > 2, clamped to 0, prev stays at 5 → £0.00
    // Row 2: raw=52, prev=5, delta=47 > 2, clamped to 0 → £0.00
    const rows = [
      { t: 1000, today_import_kwh: 5 },
      { t: 2000, today_import_kwh: 50 },
      { t: 3000, today_import_kwh: 52 },
    ];
    const result = computeImportCost(rows, FLAT_15P);
    expect(result[1]._import_cost).toBeCloseTo(0, 5);
    expect(result[2]._import_cost).toBeCloseTo(0, 5);
  });

  it('returns NaN for _import_cost when today_import_kwh is missing', () => {
    const rows = [{ t: 1000 }];
    const result = computeImportCost(rows, FLAT_15P);
    expect(Number.isNaN(result[0]._import_cost)).toBe(true);
  });

  it('returns NaN for a row with missing field between valid rows', () => {
    // Row 0: initialises prev → £0.00
    // Row 1: field missing → NaN (gap in chart)
    // Row 2: delta from prev (still 1) to raw=3 → £0.30
    const rows = [
      { t: 1000, today_import_kwh: 1 },
      { t: 2000 },
      { t: 3000, today_import_kwh: 3 },
    ];
    const result = computeImportCost(rows, FLAT_15P);
    expect(result[0]._import_cost).toBeCloseTo(0, 5);
    expect(Number.isNaN(result[1]._import_cost)).toBe(true);
    expect(result[2]._import_cost).toBeCloseTo(0.30, 5);
  });

  it('returns an empty array for an empty input', () => {
    expect(computeImportCost([], FLAT_15P)).toEqual([]);
  });

  it('preserves the input row shape for downstream chart fields', () => {
    // Two rows so we get a non-zero cost
    const rows = [
      { t: 1000, today_import_kwh: 0, today_export_kwh: 0.5 },
      { t: 2000, today_import_kwh: 1, today_export_kwh: 1.0 },
    ];
    const result = computeImportCost(rows, FLAT_15P);
    expect(result[0].t).toBe(1000);
    expect(result[0].today_import_kwh).toBe(0);
    expect(result[0].today_export_kwh).toBe(0.5);
    expect(result[0]._import_cost).toBeCloseTo(0, 5);
    expect(result[1]._import_cost).toBeCloseTo(0.15, 5);
  });

  it('does not mutate the input rows', () => {
    const rows = [{ t: 1000, today_import_kwh: 5 }];
    const original = JSON.parse(JSON.stringify(rows));
    computeImportCost(rows, FLAT_15P);
    expect(rows).toEqual(original);
  });

  it('handles many rows in a single pass', () => {
    const rows = Array.from({ length: 100 }, (_, i) => ({
      t: 1000 + i * 60_000,
      today_import_kwh: i * 0.5,
    }));
    const result = computeImportCost(rows, FLAT_15P);
    expect(result).toHaveLength(100);
    // Row 0: no delta → £0.00
    // Rows 1-99: each step 0.5 kWh at 15p = £0.075, 99 steps = £7.425
    expect(result[0]._import_cost).toBeCloseTo(0, 5);
    expect(result[99]._import_cost).toBeCloseTo(99 * 0.075, 5);
  });
});

/**
 * Tests for `computeExportIncome` — the pure-data helper that powers the
 * "Export Income" series on the History page's Cost tab.
 *
 * Adds a `_export_income` field equal to the cumulative export income in £,
 * computed from `today_export_kwh` deltas and the export tariff config.
 * Missing data is represented as `NaN` so the chart leaves a gap rather
 * than drawing a misleading zero.
 */
describe('computeExportIncome', () => {
  it('accumulates income from monotonic export counter deltas', () => {
    // Row 0: prev=0, no delta → £0.00
    // Row 1: delta=1 kWh at 15p → £0.15
    // Row 2: delta=1 kWh at 15p → £0.30 total
    const rows = [
      { t: 1000, today_export_kwh: 0 },
      { t: 2000, today_export_kwh: 1 },
      { t: 3000, today_export_kwh: 2 },
    ];
    const result = computeExportIncome(rows, FLAT_15P);
    expect(result[0]._export_income).toBeCloseTo(0, 5);
    expect(result[1]._export_income).toBeCloseTo(0.15, 5);
    expect(result[2]._export_income).toBeCloseTo(0.30, 5);
  });

  it('handles midnight rollover for export counter', () => {
    // Row 0: prev=15, no delta → £0.00
    // Row 1: prev=15, raw=1, rollover detected, delta=1 kWh at 15p → £0.15
    const rows = [
      { t: 1000, today_export_kwh: 15 },
      { t: 2000, today_export_kwh: 1 },
    ];
    const result = computeExportIncome(rows, FLAT_15P);
    expect(result[1]._export_income).toBeCloseTo(0.15, 5);
  });

  it('clamps spikes > 2 kWh to zero', () => {
    const rows = [
      { t: 1000, today_export_kwh: 3 },
      { t: 2000, today_export_kwh: 30 },
    ];
    const result = computeExportIncome(rows, FLAT_15P);
    expect(result[1]._export_income).toBeCloseTo(0, 5);
  });

  it('returns NaN for _export_income when today_export_kwh is missing', () => {
    const rows = [{ t: 1000 }];
    const result = computeExportIncome(rows, FLAT_15P);
    expect(Number.isNaN(result[0]._export_income)).toBe(true);
  });

  it('returns NaN for a row with missing field between valid rows', () => {
    const rows = [
      { t: 1000, today_export_kwh: 1 },
      { t: 2000 },
      { t: 3000, today_export_kwh: 3 },
    ];
    const result = computeExportIncome(rows, FLAT_15P);
    expect(result[0]._export_income).toBeCloseTo(0, 5);
    expect(Number.isNaN(result[1]._export_income)).toBe(true);
    expect(result[2]._export_income).toBeCloseTo(0.30, 5);
  });

  it('returns an empty array for an empty input', () => {
    expect(computeExportIncome([], FLAT_15P)).toEqual([]);
  });

  it('preserves the input row shape', () => {
    const rows = [
      { t: 1000, today_export_kwh: 0, today_import_kwh: 1 },
      { t: 2000, today_export_kwh: 2, today_import_kwh: 1 },
    ];
    const result = computeExportIncome(rows, FLAT_15P);
    expect(result[0].t).toBe(1000);
    expect(result[0].today_export_kwh).toBe(0);
    expect(result[0].today_import_kwh).toBe(1);
    expect(result[0]._export_income).toBeCloseTo(0, 5);
    expect(result[1]._export_income).toBeCloseTo(0.30, 5);
  });

  it('does not mutate the input rows', () => {
    const rows = [{ t: 1000, today_export_kwh: 5 }];
    const original = JSON.parse(JSON.stringify(rows));
    computeExportIncome(rows, FLAT_15P);
    expect(rows).toEqual(original);
  });

  it('handles many rows in a single pass', () => {
    const rows = Array.from({ length: 100 }, (_, i) => ({
      t: 1000 + i * 60_000,
      today_export_kwh: i * 0.3,
    }));
    const result = computeExportIncome(rows, FLAT_15P);
    expect(result).toHaveLength(100);
    // Row 0: no delta → £0.00
    // Rows 1-99: each step 0.3 kWh at 15p = £0.045, 99 steps = £4.455
    expect(result[0]._export_income).toBeCloseTo(0, 5);
    expect(result[99]._export_income).toBeCloseTo(99 * 0.045, 5);
  });
});

/**
 * Tests for `computeCombinedCost` — the combined preprocess that computes
 * both import cost and export income in a single pass.
 *
 * Adds both `_import_cost` and `_export_income` fields to each row.
 */
describe('computeCombinedCost', () => {
  it('computes both import cost and export income', () => {
    const rows = [
      { t: 1000, today_import_kwh: 0, today_export_kwh: 0 },
      { t: 2000, today_import_kwh: 1, today_export_kwh: 2 },
      { t: 3000, today_import_kwh: 3, today_export_kwh: 4 },
    ];
    const result = computeCombinedCost(rows, FLAT_15P, FLAT_10P);
    expect(result).toHaveLength(3);
    // Import: row1 delta=1 kWh at 15p = £0.15, row2 delta=2 kWh = £0.45 total
    expect(result[1]._import_cost).toBeCloseTo(0.15, 5);
    expect(result[2]._import_cost).toBeCloseTo(0.45, 5);
    // Export: row1 delta=2 kWh at 10p = £0.20, row2 delta=2 kWh = £0.40 total
    expect(result[1]._export_income).toBeCloseTo(0.20, 5);
    expect(result[2]._export_income).toBeCloseTo(0.40, 5);
  });

  it('handles missing fields independently', () => {
    // Only import data present, export missing
    const rows = [
      { t: 1000, today_import_kwh: 0 },
      { t: 2000, today_import_kwh: 2 },
    ];
    const result = computeCombinedCost(rows, FLAT_15P, FLAT_10P);
    expect(result[0]._import_cost).toBeCloseTo(0, 5);
    expect(result[1]._import_cost).toBeCloseTo(0.30, 5);
    expect(Number.isNaN(result[0]._export_income)).toBe(true);
    expect(Number.isNaN(result[1]._export_income)).toBe(true);
  });

  it('returns an empty array for an empty input', () => {
    expect(computeCombinedCost([], FLAT_15P, FLAT_10P)).toEqual([]);
  });

  it('preserves the input row shape', () => {
    const rows = [
      { t: 1000, today_import_kwh: 0, today_export_kwh: 0, soc: 50 },
      { t: 2000, today_import_kwh: 1, today_export_kwh: 2, soc: 55 },
    ];
    const result = computeCombinedCost(rows, FLAT_15P, FLAT_10P);
    expect(result[0].t).toBe(1000);
    expect(result[0].today_import_kwh).toBe(0);
    expect(result[0].today_export_kwh).toBe(0);
    expect(result[0].soc).toBe(50);
    expect(result[0]._import_cost).toBeCloseTo(0, 5);
    expect(result[0]._export_income).toBeCloseTo(0, 5);
    expect(result[1]._import_cost).toBeCloseTo(0.15, 5);
    expect(result[1]._export_income).toBeCloseTo(0.20, 5);
  });

  it('does not mutate the input rows', () => {
    const rows = [
      { t: 1000, today_import_kwh: 1, today_export_kwh: 2 },
    ];
    const original = JSON.parse(JSON.stringify(rows));
    computeCombinedCost(rows, FLAT_15P, FLAT_10P);
    expect(rows).toEqual(original);
  });

  it('uses different tariff rates for import and export', () => {
    // Import at 30p, export at 5p
    const importCfg = flatTariffConfig(0.30);
    const exportCfg = flatTariffConfig(0.05);
    const rows = [
      { t: 1000, today_import_kwh: 0, today_export_kwh: 0 },
      { t: 2000, today_import_kwh: 2, today_export_kwh: 1.5 },
    ];
    const result = computeCombinedCost(rows, importCfg, exportCfg);
    // Import: 2 kWh at 30p = £0.60
    expect(result[1]._import_cost).toBeCloseTo(0.60, 5);
    // Export: 1.5 kWh at 5p = £0.075
    expect(result[1]._export_income).toBeCloseTo(0.075, 5);
  });

  it('handles many rows with both counters', () => {
    const rows = Array.from({ length: 50 }, (_, i) => ({
      t: 1000 + i * 60_000,
      today_import_kwh: i * 0.4,
      today_export_kwh: i * 0.6,
    }));
    const result = computeCombinedCost(rows, FLAT_15P, FLAT_10P);
    expect(result).toHaveLength(50);
    // Import: each step 0.4 kWh at 15p = £0.06, 49 steps = £2.94
    expect(result[49]._import_cost).toBeCloseTo(49 * 0.06, 5);
    // Export: each step 0.6 kWh at 10p = £0.06, 49 steps = £2.94
    expect(result[49]._export_income).toBeCloseTo(49 * 0.06, 5);
  });
});
