import { describe, it, expect } from 'vitest';
import { batteryRuntimeLine, readBatteryRuntime, type RuntimeFormat } from '../../src/lib/batteryRuntime';

// Issue #472. UTC formatters, so the tests don't depend on the time zone.
const UTC: RuntimeFormat = {
  time: (d) => d.toISOString().slice(11, 16),
  hour: (d) => d.getUTCHours(),
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

  it('reads a risk only with its confidence, and an older answer without either', () => {
    const base = { outcome: 'recharges_first', at: 5, by: 'charge' };
    expect(readBatteryRuntime({ ok: true, data: base })).toEqual(base);
    expect(readBatteryRuntime({ ok: true, data: { ...base, risk: 3, confidence: 'low' } })).not.toBeNull();
    expect(readBatteryRuntime({ ok: true, data: { ...base, risk: 3 } })).toBeNull();
    expect(readBatteryRuntime({ ok: true, data: { ...base, confidence: 'low' } })).toBeNull();
    expect(readBatteryRuntime({ ok: true, data: { ...base, risk: 3, confidence: 'sure' } })).toBeNull();
    expect(readBatteryRuntime({ ok: true, data: { ...base, risk: 'soon', confidence: 'low' } })).toBeNull();
  });
});

describe('batteryRuntimeLine (issue #472)', () => {
  it('says a range when a busy and a quiet day disagree, with a confidence', () => {
    const l = line({
      outcome: 'empty_at', at: ts('2026-11-14T22:30:00Z'),
      earliest: ts('2026-11-14T21:30:00Z'), latest: ts('2026-11-14T23:00:00Z'), confidence: 'medium',
    })!;
    // 18:00 or later today is "tonight".
    expect(l.text).toBe('Empty tonight between 21:30 and 23:00');
    expect(l.confidence).toBe('medium');
    expect(l.dots).toBe(2);
    expect(l.title).toBe('Most likely 22:30 · between 21:30 and 23:00 · medium confidence');
  });

  it('says "about" when the ends are within half an hour, never an exact time', () => {
    const l = line({
      outcome: 'empty_at', at: ts('2026-11-14T22:00:00Z'),
      earliest: ts('2026-11-14T22:00:00Z'), latest: ts('2026-11-14T22:30:00Z'), confidence: 'high',
    })!;
    expect(l.text).toBe('Empty tonight at about 22:00');
    expect(l.dots).toBe(3);
    const same = line({
      outcome: 'empty_at', at: ts('2026-11-14T22:00:00Z'),
      earliest: ts('2026-11-14T22:00:00Z'), latest: ts('2026-11-14T22:00:00Z'), confidence: 'high',
    })!;
    expect(same.text).toBe('Empty tonight at about 22:00');
    // Without a range at all, still a prediction.
    expect(line({ outcome: 'empty_at', at: ts('2026-11-14T22:00:00Z'), earliest: null, latest: null, confidence: 'low' })!.text)
      .toBe('Empty tonight at about 22:00');
  });

  it('says "or later" when the quiet day never empties', () => {
    const l = line({ outcome: 'empty_at', at: ts('2026-11-14T22:30:00Z'), earliest: ts('2026-11-14T21:30:00Z'), latest: null, confidence: 'low' })!;
    expect(l.text).toBe('Empty tonight at 21:30 or later');
    expect(l.title).toBe('Most likely 22:30 · not before about 21:30 · low confidence');
    expect(l.dots).toBe(1);
  });

  it('explains the time and the dots in words, for the ⓘ', () => {
    const l = line({
      outcome: 'empty_at', at: ts('2026-11-14T22:30:00Z'),
      earliest: ts('2026-11-14T21:30:00Z'), latest: ts('2026-11-14T23:00:00Z'), confidence: 'medium',
    })!;
    expect(l.help).toBe(
      'Most likely 22:30, between 21:30 and 23:00. This is when the battery reaches its reserve, from your usage '
      + 'history and the solar forecast. A busier day could empty it sooner, a quiet one later. '
      + 'Medium confidence (two dots): a busy and a quiet day are within three hours of each other.',
    );
    const low = line({ outcome: 'empty_at', at: ts('2026-11-14T22:30:00Z'), earliest: null, latest: null, confidence: 'low' })!;
    expect(low.help).toMatch(/^Most likely 22:30\. This is when .* Low confidence \(one dot\): a busy and a quiet day are more than three hours apart, or there's under two weeks of history\.$/);
    const high = line({ outcome: 'empty_at', at: ts('2026-11-14T22:30:00Z'), earliest: null, latest: null, confidence: 'high' })!;
    expect(high.help).toMatch(/High confidence \(three dots\): a busy and a quiet day are within an hour of each other\.$/);
    // Lines without dots need no explaining.
    expect(line({ outcome: 'at_reserve' })!.help).toBeNull();
  });

  it('names tomorrow, and a later day by its weekday, from the range start', () => {
    expect(line({ outcome: 'empty_at', at: ts('2026-11-15T06:00:00Z'), earliest: null, latest: null, confidence: 'low' })!.text)
      .toBe('Empty tomorrow at about 06:00');
    expect(line({ outcome: 'empty_at', at: ts('2026-11-16T06:00:00Z'), earliest: null, latest: null, confidence: 'low' })!.text)
      .toBe('Empty Mon at about 06:00');
    expect(line({
      outcome: 'empty_at', at: ts('2026-11-15T06:00:00Z'),
      earliest: ts('2026-11-15T05:00:00Z'), latest: ts('2026-11-15T07:30:00Z'), confidence: 'medium',
    })!.text).toBe('Empty tomorrow between 05:00 and 07:30');
  });

  it('names the day from the range start, and a range may run over midnight', () => {
    // Starts tonight, ends after midnight: the day word is the start's.
    expect(line({
      outcome: 'empty_at', at: ts('2026-11-14T23:30:00Z'),
      earliest: ts('2026-11-14T22:30:00Z'), latest: ts('2026-11-15T01:00:00Z'), confidence: 'medium',
    })!.text).toBe('Empty tonight between 22:30 and 01:00');
    // A start before 18:00 today has no day word.
    const day = new Date('2026-11-14T08:00:00Z');
    const early = batteryRuntimeLine(readBatteryRuntime({ ok: true, data: {
      outcome: 'empty_at', at: ts('2026-11-14T15:00:00Z'),
      earliest: ts('2026-11-14T14:00:00Z'), latest: ts('2026-11-14T16:00:00Z'), confidence: 'medium',
    } }), day, UTC)!;
    expect(early.text).toBe('Empty between 14:00 and 16:00');
  });

  it('says only the likely time when the start of the range has already passed', () => {
    const l = line({
      outcome: 'empty_at', at: ts('2026-11-14T18:30:00Z'),
      earliest: ts('2026-11-14T17:30:00Z'), latest: ts('2026-11-14T20:00:00Z'), confidence: 'medium',
    })!;
    expect(l.text).toBe('Empty tonight at about 18:30');
  });

  it('says what refills it first', () => {
    expect(line({ outcome: 'recharges_first', at: ts('2026-11-14T23:30:00Z'), by: 'charge' })!.text)
      .toBe('Lasts until the 23:30 charge');
    expect(line({ outcome: 'recharges_first', at: ts('2026-11-15T09:00:00Z'), by: 'solar' })!.text)
      .toBe('Lasts until solar tomorrow at about 09:00');
    expect(line({ outcome: 'recharges_first', at: ts('2026-11-15T09:00:00Z'), by: 'solar' })!.dots).toBeNull();
  });

  it("names the charge's day when it isn't today", () => {
    expect(line({ outcome: 'recharges_first', at: ts('2026-11-15T00:30:00Z'), by: 'charge' })!.text)
      .toBe("Lasts until tomorrow's 00:30 charge");
    expect(line({ outcome: 'recharges_first', at: ts('2026-11-16T06:00:00Z'), by: 'charge' })!.text)
      .toBe("Lasts until Mon's 06:00 charge");
  });

  it('warns when a busy evening could run out before the charge, with dots', () => {
    const l = line({
      outcome: 'recharges_first', at: ts('2026-11-14T23:30:00Z'), by: 'charge',
      risk: ts('2026-11-14T21:30:00Z'), confidence: 'medium',
    })!;
    expect(l.text).toBe('Empty tonight at 21:30 or later, charge at 23:30');
    expect(l.dots).toBe(2);
    expect(l.confidence).toBe('medium');
    expect(l.title).toBe(
      'Most likely it lasts until the 23:30 charge, but a busier day than usual (more use, less sun) could empty it from 21:30 · medium confidence',
    );
    expect(l.help).toBe(
      'Most likely it lasts until the 23:30 charge, but a busier day than usual (more use, less sun) could empty it from 21:30. '
      + 'Two dots: a busier day would run out up to three hours before the charge.',
    );
  });

  it('names the charge\'s day only when it differs from the risk\'s', () => {
    expect(line({
      outcome: 'recharges_first', at: ts('2026-11-15T00:10:00Z'), by: 'charge',
      risk: ts('2026-11-14T22:30:00Z'), confidence: 'high',
    })!.text).toBe('Empty tonight at 22:30 or later, charge tomorrow at 00:10');
    // Both tomorrow: said once.
    expect(line({
      outcome: 'recharges_first', at: ts('2026-11-15T05:00:00Z'), by: 'charge',
      risk: ts('2026-11-15T03:00:00Z'), confidence: 'low',
    })!.text).toBe('Empty tomorrow at 03:00 or later, charge at 05:00');
  });

  it('warns the same way when solar is the refill', () => {
    const l = line({
      outcome: 'recharges_first', at: ts('2026-11-15T09:00:00Z'), by: 'solar',
      risk: ts('2026-11-15T02:00:00Z'), confidence: 'low',
    })!;
    expect(l.text).toBe('Empty tomorrow at 02:00 or later, solar at about 09:00');
    expect(l.dots).toBe(1);
    expect(l.help).toMatch(/Most likely it lasts until solar at about 09:00, but/);
    expect(l.help).toMatch(/One dot: a busier day would run out more than three hours before solar takes over/);
  });

  it('keeps the plain line when the busy day lasts too, or the backend is older', () => {
    // No risk: unchanged, no dots.
    const plain = line({ outcome: 'recharges_first', at: ts('2026-11-14T23:30:00Z'), by: 'charge' })!;
    expect(plain.text).toBe('Lasts until the 23:30 charge');
    expect(plain.dots).toBeNull();
  });

  it('says only that it may run out when the risk time has already passed, with the refill\'s day', () => {
    const l = line({
      outcome: 'recharges_first', at: ts('2026-11-14T23:30:00Z'), by: 'charge',
      risk: ts('2026-11-14T17:30:00Z'), confidence: 'high',
    })!;
    expect(l.text).toBe('May run out before the 23:30 charge');
    expect(l.dots).toBe(3);
    expect(line({
      outcome: 'recharges_first', at: ts('2026-11-15T06:00:00Z'), by: 'charge',
      risk: ts('2026-11-14T17:30:00Z'), confidence: 'low',
    })!.text).toBe("May run out before tomorrow's 06:00 charge");
    expect(line({
      outcome: 'recharges_first', at: ts('2026-11-15T09:00:00Z'), by: 'solar',
      risk: ts('2026-11-14T17:30:00Z'), confidence: 'low',
    })!.text).toBe('May run out before solar tomorrow at about 09:00');
  });

  it('tells an evening from the hour, not from how the time is written', () => {
    // A 12-hour formatter: "10:30 PM" must still be tonight.
    const twelve: RuntimeFormat = { ...UTC, time: (d) => `${d.getUTCHours() % 12 || 12}:30 ${d.getUTCHours() >= 12 ? 'PM' : 'AM'}` };
    const l = batteryRuntimeLine(readBatteryRuntime({ ok: true, data: {
      outcome: 'empty_at', at: ts('2026-11-14T22:30:00Z'), earliest: null, latest: null, confidence: 'low',
    } }), NOW, twelve)!;
    expect(l.text).toBe('Empty tonight at about 10:30 PM');
  });

  it('says when a charging battery is full, or reaches its slot target', () => {
    const full = line({ outcome: 'full_at', at: ts('2026-11-14T20:30:00Z'), soc: 100 })!;
    expect(full.text).toBe('Full at about 20:30');
    expect(full.dots).toBeNull();
    expect(line({ outcome: 'full_at', at: ts('2026-11-15T04:00:00Z'), soc: 80 })!.text).toBe('80% tomorrow at about 04:00');
  });

  // Issue #476: with the toggle on, a blank card looked broken, so a pause
  // says so rather than showing nothing.
  it('says discharge is paused, with why there is no time behind the ⓘ', () => {
    const l = line({ outcome: 'paused' })!;
    expect(l.text).toBe('Discharge paused');
    expect(l.dots).toBeNull();
    expect(l.confidence).toBeNull();
    expect(l.help).toMatch(/won't discharge until the pause ends/);
    expect(l.help).toMatch(/HEM can't tell when that will be, so there's no time to give\./);
  });

  it('covers the rest', () => {
    expect(line({ outcome: 'lasts_beyond', hours: 24 })!.text).toBe('Lasts over 24 hours');
    expect(line({ outcome: 'at_reserve' })!.text).toBe('At Reserve');
    expect(line({ outcome: 'below_reserve' })!.text).toBe('Below Reserve');
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

  it('shows no time that has already passed (issue #472)', () => {
    const past = ts('2026-11-14T17:30:00Z');
    expect(line({ outcome: 'recharges_first', at: past, by: 'solar' })).toBeNull();
    expect(line({ outcome: 'empty_at', at: past, earliest: null, latest: null, confidence: 'high' })).toBeNull();
    expect(line({ outcome: 'full_at', at: past, soc: 100 })).toBeNull();
    // A minute of slack for the clock; the future is untouched.
    expect(line({ outcome: 'recharges_first', at: ts('2026-11-14T17:59:30Z'), by: 'solar' })).not.toBeNull();
    expect(line({ outcome: 'recharges_first', at: ts('2026-11-14T18:30:00Z'), by: 'solar' })!.text).toBe('Lasts until solar at about 18:30');
  });

  it('still drops a stale cannot-calculate answer', () => {
    expect(line({ outcome: 'charging', computed_at: ts('2026-11-14T17:44:00Z') })).toBeNull();
  });

  it('drops an answer more than 15 minutes old', () => {
    expect(line({ outcome: 'at_reserve', computed_at: ts('2026-11-14T17:46:00Z') })).not.toBeNull();
    expect(line({ outcome: 'at_reserve', computed_at: ts('2026-11-14T17:44:00Z') })).toBeNull();
  });
});
