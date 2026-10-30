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
    void view; void floorSoc; return '';
}

/** Minutes left of the hold at 100%, or null outside the hold. */
export function holdRemainingMinutes(view: CalibrationView, nowMs: number): number | null {
    void view; void nowMs; return null;
}

/** Day and time, e.g. "30 Oct, 18:00". */
export function formatCalibrationTime(ms: number): string {
    void ms; return '';
}

export function describeLastCalibration(record: CalibrationRecord | null): string {
    void record; return '';
}

/** A hold typed in hours, as minutes, or null when it isn't a number of hours within the limits. */
export function parseHoldHours(text: string, minMinutes: number, maxMinutes: number): number | null {
    void text; void minMinutes; void maxMinutes; return null;
}
