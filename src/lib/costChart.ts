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
 * Maximum per-bucket delta (kWh) for a cumulative daily counter.
 *
 * 2 kWh per bucket is generous: 10 kW sustained for 12 min. Even for
 * 1-minute buckets this is generous. This is the last line of defense
 * against corrupted counter values that slip through the backend sanitizer.
 */
const MAX_DELTA_KWH = 2;

/**
 * Compute the cumulative import cost (£) from `today_import_kwh` deltas
 * and the import tariff config.
 *
 * Adds a `_import_cost` field to each row. Missing data is represented as
 * `NaN` so the chart leaves a gap rather than drawing a misleading zero.
 */
export function computeImportCost(
  rows: CostInput[],
  tariffCfg: TariffConfig,
): CostRow[] {
  let prev: number | null = null;
  let acc = 0;
  return rows.map((row) => {
    const raw = row.today_import_kwh;
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

      // Clamp delta to physically plausible maximum.
      if (delta > MAX_DELTA_KWH) {
        // Spike detected: zero the delta AND don't update prev,
        // so the corrupted cumulative value doesn't permanently
        // inflate the baseline. The next real reading will produce
        // a catch-up delta (capped at MAX_DELTA_KWH), then prev re-syncs.
        delta = 0;
      } else {
        // Normal delta — advance the baseline.
        prev = raw;
      }
    } else {
      prev = raw;
    }
    const rate =
      rateForTimestamp(tariffCfg, row.t ?? 0) ?? tariffCfg.slots[0]?.rate ?? 0;
    acc += delta * rate;
    return { ...row, _import_cost: acc } as CostRow;
  });
}

/**
 * Compute the cumulative export income (£) from `today_export_kwh` deltas
 * and the export tariff config.
 *
 * Adds a `_export_income` field to each row. Missing data is represented as
 * `NaN` so the chart leaves a gap rather than drawing a misleading zero.
 */
export function computeExportIncome(
  rows: CostInput[],
  tariffCfg: TariffConfig,
): CostRow[] {
  let prev: number | null = null;
  let acc = 0;
  return rows.map((row) => {
    const raw = row.today_export_kwh;
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
      // Clamp delta to physically plausible maximum.
      if (delta > MAX_DELTA_KWH) {
        delta = 0;
      } else {
        prev = raw;
      }
    } else {
      prev = raw;
    }
    const rate =
      rateForTimestamp(tariffCfg, row.t ?? 0) ?? tariffCfg.slots[0]?.rate ?? 0;
    acc += delta * rate;
    return { ...row, _export_income: acc } as CostRow;
  });
}

/**
 * Compute both import cost and export income in a single pass over the
 * merged data. More efficient than running two separate passes when both
 * series are needed on the same chart.
 *
 * Adds both `_import_cost` and `_export_income` fields to each row.
 */
export function computeCombinedCost(
  rows: CostInput[],
  importTariffCfg: TariffConfig,
  exportTariffCfg: TariffConfig,
): CostRow[] {
  // Run both accumulators independently on the same input, then merge
  // the derived fields into each row. This keeps the per-field logic
  // identical to the single-series versions.
  const importRows = computeImportCost(rows, importTariffCfg);
  const exportRows = computeExportIncome(rows, exportTariffCfg);

  return importRows.map((row, i) => {
    const exportRow = exportRows[i];
    return {
      ...row,
      _export_income: exportRow?._export_income ?? Number.NaN,
    } as CostRow;
  });
}
