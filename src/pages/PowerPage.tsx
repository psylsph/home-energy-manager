import { useEffect, useMemo, useRef, useState } from 'react';
import {
  Area,
  CartesianGrid,
  ComposedChart,
  Line,
  ReferenceLine,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';
import { apiGet, fetchHistory, isTauri } from '../lib/api';
import {
  getHistoryChartGridProps,
  HISTORY_RANGE_MS,
  HISTORY_RANGES,
  getHistoryRangeDomain,
  getHistoryXAxisMinTickGap,
  getHistoryXAxisTicks,
  formatHistoryXAxisTick,
  getHistoryPickerMax,
  getHistoryPickerValue,
  historyPickerInputType,
  historyPickerValueToOffset,
  isRollingHistoryRange,
  shouldRefreshHistoryRange,
  shouldTrimHistoryRangeLeadingGap,
  supportsHistoryDate,
  trimDomainStartToFirstDataPoint,
} from '../lib/historyRangeConfig';
import { formatPower } from '../lib/format';
import { getSeriesOpacity } from '../lib/chartSeries';
import { SeriesLegend } from '../components/SeriesLegend';
import type { SeriesLegendItem } from '../components/SeriesLegend';
import type { HistoryRange, TimePoint } from '../lib/types';
import { useInverterStore } from '../store/useInverterStore';
import {
  bucketSocAvg,
  calculatePowerReport,
  type PowerRow,
  type PowerBucket,
  type PowerReport,
  type PowerReportSummary,
} from './powerReport';

type PowerSeriesKey =
  | 'solarPower'
  | 'batteryPower'
  | 'gridPower'
  | 'homePower';

type PowerChartKey = PowerSeriesKey | 'soc';

interface PowerHistoryState {
  range: HistoryRange | null;
  data: Record<string, TimePoint[]>;
  error: string;
}

// The net battery_power / grid_power drive the combined power CHART (one signed
// line each). The directional `_charge_power` / `_discharge_power` /
// `_grid_import_power` / `_grid_export_power` fields are split by sign on the
// server BEFORE bucket aggregation and drive the Consumption Report's
// directional energy + peak figures, so charge/discharge (import/export) no
// longer cancel within a wide bucket.
const HISTORY_FIELDS = [
  'solar_power', 'battery_power', 'grid_power', 'home_power', 'soc',
  '_charge_power', '_discharge_power', '_grid_import_power', '_grid_export_power',
];
const EMPTY_HISTORY_DATA: Record<string, TimePoint[]> = {};

const POWER_SERIES: { key: PowerSeriesKey; label: string; color: string }[] = [
  { key: 'solarPower', label: 'Combined PV', color: '#F59E0B' },
  { key: 'batteryPower', label: 'Battery', color: '#22C55E' },
  { key: 'gridPower', label: 'Grid', color: '#EF4444' },
  { key: 'homePower', label: 'Load / Home', color: '#14B8A6' },
];

const HOME_POWER_SERIES = POWER_SERIES.find((series) => series.key === 'homePower');
const DIRECTIONAL_POWER_SERIES = POWER_SERIES.filter((series) => series.key !== 'homePower');
const SOC_SERIES = { key: 'soc', label: 'Battery SOC', color: '#A78BFA' } as const;
const POWER_CHART_SERIES: SeriesLegendItem<PowerChartKey>[] = [
  ...POWER_SERIES,
  { ...SOC_SERIES, marker: 'line' },
];

const SPIKE_THRESHOLD_W = 4000;

function removePowerSpikes(points: TimePoint[]): TimePoint[] {
  if (points.length < 3) return points;
  return points.map((point, i) => {
    if (i === 0 || i === points.length - 1) return point;
    const prev = points[i - 1];
    const next = points[i + 1];
    const dPrev = Math.abs(point.v - prev.v);
    const dNext = Math.abs(point.v - next.v);
    const dNeighbors = Math.abs(next.v - prev.v);
    if (
      dPrev > SPIKE_THRESHOLD_W
      && dNext > SPIKE_THRESHOLD_W
      && dNeighbors < SPIKE_THRESHOLD_W * 0.5
    ) {
      return { t: point.t, v: (prev.v + next.v) / 2 };
    }
    return point;
  });
}

function pointsByTimestamp(points: TimePoint[] | undefined): Map<number, number> {
  return new Map((points ?? []).map((p) => [p.t, p.v]));
}

function buildPowerRows(data: Record<string, TimePoint[]>): PowerRow[] {
  const solar = pointsByTimestamp(data.solar_power);
  const battery = pointsByTimestamp(data.battery_power);
  const grid = pointsByTimestamp(data.grid_power);
  const home = pointsByTimestamp(data.home_power);
  const soc = pointsByTimestamp(data.soc);
  const charge = pointsByTimestamp(data._charge_power);
  const discharge = pointsByTimestamp(data._discharge_power);
  const gridImport = pointsByTimestamp(data._grid_import_power);
  const gridExport = pointsByTimestamp(data._grid_export_power);
  const timestamps = new Set<number>();

  for (const field of HISTORY_FIELDS) {
    for (const point of data[field] ?? []) {
      timestamps.add(point.t);
    }
  }

  return [...timestamps].sort((a, b) => a - b).map((t) => {
    const solarValue = solar.get(t);
    const batteryValue = battery.get(t);
    const gridValue = grid.get(t);
    const homeValue = home.get(t);
    const socValue = soc.get(t);
    const chargeValue = charge.get(t);
    const dischargeValue = discharge.get(t);
    const gridImportValue = gridImport.get(t);
    const gridExportValue = gridExport.get(t);

    return {
      t,
      solarPower: solarValue == null ? null : Math.max(solarValue, 0),
      batteryPower: batteryValue == null ? null : batteryValue,
      gridPower: gridValue == null ? null : -gridValue,
      homePower: homeValue == null ? null : Math.max(homeValue, 0),
      soc: socValue == null ? null : Math.min(100, Math.max(0, socValue)),
      chargePower: chargeValue == null ? null : Math.max(chargeValue, 0),
      dischargePower: dischargeValue == null ? null : Math.max(dischargeValue, 0),
      gridImportPower: gridImportValue == null ? null : Math.max(gridImportValue, 0),
      gridExportPower: gridExportValue == null ? null : Math.max(gridExportValue, 0),
    };
  });
}

function calculateDomain(rows: PowerRow[]): [number, number] {
  const max = rows.reduce((acc, row) => {
    const rowMax = POWER_SERIES.reduce((seriesAcc, series) => {
      const value = row[series.key];
      return Math.max(seriesAcc, Math.abs(value ?? 0));
    }, 0);
    return Math.max(acc, rowMax);
  }, 0);
  const rounded = Math.max(1000, Math.ceil(max / 1000) * 1000);
  return [-rounded, rounded];
}

function formatAxisWatts(value: number): string {
  const abs = Math.abs(value);
  if (abs >= 1000) return `${value < 0 ? '-' : ''}${Math.round(abs / 100) / 10}k`;
  return `${Math.round(value)}`;
}

function formatAxisPercent(value: number): string {
  return `${Math.round(value)}%`;
}

function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 60000);
    return () => clearInterval(id);
  }, []);
  return now;
}

function PowerStat({ label, value, color, direction, waiting }: {
  label: string;
  value: number;
  color: string;
  direction: string;
  waiting?: boolean;
}) {
  return (
    <div className="bg-bg-elevated rounded-xl px-4 py-3">
      <div className="flex items-center justify-between gap-3">
        <span className="text-text-secondary text-xs font-sans">{label}</span>
        <span className="text-[10px] uppercase tracking-wide font-semibold" style={{ color }}>
          {direction}
        </span>
      </div>
      <div className="mt-2 min-h-7 flex items-end">
        {waiting ? (
          <span className="text-text-secondary text-xs font-sans font-medium">
            Waiting for data
          </span>
        ) : (
          <span className="text-text-primary text-xl font-mono font-bold">
            {formatPower(value)}
          </span>
        )}
      </div>
    </div>
  );
}

function formatKwh(value: number): string {
  return `${value.toFixed(value >= 100 ? 0 : 1)} kWh`;
}

function formatPercentValue(value: number | null): string {
  return value == null ? '—' : `${Math.round(value)}%`;
}

function formatWatts(value: number): string {
  return formatPower(value);
}

/** Format a £ amount to 2 decimal places. Returns "—" if the value is
 * zero and `hideZero` is true (used for the standing-charge tile so it
 * doesn't show "£0.00" when no Standing Charge is configured). */
function formatGbp(value: number, hideZero: boolean = false): string {
  if (hideZero && value === 0) return '—';
  return `£${value.toFixed(2)}`;
}

/** Render the per-day standing-charge subtitle shown on the standing
 * charge card, e.g. " (54.86p/day × 7d)". Returns the empty string when
 * no Standing Charge is configured or the days count is 0 / 1. */
function standingChargeSubtitle(summary: PowerReportSummary): string {
  if (summary.standingChargePPerDay <= 0 || summary.daysInRange <= 0) return '';
  const daysSuffix = summary.daysInRange === 1 ? '1d' : `${summary.daysInRange}d`;
  return ` <span style="font-size:11px;color:#64748b">(${summary.standingChargePPerDay.toFixed(2)}p/day × ${daysSuffix})</span>`;
}

function formatLocalDateTime(ts: number): string {
  return new Date(ts).toLocaleString();
}

function csvCell(value: string | number | null | undefined): string {
  if (value == null) return '';
  const text = String(value);
  return /[",\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

function downloadTextFile(content: string, fileName: string, mimeType: string) {
  const blob = new Blob([content], { type: mimeType });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = fileName;
  a.style.display = 'none';
  document.body.appendChild(a);
  a.click();
  setTimeout(() => {
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  }, 1000);
}

// Label for the selected period, mirroring the History tab's window label so
// the Power and History switchers stay in sync. Offset-aware so older periods
// are described by their date(s) instead of always "Today" / "Now".
function powerWindowLabel(range: HistoryRange, offset: number): string {
  if (range === 'month') {
    const now = new Date();
    let targetMonth = now.getMonth() - offset;
    let targetYear = now.getFullYear();
    while (targetMonth < 0) {
      targetMonth += 12;
      targetYear -= 1;
    }
    const d = new Date(targetYear, targetMonth, 1);
    return d.toLocaleDateString([], { month: 'long', year: 'numeric' });
  }
  if (range === 'today') {
    if (offset === 0) return 'Today';
    const now = new Date();
    const target = new Date(now.getFullYear(), now.getMonth(), now.getDate() - offset);
    return target.toLocaleDateString([], { weekday: 'short', month: 'short', day: 'numeric' });
  }
  if (offset === 0) return 'Now';
  const ms = HISTORY_RANGE_MS[range] ?? HISTORY_RANGE_MS['24h'] ?? 86400000;
  const end = new Date(Date.now() - offset * ms);
  const start = new Date(end.getTime() - ms);
  const fmt = (d: Date) =>
    range === '1h' || range === '6h' || range === '12h' || range === '24h'
      ? d.toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
      : range === '6m' || range === '1y'
        ? d.toLocaleDateString([], { month: 'short', day: 'numeric', year: 'numeric' })
        : d.toLocaleDateString([], { month: 'short', day: 'numeric' });
  return `${fmt(start)} – ${fmt(end)}`;
}

function exportFileSafeLabel(label: string): string {
  return label.replace(/[^\w-]+/g, '_').replace(/^_+|_+$/g, '').toLowerCase();
}

function batteryDirection(value: number | null): string {
  if (value == null || Math.abs(value) < 1) return 'Idle';
  return value > 0 ? 'Discharging' : 'Charging';
}

function gridDirection(value: number | null): string {
  if (value == null || Math.abs(value) < 1) return 'Idle';
  return value > 0 ? 'Importing' : 'Exporting';
}

function exportPowerCSV(report: PowerReport, rows: PowerRow[]) {
  const s = report.summary;
  const sections: string[][] = [];

  sections.push(
    ['Report', 'Power'],
    ['Period', s.periodLabel],
    ['Generated', s.generatedAt.toLocaleString()],
    ['Total Solar Generation kWh', s.solarKwh.toFixed(3)],
    ['Total Home Load kWh', s.homeKwh.toFixed(3)],
    ['Total Grid Import kWh', s.importKwh.toFixed(3)],
    ['Total Grid Export kWh', s.exportKwh.toFixed(3)],
    ['Net Grid kWh', s.netGridKwh.toFixed(3)],
    ['Total Battery Charge kWh', s.batteryChargeKwh.toFixed(3)],
    ['Total Battery Discharge kWh', s.batteryDischargeKwh.toFixed(3)],
    // Issue #131: cost totals matching the selected range. Issue #131:
    // Standing Charge is the per-day amount × number of days the window
    // touches; the kWh component is the rest of import cost.
    ['Total Import Cost GBP', s.importCostGbp.toFixed(2)],
    ['Total Export Income GBP', s.exportIncomeGbp.toFixed(2)],
    ['Total Net Cost GBP', s.netCostGbp.toFixed(2)],
    ['Standing Charge GBP', s.standingChargeGbp.toFixed(2)],
    ['Standing Charge p/day', s.standingChargePPerDay.toFixed(2)],
    ['Days in Range', s.daysInRange.toString()],
    ['Peak Solar W', Math.round(s.peakSolarW).toString()],
    ['Peak Home Load W', Math.round(s.peakHomeW).toString()],
    ['Peak Grid Import W', Math.round(s.peakGridImportW).toString()],
    ['Peak Grid Export W', Math.round(s.peakGridExportW).toString()],
    ['Peak Battery Charge W', Math.round(s.peakBatteryChargeW).toString()],
    ['Peak Battery Discharge W', Math.round(s.peakBatteryDischargeW).toString()],
    ['Minimum SOC %', s.socMin == null ? '' : s.socMin.toFixed(1)],
    ['Maximum SOC %', s.socMax == null ? '' : s.socMax.toFixed(1)],
    ['Average SOC %', s.socAvg == null ? '' : s.socAvg.toFixed(1)],
    ['Solar Coverage %', s.solarCoveragePct == null ? '' : s.solarCoveragePct.toFixed(1)],
    ['Grid Dependency %', s.gridDependencyPct == null ? '' : s.gridDependencyPct.toFixed(1)],
    [],
    ['Bucket Breakdown'],
    ['Bucket', 'Solar kWh', 'Home Load kWh', 'Grid Import kWh', 'Grid Export kWh', 'Battery Charge kWh', 'Battery Discharge kWh', 'Min SOC %', 'Avg SOC %', 'Max SOC %'],
  );

  for (const bucket of report.buckets) {
    sections.push([
      bucket.label,
      bucket.solarKwh.toFixed(3),
      bucket.homeKwh.toFixed(3),
      bucket.importKwh.toFixed(3),
      bucket.exportKwh.toFixed(3),
      bucket.batteryChargeKwh.toFixed(3),
      bucket.batteryDischargeKwh.toFixed(3),
      bucket.socMin == null ? '' : bucket.socMin.toFixed(1),
      bucketSocAvg(bucket) == null ? '' : bucketSocAvg(bucket)!.toFixed(1),
      bucket.socMax == null ? '' : bucket.socMax.toFixed(1),
    ]);
  }

  sections.push(
    [],
    ['Detailed Samples'],
    ['Timestamp ISO', 'Timestamp Local', 'Solar W', 'Battery W', 'Battery Direction', 'Grid W', 'Grid Direction', 'Home Load W', 'SOC %'],
  );

  for (const row of rows) {
    sections.push([
      new Date(row.t).toISOString(),
      formatLocalDateTime(row.t),
      row.solarPower == null ? '' : Math.round(row.solarPower).toString(),
      row.batteryPower == null ? '' : Math.round(row.batteryPower).toString(),
      batteryDirection(row.batteryPower),
      row.gridPower == null ? '' : Math.round(row.gridPower).toString(),
      gridDirection(row.gridPower),
      row.homePower == null ? '' : Math.round(row.homePower).toString(),
      row.soc == null ? '' : row.soc.toFixed(1),
    ]);
  }

  const csv = sections.map((row) => row.map(csvCell).join(',')).join('\n');
  const fileName = `givenergy_power_${exportFileSafeLabel(s.periodLabel)}.csv`;
  downloadTextFile(csv, fileName, 'text/csv;charset=utf-8;');
}

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function renderBarChart(title: string, buckets: PowerBucket[], series: { key: keyof PowerBucket; label: string; color: string }[]): string {
  const width = 920;
  const height = 280;
  const left = 54;
  const right = 18;
  const top = 36;
  const bottom = 54;
  const chartW = width - left - right;
  const chartH = height - top - bottom;
  const maxVal = Math.max(
    0.1,
    ...buckets.flatMap((bucket) => series.map((s) => Number(bucket[s.key]) || 0)),
  );
  const groupW = chartW / Math.max(1, buckets.length);
  const barW = Math.max(2, Math.min(18, groupW / (series.length + 1)));
  const labelEvery = Math.max(1, Math.ceil(buckets.length / 12));

  const bars = buckets.flatMap((bucket, bucketIndex) => series.map((item, seriesIndex) => {
    const value = Number(bucket[item.key]) || 0;
    const barH = value / maxVal * chartH;
    const x = left + bucketIndex * groupW + (groupW - barW * series.length) / 2 + seriesIndex * barW;
    const y = top + chartH - barH;
    return `<rect x="${x.toFixed(1)}" y="${y.toFixed(1)}" width="${barW.toFixed(1)}" height="${barH.toFixed(1)}" rx="2" fill="${item.color}" />`;
  })).join('');

  const labels = buckets.map((bucket, index) => {
    if (index % labelEvery !== 0 && index !== buckets.length - 1) return '';
    const x = left + index * groupW + groupW / 2;
    return `<text x="${x.toFixed(1)}" y="${height - 18}" text-anchor="middle" class="axis-label">${escapeHtml(bucket.label)}</text>`;
  }).join('');

  const grid = [0, 0.25, 0.5, 0.75, 1].map((ratio) => {
    const y = top + chartH - ratio * chartH;
    const val = maxVal * ratio;
    return `<line x1="${left}" x2="${width - right}" y1="${y}" y2="${y}" class="grid" />`
      + `<text x="${left - 8}" y="${y + 4}" text-anchor="end" class="axis-label">${val.toFixed(maxVal >= 10 ? 0 : 1)}</text>`;
  }).join('');

  const legend = series.map((item, index) => {
    const x = left + index * 150;
    return `<g><rect x="${x}" y="14" width="10" height="10" rx="2" fill="${item.color}" />`
      + `<text x="${x + 16}" y="23" class="legend-label">${escapeHtml(item.label)}</text></g>`;
  }).join('');

  return `<section class="chart-card"><h2>${escapeHtml(title)}</h2><svg viewBox="0 0 ${width} ${height}" role="img" aria-label="${escapeHtml(title)}">${legend}${grid}${bars}${labels}</svg></section>`;
}

function renderCombinedPowerChart(rows: PowerRow[]): string {
  const width = 920;
  const height = 300;
  const left = 54;
  const right = 54;
  const top = 40;
  const bottom = 48;
  const chartW = width - left - right;
  const chartH = height - top - bottom;
  const filteredRows = rows.filter((row) => row.solarPower != null || row.batteryPower != null || row.gridPower != null || row.homePower != null || row.soc != null);
  if (filteredRows.length < 2) {
    return '<section class="chart-card"><h2>Combined Power Flow</h2><p class="muted">Not enough data for a combined power chart.</p></section>';
  }
  const minT = filteredRows[0].t;
  const maxT = filteredRows[filteredRows.length - 1].t;
  const maxPower = Math.max(
    1000,
    ...filteredRows.flatMap((row) => [
      Math.abs(row.solarPower ?? 0),
      Math.abs(row.batteryPower ?? 0),
      Math.abs(row.gridPower ?? 0),
      Math.abs(row.homePower ?? 0),
    ]),
  );
  const yMax = Math.ceil(maxPower / 1000) * 1000;
  const xFor = (t: number) => left + ((t - minT) / Math.max(1, maxT - minT)) * chartW;
  const yForPower = (v: number) => top + chartH / 2 - (v / yMax) * (chartH / 2);
  const yForSoc = (v: number) => top + chartH - (v / 100) * chartH;
  const series = [
    { key: 'solarPower' as const, label: 'Solar', color: '#F59E0B', dash: '' },
    { key: 'batteryPower' as const, label: 'Battery', color: '#22C55E', dash: '' },
    { key: 'gridPower' as const, label: 'Grid', color: '#EF4444', dash: '' },
    { key: 'homePower' as const, label: 'Home/load', color: '#14B8A6', dash: '' },
  ];
  const polylines = series.map((item) => {
    const points = filteredRows
      .filter((row) => row[item.key] != null)
      .map((row) => `${xFor(row.t).toFixed(1)},${yForPower(row[item.key] ?? 0).toFixed(1)}`)
      .join(' ');
    return `<polyline points="${points}" fill="none" stroke="${item.color}" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" />`;
  }).join('');
  const socPoints = filteredRows
    .filter((row) => row.soc != null)
    .map((row) => `${xFor(row.t).toFixed(1)},${yForSoc(row.soc ?? 0).toFixed(1)}`)
    .join(' ');
  const socLine = socPoints
    ? `<polyline points="${socPoints}" fill="none" stroke="#A78BFA" stroke-width="2" stroke-dasharray="5 4" stroke-linecap="round" stroke-linejoin="round" />`
    : '';
  const grid = [-1, -0.5, 0, 0.5, 1].map((ratio) => {
    const y = top + chartH / 2 - ratio * chartH / 2;
    const value = yMax * ratio;
    return `<line x1="${left}" x2="${width - right}" y1="${y}" y2="${y}" class="grid" />`
      + `<text x="${left - 8}" y="${y + 4}" text-anchor="end" class="axis-label">${formatAxisWatts(value)}</text>`;
  }).join('');
  const socAxis = [0, 50, 100].map((value) => {
    const y = yForSoc(value);
    return `<text x="${width - right + 8}" y="${y + 4}" class="axis-label">${value}%</text>`;
  }).join('');
  const legendItems = [
    ...series,
    { key: 'soc' as const, label: 'SOC', color: '#A78BFA', dash: '5 4' },
  ].map((item, index) => {
    const x = left + index * 135;
    return `<g><line x1="${x}" x2="${x + 20}" y1="18" y2="18" stroke="${item.color}" stroke-width="3" stroke-dasharray="${item.dash}" />`
      + `<text x="${x + 28}" y="22" class="legend-label">${escapeHtml(item.label)}</text></g>`;
  }).join('');
  const startLabel = new Date(minT).toLocaleDateString();
  const endLabel = new Date(maxT).toLocaleDateString();
  return `<section class="chart-card"><h2>Combined Power Flow</h2><svg viewBox="0 0 ${width} ${height}" role="img" aria-label="Combined Power Flow">${legendItems}${grid}<line x1="${left}" x2="${left}" y1="${top}" y2="${top + chartH}" class="grid" /><line x1="${width - right}" x2="${width - right}" y1="${top}" y2="${top + chartH}" class="grid" />${polylines}${socLine}${socAxis}<text x="${left}" y="${height - 16}" class="axis-label">${escapeHtml(startLabel)}</text><text x="${width - right}" y="${height - 16}" text-anchor="end" class="axis-label">${escapeHtml(endLabel)}</text></svg></section>`;
}

function renderDonut(title: string, items: { label: string; value: number; color: string }[]): string {
  const total = items.reduce((acc, item) => acc + Math.max(item.value, 0), 0);
  let cursor = 0;
  const stops = total > 0 ? items.map((item) => {
    const start = cursor;
    const degrees = Math.max(item.value, 0) / total * 360;
    cursor += degrees;
    return `${item.color} ${start.toFixed(1)}deg ${cursor.toFixed(1)}deg`;
  }).join(', ') : '#30363d 0deg 360deg';
  const legend = items.map((item) => (
    `<div class="donut-legend-row"><span class="swatch" style="background:${item.color}"></span>`
    + `<span>${escapeHtml(item.label)}</span><strong>${formatKwh(item.value)}</strong></div>`
  )).join('');
  return `<section class="donut-card"><h2>${escapeHtml(title)}</h2><div class="donut-wrap"><div class="donut" style="background: conic-gradient(${stops});"><span>${formatKwh(total)}</span></div><div class="donut-legend">${legend}</div></div></section>`;
}

function bucketHighlight(buckets: PowerBucket[], field: keyof PowerBucket, label: string): string {
  const best = buckets.reduce<PowerBucket | null>((current, bucket) => {
    if (current == null) return bucket;
    return Number(bucket[field]) > Number(current[field]) ? bucket : current;
  }, null);
  if (best == null) return '';
  return `<div class="highlight"><span>${escapeHtml(label)}</span><strong>${escapeHtml(best.label)} · ${formatKwh(Number(best[field]) || 0)}</strong></div>`;
}

function socLowHighlight(buckets: PowerBucket[]): string {
  const lows = buckets.filter((bucket) => bucket.socMin != null);
  if (lows.length === 0) return '';
  const low = lows.reduce((current, bucket) => (bucket.socMin! < current.socMin! ? bucket : current));
  return `<div class="highlight"><span>Lowest SOC</span><strong>${escapeHtml(low.label)} · ${low.socMin!.toFixed(0)}%</strong></div>`;
}

function exportPowerPDF(report: PowerReport, rows: PowerRow[]): 'opened' | 'downloaded' {
  const s = report.summary;
  const solarToHomeEstimate = Math.max(0, s.solarKwh - s.exportKwh - s.batteryChargeKwh);
  const batteryToHomeEstimate = Math.min(s.batteryDischargeKwh, Math.max(0, s.homeKwh - s.importKwh - solarToHomeEstimate));
  const reportHtml = `<!doctype html>
<html>
<head>
<meta charset="utf-8" />
<title>Consumption Report - ${escapeHtml(s.periodLabel)}</title>
<style>
  :root { color-scheme: light; font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
  body { margin: 0; background: #f3f4f6; color: #0f172a; }
  .page { max-width: 980px; margin: 0 auto; padding: 28px; }
  header { display: flex; align-items: flex-start; justify-content: space-between; gap: 24px; margin-bottom: 22px; }
  h1 { margin: 0 0 6px; font-size: 30px; letter-spacing: -0.04em; }
  h2 { margin: 0 0 14px; font-size: 17px; }
  .muted { color: #64748b; font-size: 13px; }
  .actions { display: flex; gap: 8px; }
  button { border: 0; background: #0ea5e9; color: white; border-radius: 999px; padding: 9px 14px; font-weight: 700; cursor: pointer; }
  .grid-cards { display: grid; grid-template-columns: repeat(4, 1fr); gap: 12px; margin-bottom: 18px; }
  .card, .chart-card, .donut-card, .table-card, .highlight { background: white; border: 1px solid #e2e8f0; border-radius: 18px; box-shadow: 0 8px 24px rgba(15, 23, 42, 0.06); }
  .card { padding: 15px; }
  .card span { display: block; color: #64748b; font-size: 11px; text-transform: uppercase; letter-spacing: .08em; font-weight: 800; }
  .card strong { display: block; margin-top: 7px; font-size: 22px; letter-spacing: -0.04em; }
  .chart-card, .donut-card, .table-card { padding: 18px; margin-bottom: 16px; page-break-inside: avoid; }
  .charts-2 { display: grid; grid-template-columns: repeat(2, 1fr); gap: 14px; }
  .donut-wrap { display: flex; align-items: center; gap: 18px; }
  .donut { width: 132px; height: 132px; border-radius: 50%; display: grid; place-items: center; position: relative; flex: 0 0 auto; }
  .donut::after { content: ''; position: absolute; inset: 26px; background: white; border-radius: 50%; }
  .donut span { position: relative; z-index: 1; font-size: 13px; font-weight: 800; text-align: center; }
  .donut-legend { flex: 1; display: flex; flex-direction: column; gap: 7px; font-size: 12px; }
  .donut-legend-row { display: grid; grid-template-columns: 12px 1fr auto; align-items: center; gap: 8px; }
  .swatch { width: 10px; height: 10px; border-radius: 3px; }
  .grid { stroke: #e2e8f0; stroke-width: 1; }
  .axis-label { fill: #64748b; font-size: 10px; font-weight: 700; }
  .legend-label { fill: #334155; font-size: 12px; font-weight: 700; }
  .highlights { display: grid; grid-template-columns: repeat(5, 1fr); gap: 10px; margin-bottom: 16px; }
  .highlight { padding: 12px; }
  .highlight span { display:block; color: #64748b; font-size: 11px; font-weight: 800; text-transform: uppercase; }
  .highlight strong { display:block; margin-top: 5px; font-size: 13px; }
  table { width: 100%; border-collapse: collapse; font-size: 11px; }
  th, td { padding: 7px 6px; border-bottom: 1px solid #e2e8f0; text-align: right; }
  th:first-child, td:first-child { text-align: left; }
  th { color: #475569; font-size: 10px; text-transform: uppercase; letter-spacing: .06em; }
  @media print { body { background: white; } .page { max-width: none; padding: 0; } .actions { display: none; } .card, .chart-card, .donut-card, .table-card, .highlight { box-shadow: none; } }
</style>
</head>
<body>
<div class="page">
  <header>
    <div>
      <h1>Consumption Report</h1>
      <div class="muted">Home Energy Manager · ${escapeHtml(s.periodLabel)} · Generated ${escapeHtml(s.generatedAt.toLocaleString())}</div>
      <div class="muted">Energy totals are estimated from the currently displayed Power samples.</div>
    </div>
    <div class="actions"><button onclick="window.print()">Save as PDF</button></div>
  </header>

  <section class="grid-cards">
    <div class="card"><span>Solar generated</span><strong style="color:#d97706">${formatKwh(s.solarKwh)}</strong></div>
    <div class="card"><span>Home consumed</span><strong style="color:#0f766e">${formatKwh(s.homeKwh)}</strong></div>
    <div class="card"><span>Grid import</span><strong style="color:#dc2626">${formatKwh(s.importKwh)}</strong></div>
    <div class="card"><span>Grid export</span><strong style="color:#0284c7">${formatKwh(s.exportKwh)}</strong></div>
    <div class="card"><span>Net grid</span><strong>${formatKwh(s.netGridKwh)}</strong></div>
    <div class="card"><span>Battery charged</span><strong style="color:#7c3aed">${formatKwh(s.batteryChargeKwh)}</strong></div>
    <div class="card"><span>Battery discharged</span><strong style="color:#16a34a">${formatKwh(s.batteryDischargeKwh)}</strong></div>
    <div class="card"><span>SOC range</span><strong>${formatPercentValue(s.socMin)} – ${formatPercentValue(s.socMax)}</strong></div>
    <div class="card"><span>Solar coverage</span><strong>${formatPercentValue(s.solarCoveragePct)}</strong></div>
    <div class="card"><span>Grid dependency</span><strong>${formatPercentValue(s.gridDependencyPct)}</strong></div>
    <div class="card"><span>Peak home load</span><strong>${formatWatts(s.peakHomeW)}</strong></div>
    <div class="card"><span>Peak import/export</span><strong>${formatWatts(s.peakGridImportW)} / ${formatWatts(s.peakGridExportW)}</strong></div>
  </section>

  <!-- Issue #131: cost tiles matching the selected range/offset. Sourced
       from /api/report (server-integrated from the today_*_kwh counters
       and the configured tariff + Standing Charge). The kWh × rate
       component and the standing-charge component are shown separately
       so the user can see where the fixed daily cost comes from. -->
  <section class="grid-cards">
    <div class="card"><span>Import cost</span><strong style="color:#dc2626">${formatGbp(s.importCostGbp)}</strong></div>
    <div class="card"><span>Export income</span><strong style="color:#0284c7">${formatGbp(s.exportIncomeGbp)}</strong></div>
    <div class="card"><span>Net cost</span><strong>${formatGbp(s.netCostGbp)}</strong></div>
    <div class="card"><span>Standing Charge</span><strong>${formatGbp(s.standingChargeGbp)}${standingChargeSubtitle(s)}</strong></div>
  </section>

  ${renderCombinedPowerChart(rows)}
  ${renderBarChart('Solar generation vs home load', report.buckets, [
    { key: 'solarKwh', label: 'Solar', color: '#F59E0B' },
    { key: 'homeKwh', label: 'Home/load', color: '#14B8A6' },
  ])}
  ${renderBarChart('Grid import vs export', report.buckets, [
    { key: 'importKwh', label: 'Import', color: '#EF4444' },
    { key: 'exportKwh', label: 'Export', color: '#38BDF8' },
  ])}
  ${renderBarChart('Battery charge vs discharge', report.buckets, [
    { key: 'batteryChargeKwh', label: 'Charge', color: '#8B5CF6' },
    { key: 'batteryDischargeKwh', label: 'Discharge', color: '#22C55E' },
  ])}

  <section class="charts-2">
    ${renderDonut('Grid balance', [
      { label: 'Import', value: s.importKwh, color: '#EF4444' },
      { label: 'Export', value: s.exportKwh, color: '#38BDF8' },
    ])}
    ${renderDonut('Battery activity', [
      { label: 'Charge', value: s.batteryChargeKwh, color: '#8B5CF6' },
      { label: 'Discharge', value: s.batteryDischargeKwh, color: '#22C55E' },
    ])}
  </section>
  <section class="charts-2">
    ${renderDonut('Estimated solar destination', [
      { label: 'Used locally', value: solarToHomeEstimate, color: '#14B8A6' },
      { label: 'Charged battery', value: s.batteryChargeKwh, color: '#8B5CF6' },
      { label: 'Exported', value: s.exportKwh, color: '#38BDF8' },
    ])}
    ${renderDonut('Estimated home source', [
      { label: 'Grid import', value: s.importKwh, color: '#EF4444' },
      { label: 'Battery discharge', value: batteryToHomeEstimate, color: '#22C55E' },
      { label: 'Direct solar / other', value: Math.max(0, s.homeKwh - s.importKwh - batteryToHomeEstimate), color: '#F59E0B' },
    ])}
  </section>

  <section class="highlights">
    ${bucketHighlight(report.buckets, 'solarKwh', 'Best solar')}
    ${bucketHighlight(report.buckets, 'homeKwh', 'Highest load')}
    ${bucketHighlight(report.buckets, 'importKwh', 'Highest import')}
    ${bucketHighlight(report.buckets, 'exportKwh', 'Highest export')}
    ${socLowHighlight(report.buckets)}
  </section>

  <section class="table-card">
    <h2>Bucket breakdown</h2>
    <table>
      <thead><tr><th>Bucket</th><th>Solar</th><th>Home</th><th>Import</th><th>Export</th><th>Charge</th><th>Discharge</th><th>Avg SOC</th></tr></thead>
      <tbody>
        ${report.buckets.map((bucket) => `<tr><td>${escapeHtml(bucket.label)}</td><td>${bucket.solarKwh.toFixed(2)}</td><td>${bucket.homeKwh.toFixed(2)}</td><td>${bucket.importKwh.toFixed(2)}</td><td>${bucket.exportKwh.toFixed(2)}</td><td>${bucket.batteryChargeKwh.toFixed(2)}</td><td>${bucket.batteryDischargeKwh.toFixed(2)}</td><td>${bucketSocAvg(bucket) == null ? '—' : bucketSocAvg(bucket)!.toFixed(0) + '%'}</td></tr>`).join('')}
      </tbody>
    </table>
  </section>
</div>
<script>setTimeout(() => window.print(), 500);</script>
</body>
</html>`;

  const fileName = `givenergy_consumption_${exportFileSafeLabel(s.periodLabel)}.html`;
  if (isTauri) {
    downloadTextFile(reportHtml, fileName, 'text/html;charset=utf-8;');
    return 'downloaded';
  }

  const win = window.open('', '_blank');
  if (!win) {
    downloadTextFile(reportHtml, fileName, 'text/html;charset=utf-8;');
    return 'downloaded';
  }
  win.document.open();
  win.document.write(reportHtml);
  win.document.close();
  win.focus();
  return 'opened';
}

export default function PowerPage() {
  const snapshot = useInverterStore((state) => state.snapshot);
  const range = useInverterStore((state) => state.chartRange);
  const setRange = useInverterStore((state) => state.setChartRange);
  const gridLineWeight = useInverterStore((state) => state.gridLineWeight);
  const now = useNow();
  const rolling = isRollingHistoryRange(range);
  const [offset, setOffset] = useState(0);
  const lastDateRef = useRef(getHistoryPickerValue(range, offset));
  const refreshKey = shouldRefreshHistoryRange(range, offset) ? now : 0;
  const [history, setHistory] = useState<PowerHistoryState>({
    range: null,
    data: {},
    error: '',
  });
  const [mutedSeries, setMutedSeries] = useState<Partial<Record<PowerChartKey, boolean>>>({});
  const [exportToast, setExportToast] = useState<string | null>(null);
  // Issue #131: cost totals matching the selected range, fetched from
  // /api/report. Defaults to zeroed values while the request is in flight
  // or fails — the report degrades gracefully to kWh-only when the
  // backend hasn't returned cost data yet.
  const [cost, setCost] = useState<{
    importCostGbp: number;
    exportIncomeGbp: number;
    netCostGbp: number;
    standingChargeGbp: number;
    standingChargePPerDay: number;
    daysInRange: number;
  }>({
    importCostGbp: 0,
    exportIncomeGbp: 0,
    netCostGbp: 0,
    standingChargeGbp: 0,
    standingChargePPerDay: 0,
    daysInRange: 0,
  });

  // Re-fetch cost whenever the user changes range / offset. We pass the
  // same query params as the History page's graph fetcher so the totals
  // always match what's on screen.
  useEffect(() => {
    let cancelled = false;
    const params = new URLSearchParams();
    params.set('range', range);
    if (offset) params.set('offset', String(offset));
    if (rolling) params.set('rolling', 'true');
    apiGet<{
      ok: boolean;
      import_cost_gbp: number;
      export_income_gbp: number;
      net_cost_gbp: number;
      standing_charge_gbp: number;
      days_in_range: number;
      standing_charge_p_per_day: number;
    }>(`/api/report?${params.toString()}`)
      .then((res) => {
        if (cancelled || !res.ok) return;
        setCost({
          importCostGbp: res.import_cost_gbp ?? 0,
          exportIncomeGbp: res.export_income_gbp ?? 0,
          netCostGbp: res.net_cost_gbp ?? 0,
          standingChargeGbp: res.standing_charge_gbp ?? 0,
          standingChargePPerDay: res.standing_charge_p_per_day ?? 0,
          daysInRange: res.days_in_range ?? 0,
        });
      })
      .catch(() => {
        // Network failure / no backend — keep the previous cost values
        // rather than zeroing them, so a transient blip doesn't make
        // the report flicker to £0.
      });
    return () => {
      cancelled = true;
    };
  }, [range, offset, rolling, refreshKey]);

  const handleRangeChange = (r: HistoryRange) => {
    setRange(r);
    setOffset(0);
  };

  useEffect(() => {
    let cancelled = false;
    fetchHistory(range, HISTORY_FIELDS, offset, rolling)
      .then((result) => {
        if (cancelled) return;
        const cleaned: Record<string, TimePoint[]> = {};
        for (const [field, points] of Object.entries(result)) {
          cleaned[field] = removePowerSpikes(points);
        }
        setHistory({ range, data: cleaned, error: '' });
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setHistory({
          range,
          data: {},
          error: err instanceof Error ? err.message : 'Failed to load power history',
        });
      });
    return () => {
      cancelled = true;
    };
  }, [range, offset, refreshKey, rolling]);

  const loading = history.range !== range;
  const data = loading ? EMPTY_HISTORY_DATA : history.data;
  const error = loading ? '' : history.error;
  const rows = useMemo(() => buildPowerRows(data), [data]);
  const xDomain = useMemo(() => getHistoryRangeDomain(range, offset, now), [range, offset, now]);
  const displayDomain = useMemo(
    () => shouldTrimHistoryRangeLeadingGap(range) ? trimDomainStartToFirstDataPoint(xDomain, data) : xDomain,
    [data, range, xDomain],
  );
  const yDomain = useMemo(() => calculateDomain(rows), [rows]);
  const report = useMemo(() => {
    const r = calculatePowerReport(
      rows,
      range,
      displayDomain,
      powerWindowLabel(range, offset),
    );
    // Issue #131: overlay the cost totals fetched from /api/report onto
    // the in-memory report summary. The kWh integration runs client-side
    // from the rendered Power samples; the cost integration runs server-side
    // from the cumulative today_*_kwh counters (so it matches the History
    // page's cost graph exactly). Until /api/report returns, the cost
    // fields stay at their default of 0 and the report tiles render as
    // "—" — see the exportPowerPDF fallback for that.
    return {
      ...r,
      summary: {
        ...r.summary,
        importCostGbp: cost.importCostGbp,
        exportIncomeGbp: cost.exportIncomeGbp,
        netCostGbp: cost.netCostGbp,
        standingChargeGbp: cost.standingChargeGbp,
        standingChargePPerDay: cost.standingChargePPerDay,
        daysInRange: cost.daysInRange,
      },
    };
  }, [rows, range, displayDomain, offset, cost]);
  const hasData = rows.length > 0;
  const waitingForLiveData = snapshot == null;
  const toggleSeries = (key: PowerChartKey) => {
    setMutedSeries((current) => ({ ...current, [key]: !current[key] }));
  };

  useEffect(() => {
    if (!exportToast) return;
    const id = setTimeout(() => setExportToast(null), 3000);
    return () => clearTimeout(id);
  }, [exportToast]);

  const currentSolar = Math.max(snapshot?.solar_power ?? 0, 0);
  const currentBattery = snapshot?.battery_power ?? 0;
  const currentGrid = snapshot?.grid_power ?? 0;
  const currentHome = Math.max(snapshot?.home_power ?? 0, 0);
  const batteryDirection = currentBattery > 0 ? 'Discharging' : currentBattery < 0 ? 'Charging' : 'Idle';
  const batteryColor = currentBattery > 0 ? '#22C55E' : currentBattery < 0 ? '#6366F1' : '#8B949E';
  const gridDirection = currentGrid < 0 ? 'Importing' : currentGrid > 0 ? 'Exporting' : 'Idle';
  const gridColor = currentGrid < 0 ? '#EF4444' : currentGrid > 0 ? '#38BDF8' : '#8B949E';

  return (
    <div className="flex flex-col gap-4 max-w-5xl mx-auto">
      <div className="flex items-center justify-between gap-3">
        <div>
          <h1 className="text-text-primary text-lg font-semibold font-sans">Power</h1>
          <p className="text-text-secondary text-xs font-sans">
            Live and historical power direction
          </p>
        </div>
        <div className="text-text-secondary text-xs font-sans text-right">
          {snapshot ? new Date(snapshot.timestamp * 1000).toLocaleTimeString() : 'Waiting for data'}
        </div>
      </div>

      <div className="grid grid-cols-2 lg:grid-cols-4 gap-3">
        <PowerStat
          label="Combined PV"
          value={currentSolar}
          color="#F59E0B"
          direction="Generation"
          waiting={waitingForLiveData}
        />
        <PowerStat
          label="Battery"
          value={Math.abs(currentBattery)}
          color={batteryColor}
          direction={batteryDirection}
          waiting={waitingForLiveData}
        />
        <PowerStat
          label="Grid"
          value={Math.abs(currentGrid)}
          color={gridColor}
          direction={gridDirection}
          waiting={waitingForLiveData}
        />
        <PowerStat
          label="Load / Home"
          value={currentHome}
          color="#14B8A6"
          direction="Load"
          waiting={waitingForLiveData}
        />
      </div>

      <div className="flex flex-col gap-2">
        <div className="bg-bg-surface rounded-xl p-2 min-w-0">
          <select
            value={range}
            onChange={(e) => handleRangeChange(e.target.value as HistoryRange)}
            className="sm:hidden w-full bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-sans font-medium outline-none border border-white/10"
            aria-label="Select time range"
          >
            {HISTORY_RANGES.map((r) => (
              <option key={r.key} value={r.key}>{r.label}</option>
            ))}
          </select>
          <div className="hidden sm:flex items-center gap-2 overflow-x-auto">
            {HISTORY_RANGES.map((r) => (
              <button
                key={r.key}
                type="button"
                aria-pressed={range === r.key}
                onClick={() => handleRangeChange(r.key)}
                className={`shrink-0 px-3 py-1.5 rounded-lg text-xs font-sans font-medium transition-colors ${
                  range === r.key
                    ? 'bg-flow-active text-bg-base'
                    : 'bg-bg-elevated text-text-secondary hover:text-text-primary'
                }`}
              >
                {r.label}
              </button>
            ))}
          </div>
        </div>
        <div className="flex items-center justify-center gap-1 bg-bg-surface rounded-xl p-1.5">
          <button
            onClick={() => setOffset((o) => o + 1)}
            className="shrink-0 text-text-secondary hover:text-text-primary text-sm font-sans px-2.5 py-1.5 rounded-lg hover:bg-bg-elevated transition-colors"
          >
            ◀ Older
          </button>
          {supportsHistoryDate(range) ? (
            <input
              type={historyPickerInputType(range)}
              value={getHistoryPickerValue(range, offset)}
              max={getHistoryPickerMax(range)}
              onChange={(e) => {
                const newVal = e.target.value;
                const oldVal = lastDateRef.current;
                lastDateRef.current = newVal;
                setOffset(historyPickerValueToOffset(range, newVal));
                // Only blur on an actual day change — not when the user is
                // browsing months/years in the native picker (which can fire
                // onChange on some platforms). Month-type inputs have no day
                // component so any change is a deliberate selection.
                const isDate = newVal.split('-').length === 3;
                const dayChanged = !isDate || newVal.split('-')[2] !== oldVal.split('-')[2];
                if (dayChanged) {
                  e.target.blur();
                }
              }}
              aria-label="Select period date"
              className="bg-transparent text-text-primary text-sm font-sans text-center px-1 py-0.5 rounded-md outline-none cursor-pointer hover:bg-bg-elevated transition-colors"
            />
          ) : (
            <span className="text-text-secondary text-sm font-sans text-center truncate px-1">
              {powerWindowLabel(range, offset)}
            </span>
          )}
          <button
            onClick={() => setOffset((o) => Math.max(0, o - 1))}
            disabled={offset === 0}
            className="shrink-0 text-text-secondary hover:text-text-primary text-sm font-sans px-2.5 py-1.5 rounded-lg hover:bg-bg-elevated transition-colors disabled:opacity-30"
          >
            Newer ▶
          </button>
        </div>
        <div className="flex items-center justify-end gap-2 bg-bg-surface rounded-xl p-2">
          <button
            type="button"
            onClick={() => {
              exportPowerCSV(
                calculatePowerReport(rows, range, displayDomain, powerWindowLabel(range, offset)),
                rows,
              );
              setExportToast('CSV downloaded to your Downloads folder — ' + report.summary.periodLabel);
            }}
            disabled={loading || !hasData || Boolean(error)}
            className="shrink-0 text-text-secondary hover:text-text-primary text-xs font-sans px-3 py-1.5 rounded-lg bg-bg-elevated hover:bg-bg-elevated/80 transition-colors disabled:opacity-30 disabled:cursor-not-allowed"
          >
            CSV
          </button>
          <button
            type="button"
            onClick={() => {
              const result = exportPowerPDF(
                calculatePowerReport(rows, range, displayDomain, powerWindowLabel(range, offset)),
                rows,
              );
              setExportToast(
                result === 'downloaded'
                  ? 'Consumption report downloaded to your Downloads folder — ' + report.summary.periodLabel
                  : 'Consumption report opened in a new window — ' + report.summary.periodLabel,
              );
            }}
            disabled={loading || !hasData || Boolean(error)}
            className="shrink-0 text-text-secondary hover:text-text-primary text-xs font-sans px-3 py-1.5 rounded-lg bg-bg-elevated hover:bg-bg-elevated/80 transition-colors disabled:opacity-30 disabled:cursor-not-allowed"
          >
            Consumption Report
          </button>
        </div>
      </div>

      {exportToast && (
        <div className="fixed bottom-20 left-1/2 -translate-x-1/2 z-50 bg-bg-surface border border-battery/30 rounded-xl px-4 py-2.5 shadow-lg text-sm text-text-primary font-sans flex items-center gap-2 animate-in fade-in slide-in-from-bottom-2 duration-200">
          <svg className="w-4 h-4 text-battery shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
            <path strokeLinecap="round" strokeLinejoin="round" d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z" />
          </svg>
          {exportToast}
        </div>
      )}

      <div className="bg-bg-elevated rounded-xl p-4">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between mb-3">
          <h2 className="text-text-primary text-sm font-sans font-bold">Power Flow</h2>
          <SeriesLegend items={POWER_CHART_SERIES} muted={mutedSeries} onToggle={toggleSeries} />
        </div>

        {loading ? (
          <div className="flex flex-col items-center justify-center h-[320px] gap-4">
            <div className="w-8 h-8 border-4 border-flow-active border-t-transparent rounded-full animate-spin" />
            <p className="text-text-secondary text-sm font-sans">Loading power history…</p>
          </div>
        ) : error ? (
          <div className="flex items-center justify-center h-[320px] text-red-400 text-sm font-sans">
            {error}
          </div>
        ) : !hasData ? (
          <div className="flex flex-col items-center justify-center h-[320px] gap-2">
            <p className="text-text-secondary text-sm font-sans">No power history for this range</p>
            <p className="text-text-secondary/50 text-xs font-sans">
              History is recorded while the app is running and connected
            </p>
          </div>
        ) : (
          <ResponsiveContainer width="100%" height={320}>
            <ComposedChart data={rows} margin={{ top: 10, right: 4, left: -12, bottom: 0 }}>
              <defs>
                {DIRECTIONAL_POWER_SERIES.map((series) => (
                  <linearGradient
                    key={series.key}
                    id={`power-grad-${series.key}`}
                    x1="0"
                    y1="0"
                    x2="0"
                    y2="1"
                  >
                    <stop offset="5%" stopColor={series.color} stopOpacity={0.28} />
                    <stop offset="95%" stopColor={series.color} stopOpacity={0} />
                  </linearGradient>
                ))}
              </defs>
              <CartesianGrid {...getHistoryChartGridProps(gridLineWeight)} />
              <ReferenceLine
                yAxisId="power"
                y={0}
                stroke="rgba(255,255,255,0.28)"
                strokeWidth={1.5}
              />
              <XAxis
                dataKey="t"
                type="number"
                domain={displayDomain}
                ticks={getHistoryXAxisTicks(range, displayDomain)}
                tickFormatter={(v: number) => formatHistoryXAxisTick(v, range)}
                stroke="#8B949E"
                tick={{ fontSize: 11, style: { fontWeight: 700 } }}
                tickLine={false}
                axisLine={false}
                minTickGap={getHistoryXAxisMinTickGap(range)}
              />
              <YAxis
                yAxisId="power"
                stroke="#8B949E"
                tick={{ fontSize: 11, style: { fontWeight: 700 } }}
                tickLine={false}
                axisLine={false}
                domain={yDomain}
                tickFormatter={(v: number) => formatAxisWatts(v)}
              />
              <YAxis
                yAxisId="soc"
                orientation="right"
                width={36}
                stroke={SOC_SERIES.color}
                tick={{ fontSize: 11, style: { fontWeight: 700 }, fill: SOC_SERIES.color }}
                tickLine={false}
                axisLine={false}
                domain={[0, 100]}
                tickFormatter={(v: number) => formatAxisPercent(v)}
              />
              <Tooltip
                contentStyle={{
                  backgroundColor: '#21262D',
                  border: '1px solid rgba(255,255,255,0.1)',
                  borderRadius: '8px',
                  fontSize: '12px',
                  color: '#F0F6FC',
                }}
                labelFormatter={(v) => {
                  const n = typeof v === 'number' ? v : Number(v);
                  return new Date(n).toLocaleString();
                }}
                formatter={(value, name) => {
                  const n = typeof value === 'number' ? value : Number(value);
                  const key = String(name) as PowerChartKey;
                  if (key === SOC_SERIES.key) {
                    return [formatAxisPercent(n), SOC_SERIES.label];
                  }
                  const label = POWER_SERIES.find((series) => series.key === key)?.label ?? name;
                  const batteryDirection = n < 0 ? 'Charge' : n > 0 ? 'Discharge' : '';
                  const gridDirection = n < 0 ? 'Export' : n > 0 ? 'Import' : '';
                  const displayLabel = key === 'batteryPower' && batteryDirection
                    ? 'Battery ' + batteryDirection
                    : key === 'gridPower' && gridDirection
                      ? 'Grid ' + gridDirection
                      : label;
                  return [formatPower(Math.abs(n)), displayLabel];
                }}
              />
              {DIRECTIONAL_POWER_SERIES.map((series) => (
                <Area
                  key={series.key}
                  yAxisId="power"
                  type="monotone"
                  dataKey={series.key}
                  stroke={series.color}
                  fill={`url(#power-grad-${series.key})`}
                  opacity={getSeriesOpacity(mutedSeries[series.key] ?? false)}
                  strokeWidth={2}
                  dot={false}
                  isAnimationActive={false}
                  connectNulls
                />
              ))}
              {HOME_POWER_SERIES && (
                <Line
                  yAxisId="power"
                  type="monotone"
                  dataKey={HOME_POWER_SERIES.key}
                  stroke={HOME_POWER_SERIES.color}
                  opacity={getSeriesOpacity(mutedSeries[HOME_POWER_SERIES.key] ?? false)}
                  strokeWidth={3}
                  dot={false}
                  activeDot={mutedSeries[HOME_POWER_SERIES.key] ? false : { r: 4 }}
                  isAnimationActive={false}
                  connectNulls
                />
              )}
              <Line
                yAxisId="soc"
                type="monotone"
                dataKey={SOC_SERIES.key}
                stroke={SOC_SERIES.color}
                opacity={getSeriesOpacity(mutedSeries[SOC_SERIES.key] ?? false)}
                strokeWidth={2}
                strokeDasharray="5 4"
                dot={false}
                activeDot={mutedSeries[SOC_SERIES.key] ? false : { r: 4 }}
                isAnimationActive={false}
                connectNulls
              />
            </ComposedChart>
          </ResponsiveContainer>
        )}
      </div>
    </div>
  );
}
