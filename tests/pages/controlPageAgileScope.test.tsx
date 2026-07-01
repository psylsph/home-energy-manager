/**
 * Tests for the three-mode Agile scope UI (Off / Full / Charge Only /
 * Discharge Only) on ControlPage.
 *
 * Background: Agile Octopus originally drove both charge and discharge
 * sides of the inverter from prices, hiding the user's manual schedule
 * sections whenever Agile was on. The slot-based refactor adds two
 * intermediate scopes — "Charge only" and "Discharge only" — that
 * drive just one side from prices and leave the user's schedule in
 * control of the other. The visibility matrix in ControlPage.tsx
 * is:
 *
 *   Standard       — all 3 sections visible, fully editable
 *   Cosy           — all 3 sections visible (Cosy owns charge only)
 *   Agile (full)   — all 3 sections hidden (Agile owns both)
 *   Agile Charge   — Charge Schedule hidden, Timed Discharge +
 *     Only           Discharge Schedule visible + greyed
 *   Agile          — Timed Discharge + Discharge Schedule hidden,
 *     Discharge Only  Charge Schedule visible + greyed
 *
 * The greyed sections also get a "Controlled by manual timer"
 * label so the user understands why the slots are dim while an
 * Agile sub-mode is active.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, within } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    // Default mock — Standard mode + Scope Off unless overridden.
    if (path === '/api/agile')
      return {
        ok: true,
        enabled: false,
        scope: 'off',
        region: 'A',
        charge_threshold: 10,
        discharge_threshold: 30,
      };
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
    return { ok: true };
  }),
  apiPost: vi.fn(async () => ({ ok: true })),
}));

import { apiGet, apiPost } from '../../src/lib/api';
import { useInverterStore } from '../../src/store/useInverterStore';

function silenceConsoleError() {
  vi.spyOn(console, 'error').mockImplementation(() => {});
  vi.spyOn(console, 'warn').mockImplementation(() => {});
}

function emptySlot() {
  return {
    enabled: false,
    start_hour: 0,
    start_minute: 0,
    end_hour: 0,
    end_minute: 0,
    target_soc: 100,
  };
}

function makeSnapshot(overrides: Record<string, unknown> = {}) {
  return {
    timestamp: 0,
    grid_voltage: 230,
    grid_frequency: 50,
    grid_power: 0,
    solar_power: 0,
    home_power: 0,
    battery_power: 0,
    battery_soc: 50,
    battery_temperature: 25,
    battery_current: 0,
    battery_voltage: 50,
    battery_cycles: 100,
    battery_state: 'idle',
    battery_capacity_kwh: 10,
    battery_charge_limit: 50,
    battery_discharge_limit: 50,
    energy_today: { solar_kwh: 0, grid_import_kwh: 0, grid_export_kwh: 0, home_kwh: 0, battery_charge_kwh: 0, battery_discharge_kwh: 0 },
    energy_lifetime: { solar_kwh: 0, grid_import_kwh: 0, grid_export_kwh: 0, home_kwh: 0, battery_charge_kwh: 0, battery_discharge_kwh: 0 },
    pv1_voltage: 0,
    pv1_current: 0,
    pv1_power: 0,
    pv2_voltage: 0,
    pv2_current: 0,
    pv2_power: 0,
    grid_loss: false,
    inverter_trip: false,
    battery_over_temp: false,
    inverter_temperature: 30,
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
    agile_scope: 'off',
    max_charge_slots: 2,
    max_discharge_slots: 2,
    charge_slots: [emptySlot(), emptySlot()],
    discharge_slots: [emptySlot(), emptySlot()],
    meters: [],
    inverter_serial: 'FD2328G358',
    firmware_version: '318',
    dsp_firmware_version: '318',
    dc_dsp_firmware_version: '',
    device_type: 'ac_coupled',
    device_type_display: 'AC Coupled',
    device_type_code: '3001',
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

describe('<ControlPage/> — Agile scope UI', () => {
  beforeEach(() => {
    silenceConsoleError();
    // Reset mock implementations from any previous test so per-test
    // `mockImplementation` calls don't bleed across tests.
    vi.mocked(apiGet).mockReset();
    vi.mocked(apiGet).mockImplementation(async (path: string) => {
      // Default mock — Standard mode + Scope Off unless overridden.
      if (path === '/api/agile')
        return {
          ok: true,
          enabled: false,
          scope: 'off',
          region: 'A',
          charge_threshold: 10,
          discharge_threshold: 30,
        };
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
      return { ok: true };
    });
    vi.mocked(apiPost).mockReset();
    vi.mocked(apiPost).mockImplementation(async () => ({ ok: true }));
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
    // ControlPage now gates its controls on connectionState === 'connected'
    // (matching Battery / Solar / Inverter / Meters). These tests exercise
    // the Agile-scope UI, not the connection gate, so start each one
    // connected with a snapshot in place.
    useInverterStore.setState({ connectionState: 'connected' });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    cleanup();
    useInverterStore.setState({ snapshot: null, connectionState: 'disconnected' });
  });

  // ---------------------------------------------------------------
  // Scope select rendering
  // ---------------------------------------------------------------

  it('renders the select with five mode options including three Agile sub-modes', async () => {
    useInverterStore.setState({ snapshot: makeSnapshot() });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    const allCombos = await screen.findAllByRole('combobox');
    const select = allCombos[0] as HTMLSelectElement;

    const optionValues = Array.from(select.options).map((o) => o.value);
    expect(optionValues).toContain('standard');
    expect(optionValues).toContain('cosy');
    expect(optionValues).toContain('agile');
    expect(optionValues).toContain('agile_charge');
    expect(optionValues).toContain('agile_discharge');
  });

  // ---------------------------------------------------------------
  // Wire format: scope field is sent on POST /api/agile
  // ---------------------------------------------------------------

  it('sends scope="full" when selecting Agile (full)', async () => {
    useInverterStore.setState({ snapshot: makeSnapshot() });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    const allCombos = await screen.findAllByRole('combobox');
    const select = allCombos[0] as HTMLSelectElement;

    fireEvent.change(select, { target: { value: 'agile' } });
    // Wait for the POST to fire after the change.
    await waitFor(() => {
      expect(apiPost).toHaveBeenCalledWith(
        '/api/agile',
        expect.objectContaining({ scope: 'full' }),
      );
    });
  });

  it('sends scope="charge_only" when selecting Agile — Charge only', async () => {
    useInverterStore.setState({ snapshot: makeSnapshot() });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    const allCombos = await screen.findAllByRole('combobox');
    const select = allCombos[0] as HTMLSelectElement;

    fireEvent.change(select, { target: { value: 'agile_charge' } });
    await waitFor(() => {
      expect(apiPost).toHaveBeenCalledWith(
        '/api/agile',
        expect.objectContaining({ scope: 'charge_only' }),
      );
    });
  });

  it('sends scope="discharge_only" when selecting Agile — Discharge only', async () => {
    useInverterStore.setState({ snapshot: makeSnapshot() });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    const allCombos = await screen.findAllByRole('combobox');
    const select = allCombos[0] as HTMLSelectElement;

    fireEvent.change(select, { target: { value: 'agile_discharge' } });
    await waitFor(() => {
      expect(apiPost).toHaveBeenCalledWith(
        '/api/agile',
        expect.objectContaining({ scope: 'discharge_only' }),
      );
    });
  });

  it('sends scope="off" when switching back to Standard via the dropdown', async () => {
    // Start in Agile then switch to Standard.
    useInverterStore.setState({
      snapshot: makeSnapshot({ agile_scope: 'full', agile_enabled: true }),
    });
    vi.mocked(apiGet).mockImplementation(async (path: string) => {
      // Important: return proper shapes for every endpoint the page's
      // useEffects hit. A bare `{ ok: true }` for /api/cosy would
      // throw inside the slot loader (res.slots undefined) and
      // `loaded` would never flip true, causing `handleModeChange`'s
      // guard to early-return without firing the POST we're testing.
      if (path === '/api/agile')
        return { ok: true, enabled: true, scope: 'full', region: 'A', charge_threshold: 10, discharge_threshold: 30 };
      if (path === '/api/cosy')
        return {
          ok: true,
          enabled: false,
          slots: Array.from({ length: 3 }, () => ({
            enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100,
          })),
        };
      return { ok: true };
    });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    const allCombos = await screen.findAllByRole('combobox');
    const select = allCombos[0] as HTMLSelectElement;

    // The bootstrap useEffect should have set the dropdown to 'agile'
    // based on the snapshot's agile_scope.
    await waitFor(() => {
      expect(select.value).toBe('agile');
    });

    fireEvent.change(select, { target: { value: 'standard' } });
    await waitFor(() => {
      expect(apiPost).toHaveBeenCalledWith(
        '/api/agile',
        expect.objectContaining({ scope: 'off' }),
      );
    }, { timeout: 3000 });
  });

  // ---------------------------------------------------------------
  // Bootstrap from snapshot: scope field drives initial mode
  // ---------------------------------------------------------------

  it('bootstraps chargeMode to agile_charge when snapshot.agile_scope is charge_only', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ agile_scope: 'charge_only', agile_enabled: true }),
    });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    const allCombos = await screen.findAllByRole('combobox');
    const select = allCombos[0] as HTMLSelectElement;
    expect(select.value).toBe('agile_charge');
  });

  it('bootstraps chargeMode to agile_discharge when snapshot.agile_scope is discharge_only', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ agile_scope: 'discharge_only', agile_enabled: true }),
    });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    const allCombos = await screen.findAllByRole('combobox');
    const select = allCombos[0] as HTMLSelectElement;
    expect(select.value).toBe('agile_discharge');
  });

  it('bootstraps chargeMode to agile when snapshot.agile_scope is full', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ agile_scope: 'full', agile_enabled: true }),
    });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    const allCombos = await screen.findAllByRole('combobox');
    const select = allCombos[0] as HTMLSelectElement;
    expect(select.value).toBe('agile');
  });

  it('bootstraps chargeMode to standard when snapshot.agile_scope is off', async () => {
    useInverterStore.setState({ snapshot: makeSnapshot() });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    const allCombos = await screen.findAllByRole('combobox');
    const select = allCombos[0] as HTMLSelectElement;
    expect(select.value).toBe('standard');
  });

  // ---------------------------------------------------------------
  // Threshold visibility per scope
  // ---------------------------------------------------------------

  it('shows both thresholds in Agile (full) mode', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ agile_scope: 'full', agile_enabled: true }),
    });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    await waitFor(() => {
      expect(screen.getByText('Charge when below')).toBeDefined();
    });
    expect(screen.getByText('Discharge when above')).toBeDefined();
  });

  it('hides Discharge threshold in Agile Charge Only mode and shows the hidden-value hint', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ agile_scope: 'charge_only', agile_enabled: true }),
    });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    await waitFor(() => {
      expect(screen.getByText('Charge when below')).toBeDefined();
    });
    // Discharge threshold slider is hidden.
    expect(screen.queryByText('Discharge when above')).toBeNull();
    // The hidden-value hint is shown so the user knows their setting
    // is preserved.
    expect(screen.getByText(/Discharge threshold.*hidden because Charge Only mode ignores discharging/)).toBeDefined();
  });

  it('hides Charge threshold in Agile Discharge Only mode and shows the hidden-value hint', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ agile_scope: 'discharge_only', agile_enabled: true }),
    });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    await waitFor(() => {
      expect(screen.getByText('Discharge when above')).toBeDefined();
    });
    expect(screen.queryByText('Charge when below')).toBeNull();
    expect(screen.getByText(/Charge threshold.*hidden because Discharge Only mode ignores charging/)).toBeDefined();
  });

  // ---------------------------------------------------------------
  // "Controlled by manual timer" label on coexisting schedule slots
  // ---------------------------------------------------------------

  it('shows the "Controlled by manual timer" label on discharge slots in Agile Charge Only mode', async () => {
    // Charge Only mode owns charging; the user's Discharge Schedule
    // coexists and should render with the explanatory label.
    useInverterStore.setState({
      snapshot: makeSnapshot({
        agile_scope: 'charge_only',
        agile_enabled: true,
        // Give the discharge slot a configured window so the editor
        // renders its body (not just the toggle).
        discharge_slots: [{
          enabled: true, start_hour: 16, start_minute: 0, end_hour: 19, end_minute: 0, target_soc: 4,
        }, emptySlot()],
      }),
    });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    await screen.findByRole('heading', { name: 'Discharge Schedule', exact: true });
    // The label should appear on each visible discharge slot.
    expect(screen.getAllByText(/Controlled by manual timer/).length).toBeGreaterThan(0);
  });

  it('does NOT show the "Controlled by manual timer" label in Standard mode', async () => {
    // Regression guard: the label must only appear in Agile sub-modes.
    // Standard mode renders the same slots without the label.
    useInverterStore.setState({
      snapshot: makeSnapshot({
        discharge_slots: [{
          enabled: true, start_hour: 16, start_minute: 0, end_hour: 19, end_minute: 0, target_soc: 4,
        }, emptySlot()],
      }),
    });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    await screen.findByRole('heading', { name: 'Discharge Schedule', exact: true });
    expect(screen.queryByText(/Controlled by manual timer/)).toBeNull();
  });

  it('hides Charge Schedule section in Agile Charge Only mode', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ agile_scope: 'charge_only', agile_enabled: true }),
    });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    // Wait for the page to settle.
    await screen.findByRole('heading', { name: 'Discharge Schedule', exact: true });
    expect(screen.queryByRole('heading', { name: 'Charge Schedule', exact: true })).toBeNull();
  });

  it('hides Discharge Schedule section in Agile Discharge Only mode', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ agile_scope: 'discharge_only', agile_enabled: true }),
    });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    await screen.findByRole('heading', { name: 'Charge Schedule', exact: true });
    expect(screen.queryByRole('heading', { name: 'Discharge Schedule', exact: true })).toBeNull();
    expect(screen.queryByRole('heading', { name: 'Timed Discharge', exact: true })).toBeNull();
  });

  it('hides all three schedule sections in Agile (full) mode', async () => {
    // Regression guard for the existing Agile behaviour — Full mode
    // still hides everything because it owns both sides.
    useInverterStore.setState({
      snapshot: makeSnapshot({ agile_scope: 'full', agile_enabled: true }),
    });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    await waitFor(() => {
      expect(screen.queryByRole('heading', { name: 'Charge Schedule', exact: true })).toBeNull();
      expect(screen.queryByRole('heading', { name: 'Discharge Schedule', exact: true })).toBeNull();
      expect(screen.queryByRole('heading', { name: 'Timed Discharge', exact: true })).toBeNull();
    });
  });

  // -----------------------------------------------------------------
  // Threshold gap enforcement
  // -----------------------------------------------------------------
  //
  // The Apply handler (saveConfig) clamps the discharge threshold up to
  // charge_threshold + 5 before POSTing, so an inverted or overlapping
  // pair can never reach the backend. These tests pin the clamp because
  // an inverted pair would make the state machine never charge (price is
  // always >= the discharge threshold) — a silent footgun.

  it('clamps discharge threshold up to charge_threshold + 5 when the pair is inverted', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ agile_scope: 'full', agile_enabled: true }),
    });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    // Scope to the AgileControls root (it shares a root with the
    // thresholds AND the Save button, which disambiguates it from the
    // per-slot "Save" buttons in the schedule editors below).
    const agileRoot = screen.getByText('Charge when below').closest('div.space-y-4')!;
    const chargeSlider = within(
      screen.getByText('Charge when below').parentElement!.parentElement!,
    ).getByRole('slider');
    const dischargeSlider = within(
      screen.getByText('Discharge when above').parentElement!.parentElement!,
    ).getByRole('slider');

    // Drag charge above discharge (inverted).
    fireEvent.change(chargeSlider, { target: { value: '30' } });
    fireEvent.change(dischargeSlider, { target: { value: '10' } });

    // The Agile Apply button reads "Save" (Cosy's reads "Apply").
    fireEvent.click(within(agileRoot).getByRole('button', { name: 'Save' }));

    await waitFor(() => {
      expect(apiPost).toHaveBeenCalledWith(
        '/api/agile',
        expect.objectContaining({
          charge_threshold: 30,
          discharge_threshold: 35, // clamped up to charge + 5
        }),
      );
    });
  });

  it('leaves a valid 5p+ gap untouched on Apply', async () => {
    useInverterStore.setState({
      snapshot: makeSnapshot({ agile_scope: 'full', agile_enabled: true }),
    });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    const agileRoot = screen.getByText('Charge when below').closest('div.space-y-4')!;
    const chargeSlider = within(
      screen.getByText('Charge when below').parentElement!.parentElement!,
    ).getByRole('slider');
    const dischargeSlider = within(
      screen.getByText('Discharge when above').parentElement!.parentElement!,
    ).getByRole('slider');

    // 12p / 35p — a 23p gap, well clear of the clamp.
    fireEvent.change(chargeSlider, { target: { value: '12' } });
    fireEvent.change(dischargeSlider, { target: { value: '35' } });
    fireEvent.click(within(agileRoot).getByRole('button', { name: 'Save' }));

    await waitFor(() => {
      expect(apiPost).toHaveBeenCalledWith(
        '/api/agile',
        expect.objectContaining({
          charge_threshold: 12,
          discharge_threshold: 35,
        }),
      );
    });
  });

  it('saves Agile thresholds without forcing Agile mode on', async () => {
    useInverterStore.setState({
      // Render the Agile threshold controls, then assert their Save is a
      // pure threshold PATCH. The backend tests cover the Standard/off
      // case; this page test pins the front-end payload so it cannot
      // reintroduce `enabled: true` (legacy Full toggle) or a stale scope
      // field and silently re-arm Agile.
      snapshot: makeSnapshot({ agile_scope: 'full', agile_enabled: true }),
    });
    const { default: ControlPage } = await import('../../src/pages/ControlPage');
    render(<ControlPage />);

    const agileRoot = screen.getByText('Charge when below').closest('div.space-y-4')!;
    const chargeSlider = within(
      screen.getByText('Charge when below').parentElement!.parentElement!,
    ).getByRole('slider');
    const dischargeSlider = within(
      screen.getByText('Discharge when above').parentElement!.parentElement!,
    ).getByRole('slider');

    fireEvent.change(chargeSlider, { target: { value: '11' } });
    fireEvent.change(dischargeSlider, { target: { value: '29' } });
    fireEvent.click(within(agileRoot).getByRole('button', { name: 'Save' }));

    await waitFor(() => {
      expect(apiPost).toHaveBeenCalledWith(
        '/api/agile',
        expect.objectContaining({
          charge_threshold: 11,
          discharge_threshold: 29,
        }),
      );
    });
    const agileCall = vi.mocked(apiPost).mock.calls.find(([path]) => path === '/api/agile');
    expect(agileCall).toBeDefined();
    const payload = agileCall?.[1] as Record<string, unknown>;
    expect(payload).not.toHaveProperty('enabled');
    expect(payload).not.toHaveProperty('scope');
  });
});
