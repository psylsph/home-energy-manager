import { describe, it, expect } from 'vitest';
import { render } from '@testing-library/react';
import BatteryPanel from '../../src/components/BatteryPanel';
import type { InverterSnapshot } from '../../src/lib/types';

function snapshot(): InverterSnapshot {
  return {
    soc: 52,
    battery_state: 'charging',
    battery_power: -400,
    battery_voltage: 51.2,
    battery_current: 7.8,
    battery_temperature: 19.5,
    battery_mode: 'eco',
    cosy_active: false,
    cosy_enabled: false,
    enable_charge: false,
    enable_discharge: false,
    charge_slots: [],
    discharge_slots: [],
    eps_power_w: 0,
    battery_reserve: 4,
    today_charge_kwh: 3.2,
    today_discharge_kwh: 1.1,
  } as InverterSnapshot;
}

describe('<BatteryPanel/> responsive gauge orientation', () => {
  it('uses a horizontal battery gauge on mobile and keeps the vertical gauge for sm+', () => {
    const { container } = render(<BatteryPanel snapshot={snapshot()} />);

    const horizontalGauge = container.querySelector('svg[data-orientation="horizontal"]');
    const verticalGauge = container.querySelector('svg[data-orientation="vertical"]');
    expect(horizontalGauge).not.toBeNull();
    expect(verticalGauge).not.toBeNull();
    expect(horizontalGauge?.parentElement?.className).toContain('sm:hidden');
    expect(verticalGauge?.parentElement?.className).toContain('hidden sm:block');
  });
});

describe('<BatteryPanel/> Gateway null telemetry fields', () => {
  // The Gateway (DTC 0x70xx) sets battery_voltage / battery_current to
  // f32::NAN; serde_json serialises NaN as null. Both must render '—',
  // but battery_current used to show '0.0A' because BatteryPanel wrapped
  // it in Math.abs() (which coerces null → 0) before formatCurrent's
  // finite-guard could fire.
  it('renders the battery current as an em-dash when it is null (Gateway)', () => {
    const gatewaySnapshot = {
      ...snapshot(),
      battery_voltage: null as unknown as number,
      battery_current: null as unknown as number,
    } as InverterSnapshot;
    const { container } = render(<BatteryPanel snapshot={gatewaySnapshot} />);

    // The Voltage and Current rows sit side-by-side in the grid. Find them
    // by their preceding label text and confirm both show '—'.
    const rows = Array.from(container.querySelectorAll('span.text-text-secondary'));
    const voltageRow = rows.find((r) => r.textContent === 'Voltage');
    const currentRow = rows.find((r) => r.textContent === 'Current');
    expect(voltageRow?.nextElementSibling?.textContent).toBe('—');
    expect(currentRow?.nextElementSibling?.textContent).toBe('—');
  });

  it('renders the absolute battery current for a normal snapshot', () => {
    // Sanity check: the finiteAbs change must not break the regular path.
    // battery_current: -7.8 (charging) → displayed as 7.8A (absolute).
    const { container } = render(<BatteryPanel snapshot={snapshot()} />);
    const rows = Array.from(container.querySelectorAll('span.text-text-secondary'));
    const currentRow = rows.find((r) => r.textContent === 'Current');
    expect(currentRow?.nextElementSibling?.textContent).toBe('7.8A');
  });
});

describe('<BatteryPanel/> Power row sign convention', () => {
  // The Power row used to render "-839W" alongside a "Discharging" badge,
  // which read as a sign-convention bug. The badge above the row carries
  // the direction signal; the value is the plain magnitude. Lock that
  // contract in here so future refactors can't quietly reintroduce the
  // "−" / "+" prefix.
  function powerRow(container: HTMLElement): string {
    const rows = Array.from(container.querySelectorAll('span.text-text-secondary'));
    const powerRow = rows.find((r) => r.textContent === 'Power');
    const value = powerRow?.nextElementSibling?.textContent;
    if (!value) throw new Error('Power row not found');
    return value;
  }

  it('discharging battery shows plain positive magnitude, no leading − or +', () => {
    const s = {
      ...snapshot(),
      battery_state: 'discharging' as const,
      battery_power: 839,
    } as InverterSnapshot;
    const { container } = render(<BatteryPanel snapshot={s} />);
    expect(powerRow(container)).toBe('839W');
  });

  it('charging battery shows plain positive magnitude, no leading +', () => {
    const s = {
      ...snapshot(),
      battery_state: 'charging' as const,
      battery_power: -3100,
    } as InverterSnapshot;
    const { container } = render(<BatteryPanel snapshot={s} />);
    expect(powerRow(container)).toBe('3.1kW');
  });

  it('idle battery shows plain 0W (no sign prefix on a zero value either)', () => {
    const s = {
      ...snapshot(),
      battery_state: 'idle' as const,
      battery_power: 0,
    } as InverterSnapshot;
    const { container } = render(<BatteryPanel snapshot={s} />);
    expect(powerRow(container)).toBe('0W');
  });
});
