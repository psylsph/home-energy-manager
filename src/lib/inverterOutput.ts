import type { InverterSnapshot } from './types';
import { isAcCoupledDevice } from './deviceCapabilities';

/**
 * Watts lost converting between the DC side (solar + battery) and the
 * inverter's AC output (IR 24, `inverter_output_power`).
 *
 * `battery_power` is measured at the battery (DC), so on its own it never
 * matches the home + grid figures on the AC side; this gap explains why.
 * Null when there's no AC reading, on AC-coupled inverters (their solar isn't
 * a DC input), and when the result is negative, which only happens when the
 * readings were taken at slightly different moments.
 */
export function conversionLossW(
  s: Pick<InverterSnapshot, 'device_type_code' | 'solar_power' | 'battery_power' | 'inverter_output_power'>,
): number | null {
  const output = s.inverter_output_power;
  if (typeof output !== 'number' || !Number.isFinite(output)) return null;
  if (isAcCoupledDevice(s.device_type_code)) return null;
  const loss = s.solar_power + s.battery_power - output;
  return loss >= 0 ? loss : null;
}
