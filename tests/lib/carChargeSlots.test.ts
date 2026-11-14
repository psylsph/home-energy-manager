import { describe, it, expect } from 'vitest';
import { carChargeLine, type CarChargeFormat } from '../../src/lib/carChargeSlots';
import type { SmartChargeDispatch, SmartChargingResponse } from '../../src/lib/types';

// Issue #474. Times are formatted in UTC here so the tests don't depend on
// the machine's time zone; the page passes local-time formatters.
const UTC: CarChargeFormat = {
  time: (d) => d.toISOString().slice(11, 16),
  weekday: (d) => ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'][d.getUTCDay()]!,
};

// Saturday 14 November 2026, 20:00 UTC.
const NOW = new Date('2026-11-14T20:00:00Z');

function d(start: string, end: string, extra: Partial<SmartChargeDispatch> = {}): SmartChargeDispatch {
  return {
    start: `2026-11-${start}:00Z`,
    end: `2026-11-${end}:00Z`,
    type: 'SMART',
    energy_added_kwh: null,
    smart: true,
    ...extra,
  };
}

function response(dispatches: SmartChargeDispatch[], extra: Partial<SmartChargingResponse> = {}): SmartChargingResponse {
  return {
    ok: true,
    enabled: false,
    configured: true,
    checked_at: '2026-11-14T19:58:00Z',
    error: null,
    devices: 1,
    holding: false,
    hold_failing: false,
    stale_after_minutes: 15,
    dispatches,
    ...extra,
  };
}

const line = (r: SmartChargingResponse | null, now = NOW) => carChargeLine(r, now, UTC);

describe('carChargeLine (issue #474)', () => {
  it('shows nothing without a usable answer', () => {
    expect(line(null)).toBeNull();
    expect(line(response([d('14T23:30', '15T05:30')], { error: 'Octopus said no' }))).toBeNull();
    expect(line(response([d('14T23:30', '15T05:30')], { devices: 0 }))).toBeNull();
    expect(line(response([d('14T23:30', '15T05:30')], { checked_at: null }))).toBeNull();
    expect(line(response([d('14T23:30', '15T05:30')], { checked_at: 'not a time' }))).toBeNull();
  });

  it('trusts a list up to its stale limit, and not a second past it', () => {
    const r = response([d('14T23:30', '15T05:30')], { checked_at: '2026-11-14T19:45:00Z' });
    expect(line(r)).not.toBeNull();
    expect(line(r, new Date('2026-11-14T20:00:01Z'))).toBeNull();
  });

  it('shows nothing for a fresh list with no charge planned', () => {
    expect(line(response([]))).toBeNull();
    // Over already: Octopus can list one for a moment after it ends.
    expect(line(response([d('14T18:00', '14T20:00')]))).toBeNull();
  });

  it('shows the next charge, across midnight with no day', () => {
    const l = line(response([d('14T23:30', '15T05:30', { energy_added_kwh: 12.34 })]))!;
    expect(l.state).toBe('next');
    expect(l.text).toBe('Car 23:30–05:30');
    expect(l.boost).toBe(false);
    expect(l.title).toBe('12.3 kWh planned');
  });

  it('names the day for a charge more than 24 hours away', () => {
    const l = line(response([d('16T01:00', '16T03:00')]))!;
    expect(l.text).toBe('Car Mon 01:00–03:00');
    // Exactly 24 hours away is still "within a day".
    expect(line(response([d('15T20:00', '15T21:00')]))!.text).toBe('Car 20:00–21:00');
  });

  it('says until when a running charge goes', () => {
    const l = line(response([d('14T19:30', '14T22:00')]))!;
    expect(l.state).toBe('active');
    expect(l.text).toBe('Car charging until 22:00');
    // Starting exactly now counts as running.
    expect(line(response([d('14T20:00', '14T21:00')]))!.state).toBe('active');
  });

  it('merges back-to-back and overlapping charges, across cars', () => {
    const l = line(response([
      d('14T23:30', '15T00:00', { energy_added_kwh: 2 }),
      d('15T00:00', '15T00:30', { energy_added_kwh: 3 }),
      d('15T00:15', '15T01:30'),
      d('15T03:00', '15T04:00'),
    ]))!;
    expect(l.text).toBe('Car 23:30–01:30');
    expect(l.title).toBe('5.0 kWh planned · 1 more planned');
  });

  it("doesn't merge across a one-minute gap", () => {
    const l = line(response([d('14T23:30', '15T00:00'), d('15T00:01', '15T00:30')]))!;
    expect(l.text).toBe('Car 23:30–00:00');
    expect(l.title).toBe('1 more planned');
  });

  it('merges a running charge with the one that follows it', () => {
    expect(line(response([d('14T19:30', '14T20:30'), d('14T20:30', '14T21:00')]))!.text)
      .toBe('Car charging until 21:00');
  });

  it('marks a boost, but only when every part of the block is one', () => {
    const boost = { type: 'BOOST', smart: false };
    expect(line(response([d('14T23:30', '15T00:30', boost)]))!.text).toBe('Car 23:30–00:30 · Boost');
    expect(line(response([d('14T23:30', '15T00:30', boost)]))!.boost).toBe(true);
    const mixed = line(response([d('14T23:30', '15T00:00', boost), d('15T00:00', '15T00:30')]))!;
    expect(mixed.text).toBe('Car 23:30–00:30');
    expect(mixed.boost).toBe(false);
  });

  it('skips dispatches with times it cannot read', () => {
    const l = line(response([
      { ...d('14T21:00', '14T22:00'), start: 'garbage' },
      { ...d('14T21:00', '14T22:00'), end: '' },
      d('14T23:30', '15T05:30'),
    ]))!;
    expect(l.text).toBe('Car 23:30–05:30');
    expect(l.title).toBe('');
    // A dispatch ending before it starts is unreadable too.
    expect(line(response([d('15T05:30', '14T23:30')]))).toBeNull();
  });

  it('formats in local time by default', () => {
    const l = carChargeLine(response([d('14T23:30', '15T05:30')]), NOW)!;
    const fmt = (iso: string) =>
      new Date(iso).toLocaleTimeString('en-GB', { hour: '2-digit', minute: '2-digit', hour12: false });
    expect(l.text).toBe(`Car ${fmt('2026-11-14T23:30:00Z')}–${fmt('2026-11-15T05:30:00Z')}`);
  });
});
