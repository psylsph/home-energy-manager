import { useEffect, useState } from 'react';
import { apiGet } from '../lib/api';
import { readUpcomingPlanWrite, type UpcomingPlanWrite } from '../lib/plannedCharge';

/** The answer is tariff arithmetic and two latch reads; a minute is plenty. */
export const PLANNED_CHARGE_POLL_MS = 60_000;
/** Give up on a request after this long (a stalled Pi or connection). */
export const PLANNED_CHARGE_TIMEOUT_MS = 10_000;

export interface PlannedChargeState {
  /** The last usable answer: `null` when nothing is coming or none yet. */
  upcoming: UpcomingPlanWrite | null;
  /** When that answer arrived (ms since the epoch); the line counts from it. */
  fetchedAt: number;
  /** True once the first request has settled, or at once when off. */
  loaded: boolean;
}

const OFF: PlannedChargeState = { upcoming: null, fetchedAt: 0, loaded: true };

/**
 * When the Planner next writes charge slot 1 by itself (issue #481), for the
 * house view's Next Charge card. A failed or unusable refresh keeps the last
 * good answer; `plannedChargeLine` drops it once it goes stale.
 */
export function usePlannedCharge(enabled: boolean): PlannedChargeState {
  const [state, setState] = useState<PlannedChargeState>({ upcoming: null, fetchedAt: 0, loaded: false });

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    let inFlight: AbortController | null = null;
    const load = async () => {
      // One request at a time: a slow answer must not be overtaken by a
      // fresher one and then overwrite it.
      if (inFlight) return;
      const controller = new AbortController();
      inFlight = controller;
      const timeout = setTimeout(() => controller.abort(), PLANNED_CHARGE_TIMEOUT_MS);
      try {
        const upcoming = readUpcomingPlanWrite(await apiGet('/api/forecast/plan/upcoming', controller.signal));
        if (cancelled) return;
        setState((prev) => (upcoming === undefined
          ? { ...prev, loaded: true }
          : { upcoming, fetchedAt: Date.now(), loaded: true }));
      } catch {
        // Error, timeout or unmount: keep the last answer.
        if (!cancelled) setState((prev) => ({ ...prev, loaded: true }));
      } finally {
        clearTimeout(timeout);
        inFlight = null;
      }
    };
    void load();
    const timer = setInterval(() => void load(), PLANNED_CHARGE_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
      inFlight?.abort();
    };
  }, [enabled]);

  return enabled ? state : OFF;
}
