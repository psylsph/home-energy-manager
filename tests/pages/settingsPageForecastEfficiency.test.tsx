import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Issue #283: the Forecast battery-efficiency inputs live in the Solar
// section. They hydrate from GET /api/settings (shown as whole percents),
// round-trip through the solar-arrays save (converted back to 0-1), and
// out-of-range values block the save client-side.
// ---------------------------------------------------------------------------

const apiGetMock = vi.fn(async (path: string) => {
  if (path === '/api/settings') {
    return {
      ok: true,
      data: {
        host: '',
        port: 8899,
        interval_secs: 20,
        pv1_rated_kw: 5,
        pv2_rated_kw: 0,
        solar_arrays: [],
        forecast_charge_efficiency: 0.9,
        forecast_discharge_efficiency: 0.95,
      },
    };
  }
  if (path === '/api/weather') {
    return { ok: true, data: { config: { enabled: false } } };
  }
  return { ok: true, data: {} };
});

const postedBodies: unknown[] = [];
const apiPostMock = vi.fn(async (_path: string, body: unknown) => {
  postedBodies.push(body);
  return { ok: true };
});

vi.mock('../../src/lib/api', () => ({
  apiGet: (path: string) => apiGetMock(path),
  apiPost: (path: string, body: unknown) => apiPostMock(path, body),
  getApiBase: () => 'http://127.0.0.1:7337',
}));

import SettingsPage from '../../src/pages/SettingsPage';

describe('SettingsPage forecast efficiencies', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    postedBodies.length = 0;
  });

  afterEach(() => {
    cleanup();
  });

  it('hydrates the efficiency inputs as percentages', async () => {
    render(<SettingsPage />);
    const charge = await waitFor(() => screen.getByTestId('forecast-charge-eff-input'));
    expect((charge as HTMLInputElement).value).toBe('90');
    expect(
      (screen.getByTestId('forecast-discharge-eff-input') as HTMLInputElement).value,
    ).toBe('95');
  });

  it('round-trips edited values through the solar-arrays save', async () => {
    render(<SettingsPage />);
    const charge = await waitFor(() => screen.getByTestId('forecast-charge-eff-input'));
    fireEvent.change(charge, { target: { value: '85' } });
    fireEvent.change(screen.getByTestId('forecast-discharge-eff-input'), {
      target: { value: '92' },
    });
    fireEvent.click(screen.getByTestId('solar-arrays-save'));
    await waitFor(() => expect(apiPostMock).toHaveBeenCalled());
    const body = postedBodies[0] as Record<string, unknown>;
    expect(body.forecast_charge_efficiency).toBe(0.85);
    expect(body.forecast_discharge_efficiency).toBe(0.92);
  });

  it('blocks the save for out-of-range efficiencies', async () => {
    render(<SettingsPage />);
    const charge = await waitFor(() => screen.getByTestId('forecast-charge-eff-input'));
    fireEvent.change(charge, { target: { value: '120' } });
    fireEvent.click(screen.getByTestId('solar-arrays-save'));
    await waitFor(() =>
      expect(screen.getByTestId('settings-flash').textContent).toMatch(/50/i),
    );
    expect(apiPostMock).not.toHaveBeenCalled();
  });
});
