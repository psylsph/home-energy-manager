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
 * The raw `chargingState` (HR 0 from the EVC — "Unknown", "Idle",
 * "Connected", "Starting", "Charging", "Startup Failure",
 * "End of Charging", "System Failure", "Scheduled", "Updating",
 * "Unstable CP") is consulted alongside `charging` so the app matches
 * what the charger's own display would say. `charging` / `connected` /
 * `everConnected` are kept as fallback inputs because the legacy
 * `setEvcData(power, charging, connected)` callers don't have the raw
 * string handy — those callers still get the previous four-label behaviour
 * (Charging / Connected / Disconnected / Not Found).
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
  // `charging` is the most authoritative signal: live Active_Power
  // tells us the EVC is actually delivering energy right now, even if
  // the connection-status bit is stale or the ever-connected latch
  // hasn't caught up yet (e.g. EvcConnected was just broadcast and
  // the first register read is in flight — see issue #138).
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
