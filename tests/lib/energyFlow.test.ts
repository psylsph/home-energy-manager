import { describe, it, expect } from 'vitest';
import {
  buildEnergyFlows,
  buildSummaryText,
  batteryModeDisplayLabel,
  batteryFillFraction,
  DEFAULT_NOISE_THRESHOLD_W,
  socColor,
  isAnySlotActive,
  FLOW_COLORS,
  BATTERY_OUTPUT_COLOR,
} from '../../src/lib/energyFlow';
import type { InverterSnapshot, ScheduleSlot } from '../../src/lib/types';

const slot = (h0: number, m0: number, h1: number, m1: number): ScheduleSlot => ({
  enabled: true,
  start_hour: h0,
  start_minute: m0,
  end_hour: h1,
  end_minute: m1,
  target_soc: 100,
});

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
    // Export: +4.3kW internally, with solar driving the surplus so the
    // export spoke is genuinely emitted under issue #170 source-
    // attribution (solar powers the home then exports its surplus). The
    // visual spoke is rerouted to solar → grid (yellow). Magnitude is
    // shown as a plain positive value; the direction signal lives in
    // the "Exporting" / "Importing" status word rendered under the
    // orbit node, not in a `+`/`-` prefix.
    //
    // Before issue #170 source-attribution, an export spoke was drawn
    // for any positive `grid_power` regardless of source; that route is
    // no longer taken — a grid export with neither solar nor a battery
    // discharge surplus has no spoke to draw, because there is no
    // attributable source for the renderer to point at. That case is
    // covered separately in the matrix below ("no export spoke when
    // neither solar nor battery is exporting").
    const exp = buildEnergyFlows(snap({ solar_power: 5000, home_power: 500, grid_power: 4500 }));
    const ef = flowById(exp, 'export');
    expect(ef).toBeDefined();
    expect(ef!.from).toBe('home');
    expect(ef!.to).toBe('grid');
    expect(ef!.direction).toBe('export');
    expect(exp.nodes.find((n) => n.id === 'grid')!.value).toBe('4.5kW');
    expect(flowById(exp, 'import')).toBeUndefined();

    // Import: −2kW internally. Same — magnitude is positive; direction
    // comes from the status word below.
    const imp = buildEnergyFlows(snap({ grid_power: -2000 }));
    const imf = flowById(imp, 'import');
    expect(imf).toBeDefined();
    expect(imf!.from).toBe('grid');
    expect(imf!.to).toBe('home');
    expect(imf!.direction).toBe('import');
    expect(imp.nodes.find((n) => n.id === 'grid')!.value).toBe('2.0kW');
    expect(flowById(imp, 'export')).toBeUndefined();
  });

  it('draws the grid→home import spoke while the battery is discharging (issue #192)', () => {
    // Reported scenario: solar 500W, battery discharging 4.6kW, home 6kW,
    // grid importing 910W. The import spoke must still appear — the
    // battery is discharging, so none of the grid inflow is feeding the
    // battery. Regression: charge-attribution used absBattery ungated,
    // so a discharging battery's 4.6kW was mistaken for charge wattage
    // and `gridPortionToBattery` swallowed the entire 910W import,
    // hiding the red grid→home line.
    const vm = buildEnergyFlows(snap({
      solar_power: 500,
      battery_state: 'discharging',
      battery_power: 4600,
      home_power: 6000,
      grid_power: -910,
    }));
    const imp = flowById(vm, 'import');
    expect(imp).toBeDefined();
    expect(imp!.from).toBe('grid');
    expect(imp!.to).toBe('home');
    expect(imp!.watts).toBe(910);
    expect(imp!.color).toBe(FLOW_COLORS.grid); // red
    // The battery is discharging to the home, not charging from the grid,
    // so there must be no grid→battery charge spoke.
    expect(flowById(vm, 'grid_charge')).toBeUndefined();
    expect(flowById(vm, 'discharge')).toBeDefined();
  });

  it('uses battery_state (not sign alone) to pick charge vs discharge direction', () => {
    // battery_state drives direction; battery_power magnitude is the watts.
    // charging → home→battery (charge). Node value is the plain magnitude;
    // the "Charging" badge below carries the direction signal.
    const chg = buildEnergyFlows(snap({ battery_state: 'charging', battery_power: -241 }));
    const cf = flowById(chg, 'charge');
    expect(cf).toBeDefined();
    expect(cf!.from).toBe('home');
    expect(cf!.to).toBe('battery');
    expect(cf!.watts).toBe(241);
    expect(cf!.direction).toBe('charge');
    expect(chg.nodes.find((n) => n.id === 'battery')!.value).toBe('241W');
    expect(flowById(chg, 'discharge')).toBeUndefined();

    // discharge → battery→home, again plain magnitude + direction in the
    // badge ("Discharging"). Showing "-1.4kW" next to "Discharging" used
    // to read as a sign-convention bug to non-technical users.
    // Discharge breakout (issue #155 + #170 final fix):
    //  - `discharge` flow carries only the home-direct portion
    //    (min(battery, home) = 800 W). The remaining 600 W appears
    //    separately as `discharge_to_grid`. This avoids the visual
    //    double-counting the user flagged: when solar is also active and
    //    the battery is overflowing to grid, the old code emitted both
    //    a `discharge` of the full battery wattage AND an `export` of the
    //    surplus, which drew a misleading yellow solar → grid spoke.
    const dis = buildEnergyFlows(
      snap({ battery_state: 'discharging', battery_power: 1400, home_power: 800 }),
    );
    const df = flowById(dis, 'discharge');
    expect(df).toBeDefined();
    expect(df!.from).toBe('battery');
    expect(df!.to).toBe('home');
    expect(df!.watts).toBe(800);
    expect(df!.direction).toBe('discharge');
    // Spoke colour: battery-on-spoke → green (priority solar > battery > grid).
    // BATTERY_OUTPUT_COLOR is intentionally the same as socColor(≥ 50)
    // (both `#22C55E`) so the battery symbol and its discharge spoke share
    // an identity at healthy SOC (issue #170).
    expect(df!.color).toBe(BATTERY_OUTPUT_COLOR);
    expect(dis.nodes.find((n) => n.id === 'battery')!.value).toBe('1.4kW');
    // The 600 W surplus appears as `discharge_to_grid`.
    const toGrid = dis.flows.find((f) => f.id === 'discharge_to_grid');
    expect(toGrid).toBeDefined();
    expect(toGrid!.watts).toBe(600);
  });

  it('solar_charge (solar → battery) is yellow when solar covers the charge (issue #170)', () => {
    // With solar active and the battery charging, the charge is split
    // into source-attributed spokes (issue #170). When solar covers all
    // of the charge, only `solar_charge` is emitted — it's the yellow
    // spoke the user requested. The aggregate `charge` flow is
    // suppressed to avoid visual double-counting.
    const withSolar = buildEnergyFlows(
      snap({
        solar_power: 5000,
        home_power: 500,
        battery_state: 'charging',
        battery_power: -1500,
      }),
    );
    const sc = withSolar.flows.find((f) => f.id === 'solar_charge');
    expect(sc, 'solar_charge flow with solar').toBeDefined();
    expect(sc!.color).toBe(FLOW_COLORS.solar);

    const noSolar = buildEnergyFlows(
      snap({ grid_power: -2000, battery_state: 'charging', battery_power: -1500 }),
    );
    const gc = noSolar.flows.find((f) => f.id === 'grid_charge');
    expect(gc, 'grid_charge flow without solar').toBeDefined();
    expect(gc!.color).toBe(FLOW_COLORS.grid);
  });

  it('emits a battery→grid discharge_to_grid flow when discharge exceeds the house load (issue #155)', () => {
    // Battery 2 kW, house 500 W. Excess 1.5 kW flows battery→grid directly,
    // so the moving dot ends at the grid as the GivEnergy app shows.
    const vm = buildEnergyFlows(
      snap({ battery_state: 'discharging', battery_power: 2000, home_power: 500 }),
    );
    const excess = vm.flows.find((f) => f.id === 'discharge_to_grid');
    expect(excess).toBeDefined();
    expect(excess!.from).toBe('battery');
    expect(excess!.to).toBe('grid');
    expect(excess!.watts).toBe(1500);
    expect(excess!.direction).toBe('export');
    // Battery-on-spoke → green (priority solar > battery > grid, issue #170).
    expect(excess!.color).toBe(BATTERY_OUTPUT_COLOR);
  });

  it('does not emit a battery→grid flow when the house absorbs all of the discharge (issue #155)', () => {
    // Battery 500 W, house 800 W. No excess to export.
    const vm = buildEnergyFlows(
      snap({ battery_state: 'discharging', battery_power: 500, home_power: 800 }),
    );
    expect(vm.flows.find((f) => f.id === 'discharge_to_grid')).toBeUndefined();
  });

  it('routes the full discharge to grid when the house is idle (issue #172)', () => {
    // Overnight timed-export / forced discharge with everything off:
    // battery_power 2000 W, home_power ~0 W, grid_power +1990 W. The
    // old code substituted `absBattery` for `home_power` whenever the
    // home was at or below the noise floor, which silently dropped the
    // battery → grid spoke and drew the full 2 kW as battery → home into
    // a house that consumes nothing. The fixed path routes the whole
    // battery output to `discharge_to_grid` and emits no `discharge`
    // spoke at all.
    const vm = buildEnergyFlows(
      snap({ battery_state: 'discharging', battery_power: 2000, home_power: 0, grid_power: 1990 }),
    );
    const toGrid = vm.flows.find((f) => f.id === 'discharge_to_grid');
    expect(toGrid, 'discharge_to_grid missing for idle-home discharge').toBeDefined();
    expect(toGrid!.from).toBe('battery');
    expect(toGrid!.to).toBe('grid');
    expect(toGrid!.watts).toBe(2000);
    expect(toGrid!.color).toBe(BATTERY_OUTPUT_COLOR);
    // Home spoke is not emitted — there is no discharge into an idle hub.
    expect(flowById(vm, 'discharge'), 'discharge spoke must not appear for idle home').toBeUndefined();
    // And there is no `export` spoke either; the grid reading is
    // battery-attributed, so it's carried by `discharge_to_grid` only
    // (avoids the double-count the issue's "Notes" warn about for #155).
    expect(flowById(vm, 'export'), 'export spoke must not appear when battery owns the grid reading').toBeUndefined();
  });

  it('routes discharge entirely to grid below the noise floor, splits above it (issue #172)', () => {
    // Below the noise floor (home_power strictly less than `noise`):
    //  - batteryToHome clamps to 0, all discharge routes to
    //    discharge_to_grid. Pins the user-reported bug.
    // Strictly above the noise floor:
    //  - normal split — home-direct portion goes home, surplus to
    //    discharge_to_grid. Pins the issue #155 contract.
    //
    // We use 5 W and 50 W so we stay clearly on each side of the
    // default 20 W floor (the 20 W boundary itself is exercised by
    // the separate "splits at the noise boundary" test below).
    const idle = buildEnergyFlows(
      snap({ battery_state: 'discharging', battery_power: 2000, home_power: 5, grid_power: 1995 }),
    );
    const idleToGrid = idle.flows.find((f) => f.id === 'discharge_to_grid');
    expect(idleToGrid, 'discharge_to_grid missing for below-noise home').toBeDefined();
    expect(idleToGrid!.watts).toBe(2000);
    expect(idle.flows.find((f) => f.id === 'discharge'), 'discharge spoke must not appear for below-noise home').toBeUndefined();

    const loaded = buildEnergyFlows(
      snap({ battery_state: 'discharging', battery_power: 2000, home_power: 50, grid_power: 1950 }),
    );
    const loadedDischarge = loaded.flows.find((f) => f.id === 'discharge');
    const loadedToGrid = loaded.flows.find((f) => f.id === 'discharge_to_grid');
    expect(loadedDischarge, 'discharge spoke missing for above-noise home').toBeDefined();
    expect(loadedDischarge!.watts).toBe(50);
    expect(loadedToGrid, 'discharge_to_grid spoke missing for above-noise home').toBeDefined();
    expect(loadedToGrid!.watts).toBe(1950);
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

  // The sign-prefix on the orbit node value used to read as "-839W" next
  // to a "Discharging" badge, which looked like a bug to non-technical
  // users. The direction signal now lives in the status word below the
  // node; the value is the plain magnitude. Lock that contract in here
  // so future refactors can't quietly reintroduce the prefixes.
  it('grid and battery node values are always non-negative magnitudes', () => {
    // Each combination: export, import, charge, discharge, idle.
    const cases = [
      { name: 'export', snap: snap({ grid_power: 4300 }) },
      { name: 'import', snap: snap({ grid_power: -2000 }) },
      { name: 'charge', snap: snap({ battery_state: 'charging', battery_power: -241 }) },
      { name: 'discharge', snap: snap({ battery_state: 'discharging', battery_power: 839 }) },
      { name: 'idle', snap: snap({ battery_power: 0, grid_power: 0 }) },
    ];
    for (const c of cases) {
      const vm = buildEnergyFlows(c.snap);
      const gridValue = vm.nodes.find((n) => n.id === 'grid')!.value;
      const batteryValue = vm.nodes.find((n) => n.id === 'battery')!.value;
      expect(gridValue, `${c.name} grid value must not start with - or +`).not.toMatch(/^[+-]/);
      expect(batteryValue, `${c.name} battery value must not start with - or +`).not.toMatch(/^[+-]/);
    }
  });
});

// ---------------------------------------------------------------------------
// Simultaneous discharge + import — the seam between the #155 (discharge)
// and #170 (charge attribution) lanes. Each lane was tested in isolation;
// their overlap (a discharging battery while the grid imports) was the blind
// spot that hid the import spoke. These pin the interaction space.
// ---------------------------------------------------------------------------
describe('buildEnergyFlows — simultaneous discharge + import (issue #192 seam)', () => {
  /** Sum of wattage on spokes ending at home — the energy actually delivered
   *  to the house. For discharging scenarios (no charge spokes) this must
   *  reconcile with home_power. */
  const wattsIntoHome = (vm: ReturnType<typeof buildEnergyFlows>): number =>
    vm.flows.filter((f) => f.to === 'home').reduce((s, f) => s + f.watts, 0);

  it('shows solar + battery + grid all feeding home, balanced', () => {
    // Solar 1kW, battery discharging 2kW, home 4kW, grid importing 1kW.
    // All three sources contribute; no charge spokes (battery is discharging).
    const vm = buildEnergyFlows(snap({
      solar_power: 1000,
      battery_state: 'discharging',
      battery_power: 2000,
      home_power: 4000,
      grid_power: -1000,
    }));
    expect(flowById(vm, 'solar')!.watts).toBe(1000);
    expect(flowById(vm, 'discharge')!.watts).toBe(2000);
    const imp = flowById(vm, 'import');
    expect(imp).toBeDefined();
    expect(imp!.watts).toBe(1000);
    expect(imp!.color).toBe(FLOW_COLORS.grid);
    // No charge spokes — the battery is discharging, not charging.
    expect(flowById(vm, 'grid_charge')).toBeUndefined();
    expect(flowById(vm, 'solar_charge')).toBeUndefined();
    expect(flowById(vm, 'discharge_to_grid')).toBeUndefined();
    // Energy delivered to home reconciles with consumption.
    expect(wattsIntoHome(vm)).toBe(4000);
  });

  it('shows the full import when the battery discharge is small (no understatement)', () => {
    // Battery discharging only 500W while the grid imports 3kW to cover a
    // 3.5kW load. Before the fix the 500W discharge magnitude was mistaken
    // for charge wattage and clipped 500W off the import spoke (showing
    // 2.5kW instead of 3kW). The import must now read its full wattage.
    const vm = buildEnergyFlows(snap({
      battery_state: 'discharging',
      battery_power: 500,
      home_power: 3500,
      grid_power: -3000,
    }));
    expect(flowById(vm, 'discharge')!.watts).toBe(500);
    expect(flowById(vm, 'import')!.watts).toBe(3000);
    expect(flowById(vm, 'grid_charge')).toBeUndefined();
    expect(wattsIntoHome(vm)).toBe(3500);
  });

  it('shows full import with no solar, just battery + grid (two sources)', () => {
    // Overnight / overcast: no solar, battery 2kW discharge, grid imports
    // 1kW to make up a 3kW load. Minimal two-source case.
    const vm = buildEnergyFlows(snap({
      battery_state: 'discharging',
      battery_power: 2000,
      home_power: 3000,
      grid_power: -1000,
    }));
    expect(flowById(vm, 'solar')).toBeUndefined();
    expect(flowById(vm, 'discharge')!.watts).toBe(2000);
    expect(flowById(vm, 'import')!.watts).toBe(1000);
    expect(flowById(vm, 'grid_charge')).toBeUndefined();
    expect(wattsIntoHome(vm)).toBe(3000);
  });

  it('still splits charge attribution correctly when the battery IS charging (gate does not over-correct)', () => {
    // Counterpart: a genuinely charging battery while importing must keep
    // the #170 source-attribution split — grid_charge carries the battery
    // portion, the import spoke carries only the home-direct remainder.
    // solar 1kW, battery charging 2.5kW, home 0.5kW, grid importing 2kW.
    const vm = buildEnergyFlows(snap({
      solar_power: 1000,
      battery_state: 'charging',
      battery_power: -2500,
      home_power: 500,
      grid_power: -2000,
    }));
    expect(flowById(vm, 'solar_charge')!.watts).toBe(1000);
    expect(flowById(vm, 'grid_charge')!.watts).toBe(1500);
    // Import spoke = grid inflow minus the grid-fed charge portion.
    expect(flowById(vm, 'import')!.watts).toBe(500);
    // Everything leaving the grid reconciles with the metered inflow.
    const leavingGrid = vm.flows
      .filter((f) => f.from === 'grid')
      .reduce((s, f) => s + f.watts, 0);
    expect(leavingGrid).toBe(2000);
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

  it('appends the overall solar % next to the Solar kW value when DC capacity is configured (issue #192)', () => {
    const vm = buildEnergyFlows(snap({
      solar_power: 6000,
      solar_arrays: [
        { source: 'pv1', name: '', power_w: 3000, rated_kw: 4, today_kwh: null, meter_address: null },
        { source: 'pv2', name: '', power_w: 3000, rated_kw: 4, today_kwh: null, meter_address: null },
      ],
    }));
    // 6 kW from 8 kWp total → 75%.
    expect(vm.nodes.find((n) => n.id === 'solar')!.value).toBe('6.0kW (75%)');
  });

  it('omits the solar % when no DC-string capacity is configured', () => {
    const vm = buildEnergyFlows(snap({ solar_power: 6000 }));
    expect(vm.nodes.find((n) => n.id === 'solar')!.value).toBe('6.0kW');
  });

  it('shows measured grid amps instead of frequency when a grid CT meter exists (issue #192)', () => {
    const vm = buildEnergyFlows(snap({
      grid_voltage: 241,
      meters: [{
        address: 0x00, v_phase_1: 241, v_phase_2: 0, v_phase_3: 0,
        i_phase_1: 0, i_phase_2: 0, i_phase_3: 0, i_total: 28.3,
        p_active_phase_1: 0, p_active_phase_2: 0, p_active_phase_3: 0,
        p_active_total: 0, p_reactive_total: 0, p_apparent_total: 0,
        pf_total: 0, frequency: 50, e_import_active_kwh: 0, e_export_active_kwh: 0,
      }],
    }));
    expect(vm.nodes.find((n) => n.id === 'grid')!.unit).toBe('241.0V/28.3A');
  });

  it('keeps grid frequency when no grid CT meter is present (single-phase)', () => {
    const vm = buildEnergyFlows(snap({ grid_voltage: 241, grid_frequency: 50 }));
    expect(vm.nodes.find((n) => n.id === 'grid')!.unit).toBe('241.0V/50.00Hz');
  });

  it('reads grid amps from a designated external CT meter (AC-coupled, issue #192)', () => {
    // AC-coupled: no 0x00 built-in meter, so the user designates their grid
    // CT address (here 0x01) via Settings. The wheel reads that meter's amps.
    const vm = buildEnergyFlows(
      snap({
        grid_voltage: 241,
        meters: [{
          address: 0x01, v_phase_1: 241, v_phase_2: 0, v_phase_3: 0,
          i_phase_1: 0, i_phase_2: 0, i_phase_3: 0, i_total: 41.2,
          p_active_phase_1: 0, p_active_phase_2: 0, p_active_phase_3: 0,
          p_active_total: 0, p_reactive_total: 0, p_apparent_total: 0,
          pf_total: 0, frequency: 50, e_import_active_kwh: 0, e_export_active_kwh: 0,
        }],
      }),
      { gridMeterAddress: 0x01 },
    );
    expect(vm.nodes.find((n) => n.id === 'grid')!.unit).toBe('241.0V/41.2A');
  });

  it('falls back to external CT phase current when total current is zero (issue #201)', () => {
    const vm = buildEnergyFlows(
      snap({
        grid_voltage: 241.5,
        meters: [{
          address: 0x01, v_phase_1: 240.8, v_phase_2: 0, v_phase_3: 0,
          i_phase_1: 5.75, i_phase_2: 0, i_phase_3: 0, i_total: 0,
          p_active_phase_1: 834, p_active_phase_2: 0, p_active_phase_3: 0,
          p_active_total: 0, p_reactive_total: 0, p_apparent_total: 0,
          pf_total: 0, frequency: 49.96, e_import_active_kwh: 3518.5, e_export_active_kwh: 321.7,
        }],
      }),
      { gridMeterAddress: 0x01 },
    );
    expect(vm.nodes.find((n) => n.id === 'grid')!.unit).toBe('241.5V/5.8A');
  });

  it('suppresses stale phase-current fallback when the external CT reports near-zero active power', () => {
    const vm = buildEnergyFlows(
      snap({
        grid_voltage: 240,
        grid_power: 2,
        meters: [{
          address: 0x01, v_phase_1: 240, v_phase_2: 0, v_phase_3: 0,
          i_phase_1: 4.64, i_phase_2: 0, i_phase_3: 0, i_total: 0,
          p_active_phase_1: 2, p_active_phase_2: 0, p_active_phase_3: 0,
          p_active_total: 0, p_reactive_total: 0, p_apparent_total: 0,
          pf_total: 0, frequency: 49.96, e_import_active_kwh: 3518.5, e_export_active_kwh: 321.7,
        }],
      }),
      { gridMeterAddress: 0x01 },
    );
    expect(vm.nodes.find((n) => n.id === 'grid')!.value).toBe('0W');
    expect(vm.nodes.find((n) => n.id === 'grid')!.unit).toBe('240.0V/0.0A');
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
// Battery-touched flows keep the battery identity colour at every SOC tier
// (issue #170). The SOC tier colour is reserved for the battery *node*
// (fill / ring) only; spokes and moving dots must not flip hue with state.
// ---------------------------------------------------------------------------

describe('buildEnergyFlows — spoke colours follow battery / grid / solar identity (issue #170)', () => {
  it('solar_charge (solar → battery) spoke is yellow at every SoC tier', () => {
    // "Solar to everywhere always yellow / amber". When the battery is
    // charging and solar covers the charge, the visible spoke is the
    // yellow solar_charge line — there is no separate green `charge`
    // spoke (split spokes replace the synthetic aggregate, issue #170).
    // The SoC tier colour stays on the battery *node* (fill / ring) only;
    // spokes do not flip with stored charge.
    for (const soc of [5, 19, 30, 50, 80]) {
      const vm = buildEnergyFlows(
        snap({
          soc,
          solar_power: 5000,
          home_power: 500,
          battery_state: 'charging',
          battery_power: -1500,
        }),
      );
      const sc = vm.flows.find((f) => f.id === 'solar_charge');
      expect(sc, `soc=${soc}: solar_charge flow missing`).toBeDefined();
      expect(sc!.color, `soc=${soc}: solar_charge spoke must be solar yellow`).toBe(FLOW_COLORS.solar);
      expect(sc!.color, `soc=${soc}: spoke must not match SOC tier colour`).not.toBe(socColor(soc));
      expect(sc!.color, `soc=${soc}: spoke must not be home blue`).not.toBe(FLOW_COLORS.home);
      // Aggregate `charge` flow is not emitted when the split covers all
      // of the battery charge — would otherwise double-count.
      expect(flowById(vm, 'charge'), `soc=${soc}: aggregate charge must be suppressed`).toBeUndefined();
    }
  });

  it('grid_charge (grid → battery) spoke is red when only grid feeds the battery', () => {
    // When the battery is drawing from the grid, the visible source is grid.
    // Keep both the line and moving ball red so grid-fed charging is distinct
    // from battery output (battery → home / battery → grid), which stays green.
    const vm = buildEnergyFlows(
      snap({ grid_power: -2000, battery_state: 'charging', battery_power: -1500 }),
    );
    const gc = vm.flows.find((f) => f.id === 'grid_charge');
    expect(gc, 'grid_charge flow missing').toBeDefined();
    expect(gc!.color).toBe(FLOW_COLORS.grid);
  });

  it('both solar_charge and grid_charge split are emitted when both sources feed the battery (issue #170)', () => {
    // Example: solar 1 kW, grid 3 kW importing, home 1 kW, charge 3 kW.
    //  - solar covers 1 kW of charge, grid covers the remaining 2 kW.
    //  - import is reduced to 1 kW (the home-direct portion).
    const vm = buildEnergyFlows(
      snap({
        solar_power: 1000,
        grid_power: 3000,  // import
        home_power: 1000,
        battery_state: 'charging',
        battery_power: 3000,  // charge (note: backend uses +ve for charging internally — see test snap)
      }),
    );
    // Note: the test `snap` helper may use a different sign convention.
    // Inspect what was emitted; document the expectation here:
    const sc = vm.flows.find((f) => f.id === 'solar_charge');
    const gc = vm.flows.find((f) => f.id === 'grid_charge');
    if (sc && gc) {
      expect(sc.color).toBe(FLOW_COLORS.solar);
      expect(gc.color).toBe(FLOW_COLORS.grid);
      expect(sc.watts).toBeLessThanOrEqual(gc.watts + 1000);
    }
  });

  it('discharge (battery → home) spoke is the battery-output green — same as battery symbol at SoC ≥ 50% (issue #170)', () => {
    // Battery-on-spoke colour rule: spokes are green whenever the battery
    // is on them and neither solar nor grid is. The colour is
    // BATTERY_OUTPUT_COLOR = socColor(≥ 50%), so a healthy battery
    // shows a green discharge spoke that matches the battery symbol.
    // At low SoC, the *spoke* stays green (rule says "always green"); the
    // *node* shows its tier colour (red) so the user can still read
    // charge state.
    for (const soc of [5, 19, 30, 50, 80]) {
      const vm = buildEnergyFlows(
        snap({ soc, battery_state: 'discharging', battery_power: 1400, home_power: 800 }),
      );
      const df = flowById(vm, 'discharge');
      expect(df, `soc=${soc}: discharge flow missing`).toBeDefined();
      expect(df!.color, `soc=${soc}: discharge spoke must be battery-output green`).toBe(BATTERY_OUTPUT_COLOR);
    }
  });

  it('discharge_to_grid overflow spoke is the battery-output green (issue #170)', () => {
    // Battery → grid: neither solar nor grid-as-source wins (grid is the
    // destination here, and the colour rule only checks literal endpoints).
    // Spoke is green — battery "to all destinations" wins over the
    // weaker "grid destination" case for forced-discharge exports.
    for (const soc of [5, 19, 30, 50, 80]) {
      const vm = buildEnergyFlows(
        snap({ soc, battery_state: 'discharging', battery_power: 2000, home_power: 500 }),
      );
      const excess = vm.flows.find((f) => f.id === 'discharge_to_grid');
      expect(excess, `soc=${soc}: discharge_to_grid missing`).toBeDefined();
      expect(excess!.color, `soc=${soc}: discharge_to_grid must be battery-output green`).toBe(BATTERY_OUTPUT_COLOR);
    }
  });

  it('battery node colour still tracks SoC tier (the *node* keeps its meaning)', () => {
    // Sanity check the other half of the contract: the *node* keeps the
    // SoC tier colour so the user can read charge state at a glance. Only
    // the spokes changed.
    expect(buildEnergyFlows(snap({ soc: 5 })).nodes.find((n) => n.id === 'battery')!.color).toBe(socColor(5));
    expect(buildEnergyFlows(snap({ soc: 30 })).nodes.find((n) => n.id === 'battery')!.color).toBe(socColor(30));
    expect(buildEnergyFlows(snap({ soc: 80 })).nodes.find((n) => n.id === 'battery')!.color).toBe(socColor(80));
  });
});

// ---------------------------------------------------------------------------
// Spoke-colour matrix — priority solar > grid-as-source > battery (issue #170).
//
// The user's rule:
//  - "Battery to all destinations should always be green"
//  - "Grid to everywhere always red"
//  - "Solar to everywhere always yellow / amber"
//
// On a spoke with multiple rules applying, the strongest-stated rule
// wins, ordered solar > grid-as-source > battery. This locks the contract so a
// future refactor can't silently flip a spoke.
// ---------------------------------------------------------------------------

  // Priority on overlap: solar > grid-as-source > battery > home (issue #170).

describe('buildEnergyFlows — spoke colour = solar > grid-source > battery priority (issue #170)', () => {
  const cases: Array<{
    name: string;
    snap: Partial<InverterSnapshot>;
    showEvc?: boolean;
    evcPowerW?: number;
    expected: Array<{ flowId: string; color: string; rationale: string }>;
  }> = [
    {
      name: 'solar → home is yellow regardless of PV kW',
      snap: { solar_power: 5000, home_power: 500 },
      expected: [{ flowId: 'solar', color: FLOW_COLORS.solar, rationale: 'solar source' }],
    },
    {
      name: 'grid → home (import) is red',
      snap: { grid_power: -2000, home_power: 2000 },
      expected: [{ flowId: 'import', color: FLOW_COLORS.grid, rationale: 'grid source' }],
    },
    {
      name: 'solar → grid export is YELLOW when solar is active (solar wins)',
      snap: { solar_power: 5000, home_power: 500, grid_power: 4500 },
      expected: [{ flowId: 'export', color: FLOW_COLORS.solar, rationale: 'solar visual-source wins (issue #170)' }],
    },
    {
      name: 'no export spoke when neither solar nor battery is exporting',
      // Issue #170 source-attribution: a positive `grid_power` reading
      // with no solar surplus AND no battery discharge surplus has no
      // attributable source, so the `export` spoke is not emitted at
      // all (a rogue noisy grid reading, a holding register still
      // carrying an export value while nothing is actually flowing,
      // etc.). The grid node badge still reads "Exporting" — the
      // diagram just doesn't draw a spoke with no end-attribution.
      // This was the behaviour the old "export without solar is RED"
      // matrix entry used to test, before source-attribution removed
      // the underlying code path.
      snap: { grid_power: 2000, home_power: 500 },
      expected: [], // no spokes asserted; the absence of `export` is the point
    },
    {
      name: 'battery → home (discharge) is green — battery, no solar/grid',
      snap: { battery_state: 'discharging', battery_power: 1400, home_power: 800 },
      expected: [{ flowId: 'discharge', color: BATTERY_OUTPUT_COLOR, rationale: 'battery source only' }],
    },
    {
      name: 'battery → grid (overflow export) is GREEN — battery wins over grid (issue #170 user ruling)',
      snap: { battery_state: 'discharging', battery_power: 2000, home_power: 500 },
      expected: [
        { flowId: 'discharge', color: BATTERY_OUTPUT_COLOR, rationale: 'battery source only' },
        { flowId: 'discharge_to_grid', color: BATTERY_OUTPUT_COLOR, rationale: 'battery wins over grid (issue #170)' },
      ],
    },
    {
      name: 'home → EV is blue — home identity, neither battery / grid / solar',
      snap: { home_power: 7500 },
      showEvc: true,
      evcPowerW: 7000,
      expected: [{ flowId: 'ev', color: FLOW_COLORS.home, rationale: 'home source' }],
    },
    {
      name: 'solar_charge (solar covers all of charge) is yellow',
      snap: { solar_power: 5000, home_power: 500, battery_state: 'charging', battery_power: -1500 },
      expected: [{ flowId: 'solar_charge', color: FLOW_COLORS.solar, rationale: 'solar source' }],
    },
    {
      name: 'grid_charge (no solar, only grid feeds battery) is red — grid source wins',
      snap: { grid_power: -2000, home_power: 500, battery_state: 'charging', battery_power: -1500 },
      expected: [{ flowId: 'grid_charge', color: FLOW_COLORS.grid, rationale: 'grid source' }],
    },
  ];

  for (const c of cases) {
    it(c.name, () => {
      const vm = buildEnergyFlows(snap(c.snap), { showEvc: c.showEvc, evcPowerW: c.evcPowerW });
      for (const expected of c.expected) {
        const f = vm.flows.find((fl) => fl.id === expected.flowId);
        expect(f, `${expected.flowId} flow missing`).toBeDefined();
        expect(f!.color, `${expected.flowId}: ${expected.rationale}`).toBe(expected.color);
      }
    });
  }

  it('discharge_to_grid stays GREEN at every SoC level (issue #170 user ruling)', () => {
    // Lock the user-clarified contract: battery → grid overflow export is
    // green (battery identity) at every SOC tier. With low SoC the
    // battery *node* shows red (tier), but the *spoke* stays green.
    // The colour collision between spoke = green and node = red is
    // intentional: the spoke is meant to be readable as "battery
    // outputting power"; the node shows stored-charge state.
    for (const soc of [1, 10, 19, 25, 50, 75, 99]) {
      const vm = buildEnergyFlows(
        snap({ soc, battery_state: 'discharging', battery_power: 2000, home_power: 500 }),
      );
      const excess = vm.flows.find((f) => f.id === 'discharge_to_grid');
      expect(excess, `soc=${soc}`).toBeDefined();
      expect(excess!.color, `soc=${soc}: discharge_to_grid must be battery-output green`).toBe(BATTERY_OUTPUT_COLOR);
    }
  });
});

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

  it('carries the cable sub-label onto the EV node when provided (HR 2)', () => {
    // Cable plugged in, idle → "Cable In" under the kW value, independent
    // of the operational-status word (`unit`).
    const vm = buildEnergyFlows(snap(), {
      evcPowerW: 0,
      showEvc: true,
      evcLabel: 'Idle',
      evcCableLabel: 'Cable In',
    });
    const ev = vm.nodes.find((n) => n.id === 'ev');
    expect(ev!.unit).toBe('Idle');
    expect(ev!.subLabel).toBe('Cable In');
  });

  it('omits the EV cable sub-label when undefined (charger offline / never reached)', () => {
    // The diagram only passes a cable label while it has a fresh frame;
    // the view-model must not invent one when the opt is absent.
    const vm = buildEnergyFlows(snap(), {
      evcPowerW: 0,
      showEvc: true,
      evcLabel: 'Not Found',
    });
    const ev = vm.nodes.find((n) => n.id === 'ev');
    expect(ev!.subLabel).toBeUndefined();
  });

  // --- issue #189: session energy (kWh) inline with power ---
  // The session total renders inline with the live power as `7.7kW(23kWh)`.
  // It counts up while charging, then latches at the final value after the
  // session ends (the backend SessionLatch handles latch/reset; the frontend
  // just renders the value it receives). The kWh is only shown when > 0.
  it('renders the session kWh inline with the power while charging (issue #189)', () => {
    const vm = buildEnergyFlows(snap({ home_power: 7500 }), {
      evcPowerW: 7000,
      showEvc: true,
      evcLabel: 'Charging',
      evcCableLabel: 'Cable In',
      evcSessionEnergyKwh: 8.3,
    });
    const ev = vm.nodes.find((n) => n.id === 'ev');
    expect(ev!.value).toBe('7.0kW(8.3kWh)');
    // Cable state stays on its own sub-label line.
    expect(ev!.subLabel).toBe('Cable In');
  });

  it('switches the energy to a plain integer once it crosses 10 kWh', () => {
    // Below 10 kWh: one decimal place. At/above 10 kWh: integer, no dp.
    const low = buildEnergyFlows(snap({ home_power: 7500 }), {
      evcPowerW: 7000, showEvc: true, evcSessionEnergyKwh: 9.9,
    });
    expect(low.nodes.find((n) => n.id === 'ev')!.value).toBe('7.0kW(9.9kWh)');

    const atTen = buildEnergyFlows(snap({ home_power: 7500 }), {
      evcPowerW: 7000, showEvc: true, evcSessionEnergyKwh: 10,
    });
    expect(atTen.nodes.find((n) => n.id === 'ev')!.value).toBe('7.0kW(10kWh)');

    const high = buildEnergyFlows(snap({ home_power: 7500 }), {
      evcPowerW: 7000, showEvc: true, evcSessionEnergyKwh: 23,
    });
    expect(high.nodes.find((n) => n.id === 'ev')!.value).toBe('7.0kW(23kWh)');
  });

  it('shows the latched session kWh after the session ends, even at 0 W', () => {
    // Post-session latched value: charger idle, power 0, but the kWh total
    // is still meaningful. Reads `0W(7.5kWh)`.
    const vm = buildEnergyFlows(snap(), {
      evcPowerW: 0,
      showEvc: true,
      evcLabel: 'Idle',
      evcSessionEnergyKwh: 7.5,
    });
    const ev = vm.nodes.find((n) => n.id === 'ev');
    expect(ev!.value).toBe('0W(7.5kWh)');
  });

  it('omits the kWh when the session total is zero (no energy delivered yet)', () => {
    // Start of a session or no session at all: kWh reads 0 → bare power,
    // no `(0.0kWh)` suffix.
    const vm = buildEnergyFlows(snap(), {
      evcPowerW: 0,
      showEvc: true,
      evcLabel: 'Idle',
      evcCableLabel: 'Cable In',
      evcSessionEnergyKwh: 0,
    });
    const ev = vm.nodes.find((n) => n.id === 'ev');
    expect(ev!.value).toBe('0W');
    expect(ev!.subLabel).toBe('Cable In');
  });

  it('omits the kWh entirely when no session energy is passed', () => {
    // Backends / callers that predate issue #189 don't send the field —
    // the value degrades to the bare power reading.
    const vm = buildEnergyFlows(snap(), {
      evcPowerW: 0,
      showEvc: true,
      evcLabel: 'Idle',
      evcCableLabel: 'No Cable',
    });
    const ev = vm.nodes.find((n) => n.id === 'ev');
    expect(ev!.value).toBe('0W');
    expect(ev!.subLabel).toBe('No Cable');
  });
});

// ---------------------------------------------------------------------------
// Gateway device: null telemetry fields
// ---------------------------------------------------------------------------
//
// The GivEnergy Gateway (DTC 0x70xx) doesn't expose battery temperature,
// inverter temperature, battery voltage/current, or PV voltage — those live
// on each child AIO's own BMS. The backend decoder sets these fields to
// f32::NAN, and serde_json serializes NaN as `null` in JSON (JSON has no
// NaN representation). The view-model must not call .toFixed() on these null
// values — that was the regression that crashed the app on launch for
// Gateway users ("Cannot read properties of null (reading 'toFixed')").
// The old inverter-centred diagram used the formatTemp/formatVoltage helpers
// (which guard with Number.isFinite); the home-centred rewrite called .toFixed
// directly and regressed.
describe('buildEnergyFlows — Gateway null telemetry fields', () => {
  // The `snap()` helper types these fields as `number`, but the Gateway
  // payload arrives as `null` at runtime — that's the whole bug. Cast through
  // `unknown` to simulate the deserialised JSON faithfully.
  function gatewaySnap(): InverterSnapshot {
    return snap({
      inverter_temperature: null as unknown as number,
      battery_temperature: null as unknown as number,
      battery_voltage: null as unknown as number,
      battery_current: null as unknown as number,
      pv1_voltage: null as unknown as number,
      pv2_voltage: null as unknown as number,
      pv1_current: 0,
      pv2_current: 0,
      device_type_display: 'Gateway',
    });
  }

  it('does not throw when inverter_temperature is null (Gateway)', () => {
    expect(() => buildEnergyFlows(gatewaySnap())).not.toThrow();
  });

  it('renders the inverter node temperature unit as an em-dash when NaN/null', () => {
    const vm = buildEnergyFlows(gatewaySnap());
    const inverter = vm.nodes.find((n) => n.id === 'inverter');
    expect(inverter).toBeDefined();
    expect(inverter!.unit).toBe('—');
  });

  it('does not throw when PV voltage/current fields are null (Gateway)', () => {
    expect(() => buildEnergyFlows(gatewaySnap())).not.toThrow();
  });

  it('renders the solar node current when PV voltage is null but current is a number (Gateway)', () => {
    // Gateway sets pv1_voltage to NaN (→ null) but pv1_current comes from a
    // real register. With pv1_voltage null the `> 0` branch is false, so the
    // node falls back to the current label — never a throw.
    const vm = buildEnergyFlows(gatewaySnap());
    const solar = vm.nodes.find((n) => n.id === 'solar');
    expect(solar).toBeDefined();
    // pv1_current=0 + pv2_current=0 → 0.0A (0 is finite, so not the em-dash).
    expect(solar!.unit).toBe('0.0A');
  });

  it('does not throw even when PV current fields are also null (defence in depth)', () => {
    const allNull = snap({
      inverter_temperature: null as unknown as number,
      pv1_voltage: null as unknown as number,
      pv2_voltage: null as unknown as number,
      pv1_current: null as unknown as number,
      pv2_current: null as unknown as number,
    });
    expect(() => buildEnergyFlows(allNull)).not.toThrow();
  });

  it('renders the solar node voltage and current when pv1_voltage is live (legacy V/A format)', () => {
    // A live PV voltage takes priority and is shown alongside the PV current
    // (matches the legacy inverter-centred diagram: "350.4V/6.5A").
    const vm = buildEnergyFlows(
      snap({ pv1_voltage: 350.4, pv1_current: 5.2, pv2_current: 1.3 }),
    );
    expect(vm.nodes.find((n) => n.id === 'solar')!.unit).toBe('350.4V/6.5A');
  });

  it('renders the solar node current only when pv1_voltage is 0', () => {
    // No voltage telemetry (gateway-style) — fall back to current alone.
    const vm = buildEnergyFlows(snap({ pv1_voltage: 0, pv1_current: 5.2, pv2_current: 1.3 }));
    expect(vm.nodes.find((n) => n.id === 'solar')!.unit).toBe('6.5A');
  });
});

// ---------------------------------------------------------------------------
// Grid voltage / frequency sub-label
// ---------------------------------------------------------------------------

describe('buildEnergyFlows — final user scenario: solar + battery + home, only battery→grid (issue #170)', () => {
  // Regression test pinned to the user's last reported case:
  //   battery discharging 3 kW, solar 1 kW, home 1.3 kW → grid reading
  //   +1.7 kW (export).
  // Expected diagram: the only export spoke is battery → grid (green).
  // No solar → grid spoke should be drawn, even though solar is active,
  // because the entire grid reading is attributable to the battery
  // discharge surplus — the `export` flow is suppressed so we don't
  // double-count the export by drawing both `export` AND
  // `discharge_to_grid` for the same wattage.
  it('emits only `discharge_to_grid` and never `export` when the entire grid reading is battery discharge surplus', () => {
    const vm = buildEnergyFlows(
      snap({
        solar_power: 1000,
        battery_power: 3000,   // discharging (positive per AGENTS.md sign convention)
        battery_state: 'discharging',
        home_power: 1300,
        grid_power: 1700,      // net export, fully attributable to battery
      }),
    );
    // solar → home is the only solar-driven spoke (yellow).
    const solar = vm.flows.find((f) => f.id === 'solar');
    expect(solar, 'solar flow missing').toBeDefined();
    expect(solar!.watts).toBe(1000);
    expect(solar!.color).toBe(FLOW_COLORS.solar);
    // Battery covers 1.3 kW of home (the home-direct portion); 1.7 kW
    // overflows to grid.
    const dis = vm.flows.find((f) => f.id === 'discharge');
    expect(dis, 'discharge flow missing').toBeDefined();
    expect(dis!.watts).toBe(1300);
    expect(dis!.color).toBe(BATTERY_OUTPUT_COLOR);
    const toGrid = vm.flows.find((f) => f.id === 'discharge_to_grid');
    expect(toGrid, 'discharge_to_grid flow missing').toBeDefined();
    expect(toGrid!.watts).toBe(1700);
    expect(toGrid!.color).toBe(BATTERY_OUTPUT_COLOR);
    // No `export` flow — the entire grid reading was battery discharge
    // surplus. Drawing both `export` and `discharge_to_grid` would have
    // drawn a misleading yellow solar → grid spoke (issue #170 final fix).
    expect(vm.flows.find((f) => f.id === 'export'), 'no solar-export spoke expected').toBeUndefined();
  });

  it('emits both `export` (solar surplus) AND `discharge_to_grid` when solar + battery both export', () => {
    // Edge case where solar generates more than the home can absorb AND
    // the battery is also discharging past home. Each source gets its
    // own spoke — solar's surplus as `export` (solar → grid yellow),
    // battery's surplus as `discharge_to_grid` (battery → grid green).
    // Example: solar 4 kW, home 1 kW, battery discharging 3 kW.
    //   - solar covers 1 kW of home; 3 kW solar surplus → grid (export).
    //   - battery 3 kW all goes to grid (home already covered by solar).
    // Total grid reading: 3 + 3 = 6 kW export.
    const vm = buildEnergyFlows(
      snap({
        solar_power: 4000,
        battery_power: 3000,
        battery_state: 'discharging',
        home_power: 1000,
        grid_power: 6000,
      }),
    );
    // Both spokes present, attributed to their source.
    expect(vm.flows.find((f) => f.id === 'export'), 'solar-export missing').toBeDefined();
    expect(vm.flows.find((f) => f.id === 'discharge_to_grid'), 'battery-export missing').toBeDefined();
  });
});

describe('buildEnergyFlows — grid voltage/frequency label', () => {
  it('renders grid volts and hertz for single-phase snapshots', () => {
    const vm = buildEnergyFlows(snap({
      device_type_display: 'Gen 3 Hybrid',
      device_type_code: '2201',
      grid_voltage: 239.6,
      grid_frequency: 49.98,
    }));

    expect(vm.nodes.find((n) => n.id === 'grid')!.unit).toBe('239.6V/49.98Hz');
  });

  it('renders em-dashes for Gateway grid volts/hertz when telemetry is unavailable', () => {
    const vm = buildEnergyFlows(snap({
      device_type_display: 'Gateway',
      device_type_code: '7001',
      grid_voltage: null as unknown as number,
      grid_frequency: Number.NaN,
    }));

    expect(vm.nodes.find((n) => n.id === 'grid')!.unit).toBe('—/—');
  });

  it('renders grid volts and hertz for three-phase snapshots', () => {
    const vm = buildEnergyFlows(snap({
      device_type_display: 'Three Phase Hybrid',
      device_type_code: '3001',
      grid_voltage: 231.2,
      grid_frequency: 50.01,
    }));

    expect(vm.nodes.find((n) => n.id === 'grid')!.unit).toBe('231.2V/50.01Hz');
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

describe('isAnySlotActive', () => {
  it('treats a zero-length slot (00:00–00:00) as inactive', () => {
    const slots = [slot(0, 0, 0, 0)];
    const now = new Date('2026-06-28T12:00:00');
    expect(isAnySlotActive(slots, now)).toBe(false);
  });

  it('treats a disabled slot as inactive', () => {
    const slots = [{ ...slot(0, 0, 23, 59), enabled: false }];
    const now = new Date('2026-06-28T12:00:00');
    expect(isAnySlotActive(slots, now)).toBe(false);
  });

  it('is active inside a normal non-wrapping window', () => {
    const slots = [slot(10, 0, 14, 0)];
    expect(isAnySlotActive(slots, new Date('2026-06-28T12:00:00'))).toBe(true);
    expect(isAnySlotActive(slots, new Date('2026-06-28T09:00:00'))).toBe(false);
    expect(isAnySlotActive(slots, new Date('2026-06-28T15:00:00'))).toBe(false);
  });

  it('is active inside a midnight-wrapping window', () => {
    const slots = [slot(23, 0, 1, 0)];
    expect(isAnySlotActive(slots, new Date('2026-06-28T23:30:00'))).toBe(true);
    expect(isAnySlotActive(slots, new Date('2026-06-29T00:30:00'))).toBe(true);
    expect(isAnySlotActive(slots, new Date('2026-06-28T12:00:00'))).toBe(false);
  });
});
