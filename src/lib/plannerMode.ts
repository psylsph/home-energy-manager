/**
 * Where the Planner's automatic charge plan can go wrong because of the
 * charging mode or Timed Charge (issues #437, #448).
 *
 * The Planner only writes charge slot 1 and arms Timed Charge (single-phase); it reads no
 * mode. Standard mode leaves that alone. Cosy, Agile and Adaptive Charge each
 * own the charge schedule or rate, so a plan written beside them is replaced
 * or held back.
 */
import type { InverterSnapshot } from './types';
import { deviceHasBatteryControls, isThreePhaseLimitModel } from './deviceCapabilities';

export interface PlannerModeConflict {
  /** Display name of the mode, in Title Case. */
  mode: string;
  /** One sentence, in sentence case. */
  message: string;
}

const OVERWRITES = 'rewrites the charge slots and will overwrite the plan';

/** The mode getting in the Planner's way, or `null` in Standard mode (or when the mode isn't reported). */
export function plannerModeConflict(
  snapshot: Pick<InverterSnapshot, 'charging_mode'> | null | undefined,
): PlannerModeConflict | null {
  switch (snapshot?.charging_mode) {
    case 'cosy':
      return { mode: 'Cosy', message: `Cosy mode is on and ${OVERWRITES}.` };
    case 'agile':
      return { mode: 'Agile', message: `Agile mode is on and ${OVERWRITES}.` };
    case 'agile_charge':
      return { mode: 'Agile Charge', message: `Agile Charge mode is on and ${OVERWRITES}.` };
    case 'adaptive':
      return {
        mode: 'Adaptive Charge',
        message: 'Adaptive Charge is on and owns the charge rate, so automatic planning waits.',
      };
    default:
      // Standard, Agile discharge-only (no charge writes), or not reported.
      return null;
  }
}

/** Automatic planning (refresh or apply) is on, on a model it runs on (a Gateway sits it out). */
export function plannerAutomationOn(
  snapshot: Pick<
    InverterSnapshot,
    'device_type_code' | 'forecast_plan_auto_refresh' | 'forecast_plan_auto_apply_enabled'
  > | null | undefined,
): boolean {
  if (!snapshot || !deviceHasBatteryControls(snapshot.device_type_code)) return false;
  return snapshot.forecast_plan_auto_refresh === true
    || snapshot.forecast_plan_auto_apply_enabled === true;
}

/**
 * True when automatic planning is on, charge slot 1 (the only slot the Planner
 * writes) is set, and Timed Charge is off, so the slot won't run. A plan that
 * needed no charge clears slot 1, so that isn't a fault. A mode that owns the
 * schedule has its own warning. Three-phase models are left out: the Planner
 * doesn't set their Timed Charge flag, so "off" there says nothing about it.
 */
export function plannerTimedChargeOff(
  snapshot: InverterSnapshot | null | undefined,
): boolean {
  if (!snapshot || !plannerAutomationOn(snapshot)) return false;
  if (isThreePhaseLimitModel(snapshot.device_type_code)) return false;
  if (snapshot.enable_charge) return false;
  if (plannerModeConflict(snapshot)) return false;
  return snapshot.charge_slots?.[0]?.enabled === true;
}
