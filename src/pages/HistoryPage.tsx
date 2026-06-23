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
import { fetchHistory, apiGet, isTauri } from '../lib/api';
import {
  HISTORY_CHART_GRID_PROPS,
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
  shouldTrimHistoryRangeLeadingGap,
  supportsHistoryDate,
  trimDomainStartToFirstDataPoint,
} from '../lib/historyRangeConfig';
import { getSeriesOpacity, removeSpikes } from '../lib/chartSeries';
import { SeriesLegend } from '../components/SeriesLegend';
import { useInverterStore } from '../store/useInverterStore';
import type { SeriesLegendItem } from '../components/SeriesLegend';
import type { HistoryRange, PollSettings, TariffConfig } from '../lib/types';
import { rateForTimestamp, defaultTariffConfig, flatTariffConfig } from '../lib/tariff';
import { computeTempDifferential, computeBatteryExternalDifferential } from '../lib/temperatureChart';
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
  fields: { field: string; color: string; label?: string; transform?: (v: number) => number }[];
  unit: string;
  yDomain?: [number, number];
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

function getCharts(tab: MetricTab, importTariffCfg: TariffConfig, exportTariffCfg: TariffConfig): ChartDef[] {
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
          fields: [
            { field: 'battery_power', color: '#22C55E', label: 'Charge', transform: (v: number) => v < 0 ? Math.abs(v) : 0 },
            { field: 'battery_power', color: '#EF4444', label: 'Discharge', transform: (v: number) => v > 0 ? v : 0 },
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
          fields: [{ field: 'today_solar_kwh', color: '#F59E0B' }],
        },
      ];
    case 'grid':
      return [
        {
          key: 'grid-power',
          title: 'Grid Power (W)',
          unit: 'W',
          fields: [
            { field: 'grid_power', color: '#22C55E', label: 'Export', transform: (v: number) => v > 0 ? v : 0 },
            { field: 'grid_power', color: '#EF4444', label: 'Import', transform: (v: number) => v < 0 ? Math.abs(v) : 0 },
          ],
        },
        {
          key: 'grid-voltage',
          title: 'Grid Voltage (V)',
          unit: 'V',
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
          key: 'battery-temp',
          title: 'Battery Temperature',
          unit: '°C',
          fields: [{ field: 'battery_temperature', color: '#22C55E' }],
        },
        {
          key: 'inverter-temp',
          title: 'Inverter Temperature',
          unit: '°C',
          fields: [{ field: 'inverter_temperature', color: '#F59E0B' }],
        },
        {
          key: 'external-temp',
          title: 'Ambient Temperature',
          unit: '°C',
          fields: [{ field: 'external_temperature', color: '#38BDF8' }],
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
          title: 'Battery − Ambient (°C)',
          unit: '°C',
          fields: [{ field: '_batt_ext_diff', color: '#F472B6' }],
          requires: ['battery_temperature', 'external_temperature'],
          preprocess: (merged) => computeBatteryExternalDifferential(merged),
        },
      ];
    case 'cost':
      return [
        {
          key: 'import-cost',
          title: 'Import Cost (£)',
          unit: '£',
          fields: [{ field: '_import_cost', color: '#EF4444' }],
          requires: ['today_import_kwh'],
          preprocess: (merged) => {
            let prev: number | null = null;
            let acc = 0;
            return merged.map((row) => {
              const raw = row.today_import_kwh;
              let delta = 0;
              if (raw != null && prev != null) {
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
                // 2 kWh per bucket is generous: 10 kW sustained for 12 min.
                // Even for 1-minute buckets this is generous.
                // This is the last line of defense against corrupted counter
                // values that slip through the backend sanitizer.
                if (delta > 2) {
                  // Spike detected: zero the delta AND don't update prev,
                  // so the corrupted cumulative value doesn't permanently
                  // inflate the baseline. The next real reading will produce
                  // a catch-up delta (capped at 2), then prev re-syncs.
                  delta = 0;
                } else {
                  // Normal delta — advance the baseline.
                  prev = raw;
                }
              } else if (raw != null) {
                prev = raw;
              }
              const rate = rateForTimestamp(importTariffCfg, row.t) ?? importTariffCfg.slots[0]?.rate ?? 0;
              acc += delta * rate;
              return { ...row, _import_cost: acc };
            });
          },
        },
        {
          key: 'export-income',
          title: 'Export Income (£)',
          unit: '£',
          fields: [{ field: '_export_income', color: '#22C55E' }],
          requires: ['today_export_kwh'],
          preprocess: (merged) => {
            let prev: number | null = null;
            let acc = 0;
            return merged.map((row) => {
              const raw = row.today_export_kwh;
              let delta = 0;
              if (raw != null && prev != null) {
                if (raw >= prev) {
                  delta = raw - prev;
                } else if (prev > 5 && raw < 5) {
                  delta = raw;
                }
                // Clamp delta to physically plausible maximum (same as import).
                if (delta > 2) delta = 0;
              }
              if (raw != null) prev = raw;
              const rate = rateForTimestamp(exportTariffCfg, row.t) ?? exportTariffCfg.slots[0]?.rate ?? 0;
              acc += delta * rate;
              return { ...row, _export_income: acc };
            });
          },
        },
      ];
  }
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

function ChartCard({ chart, data, range, domain, ticks }: {
  chart: ChartDef;
  data: Record<string, TimePoint[]>;
  range: HistoryRange;
  domain: [number, number];
  ticks?: number[];
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
      const raw = row[f.field];
      const value = raw !== undefined && f.transform ? f.transform(raw) : (raw ?? null);
      out[name] = value ?? null;
    });
    return out;
  });

  // Charts use their declared yDomain (e.g. SOC fixed at 0-100) or Recharts
  // auto-scaling otherwise. The shared Y-axis lock feature is scoped to the
  // Solar power chart on the dashboard only (see SolarPowerChart.tsx).
  const yDomain: [number, number] | undefined = chart.yDomain;

  return (
    <div className="bg-bg-elevated rounded-xl p-4 relative">
      <div className="flex items-center justify-between mb-3 gap-2">
        <h3 className="text-text-primary text-sm font-sans font-bold">{chart.title}</h3>
        {chart.fields.length > 1 && (
          <SeriesLegend items={legendItems} muted={mutedSeries} onToggle={toggleSeries} />
        )}
      </div>
      <ResponsiveContainer width="100%" height={200}>
        <AreaChart data={seriesData} margin={{ top: 5, right: 5, left: -20, bottom: 0 }}>
          <defs>
            {chart.fields.map((f, i) => (
              <linearGradient key={i} id={`grad-${chart.key}-${i}`} x1="0" y1="0" x2="0" y2="1">
                <stop offset="5%" stopColor={f.color} stopOpacity={0.3} />
                <stop offset="95%" stopColor={f.color} stopOpacity={0} />
              </linearGradient>
            ))}
          </defs>
          <CartesianGrid {...HISTORY_CHART_GRID_PROPS} />
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
            stroke="#8B949E"
            tick={{ fontSize: 11, style: { fontWeight: 700 } }}
            tickLine={false}
            axisLine={false}
            domain={yDomain ?? chart.yDomain}
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

function exportCSV(charts: ChartDef[], data: Record<string, TimePoint[]>, range: HistoryRange, offset: number, onExported: () => void) {
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

  // Handle preprocess for cost tab
  const costCharts = charts.filter((c) => c.preprocess);
  let processed: Record<string, number>[] = [];
  if (costCharts.length > 0) {
    const rawMerged = sortedTs.map((t) => {
      const row: Record<string, number> = { t };
      for (const f of allFields) {
        const pt = data[f]?.find((p) => p.t === t);
        if (pt) row[f] = pt.v;
      }
      return row;
    });
    for (const c of costCharts) {
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

  const label = charts[0]?.key ?? 'export';
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
  const [offset, setOffset] = useState(0);
  const lastDateRef = useRef(getHistoryPickerValue(range, offset));
  const [data, setData] = useState<Record<string, TimePoint[]>>({});
  
  const now = useNow();
  const rolling = isRollingHistoryRange(range);
  const refreshKey = shouldRefreshHistoryRange(range, offset) ? now : 0;
  const xDomain: [number, number] = getHistoryRangeDomain(range, offset, now);
  const displayDomain = shouldTrimHistoryRangeLeadingGap(range)
    ? trimDomainStartToFirstDataPoint(xDomain, data)
    : xDomain;

  const [importTariffCfg, setImportTariffCfg] = useState<TariffConfig>(() => defaultTariffConfig());
  const [exportTariffCfg, setExportTariffCfg] = useState<TariffConfig>(() =>
    flatTariffConfig(0.15),
  );

  useEffect(() => {
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; data: PollSettings }>('/api/settings');
        if (res.data.import_tariff_config) {
          setImportTariffCfg(res.data.import_tariff_config);
        } else if (res.data.import_tariff) {
          setImportTariffCfg(flatTariffConfig(res.data.import_tariff));
        }
        if (res.data.export_tariff_config) {
          setExportTariffCfg(res.data.export_tariff_config);
        } else if (res.data.export_tariff) {
          setExportTariffCfg(flatTariffConfig(res.data.export_tariff));
        }
      } catch { /* use defaults */ }
    })();
  }, []);

  

  useEffect(() => {
    let cancelled = false;
    const charts = getCharts(tab, importTariffCfg, exportTariffCfg);
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
          // Convert UTC timestamps from the backend to local time so the
          // chart's X-axis (which uses local-time domain boundaries) aligns
          // correctly. The inverter's today_*_kwh counters reset at UTC
          // midnight; without this shift the reset appears 1 hour late in
          // timezones east of UTC (e.g. 01:00 in BST).
          //
          // `getTimezoneOffset()` returns minutes to *add* to local time to
          // get UTC (negative for zones east of UTC), so adding it to a UTC
          // timestamp gives the equivalent local-time epoch ms.
          //
          // The shift also pushes points from 00:00–01:00 local back into
          // the previous local day (they still carry yesterday's counter
          // values). Trim those — they're outside the Today window and would
          // otherwise extend the X-axis to start before midnight.
          const tzOffsetMs = new Date().getTimezoneOffset() * 60 * 1000;
          const domainStartMs = xDomain[0];
          for (const [field, pts] of Object.entries(result)) {
            cleaned[field] = removeSpikes(
              pts
                .map((p) => ({ t: p.t + tzOffsetMs, v: p.v }))
                .filter((p) => p.t >= domainStartMs),
              field,
            );
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
  }, [tab, range, offset, importTariffCfg, exportTariffCfg, refreshKey, rolling, xDomain]);

  const handleTabChange = (t: MetricTab) => {
    setTab(t);
    setOffset(0);
  };

  const handleRangeChange = (r: HistoryRange) => {
    setChartRange(r);
    setOffset(0);
  };

  const charts = getCharts(tab, importTariffCfg, exportTariffCfg);
  const hasData = Object.values(data).some((pts) => pts.length > 0);
  const [csvToast, setCsvToast] = useState<string | null>(null);

  useEffect(() => {
    if (csvToast) {
      const id = setTimeout(() => setCsvToast(null), 3000);
      return () => clearTimeout(id);
    }
  }, [csvToast]);

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

      {/* Navigation */}
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
        <span className="w-px h-4 bg-white/10 mx-1" />
        <button
          onClick={() => exportCSV(charts, data, range, offset, () => setCsvToast('CSV downloaded to your Downloads folder — ' + formatWindowLabel(range, offset)))}
          disabled={!hasData}
          className="shrink-0 text-text-secondary hover:text-text-primary text-xs font-sans px-2 py-1 rounded-lg hover:bg-bg-elevated transition-colors disabled:opacity-30 flex items-center gap-1"
        >
          <svg className="w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
            <path strokeLinecap="round" strokeLinejoin="round" d="M12 10v6m0 0l-3-3m3 3l3-3m2 8H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z" />
          </svg>
          CSV
        </button>
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
            />
          ))}
          {tab === 'temperature' && (
            <p className="text-text-secondary/60 text-[11px] font-sans">
              Ambient temperature data by{' '}
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
