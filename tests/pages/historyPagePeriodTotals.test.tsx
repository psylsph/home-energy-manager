import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// HistoryPage already has HistoryPage.test.tsx covering tabs, ranges,
// navigation, empty state, and combined export. This file adds coverage
// for the untested derived-value logic: the PeriodTotals tiles (different
// additive quantities per tab), the Cost tab's standing-charge breakdown
// wiring (hasStandingCharge controls extra chart fields), and the window
// label display for paged offsets.
// ---------------------------------------------------------------------------

type FetchHistoryCall = { range: string; fields: string[]; offset: number; rolling: boolean };

const fetchHistoryCalls: FetchHistoryCall[] = [];
const fetchHistoryMock = vi.fn(async (...args: unknown[]) => {
  const [range, fields, offset, rolling] = args as [string, string[], number, boolean];
  fetchHistoryCalls.push({ range, fields: [...fields], offset, rolling });
  // Return minimal data so hasData is true.
  const result: Record<string, { t: number; v: number }[]> = {};
  for (const f of fields) result[f] = [{ t: 1_700_000_000_000, v: 1 }];
  return result;
});

const apiGetMock = vi.fn(async (path: string) => {
  if (path === '/api/settings') {
    return { ok: true, data: { import_standing_charge_p_per_day: 0 } };
  }
  return { ok: true, data: {} };
});

vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGetMock(...(args as [string])),
  fetchHistory: (...args: unknown[]) => fetchHistoryMock(...args),
  fetchHistorySummary: async () => ({
    solar_generated_kwh: 12.5,
    battery_charged_kwh: 5.0,
    battery_discharged_kwh: 3.2,
    grid_imported_kwh: 2.1,
    grid_exported_kwh: 4.8,
    home_consumed_kwh: 8.3,
    import_cost_gbp: 0.63,
    export_income_gbp: 0.72,
    net_cost_gbp: -0.09,
  }),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  isTauri: false,
}));

vi.mock('recharts', () => ({
  ResponsiveContainer: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  AreaChart: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  Area: () => null,
  CartesianGrid: () => null,
  Tooltip: () => null,
  XAxis: () => null,
  YAxis: () => null,
}));

globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

import HistoryPage from '../../src/pages/HistoryPage';
import { useInverterStore } from '../../src/store/useInverterStore';

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

async function clickTab(label: string) {
  const btn = await screen.findByRole('button', { name: label });
  fireEvent.click(btn);
}

describe('<HistoryPage/> — period totals, cost breakdown, window labels', () => {
  beforeEach(() => {
    silenceConsoleError();
    fetchHistoryCalls.length = 0;
    fetchHistoryMock.mockClear();
    apiGetMock.mockClear();
    // Reset settings mock to no standing charge by default.
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/settings') {
        return { ok: true, data: { import_standing_charge_p_per_day: 0 } };
      }
      return { ok: true, data: {} };
    });
    useInverterStore.setState({
      snapshot: null,
      chartRange: '24h',
      gridLineWeight: 'standard',
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  describe('period totals — per-tab additive values', () => {
    it('shows Charged / Discharged totals on the Battery tab', async () => {
      render(<HistoryPage />);
      // Wait for data + summary to settle, then check the Period Totals section.
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      expect(await screen.findByText('Charged')).toBeDefined();
      expect(screen.getByText('Discharged')).toBeDefined();
      // Values come from fetchHistorySummary mock: 5.0 / 3.2 kWh.
      expect(screen.getByText('5.0kWh')).toBeDefined();
      expect(screen.getByText('3.2kWh')).toBeDefined();
    });

    it('shows Generated total on the Solar tab', async () => {
      render(<HistoryPage />);
      await clickTab('Solar');
      await waitFor(() => {
        expect(screen.getByText('Generated')).toBeDefined();
      });
      expect(screen.getByText('12.5kWh')).toBeDefined();
    });

    it('shows Imported / Exported totals on the Grid tab', async () => {
      render(<HistoryPage />);
      await clickTab('Grid');
      await waitFor(() => {
        expect(screen.getByText('Imported')).toBeDefined();
      });
      expect(screen.getByText('Exported')).toBeDefined();
      expect(screen.getByText('2.1kWh')).toBeDefined();
      expect(screen.getByText('4.8kWh')).toBeDefined();
    });

    it('shows Consumed total on the Home tab', async () => {
      render(<HistoryPage />);
      await clickTab('Home');
      await waitFor(() => {
        expect(screen.getByText('Consumed')).toBeDefined();
      });
      expect(screen.getByText('8.3kWh')).toBeDefined();
    });

    it('shows Import cost / Export income / Net cost on the Cost tab', async () => {
      render(<HistoryPage />);
      await clickTab('Cost');
      await waitFor(() => {
        expect(screen.getByText('Import cost')).toBeDefined();
      });
      expect(screen.getByText('Export income')).toBeDefined();
      expect(screen.getByText('Net cost')).toBeDefined();
      // Net cost is negative (-0.09) → formatted as GBP with minus.
      expect(screen.getByText(/-£0.09|-£0\.09/)).toBeDefined();
    });

    it('hides the period totals section entirely on the Temperature tab', async () => {
      render(<HistoryPage />);
      await clickTab('Temperature');
      await waitFor(() => {
        // Temperature tab returns [] from getPeriodTotalItems, so the
        // PeriodTotals component renders null.
        expect(screen.queryByText('Period totals')).toBeNull();
      });
    });
  });

  describe('cost tab — standing charge breakdown wiring', () => {
    it('does not request the standing-charge breakdown fields when no standing charge is configured', async () => {
      render(<HistoryPage />);
      await clickTab('Cost');
      await waitFor(() => {
        expect(fetchHistoryCalls.some((c) => c.fields.includes('_import_cost'))).toBe(true);
      });
      const costCall = fetchHistoryCalls.at(-1)!;
      // Without a standing charge, only _import_cost + _export_income are
      // requested — no _import_energy_cost or _import_standing_charge.
      expect(costCall.fields).not.toContain('_import_energy_cost');
      expect(costCall.fields).not.toContain('_import_standing_charge');
    });

    it('requests the standing-charge breakdown fields when a standing charge is configured', async () => {
      // Simulate /api/settings returning a positive standing charge.
      apiGetMock.mockImplementation(async (path: string) => {
        if (path === '/api/settings') {
          return { ok: true, data: { import_standing_charge_p_per_day: 54.86 } };
        }
        return { ok: true, data: {} };
      });
      render(<HistoryPage />);
      await clickTab('Cost');
      await waitFor(() => {
        const costCall = fetchHistoryCalls.at(-1)!;
        expect(costCall.fields).toContain('_import_cost');
        expect(costCall.fields).toContain('_import_energy_cost');
        expect(costCall.fields).toContain('_import_standing_charge');
      });
    });
  });

  describe('window label — paged offset display', () => {
    it('shows "Now" at offset 0 for a rolling range', async () => {
      render(<HistoryPage />);
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      // The default 24h range at offset 0 shows "Now" in the nav label.
      // The text may appear in multiple places (e.g. the nav span), so use
      // getAllByText to check it's present.
      expect(screen.getAllByText('Now').length).toBeGreaterThan(0);
    });

    it('shows a date range label after paging back', async () => {
      render(<HistoryPage />);
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      fireEvent.click(screen.getByRole('button', { name: /Older/i }));
      // After paging back, the label is no longer "Now" — it's a date range.
      await waitFor(() => {
        expect(screen.queryByText('Now')).toBeNull();
      });
    });
  });
});
