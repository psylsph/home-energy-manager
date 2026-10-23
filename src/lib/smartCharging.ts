import type { SmartChargeDispatch, SmartChargingResponse } from './types';

/** Accept only a well-formed response; anything else means "unavailable". */
export function readDispatchesResponse(_value: unknown): SmartChargingResponse | null {
  return null;
}

/** What kind of charge this is, and whether the battery is held for it. */
export function dispatchLabel(_dispatch: SmartChargeDispatch): string {
  return '';
}
