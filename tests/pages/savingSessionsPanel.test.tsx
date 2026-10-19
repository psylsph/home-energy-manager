import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';
import type { SavingSessionsResponse } from '../../src/lib/types';

const apiGet = vi.fn();
const apiPost = vi.fn();
vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGet(...args),
  apiPost: (...args: unknown[]) => apiPost(...args),
}));

import SavingSessionsPanel from '../../src/components/SavingSessionsPanel';

function reply(overrides: Partial<SavingSessionsResponse> = {}): SavingSessionsResponse {
  return {
    ok: true,
    mode: 'alert',
    configured: true,
    alerts_ready: true,
    checked_at: '2026-10-17T12:00:00Z',
    error: null,
    enrolled: true,
    auto_join: false,
    join_failures: [],
    sessions: [],
    export_slot: 1,
    export_floor_soc: 20,
    max_discharge_slots: 3,
    ...overrides,
  };
}

const session = {
  id: 9,
  start: '2026-10-19T17:00:00Z',
  end: '2026-10-19T18:00:00Z',
  reward_octopoints_per_kwh: 73,
  announced: true,
  skipped: false,
  export: null as { state: string; detail?: string | null } | null,
};

describe('SavingSessionsPanel', () => {
  beforeEach(() => {
    apiGet.mockReset();
    apiPost.mockReset();
  });
  afterEach(cleanup);

  it('lists joined sessions in UK time with the reward', async () => {
    apiGet.mockResolvedValue(reply({ sessions: [session] }));
    render(<SavingSessionsPanel />);
    expect(await screen.findByText(/Mon 19 Oct, 18:00–19:00/)).toBeTruthy();
    expect(screen.getByText(/73 OctoPoints per kWh/)).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Alert Only' }).getAttribute('aria-pressed')).toBe('true');
    expect(screen.getByRole('button', { name: 'Off' }).getAttribute('aria-pressed')).toBe('false');
  });

  it('says so when nothing is coming up, and that other events are ignored', async () => {
    apiGet.mockResolvedValue(reply());
    render(<SavingSessionsPanel />);
    expect(await screen.findByText(/No joined Saving Sessions coming up/)).toBeTruthy();
    expect(screen.getByText(/Weekend Happy Hours/)).toBeTruthy();
  });

  it('switches the mode and reloads', async () => {
    apiGet.mockResolvedValue(reply({ mode: 'off' }));
    apiPost.mockResolvedValue({ ok: true });
    render(<SavingSessionsPanel />);
    fireEvent.click(await screen.findByRole('button', { name: 'Alert Only' }));
    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/octopus/sessions/mode', { mode: 'alert' }),
    );
    await waitFor(() => expect(apiGet.mock.calls.length).toBeGreaterThanOrEqual(2));
  });

  it('shows nothing but the switch when off', async () => {
    apiGet.mockResolvedValue(reply({ mode: 'off', sessions: [session] }));
    render(<SavingSessionsPanel />);
    await screen.findByRole('button', { name: 'Off' });
    expect(screen.queryByText(/Mon 19 Oct/)).toBeNull();
    expect(screen.queryByText(/No joined Saving Sessions/)).toBeNull();
  });

  it('warns when no alert channel is set up', async () => {
    apiGet.mockResolvedValue(reply({ alerts_ready: false }));
    render(<SavingSessionsPanel />);
    expect((await screen.findByText(/No alert channel is set up/)).textContent).toMatch(/Notifications/);
  });

  it('asks for Octopus credentials when they are missing', async () => {
    apiGet.mockResolvedValue(reply({ configured: false }));
    render(<SavingSessionsPanel />);
    expect(await screen.findByText(/needs your Octopus account number and API key/)).toBeTruthy();
    expect(screen.queryByText(/No joined Saving Sessions/)).toBeNull();
  });

  it('explains a failed check and keeps the last good list', async () => {
    apiGet.mockResolvedValue(reply({ error: 'Octopus answered HTTP 502', sessions: [session] }));
    render(<SavingSessionsPanel />);
    expect(await screen.findByText(/could not be checked just now: Octopus answered HTTP 502/)).toBeTruthy();
    expect(screen.getByText(/from the last successful check/)).toBeTruthy();
    expect(screen.getByText(/Mon 19 Oct/)).toBeTruthy();
    // It must not read like a billing error.
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('does not claim "nothing coming up" when the check failed', async () => {
    apiGet.mockResolvedValue(reply({ error: 'Octopus answered HTTP 502' }));
    render(<SavingSessionsPanel />);
    await screen.findByText(/could not be checked just now/);
    expect(screen.queryByText(/No joined Saving Sessions coming up/)).toBeNull();
  });

  it('mentions an account that has not joined Octoplus', async () => {
    apiGet.mockResolvedValue(reply({ enrolled: false }));
    render(<SavingSessionsPanel />);
    expect(await screen.findByText(/has not joined Octoplus Saving Sessions/)).toBeTruthy();
  });

  it('shows a save failure without an alert role', async () => {
    apiGet.mockResolvedValue(reply({ mode: 'off' }));
    apiPost.mockRejectedValue(new Error('Could not save the setting'));
    render(<SavingSessionsPanel />);
    fireEvent.click(await screen.findByRole('button', { name: 'Alert Only' }));
    expect(await screen.findByText('Could not save the setting')).toBeTruthy();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('renders nothing when the backend answer is unusable or fails', async () => {
    apiGet.mockResolvedValue({ ok: true, data: { something: 'else' } });
    const first = render(<SavingSessionsPanel />);
    await waitFor(() => expect(apiGet).toHaveBeenCalled());
    expect(first.container.querySelector('[data-testid="saving-sessions-panel"]')).toBeNull();
    first.unmount();

    apiGet.mockRejectedValue(new Error('offline'));
    const second = render(<SavingSessionsPanel />);
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(2));
    expect(second.container.querySelector('[data-testid="saving-sessions-panel"]')).toBeNull();
  });

  describe('Activate Export', () => {
    const exporting = (overrides: Partial<SavingSessionsResponse> = {}) =>
      reply({ mode: 'export', ...overrides });

    it('offers three modes and marks the current one', async () => {
      apiGet.mockResolvedValue(exporting());
      render(<SavingSessionsPanel />);
      expect((await screen.findByRole('button', { name: 'Activate Export' })).getAttribute('aria-pressed')).toBe('true');
      expect(screen.getByRole('button', { name: 'Alert Only' }).getAttribute('aria-pressed')).toBe('false');
      expect(screen.getByRole('button', { name: 'Off' }).getAttribute('aria-pressed')).toBe('false');
    });

    it('switches to it, and says plainly what it will do', async () => {
      apiGet.mockResolvedValue(reply());
      apiPost.mockResolvedValue({ ok: true });
      render(<SavingSessionsPanel />);
      expect(screen.queryByText(/will export from your battery/)).toBeNull();
      fireEvent.click(await screen.findByRole('button', { name: 'Activate Export' }));
      await waitFor(() =>
        expect(apiPost).toHaveBeenCalledWith('/api/octopus/sessions/mode', { mode: 'export' }),
      );
    });

    it('explains the export and shows the slot and floor controls only in this mode', async () => {
      apiGet.mockResolvedValue(exporting());
      const first = render(<SavingSessionsPanel />);
      expect(await screen.findByText(/will export from your battery/)).toBeTruthy();
      expect(screen.getByLabelText('Slot To Use')).toBeTruthy();
      expect(screen.getByLabelText('Stop Exporting At')).toBeTruthy();
      first.unmount();

      apiGet.mockResolvedValue(reply());
      render(<SavingSessionsPanel />);
      await screen.findByRole('button', { name: 'Off' });
      expect(screen.queryByLabelText('Slot To Use')).toBeNull();
      expect(screen.queryByText(/will export from your battery/)).toBeNull();
    });

    it('offers exactly the inverter\'s slots, with the saved one selected', async () => {
      apiGet.mockResolvedValue(exporting({ export_slot: 2, max_discharge_slots: 3 }));
      render(<SavingSessionsPanel />);
      const select = (await screen.findByLabelText('Slot To Use')) as HTMLSelectElement;
      expect(Array.from(select.options).map((o) => o.value)).toEqual(['1', '2', '3']);
      expect(select.value).toBe('2');
      expect((screen.getByLabelText('Stop Exporting At') as HTMLInputElement).value).toBe('20');
    });

    it('saves a changed slot and floor together, and not before anything changed', async () => {
      apiGet.mockResolvedValue(exporting());
      apiPost.mockResolvedValue({ ok: true });
      render(<SavingSessionsPanel />);
      const save = await screen.findByRole('button', { name: 'Save Export Settings' });
      expect(save).toBeDisabled();

      fireEvent.change(screen.getByLabelText('Slot To Use'), { target: { value: '3' } });
      fireEvent.change(screen.getByLabelText('Stop Exporting At'), { target: { value: '45' } });
      expect(save).toBeEnabled();
      fireEvent.click(save);
      await waitFor(() =>
        expect(apiPost).toHaveBeenCalledWith('/api/octopus/sessions/mode', {
          mode: 'export',
          export_slot: 3,
          export_floor_soc: 45,
        }),
      );
    });

    it('offers no slot choice and no Save when the inverter has a single slot', async () => {
      // Nothing is configurable: the one slot is used. A Save button that can
      // never enable, and a picker with one option, would only confuse.
      apiGet.mockResolvedValue(exporting({ max_discharge_slots: 1, export_slot: 1 }));
      render(<SavingSessionsPanel />);
      expect(await screen.findByText(/will export from your battery/)).toBeTruthy();
      expect(screen.getByText(/single export slot/)).toBeTruthy();
      expect(screen.queryByLabelText('Slot To Use')).toBeNull();
      expect(screen.queryByLabelText('Stop Exporting At')).toBeNull();
      expect(screen.queryByRole('button', { name: 'Save Export Settings' })).toBeNull();
    });

    it('offers the slot picker, but not the floor, on a two-slot inverter, and Save follows the change', async () => {
      apiGet.mockResolvedValue(exporting({ max_discharge_slots: 2, export_slot: 1 }));
      apiPost.mockResolvedValue({ ok: true });
      render(<SavingSessionsPanel />);
      const select = (await screen.findByLabelText('Slot To Use')) as HTMLSelectElement;
      expect(Array.from(select.options).map((o) => o.value)).toEqual(['1', '2']);
      expect(screen.queryByLabelText('Stop Exporting At')).toBeNull();
      const save = screen.getByRole('button', { name: 'Save Export Settings' });
      expect(save).toBeDisabled();

      fireEvent.change(select, { target: { value: '2' } });
      expect(save).toBeEnabled();
      fireEvent.click(save);
      await waitFor(() =>
        expect(apiPost).toHaveBeenCalledWith('/api/octopus/sessions/mode', {
          mode: 'export',
          export_slot: 2,
          export_floor_soc: 20,
        }),
      );
    });

    it('lets the user correct a saved slot the inverter does not have', async () => {
      // E.g. a slot chosen on a previous inverter. Booking would fail, so the
      // picker must appear, show a slot that exists, and Save must be live.
      apiGet.mockResolvedValue(exporting({ max_discharge_slots: 1, export_slot: 2 }));
      apiPost.mockResolvedValue({ ok: true });
      render(<SavingSessionsPanel />);
      const select = (await screen.findByLabelText('Slot To Use')) as HTMLSelectElement;
      expect(Array.from(select.options).map((o) => o.value)).toEqual(['1']);
      expect(select.value).toBe('1');
      const save = screen.getByRole('button', { name: 'Save Export Settings' });
      expect(save).toBeEnabled();
      fireEvent.click(save);
      await waitFor(() =>
        expect(apiPost).toHaveBeenCalledWith('/api/octopus/sessions/mode', {
          mode: 'export',
          export_slot: 1,
          export_floor_soc: 20,
        }),
      );
    });

    it('shows each session\'s export status', async () => {
      apiGet.mockResolvedValue(exporting({
        sessions: [
          { ...session, id: 1, export: { state: 'booked' } },
          { ...session, id: 2, start: '2026-10-20T17:00:00Z', end: '2026-10-20T18:00:00Z', export: { state: 'error', detail: 'Slot 3 is not on this inverter' } },
          { ...session, id: 3, start: '2026-10-21T17:00:00Z', end: '2026-10-21T18:00:00Z', skipped: true, export: { state: 'skipped' } },
        ],
      }));
      render(<SavingSessionsPanel />);
      expect(await screen.findByText('Export Booked')).toBeTruthy();
      expect(screen.getByText('Not Booked: Slot 3 is not on this inverter')).toBeTruthy();
      expect(screen.getByText('Skipped')).toBeTruthy();
    });

    it('skips and un-skips a session', async () => {
      apiGet.mockResolvedValue(exporting({
        sessions: [
          { ...session, id: 1, export: { state: 'booked' } },
          { ...session, id: 3, start: '2026-10-21T17:00:00Z', end: '2026-10-21T18:00:00Z', skipped: true, export: { state: 'skipped' } },
        ],
      }));
      apiPost.mockResolvedValue({ ok: true });
      render(<SavingSessionsPanel />);
      const skipButtons = await screen.findAllByRole('button', { name: /^Skip Session|^Don.t Skip/ });
      expect(skipButtons).toHaveLength(2);

      fireEvent.click(screen.getByRole('button', { name: /Skip Session.*Mon 19 Oct/ }));
      await waitFor(() =>
        expect(apiPost).toHaveBeenCalledWith('/api/octopus/sessions/skip', { id: 1, skip: true }),
      );
      fireEvent.click(screen.getByRole('button', { name: /Don.t Skip.*Wed 21 Oct/ }));
      await waitFor(() =>
        expect(apiPost).toHaveBeenCalledWith('/api/octopus/sessions/skip', { id: 3, skip: false }),
      );
    });

    it('offers no skip once a session is running or over', async () => {
      apiGet.mockResolvedValue(exporting({
        sessions: [
          { ...session, id: 1, export: { state: 'running' } },
          { ...session, id: 2, start: '2026-10-20T17:00:00Z', end: '2026-10-20T18:00:00Z', export: { state: 'done' } },
        ],
      }));
      render(<SavingSessionsPanel />);
      expect(await screen.findByText('Exporting Now')).toBeTruthy();
      expect(screen.queryByRole('button', { name: /Skip Session/ })).toBeNull();
    });

    it('shows nothing about exports in alert mode', async () => {
      apiGet.mockResolvedValue(reply({ sessions: [{ ...session, export: null }] }));
      render(<SavingSessionsPanel />);
      await screen.findByText(/Mon 19 Oct/);
      expect(screen.queryByRole('button', { name: /Skip Session/ })).toBeNull();
      expect(screen.queryByLabelText('Slot To Use')).toBeNull();
    });

    it('shows a save failure without an alert role', async () => {
      apiGet.mockResolvedValue(exporting());
      apiPost.mockRejectedValue(new Error('Slot must be between 1 and 2 on this inverter'));
      render(<SavingSessionsPanel />);
      fireEvent.change(await screen.findByLabelText('Slot To Use'), { target: { value: '3' } });
      fireEvent.click(screen.getByRole('button', { name: 'Save Export Settings' }));
      expect(await screen.findByText('Slot must be between 1 and 2 on this inverter')).toBeTruthy();
      expect(screen.queryByRole('alert')).toBeNull();
    });
  });

  describe('automatic joining', () => {
    it('offers the switch off by default and says what it does', async () => {
      apiGet.mockResolvedValue(reply());
      render(<SavingSessionsPanel />);
      const box = (await screen.findByRole('checkbox', {
        name: /Join New Saving Sessions Automatically/,
      })) as HTMLInputElement;
      expect(box.checked).toBe(false);
      expect(screen.getByText(/HEM will not sign it up/)).toBeTruthy();
    });

    it('turns it on without changing the mode, then reloads', async () => {
      apiGet.mockResolvedValue(reply({ mode: 'export' }));
      apiPost.mockResolvedValue({ ok: true });
      render(<SavingSessionsPanel />);
      fireEvent.click(await screen.findByRole('checkbox', { name: /Join New Saving Sessions/ }));
      await waitFor(() =>
        expect(apiPost).toHaveBeenCalledWith('/api/octopus/sessions/mode', {
          mode: 'export',
          auto_join: true,
        }),
      );
      await waitFor(() => expect(apiGet.mock.calls.length).toBeGreaterThanOrEqual(2));
    });

    it('turns it off again', async () => {
      apiGet.mockResolvedValue(reply({ auto_join: true }));
      apiPost.mockResolvedValue({ ok: true });
      render(<SavingSessionsPanel />);
      const box = (await screen.findByRole('checkbox', { name: /Join New Saving Sessions/ })) as HTMLInputElement;
      expect(box.checked).toBe(true);
      fireEvent.click(box);
      await waitFor(() =>
        expect(apiPost).toHaveBeenCalledWith('/api/octopus/sessions/mode', {
          mode: 'alert',
          auto_join: false,
        }),
      );
    });

    it('is not offered when the feature is off or Octopus is not set up', async () => {
      apiGet.mockResolvedValue(reply({ mode: 'off' }));
      const { unmount } = render(<SavingSessionsPanel />);
      await screen.findByRole('button', { name: 'Off' });
      expect(screen.queryByRole('checkbox')).toBeNull();
      unmount();

      apiGet.mockResolvedValue(reply({ configured: false }));
      render(<SavingSessionsPanel />);
      await screen.findByText(/needs your Octopus account number/);
      expect(screen.queryByRole('checkbox')).toBeNull();
    });

    it('says nothing will happen until the account is signed up to Octoplus', async () => {
      apiGet.mockResolvedValue(reply({ auto_join: true, enrolled: false }));
      render(<SavingSessionsPanel />);
      expect(await screen.findByText(/Nothing will be joined until the account has joined/)).toBeTruthy();
    });

    it('reports a failed join and that it will not be retried', async () => {
      apiGet.mockResolvedValue(reply({
        auto_join: true,
        join_failures: [{
          id: 9,
          start: '2026-10-19T17:00:00Z',
          end: '2026-10-19T18:00:00Z',
          error: 'Octopus would not join it: Event has ended',
        }],
      }));
      render(<SavingSessionsPanel />);
      const note = await screen.findByText(/Could not join Mon 19 Oct, 18:00–19:00/);
      expect(note.textContent).toMatch(/Event has ended/);
      expect(note.textContent).toMatch(/will not try this one again/);
    });

    it('hides old failures once automatic joining is switched off', async () => {
      apiGet.mockResolvedValue(reply({
        auto_join: false,
        join_failures: [{
          id: 9,
          start: '2026-10-19T17:00:00Z',
          end: '2026-10-19T18:00:00Z',
          error: 'x',
        }],
      }));
      render(<SavingSessionsPanel />);
      await screen.findByRole('checkbox', { name: /Join New Saving Sessions/ });
      expect(screen.queryByText(/Could not join/)).toBeNull();
    });
  });
});
