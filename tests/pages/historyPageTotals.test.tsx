import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';

const summary = {
  solar_generated_kwh: 12.3,
  battery_charged_kwh: 1.2,
  battery_discharged_kwh: 2.4,
  grid_imported_kwh: 3.5,
  grid_exported_kwh: 4.6,
  home_consumed_kwh: 9.8,
  import_cost_gbp: 4.5,
  export_income_gbp: 1.25,
  net_cost_gbp: 3.25,
};

const fetchHistorySummaryMock = vi.fn(async () => summary);
const fetchHistoryMock = vi.fn(async (_range: string, fields: string[]) => {
  const result: Record<string, Array<{ t: number; v: number }>> = {};
  for (const field of fields) result[field] = [{ t: 1_720_000_000_000, v: 1 }];
  return result;
});
const apiGetMock = vi.fn(async () => ({ ok: true, data: {} }));

vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGetMock(...(args as [])),
  fetchHistory: (...args: unknown[]) => fetchHistoryMock(...(args as [string, string[]])),
  fetchHistorySummary: (...args: unknown[]) =>
    fetchHistorySummaryMock(...(args as [string, number, boolean])),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  isTauri: false,
}));

globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

import HistoryPage from '../../src/pages/HistoryPage';
import { useInverterStore } from '../../src/store/useInverterStore';

describe('<HistoryPage/> — period totals (issue #237)', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    fetchHistoryMock.mockClear();
    fetchHistorySummaryMock.mockClear();
    apiGetMock.mockClear();
    useInverterStore.setState({ chartRange: '24h', gridLineWeight: 'standard' });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it('shows the additive totals belonging to each energy tab', async () => {
    render(<HistoryPage />);
    expect(await screen.findByRole('region', { name: 'Period totals' })).toBeDefined();
    expect(screen.getByText('1.2kWh')).toBeDefined();
    expect(screen.getByText('2.4kWh')).toBeDefined();

    fireEvent.click(screen.getByRole('button', { name: 'Solar', exact: true }));
    expect(await screen.findByText('12.3kWh')).toBeDefined();

    fireEvent.click(screen.getByRole('button', { name: 'Grid', exact: true }));
    expect(await screen.findByText('3.5kWh')).toBeDefined();
    expect(screen.getByText('4.6kWh')).toBeDefined();

    fireEvent.click(screen.getByRole('button', { name: 'Home', exact: true }));
    expect(await screen.findByText('9.8kWh')).toBeDefined();
  });

  it('shows import, export and net money on the Cost tab', async () => {
    render(<HistoryPage />);
    fireEvent.click(await screen.findByRole('button', { name: 'Cost', exact: true }));

    expect(await screen.findByText('£4.50')).toBeDefined();
    expect(screen.getByText('£1.25')).toBeDefined();
    expect(screen.getByText('£3.25')).toBeDefined();
  });

  it('does not mislabel temperature readings as an additive total', async () => {
    render(<HistoryPage />);
    await screen.findByRole('region', { name: 'Period totals' });
    fireEvent.click(screen.getByRole('button', { name: 'Temperature', exact: true }));

    await waitFor(() => {
      expect(screen.queryByRole('region', { name: 'Period totals' })).toBeNull();
    });
  });

  it('refetches totals for range and offset changes', async () => {
    render(<HistoryPage />);
    await waitFor(() => expect(fetchHistorySummaryMock).toHaveBeenCalledWith('24h', 0, true));

    fireEvent.click(screen.getByRole('button', { name: '7d', exact: true }));
    await waitFor(() => expect(fetchHistorySummaryMock).toHaveBeenCalledWith('7d', 0, true));

    fireEvent.click(screen.getByRole('button', { name: /Older/i }));
    await waitFor(() => expect(fetchHistorySummaryMock).toHaveBeenCalledWith('7d', 1, true));
  });
});
