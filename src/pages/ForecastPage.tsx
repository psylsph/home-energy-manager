import { useEffect, useRef, useState } from 'react';
import {
  Area,
  AreaChart,
  CartesianGrid,
  ComposedChart,
  Customized,
  Legend,
  Line,
  LineChart,
  ReferenceLine,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';
import { apiGet, apiPost } from '../lib/api';
import { timeAxisProps } from '../lib/chartAxis';
import { tooltipStyleProps } from '../lib/chartTooltip';
import { formatEnergy } from '../lib/format';
import { getHistoryChartGridProps } from '../lib/historyRangeConfig';
import {
  anchorSeriesAtNow,
  FORECAST_HORIZON_HOURS,
  forecastChargeMarkers,
  truncateSeriesAtNextChargeStart,
  formatForecastXAxisTick,
  forecastPlanTitle,
  forecastStatusMessages,
  forecastXAxisTicks,
  forecastYAxisScale,
  forwardHourTimestamps,
  insertChargeStartVertices,
  relabelToStateInstants,
  shouldRefetchForecast,
  toBatteryChartData,
  toConsumptionChartData,
  toSolarChartData,
  tomorrowSummary,
} from '../lib/forecast';
import type { ForecastData, PlanResponse } from '../lib/forecast';
import type { InverterSnapshot } from '../lib/types';
import { useInverterStore } from '../store/useInverterStore';

/**
 * Forecast & planning page (issue #283).
 *
 * Displays the backend's assembled prediction: tomorrow's solar and
 * consumption with uncertainty bands, the projected battery SOC, and the
 * headline numbers. Everything is computed backend-side from Open-Meteo
 * radiation plus the user's own history; this page only renders. Every
 * degradation state renders an explanation — never zeros pretending to be
 * a prediction.
 */

function ChartCard({
  title,
  footer,
  children,
}: {
  title: string;
  /** Optional caption rendered INSIDE the card background, below the
   *  fixed-height chart area. Anything placed among `children` would
   *  land inside the `h-56` div alongside the 100%-height chart and
   *  overflow the card's rounded background. */
  footer?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <section className="bg-bg-surface rounded-lg p-3 sm:p-4">
      <h2 className="text-sm font-semibold text-text-primary mb-3">{title}</h2>
      <div className="h-56 sm:h-64">{children}</div>
      {footer ? <div className="mt-2">{footer}</div> : null}
    </section>
  );
}

function SummaryTile({
  label,
  value,
  testId,
  hint,
}: {
  label: string;
  value: string;
  testId: string;
  hint?: string;
}) {
  return (
    <div className="bg-bg-surface rounded-lg p-3 sm:p-4 flex flex-col gap-1">
      <span className="text-xs text-text-secondary">{label}</span>
      <span data-testid={testId} className="text-lg sm:text-xl font-bold text-text-primary">
        {value}
      </span>
      {hint ? <span className="text-[11px] text-text-secondary">{hint}</span> : null}
    </div>
  );
}

function ForecastApplyProgress() {
  return (
    <div
      role="status"
      aria-live="polite"
      aria-atomic="true"
      data-testid="forecast-plan-progress"
      className="pointer-events-none fixed inset-x-4 top-4 z-[100] mx-auto flex max-w-lg items-center gap-4 rounded-xl border-2 border-amber-200 bg-amber-400 px-5 py-4 text-slate-950 shadow-2xl ring-4 ring-black/20"
    >
      <span
        aria-hidden="true"
        className="h-7 w-7 shrink-0 animate-spin rounded-full border-4 border-current border-t-transparent"
      />
      <div>
        <div className="text-base font-bold">Applying changes to inverter…</div>
        <div className="mt-0.5 text-sm font-medium text-slate-800">
          Scheduling the charge slot. This can take several seconds; please keep this page open.
        </div>
      </div>
    </div>
  );
}

type ForecastChargeSlot = NonNullable<PlanResponse['apply']>['charge_slot'];
const FORECAST_CHARGE_CONFIRM_TIMEOUT_MS = 15_000;

function forecastChargeSlotMatchesReadback(
  snapshot: InverterSnapshot | null,
  desired: ForecastChargeSlot,
): boolean {
  const actual = snapshot?.charge_slots[desired.slot - 1];
  if (!actual) return false;
  return actual.enabled === desired.enabled
    && actual.start_hour === desired.start_hour
    && actual.start_minute === desired.start_minute
    && actual.end_hour === desired.end_hour
    && actual.end_minute === desired.end_minute
    && snapshot?.enable_charge === true;
}

/** Charge-slot writes are queued by the backend and applied by the poll loop.
 * Keep the Forecast progress state up until a newer snapshot confirms the
 * requested slot, matching the Control page's user-facing behaviour. */
function waitForForecastChargeReadback(
  desired: ForecastChargeSlot,
  snapshotBeforeApply: InverterSnapshot,
): Promise<void> {
  return new Promise((resolve) => {
    let unsubscribe = () => {};
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      window.clearTimeout(timeout);
      unsubscribe();
      resolve();
    };
    const check = () => {
      const current = useInverterStore.getState().snapshot;
      if (current !== snapshotBeforeApply && forecastChargeSlotMatchesReadback(current, desired)) {
        finish();
      }
    };
    const timeout = window.setTimeout(finish, FORECAST_CHARGE_CONFIRM_TIMEOUT_MS);
    unsubscribe = useInverterStore.subscribe(check);
    check();
  });
}

type ForecastGridOverlayProps = {
  offset?: { left: number; top: number; width: number; height: number };
  yAxisMap?: Record<string, { scale?: (value: number) => number }>;
  horizontalValues?: number[];
  stroke?: string;
  strokeWidth?: number;
  strokeDasharray?: string;
};

/**
 * Recharts paints Area fills after CartesianGrid. Drawing these explicit
 * horizontal lines from Customized keeps the grid visible over translucent
 * uncertainty bands, while the base CartesianGrid continues to provide the
 * vertical lines.
 */
function ForecastGridOverlay({
  offset,
  yAxisMap,
  horizontalValues = [],
  stroke = 'rgba(255,255,255,0.08)',
  strokeWidth = 1,
  strokeDasharray,
}: ForecastGridOverlayProps) {
  const yAxis = Object.values(yAxisMap ?? {})[0];
  if (!offset || !yAxis?.scale) return null;

  return (
    <g className="forecast-cartesian-grid-overlay" pointerEvents="none">
      {horizontalValues.map((value) => {
        const y = yAxis.scale?.(value);
        if (y == null) return null;
        return (
          <line
            key={value}
            x1={offset.left}
            y1={y}
            x2={offset.left + offset.width}
            y2={y}
            stroke={stroke}
            strokeWidth={strokeWidth}
            strokeDasharray={strokeDasharray}
          />
        );
      })}
    </g>
  );
}

export default function ForecastPage() {
  const [data, setData] = useState<ForecastData | null>(null);
  const [plan, setPlan] = useState<PlanResponse | null>(null);
  const [applyState, setApplyState] = useState<'idle' | 'sending' | 'done' | 'error'>('idle');
  const [applyError, setApplyError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [minSocPctInput, setMinSocPctInput] = useState<string>('20');
  const [minSocSaving, setMinSocSaving] = useState(false);
  const [minSocError, setMinSocError] = useState<string | null>(null);
  const [minSocPct, setMinSocPct] = useState<number>(20);
  const [planAutoRefresh, setPlanAutoRefresh] = useState<boolean>(false);
  const snapshot = useInverterStore((s) => s.snapshot);
  const gridLineWeight = useInverterStore((state) => state.gridLineWeight);
  const lastRefetchRef = useRef<number>(0);
  const lastTriggerRef = useRef<{
    soc: number | null;
    maxPowerW: number;
  }>({ soc: null, maxPowerW: 0 });

  const load = async () => {
    try {
      const [forecastRes, planRes, settingsRes] = await Promise.all([
        apiGet<{ ok: boolean; data: ForecastData }>('/api/forecast'),
        apiGet<{ ok: boolean; data: PlanResponse }>('/api/forecast/plan'),
        apiGet<{
          ok: boolean;
          data: { forecast_min_soc_pct?: number; forecast_plan_auto_refresh?: boolean } & Record<string, unknown>;
        }>('/api/settings'),
      ]);
      setData(forecastRes.data);
      setPlan(planRes.data);
      if (settingsRes.data.forecast_min_soc_pct != null) {
        const v = Math.round(settingsRes.data.forecast_min_soc_pct);
        setMinSocPctInput(String(v));
        setMinSocPct(v);
      }
      setPlanAutoRefresh(settingsRes.data.forecast_plan_auto_refresh ?? false);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load forecast');
    }
  };

  useEffect(() => {
    let cancelled = false;
    const wrap = async () => {
      if (cancelled) return;
      await load();
      if (!cancelled) setLoaded(true);
    };
    void wrap();
    return () => {
      cancelled = true;
    };
  }, []);

  // Refetch when the live snapshot changes meaningfully: SOC ± 1 pp,
  // the rate-cap changed, or the safety-net interval (30 s) has elapsed.
  useEffect(() => {
    const newSoc = snapshot?.soc ?? null;
    const newMaxPower = snapshot?.max_battery_power_w ?? 0;
    const last = lastTriggerRef.current;
    const now = Date.now();
    if (
      shouldRefetchForecast(
        last.soc,
        newSoc,
        lastRefetchRef.current,
        now,
        last.maxPowerW,
        newMaxPower,
      )
    ) {
      lastRefetchRef.current = now;
      lastTriggerRef.current = { soc: newSoc, maxPowerW: newMaxPower };
      void load();
    } else {
      // Track the latest snapshot state either way so the next call has
      // a current SOC for delta comparison.
      lastTriggerRef.current = { soc: newSoc, maxPowerW: newMaxPower };
    }
  }, [snapshot]);

  const saveMinSoc = async () => {
    const next = Number(minSocPctInput);
    if (!Number.isFinite(next)) {
      setMinSocError('Min SOC must be a number');
      return;
    }
    if (!(0 <= next && next <= 100)) {
      setMinSocError('Min SOC must be between 0 and 100');
      return;
    }
    setMinSocError(null);
    setMinSocSaving(true);
    try {
      await apiPost('/api/settings', { forecast_min_soc_pct: next });
      setMinSocPct(next);
      // Refetch — the planner sees the new floor on the next call.
      await load();
    } catch (e) {
      setMinSocError(e instanceof Error ? e.message : 'Save failed');
    } finally {
      setMinSocSaving(false);
    }
  };

  const handleApply = async () => {
    if (!plan || !plan.apply) return;
    const snapshotBeforeApply = useInverterStore.getState().snapshot;
    setApplyState('sending');
    setApplyError(null);
    try {
      await apiPost('/api/control/charge-slot', plan.apply.charge_slot);
      await apiPost('/api/control/timed-charge', plan.apply.timed_charge);
      if (snapshotBeforeApply) {
        await waitForForecastChargeReadback(plan.apply.charge_slot, snapshotBeforeApply);
      }
      setApplyState('done');
    } catch (e) {
      setApplyState('error');
      setApplyError(e instanceof Error ? e.message : 'Apply failed');
    }
  };

  if (error) {
    return (
      <div data-testid="forecast-error" className="bg-red-500/10 border border-red-500/30 rounded-lg p-4 text-sm text-text-primary">
        Couldn’t load the forecast: {error}
      </div>
    );
  }

  if (!loaded || !data) {
    return <div className="text-text-secondary text-sm animate-pulse">Loading forecast…</div>;
  }

  const statusMessages = forecastStatusMessages(data.status);
  const summary = tomorrowSummary(data);
  const solarChart = toSolarChartData(data.solar);
  // Charge window markers come first: the start marker's instant is also
  // where the "if charge enacted" line must part from the projection
  // (see insertChargeStartVertices below).
  const chargeMarkers =
    plan?.recommendation?.kind === 'charge'
      ? forecastChargeMarkers(data.generated_at, plan.recommendation.window)
      : [];
  const chargeStartTs =
    chargeMarkers.find((m) => m.kind === 'start')?.timestamp ?? null;

  // Anchor the projection at "now": the stored forward hours are full
  // future hours, so without the anchor the chart's left edge is the END
  // of the first hour — a full hour's drain below the live SOC (graph
  // starting at 48% while the battery sat at 59%). The anchor uses the
  // payload's generation time and the SOC the simulation actually started
  // from, so it always lines up with the series regardless of fetch
  // latency. Both lines are also re-labelled to state instants first: the
  // simulation records each hourly bucket's END state under the
  // bucket-start timestamp, which draws every change an hour early — a
  // 23:30–23:59 window landed entirely in the 23:00 bucket's point, so the
  // dashed "if charge enacted" line visibly climbed before the
  // charge-start marker.
  const anchoredProjection = data.battery
    ? anchorSeriesAtNow(
        relabelToStateInstants(data.battery.hours),
        data.generated_at,
        data.battery.start_soc_pct,
      )
    : [];
  // Merge the planner's "if we follow the recommendation" trajectory onto
  // the battery projection so the Forecast tab's Battery projection chart
  // can draw it as a second line — same x-axis (unix seconds). The
  // hypothetical series ends at the next cheap-period start; anything
  // after that must be based on a fresh recommendation. The vertex
  // insertion pins the dashed line to the projection until the window's
  // start instant, so its rise begins exactly at the green marker instead
  // of across the whole preceding hour.
  const withChargePrepared =
    plan?.recommendation?.kind === 'charge'
      ? // The what-if trajectory starts from the same live SOC at the
        // same generation time — anchor it too so both lines meet at
        // "now" and the divergence reads as the charge's effect.
        anchorSeriesAtNow(
          truncateSeriesAtNextChargeStart(
            relabelToStateInstants(plan.recommendation.with_charge_series),
            data.generated_at,
            plan.recommendation.window,
          ),
          data.generated_at,
          data.battery?.start_soc_pct ?? plan.recommendation.current_soc_pct,
        )
      : [];
  const { projection: projectionHours, withCharge: withChargeSeries } =
    chargeStartTs != null && withChargePrepared.length > 0
      ? insertChargeStartVertices(anchoredProjection, withChargePrepared, chargeStartTs)
      : { projection: anchoredProjection, withCharge: withChargePrepared };

  const batteryChart = data.battery
    ? toBatteryChartData({ ...data.battery, hours: projectionHours })
    : [];
  const batteryChartWithPlan = batteryChart.map((p) => {
    const match = withChargeSeries.find(([ts]) => ts === p.timestamp);
    return match ? { ...p, withCharge: match[1] } : { ...p, withCharge: undefined };
  });
  const hasWithCharge = withChargeSeries.length > 0;
  // The consumption profile is a typical-day hour-of-day series; tile it
  // onto the forward timestamps so all three charts share one x-axis —
  // same start (now), same horizon — instead of a midnight-anchored 24 h
  // view that can't be compared hour-for-hour against the projections.
  // Prefer the solar series' timestamps (the payload's forward axis),
  // fall back to the battery projection's, and generate a now-anchored
  // 72 h axis only when both are empty (weather-off degraded state).
  const forwardTimestamps = solarChart.length > 0
    ? solarChart.map((p) => p.timestamp)
    : batteryChart.map((p) => p.timestamp);
  const consumptionEmpty =
    data.consumption_weekday.length === 0 && data.consumption_weekend.length === 0;
  const consumptionTimestamps =
    forwardTimestamps.length > 0
      ? forwardTimestamps
      : forwardHourTimestamps(FORECAST_HORIZON_HOURS);
  const consumptionChart = consumptionEmpty
    ? []
    : toConsumptionChartData(
        data.consumption_weekday,
        data.consumption_weekend,
        consumptionTimestamps,
      );
  const forecastAxisDomain: [number, number] = [
    data.generated_at,
    data.generated_at + FORECAST_HORIZON_HOURS * 3600,
  ];
  const forecastAxisTicks = forecastXAxisTicks(
    forecastAxisDomain[0],
    forecastAxisDomain[1],
  );
  const solarYAxis = forecastYAxisScale(
    Math.max(0, ...solarChart.map((point) => point.high)),
  );
  const consumptionYAxis = forecastYAxisScale(
    Math.max(
      0,
      ...consumptionChart.flatMap((point) => [
        point.weekdayP75 ?? 0,
        point.weekendP75 ?? 0,
      ]),
    ),
  );

  return (
    <div className="flex flex-col gap-3 sm:gap-4 max-w-5xl">
      <div>
        <h1 className="text-lg font-bold text-text-primary">Forecast</h1>
        <p className="text-xs text-text-secondary">
          Predictions from Open-Meteo radiation calibrated against your own
          generation history.
        </p>
      </div>

      {statusMessages.length > 0 && (
        <div
          data-testid="forecast-status-banner"
          className="bg-amber-500/10 border border-amber-500/30 rounded-lg p-3 sm:p-4 flex flex-col gap-1"
        >
          {statusMessages.map((m) => (
            <p key={m} className="text-xs text-text-primary">
              {m}
            </p>
          ))}
        </div>
      )}

      {/* Planner floor — a setting, not a statistic, so it lives outside
          the "Tomorrow" tiles. The planner sizes the overnight charge so
          the battery never dips below this percentage across the forward
          window, not just at the end. Editing saves immediately and
          triggers a plan refetch. */}
      <section className="bg-bg-surface rounded-lg p-3 sm:p-4 flex flex-col gap-1">
        <div className="flex items-center gap-3">
          <label
            htmlFor="forecast-min-soc-input"
            title="The planner never lets the battery drop below this percentage over the next 72 h"
            className="text-text-primary text-sm font-sans font-medium"
          >
            Minimum battery level
          </label>
          <input
            id="forecast-min-soc-input"
            type="number"
            min={0}
            max={100}
            step={1}
            value={minSocPctInput}
            onChange={(e) => setMinSocPctInput(e.target.value)}
            onBlur={() => void saveMinSoc()}
            disabled={!data || minSocSaving}
            className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono w-24 border border-transparent focus:outline-none focus:border-accent disabled:opacity-50"
            data-testid="forecast-min-soc-input"
          />
          <span className="text-text-secondary text-xs font-sans">%</span>
          {minSocError && (
            <span
              data-testid="forecast-min-soc-error"
              className="text-xs text-red-400"
            >
              {minSocError}
            </span>
          )}
        </div>
        <p className="text-xs text-text-secondary font-sans">
          The planner sizes the next overnight charge so the battery stays
          above this percentage until the following cheap period. Lower means less grid
          import; higher keeps more backup in reserve.
        </p>
      </section>

      <div className="grid grid-cols-2 md:grid-cols-5 gap-2 sm:gap-3">
        <div className="col-span-2 md:col-span-5 text-xs font-semibold uppercase tracking-wide text-text-secondary">
          Tomorrow
        </div>

        <SummaryTile
          label="Solar prediction"
          value={formatEnergy(summary.solarKwh)}
          testId="forecast-solar-tomorrow"
          hint={
            data.performance_ratio != null
              ? `calibrated against ${data.performance_ratio_days ?? '?'} days of your history (last 2 weeks)`
              : 'preliminary — still calibrating'
          }
        />
        <SummaryTile
          label="House consumption"
          value={formatEnergy(summary.consumptionKwh)}
          testId="forecast-consumption-tomorrow"
          hint={`${data.consumption_days_observed} days observed`}
        />
        <SummaryTile
          label="Expected export"
          value={formatEnergy(
            plan?.recommendation?.kind === 'charge'
              ? plan.recommendation.export_tomorrow_with_charge_kwh
              : summary.surplusKwh,
          )}
          testId="forecast-surplus-tomorrow"
          hint={plan?.recommendation?.kind === 'charge' ? 'with charge plan' : undefined}
        />
        <SummaryTile
          label="Expected import"
          value={formatEnergy(
            plan?.recommendation?.kind === 'charge'
              ? plan.recommendation.import_tomorrow_with_charge_kwh
              : summary.importKwh,
          )}
          testId="forecast-import-tomorrow"
          hint={
            plan?.recommendation?.kind === 'charge'
              ? `incl. ${plan.recommendation.kwh.toFixed(1)} kWh charge`
              : undefined
          }
        />
        {summary.startSocPct != null && (
          <SummaryTile
            label="Battery now"
            value={`${Math.round(summary.startSocPct)}%`}
            testId="forecast-start-soc"
          />
        )}
      </div>

      {plan && plan.recommendation && (
        <section
          data-testid="forecast-plan"
          className="bg-bg-surface rounded-lg p-3 sm:p-4 flex flex-col gap-2"
          aria-live="polite"
        >
          <div className="flex items-baseline justify-between gap-2">
            <h2 className="text-sm font-semibold text-text-primary">
              {forecastPlanTitle(plan.recommendation)}
            </h2>
            {plan.recommendation.kind !== 'no_plan' && (
              <span
                data-testid="forecast-plan-current-soc"
                className="text-[11px] text-text-secondary"
                title="Live snapshot SOC at the time the plan was computed"
              >
                Current SOC {Math.round(plan.recommendation.current_soc_pct)}%
              </span>
            )}
          </div>
          {plan.recommendation.kind === 'charge' && (
            <p className="text-xs text-text-secondary">
              <span data-testid="forecast-plan-kwh">
                Charge {plan.recommendation.kwh.toFixed(1)} kWh in the off-peak
                window ({plan.recommendation.window.start}–
                {plan.recommendation.window.end},{' '}
                {(plan.recommendation.window.rate * 100).toFixed(1)}p)
              </span>{' '}
              to clear{' '}
              <span data-testid="forecast-plan-trough">
                {Math.round(plan.recommendation.observed_min_soc_pct)}%
              </span>{' '}
              trough →{' '}
              <span data-testid="forecast-plan-after-min">
                {plan.recommendation.after_min_soc_pct.toFixed(0)}%
              </span>{' '}
              (charge rate 100%) — about £
              {(plan.recommendation.kwh * plan.recommendation.window.rate).toFixed(2)} of
              grid import.
            </p>
          )}
          {plan.recommendation.kind === 'no_charge_needed' && (
            <p className="text-xs text-text-secondary">
              Projected low{' '}
              <span data-testid="forecast-plan-observed-min">
                {Math.round(plan.recommendation.observed_min_soc_pct)}%
              </span>{' '}
              — your {Math.round(plan.recommendation.min_soc_pct)}% floor is held
              on solar alone.
            </p>
          )}
          {plan.recommendation.kind === 'no_plan' && (
            <p className="text-xs text-text-secondary">{plan.recommendation.reason}</p>
          )}
          {plan.apply && plan.recommendation.kind === 'charge' && (
            <div className="flex items-center gap-2">
              <button
                type="button"
                data-testid="forecast-plan-apply"
                aria-busy={applyState === 'sending'}
                disabled={applyState === 'sending' || applyState === 'done'}
                onClick={() => void handleApply()}
                className="px-3 py-1.5 rounded-md bg-accent text-accent-on text-sm font-medium hover:opacity-90 disabled:opacity-50"
              >
                {applyState === 'idle' && 'Apply — schedule charge slot'}
                {applyState === 'sending' && 'Applying…'}
                {applyState === 'done' && 'Applied ✓'}
                {applyState === 'error' && 'Retry Apply'}
              </button>
              {applyError && (
                <span data-testid="forecast-plan-error" role="alert" className="text-xs text-red-400">
                  {applyError}
                </span>
              )}
            </div>
          )}
          {plan.recommendation.kind === 'charge' && (
            <p
              data-testid="forecast-plan-cycle-note"
              className="text-[11px] text-text-secondary"
            >
              This plan covers the next charge cycle only.{' '}
              {planAutoRefresh
                ? 'Auto-refresh re-sizes charge slot 1 from the live battery shortly before each cheap period — no daily re-apply needed.'
                : 'The inverter repeats the applied slot every night, so re-apply tomorrow to keep it in step with the battery.'}
            </p>
          )}
          {applyState === 'sending' && <ForecastApplyProgress />}
        </section>
      )}

      <ChartCard title="Solar forecast (next 72 h)">
        {solarChart.length === 0 ? (
          <div className="h-full flex items-center justify-center text-xs text-text-secondary">
            No forecast data yet.
          </div>
        ) : (
          <ResponsiveContainer width="100%" height="100%">
            <AreaChart data={solarChart}>
              <CartesianGrid
                {...getHistoryChartGridProps(gridLineWeight)}
                horizontal={false}
                horizontalValues={solarYAxis.ticks}
              />
              <Customized
                component={ForecastGridOverlay}
                {...getHistoryChartGridProps(gridLineWeight)}
                horizontalValues={solarYAxis.ticks}
              />
              <XAxis
                dataKey="timestamp"
                stroke="#94a3b8"
                fontSize={10}
                {...timeAxisProps}
                domain={forecastAxisDomain}
                ticks={forecastAxisTicks}
                tickFormatter={formatForecastXAxisTick}
                interval={0}
                minTickGap={0}
                angle={-30}
                textAnchor="end"
                height={42}
              />
              <YAxis
                stroke="#94a3b8"
                fontSize={10}
                unit="kWh"
                width={48}
                domain={[0, solarYAxis.max]}
                ticks={solarYAxis.ticks}
                allowDataOverflow
              />
              <Tooltip
                {...tooltipStyleProps}
                labelFormatter={(ts) => formatForecastXAxisTick(Number(ts))}
                formatter={(value: number | string) => `${Number(value).toFixed(2)} kWh`}
              />
              <Area
                type="monotone"
                dataKey="kwh"
                stroke="#f59e0b"
                fill="#f59e0b"
                fillOpacity={0.25}
                dot={false}
                name="Prediction"
              />
              <Area
                type="monotone"
                dataKey="low"
                stroke="none"
                fill="transparent"
                name="Low"
                legendType="none"
              />
              <Area
                type="monotone"
                dataKey="high"
                stroke="none"
                fill="transparent"
                name="High"
                legendType="none"
              />
            </AreaChart>
          </ResponsiveContainer>
        )}
      </ChartCard>

      <ChartCard title="Consumption profile (next 72 h)">
        {consumptionEmpty || consumptionChart.length === 0 ? (
          <div className="h-full flex items-center justify-center text-xs text-text-secondary">
            Not enough history yet.
          </div>
        ) : (
          <ResponsiveContainer width="100%" height="100%">
            <ComposedChart data={consumptionChart}>
              <CartesianGrid
                {...getHistoryChartGridProps(gridLineWeight)}
                horizontal={false}
                horizontalValues={consumptionYAxis.ticks}
              />
              <Customized
                component={ForecastGridOverlay}
                {...getHistoryChartGridProps(gridLineWeight)}
                horizontalValues={consumptionYAxis.ticks}
              />
              <XAxis
                dataKey="timestamp"
                stroke="#94a3b8"
                fontSize={10}
                {...timeAxisProps}
                domain={forecastAxisDomain}
                ticks={forecastAxisTicks}
                tickFormatter={formatForecastXAxisTick}
                interval={0}
                minTickGap={0}
                angle={-30}
                textAnchor="end"
                height={42}
              />
              <YAxis
                stroke="#94a3b8"
                fontSize={10}
                unit="kWh"
                width={48}
                domain={[0, consumptionYAxis.max]}
                ticks={consumptionYAxis.ticks}
                allowDataOverflow
              />
              <Tooltip
                {...tooltipStyleProps}
                labelFormatter={(ts) => formatForecastXAxisTick(Number(ts))}
                formatter={(value: number | string) => `${Number(value).toFixed(2)} kWh`}
              />
              <Area
                type="monotone"
                dataKey="weekdayP25"
                stroke="none"
                fill="#38bdf8"
                fillOpacity={0.15}
                name="Weekday low estimate"
                legendType="none"
              />
              <Area
                type="monotone"
                dataKey="weekdayP75"
                stroke="none"
                fill="#38bdf8"
                fillOpacity={0.15}
                name="Weekday high estimate"
                legendType="none"
              />
              <Area
                type="monotone"
                dataKey="weekendP25"
                stroke="none"
                fill="#a78bfa"
                fillOpacity={0.15}
                name="Weekend low estimate"
                legendType="none"
              />
              <Area
                type="monotone"
                dataKey="weekendP75"
                stroke="none"
                fill="#a78bfa"
                fillOpacity={0.15}
                name="Weekend high estimate"
                legendType="none"
              />
              <Line
                type="monotone"
                dataKey="weekday"
                stroke="#38bdf8"
                strokeWidth={2}
                dot={false}
                name="Weekday median"
              />
              <Line
                type="monotone"
                dataKey="weekend"
                stroke="#a78bfa"
                strokeWidth={2}
                dot={false}
                name="Weekend median"
              />
              <Legend />
            </ComposedChart>
          </ResponsiveContainer>
        )}
      </ChartCard>

      <ChartCard
        title="Battery projection (next 72 h)"
        footer={
          hasWithCharge && plan?.recommendation?.kind === 'charge' ? (
            <p className="text-[10px] text-text-secondary/70 font-sans leading-snug">
              <span
                aria-hidden
                className="inline-block w-3 h-px align-middle mr-1"
                style={{ borderTop: '2px dashed #60a5fa' }}
              />
              SOC if overnight charge enacted —
              Tomorrow {plan.recommendation.window.start}–{plan.recommendation.window.end},
              {' '}{plan.recommendation.kwh.toFixed(1)} kWh.
            </p>
          ) : undefined
        }
      >
        {batteryChart.length === 0 ? (
          <div className="h-full flex items-center justify-center text-xs text-text-secondary">
            No battery projection — connect to the inverter.
          </div>
        ) : (
          <ResponsiveContainer width="100%" height="100%">
            <LineChart data={batteryChartWithPlan}>
              <CartesianGrid {...getHistoryChartGridProps(gridLineWeight)} />
              {chargeMarkers.map((marker) => {
                const colour = marker.kind === 'start' ? '#34d399' : '#fbbf24';
                return (
                  <ReferenceLine
                    key={`${marker.kind}-${marker.timestamp}`}
                    x={marker.timestamp}
                    stroke={colour}
                    strokeDasharray="4 3"
                  />
                );
              })}
              <XAxis
                dataKey="timestamp"
                stroke="#94a3b8"
                fontSize={10}
                {...timeAxisProps}
                domain={forecastAxisDomain}
                ticks={forecastAxisTicks}
                tickFormatter={formatForecastXAxisTick}
                interval={0}
                minTickGap={0}
                angle={-30}
                textAnchor="end"
                height={42}
              />
              <YAxis stroke="#94a3b8" fontSize={10} domain={[0, 100]} unit="%" width={44} />
              <Tooltip
                {...tooltipStyleProps}
                labelFormatter={(ts) => formatForecastXAxisTick(Number(ts))}
                formatter={(value: number | string, name: string) =>
                  Number.isFinite(Number(value)) ? [`${Number(value).toFixed(0)}%`, name] : ['—', name]
                }
              />
              {data.battery && (
                <>
                  <ReferenceLine
                    y={data.battery.reserve_soc_pct}
                    stroke="#f87171"
                    strokeDasharray="4 4"
                    label={{ value: 'reserve', fill: '#f87171', fontSize: 10 }}
                  />
                  <ReferenceLine
                    y={minSocPct}
                    stroke="#fbbf24"
                    strokeDasharray="6 3"
                    label={{ value: `min ${minSocPct}%`, fill: '#fbbf24', fontSize: 10 }}
                  />
                </>
              )}
              <Line
                type="monotone"
                dataKey="soc"
                stroke="#34d399"
                strokeWidth={2}
                dot={false}
                name="SOC"
              />
              {hasWithCharge && (
                <Line
                  type="monotone"
                  dataKey="withCharge"
                  stroke="#60a5fa"
                  strokeWidth={2}
                  strokeDasharray="6 4"
                  dot={false}
                  name="If charge enacted"
                  connectNulls
                />
              )}
              {chargeMarkers.length > 0 && (
                <Legend
                  payload={[
                    { value: 'Charge start', type: 'line', color: '#34d399' },
                    { value: 'Charge end', type: 'line', color: '#fbbf24' },
                  ]}
                />
              )}
            </LineChart>
          </ResponsiveContainer>
        )}
      </ChartCard>

      <p className="text-[11px] text-text-secondary">
        Forecast data by Open-Meteo.com (CC-BY 4.0). Solar predictions are
        Open-Meteo radiation scaled by a performance ratio fitted from your
        own generation history (the last 2 weeks). Solar band is a ±20%
        model estimate until measured accuracy statistics accumulate.
      </p>
    </div>
  );
}
