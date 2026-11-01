import { describe, it, expect } from 'vitest';
import { unwrapTimedExportSchedule } from '../../src/lib/timedExportSchedule';

/** The shared `GET /api/timed-export` parser (Control page + house view schedule card). */
describe('unwrapTimedExportSchedule', () => {
  const slot = { enabled: true, start_hour: 16, start_minute: 0, end_hour: 19, end_minute: 0, target_soc: 4 };

  it('reads the schedule and its machine state', () => {
    expect(unwrapTimedExportSchedule({ ok: true, data: { schedule_enabled: true, slots: [slot], machine_state: 'Active' } }))
      .toEqual({ schedule_enabled: true, slots: [slot], machine_state: 'Active' });
  });

  it("falls back to an older backend's Debug-formatted state", () => {
    expect(unwrapTimedExportSchedule({ ok: true, data: { schedule_enabled: false, slots: [], state: 'Configured' } })?.machine_state)
      .toBe('Configured');
  });

  it('returns null for anything that is not a schedule', () => {
    expect(unwrapTimedExportSchedule(null)).toBeNull();
    expect(unwrapTimedExportSchedule(undefined)).toBeNull();
    expect(unwrapTimedExportSchedule('nope' as never)).toBeNull();
    expect(unwrapTimedExportSchedule({ ok: false, data: null })).toBeNull();
    expect(unwrapTimedExportSchedule({ ok: true, data: 'text' })).toBeNull();
    expect(unwrapTimedExportSchedule({ ok: true, data: { slots: [slot] } })).toBeNull();
  });
});
