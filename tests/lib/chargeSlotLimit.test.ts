import { describe, it, expect } from 'vitest';
import {
  assessChargeSlot,
  describeChargeSlotLimit,
  slotWindowMinutes,
} from '../../src/lib/chargeSlotLimit';

// Issue #362: the charge limit is one global setting, so a long timed-charge
// slot can be silently throttled. These helpers say what a slot can deliver at
// the limit the inverter currently holds. Everything is pure and takes the clock
// as an argument, so no test depends on the time of day it runs.

const night = { start_hour: 0, start_minute: 30, end_hour: 6, end_minute: 30, target_soc: 100 };

describe('slotWindowMinutes', () => {
  it('measures a same-day window', () => {
    expect(slotWindowMinutes(night)).toBe(360);
  });

  it('wraps a window that crosses midnight', () => {
    expect(slotWindowMinutes({ start_hour: 23, start_minute: 30, end_hour: 5, end_minute: 30 })).toBe(360);
  });

  it('treats an empty window as zero rather than a full day', () => {
    expect(slotWindowMinutes({ start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0 })).toBe(0);
    expect(slotWindowMinutes({ start_hour: 3, start_minute: 15, end_hour: 3, end_minute: 15 })).toBe(0);
  });

  it('counts only the time left when the clock is inside the window', () => {
    expect(slotWindowMinutes(night, 5 * 60 + 30)).toBe(60);
  });

  it('counts the time left across midnight for a wrapping window', () => {
    const wrapping = { start_hour: 23, start_minute: 30, end_hour: 5, end_minute: 30 };
    expect(slotWindowMinutes(wrapping, 1 * 60)).toBe(4 * 60 + 30);
    expect(slotWindowMinutes(wrapping, 23 * 60 + 45)).toBe(5 * 60 + 45);
  });

  it('uses the whole window when the clock is outside it or unknown', () => {
    expect(slotWindowMinutes(night, 12 * 60)).toBe(360);
    expect(slotWindowMinutes(night, null)).toBe(360);
    expect(slotWindowMinutes(night, 6 * 60 + 30)).toBe(360); // end is exclusive
  });
});

describe('assessChargeSlot', () => {
  const base = { slot: night, limitWatts: 2600, capacityKwh: 9.5, socPercent: 20, nowMinute: 12 * 60 };

  it('reports what the slot can add at the limit', () => {
    const a = assessChargeSlot(base);
    // 2.6 kW for 6 h = 15.6 kWh, more than the 9.5 kWh pack holds.
    expect(a.kind).toBe('ok');
    if (a.kind === 'none') throw new Error('unreachable');
    expect(a.addableKwh).toBeCloseTo(15.6, 5);
    expect(a.addablePercent).toBeGreaterThan(100);
    expect(a.hours).toBe(6);
  });

  it('flags a slot the limit stops from reaching the target', () => {
    // 0.4 kW for 6 h = 2.4 kWh = ~25% of 9.5 kWh; 20% + 25% = 45%, short of 100%.
    const a = assessChargeSlot({ ...base, limitWatts: 400 });
    expect(a.kind).toBe('short');
    if (a.kind !== 'short') throw new Error('unreachable');
    expect(a.addablePercent).toBeCloseTo(25.26, 1);
    expect(a.reachPercent).toBeCloseTo(45.26, 1);
    expect(a.targetPercent).toBe(100);
    expect(a.socPercent).toBe(20);
  });

  it('is not short when the limit just covers what is needed', () => {
    // Needs 80% of 9.5 kWh = 7.6 kWh over 6 h = 1267 W.
    expect(assessChargeSlot({ ...base, limitWatts: 1300 }).kind).toBe('ok');
  });

  it('ignores a shortfall of a point or two, which is rounding on the reading', () => {
    // 1250 W over 6 h is 78.9%; 20 + 78.9 = 98.9, one point under 100.
    expect(assessChargeSlot({ ...base, limitWatts: 1250 }).kind).toBe('ok');
    expect(assessChargeSlot({ ...base, limitWatts: 1100 }).kind).toBe('short');
  });

  it('is never short when the battery already holds the target', () => {
    expect(assessChargeSlot({ ...base, limitWatts: 400, socPercent: 100 }).kind).toBe('ok');
    expect(assessChargeSlot({ ...base, limitWatts: 400, socPercent: 80, slot: { ...night, target_soc: 70 } }).kind).toBe('ok');
  });

  it('still describes the slot when the battery level is unknown, but never warns', () => {
    const a = assessChargeSlot({ ...base, limitWatts: 400, socPercent: null });
    expect(a.kind).toBe('ok');
  });

  it('uses only the remaining time once the slot is running', () => {
    // 5:30 with the slot ending at 06:30: one hour left, so 1 kW adds 1 kWh.
    const a = assessChargeSlot({ ...base, limitWatts: 1000, nowMinute: 5 * 60 + 30 });
    if (a.kind === 'none') throw new Error('unreachable');
    expect(a.addableKwh).toBeCloseTo(1, 5);
    expect(a.inProgress).toBe(true);
    expect(a.kind).toBe('short');
  });

  it('flags a limit of zero, which stops charging altogether', () => {
    // The DC-hybrid register accepts 0: the most restrictive limit there is.
    const a = assessChargeSlot({ ...base, limitWatts: 0 });
    expect(a.kind).toBe('short');
    if (a.kind !== 'short') throw new Error('unreachable');
    expect(a.addableKwh).toBe(0);
    expect(a.reachPercent).toBe(20);
  });

  it('counts the other enabled slots, which charge before the schedule ends', () => {
    // 1.3 kW for 2 h is 27% of the pack, short of 80% on its own; with a 4 h
    // slot beside it the schedule adds 82% and does reach the target.
    const first = { ...night, start_hour: 0, start_minute: 0, end_hour: 2, end_minute: 0 };
    const second = { ...night, start_hour: 2, start_minute: 0, end_hour: 6, end_minute: 0 };
    const alone = assessChargeSlot({ ...base, slot: first, limitWatts: 1300 });
    expect(alone.kind).toBe('short');
    const together = assessChargeSlot({ ...base, slot: first, limitWatts: 1300, otherSlots: [second] });
    expect(together.kind).toBe('ok');
    if (together.kind === 'none') throw new Error('unreachable');
    expect(together.hours).toBe(6);
  });

  it('still flags a schedule that falls short even with every slot counted', () => {
    const first = { ...night, start_hour: 0, start_minute: 0, end_hour: 2, end_minute: 0 };
    const second = { ...night, start_hour: 2, start_minute: 0, end_hour: 3, end_minute: 0 };
    expect(assessChargeSlot({ ...base, slot: first, limitWatts: 1300, otherSlots: [second] }).kind).toBe('short');
  });

  it('does not credit a neighbour that stops charging at a lower target', () => {
    // Per-slot targets (Gen3): the 50% slot stops there, so it cannot help
    // this slot reach 100%.
    const first = { ...night, start_hour: 0, start_minute: 0, end_hour: 2, end_minute: 0 };
    const lower = { ...night, start_hour: 2, start_minute: 0, end_hour: 6, end_minute: 0, target_soc: 50 };
    expect(assessChargeSlot({ ...base, slot: first, limitWatts: 1300, otherSlots: [lower] }).kind).toBe('short');
    const equal = { ...lower, target_soc: 100 };
    expect(assessChargeSlot({ ...base, slot: first, limitWatts: 1300, otherSlots: [equal] }).kind).toBe('ok');
  });

  it('counts only the time left in a running neighbour', () => {
    const running = { ...night, start_hour: 0, start_minute: 0, end_hour: 6, end_minute: 0 };
    const later = { ...night, start_hour: 8, start_minute: 0, end_hour: 10, end_minute: 0 };
    const a = assessChargeSlot({ ...base, slot: later, limitWatts: 1000, otherSlots: [running], nowMinute: 5 * 60 });
    if (a.kind === 'none') throw new Error('unreachable');
    // 1 h left in the running slot + 2 h in this one.
    expect(a.hours).toBe(3);
    expect(a.inProgress).toBe(false);
  });

  it.each([
    ['no limit', { limitWatts: null }],
    ['a negative limit', { limitWatts: -5 }],
    ['a non-finite limit', { limitWatts: Number.NaN }],
    ['no capacity', { capacityKwh: 0 }],
    ['an implausible capacity from a corrupt read', { capacityKwh: 4000 }],
    ['an empty window', { slot: { ...night, end_hour: 0, end_minute: 30 } }],
  ])('says nothing for %s', (_label, override) => {
    expect(assessChargeSlot({ ...base, ...override }).kind).toBe('none');
  });
});

describe('describeChargeSlotLimit', () => {
  const input = {
    slot: night,
    limitPercent: 17,
    limitWatts: 400,
    capacityKwh: 9.5,
    socPercent: 20,
    nowMinute: 12 * 60,
  };

  it('states the limit and a shortfall in plain words', () => {
    const d = describeChargeSlotLimit(input);
    expect(d.limitLine).toBe('Charge limit: 17% (0.4 kW), set under Battery and Power Controls.');
    expect(d.shortfall).toBe(
      'At this limit the slot can add about 2.4 kWh (25% of the battery). From the current 20% it would reach about 45%, short of the 100% target.',
    );
  });

  it('reports no shortfall when the target is reachable', () => {
    const d = describeChargeSlotLimit({ ...input, limitPercent: 100, limitWatts: 2600 });
    expect(d.limitLine).toBe('Charge limit: 100% (2.6 kW), set under Battery and Power Controls.');
    expect(d.shortfall).toBeNull();
  });

  it('says the slot has already started when it is running', () => {
    const d = describeChargeSlotLimit({ ...input, nowMinute: 5 * 60 + 30 });
    expect(d.shortfall).toContain('In the time left this slot can add about 0.4 kWh');
  });

  it('names the enabled slots together when others contribute', () => {
    const other = { ...night, start_hour: 8, start_minute: 0, end_hour: 9, end_minute: 0 };
    const d = describeChargeSlotLimit({ ...input, otherSlots: [other] });
    expect(d.shortfall).toBe(
      'At this limit your enabled slots together can add about 2.8 kWh (29% of the battery). '
      + 'From the current 20% it would reach about 49%, short of the 100% target.',
    );
  });

  it('gives only the limit when the schedule is switched off, since nothing will run', () => {
    const d = describeChargeSlotLimit({ ...input, armed: false });
    expect(d.limitLine).toBe('Charge limit: 17% (0.4 kW), set under Battery and Power Controls.');
    expect(d.shortfall).toBeNull();
  });

  it('describes a zero limit as adding nothing', () => {
    const d = describeChargeSlotLimit({ ...input, limitPercent: 0, limitWatts: 0 });
    expect(d.limitLine).toBe('Charge limit: 0%, set under Battery and Power Controls.');
    expect(d.shortfall).toBe(
      'At this limit the slot can add about 0.0 kWh (0% of the battery). '
      + 'From the current 20% it would reach about 20%, short of the 100% target.',
    );
  });

  it('gives only the limit when the watt figure is unknown', () => {
    const d = describeChargeSlotLimit({ ...input, limitWatts: null });
    expect(d.limitLine).toBe('Charge limit: 17%, set under Battery and Power Controls.');
    expect(d.shortfall).toBeNull();
  });

  it('names the automation that owns the limit and suppresses the warning', () => {
    const d = describeChargeSlotLimit({ ...input, managedBy: 'Adaptive Charge' });
    expect(d.limitLine).toBe('Charge limit: 17% (0.4 kW). Adaptive Charge adjusts this.');
    expect(d.shortfall).toBeNull();
  });

  it('shows nothing when the limit itself is unknown', () => {
    expect(describeChargeSlotLimit({ ...input, limitPercent: null })).toEqual({ limitLine: null, shortfall: null });
  });
});
