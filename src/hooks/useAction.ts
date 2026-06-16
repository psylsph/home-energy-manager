import { useState, useCallback, useEffect, useRef } from 'react';
import { apiPost } from '../lib/api';

export interface ActionState {
  loading: boolean;
  success: boolean;
  error: string | null;
}

/**
 * Submit a control action to the backend API and surface transient loading /
 * success / error feedback to the UI.
 *
 * The success and error states auto-clear after a short delay so the
 * checkmark / error badge disappears again. That feedback-clearing timeout is
 * tracked in a ref so it can be:
 *
 *   - cancelled when a new request starts (prevents multiple timeouts stacking
 *     up if the user rapidly clicks a button), and
 *   - cancelled on unmount (prevents React from updating state on a component
 *     that is no longer mounted).
 */
export function useAction() {
  const [state, setState] = useState<ActionState>({
    loading: false,
    success: false,
    error: null,
  });

  // Handle of the pending feedback-clearing timeout, if any. Tracked so we can
  // cancel it on a new request or on unmount.
  const resetTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearResetTimer = useCallback(() => {
    if (resetTimerRef.current !== null) {
      clearTimeout(resetTimerRef.current);
      resetTimerRef.current = null;
    }
  }, []);

  // Schedule a feedback reset, replacing any previously scheduled one so we
  // never have two timeouts racing to update the same state.
  const scheduleReset = useCallback(
    (delayMs: number) => {
      clearResetTimer();
      resetTimerRef.current = setTimeout(() => {
        resetTimerRef.current = null;
        // Only clear transient feedback that hasn't already been superseded by
        // a newer request (defensive — execute also clears the timer directly).
        setState((s) =>
          s.success || s.error ? { ...s, success: false, error: null } : s,
        );
      }, delayMs);
    },
    [clearResetTimer],
  );

  // Cancel any pending feedback reset when the component unmounts so React
  // never tries to update an unmounted component. This cleanup only calls
  // clearTimeout (never setState), so it does not trip the
  // `react-hooks/set-state-in-effect` lint rule.
  useEffect(() => clearResetTimer, [clearResetTimer]);

  const execute = useCallback(
    async (path: string, body?: unknown) => {
      // A new request supersedes any pending feedback reset — clear it first
      // so rapid clicks never stack multiple timeouts.
      clearResetTimer();
      setState({ loading: true, success: false, error: null });
      try {
        await apiPost(path, body);
        setState({ loading: false, success: true, error: null });
        scheduleReset(2000);
      } catch (e) {
        setState({ loading: false, success: false, error: (e as Error).message });
        scheduleReset(3000);
      }
    },
    [clearResetTimer, scheduleReset],
  );

  return { ...state, execute };
}
