/**
 * Current-weather display for the house view (issue #422).
 *
 * Open-Meteo reports a WMO weather interpretation code
 * (<https://open-meteo.com/en/docs>, "WMO Weather interpretation codes") and an
 * `is_day` flag. This maps them onto a small set of icons and decides whether
 * the reading is fresh enough to show at all. Pure, so it's unit-tested
 * without a backend.
 */

export type WeatherIconKind =
  | 'clear-day'
  | 'clear-night'
  | 'partly-day'
  | 'partly-night'
  | 'cloudy'
  | 'fog'
  | 'drizzle'
  | 'rain'
  | 'snow'
  | 'thunder';

/** Icon for a WMO code. Unknown or missing codes fall back to a plain cloud. */
export function weatherIconKind(code: number | null | undefined, isDay: boolean | null | undefined): WeatherIconKind {
  const day = isDay !== false;
  if (code == null || !Number.isFinite(code)) return 'cloudy';
  if (code === 0) return day ? 'clear-day' : 'clear-night';
  if (code === 1 || code === 2) return day ? 'partly-day' : 'partly-night';
  if (code === 3) return 'cloudy';
  if (code === 45 || code === 48) return 'fog';
  if (code >= 51 && code <= 57) return 'drizzle';
  if ((code >= 61 && code <= 67) || (code >= 80 && code <= 82)) return 'rain';
  if ((code >= 71 && code <= 77) || code === 85 || code === 86) return 'snow';
  if (code >= 95 && code <= 99) return 'thunder';
  return 'cloudy';
}

/** Plain-English description, used as the icon's accessible name. */
export function weatherDescription(kind: WeatherIconKind): string {
  switch (kind) {
    case 'clear-day':
    case 'clear-night':
      return 'Clear';
    case 'partly-day':
    case 'partly-night':
      return 'Partly cloudy';
    case 'cloudy':
      return 'Cloudy';
    case 'fog':
      return 'Fog';
    case 'drizzle':
      return 'Drizzle';
    case 'rain':
      return 'Rain';
    case 'snow':
      return 'Snow';
    case 'thunder':
      return 'Thunderstorm';
  }
}

/** The subset of `GET /api/weather` the house view reads. */
export interface LiveWeatherState {
  config: { enabled: boolean };
  last_fetch_at: string | null;
  last_fetched_temperature_c: number | null;
  last_fetched_weather_code?: number | null;
  last_fetched_is_day?: boolean | null;
  last_fetched_solar_w_m2?: number | null;
}

/** A reading older than this is hidden rather than shown as current. */
export const WEATHER_MAX_AGE_MS = 60 * 60 * 1000;

export interface CurrentWeather {
  temperatureC: number;
  kind: WeatherIconKind;
  description: string;
  /** Solar irradiance (W/m²), or `null` when unknown or at night with none. */
  solarWm2: number | null;
}

/** Bars in the solar-intensity gauge; each is 200 W/m² (clear midday sun is about 1000). */
export const SOLAR_LEVEL_BARS = 5;
const W_M2_PER_BAR = 200;

/** Lit bars for an irradiance: any sun lights one, 1000 W/m² or more lights all five. */
export function solarLevel(wM2: number): number {
  if (!Number.isFinite(wM2) || wM2 <= 0) return 0;
  return Math.min(SOLAR_LEVEL_BARS, Math.ceil(wM2 / W_M2_PER_BAR));
}

/** Irradiance as shown under the temperature, e.g. `"486 W/m²"`. */
export function formatIrradiance(wM2: number): string {
  return `${Math.round(wM2)} W/m²`;
}

/**
 * What to show, or `null` to hide the weather: when the integration is off,
 * nothing has been fetched yet, or the last fetch is more than an hour old
 * (or stamped in the future by more than that — a wrong clock).
 */
export function currentWeather(state: LiveWeatherState | null | undefined, now: number): CurrentWeather | null {
  if (!state || !state.config?.enabled) return null;
  const temp = state.last_fetched_temperature_c;
  if (temp == null || !Number.isFinite(temp)) return null;
  if (!state.last_fetch_at) return null;
  const fetchedAt = Date.parse(state.last_fetch_at);
  if (!Number.isFinite(fetchedAt) || Math.abs(now - fetchedAt) > WEATHER_MAX_AGE_MS) return null;
  const kind = weatherIconKind(state.last_fetched_weather_code, state.last_fetched_is_day);
  const solar = state.last_fetched_solar_w_m2;
  // Hide a bad value, and a zero unless it's known to be daytime (a zero at
  // night, or with no day flag, says nothing); a daytime zero is real.
  const solarWm2 = solar == null || !Number.isFinite(solar) || solar < 0 || (solar === 0 && state.last_fetched_is_day !== true)
    ? null
    : solar;
  return { temperatureC: temp, kind, description: weatherDescription(kind), solarWm2 };
}

/** Temperature as shown next to the icon, e.g. `"10 °C"`. */
export function formatWeatherTemp(c: number): string {
  return `${Math.round(c)} °C`;
}
