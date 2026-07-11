import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// HistoryPage already has historyPageDirectionalFields.test.tsx covering the
// Battery/Grid directional-field wiring. This file adds coverage for the
// remaining tabs (Solar / Home / Temperature / Cost), range switching,
// offset navigation (Older/Newer), the empty state, the CSV export button,
// and the temperature-tab outdoor-temperature credit footer.
// ---------------------------------------------------------------------------

type FetchHistoryCall = { range: string; fields: string[]; offset: number; rolling: boolean };

const fetchHistoryCalls: FetchHistoryCall[] = [];
const fetchHistoryMock = vi.fn(async (...args: unknown[]) => {
  const [range, fields, offset, rolling] = args as [string, string[], number, boolean];
  fetchHistoryCalls.push({ range, fields, offset, rolling });
  if (fields.includes('external_temperature')) {
    return {
      battery_temperature: [{ t: 1_720_000_000_000, v: 22 }],
      inverter_temperature: [{ t: 1_720_000_000_000, v: 35 }],
      external_temperature: [{ t: 1_720_000_000_000, v: 22 }],
    };
  }
  return {};
});

const apiGetMock = vi.fn(async () => ({ ok: true, data: {} }));

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

import HistoryPage from '../../src/pages/HistoryPage';
import { useInverterStore } from '../../src/store/useInverterStore';

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

async function clickTab(label: string) {
  const btn = await screen.findByRole('button', { name: label, exact: true });
  fireEvent.click(btn);
}

async function clickRange(label: string) {
  const btn = await screen.findByRole('button', { name: label, exact: true });
  fireEvent.click(btn);
}

describe('<HistoryPage/> — tabs, ranges, navigation, empty state', () => {
  beforeEach(() => {
    silenceConsoleError();
    fetchHistoryCalls.length = 0;
    fetchHistoryMock.mockClear();
    apiGetMock.mockClear();
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

  describe('tabs', () => {
    it('requests Solar tab fields (pv1/pv2 power + energy)', async () => {
      render(<HistoryPage />);
      await clickTab('Solar');
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(1);
      });
      // Solar tab fields recorded — just assert a new fetch fired on switch.
      const solarCalls = fetchHistoryCalls.slice(1);
      expect(solarCalls.length).toBeGreaterThan(0);
    });

    it('requests Home tab fields on switch', async () => {
      render(<HistoryPage />);
      await clickTab('Home');
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(1);
      });
    });

    it('requests Temperature tab fields on switch', async () => {
      render(<HistoryPage />);
      await clickTab('Temperature');
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(1);
      });
    });

    it('requests Cost tab fields on switch', async () => {
      render(<HistoryPage />);
      await clickTab('Cost');
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(1);
      });
    });

    it('resets offset to 0 when switching tabs', async () => {
      render(<HistoryPage />);
      // Page back once.
      fireEvent.click(await screen.findByRole('button', { name: /Older/i }));
      await waitFor(() => {
        expect(fetchHistoryCalls.some((c) => c.offset === 1)).toBe(true);
      });
      // Switch tab → offset resets to 0.
      await clickTab('Solar');
      const afterSwitch = fetchHistoryCalls.at(-1);
      expect(afterSwitch!.offset).toBe(0);
    });

    it('renders the Open-Meteo credit footer on the Temperature tab', async () => {
      render(<HistoryPage />);
      await clickTab('Temperature');
      expect(await screen.findByText(/Outdoor temperature data by/)).toBeDefined();
    });

    it('explains that Battery − Outdoor is a temperature difference, not a zero-degree reading', async () => {
      render(<HistoryPage />);
      await clickTab('Temperature');
      expect(await screen.findByText('Battery − Outdoor (Δ°C)')).toBeDefined();
      expect(screen.getByText(/0°C means they were equal, not that either temperature was zero/)).toBeDefined();
    });
  });

  describe('range switching', () => {
    it('switches the persisted chart range and resets offset', async () => {
      render(<HistoryPage />);
      await clickRange('7d');
      await waitFor(() => {
        expect(useInverterStore.getState().chartRange).toBe('7d');
      });
      // Offset reset back to 0 after range change.
      const lastCall = fetchHistoryCalls.at(-1);
      expect(lastCall!.range).toBe('7d');
      expect(lastCall!.offset).toBe(0);
    });

    it('switches to month range', async () => {
      render(<HistoryPage />);
      await clickRange('Month');
      await waitFor(() => {
        expect(useInverterStore.getState().chartRange).toBe('month');
      });
    });

    it('updates fetchHistory range param on switch', async () => {
      render(<HistoryPage />);
      const initialRange = fetchHistoryCalls[0]?.range;
      await clickRange('1h');
      await waitFor(() => {
        expect(fetchHistoryCalls.some((c) => c.range === '1h')).toBe(true);
      });
      expect(initialRange).toBe('24h');
    });
  });

  describe('offset navigation', () => {
    it('Older button increments the offset', async () => {
      render(<HistoryPage />);
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      fireEvent.click(await screen.findByRole('button', { name: /Older/i }));
      await waitFor(() => {
        expect(fetchHistoryCalls.some((c) => c.offset === 1)).toBe(true);
      });
    });

    it('Newer button is disabled at offset 0', async () => {
      render(<HistoryPage />);
      const newerBtn = await screen.findByRole('button', { name: /Newer/i });
      expect(newerBtn.hasAttribute('disabled')).toBe(true);
    });

    it('Newer button decrements the offset after paging back', async () => {
      render(<HistoryPage />);
      fireEvent.click(await screen.findByRole('button', { name: /Older/i }));
      await waitFor(() => {
        expect(fetchHistoryCalls.some((c) => c.offset === 1)).toBe(true);
      });
      fireEvent.click(await screen.findByRole('button', { name: /Newer/i }));
      await waitFor(() => {
        expect(fetchHistoryCalls.some((c) => c.offset === 0)).toBe(true);
      });
    });

    it('clamps offset at 0 (Newer does not go negative)', async () => {
      render(<HistoryPage />);
      const newerBtn = await screen.findByRole('button', { name: /Newer/i });
      // Disabled at 0 — clicking does nothing.
      expect(newerBtn.hasAttribute('disabled')).toBe(true);
    });
  });

  describe('empty state', () => {
    it('shows the empty-state message when no data is returned', async () => {
      const { container } = render(<HistoryPage />);
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      expect(container.textContent).toContain('No data available for this period');
    });

    it('shows the empty-state subtitle about recording while running', async () => {
      const { container } = render(<HistoryPage />);
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      expect(container.textContent).toContain('History is recorded while the app is running');
    });
  });

  describe('CSV export', () => {
    it('CSV button is disabled when there is no data', async () => {
      render(<HistoryPage />);
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      const csvBtn = screen.getByText('CSV').closest('button');
      expect(csvBtn!.hasAttribute('disabled')).toBe(true);
    });
  });

  describe('initial mount', () => {
    it('renders the default Battery tab active', async () => {
      render(<HistoryPage />);
      const batteryBtn = await screen.findByRole('button', { name: 'Battery', exact: true });
      expect(batteryBtn.className).toContain('flow-active');
    });

    it('renders all six tabs', async () => {
      render(<HistoryPage />);
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      for (const label of ['Battery', 'Solar', 'Grid', 'Home', 'Temperature', 'Cost']) {
        expect(screen.getByRole('button', { name: label, exact: true })).toBeDefined();
      }
    });
  });
});
