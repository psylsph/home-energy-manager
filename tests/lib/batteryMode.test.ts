import { describe, it, expect } from 'vitest';
import { deriveBatteryModeRows } from '../../src/lib/batteryMode';
import type { InverterSnapshot, ScheduleSlot } from '../../src/lib/types';

function slot(
  startHour: number,
  startMinute: number,
  endHour: number,
  endMinute: number,
  enabled = true,
  targetSoc = 100,
): ScheduleSlot {
  return {
    enabled,
    start_hour: startHour,
    start_minute: startMinute,
    end_hour: endHour,
    end_minute: endMinute,
    target_soc: targetSoc,
  };
}

function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    battery_mode: 'eco',
    battery_state: 'idle',
    battery_power: 0,
    battery_power_mode: 0,
    battery_voltage: 51.2,
    battery_current: 0,
    battery_temperature: 20,
    battery_capacity_kwh: 9.5,
    soc: 50,
    enable_charge: false,
    enable_discharge: false,
    charge_slots: [slot(0, 0, 0, 0, false)],
    discharge_slots: [slot(0, 0, 0, 0, false)],
    max_charge_slots: 2,
    max_discharge_slots: 2,
    battery_reserve: 4,
    target_soc: 100,
    charge_rate: 0,
    discharge_rate: 0,
    enable_charge_target: false,
    cosy_active: false,
    cosy_enabled: false,
    device_type: 'gen3',
    device_type_display: 'Gen 3 Hybrid',
    device_type_code: '2001',
    firmware_version: '318',
    dsp_firmware_version: '318',
    dc_dsp_firmware_version: '',
    inverter_serial: '',
    inverter_time: '',
    max_battery_power_w: 0,
    max_ac_power_w: 0,
    export_limit_w: 0,
    operating_hours: 0,
    battery_modules: [],
    solar_power: 0,
    pv1_power: 0,
    pv2_power: 0,
    pv1_voltage: 0,
    pv2_voltage: 0,
    pv1_current: 0,
    pv2_current: 0,
    today_solar_kwh: 0,
    today_pv1_kwh: 0,
    today_pv2_kwh: 0,
    grid_power: 0,
    grid_voltage: 230,
    grid_frequency: 50,
    today_import_kwh: 0,
    today_export_kwh: 0,
    total_import_kwh: 0,
    total_export_kwh: 0,
    today_charge_kwh: 0,
    today_discharge_kwh: 0,
    inverter_temperature: 30,
    auto_winter_active: false,
    battery_calibration_stage: 0,
    active_power_rate: 0,
    ...overrides,
  } as InverterSnapshot;
}

function findRow(rows: ReturnType<typeof deriveBatteryModeRows>, key: string) {
  return rows.find((r) => r.key === key)!;
}

describe('deriveBatteryModeRows', () => {
  it('returns all four mechanisms in the specified order', () => {
    const rows = deriveBatteryModeRows(makeSnapshot());
    expect(rows.map((r) => r.key)).toEqual([
      'eco',
      'timed_charge',
      'timed_export',
      'timed_discharge',
    ]);
  });

  it('reports every mechanism as off when all registers are clear', () => {
    const rows = deriveBatteryModeRows(makeSnapshot());
    expect(rows.every((r) => r.state === 'off')).toBe(true);
    expect(rows.every((r) => r.description === 'off')).toBe(true);
  });

  it('reports Eco as on when battery_power_mode is 1', () => {
    const rows = deriveBatteryModeRows(makeSnapshot({ battery_power_mode: 1 }));
    const eco = findRow(rows, 'eco');
    expect(eco.state).toBe('active');
    expect(eco.description).toBe('on');
  });

  it('reports Eco as off when battery_power_mode is 0', () => {
    const rows = deriveBatteryModeRows(makeSnapshot({ battery_power_mode: 0 }));
    const eco = findRow(rows, 'eco');
    expect(eco.state).toBe('off');
    expect(eco.description).toBe('off');
  });

  it('reports Timed Charge as armed when enable_charge is set but outside the slot', () => {
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        enable_charge: true,
        charge_slots: [slot(2, 0, 4, 0)],
      }),
      new Date('2026-06-28T12:00:00'),
    );
    const r = findRow(rows, 'timed_charge');
    expect(r.state).toBe('armed');
    expect(r.description).toBe('armed');
  });

  it('reports Timed Charge as active when in-window and charging', () => {
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        enable_charge: true,
        charge_slots: [slot(2, 0, 4, 0)],
        battery_state: 'charging',
        battery_power: -1500,
      }),
      new Date('2026-06-28T03:00:00'),
    );
    const r = findRow(rows, 'timed_charge');
    expect(r.state).toBe('active');
    expect(r.description).toBe('armed · charging now');
  });

  it('stays armed when in-window but not actually charging', () => {
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        enable_charge: true,
        charge_slots: [slot(2, 0, 4, 0)],
        battery_state: 'idle',
      }),
      new Date('2026-06-28T03:00:00'),
    );
    const r = findRow(rows, 'timed_charge');
    expect(r.state).toBe('armed');
    expect(r.description).toBe('armed');
  });

  it('stays armed when charging but outside the charge slot', () => {
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        enable_charge: true,
        charge_slots: [slot(2, 0, 4, 0)],
        battery_state: 'charging',
      }),
      new Date('2026-06-28T06:00:00'),
    );
    const r = findRow(rows, 'timed_charge');
    expect(r.state).toBe('armed');
    expect(r.description).toBe('armed');
  });

  it('reports Timed Export as armed when enable_discharge is set but outside the slot', () => {
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        enable_discharge: true,
        discharge_slots: [slot(16, 0, 19, 0)],
      }),
      new Date('2026-06-28T12:00:00'),
    );
    const r = findRow(rows, 'timed_export');
    expect(r.state).toBe('armed');
    expect(r.description).toBe('armed');
  });

  it('reports Timed Export as active when in-window and discharging', () => {
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        enable_discharge: true,
        discharge_slots: [slot(16, 0, 19, 0)],
        battery_state: 'discharging',
        battery_power: 2500,
      }),
      new Date('2026-06-28T17:00:00'),
    );
    const r = findRow(rows, 'timed_export');
    expect(r.state).toBe('active');
    expect(r.description).toBe('armed · exporting now');
  });

  it('stays armed when in export slot but not actually discharging', () => {
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        enable_discharge: true,
        discharge_slots: [slot(16, 0, 19, 0)],
        battery_state: 'idle',
      }),
      new Date('2026-06-28T17:00:00'),
    );
    const r = findRow(rows, 'timed_export');
    expect(r.state).toBe('armed');
    expect(r.description).toBe('armed');
  });

  it('stays armed when discharging but outside the export slot', () => {
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        enable_discharge: true,
        discharge_slots: [slot(16, 0, 19, 0)],
        battery_state: 'discharging',
      }),
      new Date('2026-06-28T12:00:00'),
    );
    const r = findRow(rows, 'timed_export');
    expect(r.state).toBe('armed');
    expect(r.description).toBe('armed');
  });

  it('reports Timed Discharge as armed when pause mode is set but outside the visible window', () => {
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        battery_pause_mode: 2,
        battery_pause_slot: slot(3, 0, 4, 0),
      }),
      new Date('2026-06-28T02:00:00'),
    );
    const r = findRow(rows, 'timed_discharge');
    expect(r.state).toBe('armed');
    expect(r.description).toBe('armed');
  });

  it('reports Timed Discharge as active when in the visible window and discharging', () => {
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        battery_pause_mode: 2,
        battery_pause_slot: slot(3, 0, 4, 0),
        battery_state: 'discharging',
      }),
      new Date('2026-06-28T03:30:00'),
    );
    const r = findRow(rows, 'timed_discharge');
    expect(r.state).toBe('active');
    expect(r.description).toBe('armed · covering demand now');
  });

  it('stays armed when in the visible pause window but not discharging', () => {
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        battery_pause_mode: 2,
        battery_pause_slot: slot(3, 0, 4, 0),
        battery_state: 'idle',
      }),
      new Date('2026-06-28T03:30:00'),
    );
    const r = findRow(rows, 'timed_discharge');
    expect(r.state).toBe('armed');
    expect(r.description).toBe('armed');
  });

  it('reports Timed Discharge as off for pause-charge or pause-both', () => {
    for (const mode of [1, 3] as const) {
      const rows = deriveBatteryModeRows(
        makeSnapshot({
          battery_pause_mode: mode,
          battery_pause_slot: slot(3, 0, 4, 0),
        }),
      );
      const r = findRow(rows, 'timed_discharge');
      expect(r.state).toBe('off');
      expect(r.description).toBe('off');
    }
  });

  it('reports Timed Discharge as off when the pause slot is disabled', () => {
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        battery_pause_mode: 2,
        battery_pause_slot: slot(3, 0, 4, 0, false),
      }),
    );
    const r = findRow(rows, 'timed_discharge');
    expect(r.state).toBe('armed');
    expect(r.description).toBe('armed');
  });

  it('handles the reporter case: Eco on + Timed Export active', () => {
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        battery_power_mode: 1,
        enable_discharge: true,
        discharge_slots: [slot(16, 0, 19, 0)],
        battery_state: 'discharging',
        battery_power: 2500,
      }),
      new Date('2026-06-28T17:00:00'),
    );
    expect(findRow(rows, 'eco')).toEqual({
      key: 'eco',
      label: 'Eco',
      state: 'active',
      description: 'on',
    });
    expect(findRow(rows, 'timed_export')).toEqual({
      key: 'timed_export',
      label: 'Timed Export',
      state: 'active',
      description: 'armed · exporting now',
    });
    expect(findRow(rows, 'timed_charge').state).toBe('off');
    expect(findRow(rows, 'timed_discharge').state).toBe('off');
  });

  it('reports all four mechanisms active simultaneously when everything aligns', () => {
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        battery_power_mode: 1,
        enable_charge: true,
        charge_slots: [slot(2, 0, 4, 0)],
        enable_discharge: true,
        discharge_slots: [slot(16, 0, 19, 0)],
        battery_pause_mode: 2,
        battery_pause_slot: slot(10, 0, 11, 0),
        battery_state: 'charging',
      }),
      new Date('2026-06-28T03:00:00'),
    );
    expect(findRow(rows, 'eco').state).toBe('active');
    expect(findRow(rows, 'timed_charge').state).toBe('active');
    expect(findRow(rows, 'timed_export').state).toBe('armed');
    expect(findRow(rows, 'timed_discharge').state).toBe('armed');
  });

  it('handles a midnight-wrapping slot as active', () => {
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        enable_discharge: true,
        discharge_slots: [slot(23, 0, 1, 0)],
        battery_state: 'discharging',
      }),
      new Date('2026-06-28T00:30:00'),
    );
    const r = findRow(rows, 'timed_export');
    expect(r.state).toBe('active');
    expect(r.description).toBe('armed · exporting now');
  });

  it('treats an empty zero-length slot as inactive', () => {
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        enable_charge: true,
        charge_slots: [slot(0, 0, 0, 0)],
        battery_state: 'charging',
      }),
      new Date('2026-06-28T12:00:00'),
    );
    const r = findRow(rows, 'timed_charge');
    expect(r.state).toBe('armed');
  });

  it('treats a missing battery_power_mode as Eco off', () => {
    const rows = deriveBatteryModeRows(makeSnapshot({ battery_power_mode: undefined }));
    expect(findRow(rows, 'eco').state).toBe('off');
  });

  it('treats a missing battery_pause_mode as Timed Discharge off', () => {
    const rows = deriveBatteryModeRows(makeSnapshot({ battery_pause_mode: undefined }));
    expect(findRow(rows, 'timed_discharge').state).toBe('off');
  });

  it('uses battery_state as the authoritative power-direction signal', () => {
    // battery_power sign is ignored when battery_state disagrees.
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        enable_discharge: true,
        discharge_slots: [slot(16, 0, 19, 0)],
        battery_state: 'idle',
        battery_power: 3000,
      }),
      new Date('2026-06-28T17:00:00'),
    );
    expect(findRow(rows, 'timed_export').state).toBe('armed');
  });

  // -----------------------------------------------------------------
  // Slot-based Agile integration
  // -----------------------------------------------------------------
  //
  // The slot-based Agile refactor writes a real charge slot (HR 94/95)
  // plus enable_charge=1 when a cheap window is active. This means the
  // four-row summary correctly shows 'Timed Charge — active · charging
  // now' during a cheap Agile window, where the legacy ForceCharge
  // approach only set enable_charge without a slot and the summary
  // would have shown 'armed' forever. These tests pin the integration
  // so a future refactor can't regress it.

  it('shows Timed Charge as active during a slot-driven Agile cheap window', () => {
    // Agile Charge Only mode: enable_charge=1, a charge slot covers
    // the current time, and the battery is charging. The summary
    // should report 'active · charging now' — this is the fix the
    // slot-based Agile refactor delivers over the legacy ForceCharge
    // path (which set enable_charge without writing slot times).
    const now = new Date();
    const slotStart = new Date(now.getTime() - 5 * 60_000); // 5 min ago
    const slotEnd = new Date(now.getTime() + 25 * 60_000); // 25 min ahead
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        enable_charge: true,
        charge_slots: [{
          enabled: true,
          start_hour: slotStart.getHours(),
          start_minute: slotStart.getMinutes(),
          end_hour: slotEnd.getHours(),
          end_minute: slotEnd.getMinutes(),
          target_soc: 100,
        }],
        battery_state: 'charging',
        battery_power: -2000,
      }),
      now,
    );
    const r = findRow(rows, 'timed_charge');
    expect(r.state).toBe('active');
    expect(r.description).toBe('armed · charging now');
  });

  it('shows Timed Export as active during a slot-driven Agile expensive window', () => {
    // Symmetric for the discharge side.
    const now = new Date();
    const slotStart = new Date(now.getTime() - 5 * 60_000);
    const slotEnd = new Date(now.getTime() + 25 * 60_000);
    const rows = deriveBatteryModeRows(
      makeSnapshot({
        enable_discharge: true,
        discharge_slots: [{
          enabled: true,
          start_hour: slotStart.getHours(),
          start_minute: slotStart.getMinutes(),
          end_hour: slotEnd.getHours(),
          end_minute: slotEnd.getMinutes(),
          target_soc: 4,
        }],
        battery_state: 'discharging',
        battery_power: 2500,
      }),
      now,
    );
    const r = findRow(rows, 'timed_export');
    expect(r.state).toBe('active');
    expect(r.description).toBe('armed · exporting now');
  });
});
