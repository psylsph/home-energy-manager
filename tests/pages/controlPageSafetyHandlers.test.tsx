import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Control-page handlers that change what a physical battery or inverter does
// and had no coverage on their failure / decline paths:
//
//  - Start Battery Calibration (a multi-hour cycle that cannot be cancelled)
//    and Reboot Inverter: both sit behind a confirm() that must be honoured,
//    and both must report a failed request instead of looking sent.
//  - The Eco / Timed Charge mode toggles: a rejected request must clear the
//    "Applying…" state and show an error, not leave the page stuck.
//  - The discharge-limit and active-power saves.
// ---------------------------------------------------------------------------

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
        data: { import_tariff: 0.285, export_tariff: 0.15, import_tariff_config: null },
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

function connect(overrides: Partial<InverterSnapshot> = {}, developerMode = false) {
  useInverterStore.setState({
    snapshot: makeSnapshot(overrides),
    developerMode,
    connectionState: 'connected',
    connectedHost: '192.168.1.36:8899',
    // Mode-change state lives in the store (it survives navigation by design),
    // so a toggle left "applying" by one test must not leak into the next.
    batteryModePending: null,
    batteryModeError: null,
  });
}

/** Post mock that fails the given path and accepts everything else. */
function failOn(path: string, error: unknown = new Error('dongle busy')) {
  vi.mocked(apiPost).mockImplementation(async (p: string) => {
    if (p === path) throw error;
    return { ok: true, data: {} };
  });
}

function calls(path: string) {
  return vi.mocked(apiPost).mock.calls.filter((c) => c[0] === path);
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

describe('<ControlPage/> — Battery Calibration', () => {
  const start = () => screen.getByRole('button', { name: /Start Calibration|Sending|Sent|Error/ });

  it('is hidden when the battery auto-calibrates', () => {
    connect({ supports_battery_calibration: false }, true);
    render(<ControlPage />);
    expect(screen.queryByText('Battery Calibration')).toBeNull();
  });

  it('is hidden outside developer mode even on a battery that supports it', () => {
    connect({ supports_battery_calibration: true }, false);
    render(<ControlPage />);
    expect(screen.queryByText('Battery Calibration')).toBeNull();
  });

  it('does nothing when the confirmation is declined', async () => {
    connect({ supports_battery_calibration: true }, true);
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(false);
    render(<ControlPage />);

    fireEvent.click(start());

    expect(confirmSpy).toHaveBeenCalledTimes(1);
    expect(confirmSpy.mock.calls[0][0]).toMatch(/BATTERY CALIBRATION/);
    expect(calls('/api/control/calibration')).toHaveLength(0);
    expect(start().textContent).toBe('Start Calibration');
  });

  it('starts stage 1 once the user confirms', async () => {
    connect({ supports_battery_calibration: true }, true);
    vi.spyOn(window, 'confirm').mockReturnValue(true);
    render(<ControlPage />);

    fireEvent.click(start());

    await waitFor(() => expect(start().textContent).toBe('✓ Sent'));
    expect(apiPost).toHaveBeenCalledWith('/api/control/calibration', { stage: 1 });
  });

  it('shows an error, not "Sent", when the request fails', async () => {
    connect({ supports_battery_calibration: true }, true);
    vi.spyOn(window, 'confirm').mockReturnValue(true);
    failOn('/api/control/calibration');
    render(<ControlPage />);

    fireEvent.click(start());

    await waitFor(() => expect(start().textContent).toBe('✗ Error'));
    expect(screen.queryByText('✓ Sent')).toBeNull();
  });

  it('cannot be started again while a calibration is running', () => {
    for (const stage of [1, 2, 3, 4, 5, 6]) {
      connect({ supports_battery_calibration: true, battery_calibration_stage: stage }, true);
      const { unmount } = render(<ControlPage />);
      expect(start()).toBeDisabled();
      unmount();
    }
  });

  it('can be started when off or finished', () => {
    for (const stage of [0, 7]) {
      connect({ supports_battery_calibration: true, battery_calibration_stage: stage }, true);
      const { unmount } = render(<ControlPage />);
      expect(start()).toBeEnabled();
      unmount();
    }
  });

  it('names the stage, including one it does not recognise', () => {
    const labels: Array<[number, string]> = [
      [0, 'Off'],
      [1, 'Discharging…'],
      [5, 'Balancing…'],
      [7, 'Finished'],
      [9, 'Unknown (9)'],
    ];
    for (const [stage, label] of labels) {
      connect({ supports_battery_calibration: true, battery_calibration_stage: stage }, true);
      const { unmount } = render(<ControlPage />);
      expect(screen.getByText(label)).toBeInTheDocument();
      unmount();
    }
  });
});

describe('<ControlPage/> — Reboot Inverter', () => {
  const reboot = () => screen.getByRole('button', { name: /Reboot Inverter|Sending|Sent|Error/ });

  it('does nothing when the confirmation is declined', () => {
    connect({}, true);
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(false);
    render(<ControlPage />);

    fireEvent.click(reboot());

    expect(confirmSpy.mock.calls[0][0]).toMatch(/REBOOT INVERTER/);
    expect(calls('/api/control/reboot')).toHaveLength(0);
    expect(reboot().textContent).toBe('Reboot Inverter');
  });

  it('reboots once the user confirms', async () => {
    connect({}, true);
    vi.spyOn(window, 'confirm').mockReturnValue(true);
    render(<ControlPage />);

    fireEvent.click(reboot());

    await waitFor(() => expect(reboot().textContent).toBe('✓ Sent'));
    expect(calls('/api/control/reboot')).toHaveLength(1);
  });

  it('shows an error, not "Sent", when the request fails', async () => {
    connect({}, true);
    vi.spyOn(window, 'confirm').mockReturnValue(true);
    failOn('/api/control/reboot');
    render(<ControlPage />);

    fireEvent.click(reboot());

    await waitFor(() => expect(reboot().textContent).toBe('✗ Error'));
  });

  it('is disabled while the request is in flight, so it cannot be sent twice', async () => {
    connect({}, true);
    vi.spyOn(window, 'confirm').mockReturnValue(true);
    let release!: () => void;
    vi.mocked(apiPost).mockImplementation(
      (p: string) =>
        p === '/api/control/reboot'
          ? new Promise((resolve) => {
              release = () => resolve({ ok: true, data: {} });
            })
          : Promise.resolve({ ok: true, data: {} }),
    );
    render(<ControlPage />);

    fireEvent.click(reboot());

    await waitFor(() => expect(reboot().textContent).toBe('Sending...'));
    expect(reboot()).toBeDisabled();
    fireEvent.click(reboot());
    expect(calls('/api/control/reboot')).toHaveLength(1);
    release();
    await waitFor(() => expect(reboot().textContent).toBe('✓ Sent'));
  });
});

describe('<ControlPage/> — battery mode toggles', () => {
  // While one change is applying its own button reads "Applying…", so match
  // each button by its idle label.
  const timedCharge = () => screen.getByRole('button', { name: /Timed Charge/ });

  it('shows the error and frees the Eco button when the request is rejected', async () => {
    connect();
    failOn('/api/control/eco', new Error('inverter refused'));
    render(<ControlPage />);

    fireEvent.click(screen.getByRole('button', { name: /^Eco/ }));

    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toBe('inverter refused');
    });
    // Not stuck on "Applying…": the control is usable again.
    expect(screen.queryByText('Applying…')).toBeNull();
    expect(screen.getByRole('button', { name: /^Eco/ })).toBeEnabled();
  });

  it('falls back to a generic message when the rejection is not an Error', async () => {
    connect();
    failOn('/api/control/eco', 'nope');
    render(<ControlPage />);
    fireEvent.click(screen.getByRole('button', { name: /^Eco/ }));
    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toBe('Eco toggle failed.');
    });
  });

  it('shows the error and frees the Timed Charge button when the request is rejected', async () => {
    connect();
    failOn('/api/control/timed-charge', new Error('slot invalid'));
    render(<ControlPage />);

    fireEvent.click(timedCharge());

    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toBe('slot invalid');
    });
    expect(screen.queryByText('Applying…')).toBeNull();
    expect(timedCharge()).toBeEnabled();
  });

  it('uses a generic message for a non-Error Timed Charge rejection', async () => {
    connect();
    failOn('/api/control/timed-charge', null);
    render(<ControlPage />);
    fireEvent.click(timedCharge());
    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toBe('Timed Charge toggle failed.');
    });
  });

  it('clears a previous error when the next toggle is attempted', async () => {
    connect();
    let attempts = 0;
    vi.mocked(apiPost).mockImplementation(async (p: string) => {
      if (p === '/api/control/eco' && attempts++ === 0) throw new Error('first try failed');
      return { ok: true, data: {} };
    });
    render(<ControlPage />);

    fireEvent.click(screen.getByRole('button', { name: /^Eco/ }));
    await screen.findByRole('alert');

    fireEvent.click(screen.getByRole('button', { name: /^Eco/ }));
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
  });

  it('ignores a second click while the first toggle is still being applied', async () => {
    connect();
    let release!: () => void;
    vi.mocked(apiPost).mockImplementation(
      (p: string) =>
        p === '/api/control/eco'
          ? new Promise((resolve) => {
              release = () => resolve({ ok: true, data: {} });
            })
          : Promise.resolve({ ok: true, data: {} }),
    );
    render(<ControlPage />);

    fireEvent.click(screen.getByRole('button', { name: /^Eco/ }));
    await screen.findByText('Applying…');
    // Every mode button is locked while one change is outstanding.
    expect(timedCharge()).toBeDisabled();
    fireEvent.click(timedCharge());

    expect(calls('/api/control/timed-charge')).toHaveLength(0);
    expect(calls('/api/control/eco')).toHaveLength(1);
    release();
  });
});

describe('<ControlPage/> — power limit saves', () => {
  function sliders(): HTMLInputElement[] {
    return screen.getAllByRole('slider') as HTMLInputElement[];
  }
  function saveFor(slider: HTMLElement): HTMLButtonElement {
    return slider.parentElement!.querySelector('button') as HTMLButtonElement;
  }

  it('saves the discharge power limit as the register value', async () => {
    connect();
    render(<ControlPage />);

    // Sliders: [force-duration, min-soc, charge, discharge, active-power].
    const discharge = sliders()[3];
    expect(discharge.value).toBe('100'); // snapshot 50 x display multiplier 2
    fireEvent.change(discharge, { target: { value: '30' } });
    fireEvent.click(saveFor(discharge));

    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/control/discharge-rate', { limit: 15 }),
    );
  });

  it('shows an error and reverts the discharge slider when the save fails', async () => {
    connect();
    failOn('/api/control/discharge-rate', new Error('register write rejected'));
    render(<ControlPage />);

    const discharge = sliders()[3];
    fireEvent.change(discharge, { target: { value: '30' } });
    fireEvent.click(saveFor(discharge));

    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toMatch(/Discharge power limit save failed/);
    });
    await waitFor(() => expect(discharge.value).toBe('100'));
  });

  it('saves the inverter active power limit', async () => {
    connect();
    render(<ControlPage />);

    const active = sliders()[4];
    expect(active.value).toBe('100');
    fireEvent.change(active, { target: { value: '60' } });
    fireEvent.click(saveFor(active));

    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/control/active-power-rate', { rate: 60 }),
    );
  });

  it('shows an error and reverts the active power slider when the save fails', async () => {
    connect();
    failOn('/api/control/active-power-rate', new Error('busy'));
    render(<ControlPage />);

    const active = sliders()[4];
    fireEvent.change(active, { target: { value: '60' } });
    fireEvent.click(saveFor(active));

    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toMatch(/Active power rate save failed/);
      expect(screen.getByRole('alert').textContent).toMatch(/busy/);
    });
    await waitFor(() => expect(active.value).toBe('100'));
  });
});
