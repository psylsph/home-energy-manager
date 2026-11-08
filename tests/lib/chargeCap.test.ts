import { describe, it, expect } from 'vitest';
import {
  capLineSoc,
  chargeCapError,
  daysSince,
  describeChargeCapState,
  describeLastFullCharge,
  parseSoc,
} from '../../src/lib/chargeCap';
import type { ChargeCapStateName } from '../../src/lib/chargeCap';

const LIMITS = { min_cap_soc: 50, max_cap_soc: 99, min_resume_soc: 40, min_gap: 2 };

describe('parseSoc', () => {
  it('accepts whole numbers only', () => {
    expect(parseSoc('95')).toBe(95);
    expect(parseSoc(' 7 ')).toBe(7);
    for (const bad of ['', '9.5', '-1', 'abc', '1000', '95%']) {
      expect(parseSoc(bad), bad).toBeNull();
    }
  });
});

describe('chargeCapError', () => {
  it('matches the backend limits', () => {
    expect(chargeCapError(95, 92, LIMITS)).toBeNull();
    expect(chargeCapError(50, 40, LIMITS)).toBeNull();
    expect(chargeCapError(99, 97, LIMITS)).toBeNull();
    expect(chargeCapError(49, 40, LIMITS)).toMatch(/between 50% and 99%/);
    expect(chargeCapError(100, 90, LIMITS)).toMatch(/between 50% and 99%/);
    expect(chargeCapError(null, 90, LIMITS)).toMatch(/between/);
    expect(chargeCapError(95, 39, LIMITS)).toMatch(/40% or more/);
    expect(chargeCapError(95, null, LIMITS)).toMatch(/40% or more/);
    expect(chargeCapError(95, 94, LIMITS)).toMatch(/at least 2% below/);
    expect(chargeCapError(95, 95, LIMITS)).toMatch(/at least 2% below/);
  });
});

describe('describeChargeCapState', () => {
  it('has a sentence for every state', () => {
    const states: ChargeCapStateName[] = [
      'off', 'unsupported', 'waiting', 'pending', 'holding', 'releasing', 'yielding', 'blocked', 'unreadable',
    ];
    for (const state of states) {
      const text = describeChargeCapState(state, 95, 92);
      expect(text.length, state).toBeGreaterThan(0);
      expect(text[0], state).toBe(text[0].toUpperCase());
    }
    expect(describeChargeCapState('holding', 95, 92)).toBe('Charging paused at 95%. Charges again at 92%.');
    expect(describeChargeCapState('waiting', 90, 85)).toMatch(/reaches 90%/);
    expect(describeChargeCapState('unreadable', 95, 92)).toMatch(/can't read this inverter's pause/);
  });
});

describe('describeChargeCapState with an unreadable pause', () => {
  it('adds that the cap cannot let go while it is holding or releasing', () => {
    for (const state of ['holding', 'releasing'] as const) {
      const plain = describeChargeCapState(state, 95, 92);
      const text = describeChargeCapState(state, 95, 92, true);
      expect(text.startsWith(plain), state).toBe(true);
      expect(text, state).toMatch(/can't read the pause, so it can't let go yet/);
    }
  });

  it('leaves every other state alone', () => {
    for (const state of ['off', 'unsupported', 'waiting', 'pending', 'yielding', 'blocked', 'unreadable'] as const) {
      expect(describeChargeCapState(state, 95, 92, true), state).toBe(describeChargeCapState(state, 95, 92));
    }
  });
});

describe('capLineSoc', () => {
  it('draws the line only while the cap is on and below 100%', () => {
    expect(capLineSoc({ charge_cap_enabled: true, charge_cap_soc: 95 })).toBe(95);
    expect(capLineSoc({ charge_cap_enabled: false, charge_cap_soc: 95 })).toBeNull();
    expect(capLineSoc({ charge_cap_soc: 95 })).toBeNull();
    expect(capLineSoc({ charge_cap_enabled: true, charge_cap_soc: 100 })).toBeNull();
    expect(capLineSoc({ charge_cap_enabled: true, charge_cap_soc: 0 })).toBeNull();
    expect(capLineSoc({ charge_cap_enabled: true, charge_cap_soc: Number.NaN })).toBeNull();
    expect(capLineSoc({ charge_cap_enabled: true })).toBeNull();
    expect(capLineSoc(null)).toBeNull();
    expect(capLineSoc(undefined)).toBeNull();
  });
});

describe('last full charge', () => {
  const NOW = Date.UTC(2026, 10, 3, 12, 0);
  const secondsAgo = (days: number) => NOW / 1000 - days * 86_400;

  it('counts whole days', () => {
    expect(daysSince(secondsAgo(0.5), NOW)).toBe(0);
    expect(daysSince(secondsAgo(3.9), NOW)).toBe(3);
    expect(daysSince(null, NOW)).toBeNull();
    expect(daysSince(undefined, NOW)).toBeNull();
    // A reading stamped ahead of this clock isn't "minus one day ago".
    expect(daysSince(NOW / 1000 + 3600, NOW)).toBe(0);
  });

  it('describes it in words', () => {
    expect(describeLastFullCharge(secondsAgo(0.2), NOW)).toBe('Last full charge: today.');
    expect(describeLastFullCharge(secondsAgo(1.2), NOW)).toBe('Last full charge: yesterday.');
    expect(describeLastFullCharge(secondsAgo(12), NOW)).toBe('Last full charge: 12 days ago.');
    expect(describeLastFullCharge(null, NOW)).toBe('Last full charge: not in the history yet.');
  });
});
