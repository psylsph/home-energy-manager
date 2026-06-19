import { describe, it, expect } from 'vitest';
import { removeSpikes, isCumulativeField, SPIKE_THRESHOLDS } from './chartSeries';
import type { TimePoint } from './types';

/** Build a `TimePoint[]` from bare values at 1s spacing (ts is irrelevant to the maths). */
function series(values: number[]): TimePoint[] {
  return values.map((v, i) => ({ t: i, v }));
}

describe('isCumulativeField', () => {
  it('flags today_*_kwh daily counters as cumulative', () => {
    for (const f of [
      'today_solar_kwh',
      'today_import_kwh',
      'today_export_kwh',
      'today_charge_kwh',
      'today_discharge_kwh',
      'today_consumption_kwh',
      'today_ac_charge_kwh',
    ]) {
      expect(isCumulativeField(f), `${f} should be cumulative`).toBe(true);
    }
  });

  it('treats instantaneous rates/gauges as non-cumulative', () => {
    for (const f of [
      'solar_power',
      'pv1_power',
      'pv2_power',
      'battery_power',
      'grid_power',
      'home_power',
      'grid_voltage',
      'soc',
      // Derived chart fields are not raw counters either.
      '_import_cost',
      '_export_income',
    ]) {
      expect(isCumulativeField(f), `${f} should NOT be cumulative`).toBe(false);
    }
  });

  it('keeps SPIKE_THRESHOLDS cumulative entries consistent with the predicate', () => {
    // Every field the thresholds table marks as a kWh counter must be
    // recognised as cumulative — guards against a newly-added counter getting
    // interpolation by accident.
    for (const field of Object.keys(SPIKE_THRESHOLDS)) {
      if (/_kwh$/.test(field)) {
        expect(isCumulativeField(field), `${field}`).toBe(true);
      }
    }
  });
});

describe('removeSpikes', () => {
  it('returns the input unchanged for series shorter than 3 points', () => {
    expect(removeSpikes(series([]), 'solar_power')).toEqual(series([]));
    expect(removeSpikes(series([5]), 'solar_power')).toEqual(series([5]));
    expect(removeSpikes(series([5, 6]), 'solar_power')).toEqual(series([5, 6]));
  });

  it('never replaces the first or last point', () => {
    // 0, 10, 0 — the middle is a spike, but the endpoints stay untouched.
    const out = removeSpikes(series([0, 9999, 0]), 'solar_power');
    expect(out[0].v).toBe(0);
    expect(out[2].v).toBe(0);
  });

  // ---- Cumulative fields: carry-forward ---------------------------------

  it('carries the previous value forward for a cumulative counter spike', () => {
    // Monotonic solar counter with a corruption at index 2 (jumps to 50 then
    // back to ~3). Threshold for today_solar_kwh is 5 kWh.
    const input = series([0, 2, 50, 3, 5]);
    const out = removeSpikes(input, 'today_solar_kwh');
    // Spike (50) must be replaced by the previous GOOD value (2), NOT the
    // interpolated midpoint of 2 and 3 (= 2.5).
    expect(out[2].v).toBe(2);
    expect(out[2].v).not.toBe(2.5);
    // Monotonicity across the repaired series is preserved.
    const values = out.map((p) => p.v);
    expect(values).toEqual([0, 2, 2, 3, 5]);
  });

  it('keeps a cumulative series monotonic after a spike repair', () => {
    // [1, 2, 80, 3, 4] → spike 80 carried forward as 2 → [1,2,2,3,4] (non-decreasing).
    const out = removeSpikes(series([1, 2, 80, 3, 4]), 'today_import_kwh');
    const values = out.map((p) => p.v);
    for (let i = 1; i < values.length; i++) {
      expect(values[i]).toBeGreaterThanOrEqual(values[i - 1]);
    }
  });

  it('carries forward the previous good value across multiple isolated spikes', () => {
    // Two isolated spikes (80, 90) separated by real points. Each must be
    // repaired to the value immediately before it and the series stays
    // monotonic — never a synthetic midpoint.
    //   [1, 2, 80, 3, 90, 4] → [1, 2, 2, 3, 3, 4]
    const out = removeSpikes(series([1, 2, 80, 3, 90, 4]), 'today_charge_kwh');
    expect(out.map((p) => p.v)).toEqual([1, 2, 2, 3, 3, 4]);
  });

  it('leaves a clean cumulative series untouched', () => {
    const input = series([0, 1, 2, 3, 4]);
    expect(removeSpikes(input, 'today_solar_kwh')).toEqual(input);
  });

  // ---- Non-cumulative fields: interpolation -----------------------------

  it('interpolates (midpoint) for an instantaneous-rate spike', () => {
    // Power field, not cumulative → midpoint of neighbours (2 and 4 = 3).
    const out = removeSpikes(series([2, 9999, 4]), 'solar_power');
    expect(out[1].v).toBe(3);
  });

  it('interpolates for a SOC gauge spike', () => {
    // soc is a gauge; interpolation is the least-bad estimate.
    const out = removeSpikes(series([50, 100, 55]), 'soc');
    expect(out[1].v).toBeCloseTo(52.5);
  });

  it('leaves a clean rate series untouched', () => {
    const input = series([100, 120, 140, 130]);
    expect(removeSpikes(input, 'battery_power')).toEqual(input);
  });

  // ---- Threshold / detection mechanics ----------------------------------

  it('does not flag a legitimate step as a spike when neighbours disagree', () => {
    // A real ramp 0 → 100 → 200: dNeighbors is large, so not a spike.
    const input = series([0, 100, 200]);
    expect(removeSpikes(input, 'today_solar_kwh')).toEqual(input);
  });

  it('respects the per-field threshold', () => {
    // today_solar_kwh threshold is 5 kWh. A 4-unit jump is below threshold.
    const input = series([0, 4, 8]);
    expect(removeSpikes(input, 'today_solar_kwh')).toEqual(input);
  });

  it('uses a high default threshold for unknown fields', () => {
    // Unknown field with a 1000-unit spike — default 4000 means NOT detected.
    const input = series([0, 1000, 2]);
    expect(removeSpikes(input, 'mystery_field')).toEqual(input);
  });

  it('preserves timestamps of repaired points', () => {
    const input: TimePoint[] = [
      { t: 1000, v: 0 },
      { t: 2000, v: 50 },
      { t: 3000, v: 3 },
    ];
    const out = removeSpikes(input, 'today_solar_kwh');
    // The repaired point keeps its own timestamp (carry-forward changes only v).
    expect(out[1].t).toBe(2000);
  });

  it('does not mutate the input array', () => {
    const input = series([0, 50, 3]);
    const snapshot = input.map((p) => ({ ...p }));
    removeSpikes(input, 'today_solar_kwh');
    expect(input).toEqual(snapshot);
  });
});
