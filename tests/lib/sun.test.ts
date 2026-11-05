import { describe, it, expect } from 'vitest';
import { skyScene, sunTimes } from '../../src/lib/sun';

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

describe('skyScene', () => {
  const day = sunTimes(at('2026-11-01T12:00:00Z'), LONDON.lat, LONDON.lon)!;
  const next = sunTimes(at('2026-11-01T12:00:00Z'), LONDON.lat, LONDON.lon, 1)!;
  const scene = (now: number) => skyScene(now, LONDON.lat, LONDON.lon)!;

  it('puts the sun at the start of the arc at sunrise', () => {
    expect(scene(day.sunrise)).toEqual({ rise: day.sunrise, set: day.sunset, sun: 0 });
  });

  it('puts the sun halfway along at the middle of the day', () => {
    const s = scene((day.sunrise + day.sunset) / 2);
    expect(s.sun).toBeCloseTo(0.5, 6);
    expect(s.rise).toBe(day.sunrise);
  });

  it('has the sun still up one minute before sunset, and in 0..1', () => {
    const s = scene(day.sunset - MIN);
    expect(s.sun).toBeGreaterThan(0.99);
    expect(s.sun).toBeLessThan(1);
  });

  it('has the sun down at sunset itself, and shows the coming day\'s times', () => {
    expect(scene(day.sunset)).toEqual({ rise: next.sunrise, set: next.sunset, sun: null });
    expect(scene(at('2026-11-01T23:30:00Z'))).toEqual({ rise: next.sunrise, set: next.sunset, sun: null });
  });

  it("keeps today's times, with no sun, before dawn", () => {
    expect(scene(day.sunrise - MIN)).toEqual({ rise: day.sunrise, set: day.sunset, sun: null });
  });

  it('is null in polar day or night and for bad input', () => {
    expect(skyScene(at('2026-06-21T12:00:00Z'), 78.2, 15.6)).toBeNull();
    expect(skyScene(at('2026-12-21T12:00:00Z'), 78.2, 15.6)).toBeNull();
    expect(skyScene(Number.NaN, 51, 0)).toBeNull();
    expect(skyScene(at('2026-11-01T12:00:00Z'), Number.NaN, 0)).toBeNull();
  });

  it("keeps the last day with a sunrise when tomorrow hasn't one", () => {
    // 68°N in December: the polar night closes in, and the day after the last
    // sunrise has none. Asked just after that day's sunset.
    const now = at('2026-12-09T13:00:00Z');
    const last = sunTimes(now, 68, 0)!;
    expect(sunTimes(now, 68, 0, 1)).toBeNull();
    expect(skyScene(now, 68, 0)).toEqual({ rise: last.sunrise, set: last.sunset, sun: null });
  });
});
