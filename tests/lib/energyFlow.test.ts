import { describe, it, expect } from 'vitest';
import {
  buildEnergyFlows,
  buildSummaryText,
  batteryModeDisplayLabel,
  batteryFillFraction,
  DEFAULT_NOISE_THRESHOLD_W,
} from '../../src/lib/energyFlow';
import type { InverterSnapshot, ScheduleSlot } from '../../src/lib/types';

/** Minimal snapshot with override-able fields. SOC/power fields default to
 *  a sane "everything idle" baseline; tests spread over the fields they
 *  care about. */
function snap(over: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    timestamp: 0,
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
    grid_voltage: 230,
    grid_frequency: 50,
    grid_online: true,
    grid_loss: false,
    inverter_trip: false,
    battery_over_temp: false,
    home_power: 0,
    inverter_temperature: 30,
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
    charge_rate: 0,
    discharge_rate: 0,
    active_power_rate: 0,
    max_battery_power_w: 0,
    max_ac_power_w: 0,
    export_limit_w: 0,
    target_soc: 100,
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
    max_charge_slots: 0,
    max_discharge_slots: 0,
    charge_slots: [],
    discharge_slots: [],
    meters: [],
    inverter_serial: '',
    firmware_version: '',
    dsp_firmware_version: '',
    dc_dsp_firmware_version: '',
    device_type: '',
    device_type_display: 'Gen 3 Hybrid',
    device_type_code: '2201',
    battery_calibration_stage: 0,
    enable_ammeter: false,
    enable_reversed_ct_clamp: false,
    meter_type: 0,
    supports_battery_calibration: false,
    ac_eps_enabled: false,
    ac_export_priority: 0,
    ...over,
  } as InverterSnapshot;
}

/** Find a flow by id, or undefined if filtered out. */
function flowById(vm: ReturnType<typeof buildEnergyFlows>, id: string) {
  return vm.flows.find((f) => f.id === id);
}

// ---------------------------------------------------------------------------
// Sign conventions (AGENTS.md "Battery power sign convention")
// ---------------------------------------------------------------------------

describe('buildEnergyFlows — sign conventions (home-centred)', () => {
  it('emits solar→home for positive solar_power above threshold', () => {
    const vm = buildEnergyFlows(snap({ solar_power: 5000 }));
    const f = flowById(vm, 'solar');
    expect(f).toBeDefined();
    expect(f!.from).toBe('solar');
    expect(f!.to).toBe('home');
    expect(f!.watts).toBe(5000);
    expect(f!.direction).toBe('generate');
  });

  it('treats +grid_power as export (home→grid) and −grid_power as import (grid→home)', () => {
    // Export: +4.3kW internally, displayed as -4.3kW because power leaves the home.
    const exp = buildEnergyFlows(snap({ grid_power: 4300 }));
    const ef = flowById(exp, 'export');
    expect(ef).toBeDefined();
    expect(ef!.from).toBe('home');
    expect(ef!.to).toBe('grid');
    expect(ef!.direction).toBe('export');
    expect(exp.nodes.find((n) => n.id === 'grid')!.value).toBe('-4.3kW');
    expect(flowById(exp, 'import')).toBeUndefined();

    // Import: −2kW internally, displayed as +2.0kW because power enters the home.
    const imp = buildEnergyFlows(snap({ grid_power: -2000 }));
    const imf = flowById(imp, 'import');
    expect(imf).toBeDefined();
    expect(imf!.from).toBe('grid');
    expect(imf!.to).toBe('home');
    expect(imf!.direction).toBe('import');
    expect(imp.nodes.find((n) => n.id === 'grid')!.value).toBe('+2.0kW');
    expect(flowById(imp, 'export')).toBeUndefined();
  });

  it('uses battery_state (not sign alone) to pick charge vs discharge direction', () => {
    // battery_state drives direction; battery_power magnitude is the watts.
    // charging → home→battery (charge).
    const chg = buildEnergyFlows(snap({ battery_state: 'charging', battery_power: -241 }));
    const cf = flowById(chg, 'charge');
    expect(cf).toBeDefined();
    expect(cf!.from).toBe('home');
    expect(cf!.to).toBe('battery');
    expect(cf!.watts).toBe(241);
    expect(cf!.direction).toBe('charge');
    expect(chg.nodes.find((n) => n.id === 'battery')!.value).toBe('+241W');
    expect(flowById(chg, 'discharge')).toBeUndefined();

    // discharge → battery→home, displayed as negative because power leaves the battery node.
    const dis = buildEnergyFlows(snap({ battery_state: 'discharging', battery_power: 1400 }));
    const df = flowById(dis, 'discharge');
    expect(df).toBeDefined();
    expect(df!.from).toBe('battery');
    expect(df!.to).toBe('home');
    expect(df!.direction).toBe('discharge');
    expect(dis.nodes.find((n) => n.id === 'battery')!.value).toBe('-1.4kW');
  });

  it('never emits a self-flow for home (it is the hub, not a spoke)', () => {
    // Home's consumption shows as the hub node value, not a directed flow.
    const vm = buildEnergyFlows(snap({ home_power: 501 }));
    expect(flowById(vm, 'home')).toBeUndefined();
    // But the home node itself is active and carries the consumption value.
    const home = vm.nodes.find((n) => n.id === 'home');
    expect(home).toBeDefined();
    expect(home!.active).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// Noise threshold gating
// ---------------------------------------------------------------------------

describe('buildEnergyFlows — noise threshold', () => {
  it('filters out flows below the threshold (default 20W)', () => {
    const vm = buildEnergyFlows(snap({ solar_power: 15, grid_power: 10 }));
    expect(flowById(vm, 'solar')).toBeUndefined();
    expect(flowById(vm, 'import')).toBeUndefined();
    expect(vm.nodes.find((n) => n.id === 'solar')!.active).toBe(false);
  });

  it('treats the threshold as a strict greater-than boundary', () => {
    // solar_power == threshold is NOT active (matches the legacy diagram's
    // strict `> noiseThreshold`); one watt above is.
    expect(flowById(buildEnergyFlows(snap({ solar_power: DEFAULT_NOISE_THRESHOLD_W })), 'solar'))
      .toBeUndefined();
    expect(flowById(buildEnergyFlows(snap({ solar_power: DEFAULT_NOISE_THRESHOLD_W + 1 })), 'solar'))
      .toBeDefined();
  });

  it('honours a custom threshold', () => {
    const vm = buildEnergyFlows(snap({ solar_power: 100 }), { noiseThresholdW: 150 });
    expect(flowById(vm, 'solar')).toBeUndefined();
    const vm2 = buildEnergyFlows(snap({ solar_power: 200 }), { noiseThresholdW: 150 });
    expect(flowById(vm2, 'solar')).toBeDefined();
  });

  it('clamps sub-threshold node values to "0W"', () => {
    const vm = buildEnergyFlows(snap({ solar_power: 5 }));
    expect(vm.nodes.find((n) => n.id === 'solar')!.value).toBe('0W');
  });

  it('exposes battery SOC as a node ring percentage for the radial diagram', () => {
    const vm = buildEnergyFlows(snap({ soc: 31 }));
    expect(vm.nodes.find((n) => n.id === 'battery')!.ringPercent).toBe(31);
  });

  it('maxFlowWatts is ≥1 even when no flows are active', () => {
    const vm = buildEnergyFlows(snap());
    expect(vm.maxFlowWatts).toBe(1);
    expect(vm.flows).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// EV charger node + flow
// ---------------------------------------------------------------------------

describe('buildEnergyFlows — EV charger', () => {
  it('omits the EV node entirely when showEvc is false', () => {
    const vm = buildEnergyFlows(snap(), { evcPowerW: 7000, showEvc: false });
    expect(vm.nodes.find((n) => n.id === 'ev')).toBeUndefined();
    expect(flowById(vm, 'ev')).toBeUndefined();
  });

  it('emits EV node + home→ev flow when charging above threshold', () => {
    const vm = buildEnergyFlows(snap({ home_power: 7500 }), { evcPowerW: 7000, showEvc: true });
    const ev = vm.nodes.find((n) => n.id === 'ev');
    expect(ev).toBeDefined();
    expect(ev!.active).toBe(true);
    expect(ev!.unit).toBe('Charging');
    const f = flowById(vm, 'ev');
    expect(f!.from).toBe('home');
    expect(f!.to).toBe('ev');
    expect(f!.watts).toBe(7000);
  });

  it('shows EV node as Idle when configured but not charging', () => {
    const vm = buildEnergyFlows(snap(), { evcPowerW: 0, showEvc: true });
    const ev = vm.nodes.find((n) => n.id === 'ev');
    expect(ev).toBeDefined();
    expect(ev!.active).toBe(false);
    expect(ev!.unit).toBe('Idle');
    expect(flowById(vm, 'ev')).toBeUndefined();
  });

  it('uses the resolved evcLabel for the node unit when provided', () => {
    // Charger configured + reachable but not delivering → "Connected".
    const vm = buildEnergyFlows(snap(), { evcPowerW: 0, showEvc: true, evcLabel: 'Connected' });
    expect(vm.nodes.find((n) => n.id === 'ev')!.unit).toBe('Connected');
    // A never-reached host → "Not Found" (issue #138).
    const vm2 = buildEnergyFlows(snap(), { evcPowerW: 0, showEvc: true, evcLabel: 'Not Found' });
    expect(vm2.nodes.find((n) => n.id === 'ev')!.unit).toBe('Not Found');
  });
});

// ---------------------------------------------------------------------------
// Battery fill fraction (AA-cell gauge)
// ---------------------------------------------------------------------------

describe('batteryFillFraction', () => {
  it('maps 0% to 0 and 100% to 1', () => {
    expect(batteryFillFraction(0)).toBe(0);
    expect(batteryFillFraction(100)).toBe(1);
  });

  it('is linear in SOC', () => {
    expect(batteryFillFraction(50)).toBe(0.5);
    expect(batteryFillFraction(25)).toBe(0.25);
    expect(batteryFillFraction(97)).toBe(0.97);
  });

  it('clamps out-of-range values', () => {
    expect(batteryFillFraction(150)).toBe(1);
    expect(batteryFillFraction(-20)).toBe(0);
  });

  it('treats NaN / non-finite as 0', () => {
    expect(batteryFillFraction(Number.NaN)).toBe(0);
    expect(batteryFillFraction(Number.POSITIVE_INFINITY)).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// Battery mode label (de-duped from EnergyFlowDiagram + BatteryPanel)
// ---------------------------------------------------------------------------

describe('batteryModeDisplayLabel', () => {
  const slot = (h0: number, m0: number, h1: number, m1: number): ScheduleSlot => ({
    enabled: true, start_hour: h0, start_minute: m0, end_hour: h1, end_minute: m1, target_soc: 100,
  });

  it('returns the raw mode label by default', () => {
    expect(batteryModeDisplayLabel('eco', false, false, false, false, [], [])).toBe('Eco');
    expect(batteryModeDisplayLabel('timed_demand', false, false, false, false, [], [])).toBe('Timed Demand');
  });

  it('returns "Cosy" when cosy is active', () => {
    expect(batteryModeDisplayLabel('eco', true, false, false, false, [], [])).toBe('Cosy');
  });

  it('returns "Cosy" when cosy-enabled and in an eco mode', () => {
    expect(batteryModeDisplayLabel('eco', false, true, false, false, [], [])).toBe('Cosy');
    expect(batteryModeDisplayLabel('eco_paused', false, true, false, false, [], [])).toBe('Cosy');
    // Non-eco mode with cosy enabled falls through to the raw label.
    expect(batteryModeDisplayLabel('timed_demand', false, true, false, false, [], [])).toBe('Timed Demand');
  });

  it('returns an Eco charging label when inside an enabled charge window', () => {
    // Window 00:00–23:59 always active.
    const slots = [slot(0, 0, 23, 59)];
    expect(batteryModeDisplayLabel('eco', false, false, true, false, slots, [])).toBe('Eco (Charging)');
    expect(batteryModeDisplayLabel('eco_paused', false, false, true, false, slots, [])).toBe('Eco (Charging)');
  });

  it('returns an Eco discharging label when inside an enabled discharge window', () => {
    const slots = [slot(0, 0, 23, 59)];
    expect(batteryModeDisplayLabel('eco', false, false, false, true, [], slots)).toBe('Eco (Discharging)');
    expect(batteryModeDisplayLabel('eco_paused', false, false, false, true, [], slots)).toBe('Eco (Discharging)');
  });

  it('prefixes active-window labels with the actual timed/export mode', () => {
    const slots = [slot(0, 0, 23, 59)];
    expect(batteryModeDisplayLabel('timed_demand', false, false, true, false, slots, []))
      .toBe('Timed Demand (Charging)');
    expect(batteryModeDisplayLabel('timed_export', false, false, false, true, [], slots))
      .toBe('Timed Export (Discharging)');
    expect(batteryModeDisplayLabel('export_paused', false, false, true, false, slots, []))
      .toBe('Export Paused (Charging)');
  });

  it('combines the suffix when both charge and discharge windows are active', () => {
    const slots = [slot(0, 0, 23, 59)];
    expect(batteryModeDisplayLabel('eco', false, false, true, true, slots, slots))
      .toBe('Eco (Charging & Discharging)');
  });

  it('does not override when enable flag is set but no slot is active', () => {
    // Window in the past relative to a fixed noon `now`.
    const slots = [slot(0, 0, 1, 0)];
    const noon = new Date(2025, 0, 1, 12, 0);
    expect(batteryModeDisplayLabel('eco', false, false, true, false, slots, [], noon)).toBe('Eco');
  });

  it('handles wrap-around midnight slots', () => {
    // 23:00–01:00 active at midnight.
    const slots = [slot(23, 0, 1, 0)];
    const midnight = new Date(2025, 0, 1, 0, 30);
    expect(batteryModeDisplayLabel('eco', false, false, true, false, slots, [], midnight))
      .toBe('Eco (Charging)');
  });
});

// ---------------------------------------------------------------------------
// Summary sentence
// ---------------------------------------------------------------------------

describe('buildSummaryText', () => {
  const base = {
    solarActive: false, solarWatts: 0,
    isExporting: false, exportWatts: 0,
    isImporting: false, importWatts: 0,
    isCharging: false, chargeWatts: 0,
    isDischarging: false, dischargeWatts: 0,
    homeActive: false, homeWatts: 0,
    evcActive: false, evcWatts: 0,
    noise: 20,
  };

  it('reads "System is idle." when nothing is active', () => {
    expect(buildSummaryText(base)).toBe('System is idle.');
  });

  it('solar powering home + charging battery + exporting', () => {
    expect(buildSummaryText({
      ...base, solarActive: true, solarWatts: 5000,
      homeActive: true, homeWatts: 501,
      isCharging: true, chargeWatts: 241,
      isExporting: true, exportWatts: 4300,
    })).toBe('Solar is powering the home, charging the battery at 241W and exporting 4.3kW to the grid.');
  });

  it('solar alone powering home', () => {
    expect(buildSummaryText({
      ...base, solarActive: true, solarWatts: 3000,
      homeActive: true, homeWatts: 500,
    })).toBe('Solar is powering the home.');
  });

  it('battery + solar powering home (plural verb)', () => {
    expect(buildSummaryText({
      ...base, solarActive: true, solarWatts: 1000,
      isDischarging: true, dischargeWatts: 500,
      homeActive: true, homeWatts: 1500,
    })).toBe('Solar and the battery are powering the home.');
  });

  it('grid import powering home at night', () => {
    expect(buildSummaryText({
      ...base, isImporting: true, importWatts: 900,
      homeActive: true, homeWatts: 900,
    })).toBe('The grid is powering the home.');
  });

  it('no home load: solar charging battery and exporting', () => {
    expect(buildSummaryText({
      ...base, solarActive: true, solarWatts: 5000,
      isCharging: true, chargeWatts: 241,
      isExporting: true, exportWatts: 4300,
    })).toBe('Solar is generating 5.0kW, charging the battery at 241W and exporting 4.3kW to the grid.');
  });

  it('home load but all sources under threshold falls back to consumption', () => {
    expect(buildSummaryText({
      ...base, homeActive: true, homeWatts: 60,
    })).toBe('Home is consuming 60W.');
  });

  it('includes EV charging as a destination', () => {
    expect(buildSummaryText({
      ...base, solarActive: true, solarWatts: 7000,
      homeActive: true, homeWatts: 7500,
      evcActive: true, evcWatts: 7000,
    })).toBe('Solar is powering the home, charging the EV at 7.0kW.');
  });
});
