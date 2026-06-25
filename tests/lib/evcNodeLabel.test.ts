import { describe, it, expect } from 'vitest';
import { evcNodeLabel } from '../../src/lib/evcLabel';

/**
 * Tests for the EV Charger node label picker (issue #138).
 *
 * The label needs to distinguish four states so users get an actionable
 * message rather than a misleading "Disconnected" for every failure
 * mode:
 *
 *  - Charging         power is flowing
 *  - Connected        charger is reachable and idle
 *  - Disconnected     charger was reachable and is now offline
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

  it('returns "Connected" when idle but reachable', () => {
    expect(evcNodeLabel(false, true, true)).toBe('Connected');
  });

  it('returns "Connected" on a fresh host that just connected but is idle', () => {
    expect(evcNodeLabel(false, true, false)).toBe('Connected');
  });

  it('returns "Disconnected" when we used to see it and now we don\'t', () => {
    expect(evcNodeLabel(false, false, true)).toBe('Disconnected');
  });

  it('returns "Not Found" when we have never successfully reached the host', () => {
    // This is the user-reported case from issue #138: a typo like
    // "10.1.71" instead of "10.1.1.71" produces exactly this state.
    expect(evcNodeLabel(false, false, false)).toBe('Not Found');
  });
});
