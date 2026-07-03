import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, within } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/agile') return { ok: true, enabled: false };
    if (path === '/api/auto-winter') {
      return {
        ok: true,
        data: {
          config: {
            enabled: false,
            cold_threshold: 8,
            recovery_threshold: 12,
            target_soc: 80,
            debounce_readings: 10,
          },
        },
      };
    }
    if (path === '/api/cosy') return { ok: true, enabled: false, slots: [] };
    if (path === '/api/settings') {
      return {
        ok: true,
        data: {
          import_tariff: 0.285,
          export_tariff: 0.15,
          import_tariff_config: null,
        },
      };
    }
    if (path === '/api/load-limiter') {
      return {
        ok: true,
        data: {
          config: {
            enabled: false,
            threshold_w: 3000,
            trigger_delay_minutes: 0,
            start_hour: 0,
            start_minute: 0,
            end_hour: 0,
            end_minute: 0,
          },
        },
      };
    }
    return { ok: true, data: {} };
  }),
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  fetchHistory: vi.fn().mockResolvedValue({}),
  isTauri: false,
}));

import ControlPage from '../../src/pages/ControlPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import { apiPost } from '../../src/lib/api';
import type { InverterSnapshot, ScheduleSlot } from '../../src/lib/types';

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

function emptySlot(overrides: Partial<ScheduleSlot> = {}): ScheduleSlot {
  return {
    enabled: false,
    start_hour: 0,
    start_minute: 0,
    end_hour: 0,
    end_minute: 0,
    target_soc: 100,
    ...overrides,
  };
}

function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
  const exportSlot = emptySlot({
    enabled: true,
    start_hour: 17,
    end_hour: 19,
    target_soc: 20,
  });
  return {
    timestamp: Math.floor(Date.now() / 1000),
    solar_power: 0,
    pv1_power: 0,
    pv2_power: 0,
    pv1_voltage: 0,
    pv2_voltage: 0,
    pv1_current: 0,
    pv2_current: 0,
    battery_power: 0,
    soc: 50,
    battery_voltage: 50,
    battery_current: 0,
    battery_state: 'idle',
    battery_temperature: 20,
    battery_capacity_kwh: 9.5,
    eps_power_w: 0,
    grid_power: 0,
    grid_voltage: 240,
    grid_frequency: 50,
    grid_online: true,
    grid_loss: false,
    inverter_trip: false,
    battery_over_temp: false,
    home_power: 0,
    inverter_temperature: 25,
    inverter_time: '',
    today_solar_kwh: 0,
    today_pv1_kwh: 0,
    today_pv2_kwh: 0,
    today_import_kwh: 0,
    today_export_kwh: 0,
    today_charge_kwh: 0,
    total_import_kwh: 0,
    total_export_kwh: 0,
    total_solar_kwh: 0,
    total_charge_kwh: 0,
    total_discharge_kwh: 0,
    total_throughput_kwh: 0,
    operating_hours: 0,
    today_discharge_kwh: 0,
    today_consumption_kwh: 0,
    home_energy_today_kwh: 0,
    battery_modules: [],
    battery_mode: 'eco',
    battery_power_mode: 1,
    battery_reserve: 20,
    charge_rate: 50,
    discharge_rate: 50,
    active_power_rate: 100,
    max_battery_power_w: 5000,
    max_ac_power_w: 5000,
    export_limit_w: 0,
    target_soc: 4,
    enable_charge_target: false,
    enable_charge: false,
    enable_discharge: false,
    auto_winter_active: false,
    load_limiter_active: false,
    cosy_active: false,
    cosy_enabled: false,
    agile_active: false,
    agile_state: 'idle',
    agile_enabled: false,
    max_charge_slots: 2,
    max_discharge_slots: 2,
    charge_slots: [emptySlot(), emptySlot()],
    discharge_slots: [exportSlot, emptySlot()],
    meters: [],
    inverter_serial: 'FD2328G358',
    firmware_version: '318',
    dsp_firmware_version: '318',
    dc_dsp_firmware_version: '',
    device_type: 'gen3',
    device_type_display: 'Gen3',
    device_type_code: '2001',
    battery_calibration_stage: 0,
    enable_ammeter: false,
    enable_reversed_ct_clamp: false,
    meter_type: 0,
    supports_battery_calibration: false,
    ac_eps_enabled: false,
    ac_export_priority: 0,
    battery_pause_mode: 0,
    battery_pause_slot: emptySlot(),
    ...overrides,
  };
}

describe('<ControlPage/> — independent battery mechanisms', () => {
  beforeEach(() => {
    silenceConsoleError();
    vi.mocked(apiPost).mockClear();
    vi.stubGlobal(
      'matchMedia',
      vi.fn().mockImplementation((query: string) => ({
        matches: false,
        media: query,
        onchange: null,
        addListener: vi.fn(),
        removeListener: vi.fn(),
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        dispatchEvent: vi.fn(),
      })),
    );
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    cleanup();
    useInverterStore.setState({ snapshot: null, connectionState: 'disconnected' });
  });

  async function batteryModeSection() {
    const heading = await screen.findByRole('heading', { name: 'Battery Mode', exact: true });
    const section = heading.closest('section');
    if (!section) throw new Error('Battery Mode heading has no <section> ancestor');
    return section;
  }

  it('shows Eco, Timed Charge, Timed Export and Timed Discharge together on a supported device', async () => {
    // The default fixture is a Gen hybrid (2001), which hides Timed Discharge.
    // Render on an All-in-One (8001), where HR318-320 slot writes are supported,
    // so all four independent mechanisms are visible.
    useInverterStore.setState({
      snapshot: makeSnapshot({ device_type_code: '8001' }),
      developerMode: false,
      connectionState: 'connected',
    });
    render(<ControlPage />);

    const section = await batteryModeSection();
    expect(within(section).getByText('Eco')).toBeDefined();
    expect(within(section).getByText('Timed Charge')).toBeDefined();
    expect(within(section).getByText('Timed Export')).toBeDefined();
    expect(within(section).getByText('Timed Discharge')).toBeDefined();
  });

  it('toggles Timed Export through the split endpoint, not the old combined mode endpoint', async () => {
    useInverterStore.setState({ snapshot: makeSnapshot({ enable_discharge: false }), developerMode: false, connectionState: 'connected' });
    render(<ControlPage />);

    const section = await batteryModeSection();
    fireEvent.click(within(section).getByText('Timed Export').closest('button')!);

    expect(vi.mocked(apiPost)).toHaveBeenCalledWith('/api/control/timed-export', { enabled: true });
    expect(vi.mocked(apiPost).mock.calls.some(([path]) => path === '/api/control/mode')).toBe(false);
  });

  it('toggles Timed Charge independently through HR96 endpoint', async () => {
    useInverterStore.setState({ snapshot: makeSnapshot({ enable_charge: false }), developerMode: false, connectionState: 'connected' });
    render(<ControlPage />);

    const section = await batteryModeSection();
    fireEvent.click(within(section).getByText('Timed Charge').closest('button')!);

    expect(vi.mocked(apiPost)).toHaveBeenCalledWith('/api/control/timed-charge', { enabled: true });
  });

  it('saves Timed Discharge as a separate pause-window mechanism', async () => {
    // Timed Discharge requires confirmed HR318-320 slot support; render on
    // All-in-One (8001) where the button is visible. The default Gen-hybrid
    // fixture (2001) hides the control.
    useInverterStore.setState({
      snapshot: makeSnapshot({ device_type_code: '8001', battery_pause_mode: 0 }),
      developerMode: false,
      connectionState: 'connected',
    });
    render(<ControlPage />);

    const section = await batteryModeSection();
    fireEvent.click(within(section).getByText('Timed Discharge').closest('button')!);

    expect(vi.mocked(apiPost)).toHaveBeenCalledWith('/api/control/timed-discharge', {
      enabled: true,
      start_hour: 3,
      start_minute: 0,
      end_hour: 4,
      end_minute: 0,
    });
  });
});
