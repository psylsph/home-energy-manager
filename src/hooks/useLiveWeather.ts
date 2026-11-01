import { useEffect, useState } from 'react';
import { apiGet } from '../lib/api';
import { currentWeather, type CurrentWeather, type LiveWeatherState } from '../lib/weatherIcon';
import { useNow } from './useNow';

/** Matches the backend's 15-minute live fetch interval. */
export const LIVE_WEATHER_POLL_MS = 15 * 60 * 1000;

/**
 * The current weather for the house view (issue #422), or `null` when the
 * weather integration is off, hasn't fetched yet, or its reading is stale.
 * Polls `GET /api/weather` only while mounted; a failed request keeps the
 * last good state, which ages out on its own after an hour.
 */
export function useLiveWeather(): CurrentWeather | null {
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

  return currentWeather(state, now);
}
