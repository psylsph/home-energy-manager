import { describe, it, expect } from 'vitest';
import {
  calculatePowerReport,
  integratePair,
  positivePart,
  type PowerRow,
} from '../../src/pages/powerReport';

// ---------------------------------------------------------------------------
// Regression tests for the directional sign-cancellation bug fixed in
// PR #166: the Consumption Report's directional energy + peak figures
// must integrate / peak the server-split magnitudes (`chargePower` /
// `dischargePower` / `gridImportPower` / `gridExportPower`), not re-split
// the net signed `battery_power` / `grid_power` columns. Integrating the
// net would let a bucket's import and export (charge and discharge) cancel
// before integration, collapsing the directional sums toward 0.
//
// These tests pin:
//   - the directional integration totals (the bug PR #166 closes)
//   - the directional peak figures (max of per-row magnitudes)
//   - the trapezoidal math via `integratePair` / `positivePart` directly
//   - the route through `calculatePowerReport`, including bucket
//     granularity (hour / day / month) and the max-gap filter
//
// The net `battery_power` / `grid_power` fields are NOT exercised - the
// Consumption Report's directional math is intentionally independent of
// them (they drive the chart, not the report).
// ---------------------------------------------------------------------------

/** Build a `PowerRow` with only the directional fields populated. */
function row(t: number, override: Partial<PowerRow> = {}): PowerRow {
  return {
    t,
    solarPower: null,
    batteryPower: null,
    gridPower: null,
    homePower: null,
    soc: null,
    chargePower: null,
    dischargePower: null,
    gridImportPower: null,
    gridExportPower: null,
    ...override,
  };
}

describe('powerReport — directional charge / discharge integration (PR #166)', () => {
  it('integrates a mixed-direction battery series without cancellation', () => {
    // Two readings 1h apart, the bucket fully inside the domain. Battery
    // field is empty (the report's directional energy ignores it), but the
    // directional series carries:
    //   - charging 1500 W for the first half, 500 W for the second half
    //   - discharging 800 W for the first half, 1200 W for the second half
    // Trapezoidal:
    //   batteryCharge    = ((1500+500)/2) * 1h / 1000  = 1.0  kWh
    //   batteryDischarge = ((800+1200)/2) * 1h / 1000  = 1.0  kWh
    // The OLD net-split code would have returned ~0 for both because the
    // pre-bucket average of the signed net cancelled out.
    const t0 = 1_700_000_000_000;
    const rows: PowerRow[] = [
      row(t0, {
        chargePower: 1500,
        dischargePower: 800,
        gridImportPower: 100,
        gridExportPower: 200,
      }),
      row(t0 + 3_600_000, {
        chargePower: 500,
        dischargePower: 1200,
        gridImportPower: 300,
        gridExportPower: 0,
      }),
    ];

    const report = calculatePowerReport(rows, '1h', [t0 - 1, t0 + 3_600_001], 0, 'Test');

    expect(report.summary.batteryChargeKwh).toBeCloseTo(1.0, 6);
    expect(report.summary.batteryDischargeKwh).toBeCloseTo(1.0, 6);
    expect(report.summary.importKwh).toBeCloseTo(0.2, 6);
    expect(report.summary.exportKwh).toBeCloseTo(0.1, 6);
  });

  it('reports peak W from the directional magnitudes, not the net', () => {
    // Two readings; charge peaks at 2500 W in the second reading,
    // discharge peaks at 1800 W in the first.
    const t0 = 1_700_100_000_000;
    const rows: PowerRow[] = [
      row(t0, { chargePower: 1000, dischargePower: 1800, gridImportPower: 5000 }),
      row(t0 + 60_000, { chargePower: 2500, dischargePower: 1200, gridExportPower: 4200 }),
    ];

    const report = calculatePowerReport(rows, '1h', [t0 - 1, t0 + 60_001], 0, 'Test');

    expect(report.summary.peakBatteryChargeW).toBe(2500);
    expect(report.summary.peakBatteryDischargeW).toBe(1800);
    expect(report.summary.peakGridImportW).toBe(5000);
    expect(report.summary.peakGridExportW).toBe(4200);
  });

  it('does NOT let mixed-direction data cancel within one bucket', () => {
    // Bypass the time-domain filter by stuffing everything into the same
    // hour-bucket with 1-second spacing - the report has to walk each
    // pair regardless of bucket structure. Two reads, battery in opposite
    // directions: the OLD code's net split would have one direction equal
    // to the magnitude and the other equal to zero. PR #166 splits before
    // integration, so each direction sees its full magnitude independently.
    const t0 = 1_700_200_000_000;
    const dt = 1_000; // 1s between readings
    const rows: PowerRow[] = [
      row(t0, { chargePower: 2000, dischargePower: 0, gridImportPower: 3000, gridExportPower: 0 }),
      row(t0 + dt, { chargePower: 0, dischargePower: 2000, gridImportPower: 0, gridExportPower: 3000 }),
    ];

    const report = calculatePowerReport(rows, '1h', [t0 - 1, t0 + dt + 1], 0, 'Test');

    // Trapezoid of (2000, 0) over 1s with 1000-W reduction:
    //   ((2000 + 0) / 2) * (1000ms / 3600000) / 1000 = 0.0002778 kWh
    // (both directions contribute the same shape of trapzoid under
    // positivePart, so both totals land on this small value.)
    const expectedKwh = (2000 / 2) * (dt / 3_600_000) / 1000;
    expect(report.summary.batteryChargeKwh).toBeGreaterThan(0);
    expect(report.summary.batteryDischargeKwh).toBeGreaterThan(0);
    expect(report.summary.batteryChargeKwh).toBeCloseTo(expectedKwh, 6);
    expect(report.summary.batteryDischargeKwh).toBeCloseTo(expectedKwh, 6);
    // Grid uses different magnitudes (3000 W), so its expected kWh differs:
    //   ((3000 + 0) / 2) * (1000ms / 3600000) / 1000 = 0.0004167 kWh
    const expectedGridKwh = (3000 / 2) * (dt / 3_600_000) / 1000;
    expect(report.summary.importKwh).toBeCloseTo(expectedGridKwh, 6);
    expect(report.summary.exportKwh).toBeCloseTo(expectedGridKwh, 6);
  });

  it('produces the same totals regardless of how readings are split across buckets', () => {
    // Pin: bucketing is a display concern (per the chosen range's
    // granularity); the summary totals integrate over the full domain.
    // Two datasets that produce the same kWh after integration should
    // give the same summary, regardless of where the bucket boundaries
    // fall.
    const t0 = 1_700_300_000_000;
    const long: PowerRow[] = [
      row(t0, { chargePower: 1000 }),
      row(t0 + 3_600_000, { chargePower: 500 }),
    ];
    // Identical readings, but with a synthetic halfway sample that does
    // not change the trapzoidal integral of the directional series.
    const split: PowerRow[] = [
      row(t0, { chargePower: 1000 }),
      row(t0 + 1_800_000, { chargePower: 750 }),
      row(t0 + 3_600_000, { chargePower: 500 }),
    ];
    const domain: [number, number] = [t0 - 1, t0 + 3_600_001];
    const a = calculatePowerReport(long, '1h', domain, 0, 'A');
    const b = calculatePowerReport(split, '1h', domain, 0, 'B');
    expect(a.summary.batteryChargeKwh).toBeCloseTo(b.summary.batteryChargeKwh, 6);
  });

  it('does not sum pairs across a large gap in the readings', () => {
    // Median-interval × 3.5 = maxGapMs: a gap more than 3.5× the typical
    // interval is treated as a discontinuity and skipped. Build 5 rows so
    // the median settles on the typical 1-minute interval - with only 2
    // or 3 rows, the median can drift onto the gap itself and disable the
    // filter (PR #166 doesn't change this; the median-of-N behaviour is
    // pre-existing and just easier to reason about with N >= 5).
    const t0 = 1_700_400_000_000;
    const minute = 60_000;
    const rows: PowerRow[] = [
      row(t0, { chargePower: 2000 }),
      row(t0 + minute, { chargePower: 2000 }),
      row(t0 + 2 * minute, { chargePower: 2000 }),
      row(t0 + 3 * minute, { chargePower: 2000 }),
      // 2-week gap after the close cluster; the 4 close intervals settle
      // the median at 1 minute, so 3.5× is ~3.5 minutes and the 2-week
      // gap blows past the filter.
      row(t0 + 3 * minute + 14 * 86_400_000, { chargePower: 2000 }),
    ];
    const report = calculatePowerReport(
      rows,
      '1y',
      [t0 - 1, t0 + 3 * minute + 14 * 86_400_000 + 1],
      0,
      'Test',
    );
    // Three pairs integrate cleanly (4 close rows = 3 close pairs); the
    // fourth pair (last close → far row) is skipped because the gap is
    // too big. Trapezoid of (2000, 2000) over 1 min:
    //   ((2000 + 2000) / 2) * (1 min / 60 min/hr) / 1000 = 0.0333 kWh each
    // The sum across the 3 close pairs ≈ 3 × 0.0333 ≈ 0.1 kWh.
    const perPairKwh = 2000 * (minute / 3_600_000) / 1000;
    expect(report.summary.batteryChargeKwh).toBeCloseTo(perPairKwh * 3, 6);
  });
});

describe('powerReport — primitives', () => {
  it('positivePart clamps negatives to 0 and treats null as 0', () => {
    expect(positivePart(5)).toBe(5);
    expect(positivePart(-3)).toBe(0);
    expect(positivePart(0)).toBe(0);
    expect(positivePart(null)).toBe(0);
    expect(positivePart(undefined)).toBe(0);
  });

  it('integratePair averages two readings × hours / 1000', () => {
    // 1000 W + 0 W over 1 hour = 0.5 kWh.
    expect(integratePair(1000, 0, 1, positivePart)).toBeCloseTo(0.5, 6);
    // Constant 1000 W over 1 hour = 1 kWh.
    expect(integratePair(1000, 1000, 1, positivePart)).toBeCloseTo(1.0, 6);
    // Both null = 0.
    expect(integratePair(null, null, 1, positivePart)).toBe(0);
    // Single-sided: uses that side's value (avg = the value × hours).
    expect(integratePair(null, 2000, 1, positivePart)).toBeCloseTo(2.0, 6);
    expect(integratePair(1500, null, 1, positivePart)).toBeCloseTo(1.5, 6);
  });

  it('integratePair uses the transform before averaging (no sign cancellation)', () => {
    // 2000 W and -2000 W over 1 hour WITHOUT the positivePart transform
    // would trapzoidally average to 0, which is exactly the bug PR #166
    // fixes. With positivePart, the trapzoid sees (2000, 0) over 1 hour
    // → 1 kWh.
    expect(integratePair(2000, -2000, 1, positivePart)).toBeCloseTo(1.0, 6);
  });

  it('calculatePowerReport returns zero reports for an empty input', () => {
    const report = calculatePowerReport([], '24h', [0, 1], 0, 'Empty');
    expect(report.summary.batteryChargeKwh).toBe(0);
    expect(report.summary.batteryDischargeKwh).toBe(0);
    expect(report.summary.importKwh).toBe(0);
    expect(report.summary.exportKwh).toBe(0);
    expect(report.summary.solarKwh).toBe(0);
    expect(report.summary.homeKwh).toBe(0);
    expect(report.buckets).toEqual([]);
    // Coverage / dependency percentages are null when there is no load.
    expect(report.summary.solarCoveragePct).toBeNull();
    expect(report.summary.gridDependencyPct).toBeNull();
  });
});
