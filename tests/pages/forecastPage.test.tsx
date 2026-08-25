import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// ForecastPage (issue #283 Phase 1): the summary card renders the payload's
// headline numbers, degradation statuses render as explanations (never
// zeros pretending to be predictions), and the battery projection is
// omitted cleanly when the backend reports no battery.
// ---------------------------------------------------------------------------

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/forecast') {
      return { ok: true, data: fullPayload() };
    }
    return { ok: true, data: {} };
  }),
}));

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
import { apiGet } from '../../src/lib/api';

const apiGetMock = vi.mocked(apiGet);

function fullPayload() {
  return {
    generated_at: 1_700_000_000,
    status: [],
    performance_ratio: 0.8,
    performance_ratio_days: 12,
    solar: [
      { timestamp: 1_700_003_600, kwh: 2.0, band_low: 1.6, band_high: 2.4 },
    ],
    solar_today_remaining_kwh: 10,
    solar_tomorrow_kwh: 18.4,
    consumption: Array.from({ length: 24 }, (_, hour) => ({
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
  };
}

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
