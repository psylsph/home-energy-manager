import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, within } from '@testing-library/react';

// ---------------------------------------------------------------------------
// SettingsPage is a large form page. Existing tests (settingsPageGridLines,
// InverterAddress, NodeStatusWords, StandingCharge) cover specific
// sub-sections in depth. This file adds broad coverage of the page shell:
// the loading gate, settings hydration from /api/settings, the connection
// state badge, section headings, the discovery flow, the interval select
// clamp, the connect save round-trip, and developer-only section visibility.
// ---------------------------------------------------------------------------

type SettingsShape = Record<string, unknown>;

const apiGetMock = vi.fn();
const apiPostMock = vi.fn();

vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGetMock(...(args as [string])),
  apiPost: (...args: unknown[]) => apiPostMock(...(args as [string, unknown])),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  isTauri: false,
}));

import SettingsPage from '../../src/pages/SettingsPage';
import { useInverterStore } from '../../src/store/useInverterStore';

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

function defaultSettings(overrides: SettingsShape = {}): SettingsShape {
  return {
    host: '192.168.1.10',
    port: 8899,
    serial: 'SA12345678',
    interval_secs: 20,
    http_port: 7337,
    evc_host: '',
    evc_port: 502,
    disable_auto_discovery: false,
    autostart_enabled: false,
    api_key: '',
    api_port: 7338,
    hidden_panels: [],
    import_tariff_config: null,
    export_tariff_config: null,
    import_tariff: null,
    export_tariff: null,
    import_standing_charge_p_per_day: null,
    ...overrides,
  };
}

/**
 * Default /api/* responses for every endpoint SettingsPage hits on mount.
 * Tests can override individual responses via apiGetMock.mockImplementation.
 */
function alertConfig(overrides: SettingsShape = {}): SettingsShape {
  return {
    enabled: false,
    telegram_bot_token: '',
    telegram_chat_id: '',
    cooldown_minutes: 30,
    batt_temp_min: 0,
    batt_temp_max: 0,
    inverter_temp_min: 8,
    inverter_temp_max: 60,
    soc_min: 4,
    soc_max: 100,
    grid_offline_enabled: false,
    inverter_trip_enabled: false,
    battery_over_temp_enabled: false,
    connection_lost_enabled: false,
    solar_clipping_enabled: false,
    solar_clipping_ceiling_w: 0,
    ntfy_topic: '',
    ntfy_server: 'https://ntfy.sh',
    pushover_app_token: '',
    pushover_user_key: '',
    ...overrides,
  };
}

function mountApiMocks(settingsOverrides: SettingsShape = {}) {
  apiGetMock.mockImplementation(async (path: string) => {
    if (path === '/api/settings') {
      return { ok: true, data: defaultSettings(settingsOverrides) };
    }
    if (path === '/api/alerts') {
      return {
        ok: true,
        data: {
          config: alertConfig(),
        },
      };
    }
    if (path === '/api/weather') {
      return {
        ok: true,
        data: {
          config: { enabled: false, latitude: null, longitude: null, update_interval_mins: 30, postcode: '' },
          current: null,
          history: [],
          backfill_in_progress: false,
        },
      };
    }
    if (path === '/api/status') {
      return { ok: true, lan_ip: '192.168.1.50', clients: [], client_count: 0 };
    }
    if (path === '/api/discover') {
      return { ok: true, subnets: ['192.168.1'], inverters: [] };
    }
    if (path === '/api/evc/discover') {
      return { ok: true, subnets: [], chargers: [] };
    }
    return { ok: true, data: {} };
  });
  apiPostMock.mockResolvedValue({ ok: true });
}

describe('<SettingsPage/> — page shell & hydration', () => {
  beforeEach(() => {
    silenceConsoleError();
    apiGetMock.mockReset();
    apiPostMock.mockReset();
    useInverterStore.setState({
      snapshot: null,
      connectionState: 'disconnected',
      connectedHost: null,
      developerMode: false,
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  describe('loading gate', () => {
    it('shows the loading spinner before /api/settings resolves', () => {
      apiGetMock.mockImplementation(() => new Promise(() => {}));
      const { container } = render(<SettingsPage />);
      expect(container.querySelector('.animate-spin')).not.toBeNull();
      expect(container.textContent).toContain('Loading settings');
    });

    it('replaces the spinner with the form once settings load', async () => {
      mountApiMocks();
      render(<SettingsPage />);
      expect(await screen.findByText('Inverter Connection')).toBeDefined();
    });

    it('still renders the form when settings load fails (graceful degrade)', async () => {
      mountApiMocks();
      apiGetMock.mockImplementation(async (path: string) => {
        if (path === '/api/settings') throw new Error('boom');
        // fall through to defaults for the other endpoints
        if (path === '/api/alerts') return { ok: true, data: { config: {} } };
        if (path === '/api/weather') return { ok: true, data: { config: {} } };
        if (path === '/api/status') return { ok: true, lan_ip: null, clients: [] };
        return { ok: true, data: {} };
      });
      render(<SettingsPage />);
      expect(await screen.findByText('Inverter Connection')).toBeDefined();
    });
  });

  describe('hydration', () => {
    it('prefills the inverter address from /api/settings', async () => {
      mountApiMocks({ host: '10.0.0.5' });
      render(<SettingsPage />);
      const input = await screen.findByPlaceholderText('e.g. 192.168.1.50') as HTMLInputElement;
      expect(input.value).toBe('10.0.0.5');
    });

    it('prefills the serial from /api/settings', async () => {
      mountApiMocks({ serial: 'SA999' });
      render(<SettingsPage />);
      const input = await screen.findByPlaceholderText('Leave blank to auto-detect') as HTMLInputElement;
      expect(input.value).toBe('SA999');
    });

    it('clamps an out-of-range poll interval to a valid step', async () => {
      mountApiMocks({ interval_secs: 33 });
      render(<SettingsPage />);
      // The interval is rendered as a row of buttons labelled with the
      // step in seconds. 33 clamps to the nearest lower step (30), so the
      // 30s button is the active one.
      await screen.findByText('Refresh Interval');
      const thirtyBtn = screen.getByText('30s');
      expect(thirtyBtn.className).toContain('flow-active');
    });

    it('hydrates the standing charge when set', async () => {
      mountApiMocks({ import_standing_charge_p_per_day: 54.86 });
      render(<SettingsPage />);
      const input = await screen.findByPlaceholderText('e.g. 54.86') as HTMLInputElement;
      expect(input.value).toBe('54.86');
    });
  });

  describe('connection state badge', () => {
    it('shows the current connection state', async () => {
      mountApiMocks();
      useInverterStore.setState({ connectionState: 'connected' });
      render(<SettingsPage />);
      expect(await screen.findByText('connected')).toBeDefined();
    });

    it('shows the connected host next to the state', async () => {
      mountApiMocks();
      useInverterStore.setState({
        connectionState: 'connected',
        connectedHost: '192.168.1.10:8899',
      });
      render(<SettingsPage />);
      expect(await screen.findByText('— 192.168.1.10:8899')).toBeDefined();
    });
  });

  describe('section headings', () => {
    it('renders all the always-visible sections', async () => {
      mountApiMocks();
      render(<SettingsPage />);
      for (const heading of [
        'Inverter Connection',
        'Remote / Mobile Network Access',
        'App',
        'Energy Tariffs',
        'Panel Controls',
        'EV Charger',
        'About',
      ]) {
        expect(await screen.findByText(heading)).toBeDefined();
      }
    });

    it('shows the Apple Watch / mini display GUI URL', async () => {
      // The mini page is a tiny self-contained glance view the user opens
      // in a browser (phone/watch). It must be copyable and point at /mini.
      mountApiMocks();
      render(<SettingsPage />);
      // /api/status is mocked to return lan_ip 192.168.1.50, so lanUrl
      // builds from that rather than the getApiBase() fallback.
      const miniUrl = await screen.findByText(
        'http://192.168.1.50:7337/mini',
      );
      expect(miniUrl).toBeDefined();
      // The helper line names the use case so a user can find it.
      expect(
        await screen.findByText(/mini display/),
      ).toBeDefined();
    });
  });

  describe('discover', () => {
    /** Scope a query to the Inverter Connection section so it doesn't
     *  collide with the EV Charger section's identically-labelled buttons. */
    async function inverterSection(): Promise<HTMLElement> {
      const heading = await screen.findByText('Inverter Connection');
      let el: HTMLElement | null = heading;
      while (el && el.tagName.toLowerCase() !== 'section') el = el.parentElement;
      return el ?? heading;
    }

    it('lists discovered inverters', async () => {
      mountApiMocks();
      apiGetMock.mockImplementation(async (path: string) => {
        if (path === '/api/discover') {
          return {
            ok: true,
            subnets: ['192.168.1'],
            inverters: [{ host: '192.168.1.99', port: 8899, serial: 'SA111' }],
          };
        }
        if (path === '/api/settings') return { ok: true, data: defaultSettings() };
        if (path === '/api/alerts') return { ok: true, data: { config: {} } };
        if (path === '/api/weather') return { ok: true, data: { config: {} } };
        if (path === '/api/status') return { ok: true, lan_ip: null, clients: [] };
        return { ok: true, data: {} };
      });
      render(<SettingsPage />);
      const section = await inverterSection();

      const discoverBtn = within(section).getByRole('button', { name: 'Scan Network' });
      fireEvent.click(discoverBtn);

      await waitFor(() => {
        expect(within(section).getByText('192.168.1.99:8899')).toBeDefined();
      });
    });

    it('shows the no-inverters message when discovery finds none', async () => {
      mountApiMocks();
      render(<SettingsPage />);
      const section = await inverterSection();
      fireEvent.click(within(section).getByRole('button', { name: 'Scan Network' }));
      await waitFor(() => {
        expect(within(section).getByText(/No inverters found/)).toBeDefined();
      });
    });
  });

  describe('connect save round-trip', () => {
    /** Scope a query to the Inverter Connection section (same reason as
     *  the discover tests above — identical button labels elsewhere). */
    async function inverterSection(): Promise<HTMLElement> {
      const heading = await screen.findByText('Inverter Connection');
      let el: HTMLElement | null = heading;
      while (el && el.tagName.toLowerCase() !== 'section') el = el.parentElement;
      return el ?? heading;
    }

    it('posts the host to /api/settings when Connect is clicked', async () => {
      mountApiMocks({ host: '' });
      render(<SettingsPage />);
      const input = await screen.findByPlaceholderText('e.g. 192.168.1.50') as HTMLInputElement;
      fireEvent.change(input, { target: { value: '192.168.1.20' } });

      const section = await inverterSection();
      const connectBtn = within(section).getByRole('button', { name: /^Connect$/ });
      fireEvent.click(connectBtn);

      await waitFor(() => {
        expect(apiPostMock).toHaveBeenCalledWith('/api/settings', expect.objectContaining({ host: '192.168.1.20' }));
      });
    });
  });

  describe('notifications temperature thresholds', () => {
    function mountWithAlerts(snapshot: unknown = null, overrides: SettingsShape = {}) {
      mountApiMocks();
      apiGetMock.mockImplementation(async (path: string) => {
        if (path === '/api/settings') return { ok: true, data: defaultSettings() };
        if (path === '/api/alerts') return { ok: true, data: { config: alertConfig({ enabled: true, ...overrides }) } };
        if (path === '/api/weather') return { ok: true, data: { config: {} } };
        if (path === '/api/status') return { ok: true, lan_ip: null, clients: [] };
        return { ok: true, data: {} };
      });
      useInverterStore.setState({ snapshot: snapshot as never });
      render(<SettingsPage />);
    }

    function inputAfterText(text: string): HTMLInputElement {
      const el = screen.getByText(text);
      const input = el.closest('label')?.querySelector('input');
      if (!input) throw new Error(`input not found for ${text}`);
      return input;
    }

    it('shows separate battery and inverter temperature bounds for non-Gateway devices', async () => {
      mountWithAlerts({ device_type_code: '2001' });
      expect(await screen.findByText('Temperature & SOC')).toBeDefined();
      expect(screen.getByText('Battery temp below °C')).toBeDefined();
      expect(screen.getByText('Battery temp above °C')).toBeDefined();
      expect(screen.getByText('Inverter temp below °C')).toBeDefined();
      expect(screen.getByText('Inverter temp above °C')).toBeDefined();
      expect(inputAfterText('Inverter temp below °C').value).toBe('8');
      expect(inputAfterText('Inverter temp above °C').value).toBe('60');
    });

    it('shows temperature bounds when no device type is known yet', async () => {
      mountWithAlerts(null);
      expect(await screen.findByText('Temperature & SOC')).toBeDefined();
      expect(screen.getByText('Battery temp below °C')).toBeDefined();
      expect(screen.getByText('Inverter temp below °C')).toBeDefined();
    });

    it('hides battery and inverter temperature bounds for Gateway devices', async () => {
      mountWithAlerts({ device_type_code: '7001' });
      expect(await screen.findByText('SOC')).toBeDefined();
      expect(screen.getByText(/Gateway does not expose battery or inverter temperature telemetry/)).toBeDefined();
      expect(screen.queryByText('Battery temp below °C')).toBeNull();
      expect(screen.queryByText('Battery temp above °C')).toBeNull();
      expect(screen.queryByText('Inverter temp below °C')).toBeNull();
      expect(screen.queryByText('Inverter temp above °C')).toBeNull();
      expect(screen.getByText('SOC below %')).toBeDefined();
      expect(screen.getByText('SOC above %')).toBeDefined();
    });

    it('shows a separate inverter trip alert toggle', async () => {
      mountWithAlerts({ device_type_code: '2001' });
      expect(await screen.findByText('Inverter Trip')).toBeDefined();
    });

    it('posts inverter temperature bounds and mirrors them into the store when saving (issue #183)', async () => {
      // Start from a known baseline so the store-update assertion is meaningful.
      useInverterStore.setState({ inverterTempConfig: { inverter_temp_min: 8, inverter_temp_max: 60 } });
      mountWithAlerts({ device_type_code: '2001' });
      await screen.findByText('Temperature & SOC');
      fireEvent.change(inputAfterText('Inverter temp below °C'), { target: { value: '7.5' } });
      fireEvent.change(inputAfterText('Inverter temp above °C'), { target: { value: '62' } });

      fireEvent.click(screen.getByRole('button', { name: 'Save Notification Settings' }));

      await waitFor(() => {
        expect(apiPostMock).toHaveBeenCalledWith('/api/alerts', expect.objectContaining({
          inverter_temp_min: 7.5,
          inverter_temp_max: 62,
        }));
      });
      // The saved thresholds are mirrored into the store so the
      // SystemAlertBanners re-render with the new values immediately instead
      // of caching the mount-time config until a hard refresh (issue #183).
      await waitFor(() => {
        expect(useInverterStore.getState().inverterTempConfig).toEqual({
          inverter_temp_min: 7.5,
          inverter_temp_max: 62,
        });
      });
    });
  });

  describe('developer mode', () => {
    it('always renders the Developer section heading (the toggle lives there)', async () => {
      mountApiMocks();
      useInverterStore.setState({ developerMode: false });
      render(<SettingsPage />);
      expect(await screen.findByText('Developer')).toBeDefined();
    });

    it('hides the read-only API guidance text when developer mode is off', async () => {
      mountApiMocks();
      useInverterStore.setState({ developerMode: false });
      render(<SettingsPage />);
      await screen.findByText('Developer');
      // The API-port guidance only renders inside the developerMode block.
      expect(screen.queryByText(/SolarWatch/)).toBeNull();
    });

    it('shows the read-only API guidance text when developer mode is on', async () => {
      mountApiMocks();
      useInverterStore.setState({ developerMode: true });
      render(<SettingsPage />);
      expect(await screen.findByText(/SolarWatch/)).toBeDefined();
    });
  });
});
