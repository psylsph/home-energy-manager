/**
 * Precision coverage for the tariff Rate entry field in Settings.
 *
 * The rate input renders the stored £/kWh value as pence to 3 decimal places
 * (matching the most precise field in the app) and uses step="0.001" so a
 * sub-penny value like 12.345p passes HTML number-input validation. This
 * hydrates Settings with a 0.12345 £/kWh rate and asserts the rendered input
 * shows "12.345" at step 0.001.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, waitFor } from '@testing-library/react';

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
          // 0.12345 £/kWh = 12.345p — needs 3dp.
          import_tariff_config: { slots: [{ start: '00:00', end: '23:59', rate: 0.12345 }] },
          export_tariff_config: { slots: [{ start: '00:00', end: '23:59', rate: 0.15 }] },
          evc_host: '',
          import_standing_charge_p_per_day: 0,
        },
      };
    }
    if (path === '/api/alerts') {
      return { ok: true, data: { config: { enabled: false, telegram: { bot_token: '', chat_id: '', enabled: false }, ntfy: { topic: '', server: 'https://ntfy.sh', enabled: false }, thresholds: {} } } };
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

describe('<SettingsPage/> — tariff Rate field is 3dp', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    useInverterStore.setState({
      developerMode: false,
      inverterTempConfig: null,
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it('renders the import rate to 3dp (12.345p) with a 0.001 step', async () => {
    render(<SettingsPage />);
    // Import editor is rendered before Export, so its Rate input is first.
    const rateInputs = await screen.findAllByLabelText(/Rate \(p\/kWh\)/i);
    const importRate = rateInputs[0] as HTMLInputElement;
    await waitFor(() => {
      expect(importRate.value).toBe('12.345');
    });
    expect(importRate.getAttribute('step')).toBe('0.001');
  });
});
