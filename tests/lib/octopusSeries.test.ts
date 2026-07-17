import { describe, expect, it } from 'vitest';
import {
  cumulativeOctopusSeries,
  mergeOctopusSeries,
  octopusSeriesTotal,
} from '../../src/lib/octopusSeries';

describe('Octopus supplier series', () => {
  it('merges independently-timestamped streams in chronological order', () => {
    expect(mergeOctopusSeries({
      electricity_import: [{ t: 20, v: 2 }, { t: 10, v: 1 }],
      electricity_export: [{ t: 10, v: 0.5 }],
    }, ['electricity_import', 'electricity_export'])).toEqual([
      { t: 10, electricity_import: 1, electricity_export: 0.5 },
      { t: 20, electricity_import: 2 },
    ]);
  });

  it('builds separate cumulative lines and carries sparse stream totals forward', () => {
    expect(cumulativeOctopusSeries({
      electricity_import: [{ t: 10, v: 1 }, { t: 20, v: 2 }],
      electricity_export: [{ t: 10, v: 0.5 }, { t: 30, v: 0.25 }],
    }, ['electricity_import', 'electricity_export'])).toEqual([
      { t: 10, electricity_import: 1, electricity_export: 0.5 },
      { t: 20, electricity_import: 3, electricity_export: 0.5 },
      { t: 30, electricity_import: 3, electricity_export: 0.75 },
    ]);
  });

  it('totals the selected period and ignores non-finite values', () => {
    expect(octopusSeriesTotal([
      { t: 10, v: 1.25 },
      { t: 20, v: Number.NaN },
      { t: 30, v: 2.5 },
    ])).toBe(3.75);
    expect(octopusSeriesTotal(undefined)).toBe(0);
  });
});
