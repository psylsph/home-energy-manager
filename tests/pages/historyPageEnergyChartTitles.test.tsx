import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Issue #282: the Solar / Grid / Home energy charts had hard-coded
// "…Energy Today (kWh)" titles even when the selected window was a week,
// month, or year. The titles are now neutral ("PV Energy (kWh)") — the
// range bar and the date picker already communicate the window.
// ---------------------------------------------------------------------------

const fetchHistoryMock = vi.fn(async (...args: unknown[]) => {
  const fields = args[1] as string[];
  // Return minimal data per requested field so hasData is true and the
  // chart headings actually render.
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
  const btn = await screen.findByRole('button', { name: label, exact: true });
  fireEvent.click(btn);
}

/** Every chart heading currently on screen (h3 chart titles + h2 sections). */
function chartHeadingTexts(): string[] {
  return screen
    .getAllByRole('heading')
    .map((el) => el.textContent ?? '');
}

describe('<HistoryPage/> — energy chart titles are range-neutral (issue #282)', () => {
  beforeEach(() => {
    silenceConsoleError();
    fetchHistoryMock.mockClear();
    apiGetMock.mockClear();
    useInverterStore.setState({
      snapshot: null,
      chartRange: '7d',
      gridLineWeight: 'standard',
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it('titles the Solar energy chart "PV Energy (kWh)" with no "Today"', async () => {
    render(<HistoryPage />);
    await clickTab('Solar');
    expect(await screen.findByText('PV Energy (kWh)')).toBeDefined();
    expect(chartHeadingTexts().some((t) => t.includes('Today'))).toBe(false);
  });

  it('titles the Grid energy chart "Grid Energy (kWh)" with no "Today"', async () => {
    render(<HistoryPage />);
    await clickTab('Grid');
    expect(await screen.findByText('Grid Energy (kWh)')).toBeDefined();
    expect(chartHeadingTexts().some((t) => t.includes('Today'))).toBe(false);
  });

  it('titles the Home energy chart "Load Energy (kWh)" with no "Today"', async () => {
    render(<HistoryPage />);
    await clickTab('Home');
    expect(await screen.findByText('Load Energy (kWh)')).toBeDefined();
    expect(chartHeadingTexts().some((t) => t.includes('Today'))).toBe(false);
  });

  // The titles stay neutral even on the literal Today range — the window
  // label next to the picker already says which day is shown.
  it('keeps titles neutral on the "today" range too', async () => {
    useInverterStore.setState({ chartRange: 'today' });
    render(<HistoryPage />);
    await clickTab('Solar');
    expect(await screen.findByText('PV Energy (kWh)')).toBeDefined();
    expect(chartHeadingTexts().some((t) => t.includes('Today'))).toBe(false);
  });
});
