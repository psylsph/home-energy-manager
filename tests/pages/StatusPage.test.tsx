import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, cleanup } from '@testing-library/react';
import { useInverterStore } from '../../src/store/useInverterStore';
import StatusPage from '../../src/pages/StatusPage';
import type { InverterSnapshot } from '../../src/lib/types';

// Mock child components to avoid deep rendering issues
vi.mock('../../src/components/EnergyOrbitDiagram', () => ({
  default: () => <div data-testid="energy-orbit-diagram">EnergyOrbitDiagram</div>,
}));
vi.mock('../../src/components/BatteryPanel', () => ({
  default: () => <div data-testid="battery-panel">BatteryPanel</div>,
}));
vi.mock('../../src/components/SummaryTiles', () => ({
  default: () => <div data-testid="summary-tiles">SummaryTiles</div>,
}));
vi.mock('../../src/components/ColdBatteryWarning', () => ({
  default: () => <div data-testid="cold-battery-warning">ColdBatteryWarning</div>,
}));

// Mock apiPost
vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn(),
}));

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
    connectedHost: null,
    connectedSince: null,
    connectFailures: 0,
    evcHost: '',
    evcPower: 0,
    evcChargingState: '',
    evcCharging: false,
    evcConnected: false,
    evcEverConnected: false,
  });
}

beforeEach(() => {
  cleanup();
  resetStore();
});

describe('StatusPage', () => {
  it('shows loading spinner when no snapshot and disconnected', () => {
    useInverterStore.setState({ connectionState: 'disconnected' });
    const { container } = render(<StatusPage />);
    expect(container.querySelector('.animate-spin')).not.toBeNull();
    expect(container.textContent).toContain('Disconnected');
  });

  it('shows reconnecting message when reconnecting', () => {
    useInverterStore.setState({ connectionState: 'reconnecting' });
    const { container } = render(<StatusPage />);
    expect(container.textContent).toContain('reconnecting');
  });

  it('shows waiting message when connected but no snapshot yet', () => {
    useInverterStore.setState({ connectionState: 'connected' });
    const { container } = render(<StatusPage />);
    expect(container.textContent).toContain('Waiting for data');
  });

  it('shows failure advice banner after 5+ connect failures', () => {
    useInverterStore.setState({
      connectionState: 'disconnected',
      connectFailures: 5,
    });
    const { container } = render(<StatusPage />);
    expect(container.textContent).toContain("Can't reach the dongle");
  });

  it('does not show failure advice banner with few failures', () => {
    useInverterStore.setState({
      connectionState: 'disconnected',
      connectFailures: 2,
    });
    const { container } = render(<StatusPage />);
    expect(container.textContent).not.toContain("Can't reach the dongle");
  });

  it('shows host info when connectedHost is set', () => {
    useInverterStore.setState({
      connectionState: 'disconnected',
      connectedHost: '192.168.1.10:8899',
    });
    const { container } = render(<StatusPage />);
    expect(container.textContent).toContain('192.168.1.10');
  });

  it('shows connectedSince duration when available', () => {
    useInverterStore.setState({
      connectionState: 'disconnected',
      connectedSince: Date.now() - 120_000, // 2 minutes ago
    });
    const { container } = render(<StatusPage />);
    expect(container.textContent).toMatch(/last connected/i);
  });

  it('renders main content when snapshot is available', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot(),
      connectionState: 'connected',
    });
    const { container } = render(<StatusPage />);
    expect(container.querySelector('[data-testid="energy-orbit-diagram"]')).not.toBeNull();
    expect(container.querySelector('[data-testid="battery-panel"]')).not.toBeNull();
    expect(container.querySelector('[data-testid="summary-tiles"]')).not.toBeNull();
  });

  it('places the diagram beside the panels in a landscape grid', () => {
    // The status page arranges the energy-flow diagram next to the summary
    // + battery panels (rather than stacked) so the whole page fits in
    // ~800px tall on wide viewports. The layout is driven by responsive
    // Tailwind classes on this grid container; here we assert the
    // structure (diagram section + panels column are siblings inside the
    // grid) so a future refactor can't silently drop the side-by-side
    // arrangement.
    useInverterStore.setState({
      snapshot: makeSnapshot(),
      connectionState: 'connected',
    });
    const { container } = render(<StatusPage />);
    const grid = container.querySelector('[data-testid="status-landscape-grid"]');
    expect(grid).not.toBeNull();
    // The diagram lives in a <section> that is a direct child of the grid.
    expect(grid!.querySelector(':scope > section')).not.toBeNull();
    // Both panels render inside the grid's panels column.
    expect(grid!.querySelector('[data-testid="summary-tiles"]')).not.toBeNull();
    expect(grid!.querySelector('[data-testid="battery-panel"]')).not.toBeNull();
  });

  it('shows connection status bar when not connected even with snapshot', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot(),
      connectionState: 'reconnecting',
    });
    const { container } = render(<StatusPage />);
    expect(container.textContent).toContain('reconnecting');
  });

  it('hides connection status bar when connected', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot(),
      connectionState: 'connected',
    });
    const { container } = render(<StatusPage />);
    // The status bar text "reconnecting" / "disconnected" should not appear
    expect(container.textContent).not.toContain('reconnecting');
    expect(container.textContent).not.toContain('disconnected');
  });

  it('shows grid fault banner when grid fault is detected', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({
        grid_online: false,
        grid_loss: true,
        soc: 50,
        battery_power: 500,
      }),
      connectionState: 'connected',
    });
    const { container } = render(<StatusPage />);
    // The grid fault banner should render (it checks hasGridFault)
    const banner = container.querySelector('.bg-red-950\\/50');
    expect(banner).not.toBeNull();
  });
});
