import { describe, expect, it } from 'vitest';
import {
  DEFAULT_ADAPTIVE_PERIOD,
  adaptiveSocFieldCaption,
  adaptiveStateLabel,
  validateAdaptiveChargeConfig,
  type AdaptiveChargeConfig,
} from '../../src/lib/adaptiveCharge';

/**
 * Coverage for src/lib/adaptiveCharge.ts — the Adaptive Charge config
 * validator + label helpers. The existing adaptiveCharge.test.ts covers the
 * happy paths and a few rejection cases; this file targets the remaining
 * untested branches in validateAdaptiveChargeConfig, DEFAULT_ADAPTIVE_PERIOD,
 * adaptiveSocFieldCaption and adaptiveStateLabel.
 */

function config(overrides: Partial<AdaptiveChargeConfig> = {}): AdaptiveChargeConfig {
  return {
    periods: [{ ...DEFAULT_ADAPTIVE_PERIOD }],
    confirmation_readings: 2,
    ...overrides,
  };
}

describe('DEFAULT_ADAPTIVE_PERIOD', () => {
  it('exports a sensible daytime default', () => {
    expect(DEFAULT_ADAPTIVE_PERIOD.enabled).toBe(true);
    expect(DEFAULT_ADAPTIVE_PERIOD.all_day).toBe(false);
    expect(DEFAULT_ADAPTIVE_PERIOD.start_hour).toBe(8);
    expect(DEFAULT_ADAPTIVE_PERIOD.end_hour).toBe(17);
  });

  it('has low_soc strictly below recovery_soc', () => {
    expect(DEFAULT_ADAPTIVE_PERIOD.low_soc).toBeLessThan(DEFAULT_ADAPTIVE_PERIOD.recovery_soc);
  });

  it('has recovery_rate_percent >= preferred_rate_percent', () => {
    expect(DEFAULT_ADAPTIVE_PERIOD.recovery_rate_percent)
      .toBeGreaterThanOrEqual(DEFAULT_ADAPTIVE_PERIOD.preferred_rate_percent);
  });
});

describe('validateAdaptiveChargeConfig — period count limits', () => {
  it('rejects more than four periods', () => {
    const periods = Array.from({ length: 5 }, () => ({
      ...DEFAULT_ADAPTIVE_PERIOD,
      start_hour: 0,
      end_hour: 1,
    }));
    // Shift each period to avoid overlap before hitting the count guard.
    periods.forEach((p, i) => {
      p.start_hour = i;
      p.end_hour = i + 1;
    });
    const result = validateAdaptiveChargeConfig(config({ periods }));
    expect(result).toContain('at most four');
  });

  it('accepts exactly four non-overlapping periods', () => {
    const periods = [0, 1, 2, 3].map((h) => ({
      ...DEFAULT_ADAPTIVE_PERIOD,
      start_hour: h,
      end_hour: h + 1,
    }));
    expect(validateAdaptiveChargeConfig(config({ periods }))).toBeNull();
  });
});

describe('validateAdaptiveChargeConfig — confirmation_readings bounds', () => {
  it('rejects confirmation_readings below 1', () => {
    expect(
      validateAdaptiveChargeConfig(config({ confirmation_readings: 0 })),
    ).toContain('between 1 and 10');
  });

  it('rejects confirmation_readings above 10', () => {
    expect(
      validateAdaptiveChargeConfig(config({ confirmation_readings: 11 })),
    ).toContain('between 1 and 10');
  });

  it('accepts confirmation_readings of 1', () => {
    expect(
      validateAdaptiveChargeConfig(config({ confirmation_readings: 1 })),
    ).toBeNull();
  });

  it('accepts confirmation_readings of 10', () => {
    expect(
      validateAdaptiveChargeConfig(config({ confirmation_readings: 10 })),
    ).toBeNull();
  });
});

describe('validateAdaptiveChargeConfig — SOC bounds', () => {
  it('rejects low_soc below 4', () => {
    const result = validateAdaptiveChargeConfig(
      config({ periods: [{ ...DEFAULT_ADAPTIVE_PERIOD, low_soc: 3 }] }),
    );
    expect(result).toContain('Low SOC');
    expect(result).toContain('Period 1');
  });

  it('rejects low_soc above 99', () => {
    const result = validateAdaptiveChargeConfig(
      config({ periods: [{ ...DEFAULT_ADAPTIVE_PERIOD, low_soc: 100 }] }),
    );
    expect(result).toContain('Low SOC');
  });

  it('accepts low_soc at exactly 4', () => {
    expect(
      validateAdaptiveChargeConfig(
        config({ periods: [{ ...DEFAULT_ADAPTIVE_PERIOD, low_soc: 4 }] }),
      ),
    ).toBeNull();
  });

  it('accepts low_soc at exactly 99', () => {
    expect(
      validateAdaptiveChargeConfig(
        config({ periods: [{ ...DEFAULT_ADAPTIVE_PERIOD, low_soc: 99, recovery_soc: 100 }] }),
      ),
    ).toBeNull();
  });

  it('rejects recovery_soc above 100', () => {
    const result = validateAdaptiveChargeConfig(
      config({
        periods: [
          {
            ...DEFAULT_ADAPTIVE_PERIOD,
            low_soc: 30,
            recovery_soc: 101,
          },
        ],
      }),
    );
    expect(result).toContain('Recovery SOC');
  });
});

describe('validateAdaptiveChargeConfig — charge rate bounds', () => {
  it('rejects preferred_rate_percent below 0', () => {
    const result = validateAdaptiveChargeConfig(
      config({
        periods: [{ ...DEFAULT_ADAPTIVE_PERIOD, preferred_rate_percent: -1 }],
      }),
    );
    expect(result).toContain('Charge rates must be between');
  });

  it('rejects preferred_rate_percent above 100', () => {
    const result = validateAdaptiveChargeConfig(
      config({
        periods: [{ ...DEFAULT_ADAPTIVE_PERIOD, preferred_rate_percent: 101 }],
      }),
    );
    expect(result).toContain('Charge rates must be between');
  });

  it('rejects recovery_rate_percent below 0', () => {
    const result = validateAdaptiveChargeConfig(
      config({
        periods: [{ ...DEFAULT_ADAPTIVE_PERIOD, recovery_rate_percent: -1 }],
      }),
    );
    expect(result).toContain('Charge rates must be between');
  });

  it('rejects recovery_rate_percent above 100', () => {
    const result = validateAdaptiveChargeConfig(
      config({
        periods: [{ ...DEFAULT_ADAPTIVE_PERIOD, recovery_rate_percent: 101 }],
      }),
    );
    expect(result).toContain('Charge rates must be between');
  });

  it('accepts both rates at 0%', () => {
    expect(
      validateAdaptiveChargeConfig(
        config({
          periods: [
            {
              ...DEFAULT_ADAPTIVE_PERIOD,
              preferred_rate_percent: 0,
              recovery_rate_percent: 0,
            },
          ],
        }),
      ),
    ).toBeNull();
  });

  it('accepts both rates at 100%', () => {
    expect(
      validateAdaptiveChargeConfig(
        config({
          periods: [
            {
              ...DEFAULT_ADAPTIVE_PERIOD,
              preferred_rate_percent: 100,
              recovery_rate_percent: 100,
            },
          ],
        }),
      ),
    ).toBeNull();
  });
});

describe('validateAdaptiveChargeConfig — time window edge cases', () => {
  it('rejects start == end when all_day is false', () => {
    const result = validateAdaptiveChargeConfig(
      config({
        periods: [
          {
            ...DEFAULT_ADAPTIVE_PERIOD,
            start_hour: 8,
            start_minute: 0,
            end_hour: 8,
            end_minute: 0,
            all_day: false,
          },
        ],
      }),
    );
    expect(result).toContain('Start and end must differ');
  });

  it('accepts start == end when all_day is true', () => {
    const result = validateAdaptiveChargeConfig(
      config({
        periods: [
          {
            ...DEFAULT_ADAPTIVE_PERIOD,
            start_hour: 8,
            start_minute: 0,
            end_hour: 8,
            end_minute: 0,
            all_day: true,
          },
        ],
      }),
    );
    expect(result).toBeNull();
  });

  it('accepts a single all_day period (covers all 1440 minutes)', () => {
    expect(
      validateAdaptiveChargeConfig(
        config({
          periods: [{ ...DEFAULT_ADAPTIVE_PERIOD, all_day: true }],
        }),
      ),
    ).toBeNull();
  });

  it('rejects an all_day period overlapping another enabled period', () => {
    const result = validateAdaptiveChargeConfig(
      config({
        periods: [
          { ...DEFAULT_ADAPTIVE_PERIOD, all_day: true },
          { ...DEFAULT_ADAPTIVE_PERIOD, start_hour: 6, end_hour: 7 },
        ],
      }),
    );
    expect(result).toContain('overlaps');
  });

  it('accepts same-time windows when one period is disabled (skips overlap check)', () => {
    // A disabled period is skipped, so two periods covering the same window
    // with one disabled should pass — the enabled-period set is what matters.
    const result = validateAdaptiveChargeConfig(
      config({
        periods: [
          { ...DEFAULT_ADAPTIVE_PERIOD, enabled: true },
          { ...DEFAULT_ADAPTIVE_PERIOD, enabled: false },
        ],
      }),
    );
    expect(result).toBeNull();
  });

  it('reports the correct 1-based period index in errors', () => {
    const periods = [
      { ...DEFAULT_ADAPTIVE_PERIOD, enabled: false },
      { ...DEFAULT_ADAPTIVE_PERIOD, low_soc: 2 }, // second enabled period, bad SOC
    ];
    const result = validateAdaptiveChargeConfig(config({ periods }));
    // Index 1 in the array → "Period 2" in the message.
    expect(result).toContain('Period 2');
  });
});

describe('adaptiveSocFieldCaption', () => {
  it('low_soc caption mentions charge-rate trigger', () => {
    expect(adaptiveSocFieldCaption('low_soc')).toMatch(/charge-rate trigger/i);
  });

  it('recovery_soc caption mentions dropping to preferred rate', () => {
    expect(adaptiveSocFieldCaption('recovery_soc')).toMatch(/preferred charge rate/i);
  });
});

describe('adaptiveStateLabel', () => {
  it('labels outside_window', () => {
    expect(adaptiveStateLabel('outside_window')).toBe('Outside configured period');
  });

  it('labels preferred', () => {
    expect(adaptiveStateLabel('preferred')).toBe('Preferred rate active');
  });

  it('labels restoring', () => {
    expect(adaptiveStateLabel('restoring')).toBe('Restoring manual rate');
  });

  it('labels error', () => {
    expect(adaptiveStateLabel('error')).toBe('Error');
  });

  it('falls back to "Inactive" for unknown states', () => {
    expect(adaptiveStateLabel('something_new')).toBe('Inactive');
  });

  it('falls back to "Inactive" for empty string', () => {
    expect(adaptiveStateLabel('')).toBe('Inactive');
  });
});
