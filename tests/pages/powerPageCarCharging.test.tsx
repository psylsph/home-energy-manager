import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, within, act } from '@testing-library/react';

// Issue #454 phase 3: the Power page splits the car out of the load once a
// car-charging threshold is set — the live tiles, the chart and the
// Consumption Report with its CSV and HTML exports.

let settingsData: Record<string, unknown> = {};
// What the server adds to /api/report when a car-charging threshold is set.
let reportCar: Record<string, unknown> = {};
const apiGetMock = vi.fn(async (path: string) => {
  if (path.startsWith('/api/report')) {
    return {
      ok: true, import_cost_gbp: 0, export_income_gbp: 0, net_cost_gbp: 0,
      standing_charge_gbp: 0, standing_charge_p_per_day: 0, days_in_range: 1,
      ...reportCar,
    };
  }
  if (path === '/api/settings') return { ok: true, data: settingsData };
  return { ok: true, data: {} };
});
const fetchHistoryMock = vi.fn();

vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGetMock(...(args as [string])),
  fetchHistory: (...args: unknown[]) => fetchHistoryMock(...args),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  isTauri: false,
}));

vi.mock('recharts', () => ({
  ResponsiveContainer: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  ComposedChart: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  Area: () => null,
  Line: () => null,
  ReferenceLine: () => null,
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

import PowerPage from '../../src/pages/PowerPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

const MIN = 60_000;
// Inside the page's default 24 h window, which the report integrates over.
const T0 = Math.floor((Date.now() - 30 * MIN) / MIN) * MIN;
const series = (values: number[]) => values.map((v, i) => ({ t: T0 + i * MIN, v }));

/** Ten minutes of a 9 kW load, 7.36 kW of it the car. */
function historyWithCar(): Record<string, { t: number; v: number }[]> {
  const n = 10;
  return {
    solar_power: series(Array(n).fill(0)),
    battery_power: series(Array(n).fill(0)),
    grid_power: series(Array(n).fill(-9000)),
    home_power: series(Array(n).fill(9000)),
    soc: series(Array(n).fill(50)),
    _car_power: series(Array(n).fill(7360)),
    _home_excl_car_power: series(Array(n).fill(1640)),
  };
}

function historyWithoutCar() {
  const { _car_power: _c, _home_excl_car_power: _h, ...rest } = historyWithCar();
  void _c;
  void _h;
  return rest;
}

const snap = (overrides: Partial<InverterSnapshot> = {}) =>
  ({
    timestamp: Math.floor(Date.now() / 1000),
    solar_power: 0, battery_power: 0, grid_power: -9000, grid_voltage: 240,
    home_power: 9000, soc: 50, ...overrides,
  }) as unknown as InverterSnapshot;

/** Let the /api/report response land before exporting. */
const settleReport = () => act(async () => { await new Promise((r) => setTimeout(r, 30)); });
const stats = () => screen.getByTestId('power-stats');
const tile = (label: string) => within(stats()).getByText(label, { selector: 'span' }).closest('div')!.parentElement!;

describe('<PowerPage/> — car charging split (issue #454)', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    settingsData = {};
    reportCar = {};
    apiGetMock.mockClear();
    fetchHistoryMock.mockReset();
    fetchHistoryMock.mockResolvedValue(historyWithoutCar());
    useInverterStore.setState({
      snapshot: null, chartRange: '24h', gridLineWeight: 'standard',
      evcHost: '', evcConnected: false, evcPower: 0,
    });
  });
  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  describe('live tiles', () => {
    it('shows one Load / Home tile when no threshold is set', () => {
      useInverterStore.setState({ snapshot: snap() });
      render(<PowerPage />);
      expect(within(stats()).getByText('Load / Home')).toBeTruthy();
      expect(within(stats()).queryByText('Car Charging')).toBeNull();
      expect(within(tile('Load / Home')).getByText('9.0kW')).toBeTruthy();
    });

    it('splits the tile into Home and an estimated Car Charging once a threshold is set', async () => {
      settingsData = { forecast_exclude_load_above_kw: 3 };
      useInverterStore.setState({ snapshot: snap() });
      render(<PowerPage />);
      await within(stats()).findByText('Car Charging');
      // 32 A at 240 V is 7680 W; the rest of the 9 kW load is the house.
      expect(within(tile('Car Charging')).getByText('7.7kW')).toBeTruthy();
      expect(within(tile('Home')).getByText('1.3kW')).toBeTruthy();
      expect(within(stats()).queryByText('Load / Home')).toBeNull();
    });

    it('uses the saved charger current', async () => {
      settingsData = { forecast_exclude_load_above_kw: 3, car_charger_current_a: 16 };
      useInverterStore.setState({ snapshot: snap() });
      render(<PowerPage />);
      await within(stats()).findByText('Car Charging');
      expect(within(tile('Car Charging')).getByText('3.8kW')).toBeTruthy();
    });

    it('shows no car while the load is under the threshold', async () => {
      settingsData = { forecast_exclude_load_above_kw: 3 };
      useInverterStore.setState({ snapshot: snap({ home_power: 800 }) });
      render(<PowerPage />);
      await within(stats()).findByText('Car Charging');
      expect(within(tile('Car Charging')).getByText('0W')).toBeTruthy();
      expect(within(tile('Home')).getByText('800W')).toBeTruthy();
    });

    it("prefers a polled charger's measured power over the estimate", async () => {
      settingsData = { forecast_exclude_load_above_kw: 3 };
      useInverterStore.setState({
        snapshot: snap(), evcHost: '192.168.1.50', evcConnected: true, evcPower: 6500,
      });
      render(<PowerPage />);
      await within(stats()).findByText('Car Charging');
      expect(within(tile('Car Charging')).getByText('6.5kW')).toBeTruthy();
      expect(within(tile('Home')).getByText('2.5kW')).toBeTruthy();
    });

    it('never lets the house go negative when the measured charger exceeds the load', async () => {
      settingsData = { forecast_exclude_load_above_kw: 3 };
      useInverterStore.setState({
        snapshot: snap({ home_power: 5000 }), evcHost: '192.168.1.50', evcConnected: true, evcPower: 7000,
      });
      render(<PowerPage />);
      await within(stats()).findByText('Car Charging');
      expect(within(tile('Car Charging')).getByText('5.0kW')).toBeTruthy();
      expect(within(tile('Home')).getByText('0W')).toBeTruthy();
    });

    it('keeps the explanation behind an ⓘ on the Car Charging tile', async () => {
      settingsData = { forecast_exclude_load_above_kw: 3 };
      useInverterStore.setState({ snapshot: snap() });
      render(<PowerPage />);
      await within(stats()).findByText('Car Charging');
      const tip = screen.getByRole('button', { name: 'More about Car Charging' });
      const text = document.getElementById(tip.getAttribute('aria-describedby')!)!.textContent;
      expect(text).toMatch(/estimate/i);
    });
  });

  describe('chart', () => {
    it('always asks the server for the car series, which it omits when the feature is off', async () => {
      render(<PowerPage />);
      await waitFor(() => expect(fetchHistoryMock).toHaveBeenCalled());
      const fields = fetchHistoryMock.mock.calls[0][1] as string[];
      expect(fields).toContain('_car_power');
      expect(fields).toContain('_home_excl_car_power');
    });

    it('leaves the legend alone without car data', async () => {
      render(<PowerPage />);
      await waitFor(() => expect(screen.getAllByText('Load / Home').length).toBeGreaterThan(0));
      expect(screen.queryByText('Car Charging')).toBeNull();
    });

    it('plots Home without the car and adds a Car Charging series when the server returns one', async () => {
      fetchHistoryMock.mockResolvedValue(historyWithCar());
      render(<PowerPage />);
      expect(await screen.findByRole('button', { name: /Car Charging/ })).toBeTruthy();
      expect(screen.getByRole('button', { name: /Home \(Excl\. Car\)/ })).toBeTruthy();
      expect(screen.queryByRole('button', { name: /Load \/ Home/ })).toBeNull();
    });

    it('does not mistake a one-minute car charge for a spike', async () => {
      const h = historyWithCar();
      h._car_power = series([0, 0, 0, 7360, 0, 0, 0]);
      h._home_excl_car_power = series(Array(7).fill(1640));
      h.home_power = series([1640, 1640, 1640, 9000, 1640, 1640, 1640]);
      for (const k of ['solar_power', 'battery_power', 'grid_power', 'soc']) h[k] = h[k].slice(0, 7);
      fetchHistoryMock.mockResolvedValue(h);
      const created: Blob[] = [];
      Object.defineProperty(URL, 'createObjectURL', {
        configurable: true,
        value: (b: Blob) => { created.push(b); return 'blob:csv'; },
      });
      Object.defineProperty(URL, 'revokeObjectURL', { configurable: true, value: vi.fn() });
      vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {});
      render(<PowerPage />);
      await waitFor(() => expect(screen.getByRole('button', { name: 'CSV' }).hasAttribute('disabled')).toBe(false));
      fireEvent.click(screen.getByRole('button', { name: 'CSV' }));
      await waitFor(() => expect(created.length).toBe(1));
      const csv = await created[0].text();
      expect(csv).toContain('7360');
    });
  });

  describe('Consumption Report', () => {
    async function exportCsv(data: Record<string, { t: number; v: number }[]>) {
      fetchHistoryMock.mockResolvedValue(data);
      const created: Blob[] = [];
      Object.defineProperty(URL, 'createObjectURL', {
        configurable: true,
        value: (b: Blob) => { created.push(b); return 'blob:csv'; },
      });
      Object.defineProperty(URL, 'revokeObjectURL', { configurable: true, value: vi.fn() });
      vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {});
      render(<PowerPage />);
      await waitFor(() => expect(screen.getByRole('button', { name: 'CSV' }).hasAttribute('disabled')).toBe(false));
      await settleReport();
      fireEvent.click(screen.getByRole('button', { name: 'CSV' }));
      await waitFor(() => expect(created.length).toBe(1));
      return created[0].text();
    }

    async function exportHtml(data: Record<string, { t: number; v: number }[]>) {
      fetchHistoryMock.mockResolvedValue(data);
      const write = vi.fn();
      vi.spyOn(window, 'open').mockReturnValue({
        document: { open: vi.fn(), write, close: vi.fn() },
        focus: vi.fn(),
      } as never);
      render(<PowerPage />);
      await waitFor(() => expect(screen.getByRole('button', { name: 'CSV' }).hasAttribute('disabled')).toBe(false));
      await settleReport();
      fireEvent.click(screen.getByRole('button', { name: /Consumption Report/i }));
      await waitFor(() => expect(write).toHaveBeenCalled());
      return String(write.mock.calls[0][0]);
    }

    it('adds the car to the CSV summary, buckets and samples', async () => {
      const csv = await exportCsv(historyWithCar());
      expect(csv).toContain('Total Home Load kWh');
      expect(csv).toMatch(/Car Charging kWh,"?1\.104"?/); // 7.36 kW x 9 min
      expect(csv).toContain('Peak Car Charging W,7360');
      expect(csv).toMatch(/Bucket,Solar kWh,Home Load kWh,Car Charging kWh/);
      expect(csv).toMatch(/Timestamp ISO,Timestamp Local,Solar W,Battery W,Battery Direction,Grid W,Grid Direction,Home Load W,Car Charging W,SOC %/);
    });

    it('leaves the CSV exactly as it was without car data', async () => {
      const csv = await exportCsv(historyWithoutCar());
      expect(csv).not.toContain('Car Charging');
    });

    it('adds the car to the HTML report', async () => {
      const html = await exportHtml(historyWithCar());
      expect(html).toContain('Car Charging');
      expect(html).toContain('<th>Car</th>');
    });

    it('shows what the car cost, apart from the rest of the import cost, in the HTML report', async () => {
      reportCar = { car_cost_gbp: 0.92, car_grid_kwh: 3.68 };
      const html = await exportHtml(historyWithCar());
      expect(html).toContain('<span>Car Charging Cost</span>');
      expect(html).toContain('£0.92');
      expect(html).toContain('3.7 kWh from the grid');
    });

    it('shows the car cost in the CSV summary', async () => {
      reportCar = { car_cost_gbp: 0.92, car_grid_kwh: 3.68 };
      const csv = await exportCsv(historyWithCar());
      expect(csv).toContain('Car Charging Cost GBP,0.92');
      expect(csv).toContain('Car Charging Grid kWh,3.680');
    });

    it('shows no car cost when the server sends none', async () => {
      reportCar = { car_cost_gbp: null, car_grid_kwh: null };
      const html = await exportHtml(historyWithCar());
      expect(html).not.toContain('Car Charging Cost');
      reportCar = {};
      cleanup();
      const csv = await exportCsv(historyWithCar());
      expect(csv).not.toContain('Car Charging Cost');
    });

    it('does not carry a car cost over to a report without car data', async () => {
      reportCar = { car_cost_gbp: 0.92, car_grid_kwh: 3.68 };
      const html = await exportHtml(historyWithoutCar());
      expect(html).not.toContain('Car Charging');
    });

    it('leaves the HTML report as it was without car data', async () => {
      const html = await exportHtml(historyWithoutCar());
      expect(html).not.toContain('Car Charging');
      expect(html).not.toContain('<th>Car</th>');
    });
  });
});
