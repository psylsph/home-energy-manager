/**
 * Computes a data-driven y-axis domain that tightly bounds the recorded
 * values with `padding` on each side, instead of a 0-based auto-scale.
 *
 * Used by charts whose real-world range is narrow and sits well above zero
 * (e.g. Grid Voltage hovers around 230–240 V). With Recharts' default
 * behaviour the series is squashed flat against the top of the plot, so the
 * small day-to-day fluctuations the user actually cares about become
 * unreadable. Snapping to `[min − pad, max + pad]` (issue #152) gives the
 * line room to breathe while keeping the absolute scale honest.
 *
 * Returns `undefined` when there are no finite values (empty range, all-gap
 * data), leaving the chart's default auto-scaling in place.
 *
 * `null` / `undefined` / non-finite values are skipped — they represent
 * missing buckets where the chart should leave a gap, not a zero reading.
 */
export function computeTightDomain(
  values: Array<number | null | undefined>,
  padding: number,
): [number, number] | undefined {
  let lo = Infinity;
  let hi = -Infinity;
  for (const v of values) {
    if (v === null || v === undefined) continue;
    if (!Number.isFinite(v)) continue;
    if (v < lo) lo = v;
    if (v > hi) hi = v;
  }
  if (!Number.isFinite(lo) || !Number.isFinite(hi)) return undefined;
  return [lo - padding, hi + padding];
}
