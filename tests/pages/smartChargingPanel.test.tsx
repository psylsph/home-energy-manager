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
import { useInverterStore } from '../../src/store/useInverterStore';

function reply(overrides: Partial<SmartChargingResponse> = {}): SmartChargingResponse {
  return {
    ok: true,
    enabled: true,
    configured: true,
    checked_at: '2026-10-17T12:00:00Z',
    error: null,
    devices: 1,
    holding: false,
    hold_failing: false,
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

  describe('on a Gateway', () => {
    // Issue #428: a pause written to a Gateway never reaches the AIO.
    afterEach(() => useInverterStore.setState({ snapshot: null }));

    it("doesn't offer the battery hold", async () => {
      useInverterStore.setState({ snapshot: { device_type_code: '7001' } as never });
      apiGet.mockResolvedValue(reply({ enabled: false }));
      const { container } = render(<SmartChargingPanel />);
      await waitFor(() => expect(apiGet).toHaveBeenCalled());
      await act(async () => {});
      expect(screen.queryByRole('switch', { name: /Hold the Battery/ })).toBeNull();
      expect(container.textContent).toBe('');
    });

    it("doesn't offer it while the hold is on either: the hold sits out on a Gateway", async () => {
      useInverterStore.setState({ snapshot: { device_type_code: '7001' } as never });
      apiGet.mockResolvedValue(reply({ enabled: true }));
      const { container } = render(<SmartChargingPanel />);
      await waitFor(() => expect(apiGet).toHaveBeenCalled());
      await act(async () => {});
      expect(container.textContent).toBe('');
    });
  });

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

  it("says in the ⓘ that the Planner uses the list", async () => {
    apiGet.mockResolvedValue(reply({ dispatches: [smart, boost] }));
    render(<SmartChargingPanel />);
    fireEvent.click(
      await screen.findByRole('button', { name: 'More about holding the battery during smart charges' }),
    );
    const tip = screen.getByRole('tooltip').textContent;
    expect(tip).toMatch(/Planner uses the same list/);
    // The car's energy goes in only once it isn't already in the history.
    expect(tip).toMatch(/once your usage history no longer includes the car/);
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

  it('says when a charge slot is charging the battery instead of the hold pausing it', async () => {
    // 9 Nov 23:30: the user's own charge slot was charging the battery when
    // the car's smart charge began, so the hold had nothing to pause.
    apiGet.mockResolvedValue(reply({ hold_charge_slot: true, dispatches: [smart] }));
    render(<SmartChargingPanel />);
    const line = await screen.findByText('Not holding the battery: your charge slot is charging it.');
    // Where "No smart charges planned." goes: after the list, before Last checked.
    const list = screen.getByRole('list', { name: 'Planned car charges' });
    const checked = screen.getByText(/Last checked/);
    expect(list.compareDocumentPosition(line) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(line.compareDocumentPosition(checked) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.queryByText(/Holding the battery now/)).toBeNull();
    expect(screen.queryByText(/Couldn't pause the battery/)).toBeNull();
  });

  it('says nothing about a charge slot while the hold is off', async () => {
    apiGet.mockResolvedValue(reply({ enabled: false, hold_charge_slot: true, dispatches: [smart] }));
    render(<SmartChargingPanel />);
    await screen.findByRole('switch', { name: /Hold the Battery/ });
    expect(screen.queryByText(/your charge slot is charging it/)).toBeNull();
  });

  it("says when the inverter won't take the pause instead of claiming it's held", async () => {
    // Issue #431: the hold kept trying while the battery emptied into the car.
    apiGet.mockResolvedValue(reply({ holding: true, hold_failing: true, dispatches: [smart] }));
    render(<SmartChargingPanel />);
    expect(
      await screen.findByText(/Couldn't pause the battery, so it may be charging the car/),
    ).toBeTruthy();
    // Fourth code review #8: true whether a write or a read is failing.
    expect(screen.getByText(/HEM keeps trying/)).toBeTruthy();
    expect(screen.queryByText(/Retrying every 5 minutes/)).toBeNull();
    expect(screen.queryByText(/Holding the battery now/)).toBeNull();
  });

  it("says the battery isn't paused when an All-in-One hold never got to start (third code review #4)", async () => {
    // No usable reading to save, so the hold never paused: not holding, but failing.
    apiGet.mockResolvedValue(reply({ holding: false, hold_failing: true, dispatches: [smart] }));
    render(<SmartChargingPanel />);
    expect(await screen.findByText(/Couldn't pause the battery/)).toBeTruthy();
    expect(screen.queryByText(/Holding the battery now/)).toBeNull();
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

  describe('topping the battery up during smart charges (issue #434)', () => {
    const charge = (overrides: Partial<SmartChargingResponse> = {}) =>
      reply({
        charge_enabled: false,
        charging: false,
        charge_state: 'idle',
        charge_reason: null,
        charge_note: null,
        charge_target_soc: 100,
        ...overrides,
      });

    it('is offered only while the hold is on', async () => {
      apiGet.mockResolvedValue(charge({ enabled: false }));
      render(<SmartChargingPanel />);
      await screen.findByRole('switch', { name: /Hold the Battery/ });
      expect(screen.queryByRole('switch', { name: 'Top Up the Battery During Smart Charges' })).toBeNull();
    });

    it('is offered with a one-line hint and the detail behind an ⓘ', async () => {
      apiGet.mockResolvedValue(charge());
      render(<SmartChargingPanel />);
      const toggle = await screen.findByRole('switch', {
        name: 'Top Up the Battery During Smart Charges',
      });
      expect(toggle.getAttribute('aria-checked')).toBe('false');
      expect(screen.getByText('Charges the battery from the cheap grid while the car smart-charges.')).toBeTruthy();
      fireEvent.click(
        screen.getByRole('button', { name: 'More about topping the battery up during smart charges' }),
      );
      const tip = screen.getByRole('tooltip').textContent;
      expect(tip).toMatch(/whole home at the off-peak rate/);
      expect(tip).toMatch(/up to 100%/);
      expect(tip).toMatch(/puts your charge settings back/);
    });

    it('turns on through its own endpoint', async () => {
      apiGet.mockResolvedValue(charge());
      apiPost.mockResolvedValue({ ok: true, enabled: true });
      render(<SmartChargingPanel />);
      fireEvent.click(
        await screen.findByRole('switch', { name: 'Top Up the Battery During Smart Charges' }),
      );
      await waitFor(() =>
        expect(apiPost).toHaveBeenCalledWith('/api/octopus/dispatches/charge', { enabled: true }),
      );
      expect(apiPost).not.toHaveBeenCalledWith('/api/octopus/dispatches/hold', expect.anything());
    });

    it('says when it is charging, and to what level', async () => {
      apiGet.mockResolvedValue(
        charge({ charge_enabled: true, holding: true, charging: true, charge_state: 'charging', charge_target_soc: 90 }),
      );
      render(<SmartChargingPanel />);
      expect(await screen.findByText('Charging the battery now, up to 90%.')).toBeTruthy();
    });

    it('says when the inverter is not taking the charge settings', async () => {
      apiGet.mockResolvedValue(charge({ charge_enabled: true, charge_state: 'failing' }));
      render(<SmartChargingPanel />);
      expect(await screen.findByText(/isn.t taking the charge settings/)).toBeTruthy();
    });

    it('says while it is putting the settings back', async () => {
      apiGet.mockResolvedValue(charge({ charge_enabled: true, charge_state: 'restoring' }));
      render(<SmartChargingPanel />);
      expect(await screen.findByText('Putting your charge settings back after the top-up.')).toBeTruthy();
    });

    it('says why it is sitting out', async () => {
      apiGet.mockResolvedValue(
        charge({
          charge_enabled: true,
          holding: true,
          charge_state: 'standing_aside',
          charge_reason: 'manual_force',
          charge_note: 'Force Charge is running',
        }),
      );
      render(<SmartChargingPanel />);
      expect(await screen.findByText('Not topping up the battery. Force Charge is running.')).toBeTruthy();
    });

    it('says nothing about sitting out while switched off', async () => {
      apiGet.mockResolvedValue(
        charge({ charge_state: 'standing_aside', charge_reason: 'full', charge_note: 'The battery is already at 100%' }),
      );
      render(<SmartChargingPanel />);
      await screen.findByRole('switch', { name: 'Top Up the Battery During Smart Charges' });
      expect(screen.queryByText(/Not topping up/)).toBeNull();
    });
  });

  it('renders nothing when the very first load fails', async () => {
    apiGet.mockRejectedValue(new Error('offline'));
    const { container } = render(<SmartChargingPanel />);
    await waitFor(() => expect(apiGet).toHaveBeenCalled());
    expect(container.querySelector('[data-testid="smart-charging-panel"]')).toBeNull();
  });
});
