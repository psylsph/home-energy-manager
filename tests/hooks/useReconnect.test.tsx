import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, act } from '@testing-library/react';

// Replace only apiPost — the hook uses nothing else from this module.
vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn(),
}));

import { useReconnect } from '../../src/hooks/useReconnect';
import { apiPost } from '../../src/lib/api';
import { useInverterStore } from '../../src/store/useInverterStore';

const mockApiPost = vi.mocked(apiPost);

/** A controllable promise so tests can observe the in-flight `reconnecting` state. */
function deferred() {
  let resolve!: (value: unknown) => void;
  const promise = new Promise((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

describe('useReconnect', () => {
  beforeEach(() => {
    // Fake timers drive the label-reset timeout precisely. Microtasks (promise
    // resolution) are not faked, so `await apiPost(...)` still resolves.
    vi.useFakeTimers();
    mockApiPost.mockReset();
    useInverterStore.setState({ reconnectRequestedAt: null });
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    useInverterStore.setState({ reconnectRequestedAt: null });
  });

  it('starts idle', () => {
    const { result } = renderHook(() => useReconnect());
    expect(result.current.reconnecting).toBe(false);
  });

  it('exposes a stable reconnect reference across renders', () => {
    const { result, rerender } = renderHook(() => useReconnect());
    const first = result.current.reconnect;
    rerender();
    rerender();
    expect(result.current.reconnect).toBe(first);
  });

  it('flips reconnecting on and POSTs /api/reconnect immediately on click', () => {
    const { promise } = deferred();
    mockApiPost.mockReturnValue(promise);

    const { result } = renderHook(() => useReconnect());
    let pending!: Promise<void>;
    act(() => {
      pending = result.current.reconnect();
    });

    expect(result.current.reconnecting).toBe(true);
    expect(mockApiPost).toHaveBeenCalledWith('/api/reconnect');

    // Avoid an unhandled-rejection by resolving the dangling promise.
    void pending;
    void promise;
  });

  it('stamps the shared reconnectRequestedAt timestamp once the request lands', async () => {
    vi.setSystemTime(new Date('2024-01-01T12:00:00Z'));
    mockApiPost.mockResolvedValue({ ok: true });

    const { result } = renderHook(() => useReconnect());
    await act(async () => {
      await result.current.reconnect();
    });

    // The store timestamp is what the header notice reads — this is the
    // whole point of the change: visible feedback even against a dead dongle.
    expect(useInverterStore.getState().reconnectRequestedAt).toBe(Date.now());
  });

  it('stamps the timestamp even when the request rejects (the click still counts)', async () => {
    vi.setSystemTime(new Date('2024-01-01T12:00:00Z'));
    mockApiPost.mockRejectedValueOnce(new Error('network down'));

    const { result } = renderHook(() => useReconnect());
    await act(async () => {
      await result.current.reconnect();
    });

    // The swallow-on-error path must still record the attempt so the user
    // sees their click registered. Without this, a flaky LAN wouldn't give
    // any feedback at all.
    expect(useInverterStore.getState().reconnectRequestedAt).toBe(Date.now());
  });

  it('holds the reconnecting label for the reset window, then clears it', async () => {
    mockApiPost.mockResolvedValue({ ok: true });
    const { result } = renderHook(() => useReconnect());

    await act(async () => {
      await result.current.reconnect();
    });
    expect(result.current.reconnecting).toBe(true);

    // One millisecond before the window elapses the label is still showing.
    act(() => {
      vi.advanceTimersByTime(2999);
    });
    expect(result.current.reconnecting).toBe(true);

    // Cross the 3000ms boundary — the button releases.
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(result.current.reconnecting).toBe(false);
  });

  it('cancels the pending reset timeout on unmount (no setState after unmount)', async () => {
    const setTimeoutSpy = vi.spyOn(globalThis, 'setTimeout');
    const clearTimeoutSpy = vi.spyOn(globalThis, 'clearTimeout');

    mockApiPost.mockResolvedValue({ ok: true });
    const { result, unmount } = renderHook(() => useReconnect());

    await act(async () => {
      await result.current.reconnect();
    });
    expect(result.current.reconnecting).toBe(true);

    // The reset timeout is the most recently scheduled timer after the
    // request resolves. Capture it to prove unmount cancels exactly it.
    const resetHandle = setTimeoutSpy.mock.results.at(-1)?.value;
    expect(resetHandle).toBeDefined();

    unmount();
    expect(clearTimeoutSpy).toHaveBeenCalledWith(resetHandle);

    // Advancing the clock after unmount must be harmless.
    expect(() => {
      act(() => {
        vi.advanceTimersByTime(5000);
      });
    }).not.toThrow();
  });

  it('does not stack reset timeouts across rapid invocations', async () => {
    const setTimeoutSpy = vi.spyOn(globalThis, 'setTimeout');
    const clearTimeoutSpy = vi.spyOn(globalThis, 'clearTimeout');

    mockApiPost.mockResolvedValue({ ok: true });
    const { result } = renderHook(() => useReconnect());

    await act(async () => {
      await result.current.reconnect();
    });
    const firstResetHandle = setTimeoutSpy.mock.results.at(-1)?.value;
    expect(firstResetHandle).toBeDefined();

    // A second click before the first reset fires must cancel the pending
    // first reset so the two never race to update the same state.
    await act(async () => {
      await result.current.reconnect();
    });
    expect(clearTimeoutSpy).toHaveBeenCalledWith(firstResetHandle);
  });
});
