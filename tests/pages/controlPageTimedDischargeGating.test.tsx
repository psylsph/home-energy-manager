import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, within } from '@testing-library/react';

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
          full_power_discharge_in_eco_mode: false,
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
    discharge_slots: [emptySlot(), emptySlot()],
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

describe('<ControlPage/> — Timed Discharge device gating', () => {
  // The pause registers (HR318-320) only exist in the HR 300-359 block, which
  // is exclusive to AC-coupled (3001/3002), AC three-phase (60xx) and
  // residential All-in-One (80xx). On every other family both the Quick
  // Action button (Battery Mode section) and the dedicated schedule section
  // (heading "Timed Discharge") must be hidden, and no /api/control/
  // timed-discharge call should be possible from the UI.

  beforeEach(() => {
    silenceConsoleError();
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
    useInverterStore.setState({ snapshot: null });
  });

  async function batteryModeSection() {
    const heading = await screen.findByRole('heading', { name: 'Battery Mode', exact: true });
    const section = heading.closest('section');
    if (!section) throw new Error('Battery Mode heading has no <section> ancestor');
    return section;
  }

  describe('hidden on devices without the HR 300-359 block', () => {
    it.each([
      ['1001', 'Gen1 hybrid (reported case)'],
      ['2001', 'Gen hybrid (pre-ARM-refined)'],
      ['4001', 'Three-phase'],
      ['7001', 'Gateway'],
      ['8101', 'Hybrid HV Gen3'],
      ['8301', 'Gen4 hybrid'],
      ['2301', 'PV inverter'],
    ])('hides the button and section for %s (%s)', async (code) => {
      useInverterStore.setState({
        snapshot: makeSnapshot({ device_type_code: code }),
        developerMode: false,
      });
      render(<ControlPage />);

      // Quick Action button is absent from the Battery Mode section.
      const section = await batteryModeSection();
      expect(within(section).queryByText('Timed Discharge')).toBeNull();

      // Dedicated schedule section (its own heading) is also absent.
      expect(screen.queryByRole('heading', { name: 'Timed Discharge' })).toBeNull();

      // The three always-available mechanisms still render, proving we hid
      // only Timed Discharge and not the whole section.
      expect(within(section).getByText('Eco')).toBeDefined();
      expect(within(section).getByText('Timed Charge')).toBeDefined();
      expect(within(section).getByText('Timed Export')).toBeDefined();
    });
  });

  describe('shown on devices with the HR 300-359 block', () => {
    it.each([
      ['3001', 'AC-coupled'],
      ['3002', 'AC-coupled Mk2'],
      ['8001', 'AIO 6kW'],
      ['80FF', 'AIO family'],
    ])('shows the button and section for %s (%s)', async (code) => {
      useInverterStore.setState({
        snapshot: makeSnapshot({ device_type_code: code }),
        developerMode: false,
      });
      render(<ControlPage />);

      const section = await batteryModeSection();
      expect(within(section).getByText('Timed Discharge')).toBeDefined();
      expect(screen.getByRole('heading', { name: 'Timed Discharge' })).toBeDefined();
    });
  });

  it('stays hidden before the first snapshot arrives', async () => {
    useInverterStore.setState({ snapshot: null, developerMode: false });
    render(<ControlPage />);

    await screen.findByRole('heading', { name: 'Battery Mode' });
    expect(screen.queryByText('Timed Discharge')).toBeNull();
    expect(screen.queryByRole('heading', { name: 'Timed Discharge' })).toBeNull();
  });
});
