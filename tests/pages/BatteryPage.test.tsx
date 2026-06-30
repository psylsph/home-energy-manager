import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/react';

// ---------------------------------------------------------------------------
// BatteryPage gates on connectivity (renders AwaitingConnection unless
// connected), lists expandable battery modules with cell-voltage bars / cell
// temps / BMS registers, shows a batteryless message for devices with no
// modules, and conditionally renders the SOC trend chart. We mock the heavy
// children and drive the store through connected / disconnected states.
// ---------------------------------------------------------------------------

vi.mock('../../src/components/ColdBatteryWarning', () => ({
  default: () => <div data-testid="cold-warning">ColdBatteryWarning</div>,
}));
vi.mock('../../src/components/BatteryPanel', () => ({
  default: ({ snapshot }: { snapshot: { soc: number } }) => (
    <div data-testid="battery-panel">BatteryPanel soc={snapshot.soc}</div>
  ),
}));
vi.mock('../../src/components/BatterySocChart', () => ({
  default: () => <div data-testid="soc-chart">BatterySocChart</div>,
}));
vi.mock('../../src/components/AwaitingConnection', () => ({
  default: ({ showFaq }: { showFaq?: boolean }) => (
    <div data-testid="awaiting">AwaitingConnection faq={String(showFaq ?? false)}</div>
  ),
}));

import BatteryPage from '../../src/pages/BatteryPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot, BatteryModule } from '../../src/lib/types';

function makeModule(overrides: Partial<BatteryModule> = {}): BatteryModule {
  return {
    index: 0,
    soc: 80,
    temperature: 22,
    voltage: 52.4,
    current: 0,
    serial: 'BAT-001',
    num_cycles: 12,
    num_cells: 16,
    cell_voltages: [3.3, 3.31, 3.32, 3.3],
    cell_temperatures: [21, 22, 23, 24],
    bms_firmware: 5,
    capacity_ah: 9.5,
    design_capacity_ah: 9.5,
    remaining_capacity_ah: 7.6,
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

describe('BatteryPage', () => {
  describe('connectivity gating', () => {
    it('renders the awaiting placeholder when disconnected', () => {
      render(<BatteryPage />);
      expect(screen.getByTestId('awaiting')).toBeDefined();
      expect(screen.getByTestId('awaiting').textContent).toContain('faq=true');
    });

    it('renders the awaiting placeholder when connected but no snapshot', () => {
      useInverterStore.setState({ connectionState: 'connected' });
      render(<BatteryPage />);
      expect(screen.getByTestId('awaiting')).toBeDefined();
    });

    it('renders the awaiting placeholder when reconnecting (even with snapshot)', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot(),
        connectionState: 'reconnecting',
      });
      render(<BatteryPage />);
      expect(screen.getByTestId('awaiting')).toBeDefined();
    });
  });

  describe('with a connected snapshot', () => {
    it('renders the battery panel and cold warning', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({ soc: 64 }),
        connectionState: 'connected',
      });
      render(<BatteryPage />);
      expect(screen.getByTestId('battery-panel').textContent).toContain('soc=64');
      expect(screen.getByTestId('cold-warning')).toBeDefined();
    });

    it('renders the SOC trend chart when panel graphs are enabled', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot(),
        connectionState: 'connected',
        panelGraphsEnabled: true,
      });
      render(<BatteryPage />);
      expect(screen.getByTestId('soc-chart')).toBeDefined();
    });

    it('hides the SOC trend chart when panel graphs are disabled', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot(),
        connectionState: 'connected',
        panelGraphsEnabled: false,
      });
      render(<BatteryPage />);
      expect(screen.queryByTestId('soc-chart')).toBeNull();
    });
  });

  describe('battery modules', () => {
    it('shows the module count heading', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({ battery_modules: [makeModule(), makeModule({ index: 1 })] }),
        connectionState: 'connected',
      });
      render(<BatteryPage />);
      expect(screen.getByText(/Modules \(2\)/)).toBeDefined();
    });

    it('renders a header row per module with SOC/voltage/temp', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({
          battery_modules: [makeModule({ index: 0, soc: 82, voltage: 52.4, temperature: 23 })],
        }),
        connectionState: 'connected',
      });
      render(<BatteryPage />);
      expect(screen.getByText('#1')).toBeDefined();
      expect(screen.getByText('82%')).toBeDefined();
      expect(screen.getByText('52.4V')).toBeDefined();
      expect(screen.getByText('23.0°C')).toBeDefined();
    });

    it('expands a module to show cell voltage chart and details', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({
          battery_modules: [
            makeModule({
              index: 0,
              serial: 'BAT-X',
              num_cycles: 42,
              bms_firmware: 7,
              design_capacity_ah: 9.5,
              capacity_ah: 9.0,
              cell_voltages: [3.2, 3.3, 3.4, 3.3],
            }),
          ],
        }),
        connectionState: 'connected',
      });
      render(<BatteryPage />);
      // Collapsed: details hidden.
      expect(screen.queryByText('Cell Voltages')).toBeNull();
      // Click the module header to expand.
      fireEvent.click(screen.getByText('#1'));
      expect(screen.getByText('Cell Voltages')).toBeDefined();
      expect(screen.getByText('BAT-X')).toBeDefined();
      expect(screen.getByText('42')).toBeDefined();
      expect(screen.getByText('7')).toBeDefined();
    });

    it('collapses an expanded module on a second click', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({
          battery_modules: [makeModule({ cell_voltages: [3.3, 3.3] })],
        }),
        connectionState: 'connected',
      });
      render(<BatteryPage />);
      const header = screen.getByText('#1');
      fireEvent.click(header); // expand
      expect(screen.getByText('Cell Voltages')).toBeDefined();
      fireEvent.click(header); // collapse
      expect(screen.queryByText('Cell Voltages')).toBeNull();
    });

    it('shows the developer BMS status registers only in developer mode', () => {
      const module = makeModule({
        bms_status_registers: [256, 257, 258, 259, 260],
        bms_status: [1, 2, 3],
        bms_warnings: [0, 1],
      });
      useInverterStore.setState({
        snapshot: makeSnapshot({ battery_modules: [module] }),
        connectionState: 'connected',
        developerMode: true,
      });
      render(<BatteryPage />);
      fireEvent.click(screen.getByText('#1'));
      expect(screen.getByText('Developer: Raw BMS Status Registers')).toBeDefined();
      expect(screen.getByText('IR90')).toBeDefined();
    });

    it('hides developer BMS registers when developer mode is off', () => {
      const module = makeModule({
        bms_status_registers: [256, 257],
      });
      useInverterStore.setState({
        snapshot: makeSnapshot({ battery_modules: [module] }),
        connectionState: 'connected',
        developerMode: false,
      });
      render(<BatteryPage />);
      fireEvent.click(screen.getByText('#1'));
      expect(screen.queryByText('Developer: Raw BMS Status Registers')).toBeNull();
    });

    it('shows cell temperature probes when present', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({
          battery_modules: [makeModule({ cell_temperatures: [21.5, 22.5, 23.5] })],
        }),
        connectionState: 'connected',
      });
      render(<BatteryPage />);
      fireEvent.click(screen.getByText('#1'));
      expect(screen.getByText('Cell Group Temps')).toBeDefined();
      expect(screen.getByText('21.5°C')).toBeDefined();
    });

    it('renders lifetime throughput + battery life remaining when set', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({
          battery_capacity_kwh: 9.5,
          total_throughput_kwh: 1000,
          battery_modules: [makeModule()],
        }),
        connectionState: 'connected',
      });
      render(<BatteryPage />);
      fireEvent.click(screen.getByText('#1'));
      expect(screen.getByText('Total Throughput')).toBeDefined();
      expect(screen.getByText('1000 kWh')).toBeDefined();
      expect(screen.getByText('Battery Life Remaining')).toBeDefined();
    });

    it('renders state of health when design + actual capacity are set', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({
          battery_modules: [makeModule({ design_capacity_ah: 10, capacity_ah: 9 })],
        }),
        connectionState: 'connected',
      });
      render(<BatteryPage />);
      fireEvent.click(screen.getByText('#1'));
      expect(screen.getByText('State of Health')).toBeDefined();
      // 9 / 10 * 100 = 90%
      expect(screen.getByText('90%')).toBeDefined();
    });
  });

  describe('batteryless devices', () => {
    it('shows the no-modules message for a standard device', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({ battery_modules: [], device_type_code: '2201' }),
        connectionState: 'connected',
      });
      render(<BatteryPage />);
      expect(screen.getByText('No battery module data available.')).toBeDefined();
      expect(screen.getByText(/will appear once detected/)).toBeDefined();
    });

    it('shows the Gateway-specific message for a device_type_code starting with 70', () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({ battery_modules: [], device_type_code: '7001' }),
        connectionState: 'connected',
      });
      render(<BatteryPage />);
      expect(screen.getByText(/Gateway does not expose per-cell telemetry/)).toBeDefined();
    });
  });
});
