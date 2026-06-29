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
  formatVoltage,
  formatFrequency,
  formatCurrent,
  formatTemp,
} from './format';

/** True when a flow with the given id is present in the list. */
export function hasFlow(flows: EnergyFlow[], id: string): boolean {
  return flows.some((f) => f.id === id);
}

/**
 * Resolve the *visual* endpoint pair of a flow. The charge/ export spokes
 * have been split into source-attributed flows (solar_charge, grid_charge)
 * upstream of this point, so those spokes use their natural endpoints
 * directly. The remaining reroute is for `export` when solar is active —
 * the home → grid spoke is rerouted to start at solar so the eye follows
 * generation → grid (issue #155 follow-the-energy visual). The SVG
 * renderer and the spoke-colour rule both consume this so the rendered
 * line and its colour always agree (issue #170).
 */
export function visualEndpoints(flow: EnergyFlow, flows: EnergyFlow[]): { from: FlowNodeId; to: FlowNodeId } {
  const solarActive = hasFlow(flows, 'solar');
  if (flow.id === 'export' && solarActive) return { from: 'solar', to: 'grid' };
  return { from: flow.from, to: flow.to };
}

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

/**
 * Fixed colour for *battery-output* spokes (battery → home, battery → grid)
 * and the home → battery charge spoke.
 *
 * Issue #170: "Battery to all destinations should always be green" — the
 * spokes use a single, fixed green so the user sees a consistent signal
 * regardless of stored charge. Power throughput is independent of stored
 * energy (20 A at 80 % is the same throughput as 20 A at 10 %), so the
 * SoC tier colours must not flicker the spokes. The brand colour
 * (`FLOW_COLORS.battery = '#FBBF24'` amber, used for the sidebar Battery
 * icon when no snapshot is available) is a separate identity from this.
 *
 * The user also asked for the spoke green to *match* the battery symbol
 * green — that's `#22C55E`, the SoC tier ≥ 50 % colour. Using this same
 * green means a user looking at a green Battery node and a green
 * battery-origin spoke sees them as the same identity.
 *
 * `#22C55E` is also distinct from:
 *  - FLOW_COLORS.grid (`#EF5A4F`) red — never confuse a forced-discharge
 *    export with grid import,
 *  - FLOW_COLORS.solar (`#F5E04A`) yellow — never confuse a battery
 *    discharge spoke with a solar spoke,
 *  - FLOW_COLORS.home (`#4F7DFF`) blue — never confuse a discharge spoke
 *    with the home-as-source rule.
 */
export const BATTERY_OUTPUT_COLOR = '#22C55E';

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
// Battery mode label — extracted here to de-dupe display logic across the
// status diagram and BatteryPanel. Cosy remains the highest-priority display
// override; active force windows are rendered as mode-aware annotations so
// the underlying HR(27) mode stays visible.
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
export function isAnySlotActive(slots: ScheduleSlot[] | undefined, now: Date): boolean {
  const curMin = now.getHours() * 60 + now.getMinutes();
  return (slots ?? []).some((slot) => {
    if (!slot.enabled) return false;
    const startMin = slot.start_hour * 60 + slot.start_minute;
    const endMin = slot.end_hour * 60 + slot.end_minute;
    if (startMin === endMin) return false;
    return startMin < endMin
      ? curMin >= startMin && curMin < endMin
      : curMin >= startMin || curMin < endMin;
  });
}

/**
 * Display label for the battery mode, applying the Cosy / active-window display overrides.
 *
 * - "Cosy" when cosy mode is active or enabled-and-in-an-eco-mode.
 * - `"<mode> (Charging)"` / `"<mode> (Discharging)"` when a force charge or
 *   force discharge window is currently active (enable flag set AND inside a
 *   slot window — the enable register is a sticky schedule-enable, not an
 *   instantaneous "now" signal). If both windows are active, the suffix is
 *   combined as `"Charging & Discharging"` so the rare overlap is explicit.
 *   Eco Paused is intentionally grouped under "Eco" here: the active slot is
 *   the important transient state, while the underlying eco-family mode remains visible.
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
  const rawModeLabel = BATTERY_MODE_LABELS[mode] ?? mode;
  const modeLabel = mode === 'eco' || mode === 'eco_paused' ? 'Eco' : rawModeLabel;
  if (forceChargeActive || forceDischargeActive) {
    const activeStates = [
      forceChargeActive ? 'Charging' : null,
      forceDischargeActive ? 'Discharging' : null,
    ].filter((state): state is string => state !== null);
    return `${modeLabel} (${activeStates.join(' & ')})`;
  }
  return rawModeLabel;
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
  const batteryColor = socColor(s.soc);

  // --- Spoke colour rule (issue #170) ---
  //
  // The visual spokes are coloured by their **visual** source. The user
  // articulated three identity rules:
  //
  //  - "Solar to everywhere always yellow / amber" — solar wins the
  //    colour rule whenever it's on the spoke.
  //  - "Battery to all destinations always green" — battery wins when
  //    solar is absent (so discharge and discharge_to_grid stay green
  //    even though the latter touches grid).
  //  - "Grid to everywhere always red" — grid takes over when neither
  //    solar nor battery is on the spoke (import, lone grid_charge).
  //
  // Priority is therefore solar > battery > grid > home. This locks the
  // "Battery to all destinations" semantics: a battery-as-source spoke is
  // green even when the destination is grid.
  const COLOUR_BY_IDENTITY: Record<FlowNodeId, string> = {
    solar: FLOW_COLORS.solar,
    battery: BATTERY_OUTPUT_COLOR,
    grid: FLOW_COLORS.grid,
    home: FLOW_COLORS.home,
    inverter: FLOW_COLORS.inverter,
    ev: FLOW_COLORS.ev,
  };
  const spokeColor = (from: FlowNodeId, to: FlowNodeId): string => {
    if (from === 'solar' || to === 'solar') return COLOUR_BY_IDENTITY.solar;
    if (from === 'battery' || to === 'battery') return COLOUR_BY_IDENTITY.battery;
    if (from === 'grid' || to === 'grid') return COLOUR_BY_IDENTITY.grid;
    return COLOUR_BY_IDENTITY[from];
  };

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
    // Colour is computed from the **visual** endpoint pair (after the
    // solar reroute) so the rendered spoke matches the colour the view-
    // model records. The colour may be revised once all flows exist (a
    // forward reference is needed because the solar reroute depends on
    // whether a `solar` flow exists in the same render). See
    // `colourFlowsFromVisualRoutes()`.
    flows.push({ id, from, to, watts, direction, label });
  };

  // Battery-charge source attribution (issue #170).
  //
  // The inverter reports a single `battery_power` for the charge wattage
  // but doesn't say which source fed it. We split the charge between
  // solar and grid by attribution priority: solar first (cheaper,
  // greener), grid only when charge exceeds solar. Then:
  //  - `charge` is the total battery-charge spoke (visually routed to
  //    start at solar when solar is sufficient, otherwise at grid).
  //  - `solar_charge` is the solar-only outer arc (solar → battery) when
  //    solar covers part of the charge. This is the visible line from
  //    Solar to Battery the user expects to see.
  //  - `grid_charge` is the grid → battery outer arc when grid contributes
  //    any portion. This is the visible line from Grid to Battery the
  //    user expects when grid is feeding the battery (issue #170).
  //  - `import` is reduced by the grid-charging portion so the red
  //    grid → home spoke only carries the home-direct portion and the
  //    totals balance: solar + grid − home = charge + export.
  //
  // Example: solar 1 kW, grid 3 kW importing, home 1 kW, charge 3 kW.
  //  - solar_charge: 1 kW (the whole of solar, min(solar, charge))
  //  - grid_charge: 2 kW (the excess)
  //  - import: 1 kW (the household portion, 3 − 2)
  //  - total wattage leaving grid: 1 + 2 = 3 kW ✓
  //
  // Another example: solar 1 kW, grid 0, home 1 kW, charge 0.
  //  - solar_charge: 0, grid_charge: 0, import: 0, solar_direct: 1 kW
  //
  // Attributing the *whole of solar* to the battery (even if some of it
  // could equally have gone to the home) keeps the diagram simple: the
  // user sees solar feeding home, and solar feeding battery, and grid
  // covering whatever remains. The maths balances because solar and
  // grid together must cover home + charge + export.
  const solarChargeWatts = solarActive ? Math.min(s.solar_power, absBattery) : 0;
  const gridChargeWatts = isImporting
    ? Math.max(0, absBattery - solarChargeWatts)
    : 0;
  const gridPortionToBattery = Math.min(gridChargeWatts, absGrid);

  // Battery-discharge source attribution. When discharging, the battery
  // powers the home first and any surplus flows battery → grid. The
  // home-direct portion is `batteryToHome`; the surplus is `batteryToGrid`.
  // Both quantities may also consume part of the grid reading when the
  // export is partly battery-driven.
  const batteryToHome = isDischarging
    ? Math.min(absBattery, s.home_power > noise ? s.home_power : absBattery)
    : 0;
  const batteryToGrid = isDischarging
    ? Math.max(0, absBattery - batteryToHome)
    : 0;

  if (solarActive) {
    push('solar', 'solar', 'home', s.solar_power, 'generate',
      `${formatPower(s.solar_power)} from solar`);
  }
  if (isImporting) {
    // The import spoke carries the home-direct grid portion; the
    // grid-charge spoke (emitted below when grid feeds the battery) carries
    // the rest.
    const importDisplayW = Math.max(0, absGrid - gridPortionToBattery);
    if (importDisplayW > noise) {
      push('import', 'grid', 'home', importDisplayW, 'import',
        `${formatPower(importDisplayW)} importing`);
    }
  }
  // Export-spoke source attribution (issue #170 final simplification).
  //
  // Simple check the user articulated: "if solar < house load then no
  // solar export" — the rule is about the *solar-source* export, not
  // export in general. Three cases:
  //
  //  1. Solar is generating AND its own surplus exceeds home load: emit a
  //     yellow solar-driven `export` spoke of size (solar − home) for the
  //     portion attributable to solar.
  //  2. Solar is off / solar is generating but solar ≤ home (no solar
  //     surplus): the export, if any, comes purely from a non-solar
  //     source (typically battery discharge_to_grid). Skip the `export`
  //     spoke entirely — drawing it would imply solar exports to grid
  //     when it doesn't.
  //  3. Battery discharge overflow is emitted independently via
  //     `discharge_to_grid`. That spoke carries any export attributable
  //     to the battery (and replaces a parallel `export` spoke for the
  //     same wattage).
  //
  // Net effect:
  //  - solar > home_load → `export` spoke drawn (yellow solar surplus).
  //  - solar ≤ home_load → no `export` spoke (solar doesn't export).
  //    Battery discharges still show as `discharge_to_grid` if applicable.
  const solarSurplusW = Math.max(0, s.solar_power - (s.home_power > noise ? s.home_power : 0));
  if (isExporting && solarSurplusW > noise) {
    push('export', 'home', 'grid', solarSurplusW, 'export',
      `${formatPower(solarSurplusW)} exporting`);
  }
  if (isCharging) {
    // Source attribution (issue #170): emit one or two source-attributed
    // spokes so the user sees explicit "Solar to Battery" / "Grid to
    // Battery" lines wherever the data supports them. We never emit the
    // synthetic aggregate `charge` flow alongside the splits — that would
    // double-count and visually confuse the eye (issue #170).
    //
    //  - solar only feeds the battery: `solar_charge` (yellow, solar →
    //    battery).
    //  - grid only feeds the battery: `grid_charge` (red, grid → battery).
    //  - both feed the battery: `solar_charge` + `grid_charge`, no
    //    aggregate `charge`.
    //  - solar = 0 and grid not importing (rare; e.g. the inverter reports
    //    charge without a recognised source — perhaps from the EV's V2H
    //    path): fall back to a single `charge` flow (home → battery, green)
    //    so the user still sees a battery-charging spoke.
    const splitCoversAll = solarChargeWatts + gridChargeWatts >= absBattery - noise;
    const onlySolarFeeds = solarChargeWatts > noise && gridChargeWatts <= noise;
    const onlyGridFeeds = gridChargeWatts > noise && solarChargeWatts <= noise;
    if (onlySolarFeeds) {
      push('solar_charge', 'solar', 'battery', solarChargeWatts, 'charge',
        `${formatPower(solarChargeWatts)} from solar to battery`);
    } else if (onlyGridFeeds) {
      push('grid_charge', 'grid', 'battery', gridChargeWatts, 'charge',
        `${formatPower(gridChargeWatts)} from grid to battery`);
    } else if (splitCoversAll) {
      // Both sources contribute — emit both split spokes, no aggregate.
      if (solarChargeWatts > noise) {
        push('solar_charge', 'solar', 'battery', solarChargeWatts, 'charge',
          `${formatPower(solarChargeWatts)} from solar to battery`);
      }
      if (gridChargeWatts > noise) {
        push('grid_charge', 'grid', 'battery', gridChargeWatts, 'charge',
          `${formatPower(gridChargeWatts)} from grid to battery`);
      }
    } else {
      // Fallback: no recognisable upstream source for the charge. Emit a
      // single `charge` spoke (home → battery) so the user still sees a
      // battery-charging indicator.
      push('charge', 'home', 'battery', absBattery, 'charge',
        `${formatPower(absBattery)} charging battery`);
    }
  }
  // When the battery is discharging, the export spoke IS the
  // discharge-to-grid overflow. The inverter reports a single grid_power
  // and a single home_power; if battery outflow exceeds home load, the
  // surplus shows up as `grid_power > 0` (export). We must not
  // double-count by emitting both `export` (home → grid) and
  // `discharge_to_grid` (battery → grid) for the same wattage (issue
  // #170 final user clarification: "only Battery flow to grid in this
  // scenario"). The discharge spoke itself carries only the home-direct
  // portion; the surplus appears as discharge_to_grid.
  if (isDischarging && batteryToHome > noise) {
    push('discharge', 'battery', 'home', batteryToHome, 'discharge',
      `${formatPower(batteryToHome)} from battery`);
  }
  // Battery → grid: discharge overflow that exceeds the house load. Drawn
  // directly battery→grid so the dot ends at the grid instead of the
  // house. Source is battery → the spoke stays BATTERY_OUTPUT_COLOR green
  // (issue #170, priority solar > battery > grid).
  if (batteryToGrid > noise) {
    push('discharge_to_grid', 'battery', 'grid',
      batteryToGrid, 'export',
      `${formatPower(batteryToGrid)} exporting`);
  }
  // No self-flow for home: it is the hub, not a spoke. Its consumption is
  // shown as the hub node's value, not as a directed flow.
  if (evcActive) {
    push('ev', 'home', 'ev', evcPower, 'consume',
      `${formatPower(evcPower)} to EV`);
  }

  // Resolve spoke colours from the **visual** endpoint pair (after the
  // solar reroute of `charge` and `export`). Without this pass, the spoke
  // colour would be computed from the logical `from`/`to` only, missing
  // the fact that the rendered line starts at solar when solar is active
  // (issue #170: user expects solar-touched spokes to be yellow).
  for (const f of flows) {
    const v = visualEndpoints(f, flows);
    f.color = spokeColor(v.from, v.to);
  }

  // --- Nodes (pre-formatted strings; renderer is dumb) ---
  const nodes: FlowNode[] = [
    {
      id: 'solar',
      label: 'Solar',
      value: formatVisualPower(s.solar_power, noise),
      // PV1 voltage drives the volts label; when the dongle reports a real
      // voltage we show "V/A" (matches the legacy inverter-centred diagram).
      // PV strings without voltage telemetry (some gateways) fall back to
      // current only.
      unit: s.pv1_voltage > 0
        ? `${formatVoltage(s.pv1_voltage)}/${formatCurrent(s.pv1_current + s.pv2_current)}`
        : formatCurrent(s.pv1_current + s.pv2_current),
      color: FLOW_COLORS.solar,
      active: solarActive,
    },
    {
      id: 'grid',
      label: 'Grid',
      // Direction lives in the status word below the node ("Importing" /
      // "Exporting"), so the magnitude is shown as a plain positive number
      // here. The old `+` / `-` prefix on the value was redundant with the
      // badge and read as "negative discharge" to non-technical users.
      value: formatVisualPower(absGrid, noise),
      unit: `${formatVoltage(s.grid_voltage)}/${formatFrequency(s.grid_frequency)}`,
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
      // Magnitude only — the "Charging" / "Discharging" / "Idle" badge
      // (and the SOC · mode unit line) already tells the user which way
      // the energy is flowing. Showing "-839W" alongside a "Discharging"
      // label made it look like a bug.
      value: formatVisualPower(absBattery, noise),
      unit: `${formatPercent(s.soc)} · ${modeLabel}`,
      color: batteryColor,
      ringPercent: s.soc,
      active: isCharging || isDischarging,
    },
    {
      id: 'inverter',
      label: 'Inverter',
      value: s.device_type_display || '—',
      unit: formatTemp(s.inverter_temperature),
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
