import type { InverterSnapshot } from './types';

/**
 * Whether the inverter exposes the Emergency Power Supply (EPS) enable
 * register at HR 317.
 *
 * Mirrors the backend's `DeviceType::supports_eps` and the givenergy-modbus
 * reference library's `_AC_CONFIG_BLOCK_MODELS = {AC, AC_3PH, ALL_IN_ONE}`:
 *
 *   - 0x30xx — AC-coupled (single-phase AC battery inverter)
 *   - 0x60xx — AC three-phase
 *   - 0x80xx — Residential All-in-One (AIO 6kW, 3.6kW, 5kW)
 *
 * DC hybrids (Gen1/2/3/4, Polar, Gen3+) and pure three-phase models have no
 * AC output stage and lack HR 317; writing it is silently dropped by the
 * firmware. Used by `ControlPage` to hide the EPS toggle and by the `/api/control/eps`
 * handler to refuse the write with HTTP 400.
 *
 * Returns false when the device type code is missing (pre-snapshot state) so
 * the UI doesn't briefly flash a control that will be rejected.
 */
export function deviceSupportsEps(
  snapshot: Pick<InverterSnapshot, 'device_type_code'> | null | undefined,
): boolean {
  const code = snapshot?.device_type_code;
  if (!code) return false;
  return (
    code === '3001'
    || code === '3002'
    || code.startsWith('60')
    || code.startsWith('80')
  );
}