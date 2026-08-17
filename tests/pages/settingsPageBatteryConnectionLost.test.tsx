import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Mock — follows settingsPageSolarArrays.test.tsx exactly.
// ---------------------------------------------------------------------------

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/settings') {
      return {
        ok: true,
        data: {
          host: '192.168.1.50', port: 8899, serial: 'TEST123',
          interval_secs: 20, http_port: 7337, evc_port: 502,
          import_tariff_config: null, export_tariff_config: null, evc_host: '',
          pv1_rated_kw: 3.8, pv2_rated_kw: 1.3,
          solar_arrays: [],
        },
      };
    }
    if (path === '/api/alerts') {
      // Flat AlertsConfig shape — mirrors SettingsPage.test.tsx alertConfig().
      return {
        ok: true,
        data: {
          config: {
            enabled: true,
            telegram_bot_token: '',
            telegram_chat_id: '',
            cooldown_minutes: 30,
            batt_temp_min: 0, batt_temp_max: 0,
            inverter_temp_min: 8, inverter_temp_max: 60,
            soc_min: 4, soc_max: 100,
            grid_offline_enabled: false, inverter_trip_enabled: false,
            battery_over_temp_enabled: false,
            battery_connection_lost_enabled: true,
            connection_lost_enabled: false,
            solar_clipping_enabled: false, solar_clipping_ceiling_w: 0,
            ntfy_topic: '', ntfy_server: 'https://ntfy.sh',
            pushover_app_token: '', pushover_user_key: '',
          },
        },
      };
    }
    if (path === '/api/weather') {
      return { ok: true, data: { config: { enabled: false, latitude: null, longitude: null, update_interval_mins: 30 }, current: null, history: [] } };
    }
    if (path === '/api/status') return { ok: true, lan_ip: null, clients: [], client_count: 0 };
    if (path === '/api/discover') return { ok: true, subnets: [], inverters: [] };
    if (path === '/api/evc/discover') return { ok: true, subnets: [], chargers: [] };
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

import SettingsPage from '../../src/pages/SettingsPage';
import { useInverterStore } from '../../src/store/useInverterStore';

function silenceConsoleError() {
  vi.spyOn(console, 'error').mockImplementation(() => {});
}

describe('<SettingsPage/> — Battery Connection Lost toggle wording (issue #272)', () => {
  beforeEach(() => {
    silenceConsoleError();
    useInverterStore.setState({ gridLineWeight: 'standard' });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    useInverterStore.setState({ developerMode: false });
    cleanup();
  });

  it('describes both detection modes: BMS silence and DC breaker trip', async () => {
    render(<SettingsPage />);
    // The row is always rendered once alerts config loads (no collapsible).
    const label = await screen.findByText('Battery Connection Lost');
    // The description block is inside the same <span> parent.
    const row = label.closest('span') ?? label.parentElement;
    expect(row).not.toBeNull();
    const text = (row as HTMLElement).textContent ?? '';
    // Covers the per-battery BMS-silence mode…
    expect(text).toMatch(/stops responding/i);
    // …and the breaker-trip voltage-mismatch mode added for issue #272.
    expect(text).toMatch(/breaker/i);
    expect(text).toMatch(/voltage collapses/i);
  });
});
