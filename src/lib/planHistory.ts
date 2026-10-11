/**
 * Pure helpers for the Planner's "past" view (issue #361): stitching the
 * recorded planner decisions into the lines the planner expected, setting
 * them against what the battery and the sun actually did, and summarising
 * the gap. No fetching, no React.
 */

import { forecastPlanTitle, relabelToStateInstants } from './forecast';
import type { ForecastData, PlanResponse } from './forecast';

const HOUR = 3600;

/** One stored planner decision, as `GET /api/forecast/plan-history` serves
 *  it. `plan` or `forecast` is null when the stored JSON could not be read. */
export type PlanHistorySnapshot = {
  recorded_at: number;
  /** What recorded it: `before_window`, `auto_apply` or `refresh`. */
  kind: string;
  strategy: string;
  plan: PlanResponse | null;
  forecast: ForecastData | null;
};

/** A history point as `/api/history` returns it: milliseconds and a value. */
export type HistoryPoint = { t: number; v: number };

/** A `[unix seconds, value]` point; null breaks the line. */
export type SeriesPoint = [number, number | null];

export type PastRange = 'off' | '24h' | '3d' | '7d';

/** The choices offered by the Planner's "Show past" control. */
export const PAST_RANGES: { key: PastRange; label: string }[] = [
  { key: 'off', label: 'Off' },
  { key: '24h', label: '24 hours' },
  { key: '3d', label: '3 days' },
  { key: '7d', label: '7 days' },
];

/** How far back a range reaches, in seconds (0 for Off). */
export function pastRangeSeconds(range: PastRange): number {
  switch (range) {
    case '24h':
      return 24 * HOUR;
    case '3d':
      return 3 * 24 * HOUR;
    case '7d':
      return 7 * 24 * HOUR;
    default:
      return 0;
  }
}

/** The chart's x-axis domain: the forward axis, pulled back by the range. */
export function pastAxisDomain(
  range: PastRange,
  forwardStart: number,
  forwardEnd: number,
): [number, number] {
  return [forwardStart - pastRangeSeconds(range), forwardEnd];
}

const byRecordedAt = (a: PlanHistorySnapshot, b: PlanHistorySnapshot) =>
  a.recorded_at - b.recorded_at;

const floorHour = (ts: number) => Math.floor(ts / HOUR) * HOUR;

/** The SOC trajectory a stored decision expected: the what-if line when it
 *  was a charge plan, otherwise the solar-only projection. Anchored at the
 *  live SOC the plan started from, with the end-of-bucket states re-labelled
 *  to the instants they hold. */
function expectedSocSeries(snapshot: PlanHistorySnapshot): [number, number][] {
  const forecast = snapshot.forecast;
  const battery = forecast?.battery;
  if (!forecast || !battery) return [];
  const recommendation = snapshot.plan?.recommendation;
  const withCharge =
    recommendation?.kind === 'charge' ? recommendation.with_charge_series : [];
  const raw = withCharge.length > 0 ? withCharge : battery.hours;
  if (raw.length === 0) return [];
  return [
    [forecast.generated_at, battery.start_soc_pct],
    ...relabelToStateInstants(raw),
  ];
}

/** The planner's expected battery SOC over the past: each decision's line
 *  runs from when it was made until the next decision replaced it (or now),
 *  with a null between decisions so they are not joined into one line. */
export function stitchPlannedBattery(
  snapshots: PlanHistorySnapshot[],
  nowSec: number,
): SeriesPoint[] {
  const sorted = [...snapshots].sort(byRecordedAt);
  const out: SeriesPoint[] = [];
  sorted.forEach((snapshot, index) => {
    const next = sorted[index + 1];
    const series = expectedSocSeries(snapshot).filter(([ts]) =>
      next ? ts < next.recorded_at && ts <= nowSec : ts <= nowSec,
    );
    if (series.length === 0) return;
    if (out.length > 0) {
      // Break the line from the previous decision.
      const last = out[out.length - 1];
      if (last[1] !== null) out.push([last[0] + 1, null]);
    }
    out.push(...series);
  });
  // A break left dangling by a trailing decision with nothing to draw.
  while (out.length > 0 && out[out.length - 1][1] === null) out.pop();
  return out;
}

/** The solar forecast each completed past hour was made under: the latest
 *  decision recorded at or before that hour. kWh per hour, hour-aligned. */
export function stitchPlannedSolar(
  snapshots: PlanHistorySnapshot[],
  nowSec: number,
): [number, number][] {
  const sorted = [...snapshots].sort(byRecordedAt);
  const out: [number, number][] = [];
  sorted.forEach((snapshot, index) => {
    const next = sorted[index + 1];
    const start = floorHour(snapshot.recorded_at);
    const end = next ? floorHour(next.recorded_at) : Infinity;
    for (const hour of snapshot.forecast?.solar ?? []) {
      const ts = floorHour(hour.timestamp);
      if (ts < start || ts >= end || ts + HOUR > nowSec) continue;
      out.push([ts, hour.kwh]);
    }
  });
  return out.sort((a, b) => a[0] - b[0]);
}

/** Measured battery SOC from `/api/history` points (milliseconds), limited
 *  to the visible window. */
export function actualSocSeries(
  points: HistoryPoint[] | undefined,
  fromSec: number,
  toSec: number,
): [number, number][] {
  const out: [number, number][] = [];
  for (const point of points ?? []) {
    const ts = point.t / 1000;
    if (Number.isFinite(point.v) && ts >= fromSec && ts <= toSec) out.push([ts, point.v]);
  }
  return out;
}

/** Measured solar generation per completed hour, kWh, from `/api/history`
 *  power points (watts): the mean power in an hour is that hour's energy. */
export function actualSolarKwhByHour(
  points: HistoryPoint[] | undefined,
  nowSec: number,
): [number, number][] {
  const hours = new Map<number, { sum: number; count: number }>();
  for (const point of points ?? []) {
    if (!Number.isFinite(point.v)) continue;
    const hour = floorHour(point.t / 1000);
    if (hour + HOUR > nowSec) continue;
    const entry = hours.get(hour) ?? { sum: 0, count: 0 };
    entry.sum += point.v;
    entry.count += 1;
    hours.set(hour, entry);
  }
  return [...hours.entries()]
    .sort((a, b) => a[0] - b[0])
    .map(([hour, { sum, count }]) => [hour, sum / count / 1000]);
}

/** How far a reading may be from a planned instant and still stand for it. */
const MAX_READING_GAP_SECS = HOUR;

export type BatteryComparison = {
  /** Planned instants that had a reading nearby. */
  points: number;
  /** Average size of the miss, in SOC percentage points. */
  meanAbsErrorPct: number;
  /** Average signed miss (reality − plan): positive means the battery
   *  finished higher than planned. */
  biasPct: number;
};

/** Compare the planned SOC with the measured SOC at each planned instant.
 *  The first point of every decision is where it started from — the live
 *  SOC itself — so it is left out rather than allowed to flatter the plan. */
export function compareBattery(
  planned: SeriesPoint[],
  actual: [number, number][],
): BatteryComparison | null {
  if (actual.length === 0) return null;
  const readings = [...actual].sort((a, b) => a[0] - b[0]);
  let count = 0;
  let absSum = 0;
  let signedSum = 0;
  let startOfDecision = true;
  for (const [ts, soc] of planned) {
    if (soc === null) {
      startOfDecision = true;
      continue;
    }
    if (startOfDecision) {
      startOfDecision = false;
      continue;
    }
    const nearest = nearestReading(readings, ts);
    if (nearest === null || Math.abs(nearest[0] - ts) > MAX_READING_GAP_SECS) continue;
    const error = nearest[1] - soc;
    count += 1;
    absSum += Math.abs(error);
    signedSum += error;
  }
  if (count === 0) return null;
  return { points: count, meanAbsErrorPct: absSum / count, biasPct: signedSum / count };
}

/** The reading closest in time to `ts`, from readings sorted by time. */
function nearestReading(
  sorted: [number, number][],
  ts: number,
): [number, number] | null {
  let low = 0;
  let high = sorted.length;
  while (low < high) {
    const mid = (low + high) >> 1;
    if (sorted[mid][0] < ts) low = mid + 1;
    else high = mid;
  }
  const after = sorted[low];
  const before = sorted[low - 1];
  if (!after) return before ?? null;
  if (!before) return after;
  return ts - before[0] <= after[0] - ts ? before : after;
}

export type SolarComparison = {
  hours: number;
  forecastKwh: number;
  actualKwh: number;
};

/** Total the forecast and the measured generation over the hours that have
 *  both, so a hour with no reading does not look like a shortfall. */
export function compareSolar(
  forecast: [number, number][],
  actual: [number, number][],
): SolarComparison | null {
  const measured = new Map(actual.map(([ts, kwh]) => [floorHour(ts), kwh]));
  let hours = 0;
  let forecastKwh = 0;
  let actualKwh = 0;
  for (const [ts, kwh] of forecast) {
    const reading = measured.get(floorHour(ts));
    if (reading === undefined) continue;
    hours += 1;
    forecastKwh += kwh;
    actualKwh += reading;
  }
  return hours === 0 ? null : { hours, forecastKwh, actualKwh };
}

/** Fold named `[ts, value]` series into chart rows keyed by timestamp,
 *  alongside the base rows, sorted by time. */
export function mergeSeries<Row extends { timestamp: number }>(
  base: Row[],
  series: Record<string, SeriesPoint[]>,
): Array<Row & Record<string, number | null | undefined>> {
  const rows = new Map<number, Row & Record<string, number | null | undefined>>();
  for (const row of base) rows.set(row.timestamp, { ...row });
  for (const [key, points] of Object.entries(series)) {
    for (const [ts, value] of points) {
      const row = rows.get(ts) ?? ({ timestamp: ts } as Row);
      (row as Record<string, unknown>)[key] = value;
      rows.set(ts, row as Row & Record<string, number | null | undefined>);
    }
  }
  return [...rows.values()].sort((a, b) => a.timestamp - b.timestamp);
}

const KIND_LABELS: Record<string, string> = {
  before_window: 'Before the cheap window',
  auto_apply: 'Auto-applied',
  refresh: 'Refreshed',
};

export type DecisionRow = {
  recordedAt: number;
  /** The raw kind, so two decisions in one second stay distinguishable. */
  kind: string;
  kindLabel: string;
  title: string;
  rationale: string | null;
};

/** The recorded decisions as a readable list, newest first, so the reason
 *  behind each night's plan can be read back. */
export function decisionRows(snapshots: PlanHistorySnapshot[]): DecisionRow[] {
  return [...snapshots].sort(byRecordedAt).reverse().map((snapshot) => {
    const recommendation = snapshot.plan?.recommendation;
    let rationale: string | null = null;
    if (recommendation) {
      rationale =
        recommendation.kind === 'no_plan' ? recommendation.reason : recommendation.rationale;
    }
    return {
      recordedAt: snapshot.recorded_at,
      kind: snapshot.kind,
      kindLabel: KIND_LABELS[snapshot.kind] ?? snapshot.kind,
      title: recommendation ? forecastPlanTitle(recommendation) : 'Plan unavailable',
      rationale,
    };
  });
}

/** `/api/history` request for the measured SOC and solar power over the
 *  visible past. The explicit window sets the exact span; the range only
 *  picks the bucket size, and the endpoint has no 3-day one, so 3 days use
 *  the 7-day (half-hour) buckets. */
export function pastHistoryPath(range: PastRange, nowSec: number): string {
  const params = new URLSearchParams({
    range: range === '24h' ? '24h' : '7d',
    fields: 'soc,solar_power',
    start_ms: String((nowSec - pastRangeSeconds(range)) * 1000),
    end_ms: String(nowSec * 1000),
  });
  return `/api/history?${params}`;
}

/** `/api/forecast/plan-history` request for the decisions in the visible
 *  past. `to` is exclusive, so a decision made this second is included. */
export function planHistoryPath(range: PastRange, nowSec: number): string {
  const params = new URLSearchParams({
    from: String(nowSec - pastRangeSeconds(range)),
    to: String(nowSec + 1),
  });
  return `/api/forecast/plan-history?${params}`;
}

/** Spacing of the x-axis labels: longer views are labelled daily so the
 *  axis stays readable. */
export function pastTickStepSeconds(range: PastRange): number {
  return pastRangeSeconds(range) > 24 * HOUR ? 24 * HOUR : 12 * HOUR;
}
