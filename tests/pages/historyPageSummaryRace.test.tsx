import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

type Summary = {
  solar_generated_kwh: number;
  battery_charged_kwh: number;
  battery_discharged_kwh: number;
  grid_imported_kwh: number;
  grid_exported_kwh: number;
  home_consumed_kwh: number;
  import_cost_gbp: number;
  export_income_gbp: number;
  net_cost_gbp: number;
};

type PendingSummary = {
  offset: number;
  resolve: (summary: Summary) => void;
};

const pendingSummaries: PendingSummary[] = [];
const fetchHistorySummaryMock = vi.fn(
  async (...args: [string, number, boolean]) => {
    const offset = args[1];
    return new Promise<Summary>((resolve) => {
      pendingSummaries.push({ offset, resolve });
    });
  },
);

vi.mock('../../src/lib/api', () => ({
  apiGet: async () => ({ ok: true, data: { import_standing_charge_p_per_day: 0 } }),
  fetchHistory: async () => ({
    soc: [{ t: 1_700_000_000_000, v: 50 }],
  }),
  fetchHistorySummary: (...args: [string, number, boolean]) => fetchHistorySummaryMock(...args),
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

import HistoryPage from '../../src/pages/HistoryPage';

const summary = (batteryCharged: number): Summary => ({
  solar_generated_kwh: 0,
  battery_charged_kwh: batteryCharged,
  battery_discharged_kwh: 0,
  grid_imported_kwh: 0,
  grid_exported_kwh: 0,
  home_consumed_kwh: 0,
  import_cost_gbp: 0,
  export_income_gbp: 0,
  net_cost_gbp: 0,
});

describe('<HistoryPage/> — summary request ordering', () => {
  beforeEach(() => {
    pendingSummaries.length = 0;
    fetchHistorySummaryMock.mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it('keeps the newest historical offset summary when responses resolve in reverse', async () => {
    render(<HistoryPage />);
    await waitFor(() => expect(pendingSummaries).toHaveLength(1));

    fireEvent.click(await screen.findByRole('button', { name: /Older/i }));
    await waitFor(() => expect(pendingSummaries).toHaveLength(2));

    const olderRequest = pendingSummaries.find((request) => request.offset === 1);
    const initialRequest = pendingSummaries.find((request) => request.offset === 0);
    expect(olderRequest).toBeDefined();
    expect(initialRequest).toBeDefined();

    olderRequest!.resolve(summary(22));
    expect(await screen.findByText('22.0kWh')).toBeDefined();

    // The response from the superseded offset arrives late and must not
    // replace the summary now displayed for offset 1.
    initialRequest!.resolve(summary(11));
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(screen.getByText('22.0kWh')).toBeDefined();
    expect(screen.queryByText('11.0kWh')).toBeNull();
  });
});
