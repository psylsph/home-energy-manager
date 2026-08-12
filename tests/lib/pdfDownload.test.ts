import { describe, it, expect, vi, beforeEach } from 'vitest';
import type { jsPDF } from 'jspdf';
import type { OctopusExportData } from '../../src/lib/octopusExport';

// ---------------------------------------------------------------------------
// downloadOctopusSummaryPdf() is a thin orchestrator: it delegates to
// buildOctopusSummaryPdf() and then calls .save(fileName) on the returned
// jsPDF instance. We mock the export module so we can assert (a) that
// buildOctopusSummaryPdf received the data unchanged, (b) that .save() was
// invoked with the exact file name, and (c) that the promise rejects
// propagate when either step fails. The mock is hoisted above the import of
// the module under test.
// ---------------------------------------------------------------------------

// vi.mock factories are hoisted above every import and every top-level const,
// so the mock function must live inside vi.hoisted() to be in scope when the
// factory runs.
const { buildOctopusSummaryPdf } = vi.hoisted(() => ({
  buildOctopusSummaryPdf: vi.fn<(data: OctopusExportData) => Promise<jsPDF>>(),
}));
vi.mock('../../src/lib/octopusExport', () => ({ buildOctopusSummaryPdf }));

import { downloadOctopusSummaryPdf } from '../../src/lib/octopusPdfDownload';

/** Minimal OctopusExportData fixture — only identity matters here, not shape. */
function fixture(): OctopusExportData {
  const summary = {
    electricity_import_kwh: 1.5,
    electricity_export_kwh: 0.5,
    gas_usage: 1.0,
    electricity_energy_cost_gbp: 2.0,
    electricity_standing_cost_gbp: 0.5,
    electricity_total_cost_gbp: 2.5,
    export_income_gbp: 0.1,
    gas_energy_cost_gbp: 0.4,
    gas_standing_cost_gbp: 0.2,
    gas_total_cost_gbp: 0.6,
    net_cost_gbp: 2.8,
    pricing_complete: true,
  };
  return {
    rangeLabel: '30 days <test>',
    generatedAt: new Date('2026-07-17T12:00:00Z'),
    gasUnit: 'kwh',
    costPeriods: [{ ...summary, period: '2026-07-17' }],
    historySeries: {},
    billing: {
      totals: summary,
      daily: [{ ...summary, period: '2026-07-17' }],
      monthly: [{ ...summary, period: '2026-07' }],
      yearly: [{ ...summary, period: '2026' }],
      gas_cost_available: true,
    },
    comparison: {
      totals: {
        octopus_import_kwh: 1.5,
        hem_import_kwh: 1.4,
        import_difference_kwh: -0.1,
        octopus_export_kwh: 0.5,
        hem_export_kwh: 0.6,
        export_difference_kwh: 0.1,
        expected_import_intervals: 48,
        import_intervals: 48,
        missing_import_intervals: 0,
        expected_export_intervals: 48,
        export_intervals: 48,
        missing_export_intervals: 0,
        expected_gas_intervals: 48,
        gas_intervals: 48,
        missing_gas_intervals: 0,
      },
      days: [],
      import_stream_available: true,
      export_stream_available: true,
      gas_stream_available: true,
    },
  };
}

/** Build a fake jsPDF whose only observable method is save(). */
function fakePdf(): jsPDF {
  return { save: vi.fn() } as unknown as jsPDF;
}

describe('downloadOctopusSummaryPdf', () => {
  beforeEach(() => buildOctopusSummaryPdf.mockReset());

  it('builds a PDF from the provided data and saves it under the given file name', async () => {
    const data = fixture();
    const pdf = fakePdf();
    buildOctopusSummaryPdf.mockResolvedValue(pdf);

    await downloadOctopusSummaryPdf(data, 'octopus-summary.pdf');

    expect(buildOctopusSummaryPdf).toHaveBeenCalledOnce();
    expect(buildOctopusSummaryPdf).toHaveBeenCalledWith(data);
    expect(pdf.save).toHaveBeenCalledOnce();
    expect(pdf.save).toHaveBeenCalledWith('octopus-summary.pdf');
  });

  it('passes the data object to the builder unchanged (no mutation or cloning)', async () => {
    const data = fixture();
    buildOctopusSummaryPdf.mockResolvedValue(fakePdf());

    await downloadOctopusSummaryPdf(data, 'report.pdf');

    expect(buildOctopusSummaryPdf).toHaveBeenCalledWith(data);
    // Same reference — the function must not clone or restructure the data.
    expect(buildOctopusSummaryPdf.mock.calls[0][0]).toBe(data);
  });

  it('passes the exact file name to save() without altering extension or path', async () => {
    const pdf = fakePdf();
    buildOctopusSummaryPdf.mockResolvedValue(pdf);

    await downloadOctopusSummaryPdf(fixture(), 'my report (2026-07).pdf');

    expect(pdf.save).toHaveBeenCalledWith('my report (2026-07).pdf');
  });

  it('resolves to undefined on success', async () => {
    buildOctopusSummaryPdf.mockResolvedValue(fakePdf());

    await expect(
      downloadOctopusSummaryPdf(fixture(), 'octopus-summary.pdf'),
    ).resolves.toBeUndefined();
  });

  it('does not call save() when buildOctopusSummaryPdf rejects', async () => {
    const pdf = fakePdf();
    buildOctopusSummaryPdf.mockRejectedValueOnce(new Error('pdf build failed'));

    await expect(
      downloadOctopusSummaryPdf(fixture(), 'octopus-summary.pdf'),
    ).rejects.toThrow('pdf build failed');

    expect(buildOctopusSummaryPdf).toHaveBeenCalledOnce();
    expect(pdf.save).not.toHaveBeenCalled();
  });

  it('propagates a save() failure as a rejection', async () => {
    const pdf = fakePdf();
    (pdf.save as ReturnType<typeof vi.fn>).mockImplementation(() => {
      throw new Error('disk full');
    });
    buildOctopusSummaryPdf.mockResolvedValue(pdf);

    await expect(
      downloadOctopusSummaryPdf(fixture(), 'octopus-summary.pdf'),
    ).rejects.toThrow('disk full');

    expect(pdf.save).toHaveBeenCalledOnce();
  });

  it('awaits build before saving (save runs strictly after build resolves)', async () => {
    const pdf = fakePdf();
    buildOctopusSummaryPdf.mockResolvedValue(pdf);

    const promise = downloadOctopusSummaryPdf(fixture(), 'octopus-summary.pdf');
    // The function awaits build internally, so by the time the microtask
    // queue drains, save() should have been called.
    await promise;

    expect(buildOctopusSummaryPdf).toHaveBeenCalledOnce();
    expect(pdf.save).toHaveBeenCalledOnce();
    expect(pdf.save).toHaveBeenCalledWith('octopus-summary.pdf');
  });
});
