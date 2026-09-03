import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Review H16: the power-limit / Minimum-SOC saves caught failures with a
// console.warn (mislabeled "Slot save failed") and never reconciled the
// optimistic draft — a rejected write was indistinguishable from a
// confirmed one. Drag Minimum SOC 20→80, save, watch the dongle write fail:
// no error anywhere, the slider still read 80%, and the inverter kept its
// real setting. A failed save must surface an error banner and snap the
// slider back to the snapshot value.
// ---------------------------------------------------------------------------

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
  apiPost: vi.fn(),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  fetchHistory: vi.fn().mockResolvedValue({}),
  isTauri: false,
}));

import ControlPage from '../../src/pages/ControlPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';
import { apiPost } from '../../src/lib/api';
import { LOGARITHMIC_RANGE_MAX, logarithmicValueToPosition } from '../../src/lib/logarithmicRange';

function silenceConsole() {
  vi.spyOn(console, 'error').mockImplementation(() => {});
  vi.spyOn(console, 'warn').mockImplementation(() => {});
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
    charge_slots: [
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
    ],
    discharge_slots: [
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
    ],
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
    battery_pause_slot: {
      enabled: false,
      start_hour: 0,
      start_minute: 0,
      end_hour: 0,
      end_minute: 0,
      target_soc: 100,
    },
    ...overrides,
  };
}

/** The Save button that shares a row with the given slider. */
function saveButtonFor(slider: HTMLElement): HTMLButtonElement {
  const button = slider.parentElement?.querySelector<HTMLButtonElement>('button');
  expect(button, 'slider row should contain a Save button').toBeDefined();
  return button!;
}

/** Section 6 sliders, in DOM order: [force-duration, min-soc, charge, discharge, active-power]. */
function powerSliders(): HTMLElement[] {
  return screen.getAllByRole('slider');
}

describe('<ControlPage/> — power-control save failures surface and reconcile', () => {
  beforeEach(() => {
    silenceConsole();
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
    window.localStorage.clear();
    vi.mocked(apiPost).mockReset();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    cleanup();
    useInverterStore.setState({ snapshot: null, connectionState: 'disconnected' });
  });

  it('shows an error and reverts the Minimum SOC slider when the save fails', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot(),
      developerMode: false,
      connectionState: 'connected',
      connectedHost: '192.168.1.36:8899',
    });
    vi.mocked(apiPost).mockImplementation(async (path: string) => {
      if (path === '/api/control/reserve') {
        throw new Error('dongle busy');
      }
      return { ok: true, data: {} };
    });
    render(<ControlPage />);

    const slider = screen.getByLabelText('Minimum SOC') as HTMLInputElement;
    expect(slider.getAttribute('aria-valuenow')).toBe('20');

    // Drag to 80% and save. The Minimum SOC slider maps positions through a
    // logarithmic scale, so drive it the same way the page does.
    const positionFor = (soc: number) =>
      String(logarithmicValueToPosition(soc, 4, 100, LOGARITHMIC_RANGE_MAX));
    fireEvent.change(slider, { target: { value: positionFor(80) } });
    expect(slider.getAttribute('aria-valuenow')).toBe('80');
    fireEvent.click(saveButtonFor(slider));

    // The failure must surface…
    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toMatch(/Minimum SOC save failed/);
      expect(screen.getByRole('alert').textContent).toMatch(/dongle busy/);
    });
    // …and the optimistic draft must reconcile back to the inverter value.
    await waitFor(() => {
      expect(slider.getAttribute('aria-valuenow')).toBe('20');
    });
    // The failed write must still have been attempted.
    expect(apiPost).toHaveBeenCalledWith('/api/control/reserve', { soc: 80 });
  });

  it('shows an error and reverts the Charge Power Limit slider when the save fails', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot(),
      developerMode: false,
      connectionState: 'connected',
      connectedHost: '192.168.1.36:8899',
    });
    vi.mocked(apiPost).mockImplementation(async (path: string) => {
      if (path === '/api/control/charge-rate') {
        throw new Error('register write rejected');
      }
      return { ok: true, data: {} };
    });
    render(<ControlPage />);

    // Sliders in section 6: [force-duration, min-soc, charge, …].
    const chargeSlider = powerSliders()[2] as HTMLInputElement;
    expect(chargeSlider.value).toBe('100'); // snapshot 50 × display multiplier 2

    fireEvent.change(chargeSlider, { target: { value: '30' } });
    expect(chargeSlider.value).toBe('30');
    fireEvent.click(saveButtonFor(chargeSlider));

    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toMatch(/Charge power limit save failed/);
    });
    await waitFor(() => {
      expect(chargeSlider.value).toBe('100');
    });
    expect(apiPost).toHaveBeenCalledWith('/api/control/charge-rate', { limit: 15 });
  });

  it('clears the error banner after a successful save', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot(),
      developerMode: false,
      connectionState: 'connected',
      connectedHost: '192.168.1.36:8899',
    });
    let reserveCalls = 0;
    vi.mocked(apiPost).mockImplementation(async (path: string) => {
      if (path === '/api/control/reserve') {
        reserveCalls += 1;
        if (reserveCalls === 1) throw new Error('dongle busy');
      }
      return { ok: true, data: {} };
    });
    render(<ControlPage />);

    const slider = screen.getByLabelText('Minimum SOC') as HTMLInputElement;
    fireEvent.change(slider, { target: { value: '80' } });
    fireEvent.click(saveButtonFor(slider));
    await screen.findByRole('alert');

    // Retry the same save; it now succeeds and the banner clears.
    fireEvent.change(slider, { target: { value: '80' } });
    fireEvent.click(saveButtonFor(slider));
    await waitFor(() => {
      expect(screen.queryByRole('alert')).toBeNull();
    });
  });
});
