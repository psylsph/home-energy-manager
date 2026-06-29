/**
 * Home-centred radial energy-flow diagram.
 *
 * Replaces the old inverter-centred `EnergyFlowDiagram`. Home sits at the
 * centre; Solar / Grid / Battery (and EV when configured) orbit it as
 * circular nodes, with animated spokes showing where energy is moving right
 * now. The inverter is demoted from "hub" to a small info line beneath the
 * diagram — it's plumbing, not the thing a homeowner cares about.
 *
 * Renders from the [`buildEnergyFlows`] view-model: sign logic, thresholding,
 * and string-building live in `src/lib/energyFlow.ts`. This component is only
 * responsible for the radial visual language.
 */
import { memo, useId } from 'react';
import { useIsMobile } from '../hooks/useIsMobile';
import { useInverterStore } from '../store/useInverterStore';
import type { InverterSnapshot } from '../lib/types';
import type { EnergyFlow, FlowNode, FlowNodeId } from '../types/energyFlow';
import { buildEnergyFlows, FLOW_COLORS, visualEndpoints } from '../lib/energyFlow';
import { evcNodeLabel } from '../lib/evcLabel';

interface Props {
  snapshot: InverterSnapshot;
  /** EV Charger active power in watts. 0 = not charging or no data. */
  evcPower?: number;
  /** Raw EV Charger charging-state string from HR 0 (issue #139). */
  evcChargingState?: string;
  /** Whether the EV Charger is actively charging. */
  evcCharging?: boolean;
  /** Whether the EV Charger is connected/responding. */
  evcConnected?: boolean;
  /** Latch: true once a valid EVC snapshot has ever arrived (issue #138). */
  evcEverConnected?: boolean;
  /** Whether EV Charger is configured (non-empty host). When falsy, EV node is hidden. */
  showEvc?: boolean;
}

// ---------------------------------------------------------------------------
// Geometry — tuned to match the reference image: a large outer orbit, a
// smaller central Home node, large circular satellites, grey spokes, and
// moving coloured dots rather than arrow-heads.
// ---------------------------------------------------------------------------

const VB = 520;
const CX = VB / 2;
const CY = 260;
const ORBIT_CY = 260;
const OUTER_ORBIT_R = 202;
const HUB_R = 48;
const SAT_R = 47;
const BATTERY_R = 50;

function satellitePositions(showEv: boolean): Partial<Record<FlowNodeId, { x: number; y: number }>> {
  if (showEv) {
    return {
      solar: { x: CX, y: 78 },
      ev: { x: 145, y: 208 },
      battery: { x: 92, y: 370 },
      grid: { x: 428, y: 370 },
    };
  }

  return {
    solar: { x: CX, y: 78 },
    battery: { x: 118, y: 370 },
    grid: { x: 402, y: 370 },
  };
}

function radiusFor(id: FlowNodeId): number {
  if (id === 'home') return HUB_R;
  if (id === 'battery') return BATTERY_R;
  return SAT_R;
}

function trimLine(
  from: { x: number; y: number },
  to: { x: number; y: number },
  fromR: number,
  toR: number,
): { x1: number; y1: number; x2: number; y2: number; mx: number; my: number } {
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const len = Math.hypot(dx, dy) || 1;
  const ux = dx / len;
  const uy = dy / len;
  const x1 = from.x + ux * fromR;
  const y1 = from.y + uy * fromR;
  const x2 = to.x - ux * toR;
  const y2 = to.y - uy * toR;
  return { x1, y1, x2, y2, mx: (x1 + x2) / 2, my: (y1 + y2) / 2 };
}

function nodeFill(id: FlowNodeId): string {
  switch (id) {
    case 'solar': return 'var(--app-flow-node-solar-bg, #302C15)';
    case 'grid': return 'var(--app-flow-node-grid-bg, #2F1F20)';
    case 'home': return 'var(--app-flow-node-home-bg, #1D2D55)';
    case 'battery': return 'var(--app-flow-node-battery-bg, #332A16)';
    case 'ev': return 'var(--app-flow-node-ev-bg, #2B1E3A)';
    case 'inverter': return 'var(--app-flow-node-inverter-bg, #132C34)';
  }
}

function displayValue(node: FlowNode): string {
  return node.value;
}

function statusFor(node: FlowNode, flows: EnergyFlow[]): string {
  if (node.id === 'solar') return node.active ? 'Generating' : 'Idle';
  if (node.id === 'grid') {
    // Status word driven by rendered flows first, falling back to the raw
    // grid_power direction. The visible `export` spoke is suppressed when
    // solar ≤ home load (issue #170 final fix), but the grid node should
    // still read "Exporting" so the user knows the *battery* is exporting
    // (or pure grid export with no solar at all).
    if (flows.some((f) => f.direction === 'import')) return 'Importing';
    if (flows.some((f) => f.direction === 'export')) return 'Exporting';
    if (flows.some((f) => f.direction === 'discharge')) return 'Exporting';
    return 'Idle';
  }
  if (node.id === 'battery') {
    if (flows.some((f) => f.direction === 'charge')) return 'Charging';
    if (flows.some((f) => f.direction === 'discharge')) return 'Discharging';
    return 'Idle';
  }
  if (node.id === 'ev') return node.unit;
  return node.unit;
}

function batterySocText(node: FlowNode): string {
  return node.unit.split(' · ')[0] ?? node.unit;
}

// ---------------------------------------------------------------------------
// Icons — compact inline SVG glyphs, avoiding emoji rendering differences.
// ---------------------------------------------------------------------------

function NodeIcon({ id, x, y, color, size = 30 }: { id: FlowNodeId; x: number; y: number; color: string; size?: number }) {
  const s = size;
  if (id === 'solar') {
    return (
      <g stroke={color} strokeWidth={3} strokeLinecap="round" fill="none">
        <circle cx={x} cy={y} r={s * 0.22} fill={color} stroke="none" />
        {Array.from({ length: 8 }).map((_, i) => {
          const a = (i * Math.PI) / 4;
          const x1 = x + Math.cos(a) * s * 0.36;
          const y1 = y + Math.sin(a) * s * 0.36;
          const x2 = x + Math.cos(a) * s * 0.50;
          const y2 = y + Math.sin(a) * s * 0.50;
          return <line key={i} x1={x1} y1={y1} x2={x2} y2={y2} />;
        })}
      </g>
    );
  }

  if (id === 'home') {
    return (
      <g fill={color}>
        <path d={`M ${x - 18} ${y - 2} L ${x} ${y - 19} L ${x + 18} ${y - 2} L ${x + 13} ${y - 2} L ${x + 13} ${y + 16} L ${x + 4} ${y + 16} L ${x + 4} ${y + 5} L ${x - 4} ${y + 5} L ${x - 4} ${y + 16} L ${x - 13} ${y + 16} L ${x - 13} ${y - 2} Z`} />
      </g>
    );
  }

  if (id === 'grid') {
    return (
      <g stroke={color} strokeWidth={2.8} strokeLinecap="round" strokeLinejoin="round" fill="none">
        <path d={`M ${x} ${y - 27} L ${x - 23} ${y + 24} H ${x + 23} Z`} />
        <path d={`M ${x} ${y - 27} V ${y + 24}`} />
        <path d={`M ${x - 18} ${y - 11} H ${x + 18}`} />
        <path d={`M ${x - 23} ${y + 3} H ${x + 23}`} />
        <path d={`M ${x - 17} ${y + 15} H ${x + 17}`} />
        <path d={`M ${x - 18} ${y - 11} L ${x + 23} ${y + 3} L ${x - 17} ${y + 15} L ${x + 23} ${y + 24}`} opacity={0.75} />
        <path d={`M ${x + 18} ${y - 11} L ${x - 23} ${y + 3} L ${x + 17} ${y + 15} L ${x - 23} ${y + 24}`} opacity={0.75} />
      </g>
    );
  }

  if (id === 'ev') {
    // Filled side-profile EV, adapted from common CC0 / Material-style
    // electric-car SVG silhouettes so it stays readable at small node size.
    return (
      <g data-testid="ev-car-icon" transform={`translate(${x-30} ${y - 23}) scale(2.55)`}>
        <path
          d="M4.2 13.2h1.1a2.4 2.4 0 0 1 4.8 0h3.8a2.4 2.4 0 0 1 4.8 0h1.1c.7 0 1.2-.5 1.2-1.2V9.5c0-.7-.4-1.3-1-1.6l-2.7-1.2-2.1-2.8c-.4-.6-1.1-.9-1.8-.9H8.7c-.8 0-1.5.4-1.9 1.1L5.1 7.2 3.5 8c-.3.2-.5.5-.5.9V12c0 .7.5 1.2 1.2 1.2Z"
          fill={color}
          opacity={0.95}
        />
        <path d="M8.8 4.8h4.3l1.7 2.2H7.6l1.2-2.2Z" fill="var(--app-bg-elevated, #21262D)" opacity={0.95} />
        <path d="M15.1 4.8 16.8 7h-2.6V4.8h.9Z" fill="var(--app-bg-elevated, #21262D)" opacity={0.95} />
        <circle cx="7.7" cy="13.2" r="1.35" fill="var(--app-bg-elevated, #21262D)" />
        <circle cx="16.3" cy="13.2" r="1.35" fill="var(--app-bg-elevated, #21262D)" />
        <path
          d="M11.8 9.1h2.1l-1.2 2.1h1.5l-2.9 3.2.8-2.4h-1.4l1.1-2.9Z"
          fill="var(--app-bg-elevated, #21262D)"
          opacity={0.95}
        />
        <path
          d="M19.5 3.5v2.4h1.4v3.2"
          fill="none"
          stroke={color}
          strokeWidth="1.2"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </g>
    );
  }

  return null;
}

// ---------------------------------------------------------------------------
// Tracks + moving flow dots
// ---------------------------------------------------------------------------

interface TrackProps {
  id: FlowNodeId;
  posOf: (id: FlowNodeId) => { x: number; y: number };
}

function StaticTrack({ id, posOf }: TrackProps) {
  const home = posOf('home');
  const sat = posOf(id);
  const { x1, y1, x2, y2 } = trimLine(home, sat, HUB_R, radiusFor(id));
  return (
    <line
      x1={x1}
      y1={y1}
      x2={x2}
      y2={y2}
      stroke="rgba(79, 84, 94, 0.55)"
      strokeWidth={5}
      strokeLinecap="round"
    />
  );
}

interface FlowPath {
  path: string;
  /** Visual path length in viewBox units. Used to normalise animation
   *  duration so short spokes and long outer arcs move at a comparable pace
   *  (issue: discharge-to-grid arc was racing at the same speed as a short
   *  battery→home spoke). */
  length: number;
  mx: number;
  my: number;
  route: 'direct' | 'outer';
  color: string;
}

function isOuterRoute(from: FlowNodeId, to: FlowNodeId): boolean {
  return from !== 'home' && to !== 'home' && from !== 'ev' && to !== 'ev';
}

function orbitAngle(pos: { x: number; y: number }): number {
  return Math.atan2(pos.y - ORBIT_CY, pos.x - CX);
}

function orbitPoint(angle: number): { x: number; y: number } {
  return {
    x: CX + OUTER_ORBIT_R * Math.cos(angle),
    y: ORBIT_CY + OUTER_ORBIT_R * Math.sin(angle),
  };
}

function clockwiseDelta(from: number, to: number): number {
  return (to - from + Math.PI * 2) % (Math.PI * 2);
}

function orbitGap(id: FlowNodeId): number {
  return Math.asin(Math.min(0.45, (radiusFor(id) + 12) / OUTER_ORBIT_R));
}

function outerArcPath(from: FlowNodeId, to: FlowNodeId, posOf: (id: FlowNodeId) => { x: number; y: number }): FlowPath {
  const fromAngle = orbitAngle(posOf(from));
  const toAngle = orbitAngle(posOf(to));
  const cw = clockwiseDelta(fromAngle, toAngle);
  const ccw = clockwiseDelta(toAngle, fromAngle);
  const sweep = cw <= ccw ? 1 : 0;
  const startAngle = sweep === 1 ? fromAngle + orbitGap(from) : fromAngle - orbitGap(from);
  const endAngle = sweep === 1 ? toAngle - orbitGap(to) : toAngle + orbitGap(to);
  const delta = sweep === 1 ? clockwiseDelta(startAngle, endAngle) : clockwiseDelta(endAngle, startAngle);
  const largeArc = delta > Math.PI ? 1 : 0;
  const start = orbitPoint(startAngle);
  const end = orbitPoint(endAngle);
  const mid = orbitPoint(sweep === 1 ? startAngle + delta / 2 : startAngle - delta / 2);
  return {
    path: `M ${start.x} ${start.y} A ${OUTER_ORBIT_R} ${OUTER_ORBIT_R} 0 ${largeArc} ${sweep} ${end.x} ${end.y}`,
    length: OUTER_ORBIT_R * delta,
    mx: mid.x,
    my: mid.y,
    route: 'outer',
    color: FLOW_COLORS[from],
  };
}

function directPath(from: FlowNodeId, to: FlowNodeId, posOf: (id: FlowNodeId) => { x: number; y: number }): FlowPath {
  const a = posOf(from);
  const b = posOf(to);
  const { x1, y1, x2, y2, mx, my } = trimLine(a, b, radiusFor(from), radiusFor(to));
  return {
    path: `M ${x1} ${y1} L ${x2} ${y2}`,
    length: Math.hypot(x2 - x1, y2 - y1),
    mx,
    my,
    route: 'direct',
    color: FLOW_COLORS[from],
  };
}

function flowPath(flow: EnergyFlow, flows: EnergyFlow[], posOf: (id: FlowNodeId) => { x: number; y: number }): FlowPath {
  const { from, to } = visualEndpoints(flow, flows);
  const routed = isOuterRoute(from, to)
    ? outerArcPath(from, to, posOf)
    : directPath(from, to, posOf);
  return { ...routed, color: flow.color ?? routed.color };
}

interface DotProps {
  flow: EnergyFlow;
  flows: EnergyFlow[];
  posOf: (id: FlowNodeId) => { x: number; y: number };
  maxW: number;
  reduced: boolean;
}

function FlowTrack({ flow, flows, posOf }: Omit<DotProps, 'maxW' | 'reduced'>) {
  const routed = flowPath(flow, flows, posOf);
  return (
    <path
      data-flow-track-id={flow.id}
      data-route={routed.route}
      d={routed.path}
      fill="none"
      stroke={routed.color}
      strokeWidth={6}
      strokeLinecap="round"
      opacity={0.34}
    />
  );
}

function FlowDot({ flow, flows, posOf, maxW, reduced }: DotProps) {
  const routed = flowPath(flow, flows, posOf);
  const strength = Math.min(1, flow.watts / maxW);
  const r = 6 + 3 * strength;
  // Duration is path-length-driven so a short battery→home spoke and a long
  // battery→grid outer arc traverse at a comparable visual pace. Higher-
  // energy flows get a modest speed boost (≈2× at full strength vs idle)
  // so a 5 kW export still feels more energetic than a 200 W trickle, but
  // neither flies across the screen. The base speed (45 px/s) was halved
  // from 90 px/s after the user asked for a calmer, slower animation in
  // the energy-flow diagram (issue #170).
  const BASE_SPEED_PX_PER_S = 45;
  const speed = BASE_SPEED_PX_PER_S * (0.55 + strength * 0.9);
  const durSeconds = Math.min(8, Math.max(1.0, routed.length / speed));
  const dur = `${durSeconds.toFixed(2)}s`;

  if (reduced) {
    return (
      <circle
        data-flow-id={flow.id}
        data-route={routed.route}
        data-duration={dur}
        cx={routed.mx}
        cy={routed.my}
        r={r}
        fill={routed.color}
        opacity={0.95}
      />
    );
  }

  return (
    <g data-flow-id={flow.id} data-route={routed.route} data-duration={dur}>
      <circle r={r} fill={routed.color} opacity={0.95}>
        <animateMotion path={routed.path} dur={dur} repeatCount="indefinite" />
      </circle>
      <circle r={r + 6} fill={routed.color} opacity={0.12}>
        <animateMotion path={routed.path} dur={dur} repeatCount="indefinite" />
      </circle>
    </g>
  );
}

// ---------------------------------------------------------------------------
// Nodes
// ---------------------------------------------------------------------------

interface NodeProps {
  node: FlowNode;
  x: number;
  y: number;
  r: number;
  flows: EnergyFlow[];
  hub?: boolean;
  mobile?: boolean;
  showStatusWords?: boolean;
}

function BatterySocRing({ node, x, y, r }: { node: FlowNode; x: number; y: number; r: number }) {
  const pct = Math.max(0, Math.min(100, node.ringPercent ?? 0));
  const ringR = r + 2;
  const circumference = 2 * Math.PI * ringR;
  const filled = circumference * (pct / 100);
  return (
    <g data-testid="battery-soc-ring">
      <circle
        data-testid="battery-soc-ring-track"
        cx={x}
        cy={y}
        r={ringR}
        fill="none"
        stroke={node.color}
        strokeOpacity={0.20}
        strokeWidth={6}
      />
      <circle
        cx={x}
        cy={y}
        r={ringR}
        fill="none"
        stroke={node.color}
        strokeWidth={6}
        strokeLinecap="round"
        strokeDasharray={`${filled} ${circumference - filled}`}
        transform={`rotate(-90 ${x} ${y})`}
      />
    </g>
  );
}

function BatteryGlyph({ node, x, y }: { node: FlowNode; x: number; y: number }) {
  const pct = Math.max(0, Math.min(100, node.ringPercent ?? 0));
  const bodyX = x - 28;
  const bodyY = y - 18;
  const bodyW = 50;
  const bodyH = 34;
  const fillW = Math.max(3, (bodyW - 8) * (pct / 100));
  return (
    <g data-testid="battery-glyph">
      <rect
        data-testid="battery-glyph-body"
        x={bodyX}
        y={bodyY}
        width={bodyW}
        height={bodyH}
        rx={7}
        fill={node.color}
        fillOpacity={0.16}
        stroke={node.color}
        strokeWidth={2.5}
      />
      <rect
        x={bodyX + bodyW}
        y={bodyY + 10}
        width={7}
        height={14}
        rx={3}
        fill={node.color}
        opacity={0.85}
      />
      <rect
        data-testid="battery-glyph-fill"
        x={bodyX + 4}
        y={bodyY + 4}
        width={fillW}
        height={bodyH - 8}
        rx={4}
        fill={node.color}
        opacity={0.55}
      />
    </g>
  );
}

function HubNode({ node, x, y, r, mobile }: NodeProps) {
  return (
    <g aria-label={`${node.label}: ${node.value} ${node.unit}`}>
      {node.active && <circle cx={x} cy={y} r={r + 9} fill={node.color} opacity={0.10} />}
      <circle data-node-body={node.id} cx={x} cy={y} r={r} fill={nodeFill(node.id)} stroke={node.color} strokeWidth={2.5} />
      <NodeIcon id="home" x={x} y={y} color={node.color} size={32} />
      <text
        x={x}
        y={y + r + 37}
        textAnchor="middle"
        fill="var(--app-text-primary, #F0F6FC)"
        fontSize={mobile ? 20 : 19}
        fontWeight={700}
        fontFamily="var(--font-mono, monospace)"
      >
        {displayValue(node)}
      </text>
    </g>
  );
}

function SatelliteNode({ node, x, y, r, flows, mobile, showStatusWords }: NodeProps) {
  const status = statusFor(node, flows);
  const value = displayValue(node);
  const isBattery = node.id === 'battery';
  const isEv = node.id === 'ev';
  // Solar shows its PV voltage/current under the kW value (legacy
  // inverter-centred diagram behaviour). Grid now mirrors that pattern with
  // voltage/frequency under the kW value; the status word still comes from
  // the flow direction below it.
  const showSubLabel = (node.id === 'solar' || node.id === 'grid') && node.unit.length > 0;

  return (
    <g aria-label={`${node.label}: ${value} ${status}`}>
      {node.active && <circle cx={x} cy={y} r={r + 10} fill={node.color} opacity={0.10} />}
      {isBattery && <BatterySocRing node={node} x={x} y={y} r={r} />}
      <circle
        data-node-body={node.id}
        cx={x}
        cy={y}
        r={r}
        fill={isBattery ? node.color : nodeFill(node.id)}
        fillOpacity={isBattery ? 0.16 : undefined}
        stroke={node.color}
        strokeWidth={2.5}
      />

      {isBattery ? (
        <>
          <BatteryGlyph node={node} x={x} y={y} />
          {/*
           * The battery node's fill is `node.color` (the SOC tier colour)
           * at ~16 % opacity, so drawing the SoC text in the same
           * `node.color` produces zero contrast — text and background
           * share the same hue. Use the fixed primary text colour instead
           * so the % is legible at every SOC tier (issue #170).
           * The SoC tier colour is still preserved on the ring + glyph
           * fill so the colour-coded state of the node itself stays
           * meaningful.
           */}
          <text
            x={x - 2}
            y={y + 7}
            textAnchor="middle"
            fill="var(--app-text-primary, #F0F6FC)"
            fontSize={mobile ? 18 : 17}
            fontWeight={800}
            fontFamily="var(--font-mono, monospace)"
          >
            {batterySocText(node)}
          </text>
        </>
      ) : (
        <NodeIcon id={node.id} x={x} y={y} color={node.color} size={isEv ? 34 : 32} />
      )}

      <text
        x={x}
        y={y + r + 32}
        textAnchor="middle"
        fill="var(--app-text-primary, #F0F6FC)"
        fontSize={mobile ? 20 : 19}
        fontWeight={700}
        fontFamily="var(--font-mono, monospace)"
      >
        {value}
      </text>
      {showSubLabel && (
        <text
          data-testid={`${node.id}-sublabel`}
          x={x}
          y={y + r + 50}
          textAnchor="middle"
          fill="var(--app-text-primary, #F0F6FC)"
          fontSize={mobile ? 14 : 13}
          fontFamily="var(--font-mono, monospace)"
        >
          {node.unit}
        </text>
      )}
      {showStatusWords && (
        <text
          x={x}
          y={y + r + 72}
          textAnchor="middle"
          fill="var(--app-text-secondary, #8B949E)"
          fontSize={mobile ? 15 : 14}
          fontFamily="var(--font-sans, sans-serif)"
        >
          {status}
        </text>
      )}
    </g>
  );
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

function EnergyOrbitDiagramInner({
  snapshot,
  evcPower = 0,
  evcChargingState = '',
  evcCharging = false,
  evcConnected = false,
  evcEverConnected,
  showEvc = false,
}: Props) {
  const mobile = useIsMobile();
  const orbitMaskId = useId().replace(/:/g, '');
  const showFlowStatusWords = useInverterStore((st) => st.showFlowStatusWords);
  const noise = useInverterStore((st) => st.visualNoiseThreshold);
  // Reduced-motion users get static flow dots (no SMIL animation).
  const reduced =
    typeof window !== 'undefined' &&
    typeof window.matchMedia === 'function' &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  const evcLabel = showEvc
    ? evcNodeLabel(evcCharging, evcConnected, !!evcEverConnected, evcChargingState)
    : undefined;

  const vm = buildEnergyFlows(snapshot, {
    noiseThresholdW: noise,
    evcPowerW: evcPower,
    showEvc,
    evcLabel,
  });

  const positions = satellitePositions(showEvc);
  const posOf = (id: FlowNodeId): { x: number; y: number } =>
    id === 'home' ? { x: CX, y: CY } : (positions[id] ?? { x: CX, y: CY });

  const nodeById = (id: FlowNodeId): FlowNode | undefined =>
    vm.nodes.find((n) => n.id === id);
  const home = nodeById('home')!;
  const battery = nodeById('battery');
  const inverter = nodeById('inverter');
  const batteryMode = battery?.unit.split(' · ')[1] ?? '—';

  const satelliteIds: FlowNodeId[] = showEvc
    ? ['solar', 'ev', 'battery', 'grid']
    : ['solar', 'battery', 'grid'];
  const viewBoxHeight = showFlowStatusWords ? VB : 480;

  return (
    <div className="flex flex-col items-center">
      <div className="flex justify-center w-full">
        <svg
          viewBox={`0 0 ${VB} ${viewBoxHeight}`}
          className="w-full"
          style={{ maxWidth: '520px', fontFamily: 'var(--font-sans, sans-serif)' }}
          role="img"
          aria-label={`Energy flow. ${vm.summaryText}`}
        >
          <defs>
            <mask id={orbitMaskId}>
              <rect x="0" y="0" width={VB} height={VB} fill="white" />
              {satelliteIds.map((id) => {
                const p = posOf(id);
                return <circle key={id} cx={p.x} cy={p.y} r={radiusFor(id) + 13} fill="black" />;
              })}
            </mask>
          </defs>

          {/* Large outer orbit ring from the reference image, with gaps where
              the node symbols sit so it never visibly runs through them. */}
          <circle
            data-testid="energy-orbit-ring"
            cx={CX}
            cy={ORBIT_CY}
            r={OUTER_ORBIT_R}
            fill="none"
            stroke="rgba(79, 84, 94, 0.28)"
            strokeWidth={6}
            mask={`url(#${orbitMaskId})`}
          />

          {/* Fixed grey tracks from Home to each visible satellite. */}
          {satelliteIds.map((id) => (
            <StaticTrack key={id} id={id} posOf={posOf} />
          ))}

          {/* Source-coloured active tracks sit on top of the idle grey rails. */}
          {vm.flows.map((f) => (
            <FlowTrack
              key={`track-${f.id}`}
              flow={f}
              flows={vm.flows}
              posOf={posOf}
            />
          ))}

          {/* Moving dots show active flow direction and strength. */}
          {vm.flows.map((f) => (
            <FlowDot
              key={`dot-${f.id}`}
              flow={f}
              flows={vm.flows}
              posOf={posOf}
              maxW={vm.maxFlowWatts}
              reduced={reduced}
            />
          ))}

          <HubNode node={home} x={CX} y={CY} r={HUB_R} flows={vm.flows} hub mobile={mobile} />

          {satelliteIds.map((id) => {
            const node = nodeById(id);
            if (!node) return null;
            const p = posOf(id);
            return (
              <SatelliteNode
                key={id}
                node={node}
                x={p.x}
                y={p.y}
                r={radiusFor(id)}
                flows={vm.flows}
                mobile={mobile}
                showStatusWords={showFlowStatusWords}
              />
            );
          })}

          {/* Inverter mini-card — rendered inside the SVG, below the home
              hub's kW value and below the battery/grid satellites along the
              orbit ring, so the diagram card collapses to just the SVG
              height. Each item (model · temperature · battery mode) sits
              on its own line. */}
          {inverter && (
            <g
              data-testid="inverter-mini-card"
              aria-label={`Inverter: ${inverter.value}, ${inverter.unit}, ${batteryMode}`}
            >
              {/* Translucent pill background so the chip stays readable over
                  spokes and flow dots. Sits inside the outer orbit ring,
                  at the bottom of the SVG below the battery/grid satellites.
                  Width is sized for the longest realistic battery-mode label
                  "Timed Demand (Discharging)" (25 chars at fontSize 11
                  sans-serif ≈ ~140 px) plus a small horizontal margin on
                  each side. Without the widen the third row clipped
                  against the pill border on AC-coupled inverters whose
                  battery mode appended "(Discharging)". */}
              <rect
                x={CX - 84}
                y={385}
                width={168}
                height={48}
                rx={10}
                fill="var(--app-bg-surface, #161B22)"
                fillOpacity={0.82}
                stroke="rgba(255,255,255,0.08)"
                strokeWidth={1}
              />
              <text
                x={CX}
                y={399}
                textAnchor="middle"
                fill="var(--app-text-primary, #F0F6FC)"
                fontSize={13}
                fontWeight={600}
                fontFamily="var(--font-sans, sans-serif)"
              >
                {inverter.value}
              </text>
              <text
                x={CX}
                y={415}
                textAnchor="middle"
                fill="var(--app-text-secondary, #8B949E)"
                fontSize={11}
                fontWeight={600}
                fontFamily="var(--font-sans, sans-serif)"
              >
                {inverter.unit}
              </text>
              <text
                x={CX}
                y={429}
                textAnchor="middle"
                fill="var(--app-text-secondary, #8B949E)"
                fontSize={11}
                fontWeight={600}
                fontFamily="var(--font-sans, sans-serif)"
              >
                {batteryMode}
              </text>
            </g>
          )}
        </svg>
      </div>
    </div>
  );
}

const EnergyOrbitDiagram = memo(EnergyOrbitDiagramInner);
export default EnergyOrbitDiagram;
