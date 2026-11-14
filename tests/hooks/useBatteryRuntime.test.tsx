import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, waitFor, cleanup, act } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({ apiGet: vi.fn() }));

import { apiGet } from '../../src/lib/api';
import { useBatteryRuntime } from '../../src/hooks/useBatteryRuntime';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

/** Issue #472: asked for only while Show Battery Runtime is on. */

const answer = { ok: true, data: { outcome: 'at_reserve', computed_at: 1 } };
const snap = (soc: number) => ({ soc } as InverterSnapshot);

describe('useBatteryRuntime', () => {
  beforeEach(() => {
    vi.mocked(apiGet).mockReset();
    useInverterStore.setState({ showBatteryRuntime: false, snapshot: snap(50) });
  });
  afterEach(() => {
    cleanup();
    useInverterStore.setState({ showBatteryRuntime: false, snapshot: null });
  });

  it('asks nothing while off', () => {
    const { result } = renderHook(() => useBatteryRuntime());
    expect(result.current).toBeNull();
    expect(apiGet).not.toHaveBeenCalled();
  });

  it('fetches while on, and again when SOC moves two points', async () => {
    vi.mocked(apiGet).mockResolvedValue(answer);
    useInverterStore.setState({ showBatteryRuntime: true });
    const { result } = renderHook(() => useBatteryRuntime());
    await waitFor(() => expect(result.current).toEqual({ outcome: 'at_reserve', computed_at: 1 }));
    expect(apiGet).toHaveBeenCalledWith('/api/battery/runtime', expect.anything());
    act(() => useInverterStore.setState({ snapshot: snap(51) }));
    expect(apiGet).toHaveBeenCalledTimes(1);
    act(() => useInverterStore.setState({ snapshot: snap(48) }));
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(2));
  });

  it('keeps the last answer when a refresh fails', async () => {
    vi.mocked(apiGet).mockResolvedValueOnce(answer).mockRejectedValueOnce(new Error('offline'));
    useInverterStore.setState({ showBatteryRuntime: true });
    const { result } = renderHook(() => useBatteryRuntime());
    await waitFor(() => expect(result.current).not.toBeNull());
    act(() => useInverterStore.setState({ snapshot: snap(40) }));
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(2));
    expect(result.current).toEqual({ outcome: 'at_reserve', computed_at: 1 });
  });

  it('shows nothing once switched off', async () => {
    vi.mocked(apiGet).mockResolvedValue(answer);
    useInverterStore.setState({ showBatteryRuntime: true });
    const { result } = renderHook(() => useBatteryRuntime());
    await waitFor(() => expect(result.current).not.toBeNull());
    act(() => useInverterStore.setState({ showBatteryRuntime: false }));
    expect(result.current).toBeNull();
  });
});
