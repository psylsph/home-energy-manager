import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, act } from '@testing-library/react';
import { useAction } from '../../src/hooks/useAction';
import { apiPost } from '../../src/lib/api';

// Replace only apiPost — the hook uses nothing else from this module, so a
// partial mock keeps the rest of the real module intact.
vi.mock('../../src/lib/api', () => ({
  apiPost: vi.fn(),
}));

const mockApiPost = vi.mocked(apiPost);

/** A controllable promise so tests can observe the in-flight `loading` state. */
function deferred() {
  let resolve!: (value: unknown) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe('useAction', () => {
  beforeEach(() => {
    // Fake timers let us assert exact reset delays and synchronously drive the
    // feedback-clearing timeouts. Microtasks (promise resolution) are not
    // faked, so `await apiPost(...)` still resolves normally.
    vi.useFakeTimers();
    mockApiPost.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('starts idle', () => {
    const { result } = renderHook(() => useAction());
    expect(result.current.loading).toBe(false);
    expect(result.current.success).toBe(false);
    expect(result.current.error).toBeNull();
  });

  it('exposes a stable execute reference across renders', () => {
    const { result, rerender } = renderHook(() => useAction());
    const first = result.current.execute;
    rerender();
    rerender();
    expect(result.current.execute).toBe(first);
  });

  it('shows loading while the request is in flight, then success', async () => {
    const { promise, resolve } = deferred();
    mockApiPost.mockReturnValue(promise);

    const { result } = renderHook(() => useAction());
    let pending!: Promise<void>;
    act(() => {
      pending = result.current.execute('/api/foo', { a: 1 });
    });

    expect(result.current.loading).toBe(true);
    expect(result.current.success).toBe(false);
    expect(mockApiPost).toHaveBeenCalledWith('/api/foo', { a: 1 });

    await act(async () => {
      resolve({ ok: true });
      await pending;
    });

    expect(result.current.loading).toBe(false);
    expect(result.current.success).toBe(true);
    expect(result.current.error).toBeNull();
  });

  it('clears the success badge exactly after 2000ms', async () => {
    mockApiPost.mockResolvedValue({ ok: true });
    const { result } = renderHook(() => useAction());

    await act(async () => {
      await result.current.execute('/api/foo');
    });
    expect(result.current.success).toBe(true);

    // One millisecond before the window elapses, success is still showing.
    act(() => {
      vi.advanceTimersByTime(1999);
    });
    expect(result.current.success).toBe(true);

    // Cross the 2000ms boundary — the feedback clears.
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(result.current.success).toBe(false);
    expect(result.current.error).toBeNull();
  });

  it('surfaces an error message and clears it exactly after 3000ms', async () => {
    mockApiPost.mockRejectedValueOnce(new Error('dongle busy'));
    const { result } = renderHook(() => useAction());

    await act(async () => {
      await result.current.execute('/api/foo');
    });
    expect(result.current.loading).toBe(false);
    expect(result.current.success).toBe(false);
    expect(result.current.error).toBe('dongle busy');

    act(() => {
      vi.advanceTimersByTime(2999);
    });
    expect(result.current.error).toBe('dongle busy');

    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(result.current.error).toBeNull();
    expect(result.current.success).toBe(false);
  });

  it('can be invoked again after a feedback reset (cycle repeats)', async () => {
    mockApiPost.mockResolvedValue({ ok: true });
    const { result } = renderHook(() => useAction());

    await act(async () => {
      await result.current.execute('/a');
    });
    expect(result.current.success).toBe(true);

    act(() => {
      vi.advanceTimersByTime(2000);
    });
    expect(result.current.success).toBe(false);

    // Second cycle behaves identically.
    await act(async () => {
      await result.current.execute('/b');
    });
    expect(result.current.success).toBe(true);
    act(() => {
      vi.advanceTimersByTime(2000);
    });
    expect(result.current.success).toBe(false);
  });

  it('cancels the pending reset timeout on unmount (no setState after unmount)', async () => {
    const setTimeoutSpy = vi.spyOn(globalThis, 'setTimeout');
    const clearTimeoutSpy = vi.spyOn(globalThis, 'clearTimeout');

    mockApiPost.mockResolvedValue({ ok: true });
    const { result, unmount } = renderHook(() => useAction());

    await act(async () => {
      await result.current.execute('/api/foo');
    });
    expect(result.current.success).toBe(true);

    // The reset timeout is the most recently scheduled timer after the success
    // path resolves. Capture its handle so we can prove the unmount cleanup
    // cancels exactly this one (and not just "some" timer).
    const resetHandle = setTimeoutSpy.mock.results.at(-1)?.value;
    expect(resetHandle).toBeDefined();

    unmount();

    expect(clearTimeoutSpy).toHaveBeenCalledWith(resetHandle);

    // Advancing the clock after unmount must be harmless — the timer was
    // cancelled, so no state update can be queued for the unmounted hook.
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
    const { result } = renderHook(() => useAction());

    // First invocation succeeds and schedules a 2000ms reset.
    await act(async () => {
      await result.current.execute('/first');
    });
    const firstResetHandle = setTimeoutSpy.mock.results.at(-1)?.value;
    expect(firstResetHandle).toBeDefined();

    // A second invocation before the first reset fires must cancel the pending
    // first reset so the two timeouts never race to update the same state.
    await act(async () => {
      await result.current.execute('/second');
    });
    expect(clearTimeoutSpy).toHaveBeenCalledWith(firstResetHandle);

    // After advancing past the original window, exactly one reset fires.
    act(() => {
      vi.advanceTimersByTime(3000);
    });
    expect(result.current.success).toBe(false);
  });

  it('a new invocation replaces a stale success badge', async () => {
    mockApiPost.mockResolvedValue({ ok: true });
    const { result } = renderHook(() => useAction());

    await act(async () => {
      await result.current.execute('/first');
    });
    expect(result.current.success).toBe(true);

    // Start a second call mid-success-window: loading takes over and the old
    // success badge is cleared immediately (not left showing stale success).
    const { promise, resolve } = deferred();
    mockApiPost.mockReturnValue(promise);

    let pending!: Promise<void>;
    act(() => {
      pending = result.current.execute('/second');
    });
    expect(result.current.loading).toBe(true);
    expect(result.current.success).toBe(false);

    await act(async () => {
      resolve({ ok: true });
      await pending;
    });
    expect(result.current.loading).toBe(false);
    expect(result.current.success).toBe(true);
  });
});
