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
    const warning = await screen.findByText(/Alerts are off or no alert channel is set up, so you will not be told/);
    expect(warning.textContent).toMatch(/Notifications/);
    expect(warning.className).toMatch(/yellow/);
    // alerts_ready is also false with a channel set up but alerts switched off.
    expect(warning.textContent).toMatch(/Turn alerts on/);
  });

  it('explains that the mode buttons act on joined sessions and that Off also stops joining', async () => {
    // #377: the buttons and the join tick read as one setting.
    apiGet.mockResolvedValue(reply());
    render(<SavingSessionsPanel />);
    const modes = await screen.findByRole('group', { name: /^For Sessions You Have Joined/ });
    // The group is named by the visible caption, not a second copy of it.
    expect(modes.hasAttribute('aria-label')).toBe(false);
    // Off sits under that caption but also stops joining; the caption says so.
    expect(modes.getAttribute('aria-labelledby')).toBeTruthy();
    expect(document.getElementById(modes.getAttribute('aria-labelledby')!)!.textContent).toMatch(/Off also stops joining/);
    expect(screen.getByText(/Joining is separate/)).toBeTruthy();
    // Messages follow joining, not Octopus's announcement.
    expect(screen.getByText(/when you have joined a Saving Session/)).toBeTruthy();
    expect(screen.queryByText(/when it is announced/)).toBeNull();
    expect(screen.getByText(/Off turns all of this off, joining included/)).toBeTruthy();
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

  describe('Alert And Export', () => {
    const exporting = (overrides: Partial<SavingSessionsResponse> = {}) =>
      reply({ mode: 'export', ...overrides });

    it('offers three modes and marks the current one', async () => {
      apiGet.mockResolvedValue(exporting());
      render(<SavingSessionsPanel />);
      expect((await screen.findByRole('button', { name: 'Alert And Export' })).getAttribute('aria-pressed')).toBe('true');
      expect(screen.getByRole('button', { name: 'Alert Only' }).getAttribute('aria-pressed')).toBe('false');
      expect(screen.getByRole('button', { name: 'Off' }).getAttribute('aria-pressed')).toBe('false');
    });

    it('switches to it, and says plainly what it will do', async () => {
      apiGet.mockResolvedValue(reply());
      apiPost.mockResolvedValue({ ok: true });
      render(<SavingSessionsPanel />);
      expect(screen.queryByText(/will export from your battery/)).toBeNull();
      fireEvent.click(await screen.findByRole('button', { name: 'Alert And Export' }));
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
        // No mode: re-sending one loaded up to a minute ago could undo a
        // change made elsewhere.
        expect(apiPost).toHaveBeenCalledWith('/api/octopus/sessions/mode', {
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

    it('says so when export settings are saved while the mode has changed elsewhere', async () => {
      // The save sends no mode, so from a stale page it keeps whatever mode
      // is set now; without a word, the export box would just vanish.
      apiGet.mockResolvedValue(exporting());
      apiPost.mockResolvedValue({ ok: true, mode: 'alert' });
      render(<SavingSessionsPanel />);
      fireEvent.change(await screen.findByLabelText('Slot To Use'), { target: { value: '3' } });
      fireEvent.click(screen.getByRole('button', { name: 'Save Export Settings' }));
      const notice = await screen.findByText(/Export settings saved, but Saving Sessions is set to Alert Only/);
      expect(notice.getAttribute('role')).toBe('status');
      expect(notice.textContent).toMatch(/Choose Alert And Export/);
    });

    it('says nothing extra when export settings are saved in Alert And Export', async () => {
      apiGet.mockResolvedValue(exporting());
      apiPost.mockResolvedValue({ ok: true, mode: 'export' });
      render(<SavingSessionsPanel />);
      fireEvent.change(await screen.findByLabelText('Slot To Use'), { target: { value: '3' } });
      fireEvent.click(screen.getByRole('button', { name: 'Save Export Settings' }));
      await waitFor(() => expect(apiPost).toHaveBeenCalled());
      await waitFor(() => expect(apiGet.mock.calls.length).toBeGreaterThanOrEqual(2));
      expect(screen.queryByText(/Export settings saved, but/)).toBeNull();
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
        name: /Join Sessions For Me/,
      })) as HTMLInputElement;
      // Not "new": sessions already on offer are joined too.
      expect(screen.queryByText(/new sessions/i)).toBeNull();
      expect(box.checked).toBe(false);
      expect(screen.getByText(/HEM will not sign it up/)).toBeTruthy();
      expect(screen.getByText(/shows it here, messages you if an alert channel is set up/)).toBeTruthy();
      // auto_join() takes everything on offer it has not tried, not only
      // offers that arrive after the tick.
      expect(screen.getByText(/including any already on offer when you tick this/)).toBeTruthy();
    });

    it('turns it on without changing the mode, then reloads', async () => {
      apiGet.mockResolvedValue(reply({ mode: 'export' }));
      apiPost.mockResolvedValue({ ok: true });
      render(<SavingSessionsPanel />);
      fireEvent.click(await screen.findByRole('checkbox', { name: /Join Sessions For Me/ }));
      await waitFor(() =>
        // The tick alone: a stale mode sent with it could switch the
        // feature and stop a running export.
        expect(apiPost).toHaveBeenCalledWith('/api/octopus/sessions/mode', { auto_join: true }),
      );
      await waitFor(() => expect(apiGet.mock.calls.length).toBeGreaterThanOrEqual(2));
    });

    it('turns it off again', async () => {
      apiGet.mockResolvedValue(reply({ auto_join: true }));
      apiPost.mockResolvedValue({ ok: true });
      render(<SavingSessionsPanel />);
      const box = (await screen.findByRole('checkbox', { name: /Join Sessions For Me/ })) as HTMLInputElement;
      expect(box.checked).toBe(true);
      fireEvent.click(box);
      await waitFor(() =>
        expect(apiPost).toHaveBeenCalledWith('/api/octopus/sessions/mode', { auto_join: false }),
      );
    });

    it('is offered while Off, so it can be unticked before a mode is chosen', async () => {
      // Choosing a mode wakes a check straight away; if the tick were hidden
      // while Off, sessions would be joined before it could be unticked.
      apiGet.mockResolvedValue(reply({ mode: 'off', auto_join: true }));
      apiPost.mockResolvedValue({ ok: true });
      render(<SavingSessionsPanel />);
      const box = (await screen.findByRole('checkbox', { name: /Join Sessions For Me/ })) as HTMLInputElement;
      expect(box.checked).toBe(true);
      fireEvent.click(box);
      await waitFor(() =>
        expect(apiPost).toHaveBeenCalledWith('/api/octopus/sessions/mode', { auto_join: false }),
      );
    });

    it('is not offered when Octopus is not set up', async () => {
      for (const mode of ['off', 'alert'] as const) {
        apiGet.mockResolvedValue(reply({ mode, configured: false, auto_join: true }));
        const { unmount } = render(<SavingSessionsPanel />);
        await screen.findByRole('button', { name: 'Off' });
        expect(screen.queryByRole('checkbox')).toBeNull();
        // Nothing to promise about resuming without credentials.
        expect(screen.queryByText(/paused/)).toBeNull();
        unmount();
      }
    });

    it('says nothing will happen until the account is signed up to Octoplus, once', async () => {
      apiGet.mockResolvedValue(reply({ auto_join: true, enrolled: false }));
      render(<SavingSessionsPanel />);
      expect(await screen.findByText(/Nothing will be joined until it has/)).toBeTruthy();
      expect(screen.getAllByText(/Octoplus Saving Sessions yet|Nothing will be joined/)).toHaveLength(1);
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

    it('sits in its own Joining section, ahead of the export settings', async () => {
      // Below the export box it read as part of export mode (#377).
      apiGet.mockResolvedValue(reply({ mode: 'export' }));
      render(<SavingSessionsPanel />);
      const section = await screen.findByRole('group', { name: 'Joining' });
      const box = screen.getByRole('checkbox', { name: /Join Sessions For Me/ });
      expect(section.contains(box)).toBe(true);
      const exportText = screen.getByText(/will export from your battery/);
      expect(section.compareDocumentPosition(exportText) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    });

    it('works the same whichever of the two active modes is chosen', async () => {
      for (const mode of ['alert', 'export'] as const) {
        apiGet.mockResolvedValue(reply({ mode }));
        apiPost.mockReset();
        apiPost.mockResolvedValue({ ok: true });
        const { unmount } = render(<SavingSessionsPanel />);
        fireEvent.click(await screen.findByRole('checkbox', { name: /Join Sessions For Me/ }));
        expect(screen.getByText(/whichever of Alert Only or Alert And Export is chosen/)).toBeTruthy();
        await waitFor(() =>
          expect(apiPost).toHaveBeenCalledWith('/api/octopus/sessions/mode', { auto_join: true }),
        );
        unmount();
      }
    });

    it('says joining is paused while Off, when it is saved on', async () => {
      // Off stops every request to Octopus, joins included, but the saved
      // choice survives. Without this note it looked like joining was armed.
      apiGet.mockResolvedValue(reply({ mode: 'off', auto_join: true }));
      render(<SavingSessionsPanel />);
      const note = await screen.findByText(/Joining is paused while Saving Sessions is Off/);
      expect(note.getAttribute('role')).toBe('status');
      expect(note.textContent).toMatch(/Alert Only or Alert And Export/);
      // Choosing a mode checks at once, joining whatever is on offer now.
      expect(note.textContent).toMatch(/straight away joins any session on offer that it has not tried before/);
      // Joining needs an account signed up to Octoplus, which is not known while Off.
      expect(note.textContent).toMatch(/if your account is signed up to Octoplus Saving Sessions/);
      expect(screen.getByRole('group', { name: 'Joining' }).contains(note)).toBe(true);
    });

    it('says nothing about pausing when joining was never turned on', async () => {
      apiGet.mockResolvedValue(reply({ mode: 'off', auto_join: false }));
      render(<SavingSessionsPanel />);
      await screen.findByRole('button', { name: 'Off' });
      expect(screen.queryByText(/paused/)).toBeNull();
    });

    it('treats a missing alert channel as a choice, not a fault, when joining is on', async () => {
      // Join-only is "Alert Only + tick" with no channel; a yellow warning
      // there reads as something being broken.
      apiGet.mockResolvedValue(reply({ alerts_ready: false, auto_join: true }));
      render(<SavingSessionsPanel />);
      const note = await screen.findByText(/HEM will still join sessions for you/);
      expect(note.textContent).toMatch(/failed joins still show here/);
      expect(note.textContent).toMatch(/Alerts are off or no alert channel is set up/);
      expect(note.className).not.toMatch(/yellow/);
      expect(screen.queryByText(/Alerts are off or no alert channel is set up, so you will not be told/)).toBeNull();
    });

    it('does not promise joining for an account that is not signed up to Octoplus', async () => {
      // auto_join() does nothing until the account is enrolled.
      apiGet.mockResolvedValue(reply({ alerts_ready: false, auto_join: true, enrolled: false }));
      render(<SavingSessionsPanel />);
      await screen.findByText(/Nothing will be joined until it has/);
      expect(screen.queryByText(/HEM will still join sessions for you/)).toBeNull();
      expect(screen.getByText(/so you will not be told/).className).toMatch(/yellow/);
    });

    it('does not promise joining before enrolment is known', async () => {
      // Before the first check, or after a failed login, enrolled is null and
      // auto_join() joins nothing.
      apiGet.mockResolvedValue(reply({
        alerts_ready: false,
        auto_join: true,
        enrolled: null,
        error: 'Octopus answered HTTP 401',
      }));
      render(<SavingSessionsPanel />);
      const warning = await screen.findByText(/so you will not be told/);
      expect(warning.className).toMatch(/yellow/);
      expect(screen.queryByText(/HEM will still join sessions for you/)).toBeNull();
    });

    it('does not promise joining when the latest check failed after a good one', async () => {
      // A failed check keeps the last enrolment answer, but with no login
      // nothing can be joined.
      apiGet.mockResolvedValue(reply({
        alerts_ready: false,
        auto_join: true,
        enrolled: true,
        error: 'Octopus answered HTTP 401',
      }));
      render(<SavingSessionsPanel />);
      const warning = await screen.findByText(/so you will not be told/);
      expect(warning.className).toMatch(/yellow/);
      expect(screen.queryByText(/HEM will still join sessions for you/)).toBeNull();
    });

    it('still warns about a missing alert channel in Alert And Export, joining or not', async () => {
      // Export start, finish and failure messages would go nowhere too.
      apiGet.mockResolvedValue(reply({ mode: 'export', alerts_ready: false, auto_join: true }));
      render(<SavingSessionsPanel />);
      const warning = await screen.findByText(/Alerts are off or no alert channel is set up, so you will not be told/);
      expect(warning.className).toMatch(/yellow/);
      expect(screen.queryByText(/HEM will still join sessions for you/)).toBeNull();
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
      await screen.findByRole('checkbox', { name: /Join Sessions For Me/ });
      expect(screen.queryByText(/Could not join/)).toBeNull();
    });
  });
});
