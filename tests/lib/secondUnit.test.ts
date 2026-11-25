import { describe, it, expect } from 'vitest';
import { secondUnitStatus } from '../../src/lib/secondUnit';
import type { SecondUnitInfo } from '../../src/lib/types';

const info = (over: Partial<SecondUnitInfo>): SecondUnitInfo => ({
  state: 'merged',
  solar_power: 0,
  battery_power: 0,
  soc: 0,
  battery_capacity_kwh: 0,
  today_charge_kwh: 0,
  today_discharge_kwh: 0,
  ...over,
});

describe('secondUnitStatus (issue #456)', () => {
  it('says nothing when no second HEM is configured', () => {
    expect(secondUnitStatus(undefined)).toBe('');
    expect(secondUnitStatus(null)).toBe('');
  });

  it('says the figures cover both inverters when merged', () => {
    expect(secondUnitStatus(info({ age_secs: 12 }))).toBe(
      'Reading the second inverter (12 s ago). Home, Solar and Grid cover both inverters.',
    );
  });

  it('reads "just now" for a fresh answer and copes with no age', () => {
    expect(secondUnitStatus(info({ age_secs: 0 }))).toContain('(just now)');
    expect(secondUnitStatus(info({}))).toBe(
      'Reading the second inverter. Home, Solar and Grid cover both inverters.',
    );
  });

  it('says why the second inverter is left out when it is not merged', () => {
    expect(secondUnitStatus(info({ state: 'unreachable' }))).toContain('hasn’t answered yet');
    expect(secondUnitStatus(info({ state: 'stale', age_secs: 300 }))).toContain('went quiet');
    expect(secondUnitStatus(info({ state: 'invalid' }))).toContain('can’t be used');
    for (const state of ['unreachable', 'stale', 'invalid'] as const) {
      expect(secondUnitStatus(info({ state }))).toMatch(/Home shows this inverter only|can’t be used/);
    }
  });

  it('gives no text for an unknown state from a newer backend', () => {
    expect(secondUnitStatus(info({ state: 'later' as never }))).toBe('');
  });
});
