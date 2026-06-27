/**
 * Tests for surfacing GivEnergy's "Timed Export" as its own selectable
 * battery mode on ControlPage — the frontend half of issue #156.
 *
 * The backend half is already done and pinned in the Rust tests:
 *   - `BatteryMode::TimedExport` (model.rs) decodes `eco_mode=0,
 *      enable_discharge=true` as a distinct mode.
 *   - `ControlCommand::SetTimedExportMode` writes HR(27)=0 (max power →
 *      export surplus) while `SetTimedDemandMode` writes HR(27)=1 (match
 *      demand). The sole register separating "discharge to home" from
 *      "export to grid" is HR(27) — see the `#156` invariant test in
 *      encoder.rs and the routing tests in api.rs.
 *
 * What was left: the React UI collapsed both timed modes into a single
 * "Timed Discharge" control. `TIMED_MODES` only listed `timed_demand`, the
 * `displayMode` line remapped an inverter-reported `timed_export` to
 * `timed_demand` for rendering, and the "Timed" toggle hard-coded
 * `timed_demand`. So a user whose inverter was actually exporting to grid
 * (HR27=0) saw "Timed Discharge", and had no way to choose export.
 *
 * These tests assert the two halves of the fix and are deliberately written
 * against observable behaviour (a distinct "Timed Export" control that posts
 * `mode: "timed_export"`) rather than against layout, so they stay valid
 * whether export is rendered as a sub-mode button, a category, or anything
 * else that exposes the label and the action.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, within } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Mocks — same shape as controlPageChargeSchedule.test.tsx. Stub the
// side-effecting fetches so the page's sub-component mount effects (auto
// winter, cosy, agile, settings tariff, load-limiter) don't throw. The real
// `useInverterStore` is the subject under test, seeded directly.
// ---------------------------------------------------------------------------

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
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
    if (path === '/api/cosy') return { ok: true, enabled: false, slots: [] };
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
import { apiPost } from '../../src/lib/api';
import type { InverterSnapshot, ScheduleSlot } from '../../src/lib/types';

/** Silence noisy React act() warnings from async setState in mount effects. */
function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

/**
 * Build a complete InverterSnapshot for a Gen3 Hybrid (device type '2001').
 * Defaults encode the issue #156 reporter's exact inverter state: a
 * configured Timed Export slot (17:00-19:00, target 20%), `enable_discharge`
 * ON (HR59=1), and `battery_mode: 'timed_export'` (the mode derived from
 * HR27=0 / max-power export).
 */
function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
  const exportSlot: ScheduleSlot = {
    enabled: true,
    start_hour: 17,
    start_minute: 0,
    end_hour: 19,
    end_minute: 0,
    target_soc: 20,
  };
  const emptySlot: ScheduleSlot = {
    enabled: false,
    start_hour: 0,
    start_minute: 0,
    end_hour: 6,
    end_minute: 0,
    target_soc: 100,
  };
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
    // The reporter's state: inverter is in Timed Export (HR27=0, HR59=1).
    battery_mode: 'timed_export',
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
    enable_discharge: true,
    auto_winter_active: false,
    load_limiter_active: false,
    cosy_active: false,
    cosy_enabled: false,
    agile_active: false,
    agile_state: 'idle',
    agile_enabled: false,
    max_charge_slots: 2,
    max_discharge_slots: 2,
    charge_slots: [emptySlot, emptySlot],
    discharge_slots: [exportSlot, emptySlot],
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

describe('<ControlPage/> — Timed Export as its own mode (issue #156)', () => {
  beforeEach(() => {
    silenceConsoleError();
    vi.mocked(apiPost).mockClear();
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

  /** Resolve the "Battery Mode" <section> so assertions are scoped and don't
   *  collide with unrelated text elsewhere on the page. */
  async function batteryModeSection() {
    const heading = await screen.findByRole('heading', {
      name: 'Battery Mode',
      exact: true,
    });
    const section = heading.closest('section');
    if (!section) throw new Error('Battery Mode heading has no <section> ancestor');
    return section;
  }

  it('shows a distinct Timed Export control when the inverter reports timed_export (no mislabelling)', async () => {
    // Reporter's exact state: HR27=0 / HR59=1 → battery_mode 'timed_export'.
    // The old UI remapped this to 'timed_demand' and showed "Timed
    // Discharge", contradicting GivEnergy Cloud. There must now be a control
    // labelled "Timed Export" that is the active selection.
    useInverterStore.setState({ snapshot: makeSnapshot(), developerMode: false });
    render(<ControlPage />);

    const section = await batteryModeSection();

    // A "Timed Export" control exists (previously absent — the only label
    // shown was "Timed Discharge").
    expect(within(section).getByText('Timed Export')).toBeDefined();

    // And it is the active mode (carries the active styling), not Timed
    // Discharge. This guards against a relabelling that still highlights the
    // wrong button.
    const exportButton = within(section).getByText('Timed Export').closest('button');
    expect(exportButton).toBeDefined();
    expect(exportButton!.className).toMatch(/bg-battery\/20.*border-battery|border-battery.*bg-battery\/20/);
  });

  it('lets the user select Timed Export, posting mode=timed_export to the mode endpoint', async () => {
    // Start from Timed Demand so the timed sub-modes are already visible;
    // the user then switches to Export. The backend already routes
    // `set_mode("timed_export")` → SetTimedExportMode (HR27=0), but the old
    // UI never sent that mode. Selecting the control must POST it.
    useInverterStore.setState({
      snapshot: makeSnapshot({ battery_mode: 'timed_demand' }),
      developerMode: false,
    });
    render(<ControlPage />);

    const section = await batteryModeSection();
    const exportButton = within(section).getByText('Timed Export').closest('button');
    fireEvent.click(exportButton!);

    // The first POST after a click is the mode switch. Body must carry the
    // exact mode string the backend routes for export.
    const modeCall = vi
      .mocked(apiPost)
      .mock.calls.find(([path]) => path === '/api/control/mode');
    expect(modeCall, 'expected a POST to /api/control/mode').toBeDefined();
    expect(modeCall![1]).toMatchObject({ mode: 'timed_export' });
  });

  it('keeps Timed Discharge and Timed Export as two distinct controls', async () => {
    // Guard against a refactor that re-collapses the two modes into one
    // control (the original #156 regression). Both labels must render, and
    // selecting each posts a different mode — the register that separates
    // them (HR27) is what makes export different from demand.
    useInverterStore.setState({
      snapshot: makeSnapshot({ battery_mode: 'timed_demand' }),
      developerMode: false,
    });
    render(<ControlPage />);

    const section = await batteryModeSection();
    expect(within(section).getByText('Timed Discharge')).toBeDefined();
    expect(within(section).getByText('Timed Export')).toBeDefined();

    // Selecting Demand posts demand; selecting Export posts export. Same
    // schedule registers, different HR27 — the modes must not be aliases.
    vi.mocked(apiPost).mockClear();
    fireEvent.click(within(section).getByText('Timed Discharge').closest('button')!);
    const demandMode = vi
      .mocked(apiPost)
      .mock.calls.find(([path]) => path === '/api/control/mode');
    expect(demandMode![1]).toMatchObject({ mode: 'timed_demand' });

    vi.mocked(apiPost).mockClear();
    fireEvent.click(within(section).getByText('Timed Export').closest('button')!);
    const exportMode = vi
      .mocked(apiPost)
      .mock.calls.find(([path]) => path === '/api/control/mode');
    expect(exportMode![1]).toMatchObject({ mode: 'timed_export' });
  });
});
