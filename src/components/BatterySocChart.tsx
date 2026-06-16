import { useEffect, useState } from 'react';
import {
  Area,
  AreaChart,
  CartesianGrid,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';
import { fetchHistory } from '../lib/api';
import { removeSpikes } from '../lib/chartSeries';
import {
  HISTORY_CHART_GRID_PROPS,
  formatHistoryXAxisTick,
  getHistoryRangeDomain,
  getHistoryXAxisMinTickGap,
  getHistoryXAxisTicks,
  shouldRefreshHistoryRange,
} from '../lib/historyRangeConfig';
import type { TimePoint } from '../lib/types';

// "SOC %" chart from the History → Battery tab, replicated on the Battery tab
// and pinned to today so the Battery tab carries its own SOC-over-time trend
// (something the Status page does not show). Same fetch path, spike filter,
// and axis helpers as the History charts — no parallel data pipeline.

const SOC_COLOR = '#6366F1';
const RANGE = 'today' as const;
const FIELD = 'soc';

function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 60000);
    return () => clearInterval(id);
  }, []);
  return now;
}

export default function BatterySocChart() {
  const now = useNow();
  const refreshKey = shouldRefreshHistoryRange(RANGE, 0) ? now : 0;
  const [points, setPoints] = useState<TimePoint[] | null>(null);
  const [error, setError] = useState(false);

  useEffect(() => {
    let cancelled = false;
    fetchHistory(RANGE, [FIELD], 0, false)
      .then((result) => {
        if (cancelled) return;
        const raw = result[FIELD] ?? [];
        setPoints(removeSpikes(raw, FIELD));
        setError(false);
      })
      .catch(() => {
        if (cancelled) return;
        setPoints([]);
        setError(true);
      });
    return () => {
      cancelled = true;
    };
  }, [refreshKey]);

  const domain = getHistoryRangeDomain(RANGE, 0, now);
  const ticks = getHistoryXAxisTicks(RANGE, domain);
  const hasData = (points?.length ?? 0) > 0;

  return (
    <section className="bg-bg-surface rounded-2xl p-5">
      <h3 className="text-text-primary text-sm font-semibold tracking-wide mb-3">
        SOC Today
      </h3>
      {points === null ? (
        <div className="flex items-center justify-center h-[180px]">
          <div className="w-8 h-8 border-4 border-flow-active border-t-transparent rounded-full animate-spin" />
        </div>
      ) : error ? (
        <div className="flex items-center justify-center h-[180px] text-red-400 text-sm font-sans">
          Failed to load SOC history
        </div>
      ) : !hasData ? (
        <div className="flex flex-col items-center justify-center h-[180px] gap-1">
          <p className="text-text-secondary text-sm font-sans">No SOC history yet today</p>
          <p className="text-text-secondary/50 text-xs font-sans">
            History is recorded while the app is running and connected
          </p>
        </div>
      ) : (
        <ResponsiveContainer width="100%" height={180}>
          <AreaChart data={points} margin={{ top: 5, right: 5, left: -20, bottom: 0 }}>
            <defs>
              <linearGradient id="grad-battery-soc" x1="0" y1="0" x2="0" y2="1">
                <stop offset="5%" stopColor={SOC_COLOR} stopOpacity={0.3} />
                <stop offset="95%" stopColor={SOC_COLOR} stopOpacity={0} />
              </linearGradient>
            </defs>
            <CartesianGrid {...HISTORY_CHART_GRID_PROPS} />
            <XAxis
              dataKey="t"
              type="number"
              domain={domain}
              ticks={ticks}
              tickFormatter={(v: number) => formatHistoryXAxisTick(v, RANGE)}
              stroke="#8B949E"
              tick={{ fontSize: 11, style: { fontWeight: 700 } }}
              tickLine={false}
              axisLine={false}
              minTickGap={getHistoryXAxisMinTickGap(RANGE)}
            />
            <YAxis
              stroke="#8B949E"
              tick={{ fontSize: 11, style: { fontWeight: 700 } }}
              tickLine={false}
              axisLine={false}
              domain={[0, 100]}
              tickFormatter={(v: number) => `${Math.round(v)}%`}
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
                return [`${Math.round(n)}%`, 'SOC'];
              }}
            />
            <Area
              type="monotone"
              dataKey="v"
              name="SOC"
              stroke={SOC_COLOR}
              fill="url(#grad-battery-soc)"
              strokeWidth={2}
              dot={false}
              isAnimationActive={false}
              connectNulls
            />
          </AreaChart>
        </ResponsiveContainer>
      )}
    </section>
  );
}
