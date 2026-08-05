import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, within } from '@testing-library/react';

// ControlPage fires several apiGet calls on mount. Stub the side-effecting
// ones so the page mounts cleanly; the Adaptive Charge section additionally
// fetches /api/adaptive-charge, whose config we control per test.
vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/adaptive-charge') {
      return {
        ok: true,
        data: {
          config: {
            periods: [{
              ...DEFAULT_ADAPTIVE_PERIOD,
              enabled: true,
              low_soc: 58,
              recovery_soc: 70,
            }],
            confirmation_readings: 2,
          },
        },
      };
    }
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
        data: { import_tariff: 0.285, export_tariff: 0.15, import_tariff_config: null },
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

import { DEFAULT_ADAPTIVE_PERIOD } from '../../src/lib/adaptiveCharge';
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
    soc: 51,
    battery_voltage: 50,
    battery_current: 0,
    battery_state: 'discharging',
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
    battery_reserve: 4,
    charge_rate: 0,
    discharge_rate: 50,
    active_power_rate: 100,
    max_battery_power_w: 3600,
    max_ac_power_w: 3600,
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
    agile_scope: 'off',
    adaptive_charge_enabled: true,
    max_charge_slots: 2,
    max_discharge_slots: 2,
    charge_slots: [emptySlot(), emptySlot()],
    discharge_slots: [emptySlot(), emptySlot()],
    meters: [],
    inverter_serial: 'FA2311F152',
    firmware_version: '318',
    dsp_firmware_version: '318',
    dc_dsp_firmware_version: '',
    device_type: 'gen3',
    device_type_display: 'Gen 3 Hybrid',
    device_type_code: '2003',
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

describe('<ControlPage/> — Adaptive Charge Low SOC caption', () => {
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
    vi.useRealTimers();
    vi.unstubAllGlobals();
    cleanup();
    useInverterStore.setState({ snapshot: null, connectionState: 'disconnected' });
  });

  async function adaptiveSection() {
    // Adaptive Charge renders a "Period 1" label (a <span>, not a heading)
    // when a period is enabled. Wait for the fetched config to land.
    const periodLabel = await screen.findByText('Period 1');
    const card = periodLabel.closest('div.bg-bg-surface');
    if (!card) throw new Error('Period card not found');
    return card;
  }

  it('renders the charge-rate-only caveat under the Low SOC field (issue #256)', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot(),
      developerMode: false,
      connectionState: 'connected',
    });
    render(<ControlPage />);

    const card = await adaptiveSection();
    // The Low SOC label is present and shows the user's 58% value.
    expect(within(card).getByText('Low SOC')).toBeDefined();
    expect(within(card).getByText('58%')).toBeDefined();
    // The caption steers the user away from treating Low SOC as a discharge floor.
    expect(within(card).getByText(/does not stop discharge/i)).toBeDefined();
    expect(within(card).getByText(/discharge cutoff soc/i)).toBeDefined();
  });
});
