import { describe, expect, it } from 'vitest';
import {
  DEFAULT_ADAPTIVE_PERIOD,
  adaptiveSocFieldCaption,
  adaptiveStateLabel,
  validateAdaptiveChargeConfig,
  type AdaptiveChargeConfig,
} from '../../src/lib/adaptiveCharge';

function config(overrides: Partial<AdaptiveChargeConfig> = {}): AdaptiveChargeConfig {
  return {
    periods: [{ ...DEFAULT_ADAPTIVE_PERIOD }],
    confirmation_readings: 2,
    ...overrides,
  };
}

describe('validateAdaptiveChargeConfig', () => {
  it('accepts the default daytime period', () => {
    expect(validateAdaptiveChargeConfig(config())).toBeNull();
  });

  it('accepts adjacent overnight periods', () => {
    expect(validateAdaptiveChargeConfig(config({
      periods: [
        { ...DEFAULT_ADAPTIVE_PERIOD, start_hour: 22, end_hour: 6 },
        { ...DEFAULT_ADAPTIVE_PERIOD, start_hour: 6, end_hour: 8 },
      ],
    }))).toBeNull();
  });

  it('rejects overlap across midnight', () => {
    expect(validateAdaptiveChargeConfig(config({
      periods: [
        { ...DEFAULT_ADAPTIVE_PERIOD, start_hour: 22, end_hour: 6 },
        { ...DEFAULT_ADAPTIVE_PERIOD, start_hour: 5, end_hour: 8 },
      ],
    }))).toContain('overlaps');
  });

  it('rejects invalid SOC hysteresis and inverted rates', () => {
    expect(validateAdaptiveChargeConfig(config({
      periods: [{ ...DEFAULT_ADAPTIVE_PERIOD, recovery_soc: 30 }],
    }))).toContain('Recovery SOC');

    expect(validateAdaptiveChargeConfig(config({
      periods: [{
        ...DEFAULT_ADAPTIVE_PERIOD,
        preferred_rate_percent: 80,
        recovery_rate_percent: 60,
      }],
    }))).toContain('Recovery rate');
  });

  it('requires an enabled period', () => {
    expect(validateAdaptiveChargeConfig(config({
      periods: [{ ...DEFAULT_ADAPTIVE_PERIOD, enabled: false }],
    }))).toContain('Enable at least one');
  });
});

describe('adaptiveSocFieldCaption', () => {
  it('clarifies that Low SOC is a charge-rate trigger, not a discharge floor', () => {
    const caption = adaptiveSocFieldCaption('low_soc');
    // Must steer users away from the #256 misunderstanding: Low SOC does
    // not cap discharge, and must point them at the right controls.
    expect(caption).toMatch(/charge-rate trigger/i);
    expect(caption).toMatch(/does not stop discharge/i);
    expect(caption).toMatch(/target soc/i);
  });

  it('describes Recovery SOC as the return-to-preferred threshold', () => {
    expect(adaptiveSocFieldCaption('recovery_soc')).toMatch(/preferred charge rate/i);
  });
});

describe('adaptiveStateLabel', () => {
  it('returns human-readable runtime labels', () => {
    expect(adaptiveStateLabel('recovery')).toBe('Low-SOC recovery active');
    expect(adaptiveStateLabel('suspended_auto_winter')).toBe('Suspended by Auto Winter');
    expect(adaptiveStateLabel(undefined)).toBe('Inactive');
  });
});
