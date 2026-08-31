/**
 * Pure helpers and types for the Forecast page (issue #283).
 *
 * Everything here is a transformation of the `GET /api/forecast` payload —
 * no fetching, no React — so the maths is unit-testable in isolation.
 */

export type ForecastSolarHour = {
  timestamp: number;
  kwh: number;
  band_low: number;
  band_high: number;
};

export type ForecastConsumptionHour = {
  hour: number;
  kwh: number;
  p25: number;
  p75: number;
};

export type ForecastBattery = {
  capacity_kwh: number;
  start_soc_pct: number;
  reserve_soc_pct: number;
  hours: [number, number][];
  end_soc_pct: number;
};

/** Number of forward hours displayed by the Forecast page. */
export const FORECAST_HORIZON_HOURS = 72;

export type ForecastData = {
  generated_at: number;
  status: string[];
  performance_ratio: number | null;
  performance_ratio_days: number | null;
  solar: ForecastSolarHour[];
  solar_today_remaining_kwh: number;
  solar_tomorrow_kwh: number;
  consumption_weekday: ForecastConsumptionHour[];
  consumption_weekend: ForecastConsumptionHour[];
  consumption_days_observed: number;
  consumption_sufficient: boolean;
  consumption_tomorrow_kwh: number;
  battery: ForecastBattery | null;
  import_tomorrow_kwh: number;
  export_tomorrow_kwh: number;
};

/**
 * Map backend degradation codes to a human explanation. Unknown codes pass
 * through verbatim so a newer backend's codes stay visible on an older UI.
 */
export function forecastStatusMessages(status: string[]): string[] {
  return status.map((code) => {
    switch (code) {
      case 'weather_disabled':
        return 'Weather integration is off — enable it in Settings so solar forecasts can be fetched.';
      case 'no_coordinates':
        return 'No location configured — set a postcode or coordinates in Settings.';
      case 'no_forecast_data':
        return 'No forecast data yet — the fetcher runs every three hours once weather is enabled.';
      case 'calibrating':
        return 'Solar model is still calibrating against your generation history; numbers are preliminary.';
      case 'insufficient_consumption_history':
        return 'Learning your consumption history — about a week of data is needed.';
      case 'no_snapshot':
        return 'No inverter connection yet — the battery projection needs a live snapshot.';
      case 'no_battery_capacity':
        return 'Battery capacity unknown — connect to the inverter so it can be read.';
      default:
        return code;
    }
  });
}

export type SolarChartPoint = {
  timestamp: number;
  kwh: number;
  low: number;
  high: number;
};

/** Chart-ready solar points with the band clamped at zero. */
export function toSolarChartData(points: ForecastSolarHour[]): SolarChartPoint[] {
  return points.map((p) => ({
    timestamp: p.timestamp,
    kwh: p.kwh,
    low: Math.max(0, p.band_low),
    high: Math.max(0, p.band_high),
  }));
}

export type BatteryChartPoint = { timestamp: number; soc: number };

/** Prepend the live SOC at the forecast's generation time so a chart
 *  built from `[timestamp, soc][]` pairs starts at "now". The stored
 *  forward hours are full FUTURE hours (elapsed ones are dropped by
 *  the query), so the first series point is the end of the first hour
 *  — a full hour's drain below the live SOC, which reads wrong next
 *  to the "Battery now" tile (user report: graph starting at 48% while
 *  the battery sat at 59%). Skipped when the anchor would not precede
 *  the first point (already anchored) or the series is empty. */
export function anchorSeriesAtNow(
  series: [number, number][],
  generatedAt: number,
  startValue: number,
): [number, number][] {
  if (series.length === 0 || generatedAt >= series[0][0]) {
    return series;
  }
  return [[generatedAt, startValue], ...series];
}

/** Chart-ready SOC projection points. */
export function toBatteryChartData(battery: ForecastBattery): BatteryChartPoint[] {
  return battery.hours.map(([timestamp, soc]) => ({ timestamp, soc }));
}

export type ConsumptionChartPoint = {
  timestamp: number;
  weekday: number | null;
  weekdayP25: number | null;
  weekdayP75: number | null;
  weekend: number | null;
  weekendP25: number | null;
  weekendP75: number | null;
};

/** Tile weekday/weekend profiles onto the forward timestamps so the
 * Consumption chart shares the same x-axis, start time and horizon as the
 * other forecast charts. Only the profile matching each timestamp's local
 * day type is populated, leaving Recharts to break the other line. */
export function toConsumptionChartData(
  weekday: ForecastConsumptionHour[],
  weekend: ForecastConsumptionHour[],
  timestamps: number[],
): ConsumptionChartPoint[] {
  const byHour = (series: ForecastConsumptionHour[]) => {
    const map = new Map<number, ForecastConsumptionHour>();
    for (const c of series) map.set(c.hour, c);
    return map;
  };
  const weekdayByHour = byHour(weekday);
  const weekendByHour = byHour(weekend);
  return timestamps.map((ts) => {
    const date = new Date(ts * 1000);
    const isWeekend = date.getDay() === 0 || date.getDay() === 6;
    const c = (isWeekend ? weekendByHour : weekdayByHour).get(date.getHours());
    return {
      timestamp: ts,
      weekday: isWeekend ? null : c?.kwh ?? 0,
      weekdayP25: isWeekend ? null : c?.p25 ?? 0,
      weekdayP75: isWeekend ? null : c?.p75 ?? 0,
      weekend: isWeekend ? c?.kwh ?? 0 : null,
      weekendP25: isWeekend ? c?.p25 ?? 0 : null,
      weekendP75: isWeekend ? c?.p75 ?? 0 : null,
    };
  });
}

const FORECAST_X_AXIS_TICK_INTERVAL_SECONDS = 12 * 60 * 60;
const FORECAST_Y_AXIS_TARGET_INTERVALS = 8;

export type ForecastYAxisScale = {
  max: number;
  ticks: number[];
};

/** Return dated x-axis ticks at 12-hour intervals, including both bounds. */
export function forecastXAxisTicks(start: number, end: number): number[] {
  if (!Number.isFinite(start) || !Number.isFinite(end) || end < start) return [];
  const ticks: number[] = [];
  for (let tick = start; tick <= end; tick += FORECAST_X_AXIS_TICK_INTERVAL_SECONDS) {
    ticks.push(tick);
  }
  if (ticks[ticks.length - 1] !== end) ticks.push(end);
  return ticks;
}

/** Format a forecast x-axis tick with the calendar date and local time. */
export function formatForecastXAxisTick(tsSeconds: number): string {
  const date = new Date(tsSeconds * 1000);
  const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun',
    'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
  return `${String(date.getDate()).padStart(2, '0')} ${months[date.getMonth()]} ${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}`;
}

/** Choose a clean, adaptive y-axis scale with evenly spaced ticks. */
export function forecastYAxisScale(dataMax: number): ForecastYAxisScale {
  if (!Number.isFinite(dataMax) || dataMax <= 0) {
    return { max: 1, ticks: [0, 0.2, 0.4, 0.6, 0.8, 1] };
  }

  const rawStep = dataMax / FORECAST_Y_AXIS_TARGET_INTERVALS;
  const magnitude = 10 ** Math.floor(Math.log10(rawStep));
  const normalized = rawStep / magnitude;
  const niceFactor = normalized <= 1 ? 1 : normalized <= 2 ? 2 : normalized <= 5 ? 5 : 10;
  const step = niceFactor * magnitude;
  const max = Math.ceil(dataMax / step) * step;
  const count = Math.round(max / step);
  const ticks = Array.from({ length: count + 1 }, (_, index) => {
    // Avoid labels such as 0.30000000000000004 when the step is fractional.
    return Number((index * step).toPrecision(12));
  });
  return { max, ticks };
}

/** Hourly timestamps starting at the current hour boundary, spanning
 *  `count` hours forward. Used when neither the solar series nor the
 *  battery projection provides timestamps (degraded, weather-off
 *  state) so the consumption chart still renders on a now-anchored
 *  axis instead of reverting to a midnight-anchored typical day. */
export function forwardHourTimestamps(count: number): number[] {
  const start = Math.floor(Date.now() / 1000 / 3600) * 3600;
  return Array.from({ length: count }, (_, i) => start + i * 3600);
}

export type TomorrowSummary = {
  solarKwh: number;
  consumptionKwh: number;
  surplusKwh: number;
  importKwh: number;
  startSocPct: number | null;
};

/** The "Tomorrow" summary card numbers. Surplus is the predicted export. */
export function tomorrowSummary(data: ForecastData): TomorrowSummary {
  return {
    solarKwh: data.solar_tomorrow_kwh,
    consumptionKwh: data.consumption_tomorrow_kwh,
    surplusKwh: data.export_tomorrow_kwh,
    importKwh: data.import_tomorrow_kwh,
    startSocPct: data.battery ? data.battery.start_soc_pct : null,
  };
}

// ---------------------------------------------------------------------------
// Planner (Phase 2)
// ---------------------------------------------------------------------------

export type PlannerChargeWindow = {
  /** "HH:MM" wall-clock start — the schedule may cross midnight. */
  start: string;
  /** "HH:MM" wall-clock end. */
  end: string;
  /** £/kWh inside the window. */
  rate: number;
  /** True when the start occurs on the day after the planner's `now`. */
  tomorrow: boolean;
};

export type ForecastChargeMarker = {
  kind: 'start' | 'end';
  timestamp: number;
};

/** Return the start/end of the one planned charge occurrence visible on the
 * forecast's forward axis. The next occurrence is intentionally omitted:
 * the planner is recalculated from the next live SOC before it is applied. */
export function forecastChargeMarkers(
  generatedAt: number,
  window: PlannerChargeWindow,
): ForecastChargeMarker[] {
  const parseMinutes = (value: string): number | null => {
    const match = /^(\d{2}):(\d{2})$/.exec(value);
    if (!match) return null;
    const hour = Number(match[1]);
    const minute = Number(match[2]);
    return hour <= 23 && minute <= 59 ? hour * 60 + minute : null;
  };
  const startMin = parseMinutes(window.start);
  const endMin = parseMinutes(window.end);
  if (startMin == null || endMin == null || endMin === startMin) return [];

  const horizonEnd = generatedAt + FORECAST_HORIZON_HOURS * 3600;
  const firstDate = new Date(generatedAt * 1000);
  firstDate.setHours(0, 0, 0, 0);
  if (window.tomorrow) firstDate.setDate(firstDate.getDate() + 1);

  const start = new Date(firstDate);
  start.setHours(Math.floor(startMin / 60), startMin % 60, 0, 0);
  const end = new Date(firstDate);
  end.setHours(Math.floor(endMin / 60), endMin % 60, 0, 0);
  if (endMin < startMin) end.setDate(end.getDate() + 1);
  const startTimestamp = start.getTime() / 1000;
  const endTimestamp = end.getTime() / 1000;
  const markers: ForecastChargeMarker[] = [];
  if (startTimestamp >= generatedAt && startTimestamp <= horizonEnd) {
    markers.push({ kind: 'start', timestamp: startTimestamp });
  }
  if (endTimestamp >= generatedAt && endTimestamp <= horizonEnd) {
    markers.push({ kind: 'end', timestamp: endTimestamp });
  }
  return markers;
}

/** Keep the hypothetical SOC line only through the start of the next
 * scheduled charge occurrence. The next occurrence will be recalculated
 * from a fresh live SOC, so extending tonight's result beyond that point
 * would imply a repeated charge that has not been planned. */
export function truncateSeriesAtNextChargeStart(
  series: [number, number][],
  generatedAt: number,
  window: PlannerChargeWindow,
): [number, number][] {
  const parseMinutes = (value: string): number | null => {
    const match = /^(\d{2}):(\d{2})$/.exec(value);
    if (!match) return null;
    const hour = Number(match[1]);
    const minute = Number(match[2]);
    return hour <= 23 && minute <= 59 ? hour * 60 + minute : null;
  };
  const startMin = parseMinutes(window.start);
  const endMin = parseMinutes(window.end);
  if (startMin == null || endMin == null || startMin === endMin) return series;

  const firstDate = new Date(generatedAt * 1000);
  firstDate.setHours(0, 0, 0, 0);
  if (window.tomorrow) firstDate.setDate(firstDate.getDate() + 1);
  const firstStart = new Date(firstDate);
  firstStart.setHours(Math.floor(startMin / 60), startMin % 60, 0, 0);
  if (firstStart.getTime() / 1000 < generatedAt) firstStart.setDate(firstStart.getDate() + 1);
  const nextStart = new Date(firstStart);
  nextStart.setDate(nextStart.getDate() + 1);
  const nextStartTimestamp = nextStart.getTime() / 1000;
  return series.filter(([timestamp]) => timestamp <= nextStartTimestamp);
}

export type PlanRecommendation =
  | {
      kind: 'charge';
      window: PlannerChargeWindow;
      /** AC kWh to draw from the grid during the next charge cycle. */
      kwh: number;
      /** The user's configured minimum-allowable SOC, %. */
      min_soc_pct: number;
      /** Lowest SOC in the uncharged projection, %. */
      observed_min_soc_pct: number;
      /** Lowest SOC across the hours the charge can influence, %. */
      after_min_soc_pct: number;
      /** Live snapshot SOC at the time the plan was computed, %. */
      current_soc_pct: number;
      rationale: string;
      /** Per-hour SOC trajectory when the recommended charge is applied
       *  during the next window, ending at the following cheap period, in the
       *  `[timestamp_unix, soc_pct]` shape used by the forecast
       *  payload's `battery.hours`. The Forecast tab's Battery
       *  projection chart draws it as a dashed line next to the
       *  solar-only projection so the user can see what enacting the
       *  recommendation actually does. Empty when the planner has
       *  nothing to offer (no_plan / no_charge_needed). */
      with_charge_series: [number, number][];
      /** Tomorrow's grid import under the recommended plan, kWh — the
       *  window's grid draw plus the residual import of tomorrow's
       *  what-if hours. Drives the Tomorrow "Expected import" tile so
       *  the tile agrees with the plan instead of the uncharged
       *  simulation. */
      import_tomorrow_with_charge_kwh: number;
      /** Tomorrow's grid export under the recommended plan, kWh. */
      export_tomorrow_with_charge_kwh: number;
    }
  | {
      kind: 'no_charge_needed';
      /** The user's configured minimum-allowable SOC, %. */
      min_soc_pct: number;
      /** Lowest SOC in the projection — stays above the minimum. */
      observed_min_soc_pct: number;
      /** Live snapshot SOC at the time the plan was computed, %. */
      current_soc_pct: number;
      rationale: string;
    }
  | { kind: 'no_plan'; reason: string };

export type PlanApply = {
  charge_slot: {
    slot: 1;
    enabled: true;
    start_hour: number;
    start_minute: number;
    end_hour: number;
    end_minute: number;
    target_soc: number;
    charge_rate_percent: 100;
  };
  timed_charge: { enabled: true };
} | null;

export type PlanResponse = {
  recommendation: PlanRecommendation;
  apply: PlanApply;
};

/** Short headline for the Plan card. Degrades gracefully per kind. */
export function forecastPlanTitle(rec: PlanRecommendation): string {
  if (rec.kind === 'charge') {
    const when = rec.window.tomorrow ? 'Tomorrow' : 'Tonight';
    return `Overnight charge — ${when} ${rec.window.start}\u2013${rec.window.end}, ${rec.kwh.toFixed(1)} kWh`;
  }
  if (rec.kind === 'no_charge_needed') {
    return `No overnight charge needed — solar covers the day`;
  }
  return `Plan not ready yet — ${rec.reason}`;
}

/**
 * Decide whether the Forecast page should refetch `/api/forecast` and
 * `/api/forecast/plan` given a snapshot change.
 *
 * Triggers (any of):
 * - SOC changed by ≥ 1 percentage point (battery projection moves meaningfully),
 * - The charge / max-battery-power register changed (rate cap shifted),
 * - ≥ `FORECAST_REFRESH_INTERVAL_MS` have elapsed since the last refresh
 *   (safety net for slow-moving data sources like the consumption profile).
 *
 * Debounced by `MIN_REFRESH_INTERVAL_MS` so a flurry of WS snapshot
 * pushes doesn't refetch more than once every few seconds.
 *
 * `null` snapshots (inverter not connected yet) count as "no change".
 *
 * Issue #283.
 */
export const FORECAST_REFRESH_INTERVAL_MS = 30_000;
export const FORECAST_MIN_REFRESH_INTERVAL_MS = 5_000;
export const FORECAST_SOC_DELTA_PCT = 1;

export function shouldRefetchForecast(
  prevSocPct: number | null,
  newSocPct: number | null,
  lastRefetchMs: number,
  nowMs: number,
  prevMaxBatteryPowerW: number,
  newMaxBatteryPowerW: number,
): boolean {
  // First snapshot arrival or capability change is always a refetch.
  if (prevSocPct === null && newSocPct !== null) return true;
  if (prevMaxBatteryPowerW !== newMaxBatteryPowerW) return true;
  if (prevSocPct === null || newSocPct === null) return false;

  const socDelta = Math.abs(prevSocPct - newSocPct);
  if (socDelta >= FORECAST_SOC_DELTA_PCT) return true;

  // Safety-net periodic refresh AND the debounce floor.
  if (nowMs - lastRefetchMs < FORECAST_MIN_REFRESH_INTERVAL_MS) return false;
  if (nowMs - lastRefetchMs >= FORECAST_REFRESH_INTERVAL_MS) return true;

  return false;
}
