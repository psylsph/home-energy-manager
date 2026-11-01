import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, act, cleanup } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({ apiGet: vi.fn() }));

import { apiGet } from '../../src/lib/api';
import { TIMED_EXPORT_POLL_MS, TIMED_EXPORT_TIMEOUT_MS, useTimedExportSchedule } from '../../src/hooks/useTimedExportSchedule';

/** House view schedule card: the HEM-managed Timed Export schedule (issue #422). */
describe('useTimedExportSchedule', () => {
  const slot = { enabled: true, start_hour: 16, start_minute: 0, end_hour: 19, end_minute: 0, target_soc: 4 };

  beforeEach(() => {
    vi.useFakeTimers();
    vi.mocked(apiGet).mockReset();
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it('is not loaded until the first answer, then holds the schedule', async () => {
    vi.mocked(apiGet).mockResolvedValue({ ok: true, data: { schedule_enabled: true, slots: [slot], state: 'Idle' } });
    const { result } = renderHook(() => useTimedExportSchedule());
    expect(result.current).toEqual({ schedule: null, loaded: false });
    await act(async () => {});
    expect(result.current).toEqual({ schedule: { schedule_enabled: true, slots: [slot] }, loaded: true });
    expect(apiGet).toHaveBeenCalledWith('/api/timed-export', expect.any(AbortSignal));
  });

  it('treats a reply that is not a schedule as no answer, as the Control page does', async () => {
    vi.mocked(apiGet).mockResolvedValue({ ok: true, data: {} });
    const { result } = renderHook(() => useTimedExportSchedule());
    await act(async () => {});
    expect(result.current).toEqual({ schedule: null, loaded: true });
  });

  it('fills in missing slots as none', async () => {
    vi.mocked(apiGet).mockResolvedValue({ ok: true, data: { schedule_enabled: false } });
    const { result } = renderHook(() => useTimedExportSchedule());
    await act(async () => {});
    expect(result.current).toEqual({ schedule: { schedule_enabled: false, slots: [] }, loaded: true });
  });

  it('gives up on a stalled request after the timeout, so the card falls back', async () => {
    vi.mocked(apiGet).mockImplementationOnce((_path, signal) => new Promise((_, reject) => {
      signal!.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')));
    }) as never);
    const { result } = renderHook(() => useTimedExportSchedule());
    await act(async () => { vi.advanceTimersByTime(TIMED_EXPORT_TIMEOUT_MS - 1); });
    expect(result.current.loaded).toBe(false);
    await act(async () => { vi.advanceTimersByTime(1); });
    expect(result.current).toEqual({ schedule: null, loaded: true });
  });

  it('times out well before the next poll, so requests never overlap', () => {
    expect(TIMED_EXPORT_TIMEOUT_MS).toBeLessThan(TIMED_EXPORT_POLL_MS / 2);
  });

  it('aborts the request in flight on unmount', async () => {
    let seen: AbortSignal | undefined;
    vi.mocked(apiGet).mockImplementationOnce((_path, signal) => {
      seen = signal;
      return new Promise(() => {}) as never;
    });
    const { unmount } = renderHook(() => useTimedExportSchedule());
    unmount();
    expect(seen!.aborted).toBe(true);
  });

  it('polls every minute and keeps the last schedule when a request fails', async () => {
    vi.mocked(apiGet).mockResolvedValueOnce({ ok: true, data: { schedule_enabled: true, slots: [slot] } });
    const { result } = renderHook(() => useTimedExportSchedule());
    await act(async () => {});
    vi.mocked(apiGet).mockRejectedValueOnce(new Error('offline'));
    await act(async () => { vi.advanceTimersByTime(TIMED_EXPORT_POLL_MS); });
    expect(result.current).toEqual({ schedule: { schedule_enabled: true, slots: [slot] }, loaded: true });
    vi.mocked(apiGet).mockResolvedValueOnce({ ok: false });
    await act(async () => { vi.advanceTimersByTime(TIMED_EXPORT_POLL_MS); });
    expect(result.current).toEqual({ schedule: { schedule_enabled: true, slots: [slot] }, loaded: true });
    expect(apiGet).toHaveBeenCalledTimes(3);
  });

  it('counts as loaded with no schedule when the first request fails, so the card can fall back', async () => {
    vi.mocked(apiGet).mockRejectedValueOnce(new Error('offline'));
    const { result } = renderHook(() => useTimedExportSchedule());
    await act(async () => {});
    expect(result.current).toEqual({ schedule: null, loaded: true });
  });

  it('counts as loaded with no schedule when the first answer is not ok', async () => {
    vi.mocked(apiGet).mockResolvedValueOnce({ ok: false });
    const { result } = renderHook(() => useTimedExportSchedule());
    await act(async () => {});
    expect(result.current).toEqual({ schedule: null, loaded: true });
  });

  it('ignores a late failure after unmount', async () => {
    let reject: (e: unknown) => void = () => {};
    vi.mocked(apiGet).mockReturnValueOnce(new Promise((_, r) => { reject = r; }) as never);
    const { result, unmount } = renderHook(() => useTimedExportSchedule());
    unmount();
    await act(async () => { reject(new Error('late')); });
    expect(result.current).toEqual({ schedule: null, loaded: false });
  });

  it('stops polling and ignores a late answer after unmount', async () => {
    let resolve: (v: unknown) => void = () => {};
    vi.mocked(apiGet).mockReturnValueOnce(new Promise((r) => { resolve = r; }) as never);
    const { result, unmount } = renderHook(() => useTimedExportSchedule());
    unmount();
    await act(async () => { resolve({ ok: true, data: { schedule_enabled: true, slots: [slot] } }); });
    expect(result.current).toEqual({ schedule: null, loaded: false });
    vi.advanceTimersByTime(TIMED_EXPORT_POLL_MS * 3);
    expect(apiGet).toHaveBeenCalledTimes(1);
  });
});
