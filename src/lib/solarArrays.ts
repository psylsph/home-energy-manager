import type { SolarArraySummary, SolarArraySource } from './types';

/** Chart colours for the DC PV strings — shared by the PV Power trend chart
 *  and the "% of max" array cards so a given string reads as one colour
 *  across the whole Solar page. The PV2 card used to be hard-coded amber and
 *  didn't match the blue PV2 series in the graph (issue #192). */
export const SOLAR_PV1_COLOR = '#F59E0B';
export const SOLAR_PV2_COLOR = '#3B82F6';

/**
 * Percentage of rated peak capacity a solar array is currently producing
 * (issue #110). Returns `null` when the array has no rated capacity
 * (`rated_kw <= 0`) so the caller can hide the % and show power-only.
 *
 * Capped at 100% only for display sanity when the dongle reports a glitch
 * spike above the array's rating — callers that want the raw ratio should
 * divide directly.
 */
export function percentOfRated(powerW: number, ratedKw: number): number | null {
  if (!ratedKw || ratedKw <= 0) return null;
  const pct = (powerW / 1000 / ratedKw) * 100;
  if (!Number.isFinite(pct)) return null;
  return pct;
}

/**
 * Overall solar production as a percentage of total configured DC-string
 * capacity (issue #192): the energy-flow wheel shows this next to the Solar
 * kW value so the user can see "how much more is possible".
 *
 * Only DC strings (`pv1` / `pv2`) count toward the denominator, matching the
 * `solar_power` the wheel displays (pv1 + pv2) — external CT-meter arrays
 * aren't part of that reading, so including their capacity would understate
 * the %. Returns `null` when no DC-string capacity is configured so the
 * caller omits the % entirely. Can exceed 100% on a bright edge-of-cloud
 * moment, mirroring `percentOfRated`.
 */
export function solarOverallPercent(
  solarPowerW: number,
  arrays: SolarArraySummary[] | undefined | null,
): number | null {
  if (!arrays) return null;
  let totalKw = 0;
  for (const a of arrays) {
    if ((a.source === 'pv1' || a.source === 'pv2') && a.rated_kw > 0) {
      totalKw += a.rated_kw;
    }
  }
  if (totalKw <= 0) return null;
  const pct = (solarPowerW / 1000 / totalKw) * 100;
  if (!Number.isFinite(pct)) return null;
  return pct;
}

/**
 * Human-readable label for a solar array, preferring the user-entered name
 * and falling back to a default derived from the array's source. Mirrors
 * the backend's "empty name → default" contract documented on
 * `SolarArraySummary.name`.
 */
export function arrayLabel(arr: SolarArraySummary): string {
  if (arr.name.trim()) return arr.name.trim();
  switch (arr.source) {
    case 'pv1':
      return 'PV1';
    case 'pv2':
      return 'PV2';
    case 'meter': {
      const addr = arr.meter_address ?? 0;
      return `Meter 0x${addr.toString(16).padStart(2, '0')}`;
    }
    default:
      return 'Solar array';
  }
}

/** Colour for a solar array's power text + progress bar. DC strings match the
 *  PV Power chart (PV1 amber, PV2 blue); external CT-meter arrays (AC-coupled)
 *  fall back to the solar amber since they have no dedicated graph series. */
export function solarArrayColor(source: SolarArraySource): string {
  return source === 'pv2' ? SOLAR_PV2_COLOR : SOLAR_PV1_COLOR;
}

/** Format a percentage value for display, e.g. `42` → "42%". Returns "—"
 *  when `null` (no rated capacity configured). */
export function formatPercent(pct: number | null): string {
  if (pct == null) return '—';
  return `${Math.round(pct)}%`;
}

/**
 * Static Y-axis ceiling (in watts) for the solar power trend chart, derived
 * from the configured DC-string nameplate capacities (issue #192).
 *
 * The user asked for the *higher* of the two PV sizes — not their sum — so
 * each string's output is read against a single string's peak. That's the
 * natural reference for "how is each string doing relative to its own
 * capacity": a 4 kWp PV1 and a 4 kWp PV2 chart against 4 kW, not 8 kW.
 *
 * Returns `null` when neither string has a rated capacity configured, so the
 * chart falls back to its data-driven max (and the Y-Lock toggle).
 */
export function solarChartNameplateCeilingW(pv1RatedKw: number, pv2RatedKw: number): number | null {
  const max = Math.max(pv1RatedKw, pv2RatedKw);
  if (!(max > 0)) return null;
  return max * 1000;
}

/**
 * Convenience wrapper: pull the DC-string nameplate ceiling straight from
 * `snapshot.solar_arrays` (issue #192). Reads only the `pv1` / `pv2` source
 * entries — the trend chart plots DC string power, so external CT-meter
 * arrays (AC-coupled) don't set its scale. Returns `null` when no DC-string
 * capacity is configured so the chart keeps its data-driven fallback.
 */
export function solarChartNameplateCeilingFromArrays(
  arrays: SolarArraySummary[] | undefined | null,
): number | null {
  if (!arrays) return null;
  let pv1 = 0;
  let pv2 = 0;
  for (const a of arrays) {
    if (a.source === 'pv1' && a.rated_kw > pv1) pv1 = a.rated_kw;
    if (a.source === 'pv2' && a.rated_kw > pv2) pv2 = a.rated_kw;
  }
  return solarChartNameplateCeilingW(pv1, pv2);
}
