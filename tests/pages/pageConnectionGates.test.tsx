import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import type { ComponentType } from 'react';

import BatteryPage from '../../src/pages/BatteryPage';
import SolarPage from '../../src/pages/SolarPage';
import InverterPage from '../../src/pages/InverterPage';
import MetersPage from '../../src/pages/MetersPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

// ---------------------------------------------------------------------------
// Battery / Solar / Inverter used to gate their waiting screen on
// `!snapshot` alone, while ControlPage gated on `connectionState`. That left
// a window — after the poll loop drops the connection but before the next WS
// frame clears the store — where these three pages rendered their full body
// against a stale snapshot while Control correctly showed "reconnecting".
// They now all gate on `!snapshot || connectionState !== 'connected'` and
// share the <AwaitingConnection/> placeholder, so the wording and the gate
// match ControlPage (and StatusPage). These tests pin both.
// ---------------------------------------------------------------------------

function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    soc: 50,
    battery_state: 'idle',
    battery_power: 0,
    battery_voltage: 51.2,
    battery_current: 0,
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
    // A populated module so BatteryPage's "Modules" section renders —
    // it's the page's distinctive connected-state marker. Solar / Inverter
    // ignore the field.
    battery_modules: [
      {
        index: 1,
        soc: 50,
        temperature: 20,
        voltage: 51.2,
        current: 0,
        serial: 'BAT001',
        num_cycles: 0,
        num_cells: 16,
        cell_voltages: [],
        cell_temperatures: [],
        bms_firmware: 0,
        capacity_ah: 0,
        design_capacity_ah: 0,
        remaining_capacity_ah: 0,
      },
    ],
    ...overrides,
  } as InverterSnapshot;
}

beforeEach(() => {
  cleanup();
  useInverterStore.setState({
    snapshot: null,
    connectionState: 'connected',
    developerMode: false,
    panelGraphsEnabled: false,
  });
});

afterEach(() => {
  useInverterStore.setState({ snapshot: null, connectionState: 'disconnected' });
});

// Per-page marker that only renders in the connected body, never in the
// placeholder. Lets us assert the gate hid the page content. All three are
// headings so we query by role (which tolerates text split across child
// nodes, e.g. BatteryPage's "Modules (1)" being three text nodes).
const CONNECTED_MARKERS: Record<string, RegExp> = {
  BatteryPage: /Modules/,
  SolarPage: /Solar Overview/,
  InverterPage: /Device Info/,
  MetersPage: /External CT Meters/,
};

describe.each([
  ['BatteryPage', BatteryPage],
  ['SolarPage', SolarPage],
  ['InverterPage', InverterPage],
  ['MetersPage', MetersPage],
] as Array<[string, ComponentType]>)(
  '<%s/> connection-state gate',
  (name, Page) => {
    it('shows the reconnecting screen and hides the page body while reconnecting — even with a stale snapshot', () => {
      // A snapshot is present but the poll loop has dropped the session.
      // The gate must fire on connectionState, not snapshot nullity, or the
      // page renders its body against stale data (the original report).
      useInverterStore.setState({
        snapshot: makeSnapshot(),
        connectionState: 'reconnecting',
        connectedHost: '192.168.1.36:8899',
      });
      render(<Page />);

      expect(
        screen.getByText('Connection lost — reconnecting…'),
      ).toBeDefined();
      expect(
        screen.queryByRole('heading', { name: CONNECTED_MARKERS[name]! }),
      ).toBeNull();
    });

    it('shows the disconnected wording and hides the page body', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot(),
        connectionState: 'disconnected',
        connectedHost: '192.168.1.36:8899',
      });
      render(<Page />);

      expect(
        screen.getByText('Disconnected — will retry automatically'),
      ).toBeDefined();
      expect(
        screen.queryByRole('heading', { name: CONNECTED_MARKERS[name]! }),
      ).toBeNull();
    });

    it('renders the page body once connected — no waiting copy leaks through', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot(),
        connectionState: 'connected',
      });
      render(<Page />);

      // No placeholder copy when the session is healthy.
      expect(screen.queryByText(/Connection lost/i)).toBeNull();
      expect(screen.queryByText(/Disconnected/i)).toBeNull();
      expect(screen.queryByText(/Waiting for data/i)).toBeNull();

      // And the page's own body is present.
      expect(
        screen.getByRole('heading', { name: CONNECTED_MARKERS[name]! }),
      ).toBeDefined();
    });
  },
);
