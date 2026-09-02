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
import { openExternal } from '../lib/openExternal';
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
  forecastExportTitle,
  forecastPlanTitle,
  forecastStatusMessages,
  forecastXAxisTicks,
  forecastYAxisScale,
  forwardHourTimestamps,
  parseLeadMinutes,
  planAutoApplyTriggerLabel,
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
  legend,
  footer,
  children,
}: {
  title: string;
  /** Toggleable line legend rendered directly under the chart area —
   *  closer to the graph than the footer captions. */
  legend?: React.ReactNode;
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
      {legend ? <div className="mt-1.5">{legend}</div> : null}
      {footer ? <div className="mt-2">{footer}</div> : null}
    </section>
  );
}

/** Toggleable line legend for the Battery projection chart: activating a
 *  description hides/shows that line. Plain buttons rather than
 *  recharts' Legend so every entry is a real, keyboard-reachable control
 *  with an aria-pressed state. */
function LineToggleLegend({
  lines,
  hidden,
  onToggle,
}: {
  lines: { key: string; label: string; colour: string; style?: 'solid' | 'dashed' | 'dotted' }[];
  hidden: ReadonlySet<string>;
  onToggle: (key: string) => void;
}) {
  return (
    <div
      data-testid="forecast-line-legend"
      role="group"
      aria-label="Projection lines — activate to show or hide a line"
      className="flex flex-wrap items-center justify-center gap-x-4 gap-y-1"
    >
      {lines.map((line) => {
        const visible = !hidden.has(line.key);
        return (
          <button
            key={line.key}
            type="button"
            onClick={() => onToggle(line.key)}
            aria-pressed={visible}
            className={`inline-flex cursor-pointer items-center gap-1.5 text-xs font-sans ${
              visible ? 'text-text-primary' : 'text-text-secondary/50 line-through'
            }`}
          >
            <span
              aria-hidden
              className="inline-block w-4 align-middle"
              style={{
                borderTop: `2px ${line.style ?? 'solid'} ${line.colour}`,
                opacity: visible ? 1 : 0.35,
              }}
            />
            {line.label}
          </button>
        );
      })}
    </div>
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

/** The Forecast page's three chart tabs, in tab-strip (and keyboard) order. */
const CHART_TABS = [
  ['battery', 'Battery'],
  ['solar', 'Solar'],
  ['consumption', 'Home use'],
] as const;
type ForecastChart = (typeof CHART_TABS)[number][0];

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
  const [planAutoApply, setPlanAutoApply] = useState<boolean>(false);
  const [planAutoApplyLeadInput, setPlanAutoApplyLeadInput] = useState<string>('30');
  // True while the lead field holds edits that haven't been saved yet —
  // the blur handler only saves then, so merely focusing the field can't
  // fire a spurious request.
  const [planAutoApplyLeadDirty, setPlanAutoApplyLeadDirty] = useState(false);
  const [planAutoApplySaving, setPlanAutoApplySaving] = useState(false);
  const [planAutoApplyError, setPlanAutoApplyError] = useState<string | null>(null);
  // Battery projection lines the user has hidden via the legend (issue
  // #297): keys are 'soc' | 'withCharge' | 'withCurrent'. Session-scoped
  // on purpose — a fresh page starts with every line visible.
  const [hiddenLines, setHiddenLines] = useState<ReadonlySet<string>>(new Set());
  const toggleLine = (key: string) =>
    setHiddenLines((prev) => {
      const next = new Set(prev);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  // One merged control owns both planner flags: auto-apply is the
  // configurable, notifying successor of the older fixed-lead auto-refresh
  // — they do the same job (keep charge slot 1 in step before the cheap
  // window), so showing two toggles read as duplicated features. The
  // control reads on when either flag is set (legacy settings may have
  // only auto-refresh), and every save writes both: toggling off clears
  // both, editing the lead upgrades legacy auto-refresh to auto-apply.
  const planAutoHandling = planAutoApply || planAutoRefresh;
  // Forecast battery efficiencies (issue #283), edited as whole percents
  // and persisted as 0–1 ratios. They save together on blur — the planner
  // needs both to be sane, so one invalid field blocks the pair.
  const [chargeEffInput, setChargeEffInput] = useState<string>('90');
  const [dischargeEffInput, setDischargeEffInput] = useState<string>('95');
  const [chargeEffPct, setChargeEffPct] = useState<number>(90);
  const [dischargeEffPct, setDischargeEffPct] = useState<number>(95);
  const [effSaving, setEffSaving] = useState(false);
  const [effError, setEffError] = useState<string | null>(null);
  const [activeChart, setActiveChart] = useState<ForecastChart>('battery');
  // Roving-tabindex refs for the chart tab strip: after an arrow-key
  // selection the newly active tab must receive focus (WAI-ARIA tabs).
  const chartTabRefs = useRef<Record<ForecastChart, HTMLButtonElement | null>>({
    battery: null,
    solar: null,
    consumption: null,
  });
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
          data: {
            forecast_min_soc_pct?: number;
            forecast_plan_auto_refresh?: boolean;
            forecast_plan_auto_apply_enabled?: boolean;
            forecast_plan_auto_apply_lead_minutes?: number;
            forecast_charge_efficiency?: number;
            forecast_discharge_efficiency?: number;
          } & Record<string, unknown>;
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
      setChargeEffInput(
        settingsRes.data.forecast_charge_efficiency != null
          ? String(Math.round(settingsRes.data.forecast_charge_efficiency * 100))
          : '90',
      );
      setDischargeEffInput(
        settingsRes.data.forecast_discharge_efficiency != null
          ? String(Math.round(settingsRes.data.forecast_discharge_efficiency * 100))
          : '95',
      );
      setPlanAutoApply(settingsRes.data.forecast_plan_auto_apply_enabled ?? false);
      setPlanAutoApplyLeadInput(
        String(settingsRes.data.forecast_plan_auto_apply_lead_minutes ?? 30),
      );
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

  // Persist the merged automatic-handling control. The next enabled state
  // is explicit — the checkbox toggle and the lead field's blur handler
  // want different values, so neither may infer it (inferring the flip
  // here is what used to silently disable auto-apply whenever the lead
  // field blurred). Both planner flags are written together: auto-apply
  // supersedes auto-refresh, so it is always turned off here — never left
  // on behind the UI's back — and turning the control off clears the
  // legacy flag too.
  const savePlanAutoApply = async (nextEnabled: boolean) => {
    // An emptied (or half-typed) field must not save as 0 — Number('') is
    // 0, which would silently move the trigger to the window's own start.
    // parseLeadMinutes rejects anything that isn't plain digits; only the
    // upper bound still needs checking here.
    const lead = parseLeadMinutes(planAutoApplyLeadInput);
    if (lead == null || lead > 120) {
      setPlanAutoApplyError('Lead time must be a whole number between 0 and 120');
      return;
    }
    setPlanAutoApplyError(null);
    setPlanAutoApplySaving(true);
    try {
      await apiPost('/api/settings', {
        forecast_plan_auto_apply_enabled: nextEnabled,
        forecast_plan_auto_apply_lead_minutes: lead,
        forecast_plan_auto_refresh: false,
      });
      setPlanAutoApply(nextEnabled);
      setPlanAutoRefresh(false);
      setPlanAutoApplyLeadDirty(false);
      // Refetch so the plan note and trigger time reflect the new state.
      await load();
    } catch (e) {
      setPlanAutoApplyError(e instanceof Error ? e.message : 'Save failed');
    } finally {
      setPlanAutoApplySaving(false);
    }
  };

  // Persist both battery efficiencies together (the planner needs the
  // pair consistent), parsed as percents → 0–1 ratios. Out-of-range values
  // are rejected client-side (mirrors the backend's 0.5–1.0 validation) so
  // the save never round-trips a nonsense ratio.
  const saveEfficiencies = async () => {
    const charge = Number(chargeEffInput);
    const discharge = Number(dischargeEffInput);
    const valid = (n: number) => Number.isInteger(n) && n >= 50 && n <= 100;
    if (!valid(charge) || !valid(discharge)) {
      setEffError('Battery efficiencies must be whole numbers between 50 and 100');
      return;
    }
    setEffError(null);
    setEffSaving(true);
    try {
      await apiPost('/api/settings', {
        forecast_charge_efficiency: charge / 100,
        forecast_discharge_efficiency: discharge / 100,
      });
      setChargeEffPct(charge);
      setDischargeEffPct(discharge);
      // Refetch — the planner sees the new efficiencies on the next call.
      await load();
    } catch (e) {
      setEffError(e instanceof Error ? e.message : 'Save failed');
    } finally {
      setEffSaving(false);
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
  // Third line (issue #297): the projection with the inverter's CURRENT
  // schedule applied — enabled charge/discharge slots as configured — so
  // "leave everything as it is" has a line of its own between the Eco
  // projection and the plan's hypothetical charge. Same anchor treatment
  // as the other two series so all three meet at "now".
  const withScheduleSeries =
    data.battery?.with_current_schedule && data.battery.with_current_schedule.length > 0
      ? anchorSeriesAtNow(
          relabelToStateInstants(data.battery.with_current_schedule),
          data.generated_at,
          data.battery.start_soc_pct,
        )
      : [];
  const hasWithSchedule = withScheduleSeries.length > 0;
  const batteryChartData = batteryChartWithPlan.map((p) => {
    const match = hasWithSchedule
      ? withScheduleSeries.find(([ts]) => ts === p.timestamp)
      : undefined;
    return match ? { ...p, withCurrent: match[1] } : { ...p, withCurrent: undefined };
  });
  const lineVisible = (key: string) => !hiddenLines.has(key);
  // Legend descriptions: SOC always (the no-grid-charging baseline); the
  // what-if lines only when data exists for them.
  const batteryLegendLines = [
    { key: 'soc', label: 'If solar only', colour: '#34d399', style: 'solid' as const },
    ...(hasWithCharge
      ? [
          {
            key: 'withCharge',
            label: 'If charge enacted',
            colour: '#60a5fa',
            style: 'dashed' as const,
          },
        ]
      : []),
    ...(hasWithSchedule
      ? [
          {
            key: 'withCurrent',
            label: 'With current schedule',
            colour: '#f472b6',
            style: 'dotted' as const,
          },
        ]
      : []),
  ];
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
        <p
          className="text-xs text-text-secondary"
          data-testid="forecast-attribution"
        >
          Predictions from{' '}
          <button
            type="button"
            onClick={() => void openExternal('https://open-meteo.com/')}
            className="text-accent underline hover:opacity-80 inline"
          >
            Open-Meteo.com
          </button>{' '}
          radiation calibrated against your own generation history — licensed
          under CC BY 4.0.
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

      <div className="order-2 grid grid-cols-2 md:grid-cols-4 gap-2 sm:gap-3">
        <div className="col-span-2 md:col-span-4 text-xs font-semibold uppercase tracking-wide text-text-secondary">
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
      </div>

      {plan && plan.recommendation && (
        <section
          data-testid="forecast-plan"
          className="order-1 bg-bg-surface rounded-xl border border-accent/20 p-4 sm:p-5 flex flex-col gap-3 shadow-sm"
          aria-live="polite"
        >
          <div className="flex items-start justify-between gap-3">
            <div>
              <p className="text-[11px] font-semibold uppercase tracking-wide text-accent">Today’s recommendation</p>
              <h2 className="mt-1 text-base sm:text-lg font-semibold text-text-primary">
              {forecastPlanTitle(plan.recommendation)}
              </h2>
            </div>
            {summary.startSocPct != null && (
              <span
                data-testid="forecast-plan-current-soc"
                className="shrink-0 rounded-full bg-bg-elevated px-3 py-1.5 text-xs text-text-secondary"
                title="Current battery state of charge"
              >
                Battery now{' '}
                {Math.round(
                  plan.recommendation.kind === 'no_plan'
                    ? summary.startSocPct
                    : plan.recommendation.current_soc_pct,
                )}%
              </span>
            )}
          </div>
          {plan.recommendation.kind === 'charge' && (
            <div className="grid grid-cols-2 sm:grid-cols-4 gap-2">
              <div className="rounded-lg bg-bg-elevated p-3">
                <div className="text-[11px] text-text-secondary">Charge</div>
                <div data-testid="forecast-plan-kwh" className="mt-0.5 text-sm font-semibold text-text-primary">{plan.recommendation.kwh.toFixed(1)} kWh</div>
              </div>
              <div className="rounded-lg bg-bg-elevated p-3">
                <div className="text-[11px] text-text-secondary">Time</div>
                <div className="mt-0.5 text-sm font-semibold text-text-primary">{plan.recommendation.window.start}–{plan.recommendation.window.end}</div>
              </div>
              <div className="rounded-lg bg-bg-elevated p-3">
                <div className="text-[11px] text-text-secondary">Lowest battery</div>
                <div className="mt-0.5 text-sm font-semibold text-text-primary"><span data-testid="forecast-plan-trough">{Math.round(plan.recommendation.observed_min_soc_pct)}%</span> → <span data-testid="forecast-plan-after-min">{plan.recommendation.after_min_soc_pct.toFixed(0)}%</span></div>
              </div>
              <div className="rounded-lg bg-bg-elevated p-3">
                <div className="text-[11px] text-text-secondary">Estimated cost</div>
                <div className="mt-0.5 text-sm font-semibold text-text-primary">£{(plan.recommendation.kwh * plan.recommendation.window.rate).toFixed(2)}</div>
              </div>
            </div>
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
          <div className="flex items-center justify-between gap-3 border-t border-white/10 pt-3">
            <div>
              <div className="text-xs font-medium text-text-primary">Automatic planning</div>
              <div className="mt-0.5 text-[11px] text-text-secondary" data-testid="forecast-auto-apply-summary">
                {planAutoHandling
                  ? planAutoApply && plan.recommendation.kind === 'charge'
                    ? `On · applies at ${planAutoApplyTriggerLabel(
                        plan.recommendation.window.start,
                        // Same strict parser as the note below — Number('')
                        // is 0, which would read an emptied field as a
                        // zero lead and name the window's own start.
                        parseLeadMinutes(planAutoApplyLeadInput) ?? Number.NaN,
                      ) ?? 'the configured time'}`
                    : 'On · keeps the next charge slot up to date'
                  : 'Off · use the Apply button for each new plan'}
              </div>
            </div>
            <span className={`rounded-full px-2.5 py-1 text-[11px] font-semibold ${planAutoHandling ? 'bg-emerald-500/15 text-emerald-300' : 'bg-bg-elevated text-text-secondary'}`}>
              {planAutoHandling ? 'On' : 'Off'}
            </span>
          </div>
          <details className="group rounded-lg bg-bg-elevated/50">
            <summary className="cursor-pointer list-none px-3 py-2.5 flex items-center justify-between gap-3 text-xs font-medium text-text-primary">
              <span>Plan settings</span>
              <span aria-hidden className="text-text-secondary transition-transform group-open:rotate-180">⌄</span>
            </summary>
            <div className="border-t border-white/10 px-3 pb-3">
          {/* Planner floor — the planner sizes the overnight charge so the
              battery never dips below this percentage across the forward
              window. Editing saves immediately and triggers a plan refetch. */}
          <div className="pt-3 flex flex-col gap-1">
            <div className="flex items-center gap-3 flex-wrap">
              <label
                htmlFor="forecast-min-soc-input"
                title="The planner never lets the battery drop below this percentage over the next 72 h"
                className="text-text-primary text-xs font-sans font-medium"
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
                className="bg-bg-surface text-text-primary rounded-lg px-3 py-1.5 text-sm font-mono w-20 border border-transparent focus:outline-none focus:border-accent disabled:opacity-50"
                data-testid="forecast-min-soc-input"
              />
              <span className="text-text-secondary text-xs font-sans">%</span>
              {minSocError && (
                <span data-testid="forecast-min-soc-error" className="text-xs text-red-400">
                  {minSocError}
                </span>
              )}
            </div>
            <p className="text-[11px] text-text-secondary font-sans">
              The planner sizes the next overnight charge so the battery stays above this
              level until the following cheap period. Lower means less grid import; higher
              keeps more backup in reserve.
            </p>
          </div>
          {/* Automatic plan handling — one control for keeping charge slot 1
              in step with the plan: the backend applies the calculated plan
              (or clears the slot when no charge is needed) the configured
              minutes before the cheap tariff window and notifies via the
              alert channels. The older fixed-lead auto-refresh flag is
              folded in here rather than shown as a second toggle: turning
              the control on or editing the lead upgrades it to auto-apply,
              turning the control off clears both. */}
          <div className="border-t border-white/10 pt-2 mt-1 flex flex-col gap-1">
            <div className="flex items-center gap-3 flex-wrap">
              <label
                htmlFor="forecast-auto-apply-toggle"
                title="Applies the calculated charging plan automatically before the cheap tariff window and notifies you"
                className="text-text-primary text-xs font-sans font-medium"
              >
                Apply charging plan automatically
              </label>
              <input
                id="forecast-auto-apply-toggle"
                type="checkbox"
                checked={planAutoHandling}
                onChange={() => void savePlanAutoApply(!planAutoHandling)}
                disabled={planAutoApplySaving}
                className="h-5 w-5 accent-accent"
                data-testid="forecast-auto-apply-toggle"
              />
              <span className="text-text-secondary text-xs font-sans">every day</span>
              <input
                id="forecast-auto-apply-lead"
                type="number"
                min={0}
                max={120}
                step={1}
                value={planAutoApplyLeadInput}
                onChange={(e) => {
                  setPlanAutoApplyLeadInput(e.target.value);
                  setPlanAutoApplyLeadDirty(true);
                }}
                onBlur={() => {
                  // Save an edited lead time without touching the enabled
                  // state — this must never toggle the trigger off. For a
                  // legacy auto-refresh-only setting this also upgrades it
                  // to auto-apply.
                  if (planAutoHandling && planAutoApplyLeadDirty) {
                    void savePlanAutoApply(true);
                  }
                }}
                disabled={planAutoApplySaving}
                className="bg-bg-elevated text-text-primary rounded-lg px-3 py-1.5 text-sm font-mono w-20 border border-transparent focus:outline-none focus:border-accent disabled:opacity-50"
                data-testid="forecast-auto-apply-lead"
              />
              <span className="text-text-secondary text-xs font-sans">
                min before the cheap window
              </span>
              {planAutoApplyError && (
                <span
                  data-testid="forecast-auto-apply-error"
                  role="alert"
                  className="text-xs text-red-400"
                >
                  {planAutoApplyError}
                </span>
              )}
            </div>
            <p className="text-[11px] text-text-secondary font-sans">
              {planAutoHandling ? (
                <span data-testid="forecast-auto-apply-note">
                  {planAutoApply ? (
                    (() => {
                      const rec = plan.recommendation;
                      const windowStart =
                        rec.kind === 'charge' ? rec.window.start : null;
                      // Invalid input (empty or half-typed) → NaN → a null
                      // trigger label → the lead-agnostic fallback note
                      // (see parseLeadMinutes).
                      const lead =
                        parseLeadMinutes(planAutoApplyLeadInput) ?? Number.NaN;
                      const trigger =
                        windowStart != null
                          ? planAutoApplyTriggerLabel(windowStart, lead)
                          : null;
                      return trigger && windowStart
                        ? `On — today's plan applies itself at ${trigger}, ${planAutoApplyLeadInput} min before the ${windowStart} window. You'll get a notification when it runs.`
                        : 'On — the plan applies itself before each cheap window and you\'ll get a notification when it runs.';
                    })()
                  ) : (
                    'Auto-refresh keeps this plan’s charge slot in step from the live battery before each cheap window — no daily re-apply needed. Editing the lead time upgrades this to auto-apply.'
                  )}
                </span>
              ) : (
                <span data-testid="forecast-auto-apply-note">
                  Off — apply the plan yourself with the button above. Switch
                  on to have the app apply it automatically before each cheap
                  window and send you a notification.
                </span>
              )}
            </p>
          </div>
          {/* Battery efficiencies — planner inputs (issue #283). Moved here
              from the Settings page so all Forecast settings live on the
              Forecast page. They save together on blur. */}
          <div className="border-t border-white/10 pt-2 mt-1 flex flex-col gap-1">
            <div className="flex items-center gap-3 flex-wrap">
              <label
                htmlFor="forecast-charge-eff"
                className="text-text-primary text-xs font-sans font-medium"
              >
                Battery charge efficiency
              </label>
              <input
                id="forecast-charge-eff"
                type="number"
                min={50}
                max={100}
                step={1}
                value={chargeEffInput}
                onChange={(e) => setChargeEffInput(e.target.value)}
                onBlur={() => {
                  if (
                    Number(chargeEffInput) !== chargeEffPct ||
                    Number(dischargeEffInput) !== dischargeEffPct
                  ) {
                    void saveEfficiencies();
                  }
                }}
                disabled={effSaving}
                className="bg-bg-elevated text-text-primary rounded-lg px-3 py-1.5 text-sm font-mono w-20 border border-transparent focus:outline-none focus:border-accent disabled:opacity-50"
                data-testid="forecast-charge-eff-input"
              />
              <label
                htmlFor="forecast-discharge-eff"
                className="text-text-primary text-xs font-sans font-medium"
              >
                Battery discharge efficiency
              </label>
              <input
                id="forecast-discharge-eff"
                type="number"
                min={50}
                max={100}
                step={1}
                value={dischargeEffInput}
                onChange={(e) => setDischargeEffInput(e.target.value)}
                onBlur={() => {
                  if (
                    Number(chargeEffInput) !== chargeEffPct ||
                    Number(dischargeEffInput) !== dischargeEffPct
                  ) {
                    void saveEfficiencies();
                  }
                }}
                disabled={effSaving}
                className="bg-bg-elevated text-text-primary rounded-lg px-3 py-1.5 text-sm font-mono w-20 border border-transparent focus:outline-none focus:border-accent disabled:opacity-50"
                data-testid="forecast-discharge-eff-input"
              />
              <span className="text-text-secondary text-xs font-sans">%</span>
              {effError && (
                <span
                  data-testid="forecast-eff-error"
                  role="alert"
                  className="text-xs text-red-400"
                >
                  {effError}
                </span>
              )}
            </div>
            <p className="text-[11px] text-text-secondary font-sans">
              Used by the battery projection and the plan’s duration maths.
              Default 90 / 95 — round-trip ≈ 85.5%.
            </p>
          </div>
            </div>
          </details>
          {plan.recommendation.kind === 'charge' && (
            <p
              data-testid="forecast-plan-cycle-note"
              className="text-[11px] text-text-secondary"
            >
              This plan covers the next charge cycle only.{' '}
              {planAutoApply
                ? 'Auto-apply applies this plan automatically before each cheap window and notifies you — no daily re-apply needed.'
                : planAutoRefresh
                ? 'Auto-refresh re-sizes charge slot 1 from the live battery shortly before each cheap period — no daily re-apply needed.'
                : 'The inverter repeats the applied slot every night, so re-apply tomorrow to keep it in step with the battery.'}
            </p>
          )}
          {applyState === 'sending' && <ForecastApplyProgress />}
        </section>
      )}

      {plan?.export?.kind === 'export' && (
        <section
          data-testid="forecast-export-card"
          className="order-2 bg-bg-surface rounded-xl border border-accent/20 p-4 sm:p-5 flex flex-col gap-3 shadow-sm"
          aria-label="Export opportunity"
        >
          <div>
            <p className="text-[11px] font-semibold uppercase tracking-wide text-accent">Export opportunity</p>
            <h3 className="mt-1 text-base sm:text-lg font-semibold text-text-primary">
              {forecastExportTitle(plan.export)}
            </h3>
          </div>
          <div className="grid grid-cols-2 sm:grid-cols-4 gap-2">
            <div className="rounded-lg bg-bg-elevated p-3">
              <div className="text-[11px] text-text-secondary">Sell</div>
              <div data-testid="forecast-export-kwh" className="mt-0.5 text-sm font-semibold text-text-primary">{plan.export.kwh.toFixed(1)} kWh</div>
            </div>
            <div className="rounded-lg bg-bg-elevated p-3">
              <div className="text-[11px] text-text-secondary">Time</div>
              <div data-testid="forecast-export-window" className="mt-0.5 text-sm font-semibold text-text-primary">{plan.export.window.start}–{plan.export.window.end}</div>
            </div>
            <div className="rounded-lg bg-bg-elevated p-3">
              <div className="text-[11px] text-text-secondary">Est. earnings</div>
              <div data-testid="forecast-export-earning" className="mt-0.5 text-sm font-semibold text-text-primary">£{plan.export.earning.toFixed(2)}</div>
            </div>
            <div className="rounded-lg bg-bg-elevated p-3">
              <div className="text-[11px] text-text-secondary">Battery floor held</div>
              <div data-testid="forecast-export-floor" className="mt-0.5 text-sm font-semibold text-text-primary">{plan.export.after_min_soc_pct.toFixed(0)}%</div>
            </div>
          </div>
          <p data-testid="forecast-export-rationale" className="text-xs text-text-secondary">{plan.export.rationale}</p>
        </section>
      )}

      <div className="order-3 flex flex-col gap-3 sm:gap-4">
        <div
          role="tablist"
          aria-label="Forecast chart"
          className="grid grid-cols-3 rounded-lg bg-bg-surface p-1"
          onKeyDown={(e) => {
            // WAI-ARIA tabs keyboard pattern: Arrow keys select and focus
            // the neighbouring tab (wrapping), Home/End jump to the ends.
            // The keys are consumed so the page never scrolls underneath.
            const order = CHART_TABS.map(([chart]) => chart);
            const idx = order.indexOf(activeChart);
            let next: number | null = null;
            if (e.key === 'ArrowRight') next = (idx + 1) % order.length;
            else if (e.key === 'ArrowLeft')
              next = (idx - 1 + order.length) % order.length;
            else if (e.key === 'Home') next = 0;
            else if (e.key === 'End') next = order.length - 1;
            if (next == null) return;
            e.preventDefault();
            setActiveChart(order[next]);
            chartTabRefs.current[order[next]]?.focus();
          }}
        >
          {CHART_TABS.map(([chart, label]) => (
            <button
              key={chart}
              type="button"
              role="tab"
              id={`forecast-chart-tab-${chart}`}
              aria-controls={`forecast-chart-panel-${chart}`}
              aria-selected={activeChart === chart}
              tabIndex={activeChart === chart ? 0 : -1}
              data-testid={`forecast-chart-tab-${chart}`}
              onClick={() => setActiveChart(chart)}
              ref={(el) => {
                chartTabRefs.current[chart] = el;
              }}
              className={`rounded-md px-3 py-2 text-xs sm:text-sm font-medium transition-colors ${activeChart === chart ? 'bg-bg-elevated text-text-primary shadow-sm' : 'text-text-secondary hover:text-text-primary'}`}
            >
              {label}
            </button>
          ))}
        </div>

      <div
        role="tabpanel"
        id="forecast-chart-panel-solar"
        aria-labelledby="forecast-chart-tab-solar"
        className={activeChart === 'solar' ? '' : 'hidden'}
        aria-hidden={activeChart !== 'solar'}
      >
      <ChartCard title="Expected solar generation · next 72 hours">
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
      </div>

      <div
        role="tabpanel"
        id="forecast-chart-panel-consumption"
        aria-labelledby="forecast-chart-tab-consumption"
        className={activeChart === 'consumption' ? '' : 'hidden'}
        aria-hidden={activeChart !== 'consumption'}
      >
      <ChartCard title="Expected home use · next 72 hours">
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
      </div>

      <div
        role="tabpanel"
        id="forecast-chart-panel-battery"
        aria-labelledby="forecast-chart-tab-battery"
        className={activeChart === 'battery' ? '' : 'hidden'}
        aria-hidden={activeChart !== 'battery'}
      >
      <ChartCard
        title="Battery projection · next 72 hours"
        legend={
          batteryChart.length > 0 ? (
            <LineToggleLegend
              lines={batteryLegendLines}
              hidden={hiddenLines}
              onToggle={toggleLine}
            />
          ) : undefined
        }
        footer={
          (data.battery != null && lineVisible('soc')) ||
          (hasWithCharge && lineVisible('withCharge')) ||
          (hasWithSchedule && lineVisible('withCurrent')) ||
          (chargeMarkers.length > 0 && lineVisible('withCharge')) ? (
            <>
              {data.battery != null && lineVisible('soc') ? (
                <p
                  data-testid="forecast-caption-baseline"
                  className="text-[11px] text-text-secondary/70 font-sans leading-snug"
                >
                  <span
                    aria-hidden
                    className="inline-block w-3 h-px align-middle mr-1"
                    style={{ borderTop: '2px solid #34d399' }}
                  />
                  SOC if no slots are configured — solar surplus only.
                </p>
              ) : null}
              {hasWithCharge &&
              lineVisible('withCharge') &&
              plan?.recommendation?.kind === 'charge' ? (
                <p
                  data-testid="forecast-caption-plan"
                  className="text-[11px] text-text-secondary/70 font-sans leading-snug"
                >
                  <span
                    aria-hidden
                    className="inline-block w-3 h-px align-middle mr-1"
                    style={{ borderTop: '2px dashed #60a5fa' }}
                  />
                  SOC if overnight charge enacted —
                  Tomorrow {plan.recommendation.window.start}–{plan.recommendation.window.end},
                  {' '}{plan.recommendation.kwh.toFixed(1)} kWh.
                </p>
              ) : null}
              {hasWithSchedule && lineVisible('withCurrent') ? (
                <p
                  data-testid="forecast-caption-schedule"
                  className="text-[11px] text-text-secondary/70 font-sans leading-snug"
                >
                  <span
                    aria-hidden
                    className="inline-block w-3 h-px align-middle mr-1"
                    style={{ borderTop: '2px dotted #f472b6' }}
                  />
                  SOC with your current inverter schedule — enabled charge/discharge
                  slots as configured.
                </p>
              ) : null}
              {chargeMarkers.length > 0 && lineVisible('withCharge') ? (
                // Marker keys live in the footer, small and away from the
                // graph — the legend under the chart is for the lines.
                <p
                  data-testid="forecast-charge-legend"
                  className="text-[10px] text-text-secondary/70 font-sans leading-snug"
                >
                  <span
                    aria-hidden
                    className="inline-block w-3 h-px align-middle mr-1"
                    style={{ borderTop: '2px dashed #34d399' }}
                  />
                  Charge start
                  <span
                    aria-hidden
                    className="inline-block w-3 h-px align-middle mr-1 ml-3"
                    style={{ borderTop: '2px dashed #fbbf24' }}
                  />
                  Charge end
                </p>
              ) : null}
            </>
          ) : undefined
        }
      >
        {batteryChart.length === 0 ? (
          <div className="h-full flex items-center justify-center text-xs text-text-secondary">
            No battery projection — connect to the inverter.
          </div>
        ) : (
          <ResponsiveContainer width="100%" height="100%">
            <LineChart data={batteryChartData}>
              <CartesianGrid {...getHistoryChartGridProps(gridLineWeight)} />
              {lineVisible('withCharge') &&
                chargeMarkers.map((marker) => {
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
              {lineVisible('soc') && (
                <Line
                  type="monotone"
                  dataKey="soc"
                  stroke="#34d399"
                  strokeWidth={2}
                  dot={false}
                  name="If solar only"
                  isAnimationActive={false}
                />
              )}
              {hasWithCharge && lineVisible('withCharge') && (
                <Line
                  type="monotone"
                  dataKey="withCharge"
                  stroke="#60a5fa"
                  strokeWidth={2}
                  strokeDasharray="6 4"
                  dot={false}
                  name="If charge enacted"
                  connectNulls
                  isAnimationActive={false}
                />
              )}
              {hasWithSchedule && lineVisible('withCurrent') && (
                <Line
                  type="monotone"
                  dataKey="withCurrent"
                  stroke="#f472b6"
                  strokeWidth={2}
                  strokeDasharray="2 4"
                  dot={false}
                  name="With current schedule"
                  connectNulls
                  isAnimationActive={false}
                />
              )}
            </LineChart>
          </ResponsiveContainer>
        )}
      </ChartCard>
      </div>
      </div>

    </div>
  );
}
