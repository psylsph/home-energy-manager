import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, within } from '@testing-library/react';

// ---------------------------------------------------------------------------
// MetersPage lists CT clamp meters read off the inverter's CT bus. For
// three-phase / HV models the decoder synthesises a "built-in grid CT"
// entry at address 0x00 (the IR 1060-1119 block reports grid flow inline
// rather than over an external CT bus). The card must label that entry
// distinctly from a real external clamp (0x01-0x08) and must not render its
// zeroed per-phase power fields, which would otherwise look like a broken
// external meter. These tests pin both behaviours.
// ---------------------------------------------------------------------------

vi.mock('../../src/components/AwaitingConnection', () => ({
  default: ({ showFaq }: { showFaq?: boolean }) => (
    <div data-testid="awaiting">AwaitingConnection faq={String(showFaq ?? false)}</div>
  ),
}));

import MetersPage from '../../src/pages/MetersPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot, MeterData } from '../../src/lib/types';

/** A synthetic built-in grid CT, as produced by `decode_input_1060_1119`. */
function syntheticMeter(overrides: Partial<MeterData> = {}): MeterData {
  return {
    address: 0x00,
    v_phase_1: 415.0, v_phase_2: 416.0, v_phase_3: 414.0,
    i_phase_1: 1.0, i_phase_2: 1.2, i_phase_3: 0.8,
    i_ln: 0, i_total: 1.0,
    // No signed per-phase grid registers on three-phase — left at 0 and
    // must be hidden, not rendered as "+0W".
    p_active_phase_1: 0, p_active_phase_2: 0, p_active_phase_3: 0,
    p_active_total: -600, // exporting 600W
    p_reactive_total: 0, p_apparent_total: 1500, pf_total: 0.98,
    frequency: 50.0, e_import_active_kwh: 88.8, e_export_active_kwh: 99.9,
    ...overrides,
  };
}

/** A real external CT clamp at address 0x01 with live per-phase readings. */
function externalMeter(overrides: Partial<MeterData> = {}): MeterData {
  return {
    address: 0x01,
    v_phase_1: 240.0, v_phase_2: 0.0, v_phase_3: 0.0,
    i_phase_1: 2.5, i_phase_2: 0.0, i_phase_3: 0.0,
    i_ln: 0, i_total: 2.5,
    p_active_phase_1: 500, p_active_phase_2: 0, p_active_phase_3: 0,
    p_active_total: 500, // importing 500W
    p_reactive_total: 0, p_apparent_total: 500, pf_total: 1.0,
    frequency: 50.0, e_import_active_kwh: 12.3, e_export_active_kwh: 0.0,
    ...overrides,
  };
}

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
    dc_dsp_firmware_version: '', device_type: '', device_type_display: 'Three Phase',
    device_type_code: '4001', battery_calibration_stage: 0, enable_ammeter: false,
    enable_reversed_ct_clamp: false, meter_type: 0, supports_battery_calibration: false,
    ac_eps_enabled: false, ac_export_priority: 0,
    ...overrides,
  };
}

function resetStore() {
  useInverterStore.setState({
    snapshot: null,
    connectionState: 'disconnected',
    developerMode: false,
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

describe('MetersPage', () => {
  describe('connectivity gating', () => {
    it('renders the awaiting placeholder when disconnected', () => {
      render(<MetersPage />);
      expect(screen.getByTestId('awaiting')).toBeDefined();
      expect(screen.getByTestId('awaiting').textContent).toContain('faq=true');
    });

    it('renders the awaiting placeholder when connected but no snapshot', () => {
      useInverterStore.setState({ connectionState: 'connected' });
      render(<MetersPage />);
      expect(screen.getByTestId('awaiting')).toBeDefined();
    });
  });

  describe('empty state', () => {
    it('shows the no-meters message when no meters are present', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({ meters: [] }),
        connectionState: 'connected',
      });
      render(<MetersPage />);
      expect(
        screen.getByText('No external CT meters detected on your system.'),
      ).toBeDefined();
    });
  });

  describe('synthetic built-in grid CT vs external meter (address 0x00)', () => {
    it('labels the synthetic CT as "Built-in Grid CT", not "Meter 0x00"', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({ meters: [syntheticMeter()] }),
        connectionState: 'connected',
      });
      render(<MetersPage />);
      expect(screen.getByText('Built-in Grid CT')).toBeDefined();
      // Must NOT render the generic external-meter label — that is the
      // confusion the issue warns about (a zeroed "Meter 0x00" looking
      // like a broken external clamp).
      expect(screen.queryByText('Meter 0x00')).toBeNull();
    });

    it('hides per-phase power for the synthetic CT', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({ meters: [syntheticMeter()] }),
        connectionState: 'connected',
      });
      render(<MetersPage />);
      // Per-phase power is unavailable (no signed registers); the zeroed
      // fields must not be rendered as "+0W".
      expect(screen.queryByText('+0W')).toBeNull();
    });

    it('labels a real external clamp with its Modbus address', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({ meters: [externalMeter()] }),
        connectionState: 'connected',
      });
      render(<MetersPage />);
      expect(screen.getByText('Meter 0x01')).toBeDefined();
      expect(screen.queryByText('Built-in Grid CT')).toBeNull();
    });

    it('shows per-phase power for a real external clamp', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({ meters: [externalMeter()] }),
        connectionState: 'connected',
      });
      render(<MetersPage />);
      expect(screen.getByText('+500W')).toBeDefined();
    });

    it('distinguishes both when a three-phase system also has an external meter', () => {
      // A three-phase inverter can report the built-in grid CT (0x00) AND
      // an external clamp (0x09 via IR 1244-1245). Both cards must render
      // with the correct label and per-phase visibility so the user can
      // tell them apart at a glance.
      useInverterStore.setState({
        snapshot: makeSnapshot({
          meters: [
            syntheticMeter(),
            externalMeter({ address: 0x09, p_active_phase_1: 750, p_active_total: 750 }),
          ],
        }),
        connectionState: 'connected',
      });
      render(<MetersPage />);

      const syntheticCard = screen.getByText('Built-in Grid CT').closest('.bg-bg-surface')!;
      const externalCard = screen.getByText('Meter 0x09').closest('.bg-bg-surface')!;

      // Synthetic: per-phase power hidden.
      expect(within(syntheticCard).queryByText('+0W')).toBeNull();
      // External: per-phase power shown.
      expect(within(externalCard).getByText('+750W')).toBeDefined();
    });
  });
});
