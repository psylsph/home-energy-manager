import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, within } from '@testing-library/react';

// ---------------------------------------------------------------------------
// More SettingsPage handlers with no failure-path coverage:
//
//  - Start on Login: the plugin call, the Startup-folder fallback when it
//    fails, and the revert when both fail (Tauri shell only).
//  - Minimise to Tray / Start Hidden: the optimistic toggle reverting.
//  - Saving the inverter connection: the restart prompt when the host changed.
//  - Generating the API key and applying the network settings: failures.
//  - The page staying usable when its initial loads fail.
//  - The noise-threshold clamp, the update banner link and external links.
// ---------------------------------------------------------------------------

type Json = Record<string, unknown>;

const apiGetMock = vi.fn();
const apiPostMock = vi.fn();
const openExternalMock = vi.fn();
const enableMock = vi.fn();
const disableMock = vi.fn();
const invokeMock = vi.fn();

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
vi.mock('@tauri-apps/plugin-autostart', () => ({
  enable: (...args: unknown[]) => enableMock(...args),
  disable: (...args: unknown[]) => disableMock(...args),
}));
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

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

function mount(overrides: Json = {}) {
  apiGetMock.mockImplementation(async (path: string) => {
    if (path === '/api/settings') return { ok: true, data: settings(overrides) };
    if (path === '/api/alerts') return { ok: true, data: { config: {} } };
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
    if (path === '/api/status') return { ok: true, lan_ip: null, clients: [], client_count: 0 };
    return { ok: true, data: {} };
  });
  apiPostMock.mockResolvedValue({ ok: true, message: 'Saved' });
}

function failPost(path: string, error: unknown = new Error('rejected')) {
  apiPostMock.mockImplementation(async (p: string) => {
    if (p === path) throw error;
    return { ok: true, message: 'Saved' };
  });
}

const tauriWindow = window as unknown as { __TAURI_INTERNALS__?: unknown };

/** A labelled switch, found the way assistive technology finds it. */
function rowToggle(label: string): HTMLElement {
  return screen.getByRole('switch', { name: label });
}
const isOn = (toggle: HTMLElement) => toggle.getAttribute('aria-checked') === 'true';

beforeEach(() => {
  vi.spyOn(console, 'error').mockImplementation(() => {});
  vi.spyOn(console, 'warn').mockImplementation(() => {});
  for (const m of [apiGetMock, apiPostMock, openExternalMock, enableMock, disableMock, invokeMock]) m.mockReset();
  enableMock.mockResolvedValue(undefined);
  disableMock.mockResolvedValue(undefined);
  invokeMock.mockResolvedValue(undefined);
  localStorage.removeItem('saved_host');
  useInverterStore.setState({
    snapshot: null,
    connectionState: 'disconnected',
    connectedHost: null,
    developerMode: false,
    evcHost: '',
    latestVersionInfo: null,
    visualNoiseThreshold: 10,
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  cleanup();
  delete tauriWindow.__TAURI_INTERNALS__;
  localStorage.removeItem('saved_host');
});

describe('<SettingsPage/> — Start on Login (Tauri shell)', () => {
  beforeEach(() => {
    tauriWindow.__TAURI_INTERNALS__ = {};
  });

  it('is not offered outside the Tauri shell', async () => {
    delete tauriWindow.__TAURI_INTERNALS__;
    mount();
    render(<SettingsPage />);
    await screen.findByRole('heading', { name: 'App' });
    expect(screen.queryByText('Start on Login')).toBeNull();
  });

  it('registers the entry, persists the choice and confirms it', async () => {
    mount();
    render(<SettingsPage />);
    await screen.findByText('Start on Login');

    fireEvent.click(rowToggle('Start on Login'));

    await screen.findByText('Will start automatically when you log in');
    expect(enableMock).toHaveBeenCalledTimes(1);
    expect(apiPostMock).toHaveBeenCalledWith('/api/settings', { autostart_enabled: true });
    expect(isOn(rowToggle('Start on Login'))).toBe(true);
  });

  it('removes the entry when switched off', async () => {
    mount({ autostart_enabled: true });
    render(<SettingsPage />);
    await screen.findByText('Start on Login');
    await waitFor(() => expect(isOn(rowToggle('Start on Login'))).toBe(true));

    fireEvent.click(rowToggle('Start on Login'));

    await screen.findByText('Will no longer start automatically when you log in');
    expect(disableMock).toHaveBeenCalledTimes(1);
    expect(apiPostMock).toHaveBeenCalledWith('/api/settings', { autostart_enabled: false });
  });

  it('falls back to a Startup-folder shortcut when the plugin call fails', async () => {
    enableMock.mockRejectedValue(new Error('registry access denied'));
    mount();
    render(<SettingsPage />);
    await screen.findByText('Start on Login');

    fireEvent.click(rowToggle('Start on Login'));

    await screen.findByText('Will start automatically when you log in (Startup folder)');
    expect(invokeMock).toHaveBeenCalledWith('autostart_fallback', { enable: true });
    expect(apiPostMock).toHaveBeenCalledWith('/api/settings', { autostart_enabled: true });
    expect(isOn(rowToggle('Start on Login'))).toBe(true);
  });

  it('uses the fallback to remove the entry too', async () => {
    disableMock.mockRejectedValue(new Error('registry access denied'));
    mount({ autostart_enabled: true });
    render(<SettingsPage />);
    await screen.findByText('Start on Login');
    await waitFor(() => expect(isOn(rowToggle('Start on Login'))).toBe(true));

    fireEvent.click(rowToggle('Start on Login'));

    await screen.findByText('Will no longer start automatically when you log in');
    expect(invokeMock).toHaveBeenCalledWith('autostart_fallback', { enable: false });
  });

  it('puts the toggle back and explains both failures when neither route works', async () => {
    enableMock.mockRejectedValue(new Error('plugin failed'));
    invokeMock.mockRejectedValue(new Error('shortcut failed'));
    mount();
    render(<SettingsPage />);
    await screen.findByText('Start on Login');

    fireEvent.click(rowToggle('Start on Login'));

    await screen.findByText('Failed to enable auto-start: plugin failed / shortcut failed');
    await waitFor(() => expect(isOn(rowToggle('Start on Login'))).toBe(false));
    // The choice was never saved.
    expect(apiPostMock).not.toHaveBeenCalledWith('/api/settings', { autostart_enabled: true });
  });

  it('describes a failed disable as a disable', async () => {
    disableMock.mockRejectedValue('plugin failed');
    invokeMock.mockRejectedValue('shortcut failed');
    mount({ autostart_enabled: true });
    render(<SettingsPage />);
    await screen.findByText('Start on Login');
    await waitFor(() => expect(isOn(rowToggle('Start on Login'))).toBe(true));

    fireEvent.click(rowToggle('Start on Login'));

    await screen.findByText('Failed to disable auto-start: plugin failed / shortcut failed');
    await waitFor(() => expect(isOn(rowToggle('Start on Login'))).toBe(true));
  });

  it('puts Minimise to Tray back when the save fails', async () => {
    mount();
    render(<SettingsPage />);
    await screen.findByText('Minimise to Tray');
    failPost('/api/settings', new Error('settings locked'));

    fireEvent.click(rowToggle('Minimise to Tray'));

    await screen.findByText('Failed to update minimise-to-tray: settings locked');
    await waitFor(() => expect(isOn(rowToggle('Minimise to Tray'))).toBe(false));
  });

  it('puts Start Hidden in Tray back when the save fails', async () => {
    mount();
    render(<SettingsPage />);
    await screen.findByText('Start Hidden in Tray');
    failPost('/api/settings', new Error('settings locked'));

    fireEvent.click(rowToggle('Start Hidden in Tray'));

    await screen.findByText('Failed to update start-minimised: settings locked');
    await waitFor(() => expect(isOn(rowToggle('Start Hidden in Tray'))).toBe(false));
  });
});

describe('<SettingsPage/> — saving the inverter connection', () => {
  async function open(overrides: Json = {}) {
    mount(overrides);
    render(<SettingsPage />);
    const heading = await screen.findByRole('heading', { name: 'Inverter Connection' });
    const section = heading.closest('section') as HTMLElement;
    const host = within(section).getByPlaceholderText(/192\.168/) as HTMLInputElement;
    await waitFor(() => expect(host.value).toBe('192.168.1.10'));
    return { section, host, connect: () => within(section).getByRole('button', { name: /^(Connect|Saving…)$/ }) };
  }

  it('asks for a restart when the host differs from the one saved before', async () => {
    localStorage.setItem('saved_host', '192.168.1.99');
    const { connect } = await open();

    fireEvent.click(connect());

    await screen.findByText('Restart Required');
    expect(apiPostMock).toHaveBeenCalledWith('/api/settings', { host: '192.168.1.10', port: 8899, serial: 'SA12345678' });
    expect(localStorage.getItem('saved_host')).toBe('192.168.1.10');

    fireEvent.click(screen.getByRole('button', { name: 'Got it' }));
    await screen.findByText('Connection saved. Restart required.');
    expect(screen.queryByText('Restart Required')).toBeNull();
  });

  it('just reconnects when the host has not changed', async () => {
    localStorage.setItem('saved_host', '192.168.1.10');
    const { connect } = await open();

    fireEvent.click(connect());

    await screen.findByText('Settings saved — reconnecting…');
    expect(screen.queryByText('Restart Required')).toBeNull();
  });

  it('does not ask for a restart on the very first save', async () => {
    const { connect } = await open();
    fireEvent.click(connect());
    await screen.findByText('Settings saved — reconnecting…');
    expect(screen.queryByText('Restart Required')).toBeNull();
    expect(localStorage.getItem('saved_host')).toBe('192.168.1.10');
  });

  it('reports a failed save and remembers nothing', async () => {
    localStorage.setItem('saved_host', '192.168.1.99');
    const { connect } = await open();
    failPost('/api/settings', new Error('write failed'));

    fireEvent.click(connect());

    await screen.findByText('write failed');
    expect(localStorage.getItem('saved_host')).toBe('192.168.1.99');
    expect(screen.queryByText('Restart Required')).toBeNull();
    expect(connect()).toBeEnabled();
  });

  it('uses a generic message for a non-Error failure', async () => {
    const { connect } = await open();
    failPost('/api/settings', null);
    fireEvent.click(connect());
    await screen.findByText('Failed to save settings');
  });

  it('will not connect to an address that is not IPv4', async () => {
    const { host, connect } = await open();
    fireEvent.change(host, { target: { value: 'not-an-ip' } });
    expect(connect()).toBeDisabled();
    fireEvent.click(connect());
    expect(apiPostMock).not.toHaveBeenCalledWith('/api/settings', expect.objectContaining({ host: 'not-an-ip' }));
  });
});

describe('<SettingsPage/> — authenticated API key and network', () => {
  beforeEach(() => {
    useInverterStore.setState({ developerMode: true });
  });
  const generate = () => screen.findByRole('button', { name: /Generate (new )?API key/ });

  it('shows the new secret once and marks a key as configured', async () => {
    mount();
    apiPostMock.mockResolvedValue({ ok: true, data: { api_key: 'secret-abcdef-1234' } });
    render(<SettingsPage />);

    fireEvent.click(await generate());

    await screen.findByText('secret-abcdef-1234');
    expect(apiPostMock).toHaveBeenCalledWith('/api/settings', { api_key_generate: true, api_port: 7338 });
    expect(screen.getByText(/ends 1234/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Clear saved key' })).toBeInTheDocument();
  });

  it('refuses a response that carries no secret rather than showing a blank key', async () => {
    mount();
    apiPostMock.mockResolvedValue({ ok: true, data: {} });
    render(<SettingsPage />);

    fireEvent.click(await generate());

    await screen.findByText('Key generation did not return a secret');
    expect(screen.queryByRole('button', { name: 'Clear saved key' })).toBeNull();
  });

  it('reports a failed generation', async () => {
    mount();
    failPost('/api/settings', new Error('verifier store unavailable'));
    render(<SettingsPage />);
    fireEvent.click(await generate());
    await screen.findByText('verifier store unavailable');
  });

  it('uses a generic message when generation fails without one', async () => {
    mount();
    failPost('/api/settings', null);
    render(<SettingsPage />);
    fireEvent.click(await generate());
    await screen.findByText('Failed to generate API key');
  });

  it('reports a failed network apply, and a generic message when none is given', async () => {
    mount({ api_port: 7338, api_bind_address: '127.0.0.1' });
    render(<SettingsPage />);
    const apply = await screen.findByRole('button', { name: 'Apply network settings' });

    failPost('/api/settings', new Error('address already in use'));
    fireEvent.click(apply);
    await screen.findByText('address already in use');

    failPost('/api/settings', null);
    fireEvent.click(apply);
    await screen.findByText('Failed to save API network settings');
  });

  it('refuses a blank port when applying network settings', async () => {
    mount();
    render(<SettingsPage />);
    const apply = await screen.findByRole('button', { name: 'Apply network settings' });
    fireEvent.change(screen.getByLabelText('Port'), { target: { value: '' } });
    fireEvent.click(apply);
    await screen.findByText('API port cannot be blank');
    expect(apiPostMock).not.toHaveBeenCalledWith('/api/settings', expect.objectContaining({ api_bind_address: null }));
  });
});

describe('<SettingsPage/> — when the initial loads fail', () => {
  it('still renders every section, with defaults, instead of crashing', async () => {
    apiGetMock.mockRejectedValue(new Error('backend not ready'));
    render(<SettingsPage />);

    for (const heading of ['Inverter Connection', 'Notifications', 'Local Weather', 'EV Charger', 'Panel Controls']) {
      expect(await screen.findByRole('heading', { name: heading })).toBeInTheDocument();
    }
    await waitFor(() => expect(console.warn).toHaveBeenCalled());
  });

  it('keeps the page usable when only the weather and status loads fail', async () => {
    apiGetMock.mockImplementation(async (path: string) => {
      if (path === '/api/settings') return { ok: true, data: settings() };
      if (path === '/api/alerts') return { ok: true, data: { config: {} } };
      throw new Error(`${path} unavailable`);
    });
    render(<SettingsPage />);
    const heading = await screen.findByRole('heading', { name: 'Inverter Connection' });
    const host = within(heading.closest('section') as HTMLElement).getByPlaceholderText(/192\.168/) as HTMLInputElement;
    await waitFor(() => expect(host.value).toBe('192.168.1.10'));
  });
});

describe('<SettingsPage/> — smaller controls', () => {
  it('clamps the noise threshold typed into the box to 0-100 and ignores junk', async () => {
    mount();
    render(<SettingsPage />);
    const heading = await screen.findByText('Energy Flow Diagram');
    const section = heading.closest('section') as HTMLElement;
    const box = within(section).getByRole('spinbutton') as HTMLInputElement;

    fireEvent.change(box, { target: { value: '250' } });
    expect(useInverterStore.getState().visualNoiseThreshold).toBe(100);
    fireEvent.change(box, { target: { value: '-40' } });
    expect(useInverterStore.getState().visualNoiseThreshold).toBe(0);
    fireEvent.change(box, { target: { value: '35' } });
    expect(useInverterStore.getState().visualNoiseThreshold).toBe(35);
    // A cleared box (not a number) leaves the value alone.
    fireEvent.change(box, { target: { value: '' } });
    expect(useInverterStore.getState().visualNoiseThreshold).toBe(35);
  });

  it('lets the slider set the noise threshold', async () => {
    mount();
    render(<SettingsPage />);
    const heading = await screen.findByText('Energy Flow Diagram');
    const slider = within(heading.closest('section') as HTMLElement).getByRole('slider');
    fireEvent.change(slider, { target: { value: '55' } });
    expect(useInverterStore.getState().visualNoiseThreshold).toBe(55);
  });

  it('opens the release page from the update notice', async () => {
    mount();
    useInverterStore.setState({
      latestVersionInfo: {
        current_version: '0.85.4',
        latest_version: '9.9.9',
        update_available: true,
        release_url: 'https://example.test/release/9.9.9',
      } as never,
    });
    render(<SettingsPage />);

    fireEvent.click(await screen.findByRole('button', { name: 'View release' }));

    expect(openExternalMock).toHaveBeenCalledWith('https://example.test/release/9.9.9');
  });

  it('falls back to the releases page when no release URL was given', async () => {
    mount();
    useInverterStore.setState({
      latestVersionInfo: { current_version: '0.85.4', latest_version: '9.9.9', update_available: true } as never,
    });
    render(<SettingsPage />);
    fireEvent.click(await screen.findByRole('button', { name: 'View release' }));
    expect(openExternalMock).toHaveBeenCalledWith('https://github.com/psylsph/home-energy-manager/releases/latest');
  });

  it('offers no release button when already up to date', async () => {
    mount();
    useInverterStore.setState({
      latestVersionInfo: { current_version: '0.85.4', latest_version: '0.85.4', update_available: false } as never,
    });
    render(<SettingsPage />);
    // Current and latest are both shown, so the release line is present…
    await screen.findByText(/Latest release:/);
    expect(screen.queryByRole('button', { name: 'View release' })).toBeNull();
  });

  it('opens the project site from About', async () => {
    mount();
    render(<SettingsPage />);
    const about = (await screen.findByRole('heading', { name: 'About' })).closest('section') as HTMLElement;
    fireEvent.click(within(about).getAllByRole('button')[0]);
    expect(openExternalMock).toHaveBeenCalledWith('https://psylsph.github.io/home-energy-manager/');
  });
});
