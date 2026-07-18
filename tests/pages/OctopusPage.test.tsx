import { cleanup, render, screen, waitFor, fireEvent } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(),
  apiPost: vi.fn(),
}));

const { pdfDownloadMock } = vi.hoisted(() => ({ pdfDownloadMock: vi.fn() }));
vi.mock('../../src/lib/octopusPdfDownload', () => ({
  downloadOctopusSummaryPdf: pdfDownloadMock,
}));

vi.mock('recharts', () => ({
  ResponsiveContainer: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  AreaChart: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  LineChart: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  Area: () => null,
  Line: () => null,
  CartesianGrid: () => null,
  Legend: () => null,
  Tooltip: () => null,
  XAxis: () => null,
  YAxis: () => null,
}));

import OctopusPage from '../../src/pages/OctopusPage';
import { apiGet, apiPost } from '../../src/lib/api';

const createObjectUrlMock = vi.fn(() => 'blob:octopus-summary');
const revokeObjectUrlMock = vi.fn();
const anchorClickMock = vi.fn();

const status = {
  ok: true,
  configured: true,
  data: {
    syncing: false,
    last_sync_at: '2026-07-17T12:00:00Z',
    last_error: null,
    backfill_complete: false,
    discovered_streams: 3,
    imported_intervals: 20,
  },
  bounds: null,
  gas_unit_note: 'Gas values are supplier-reported units.',
};

describe('OctopusPage', () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  beforeEach(() => {
    createObjectUrlMock.mockClear();
    revokeObjectUrlMock.mockClear();
    anchorClickMock.mockClear();
    pdfDownloadMock.mockClear();
    Object.defineProperty(URL, 'createObjectURL', { configurable: true, value: createObjectUrlMock });
    Object.defineProperty(URL, 'revokeObjectURL', { configurable: true, value: revokeObjectUrlMock });
    vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(anchorClickMock);
    vi.mocked(apiGet).mockImplementation(async (path: string) => {
      if (path === '/api/octopus/status') return status;
      if (path.startsWith('/api/octopus/comparison')) return {
        ok: true,
        data: {
          totals: {
            octopus_import_kwh: 1.25, hem_import_kwh: 1.3, import_difference_kwh: 0.05,
            octopus_export_kwh: 0.5, hem_export_kwh: 0.48, export_difference_kwh: -0.02,
            expected_import_intervals: 48, import_intervals: 47, missing_import_intervals: 1,
            expected_export_intervals: 48, export_intervals: 48, missing_export_intervals: 0,
            expected_gas_intervals: 48, gas_intervals: 46, missing_gas_intervals: 2,
          },
          days: [{
            date: '2026-07-17',
            octopus_import_kwh: 1.25, hem_import_kwh: 1.3,
            import_difference_kwh: 0.05, import_difference_percent: 4,
            octopus_export_kwh: 0.5, hem_export_kwh: 0.48,
            export_difference_kwh: -0.02, export_difference_percent: -4,
            expected_import_intervals: 48, import_intervals: 47, missing_import_intervals: 1,
            expected_export_intervals: 48, export_intervals: 48, missing_export_intervals: 0,
            expected_gas_intervals: 48, gas_intervals: 46, missing_gas_intervals: 2,
          }],
          import_stream_available: true,
          export_stream_available: true,
          gas_stream_available: true,
        },
      };
      if (path.startsWith('/api/octopus/summary')) return {
        ok: true,
        gas_unit: 'kwh',
        estimated: true,
        data: {
          gas_cost_available: true,
          totals: {
            electricity_import_kwh: 1.25, electricity_export_kwh: 0.5, gas_usage: 3.5,
            electricity_energy_cost_gbp: 0.25, electricity_standing_cost_gbp: 0.5,
            electricity_total_cost_gbp: 0.75, export_income_gbp: 0.08,
            gas_energy_cost_gbp: 0.2, gas_standing_cost_gbp: 0.3,
            gas_total_cost_gbp: 0.5, net_cost_gbp: 1.17, pricing_complete: true,
          },
          daily: [{
            period: '2026-07-17', electricity_import_kwh: 1.25, electricity_export_kwh: 0.5,
            gas_usage: 3.5, electricity_energy_cost_gbp: 0.25,
            electricity_standing_cost_gbp: 0.5, electricity_total_cost_gbp: 0.75,
            export_income_gbp: 0.08, gas_energy_cost_gbp: 0.2,
            gas_standing_cost_gbp: 0.3, gas_total_cost_gbp: 0.5,
            net_cost_gbp: 1.17, pricing_complete: true,
          }],
          monthly: [{
            period: '2026-07', electricity_import_kwh: 1.25, electricity_export_kwh: 0.5,
            gas_usage: 3.5, electricity_energy_cost_gbp: 0.25,
            electricity_standing_cost_gbp: 0.5, electricity_total_cost_gbp: 0.75,
            export_income_gbp: 0.08, gas_energy_cost_gbp: 0.2,
            gas_standing_cost_gbp: 0.3, gas_total_cost_gbp: 0.5,
            net_cost_gbp: 1.17, pricing_complete: true,
          }],
          yearly: [],
        },
      };
      return {
        ok: true,
        data: {
          electricity_import: [{ t: 1_700_000_000_000, v: 1.25 }],
          electricity_export: [{ t: 1_700_000_000_000, v: 0.5 }],
          gas: [{ t: 1_700_000_000_000, v: 3.5 }],
        },
      };
    });
    vi.mocked(apiPost).mockResolvedValue({ ok: true });
  });

  it('renders supplier electricity and gas on their own page with the unit warning', async () => {
    render(<OctopusPage />);
    expect(await screen.findByText('Electricity consumption')).toBeDefined();
    expect(screen.getByText('Gas consumption')).toBeDefined();
    expect(screen.getByText('Cumulative electricity')).toBeDefined();
    expect(screen.getByText('Cumulative gas')).toBeDefined();
    expect(screen.getAllByText('1.250 kWh').length).toBeGreaterThan(0);
    expect(screen.getAllByText('0.500 kWh').length).toBeGreaterThan(0);
    expect(screen.getAllByText('3.500').length).toBeGreaterThan(0);
    expect(screen.getByText('Gas values are supplier-reported units.')).toBeDefined();
    expect(screen.getByText('3 meter stream(s)')).toBeDefined();
    expect(screen.getByText('Older history backfilling')).toBeDefined();
    expect(screen.getByText('Estimated supplier costs')).toBeDefined();
    expect(screen.getByText('Monthly summary')).toBeDefined();
    expect(screen.getByText('Octopus versus HEM')).toBeDefined();
    expect(screen.getByText('Supplier data completeness')).toBeDefined();
    expect(screen.getByText('Supplier costs')).toBeDefined();
    expect(screen.getByText('Export income')).toBeDefined();
    expect(screen.getByText('47 of 48 intervals · 1 missing')).toBeDefined();
    expect(screen.getByText('46 of 48 intervals · 2 missing')).toBeDefined();
    expect(screen.getAllByText('£1.17').length).toBeGreaterThan(0);
    const headings = screen.getAllByRole('heading').map((heading) => heading.textContent ?? '');
    expect(headings.indexOf('Supplier data completeness')).toBeLessThan(headings.indexOf('Supplier costs'));
    expect(headings.indexOf('Cumulative gas')).toBeLessThan(headings.indexOf('Billing summary tables'));
    expect(headings.indexOf('Billing summary tables')).toBeLessThan(headings.indexOf('Daily HEM comparison'));
  });

  it('downloads CSV and PDF summaries without opening a popup', async () => {
    render(<OctopusPage />);
    const csv = await screen.findByRole('button', { name: 'CSV' });
    fireEvent.click(csv);
    expect(createObjectUrlMock).toHaveBeenCalledOnce();
    expect(anchorClickMock).toHaveBeenCalledOnce();
    expect(revokeObjectUrlMock).toHaveBeenCalledWith('blob:octopus-summary');
    expect(screen.getByRole('status').textContent).toContain('CSV downloaded');

    fireEvent.click(screen.getByRole('button', { name: 'PDF report' }));
    await waitFor(() => expect(pdfDownloadMock).toHaveBeenCalledWith(
      expect.objectContaining({ rangeLabel: '30 days' }),
      expect.stringMatching(/givenergy_octopus_summary_30d_.*\.pdf/),
    ));
    expect(screen.getByRole('status').textContent).toContain('PDF downloaded');
  });

  it('shows a useful error when direct PDF generation fails', async () => {
    pdfDownloadMock.mockRejectedValueOnce(new Error('PDF generation failed'));
    render(<OctopusPage />);
    fireEvent.click(await screen.findByRole('button', { name: 'PDF report' }));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('PDF generation failed'));
  });

  it('starts a manual sync and reflects its in-progress state', async () => {
    render(<OctopusPage />);
    const button = await screen.findByRole('button', { name: 'Sync now' });
    fireEvent.click(button);
    await waitFor(() => expect(apiPost).toHaveBeenCalledWith('/api/octopus/sync'));
    expect(screen.getByRole('button', { name: 'Syncing…' }).hasAttribute('disabled')).toBe(true);
  });
});
