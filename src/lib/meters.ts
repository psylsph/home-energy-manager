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
 * Minimum real-power reading needed before trusting EM115 phase-current fallback.
 * The energy wheel treats smaller grid-power values as idle too, and EM115s can
 * briefly leave a stale phase-current value beside a near-zero active-power
 * reading.
 */
const PHASE_CURRENT_FALLBACK_MIN_ACTIVE_POWER_W = 20;

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
 * explicit address pins a specific clamp.
 *
 * Most meters populate `i_total`, but some single-phase EM115 installs report
 * `i_total = 0` while `i_phase_1` contains the real current. Treat a non-zero
 * total as authoritative. Otherwise, only fall back to populated phase currents
 * when the meter's active-power fields show meaningful flow; that avoids
 * displaying a stale phase-current sample beside an idle / near-idle grid
 * reading.
 *
 * Returns `null` when no meter matches (so the caller keeps its frequency
 * display) and when neither total nor phase currents are finite.
 */
export function gridMeterCurrentA(
  meters: MeterData[] | undefined | null,
  address: number = GRID_CT_AUTO,
): number | null {
  const grid = resolveGridMeter(meters, address);
  if (!grid) return null;

  if (Number.isFinite(grid.i_total) && grid.i_total !== 0) {
    return Math.abs(grid.i_total);
  }

  const activePowerFields = [
    grid.p_active_total,
    grid.p_active_phase_1,
    grid.p_active_phase_2,
    grid.p_active_phase_3,
  ];
  const hasActivePower = activePowerFields.some(
    (watts) => Number.isFinite(watts) && Math.abs(watts) > PHASE_CURRENT_FALLBACK_MIN_ACTIVE_POWER_W,
  );
  if (hasActivePower) {
    const phaseTotal = [grid.i_phase_1, grid.i_phase_2, grid.i_phase_3]
      .filter((amps) => Number.isFinite(amps) && amps !== 0)
      .reduce((sum, amps) => sum + Math.abs(amps), 0);
    if (phaseTotal > 0) return phaseTotal;
  }

  if (Number.isFinite(grid.i_total)) return Math.abs(grid.i_total);
  return null;
}
