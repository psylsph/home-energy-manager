import { describe, it, expect } from 'vitest';
import { moonAltitude, moonLitPath, moonPass, moonPhase, skyScene } from '../../src/lib/moon';
import { sunTimes } from '../../src/lib/sun';

/**
 * The moon for the house view's sky. The anchors are eclipses, whose times
 * are fixed: the total lunar eclipse of 3 March 2026 (full moon, ~11:33 UTC)
 * and the total solar eclipse of 12 August 2026 (new moon, ~17:37 UTC).
 * Rise and set are checked against what must hold physically.
 */

const ALTON = { lat: 51.1058, lon: -1.048 };
const at = (iso: string) => Date.parse(iso);
const HOUR = 3_600_000;
const deg = (r: number) => (r * 180) / Math.PI;

describe('moonPhase', () => {
  it('is full at the March 2026 lunar eclipse', () => {
    const { phase, lit } = moonPhase(at('2026-03-03T11:33:00Z'));
    expect(lit).toBeGreaterThan(0.99);
    expect(phase).toBeCloseTo(0.5, 1);
  });

  it('is new at the August 2026 solar eclipse', () => {
    const { phase, lit } = moonPhase(at('2026-08-12T17:37:00Z'));
    expect(lit).toBeLessThan(0.01);
    expect(Math.min(phase, 1 - phase)).toBeLessThan(0.02);
  });

  it('waxes after new and wanes after full', () => {
    const waxing = moonPhase(at('2026-08-19T12:00:00Z'));
    expect(waxing.phase).toBeGreaterThan(0.15);
    expect(waxing.phase).toBeLessThan(0.35);
    const waning = moonPhase(at('2026-03-10T12:00:00Z'));
    expect(waning.phase).toBeGreaterThan(0.65);
    expect(waning.phase).toBeLessThan(0.85);
  });
});

describe('moonPass', () => {
  it('finds rise and set where the moon meets the horizon, with it up in between', () => {
    for (const iso of ['2026-11-01T22:00:00Z', '2026-03-03T12:00:00Z', '2026-08-15T03:00:00Z']) {
      const p = moonPass(at(iso), ALTON.lat, ALTON.lon)!;
      expect(p.set).toBeGreaterThan(p.rise);
      // A pass lasts roughly 8–17 hours at this latitude.
      expect((p.set - p.rise) / HOUR).toBeGreaterThan(7);
      expect((p.set - p.rise) / HOUR).toBeLessThan(18);
      for (const t of [p.rise, p.set]) {
        expect(Math.abs(deg(moonAltitude(t, ALTON.lat, ALTON.lon)) - 0.133)).toBeLessThan(0.05);
      }
      expect(moonAltitude((p.rise + p.set) / 2, ALTON.lat, ALTON.lon)).toBeGreaterThan(0);
      expect(moonAltitude(p.rise - HOUR, ALTON.lat, ALTON.lon)).toBeLessThan(0);
      expect(moonAltitude(p.set + HOUR, ALTON.lat, ALTON.lon)).toBeLessThan(0);
    }
  });

  it('rises around sunset at full moon and around sunrise at new moon', () => {
    const fullDay = at('2026-03-03T12:00:00Z');
    const full = moonPass(fullDay, ALTON.lat, ALTON.lon)!;
    expect(full.up).toBe(false);
    expect(Math.abs(full.rise - sunTimes(fullDay, ALTON.lat, ALTON.lon)!.sunset) / HOUR).toBeLessThan(1.5);

    const newDawn = at('2026-08-12T02:00:00Z');
    const fresh = moonPass(newDawn, ALTON.lat, ALTON.lon)!;
    expect(Math.abs(fresh.rise - sunTimes(newDawn, ALTON.lat, ALTON.lon)!.sunrise) / HOUR).toBeLessThan(1.5);
  });

  it('tracks how far through its pass the moon is while it is up', () => {
    const p = moonPass(at('2026-11-01T22:00:00Z'), ALTON.lat, ALTON.lon)!;
    const mid = moonPass((p.rise + p.set) / 2, ALTON.lat, ALTON.lon)!;
    expect(mid.up).toBe(true);
    expect(mid.fraction).toBeCloseTo(0.5, 2);
    expect(mid.rise).toBeCloseTo(p.rise, -4);
    expect(mid.set).toBeCloseTo(p.set, -4);
  });

  it('gives the next pass, at fraction 0, while the moon is down', () => {
    const p = moonPass(at('2026-03-03T12:00:00Z'), ALTON.lat, ALTON.lon)!;
    expect(p.up).toBe(false);
    expect(p.fraction).toBe(0);
    expect(p.rise).toBeGreaterThan(at('2026-03-03T12:00:00Z'));
  });

  it('finds the same rise and set to the millisecond as the minutes tick by', () => {
    // A minute's label must not flicker between neighbouring minutes.
    const t = at('2026-08-22T12:00:00Z');
    const first = moonPass(t, ALTON.lat, ALTON.lon)!;
    for (const later of [37_000, 60_000, 4 * 60_000, 3 * 3_600_000]) {
      const p = moonPass(t + later, ALTON.lat, ALTON.lon)!;
      expect(p.rise).toBe(first.rise);
      expect(p.set).toBe(first.set);
    }
  });

  it('is unknown for bad input', () => {
    expect(moonPass(Number.NaN, 51, 0)).toBeNull();
    expect(moonPass(at('2026-03-03T12:00:00Z'), 95, 0)).toBeNull();
  });
});

describe('moonLitPath', () => {
  const sweeps = (d: string) => [...d.matchAll(/A [\d.]+ [\d.]+ 0 0 (\d)/g)].map((m) => Number(m[1]));
  const rx = (d: string) => Number(d.match(/A [\d.]+ [\d.]+ 0 0 \d 0 [\d.]+ A ([\d.]+)/)![1]);

  it('lights the right side while waxing and the left while waning', () => {
    expect(sweeps(moonLitPath(0.1, 6))[0]).toBe(1);
    expect(sweeps(moonLitPath(0.9, 6))[0]).toBe(0);
  });

  it('bulges the terminator into the dark side for a gibbous moon and the lit side for a crescent', () => {
    expect(sweeps(moonLitPath(0.1, 6))[1]).toBe(0); // waxing crescent
    expect(sweeps(moonLitPath(0.4, 6))[1]).toBe(1); // waxing gibbous
    expect(sweeps(moonLitPath(0.6, 6))[1]).toBe(0); // waning gibbous
    expect(sweeps(moonLitPath(0.9, 6))[1]).toBe(1); // waning crescent
  });

  it('is a straight terminator at the quarters and a full disc at full', () => {
    expect(rx(moonLitPath(0.25, 6))).toBe(0);
    expect(rx(moonLitPath(0.75, 6))).toBe(0);
    expect(rx(moonLitPath(0.5, 6))).toBe(6);
  });
});

describe('skyScene', () => {
  const scene = (iso: string) => skyScene(at(iso), ALTON.lat, ALTON.lon)!;

  it('labels the sun by day and puts it along its arc', () => {
    const s = scene('2026-11-01T11:47:00Z');
    expect(s.labels.body).toBe('sun');
    expect(s.sun).toBeCloseTo(0.5, 1);
    const day = sunTimes(at('2026-11-01T11:47:00Z'), ALTON.lat, ALTON.lon)!;
    expect(s.labels.rise).toBe(day.sunrise);
    expect(s.labels.set).toBe(day.sunset);
  });

  it("labels tonight's moonrise, ending the arc at sunrise while the moon is still up then", () => {
    // Rises 22:25, sets 14:07 tomorrow afternoon: the arc covers the night only.
    const t = at('2026-11-01T18:00:00Z');
    const s = scene('2026-11-01T18:00:00Z');
    const pass = moonPass(t, ALTON.lat, ALTON.lon)!;
    const sunrise = sunTimes(t, ALTON.lat, ALTON.lon, 1)!.sunrise;
    expect(pass.set).toBeGreaterThan(sunrise);
    expect(s.labels).toEqual({ body: 'moon', rise: pass.rise, set: sunrise });
    expect(s.sun).toBeNull();
    expect(s.moon).toBeNull();
  });

  it('puts the moon along its night-time path once it is up', () => {
    const t = at('2026-11-02T03:00:00Z');
    const s = scene('2026-11-02T03:00:00Z');
    const pass = moonPass(t, ALTON.lat, ALTON.lon)!;
    const sunrise = sunTimes(t, ALTON.lat, ALTON.lon)!.sunrise;
    expect(s.labels.body).toBe('moon');
    expect(s.labels.set).toBe(sunrise);
    expect(s.moon!.fraction).toBeCloseTo((t - pass.rise) / (sunrise - pass.rise), 6);
    expect(s.moon!.south).toBe(false);
  });

  it('starts the arc at sunset for a moon already up before dark', () => {
    // A waxing gibbous rose in the late afternoon, before sunset.
    const t = at('2026-08-24T23:09:00Z');
    const pass = moonPass(t, ALTON.lat, ALTON.lon)!;
    const sunset = sunTimes(t, ALTON.lat, ALTON.lon)!.sunset;
    expect(pass.up).toBe(true);
    expect(pass.rise).toBeLessThan(sunset);
    const s = skyScene(t, ALTON.lat, ALTON.lon)!;
    expect(s.labels.rise).toBe(sunset);
    expect(s.labels.set).toBe(Math.min(pass.set, sunTimes(t, ALTON.lat, ALTON.lon, 1)!.sunrise));
    expect(s.moon!.fraction).toBeCloseTo((t - sunset) / (s.labels.set - sunset), 6);
  });

  it('ends the arc at moonset when the moon sets before dawn', () => {
    // Near first quarter the moon sets around midnight.
    const t = at('2026-08-20T20:30:00Z');
    const pass = moonPass(t, ALTON.lat, ALTON.lon)!;
    expect(pass.up).toBe(true);
    expect(pass.set).toBeLessThan(sunTimes(t, ALTON.lat, ALTON.lon, 1)!.sunrise);
    expect(skyScene(t, ALTON.lat, ALTON.lon)!.labels.set).toBe(pass.set);
  });

  it('leaves the moon off by day, where its place would not match the sun\'s times', () => {
    // Up since late last night, setting mid-afternoon.
    expect(moonPass(at('2026-11-02T11:00:00Z'), ALTON.lat, ALTON.lon)!.up).toBe(true);
    const s = scene('2026-11-02T11:00:00Z');
    expect(s.labels.body).toBe('sun');
    expect(s.sun).not.toBeNull();
    expect(s.moon).toBeNull();
  });

  it("keeps the sun's times at night when the moon won't be up before sunrise", () => {
    // A waxing crescent has set; it next rises around midday tomorrow.
    const t = at('2026-08-17T23:23:00Z');
    const pass = moonPass(t, ALTON.lat, ALTON.lon)!;
    expect(pass.up).toBe(false);
    expect(pass.rise).toBeGreaterThan(sunTimes(t, ALTON.lat, ALTON.lon, 1)!.sunrise);
    const s = skyScene(t, ALTON.lat, ALTON.lon)!;
    expect(s.labels.body).toBe('sun');
    expect(s.moon).toBeNull();
  });

  it('flags the southern hemisphere for a mirrored moon', () => {
    expect(skyScene(at('2026-11-02T18:00:00Z'), -33.87, 151.21)!.moon!.south).toBe(true);
  });

  it('is unknown where the sun never sets', () => {
    expect(skyScene(at('2026-06-21T12:00:00Z'), 78.2, 15.6)).toBeNull();
  });
});
