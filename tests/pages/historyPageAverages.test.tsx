import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// HistoryPage average-line coverage (issue #345).
//
// The chart library is mocked so we can capture the props of every
// <ReferenceLine> the page would render — asserting the value, dash and
// label without needing a real SVG layout pass. The store is used for real
// (like the other HistoryPage tests) so we exercise the actual persisted
// setting wiring.
// ---------------------------------------------------------------------------

const { referenceLines, areaChartData } = vi.hoisted(() => ({
  referenceLines: [] as Array<Record<string, unknown>>,
  // Every <AreaChart> row set the page renders, so tests can wait for the
  // fetched data to actually flush into a chart before asserting on the
  // average <ReferenceLine>s.
  areaChartData: [] as Array<Array<Record<string, unknown>>>,
}));

vi.mock('recharts', () => ({
  ResponsiveContainer: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  AreaChart: (props: { children?: React.ReactNode; data?: Array<Record<string, unknown>> }) => {
    if (props.data) areaChartData.push(props.data);
    return <div>{props.children}</div>;
  },
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

// Data for both an instantaneous series (SOC, mean 50 %) and cumulative ones
// (daily kWh counters and the server-integrated cost/income series, all of
// which ramp up across the window). The cumulative series' means (2.5, 4.5,
// £1.50, £0.50) must never reach an average line or a tooltip read-out.
const readings: Record<string, Array<{ t: number; v: number }>> = {
  soc: [
    { t: 1_700_000_000_000, v: 40 },
    { t: 1_700_000_003_600_000, v: 60 },
  ],
  today_charge_kwh: [
    { t: 1_700_000_000_000, v: 1 },
    { t: 1_700_000_003_600_000, v: 4 },
  ],
  today_discharge_kwh: [
    { t: 1_700_000_000_000, v: 2 },
    { t: 1_700_000_003_600_000, v: 7 },
  ],
  _import_cost: [
    { t: 1_700_000_000_000, v: 0.5 },
    { t: 1_700_000_003_600_000, v: 2.5 },
  ],
  _export_income: [
    { t: 1_700_000_000_000, v: 0.2 },
    { t: 1_700_000_003_600_000, v: 0.8 },
  ],
};

const fetchHistoryMock = vi.fn(async (...args: unknown[]) => {
  const fields = args[1] as string[];
  const result: Record<string, { t: number; v: number }[]> = {};
  for (const field of fields) {
    if (readings[field]) result[field] = readings[field];
  }
  return result;
});

/** Wait until a chart row set carries a real value for `field`. */
async function waitForChartData(field: string) {
  await waitFor(() => {
    expect(
      areaChartData.some((rows) => rows.some((row) => row[field] != null)),
      `no chart data for ${field}`,
    ).toBe(true);
  });
}

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
    areaChartData.length = 0;
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

  it('draws a horizontal average line per instantaneous series when enabled', async () => {
    useInverterStore.getState().setShowHistoryAverages(true);
    render(<HistoryPage />);

    // The Energy (kWh) chart's cumulative counters have data by now, so their
    // means (2.5 / 4.5 kWh) would have produced two more lines before the
    // cumulative series were excluded (#345 follow-up).
    await waitForChartData('today_charge_kwh');
    await waitFor(() => {
      expect(referenceLines).toHaveLength(1);
    });
    // Mean of 40 and 60.
    expect(referenceLines[0].y).toBe(50);
    expect(referenceLines[0].strokeDasharray).toBe('2 4');
    // The value lives in the tooltip now, not as a static line label.
    expect(referenceLines[0].label).toBeUndefined();
  });

  it('draws no average line on the cumulative cost chart', async () => {
    useInverterStore.getState().setShowHistoryAverages(true);
    render(<HistoryPage />);
    // Let the Battery tab settle first, then drop its lines so this test only
    // sees what the Cost tab draws.
    await waitForChartData('today_charge_kwh');
    fireEvent.click(screen.getByRole('button', { name: 'Cost', exact: true }));
    referenceLines.length = 0;

    // Import Cost and Export Income are running totals integrated by the
    // server, so their window means (£1.50 / £0.50) mean nothing to the user.
    await waitForChartData('_import_cost');
    expect(referenceLines).toHaveLength(0);
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
