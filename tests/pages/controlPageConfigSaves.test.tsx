import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, within } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Control-page configuration saves with no coverage on their failure paths:
// Auto Winter, Load Discharge Limiter, Charging Mode / Cosy slots, and
// Adaptive Charge (client-side validation + server rejection). Each must say
// so when a save fails, and the Charging Mode controls must stay locked when
// the saved Cosy schedule could not be loaded (applying empty fallback slots
// would overwrite it).
// ---------------------------------------------------------------------------

const cosySlots = () =>
  Array.from({ length: 3 }, () => ({
    enabled: false,
    start_hour: 0,
    start_minute: 0,
    end_hour: 0,
    end_minute: 0,
    target_soc: 100,
  }));

const defaultLimiter = vi.hoisted(() => ({
  enabled: false,
  threshold_w: 3000,
  trigger_delay_minutes: 0,
  start_hour: 0,
  start_minute: 0,
  end_hour: 0,
  end_minute: 0,
}));

const state = vi.hoisted(() => ({
  cosy: { ok: true, enabled: false, slots: [] as unknown[] } as unknown,
  cosyFails: false,
  adaptive: null as unknown,
  winter: {
    enabled: false,
    cold_threshold: 8,
    recovery_threshold: 12,
    target_soc: 80,
    debounce_readings: 10,
  },
  limiter: {
    enabled: false,
    threshold_w: 3000,
    trigger_delay_minutes: 0,
    start_hour: 0,
    start_minute: 0,
    end_hour: 0,
    end_minute: 0,
  },
}));

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/agile') return { ok: true, enabled: false };
    if (path === '/api/auto-winter') return { ok: true, data: { config: state.winter } };
    if (path === '/api/cosy') {
      if (state.cosyFails) throw new Error('cosy unavailable');
      return state.cosy;
    }
    if (path === '/api/adaptive-charge' && state.adaptive) {
      return { ok: true, data: { config: state.adaptive } };
    }
    if (path === '/api/settings') {
      return { ok: true, data: { import_tariff: 0.285, export_tariff: 0.15, import_tariff_config: null } };
    }
    if (path === '/api/load-limiter') return { ok: true, data: { config: state.limiter } };
    return { ok: true, data: {} };
  }),
  apiPost: vi.fn(),
  getApiBase: () => 'http://localhost:7337',
  getServerPort: () => 7337,
  fetchHistory: vi.fn().mockResolvedValue({}),
  isTauri: false,
}));

import ControlPage from '../../src/pages/ControlPage';
import { useInverterStore } from '../../src/store/useInverterStore';
import { apiPost } from '../../src/lib/api';
import { DEFAULT_ADAPTIVE_PERIOD } from '../../src/lib/adaptiveCharge';
import { makeSnapshot } from '../fixtures/snapshot';
import type { InverterSnapshot } from '../../src/lib/types';

function connect(overrides: Partial<InverterSnapshot> = {}) {
  useInverterStore.setState({
    snapshot: makeSnapshot(overrides),
    developerMode: false,
    connectionState: 'connected',
    connectedHost: '192.168.1.36:8899',
    batteryModePending: null,
    batteryModeError: null,
  });
}

function failOn(path: string, error: unknown = new Error('rejected')) {
  vi.mocked(apiPost).mockImplementation(async (p: string) => {
    if (p === path) throw error;
    return { ok: true, data: {} };
  });
}

const calls = (path: string) => vi.mocked(apiPost).mock.calls.filter((c) => c[0] === path);

/** The page section that carries the given heading. */
function section(name: string): HTMLElement {
  const heading = screen.getByRole('heading', { name });
  return heading.closest('section') as HTMLElement;
}

beforeEach(() => {
  vi.spyOn(console, 'error').mockImplementation(() => {});
  vi.spyOn(console, 'warn').mockImplementation(() => {});
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
  window.localStorage.clear();
  vi.mocked(apiPost).mockReset();
  vi.mocked(apiPost).mockResolvedValue({ ok: true, data: {} });
  state.cosy = { ok: true, enabled: false, slots: cosySlots() };
  state.cosyFails = false;
  state.adaptive = null;
  state.limiter = { ...defaultLimiter };
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  cleanup();
  useInverterStore.setState({
    snapshot: null,
    connectionState: 'disconnected',
    developerMode: false,
    batteryModePending: null,
    batteryModeError: null,
  });
});

describe('<ControlPage/> — Auto Winter save', () => {
  const save = () => within(section('Auto Winter Mode')).getByRole('button', { name: /Save|Saving|Saved|Error/ });

  it('posts the loaded configuration and confirms it', async () => {
    connect();
    render(<ControlPage />);

    fireEvent.click(save());

    await waitFor(() => expect(save().textContent).toBe('✓ Saved'));
    expect(apiPost).toHaveBeenCalledWith('/api/auto-winter', {
      enabled: false,
      cold_threshold: 8,
      recovery_threshold: 12,
      target_soc: 80,
      debounce_readings: 10,
    });
  });

  it('saves the enabled flag after it is switched on', async () => {
    connect();
    render(<ControlPage />);
    const winter = section('Auto Winter Mode');

    // The first button in the section is the master Enable toggle.
    fireEvent.click(within(winter).getAllByRole('button')[0]);
    expect(within(winter).getByText('Cold Threshold')).toBeInTheDocument();
    fireEvent.click(save());

    await waitFor(() => expect(calls('/api/auto-winter')).toHaveLength(1));
    expect(calls('/api/auto-winter')[0][1]).toMatchObject({ enabled: true });
  });

  it('shows an error when the save is rejected', async () => {
    connect();
    failOn('/api/auto-winter');
    render(<ControlPage />);
    fireEvent.click(save());
    await waitFor(() => expect(save().textContent).toBe('✗ Error'));
  });
});

describe('<ControlPage/> — Load Discharge Limiter save', () => {
  // This component's root is a div, not a section, so scope to the heading's
  // own container rather than the enclosing "Battery and Power Controls" one.
  const save = () =>
    within(screen.getByRole('heading', { name: 'Load Discharge Limiter' }).parentElement as HTMLElement).getByRole(
      'button',
      { name: /^(Save|Saving\.\.\.|✓ Saved|✗ Error)$/ },
    );

  it('posts the loaded configuration', async () => {
    state.limiter = {
      enabled: true,
      threshold_w: 3500,
      trigger_delay_minutes: 7,
      start_hour: 1,
      start_minute: 30,
      end_hour: 6,
      end_minute: 0,
    };
    connect();
    render(<ControlPage />);
    // Wait for the saved configuration to load: until then the form holds
    // defaults, and saving would post those instead.
    await screen.findByText('7 min');

    fireEvent.click(save());

    await waitFor(() => expect(save().textContent).toBe('✓ Saved'));
    expect(apiPost).toHaveBeenCalledWith('/api/load-limiter', state.limiter);
  });

  it('shows an error when the save is rejected', async () => {
    connect();
    failOn('/api/load-limiter');
    render(<ControlPage />);
    fireEvent.click(save());
    await waitFor(() => expect(save().textContent).toBe('✗ Error'));
  });
});

describe('<ControlPage/> — Charging Mode', () => {
  const modeSelect = () => within(section('Charging Mode')).getByRole('combobox') as HTMLSelectElement;
  const apply = () => within(section('Charging Mode')).getByRole('button', { name: /^(Apply|✓|!|\.\.\.)$/ });

  it('stays locked when the saved Cosy schedule could not be loaded', async () => {
    // Applying empty fallback slots would overwrite the user's persisted schedule.
    state.cosyFails = true;
    connect();
    render(<ControlPage />);

    await waitFor(() => expect(modeSelect()).toBeDisabled());
    expect(apply()).toBeDisabled();
    fireEvent.click(apply());
    expect(calls('/api/charging-mode')).toHaveLength(0);
  });

  it('applies the current mode together with the loaded Cosy slots', async () => {
    connect();
    render(<ControlPage />);
    await waitFor(() => expect(apply()).toBeEnabled());

    fireEvent.click(apply());

    await waitFor(() => expect(apply().textContent).toBe('✓'));
    expect(apiPost).toHaveBeenCalledWith('/api/charging-mode', {
      mode: 'standard',
      cosy_slots: cosySlots(),
    });
  });

  it('flags a rejected apply', async () => {
    connect();
    failOn('/api/charging-mode');
    render(<ControlPage />);
    await waitFor(() => expect(apply()).toBeEnabled());

    fireEvent.click(apply());

    await waitFor(() => expect(apply().textContent).toBe('!'));
  });

  describe('Cosy slots', () => {
    beforeEach(() => {
      state.cosy = { ok: true, enabled: true, slots: cosySlots() };
    });
    const saveSlots = () => within(section('Charging Mode')).getByRole('button', { name: /Save slots|Saving|Saved|Error/ });

    it('saves an edited slot', async () => {
      connect();
      render(<ControlPage />);
      await waitFor(() => expect(saveSlots()).toBeInTheDocument());
      const charging = section('Charging Mode');

      // Enable the first slot, then pull its target SOC down.
      fireEvent.click(charging.querySelectorAll<HTMLButtonElement>('button[class*="w-9"]')[0]);
      const targets = within(charging).getAllByRole('slider');
      fireEvent.change(targets[0], { target: { value: '60' } });
      fireEvent.click(saveSlots());

      await waitFor(() => expect(saveSlots().textContent).toBe('✓ Saved'));
      const [, body] = calls('/api/cosy')[0] as [string, { enabled: boolean; slots: Array<Record<string, unknown>> }];
      expect(body.enabled).toBe(true);
      expect(body.slots[0]).toMatchObject({ enabled: true, target_soc: 60 });
      expect(body.slots[1]).toMatchObject({ enabled: false });
    });

    it('shows an error when saving the slots is rejected', async () => {
      connect();
      failOn('/api/cosy');
      render(<ControlPage />);
      await waitFor(() => expect(saveSlots()).toBeInTheDocument());
      fireEvent.click(saveSlots());
      await waitFor(() => expect(saveSlots().textContent).toBe('✗ Error'));
    });
  });
});

describe('<ControlPage/> — Adaptive Charge save', () => {
  const save = () => screen.getByRole('button', { name: /Save Adaptive Charge|Saving|Saved|Check settings/ });
  const period = (overrides: Record<string, unknown> = {}) => ({
    ...DEFAULT_ADAPTIVE_PERIOD,
    enabled: true,
    low_soc: 20,
    recovery_soc: 30,
    ...overrides,
  });

  beforeEach(() => {
    state.adaptive = { periods: [period()], confirmation_readings: 2 };
  });

  it('posts a valid configuration', async () => {
    connect({ adaptive_charge_enabled: true });
    render(<ControlPage />);
    await waitFor(() => expect(save()).toBeInTheDocument());

    fireEvent.click(save());

    await waitFor(() => expect(save().textContent).toBe('✓ Saved'));
    expect(apiPost).toHaveBeenCalledWith('/api/adaptive-charge', {
      config: { periods: [period()], confirmation_readings: 2 },
    });
  });

  it('refuses a configuration with no enabled period, without calling the server', async () => {
    state.adaptive = { periods: [period({ enabled: false })], confirmation_readings: 2 };
    connect({ adaptive_charge_enabled: true });
    render(<ControlPage />);
    // Wait for the saved (disabled) configuration to load.
    await waitFor(() => expect(screen.getByRole('checkbox', { name: /Enabled/ })).not.toBeChecked());

    fireEvent.click(save());

    expect(await screen.findByText('Enable at least one period.')).toBeInTheDocument();
    expect(save().textContent).toBe('Check settings');
    expect(calls('/api/adaptive-charge')).toHaveLength(0);
  });

  it('refuses a recovery SOC at or below the low SOC', async () => {
    state.adaptive = { periods: [period({ low_soc: 40, recovery_soc: 40 })], confirmation_readings: 2 };
    connect({ adaptive_charge_enabled: true });
    render(<ControlPage />);
    await waitFor(() => expect(screen.getByRole('checkbox', { name: /Enabled/ })).toBeChecked());

    fireEvent.click(save());

    expect(await screen.findByText(/Recovery SOC must be above Low SOC/)).toBeInTheDocument();
    expect(calls('/api/adaptive-charge')).toHaveLength(0);
  });

  it('shows the server message when a valid configuration is rejected', async () => {
    connect({ adaptive_charge_enabled: true });
    failOn('/api/adaptive-charge', new Error('Adaptive Charge is not supported on this inverter'));
    render(<ControlPage />);
    await waitFor(() => expect(save()).toBeInTheDocument());

    fireEvent.click(save());

    expect(
      await screen.findByText('Adaptive Charge is not supported on this inverter'),
    ).toBeInTheDocument();
    expect(save().textContent).toBe('Check settings');
  });

  it('uses a generic message when the rejection carries none', async () => {
    connect({ adaptive_charge_enabled: true });
    failOn('/api/adaptive-charge', null);
    render(<ControlPage />);
    await waitFor(() => expect(save()).toBeInTheDocument());
    fireEvent.click(save());
    expect(await screen.findByText('Unable to save Adaptive Charge.')).toBeInTheDocument();
  });
});
