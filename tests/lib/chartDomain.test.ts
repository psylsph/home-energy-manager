import { describe, it, expect } from 'vitest';
import { computeTightDomain } from '../../src/lib/chartDomain';

/**
 * Tests for `computeTightDomain` — the pure helper that powers the tight
 * y-axis scaling on the History page's Grid Voltage (V) chart (issue #152).
 *
 * Grid voltage sits in a narrow band (~230–240 V), so a 0-based axis
 * compresses the line and hides the fluctuations users care about. The
 * helper returns `[min − pad, max + pad]` so the line gets room to breathe.
 */
describe('computeTightDomain', () => {
  it('bounds the domain to min/max of the data ± padding', () => {
    const domain = computeTightDomain([230, 235, 242], 10);
    expect(domain).toEqual([220, 252]);
  });

  it('uses a single value as both bounds when all points are equal', () => {
    // A flat period: every reading is 232 V. Domain becomes [222, 242].
    const domain = computeTightDomain([232, 232, 232], 10);
    expect(domain).toEqual([222, 242]);
  });

  it('handles a single data point', () => {
    expect(computeTightDomain([240], 10)).toEqual([230, 250]);
  });

  it('respects arbitrary padding values', () => {
    // Tighter padding keeps more of the plot focused on the line.
    expect(computeTightDomain([100, 200], 5)).toEqual([95, 205]);
    expect(computeTightDomain([100, 200], 0)).toEqual([100, 200]);
  });

  it('skips null and undefined values (missing buckets)', () => {
    // Gaps in the series must not pull the min/max toward zero.
    const domain = computeTightDomain([230, null, undefined, 242], 10);
    expect(domain).toEqual([220, 252]);
  });

  it('skips non-finite values (NaN / Infinity)', () => {
    // NaN comes from derived fields with missing inputs (see temperatureChart).
    const domain = computeTightDomain([228, Number.NaN, Number.POSITIVE_INFINITY, 241], 10);
    expect(domain).toEqual([218, 251]);
  });

  it('supports negative values (e.g. below-zero temperatures)', () => {
    expect(computeTightDomain([-5, 2], 3)).toEqual([-8, 5]);
  });

  it('returns undefined for an empty input (no data in range)', () => {
    // Lets the chart fall back to its default auto-scaling.
    expect(computeTightDomain([], 10)).toBeUndefined();
  });

  it('returns undefined when every value is missing', () => {
    expect(computeTightDomain([null, undefined, Number.NaN], 10)).toBeUndefined();
  });

  it('works with floating-point readings', () => {
    const domain = computeTightDomain([229.7, 240.3], 10);
    expect(domain![0]).toBeCloseTo(219.7, 5);
    expect(domain![1]).toBeCloseTo(250.3, 5);
  });

  it('handles a realistic bucketed voltage series', () => {
    // A day of grid voltage readings.
    const values = Array.from({ length: 96 }, (_, i) => 230 + Math.sin(i / 10) * 5);
    const domain = computeTightDomain(values, 10);
    const min = Math.min(...values);
    const max = Math.max(...values);
    expect(domain).toEqual([min - 10, max + 10]);
  });
});
