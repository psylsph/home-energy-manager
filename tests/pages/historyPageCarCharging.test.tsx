import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// Issue #454: with a car-charging threshold set, the Home tab splits the car
// out of the home load; with none set it is exactly the page it always was.

type FetchHistoryCall = { range: string; fields: string[] };
const fetchHistoryCalls: FetchHistoryCall[] = [];
const fetchHistoryMock = vi.fn(async (...args: unknown[]) => {
  const [range, fields] = args as [string, string[]];
  fetchHistoryCalls.push({ range, fields: [...fields] });
  const result: Record<string, { t: number; v: number }[]> = {};
  for (const f of fields) result[f] = [{ t: 1_700_000_000_000, v: 1 }, { t: 1_700_000_300_000, v: 2 }];
  return result;
});

let settingsData: Record<string, unknown> = {};
const apiGetMock = vi.fn(async (path: string) => {
  if (path === '/api/settings') return { ok: true, data: settingsData };
  return { ok: true, data: {} };
});

let summaryCarKwh: number | null = null;
let summaryCarCost: number | null = null;
vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGetMock(...(args as [string])),
  fetchHistory: (...args: unknown[]) => fetchHistoryMock(...args),
  fetchHistorySummary: async () => ({
    solar_generated_kwh: 12.5,
    battery_charged_kwh: 5.0,
    battery_discharged_kwh: 3.2,
    grid_imported_kwh: 2.1,
    grid_exported_kwh: 4.8,
    home_consumed_kwh: 18.3,
    car_kwh: summaryCarKwh,
    car_cost_gbp: summaryCarCost,
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

async function openHomeTab() {
  fireEvent.click(await screen.findByRole('button', { name: 'Home' }));
}

const fieldsUsed = () => new Set(fetchHistoryCalls.flatMap((c) => c.fields));

describe('<HistoryPage/> — car charging split (issue #454)', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    fetchHistoryCalls.length = 0;
    fetchHistoryMock.mockClear();
    settingsData = {};
    summaryCarKwh = null;
    summaryCarCost = null;
    useInverterStore.setState({ snapshot: null, chartRange: '24h', gridLineWeight: 'standard' });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it('is unchanged when no threshold is set', async () => {
    render(<HistoryPage />);
    await openHomeTab();
    await waitFor(() => expect(fieldsUsed().has('home_power')).toBe(true));
    expect(fieldsUsed().has('home_energy_today_kwh')).toBe(true);
    for (const f of fieldsUsed()) expect(f).not.toMatch(/car/);
    expect(screen.queryByText('Car Charging')).toBeNull();
  });

  it.each([0.5, 0, 26, 40])(
    'is unchanged for a hand-edited threshold of %s kW, which the server ignores',
    async (kw) => {
      settingsData = { forecast_exclude_load_above_kw: kw };
      render(<HistoryPage />);
      await openHomeTab();
      await waitFor(() => expect(fieldsUsed().has('home_power')).toBe(true));
      for (const f of fieldsUsed()) expect(f).not.toMatch(/car/);
    },
  );

  it('splits the car out of both Home charts once a threshold is set', async () => {
    settingsData = { forecast_exclude_load_above_kw: 3 };
    summaryCarKwh = 7.4;
    render(<HistoryPage />);
    await openHomeTab();
    await waitFor(() => expect(fieldsUsed().has('_car_power')).toBe(true));
    const used = fieldsUsed();
    for (const f of [
      '_home_excl_car_power',
      '_car_power',
      '_home_excl_car_energy_today',
      '_car_energy_today',
    ]) {
      expect(used.has(f), f).toBe(true);
    }
    expect(used.has('home_power')).toBe(false);
    expect(used.has('home_energy_today_kwh')).toBe(false);
    expect((await screen.findAllByText('Car Charging')).length).toBeGreaterThan(0);
    expect(screen.getAllByText('Home (Excl. Car)').length).toBeGreaterThan(0);
  });

  it('shows the car energy beside Consumed in the period totals', async () => {
    settingsData = { forecast_exclude_load_above_kw: 3 };
    summaryCarKwh = 7.4;
    render(<HistoryPage />);
    await openHomeTab();
    expect(await screen.findByText('Consumed')).toBeDefined();
    expect(screen.getByText('18.3kWh')).toBeDefined();
    expect(await screen.findByText('7.4kWh')).toBeDefined();
  });

  it('shows no car total when the server reports none', async () => {
    settingsData = { forecast_exclude_load_above_kw: 3 };
    summaryCarKwh = null;
    render(<HistoryPage />);
    await openHomeTab();
    expect(await screen.findByText('Consumed')).toBeDefined();
    expect(screen.queryByText('7.4kWh')).toBeNull();
  });

  describe('Cost tab', () => {
    const openCostTab = async () => fireEvent.click(await screen.findByRole('button', { name: 'Cost' }));

    it('is unchanged when no threshold is set', async () => {
      render(<HistoryPage />);
      await openCostTab();
      await waitFor(() => expect(fieldsUsed().has('_import_cost')).toBe(true));
      expect(fieldsUsed().has('_car_cost')).toBe(false);
      expect(screen.queryByText('Car Charging')).toBeNull();
    });

    it('plots the car cost beside the import cost and totals it', async () => {
      settingsData = { forecast_exclude_load_above_kw: 3 };
      summaryCarCost = 0.92;
      render(<HistoryPage />);
      await openCostTab();
      await waitFor(() => expect(fieldsUsed().has('_car_cost')).toBe(true));
      expect(fieldsUsed().has('_import_cost')).toBe(true);
      expect((await screen.findAllByText('Car Charging Cost')).length).toBeGreaterThan(0);
      expect(await screen.findByText('£0.92')).toBeTruthy();
    });

    it('explains in an ⓘ that the car cost is part of the import cost', async () => {
      settingsData = { forecast_exclude_load_above_kw: 3 };
      summaryCarCost = 0.92;
      render(<HistoryPage />);
      await openCostTab();
      const tip = await screen.findByRole('button', { name: 'More about Car Charging' });
      const text = document.getElementById(tip.getAttribute('aria-describedby')!)!.textContent;
      expect(text).toMatch(/included in Import Cost/i);
      expect(text).toMatch(/grid/i);
    });

    it('shows no car tile when the server sends no cost', async () => {
      settingsData = { forecast_exclude_load_above_kw: 3 };
      summaryCarCost = null;
      render(<HistoryPage />);
      await openCostTab();
      expect(await screen.findByText('Net Cost')).toBeTruthy();
      expect(screen.queryByRole('button', { name: 'More about Car Charging' })).toBeNull();
    });
  });
});
