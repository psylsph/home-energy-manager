import { useState, useEffect } from 'react';
import {
  AreaChart,
  Area,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
} from 'recharts';
import { fetchHistory, apiGet } from '../lib/api';
import type { HistoryRange, PollSettings } from '../lib/types';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface TimePoint {
  t: number;
  v: number;
}

type MetricTab = 'battery' | 'solar' | 'grid' | 'home' | 'cost';

interface ChartDef {
  key: string;
  title: string;
  fields: { field: string; color: string; transform?: (v: number) => number }[];
  unit: string;
  yDomain?: [number, number];
}

// ---------------------------------------------------------------------------
// Outlier filtering
// ---------------------------------------------------------------------------

/// Per-field spike detection thresholds. A point is considered a spike if its
/// value differs from both neighbors by more than the threshold while the
/// neighbors differ by less than half the threshold.
const SPIKE_THRESHOLDS: Record<string, number> = {
  soc: 15,
  solar_power: 4000,
  pv1_power: 4000,
  pv2_power: 4000,
  battery_power: 4000,
  grid_power: 4000,
  home_power: 4000,
};

function removeSpikes(points: TimePoint[], field: string): TimePoint[] {
  if (points.length < 3) return points;
  const threshold = SPIKE_THRESHOLDS[field] ?? 4000;
  const result: TimePoint[] = [];
  for (let i = 0; i < points.length; i++) {
    if (i === 0 || i === points.length - 1) {
      result.push(points[i]);
      continue;
    }
    const prev = points[i - 1];
    const cur = points[i];
    const next = points[i + 1];
    const dPrev = Math.abs(cur.v - prev.v);
    const dNext = Math.abs(cur.v - next.v);
    const dNeighbors = Math.abs(next.v - prev.v);
    if (dPrev > threshold && dNext > threshold && dNeighbors < threshold * 0.5) {
      result.push({ t: cur.t, v: (prev.v + next.v) / 2 });
    } else {
      result.push(cur);
    }
  }
  return result;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const RANGES: { key: HistoryRange; label: string }[] = [
  { key: '1h', label: '1h' },
  { key: '6h', label: '6h' },
  { key: '24h', label: '24h' },
  { key: '7d', label: '7d' },
  { key: '30d', label: '30d' },
  { key: '6m', label: '6m' },
  { key: '1y', label: '1y' },
];

const TABS: { key: MetricTab; label: string }[] = [
  { key: 'battery', label: 'Battery' },
  { key: 'solar', label: 'Solar' },
  { key: 'grid', label: 'Grid' },
  { key: 'home', label: 'Home' },
  { key: 'cost', label: 'Cost' },
];

function getCharts(tab: MetricTab, importTariff: number, exportTariff: number): ChartDef[] {
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
            { field: 'battery_power', color: '#22C55E', transform: (v: number) => v > 0 ? v : 0 },
            { field: 'battery_power', color: '#EF4444', transform: (v: number) => v < 0 ? Math.abs(v) : 0 },
          ],
        },
        {
          key: 'battery-energy',
          title: 'Energy (kWh)',
          unit: 'kWh',
          fields: [
            { field: 'today_charge_kwh', color: '#22C55E' },
            { field: 'today_discharge_kwh', color: '#EF4444' },
          ],
        },
      ];
    case 'solar':
      return [
        {
          key: 'pv-power',
          title: 'PV Power (W)',
          unit: 'W',
          fields: [{ field: 'solar_power', color: '#F59E0B' }],
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
            { field: 'grid_power', color: '#22C55E', transform: (v: number) => v > 0 ? v : 0 },
            { field: 'grid_power', color: '#EF4444', transform: (v: number) => v < 0 ? Math.abs(v) : 0 },
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
          fields: [{ field: 'today_consumption_kwh', color: '#14B8A6' }],
        },
      ];
    case 'cost':
      return [
        {
          key: 'import-cost',
          title: 'Import Cost (£)',
          unit: '£',
          fields: [{ field: 'today_import_kwh', color: '#EF4444', transform: (v: number) => v * importTariff }],
        },
        {
          key: 'export-income',
          title: 'Export Income (£)',
          unit: '£',
          fields: [{ field: 'today_export_kwh', color: '#22C55E', transform: (v: number) => v * exportTariff }],
        },
      ];
  }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatXAxis(ts: number, range: HistoryRange): string {
  const d = new Date(ts);
  if (range === '1h' || range === '6h' || range === '24h') {
    return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }
  if (range === '7d' || range === '30d') {
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
  }
  return d.toLocaleDateString([], { month: 'short', year: 'numeric' });
}

function formatWindowLabel(range: HistoryRange, offset: number): string {
  if (offset === 0) return 'Now';
  const rangeMs: Record<string, number> = {
    '1h': 3600000,
    '6h': 21600000,
    '24h': 86400000,
    '7d': 604800000,
    '30d': 2592000000,
    '6m': 15552000000,
    '1y': 31536000000,
  };
  const ms = rangeMs[range] ?? 86400000;
  const end = new Date(Date.now() - offset * ms);
  const start = new Date(end.getTime() - ms);
  const fmt = (d: Date) =>
    range === '1h' || range === '6h' || range === '24h'
      ? d.toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
      : d.toLocaleDateString([], { month: 'short', day: 'numeric' });
  return `${fmt(start)} — ${fmt(end)}`;
}

// ---------------------------------------------------------------------------
// Single chart component
// ---------------------------------------------------------------------------

function ChartCard({ chart, data, range, domain }: {
  chart: ChartDef;
  data: Record<string, TimePoint[]>;
  range: HistoryRange;
  domain: [number, number];
}) {
  const allFields = chart.fields.map((f) => f.field);
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

  const merged = sortedTs.map((t) => {
    const row: Record<string, number> = { t };
    for (const f of uniqueFields) {
      const pts = rawPoints[f];
      const pt = pts.find((p) => p.t === t);
      if (pt) row[f] = pt.v;
    }
    return row;
  });

  const seriesNames = chart.fields.map((f, i) => {
    const suffix = chart.fields.filter((ff, j) => j < i && ff.field === f.field).length;
    return `${f.field}${suffix > 0 ? `_${suffix}` : ''}`;
  });

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

  return (
    <div className="bg-bg-elevated rounded-xl p-4">
      <h3 className="text-text-secondary text-xs font-sans font-medium mb-3">{chart.title}</h3>
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
          <CartesianGrid strokeDasharray="3 3" stroke="rgba(255,255,255,0.06)" />
          <XAxis
            dataKey="t"
            type="number"
            domain={domain}
            tickFormatter={(v: number) => formatXAxis(v, range)}
            stroke="#8B949E"
            tick={{ fontSize: 11 }}
            tickLine={false}
            axisLine={false}
            minTickGap={40}
          />
          <YAxis
            stroke="#8B949E"
            tick={{ fontSize: 11 }}
            tickLine={false}
            axisLine={false}
            domain={chart.yDomain}
            tickFormatter={(v: number) =>
              chart.unit === '£' ? `£${v.toFixed(2)}` : `${Math.round(v)}`
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
            formatter={(value) => {
              const n = typeof value === 'number' ? value : 0;
              if (chart.unit === '£') return [`£${n.toFixed(2)}`, chart.unit];
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
  const [range, setRange] = useState<HistoryRange>('24h');
  const [offset, setOffset] = useState(0);
  const [data, setData] = useState<Record<string, TimePoint[]>>({});
  const [loadingKey, setLoadingKey] = useState(0);
  const now = useNow();
  const rangeMs: Record<string, number> = {
    '1h': 3600000,
    '6h': 21600000,
    '24h': 86400000,
    '7d': 604800000,
    '30d': 2592000000,
    '6m': 15552000000,
    '1y': 31536000000,
  };
  const windowMs = rangeMs[range] ?? 86400000;
  const domainEnd = now - offset * windowMs;
  const xDomain: [number, number] = [domainEnd - windowMs, domainEnd];
  const [importTariff, setImportTariff] = useState(0.285);
  const [exportTariff, setExportTariff] = useState(0.15);

  useEffect(() => {
    (async () => {
      try {
        const res = await apiGet<{ ok: boolean; data: PollSettings }>('/api/settings');
        if (res.data.import_tariff) setImportTariff(res.data.import_tariff);
        if (res.data.export_tariff) setExportTariff(res.data.export_tariff);
      } catch { /* use defaults */ }
    })();
  }, []);

  const loading = loadingKey > 0;

  useEffect(() => {
    let cancelled = false;
    const charts = getCharts(tab, importTariff, exportTariff);
    const allFields = [...new Set(charts.flatMap((c) => c.fields.map((f) => f.field)))];
    fetchHistory(range, allFields, offset)
      .then((result) => {
        if (!cancelled) {
          const cleaned: Record<string, TimePoint[]> = {};
          for (const [field, pts] of Object.entries(result)) {
            cleaned[field] = removeSpikes(pts, field);
          }
          setData(cleaned);
          setLoadingKey((k) => Math.max(0, k - 1));
        }
      })
      .catch(() => {
        if (!cancelled) {
          setData({});
          setLoadingKey((k) => Math.max(0, k - 1));
        }
      });
    return () => { cancelled = true; };
  }, [tab, range, offset, importTariff, exportTariff]);

  const handleTabChange = (t: MetricTab) => {
    setTab(t);
    setOffset(0);
  };

  const handleRangeChange = (r: HistoryRange) => {
    setRange(r);
    setOffset(0);
  };

  const charts = getCharts(tab, importTariff, exportTariff);
  const hasData = Object.values(data).some((pts) => pts.length > 0);

  return (
    <div className="flex flex-col gap-4 max-w-4xl mx-auto">
      {/* Tab bar */}
      <div className="flex gap-1 bg-bg-surface rounded-xl p-1">
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

      {/* Time range */}
      <div className="flex items-center gap-2 bg-bg-surface rounded-xl p-2 overflow-x-auto">
        {RANGES.map((r) => (
          <button
            key={r.key}
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

      {/* Navigation */}
      <div className="flex items-center justify-center gap-2 bg-bg-surface rounded-xl p-2">
        <button
          onClick={() => setOffset((o) => o + 1)}
          className="shrink-0 text-text-secondary hover:text-text-primary text-xs font-sans px-2 py-1 rounded-lg hover:bg-bg-elevated transition-colors"
        >
          ◀ Older
        </button>
        <span className="text-text-secondary text-xs font-sans text-center truncate px-1">
          {formatWindowLabel(range, offset)}
        </span>
        <button
          onClick={() => setOffset((o) => Math.max(0, o - 1))}
          disabled={offset === 0}
          className="shrink-0 text-text-secondary hover:text-text-primary text-xs font-sans px-2 py-1 rounded-lg hover:bg-bg-elevated transition-colors disabled:opacity-30"
        >
          Newer ▶
        </button>
      </div>

      {/* Charts */}
      {loading ? (
        <div className="flex flex-col items-center justify-center py-20 gap-4">
          <div className="w-8 h-8 border-4 border-flow-active border-t-transparent rounded-full animate-spin" />
          <p className="text-text-secondary text-sm font-sans">Loading history…</p>
        </div>
      ) : !hasData ? (
        <div className="flex flex-col items-center justify-center py-20 gap-3">
          <p className="text-text-secondary text-sm font-sans">No data available for this period</p>
          <p className="text-text-secondary/50 text-xs font-sans">
            History is recorded while the app is running and connected
          </p>
        </div>
      ) : (
        <div className="flex flex-col gap-4">
          {charts.map((chart) => (
            <ChartCard key={chart.key} chart={chart} data={data} range={range} domain={xDomain} />
          ))}
        </div>
      )}
    </div>
  );
}
