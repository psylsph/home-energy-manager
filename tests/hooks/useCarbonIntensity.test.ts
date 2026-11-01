import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, act } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({ apiGet: vi.fn() }));

import { apiGet } from '../../src/lib/api';
import { CARBON_POLL_MS, useCarbonIntensity } from '../../src/hooks/useCarbonIntensity';

/** House-view carbon intensity polling (issue #401). */

const NOW = Date.parse('2026-11-01T13:10:00Z');
const reply = (forecast = 183) => ({
  ok: true,
  data: {
    region: 'South England',
    fetched_at: '2026-11-01T13:05:00Z',
    periods: [
      { from: '2026-11-01T13:00:00Z', to: '2026-11-01T13:30:00Z', forecast, index: 'high' },
      { from: '2026-11-01T13:30:00Z', to: '2026-11-01T14:00:00Z', forecast: 90, index: 'low' },
    ],
  },
});

async function flush() {
  await act(async () => { await Promise.resolve(); });
}

describe('useCarbonIntensity', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(NOW);
    vi.mocked(apiGet).mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('shows the current half hour', async () => {
    vi.mocked(apiGet).mockResolvedValue(reply());
    const { result } = renderHook(() => useCarbonIntensity());
    expect(result.current).toBeNull();
    await flush();
    expect(result.current?.intensity).toBe(183);
    expect(apiGet).toHaveBeenCalledWith('/api/carbon-intensity');
  });

  it('stays hidden with no forecast or a not-ok reply', async () => {
    vi.mocked(apiGet).mockResolvedValueOnce({ ok: true, data: null });
    const { result, unmount } = renderHook(() => useCarbonIntensity());
    await flush();
    expect(result.current).toBeNull();
    unmount();

    vi.mocked(apiGet).mockResolvedValueOnce({ ok: false, data: null });
    const second = renderHook(() => useCarbonIntensity());
    await flush();
    expect(second.result.current).toBeNull();
  });

  it('refreshes every 5 minutes and keeps the last forecast when a refresh fails', async () => {
    expect(CARBON_POLL_MS).toBe(5 * 60 * 1000);
    vi.mocked(apiGet).mockResolvedValueOnce(reply()).mockRejectedValueOnce(new Error('offline'));
    const { result } = renderHook(() => useCarbonIntensity());
    await flush();
    await act(async () => { vi.advanceTimersByTime(CARBON_POLL_MS - 1); });
    expect(apiGet).toHaveBeenCalledTimes(1);
    await act(async () => { vi.advanceTimersByTime(1); });
    await flush();
    expect(apiGet).toHaveBeenCalledTimes(2);
    expect(result.current?.intensity).toBe(183);
  });

  it('shows the forecast soon after a first reply with none', async () => {
    // The backend retries 5 minutes after a failure; the view follows suit.
    vi.mocked(apiGet).mockResolvedValueOnce({ ok: true, data: null }).mockResolvedValueOnce(reply());
    const { result } = renderHook(() => useCarbonIntensity());
    await flush();
    expect(result.current).toBeNull();
    await act(async () => { vi.advanceTimersByTime(CARBON_POLL_MS); });
    await flush();
    expect(result.current?.intensity).toBe(183);
  });

  it('moves to the next half hour between polls and hides when the forecast runs out', async () => {
    vi.mocked(apiGet).mockResolvedValueOnce(reply()).mockRejectedValue(new Error('offline'));
    const { result } = renderHook(() => useCarbonIntensity());
    await flush();
    await act(async () => { vi.advanceTimersByTime(21 * 60_000); }); // 13:31
    expect(result.current?.intensity).toBe(90);
    await act(async () => { vi.advanceTimersByTime(30 * 60_000); }); // 14:01
    expect(result.current).toBeNull();
  });

  it('stops polling and ignores a late reply after unmount', async () => {
    let resolve!: (v: unknown) => void;
    vi.mocked(apiGet).mockReturnValue(new Promise((r) => { resolve = r; }) as never);
    const { result, unmount } = renderHook(() => useCarbonIntensity());
    unmount();
    await act(async () => { resolve(reply()); await Promise.resolve(); });
    expect(result.current).toBeNull();
    vi.advanceTimersByTime(CARBON_POLL_MS * 2);
    expect(apiGet).toHaveBeenCalledTimes(1);
  });
});
