import { memo } from 'react';
import type { InverterSnapshot } from '../lib/types';
import { formatPower, formatPercent, formatCurrent, formatTemp, formatVoltage } from '../lib/format';

interface Props {
  snapshot: InverterSnapshot;
}

// Radial layout — Inverter hub at centre, four nodes at cardinal points.
const W = 520;
const H = 425;

const NODES = {
  inverter: { cx: W / 2, cy: 207, color: '#22D3EE', label: 'Inverter' },
  solar:    { cx: W / 2, cy: 50,  color: '#F59E0B', label: 'Solar' },
  home:     { cx: 55,    cy: 207, color: '#14B8A6', label: 'Home' },
  grid:     { cx: W - 55, cy: 207, color: '#EF4444', label: 'Grid' },
  battery:  { cx: W / 2, cy: 365, color: '#6366F1', label: 'Battery' },
};

const MODE_LABELS: Record<string, string> = {
  eco: 'Eco',
  eco_paused: 'Eco Paused',
  timed_demand: 'Timed Discharge',
  timed_export: 'Timed Export',
  export_paused: 'Paused',
};

function modeLabel(mode: string): string {
  return MODE_LABELS[mode] || mode;
}

// ---------------------------------------------------------------------------
// Flow line
// ---------------------------------------------------------------------------

interface FlowDef {
  id: string;
  from: { cx: number; cy: number };
  to: { cx: number; cy: number };
  active: boolean;
  power: number;
  /** Where to place the power label relative to the midpoint: 'above' | 'below' | 'left' | 'right' */
  labelSide: 'above' | 'below' | 'left' | 'right';
}

function FlowTrack({ flow }: { flow: FlowDef }) {
  const dx = flow.to.cx - flow.from.cx;
  const dy = flow.to.cy - flow.from.cy;
  const len = Math.sqrt(dx * dx + dy * dy);

  const offset = 50;
  const ux = dx / len;
  const uy = dy / len;
  const x1 = flow.from.cx + ux * offset;
  const y1 = flow.from.cy + uy * offset;
  const x2 = flow.to.cx - ux * offset;
  const y2 = flow.to.cy - uy * offset;

  return (
    <line
      x1={x1} y1={y1} x2={x2} y2={y2}
      stroke="#21262D"
      strokeWidth={2}
      strokeLinecap="round"
    />
  );
}

function FlowAnimation({ flow }: { flow: FlowDef }) {
  const dx = flow.to.cx - flow.from.cx;
  const dy = flow.to.cy - flow.from.cy;
  const len = Math.sqrt(dx * dx + dy * dy);

  const offset = 50;
  const ux = dx / len;
  const uy = dy / len;
  const x1 = flow.from.cx + ux * offset;
  const y1 = flow.from.cy + uy * offset;
  const x2 = flow.to.cx - ux * offset;
  const y2 = flow.to.cy - uy * offset;

  const mx = (x1 + x2) / 2;
  const my = (y1 + y2) / 2;

  const angle = Math.atan2(dy, dx) * 180 / Math.PI;

  if (!flow.active) return null;

  return (
    <>
      <line
        x1={x1} y1={y1} x2={x2} y2={y2}
        stroke="#22D3EE"
        strokeWidth={2.5}
        strokeLinecap="round"
        strokeDasharray="8 6"
        opacity={0.85}
      >
        <animate
          attributeName="stroke-dashoffset"
          from="0"
          to={-28}
          dur="0.8s"
          repeatCount="indefinite"
        />
      </line>
      {/* Arrow at midpoint */}
      <polygon
        points="0,-4.5 9,0 0,4.5"
        fill="#22D3EE"
        transform={`translate(${mx},${my}) rotate(${angle})`}
      />
    </>
  );
}

// ---------------------------------------------------------------------------
// Nodes
// ---------------------------------------------------------------------------

interface NodeProps {
  cx: number;
  cy: number;
  color: string;
  label: string;
  value: string;
  unit: string;
  hub?: boolean;
}

function FlowNode({ cx, cy, color, label, value, unit, hub }: NodeProps) {
  const r = hub ? 48 : 44;
  return (
    <g>
      {/* Subtle outer glow */}
      <circle cx={cx} cy={cy} r={r + 5} fill="none" stroke={color} strokeWidth={1} opacity={0.15} />
      {/* Main circle */}
      <circle cx={cx} cy={cy} r={r} fill="#0D1117" stroke={color} strokeWidth={hub ? 2.5 : 2} />
      {/* Label */}
      <text
        x={cx} y={cy - (hub ? 12 : 11)}
        textAnchor="middle"
        fill={color}
        fontSize={hub ? 10 : 10.5}
        fontWeight="700"
        fontFamily="var(--font-sans, sans-serif)"
        letterSpacing="0.6"
      >
        {label.toUpperCase()}
      </text>
      {/* Value */}
      <text
        x={cx} y={cy + (hub ? 8 : 7)}
        textAnchor="middle"
        fill="#F0F6FC"
        fontSize="15"
        fontWeight="700"
        fontFamily="var(--font-mono, monospace)"
      >
        {value}
      </text>
      {/* Unit / secondary info */}
      <text
        x={cx} y={cy + (hub ? 23 : 22)}
        textAnchor="middle"
        fill="#8B949E"
        fontSize="10"
        fontFamily="var(--font-mono, monospace)"
      >
        {unit}
      </text>
    </g>
  );
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

function EnergyFlowDiagramInner({ snapshot: s }: Props) {
  const isCharging = s.battery_state === 'charging';
  const isDischarging = s.battery_state === 'discharging';
  const isExporting = s.grid_power > 0;
  const isImporting = s.grid_power < 0;

  const flows: FlowDef[] = [
    // Solar → Inverter (always top→centre)
    {
      id: 'solar',
      from: NODES.solar,
      to: NODES.inverter,
      active: s.solar_power > 0,
      power: s.solar_power,
      labelSide: 'right',
    },
    // Inverter → Home (always centre→right)
    {
      id: 'home',
      from: NODES.inverter,
      to: NODES.home,
      active: s.home_power > 0,
      power: s.home_power,
      labelSide: 'below',
    },
    // Grid → Inverter (importing, left→centre)
    {
      id: 'import',
      from: NODES.grid,
      to: NODES.inverter,
      active: isImporting,
      power: Math.abs(s.grid_power),
      labelSide: 'above',
    },
    // Inverter → Grid (exporting, centre→left)
    {
      id: 'export',
      from: NODES.inverter,
      to: NODES.grid,
      active: isExporting,
      power: Math.abs(s.grid_power),
      labelSide: 'above',
    },
    // Inverter → Battery (charging, centre→bottom)
    {
      id: 'charge',
      from: NODES.inverter,
      to: NODES.battery,
      active: isCharging,
      power: Math.abs(s.battery_power),
      labelSide: 'right',
    },
    // Battery → Inverter (discharging, bottom→centre)
    {
      id: 'discharge',
      from: NODES.battery,
      to: NODES.inverter,
      active: isDischarging,
      power: Math.abs(s.battery_power),
      labelSide: 'right',
    },
  ];

  return (
    <div className="flex justify-center">
      <svg
        viewBox={`0 0 ${W} ${H}`}
        className="w-full"
        style={{ maxWidth: '560px', fontFamily: 'var(--font-sans, sans-serif)' }}
      >
        {/* Layer 1: All gray tracks (behind everything) */}
        {flows.map((f) => (
          <FlowTrack key={`track-${f.id}`} flow={f} />
        ))}

        {/* Layer 2: All animated cyan flows (on top of all tracks) */}
        {flows.map((f) => (
          <FlowAnimation key={`anim-${f.id}`} flow={f} />
        ))}

        {/* Layer 3: Nodes (on top of everything) */}
        <FlowNode
          {...NODES.solar}
          value={formatPower(s.solar_power)}
          unit={`${formatVoltage(s.pv1_voltage)}/${formatCurrent(s.pv1_current + s.pv2_current)}`}
        />
        <FlowNode
          {...NODES.grid}
          value={formatPower(Math.abs(s.grid_power))}
          unit={isImporting ? 'Import' : isExporting ? 'Export' : 'Idle'}
        />
        <FlowNode
          {...NODES.home}
          value={formatPower(s.home_power)}
          unit="Consumption"
        />
        <FlowNode
          {...NODES.battery}
          value={formatPower(Math.abs(s.battery_power))}
          unit={formatPercent(s.soc)}
        />
        {/* Battery mode label */}
        <text
          x={W / 2}
          y={400}
          textAnchor="middle"
          fill="#8B949E"
          style={{ fontSize: 10, fontFamily: 'sans-serif' }}
        >
          {modeLabel(s.battery_mode)}
        </text>
        <FlowNode
          {...NODES.inverter}
          hub
          value={formatTemp(s.inverter_temperature)}
          unit={s.device_type || '—'}
        />
      </svg>
    </div>
  );
}

const EnergyFlowDiagram = memo(EnergyFlowDiagramInner);
export default EnergyFlowDiagram;
