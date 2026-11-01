/**
 * House-view model (issue #422).
 *
 * Maps the shared [`EnergyFlowViewModel`](../types/energyFlow.ts) onto the
 * illustrated house: one route per flow, readings with leader lines, the top
 * status pill and the battery gauge. Pure — the component only draws what
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
  offsetPolyline,
  pathD,
  polylineLength,
  routeBetween,
  type HousePart,
  type ScreenPoint,
  type WorldPoint,
} from './houseGeometry';

// ---------------------------------------------------------------------------
// Layout (SVG user units, inside HOUSE_VIEWBOX)
// ---------------------------------------------------------------------------

/** Gap between parallel lanes where flows share wiring. */
export const LANE_GAP = 3.5;

interface ReadingSlot {
  /** Left edge of the reading text; the leader line drops from here. */
  x: number;
  /** Baseline of the big value. */
  valueY: number;
  /** Where the leader leaves the reading: above (top row) or below (bottom row) the text. */
  edgeY: number;
}

/** Top row: Solar and Home. Bottom row: Grid and Battery. EV sits under the weather. */
export const READING_SLOTS: Record<'solar' | 'home' | 'grid' | 'battery' | 'ev', ReadingSlot> = {
  solar: { x: 180, valueY: 70, edgeY: 94 },
  home: { x: 286, valueY: 70, edgeY: 94 },
  ev: { x: 24, valueY: 128, edgeY: 180 },
  // Bottom readings sit close under their anchors so the leaders stay short
  // and don't cross the gable wall.
  grid: { x: 100, valueY: 446, edgeY: 424 },
  battery: { x: 246, valueY: 446, edgeY: 424 },
};

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
  /** Status pill text (Grid, Battery, EV). */
  status?: string;
  color: string;
  active: boolean;
  slot: ReadingSlot;
  /** Leader polyline from the reading to its anchor on the house. */
  leader: ScreenPoint[];
  anchor: ScreenPoint;
}

export interface HouseRoute {
  id: string;
  color: string;
  watts: number;
  /** 0–1 share of the largest flow; drives line weight and animation speed. */
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
  /** Grey wiring drawn under the active routes. */
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

function worldRoute(flow: EnergyFlow, flows: EnergyFlow[]): WorldPoint[] | null {
  const { from, to } = visualEndpoints(flow, flows);
  if (to === 'ev') return EV_LEG;
  if (isHousePart(from) && isHousePart(to) && from !== to) return routeBetween(from, to);
  return null;
}

function isHousePart(id: FlowNodeId): id is HousePart {
  return id === 'solar' || id === 'battery' || id === 'home' || id === 'grid';
}

function leaderFor(slot: ReadingSlot, anchor: ScreenPoint): ScreenPoint[] {
  return [
    { x: slot.x, y: slot.edgeY },
    { x: slot.x, y: anchor.y },
    { x: anchor.x, y: anchor.y },
  ];
}

function reading(node: FlowNode, withStatus: boolean): HouseReading {
  const id = node.id as keyof typeof READING_SLOTS;
  const slot = READING_SLOTS[id];
  const anchor = isoPoint(HOUSE_ANCHORS[id]);
  const { number, unit, suffix } = splitPowerValue(node.value);
  return {
    id: node.id,
    label: node.label,
    number,
    unit,
    suffix,
    soc: node.id === 'battery' ? node.socText : undefined,
    status: withStatus ? nodeStatus(node) : undefined,
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

  // One lane per flow, centred on the wiring, so flows that share a stretch
  // of wall sit side by side instead of hiding each other.
  const routed = vm.flows
    .map((flow) => ({ flow, world: worldRoute(flow, vm.flows) }))
    .filter((r): r is { flow: EnergyFlow; world: WorldPoint[] } => r.world !== null);
  const routes: HouseRoute[] = routed.map(({ flow, world }, i) => {
    const offset = (i - (routed.length - 1) / 2) * LANE_GAP;
    const screen = offsetPolyline(project(world), offset);
    const { from } = visualEndpoints(flow, vm.flows);
    return {
      id: flow.id,
      color: flow.color ?? FLOW_COLORS[from],
      watts: flow.watts,
      strength: Math.min(1, flow.watts / vm.maxFlowWatts),
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
