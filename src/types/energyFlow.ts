/**
 * Derived view-model for the energy-flow diagram.
 *
 * The live [`InverterSnapshot`](../lib/types.ts) is a flat bag of register
 * readings with sign conventions that differ per field (battery +ve =
 * discharge, grid +ve = export — see AGENTS.md "Battery power sign
 * convention"). The diagram needs something friendlier: a list of nodes,
 * a list of directed flows between them, and a plain-English sentence
 * describing what's happening right now.
 *
 * [`buildEnergyFlows`](../lib/energyFlow.ts) produces this view-model from a
 * snapshot so the SVG component stays a pure renderer — no sign logic, no
 * noise-threshold branching, no string-building in JSX. Both the simple
 * (home-centred) and any future diagram variant render from the same model.
 */

/** Stable identifier for a node on the diagram. */
export type FlowNodeId =
  | 'solar'
  | 'home'
  | 'grid'
  | 'battery'
  | 'inverter'
  | 'ev';

/** What kind of energy movement a flow represents. Drives colour + label. */
export type FlowDirection =
  /** Solar generating into the system. */
  | 'generate'
  /** Battery absorbing energy (charging). */
  | 'charge'
  /** Battery releasing energy (discharging). */
  | 'discharge'
  /** Grid supplying energy to the home. */
  | 'import'
  /** Home/system sending energy to the grid. */
  | 'export'
  /** Home powering a load (EV charger). */
  | 'consume';

/**
 * A single node on the diagram.
 *
 * `value` / `unit` are pre-formatted strings (e.g. `"5.0kW"`, `"97% · Eco"`)
 * so the renderer never touches formatters. `active` lets the renderer glow
 * / emphasise nodes that are currently moving meaningful energy.
 */
export interface FlowNode {
  id: FlowNodeId;
  label: string;
  value: string;
  unit: string;
  color: string;
  /** Optional 0–100 ring fill (currently battery state-of-charge). */
  ringPercent?: number;
  /** Whether this node is currently moving meaningful power. */
  active: boolean;
}

/**
 * A directed energy movement between two nodes.
 *
 * `watts` is always non-negative — direction is carried by `from`/`to`, not
 * sign, so the renderer never has to interpret a sign. Flows below the
 * noise threshold are filtered out entirely by [`buildEnergyFlows`].
 */
export interface EnergyFlow {
  id: string;
  from: FlowNodeId;
  to: FlowNodeId;
  watts: number;
  direction: FlowDirection;
  /** Human-readable label, e.g. "4.3kW exporting". */
  label: string;
  /** Optional rendered colour override for stateful sources such as battery SOC. */
  color?: string;
}

/**
 * Complete derived state for one snapshot, ready to render.
 */
export interface EnergyFlowViewModel {
  nodes: FlowNode[];
  flows: EnergyFlow[];
  /** Plain-English summary, e.g. "Solar is powering the home…". */
  summaryText: string;
  /** Largest flow wattage, for stroke-width scaling (≥1 to avoid div-by-zero). */
  maxFlowWatts: number;
}
