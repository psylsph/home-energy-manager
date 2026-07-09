import { useEffect, useMemo, useState } from 'react';
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
import { solarChartNameplateCeilingFromArrays, SOLAR_PV1_COLOR as PV1_COLOR, SOLAR_PV2_COLOR as PV2_COLOR } from '../lib/solarArrays';
import {
  getHistoryChartGridProps,
  formatHistoryXAxisTick,
  getHistoryRangeDomain,
  getHistoryXAxisMinTickGap,
  getHistoryXAxisTicks,
  isRollingHistoryRange,
  shouldRefreshHistoryRange,
} from '../lib/historyRangeConfig';
import { useInverterStore } from '../store/useInverterStore';
import type { TimePoint } from '../lib/types';

// "PV Power (W)" chart from the History → Solar tab, replicated on the Solar
// tab so the tab carries its own solar-output trend (issue #81). The time
// scale follows the user's "Panel Graphs" preference (Today or Rolling 24H).
// Same fetch path, spike filter, and axis helpers as the History charts — the
// Power/History solar graphs are left untouched ("repeat, not move"). PV2
// renders only when the solar history has any non-zero pv2_power sample,
// matching the page's live auto-detect.

const PV1_FIELD = 'pv1_power';
const PV2_FIELD = 'pv2_power';

function chartTitle(scale: 'today' | '24h'): string {
  return scale === '24h' ? 'Solar Power — Last 24h' : 'Solar Power Today';
}

interface PvRow {
  t: number;
  pv1_power: number | null;
  pv2_power: number | null;
}

function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 60000);
    return () => clearInterval(id);
  }, []);
  return now;
}

/** Find the maximum power value across PV1 (and PV2 if present) in chart rows.
 *  Rounds up to a generous coarse ceiling (every 5kW) so the Y-axis stays
 *  stable across time ranges that have different AVG bucket sizes.
 */
function computeYMax(rows: PvRow[], hasPv2: boolean): number {
  let max = 0;
  for (const r of rows) {
    if (r.pv1_power !== null && r.pv1_power > max) max = r.pv1_power;
    if (hasPv2 && r.pv2_power !== null && r.pv2_power > max) max = r.pv2_power;
  }
  // Round up to next 5kW to give a stable ceiling across range switches.
  // Past 10kW, round to next 10kW. This avoids the Y-axis jumping when
  // switching ranges (different time-bucket AVG sizes produce different
  // data max values from the same underlying readings).
  if (max <= 10000) return Math.ceil(max / 5000) * 5000 || 5000;
  return Math.ceil(max / 10000) * 10000;
}

export default function SolarPowerChart() {
  const scale = useInverterStore((state) => state.panelGraphsScale);
  const gridLineWeight = useInverterStore((state) => state.gridLineWeight);
  const yLock = useInverterStore((state) => state.panelGraphsYLock);
  const yLockMax = useInverterStore((state) => state.panelGraphsYLockMax);
  const setYLockMax = useInverterStore((state) => state.setPanelGraphsYLockMax);
  // DC-string nameplate capacities (issue #192): drive a static Y-axis
  // ceiling so the chart is scaled to "how full are the panels", not to
  // whatever the largest sample happens to be.
  const solarArrays = useInverterStore((state) => state.snapshot?.solar_arrays);
  const range = scale;
  const rolling = isRollingHistoryRange(range);
  const now = useNow();
  const refreshKey = shouldRefreshHistoryRange(range, 0) ? now : 0;
  const [pv1, setPv1] = useState<TimePoint[] | null>(null);
  const [pv2, setPv2] = useState<TimePoint[]>([]);
  const [error, setError] = useState(false);

  useEffect(() => {
    let cancelled = false;
    fetchHistory(range, [PV1_FIELD, PV2_FIELD], 0, rolling)
      .then((result) => {
        if (cancelled) return;
        setPv1(removeSpikes(result[PV1_FIELD] ?? [], PV1_FIELD));
        setPv2(removeSpikes(result[PV2_FIELD] ?? [], PV2_FIELD));
        setError(false);
      })
      .catch(() => {
        if (cancelled) return;
        setPv1([]);
        setPv2([]);
        setError(true);
      });
    return () => {
      cancelled = true;
    };
  }, [range, rolling, refreshKey]);

  // Merge PV1 + PV2 by timestamp into chart rows.
  const rows: PvRow[] = useMemo(() => {
    if (pv1 === null) return [];
    const byT = new Map<number, PvRow>();
    const ensure = (t: number) => {
      let row = byT.get(t);
      if (!row) {
        row = { t, pv1_power: null, pv2_power: null };
        byT.set(t, row);
      }
      return row;
    };
    for (const p of pv1) ensure(p.t).pv1_power = p.v;
    for (const p of pv2) ensure(p.t).pv2_power = p.v;
    return [...byT.values()].sort((a, b) => a.t - b.t);
  }, [pv1, pv2]);

  const hasPv2 = pv2.some((p) => p.v > 0);
  const domain = getHistoryRangeDomain(range, 0, now);
  const ticks = getHistoryXAxisTicks(range, domain);
  const hasData = rows.length > 0;

  // Static Y-axis ceiling from the configured DC-string nameplate (issue
  // #192): the higher of the two PV string sizes (not their sum), so each
  // string is read against a single string's peak. Takes precedence over the
  // data-driven Y-Lock below — the ceiling no longer depends on observed
  // samples, so it's stable across Today/24h range switches by construction.
  // `null` when no DC-string capacity is configured → fall back to Y-Lock.
  const nameplateCeilingW = useMemo(
    () => solarChartNameplateCeilingFromArrays(solarArrays),
    [solarArrays],
  );

  // Y-axis domain: nameplate ceiling first (issue #192), else the shared
  // data-driven Y-Lock ceiling when the user has it enabled.
  let yDomain: [number, number] | undefined;
  if (nameplateCeilingW != null && rows.length > 0) {
    yDomain = [0, nameplateCeilingW];
  } else if (yLock && rows.length > 0) {
    const ceiling = computeYMax(rows, hasPv2);
    const shared = Math.max(yLockMax, ceiling);
    if (shared > yLockMax) setYLockMax(shared);
    yDomain = [0, shared];
  }

  return (
    <section className="bg-bg-surface rounded-2xl p-5">
      <h3 className="text-text-primary text-sm font-semibold tracking-wide mb-3">
        {chartTitle(scale)}
      </h3>
      {pv1 === null ? (
        <div className="flex items-center justify-center h-[180px]">
          <div className="w-8 h-8 border-4 border-flow-active border-t-transparent rounded-full animate-spin" />
        </div>
      ) : error ? (
        <div className="flex items-center justify-center h-[180px] text-red-400 text-sm font-sans">
          Failed to load solar history
        </div>
      ) : !hasData ? (
        <div className="flex flex-col items-center justify-center h-[180px] gap-1">
          <p className="text-text-secondary text-sm font-sans">No solar history yet today</p>
          <p className="text-text-secondary/50 text-xs font-sans">
            History is recorded while the app is running and connected
          </p>
        </div>
      ) : (
        <ResponsiveContainer width="100%" height={180}>
          <AreaChart data={rows} margin={{ top: 5, right: 5, left: -20, bottom: 0 }}>
            <defs>
              <linearGradient id="grad-solar-pv1" x1="0" y1="0" x2="0" y2="1">
                <stop offset="5%" stopColor={PV1_COLOR} stopOpacity={0.3} />
                <stop offset="95%" stopColor={PV1_COLOR} stopOpacity={0} />
              </linearGradient>
              <linearGradient id="grad-solar-pv2" x1="0" y1="0" x2="0" y2="1">
                <stop offset="5%" stopColor={PV2_COLOR} stopOpacity={0.3} />
                <stop offset="95%" stopColor={PV2_COLOR} stopOpacity={0} />
              </linearGradient>
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
              stroke="#8B949E"
              tick={{ fontSize: 11, style: { fontWeight: 700 } }}
              tickLine={false}
              axisLine={false}
              domain={yDomain}
              tickFormatter={(v: number) => `${Math.round(v)}`}
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
                const n = typeof value === 'number' ? value : 0;
                return [`${Math.round(n)} W`, name];
              }}
            />
            <Area
              type="monotone"
              dataKey={PV1_FIELD}
              name="PV1"
              stroke={PV1_COLOR}
              fill="url(#grad-solar-pv1)"
              strokeWidth={2}
              dot={false}
              isAnimationActive={false}
              connectNulls
            />
            {hasPv2 && (
              <Area
                type="monotone"
                dataKey={PV2_FIELD}
                name="PV2"
                stroke={PV2_COLOR}
                fill="url(#grad-solar-pv2)"
                strokeWidth={2}
                dot={false}
                isAnimationActive={false}
                connectNulls
              />
            )}
          </AreaChart>
        </ResponsiveContainer>
      )}
    </section>
  );
}
