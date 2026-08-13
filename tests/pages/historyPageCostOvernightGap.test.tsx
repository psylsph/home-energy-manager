import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';

// Issue #269: The period-totals summary on the Cost tab must reflect the
// server's corrected cost values — which now include energy accumulated
// during short overnight gaps between the last pre-midnight reading and the
// first post-midnight reading. This test verifies the frontend wiring:
// given a summary with gap-inclusive cost values, the Cost tab renders them
// correctly. The underlying arithmetic is covered by Rust unit tests in
// history/mod.rs (cost_series_overnight_gap_credits_short_gap_energy etc.).

const summaryWithGapEnergy = {
  solar_generated_kwh: 35.0,
  battery_charged_kwh: 5.0,
  battery_discharged_kwh: 8.0,
  grid_imported_kwh: 32.4, // 3 × (10 + 0.8) kWh (daily + gap energy)
  grid_exported_kwh: 2.0,
  home_consumed_kwh: 28.0,
  // 3 days × (10 + 0.8) kWh × £0.25 = £8.10 (previously £7.50 without gap energy)
  import_cost_gbp: 8.10,
  export_income_gbp: 0.50,
  net_cost_gbp: 7.60,
};

const fetchHistorySummaryMock = vi.fn(async () => summaryWithGapEnergy);
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

describe('<HistoryPage/> — Cost tab with overnight gap energy (#269)', () => {
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

  it('renders the gap-inclusive import cost on the Cost tab', async () => {
    render(<HistoryPage />);
    fireEvent.click(await screen.findByRole('button', { name: 'Cost', exact: true }));

    // The summary box must show the gap-inclusive cost: £8.10, not the
    // old £7.50 that dropped overnight gap energy.
    expect(await screen.findByText('£8.10')).toBeDefined();
    expect(screen.getByText('£0.50')).toBeDefined();
    expect(screen.getByText('£7.60')).toBeDefined();
  });

  it('renders the gap-inclusive imported energy on the Grid tab', async () => {
    render(<HistoryPage />);
    fireEvent.click(await screen.findByRole('button', { name: 'Grid', exact: true }));

    // 32.4 kWh includes the 3 × 0.8 kWh gap energy previously dropped.
    expect(await screen.findByText('32.4kWh')).toBeDefined();
  });
});
