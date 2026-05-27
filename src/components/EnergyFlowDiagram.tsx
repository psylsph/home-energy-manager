import type { InverterSnapshot } from '../lib/types';
import { formatPower, formatVoltage, formatPercent, formatEnergy } from '../lib/format';

interface Props {
  snapshot: InverterSnapshot;
}

// Node positions in SVG viewBox (400 x 400)
const NODES = {
  solar:   { cx: 200, cy: 55,  color: '#F59E0B',  label: 'Solar' },
  grid:    { cx: 55,  cy: 210, color: '#EF4444',   label: 'Grid' },
  home:    { cx: 345, cy: 210, color: '#14B8A6',   label: 'Home' },
  battery: { cx: 200, cy: 355, color: '#6366F1', label: 'Battery' },
};

interface FlowDef {
  id: string;
  from: { cx: number; cy: number };
  to: { cx: number; cy: number };
  active: boolean;
  power: number;
}

function FlowLine({ flow }: { flow: FlowDef }) {
  const dx = flow.to.cx - flow.from.cx;
  const dy = flow.to.cy - flow.from.cy;
  const len = Math.sqrt(dx * dx + dy * dy);

  // Start/end offset so line starts at node edge
  const offset = 42;
  const ux = dx / len;
  const uy = dy / len;
  const x1 = flow.from.cx + ux * offset;
  const y1 = flow.from.cy + uy * offset;
  const x2 = flow.to.cx - ux * offset;
  const y2 = flow.to.cy - uy * offset;

  // Midpoint for power label
  const mx = (x1 + x2) / 2;
  const my = (y1 + y2) / 2;

  return (
    <g>
      {/* Base line (inactive) */}
      <line
        x1={x1} y1={y1} x2={x2} y2={y2}
        stroke="#21262D"
        strokeWidth={3}
        strokeLinecap="round"
      />
      {/* Active flow */}
      {flow.active && (
        <>
          <line
            x1={x1} y1={y1} x2={x2} y2={y2}
            stroke="#22D3EE"
            strokeWidth={3}
            strokeLinecap="round"
            strokeDasharray="8 6"
            opacity={0.9}
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
            points="0,-5 10,0 0,5"
            fill="#22D3EE"
            transform={`translate(${mx},${my}) rotate(${Math.atan2(dy, dx) * 180 / Math.PI})`}
          />
          {/* Power label */}
          <text
            x={mx}
            y={my - 14}
            textAnchor="middle"
            fill="#F0F6FC"
            fontSize="13"
            fontFamily="var(--font-mono, monospace)"
            fontWeight="600"
          >
            {formatPower(flow.power)}
          </text>
        </>
      )}
    </g>
  );
}

function FlowNode({
  cx, cy, color, label, primary, secondary,
}: {
  cx: number; cy: number; color: string; label: string;
  primary: string; secondary: string;
}) {
  return (
    <g>
      {/* Circle background */}
      <circle cx={cx} cy={cy} r={38} fill="#161B22" stroke={color} strokeWidth={2.5} />
      {/* Label */}
      <text
        x={cx} y={cy - 8}
        textAnchor="middle"
        fill={color}
        fontSize="11"
        fontWeight="600"
        fontFamily="var(--font-sans, sans-serif)"
      >
        {label}
      </text>
      {/* Primary value */}
      <text
        x={cx} y={cy + 10}
        textAnchor="middle"
        fill="#F0F6FC"
        fontSize="14"
        fontWeight="700"
        fontFamily="var(--font-mono, monospace)"
      >
        {primary}
      </text>
      {/* Secondary */}
      <text
        x={cx} y={cy + 24}
        textAnchor="middle"
        fill="#8B949E"
        fontSize="10"
        fontFamily="var(--font-mono, monospace)"
      >
        {secondary}
      </text>
    </g>
  );
}

export default function EnergyFlowDiagram({ snapshot: s }: Props) {
  const isChargingFromSolar = s.battery_state === 'charging' && s.solar_power > 0;
  const isDischarging = s.battery_state === 'discharging';

  const flows: FlowDef[] = [
    {
      id: 'solar-home',
      from: NODES.solar, to: NODES.home,
      active: s.solar_power > 0,
      power: s.solar_power,
    },
    {
      id: 'solar-battery',
      from: NODES.solar, to: NODES.battery,
      active: isChargingFromSolar,
      power: Math.abs(s.battery_power),
    },
    {
      id: 'battery-home',
      from: NODES.battery, to: NODES.home,
      active: isDischarging,
      power: Math.abs(s.battery_power),
    },
    {
      id: 'grid-home',
      from: NODES.grid, to: NODES.home,
      active: s.grid_power > 0,
      power: s.grid_power,
    },
    {
      id: 'solar-grid',
      from: NODES.solar, to: NODES.grid,
      active: s.grid_power < 0,
      power: Math.abs(s.grid_power),
    },
  ];

  return (
    <svg
      viewBox="0 0 400 410"
      className="w-full h-auto max-h-[500px]"
      style={{ fontFamily: 'var(--font-sans, sans-serif)' }}
    >
      {/* Connection lines (rendered behind nodes) */}
      {flows.map((f) => (
        <FlowLine key={f.id} flow={f} />
      ))}

      {/* Nodes */}
      <FlowNode
        {...NODES.solar}
        primary={formatPower(s.solar_power)}
        secondary={formatVoltage(s.pv1_voltage)}
      />
      <FlowNode
        {...NODES.grid}
        primary={formatPower(s.grid_power)}
        secondary={formatVoltage(s.grid_voltage)}
      />
      <FlowNode
        {...NODES.home}
        primary={formatPower(s.home_power)}
        secondary={formatEnergy(s.today_consumption_kwh)}
      />
      <FlowNode
        {...NODES.battery}
        primary={formatPercent(s.soc)}
        secondary={formatPower(s.battery_power)}
      />
    </svg>
  );
}
