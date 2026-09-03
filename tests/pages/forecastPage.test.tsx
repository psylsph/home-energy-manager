import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, act, within } from '@testing-library/react';
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
    exportAdvice?: Record<string, unknown> | null,
  ): { ok: true; data: unknown } => {
    const base: Record<string, unknown> = (() => {
    if (kind === 'charge') {
      return {
          recommendation: {
            kind: 'charge',
            window: { start: '02:00', end: '03:36', rate: 0.09, tomorrow: true },
            kwh: 3.2,
            min_soc_pct: 20,
            observed_min_soc_pct: 4,
            after_min_soc_pct: 80,
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
        };
    }
    if (kind === 'no_charge_needed') {
      return {
          recommendation: {
            kind: 'no_charge_needed',
            min_soc_pct: 20,
            observed_min_soc_pct: 80,
            current_soc_pct: 80,
            rationale: 'Sunny day — the battery fills from solar.',
          },
          apply: null,
      };
    }
    if (kind === 'no_plan') {
      return {
          recommendation: {
            kind: 'no_plan',
            reason: 'no battery projection available — connect to the inverter',
          },
          apply: null,
      };
    }
    return {};
    })();
    if (exportAdvice !== undefined) {
      base.export = exportAdvice;
    }
    return {
      ok: true,
      data: base,
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
  Line: ({ name, stroke, strokeWidth, isAnimationActive }: {
    name?: string;
    stroke?: string;
    strokeWidth?: number;
    isAnimationActive?: boolean;
  }) => (
    <div
      data-stroke={stroke}
      data-stroke-width={strokeWidth}
      data-animate={isAnimationActive === false ? 'off' : 'on'}
    >{name}</div>
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
      expect(screen.getByText('Tomorrow')).toBeTruthy();
    });
    await waitFor(() => {
      expect(screen.getByTestId('forecast-solar-tomorrow').textContent).toMatch(/18\.4/);
      expect(screen.getByTestId('forecast-consumption-tomorrow').textContent).toMatch(/11\.2/);
      expect(screen.getByTestId('forecast-surplus-tomorrow').textContent).toMatch(/7\.2/);
      expect(screen.getByTestId('forecast-import-tomorrow').textContent).toMatch(/1\.1/);
    });
    // Calibrated — no degradation banner.
    expect(screen.queryByTestId('forecast-status-banner')).toBeNull();
  });

  it('attributes forecast data to Open-Meteo under CC BY 4.0', async () => {
    // Open-Meteo's CC-BY 4.0 licence requires attribution where the data is
    // presented — the page's entire content is Open-Meteo-derived.
    render(<ForecastPage />);
    await waitFor(() => {
      expect(screen.getByTestId('forecast-attribution')).toBeTruthy();
    });
    expect(screen.getByText('Open-Meteo.com')).toBeTruthy();
    expect(screen.getByTestId('forecast-attribution').textContent).toMatch(
      /CC BY 4\.0/,
    );
  });

  it('offers the three forecast charts across the 72-hour forward axis', async () => {
    // The consumption chart is tiled onto the forward timestamps (the
    // solar series' axis) instead of a midnight-anchored 24 h "typical
    // day" — the three charts start at the same "now" and cover the full
    // 72-hour forecast horizon.
    render(<ForecastPage />);
    await waitFor(() => {
      expect(screen.getByText('Expected solar generation · next 72 hours')).toBeTruthy();
      expect(screen.getByText('Expected home use · next 72 hours')).toBeTruthy();
      expect(screen.getByText('Battery projection · next 72 hours')).toBeTruthy();
    });
    expect(screen.queryByText('Consumption profile (typical day)')).toBeNull();
  });

  it('shows one chart at a time and switches charts with tabs', async () => {
    render(<ForecastPage />);
    const batteryTab = await screen.findByTestId('forecast-chart-tab-battery');
    const solarTab = screen.getByTestId('forecast-chart-tab-solar');
    expect(batteryTab.getAttribute('aria-selected')).toBe('true');
    expect(solarTab.getAttribute('aria-selected')).toBe('false');

    fireEvent.click(solarTab);

    expect(solarTab.getAttribute('aria-selected')).toBe('true');
    expect(batteryTab.getAttribute('aria-selected')).toBe('false');
    expect(
      screen.getByText('Expected solar generation · next 72 hours').closest('[aria-hidden]')
        ?.getAttribute('aria-hidden'),
    ).toBe('false');
  });

  it('links each chart tab to an addressable tabpanel', async () => {
    // role=tab without an addressable role=tabpanel breaks the ARIA tabs
    // contract: screen readers announce a selected tab whose panel can
    // never be located. Every tab must point at a real panel via
    // aria-controls, the panel must point back via aria-labelledby, and
    // only the active tab may sit in the tab order (roving tabindex).
    render(<ForecastPage />);
    const batteryTab = await screen.findByTestId('forecast-chart-tab-battery');
    expect(batteryTab.getAttribute('aria-selected')).toBe('true');

    const charts = ['battery', 'solar', 'consumption'] as const;
    for (const chart of charts) {
      const tab = screen.getByTestId(`forecast-chart-tab-${chart}`);
      const panelId = tab.getAttribute('aria-controls');
      expect(panelId, `${chart} tab must reference a panel`).toMatch(
        /^forecast-chart-panel-/,
      );
      const panel = document.getElementById(panelId ?? '');
      expect(panel, `${chart} panel must exist`).not.toBeNull();
      expect(panel?.getAttribute('role')).toBe('tabpanel');
      expect(panel?.getAttribute('aria-labelledby')).toBe(tab.id);
    }

    for (const chart of charts) {
      const tab = screen.getByTestId(`forecast-chart-tab-${chart}`) as HTMLButtonElement;
      expect(tab.tabIndex, `only the active tab is tabbable`).toBe(
        chart === 'battery' ? 0 : -1,
      );
    }
  });

  it('arrow keys move through the chart tabs and wrap at the ends', async () => {
    // The tabs pattern expects a keyboard path: Arrow keys select and
    // focus the neighbouring tab (wrapping), Home/End jump to the ends.
    render(<ForecastPage />);
    const battery = (await screen.findByTestId(
      'forecast-chart-tab-battery',
    )) as HTMLButtonElement;
    battery.focus();
    expect(document.activeElement).toBe(battery);

    const solar = screen.getByTestId('forecast-chart-tab-solar') as HTMLButtonElement;
    fireEvent.keyDown(battery, { key: 'ArrowRight' });
    expect(solar.getAttribute('aria-selected')).toBe('true');
    expect(document.activeElement).toBe(solar);

    fireEvent.keyDown(solar, { key: 'ArrowLeft' });
    expect(battery.getAttribute('aria-selected')).toBe('true');
    expect(document.activeElement).toBe(battery);

    // Left from the first tab wraps to the last.
    const consumption = screen.getByTestId(
      'forecast-chart-tab-consumption',
    ) as HTMLButtonElement;
    fireEvent.keyDown(battery, { key: 'ArrowLeft' });
    expect(consumption.getAttribute('aria-selected')).toBe('true');
    expect(document.activeElement).toBe(consumption);

    // Home/End jump to the first/last tab.
    fireEvent.keyDown(consumption, { key: 'Home' });
    expect(battery.getAttribute('aria-selected')).toBe('true');
    expect(document.activeElement).toBe(battery);
    fireEvent.keyDown(battery, { key: 'End' });
    expect(consumption.getAttribute('aria-selected')).toBe('true');
    expect(document.activeElement).toBe(consumption);
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
      expect(screen.getByText('Expected home use · next 72 hours')).toBeTruthy();
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
    expect(screen.getByText('Today’s recommendation')).toBeTruthy();
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
      .getByText('Battery projection · next 72 hours')
      .closest('section');
    expect(card).not.toBeNull();
    const caption = container.textContent?.match(/SOC if overnight charge enacted/);
    expect(caption).toBeTruthy();
    const chartDiv = screen.getByText('Battery projection · next 72 hours').parentElement
      ?.querySelector('div.h-56');
    expect(chartDiv).not.toBeNull();
    // Caption paragraph lives after the chart div, still inside section.
    const footer = card?.querySelector('[data-testid="forecast-caption-plan"]');
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
    // Marker keys live in the card FOOTER — small and away from the
    // graph — not in the under-chart line legend.
    expect(
      screen.getByTestId('forecast-charge-legend').closest('div')?.className,
    ).toMatch(/mt-2/);
    expect(screen.getByTestId('forecast-line-legend').textContent).not.toMatch(
      'Charge start',
    );
  });

  it('draws the current-schedule line on the Battery projection chart when slots are enabled', async () => {
    // Issue #297: a third line projecting the battery under the
    // inverter's CURRENT schedule — neither the Eco projection (no
    // timed windows) nor the plan's hypothetical charge.
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') {
        return {
          ok: true,
          data: {
            ...fullPayload(),
            battery: {
              ...fullPayload().battery,
              with_current_schedule: [
                [1_700_003_600, 65],
                [1_700_007_200, 65],
              ],
            },
          },
        };
      }
      if (path === '/api/forecast/plan') return planPayload('no_plan');
      return { ok: true, data: {} };
    });
    const { container } = render(<ForecastPage />);
    await waitFor(() => {
      // The legend gains the toggleable description…
      expect(
        screen.getByRole('button', { name: 'With current schedule' }),
      ).toBeTruthy();
    });
    // …and the pink line itself renders on the chart.
    expect(container.querySelector('[data-stroke="#f472b6"]')).not.toBeNull();
    // A caption inside the card explains what the line represents.
    const card = screen
      .getByText('Battery projection · next 72 hours')
      .closest('section');
    expect(card?.textContent).toMatch(/current inverter schedule/i);
  });

  it('hides the current-schedule line when no slot is enabled', async () => {
    // Without enabled slots the projection would duplicate the Eco line:
    // neither the pink line nor its legend entry may render.
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('no_plan');
      return { ok: true, data: {} };
    });
    const { container } = render(<ForecastPage />);
    await screen.findByText('Tomorrow');
    expect(
      screen.queryByRole('button', { name: 'With current schedule' }),
    ).toBeNull();
    expect(container.querySelector('[data-stroke="#f472b6"]')).toBeNull();
  });

  it('gives every projection line a toggleable legend entry under the chart', async () => {
    // Issue #297 feedback: the line descriptions (including the
    // no-grid-charging SOC baseline) belong in a legend close under the
    // graph; the Charge start/end marker keys must not be in it.
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') {
        return {
          ok: true,
          data: {
            ...fullPayload(),
            battery: {
              ...fullPayload().battery,
              with_current_schedule: [[1_700_003_600, 65]],
            },
          },
        };
      }
      if (path === '/api/forecast/plan') return planPayload('charge');
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    const legend = await screen.findByTestId('forecast-line-legend');
    expect(legend.textContent).toMatch('If solar only');
    expect(legend.textContent).toMatch('If charge enacted');
    expect(legend.textContent).toMatch('With current schedule');
    expect(legend.textContent).not.toMatch('Charge start');
    expect(legend.textContent).not.toMatch('Charge end');
  });

  it('keeps the no-grid-charging SOC line in the legend even with no plan or schedule', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('no_plan');
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    const legend = await screen.findByTestId('forecast-line-legend');
    expect(legend.textContent).toBe('If solar only');
    // The baseline has a footer description like the other lines —
    // "do nothing" must be spelled out, not just named (issue #297
    // feedback).
    expect(screen.getByTestId('forecast-caption-baseline').textContent).toMatch(
      /SOC if no slots are configured/,
    );
  });

  it('toggles a projection line off and on from the legend', async () => {
    const { container } = render(<ForecastPage />);
    const soc = await screen.findByRole('button', { name: 'If solar only' });
    const socLine = () => container.querySelector('[data-stroke="#34d399"]');
    expect(soc.getAttribute('aria-pressed')).toBe('true');
    expect(socLine()).not.toBeNull();
    expect(screen.getByTestId('forecast-caption-baseline')).toBeTruthy();

    fireEvent.click(soc);
    expect(soc.getAttribute('aria-pressed')).toBe('false');
    expect(socLine()).toBeNull();
    // Its description hides with it.
    expect(screen.queryByTestId('forecast-caption-baseline')).toBeNull();

    fireEvent.click(soc);
    expect(soc.getAttribute('aria-pressed')).toBe('true');
    expect(socLine()).not.toBeNull();
    expect(screen.getByTestId('forecast-caption-baseline')).toBeTruthy();
  });

  it('hiding the plan line also hides the charge markers and its caption', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('charge');
      return { ok: true, data: {} };
    });
    const { container } = render(<ForecastPage />);
    expect(await screen.findByTestId('forecast-charge-legend')).toBeTruthy();
    expect(screen.getAllByTestId('forecast-charge-marker')).toHaveLength(2);

    fireEvent.click(screen.getByRole('button', { name: 'If charge enacted' }));
    expect(container.querySelector('[data-stroke="#60a5fa"]')).toBeNull();
    expect(
      container.querySelectorAll('[data-testid="forecast-charge-marker"]'),
    ).toHaveLength(0);
    expect(screen.queryByTestId('forecast-charge-legend')).toBeNull();
    expect(screen.queryByText(/SOC if overnight charge enacted/)).toBeNull();

    // Toggling back restores everything.
    fireEvent.click(screen.getByRole('button', { name: 'If charge enacted' }));
    expect(screen.getAllByTestId('forecast-charge-marker')).toHaveLength(2);
    expect(screen.getByTestId('forecast-charge-legend')).toBeTruthy();
  });

  it('toggles the current-schedule line and its caption off and on', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') {
        return {
          ok: true,
          data: {
            ...fullPayload(),
            battery: {
              ...fullPayload().battery,
              with_current_schedule: [[1_700_003_600, 65]],
            },
          },
        };
      }
      if (path === '/api/forecast/plan') return planPayload('no_plan');
      return { ok: true, data: {} };
    });
    const { container } = render(<ForecastPage />);
    const button = await screen.findByRole('button', {
      name: 'With current schedule',
    });
    expect(container.querySelector('[data-stroke="#f472b6"]')).not.toBeNull();
    expect(screen.getByTestId('forecast-caption-schedule')).toBeTruthy();

    fireEvent.click(button);
    expect(container.querySelector('[data-stroke="#f472b6"]')).toBeNull();
    expect(screen.queryByTestId('forecast-caption-schedule')).toBeNull();

    fireEvent.click(button);
    expect(container.querySelector('[data-stroke="#f472b6"]')).not.toBeNull();
    expect(screen.getByTestId('forecast-caption-schedule')).toBeTruthy();
  });

  it('renders every battery line without the draw-in animation', async () => {
    // Toggling a line via the legend replays recharts' draw-in animation
    // on the survivors unless it is disabled — the app-wide convention.
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') {
        return {
          ok: true,
          data: {
            ...fullPayload(),
            battery: {
              ...fullPayload().battery,
              with_current_schedule: [[1_700_003_600, 65]],
            },
          },
        };
      }
      if (path === '/api/forecast/plan') return planPayload('charge');
      return { ok: true, data: {} };
    });
    const { container } = render(<ForecastPage />);
    await waitFor(() => {
      expect(container.querySelector('[data-stroke="#f472b6"]')).not.toBeNull();
    });
    for (const stroke of ['#34d399', '#60a5fa', '#f472b6']) {
      expect(
        container
          .querySelector(`[data-stroke="${stroke}"]`)
          ?.getAttribute('data-animate'),
      ).toBe('off');
    }
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


describe('ForecastPage export advice', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useInverterStore.setState({ gridLineWeight: 'standard' });
  });

  afterEach(() => {
    cleanup();
  });

  const exportAdvice = {
    kind: 'export',
    window: { start: '16:00', end: '18:16', rate: 0.35, tomorrow: false },
    kwh: 5.7,
    min_soc_pct: 20,
    after_min_soc_pct: 20.4,
    earning: 2.0,
    rationale:
      'Selling about 5.7 kWh in the 35.0p export window (16:00–18:16) earns about £2.00.',
    with_export_series: [
      [1_700_003_600, 62],
      [1_700_007_200, 40],
    ],
  };

  it('renders the read-only export opportunity card when the plan carries export advice', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('charge', exportAdvice);
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    const card = await screen.findByTestId('forecast-export-card');
    expect(card.textContent).toMatch(/Export opportunity/);
    expect(screen.getByTestId('forecast-export-kwh').textContent).toMatch(/5\.7/);
    expect(screen.getByTestId('forecast-export-window').textContent).toMatch(/16:00/);
    expect(screen.getByTestId('forecast-export-window').textContent).toMatch(/18:16/);
    expect(screen.getByTestId('forecast-export-earning').textContent).toMatch(/£2\.00/);
    expect(screen.getByTestId('forecast-export-rationale').textContent).toMatch(
      /35\.0p export window/,
    );
    // Read-only v1: the export advice must not offer any Apply control.
    expect(within(card).queryByRole('button')).toBeNull();
  });

  it('hides the export card when the advice stood down', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') {
        return planPayload('charge', { kind: 'no_export', reason: 'nothing spare' });
      }
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    await screen.findByTestId('forecast-plan');
    expect(screen.queryByTestId('forecast-export-card')).toBeNull();
  });

  it('hides the export card when the payload carries no export advice', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('charge');
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    await screen.findByTestId('forecast-plan');
    expect(screen.queryByTestId('forecast-export-card')).toBeNull();
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

describe('ForecastPage plan settings edits survive background refetches', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useInverterStore.setState({ snapshot: null } as never);
  });
  afterEach(() => {
    cleanup();
    useInverterStore.setState({ snapshot: null } as never);
  });

  it('preserves unsaved edits across a SOC-triggered refetch (review H17)', async () => {
    // Review H17: every background refetch rewrote the Plan settings form
    // from /api/settings — typing a new value into "Minimum battery level"
    // snapped back mid-edit whenever the SOC moved by a point and the page
    // refetched.
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('no_charge_needed');
      if (path === '/api/settings') {
        return {
          ok: true,
          data: {
            forecast_min_soc_pct: 20,
            forecast_charge_efficiency: 0.9,
            forecast_discharge_efficiency: 0.95,
            forecast_plan_auto_apply_lead_minutes: 30,
          },
        };
      }
      return { ok: true, data: {} };
    });
    useInverterStore.setState({ snapshot: { soc: 50, max_battery_power_w: 3000 } as never });
    render(<ForecastPage />);

    const minSoc = (await waitFor(() =>
      screen.getByTestId('forecast-min-soc-input'),
    )) as HTMLInputElement;
    expect(minSoc.value).toBe('20');

    // The user starts typing a new value — not saved yet.
    fireEvent.change(minSoc, { target: { value: '45' } });
    expect(minSoc.value).toBe('45');

    apiGetMock.mockClear();
    // SOC moves 2 pp → the page refetches in the background.
    await act(async () => {
      useInverterStore.setState({ snapshot: { soc: 52, max_battery_power_w: 3000 } as never });
    });
    await waitFor(() => {
      expect(apiGetMock.mock.calls.some((c) => c[0] === '/api/forecast/plan')).toBe(true);
    });

    // The unsaved edit must survive the refetch.
    expect(
      (screen.getByTestId('forecast-min-soc-input') as HTMLInputElement).value,
    ).toBe('45');
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

  it('auto-apply note falls back to the generic text while the lead field is invalid', async () => {
    // With auto-apply on, the note names the trigger time. A half-typed
    // lead input must never render "NaN:NaN" — the note falls back to the
    // lead-agnostic text until the value is valid again.
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('charge');
      if (path === '/api/settings') {
        return {
          ok: true,
          data: {
            forecast_min_soc_pct: 20,
            forecast_plan_auto_apply_enabled: true,
            forecast_plan_auto_apply_lead_minutes: 30,
          },
        };
      }
      return { ok: true, data: {} };
    });
    render(<ForecastPage />);
    const note = await waitFor(() => screen.getByTestId('forecast-auto-apply-note'));
    // Plan window starts 02:00, lead 30 → triggers 01:30.
    expect(note.textContent).toMatch(/applies itself at 01:30/);
    const lead = screen.getByTestId('forecast-auto-apply-lead') as HTMLInputElement;
    fireEvent.change(lead, { target: { value: '' } });
    await waitFor(() => {
      expect(note.textContent).not.toMatch(/NaN/);
      expect(note.textContent).toMatch(/applies itself before each cheap window/i);
    });
  });
});

describe('ForecastPage auto-apply toggle', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useInverterStore.setState({ snapshot: null } as never);
  });
  afterEach(() => {
    cleanup();
    useInverterStore.setState({ snapshot: null } as never);
  });

  // Auto-apply on with the default 30-minute lead; the plan targets a
  // 02:00 window so the trigger note is deterministic.
  const mockLoad = () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('charge');
      if (path === '/api/settings') {
        return {
          ok: true,
          data: {
            forecast_min_soc_pct: 20,
            forecast_plan_auto_apply_enabled: true,
            forecast_plan_auto_apply_lead_minutes: 30,
          },
        };
      }
      return { ok: true, data: {} };
    });
  };

  it('editing the lead time and blurring keeps auto-apply on', async () => {
    // Regression: the blur handler used to call the toggle's save, which
    // always posted the flipped enabled state — adjusting the lead time
    // silently disabled the whole trigger.
    mockLoad();
    render(<ForecastPage />);
    const lead = await screen.findByTestId('forecast-auto-apply-lead');
    apiPostMocked.mockClear();
    fireEvent.change(lead, { target: { value: '45' } });
    fireEvent.blur(lead);
    await waitFor(() => {
      const post = apiPostMocked.mock.calls.find((c) => c[0] === '/api/settings');
      expect(post?.[1]).toEqual({
        forecast_plan_auto_apply_enabled: true,
        forecast_plan_auto_apply_lead_minutes: 45,
        forecast_plan_auto_refresh: false,
      });
    });
    const toggle = screen.getByTestId(
      'forecast-auto-apply-toggle',
    ) as HTMLInputElement;
    expect(toggle.checked).toBe(true);
  });

  it('blurring an untouched lead field does not save', async () => {
    mockLoad();
    render(<ForecastPage />);
    const lead = await screen.findByTestId('forecast-auto-apply-lead');
    apiPostMocked.mockClear();
    fireEvent.focus(lead);
    fireEvent.blur(lead);
    // Let any (wrong) save attempt flush before asserting none happened.
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(
      apiPostMocked.mock.calls.some((c) => c[0] === '/api/settings'),
    ).toBe(false);
  });

  it('the checkbox posts the flipped enabled state', async () => {
    mockLoad();
    render(<ForecastPage />);
    const toggle = (await screen.findByTestId(
      'forecast-auto-apply-toggle',
    )) as HTMLInputElement;
    await waitFor(() => expect(toggle.checked).toBe(true));
    apiPostMocked.mockClear();
    fireEvent.click(toggle);
    await waitFor(() => {
      const post = apiPostMocked.mock.calls.find((c) => c[0] === '/api/settings');
      expect(post?.[1]).toEqual({
        forecast_plan_auto_apply_enabled: false,
        forecast_plan_auto_apply_lead_minutes: 30,
        forecast_plan_auto_refresh: false,
      });
    });
  });

  it('an invalid lead blocks the save and reports the constraint', async () => {
    mockLoad();
    render(<ForecastPage />);
    const toggle = (await screen.findByTestId(
      'forecast-auto-apply-toggle',
    )) as HTMLInputElement;
    await waitFor(() => expect(toggle.checked).toBe(true));
    const lead = screen.getByTestId('forecast-auto-apply-lead');
    apiPostMocked.mockClear();
    fireEvent.change(lead, { target: { value: '999' } });
    fireEvent.click(toggle);
    await waitFor(() => {
      expect(screen.getByTestId('forecast-auto-apply-error').textContent).toMatch(
        /between 0 and 120/,
      );
    });
    expect(
      apiPostMocked.mock.calls.some((c) => c[0] === '/api/settings'),
    ).toBe(false);
    // The controlled checkbox never flipped.
    expect(
      (screen.getByTestId('forecast-auto-apply-toggle') as HTMLInputElement)
        .checked,
    ).toBe(true);
  });

  it('an emptied lead field blocks the save instead of posting a zero lead', async () => {
    // Number('') is 0: clearing the field and blurring used to save lead 0
    // — silently moving the trigger to the window's own start. It must
    // report the constraint and leave the stored lead untouched.
    mockLoad();
    render(<ForecastPage />);
    const lead = await screen.findByTestId('forecast-auto-apply-lead');
    await waitFor(() => {
      expect(
        (screen.getByTestId('forecast-auto-apply-toggle') as HTMLInputElement)
          .checked,
      ).toBe(true);
    });
    apiPostMocked.mockClear();
    fireEvent.change(lead, { target: { value: '' } });
    fireEvent.blur(lead);
    await waitFor(() => {
      expect(screen.getByTestId('forecast-auto-apply-error').textContent).toMatch(
        /between 0 and 120/,
      );
    });
    expect(
      apiPostMocked.mock.calls.some((c) => c[0] === '/api/settings'),
    ).toBe(false);
  });

  it('the Automatic planning summary falls back while the lead field is invalid', async () => {
    // The summary line used Number(leadInput), so an emptied field read
    // as a zero lead and rendered "applies at 02:00" — the window's own
    // start — while the note below it correctly fell back to the generic
    // text. Both displays must share the strict parser so they can never
    // disagree.
    mockLoad();
    render(<ForecastPage />);
    const summary = await screen.findByTestId('forecast-auto-apply-summary');
    // 30-minute lead before the 02:00 window.
    expect(summary.textContent).toMatch(/applies at 01:30/);

    fireEvent.change(screen.getByTestId('forecast-auto-apply-lead'), {
      target: { value: '' },
    });

    expect(summary.textContent).not.toMatch(/applies at 02:00/);
    expect(summary.textContent).toMatch(/the configured time/);
  });
});

describe('ForecastPage merged automatic-handling control', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useInverterStore.setState({ snapshot: null } as never);
  });
  afterEach(() => {
    cleanup();
    useInverterStore.setState({ snapshot: null } as never);
  });

  // Auto-apply and auto-refresh do the same job (keep charge slot 1 in
  // step before the cheap window), so the UI presents ONE control. It
  // reads on when either backend flag is set, and every save writes both:
  // off clears both; editing the lead upgrades legacy auto-refresh to
  // auto-apply. `backend` simulates the settings resource: GET reflects
  // what POST saved, so the post-save refetch behaves like the real thing.
  const mockLoad = (opts: {
    autoRefresh?: boolean;
    autoApply?: boolean;
  } = {}) => {
    const backend = {
      forecast_plan_auto_refresh: opts.autoRefresh ?? false,
      forecast_plan_auto_apply_enabled: opts.autoApply ?? false,
      forecast_plan_auto_apply_lead_minutes: 30,
    };
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('charge');
      if (path === '/api/settings') {
        return {
          ok: true,
          data: { forecast_min_soc_pct: 20, ...backend },
        };
      }
      return { ok: true, data: {} };
    });
    apiPostMocked.mockImplementation(async (path: string, body: unknown) => {
      if (path === '/api/settings') Object.assign(backend, body);
      return { ok: true };
    });
  };

  it('reads on for a legacy auto-refresh-only setting and explains the upgrade', async () => {
    mockLoad({ autoRefresh: true });
    render(<ForecastPage />);
    const toggle = (await screen.findByTestId(
      'forecast-auto-apply-toggle',
    )) as HTMLInputElement;
    await waitFor(() => expect(toggle.checked).toBe(true));
    expect(screen.getByTestId('forecast-auto-apply-note').textContent).toMatch(
      /Auto-refresh keeps this plan/i,
    );
  });

  it('turning the control off clears both planner flags', async () => {
    mockLoad({ autoRefresh: true });
    render(<ForecastPage />);
    const toggle = (await screen.findByTestId(
      'forecast-auto-apply-toggle',
    )) as HTMLInputElement;
    await waitFor(() => expect(toggle.checked).toBe(true));
    fireEvent.click(toggle);
    await waitFor(() => {
      const post = apiPostMocked.mock.calls.find((c) => c[0] === '/api/settings');
      expect(post?.[1]).toEqual({
        forecast_plan_auto_apply_enabled: false,
        forecast_plan_auto_apply_lead_minutes: 30,
        forecast_plan_auto_refresh: false,
      });
    });
    await waitFor(() => expect(toggle.checked).toBe(false));
  });

  it('editing the lead time upgrades legacy auto-refresh to auto-apply', async () => {
    mockLoad({ autoRefresh: true });
    render(<ForecastPage />);
    const lead = await screen.findByTestId('forecast-auto-apply-lead');
    await waitFor(() => {
      expect(
        (screen.getByTestId('forecast-auto-apply-toggle') as HTMLInputElement)
          .checked,
      ).toBe(true);
    });
    fireEvent.change(lead, { target: { value: '45' } });
    fireEvent.blur(lead);
    await waitFor(() => {
      const post = apiPostMocked.mock.calls.find((c) => c[0] === '/api/settings');
      expect(post?.[1]).toEqual({
        forecast_plan_auto_apply_enabled: true,
        forecast_plan_auto_apply_lead_minutes: 45,
        forecast_plan_auto_refresh: false,
      });
    });
    await waitFor(() => {
      expect(screen.getByTestId('forecast-auto-apply-note').textContent).toMatch(
        /applies itself at/i,
      );
    });
  });

  it('reverts the switch and reports the failure when the save fails', async () => {
    mockLoad();
    apiPostMocked.mockRejectedValueOnce(new Error('backend offline'));
    render(<ForecastPage />);
    const toggle = (await screen.findByTestId(
      'forecast-auto-apply-toggle',
    )) as HTMLInputElement;
    await waitFor(() => expect(toggle.checked).toBe(false));
    fireEvent.click(toggle);
    await waitFor(() => {
      expect(screen.getByTestId('forecast-auto-apply-error').textContent).toMatch(
        /backend offline/,
      );
    });
    // The optimistic flip is rolled back so the UI never claims the
    // planner owns the slot when the backend says otherwise.
    expect(toggle.checked).toBe(false);
  });
});

describe('ForecastPage battery efficiencies', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useInverterStore.setState({ snapshot: null } as never);
  });
  afterEach(() => {
    cleanup();
    useInverterStore.setState({ snapshot: null } as never);
  });

  // Moved here from the Settings page's Solar section (issue #283); the
  // inputs hydrate as whole percents and save together on blur as 0–1
  // ratios. `backend` reflects POSTed settings so the post-save refetch
  // keeps the edited values, like the real resource.
  const mockLoad = () => {
    const backend: Record<string, unknown> = {
      forecast_min_soc_pct: 20,
      forecast_charge_efficiency: 0.9,
      forecast_discharge_efficiency: 0.95,
    };
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/forecast') return { ok: true, data: fullPayload() };
      if (path === '/api/forecast/plan') return planPayload('charge');
      if (path === '/api/settings') {
        return { ok: true, data: { ...backend } };
      }
      return { ok: true, data: {} };
    });
    apiPostMocked.mockImplementation(async (path: string, body: unknown) => {
      if (path === '/api/settings') Object.assign(backend, body);
      return { ok: true };
    });
  };

  it('hydrates the efficiency inputs as percentages', async () => {
    mockLoad();
    render(<ForecastPage />);
    const charge = (await screen.findByTestId(
      'forecast-charge-eff-input',
    )) as HTMLInputElement;
    expect(charge.value).toBe('90');
    expect(
      (screen.getByTestId('forecast-discharge-eff-input') as HTMLInputElement)
        .value,
    ).toBe('95');
  });

  it('saves both efficiencies as 0–1 ratios on blur', async () => {
    mockLoad();
    render(<ForecastPage />);
    const charge = (await screen.findByTestId(
      'forecast-charge-eff-input',
    )) as HTMLInputElement;
    await waitFor(() => expect(charge.value).toBe('90'));
    apiPostMocked.mockClear();
    fireEvent.change(charge, { target: { value: '85' } });
    fireEvent.blur(charge);
    await waitFor(() => {
      const post = apiPostMocked.mock.calls.find((c) => c[0] === '/api/settings');
      expect(post?.[1]).toEqual({
        forecast_charge_efficiency: 0.85,
        forecast_discharge_efficiency: 0.95,
      });
    });
  });

  it('blocks the save for out-of-range efficiencies', async () => {
    mockLoad();
    render(<ForecastPage />);
    const charge = (await screen.findByTestId(
      'forecast-charge-eff-input',
    )) as HTMLInputElement;
    await waitFor(() => expect(charge.value).toBe('90'));
    apiPostMocked.mockClear();
    fireEvent.change(charge, { target: { value: '120' } });
    fireEvent.blur(charge);
    await waitFor(() => {
      expect(screen.getByTestId('forecast-eff-error').textContent).toMatch(
        /between 50 and 100/,
      );
    });
    expect(
      apiPostMocked.mock.calls.some((c) => c[0] === '/api/settings'),
    ).toBe(false);
  });

  it('does not re-save when blurred without edits', async () => {
    mockLoad();
    render(<ForecastPage />);
    const charge = (await screen.findByTestId(
      'forecast-charge-eff-input',
    )) as HTMLInputElement;
    await waitFor(() => expect(charge.value).toBe('90'));
    apiPostMocked.mockClear();
    fireEvent.focus(charge);
    fireEvent.blur(charge);
    // Let any (wrong) save attempt flush before asserting none happened.
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(
      apiPostMocked.mock.calls.some((c) => c[0] === '/api/settings'),
    ).toBe(false);
  });
});
