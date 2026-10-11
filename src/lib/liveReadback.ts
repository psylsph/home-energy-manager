import { formatPowerLimitLabel } from './powerLimit';

/**
 * Read-only "what the inverter last reported" line shown beside a charge
 * setting (issue #362). It is derived from the snapshot alone, never from a
 * local draft, so an unsaved slider cannot make it claim a value the inverter
 * does not hold.
 */

/** Age after which a snapshot is shown as "as last read" rather than current. */
export const LIVE_READBACK_STALE_AFTER_SECONDS = 60;

export type LiveReadout =
  | { kind: 'live'; label: string }
  | { kind: 'stale'; label: string; ageSeconds: number }
  | { kind: 'unknown' };

export interface LiveReadoutInput {
  /**
   * Whether the inverter is connected now. A cached snapshot replayed while it
   * is offline is not a current reading, so nothing is shown as current then.
   */
  connected: boolean;
  /** Percentage the register holds, already snapped to what the inverter will hold. */
  percent: number | null | undefined;
  /** Watts for that percentage, or `null` when the maximum is unknown. */
  watts: number | null;
  /**
   * When this browser received the snapshot, in ms on the same clock as
   * `nowMs`. Using receipt time keeps the age independent of the host clock.
   */
  receivedAtMs: number | null | undefined;
  /** Current time in milliseconds, injected so callers and tests control the clock. */
  nowMs: number;
  staleAfterSeconds?: number;
}

/**
 * Decide how to present the inverter's reported value.
 *
 * - `unknown` when there is no percentage, or nothing has been received: a
 *   value without a time cannot be called current, and a guess would be worse
 *   than saying so.
 * - `stale` when the snapshot is older than the threshold; the age is returned
 *   so the page can say how old it is.
 * - `live` otherwise. A timestamp ahead of the clock (small skew) counts as live.
 */
export function liveReadout({
  connected,
  percent,
  watts,
  receivedAtMs,
  nowMs,
  staleAfterSeconds = LIVE_READBACK_STALE_AFTER_SECONDS,
}: LiveReadoutInput): LiveReadout {
  if (!connected) return { kind: 'unknown' };
  if (percent == null || !Number.isFinite(percent)) return { kind: 'unknown' };
  if (receivedAtMs == null || !Number.isFinite(receivedAtMs)) return { kind: 'unknown' };
  const label = formatPowerLimitLabel(percent, watts);
  // Round first: the age is compared as the whole seconds it is shown as, so
  // a reading that displays as 60 s is not called stale at the 60 s threshold.
  const ageSeconds = Math.round(Math.max(0, (nowMs - receivedAtMs) / 1000));
  if (ageSeconds > staleAfterSeconds) {
    return { kind: 'stale', label, ageSeconds };
  }
  return { kind: 'live', label };
}

/** Human-readable age: seconds under a minute, whole minutes after that. */
function formatAge(ageSeconds: number): string {
  if (ageSeconds < 60) return `${ageSeconds}s ago`;
  return `${Math.round(ageSeconds / 60)} min ago`;
}

/** The sentence shown beside a charge setting for a given readout. */
export function describeLiveReadout(readout: LiveReadout): string {
  if (readout.kind === 'unknown') return 'Inverter Charge Rate: unknown';
  if (readout.kind === 'live') return `Inverter Charge Rate: ${readout.label}`;
  return `Inverter Charge Rate: ${readout.label} · as last read ${formatAge(readout.ageSeconds)}`;
}
