import { describe, it, expect } from 'vitest';
import { dispatchLabel, readDispatchesResponse } from '../../src/lib/smartCharging';
import type { SmartChargeDispatch } from '../../src/lib/types';

const valid = {
  ok: true,
  enabled: true,
  configured: true,
  checked_at: '2026-10-17T12:00:00Z',
  error: null,
  devices: 1,
  holding: false,
  stale_after_minutes: 15,
  dispatches: [],
};

function dispatch(overrides: Partial<SmartChargeDispatch> = {}): SmartChargeDispatch {
  return {
    start: '2026-10-17T13:00:00Z',
    end: '2026-10-17T13:30:00Z',
    type: 'SMART',
    energy_added_kwh: null,
    smart: true,
    ...overrides,
  };
}

describe('readDispatchesResponse', () => {
  it('accepts a well-formed response', () => {
    expect(readDispatchesResponse(valid)).toEqual(valid);
  });

  it('rejects anything else', () => {
    expect(readDispatchesResponse(null)).toBeNull();
    expect(readDispatchesResponse('nope')).toBeNull();
    expect(readDispatchesResponse({ ...valid, ok: false })).toBeNull();
    expect(readDispatchesResponse({ ...valid, enabled: 'yes' })).toBeNull();
    expect(readDispatchesResponse({ ...valid, dispatches: undefined })).toBeNull();
  });
});

describe('dispatchLabel', () => {
  it('says a smart charge holds the battery', () => {
    expect(dispatchLabel(dispatch())).toBe('Smart Charge · Battery Held');
  });

  it('adds the energy when Octopus gives it', () => {
    expect(dispatchLabel(dispatch({ energy_added_kwh: 4.25 }))).toBe(
      'Smart Charge · 4.3 kWh · Battery Held',
    );
  });

  it('says a boost is at the peak rate and the battery is not held', () => {
    expect(dispatchLabel(dispatch({ type: 'BOOST', smart: false }))).toBe(
      'Boost · Peak Rate · Battery Not Held',
    );
  });

  it('names an unrecognised type as Octopus sent it', () => {
    expect(dispatchLabel(dispatch({ type: 'SOMETHING_NEW', smart: false }))).toBe(
      'SOMETHING_NEW · Battery Not Held',
    );
  });
});
