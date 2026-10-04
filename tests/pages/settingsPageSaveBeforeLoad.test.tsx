import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, within, act } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Until a section's saved configuration has loaded, its form holds defaults.
// Saving then would overwrite what the user had configured: Save Notification
// Settings would blank the Telegram token and Save Location the postcode. The
// page already waits for /api/settings before showing anything, but alerts and
// weather load separately, so their Save buttons must stay disabled until their
// own load has succeeded. A load that FAILS keeps Save off and says why: the
// form then shows defaults and saving would overwrite the stored tokens.
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

import SettingsPage from '../../src/pages/SettingsPage';
import { useInverterStore } from '../../src/store/useInverterStore';

const tariff = () => ({ slots: [{ start: '00:00', end: '23:59', rate: 0.15 }], version: 2 });

const settingsBody = () => ({
  host: '192.168.1.10', port: 8899, serial: 'SA12345678', interval_secs: 20, http_port: 7337,
  evc_host: '192.168.1.60', evc_port: 502, disable_auto_discovery: true, autostart_enabled: false,
  minimise_to_tray: false, start_minimised: false, check_for_updates: true, api_key: '', api_port: 7338,
  hidden_panels: [], solar_arrays: [], import_tariff_config: tariff(), export_tariff_config: tariff(),
  octopus_enabled: false, octopus_account_number: '', octopus_api_key_configured: false,
  octopus_gas_unit: 'unknown', octopus_economy7_start: '00:30', octopus_economy7_end: '07:30',
});

const alertsBody = () => ({
  enabled: true, telegram_bot_token: 'secret-token', telegram_chat_id: '42', cooldown_minutes: 30,
  batt_temp_min: 0, batt_temp_max: 0, inverter_temp_min: 8, inverter_temp_max: 60, soc_min: 4, soc_max: 100,
  grid_offline_enabled: false, inverter_trip_enabled: false, battery_over_temp_enabled: false,
  battery_connection_lost_enabled: true, connection_lost_enabled: false, solar_clipping_enabled: false,
  solar_clipping_ceiling_w: 0, ntfy_topic: '', ntfy_server: 'https://ntfy.sh', pushover_app_token: '',
  pushover_user_key: '',
});

const weatherBody = () => ({
  config: { enabled: true, latitude: 51.5, longitude: -0.12, update_interval_mins: 30, postcode: 'SW1A 1AA' },
  current: null, history: [], backfill_in_progress: false,
});

type Deferred = { resolve: (v: unknown) => void; reject: (e: unknown) => void };
let pending: Record<string, Deferred>;

/** Hold the given GETs until the test releases them; answer the rest at once. */
function mount(hold: string[]) {
  pending = {};
  const body: Record<string, () => unknown> = {
    '/api/settings': () => ({ ok: true, data: settingsBody() }),
    '/api/alerts': () => ({ ok: true, data: { config: alertsBody() } }),
    '/api/weather': () => ({ ok: true, data: weatherBody() }),
    '/api/status': () => ({ ok: true, lan_ip: null, clients: [], client_count: 0 }),
  };
  apiGetMock.mockImplementation((path: string) => {
    if (hold.includes(path)) {
      return new Promise((resolve, reject) => {
        pending[path] = { resolve: () => resolve(body[path]()), reject };
      });
    }
    return Promise.resolve(body[path]?.() ?? { ok: true, data: {} });
  });
  apiPostMock.mockResolvedValue({ ok: true, message: 'Saved' });
}

const release = (path: string) => act(async () => pending[path].resolve(undefined));
const fail = (path: string) => act(async () => pending[path].reject(new Error('backend not ready')));

async function section(heading: string): Promise<HTMLElement> {
  return (await screen.findByRole('heading', { name: heading })).closest('section') as HTMLElement;
}
const button = (s: HTMLElement, name: string | RegExp) => within(s).getByRole('button', { name });

beforeEach(() => {
  vi.spyOn(console, 'error').mockImplementation(() => {});
  vi.spyOn(console, 'warn').mockImplementation(() => {});
  apiGetMock.mockReset();
  apiPostMock.mockReset();
  useInverterStore.setState({
    snapshot: null, connectionState: 'disconnected', connectedHost: null, developerMode: true, evcHost: '',
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  cleanup();
  localStorage.removeItem('saved_host');
});

describe('<SettingsPage/> — the settings-backed saves cannot be reached before the load', () => {
  // Existing behaviour, pinned: the whole page is a spinner until /api/settings
  // settles, so EV charger / tariff / solar / Octopus / panel / port / API-key
  // saves can never post defaults. (Alerts and weather load separately; see below.)
  it('shows a loading state, and none of the Save buttons, until /api/settings arrives', async () => {
    mount(['/api/settings']);
    render(<SettingsPage />);

    await screen.findByText('Loading settings…');
    expect(screen.queryByRole('button', { name: /^Save/ })).toBeNull();
    expect(apiPostMock).not.toHaveBeenCalled();

    await release('/api/settings');
    expect(await screen.findByRole('heading', { name: 'EV Charger' })).toBeInTheDocument();
  });

  it('does not stay on the spinner when the settings load fails', async () => {
    mount(['/api/settings']);
    render(<SettingsPage />);
    await screen.findByText('Loading settings…');

    await fail('/api/settings');

    expect(await screen.findByRole('heading', { name: 'EV Charger' })).toBeInTheDocument();
  });
});

describe('<SettingsPage/> — notifications wait for the alert config', () => {
  it('Save Notification Settings is disabled until /api/alerts has loaded', async () => {
    mount(['/api/alerts']);
    render(<SettingsPage />);
    const s = await section('Notifications');
    expect(button(s, 'Save Notification Settings')).toBeDisabled();

    await release('/api/alerts');

    await waitFor(() => expect(button(s, 'Save Notification Settings')).toBeEnabled());
  });

  it('never posts default alert settings over the saved ones', async () => {
    mount(['/api/alerts']);
    render(<SettingsPage />);
    const s = await section('Notifications');
    fireEvent.click(button(s, 'Save Notification Settings'));
    expect(apiPostMock).not.toHaveBeenCalledWith('/api/alerts', expect.anything());

    await release('/api/alerts');
    await waitFor(() => expect(button(s, 'Save Notification Settings')).toBeEnabled());
    fireEvent.click(button(s, 'Save Notification Settings'));

    await waitFor(() =>
      expect(apiPostMock).toHaveBeenCalledWith('/api/alerts', expect.objectContaining({ telegram_bot_token: 'secret-token' })),
    );
  });

  it('stays locked, and says why, when the alert config fails to load', async () => {
    mount(['/api/alerts']);
    render(<SettingsPage />);
    const s = await section('Notifications');

    await fail('/api/alerts');

    await within(s).findByText(/saved notification settings could not be loaded/);
    expect(button(s, 'Save Notification Settings')).toBeDisabled();
    // The defaults on screen must never reach the server.
    fireEvent.click(button(s, 'Save Notification Settings'));
    expect(apiPostMock).not.toHaveBeenCalledWith('/api/alerts', expect.anything());
  });

  it('treats a response that is not a success as a failed load', async () => {
    mount([]);
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/alerts') return { ok: false };
      if (path === '/api/settings') return { ok: true, data: settingsBody() };
      if (path === '/api/weather') return { ok: true, data: weatherBody() };
      return { ok: true, lan_ip: null, clients: [], client_count: 0 };
    });
    render(<SettingsPage />);
    const s = await section('Notifications');
    await within(s).findByText(/saved notification settings could not be loaded/);
    expect(button(s, 'Save Notification Settings')).toBeDisabled();
  });

  it('shows no warning when the alert config loads', async () => {
    mount([]);
    render(<SettingsPage />);
    const s = await section('Notifications');
    await waitFor(() => expect(button(s, 'Save Notification Settings')).toBeEnabled());
    expect(within(s).queryByText(/could not be loaded/)).toBeNull();
  });

  it('is inert until the config has loaded, so nothing typed can be lost', async () => {
    mount(['/api/alerts']);
    render(<SettingsPage />);
    const s = await section('Notifications');
    expect(s).toHaveAttribute('inert');

    await release('/api/alerts');

    await waitFor(() => expect(s).not.toHaveAttribute('inert'));
  });

  it('stays inert after a failed load', async () => {
    mount(['/api/alerts']);
    render(<SettingsPage />);
    const s = await section('Notifications');
    await fail('/api/alerts');
    await within(s).findByText(/could not be loaded/);
    expect(s).toHaveAttribute('inert');
  });
});

describe('<SettingsPage/> — weather offers no Save until it has loaded', () => {
  // Existing behaviour, pinned: the location form (and its Save button) only
  // renders once the saved weather config has loaded and is enabled, so a
  // blank postcode can never be posted over the saved one.
  it('has no Save Location button before /api/weather arrives, and has one after', async () => {
    mount(['/api/weather']);
    render(<SettingsPage />);
    const s = await section('Local Weather');
    expect(within(s).queryByRole('button', { name: /Save Location/ })).toBeNull();

    await release('/api/weather');

    expect(await within(s).findByRole('button', { name: 'Save Location' })).toBeEnabled();
  });
});
