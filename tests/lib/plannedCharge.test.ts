import { describe, it, expect } from 'vitest';
import { plannedChargeLine, readUpcomingPlanWrite, PLANNED_CHARGE_STALE_MS } from '../../src/lib/plannedCharge';

// Issue #481: the Planner's coming write to charge slot 1, from
// `GET /api/forecast/plan/upcoming`, as a line under the Next Charge card.

const FETCHED = Date.parse('2026-11-17T17:06:00Z');
const MIN = 60_000;

const data = (extra: Record<string, unknown> = {}) => ({
  trigger: 'auto_apply',
  cheap_start: '22:00',
  cheap_end: '23:59',
  applies_at: '21:30',
  applies_in_minutes: 264,
  applies_day: 0,
  running: false,
  ...extra,
});

describe('readUpcomingPlanWrite', () => {
  it('reads the payload', () => {
    expect(readUpcomingPlanWrite({ ok: true, data: data() })).toEqual({
      trigger: 'auto_apply',
      cheapStart: '22:00',
      cheapEnd: '23:59',
      appliesAt: '21:30',
      appliesInMinutes: 264,
      appliesDay: 0,
      running: false,
    });
  });

  it('reads nothing coming as null, and an unusable answer as undefined', () => {
    expect(readUpcomingPlanWrite({ ok: true, data: null })).toBeNull();
    expect(readUpcomingPlanWrite(null)).toBeUndefined();
    expect(readUpcomingPlanWrite('nope')).toBeUndefined();
    expect(readUpcomingPlanWrite({ ok: false, error: 'x' })).toBeUndefined();
    expect(readUpcomingPlanWrite({ ok: true })).toBeUndefined();
    expect(readUpcomingPlanWrite({ ok: true, data: data({ applies_at: '9:5' }) })).toBeUndefined();
    expect(readUpcomingPlanWrite({ ok: true, data: data({ applies_in_minutes: -1 }) })).toBeUndefined();
    expect(readUpcomingPlanWrite({ ok: true, data: data({ applies_in_minutes: 'soon' }) })).toBeUndefined();
    expect(readUpcomingPlanWrite({ ok: true, data: data({ trigger: 'manual' }) })).toBeUndefined();
    expect(readUpcomingPlanWrite({ ok: true, data: data({ applies_day: -1 }) })).toBeUndefined();
    expect(readUpcomingPlanWrite({ ok: true, data: data({ applies_day: 'tomorrow' }) })).toBeUndefined();
    expect(readUpcomingPlanWrite({ ok: true, data: data({ applies_day: undefined }) })).toBeUndefined();
  });
});

describe('plannedChargeLine', () => {
  const upcoming = readUpcomingPlanWrite({ ok: true, data: data() })!;

  it('says when the Planner updates the slot, naming the cheap period in the title', () => {
    const line = plannedChargeLine(upcoming, FETCHED, FETCHED);
    expect(line?.text).toBe('Planner updates at 21:30');
    expect(line?.title).toContain('22:00–23:59');
    expect(line?.title).toContain('21:30');
  });

  it('keeps showing it as the minutes pass, until the write time', () => {
    expect(plannedChargeLine({ ...upcoming, appliesInMinutes: 4 }, FETCHED, FETCHED + 3 * MIN)?.text).toBe('Planner updates at 21:30');
  });

  it('goes at the write time, because the write has run by then', () => {
    const soon = { ...upcoming, appliesInMinutes: 3 };
    expect(plannedChargeLine(soon, FETCHED, FETCHED + 2 * MIN)?.text).toBe('Planner updates at 21:30');
    expect(plannedChargeLine(soon, FETCHED, FETCHED + 3 * MIN)).toBeNull();
    expect(plannedChargeLine(soon, FETCHED, FETCHED + 4 * MIN)).toBeNull();
  });

  it('is nothing when nothing is coming, or the write is due this minute', () => {
    expect(plannedChargeLine(null, FETCHED, FETCHED)).toBeNull();
    const now = readUpcomingPlanWrite({ ok: true, data: data({ applies_in_minutes: 0 }) })!;
    expect(plannedChargeLine(now, FETCHED, FETCHED)).toBeNull();
  });

  it('is nothing once the answer is stale, so a failing backend leaves no old promise', () => {
    expect(plannedChargeLine(upcoming, FETCHED, FETCHED + PLANNED_CHARGE_STALE_MS + 1)).toBeNull();
  });

  it('says tomorrow when the backend puts the write on the next day', () => {
    // Fetched at 17:06 with the write at 01:30: 504 minutes ahead.
    const next = readUpcomingPlanWrite({ ok: true, data: data({ applies_at: '01:30', applies_in_minutes: 504, applies_day: 1, cheap_start: '02:00', cheap_end: '05:00' }) })!;
    expect(plannedChargeLine(next, FETCHED, FETCHED)?.text).toBe('Planner updates tomorrow at 01:30');
    // Still tomorrow a few minutes later; the day is fixed when it was fetched.
    expect(plannedChargeLine(next, FETCHED, FETCHED + 3 * MIN)?.text).toBe('Planner updates tomorrow at 01:30');
  });

  it('says nothing for a write two days out, where tomorrow would be wrong', () => {
    const later = readUpcomingPlanWrite({ ok: true, data: data({ applies_at: '01:30', applies_in_minutes: 1944, applies_day: 2 }) })!;
    expect(plannedChargeLine(later, FETCHED, FETCHED)).toBeNull();
  });

  it('does not say tomorrow for a write later the same evening', () => {
    expect(plannedChargeLine(upcoming, FETCHED, FETCHED)?.text).not.toContain('tomorrow');
  });

  it('names the nightly refresh when that is the trigger', () => {
    const refresh = readUpcomingPlanWrite({ ok: true, data: data({ trigger: 'refresh' }) })!;
    expect(plannedChargeLine(refresh, FETCHED, FETCHED)?.title).toContain('nightly');
  });

  it('is nothing when the clock steps back before the fetch', () => {
    // A clock jump must not turn into a promise that lasts for ever.
    expect(plannedChargeLine(upcoming, FETCHED, FETCHED - 5 * MIN)).toBeNull();
  });

  it('says the cheap period is already under way for a window being planned as it runs', () => {
    const running = readUpcomingPlanWrite({ ok: true, data: data({ running: true, applies_in_minutes: 3 }) })!;
    expect(plannedChargeLine(running, FETCHED, FETCHED)?.title).toContain('under way');
  });
});
