/**
 * Pure-data helpers for the History page's Cost tab.
 *
 * Kept separate from HistoryPage.tsx so the cost accumulation logic can be
 * unit-tested without rendering React. The data shape mirrors what
 * `fetchHistory` returns after bucketing: one object per bucket, with raw
 * field values keyed by field name and any missing readings as `null`/
 * `undefined`. See `tests/lib/costChart.test.ts` for the contract.
 */

import type { TariffConfig } from './types';
import { rateForTimestamp } from './tariff';

export type CostRow = Record<string, number>;

export type CostInput = Record<string, number | undefined>;

/**
 * Maximum plausible sustained power (kW) for a residential system.
 *
 * Used to bound per-bucket energy deltas as the last line of defense against
 * corrupted cumulative counters that slip past the backend sanitizer and
 * history-repair SQL. 15 kW matches the backend's own grid-power clamp
 * (`±15 kW`); solar (≤10 kW) and battery (≤10 kW) sit well inside it.
 */
const MAX_PLAUSIBLE_POWER_KW = 15;

/**
 * Per-row energy ceiling (kWh) for a cumulative-counter delta.
 *
 * Scales the max plausible power by the *wider* of the nominal bucket width
 * or the actual elapsed time since the last counted reading. Scaling by
 * bucket width is what keeps Cost totals consistent across ranges (issue
 * #133): a 12-hour bucket legitimately captures far more energy than a
 * 30-minute one, so the spike ceiling must grow with the bucket. Using the
 * actual elapsed time too means a data gap (e.g. the app was offline) does
 * not trip the clamp on the first reading after the gap.
 */
function maxDeltaKwh(bucketSecs: number, elapsedMs: number): number {
  const bucketHours = bucketSecs / 3600;
  const elapsedHours = elapsedMs / 3_600_000;
  return MAX_PLAUSIBLE_POWER_KW * Math.max(bucketHours, elapsedHours);
}

/**
 * Compute the cumulative import cost (£) from `today_import_kwh` deltas
 * and the import tariff config.
 *
 * `bucketSecs` is the backend aggregation bucket size for the current range
 * (see `rangeToBucketSecs`); it sets the per-bucket spike ceiling so wider
 * ranges do not silently discard legitimate energy (issue #133).
 *
 * Adds a `_import_cost` field to each row. Missing data is represented as
 * `NaN` so the chart leaves a gap rather than drawing a misleading zero.
 */
export function computeImportCost(
  rows: CostInput[],
  tariffCfg: TariffConfig,
  bucketSecs: number,
): CostRow[] {
  let prev: number | null = null;
  let prevT: number | null = null;
  let acc = 0;
  return rows.map((row) => {
    const raw = row.today_import_kwh;
    const t = row.t ?? 0;
    // If the field is absent from this row, emit NaN so the chart leaves a
    // gap rather than drawing a misleading zero. The accumulator is preserved
    // so the next real reading resumes from the correct baseline.
    if (raw == null) {
      return { ...row, _import_cost: Number.NaN } as CostRow;
    }
    let delta = 0;
    if (prev != null) {
      if (raw >= prev) {
        delta = raw - prev;
      } else if (prev > 5 && raw < 5) {
        // Midnight rollover: counter reset to near-zero.
        // prev was yesterday's final value (any positive amount),
        // raw is today's running total since midnight.
        // The delta is just the new day's accumulated import.
        delta = raw;
      }
      // else: small data glitch (counter dipped slightly),
      // skip this delta (delta stays 0)

      const elapsedMs = prevT != null ? Math.max(0, t - prevT) : 0;
      if (delta > maxDeltaKwh(bucketSecs, elapsedMs)) {
        // Spike detected: zero the delta AND don't advance prev,
        // so the corrupted cumulative value doesn't permanently
        // inflate the baseline. The next real reading will produce
        // a catch-up delta (re-checked against the ceiling), then prev re-syncs.
        delta = 0;
      } else {
        // Normal delta — advance the baseline.
        prev = raw;
        prevT = t;
      }
    } else {
      prev = raw;
      prevT = t;
    }
    const rate =
      rateForTimestamp(tariffCfg, t) ?? tariffCfg.slots[0]?.rate ?? 0;
    acc += delta * rate;
    return { ...row, _import_cost: acc } as CostRow;
  });
}

/**
 * Compute the cumulative export income (£) from `today_export_kwh` deltas
 * and the export tariff config.
 *
 * `bucketSecs` is the backend aggregation bucket size for the current range
 * (see `rangeToBucketSecs`); it sets the per-bucket spike ceiling so wider
 * ranges do not silently discard legitimate energy (issue #133).
 *
 * Adds a `_export_income` field to each row. Missing data is represented as
 * `NaN` so the chart leaves a gap rather than drawing a misleading zero.
 */
export function computeExportIncome(
  rows: CostInput[],
  tariffCfg: TariffConfig,
  bucketSecs: number,
): CostRow[] {
  let prev: number | null = null;
  let prevT: number | null = null;
  let acc = 0;
  return rows.map((row) => {
    const raw = row.today_export_kwh;
    const t = row.t ?? 0;
    // If the field is absent from this row, emit NaN so the chart leaves a
    // gap rather than drawing a misleading zero.
    if (raw == null) {
      return { ...row, _export_income: Number.NaN } as CostRow;
    }
    let delta = 0;
    if (prev != null) {
      if (raw >= prev) {
        delta = raw - prev;
      } else if (prev > 5 && raw < 5) {
        // Midnight rollover
        delta = raw;
      }

      const elapsedMs = prevT != null ? Math.max(0, t - prevT) : 0;
      if (delta > maxDeltaKwh(bucketSecs, elapsedMs)) {
        delta = 0;
      } else {
        prev = raw;
        prevT = t;
      }
    } else {
      prev = raw;
      prevT = t;
    }
    const rate =
      rateForTimestamp(tariffCfg, t) ?? tariffCfg.slots[0]?.rate ?? 0;
    acc += delta * rate;
    return { ...row, _export_income: acc } as CostRow;
  });
}

/**
 * Compute both import cost and export income in a single pass over the
 * merged data. More efficient than running two separate passes when both
 * series are needed on the same chart.
 *
 * `bucketSecs` is forwarded to both accumulators (see `computeImportCost` /
 * `computeExportIncome`).
 *
 * Adds both `_import_cost` and `_export_income` fields to each row.
 */
export function computeCombinedCost(
  rows: CostInput[],
  importTariffCfg: TariffConfig,
  exportTariffCfg: TariffConfig,
  bucketSecs: number,
): CostRow[] {
  // Run both accumulators independently on the same input, then merge
  // the derived fields into each row. This keeps the per-field logic
  // identical to the single-series versions.
  const importRows = computeImportCost(rows, importTariffCfg, bucketSecs);
  const exportRows = computeExportIncome(rows, exportTariffCfg, bucketSecs);

  return importRows.map((row, i) => {
    const exportRow = exportRows[i];
    return {
      ...row,
      _export_income: exportRow?._export_income ?? Number.NaN,
    } as CostRow;
  });
}
