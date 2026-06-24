import { describe, it, expect } from 'vitest';
import {
  computeImportCost,
  computeExportIncome,
  computeCombinedCost,
} from '../../src/lib/costChart';
import { flatTariffConfig } from '../../src/lib/tariff';

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
 *
 * The third argument is `bucketSecs` — the backend aggregation bucket size
 * for the range, which sets the per-bucket spike ceiling (see issue #133).
 * `BUCKET_30M` (1800 s, the 7d range) gives a ceiling of 15 kW × 0.5 h =
 * 7.5 kWh, generous enough that the accumulation tests below never trip it.
 */

const FLAT_15P = flatTariffConfig(0.15);
const FLAT_10P = flatTariffConfig(0.10);

/** 30-minute buckets — the 7d range. Ceiling = 15 kW × 0.5 h = 7.5 kWh. */
const BUCKET_30M = 1800;

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
    const result = computeImportCost(rows, FLAT_15P, BUCKET_30M);
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
    const result = computeImportCost(rows, FLAT_15P, BUCKET_30M);
    expect(result[1]._import_cost).toBeCloseTo(0.30, 5);
  });

  it('skips small counter dips (data glitch)', () => {
    // Row 0: prev=5, no delta → £0.00
    // Row 1: raw=4.9 < prev=5, not a rollover (prev not > 5), delta stays 0
    const rows = [
      { t: 1000, today_import_kwh: 5 },
      { t: 2000, today_import_kwh: 4.9 },
    ];
    const result = computeImportCost(rows, FLAT_15P, BUCKET_30M);
    expect(result[1]._import_cost).toBeCloseTo(0, 5);
  });

  it('counts a delta under the bucket-scaled ceiling', () => {
    // 30-min buckets: ceiling = 15 kW × 0.5 h = 7.5 kWh. A 5 kWh jump
    // (~10 kW sustained for 30 min) is large but plausible → counted. The
    // old fixed 2 kWh ceiling would have zeroed this.
    const rows = [
      { t: 0, today_import_kwh: 0 },
      { t: 1_800_000, today_import_kwh: 5 },
    ];
    const result = computeImportCost(rows, FLAT_15P, BUCKET_30M);
    expect(result[1]._import_cost).toBeCloseTo(5 * 0.15, 5); // £0.75
  });

  it('clamps a delta over the bucket-scaled ceiling and does not advance prev', () => {
    // 30-min buckets: ceiling = 7.5 kWh. A 45 kWh jump is impossible → clamped,
    // and prev is frozen so the corruption can't inflate the baseline. The
    // follow-up catch-up delta (47 kWh) is also over the ceiling → still £0.
    const rows = [
      { t: 0, today_import_kwh: 5 },
      { t: 1_800_000, today_import_kwh: 50 },
      { t: 3_600_000, today_import_kwh: 52 },
    ];
    const result = computeImportCost(rows, FLAT_15P, BUCKET_30M);
    expect(result[1]._import_cost).toBeCloseTo(0, 5);
    expect(result[2]._import_cost).toBeCloseTo(0, 5);
  });

  it('returns NaN for _import_cost when today_import_kwh is missing', () => {
    const rows = [{ t: 1000 }];
    const result = computeImportCost(rows, FLAT_15P, BUCKET_30M);
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
    const result = computeImportCost(rows, FLAT_15P, BUCKET_30M);
    expect(result[0]._import_cost).toBeCloseTo(0, 5);
    expect(Number.isNaN(result[1]._import_cost)).toBe(true);
    expect(result[2]._import_cost).toBeCloseTo(0.30, 5);
  });

  it('returns an empty array for an empty input', () => {
    expect(computeImportCost([], FLAT_15P, BUCKET_30M)).toEqual([]);
  });

  it('preserves the input row shape for downstream chart fields', () => {
    // Two rows so we get a non-zero cost
    const rows = [
      { t: 1000, today_import_kwh: 0, today_export_kwh: 0.5 },
      { t: 2000, today_import_kwh: 1, today_export_kwh: 1.0 },
    ];
    const result = computeImportCost(rows, FLAT_15P, BUCKET_30M);
    expect(result[0].t).toBe(1000);
    expect(result[0].today_import_kwh).toBe(0);
    expect(result[0].today_export_kwh).toBe(0.5);
    expect(result[0]._import_cost).toBeCloseTo(0, 5);
    expect(result[1]._import_cost).toBeCloseTo(0.15, 5);
  });

  it('does not mutate the input rows', () => {
    const rows = [{ t: 1000, today_import_kwh: 5 }];
    const original = JSON.parse(JSON.stringify(rows));
    computeImportCost(rows, FLAT_15P, BUCKET_30M);
    expect(rows).toEqual(original);
  });

  it('handles many rows in a single pass', () => {
    const rows = Array.from({ length: 100 }, (_, i) => ({
      t: 1000 + i * 60_000,
      today_import_kwh: i * 0.5,
    }));
    const result = computeImportCost(rows, FLAT_15P, BUCKET_30M);
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
    const result = computeExportIncome(rows, FLAT_15P, BUCKET_30M);
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
    const result = computeExportIncome(rows, FLAT_15P, BUCKET_30M);
    expect(result[1]._export_income).toBeCloseTo(0.15, 5);
  });

  it('clamps a delta over the bucket-scaled ceiling', () => {
    // 30-min buckets: ceiling = 7.5 kWh. A 27 kWh jump is impossible → clamped.
    const rows = [
      { t: 0, today_export_kwh: 3 },
      { t: 1_800_000, today_export_kwh: 30 },
    ];
    const result = computeExportIncome(rows, FLAT_15P, BUCKET_30M);
    expect(result[1]._export_income).toBeCloseTo(0, 5);
  });

  it('returns NaN for _export_income when today_export_kwh is missing', () => {
    const rows = [{ t: 1000 }];
    const result = computeExportIncome(rows, FLAT_15P, BUCKET_30M);
    expect(Number.isNaN(result[0]._export_income)).toBe(true);
  });

  it('returns NaN for a row with missing field between valid rows', () => {
    const rows = [
      { t: 1000, today_export_kwh: 1 },
      { t: 2000 },
      { t: 3000, today_export_kwh: 3 },
    ];
    const result = computeExportIncome(rows, FLAT_15P, BUCKET_30M);
    expect(result[0]._export_income).toBeCloseTo(0, 5);
    expect(Number.isNaN(result[1]._export_income)).toBe(true);
    expect(result[2]._export_income).toBeCloseTo(0.30, 5);
  });

  it('returns an empty array for an empty input', () => {
    expect(computeExportIncome([], FLAT_15P, BUCKET_30M)).toEqual([]);
  });

  it('preserves the input row shape', () => {
    const rows = [
      { t: 1000, today_export_kwh: 0, today_import_kwh: 1 },
      { t: 2000, today_export_kwh: 2, today_import_kwh: 1 },
    ];
    const result = computeExportIncome(rows, FLAT_15P, BUCKET_30M);
    expect(result[0].t).toBe(1000);
    expect(result[0].today_export_kwh).toBe(0);
    expect(result[0].today_import_kwh).toBe(1);
    expect(result[0]._export_income).toBeCloseTo(0, 5);
    expect(result[1]._export_income).toBeCloseTo(0.30, 5);
  });

  it('does not mutate the input rows', () => {
    const rows = [{ t: 1000, today_export_kwh: 5 }];
    const original = JSON.parse(JSON.stringify(rows));
    computeExportIncome(rows, FLAT_15P, BUCKET_30M);
    expect(rows).toEqual(original);
  });

  it('handles many rows in a single pass', () => {
    const rows = Array.from({ length: 100 }, (_, i) => ({
      t: 1000 + i * 60_000,
      today_export_kwh: i * 0.3,
    }));
    const result = computeExportIncome(rows, FLAT_15P, BUCKET_30M);
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
    const result = computeCombinedCost(rows, FLAT_15P, FLAT_10P, BUCKET_30M);
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
    const result = computeCombinedCost(rows, FLAT_15P, FLAT_10P, BUCKET_30M);
    expect(result[0]._import_cost).toBeCloseTo(0, 5);
    expect(result[1]._import_cost).toBeCloseTo(0.30, 5);
    expect(Number.isNaN(result[0]._export_income)).toBe(true);
    expect(Number.isNaN(result[1]._export_income)).toBe(true);
  });

  it('returns an empty array for an empty input', () => {
    expect(computeCombinedCost([], FLAT_15P, FLAT_10P, BUCKET_30M)).toEqual([]);
  });

  it('preserves the input row shape', () => {
    const rows = [
      { t: 1000, today_import_kwh: 0, today_export_kwh: 0, soc: 50 },
      { t: 2000, today_import_kwh: 1, today_export_kwh: 2, soc: 55 },
    ];
    const result = computeCombinedCost(rows, FLAT_15P, FLAT_10P, BUCKET_30M);
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
    computeCombinedCost(rows, FLAT_15P, FLAT_10P, BUCKET_30M);
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
    const result = computeCombinedCost(rows, importCfg, exportCfg, BUCKET_30M);
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
    const result = computeCombinedCost(rows, FLAT_15P, FLAT_10P, BUCKET_30M);
    expect(result).toHaveLength(50);
    // Import: each step 0.4 kWh at 15p = £0.06, 49 steps = £2.94
    expect(result[49]._import_cost).toBeCloseTo(49 * 0.06, 5);
    // Export: each step 0.6 kWh at 10p = £0.06, 49 steps = £2.94
    expect(result[49]._export_income).toBeCloseTo(49 * 0.06, 5);
  });
});

/**
 * Regression tests for issue #133: the per-bucket spike ceiling must scale
 * with bucket width, otherwise legitimate per-bucket energy gets silently
 * discarded on wide ranges and the cumulative Cost totals shrink as the
 * range widens (e.g. Export Income £12 → £2.40 → £0.32 for the same data).
 *
 * The old code used a fixed 2 kWh ceiling regardless of bucket size. With a
 * 12-hour bucket (6m range) almost every real bucket exceeded 2 kWh and was
 * zeroed. The fix scales the ceiling by `MAX_PLAUSIBLE_POWER_KW × bucketHours`
 * (plus actual elapsed time, to tolerate data gaps).
 */
describe('issue #133 — bucket-width-aware spike ceiling', () => {
  it('counts legitimate per-bucket energy that the old 2 kWh flat cap would discard', () => {
    // 3 kWh of export in a single 1-hour bucket (month range): 3 kW sustained.
    // The old fixed 2 kWh ceiling zeroed this; the scaled ceiling
    // (15 kW × 1 h = 15 kWh) counts it.
    const rows = [
      { t: 0, today_export_kwh: 0 },
      { t: 3_600_000, today_export_kwh: 3 },
    ];
    const result = computeExportIncome(rows, FLAT_15P, 3600);
    expect(result[1]._export_income).toBeCloseTo(3 * 0.15, 5); // £0.45, not £0
  });

  it('gives the same total cost for the same energy regardless of bucket width', () => {
    // 3 kWh of export expressed two ways:
    //   (a) three 30-min buckets of 1 kWh each  — 7d range (bucket_secs=1800)
    //   (b) one 1-hour bucket of 3 kWh          — month range (bucket_secs=3600)
    // Both must total the same income. Under the old fixed cap (b) was zeroed.
    const rate = 0.15;
    const narrowBuckets = [
      { t: 0, today_export_kwh: 0 },
      { t: 1_800_000, today_export_kwh: 1 },
      { t: 3_600_000, today_export_kwh: 2 },
      { t: 5_400_000, today_export_kwh: 3 },
    ];
    const wideBuckets = [
      { t: 0, today_export_kwh: 0 },
      { t: 3_600_000, today_export_kwh: 3 },
    ];
    const narrow = computeExportIncome(narrowBuckets, FLAT_15P, 1800)[3]._export_income;
    const wide = computeExportIncome(wideBuckets, FLAT_15P, 3600)[1]._export_income;
    expect(narrow).toBeCloseTo(3 * rate, 5); // £0.45
    expect(wide).toBeCloseTo(3 * rate, 5); // £0.45 — consistent across ranges
  });

  it('still clamps genuine corruption on a wide bucket', () => {
    // 12-hour bucket (6m range): ceiling = 15 kW × 12 h = 180 kWh. A 250 kWh
    // jump is still impossible → clamped. The defense is not disabled, just
    // scaled to the bucket width.
    const rows = [
      { t: 0, today_import_kwh: 5 },
      { t: 43_200_000, today_import_kwh: 255 },
    ];
    const result = computeImportCost(rows, FLAT_15P, 43200);
    expect(result[1]._import_cost).toBeCloseTo(0, 5);
  });

  it('tolerates a data gap via actual elapsed time', () => {
    // 30-min buckets (ceiling 7.5 kWh by bucket width), but the two readings
    // are 6 hours apart (app was offline). 4 kWh over 6 h is ~0.7 kW — well
    // within the elapsed-scaled ceiling (15 kW × 6 h = 90 kWh) → counted.
    const rows = [
      { t: 0, today_import_kwh: 0 },
      { t: 21_600_000, today_import_kwh: 4 },
    ];
    const result = computeImportCost(rows, FLAT_15P, 1800);
    expect(result[1]._import_cost).toBeCloseTo(4 * 0.15, 5); // £0.60
  });
});
