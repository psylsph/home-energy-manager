import { useEffect, useState } from 'react';
import { apiGet } from '../lib/api';
import { readDispatchesResponse } from '../lib/smartCharging';
import type { SmartChargingResponse } from '../lib/types';
import { useInverterStore } from '../store/useInverterStore';

/** The backend checks Octopus every 3 minutes; a minute here is plenty. */
export const CAR_CHARGE_POLL_MS = 60_000;
/**
 * Ask again this soon when the backend hasn't checked Octopus yet: switching
 * the line on wakes the backend's check, which takes a few seconds.
 */
export const CAR_CHARGE_RECHECK_MS = 5_000;
/** Give up on a request after this long (a stalled Pi or connection). */
export const CAR_CHARGE_TIMEOUT_MS = 10_000;

export interface CarChargeSlotsState {
  /** The last usable answer; `null` when switched off or none yet. */
  response: SmartChargingResponse | null;
  /** True once the first request has settled, or at once when off. */
  loaded: boolean;
}

const OFF: CarChargeSlotsState = { response: null, loaded: true };
const UNSETTLED: CarChargeSlotsState = { response: null, loaded: false };

/**
 * The car's planned Intelligent Octopus Go charges for the house view's
 * Next Charge card (issue #474). Asks `GET /api/octopus/dispatches` only
 * while Settings → Panel Controls → Show Car Charging Slots is on (and
 * Octopus is set up, which the store's flag already includes). A failed or
 * unusable refresh keeps the last good answer, as the Octopus page does;
 * `carChargeLine` hides it once it goes stale.
 */
export function useCarChargeSlots(): CarChargeSlotsState {
  const enabled = useInverterStore((s) => s.showCarCharging);
  const settled = useInverterStore((s) => s.navSettingsLoaded);
  const [state, setState] = useState<CarChargeSlotsState>({ response: null, loaded: false });

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    let inFlight: AbortController | null = null;
    let recheck: ReturnType<typeof setTimeout> | null = null;
    const load = async () => {
      const controller = new AbortController();
      inFlight = controller;
      const timeout = setTimeout(() => controller.abort(), CAR_CHARGE_TIMEOUT_MS);
      try {
        const res = readDispatchesResponse(await apiGet('/api/octopus/dispatches', controller.signal));
        if (cancelled) return;
        setState((prev) => ({ response: res ?? prev.response, loaded: true }));
        // Not checked yet (just switched on): ask again soon, not in a minute.
        if (res && res.configured && !res.checked_at && !res.error && recheck === null) {
          recheck = setTimeout(() => {
            recheck = null;
            void load();
          }, CAR_CHARGE_RECHECK_MS);
        }
      } catch {
        // Error, timeout or unmount: keep the last answer.
        if (!cancelled) setState((prev) => ({ ...prev, loaded: true }));
      } finally {
        clearTimeout(timeout);
        inFlight = null;
      }
    };
    void load();
    const timer = setInterval(() => void load(), CAR_CHARGE_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
      if (recheck !== null) clearTimeout(recheck);
      inFlight?.abort();
    };
  }, [enabled]);

  if (!settled) return UNSETTLED;
  return enabled ? state : OFF;
}
