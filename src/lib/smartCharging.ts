import type { SmartChargeDispatch, SmartChargingResponse } from './types';

/** Accept only a well-formed response; anything else means "unavailable". */
export function readDispatchesResponse(value: unknown): SmartChargingResponse | null {
  if (!value || typeof value !== 'object') return null;
  const v = value as Partial<SmartChargingResponse>;
  if (v.ok !== true || typeof v.enabled !== 'boolean' || !Array.isArray(v.dispatches)) {
    return null;
  }
  return v as SmartChargingResponse;
}

/** What kind of charge this is, and whether the battery is held for it. */
export function dispatchLabel(dispatch: SmartChargeDispatch): string {
  const parts: string[] = [];
  if (dispatch.smart) {
    parts.push('Smart Charge');
  } else if (dispatch.type?.toLowerCase() === 'boost' || dispatch.type?.toLowerCase() === 'bump-charge') {
    parts.push('Boost', 'Peak Rate');
  } else {
    parts.push(dispatch.type ?? 'Unknown');
  }
  if (dispatch.energy_added_kwh != null) parts.push(`${dispatch.energy_added_kwh.toFixed(1)} kWh`);
  parts.push(dispatch.smart ? 'Battery Held' : 'Battery Not Held');
  return parts.join(' · ');
}
