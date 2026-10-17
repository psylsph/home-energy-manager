import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, act } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Issue #362 follow-up: the reporter forgot the Battery Charge Power Limit and
// the battery under-charged overnight. The Control page now shows a read-only
// "Inverter Charge Rate" line beside the charge limit. It is fed only from the
// snapshot, so dragging the slider without saving does not change it.
//
// Fixed clock: the snapshot timestamp and the page's notion of "now" are both
// pinned with fake timers, so the stale state is deterministic.
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
import type { InverterSnapshot } from '../../src/lib/types';
import { apiPost } from '../../src/lib/api';

const NOW_MS = Date.UTC(2026, 9, 11, 12, 0, 0);

function makeSnapshot(overrides: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    timestamp: Math.floor(NOW_MS / 1000),
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
    battery_reserve: 4,
    charge_rate: 17,
    discharge_rate: 17,
    active_power_rate: 100,
    enable_charge: false,
    enable_discharge: false,
    battery_power_mode: 1,
    charge_slots: [],
    discharge_slots: [],
    device_type_code: '2001',
    max_battery_power_w: 2600,
    max_ac_power_w: 2600,
    max_charge_slots: 1,
    max_discharge_slots: 1,
    ...overrides,
  } as unknown as InverterSnapshot;
}

function renderWith(snapshot: InverterSnapshot, receivedAtMs: number | null = NOW_MS) {
  useInverterStore.setState({
    snapshot,
    snapshotReceivedAt: receivedAtMs,
    developerMode: false,
    connectionState: 'connected',
    connectedHost: '192.168.1.36:8899',
  });
  return render(<ControlPage />);
}

describe('Control page: inverter-reported charge limit', () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ['Date', 'setInterval', 'clearInterval'] });
    vi.setSystemTime(NOW_MS);
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
    vi.mocked(apiPost).mockResolvedValue({ ok: true });
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    cleanup();
    useInverterStore.setState({ snapshot: null, connectionState: 'disconnected' });
  });

  it('shows the register the inverter last reported beside the charge limit', () => {
    // Register 17 on a 9.5 kWh pack behind a 2.6 kW inverter = 62% (1.6 kW).
    renderWith(makeSnapshot());
    // Once, beside the limit slider. Charge slot cards no longer repeat it.
    expect(screen.getAllByText('Inverter Charge Rate: 62% (1.6 kW)')).toHaveLength(1);
  });

  it('does not repeat the reported charge limit on each charge slot card', () => {
    // The reporter set a slot and forgot the limit, so the slot card itself
    // must say what the inverter is limited to.
    renderWith(makeSnapshot({
      enable_charge: true,
      charge_slots: [{ start_hour: 0, start_minute: 0, end_hour: 6, end_minute: 0, enabled: true, target_soc: 100 }],
    } as Partial<InverterSnapshot>));
    expect(screen.getAllByText('Inverter Charge Rate: 62% (1.6 kW)')).toHaveLength(1);
  });

  it('is not thrown off by a host clock that runs behind the viewer', () => {
    // The host's own timestamp is ten minutes old, but the snapshot arrived a
    // second ago on this machine, so the reading is current.
    renderWith(makeSnapshot({ timestamp: Math.floor(NOW_MS / 1000) - 600 }), NOW_MS - 1_000);
    expect(screen.getAllByText('Inverter Charge Rate: 62% (1.6 kW)')).toHaveLength(1);
    expect(screen.queryByText(/as last read/)).toBeNull();
  });

  it('turns stale while the page stays open, without a new snapshot', async () => {
    renderWith(makeSnapshot(), NOW_MS);
    expect(screen.getByText('Inverter Charge Rate: 62% (1.6 kW)')).toBeTruthy();
    // Advancing the faked timers also moves Date on, so one call is enough.
    // The clock ticks every 5 s, so 95 s is the first tick past 90 s, before
    // the 120 s connection watchdog would clear the reading.
    await act(async () => {
      vi.advanceTimersByTime(90_000);
    });
    expect(screen.queryByText(/as last read/)).toBeNull();
    await act(async () => {
      vi.advanceTimersByTime(5_000);
    });
    expect(screen.getByText(/as last read 95s ago/)).toBeTruthy();
  });

  it('stays live through a whole default poll cycle and its reads', async () => {
    // A default install polls every 60 s, plus the time the reads take. The
    // reading must not flip to "as last read" just before each new one lands.
    renderWith(makeSnapshot(), NOW_MS);
    await act(async () => {
      vi.advanceTimersByTime(85_000);
    });
    expect(screen.getByText('Inverter Charge Rate: 62% (1.6 kW)')).toBeTruthy();
    expect(screen.queryByText(/as last read/)).toBeNull();
  });

  it('does not show a replayed reading as current while the inverter is offline', () => {
    // A reloaded page can receive the last snapshot the server cached before
    // the inverter dropped. That is not a reading from now.
    renderWith(makeSnapshot());
    act(() => useInverterStore.setState({ connectionState: 'disconnected' }));
    // The page swaps its controls for a waiting placeholder while offline, so
    // the cached 62% is not rendered as a live value anywhere.
    expect(screen.queryByText(/Inverter Charge Rate/)).toBeNull();
    expect(screen.queryByText(/62% \(1\.6 kW\)/)).toBeNull();
  });

  it('runs the readout clock only while there is a live reading to age', () => {
    const intervals = vi.spyOn(globalThis, 'setInterval');
    const fiveSecondTicks = () => intervals.mock.calls.filter(([, ms]) => ms === 5000).length;

    renderWith(makeSnapshot(), null);
    expect(fiveSecondTicks()).toBe(0);
    cleanup();
    intervals.mockClear();

    renderWith(makeSnapshot(), NOW_MS);
    expect(fiveSecondTicks()).toBe(1);
  });

  it('keeps showing the reported value while the slider is dragged but not saved', () => {
    renderWith(makeSnapshot());
    const slider = screen.getAllByRole('slider')[0];
    fireEvent.change(slider, { target: { value: '30' } });

    // The slider label follows the draft, the reported line does not.
    expect(screen.getAllByText('Inverter Charge Rate: 62% (1.6 kW)')).toHaveLength(1);
    expect(apiPost).not.toHaveBeenCalled();
  });

  it('marks the reported value as last read when the snapshot is older than a minute', () => {
    renderWith(makeSnapshot(), NOW_MS - 300_000);
    expect(screen.getAllByText(/Inverter Charge Rate: 62% \(1\.6 kW\) · as last read 5 min ago/)).toHaveLength(1);
  });

  it('says unknown rather than guessing when the snapshot cannot be dated', async () => {
    renderWith(makeSnapshot(), null);
    await act(async () => {});
    expect(screen.getAllByText('Inverter Charge Rate: Unknown')).toHaveLength(1);
  });
});
