import { rateForTimestamp } from './tariff';
import type { TariffConfig } from './types';

/** One Agile half-hour price, in pence/kWh. */
export interface SummaryPriceSlot {
  validFrom: Date;
  validTo: Date;
  pence: number;
}

export type PriceDecision = 'charge' | 'discharge' | 'nothing';

export interface AgilePriceSummaryInputs {
  /** The saved import tariff (see `savedTariff`). */
  importTariff: TariffConfig;
  /** The saved export tariff (see `savedTariff`). */
  exportTariff: TariffConfig;
  batteryKwh: number;
  now: Date;
}

export interface AgilePriceSummary {
  chargeCount: number;
  dischargeCount: number;
  idleCount: number;
  /** Mean Agile price of the charge / discharge half-hours, pence; 0 when none. */
  avgChargePrice: number;
  avgDischargePrice: number;
  minPrice: number;
  maxPrice: number;
  /** Mean pence/kWh a charge half-hour saves over the import rate at its time; null when none. */
  chargeGainP: number | null;
  /** Mean pence/kWh a discharge half-hour earns over the export rate at its time; null when none. */
  dischargeGainP: number | null;
  /** Mean import rate across the charge half-hours, pence; null when none. */
  standardImportP: number | null;
  /** Rough daily saving, £: one battery's throughput spread over the day's half-hours. */
  totalSaving: number;
}

const mean = (xs: number[]) => (xs.length ? xs.reduce((a, x) => a + x, 0) / xs.length : null);

/** Pence/kWh of `tariff` at the start of `slot`. Never null: `savedTariff` has slots. */
const rateP = (tariff: TariffConfig, slot: SummaryPriceSlot) =>
  (rateForTimestamp(tariff, slot.validFrom.getTime()) ?? 0) * 100;

/**
 * The Agile price summary on the Control page (issue #485): each charge or
 * discharge half-hour is measured against the saved tariff's rate at that
 * half-hour, so a time-of-day tariff compares like with like.
 */
export function agilePriceSummary(
  prices: SummaryPriceSlot[],
  decide: (pence: number) => PriceDecision,
  { importTariff, exportTariff, batteryKwh, now }: AgilePriceSummaryInputs,
): AgilePriceSummary {
  const upcoming = prices.filter((s) => s.validTo > now);
  const charge = upcoming.filter((s) => decide(s.pence) === 'charge');
  const discharge = upcoming.filter((s) => decide(s.pence) === 'discharge');
  const pence = upcoming.map((s) => s.pence);

  const chargeGainP = mean(charge.map((s) => rateP(importTariff, s) - s.pence));
  const dischargeGainP = mean(discharge.map((s) => s.pence - rateP(exportTariff, s)));
  const saving = (count: number, gainP: number | null) =>
    (count / 48) * batteryKwh * Math.max(0, (gainP ?? 0) / 100);

  return {
    chargeCount: charge.length,
    dischargeCount: discharge.length,
    idleCount: upcoming.length - charge.length - discharge.length,
    avgChargePrice: mean(charge.map((s) => s.pence)) ?? 0,
    avgDischargePrice: mean(discharge.map((s) => s.pence)) ?? 0,
    minPrice: pence.length ? Math.min(...pence) : 0,
    maxPrice: pence.length ? Math.max(...pence) : 0,
    chargeGainP,
    dischargeGainP,
    standardImportP: mean(charge.map((s) => rateP(importTariff, s))),
    totalSaving: saving(charge.length, chargeGainP) + saving(discharge.length, dischargeGainP),
  };
}
