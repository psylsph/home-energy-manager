import { describe, it, expect } from 'vitest';
import { evcNodeLabel } from '../../src/lib/evcLabel';

/**
 * Tests for the EV Charger node label picker (issues #138 and #139).
 *
 * The label needs to distinguish several states so users get an actionable
 * message rather than a misleading "Disconnected" for every failure mode,
 * and so the app matches the EVC's own display when the charger is idle
 * after a session ends:
 *
 *  - Charging         power is flowing
 *  - Idle             EVC reports state=1, no power flowing
 *  - Connected        charger is reachable and idle (state≠Idle)
 *  - Disconnected     charger was reachable and is now offline (state≠Idle)
 *  - Not Found        we have never successfully reached the configured host
 */
describe('evcNodeLabel', () => {
  it('returns "Charging" when the charger is delivering power', () => {
    expect(evcNodeLabel(true, true, true)).toBe('Charging');
  });

  it('returns "Charging" even when the connected flag is stale false (power is the truth)', () => {
    // Edge case: a brief Modbus blip could leave connected=false while
    // charging=true from the previous frame. Power wins.
    expect(evcNodeLabel(true, false, true)).toBe('Charging');
  });

  it('returns "Charging" on a fresh host that just started delivering power', () => {
    expect(evcNodeLabel(true, true, false)).toBe('Charging');
  });

  it('returns "Idle" when the EVC reports state=1 with no power (issue #139)', () => {
    // User-reported case: state=1 (Idle), conn=0 (Not Connected), P=0W
    // after a charging session ends and the cable is unplugged.
    expect(evcNodeLabel(false, false, true, 'Idle')).toBe('Idle');
  });

  it('returns "Idle" when state=1 with the cable still plugged in', () => {
    // state=1 + conn=1: cable plugged in but charger not delivering
    // power. The EVC's own display says "Idle" and so should we.
    expect(evcNodeLabel(false, true, true, 'Idle')).toBe('Idle');
  });

  it('returns "Idle" on a fresh host that has never charged', () => {
    // Brand-new host that has answered but only reported state=1 so far.
    // everConnected is true (TCP/Modbus handshake completed and
    // EvcConnected was broadcast), but the EVC says Idle — "Idle" wins.
    expect(evcNodeLabel(false, true, true, 'Idle')).toBe('Idle');
  });

  it('"Idle" does NOT override an in-progress charge (power is the truth)', () => {
    // Edge case: the raw charging_state string lags the active_power
    // register by one poll cycle, or vice-versa. Active power wins —
    // we shouldn't show "Idle" while power is flowing.
    expect(evcNodeLabel(true, true, true, 'Idle')).toBe('Charging');
  });

  it('returns "Connected" when idle but reachable (state is not "Idle")', () => {
    // Pre-existing behaviour: state=2 ("Connected"), cable in, no power,
    // host previously reachable. Latch is true.
    expect(evcNodeLabel(false, true, true, 'Connected')).toBe('Connected');
  });

  it('returns "Connected" on a fresh host that just connected but is idle', () => {
    expect(evcNodeLabel(false, true, false)).toBe('Connected');
  });

  it('returns "Disconnected" when we used to see it and now we don\'t', () => {
    expect(evcNodeLabel(false, false, true)).toBe('Disconnected');
  });

  it('returns "Disconnected" when state is something other than "Idle"', () => {
    // state=6 ("End of Charging") or state=10 ("Unstable CP") with the
    // cable unplugged — neither is "Idle", so we fall through to the
    // legacy Disconnected label. (We don't expose every EVC state as a
    // distinct label — see evcLabel.ts docstring.)
    expect(evcNodeLabel(false, false, true, 'End of Charging')).toBe('Disconnected');
  });

  it('returns "Not Found" when we have never successfully reached the host', () => {
    // This is the user-reported case from issue #138: a typo like
    // "10.1.71" instead of "10.1.1.71" produces exactly this state.
    expect(evcNodeLabel(false, false, false)).toBe('Not Found');
  });

  it('returns "Idle" when the charger reports state=1 even if everConnected is false', () => {
    // Defensive: a real EVC register read is the most authoritative
    // signal we have. If it says Idle, we show Idle — the everConnected
    // latch only governs fallback labels (Connected / Disconnected /
    // Not Found). In practice this case shouldn't occur because
    // `setEvcData(...)` latches everConnected to true on the same
    // call as the first successful read, but the semantic is clean.
    expect(evcNodeLabel(false, false, false, 'Idle')).toBe('Idle');
  });
});
