import { describe, it, expect } from 'vitest';
import {
  capacityPercent,
  cellSpreadMv,
  dayToMs,
  hasMetric,
  healthModules,
  latestPerModule,
  pivotHealth,
} from '../../src/lib/batteryHealth';
import type { BatteryHealthDay } from '../../src/lib/types';

function row(overrides: Partial<BatteryHealthDay> = {}): BatteryHealthDay {
  return {
    day: '2026-10-01',
    module_index: 0,
    min_cell_v: 3.3,
    max_cell_v: 3.32,
    max_spread_mv: 20,
    full_spread_mv: 12,
    cycles: 100,
    max_temp_c: 24,
    capacity_ah: 95,
    design_capacity_ah: 100,
    bms_firmware: 3015,
    samples: 10,
    ...overrides,
  };
}

describe('cellSpreadMv', () => {
  it('is the max minus min in mV', () => {
    expect(cellSpreadMv([3.3, 3.32, 3.31])).toBeCloseTo(20, 5);
  });
  it('is null without two cells', () => {
    expect(cellSpreadMv(undefined)).toBeNull();
    expect(cellSpreadMv([])).toBeNull();
    expect(cellSpreadMv([3.3])).toBeNull();
  });
  it('is null when any cell is a failed read, not a 3 V spread', () => {
    expect(cellSpreadMv([3.3, 0, 3.31])).toBeNull();
    expect(cellSpreadMv([3.3, 65.535])).toBeNull();
    expect(cellSpreadMv([3.3, 4.9])).toBeNull();
    expect(cellSpreadMv([3.3, NaN])).toBeNull();
  });
  it('still reports a collapsing cell', () => {
    expect(cellSpreadMv([3.3, 2.1])).toBeCloseTo(1200, 5);
  });
});

describe('capacityPercent', () => {
  it('is capacity over design', () => {
    expect(capacityPercent({ capacity_ah: 95, design_capacity_ah: 100 })).toBeCloseTo(95);
  });
  it('is null when either is unknown or zero', () => {
    expect(capacityPercent({ capacity_ah: null, design_capacity_ah: 100 })).toBeNull();
    expect(capacityPercent({ capacity_ah: 95, design_capacity_ah: null })).toBeNull();
    expect(capacityPercent({ capacity_ah: 0, design_capacity_ah: 100 })).toBeNull();
    expect(capacityPercent({ capacity_ah: 95, design_capacity_ah: 0 })).toBeNull();
  });
});

describe('healthModules', () => {
  it('lists each slot once in slot order with 1-based labels', () => {
    const rows = [
      row({ module_index: 1 }),
      row({ module_index: 0 }),
      row({ module_index: 1, day: '2026-10-02' }),
    ];
    expect(healthModules(rows)).toEqual([
      { key: 'm0', label: '#1' },
      { key: 'm1', label: '#2' },
    ]);
  });
  it('is one module per slot, however many days or rows it has', () => {
    const rows = [0, 1, 2, 3].map((i) => row({ module_index: 0, day: `2026-10-0${i + 1}` }));
    expect(healthModules(rows)).toEqual([{ key: 'm0', label: '#1' }]);
  });
  it('is empty without rows', () => {
    expect(healthModules([])).toEqual([]);
  });
});

describe('pivotHealth', () => {
  it('makes one row per day with a column per module, oldest first', () => {
    const rows = [
      row({ day: '2026-10-02', module_index: 0, full_spread_mv: 14 }),
      row({ day: '2026-10-01', module_index: 0, full_spread_mv: 12 }),
      row({ day: '2026-10-01', module_index: 1, full_spread_mv: 30 }),
    ];
    const out = pivotHealth(rows, 'full_spread_mv');
    expect(out.map((r) => r.day)).toEqual(['2026-10-01', '2026-10-02']);
    expect(out[0].m0).toBe(12);
    expect(out[0].m1).toBe(30);
    expect(out[1].m0).toBe(14);
    expect(out[1].m1).toBeUndefined();
  });
  it('drops days with no value and malformed days', () => {
    const rows = [
      row({ day: '2026-10-01', full_spread_mv: null }),
      row({ day: 'garbage', full_spread_mv: 5 }),
      row({ day: '2026-10-03', full_spread_mv: 9 }),
    ];
    expect(pivotHealth(rows, 'full_spread_mv').map((r) => r.day)).toEqual(['2026-10-03']);
  });
  it('derives capacity as a percentage of design', () => {
    const out = pivotHealth([row({ capacity_ah: 90, design_capacity_ah: 100 })], 'capacity_pct');
    expect(out[0].m0).toBeCloseTo(90);
  });
  it('keeps a zero value (not a missing one)', () => {
    const out = pivotHealth([row({ cycles: 0 })], 'cycles');
    expect(out[0].m0).toBe(0);
  });
  it('is empty without rows', () => {
    expect(pivotHealth([], 'cycles')).toEqual([]);
  });
});

describe('dayToMs', () => {
  it('is local noon of the day', () => {
    const d = new Date(dayToMs('2026-10-01'));
    expect([d.getFullYear(), d.getMonth(), d.getDate(), d.getHours()]).toEqual([2026, 9, 1, 12]);
  });
  it('is NaN for a malformed day', () => {
    expect(dayToMs('2026-1-1')).toBeNaN();
    expect(dayToMs('')).toBeNaN();
  });
});

describe('hasMetric', () => {
  it('is true only when some row carries the metric', () => {
    expect(hasMetric([row({ full_spread_mv: null })], 'full_spread_mv')).toBe(false);
    expect(hasMetric([row({ full_spread_mv: null }), row({ full_spread_mv: 8 })], 'full_spread_mv')).toBe(true);
    expect(hasMetric([], 'cycles')).toBe(false);
  });
});

describe('latestPerModule', () => {
  it("takes each module's most recent day", () => {
    const rows = [
      row({ module_index: 0, day: '2026-10-01', cycles: 100 }),
      row({ module_index: 0, day: '2026-10-05', cycles: 104 }),
      row({ module_index: 1, day: '2026-10-03', cycles: 50, capacity_ah: 48, design_capacity_ah: 50 }),
    ];
    const out = latestPerModule(rows);
    expect(out.map((m) => [m.key, m.day, m.cycles])).toEqual([
      ['m0', '2026-10-05', 104],
      ['m1', '2026-10-03', 50],
    ]);
    expect(out[1].capacityPct).toBeCloseTo(96);
  });
  it('is empty without rows', () => {
    expect(latestPerModule([])).toEqual([]);
  });

  it('uses the latest day that has each figure, so a partial today does not blank or swap it', () => {
    const rows = [
      row({ day: '2026-10-04', full_spread_mv: 12, max_spread_mv: 20, capacity_ah: 95, cycles: 100 }),
      // Today so far: never nearly full, no capacity read yet.
      row({ day: '2026-10-05', full_spread_mv: null, max_spread_mv: 31, capacity_ah: null, cycles: 101 }),
    ];
    const [m] = latestPerModule(rows);
    expect(m.day).toBe('2026-10-05');
    expect(m.spreadMv).toBe(12);
    expect(m.spreadKind).toBe('full');
    expect(m.spreadDay).toBe('2026-10-04');
    expect(m.capacityPct).toBeCloseTo(95);
    expect(m.cycles).toBe(101);
  });

  it("falls back to the day's worst spread only when it was never nearly full", () => {
    const rows = [
      row({ day: '2026-10-04', full_spread_mv: null, max_spread_mv: 20 }),
      row({ day: '2026-10-05', full_spread_mv: null, max_spread_mv: 31 }),
    ];
    const [m] = latestPerModule(rows);
    expect(m.spreadMv).toBe(31);
    expect(m.spreadKind).toBe('worst');
    expect(m.spreadDay).toBe('2026-10-05');
  });

  it('has no spread when no day recorded one', () => {
    const [m] = latestPerModule([row({ full_spread_mv: null, max_spread_mv: null })]);
    expect(m.spreadMv).toBeNull();
    expect(m.spreadKind).toBeNull();
    expect(m.spreadDay).toBeNull();
  });
});
