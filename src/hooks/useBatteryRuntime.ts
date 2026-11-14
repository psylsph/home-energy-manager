import { useEffect, useState } from 'react';
import { apiGet } from '../lib/api';
import { readBatteryRuntime, type BatteryRuntime } from '../lib/batteryRuntime';
import { useInverterStore } from '../store/useInverterStore';

/** The backend reuses an answer for five minutes; a minute here is plenty. */
export const BATTERY_RUNTIME_POLL_MS = 60_000;
/** Give up on a request after this long (building the forecast on a Pi). */
export const BATTERY_RUNTIME_TIMEOUT_MS = 20_000;

/**
 * When the battery will reach its reserve (issue #472), for the house
 * view's Battery Level card. Asks `GET /api/battery/runtime` only while
 * Settings → Panel Controls → Show Battery Runtime is on, every minute and
 * when SOC moves by two points. A failed refresh keeps the last answer;
 * `batteryRuntimeLine` drops it once it's stale.
 */
export function useBatteryRuntime(): BatteryRuntime | null {
  const enabled = useInverterStore((s) => s.showBatteryRuntime);
  // Coarse SOC, so a refresh follows a real move rather than every poll.
  const socStep = useInverterStore((s) => (s.snapshot ? Math.floor(s.snapshot.soc / 2) : null));
  const [runtime, setRuntime] = useState<BatteryRuntime | null>(null);

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    let inFlight: AbortController | null = null;
    const load = async () => {
      inFlight?.abort();
      const controller = new AbortController();
      inFlight = controller;
      const timeout = setTimeout(() => controller.abort(), BATTERY_RUNTIME_TIMEOUT_MS);
      try {
        const res = readBatteryRuntime(await apiGet('/api/battery/runtime', controller.signal));
        if (!cancelled && res) setRuntime(res);
      } catch {
        // Error, timeout or unmount: keep the last answer.
      } finally {
        clearTimeout(timeout);
        if (inFlight === controller) inFlight = null;
      }
    };
    void load();
    const timer = setInterval(() => void load(), BATTERY_RUNTIME_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
      inFlight?.abort();
    };
  }, [enabled, socStep]);

  return enabled ? runtime : null;
}
