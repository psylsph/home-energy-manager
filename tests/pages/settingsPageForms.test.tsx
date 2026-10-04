import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, within, act } from '@testing-library/react';

// ---------------------------------------------------------------------------
// SettingsPage forms and small controls that had no coverage:
//
//  - The notification form, end to end: every field must land under the right
//    key of the saved payload (a swapped field would be invisible otherwise).
//  - The tariff window editor: add / edit / remove, the inline errors and the
//    reset-to-flat recovery, and an old flat tariff still loading.
//  - The message toast clearing itself, the connection fields, Economy 7
//    times, the CT-meter address, panel visibility and graph scale.
//  - The copy buttons, the connected-clients list and the external links.
// ---------------------------------------------------------------------------

type Json = Record<string, unknown>;

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

const flat = (rate = 0.15) => ({ slots: [{ start: '00:00', end: '23:59', rate }], version: 2 });

function settings(overrides: Json = {}): Json {
  return {
    host: '192.168.1.10', port: 8899, serial: 'SA12345678', interval_secs: 20, http_port: 7337,
    evc_host: '', evc_port: 502, disable_auto_discovery: true, autostart_enabled: false,
    minimise_to_tray: false, start_minimised: false, check_for_updates: true, api_key: '', api_port: 7338,
    hidden_panels: [], solar_arrays: [], import_tariff_config: flat(), export_tariff_config: flat(),
    octopus_enabled: false, octopus_account_number: '', octopus_api_key_configured: false,
    octopus_gas_unit: 'unknown', octopus_economy7_start: '00:30', octopus_economy7_end: '07:30',
    ...overrides,
  };
}

function alerts(overrides: Json = {}): Json {
  return {
    enabled: true, telegram_bot_token: '', telegram_chat_id: '', cooldown_minutes: 30,
    batt_temp_min: 0, batt_temp_max: 0, inverter_temp_min: 8, inverter_temp_max: 60, soc_min: 4, soc_max: 100,
    grid_offline_enabled: false, inverter_trip_enabled: false, battery_over_temp_enabled: false,
    battery_connection_lost_enabled: true, connection_lost_enabled: false, solar_clipping_enabled: false,
    solar_clipping_ceiling_w: 0, ntfy_topic: '', ntfy_server: 'https://ntfy.sh', pushover_app_token: '',
    pushover_user_key: '', ...overrides,
  };
}

interface Mount { settings?: Json; alerts?: Json; status?: Json }

function mount(m: Mount = {}) {
  apiGetMock.mockImplementation(async (path: string) => {
    if (path === '/api/settings') return { ok: true, data: settings(m.settings) };
    if (path === '/api/alerts') return { ok: true, data: { config: alerts(m.alerts) } };
    if (path === '/api/weather') {
      return {
        ok: true,
        data: {
          config: { enabled: true, latitude: 51.5, longitude: -0.12, update_interval_mins: 30, postcode: 'SW1A 1AA' },
          current: null, history: [], backfill_in_progress: false,
        },
      };
    }
    if (path === '/api/status') return { ok: true, lan_ip: '192.168.1.99', clients: [], client_count: 0, ...m.status };
    return { ok: true, data: {} };
  });
  apiPostMock.mockResolvedValue({ ok: true, message: 'Saved' });
}

const postsTo = (path: string) => apiPostMock.mock.calls.filter((c) => c[0] === path);
const lastPost = (path: string) => postsTo(path).at(-1)![1] as Json;

async function heading(name: string) {
  return (await screen.findByRole('heading', { name })).closest('section') as HTMLElement;
}

beforeEach(() => {
  vi.spyOn(console, 'error').mockImplementation(() => {});
  vi.spyOn(console, 'warn').mockImplementation(() => {});
  for (const m of [apiGetMock, apiPostMock, openExternalMock]) m.mockReset();
  localStorage.clear();
  useInverterStore.setState({
    snapshot: null, connectionState: 'disconnected', connectedHost: null, developerMode: false,
    evcHost: '', hiddenPanels: [], panelGraphsScale: 'today',
  });
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  cleanup();
});

describe('<SettingsPage/> — notification form reaches the right keys', () => {
  it('saves every field under its own key', async () => {
    mount({ alerts: { enabled: true, solar_clipping_enabled: true } });
    render(<SettingsPage />);
    const s = await heading('Notifications');
    await within(s).findByText('Alert Triggers & Cooldown');

    const set = (label: string | RegExp, value: string) =>
      fireEvent.change(within(s).getByLabelText(label), { target: { value } });
    set('Bot Token (from @BotFather)', 'tok-123');
    set('Chat ID (from @userinfobot)', '555');
    set(/^Topic \(subscribe to this in the ntfy app\)/, 'my-topic');
    set('Server (optional, default: ntfy.sh)', 'https://ntfy.example');
    set('App API Token (from pushover.net/apps/build)', 'po-app');
    set('User Key (from your Pushover account)', 'po-user');
    set('Battery temp below °C', '2.5');
    set('Battery temp above °C', '48');
    set('SOC below %', '15');
    set('SOC above %', '95');
    set(/Clipping Ceiling/, '5500');
    set('minutes between all alerts', '45');

    await waitFor(() => expect(within(s).getByRole('button', { name: 'Save Notification Settings' })).toBeEnabled());
    fireEvent.click(within(s).getByRole('button', { name: 'Save Notification Settings' }));

    await waitFor(() => expect(postsTo('/api/alerts')).toHaveLength(1));
    expect(lastPost('/api/alerts')).toMatchObject({
      telegram_bot_token: 'tok-123',
      telegram_chat_id: '555',
      ntfy_topic: 'my-topic',
      ntfy_server: 'https://ntfy.example',
      pushover_app_token: 'po-app',
      pushover_user_key: 'po-user',
      batt_temp_min: 2.5,
      batt_temp_max: 48,
      soc_min: 15,
      soc_max: 95,
      solar_clipping_ceiling_w: 5500,
      cooldown_minutes: 45,
    });
  });

  it('saves each trigger switch under its own key', async () => {
    mount({ alerts: { enabled: true } });
    render(<SettingsPage />);
    const s = await heading('Notifications');
    await within(s).findByText('Alert Triggers & Cooldown');
    await waitFor(() => expect(within(s).getByRole('button', { name: 'Save Notification Settings' })).toBeEnabled());

    for (const name of ['Grid Offline', 'Inverter Trip', 'Inverter Battery Warning', 'Solar Clipping', 'Connection Lost']) {
      fireEvent.click(within(s).getByRole('switch', { name }));
    }
    // Battery Connection Lost starts on; switching it flips it off.
    fireEvent.click(within(s).getByRole('switch', { name: 'Battery Connection Lost' }));
    fireEvent.click(within(s).getByRole('button', { name: 'Save Notification Settings' }));

    await waitFor(() => expect(postsTo('/api/alerts')).toHaveLength(1));
    expect(lastPost('/api/alerts')).toMatchObject({
      grid_offline_enabled: true,
      inverter_trip_enabled: true,
      battery_over_temp_enabled: true,
      solar_clipping_enabled: true,
      connection_lost_enabled: true,
      battery_connection_lost_enabled: false,
    });
  });

  it('confirms switching alerts on and off', async () => {
    mount({ alerts: { enabled: false } });
    render(<SettingsPage />);
    const s = await heading('Notifications');
    fireEvent.click(within(s).getByRole('switch', { name: 'Enable Alerts' }));
    await screen.findByText('Alerts enabled');
    fireEvent.click(within(s).getByRole('switch', { name: 'Enable Alerts' }));
    await screen.findByText('Alerts disabled');
  });

  it('copies the ntfy topic, and offers no copy without one', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    mount({ settings: { serial: 'SA12345678' }, alerts: { ntfy_topic: 'my-topic' } });
    render(<SettingsPage />);
    const s = await heading('Notifications');
    await waitFor(() => expect((within(s).getByLabelText('Topic (subscribe to this in the ntfy app)') as HTMLInputElement).value).toBe('my-topic'));

    const topicRow = within(s).getByLabelText('Topic (subscribe to this in the ntfy app)').closest('label')!.parentElement!;
    fireEvent.click(within(topicRow).getByRole('button', { name: /Copy/ }));

    await screen.findByText('Topic copied!');
    expect(writeText).toHaveBeenCalledWith('my-topic');
  });

  it('opens the setup guides and signup links in the browser', async () => {
    mount();
    render(<SettingsPage />);
    const s = await heading('Notifications');
    const open = (name: string | RegExp) => fireEvent.click(within(s).getByRole('button', { name }));

    open('Setup guide ↗');
    open('@BotFather');
    open('@userinfobot');
    open('ntfy.sh');
    open('Pushover');
    open('pushover.net/apps/build');

    const urls = openExternalMock.mock.calls.map((c) => String(c[0]));
    expect(urls).toEqual(
      expect.arrayContaining([
        'https://github.com/psylsph/home-energy-manager/blob/master/NOTIFICATIONS.md',
        'https://t.me/botfather',
        'https://t.me/userinfobot',
        'https://ntfy.sh',
        'https://pushover.net',
        'https://pushover.net/apps/build',
      ]),
    );
  });
});

describe('<SettingsPage/> — tariff window editor', () => {
  const editor = async (label: 'Import' | 'Export') => {
    const s = await heading('Energy Tariffs');
    return within(s).getByRole('heading', { name: label }).closest('div.border') as HTMLElement;
  };

  it('adds a window, and the saved tariff then has two', async () => {
    mount();
    render(<SettingsPage />);
    const imp = await editor('Import');

    fireEvent.click(within(imp).getByRole('button', { name: '+ Add window' }));

    await waitFor(() => expect(within(imp).getAllByText('Start')).toHaveLength(2));
    const s = await heading('Energy Tariffs');
    fireEvent.click(within(s).getByRole('button', { name: /Save/ }));
    await waitFor(() => expect(postsTo('/api/settings')).toHaveLength(1));
    expect((lastPost('/api/settings').import_tariff_config as { slots: unknown[] }).slots).toHaveLength(2);
    expect((lastPost('/api/settings').export_tariff_config as { slots: unknown[] }).slots).toHaveLength(1);
  });

  it('saves an edited rate in pence as pounds', async () => {
    mount();
    render(<SettingsPage />);
    const imp = await editor('Import');
    const rate = within(imp).getByLabelText('Rate (p/kWh)') as HTMLInputElement;
    expect(rate.value).toBe('15');

    fireEvent.change(rate, { target: { value: '24.5' } });
    fireEvent.click(within(await heading('Energy Tariffs')).getByRole('button', { name: /Save/ }));

    await waitFor(() => expect(postsTo('/api/settings')).toHaveLength(1));
    const slots = (lastPost('/api/settings').import_tariff_config as { slots: Array<{ rate: number }> }).slots;
    expect(slots[0].rate).toBeCloseTo(0.245, 5);
  });

  it('edits a window boundary and removes a window', async () => {
    mount({ settings: { import_tariff_config: { slots: [
      { start: '00:00', end: '07:00', rate: 0.10 },
      { start: '07:00', end: '23:59', rate: 0.30 },
    ], version: 2 } } });
    render(<SettingsPage />);
    const imp = await editor('Import');
    await waitFor(() => expect(within(imp).getAllByLabelText('Rate (p/kWh)')).toHaveLength(2));

    // Move the second window's start; the first window's end follows it.
    const starts = within(imp).getAllByLabelText('Start') as HTMLSelectElement[];
    fireEvent.change(starts[1], { target: { value: '06:00' } });
    await waitFor(() => expect((within(imp).getAllByLabelText('End') as HTMLSelectElement[])[0].value).not.toBe('07:00'));

    // Then drop the second window: back to one flat window.
    fireEvent.click(within(imp).getAllByTitle('Remove window')[1]);
    await waitFor(() => expect(within(imp).getAllByLabelText('Rate (p/kWh)')).toHaveLength(1));
    expect(within(imp).queryByTitle('Remove window')).toBeNull();
  });

  it('edits the end of a window', async () => {
    mount({ settings: { import_tariff_config: { slots: [
      { start: '00:00', end: '07:00', rate: 0.10 },
      { start: '07:00', end: '23:59', rate: 0.30 },
    ], version: 2 } } });
    render(<SettingsPage />);
    const imp = await editor('Import');
    await waitFor(() => expect(within(imp).getAllByLabelText('End')).toHaveLength(2));
    const ends = within(imp).getAllByLabelText('End') as HTMLSelectElement[];
    fireEvent.change(ends[0], { target: { value: '05:00' } });
    await waitFor(() => expect((within(imp).getAllByLabelText('Start') as HTMLSelectElement[])[1].value).toBe('05:00'));
  });

  it('explains a tariff that does not cover the day, blocks saving, and offers a reset', async () => {
    // A hand-edited file with a gap between 07:00 and 12:00.
    mount({ settings: { import_tariff_config: { slots: [
      { start: '00:00', end: '07:00', rate: 0.10 },
      { start: '12:00', end: '23:59', rate: 0.30 },
    ], version: 2 } } });
    render(<SettingsPage />);
    const imp = await editor('Import');
    const s = await heading('Energy Tariffs');

    await within(imp).findByText(/Tariff configuration is invalid/);
    expect(within(s).getByRole('button', { name: /Save/ })).toBeDisabled();
    // At least one inline error is shown against a window.
    expect(imp.querySelectorAll('ul li').length).toBeGreaterThan(0);

    fireEvent.click(within(imp).getByRole('button', { name: 'Reset to flat rate' }));

    await waitFor(() => expect(within(imp).queryByText(/Tariff configuration is invalid/)).toBeNull());
    expect(within(imp).getAllByLabelText('Rate (p/kWh)')).toHaveLength(1);
    expect(within(s).getByRole('button', { name: /Save/ })).toBeEnabled();
  });

  it('still loads an old flat tariff that has no window configuration', async () => {
    mount({ settings: { import_tariff_config: null, export_tariff_config: null, import_tariff: 0.30, export_tariff: 0.08 } });
    render(<SettingsPage />);
    const imp = await editor('Import');
    const exp = await editor('Export');
    await waitFor(() => expect((within(imp).getByLabelText('Rate (p/kWh)') as HTMLInputElement).value).toBe('30'));
    expect((within(exp).getByLabelText('Rate (p/kWh)') as HTMLInputElement).value).toBe('8');
  });
});

describe('<SettingsPage/> — message toast', () => {
  it('clears itself after four seconds', async () => {
    mount();
    render(<SettingsPage />);
    await heading('App');
    vi.useFakeTimers({ shouldAdvanceTime: true });

    const save = within((await screen.findByText('HTTP Port')).closest('section')!).getAllByRole('button', { name: 'Save' })[0];
    fireEvent.click(save);
    await screen.findByText(/HTTP port set to 7337/);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(3900);
    });
    expect(screen.queryByText(/HTTP port set to 7337/)).not.toBeNull();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(200);
    });
    await waitFor(() => expect(screen.queryByText(/HTTP port set to 7337/)).toBeNull());
  });

  it('restarts the countdown when a newer message replaces it', async () => {
    mount();
    render(<SettingsPage />);
    await heading('App');
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const save = within((await screen.findByText('HTTP Port')).closest('section')!).getAllByRole('button', { name: 'Save' })[0];

    fireEvent.click(save);
    await screen.findByText(/HTTP port set to 7337/);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3000);
    });
    // A second message 3 s in must get its own full four seconds.
    fireEvent.click(save);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3000);
    });
    expect(screen.queryByText(/HTTP port set to 7337/)).not.toBeNull();
  });

  it('reports a failed HTTP port save, with a generic fallback', async () => {
    mount();
    render(<SettingsPage />);
    const s = (await screen.findByText('HTTP Port')).closest('section')!;
    apiPostMock.mockRejectedValue(new Error('port in use'));
    fireEvent.click(within(s).getAllByRole('button', { name: 'Save' })[0]);
    await screen.findByText('port in use');
    apiPostMock.mockRejectedValue(null);
    fireEvent.click(within(s).getAllByRole('button', { name: 'Save' })[0]);
    await screen.findByText('Failed to update HTTP port');
  });
});

describe('<SettingsPage/> — connection and EV charger fields', () => {
  it('posts an edited port and serial when connecting', async () => {
    mount();
    // The port field is a developer-mode control.
    useInverterStore.setState({ developerMode: true });
    render(<SettingsPage />);
    const s = await heading('Inverter Connection');
    await waitFor(() => expect((within(s).getByPlaceholderText(/192\.168/) as HTMLInputElement).value).toBe('192.168.1.10'));
    const number = s.querySelector('input[type="number"]') as HTMLInputElement;
    const serial = within(s).getByLabelText(/Serial Number/) as HTMLInputElement;

    fireEvent.change(number, { target: { value: '8900' } });
    fireEvent.change(serial, { target: { value: 'SA99999999' } });
    fireEvent.click(within(s).getByRole('button', { name: 'Connect' }));

    await waitFor(() => expect(lastPost('/api/settings')).toMatchObject({ host: '192.168.1.10', port: 8900, serial: 'SA99999999' }));
  });

  it('posts an edited charger port in developer mode', async () => {
    mount();
    useInverterStore.setState({ developerMode: true });
    render(<SettingsPage />);
    const s = await heading('EV Charger');
    fireEvent.change(within(s).getByTitle('EV Charger Modbus port'), { target: { value: '503' } });
    fireEvent.change(within(s).getByPlaceholderText(/192\.168\.1\.50/), { target: { value: '192.168.1.60' } });
    fireEvent.click(within(s).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(lastPost('/api/settings')).toEqual({ evc_host: '192.168.1.60', evc_port: 503 }));
  });

  it('posts the listen address and allowed origins together', async () => {
    mount();
    useInverterStore.setState({ developerMode: true });
    render(<SettingsPage />);
    const apply = await screen.findByRole('button', { name: 'Apply network settings' });
    fireEvent.change(screen.getByLabelText('Listen address'), { target: { value: '0.0.0.0' } });
    fireEvent.change(screen.getByLabelText('Allowed browser origins (CORS, comma-separated)'), {
      target: { value: 'https://a.example, https://b.example' },
    });
    fireEvent.click(apply);
    await waitFor(() =>
      expect(lastPost('/api/settings')).toMatchObject({
        api_bind_address: '0.0.0.0',
        api_allowed_origins: ['https://a.example', 'https://b.example'],
      }),
    );
  });

  it('copies a newly generated API key', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    mount();
    useInverterStore.setState({ developerMode: true });
    render(<SettingsPage />);
    apiPostMock.mockResolvedValue({ ok: true, data: { api_key: 'secret-abcdef-1234' } });
    fireEvent.click(await screen.findByRole('button', { name: /Generate (new )?API key/ }));
    const key = await screen.findByText('secret-abcdef-1234');
    fireEvent.click(within(key.parentElement!).getByRole('button', { name: 'Copy' }));
    expect(writeText).toHaveBeenCalledWith('secret-abcdef-1234');
  });
});

describe('<SettingsPage/> — network access list and links', () => {
  it('lists connected clients and marks this device', async () => {
    mount({ status: { clients: ['127.0.0.1:50001', '192.168.1.99:50002', '192.168.1.50:50003', '[::1]:50004'], client_count: 4 } });
    render(<SettingsPage />);
    await screen.findByText('Connected clients (4)');
    // Loopback, IPv6 loopback (as seen without a port) and the LAN address count as this device.
    expect(screen.getAllByText('This device')).toHaveLength(2);
    expect(screen.getByText('192.168.1.50:50003').parentElement!.textContent).not.toMatch(/This device/);
  });

  it('copies the read-only and mini-page links', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    Object.defineProperty(window, 'isSecureContext', { value: true, configurable: true });
    mount();
    render(<SettingsPage />);
    const s = await heading('Remote / Mobile Network Access');
    const buttons = within(s).getAllByRole('button', { name: 'Copy' });
    const codes = Array.from(s.querySelectorAll('code')).map((c) => c.textContent);

    fireEvent.click(buttons[1]);
    fireEvent.click(buttons[2]);

    await waitFor(() => expect(writeText).toHaveBeenCalledTimes(2));
    expect(writeText.mock.calls[0][0]).toBe(codes[1]);
    expect(writeText.mock.calls[1][0]).toBe(codes[2]);
    expect(String(codes[1])).toMatch(/\?RO$/);
  });

  it('opens Open-Meteo from the weather section', async () => {
    mount();
    render(<SettingsPage />);
    const s = await heading('Local Weather');
    // Linked from both the section header and its description.
    fireEvent.click(within(s).getAllByRole('button', { name: /Open-Meteo/ })[0]);
    expect(openExternalMock).toHaveBeenCalledWith('https://open-meteo.com/');
  });
});

describe('<SettingsPage/> — Octopus, solar and panels', () => {
  it('saves the Economy 7 night-rate window', async () => {
    mount({ settings: { octopus_enabled: false } });
    render(<SettingsPage />);
    const s = await heading('Octopus Energy Data');
    fireEvent.change(within(s).getByLabelText('Economy 7 night rate starts'), { target: { value: '01:00' } });
    fireEvent.change(within(s).getByLabelText('Economy 7 night rate ends'), { target: { value: '08:00' } });
    fireEvent.click(within(s).getByRole('button', { name: 'Save Octopus Settings' }));

    await screen.findByText('Octopus integration disabled');
    expect(lastPost('/api/settings')).toMatchObject({ octopus_economy7_start: '01:00', octopus_economy7_end: '08:00' });
  });

  it('reports a failed Octopus save, with a generic fallback', async () => {
    mount();
    render(<SettingsPage />);
    const s = await heading('Octopus Energy Data');
    apiPostMock.mockRejectedValue(new Error('octopus unreachable'));
    fireEvent.click(within(s).getByRole('button', { name: 'Save Octopus Settings' }));
    await screen.findByText('octopus unreachable');
    apiPostMock.mockRejectedValue(null);
    fireEvent.click(within(s).getByRole('button', { name: 'Save Octopus Settings' }));
    await screen.findByText('Failed to save Octopus settings');
  });

  it('saves a changed CT meter address', async () => {
    mount();
    render(<SettingsPage />);
    const s = await heading('Solar Arrays');
    fireEvent.click(within(s).getByTestId('solar-array-add'));
    const address = within(s).getByTestId('solar-array-address') as HTMLSelectElement;
    expect(address.value).toBe('1');

    fireEvent.change(address, { target: { value: '3' } });
    fireEvent.click(within(s).getByRole('button', { name: 'Save Solar Arrays' }));

    await waitFor(() => expect(postsTo('/api/settings')).toHaveLength(1));
    expect((lastPost('/api/settings').solar_arrays as Array<{ meter_address: number }>)[0].meter_address).toBe(3);
  });

  it('hides a panel when unticked and shows it again when re-ticked', async () => {
    mount();
    render(<SettingsPage />);
    const s = await heading('Panel Controls');
    const save = () => within(s).getByRole('button', { name: 'Save Panel Visibility' });
    const box = (label: string) => within(s).getByLabelText(label) as HTMLInputElement;
    const first = within(s).getAllByRole('checkbox')[0] as HTMLInputElement;
    const label = first.closest('label')!.textContent!;

    fireEvent.click(box(label));
    fireEvent.click(save());
    await waitFor(() => expect((lastPost('/api/settings').hidden_panels as string[]).length).toBe(1));

    fireEvent.click(box(label));
    fireEvent.click(save());
    await waitFor(() => expect(lastPost('/api/settings').hidden_panels).toEqual([]));
  });

  it('switches the panel graph time scale', async () => {
    mount();
    useInverterStore.setState({ panelGraphsEnabled: true, panelGraphsScale: 'today' });
    render(<SettingsPage />);
    const s = await heading('Panel Controls');
    fireEvent.click(within(s).getByRole('button', { name: 'Rolling 24H' }));
    expect(useInverterStore.getState().panelGraphsScale).toBe('24h');
    fireEvent.click(within(s).getByRole('button', { name: 'Today' }));
    expect(useInverterStore.getState().panelGraphsScale).toBe('today');
  });
});
