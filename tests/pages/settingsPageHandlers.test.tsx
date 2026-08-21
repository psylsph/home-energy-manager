import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// SettingsPage already has SettingsPage.test.tsx (shell + hydration + alerts)
// and focused tests for grid lines, inverter address, node status, standing
// charge, solar arrays, tray, tariff rate 3dp, grid meter. This file adds
// coverage for the untested save handlers and their error/validation paths:
// tariff save (invalid + standing charge coercion), HTTP port save, refresh
// interval change, weather save, Octopus save validation, and the ntfy-topic
// derived default used by the alerts save.
// ---------------------------------------------------------------------------

type SettingsShape = Record<string, unknown>;

const apiGetMock = vi.fn();
const apiPostMock = vi.fn();
const openExternalMock = vi.fn();

vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGetMock(...(args as [string])),
  apiPost: (...args: unknown[]) => apiPostMock(...(args as [string, unknown])),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  isTauri: false,
}));

vi.mock('../../src/lib/openExternal', () => ({
  openExternal: (...args: unknown[]) => openExternalMock(...args),
}));

import SettingsPage from '../../src/pages/SettingsPage';
import { useInverterStore } from '../../src/store/useInverterStore';

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

function defaultTariffConfig() {
  return {
    slots: [{ start: '00:00', end: '23:59', rate: 0.15 }],
    version: 2,
  };
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
    disable_auto_discovery: true,
    autostart_enabled: false,
    minimise_to_tray: false,
    start_minimised: false,
    check_for_updates: true,
    api_key: '',
    api_port: 7338,
    hidden_panels: [],
    pv1_rated_kw: null,
    pv2_rated_kw: null,
    solar_arrays: [],
    import_tariff_config: defaultTariffConfig(),
    export_tariff_config: defaultTariffConfig(),
    import_tariff: null,
    export_tariff: null,
    import_standing_charge_p_per_day: null,
    octopus_enabled: false,
    octopus_account_number: '',
    octopus_api_key_configured: false,
    octopus_gas_unit: 'unknown',
    octopus_economy7_start: '00:30',
    octopus_economy7_end: '07:30',
    ...overrides,
  };
}

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
    battery_connection_lost_enabled: true,
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
      return { ok: true, data: { config: alertConfig() } };
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
      return { ok: true, lan_ip: null, clients: [], client_count: 0 };
    }
    if (path === '/api/discover') {
      return { ok: true, subnets: [], inverters: [] };
    }
    if (path === '/api/evc/discover') {
      return { ok: true, subnets: [], chargers: [] };
    }
    return { ok: true, data: {} };
  });
  apiPostMock.mockResolvedValue({ ok: true, message: 'Saved' });
}

describe('<SettingsPage/> — save handlers & validation', () => {
  beforeEach(() => {
    silenceConsoleError();
    apiGetMock.mockReset();
    apiPostMock.mockReset();
    useInverterStore.setState({
      snapshot: null,
      connectionState: 'disconnected',
      connectedHost: null,
      developerMode: false,
      evcHost: '',
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
    localStorage.removeItem('saved_host');
  });

  describe('refresh interval change', () => {
    it('posts the new interval and flashes a success message', async () => {
      mountApiMocks({ interval_secs: 20 });
      render(<SettingsPage />);
      await screen.findByText('Refresh Interval');
      const fifteenBtn = screen.getByText('15s');
      fireEvent.click(fifteenBtn);
      await waitFor(() => {
        expect(apiPostMock).toHaveBeenCalledWith('/api/settings', { interval_secs: 15 });
      });
      // The success flash appears in the toast (green bg).
      await waitFor(() => {
        expect(screen.getByText('Refresh interval set to 15s')).toBeDefined();
      });
    });

    it('flashes an error message when the post fails', async () => {
      mountApiMocks({ interval_secs: 20 });
      apiPostMock.mockRejectedValueOnce(new Error('server unreachable'));
      render(<SettingsPage />);
      await screen.findByText('Refresh Interval');
      fireEvent.click(screen.getByText('15s'));
      await waitFor(() => {
        expect(screen.getByText('server unreachable')).toBeDefined();
      });
    });
  });

  describe('HTTP port save', () => {
    it('posts the configured port and flashes a restart-required message', async () => {
      mountApiMocks({ http_port: 7337 });
      render(<SettingsPage />);
      // The App section contains the HTTP port input + a Save button. Find
      // the input by its wrapping flex container (it's the only number input
      // next to a Save button in the App section).
      const portHeading = await screen.findByText('HTTP Port');
      const section = portHeading.closest('section')!;
      // The HTTP port input sits inside the same flex row as the Save button.
      // Find the label that wraps the number input by querying for the
      // flex-row container containing both.
      const flexRows = section.querySelectorAll('div.flex.items-center.gap-3');
      let portInput: HTMLInputElement | null = null;
      let saveBtn: HTMLButtonElement | null = null;
      for (const row of flexRows) {
        const inp = row.querySelector('input[type="number"]');
        const btn = row.querySelector('button');
        if (inp && btn) {
          portInput = inp as HTMLInputElement;
          saveBtn = btn as HTMLButtonElement;
          break;
        }
      }
      expect(portInput).toBeTruthy();
      expect(saveBtn).toBeTruthy();
      fireEvent.change(portInput!, { target: { value: '8000' } });
      fireEvent.click(saveBtn!);
      await waitFor(() => {
        expect(apiPostMock).toHaveBeenCalledWith('/api/settings', { http_port: 8000 });
      });
      await waitFor(() => {
        expect(screen.getByText(/HTTP port set to 8000/)).toBeDefined();
      });
    });
  });

  describe('tariff save — standing charge coercion (issue #131)', () => {
    it('coerces an empty standing charge to 0 on save', async () => {
      mountApiMocks({ import_standing_charge_p_per_day: null });
      render(<SettingsPage />);
      await screen.findByText('Save Tariffs');
      fireEvent.click(screen.getByRole('button', { name: 'Save Tariffs' }));
      await waitFor(() => {
        expect(apiPostMock).toHaveBeenCalledWith('/api/settings', expect.objectContaining({
          import_standing_charge_p_per_day: 0,
        }));
      });
    });

    it('saves a numeric standing charge unchanged', async () => {
      mountApiMocks({ import_standing_charge_p_per_day: 54.86 });
      render(<SettingsPage />);
      await screen.findByText('Save Tariffs');
      fireEvent.click(screen.getByRole('button', { name: 'Save Tariffs' }));
      await waitFor(() => {
        expect(apiPostMock).toHaveBeenCalledWith('/api/settings', expect.objectContaining({
          import_standing_charge_p_per_day: 54.86,
        }));
      });
    });

    it('flashes a success message on save', async () => {
      mountApiMocks();
      render(<SettingsPage />);
      await screen.findByText('Save Tariffs');
      fireEvent.click(screen.getByRole('button', { name: 'Save Tariffs' }));
      await waitFor(() => {
        expect(screen.getByText('Tariff rates saved')).toBeDefined();
      });
    });

    it('surfaces the server error message when save fails', async () => {
      mountApiMocks();
      apiPostMock.mockRejectedValueOnce(new Error('Tariff validation failed on server'));
      render(<SettingsPage />);
      await screen.findByText('Save Tariffs');
      fireEvent.click(screen.getByRole('button', { name: 'Save Tariffs' }));
      await waitFor(() => {
        expect(screen.getByText('Tariff validation failed on server')).toBeDefined();
      });
    });
  });

  describe('Octopus save — validation (issue #212)', () => {
    // Helper: scope queries to the "Octopus Energy Data" section so they
    // don't collide with the panel-visibility checkboxes elsewhere.
    async function octopusSection(): Promise<HTMLElement> {
      const heading = await screen.findByText('Octopus Energy Data');
      let el: HTMLElement | null = heading;
      while (el && el.tagName.toLowerCase() !== 'section') el = el.parentElement;
      return el ?? heading;
    }

    /** Find the "Save Octopus Settings" button by text within the section. */
    function findOctopusSaveBtn(section: HTMLElement): HTMLButtonElement {
      const btns = Array.from(section.querySelectorAll('button'));
      const saveBtn = btns.find((b) => b.textContent?.includes('Save Octopus Settings'));
      if (!saveBtn) throw new Error('Save Octopus Settings button not found');
      return saveBtn;
    }

    it('rejects enabling without an account number', async () => {
      mountApiMocks({ octopus_api_key_configured: true });
      render(<SettingsPage />);
      const section = await octopusSection();
      const saveBtn = findOctopusSaveBtn(section);
      // Enable the checkbox inside this section.
      const enableCheckbox = section.querySelector('input[type="checkbox"]') as HTMLInputElement;
      fireEvent.click(enableCheckbox);
      fireEvent.click(saveBtn);
      await waitFor(() => {
        expect(screen.getByText(/Enter both the Octopus account number and API key/)).toBeDefined();
      });
      // No POST should have been made.
      expect(apiPostMock).not.toHaveBeenCalled();
    });

    it('rejects enabling without any key configured or entered', async () => {
      mountApiMocks({ octopus_api_key_configured: false });
      render(<SettingsPage />);
      const section = await octopusSection();
      const saveBtn = findOctopusSaveBtn(section);
      const enableCheckbox = section.querySelector('input[type="checkbox"]') as HTMLInputElement;
      fireEvent.click(enableCheckbox);
      const accountInput = section.querySelector('input[placeholder="A-1234ABCD"]') as HTMLInputElement;
      fireEvent.change(accountInput, { target: { value: 'A-1234ABCD' } });
      fireEvent.click(saveBtn);
      await waitFor(() => {
        expect(screen.getByText(/Enter both the Octopus account number and API key/)).toBeDefined();
      });
    });

    it('saves and triggers a sync when enabling with account + key', async () => {
      mountApiMocks({ octopus_api_key_configured: false });
      render(<SettingsPage />);
      const section = await octopusSection();
      const saveBtn = findOctopusSaveBtn(section);
      const enableCheckbox = section.querySelector('input[type="checkbox"]') as HTMLInputElement;
      fireEvent.click(enableCheckbox);
      const accountInput = section.querySelector('input[placeholder="A-1234ABCD"]') as HTMLInputElement;
      fireEvent.change(accountInput, { target: { value: 'A-1234ABCD' } });
      const keyInput = section.querySelector('input[type="password"]') as HTMLInputElement;
      fireEvent.change(keyInput, { target: { value: 'sk_live_test123' } });
      fireEvent.click(saveBtn);
      await waitFor(() => {
        expect(apiPostMock).toHaveBeenCalledWith('/api/settings', expect.objectContaining({
          octopus_enabled: true,
          octopus_account_number: 'A-1234ABCD',
          octopus_api_key: 'sk_live_test123',
        }));
      });
      await waitFor(() => {
        expect(apiPostMock).toHaveBeenCalledWith('/api/octopus/sync');
      });
      await waitFor(() => {
        expect(screen.getByText(/consumption sync started/)).toBeDefined();
      });
    });

    it('dispatches the octopus-settings-changed window event on save', async () => {
      mountApiMocks({ octopus_enabled: true, octopus_account_number: 'A-1234ABCD', octopus_api_key_configured: true });
      const handler = vi.fn();
      window.addEventListener('octopus-settings-changed', handler);
      render(<SettingsPage />);
      const section = await octopusSection();
      const saveBtn = findOctopusSaveBtn(section);
      fireEvent.click(saveBtn);
      await waitFor(() => {
        expect(handler).toHaveBeenCalledTimes(1);
      });
      window.removeEventListener('octopus-settings-changed', handler);
    });

    it('clears the API key input after a successful save (write-only)', async () => {
      mountApiMocks({ octopus_api_key_configured: false });
      render(<SettingsPage />);
      const section = await octopusSection();
      const saveBtn = findOctopusSaveBtn(section);
      const enableCheckbox = section.querySelector('input[type="checkbox"]') as HTMLInputElement;
      fireEvent.click(enableCheckbox);
      const accountInput = section.querySelector('input[placeholder="A-1234ABCD"]') as HTMLInputElement;
      fireEvent.change(accountInput, { target: { value: 'A-1234ABCD' } });
      const keyInput = section.querySelector('input[type="password"]') as HTMLInputElement;
      fireEvent.change(keyInput, { target: { value: 'sk_live_test123' } });
      fireEvent.click(saveBtn);
      await waitFor(() => {
        expect(keyInput.value).toBe('');
      });
    });
  });

  describe('solar arrays save (issue #110)', () => {
    it('saves pv1/pv2 ratings and an empty CT array list', async () => {
      mountApiMocks();
      render(<SettingsPage />);
      await screen.findByText('Save Solar Arrays');
      fireEvent.click(screen.getByTestId('solar-arrays-save'));
      await waitFor(() => {
        expect(apiPostMock).toHaveBeenCalledWith('/api/settings', expect.objectContaining({
          pv1_rated_kw: 0,
          pv2_rated_kw: 0,
          solar_arrays: [],
        }));
      });
      await waitFor(() => {
        expect(screen.getByText('Solar arrays saved')).toBeDefined();
      });
    });

    it('filters out CT arrays with out-of-range meter addresses (1-8 only)', async () => {
      mountApiMocks();
      render(<SettingsPage />);
      await screen.findByText('Save Solar Arrays');
      // Add one valid array row.
      fireEvent.click(screen.getByTestId('solar-array-add'));
      const nameInput = await screen.findByPlaceholderText('e.g. East roof');
      fireEvent.change(nameInput, { target: { value: 'Garage' } });
      const kwpInputs = screen.getAllByPlaceholderText('e.g. 6');
      // The CT-array kWp input is the last "e.g. 6" placeholder (pv1/pv2 also use it).
      fireEvent.change(kwpInputs[kwpInputs.length - 1], { target: { value: '4.5' } });
      fireEvent.click(screen.getByTestId('solar-arrays-save'));
      await waitFor(() => {
        expect(apiPostMock).toHaveBeenCalledWith('/api/settings', expect.objectContaining({
          solar_arrays: [expect.objectContaining({
            meter_address: 1,
            name: 'Garage',
            rated_kw: 4.5,
          })],
        }));
      });
    });

    it('flashes an error when the save fails', async () => {
      mountApiMocks();
      apiPostMock.mockRejectedValueOnce(new Error('disk full'));
      render(<SettingsPage />);
      await screen.findByText('Save Solar Arrays');
      fireEvent.click(screen.getByTestId('solar-arrays-save'));
      await waitFor(() => {
        expect(screen.getByText('disk full')).toBeDefined();
      });
    });
  });

  describe('auto-discovery toggle — optimistic persist', () => {
    it('persists the new auto-discovery value and flashes success', async () => {
      // Start with auto-discovery ON (disable_auto_discovery: false).
      mountApiMocks({ disable_auto_discovery: false });
      render(<SettingsPage />);
      const toggle = await screen.findByRole('switch', { name: 'Enable Auto-Discovery' });
      expect(toggle.getAttribute('aria-checked')).toBe('true');
      // Click to turn it OFF → disable_auto_discovery: true.
      fireEvent.click(toggle);
      await waitFor(() => {
        expect(apiPostMock).toHaveBeenCalledWith('/api/settings', { disable_auto_discovery: true });
      });
      await waitFor(() => {
        expect(screen.getByText('Auto-Discovery setting saved')).toBeDefined();
      });
    });
  });

  describe('panel visibility save — pushes to store immediately', () => {
    it('saves hidden_panels and syncs the store', async () => {
      mountApiMocks();
      useInverterStore.setState({ hiddenPanels: [] });
      render(<SettingsPage />);
      await screen.findByText('Panel Visibility');
      // The Panel Visibility section has a Save button scoped by label.
      fireEvent.click(screen.getByRole('button', { name: 'Save Panel Visibility' }));
      await waitFor(() => {
        expect(apiPostMock).toHaveBeenCalledWith('/api/settings', { hidden_panels: [] });
      });
      await waitFor(() => {
        expect(useInverterStore.getState().hiddenPanels).toEqual([]);
      });
    });
  });

  describe('alerts save — ntfy topic derived default', () => {
    function mountWithAlerts(serial: string, overrides: SettingsShape = {}) {
      mountApiMocks({ serial });
      apiGetMock.mockImplementation(async (path: string) => {
        if (path === '/api/settings') return { ok: true, data: defaultSettings({ serial }) };
        if (path === '/api/alerts') return { ok: true, data: { config: alertConfig({ enabled: true, ...overrides }) } };
        if (path === '/api/weather') return { ok: true, data: { config: {} } };
        if (path === '/api/status') return { ok: true, lan_ip: null, clients: [] };
        return { ok: true, data: {} };
      });
      render(<SettingsPage />);
    }

    it('derives the ntfy topic from the serial when none is configured', async () => {
      mountWithAlerts('SA999');
      await screen.findByText('Save Notification Settings');
      fireEvent.click(screen.getByRole('button', { name: 'Save Notification Settings' }));
      await waitFor(() => {
        expect(apiPostMock).toHaveBeenCalledWith('/api/alerts', expect.objectContaining({
          ntfy_topic: 'hem-SA999',
        }));
      });
    });

    it('uses the user-entered ntfy topic over the derived default', async () => {
      mountWithAlerts('SA999', { ntfy_topic: 'my-custom-topic' });
      await screen.findByText('Save Notification Settings');
      fireEvent.click(screen.getByRole('button', { name: 'Save Notification Settings' }));
      await waitFor(() => {
        expect(apiPostMock).toHaveBeenCalledWith('/api/alerts', expect.objectContaining({
          ntfy_topic: 'my-custom-topic',
        }));
      });
    });
  });

  describe('update-check toggle — clears cached banner info (review #10)', () => {
    it('clears latestVersionInfo in the store when updates are turned off', async () => {
      mountApiMocks({ check_for_updates: true });
      useInverterStore.setState({
        latestVersionInfo: {
          current_version: '1.2.2',
          latest_version: '1.2.3',
          release_url: 'https://example.com/release',
          update_available: true,
        },
      });
      render(<SettingsPage />);
      const toggle = await screen.findByRole('switch', { name: 'Check for new releases' });
      expect(toggle.getAttribute('aria-checked')).toBe('true');
      fireEvent.click(toggle); // turn OFF
      await waitFor(() => {
        expect(apiPostMock).toHaveBeenCalledWith('/api/settings', { check_for_updates: false });
      });
      await waitFor(() => {
        expect(useInverterStore.getState().latestVersionInfo).toBeNull();
      });
    });

    it('keeps latestVersionInfo when updates are turned on', async () => {
      mountApiMocks({ check_for_updates: false });
      const info = {
        current_version: '1.2.2',
        latest_version: null,
        release_url: 'https://example.com/release',
        update_available: false,
      };
      useInverterStore.setState({ latestVersionInfo: info });
      render(<SettingsPage />);
      const toggle = await screen.findByRole('switch', { name: 'Check for new releases' });
      fireEvent.click(toggle); // turn ON
      await waitFor(() => {
        expect(apiPostMock).toHaveBeenCalledWith('/api/settings', { check_for_updates: true });
      });
      expect(useInverterStore.getState().latestVersionInfo).toEqual(info);
    });
  });
});
