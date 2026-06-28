import { describe, expect, test } from 'vitest';
import type { TariffConfig } from '../../src/lib/types';
import {
  addTariffSlot,
  defaultTariffConfig,
  flatTariffConfig,
  halfHourOptions,
  isTariffConfigValid,
  minutesToHHMM,
  parseHHMM,
  rateForMinutes,
  removeTariffSlot,
  updateTariffSlot,
  validateTariffConfig,
} from '../../src/lib/tariff';

describe('parseHHMM', () => {
  test('parses midnight as 0', () => {
    expect(parseHHMM('00:00')).toBe(0);
  });

  test('parses noon as 720', () => {
    expect(parseHHMM('12:00')).toBe(720);
  });

  test('parses 23:59 as 1439 (the latest representable clock time)', () => {
    expect(parseHHMM('23:59')).toBe(1439);
  });

  test('rejects "24:00" (not a real clock time)', () => {
    expect(parseHHMM('24:00')).toBeNull();
  });

  test('rejects malformed input', () => {
    expect(parseHHMM('not-a-time')).toBeNull();
    expect(parseHHMM('25:00')).toBeNull();
    expect(parseHHMM('12:60')).toBeNull();
    expect(parseHHMM('12')).toBeNull();
    expect(parseHHMM('')).toBeNull();
  });
});

describe('minutesToHHMM', () => {
  test('clamps to 23:59 for end-of-day inputs', () => {
    expect(minutesToHHMM(1439)).toBe('23:59');
    // Anything beyond 1439 (e.g. legacy 1440) clamps down so callers never
    // see the bogus "24:00" representation.
    expect(minutesToHHMM(1440)).toBe('23:59');
    expect(minutesToHHMM(9999)).toBe('23:59');
  });

  test('clamps negatives to 00:00', () => {
    expect(minutesToHHMM(-10)).toBe('00:00');
  });

  test('zero-pads single-digit hours and minutes', () => {
    expect(minutesToHHMM(5)).toBe('00:05');
    expect(minutesToHHMM(60)).toBe('01:00');
    expect(minutesToHHMM(90)).toBe('01:30');
  });
});

describe('halfHourOptions', () => {
  test('includes both 00:00 and 23:59', () => {
    const opts = halfHourOptions();
    expect(opts[0]).toBe('00:00');
    expect(opts).toContain('23:59');
  });

  test('does not include "24:00"', () => {
    expect(halfHourOptions()).not.toContain('24:00');
  });

  test('produces half-hour granularity between 00:00 and 23:30', () => {
    const opts = halfHourOptions();
    // 24 hours × 2 slots/hour = 48 half-hour entries + 23:59 = 49 total.
    expect(opts).toHaveLength(49);
    expect(opts).toContain('00:30');
    expect(opts).toContain('12:00');
    expect(opts).toContain('23:30');
  });
});

describe('defaultTariffConfig', () => {
  test('ends with "23:59" (not "24:00")', () => {
    const cfg = defaultTariffConfig();
    expect(cfg.slots.at(-1)!.end).toBe('23:59');
  });
});

describe('flatTariffConfig', () => {
  test('covers the whole day with a single inclusive slot', () => {
    const cfg = flatTariffConfig(0.25);
    expect(cfg.slots).toHaveLength(1);
    expect(cfg.slots[0]!.start).toBe('00:00');
    expect(cfg.slots[0]!.end).toBe('23:59');
  });
});

describe('rateForMinutes', () => {
  test('final slot covers minute 1439 (23:59) inclusively', () => {
    const cfg = defaultTariffConfig();
    // 23:59 must resolve to the peak rate (final slot) and NOT the off-peak
    // rate — without the inclusive-last-slot rule this minute would be
    // uncovered.
    expect(rateForMinutes(cfg, 1439)).toBe(0.285);
    expect(rateForMinutes(cfg, 1438)).toBe(0.285);
  });

  test('intermediate slot is half-open at its end', () => {
    const cfg = defaultTariffConfig();
    // 05:30 = minute 330 is the off-peak slot's exclusive end → peak.
    expect(rateForMinutes(cfg, 330)).toBe(0.285);
    // 05:29 = minute 329 is still inside the off-peak window.
    expect(rateForMinutes(cfg, 329)).toBe(0.09);
  });

  test('rate lookup matches legacy peak/off-peak at every minute', () => {
    const cfg = defaultTariffConfig();
    // Old off-peak window was [30, 330). Every minute must agree.
    for (let m = 0; m < 1440; m++) {
      const expected = m >= 30 && m < 330 ? 0.09 : 0.285;
      expect(rateForMinutes(cfg, m)).toBe(expected);
    }
  });

  test('returns null when there are no slots', () => {
    expect(rateForMinutes({ slots: [] }, 100)).toBeNull();
  });
});

describe('addTariffSlot', () => {
  test('splits a flat rate at noon when starting from a single slot', () => {
    const cfg = flatTariffConfig(0.20);
    const after = addTariffSlot(cfg, 0.15);
    expect(after.slots).toHaveLength(2);
    expect(after.slots[0]!.start).toBe('00:00');
    expect(after.slots[0]!.end).toBe('12:00');
    expect(after.slots[1]!.start).toBe('12:00');
    expect(after.slots[1]!.end).toBe('23:59');
  });

  test('produces a contiguous, valid tiling after each add', () => {
    let cfg = flatTariffConfig(0.10);
    for (let i = 0; i < 5; i++) {
      cfg = addTariffSlot(cfg, 0.10);
      expect(isTariffConfigValid(cfg), `invalid after add ${i + 1}`).toBe(true);
      expect(cfg.slots[0]!.start).toBe('00:00');
      expect(cfg.slots.at(-1)!.end).toBe('23:59');
    }
  });

  test('respects the 10-slot cap', () => {
    let cfg = flatTariffConfig(0.10);
    for (let i = 0; i < 20; i++) {
      cfg = addTariffSlot(cfg, 0.10);
    }
    expect(cfg.slots.length).toBeLessThanOrEqual(10);
  });

  test('grows to exactly the cap, then stops', () => {
    // Pins MAX_TARIFF_SLOTS = 10. From a flat config, 9 successful adds
    // reach the cap (1 → 10 slots); the 10th add must be a no-op.
    let cfg = flatTariffConfig(0.10);
    for (let i = 0; i < 9; i++) {
      cfg = addTariffSlot(cfg, 0.10);
    }
    expect(cfg.slots).toHaveLength(10);
    const beforeNoOp = cfg;
    const afterNoOp = addTariffSlot(cfg, 0.10);
    expect(afterNoOp).toBe(beforeNoOp);
    expect(afterNoOp.slots).toHaveLength(10);
  });

  test('new slot uses the supplied defaultRate (not the split slot\'s rate)', () => {
    // A user with a flat rate who clicks "Add window" likely wants to set
    // up a time-of-use tariff with a different rate from the flat default
    // — so the new slot is seeded with the caller's `defaultRate`, not
    // inherited from the slot that got split.
    const cfg = flatTariffConfig(0.25);
    const after = addTariffSlot(cfg, 0.99);
    expect(after.slots[0]!.rate).toBe(0.25);
    expect(after.slots[1]!.rate).toBe(0.99);
  });
});

describe('updateTariffSlot cascades end-change to next slot start', () => {
  test('changing a middle slot\'s end moves the next slot\'s start', () => {
    const cfg: TariffConfig = {
      slots: [
        { start: '00:00', end: '06:00', rate: 0.10 },
        { start: '06:00', end: '18:00', rate: 0.20 },
        { start: '18:00', end: '23:59', rate: 0.30 },
      ],
    };
    const after = updateTariffSlot(cfg, 1, 'end', '12:00');
    expect(after.slots[1]!.end).toBe('12:00');
    expect(after.slots[2]!.start).toBe('12:00');
    expect(after.slots[2]!.end).toBe('23:59');
  });

  test('changing the last slot\'s end does NOT cascade', () => {
    // Last slot's end is fixed at 23:59 by validation, but the function
    // shouldn't crash if asked.
    const cfg: TariffConfig = {
      slots: [
        { start: '00:00', end: '12:00', rate: 0.10 },
        { start: '12:00', end: '23:59', rate: 0.20 },
      ],
    };
    const after = updateTariffSlot(cfg, 1, 'rate', 0.50);
    expect(after.slots[1]!.rate).toBe(0.50);
    expect(after.slots[1]!.end).toBe('23:59');
  });

  test('resulting config remains valid (no gap, no overlap)', () => {
    const cfg: TariffConfig = {
      slots: [
        { start: '00:00', end: '06:00', rate: 0.10 },
        { start: '06:00', end: '18:00', rate: 0.20 },
        { start: '18:00', end: '23:59', rate: 0.30 },
      ],
    };
    // Pick an end that's well before the next slot's start.
    const after = updateTariffSlot(cfg, 1, 'end', '10:00');
    expect(isTariffConfigValid(after)).toBe(true);
  });

  test('changing a middle slot\'s start cascades backward to the previous slot\'s end', () => {
    // Symmetric to the end cascade: editing a non-first slot's start
    // moves the previous slot's end to match, keeping the day tiled.
    const cfg: TariffConfig = {
      slots: [
        { start: '00:00', end: '06:00', rate: 0.10 },
        { start: '06:00', end: '18:00', rate: 0.20 },
        { start: '18:00', end: '23:59', rate: 0.30 },
      ],
    };
    const after = updateTariffSlot(cfg, 2, 'start', '14:00');
    expect(after.slots[1]!.end).toBe('14:00');
    expect(after.slots[2]!.start).toBe('14:00');
    expect(after.slots[2]!.end).toBe('23:59');
    expect(isTariffConfigValid(after)).toBe(true);
  });

  test('changing the first slot\'s start does NOT cascade', () => {
    // First slot's start is fixed at 00:00 by validation; the function
    // shouldn't crash or mutate neighbours if asked.
    const cfg: TariffConfig = {
      slots: [
        { start: '00:00', end: '12:00', rate: 0.10 },
        { start: '12:00', end: '23:59', rate: 0.20 },
      ],
    };
    const after = updateTariffSlot(cfg, 0, 'rate', 0.50);
    expect(after.slots[0]!.rate).toBe(0.50);
    expect(after.slots[0]!.start).toBe('00:00');
    expect(after.slots[1]!.start).toBe('12:00');
  });
});

describe('removeTariffSlot', () => {
  test('fallback when only one slot exists uses "23:59"', () => {
    const cfg = flatTariffConfig(0.42);
    const after = removeTariffSlot(cfg, 0);
    expect(after.slots).toHaveLength(1);
    expect(after.slots[0]!.end).toBe('23:59');
    expect(after.slots[0]!.rate).toBe(0.42);
  });
});

describe('validateTariffConfig', () => {
  test('accepts a valid 24-hour tiling', () => {
    expect(validateTariffConfig(defaultTariffConfig())).toHaveLength(0);
    expect(validateTariffConfig(flatTariffConfig(0.20))).toHaveLength(0);
  });

  test('flags a gap between slots', () => {
    const cfg: TariffConfig = {
      slots: [
        { start: '00:00', end: '05:00', rate: 0.10 },
        { start: '06:00', end: '23:59', rate: 0.20 },
      ],
    };
    const errors = validateTariffConfig(cfg);
    expect(errors.length).toBeGreaterThan(0);
    expect(errors.some((e) => e.message.toLowerCase().includes('gap'))).toBe(true);
  });

  test('flags overlapping slots', () => {
    const cfg: TariffConfig = {
      slots: [
        { start: '00:00', end: '06:00', rate: 0.10 },
        { start: '05:00', end: '23:59', rate: 0.20 },
      ],
    };
    const errors = validateTariffConfig(cfg);
    expect(errors.some((e) => e.message.toLowerCase().includes('overlap'))).toBe(true);
  });

  test('flags a final slot not ending at 23:59', () => {
    const cfg: TariffConfig = {
      slots: [{ start: '00:00', end: '20:00', rate: 0.10 }],
    };
    const errors = validateTariffConfig(cfg);
    expect(errors.some((e) => e.message.includes('23:59'))).toBe(true);
  });

  test('flags a first slot not starting at 00:00', () => {
    const cfg: TariffConfig = {
      slots: [{ start: '01:00', end: '23:59', rate: 0.10 }],
    };
    const errors = validateTariffConfig(cfg);
    expect(errors.some((e) => e.message.includes('00:00'))).toBe(true);
  });

  test('flags negative rates', () => {
    const cfg: TariffConfig = {
      slots: [{ start: '00:00', end: '23:59', rate: -0.1 }],
    };
    const errors = validateTariffConfig(cfg);
    expect(errors.some((e) => e.field === 'rate')).toBe(true);
  });

  test('flags empty slot list', () => {
    const errors = validateTariffConfig({ slots: [] });
    expect(errors.some((e) => e.field === 'config')).toBe(true);
  });

  test('flags malformed time strings', () => {
    const cfg: TariffConfig = {
      slots: [{ start: 'garbage', end: '23:59', rate: 0.10 }],
    };
    const errors = validateTariffConfig(cfg);
    expect(errors.length).toBeGreaterThan(0);
  });
});