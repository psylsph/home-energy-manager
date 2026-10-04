import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, within, act } from '@testing-library/react';

// ---------------------------------------------------------------------------
// SettingsPage handlers that had no coverage on their failure and edge paths:
// clearing the API key, saving / scanning for the EV charger, scanning for the
// inverter, the alert save and test sends, the weather save and backfill
// (including its progress polling), panel visibility, the optimistic toggles
// that must revert when the save fails, and the clipboard fallback.
// ---------------------------------------------------------------------------

type Json = Record<string, unknown>;

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

import SettingsPage from '../../src/pages/SettingsPage';
import { useInverterStore } from '../../src/store/useInverterStore';

const tariff = () => ({ slots: [{ start: '00:00', end: '23:59', rate: 0.15 }], version: 2 });

function settings(overrides: Json = {}): Json {
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
    solar_arrays: [],
    import_tariff_config: tariff(),
    export_tariff_config: tariff(),
    octopus_enabled: false,
    octopus_account_number: '',
    octopus_api_key_configured: false,
    octopus_gas_unit: 'unknown',
    octopus_economy7_start: '00:30',
    octopus_economy7_end: '07:30',
    ...overrides,
  };
}

function alertConfig(overrides: Json = {}): Json {
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

interface WeatherData {
  config: { enabled: boolean; latitude: number | null; longitude: number | null; update_interval_mins: number; postcode: string };
  current: null;
  history: unknown[];
  backfill_in_progress: boolean;
}

function weather(overrides: Partial<WeatherData['config']> = {}, backfill = false): WeatherData {
  return {
    config: { enabled: true, latitude: 51.5, longitude: -0.12, update_interval_mins: 30, postcode: 'SW1A 1AA', ...overrides },
    current: null,
    history: [],
    backfill_in_progress: backfill,
  };
}

interface Mount {
  settings?: Json;
  alerts?: Json;
  weather?: WeatherData;
  evc?: Json | Error;
  inverters?: Json | Error;
}

function mount(m: Mount = {}) {
  apiGetMock.mockImplementation(async (path: string) => {
    if (path === '/api/settings') return { ok: true, data: settings(m.settings) };
    if (path === '/api/alerts') return { ok: true, data: { config: alertConfig(m.alerts) } };
    if (path === '/api/weather') return { ok: true, data: m.weather ?? weather() };
    if (path === '/api/status') return { ok: true, lan_ip: '192.168.1.99', clients: [], client_count: 0 };
    if (path === '/api/discover') {
      if (m.inverters instanceof Error) throw m.inverters;
      return m.inverters ?? { ok: true, subnets: [], inverters: [] };
    }
    if (path === '/api/evc/discover') {
      if (m.evc instanceof Error) throw m.evc;
      return m.evc ?? { ok: true, subnets: [], chargers: [] };
    }
    return { ok: true, data: {} };
  });
  apiPostMock.mockResolvedValue({ ok: true, message: 'Saved' });
}

const postsTo = (path: string) => apiPostMock.mock.calls.filter((c) => c[0] === path);

function failPost(path: string, error: unknown = new Error('rejected')) {
  apiPostMock.mockImplementation(async (p: string) => {
    if (p === path) throw error;
    return { ok: true, message: 'Saved' };
  });
}

beforeEach(() => {
  vi.spyOn(console, 'error').mockImplementation(() => {});
  vi.spyOn(console, 'warn').mockImplementation(() => {});
  apiGetMock.mockReset();
  apiPostMock.mockReset();
  useInverterStore.setState({
    snapshot: null,
    connectionState: 'disconnected',
    connectedHost: null,
    developerMode: false,
    evcHost: '',
    evcEverConnected: false,
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.useRealTimers();
  cleanup();
  localStorage.removeItem('saved_host');
});

/** The page section that carries the given h2. */
async function sectionOf(heading: string): Promise<HTMLElement> {
  const h = await screen.findByRole('heading', { name: heading });
  return h.closest('section') as HTMLElement;
}

describe('<SettingsPage/> — clearing the API key', () => {
  async function open(overrides: Json = {}) {
    mount({ settings: { api_key_configured: true, api_key_last4: 'a1b2', api_port: 7338, ...overrides } });
    useInverterStore.setState({ developerMode: true });
    render(<SettingsPage />);
    return screen.findByRole('button', { name: 'Clear saved key' });
  }

  it('clears the key and reports it', async () => {
    const clear = await open();
    fireEvent.click(clear);

    await screen.findByText(/API key cleared/);
    expect(apiPostMock).toHaveBeenCalledWith('/api/settings', { api_key: '', api_port: 7338 });
    // No key is configured any more: the clear button is gone.
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Clear saved key' })).toBeNull());
    expect(screen.getByRole('button', { name: 'Generate API key' })).toBeInTheDocument();
  });

  it('refuses a blank port rather than posting one', async () => {
    const clear = await open();
    fireEvent.change(screen.getByLabelText('Port'), { target: { value: '' } });
    fireEvent.click(clear);

    await screen.findByText('API port cannot be blank');
    expect(postsTo('/api/settings')).toHaveLength(0);
  });

  it('reports a failed clear and keeps the key configured', async () => {
    const clear = await open();
    failPost('/api/settings', new Error('disk full'));
    fireEvent.click(clear);

    await screen.findByText('disk full');
    expect(screen.getByRole('button', { name: 'Clear saved key' })).toBeInTheDocument();
  });

  it('falls back to a generic message when the failure carries none', async () => {
    const clear = await open();
    failPost('/api/settings', 'nope');
    fireEvent.click(clear);
    await screen.findByText('Failed to clear API key');
  });
});

describe('<SettingsPage/> — EV charger', () => {
  const save = (section: HTMLElement) => within(section).getByRole('button', { name: 'Save' });
  const host = (section: HTMLElement) =>
    within(section).getByPlaceholderText(/192\.168\.1\.50/) as HTMLInputElement;

  it('saves the address, tells the store, and resets cached charger state', async () => {
    mount();
    useInverterStore.setState({ evcEverConnected: true });
    render(<SettingsPage />);
    const section = await sectionOf('EV Charger');

    fireEvent.change(host(section), { target: { value: '192.168.1.60' } });
    fireEvent.click(save(section));

    await screen.findByText('EV Charger settings saved');
    expect(apiPostMock).toHaveBeenCalledWith('/api/settings', { evc_host: '192.168.1.60', evc_port: 502 });
    expect(useInverterStore.getState().evcHost).toBe('192.168.1.60');
    // A new host must not inherit "was connected" from the previous one.
    expect(useInverterStore.getState().evcEverConnected).toBe(false);
  });

  it('says the charger is disabled when the address is cleared', async () => {
    mount({ settings: { evc_host: '192.168.1.60' } });
    render(<SettingsPage />);
    const section = await sectionOf('EV Charger');
    await waitFor(() => expect(host(section).value).toBe('192.168.1.60'));

    fireEvent.change(host(section), { target: { value: '' } });
    fireEvent.click(save(section));

    await screen.findByText('EV Charger disabled');
    expect(apiPostMock).toHaveBeenCalledWith('/api/settings', { evc_host: '', evc_port: 502 });
  });

  it('will not save an address that is not an IPv4 address', async () => {
    mount();
    render(<SettingsPage />);
    const section = await sectionOf('EV Charger');

    fireEvent.change(host(section), { target: { value: 'not-an-ip' } });

    expect(host(section)).toHaveAttribute('aria-invalid', 'true');
    expect(save(section)).toBeDisabled();
    expect(within(section).getByText(/four numbers separated by dots/)).toBeInTheDocument();
    fireEvent.click(save(section));
    expect(postsTo('/api/settings')).toHaveLength(0);
  });

  it('reports a failed save', async () => {
    mount();
    render(<SettingsPage />);
    const section = await sectionOf('EV Charger');
    failPost('/api/settings', new Error('write failed'));

    fireEvent.change(host(section), { target: { value: '192.168.1.60' } });
    fireEvent.click(save(section));

    await screen.findByText('write failed');
    expect(useInverterStore.getState().evcHost).toBe('');
  });

  it('falls back to a generic message for a non-Error failure', async () => {
    mount();
    render(<SettingsPage />);
    const section = await sectionOf('EV Charger');
    failPost('/api/settings', null);
    fireEvent.change(host(section), { target: { value: '192.168.1.60' } });
    fireEvent.click(save(section));
    await screen.findByText('Failed to save EV charger settings');
  });

  describe('network scan', () => {
    const scan = (section: HTMLElement) => within(section).getByRole('button', { name: /Scan Network|Scanning/ });

    it('lists what it finds and fills the address from the one chosen', async () => {
      mount({ evc: { ok: true, subnets: ['192.168.1'], chargers: [{ ip: '192.168.1.77', port: 502, serial: 'EVC-001' }, { host: '192.168.1.78', port: 503 }] } });
      render(<SettingsPage />);
      const section = await sectionOf('EV Charger');

      fireEvent.click(scan(section));

      await within(section).findByText('192.168.1.77:502');
      expect(within(section).getByText('EVC-001')).toBeInTheDocument();
      // A charger without a serial is described generically.
      expect(within(section).getByText('Standard Modbus TCP device')).toBeInTheDocument();

      fireEvent.click(within(section).getAllByRole('button', { name: 'Use' })[0]);
      expect(host(section).value).toBe('192.168.1.77');
    });

    it('says so, and what was scanned, when nothing is found', async () => {
      mount({ evc: { ok: true, subnets: ['192.168.1', '10.0.0'], chargers: [] } });
      render(<SettingsPage />);
      const section = await sectionOf('EV Charger');
      fireEvent.click(scan(section));
      await within(section).findByText(/No EV chargers found on the network\. Scanned: 192\.168\.1\.x, 10\.0\.0\.x/);
    });

    it('says so without a subnet list when none was reported', async () => {
      mount();
      render(<SettingsPage />);
      const section = await sectionOf('EV Charger');
      fireEvent.click(scan(section));
      const note = await within(section).findByText(/No EV chargers found on the network\./);
      expect(note.textContent).not.toMatch(/Scanned/);
    });

    it('shows the failure when the scan itself fails', async () => {
      mount({ evc: new Error('backend down') });
      render(<SettingsPage />);
      const section = await sectionOf('EV Charger');
      fireEvent.click(scan(section));
      await within(section).findByText('backend down');
      // And the scan button is usable again.
      expect(scan(section)).toBeEnabled();
    });
  });
});

describe('<SettingsPage/> — inverter scan', () => {
  const scan = (section: HTMLElement) => within(section).getByRole('button', { name: /Scan Network|Scanning/ });

  it('fills the host, port and serial from the inverter chosen', async () => {
    mount({ inverters: { ok: true, subnets: [], inverters: [{ ip: '192.168.1.55', port: 8899, serial: 'SA99999999', generation: 'Gen3' }] } });
    render(<SettingsPage />);
    const section = await sectionOf('Inverter Connection');

    fireEvent.click(scan(section));

    await within(section).findByText('192.168.1.55:8899');
    expect(within(section).getByText(/SA99999999 · Gen3/)).toBeInTheDocument();
    fireEvent.click(within(section).getByRole('button', { name: 'Use' }));
    expect((within(section).getByPlaceholderText(/192\.168/) as HTMLInputElement).value).toBe('192.168.1.55');
  });

  it('describes an inverter that reports no serial', async () => {
    mount({ inverters: { ok: true, subnets: [], inverters: [{ host: '192.168.1.55', port: 8899 }] } });
    render(<SettingsPage />);
    const section = await sectionOf('Inverter Connection');
    fireEvent.click(scan(section));
    await within(section).findByText('Unknown serial');
  });

  it('reports what was scanned when nothing is found', async () => {
    mount({ inverters: { ok: true, subnets: ['192.168.0'], inverters: [] } });
    render(<SettingsPage />);
    const section = await sectionOf('Inverter Connection');
    fireEvent.click(scan(section));
    await within(section).findByText(/No inverters found on the network\. Scanned: 192\.168\.0\.x/);
  });

  it('shows the failure when the scan fails', async () => {
    mount({ inverters: new Error('scan blew up') });
    render(<SettingsPage />);
    const section = await sectionOf('Inverter Connection');
    fireEvent.click(scan(section));
    await within(section).findByText('scan blew up');
  });
});

describe('<SettingsPage/> — notifications', () => {
  const configured = { telegram_bot_token: 'tok', telegram_chat_id: '42' };

  it('reports a rejected alert save without touching the inverter-temperature banner thresholds', async () => {
    // The form holds 5-75, the store 8-60: a rejected save must leave the store alone.
    mount({ alerts: { inverter_temp_min: 5, inverter_temp_max: 75 } });
    apiPostMock.mockResolvedValue({ ok: false, message: 'Telegram token looks invalid' });
    useInverterStore.setState({ inverterTempConfig: { inverter_temp_min: 8, inverter_temp_max: 60 } });
    render(<SettingsPage />);
    const save = await screen.findByRole('button', { name: 'Save Notification Settings' });
    await waitFor(() => expect(apiGetMock).toHaveBeenCalledWith('/api/alerts'));

    fireEvent.click(save);

    await screen.findByText('Telegram token looks invalid');
    expect(useInverterStore.getState().inverterTempConfig).toEqual({ inverter_temp_min: 8, inverter_temp_max: 60 });
  });

  it('mirrors saved inverter-temperature thresholds into the store on success', async () => {
    mount({ alerts: { inverter_temp_min: 5, inverter_temp_max: 75 } });
    apiPostMock.mockResolvedValue({ ok: true, message: 'Alert settings saved' });
    render(<SettingsPage />);
    const save = await screen.findByRole('button', { name: 'Save Notification Settings' });
    await waitFor(() => expect(apiGetMock).toHaveBeenCalledWith('/api/alerts'));

    fireEvent.click(save);

    await screen.findByText('Alert settings saved');
    await waitFor(() =>
      expect(useInverterStore.getState().inverterTempConfig).toEqual({ inverter_temp_min: 5, inverter_temp_max: 75 }),
    );
  });

  it('shows a generic message when the alert save throws', async () => {
    mount();
    failPost('/api/alerts');
    render(<SettingsPage />);
    fireEvent.click(await screen.findByRole('button', { name: 'Save Notification Settings' }));
    await screen.findByText('Failed to save alert settings');
  });

  it('sends a test notification and reports its outcome', async () => {
    mount({ alerts: configured });
    apiPostMock.mockResolvedValue({ ok: true, message: 'Test message sent' });
    render(<SettingsPage />);
    const test = await screen.findByRole('button', { name: 'Send Test Notification' });
    await waitFor(() => expect(test).toBeEnabled());

    fireEvent.click(test);

    await screen.findByText('Test message sent');
    expect(apiPostMock).toHaveBeenCalledWith('/api/alerts/test', {});
  });

  it('says how to fix a test notification that fails to send', async () => {
    mount({ alerts: configured });
    failPost('/api/alerts/test');
    render(<SettingsPage />);
    const test = await screen.findByRole('button', { name: 'Send Test Notification' });
    await waitFor(() => expect(test).toBeEnabled());
    fireEvent.click(test);
    await screen.findByText('Failed to Send Message — check API key and settings');
  });

  it('cannot send a test until a channel is configured', async () => {
    // The serial-derived default ntfy topic counts as a channel, so use no serial.
    mount({ settings: { serial: '' } });
    render(<SettingsPage />);
    expect(await screen.findByRole('button', { name: 'Send Test Notification' })).toBeDisabled();
  });
});

describe('<SettingsPage/> — local weather', () => {
  const save = () => screen.getByRole('button', { name: /Save Location|Saving/ });
  const backfill = () => screen.getByRole('button', { name: /Backfill History|Backfilling/ });

  it('posts the postcode and coordinates, then shows what the backend resolved', async () => {
    mount({ weather: weather({ postcode: 'SW1A 1AA', latitude: null, longitude: null }) });
    render(<SettingsPage />);
    await screen.findByRole('heading', { name: 'Local Weather' });
    await waitFor(() => expect(save()).toBeInTheDocument());

    // After saving, the backend resolves the postcode; the form must follow.
    apiGetMock.mockImplementation(async (path: string) =>
      path === '/api/weather'
        ? { ok: true, data: weather({ postcode: 'SW1A 1AA', latitude: 51.5, longitude: -0.12 }) }
        : { ok: true, data: {} },
    );
    fireEvent.click(save());

    await screen.findByText('Saved');
    expect(apiPostMock).toHaveBeenCalledWith('/api/weather', { enabled: true, postcode: 'SW1A 1AA' });
    await waitFor(() => expect(screen.getByDisplayValue('51.5')).toBeInTheDocument());
    expect(screen.getByDisplayValue('-0.12')).toBeInTheDocument();
  });

  it('reports a failed save', async () => {
    mount();
    failPost('/api/weather', new Error('postcode lookup failed'));
    render(<SettingsPage />);
    await screen.findByRole('heading', { name: 'Local Weather' });
    await waitFor(() => expect(save()).toBeEnabled());
    fireEvent.click(save());
    await screen.findByText('postcode lookup failed');
  });

  it('falls back to a generic message for a non-Error failure', async () => {
    mount();
    failPost('/api/weather', null);
    render(<SettingsPage />);
    await screen.findByRole('heading', { name: 'Local Weather' });
    await waitFor(() => expect(save()).toBeEnabled());
    fireEvent.click(save());
    await screen.findByText('Failed to save weather config');
  });

  it('cannot backfill until a location has been saved', async () => {
    mount({ weather: weather({ latitude: null, longitude: null }) });
    render(<SettingsPage />);
    await screen.findByRole('heading', { name: 'Local Weather' });
    await waitFor(() => expect(backfill()).toBeInTheDocument());
    expect(backfill()).toBeDisabled();
    expect(backfill()).toHaveAttribute('title', 'Save a location first');
  });

  it('starts a backfill and polls for progress until it finishes', async () => {
    mount();
    render(<SettingsPage />);
    await screen.findByRole('heading', { name: 'Local Weather' });
    await waitFor(() => expect(backfill()).toBeEnabled());

    // The backend reports in-progress at first, then done.
    let polls = 0;
    apiGetMock.mockImplementation(async (path: string) => {
      if (path !== '/api/weather') return { ok: true, data: {} };
      polls += 1;
      return { ok: true, data: weather({}, polls < 2) };
    });
    vi.useFakeTimers({ shouldAdvanceTime: true });
    fireEvent.click(backfill());

    await screen.findByText('Backfill started — fetching historical weather in the background');
    expect(postsTo('/api/weather/backfill')).toHaveLength(1);
    await waitFor(() => expect(backfill().textContent).toBe('Backfilling…'));

    // The first poll is immediate (still running); the next one fires on the interval.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3100);
    });
    await waitFor(() => expect(backfill().textContent).toBe('Backfill History'));
    expect(polls).toBeGreaterThanOrEqual(2);
  });

  it('reports a backfill that fails to start and lets it be retried', async () => {
    mount();
    failPost('/api/weather/backfill', new Error('quota exceeded'));
    render(<SettingsPage />);
    await screen.findByRole('heading', { name: 'Local Weather' });
    await waitFor(() => expect(backfill()).toBeEnabled());
    // Make any progress poll report "still running", so only the failure
    // handler itself can put the button back (otherwise the poll would).
    apiGetMock.mockImplementation(async (path: string) =>
      path === '/api/weather' ? { ok: true, data: weather({}, true) } : { ok: true, data: {} },
    );

    fireEvent.click(backfill());

    await screen.findByText('quota exceeded');
    expect(backfill().textContent).toBe('Backfill History');
    expect(backfill()).toBeEnabled();
  });
});

/**
 * The Enable Weather / Enable Alerts toggles have no role or accessible name
 * (only the update-check switch is labelled), so find them by their row label.
 */
function rowToggle(label: string): HTMLElement {
  const row = screen.getByText(label).parentElement as HTMLElement;
  return row.querySelector('.cursor-pointer') as HTMLElement;
}

describe('<SettingsPage/> — optimistic toggles revert when the save fails', () => {
  it('reports a failed weather enable', async () => {
    mount({ weather: weather({ enabled: false }) });
    render(<SettingsPage />);
    await sectionOf('Local Weather');
    failPost('/api/weather', new Error('weather save failed'));

    fireEvent.click(rowToggle('Enable Weather'));

    await screen.findByText('weather save failed');
  });

  it('reports a failed alerts enable', async () => {
    mount();
    render(<SettingsPage />);
    await sectionOf('Notifications');
    failPost('/api/alerts', new Error('alerts save failed'));

    fireEvent.click(rowToggle('Enable Alerts'));

    await screen.findByText('alerts save failed');
  });

  it('puts the update-check switch back and says why when the save fails', async () => {
    mount({ settings: { check_for_updates: true } });
    render(<SettingsPage />);
    const section = await sectionOf('Updates');
    const toggle = within(section).getByRole('switch');
    expect(toggle.getAttribute('aria-checked')).toBe('true');
    failPost('/api/settings', new Error('settings locked'));

    fireEvent.click(toggle);

    await screen.findByText('Failed to update update-checking: settings locked');
    await waitFor(() => expect(toggle.getAttribute('aria-checked')).toBe('true'));
  });

  it('reports a failed panel visibility save', async () => {
    mount();
    render(<SettingsPage />);
    await screen.findByText('Panel Visibility');
    failPost('/api/settings');

    fireEvent.click(screen.getByRole('button', { name: 'Save Panel Visibility' }));

    await screen.findByText('Failed to save panel visibility');
  });
});

describe('<SettingsPage/> — copying the access URL', () => {
  const copy = async () => {
    mount();
    render(<SettingsPage />);
    const section = await sectionOf('Remote / Mobile Network Access');
    return within(section).getAllByRole('button', { name: 'Copy' })[0];
  };

  afterEach(() => {
    vi.unstubAllGlobals();
    Object.defineProperty(window, 'isSecureContext', { value: true, configurable: true });
  });

  it('uses the clipboard API in a secure context', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    Object.defineProperty(window, 'isSecureContext', { value: true, configurable: true });

    fireEvent.click(await copy());

    await screen.findByText('URL copied!');
    expect(writeText).toHaveBeenCalledWith('http://192.168.1.99:7337');
  });

  it('falls back to selecting and copying when the page is not a secure context', async () => {
    Object.defineProperty(window, 'isSecureContext', { value: false, configurable: true });
    const exec = vi.fn().mockReturnValue(true);
    (document as unknown as { execCommand: unknown }).execCommand = exec;

    fireEvent.click(await copy());

    await screen.findByText('URL copied!');
    expect(exec).toHaveBeenCalledWith('copy');
    // The temporary textarea is removed again.
    expect(document.querySelector('textarea[style*="fixed"]')).toBeNull();
  });

  it('asks the user to copy by hand when the fallback fails', async () => {
    Object.defineProperty(window, 'isSecureContext', { value: false, configurable: true });
    (document as unknown as { execCommand: unknown }).execCommand = vi.fn(() => {
      throw new Error('blocked');
    });

    fireEvent.click(await copy());

    await screen.findByText('Copy failed — please select and copy manually');
    expect(document.querySelector('textarea[style*="fixed"]')).toBeNull();
  });
});
