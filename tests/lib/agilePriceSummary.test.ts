import { describe, expect, test } from 'vitest';
import { agilePriceSummary, type SummaryPriceSlot } from '../../src/lib/agilePriceSummary';
import { flatTariffConfig, savedTariff } from '../../src/lib/tariff';
import type { TariffConfig } from '../../src/lib/types';

/** A half-hour starting at local `hh:mm` on 18 Nov 2026. */
function slot(hh: number, mm: number, pence: number): SummaryPriceSlot {
  const validFrom = new Date(2026, 10, 18, hh, mm);
  return { validFrom, validTo: new Date(validFrom.getTime() + 30 * 60_000), pence };
}

const NOW = new Date(2026, 10, 18, 0, 0);
const decide = (p: number) => (p <= 10 ? 'charge' : p >= 30 ? 'discharge' : 'nothing') as const;

const OFF_PEAK: TariffConfig = {
  slots: [
    { start: '00:00', end: '05:30', rate: 0.07 },
    { start: '05:30', end: '23:59', rate: 0.3 },
  ],
};

describe('savedTariff', () => {
  test('prefers the saved time-of-day tariff over the legacy single rate', () => {
    expect(savedTariff(OFF_PEAK, 0.15, 0.2)).toBe(OFF_PEAK);
  });

  test('falls back to the legacy single rate when no tariff is saved', () => {
    expect(savedTariff(null, 0.15, 0.2)).toEqual(flatTariffConfig(0.15));
    expect(savedTariff(undefined, 0.15, 0.2)).toEqual(flatTariffConfig(0.15));
  });

  test('treats a saved tariff with no slots as not saved', () => {
    expect(savedTariff({ slots: [] }, 0.15, 0.2)).toEqual(flatTariffConfig(0.15));
  });

  test('uses the default rate when neither is present', () => {
    expect(savedTariff(null, null, 0.2)).toEqual(flatTariffConfig(0.2));
    expect(savedTariff(null, Number.NaN, 0.2)).toEqual(flatTariffConfig(0.2));
  });
});

describe('agilePriceSummary', () => {
  test('measures discharge earnings against the saved export tariff', () => {
    const s = agilePriceSummary([slot(17, 0, 34)], decide, {
      importTariff: flatTariffConfig(0.285),
      exportTariff: flatTariffConfig(0.04),
      batteryKwh: 9.5,
      now: NOW,
    });
    expect(s.dischargeGainP).toBeCloseTo(30);
    expect(s.totalSaving).toBeCloseTo((1 / 48) * 9.5 * 0.3);
  });

  test('measures each charge slot against the import rate at its own time', () => {
    // 02:00 sits in the 7p off-peak, 12:00 in the 30p peak.
    const s = agilePriceSummary([slot(2, 0, 5), slot(12, 0, 8)], decide, {
      importTariff: OFF_PEAK,
      exportTariff: flatTariffConfig(0.15),
      batteryKwh: 10,
      now: NOW,
    });
    expect(s.chargeGainP).toBeCloseTo(((7 - 5) + (30 - 8)) / 2);
    expect(s.standardImportP).toBeCloseTo((7 + 30) / 2);
  });

  test('ignores half-hours that have already ended', () => {
    const past = slot(23, 0, 40);
    past.validFrom = new Date(2026, 10, 17, 23, 0);
    past.validTo = new Date(2026, 10, 17, 23, 30);
    const s = agilePriceSummary([past, slot(1, 0, 20)], decide, {
      importTariff: flatTariffConfig(0.285),
      exportTariff: flatTariffConfig(0.15),
      batteryKwh: 10,
      now: NOW,
    });
    expect(s.dischargeCount).toBe(0);
    expect(s.idleCount).toBe(1);
    expect(s.minPrice).toBe(20);
    expect(s.maxPrice).toBe(20);
  });

  test('no gain and no saving when the Agile price is worse than the standard rate', () => {
    const s = agilePriceSummary([slot(17, 0, 32)], decide, {
      importTariff: flatTariffConfig(0.285),
      exportTariff: flatTariffConfig(0.4),
      batteryKwh: 10,
      now: NOW,
    });
    expect(s.dischargeGainP).toBeCloseTo(-8);
    expect(s.totalSaving).toBe(0);
  });

  test('empty forecast gives zero counts, no gains and no saving', () => {
    const s = agilePriceSummary([], decide, {
      importTariff: flatTariffConfig(0.285),
      exportTariff: flatTariffConfig(0.15),
      batteryKwh: 10,
      now: NOW,
    });
    expect(s).toMatchObject({
      chargeCount: 0,
      dischargeCount: 0,
      idleCount: 0,
      chargeGainP: null,
      dischargeGainP: null,
      standardImportP: null,
      minPrice: 0,
      maxPrice: 0,
      totalSaving: 0,
    });
  });
});
