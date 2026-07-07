import { describe, it, expect } from 'vitest';
import { percentOfRated, arrayLabel, formatPercent } from '../../src/lib/solarArrays';
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
