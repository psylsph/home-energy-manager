import { describe, it, expect } from 'vitest';
import type { InverterSnapshot } from '../../src/lib/types';
import {
  balanceSummary,
  todayBalance,
} from '../../src/lib/energyBalance';

function snap(over: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    today_solar_kwh: 0,
    today_import_kwh: 0,
    today_discharge_kwh: 0,
    home_energy_today_kwh: 0,
    today_export_kwh: 0,
    today_charge_kwh: 0,
    device_type_code: '2201',
    ...over,
  } as InverterSnapshot;
}

describe('todayBalance', () => {
  const balance = todayBalance(snap({
    today_solar_kwh: 12,
    today_import_kwh: 2,
    today_discharge_kwh: 1,
    home_energy_today_kwh: 6,
    today_export_kwh: 4,
    today_charge_kwh: 2.5,
  }));

  it('builds the six segments with Title Case labels', () => {
    expect(balance.inSegments.map((s) => [s.key, s.label])).toEqual([
      ['solar', 'Solar'],
      ['grid_import', 'Grid Import'],
      ['battery_out', 'Battery Out'],
    ]);
    expect(balance.outSegments.map((s) => [s.key, s.label])).toEqual([
      ['home_use', 'Home Use'],
      ['export', 'Grid Export'],
      ['battery_in', 'Battery In'],
    ]);
  });

  it('totals each bar as the sum of its segments', () => {
    const sum = (segs: { kwh: number }[]) => segs.reduce((t, s) => t + s.kwh, 0);
    expect(balance.inTotal).toBe(sum(balance.inSegments));
    expect(balance.outTotal).toBe(sum(balance.outSegments));
    expect(balance.inTotal).toBe(15);
    expect(balance.outTotal).toBe(12.5);
  });

  it('zooms both bars to the larger total, so the gap stays visible (D1)', () => {
    // In (15) > Out (12.5): the scale is the In total and the In bar fills
    // its whole width; the Out bar only reaches its own share.
    expect(balance.scaleKwh).toBe(15);
    expect(balance.inSegments.every((s) => s.fraction === s.kwh / 15)).toBe(true);
    expect(balance.outSegments.every((s) => s.fraction === s.kwh / 15)).toBe(true);
    const inWidth = balance.inSegments.reduce((t, s) => t + s.fraction, 0);
    const outWidth = balance.outSegments.reduce((t, s) => t + s.fraction, 0);
    expect(inWidth).toBe(1);
    expect(outWidth).toBeCloseTo(12.5 / 15);
  });

  it('keeps the shared scale when Out > In', () => {
    const b = todayBalance(snap({
      today_solar_kwh: 2,
      today_import_kwh: 3,
      today_discharge_kwh: 4,
      home_energy_today_kwh: 5,
      today_export_kwh: 3,
      today_charge_kwh: 4,
    }));
    expect(b.inTotal).toBe(9);
    expect(b.outTotal).toBe(12);
    expect(b.scaleKwh).toBe(12);
    const inWidth = b.inSegments.reduce((t, s) => t + s.fraction, 0);
    const outWidth = b.outSegments.reduce((t, s) => t + s.fraction, 0);
    expect(outWidth).toBe(1);
    expect(inWidth).toBeCloseTo(0.75);
  });

  it('lists zero segments with a 0 fraction so 0 kWh is never missing data', () => {
    const b = todayBalance(snap({ today_solar_kwh: 8 }));
    expect(b.inSegments.map((s) => [s.key, s.fraction])).toEqual([
      ['solar', 1],
      ['grid_import', 0],
      ['battery_out', 0],
    ]);
    expect(b.outSegments.every((s) => s.fraction === 0 && s.kwh === 0)).toBe(true);
  });

  it('counts non-finite and negative readings as 0', () => {
    const b = todayBalance(snap({
      today_solar_kwh: Number.NaN,
      today_import_kwh: -2,
      today_discharge_kwh: Number.POSITIVE_INFINITY,
      home_energy_today_kwh: -0.5,
      today_export_kwh: Number.NaN,
      today_charge_kwh: 3,
    }));
    expect(b.inTotal).toBe(0);
    expect(b.outTotal).toBe(3);
  });

  it('keeps fractions finite when nothing has been recorded', () => {
    const b = todayBalance(snap());
    expect(b.inTotal).toBe(0);
    expect(b.outTotal).toBe(0);
    expect(b.scaleKwh).toBe(0);
    expect([...b.inSegments, ...b.outSegments].every((s) => s.fraction === 0)).toBe(true);
  });

  it('flags the unmeasured solar on the models with no PV input of their own', () => {
    expect(todayBalance(snap({ device_type_code: '3001' })).acCoupledSolarUnmeasured).toBe(true);
    expect(todayBalance(snap({ device_type_code: '3002' })).acCoupledSolarUnmeasured).toBe(true);
    // Third code review #7: the residential All-in-Ones are AC-coupled too —
    // no PV strings (GivTCP's PV_STRING_VI_UNSUPPORTED), so Solar reads 0.
    for (const code of ['8001', '8002', '8003']) {
      expect(todayBalance(snap({ device_type_code: code })).acCoupledSolarUnmeasured).toBe(true);
    }
    // The AIO Hybrid has PV strings of its own.
    expect(todayBalance(snap({ device_type_code: '8201' })).acCoupledSolarUnmeasured).toBe(false);
    expect(todayBalance(snap({ device_type_code: '2201' })).acCoupledSolarUnmeasured).toBe(false);
    expect(todayBalance(snap({ device_type_code: '7001' })).acCoupledSolarUnmeasured).toBe(false);
    expect(todayBalance(snap({ device_type_code: '' })).acCoupledSolarUnmeasured).toBe(false);
  });
});

describe('balanceSummary', () => {
  it('says how much solar stayed home and how much came off the grid', () => {
    // 4.6 of 10 kWh solar kept home → 46%; 20.4 of 40 kWh in imported → 51%
    // (the numbers from the plan's example line).
    const summary = balanceSummary(todayBalance(snap({
      today_solar_kwh: 10,
      today_import_kwh: 20.4,
      today_discharge_kwh: 9.6,
      today_export_kwh: 5.4,
      home_energy_today_kwh: 30,
      today_charge_kwh: 12,
    })));
    // The battery discharged, so some export may have been battery energy:
    // solar less export is only the least that stayed home.
    expect(summary).toBe('At least 46% of solar used or stored at home · 51% of energy in came from the grid');
  });

  it('gives the exact solar share when the battery never discharged', () => {
    // No discharge: every exported kWh was solar.
    const summary = balanceSummary(todayBalance(snap({
      today_solar_kwh: 10,
      today_import_kwh: 2,
      today_export_kwh: 4,
      home_energy_today_kwh: 6,
      today_charge_kwh: 2,
    })));
    expect(summary).toBe('60% of solar used or stored at home · 17% of energy in came from the grid');
  });

  it('drops the solar share when battery export may account for all of it (second code review #6)', () => {
    // 10 kWh solar, 12 kWh exported after a Timed Export from the battery:
    // the old line said 0%, though most of the solar may well have stayed home.
    const summary = balanceSummary(todayBalance(snap({
      today_solar_kwh: 10,
      today_import_kwh: 4,
      today_discharge_kwh: 9,
      today_export_kwh: 12,
      home_energy_today_kwh: 7,
      today_charge_kwh: 4,
    })));
    expect(summary).not.toContain('solar');
    expect(summary).toContain(' of energy in came from the grid');
  });

  it('drops the solar share when there was no solar today', () => {
    const summary = balanceSummary(todayBalance(snap({
      today_import_kwh: 5,
      home_energy_today_kwh: 5,
    })));
    expect(summary).toBe('100% of energy in came from the grid');
  });

  it('drops the solar share on AC-coupled systems (the solar is unmeasured)', () => {
    const summary = balanceSummary(todayBalance(snap({
      device_type_code: '3001',
      today_solar_kwh: 0.2,
      today_import_kwh: 4.9,
      home_energy_today_kwh: 8,
      today_export_kwh: 5.1,
      today_charge_kwh: 2,
    })));
    expect(summary).not.toContain('solar');
    expect(summary).toContain(' of energy in came from the grid');
  });

  it('is null when there is nothing meaningful to say', () => {
    expect(balanceSummary(todayBalance(snap()))).toBeNull();
    expect(balanceSummary(todayBalance(snap({ device_type_code: '3001' })))).toBeNull();
  });
});

describe('todayBalance — car charging split (issue #454)', () => {
  const base = {
    today_solar_kwh: 12,
    today_import_kwh: 14,
    today_discharge_kwh: 1,
    home_energy_today_kwh: 16,
    today_export_kwh: 4,
    today_charge_kwh: 7,
  };
  const out = (b: ReturnType<typeof todayBalance>) =>
    Object.fromEntries(b.outSegments.map((s) => [s.key, s.kwh]));

  it('is identical without a car figure', () => {
    expect(todayBalance(snap(base), null)).toEqual(todayBalance(snap(base)));
    expect(todayBalance(snap(base), undefined)).toEqual(todayBalance(snap(base)));
  });

  it('carves the car out of Home Use and leaves the totals alone', () => {
    const plain = todayBalance(snap(base));
    const split = todayBalance(snap(base), 10.4);
    expect(split.outSegments.map((s) => [s.key, s.label])).toEqual([
      ['home_use', 'Home Use'],
      ['car', 'Car Charging'],
      ['export', 'Grid Export'],
      ['battery_in', 'Battery In'],
    ]);
    expect(out(split).home_use).toBeCloseTo(5.6, 9);
    expect(out(split).car).toBeCloseTo(10.4, 9);
    expect(split.outTotal).toBeCloseTo(plain.outTotal, 9);
    expect(split.inTotal).toBe(plain.inTotal);
    expect(split.scaleKwh).toBe(plain.scaleKwh);
  });

  it('keeps the segment fractions summing to the same share of the scale', () => {
    const plain = todayBalance(snap(base));
    const split = todayBalance(snap(base), 10.4);
    const sum = (b: ReturnType<typeof todayBalance>) => b.outSegments.reduce((t, s) => t + s.fraction, 0);
    expect(sum(split)).toBeCloseTo(sum(plain), 9);
  });

  it('never takes more than Home Use for the car, so nothing goes negative', () => {
    const split = todayBalance(snap(base), 40);
    expect(out(split).home_use).toBe(0);
    expect(out(split).car).toBe(16);
    expect(split.outTotal).toBeCloseTo(todayBalance(snap(base)).outTotal, 9);
  });

  it('lists a zero car as a segment that is not drawn, and keeps Home Use whole', () => {
    const split = todayBalance(snap(base), 0);
    expect(out(split).car).toBe(0);
    expect(out(split).home_use).toBe(16);
    expect(split.outSegments.find((s) => s.key === 'car')!.fraction).toBe(0);
  });

  it.each([NaN, -3, Infinity])('treats %s as no car', (bad) => {
    const split = todayBalance(snap(base), bad);
    expect(out(split).home_use).toBe(16);
    expect(out(split).car).toBe(0);
  });

  it('leaves the summary line alone', () => {
    expect(balanceSummary(todayBalance(snap(base), 10.4))).toBe(balanceSummary(todayBalance(snap(base))));
  });
});
