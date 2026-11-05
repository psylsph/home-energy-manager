import { describe, it, expect } from 'vitest';
import {
  defaultBooking,
  formatEventWindow,
  formatSlotRange,
  splitEvents,
  stateLabel,
  validateBooking,
} from '../../src/lib/exportOnce';
import type { ExportOnceEvent } from '../../src/lib/types';

const NOW = '2026-10-17 12:34';

function booking(overrides: Partial<Parameters<typeof validateBooking>[0]> = {}) {
  return { slot: 1, date: '2026-10-17', start: '18:00', end: '19:00', target_soc: 20, ...overrides };
}

function event(overrides: Partial<ExportOnceEvent> = {}): ExportOnceEvent {
  return {
    id: 'e1',
    slot: 1,
    start: '2026-10-17 18:00',
    end: '2026-10-17 19:00',
    target_soc: 20,
    source: 'manual',
    state: 'pending',
    ...overrides,
  };
}

describe('validateBooking', () => {
  it('accepts a normal booking', () => {
    expect(validateBooking(booking(), NOW, 2)).toBeNull();
  });

  it('accepts a window that is already under way', () => {
    expect(validateBooking(booking({ start: '12:00', end: '13:00' }), NOW, 2)).toBeNull();
  });

  it('rejects a slot the inverter does not have', () => {
    expect(validateBooking(booking({ slot: 0 }), NOW, 2)).toMatch(/slot/i);
    expect(validateBooking(booking({ slot: 3 }), NOW, 2)).toMatch(/slot/i);
  });

  it('rejects an end that is not after the start', () => {
    expect(validateBooking(booking({ start: '19:00', end: '19:00' }), NOW, 2)).toMatch(/after/i);
    expect(validateBooking(booking({ start: '19:00', end: '18:00' }), NOW, 2)).toMatch(/after/i);
  });

  it('rejects a window that has already ended', () => {
    expect(validateBooking(booking({ start: '10:00', end: '11:00' }), NOW, 2)).toMatch(/ended/i);
    expect(validateBooking(booking({ date: '2026-10-16' }), NOW, 2)).toMatch(/ended/i);
  });

  it('rejects bookings more than 14 days ahead', () => {
    expect(validateBooking(booking({ date: '2026-10-31' }), NOW, 2)).toBeNull();
    expect(validateBooking(booking({ date: '2026-11-01' }), NOW, 2)).toMatch(/14 days/);
  });

  it('rejects a discharge floor outside 4-99', () => {
    expect(validateBooking(booking({ target_soc: 3 }), NOW, 2)).toMatch(/4/);
    expect(validateBooking(booking({ target_soc: 100 }), NOW, 2)).toMatch(/99/);
  });

  it('rejects blank or malformed fields', () => {
    expect(validateBooking(booking({ date: '' }), NOW, 2)).toMatch(/date/i);
    expect(validateBooking(booking({ start: '' }), NOW, 2)).toMatch(/start/i);
    expect(validateBooking(booking({ end: '7pm' }), NOW, 2)).toMatch(/end/i);
  });
});

describe('defaultBooking', () => {
  it('starts at the next whole hour today and lasts an hour', () => {
    expect(defaultBooking('2026-10-17 12:34')).toMatchObject({
      date: '2026-10-17',
      start: '13:00',
      end: '14:00',
    });
  });

  it('rolls to tomorrow evening late at night', () => {
    expect(defaultBooking('2026-10-17 22:10')).toMatchObject({
      date: '2026-10-18',
      start: '18:00',
      end: '19:00',
    });
  });

  it('rolls over a month end', () => {
    expect(defaultBooking('2026-10-31 23:30').date).toBe('2026-11-01');
  });

  it('produces a booking the validator accepts', () => {
    for (const now of ['2026-10-17 00:05', '2026-10-17 12:34', '2026-10-17 21:59', '2026-10-17 23:59']) {
      expect(validateBooking(defaultBooking(now), now, 2)).toBeNull();
    }
  });
});

describe('formatting', () => {
  it('describes a same-day window without locale dependence', () => {
    expect(formatEventWindow(event())).toBe('Sat 17 Oct, 18:00–19:00');
  });

  it('names the end date when the window spans days', () => {
    expect(formatEventWindow(event({ end: '2026-10-18 01:00' }))).toBe(
      'Sat 17 Oct 18:00 – Sun 18 Oct 01:00',
    );
  });

  it('formats slot content, including an empty slot', () => {
    expect(
      formatSlotRange({ enabled: true, start_hour: 22, start_minute: 5, end_hour: 23, end_minute: 0, target_soc: 4 }),
    ).toBe('22:05–23:00');
    expect(
      formatSlotRange({ enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 4 }),
    ).toBe('not set');
  });

  it('labels every state', () => {
    expect(stateLabel('pending')).toBe('Booked');
    expect(stateLabel('active')).toBe('Running');
    expect(stateLabel('done')).toBe('Finished');
    expect(stateLabel('cancelled')).toBe('Cancelled');
    expect(stateLabel('failed')).toBe('Failed');
  });
});

describe('splitEvents', () => {
  it('puts live events first by start time and keeps the five latest finished', () => {
    const events = [
      event({ id: 'late', start: '2026-10-19 18:00', end: '2026-10-19 19:00' }),
      event({ id: 'run', state: 'active', start: '2026-10-17 12:00', end: '2026-10-17 13:00' }),
      ...Array.from({ length: 7 }, (_, i) =>
        event({ id: `d${i}`, state: 'done', start: `2026-10-0${i + 1} 18:00`, end: `2026-10-0${i + 1} 19:00` }),
      ),
    ];
    const { upcoming, recent } = splitEvents(events);
    expect(upcoming.map((e) => e.id)).toEqual(['run', 'late']);
    expect(recent.map((e) => e.id)).toEqual(['d6', 'd5', 'd4', 'd3', 'd2']);
  });
});
