/**
 * Pick a label for the EV Charger node on the energy-flow diagram.
 *
 * The state machine distinguishes five live-data cases plus one
 * unreachable case so users get an actionable message instead of a
 * misleading "Disconnected" whenever the configured host doesn't respond
 * (issue #138), and so the charger's own "Idle" / "Charging" wording is
 * echoed on the Status page when the cable is unplugged after a session
 * (issue #139).
 *
 * `charging` means power is flowing: `buildEnergyFlows` passes the same
 * `evcActive` that draws the EV flow line, so the label and the line always
 * agree (issue #458). The state register alone never makes
 * it "Charging": a bridged charger can report Charging at 0 W with no cable,
 * and a real one does while the car has paused itself.
 *
 * The raw `chargingState` (HR 0 from the EVC — "Unknown", "Idle",
 * "Connected", "Starting", "Charging", "Startup Failure",
 * "End of Charging", "System Failure", "Scheduled", "Updating",
 * "Unstable CP") is consulted when no power flows, so an idle charger
 * reads "Idle" as its own display would (issue #139).
 *
 * Note: `connected` here is the charger's *network* reachability (we
 * successfully read a Modbus frame from it), NOT the physical cable state.
 * The cable (HR 2 `connection_status`) is surfaced separately under the EV
 * node's kW value as "Cable In" / "No Cable".
 *
 * | charging | chargingState | connected | everConnected | label          |
 * | -------- | ------------- | --------- | ------------- | -------------- |
 * | true     | *             | *         | *             | "Charging"     |
 * | false    | "Idle"        | *         | *             | "Idle"         |
 * | false    | (other)       | true      | *             | "Connected"    |
 * | false    | (other)       | false     | true          | "Disconnected" |
 * | false    | (other)       | false     | false         | "Not Found"    |
 *
 * Extracted from `EnergyFlowDiagram` so the rules can be unit-tested
 * without rendering the full SVG (and so the file only exports React
 * components, which keeps `vite-plugin-react`'s fast-refresh happy).
 */
export function evcNodeLabel(
  charging: boolean,
  connected: boolean,
  everConnected: boolean,
  chargingState: string = '',
): string {
  // Power flowing is the most authoritative signal: it tells us the EVC
  // is delivering energy right now, even if the connection-status bit is
  // stale or the ever-connected latch hasn't caught up yet (e.g.
  // EvcConnected was just broadcast and the first register read is in
  // flight — see issue #138).
  if (charging) return 'Charging';
  // "Idle" wins ahead of the connected flag: a charger that has just
  // finished a session but still has the cable plugged in (state=1,
  // conn=1) should read "Idle" rather than "Connected", and a charger
  // that's been unplugged (state=1, conn=0) should read "Idle" rather
  // than "Disconnected" — both because that's what the EVC's own
  // display says (issue #139). Reaching this branch requires
  // `charging=false`, so a stale Idle string can never mask a live
  // power flow.
  if (chargingState === 'Idle') return 'Idle';
  if (connected) return 'Connected';
  return everConnected ? 'Disconnected' : 'Not Found';
}
