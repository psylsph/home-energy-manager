/**
 * Format a duration in minutes as a human-readable label for the UI.
 *
 *   1–59 min   → "30m"
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

/** Clamp a value to the slider's valid range (1..=1440 minutes). */
export function clampDurationMinutes(value: number): number {
  if (!Number.isFinite(value)) return 30;
  return Math.max(1, Math.min(1440, Math.round(value)));
}

/** Storage key for the persisted duration value. */
export const FORCE_DURATION_STORAGE_KEY = 'forceDurationMinutes';

/** Default duration if nothing is persisted. */
export const FORCE_DURATION_DEFAULT = 30;

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
