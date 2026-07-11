/**
 * Pure-data helpers for the History page's Temperature tab.
 *
 * Kept separate from HistoryPage.tsx so the differential logic can be
 * unit-tested without rendering React. The data shape mirrors what
 * `fetchHistory` returns after bucketing: one object per bucket, with raw
 * field values keyed by field name and any missing readings as `null`/
 * `undefined`. See `tests/lib/temperatureChart.test.ts` for the contract.
 */

export type TemperatureRow = Record<string, number>;

/**
 * A bucket of raw temperature history. Optional fields model the real
 * `ChartCard` merged-row shape: missing readings are absent keys, not
 * `null` values. See `HistoryPage.tsx` ChartCard.merged construction.
 */
export type TemperatureInput = Record<string, number | undefined>;

/**
 * Compute the battery-minus-inverter temperature differential for each row.
 *
 * Used by the "Battery − Inverter (°C)" chart on the History page. A positive
 * value means the battery is warmer than the inverter; a negative value means
 * the inverter is running hotter than the battery. On a hot day with a hard-
 * working inverter this fans up to several degrees; on a cold day the two
 * should sit close together.
 *
 * Missing data (either field absent on a row) is represented as `NaN` so
 * Recharts leaves a visible gap in the line rather than drawing a misleading
 * zero. `NaN` is a valid `number` so the result still satisfies the strict
 * `Record<string, number>` shape that `ChartDef.preprocess` requires.
 */
export function computeTempDifferential(rows: TemperatureInput[]): TemperatureRow[] {
  return rows.map((row) => {
    const batt = row.battery_temperature;
    const inv = row.inverter_temperature;
    const out: TemperatureRow = { ...row } as TemperatureRow;
    out._temp_diff = batt != null && inv != null ? batt - inv : Number.NaN;
    return out;
  });
}

/**
 * Compute the battery-minus-outdoor temperature differential for each row.
 *
 * Used by the "Battery − Outdoor (Δ°C)" chart on the History page. A positive
 * value means the battery is warmer than the Open-Meteo outdoor temperature;
 * a negative value means the outdoor temperature is warmer than the battery.
 * A value near zero means they are approximately equal — it is not a recorded
 * 0°C battery or outdoor temperature.
 *
 * Missing data (either field absent on a row) is represented as `NaN` so
 * Recharts leaves a visible gap rather than drawing a misleading zero.
 */
export function computeBatteryExternalDifferential(rows: TemperatureInput[]): TemperatureRow[] {
  return rows.map((row) => {
    const batt = row.battery_temperature;
    const ext = row.external_temperature;
    const out: TemperatureRow = { ...row } as TemperatureRow;
    out._batt_ext_diff = batt != null && ext != null ? batt - ext : Number.NaN;
    return out;
  });
}
