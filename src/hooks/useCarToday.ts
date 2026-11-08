import { useEffect, useState } from 'react';
import { apiGet } from '../lib/api';

/** How often the Today bars re-ask: the figure is built from minute rows. */
export const CAR_TODAY_REFRESH_MS = 60_000;

/**
 * The car's estimated energy so far today, in kWh (issue #454), for the Status
 * page's Today bars. `null` until the first answer and whenever no
 * car-charging threshold is set. It is asked for on mount and then each
 * minute, not on every snapshot; a failed refresh keeps the last figure for
 * the rest of that day (never into the next, whose Home Use it would wrongly
 * shrink), and nothing is applied after unmount.
 */
export function useCarToday(): number | null {
  const [latest, setLatest] = useState<{ kwh: number; day: string } | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      apiGet<{ ok: boolean; data: { enabled: boolean; car_kwh: number | null } }>(
        '/api/history/car-today',
      )
        .then((res) => {
          if (cancelled || !res.ok) return;
          const kwh = res.data.enabled ? res.data.car_kwh : null;
          setLatest(kwh == null ? null : { kwh, day: new Date().toDateString() });
        })
        .catch(() => {
          // Keep what we have: the bars are still right without a refresh.
        });
    };
    load();
    const id = setInterval(load, CAR_TODAY_REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, []);

  return latest != null && latest.day === new Date().toDateString() ? latest.kwh : null;
}
