import { describe, it, expect } from 'vitest';
import {
  carPower,
  effectiveVoltage,
  estimateCarPower,
  isCarThresholdKw,
  isUsableVoltage,
  MIN_THRESHOLD_KW,
  MAX_THRESHOLD_KW,
} from '../../src/lib/carCharging';
import fixture from '../fixtures/car-charging-rule.json';

describe('carPower (mirror of history/car_charging.rs, issue #454)', () => {
  it.each(fixture.cases)('$name', (c) => {
    expect(carPower(c.home_w, c.grid_v, c.threshold_w, c.amps)).toBeCloseTo(c.expected_w, 9);
  });

  it('is never the car for a corrupt load', () => {
    for (const h of [NaN, Infinity, -Infinity]) {
      expect(carPower(h, 230, 3000, 32)).toBe(0);
    }
  });

  it('keeps car + house equal to home for any load', () => {
    for (const h of [0, 1, 2999, 3000, 3001, 7360, 7361, 15000]) {
      for (const v of [null, 210, 230, 250]) {
        const car = carPower(h, v, 3000, 32);
        expect(car).toBeGreaterThanOrEqual(0);
        expect(car).toBeLessThanOrEqual(h);
      }
    }
  });
});

describe('isUsableVoltage / effectiveVoltage (the voltage the rule uses)', () => {
  it('uses a reading inside the sanitizer range, and 230 V otherwise', () => {
    for (const v of [180, 230, 240.5, 280]) {
      expect(isUsableVoltage(v)).toBe(true);
      expect(effectiveVoltage(v)).toBe(v);
    }
    for (const v of [null, undefined, NaN, Infinity, 179.9, 280.1]) {
      expect(isUsableVoltage(v)).toBe(false);
      expect(effectiveVoltage(v)).toBe(230);
    }
  });
});

describe('estimateCarPower (from the settings)', () => {
  it('is null with no threshold, so the feature is off', () => {
    expect(estimateCarPower(9000, 230, { forecast_exclude_load_above_kw: null })).toBeNull();
    expect(estimateCarPower(9000, 230, {})).toBeNull();
    expect(estimateCarPower(9000, 230, { forecast_exclude_load_above_kw: 0 })).toBeNull();
  });

  it('uses the threshold in kW and the charger current', () => {
    const s = { forecast_exclude_load_above_kw: 3, car_charger_current_a: 16 };
    expect(estimateCarPower(9000, 230, s)).toBe(3680);
    expect(estimateCarPower(2000, 230, s)).toBe(0);
  });

  it('defaults to 32 A and ignores an out-of-range current', () => {
    expect(estimateCarPower(9000, 230, { forecast_exclude_load_above_kw: 3 })).toBe(7360);
    expect(estimateCarPower(9000, 230, { forecast_exclude_load_above_kw: 3, car_charger_current_a: 5 })).toBe(7360);
    expect(estimateCarPower(9000, 230, { forecast_exclude_load_above_kw: 3, car_charger_current_a: 81 })).toBe(7360);
  });

  it('ignores a threshold outside 1 to 25 kW, like the backend', () => {
    expect(estimateCarPower(9000, 230, { forecast_exclude_load_above_kw: 0.5 })).toBeNull();
    expect(estimateCarPower(9000, 230, { forecast_exclude_load_above_kw: 26 })).toBeNull();
  });
});

describe('isCarThresholdKw (the one range check behind every page)', () => {
  it('accepts 1 to 25 kW inclusive', () => {
    expect(MIN_THRESHOLD_KW).toBe(1);
    expect(MAX_THRESHOLD_KW).toBe(25);
    for (const kw of [1, 3, 7.4, 25]) expect(isCarThresholdKw(kw)).toBe(true);
  });

  it('rejects an empty, corrupt or out-of-range threshold', () => {
    for (const kw of [null, undefined, NaN, Infinity, 0, 0.99, 25.01, 40, -3]) {
      expect(isCarThresholdKw(kw)).toBe(false);
    }
  });

  it('agrees with estimateCarPower about whether the feature is on', () => {
    for (const kw of [null, 0, 0.5, 1, 3, 25, 26]) {
      expect(estimateCarPower(9000, 230, { forecast_exclude_load_above_kw: kw }) !== null).toBe(
        isCarThresholdKw(kw),
      );
    }
  });
});
