/**
 * Tests for the Force Charge / Discharge Duration control helpers.
 *
 * These cover:
 *   - Duration label formatting (30m, 1h 30m, 24h)
 *   - Slider value clamping (1..=1440)
 *   - localStorage round-trip with validation
 *
 * The control itself is a small piece of UI inside ControlPage.tsx —
 * rather than mounting the whole page, the pure logic is extracted
 * into `forceDuration.ts` and tested here. The React component
 * integration is covered by Playwright E2E tests.
 */

import { describe, it, expect, beforeEach } from 'vitest';
import {
  FORCE_DURATION_DEFAULT,
  FORCE_DURATION_STORAGE_KEY,
  clampDurationMinutes,
  formatDurationLabel,
  readPersistedDuration,
} from './forceDuration';

describe('formatDurationLabel', () => {
  it('formats sub-hour values as minutes', () => {
    expect(formatDurationLabel(1)).toBe('1m');
    expect(formatDurationLabel(30)).toBe('30m');
    expect(formatDurationLabel(59)).toBe('59m');
  });

  it('formats exact-hour values without trailing minutes', () => {
    expect(formatDurationLabel(60)).toBe('1h');
    expect(formatDurationLabel(120)).toBe('2h');
    expect(formatDurationLabel(720)).toBe('12h');
  });

  it('formats hour+minute values with both', () => {
    expect(formatDurationLabel(90)).toBe('1h 30m');
    expect(formatDurationLabel(61)).toBe('1h 1m');
    expect(formatDurationLabel(1439)).toBe('23h 59m');
  });

  it('shows 24h at the slider cap (1440 minutes)', () => {
    expect(formatDurationLabel(1440)).toBe('24h');
    expect(formatDurationLabel(1500)).toBe('24h');
    expect(formatDurationLabel(9999)).toBe('24h');
  });

  it('handles edge cases gracefully', () => {
    expect(formatDurationLabel(0)).toBe('0m');
    expect(formatDurationLabel(-5)).toBe('0m');
    expect(formatDurationLabel(NaN)).toBe('0m');
    expect(formatDurationLabel(Infinity)).toBe('0m');
  });

  it('truncates fractional minute values (display is conservative)', () => {
    // The slider can only produce integer values (step=1), so this is
    // a defensive case. Truncating rather than rounding avoids showing
    // a label that claims more time than was actually selected (e.g.
    // 30.7 minutes displaying as "31m" when the user picked 30.7).
    expect(formatDurationLabel(30.7)).toBe('30m');
    expect(formatDurationLabel(89.9)).toBe('1h 29m');
  });
});

describe('clampDurationMinutes', () => {
  it('passes through values in range', () => {
    expect(clampDurationMinutes(1)).toBe(1);
    expect(clampDurationMinutes(30)).toBe(30);
    expect(clampDurationMinutes(1440)).toBe(1440);
  });

  it('clamps values below 1', () => {
    expect(clampDurationMinutes(0)).toBe(1);
    expect(clampDurationMinutes(-5)).toBe(1);
  });

  it('clamps values above 1440', () => {
    expect(clampDurationMinutes(1441)).toBe(1440);
    expect(clampDurationMinutes(9999)).toBe(1440);
  });

  it('rounds fractional values', () => {
    expect(clampDurationMinutes(30.4)).toBe(30);
    expect(clampDurationMinutes(30.6)).toBe(31);
  });

  it('returns the default for non-finite values', () => {
    expect(clampDurationMinutes(NaN)).toBe(FORCE_DURATION_DEFAULT);
    expect(clampDurationMinutes(Infinity)).toBe(FORCE_DURATION_DEFAULT);
  });
});

describe('readPersistedDuration', () => {
  let storage: Map<string, string>;

  beforeEach(() => {
    storage = new Map();
  });

  const mockStorage = {
    getItem: (key: string) => storage.get(key) ?? null,
  };

  it('returns the default when storage is null', () => {
    expect(readPersistedDuration(null)).toBe(FORCE_DURATION_DEFAULT);
  });

  it('returns the default when nothing is persisted', () => {
    expect(readPersistedDuration(mockStorage)).toBe(FORCE_DURATION_DEFAULT);
  });

  it('returns the default for non-numeric values', () => {
    storage.set(FORCE_DURATION_STORAGE_KEY, 'not-a-number');
    expect(readPersistedDuration(mockStorage)).toBe(FORCE_DURATION_DEFAULT);
  });

  it('returns the default for empty string', () => {
    storage.set(FORCE_DURATION_STORAGE_KEY, '');
    expect(readPersistedDuration(mockStorage)).toBe(FORCE_DURATION_DEFAULT);
  });

  it('clamps out-of-range values', () => {
    storage.set(FORCE_DURATION_STORAGE_KEY, '0');
    expect(readPersistedDuration(mockStorage)).toBe(1);

    storage.set(FORCE_DURATION_STORAGE_KEY, '-5');
    expect(readPersistedDuration(mockStorage)).toBe(1);

    storage.set(FORCE_DURATION_STORAGE_KEY, '9999');
    expect(readPersistedDuration(mockStorage)).toBe(1440);
  });

  it('returns valid persisted values unchanged', () => {
    storage.set(FORCE_DURATION_STORAGE_KEY, '30');
    expect(readPersistedDuration(mockStorage)).toBe(30);

    storage.set(FORCE_DURATION_STORAGE_KEY, '1');
    expect(readPersistedDuration(mockStorage)).toBe(1);

    storage.set(FORCE_DURATION_STORAGE_KEY, '1440');
    expect(readPersistedDuration(mockStorage)).toBe(1440);

    storage.set(FORCE_DURATION_STORAGE_KEY, '720');
    expect(readPersistedDuration(mockStorage)).toBe(720);
  });
});
