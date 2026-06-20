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
  home_energy_today_kwh: 5,
};

/**
 * Cumulative monotonic daily counters. These reset to ~0 at midnight and
 * otherwise only ever increase. A spike in such a counter must be repaired
 * by **carrying the previous value forward**, not by interpolating between
 * neighbours — interpolation would invent a value that is neither a real
 * reading nor monotonic, which corrupts any per-bucket delta/cost derived
 * from the counter. This mirrors the backend sanitizer, which falls back to
 * the previous reading when a `today_*_kwh` value is out of range.
 *
 * Identified by the `today_*_kwh` naming convention so newly-added daily
 * counters are handled automatically. `home_energy_today_kwh` is also
 * cumulative (integrated from `home_power` in the sanitizer) but uses a
 * different prefix, so it's matched explicitly. Instantaneous rates/gauges
 * (power, voltage, SOC) stay on interpolation, where a midpoint is the
 * least-bad estimate.
 */
export function isCumulativeField(field: string): boolean {
  return /^today_.*_kwh$/.test(field) || field === 'home_energy_today_kwh';
}

/**
 * Replace single-point spikes with the average of their neighbours. Shared
 * between the History page charts and any other chart that renders raw polled
 * series (e.g. the Battery tab's today-SOC chart). Keeps post-query spike
 * filtering consistent everywhere a series is drawn.
 *
 * For cumulative counters ([`isCumulativeField`]) a detected spike is instead
 * replaced by the last accepted value (carry-forward) so the series stays
 * monotonic. `lastGoodV` tracks that baseline; a detected spike carries it
 * forward unchanged, so across multiple isolated spikes each repair anchors
 * to the most recent good reading rather than inventing a value.
 */
export function removeSpikes(points: TimePoint[], field: string): TimePoint[] {
  if (points.length < 3) return points;
  const threshold = SPIKE_THRESHOLDS[field] ?? 4000;
  const cumulative = isCumulativeField(field);
  const result: TimePoint[] = [];
  // Baseline for cumulative carry-forward; updated on every accepted point.
  // (The detector only fires for isolated spikes whose neighbours agree, so
  // the previous raw point is always a real reading — lastGoodV just makes
  // the carry-forward explicit and robust.)
  let lastGoodV = points[0].v;
  for (let i = 0; i < points.length; i++) {
    if (i === 0 || i === points.length - 1) {
      result.push(points[i]);
      lastGoodV = points[i].v;
      continue;
    }
    const prev = points[i - 1];
    const cur = points[i];
    const next = points[i + 1];
    const dPrev = Math.abs(cur.v - prev.v);
    const dNext = Math.abs(cur.v - next.v);
    const dNeighbors = Math.abs(next.v - prev.v);
    if (dPrev > threshold && dNext > threshold && dNeighbors < threshold * 0.5) {
      const replacement = cumulative ? lastGoodV : (prev.v + next.v) / 2;
      result.push({ t: cur.t, v: replacement });
    } else {
      result.push(cur);
      lastGoodV = cur.v;
    }
  }
  return result;
}
