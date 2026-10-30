import { useEffect, useMemo, useState } from 'react';
import {
  CartesianGrid,
  Legend,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';
import { fetchBatteryHealth } from '../lib/api';
import {
  HEALTH_RANGES,
  dayToMs,
  describeSocEvents,
  describeTrend,
  hasMetric,
  healthModules,
  latestPerModule,
  moduleKey,
  pivotHealth,
  socEventDays,
  type HealthMetric,
  type HealthModule,
} from '../lib/batteryHealth';
import { tooltipStyleProps } from '../lib/chartTooltip';
import { getHistoryChartGridProps } from '../lib/historyRangeConfig';
import { useInverterStore } from '../store/useInverterStore';
import type { BatteryHealth } from '../lib/types';

// Battery Health (issue #393): the daily per-module summary the backend keeps
// (cell spread, cycles, BMS-reported capacity, temperature) charted over
// months, so a failing cell or fading capacity shows as a trend. GivEnergy
// warranties are no longer honoured, so owners have to spot this themselves.

const REFRESH_MS = 5 * 60_000;

/** Most recent SOC warning days listed. */
const SOC_EVENTS_SHOWN = 10;

const TREND_TONE = {
  warn: 'text-amber-400',
  ok: 'text-text-secondary',
  muted: 'text-text-secondary/60',
} as const;

const MODULE_COLORS = ['#6366F1', '#F59E0B', '#10B981', '#EF4444', '#06B6D4', '#A855F7'];

type SpreadMode = 'full' | 'worst';

const SPREAD_METRIC: Record<SpreadMode, HealthMetric> = {
  full: 'full_spread_mv',
  worst: 'max_spread_mv',
};

function formatDay(ms: number): string {
  return new Date(ms).toLocaleDateString(undefined, { day: 'numeric', month: 'short' });
}

interface ChartProps {
  title: string;
  hint?: string;
  data: ReturnType<typeof pivotHealth>;
  modules: HealthModule[];
  unit: string;
  digits: number;
}

function HealthLineChart({ title, hint, data, modules, unit, digits }: ChartProps) {
  // Same grid as every other chart, including the user's grid-line weight.
  const gridLineWeight = useInverterStore((state) => state.gridLineWeight);
  return (
    <div className="space-y-1">
      <div className="text-text-secondary text-xs">{title}</div>
      {hint && <div className="text-text-secondary/60 text-[11px]">{hint}</div>}
      <ResponsiveContainer width="100%" height={180}>
        <LineChart data={data} margin={{ top: 5, right: 5, left: -15, bottom: 0 }}>
          <CartesianGrid {...getHistoryChartGridProps(gridLineWeight)} />
          <XAxis
            dataKey="t"
            type="number"
            scale="time"
            domain={['dataMin', 'dataMax']}
            tickFormatter={formatDay}
            stroke="#8B949E"
            tick={{ fontSize: 11, style: { fontWeight: 700 } }}
            tickLine={false}
            axisLine={false}
            minTickGap={40}
          />
          <YAxis
            stroke="#8B949E"
            tick={{ fontSize: 11, style: { fontWeight: 700 } }}
            tickLine={false}
            axisLine={false}
            domain={['auto', 'auto']}
            tickFormatter={(v: number) => v.toFixed(digits)}
          />
          <Tooltip
            {...tooltipStyleProps}
            labelFormatter={(v) => formatDay(Number(v))}
            formatter={(value) => `${Number(value).toFixed(digits)} ${unit}`}
          />
          {modules.length > 1 && <Legend wrapperStyle={{ fontSize: 11 }} />}
          {modules.map((m, i) => (
            <Line
              key={m.key}
              type="monotone"
              dataKey={m.key}
              name={`Module ${m.label}`}
              stroke={MODULE_COLORS[i % MODULE_COLORS.length]}
              strokeWidth={2}
              dot={data.length < 15}
              isAnimationActive={false}
              connectNulls
            />
          ))}
        </LineChart>
      </ResponsiveContainer>
    </div>
  );
}

function fmt(value: number | null, digits: number, unit: string): string {
  return value === null ? '—' : `${value.toFixed(digits)}${unit}`;
}

export default function BatteryHealthPanel() {
  const [days, setDays] = useState<number>(180);
  const [spreadMode, setSpreadMode] = useState<SpreadMode>('full');
  // The result remembers which range it answers, so a range change reads as
  // loading without setting state inside the effect.
  const [result, setResult] = useState<{ days: number; health: BatteryHealth | null } | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      fetchBatteryHealth(days)
        .then((health) => {
          if (!cancelled) setResult({ days, health });
        })
        .catch(() => {
          // A failed refresh keeps what is shown; only a failed first load
          // for this range reports an error.
          if (!cancelled) setResult((current) => (current?.days === days ? current : { days, health: null }));
        });
    };
    load();
    // Today's row keeps changing while the page is open.
    const timer = setInterval(load, REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [days]);

  const loading = result === null || result.days !== days;
  const failed = !loading && result.health === null;
  const rows = useMemo(() => (loading ? [] : (result.health?.days ?? [])), [loading, result]);
  const trends = useMemo(() => (loading ? [] : (result.health?.trend ?? [])), [loading, result]);
  const modules = useMemo(() => healthModules(rows), [rows]);
  const summaries = useMemo(() => latestPerModule(rows), [rows]);
  const socEvents = useMemo(() => socEventDays(rows).slice(0, SOC_EVENTS_SHOWN), [rows]);

  // Spread near full charge is the like-for-like figure; fall back to the
  // day's worst when no module has been nearly full in the range.
  const hasFull = hasMetric(rows, 'full_spread_mv');
  const effectiveMode: SpreadMode = spreadMode === 'full' && !hasFull ? 'worst' : spreadMode;
  const spreadData = useMemo(() => pivotHealth(rows, SPREAD_METRIC[effectiveMode]), [rows, effectiveMode]);
  const capacityData = useMemo(() => pivotHealth(rows, 'capacity_pct'), [rows]);
  const cyclesData = useMemo(() => pivotHealth(rows, 'cycles'), [rows]);
  const tempData = useMemo(() => pivotHealth(rows, 'max_temp_c'), [rows]);

  return (
    <section className="bg-bg-surface rounded-2xl p-6 space-y-4" aria-label="Battery Health">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="text-text-primary text-sm font-semibold tracking-wide">Battery Health</h3>
        <div className="flex gap-1" role="group" aria-label="Battery health range">
          {HEALTH_RANGES.map((r) => (
            <button
              key={r.label}
              type="button"
              aria-pressed={days === r.days}
              onClick={() => setDays(r.days)}
              className={`px-2.5 py-1 rounded-lg text-xs font-medium ${
                days === r.days ? 'bg-accent text-on-accent' : 'bg-bg-elevated text-text-secondary'
              }`}
            >
              {r.label}
            </button>
          ))}
        </div>
      </div>

      {loading ? (
        <div className="flex items-center justify-center h-[120px]">
          <div className="w-8 h-8 border-4 border-accent border-t-transparent rounded-full animate-spin" />
        </div>
      ) : failed ? (
        <p className="text-red-400 text-sm font-sans">Failed to load battery health</p>
      ) : rows.length === 0 ? (
        <div className="bg-bg-elevated/50 rounded-lg px-4 py-3">
          <p className="text-text-secondary text-sm">No battery health recorded yet.</p>
          <p className="text-text-secondary/60 text-xs mt-1">
            A daily summary per battery module (cell spread, cycles, capacity) is recorded while the app
            is running and connected. Trends appear after a few days.
          </p>
        </div>
      ) : (
        <>
          <div className="flex flex-col gap-2">
            {summaries.map((m) => {
              const trend = describeTrend(trends.find((t) => moduleKey(t.module_index) === m.key));
              return (
                <div
                  key={m.key}
                  className="bg-bg-elevated rounded-xl px-4 py-3 grid grid-cols-2 sm:grid-cols-4 gap-2 text-sm"
                >
                  <div>
                    <div className="text-text-secondary text-xs">Module</div>
                    <div className="text-text-primary font-mono">{m.label}</div>
                  </div>
                  <div>
                    <div className="text-text-secondary text-xs">
                      Cell spread{m.spreadKind ? (m.spreadKind === 'full' ? ' · near full' : " · day's worst") : ''}
                    </div>
                    <div className="text-text-primary font-mono">{fmt(m.spreadMv, 0, ' mV')}</div>
                  </div>
                  <div>
                    <div className="text-text-secondary text-xs">State of Health</div>
                    <div className="text-text-primary font-mono">{fmt(m.capacityPct, 0, '%')}</div>
                  </div>
                  <div>
                    <div className="text-text-secondary text-xs">Cycles</div>
                    <div className="text-text-primary font-mono">{fmt(m.cycles, 0, '')}</div>
                  </div>
                  {trend && (
                    <div className={`col-span-full text-xs ${TREND_TONE[trend.tone]}`}>{trend.text}</div>
                  )}
                </div>
              );
            })}
          </div>

          {socEvents.length > 0 && (
            <div className="bg-amber-900/20 border border-amber-700/30 rounded-xl px-4 py-3 space-y-1">
              <div className="text-text-primary text-xs font-medium">SOC warnings</div>
              <p className="text-text-secondary/80 text-[11px]">
                Days the battery&apos;s own state of charge jumped in one step or stayed put while energy
                flowed, the pattern of the GivEnergy BMS SOC bug. A full discharge and charge usually
                corrects it: set the battery reserve to 4%, run Force Discharge until it stops, then Force
                Charge to 100%, and put your reserve back afterwards.
              </p>
              <ul className="text-text-secondary text-xs space-y-0.5">
                {socEvents.map((e) => (
                  <li key={`${e.day}-${e.module_index}`}>
                    {formatDay(dayToMs(e.day))} · Module #{e.module_index + 1} · {describeSocEvents(e)}
                  </li>
                ))}
              </ul>
            </div>
          )}

          <div className="space-y-4">
            {spreadData.length > 0 && (
              <div className="space-y-2">
                <div className="flex gap-1" role="group" aria-label="Cell spread measure">
                  {hasFull && (
                    <button
                      type="button"
                      aria-pressed={effectiveMode === 'full'}
                      onClick={() => setSpreadMode('full')}
                      className={`px-2.5 py-1 rounded-lg text-xs font-medium ${
                        effectiveMode === 'full' ? 'bg-accent text-on-accent' : 'bg-bg-elevated text-text-secondary'
                      }`}
                    >
                      Near full
                    </button>
                  )}
                  <button
                    type="button"
                    aria-pressed={effectiveMode === 'worst'}
                    onClick={() => setSpreadMode('worst')}
                    className={`px-2.5 py-1 rounded-lg text-xs font-medium ${
                      effectiveMode === 'worst' ? 'bg-accent text-on-accent' : 'bg-bg-elevated text-text-secondary'
                    }`}
                  >
                    Day&apos;s worst
                  </button>
                </div>
                <HealthLineChart
                  title="Cell spread (highest − lowest cell)"
                  hint={
                    effectiveMode === 'full'
                      ? 'Largest spread seen each day while the module was above 95%. Cells drift apart near full and empty even when healthy, so trend this figure; a steady widening is the warning sign.'
                      : "Largest spread seen each day at any charge level. It rises near full and empty even on a healthy battery, so the near-full view is a fairer trend."
                  }
                  data={spreadData}
                  modules={modules}
                  unit="mV"
                  digits={0}
                />
              </div>
            )}
            {capacityData.length > 0 && (
              <HealthLineChart
                title="State of Health"
                hint="Capacity as a percentage of design, as the battery's own BMS estimates it. It only updates when the BMS recalibrates."
                data={capacityData}
                modules={modules}
                unit="%"
                digits={0}
              />
            )}
            {cyclesData.length > 0 && (
              <HealthLineChart title="Charge cycles" data={cyclesData} modules={modules} unit="" digits={0} />
            )}
            {tempData.length > 0 && (
              <HealthLineChart
                title="Hottest cell temperature (daily max)"
                data={tempData}
                modules={modules}
                unit="°C"
                digits={0}
              />
            )}
          </div>
        </>
      )}
    </section>
  );
}
