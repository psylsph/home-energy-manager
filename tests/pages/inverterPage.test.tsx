/**
 * Coverage for InverterPage's `finiteAbs` swap on `battery_current`.
 *
 * The Gateway (DTC 0x70xx) sets `battery_current` to f32::NAN; serde_json
 * serialises NaN as null. Before the fix, `Math.abs(null)` coerced the
 * value to 0 *before* `formatCurrent`'s `Number.isFinite` guard could
 * fire, so the field rendered as '0.0A' instead of '—'. `finiteAbs`
 * preserves the null signal so the em-dash renders.
 *
 * Mirrors the matching BatteryPanel test in `tests/components/BatteryPanel.test.tsx`.
 */
import { describe, it, expect, beforeEach } from 'vitest';
import { render, cleanup } from '@testing-library/react';
import InverterPage from '../../src/pages/InverterPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    soc: 50,
    battery_state: 'idle',
    battery_power: 0,
    battery_voltage: 51.2,
    battery_current: -7.8,
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
    battery_modules: [],
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
    ...overrides,
  } as InverterSnapshot;
}

beforeEach(() => {
  cleanup();
  useInverterStore.setState({ snapshot: null, connectionState: 'connected' });
});

describe('<InverterPage/> Gateway null telemetry fields', () => {
  it('renders the battery current as an em-dash when it is null (Gateway)', () => {
    const gatewaySnapshot = makeSnapshot({
      battery_voltage: null as unknown as number,
      battery_current: null as unknown as number,
    });
    useInverterStore.setState({ snapshot: gatewaySnapshot });

    const { container } = render(<InverterPage />);
    const currentRow = Array.from(container.querySelectorAll('span.text-text-secondary'))
      .find((r) => r.textContent === 'Current');
    expect(currentRow?.nextElementSibling?.textContent).toBe('—');
  });

  it('renders the absolute battery current for a normal snapshot', () => {
    // battery_current: -7.8 (charging) → displayed as 7.8A (absolute).
    useInverterStore.setState({ snapshot: makeSnapshot() });
    const { container } = render(<InverterPage />);
    const currentRow = Array.from(container.querySelectorAll('span.text-text-secondary'))
      .find((r) => r.textContent === 'Current');
    expect(currentRow?.nextElementSibling?.textContent).toBe('7.8A');
  });
});

describe('<InverterPage/> Power row sign convention', () => {
  // InverterPage's Grid Power and Battery Power rows used to render the
  // signed snapshot value directly (-839W for discharging, -198W for
  // exporting). For consistency with BatteryPanel and the orbit diagram,
  // these are now plain positive magnitudes — the direction signal is
  // implicit (these are raw readouts on the technical Inverter page, but
  // the same value elsewhere in the app is positive, so changing this
  // site too avoids a confusing cross-page mismatch).
  function rowValue(container: HTMLElement, label: string): string {
    const labelEl = Array.from(container.querySelectorAll('span.text-text-secondary'))
      .find((r) => r.textContent === label);
    const value = labelEl?.nextElementSibling?.textContent;
    if (!value) throw new Error(`row not found: ${label}`);
    return value;
  }

  it('battery Power row shows plain positive magnitude when discharging', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ battery_state: 'discharging', battery_power: 839 }),
    });
    const { container } = render(<InverterPage />);
    expect(rowValue(container, 'Power')).toBe('839W');
  });

  it('battery Power row shows plain positive magnitude when charging', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ battery_state: 'charging', battery_power: -3100 }),
    });
    const { container } = render(<InverterPage />);
    expect(rowValue(container, 'Power')).toBe('3.1kW');
  });

  it('Grid Power row shows plain positive magnitude when exporting', () => {
    useInverterStore.setState({ snapshot: makeSnapshot({ grid_power: 198 }) });
    const { container } = render(<InverterPage />);
    expect(rowValue(container, 'Grid Power')).toBe('198W');
  });

  it('Grid Power row shows plain positive magnitude when importing', () => {
    useInverterStore.setState({ snapshot: makeSnapshot({ grid_power: -2200 }) });
    const { container } = render(<InverterPage />);
    expect(rowValue(container, 'Grid Power')).toBe('2.2kW');
  });
});
describe('<InverterPage/> charge/discharge rate scale', () => {
  function rowValue(container: HTMLElement, label: string): string {
    const labelEl = Array.from(container.querySelectorAll('span.text-text-secondary'))
      .find((r) => r.textContent === label);
    const value = labelEl?.nextElementSibling?.textContent;
    if (!value) throw new Error(`row not found: ${label}`);
    return value;
  }

  // Issue #346: HR 111/112 run 0-50 and the UI doubles them; HR 313/314 and
  // HR 1108/1110 already run 1-100. The Inverter page used to render the raw
  // register, so a DC-hybrid user who set 66% on the Control page saw "33%".
  it('doubles the half-scale register on a DC hybrid', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ device_type_code: '2201', charge_rate: 33, discharge_rate: 25 }),
    });
    const { container } = render(<InverterPage />);
    expect(rowValue(container, 'Charge Rate')).toBe('66%');
    expect(rowValue(container, 'Discharge Rate')).toBe('50%');
  });

  it('passes the direct register through on a three-phase model', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ device_type_code: '4001', charge_rate: 66, discharge_rate: 100 }),
    });
    const { container } = render(<InverterPage />);
    expect(rowValue(container, 'Charge Rate')).toBe('66%');
    expect(rowValue(container, 'Discharge Rate')).toBe('100%');
  });

  it('passes the direct register through on an AC-coupled model', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ device_type_code: '3001', charge_rate: 66 }),
    });
    const { container } = render(<InverterPage />);
    expect(rowValue(container, 'Charge Rate')).toBe('66%');
  });

  it('doubles the half-scale register on the Gen1 Hybrid the bug was reported on', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ device_type_code: '1001', charge_rate: 33, discharge_rate: 25 }),
    });
    const { container } = render(<InverterPage />);
    expect(rowValue(container, 'Charge Rate')).toBe('66%');
    expect(rowValue(container, 'Discharge Rate')).toBe('50%');
  });

  it('reads the Gen1 register through the pack size, matching the Control page', () => {
    // 9.5 kWh behind a 2600 W inverter (the reporter's setup): register 17 is
    // 17% of 9500 Wh = 1615 W = 62% of the inverter maximum, and anything above
    // register 27 already reaches it (GivTCP read.py).
    useInverterStore.setState({
      snapshot: makeSnapshot({
        device_type_code: '1001',
        battery_capacity_kwh: 9.5,
        max_battery_power_w: 2600,
        charge_rate: 17,
        discharge_rate: 50,
      }),
    });
    const { container } = render(<InverterPage />);
    expect(rowValue(container, 'Charge Rate')).toBe('62%');
    expect(rowValue(container, 'Discharge Rate')).toBe('100%');
  });

  it('passes the Gateway HR 313/314 register through unchanged', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ device_type_code: '7001', charge_rate: 66, discharge_rate: 40 }),
    });
    const { container } = render(<InverterPage />);
    expect(rowValue(container, 'Charge Rate')).toBe('66%');
    expect(rowValue(container, 'Discharge Rate')).toBe('40%');
  });

  it('clamps a corrupt half-scale register to 100%', () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ device_type_code: '2201', charge_rate: 200 }),
    });
    const { container } = render(<InverterPage />);
    expect(rowValue(container, 'Charge Rate')).toBe('100%');
  });
});
