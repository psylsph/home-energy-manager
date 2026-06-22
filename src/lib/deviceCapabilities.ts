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

/**
 * Whether the inverter exposes a configurable grid export limit register.
 *
 * Mirrors the register routing in the backend `set_export_limit` handler
 * (`server/api.rs`), which is the authoritative source for which device
 * families have a user-writable export limit:
 *
 *   - 0x70xx — Gateway        → HR 2071 (plant-level export limit, raw W)
 *   - 0x50xx — EMS            → HR 2071
 *   - 0x51xx — EMS Commercial → HR 2071
 *   - 0x40xx — three-phase    → HR 1063 (`p_export_limit`, deci-W)
 *   - 0x41xx — AIO Commercial → HR 1063
 *   - 0x60xx — AC three-phase → HR 1063
 *   - 0x81xx — HV Gen3 hybrid → HR 1063
 *   - 0x82xx — All-in-One hybrid → HR 1063
 *
 * Single-phase / AC-coupled hybrids (Gen1/2/3/4, Polar, Gen3+, AC, AC Mk2,
 * and the residential All-in-One 0x80xx) have NO configurable export limit:
 * their HR(26) `grid_port_max_power_output` is the read-only rated hardware
 * max output, not an export-limit setting, and `/api/control/export-limit`
 * refuses the write for them. Used by `InverterPage` to show the "Grid Export
 * Limit" row only where it is meaningful.
 *
 * Returns false when the device type code is missing (pre-snapshot state).
 */
export function deviceSupportsExportLimit(
  snapshot: Pick<InverterSnapshot, 'device_type_code'> | null | undefined,
): boolean {
  const code = snapshot?.device_type_code;
  if (!code) return false;
  return (
    code.startsWith('40')
    || code.startsWith('41')
    || code.startsWith('50')
    || code.startsWith('51')
    || code.startsWith('60')
    || code.startsWith('70')
    || code.startsWith('81')
    || code.startsWith('82')
  );
}
