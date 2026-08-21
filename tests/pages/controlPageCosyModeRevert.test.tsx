/**
 * Tests for the Cosy Charging Mode failure-revert race on ControlPage.
 *
 * `CosyChargingSection.handleModeChange` optimistically flips the dropdown
 * and reverts via `onModeChange(mode)` on API failure, where `mode` is the
 * prop captured at render time. If the user selects another mode while the
 * first request is still in flight, the stale closure would revert the
 * dropdown to the pre-first-change mode, clobbering the newer selection.
 *
 * The fix tracks the latest requested mode and only reverts when the user
 * hasn't chosen a newer mode since the failing request started.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, act } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/adaptive-charge') {
      return {
        ok: true,
        data: {
          config: {
            periods: [
              {
                enabled: false,
                start_hour: 0,
                start_minute: 0,
                end_hour: 0,
                end_minute: 0,
                target_soc: 100,
                low_soc: 20,
                recovery_soc: 30,
                min_charge_rate: 50,
                days: [false, false, false, false, false, false, false],
              },
            ],
            confirmation_readings: 2,
          },
        },
      };
    }
    if (path === '/api/agile') return { ok: true, enabled: false, scope: 'off' };
    if (path === '/api/auto-winter')
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
    if (path === '/api/cosy')
      return {
        ok: true,
        enabled: false,
        slots: Array.from({ length: 3 }, () => ({
          enabled: false,
          start_hour: 0,
          start_minute: 0,
          end_hour: 0,
          end_minute: 0,
          target_soc: 100,
        })),
      };
    if (path === '/api/settings')
      return {
        ok: true,
        data: { import_tariff: 0.285, export_tariff: 0.15, import_tariff_config: null },
      };
    if (path === '/api/load-limiter')
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
    battery_reserve: 4,
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
    device_type: 'AllInOne6kW',
    device_type_display: 'All-in-One 6kW',
    device_type_code: '8001',
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

describe('<ControlPage/> — Cosy mode failure revert', () => {
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
    useInverterStore.setState({ snapshot: null, connectionState: 'disconnected' });
  });

  it('does not revert to the pre-change mode when a newer selection follows a failing request', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ cosy_enabled: false }),
      developerMode: false,
      connectionState: 'connected',
    });

    // First /api/charging-mode POST hangs until we reject it, so a second
    // selection can race in before the failure handler runs.
    let rejectFirst: ((err: Error) => void) | null = null;
    vi.mocked(apiPost).mockImplementation(async (path: string) => {
      if (path === '/api/charging-mode') {
        if (!rejectFirst) {
          return new Promise<never>((_resolve, reject) => {
            rejectFirst = reject;
          });
        }
        return { ok: true };
      }
      return { ok: true, data: {} };
    });

    render(<ControlPage />);

    const select = (await screen.findByRole('combobox')) as HTMLSelectElement;
    expect(select.value).toBe('standard');

    // 1) User picks Cosy — request in flight, dropdown optimistically flips.
    await act(async () => {
      fireEvent.change(select, { target: { value: 'cosy' } });
    });
    expect(select.value).toBe('cosy');

    // 2) User picks Adaptive while the first request is still pending.
    await act(async () => {
      fireEvent.change(select, { target: { value: 'adaptive' } });
    });
    expect(select.value).toBe('adaptive');

    // 3) The first request now fails. The stale failure handler must NOT
    // clobber the newer selection back to the pre-first-change 'standard'.
    rejectFirst?.(new Error('boom'));
    await act(async () => {});
    await act(async () => {});

    expect(select.value).toBe('adaptive');
  });

  it('still reverts to the persisted mode when a failing request is the latest selection', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ cosy_enabled: false }),
      developerMode: false,
      connectionState: 'connected',
    });

    vi.mocked(apiPost).mockImplementation(async (path: string) => {
      if (path === '/api/charging-mode') throw new Error('boom');
      return { ok: true, data: {} };
    });

    render(<ControlPage />);

    const select = (await screen.findByRole('combobox')) as HTMLSelectElement;
    expect(select.value).toBe('standard');

    await act(async () => {
      fireEvent.change(select, { target: { value: 'cosy' } });
    });

    // No newer selection: the failure must revert to 'standard'.
    expect(select.value).toBe('standard');
  });
});
