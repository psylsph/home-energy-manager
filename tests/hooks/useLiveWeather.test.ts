import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, act } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({ apiGet: vi.fn() }));

import { apiGet } from '../../src/lib/api';
import { LIVE_WEATHER_POLL_MS, useLiveWeather } from '../../src/hooks/useLiveWeather';

/** House-view weather polling (issue #422). */

const NOW = Date.parse('2026-10-31T12:00:00Z');
const fresh = (over: Record<string, unknown> = {}) => ({
  ok: true,
  data: {
    config: { enabled: true },
    last_fetch_at: '2026-10-31T11:55:00Z',
    last_fetched_temperature_c: 9.6,
    last_fetched_weather_code: 3,
    last_fetched_is_day: true,
    ...over,
  },
});

async function flush() {
  await act(async () => { await Promise.resolve(); });
}

describe('useLiveWeather', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(NOW);
    vi.mocked(apiGet).mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('shows the fetched weather', async () => {
    vi.mocked(apiGet).mockResolvedValue(fresh());
    const { result } = renderHook(() => useLiveWeather());
    expect(result.current.weather).toBeNull();
    await flush();
    expect(result.current.weather).toEqual({ temperatureC: 9.6, kind: 'cloudy', description: 'Cloudy', solarWm2: null });
    expect(apiGet).toHaveBeenCalledWith('/api/weather');
  });

  it('stays hidden when weather is turned off', async () => {
    vi.mocked(apiGet).mockResolvedValue(fresh({ config: { enabled: false } }));
    const { result } = renderHook(() => useLiveWeather());
    await flush();
    expect(result.current.weather).toBeNull();
  });

  it('ignores a not-ok response', async () => {
    vi.mocked(apiGet).mockResolvedValue({ ok: false, data: null });
    const { result } = renderHook(() => useLiveWeather());
    await flush();
    expect(result.current.weather).toBeNull();
  });

  it('refreshes every 15 minutes and keeps the last reading when a refresh fails', async () => {
    vi.mocked(apiGet).mockResolvedValueOnce(fresh()).mockRejectedValueOnce(new Error('offline'));
    const { result } = renderHook(() => useLiveWeather());
    await flush();
    expect(result.current.weather?.temperatureC).toBe(9.6);

    await act(async () => { vi.advanceTimersByTime(LIVE_WEATHER_POLL_MS); });
    await flush();
    expect(apiGet).toHaveBeenCalledTimes(2);
    expect(result.current.weather?.temperatureC).toBe(9.6);
  });

  it('drops the reading once it goes stale, even without a new fetch', async () => {
    vi.mocked(apiGet).mockResolvedValueOnce(fresh()).mockRejectedValue(new Error('offline'));
    const { result } = renderHook(() => useLiveWeather());
    await flush();
    expect(result.current.weather).not.toBeNull();

    // 11:55 fetch + 61 minutes → stale; the minute clock notices.
    await act(async () => { vi.advanceTimersByTime(61 * 60 * 1000); });
    await flush();
    expect(result.current.weather).toBeNull();
  });

  it('keeps the location for the sky after the reading goes stale', async () => {
    vi.mocked(apiGet).mockResolvedValueOnce(fresh({ config: { enabled: true, latitude: 51.1, longitude: -1.05 } }))
      .mockRejectedValue(new Error('offline'));
    const { result } = renderHook(() => useLiveWeather());
    await flush();
    expect(result.current.location).toEqual({ latitude: 51.1, longitude: -1.05 });

    await act(async () => { vi.advanceTimersByTime(61 * 60 * 1000); });
    await flush();
    expect(result.current.weather).toBeNull();
    expect(result.current.location).toEqual({ latitude: 51.1, longitude: -1.05 });
  });

  it('stops polling and ignores a late reply after unmount', async () => {
    let resolve!: (v: unknown) => void;
    vi.mocked(apiGet).mockReturnValue(new Promise((r) => { resolve = r; }) as never);
    const { result, unmount } = renderHook(() => useLiveWeather());
    unmount();
    await act(async () => { resolve(fresh()); await Promise.resolve(); });
    expect(result.current.weather).toBeNull();
    vi.advanceTimersByTime(LIVE_WEATHER_POLL_MS * 2);
    expect(apiGet).toHaveBeenCalledTimes(1);
  });
});
