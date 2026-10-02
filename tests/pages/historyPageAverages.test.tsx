import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// HistoryPage average-line coverage (issue #345).
//
// The chart library is mocked so we can capture the props of every
// <ReferenceLine> the page would render — asserting the value, dash and
// label without needing a real SVG layout pass. The store is used for real
// (like the other HistoryPage tests) so we exercise the actual persisted
// setting wiring.
// ---------------------------------------------------------------------------

const { referenceLines } = vi.hoisted(() => ({
  referenceLines: [] as Array<Record<string, unknown>>,
}));

vi.mock('recharts', () => ({
  ResponsiveContainer: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  AreaChart: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  Area: () => null,
  CartesianGrid: () => null,
  Tooltip: () => null,
  XAxis: () => null,
  YAxis: () => null,
  ReferenceLine: (props: Record<string, unknown>) => {
    referenceLines.push(props);
    return null;
  },
}));

const fetchHistoryMock = vi.fn(async (...args: unknown[]) => {
  const fields = args[1] as string[];
  const result: Record<string, { t: number; v: number }[]> = {};
  // Only SOC has readings on the default Battery tab, so it is the only
  // series with an average; the other fields stay empty (no line drawn).
  if (fields.includes('soc')) {
    result.soc = [
      { t: 1_700_000_000_000, v: 40 },
      { t: 1_700_000_003_600_000, v: 60 },
    ];
  }
  return result;
});

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async () => ({ ok: true, data: {} })),
  fetchHistory: (...args: unknown[]) => fetchHistoryMock(...args),
  fetchHistorySummary: async () => ({
    solar_generated_kwh: 0,
    battery_charged_kwh: 0,
    battery_discharged_kwh: 0,
    grid_imported_kwh: 0,
    grid_exported_kwh: 0,
    home_consumed_kwh: 0,
    import_cost_gbp: 0,
    export_income_gbp: 0,
    net_cost_gbp: 0,
  }),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  isTauri: false,
}));

globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

import HistoryPage, { HistoryTooltip } from '../../src/pages/HistoryPage';
import { useInverterStore } from '../../src/store/useInverterStore';

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

describe('<HistoryPage/> — chart average lines (issue #345)', () => {
  beforeEach(() => {
    silenceConsoleError();
    referenceLines.length = 0;
    fetchHistoryMock.mockClear();
    localStorage.removeItem('showHistoryAverages');
    useInverterStore.setState({
      snapshot: null,
      chartRange: '24h',
      showHistoryAverages: false,
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.removeItem('showHistoryAverages');
  });

  it('draws no average line while the setting is off', async () => {
    render(<HistoryPage />);
    await waitFor(() => {
      expect(fetchHistoryMock).toHaveBeenCalled();
    });
    // Let the resolved fetch flush through state.
    await screen.findByText('SOC %');
    expect(referenceLines).toHaveLength(0);
  });

  it('draws a horizontal average line per series when enabled', async () => {
    useInverterStore.getState().setShowHistoryAverages(true);
    render(<HistoryPage />);

    await waitFor(() => {
      expect(referenceLines).toHaveLength(1);
    });
    // Mean of 40 and 60.
    expect(referenceLines[0].y).toBe(50);
    expect(referenceLines[0].strokeDasharray).toBe('2 4');
    // The value lives in the tooltip now, not as a static line label.
    expect(referenceLines[0].label).toBeUndefined();
  });

  it('persists the toggle to localStorage', () => {
    useInverterStore.getState().setShowHistoryAverages(true);
    expect(localStorage.getItem('showHistoryAverages')).toBe('true');
    useInverterStore.getState().setShowHistoryAverages(false);
    expect(localStorage.getItem('showHistoryAverages')).toBe('false');
  });
});

describe('<HistoryTooltip/> — average read-out (issue #345)', () => {
  it('appends the window average to each series value', () => {
    const { container } = render(
      <HistoryTooltip
        active
        label={1_700_000_000_000}
        unit="%"
        payload={[{ value: 40, color: '#6366F1', dataKey: 'soc' }]}
        seriesMeta={{ soc: { average: 50, muted: false } }}
      />,
    );
    expect(container.textContent).toContain('40 %');
    expect(container.textContent).toContain('(avg 50 %)');
  });

  it('omits the average when the series is muted', () => {
    const { container } = render(
      <HistoryTooltip
        active
        label={1_700_000_000_000}
        unit="W"
        payload={[{ value: 1200, color: '#22C55E', dataKey: 'charge' }]}
        seriesMeta={{ charge: { average: 900, muted: true } }}
      />,
    );
    expect(container.textContent).toContain('1200 W');
    expect(container.textContent).not.toContain('avg');
  });

  it('omits the average when averages are off (null)', () => {
    const { container } = render(
      <HistoryTooltip
        active
        label={1_700_000_000_000}
        unit="£"
        payload={[{ value: 1.5, color: '#EF4444', dataKey: 'cost' }]}
        seriesMeta={{ cost: { average: null, muted: false } }}
      />,
    );
    expect(container.textContent).toContain('£1.50');
    expect(container.textContent).not.toContain('avg');
  });

  it('renders nothing when inactive', () => {
    const { container } = render(
      <HistoryTooltip
        active={false}
        unit="W"
        payload={[{ value: 1, dataKey: 'x' }]}
        seriesMeta={{}}
      />,
    );
    expect(container.firstChild).toBeNull();
  });
});
