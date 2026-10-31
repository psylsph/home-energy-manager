import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, act } from '@testing-library/react';
import type { SmartChargingResponse } from '../../src/lib/types';

const apiGet = vi.fn();
const apiPost = vi.fn();
vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGet(...args),
  apiPost: (...args: unknown[]) => apiPost(...args),
}));

import SmartChargingPanel from '../../src/components/SmartChargingPanel';

function reply(overrides: Partial<SmartChargingResponse> = {}): SmartChargingResponse {
  return {
    ok: true,
    enabled: true,
    configured: true,
    checked_at: '2026-10-17T12:00:00Z',
    error: null,
    devices: 1,
    holding: false,
    stale_after_minutes: 15,
    dispatches: [],
    ...overrides,
  };
}

const smart = {
  start: '2026-10-19T13:00:00Z',
  end: '2026-10-19T14:30:00Z',
  type: 'SMART',
  energy_added_kwh: 6,
  smart: true,
};

const boost = {
  start: '2026-10-19T17:00:00Z',
  end: '2026-10-19T18:00:00Z',
  type: 'BOOST',
  energy_added_kwh: null,
  smart: false,
};

describe('SmartChargingPanel', () => {
  beforeEach(() => {
    apiGet.mockReset();
    apiPost.mockReset();
  });
  afterEach(cleanup);

  it('keeps the hold explanation behind an ⓘ, with a one-line hint (issue #423)', async () => {
    apiGet.mockResolvedValue(reply());
    render(<SmartChargingPanel />);
    const help = await screen.findByRole('button', { name: 'More about holding the battery during smart charges' });
    expect(screen.getByText(/Boosts are left alone/)).toBeTruthy();
    expect(screen.queryByRole('tooltip')).toBeNull();
    fireEvent.click(help);
    expect(screen.getByRole('tooltip').textContent).toMatch(/Boost charges are billed at the peak rate/);
  });

  it('lists planned charges in UK time and says which hold the battery', async () => {
    apiGet.mockResolvedValue(reply({ dispatches: [smart, boost] }));
    render(<SmartChargingPanel />);
    expect(await screen.findByText(/Mon 19 Oct, 14:00–15:30/)).toBeTruthy();
    expect(screen.getByText('Smart Charge · 6.0 kWh · Battery Held')).toBeTruthy();
    expect(screen.getByText(/Mon 19 Oct, 18:00–19:00/)).toBeTruthy();
    expect(screen.getByText('Boost · Peak Rate · Battery Not Held')).toBeTruthy();
    expect(apiGet).toHaveBeenCalledWith('/api/octopus/dispatches');
  });

  it('turns the hold on and reloads', async () => {
    apiGet.mockResolvedValue(reply({ enabled: false }));
    apiPost.mockResolvedValue({ ok: true, enabled: true });
    render(<SmartChargingPanel />);
    const toggle = await screen.findByRole('switch', { name: /Hold the Battery/ });
    expect(toggle.getAttribute('aria-checked')).toBe('false');
    fireEvent.click(toggle);
    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/octopus/dispatches/hold', { enabled: true }),
    );
    await waitFor(() => expect(apiGet.mock.calls.length).toBeGreaterThanOrEqual(2));
  });

  it('shows only the switch and what it does while off', async () => {
    apiGet.mockResolvedValue(reply({ enabled: false, dispatches: [smart] }));
    render(<SmartChargingPanel />);
    await screen.findByRole('switch', { name: /Hold the Battery/ });
    expect(screen.getByText(/23:30–05:30/)).toBeTruthy();
    expect(screen.queryByText(/Mon 19 Oct/)).toBeNull();
    expect(screen.queryByText(/Last checked/)).toBeNull();
  });

  it('says when the battery is being held now', async () => {
    apiGet.mockResolvedValue(reply({ holding: true, dispatches: [smart] }));
    render(<SmartChargingPanel />);
    expect(await screen.findByText(/Holding the battery now/)).toBeTruthy();
  });

  it('asks for credentials when the account is not set up', async () => {
    apiGet.mockResolvedValue(reply({ configured: false, checked_at: null }));
    render(<SmartChargingPanel />);
    expect(await screen.findByText(/needs your Octopus account number and API key/)).toBeTruthy();
  });

  it('reports a failed check and that the battery behaves as normal once the list is old', async () => {
    apiGet.mockResolvedValue(
      reply({ error: 'Octopus returned an error: Unable to fetch (KT-CT-4340)', dispatches: [smart] }),
    );
    render(<SmartChargingPanel />);
    expect(await screen.findByText(/Couldn.t check planned charges: .*KT-CT-4340/)).toBeTruthy();
    expect(screen.getByText(/behaves as normal once the last list is more than 15 minutes old/)).toBeTruthy();
    expect(screen.getByText(/Mon 19 Oct, 14:00–15:30/)).toBeTruthy();
  });

  it('says when the account has no smart-charging car or charger', async () => {
    apiGet.mockResolvedValue(reply({ devices: 0 }));
    render(<SmartChargingPanel />);
    expect(await screen.findByText(/No smart-charging car or charger/)).toBeTruthy();
    expect(screen.queryByText(/No smart charges planned/)).toBeNull();
  });

  it('says when no charges are planned', async () => {
    apiGet.mockResolvedValue(reply());
    render(<SmartChargingPanel />);
    expect(await screen.findByText(/No smart charges planned/)).toBeTruthy();
  });

  it('shows a save failure and keeps the switch as it was', async () => {
    apiGet.mockResolvedValue(reply({ enabled: false }));
    apiPost.mockRejectedValue(new Error('Could not save the setting: disk full'));
    render(<SmartChargingPanel />);
    fireEvent.click(await screen.findByRole('switch', { name: /Hold the Battery/ }));
    expect(await screen.findByText(/disk full/)).toBeTruthy();
    expect(
      screen.getByRole('switch', { name: /Hold the Battery/ }).getAttribute('aria-checked'),
    ).toBe('false');
  });

  it('renders nothing when the backend answer is unusable', async () => {
    apiGet.mockResolvedValue({ ok: false });
    const { container } = render(<SmartChargingPanel />);
    await waitFor(() => expect(apiGet).toHaveBeenCalled());
    expect(container.innerHTML).toBe('');
  });

  // ---- A failed refresh must not hide the panel (review of v0.94.0, finding 6) ----

  describe('when a refresh fails', () => {
    // Advance a refresh interval and let React apply what the refresh did.
    const refresh = () =>
      act(async () => {
        await vi.advanceTimersByTimeAsync(60_000);
      });

    beforeEach(() => {
      vi.useFakeTimers({ shouldAdvanceTime: true });
    });
    afterEach(() => {
      vi.useRealTimers();
    });

    it('keeps the last good data and the switch', async () => {
      apiGet.mockResolvedValue(reply({ holding: true, dispatches: [smart] }));
      render(<SmartChargingPanel />);
      expect(await screen.findByText(/Mon 19 Oct, 14:00–15:30/)).toBeTruthy();

      apiGet.mockRejectedValue(new Error('offline'));
      await refresh();
      expect(apiGet).toHaveBeenCalledTimes(2);

      expect(screen.getByTestId('smart-charging-panel')).toBeTruthy();
      expect(screen.getByRole('switch', { name: /Hold the Battery/i })).toBeTruthy();
      expect(screen.getByText(/Mon 19 Oct, 14:00–15:30/)).toBeTruthy();
    });

    it('recovers when a later refresh works', async () => {
      apiGet.mockResolvedValueOnce(reply({ dispatches: [smart] }));
      render(<SmartChargingPanel />);
      await screen.findByText(/Mon 19 Oct, 14:00–15:30/);

      apiGet.mockRejectedValueOnce(new Error('offline'));
      await refresh();
      apiGet.mockResolvedValue(reply({ dispatches: [boost] }));
      await refresh();

      expect(await screen.findByText(/Mon 19 Oct, 18:00–19:00/)).toBeTruthy();
      expect(screen.queryByText(/Mon 19 Oct, 14:00–15:30/)).toBeNull();
    });

    it('shows the new switch position even if the reload after a save fails', async () => {
      apiGet.mockResolvedValueOnce(reply({ enabled: true }));
      apiPost.mockResolvedValue({ ok: true });
      render(<SmartChargingPanel />);
      const toggle = await screen.findByRole('switch', { name: /Hold the Battery/i });
      expect(toggle.getAttribute('aria-checked')).toBe('true');

      apiGet.mockRejectedValue(new Error('offline'));
      fireEvent.click(toggle);
      await waitFor(() => expect(apiPost).toHaveBeenCalled());
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });

      expect(
        screen.getByRole('switch', { name: /Hold the Battery/i }).getAttribute('aria-checked'),
      ).toBe('false');
    });
  });

  it('renders nothing when the very first load fails', async () => {
    apiGet.mockRejectedValue(new Error('offline'));
    const { container } = render(<SmartChargingPanel />);
    await waitFor(() => expect(apiGet).toHaveBeenCalled());
    expect(container.querySelector('[data-testid="smart-charging-panel"]')).toBeNull();
  });
});
