/** Helpers for the guided battery calibration cycle (issue #418). */

export type CalibrationOutcome =
  | { result: 'completed' }
  | { result: 'cancelled' }
  | { result: 'failed'; reason: string };

export interface CalibrationView {
  phase: 'discharging' | 'charging' | 'holding' | 'restoring';
  started_at_ms: number;
  phase_started_at_ms: number;
  hold_minutes: number;
  /** Holding: when the battery reached 100%. */
  since_ms?: number;
  /** Restoring: how the cycle will be reported once the settings are back. */
  outcome?: CalibrationOutcome;
}

export interface CalibrationRecord {
  started_at_ms: number;
  finished_at_ms: number;
  outcome: CalibrationOutcome;
}

export interface CalibrationStatus {
  ok: boolean;
  supported: boolean;
  running: boolean;
  calibration: CalibrationView | null;
  last: CalibrationRecord | null;
  default_hold_minutes: number;
  min_hold_minutes: number;
  max_hold_minutes: number;
  floor_soc: number;
}

export function describePhase(view: CalibrationView, floorSoc: number): string {
  switch (view.phase) {
    case 'discharging':
      return `Exporting down to ${floorSoc}%`;
    case 'charging':
      return 'Charging to 100%';
    case 'holding':
      return 'Holding at 100% while the cells balance';
    case 'restoring':
      return view.outcome?.result === 'cancelled'
        ? 'Cancelling: putting your settings back'
        : 'Putting your settings back';
  }
}

/** Minutes left of the hold at 100%, or null outside the hold. */
export function holdRemainingMinutes(view: CalibrationView, nowMs: number): number | null {
  if (view.phase !== 'holding' || view.since_ms == null) return null;
  const leftMs = view.since_ms + view.hold_minutes * 60_000 - nowMs;
  return Math.max(0, Math.ceil(leftMs / 60_000));
}

/** Day and time, e.g. "30 Oct, 18:00". */
export function formatCalibrationTime(ms: number): string {
  return new Date(ms).toLocaleString([], {
    day: 'numeric',
    month: 'short',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  });
}

export function describeLastCalibration(record: CalibrationRecord | null): string {
  if (!record) return '';
  const when = formatCalibrationTime(record.finished_at_ms);
  switch (record.outcome.result) {
    case 'completed':
      return `Last calibration finished ${when}.`;
    case 'cancelled':
      return `Last calibration was cancelled ${when}.`;
    case 'failed':
      return `Last calibration stopped ${when}: ${record.outcome.reason}`;
  }
}

/** A hold typed in hours, as minutes, or null when it isn't a number of hours within the limits. */
export function parseHoldHours(text: string, minMinutes: number, maxMinutes: number): number | null {
  const trimmed = text.trim();
  if (!/^\d+(\.\d+)?$/.test(trimmed)) return null;
  const minutes = Math.round(Number(trimmed) * 60);
  return minutes >= minMinutes && minutes <= maxMinutes ? minutes : null;
}
