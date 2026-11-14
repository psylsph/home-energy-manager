import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, waitFor, cleanup, act } from '@testing-library/react';

vi.mock('../../src/lib/api', () => ({ apiGet: vi.fn() }));

import { apiGet } from '../../src/lib/api';
import { CAR_CHARGE_POLL_MS, CAR_CHARGE_RECHECK_MS, useCarChargeSlots } from '../../src/hooks/useCarChargeSlots';
import { useInverterStore } from '../../src/store/useInverterStore';

/** Issue #474: the Status page asks for the car's slots only when switched on. */

const answer = {
  ok: true, enabled: false, configured: true, checked_at: '2026-11-01T17:58:00Z', error: null,
  devices: 1, holding: false, hold_failing: false, stale_after_minutes: 15, dispatches: [],
};

describe('useCarChargeSlots', () => {
  beforeEach(() => {
    vi.mocked(apiGet).mockReset();
    useInverterStore.setState({ showCarCharging: false, navSettingsLoaded: true });
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    useInverterStore.setState({ showCarCharging: false });
  });

  it("isn't ready until the settings say whether it's on, so the card doesn't show and then jump", () => {
    useInverterStore.setState({ navSettingsLoaded: false });
    const { result } = renderHook(() => useCarChargeSlots());
    expect(result.current).toEqual({ response: null, loaded: false });
    expect(apiGet).not.toHaveBeenCalled();
    act(() => useInverterStore.setState({ navSettingsLoaded: true }));
    expect(result.current).toEqual({ response: null, loaded: true });
  });

  it('asks nothing while the switch is off, and is ready at once', () => {
    const { result } = renderHook(() => useCarChargeSlots());
    expect(result.current).toEqual({ response: null, loaded: true });
    expect(apiGet).not.toHaveBeenCalled();
  });

  it('fetches the dispatches while on, and refreshes them every minute', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(apiGet).mockResolvedValue(answer);
    useInverterStore.setState({ showCarCharging: true });
    const { result } = renderHook(() => useCarChargeSlots());
    expect(result.current.loaded).toBe(false);
    await waitFor(() => expect(result.current.loaded).toBe(true));
    expect(result.current.response?.devices).toBe(1);
    expect(apiGet).toHaveBeenCalledWith('/api/octopus/dispatches', expect.anything());
    await act(async () => { vi.advanceTimersByTime(CAR_CHARGE_POLL_MS); });
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(2));
  });

  it("asks again soon when the backend hasn't checked yet, as just after switching it on", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(apiGet)
      .mockResolvedValueOnce({ ...answer, checked_at: null })
      .mockResolvedValue(answer);
    useInverterStore.setState({ showCarCharging: true });
    const { result } = renderHook(() => useCarChargeSlots());
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(1));
    await act(async () => { vi.advanceTimersByTime(CAR_CHARGE_RECHECK_MS); });
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(result.current.response?.checked_at).toBe(answer.checked_at));
  });

  it('keeps the last good answer when a refresh fails or comes back unusable', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(apiGet)
      .mockResolvedValueOnce(answer)
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce({ ok: false });
    useInverterStore.setState({ showCarCharging: true });
    const { result } = renderHook(() => useCarChargeSlots());
    await waitFor(() => expect(result.current.response).not.toBeNull());
    await act(async () => { vi.advanceTimersByTime(CAR_CHARGE_POLL_MS); });
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(2));
    await act(async () => { vi.advanceTimersByTime(CAR_CHARGE_POLL_MS); });
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(3));
    expect(result.current.response?.devices).toBe(1);
  });

  it('is ready, with nothing to show, when the first request fails', async () => {
    vi.mocked(apiGet).mockRejectedValue(new Error('offline'));
    useInverterStore.setState({ showCarCharging: true });
    const { result } = renderHook(() => useCarChargeSlots());
    await waitFor(() => expect(result.current.loaded).toBe(true));
    expect(result.current.response).toBeNull();
  });

  it('forgets the answer and stops asking when switched off', async () => {
    vi.mocked(apiGet).mockResolvedValue(answer);
    useInverterStore.setState({ showCarCharging: true });
    const { result } = renderHook(() => useCarChargeSlots());
    await waitFor(() => expect(result.current.response).not.toBeNull());
    act(() => useInverterStore.setState({ showCarCharging: false }));
    expect(result.current).toEqual({ response: null, loaded: true });
  });
});
