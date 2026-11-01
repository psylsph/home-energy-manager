import { useEffect, useState } from 'react';
import { apiGet } from '../lib/api';
import { currentCarbon, type CarbonForecast, type CurrentCarbon } from '../lib/carbonIntensity';
import { useNow } from './useNow';

/**
 * Matches the backend's 5-minute retry gap, so a missed first fetch shows up
 * soon; the backend answers most polls from its 30-minute cache.
 */
export const CARBON_POLL_MS = 5 * 60 * 1000;

/**
 * Grid carbon intensity for the house view (issue #401), or `null` when
 * there's nothing current to show (weather off, or no forecast). Polls
 * `GET /api/carbon-intensity` only while mounted, so nothing is fetched
 * unless the house view is open; a failed request keeps the last forecast,
 * which runs out on its own.
 */
export function useCarbonIntensity(): CurrentCarbon | null {
  const [forecast, setForecast] = useState<CarbonForecast | null>(null);
  // Re-pick the current half hour every minute between polls.
  const now = useNow(60_000);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const res = await apiGet<{ ok: boolean; data: CarbonForecast | null }>('/api/carbon-intensity');
        if (!cancelled && res.ok) setForecast(res.data);
      } catch {
        // Keep the last forecast.
      }
    };
    void load();
    const timer = setInterval(() => void load(), CARBON_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  return currentCarbon(forecast, now);
}
