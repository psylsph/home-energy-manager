/**
 * Tests for the Load Discharge Limiter status display on ControlPage.
 *
 * Covers issue #158: when the load limiter triggers and pauses battery
 * discharge, the inverter's `battery_mode` flips from `eco` to
 * `eco_paused` (HR 110 = 100%). The previous frontend state derivation
 * gated the "Paused" / "Recovering…" / "Monitoring…" labels on
 * `battery_mode === 'eco'`, so the GUI reverted to showing "Idle" the
 * moment the limiter did its job. The same gate also triggered a
 * misleading "Load limiter only operates in Eco mode (current: eco
 * paused)" banner — the limiter is the thing that put it in Eco Paused.
 *
 * The fix: treat `eco` and `eco_paused` both as "limiter operating"
 * modes for the status label, and only show the banner when the mode
 * is something the limiter genuinely can't run in (Timed, Export).
 *
 * The backend's `check_load_limiter` (`src-tauri/src/inverter/state_machines.rs`)
 * accepts both Eco and EcoPaused; `snapshot.load_limiter_active` remains true
 * through Paused, PausedFromRestart, and LowLoadPending because the battery is
 * still paused during its recovery delay.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, within, fireEvent, waitFor } from '@testing-library/react';

const loadLimiterMock = vi.hoisted(() => ({ enabled: true }));
const temperatureLimiterMock = vi.hoisted(() => ({ enabled: true }));
const autoWinterMock = vi.hoisted(() => ({ enabled: false }));

// ---------------------------------------------------------------------------
// Mocks — ControlPage pulls in the api helpers. Stub the side-effecting
// fetches so the page mounts past its sub-component loaders (auto-winter,
// cosy, agile, settings tariff, load-limiter). The real `useInverterStore`
// is the subject under test, so we use it as-is and seed snapshot state
// directly.
// ---------------------------------------------------------------------------

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/agile') return { ok: true, enabled: false };
    if (path === '/api/auto-winter')
      return {
        ok: true,
        data: {
          config: {
            enabled: autoWinterMock.enabled,
            cold_threshold: 8,
            recovery_threshold: 12,
            target_soc: 80,
            debounce_readings: 10,
          },
        },
      };
    if (path === '/api/cosy') return { ok: true, enabled: false, slots: [] };
    if (path === '/api/settings')
      return {
        ok: true,
        data: { import_tariff: 0.285, export_tariff: 0.15, import_tariff_config: null },
      };
    if (path === '/api/temperature-limiter')
      return {
        ok: true,
        data: {
          config: {
            enabled: temperatureLimiterMock.enabled,
            high_threshold: 60,
            recovery_threshold: 55,
            confirmation_readings: 3,
          },
        },
      };
    if (path === '/api/load-limiter')
      return {
        ok: true,
        data: {
          config: {
            enabled: loadLimiterMock.enabled,
            threshold_w: 7000,
            trigger_delay_minutes: 5,
            start_hour: 0,
            start_minute: 0,
            end_hour: 0,
            end_minute: 0,
          },
        },
      };
    return { ok: true, data: {} };
  }),
  apiPost: vi.fn(async (path: string) => {
    if (path === '/api/control/unpause') loadLimiterMock.enabled = false;
    return { ok: true, data: {} };
  }),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  fetchHistory: vi.fn().mockResolvedValue({}),
  isTauri: false,
}));

// Imported after the vi.mock() calls above (factories are hoisted regardless).
import ControlPage from '../../src/pages/ControlPage';
import { apiGet, apiPost } from '../../src/lib/api';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

/** Silence noisy React act() warnings from async setState in mount effects. */
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
    battery_reserve: 4,
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
    max_charge_slots: 1,
    max_discharge_slots: 1,
    charge_slots: [],
    discharge_slots: [],
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
    ...overrides,
  };
}

describe('<ControlPage/> — Load Discharge Limiter status (issue #158)', () => {
  beforeEach(() => {
    loadLimiterMock.enabled = true;
    temperatureLimiterMock.enabled = true;
    autoWinterMock.enabled = false;
    vi.mocked(apiGet).mockClear();
    vi.mocked(apiPost).mockClear();
    silenceConsoleError();
    // jsdom doesn't implement matchMedia; stub it defensively.
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
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    cleanup();
    useInverterStore.setState({ snapshot: null, connectionState: 'disconnected' });
  });

  /** Resolve the Load Discharge Limiter wrapper so assertions can be
   *  scoped to it and avoid collisions with the many other "kW" / slider
   *  values on the rest of the page (battery reserve, charge rate, etc.).
   *  The section is a <div>, not a <section>, because it doesn't need
   *  the visual separator styling other sections on the page get. */
  async function loadLimiterSection() {
    const heading = await screen.findByRole('heading', {
      name: 'Load Discharge Limiter',
      exact: true,
    });
    const section = heading.closest<HTMLElement>('div.space-y-3');
    if (!section) throw new Error('Load Discharge Limiter heading has no <div.space-y-3> ancestor');
    return section;
  }

  it('hides disabled feature settings and reveals them when toggled on', async () => {
    loadLimiterMock.enabled = false;
    temperatureLimiterMock.enabled = false;
    autoWinterMock.enabled = false;
    useInverterStore.setState({
      snapshot: makeSnapshot({ battery_mode: 'timed_demand', auto_winter_active: true }),
      developerMode: false,
      connectionState: 'connected',
    });
    render(<ControlPage />);

    const loadSection = await loadLimiterSection();
    const temperatureHeading = await screen.findByRole('heading', {
      name: 'Inverter Temperature Limiter',
    });
    const temperatureSection = temperatureHeading.closest<HTMLElement>('div.space-y-3');
    const winterHeading = await screen.findByRole('heading', { name: 'Auto Winter Mode' });
    const winterSection = winterHeading.closest<HTMLElement>('section');
    if (!temperatureSection || !winterSection) throw new Error('feature section missing');

    expect(within(loadSection).queryByText('Load Threshold')).toBeNull();
    expect(within(loadSection).queryByText(/Load limiter only operates in/)).toBeNull();
    expect(within(loadSection).getByRole('button', { name: 'Save' })).toBeDefined();
    expect(within(temperatureSection).queryByText('Pause Threshold')).toBeNull();
    expect(within(temperatureSection).getByRole('button', { name: 'Save' })).toBeDefined();
    expect(within(winterSection).queryByText('Cold Threshold')).toBeNull();
    expect(within(winterSection).getByRole('button', { name: 'Save' })).toBeDefined();
    expect(within(winterSection).queryByText(/implemented locally within this app/i)).toBeNull();
    expect(within(winterSection).queryByText(/Winter mode active/)).toBeNull();

    fireEvent.click(within(loadSection).getAllByRole('button')[0]);
    fireEvent.click(within(temperatureSection).getAllByRole('button')[0]);
    fireEvent.click(within(winterSection).getAllByRole('button')[0]);

    expect(within(loadSection).getByText('Load Threshold')).toBeDefined();
    expect(within(loadSection).getByText(/Load limiter only operates in/)).toBeDefined();
    expect(within(loadSection).getByRole('button', { name: 'Save' })).toBeDefined();
    expect(within(temperatureSection).getByText('Pause Threshold')).toBeDefined();
    expect(within(temperatureSection).getByRole('button', { name: 'Save' })).toBeDefined();
    expect(within(winterSection).getByText('Cold Threshold')).toBeDefined();
    expect(within(winterSection).getByRole('button', { name: 'Save' })).toBeDefined();
    expect(within(winterSection).getByText(/Winter mode active/)).toBeDefined();
  });

  it('shows active inverter-temperature protection and its all-mode warning', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({
        battery_mode: 'eco_paused',
        battery_reserve: 100,
        inverter_temperature: 62,
        temperature_limiter_active: true,
      }),
      developerMode: false,
      connectionState: 'connected',
    });
    render(<ControlPage />);

    const heading = await screen.findByRole('heading', {
      name: 'Inverter Temperature Limiter',
    });
    const section = heading.closest('div.space-y-3');
    if (!section) throw new Error('temperature limiter section missing');
    expect(within(section).getByText(/Paused · 62.0°C/)).toBeDefined();
    expect(within(section).getByText(/Thermal protection overrides every discharge mode/)).toBeDefined();
    expect(screen.getByRole('button', { name: /Disable Limiter & Resume Discharge/i })).toBeDefined();
  });

  it('saves inverter-temperature limiter thresholds and confirmations', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot(),
      developerMode: false,
      connectionState: 'connected',
    });
    render(<ControlPage />);
    const heading = await screen.findByRole('heading', {
      name: 'Inverter Temperature Limiter',
    });
    const section = heading.closest('div.space-y-3');
    if (!section) throw new Error('temperature limiter section missing');

    fireEvent.click(within(section).getByRole('button', { name: 'Save' }));
    await waitFor(() => {
      expect(apiPost).toHaveBeenCalledWith('/api/temperature-limiter', {
        enabled: true,
        high_threshold: 60,
        recovery_threshold: 55,
        confirmation_readings: 3,
      });
    });
  });

  it('makes the recovery condition explicit in the delay control', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot(),
      developerMode: false,
      connectionState: 'connected',
    });
    render(<ControlPage />);

    const section = await loadLimiterSection();
    expect(within(section).getByText('Pause / Recovery Delay')).toBeDefined();
    expect(within(section).getByText(
      /Pauses discharge after load stays above the threshold; resumes discharge after it stays at or below the threshold/,
    )).toBeDefined();
  });

  it('disables the limiter when its Quick Action resumes battery discharge', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({
        battery_mode: 'eco_paused',
        battery_reserve: 100,
        load_limiter_active: true,
        home_power: 10000,
      }),
      developerMode: false,
      connectionState: 'connected',
    });
    render(<ControlPage />);
    await loadLimiterSection();
    vi.mocked(apiGet).mockClear();

    fireEvent.click(await screen.findByRole('button', { name: /Disable Limiter & Resume Discharge/i }));

    await waitFor(() => {
      expect(apiPost).toHaveBeenCalledWith('/api/control/unpause', undefined);
    });
    await waitFor(() => {
      expect(apiGet).toHaveBeenCalledWith('/api/load-limiter');
    });
    expect(loadLimiterMock.enabled).toBe(false);
  });

  it('shows "Paused" when the limiter is active in eco_paused mode with load still high', async () => {
    // The exact issue #158 scenario: home power 10 kW, threshold 7 kW,
    // battery mode has flipped to eco_paused (HR 110 = 100% written by the
    // limiter), and load_limiter_active is true (state machine is in
    // Paused / PausedFromRestart).
    useInverterStore.setState({
      snapshot: makeSnapshot({
        battery_mode: 'eco_paused',
        battery_reserve: 100,
        load_limiter_active: true,
        home_power: 10000,
      }),
      developerMode: false,
      connectionState: 'connected',
    });
    render(<ControlPage />);

    const section = await loadLimiterSection();

    // THE BUG: with the old isEco gate, this would render "Idle" because
    // battery_mode was eco_paused, not eco. The fix treats eco_paused as a
    // valid operating mode so the state shows the real limiter state.
    expect(within(section).getByText('Paused')).toBeDefined();

    // The warning banner must NOT appear — the limiter is the thing that
    // put it in eco_paused, so "load limiter only operates in Eco (current:
    // eco paused)" is misleading while the limiter is actively holding.
    expect(within(section).queryByText(/Load limiter only operates in/)).toBeNull();
  });

  it('shows "Recovering…" when the limiter is active in eco_paused mode and load has dropped', async () => {
    // LowLoadPending: home_power has been below threshold for the debounce
    // count, the limiter is restoring Eco. battery_mode is still eco_paused
    // until the restore write succeeds; load_limiter_active stays true
    // through the recovery countdown.
    useInverterStore.setState({
      snapshot: makeSnapshot({
        battery_mode: 'eco_paused',
        battery_reserve: 100,
        load_limiter_active: true,
        home_power: 5000, // below 7 kW threshold
      }),
      developerMode: false,
      connectionState: 'connected',
    });
    render(<ControlPage />);

    const section = await loadLimiterSection();
    expect(within(section).getByText('Recovering…')).toBeDefined();
    expect(within(section).queryByText(/Load limiter only operates in/)).toBeNull();
  });

  it('shows "Monitoring…" in eco_paused mode when load_limiter_active is briefly false (regression)', async () => {
    // Defensive: if the snapshot briefly shows eco_paused with
    // load_limiter_active = false (e.g. mid-transition between snapshot
    // reads), the GUI should fall back to "Monitoring…" if the load is
    // still above threshold — the limiter is enabled, the mode is valid,
    // and the next poll will start the HighLoadPending countdown. The
    // critical assertion is the warning banner: it must NOT appear
    // because eco_paused is a valid operating mode for the limiter.
    useInverterStore.setState({
      snapshot: makeSnapshot({
        battery_mode: 'eco_paused',
        battery_reserve: 100,
        load_limiter_active: false,
        home_power: 10000,
      }),
      developerMode: false,
      connectionState: 'connected',
    });
    render(<ControlPage />);

    const section = await loadLimiterSection();
    expect(within(section).getByText('Monitoring…')).toBeDefined();
    // No banner — eco_paused is a valid limiter mode.
    expect(within(section).queryByText(/Load limiter only operates in/)).toBeNull();
  });

  it('shows "Idle" in eco_paused mode when load_limiter_active is false and load is below threshold', async () => {
    // eco_paused + load_limiter_active=false + load below threshold =
    // nothing to do. The state should be Idle, no warning banner (because
    // eco_paused is a valid operating mode).
    useInverterStore.setState({
      snapshot: makeSnapshot({
        battery_mode: 'eco_paused',
        battery_reserve: 100,
        load_limiter_active: false,
        home_power: 2000,
      }),
      developerMode: false,
      connectionState: 'connected',
    });
    render(<ControlPage />);

    const section = await loadLimiterSection();
    expect(within(section).getByText('Idle')).toBeDefined();
    expect(within(section).queryByText(/Load limiter only operates in/)).toBeNull();
  });

  it('shows "Monitoring…" in eco mode while load is above threshold and limiter is not yet active', async () => {
    // Pre-trigger: load is high but the debounce countdown hasn't elapsed
    // yet, so load_limiter_active is false. State should read "Monitoring…"
    // so the user knows the limiter is watching.
    useInverterStore.setState({
      snapshot: makeSnapshot({
        battery_mode: 'eco',
        battery_reserve: 4,
        load_limiter_active: false,
        home_power: 10000,
      }),
      developerMode: false,
      connectionState: 'connected',
    });
    render(<ControlPage />);

    const section = await loadLimiterSection();
    expect(within(section).getByText('Monitoring…')).toBeDefined();
    expect(within(section).queryByText(/Load limiter only operates in/)).toBeNull();
  });

  it('shows the "operates in Eco mode" warning when battery is in a Timed mode', async () => {
    // The banner is only for modes the limiter cannot run in. Timed Demand
    // is a hard block: the limiter will yield to other automation.
    useInverterStore.setState({
      snapshot: makeSnapshot({
        battery_mode: 'timed_demand',
        battery_reserve: 4,
        load_limiter_active: false,
        home_power: 0,
      }),
      developerMode: false,
      connectionState: 'connected',
    });
    render(<ControlPage />);

    const section = await loadLimiterSection();
    // State stays Idle because the limiter won't run in Timed.
    expect(within(section).getByText('Idle')).toBeDefined();
    // The warning banner must be present.
    expect(within(section).getByText(/Load limiter only operates in/)).toBeDefined();
    expect(within(section).getByText(/timed demand/)).toBeDefined();
  });

  it('shows "Idle" in eco mode when home power is below threshold', async () => {
    // Normal idle: eco mode, low load. No monitoring, no pausing, no banner.
    useInverterStore.setState({
      snapshot: makeSnapshot({
        battery_mode: 'eco',
        battery_reserve: 4,
        load_limiter_active: false,
        home_power: 2000,
      }),
      developerMode: false,
      connectionState: 'connected',
    });
    render(<ControlPage />);

    const section = await loadLimiterSection();
    expect(within(section).getByText('Idle')).toBeDefined();
    expect(within(section).queryByText(/Load limiter only operates in/)).toBeNull();
  });

  it('keeps showing "Idle" (and a warning) when the battery is in an unknown mode', async () => {
    // Edge case: snapshot arrives with battery_mode='unknown' (e.g. the
    // very first poll before the inverter has been read). The limiter
    // can't run, so the banner should show.
    useInverterStore.setState({
      snapshot: makeSnapshot({
        battery_mode: 'unknown',
        battery_reserve: 0,
        load_limiter_active: false,
        home_power: 0,
      }),
      developerMode: false,
      connectionState: 'connected',
    });
    render(<ControlPage />);

    const section = await loadLimiterSection();
    expect(within(section).getByText('Idle')).toBeDefined();
    expect(within(section).getByText(/Load limiter only operates in/)).toBeDefined();
  });
});
