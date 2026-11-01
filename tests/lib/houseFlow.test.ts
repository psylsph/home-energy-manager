import { describe, it, expect } from 'vitest';
import { buildEnergyFlows, FLOW_COLORS, BATTERY_OUTPUT_COLOR, socColor } from '../../src/lib/energyFlow';
import {
  LANE_GAP,
  READING_SLOTS,
  batteryBarsLit,
  buildHouseScene,
  houseStatusPill,
  splitPowerValue,
  type HouseScene,
} from '../../src/lib/houseFlow';
import { BATTERY_BARS, EV_LEG, HOUSE_ANCHORS, LEGS, isoPoint, pathD } from '../../src/lib/houseGeometry';
import type { InverterSnapshot } from '../../src/lib/types';
import type { BuildEnergyFlowsOptions } from '../../src/lib/energyFlow';

/**
 * House-view model (issue #422). The key invariant: the house draws exactly
 * the view model's flows, in the view model's colours, so it can never
 * disagree with the orbit.
 */

function snap(over: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    timestamp: 0,
    solar_power: 0, pv1_power: 0, pv2_power: 0,
    pv1_voltage: 0, pv2_voltage: 0, pv1_current: 0, pv2_current: 0,
    battery_power: 0, soc: 50, battery_voltage: 50, battery_current: 0,
    battery_state: 'idle', battery_temperature: 20, battery_capacity_kwh: 9.5,
    eps_power_w: 0, grid_power: 0, grid_voltage: 230, grid_frequency: 50,
    grid_online: true, grid_loss: false, inverter_trip: false,
    battery_over_temp: false, home_power: 0, inverter_temperature: 30,
    inverter_time: '',
    today_solar_kwh: 0, today_pv1_kwh: 0, today_pv2_kwh: 0,
    today_import_kwh: 0, today_export_kwh: 0, today_charge_kwh: 0,
    total_import_kwh: 0, total_export_kwh: 0, total_solar_kwh: 0,
    total_charge_kwh: 0, total_discharge_kwh: 0, total_throughput_kwh: 0,
    battery_mode: 'eco',
    ...over,
  } as InverterSnapshot;
}

const NOON = new Date(2026, 9, 31, 12, 0);

function scene(s: InverterSnapshot, opts: BuildEnergyFlowsOptions = {}, connected = true, gridFault = false) {
  const vm = buildEnergyFlows(s, { noiseThresholdW: 20, now: NOON, ...opts });
  const sc = buildHouseScene(vm, { connectionState: connected ? 'connected' : 'disconnected', gridFault });
  return { vm, sc };
}

const reading = (sc: HouseScene, id: string) => sc.readings.find((r) => r.id === id)!;

/** Every vm flow appears once, with the vm's colour (the cross-view invariant). */
function expectRoutesMatchFlows(vm: ReturnType<typeof buildEnergyFlows>, sc: HouseScene) {
  expect(sc.routes.map((r) => r.id).sort()).toEqual(vm.flows.map((f) => f.id).sort());
  for (const f of vm.flows) {
    const route = sc.routes.find((r) => r.id === f.id)!;
    expect(route.color).toBe(f.color);
    expect(route.watts).toBe(f.watts);
    expect(route.d.startsWith('M ')).toBe(true);
    expect(route.length).toBeGreaterThan(0);
  }
}

describe('buildHouseScene — flow states', () => {
  it('charging from the grid: grid → battery, Importing / Charging', () => {
    const { vm, sc } = scene(snap({ battery_power: -3000, battery_state: 'charging', grid_power: -3200, home_power: 200 }));
    expectRoutesMatchFlows(vm, sc);
    expect(sc.routes.map((r) => r.id)).toContain('grid_charge');
    expect(reading(sc, 'grid').status).toBe('Importing');
    expect(reading(sc, 'battery').status).toBe('Charging');
  });

  it('charging from solar: solar → battery in the solar colour, panels glow', () => {
    const { vm, sc } = scene(snap({ solar_power: 3000, pv1_power: 3000, battery_power: -2500, battery_state: 'charging', home_power: 500 }));
    expectRoutesMatchFlows(vm, sc);
    const charge = sc.routes.find((r) => r.id === 'solar_charge')!;
    expect(charge.color).toBe(FLOW_COLORS.solar);
    expect(sc.solarActive).toBe(true);
    expect(reading(sc, 'solar').status).toBeUndefined();
  });

  it('discharging: battery → home in the battery-output green', () => {
    const { vm, sc } = scene(snap({ battery_power: 2000, battery_state: 'discharging', home_power: 2000 }));
    expectRoutesMatchFlows(vm, sc);
    expect(sc.routes.find((r) => r.id === 'discharge')!.color).toBe(BATTERY_OUTPUT_COLOR);
    expect(reading(sc, 'battery').status).toBe('Discharging');
    expect(reading(sc, 'grid').status).toBe('Idle');
  });

  it('discharging to the grid: battery → grid, Exporting', () => {
    const { vm, sc } = scene(snap({ battery_power: 3000, battery_state: 'discharging', grid_power: 2800, home_power: 200 }));
    expectRoutesMatchFlows(vm, sc);
    expect(sc.routes.map((r) => r.id)).toContain('discharge_to_grid');
    expect(reading(sc, 'grid').status).toBe('Exporting');
  });

  it('importing: grid → home in the grid colour', () => {
    const { vm, sc } = scene(snap({ grid_power: -1500, home_power: 1500 }));
    expectRoutesMatchFlows(vm, sc);
    expect(sc.routes.find((r) => r.id === 'import')!.color).toBe(FLOW_COLORS.grid);
    expect(reading(sc, 'grid').status).toBe('Importing');
  });

  it('exporting a solar surplus: the export route leaves from the panels', () => {
    const { vm, sc } = scene(snap({ solar_power: 4000, pv1_power: 4000, grid_power: 3500, home_power: 500 }));
    expectRoutesMatchFlows(vm, sc);
    const exp = sc.routes.find((r) => r.id === 'export')!;
    // visualEndpoints reroutes export from solar while the panels generate,
    // so the route starts on the panels' leg (within its lane offset).
    const panelStart = isoPoint(LEGS.solar[0]);
    const [, mx, my] = /^M (\S+) (\S+)/.exec(exp.d)!;
    expect(Math.hypot(Number(mx) - panelStart.x, Number(my) - panelStart.y)).toBeLessThanOrEqual(LANE_GAP * 2);
    // …and ends at the gate.
    const gate = isoPoint(HOUSE_ANCHORS.grid);
    const [, ex, ey] = /L (\S+) (\S+)$/.exec(exp.d)!;
    expect(Math.hypot(Number(ex) - gate.x, Number(ey) - gate.y)).toBeLessThanOrEqual(LANE_GAP * 2);
    expect(reading(sc, 'grid').status).toBe('Exporting');
  });

  it('idle: no routes, Idle pills, panels dark', () => {
    const { sc } = scene(snap());
    expect(sc.routes).toEqual([]);
    expect(reading(sc, 'grid').status).toBe('Idle');
    expect(reading(sc, 'battery').status).toBe('Idle');
    expect(sc.solarActive).toBe(false);
  });

  it('sub-threshold readings draw no flow and read Idle', () => {
    const { vm, sc } = scene(snap({ solar_power: 15, pv1_power: 15, battery_power: 15, grid_power: -15, home_power: 15 }));
    expect(vm.flows).toEqual([]);
    expect(sc.routes).toEqual([]);
    expect(sc.solarActive).toBe(false);
    expect(reading(sc, 'grid').status).toBe('Idle');
    expect(reading(sc, 'battery').status).toBe('Idle');
  });

  it('scales strength by the largest flow', () => {
    const { sc } = scene(snap({ solar_power: 4000, pv1_power: 4000, battery_power: -1000, battery_state: 'charging', grid_power: 2500, home_power: 500 }));
    const max = Math.max(...sc.routes.map((r) => r.strength));
    expect(max).toBe(1);
    for (const r of sc.routes) expect(r.strength).toBeGreaterThan(0);
  });
});

describe('buildHouseScene — unroutable flows', () => {
  it('skips a flow that does not join two parts of the house', () => {
    const { vm } = scene(snap({ grid_power: -1500, home_power: 1500 }));
    const odd = {
      ...vm,
      flows: [
        ...vm.flows,
        { id: 'odd', from: 'inverter' as const, to: 'home' as const, watts: 100, direction: 'consume' as const, label: '' },
        { id: 'loop', from: 'home' as const, to: 'home' as const, watts: 100, direction: 'consume' as const, label: '' },
      ],
    };
    const sc = buildHouseScene(odd, { connectionState: 'connected', gridFault: false });
    expect(sc.routes.map((r) => r.id)).toEqual(['import']);
  });

  it('falls back to the source colour when a flow has none', () => {
    const { vm } = scene(snap({ grid_power: -1500, home_power: 1500 }));
    const bare = { ...vm, flows: vm.flows.map((f) => ({ ...f, color: undefined })) };
    const sc = buildHouseScene(bare, { connectionState: 'connected', gridFault: false });
    expect(sc.routes[0].color).toBe(FLOW_COLORS.grid);
  });

  it('copes with a view model missing nodes', () => {
    const sc = buildHouseScene({ nodes: [], flows: [], summaryText: '', maxFlowWatts: 1 }, { connectionState: 'connected', gridFault: false });
    expect(sc.readings).toEqual([]);
    expect(sc.pill.mode).toBe('—');
    expect(sc.solarActive).toBe(false);
    expect(sc.battery.barsLit).toBe(0);
  });
});

describe('buildHouseScene — lanes', () => {
  it('gives concurrent flows distinct, evenly spaced lanes', () => {
    const { sc } = scene(snap({ solar_power: 4000, pv1_power: 4000, battery_power: -1500, battery_state: 'charging', grid_power: 2000, home_power: 500 }));
    expect(sc.routes.length).toBeGreaterThanOrEqual(3);
    const paths = sc.routes.map((r) => r.d);
    expect(new Set(paths).size).toBe(paths.length);
  });

  it('centres a single flow on the wiring', () => {
    const { sc } = scene(snap({ grid_power: -1500, home_power: 1500 }));
    expect(sc.routes).toHaveLength(1);
    // A lone lane has zero offset, so it starts exactly at the gate.
    const gate = isoPoint(HOUSE_ANCHORS.grid);
    expect(sc.routes[0].d.startsWith(`M ${gate.x} ${gate.y}`)).toBe(true);
    expect(LANE_GAP).toBeGreaterThan(0);
  });
});

describe('buildHouseScene — readings', () => {
  it('shows the four readings in their slots with leaders to their anchors', () => {
    const { sc } = scene(snap({ home_power: 500, grid_power: -500 }));
    expect(sc.readings.map((r) => r.id)).toEqual(['solar', 'home', 'grid', 'battery']);
    for (const r of sc.readings) {
      const slot = READING_SLOTS[r.id as keyof typeof READING_SLOTS];
      expect(r.slot).toBe(slot);
      expect(r.leader[0]).toEqual({ x: slot.x, y: slot.edgeY });
      expect(r.leader[r.leader.length - 1]).toEqual(r.anchor);
      expect(r.anchor).toEqual(isoPoint(HOUSE_ANCHORS[r.id as keyof typeof HOUSE_ANCHORS]));
    }
  });

  it('splits the battery value and adds the SOC', () => {
    const { sc } = scene(snap({ battery_power: 3500, battery_state: 'discharging', home_power: 3500, soc: 33.5 }));
    const b = reading(sc, 'battery');
    expect(b.number).toBe('3.5');
    expect(b.unit).toBe('kW');
    expect(b.soc).toBe('34%');
    expect(b.color).toBe(socColor(33.5));
    expect(reading(sc, 'home').soc).toBeUndefined();
  });

  it('uses the view-model colours for the labels', () => {
    const { sc } = scene(snap());
    expect(reading(sc, 'solar').color).toBe(FLOW_COLORS.solar);
    expect(reading(sc, 'home').color).toBe(FLOW_COLORS.home);
    expect(reading(sc, 'grid').color).toBe(FLOW_COLORS.grid);
  });

  it('adds the EV reading, post wiring and route when a charger is configured', () => {
    const { vm, sc } = scene(snap({ home_power: 7500, grid_power: -7500 }), {
      showEvc: true, evcPowerW: 7000, evcLabel: 'Charging', evcSessionEnergyKwh: 12,
    });
    expectRoutesMatchFlows(vm, sc);
    const ev = reading(sc, 'ev');
    expect(ev.number).toBe('7.0');
    expect(ev.unit).toBe('kW');
    expect(ev.suffix).toMatch(/kWh/);
    expect(ev.status).toBe('Charging');
    expect(ev.color).toBe(FLOW_COLORS.ev);
    expect(sc.idleTracks).toContain(pathD(EV_LEG.map(isoPoint)));
    expect(sc.routes.find((r) => r.id === 'ev')).toBeDefined();
  });

  it('leaves the EV out when no charger is configured', () => {
    const { sc } = scene(snap());
    expect(sc.readings.find((r) => r.id === 'ev')).toBeUndefined();
    expect(sc.idleTracks).toHaveLength(4);
  });
});

describe('buildHouseScene — pill and battery gauge', () => {
  it('shows the full battery mode', () => {
    const { sc } = scene(snap({ battery_mode: 'timed_demand', enable_charge: true, charge_slots: [{ enabled: true, start_hour: 11, start_minute: 0, end_hour: 13, end_minute: 0, target_soc: 100 }] }));
    expect(sc.pill.mode).toBe('Timed Demand (Charging)');
    expect(sc.pill.grid).toBe('On Grid');
  });

  it('reports a grid fault and a lost connection', () => {
    expect(scene(snap(), {}, true, true).sc.pill).toMatchObject({ grid: 'Grid Offline', tone: 'fault' });
    expect(scene(snap(), {}, false).sc.pill).toMatchObject({ grid: 'Disconnected', tone: 'fault' });
  });

  it('lights SOC bars in the SOC colour', () => {
    const { sc } = scene(snap({ soc: 62 }));
    expect(sc.battery).toEqual({ barsLit: 6, color: socColor(62), fraction: 0.62 });
  });
});

describe('splitPowerValue', () => {
  it.each([
    ['3.7kW', { number: '3.7', unit: 'kW', suffix: '' }],
    ['170W', { number: '170', unit: 'W', suffix: '' }],
    ['0W', { number: '0', unit: 'W', suffix: '' }],
    ['-1.2kW', { number: '-1.2', unit: 'kW', suffix: '' }],
    ['7.7kW(23kWh)', { number: '7.7', unit: 'kW', suffix: '(23kWh)' }],
    ['—', { number: '—', unit: '', suffix: '' }],
    ['Gen3 Hybrid', { number: 'Gen3 Hybrid', unit: '', suffix: '' }],
  ])('%s', (input, expected) => {
    expect(splitPowerValue(input)).toEqual(expected);
  });
});

describe('houseStatusPill', () => {
  it('reads On Grid when connected with no fault', () => {
    expect(houseStatusPill('Eco', 'connected', false)).toEqual({ mode: 'Eco', grid: 'On Grid', tone: 'ok' });
  });
  it('prefers the connection state over a stale grid fault', () => {
    expect(houseStatusPill('Eco', 'reconnecting', true)).toEqual({ mode: 'Eco', grid: 'Reconnecting', tone: 'warn' });
    expect(houseStatusPill('Eco', 'disconnected', true)).toEqual({ mode: 'Eco', grid: 'Disconnected', tone: 'fault' });
  });
  it('shows a dash when the mode is unknown', () => {
    expect(houseStatusPill(undefined, 'connected', true)).toEqual({ mode: '—', grid: 'Grid Offline', tone: 'fault' });
  });
});

describe('batteryBarsLit', () => {
  it.each([
    [0, 0],
    [-5, 0],
    [Number.NaN, 0],
    [1, 1],
    [4, 1],
    [50, 5],
    [96, BATTERY_BARS],
    [100, BATTERY_BARS],
    [140, BATTERY_BARS],
  ])('SOC %s → %s bars', (soc, bars) => {
    expect(batteryBarsLit(soc)).toBe(bars);
  });
});
