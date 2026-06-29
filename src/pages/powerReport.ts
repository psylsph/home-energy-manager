// ---------------------------------------------------------------------------
// Power-page Consumption Report aggregation
//
// Pure helpers extracted from PowerPage.tsx so the bucket integration can be
// unit-tested without rendering the React tree. The shape returned by
// `calculatePowerReport` is what the Power page's "Consumption Report" button
// shows; PR #166 changed the directional energy + peak fields to integrate
// the server-derived `_charge_power` / `_discharge_power` /
// `_grid_import_power` / `_grid_export_power` magnitudes instead of
// re-splitting the net signed `battery_power` / `grid_power` on the client
// (which would let charge and discharge cancel inside a wide bucket).
// ---------------------------------------------------------------------------

import type { HistoryRange } from '../lib/types';

export interface PowerRow {
  t: number;
  solarPower: number | null;
  batteryPower: number | null;
  /** Grid power, negated relative to the backend so positive = importing. */
  gridPower: number | null;
  homePower: number | null;
  soc: number | null;
  /**
   * Directional magnitudes (W, >= 0), pre-split server-side before bucket
   * aggregation. The Consumption Report integrates / peaks these instead of
   * re-splitting the net battery/grid power, which cancels charge against
   * discharge (import against export) within a wide bucket. Null when a
   * timestamp has no sample.
   */
  chargePower: number | null;
  dischargePower: number | null;
  gridImportPower: number | null;
  gridExportPower: number | null;
}

export interface PowerBucket {
  start: number;
  label: string;
  solarKwh: number;
  homeKwh: number;
  importKwh: number;
  exportKwh: number;
  batteryChargeKwh: number;
  batteryDischargeKwh: number;
  socMin: number | null;
  socMax: number | null;
  socSum: number;
  socCount: number;
}

export interface PowerReportSummary {
  periodLabel: string;
  generatedAt: Date;
  solarKwh: number;
  homeKwh: number;
  importKwh: number;
  exportKwh: number;
  netGridKwh: number;
  batteryChargeKwh: number;
  batteryDischargeKwh: number;
  peakSolarW: number;
  peakHomeW: number;
  peakGridImportW: number;
  peakGridExportW: number;
  peakBatteryChargeW: number;
  peakBatteryDischargeW: number;
  socMin: number | null;
  socMax: number | null;
  socAvg: number | null;
  solarCoveragePct: number | null;
  gridDependencyPct: number | null;
  // Issue #131: cost totals sourced from /api/report (server-side
  // integration against the configured tariff + standing charge). When
  // the fetch hasn't returned yet, these stay at 0 and the report tiles
  // show "—" via the formatGbp fallback in the export rendering.
  importCostGbp: number;
  exportIncomeGbp: number;
  netCostGbp: number;
  standingChargeGbp: number;
  standingChargePPerDay: number;
  daysInRange: number;
}

export interface PowerReport {
  summary: PowerReportSummary;
  buckets: PowerBucket[];
}

/** Median of positive dt across `rows` (already-sorted by `t` not assumed). */
export function medianIntervalMs(rows: PowerRow[]): number | null {
  const intervals = rows
    .slice(1)
    .map((row, i) => row.t - rows[i].t)
    .filter((dt) => dt > 0)
    .sort((a, b) => a - b);
  if (intervals.length === 0) return null;
  return intervals[Math.floor(intervals.length / 2)];
}

/** Clamp to >= 0; treats null/undefined as 0. */
export function positivePart(value: number | null | undefined): number {
  return Math.max(value ?? 0, 0);
}

/**
 * Trapezoidal energy in kWh between two readings `a` and `b`, spanning
 * `hours` of time. `transform` is applied to each reading's value (e.g.
 * [`positivePart`]) before integration so negative readings contribute
 * zero rather than cancelling positive ones across the bucket boundary.
 */
export function integratePair(
  a: number | null,
  b: number | null,
  hours: number,
  transform: (value: number | null | undefined) => number,
): number {
  if (a == null && b == null) return 0;
  if (a == null) return transform(b) * hours / 1000;
  if (b == null) return transform(a) * hours / 1000;
  return ((transform(a) + transform(b)) / 2) * hours / 1000;
}

function bucketGranularity(range: HistoryRange): 'hour' | 'day' | 'month' {
  if (range === '6m' || range === '1y') return 'month';
  if (range === '7d' || range === '30d' || range === 'month') return 'day';
  return 'hour';
}

function bucketStartMs(ts: number, range: HistoryRange): number {
  const d = new Date(ts);
  switch (bucketGranularity(range)) {
    case 'month':
      return new Date(d.getFullYear(), d.getMonth(), 1).getTime();
    case 'day':
      return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
    case 'hour':
      return new Date(d.getFullYear(), d.getMonth(), d.getDate(), d.getHours()).getTime();
  }
}

function bucketLabel(start: number, range: HistoryRange): string {
  const d = new Date(start);
  switch (bucketGranularity(range)) {
    case 'month':
      return d.toLocaleDateString([], { month: 'short', year: 'numeric' });
    case 'day':
      if (range === 'month') return String(d.getDate());
      return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
    case 'hour':
      return d.toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit' });
  }
}

function emptyBucket(start: number, range: HistoryRange): PowerBucket {
  return {
    start,
    label: bucketLabel(start, range),
    solarKwh: 0,
    homeKwh: 0,
    importKwh: 0,
    exportKwh: 0,
    batteryChargeKwh: 0,
    batteryDischargeKwh: 0,
    socMin: null,
    socMax: null,
    socSum: 0,
    socCount: 0,
  };
}

function addSoc(bucket: PowerBucket, soc: number | null) {
  if (soc == null) return;
  bucket.socMin = bucket.socMin == null ? soc : Math.min(bucket.socMin, soc);
  bucket.socMax = bucket.socMax == null ? soc : Math.max(bucket.socMax, soc);
  bucket.socSum += soc;
  bucket.socCount += 1;
}

/** Average SOC over the bucket's readings, or null if none. */
export function bucketSocAvg(bucket: PowerBucket): number | null {
  return bucket.socCount > 0 ? bucket.socSum / bucket.socCount : null;
}

/**
 * Build the Consumption Report. Walks the row pairs in time order,
 * integrates each direction's kWh via the trapezoidal rule on magnitudes,
 * and groups the per-bucket totals by the range's granularity (hour for
 * sub-day ranges, day for week/month, month for 6m/1y).
 *
 * The directional energy + peak figures (`importKwh`, `exportKwh`,
 * `batteryChargeKwh`, `batteryDischargeKwh`, `peakGridImportW`, etc.)
 * integrate the server-split magnitudes, NOT the net signed fields,
 * so charge never cancels discharge within a wide bucket. The net
 * `battery_power` / `grid_power` series on the chart are still signed
 * (one line each, drawn as a connected signed waveform) - only the
 * Consumption Report's directional sums switched (PR #166).
 *
 * `periodLabel` is supplied by the caller (typically
 * `powerWindowLabel(range, offset)` from PowerPage) so this module
 * stays free of UI/i18n dependencies and tests can pass `''`.
 */
export function calculatePowerReport(
  rows: PowerRow[],
  range: HistoryRange,
  domain: [number, number],
  periodLabel: string = '',
): PowerReport {
  const buckets = new Map<number, PowerBucket>();
  const getBucket = (ts: number) => {
    const start = bucketStartMs(ts, range);
    const existing = buckets.get(start);
    if (existing) return existing;
    const created = emptyBucket(start, range);
    buckets.set(start, created);
    return created;
  };

  const sortedRows = rows.filter((row) => row.t >= domain[0] && row.t <= domain[1]).sort((a, b) => a.t - b.t);
  const medianMs = medianIntervalMs(sortedRows);
  const maxGapMs = medianMs == null ? Infinity : medianMs * 3.5;

  let solarKwh = 0;
  let homeKwh = 0;
  let importKwh = 0;
  let exportKwh = 0;
  let batteryChargeKwh = 0;
  let batteryDischargeKwh = 0;

  for (let i = 0; i < sortedRows.length - 1; i++) {
    const a = sortedRows[i];
    const b = sortedRows[i + 1];
    const rawDt = b.t - a.t;
    if (rawDt <= 0 || rawDt > maxGapMs) continue;
    const start = Math.max(a.t, domain[0]);
    const end = Math.min(b.t, domain[1]);
    const hours = (end - start) / 3600000;
    if (hours <= 0) continue;

    const solar = integratePair(a.solarPower, b.solarPower, hours, positivePart);
    const home = integratePair(a.homePower, b.homePower, hours, positivePart);
    // Directional energy integrates the server-split magnitudes (already >= 0),
    // not the net grid/battery power. Integrating the net would let a bucket's
    // import and export (charge and discharge) cancel before integration - the
    // same sign-cancellation the History directional charts had.
    const gridImport = integratePair(a.gridImportPower, b.gridImportPower, hours, positivePart);
    const gridExport = integratePair(a.gridExportPower, b.gridExportPower, hours, positivePart);
    const batteryCharge = integratePair(a.chargePower, b.chargePower, hours, positivePart);
    const batteryDischarge = integratePair(a.dischargePower, b.dischargePower, hours, positivePart);

    solarKwh += solar;
    homeKwh += home;
    importKwh += gridImport;
    exportKwh += gridExport;
    batteryChargeKwh += batteryCharge;
    batteryDischargeKwh += batteryDischarge;

    const midpoint = start + (end - start) / 2;
    const bucket = getBucket(midpoint);
    bucket.solarKwh += solar;
    bucket.homeKwh += home;
    bucket.importKwh += gridImport;
    bucket.exportKwh += gridExport;
    bucket.batteryChargeKwh += batteryCharge;
    bucket.batteryDischargeKwh += batteryDischarge;
  }

  for (const row of sortedRows) {
    addSoc(getBucket(row.t), row.soc);
  }

  const socValues = sortedRows.map((row) => row.soc).filter((soc): soc is number => soc != null);
  const socMin = socValues.length ? Math.min(...socValues) : null;
  const socMax = socValues.length ? Math.max(...socValues) : null;
  const socAvg = socValues.length ? socValues.reduce((acc, soc) => acc + soc, 0) / socValues.length : null;

  const summary: PowerReportSummary = {
    periodLabel,
    generatedAt: new Date(),
    solarKwh,
    homeKwh,
    importKwh,
    exportKwh,
    netGridKwh: importKwh - exportKwh,
    batteryChargeKwh,
    batteryDischargeKwh,
    peakSolarW: Math.max(0, ...sortedRows.map((row) => positivePart(row.solarPower))),
    peakHomeW: Math.max(0, ...sortedRows.map((row) => positivePart(row.homePower))),
    // Peaks come from the directional magnitudes too (no net cancellation).
    // These remain a max of per-bucket AVERAGES, so they understate the true
    // instantaneous peak at coarse buckets - same as peakSolarW / peakHomeW.
    peakGridImportW: Math.max(0, ...sortedRows.map((row) => positivePart(row.gridImportPower))),
    peakGridExportW: Math.max(0, ...sortedRows.map((row) => positivePart(row.gridExportPower))),
    peakBatteryChargeW: Math.max(0, ...sortedRows.map((row) => positivePart(row.chargePower))),
    peakBatteryDischargeW: Math.max(0, ...sortedRows.map((row) => positivePart(row.dischargePower))),
    socMin,
    socMax,
    socAvg,
    solarCoveragePct: homeKwh > 0 ? solarKwh / homeKwh * 100 : null,
    gridDependencyPct: homeKwh > 0 ? importKwh / homeKwh * 100 : null,
    // Issue #131: cost totals filled in by the HTTP layer once /api/report
    // returns. Defaults are 0 so the report tiles can render immediately.
    importCostGbp: 0,
    exportIncomeGbp: 0,
    netCostGbp: 0,
    standingChargeGbp: 0,
    standingChargePPerDay: 0,
    daysInRange: 0,
  };

  return {
    summary,
    buckets: [...buckets.values()].sort((a, b) => a.start - b.start),
  };
}
