/**
 * Home-centred radial energy-flow diagram.
 *
 * Replaces the old inverter-centred `EnergyFlowDiagram`. Home sits at the
 * centre; Solar / Grid / Battery (and EV when configured) orbit it as
 * circular nodes, with animated spokes showing where energy is moving right
 * now. The inverter is demoted from "hub" to a small info line beneath the
 * diagram — it's plumbing, not the thing a homeowner cares about.
 *
 * Renders purely from the [`buildEnergyFlows`] view-model: no sign logic, no
 * noise-threshold branching, no string-building here. See
 * `src/lib/energyFlow.ts` for the conventions (battery +ve = discharge,
 * grid +ve = export, home-centred topology).
 *
 * ## Layout balance with/without EV
 *
 * Node positions are computed from the *visible* node set, not a fixed map,
 * so removing the optional EV node doesn't leave a hole on the left:
 *  - **With EV** (4 satellites): Solar top, Grid right, Battery bottom, EV
 *    left — a symmetric cross.
 *  - **Without EV** (3 satellites): Solar top, Grid lower-right, Battery
 *    lower-left — a balanced triangle with its base spread across the
 *    bottom, so no single quadrant reads as empty.
 */

import { memo } from 'react';
import { useIsMobile } from '../hooks/useIsMobile';
import { useInverterStore } from '../store/useInverterStore';
import type { InverterSnapshot } from '../lib/types';
import type { EnergyFlow, FlowNode, FlowNodeId } from '../types/energyFlow';
import { buildEnergyFlows, FLOW_COLORS } from '../lib/energyFlow';
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
// Geometry — a 460×460 viewBox, Home hub at centre, satellites on an orbit.
// ---------------------------------------------------------------------------

const VB = 460;
const CX = VB / 2;
const CY = VB / 2;
const ORBIT_R = 158; // satellite centre distance from Home centre
const HUB_R = 74;    // Home node radius
const SAT_R = 58;    // satellite node radius

/** Pixel position for a satellite at a given clock angle (deg, 0=right, 90=top). */
function satPos(angleDeg: number): { x: number; y: number } {
  const r = (angleDeg * Math.PI) / 180;
  return { x: CX + ORBIT_R * Math.cos(r), y: CY - ORBIT_R * Math.sin(r) };
}

/**
 * Satellite positions keyed by node id, chosen for visual balance whether or
 * not the optional EV node is shown. See file header.
 */
function satellitePositions(showEv: boolean): Partial<Record<FlowNodeId, { x: number; y: number }>> {
  if (showEv) {
    // Symmetric cross: Solar 12, Grid 3, Battery 6, EV 9.
    return {
      solar: satPos(90),
      grid: satPos(0),
      battery: satPos(270),
      ev: satPos(180),
    };
  }
  // Balanced triangle: Solar 12, Grid ~5 (lower-right), Battery ~7 (lower-left).
  return {
    solar: satPos(90),
    grid: satPos(-40),
    battery: satPos(220),
  };
}

// ---------------------------------------------------------------------------
// Flow spoke (animated, directional)
// ---------------------------------------------------------------------------

interface SpokeProps {
  flow: EnergyFlow;
  posOf: (id: FlowNodeId) => { x: number; y: number };
  maxW: number;
  reduced: boolean;
}

function FlowSpoke({ flow, posOf, maxW, reduced }: SpokeProps) {
  const a = posOf(flow.from);
  const b = posOf(flow.to);
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const len = Math.hypot(dx, dy) || 1;
  const ux = dx / len;
  const uy = dy / len;
  // Trim each end by its node radius so the spoke starts/ends at the edges.
  const r1 = flow.from === 'home' ? HUB_R : SAT_R;
  const r2 = flow.to === 'home' ? HUB_R : SAT_R;
  const x1 = a.x + ux * r1;
  const y1 = a.y + uy * r1;
  const x2 = b.x - ux * r2;
  const y2 = b.y - uy * r2;
  // Stroke width scales with this flow's share of the largest active flow.
  const sw = 2 + 4 * Math.min(1, flow.watts / maxW);
  const mx = (x1 + x2) / 2;
  const my = (y1 + y2) / 2;
  const angle = (Math.atan2(dy, dx) * 180) / Math.PI;
  // Colour by the source node so the spoke reads as "from X".
  const color = FLOW_COLORS[flow.from];

  return (
    <g>
      {/* Faint track behind the animated line */}
      <line x1={x1} y1={y1} x2={x2} y2={y2} stroke="#21262D" strokeWidth={2} strokeLinecap="round" />
      {/* Animated flow — dashes move in the from→to direction */}
      <line
        x1={x1} y1={y1} x2={x2} y2={y2}
        stroke={color}
        strokeWidth={sw}
        strokeLinecap="round"
        strokeDasharray="9 7"
        opacity={0.9}
      >
        {!reduced && (
          <animate attributeName="stroke-dashoffset" from="0" to="-32" dur="0.8s" repeatCount="indefinite" />
        )}
      </line>
      {/* Direction arrow at the midpoint */}
      <polygon
        points="0,-7 14,0 0,7"
        fill={color}
        transform={`translate(${mx},${my}) rotate(${angle})`}
      />
    </g>
  );
}

// ---------------------------------------------------------------------------
// Node
// ---------------------------------------------------------------------------

interface NodeProps {
  node: FlowNode;
  x: number;
  y: number;
  r: number;
  hub?: boolean;
  mobile?: boolean;
}

function NodeCircle({ node, x, y, r, hub, mobile }: NodeProps) {
  return (
    <g>
      {/* Active glow */}
      {node.active && (
        <circle cx={x} cy={y} r={r + 5} fill="none" stroke={node.color} strokeWidth={1} opacity={0.18} />
      )}
      {/* Body */}
      <circle
        cx={x} cy={y} r={r}
        fill="var(--app-bg-elevated, #21262D)"
        stroke={node.color}
        strokeWidth={hub ? 2.5 : 2}
      />
      {/* Label */}
      <text
        x={x} y={y - r + (hub ? 20 : 17)}
        textAnchor="middle"
        fill={node.color}
        fontSize={mobile ? 11 : 10.5}
        fontWeight={700}
        fontFamily="var(--font-sans, sans-serif)"
        letterSpacing="0.6"
      >
        {node.label.toUpperCase()}
      </text>
      {/* Value */}
      <text
        x={x} y={y + 3}
        textAnchor="middle"
        fill="var(--app-text-primary, #F0F6FC)"
        fontSize={hub ? (mobile ? 18 : 17) : (mobile ? 17 : 16)}
        fontWeight={700}
        fontFamily="var(--font-mono, monospace)"
      >
        {node.value}
      </text>
      {/* Unit / secondary */}
      <text
        x={x} y={y + (hub ? 23 : 20)}
        textAnchor="middle"
        fill={hub ? 'var(--app-text-secondary, #8B949E)' : node.color}
        fontSize={mobile ? 11 : 10}
        fontWeight={700}
        fontFamily="var(--font-mono, monospace)"
      >
        {node.unit}
      </text>
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
  const noise = useInverterStore((st) => st.visualNoiseThreshold);
  // Reduced-motion users get static spokes (no SMIL animation).
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
  const inverter = nodeById('inverter');

  // Satellite nodes to render, in draw order.
  const satelliteIds: FlowNodeId[] = showEvc
    ? ['solar', 'grid', 'battery', 'ev']
    : ['solar', 'grid', 'battery'];

  return (
    <div className="flex flex-col items-center gap-3">
      <div className="flex justify-center w-full">
        <svg
          viewBox={`0 0 ${VB} ${VB}`}
          className="w-full"
          style={{ maxWidth: '460px', fontFamily: 'var(--font-sans, sans-serif)' }}
          role="img"
          aria-label={`Energy flow. ${vm.summaryText}`}
        >
          {/* Layer 1: flow spokes (behind nodes) */}
          {vm.flows.map((f) => (
            <FlowSpoke key={f.id} flow={f} posOf={posOf} maxW={vm.maxFlowWatts} reduced={reduced} />
          ))}

          {/* Layer 2: Home hub (centre) */}
          <NodeCircle node={home} x={CX} y={CY} r={HUB_R} hub mobile={mobile} />

          {/* Layer 3: satellites */}
          {satelliteIds.map((id) => {
            const node = nodeById(id);
            if (!node) return null;
            const p = posOf(id);
            return <NodeCircle key={id} node={node} x={p.x} y={p.y} r={SAT_R} mobile={mobile} />;
          })}
        </svg>
      </div>

      {/* Plain-English summary */}
      <p className="text-text-secondary text-sm font-sans text-center max-w-md px-4">
        {vm.summaryText}
      </p>

      {/* Inverter mini-card (demoted from hub to a supporting line) */}
      {inverter && (
        <p className="text-text-secondary text-xs font-sans text-center">
          <span className="text-text-primary font-medium">Inverter</span>
          {' · '}
          {inverter.value}
          {' · '}
          {inverter.unit}
        </p>
      )}
    </div>
  );
}

const EnergyOrbitDiagram = memo(EnergyOrbitDiagramInner);
export default EnergyOrbitDiagram;
