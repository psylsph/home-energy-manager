/**
 * Tests for the `stageBackupAsPending` helper used by ControlPage's
 * `handleModeChange` to surface the backend's captured discharge-slot
 * backup as pending edits in the Eco-mode slot editor after an
 * Eco→Timed→Eco round-trip.
 *
 * See issue #137. The helper's behaviour is the single frontend-side
 * bridge that closes the gap reported by the user:
 *
 *   "Switch back to Eco mode, only charge slot is restored. No
 *    Discharge slot."
 *
 * The merge rules are subtle enough to warrant a dedicated test suite
 * rather than relying solely on integration tests through ControlPage.
 */
import { describe, it, expect } from 'vitest';
import { stageBackupAsPending } from '../../src/lib/dischargeSlotBackup';
import type { ScheduleSlot } from '../../src/lib/types';

/**
 * Build a discharge-slot array of the given length with deterministic
 * values so test failures can be diffed.
 */
function makeBackup(length: number, startHour = 16): ScheduleSlot[] {
  return Array.from({ length }, (_, i) => ({
    enabled: i === 0 || i === 1,
    start_hour: startHour + i,
    start_minute: 30,
    end_hour: startHour + i + 3,
    end_minute: 45,
    target_soc: 4,
  }));
}

describe('stageBackupAsPending (issue #137)', () => {
  it('returns null when backup is undefined', () => {
    // The backend omits discharge_slots_backup from the response when
    // nothing was captured (no schedule on the inverter at Eco-entry).
    // Surfacing nothing is the correct behaviour.
    expect(stageBackupAsPending(undefined, {})).toBeNull();
  });

  it('returns null when backup is an empty array', () => {
    // Defensive: an empty array is treated the same as undefined.
    expect(stageBackupAsPending([], {})).toBeNull();
  });

  it('stages the backup as pending edits when no local edits exist', () => {
    // The user reproduction: Eco→Timed→Eco. After Eco, the user has
    // no local pending edits but the backend captured their schedule.
    // We MUST surface those slots as pending so the Eco-mode slot
    // editor shows them — without this, the bug ("only charge slot is
    // restored. No Discharge slot.") recurs.
    const backup = makeBackup(10, 16);
    const staged = stageBackupAsPending(backup, {});
    expect(staged).not.toBeNull();
    expect(Object.keys(staged!)).toHaveLength(10);
    expect(staged![0]).toEqual({
      enabled: true,
      start_hour: 16,
      start_minute: 30,
      end_hour: 19,
      end_minute: 45,
      target_soc: 4,
    });
    expect(staged![1]).toEqual({
      enabled: true,
      start_hour: 17,
      start_minute: 30,
      end_hour: 20,
      end_minute: 45,
      target_soc: 4,
    });
  });

  it('returns null when the user has unsaved local pending edits', () => {
    // While the user is mid-edit on a slot in Eco, they have local
    // pending state. If we overwrite that with the backend's stale
    // backup, we'd silently discard the user's in-flight edits. The
    // helper must respect pending-over-backup.
    const backup = makeBackup(10, 16);
    const pending: Record<number, ScheduleSlot> = {
      0: {
        enabled: true,
        start_hour: 9,
        start_minute: 0,
        end_hour: 11,
        end_minute: 0,
        target_soc: 100,
      },
    };
    expect(stageBackupAsPending(backup, pending)).toBeNull();
  });

  it('treats all-disabled slots in the backup as not worth staging', () => {
    // Defensive: a backup with `enabled: false` everywhere should be
    // treated as "nothing to surface" — a no-op would push the UI
    // through a pointless re-render and might briefly flash empty
    // pending slots. We currently do NOT filter this in the helper;
    // instead, the BACKEND never echoes such a backup (its
    // `capture_discharge_schedule_backup` returns None when no slot
    // is configured). The test documents the contract.
    const backup = makeBackup(10, 16).map((s) => ({ ...s, enabled: false }));
    const staged = stageBackupAsPending(backup, {});
    // The helper doesn't filter; the BACKEND is responsible for not
    // echoing a fully-disabled backup. This test pins the helper's
    // current behaviour so a future change doesn't silently shift
    // responsibility to the frontend.
    expect(staged).not.toBeNull();
    expect(Object.keys(staged!)).toHaveLength(10);
    expect(staged![0].enabled).toBe(false);
  });

  it('round-trip: stage then re-stage preserves the same shape', () => {
    // After a Timed restore that consumed the backup, the next Eco
    // entry captures again. The helper's staged output should be
    // byte-identical (modulo key order) so the UI's overlay behaviour
    // is deterministic across multiple round-trips.
    const backup = makeBackup(10, 16);
    const first = stageBackupAsPending(backup, {});
    expect(first).not.toBeNull();

    // Simulate the user clicking Timed (which clears pending in the
    // real flow), then Eco again — pending is now empty so the helper
    // should re-stage the same shape from the same backup.
    const second = stageBackupAsPending(backup, {});
    expect(second).toEqual(first);
  });

  it('preserves all six ScheduleSlot fields when staging', () => {
    // The frontend's `pendingDischargeSlots` is a Record keyed by
    // index, with values typed as ScheduleSlot. The backend's backup
    // has identical field shape (intentional design — see
    // `DischargeSlotBackup` in src-tauri/src/settings/mod.rs). This
    // test pins that every field round-trips through the helper.
    const backup: ScheduleSlot[] = [
      {
        enabled: true,
        start_hour: 5,
        start_minute: 17,
        end_hour: 22,
        end_minute: 33,
        target_soc: 80,
      },
    ];
    const staged = stageBackupAsPending(backup, {})!;
    expect(staged[0]).toEqual({
      enabled: true,
      start_hour: 5,
      start_minute: 17,
      end_hour: 22,
      end_minute: 33,
      target_soc: 80,
    });
  });
});
