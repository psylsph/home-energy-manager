import type { ScheduleSlot } from './types';

/**
 * Parse a `GET /api/timed-export` reply (the HEM-managed Timed Export
 * schedule, issue #289). Shared by the Control page and the house view's
 * schedule card so the endpoint has one parser. `null` when the reply isn't
 * a schedule (an error, or a backend without the endpoint).
 */
export function unwrapTimedExportSchedule(res: { ok: boolean; data: unknown } | null | undefined): {
  schedule_enabled: boolean;
  slots: ScheduleSlot[];
  /** Legacy Debug-formatted state string, kept for backwards compatibility. */
  state?: string;
  /** Backend machine state name: Off / Configured / Active / Entering / Exiting / BlockedByPause / Error. */
  machine_state?: unknown;
  /** Whether the inverter was classified as HR59 re-arming firmware. */
  device_rearm_confirmed?: boolean;
} | null {
  if (!res || typeof res !== 'object') return null;
  const data = (res as { data?: unknown }).data;
  if (!data || typeof data !== 'object') return null;
  if (!('schedule_enabled' in data)) return null;
  const schedule = data as {
    schedule_enabled: boolean;
    slots: ScheduleSlot[];
    state?: string;
    machine_state?: unknown;
    device_rearm_confirmed?: boolean;
  };
  // Older backend builds exposed only the Debug-formatted `state` field.
  // Normalize it here so transitional/error presentation keeps working
  // during a rolling upgrade as well as with the current machine_state field.
  return {
    ...schedule,
    machine_state: schedule.machine_state ?? schedule.state,
  };
}
