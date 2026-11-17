import { describe, it, expect } from 'vitest';
import { formatSavedAt } from '../../src/lib/scheduleReset';

describe('formatSavedAt', () => {
  it('gives the day, month and 24-hour time', () => {
    // Built from local parts, so the test holds in any time zone.
    const at = new Date(2026, 10, 14, 18, 2).getTime() / 1000;
    expect(formatSavedAt(at)).toBe('14 Nov 18:02');
  });

  it('pads the hour and minute', () => {
    const at = new Date(2026, 0, 3, 7, 5).getTime() / 1000;
    expect(formatSavedAt(at)).toBe('3 Jan 07:05');
  });

  it('does not show "Invalid Date" for a bad timestamp', () => {
    expect(formatSavedAt(Number.NaN)).toBe('earlier');
  });
});
