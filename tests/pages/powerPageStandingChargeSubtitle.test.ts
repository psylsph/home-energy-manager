/**
 * Precision coverage for the standing-charge subtitle on the Power page.
 *
 * `standingChargeSubtitle` renders the p/day figure to 3 decimal places
 * (e.g. 54.860p/day), matching the app-wide 3dp standard for pence-denominated
 * tariff figures. The £ amount on the same card stays at 2dp (currency).
 */
import { describe, it, expect } from 'vitest';
import { standingChargeSubtitle } from '../../src/pages/powerReport';
import type { PowerReportSummary } from '../../src/pages/powerReport';

function makeSummary(overrides: Partial<PowerReportSummary>): PowerReportSummary {
  return {
    importCostGbp: 0,
    exportIncomeGbp: 0,
    netCostGbp: 0,
    standingChargeGbp: 0,
    standingChargePPerDay: 0,
    daysInRange: 0,
    ...overrides,
  } as PowerReportSummary;
}

describe('standingChargeSubtitle — p/day renders to 3dp', () => {
  it('renders the configured p/day figure to 3 decimal places', () => {
    const out = standingChargeSubtitle(makeSummary({ standingChargePPerDay: 54.86, daysInRange: 7 }));
    expect(out).toContain('54.860p/day');
    expect(out).not.toContain('54.86p/day');
  });

  it('renders a sub-penny standing charge without truncation', () => {
    const out = standingChargeSubtitle(
      makeSummary({ standingChargePPerDay: 12.345, daysInRange: 2 }),
    );
    expect(out).toContain('12.345p/day');
  });

  it('returns the empty string when no standing charge is configured', () => {
    expect(standingChargeSubtitle(makeSummary({ standingChargePPerDay: 0, daysInRange: 7 }))).toBe('');
  });
});
