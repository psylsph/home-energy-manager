import { describe, expect, it } from 'vitest';
import {
  buildSystemAlerts,
  hasGridFault,
  hasInverterTemperatureAlert,
} from '../../src/lib/gridFault';
import type { InverterSnapshot } from '../../src/lib/types';

function snap(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    grid_loss: false,
    grid_online: true,
    inverter_trip: false,
    battery_over_temp: false,
    inverter_temperature: 30,
    soc: 80,
    ...overrides,
  } as InverterSnapshot;
}

const tempConfig = { inverter_temp_min: 8, inverter_temp_max: 60 };

describe('grid/system alert helpers', () => {
  it('keeps grid loss separate from inverter trip and battery warnings', () => {
    expect(hasGridFault(snap({ inverter_trip: true }))).toBe(false);
    expect(hasGridFault(snap({ battery_over_temp: true }))).toBe(false);
    expect(hasGridFault(snap({ grid_loss: true }))).toBe(true);
    expect(hasGridFault(snap({ grid_online: false }))).toBe(true);
  });

  it('returns separate alert entries when multiple faults are active', () => {
    const alerts = buildSystemAlerts(
      snap({ grid_loss: true, inverter_trip: true, battery_over_temp: true }),
      tempConfig,
    );
    expect(alerts.map((alert) => alert.kind)).toEqual(['grid', 'inverter_trip', 'battery_over_temp']);
  });

  it('adds an inverter temperature high alert using configured bounds', () => {
    const alerts = buildSystemAlerts(snap({ inverter_temperature: 65 }), tempConfig);
    expect(alerts.map((alert) => alert.kind)).toContain('inverter_temp_high');
    expect(hasInverterTemperatureAlert(snap({ inverter_temperature: 65 }), tempConfig)).toBe(true);
  });

  it('adds an inverter temperature low alert using configured bounds', () => {
    const alerts = buildSystemAlerts(snap({ inverter_temperature: 5 }), tempConfig);
    expect(alerts.map((alert) => alert.kind)).toContain('inverter_temp_low');
  });

  it('ignores non-finite inverter temperature values', () => {
    expect(hasInverterTemperatureAlert(snap({ inverter_temperature: Number.NaN }), tempConfig)).toBe(false);
  });
});
