import React from 'react';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, render, screen, cleanup, fireEvent, waitFor, within } from '@testing-library/react';
import { useInverterStore } from '../../src/store/useInverterStore';

// ---------------------------------------------------------------------------
// ForecastPage "Show past" view (issue #361): earlier planner decisions and
// what really happened are drawn on the Battery and Solar charts, with the
// reasons listed underneath so a past night can be judged.
// ---------------------------------------------------------------------------

const G = 1_700_000_000; // the payload's generated_at, taken as "now"
const HOUR = 3600;
const hourOf = (ts: number) => Math.floor(ts / HOUR) * HOUR;
const START = G - 12 * HOUR; // when the stored decision was made
const HS = hourOf(START);

const { forwardPayload, planResponse, pastDecision } = vi.hoisted(() => {
  const forwardPayload = () => ({
    generated_at: 1_700_000_000,
    forecast_fetched_at: 1_699_996_400,
    forecast_age_seconds: 3_600,
    status: [],
    performance_ratio: 0.8,
    performance_ratio_days: 12,
    solar: [{ timestamp: 1_700_003_600, kwh: 2.0, band_low: 1.6, band_high: 2.4 }],
    solar_today_remaining_kwh: 10,
    solar_tomorrow_kwh: 18.4,
    consumption_weekday: [],
    consumption_weekend: [],
    consumption_days_observed: 14,
    consumption_sufficient: true,
    consumption_tomorrow_kwh: 11.2,
    battery: {
      capacity_kwh: 9.5,
      start_soc_pct: 62,
      reserve_soc_pct: 15,
      hours: [[1_700_003_600, 70]],
      end_soc_pct: 70,
      with_current_schedule: null,
    },
    import_tomorrow_kwh: 1.1,
    export_tomorrow_kwh: 7.2,
  });
  const planResponse = () => ({
    recommendation: { kind: 'no_plan', reason: 'no battery projection available' },
    apply: null,
  });
  const pastDecision = (start: number, hs: number) => ({
    recorded_at: start,
    kind: 'before_window',
    strategy: 'min_soc',
    plan: {
      recommendation: {
        kind: 'no_charge_needed',
        min_soc_pct: 20,
        observed_min_soc_pct: 55,
        current_soc_pct: 60,
        rationale: 'Sunny day — the battery fills from solar.',
      },
      apply: null,
    },
    forecast: {
      generated_at: start,
      forecast_fetched_at: start,
      forecast_age_seconds: 0,
      status: [],
      performance_ratio: 0.8,
      performance_ratio_days: 10,
      solar: [
        { timestamp: hs, kwh: 2.0, band_low: 1.6, band_high: 2.4 },
        { timestamp: hs + 3600, kwh: 3.0, band_low: 2.4, band_high: 3.6 },
      ],
      solar_today_remaining_kwh: 0,
      solar_tomorrow_kwh: 0,
      consumption_weekday: [],
      consumption_weekend: [],
      consumption_days_observed: 0,
      consumption_sufficient: false,
      consumption_tomorrow_kwh: 0,
      battery: {
        capacity_kwh: 9.5,
        start_soc_pct: 60,
        reserve_soc_pct: 4,
        hours: [
          [start, 58],
          [start + 3600, 55],
        ],
        end_soc_pct: 55,
        with_current_schedule: null,
      },
      import_tomorrow_kwh: 0,
      export_tomorrow_kwh: 0,
    },
  });
  return { forwardPayload, planResponse, pastDecision };
});

// Counts how often the past series is stitched, so a re-render that changes
// nothing about the past can be shown not to rebuild it.
const { stitchSpy } = vi.hoisted(() => ({ stitchSpy: vi.fn() }));
vi.mock('../../src/lib/planHistory', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/lib/planHistory')>();
  return {
    ...actual,
    stitchPlannedBattery: (...args: Parameters<typeof actual.stitchPlannedBattery>) => {
      stitchSpy();
      return actual.stitchPlannedBattery(...args);
    },
  };
});

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(),
  apiPost: vi.fn(async () => ({ ok: true })),
  getApiBase: () => 'http://127.0.0.1:7337',
}));

vi.mock('recharts', () => {
  const Line = ({ name, dataKey, yAxisId, xAxisId }: { name?: string; dataKey?: string; yAxisId?: string | number; xAxisId?: string | number }) => (
    <div data-testid={`line-${dataKey}`} data-yaxis-id={String(yAxisId ?? 0)} data-xaxis-id={String(xAxisId ?? 0)}>{name}</div>
  );
  return {
  ResponsiveContainer: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  // The real AreaChart silently drops <Line> children (only a ComposedChart
  // draws a mix), so the mock does too: a Line put in one must not pass.
  AreaChart: ({ children }: { children: React.ReactNode }) => (
    <div>
      {React.Children.toArray(children).filter(
        (child) => !(React.isValidElement(child) && child.type === Line),
      )}
    </div>
  ),
  ComposedChart: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  LineChart: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  Area: ({ name, dataKey, yAxisId, xAxisId }: { name?: string; dataKey?: string; yAxisId?: string | number; xAxisId?: string | number }) => (
    <div data-testid={`area-${dataKey}`} data-yaxis-id={String(yAxisId ?? 0)} data-xaxis-id={String(xAxisId ?? 0)}>{name}</div>
  ),
  Line,
  Customized: () => <div />,
  XAxis: ({ domain, xAxisId }: { domain?: [number, number]; xAxisId?: string | number }) => (
    <div
      data-testid="x-axis"
      data-domain={JSON.stringify(domain ?? null)}
      data-xaxis-id={String(xAxisId ?? 0)}
    />
  ),
  YAxis: ({ unit, yAxisId }: { unit?: string; yAxisId?: string | number }) => (
    <div data-testid="y-axis" data-unit={unit ?? ''} data-yaxis-id={String(yAxisId ?? 0)} />
  ),
  Tooltip: () => <div />,
  CartesianGrid: () => <div />,
  ReferenceLine: ({ x, label, yAxisId, xAxisId }: { x?: number; label?: { value?: string }; yAxisId?: string | number; xAxisId?: string | number }) => (
    <div data-testid="reference-line" data-x={x} data-label={label?.value} data-yaxis-id={String(yAxisId ?? 0)} data-xaxis-id={String(xAxisId ?? 0)} />
  ),
  Legend: () => <div />,
  };
});

globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

import ForecastPage from '../../src/pages/ForecastPage';
import { apiGet } from '../../src/lib/api';

const apiGetMock = vi.mocked(apiGet);

const historyData = {
  soc: [
    { t: START * 1000, v: 58 },
    { t: (START + HOUR) * 1000, v: 60 },
    { t: (START + 2 * HOUR) * 1000, v: 50 },
  ],
  solar_power: [
    { t: HS * 1000, v: 1000 },
    { t: (HS + HOUR) * 1000, v: 3000 },
  ],
};

type Routes = {
  planHistory?: () => Promise<unknown> | unknown;
  history?: () => Promise<unknown> | unknown;
};

function route(routes: Routes = {}) {
  apiGetMock.mockImplementation(async (path: string) => {
    if (path === '/api/forecast') return { ok: true, data: forward() };
    if (path === '/api/forecast/plan') return { ok: true, data: planResponse() };
    if (path === '/api/settings') return { ok: true, data: {} };
    if (path.startsWith('/api/forecast/plan-history')) {
      return (
        (await routes.planHistory?.()) ?? {
          ok: true,
          data: { snapshots: [pastDecision(START, HS)] },
        }
      );
    }
    if (path.startsWith('/api/history?')) {
      return (await routes.history?.()) ?? { ok: true, data: historyData };
    }
    return { ok: true, data: {} };
  });
}

// Each test starts from the stock forward payload and may reshape it.
let forward: () => Record<string, unknown> = () => forwardPayload();

const calledWith = (prefix: string) =>
  apiGetMock.mock.calls.map(([path]) => String(path)).filter((p) => p.startsWith(prefix));

async function choose(label: string) {
  const control = await screen.findByTestId('forecast-past-range');
  fireEvent.click(within(control).getByRole('button', { name: label }));
}

describe('ForecastPage — Show past (issue #361)', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    forward = () => forwardPayload();
    useInverterStore.setState({ gridLineWeight: 'standard', snapshot: null });
    route();
  });

  afterEach(() => {
    cleanup();
  });

  it('starts with the past view off and fetches no history', async () => {
    render(<ForecastPage />);
    const control = await screen.findByTestId('forecast-past-range');
    expect(
      within(control).getByRole('button', { name: 'Off' }).getAttribute('aria-pressed'),
    ).toBe('true');
    expect(screen.queryByTestId('forecast-past-decisions')).toBeNull();
    expect(calledWith('/api/forecast/plan-history')).toHaveLength(0);
    expect(calledWith('/api/history?')).toHaveLength(0);
    expect(screen.queryByTestId('line-pastPlanned')).toBeNull();
  });

  it('fetches the decisions and the measured history for the chosen range', async () => {
    render(<ForecastPage />);
    await choose('24 hours');
    await waitFor(() => expect(calledWith('/api/history?')).toHaveLength(1));
    const decisions = new URLSearchParams(calledWith('/api/forecast/plan-history')[0].split('?')[1]);
    expect(decisions.get('from')).toBe(String(G - 24 * HOUR + 1));
    expect(decisions.get('to')).toBe(String(G + 1));
    const history = new URLSearchParams(calledWith('/api/history?')[0].split('?')[1]);
    expect(history.get('fields')).toBe('soc,solar_power');
    expect(history.get('start_ms')).toBe(String((G - 24 * HOUR) * 1000));
  });

  it('offers 30 and 90 day views and reads the full window for the longest one', async () => {
    render(<ForecastPage />);
    const control = await screen.findByTestId('forecast-past-range');
    expect(within(control).getByRole('button', { name: '30 days' })).toBeTruthy();
    await choose('90 days');
    await waitFor(() => expect(calledWith('/api/history?')).toHaveLength(1));
    const decisions = new URLSearchParams(calledWith('/api/forecast/plan-history')[0].split('?')[1]);
    expect(decisions.get('from')).toBe(String(G - 90 * 24 * HOUR + 1));
    const history = new URLSearchParams(calledWith('/api/history?')[0].split('?')[1]);
    expect(history.get('range')).toBe('7d');
    expect(history.get('start_ms')).toBe(String((G - 90 * 24 * HOUR) * 1000));
    await screen.findByTestId('line-pastPlanned');
    const domains = screen
      .getAllByTestId('x-axis')
      .map((axis) => JSON.parse(axis.getAttribute('data-domain') ?? 'null') as [number, number]);
    expect(domains.some(([start]) => start === G - 90 * 24 * HOUR)).toBe(true);
  });

  it('draws the past plan and the actual SOC on the battery chart, in their own colours', async () => {
    render(<ForecastPage />);
    await choose('24 hours');
    expect(await screen.findByTestId('line-pastPlanned')).toBeTruthy();
    expect(screen.getByTestId('line-actualSoc')).toBeTruthy();
    const legend = screen.getByTestId('forecast-line-legend');
    expect(within(legend).getByRole('button', { name: /Planned \(past\)/ })).toBeTruthy();
    expect(within(legend).getByRole('button', { name: /Actual/ })).toBeTruthy();
    // The forward lines are still there.
    expect(screen.getByTestId('line-soc')).toBeTruthy();
  });

  it('does not rebuild the past series for an unrelated store update', async () => {
    render(<ForecastPage />);
    await choose('24 hours');
    await screen.findByTestId('line-pastPlanned');
    const before = stitchSpy.mock.calls.length;
    expect(before).toBeGreaterThan(0);
    act(() => {
      useInverterStore.setState({ gridLineWeight: 'subtle' });
    });
    act(() => {
      useInverterStore.setState({ gridLineWeight: 'standard' });
    });
    expect(stitchSpy.mock.calls.length).toBe(before);
  });

  it('lets the past lines be hidden from the legend like the others', async () => {
    render(<ForecastPage />);
    await choose('24 hours');
    await screen.findByTestId('line-pastPlanned');
    const legend = screen.getByTestId('forecast-line-legend');
    fireEvent.click(within(legend).getByRole('button', { name: /Planned \(past\)/ }));
    expect(screen.queryByTestId('line-pastPlanned')).toBeNull();
    expect(screen.getByTestId('line-actualSoc')).toBeTruthy();
  });

  it('stretches the axis back by the range and marks where the past ends', async () => {
    render(<ForecastPage />);
    await choose('3 days');
    await screen.findByTestId('line-pastPlanned');
    const domains = screen
      .getAllByTestId('x-axis')
      .map((axis) => JSON.parse(axis.getAttribute('data-domain') ?? 'null') as [number, number]);
    expect(domains.some(([start]) => start === G - 3 * 24 * HOUR)).toBe(true);
    const now = screen
      .getAllByTestId('reference-line')
      .filter((line) => line.getAttribute('data-label') === 'now');
    expect(now.length).toBeGreaterThan(0);
    expect(now[0].getAttribute('data-x')).toBe(String(G));
  });

  it('binds everything on every chart to an axis that chart has, or the page crashes (issue #364)', async () => {
    // Recharts looks each series and reference line up by its yAxisId, which
    // defaults to 0. One whose id matches no YAxis in its chart throws
    // "Invariant failed" while drawing; a past range is what puts the "now"
    // marker on screen, which is how the solar chart crashed in 0.89.0.
    // A home-use profile, so the third chart draws too.
    const profile = Array.from({ length: 24 }, (_, hour) => ({ hour, kwh: 0.5, p25: 0.3, p75: 0.8 }));
    forward = () => ({ ...forwardPayload(), consumption_weekday: profile, consumption_weekend: profile });
    const { container } = render(<ForecastPage />);
    await choose('24 hours');
    await screen.findByTestId('line-pastSolar');
    for (const chart of ['battery', 'solar', 'consumption']) {
      const panel = container.querySelector<HTMLElement>(`#forecast-chart-panel-${chart}`);
      expect(panel, chart).not.toBeNull();
      const bound = within(panel!).queryAllByTestId(/^(line-|area-|reference-line)/);
      expect(bound.length, `${chart} draws something`).toBeGreaterThan(0);
      const yAxisIds = new Set(
        within(panel!).getAllByTestId('y-axis').map((axis) => axis.getAttribute('data-yaxis-id')),
      );
      const xAxisIds = new Set(
        within(panel!).getAllByTestId('x-axis').map((axis) => axis.getAttribute('data-xaxis-id')),
      );
      for (const element of bound) {
        const label = `${chart}: ${element.getAttribute('data-testid')}`;
        expect(yAxisIds.has(element.getAttribute('data-yaxis-id')), `${label} y axis`).toBe(true);
        expect(xAxisIds.has(element.getAttribute('data-xaxis-id')), `${label} x axis`).toBe(true);
      }
    }
    // The solar chart's "now" marker is among them once a past range is shown.
    const solarPanel = container.querySelector<HTMLElement>('#forecast-chart-panel-solar')!;
    expect(
      within(solarPanel)
        .getAllByTestId('reference-line')
        .filter((line) => line.getAttribute('data-label') === 'now'),
    ).toHaveLength(1);
  });

  it('labels the solar chart in kW, because each hourly point is an average power', async () => {
    const { container } = render(<ForecastPage />);
    await choose('24 hours');
    await screen.findByTestId('line-pastSolar');
    const solarPanel = container.querySelector<HTMLElement>('#forecast-chart-panel-solar')!;
    expect(within(solarPanel).getByTestId('y-axis').getAttribute('data-unit')).toBe('kW');
  });

  it('draws the past forecast and the measured generation on the solar chart', async () => {
    render(<ForecastPage />);
    await choose('24 hours');
    expect(await screen.findByTestId('line-pastSolar')).toBeTruthy();
    expect(screen.getByTestId('line-actualSolar')).toBeTruthy();
  });

  it('says how far the plan and the forecast were from reality', async () => {
    render(<ForecastPage />);
    await choose('24 hours');
    const battery = await screen.findByTestId('forecast-past-battery-accuracy');
    // Planned 58 and 55 (the 60 it started from isn't scored) against
    // measured 60 and 50: 3.5 points out on average, finishing 1.5 below.
    expect(battery.textContent).toMatch(/3\.5 points/);
    expect(battery.textContent).toMatch(/1\.5 points below/);
    const solar = screen.getByTestId('forecast-past-solar-accuracy');
    // Forecast 2 + 3 kWh, measured 1 + 3 kWh over two hours.
    expect(solar.textContent).toMatch(/5\.0 kWh/);
    expect(solar.textContent).toMatch(/4\.0 kWh/);
    expect(solar.textContent).toMatch(/2 hours/);
  });

  it('lists each decision with when, why and what it recommended', async () => {
    render(<ForecastPage />);
    await choose('24 hours');
    const list = await screen.findByTestId('forecast-past-decisions');
    const items = within(list).getAllByTestId('forecast-past-decision');
    expect(items).toHaveLength(1);
    expect(items[0].textContent).toContain('Before the cheap window');
    expect(items[0].textContent).toContain('No overnight charge needed');
    expect(items[0].textContent).toContain('Sunny day — the battery fills from solar.');
  });

  it('explains when nothing was recorded in the period', async () => {
    route({ planHistory: () => ({ ok: true, data: { snapshots: [] } }) });
    render(<ForecastPage />);
    await choose('7 days');
    const empty = await screen.findByTestId('forecast-past-empty');
    expect(empty.textContent).toMatch(/No planner decisions/);
    expect(screen.queryByTestId('forecast-past-decisions')).toBeNull();
    expect(screen.queryByTestId('line-pastPlanned')).toBeNull();
  });

  it('keeps the page working and says so when the history will not load', async () => {
    route({
      planHistory: () => {
        throw new Error('history offline');
      },
    });
    render(<ForecastPage />);
    await choose('24 hours');
    const error = await screen.findByTestId('forecast-past-error');
    expect(error.textContent).toMatch(/history offline/);
    // The forward charts are untouched.
    expect(screen.getByTestId('line-soc')).toBeTruthy();
    expect(screen.queryByTestId('forecast-error')).toBeNull();
  });

  it('still shows the plan when only the measured history is unavailable', async () => {
    route({
      history: () => {
        throw new Error('no readings');
      },
    });
    render(<ForecastPage />);
    await choose('24 hours');
    expect(await screen.findByTestId('forecast-past-decisions')).toBeTruthy();
    expect(screen.getByTestId('line-pastPlanned')).toBeTruthy();
    expect(screen.queryByTestId('line-actualSoc')).toBeNull();
    expect(screen.queryByTestId('forecast-past-battery-accuracy')).toBeNull();
  });

  it('switching back to Off removes the past from the page', async () => {
    render(<ForecastPage />);
    await choose('24 hours');
    await screen.findByTestId('line-pastPlanned');
    await choose('Off');
    expect(screen.queryByTestId('line-pastPlanned')).toBeNull();
    expect(screen.queryByTestId('forecast-past-decisions')).toBeNull();
  });

  it('shows the latest choice when an earlier request answers late', async () => {
    let releaseFirst: (value: unknown) => void = () => {};
    const slowFirst = new Promise((resolve) => {
      releaseFirst = resolve;
    });
    let call = 0;
    route({
      planHistory: () => {
        call += 1;
        if (call === 1) return slowFirst;
        return {
          ok: true,
          data: {
            snapshots: [
              {
                ...pastDecision(START, HS),
                plan: {
                  recommendation: {
                    kind: 'no_plan',
                    reason: 'second request',
                  },
                  apply: null,
                },
              },
            ],
          },
        };
      },
    });
    render(<ForecastPage />);
    await choose('24 hours');
    await choose('7 days');
    await waitFor(() =>
      expect(screen.getAllByTestId('forecast-past-decision')[0].textContent).toMatch(
        /second request/,
      ),
    );
    releaseFirst({
      ok: true,
      data: { snapshots: [pastDecision(START - HOUR, HS)] },
    });
    // A macrotask turn drains every pending promise continuation, so the
    // late answer has been delivered (and must have been ignored).
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    const items = screen.getAllByTestId('forecast-past-decision');
    expect(items).toHaveLength(1);
    expect(items[0].textContent).toMatch(/second request/);
  });
  it('follows the forecast forward: a later generation time fetches the past again', async () => {
    render(<ForecastPage />);
    await choose('24 hours');
    await waitFor(() => expect(calledWith('/api/history?')).toHaveLength(1));
    // The forecast refetches (the battery moved) and now reports a time
    // well past the window the first fetch used.
    forward = () => ({ ...forwardPayload(), generated_at: G + 1_800 });
    act(() => {
      useInverterStore.setState({
        snapshot: { soc: 50, max_battery_power_w: 3000 } as never,
      });
    });
    await waitFor(() => expect(calledWith('/api/history?')).toHaveLength(2));
    const second = new URLSearchParams(calledWith('/api/history?')[1].split('?')[1]);
    expect(second.get('end_ms')).toBe(String((G + 1_800) * 1000));
    const nowLine = await waitFor(() => {
      const lines = screen
        .getAllByTestId('reference-line')
        .filter((line) => line.getAttribute('data-label') === 'now');
      expect(lines[0].getAttribute('data-x')).toBe(String(G + 1_800));
      return lines[0];
    });
    expect(nowLine).toBeTruthy();
  });

  it('does not refetch for a forecast that has barely moved', async () => {
    render(<ForecastPage />);
    await choose('24 hours');
    await waitFor(() => expect(calledWith('/api/history?')).toHaveLength(1));
    forward = () => ({ ...forwardPayload(), generated_at: G + 60 });
    act(() => {
      useInverterStore.setState({
        snapshot: { soc: 50, max_battery_power_w: 3000 } as never,
      });
    });
    await waitFor(() => expect(calledWith('/api/forecast')).not.toHaveLength(1));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(calledWith('/api/history?')).toHaveLength(1);
  });

  it('shows the loading state again, not the old result, after Off and back', async () => {
    render(<ForecastPage />);
    await choose('24 hours');
    await screen.findByTestId('line-pastPlanned');
    await choose('Off');
    let release: (value: unknown) => void = () => {};
    const gate = new Promise((resolve) => {
      release = resolve;
    });
    route({ planHistory: () => gate });
    await choose('24 hours');
    expect(screen.getByRole('status').textContent).toMatch(/Loading the past/);
    expect(screen.queryByTestId('line-pastPlanned')).toBeNull();
    release({ ok: true, data: { snapshots: [pastDecision(START, HS)] } });
    expect(await screen.findByTestId('line-pastPlanned')).toBeTruthy();
    expect(screen.queryByRole('status')).toBeNull();
  });

  it('keys two decisions made in the same second apart', async () => {
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {});
    try {
      route({
        planHistory: () => ({
          ok: true,
          data: {
            snapshots: [
              pastDecision(START, HS),
              { ...pastDecision(START, HS), kind: 'auto_apply' },
            ],
          },
        }),
      });
      render(<ForecastPage />);
      await choose('24 hours');
      const items = await screen.findAllByTestId('forecast-past-decision');
      expect(items).toHaveLength(2);
      const duplicateKey = errors.mock.calls.some((call) => String(call[0]).includes('same key'));
      expect(duplicateKey).toBe(false);
    } finally {
      errors.mockRestore();
    }
  });

  it('leaves the decisions off the Home use tab, which has no past view', async () => {
    render(<ForecastPage />);
    await choose('24 hours');
    await screen.findByTestId('forecast-past-decisions');
    fireEvent.click(screen.getByTestId('forecast-chart-tab-consumption'));
    expect(screen.queryByTestId('forecast-past-decisions')).toBeNull();
    expect(screen.queryByTestId('forecast-past-range')).toBeNull();
    fireEvent.click(screen.getByTestId('forecast-chart-tab-solar'));
    expect(screen.getByTestId('forecast-past-decisions')).toBeTruthy();
  });

  it('still draws the past when there is no forward projection to show', async () => {
    forward = () => ({ ...forwardPayload(), battery: null, solar: [] });
    render(<ForecastPage />);
    await choose('7 days');
    expect(await screen.findByTestId('line-pastPlanned')).toBeTruthy();
    expect(screen.getByTestId('line-actualSoc')).toBeTruthy();
    expect(screen.getByTestId('line-pastSolar')).toBeTruthy();
    expect(screen.getByTestId('line-actualSolar')).toBeTruthy();
    expect(screen.queryByText(/No battery projection/)).toBeNull();
    expect(screen.queryByText(/No forecast data yet/)).toBeNull();
  });

  it('says the planned line is what the plan expected if it was followed', async () => {
    render(<ForecastPage />);
    await choose('24 hours');
    const note = await screen.findByTestId('forecast-past-plan-note');
    expect(note.textContent).toMatch(/if its recommendation was followed/);
  });
});
