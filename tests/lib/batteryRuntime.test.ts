import { describe, it, expect } from 'vitest';
import { batteryRuntimeLine, readBatteryRuntime, type RuntimeFormat } from '../../src/lib/batteryRuntime';

// Issue #472. UTC formatters, so the tests don't depend on the time zone.
const UTC: RuntimeFormat = {
  time: (d) => d.toISOString().slice(11, 16),
  day: (d) => d.toISOString().slice(0, 10),
  weekday: (d) => ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'][d.getUTCDay()]!,
};
// Saturday 14 November 2026, 18:00 UTC.
const NOW = new Date('2026-11-14T18:00:00Z');
const ts = (iso: string) => Date.parse(iso) / 1000;
const line = (data: unknown) => batteryRuntimeLine(readBatteryRuntime({ ok: true, data }), NOW, UTC);

describe('readBatteryRuntime', () => {
  it('accepts only a well-formed answer', () => {
    expect(readBatteryRuntime(null)).toBeNull();
    expect(readBatteryRuntime({ ok: false })).toBeNull();
    expect(readBatteryRuntime({ ok: true, data: { outcome: 'empty_at' } })).toBeNull();
    expect(readBatteryRuntime({ ok: true, data: { outcome: 'nonsense' } })).toBeNull();
    expect(readBatteryRuntime({ ok: true, data: { outcome: 'charging' } })).toEqual({ outcome: 'charging' });
    expect(readBatteryRuntime({ ok: true, data: { outcome: 'paused' } })).toEqual({ outcome: 'paused' });
    expect(readBatteryRuntime({ ok: true, data: { outcome: 'full_at', at: 5 } })).toBeNull();
    expect(readBatteryRuntime({ ok: true, data: { outcome: 'full_at', at: 5, soc: 100 } })).not.toBeNull();
  });
});

describe('batteryRuntimeLine (issue #472)', () => {
  it('says when it empties today, with a confidence', () => {
    const l = line({
      outcome: 'empty_at', at: ts('2026-11-14T22:30:00Z'),
      earliest: ts('2026-11-14T21:30:00Z'), latest: ts('2026-11-14T23:00:00Z'), confidence: 'medium',
    })!;
    expect(l.text).toBe('Empty at 22:30');
    expect(l.confidence).toBe('medium');
    expect(l.dots).toBe(2);
    expect(l.title).toBe('Likely between 21:30 and 23:00 · medium confidence');
  });

  it('explains the time and the dots in words, for the ⓘ', () => {
    const l = line({
      outcome: 'empty_at', at: ts('2026-11-14T22:30:00Z'),
      earliest: ts('2026-11-14T21:30:00Z'), latest: ts('2026-11-14T23:00:00Z'), confidence: 'medium',
    })!;
    expect(l.help).toBe(
      'Likely between 21:30 and 23:00. This is when the battery reaches its reserve, from your usage history '
      + 'and the solar forecast, to the nearest half hour. Medium confidence (two dots): a busy and a quiet '
      + 'day are within three hours of each other.',
    );
    const low = line({ outcome: 'empty_at', at: ts('2026-11-14T22:30:00Z'), earliest: null, latest: null, confidence: 'low' })!;
    expect(low.help).toMatch(/^This is when .* Low confidence \(one dot\): a busy and a quiet day are more than three hours apart, or there's under two weeks of history\.$/);
    const high = line({ outcome: 'empty_at', at: ts('2026-11-14T22:30:00Z'), earliest: null, latest: null, confidence: 'high' })!;
    expect(high.help).toMatch(/High confidence \(three dots\): a busy and a quiet day are within an hour of each other\.$/);
    // Lines without dots need no explaining.
    expect(line({ outcome: 'at_reserve' })!.help).toBeNull();
  });

  it('names tomorrow, and a later day by its weekday', () => {
    expect(line({ outcome: 'empty_at', at: ts('2026-11-15T06:00:00Z'), earliest: null, latest: null, confidence: 'low' })!.text)
      .toBe('Empty tomorrow at 06:00');
    expect(line({ outcome: 'empty_at', at: ts('2026-11-16T06:00:00Z'), earliest: null, latest: null, confidence: 'low' })!.text)
      .toBe('Empty Mon at 06:00');
  });

  it('says only what it knows of the range', () => {
    const l = line({ outcome: 'empty_at', at: ts('2026-11-14T22:30:00Z'), earliest: ts('2026-11-14T21:30:00Z'), latest: null, confidence: 'low' })!;
    expect(l.title).toBe('Not before about 21:30 · low confidence');
    expect(l.dots).toBe(1);
    const h = line({ outcome: 'empty_at', at: ts('2026-11-14T22:30:00Z'), earliest: null, latest: null, confidence: 'high' })!;
    expect(h.title).toBe('High confidence');
    expect(h.dots).toBe(3);
  });

  it('says what refills it first', () => {
    expect(line({ outcome: 'recharges_first', at: ts('2026-11-14T23:30:00Z'), by: 'charge' })!.text)
      .toBe('Lasts until the 23:30 charge');
    expect(line({ outcome: 'recharges_first', at: ts('2026-11-15T09:00:00Z'), by: 'solar' })!.text)
      .toBe('Lasts until solar tomorrow at 09:00');
    expect(line({ outcome: 'recharges_first', at: ts('2026-11-15T09:00:00Z'), by: 'solar' })!.dots).toBeNull();
  });

  it("names the charge's day when it isn't today", () => {
    expect(line({ outcome: 'recharges_first', at: ts('2026-11-15T00:30:00Z'), by: 'charge' })!.text)
      .toBe("Lasts until tomorrow's 00:30 charge");
    expect(line({ outcome: 'recharges_first', at: ts('2026-11-16T06:00:00Z'), by: 'charge' })!.text)
      .toBe("Lasts until Mon's 06:00 charge");
  });

  it('says when a charging battery is full, or reaches its slot target', () => {
    const full = line({ outcome: 'full_at', at: ts('2026-11-14T20:30:00Z'), soc: 100 })!;
    expect(full.text).toBe('Full at 20:30');
    expect(full.dots).toBeNull();
    expect(line({ outcome: 'full_at', at: ts('2026-11-15T04:00:00Z'), soc: 80 })!.text).toBe('80% tomorrow at 04:00');
  });

  // Issue #476: with the toggle on, a blank card looked broken, so a pause
  // says so rather than showing nothing.
  it('says discharge is paused, with why there is no time behind the ⓘ', () => {
    const l = line({ outcome: 'paused' })!;
    expect(l.text).toBe('Discharge paused');
    expect(l.dots).toBeNull();
    expect(l.confidence).toBeNull();
    expect(l.help).toMatch(/won't discharge until the pause ends/);
  });

  it('covers the rest', () => {
    expect(line({ outcome: 'lasts_beyond', hours: 24 })!.text).toBe('Lasts over 24 hours');
    expect(line({ outcome: 'at_reserve' })!.text).toBe('At its reserve');
    expect(batteryRuntimeLine(null, NOW, UTC)).toBeNull();
  });

  // Issue #476 follow-up: with the toggle on, a blank card looked broken. An
  // answer the backend can't give says so, with the reason behind the ⓘ.
  it('says it cannot calculate while charging faster than the forecast', () => {
    const l = line({ outcome: 'charging' })!;
    expect(l.text).toBe('Unable to calculate: charging faster than forecast');
    expect(l.dots).toBeNull();
    expect(l.confidence).toBeNull();
    expect(l.help).toMatch(/forecast/);
    expect(l.help).toMatch(/Force Charge/);
  });

  it.each([
    ['no_snapshot', /no live reading/],
    ['not_connected', /no live reading/],
    ['no_battery_capacity', /battery capacity or power limits/],
    ['insufficient_consumption_history', /usage history/],
    ['no_projection', /couldn't be run/],
  ])('says it cannot calculate when unknown (%s), with the reason in the help', (reason, help) => {
    const l = line({ outcome: 'unknown', reason })!;
    expect(l.text).toBe('Unable to calculate');
    expect(l.dots).toBeNull();
    expect(l.help).toMatch(help);
  });

  it('gives a general reason for an unknown one it does not recognise, or none', () => {
    expect(line({ outcome: 'unknown', reason: 'something_new' })!.help).toMatch(/couldn't be run/);
    expect(line({ outcome: 'unknown' })!.text).toBe('Unable to calculate');
  });

  it('still drops a stale cannot-calculate answer', () => {
    expect(line({ outcome: 'charging', computed_at: ts('2026-11-14T17:44:00Z') })).toBeNull();
  });

  it('drops an answer more than 15 minutes old', () => {
    expect(line({ outcome: 'at_reserve', computed_at: ts('2026-11-14T17:46:00Z') })).not.toBeNull();
    expect(line({ outcome: 'at_reserve', computed_at: ts('2026-11-14T17:44:00Z') })).toBeNull();
  });
});
