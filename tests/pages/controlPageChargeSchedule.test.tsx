/**
 * Tests for the Charge Schedule slot rendering on ControlPage.
 *
 * Covers issue #135: when charge slot 1 holds leftover/factory times in
 * HR 94-95 (e.g. 02:00-05:00) but the master enable_charge flag (HR 96)
 * is OFF, the slot was painted as an armed charge (toggle ON, populated
 * window, Target SOC) even though the inverter will never charge. The
 * fix keeps the slot configuration visible (so issue #41 does not
 * regress — a genuinely configured slot must not vanish when firmware
 * transiently clears HR 96) but visually distinguishes "armed" from
 * "configured but not active".
 *
 * The decoder contract that makes this possible is pinned in
 * `src-tauri/src/inverter/decoder.rs`:
 *   - `charge_slot1_from_hr94_95_is_enabled_when_hr96_is_off`
 *   - `charge_slot_enabled_not_gated_on_enable_charge_flag`
 * `slot.enabled` reflects *configured times* and is deliberately NOT
 * gated on `enable_charge`. These frontend tests assert the UI honours
 * both halves of that contract: the slot stays visible, and its
 * armed/not-active state is derived from `enable_charge`.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, within } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Mocks — ControlPage pulls in the api helpers and useAction (which itself
// only depends on the mocked apiPost). Stub the side-effecting fetches so the
// page mounts past its sub-component loaders (auto-winter, cosy, agile,
// settings tariff, load-limiter). The real `useInverterStore` is the subject
// under test, so we use it as-is and seed snapshot state directly.
// ---------------------------------------------------------------------------

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    // Return the minimum shape each sub-component's mount effect needs so
    // nothing throws on destructuring. Unknown paths fall through to a
    // generic ok envelope (the sub-components wrap their fetches in
    // try/catch and fall back to defaults regardless).
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

// Imported after the vi.mock() calls above (factories are hoisted regardless).
import ControlPage from '../../src/pages/ControlPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot, ScheduleSlot } from '../../src/lib/types';

/** Silence noisy React act() warnings from async setState in mount effects. */
function silenceConsoleError() {
  return vi.spyOn(console, 'error').mockImplementation(() => {});
}

/**
 * Build a complete InverterSnapshot for a Gen3 Hybrid (device type code
 * '2001', matching the issue #135 reporter's hardware), with the exact
 * register state that triggers the phantom-slot bug:
 *   - charge slot 1 configured 02:00-05:00 with Target SOC 100 (HR 242)
 *   - global target SOC 4 (HR 116)
 *   - enable_charge OFF (HR 96 = 0) — the slot is inert
 */
function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
  const configuredChargeSlot: ScheduleSlot = {
    enabled: true,
    start_hour: 2,
    start_minute: 0,
    end_hour: 5,
    end_minute: 0,
    target_soc: 100,
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
    charge_slots: [configuredChargeSlot],
    discharge_slots: [emptySlot],
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

describe('<ControlPage/> — Charge Schedule armed vs not-active (issue #135)', () => {
  beforeEach(() => {
    silenceConsoleError();
    // jsdom doesn't implement matchMedia; stub it defensively even though
    // ControlPage itself doesn't use it (some transitive path might).
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

  /** Resolve the Charge Schedule <section> so assertions can be scoped to
   *  it and avoid collisions with the many other "%" / slider values on the
   *  rest of the page (battery reserve, charge rate, etc.). */
  async function chargeScheduleSection() {
    const heading = await screen.findByRole('heading', {
      name: 'Charge Schedule',
      exact: true,
    });
    const section = heading.closest('section');
    if (!section) throw new Error('Charge Schedule heading has no <section> ancestor');
    return section;
  }

  it('shows a configured charge slot as dimmed when enable_charge is off (issue #135)', async () => {
    // The exact register state the reporter is on: HR 94/95 = 02:00/05:00,
    // HR 96 = 0 (enable_charge OFF), HR 116 = 4, HR 242 = 100. The slot is
    // configured but the inverter will not charge — the UI must dim it
    // (opacity-60) so the user can see it's inert.
    useInverterStore.setState({ snapshot: makeSnapshot(), developerMode: false });
    render(<ControlPage />);

    const section = await chargeScheduleSection();

    // The slot card is rendered (Slot 1 label present) — the slot is shown,
    // not hidden. The Target SOC value ("100%") only renders when the slot
    // is enabled, so its presence proves the configured slot is visible.
    expect(within(section).getByText('Slot 1')).toBeDefined();
    expect(within(section).getByText('100%')).toBeDefined();

    // THE BUG: with enable_charge OFF, the slot card must carry the dimmed
    // class — the old behaviour painted it as an armed charge.
    const slotCard = within(section).getByText('Slot 1').closest('div.bg-bg-surface');
    expect(slotCard).toBeDefined();
    expect(slotCard!.className).toMatch(/opacity-60/);
  });

  it('does NOT hide a configured charge slot when enable_charge is off (issue #41 regression)', async () => {
    // Regression guard for issue #41: a genuinely configured slot must keep
    // rendering its times after an ECO<->Timed transition that transiently
    // clears HR 96. The dimmed-when-not-armed style must not collapse the
    // slot card entirely.
    useInverterStore.setState({
      snapshot: makeSnapshot({
        charge_slots: [
          {
            enabled: true,
            start_hour: 9,
            start_minute: 0,
            end_hour: 11,
            end_minute: 0,
            target_soc: 80,
          },
        ],
      }),
      developerMode: false,
    });
    render(<ControlPage />);

    const section = await chargeScheduleSection();

    // The slot's Target SOC ("80%") only renders inside the editor when
    // the slot is enabled. If a future change re-gated slot visibility (or
    // slot.enabled) on enable_charge, this would disappear exactly as it
    // did in issue #41 — the slot would vanish from the UI.
    expect(within(section).getByText('80%')).toBeDefined();
  });
});
