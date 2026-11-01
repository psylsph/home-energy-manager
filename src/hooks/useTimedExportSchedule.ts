import { useEffect, useState } from 'react';
import { apiGet } from '../lib/api';
import type { TimedExportSchedule } from '../lib/chargeSchedule';
import { unwrapTimedExportSchedule } from '../lib/timedExportSchedule';

/** The schedule only changes when someone edits it; a minute is soon enough. */
export const TIMED_EXPORT_POLL_MS = 60_000;
/** Give up on a request after this long (a stalled Pi or connection). */
export const TIMED_EXPORT_TIMEOUT_MS = 10_000;

export interface TimedExportScheduleState {
  /** `null` when there's no answer yet or the backend couldn't be asked. */
  schedule: TimedExportSchedule | null;
  /** True once the first request has settled, either way (answer, error or timeout). */
  loaded: boolean;
}

/**
 * The HEM-managed Timed Export schedule for the house view's schedule card
 * (issue #422). Polls `GET /api/timed-export` while mounted; each request is
 * aborted after [`TIMED_EXPORT_TIMEOUT_MS`] or on unmount. The timeout is far
 * shorter than the poll interval, so requests never overlap and a slow one
 * can't overwrite a newer answer. A failed request keeps the last
 * good schedule; `loaded` lets the card wait for the first answer instead of
 * flicking from the snapshot's view to the schedule's, without waiting for
 * ever on a stalled connection.
 */
export function useTimedExportSchedule(): TimedExportScheduleState {
  const [state, setState] = useState<TimedExportScheduleState>({ schedule: null, loaded: false });

  useEffect(() => {
    let cancelled = false;
    let inFlight: AbortController | null = null;
    const load = async () => {
      const controller = new AbortController();
      inFlight = controller;
      const timeout = setTimeout(() => controller.abort(), TIMED_EXPORT_TIMEOUT_MS);
      try {
        const res = await apiGet<{ ok: boolean; data: unknown }>('/api/timed-export', controller.signal);
        if (cancelled) return;
        const parsed = unwrapTimedExportSchedule(res);
        if (parsed) {
          setState({ schedule: { schedule_enabled: parsed.schedule_enabled === true, slots: parsed.slots ?? [] }, loaded: true });
        } else {
          setState((prev) => ({ ...prev, loaded: true }));
        }
      } catch {
        // Error, timeout or unmount: keep the last schedule.
        if (!cancelled) setState((prev) => ({ ...prev, loaded: true }));
      } finally {
        clearTimeout(timeout);
        inFlight = null;
      }
    };
    void load();
    const timer = setInterval(() => void load(), TIMED_EXPORT_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
      inFlight?.abort();
    };
  }, []);

  return state;
}
