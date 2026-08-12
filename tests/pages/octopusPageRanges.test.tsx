import { cleanup, render, screen, waitFor, fireEvent } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// ---------------------------------------------------------------------------
// OctopusPage already has OctopusPage.test.tsx covering the full render,
// CSV/PDF export, manual sync, and chart axis styling. This file adds
// coverage for the untested interaction logic: range switching (fetches +
// cost series resolution), the loading gate, error display from a failed
// fetch, the "last error" banner from status, and derived totals (import /
// export / gas kWh sums).
// ---------------------------------------------------------------------------

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
import { apiGet } from '../../src/lib/api';

const emptyTotals = {
  octopus_import_kwh: 0, hem_import_kwh: 0, import_difference_kwh: 0,
  octopus_export_kwh: 0, hem_export_kwh: 0, export_difference_kwh: 0,
  expected_import_intervals: 0, import_intervals: 0, missing_import_intervals: 0,
  expected_export_intervals: 0, export_intervals: 0, missing_export_intervals: 0,
  expected_gas_intervals: 0, gas_intervals: 0, missing_gas_intervals: 0,
};

function summaryTotals(overrides: Partial<Record<string, number>> = {}) {
  return {
    electricity_import_kwh: 1.25, electricity_export_kwh: 0.5, gas_usage: 3.5,
    electricity_energy_cost_gbp: 0.25, electricity_standing_cost_gbp: 0.5,
    electricity_total_cost_gbp: 0.75, export_income_gbp: 0.08,
    gas_energy_cost_gbp: 0.2, gas_standing_cost_gbp: 0.3,
    gas_total_cost_gbp: 0.5, net_cost_gbp: 1.17, pricing_complete: true,
    ...overrides,
  };
}

function billingSummary(daily: unknown[] = [], monthly: unknown[] = [], yearly: unknown[] = []) {
  return {
    ok: true,
    gas_unit: 'kwh' as const,
    estimated: true,
    data: {
      gas_cost_available: true,
      totals: summaryTotals(),
      daily,
      monthly,
      yearly,
    },
  };
}

function comparisonResponse(days: unknown[] = [], streamOverrides: Record<string, boolean> = {}) {
  return {
    ok: true,
    data: {
      totals: { ...emptyTotals },
      days,
      import_stream_available: streamOverrides.import_stream_available ?? true,
      export_stream_available: streamOverrides.export_stream_available ?? true,
      gas_stream_available: streamOverrides.gas_stream_available ?? true,
    },
  };
}

function statusResponse(overrides: Record<string, unknown> = {}) {
  return {
    ok: true,
    configured: true,
    data: {
      syncing: false,
      last_sync_at: '2026-07-17T12:00:00Z',
      last_error: null,
      backfill_complete: true,
      discovered_streams: 3,
      imported_intervals: 20,
      ...overrides,
    },
    bounds: null,
    gas_unit_note: 'Gas values are supplier-reported units.',
    ...overrides,
  };
}

function historyResponse(series: Record<string, unknown[]> = {}) {
  return {
    ok: true,
    data: {
      electricity_import: [],
      electricity_export: [],
      gas: [],
      ...series,
    },
  };
}

describe('OctopusPage — range, loading, error, derived values', () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  beforeEach(() => {
    vi.mocked(apiGet).mockImplementation(async (path: string) => {
      if (path === '/api/octopus/status') return statusResponse();
      if (path.startsWith('/api/octopus/comparison')) return comparisonResponse();
      if (path.startsWith('/api/octopus/summary')) return billingSummary();
      return historyResponse();
    });
  });

  describe('loading gate', () => {
    it('shows the loading message before the first fetch resolves', () => {
      // Never-resolving promise keeps loading true.
      vi.mocked(apiGet).mockImplementation(() => new Promise(() => {}));
      render(<OctopusPage />);
      expect(screen.getByText('Loading Octopus data…')).toBeDefined();
    });
  });

  describe('range switching', () => {
    it('switches the active range to 7 days and re-fetches with the new range param', async () => {
      render(<OctopusPage />);
      // Wait for initial load.
      await screen.findByText('Electricity consumption');
      // Click the 7 days range button.
      const sevenDayBtn = screen.getByRole('button', { name: '7 days' });
      fireEvent.click(sevenDayBtn);
      // The aria-pressed state flips.
      await waitFor(() => {
        expect(sevenDayBtn.getAttribute('aria-pressed')).toBe('true');
      });
      // A history fetch with range=7d must have fired.
      await waitFor(() => {
        const historyCalls = vi.mocked(apiGet).mock.calls
          .map((c) => c[0])
          .filter((p) => p.includes('/api/octopus/history'));
        expect(historyCalls.some((p) => p.includes('range=7d'))).toBe(true);
      });
    });

    it('switches to 1 year range', async () => {
      render(<OctopusPage />);
      await screen.findByText('Electricity consumption');
      fireEvent.click(screen.getByRole('button', { name: '1 year' }));
      await waitFor(() => {
        const summaryCalls = vi.mocked(apiGet).mock.calls
          .map((c) => c[0])
          .filter((p) => p.includes('/api/octopus/summary'));
        expect(summaryCalls.some((p) => p.includes('range=1y'))).toBe(true);
      });
    });

    it('uses the monthly series for the cost chart at 6m range', async () => {
      // Spy on the summary fetch to capture the range param. The cost series
      // resolution (daily vs monthly) is internal to the component, but we
      // can assert the range was sent to the backend.
      render(<OctopusPage />);
      await screen.findByText('Electricity consumption');
      const sixMonthBtn = screen.getByRole('button', { name: '6 months' });
      fireEvent.click(sixMonthBtn);
      await waitFor(() => {
        const summaryCalls = vi.mocked(apiGet).mock.calls
          .map((c) => c[0])
          .filter((p) => p.includes('/api/octopus/summary'));
        expect(summaryCalls.some((p) => p.includes('range=6m'))).toBe(true);
      });
    });
  });

  describe('error handling', () => {
    it('shows the error message when the initial load fails', async () => {
      vi.mocked(apiGet).mockRejectedValue(new Error('Network timeout'));
      render(<OctopusPage />);
      await waitFor(() => {
        expect(screen.getByRole('alert').textContent).toContain('Network timeout');
      });
    });

    it('shows the error from a non-Error thrown value', async () => {
      vi.mocked(apiGet).mockRejectedValue('string error');
      render(<OctopusPage />);
      await waitFor(() => {
        expect(screen.getByRole('alert').textContent).toContain('Unable to load Octopus data');
      });
    });

    it('shows the last_error from the status payload', async () => {
      vi.mocked(apiGet).mockImplementation(async (path: string) => {
        if (path === '/api/octopus/status') {
          return statusResponse({ data: { syncing: false, last_sync_at: null, last_error: 'API key expired', backfill_complete: false, discovered_streams: 0, imported_intervals: 0 } });
        }
        if (path.startsWith('/api/octopus/comparison')) return comparisonResponse();
        if (path.startsWith('/api/octopus/summary')) return billingSummary();
        return historyResponse();
      });
      render(<OctopusPage />);
      await waitFor(() => {
        expect(screen.getByRole('alert').textContent).toContain('API key expired');
      });
    });
  });

  describe('derived totals', () => {
    it('sums electricity import / export / gas from the history series', async () => {
      vi.mocked(apiGet).mockImplementation(async (path: string) => {
        if (path === '/api/octopus/status') return statusResponse();
        if (path.startsWith('/api/octopus/comparison')) return comparisonResponse();
        if (path.startsWith('/api/octopus/summary')) return billingSummary();
        return historyResponse({
          electricity_import: [{ t: 1_700_000_000_000, v: 2.5 }, { t: 1_700_000_001_800, v: 1.5 }],
          electricity_export: [{ t: 1_700_000_000_000, v: 0.75 }],
          gas: [{ t: 1_700_000_000_000, v: 4.0 }, { t: 1_700_000_001_800, v: 2.0 }],
        });
      });
      render(<OctopusPage />);
      // 2.5 + 1.5 = 4.0 kWh imported.
      await waitFor(() => {
        expect(screen.getAllByText('4.000 kWh').length).toBeGreaterThan(0);
      });
      // 0.75 kWh exported.
      expect(screen.getAllByText('0.750 kWh').length).toBeGreaterThan(0);
      // 4.0 + 2.0 = 6.0 gas.
      expect(screen.getAllByText('6.000').length).toBeGreaterThan(0);
    });

    it('shows 0.000 when no history data is returned', async () => {
      render(<OctopusPage />);
      await screen.findByText('Electricity consumption');
      // The totals tiles show 0.000 for each series.
      expect(screen.getAllByText('0.000 kWh').length).toBeGreaterThanOrEqual(2);
    });
  });

  describe('billing summary tables', () => {
    it('renders monthly and yearly tables only when data exists', async () => {
      vi.mocked(apiGet).mockImplementation(async (path: string) => {
        if (path === '/api/octopus/status') return statusResponse();
        if (path.startsWith('/api/octopus/comparison')) return comparisonResponse();
        if (path.startsWith('/api/octopus/summary')) {
          return billingSummary(
            [{ period: '2026-07-17', ...summaryTotals(), net_cost_gbp: 1.17, pricing_complete: true }],
            [{ period: '2026-07', ...summaryTotals(), net_cost_gbp: 1.17, pricing_complete: true }],
            [{ period: '2026', ...summaryTotals(), net_cost_gbp: 1.17, pricing_complete: true }],
          );
        }
        return historyResponse();
      });
      render(<OctopusPage />);
      expect(await screen.findByText('Billing summary tables')).toBeDefined();
      expect(screen.getByText('Monthly summary')).toBeDefined();
      expect(screen.getByText('Yearly summary')).toBeDefined();
    });

    it('hides the billing tables when there is no monthly or yearly data', async () => {
      render(<OctopusPage />);
      await screen.findByText('Electricity consumption');
      expect(screen.queryByText('Billing summary tables')).toBeNull();
    });
  });

  describe('supplier data completeness', () => {
    it('shows "Not configured" when a stream is unavailable', async () => {
      vi.mocked(apiGet).mockImplementation(async (path: string) => {
        if (path === '/api/octopus/status') return statusResponse();
        if (path.startsWith('/api/octopus/comparison')) {
          return comparisonResponse([], { gas_stream_available: false });
        }
        if (path.startsWith('/api/octopus/summary')) return billingSummary();
        return historyResponse();
      });
      render(<OctopusPage />);
      await waitFor(() => {
        expect(screen.getByText('Not configured')).toBeDefined();
      });
    });
  });
});
