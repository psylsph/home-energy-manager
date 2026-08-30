/**
 * Save-and-enable parity between the two discharge mechanisms on
 * ControlPage: the Timed Export schedule (discharge slots via
 * `/api/control/discharge-slot`) and the Timed Discharge window (pause
 * registers via `/api/control/timed-discharge`).
 *
 * History (issue #289): the two editors behaved inconsistently for the
 * whole life of the feature — saving an enabled Timed Discharge window
 * armed the mechanism immediately, while saving an enabled Timed Export
 * slot only stored the times and silently left the mode off until the
 * user separately pressed the Timed Export button. Users set an export
 * window in advance, saw nothing happen, and lost trust in the app.
 *
 * These tests pin the consistent contract so a regression of either
 * side — or a new asymmetry — fails loudly:
 *   1. Saving an ENABLED slot of either type POSTs `enabled: true` to
 *      the correct endpoint with the editor's window.
 *   2. Saving a DISABLED slot POSTs `enabled: false` and arms nothing.
 *   3. Both sections carry matching save-and-enable wording, so the
 *      behaviour is communicated, not just implemented.
 *   4. After a save, the corresponding Battery Mode indicator lights up
 *      from the confirming snapshot — the save visibly activates the
 *      mode, which is the whole point of the fix.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, act, within, waitFor } from '@testing-library/react';

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
import { apiPost } from '../../src/lib/api';
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
 * All-in-One (8001) fixture: supports BOTH mechanisms — the Timed
 * Export schedule (HR 56-57) and the Timed Discharge pause window
 * (HR 318-320) — so one render exercises both editors side by side.
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
    // Disabled-but-populated windows (the shape the decoder reports for
    // previously configured slots) so the editors default to concrete
    // values — the ones the save-parity assertions pin.
    discharge_slots: [
      emptySlot({ start_hour: 16, end_hour: 19, target_soc: 4 }),
      emptySlot({ start_hour: 20, end_hour: 22, target_soc: 10 }),
    ],
    meters: [],
    inverter_serial: 'BAT000131',
    firmware_version: '318',
    dsp_firmware_version: '318',
    dc_dsp_firmware_version: '',
    device_type: 'AllInOne6kW',
    device_type_display: 'All-in-One 6kW',
    device_type_code: '8001',
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
  } as InverterSnapshot;
}

describe('<ControlPage/> — Timed Export / Timed Discharge save parity', () => {
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
    useInverterStore.setState({
      snapshot: makeSnapshot(),
      developerMode: false,
      connectionState: 'connected',
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    cleanup();
    useInverterStore.setState({
      snapshot: null,
      connectionState: 'disconnected',
      // Battery-mode action state is store-owned (survives page navigation)
      // — reset it so one test's in-flight arm can't leak into the next.
      batteryModePending: null,
      batteryModePendingSince: null,
      batteryModeError: null,
      timedExportArmFailed: false,
    });
  });

  async function sectionByHeading(name: string): Promise<HTMLElement> {
    const heading = await screen.findByRole('heading', { name, exact: true });
    const section = heading.closest('section');
    if (!section) throw new Error(`${name} heading has no <section> ancestor`);
    return section;
  }

  /** Toggle the slot editor at `slotLabel` ("Slot 1" / "Slot 2") on, then Save. */
  async function enableAndSave(
    section: HTMLElement,
    slotLabel: string,
    saveIndex = 0,
  ): Promise<void> {
    fireEvent.click(within(section).getByLabelText(`${slotLabel} disabled`));
    const saveButtons = within(section).getAllByRole('button', { name: 'Save' });
    fireEvent.click(saveButtons[saveIndex]);
  }

  it('saving an enabled Timed Export slot POSTs enabled: true to the discharge-slot endpoint', async () => {
    render(<ControlPage />);

    const section = await sectionByHeading('Timed Export');
    await enableAndSave(section, 'Slot 1');

    expect(vi.mocked(apiPost)).toHaveBeenCalledWith('/api/control/discharge-slot', {
      slot: 1,
      enabled: true,
      start_hour: 16,
      start_minute: 0,
      end_hour: 19,
      end_minute: 0,
      target_soc: 4,
    });
  });

  it('shows a prominent progress indicator for the full duration of a slot save', async () => {
    let finishSave: ((value: { ok: boolean; data: object }) => void) | undefined;
    vi.mocked(apiPost).mockImplementationOnce(
      () => new Promise((resolve) => { finishSave = resolve; }),
    );
    render(<ControlPage />);

    const section = await sectionByHeading('Timed Export');
    fireEvent.click(within(section).getAllByRole('button', { name: 'Save' })[0]);

    const progress = screen.getByText('Applying changes to inverter…').closest('[role="status"]');
    expect(progress).not.toBeNull();
    expect(progress).toHaveTextContent('Applying changes to inverter');
    expect(progress).toHaveClass('fixed', 'z-[100]');
    expect(section.contains(progress)).toBe(false);
    expect(within(section).getByRole('button', { name: 'Applying…' })).toBeDisabled();

    await act(async () => {
      finishSave?.({ ok: true, data: {} });
    });
    await waitFor(() => {
      expect(within(section).queryByText(/Applying changes to inverter/)).toBeNull();
    });
  });

  it('saving an enabled Timed Discharge window POSTs enabled: true to the timed-discharge endpoint', async () => {
    render(<ControlPage />);

    const section = await sectionByHeading('Timed Discharge');
    await enableAndSave(section, 'Slot 1');

    expect(vi.mocked(apiPost)).toHaveBeenCalledWith('/api/control/timed-discharge', {
      enabled: true,
      start_hour: 3,
      start_minute: 0,
      end_hour: 4,
      end_minute: 0,
    });
  });

  it('keeps progress visible until all Timed Discharge registers confirm', async () => {
    vi.mocked(apiPost).mockResolvedValueOnce({ ok: true, data: {} });
    render(<ControlPage />);

    const section = await sectionByHeading('Timed Discharge');
    await enableAndSave(section, 'Slot 1');
    await waitFor(() => expect(apiPost).toHaveBeenCalledWith(
      '/api/control/timed-discharge',
      { enabled: true, start_hour: 3, start_minute: 0, end_hour: 4, end_minute: 0 },
    ));
    expect(screen.getByText('Applying changes to inverter…')).toBeDefined();

    // HR318 can read back before both inverse-window registers. Do not clear
    // progress on this partial state.
    act(() => {
      useInverterStore.setState({
        snapshot: makeSnapshot({
          battery_pause_mode: 2,
          battery_pause_slot: emptySlot({
            enabled: true,
            start_hour: 4,
            start_minute: 0,
            end_hour: 2,
            end_minute: 59,
          }),
        }),
      });
    });
    expect(screen.getByText('Applying changes to inverter…')).toBeDefined();

    act(() => {
      useInverterStore.setState({
        snapshot: makeSnapshot({
          battery_pause_mode: 2,
          battery_pause_slot: emptySlot({
            enabled: true,
            start_hour: 4,
            start_minute: 0,
            end_hour: 3,
            end_minute: 0,
          }),
        }),
      });
    });
    await waitFor(() => {
      expect(screen.queryByText('Applying changes to inverter…')).toBeNull();
    });
  });

  it('saving a disabled Timed Export slot POSTs enabled: false and arms nothing', async () => {
    render(<ControlPage />);

    const section = await sectionByHeading('Timed Export');
    // Leave the toggle off — Save posts the slot as disabled.
    fireEvent.click(within(section).getAllByRole('button', { name: 'Save' })[0]);

    expect(vi.mocked(apiPost)).toHaveBeenCalledWith(
      '/api/control/discharge-slot',
      expect.objectContaining({ slot: 1, enabled: false }),
    );
  });

  it('saving a disabled Timed Discharge window POSTs enabled: false', async () => {
    render(<ControlPage />);

    const section = await sectionByHeading('Timed Discharge');
    fireEvent.click(within(section).getByRole('button', { name: 'Save' }));

    expect(vi.mocked(apiPost)).toHaveBeenCalledWith(
      '/api/control/timed-discharge',
      expect.objectContaining({ enabled: false }),
    );
    expect(screen.getByText('Applying changes to inverter…')).toBeDefined();

    act(() => {
      useInverterStore.setState({
        snapshot: makeSnapshot({
          battery_pause_mode: 0,
          battery_pause_slot: emptySlot(),
        }),
      });
    });
    await waitFor(() => {
      expect(screen.queryByText('Applying changes to inverter…')).toBeNull();
    });
  });

  it('saving slot 2 of the Timed Export schedule POSTs slot: 2 with its own window', async () => {
    render(<ControlPage />);

    const section = await sectionByHeading('Timed Export');
    // Slot 2's editor is the second Save button in the section.
    fireEvent.click(within(section).getByLabelText('Slot 2 disabled'));
    fireEvent.click(within(section).getAllByRole('button', { name: 'Save' })[1]);

    expect(vi.mocked(apiPost)).toHaveBeenCalledWith('/api/control/discharge-slot', {
      slot: 2,
      enabled: true,
      start_hour: 20,
      start_minute: 0,
      end_hour: 22,
      end_minute: 0,
      target_soc: 10,
    });
  });

  it('a saved Timed Discharge window lights the Battery Mode indicator immediately and stays lit once the snapshot confirms', async () => {
    render(<ControlPage />);

    const section = await sectionByHeading('Timed Discharge');
    await enableAndSave(section, 'Slot 1');

    const modeButton = screen.getByRole('button', { name: /Timed Discharge/ });
    // The save applies an optimistic 10s override, so the indicator responds
    // to the save itself — no dead time waiting for the poll round-trip.
    expect(modeButton.getAttribute('aria-pressed')).toBe('true');

    // Once the poll loop confirms HR318=2, the snapshot keeps it lit even
    // after the optimistic override expires.
    act(() => {
      useInverterStore.setState({
        snapshot: makeSnapshot({
          battery_pause_mode: 2,
          battery_pause_slot: emptySlot({
            enabled: true,
            start_hour: 4,
            start_minute: 0,
            end_hour: 3,
            end_minute: 0,
          }),
        }),
      });
    });

    expect(modeButton.getAttribute('aria-pressed')).toBe('true');
  });

  it('surfaces a backend rejection when the window has no duration', async () => {
    vi.mocked(apiPost).mockRejectedValueOnce(
      new Error('Start and end times must differ for an enabled Timed Export slot'),
    );
    render(<ControlPage />);

    const section = await sectionByHeading('Timed Export');
    // Enable slot 1, then collapse the end time onto the start time.
    fireEvent.click(within(section).getByLabelText('Slot 1 disabled'));
    const selects = within(section).getAllByRole('combobox');
    // Slot 1's editor renders Start (hour, minute) then End (hour, minute).
    fireEvent.change(selects[2], { target: { value: '16' } });
    fireEvent.click(within(section).getAllByRole('button', { name: 'Save' })[0]);

    // The editor sent exactly what the user picked — the backend is the one
    // that rejects a zero-duration window.
    expect(vi.mocked(apiPost)).toHaveBeenCalledWith(
      '/api/control/discharge-slot',
      expect.objectContaining({ slot: 1, enabled: true, end_hour: 16 }),
    );
    expect(await within(section).findByText('✗ Error')).toBeDefined();
    expect(within(section).getByRole('alert')).toHaveTextContent(
      'Start and end times must differ for an enabled Timed Export slot',
    );
  });

  it('replaces progress with the full backend error when a Timed Export save fails later', async () => {
    let failSave: ((reason: Error) => void) | undefined;
    vi.mocked(apiPost).mockImplementationOnce(
      () => new Promise((_resolve, reject) => { failSave = reject; }),
    );
    render(<ControlPage />);

    const section = await sectionByHeading('Timed Export');
    fireEvent.click(within(section).getAllByRole('button', { name: 'Save' })[0]);
    expect(screen.getByText('Applying changes to inverter…')).toBeDefined();

    await act(async () => {
      failSave?.(new Error(
        'Timed Export slot 1 could not be saved because inverter writes did not complete. Export was not armed.',
      ));
    });

    expect(screen.queryByText('Applying changes to inverter…')).toBeNull();
    expect(within(section).getByRole('alert')).toHaveTextContent(
      'Timed Export slot 1 could not be saved because inverter writes did not complete. Export was not armed.',
    );
  });

  it('both sections carry matching save-and-enable wording', async () => {
    render(<ControlPage />);

    const exportSection = await sectionByHeading('Timed Export');
    const dischargeSection = await sectionByHeading('Timed Discharge');

    expect(
      within(exportSection).getByText(/Saving an enabled slot also enables Timed Export/),
    ).toBeDefined();
    expect(
      within(dischargeSection).getByText(/Saving an enabled slot also enables Timed Discharge/),
    ).toBeDefined();
  });

  it('a saved Timed Export slot lights the Battery Mode Timed Export indicator once the snapshot confirms', async () => {
    render(<ControlPage />);

    const section = await sectionByHeading('Timed Export');
    await enableAndSave(section, 'Slot 1');

    // The Battery Mode Timed Export button starts unpressed.
    const modeButton = screen.getByRole('button', { name: /Timed Export/ });
    expect(modeButton.getAttribute('aria-pressed')).toBe('false');

    // Simulate the poll loop confirming the armed mode + configured window.
    act(() => {
      useInverterStore.setState({
        snapshot: makeSnapshot({
          enable_discharge: true,
          battery_power_mode: 0,
          discharge_slots: [
            emptySlot({
              enabled: true,
              start_hour: 16,
              end_hour: 19,
              start_minute: 0,
              end_minute: 0,
              target_soc: 4,
            }),
            emptySlot(),
          ],
        }),
      });
    });

    expect(modeButton.getAttribute('aria-pressed')).toBe('true');
  });
});
