/**
 * Precision coverage for the Agile half-hour price card on ControlPage.
 *
 * The Min / Max / Avg pence figures must render to 3 decimal places so a
 * sub-penny Octopus price (e.g. 12.345p) isn't truncated to 1dp. This mocks
 * the Octopus API `fetch` (native fetch, not apiGet) to return a single
 * upcoming slot at 12.345p, switches the charging-mode dropdown to Agile so
 * the price card mounts, and asserts the 3dp format on screen.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

function emptySlot() {
  return {
    enabled: false,
    start_hour: 0,
    start_minute: 0,
    end_hour: 0,
    end_minute: 0,
    target_soc: 100,
  };
}

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/agile')
      return { ok: true, enabled: false, scope: 'off', region: 'A', charge_threshold: 10, discharge_threshold: 30 };
    if (path === '/api/auto-winter')
      return { ok: true, data: { config: { enabled: false, cold_threshold: 8, recovery_threshold: 12, target_soc: 80, debounce_readings: 10 } } };
    if (path === '/api/cosy')
      return { ok: true, enabled: false, slots: Array.from({ length: 3 }, () => emptySlot()) };
    if (path === '/api/settings')
      return { ok: true, data: { import_tariff: 0.285, export_tariff: 0.15, import_tariff_config: null } };
    if (path === '/api/load-limiter')
      return { ok: true, data: { config: { enabled: false, threshold_w: 3000, trigger_delay_minutes: 0, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0 } } };
    return { ok: true };
  }),
  apiPost: vi.fn(async () => ({ ok: true })),
}));

import { useInverterStore } from '../../src/store/useInverterStore';

function makeSnapshot() {
  return {
    timestamp: 0,
    grid_voltage: 230,
    grid_frequency: 50,
    grid_power: 0,
    solar_power: 0,
    home_power: 0,
    battery_power: 0,
    battery_soc: 50,
    battery_temperature: 25,
    battery_current: 0,
    battery_voltage: 50,
    battery_cycles: 100,
    battery_state: 'idle',
    battery_capacity_kwh: 10,
    battery_charge_limit: 50,
    battery_discharge_limit: 50,
    energy_today: { solar_kwh: 0, grid_import_kwh: 0, grid_export_kwh: 0, home_kwh: 0, battery_charge_kwh: 0, battery_discharge_kwh: 0 },
    energy_lifetime: { solar_kwh: 0, grid_import_kwh: 0, grid_export_kwh: 0, home_kwh: 0, battery_charge_kwh: 0, battery_discharge_kwh: 0 },
    pv1_voltage: 0, pv1_current: 0, pv1_power: 0,
    pv2_voltage: 0, pv2_current: 0, pv2_power: 0,
    grid_loss: false, inverter_trip: false, battery_over_temp: false, inverter_temperature: 30,
    operating_hours: 0, today_discharge_kwh: 0, today_consumption_kwh: 0, home_energy_today_kwh: 0,
    battery_modules: [], battery_mode: 'eco', battery_reserve: 4,
    charge_rate: 50, discharge_rate: 50, active_power_rate: 100,
    max_battery_power_w: 5000, max_ac_power_w: 5000, export_limit_w: 0,
    target_soc: 4, enable_charge_target: false, enable_charge: false, enable_discharge: false,
    auto_winter_active: false, load_limiter_active: false, cosy_active: false, cosy_enabled: false,
    agile_active: false, agile_state: 'idle', agile_enabled: false, agile_scope: 'off',
    max_charge_slots: 2, max_discharge_slots: 2,
    charge_slots: [emptySlot(), emptySlot()], discharge_slots: [emptySlot(), emptySlot()],
    meters: [], inverter_serial: 'FD2328G358', firmware_version: '318',
    dsp_firmware_version: '318', dc_dsp_firmware_version: '',
    device_type: 'ac_coupled', device_type_display: 'AC Coupled', device_type_code: '3001',
    battery_calibration_stage: 0,
  };
}

describe('<ControlPage/> — Agile price card renders pence to 3dp', () => {
  let fetchSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.spyOn(console, 'warn').mockImplementation(() => {});

    // One upcoming slot at 12.345p — a sub-penny price that 1dp would
    // truncate to 12.3p. valid_from = now keeps it inside the rolling
    // upcoming window the card displays.
    const now = Date.now();
    const slot = {
      valid_from: new Date(now).toISOString(),
      valid_to: new Date(now + 30 * 60 * 1000).toISOString(),
      value_inc_vat: 12.345,
    };
    fetchSpy = vi.spyOn(globalThis, 'fetch').mockResolvedValue({
      ok: true,
      json: async () => ({ results: [slot] }),
    } as Response);

    useInverterStore.setState({ connectionState: 'connected' });
  });

  afterEach(() => {
    fetchSpy.mockRestore();
    vi.restoreAllMocks();
    cleanup();
  });

  it('shows Min / Max prices at 3 decimal places (12.345p, not 12.3p)', async () => {
    useInverterStore.setState({ snapshot: makeSnapshot() });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    // First combobox is the charging-mode dropdown.
    const select = (await screen.findAllByRole('combobox'))[0] as HTMLSelectElement;
    fireEvent.change(select, { target: { value: 'agile' } });

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalled();
    });
    // Min (and Max) must render the sub-penny value to 3dp.
    expect(await screen.findByText(/Min\s+12\.345p/)).toBeDefined();
    expect(await screen.findByText(/Max\s+12\.345p/)).toBeDefined();
  });
});
