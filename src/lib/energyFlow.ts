/**
 * Derived energy-flow view-model.
 *
 * Converts a live [`InverterSnapshot`](./types.ts) into the node / flow /
 * summary representation the diagram renders — see
 * [`EnergyFlowViewModel`](../types/energyFlow.ts). Pure: no React, no DOM,
 * fully unit-testable. All sign conventions and noise-threshold gating live
 * here so the SVG component is a dumb renderer.
 *
 * ## Sign conventions (per AGENTS.md)
 *
 * - `battery_power`: **+ve = discharging**, −ve = charging. We also trust
 *   `battery_state` ('charging' / 'discharging' / 'idle') as the primary
 *   signal (the backend derives it from BMS status) and use the sign as a
 *   cross-check / fallback.
 * - `grid_power`: **+ve = exporting**, −ve = importing.
 * - `solar_power` / `home_power`: always +ve.
 *
 * ## Noise threshold
 *
 * Flows and "active" node state are gated by `noiseThresholdW` (default
 * 20W, user-adjustable in Settings → Panel Controls). Sub-threshold
 * readings produce no flow and read as idle — this matches the legacy
 * `EnergyFlowDiagram` behaviour so the upgrade is invisible to existing
 * users.
 */

import type { InverterSnapshot, ScheduleSlot } from './types';
import type {
  EnergyFlow,
  EnergyFlowViewModel,
  FlowDirection,
  FlowNode,
  FlowNodeId,
} from '../types/energyFlow';
import {
  formatPercent,
  formatPower,
  formatVisualPower,
} from './format';

/** Default noise floor — matches the store's `visualNoiseThreshold` default. */
export const DEFAULT_NOISE_THRESHOLD_W = 20;

// ---------------------------------------------------------------------------
// Node colours — kept here (not in the component) so the legend, the glyph,
// and any future diagram variant all share one source of truth.
// ---------------------------------------------------------------------------

export const FLOW_COLORS = {
  solar: '#F5E04A',
  grid: '#EF5A4F',
  home: '#4F7DFF',
  battery: '#FBBF24',
  inverter: '#22D3EE',
  ev: '#A855F7',
} as const satisfies Record<FlowNodeId, string>;

/** Battery SOC tier colour, shared by the panel gauge and the diagram node. */
export function socColor(soc: number): string {
  if (soc < 20) return '#EF4444';
  if (soc < 50) return '#F59E0B';
  return '#22C55E';
}

/**
 * Fraction (0–1) of the battery body to fill for a given SOC. Clamped and
 * NaN-safe. Used by the AA-cell [`BatteryGauge`] so the fill-height math
 * lives here (unit-tested) rather than in the component.
 */
export function batteryFillFraction(soc: number): number {
  if (!Number.isFinite(soc)) return 0;
  return Math.max(0, Math.min(1, soc / 100));
}

// ---------------------------------------------------------------------------
// Battery mode label — extracted here to de-dupe the copy that previously
// lived in both EnergyFlowDiagram.tsx and BatteryPanel.tsx. Cosy / Override
// overrides mirror the existing inline logic exactly.
// ---------------------------------------------------------------------------

const BATTERY_MODE_LABELS: Record<string, string> = {
  unknown: 'Unknown',
  eco: 'Eco',
  eco_paused: 'Eco Paused',
  timed_demand: 'Timed Demand',
  timed_export: 'Timed Export',
  export_paused: 'Export Paused',
};

/** True when `now` falls inside any enabled slot's [start, end) window. */
function isAnySlotActive(slots: ScheduleSlot[] | undefined, now: Date): boolean {
  const curMin = now.getHours() * 60 + now.getMinutes();
  return (slots ?? []).some((slot) => {
    if (!slot.enabled) return false;
    const startMin = slot.start_hour * 60 + slot.start_minute;
    const endMin = slot.end_hour * 60 + slot.end_minute;
    return startMin < endMin
      ? curMin >= startMin && curMin < endMin
      : curMin >= startMin || curMin < endMin;
  });
}

/**
 * Display label for the battery mode, applying the Cosy / Override overrides.
 *
 * - "Cosy" when cosy mode is active or enabled-and-in-an-eco-mode.
 * - "Override" when a force charge or force discharge window is currently
 *   active (enable flag set AND inside a slot window — the enable register
 *   is a sticky schedule-enable, not an instantaneous "now" signal).
 * - Otherwise the raw mode label (Eco / Timed Demand / …).
 *
 * `now` is a parameter (default `new Date()`) so tests are deterministic.
 */
export function batteryModeDisplayLabel(
  mode: string,
  cosyActive: boolean,
  cosyEnabled: boolean,
  enableCharge: boolean,
  enableDischarge: boolean,
  chargeSlots: ScheduleSlot[] | undefined,
  dischargeSlots: ScheduleSlot[] | undefined,
  now: Date = new Date(),
): string {
  if (cosyActive) return 'Cosy';
  if (cosyEnabled && (mode === 'eco' || mode === 'eco_paused')) return 'Cosy';
  const forceChargeActive = enableCharge && isAnySlotActive(chargeSlots, now);
  const forceDischargeActive = enableDischarge && isAnySlotActive(dischargeSlots, now);
  if (forceChargeActive || forceDischargeActive) return 'Override';
  return BATTERY_MODE_LABELS[mode] ?? mode;
}

// ---------------------------------------------------------------------------
// Options + builder
// ---------------------------------------------------------------------------

export interface BuildEnergyFlowsOptions {
  /**
   * Noise floor in watts. Flows and active-node state below this read as
   * zero. Default [`DEFAULT_NOISE_THRESHOLD_W`].
   */
  noiseThresholdW?: number;
  /** EV charger power (watts). When >0 an EV node + flow is emitted. */
  evcPowerW?: number;
  /** When false, the EV node is omitted entirely (charger not configured). */
  showEvc?: boolean;
  /**
   * Resolved EV node label (e.g. "Charging" / "Idle" / "Connected" /
   * "Disconnected" / "Not Found") from [`evcNodeLabel`]. When omitted the
   * node falls back to "Charging" / "Idle" based on power alone.
   */
  evcLabel?: string;
  /** Injected clock for deterministic summary / slot-window tests. */
  now?: Date;
}

/**
 * Build the diagram view-model from a snapshot.
 *
 * Order of operations matters for the summary sentence: solar is attributed
 * first, then battery, then grid — matching how a homeowner thinks about
 * "what's powering my house".
 */
export function buildEnergyFlows(
  snapshot: InverterSnapshot,
  opts: BuildEnergyFlowsOptions = {},
): EnergyFlowViewModel {
  const noise = opts.noiseThresholdW ?? DEFAULT_NOISE_THRESHOLD_W;
  const now = opts.now ?? new Date();
  const s = snapshot;

  // --- Derived booleans (single source of truth for flows + summary) ---
  const solarActive = s.solar_power > noise;
  const absGrid = Math.abs(s.grid_power);
  const isExporting = s.grid_power > noise;
  const isImporting = s.grid_power < -noise;
  const absBattery = Math.abs(s.battery_power);
  // battery_state is the authoritative signal; sign is a cross-check.
  const isCharging = s.battery_state === 'charging' && absBattery > noise;
  const isDischarging = s.battery_state === 'discharging' && absBattery > noise;
  const homeActive = s.home_power > noise;
  const evcPower = opts.evcPowerW ?? 0;
  const evcActive = !!opts.showEvc && evcPower > noise;

  const modeLabel = batteryModeDisplayLabel(
    s.battery_mode,
    s.cosy_active,
    s.cosy_enabled,
    s.enable_charge,
    s.enable_discharge,
    s.charge_slots,
    s.discharge_slots,
    now,
  );

  // --- Flows (watts always non-negative; direction via from/to) ---
  //
  // Home-centred topology: the inverter is treated as passive plumbing,
  // so every flow is spoken of relative to the *home* (the radial hub)
  // rather than the physical inverter. Solar generates INTO home, grid
  // imports INTO home / exports OUT of home, battery charges FROM home /
  // discharges INTO home. This matches how a homeowner thinks about
  // "what's powering my house" (the whole point of the radial diagram)
  // and is how consumer energy apps (Tesla, etc.) present it. The
  // inverter still appears as a passive info node below the diagram.
  const flows: EnergyFlow[] = [];
  const push = (
    id: string,
    from: FlowNodeId,
    to: FlowNodeId,
    watts: number,
    direction: FlowDirection,
    label: string,
  ): void => {
    flows.push({ id, from, to, watts, direction, label });
  };

  if (solarActive) {
    push('solar', 'solar', 'home', s.solar_power, 'generate',
      `${formatPower(s.solar_power)} from solar`);
  }
  if (isImporting) {
    push('import', 'grid', 'home', absGrid, 'import',
      `${formatPower(absGrid)} importing`);
  }
  if (isExporting) {
    push('export', 'home', 'grid', absGrid, 'export',
      `${formatPower(absGrid)} exporting`);
  }
  if (isCharging) {
    push('charge', 'home', 'battery', absBattery, 'charge',
      `${formatPower(absBattery)} charging battery`);
  }
  if (isDischarging) {
    push('discharge', 'battery', 'home', absBattery, 'discharge',
      `${formatPower(absBattery)} from battery`);
  }
  // No self-flow for home: it is the hub, not a spoke. Its consumption is
  // shown as the hub node's value, not as a directed flow.
  if (evcActive) {
    push('ev', 'home', 'ev', evcPower, 'consume',
      `${formatPower(evcPower)} to EV`);
  }

  // --- Nodes (pre-formatted strings; renderer is dumb) ---
  const nodes: FlowNode[] = [
    {
      id: 'solar',
      label: 'Solar',
      value: formatVisualPower(s.solar_power, noise),
      unit: s.pv1_voltage > 0
        ? `${s.pv1_voltage.toFixed(1)}V`
        : `${(s.pv1_current + s.pv2_current).toFixed(1)}A`,
      color: FLOW_COLORS.solar,
      active: solarActive,
    },
    {
      id: 'grid',
      label: 'Grid',
      value: `${isExporting ? '-' : isImporting ? '+' : ''}${formatVisualPower(absGrid, noise)}`,
      unit: isImporting ? 'Import' : isExporting ? 'Export' : 'Idle',
      color: FLOW_COLORS.grid,
      active: isImporting || isExporting,
    },
    {
      id: 'home',
      label: 'Home',
      value: formatVisualPower(s.home_power, noise),
      unit: 'Consumption',
      color: FLOW_COLORS.home,
      active: homeActive,
    },
    {
      id: 'battery',
      label: 'Battery',
      value: `${isDischarging ? '-' : isCharging ? '+' : ''}${formatVisualPower(absBattery, noise)}`,
      unit: `${formatPercent(s.soc)} · ${modeLabel}`,
      color: socColor(s.soc),
      ringPercent: s.soc,
      active: isCharging || isDischarging,
    },
    {
      id: 'inverter',
      label: 'Inverter',
      value: s.device_type_display || '—',
      unit: `${s.inverter_temperature.toFixed(1)}°C`,
      color: FLOW_COLORS.inverter,
      active: solarActive || isImporting || isExporting || isCharging || isDischarging,
    },
  ];
  if (opts.showEvc) {
    nodes.push({
      id: 'ev',
      label: 'EV',
      value: formatVisualPower(evcPower, noise),
      unit: opts.evcLabel ?? (evcActive ? 'Charging' : 'Idle'),
      color: FLOW_COLORS.ev,
      active: evcActive,
    });
  }

  const maxFlowWatts = Math.max(...flows.map((f) => f.watts), 1);

  return {
    nodes,
    flows,
    summaryText: buildSummaryText({
      solarActive,
      solarWatts: s.solar_power,
      isExporting,
      exportWatts: absGrid,
      isImporting,
      importWatts: absGrid,
      isCharging,
      chargeWatts: absBattery,
      isDischarging,
      dischargeWatts: absBattery,
      homeActive,
      homeWatts: s.home_power,
      evcActive,
      evcWatts: evcPower,
      noise,
    }),
    maxFlowWatts,
  };
}

// ---------------------------------------------------------------------------
// Summary sentence
// ---------------------------------------------------------------------------

interface SummaryInputs {
  solarActive: boolean;
  solarWatts: number;
  isExporting: boolean;
  exportWatts: number;
  isImporting: boolean;
  importWatts: number;
  isCharging: boolean;
  chargeWatts: number;
  isDischarging: boolean;
  dischargeWatts: number;
  homeActive: boolean;
  homeWatts: number;
  evcActive: boolean;
  evcWatts: number;
  noise: number;
}

/**
 * Build a single plain-English sentence describing the current energy state.
 *
 * Two sentence shapes:
 *
 * 1. **Home is being powered** (home load + at least one source):
 *    `"{sources} {is/are} powering the home{, + destinations}."`
 *    e.g. "Solar is powering the home, charging the battery and exporting
 *    4.3kW to the grid."
 *
 * 2. **No home load** (or no recognised source): describe generation and
 *    where the energy is going.
 *    e.g. "Solar is generating 5.0kW and charging the battery."
 *
 * When nothing at all is active it reads `"System is idle."`. Sub-threshold
 * values are treated as zero (the caller already gated them) so the sentence
 * never mentions a 3W "import". Source order is solar → battery → grid,
 * matching how a homeowner thinks about "what's powering my house".
 *
 * Kept pure (watts figures pass through [`formatPower`]) so it unit-tests
 * against exact strings.
 */
export function buildSummaryText(inp: SummaryInputs): string {
  const destinations = destinationClauses(inp);

  // --- Sources into the home, in priority order ---
  const sources: string[] = [];
  if (inp.solarActive) sources.push('Solar');
  if (inp.isDischarging) sources.push('the battery');
  if (inp.isImporting) sources.push('the grid');

  // Case 1: home load with at least one recognised source.
  if (inp.homeActive && sources.length > 0) {
    const subject = joinList(sources);
    const verb = sources.length === 1 ? 'is' : 'are';
    let sentence = `${cap(subject)} ${verb} powering the home`;
    if (destinations.length > 0) {
      sentence += `, ${joinList(destinations)}`;
    }
    return `${sentence}.`;
  }

  // Nothing flowing at all.
  if (sources.length === 0 && destinations.length === 0) {
    if (inp.homeActive) return `Home is consuming ${formatPower(inp.homeWatts)}.`;
    return 'System is idle.';
  }

  // Home load present but no source classified active (e.g. everything under
  // threshold except the home reading) — don't pretend a source exists.
  if (inp.homeActive) {
    return `Home is consuming ${formatPower(inp.homeWatts)}.`;
  }

  // Case 2: no home load — narrate generation + destinations.
  const parts: string[] = [];
  if (inp.solarActive) parts.push(`Solar is generating ${formatPower(inp.solarWatts)}`);
  if (inp.isDischarging) parts.push(`the battery is discharging ${formatPower(inp.dischargeWatts)}`);
  if (inp.isImporting) parts.push(`importing ${formatPower(inp.importWatts)} from the grid`);
  parts.push(...destinations);
  if (parts.length === 0) return 'System is idle.';
  parts[0] = cap(parts[0]);
  return `${joinList(parts)}.`;
}

/** Clauses describing where energy is going (not into the home). */
function destinationClauses(inp: SummaryInputs): string[] {
  const out: string[] = [];
  if (inp.isCharging) out.push(`charging the battery at ${formatPower(inp.chargeWatts)}`);
  if (inp.isExporting) out.push(`exporting ${formatPower(inp.exportWatts)} to the grid`);
  if (inp.evcActive) out.push(`charging the EV at ${formatPower(inp.evcWatts)}`);
  return out;
}

/** Join a list with commas and a final "and" (no Oxford comma): [a,b,c] → "a, b and c". */
function joinList(items: string[]): string {
  if (items.length <= 1) return items.join('');
  return `${items.slice(0, -1).join(', ')} and ${items.at(-1)}`;
}

/** Capitalise the first letter (sentences start with a lowercased clause). */
function cap(s: string): string {
  return s.length === 0 ? s : s[0].toUpperCase() + s.slice(1);
}
