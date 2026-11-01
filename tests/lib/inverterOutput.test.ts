import { describe, it, expect } from 'vitest';
import { conversionLossW } from '../../src/lib/inverterOutput';

const aio = { device_type_code: '8001', solar_power: 0, battery_power: 0 };

describe('conversionLossW', () => {
  it('is the DC power in minus the AC power out while discharging', () => {
    // The user report: battery 446 W DC, inverter 390 W AC.
    expect(conversionLossW({ ...aio, battery_power: 446, inverter_output_power: 390 })).toBe(56);
  });

  it('counts solar as DC input alongside the battery', () => {
    expect(
      conversionLossW({ ...aio, solar_power: 3000, battery_power: -1000, inverter_output_power: 1900 }),
    ).toBe(100);
  });

  it('works when charging from AC (both sides negative)', () => {
    expect(conversionLossW({ ...aio, battery_power: -1000, inverter_output_power: -1100 })).toBe(100);
  });

  it('is null without an AC output reading', () => {
    expect(conversionLossW({ ...aio, battery_power: 446 })).toBeNull();
    expect(conversionLossW({ ...aio, battery_power: 446, inverter_output_power: null })).toBeNull();
    expect(conversionLossW({ ...aio, battery_power: 446, inverter_output_power: Number.NaN })).toBeNull();
  });

  it('is null when the readings come out negative (taken at different moments)', () => {
    expect(conversionLossW({ ...aio, battery_power: 300, inverter_output_power: 390 })).toBeNull();
  });

  it('is null on AC-coupled inverters, whose solar is not a DC input', () => {
    expect(
      conversionLossW({ ...aio, device_type_code: '3001', solar_power: 2000, battery_power: 0, inverter_output_power: 0 }),
    ).toBeNull();
  });
});
