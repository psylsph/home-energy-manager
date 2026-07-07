import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, within } from '@testing-library/react';

vi.mock('../../src/components/AwaitingConnection', () => ({
  default: () => <div data-testid="awaiting" />,
}));
vi.mock('../../src/components/SolarPowerChart', () => ({
  default: () => <div data-testid="solar-power-chart" />,
}));

import SolarPage from '../../src/pages/SolarPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot, SolarArraySummary } from '../../src/lib/types';

function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    timestamp: 0,
    solar_power: 0, pv1_power: 0, pv2_power: 0,
    pv1_voltage: 0, pv2_voltage: 0, pv1_current: 0, pv2_current: 0,
    battery_power: 0, soc: 50, battery_voltage: 50, battery_current: 0,
    battery_state: 'idle', battery_temperature: 20, battery_capacity_kwh: 9.5,
    eps_power_w: 0, grid_power: 0, grid_voltage: 230, grid_frequency: 50,
    grid_online: true, grid_loss: false, inverter_trip: false,
    battery_over_temp: false, home_power: 0, inverter_temperature: 30,
    inverter_time: '',
    today_solar_kwh: 0, today_pv1_kwh: 0, today_pv2_kwh: 0,
    today_import_kwh: 0, today_export_kwh: 0, today_charge_kwh: 0,
    total_import_kwh: 0, total_export_kwh: 0, total_solar_kwh: 0,
    total_charge_kwh: 0, total_discharge_kwh: 0, total_throughput_kwh: 0,
    operating_hours: 0, today_discharge_kwh: 0, today_consumption_kwh: 0,
    home_energy_today_kwh: 0, battery_modules: [], battery_mode: 'eco',
    battery_reserve: 4, charge_rate: 0, discharge_rate: 0, active_power_rate: 0,
    max_battery_power_w: 0, max_ac_power_w: 0, export_limit_w: 0, target_soc: 100,
    enable_charge_target: false, enable_charge: false, enable_discharge: false,
    auto_winter_active: false, load_limiter_active: false, cosy_active: false,
    cosy_enabled: false, agile_active: false, agile_state: 'idle', agile_enabled: false,
    max_charge_slots: 0, max_discharge_slots: 0, charge_slots: [], discharge_slots: [],
    meters: [], inverter_serial: '', firmware_version: '', dsp_firmware_version: '',
    dc_dsp_firmware_version: '', device_type: '', device_type_display: 'Gen 3 Hybrid',
    device_type_code: '2201', battery_calibration_stage: 0, enable_ammeter: false,
    enable_reversed_ct_clamp: false, meter_type: 0, supports_battery_calibration: false,
    ac_eps_enabled: false, ac_export_priority: 0,
    ...overrides,
  };
}

function resetStore() {
  useInverterStore.setState({
    snapshot: null,
    connectionState: 'disconnected',
    panelGraphsEnabled: true,
  });
}

beforeEach(() => {
  cleanup();
  resetStore();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('SolarPage — solar arrays "% of max" section (issue #110)', () => {
  it('hides the section entirely when the snapshot has no solar_arrays', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ solar_arrays: [] }),
      connectionState: 'connected',
    });
    render(<SolarPage />);
    expect(screen.queryByTestId('solar-arrays')).toBeNull();
  });

  it('hides the section when solar_arrays is omitted from the snapshot', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot(),
      connectionState: 'connected',
    });
    render(<SolarPage />);
    expect(screen.queryByTestId('solar-arrays')).toBeNull();
  });

  it('renders PV1 / PV2 arrays with their live power, % of max, and today energy', () => {
    const arrays: SolarArraySummary[] = [
      {
        source: 'pv1', name: '', power_w: 4200, rated_kw: 6,
        today_kwh: 18.5, meter_address: null,
      },
      {
        source: 'pv2', name: '', power_w: 2100, rated_kw: 4.2,
        today_kwh: 7.5, meter_address: null,
      },
    ];
    useInverterStore.setState({
      snapshot: makeSnapshot({ solar_arrays: arrays }),
      connectionState: 'connected',
    });
    render(<SolarPage />);
    const section = screen.getByTestId('solar-arrays');
    // Both arrays are rendered. PV1: 4.2 kW of 6 kWp = 70%. PV2: 2.1 of 4.2 = 50%.
    expect(within(section).getByText('PV1')).toBeDefined();
    expect(within(section).getByText('PV2')).toBeDefined();
    expect(within(section).getByText('70% of max')).toBeDefined();
    expect(within(section).getByText('50% of max')).toBeDefined();
    // Rated kWp labels are shown so the user sees the denominator.
    expect(within(section).getByText('6 kWp')).toBeDefined();
    expect(within(section).getByText('4.2 kWp')).toBeDefined();
    // DC strings carry today's energy.
    expect(within(section).getByText(/Today: 18\.5kWh/)).toBeDefined();
    expect(within(section).getByText(/Today: 7\.5kWh/)).toBeDefined();
    // Progress bar aria-valuenow reflects the (rounded) percent.
    const bars = within(section).getAllByRole('progressbar');
    expect(bars).toHaveLength(2);
    expect(bars[0].getAttribute('aria-valuenow')).toBe('70');
    expect(bars[1].getAttribute('aria-valuenow')).toBe('50');
  });

  it('uses the user-entered name when provided and falls back to a default otherwise', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({
        solar_arrays: [
          // Named CT-meter array (AC-coupled).
          { source: 'meter', name: 'East roof', power_w: 3200, rated_kw: 6,
            today_kwh: null, meter_address: 1 },
          // Unnamed CT-meter array → falls back to its hex address.
          { source: 'meter', name: '', power_w: 2100, rated_kw: 4.2,
            today_kwh: null, meter_address: 2 },
        ],
      }),
      connectionState: 'connected',
    });
    render(<SolarPage />);
    const section = screen.getByTestId('solar-arrays');
    expect(within(section).getByText('East roof')).toBeDefined();
    expect(within(section).getByText('Meter 0x02')).toBeDefined();
  });

  it('hides the progress bar and "of max" label when no rated capacity is configured', () => {
    // A CT-meter entry with rated_kw = 0 still surfaces the power, but
    // the % bar and "of max" copy are suppressed (no denominator).
    useInverterStore.setState({
      snapshot: makeSnapshot({
        solar_arrays: [
          { source: 'meter', name: 'Garage', power_w: 1500, rated_kw: 0,
            today_kwh: null, meter_address: 3 },
        ],
      }),
      connectionState: 'connected',
    });
    render(<SolarPage />);
    const section = screen.getByTestId('solar-arrays');
    expect(within(section).getByText('Garage')).toBeDefined();
    expect(within(section).queryByText(/of max/)).toBeNull();
    expect(within(section).queryByRole('progressbar')).toBeNull();
    // CT meters have no per-day counter, so "Today: …" is absent.
    expect(within(section).queryByText(/Today:/)).toBeNull();
  });

  it('caps the visual bar at 100% when generation exceeds nameplate', () => {
    // 6.5 kW from a 6 kWp array (108%) — the numeric % still shows the
    // real value but the bar is clamped so the track doesn't overflow.
    useInverterStore.setState({
      snapshot: makeSnapshot({
        solar_arrays: [
          { source: 'pv1', name: '', power_w: 6500, rated_kw: 6,
            today_kwh: 25, meter_address: null },
        ],
      }),
      connectionState: 'connected',
    });
    render(<SolarPage />);
    const section = screen.getByTestId('solar-arrays');
    const bar = within(section).getByRole('progressbar');
    // Aria value reflects the rounded REAL percent, not the clamped fill.
    expect(bar.getAttribute('aria-valuenow')).toBe('108');
    // But the rendered width is clamped at 100%.
    const fill = bar.firstElementChild as HTMLElement;
    expect(fill.style.width).toBe('100%');
  });

  it('shows power in kW alongside the %', () => {
    // Both kW value and % are shown together in each card.
    useInverterStore.setState({
      snapshot: makeSnapshot({
        solar_arrays: [
          { source: 'pv1', name: '', power_w: 4200, rated_kw: 6,
            today_kwh: 18.5, meter_address: null },
        ],
      }),
      connectionState: 'connected',
    });
    render(<SolarPage />);
    const section = screen.getByTestId('solar-arrays');
    // kW label is present (e.g. "4.2kW" — no space, per formatPower).
    expect(within(section).getByText('4.2kW')).toBeDefined();
  });

  it('shows the rated kWp label for each array', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({
        solar_arrays: [
          { source: 'pv1', name: '', power_w: 2000, rated_kw: 6,
            today_kwh: 10, meter_address: null },
        ],
      }),
      connectionState: 'connected',
    });
    render(<SolarPage />);
    const section = screen.getByTestId('solar-arrays');
    // Rated denominator is shown so the user can verify the %.
    expect(within(section).getByText('6 kWp')).toBeDefined();
  });

  it('CT meter array shows power without today_kwh or % bar when rated_kw is 0', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({
        solar_arrays: [
          { source: 'meter', name: 'AC panel', power_w: 2500, rated_kw: 0,
            today_kwh: null, meter_address: 4 },
        ],
      }),
      connectionState: 'connected',
    });
    render(<SolarPage />);
    const section = screen.getByTestId('solar-arrays');
    // Power is shown.
    expect(within(section).getByText('AC panel')).toBeDefined();
    // No "of max" since rated is 0.
    expect(within(section).queryByText(/of max/)).toBeNull();
    // No progress bar without a denominator.
    expect(within(section).queryByRole('progressbar')).toBeNull();
    // CT meters have no per-day counter, so no "Today:" label.
    expect(within(section).queryByText(/Today:/)).toBeNull();
  });
});
