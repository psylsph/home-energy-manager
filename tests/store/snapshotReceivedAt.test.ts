import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

// The Control page ages the "Inverter Charge Rate" reading from the moment
// this browser received it (#362), so the store must stamp that on every
// snapshot and clear it with the snapshot.

const NOW_MS = Date.UTC(2026, 9, 11, 12, 0, 0);
const snapshot = { timestamp: 1 } as unknown as InverterSnapshot;

describe('snapshotReceivedAt', () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ['Date'] });
    vi.setSystemTime(NOW_MS);
  });

  afterEach(() => {
    vi.useRealTimers();
    useInverterStore.setState({ snapshot: null, snapshotReceivedAt: null });
  });

  it('records the local time a snapshot was set', () => {
    expect(useInverterStore.getState().snapshotReceivedAt).toBeNull();
    useInverterStore.getState().setSnapshot(snapshot);
    expect(useInverterStore.getState().snapshotReceivedAt).toBe(NOW_MS);
  });

  it('moves on when a newer snapshot arrives', () => {
    useInverterStore.getState().setSnapshot(snapshot);
    vi.setSystemTime(NOW_MS + 10_000);
    useInverterStore.getState().setSnapshot(snapshot);
    expect(useInverterStore.getState().snapshotReceivedAt).toBe(NOW_MS + 10_000);
  });

  it('clears with the snapshot', () => {
    useInverterStore.getState().setSnapshot(snapshot);
    useInverterStore.getState().clearSnapshot();
    expect(useInverterStore.getState().snapshot).toBeNull();
    expect(useInverterStore.getState().snapshotReceivedAt).toBeNull();
  });
});
