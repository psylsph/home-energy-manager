/**
 * Sunrise and sunset for the house view's sky. Worked out locally from the
 * weather location's coordinates with the standard sunrise equation
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

/** What the house view's sky band shows at a moment. */
export interface SkyScene {
  /** The day's sunrise and sunset (ms): the times beside the arc's ends. */
  rise: number;
  set: number;
  /** The sun's place along the arc (0 sunrise, 1 sunset), while it's up. */
  sun: number | null;
}

/**
 * The sky band at `now`: the day's arc with sunrise and sunset at its ends, and
 * where the sun is on it while it's up. Once the sun has set the ends show the
 * coming day's times (the sun isn't on the arc until then). `null` where the
 * sun doesn't rise or set, which is where the band is left off entirely.
 */
export function skyScene(now: number, latitude: number, longitude: number): SkyScene | null {
  const day = sunTimes(now, latitude, longitude);
  if (!day) return null;
  if (now >= day.sunset) {
    // Tomorrow's times if there are any; a polar edge keeps tonight's.
    const next = sunTimes(now, latitude, longitude, 1);
    if (next && next.sunrise > now) return { rise: next.sunrise, set: next.sunset, sun: null };
    return { rise: day.sunrise, set: day.sunset, sun: null };
  }
  const up = now >= day.sunrise;
  return {
    rise: day.sunrise,
    set: day.sunset,
    sun: up ? (now - day.sunrise) / (day.sunset - day.sunrise) : null,
  };
}
