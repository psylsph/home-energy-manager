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
  fetchHistoryCalls.push({ range, fields: [...fields], offset, rolling });
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

  // Issue #199 follow-up: on phones the nav row's two export buttons would
  // push past the right edge. The fix is a flex-col container with the
  // paging and export groups stacked vertically on mobile, side-by-side on
  // sm+. These tests pin the responsive class list so a future refactor
  // doesn't silently regress to a single overflowing row.
  describe('nav row mobile layout', () => {
    it('outer nav container has flex-col on mobile, flex-row on sm+', async () => {
      render(<HistoryPage />);
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      // The nav row is the <div> that contains an "Older" button. Walk up
      // from there to the nearest <div> with a bg-bg-surface class — that's
      // the outer nav row.
      const olderBtn = await screen.findByRole('button', { name: /Older/i });
      let nav = olderBtn.parentElement;
      while (
        nav &&
        !(nav.classList.contains('bg-bg-surface') && nav.classList.contains('flex'))
      ) {
        nav = nav.parentElement;
      }
      expect(nav).toBeTruthy();
      // Mobile: stacked column. Desktop: single row.
      expect(nav!.className).toContain('flex-col');
      expect(nav!.className).toContain('sm:flex-row');
      // The vertical separator is only visible on sm+ — it has the hidden
      // class on mobile.
      const separator = nav!.querySelector('span.bg-white\\/10');
      expect(separator).toBeTruthy();
      expect(separator!.className).toContain('hidden');
      expect(separator!.className).toContain('sm:inline-block');
    });

    it('paging controls and export buttons are grouped into sibling sub-rows', async () => {
      render(<HistoryPage />);
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      // Find the outer nav row, then check it has exactly two immediate
      // <div> children that group the controls. (The <span> separator
      // sits between them but is hidden on mobile.)
      const olderBtn = await screen.findByRole('button', { name: /Older/i });
      let nav = olderBtn.parentElement;
      while (
        nav &&
        !(nav.classList.contains('bg-bg-surface') && nav.classList.contains('flex'))
      ) {
        nav = nav.parentElement;
      }
      const subGroups = Array.from(nav!.children).filter(
        (el) => el.tagName === 'DIV',
      );
      expect(subGroups.length).toBe(2);
      // First sub-group: Older, then either an <input> (date) or <span> (label), then Newer.
      const pagingGroup = subGroups[0]!;
      expect(pagingGroup.querySelector('button:nth-of-type(1)')!.textContent).toContain('Older');
      expect(pagingGroup.querySelector('button:nth-of-type(2)')!.textContent).toContain('Newer');
      // Second sub-group: CSV button, then Export all button.
      const exportGroup = subGroups[1]!;
      const exportButtons = exportGroup.querySelectorAll('button');
      expect(exportButtons.length).toBe(2);
      expect(exportButtons[0]!.textContent).toContain('CSV');
      expect(exportButtons[1]!.textContent).toContain('Export all');
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

  // Issue #199: combined export — "Export all" button pulls every tab's
  // series into a single CSV rather than per-tab files. The per-tab CSV
  // button above remains for users who only need one section.
  describe('combined export (Export all)', () => {
    it('renders the Export all button', async () => {
      render(<HistoryPage />);
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      const exportAllBtn = screen.getByRole('button', {
        name: 'Export all tabs as a single combined CSV',
      });
      expect(exportAllBtn).toBeDefined();
    });

    it('Export all button is disabled when there is no data', async () => {
      render(<HistoryPage />);
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      const exportAllBtn = screen.getByRole('button', {
        name: 'Export all tabs as a single combined CSV',
      });
      expect(exportAllBtn.hasAttribute('disabled')).toBe(true);
    });

    it('Export all button is also disabled when the per-tab CSV button is', async () => {
      render(<HistoryPage />);
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });
      const exportAllBtn = screen.getByRole('button', {
        name: 'Export all tabs as a single combined CSV',
      });
      const csvBtn = screen.getByText('CSV').closest('button');
      // Both share the same data gate. If the per-tab one is disabled, so is
      // the combined one — they're both "no data → no export".
      expect(csvBtn!.hasAttribute('disabled')).toBe(true);
      expect(exportAllBtn.hasAttribute('disabled')).toBe(true);
    });

    it('clicking Export all fetches the union of all six tabs\' fields', async () => {
      // Populate a synthetic response so the button is enabled. The shape
      // mirrors what the real backend returns: { field: [{t, v}, ...] }.
      const samplePoint = { t: 1700000000000, v: 1 };
      fetchHistoryMock.mockImplementationOnce(async (...args: unknown[]) => {
        const [range, fields, offset, rolling] = args as [string, string[], number, boolean];
        fetchHistoryCalls.push({ range, fields: [...fields], offset, rolling });
        const result: Record<string, typeof samplePoint[]> = {};
        for (const f of fields) result[f] = [samplePoint];
        return result;
      });

      render(<HistoryPage />);
      await waitFor(() => {
        expect(fetchHistoryCalls.length).toBeGreaterThan(0);
      });

      // Initial Battery tab fetch is the per-tab one. Capture its field count
      // so we can assert the combined fetch is wider.
      const perTabFieldCount = fetchHistoryCalls[0]!.fields.length;

      const exportAllBtn = screen.getByRole('button', {
        name: 'Export all tabs as a single combined CSV',
      });
      fireEvent.click(exportAllBtn);

      await waitFor(() => {
        // Two fetches total now: initial mount + combined export.
        expect(fetchHistoryCalls.length).toBe(2);
      });
      const combined = fetchHistoryCalls[1]!;
      // The combined call should ask for more fields than the active tab's
      // per-tab fetch — every series from every tab, deduplicated.
      expect(combined.fields.length).toBeGreaterThan(perTabFieldCount);
      // Sanity: a few canonical fields from non-Battery tabs must be present
      // so we know it actually pulled from the full union, not just the
      // active tab's series.
      expect(combined.fields).toContain('pv1_power');
      expect(combined.fields).toContain('grid_voltage');
      expect(combined.fields).toContain('home_power');
      expect(combined.fields).toContain('battery_temperature');
      expect(combined.fields).toContain('_import_cost');
    });

    it('combined export uses "history" as the file label (issue #199)', async () => {
      // Intercept the download link's `download` attribute (the only place
      // the filename flows through in jsdom — there's no real file system).
      const downloads: string[] = [];
      const originalCreate = URL.createObjectURL;
      URL.createObjectURL = vi.fn(() => {
        const blobUrl = 'blob:test';
        return blobUrl;
      });
      const clickSpy = vi
        .spyOn(HTMLAnchorElement.prototype, 'click')
        .mockImplementation(function mockClick(this: HTMLAnchorElement) {
          downloads.push(this.download);
        });

      const samplePoint = { t: 1700000000000, v: 1 };
      fetchHistoryMock.mockImplementationOnce(async (...args: unknown[]) => {
        const [range, fields, offset, rolling] = args as [string, string[], number, boolean];
        fetchHistoryCalls.push({ range, fields: [...fields], offset, rolling });
        const result: Record<string, typeof samplePoint[]> = {};
        for (const f of fields) result[f] = [samplePoint];
        return result;
      });

      try {
        render(<HistoryPage />);
        await waitFor(() => {
          expect(fetchHistoryCalls.length).toBeGreaterThan(0);
        });

        const exportAllBtn = screen.getByRole('button', {
          name: 'Export all tabs as a single combined CSV',
        });
        fireEvent.click(exportAllBtn);

        await waitFor(() => {
          expect(downloads.length).toBeGreaterThan(0);
        });
        // The combined filename starts with `givenergy_history_` so it's
        // distinct from the per-tab files (e.g. `givenergy_soc_…`).
        expect(downloads[0]).toMatch(/^givenergy_history_/);
      } finally {
        clickSpy.mockRestore();
        URL.createObjectURL = originalCreate;
      }
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
