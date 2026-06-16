import type { TimePoint } from './types';

export function getSeriesOpacity(muted: boolean): number {
  return muted ? 0.22 : 1;
}

/**
 * Per-field spike detection thresholds. A point is considered a spike if its
 * value differs from both neighbors by more than the threshold while the
 * neighbors differ by less than half the threshold.
 */
export const SPIKE_THRESHOLDS: Record<string, number> = {
  soc: 15,
  solar_power: 4000,
  pv1_power: 4000,
  pv2_power: 4000,
  battery_power: 4000,
  grid_power: 4000,
  home_power: 4000,
  // Daily energy counters (kWh) — these are cumulative monotonic counters
  // used to derive cost via delta computation. Even a small corruption of
  // 3-5 kWh produces a ~£1 cost spike at standard tariff rates.
  // Threshold: 5 kWh is generous (10 kW sustained for 30 min), but tight
  // enough to catch the 10-50 kWh corruptions common from the dongle.
  today_solar_kwh: 5,
  today_import_kwh: 5,
  today_export_kwh: 5,
  today_charge_kwh: 5,
  today_discharge_kwh: 5,
  today_consumption_kwh: 5,
};

/**
 * Replace single-point spikes with the average of their neighbours. Shared
 * between the History page charts and any other chart that renders raw polled
 * series (e.g. the Battery tab's today-SOC chart). Keeps post-query spike
 * filtering consistent everywhere a series is drawn.
 */
export function removeSpikes(points: TimePoint[], field: string): TimePoint[] {
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
