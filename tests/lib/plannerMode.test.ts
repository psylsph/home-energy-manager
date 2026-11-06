import { describe, it, expect } from 'vitest';
import { plannerAutomationOn, plannerModeConflict, plannerTimedChargeOff } from '../../src/lib/plannerMode';
import type { InverterSnapshot, ScheduleSlot } from '../../src/lib/types';

const SLOT: ScheduleSlot = {
  enabled: true, start_hour: 0, start_minute: 30, end_hour: 5, end_minute: 30, target_soc: 100,
};
const EMPTY_SLOT: ScheduleSlot = {
  enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100,
};

function snap(over: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    device_type_code: '2001',
    charging_mode: 'standard',
    enable_charge: true,
    charge_slots: [SLOT, EMPTY_SLOT],
    forecast_plan_auto_refresh: true,
    forecast_plan_auto_apply_enabled: false,
    ...over,
  } as InverterSnapshot;
}

describe('plannerModeConflict', () => {
  it('is quiet in Standard mode, and when the mode is not reported', () => {
    expect(plannerModeConflict(snap())).toBeNull();
    expect(plannerModeConflict(snap({ charging_mode: undefined }))).toBeNull();
    expect(plannerModeConflict(null)).toBeNull();
  });

  it('names Cosy and says it rewrites charge slot 1', () => {
    const c = plannerModeConflict(snap({ charging_mode: 'cosy' }));
    expect(c?.mode).toBe('Cosy');
    expect(c?.message).toMatch(/Cosy/);
    expect(c?.message).toMatch(/overwrite/i);
  });

  it('names Adaptive Charge and says the plan waits instead', () => {
    const c = plannerModeConflict(snap({ charging_mode: 'adaptive' }));
    expect(c?.mode).toBe('Adaptive Charge');
    expect(c?.message).toMatch(/wait/i);
  });

  it('warns for the Agile modes that charge, not the discharge-only one', () => {
    expect(plannerModeConflict(snap({ charging_mode: 'agile' }))?.mode).toBe('Agile');
    expect(plannerModeConflict(snap({ charging_mode: 'agile_charge' }))?.mode).toBe('Agile Charge');
    expect(plannerModeConflict(snap({ charging_mode: 'agile_discharge' }))).toBeNull();
  });
});

describe('plannerTimedChargeOff', () => {
  it('shows when automatic planning left a charge slot set but Timed Charge is off', () => {
    expect(plannerTimedChargeOff(snap({ enable_charge: false }))).toBe(true);
    expect(plannerTimedChargeOff(snap({
      enable_charge: false, forecast_plan_auto_refresh: false, forecast_plan_auto_apply_enabled: true,
    }))).toBe(true);
  });

  it('stays quiet while Timed Charge is armed', () => {
    expect(plannerTimedChargeOff(snap({ enable_charge: true }))).toBe(false);
  });

  it('stays quiet when automatic planning is off', () => {
    expect(plannerTimedChargeOff(snap({
      enable_charge: false, forecast_plan_auto_refresh: false, forecast_plan_auto_apply_enabled: false,
    }))).toBe(false);
  });

  it('stays quiet when the plan needed no charge and cleared the slot', () => {
    expect(plannerTimedChargeOff(snap({ enable_charge: false, charge_slots: [EMPTY_SLOT, EMPTY_SLOT] }))).toBe(false);
  });

  it('stays quiet when another mode owns the schedule (that has its own warning)', () => {
    expect(plannerTimedChargeOff(snap({ enable_charge: false, charging_mode: 'cosy' }))).toBe(false);
    expect(plannerTimedChargeOff(snap({ enable_charge: false, charging_mode: 'adaptive' }))).toBe(false);
  });

  it('looks at charge slot 1 only, the slot the Planner writes', () => {
    // The plan needed no charge and cleared slot 1; an unrelated slot 2 stays set.
    expect(plannerTimedChargeOff(snap({ enable_charge: false, charge_slots: [EMPTY_SLOT, SLOT] }))).toBe(false);
  });

  it('stays quiet on three-phase models, where the Planner does not set Timed Charge', () => {
    expect(plannerTimedChargeOff(snap({ enable_charge: false, device_type_code: '4001' }))).toBe(false);
    expect(plannerTimedChargeOff(snap({ enable_charge: false, device_type_code: '6001' }))).toBe(false);
  });

  it('stays quiet on a Gateway, where automatic planning sits out', () => {
    expect(plannerTimedChargeOff(snap({ enable_charge: false, device_type_code: '7001' }))).toBe(false);
  });

  it('stays quiet without a snapshot', () => {
    expect(plannerTimedChargeOff(null)).toBe(false);
  });
});

describe('plannerAutomationOn', () => {
  it('is on for either trigger, and off for neither', () => {
    expect(plannerAutomationOn(snap({ forecast_plan_auto_refresh: true }))).toBe(true);
    expect(plannerAutomationOn(snap({
      forecast_plan_auto_refresh: false, forecast_plan_auto_apply_enabled: true,
    }))).toBe(true);
    expect(plannerAutomationOn(snap({
      forecast_plan_auto_refresh: false, forecast_plan_auto_apply_enabled: false,
    }))).toBe(false);
    expect(plannerAutomationOn(null)).toBe(false);
  });

  it('is off on a Gateway even when the setting is still on', () => {
    expect(plannerAutomationOn(snap({ device_type_code: '7001' }))).toBe(false);
  });
});
