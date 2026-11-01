/**
 * House-view model (issue #422).
 *
 * Maps the shared [`EnergyFlowViewModel`](../types/energyFlow.ts) onto the
 * illustrated house: one cable per part (solar, battery, home, grid, EV)
 * through the inverter, readings with leader lines, the top status pill and
 * the battery gauge. Pure — the component only draws what
 * this returns. Colours, status words, numbers and the noise threshold all
 * come from the view model, so the house and the orbit always agree.
 */
import type { ConnectionState } from './types';
import type { EnergyFlow, EnergyFlowViewModel, FlowNode, FlowNodeId } from '../types/energyFlow';
import { FLOW_COLORS, batteryFillFraction, nodeStatus, socColor, visualEndpoints } from './energyFlow';
import {
  BATTERY_BARS,
  EV_LEG,
  HOUSE_ANCHORS,
  LEGS,
  isoPoint,
  pathD,
  polylineLength,
  type HousePart,
  type ScreenPoint,
  type WorldPoint,
} from './houseGeometry';

// ---------------------------------------------------------------------------
// Layout (SVG user units, inside HOUSE_VIEWBOX)
// ---------------------------------------------------------------------------

interface ReadingSlot {
  /** Left edge of the reading text; the leader line drops from here. */
  x: number;
  /** Baseline of the big value. */
  valueY: number;
  /** Where the leader leaves the reading: above (top row) or below (bottom row) the text. */
  edgeY: number;
}

/** Screen x of an anchor, rounded to the SVG's 0.01 grid. */
const anchorX = (id: keyof typeof HOUSE_ANCHORS) => isoPoint(HOUSE_ANCHORS[id]).x;

/** Top row: Solar and Home. Bottom row: Grid and Battery. EV sits under the weather. */
export const READING_SLOTS: Record<'solar' | 'home' | 'grid' | 'battery' | 'ev', ReadingSlot> = {
  // Solar sits straight above the panel centre, so its leader is one vertical
  // line clear of the chimney; Home moves right to make room.
  solar: { x: anchorX('solar'), valueY: 70, edgeY: 94 },
  home: { x: 302, valueY: 70, edgeY: 94 },
  // Low enough to leave a gap under the carbon intensity line (issue #401).
  ev: { x: 24, valueY: 142, edgeY: 194 },
  // Bottom readings sit close under their anchors so the leaders stay short
  // and don't cross the gable wall: Grid straight below the gate.
  grid: { x: anchorX('grid'), valueY: 446, edgeY: 424 },
  // Straight below the cabinet's front foot: a single vertical leader.
  battery: { x: anchorX('battery'), valueY: 446, edgeY: 424 },
};

/**
 * Width of a reading's tap area from its slot's x: under the gap to the next
 * reading's text on the same row, so a tap on Home never opens Solar's tooltip.
 */
export const READING_HIT_WIDTH = 100;

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

export interface HouseReading {
  id: FlowNodeId;
  /** Title-case label from the view model (drawn upper-case by CSS). */
  label: string;
  /** Big number, e.g. `"3.7"`. */
  number: string;
  /** Unit after the number, e.g. `"kW"`. */
  unit: string;
  /** Anything after the unit in the formatted value, e.g. EV session energy `"(23kWh)"`. */
  suffix: string;
  /** Battery only: SOC text shown after the power, e.g. `"34%"`. */
  soc?: string;
  /**
   * Solar only: share of the array size, e.g. `"62%"`, shown on the label
   * line — after the power it would run into the Home reading.
   */
  percent?: string;
  /** Status pill text (Grid, Battery, EV). */
  status?: string;
  /** Volts / amps (or hertz) for the tooltip, e.g. `"350V · 12.3A"`; Solar, Grid and Battery only. */
  detail?: string;
  color: string;
  active: boolean;
  slot: ReadingSlot;
  /** Leader polyline from the reading to its anchor on the house. */
  leader: ScreenPoint[];
  anchor: ScreenPoint;
}

export type HouseCable = HousePart | 'ev';

export interface HouseRoute {
  /** The cable: one per part. */
  id: HouseCable;
  /** Colour of the largest flow the cable carries. */
  color: string;
  /** Net power along the cable. */
  watts: number;
  /**
   * True when the part supplies power: it runs from the part towards the
   * inverter. False when the part receives it (for the EV, from the house).
   * Drawn as `data-supplies` on the route.
   */
  supplies: boolean;
  /** 0–1 share of the busiest cable; drives line weight and animation speed. */
  strength: number;
  d: string;
  length: number;
}

export type GridTone = 'ok' | 'warn' | 'fault';

export interface HouseStatusPill {
  /** Full battery mode label, never truncated. */
  mode: string;
  grid: string;
  tone: GridTone;
}

export interface HouseScene {
  readings: HouseReading[];
  routes: HouseRoute[];
  /** Grey wiring drawn under the active cables. */
  idleTracks: string[];
  pill: HouseStatusPill;
  solarActive: boolean;
  /** Battery cabinet gauge. */
  battery: { barsLit: number; color: string; fraction: number };
}

export interface BuildHouseSceneOptions {
  connectionState: ConnectionState;
  gridFault: boolean;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Split a formatted power such as `"3.7kW"`, `"170W"` or `"7.7kW(23kWh)"`
 * into number, unit and suffix so the number can be drawn large. Anything
 * that isn't a power (e.g. `"—"`) comes back whole as the number.
 */
export function splitPowerValue(value: string): { number: string; unit: string; suffix: string } {
  const m = /^(-?[\d.,]+)\s*(kW|W)(.*)$/.exec(value.trim());
  if (!m) return { number: value, unit: '', suffix: '' };
  return { number: m[1], unit: m[2], suffix: m[3] };
}

/** The top pill: battery mode on the left, connection / grid state on the right. */
export function houseStatusPill(
  mode: string | undefined,
  connectionState: ConnectionState,
  gridFault: boolean,
): HouseStatusPill {
  const m = mode ?? '—';
  if (connectionState === 'reconnecting') return { mode: m, grid: 'Reconnecting', tone: 'warn' };
  if (connectionState !== 'connected') return { mode: m, grid: 'Disconnected', tone: 'fault' };
  if (gridFault) return { mode: m, grid: 'Grid Offline', tone: 'fault' };
  return { mode: m, grid: 'On Grid', tone: 'ok' };
}

/** Number of lit SOC bars on the battery cabinet (rounded, any lit bar shown from 1%). */
export function batteryBarsLit(soc: number): number {
  const f = batteryFillFraction(soc);
  if (f <= 0) return 0;
  return Math.max(1, Math.round(f * BATTERY_BARS));
}

function project(points: WorldPoint[]): ScreenPoint[] {
  return points.map(isoPoint);
}

function isHousePart(id: FlowNodeId): id is HousePart {
  return id === 'solar' || id === 'battery' || id === 'home' || id === 'grid';
}

interface CableLoad {
  /** Flows leaving the part (towards the inverter). */
  out: EnergyFlow[];
  /** Flows arriving at the part (from the inverter). */
  in: EnergyFlow[];
}

const sum = (flows: EnergyFlow[]) => flows.reduce((t, f) => t + f.watts, 0);

/**
 * Which flows each cable carries. A flow from A to B runs along A's cable
 * into the inverter and out along B's; the EV flow has its own cable from
 * the house, so it isn't counted on the home cable.
 */
export function cableLoads(flows: EnergyFlow[]): Record<HouseCable, CableLoad> {
  const loads: Record<HouseCable, CableLoad> = {
    solar: { out: [], in: [] },
    battery: { out: [], in: [] },
    home: { out: [], in: [] },
    grid: { out: [], in: [] },
    ev: { out: [], in: [] },
  };
  for (const flow of flows) {
    const { from, to } = visualEndpoints(flow);
    if (from === to) continue;
    if (to === 'ev') {
      loads.ev.in.push(flow);
      continue;
    }
    // The house never supplies power. The view model's fallback 'charge'
    // (home → battery, for a charge with no recognised source) comes from
    // the inverter, so it isn't counted as leaving the home cable.
    if (isHousePart(from) && from !== 'home') loads[from].out.push(flow);
    if (isHousePart(to)) loads[to].in.push(flow);
  }
  return loads;
}

function cableColor(flows: EnergyFlow[]): string {
  const top = flows.reduce((a, b) => (b.watts > a.watts ? b : a));
  return top.color ?? FLOW_COLORS[visualEndpoints(top).from];
}

function leaderFor(slot: ReadingSlot, anchor: ScreenPoint): ScreenPoint[] {
  return [
    { x: slot.x, y: slot.edgeY },
    { x: slot.x, y: anchor.y },
    { x: anchor.x, y: anchor.y },
  ];
}

/**
 * The electrical detail behind a reading, from the same view model the orbit
 * uses: PV volts/amps for Solar, grid volts with amps (from a grid CT) or
 * hertz for Grid, battery volts/amps for Battery. `undefined` when there is
 * none (Home, EV, or a missing measurement).
 */
export function electricalDetail(node: FlowNode): string | undefined {
  const raw = node.id === 'battery' ? node.subLabel : node.id === 'solar' || node.id === 'grid' ? node.unit : undefined;
  return raw ? raw.split('/').join(' · ') : undefined;
}

function reading(node: FlowNode, withStatus: boolean): HouseReading {
  const id = node.id as keyof typeof READING_SLOTS;
  const slot = READING_SLOTS[id];
  const anchor = isoPoint(HOUSE_ANCHORS[id]);
  const { number, unit, suffix } = splitPowerValue(node.value);
  // Solar's "(62%)" moves to the label line (`percent`); only the EV's
  // session energy stays after the unit.
  return {
    id: node.id,
    label: node.label,
    number,
    unit,
    suffix: node.percentText ? '' : suffix,
    soc: node.id === 'battery' ? node.socText : undefined,
    percent: node.percentText,
    status: withStatus ? nodeStatus(node) : undefined,
    detail: electricalDetail(node),
    color: node.color,
    active: node.active,
    slot,
    leader: leaderFor(slot, anchor),
    anchor,
  };
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

export function buildHouseScene(vm: EnergyFlowViewModel, opts: BuildHouseSceneOptions): HouseScene {
  const byId = (id: FlowNodeId) => vm.nodes.find((n) => n.id === id);
  const battery = byId('battery');
  const solar = byId('solar');
  const ev = byId('ev');

  const readings: HouseReading[] = [];
  for (const id of ['solar', 'home', 'grid', 'battery'] as const) {
    const node = byId(id);
    if (node) readings.push(reading(node, id === 'grid' || id === 'battery'));
  }
  if (ev) readings.push(reading(ev, true));

  // One cable per part. Its direction and power are the part's net flow;
  // its colour is that of the largest flow it carries.
  const loads = cableLoads(vm.flows);
  const cables = (['solar', 'battery', 'home', 'grid', 'ev'] as const).flatMap((id) => {
    const { out, in: inbound } = loads[id];
    const net = sum(out) - sum(inbound);
    // No net power, no moving line: the idle wiring still shows the cable.
    if (net === 0) return [];
    const supplies = net > 0;
    // Never empty: net power towards the inverter needs a flow leaving the
    // part, and away from it a flow arriving.
    const carried = supplies ? out : inbound;
    const world = id === 'ev' ? EV_LEG : supplies ? LEGS[id] : [...LEGS[id]].reverse();
    return [{ id, world, watts: Math.abs(net), supplies, color: cableColor(carried) }];
  });
  const busiest = Math.max(...cables.map((c) => c.watts), 1);
  const routes: HouseRoute[] = cables.map(({ id, world, watts, supplies, color }) => {
    const screen = project(world);
    return {
      id,
      color,
      watts,
      supplies,
      strength: Math.min(1, watts / busiest),
      d: pathD(screen),
      length: polylineLength(screen),
    };
  });

  const idleTracks = (Object.keys(LEGS) as HousePart[]).map((part) => pathD(project(LEGS[part])));
  if (ev) idleTracks.push(pathD(project(EV_LEG)));

  const soc = battery?.ringPercent ?? 0;
  return {
    readings,
    routes,
    idleTracks,
    pill: houseStatusPill(battery?.mode, opts.connectionState, opts.gridFault),
    solarActive: !!solar?.active,
    battery: { barsLit: batteryBarsLit(soc), color: socColor(soc), fraction: batteryFillFraction(soc) },
  };
}
