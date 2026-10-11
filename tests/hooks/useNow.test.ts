import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, act } from '@testing-library/react';
import { useNow } from '../../src/hooks/useNow';

const START = Date.UTC(2026, 9, 11, 12, 0, 0);

// Fake timers move Date.now() forward as they fire, so these tests only ever
// advance the clock through advanceTimersByTime.
describe('useNow', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(START);
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('starts at the current time', () => {
    const { result } = renderHook(() => useNow(60_000));
    expect(result.current).toBe(START);
  });

  it('advances on each interval tick', () => {
    const { result } = renderHook(() => useNow(60_000));
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    expect(result.current).toBe(START + 60_000);
  });

  it('does not tick before the interval has elapsed', () => {
    const { result } = renderHook(() => useNow(60_000));
    act(() => {
      vi.advanceTimersByTime(59_999);
    });
    expect(result.current).toBe(START);
  });

  it('runs no clock at all when given null', () => {
    const { result } = renderHook(() => useNow(null));
    expect(vi.getTimerCount()).toBe(0);
    act(() => {
      vi.advanceTimersByTime(3_600_000);
    });
    expect(result.current).toBe(START);
  });

  it('catches up with the clock as soon as it is switched back on', () => {
    const { result, rerender } = renderHook(({ ms }: { ms: number | null }) => useNow(ms), {
      initialProps: { ms: null as number | null },
    });
    act(() => {
      vi.advanceTimersByTime(600_000);
    });
    rerender({ ms: 5_000 });
    act(() => {
      vi.advanceTimersByTime(0);
    });
    expect(result.current).toBe(START + 600_000);
  });

  it('stops the timer on unmount', () => {
    const { unmount } = renderHook(() => useNow(60_000));
    act(() => {
      vi.advanceTimersByTime(0);
    });
    expect(vi.getTimerCount()).toBe(1);
    unmount();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('restarts the clock at the new interval when it changes', () => {
    const { result, rerender } = renderHook(({ ms }: { ms: number | null }) => useNow(ms), {
      initialProps: { ms: 60_000 as number | null },
    });
    rerender({ ms: 5_000 });
    act(() => {
      vi.advanceTimersByTime(5_000);
    });
    expect(result.current).toBe(START + 5_000);
  });
});
