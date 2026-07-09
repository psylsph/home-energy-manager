import { describe, it, expect } from 'vitest';
import {
  percentOfRated,
  arrayLabel,
  formatPercent,
  solarChartNameplateCeilingW,
  solarChartNameplateCeilingFromArrays,
  solarArrayColor,
  solarOverallPercent,
} from '../../src/lib/solarArrays';
import type { SolarArraySummary } from '../../src/lib/types';

function array(
  source: SolarArraySummary['source'],
  overrides: Partial<SolarArraySummary> = {},
): SolarArraySummary {
  return {
    source,
    name: '',
    power_w: 0,
    rated_kw: 0,
    today_kwh: null,
    meter_address: null,
    ...overrides,
  };
}

describe('percentOfRated', () => {
  it('returns the live output as a percentage of rated kWp', () => {
    // 6 kWp array producing 4.2 kW → 70%.
    expect(percentOfRated(4200, 6)).toBe(70);
    // 4.2 kWp array producing 2.1 kW → 50%.
    expect(percentOfRated(2100, 4.2)).toBeCloseTo(50, 5);
  });

  it('is null when no rated capacity is configured', () => {
    // rated_kw = 0 (default) — the caller hides the %, shows power only.
    expect(percentOfRated(4200, 0)).toBeNull();
  });

  it('is null for a negative/zero rating so a bad config never inverts the display', () => {
    expect(percentOfRated(4200, -5)).toBeNull();
  });

  it('can exceed 100% when generation outpaces the rated kWp', () => {
    // Arrays can momentarily produce above nameplate on a bright edge-of-
    // cloud day; the % should reflect reality, not clamp silently.
    expect(percentOfRated(6500, 6)).toBeCloseTo(108.33, 1);
  });

  it('is null for non-finite input (NaN power from a decode glitch)', () => {
    expect(percentOfRated(NaN, 6)).toBeNull();
  });
});

describe('arrayLabel', () => {
  it('prefers the user-entered name when set', () => {
    expect(arrayLabel(array('pv1', { name: 'East roof' }))).toBe('East roof');
  });

  it('trims surrounding whitespace from a custom name', () => {
    expect(arrayLabel(array('meter', { name: '  Garage  ' }))).toBe('Garage');
  });

  it('falls back to PV1 / PV2 for DC strings with no name', () => {
    expect(arrayLabel(array('pv1'))).toBe('PV1');
    expect(arrayLabel(array('pv2'))).toBe('PV2');
  });

  it('falls back to a hex meter address for unnamed CT-meter arrays', () => {
    expect(arrayLabel(array('meter', { meter_address: 1 }))).toBe('Meter 0x01');
    expect(arrayLabel(array('meter', { meter_address: 8 }))).toBe('Meter 0x08');
  });
});

describe('formatPercent', () => {
  it('rounds to a whole percent', () => {
    expect(formatPercent(70.4)).toBe('70%');
    expect(formatPercent(70.5)).toBe('71%');
  });

  it('renders an em dash when the percentage is null (no rating)', () => {
    expect(formatPercent(null)).toBe('—');
  });
});

describe('solarArrayColor', () => {
  it('uses the PV Power chart colours so a string reads the same everywhere', () => {
    // PV1 amber, PV2 blue — matching the trend graph's series colours.
    expect(solarArrayColor('pv1')).toBe('#F59E0B');
    expect(solarArrayColor('pv2')).toBe('#3B82F6');
  });

  it('falls back to solar amber for external CT-meter arrays', () => {
    // AC-coupled meter arrays have no dedicated graph series.
    expect(solarArrayColor('meter')).toBe('#F59E0B');
  });
});

describe('solarOverallPercent', () => {
  it('returns total solar output as a % of total DC-string capacity', () => {
    // 4 kWp PV1 + 4 kWp PV2 (8 kWp total) producing 6 kW → 75%.
    const arrays = [array('pv1', { rated_kw: 4 }), array('pv2', { rated_kw: 4 })];
    expect(solarOverallPercent(6000, arrays)).toBe(75);
  });

  it('sums only the DC strings (pv1/pv2), ignoring external CT meters', () => {
    // The wheel's solar_power reading (pv1 + pv2) doesn't include an external
    // AC-coupled array, so its 10 kWp must not dilute the denominator.
    const arrays = [array('pv1', { rated_kw: 6 }), array('meter', { rated_kw: 10, meter_address: 1 })];
    expect(solarOverallPercent(3000, arrays)).toBe(50); // 3 kW / 6 kWp
  });

  it('is null when no DC-string capacity is configured', () => {
    expect(solarOverallPercent(3000, [array('meter', { rated_kw: 10, meter_address: 1 })])).toBeNull();
    expect(solarOverallPercent(3000, [])).toBeNull();
    expect(solarOverallPercent(3000, undefined)).toBeNull();
    expect(solarOverallPercent(3000, null)).toBeNull();
  });

  it('can exceed 100% when generation outpaces nameplate', () => {
    const arrays = [array('pv1', { rated_kw: 4 }), array('pv2', { rated_kw: 4 })];
    // 10 kW from 8 kWp → 125%.
    expect(solarOverallPercent(10000, arrays)).toBe(125);
  });

  it('is null for non-finite power (NaN from a decode glitch)', () => {
    expect(solarOverallPercent(NaN, [array('pv1', { rated_kw: 4 })])).toBeNull();
  });
});

describe('solarChartNameplateCeilingW', () => {
  it('returns the higher of the two PV sizes in watts (NOT their sum)', () => {
    // 4 kWp PV1 + 4 kWp PV2 → ceiling is one string's peak (4 kW), not 8.
    expect(solarChartNameplateCeilingW(4, 4)).toBe(4000);
    // 6 kWp PV1 + 4.2 kWp PV2 → the larger string sets the scale.
    expect(solarChartNameplateCeilingW(6, 4.2)).toBe(6000);
    expect(solarChartNameplateCeilingW(4.2, 6)).toBe(6000);
  });

  it('uses the single configured string when only one is set', () => {
    expect(solarChartNameplateCeilingW(6, 0)).toBe(6000);
    expect(solarChartNameplateCeilingW(0, 4.2)).toBe(4200);
  });

  it('is null when neither string has a rated capacity', () => {
    expect(solarChartNameplateCeilingW(0, 0)).toBeNull();
  });

  it('is null for negative/garbage ratings so a bad config never sets a bogus scale', () => {
    expect(solarChartNameplateCeilingW(-5, -3)).toBeNull();
  });
});

describe('solarChartNameplateCeilingFromArrays', () => {
  it('reads the higher DC-string kWp from snapshot.solar_arrays', () => {
    const arrays = [array('pv1', { rated_kw: 6 }), array('pv2', { rated_kw: 4.2 })];
    expect(solarChartNameplateCeilingFromArrays(arrays)).toBe(6000);
  });

  it('uses the single DC string when only PV1 is configured', () => {
    expect(solarChartNameplateCeilingFromArrays([array('pv1', { rated_kw: 5 })])).toBe(5000);
  });

  it('ignores external CT-meter arrays (the chart plots DC string power)', () => {
    // A 10 kWp AC-coupled meter array must not set the DC-string chart scale.
    expect(solarChartNameplateCeilingFromArrays([array('meter', { rated_kw: 10, meter_address: 1 })])).toBeNull();
  });

  it('is null when no DC-string capacity is configured', () => {
    expect(solarChartNameplateCeilingFromArrays([])).toBeNull();
    expect(solarChartNameplateCeilingFromArrays(undefined)).toBeNull();
    expect(solarChartNameplateCeilingFromArrays(null)).toBeNull();
  });
});
