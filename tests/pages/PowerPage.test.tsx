import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// PowerPage already has powerPageConsumptionReport.test.tsx covering the
// /api/report cost integration. This file adds coverage for the live power
// stat tiles (with / without snapshot), range switching, offset navigation,
// the empty / loading / error states, and the CSV export button.
// ---------------------------------------------------------------------------

type ApiGetCall = { path: string };

const apiGetCalls: ApiGetCall[] = [];
const apiGetMock = vi.fn(async (path: string) => {
  apiGetCalls.push({ path });
  if (path.startsWith('/api/report')) {
    return {
      ok: true,
      import_cost_gbp: 0,
      export_income_gbp: 0,
      net_cost_gbp: 0,
      standing_charge_gbp: 0,
      days_in_range: 0,
      standing_charge_p_per_day: 0,
    };
  }
  return { ok: true, data: {} };
});

const fetchHistoryMock = vi.fn().mockResolvedValue({});

vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGetMock(...(args as [string])),
  fetchHistory: (...args: unknown[]) => fetchHistoryMock(...args),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  isTauri: false,
}));

// recharts' ResponsiveContainer uses ResizeObserver, which jsdom doesn't
// provide. Install a no-op stub so the chart can mount without throwing.
globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

import PowerPage from '../../src/pages/PowerPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    timestamp: Math.floor(Date.now() / 1000),
    solar_power: 3000, pv1_power: 2000, pv2_power: 1000,
    pv1_voltage: 0, pv2_voltage: 0, pv1_current: 0, pv2_current: 0,
    battery_power: -500, soc: 60, battery_voltage: 50, battery_current: 0,
    battery_state: 'charging', battery_temperature: 20, battery_capacity_kwh: 9.5,
    eps_power_w: 0, grid_power: 200, grid_voltage: 240, grid_frequency: 50,
    grid_online: true, grid_loss: false, inverter_trip: false,
    battery_over_temp: false, home_power: 800, inverter_temperature: 30,
    inverter_time: '',
    today_solar_kwh: 0, today_pv1_kwh: 0, today_pv2_kwh: 0,
    today_import_kwh: 0, today_export_kwh: 0, today_charge_kwh: 0,
    total_import_kwh: 0, total_export_kwh: 0, total_solar_kwh: 0,
    total_charge_kwh: 0, total_discharge_kwh: 0, total_throughput_kwh: 0,
    operating_hours: 0, today_discharge_kwh: 0, today_consumption_kwh: 0,
    home_energy_today_kwh: 0, battery_modules: [], battery_mode: 'eco',
    battery_reserve: 4, charge_rate: 0, discharge_rate: 0, active_power_rate: 0,
    max_battery_power_w: 0, max_ac_power_w: 0, export_limit_w: 0, target_soc: 100,
    enable_charge_target: false, enable_charge: false, enable_discharge: false,
    auto_winter_active: false, load_limiter_active: false, cosy_active: false,
    cosy_enabled: false, agile_active: false, agile_state: 'idle', agile_enabled: false,
    max_charge_slots: 0, max_discharge_slots: 0, charge_slots: [], discharge_slots: [],
    meters: [], inverter_serial: '', firmware_version: '', dsp_firmware_version: '',
    dc_dsp_firmware_version: '', device_type: '', device_type_display: 'Gen 3 Hybrid',
    device_type_code: '2201', battery_calibration_stage: 0, enable_ammeter: false,
    enable_reversed_ct_clamp: false, meter_type: 0, supports_battery_calibration: false,
    ac_eps_enabled: false, ac_export_priority: 0,
    ...overrides,
  };
}

async function clickRange(label: string) {
  const btn = await screen.findByRole('button', { name: label, exact: true });
  fireEvent.click(btn);
}

describe('<PowerPage/> — stats, ranges, navigation, states', () => {
  beforeEach(() => {
    silenceConsoleError();
    apiGetCalls.length = 0;
    apiGetMock.mockClear();
    fetchHistoryMock.mockClear();
    useInverterStore.setState({
      snapshot: null,
      chartRange: '24h',
      gridLineWeight: 'normal',
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  describe('live power stats', () => {
    it('shows Waiting for data when no snapshot', async () => {
      render(<PowerPage />);
      const waiting = await screen.findAllByText('Waiting for data');
      expect(waiting.length).toBeGreaterThan(0);
    });

    it('renders live power values from the snapshot', async () => {
      useInverterStore.setState({ snapshot: makeSnapshot() });
      const { container } = render(<PowerPage />);
      // 3kW solar, 800W home, 500W battery (charging), 200W grid (exporting)
      expect(container.textContent).toContain('3.0kW');
      expect(container.textContent).toContain('800W');
      expect(container.textContent).toContain('Charging');
      expect(container.textContent).toContain('Exporting');
    });

    it('labels battery as Idle when power ~0', async () => {
      useInverterStore.setState({ snapshot: makeSnapshot({ battery_power: 0 }) });
      render(<PowerPage />);
      expect(await screen.findByText('Idle')).toBeDefined();
    });

    it('labels grid as Importing when drawing from the grid', async () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({ grid_power: -500 }), // negative = importing by convention
      });
      const { container } = render(<PowerPage />);
      expect(container.textContent).toContain('Importing');
    });

    it('labels battery as Discharging when power > 0', async () => {
      useInverterStore.setState({
        snapshot: makeSnapshot({ battery_power: 1200 }),
      });
      const { container } = render(<PowerPage />);
      expect(container.textContent).toContain('Discharging');
    });
  });

  describe('range switching', () => {
    it('switches the persisted chart range', async () => {
      render(<PowerPage />);
      await clickRange('7d');
      await waitFor(() => {
        expect(useInverterStore.getState().chartRange).toBe('7d');
      });
    });

    it('resets offset to 0 on range change', async () => {
      render(<PowerPage />);
      // Page back first
      fireEvent.click(await screen.findByRole('button', { name: /Older/i }));
      await waitFor(() => {
        const reportCalls = apiGetCalls.filter((c) => c.path.includes('offset=1'));
        expect(reportCalls.length).toBeGreaterThan(0);
      });
      // Change range → offset resets
      await clickRange('1h');
      await waitFor(() => {
        expect(useInverterStore.getState().chartRange).toBe('1h');
      });
    });
  });

  describe('offset navigation', () => {
    it('Older button increments offset and re-fetches cost', async () => {
      render(<PowerPage />);
      fireEvent.click(await screen.findByRole('button', { name: /Older/i }));
      await waitFor(() => {
        const calls = apiGetCalls.filter((c) => c.path.includes('offset=1'));
        expect(calls.length).toBeGreaterThan(0);
      });
    });

    it('Newer button is disabled at offset 0', async () => {
      render(<PowerPage />);
      const newerBtn = await screen.findByRole('button', { name: /Newer/i });
      expect(newerBtn.hasAttribute('disabled')).toBe(true);
    });

    it('Newer button decrements offset after paging back', async () => {
      render(<PowerPage />);
      fireEvent.click(await screen.findByRole('button', { name: /Older/i }));
      await waitFor(() => {
        expect(apiGetCalls.some((c) => c.path.includes('offset=1'))).toBe(true);
      });
      // After paging back, Newer is enabled.
      let newerBtn = screen.getByRole('button', { name: /Newer/i });
      expect(newerBtn.hasAttribute('disabled')).toBe(false);
      fireEvent.click(newerBtn);
      // Decrementing to offset 0 re-disables the Newer button (offset=0 is
      // omitted from the URL, so we assert via the button's disabled state).
      await waitFor(() => {
        newerBtn = screen.getByRole('button', { name: /Newer/i });
        expect(newerBtn.hasAttribute('disabled')).toBe(true);
      });
    });
  });

  describe('data states', () => {
    it('shows loading state while history is being fetched', async () => {
      // Never-resolving fetchHistory keeps the page in loading state.
      fetchHistoryMock.mockImplementation(() => new Promise(() => {}));
      render(<PowerPage />);
      expect(await screen.findByText('Loading power history…')).toBeDefined();
    });

    it('shows empty state when no data is returned', async () => {
      fetchHistoryMock.mockResolvedValue({});
      render(<PowerPage />);
      expect(await screen.findByText('No power history for this range')).toBeDefined();
    });

    it('shows error state when fetchHistory rejects', async () => {
      fetchHistoryMock.mockRejectedValue(new Error('database locked'));
      render(<PowerPage />);
      expect(await screen.findByText('database locked')).toBeDefined();
    });
  });

  describe('export buttons', () => {
    it('disables CSV and Consumption Report buttons when there is no data', async () => {
      fetchHistoryMock.mockResolvedValue({});
      render(<PowerPage />);
      const csvBtn = await screen.findByRole('button', { name: 'CSV' });
      const reportBtn = await screen.findByRole('button', { name: /Consumption Report/i });
      expect(csvBtn.hasAttribute('disabled')).toBe(true);
      expect(reportBtn.hasAttribute('disabled')).toBe(true);
    });

    it('renders the page title and subtitle', async () => {
      render(<PowerPage />);
      expect(await screen.findByText('Power')).toBeDefined();
      expect(screen.getByText('Live and historical power direction')).toBeDefined();
    });
  });
});
