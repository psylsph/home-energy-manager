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