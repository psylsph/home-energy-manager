import { describe, expect, it } from 'vitest';
import {
  LOGARITHMIC_RANGE_MAX,
  logarithmicPositionToValue,
  logarithmicValueToPosition,
} from '../../src/lib/logarithmicRange';

describe('logarithmic range helpers', () => {
  it('maps both value endpoints to the track endpoints', () => {
    expect(logarithmicValueToPosition(4, 4, 100)).toBe(0);
    expect(logarithmicValueToPosition(100, 4, 100)).toBe(LOGARITHMIC_RANGE_MAX);
    expect(logarithmicPositionToValue(0, 4, 100)).toBe(4);
    expect(logarithmicPositionToValue(LOGARITHMIC_RANGE_MAX, 4, 100)).toBe(100);
  });

  it('gives lower values more track space than a linear range', () => {
    const twentyPercentPosition = logarithmicValueToPosition(20, 4, 100);
    expect(twentyPercentPosition).toBeGreaterThan(LOGARITHMIC_RANGE_MAX * 0.45);
    expect(twentyPercentPosition).toBeLessThan(LOGARITHMIC_RANGE_MAX * 0.55);
    expect(logarithmicPositionToValue(twentyPercentPosition, 4, 100)).toBe(20);
  });

  it('round-trips common Minimum SOC values', () => {
    for (const soc of [4, 5, 10, 20, 30, 50, 80, 100]) {
      expect(logarithmicPositionToValue(
        logarithmicValueToPosition(soc, 4, 100),
        4,
        100,
      )).toBe(soc);
    }
  });

  it('clamps values and positions outside their ranges', () => {
    expect(logarithmicValueToPosition(-1, 4, 100)).toBe(0);
    expect(logarithmicValueToPosition(101, 4, 100)).toBe(LOGARITHMIC_RANGE_MAX);
    expect(logarithmicPositionToValue(-1, 4, 100)).toBe(4);
    expect(logarithmicPositionToValue(LOGARITHMIC_RANGE_MAX + 1, 4, 100)).toBe(100);
  });

  it('snaps converted values to the requested step', () => {
    const position = logarithmicValueToPosition(62, 5, 1440);
    expect(logarithmicPositionToValue(position, 5, 1440, 5)).toBe(60);
  });

  it('rejects invalid logarithmic range configurations', () => {
    expect(() => logarithmicValueToPosition(1, 0, 100)).toThrow(RangeError);
    expect(() => logarithmicValueToPosition(1, 10, 10)).toThrow(RangeError);
    expect(() => logarithmicPositionToValue(1, 4, 100, 0)).toThrow(RangeError);
  });
});
