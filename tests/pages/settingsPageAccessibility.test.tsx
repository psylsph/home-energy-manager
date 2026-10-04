import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Every toggle on the Settings page must be a real switch: a role, an
// accessible name, a checked state, and operable from the keyboard. Only four
// of nineteen were, so a screen reader could not identify "Enable Alerts" or
// "Developer Mode", and nothing could be switched without a pointer.
//
// Also: the enable switches' error handlers must cope with a request that
// rejects with something that is not an Error (null / undefined), instead of
// throwing from inside the catch.
// ---------------------------------------------------------------------------

const apiGetMock = vi.fn();
const apiPostMock = vi.fn();

vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGetMock(...(args as [string])),
  apiPost: (...args: unknown[]) => apiPostMock(...(args as [string, unknown])),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  isTauri: false,
}));
vi.mock('../../src/lib/openExternal', () => ({ openExternal: vi.fn() }));
vi.mock('@tauri-apps/plugin-autostart', () => ({ enable: vi.fn(), disable: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

import SettingsPage from '../../src/pages/SettingsPage';
import { useInverterStore } from '../../src/store/useInverterStore';

const tariff = () => ({ slots: [{ start: '00:00', end: '23:59', rate: 0.15 }], version: 2 });

function mount() {
  apiGetMock.mockImplementation(async (path: string) => {
    if (path === '/api/settings') {
      return {
        ok: true,
        data: {
          host: '192.168.1.10', port: 8899, serial: 'SA12345678', interval_secs: 20, http_port: 7337,
          evc_host: '', evc_port: 502, disable_auto_discovery: true, autostart_enabled: false,
          minimise_to_tray: false, start_minimised: false, check_for_updates: true, api_key: '', api_port: 7338,
          hidden_panels: [], solar_arrays: [], import_tariff_config: tariff(), export_tariff_config: tariff(),
          octopus_enabled: false, octopus_account_number: '', octopus_api_key_configured: false,
          octopus_gas_unit: 'unknown', octopus_economy7_start: '00:30', octopus_economy7_end: '07:30',
        },
      };
    }
    if (path === '/api/alerts') {
      // Enabled, so the alert-trigger switches are on the page too.
      return { ok: true, data: { config: { enabled: true } } };
    }
    if (path === '/api/weather') {
      return {
        ok: true,
        data: {
          config: { enabled: true, latitude: 51.5, longitude: -0.12, update_interval_mins: 30, postcode: 'SW1A 1AA' },
          current: null, history: [], backfill_in_progress: false,
        },
      };
    }
    if (path === '/api/status') return { ok: true, lan_ip: null, clients: [], client_count: 0 };
    return { ok: true, data: {} };
  });
  apiPostMock.mockResolvedValue({ ok: true, message: 'Saved' });
}

const tauriWindow = window as unknown as { __TAURI_INTERNALS__?: unknown };

/** Every toggle on the page, by the name a screen reader should announce. */
const TOGGLES = [
  'Enable Auto-Discovery',
  'Allow battery control through the authenticated API',
  'Start on Login',
  'Minimise to Tray',
  'Start Hidden in Tray',
  'Enable Weather',
  'Enable Alerts',
  'Grid Offline',
  'Inverter Trip',
  'Inverter Battery Warning',
  'Solar Clipping',
  'Battery Connection Lost',
  'Connection Lost',
  'Show Graphs',
  'Lock Y-axis scale',
  'Show average lines on History charts',
  'Show Node Status Words',
  'Developer Mode',
  'Check for new releases',
];

beforeEach(() => {
  vi.spyOn(console, 'error').mockImplementation(() => {});
  vi.spyOn(console, 'warn').mockImplementation(() => {});
  apiGetMock.mockReset();
  apiPostMock.mockReset();
  tauriWindow.__TAURI_INTERNALS__ = {};
  useInverterStore.setState({
    snapshot: null,
    connectionState: 'disconnected',
    connectedHost: null,
    // The authenticated-API switch only shows in developer mode.
    developerMode: true,
    evcHost: '',
    panelGraphsEnabled: false,
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  cleanup();
  delete tauriWindow.__TAURI_INTERNALS__;
});

async function open() {
  mount();
  render(<SettingsPage />);
  await screen.findByRole('heading', { name: 'Notifications' });
  // Wait for the alert triggers (shown once the saved config says enabled).
  await screen.findByText('Alert Triggers & Cooldown');
}

describe('<SettingsPage/> — toggles are real switches', () => {
  it('exposes every toggle as a named switch', async () => {
    await open();
    for (const name of TOGGLES) {
      expect(screen.getByRole('switch', { name }), name).toBeInTheDocument();
    }
  });

  it('leaves no toggle without a role', async () => {
    await open();
    const bare = Array.from(document.querySelectorAll('div.cursor-pointer.shrink-0')).filter(
      (el) => el.getAttribute('role') !== 'switch',
    );
    expect(bare).toHaveLength(0);
  });

  it('reports each switch state through aria-checked', async () => {
    await open();
    expect(screen.getByRole('switch', { name: 'Enable Weather' })).toHaveAttribute('aria-checked', 'true');
    expect(screen.getByRole('switch', { name: 'Enable Alerts' })).toHaveAttribute('aria-checked', 'true');
    expect(screen.getByRole('switch', { name: 'Start on Login' })).toHaveAttribute('aria-checked', 'false');
    expect(screen.getByRole('switch', { name: 'Developer Mode' })).toHaveAttribute('aria-checked', 'true');
  });

  it('can be reached with the keyboard', async () => {
    await open();
    for (const name of TOGGLES) {
      expect(screen.getByRole('switch', { name }).getAttribute('tabindex'), name).toBe('0');
    }
  });

  it('flips with Space', async () => {
    await open();
    const dev = () => screen.getByRole('switch', { name: 'Developer Mode' });
    expect(useInverterStore.getState().developerMode).toBe(true);

    fireEvent.keyDown(dev(), { key: ' ' });
    expect(useInverterStore.getState().developerMode).toBe(false);
    expect(dev()).toHaveAttribute('aria-checked', 'false');
  });

  it('turns a switch on with Enter', async () => {
    await open();
    useInverterStore.setState({ panelGraphsEnabled: false });
    const graphs = screen.getByRole('switch', { name: 'Show Graphs' });
    fireEvent.keyDown(graphs, { key: 'Enter' });
    expect(useInverterStore.getState().panelGraphsEnabled).toBe(true);
  });

  it('does not flip on an unrelated key', async () => {
    await open();
    const graphs = screen.getByRole('switch', { name: 'Show Graphs' });
    // One key at a time: two flips would cancel out and hide the bug.
    for (const key of ['a', 'Tab', 'Escape', 'ArrowRight']) {
      fireEvent.keyDown(graphs, { key });
      expect(useInverterStore.getState().panelGraphsEnabled, key).toBe(false);
    }
  });

  it('still flips on a click', async () => {
    await open();
    fireEvent.click(screen.getByRole('switch', { name: 'Show Graphs' }));
    expect(useInverterStore.getState().panelGraphsEnabled).toBe(true);
  });

  it('saves Enable Weather through its labelled switch', async () => {
    await open();
    fireEvent.click(screen.getByRole('switch', { name: 'Enable Weather' }));
    await waitFor(() => expect(apiPostMock).toHaveBeenCalledWith('/api/weather', { enabled: false }));
  });
});

describe('<SettingsPage/> — enable switches survive a rejection that is not an Error', () => {
  for (const [label, reason] of [['null', null], ['undefined', undefined]] as const) {
    it(`Enable Alerts: a ${label} rejection shows a generic message`, async () => {
      await open();
      apiPostMock.mockImplementation(async (p: string) => {
        if (p === '/api/alerts') return Promise.reject(reason);
        return { ok: true, message: 'Saved' };
      });

      fireEvent.click(screen.getByRole('switch', { name: 'Enable Alerts' }));

      await screen.findByText('Failed to save');
    });

    it(`Enable Weather: a ${label} rejection shows a generic message`, async () => {
      await open();
      apiPostMock.mockImplementation(async (p: string) => {
        if (p === '/api/weather') return Promise.reject(reason);
        return { ok: true, message: 'Saved' };
      });

      fireEvent.click(screen.getByRole('switch', { name: 'Enable Weather' }));

      await screen.findByText('Failed to save');
    });
  }

  for (const [label, reason] of [['null', null], ['undefined', undefined]] as const) {
    it(`Enable Auto-Discovery: a ${label} rejection shows a generic message`, async () => {
      await open();
      apiPostMock.mockImplementation(async (p: string) => {
        if (p === '/api/settings') return Promise.reject(reason);
        return { ok: true, message: 'Saved' };
      });

      fireEvent.click(screen.getByRole('switch', { name: 'Enable Auto-Discovery' }));

      await screen.findByText('Failed to save');
    });
  }

  it('Enable Alerts still shows the message of a real Error', async () => {
    await open();
    apiPostMock.mockImplementation(async (p: string) => {
      if (p === '/api/alerts') throw new Error('disk full');
      return { ok: true, message: 'Saved' };
    });
    fireEvent.click(screen.getByRole('switch', { name: 'Enable Alerts' }));
    await screen.findByText('disk full');
  });
});
