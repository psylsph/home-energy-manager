import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, cleanup } from '@testing-library/react';
import EnergyOrbitDiagram from '../../src/components/EnergyOrbitDiagram';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

// jsdom has no matchMedia — useIsMobile and the reduced-motion check both
// call it. Provide a controllable mock (default: not mobile, motion allowed).
function mockMatchMedia(matches: Record<string, boolean> = {}) {
  window.matchMedia = vi.fn().mockImplementation((query: string) => ({
    matches: matches[query] ?? false,
    media: query,
    onchange: null,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    addListener: vi.fn(),
    removeListener: vi.fn(),
    dispatchEvent: vi.fn(),
  }));
}

// Minimal snapshot covering every field buildEnergyFlows reads. Typed so the
// literal is checked structurally (no `as` cast — oxc's tsx parser rejects
// the `} as Type` form here).
const BASE_SNAPSHOT: InverterSnapshot = {
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
};

function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return { ...BASE_SNAPSHOT, ...overrides };
}

function resetStore(threshold = 20) {
  useInverterStore.setState({ visualNoiseThreshold: threshold });
}

beforeEach(() => {
  cleanup();
  resetStore(20);
  mockMatchMedia();
});

describe('EnergyOrbitDiagram', () => {
  it('renders the plain-English summary sentence', () => {
    const { container } = render(
      <EnergyOrbitDiagram snapshot={makeSnapshot({ solar_power: 3000, home_power: 500 })} />,
    );
    // The summary paragraph is the <p> with the max-w-md class.
    const summary = container.querySelector('p.max-w-md');
    expect(summary?.textContent).toContain('Solar is powering the home');
  });

  it('renders the inverter as a supporting mini-card, not a hub node', () => {
    const { container } = render(<EnergyOrbitDiagram snapshot={makeSnapshot()} />);
    const inverterLine = container.querySelector('p.text-xs.text-center');
    expect(inverterLine?.textContent).toContain('Inverter');
    expect(inverterLine?.textContent).toContain('Gen 3 Hybrid');
  });

  it('draws no flow spokes when everything is below the noise threshold', () => {
    const { container } = render(<EnergyOrbitDiagram snapshot={makeSnapshot()} />);
    // Each active spoke renders one direction arrow (polygon).
    expect(container.querySelectorAll('polygon').length).toBe(0);
  });

  it('draws a spoke per active flow (solar→home)', () => {
    const { container } = render(
      <EnergyOrbitDiagram snapshot={makeSnapshot({ solar_power: 5000, home_power: 500 })} />,
    );
    // solar→home is the only active flow → exactly one arrow.
    expect(container.querySelectorAll('polygon').length).toBe(1);
  });

  it('gates flows by the store noise threshold', () => {
    resetStore(150);
    const { container } = render(
      <EnergyOrbitDiagram snapshot={makeSnapshot({ solar_power: 100, home_power: 100 })} />,
    );
    // 100W is below the 150W threshold → no spokes.
    expect(container.querySelectorAll('polygon').length).toBe(0);
  });

  it('shows export as a negative grid value and import as the "Import" label', () => {
    const exporting = render(<EnergyOrbitDiagram snapshot={makeSnapshot({ grid_power: 4300 })} />);
    const gridValue = Array.from(exporting.container.querySelectorAll('text')).map(
      (t) => t.textContent ?? '',
    );
    expect(gridValue.some((v) => v.startsWith('-'))).toBe(true);

    cleanup();
    const importing = render(<EnergyOrbitDiagram snapshot={makeSnapshot({ grid_power: -2000 })} />);
    const impValues = Array.from(importing.container.querySelectorAll('text')).map(
      (t) => t.textContent ?? '',
    );
    expect(impValues).toContain('Import');
  });

  describe('EV charger node', () => {
    it('is omitted when the charger is not configured', () => {
      const { container } = render(
        <EnergyOrbitDiagram snapshot={makeSnapshot()} showEvc={false} evcPower={7000} />,
      );
      const labels = Array.from(container.querySelectorAll('text')).map((t) => t.textContent ?? '');
      expect(labels).not.toContain('EV');
    });

    it('appears and draws a home→ev spoke when charging', () => {
      const { container } = render(
        <EnergyOrbitDiagram
          snapshot={makeSnapshot({ home_power: 7500 })}
          showEvc
          evcPower={7000}
          evcCharging
          evcConnected
        />,
      );
      const labels = Array.from(container.querySelectorAll('text')).map((t) => t.textContent ?? '');
      expect(labels).toContain('EV');
      expect(labels).toContain('Charging');
      expect(container.querySelectorAll('polygon').length).toBeGreaterThanOrEqual(1);
    });

    it('shows "Not Found" for a configured-but-unreachable charger (issue #138)', () => {
      const { container } = render(
        <EnergyOrbitDiagram snapshot={makeSnapshot()} showEvc evcPower={0} />,
      );
      const labels = Array.from(container.querySelectorAll('text')).map((t) => t.textContent ?? '');
      expect(labels).toContain('Not Found');
    });
  });

  it('disables spoke animation under prefers-reduced-motion', () => {
    mockMatchMedia({ '(prefers-reduced-motion: reduce)': true });
    const { container } = render(
      <EnergyOrbitDiagram snapshot={makeSnapshot({ solar_power: 5000, home_power: 500 })} />,
    );
    // The animate element is conditionally omitted for reduced-motion users.
    expect(container.querySelectorAll('animate').length).toBe(0);
  });
});
