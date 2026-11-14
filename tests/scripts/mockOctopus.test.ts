import { describe, it, expect } from 'vitest';
// @ts-expect-error plain JS script, no type declarations
import { answer, fileEntry, parseSlot, runningSlot } from '../../scripts/mock-octopus.mjs';

/** `npm run mock:octopus`: planned car charges for manual checks (issue #474). */

// Local time, so the slot strings mean what they say on any machine.
const at = (h: number, m = 0, dayOffset = 0) => {
  const d = new Date(2026, 10, 14, h, m, 0, 0);
  d.setDate(d.getDate() + dayOffset);
  return d;
};

describe('parseSlot', () => {
  it('reads a slot later today as a smart dispatch', () => {
    const d = parseSlot('19:00-20:30', at(18));
    expect(d).toEqual({ start: at(19).toISOString(), end: at(20, 30).toISOString(), type: 'SMART', energyAddedKwh: null });
  });

  it('runs past midnight when the end comes first', () => {
    const d = parseSlot('23:30-05:30@12', at(18));
    expect(d.start).toBe(at(23, 30).toISOString());
    expect(d.end).toBe(at(5, 30, 1).toISOString());
    // Octopus signs the energy as an import.
    expect(d.energyAddedKwh).toBe(-12);
  });

  it("moves a slot that's over to tomorrow, and keeps one that's running", () => {
    expect(parseSlot('13:00-13:30:boost', at(18)).start).toBe(at(13, 0, 1).toISOString());
    expect(parseSlot('13:00-13:30:boost', at(18)).type).toBe('BOOST');
    expect(parseSlot('17:30-19:00', at(18)).start).toBe(at(17, 30).toISOString());
  });

  it('refuses what it can\'t read', () => {
    for (const bad of ['', '25:00-01:00', '10:60-11:00', '10-11', '10:00-11:00:fast', 'x']) {
      expect(parseSlot(bad, at(18))).toBeNull();
    }
  });
});

describe('runningSlot', () => {
  it('started ten minutes ago and runs on for the given minutes', () => {
    const d = runningSlot(45, at(18));
    expect(d.start).toBe(at(17, 50).toISOString());
    expect(d.end).toBe(at(18, 45).toISOString());
  });
});

describe('fileEntry', () => {
  it('takes slot strings and dispatch objects, and drops anything else', () => {
    expect(fileEntry('19:00-20:00', at(18)).start).toBe(at(19).toISOString());
    const raw = { start: '2026-11-14T23:30:00Z', end: '2026-11-15T05:30:00Z', type: 'SMART', energyAddedKwh: -5 };
    expect(fileEntry(raw, at(18))).toBe(raw);
    expect(fileEntry(42, at(18))).toBeNull();
    expect(fileEntry({ start: 1 }, at(18))).toBeNull();
  });
});

describe('answer', () => {
  const ask = (query: string) => JSON.stringify({ query });
  it('answers the three queries HEM makes for dispatches', () => {
    expect(answer(ask('mutation { obtainKrakenToken(input: {}) { token } }'), [])).toEqual({ obtainKrakenToken: { token: 'mock-token' } });
    expect(answer(ask('query { devices(accountNumber: $a) { id } }'), []).devices[0]).toMatchObject({ deviceType: 'ELECTRIC_VEHICLES', status: { current: 'LIVE' } });
    expect(answer(ask('query { flexPlannedDispatches(deviceId: $d) { start } }'), ['x'])).toEqual({ flexPlannedDispatches: ['x'] });
  });

  it("says nothing for anything else, or a body that isn't JSON", () => {
    expect(answer(ask('query { savingSessions { events } }'), [])).toBeNull();
    expect(answer('not json', [])).toBeNull();
  });
});
