import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import EnergyFlowDiagram from '../../src/components/EnergyFlowDiagram';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** A minimal snapshot with all fields the diagram reads. */
function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    solar_power: 0,
    home_power: 0,
    grid_power: 0,
    battery_power: 0,
    battery_state: 'idle',
    soc: 50,
    pv1_voltage: 0,
    pv1_current: 0,
    pv2_current: 0,
    inverter_temperature: 25,
    device_type_display: 'Gen2',
    battery_mode: 'eco',
    cosy_active: false,
    cosy_enabled: false,
    enable_charge: false,
    enable_discharge: false,
    charge_slots: [],
    discharge_slots: [],
    agile_active: false,
    agile_state: '',
    // Satisfy the full interface with defaults for remaining fields.
    device_type: 'gen2',
    device_type_code: 'G2',
    meter_type: 0,
    pv1_power: 0,
    pv2_power: 0,
    grid_voltage: 240,
    grid_frequency: 50,
    inverter_temperature_max: 60,
    battery_temperature: 20,
    battery_voltage: 48,
    battery_current: 0,
    battery_module_count: 0,
    battery_modules: [],
    battery_serial: '',
    battery_firmware: '',
    battery_design_energy_kwh: 0,
    battery_full_energy_kwh: 0,
    battery_nominal_energy_kwh: 0,
    battery_remaining_energy_kwh: 0,
    today_solar_kwh: 0,
    today_import_kwh: 0,
    today_export_kwh: 0,
    today_charge_kwh: 0,
    today_discharge_kwh: 0,
    today_consumption_kwh: 0,
    home_energy_today_kwh: 0,
    lifetime_solar_kwh: 0,
    lifetime_import_kwh: 0,
    lifetime_export_kwh: 0,
    lifetime_charge_kwh: 0,
    lifetime_discharge_kwh: 0,
    work_time_total: 0,
    charge_slot_count: 2,
    discharge_slot_count: 2,
    ...overrides,
  };
}

/** Reset the store to a clean state before each test. */
function resetStore(threshold = 20) {
  useInverterStore.setState({
    visualNoiseThreshold: threshold,
  });
}

// ---------------------------------------------------------------------------
// SVG text content helpers
//
// The diagram renders values inside SVG <text> elements. @testing-library's
// getByText works with DOM text nodes, which includes SVG text content.
// ---------------------------------------------------------------------------

describe('EnergyFlowDiagram — noise threshold', () => {
  beforeEach(() => {
    resetStore(20);
    // jsdom doesn't implement matchMedia; the useIsMobile hook needs it.
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
    cleanup();
  });

  // ---- Solar flow -------------------------------------------------------

  it('shows solar power as "0W" and no animation when below threshold', () => {
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({
          solar_power: 15,
          // Set other flows above threshold so only solar shows "0W"
          home_power: 100,
          grid_power: 100,
          battery_power: 100,
          battery_state: 'discharging',
        })}
      />,
    );
    // The Solar node value should be "0W" (clamped)
    expect(screen.getByText('0W')).toBeDefined();
    // The Solar node label should still be visible
    expect(screen.getByText('SOLAR')).toBeDefined();
  });

  it('shows the real solar power when above threshold', () => {
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({ solar_power: 1500 })}
      />,
    );
    expect(screen.getByText('1.5kW')).toBeDefined();
  });

  it('shows solar power at exactly the threshold', () => {
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({ solar_power: 20 })}
      />,
    );
    expect(screen.getByText('20W')).toBeDefined();
  });

  // ---- Grid flow --------------------------------------------------------

  it('shows grid as "0W" and "Idle" when import is below threshold', () => {
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({
          grid_power: -15,
          solar_power: 100,
          home_power: 100,
          battery_power: 100,
          battery_state: 'discharging',
        })}
      />,
    );
    expect(screen.getByText('0W')).toBeDefined();
    expect(screen.getByText('Idle')).toBeDefined();
  });

  it('shows grid as "0W" and "Idle" when export is below threshold', () => {
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({
          grid_power: 15,
          solar_power: 100,
          home_power: 100,
          battery_power: 100,
          battery_state: 'discharging',
        })}
      />,
    );
    expect(screen.getByText('0W')).toBeDefined();
    expect(screen.getByText('Idle')).toBeDefined();
  });

  it('shows "Import" when import is above threshold', () => {
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({ grid_power: -500 })}
      />,
    );
    expect(screen.getByText('500W')).toBeDefined();
    expect(screen.getByText('Import')).toBeDefined();
  });

  it('shows "Export" when export is above threshold', () => {
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({
          grid_power: 500,
          solar_power: 100,
          home_power: 100,
          battery_power: 100,
          battery_state: 'discharging',
        })}
      />,
    );
    // Export convention: grid shows negative value
    expect(screen.getByText('-500W')).toBeDefined();
    expect(screen.getByText('Export')).toBeDefined();
  });

  // ---- Home flow --------------------------------------------------------

  it('shows home power as "0W" when below threshold', () => {
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({
          home_power: 10,
          solar_power: 100,
          grid_power: 100,
          battery_power: 100,
          battery_state: 'discharging',
        })}
      />,
    );
    expect(screen.getByText('0W')).toBeDefined();
  });

  it('shows the real home power when above threshold', () => {
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({ home_power: 800 })}
      />,
    );
    expect(screen.getByText('800W')).toBeDefined();
  });

  // ---- Battery flow -----------------------------------------------------

  it('shows battery as "0W" with no prefix when charging below threshold', () => {
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({
          battery_power: -10,
          battery_state: 'charging',
          solar_power: 100,
          home_power: 100,
          grid_power: 100,
        })}
      />,
    );
    // Should show "0W" (not "-0W") — the prefix is suppressed when below threshold
    const zeroNodes = screen.getAllByText('0W');
    expect(zeroNodes.length).toBeGreaterThanOrEqual(1);
    // No "-0W" text should exist
    expect(screen.queryByText('-0W')).toBeNull();
  });

  it('shows battery as "0W" with no prefix when discharging below threshold', () => {
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({
          battery_power: 10,
          battery_state: 'discharging',
          solar_power: 100,
          home_power: 100,
          grid_power: 100,
        })}
      />,
    );
    const zeroNodes = screen.getAllByText('0W');
    expect(zeroNodes.length).toBeGreaterThanOrEqual(1);
    expect(screen.queryByText('-0W')).toBeNull();
  });

  it('shows battery power with prefix when discharging above threshold', () => {
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({
          battery_power: 1500,
          battery_state: 'discharging',
        })}
      />,
    );
    expect(screen.getByText('-1.5kW')).toBeDefined();
  });

  it('shows battery power without prefix when charging above threshold', () => {
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({
          battery_power: -1500,
          battery_state: 'charging',
        })}
      />,
    );
    expect(screen.getByText('1.5kW')).toBeDefined();
  });

  // ---- Custom threshold -------------------------------------------------

  it('respects a custom threshold of 50W', () => {
    resetStore(50);
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({
          solar_power: 30,
          home_power: 40,
        })}
      />,
    );
    // Both are below 50W threshold, so both show "0W"
    const zeroNodes = screen.getAllByText('0W');
    expect(zeroNodes.length).toBeGreaterThanOrEqual(2);
  });

  it('shows values when above a custom threshold of 50W', () => {
    resetStore(50);
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({
          solar_power: 60,
          home_power: 2000,
        })}
      />,
    );
    expect(screen.getByText('60W')).toBeDefined();
    expect(screen.getByText('2.0kW')).toBeDefined();
  });

  it('respects a threshold of 0 (show everything)', () => {
    resetStore(0);
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({
          solar_power: 1,
          home_power: 2,
        })}
      />,
    );
    expect(screen.getByText('1W')).toBeDefined();
    expect(screen.getByText('2W')).toBeDefined();
  });

  // ---- EV flow ----------------------------------------------------------

  it('shows EV power as "0W" when below threshold', () => {
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({
          solar_power: 100,
          home_power: 100,
          grid_power: 100,
          battery_power: 100,
          battery_state: 'discharging',
        })}
        evcPower={15}
        evcCharging={true}
        evcConnected={true}
        showEvc={true}
      />,
    );
    expect(screen.getByText('0W')).toBeDefined();
  });

  it('shows EV power when above threshold', () => {
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot()}
        evcPower={7400}
        evcCharging={true}
        evcConnected={true}
        showEvc={true}
      />,
    );
    expect(screen.getByText('7.4kW')).toBeDefined();
  });

  it('shows "Idle" label when EVC reports state=1 with no power (issue #139)', () => {
    // User-reported case: state=4 (Charging) → state=1 (Idle), cable
    // unplugged, P=0W. The diagram label should switch to "Idle" rather
    // than "Disconnected" so it matches the EVC's own display. We push
    // the grid to an active import so its label reads "Import" instead
    // of "Idle" — otherwise the grid node also renders the word "Idle"
    // and the assertion below is ambiguous.
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({
          grid_power: -500,
          solar_power: 100,
          home_power: 100,
          battery_power: 100,
          battery_state: 'discharging',
        })}
        evcPower={0}
        evcChargingState="Idle"
        evcCharging={false}
        evcConnected={false}
        evcEverConnected={true}
        showEvc={true}
      />,
    );
    expect(screen.getByText('500W')).toBeDefined();
    expect(screen.getByText('Import')).toBeDefined();
    expect(screen.getByText('Idle')).toBeDefined();
  });

  it('still shows "Charging" when power is flowing even if chargingState lags as "Idle"', () => {
    // Edge case: raw string and power disagree by one poll cycle. Power
    // wins — we don't show "Idle" while current is flowing. We push
    // the grid to an active import so its label reads "Import" (not
    // "Idle") and the assertion below is unambiguous.
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({
          grid_power: -500,
          solar_power: 100,
          home_power: 100,
          battery_power: 100,
          battery_state: 'discharging',
        })}
        evcPower={6900}
        evcChargingState="Idle"
        evcCharging={true}
        evcConnected={true}
        evcEverConnected={true}
        showEvc={true}
      />,
    );
    expect(screen.getByText('6.9kW')).toBeDefined();
    expect(screen.getByText('500W')).toBeDefined();
    expect(screen.getByText('Charging')).toBeDefined();
    // No "Idle" anywhere — the EV label is "Charging" (power wins), and
    // the grid label is "Import" (we forced grid_power != 0).
    expect(screen.queryByText('Idle')).toBeNull();
  });

  // ---- Mixed scenario ---------------------------------------------------

  it('shows some flows as zero and others as real values', () => {
    render(
      <EnergyFlowDiagram
        snapshot={makeSnapshot({
          solar_power: 5,       // below 20W → "0W"
          home_power: 300,      // above 20W → "300W"
          grid_power: -500,     // above 20W → "500W" + "Import"
          battery_power: 10,    // below 20W → "0W", no prefix
          battery_state: 'discharging',
        })}
      />,
    );
    expect(screen.getByText('300W')).toBeDefined();
    expect(screen.getByText('500W')).toBeDefined();
    expect(screen.getByText('Import')).toBeDefined();
    // Solar and battery both show "0W"
    const zeroNodes = screen.getAllByText('0W');
    expect(zeroNodes.length).toBeGreaterThanOrEqual(2);
    // No "-0W" from battery
    expect(screen.queryByText('-0W')).toBeNull();
  });
});
