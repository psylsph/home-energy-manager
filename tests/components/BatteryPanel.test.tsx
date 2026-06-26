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
