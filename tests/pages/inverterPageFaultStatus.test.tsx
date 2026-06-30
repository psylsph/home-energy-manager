/**
 * Coverage for the InverterPage "Fault Status" section (GitHub issue #174).
 *
 * The boolean fault signals (grid_loss / inverter_trip / battery_over_temp)
 * are decoded for every device type but were previously surfaced only in the
 * global alert banner (App.tsx), never on the InverterPage itself. This
 * section renders them for ALL device types — including Gateway, whose own
 * bitmask block (gateway_fault_codes, IR 1622-1623) is an *independent*
 * dataset and does not reflect a grid-loss outage. On the Gateway decode path
 * `grid_loss` is never populated, so the signal arrives as `grid_online=false`;
 * `hasGridFault()` ORs the two so the Gateway is covered too (this is the exact
 * scenario the banner already relied on).
 */
import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import InverterPage from '../../src/pages/InverterPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

// Reuses the same field set as tests/pages/inverterPage.test.tsx so the page
// renders without format/NaN noise; adds the four fault booleans under test.
function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    // Fault booleans — the focus of this section
    grid_loss: false,
    grid_online: true,
    inverter_trip: false,
    battery_over_temp: false,
    soc: 50,
    battery_state: 'idle',
    battery_power: 0,
    battery_voltage: 51.2,
    battery_current: -7.8,
    battery_temperature: 20,
    battery_capacity_kwh: 9.5,
    battery_mode: 'eco',
    cosy_active: false,
    cosy_enabled: false,
    enable_charge: false,
    enable_discharge: false,
    charge_slots: [],
    discharge_slots: [],
    device_type: '',
    device_type_display: 'Gen 3 Hybrid',
    device_type_code: '2201',
    firmware_version: '',
    dsp_firmware_version: '',
    dc_dsp_firmware_version: '',
    inverter_serial: '',
    inverter_time: '',
    max_battery_power_w: 0,
    max_ac_power_w: 0,
    export_limit_w: 0,
    operating_hours: 0,
    battery_modules: [],
    battery_reserve: 4,
    charge_rate: 0,
    discharge_rate: 0,
    enable_charge_target: false,
    target_soc: 100,
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
  useInverterStore.setState({ snapshot: null, connectionState: 'connected' });
});

describe('<InverterPage/> Fault Status section', () => {
  it('renders the section heading for a normal snapshot', () => {
    useInverterStore.setState({ snapshot: makeSnapshot() });
    render(<InverterPage />);
    expect(screen.getByText('Fault Status')).toBeTruthy();
  });

  it('shows "✓ No faults" when all fault booleans are clear (non-Gateway)', () => {
    useInverterStore.setState({ snapshot: makeSnapshot() });
    render(<InverterPage />);
    expect(screen.getByText('✓ No faults')).toBeTruthy();
    expect(screen.queryByText('Grid power lost')).toBeNull();
    expect(screen.queryByText('Inverter trip detected')).toBeNull();
    expect(screen.queryByText('Battery over temperature')).toBeNull();
  });

  it('shows the grid fault when grid_loss is true', () => {
    useInverterStore.setState({ snapshot: makeSnapshot({ grid_loss: true }) });
    render(<InverterPage />);
    expect(screen.getByText('Grid power lost')).toBeTruthy();
    expect(screen.queryByText('✓ No faults')).toBeNull();
  });

  it('shows the grid fault via !grid_online even when grid_loss is false', () => {
    // Mirrors the Gateway decode path, where grid_loss is never populated and
    // the signal arrives solely as grid_online=false (what the banner already
    // relied on via hasGridFault()'s OR). Verifies the section uses the same
    // helper rather than reading grid_loss directly.
    useInverterStore.setState({ snapshot: makeSnapshot({ grid_online: false }) });
    render(<InverterPage />);
    expect(screen.getByText('Grid power lost')).toBeTruthy();
  });

  it('shows an inverter trip when inverter_trip is true', () => {
    useInverterStore.setState({ snapshot: makeSnapshot({ inverter_trip: true }) });
    render(<InverterPage />);
    expect(screen.getByText('Inverter trip detected')).toBeTruthy();
  });

  it('shows battery over-temperature when battery_over_temp is true', () => {
    useInverterStore.setState({ snapshot: makeSnapshot({ battery_over_temp: true }) });
    render(<InverterPage />);
    expect(screen.getByText('Battery over temperature')).toBeTruthy();
  });

  it('lists every active fault when multiple are set', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({
        grid_loss: true,
        inverter_trip: true,
        battery_over_temp: true,
      }),
    });
    render(<InverterPage />);
    expect(screen.getByText('Grid power lost')).toBeTruthy();
    expect(screen.getByText('Inverter trip detected')).toBeTruthy();
    expect(screen.getByText('Battery over temperature')).toBeTruthy();
    expect(screen.queryByText('✓ No faults')).toBeNull();
  });

  it('renders the section for a Gateway device and shows its grid fault', () => {
    // Regression for the issue #174 finding: a Gateway grid outage flips
    // grid_online while leaving the gateway bitmask (gateway_fault_codes)
    // empty, so the Gateway section's own "✓ No faults" previously hid the
    // fault entirely from this page. The Fault Status section must surface it
    // regardless of device type.
    useInverterStore.setState({
      snapshot: makeSnapshot({ device_type_code: '7001', grid_online: false }),
    });
    render(<InverterPage />);
    expect(screen.getByText('Fault Status')).toBeTruthy();
    expect(screen.getByText('Grid power lost')).toBeTruthy();
  });
});
