import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Mocks — HistoryPage pulls in api (fetchHistory + apiGet), the Zustand
// store, the chart library, and several helpers. Stub the side-effecting
// pieces and capture the fetchHistory calls so we can assert the
// directional-field wiring (PR #166).
// ---------------------------------------------------------------------------

type FetchHistoryCall = {
  range: string;
  fields: string[];
  offset: number;
};

const fetchHistoryCalls: FetchHistoryCall[] = [];
const fetchHistoryMock = vi.fn(
  async (...args: unknown[]) => {
    const [range, fields, offset] = args as [string, string[], number];
    fetchHistoryCalls.push({ range, fields, offset });
    return {};
  },
);

const apiGetMock = vi.fn(async () => ({ ok: true, data: {} }));

vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => {
    const [path] = args as [string];
    return apiGetMock(path);
  },
  fetchHistory: (...args: unknown[]) => fetchHistoryMock(...args),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  isTauri: false,
}));

// Imported after the vi.mock() calls above.
import HistoryPage from '../../src/pages/HistoryPage';
import { useInverterStore } from '../../src/store/useInverterStore';

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

/**
 * Switch the History tab by clicking its button. The default tab is
 * 'battery' so we don't need to click anything for that one.
 */
async function clickTab(label: string) {
  const btn = await screen.findByRole('button', { name: label, exact: true });
  fireEvent.click(btn);
}

describe('<HistoryPage/> — directional power field wiring (PR #166)', () => {
  beforeEach(() => {
    silenceConsoleError();
    fetchHistoryCalls.length = 0;
    fetchHistoryMock.mockClear();
    apiGetMock.mockClear();
    // Reset the persisted chart range so each test starts on a known default.
    useInverterStore.setState({
      snapshot: null,
      chartRange: '24h',
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  describe('Battery tab', () => {
    it('requests _charge_power and _discharge_power on the chart', async () => {
      render(<HistoryPage />);
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      // Find a call that includes the Battery tab's charge / discharge
      // chart fields. Multiple calls may be issued per render cycle (one
      // per rolling state), so look at every one.
      const fieldsUsed = new Set(fetchHistoryCalls.flatMap((c) => c.fields));
      expect(fieldsUsed.has('_charge_power')).toBe(true);
      expect(fieldsUsed.has('_discharge_power')).toBe(true);
    });

    it('no longer requests the raw battery_power for the directional chart', async () => {
      // PR #166 dropped the client-side `transform` flag from the Battery
      // tab's power chart and pointed it at the new server-derived
      // directional series. The raw `battery_power` is still on the
      // whitelist but is no longer requested by this chart.
      render(<HistoryPage />);
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      const fieldsUsed = new Set(fetchHistoryCalls.flatMap((c) => c.fields));
      expect(fieldsUsed.has('battery_power')).toBe(false);
    });
  });

  describe('Grid tab', () => {
    it('requests _grid_import_power and _grid_export_power on the chart', async () => {
      render(<HistoryPage />);
      await clickTab('Grid');
      await waitFor(() => {
        // The tab switch triggers a new fetch; spin until the grid
        // directional fields appear in some call.
        const fieldsUsed = new Set(fetchHistoryCalls.flatMap((c) => c.fields));
        expect(fieldsUsed.has('_grid_import_power')).toBe(true);
      });
      const fieldsUsed = new Set(fetchHistoryCalls.flatMap((c) => c.fields));
      expect(fieldsUsed.has('_grid_export_power')).toBe(true);
    });

    it('no longer requests the raw grid_power for the directional chart', async () => {
      render(<HistoryPage />);
      await clickTab('Grid');
      // Spin until fetchHistory has had a chance to re-fire with the Grid
      // tab's chart field list. We assert the field is *never* present.
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(1);
      });
      const fieldsUsed = new Set(fetchHistoryCalls.flatMap((c) => c.fields));
      expect(fieldsUsed.has('grid_power')).toBe(false);
    });

    it('requests the daily grid energy counters for the cumulative chart', async () => {
      render(<HistoryPage />);
      await clickTab('Grid');
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(1);
      });
      const fieldsUsed = new Set(fetchHistoryCalls.flatMap((c) => c.fields));
      expect(fieldsUsed.has('today_import_kwh')).toBe(true);
      expect(fieldsUsed.has('today_export_kwh')).toBe(true);
    });
  });

  describe('tab isolation', () => {
    it('does not request battery-side fields when on the Grid tab', async () => {
      render(<HistoryPage />);
      // Let the initial Battery fetch fire, then snapshot the call count
      // so the post-tab-switch assertion only inspects the Grid-side calls.
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      const callsBeforeGrid = fetchHistoryCalls.length;
      await clickTab('Grid');
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(callsBeforeGrid);
      });
      // Slice off the pre-switch calls; only inspect calls fired after the
      // Grid tab was activated.
      const gridCalls = fetchHistoryCalls.slice(callsBeforeGrid);
      const gridFields = new Set(gridCalls.flatMap((c) => c.fields));
      expect(gridFields.has('_charge_power')).toBe(false);
      expect(gridFields.has('_discharge_power')).toBe(false);
    });

    it('does not request grid-side fields when on the Battery tab', async () => {
      render(<HistoryPage />);
      // Default tab is Battery; just wait for the initial fetch.
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      const fieldsUsed = new Set(fetchHistoryCalls.flatMap((c) => c.fields));
      expect(fieldsUsed.has('_grid_import_power')).toBe(false);
      expect(fieldsUsed.has('_grid_export_power')).toBe(false);
    });
  });

  // Sanity: if no fetch happens at all, the field-list assertions above
  // silently no-op. Pin that fetchHistory fires on mount so we know the
  // page rendered far enough to query the API.
  it('fires at least one fetchHistory call on mount', async () => {
    render(<HistoryPage />);
    await waitFor(() => {
      expect(fetchHistoryCalls.length).toBeGreaterThan(0);
    });
  });
});
