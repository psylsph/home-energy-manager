// The car-charging rule (issue #454), mirrored from
// src-tauri/src/history/car_charging.rs: in a house where only the car
// pushes home load past a threshold, a minute above it is the car drawing
// the charger's current at the grid voltage, capped at the load. The backend
// derives History and the reports from its own copy; this one serves the
// live tile and the Status diagram, which have a snapshot but no history
// row. Both read tests/fixtures/car-charging-rule.json, so they cannot drift.

import type { PollSettings } from './types';

// The limits below mirror `CarChargingConfig` in car_charging.rs and the
// API's validation; every page and form takes them from here.
export const DEFAULT_CHARGER_AMPS = 32;
export const MIN_CHARGER_AMPS = 6;
export const MAX_CHARGER_AMPS = 80;
const FALLBACK_VOLTAGE_V = 230;
const VOLTAGE_RANGE_V: [number, number] = [180, 280];
/** The API's limits for the car charging threshold, kW. */
export const MIN_THRESHOLD_KW = 1;
export const MAX_THRESHOLD_KW = 25;

/**
 * Whether a saved car charging threshold switches the feature on: a finite
 * number of kW within the API's limits. Anything else (empty, or a
 * hand-edited value the backend ignores) means it is off.
 */
export function isCarThresholdKw(kw: number | null | undefined): kw is number {
  return kw != null && Number.isFinite(kw) && kw >= MIN_THRESHOLD_KW && kw <= MAX_THRESHOLD_KW;
}

/** The grid voltage to use: the reading when it is in range, else 230 V. */
export function effectiveVoltage(gridV: number | null | undefined): number {
  return gridV != null && Number.isFinite(gridV) && gridV >= VOLTAGE_RANGE_V[0] && gridV <= VOLTAGE_RANGE_V[1]
    ? gridV
    : FALLBACK_VOLTAGE_V;
}

/** The car's power in watts for a load of `homeW`; 0 at or below the threshold. */
export function carPower(
  homeW: number,
  gridV: number | null | undefined,
  thresholdW: number,
  amps: number,
): number {
  if (!Number.isFinite(homeW) || !(homeW > thresholdW)) return 0;
  return Math.min(amps * effectiveVoltage(gridV), homeW);
}

type CarSettings = Pick<PollSettings, 'forecast_exclude_load_above_kw' | 'car_charger_current_a'>;

/**
 * The car's estimated power from the saved settings, or null when the
 * feature is off (no threshold set, or one outside the API's 1–25 kW).
 */
export function estimateCarPower(
  homeW: number,
  gridV: number | null | undefined,
  settings: Partial<CarSettings>,
): number | null {
  const kw = settings.forecast_exclude_load_above_kw;
  if (!isCarThresholdKw(kw)) return null;
  const saved = settings.car_charger_current_a;
  const amps =
    saved != null && Number.isInteger(saved) && saved >= MIN_CHARGER_AMPS && saved <= MAX_CHARGER_AMPS
      ? saved
      : DEFAULT_CHARGER_AMPS;
  return carPower(homeW, gridV, kw * 1000, amps);
}
