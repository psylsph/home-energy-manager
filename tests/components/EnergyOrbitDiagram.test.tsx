import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, cleanup } from '@testing-library/react';
import EnergyOrbitDiagram from '../../src/components/EnergyOrbitDiagram';
import { FLOW_COLORS, socColor } from '../../src/lib/energyFlow';
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
  // `showFlowSummary` (the plain-English overview sentence) was removed in
  // commit 22fe1b8 — the diagram no longer reads it — so it's not reset here.
  useInverterStore.setState({
    showFlowStatusWords: false,
    visualNoiseThreshold: threshold,
  });
}

beforeEach(() => {
  cleanup();
  resetStore(20);
  mockMatchMedia();
});

describe('EnergyOrbitDiagram', () => {
  it('exposes a plain-English flow description via the SVG accessible label', () => {
    // The visible overview sentence was removed (commit 22fe1b8), but the
    // diagram still describes the live flow for screen readers through the
    // SVG's role="img" aria-label — now the only place the human-readable
    // summary lives, so pin it.
    const { container } = render(
      <EnergyOrbitDiagram snapshot={makeSnapshot({ solar_power: 3000, home_power: 500 })} />,
    );
    expect(container.querySelector('svg')?.getAttribute('aria-label')).toContain(
      'Solar is powering the home',
    );
  });

  it('renders inverter model, temperature, and battery mode as a supporting mini-card', () => {
    const { container } = render(<EnergyOrbitDiagram snapshot={makeSnapshot()} />);
    // The mini-card moved inside the SVG (commit 22fe1b8) as a translucent
    // pill with three text rows: model, inverter temperature, battery mode.
    const card = container.querySelector('[data-testid="inverter-mini-card"]');
    expect(card).not.toBeNull();
    expect(card?.getAttribute('aria-label')).toBe('Inverter: Gen 3 Hybrid, 30.0°C, Eco');
    const rows = Array.from(card!.querySelectorAll('text')).map((t) => t.textContent ?? '');
    expect(rows).toEqual(['Gen 3 Hybrid', '30.0°C', 'Eco']);
  });

  it('does not crash on a Gateway snapshot with null telemetry fields', () => {
    // The Gateway (DTC 0x70xx) doesn't expose inverter/battery temperature or
    // PV voltage; the backend sends f32::NAN which serde_json serialises as
    // null. The old diagram used formatTemp (finite-guarded); the radial
    // rewrite called .toFixed directly and crashed on launch for Gateway
    // users ("Cannot read properties of null (reading 'toFixed')").
    const gatewaySnapshot = makeSnapshot({
      inverter_temperature: null as unknown as number,
      battery_temperature: null as unknown as number,
      battery_voltage: null as unknown as number,
      battery_current: null as unknown as number,
      pv1_voltage: null as unknown as number,
      pv2_voltage: null as unknown as number,
      device_type_display: 'Gateway',
    });
    const { container } = render(<EnergyOrbitDiagram snapshot={gatewaySnapshot} />);
    // The mini-card shows an em-dash instead of the temperature (formatTemp is
    // NaN/null-safe), and the SVG renders without throwing.
    const card = container.querySelector('[data-testid="inverter-mini-card"]');
    expect(card).not.toBeNull();
    const rows = Array.from(card!.querySelectorAll('text')).map((t) => t.textContent ?? '');
    expect(rows[1]).toBe('—');
    expect(container.querySelector('svg')).not.toBeNull();
  });

  it('uses a shorter SVG viewBox when node status words are hidden so the inverter line sits higher', () => {
    const compact = render(<EnergyOrbitDiagram snapshot={makeSnapshot()} />);
    expect(compact.container.querySelector('svg')?.getAttribute('viewBox')).toBe('0 0 520 480');

    cleanup();
    useInverterStore.setState({ showFlowStatusWords: true });
    const expanded = render(<EnergyOrbitDiagram snapshot={makeSnapshot()} />);
    expect(expanded.container.querySelector('svg')?.getAttribute('viewBox')).toBe('0 0 520 520');
  });

  it('draws no moving flow dots when everything is below the noise threshold', () => {
    const { container } = render(<EnergyOrbitDiagram snapshot={makeSnapshot()} />);
    expect(container.querySelectorAll('[data-flow-id]').length).toBe(0);
  });

  it('draws a moving flow dot per active flow (solar→home)', () => {
    const { container } = render(
      <EnergyOrbitDiagram snapshot={makeSnapshot({ solar_power: 5000, home_power: 500 })} />,
    );
    expect(container.querySelectorAll('[data-flow-id]').length).toBe(1);
  });

  it('gates flows by the store noise threshold', () => {
    resetStore(150);
    const { container } = render(
      <EnergyOrbitDiagram snapshot={makeSnapshot({ solar_power: 100, home_power: 100 })} />,
    );
    // 100W is below the 150W threshold → no active dots.
    expect(container.querySelectorAll('[data-flow-id]').length).toBe(0);
  });

  it('shows the direction signal as a status word under the node and the magnitude as a plain positive value when status words are on', () => {
    useInverterStore.setState({ showFlowStatusWords: true });
    const exporting = render(<EnergyOrbitDiagram snapshot={makeSnapshot({ grid_power: 4300 })} />);
    const exportValues = Array.from(exporting.container.querySelectorAll('text')).map(
      (t) => t.textContent ?? '',
    );
    // Magnitude is plain (no `-` prefix); direction lives in the status word
    // below the node so the two never conflict.
    expect(exportValues).toContain('4.3kW');
    expect(exportValues).toContain('Exporting');

    cleanup();
    const importing = render(<EnergyOrbitDiagram snapshot={makeSnapshot({ grid_power: -2000 })} />);
    const impValues = Array.from(importing.container.querySelectorAll('text')).map(
      (t) => t.textContent ?? '',
    );
    expect(impValues).toContain('2.0kW');
    expect(impValues).toContain('Importing');
  });

  it('hides node status words by default and shows them when enabled', () => {
    // resetStore() in beforeEach forces showFlowStatusWords=false so this
    // test exercises the OFF path. The default in the live app is ON (see
    // loadShowFlowStatusWords), covered in settingsPageNodeStatusWords.
    const hidden = render(
      <EnergyOrbitDiagram snapshot={makeSnapshot({ grid_power: 4300 })} />,
    );
    let labels = Array.from(hidden.container.querySelectorAll('text')).map((t) => t.textContent ?? '');
    expect(labels).not.toContain('Exporting');

    cleanup();
    useInverterStore.setState({ showFlowStatusWords: true });
    const shown = render(
      <EnergyOrbitDiagram snapshot={makeSnapshot({ grid_power: 4300 })} />,
    );
    labels = Array.from(shown.container.querySelectorAll('text')).map((t) => t.textContent ?? '');
    expect(labels).toContain('Exporting');
  });

  it('renders the reference-style outer orbit ring and battery SOC ring/glyph', () => {
    useInverterStore.setState({ showFlowStatusWords: true });
    const { container } = render(
      <EnergyOrbitDiagram snapshot={makeSnapshot({ soc: 31, battery_power: 1400, battery_state: 'discharging', home_power: 800 })} />,
    );
    const orbitRing = container.querySelector('[data-testid="energy-orbit-ring"]');
    expect(orbitRing).not.toBeNull();
    expect(orbitRing?.getAttribute('mask')).toContain('url(#');
    expect(container.querySelector('[data-testid="battery-soc-ring"]')).not.toBeNull();
    expect(container.querySelector('[data-testid="battery-soc-ring-track"]')?.getAttribute('stroke')).toBe(socColor(31));
    expect(container.querySelector('[data-testid="battery-soc-ring-track"]')?.getAttribute('stroke-opacity')).toBe('0.2');
    expect(container.querySelector('[data-testid="battery-glyph"]')).not.toBeNull();
    expect(container.querySelector('[data-node-body="battery"]')?.getAttribute('fill')).toBe(socColor(31));
    expect(container.querySelector('[data-node-body="battery"]')?.getAttribute('fill-opacity')).toBe('0.16');
    expect(container.querySelector('[data-testid="battery-glyph-body"]')?.getAttribute('fill')).toBe(socColor(31));
    expect(container.querySelector('[data-testid="battery-glyph-body"]')?.getAttribute('fill-opacity')).toBe('0.16');
    expect(container.querySelector('[data-testid="battery-glyph-fill"]')?.getAttribute('fill')).toBe(socColor(31));
    const labels = Array.from(container.querySelectorAll('text')).map((t) => t.textContent ?? '');
    expect(labels).toContain('31%');
    // Magnitude is plain; direction lives in the "Discharging" status word.
    expect(labels).toContain('1.4kW');
    expect(labels).toContain('Discharging');
  });

  it('renders the solar V/A sub-label under the kW value (legacy behaviour)', () => {
    // Live PV voltage + current → "350.4V/6.5A" rendered as a small grey
    // sub-label between the kW value and the optional status word.
    const { container } = render(
      <EnergyOrbitDiagram
        snapshot={makeSnapshot({
          solar_power: 5000,
          home_power: 500,
          pv1_voltage: 350.4,
          pv1_current: 5.2,
          pv2_current: 1.3,
        })}
      />,
    );
    const sub = container.querySelector('[data-testid="solar-sublabel"]');
    expect(sub).not.toBeNull();
    expect(sub?.textContent).toBe('350.4V/6.5A');
  });

  it('falls back to a current-only sub-label when PV voltage is zero (gateway-style)', () => {
    const { container } = render(
      <EnergyOrbitDiagram
        snapshot={makeSnapshot({
          solar_power: 5000,
          home_power: 500,
          pv1_voltage: 0,
          pv1_current: 5.2,
          pv2_current: 1.3,
        })}
      />,
    );
    const sub = container.querySelector('[data-testid="solar-sublabel"]');
    expect(sub?.textContent).toBe('6.5A');
  });

  it('routes excess solar to battery and grid around the outer orbit', () => {
    const { container } = render(
      <EnergyOrbitDiagram
        snapshot={makeSnapshot({
          solar_power: 5000,
          home_power: 500,
          battery_power: -241,
          battery_state: 'charging',
          grid_power: 4300,
        })}
      />,
    );
    expect(container.querySelector('[data-flow-id="charge"]')?.getAttribute('data-route')).toBe('outer');
    expect(container.querySelector('[data-flow-id="export"]')?.getAttribute('data-route')).toBe('outer');
    expect(container.querySelector('[data-flow-id="solar"]')?.getAttribute('data-route')).toBe('direct');
  });

  it('draws source-coloured active tracks for direct spokes and outer routes', () => {
    const { container } = render(
      <EnergyOrbitDiagram
        snapshot={makeSnapshot({
          solar_power: 5000,
          home_power: 500,
          battery_power: -241,
          battery_state: 'charging',
          grid_power: 4300,
        })}
      />,
    );

    const solarTrack = container.querySelector('[data-flow-track-id="solar"]');
    expect(solarTrack).not.toBeNull();
    expect(solarTrack?.getAttribute('data-route')).toBe('direct');
    expect(solarTrack?.getAttribute('stroke')).toBe(FLOW_COLORS.solar);

    const chargeTrack = container.querySelector('[data-flow-track-id="charge"]');
    expect(chargeTrack).not.toBeNull();
    expect(chargeTrack?.getAttribute('data-route')).toBe('outer');
    expect(chargeTrack?.getAttribute('stroke')).toBe(FLOW_COLORS.solar);
  });

  it('draws a battery→grid dot when discharge exceeds the house load (issue #155)', () => {
    // Battery 2 kW discharging, house 500 W. The battery outflow exceeds the
    // house load, so a battery→grid dot is drawn for the excess (1.5 kW),
    // matching the GivEnergy app.
    const { container } = render(
      <EnergyOrbitDiagram
        snapshot={makeSnapshot({
          home_power: 500,
          battery_power: 2000,
          battery_state: 'discharging',
        })}
      />,
    );
    const discharge = container.querySelector('[data-flow-id="discharge"]');
    const toGrid = container.querySelector('[data-flow-id="discharge_to_grid"]');
    expect(discharge).not.toBeNull();
    expect(discharge?.getAttribute('data-route')).toBe('direct');
    expect(toGrid).not.toBeNull();
    expect(toGrid?.getAttribute('data-route')).toBe('outer');
    expect(container.querySelector('[data-flow-track-id="discharge"]')?.getAttribute('stroke')).toBe(socColor(50));
  });

  it('uses the current SOC colour for battery-origin moving balls under reduced motion', () => {
    mockMatchMedia({ '(prefers-reduced-motion: reduce)': true });
    const { container } = render(
      <EnergyOrbitDiagram
        snapshot={makeSnapshot({
          soc: 10,
          home_power: 500,
          battery_power: 2000,
          battery_state: 'discharging',
        })}
      />,
    );
    expect(container.querySelector('[data-flow-id="discharge"]')?.getAttribute('fill')).toBe(socColor(10));
  });

  it('does not draw a battery→grid dot when discharge is fully consumed by the house (issue #155)', () => {
    // Battery 500 W discharging, house 800 W. Battery outflow is smaller than
    // the house load — no export, no battery→grid dot.
    const { container } = render(
      <EnergyOrbitDiagram
        snapshot={makeSnapshot({
          home_power: 800,
          battery_power: 500,
          battery_state: 'discharging',
        })}
      />,
    );
    expect(container.querySelector('[data-flow-id="discharge"]')).not.toBeNull();
    expect(container.querySelector('[data-flow-id="discharge_to_grid"]')).toBeNull();
  });

  it('keys dot speed on path length so long arcs do not race', () => {
    // Battery 2 kW, house 200 W: a short battery→home spoke AND a long
    // battery→grid outer arc are both drawn. The arc length (~¼ orbit,
    // ~317 px) is ~2.3× the spoke length (~140 px), so the arc must take
    // a proportionally longer duration — not the same speed as the spoke.
    const { container } = render(
      <EnergyOrbitDiagram
        snapshot={makeSnapshot({
          home_power: 200,
          battery_power: 2000,
          battery_state: 'discharging',
        })}
      />,
    );
    const spokeDur = parseFloat(
      container.querySelector('[data-flow-id="discharge"]')?.getAttribute('data-duration') ?? '0',
    );
    const arcDur = parseFloat(
      container.querySelector('[data-flow-id="discharge_to_grid"]')?.getAttribute('data-duration') ?? '0',
    );
    expect(arcDur).toBeGreaterThan(spokeDur);
    expect(spokeDur).toBeGreaterThanOrEqual(1.0);
    expect(spokeDur).toBeLessThanOrEqual(8.0);
    expect(arcDur).toBeGreaterThanOrEqual(1.0);
    expect(arcDur).toBeLessThanOrEqual(8.0);
  });

  it('moves higher-energy balls faster than lower-energy balls in the same render', () => {
    // Solar 5 kW + battery charging at 1 kW. The solar flow is the biggest
    // (strength=1) so it should traverse faster than the 1 kW charge flow
    // (strength=0.2) — same intent as the original test, but comparing
    // flows in a single render so the result reflects relative strength.
    const { container } = render(
      <EnergyOrbitDiagram
        snapshot={makeSnapshot({
          solar_power: 5000,
          home_power: 4000,
          battery_power: -1000,
          battery_state: 'charging',
        })}
      />,
    );
    const solarDur = parseFloat(
      container.querySelector('[data-flow-id="solar"]')?.getAttribute('data-duration') ?? '0',
    );
    const chargeDur = parseFloat(
      container.querySelector('[data-flow-id="charge"]')?.getAttribute('data-duration') ?? '0',
    );
    expect(solarDur).toBeLessThan(chargeDur);
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
      useInverterStore.setState({ showFlowStatusWords: true });
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
      expect(labels).not.toContain('EV');
      const evIcon = container.querySelector('[data-testid="ev-car-icon"]');
      expect(evIcon).not.toBeNull();
      expect(evIcon?.getAttribute('transform')).toContain('scale(2.55)');
      expect(labels).toContain('Charging');
      expect(container.querySelectorAll('[data-flow-id]').length).toBeGreaterThanOrEqual(1);
    });

    it('shows "Not Found" for a configured-but-unreachable charger (issue #138)', () => {
      useInverterStore.setState({ showFlowStatusWords: true });
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
    // The animateMotion element is conditionally omitted for reduced-motion users.
    expect(container.querySelectorAll('animateMotion').length).toBe(0);
  });
});
