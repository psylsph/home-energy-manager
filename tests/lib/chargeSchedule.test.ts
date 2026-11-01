import { describe, it, expect } from 'vitest';
import { batteryEvent, batteryStoredKwh, doingIt } from '../../src/lib/chargeSchedule';
import type { InverterSnapshot, ScheduleSlot } from '../../src/lib/types';

/** Charging card helpers for the house view (issue #422, round 2). */

const slot = (sh: number, sm: number, eh: number, em: number, enabled = true): ScheduleSlot => ({
  enabled, start_hour: sh, start_minute: sm, end_hour: eh, end_minute: em, target_soc: 100,
});
const at = (h: number, m = 0) => h * 60 + m;

/** The charge window alone: batteryEvent with nothing else scheduled. */
function chargeWindow(slots: ScheduleSlot[] | undefined, enableCharge: boolean, nowMinute: number) {
  const event = batteryEvent({
    charge_slots: slots as ScheduleSlot[], enable_charge: enableCharge,
    discharge_slots: [], enable_discharge: false, battery_mode: 'eco',
    max_charge_slots: 0, max_discharge_slots: 0,
  }, null, nowMinute);
  if (!event) return null;
  const { kind, ...window } = event;
  expect(kind).toBe('charge');
  return window;
}

describe('chargeWindow', () => {
  const overnight = [slot(23, 30, 5, 30)];

  it('shows the next slot before it starts', () => {
    expect(chargeWindow(overnight, true, at(18))).toEqual({ state: 'next', start: '23:30', end: '05:30' });
  });

  it('is active from the start minute, across midnight, until the end minute', () => {
    expect(chargeWindow(overnight, true, at(23, 30))?.state).toBe('active');
    expect(chargeWindow(overnight, true, at(0, 10))?.state).toBe('active');
    expect(chargeWindow(overnight, true, at(5, 29))?.state).toBe('active');
    expect(chargeWindow(overnight, true, at(5, 30))?.state).toBe('next');
  });

  it('handles a same-day slot', () => {
    const day = [slot(13, 0, 16, 0)];
    expect(chargeWindow(day, true, at(12, 59))?.state).toBe('next');
    expect(chargeWindow(day, true, at(13))?.state).toBe('active');
    expect(chargeWindow(day, true, at(16))).toEqual({ state: 'next', start: '13:00', end: '16:00' });
  });

  it('merges overlapping and adjacent slots into one window', () => {
    // 16:00–17:00 + 17:00–18:00 is one export to 18:00, as the Control page says.
    expect(chargeWindow([slot(16, 0, 17, 0), slot(17, 0, 18, 0)], true, at(16, 30))).toEqual({ state: 'active', start: '16:00', end: '18:00' });
    expect(chargeWindow([slot(2, 0, 4, 0), slot(1, 0, 3, 0)], true, at(2, 30))).toEqual({ state: 'active', start: '01:00', end: '04:00' });
    // Merging across midnight keeps the real end.
    expect(chargeWindow([slot(22, 0, 0, 0), slot(0, 0, 2, 0)], true, at(23))).toEqual({ state: 'active', start: '22:00', end: '02:00' });
  });

  it('shows a whole-day schedule as running', () => {
    expect(chargeWindow([slot(0, 0, 12, 0), slot(12, 0, 0, 0)], true, at(9))).toEqual({ state: 'active', start: '00:00', end: '00:00' });
  });

  it('picks the soonest slot to start, wrapping past midnight', () => {
    const slots = [slot(13, 0, 15, 0), slot(2, 0, 4, 0)];
    expect(chargeWindow(slots, true, at(16))?.start).toBe('02:00');
    expect(chargeWindow(slots, true, at(5))?.start).toBe('13:00');
  });

  it('ignores disabled and cleared slots', () => {
    expect(chargeWindow([slot(1, 0, 3, 0, false), slot(0, 0, 0, 0)], true, at(2))).toBeNull();
  });

  it('shows nothing while Timed Charge is off or there are no slots', () => {
    expect(chargeWindow(overnight, false, at(0))).toBeNull();
    expect(chargeWindow(undefined, true, at(0))).toBeNull();
    expect(chargeWindow([], true, at(0))).toBeNull();
  });
});

describe('batteryEvent', () => {
  const charge = [slot(23, 30, 5, 30)];
  const exp = [slot(16, 0, 19, 0)];
  type Snap = Parameters<typeof batteryEvent>[0];
  const snap = (over: Partial<Snap> = {}): Snap => ({
    charge_slots: charge, enable_charge: true,
    discharge_slots: [], enable_discharge: false,
    battery_mode: 'eco',
    max_charge_slots: 2, max_discharge_slots: 2,
    ...over,
  });
  const managed = (slots = exp, enabled = true) => ({ schedule_enabled: enabled, slots });

  it('shows a managed Timed Export that starts first, even with the inverter in Eco and its slots cleared', () => {
    expect(batteryEvent(snap(), managed(), at(12))).toEqual({ kind: 'export', state: 'next', start: '16:00', end: '19:00' });
  });

  it('shows the charge when it starts first', () => {
    expect(batteryEvent(snap(), managed(), at(20))).toEqual({ kind: 'charge', state: 'next', start: '23:30', end: '05:30' });
  });

  it('shows a running export over a charge still to come, and the reverse', () => {
    expect(batteryEvent(snap(), managed(), at(17))).toMatchObject({ kind: 'export', state: 'active' });
    expect(batteryEvent(snap(), managed(), at(1))).toMatchObject({ kind: 'charge', state: 'active' });
  });

  it('prefers the charge on a tie', () => {
    expect(batteryEvent(snap({ charge_slots: [slot(16, 0, 17, 0)] }), managed(), at(12))?.kind).toBe('charge');
    expect(batteryEvent(snap({ charge_slots: [slot(15, 0, 17, 0)] }), managed(), at(16, 30))?.kind).toBe('charge');
  });

  it('ignores a managed schedule that is switched off', () => {
    expect(batteryEvent(snap(), managed(exp, false), at(12))?.kind).toBe('charge');
  });

  it('weighs the managed schedule and the physical discharge slots together: the soonest wins', () => {
    const s = snap({ discharge_slots: [slot(13, 0, 14, 0)], enable_discharge: true, battery_mode: 'timed_export' });
    expect(batteryEvent(s, managed(), at(12))).toMatchObject({ kind: 'export', start: '13:00' });
    expect(batteryEvent(s, managed(), at(15))).toMatchObject({ kind: 'export', start: '16:00' });
  });

  it('falls back to armed physical slots: exports in Timed Export or Export Paused', () => {
    for (const battery_mode of ['timed_export', 'export_paused'] as const) {
      const s = snap({ discharge_slots: exp, enable_discharge: true, battery_mode });
      expect(batteryEvent(s, null, at(12))?.kind).toBe('export');
    }
  });

  it('calls armed physical slots discharges in Timed Demand', () => {
    const s = snap({ discharge_slots: exp, enable_discharge: true, battery_mode: 'timed_demand' });
    expect(batteryEvent(s, null, at(12))?.kind).toBe('discharge');
  });

  it("counts only the slots the inverter has, as the Control page shows them", () => {
    // A one-slot model: a leftover second slot (e.g. HR 31-32 on Gen1) is ignored.
    const s = snap({ charge_slots: [slot(0, 0, 0, 0), slot(2, 0, 4, 0)], max_charge_slots: 1 });
    expect(batteryEvent(s, null, at(1))).toBeNull();
    const e = snap({ charge_slots: [], max_discharge_slots: 1 });
    expect(batteryEvent(e, managed([slot(0, 0, 0, 0), slot(16, 0, 19, 0)]), at(12))).toBeNull();
    // An unknown count (0) keeps every slot.
    expect(batteryEvent(snap({ max_charge_slots: 0 }), null, at(20))?.kind).toBe('charge');
  });

  it("shows the Control page's Timed Discharge, stored in the pause registers", () => {
    // Pause mode 2 with the visible window 03:00–04:00 (already inverse-decoded
    // by the backend); enable_discharge stays off.
    const s = snap({
      charge_slots: [], enable_charge: false,
      battery_pause_mode: 2, battery_pause_slot: slot(3, 0, 4, 0),
    } as Partial<Snap>);
    expect(batteryEvent(s, null, at(2))).toEqual({ kind: 'discharge', state: 'next', start: '03:00', end: '04:00' });
    expect(batteryEvent(s, null, at(3, 30))).toMatchObject({ kind: 'discharge', state: 'active' });
    // Not armed (pause mode 0/1/3), or no window: nothing.
    expect(batteryEvent({ ...s, battery_pause_mode: 1 } as Snap, null, at(2))).toBeNull();
    expect(batteryEvent({ ...s, battery_pause_slot: undefined } as Snap, null, at(2))).toBeNull();
  });

  it('shows a manual export running now even while the Timed Export schedule waits for later', () => {
    // Force Discharge (or calibration / Agile) arms a discharge slot from now.
    const s = snap({ discharge_slots: [slot(10, 0, 11, 0)], enable_discharge: true, battery_mode: 'timed_export' });
    expect(batteryEvent(s, managed(), at(10, 15))).toEqual({ kind: 'export', state: 'active', start: '10:00', end: '11:00' });
  });

  it('ignores disarmed physical slots and a disarmed charge', () => {
    expect(batteryEvent(snap({ discharge_slots: exp, enable_charge: false }), null, at(12))).toBeNull();
    expect(batteryEvent(snap({ enable_charge: false }), managed(), at(20))?.kind).toBe('export');
    expect(batteryEvent(snap({ charge_slots: undefined as never }), null, at(12))).toBeNull();
  });
});

describe('doingIt', () => {
  const noise = 20;
  it('charging: battery power into the battery above the noise floor', () => {
    expect(doingIt('charge', { battery_power: -2000, grid_power: 0 } as InverterSnapshot, noise)).toBe(true);
    expect(doingIt('charge', { battery_power: -5, grid_power: 0 } as InverterSnapshot, noise)).toBe(false);
    expect(doingIt('charge', { battery_power: 2000, grid_power: 0 } as InverterSnapshot, noise)).toBe(false);
  });

  it("exporting: the Control page's own test, the battery discharging or the grid exporting", () => {
    const t = (battery_power: number, grid_power: number) => doingIt('export', { battery_power, grid_power } as InverterSnapshot, noise);
    expect(t(2500, 2000)).toBe(true);
    // Battery at reserve while solar exports: Timed Export is still active.
    expect(t(0, 2000)).toBe(true);
    expect(t(300, 0)).toBe(true);
    expect(t(5, 10)).toBe(false);
  });

  it('discharging: battery power out above the noise floor', () => {
    expect(doingIt('discharge', { battery_power: 300, grid_power: 0 } as InverterSnapshot, noise)).toBe(true);
    expect(doingIt('discharge', { battery_power: 10, grid_power: 0 } as InverterSnapshot, noise)).toBe(false);
  });
});

describe('batteryStoredKwh', () => {
  it('multiplies SOC by capacity', () => {
    expect(batteryStoredKwh(33.5, 8.06)).toBeCloseTo(2.7, 1);
    expect(batteryStoredKwh(100, 9.5)).toBe(9.5);
  });

  it('clamps an out-of-range SOC', () => {
    expect(batteryStoredKwh(-5, 9.5)).toBe(0);
    expect(batteryStoredKwh(140, 9.5)).toBe(9.5);
  });

  it('returns null when the capacity or SOC is unknown', () => {
    expect(batteryStoredKwh(50, 0)).toBeNull();
    expect(batteryStoredKwh(50, Number.NaN)).toBeNull();
    expect(batteryStoredKwh(Number.NaN, 9.5)).toBeNull();
  });
});
