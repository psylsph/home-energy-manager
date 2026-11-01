import { describe, it, expect } from 'vitest';
import { sunTimes } from '../../src/lib/sun';

/** Sunrise and sunset for the house view's sky. */

const LONDON = { lat: 51.5074, lon: -0.1278 };
const EDINBURGH = { lat: 55.9533, lon: -3.1883 };
const at = (iso: string) => Date.parse(iso);
const MIN = 60_000;

/** Within `mins` minutes of the published time. */
function near(ms: number, iso: string, mins = 2) {
  expect(Math.abs(ms - at(iso)) / MIN).toBeLessThanOrEqual(mins);
}

describe('sunTimes', () => {
  // Published times (timeanddate.com), UTC.
  it('matches London at midsummer and midwinter', () => {
    const june = sunTimes(at('2026-06-21T12:00:00Z'), LONDON.lat, LONDON.lon)!;
    near(june.sunrise, '2026-06-21T03:43:00Z');
    near(june.sunset, '2026-06-21T20:21:00Z');
    const dec = sunTimes(at('2026-12-21T12:00:00Z'), LONDON.lat, LONDON.lon)!;
    near(dec.sunrise, '2026-12-21T08:04:00Z');
    near(dec.sunset, '2026-12-21T15:53:00Z');
  });

  it('matches Edinburgh, further north and west', () => {
    const dec = sunTimes(at('2026-12-21T12:00:00Z'), EDINBURGH.lat, EDINBURGH.lon)!;
    near(dec.sunrise, '2026-12-21T08:42:00Z');
    near(dec.sunset, '2026-12-21T15:40:00Z');
  });

  it('picks the same day from early morning to late evening', () => {
    const early = sunTimes(at('2026-11-01T00:30:00Z'), LONDON.lat, LONDON.lon)!;
    const late = sunTimes(at('2026-11-01T23:30:00Z'), LONDON.lat, LONDON.lon)!;
    expect(early).toEqual(late);
    near(early.sunrise, '2026-11-01T06:54:00Z');
  });

  it('steps to the neighbouring days', () => {
    const today = sunTimes(at('2026-11-01T12:00:00Z'), LONDON.lat, LONDON.lon)!;
    const tomorrow = sunTimes(at('2026-11-01T12:00:00Z'), LONDON.lat, LONDON.lon, 1)!;
    const gap = (tomorrow.sunrise - today.sunrise) / MIN;
    expect(gap).toBeGreaterThan(24 * 60);
    expect(gap).toBeLessThan(24 * 60 + 3);
  });

  it('has no sunrise or sunset in polar day or night, or for bad input', () => {
    expect(sunTimes(at('2026-06-21T12:00:00Z'), 78.2, 15.6)).toBeNull();
    expect(sunTimes(at('2026-12-21T12:00:00Z'), 78.2, 15.6)).toBeNull();
    expect(sunTimes(Number.NaN, 51, 0)).toBeNull();
    expect(sunTimes(at('2026-06-21T12:00:00Z'), 91, 0)).toBeNull();
  });
});
