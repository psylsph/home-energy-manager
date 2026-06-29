import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/agile') return { ok: true, enabled: false };
    if (path === '/api/auto-winter') {
      return {
        ok: true,
        data: {
          config: {
            enabled: false,
            cold_threshold: 8,
            recovery_threshold: 12,
            target_soc: 80,
            debounce_readings: 10,
          },
        },
      };
    }
    if (path === '/api/cosy') return { ok: true, enabled: false, slots: [] };
    if (path === '/api/settings') {
      return {
        ok: true,
        data: {
          import_tariff: 0.285,
          export_tariff: 0.15,
          import_tariff_config: null,
          full_power_discharge_in_eco_mode: false,
        },
      };
    }
    if (path === '/api/load-limiter') {
      return {
        ok: true,
        data: {
          config: {
            enabled: false,
            threshold_w: 3000,
            trigger_delay_minutes: 0,
            start_hour: 0,
            start_minute: 0,
            end_hour: 0,
            end_minute: 0,
          },
        },
      };
    }
    return { ok: true, data: {} };
  }),
  apiPost: vi.fn().mockResolvedValue({ ok: true, data: {} }),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  fetchHistory: vi.fn().mockResolvedValue({}),
  isTauri: false,
}));

import ControlPage from '../../src/pages/ControlPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

// ---------------------------------------------------------------------------
// ControlPage used to render the full set of controls even when the
// inverter's TCP/Modbus connection was lost. That was confusing — the
// schedule editors bound to a stale snapshot, the Quick Action buttons
// POSTed to a backend that couldn't reach the dongle, and the sliders
// presented values the user couldn't trust. The fix mirrors the
// StatusPage / InverterPage pattern: while `connectionState !== 'connected'`,
// the page swaps in a reconnecting screen with the host, a Retry button,
// and an explanation. The controls reappear automatically once the poll
// loop broadcasts Connection { state: Connected }.
// ---------------------------------------------------------------------------

function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    timestamp: Math.floor(Date.now() / 1000),
    solar_power: 0,
    pv1_power: 0,
    pv2_power: 0,
    pv1_voltage: 0,
    pv2_voltage: 0,
    pv1_current: 0,
    pv2_current: 0,
    battery_power: 0,
    soc: 50,
    battery_voltage: 50,
    battery_current: 0,
    battery_state: 'idle',
    battery_temperature: 20,
    battery_capacity_kwh: 9.5,
    eps_power_w: 0,
    grid_power: 0,
    grid_voltage: 240,
    grid_frequency: 50,
    grid_online: true,
    grid_loss: false,
    inverter_trip: false,
    battery_over_temp: false,
    home_power: 0,
    inverter_temperature: 25,
    inverter_time: '',
    today_solar_kwh: 0,
    today_pv1_kwh: 0,
    today_pv2_kwh: 0,
    today_import_kwh: 0,
    today_export_kwh: 0,
    today_charge_kwh: 0,
    total_import_kwh: 0,
    total_export_kwh: 0,
    total_solar_kwh: 0,
    total_charge_kwh: 0,
    total_discharge_kwh: 0,
    total_throughput_kwh: 0,
    operating_hours: 0,
    today_discharge_kwh: 0,
    today_consumption_kwh: 0,
    home_energy_today_kwh: 0,
    battery_modules: [],
    battery_mode: 'eco',
    battery_power_mode: 1,
    battery_reserve: 20,
    charge_rate: 50,
    discharge_rate: 50,
    active_power_rate: 100,
    max_battery_power_w: 5000,
    max_ac_power_w: 5000,
    export_limit_w: 0,
    target_soc: 4,
    enable_charge_target: false,
    enable_charge: false,
    enable_discharge: false,
    auto_winter_active: false,
    load_limiter_active: false,
    cosy_active: false,
    cosy_enabled: false,
    agile_active: false,
    agile_state: 'idle',
    agile_enabled: false,
    max_charge_slots: 2,
    max_discharge_slots: 2,
    charge_slots: [
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
    ],
    discharge_slots: [
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
    ],
    meters: [],
    inverter_serial: 'FD2328G358',
    firmware_version: '318',
    dsp_firmware_version: '318',
    dc_dsp_firmware_version: '',
    device_type: 'gen3',
    device_type_display: 'Gen3',
    device_type_code: '2001',
    battery_calibration_stage: 0,
    enable_ammeter: false,
    enable_reversed_ct_clamp: false,
    meter_type: 0,
    supports_battery_calibration: false,
    ac_eps_enabled: false,
    ac_export_priority: 0,
    battery_pause_mode: 0,
    battery_pause_slot: {
      enabled: false,
      start_hour: 0,
      start_minute: 0,
      end_hour: 0,
      end_minute: 0,
      target_soc: 100,
    },
    ...overrides,
  };
}

describe('<ControlPage/> — connection-state gate', () => {
  beforeEach(() => {
    silenceConsoleError();
    vi.stubGlobal(
      'matchMedia',
      vi.fn().mockImplementation((query: string) => ({
        matches: false,
        media: query,
        onchange: null,
        addListener: vi.fn(),
        removeListener: vi.fn(),
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        dispatchEvent: vi.fn(),
      })),
    );
    // jsdom's fetch returns undefined by default. Stub it so the Retry
    // button's POST to /api/reconnect resolves cleanly.
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true }));
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    cleanup();
    useInverterStore.setState({ snapshot: null, connectionState: 'disconnected' });
  });

  it('shows the reconnecting screen and hides all controls while reconnecting', () => {
    // Even though we have a fresh snapshot, the gate fires on
    // connectionState (not snapshot nullity). Showing controls against a
    // snapshot from a session that has since dropped was the source of the
    // "really confusing" report — the schedule editors would bind to
    // stale values and Save buttons would target a backend that can't
    // reach the dongle.
    useInverterStore.setState({
      snapshot: makeSnapshot(),
      developerMode: false,
      connectionState: 'reconnecting',
      connectedHost: '192.168.1.36:8899',
    });
    render(<ControlPage />);

    // The banner wording is the same one the StatusPage / InverterPage
    // use for this state — keep the strings in sync so a user moving
    // between pages sees the same vocabulary.
    expect(
      screen.getByText('Connection lost — reconnecting…'),
    ).toBeDefined();
    // The host string is split across two text nodes ("Host: " + the
    // IP), so use a function matcher instead of getByText. The trailing
    // :8899 is stripped — we display just the IP, same as the StatusPage
    // status bar. Use getAllByText with a custom matcher to find the
    // leaf node and avoid the multiple-matches error from parent
    // elements.
    const hostNodes = screen.getAllByText((_content, node) => {
      if (!node) return false;
      const text = node.textContent ?? '';
      // Leaf nodes only — exclude ancestors that also contain the IP
      // through their children. Matches either "Host: 192.168.1.36" or
      // the bare IP.
      const hasIp = text.includes('192.168.1.36');
      const isLeaf = Array.from(node.childNodes).every(
        (c) => c.nodeType !== 1 || (c as Element).tagName === undefined,
      );
      return hasIp && (isLeaf || text.trim() === '192.168.1.36');
    });
    expect(hostNodes.length).toBeGreaterThan(0);

    // None of the page's section headings should be rendered. We assert on
    // the most distinctive ones (Battery Mode, Quick Actions, Charge
    // Schedule) — a future control addition that accidentally leaks past
    // the gate would trip one of these.
    expect(screen.queryByRole('heading', { name: 'Quick Actions', exact: true })).toBeNull();
    expect(screen.queryByRole('heading', { name: 'Battery Mode', exact: true })).toBeNull();
    expect(screen.queryByRole('heading', { name: 'Charge Schedule', exact: true })).toBeNull();
    expect(screen.queryByRole('heading', { name: 'Discharge Schedule', exact: true })).toBeNull();
    expect(screen.queryByRole('heading', { name: 'Battery and Power Controls', exact: true })).toBeNull();

    // The Retry button is there so the user can poke the backend instead
    // of waiting for the poll loop's own back-off.
    expect(screen.getByRole('button', { name: /Retry now/i })).toBeDefined();
  });

  it('shows the disconnected wording and hides controls when the loop has given up', () => {
    useInverterStore.setState({
      snapshot: null,
      developerMode: false,
      connectionState: 'disconnected',
      connectedHost: '192.168.1.36:8899',
    });
    render(<ControlPage />);

    // 'disconnected' is the initial state before any TCP attempt has
    // succeeded and also the state the poll loop enters after
    // sustained failures where it can't even open a socket. Either way,
    // the page must not render controls.
    expect(
      screen.getByText('Disconnected — will retry automatically'),
    ).toBeDefined();
    expect(screen.queryByRole('heading', { name: 'Battery Mode', exact: true })).toBeNull();
  });

  it('renders the full controls once the connection is restored', async () => {
    // Regression guard: the gate must be transparent in the steady state.
    // If a future change accidentally persists the gate across the
    // connected transition (e.g. by tracking snapshot-presence instead of
    // connectionState), this test fails because no Battery Mode heading
    // would render.
    useInverterStore.setState({
      snapshot: makeSnapshot(),
      developerMode: false,
      connectionState: 'connected',
      connectedHost: '192.168.1.36:8899',
    });
    render(<ControlPage />);

    // None of the disconnect-screen copy should be present.
    expect(screen.queryByText(/Connection lost/i)).toBeNull();
    expect(screen.queryByText(/Disconnected/i)).toBeNull();

    // And the actual page content should render — we just check the
    // distinctive headings, the same way the other ControlPage tests do.
    expect(
      await screen.findByRole('heading', { name: 'Quick Actions', exact: true }),
    ).toBeDefined();
    expect(
      await screen.findByRole('heading', { name: 'Battery Mode', exact: true }),
    ).toBeDefined();
  });

  it('POSTs to /api/reconnect when the Retry button is clicked', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal('fetch', fetchMock);

    useInverterStore.setState({
      snapshot: makeSnapshot(),
      developerMode: false,
      connectionState: 'reconnecting',
      connectedHost: '192.168.1.36:8899',
    });
    render(<ControlPage />);

    const retryButton = screen.getByRole('button', { name: /Retry now/i });
    fireEvent.click(retryButton);

    // The button drives `POST /api/reconnect` directly (same endpoint the
    // StatusPage retry button uses) so a click from the ControlPage
    // yields the same backend behaviour.
    expect(fetchMock).toHaveBeenCalledWith('/api/reconnect', { method: 'POST' });

    // The button shows "Reconnecting…" while the request is in flight.
    expect(screen.getByText(/Reconnecting…/)).toBeDefined();
  });

  it('swallows network errors from the Retry click so the page does not crash', () => {
    // A failed POST to /api/reconnect is not an error the page should
    // surface — the poll loop's own back-off keeps retrying, so the
    // user's manual retry is best-effort. A thrown promise here would
    // surface as an unhandled rejection in production.
    const fetchMock = vi.fn().mockRejectedValue(new Error('network down'));
    vi.stubGlobal('fetch', fetchMock);

    useInverterStore.setState({
      snapshot: makeSnapshot(),
      developerMode: false,
      connectionState: 'reconnecting',
      connectedHost: '192.168.1.36:8899',
    });
    render(<ControlPage />);

    // The click must not throw and the gate must remain visible (so the
    // user can keep retrying or wait for the backend's automatic back-off).
    const retryButton = screen.getByRole('button', { name: /Retry now/i });
    expect(() => fireEvent.click(retryButton)).not.toThrow();
    expect(screen.getByText(/Connection lost/i)).toBeDefined();
    expect(fetchMock).toHaveBeenCalled();
  });
});