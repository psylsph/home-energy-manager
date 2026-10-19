import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, within } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Issue #362: the reporter forgot the Battery Charge Power Limit and the
// battery under-charged overnight. Each enabled charge slot now states the limit
// the inverter holds and warns when that limit cannot deliver the slot's target.
// Read-only: nothing here writes a limit.
//
// Fixed clock: the inverter clock is set in the snapshot, so whether a slot is
// running never depends on the time zone the tests run in.
// ---------------------------------------------------------------------------

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/adaptive-charge') {
      return { ok: true, data: { config: { periods: [], confirmation_readings: 2 } } };
    }
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

const NOON = '2026-10-11 12:00:00';
const OVERNIGHT = { start_hour: 0, start_minute: 0, end_hour: 6, end_minute: 0, enabled: true, target_soc: 100 };

function withSlot(overrides: Partial<InverterSnapshot> = {}, slot = OVERNIGHT) {
  return makeSnapshot({
    enable_charge: true,
    inverter_time: NOON,
    charge_slots: [slot],
    ...overrides,
  } as Partial<InverterSnapshot>);
}

describe('Control page: charge limit on the charge slot card', () => {
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
    vi.mocked(apiPost).mockResolvedValue({ ok: true });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    cleanup();
    useInverterStore.setState({ snapshot: null, connectionState: 'disconnected' });
  });

  async function chargeSection() {
    const heading = await screen.findByRole('heading', { name: 'Charge Schedule' });
    return within(heading.closest('section') as HTMLElement);
  }

  it('states the limit the inverter holds on an enabled slot', async () => {
    renderWith(withSlot());
    const section = await chargeSection();
    expect(section.getByText('Charge limit: 62% (1.6 kW), set under Battery and Power Controls.')).toBeTruthy();
    expect(section.queryByTestId('charge-slot-limit-shortfall')).toBeNull();
  });

  it('warns when the limit cannot reach the slot target', async () => {
    // Register 5 is 18% (0.5 kW): 6 hours adds about 2.8 kWh of a 9.5 kWh pack.
    renderWith(withSlot({ charge_rate: 5 }));
    const section = await chargeSection();
    const warning = section.getByTestId('charge-slot-limit-shortfall');
    expect(warning.textContent).toBe(
      'At this limit the slot can add about 2.8 kWh (30% of the battery). '
      + 'From the current 50% it would reach about 80%, short of the 100% target.',
    );
  });

  it('follows an unsaved change to the slot target', async () => {
    renderWith(withSlot({ charge_rate: 5 }));
    const section = await chargeSection();
    expect(section.queryByTestId('charge-slot-limit-shortfall')).toBeTruthy();
    // The first slider in the section is slot 1's Target SOC.
    fireEvent.change(section.getAllByRole('slider')[0], { target: { value: '70' } });
    expect(section.queryByTestId('charge-slot-limit-shortfall')).toBeNull();
    expect(apiPost).not.toHaveBeenCalled();
  });

  it('does not warn about one slot when the enabled slots together reach the target', async () => {
    // Register 5 is 18% (0.5 kW). Slot 1 alone adds ~30% of the pack, short of
    // a 70% target from 50%; with slot 2 (12 h in all) the schedule adds ~59%...
    renderWith(withSlot({ max_charge_slots: 2, charge_rate: 5, charge_slots: [
      { ...OVERNIGHT, end_hour: 6, target_soc: 70 },
      { start_hour: 13, start_minute: 0, end_hour: 19, end_minute: 0, enabled: true, target_soc: 70 },
    ] } as Partial<InverterSnapshot>));
    const section = await chargeSection();
    // ...so neither card warns.
    expect(section.queryAllByTestId('charge-slot-limit')).toHaveLength(2);
    expect(section.queryByTestId('charge-slot-limit-shortfall')).toBeNull();
  });

  it('does not warn while the charge schedule is switched off', async () => {
    renderWith(withSlot({ charge_rate: 5, enable_charge: false }));
    const section = await chargeSection();
    expect(section.getByTestId('charge-slot-limit')).toBeTruthy();
    expect(section.queryByTestId('charge-slot-limit-shortfall')).toBeNull();
  });

  it('warns when the limit is zero', async () => {
    renderWith(withSlot({ charge_rate: 0 }));
    const section = await chargeSection();
    expect(section.getByTestId('charge-slot-limit-shortfall').textContent)
      .toMatch(/^At this limit the slot can add about 0\.0 kWh/);
  });

  it('shows nothing on a slot that is switched off', async () => {
    renderWith(withSlot({}, { ...OVERNIGHT, enabled: false }));
    const section = await chargeSection();
    expect(section.queryByTestId('charge-slot-limit')).toBeNull();
  });

  it('counts only the time left once the slot is running', async () => {
    // 05:00 with the slot ending at 06:00: an hour at 0.5 kW adds under 0.5 kWh.
    renderWith(withSlot({ charge_rate: 5, inverter_time: '2026-10-11 05:00:00' }));
    const section = await chargeSection();
    expect(section.getByTestId('charge-slot-limit-shortfall').textContent)
      .toMatch(/^In the time left this slot can add about 0\.5 kWh/);
  });

  it('leaves the warning off while Adaptive Charge owns the limit', async () => {
    renderWith(withSlot({ charge_rate: 5, adaptive_charge_enabled: true } as Partial<InverterSnapshot>));
    const section = await chargeSection();
    expect(section.getByText('Charge limit: 18% (0.5 kW). Adaptive Charge adjusts this.')).toBeTruthy();
    expect(section.queryByTestId('charge-slot-limit-shortfall')).toBeNull();
  });

  it('leaves the warning off slot 1 while the Forecast plan owns it', async () => {
    renderWith(withSlot({ charge_rate: 5, forecast_plan_auto_apply_enabled: true } as Partial<InverterSnapshot>));
    const section = await chargeSection();
    expect(section.getByText('Charge limit: 18% (0.5 kW). The Forecast plan adjusts this.')).toBeTruthy();
    expect(section.queryByTestId('charge-slot-limit-shortfall')).toBeNull();
  });

  it('leaves the warning off every slot while the Forecast plan owns the limit', async () => {
    // The planner resets the one global charge rate, so slot 2 is no different.
    renderWith(withSlot({
      max_charge_slots: 2,
      charge_rate: 5,
      forecast_plan_auto_refresh: true,
      charge_slots: [
        { ...OVERNIGHT, end_hour: 1 },
        { start_hour: 13, start_minute: 0, end_hour: 14, end_minute: 0, enabled: true, target_soc: 100 },
      ],
    } as Partial<InverterSnapshot>));
    const section = await chargeSection();
    expect(section.getAllByText(/The Forecast plan adjusts this/)).toHaveLength(2);
    expect(section.queryByTestId('charge-slot-limit-shortfall')).toBeNull();
  });

  it('does not treat a zero reading on a direct-register inverter as a limit', async () => {
    // AC-coupled limits run 1-100; a 0 there means the config block has not
    // been read yet, not "no charging".
    renderWith(withSlot({ device_type_code: '3001', charge_rate: 0 } as Partial<InverterSnapshot>));
    const section = await chargeSection();
    expect(section.queryByTestId('charge-slot-limit')).toBeNull();
  });

  it('still gives the limit when the inverter rating is unknown, without a watt figure', async () => {
    renderWith(withSlot({ max_battery_power_w: 0 } as Partial<InverterSnapshot>));
    const section = await chargeSection();
    expect(section.queryByTestId('charge-slot-limit-shortfall')).toBeNull();
    expect(section.getByTestId('charge-slot-limit').textContent).toMatch(/^Charge limit: \d+%, set under/);
  });
});
