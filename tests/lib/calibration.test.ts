import { describe, it, expect } from 'vitest';
import {
  describeLastCalibration,
  describePhase,
  holdRemainingMinutes,
  parseHoldHours,
} from '../../src/lib/calibration';
import type { CalibrationRecord, CalibrationView } from '../../src/lib/calibration';

const T0 = Date.UTC(2026, 9, 30, 18, 0);

function view(overrides: Partial<CalibrationView> = {}): CalibrationView {
  return {
    phase: 'discharging',
    started_at_ms: T0,
    phase_started_at_ms: T0,
    hold_minutes: 120,
    ...overrides,
  };
}

describe('describePhase', () => {
  it('names each step in plain words', () => {
    expect(describePhase(view(), 4)).toBe('Exporting down to 4%');
    expect(describePhase(view({ phase: 'charging' }), 4)).toBe('Charging to 100%');
    expect(describePhase(view({ phase: 'holding', since_ms: T0 }), 4)).toBe(
      'Holding at 100% while the cells balance',
    );
    expect(
      describePhase(view({ phase: 'restoring', outcome: { result: 'completed' } }), 4),
    ).toBe('Putting your settings back');
    expect(
      describePhase(view({ phase: 'restoring', outcome: { result: 'cancelled' } }), 4),
    ).toBe('Cancelling: putting your settings back');
  });
});

describe('holdRemainingMinutes', () => {
  it('counts down the hold, never below zero', () => {
    const holding = view({ phase: 'holding', since_ms: T0 });
    expect(holdRemainingMinutes(holding, T0)).toBe(120);
    expect(holdRemainingMinutes(holding, T0 + 45 * 60_000)).toBe(75);
    expect(holdRemainingMinutes(holding, T0 + 300 * 60_000)).toBe(0);
  });

  it('is null outside the hold', () => {
    expect(holdRemainingMinutes(view(), T0)).toBeNull();
    expect(holdRemainingMinutes(view({ phase: 'holding' }), T0)).toBeNull();
  });
});

describe('describeLastCalibration', () => {
  const record = (outcome: CalibrationRecord['outcome']): CalibrationRecord => ({
    started_at_ms: T0,
    finished_at_ms: T0 + 8 * 3_600_000,
    outcome,
  });

  it('says how the last cycle ended', () => {
    expect(describeLastCalibration(record({ result: 'completed' }))).toMatch(/^Last calibration finished /);
    expect(describeLastCalibration(record({ result: 'cancelled' }))).toMatch(/^Last calibration was cancelled /);
    const failed = describeLastCalibration(record({ result: 'failed', reason: 'The battery did not reach 4%' }));
    expect(failed).toMatch(/^Last calibration stopped /);
    expect(failed).toMatch(/The battery did not reach 4%$/);
  });

  it('is empty without a record', () => {
    expect(describeLastCalibration(null)).toBe('');
  });
});

describe('parseHoldHours', () => {
  it('turns hours into minutes inside the limits', () => {
    expect(parseHoldHours('2', 30, 480)).toBe(120);
    expect(parseHoldHours('0.5', 30, 480)).toBe(30);
    expect(parseHoldHours(' 1.5 ', 30, 480)).toBe(90);
    expect(parseHoldHours('8', 30, 480)).toBe(480);
  });

  it('rejects anything else', () => {
    for (const bad of ['', 'two', '0', '0.25', '9', '-1', '1e1', 'NaN']) {
      expect(parseHoldHours(bad, 30, 480), bad).toBeNull();
    }
  });
});
