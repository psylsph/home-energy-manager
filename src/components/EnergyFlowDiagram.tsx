import { memo } from 'react';
import { useIsMobile } from '../hooks/useIsMobile';
import { useInverterStore } from '../store/useInverterStore';
import type { InverterSnapshot } from '../lib/types';
import { formatVisualPower, formatPercent, formatCurrent, formatTemp, formatVoltage } from '../lib/format';
import { evcNodeLabel } from '../lib/evcLabel';

interface Props {
  snapshot: InverterSnapshot;
  /** EV Charger active power in watts. 0 = not charging or no data. */
  evcPower?: number;
  /**
   * Raw EV Charger charging-state string from HR 0 (`"Unknown"`, `"Idle"`,
   * `"Charging"`, …). When `"Idle"` and power is zero, the diagram node
   * reads "Idle" instead of "Connected" / "Disconnected" so the app
   * matches the charger's own display (issue #139).
   */
  evcChargingState?: string;
  /** Whether the EV Charger is actively charging. */
  evcCharging?: boolean;
  /** Whether the EV Charger is connected/responding. */
  evcConnected?: boolean;
  /**
   * True once at least one valid EVC snapshot has been received since the
   * page loaded. Lets the diagram distinguish "was here, now offline"
   * ("Disconnected") from "never reached the configured host" ("Not
   * Found" — issue #138). Defaults to `evcConnected` when omitted.
   */
  evcEverConnected?: boolean;
  /** Whether EV Charger is configured (non-empty host). When falsy, EV node is hidden. */
  showEvc?: boolean;
}

// X layout — Inverter hub at centre, four primary nodes in the corners
// (Solar TL, Grid TR, Home BL, Battery BR), EV charger centred along the
// bottom when configured. The corner arrangement gives each node more room
// than the old + (cardinal) layout, so the circles and text can be slightly
// larger and still legible when the SVG scales down on mobile. The canvas is
// also near-square so it fills more vertical space on narrow screens.
const W = 460;
const H = 470;

// The node layout leaves empty margins at the top (~34px) and bottom
// (~38px) of the viewBox. Cropping them here makes the rendered diagram
// vertically tighter when embedded in a panel.
const VIEW_TOP = 16;
const VIEW_BOTTOM = 24;

const NODES = {
  inverter: { cx: W / 2, cy: 233,    color: '#22D3EE', label: 'Inverter' },
  solar:    { cx: 80,    cy: 80,     color: '#F59E0B', label: 'Solar' },
  grid:     { cx: W - 80, cy: 80,    color: '#EF4444', label: 'Grid' },
  home:     { cx: 80,    cy: 386,    color: '#14B8A6', label: 'Home' },
  battery:  { cx: W - 80, cy: 386,   color: '#6366F1', label: 'Battery' },
  ev:       { cx: W / 2, cy: 386,    color: '#10B981', label: 'EV' },
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
        points="0,-8 16,0 0,8"
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
  /** Override the auto-computed lozenge width. */
  width?: number;
  /** Override the auto-computed lozenge height. */
  height?: number;
  /** When true, use larger mobile font sizing. */
  mobile?: boolean;
}

function FlowNode({ cx, cy, color, label, value, unit, hub, width, height, mobile }: NodeProps) {
  // Lozenge (rounded-rectangle) dimensions — wider than tall so multi-char
  // values/units fit without growing the overall footprint. Same box size on
  // mobile and desktop: legibility on phones comes from the responsive font
  // bump plus the extra horizontal room the lozenge gives over a circle.
  const baseW = hub ? 156 : 132;
  const baseH = hub ? 100 : 84;
  const w = width ?? baseW;
  const h = height ?? baseH;
  const cornerR = Math.min(w, h) * 0.22;
  // Guard against non-string props that would crash React (<text> children
  // must be strings or numbers — objects cause React error #31).
  const safeLabel = typeof label === 'string' ? label : String(label ?? '');
  const safeValue = typeof value === 'string' || typeof value === 'number' ? value : String(value ?? '');
  const safeUnit = typeof unit === 'string' || typeof unit === 'number' ? unit : String(unit ?? '');
  const safeColor = typeof color === 'string' ? color : '#888';
  const nx = cx - w / 2;
  const ny = cy - h / 2;
  return (
    <g>
      {/* Subtle outer glow */}
      <rect x={nx - 4} y={ny - 4} width={w + 8} height={h + 8} rx={cornerR + 4} ry={cornerR + 4} fill="none" stroke={safeColor} strokeWidth={1} opacity={0.15} />
      {/* Main lozenge */}
      <rect x={nx} y={ny} width={w} height={h} rx={cornerR} ry={cornerR} fill="var(--app-bg-elevated)" stroke={safeColor} strokeWidth={hub ? 2.5 : 2} />
      {/* Label */}
      <text
        x={cx} y={cy - 20}
        textAnchor="middle"
        fill={safeColor}
        fontSize={hub ? (mobile ? 11.5 : 11) : (mobile ? 12 : 11.5)}
        fontWeight="700"
        fontFamily="var(--font-sans, sans-serif)"
        letterSpacing="0.6"
      >
        {safeLabel.toUpperCase()}
      </text>
      {/* Value */}
      <text
        x={cx} y={cy + 5}
        textAnchor="middle"
        fill="var(--app-text-primary)"
        fontSize={mobile ? 20 : 18}
        fontWeight="700"
        fontFamily="var(--font-mono, monospace)"
      >
        {safeValue}
      </text>
      {/* Unit / secondary info */}
      <text
        x={cx} y={cy + 24}
        textAnchor="middle"
        fill={hub ? "var(--app-text-secondary)" : safeColor}
        fontSize={hub ? (mobile ? 13 : 12) : (mobile ? 14 : 13)}
        fontWeight="700"
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

function EnergyFlowDiagramInner({ snapshot: s, evcPower = 0, evcChargingState = '', evcCharging = false, evcConnected = false, evcEverConnected, showEvc = false }: Props) {
  const mobile = useIsMobile();
  const noiseThreshold = useInverterStore((st) => st.visualNoiseThreshold);
  const isCharging = s.battery_state === 'charging';
  const isDischarging = s.battery_state === 'discharging';
  const absGrid = Math.abs(s.grid_power);
  const absBattery = Math.abs(s.battery_power);
  const isExporting = s.grid_power > noiseThreshold;
  const isImporting = s.grid_power < -noiseThreshold;
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

  const evcActive = showEvc && evcPower > noiseThreshold;
  const evcUnit = evcNodeLabel(evcCharging, evcConnected, !!evcEverConnected, evcChargingState);

  const modeLabel = modeDisplayLabel(
    s.battery_mode, s.cosy_active, s.cosy_enabled,
    s.enable_charge, s.enable_discharge, chargeSlotActive, dischargeSlotActive,
  );

  const flows: FlowDef[] = [
    // Solar → Inverter (always top→centre)
    {
      id: 'solar',
      from: NODES.solar,
      to: NODES.inverter,
      active: s.solar_power > noiseThreshold,
      power: s.solar_power,
      labelSide: 'right',
    },
    // Inverter → Home (always centre→left)
    {
      id: 'home',
      from: NODES.inverter,
      to: NODES.home,
      active: s.home_power > noiseThreshold,
      power: s.home_power,
      labelSide: 'below',
    },
    // Grid → Inverter (importing, right→centre)
    {
      id: 'import',
      from: NODES.grid,
      to: NODES.inverter,
      active: isImporting,
      power: Math.abs(s.grid_power),
      labelSide: 'above',
    },
    // Inverter → Grid (exporting, centre→right)
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
      active: isCharging && absBattery > noiseThreshold,
      power: Math.abs(s.battery_power),
      labelSide: 'right',
    },
    // Battery → Inverter (discharging, bottom→centre)
    {
      id: 'discharge',
      from: NODES.battery,
      to: NODES.inverter,
      active: isDischarging && absBattery > noiseThreshold,
      power: Math.abs(s.battery_power),
      labelSide: 'right',
    },
  ];

  // EV flow: Home → EV (energy flows from house to car)
  if (showEvc) {
    flows.push({
      id: 'ev',
      from: NODES.home,
      to: NODES.ev,
      active: evcActive,
      power: evcPower,
      labelSide: 'right',
    });
  }

  const maxPower = Math.max(...flows.map(x => x.power), 1);

  return (
    <div className="flex justify-center">
      <svg
        viewBox={`0 ${VIEW_TOP} ${W} ${H - VIEW_TOP - VIEW_BOTTOM}`}
        className="w-full"
        style={{ maxWidth: '500px', fontFamily: 'var(--font-sans, sans-serif)' }}
      >
        {/* Layer 1: All gray tracks (behind everything) */}
        {flows.map((f) => (
          <FlowTrack key={`track-${f.id}`} flow={f} />
        ))}

        {/* Layer 2: All animated cyan flows (on top of all tracks) */}
        {flows.map((f) => (
          <FlowAnimation key={`anim-${f.id}`} flow={f} maxPower={maxPower} />
        ))}

        {/* Layer 3: Nodes (on top of everything) */}
        <FlowNode
          {...NODES.solar}
          mobile={mobile}
          value={formatVisualPower(s.solar_power, noiseThreshold)}
          unit={s.pv1_voltage > 0 ? `${formatVoltage(s.pv1_voltage)}/${formatCurrent(s.pv1_current + s.pv2_current)}` : formatCurrent(s.pv1_current + s.pv2_current)}
        />
        <FlowNode
          {...NODES.grid}
          mobile={mobile}
          value={`${isExporting ? '-' : ''}${formatVisualPower(absGrid, noiseThreshold)}`}
          unit={isImporting ? 'Import' : isExporting ? 'Export' : 'Idle'}
        />
        <FlowNode
          {...NODES.home}
          mobile={mobile}
          value={formatVisualPower(s.home_power, noiseThreshold)}
          unit="Consumption"
        />
        <FlowNode
          {...NODES.battery}
          mobile={mobile}
          value={`${isDischarging && absBattery > noiseThreshold ? '-' : ''}${formatVisualPower(absBattery, noiseThreshold)}`}
          unit={`${formatPercent(s.soc)} · ${modeLabel}`}
          color={s.soc < 20 ? '#EF4444' : s.soc < 50 ? '#F59E0B' : '#22C55E'}
        />
        {s.agile_active && (
          <text
            x={W / 2}
            y={325}
            textAnchor="middle"
            fill="#F59E0B"
            style={{ fontSize: 9, fontFamily: 'sans-serif' }}
          >
            Agile: {s.agile_state}
          </text>
        )}
        <FlowNode
          {...NODES.inverter}
          mobile={mobile}
          hub
          value={formatTemp(s.inverter_temperature)}
          unit={s.device_type_display || '—'}
        />

        {/* EV Charger node — only when configured */}
        {showEvc && (
          <FlowNode
            {...NODES.ev}
            mobile={mobile}
            width={116}
            height={76}
            value={formatVisualPower(evcPower, noiseThreshold)}
            unit={evcUnit}
          />
        )}
      </svg>
    </div>
  );
}

const EnergyFlowDiagram = memo(EnergyFlowDiagramInner);
export default EnergyFlowDiagram;
