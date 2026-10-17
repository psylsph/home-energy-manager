import { formatPowerLimitLabel } from './powerLimit';

/**
 * Read-only "what the inverter last reported" line shown beside a charge
 * setting (issue #362). It is derived from the snapshot alone, never from a
 * local draft, so an unsaved slider cannot make it claim a value the inverter
 * does not hold.
 */

/**
 * Age after which a snapshot is shown as "as last read" rather than current:
 * one default 60 s poll cycle plus 30 s for the cycle's own reads. A single
 * cycle (the old 60 s) called healthy readings stale just before each new one
 * landed. It must stay below the browser's connection watchdog
 * (`SNAPSHOT_STALE_AFTER_MS` in useWebSocket, 120 s), which clears the
 * snapshot: past that the page shows "unknown" and this state is never seen.
 * A test holds the two in that order.
 */
export const LIVE_READBACK_STALE_AFTER_SECONDS = 90;

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
  // Whole elapsed seconds, compared as they are shown: a reading 90.9 s old
  // is not called stale at a 90 s threshold, and a part second (or part
  // minute) is never shown as the next one.
  const ageSeconds = Math.floor(Math.max(0, (nowMs - receivedAtMs) / 1000));
  if (ageSeconds > staleAfterSeconds) {
    return { kind: 'stale', label, ageSeconds };
  }
  return { kind: 'live', label };
}

/** Human-readable age: seconds under two minutes, so a reading just past the
 *  90 s threshold reads "95s ago" rather than "1 min ago" (younger readings
 *  are shown as live); whole elapsed minutes after that (119 s is still
 *  seconds, 150 s is "2 min ago", never rounded up). */
function formatAge(ageSeconds: number): string {
  if (ageSeconds < 120) return `${ageSeconds}s ago`;
  return `${Math.floor(ageSeconds / 60)} min ago`;
}

/** The sentence shown beside a charge setting for a given readout. */
export function describeLiveReadout(readout: LiveReadout): string {
  if (readout.kind === 'unknown') return 'Inverter Charge Rate: Unknown';
  if (readout.kind === 'live') return `Inverter Charge Rate: ${readout.label}`;
  return `Inverter Charge Rate: ${readout.label} · as last read ${formatAge(readout.ageSeconds)}`;
}
