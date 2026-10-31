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

// Capture the props handed to chart components so we can assert on styling
// (Recharts SVG ticks/tooltips are not reachable from the DOM in these tests).
const { xAxisProps, yAxisProps, tooltipProps } = vi.hoisted(() => ({
  xAxisProps: [] as Array<Record<string, unknown>>,
  yAxisProps: [] as Array<Record<string, unknown>>,
  tooltipProps: [] as Array<Record<string, unknown>>,
}));

vi.mock('recharts', () => ({
  ResponsiveContainer: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  AreaChart: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  LineChart: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  Area: () => null,
  Line: () => null,
  CartesianGrid: () => null,
  Legend: () => null,
  Tooltip: (props: Record<string, unknown>) => { tooltipProps.push(props); return null; },
  XAxis: (props: Record<string, unknown>) => { xAxisProps.push(props); return null; },
  YAxis: (props: Record<string, unknown>) => { yAxisProps.push(props); return null; },
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
    xAxisProps.length = 0;
    yAxisProps.length = 0;
    tooltipProps.length = 0;
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
    expect(await screen.findByText('Electricity Consumption')).toBeDefined();
    expect(screen.getByText('Gas Consumption')).toBeDefined();
    expect(screen.getByText('Cumulative Electricity')).toBeDefined();
    expect(screen.getByText('Cumulative Gas')).toBeDefined();
    expect(screen.getAllByText('1.250 kWh').length).toBeGreaterThan(0);
    expect(screen.getAllByText('0.500 kWh').length).toBeGreaterThan(0);
    expect(screen.getAllByText('3.500').length).toBeGreaterThan(0);
    expect(screen.getByText('Gas values are supplier-reported units.')).toBeDefined();
    expect(screen.getByText('3 meter stream(s)')).toBeDefined();
    expect(screen.getByText('Older History Backfilling')).toBeDefined();
    expect(screen.getByText('Estimated Supplier Costs')).toBeDefined();
    expect(screen.getByText('Monthly Summary')).toBeDefined();
    expect(screen.getByText('Octopus Versus HEM')).toBeDefined();
    expect(screen.getByText('Supplier Data Completeness')).toBeDefined();
    expect(screen.getByText('Supplier Costs')).toBeDefined();
    expect(screen.getByText('Export Income')).toBeDefined();
    expect(screen.getByText('47 of 48 intervals · 1 missing')).toBeDefined();
    expect(screen.getByText('46 of 48 intervals · 2 missing')).toBeDefined();
    expect(screen.getAllByText('£1.17').length).toBeGreaterThan(0);
    const headings = screen.getAllByRole('heading').map((heading) => heading.textContent ?? '');
    expect(headings.indexOf('Supplier Data Completeness')).toBeLessThan(headings.indexOf('Supplier Costs'));
    expect(headings.indexOf('Cumulative Gas')).toBeLessThan(headings.indexOf('Billing Summary Tables'));
    expect(headings.indexOf('Billing Summary Tables')).toBeLessThan(headings.indexOf('Daily HEM Comparison'));
  });

  it('downloads CSV and PDF summaries without opening a popup', async () => {
    render(<OctopusPage />);
    const csv = await screen.findByRole('button', { name: 'CSV' });
    fireEvent.click(csv);
    expect(createObjectUrlMock).toHaveBeenCalledOnce();
    expect(anchorClickMock).toHaveBeenCalledOnce();
    expect(revokeObjectUrlMock).toHaveBeenCalledWith('blob:octopus-summary');
    expect(screen.getByRole('status').textContent).toContain('CSV downloaded');

    fireEvent.click(screen.getByRole('button', { name: 'PDF Report' }));
    await waitFor(() => expect(pdfDownloadMock).toHaveBeenCalledWith(
      expect.objectContaining({ rangeLabel: '30 Days' }),
      expect.stringMatching(/givenergy_octopus_summary_30d_.*\.pdf/),
    ));
    expect(screen.getByRole('status').textContent).toContain('PDF downloaded');
  });

  it('shows a useful error when direct PDF generation fails', async () => {
    pdfDownloadMock.mockRejectedValueOnce(new Error('PDF generation failed'));
    render(<OctopusPage />);
    fireEvent.click(await screen.findByRole('button', { name: 'PDF Report' }));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('PDF generation failed'));
  });

  it('starts a manual sync and reflects its in-progress state', async () => {
    render(<OctopusPage />);
    const button = await screen.findByRole('button', { name: 'Sync Now' });
    fireEvent.click(button);
    await waitFor(() => expect(apiPost).toHaveBeenCalledWith('/api/octopus/sync'));
    expect(screen.getByRole('button', { name: 'Syncing…' }).hasAttribute('disabled')).toBe(true);
  });

  describe('households without gas', () => {
    /** Serve the normal fixtures, but report whether the household has gas. */
    function serveWithGas(gasEnabled: boolean | undefined) {
      const normal = vi.mocked(apiGet).getMockImplementation()!;
      vi.mocked(apiGet).mockImplementation(async (path: string, ...rest: unknown[]) => {
        if (path === '/api/octopus/status') {
          const { gas_enabled: _ignored, ...rest2 } = { ...status, gas_enabled: undefined } as Record<string, unknown>;
          void _ignored;
          return gasEnabled === undefined ? rest2 : { ...status, gas_enabled: gasEnabled };
        }
        return (normal as (p: string, ...r: unknown[]) => Promise<unknown>)(path, ...rest);
      });
    }

    it('shows gas everywhere when gas is on', async () => {
      serveWithGas(true);
      render(<OctopusPage />);
      expect(await screen.findByText('Gas Consumption')).toBeDefined();
      expect(screen.getByText('Cumulative Gas')).toBeDefined();
      expect(screen.getByText('Gas Cost')).toBeDefined();
      expect(screen.getByText('Gas Missing')).toBeDefined();
      expect(screen.getByRole('region', { name: 'Selected Period Totals' }).children).toHaveLength(3);
    });

    it('still shows gas when an older backend does not say', async () => {
      serveWithGas(undefined);
      render(<OctopusPage />);
      expect(await screen.findByText('Gas Consumption')).toBeDefined();
    });

    it('hides every trace of gas when the household has none', async () => {
      serveWithGas(false);
      const { container } = render(<OctopusPage />);
      await screen.findByText('Electricity Consumption');
      expect(container.textContent).not.toMatch(/gas/i);
      // Two totals tiles, not a gap where the third was.
      expect(screen.getByRole('region', { name: 'Selected Period Totals' }).children).toHaveLength(2);
      // The net cost is the electricity figure, labelled as such.
      expect(screen.getByText('Net Electricity Cost')).toBeDefined();
    });

    it('keeps gas out of the CSV and PDF exports too', async () => {
      serveWithGas(false);
      render(<OctopusPage />);
      fireEvent.click(await screen.findByRole('button', { name: 'CSV' }));
      const blob = createObjectUrlMock.mock.calls[0][0] as unknown as Blob;
      expect(await blob.text()).not.toMatch(/gas/i);

      fireEvent.click(screen.getByRole('button', { name: 'PDF Report' }));
      await waitFor(() => expect(pdfDownloadMock).toHaveBeenCalledWith(
        expect.objectContaining({ includeGas: false }),
        expect.any(String),
      ));
    });

    it('passes includeGas through as true for a household with gas', async () => {
      serveWithGas(true);
      render(<OctopusPage />);
      fireEvent.click(await screen.findByRole('button', { name: 'PDF Report' }));
      await waitFor(() => expect(pdfDownloadMock).toHaveBeenCalledWith(
        expect.objectContaining({ includeGas: true }),
        expect.any(String),
      ));
    });
  });

  it('renders chart axes with a readable, bold tick style (issue #232)', async () => {
    render(<OctopusPage />);
    await screen.findByText('Electricity Consumption');
    // Recharts otherwise derives tick fill from its muted default axis
    // stroke. Use the primary theme colour explicitly so ticks are near-white
    // in dark mode and dark in light mode, while retaining a bold weight.
    expect(xAxisProps.length).toBeGreaterThan(0);
    expect(yAxisProps.length).toBeGreaterThan(0);
    for (const props of [...xAxisProps, ...yAxisProps]) {
      const tick = props.tick as {
        fill?: string;
        style?: { fontWeight?: number };
      } | undefined;
      expect(tick?.fill).toBe('var(--app-text-primary)');
      expect(tick?.style?.fontWeight).toBe(700);
    }

    const labels = yAxisProps
      .map((props) => props.label as {
        fill?: string;
        style?: { fontWeight?: number };
      } | undefined)
      .filter((label) => label != null);
    expect(labels.length).toBeGreaterThan(0);
    for (const label of labels) {
      expect(label.fill).toBe('var(--app-text-primary)');
      expect(label.style?.fontWeight).toBe(700);
    }
  });

  it('renders cost chart tooltip labels with readable themed text', async () => {
    render(<OctopusPage />);
    await screen.findByText('Supplier Costs');

    const styledCostTooltips = tooltipProps.filter((props) => 'labelStyle' in props);
    expect(styledCostTooltips.length).toBeGreaterThanOrEqual(2);
    for (const props of styledCostTooltips) {
      expect(props.labelStyle).toMatchObject({
        color: 'var(--app-text-primary)',
        fontWeight: 700,
      });
      expect(props.contentStyle).toMatchObject({
        backgroundColor: 'var(--app-bg-elevated)',
      });
    }
  });
});
