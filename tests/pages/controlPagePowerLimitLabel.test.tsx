import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, act } from '@testing-library/react';

// ---------------------------------------------------------------------------
// Issue #346: a Gen1 Hybrid user (9.5 kWh behind a 2.6 kW inverter) set the
// Battery Charge Power Limit to 62% and the battery kept charging at the
// inverter's maximum. The slider means "% of the inverter's maximum", but the
// DC-hybrid HR 111/112 register holds a percentage of battery CAPACITY (GivTCP
// `write.py`: target = watts / (capacity / 2) * 50; `read.py`: watts =
// min(reg / 100 * capacity_w, inverter_max)). Writing 62% / 2 = 31 asked for
// 2945 W, above what the inverter can deliver, so it limited nothing, while the
// label (which had been "fixed" to percent-of-maximum) claimed 1.6 kW.
//
// So these tests pin both halves: what the page SENDS for a slider position and
// what it SHOWS for a register value, with expectations taken from the GivTCP
// formulas rather than from this app's own arithmetic.
//
// Also covered: the Gateway uses the direct 1-100% AC-limit bank (HR 313/314,
// as GivTCP writes it), and the Inverter Active Power Limit is not offered on
// the 1000-range-layout families, where HEM reads the value back from HR 1002
// but writes HR 50.
// ---------------------------------------------------------------------------

/** GivTCP `write.py` for a DC hybrid: the register that requests `watts`. */
function givTcpRegisterFor(watts: number, capacityWh: number): number {
  return Math.min((watts / (capacityWh / 2)) * 50, 50);
}

/** GivTCP `read.py` for a DC hybrid: the watts a register actually allows. */
function givTcpWattsFor(register: number, capacityWh: number, inverterMaxW: number): number {
  return Math.min((register / 100) * capacityWh, inverterMaxW);
}

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

  it('shows the reporter\'s register as a share of the inverter maximum, derived through the pack size', () => {
    // Gen1 Hybrid, 9.5 kWh, 2600 W. Register 17 = 17% of 9500 Wh = 1615 W
    // (GivTCP read.py) = 62% of the inverter's 2600 W.
    renderWith(makeSnapshot({ battery_capacity_kwh: 9.5, charge_rate: 17, discharge_rate: 17 }));
    expect(givTcpWattsFor(17, 9500, 2600)).toBeCloseTo(1615, 6);
    expect(readoutFor(powerSliders()[CHARGE])).toBe('62% (1.6 kW)');
    expect(readoutFor(powerSliders()[DISCHARGE])).toBe('62% (1.6 kW)');
  });

  it('reads the register the old version wrote (31) as full power, which is what it did', () => {
    // Register 31 is 2945 W on a 9.5 kWh pack: above the 2600 W inverter, so
    // the inverter was never limited. The honest reading is 100%, not 62%.
    expect(givTcpWattsFor(31, 9500, 2600)).toBe(2600);
    renderWith(makeSnapshot({ battery_capacity_kwh: 9.5, charge_rate: 31, discharge_rate: 50 }));
    expect(readoutFor(powerSliders()[CHARGE])).toBe('100% (2.6 kW)');
    expect(readoutFor(powerSliders()[DISCHARGE])).toBe('100% (2.6 kW)');
  });

  it('sends the register that delivers the chosen share of the inverter maximum', () => {
    // 62% of 2600 W = 1612 W. GivTCP's formula gives 16.97, i.e. register 17 -
    // not the 31 that "62% / 2" wrote.
    vi.mocked(apiPost).mockResolvedValue({ ok: true, data: {} });
    renderWith(makeSnapshot({ battery_capacity_kwh: 9.5, charge_rate: 50 }));
    const slider = powerSliders()[CHARGE];
    fireEvent.change(slider, { target: { value: '62' } });
    fireEvent.click(slider.parentElement?.querySelector('button') as HTMLButtonElement);
    expect(givTcpRegisterFor(0.62 * 2600, 9500)).toBeCloseTo(16.97, 2);
    expect(apiPost).toHaveBeenCalledWith('/api/control/charge-rate', { limit: 17 });
  });

  it('keeps the charge readout tracking the slider across its whole range', () => {
    renderWith(makeSnapshot({ battery_capacity_kwh: 9.5, charge_rate: 17 }));
    const slider = powerSliders()[CHARGE];

    // The register holds whole units of 1% of capacity (95 W), so each slider
    // position shows the percentage the inverter will really hold.
    const readouts: string[] = [];
    for (const position of [0, 10, 24, 40, 62, 86, 100]) {
      fireEvent.change(slider, { target: { value: String(position) } });
      readouts.push(readoutFor(slider));
    }

    expect(readouts).toEqual([
      '0%',
      '11% (0.3 kW)',
      '26% (0.7 kW)',
      '40% (1.0 kW)',
      '62% (1.6 kW)',
      '88% (2.3 kW)',
      '100% (2.6 kW)',
    ]);
    expect(new Set(readouts).size).toBe(readouts.length);
  });

  it('shows what a save will really do when the slider sits between two registers', () => {
    // 63% and 64% straddle registers 17 (62%) and 18 (66%).
    renderWith(makeSnapshot({ battery_capacity_kwh: 9.5, charge_rate: 17 }));
    const slider = powerSliders()[CHARGE];
    fireEvent.change(slider, { target: { value: '63' } });
    expect(readoutFor(slider)).toBe('62% (1.6 kW)');
    fireEvent.change(slider, { target: { value: '64' } });
    expect(readoutFor(slider)).toBe('66% (1.7 kW)');
  });

  it('never saves register 0 for a small non-zero percentage, which would stop charging', async () => {
    // 1% of 2600 W on a 9.5 kWh pack is register 0.27. Writing 0 disables
    // charging while the label claims a limit; the smallest real limit is 1.
    vi.mocked(apiPost).mockResolvedValue({ ok: true, data: {} });
    renderWith(makeSnapshot({ battery_capacity_kwh: 9.5, charge_rate: 17 }));
    const slider = powerSliders()[CHARGE];
    fireEvent.change(slider, { target: { value: '1' } });
    // Register 1 = 1% of 9500 Wh = 95 W = 4% of the inverter maximum.
    expect(readoutFor(slider)).toBe('4% (0.1 kW)');
    fireEvent.click(slider.parentElement?.querySelector('button') as HTMLButtonElement);
    expect(apiPost).toHaveBeenCalledWith('/api/control/charge-rate', { limit: 1 });
    // The save stays pending until the inverter reads register 1 back.
    act(() => useInverterStore.setState({
      snapshot: makeSnapshot({ battery_capacity_kwh: 9.5, charge_rate: 1 }),
    }));
    await act(async () => {});

    // A deliberate 0% still stops charging.
    fireEvent.change(slider, { target: { value: '0' } });
    fireEvent.click(slider.parentElement?.querySelector('button') as HTMLButtonElement);
    expect(apiPost).toHaveBeenCalledWith('/api/control/charge-rate', { limit: 0 });
  });

  it('keeps the plain half-scale mapping on a pack where capacity / 2 is the maximum', () => {
    // 5.12 kWh: the decoder caps the maximum at capacity / 2 = 2560 W, so the
    // register runs the full 0-50 and doubles to a percentage.
    vi.mocked(apiPost).mockResolvedValue({ ok: true, data: {} });
    renderWith(makeSnapshot({ battery_capacity_kwh: 5.12, max_battery_power_w: 2560, charge_rate: 31 }));
    const slider = powerSliders()[CHARGE];
    expect(readoutFor(slider)).toBe('62% (1.6 kW)');
    fireEvent.change(slider, { target: { value: '100' } });
    fireEvent.click(slider.parentElement?.querySelector('button') as HTMLButtonElement);
    expect(apiPost).toHaveBeenCalledWith('/api/control/charge-rate', { limit: 50 });
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

  it('moves the half-scale DC-hybrid sliders in 1% steps', () => {
    // The register is derived through the pack size, so the slider is no
    // longer restricted to even percentages.
    renderWith(makeSnapshot({ device_type_code: '2001' }));
    expect(powerSliders()[CHARGE].getAttribute('step')).toBe('1');
    expect(powerSliders()[DISCHARGE].getAttribute('step')).toBe('1');
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
    charge_rate: 10,
    discharge_rate: 10,
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

  it('renders the half-scale slider shape: 0-100 in 1% steps', () => {
    renderWith(gen1());
    for (const index of [CHARGE, DISCHARGE]) {
      const slider = powerSliders()[index];
      expect(slider.getAttribute('min')).toBe('0');
      expect(slider.getAttribute('max')).toBe('100');
      expect(slider.getAttribute('step')).toBe('1');
    }
    expect(captionFor(powerSliders()[CHARGE])).toBe('Battery Charge Power Limit');
    expect(captionFor(powerSliders()[DISCHARGE])).toBe('Battery Discharge Power Limit');
  });

  it('shows a share of 2.6 kW derived from the 13.5 kWh pack', () => {
    // Register 10 = 10% of 13500 Wh = 1350 W = 52% of 2600 W; register 25 =
    // 3375 W, above the inverter, so it reads as the full 2.6 kW.
    renderWith(gen1({ charge_rate: 10, discharge_rate: 25 }));
    expect(givTcpWattsFor(10, 13500, 2600)).toBe(1350);
    expect(readoutFor(powerSliders()[CHARGE])).toBe('52% (1.4 kW)');
    expect(readoutFor(powerSliders()[DISCHARGE])).toBe('100% (2.6 kW)');
  });

  it('gives every register below the maximum its own readout', () => {
    // 13.5 kWh behind 2.6 kW: registers 0-19 are below the inverter maximum
    // (19% of 13500 = 2565 W); 20 and above all reach it.
    const readouts = new Set<string>();
    for (let register = 0; register <= 19; register += 1) {
      cleanup();
      renderWith(gen1({ charge_rate: register }));
      const readout = readoutFor(powerSliders()[CHARGE]);
      const watts = givTcpWattsFor(register, 13500, 2600);
      const percent = Math.round((watts / 2600) * 100);
      expect(readout.startsWith(`${percent}%`)).toBe(true);
      readouts.add(readout);
    }
    expect(readouts.size).toBe(20);
    for (const register of [20, 31, 50]) {
      cleanup();
      renderWith(gen1({ charge_rate: register }));
      expect(readoutFor(powerSliders()[CHARGE])).toBe('100% (2.6 kW)');
    }
  });

  it.each([0, 2, 34, 50, 62, 98, 100])(
    'saves %i%% as the register GivTCP would write for that power',
    async (percent) => {
      renderWith(gen1());
      for (const [index, path] of [
        [CHARGE, '/api/control/charge-rate'],
        [DISCHARGE, '/api/control/discharge-rate'],
      ] as const) {
        const slider = powerSliders()[index];
        fireEvent.change(slider, { target: { value: String(percent) } });
        fireEvent.click(slider.parentElement?.querySelector('button') as HTMLButtonElement);
        const call = vi.mocked(apiPost).mock.calls.find(([called]) => called === path);
        expect(call).toBeDefined();
        const limit = (call?.[1] as { limit: number }).limit;
        const expected = givTcpRegisterFor((percent / 100) * 2600, 13500);
        // Within one register step of GivTCP's formula (100% writes the
        // register maximum instead, which is "no limit"), never beyond 0-50.
        if (percent === 100) {
          expect(limit).toBe(50);
        } else {
          expect(Math.abs(limit - expected)).toBeLessThanOrEqual(1);
        }
        expect(limit).toBeGreaterThanOrEqual(0);
        expect(limit).toBeLessThanOrEqual(50);
        // The delivered power is within one register step (135 W) of the wish.
        expect(Math.abs(givTcpWattsFor(limit, 13500, 2600) - (percent / 100) * 2600))
          .toBeLessThanOrEqual(135);
      }
    },
  );

  it('settles on the read-back value after a save and follows later external changes', async () => {
    renderWith(gen1({ charge_rate: 10 }));
    const slider = powerSliders()[CHARGE];

    // 80% of 2600 W = 2080 W = register 15.4 -> 15.
    fireEvent.change(slider, { target: { value: '80' } });
    fireEvent.click(slider.parentElement?.querySelector('button') as HTMLButtonElement);
    expect(apiPost).toHaveBeenCalledWith('/api/control/charge-rate', { limit: 15 });

    // Until the read-back arrives the slider keeps the saved position, shown as
    // the percentage register 15 really gives (1.5 x 1350 W = 2025 W = 78%).
    act(() => useInverterStore.setState({ snapshot: gen1({ charge_rate: 10 }) }));
    expect(readoutFor(powerSliders()[CHARGE])).toBe('78% (2.0 kW)');

    // The inverter reads back 15: the draft must be considered applied.
    act(() => useInverterStore.setState({ snapshot: gen1({ charge_rate: 15 }) }));
    expect(readoutFor(powerSliders()[CHARGE])).toBe('78% (2.0 kW)');

    // A later change from the GivEnergy app must show through.
    act(() => useInverterStore.setState({ snapshot: gen1({ charge_rate: 5 }) }));
    expect(readoutFor(powerSliders()[CHARGE])).toBe('26% (0.7 kW)');
  });

  it('offers the Inverter Active Power Limit on HR 50', () => {
    renderWith(gen1());
    expect(screen.queryByText('Inverter Active Power Limit')).not.toBeNull();
  });
});

/**
 * Issue #346: a limit change is queued by the API and applied register by
 * register by the poll loop, but the page used to drop its "Applying changes to
 * inverter" banner as soon as the POST returned - a few milliseconds - so the
 * user could not read it and nothing said when the inverter had actually taken
 * the value. The save now stays pending until a newer snapshot reads it back.
 */
describe('<ControlPage/> — power-limit saves wait for the inverter to confirm', () => {
  const gen1 = (overrides: Partial<InverterSnapshot> = {}) => makeSnapshot({
    device_type: 'gen1',
    device_type_display: 'Gen1',
    device_type_code: '1001',
    max_battery_power_w: 2600,
    battery_capacity_kwh: 9.5,
    charge_rate: 50,
    discharge_rate: 50,
    ...overrides,
  });
  const BANNER = 'Applying changes to inverter…';

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
    vi.useRealTimers();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    cleanup();
    useInverterStore.setState({ snapshot: null, connectionState: 'disconnected' });
  });

  const saveButton = (slider: HTMLElement) =>
    slider.parentElement?.querySelector('button') as HTMLButtonElement;

  it.each([
    ['charge', CHARGE, 'charge_rate'],
    ['discharge', DISCHARGE, 'discharge_rate'],
  ] as const)(
    'keeps the Applying banner up until the inverter reads the %s limit back',
    async (_name, index, field) => {
      renderWith(gen1());
      const slider = powerSliders()[index];
      fireEvent.change(slider, { target: { value: '62' } });
      fireEvent.click(saveButton(slider));
      await act(async () => {});

      // The POST has returned ("queued"), but the inverter has not applied it.
      expect(apiPost).toHaveBeenCalledTimes(1);
      expect(screen.queryByText(BANNER)).not.toBeNull();
      expect(saveButton(slider).disabled).toBe(true);

      // A snapshot that still shows the old value changes nothing.
      act(() => useInverterStore.setState({ snapshot: gen1() }));
      expect(screen.queryByText(BANNER)).not.toBeNull();

      // 62% of 2600 W on 9.5 kWh is register 17.
      act(() => useInverterStore.setState({ snapshot: gen1({ [field]: 17 }) }));
      await act(async () => {});
      expect(screen.queryByText(BANNER)).toBeNull();
      expect(saveButton(slider).disabled).toBe(false);
      expect(screen.queryByRole('alert')).toBeNull();
    },
  );

  it('reports a limit the inverter never reads back, and resets the slider', async () => {
    vi.useFakeTimers();
    renderWith(gen1());
    const slider = powerSliders()[CHARGE];
    fireEvent.change(slider, { target: { value: '62' } });
    fireEvent.click(saveButton(slider));
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
    expect(screen.queryByText(BANNER)).not.toBeNull();

    await act(async () => { await vi.advanceTimersByTimeAsync(20_000); });

    expect(screen.queryByText(BANNER)).toBeNull();
    const alert = screen.getByRole('alert').textContent ?? '';
    expect(alert).toMatch(/Charge power limit/);
    expect(alert).toMatch(/not confirmed|did not confirm/i);
    // A running Force Charge defers limit changes, so say so.
    expect(alert).toMatch(/Force Charge/);
    expect(alert).toMatch(/reset to the inverter's current setting/);
    expect(readoutFor(powerSliders()[CHARGE])).toBe('100% (2.6 kW)');
    expect(saveButton(powerSliders()[CHARGE]).disabled).toBe(false);
  });

  it('hands the slider back to the inverter once a save is confirmed', async () => {
    // Several percentages share one register on a capacity-relative scale. With
    // register 27 read as 99%, dragging to 97% saves the same register, so the
    // read-back never changes the base value and a kept draft would leave the
    // thumb at 97 under a "99%" label, hiding every later external change.
    renderWith(gen1({ charge_rate: 27 }));
    const slider = powerSliders()[CHARGE];
    expect(readoutFor(slider)).toBe('99% (2.6 kW)');
    fireEvent.change(slider, { target: { value: '97' } });
    fireEvent.click(saveButton(slider));
    await act(async () => {});
    act(() => useInverterStore.setState({ snapshot: gen1({ charge_rate: 27 }) }));
    await act(async () => {});

    expect(powerSliders()[CHARGE].value).toBe('99');
    // A later change from the GivEnergy app shows through.
    act(() => useInverterStore.setState({ snapshot: gen1({ charge_rate: 10 }) }));
    expect(readoutFor(powerSliders()[CHARGE])).toBe('37% (1.0 kW)');
    expect(powerSliders()[CHARGE].value).toBe('37');
  });

  it('cancels every pending confirmation when the page closes, even for overlapping saves', async () => {
    vi.useFakeTimers();
    // Baseline: timers left by an idle mount/unmount of the page itself.
    renderWith(gen1()).unmount();
    cleanup();
    const idle = vi.getTimerCount();

    const view = renderWith(gen1());
    const charge = powerSliders()[CHARGE];
    const discharge = powerSliders()[DISCHARGE];
    fireEvent.change(charge, { target: { value: '62' } });
    fireEvent.click(saveButton(charge));
    fireEvent.change(discharge, { target: { value: '62' } });
    fireEvent.click(saveButton(discharge));
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });

    view.unmount();
    // Both 20 s confirmation timers must be gone, not only the latest one.
    expect(vi.getTimerCount()).toBe(idle);
  });

  it('cancels a save that is still posting when the page closes', async () => {
    vi.useFakeTimers();
    renderWith(gen1()).unmount();
    cleanup();
    const idle = vi.getTimerCount();

    let release: () => void = () => {};
    vi.mocked(apiPost).mockImplementation(() => new Promise((resolve) => {
      release = () => resolve({ ok: true, data: {} });
    }));
    const view = renderWith(gen1());
    const slider = powerSliders()[CHARGE];
    fireEvent.change(slider, { target: { value: '62' } });
    fireEvent.click(saveButton(slider));
    view.unmount();

    // The POST resolves after the page is gone: no confirmation wait may start.
    release();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(vi.getTimerCount()).toBe(idle);
  });

  it('does not wait for confirmation when the save is rejected', async () => {
    vi.mocked(apiPost).mockRejectedValue(new Error('register write rejected'));
    renderWith(gen1());
    const slider = powerSliders()[CHARGE];
    fireEvent.change(slider, { target: { value: '62' } });
    fireEvent.click(saveButton(slider));
    await act(async () => {});

    expect(screen.queryByText(BANNER)).toBeNull();
    expect(screen.getByRole('alert').textContent).toMatch(/register write rejected/);
  });

  it('stops waiting when the page is closed mid-save', async () => {
    vi.useFakeTimers();
    const view = renderWith(gen1());
    const slider = powerSliders()[CHARGE];
    fireEvent.change(slider, { target: { value: '62' } });
    fireEvent.click(saveButton(slider));
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });

    view.unmount();
    // Nothing left to fire a state update on an unmounted page.
    await vi.advanceTimersByTimeAsync(30_000);
    expect(console.error).not.toHaveBeenCalled();
  });
});

describe('<ControlPage/> — charge-limit readout across every device family', () => {
  /** Locate a limit slider through its caption, independent of DOM order. */
  function sliderForCaption(caption: string): HTMLInputElement {
    const captionEl = screen.getByText(caption);
    const row = captionEl.closest('.space-y-1');
    return row?.querySelector('input[type="range"]') as HTMLInputElement;
  }

  // Every case uses a 9.5 kWh pack. The maximum is what the backend decoder
  // would report: the rated power capped at capacity / 2 = 4750 W. For the
  // half-scale families register 25 is 0.25C = 2375 W (GivTCP read.py), shown
  // as that share of the inverter's maximum.
  const CASES: Array<{
    code: string;
    family: string;
    caption: string;
    maxBatteryPowerW: number;
    chargeRegister: number;
    expected: string;
    showsActivePower: boolean;
  }> = [
    { code: '1001', family: 'Gen1 hybrid', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 2600, chargeRegister: 25, expected: '91% (2.4 kW)', showsActivePower: true },
    { code: '2001', family: 'Gen2/3 hybrid', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 3600, chargeRegister: 25, expected: '66% (2.4 kW)', showsActivePower: true },
    { code: '2101', family: 'Polar hybrid', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 2600, chargeRegister: 25, expected: '91% (2.4 kW)', showsActivePower: true },
    { code: '2201', family: 'Gen3 Plus hybrid', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 4750, chargeRegister: 25, expected: '50% (2.4 kW)', showsActivePower: true },
    { code: '2301', family: 'PV inverter', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 0, chargeRegister: 25, expected: '50%', showsActivePower: true },
    { code: '3001', family: 'AC-coupled', caption: 'AC Charge Power Limit', maxBatteryPowerW: 3000, chargeRegister: 66, expected: '66% (2.0 kW)', showsActivePower: true },
    { code: '3002', family: 'AC-coupled Mk2', caption: 'AC Charge Power Limit', maxBatteryPowerW: 3000, chargeRegister: 66, expected: '66% (2.0 kW)', showsActivePower: true },
    { code: '4001', family: 'Three-phase', caption: 'Three-phase Charge Power Limit', maxBatteryPowerW: 6000, chargeRegister: 66, expected: '66% (4.0 kW)', showsActivePower: false },
    { code: '5001', family: 'EMS', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 0, chargeRegister: 25, expected: '50%', showsActivePower: true },
    { code: '6001', family: 'AC three-phase', caption: 'Three-phase Charge Power Limit', maxBatteryPowerW: 6000, chargeRegister: 66, expected: '66% (4.0 kW)', showsActivePower: false },
    { code: '7001', family: 'Gateway', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 0, chargeRegister: 66, expected: '66%', showsActivePower: true },
    { code: '8001', family: 'AIO 6kW', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 4750, chargeRegister: 25, expected: '50% (2.4 kW)', showsActivePower: true },
    { code: '8002', family: 'AIO 3.6kW', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 3600, chargeRegister: 25, expected: '66% (2.4 kW)', showsActivePower: true },
    { code: '8003', family: 'AIO 5kW', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 4750, chargeRegister: 25, expected: '50% (2.4 kW)', showsActivePower: true },
    { code: '8101', family: 'Hybrid HV Gen3', caption: 'Three-phase Charge Power Limit', maxBatteryPowerW: 6000, chargeRegister: 66, expected: '66% (4.0 kW)', showsActivePower: false },
    { code: '8201', family: 'AIO Hybrid', caption: 'Three-phase Charge Power Limit', maxBatteryPowerW: 6000, chargeRegister: 66, expected: '66% (4.0 kW)', showsActivePower: false },
    { code: '8301', family: 'Gen4 hybrid', caption: 'Battery Charge Power Limit', maxBatteryPowerW: 4750, chargeRegister: 25, expected: '50% (2.4 kW)', showsActivePower: true },
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
      battery_capacity_kwh: 9.5,
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
