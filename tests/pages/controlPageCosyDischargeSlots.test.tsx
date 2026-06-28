/**
 * Tests for Cosy mode visibility of the Timed Discharge and Discharge
 * Schedule (Timed Export slot) sections on ControlPage.
 *
 * Background: Cosy mode was reworked from a "replace the battery mode"
 * preset to an independent force-charge mechanism that owns only the
 * `enable_charge` side of the inverter. The pause-discharge window
 * (Timed Discharge) and the export schedule (Discharge Schedule, the
 * schedule that drives Timed Export mode via `enable_discharge`) are
 * independent mechanisms — they don't conflict with Cosy, so the user
 * can layer them (e.g. "Cosy force-charges 02:00–05:00, and Timed
 * Discharge blocks discharge outside 16:00–19:00").
 *
 * Before this change the two discharge-schedule sections were hidden
 * whenever Cosy was enabled, forcing users to flip back to Standard
 * just to configure a discharge window. Now they stay visible in Cosy
 * (the same as Standard), and remain hidden in Agile, where price-based
 * logic owns both directions.
 *
 * The Agile exclusion is a regression guard — we must not have lifted
 * the visibility for Cosy and accidentally also lifted it for Agile.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    // Default mock — sub-component loaders need realistic shapes so the
    // page mounts past them. Tests can override `/api/cosy` per-case via
    // vi.mocked(apiGet).mockImplementationOnce(...) to flip the
    // effective charging mode (the snapshot's cosy_enabled flag is the
    // bootstrap, but the CosyChargingSection useEffect re-asserts the
    // mode from `/api/cosy` on mount and fires onModeChange if they
    // disagree).
    if (path === '/api/agile') return { ok: true, enabled: false };
    if (path === '/api/auto-winter')
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
    if (path === '/api/cosy')
      return {
        ok: true,
        enabled: false,
        slots: Array.from({ length: 3 }, () => ({
          enabled: false,
          start_hour: 0,
          start_minute: 0,
          end_hour: 0,
          end_minute: 0,
          target_soc: 100,
        })),
      };
    if (path === '/api/settings')
      return {
        ok: true,
        data: { import_tariff: 0.285, export_tariff: 0.15, import_tariff_config: null },
      };
    if (path === '/api/load-limiter')
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
import { apiGet } from '../../src/lib/api';
import type { InverterSnapshot, ScheduleSlot } from '../../src/lib/types';

/** Silence noisy React act() warnings from async setState in mount effects. */
function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

function emptySlot(overrides: Partial<ScheduleSlot> = {}): ScheduleSlot {
  return {
    enabled: false,
    start_hour: 0,
    start_minute: 0,
    end_hour: 0,
    end_minute: 0,
    target_soc: 100,
    ...overrides,
  };
}

/**
 * Build a complete InverterSnapshot for a Gen3 Hybrid (device type code
 * '2001'). The two discharge-schedule sections under test don't depend on
 * any register state — they just render the slot list from
 * `discharge_slots` (Discharge Schedule) or `battery_pause_slot`
 * (Timed Discharge) — so we only need realistic defaults.
 */
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
    max_charge_slots: 2,
    max_discharge_slots: 2,
    charge_slots: [emptySlot(), emptySlot()],
    discharge_slots: [emptySlot(), emptySlot()],
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
    battery_pause_slot: emptySlot(),
    ...overrides,
  };
}

describe('<ControlPage/> — Cosy mode discharge schedule visibility', () => {
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
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    cleanup();
    useInverterStore.setState({ snapshot: null });
  });

  async function timedDischargeSection() {
    const heading = await screen.findByRole('heading', {
      name: 'Timed Discharge',
      exact: true,
    });
    const section = heading.closest('section');
    if (!section) throw new Error('Timed Discharge heading has no <section> ancestor');
    return section;
  }

  async function dischargeScheduleSection() {
    const heading = await screen.findByRole('heading', {
      name: 'Discharge Schedule',
      exact: true,
    });
    const section = heading.closest('section');
    if (!section) throw new Error('Discharge Schedule heading has no <section> ancestor');
    return section;
  }

  it('renders Timed Discharge + Discharge Schedule in Cosy mode', async () => {
    // The snapshot's cosy_enabled flag drives the initial mode; the
    // CosyChargingSection useEffect then re-asserts the mode from
    // /api/cosy on mount and flips the dropdown if they disagree.
    // Both ends need to agree on 'cosy' for the page to actually be in
    // Cosy mode for the duration of this test, so we override the
    // /api/cosy response once to return enabled=true.
    vi.mocked(apiGet).mockImplementationOnce(async (path: string) => {
      if (path === '/api/cosy')
        return {
          ok: true,
          enabled: true,
          slots: Array.from({ length: 3 }, () => ({
            enabled: false,
            start_hour: 0,
            start_minute: 0,
            end_hour: 0,
            end_minute: 0,
            target_soc: 100,
          })),
        };
      // Fall through to the factory default for everything else.
      throw new Error(`unexpected apiGet path in cosy test: ${path}`);
    });
    useInverterStore.setState({
      snapshot: makeSnapshot({ cosy_enabled: true }),
      developerMode: false,
    });
    render(<ControlPage />);

    // Sanity-check we're really in Cosy — otherwise the test could pass
    // for the wrong reason (e.g. if a future change stopped honouring
    // snapshot.cosy_enabled).
    const select = await screen.findByRole('combobox');
    expect((select as HTMLSelectElement).value).toBe('cosy');

    // Both sections must be present. The Timed Discharge heading is
    // unique to that section; the Discharge Schedule heading is the
    // section that hosts the Timed Export slot editors. Assert on the
    // headings rather than the slot labels so we don't collide with the
    // "Slot 1" / "Slot 2" mentions in the slot-ordering warning callout.
    await timedDischargeSection();
    await dischargeScheduleSection();
  });

  it('still renders Timed Discharge + Discharge Schedule in Standard mode (regression guard)', async () => {
    // Pre-Cosy behaviour already exposed both — this test pins that
    // contract so a future change can't quietly drop Standard mode.
    useInverterStore.setState({
      snapshot: makeSnapshot({ cosy_enabled: false }),
      developerMode: false,
    });
    render(<ControlPage />);

    const select = await screen.findByRole('combobox');
    expect((select as HTMLSelectElement).value).toBe('standard');

    await timedDischargeSection();
    await dischargeScheduleSection();
  });

  it('still hides Timed Discharge + Discharge Schedule in Agile mode (regression guard)', async () => {
    // Agile drives both charge and discharge from live prices, so manual
    // schedule editors must stay hidden. This guards against an
    // over-eager refactor that drops the agile exclusion at the same
    // time as the cosy one.
    //
    // The snapshot's `agile_enabled` flag only controls what the backend
    // thinks is enabled — the front-end `chargeMode` state lives in a
    // local override that we set by changing the Charging Mode dropdown.
    // So we render with Standard and then flip the dropdown to Agile.
    useInverterStore.setState({
      snapshot: makeSnapshot({ cosy_enabled: false }),
      developerMode: false,
    });
    render(<ControlPage />);

    const select = (await screen.findByRole('combobox')) as HTMLSelectElement;
    fireEvent.change(select, { target: { value: 'agile' } });

    // Confirm the dropdown actually flipped — otherwise this is a
    // silent pass.
    expect(select.value).toBe('agile');

    expect(screen.queryByRole('heading', { name: 'Timed Discharge', exact: true })).toBeNull();
    expect(screen.queryByRole('heading', { name: 'Discharge Schedule', exact: true })).toBeNull();
  });
});