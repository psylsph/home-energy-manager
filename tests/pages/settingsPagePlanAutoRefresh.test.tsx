import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// The Forecast plan auto-refresh toggle (issue #283 scheduling): opt-in
// nightly re-sizing of charge slot 1 from the live SOC. The toggle saves
// immediately (it hands charge slot 1 to the planner), so its contract is
// optimistic-on/off with revert-on-failure — same shape as the
// update-checking toggle.
// ---------------------------------------------------------------------------

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/settings') {
      return {
        ok: true,
        data: {
          host: '',
          port: 8899,
          serial: '',
          interval_secs: 20,
          http_port: 7337,
          evc_port: 502,
          import_tariff_config: null,
          export_tariff_config: null,
          evc_host: '',
          ...hydratedSettings(),
        },
      };
    }
    if (path === '/api/alerts') {
      return {
        ok: true,
        data: {
          config: {
            enabled: false,
            telegram: { bot_token: '', chat_id: '', enabled: false },
            ntfy: { topic: '', server: 'https://ntfy.sh', enabled: false },
            thresholds: {},
          },
        },
      };
    }
    if (path === '/api/weather') {
      return {
        ok: true,
        data: {
          config: { enabled: false, latitude: null, longitude: null, update_interval_mins: 30 },
          current: null,
          history: [],
        },
      };
    }
    if (path === '/api/status') {
      return { ok: true, lan_ip: null, clients: [], client_count: 0 };
    }
    if (path === '/api/discover') {
      return { ok: true, subnets: [], inverters: [] };
    }
    if (path === '/api/evc/discover') {
      return { ok: true, subnets: [], chargers: [] };
    }
    return { ok: true, data: {} };
  }),
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  fetchHistory: vi.fn().mockResolvedValue({}),
  isTauri: false,
}));

vi.mock('../../src/lib/openExternal', () => ({
  openExternal: vi.fn().mockResolvedValue(undefined),
}));

// Set per-test BEFORE rendering: what GET /api/settings reports.
const { hydratedSettings, setHydratedSettings } = vi.hoisted(() => {
  let value: Record<string, unknown> = {};
  return {
    hydratedSettings: () => value,
    setHydratedSettings: (v: Record<string, unknown>) => {
      value = v;
    },
  };
});

// Imported after the vi.mock() calls above (factories are hoisted regardless).
import SettingsPage from '../../src/pages/SettingsPage';
import { apiPost } from '../../src/lib/api';

const apiPostMocked = vi.mocked(apiPost);

describe('<SettingsPage/> — Forecast plan auto-refresh toggle', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    setHydratedSettings({});
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it('renders off by default and explains the slot-ownership trade-off', async () => {
    render(<SettingsPage />);
    const toggle = await screen.findByRole('switch', { name: 'Auto-refresh charge plan' });
    expect(toggle.getAttribute('aria-checked')).toBe('false');
    expect(
      screen.getByText(/Takes ownership of charge slot 1/),
    ).toBeDefined();
    expect(screen.getByTestId('forecast-plan-auto-refresh-state').textContent).toBe('Off');
  });

  it('reflects the saved setting when enabled', async () => {
    setHydratedSettings({ forecast_plan_auto_refresh: true });
    render(<SettingsPage />);
    const toggle = await screen.findByRole('switch', { name: 'Auto-refresh charge plan' });
    await waitFor(() => {
      expect(toggle.getAttribute('aria-checked')).toBe('true');
    });
    expect(
      screen.getByTestId('forecast-plan-auto-refresh-state').textContent,
    ).toMatch(/managed by the planner/);
  });

  it('saves immediately when switched on', async () => {
    render(<SettingsPage />);
    const toggle = await screen.findByRole('switch', { name: 'Auto-refresh charge plan' });
    fireEvent.click(toggle);
    await waitFor(() => {
      const post = apiPostMocked.mock.calls.find((c) => c[0] === '/api/settings');
      expect(post?.[1]).toEqual({ forecast_plan_auto_refresh: true });
    });
    await waitFor(() => {
      expect(toggle.getAttribute('aria-checked')).toBe('true');
    });
  });

  it('reverts the switch without saving state when the save fails', async () => {
    apiPostMocked.mockRejectedValueOnce(new Error('backend offline'));
    render(<SettingsPage />);
    const toggle = await screen.findByRole('switch', { name: 'Auto-refresh charge plan' });
    fireEvent.click(toggle);
    await waitFor(() => {
      // The optimistic flip is rolled back so the UI never claims the
      // planner owns the slot when the backend says otherwise.
      expect(toggle.getAttribute('aria-checked')).toBe('false');
    });
    expect(screen.getByText(/Failed to update plan auto-refresh/)).toBeDefined();
  });
});
