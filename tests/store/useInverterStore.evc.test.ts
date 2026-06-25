import { describe, it, expect, beforeEach } from 'vitest';
import { useInverterStore } from '../../src/store/useInverterStore';

/**
 * Tests for the EV Charger state machine in the Zustand store (issue #138).
 *
 * The store needs to:
 *  - Latch `evcEverConnected` to true the first time we see a connected
 *    EVC snapshot, and never auto-clear it.
 *  - Be reset by `resetEvc()` so the UI returns to "Not Found" when the
 *    user saves a new host in Settings.
 *
 * These tests run against the real store so we catch selector /
 * serialisation issues that a mock would hide.
 */
describe('useInverterStore — EVC state machine (issue #138)', () => {
  beforeEach(() => {
    // Reset between tests so the latch from one test doesn't leak.
    useInverterStore.setState({
      evcHost: '',
      evcPower: 0,
      evcCharging: false,
      evcConnected: false,
      evcEverConnected: false,
    });
  });

  it('starts with evcEverConnected=false (initial state shows "Not Found")', () => {
    expect(useInverterStore.getState().evcEverConnected).toBe(false);
    expect(useInverterStore.getState().evcConnected).toBe(false);
  });

  it('latches evcEverConnected=true the first time we receive a connected snapshot', () => {
    const { setEvcData } = useInverterStore.getState();
    // Simulate the WS handler receiving {"type":"evc", "connection_status":"Connected", ...}
    setEvcData(0, false, true);
    expect(useInverterStore.getState().evcEverConnected).toBe(true);
    expect(useInverterStore.getState().evcConnected).toBe(true);
  });

  it('latches even when active_power is 0 (idle but reachable)', () => {
    // Idle car plugged in: power=0, charging=false, connected=true.
    useInverterStore.getState().setEvcData(0, false, true);
    expect(useInverterStore.getState().evcEverConnected).toBe(true);
  });

  it('does not latch when connection_status is not Connected (backend Disconnected)', () => {
    // The frontend never normally calls setEvcData with connected=false for
    // "Charging" frames, but defensively confirm the latch doesn't fire.
    useInverterStore.getState().setEvcData(0, false, false);
    expect(useInverterStore.getState().evcEverConnected).toBe(false);
  });

  it('latch stays true even after a later EvcDisconnected frame', () => {
    const { setEvcData } = useInverterStore.getState();
    // First, the charger connects.
    setEvcData(0, false, true);
    expect(useInverterStore.getState().evcEverConnected).toBe(true);
    // Then the connection drops. The WS handler calls setEvcData(0, false, false).
    setEvcData(0, false, false);
    expect(useInverterStore.getState().evcConnected).toBe(false);
    // Latch stays true: the UI will now render "Disconnected" rather than
    // regressing to "Not Found", which would lie about prior reachability.
    expect(useInverterStore.getState().evcEverConnected).toBe(true);
  });

  it('resetEvc() clears the latch so a new host can show "Not Found" again', () => {
    const { setEvcData, resetEvc } = useInverterStore.getState();
    setEvcData(1200, true, true);
    expect(useInverterStore.getState().evcEverConnected).toBe(true);
    resetEvc();
    expect(useInverterStore.getState().evcEverConnected).toBe(false);
    expect(useInverterStore.getState().evcConnected).toBe(false);
    expect(useInverterStore.getState().evcPower).toBe(0);
    expect(useInverterStore.getState().evcCharging).toBe(false);
  });

  it('markEvcConnectedReached() latches and flips evcConnected=true (issue #138)', () => {
    // Simulates the backend's `EvcConnected` WS broadcast right after
    // a successful TCP/Modbus handshake, before the first register read
    // completes. The diagram label should drop out of "Not Found" the
    // moment the backend confirms the host is reachable.
    const { markEvcConnectedReached } = useInverterStore.getState();
    expect(useInverterStore.getState().evcEverConnected).toBe(false);
    expect(useInverterStore.getState().evcConnected).toBe(false);
    markEvcConnectedReached();
    expect(useInverterStore.getState().evcEverConnected).toBe(true);
    expect(useInverterStore.getState().evcConnected).toBe(true);
  });

  it('markEvcConnectedReached() does not steal power / charging from a later snapshot', () => {
    const { setEvcData, markEvcConnectedReached } = useInverterStore.getState();
    // Simulate a transient where the first read returns power=740W (charging).
    setEvcData(740, true, true);
    // Then a reconnect happens (broadcast EvcConnected with no fresh read yet).
    markEvcConnectedReached();
    expect(useInverterStore.getState().evcPower).toBe(740);
    expect(useInverterStore.getState().evcCharging).toBe(true);
  });

  it('a later EvcDisconnected after markEvcConnectedReached flips evcConnected back to false (latch stays)', () => {
    const { markEvcConnectedReached, setEvcData } = useInverterStore.getState();
    markEvcConnectedReached();
    expect(useInverterStore.getState().evcConnected).toBe(true);
    // First read fails — backend broadcasts EvcDisconnected.
    setEvcData(0, false, false);
    expect(useInverterStore.getState().evcConnected).toBe(false);
    // Latch stays: the diagram now reads "Disconnected" rather than
    // regressing to "Not Found" (issue #138).
    expect(useInverterStore.getState().evcEverConnected).toBe(true);
  });

  it('resetEvc() clears the markEvcConnectedReached latch too', () => {
    const { markEvcConnectedReached, resetEvc } = useInverterStore.getState();
    markEvcConnectedReached();
    resetEvc();
    expect(useInverterStore.getState().evcEverConnected).toBe(false);
    expect(useInverterStore.getState().evcConnected).toBe(false);
  });

  it('after resetEvc(), the next connected frame re-latches', () => {
    const { setEvcData, resetEvc } = useInverterStore.getState();
    setEvcData(1200, true, true);
    resetEvc();
    setEvcData(0, false, true);
    expect(useInverterStore.getState().evcEverConnected).toBe(true);
  });

  it('label derivation matches evcNodeLabel for the full state space', async () => {
    // Import here to avoid circular dep at module load time.
    const { evcNodeLabel } = await import('../../src/lib/evcLabel');
    const { setEvcData, resetEvc } = useInverterStore.getState();

    // Fresh start, never connected.
    resetEvc();
    expect(evcNodeLabel(false, false, useInverterStore.getState().evcEverConnected)).toBe('Not Found');

    // Just connected (idle).
    setEvcData(0, false, true);
    expect(evcNodeLabel(false, true, useInverterStore.getState().evcEverConnected)).toBe('Connected');

    // Drop the connection — should now show "Disconnected".
    setEvcData(0, false, false);
    expect(evcNodeLabel(false, false, useInverterStore.getState().evcEverConnected)).toBe('Disconnected');

    // Charge starts.
    setEvcData(2200, true, true);
    expect(evcNodeLabel(true, true, useInverterStore.getState().evcEverConnected)).toBe('Charging');
  });
});
