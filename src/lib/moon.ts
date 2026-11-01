/**
 * The moon for the house view's sky: where it is, when it rises and sets, and
 * its phase. Low-precision lunar theory (the formulas the SunCalc library
 * uses, after Meeus, "Astronomical Algorithms"), good to a few minutes for
 * rise and set at UK latitudes, so it needs no web request. Pure, so it's
 * unit-tested.
 */

import { DAY_MS, J1970, J2000, OBLIQUITY_DEG, rad, solarLongitude, sunTimes } from './sun';

const OBLIQUITY = OBLIQUITY_DEG * rad;
/** Moon's centre altitude at rise and set: parallax less refraction and radius. */
const RISE_ALTITUDE = 0.133 * rad;

const toDays = (ms: number) => ms / DAY_MS + J1970 - J2000;

const rightAscension = (l: number, b: number) =>
  Math.atan2(Math.sin(l) * Math.cos(OBLIQUITY) - Math.tan(b) * Math.sin(OBLIQUITY), Math.cos(l));
const declination = (l: number, b: number) =>
  Math.asin(Math.sin(b) * Math.cos(OBLIQUITY) + Math.cos(b) * Math.sin(OBLIQUITY) * Math.sin(l));

function sunCoords(d: number) {
  const l = solarLongitude(d).lambda * rad;
  return { dec: declination(l, 0), ra: rightAscension(l, 0) };
}

function moonCoords(d: number) {
  const l0 = rad * (218.316 + 13.176396 * d);
  const m = rad * (134.963 + 13.064993 * d);
  const f = rad * (93.272 + 13.22935 * d);
  const l = l0 + rad * 6.289 * Math.sin(m);
  const b = rad * 5.128 * Math.sin(f);
  return { ra: rightAscension(l, b), dec: declination(l, b), distKm: 385001 - 20905 * Math.cos(m) };
}

/** The moon's altitude (radians, before refraction) at `ms` from the given place. */
export function moonAltitude(ms: number, latitude: number, longitude: number): number {
  const d = toDays(ms);
  const c = moonCoords(d);
  const sidereal = rad * (280.16 + 360.9856235 * d) + rad * longitude;
  const hourAngle = sidereal - c.ra;
  const phi = rad * latitude;
  return Math.asin(Math.sin(phi) * Math.sin(c.dec) + Math.cos(phi) * Math.cos(c.dec) * Math.cos(hourAngle));
}

/**
 * The moon's phase at `ms`: 0 new, 0.25 first quarter, 0.5 full, 0.75 last
 * quarter; `lit` is the fraction of the disc lit (0–1).
 */
export function moonPhase(ms: number): { phase: number; lit: number } {
  const d = toDays(ms);
  const s = sunCoords(d);
  const m = moonCoords(d);
  const sunKm = 149598000;
  const elongation = Math.acos(
    Math.sin(s.dec) * Math.sin(m.dec) + Math.cos(s.dec) * Math.cos(m.dec) * Math.cos(s.ra - m.ra),
  );
  const inc = Math.atan2(sunKm * Math.sin(elongation), m.distKm - sunKm * Math.cos(elongation));
  const angle = Math.atan2(
    Math.cos(s.dec) * Math.sin(s.ra - m.ra),
    Math.sin(s.dec) * Math.cos(m.dec) - Math.cos(s.dec) * Math.sin(m.dec) * Math.cos(s.ra - m.ra),
  );
  return { phase: 0.5 + (0.5 * inc * (angle < 0 ? -1 : 1)) / Math.PI, lit: (1 + Math.cos(inc)) / 2 };
}

const STEP_MS = 10 * 60_000;
/** Far enough to find a rise or set: the moon's day is ~24 h 50 min. */
const SEARCH_MS = 30 * 3_600_000;

/**
 * The first time after `from` (searching forwards, or backwards when `dir`
 * is -1) that the moon crosses the horizon going `up` (or down), to the
 * second, or `null` if it doesn't within 30 hours. The search steps along
 * fixed 10-minute marks, not from `from` itself, so it finds the same
 * crossing to the millisecond from one minute to the next.
 */
function crossing(from: number, latitude: number, longitude: number, up: boolean, dir: 1 | -1): number | null {
  const above = (t: number) => moonAltitude(t, latitude, longitude) > RISE_ALTITUDE;
  let a = dir === 1 ? Math.floor(from / STEP_MS) * STEP_MS : Math.ceil(from / STEP_MS) * STEP_MS;
  let aboveA = above(a);
  for (let step = 0; step * STEP_MS < SEARCH_MS; step++) {
    const b = a + dir * STEP_MS;
    const aboveB = above(b);
    // In time order: below → above is a rise, above → below a set.
    const [early, late, earlyAbove, lateAbove] = dir === 1 ? [a, b, aboveA, aboveB] : [b, a, aboveB, aboveA];
    if (earlyAbove !== lateAbove && lateAbove === up) {
      let lo = early;
      let hi = late;
      while (hi - lo > 1000) {
        const mid = (lo + hi) / 2;
        if (above(mid) === earlyAbove) lo = mid;
        else hi = mid;
      }
      const found = Math.round((lo + hi) / 2);
      // The first mark's step can straddle `from`: a crossing on the wrong
      // side of it isn't the one asked for.
      if (dir === 1 ? found > from : found < from) return found;
    }
    a = b;
    aboveA = aboveB;
  }
  return null;
}

export interface MoonPass {
  /** The moon is above the horizon now. */
  up: boolean;
  /** This pass's rise and set (ms): the current one while it's up, otherwise the next. */
  rise: number;
  set: number;
  /** How far through the pass (0 rise, 1 set); 0 while it's down. */
  fraction: number;
  /** 0 new, 0.25 first quarter, 0.5 full, 0.75 last quarter. */
  phase: number;
}

/** The moon's current (or next) pass across the sky, or `null` when no rise or set is found. */
export function moonPass(now: number, latitude: number, longitude: number): MoonPass | null {
  if (![now, latitude, longitude].every(Number.isFinite) || Math.abs(latitude) > 90) return null;
  const up = moonAltitude(now, latitude, longitude) > RISE_ALTITUDE;
  const rise = up ? crossing(now, latitude, longitude, true, -1) : crossing(now, latitude, longitude, true, 1);
  if (rise == null) return null;
  const set = crossing(up ? now : rise, latitude, longitude, false, 1);
  if (set == null || set <= rise) return null;
  const fraction = up ? Math.min(1, Math.max(0, (now - rise) / (set - rise))) : 0;
  return { up, rise, set, fraction, phase: moonPhase(now).phase };
}

/**
 * The lit part of a moon of radius `r` centred on the origin, as a path:
 * the lit limb plus the terminator, an ellipse half as wide as the phase is
 * from a quarter. Waxing is lit on the right (as seen from the northern
 * hemisphere); the caller mirrors it for the southern.
 */
export function moonLitPath(phase: number, r: number): string {
  const p = ((phase % 1) + 1) % 1;
  const waxing = p < 0.5;
  // Distance from the nearest quarter decides the terminator's width.
  const rx = Math.abs(Math.cos(2 * Math.PI * p)) * r;
  const gibbous = p > 0.25 && p < 0.75;
  const limb = waxing ? 1 : 0;
  const terminator = waxing !== gibbous ? 0 : 1;
  const n = (v: number) => Number(v.toFixed(2));
  return `M 0 ${-r} A ${r} ${r} 0 0 ${limb} 0 ${r} A ${n(rx)} ${r} 0 0 ${terminator} 0 ${-r} Z`;
}

/** What the house view's sky shows at a moment. */
export interface SkyScene {
  /** Whose rise and set label the arc's ends: the sun's by day, the moon's by night. */
  labels: { body: 'sun' | 'moon'; rise: number; set: number };
  /** The sun's place along the arc (0 sunrise, 1 sunset), while it's up. */
  sun: number | null;
  /**
   * The moon's place along tonight's arc, while it's up; `south` mirrors it,
   * as it's lit the other way round south of the equator.
   */
  moon: { fraction: number; phase: number; south: boolean } | null;
}

/**
 * The sky at `now`. By day: the sun along its arc, labelled with sunrise and
 * sunset (no moon — its place along its own pass wouldn't match those
 * times). By night, while the moon is up or rises before the next sunrise:
 * its path through the night — from moonrise, or sunset if it was already
 * up, to moonset, or sunrise if it's still up then — with the moon along it
 * while it's up; otherwise the sun's times. Both ends always fall within
 * tonight. `null` where the sun doesn't rise or set.
 */
export function skyScene(now: number, latitude: number, longitude: number): SkyScene | null {
  const day = sunTimes(now, latitude, longitude);
  if (!day) return null;
  const sunLabels = { body: 'sun' as const, rise: day.sunrise, set: day.sunset };
  if (now >= day.sunrise && now < day.sunset) {
    return { labels: sunLabels, sun: (now - day.sunrise) / (day.sunset - day.sunrise), moon: null };
  }
  const before = now < day.sunrise;
  const lastSunset = before ? sunTimes(now, latitude, longitude, -1)?.sunset : day.sunset;
  const nextSunrise = before ? day.sunrise : sunTimes(now, latitude, longitude, 1)?.sunrise;
  const pass = moonPass(now, latitude, longitude);
  if (!pass || lastSunset == null || nextSunrise == null || (!pass.up && pass.rise >= nextSunrise)) {
    return { labels: sunLabels, sun: null, moon: null };
  }
  const start = Math.max(pass.rise, lastSunset);
  const end = Math.min(pass.set, nextSunrise);
  return {
    labels: { body: 'moon', rise: start, set: end },
    sun: null,
    moon: pass.up
      ? { fraction: Math.min(1, Math.max(0, (now - start) / (end - start))), phase: pass.phase, south: latitude < 0 }
      : null,
  };
}
