import type { MeterData } from './types';

/**
 * Modbus address of the inverter's built-in grid CT, surfaced by the backend
 * as a synthetic meter. Three-phase / HV models populate this (the decoder
 * builds it from the inverter's per-phase grid current registers); single-
 * phase / AC-coupled systems do not — the grid is measured by an external CT
 * clamp whose address is install-specific. External CT clamps live at
 * `0x01`–`0x08` (and a secondary CT at `0x09`).
 */
export const BUILTIN_GRID_CT_ADDRESS = 0x00;

/**
 * "Auto" sentinel for the grid-CT source — the store persists this as the
 * default so amps show with zero setup on the common installs.
 *
 * No register identifies which external CT clamp is the grid point
 * (`MeterType` HR(47) / MR(64) describe the meter *model*, not its role, and
 * givenergy-modbus just probes 0x01–0x08 collecting whatever responds). So
 * address `0` means "pick sensibly": the built-in `0x00` when present, else the
 * lowest-numbered external clamp (GivEnergy's convention is that the grid CT is
 * meter 1). Users with non-standard wiring override to a specific address in
 * Settings (issue #192).
 */
export const GRID_CT_AUTO = 0;

/**
 * Resolve which meter represents the grid import/export point.
 *
 * - `address` 0 (auto, the default): the built-in `0x00` grid CT if present,
 *   otherwise the lowest-numbered external CT clamp.
 * - `address` ≥ 1: that specific external CT clamp.
 *
 * Returns `null` when nothing matches — e.g. a system with no grid meter at
 * all (single-phase hybrids with no external CT).
 */
export function resolveGridMeter(
  meters: MeterData[] | undefined | null,
  address: number = GRID_CT_AUTO,
): MeterData | null {
  if (!meters) return null;
  if (address >= 1) {
    return meters.find((m) => m.address === address) ?? null;
  }
  const builtin = meters.find((m) => m.address === BUILTIN_GRID_CT_ADDRESS);
  if (builtin) return builtin;
  const external = meters
    .filter((m) => m.address >= 1)
    .sort((a, b) => a.address - b.address);
  return external[0] ?? null;
}

/**
 * Measured grid current (amps) from the grid CT meter, when one is available
 * (issue #192: show grid amps instead of frequency on the energy wheel).
 *
 * `address` selects which meter is the grid CT — `0` (auto, the default)
 * resolves to the built-in `0x00` grid CT or the lowest external clamp; an
 * explicit address pins a specific clamp. Its `i_total` is the average of the
 * three phase currents on three-phase models (per-phase detail stays on the
 * Meters page).
 *
 * Returns `null` when no meter matches (so the caller keeps its frequency
 * display) and for a non-finite `i_total` (a decode glitch).
 */
export function gridMeterCurrentA(
  meters: MeterData[] | undefined | null,
  address: number = GRID_CT_AUTO,
): number | null {
  const grid = resolveGridMeter(meters, address);
  if (!grid || !Number.isFinite(grid.i_total)) return null;
  return grid.i_total;
}
