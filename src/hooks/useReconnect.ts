import { useState, useCallback, useEffect, useRef } from 'react';
import { apiPost } from '../lib/api';
import { useInverterStore } from '../store/useInverterStore';

/** How long the clicked button stays disabled / shows "Reconnecting…". */
const RECONNECTING_LABEL_MS = 3000;

export interface UseReconnectResult {
  /** True while a reconnect request is in flight (disables the clicked button). */
  reconnecting: boolean;
  /** POST `/api/reconnect` and stamp the shared "requested at" timestamp. */
  reconnect: () => Promise<void>;
}

/**
 * Manual reconnect trigger, shared by every place that renders a "Reconnect"
 * button — the header connection indicator, the Status page, and the shared
 * `AwaitingConnection` placeholder.
 *
 * The backend's `POST /api/reconnect` forces a fresh connection cycle, but
 * against a dead or zombie dongle the retry fails almost instantly and the
 * connection-state broadcast goes `Reconnecting`→`Reconnecting` (or
 * `Disconnected`→`Disconnected`). With no state change to react to, the UI
 * shows nothing and the click feels inert. To close that feedback gap, every
 * call stamps a shared `reconnectRequestedAt` timestamp in the store; the
 * always-visible header indicator reads it and renders "Reconnect requested
 * at HH:MM:SS", so the user can see their click registered even when the
 * dongle is unreachable.
 *
 * The local `reconnecting` flag disables whichever button was just clicked
 * for a few seconds (mirrors the original inline behaviour). It is
 * intentionally per-instance — only the clicked button needs to disable
 * itself; the timestamp above is what carries the shared feedback.
 */
export function useReconnect(): UseReconnectResult {
  const [reconnecting, setReconnecting] = useState(false);

  // Handle of the pending label-reset timeout. Tracked so a rapid second
  // click replaces it (no stacked timeouts) and so unmount cancels it (no
  // state update on an unmounted component). Mirrors `useAction`.
  const resetTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearResetTimer = useCallback(() => {
    if (resetTimerRef.current !== null) {
      clearTimeout(resetTimerRef.current);
      resetTimerRef.current = null;
    }
  }, []);

  // Cancel any pending reset on unmount. This only calls clearTimeout (never
  // setState), so it does not trip the `react-hooks/set-state-in-effect` rule.
  useEffect(() => clearResetTimer, [clearResetTimer]);

  const reconnect = useCallback(async () => {
    clearResetTimer();
    setReconnecting(true);
    try {
      await apiPost('/api/reconnect');
    } catch {
      // Swallow — the poll loop's own back-off retries regardless of whether
      // this poke landed. The timestamp below still gives the user visible
      // feedback that the click registered, which is the whole point.
    }
    // Record the attempt in the shared store so the header's
    // "Reconnect requested at HH:MM:SS" notice fires from any button.
    useInverterStore.getState().markReconnectRequested(Date.now());
    // Hold the disabled / "Reconnecting…" label briefly, then release so the
    // button can be clicked again (and so a different button can act).
    resetTimerRef.current = setTimeout(() => {
      resetTimerRef.current = null;
      setReconnecting(false);
    }, RECONNECTING_LABEL_MS);
  }, [clearResetTimer]);

  return { reconnecting, reconnect };
}
