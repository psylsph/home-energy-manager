import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, act } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Issue #346: the bracketed kW figure beside the Battery Charge / Battery
// Discharge Power Limit sliders was `min(display / 200 * battery_capacity_w,
// max_battery_power_w)`. On any pack above roughly half the inverter's rating
// the clamp swallowed the answer, so the figure read the inverter's maximum for
// the whole top of the slider. A reporter with a 2600 W inverter set 66% and
// was shown "66% (2.6 kW)" — the maximum — instead of 1.7 kW, and the History
// page appeared to confirm it.
//
// The limit registers are a percentage in both families once the DC-hybrid
// 0-50 register is doubled for display, so the figure must be
// percent / 100 * the inverter's maximum battery power.
//
// Also covered here: the Gateway uses the direct 1-100% AC-limit bank
// (HR 313/314, as GivTCP writes it), the half-scale sliders move in 2% steps,
// and the Inverter Active Power Limit is not offered on the 1000-range-layout
// families, where HEM reads the value back from HR 1002 but writes HR 50.
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
    battery_capacity_kwh: 8.2,
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
    battery_power_mode: 1,
    battery_reserve: 20,
    charge_rate: 33,
    discharge_rate: 50,
    active_power_rate: 100,
    max_battery_power_w: 2600,
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
    charge_slots: [
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
    ],
    discharge_slots: [
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
      { enabled: false, start_hour: 0, start_minute: 0, end_hour: 0, end_minute: 0, target_soc: 100 },
    ],
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
    battery_pause_slot: {
      enabled: false,
      start_hour: 0,
      start_minute: 0,
      end_hour: 0,
      end_minute: 0,
      target_soc: 100,
    },
    ...overrides,
  };
}

function renderWith(snapshot: InverterSnapshot) {
  useInverterStore.setState({
    snapshot,
    developerMode: false,
    connectionState: 'connected',
    connectedHost: '192.168.1.36:8899',
  });
  return render(<ControlPage />);
}

/** Section 6 sliders, in DOM order: [force-duration, min-soc, charge, discharge, active-power]. */
function powerSliders(): HTMLInputElement[] {
  return screen.getAllByRole('slider') as HTMLInputElement[];
}

/**
 * The percentage + bracketed kW readout that shares a header row with the
 * given slider. Each control is a `space-y-1` row whose header flex holds the
 * caption and the monospaced value.
 */
function readoutFor(slider: HTMLElement): string {
  const row = slider.closest('.space-y-1');
  const header = row?.querySelector('.justify-between');
  const value = header?.children[1];
  return value?.textContent ?? '';
}

/** The caption rendered beside the given slider. */
function captionFor(slider: HTMLElement): string {
  const row = slider.closest('.space-y-1');
  const header = row?.querySelector('.justify-between');
  return header?.children[0]?.textContent ?? '';
}

const CHARGE = 2;
const DISCHARGE = 3;

describe('<ControlPage/> — power-limit kW readouts (issue #346)', () => {
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
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    cleanup();
    useInverterStore.setState({ snapshot: null, connectionState: 'disconnected' });
  });

  it('shows the charge limit as a percentage of the inverter maximum, not the maximum itself', () => {
    // The reported case: 8.2 kWh pack behind a 2600 W inverter, HR111 = 33.
    // 66% of 2600 W is 1716 W, so the readout must say 1.7 kW. It read
    // "2.6 kW" because the capacity-derived figure (2706 W) was clamped.
    renderWith(makeSnapshot());
    expect(readoutFor(powerSliders()[CHARGE])).toBe('66% (1.7 kW)');
  });

  it('still reports 100% as the full inverter maximum', () => {
    renderWith(makeSnapshot());
    expect(readoutFor(powerSliders()[DISCHARGE])).toBe('100% (2.6 kW)');
  });

  it('keeps the charge readout tracking the slider across its whole range', () => {
    renderWith(makeSnapshot());
    const slider = powerSliders()[CHARGE];

    const readouts: string[] = [];
    for (const position of [0, 10, 24, 40, 66, 86, 100]) {
      fireEvent.change(slider, { target: { value: String(position) } });
      readouts.push(readoutFor(slider));
    }

    expect(readouts).toEqual([
      '0%',
      '10% (0.3 kW)',
      '24% (0.6 kW)',
      '40% (1.0 kW)',
      '66% (1.7 kW)',
      '86% (2.2 kW)',
      '100% (2.6 kW)',
    ]);
    // Every step must be distinct — the bug was a frozen readout.
    expect(new Set(readouts).size).toBe(readouts.length);
  });

  it('applies the same percentage-of-maximum conversion on AC-coupled models', () => {
    // 0x30xx stores the limit as a direct 1-100%, so 66% of a 3000 W ceiling
    // is 1980 W with no capacity basis and no clamp.
    renderWith(makeSnapshot({ device_type_code: '3001', charge_rate: 66, max_battery_power_w: 3000 }));
    const slider = powerSliders()[CHARGE];
    expect(readoutFor(slider)).toBe('66% (2.0 kW)');
    expect(captionFor(slider)).toBe('AC Charge Power Limit');
  });

  it('treats the Gateway limit register as a direct 1-100%, like GivTCP writes it', () => {
    // GivTCP routes the Gateway's charge rate through
    // set_battery_charge_limit_ac (HR 313/314, 1-100%) and reads it back from
    // there, so the register is neither doubled nor halved.
    renderWith(makeSnapshot({ device_type_code: '7001', charge_rate: 66, max_battery_power_w: 0 }));
    const slider = powerSliders()[CHARGE];

    expect(slider.getAttribute('min')).toBe('1');
    expect(slider.getAttribute('step')).toBe('1');
    expect(readoutFor(slider)).toBe('66%');

    fireEvent.change(slider, { target: { value: '80' } });
    fireEvent.click(slider.parentElement?.querySelector('button') as HTMLButtonElement);
    expect(apiPost).toHaveBeenCalledWith('/api/control/charge-rate', { limit: 80 });
  });

  it('moves the half-scale DC-hybrid sliders in 2% steps', () => {
    // HR 111/112 hold 0-50, so an odd percentage can never be read back: a
    // 33% draft would save 17, read back as 34%, and the slider would sit on
    // the stale 33% draft forever.
    renderWith(makeSnapshot({ device_type_code: '2001' }));
    expect(powerSliders()[CHARGE].getAttribute('step')).toBe('2');
    expect(powerSliders()[DISCHARGE].getAttribute('step')).toBe('2');
  });

  it('keeps 1% steps on the direct-percentage registers', () => {
    renderWith(makeSnapshot({ device_type_code: '3001' }));
    expect(powerSliders()[CHARGE].getAttribute('step')).toBe('1');
    expect(powerSliders()[DISCHARGE].getAttribute('step')).toBe('1');
  });

  it('omits the watt figure when the inverter maximum is unknown', () => {
    // Gateway / EMS / PV-inverter report max_battery_power_w = 0, meaning
    // "rating unknown". "(0.0 kW)" would read as "no limit".
    renderWith(makeSnapshot({ device_type_code: '2301', max_battery_power_w: 0 }));
    expect(readoutFor(powerSliders()[CHARGE])).toBe('66%');
  });

  it('formats the Inverter Active Power Limit like the other two rows', () => {
    renderWith(makeSnapshot());
    expect(readoutFor(powerSliders()[4])).toBe('100% (5.0 kW)');
  });

  it('hides the Active Power Limit on unsupported commercial device codes', () => {
    renderWith(makeSnapshot({ device_type_code: '4101' }));
    expect(screen.queryByText('Inverter Active Power Limit')).toBeNull();
  });

  it('hides the Active Power Limit where HEM reads it back from HR 1002', () => {
    // decode_holding_1000_1079 overwrites active_power_rate with HR 1002 on
    // these families, but set_active_power_rate always writes HR 50 — so the
    // slider wrote a register the layout ignores and read the old value back.
    renderWith(makeSnapshot({ device_type_code: '4001' }));
    expect(screen.queryByText('Inverter Active Power Limit')).toBeNull();
    expect(powerSliders()).toHaveLength(4);
  });
});

/**
 * Issue #346: the same readout is asserted for every device family, because
 * each one reaches a different register pair and scale. The expectations here
 * are the frontend half of the contract that the Rust
 * `every_device_type_decodes_the_charge_limit_from_the_register_it_writes`
 * test asserts against the poll blocks.
 */
/**
 * Issue #346 was reported on a Gen1 Hybrid (DTC 0x1001, 2600 W, HR 111/112 on
 * the 0-50 scale). These tests drive the full loop that user goes through —
 * read the register, move the slider, save, read the register back — across
 * the whole slider range and with a pack large enough that the old
 * capacity-derived figure was clamped to the inverter maximum.
 */
describe('<ControlPage/> — Gen1 Hybrid power-limit round trip (issue #346)', () => {
  const gen1 = (overrides: Partial<InverterSnapshot> = {}) => makeSnapshot({
    device_type: 'gen1',
    device_type_display: 'Gen1',
    device_type_code: '1001',
    max_battery_power_w: 2600,
    battery_capacity_kwh: 13.5,
    charge_rate: 33,
    discharge_rate: 25,
    ...overrides,
  });

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
    useInverterStore.setState({ snapshot: null, connectionState: 'disconnected' });
  });

  it('renders the half-scale slider shape: 0-100 in 2% steps', () => {
    renderWith(gen1());
    for (const index of [CHARGE, DISCHARGE]) {
      const slider = powerSliders()[index];
      expect(slider.getAttribute('min')).toBe('0');
      expect(slider.getAttribute('max')).toBe('100');
      expect(slider.getAttribute('step')).toBe('2');
    }
    expect(captionFor(powerSliders()[CHARGE])).toBe('Battery Charge Power Limit');
    expect(captionFor(powerSliders()[DISCHARGE])).toBe('Battery Discharge Power Limit');
  });

  it('doubles HR 111/112 and shows a share of 2.6 kW, not the clamped maximum', () => {
    // 13.5 kWh / 2 = 6.75 kW, so the old capacity formula clamped to 2.6 kW
    // for every position from 39% up.
    renderWith(gen1());
    expect(readoutFor(powerSliders()[CHARGE])).toBe('66% (1.7 kW)');
    expect(readoutFor(powerSliders()[DISCHARGE])).toBe('50% (1.3 kW)');
  });

  it('gives every slider position its own readout and never saturates', () => {
    renderWith(gen1());
    const slider = powerSliders()[CHARGE];
    const readouts: string[] = [];
    for (let percent = 2; percent <= 100; percent += 2) {
      fireEvent.change(slider, { target: { value: String(percent) } });
      const kw = ((percent / 100) * 2600 / 1000).toFixed(1);
      expect(readoutFor(slider)).toBe(`${percent}% (${kw} kW)`);
      readouts.push(readoutFor(slider));
    }
    expect(new Set(readouts).size).toBe(readouts.length);
    expect(readouts.filter((r) => r.includes('(2.6 kW)'))).toEqual(['100% (2.6 kW)']);
  });

  it.each([0, 2, 34, 50, 66, 98, 100])(
    'saves %i%% as half that on HR 111 and HR 112',
    async (percent) => {
      renderWith(gen1());
      for (const [index, path] of [
        [CHARGE, '/api/control/charge-rate'],
        [DISCHARGE, '/api/control/discharge-rate'],
      ] as const) {
        const slider = powerSliders()[index];
        fireEvent.change(slider, { target: { value: String(percent) } });
        fireEvent.click(slider.parentElement?.querySelector('button') as HTMLButtonElement);
        expect(apiPost).toHaveBeenCalledWith(path, { limit: percent / 2 });
      }
      // Never send a value HR 111/112 cannot hold.
      for (const [, body] of vi.mocked(apiPost).mock.calls) {
        expect((body as { limit: number }).limit).toBeLessThanOrEqual(50);
      }
    },
  );

  it('settles on the read-back value after a save and follows later external changes', async () => {
    renderWith(gen1());
    const slider = powerSliders()[CHARGE];

    fireEvent.change(slider, { target: { value: '80' } });
    fireEvent.click(slider.parentElement?.querySelector('button') as HTMLButtonElement);
    expect(apiPost).toHaveBeenCalledWith('/api/control/charge-rate', { limit: 40 });

    // Until the read-back arrives the slider keeps the saved position.
    act(() => useInverterStore.setState({ snapshot: gen1() }));
    expect(readoutFor(powerSliders()[CHARGE])).toBe('80% (2.1 kW)');

    // The inverter reads back 40 → 80%: the draft must be considered applied.
    act(() => useInverterStore.setState({ snapshot: gen1({ charge_rate: 40 }) }));
    expect(readoutFor(powerSliders()[CHARGE])).toBe('80% (2.1 kW)');

    // A later change from the GivEnergy app must show through, which the
    // stuck-draft bug on odd percentages prevented.
    act(() => useInverterStore.setState({ snapshot: gen1({ charge_rate: 10 }) }));
    expect(readoutFor(powerSliders()[CHARGE])).toBe('20% (0.5 kW)');
  });

  it('offers the Inverter Active Power Limit on HR 50', () => {
    renderWith(gen1());
    expect(screen.queryByText('Inverter Active Power Limit')).not.toBeNull();
  });
});

describe('<ControlPage/> — charge-limit readout across every device family', () => {
  /** Locate a limit slider through its caption, independent of DOM order. */
  function sliderForCaption(caption: string): HTMLInputElement {
    const captionEl = screen.getByText(caption);
    const row = captionEl.closest('.space-y-1');
    return row?.querySelector('input[type="range"]') as HTMLInputElement;
  }

  const CASES: Array<{
    code: string;
    family: string;
    caption: string;
    maxBatteryPowerW: number;
    chargeRegister: number;
    expected: string;
    showsActivePower: boolean;
  }> = [
    { code: '1001', family: 'Gen1 hybrid', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 2600, chargeRegister: 25, expected: '50% (1.3 kW)', showsActivePower: true },
    { code: '2001', family: 'Gen2/3 hybrid', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 3600, chargeRegister: 25, expected: '50% (1.8 kW)', showsActivePower: true },
    { code: '2101', family: 'Polar hybrid', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 2600, chargeRegister: 25, expected: '50% (1.3 kW)', showsActivePower: true },
    { code: '2201', family: 'Gen3 Plus hybrid', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 5400, chargeRegister: 25, expected: '50% (2.7 kW)', showsActivePower: true },
    { code: '2301', family: 'PV inverter', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 0, chargeRegister: 25, expected: '50%', showsActivePower: true },
    { code: '3001', family: 'AC-coupled', caption: 'AC Charge Power Limit', maxBatteryPowerW: 3000, chargeRegister: 66, expected: '66% (2.0 kW)', showsActivePower: true },
    { code: '3002', family: 'AC-coupled Mk2', caption: 'AC Charge Power Limit', maxBatteryPowerW: 3000, chargeRegister: 66, expected: '66% (2.0 kW)', showsActivePower: true },
    { code: '4001', family: 'Three-phase', caption: 'Three-phase Charge Power Limit', maxBatteryPowerW: 6000, chargeRegister: 66, expected: '66% (4.0 kW)', showsActivePower: false },
    { code: '5001', family: 'EMS', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 0, chargeRegister: 25, expected: '50%', showsActivePower: true },
    { code: '6001', family: 'AC three-phase', caption: 'Three-phase Charge Power Limit', maxBatteryPowerW: 6000, chargeRegister: 66, expected: '66% (4.0 kW)', showsActivePower: false },
    { code: '7001', family: 'Gateway', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 0, chargeRegister: 66, expected: '66%', showsActivePower: true },
    { code: '8001', family: 'AIO 6kW', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 6000, chargeRegister: 25, expected: '50% (3.0 kW)', showsActivePower: true },
    { code: '8002', family: 'AIO 3.6kW', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 3600, chargeRegister: 25, expected: '50% (1.8 kW)', showsActivePower: true },
    { code: '8003', family: 'AIO 5kW', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 5000, chargeRegister: 25, expected: '50% (2.5 kW)', showsActivePower: true },
    { code: '8101', family: 'Hybrid HV Gen3', caption: 'Three-phase Charge Power Limit', maxBatteryPowerW: 6000, chargeRegister: 66, expected: '66% (4.0 kW)', showsActivePower: false },
    { code: '8201', family: 'AIO Hybrid', caption: 'Three-phase Charge Power Limit', maxBatteryPowerW: 6000, chargeRegister: 66, expected: '66% (4.0 kW)', showsActivePower: false },
    { code: '8301', family: 'Gen4 hybrid', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 6000, chargeRegister: 25, expected: '50% (3.0 kW)', showsActivePower: true },
  ];

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
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    cleanup();
    useInverterStore.setState({ snapshot: null, connectionState: 'disconnected' });
  });

  it.each(CASES)('$code ($family)', ({ code, caption, maxBatteryPowerW, chargeRegister, expected, showsActivePower }) => {
    renderWith(makeSnapshot({
      device_type_code: code,
      max_battery_power_w: maxBatteryPowerW,
      charge_rate: chargeRegister,
    }));

    const slider = sliderForCaption(caption);
    const header = slider.closest('.space-y-1')?.querySelector('.justify-between');
    expect(header?.children[1]?.textContent).toBe(expected);

    if (showsActivePower) {
      expect(screen.queryByText('Inverter Active Power Limit')).not.toBeNull();
    } else {
      expect(screen.queryByText('Inverter Active Power Limit')).toBeNull();
    }
  });
});
