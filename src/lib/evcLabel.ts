/**
 * Pick a label for the EV Charger node on the energy-flow diagram.
 *
 * The state machine distinguishes three live-data cases plus one
 * unreachable case so users get an actionable message instead of a
 * misleading "Disconnected" whenever the configured host doesn't respond
 * (issue #138):
 *
 * | charging | connected | everConnected | label          |
 * | -------- | --------- | ------------- | -------------- |
 * | true     | *         | *             | "Charging"     |
 * | false    | true      | *             | "Connected"    |
 * | false    | false     | true          | "Disconnected" |
 * | false    | false     | false         | "Not Found"    |
 *
 * Extracted from `EnergyFlowDiagram` so the rules can be unit-tested
 * without rendering the full SVG (and so the file only exports React
 * components, which keeps `vite-plugin-react`'s fast-refresh happy).
 */
export function evcNodeLabel(
  charging: boolean,
  connected: boolean,
  everConnected: boolean,
): string {
  if (charging) return 'Charging';
  if (connected) return 'Connected';
  return everConnected ? 'Disconnected' : 'Not Found';
}
