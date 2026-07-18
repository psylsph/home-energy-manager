export const LOGARITHMIC_RANGE_MAX = 1000;

function validateRange(min: number, max: number, step: number, trackMax: number): void {
  if (!Number.isFinite(min) || min <= 0 || !Number.isFinite(max) || max <= min) {
    throw new RangeError('Logarithmic ranges require 0 < min < max');
  }
  if (!Number.isFinite(step) || step <= 0 || !Number.isFinite(trackMax) || trackMax <= 0) {
    throw new RangeError('Logarithmic range step and track maximum must be positive');
  }
}

/** Convert a user-facing value to a position on a logarithmic range track. */
export function logarithmicValueToPosition(
  value: number,
  min: number,
  max: number,
  trackMax = LOGARITHMIC_RANGE_MAX,
): number {
  validateRange(min, max, 1, trackMax);
  const boundedValue = Number.isFinite(value) ? Math.max(min, Math.min(max, value)) : min;
  return Math.round((Math.log(boundedValue / min) / Math.log(max / min)) * trackMax);
}

/** Convert a logarithmic track position to a snapped user-facing value. */
export function logarithmicPositionToValue(
  position: number,
  min: number,
  max: number,
  step = 1,
  trackMax = LOGARITHMIC_RANGE_MAX,
): number {
  validateRange(min, max, step, trackMax);
  const boundedPosition = Number.isFinite(position)
    ? Math.max(0, Math.min(trackMax, position))
    : 0;
  const value = min * Math.exp(Math.log(max / min) * boundedPosition / trackMax);
  const snapped = Math.round(value / step) * step;
  return Math.max(min, Math.min(max, snapped));
}
