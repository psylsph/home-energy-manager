import { useRef, useState, useEffect } from 'react';
import {
  AreaChart,
  Area,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
} from 'recharts';
import { apiGet, fetchHistory, isTauri } from '../lib/api';
import {
  getHistoryChartGridProps,
  HISTORY_RANGES,
  HISTORY_RANGE_MS,
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
  supportsHistoryDate,
} from '../lib/historyRangeConfig';
import { getSeriesOpacity, removeSpikes } from '../lib/chartSeries';
import { SeriesLegend } from '../components/SeriesLegend';
import { useInverterStore } from '../store/useInverterStore';
import type { SeriesLegendItem } from '../components/SeriesLegend';
import type { HistoryRange, PollSettings } from '../lib/types';
import { computeTempDifferential, computeBatteryExternalDifferential } from '../lib/temperatureChart';
import { computeTightDomain } from '../lib/chartDomain';
import { openExternal } from '../lib/openExternal';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface TimePoint {
  t: number;
  v: number;
}

type MetricTab = 'battery' | 'solar' | 'grid' | 'home' | 'cost' | 'temperature';

interface ChartDef {
  key: string;
  title: string;
  fields: {
    field: string;
    color: string;
    label?: string;
    /**
     * Optional dashed stroke (Recharts `strokeDasharray`, e.g. "4 3").
     * Undefined = solid line, matching every existing series. Used to set
     * the Standing Charge line apart from the solid cost lines it sits
     * alongside on the Cost tab.
     */
    strokeDasharray?: string;
  }[];
  unit: string;
  description?: string;
  yDomain?: [number, number];
  /**
   * When set, the y-axis snaps to the recorded data range ± this padding
   * instead of Recharts' 0-based auto-scale, so narrow-band series like
   * Grid Voltage stay readable (issue #152). Ignored when the range has
   * no finite data points (falls back to auto-scaling).
   */
  tightDomainPadding?: number;
  /**
   * Optional client-side derivation (e.g. the temperature differentials).
   * Cost/income are NOT derived here - the server computes them at native
   * reading resolution (see `_import_cost` / `_export_income`).
   */
  preprocess?: (merged: Record<string, number>[]) => Record<string, number>[];
  /** Raw field names needed by `preprocess` that aren't in `fields`. */
  requires?: string[];
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const TABS: { key: MetricTab; label: string }[] = [
  { key: 'battery', label: 'Battery' },
  { key: 'solar', label: 'Solar' },
  { key: 'grid', label: 'Grid' },
  { key: 'home', label: 'Home' },
  { key: 'temperature', label: 'Temperature' },
  { key: 'cost', label: 'Cost' },
];

function getCharts(tab: MetricTab, hasStandingCharge: boolean): ChartDef[] {
  switch (tab) {
    case 'battery':
      return [
        {
          key: 'soc',
          title: 'SOC %',
          unit: '%',
          yDomain: [0, 100],
          fields: [{ field: 'soc', color: '#6366F1' }],
        },
        {
          key: 'battery-power',
          title: 'Charge / Discharge Power',
          unit: 'W',
          // Server-derived directional fields: each direction's magnitude is
          // averaged independently per bucket, so charge and discharge don't
          // cancel inside a wide (12h/24h) bucket. Splitting a single signed
          // `battery_power` by sign on the client AFTER the server averaged it
          // collapsed both series toward 0 at coarse zoom.
          fields: [
            { field: '_charge_power', color: '#22C55E', label: 'Charge' },
            { field: '_discharge_power', color: '#EF4444', label: 'Discharge' },
          ],
        },
        {
          key: 'battery-energy',
          title: 'Energy (kWh)',
          unit: 'kWh',
          fields: [
            { field: 'today_charge_kwh', color: '#22C55E', label: 'Charge' },
            { field: 'today_discharge_kwh', color: '#EF4444', label: 'Discharge' },
          ],
        },
      ];
    case 'solar':
      return [
        {
          key: 'pv-power',
          title: 'PV Power (W)',
          unit: 'W',
          fields: [
            { field: 'pv1_power', color: '#F59E0B', label: 'PV1' },
            { field: 'pv2_power', color: '#3B82F6', label: 'PV2' },
          ],
        },
        {
          key: 'pv-energy',
          title: 'PV Energy Today (kWh)',
          unit: 'kWh',
          fields: [
            { field: 'today_pv1_kwh', color: '#F59E0B', label: 'PV1' },
            { field: 'today_pv2_kwh', color: '#3B82F6', label: 'PV2' },
            { field: 'today_solar_kwh', color: '#22C55E', label: 'Total' },
          ],
        },
        // PV output as % of rated peak (issue #110). Only returns data when
        // the user has entered a rated kWp in Settings; pv1_pct / pv2_pct are
        // null otherwise.
        {
          key: 'pv-pct',
          title: 'PV % of Rated (kWp)',
          unit: '%',
          fields: [
            { field: 'pv1_pct', color: '#F59E0B', label: 'PV1' },
            { field: 'pv2_pct', color: '#3B82F6', label: 'PV2' },
          ],
        },
      ];
    case 'grid':
      return [
        {
          key: 'grid-power',
          title: 'Grid Power (W)',
          unit: 'W',
          // Server-derived directional fields (see Charge/Discharge above):
          // import and export are each averaged independently per bucket so a
          // day that both imports and exports no longer collapses to its net.
          fields: [
            { field: '_grid_export_power', color: '#22C55E', label: 'Export' },
            { field: '_grid_import_power', color: '#EF4444', label: 'Import' },
          ],
        },
        {
          key: 'grid-energy',
          title: 'Grid Energy Today (kWh)',
          unit: 'kWh',
          // Use the inverter's own daily counters rather than integrating the
          // averaged power columns. Daily counters track the same registers as
          // the GivEnergy portal and are not subject to 5-minute-bucket
          // rounding, so this chart gives a cumulative total that matches the
          // portal export figures (issue #199 follow-up).
          fields: [
            { field: 'today_export_kwh', color: '#22C55E', label: 'Export' },
            { field: 'today_import_kwh', color: '#EF4444', label: 'Import' },
          ],
        },
        {
          key: 'grid-voltage',
          title: 'Grid Voltage (V)',
          unit: 'V',
          tightDomainPadding: 10,
          fields: [{ field: 'grid_voltage', color: '#3B82F6' }],
        },
      ];
    case 'home':
      return [
        {
          key: 'home-power',
          title: 'Load Power (W)',
          unit: 'W',
          fields: [{ field: 'home_power', color: '#14B8A6' }],
        },
        {
          key: 'home-energy',
          title: 'Load Energy Today (kWh)',
          unit: 'kWh',
          fields: [{ field: 'home_energy_today_kwh', color: '#14B8A6' }],
        },
      ];
    case 'temperature':
      return [
        {
          key: 'all-temps',
          title: 'Temperatures',
          unit: '°C',
          fields: [
            { field: 'battery_temperature', color: '#22C55E', label: 'Battery' },
            { field: 'inverter_temperature', color: '#F59E0B', label: 'Inverter' },
            { field: 'external_temperature', color: '#38BDF8', label: 'Outdoor' },
          ],
        },
        {
          key: 'temp-differential',
          title: 'Battery − Inverter (°C)',
          unit: '°C',
          fields: [{ field: '_temp_diff', color: '#A78BFA' }],
          requires: ['battery_temperature', 'inverter_temperature'],
          preprocess: (merged) => computeTempDifferential(merged),
        },
        {
          key: 'batt-ext-differential',
          title: 'Battery − Outdoor (Δ°C)',
          unit: '°C',
          description: 'Battery temperature minus Open-Meteo outdoor temperature. 0°C means they were equal, not that either temperature was zero.',
          fields: [{ field: '_batt_ext_diff', color: '#F472B6' }],
          requires: ['battery_temperature', 'external_temperature'],
          preprocess: (merged) => computeBatteryExternalDifferential(merged),
        },
      ];
    case 'cost': {
      // `_import_cost` / `_export_income` are server-derived cumulative
      // series: the backend integrates the today_*_kwh counters against the
      // configured tariff at native reading resolution, so the totals are
      // correct for time-of-use rates and consistent across every range's
      // bucket width. The frontend just plots them.
      const fields: ChartDef['fields'] = [
        { field: '_import_cost', color: '#EF4444', label: 'Import Cost' },
      ];
      // When a Standing Charge is configured, break the import cost into its
      // per-kWh energy component and the fixed daily standing charge, so the
      // user can see the difference (the gap between Import Cost and Energy
      // Cost is the standing charge). With no standing charge there's nothing
      // to differentiate: Energy Cost would just duplicate Import Cost and
      // Standing Charge would sit flat at £0, so we leave the chart as-is.
      if (hasStandingCharge) {
        fields.push(
          { field: '_import_energy_cost', color: '#F59E0B', label: 'Energy Cost' },
          {
            field: '_import_standing_charge',
            color: '#A78BFA',
            label: 'Standing Charge',
            strokeDasharray: '4 3',
          },
        );
      }
      fields.push({ field: '_export_income', color: '#22C55E', label: 'Export Income' });
      return [
        {
          key: 'cost-combined',
          title: 'Import Cost & Export Income',
          unit: '£',
          fields,
        },
      ];
    }
  }
}

// Issue #199: Combined export CSV — concatenate every tab's chart list so
// a single CSV carries every series visible on the page (Battery, Solar,
// Grid, Home, Temperature, Cost). Field collisions are deduplicated by the
// existing Set logic in exportCSV.
function getAllCharts(hasStandingCharge: boolean): ChartDef[] {
  return TABS.flatMap((t) => getCharts(t.key, hasStandingCharge));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatWindowLabel(range: HistoryRange, offset: number): string {
  if (range === 'month') {
    const now = new Date();
    const year = now.getFullYear();
    const month = now.getMonth();
    let targetMonth = month - offset;
    let targetYear = year;
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
  const fmt = (d: Date) => {
    if (range === '1h' || range === '6h' || range === '12h' || range === '24h') {
      return d.toLocaleString([], {
        month: 'short',
        day: 'numeric',
        hour: '2-digit',
        minute: '2-digit',
      });
    }
    if (range === '6m' || range === '1y') {
      return d.toLocaleDateString([], { month: 'short', day: 'numeric', year: 'numeric' });
    }
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
  };
  return `${fmt(start)} — ${fmt(end)}`;
}

// ---------------------------------------------------------------------------
// Single chart component
// ---------------------------------------------------------------------------

import type { GridLineWeight } from '../lib/historyRangeConfig';

function ChartCard({ chart, data, range, domain, ticks, gridLineWeight }: {
  chart: ChartDef;
  data: Record<string, TimePoint[]>;
  range: HistoryRange;
  domain: [number, number];
  ticks?: number[];
  gridLineWeight: GridLineWeight;
}) {
  const [mutedSeries, setMutedSeries] = useState<Partial<Record<string, boolean>>>({});
  const allFields = [...chart.fields.map((f) => f.field), ...(chart.requires ?? [])];
  const uniqueFields = [...new Set(allFields)];

  const rawPoints: Record<string, TimePoint[]> = {};
  for (const f of uniqueFields) {
    rawPoints[f] = data[f] ?? [];
  }

  const timestamps = new Set<number>();
  for (const pts of Object.values(rawPoints)) {
    for (const p of pts) timestamps.add(p.t);
  }
  const sortedTs = [...timestamps].sort((a, b) => a - b);

  let merged = sortedTs.map((t) => {
    const row: Record<string, number> = { t };
    for (const f of uniqueFields) {
      const pts = rawPoints[f];
      const pt = pts.find((p) => p.t === t);
      if (pt) row[f] = pt.v;
    }
    return row;
  });

  if (chart.preprocess) {
    merged = chart.preprocess(merged);
  }

  const seriesNames = chart.fields.map((f, i) => {
    const suffix = chart.fields.filter((ff, j) => j < i && ff.field === f.field).length;
    return `${f.field}${suffix > 0 ? `_${suffix}` : ''}`;
  });
  const legendItems: SeriesLegendItem[] = chart.fields.map((f, i) => ({
    key: seriesNames[i],
    label: f.label ?? f.field,
    color: f.color,
  }));
  const toggleSeries = (key: string) => {
    setMutedSeries((current) => ({ ...current, [key]: !current[key] }));
  };

  const seriesData = merged.map((row) => {
    const out: Record<string, number | null> = { t: row.t };
    chart.fields.forEach((f, i) => {
      const name = seriesNames[i];
      out[name] = row[f.field] ?? null;
    });
    return out;
  });

  // Charts use their declared yDomain (e.g. SOC fixed at 0-100) or Recharts
  // auto-scaling otherwise. Charts that declare `tightDomainPadding` instead
  // snap the y-axis to the recorded range ± padding (e.g. Grid Voltage ±10 V
  // so narrow fluctuations stay readable, issue #152). The shared Y-axis
  // lock feature is scoped to the Solar power chart on the dashboard only
  // (see SolarPowerChart.tsx).
  const yDomain: [number, number] | undefined =
    chart.tightDomainPadding !== undefined
      ? computeTightDomain(
          seriesData.flatMap((row) =>
            seriesNames.map((name) => row[name] as number | null | undefined),
          ),
          chart.tightDomainPadding,
        )
      : chart.yDomain;

  // Currency tick labels (`£12.25`, and up to `£xxx.xx` on the year range)
  // are much wider than `%` / `W` / `°C`. The chart's tight `left: -20`
  // margin pulls the plot past the SVG's left edge and clips the leading `£`,
  // so the £ axis gets a non-negative left margin plus an explicit wider
  // gutter. Other units keep `left: -20` and the default width via a
  // conditional spread, so their rendered output is byte-identical to before
  // (adding a `width` prop globally once broke every chart's layout).
  const isCurrency = chart.unit === '£';

  return (
    <div className="bg-bg-elevated rounded-xl p-4 relative">
      <div className="flex items-center justify-between mb-3 gap-2">
        <div>
          <h3 className="text-text-primary text-sm font-sans font-bold">{chart.title}</h3>
          {chart.description && (
            <p className="mt-1 text-text-secondary/70 text-[11px] leading-snug font-sans">{chart.description}</p>
          )}
        </div>
        {chart.fields.length > 1 && (
          <SeriesLegend items={legendItems} muted={mutedSeries} onToggle={toggleSeries} />
        )}
      </div>
      <ResponsiveContainer width="100%" height={200}>
        <AreaChart data={seriesData} margin={{ top: 5, right: 5, left: isCurrency ? 0 : -20, bottom: 0 }}>
          <defs>
            {chart.fields.map((f, i) => (
              <linearGradient key={i} id={`grad-${chart.key}-${i}`} x1="0" y1="0" x2="0" y2="1">
                <stop offset="5%" stopColor={f.color} stopOpacity={0.3} />
                <stop offset="95%" stopColor={f.color} stopOpacity={0} />
              </linearGradient>
            ))}
          </defs>
          <CartesianGrid {...getHistoryChartGridProps(gridLineWeight)} />
          <XAxis
            dataKey="t"
            type="number"
            domain={domain}
            ticks={ticks}
            tickFormatter={(v: number) => formatHistoryXAxisTick(v, range)}
            stroke="#8B949E"
            tick={{ fontSize: 11, style: { fontWeight: 700 } }}
            tickLine={false}
            axisLine={false}
            minTickGap={getHistoryXAxisMinTickGap(range)}
          />
          <YAxis
            {...(isCurrency ? { width: 58 } : {})}
            stroke="#8B949E"
            tick={{ fontSize: 11, style: { fontWeight: 700 } }}
            tickLine={false}
            axisLine={false}
            domain={yDomain}
            tickFormatter={(v: number) =>
              chart.unit === '£' ? `£${v.toFixed(2)}`
                : chart.unit === 'kWh' ? `${v.toFixed(1)}`
                : chart.unit === '°C' ? `${v.toFixed(1)}°`
                : `${Math.round(v)}`
            }
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
            separator=""
            formatter={(value) => {
              const n = typeof value === 'number' ? value : 0;
              if (chart.unit === '£') return [`£${n.toFixed(2)}`, ''];
              if (chart.unit === 'kWh') return [`${n.toFixed(1)} ${chart.unit}`, ''];
              if (chart.unit === '°C') return [`${n.toFixed(1)} °C`, ''];
              return [`${Math.round(n)} ${chart.unit}`, ''];
            }}
          />
          {chart.fields.map((f, i) => (
            <Area
              key={i}
              type="monotone"
              dataKey={seriesNames[i]}
              stroke={f.color}
              strokeDasharray={f.strokeDasharray}
              fill={`url(#grad-${chart.key}-${i})`}
              opacity={getSeriesOpacity(mutedSeries[seriesNames[i]] ?? false)}
              strokeWidth={2}
              dot={false}
              isAnimationActive={false}
              connectNulls
            />
          ))}
        </AreaChart>
      </ResponsiveContainer>
    </div>
  );
}

// ---------------------------------------------------------------------------
// CSV Export
// ---------------------------------------------------------------------------

/// Download CSV with a Save As dialog (remote browser). Falls back to a
/// simple Blob download if the File System Access API is unavailable.
async function downloadWithPrompt(csvContent: string, fileName: string, onExported: () => void) {
  try {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const handle = await (window as any).showSaveFilePicker({
      suggestedName: fileName,
      types: [{
        description: 'CSV file',
        accept: { 'text/csv': ['.csv'] },
      }],
    });
    const writable = await handle.createWritable();
    await writable.write(csvContent);
    await writable.close();
    onExported();
  } catch (err: unknown) {
    // User cancelled the dialog — do nothing
    if (err instanceof DOMException && err.name === 'AbortError') return;
    // API unavailable — fall back to simple Blob download
    const blob = new Blob([csvContent], { type: 'text/csv;charset=utf-8;' });
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
    onExported();
  }
}

function exportCSV(
  charts: ChartDef[],
  data: Record<string, TimePoint[]>,
  range: HistoryRange,
  offset: number,
  onExported: () => void,
  fileLabel?: string,
) {
  // Collect all unique field names across all charts
  const allFields = [...new Set(charts.flatMap((c) => [
    ...c.fields.map((f) => f.field),
    ...(c.requires ?? []),
  ]))];

  // Build merged time series
  const timestamps = new Set<number>();
  for (const field of allFields) {
    const pts = data[field];
    if (pts) for (const p of pts) timestamps.add(p.t);
  }
  const sortedTs = [...timestamps].sort((a, b) => a - b);

  // Apply any client-side derivations (e.g. temperature differentials) so
  // their derived fields appear in the CSV. Cost/income are plain server
  // fields and need no preprocess.
  const derivedCharts = charts.filter((c) => c.preprocess);
  let processed: Record<string, number>[] = [];
  if (derivedCharts.length > 0) {
    const rawMerged = sortedTs.map((t) => {
      const row: Record<string, number> = { t };
      for (const f of allFields) {
        const pt = data[f]?.find((p) => p.t === t);
        if (pt) row[f] = pt.v;
      }
      return row;
    });
    for (const c of derivedCharts) {
      if (c.preprocess) processed = c.preprocess(rawMerged);
    }
  }

  // Build header + rows
  const header = ['Timestamp', ...allFields];
  const rows = sortedTs.map((t) => {
    const processedRow = processed.find((r) => r.t === t);
    const iso = new Date(t).toISOString();
    const values = allFields.map((f) => {
      if (processedRow && f in processedRow) return processedRow[f]?.toString() ?? '';
      const pt = data[f]?.find((p) => p.t === t);
      return pt?.v?.toString() ?? '';
    });
    return [iso, ...values];
  });

  const csvContent = [header.join(','), ...rows.map((r) => r.join(','))].join('\n');

  // Caller-supplied label (e.g. 'history' for the combined export) wins over
  // the default per-tab label of the first chart's key.
  const label = fileLabel ?? charts[0]?.key ?? 'export';
  const windowLabel = formatWindowLabel(range, offset).replace(/[^\w-]+/g, '_');
  const fileName = `givenergy_${label}_${windowLabel}.csv`;

  if (isTauri) {
    // Tauri app: simple download to default downloads folder
    const blob = new Blob([csvContent], { type: 'text/csv;charset=utf-8;' });
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
    onExported();
  } else {
    // Remote browser: show Save As dialog, fall back to download
    downloadWithPrompt(csvContent, fileName, onExported);
  }
}

// ---------------------------------------------------------------------------
// History Page
// ---------------------------------------------------------------------------

function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 60000);
    return () => clearInterval(id);
  }, []);
  return now;
}

export default function HistoryPage() {
  const [tab, setTab] = useState<MetricTab>('battery');
  const range = useInverterStore((state) => state.chartRange);
  const setChartRange = useInverterStore((state) => state.setChartRange);
  const gridLineWeight = useInverterStore((state) => state.gridLineWeight);
  const [offset, setOffset] = useState(0);
  const lastDateRef = useRef(getHistoryPickerValue(range, offset));
  const [data, setData] = useState<Record<string, TimePoint[]>>({});
  // Whether an import Standing Charge is configured. Drives the Cost tab's
  // import-cost breakdown lines: with no standing charge there's nothing to
  // break out, so the chart stays as Import Cost + Export Income.
  const [hasStandingCharge, setHasStandingCharge] = useState(false);

  useEffect(() => {
    let cancelled = false;
    apiGet<{ ok: boolean; data: PollSettings }>('/api/settings')
      .then((res) => {
        if (!cancelled) {
          setHasStandingCharge((res?.data?.import_standing_charge_p_per_day ?? 0) > 0);
        }
      })
      .catch(() => {
        // Settings unavailable: leave the breakdown hidden (unchanged chart).
      });
    return () => { cancelled = true; };
  }, []);

  const now = useNow();
  const rolling = isRollingHistoryRange(range);
  const refreshKey = shouldRefreshHistoryRange(range, offset) ? now : 0;
  // Keep the selected window fixed even when history only contains recent
  // startup data. Cropping 1h/6h to the first point can collapse the axis to
  // a few seconds, making several Recharts ticks format as the same HH:mm
  // label (issue #216).
  const displayDomain: [number, number] = getHistoryRangeDomain(range, offset, now);

  useEffect(() => {
    let cancelled = false;
    const charts = getCharts(tab, hasStandingCharge);
    const allFields = [
      ...new Set([
        ...charts.flatMap((c) => c.fields.map((f) => f.field)),
        ...charts.flatMap((c) => c.requires ?? []),
      ]),
    ];
    fetchHistory(range, allFields, offset, rolling)
      .then((result) => {
        if (!cancelled) {
          const cleaned: Record<string, TimePoint[]> = {};
          // Timestamps are UTC epoch ms; new Date(t) and the local-time axis
          // helpers localize them at render, so plot them unchanged.
          for (const [field, pts] of Object.entries(result)) {
            cleaned[field] = removeSpikes(pts, field);
          }
          setData(cleaned);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setData({});
        }
      })
    return () => { cancelled = true; };
  }, [tab, range, offset, refreshKey, rolling, hasStandingCharge]);

  const handleTabChange = (t: MetricTab) => {
    setTab(t);
    setOffset(0);
  };

  const handleRangeChange = (r: HistoryRange) => {
    setChartRange(r);
    setOffset(0);
  };

  const charts = getCharts(tab, hasStandingCharge);
  const hasData = Object.values(data).some((pts) => pts.length > 0);
  const [csvToast, setCsvToast] = useState<string | null>(null);
  // True while the combined-export fetch is in flight. Disables both export
  // buttons so the user can't queue two fetches against the same offset.
  const [exportingAll, setExportingAll] = useState(false);

  useEffect(() => {
    if (csvToast) {
      const id = setTimeout(() => setCsvToast(null), 3000);
      return () => clearTimeout(id);
    }
  }, [csvToast]);

  // Issue #199: combined export handler. The data dict the page keeps only
  // contains the active tab's series — for "Export all" we issue a one-shot
  // fetch covering every tab's fields, then call exportCSV with that result.
  // The fetch uses the same range/offset/rolling the user is looking at, so
  // the CSV matches the visible window.
  const handleExportAll = async () => {
    if (exportingAll) return;
    setExportingAll(true);
    try {
      const allCharts = getAllCharts(hasStandingCharge);
      const allFields = [
        ...new Set([
          ...allCharts.flatMap((c) => c.fields.map((f) => f.field)),
          ...allCharts.flatMap((c) => c.requires ?? []),
        ]),
      ];
      const result = await fetchHistory(range, allFields, offset, rolling);
      const cleaned: Record<string, TimePoint[]> = {};
      for (const [field, pts] of Object.entries(result)) {
        cleaned[field] = removeSpikes(pts, field);
      }
      exportCSV(
        allCharts,
        cleaned,
        range,
        offset,
        () => setCsvToast('Combined CSV downloaded — ' + formatWindowLabel(range, offset)),
        'history',
      );
    } finally {
      setExportingAll(false);
    }
  };

  return (
    <div className="flex flex-col gap-4 max-w-4xl mx-auto">
      {/* Tab bar — dropdown on mobile, buttons on desktop */}
      <div className="bg-bg-surface rounded-xl p-2 min-w-0">
        <select
          value={tab}
          onChange={(e) => handleTabChange(e.target.value as MetricTab)}
          className="sm:hidden w-full bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-sans font-medium outline-none border border-white/10"
          aria-label="Select metric"
        >
          {TABS.map((t) => (
            <option key={t.key} value={t.key}>{t.label}</option>
          ))}
        </select>
        <div className="hidden sm:flex gap-1">
          {TABS.map((t) => (
            <button
              key={t.key}
              onClick={() => handleTabChange(t.key)}
              className={`flex-1 px-3 py-2 rounded-lg text-sm font-sans font-medium transition-colors ${
                tab === t.key
                  ? 'bg-flow-active/20 text-flow-active'
                  : 'text-text-secondary hover:text-text-primary'
              }`}
            >
              {t.label}
            </button>
          ))}
        </div>
      </div>

      {/* Time range */}
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

      {/* Navigation. The paging buttons (Older / date / Newer) are the
          primary mobile action, so on phones the export pair drops to a
          second row to avoid horizontal overflow (issue #199 follow-up).
          On sm+ everything sits in one centered row, matching the original
          layout. */}
      <div className="flex flex-col items-stretch gap-2 sm:flex-row sm:items-center sm:justify-center sm:gap-1 bg-bg-surface rounded-xl p-1.5">
        {/* Paging group: Older / date-or-label / Newer. Wrapped in its own
            flex container so the three controls stay side-by-side on
            mobile (one sub-row) when the parent is flex-col. */}
        <div className="flex items-center justify-center gap-1">
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
              {formatWindowLabel(range, offset)}
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
        <span className="hidden sm:inline-block w-px h-4 bg-white/10 mx-1" />
        {/* Export pair sits in its own group so on mobile (sm-) it stacks
            below the paging controls instead of overflowing the row. The
            inner container takes the same `items-center gap-1` styling
            desktop and mobile, so the buttons themselves don't change. */}
        <div className="flex items-center justify-center gap-1">
          <button
            onClick={() => exportCSV(charts, data, range, offset, () => setCsvToast('CSV downloaded to your Downloads folder — ' + formatWindowLabel(range, offset)))}
            disabled={!hasData || exportingAll}
            className="shrink-0 text-text-secondary hover:text-text-primary text-xs font-sans px-2 py-1 rounded-lg hover:bg-bg-elevated transition-colors disabled:opacity-30 flex items-center gap-1"
          >
            <svg className="w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M12 10v6m0 0l-3-3m3 3l3-3m2 8H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z" />
            </svg>
            CSV
          </button>
          {/* Issue #199: "Export all" pulls every tab's series into a single
              file (Battery, Solar, Grid, Home, Temperature, Cost) instead of
              the per-tab CSV the per-tab button above produces. Disabled while
              the combined fetch is in flight to prevent stacking requests. */}
          <button
            onClick={handleExportAll}
            disabled={!hasData || exportingAll}
            aria-label="Export all tabs as a single combined CSV"
            className="shrink-0 text-text-secondary hover:text-text-primary text-xs font-sans px-2 py-1 rounded-lg hover:bg-bg-elevated transition-colors disabled:opacity-30 flex items-center gap-1"
          >
            <svg className="w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M12 10v6m0 0l-3-3m3 3l3-3m2 8H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z" />
            </svg>
            {exportingAll ? 'Preparing…' : 'Export all'}
          </button>
        </div>
      </div>

      {/* CSV export toast */}
      {csvToast && (
        <div className="fixed bottom-20 left-1/2 -translate-x-1/2 z-50 bg-bg-surface border border-battery/30 rounded-xl px-4 py-2.5 shadow-lg text-sm text-text-primary font-sans flex items-center gap-2 animate-in fade-in slide-in-from-bottom-2 duration-200">
          <svg className="w-4 h-4 text-battery shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
            <path strokeLinecap="round" strokeLinejoin="round" d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z" />
          </svg>
          {csvToast}
        </div>
      )}

      {/* Charts */}
      {!hasData ? (
        <div className="flex flex-col items-center justify-center py-20 gap-3">
          <p className="text-text-secondary text-sm font-sans">No data available for this period</p>
          <p className="text-text-secondary/50 text-xs font-sans">
            History is recorded while the app is running and connected
          </p>
        </div>
      ) : (
        <div className="flex flex-col gap-4">
          {charts.map((chart) => (
            <ChartCard
              key={chart.key}
              chart={chart}
              data={data}
              range={range}
              domain={displayDomain}
              ticks={getHistoryXAxisTicks(range, displayDomain)}
              gridLineWeight={gridLineWeight}
            />
          ))}
          {tab === 'temperature' && (
            <p className="text-text-secondary/60 text-[11px] font-sans">
              Outdoor temperature data by{' '}
              <button
                onClick={() => openExternal('https://open-meteo.com/')}
                className="text-flow-active underline hover:opacity-80 inline"
              >
                Open-Meteo.com
              </button>
              {' '}— licensed under CC BY 4.0. Configure your location in Settings.
            </p>
          )}
        </div>
      )}
    </div>
  );
}
