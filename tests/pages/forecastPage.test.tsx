import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, act } from '@testing-library/react';
import { useInverterStore } from '../../src/store/useInverterStore';

// ---------------------------------------------------------------------------
// ForecastPage (issue #283 Phase 1): the summary card renders the payload's
// headline numbers, degradation statuses render as explanations (never
// zeros pretending to be predictions), and the battery projection is
// omitted cleanly when the backend reports no battery.
// ---------------------------------------------------------------------------

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/forecast') return { ok: true, data: fullPayload() };
    if (path === '/api/forecast/plan') return planPayload('no_plan');
    return { ok: true, data: {} };
  }),
  apiPost: vi.fn(async () => ({ ok: true })),
  getApiBase: () => 'http://127.0.0.1:7337',
}));

// vi.hoisted values are computed BEFORE vi.mock factories run (factories
// capture refs to these via the closure). Defining fullPayload /
// planPayload at module scope would be a ReferenceError when the mock
// factory's arrow body is executed at hoist time.
const { fullPayload, planPayload } = vi.hoisted(() => {
  const fullPayload = () => ({
    generated_at: 1_700_000_000,
    status: [],
    performance_ratio: 0.8,
    performance_ratio_days: 12,
    solar: [
      { timestamp: 1_700_003_600, kwh: 2.0, band_low: 1.6, band_high: 2.4 },
    ],
    solar_today_remaining_kwh: 10,
    solar_tomorrow_kwh: 18.4,
    consumption_weekday: Array.from({ length: 24 }, (_: number, hour: number) => ({
      hour,
      kwh: 0.5,
      p25: 0.4,
      p75: 0.6,
    })),
    consumption_weekend: Array.from({ length: 24 }, (_: number, hour: number) => ({
      hour,
      kwh: 0.75,
      p25: 0.6,
      p75: 0.9,
    })),
    consumption_days_observed: 14,
    consumption_sufficient: true,
    consumption_tomorrow_kwh: 11.2,
    battery: {
      capacity_kwh: 9.5,
      start_soc_pct: 62,
      reserve_soc_pct: 15,
      hours: [[1_700_003_600, 70]],
      end_soc_pct: 70,
    },
    import_tomorrow_kwh: 1.1,
    export_tomorrow_kwh: 7.2,
  });
  const planPayload = (
    kind: 'charge' | 'no_charge_needed' | 'no_plan',
  ): { ok: true; data: unknown } => {
    if (kind === 'charge') {
      return {
        ok: true,
        data: {
          recommendation: {
            kind: 'charge',
            window: { start: '02:00', end: '03:36', rate: 0.09, tomorrow: true },
            kwh: 3.2,
            min_soc_pct: 20,
            observed_min_soc_pct: 4,
            after_min_soc_pct: 80,
            charge_target_soc_pct: 100,
            current_soc_pct: 25,
            rationale: 'Battery is at 25% now and solar leaves it at 30%. Charging 3.2 kWh in the 9.0p window lifts it to 80%.',
            // Real planner payload (planner v2): the trajectory when the
            // The recommendation covers only the next charge occurrence;
            // the Battery tab chart stops the dashed overlay at the next
            // cheap-period start so the following plan can be recalculated.
            with_charge_series: [
              [1_700_003_600, 30],
              [1_700_007_200, 35],
              [1_700_010_800, 40],
            ],
            // Tomorrow's totals under the plan — drive the Tomorrow
            // tiles so they agree with the recommendation.
            import_tomorrow_with_charge_kwh: 3.5,
            export_tomorrow_with_charge_kwh: 0.2,
          },
          apply: {
            charge_slot: {
              slot: 1,
              enabled: true,
              start_hour: 2,
              start_minute: 0,
              end_hour: 3,
              end_minute: 36,
              target_soc: 100,
              charge_rate_percent: 100,
            },
            timed_charge: { enabled: true },
          },
        },
      };
    }
    if (kind === 'no_charge_needed') {
      return {
        ok: true,
        data: {
          recommendation: {
            kind: 'no_charge_needed',
            min_soc_pct: 20,
            observed_min_soc_pct: 80,
            current_soc_pct: 80,
            rationale: 'Sunny day — the battery fills from solar.',
          },
          apply: null,
        },
      };
    }
    return {
      ok: true,
      data: {
        recommendation: {
          kind: 'no_plan',
          reason: 'no battery projection available — connect to the inverter',
        },
        apply: null,
      },
    };
  };
  return { fullPayload, planPayload };
});

vi.mock('recharts', () => ({
  ResponsiveContainer: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  AreaChart: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  ComposedChart: ({ children }: { children: React.ReactNode }) => (
    <div data-testid="forecast-composed-chart">{children}</div>
  ),
  Area: ({ name }: { name?: string }) => <div>{name}</div>,
  Line: ({ name, stroke, strokeWidth }: {
    name?: string;
    stroke?: string;
    strokeWidth?: number;
  }) => (
    <div data-stroke={stroke} data-stroke-width={strokeWidth}>{name}</div>
  ),
  LineChart: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  Customized: ({
    horizontalValues,
    strokeWidth,
  }: {
    horizontalValues?: number[];
    strokeWidth?: number;
  }) => (
    <div
      data-testid="forecast-grid-overlay"
      data-horizontal-values={JSON.stringify(horizontalValues ?? [])}
      data-stroke-width={strokeWidth}
    />
  ),
  XAxis: () => <div />,
  YAxis: () => <div />,
  Tooltip: () => <div />,
  CartesianGrid: ({
    horizontalValues,
    strokeWidth,
  }: {
    horizontalValues?: number[];
    strokeWidth?: number;
  }) => (
    <div
      data-testid="forecast-cartesian-grid"
      data-horizontal-values={JSON.stringify(horizontalValues ?? [])}
      data-stroke-width={strokeWidth}
    />
  ),
  ReferenceLine: ({ x, label }: {
    x?: number;
    label?: { value?: string };
  }) => (
    <div
      data-testid={x == null ? 'forecast-reference-line' : 'forecast-charge-marker'}
      data-x={x}
      data-label={label?.value}
    />
  ),
  Legend: ({ payload }: {
    payload?: Array<{ value?: string }>;
  }) => payload ? (
    <div data-testid="forecast-charge-legend">
      {payload.map((entry) => entry.value).join(' ')}
    </div>
  ) : <div />,
}));

globalThis.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

import ForecastPage from '../../src/pages/ForecastPage';
import { apiGet, apiPost } from '../../src/lib/api';

const apiGetMock = vi.mocked(apiGet);
const apiPostMocked = vi.mocked(apiPost);

describe('ForecastPage', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useInverterStore.setState({ gridLineWeight: 'standard' });
  });

  afterEach(() => {
    cleanup();
  });

  it('renders the Tomorrow summary with payload numbers', async () => {
    render(<ForecastPage />);
    await waitFor(() => {
      expect(screen.getByText(/tomorrow/i)).toBeTruthy();
    });
    await waitFor(() => {
      expect(screen.getByTestId('forecast-solar-tomorrow').textContent).toMatch(/18\.4/);
      expect(screen.getByTestId('forecast-consumption-tomorrow').textContent).toMatch(/11\.2/);
      expect(screen.getByTestId('forecast-surplus-tomorrow').textContent).toMatch(/7\.2/);
      expect(screen.getByTestId('forecast-import-tomorrow').textContent).toMatch(/1\.1/);
      expect(screen.getByTestId('forecast-start-soc').textContent).toMatch(/62/);
    });
    // Calibrated — no degradation banner.
    expect(screen.queryByTestId('forecast-status-banner')).toBeNull();
  });

  it('renders all forecast charts across the 72-hour forward axis', async () => {
    // The consumption chart is tiled onto the forward timestamps (the
    // solar series' axis) instead of a midnight-anchored 24 h "typical
    // day" — the three charts start at the same "now" and cover the full
    // 72-hour forecast horizon.
    render(<ForecastPage />);
    await waitFor(() => {
      expect(screen.getByText('Solar forecast (next 72 h)')).toBeTruthy();
      expect(screen.getByText('Consumption profile (next 72 h)')).toBeTruthy();
      expect(screen.getByText('Battery projection (next 72 h)')).toBeTruthy();
    });
    expect(screen.queryByText('Consumption profile (typical day)')).toBeNull();
  });

  it('labels consumption bands as low and high estimates', async () => {
    render(<ForecastPage />);
    await waitFor(() => {
      expect(screen.getByText('Weekday low estimate')).toBeTruthy();
      expect(screen.getByText('Weekday high estimate')).toBeTruthy();
      expect(screen.getByText('Weekend low estimate')).toBeTruthy();
      expect(screen.getByText('Weekend high estimate')).toBeTruthy();
      expect(screen.getByText('Weekday median')).toBeTruthy();
      expect(screen.getByText('Weekend median')).toBeTruthy();
      expect(screen.queryByText('Median consumption')).toBeNull();
    });
  });

  it('draws distinct weekday and weekend medians without a duplicate composite line', async () => {
    render(<ForecastPage />);
    await waitFor(() => {
      expect(screen.getByText('Weekday median')).toBeTruthy();
    });
    const weekday = screen.getByText('Weekday median');
    const weekend = screen.getByText('Weekend median');
    expect(weekday.compareDocumentPosition(weekend) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.queryByText('Median consumption')).toBeNull();
  });

  it('uses a composed chart so both consumption median lines render and join the tooltip', async () => {
    render(<ForecastPage />);
    expect(await screen.findByTestId('forecast-composed-chart')).toBeTruthy();
    expect(screen.getByText('Weekday median')).toBeTruthy();
    expect(screen.getByText('Weekend median')).toBeTruthy();
  });

  it('aligns horizontal grid lines with the displayed y-axis ticks', async () => {
    render(<ForecastPage />);
    await waitFor(() => {
      const grids = screen.getAllByTestId('forecast-cartesian-grid');
      expect(grids[0].getAttribute('data-horizontal-values')).toBe(
        JSON.stringify([0, 0.5, 1, 1.5, 2, 2.5]),
      );
      expect(grids[1].getAttribute('data-horizontal-values')).toBe(
        JSON.stringify([0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6]),
      );
    });
  });

  it('uses the Chart Grid Lines preference for forecast grids', async () => {
    useInverterStore.setState({ gridLineWeight: 'subtle' });
    render(<ForecastPage />);
    await waitFor(() => {
      const grids = screen.getAllByTestId('forecast-cartesian-grid');
      expect(grids[0].getAttribute('data-stroke-width')).toBe('1');
      expect(grids[1].getAttribute('data-stroke-width')).toBe('1');
      expect(grids[2].getAttribute('data-stroke-width')).toBe('1');
      const overlays = screen.getAllByTestId('forecast-grid-overlay');
      expect(overlays[0].getAttribute('data-stroke-width')).toBe('1');
      expect(overlays[1].getAttribute('data-stroke-width')).toBe('1');
    });
  });

  it('keeps horizontal grid lines visible over the consumption bands', async () => {
    render(<ForecastPage />);
    await waitFor(() => {
      const overlays = screen.getAllByTestId('forecast-grid-overlay');
      expect(overlays[0].getAttribute('data-horizontal-values')).toBe(
        JSON.stringify([0, 0.5, 1, 1.5, 2, 2.5]),
      );
      expect(overlays[1].getAttribute('data-horizontal-values')).toBe(
        JSON.stringify([0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6]),
      );
    });
  });

  it('keeps the consumption chart on a now-anchored axis even with no solar or battery series', async () => {
    // Degraded state: weather off (no solar) and no battery projection.
    // The consumption chart must not fall back to a midnight-anchored
    // day — it generates its own now-anchored forward axis — and must
    // not show the "Not enough history" placeholder while a profile
    // exists.
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') {
        return {
          ok: true,
          data: { ...fullPayload(), solar: [], battery: null },
        };
      }
      if (path === '/api/forecast/plan') return planPayload('no_plan');
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    await waitFor(() => {
      expect(screen.getByText('Consumption profile (next 72 h)')).toBeTruthy();
    });
    expect(screen.queryByText('Not enough history yet.')).toBeNull();
  });

  it('shows the not-enough-history placeholder when no consumption profile exists', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') {
        return {
          ok: true,
          data: {
            ...fullPayload(),
            consumption_weekday: [],
            consumption_weekend: [],
          },
        };
      }
      if (path === '/api/forecast/plan') return planPayload('no_plan');
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    await waitFor(() => {
      expect(screen.getByText('Not enough history yet.')).toBeTruthy();
    });
  });

  it('shows the Tomorrow import/export tiles under the charge plan, not the uncharged simulation', async () => {
    // The tiles must agree with the plan being recommended: with a
    // Charge plan the Expected import tile includes the window's grid
    // draw (with a hint saying so) instead of the uncharged-sim
    // residual (0.3 kWh against a 6.2 kWh plan was the live report).
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('charge');
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    await waitFor(() => {
      expect(screen.getByTestId('forecast-plan').textContent).toMatch(/3\.2/);
    });
    // Uncharged tiles would read 1.1 import / 7.2 export; the plan
    // scenario numbers (3.5 / 0.2) must win.
    expect(screen.getByTestId('forecast-import-tomorrow').textContent).toMatch(/3\.5/);
    expect(screen.getByTestId('forecast-import-tomorrow').textContent).not.toMatch(/1\.1/);
    expect(screen.getByText(/incl\. 3\.2 kWh charge/)).toBeTruthy();
    expect(screen.getByTestId('forecast-surplus-tomorrow').textContent).toMatch(/0\.2/);
    expect(screen.getByText(/with charge plan/)).toBeTruthy();
  });

  it('renders degradation explanations instead of hiding the page', async () => {
    apiGetMock.mockImplementation(async () => ({
      ok: true,
      data: {
        ...fullPayload(),
        status: ['calibrating', 'no_snapshot'],
        battery: null,
      },
    }));

    render(<ForecastPage />);
    const banner = await waitFor(() =>
      screen.getByTestId('forecast-status-banner'),
    );
    expect(banner.textContent).toMatch(/calibrat/i);
    expect(banner.textContent).toMatch(/inverter/i);
    // No battery projection → the start-SOC tile is hidden, not zero.
    expect(screen.queryByTestId('forecast-start-soc')).toBeNull();
  });

  it('surfaces a fetch failure as an error card, not a crash', async () => {
    apiGetMock.mockImplementation(async () => {
      throw new Error('backend down');
    });

    render(<ForecastPage />);
    await waitFor(() => {
      expect(screen.getByTestId('forecast-error').textContent).toMatch(/backend down/i);
    });
  });
});

describe('ForecastPage plan card', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useInverterStore.setState({ gridLineWeight: 'standard' });
  });

  afterEach(() => {
    cleanup();
  });

  it('renders a Charge recommendation with an Apply button', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('charge');
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    await waitFor(() => {
      expect(screen.getByTestId('forecast-plan').textContent).toMatch(/3\.2/);
    });
    expect(screen.getByTestId('forecast-plan').textContent).toMatch(/02:00/);
    expect(screen.getByTestId('forecast-plan').textContent).toMatch(/03:36/);
    const apply = screen.getByTestId('forecast-plan-apply');
    await waitFor(() => {
      fireEvent.click(apply);
    });
    // apply-charge-slot: the slot is dispatched first.
    const slotCall = apiPostMocked.mock.calls.find((c) => c[0] === '/api/control/charge-slot');
    expect(slotCall).toBeTruthy();
    expect(slotCall?.[1]).toMatchObject({
      end_hour: 3,
      end_minute: 36,
      target_soc: 100,
      charge_rate_percent: 100,
    });
    const timedCall = apiPostMocked.mock.calls.find((c) => c[0] === '/api/control/timed-charge');
    expect(timedCall).toBeTruthy();
  });

  it('shows inverter-write progress while applying the charge plan', async () => {
    let resolveSlot: ((value: unknown) => void) | undefined;
    apiPostMocked.mockImplementationOnce(() => new Promise((resolve) => {
      resolveSlot = resolve;
    }));
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('charge');
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    const apply = await screen.findByTestId('forecast-plan-apply');
    fireEvent.click(apply);
    expect(await screen.findByRole('status')).toHaveTextContent('Applying changes to inverter');
    expect(apply).toBeDisabled();
    resolveSlot?.({ ok: true });
  });

  it('shows a retryable error when applying the charge plan fails', async () => {
    apiPostMocked.mockRejectedValueOnce(new Error('inverter busy'));
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('charge');
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    const apply = await screen.findByTestId('forecast-plan-apply');
    fireEvent.click(apply);
    expect(await screen.findByRole('alert')).toHaveTextContent('inverter busy');
    expect(apply).toHaveTextContent('Retry Apply');
    expect(apply).not.toBeDisabled();
  });

  it('draws a dashed with-charge line on the Battery projection chart when the plan recommends a charge', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('charge');
      return { ok: true, data: {} };
    });
    const { container } = render(<ForecastPage />);
    // The Battery projection chart gains a caption naming the window
    // and kWh the dashed line represents — so the user can tell what
    // the second line means without reading the plan card.
    await waitFor(() => {
      expect(container.textContent).toMatch(/SOC if overnight charge enacted/);
    });
    expect(container.textContent).toMatch(/02:00/);
    expect(container.textContent).toMatch(/03:36/);
    expect(container.textContent).toMatch(/3\.2 kWh/);
    // The caption is a footer INSIDE the card's <section> (the card's
    // rounded background), after the fixed-height chart area — not a
    // child of the h-56 chart div where it would overflow the card and
    // render outside the background.
    const card = screen
      .getByText('Battery projection (next 72 h)')
      .closest('section');
    expect(card).not.toBeNull();
    const caption = container.textContent?.match(/SOC if overnight charge enacted/);
    expect(caption).toBeTruthy();
    const chartDiv = screen.getByText('Battery projection (next 72 h)').parentElement
      ?.querySelector('div.h-56');
    expect(chartDiv).not.toBeNull();
    // Caption paragraph lives after the chart div, still inside section.
    const footer = card?.querySelector('div.mt-2 > p');
    expect(footer?.textContent).toMatch(/SOC if overnight charge enacted/);
    expect(footer?.textContent).toMatch(/3\.2 kWh/);
  });

  it('marks the next charge start and end on the Battery projection chart', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('charge');
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    await waitFor(() => {
      expect(screen.getAllByTestId('forecast-charge-marker')).toHaveLength(2);
    });
    expect(screen.getByTestId('forecast-charge-legend').textContent).toMatch(/Charge start/);
    expect(screen.getByTestId('forecast-charge-legend').textContent).toMatch(/Charge end/);
  });

  it('hides Apply when no charge is needed', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('no_charge_needed');
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    await waitFor(() => {
      expect(screen.getByTestId('forecast-plan').textContent).toMatch(/No overnight charge/i);
    });
    expect(screen.queryByTestId('forecast-plan-apply')).toBeNull();
  });

  it('renders the reason when no plan is available', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('no_plan');
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    await waitFor(() => {
      expect(screen.getByTestId('forecast-plan').textContent).toMatch(/plan (unavailable|not ready)/i);
    });
    expect(screen.queryByTestId('forecast-plan-apply')).toBeNull();
  });
});


describe('ForecastPage refresh triggers', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useInverterStore.setState({ snapshot: null } as never);
  });
  afterEach(() => {
    cleanup();
    useInverterStore.setState({ snapshot: null } as never);
  });

  it('refetches when the live snapshot SOC changes by >= 1 pp', async () => {
    useInverterStore.setState({ snapshot: { soc: 50, max_battery_power_w: 3000 } as never });
    render(<ForecastPage />);
    await waitFor(() =>
      expect(screen.getByTestId('forecast-solar-tomorrow').textContent).toMatch(/18\.4/),
    );
    apiGetMock.mockClear();
    await act(async () => {
      useInverterStore.setState({ snapshot: { soc: 51, max_battery_power_w: 3000 } as never });
    });
    await waitFor(() => {
      const calls = apiGetMock.mock.calls.filter(
        (c) => c[0] === '/api/forecast' || c[0] === '/api/forecast/plan',
      );
      expect(calls.length).toBeGreaterThanOrEqual(2);
    });
  });

  it('refetches when the rate cap changes, same SOC', async () => {
    useInverterStore.setState({ snapshot: { soc: 50, max_battery_power_w: 3000 } as never });
    render(<ForecastPage />);
    await waitFor(() =>
      expect(screen.getByTestId('forecast-solar-tomorrow').textContent).toMatch(/18\.4/),
    );
    apiGetMock.mockClear();
    await act(async () => {
      useInverterStore.setState({ snapshot: { soc: 50, max_battery_power_w: 5000 } as never });
    });
    await waitFor(() => {
      const calls = apiGetMock.mock.calls.filter(
        (c) => c[0] === '/api/forecast' || c[0] === '/api/forecast/plan',
      );
      expect(calls.length).toBeGreaterThanOrEqual(2);
    });
  });

  it('debounces no-op changes within 5 s', async () => {
    useInverterStore.setState({ snapshot: { soc: 50, max_battery_power_w: 3000 } as never });
    render(<ForecastPage />);
    await waitFor(() =>
      expect(screen.getByTestId('forecast-solar-tomorrow').textContent).toMatch(/18\.4/),
    );
    apiGetMock.mockClear();
    await act(async () => {
      useInverterStore.setState({ snapshot: { soc: 50, max_battery_power_w: 3000 } as never });
    });
    await new Promise((r) => setTimeout(r, 80));
    const calls = apiGetMock.mock.calls.filter(
      (c) => c[0] === '/api/forecast' || c[0] === '/api/forecast/plan',
    );
    expect(calls.length).toBe(0);
  });
});

  it('shows the live current SOC on the plan card so reactivity is visible', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') {
        const liveSoc = (useInverterStore.getState().snapshot as { soc?: number } | null)?.soc ?? 0;
        const out = planPayload('charge') as { data: { recommendation: { current_soc_pct: number; rationale: string } } };
        out.data.recommendation.current_soc_pct = liveSoc;
        out.data.recommendation.rationale = `Battery is at ${liveSoc}% now...`;
        return out;
      }
      return { ok: true, data: {} };
    });
    useInverterStore.setState({ snapshot: { soc: 25, max_battery_power_w: 3000 } as never });
    render(<ForecastPage />);
    await waitFor(() => {
      expect(screen.getByTestId('forecast-plan-current-soc').textContent).toMatch(/25%/);
    });
    await act(async () => {
      useInverterStore.setState({ snapshot: { soc: 32, max_battery_power_w: 3000 } as never });
    });
    await waitFor(() => {
      expect(screen.getByTestId('forecast-plan-current-soc').textContent).toMatch(/32%/);
    });
  });

describe('ForecastPage min SOC input', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useInverterStore.setState({ snapshot: null } as never);
  });
  afterEach(() => {
    cleanup();
    useInverterStore.setState({ snapshot: null } as never);
  });

  it('lets the user change the minimum SOC and refetches the plan', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan')
        return planPayload('no_charge_needed');
      if (path === '/api/settings') {
        return {
          ok: true,
          data: {
            forecast_min_soc_pct: 20,
            forecast_charge_efficiency: 0.9,
            forecast_discharge_efficiency: 0.95,
          },
        };
      }
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    const input = await waitFor(() =>
      screen.getByTestId('forecast-min-soc-input') as HTMLInputElement,
    );
    // Plan = no charge by default, but the input must still render the
    // current saved min SOC so the user can tune it.
    expect(input.value).toBe('20');

    apiPostMocked.mockClear();
    fireEvent.change(input, { target: { value: '40' } });
    fireEvent.blur(input);
    // Should POST the new min SOC and refetch /api/forecast/plan.
    await waitFor(() => {
      const post = apiPostMocked.mock.calls.find(
        (c) => c[0] === '/api/settings',
      );
      expect(post?.[1]).toEqual({ forecast_min_soc_pct: 40 });
    });
  });
});

describe('ForecastPage issue #283 feedback fixes', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useInverterStore.setState({ snapshot: null } as never);
  });
  afterEach(() => {
    cleanup();
    useInverterStore.setState({ snapshot: null } as never);
  });

  it('explains what the minimum battery level means', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('no_charge_needed');
      if (path === '/api/settings') {
        return { ok: true, data: { forecast_min_soc_pct: 20 } };
      }
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    await waitFor(() => {
      expect(
        screen.getByText(/planner sizes the next overnight charge so the battery stays/i),
      ).toBeTruthy();
    });
  });

  it('shows the calibration window on the solar prediction tile', async () => {
    render(<ForecastPage />);
    await waitFor(() => {
      expect(
        screen.getByText(/calibrated against 12 days of your history \(last 2 weeks\)/i),
      ).toBeTruthy();
    });
  });
});

describe('ForecastPage plan cycle note', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useInverterStore.setState({ snapshot: null } as never);
  });
  afterEach(() => {
    cleanup();
    useInverterStore.setState({ snapshot: null } as never);
  });

  it('tells the user the plan covers one cycle and must be re-applied daily', async () => {
    // Auto-refresh off (the default): the inverter repeats the applied
    // slot nightly, so the note has to say tomorrow needs a re-apply.
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('charge');
      if (path === '/api/settings') {
        return { ok: true, data: { forecast_min_soc_pct: 20 } };
      }
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    const note = await waitFor(() => screen.getByTestId('forecast-plan-cycle-note'));
    expect(note.textContent).toMatch(/next charge cycle only/i);
    expect(note.textContent).toMatch(/re-apply tomorrow/i);
    expect(note.textContent).not.toMatch(/auto-refresh/i);
  });

  it('drops the daily re-apply wording when auto-refresh owns the slot', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('charge');
      if (path === '/api/settings') {
        return {
          ok: true,
          data: { forecast_min_soc_pct: 20, forecast_plan_auto_refresh: true },
        };
      }
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    const note = await waitFor(() => screen.getByTestId('forecast-plan-cycle-note'));
    expect(note.textContent).toMatch(/next charge cycle only/i);
    expect(note.textContent).toMatch(/auto-refresh re-sizes charge slot 1/i);
    expect(note.textContent).not.toMatch(/re-apply tomorrow/i);
  });

  it('does not show the cycle note when nothing is applied', async () => {
    // No charge needed → no recurring slot → no daily-recalculation
    // contract to explain.
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('no_charge_needed');
      if (path === '/api/settings') {
        return { ok: true, data: { forecast_min_soc_pct: 20 } };
      }
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    await waitFor(() => {
      expect(screen.getByText(/floor is held on solar alone/i)).toBeTruthy();
    });
    expect(screen.queryByTestId('forecast-plan-cycle-note')).toBeNull();
  });
});
