import type { SolarArraySummary } from './types';

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

/** Format a percentage value for display, e.g. `42` → "42%". Returns "—"
 *  when `null` (no rated capacity configured). */
export function formatPercent(pct: number | null): string {
  if (pct == null) return '—';
  return `${Math.round(pct)}%`;
}
