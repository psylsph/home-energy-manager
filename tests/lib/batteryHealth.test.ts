import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import {
  capacityPercent,
  cellSpreadMv,
  dayToMs,
  describeSocEvents,
  describeTrend,
  socEventDays,
  hasMetric,
  formatHealthTick,
  healthAxis,
  healthModules,
  latestPerModule,
  pivotHealth,
} from '../../src/lib/batteryHealth';
import type { BatteryHealthDay, BatteryHealthTrend } from '../../src/lib/types';

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
    soc_jumps: 0,
    soc_stuck: 0,
    ...overrides,
  };
}

function trend(overrides: Partial<BatteryHealthTrend> = {}): BatteryHealthTrend {
  return { module_index: 0, status: 'ok', recent_mv: 31, baseline_mv: 30, recent_days: 7, recent_min_days: 4, ...overrides };
}

describe('describeTrend', () => {
  it('is null without a trend for the module', () => {
    expect(describeTrend(undefined)).toBeNull();
  });
  it('warns of a widening spread with both figures', () => {
    expect(describeTrend(trend({ status: 'widening', baseline_mv: 30.4, recent_mv: 71.6 }))).toEqual({
      tone: 'warn',
      text: 'Widening: 30 → 72 mV near full',
    });
  });
  it('warns of a high spread without a baseline', () => {
    expect(describeTrend(trend({ status: 'high', baseline_mv: null, recent_mv: 162 }))).toEqual({
      tone: 'warn',
      text: 'High: 162 mV near full',
    });
  });
  it('reports a steady spread', () => {
    expect(describeTrend(trend())).toEqual({ tone: 'ok', text: 'Steady: 31 mV near full' });
  });
  it('asks for more near-full days when the week has too few', () => {
    expect(describeTrend(trend({ status: 'insufficient', recent_days: 2, recent_mv: 40 }))).toEqual({
      tone: 'muted',
      text: 'Needs more near-full days (2 of 4 this week)',
    });
  });
  it('takes the near-full days needed from the backend', () => {
    expect(describeTrend(trend({ status: 'insufficient', recent_days: 4, recent_min_days: 5 }))?.text).toBe(
      'Needs more near-full days (4 of 5 this week)',
    );
  });
  it('explains a baseline still being built', () => {
    expect(describeTrend(trend({ status: 'insufficient', recent_days: 7, baseline_mv: null }))).toEqual({
      tone: 'muted',
      text: 'Building a baseline: about a month of near-full days',
    });
  });
});

describe('socEventDays', () => {
  it('lists days with events, newest first then by module', () => {
    const rows = [
      row({ day: '2026-10-01', soc_jumps: 1 }),
      row({ day: '2026-10-02' }),
      row({ day: '2026-10-03', module_index: 1, soc_stuck: 1 }),
      row({ day: '2026-10-03', module_index: 0, soc_jumps: 2 }),
    ];
    expect(socEventDays(rows)).toEqual([
      { day: '2026-10-03', module_index: 0, jumps: 2, stuck: 0 },
      { day: '2026-10-03', module_index: 1, jumps: 0, stuck: 1 },
      { day: '2026-10-01', module_index: 0, jumps: 1, stuck: 0 },
    ]);
  });
  it('treats missing counts (an older backend) as none', () => {
    const old = row();
    delete (old as Partial<BatteryHealthDay>).soc_jumps;
    delete (old as Partial<BatteryHealthDay>).soc_stuck;
    expect(socEventDays([old])).toEqual([]);
  });
});

describe('describeSocEvents', () => {
  it('counts in words', () => {
    expect(describeSocEvents({ jumps: 1, stuck: 0 })).toBe('SOC jumped once');
    expect(describeSocEvents({ jumps: 2, stuck: 0 })).toBe('SOC jumped twice');
    expect(describeSocEvents({ jumps: 3, stuck: 0 })).toBe('SOC jumped 3 times');
    expect(describeSocEvents({ jumps: 0, stuck: 1 })).toBe('SOC stuck once');
    expect(describeSocEvents({ jumps: 2, stuck: 1 })).toBe('SOC jumped twice and stuck once');
  });
});

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

describe('healthAxis', () => {
  const days = (...ds: string[]) => ds.map(dayToMs);
  const label = (ms: number) => {
    const d = new Date(ms);
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
  };
  const DAY = 86_400_000;

  it('is empty without data', () => {
    expect(healthAxis([]).ticks).toEqual([]);
  });

  it('labels a single day', () => {
    expect(healthAxis(days('2026-10-01'))).toEqual({ ticks: days('2026-10-01'), unit: 'day' });
  });

  it('labels every day of a short range, including days without data', () => {
    expect(healthAxis(days('2026-10-01', '2026-10-04')).ticks.map(label)).toEqual([
      '2026-10-01',
      '2026-10-02',
      '2026-10-03',
      '2026-10-04',
    ]);
  });

  it('spaces a range of weeks evenly in whole days, ending on the newest day', () => {
    const { ticks, unit } = healthAxis(days('2026-09-01', '2026-10-28'), 5);
    expect(unit).toBe('day');
    expect(ticks.length).toBeLessThanOrEqual(5);
    expect(ticks.length).toBeGreaterThanOrEqual(4);
    expect(label(ticks[ticks.length - 1])).toBe('2026-10-28');
    expect(ticks[0]).toBeGreaterThanOrEqual(dayToMs('2026-09-01'));
    const gaps = ticks.slice(1).map((t, i) => Math.round((t - ticks[i]) / DAY));
    expect(new Set(gaps).size).toBe(1);
  });

  it('labels months for longer ranges, inside the data and within the limit', () => {
    const { ticks, unit } = healthAxis(days('2026-04-10', '2026-10-28'), 5);
    expect(unit).toBe('month');
    expect(ticks.length).toBeLessThanOrEqual(5);
    for (const t of ticks) {
      expect(new Date(t).getDate()).toBe(1);
      expect(t).toBeGreaterThanOrEqual(dayToMs('2026-04-10'));
      expect(t).toBeLessThanOrEqual(dayToMs('2026-10-28'));
    }
    // Newest month is labelled.
    expect(label(ticks[ticks.length - 1])).toBe('2026-10-01');
  });

  it('labels the 3m range on days, newest day included', () => {
    const { ticks, unit } = healthAxis(days('2026-08-01', '2026-10-30'));
    expect(unit).toBe('day');
    expect(label(ticks[ticks.length - 1])).toBe('2026-10-30');
    expect(ticks.length).toBeGreaterThanOrEqual(3);
  });

  it('spreads a year that starts mid-month over the full four labels', () => {
    const { ticks, unit } = healthAxis(days('2025-10-15', '2026-10-30'));
    expect(unit).toBe('month');
    expect(ticks).toHaveLength(4);
    expect(label(ticks[0]) <= '2026-01-01').toBe(true);
  });

  it('never repeats a label across a whole year', () => {
    const { ticks, unit } = healthAxis(days('2025-10-28', '2026-10-28'), 5);
    const labels = ticks.map((t) => formatHealthTick(t, unit));
    expect(new Set(labels).size).toBe(labels.length);
  });

  it('shows the year on month labels', () => {
    expect(formatHealthTick(dayToMs('2026-10-01'), 'month')).toMatch(/2026/);
    expect(formatHealthTick(dayToMs('2026-10-01'), 'day')).not.toMatch(/2026/);
  });

  it('spans years for the All range without crowding', () => {
    const { ticks } = healthAxis(days('2017-01-05', '2026-10-28'), 5);
    expect(ticks.length).toBeGreaterThanOrEqual(2);
    expect(ticks.length).toBeLessThanOrEqual(5);
  });

  it('takes the range from unsorted input', () => {
    expect(healthAxis(days('2026-10-03', '2026-10-01')).ticks.map(label)).toEqual([
      '2026-10-01',
      '2026-10-02',
      '2026-10-03',
    ]);
  });

  it('treats a limit below two as two, rather than looping or overshooting', () => {
    expect(healthAxis(days('2026-10-01'), 0).ticks).toHaveLength(1);
    expect(healthAxis(days('2026-10-01', '2026-10-11'), 1).ticks.length).toBeLessThanOrEqual(2);
  });

  it('fits a phone-width chart by default: at most four labels', () => {
    for (const span of [days('2026-09-01', '2026-10-28'), days('2026-04-10', '2026-10-28'), days('2025-10-28', '2026-10-28')]) {
      expect(healthAxis(span).ticks.length).toBeLessThanOrEqual(4);
    }
  });

  it('ignores malformed days', () => {
    expect(healthAxis([Number.NaN, dayToMs('2026-10-01')]).ticks).toEqual(days('2026-10-01'));
  });

  describe('across a clock change', () => {
    const previous = process.env.TZ;
    beforeAll(() => {
      process.env.TZ = 'Europe/London';
    });
    afterAll(() => {
      if (previous === undefined) delete process.env.TZ;
      else process.env.TZ = previous;
    });

    it('keeps every day on local midday', () => {
      // UK clocks go back on 25 Oct 2026, so this span is not a whole number of days.
      expect(dayToMs('2026-10-30') - dayToMs('2026-10-20')).not.toBe(10 * DAY);
      const { ticks } = healthAxis(days('2026-10-20', '2026-10-30'), 20);
      expect(ticks).toHaveLength(11);
      for (const t of ticks) expect(new Date(t).getHours()).toBe(12);
    });
  });
});
