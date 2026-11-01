import { useEffect, useState } from 'react';
import { apiGet } from '../lib/api';
import {
  currentWeather,
  weatherLocation,
  type CurrentWeather,
  type LiveWeatherState,
  type WeatherLocation,
} from '../lib/weatherIcon';
import { useNow } from './useNow';

/** Matches the backend's 15-minute live fetch interval. */
export const LIVE_WEATHER_POLL_MS = 15 * 60 * 1000;

/**
 * The current weather for the house view (issue #422) — `null` when the
 * weather integration is off, hasn't fetched yet, or its reading is stale —
 * and the configured location, which stays while weather is on even when
 * the reading goes stale (the sky's sun position needs no fetch).
 * Polls `GET /api/weather` only while mounted; a failed request keeps the
 * last good state, which ages out on its own after an hour.
 */
export function useLiveWeather(): { weather: CurrentWeather | null; location: WeatherLocation | null } {
  const [state, setState] = useState<LiveWeatherState | null>(null);
  // Re-check freshness every minute so a stale reading disappears even if
  // the backend stops answering.
  const now = useNow(60_000);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const res = await apiGet<{ ok: boolean; data: LiveWeatherState | null }>('/api/weather');
        if (!cancelled && res.ok) setState(res.data);
      } catch {
        // Keep the last state; it ages out via `currentWeather`.
      }
    };
    void load();
    const timer = setInterval(() => void load(), LIVE_WEATHER_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  return { weather: currentWeather(state, now), location: weatherLocation(state) };
}
