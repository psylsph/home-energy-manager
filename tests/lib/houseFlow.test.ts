import { describe, it, expect } from 'vitest';
import { buildEnergyFlows, FLOW_COLORS, BATTERY_OUTPUT_COLOR, socColor } from '../../src/lib/energyFlow';
import {
  READING_HIT_WIDTH,
  READING_SLOTS,
  batteryBarsLit,
  buildHouseScene,
  cableLoads,
  electricalDetail,
  houseStatusPill,
  splitPowerValue,
  type HouseScene,
} from '../../src/lib/houseFlow';
import { BATTERY_BARS, CHIMNEY, EV_LEG, HOUSE_ANCHORS, HOUSE_VIEWBOX, LEGS, iso, isoPoint, pathD } from '../../src/lib/houseGeometry';
import type { InverterSnapshot } from '../../src/lib/types';
import type { BuildEnergyFlowsOptions } from '../../src/lib/energyFlow';

/**
 * House-view model (issue #422). The key invariant: the house draws one cable
 * per part through the inverter, each carrying the view model's flows for
 * that part in the colour of the largest one, so it can never disagree with
 * the orbit.
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

type Vm = ReturnType<typeof buildEnergyFlows>;
const cable = (sc: HouseScene, id: string) => sc.routes.find((r) => r.id === id);
const start = (d: string) => /^M (\S+) (\S+)/.exec(d)!.slice(1).map(Number);
const end = (d: string) => /L (\S+) (\S+)$/.exec(d)!.slice(1).map(Number);
const near = (p: number[], q: { x: number; y: number }) => Math.hypot(p[0] - q.x, p[1] - q.y) < 0.01;

/**
 * The cross-view invariant: every vm flow rides the cables of both its ends
 * (the EV flow its own cable), each cable takes the colour of its largest
 * flow, and no two cables share a path.
 */
function expectCablesMatchFlows(vm: Vm, sc: HouseScene) {
  for (const f of vm.flows) {
    const from = f.id === 'export' && vm.flows.some((g) => g.from === 'solar') ? 'solar' : f.from;
    if (f.to === 'ev') {
      expect(cable(sc, 'ev')).toBeDefined();
      continue;
    }
    expect(cable(sc, from)).toBeDefined();
    expect(cable(sc, f.to)).toBeDefined();
  }
  for (const r of sc.routes) {
    expect(r.d.startsWith('M ')).toBe(true);
    expect(r.length).toBeGreaterThan(0);
  }
  expect(new Set(sc.routes.map((r) => r.d)).size).toBe(sc.routes.length);
}

describe('buildHouseScene — flow states', () => {
  it('charging from the grid: grid → battery, Importing / Charging', () => {
    const { vm, sc } = scene(snap({ battery_power: -3000, battery_state: 'charging', grid_power: -3200, home_power: 200 }));
    expectCablesMatchFlows(vm, sc);
    expect(cable(sc, 'grid')).toMatchObject({ color: FLOW_COLORS.grid, watts: 3200, supplies: true });
    expect(cable(sc, 'battery')).toMatchObject({ color: FLOW_COLORS.grid, watts: 3000, supplies: false });
    expect(cable(sc, 'home')).toMatchObject({ color: FLOW_COLORS.grid, watts: 200, supplies: false });
    expect(reading(sc, 'grid').status).toBe('Importing');
    expect(reading(sc, 'battery').status).toBe('Charging');
  });

  it('charging from solar: solar → battery in the solar colour, panels glow', () => {
    const { vm, sc } = scene(snap({ solar_power: 3000, pv1_power: 3000, battery_power: -2500, battery_state: 'charging', home_power: 500 }));
    expectCablesMatchFlows(vm, sc);
    expect(cable(sc, 'solar')).toMatchObject({ color: FLOW_COLORS.solar, watts: 3000, supplies: true });
    expect(cable(sc, 'battery')!.color).toBe(FLOW_COLORS.solar);
    expect(sc.solarActive).toBe(true);
    expect(reading(sc, 'solar').status).toBeUndefined();
  });

  it('discharging: battery → home in the battery-output green', () => {
    const { vm, sc } = scene(snap({ battery_power: 2000, battery_state: 'discharging', home_power: 2000 }));
    expectCablesMatchFlows(vm, sc);
    expect(cable(sc, 'battery')).toMatchObject({ color: BATTERY_OUTPUT_COLOR, supplies: true });
    expect(cable(sc, 'home')).toMatchObject({ color: BATTERY_OUTPUT_COLOR, supplies: false });
    expect(cable(sc, 'grid')).toBeUndefined();
    expect(reading(sc, 'battery').status).toBe('Discharging');
    expect(reading(sc, 'grid').status).toBe('Idle');
  });

  it('discharging to the grid: battery → grid, Exporting', () => {
    const { vm, sc } = scene(snap({ battery_power: 3000, battery_state: 'discharging', grid_power: 2800, home_power: 200 }));
    expectCablesMatchFlows(vm, sc);
    expect(cable(sc, 'grid')).toMatchObject({ color: BATTERY_OUTPUT_COLOR, supplies: false });
    expect(reading(sc, 'grid').status).toBe('Exporting');
  });

  it('importing: grid → home in the grid colour', () => {
    const { vm, sc } = scene(snap({ grid_power: -1500, home_power: 1500 }));
    expectCablesMatchFlows(vm, sc);
    expect(sc.routes.map((r) => r.id)).toEqual(['home', 'grid']);
    expect(cable(sc, 'home')!.color).toBe(FLOW_COLORS.grid);
    expect(reading(sc, 'grid').status).toBe('Importing');
  });

  it('solar feeding home, battery and grid: one cable leaves the panels', () => {
    const { vm, sc } = scene(snap({
      solar_power: 5200, pv1_power: 5200, home_power: 1100, battery_power: -2300, battery_state: 'charging', grid_power: 1800,
    }));
    expectCablesMatchFlows(vm, sc);
    // Three flows leave the panels, but they share one cable carrying the total.
    expect(vm.flows.filter((f) => f.from === 'solar' || f.id === 'export').length).toBe(3);
    expect(sc.routes.filter((r) => r.id === 'solar')).toHaveLength(1);
    const solar = cable(sc, 'solar')!;
    expect(solar).toMatchObject({ color: FLOW_COLORS.solar, watts: 5200, supplies: true, strength: 1 });
    expect(near(start(solar.d), isoPoint(LEGS.solar[0]))).toBe(true);
    // The export cable runs out of the inverter to the gate.
    const grid = cable(sc, 'grid')!;
    expect(grid).toMatchObject({ color: FLOW_COLORS.solar, watts: 1800, supplies: false });
    expect(near(end(grid.d), isoPoint(HOUSE_ANCHORS.grid))).toBe(true);
    expect(reading(sc, 'grid').status).toBe('Exporting');
  });

  it('never sends a solar export out of the house when the house load is below the noise floor', () => {
    const { sc } = scene(snap({
      solar_power: 4000, pv1_power: 4000, home_power: 10, battery_power: -2000, battery_state: 'charging', grid_power: 2000,
    }));
    expect(cable(sc, 'home')).toBeUndefined();
    expect(cable(sc, 'solar')).toMatchObject({ watts: 4000, supplies: true });
    expect(cable(sc, 'grid')).toMatchObject({ watts: 2000, supplies: false, color: FLOW_COLORS.solar });
  });

  it('never shows the house supplying a charge whose source is unknown', () => {
    // Battery charging 2 kW with solar at 0 and the grid under the noise
    // floor: the view model's fallback 'charge' flow runs home → battery.
    const { vm, sc } = scene(snap({ battery_power: -2000, battery_state: 'charging', grid_power: -10, home_power: 0 }));
    expect(vm.flows.map((f) => f.id)).toEqual(['charge']);
    expect(cable(sc, 'home')).toBeUndefined();
    expect(cable(sc, 'battery')).toMatchObject({ watts: 2000, supplies: false });
  });

  it('colours a cable by its largest flow', () => {
    // Home is fed 300 W by solar and 600 W by the battery: battery green wins.
    const { sc } = scene(snap({
      solar_power: 300, pv1_power: 300, battery_power: 2500, battery_state: 'discharging', home_power: 900, grid_power: 1900,
    }));
    expect(cable(sc, 'home')!.color).toBe(BATTERY_OUTPUT_COLOR);
    expect(cable(sc, 'solar')!.color).toBe(FLOW_COLORS.solar);
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

  it('scales strength by the busiest cable', () => {
    const { sc } = scene(snap({ solar_power: 4000, pv1_power: 4000, battery_power: -1000, battery_state: 'charging', grid_power: 2500, home_power: 500 }));
    const max = Math.max(...sc.routes.map((r) => r.strength));
    expect(max).toBe(1);
    for (const r of sc.routes) expect(r.strength).toBeGreaterThan(0);
  });
});

describe('buildHouseScene — odd flows', () => {
  it('ignores a flow from a part to itself and carries one from the inverter on the part cable', () => {
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
    expect(sc.routes.map((r) => r.id)).toEqual(['home', 'grid']);
    expect(cable(sc, 'home')!.watts).toBe(1600);
  });

  it('carries a flow into the inverter on the source cable only', () => {
    const flows = [{ id: 'x', from: 'battery' as const, to: 'inverter' as const, watts: 400, direction: 'discharge' as const, label: '', color: '#333333' }];
    const sc = buildHouseScene({ nodes: [], flows, summaryText: '', maxFlowWatts: 400 }, { connectionState: 'connected', gridFault: false });
    expect(sc.routes.map((r) => r.id)).toEqual(['battery']);
    expect(cable(sc, 'battery')).toMatchObject({ watts: 400, supplies: true, color: '#333333' });
  });

  it('falls back to the source colour when a flow has none', () => {
    const { vm } = scene(snap({ grid_power: -1500, home_power: 1500 }));
    const bare = { ...vm, flows: vm.flows.map((f) => ({ ...f, color: undefined })) };
    const sc = buildHouseScene(bare, { connectionState: 'connected', gridFault: false });
    expect(sc.routes.every((r) => r.color === FLOW_COLORS.grid)).toBe(true);
  });

  it('draws no moving line on a cable whose flows cancel out', () => {
    const flows = [
      { id: 'a', from: 'battery' as const, to: 'home' as const, watts: 500, direction: 'discharge' as const, label: '', color: '#111111' },
      { id: 'b', from: 'grid' as const, to: 'battery' as const, watts: 500, direction: 'charge' as const, label: '', color: '#222222' },
    ];
    const sc = buildHouseScene({ nodes: [], flows, summaryText: '', maxFlowWatts: 500 }, { connectionState: 'connected', gridFault: false });
    expect(cable(sc, 'battery')).toBeUndefined();
    expect(cable(sc, 'home')).toMatchObject({ watts: 500, supplies: false });
    expect(cable(sc, 'grid')).toMatchObject({ watts: 500, supplies: true });
  });

  it('copes with a view model missing nodes', () => {
    const sc = buildHouseScene({ nodes: [], flows: [], summaryText: '', maxFlowWatts: 1 }, { connectionState: 'connected', gridFault: false });
    expect(sc.readings).toEqual([]);
    expect(sc.pill.mode).toBe('—');
    expect(sc.solarActive).toBe(false);
    expect(sc.battery.barsLit).toBe(0);
  });
});

describe('cableLoads', () => {
  it('puts each flow on the cables of both its ends, and the EV flow on its own', () => {
    const { vm } = scene(snap({ solar_power: 4000, pv1_power: 4000, grid_power: 1000, home_power: 3000 }), {
      showEvc: true, evcPowerW: 2000, evcLabel: 'Charging',
    });
    const loads = cableLoads(vm.flows);
    expect(loads.solar.out.map((f) => f.id).sort()).toEqual(['export', 'solar']);
    expect(loads.grid.in.map((f) => f.id)).toEqual(['export']);
    expect(loads.home.in.map((f) => f.id)).toEqual(['solar']);
    expect(loads.ev.in.map((f) => f.id)).toEqual(['ev']);
    expect(loads.home.out).toEqual([]);
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

  it("keeps each reading's tap area clear of the next reading on its row", () => {
    // Text starts 6 px in from the slot's x.
    expect(READING_SLOTS.solar.x + READING_HIT_WIDTH).toBeLessThan(READING_SLOTS.home.x + 6);
    expect(READING_SLOTS.grid.x + READING_HIT_WIDTH).toBeLessThan(READING_SLOTS.battery.x + 6);
  });

  it("keeps the Solar leader off the chimney: one vertical line down to the panels", () => {
    const { sc } = scene(snap());
    const leader = reading(sc, 'solar').leader;
    const xs: number[] = [];
    for (const x of [CHIMNEY.x0, CHIMNEY.x1]) for (const y of [CHIMNEY.y0, CHIMNEY.y1]) xs.push(iso(x, y).x);
    const [left, right] = [Math.min(...xs), Math.max(...xs)];
    for (const p of leader) expect(p.x < left || p.x > right).toBe(true);
    // Straight down onto the panel centre.
    expect(Math.abs(leader[leader.length - 1].x - leader[0].x)).toBeLessThan(0.01);
  });

  it('draws the Grid leader as one straight line', () => {
    const { sc } = scene(snap());
    const leader = reading(sc, 'grid').leader;
    expect(Math.abs(leader[2].x - leader[0].x)).toBeLessThan(0.01);
  });

  it('draws the Battery leader as one straight line, with its widest reading inside the view', () => {
    const { sc } = scene(snap());
    const leader = reading(sc, 'battery').leader;
    expect(Math.abs(leader[2].x - leader[0].x)).toBeLessThan(0.01);
    // "10.0 kW · 100%": 22 px mono digits (~13.2 px each), 12 px unit and SOC (~7.2 px each), plus the gaps.
    const widest = 6 + 4 * 13.2 + 3 + 2 * 7.2 + 4 + 6 * 7.2;
    expect(READING_SLOTS.battery.x + widest).toBeLessThanOrEqual(HOUSE_VIEWBOX.width);
  });

  it("moves Solar's share of the array to the label line, so it can't run into Home", () => {
    const arrays = [{ source: 'pv1' as const, name: '', power_w: 5200, rated_kw: 8.4 }];
    const { vm, sc } = scene(snap({ solar_power: 5200, pv1_power: 5200, home_power: 5200, solar_arrays: arrays }));
    expect(vm.nodes.find((n) => n.id === 'solar')!.value).toBe('5.2kW (62%)');
    const solar = reading(sc, 'solar');
    expect(solar).toMatchObject({ number: '5.2', unit: 'kW', suffix: '', percent: '62%' });
    // Without an array size there's no percentage.
    expect(reading(scene(snap({ solar_power: 5200, pv1_power: 5200, home_power: 5200 })).sc, 'solar').percent).toBeUndefined();
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
    expectCablesMatchFlows(vm, sc);
    const ev = reading(sc, 'ev');
    expect(ev.number).toBe('7.0');
    expect(ev.unit).toBe('kW');
    expect(ev.suffix).toMatch(/kWh/);
    expect(ev.status).toBe('Charging');
    expect(ev.color).toBe(FLOW_COLORS.ev);
    expect(sc.idleTracks).toContain(pathD(EV_LEG.map(isoPoint)));
    // Same colour as the orbit's home → EV spoke.
    expect(cable(sc, 'ev')).toMatchObject({ color: vm.flows.find((f) => f.id === 'ev')!.color, watts: 7000, supplies: false });
    expect(near(end(cable(sc, 'ev')!.d), isoPoint(EV_LEG[EV_LEG.length - 1]))).toBe(true);
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

describe('electricalDetail', () => {
  const vm = buildEnergyFlows(snap({
    solar_power: 3000, pv1_power: 3000, pv1_voltage: 350, pv1_current: 8.5, pv2_current: 0,
    battery_power: -2000, battery_state: 'charging', battery_voltage: 52.1, battery_current: -38.4,
    grid_power: 500, grid_voltage: 241.3, grid_frequency: 50.02, home_power: 500,
  }), { noiseThresholdW: 20, now: NOON });
  const node = (id: string) => vm.nodes.find((n) => n.id === id)!;

  it('gives Solar, Grid and Battery their volts and amps (or hertz), as the orbit shows them', () => {
    expect(electricalDetail(node('solar'))).toBe(node('solar').unit.split('/').join(' · '));
    expect(electricalDetail(node('solar'))).toMatch(/V · .*A$/);
    expect(electricalDetail(node('grid'))).toMatch(/V · .*Hz$/);
    expect(electricalDetail(node('battery'))).toBe(node('battery').subLabel!.split('/').join(' · '));
  });

  it('gives Home and a missing measurement nothing', () => {
    expect(electricalDetail(node('home'))).toBeUndefined();
    expect(electricalDetail({ ...node('battery'), subLabel: undefined })).toBeUndefined();
  });

  it('puts the detail on the readings', () => {
    const sc = buildHouseScene(vm, { connectionState: 'connected', gridFault: false });
    expect(reading(sc, 'solar').detail).toBe(electricalDetail(node('solar')));
    expect(reading(sc, 'home').detail).toBeUndefined();
  });
});
