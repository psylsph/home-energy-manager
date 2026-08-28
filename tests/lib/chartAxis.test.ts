import { describe, it, expect } from 'vitest';
import { timeAxisProps } from '../../src/lib/chartAxis';

// ---------------------------------------------------------------------------
// Shared time-series x-axis configuration: the forecast charts prepend a
// "now" anchor point (e.g. 22:45) ahead of the first full hour, and
// Recharts' default CATEGORY axis would render it as a full evenly-spaced
// slot instead of at its true 15-minute offset. Pins the shared constant
// so a future "simplification" can't silently drop the numeric axis and
// reintroduce the misaligned spacing.
// ---------------------------------------------------------------------------

describe('timeAxisProps', () => {
  it('uses a numeric (time-proportional) axis, not evenly-spaced categories', () => {
    expect(timeAxisProps.type).toBe('number');
  });

  it('pins the domain to the data so charts do not pad empty time on either side', () => {
    expect(timeAxisProps.domain).toEqual(['dataMin', 'dataMax']);
  });
});
