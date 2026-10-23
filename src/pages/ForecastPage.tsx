import { useEffect, useMemo, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';
import {
  Area,
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
import { useDeferredSave } from '../hooks/useDeferredSave';
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
  chargeTargetReadBack,
  holdTargetPct,
  forecastStatusMessages,
  forecastXAxisTicks,
  forecastYAxisScale,
  forwardHourTimestamps,
  parseLeadMinutes,
  planAutoApplyTriggerLabel,
  insertChargeStartVertices,
  relabelToStateInstants,
  minimumSocNote,
  shouldRefetchForecast,
  toBatteryChartData,
  toConsumptionChartData,
  toSolarChartData,
  tomorrowSummary,
} from '../lib/forecast';
import type { ForecastData, PlanResponse, PlanStrategy } from '../lib/forecast';
import {
  actualSocSeries,
  actualSolarAverageKwByHour,
  compareBattery,
  compareSolar,
  decisionRows,
  mergeSeries,
  PAST_RANGES,
  pastAxisDomain,
  pastHistoryPath,
  pastRangeSeconds,
  pastTickStepSeconds,
  planHistoryPath,
  stitchPlannedBattery,
  stitchPlannedSolar,
} from '../lib/planHistory';
import type { HistoryPoint, PastRange, PlanHistorySnapshot } from '../lib/planHistory';
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

/** How far the forecast's generation time must move before the past view is
 *  fetched again. */
const PAST_REFRESH_SECS = 600;

/** What the past fetch brought back for one range (issue #361). */
type PastResult = {
  range: PastRange;
  /** The instant the window ends at: the forecast's generation time. */
  nowSec: number;
  snapshots: PlanHistorySnapshot[];
  /** True when the decisions could not be fetched (as opposed to none). */
  decisionsFailed: boolean;
  soc: HistoryPoint[] | undefined;
  solarPower: HistoryPoint[] | undefined;
  errors: string[];
};

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
      aria-label="Projection Lines — Activate to Show or Hide a Line"
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
        <div className="text-base font-bold">Applying Changes to Inverter…</div>
        <div className="mt-0.5 text-sm font-medium text-slate-800">
          Scheduling the charge slot. This can take several seconds; please keep this page open.
        </div>
      </div>
    </div>
  );
}

type ForecastChargeSlot = NonNullable<PlanResponse['apply']>['charge_slot'];
const FORECAST_CHARGE_CONFIRM_TIMEOUT_MS = 15_000;

function forecastPlanIdentity(plan: PlanResponse): string {
  // The recommendation is included alongside the write payload because the
  // planner can materially change its advice even when a legacy backend has
  // not changed every apply field yet.
  return JSON.stringify({ recommendation: plan.recommendation, apply: plan.apply });
}

/** The Forecast page's three chart tabs, in tab-strip (and keyboard) order. */
const CHART_TABS = [
  ['battery', 'Battery'],
  ['solar', 'Solar'],
  ['consumption', 'Home Use'],
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
    && chargeTargetReadBack(desired.target_soc, actual.target_soc)
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

/** A minimum battery level the planner accepts: a number from 0 to 100. */
function isValidMinSocInput(value: string): boolean {
  const n = Number(value);
  return value.trim() !== '' && Number.isFinite(n) && n >= 0 && n <= 100;
}

/** A battery efficiency the planner accepts, as a whole percent. */
function isValidEfficiencyPct(n: number): boolean {
  return Number.isInteger(n) && n >= 50 && n <= 100;
}

export default function ForecastPage() {
  const [data, setData] = useState<ForecastData | null>(null);
  const [plan, setPlan] = useState<PlanResponse | null>(null);
  const [applyState, setApplyState] = useState<'idle' | 'sending' | 'done' | 'error'>('idle');
  const [appliedPlanIdentity, setAppliedPlanIdentity] = useState<string | null>(null);
  const [applyError, setApplyError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [minSocPctInput, setMinSocPctInput] = useState<string>('20');
  const [minSocError, setMinSocError] = useState<string | null>(null);
  const [minSocPct, setMinSocPct] = useState<number>(20);
  const [planStrategy, setPlanStrategy] = useState<PlanStrategy>('min_soc');
  const [planStrategySaving, setPlanStrategySaving] = useState(false);
  const [planStrategyError, setPlanStrategyError] = useState<string | null>(null);
  const [planAutoRefresh, setPlanAutoRefresh] = useState<boolean>(false);
  const [planAutoApply, setPlanAutoApply] = useState<boolean>(false);
  const [planAutoApplyLeadInput, setPlanAutoApplyLeadInput] = useState<string>('30');
  const [planAutoApplySaving, setPlanAutoApplySaving] = useState(false);
  // True while the checkbox's own save is in flight. The lead field is
  // locked then: a lead edit would save with the pre-toggle enabled state
  // and could switch the trigger straight back on.
  const [planAutoToggleSaving, setPlanAutoToggleSaving] = useState(false);
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
  // and persisted as 0–1 ratios. They save together — the planner needs
  // both to be sane, so one invalid field blocks the pair.
  const [chargeEffInput, setChargeEffInput] = useState<string>('90');
  const [dischargeEffInput, setDischargeEffInput] = useState<string>('95');
  const [effError, setEffError] = useState<string | null>(null);
  const [activeChart, setActiveChart] = useState<ForecastChart>('battery');
  // "Show Past" (issue #361): earlier planner decisions and what the
  // battery and the sun actually did, drawn behind the forward charts.
  const [pastRange, setPastRange] = useState<PastRange>('off');
  const [pastResult, setPastResult] = useState<PastResult | null>(null);
  // The payload's generation time, read by the past fetch so a refetch of
  // the forecast never restarts it.
  const generatedAtRef = useRef(0);
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

  /**
   * Fetch the forecast, plan and settings.
   *
   * `syncForm` controls whether the Plan settings INPUT fields are
   * rewritten from the server. The initial mount populates them; every
   * background refetch must leave them alone — review H17: a SOC-triggered
   * or 30 s refetch used to clobber unsaved edits mid-typing (a "45" being
   * typed into "Minimum battery level" snapped back to the saved value).
   * The form inputs only change through this page's own saves, and those
   * handlers already update both the input and the committed state, so
   * there is nothing to re-sync afterwards.
   */
  const load = async (syncForm = true) => {
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
            forecast_plan_strategy?: PlanStrategy;
            forecast_charge_efficiency?: number;
            forecast_discharge_efficiency?: number;
          } & Record<string, unknown>;
        }>('/api/settings'),
      ]);
      generatedAtRef.current = forecastRes.data.generated_at;
      setData(forecastRes.data);
      setPlan(planRes.data);
      if (settingsRes.data.forecast_min_soc_pct != null) {
        const v = Math.round(settingsRes.data.forecast_min_soc_pct);
        setMinSocPct(v);
      }
      setPlanAutoRefresh(settingsRes.data.forecast_plan_auto_refresh ?? false);
      setPlanAutoApply(settingsRes.data.forecast_plan_auto_apply_enabled ?? false);
      setPlanStrategy(
        settingsRes.data.forecast_plan_strategy === 'hold_window' ||
          settingsRes.data.forecast_plan_strategy === 'cheapest'
          ? settingsRes.data.forecast_plan_strategy
          : 'min_soc',
      );
      if (syncForm) {
        if (settingsRes.data.forecast_min_soc_pct != null) {
          setMinSocPctInput(String(Math.round(settingsRes.data.forecast_min_soc_pct)));
        }
        const chargeEfficiencyPct = settingsRes.data.forecast_charge_efficiency != null
          ? Math.round(settingsRes.data.forecast_charge_efficiency * 100)
          : 90;
        const dischargeEfficiencyPct = settingsRes.data.forecast_discharge_efficiency != null
          ? Math.round(settingsRes.data.forecast_discharge_efficiency * 100)
          : 95;
        setChargeEffInput(String(chargeEfficiencyPct));
        setDischargeEffInput(String(dischargeEfficiencyPct));
        setPlanAutoApplyLeadInput(
          String(settingsRes.data.forecast_plan_auto_apply_lead_minutes ?? 30),
        );
      }
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
      void load(false);
    } else {
      // Track the latest snapshot state either way so the next call has
      // a current SOC for delta comparison.
      lastTriggerRef.current = { soc: newSoc, maxPowerW: newMaxPower };
    }
  }, [snapshot]);

  // The past window ends at the forecast's generation time, which moves on
  // as the page refetches; the past is fetched again each time it has moved
  // a few minutes so the measured lines and recent decisions keep up.
  const pastWindowKey = data ? Math.floor(data.generated_at / PAST_REFRESH_SECS) : 0;
  // Fetch the recorded decisions and the measured history for the chosen
  // past range. The two are independent: if one fails the other still
  // draws. A late answer for a range that is no longer selected is dropped.
  useEffect(() => {
    if (pastRange === 'off') return;
    let cancelled = false;
    const nowSec = generatedAtRef.current;
    void (async () => {
      const [decisions, measured] = await Promise.allSettled([
        apiGet<{ ok: boolean; data: { snapshots: PlanHistorySnapshot[] } }>(
          planHistoryPath(pastRange, nowSec),
        ),
        apiGet<{ ok: boolean; data: Record<string, HistoryPoint[]> }>(
          pastHistoryPath(pastRange, nowSec),
        ),
      ]);
      if (cancelled) return;
      const reason = (result: PromiseRejectedResult) =>
        result.reason instanceof Error ? result.reason.message : 'request failed';
      setPastResult({
        range: pastRange,
        nowSec,
        snapshots: decisions.status === 'fulfilled' ? decisions.value.data?.snapshots ?? [] : [],
        decisionsFailed: decisions.status === 'rejected',
        soc: measured.status === 'fulfilled' ? measured.value.data?.soc : undefined,
        solarPower: measured.status === 'fulfilled' ? measured.value.data?.solar_power : undefined,
        errors: [
          ...(decisions.status === 'rejected'
            ? [`planner decisions: ${reason(decisions)}`]
            : []),
          ...(measured.status === 'rejected' ? [`measured history: ${reason(measured)}`] : []),
        ],
      });
    })();
    return () => {
      cancelled = true;
    };
  }, [pastRange, pastWindowKey]);

  const saveMinSoc = async () => {
    if (minSocPctInput.trim() === '') {
      setMinSocPctInput(String(minSocPct));
      setMinSocError('Min SOC is required');
      window.setTimeout(() => document.getElementById('forecast-min-soc-input')?.focus(), 0);
      return;
    }
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
    try {
      await apiPost('/api/settings', { forecast_min_soc_pct: next });
      setMinSocPct(next);
      // Refetch — the planner sees the new floor on the next call.
      await load(false);
    } catch (e) {
      setMinSocError(e instanceof Error ? e.message : 'Save failed');
    }
  };

  // Persist the planner strategy (issue #359). The select stays on the
  // saved value until the save succeeds, so a rejected save never shows a
  // strategy the planner isn't using.
  const savePlanStrategy = async (next: PlanStrategy) => {
    setPlanStrategyError(null);
    setPlanStrategySaving(true);
    try {
      await apiPost('/api/settings', { forecast_plan_strategy: next });
      setPlanStrategy(next);
      // Refetch — the plan is recomputed with the new strategy.
      await load(false);
    } catch (e) {
      setPlanStrategyError(e instanceof Error ? e.message : 'Save failed');
    } finally {
      setPlanStrategySaving(false);
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
      // Refetch so the plan note and trigger time reflect the new state.
      await load(false);
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
    if (!isValidEfficiencyPct(charge) || !isValidEfficiencyPct(discharge)) {
      setEffError('Battery efficiencies must be whole numbers between 50 and 100');
      return;
    }
    setEffError(null);
    try {
      await apiPost('/api/settings', {
        forecast_charge_efficiency: charge / 100,
        forecast_discharge_efficiency: discharge / 100,
      });
      // Refetch — the planner sees the new efficiencies on the next call.
      await load(false);
    } catch (e) {
      setEffError(e instanceof Error ? e.message : 'Save failed');
    }
  };

  // The Plan Settings number fields save after a pause, on blur or Enter,
  // and when the page unmounts — never only on blur (issue #391).
  const minSocSaver = useDeferredSave(saveMinSoc);
  const effSaver = useDeferredSave(saveEfficiencies);
  // An edited lead time never toggles the trigger — it saves only while
  // automatic handling is on, and upgrades a legacy auto-refresh setting
  // to auto-apply.
  const leadSaver = useDeferredSave(() =>
    planAutoHandling ? savePlanAutoApply(true) : undefined,
  );
  const saveOnEnter = (saver: { flush: () => void }) => (e: KeyboardEvent) => {
    if (e.key === 'Enter') saver.flush();
  };

  const handleApply = async () => {
    if (!plan || !plan.apply) return;
    const identity = forecastPlanIdentity(plan);
    const snapshotBeforeApply = useInverterStore.getState().snapshot;
    setApplyState('sending');
    setApplyError(null);
    let chargeSlotWritten = false;
    try {
      await apiPost('/api/control/charge-slot', plan.apply.charge_slot);
      chargeSlotWritten = true;
      await apiPost('/api/control/timed-charge', plan.apply.timed_charge);
      if (snapshotBeforeApply) {
        await waitForForecastChargeReadback(plan.apply.charge_slot, snapshotBeforeApply);
      }
      setAppliedPlanIdentity(identity);
      setApplyState('done');
    } catch (e) {
      const applyError = e instanceof Error ? e.message : 'Apply failed';
      if (chargeSlotWritten) {
        // The slot endpoint enables scheduled charging on single-phase
        // models before the separate Timed Charge request. If that second
        // request fails, undo the partial apply so a forecast failure cannot
        // leave a charge window armed unexpectedly. Restore the previous
        // state when a snapshot is available; a fresh page has no prior slot
        // to restore, so clear the slot instead.
        const previousSlot = snapshotBeforeApply?.charge_slots?.[plan.apply.charge_slot.slot - 1];
        const rollbackSlot = previousSlot
          ? { slot: plan.apply.charge_slot.slot, ...previousSlot }
          : { ...plan.apply.charge_slot, enabled: false };
        try {
          await apiPost('/api/control/charge-slot', rollbackSlot);
          if (snapshotBeforeApply) {
            await apiPost('/api/control/timed-charge', {
              enabled: snapshotBeforeApply.enable_charge,
            });
          }
        } catch (rollbackError) {
          const rollbackMessage = rollbackError instanceof Error
            ? rollbackError.message
            : 'unknown rollback error';
          setApplyError(`${applyError} (rollback failed: ${rollbackMessage})`);
          setApplyState('error');
          return;
        }
      }
      setApplyState('error');
      setApplyError(applyError);
    }
  };

  // Past view (issue #361). Only the result for the range on screen counts:
  // a stale one from an earlier choice, or one still loading, draws nothing.
  // The stitched series are derived once per fetched result rather than on
  // every render, so an unrelated re-render does not re-walk the whole past.
  // This sits above the early returns below so the hook order never changes.
  const past = pastRange !== 'off' && pastResult?.range === pastRange ? pastResult : null;
  const generatedAt = data?.generated_at;
  const pastSeries = useMemo(() => {
    const seriesNow = past?.nowSec ?? generatedAt ?? 0;
    const spanSecs = pastRangeSeconds(pastRange);
    const plannedSoc = past ? stitchPlannedBattery(past.snapshots, seriesNow) : [];
    const actualSoc = past ? actualSocSeries(past.soc, seriesNow - spanSecs, seriesNow) : [];
    const plannedSolar = past ? stitchPlannedSolar(past.snapshots, seriesNow) : [];
    const actualSolar = past ? actualSolarAverageKwByHour(past.solarPower, seriesNow) : [];
    return {
      pastPlannedSoc: plannedSoc,
      pastActualSoc: actualSoc,
      pastPlannedSolar: plannedSolar,
      pastActualSolar: actualSolar,
      batteryAccuracy: compareBattery(plannedSoc, actualSoc),
      solarAccuracy: compareSolar(plannedSolar, actualSolar),
      pastDecisions: past ? decisionRows(past.snapshots) : [],
    };
  }, [past, pastRange, generatedAt]);

  if (error) {
    return (
      <div data-testid="forecast-error" className="bg-red-500/10 border border-red-500/30 rounded-lg p-4 text-sm text-text-primary">
        Couldn’t load the forecast: {error}
      </div>
    );
  }

  if (!loaded || !data) {
    return <div className="text-text-secondary text-sm animate-pulse">Loading Forecast…</div>;
  }

  const statusMessages = forecastStatusMessages(data.status);
  const currentPlanIdentity = plan ? forecastPlanIdentity(plan) : null;
  const currentPlanWasApplied = currentPlanIdentity !== null
    && appliedPlanIdentity === currentPlanIdentity;
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
  // Issue #360: explain what a 0% minimum, or one at or below the inverter
  // reserve, means to the planner beside the input.
  const reserveNote = minimumSocNote(minSocPct, data.battery?.reserve_soc_pct);
  const holdTarget = plan?.recommendation ? holdTargetPct(plan.recommendation) : null;
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
  const pastLoading = pastRange !== 'off' && past === null;
  const pastNow = past?.nowSec ?? data.generated_at;
  const {
    pastPlannedSoc,
    pastActualSoc,
    pastPlannedSolar,
    pastActualSolar,
    batteryAccuracy,
    solarAccuracy,
    pastDecisions,
  } = pastSeries;
  const hasPastPlanned = pastPlannedSoc.length > 0;
  const hasPastActual = pastActualSoc.length > 0;
  const hasPastSolar = pastPlannedSolar.length > 0;
  const hasPastActualSolar = pastActualSolar.length > 0;
  const batteryRows = past
    ? mergeSeries(batteryChartData, {
        pastPlanned: pastPlannedSoc,
        actualSoc: pastActualSoc,
      })
    : batteryChartData;
  const solarRows = past
    ? mergeSeries(solarChart, {
        pastSolar: pastPlannedSolar,
        actualSolar: pastActualSolar,
      })
    : solarChart;
  // Past lines have their own colours: violet for what the planner
  // expected, pale for what really happened.
  const PAST_PLANNED_COLOUR = '#a78bfa';
  const PAST_ACTUAL_COLOUR = '#e2e8f0';
  const lineVisible = (key: string) => !hiddenLines.has(key);
  // Legend descriptions: SOC always (the no-grid-charging baseline); the
  // what-if lines only when data exists for them.
  const batteryLegendLines = [
    { key: 'soc', label: 'If Solar Only', colour: '#34d399', style: 'solid' as const },
    ...(hasWithCharge
      ? [
          {
            key: 'withCharge',
            label: 'If Charge Enacted',
            colour: '#60a5fa',
            style: 'dashed' as const,
          },
        ]
      : []),
    ...(hasWithSchedule
      ? [
          {
            key: 'withCurrent',
            label: 'With Current Schedule',
            colour: '#f472b6',
            style: 'dotted' as const,
          },
        ]
      : []),
    ...(hasPastPlanned
      ? [
          {
            key: 'pastPlanned',
            label: 'Planned (Past)',
            colour: PAST_PLANNED_COLOUR,
            style: 'dashed' as const,
          },
        ]
      : []),
    ...(hasPastActual
      ? [{ key: 'actualSoc', label: 'Actual', colour: PAST_ACTUAL_COLOUR, style: 'solid' as const }]
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
  // The battery and solar charts stretch back over the past view; the
  // home-use chart has no past data and keeps the forward axis.
  const chartAxisDomain = pastAxisDomain(pastRange, forecastAxisDomain[0], forecastAxisDomain[1]);
  const chartAxisTicks = forecastXAxisTicks(
    chartAxisDomain[0],
    chartAxisDomain[1],
    pastTickStepSeconds(pastRange),
  );
  const solarYAxis = forecastYAxisScale(
    Math.max(
      0,
      ...solarChart.map((point) => point.high),
      ...pastPlannedSolar.map(([, kwh]) => kwh),
      ...pastActualSolar.map(([, kwh]) => kwh),
    ),
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
        <h1 className="text-lg font-bold text-text-primary">Planner</h1>
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
          label="Solar Prediction"
          value={formatEnergy(summary.solarKwh)}
          testId="forecast-solar-tomorrow"
          hint={
            data.performance_ratio != null
              ? `calibrated against ${data.performance_ratio_days ?? '?'} days of your history (last 2 weeks)`
              : 'Preliminary — Still Calibrating'
          }
        />
        <SummaryTile
          label="House Consumption"
          value={formatEnergy(summary.consumptionKwh)}
          testId="forecast-consumption-tomorrow"
          hint={`${data.consumption_days_observed} days observed`}
        />
        <SummaryTile
          label="Expected Export"
          value={formatEnergy(
            plan?.recommendation?.kind === 'charge'
              ? plan.recommendation.export_tomorrow_with_charge_kwh
              : summary.surplusKwh,
          )}
          testId="forecast-surplus-tomorrow"
          hint={plan?.recommendation?.kind === 'charge' ? 'With Charge Plan' : undefined}
        />
        <SummaryTile
          label="Expected Import"
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
              <p className="text-[11px] font-semibold uppercase tracking-wide text-accent">Today’s Recommendation</p>
              <h2 className="mt-1 text-base sm:text-lg font-semibold text-text-primary">
              {forecastPlanTitle(plan.recommendation)}
              </h2>
            </div>
            {summary.startSocPct != null && (
              <span
                data-testid="forecast-plan-current-soc"
                className="shrink-0 rounded-full bg-bg-elevated px-3 py-1.5 text-xs text-text-secondary"
                title="Current Battery State of Charge"
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
                {holdTarget != null && (
                  <div data-testid="forecast-plan-target" className="text-[11px] text-text-secondary">
                    to {Math.round(holdTarget)}%, then hold
                  </div>
                )}
              </div>
              <div className="rounded-lg bg-bg-elevated p-3">
                <div className="text-[11px] text-text-secondary">Time</div>
                <div className="mt-0.5 text-sm font-semibold text-text-primary">{plan.recommendation.window.start}–{plan.recommendation.window.end}</div>
              </div>
              <div className="rounded-lg bg-bg-elevated p-3">
                <div className="text-[11px] text-text-secondary">Lowest Battery</div>
                <div className="mt-0.5 text-sm font-semibold text-text-primary"><span data-testid="forecast-plan-trough">{Math.round(plan.recommendation.observed_min_soc_pct)}%</span> → <span data-testid="forecast-plan-after-min">{plan.recommendation.after_min_soc_pct.toFixed(0)}%</span></div>
              </div>
              <div className="rounded-lg bg-bg-elevated p-3">
                <div className="text-[11px] text-text-secondary">Estimated Cost</div>
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
                disabled={applyState === 'sending' || currentPlanWasApplied}
                onClick={() => void handleApply()}
                className="px-3 py-1.5 rounded-md bg-accent text-accent-on text-sm font-medium hover:opacity-90 disabled:opacity-50"
              >
                {applyState === 'idle' && 'Apply — Schedule Charge Slot'}
                {applyState === 'sending' && 'Applying…'}
                {applyState === 'done' && currentPlanWasApplied && 'Applied ✓'}
                {applyState === 'done' && !currentPlanWasApplied && 'Apply — Schedule Charge Slot'}
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
              <div className="text-xs font-medium text-text-primary">Automatic Planning</div>
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
              <span>Plan Settings</span>
              <span aria-hidden className="text-text-secondary transition-transform group-open:rotate-180">⌄</span>
            </summary>
            <div className="border-t border-white/10 px-3 pb-3">
          {/* Planner strategy (issue #359) — how the overnight charge is
              sized. Saves immediately and triggers a plan refetch. */}
          <div className="pt-3 flex flex-col gap-1">
            <div className="flex items-center gap-3 flex-wrap">
              <label
                htmlFor="forecast-plan-strategy"
                className="text-text-primary text-xs font-sans font-medium"
              >
                Charging strategy
              </label>
              <select
                id="forecast-plan-strategy"
                value={planStrategy}
                onChange={(e) => void savePlanStrategy(e.target.value as PlanStrategy)}
                disabled={!data || planStrategySaving}
                className="bg-bg-surface text-text-primary rounded-lg px-3 py-1.5 text-sm border border-transparent focus:outline-none focus:border-accent disabled:opacity-50"
                data-testid="forecast-plan-strategy"
              >
                <option value="min_soc">Keep a Minimum</option>
                <option value="hold_window">Hold Through the Cheap Window</option>
                <option value="cheapest">Cheapest Overall</option>
              </select>
              {planStrategyError && (
                <span data-testid="forecast-plan-strategy-error" className="text-xs text-red-400">
                  {planStrategyError}
                </span>
              )}
            </div>
            <p className="text-[11px] text-text-secondary font-sans">
              {planStrategy === 'cheapest'
                ? 'Charges as far as the cheap rate saves money, then holds the battery there. Plans against the low solar estimate, so a dull day costs less, and leaves room for the sun when exports earn little.'
                : planStrategy === 'hold_window'
                  ? 'Charges to the lowest level that lasts until the next cheap period, then holds the battery there while the grid supplies the house for the rest of the cheap window.'
                  : 'Charges at full rate for just long enough to stay above your minimum, then lets the battery run the house again.'}
            </p>
          </div>
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
                onChange={(e) => {
                  setMinSocPctInput(e.target.value);
                  minSocSaver.edited(isValidMinSocInput(e.target.value));
                }}
                onBlur={minSocSaver.flush}
                onKeyDown={saveOnEnter(minSocSaver)}
                disabled={!data}
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
            {reserveNote && (
              <p data-testid="forecast-min-soc-reserve-note" className="text-[11px] text-amber-300 font-sans">
                {reserveNote}
              </p>
            )}
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
                onChange={() => {
                  // This save carries the lead field too, so a pending lead
                  // save must not follow it and flip the trigger back on.
                  leadSaver.cancel();
                  setPlanAutoToggleSaving(true);
                  void savePlanAutoApply(!planAutoHandling).finally(() =>
                    setPlanAutoToggleSaving(false),
                  );
                }}
                disabled={planAutoApplySaving}
                className="h-5 w-5 accent-accent"
                data-testid="forecast-auto-apply-toggle"
              />
              <span className="text-text-secondary text-xs font-sans">Every Day</span>
              <input
                id="forecast-auto-apply-lead"
                type="number"
                min={0}
                max={120}
                step={1}
                value={planAutoApplyLeadInput}
                onChange={(e) => {
                  setPlanAutoApplyLeadInput(e.target.value);
                  const lead = parseLeadMinutes(e.target.value);
                  leadSaver.edited(planAutoHandling && lead != null && lead <= 120);
                }}
                onBlur={leadSaver.flush}
                onKeyDown={saveOnEnter(leadSaver)}
                disabled={planAutoToggleSaving}
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
              Forecast page. They save together. */}
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
                onChange={(e) => {
                  setChargeEffInput(e.target.value);
                  effSaver.edited(
                    isValidEfficiencyPct(Number(e.target.value)) &&
                      isValidEfficiencyPct(Number(dischargeEffInput)),
                  );
                }}
                onBlur={effSaver.flush}
                onKeyDown={saveOnEnter(effSaver)}
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
                onChange={(e) => {
                  setDischargeEffInput(e.target.value);
                  effSaver.edited(
                    isValidEfficiencyPct(Number(e.target.value)) &&
                      isValidEfficiencyPct(Number(chargeEffInput)),
                  );
                }}
                onBlur={effSaver.flush}
                onKeyDown={saveOnEnter(effSaver)}
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
          aria-label="Export Opportunity"
        >
          <div>
            <p className="text-[11px] font-semibold uppercase tracking-wide text-accent">Export Opportunity</p>
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
              <div className="text-[11px] text-text-secondary">Est. Earnings</div>
              <div data-testid="forecast-export-earning" className="mt-0.5 text-sm font-semibold text-text-primary">£{plan.export.earning.toFixed(2)}</div>
            </div>
            <div className="rounded-lg bg-bg-elevated p-3">
              <div className="text-[11px] text-text-secondary">Battery Floor Held</div>
              <div data-testid="forecast-export-floor" className="mt-0.5 text-sm font-semibold text-text-primary">{plan.export.after_min_soc_pct.toFixed(0)}%</div>
            </div>
          </div>
          <p data-testid="forecast-export-rationale" className="text-xs text-text-secondary">{plan.export.rationale}</p>
        </section>
      )}

      <div className="order-3 flex flex-col gap-3 sm:gap-4">
        <div
          role="tablist"
          aria-label="Forecast Chart"
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

        {activeChart !== 'consumption' && (
          <div
            role="group"
            aria-label="Show Past"
            data-testid="forecast-past-range"
            className="flex flex-wrap items-center gap-2"
          >
            <span className="text-xs font-medium text-text-secondary">Show Past</span>
            <div className="inline-flex rounded-lg bg-bg-surface p-1">
              {PAST_RANGES.map(({ key, label }) => (
                <button
                  key={key}
                  type="button"
                  aria-pressed={pastRange === key}
                  onClick={() => {
                    setPastRange(key);
                    // Leaving the past view drops what was fetched, so
                    // coming back never shows a stale window.
                    if (key === 'off') setPastResult(null);
                  }}
                  className={`rounded-md px-3 py-1.5 text-xs font-medium transition-colors ${
                    pastRange === key
                      ? 'bg-bg-elevated text-text-primary shadow-sm'
                      : 'text-text-secondary hover:text-text-primary'
                  }`}
                >
                  {label}
                </button>
              ))}
            </div>
            {pastLoading && (
              <span role="status" className="text-xs text-text-secondary animate-pulse">
                Loading the past…
              </span>
            )}
          </div>
        )}

      <div
        role="tabpanel"
        id="forecast-chart-panel-solar"
        aria-labelledby="forecast-chart-tab-solar"
        className={activeChart === 'solar' ? '' : 'hidden'}
        aria-hidden={activeChart !== 'solar'}
      >
      <ChartCard
        title="Expected Solar Generation · Next 72 Hours"
        footer={
          hasPastSolar || hasPastActualSolar ? (
            <>
              <p className="text-[11px] text-text-secondary/70 font-sans leading-snug">
                {hasPastSolar ? (
                  <>
                    <span
                      aria-hidden
                      className="inline-block w-3 h-px align-middle mr-1"
                      style={{ borderTop: `2px dashed ${PAST_PLANNED_COLOUR}` }}
                    />
                    Forecast made at the time
                  </>
                ) : null}
                {hasPastActualSolar ? (
                  <>
                    <span
                      aria-hidden
                      className={`inline-block w-3 h-px align-middle mr-1 ${hasPastSolar ? 'ml-3' : ''}`}
                      style={{ borderTop: `2px solid ${PAST_ACTUAL_COLOUR}` }}
                    />
                    Actual generation
                  </>
                ) : null}
              </p>
              {solarAccuracy ? (
                <p
                  data-testid="forecast-past-solar-accuracy"
                  className="mt-1 text-[11px] text-text-secondary/70 font-sans leading-snug"
                >
                  Solar: {solarAccuracy.forecastKwh.toFixed(1)} kWh forecast against{' '}
                  {solarAccuracy.actualKwh.toFixed(1)} kWh generated over {solarAccuracy.hours}{' '}
                  {solarAccuracy.hours === 1 ? 'hour' : 'hours'}.
                </p>
              ) : null}
            </>
          ) : undefined
        }
      >
        {solarRows.length === 0 ? (
          <div className="h-full flex items-center justify-center text-xs text-text-secondary">
            No forecast data yet.
          </div>
        ) : (
          <ResponsiveContainer width="100%" height="100%">
            <ComposedChart data={solarRows}>
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
                domain={chartAxisDomain}
                ticks={chartAxisTicks}
                tickFormatter={formatForecastXAxisTick}
                interval={0}
                minTickGap={0}
                angle={-30}
                textAnchor="end"
                height={42}
              />
              {/* Each hourly point is an average power over that hour, so the
                  solar chart is in kW (issue #359). The daily totals stay kWh. */}
              <YAxis
                stroke="#94a3b8"
                fontSize={10}
                unit="kW"
                width={48}
                domain={[0, solarYAxis.max]}
                ticks={solarYAxis.ticks}
                allowDataOverflow
              />
              <Tooltip
                {...tooltipStyleProps}
                labelFormatter={(ts) => formatForecastXAxisTick(Number(ts))}
                formatter={(value: number | string) => `${Number(value).toFixed(2)} kW`}
              />
              {past && (
                <ReferenceLine
                  x={pastNow}
                  stroke="#94a3b8"
                  strokeDasharray="2 3"
                  label={{ value: 'now', fill: '#94a3b8', fontSize: 10 }}
                />
              )}
              {hasPastSolar && lineVisible('pastSolar') && (
                <Line
                  type="monotone"
                  dataKey="pastSolar"
                  stroke={PAST_PLANNED_COLOUR}
                  strokeWidth={2}
                  strokeDasharray="6 4"
                  dot={false}
                  name="Forecast (Past)"
                  connectNulls
                  isAnimationActive={false}
                />
              )}
              {hasPastActualSolar && lineVisible('actualSolar') && (
                <Line
                  type="monotone"
                  dataKey="actualSolar"
                  stroke={PAST_ACTUAL_COLOUR}
                  strokeWidth={2}
                  dot={false}
                  name="Actual"
                  connectNulls
                  isAnimationActive={false}
                />
              )}
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
            </ComposedChart>
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
      <ChartCard title="Expected Home Use · Next 72 Hours">
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
                name="Weekday Low Estimate"
                legendType="none"
              />
              <Area
                type="monotone"
                dataKey="weekdayP75"
                stroke="none"
                fill="#38bdf8"
                fillOpacity={0.15}
                name="Weekday High Estimate"
                legendType="none"
              />
              <Area
                type="monotone"
                dataKey="weekendP25"
                stroke="none"
                fill="#a78bfa"
                fillOpacity={0.15}
                name="Weekend Low Estimate"
                legendType="none"
              />
              <Area
                type="monotone"
                dataKey="weekendP75"
                stroke="none"
                fill="#a78bfa"
                fillOpacity={0.15}
                name="Weekend High Estimate"
                legendType="none"
              />
              <Line
                type="monotone"
                dataKey="weekday"
                stroke="#38bdf8"
                strokeWidth={2}
                dot={false}
                name="Weekday Median"
              />
              <Line
                type="monotone"
                dataKey="weekend"
                stroke="#a78bfa"
                strokeWidth={2}
                dot={false}
                name="Weekend Median"
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
        title="Battery Projection · Next 72 Hours"
        legend={
          batteryRows.length > 0 ? (
            <LineToggleLegend
              lines={batteryLegendLines}
              hidden={hiddenLines}
              onToggle={toggleLine}
            />
          ) : undefined
        }
        footer={
          <>
          {(data.battery != null && lineVisible('soc')) ||
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
          ) : undefined}
            {hasPastPlanned && lineVisible('pastPlanned') ? (
              <p
                data-testid="forecast-past-plan-note"
                className="text-[11px] text-text-secondary/70 font-sans leading-snug"
              >
                <span
                  aria-hidden
                  className="inline-block w-3 h-px align-middle mr-1"
                  style={{ borderTop: `2px dashed ${PAST_PLANNED_COLOUR}` }}
                />
                SOC the planner expected at the time, if its recommendation was followed.
              </p>
            ) : null}
            {batteryAccuracy ? (
              <p
                data-testid="forecast-past-battery-accuracy"
                className="mt-1 text-[11px] text-text-secondary/70 font-sans leading-snug"
              >
                Battery: over this period the charge averaged{' '}
                {batteryAccuracy.meanAbsErrorPct.toFixed(1)} points away from the plan
                {Math.abs(batteryAccuracy.biasPct) >= 0.05
                  ? `, ending ${Math.abs(batteryAccuracy.biasPct).toFixed(1)} points ${
                      batteryAccuracy.biasPct < 0 ? 'below' : 'above'
                    } it on average`
                  : ''}
                .
              </p>
            ) : null}
          </>
        }
      >
        {batteryRows.length === 0 ? (
          <div className="h-full flex items-center justify-center text-xs text-text-secondary">
            No battery projection — connect to the inverter.
          </div>
        ) : (
          <ResponsiveContainer width="100%" height="100%">
            <LineChart data={batteryRows}>
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
                domain={chartAxisDomain}
                ticks={chartAxisTicks}
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
              {past && (
                <ReferenceLine
                  x={pastNow}
                  stroke="#94a3b8"
                  strokeDasharray="2 3"
                  label={{ value: 'now', fill: '#94a3b8', fontSize: 10 }}
                />
              )}
              {hasPastPlanned && lineVisible('pastPlanned') && (
                <Line
                  type="monotone"
                  dataKey="pastPlanned"
                  stroke={PAST_PLANNED_COLOUR}
                  strokeWidth={2}
                  strokeDasharray="6 4"
                  dot={false}
                  name="Planned (Past)"
                  isAnimationActive={false}
                />
              )}
              {hasPastActual && lineVisible('actualSoc') && (
                <Line
                  type="monotone"
                  dataKey="actualSoc"
                  stroke={PAST_ACTUAL_COLOUR}
                  strokeWidth={2}
                  dot={false}
                  name="Actual"
                  connectNulls
                  isAnimationActive={false}
                />
              )}
              {lineVisible('soc') && (
                <Line
                  type="monotone"
                  dataKey="soc"
                  stroke="#34d399"
                  strokeWidth={2}
                  dot={false}
                  name="If Solar Only"
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
                  name="If Charge Enacted"
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
                  name="With Current Schedule"
                  connectNulls
                  isAnimationActive={false}
                />
              )}
            </LineChart>
          </ResponsiveContainer>
        )}
      </ChartCard>
      </div>

      {past && activeChart !== 'consumption' && (
        <section
          aria-label="Past Planner Decisions"
          className="bg-bg-surface rounded-lg p-3 sm:p-4 flex flex-col gap-2"
        >
          <h2 className="text-sm font-semibold text-text-primary">Past Planner Decisions</h2>
          {past.errors.length > 0 && (
            <p
              data-testid="forecast-past-error"
              role="alert"
              className="text-xs text-red-400"
            >
              Couldn’t load the {past.errors.join(' or the ')}.
            </p>
          )}
          {pastDecisions.length > 0 && (
            <ul data-testid="forecast-past-decisions" className="flex flex-col gap-2">
              {pastDecisions.map((decision) => (
                <li
                  key={`${decision.recordedAt}-${decision.kind}`}
                  data-testid="forecast-past-decision"
                  className="rounded-lg bg-bg-elevated p-3"
                >
                  <div className="flex flex-wrap items-baseline gap-x-2 text-[11px] text-text-secondary">
                    <span className="font-semibold">
                      {formatForecastXAxisTick(decision.recordedAt)}
                    </span>
                    <span>{decision.kindLabel}</span>
                  </div>
                  <div className="mt-0.5 text-sm font-semibold text-text-primary">
                    {decision.title}
                  </div>
                  {decision.rationale && (
                    <p className="mt-0.5 text-xs text-text-secondary">{decision.rationale}</p>
                  )}
                </li>
              ))}
            </ul>
          )}
          {pastDecisions.length === 0 && !past.decisionsFailed && (
            <p data-testid="forecast-past-empty" className="text-xs text-text-secondary">
              No planner decisions were recorded in this period. The planner records one before each
              cheap window once weather is set up, so there is nothing to look back on yet.
            </p>
          )}
        </section>
      )}
      </div>

    </div>
  );
}
