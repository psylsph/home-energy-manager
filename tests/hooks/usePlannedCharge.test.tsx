import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, waitFor, cleanup, act } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({ apiGet: vi.fn() }));

import { apiGet } from '../../src/lib/api';
import { PLANNED_CHARGE_POLL_MS, usePlannedCharge } from '../../src/hooks/usePlannedCharge';

/** Issue #481: the Status page asks when the Planner next writes the slot. */

const answer = (extra: Record<string, unknown> = {}) => ({
  ok: true,
  data: {
    trigger: 'auto_apply', cheap_start: '22:00', cheap_end: '23:59', applies_at: '21:30',
    applies_in_minutes: 264, applies_day: 0, running: false, ...extra,
  },
});

describe('usePlannedCharge', () => {
  beforeEach(() => {
    vi.mocked(apiGet).mockReset();
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it('asks nothing while off, and is ready at once', () => {
    const { result } = renderHook(() => usePlannedCharge(false));
    expect(result.current).toEqual({ upcoming: null, fetchedAt: 0, loaded: true });
    expect(apiGet).not.toHaveBeenCalled();
  });

  it('fetches the upcoming write and refreshes it every minute', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(apiGet).mockResolvedValue(answer());
    const { result } = renderHook(() => usePlannedCharge(true));
    expect(result.current.loaded).toBe(false);
    await waitFor(() => expect(result.current.loaded).toBe(true));
    expect(result.current.upcoming?.appliesAt).toBe('21:30');
    expect(result.current.fetchedAt).toBeGreaterThan(0);
    expect(apiGet).toHaveBeenCalledWith('/api/forecast/plan/upcoming', expect.anything());
    await act(async () => { vi.advanceTimersByTime(PLANNED_CHARGE_POLL_MS); });
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(2));
  });

  it('takes nothing coming as an answer, and clears the last one', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(apiGet).mockResolvedValueOnce(answer()).mockResolvedValue({ ok: true, data: null });
    const { result } = renderHook(() => usePlannedCharge(true));
    await waitFor(() => expect(result.current.upcoming).not.toBeNull());
    await act(async () => { vi.advanceTimersByTime(PLANNED_CHARGE_POLL_MS); });
    await waitFor(() => expect(result.current.upcoming).toBeNull());
    expect(result.current.loaded).toBe(true);
  });

  it('keeps the last good answer, and its time, when a refresh fails or is unusable', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(apiGet)
      .mockResolvedValueOnce(answer())
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce({ ok: false });
    const { result } = renderHook(() => usePlannedCharge(true));
    await waitFor(() => expect(result.current.upcoming).not.toBeNull());
    const fetchedAt = result.current.fetchedAt;
    await act(async () => { vi.advanceTimersByTime(PLANNED_CHARGE_POLL_MS); });
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(2));
    await act(async () => { vi.advanceTimersByTime(PLANNED_CHARGE_POLL_MS); });
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(3));
    expect(result.current.upcoming?.appliesAt).toBe('21:30');
    expect(result.current.fetchedAt).toBe(fetchedAt);
  });

  it('is ready even when the first request fails', async () => {
    vi.mocked(apiGet).mockRejectedValue(new Error('offline'));
    const { result } = renderHook(() => usePlannedCharge(true));
    await waitFor(() => expect(result.current.loaded).toBe(true));
    expect(result.current.upcoming).toBeNull();
  });

  it('does not stack requests while one is still in flight, and stops on unmount', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let signal: AbortSignal | undefined;
    vi.mocked(apiGet).mockImplementation((_path, s) => {
      signal = s;
      return new Promise(() => {});
    });
    const { unmount } = renderHook(() => usePlannedCharge(true));
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(1));
    await act(async () => { vi.advanceTimersByTime(PLANNED_CHARGE_POLL_MS); });
    expect(apiGet).toHaveBeenCalledTimes(1);
    unmount();
    expect(signal?.aborted).toBe(true);
  });
});
