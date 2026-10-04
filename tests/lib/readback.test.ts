import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act } from '@testing-library/react';
import { useInverterStore } from '../../src/store/useInverterStore';
import { waitForSnapshotReadback } from '../../src/lib/readback';
import type { InverterSnapshot } from '../../src/lib/types';

// ---------------------------------------------------------------------------
// Control writes are queued by the API and applied register by register by the
// poll loop, so the POST returning means "queued", not "applied". The helper
// holds a save pending until a NEWER snapshot shows the value (issue #346: the
// "Applying changes to inverter" banner vanished after a few milliseconds
// because nothing waited for the inverter).
// ---------------------------------------------------------------------------

const snap = (chargeRate: number): InverterSnapshot => ({ charge_rate: chargeRate } as InverterSnapshot);
const isRate = (rate: number) => (s: InverterSnapshot) => s.charge_rate === rate;

describe('waitForSnapshotReadback', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useInverterStore.setState({ snapshot: snap(50) });
  });

  afterEach(() => {
    vi.useRealTimers();
    useInverterStore.setState({ snapshot: null });
  });

  it('confirms when a newer snapshot shows the value', async () => {
    const before = useInverterStore.getState().snapshot;
    const result = waitForSnapshotReadback(isRate(17), before, 20_000);
    act(() => useInverterStore.setState({ snapshot: snap(17) }));
    await expect(result).resolves.toBe('confirmed');
  });

  it('ignores the snapshot the save started from, even if it already matches', async () => {
    // The register may already hold the value, but only a snapshot read after
    // the write proves it was applied rather than merely queued.
    useInverterStore.setState({ snapshot: snap(17) });
    const before = useInverterStore.getState().snapshot;
    const result = waitForSnapshotReadback(isRate(17), before, 20_000);
    let settled = false;
    void result.then(() => { settled = true; });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(settled).toBe(false);

    act(() => useInverterStore.setState({ snapshot: snap(17) }));
    await expect(result).resolves.toBe('confirmed');
  });

  it('keeps waiting through snapshots that do not match', async () => {
    const before = useInverterStore.getState().snapshot;
    const result = waitForSnapshotReadback(isRate(17), before, 20_000);
    act(() => useInverterStore.setState({ snapshot: snap(50) }));
    act(() => useInverterStore.setState({ snapshot: snap(30) }));
    let settled = false;
    void result.then(() => { settled = true; });
    await vi.advanceTimersByTimeAsync(5_000);
    expect(settled).toBe(false);
    act(() => useInverterStore.setState({ snapshot: snap(17) }));
    await expect(result).resolves.toBe('confirmed');
  });

  it('times out when the inverter never shows the value', async () => {
    const before = useInverterStore.getState().snapshot;
    const result = waitForSnapshotReadback(isRate(17), before, 20_000);
    await vi.advanceTimersByTimeAsync(19_999);
    act(() => useInverterStore.setState({ snapshot: snap(50) }));
    await vi.advanceTimersByTimeAsync(2);
    await expect(result).resolves.toBe('timeout');
  });

  it('stops listening once settled', async () => {
    const before = useInverterStore.getState().snapshot;
    const result = waitForSnapshotReadback(isRate(17), before, 20_000);
    act(() => useInverterStore.setState({ snapshot: snap(17) }));
    await result;
    expect(vi.getTimerCount()).toBe(0);
  });

  it('resolves aborted and clears its timer when the caller goes away', async () => {
    const controller = new AbortController();
    const before = useInverterStore.getState().snapshot;
    const result = waitForSnapshotReadback(isRate(17), before, 20_000, controller.signal);
    controller.abort();
    await expect(result).resolves.toBe('aborted');
    expect(vi.getTimerCount()).toBe(0);
    // A later matching snapshot must not matter.
    act(() => useInverterStore.setState({ snapshot: snap(17) }));
  });

  it('resolves aborted immediately for an already-aborted signal', async () => {
    const controller = new AbortController();
    controller.abort();
    const before = useInverterStore.getState().snapshot;
    await expect(waitForSnapshotReadback(isRate(17), before, 20_000, controller.signal)).resolves.toBe('aborted');
    expect(vi.getTimerCount()).toBe(0);
  });
});
