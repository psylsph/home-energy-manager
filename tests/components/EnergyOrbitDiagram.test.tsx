import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, cleanup } from '@testing-library/react';
import EnergyOrbitDiagram from '../../src/components/EnergyOrbitDiagram';
import { FLOW_COLORS, BATTERY_OUTPUT_COLOR, socColor } from '../../src/lib/energyFlow';
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
    const rowEls = Array.from(card!.querySelectorAll('text'));
    const rows = rowEls.map((t) => t.textContent ?? '');
    expect(rows).toEqual(['Gen 3 Hybrid', '30.0°C', 'Eco']);
    expect(rowEls.map((t) => t.getAttribute('fill'))).toEqual([
      'var(--app-text-secondary, #8B949E)',
      'var(--app-text-primary, #F0F6FC)',
      'var(--app-text-primary, #F0F6FC)',
    ]);
    expect(rowEls.map((t) => t.getAttribute('font-weight'))).toEqual(['500', '700', '700']);
  });

  it('uses larger inverter mini-card text on mobile so model, temperature, and mode stay readable', () => {
    mockMatchMedia({ '(max-width: 767px)': true });

    const { container } = render(<EnergyOrbitDiagram snapshot={makeSnapshot()} />);
    const card = container.querySelector('[data-testid="inverter-mini-card"]');
    expect(card).not.toBeNull();

    const rows = Array.from(card!.querySelectorAll('text'));
    expect(rows.map((t) => t.getAttribute('font-size'))).toEqual(['15', '13', '13']);

    const pill = card!.querySelector('rect');
    expect(pill?.getAttribute('width')).toBe('164');
    expect(pill?.getAttribute('height')).toBe('56');
  });

  it('widens the inverter mini-card pill so the longest battery-mode label fits (issue: AC coupled + Timed Demand Discharging)', () => {
    // The pill background is the only <rect> inside the mini-card group.
    // Before the fix it was 144 viewBox units wide, which clipped
    // "Timed Demand (Discharging)" (25 chars at fontSize 11) on AC-coupled
    // inverters whose battery-mode string appended "(Discharging)".
    const { container } = render(
      <EnergyOrbitDiagram
        snapshot={makeSnapshot({
          device_type_display: 'AC Coupled',
          battery_mode: 'timed_demand',
          enable_discharge: true,
          discharge_slots: [
            { start_hour: 0, start_minute: 0, end_hour: 23, end_minute: 59, enabled: true },
          ],
          battery_state: 'discharging',
          battery_power: 1500,
          home_power: 800,
        })}
      />,
    );
    const card = container.querySelector('[data-testid="inverter-mini-card"]');
    expect(card).not.toBeNull();
    const pill = card!.querySelector('rect');
    expect(pill).not.toBeNull();

    const pillWidth = parseFloat(pill!.getAttribute('width') ?? '0');
    const pillX = parseFloat(pill!.getAttribute('x') ?? '0');
    // 144 was the old width — pin that we widened it. The upper bound keeps
    // the pill from dominating the bottom of the diagram; only the third
    // row ("Timed Demand (Discharging)") needs the extra room.
    expect(pillWidth).toBeGreaterThan(144);
    expect(pillWidth).toBeLessThanOrEqual(180);

    // The pill is centred on the SVG's CX — confirm so a future change to
    // `x={CX - ...}` does not accidentally drift the chip off-axis.
    const CX = 260;
    expect(pillX + pillWidth / 2).toBeCloseTo(CX, 5);

    // Sanity: the worst-case battery-mode label renders into the third row
    // — guard against a future refactor that drops the label altogether.
    const rows = Array.from(card!.querySelectorAll('text')).map((t) => t.textContent ?? '');
    expect(rows[2]).toBe('Timed Demand (Discharging)');
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

  // -------------------------------------------------------------------------
  // Battery SoC text contrast + battery-origin spoke colour (issue #170).
  //
  // The battery *node* keeps its SOC tier colour (red / amber / green) on
  // fill + ring + glyph body so the user can read charge state at a glance.
  // But two things must NOT follow that colour:
  //   1. the on-node SoC % text (same hue as fill → unreadable at every
  //      tier),
  //   2. the battery-origin spokes (charge / discharge / discharge_to_grid)
  //      — power throughput is independent of stored energy, just as the
  //      solar spokes ignore PV kW and the grid spoke is always red.
  // -------------------------------------------------------------------------

  it('battery SoC % text uses a contrast fill, not the SoC tier colour (issue #170)', () => {
    // The SoC % lives inside the battery satellite's text node. We pin the
    // SVG <text> element that contains "47%" and assert its `fill` attr is
    // a fixed contrast colour, NOT the amber that the node is filled with
    // (and NOT the red / green of the other SoC tiers).
    const { container } = render(
      <EnergyOrbitDiagram
        snapshot={makeSnapshot({ soc: 47, battery_state: 'discharging', battery_power: 1400, home_power: 800 })}
      />,
    );
    const allText = Array.from(container.querySelectorAll('text'));
    const socText = allText.find((t) => (t.textContent ?? '').trim() === '47%');
    expect(socText).toBeDefined();
    const fill = socText!.getAttribute('fill') ?? '';
    // The fix lands the fill on --app-text-primary (a fixed contrast colour).
    // The previous (buggy) behaviour filled this text with node.color, which
    // for soc=47 was #F59E0B amber — identical to the node fill at 16 % →
    // unreadable.
    expect(fill).not.toBe(socColor(47));
    expect(fill).not.toBe('');
    // Negative-direction check (guard against accidentally restoring the bug).
    expect(fill).not.toBe(FLOW_COLORS.battery);
  });

  it('battery SoC % text contrast hold at every SoC tier (issue #170)', () => {
    // Pin the contract: the SoC % text fill must NOT equal any of the three
    // SOC tier colours. The node fill / ring / glyph may stay tier-coloured;
    // the on-node percent text must stay on a fixed contrast colour.
    for (const soc of [10, 30, 80]) {
      const { container } = render(
        <EnergyOrbitDiagram snapshot={makeSnapshot({ soc })} />,
      );
      const text = Array.from(container.querySelectorAll('text')).find(
        (t) => (t.textContent ?? '').trim() === `${soc}%`,
      );
      expect(text, `soc=${soc}: SoC text element missing`).toBeDefined();
      const fill = text!.getAttribute('fill') ?? '';
      expect(fill, `soc=${soc}: must not match its own tier colour`).not.toBe(socColor(soc));
    }
  });

  it('solar_charge spoke track (solar covers all of charge) is solar yellow (issue #170)', () => {
    // With solar 5 kW generating and battery charging 241 W, all of the
    // charge is attributed to solar. The emitted spoke is `solar_charge`
    // (solar → battery, yellow). The aggregate `charge` flow is
    // suppressed when the split covers the whole of the battery charge.
    const { container } = render(
      <EnergyOrbitDiagram
        snapshot={makeSnapshot({
          solar_power: 5000,
          home_power: 500,
          battery_power: -241,
          battery_state: 'charging',
          soc: 18,
        })}
      />,
    );
    const solarChargeTrack = container.querySelector('[data-flow-track-id="solar_charge"]');
    expect(solarChargeTrack, 'solar_charge track missing').not.toBeNull();
    expect(solarChargeTrack?.getAttribute('stroke')).toBe(FLOW_COLORS.solar);
    expect(solarChargeTrack?.getAttribute('stroke')).not.toBe(socColor(18));
    // Suppressed aggregate `charge` flow — the split replaces it.
    expect(container.querySelector('[data-flow-track-id="charge"]')).toBeNull();
  });

  it('grid_charge line and ball are red when the battery is drawing from grid', () => {
    const { container } = render(
      <EnergyOrbitDiagram
        snapshot={makeSnapshot({
          grid_power: -2000,
          home_power: 500,
          battery_power: -1500,
          battery_state: 'charging',
          soc: 80,
        })}
      />,
    );
    const gridChargeTrack = container.querySelector('[data-flow-track-id="grid_charge"]');
    const gridChargeDot = container.querySelector('[data-flow-id="grid_charge"] circle');
    expect(gridChargeTrack, 'grid_charge track missing').not.toBeNull();
    expect(gridChargeTrack?.getAttribute('stroke')).toBe(FLOW_COLORS.grid);
    expect(gridChargeDot?.getAttribute('fill')).toBe(FLOW_COLORS.grid);
    expect(gridChargeTrack?.getAttribute('stroke')).not.toBe(BATTERY_OUTPUT_COLOR);
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
    expect(container.querySelector('[data-flow-id="solar_charge"]')?.getAttribute('data-route')).toBe('outer');
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

    // Per issue #170: when solar covers the battery charge, the visible
    // spoke is the `solar_charge` (solar → battery) outer arc, yellow per
    // the "Solar to everywhere always yellow / amber" rule. The aggregate
    // `charge` flow is no longer emitted (the split replaces it).
    const solarChargeTrack = container.querySelector('[data-flow-track-id="solar_charge"]');
    expect(solarChargeTrack).not.toBeNull();
    expect(solarChargeTrack?.getAttribute('data-route')).toBe('outer');
    expect(solarChargeTrack?.getAttribute('stroke')).toBe(FLOW_COLORS.solar);
    expect(solarChargeTrack?.getAttribute('stroke')).not.toBe(BATTERY_OUTPUT_COLOR);
    expect(solarChargeTrack?.getAttribute('stroke')).not.toBe(FLOW_COLORS.home);
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
    // Battery-origin spokes use the battery-output green (BATTERY_OUTPUT_COLOR)
    // at every SOC tier — power throughput is independent of stored charge
    // (issue #170). The SOC tier colour is reserved for the battery *node*.
    expect(container.querySelector('[data-flow-track-id="discharge"]')?.getAttribute('stroke')).toBe(BATTERY_OUTPUT_COLOR);
    expect(container.querySelector('[data-flow-track-id="discharge_to_grid"]')?.getAttribute('stroke')).toBe(BATTERY_OUTPUT_COLOR);
  });

  it('battery→grid spoke stays on battery-output green under reduced motion (issue #170)', () => {
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
    // 10 % SOC is a red tier — yet the battery-origin spoke must NOT flip red;
    // it stays on the battery-output green regardless of stored charge.
    expect(container.querySelector('[data-flow-id="discharge"]')?.getAttribute('fill')).toBe(BATTERY_OUTPUT_COLOR);
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

  it('pins the slower base ball speed so a future tweak does not undo the calmer animation (issue #170)', () => {
    // The user asked for all balls to slow down by 50 % after the new
    // spoke-colour work was merged in. BASE_SPEED_PX_PER_S was halved
    // from 90 → 45, so a simple low-energy spoke traverses roughly twice
    // as slow as before. This test pins the new base speed by asserting
    // the per-flow duration: a ~140 px spoke at strength=0 emits a
    // 1-second clamp (the documented floor) under the new regime, while
    // the old 90 px/s regime would have produced a much faster spoke
    // (well under 1 s).
    //
    // The current compact spoke is ~87 px; at 45 px/s and full strength it
    // traverses in about 1.33 s, while the old 90 px/s regime would have hit
    // the 1 s floor. This keeps the test tied to the calmer post-#170 speed
    // without depending on the older, longer spoke geometry.
    const { container } = render(
      <EnergyOrbitDiagram
        snapshot={makeSnapshot({
          // One low-strength flow so it doesn't hit the 8 s clamp.
          // Solar 50 W, home 50 W → strength=1 for the single solar
          // spoke (maxFlowWatts=50), so speed = 45 × (0.55 + 0.9) ≈ 65
          // px/s. A ~140 px spoke ≈ 2.15 s.
          solar_power: 50,
          home_power: 50,
        })}
      />,
    );
    const dur = parseFloat(
      container.querySelector('[data-flow-id="solar"]')?.getAttribute('data-duration') ?? '0',
    );
    expect(dur).toBeGreaterThan(1.2);
    expect(dur).toBeLessThan(8.0);
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
    // (strength=1) so it should traverse faster than the 1 kW solar_charge
    // flow (strength=0.2) — same intent as the original test, but
    // comparing flows in a single render so the result reflects relative
    // strength. Note: the battery charging flows are now source-attributed
    // (solar_charge) rather than the synthetic `charge` flow (issue #170).
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
      container.querySelector('[data-flow-id="solar_charge"]')?.getAttribute('data-duration') ?? '0',
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

    // --- issue #151: physical cable state (HR 2 `connection_status`) shown
    // under the kW value as "Cable In" / "No Cable". Independent of the
    // operational-status word, and only shown while we have a fresh frame.
    it('renders "Cable In" under the EV kW value when a cable is plugged in (issue #151)', () => {
      useInverterStore.setState({ showFlowStatusWords: true });
      const { container } = render(
        <EnergyOrbitDiagram
          snapshot={makeSnapshot({ home_power: 7500 })}
          showEvc
          evcPower={7047}
          evcCharging
          evcConnected
          evcCableConnected
        />,
      );
      // The status word still reflects power flow; the cable lives on its
      // own sub-label line.
      const labels = Array.from(container.querySelectorAll('text')).map((t) => t.textContent ?? '');
      expect(labels).toContain('Charging');
      const sub = container.querySelector('[data-testid="ev-sublabel"]');
      expect(sub).not.toBeNull();
      expect(sub?.textContent).toBe('Cable In');
    });

    it('renders "No Cable" when reachable with the cable unplugged (issue #151)', () => {
      // state=2 (Connected), conn=0 — charger reachable but no cable.
      useInverterStore.setState({ showFlowStatusWords: true });
      const { container } = render(
        <EnergyOrbitDiagram
          snapshot={makeSnapshot()}
          showEvc
          evcPower={0}
          evcConnected
          evcCableConnected={false}
        />,
      );
      const sub = container.querySelector('[data-testid="ev-sublabel"]');
      expect(sub).not.toBeNull();
      expect(sub?.textContent).toBe('No Cable');
    });

    it('omits the cable sub-label for an unreachable / never-reached charger (issue #151)', () => {
      // Never reached: evcConnected=false → the diagram can't honestly see
      // the cable, so it asserts nothing.
      useInverterStore.setState({ showFlowStatusWords: true });
      const { container } = render(
        <EnergyOrbitDiagram snapshot={makeSnapshot()} showEvc evcPower={0} />,
      );
      expect(container.querySelector('[data-testid="ev-sublabel"]')).toBeNull();
    });

    // --- issue #189: session energy (kWh) inline with power ---
    // The kWh running total renders inline with the live power as
    // `7.7kW(23kWh)`. The backend SessionLatch keeps the value visible
    // after a session ends; these tests pin the rendering.
    it('renders the session kWh inline with the power while charging (issue #189)', () => {
      useInverterStore.setState({ showFlowStatusWords: true });
      const { container } = render(
        <EnergyOrbitDiagram
          snapshot={makeSnapshot({ home_power: 7500 })}
          showEvc
          evcPower={7000}
          evcCharging
          evcConnected
          evcCableConnected
          evcSessionEnergyKwh={23}
        />,
      );
      const labels = Array.from(container.querySelectorAll('text')).map((t) => t.textContent ?? '');
      // Power + energy inline (23 kWh crosses the integer threshold);
      // cable state stays on its own sub-label line.
      expect(labels).toContain('7.0kW(23kWh)');
      expect(container.querySelector('[data-testid="ev-sublabel"]')?.textContent).toBe('Cable In');
    });

    it('renders the latched session kWh after the session ends, even at 0 W (issue #189)', () => {
      // Charger idle, cable still in, but the backend latch is holding the
      // completed session's total. The kWh stays inline: `0W(7.5kWh)`.
      useInverterStore.setState({ showFlowStatusWords: true });
      const { container } = render(
        <EnergyOrbitDiagram
          snapshot={makeSnapshot()}
          showEvc
          evcPower={0}
          evcConnected
          evcCableConnected
          evcSessionEnergyKwh={7.5}
        />,
      );
      const labels = Array.from(container.querySelectorAll('text')).map((t) => t.textContent ?? '');
      expect(labels).toContain('0W(7.5kWh)');
    });

    it('uses a smaller value font for the EV node so the longer `kW(kWh)` string fits', () => {
      // The EV value (`7.0kW(23kWh)`) is much longer than the other nodes'
      // bare kW readings, so it renders at a reduced font size to avoid
      // crowding the satellite. Pin the size so a future tweak doesn't
      // accidentally restore the full-size value and overflow the node.
      useInverterStore.setState({ showFlowStatusWords: true });
      const { container } = render(
        <EnergyOrbitDiagram
          snapshot={makeSnapshot({ home_power: 7500 })}
          showEvc
          evcPower={7000}
          evcCharging
          evcConnected
          evcCableConnected
          evcSessionEnergyKwh={23}
        />,
      );
      const evValue = Array.from(container.querySelectorAll('text')).find(
        (t) => (t.textContent ?? '') === '7.0kW(23kWh)',
      );
      expect(evValue).toBeDefined();
      expect(evValue!.getAttribute('font-size')).toBe('15');
    });

    it('omits the kWh suffix when the session total is zero (no energy delivered yet)', () => {
      // Brand-new session or never charged: kWh reads 0 → bare power value,
      // no `(0.0kWh)` suffix.
      useInverterStore.setState({ showFlowStatusWords: true });
      const { container } = render(
        <EnergyOrbitDiagram
          snapshot={makeSnapshot()}
          showEvc
          evcPower={0}
          evcConnected
          evcCableConnected
          evcSessionEnergyKwh={0}
        />,
      );
      const labels = Array.from(container.querySelectorAll('text')).map((t) => t.textContent ?? '');
      expect(labels).toContain('0W');
      expect(labels).not.toContain('0W(0.0kWh)');
      expect(container.querySelector('[data-testid="ev-sublabel"]')?.textContent).toBe('Cable In');
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
