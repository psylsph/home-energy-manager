import { useEffect, useMemo, useState } from 'react';
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
import { fetchHistory } from '../lib/api';
import {
  HISTORY_CHART_GRID_PROPS,
  HISTORY_RANGES,
  getHistoryRangeDomain,
  getHistoryXAxisMinTickGap,
  getHistoryXAxisTicks,
  formatHistoryXAxisTick,
  isRollingHistoryRange,
  shouldRefreshHistoryRange,
  shouldTrimHistoryRangeLeadingGap,
  trimDomainStartToFirstDataPoint,
} from '../lib/historyRangeConfig';
import { formatPower } from '../lib/format';
import type { HistoryRange, TimePoint } from '../lib/types';
import { useInverterStore } from '../store/useInverterStore';

type PowerSeriesKey =
  | 'solarPower'
  | 'batteryPower'
  | 'gridPower'
  | 'homePower';

interface PowerRow {
  t: number;
  solarPower: number | null;
  batteryPower: number | null;
  gridPower: number | null;
  homePower: number | null;
}

interface PowerHistoryState {
  range: HistoryRange | null;
  data: Record<string, TimePoint[]>;
  error: string;
}

const HISTORY_FIELDS = ['solar_power', 'battery_power', 'grid_power', 'home_power'];
const EMPTY_HISTORY_DATA: Record<string, TimePoint[]> = {};

const POWER_SERIES: { key: PowerSeriesKey; label: string; color: string }[] = [
  { key: 'solarPower', label: 'Combined PV', color: '#F59E0B' },
  { key: 'batteryPower', label: 'Battery', color: '#22C55E' },
  { key: 'gridPower', label: 'Grid', color: '#EF4444' },
  { key: 'homePower', label: 'Load / Home', color: '#14B8A6' },
];

const HOME_POWER_SERIES = POWER_SERIES.find((series) => series.key === 'homePower');
const DIRECTIONAL_POWER_SERIES = POWER_SERIES.filter((series) => series.key !== 'homePower');

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

    return {
      t,
      solarPower: solarValue == null ? null : Math.max(solarValue, 0),
      batteryPower: batteryValue == null ? null : -batteryValue,
      gridPower: gridValue == null ? null : -gridValue,
      homePower: homeValue == null ? null : Math.max(homeValue, 0),
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

export default function PowerPage() {
  const snapshot = useInverterStore((state) => state.snapshot);
  const [range, setRange] = useState<HistoryRange>('24h');
  const now = useNow();
  const rolling = isRollingHistoryRange(range);
  const refreshKey = shouldRefreshHistoryRange(range) ? now : 0;
  const [history, setHistory] = useState<PowerHistoryState>({
    range: null,
    data: {},
    error: '',
  });

  useEffect(() => {
    let cancelled = false;
    fetchHistory(range, HISTORY_FIELDS, 0, rolling)
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
  }, [range, refreshKey, rolling]);

  const loading = history.range !== range;
  const data = loading ? EMPTY_HISTORY_DATA : history.data;
  const error = loading ? '' : history.error;
  const rows = useMemo(() => buildPowerRows(data), [data]);
  const xDomain = useMemo(() => getHistoryRangeDomain(range, 0, now), [range, now]);
  const displayDomain = useMemo(
    () => shouldTrimHistoryRangeLeadingGap(range) ? trimDomainStartToFirstDataPoint(xDomain, data) : xDomain,
    [data, range, xDomain],
  );
  const yDomain = useMemo(() => calculateDomain(rows), [rows]);
  const hasData = rows.length > 0;
  const waitingForLiveData = snapshot == null;

  const currentSolar = Math.max(snapshot?.solar_power ?? 0, 0);
  const currentBattery = snapshot?.battery_power ?? 0;
  const currentGrid = snapshot?.grid_power ?? 0;
  const currentHome = Math.max(snapshot?.home_power ?? 0, 0);
  const batteryDirection = currentBattery < 0 ? 'Discharging' : currentBattery > 0 ? 'Charging' : 'Idle';
  const batteryColor = currentBattery < 0 ? '#22C55E' : currentBattery > 0 ? '#6366F1' : '#8B949E';
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
          {snapshot ? new Date(snapshot.timestamp).toLocaleTimeString() : 'Waiting for data'}
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

      <div className="flex items-center gap-2 bg-bg-surface rounded-xl p-2 overflow-x-auto">
        {HISTORY_RANGES.map((r) => (
          <button
            key={r.key}
            type="button"
            onClick={() => setRange(r.key)}
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

      <div className="bg-bg-elevated rounded-xl p-4">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between mb-3">
          <h2 className="text-text-primary text-sm font-sans font-bold">Power Flow</h2>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
            {POWER_SERIES.map((series) => (
              <span
                key={series.key}
                className="flex items-center gap-1.5 text-xs font-sans font-semibold"
              >
                <span
                  className="inline-block w-2.5 h-2.5 rounded-full shrink-0"
                  style={{ backgroundColor: series.color }}
                />
                <span className="text-text-secondary">{series.label}</span>
              </span>
            ))}
          </div>
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
            <ComposedChart data={rows} margin={{ top: 10, right: 10, left: -12, bottom: 0 }}>
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
              <CartesianGrid {...HISTORY_CHART_GRID_PROPS} />
              <ReferenceLine y={0} stroke="rgba(255,255,255,0.28)" strokeWidth={1.5} />
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
                stroke="#8B949E"
                tick={{ fontSize: 11, style: { fontWeight: 700 } }}
                tickLine={false}
                axisLine={false}
                domain={yDomain}
                tickFormatter={(v: number) => formatAxisWatts(v)}
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
                  const key = String(name) as PowerSeriesKey;
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
                  type="monotone"
                  dataKey={series.key}
                  stroke={series.color}
                  fill={`url(#power-grad-${series.key})`}
                  strokeWidth={2}
                  dot={false}
                  isAnimationActive={false}
                  connectNulls
                />
              ))}
              {HOME_POWER_SERIES && (
                <Line
                  type="monotone"
                  dataKey={HOME_POWER_SERIES.key}
                  stroke={HOME_POWER_SERIES.color}
                  strokeWidth={3}
                  dot={false}
                  activeDot={{ r: 4 }}
                  isAnimationActive={false}
                  connectNulls
                />
              )}
            </ComposedChart>
          </ResponsiveContainer>
        )}
      </div>
    </div>
  );
}
