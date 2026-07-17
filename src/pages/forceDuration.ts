/**
 * Format a duration in minutes as a human-readable label for the UI.
 *
 *   5–59 min   → "30m"
 *   60–1439    → "1h 30m" / "2h" (no trailing zero minutes)
 *   ≥ 1440     → "24h"   (the slider's upper bound; we don't display larger values)
 *
 * The 24h case is a deliberate cap: the slider's max is 1440 and the
 * backend clamps force-charge slot minutes to 1..=1439 (so 1440 on the
 * slider wraps to a 23h59 slot). Displaying "24h" instead of "23h 59m"
 * is clearer to the user.
 */
export function formatDurationLabel(minutes: number): string {
  if (!Number.isFinite(minutes) || minutes < 1) return '0m';
  if (minutes >= 1440) return '24h';
  const whole = Math.floor(minutes);
  if (whole >= 60) {
    const h = Math.floor(whole / 60);
    const m = whole % 60;
    return m === 0 ? `${h}h` : `${h}h ${m}m`;
  }
  return `${whole}m`;
}

/** Smallest, largest, and selectable increment exposed by the control. */
export const FORCE_DURATION_MIN = 5;
export const FORCE_DURATION_MAX = 1440;
export const FORCE_DURATION_STEP = 5;

/**
 * The range input represents a logarithmic track position, not minutes.
 * A reasonably fine integer domain keeps mouse/touch interaction smooth
 * while preserving useful keyboard increments.
 */
export const FORCE_DURATION_SLIDER_MAX = 1000;

/** Storage key for the persisted duration value. */
export const FORCE_DURATION_STORAGE_KEY = 'forceDurationMinutes';

/** Default duration if nothing is persisted. */
export const FORCE_DURATION_DEFAULT = 60;

/** Clamp and snap a value to the control's five-minute increments. */
export function clampDurationMinutes(value: number): number {
  if (!Number.isFinite(value)) return FORCE_DURATION_DEFAULT;
  const rounded = Math.round(value / FORCE_DURATION_STEP) * FORCE_DURATION_STEP;
  return Math.max(FORCE_DURATION_MIN, Math.min(FORCE_DURATION_MAX, rounded));
}

/** Convert minutes to a position on the logarithmic slider track. */
export function durationToSliderPosition(minutes: number): number {
  const duration = clampDurationMinutes(minutes);
  const range = Math.log(FORCE_DURATION_MAX / FORCE_DURATION_MIN);
  return Math.round(
    (Math.log(duration / FORCE_DURATION_MIN) / range) * FORCE_DURATION_SLIDER_MAX,
  );
}

/** Convert a logarithmic slider position back to a duration in minutes. */
export function sliderPositionToDuration(position: number): number {
  if (!Number.isFinite(position)) return FORCE_DURATION_DEFAULT;
  const boundedPosition = Math.max(0, Math.min(FORCE_DURATION_SLIDER_MAX, position));
  const progress = boundedPosition / FORCE_DURATION_SLIDER_MAX;
  const duration = FORCE_DURATION_MIN
    * Math.exp(Math.log(FORCE_DURATION_MAX / FORCE_DURATION_MIN) * progress);
  return clampDurationMinutes(duration);
}

/**
 * Read the persisted duration from localStorage, falling back to the
 * default if the value is missing, invalid, or out of range.
 */
export function readPersistedDuration(
  storage: Pick<Storage, 'getItem'> | null,
): number {
  if (!storage) return FORCE_DURATION_DEFAULT;
  const raw = storage.getItem(FORCE_DURATION_STORAGE_KEY);
  if (raw == null) return FORCE_DURATION_DEFAULT;
  const parsed = Number.parseInt(raw, 10);
  if (!Number.isFinite(parsed)) return FORCE_DURATION_DEFAULT;
  return clampDurationMinutes(parsed);
}
