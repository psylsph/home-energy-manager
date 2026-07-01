import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Mocks - the Cost tab breaks the import cost into its per-kWh energy
// component and the fixed daily standing charge, but ONLY when a standing
// charge is configured. HistoryPage learns that from GET /api/settings, so
// these tests drive `import_standing_charge_p_per_day` via the apiGet mock
// and assert on the fields HistoryPage then requests from fetchHistory.
// ---------------------------------------------------------------------------

type FetchHistoryCall = { range: string; fields: string[]; offset: number };

const fetchHistoryCalls: FetchHistoryCall[] = [];
const fetchHistoryMock = vi.fn(async (...args: unknown[]) => {
  const [range, fields, offset] = args as [string, string[], number];
  fetchHistoryCalls.push({ range, fields, offset });
  return {};
});

// Standing charge served by the mocked /api/settings; each test sets it.
let standingChargePPerDay = 0;
const apiGetMock = vi.fn(async (path: string) => {
  if (path === '/api/settings') {
    return { ok: true, data: { import_standing_charge_p_per_day: standingChargePPerDay } };
  }
  return { ok: true, data: {} };
});

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

// Imported after the vi.mock() calls above.
import HistoryPage from '../../src/pages/HistoryPage';
import { useInverterStore } from '../../src/store/useInverterStore';

async function clickTab(label: string) {
  const btn = await screen.findByRole('button', { name: label, exact: true });
  fireEvent.click(btn);
}

const IMPORT_COST = '_import_cost';
const ENERGY_COST = '_import_energy_cost';
const STANDING_CHARGE = '_import_standing_charge';
const EXPORT_INCOME = '_export_income';

describe('<HistoryPage/> - Cost tab import breakdown', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    fetchHistoryCalls.length = 0;
    fetchHistoryMock.mockClear();
    apiGetMock.mockClear();
    standingChargePPerDay = 0;
    useInverterStore.setState({ snapshot: null, chartRange: '24h' });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it('requests the energy + standing-charge breakdown when a standing charge is set', async () => {
    standingChargePPerDay = 54.86;
    render(<HistoryPage />);
    await clickTab('Cost');
    await waitFor(() => {
      const fieldsUsed = new Set(fetchHistoryCalls.flatMap((c) => c.fields));
      expect(fieldsUsed.has(ENERGY_COST)).toBe(true);
    });
    const fieldsUsed = new Set(fetchHistoryCalls.flatMap((c) => c.fields));
    // All four import-cost lines plus export income are requested.
    expect(fieldsUsed.has(IMPORT_COST)).toBe(true);
    expect(fieldsUsed.has(ENERGY_COST)).toBe(true);
    expect(fieldsUsed.has(STANDING_CHARGE)).toBe(true);
    expect(fieldsUsed.has(EXPORT_INCOME)).toBe(true);
  });

  it('omits the breakdown fields when no standing charge is configured', async () => {
    standingChargePPerDay = 0;
    render(<HistoryPage />);
    await clickTab('Cost');
    // Wait until the Cost tab has fired its fetch (the total is always requested).
    await waitFor(() => {
      const fieldsUsed = new Set(fetchHistoryCalls.flatMap((c) => c.fields));
      expect(fieldsUsed.has(IMPORT_COST)).toBe(true);
    });
    const fieldsUsed = new Set(fetchHistoryCalls.flatMap((c) => c.fields));
    // Unchanged chart: only total import cost + export income, no breakdown.
    expect(fieldsUsed.has(EXPORT_INCOME)).toBe(true);
    expect(fieldsUsed.has(ENERGY_COST)).toBe(false);
    expect(fieldsUsed.has(STANDING_CHARGE)).toBe(false);
  });
});
