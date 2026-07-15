import { describe, it, expect } from 'vitest';
import {
  gridMeterCurrentA,
  resolveGridMeter,
  BUILTIN_GRID_CT_ADDRESS,
  GRID_CT_AUTO,
} from '../../src/lib/meters';
import type { MeterData } from '../../src/lib/types';

/** Minimal meter, override-able. Defaults describe an idle meter so tests
 *  only spell out the fields they care about. */
function meter(address: number, overrides: Partial<MeterData> = {}): MeterData {
  return {
    address,
    v_phase_1: 0, v_phase_2: 0, v_phase_3: 0,
    i_phase_1: 0, i_phase_2: 0, i_phase_3: 0,
    i_total: 0,
    p_active_phase_1: 0, p_active_phase_2: 0, p_active_phase_3: 0,
    p_active_total: 0, p_reactive_total: 0, p_apparent_total: 0,
    pf_total: 0, frequency: 0,
    e_import_active_kwh: 0, e_export_active_kwh: 0,
    ...overrides,
  };
}

describe('resolveGridMeter', () => {
  it('auto-prefers the built-in 0x00 grid CT over external clamps', () => {
    // Three-phase / HV surface the built-in grid CT at 0x00 even when external
    // clamps are also present — auto should pick it.
    const meters = [meter(0x01, { i_total: 5 }), meter(BUILTIN_GRID_CT_ADDRESS, { i_total: 28.34 })];
    expect(resolveGridMeter(meters, GRID_CT_AUTO)?.address).toBe(0x00);
  });

  it('auto-falls back to the lowest external clamp when no built-in exists', () => {
    // AC-coupled: no 0x00; the grid CT is conventionally meter 1 (0x01). Auto
    // picks the lowest-numbered responding clamp, not the first in the array.
    const meters = [meter(0x08, { i_total: 3 }), meter(0x01, { i_total: 5 }), meter(0x02, { i_total: 12 })];
    expect(resolveGridMeter(meters, GRID_CT_AUTO)?.address).toBe(0x01);
  });

  it('pins a specific external clamp when given an explicit address', () => {
    const meters = [meter(0x01, { i_total: 41.2 }), meter(0x02, { i_total: 12 })];
    expect(resolveGridMeter(meters, 0x02)?.address).toBe(0x02);
  });

  it('is null when the chosen address has no meter', () => {
    expect(resolveGridMeter([meter(0x01, { i_total: 5 })], 0x03)).toBeNull();
  });

  it('is null for an empty / missing meter list', () => {
    expect(resolveGridMeter([])).toBeNull();
    expect(resolveGridMeter(undefined)).toBeNull();
    expect(resolveGridMeter(null)).toBeNull();
  });
});

describe('gridMeterCurrentA', () => {
  it('returns the built-in grid CT i_total by default (three-phase / HV)', () => {
    const meters = [meter(0x00, { i_total: 28.34 }), meter(0x01, { i_total: 5 })];
    expect(gridMeterCurrentA(meters)).toBe(28.34);
  });

  it('auto-picks the lowest external clamp when there is no built-in (AC-coupled)', () => {
    // Zero-config: no address passed, no 0x00 — resolves to meter 1.
    const meters = [meter(0x08, { i_total: 3 }), meter(0x01, { i_total: 41.2 })];
    expect(gridMeterCurrentA(meters)).toBe(41.2);
  });

  it('reads a designated external CT clamp by address (issue #192)', () => {
    const meters = [meter(0x01, { i_total: 41.2 }), meter(0x02, { i_total: 12 })];
    expect(gridMeterCurrentA(meters, 0x01)).toBe(41.2);
    expect(gridMeterCurrentA(meters, 0x02)).toBe(12);
  });

  it('finds the grid meter regardless of its position in the array', () => {
    const meters = [meter(0x02, { i_total: 5 }), meter(0x00, { i_total: 12.5 })];
    expect(gridMeterCurrentA(meters)).toBe(12.5);
  });

  it('is null when the chosen address has no meter', () => {
    expect(gridMeterCurrentA([meter(0x01, { i_total: 5 })], 0x03)).toBeNull();
  });

  it('is null when there are no meters at all', () => {
    expect(gridMeterCurrentA([])).toBeNull();
    expect(gridMeterCurrentA(undefined)).toBeNull();
    expect(gridMeterCurrentA(null)).toBeNull();
  });

  it('falls back to phase current when an EM115 reports zero total current (issue #201)', () => {
    // BrianUK6's AC-coupled snapshot showed Meter 0x01 with i_total=0 but
    // i_phase_1=5.75A while importing. The energy wheel must use the real
    // single-phase current rather than displaying 0.0A.
    expect(gridMeterCurrentA([meter(0x01, { i_total: 0, i_phase_1: 5.75 })], 0x01)).toBe(5.75);
  });

  it('sums populated phase currents when total current is missing', () => {
    expect(gridMeterCurrentA([meter(0x01, { i_total: 0, i_phase_1: 5, i_phase_2: 6, i_phase_3: 7 })], 0x01)).toBe(18);
  });

  it('is null when the grid meter has no finite current fields (decode glitch)', () => {
    expect(gridMeterCurrentA([meter(0x00, { i_total: NaN, i_phase_1: NaN, i_phase_2: NaN, i_phase_3: NaN })])).toBeNull();
  });

  it('returns 0 (a real reading) rather than hiding a genuinely idle grid', () => {
    // No grid flow right now is still a valid, useful reading relative to the
    // cut-out fuse — don't suppress it as if the meter were missing.
    expect(gridMeterCurrentA([meter(0x00, { i_total: 0 })])).toBe(0);
  });
});
