/**
 * Helpers for the discharge-slot backup round-trip on issue #137.
 *
 * The backend captures the user's discharge schedule into
 * `Settings.discharge_slots_backup` whenever they switch into Eco /
 * Pause / Export Paused mode (the Gen3 firmware re-asserts
 * `enable_discharge` whenever any slot register is non-zero, so the
 * registers must be zeroed — but doing so destroys the schedule).
 * The frontend surfaces the captured backup as pending edits in the
 * Eco-mode slot editor so the user can see their saved schedule and
 * round-trip back to Timed without re-entering it.
 *
 * The merge rules are subtle (pending wins over backup, fully-empty
 * backup is treated as nothing-to-surface, etc.) so the logic lives
 * here rather than inline in ControlPage — keeps it unit-testable
 * without rendering the full page.
 */
import type { ScheduleSlot } from './types';

/**
 * Convert a captured discharge-slot backup from the backend's
 * `/api/control/mode` response into the `pendingDischargeSlots` shape
 * the UI uses, so the Eco-mode slot editor can show the user's saved
 * schedule after an Eco→Timed→Eco round-trip.
 *
 * Returns `null` when the backup should NOT overwrite the existing
 * pending state — i.e. when the user has unsaved local edits (pending
 * wins over a stale backup) or when the backup is empty/missing
 * (nothing to surface).
 *
 * See issue #137.
 */
export function stageBackupAsPending(
  backup: ScheduleSlot[] | undefined,
  currentPending: Record<number, ScheduleSlot>,
): Record<number, ScheduleSlot> | null {
  if (!backup || backup.length === 0) return null;
  if (Object.keys(currentPending).length > 0) return null;
  const next: Record<number, ScheduleSlot> = {};
  backup.forEach((s, idx) => {
    next[idx] = {
      enabled: s.enabled,
      start_hour: s.start_hour,
      start_minute: s.start_minute,
      end_hour: s.end_hour,
      end_minute: s.end_minute,
      target_soc: s.target_soc,
    };
  });
  return next;
}
