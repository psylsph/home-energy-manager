import { describe, it, expect } from 'vitest';
import {
  carbonLabel,
  currentCarbon,
  formatCarbon,
  type CarbonForecast,
  type CarbonPeriod,
} from '../../src/lib/carbonIntensity';

/** Grid carbon intensity selection for the house view (issue #401). */

const at = (iso: string) => Date.parse(iso);

function period(from: string, forecast: number, index = 'moderate'): CarbonPeriod {
  const to = new Date(at(from) + 30 * 60_000).toISOString();
  return { from, to, forecast, index };
}

function fc(periods: CarbonPeriod[], region: string | null = null): CarbonForecast {
  return { region, fetched_at: '2026-11-01T13:00:00Z', periods };
}

describe('currentCarbon', () => {
  const day = fc([
    period('2026-11-01T13:00:00Z', 183, 'high'),
    period('2026-11-01T13:30:00Z', 190, 'high'),
    period('2026-11-02T03:30:00Z', 92, 'low'),
    period('2026-11-02T04:00:00Z', 92, 'low'),
    period('2026-11-02T05:00:00Z', 120, 'moderate'),
  ], 'South England');

  it('shows the half hour covering now and the lowest one ahead', () => {
    const c = currentCarbon(day, at('2026-11-01T13:10:00Z'))!;
    expect(c.intensity).toBe(183);
    expect(c.band).toBe('high');
    expect(c.region).toBe('South England');
    expect(c.lowest).toEqual({ at: at('2026-11-02T03:30:00Z'), intensity: 92 });
  });

  it('includes the start and excludes the end of a period', () => {
    expect(currentCarbon(day, at('2026-11-01T13:00:00Z'))!.intensity).toBe(183);
    expect(currentCarbon(day, at('2026-11-01T13:30:00Z'))!.intensity).toBe(190);
  });

  it('takes the earliest of equal lows', () => {
    const unsorted = fc([
      period('2026-11-01T13:00:00Z', 183),
      period('2026-11-02T05:00:00Z', 92),
      period('2026-11-02T03:30:00Z', 92),
    ]);
    expect(currentCarbon(unsorted, at('2026-11-01T13:10:00Z'))!.lowest).toEqual({ at: at('2026-11-02T03:30:00Z'), intensity: 92 });
  });

  it('has no lowest when nothing ahead is lower than now', () => {
    expect(currentCarbon(day, at('2026-11-02T04:10:00Z'))!.lowest).toBeNull();
    expect(currentCarbon(day, at('2026-11-02T05:10:00Z'))!.lowest).toBeNull();
  });

  it('ignores periods that are already over when looking ahead', () => {
    const c = currentCarbon(fc([period('2026-11-01T12:30:00Z', 10), period('2026-11-01T13:00:00Z', 50)]), at('2026-11-01T13:05:00Z'))!;
    expect(c.lowest).toBeNull();
  });

  it('hides when no period covers now: none, run out, or a clock before the forecast', () => {
    expect(currentCarbon(null, at('2026-11-01T13:10:00Z'))).toBeNull();
    expect(currentCarbon(fc([]), at('2026-11-01T13:10:00Z'))).toBeNull();
    expect(currentCarbon(day, at('2026-11-02T05:30:00Z'))).toBeNull();
    expect(currentCarbon(day, at('2026-11-01T12:59:59Z'))).toBeNull();
    expect(currentCarbon({ region: null, fetched_at: '' } as unknown as CarbonForecast, 0)).toBeNull();
  });

  it('skips malformed periods', () => {
    const bad = fc([
      { from: 'nope', to: '2026-11-01T13:30:00Z', forecast: 1, index: 'low' },
      { from: '2026-11-01T13:00:00Z', to: '2026-11-01T13:30:00Z', forecast: Number.NaN, index: 'low' },
      { from: '2026-11-01T13:00:00Z', to: '2026-11-01T13:30:00Z', forecast: -3, index: 'low' },
      period('2026-11-01T13:00:00Z', 140),
    ]);
    expect(currentCarbon(bad, at('2026-11-01T13:10:00Z'))!.intensity).toBe(140);
  });

  it('leaves an unknown band uncoloured', () => {
    const c = currentCarbon(fc([period('2026-11-01T13:00:00Z', 100, 'extreme')]), at('2026-11-01T13:10:00Z'))!;
    expect(c.band).toBeNull();
    expect(c.region).toBeNull();
  });
});

describe('carbon formatting', () => {
  it('formats the intensity', () => {
    expect(formatCarbon(182.6)).toBe('183 g/kWh');
  });

  it('describes it for screen readers', () => {
    const lowAt = new Date(2026, 10, 2, 3, 30).getTime();
    expect(carbonLabel({ intensity: 183, band: 'high', region: 'South England', lowest: { at: lowAt, intensity: 92 } }))
      .toBe('Grid carbon 183 g/kWh, high, South England. Lowest 03:30 at 92 g/kWh.');
    expect(carbonLabel({ intensity: 50, band: null, region: null, lowest: null })).toBe('Grid carbon 50 g/kWh.');
  });
});
