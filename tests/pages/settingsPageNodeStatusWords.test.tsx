import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

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
          config: {
            enabled: false,
            latitude: null,
            longitude: null,
            update_interval_mins: 30,
          },
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

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

function toggleFor(label: string): HTMLElement {
  const labelEl = Array.from(document.body.querySelectorAll('span')).find(
    (s) => s.textContent?.trim() === label,
  );
  if (!labelEl) throw new Error(`label not found: ${label}`);
  const row = labelEl.parentElement?.parentElement;
  if (!row) throw new Error(`row not found for label: ${label}`);
  const toggle = row.querySelector('div.cursor-pointer');
  if (!toggle) throw new Error(`toggle not found for label: ${label}`);
  return toggle as HTMLElement;
}

describe('<SettingsPage/> — Energy Flow diagram node status words', () => {
  beforeEach(() => {
    silenceConsoleError();
    localStorage.removeItem('showFlowStatusWords');
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.removeItem('showFlowStatusWords');
  });

  it('defaults node status words to ON so non-technical users see Generating / Importing / Charging by default', async () => {
    // The status words carry the direction signal that used to live in a
    // `+`/`-` prefix on the orbit node value (e.g. "-839W + Discharging"
    // used to read as a bug). Now that the orbit and BatteryPanel show
    // plain magnitudes, the words are the primary direction signal, so
    // they're on by default. The toggle remains in Settings so a user
    // who prefers the bare value can still turn them off.
    //
    // We force a fresh module load so the store re-reads localStorage and
    // picks up the (absent) key; otherwise the cached store from a prior
    // test would carry whatever state the previous test set it to.
    vi.resetModules();
    const { default: FreshSettingsPage } = await import('../../src/pages/SettingsPage');
    const { useInverterStore: freshStore } = await import('../../src/store/useInverterStore');

    render(<FreshSettingsPage />);
    await screen.findByText('Show Node Status Words');

    expect(freshStore.getState().showFlowStatusWords).toBe(true);
    expect(localStorage.getItem('showFlowStatusWords')).toBeNull();
  });

  it('clicking the node-status toggle shows/hides node words, persisting each choice', async () => {
    // Fresh-load the store so it picks up the absent localStorage key and
    // initializes to the ON default, then exercise the toggle in both
    // directions.
    vi.resetModules();
    const { default: FreshSettingsPage } = await import('../../src/pages/SettingsPage');
    const { useInverterStore: freshStore } = await import('../../src/store/useInverterStore');

    render(<FreshSettingsPage />);
    await screen.findByText('Show Node Status Words');

    // The toggle starts checked (default-on). First click turns it off.
    fireEvent.click(toggleFor('Show Node Status Words'));

    await waitFor(() => {
      expect(freshStore.getState().showFlowStatusWords).toBe(false);
      expect(localStorage.getItem('showFlowStatusWords')).toBe('false');
    });

    // Second click re-enables.
    fireEvent.click(toggleFor('Show Node Status Words'));

    await waitFor(() => {
      expect(freshStore.getState().showFlowStatusWords).toBe(true);
      expect(localStorage.getItem('showFlowStatusWords')).toBe('true');
    });
  });
});
