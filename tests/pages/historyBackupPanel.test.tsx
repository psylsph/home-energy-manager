import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';
import type { BackupStatusResponse } from '../../src/lib/backup';

const apiGet = vi.fn();
const apiPost = vi.fn();
vi.mock('../../src/lib/api', () => ({
  apiGet: (...args: unknown[]) => apiGet(...args),
  apiPost: (...args: unknown[]) => apiPost(...args),
}));

import HistoryBackupPanel from '../../src/components/HistoryBackupPanel';

type Data = BackupStatusResponse['data'];

function status(overrides: Partial<Data> = {}): Data {
  return {
    config: { enabled: true, destination: '', keep: 7, interval_hours: 24, include_settings: false },
    default_destination: '/home/me/.givenergy-local/backups',
    running: false,
    last: null,
    last_success_at: null,
    next_due_at: null,
    ...overrides,
  };
}

function serve(...responses: Data[]) {
  let call = 0;
  apiGet.mockImplementation(async (path: string) => {
    if (path !== '/api/backup') throw new Error(`unexpected GET ${path}`);
    const data = responses[Math.min(call, responses.length - 1)];
    call += 1;
    return { ok: true, data };
  });
}

describe('HistoryBackupPanel', () => {
  beforeEach(() => {
    apiGet.mockReset();
    apiPost.mockReset();
  });
  afterEach(cleanup);

  it('shows the config with the default folder as a placeholder', async () => {
    serve(status());
    render(<HistoryBackupPanel />);
    const folder = (await screen.findByLabelText('Backup folder')) as HTMLInputElement;
    expect(folder.value).toBe('');
    expect(folder.placeholder).toBe('/home/me/.givenergy-local/backups');
    expect((screen.getByLabelText('Every (hours)') as HTMLInputElement).value).toBe('24');
    expect((screen.getByLabelText('Backups to keep') as HTMLInputElement).value).toBe('7');
    expect(screen.getByRole('switch', { name: 'Automatic Backups' }).getAttribute('aria-checked')).toBe('true');
    expect(screen.getByRole('switch', { name: 'Include settings.json' }).getAttribute('aria-checked')).toBe('false');
    expect(screen.getByText('No backup yet.')).toBeTruthy();
  });

  it('shows the last good backup and when the next is due', async () => {
    serve(status({
      last: {
        at: 1_792_584_000, ok: true, trigger: 'scheduled',
        path: '/home/me/.givenergy-local/backups/history-20261021-120000.db',
        size_bytes: 142 * 1024 * 1024, settings_path: null, error: null,
      },
      last_success_at: 1_792_584_000,
      next_due_at: 1_792_670_400,
    }));
    render(<HistoryBackupPanel />);
    expect(await screen.findByText(/142 MB/)).toBeTruthy();
    expect(screen.getByText('/home/me/.givenergy-local/backups/history-20261021-120000.db')).toBeTruthy();
    expect(screen.getByText(/Next backup due/)).toBeTruthy();
  });

  it('shows a failed backup with its error and the last good one', async () => {
    serve(status({
      last: {
        at: 1_792_670_400, ok: false, trigger: 'scheduled', path: null, size_bytes: null,
        settings_path: null, error: 'Backup folder /mnt/nas does not exist (is the drive or share mounted?)',
      },
      last_success_at: 1_792_584_000,
    }));
    render(<HistoryBackupPanel />);
    expect(await screen.findByText(/Last backup failed/)).toBeTruthy();
    expect(screen.getByText(/is the drive or share mounted/)).toBeTruthy();
    expect(screen.getByText(/Last good backup/)).toBeTruthy();
  });

  it('saves parsed fields and reloads', async () => {
    serve(status(), status({ config: { ...status().config, destination: '/mnt/nas', keep: 14 } }));
    apiPost.mockResolvedValue({ ok: true });
    render(<HistoryBackupPanel />);
    fireEvent.change(await screen.findByLabelText('Backup folder'), { target: { value: ' /mnt/nas ' } });
    fireEvent.change(screen.getByLabelText('Backups to keep'), { target: { value: '14' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith('/api/backup', { destination: '/mnt/nas', keep: 14, interval_hours: 24 }),
    );
    expect(await screen.findByRole('status')).toHaveProperty('textContent', 'Backup settings saved');
    await waitFor(() => expect((screen.getByLabelText('Backup folder') as HTMLInputElement).value).toBe('/mnt/nas'));
  });

  it('does not send an invalid form', async () => {
    serve(status());
    render(<HistoryBackupPanel />);
    fireEvent.change(await screen.findByLabelText('Every (hours)'), { target: { value: '0' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect((await screen.findByRole('alert')).textContent).toContain('hours');
    expect(apiPost).not.toHaveBeenCalled();
  });

  it('shows the server rejecting a folder', async () => {
    serve(status());
    apiPost.mockRejectedValue(new Error('Backup folder /mnt/nas does not exist (is the drive or share mounted?)'));
    render(<HistoryBackupPanel />);
    fireEvent.change(await screen.findByLabelText('Backup folder'), { target: { value: '/mnt/nas' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect((await screen.findByRole('alert')).textContent).toContain('does not exist');
    // What was typed survives the rejection, so it can be fixed and saved again.
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(2));
    expect((screen.getByLabelText('Backup folder') as HTMLInputElement).value).toBe('/mnt/nas');
  });

  it('turns automatic backups and settings.json on and off', async () => {
    serve(status());
    apiPost.mockResolvedValue({ ok: true });
    render(<HistoryBackupPanel />);
    fireEvent.click(await screen.findByRole('switch', { name: 'Automatic Backups' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalledWith('/api/backup', { enabled: false }));
    fireEvent.click(screen.getByRole('switch', { name: 'Include settings.json' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalledWith('/api/backup', { include_settings: true }));
  });

  it('backs up now, follows the running backup, then shows the result', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const done = status({
        last: {
          at: 1_792_584_000, ok: true, trigger: 'manual', path: '/b/history-20261021-120000.db',
          size_bytes: 2048, settings_path: null, error: null,
        },
        last_success_at: 1_792_584_000,
      });
      serve(status(), status({ running: true }), done);
      apiPost.mockResolvedValue({ ok: true, data: { running: true } });
      render(<HistoryBackupPanel />);
      fireEvent.click(await screen.findByRole('button', { name: 'Back up now' }));
      await waitFor(() => expect(apiPost).toHaveBeenCalledWith('/api/backup/run'));
      const busy = await screen.findByRole('button', { name: 'Backing up…' });
      expect((busy as HTMLButtonElement).disabled).toBe(true);
      await vi.advanceTimersByTimeAsync(3000);
      expect(await screen.findByText(/2\.0 KB/)).toBeTruthy();
      expect(screen.getByRole('button', { name: 'Back up now' })).toBeTruthy();
    } finally {
      vi.useRealTimers();
    }
  });

  it('shows a backup that failed in the background', async () => {
    serve(status(), status({
      last: {
        at: 1_792_584_000, ok: false, trigger: 'manual', path: null, size_bytes: null,
        settings_path: null, error: 'failed to create backup: disk full',
      },
    }));
    apiPost.mockResolvedValue({ ok: true, data: { running: true } });
    render(<HistoryBackupPanel />);
    fireEvent.click(await screen.findByRole('button', { name: 'Back up now' }));
    expect(await screen.findByText(/disk full/)).toBeTruthy();
  });

  it('shows a failed "Back up now" and still refreshes the status', async () => {
    serve(status());
    apiPost.mockRejectedValue(new Error('A backup is already running'));
    render(<HistoryBackupPanel />);
    fireEvent.click(await screen.findByRole('button', { name: 'Back up now' }));
    expect((await screen.findByRole('alert')).textContent).toBe('A backup is already running');
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(2));
  });

  it('disables "Back up now" while a scheduled backup is running', async () => {
    serve(status({ running: true }));
    render(<HistoryBackupPanel />);
    const button = await screen.findByRole('button', { name: 'Backing up…' });
    expect((button as HTMLButtonElement).disabled).toBe(true);
  });

  it('keeps unsaved edits when a toggle is flipped', async () => {
    serve(status(), status({ config: { ...status().config, include_settings: true } }));
    apiPost.mockResolvedValue({ ok: true });
    render(<HistoryBackupPanel />);
    fireEvent.change(await screen.findByLabelText('Backup folder'), { target: { value: '/mnt/nas' } });
    fireEvent.change(screen.getByLabelText('Backups to keep'), { target: { value: '30' } });
    fireEvent.click(screen.getByRole('switch', { name: 'Include settings.json' }));
    await waitFor(() => expect(apiPost).toHaveBeenCalledWith('/api/backup', { include_settings: true }));
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(2));
    expect((screen.getByLabelText('Backup folder') as HTMLInputElement).value).toBe('/mnt/nas');
    expect((screen.getByLabelText('Backups to keep') as HTMLInputElement).value).toBe('30');
    // The switch shows the new state straight away.
    expect(screen.getByRole('switch', { name: 'Include settings.json' }).getAttribute('aria-checked')).toBe('true');
  });

  it('disables the toggles while a save is in flight', async () => {
    serve(status());
    let finish: (value: unknown) => void = () => {};
    apiPost.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    render(<HistoryBackupPanel />);
    const toggle = await screen.findByRole('switch', { name: 'Automatic Backups' });
    fireEvent.click(toggle);
    // Both switches show they can't be used until the save finishes.
    const include = screen.getByRole('switch', { name: 'Include settings.json' });
    expect(toggle.getAttribute('aria-disabled')).toBe('true');
    expect(include.getAttribute('aria-disabled')).toBe('true');
    fireEvent.click(toggle);
    fireEvent.click(include);
    expect(apiPost).toHaveBeenCalledTimes(1);
    finish({ ok: true });
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(include.getAttribute('aria-disabled')).toBe('false'));
  });

  it('puts a toggle back when its save fails', async () => {
    serve(status());
    apiPost.mockRejectedValue(new Error('disk full'));
    render(<HistoryBackupPanel />);
    const toggle = await screen.findByRole('switch', { name: 'Automatic Backups' });
    fireEvent.click(toggle);
    expect((await screen.findByRole('alert')).textContent).toBe('disk full');
    await waitFor(() => expect(toggle.getAttribute('aria-checked')).toBe('true'));
  });

  it('checks again while a backup is running, until it finishes', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      serve(status({ running: true }), status({ running: false }));
      render(<HistoryBackupPanel />);
      expect(await screen.findByRole('button', { name: 'Backing up…' })).toBeTruthy();
      await vi.advanceTimersByTimeAsync(3000);
      expect(await screen.findByRole('button', { name: 'Back up now' })).toBeTruthy();
      const calls = apiGet.mock.calls.length;
      await vi.advanceTimersByTimeAsync(10_000);
      expect(apiGet.mock.calls.length).toBe(calls);
    } finally {
      vi.useRealTimers();
    }
  });

  it('asks for a separate folder per install', async () => {
    serve(status());
    render(<HistoryBackupPanel />);
    expect(await screen.findByText(/separate folder for each HEM install/)).toBeTruthy();
  });

  it('keeps the toggles disabled until the saved status has loaded', async () => {
    let release: (value: unknown) => void = () => {};
    let call = 0;
    apiGet.mockImplementation(async () => {
      call += 1;
      if (call === 1) return { ok: true, data: status() };
      await new Promise((resolve) => { release = resolve; });
      return { ok: true, data: status({ config: { ...status().config, enabled: false } }) };
    });
    apiPost.mockResolvedValue({ ok: true });
    render(<HistoryBackupPanel />);
    const toggle = await screen.findByRole('switch', { name: 'Automatic Backups' });
    fireEvent.click(toggle);
    await waitFor(() => expect(apiGet).toHaveBeenCalledTimes(2));
    // Saved, but the fresh status isn't back yet.
    expect(toggle.getAttribute('aria-disabled')).toBe('true');
    release(undefined);
    await waitFor(() => expect(toggle.getAttribute('aria-disabled')).toBe('false'));
    expect(toggle.getAttribute('aria-checked')).toBe('false');
  });

  it('says a backup that is already due will run soon, not at a past time', async () => {
    serve(status({ next_due_at: 1_000_000 }));
    render(<HistoryBackupPanel />);
    expect(await screen.findByText('Next backup due soon')).toBeTruthy();
  });

  it('keeps a toggle\'s new state when a status check lands mid-save', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      // A backup is running, so the panel is checking status every few
      // seconds; the check that lands mid-save still has the old config.
      serve(status({ running: true }), status({
        running: true,
        last: {
          at: 1_792_584_000, ok: true, trigger: 'scheduled', path: '/b/history-20261021-120000.db',
          size_bytes: 2048, settings_path: null, error: null,
        },
      }));
      let finish: (value: unknown) => void = () => {};
      apiPost.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
      render(<HistoryBackupPanel />);
      const include = await screen.findByRole('switch', { name: 'Include settings.json' });
      fireEvent.click(include);
      expect(include.getAttribute('aria-checked')).toBe('true');
      // A status check from before the save arrives: the switch must not flip back.
      await vi.advanceTimersByTimeAsync(3000);
      // The poll's data has rendered...
      expect(await screen.findByText(/2\.0 KB/)).toBeTruthy();
      // ...without flipping the switch back.
      expect(include.getAttribute('aria-checked')).toBe('true');
      finish({ ok: true });
    } finally {
      vi.useRealTimers();
    }
  });

  it('says old settings copies stay when settings.json is turned off', async () => {
    serve(status({ config: { ...status().config, include_settings: true } }));
    apiPost.mockResolvedValue({ ok: true });
    render(<HistoryBackupPanel />);
    fireEvent.click(await screen.findByRole('switch', { name: 'Include settings.json' }));
    const note = await screen.findByRole('status');
    expect(note.textContent).toContain('already in the backup folder stay');
  });

  it('says so when the settings cannot be loaded', async () => {
    apiGet.mockRejectedValue(new Error('offline'));
    render(<HistoryBackupPanel />);
    expect(await screen.findByText('Could not load backup settings.')).toBeTruthy();
  });
});
