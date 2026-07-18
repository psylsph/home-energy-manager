import { describe, expect, it } from 'vitest';
import {
  buildOctopusCostSeries,
  buildOctopusSummaryCsv,
  buildOctopusSummaryPdf,
  type OctopusExportData,
} from '../../src/lib/octopusExport';

function fixture(): OctopusExportData {
  const summary = {
    electricity_import_kwh: 12.345,
    electricity_export_kwh: 4.5,
    gas_usage: 8.25,
    electricity_energy_cost_gbp: 2.5,
    electricity_standing_cost_gbp: 0.6,
    electricity_total_cost_gbp: 3.1,
    export_income_gbp: 0.68,
    gas_energy_cost_gbp: 0.8,
    gas_standing_cost_gbp: 0.3,
    gas_total_cost_gbp: 1.1,
    net_cost_gbp: 3.52,
    pricing_complete: true,
  };
  return {
    rangeLabel: '30 days <test>',
    generatedAt: new Date('2026-07-17T12:00:00Z'),
    gasUnit: 'kwh',
    costPeriods: [{ ...summary, period: '2026-07-17' }],
    billing: {
      totals: summary,
      daily: [{ ...summary, period: '2026-07-17' }],
      monthly: [{ ...summary, period: '2026-07' }],
      yearly: [{ ...summary, period: '2026' }],
      gas_cost_available: true,
    },
    comparison: {
      totals: {
        octopus_import_kwh: 12.345,
        hem_import_kwh: 12.5,
        import_difference_kwh: 0.155,
        octopus_export_kwh: 4.5,
        hem_export_kwh: 4.4,
        export_difference_kwh: -0.1,
        expected_import_intervals: 48,
        import_intervals: 47,
        missing_import_intervals: 1,
        expected_export_intervals: 48,
        export_intervals: 48,
        missing_export_intervals: 0,
        expected_gas_intervals: 48,
        gas_intervals: 46,
        missing_gas_intervals: 2,
      },
      days: [{
        date: '2026-07-16',
        octopus_import_kwh: 12.345,
        hem_import_kwh: 12.5,
        import_difference_kwh: 0.155,
        import_difference_percent: 1.3,
        octopus_export_kwh: 4.5,
        hem_export_kwh: 4.4,
        export_difference_kwh: -0.1,
        export_difference_percent: -2.2,
        expected_import_intervals: 48,
        import_intervals: 47,
        missing_import_intervals: 1,
        expected_export_intervals: 48,
        export_intervals: 48,
        missing_export_intervals: 0,
        expected_gas_intervals: 48,
        gas_intervals: 46,
        missing_gas_intervals: 2,
      }],
      import_stream_available: true,
      export_stream_available: true,
      gas_stream_available: true,
    },
  };
}

describe('Octopus summary exports', () => {
  it('builds a CSV containing billing, comparison, and missing-data sections', () => {
    const csv = buildOctopusSummaryCsv(fixture());
    expect(csv).toContain('Octopus Energy Summary');
    expect(csv).toContain('Monthly Summary');
    expect(csv).toContain('Yearly Summary');
    expect(csv).toContain('HEM Comparison Totals');
    expect(csv).toContain('Daily Comparison and Missing Data');
    expect(csv).toContain('2026-07-16,12.345,12.500,0.155');
    expect(csv).toContain(',47,48,1,48,48,0,46,48,2');
  });

  it('leaves unavailable gas costs blank in CSV rather than inventing zero', () => {
    const data = fixture();
    data.billing.totals.gas_energy_cost_gbp = null;
    data.billing.totals.gas_total_cost_gbp = null;
    const csv = buildOctopusSummaryCsv(data);
    expect(csv).toContain('Gas energy cost GBP,\n');
    expect(csv).toContain('Gas total cost GBP,\n');
  });

  it('builds a real PDF document without needing a popup', async () => {
    const pdf = await buildOctopusSummaryPdf(fixture());
    expect(pdf.getNumberOfPages()).toBeGreaterThan(0);
    expect(pdf.output('arraybuffer').byteLength).toBeGreaterThan(1_000);
  });

  it('builds graph points with explicit unavailable gas costs and net fallback', () => {
    const data = fixture();
    data.billing.daily[0].gas_total_cost_gbp = null;
    data.billing.daily[0].net_cost_gbp = null;
    const points = buildOctopusCostSeries(data.billing.daily);
    expect(points).toEqual([expect.objectContaining({
      period: '2026-07-17',
      electricity_import_cost: 3.1,
      gas_cost: null,
      export_income: 0.68,
      net_cost: 2.42,
    })]);
  });
});
