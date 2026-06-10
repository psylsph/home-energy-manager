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
  eco_paused: 'Paused',
  timed_demand: 'Timed',
  timed_export: 'Timed',
  export_paused: 'Paused',
};

function modeLabel(mode: string): string {
  return MODE_LABELS[mode] || mode;
}

/** Battery mode label, overridden to "Cosy" when cosy mode is enabled, */
/** or "Override" when force charge or force discharge is active.       */
function modeDisplayLabel(
  mode: string, cosyActive: boolean, cosyEnabled: boolean,
  enableCharge: boolean, enableDischarge: boolean, inChargeWindow: boolean, inDischargeWindow: boolean,
): string {
  if (cosyActive) return 'Cosy';
  if (cosyEnabled && (mode === 'eco' || mode === 'eco_paused')) return 'Cosy';
  // Force charge is active only when the master charge-enable flag is set
  // AND the current time falls within an active charge slot window. The
  // enable_charge register (HR 96 / HR 1123) is a sticky schedule-enable
  // flag, not an instantaneous "charging now" signal.
  const forceChargeActive = enableCharge && inChargeWindow;
  // Same logic for discharge: the enable_discharge flag (HR 59) enables
  // timed slots; force discharge is only active when inside a window.
  const forceDischargeActive = enableDischarge && inDischargeWindow;
  if (forceChargeActive || forceDischargeActive) return 'Override';
  return modeLabel(mode);
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

  const offset = 55;
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

/** Compute a visual stroke width proportional to power volume. */
function flowStrokeWidth(power: number, maxPower: number): number {
  if (maxPower <= 0) return 3.5;
  return Math.max(2.5, Math.min(6, 2.5 + (power / maxPower) * 3.5));
}

function FlowAnimation({ flow, maxPower }: { flow: FlowDef; maxPower: number }) {
  const dx = flow.to.cx - flow.from.cx;
  const dy = flow.to.cy - flow.from.cy;
  const len = Math.sqrt(dx * dx + dy * dy);

  const offset = 55;
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

  const sw = flowStrokeWidth(flow.power, maxPower);

  return (
    <>
      <line
        x1={x1} y1={y1} x2={x2} y2={y2}
        stroke="#22D3EE"
        strokeWidth={sw}
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
        points="0,-6 12,0 0,6"
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
  const r = hub ? 56 : 50;
  // Guard against non-string props that would crash React (<text> children
  // must be strings or numbers — objects cause React error #31).
  const safeLabel = typeof label === 'string' ? label : String(label ?? '');
  const safeValue = typeof value === 'string' || typeof value === 'number' ? value : String(value ?? '');
  const safeUnit = typeof unit === 'string' || typeof unit === 'number' ? unit : String(unit ?? '');
  const safeColor = typeof color === 'string' ? color : '#888';
  return (
    <g>
      {/* Subtle outer glow */}
      <circle cx={cx} cy={cy} r={r + 5} fill="none" stroke={safeColor} strokeWidth={1} opacity={0.15} />
      {/* Main circle */}
      <circle cx={cx} cy={cy} r={r} fill="#0D1117" stroke={safeColor} strokeWidth={hub ? 2.5 : 2} />
      {/* Label */}
      <text
        x={cx} y={cy - (hub ? 14 : 13)}
        textAnchor="middle"
        fill={safeColor}
        fontSize={hub ? 11 : 11.5}
        fontWeight="700"
        fontFamily="var(--font-sans, sans-serif)"
        letterSpacing="0.6"
      >
        {safeLabel.toUpperCase()}
      </text>
      {/* Value */}
      <text
        x={cx} y={cy + (hub ? 10 : 9)}
        textAnchor="middle"
        fill="#F0F6FC"
        fontSize="18"
        fontWeight="700"
        fontFamily="var(--font-mono, monospace)"
      >
        {safeValue}
      </text>
      {/* Unit / secondary info */}
      <text
        x={cx} y={cy + (hub ? 27 : 26)}
        textAnchor="middle"
        fill="#8B949E"
        fontSize="11"
        fontFamily="var(--font-mono, monospace)"
      >
        {safeUnit}
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
  const chargeSlotActive = (s.charge_slots ?? []).some(slot => {
    if (!slot.enabled) return false;
    const now = new Date();
    const curMin = now.getHours() * 60 + now.getMinutes();
    const startMin = slot.start_hour * 60 + slot.start_minute;
    const endMin = slot.end_hour * 60 + slot.end_minute;
    return startMin < endMin
      ? curMin >= startMin && curMin < endMin
      : curMin >= startMin || curMin < endMin;
  });
  const dischargeSlotActive = (s.discharge_slots ?? []).some(slot => {
    if (!slot.enabled) return false;
    const now = new Date();
    const curMin = now.getHours() * 60 + now.getMinutes();
    const startMin = slot.start_hour * 60 + slot.start_minute;
    const endMin = slot.end_hour * 60 + slot.end_minute;
    return startMin < endMin
      ? curMin >= startMin && curMin < endMin
      : curMin >= startMin || curMin < endMin;
  });

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
          <FlowAnimation key={`anim-${f.id}`} flow={f} maxPower={Math.max(...flows.map(x => x.power), 1)} />
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
          {modeDisplayLabel(s.battery_mode, s.cosy_active, s.cosy_enabled, s.enable_charge, s.enable_discharge, chargeSlotActive, dischargeSlotActive)}
        </text>
        {s.agile_active && (
          <text
            x={W / 2}
            y={423}
            textAnchor="middle"
            fill="#F59E0B"
            style={{ fontSize: 9, fontFamily: 'sans-serif' }}
          >
            Agile: {s.agile_state}
          </text>
        )}
        <FlowNode
          {...NODES.inverter}
          hub
          value={formatTemp(s.inverter_temperature)}
          unit={s.device_type_display || '—'}
        />
      </svg>
    </div>
  );
}

const EnergyFlowDiagram = memo(EnergyFlowDiagramInner);
export default EnergyFlowDiagram;
