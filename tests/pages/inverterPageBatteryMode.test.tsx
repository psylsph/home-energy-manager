/**
 * Coverage for the Inverter page's new four-row battery-mechanism summary.
 *
 * The old single "Battery Mode" cell (which showed a derived label like
 * "Timed Demand") has been replaced with a "Modes" block listing Eco,
 * Timed Charge, Timed Export and Timed Discharge independently, each with an
 * off / armed / active state.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, within, cleanup } from '@testing-library/react';
import InverterPage from '../../src/pages/InverterPage';
import { useInverterStore } from '../../src/store/useInverterStore';
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

beforeEach(() => {
  cleanup();
  vi.useFakeTimers();
  vi.setSystemTime(new Date('2026-06-28T12:00:00'));
  useInverterStore.setState({ snapshot: null, connectionState: 'connected' });
});

afterEach(() => {
  vi.useRealTimers();
});

describe('<InverterPage/> battery mechanism summary', () => {
  it('replaces the old "Battery Mode" row with a "Modes" block', () => {
    useInverterStore.setState({ snapshot: makeSnapshot() });
    render(<InverterPage />);

    expect(screen.queryByText('Battery Mode')).toBeNull();
    expect(screen.getByText('Modes')).toBeDefined();
  });

  it('renders all four mechanism labels', () => {
    useInverterStore.setState({ snapshot: makeSnapshot() });
    render(<InverterPage />);

    expect(screen.getByText('Eco')).toBeDefined();
    expect(screen.getByText('Timed Charge')).toBeDefined();
    expect(screen.getByText('Timed Export')).toBeDefined();
    expect(screen.getByText('Timed Discharge')).toBeDefined();
  });

  it('renders every mechanism as off when all registers are clear', () => {
    useInverterStore.setState({ snapshot: makeSnapshot() });
    render(<InverterPage />);

    for (const key of ['eco', 'timed_charge', 'timed_export', 'timed_discharge']) {
      const row = screen.getByTestId(`battery-mode-${key}`);
      expect(row.getAttribute('data-state')).toBe('off');
      expect(within(row).getByTestId(`battery-mode-desc-${key}`).textContent).toBe('off');
    }
  });

  it('shows Eco as on when battery_power_mode is 1', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ battery_power_mode: 1 }),
    });
    render(<InverterPage />);

    const row = screen.getByTestId('battery-mode-eco');
    expect(row.getAttribute('data-state')).toBe('active');
    expect(within(row).getByTestId('battery-mode-desc-eco').textContent).toBe('on');
  });

  it('shows Timed Charge as active when armed, in-window and charging', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({
        enable_charge: true,
        charge_slots: [slot(2, 0, 4, 0)],
        battery_state: 'charging',
      }),
    });
    vi.setSystemTime(new Date('2026-06-28T03:00:00'));
    render(<InverterPage />);

    const row = screen.getByTestId('battery-mode-timed_charge');
    expect(row.getAttribute('data-state')).toBe('active');
    expect(within(row).getByTestId('battery-mode-desc-timed_charge').textContent).toBe(
      'armed · charging now',
    );
  });

  it('shows Timed Export as active when armed, in-window and discharging', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({
        enable_discharge: true,
        discharge_slots: [slot(16, 0, 19, 0)],
        battery_state: 'discharging',
      }),
    });
    vi.setSystemTime(new Date('2026-06-28T17:00:00'));
    render(<InverterPage />);

    const row = screen.getByTestId('battery-mode-timed_export');
    expect(row.getAttribute('data-state')).toBe('active');
    expect(within(row).getByTestId('battery-mode-desc-timed_export').textContent).toBe(
      'armed · exporting now',
    );
  });

  it('shows Timed Discharge as active when armed, in-window and discharging', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({
        battery_pause_mode: 2,
        battery_pause_slot: slot(3, 0, 4, 0),
        battery_state: 'discharging',
      }),
    });
    vi.setSystemTime(new Date('2026-06-28T03:30:00'));
    render(<InverterPage />);

    const row = screen.getByTestId('battery-mode-timed_discharge');
    expect(row.getAttribute('data-state')).toBe('active');
    expect(within(row).getByTestId('battery-mode-desc-timed_discharge').textContent).toBe(
      'armed · covering demand now',
    );
  });

  it('keeps armed mechanisms as armed when outside their active window', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({
        enable_charge: true,
        charge_slots: [slot(2, 0, 4, 0)],
        enable_discharge: true,
        discharge_slots: [slot(16, 0, 19, 0)],
      }),
    });
    vi.setSystemTime(new Date('2026-06-28T12:00:00'));
    render(<InverterPage />);

    expect(screen.getByTestId('battery-mode-timed_charge').getAttribute('data-state')).toBe('armed');
    expect(screen.getByTestId('battery-mode-timed_export').getAttribute('data-state')).toBe('armed');
    expect(
      within(screen.getByTestId('battery-mode-timed_charge')).getByTestId(
        'battery-mode-desc-timed_charge',
      ).textContent,
    ).toBe('armed');
    expect(
      within(screen.getByTestId('battery-mode-timed_export')).getByTestId(
        'battery-mode-desc-timed_export',
      ).textContent,
    ).toBe('armed');
  });

  it('renders the reporter case: Eco on + Timed Export active', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({
        battery_power_mode: 1,
        enable_discharge: true,
        discharge_slots: [slot(16, 0, 19, 0)],
        battery_state: 'discharging',
      }),
    });
    vi.setSystemTime(new Date('2026-06-28T17:00:00'));
    render(<InverterPage />);

    expect(screen.getByTestId('battery-mode-eco').getAttribute('data-state')).toBe('active');
    expect(screen.getByTestId('battery-mode-timed_export').getAttribute('data-state')).toBe('active');
    expect(
      within(screen.getByTestId('battery-mode-timed_export')).getByTestId(
        'battery-mode-desc-timed_export',
      ).textContent,
    ).toBe('armed · exporting now');

    expect(screen.getByTestId('battery-mode-timed_charge').getAttribute('data-state')).toBe('off');
    expect(screen.getByTestId('battery-mode-timed_discharge').getAttribute('data-state')).toBe('off');
  });

  it('renders the reporter case without mislabelling it as Timed Demand', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({
        battery_power_mode: 1,
        enable_discharge: true,
        discharge_slots: [slot(16, 0, 19, 0)],
        battery_state: 'discharging',
      }),
    });
    vi.setSystemTime(new Date('2026-06-28T17:00:00'));
    render(<InverterPage />);

    expect(screen.queryByText('Timed Demand')).toBeNull();
    expect(screen.queryByText('Timed Discharge')).toBeDefined();
  });
});
