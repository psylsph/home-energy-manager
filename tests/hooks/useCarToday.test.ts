import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';

const apiGetMock = vi.fn();
vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGetMock(...args),
}));

import { useCarToday, CAR_TODAY_REFRESH_MS } from '../../src/hooks/useCarToday';

const reply = (enabled: boolean, kwh: number | null) => ({ ok: true, data: { enabled, car_kwh: kwh } });

describe('useCarToday (issue #454)', () => {
  beforeEach(() => {
    apiGetMock.mockReset();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('is null until the first answer, then the car kWh', async () => {
    apiGetMock.mockResolvedValue(reply(true, 7.4));
    const { result } = renderHook(() => useCarToday());
    expect(result.current).toBeNull();
    await waitFor(() => expect(result.current).toBe(7.4));
    expect(apiGetMock).toHaveBeenCalledWith('/api/history/car-today');
  });

  it('stays null when the feature is off', async () => {
    apiGetMock.mockResolvedValue(reply(false, null));
    const { result } = renderHook(() => useCarToday());
    await waitFor(() => expect(apiGetMock).toHaveBeenCalled());
    expect(result.current).toBeNull();
  });

  it('asks again each minute, not on every render', async () => {
    vi.useFakeTimers();
    apiGetMock.mockResolvedValue(reply(true, 1));
    const { rerender } = renderHook(() => useCarToday());
    await act(async () => { await vi.advanceTimersByTimeAsync(0); });
    expect(apiGetMock).toHaveBeenCalledTimes(1);
    rerender();
    rerender();
    expect(apiGetMock).toHaveBeenCalledTimes(1);
    apiGetMock.mockResolvedValue(reply(true, 2));
    await act(async () => { await vi.advanceTimersByTimeAsync(CAR_TODAY_REFRESH_MS); });
    expect(apiGetMock).toHaveBeenCalledTimes(2);
  });

  it('keeps the last figure when a refresh fails', async () => {
    vi.useFakeTimers();
    apiGetMock.mockResolvedValueOnce(reply(true, 3.2));
    const { result } = renderHook(() => useCarToday());
    await act(async () => { await vi.advanceTimersByTimeAsync(0); });
    expect(result.current).toBe(3.2);
    apiGetMock.mockRejectedValue(new Error('offline'));
    await act(async () => { await vi.advanceTimersByTimeAsync(CAR_TODAY_REFRESH_MS); });
    expect(result.current).toBe(3.2);
  });

  it('drops the figure when the feature is switched off', async () => {
    vi.useFakeTimers();
    apiGetMock.mockResolvedValueOnce(reply(true, 3.2));
    const { result } = renderHook(() => useCarToday());
    await act(async () => { await vi.advanceTimersByTimeAsync(0); });
    apiGetMock.mockResolvedValue(reply(false, null));
    await act(async () => { await vi.advanceTimersByTimeAsync(CAR_TODAY_REFRESH_MS); });
    expect(result.current).toBeNull();
  });

  it('stops asking, and ignores a late answer, once unmounted', async () => {
    vi.useFakeTimers();
    let resolve: (v: unknown) => void = () => {};
    apiGetMock.mockReturnValue(new Promise((r) => { resolve = r; }));
    const { unmount } = renderHook(() => useCarToday());
    unmount();
    resolve(reply(true, 9));
    await act(async () => { await vi.advanceTimersByTimeAsync(CAR_TODAY_REFRESH_MS * 3); });
    expect(apiGetMock).toHaveBeenCalledTimes(1);
  });

  it("does not carry yesterday's figure into today when refreshes keep failing", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 10, 7, 23, 58, 0));
    apiGetMock.mockResolvedValueOnce(reply(true, 11.2));
    const { result, rerender } = renderHook(() => useCarToday());
    await act(async () => { await vi.advanceTimersByTimeAsync(0); });
    expect(result.current).toBe(11.2);

    // Past midnight, with the backend unreachable: the figure is no longer today's.
    apiGetMock.mockRejectedValue(new Error('offline'));
    await act(async () => { await vi.advanceTimersByTimeAsync(3 * 60_000); });
    rerender();
    expect(result.current).toBeNull();
  });

  it('keeps the figure across the minutes of the same day', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 10, 7, 10, 0, 0));
    apiGetMock.mockResolvedValueOnce(reply(true, 3.2));
    const { result, rerender } = renderHook(() => useCarToday());
    await act(async () => { await vi.advanceTimersByTimeAsync(0); });
    apiGetMock.mockRejectedValue(new Error('offline'));
    await act(async () => { await vi.advanceTimersByTimeAsync(5 * 60_000); });
    rerender();
    expect(result.current).toBe(3.2);
  });
});
