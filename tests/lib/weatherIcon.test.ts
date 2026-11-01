import { describe, it, expect } from 'vitest';
import {
  WEATHER_MAX_AGE_MS,
  currentWeather,
  formatWeatherTemp,
  weatherDescription,
  weatherIconKind,
  type LiveWeatherState,
  type WeatherIconKind,
} from '../../src/lib/weatherIcon';

/** House-view weather (issue #422): WMO code → icon, and when to hide it. */

describe('weatherIconKind', () => {
  it.each<[number | null | undefined, boolean | null | undefined, WeatherIconKind]>([
    [0, true, 'clear-day'],
    [0, false, 'clear-night'],
    [1, true, 'partly-day'],
    [2, false, 'partly-night'],
    [3, true, 'cloudy'],
    [45, true, 'fog'],
    [48, false, 'fog'],
    [51, true, 'drizzle'],
    [57, true, 'drizzle'],
    [61, true, 'rain'],
    [67, true, 'rain'],
    [80, true, 'rain'],
    [82, true, 'rain'],
    [71, true, 'snow'],
    [77, true, 'snow'],
    [85, true, 'snow'],
    [86, true, 'snow'],
    [95, true, 'thunder'],
    [99, true, 'thunder'],
    [4, true, 'cloudy'],
    [null, true, 'cloudy'],
    [undefined, undefined, 'cloudy'],
    [Number.NaN, true, 'cloudy'],
  ])('code %s, day %s → %s', (code, isDay, kind) => {
    expect(weatherIconKind(code, isDay)).toBe(kind);
  });

  it('assumes daytime when the day flag is missing', () => {
    expect(weatherIconKind(0, undefined)).toBe('clear-day');
    expect(weatherIconKind(0, null)).toBe('clear-day');
  });
});

describe('weatherDescription', () => {
  it.each<[WeatherIconKind, string]>([
    ['clear-day', 'Clear'],
    ['clear-night', 'Clear'],
    ['partly-day', 'Partly cloudy'],
    ['partly-night', 'Partly cloudy'],
    ['cloudy', 'Cloudy'],
    ['fog', 'Fog'],
    ['drizzle', 'Drizzle'],
    ['rain', 'Rain'],
    ['snow', 'Snow'],
    ['thunder', 'Thunderstorm'],
  ])('%s → %s', (kind, text) => {
    expect(weatherDescription(kind)).toBe(text);
  });
});

describe('currentWeather', () => {
  const NOW = Date.parse('2026-10-31T12:00:00Z');
  const state = (over: Partial<LiveWeatherState> = {}): LiveWeatherState => ({
    config: { enabled: true },
    last_fetch_at: '2026-10-31T11:50:00Z',
    last_fetched_temperature_c: 10.4,
    last_fetched_weather_code: 61,
    last_fetched_is_day: true,
    ...over,
  });

  it('shows a fresh reading', () => {
    expect(currentWeather(state(), NOW)).toEqual({ temperatureC: 10.4, kind: 'rain', description: 'Rain' });
  });

  it('hides when there is no state or the integration is off', () => {
    expect(currentWeather(null, NOW)).toBeNull();
    expect(currentWeather(undefined, NOW)).toBeNull();
    expect(currentWeather(state({ config: { enabled: false } }), NOW)).toBeNull();
  });

  it('hides before the first fetch', () => {
    expect(currentWeather(state({ last_fetched_temperature_c: null }), NOW)).toBeNull();
    expect(currentWeather(state({ last_fetch_at: null }), NOW)).toBeNull();
    expect(currentWeather(state({ last_fetched_temperature_c: Number.NaN }), NOW)).toBeNull();
  });

  it('hides a reading older than an hour, keeps one just inside', () => {
    const at = (ms: number) => new Date(NOW - ms).toISOString();
    expect(currentWeather(state({ last_fetch_at: at(WEATHER_MAX_AGE_MS) }), NOW)).not.toBeNull();
    expect(currentWeather(state({ last_fetch_at: at(WEATHER_MAX_AGE_MS + 1000) }), NOW)).toBeNull();
  });

  it('hides a reading stamped well in the future (a wrong clock)', () => {
    expect(currentWeather(state({ last_fetch_at: new Date(NOW + WEATHER_MAX_AGE_MS + 1000).toISOString() }), NOW)).toBeNull();
  });

  it('hides an unparseable timestamp', () => {
    expect(currentWeather(state({ last_fetch_at: 'yesterday' }), NOW)).toBeNull();
  });

  it('falls back to a cloud when an older backend sends no code', () => {
    const s = state();
    delete s.last_fetched_weather_code;
    delete s.last_fetched_is_day;
    expect(currentWeather(s, NOW)?.kind).toBe('cloudy');
  });
});

describe('formatWeatherTemp', () => {
  it('rounds to whole degrees', () => {
    expect(formatWeatherTemp(10.4)).toBe('10 °C');
    expect(formatWeatherTemp(-0.6)).toBe('-1 °C');
  });
});
