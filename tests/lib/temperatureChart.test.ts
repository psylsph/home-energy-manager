import { describe, it, expect } from 'vitest';
import { computeTempDifferential, computeBatteryExternalDifferential } from '../../src/lib/temperatureChart';

/**
 * Tests for `computeTempDifferential` — the pure-data helper that powers
 * the "Battery − Inverter (°C)" chart on the History page's Temperature tab.
 *
 * The function takes a row per bucketed timestamp and adds a `_temp_diff`
 * field equal to `battery_temperature - inverter_temperature`. Missing
 * data is represented as `NaN` so the chart leaves a gap rather than
 * drawing a misleading zero.
 */
describe('computeTempDifferential', () => {
  it('subtracts inverter from battery on a normal row', () => {
    const rows = [{ t: 1000, battery_temperature: 25, inverter_temperature: 40 }];
    const result = computeTempDifferential(rows);
    expect(result[0]._temp_diff).toBe(-15);
  });

  it('produces a positive value when the battery is warmer than the inverter', () => {
    // Cold morning: battery self-heating on a quiet inverter.
    const rows = [{ t: 1000, battery_temperature: 18, inverter_temperature: 12 }];
    expect(computeTempDifferential(rows)[0]._temp_diff).toBe(6);
  });

  it('produces a negative value when the inverter is running hotter', () => {
    // Hot afternoon: inverter working hard.
    const rows = [{ t: 1000, battery_temperature: 28, inverter_temperature: 55 }];
    expect(computeTempDifferential(rows)[0]._temp_diff).toBe(-27);
  });

  it('handles negative temperatures on a cold winter night', () => {
    // Both below zero — arithmetic must still hold.
    const rows = [{ t: 1000, battery_temperature: -5, inverter_temperature: -8 }];
    expect(computeTempDifferential(rows)[0]._temp_diff).toBe(3);
  });

  it('returns NaN when battery temperature is missing', () => {
    // A row with only the inverter reading — happens at inverter startup
    // before the BMS data has been polled. The merged row simply omits
    // the missing key, so the function must handle undefined values.
    const rows: { t: number; battery_temperature?: number; inverter_temperature: number }[] =
      [{ t: 1000, inverter_temperature: 30 }];
    const result = computeTempDifferential(rows);
    expect(Number.isNaN(result[0]._temp_diff as number)).toBe(true);
  });

  it('returns NaN when inverter temperature is missing', () => {
    const rows: { t: number; battery_temperature: number; inverter_temperature?: number }[] =
      [{ t: 1000, battery_temperature: 22 }];
    expect(Number.isNaN(computeTempDifferential(rows)[0]._temp_diff as number)).toBe(true);
  });

  it('returns NaN when both fields are missing', () => {
    const rows = [{ t: 1000 }];
    expect(Number.isNaN(computeTempDifferential(rows)[0]._temp_diff as number)).toBe(true);
  });

  it('returns NaN for an empty row object', () => {
    const rows = [{}];
    expect(Number.isNaN(computeTempDifferential(rows)[0]._temp_diff as number)).toBe(true);
  });

  it('returns an empty array for an empty input', () => {
    expect(computeTempDifferential([])).toEqual([]);
  });

  it('preserves the input row shape for downstream chart fields', () => {
    // The function must not strip t or any other field the chart needs.
    const rows = [{ t: 1234, battery_temperature: 25, inverter_temperature: 40 }];
    const result = computeTempDifferential(rows);
    expect(result[0].t).toBe(1234);
    expect(result[0].battery_temperature).toBe(25);
    expect(result[0].inverter_temperature).toBe(40);
    expect(result[0]._temp_diff).toBe(-15);
  });

  it('does not mutate the input rows', () => {
    // Defensive: ChartDef.preprocess runs once per chart in the same tab,
    // so reusing the merged row across charts must not double-add _temp_diff.
    const rows = [{ t: 1000, battery_temperature: 25, inverter_temperature: 40 }];
    const original = JSON.parse(JSON.stringify(rows));
    computeTempDifferential(rows);
    expect(rows).toEqual(original);
  });

  it('handles many rows in a single pass', () => {
    // Mirrors a bucketed time series: every bucket has both readings.
    const rows = Array.from({ length: 100 }, (_, i) => ({
      t: 1000 + i * 60_000,
      battery_temperature: 20 + (i % 10),
      inverter_temperature: 30 + (i % 5),
    }));
    const result = computeTempDifferential(rows);
    expect(result).toHaveLength(100);
    expect(result[0]._temp_diff).toBe(20 - 30);
    expect(result[99]._temp_diff).toBe(29 - 34);
  });
});

/**
 * Tests for `computeBatteryExternalDifferential` — the pure-data helper
 * that powers the "Battery − Ambient (°C)" chart on the History page's
 * Temperature tab.
 *
 * Adds a `_batt_ext_diff` field equal to `battery_temperature -
 * external_temperature`. Missing data is represented as `NaN` so the
 * chart leaves a gap rather than drawing a misleading zero.
 */
describe('computeBatteryExternalDifferential', () => {
  it('subtracts ambient from battery on a normal row', () => {
    const rows = [{ t: 1000, battery_temperature: 28, external_temperature: 15 }];
    const result = computeBatteryExternalDifferential(rows);
    expect(result[0]._batt_ext_diff).toBe(13);
  });

  it('produces a negative value when ambient is warmer than the battery', () => {
    // Hot summer day: outside air at 35°C, battery in a cool basement at 22°C.
    const rows = [{ t: 1000, battery_temperature: 22, external_temperature: 35 }];
    expect(computeBatteryExternalDifferential(rows)[0]._batt_ext_diff).toBe(-13);
  });

  it('produces near-zero when battery tracks ambient closely', () => {
    // Garage install with light load — battery temperature follows the air.
    const rows = [{ t: 1000, battery_temperature: 18.2, external_temperature: 18 }];
    expect(computeBatteryExternalDifferential(rows)[0]._batt_ext_diff).toBeCloseTo(0.2, 5);
  });

  it('handles negative ambient temperatures on a cold winter night', () => {
    // Sub-zero outside, battery kept above freezing by indoor install.
    const rows = [{ t: 1000, battery_temperature: 5, external_temperature: -3 }];
    expect(computeBatteryExternalDifferential(rows)[0]._batt_ext_diff).toBe(8);
  });

  it('returns NaN when battery temperature is missing', () => {
    // A row with only the external reading — happens before the first
    // inverter poll or during a reconnect gap.
    const rows: { t: number; battery_temperature?: number; external_temperature: number }[] =
      [{ t: 1000, external_temperature: 12 }];
    const result = computeBatteryExternalDifferential(rows);
    expect(Number.isNaN(result[0]._batt_ext_diff as number)).toBe(true);
  });

  it('returns NaN when external temperature is missing', () => {
    // User hasn't configured weather yet — every row is missing the field.
    const rows: { t: number; battery_temperature: number; external_temperature?: number }[] =
      [{ t: 1000, battery_temperature: 25 }];
    expect(Number.isNaN(computeBatteryExternalDifferential(rows)[0]._batt_ext_diff as number)).toBe(true);
  });

  it('returns NaN when both fields are missing', () => {
    const rows = [{ t: 1000 }];
    expect(Number.isNaN(computeBatteryExternalDifferential(rows)[0]._batt_ext_diff as number)).toBe(true);
  });

  it('returns NaN for an empty row object', () => {
    const rows = [{}];
    expect(Number.isNaN(computeBatteryExternalDifferential(rows)[0]._batt_ext_diff as number)).toBe(true);
  });

  it('returns an empty array for an empty input', () => {
    expect(computeBatteryExternalDifferential([])).toEqual([]);
  });

  it('preserves the input row shape for downstream chart fields', () => {
    const rows = [{ t: 1234, battery_temperature: 28, external_temperature: 15, inverter_temperature: 40 }];
    const result = computeBatteryExternalDifferential(rows);
    expect(result[0].t).toBe(1234);
    expect(result[0].battery_temperature).toBe(28);
    expect(result[0].external_temperature).toBe(15);
    expect(result[0].inverter_temperature).toBe(40);
    expect(result[0]._batt_ext_diff).toBe(13);
  });

  it('does not mutate the input rows', () => {
    const rows = [{ t: 1000, battery_temperature: 28, external_temperature: 15 }];
    const original = JSON.parse(JSON.stringify(rows));
    computeBatteryExternalDifferential(rows);
    expect(rows).toEqual(original);
  });

  it('handles many rows in a single pass', () => {
    const rows = Array.from({ length: 100 }, (_, i) => ({
      t: 1000 + i * 60_000,
      battery_temperature: 25 + (i % 8),
      external_temperature: 10 + (i % 12),
    }));
    const result = computeBatteryExternalDifferential(rows);
    expect(result).toHaveLength(100);
    expect(result[0]._batt_ext_diff).toBe(25 - 10);
    expect(result[99]._batt_ext_diff).toBe(28 - 13);
  });
});
