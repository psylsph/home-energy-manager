/**
 * Sunrise and sunset for the house view's sky (the moon is in `moon.ts`,
 * which shares the solar maths here). Worked out locally from the weather location's
 * coordinates with the standard sunrise equation
 * (<https://en.wikipedia.org/wiki/Sunrise_equation>), good to a minute or two
 * at UK latitudes, so it needs no web request. Pure, so it's unit-tested.
 */

export const DAY_MS = 86_400_000;
/** Julian date of the Unix epoch. */
export const J1970 = 2440587.5;
export const J2000 = 2451545;
/** Degrees to radians. */
export const rad = Math.PI / 180;
/** Obliquity of the ecliptic, degrees. */
export const OBLIQUITY_DEG = 23.4397;

const toJulian = (ms: number) => ms / DAY_MS + J1970;
const fromJulian = (j: number) => (j - J1970) * DAY_MS;

/** The sun's mean anomaly and ecliptic longitude (degrees), `d` days after J2000. */
export function solarLongitude(d: number): { m: number; lambda: number } {
  const m = (357.5291 + 0.98560028 * d) % 360;
  const c = 1.9148 * Math.sin(m * rad) + 0.02 * Math.sin(2 * m * rad) + 0.0003 * Math.sin(3 * m * rad);
  return { m, lambda: (m + c + 180 + 102.9372) % 360 };
}

/**
 * Sunrise and sunset (ms) around the solar noon `dayOffset` days from the one
 * nearest `now`, or `null` when the sun doesn't rise or doesn't set that day
 * (polar day or night).
 */
export function sunTimes(now: number, latitude: number, longitude: number, dayOffset = 0): { sunrise: number; sunset: number } | null {
  if (![now, latitude, longitude].every(Number.isFinite) || Math.abs(latitude) > 90) return null;
  // The day number whose mean solar noon is nearest `now`.
  const n = Math.round(toJulian(now) - J2000 + longitude / 360) + dayOffset;
  const meanNoon = n - longitude / 360;
  const { m, lambda } = solarLongitude(meanNoon);
  const transit = J2000 + meanNoon + 0.0053 * Math.sin(m * rad) - 0.0069 * Math.sin(2 * lambda * rad);
  const sinDec = Math.sin(lambda * rad) * Math.sin(OBLIQUITY_DEG * rad);
  const cosDec = Math.cos(Math.asin(sinDec));
  // Sun's centre 0.833° below the horizon: refraction plus its radius.
  const cosHour = (Math.sin(-0.833 * rad) - Math.sin(latitude * rad) * sinDec) / (Math.cos(latitude * rad) * cosDec);
  if (!(Math.abs(cosHour) <= 1)) return null;
  const halfDay = Math.acos(cosHour) / rad / 360;
  return { sunrise: fromJulian(transit - halfDay), sunset: fromJulian(transit + halfDay) };
}
