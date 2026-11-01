/**
 * Sunrise, sunset and where the sun (or moon) sits along its daily arc, for
 * the house view's sky. Worked out locally from the weather location's
 * coordinates with the standard sunrise equation
 * (<https://en.wikipedia.org/wiki/Sunrise_equation>), good to a minute or two
 * at UK latitudes, so it needs no web request. Pure, so it's unit-tested.
 */

const DAY_MS = 86_400_000;
/** Julian date of the Unix epoch. */
const J1970 = 2440587.5;
const J2000 = 2451545;
const rad = Math.PI / 180;

const toJulian = (ms: number) => ms / DAY_MS + J1970;
const fromJulian = (j: number) => (j - J1970) * DAY_MS;

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
  const m = (357.5291 + 0.98560028 * meanNoon) % 360;
  const c = 1.9148 * Math.sin(m * rad) + 0.02 * Math.sin(2 * m * rad) + 0.0003 * Math.sin(3 * m * rad);
  const lambda = (m + c + 180 + 102.9372) % 360;
  const transit = J2000 + meanNoon + 0.0053 * Math.sin(m * rad) - 0.0069 * Math.sin(2 * lambda * rad);
  const sinDec = Math.sin(lambda * rad) * Math.sin(23.4397 * rad);
  const cosDec = Math.cos(Math.asin(sinDec));
  // Sun's centre 0.833° below the horizon: refraction plus its radius.
  const cosHour = (Math.sin(-0.833 * rad) - Math.sin(latitude * rad) * sinDec) / (Math.cos(latitude * rad) * cosDec);
  if (!(Math.abs(cosHour) <= 1)) return null;
  const halfDay = Math.acos(cosHour) / rad / 360;
  return { sunrise: fromJulian(transit - halfDay), sunset: fromJulian(transit + halfDay) };
}

export interface SkyPosition {
  /** The sun is up, or it's night and the moon takes its path. */
  body: 'sun' | 'moon';
  /** 0 at the start of the arc (sunrise, or sunset for the moon), 1 at its end. */
  fraction: number;
  /** Today's sunrise and sunset (ms), for the labels. */
  sunrise: number;
  sunset: number;
}

/** Where along the sky's arc to draw at `now`, or `null` with no sunrise or sunset to go by. */
export function skyPosition(now: number, latitude: number, longitude: number): SkyPosition | null {
  const today = sunTimes(now, latitude, longitude);
  if (!today) return null;
  const { sunrise, sunset } = today;
  if (now >= sunrise && now < sunset) {
    return { body: 'sun', fraction: (now - sunrise) / (sunset - sunrise), sunrise, sunset };
  }
  // Night: from the last sunset to the next sunrise.
  const before = now < sunrise;
  const other = sunTimes(now, latitude, longitude, before ? -1 : 1);
  if (!other) return null;
  const [from, to] = before ? [other.sunset, sunrise] : [sunset, other.sunrise];
  return { body: 'moon', fraction: Math.min(1, Math.max(0, (now - from) / (to - from))), sunrise, sunset };
}

/** Local clock time, e.g. `"07:02"`. */
export function formatClock(ms: number): string {
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}
