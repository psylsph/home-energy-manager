import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, act } from '@testing-library/react';
import type { ReactNode } from 'react';
import type { BatteryHealthDay, BatteryHealthTrend } from '../../src/lib/types';

const apiGet = vi.fn();
vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGet(...args),
  fetchBatteryHealth: async (days: number) => {
    const res = await apiGet(`/api/battery-health?days=${days}`);
    return { days: res.data.days, trend: res.data.trend ?? [] };
  },
}));

// Recharts needs layout the test DOM doesn't have; the panel's own output is
// what's under test, so draw each chart as its title and series count.
vi.mock('recharts', () => {
  return {
    ResponsiveContainer: ({ children }: { children: ReactNode }) => <div>{children}</div>,
    LineChart: ({ data, margin, children }: { data: unknown[]; margin?: { right?: number }; children: ReactNode }) => (
      <div data-testid="line-chart" data-points={data.length} data-margin-right={margin?.right}>
        {children}
      </div>
    ),
    Line: ({ dataKey }: { dataKey: string }) => <span data-testid="line">{dataKey}</span>,
    CartesianGrid: (props: Record<string, unknown>) => (
      <i
        data-testid="grid"
        data-dash={String(props.strokeDasharray)}
        data-vertical={String(props.vertical)}
        data-horizontal={String(props.horizontal)}
      />
    ),
    XAxis: (props: { ticks?: number[]; interval?: unknown }) => (
      <i data-testid="x-axis" data-ticks={JSON.stringify(props.ticks ?? null)} data-interval={String(props.interval)} />
    ),
    YAxis: () => null,
    Tooltip: () => null,
    Legend: () => null,
  };
});

import { useInverterStore } from '../../src/store/useInverterStore';
import { getHistoryChartGridProps } from '../../src/lib/historyRangeConfig';
import BatteryHealthPanel from '../../src/components/BatteryHealthPanel';
import { dayToMs } from '../../src/lib/batteryHealth';

function day(overrides: Partial<BatteryHealthDay> = {}): BatteryHealthDay {
  return {
    day: '2026-10-01',
    module_index: 0,
    min_cell_v: 3.3,
    max_cell_v: 3.32,
    max_spread_mv: 40,
    full_spread_mv: 25,
    cycles: 100,
    max_temp_c: 24,
    capacity_ah: 95,
    design_capacity_ah: 100,
    bms_firmware: 3015,
    samples: 10,
    soc_jumps: 0,
    soc_stuck: 0,
    ...overrides,
  };
}

function serve(days: BatteryHealthDay[], trend: BatteryHealthTrend[] = []) {
  apiGet.mockImplementation(async () => ({ ok: true, data: { days, trend } }));
}

describe('BatteryHealthPanel', () => {
  beforeEach(() => {
    apiGet.mockReset();
  });
  afterEach(cleanup);

  it('asks for six months by default', async () => {
    serve([day()]);
    render(<BatteryHealthPanel />);
    await screen.findByText('Charge cycles');
    expect(apiGet).toHaveBeenCalledWith('/api/battery-health?days=180');
  });

  it('summarises each module from its latest day', async () => {
    serve([
      day({ day: '2026-10-01', full_spread_mv: 10, cycles: 100 }),
      day({ day: '2026-10-02', full_spread_mv: 25, cycles: 101 }),
      day({ module_index: 1, day: '2026-10-02', full_spread_mv: 60, cycles: 7, capacity_ah: 48, design_capacity_ah: 50 }),
    ]);
    render(<BatteryHealthPanel />);
    await screen.findByText('#2');
    expect(screen.getByText('25 mV')).toBeTruthy();
    expect(screen.getByText('60 mV')).toBeTruthy();
    expect(screen.getByText('101')).toBeTruthy();
    expect(screen.getByText('96%')).toBeTruthy();
  });

  it('draws one series per module in each chart', async () => {
    serve([
      day(),
      day({ module_index: 1 }),
    ]);
    render(<BatteryHealthPanel />);
    await screen.findByText('Charge cycles');
    // spread, capacity, cycles, temperature × two modules
    expect(screen.getAllByTestId('line-chart')).toHaveLength(4);
    expect(screen.getAllByTestId('line')).toHaveLength(8);
  });

  it('labels capacity as State of Health, as the module details do', async () => {
    serve([day({ capacity_ah: 90, design_capacity_ah: 100 })]);
    render(<BatteryHealthPanel />);
    await screen.findByText('Charge cycles');
    // Summary heading and chart title.
    expect(screen.getAllByText('State of Health')).toHaveLength(2);
    expect(screen.queryByText('Capacity')).toBeNull();
    expect(screen.getByText('90%')).toBeTruthy();
  });

  it('draws every chart with the shared grid, horizontal lines included', async () => {
    serve([day()]);
    useInverterStore.setState({ gridLineWeight: 'subtle' });
    render(<BatteryHealthPanel />);
    await screen.findByText('Charge cycles');
    const grids = screen.getAllByTestId('grid');
    expect(grids).toHaveLength(4);
    for (const grid of grids) {
      expect(grid.getAttribute('data-dash')).toBe(getHistoryChartGridProps('subtle').strokeDasharray);
      // Neither direction is switched off.
      expect(grid.getAttribute('data-vertical')).not.toBe('false');
      expect(grid.getAttribute('data-horizontal')).not.toBe('false');
    }
    useInverterStore.setState({ gridLineWeight: 'standard' });
  });

  it('labels every chart on whole days and never lets Recharts drop a label', async () => {
    serve([day({ day: '2026-10-01' }), day({ day: '2026-10-03' })]);
    render(<BatteryHealthPanel />);
    await screen.findByText('Charge cycles');
    const axes = screen.getAllByTestId('x-axis');
    expect(axes).toHaveLength(4);
    const expected = JSON.stringify(['2026-10-01', '2026-10-02', '2026-10-03'].map(dayToMs));
    for (const axis of axes) {
      expect(axis.getAttribute('data-ticks')).toBe(expected);
      expect(axis.getAttribute('data-interval')).toBe('0');
    }
  });

  it('leaves room for the newest day label, which sits on the right edge', async () => {
    serve([day({ day: '2026-10-01' }), day({ day: '2026-10-30' })]);
    render(<BatteryHealthPanel />);
    await screen.findByText('Charge cycles');
    // Half of a centred month label ("Oct 2026", 11px bold) is about 27px.
    for (const chart of screen.getAllByTestId('line-chart')) {
      expect(Number(chart.getAttribute('data-margin-right'))).toBeGreaterThanOrEqual(32);
    }
  });

  it('says which spread the summary shows', async () => {
    serve([day({ full_spread_mv: 25, max_spread_mv: 40 })]);
    const first = render(<BatteryHealthPanel />);
    expect(await screen.findByText('Cell spread · near full')).toBeTruthy();
    first.unmount();

    serve([day({ full_spread_mv: null, max_spread_mv: 40 })]);
    render(<BatteryHealthPanel />);
    expect(await screen.findByText("Cell spread · day's worst")).toBeTruthy();
  });

  it('switches the spread chart between near-full and the days worst', async () => {
    serve([day({ full_spread_mv: 25, max_spread_mv: 40 })]);
    render(<BatteryHealthPanel />);
    await screen.findByText('Charge cycles');
    const nearFull = screen.getByRole('button', { name: 'Near full' });
    const worst = screen.getByRole('button', { name: "Day's worst" });
    expect(nearFull.getAttribute('aria-pressed')).toBe('true');
    fireEvent.click(worst);
    expect(worst.getAttribute('aria-pressed')).toBe('true');
    expect(nearFull.getAttribute('aria-pressed')).toBe('false');
  });

  it('falls back to the days worst when no module has been nearly full', async () => {
    serve([day({ full_spread_mv: null, max_spread_mv: 40 })]);
    render(<BatteryHealthPanel />);
    await screen.findByText('Charge cycles');
    expect(screen.queryByRole('button', { name: 'Near full' })).toBeNull();
    expect(screen.getByRole('button', { name: "Day's worst" }).getAttribute('aria-pressed')).toBe('true');
    // The summary falls back too.
    expect(screen.getByText('40 mV')).toBeTruthy();
  });

  it('omits charts for metrics the batteries do not report', async () => {
    // HV stacks have no cycle count or capacity.
    serve([day({ cycles: null, capacity_ah: null, design_capacity_ah: null })]);
    render(<BatteryHealthPanel />);
    await screen.findByText('Hottest cell temperature (daily max)');
    expect(screen.queryByText('Charge cycles')).toBeNull();
    // Only the summary heading remains; the chart is gone.
    expect(screen.getAllByText('State of Health')).toHaveLength(1);
    expect(screen.getAllByText('—').length).toBeGreaterThan(0);
  });

  it('refetches when the range changes', async () => {
    serve([day()]);
    render(<BatteryHealthPanel />);
    await screen.findByText('Charge cycles');
    fireEvent.click(screen.getByRole('button', { name: '1y' }));
    await waitFor(() => expect(apiGet).toHaveBeenCalledWith('/api/battery-health?days=365'));
    expect(screen.getByRole('button', { name: '1y' }).getAttribute('aria-pressed')).toBe('true');
    await screen.findByText('Charge cycles');
  });

  it("refreshes while open, so today's row keeps up", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      serve([day({ full_spread_mv: 25 })]);
      render(<BatteryHealthPanel />);
      expect(await screen.findByText('25 mV')).toBeTruthy();

      serve([day({ full_spread_mv: 41 })]);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(5 * 60_000);
      });
      expect(screen.getByText('41 mV')).toBeTruthy();
    } finally {
      vi.useRealTimers();
    }
  });

  it('keeps the last good data when a refresh fails', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      serve([day({ full_spread_mv: 25 })]);
      render(<BatteryHealthPanel />);
      expect(await screen.findByText('25 mV')).toBeTruthy();

      apiGet.mockRejectedValue(new Error('offline'));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(5 * 60_000);
      });
      expect(screen.getByText('25 mV')).toBeTruthy();
      expect(screen.queryByText('Failed to load battery health')).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("shows each module's spread trend", async () => {
    serve(
      [day(), day({ module_index: 1 })],
      [
        { module_index: 0, status: 'ok', recent_mv: 25, baseline_mv: 24, recent_days: 7, recent_min_days: 4 },
        { module_index: 1, status: 'widening', recent_mv: 72, baseline_mv: 30, recent_days: 7, recent_min_days: 4 },
      ],
    );
    render(<BatteryHealthPanel />);
    expect(await screen.findByText('Widening: 30 → 72 mV near full')).toBeTruthy();
    expect(screen.getByText('Steady: 25 mV near full')).toBeTruthy();
  });

  it('lists days the SOC jumped or stuck', async () => {
    serve([
      day({ day: '2026-10-01', soc_jumps: 1 }),
      day({ day: '2026-10-02', module_index: 1, soc_jumps: 2, soc_stuck: 1 }),
    ]);
    render(<BatteryHealthPanel />);
    expect(await screen.findByText('SOC warnings')).toBeTruthy();
    const items = screen.getAllByRole('listitem').map((li) => li.textContent);
    expect(items).toHaveLength(2);
    expect(items[0]).toContain('Module #2');
    expect(items[0]).toContain('SOC jumped twice and stuck once');
    expect(items[1]).toContain('Module #1');
    expect(items[1]).toContain('SOC jumped once');
    // HEM can run the cycle itself; the note points there and still says how by hand.
    expect(screen.getByText(/Battery calibration cycle on the Control page/)).toBeTruthy();
    expect(screen.getByText(/set the battery reserve to 4%/)).toBeTruthy();
  });

  it('shows no SOC warnings when there were none', async () => {
    serve([day()]);
    render(<BatteryHealthPanel />);
    await screen.findByText('Charge cycles');
    expect(screen.queryByText('SOC warnings')).toBeNull();
  });

  it('explains an empty history', async () => {
    serve([]);
    render(<BatteryHealthPanel />);
    await screen.findByText('No battery health recorded yet.');
    expect(screen.queryByTestId('line-chart')).toBeNull();
  });

  it('reports a failed load', async () => {
    apiGet.mockRejectedValue(new Error('boom'));
    render(<BatteryHealthPanel />);
    await screen.findByText('Failed to load battery health');
    expect(screen.queryByText('No battery health recorded yet.')).toBeNull();
  });

  it('ignores a slow answer for a range that is no longer selected', async () => {
    let releaseSlow: (v: unknown) => void = () => {};
    apiGet.mockImplementationOnce(
      () => new Promise((resolve) => { releaseSlow = resolve; }),
    );
    apiGet.mockImplementation(async () => ({ ok: true, data: { days: [day({ cycles: 222 })] } }));
    render(<BatteryHealthPanel />);
    fireEvent.click(screen.getByRole('button', { name: '1y' }));
    await screen.findByText('222');
    // The stale 6-month answer arrives late and must not replace it.
    releaseSlow({ ok: true, data: { days: [day({ cycles: 111 })] } });
    await Promise.resolve();
    expect(screen.queryByText('111')).toBeNull();
    expect(screen.getByText('222')).toBeTruthy();
  });
});
