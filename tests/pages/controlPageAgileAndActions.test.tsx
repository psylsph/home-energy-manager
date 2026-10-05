import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, within, act } from '@testing-library/react';

// ---------------------------------------------------------------------------
// More Control-page behaviour with no coverage:
//
//  - Agile: the postcode-to-region lookup (every outcome, debounced) and the
//    threshold save, including the rule that keeps charge and discharge
//    thresholds a minimum gap apart.
//  - Force Charge / Force Discharge: a rejected request is reported, and a
//    request the inverter never confirms times out with an explanation.
//  - The EPS switch and Timed Discharge: failure handling and the timeout.
//  - The Inverter Temperature Limiter: its save and the rule that the
//    recovery threshold can never sit above the high threshold.
// ---------------------------------------------------------------------------

const state = vi.hoisted(() => ({
  tempLimiter: { enabled: true, high_threshold: 70, recovery_threshold: 60, confirmation_readings: 3 },
}));

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/agile') {
      return { ok: true, enabled: false, region: 'A', charge_threshold: 10, discharge_threshold: 30 };
    }
    if (path === '/api/auto-winter') {
      return {
        ok: true,
        data: { config: { enabled: false, cold_threshold: 8, recovery_threshold: 12, target_soc: 80, debounce_readings: 10 } },
      };
    }
    if (path === '/api/cosy') {
      return {
        ok: true,
        enabled: false,
        slots: Array.from({ length: 3 }, () => ({
          enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100,
        })),
      };
    }
    if (path === '/api/settings') {
      return { ok: true, data: { import_tariff: 0.285, export_tariff: 0.15, import_tariff_config: null } };
    }
    if (path === '/api/load-limiter') {
      return {
        ok: true,
        data: { config: { enabled: false, threshold_w: 3000, trigger_delay_minutes: 0, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0 } },
      };
    }
    if (path === '/api/temperature-limiter') return { ok: true, data: { config: state.tempLimiter } };
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

function failOn(path: string | RegExp, error: unknown = new Error('rejected')) {
  vi.mocked(apiPost).mockImplementation(async (p: string) => {
    if (typeof path === 'string' ? p === path : path.test(p)) throw error;
    return { ok: true, data: {} };
  });
}

const calls = (path: string) => vi.mocked(apiPost).mock.calls.filter((c) => c[0] === path);

/** The octopus price feed and postcodes.io, answered per URL. */
let postcodeResponse: () => Promise<Partial<Response>>;
let fetchSpy: ReturnType<typeof vi.spyOn>;

/** True when the request goes to the postcodes.io host (parsed, not substring-matched). */
function isPostcodeRequest(input: unknown): boolean {
  try {
    return new URL(String(input)).hostname === 'api.postcodes.io';
  } catch {
    return false;
  }
}

function postcodeCalls() {
  return fetchSpy.mock.calls.filter((c: unknown[]) => isPostcodeRequest(c[0]));
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
  state.tempLimiter = { enabled: true, high_threshold: 70, recovery_threshold: 60, confirmation_readings: 3 };
  postcodeResponse = async () => ({ ok: true, json: async () => ({ status: 200 }) });
  fetchSpy = vi.spyOn(globalThis, 'fetch').mockImplementation((async (input: unknown) => {
    if (isPostcodeRequest(input)) return postcodeResponse();
    // Octopus price feed: no upcoming slots is enough for these tests.
    return { ok: true, json: async () => ({ results: [] }) };
  }) as typeof fetch);
});

afterEach(() => {
  vi.useRealTimers();
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

/** Switch the page into Agile mode so its controls appear. */
async function openAgile() {
  connect();
  render(<ControlPage />);
  const select = (await screen.findAllByRole('combobox'))[0] as HTMLSelectElement;
  await waitFor(() => expect(select).toBeEnabled());
  fireEvent.change(select, { target: { value: 'agile' } });
  return screen.findByPlaceholderText('e.g. SW1A 1AA') as Promise<HTMLInputElement>;
}

describe('<ControlPage/> — Agile postcode lookup', () => {
  async function typeAndWait(box: HTMLInputElement, value: string) {
    fireEvent.change(box, { target: { value } });
    // The lookup is debounced by half a second.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 600));
    });
  }

  it('sets the region from the postcode area', async () => {
    const box = await openAgile();
    await typeAndWait(box, 'SW1A 1AA');

    await screen.findByText('Region set to C');
    expect(postcodeCalls()).toHaveLength(1);
    expect(String(postcodeCalls()[0][0])).toMatch(/postcodes\.io\/postcodes\/SW1A1AA$/);
  });

  it('does not look anything up for fewer than three characters', async () => {
    const box = await openAgile();
    await typeAndWait(box, 'SW');
    expect(postcodeCalls()).toHaveLength(0);
    expect(screen.queryByText(/Region set to/)).toBeNull();
  });

  it('waits for typing to stop, then looks up only the final postcode', async () => {
    const box = await openAgile();
    fireEvent.change(box, { target: { value: 'SW1' } });
    fireEvent.change(box, { target: { value: 'SW1A' } });
    fireEvent.change(box, { target: { value: 'SW1A 1AA' } });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 600));
    });
    expect(postcodeCalls()).toHaveLength(1);
    expect(String(postcodeCalls()[0][0])).toMatch(/SW1A1AA$/);
  });

  it('reports a postcode the service does not know', async () => {
    postcodeResponse = async () => ({ ok: false });
    const box = await openAgile();
    await typeAndWait(box, 'SW1A 1AA');
    await screen.findByText('Could not determine region');
  });

  it('reports a service answer that is not a success', async () => {
    postcodeResponse = async () => ({ ok: true, json: async () => ({ status: 404 }) });
    const box = await openAgile();
    await typeAndWait(box, 'SW1A 1AA');
    await screen.findByText('Could not determine region');
  });

  it('reports a postcode whose area maps to no Octopus region', async () => {
    const box = await openAgile();
    await typeAndWait(box, 'ZZ9 9ZZ');
    await screen.findByText('Could not determine region');
    expect(screen.queryByText(/Region set to/)).toBeNull();
  });

  it('reports a postcode that starts with no letters', async () => {
    const box = await openAgile();
    await typeAndWait(box, '123 456');
    await screen.findByText('Could not determine region');
  });

  it('reports a failed lookup', async () => {
    postcodeResponse = async () => {
      throw new Error('network down');
    };
    const box = await openAgile();
    await typeAndWait(box, 'SW1A 1AA');
    await screen.findByText('Lookup failed');
  });

  it('clears the lookup status again when the postcode is shortened', async () => {
    const box = await openAgile();
    await typeAndWait(box, 'SW1A 1AA');
    await screen.findByText('Region set to C');
    fireEvent.change(box, { target: { value: 'SW' } });
    await waitFor(() => expect(screen.queryByText(/Region set to/)).toBeNull());
  });
});

describe('<ControlPage/> — Agile threshold save', () => {
  const save = async () => {
    await openAgile();
    const section = screen.getByText('Postcode').closest('div.space-y-4') as HTMLElement;
    return within(section).getByRole('button', { name: /^(Save|Saving\.\.\.|✓ Saved|✗ Error)$/ });
  };

  it('posts the region and thresholds without touching the mode', async () => {
    const button = await save();
    fireEvent.click(button);
    await waitFor(() => expect(button.textContent).toBe('✓ Saved'));
    expect(apiPost).toHaveBeenCalledWith('/api/agile', {
      region: 'A',
      charge_threshold: 10,
      discharge_threshold: 30,
    });
  });

  it('keeps discharge at least 5p above charge, so the inverter can still charge', async () => {
    const button = await save();
    const sliders = screen.getAllByRole('slider') as HTMLInputElement[];
    const charge = sliders.find((s) => s.max === '50')!;

    // Charge below 40p keeps the 30p discharge more than 5p away; 28p does not.
    fireEvent.change(charge, { target: { value: '28' } });
    fireEvent.click(button);

    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/agile', {
        region: 'A',
        charge_threshold: 28,
        discharge_threshold: 33,
      }),
    );
  });

  it('shows an error when the save is rejected', async () => {
    const button = await save();
    failOn('/api/agile');
    fireEvent.click(button);
    await waitFor(() => expect(button.textContent).toBe('✗ Error'));
  });
});

describe('<ControlPage/> — Force Charge and Force Discharge', () => {
  it('reports a rejected Force Charge and frees the button', async () => {
    connect();
    failOn('/api/control/force-charge', new Error('inverter busy'));
    render(<ControlPage />);

    fireEvent.click(screen.getByRole('button', { name: /Force Charge/ }));

    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe('inverter busy'));
    expect(screen.queryByText('Starting Force Charge…')).toBeNull();
    expect(screen.getByRole('button', { name: /Force Charge/ })).toBeEnabled();
  });

  it('uses a generic message when the Force Charge rejection carries none', async () => {
    connect();
    failOn('/api/control/force-charge', null);
    render(<ControlPage />);
    fireEvent.click(screen.getByRole('button', { name: /Force Charge/ }));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe('Force Charge failed to start.'));
  });

  it('explains when the inverter never confirms a Force Discharge', async () => {
    connect();
    render(<ControlPage />);
    vi.useFakeTimers({ shouldAdvanceTime: true });

    fireEvent.click(screen.getByRole('button', { name: /Force Discharge/ }));
    await waitFor(() => expect(calls('/api/control/force-discharge')).toHaveLength(1));
    // The request was accepted, but no snapshot ever shows the discharge.
    expect(screen.queryByRole('alert')).toBeNull();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(30_000);
    });

    await waitFor(() =>
      expect(screen.getByRole('alert').textContent).toBe(
        'The inverter did not confirm that Force Discharge started. Please try again.',
      ),
    );
    // And the control is available again rather than stuck pending.
    expect(screen.getByRole('button', { name: /Force Discharge/ })).toBeEnabled();
  });
});

describe('<ControlPage/> — EPS switch', () => {
  const eps = () => screen.getByRole('button', { name: 'Emergency Power Supply' });

  it('is only offered on models that support it', () => {
    connect({ device_type_code: '2001' });
    render(<ControlPage />);
    expect(screen.queryByRole('button', { name: 'Emergency Power Supply' })).toBeNull();
  });

  it('reports a rejected request and frees the switch', async () => {
    connect({ device_type_code: '3001', ac_eps_enabled: false });
    failOn('/api/control/eps', new Error('EPS write rejected'));
    render(<ControlPage />);

    fireEvent.click(eps());

    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe('EPS write rejected'));
    expect(screen.queryByText('Updating EPS…')).toBeNull();
    expect(eps()).toBeEnabled();
  });

  it('uses a generic message when the rejection carries none', async () => {
    connect({ device_type_code: '3001', ac_eps_enabled: false });
    failOn('/api/control/eps', null);
    render(<ControlPage />);
    fireEvent.click(eps());
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe('EPS toggle failed.'));
  });

  it('says so when the inverter never confirms the change', async () => {
    connect({ device_type_code: '3001', ac_eps_enabled: false });
    render(<ControlPage />);
    vi.useFakeTimers({ shouldAdvanceTime: true });

    fireEvent.click(eps());
    await screen.findByText('Updating EPS…');
    expect(eps()).toBeDisabled();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(30_000);
    });

    await waitFor(() =>
      expect(screen.getByRole('alert').textContent).toBe('EPS did not confirm the change. Please try again.'),
    );
    expect(screen.queryByText('Updating EPS…')).toBeNull();
  });
});

describe('<ControlPage/> — Timed Discharge toggle', () => {
  it('reports a rejected request and drops the optimistic state', async () => {
    connect();
    failOn('/api/control/timed-discharge', new Error('pause registers unavailable'));
    render(<ControlPage />);

    fireEvent.click(screen.getByRole('button', { name: /Timed Discharge/ }));

    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe('pause registers unavailable'));
    expect(screen.queryByText('Applying…')).toBeNull();
    expect(screen.getByRole('button', { name: /Timed Discharge/ })).toBeEnabled();
  });

  it('uses a generic message when the rejection carries none', async () => {
    connect();
    failOn('/api/control/timed-discharge', null);
    render(<ControlPage />);
    fireEvent.click(screen.getByRole('button', { name: /Timed Discharge/ }));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe('Timed Discharge toggle failed.'));
  });
});

describe('<ControlPage/> — Inverter Temperature Limiter', () => {
  const block = () =>
    screen.getByRole('heading', { name: 'Inverter Temperature Limiter' }).parentElement as HTMLElement;
  const save = () => within(block()).getByRole('button', { name: /^(Save|Saving\.\.\.|✓ Saved|✗ Error)$/ });

  it('posts the loaded configuration', async () => {
    connect();
    render(<ControlPage />);
    // Sliders only appear once the saved (enabled) configuration has loaded.
    await waitFor(() => expect(within(block()).getAllByRole('slider').length).toBeGreaterThan(0));

    fireEvent.click(save());

    await waitFor(() => expect(save().textContent).toBe('✓ Saved'));
    expect(apiPost).toHaveBeenCalledWith('/api/temperature-limiter', {
      enabled: true,
      high_threshold: 70,
      recovery_threshold: 60,
      confirmation_readings: 3,
    });
  });

  it('never lets the recovery threshold sit above the high threshold', async () => {
    connect();
    render(<ControlPage />);
    await waitFor(() => expect(within(block()).getAllByRole('slider').length).toBeGreaterThan(0));
    const high = within(block()).getAllByRole('slider').find((s) => (s as HTMLInputElement).min === '30')!;

    // Lowering the high threshold to 50 must pull the 60 recovery below it.
    fireEvent.change(high, { target: { value: '50' } });
    fireEvent.click(save());

    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/temperature-limiter', {
        enabled: true,
        high_threshold: 50,
        recovery_threshold: 49,
        confirmation_readings: 3,
      }),
    );
  });

  it('shows an error when the save is rejected', async () => {
    connect();
    failOn('/api/temperature-limiter');
    render(<ControlPage />);
    await waitFor(() => expect(within(block()).getAllByRole('slider').length).toBeGreaterThan(0));
    fireEvent.click(save());
    await waitFor(() => expect(save().textContent).toBe('✗ Error'));
  });
});
