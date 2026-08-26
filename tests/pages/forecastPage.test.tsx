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
    consumption: Array.from({ length: 24 }, (_: number, hour: number) => ({
      hour,
      kwh: 0.5,
      p25: 0.4,
      p75: 0.6,
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
            window: { start: '02:00', end: '05:00', rate: 0.09, tomorrow: true },
            kwh: 3.2,
            min_soc_pct: 20,
            observed_min_soc_pct: 4,
            after_min_soc_pct: 80,
            charge_target_soc_pct: 80,
            current_soc_pct: 25,
            rationale: 'Battery is at 25% now and solar leaves it at 30%. Charging 3.2 kWh in the 9.0p window lifts it to 80%.',
            // Real planner payload (planner v2): the trajectory when the
            // recommended charge is applied to every window occurrence —
            // the Battery tab chart renders this as a dashed overlay on
            // top of the recorded SOC history.
            with_charge_series: [
              [1_700_003_600, 30],
              [1_700_007_200, 35],
              [1_700_010_800, 40],
            ],
          },
          apply: {
            charge_slot: {
              slot: 1,
              enabled: true,
              start_hour: 2,
              start_minute: 0,
              end_hour: 5,
              end_minute: 0,
              target_soc: 80,
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
  AreaChart: () => <div />,
  Area: () => <div />,
  Line: () => <div />,
  LineChart: () => <div />,
  XAxis: () => <div />,
  YAxis: () => <div />,
  Tooltip: () => <div />,
  CartesianGrid: () => <div />,
  ReferenceLine: () => <div />,
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
    expect(screen.getByTestId('forecast-plan').textContent).toMatch(/05:00/);
    const apply = screen.getByTestId('forecast-plan-apply');
    await waitFor(() => {
      fireEvent.click(apply);
    });
    // apply-charge-slot: the slot is dispatched first.
    const slotCall = apiPostMocked.mock.calls.find((c) => c[0] === '/api/control/charge-slot');
    expect(slotCall).toBeTruthy();
    const timedCall = apiPostMocked.mock.calls.find((c) => c[0] === '/api/control/timed-charge');
    expect(timedCall).toBeTruthy();
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
    expect(container.textContent).toMatch(/05:00/);
    expect(container.textContent).toMatch(/3\.2 kWh/);
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
