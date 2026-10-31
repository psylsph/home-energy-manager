import { useCallback, useEffect, useRef, useState } from 'react';
import { apiGet, apiPost } from '../lib/api';
import {
  formatBackupTime,
  formatBytes,
  formatNextDue,
  parseBackupForm,
} from '../lib/backup';
import type { BackupForm, BackupStatusResponse, HistoryBackupConfig } from '../lib/backup';
import Toggle from './Toggle';
import HelpTip, { HeadingWithHelp } from './HelpTip';

type Status = BackupStatusResponse['data'];

const RUNNING_POLL_MS = 3000;

function formFrom(status: Status): BackupForm {
  return {
    destination: status.config.destination,
    keep: String(status.config.keep),
    intervalHours: String(status.config.interval_hours),
  };
}

/**
 * Settings → History Backup (issue #379): scheduled, verified copies of
 * history.db, a "Back up now" button and the last result.
 */
export default function HistoryBackupPanel({ id }: { id?: string } = {}) {
  const [status, setStatus] = useState<Status | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  const [form, setForm] = useState<BackupForm | null>(null);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<{ kind: 'ok' | 'error'; text: string } | null>(null);
  const [backingUp, setBackingUp] = useState(false);
  /** Unix seconds when `status` arrived, for "due soon". */
  const [statusAt, setStatusAt] = useState(0);

  /**
   * Fields a toggle save is still sending. A status fetched meanwhile (the
   * running-backup poll) predates the save, so these override its config.
   */
  const pendingFields = useRef<Partial<HistoryBackupConfig> | null>(null);

  /** Take a status response; `resetForm` also replaces the text fields with the saved values. */
  const apply = useCallback((data: Status, resetForm: boolean) => {
    const pending = pendingFields.current;
    setStatus(pending ? { ...data, config: { ...data.config, ...pending } } : data);
    setStatusAt(Date.now() / 1000);
    setLoadFailed(false);
    if (resetForm) setForm(formFrom(data));
  }, []);

  const refresh = useCallback(async (resetForm: boolean) => {
    try {
      apply((await apiGet<BackupStatusResponse>('/api/backup')).data, resetForm);
    } catch {
      setLoadFailed(true);
    }
  }, [apply]);

  useEffect(() => {
    let cancelled = false;
    apiGet<BackupStatusResponse>('/api/backup')
      .then((res) => {
        if (!cancelled) apply(res.data, true);
      })
      .catch(() => {
        if (!cancelled) setLoadFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [apply]);

  // Follow a backup that is running (scheduled, or started elsewhere) until
  // it finishes, so the button and last result don't go stale.
  const running = status?.running === true;
  useEffect(() => {
    if (!running) return;
    const timer = setInterval(() => void refresh(false), RUNNING_POLL_MS);
    return () => clearInterval(timer);
  }, [running, refresh]);

  /**
   * Save `fields`. The Save button resets the text fields to what was saved;
   * a toggle leaves them alone so unsaved edits survive.
   */
  const saveFields = async (fields: Partial<HistoryBackupConfig>, okText: string, resetForm: boolean) => {
    if (saving) return;
    setSaving(true);
    setMessage(null);
    if (!resetForm) {
      // Show a toggle's new state straight away; a failure reloads it.
      pendingFields.current = fields;
      setStatus((current) => (current ? { ...current, config: { ...current.config, ...fields } } : current));
    }
    let saved = false;
    try {
      await apiPost('/api/backup', fields);
      saved = true;
      setMessage({ kind: 'ok', text: okText });
    } catch (e) {
      setMessage({ kind: 'error', text: e instanceof Error ? e.message : 'Could not save backup settings' });
    } finally {
      pendingFields.current = null;
      // A rejected Save keeps what was typed, so it can be corrected. The
      // controls stay disabled until the fresh status is in.
      await refresh(resetForm && saved);
      setSaving(false);
    }
  };

  const handleSave = () => {
    if (!form) return;
    const parsed = parseBackupForm(form);
    if (!parsed.ok) {
      setMessage({ kind: 'error', text: parsed.error });
      return;
    }
    void saveFields(parsed.value, 'Backup settings saved', true);
  };

  /**
   * Start a backup. The server replies as soon as it has started, so a slow
   * backup can't outlast a proxy timeout; the panel then follows `running`
   * until the last-result line shows how it went.
   */
  const handleBackUpNow = async () => {
    setBackingUp(true);
    setMessage(null);
    try {
      await apiPost('/api/backup/run');
    } catch (e) {
      setMessage({ kind: 'error', text: e instanceof Error ? e.message : 'Could not start the backup' });
    } finally {
      await refresh(false);
      setBackingUp(false);
    }
  };

  const last = status?.last ?? null;
  const busy = backingUp || status?.running === true;
  const inputClass =
    'bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-accent outline-none transition-colors';

  return (
    <section id={id} className="bg-bg-surface rounded-xl p-5 flex flex-col gap-4 scroll-mt-32 md:scroll-mt-24" aria-labelledby="history-backup-heading">
      <div>
        <HeadingWithHelp
          as="h2"
          id="history-backup-heading"
          className="text-text-primary text-lg font-semibold font-sans"
          title="History Backup"
          help={
            <>
              HEM saves a checked copy of its history database on a schedule. Copies are safe to take while HEM is
              running, unlike copying <span className="font-mono">history.db</span> yourself.
            </>
          }
        />
        <p className="text-text-secondary text-sm mt-1">Scheduled, checked copies of your history.</p>
      </div>

      {loadFailed && !status && (
        <p className="text-red-400 text-sm font-sans">Could not load backup settings.</p>
      )}

      {status && form && (
        <>
          <div className="flex items-center justify-between gap-3">
            <span className="text-text-primary text-sm font-sans font-medium">Automatic Backups</span>
            <Toggle
              checked={status.config.enabled}
              onChange={(enabled) =>
                void saveFields({ enabled }, enabled ? 'Automatic backups on' : 'Automatic backups off', false)
              }
              ariaLabel="Automatic Backups"
              disabled={saving}
            />
          </div>

          <div className="flex flex-col gap-1">
            <label htmlFor="history-backup-folder" className="text-text-primary text-sm font-sans">
              Backup Folder
            </label>
            <input
              id="history-backup-folder"
              type="text"
              className={inputClass}
              value={form.destination}
              placeholder={status.default_destination}
              aria-describedby="history-backup-folder-hint"
              onChange={(e) => setForm({ ...form, destination: e.target.value })}
            />
            <span className="text-text-secondary text-xs font-sans">
              <span id="history-backup-folder-hint">
                Blank for the default. It must already exist. Use one folder per HEM install: older
                backups in it are deleted.
              </span>{' '}
              <HelpTip label="Backup Folder">
                Leave blank for the default. A folder you choose, such as a NAS share, must already exist; if it is
                missing the backup fails and you are told, rather than filling the local disk. Use a separate folder for
                each HEM install: older backups in the folder are deleted to keep the count.
              </HelpTip>
            </span>
          </div>

          <div className="flex flex-wrap gap-4">
            <label className="flex flex-col gap-1">
              <span className="text-text-primary text-sm font-sans">Every (Hours)</span>
              <input
                type="number"
                min={1}
                className={`${inputClass} w-24`}
                value={form.intervalHours}
                onChange={(e) => setForm({ ...form, intervalHours: e.target.value })}
              />
            </label>
            <label className="flex flex-col gap-1">
              <span className="text-text-primary text-sm font-sans">Backups to Keep</span>
              <input
                type="number"
                min={1}
                className={`${inputClass} w-24`}
                value={form.keep}
                onChange={(e) => setForm({ ...form, keep: e.target.value })}
              />
            </label>
          </div>

          <div className="flex items-start justify-between gap-3">
            <div className="flex flex-col gap-0.5">
              <span className="text-text-primary text-sm font-sans font-medium">
                Include settings.json{' '}
                <HelpTip label="Include settings.json">
                  Useful for a full restore, but settings.json holds your Octopus API key and alert tokens in plain
                  text. Only turn this on if the backup folder is private.
                </HelpTip>
              </span>
              <span className="text-text-secondary text-xs font-sans">
                Holds your keys in plain text. Only for a private folder.
              </span>
            </div>
            <div className="shrink-0 mt-1">
              <Toggle
                checked={status.config.include_settings}
                onChange={(include_settings) =>
                  void saveFields(
                    { include_settings },
                    include_settings
                      ? 'settings.json will be included'
                      : 'settings.json will not be included. Copies already in the backup folder stay there: delete them if the folder is not private.',
                    false,
                  )
                }
                ariaLabel="Include settings.json"
                disabled={saving}
              />
            </div>
          </div>

          <div className="text-sm font-sans" aria-live="polite">
            {last == null ? (
              <p className="text-text-secondary">No backup yet.</p>
            ) : last.ok ? (
              <p className="text-text-secondary">
                Last backup: <span className="text-text-primary">{formatBackupTime(last.at)}</span>
                {' · '}
                {formatBytes(last.size_bytes)}
                {last.path && (
                  <span className="block font-mono text-xs break-all">{last.path}</span>
                )}
              </p>
            ) : (
              <p className="text-red-400">
                Last backup failed {formatBackupTime(last.at)}: {last.error}
                {status.last_success_at != null && (
                  <span className="block text-text-secondary">
                    Last good backup: {formatBackupTime(status.last_success_at)}
                  </span>
                )}
              </p>
            )}
            {status.config.enabled && status.next_due_at != null && (
              <p className="text-text-secondary text-xs mt-1">Next backup due {formatNextDue(status.next_due_at, statusAt)}</p>
            )}
          </div>

          <div className="flex flex-wrap items-center gap-3">
            <button
              type="button"
              onClick={handleSave}
              disabled={saving}
              className="bg-accent/20 text-accent text-sm font-sans font-semibold px-4 py-1.5 rounded-md hover:bg-accent/30 transition-colors disabled:opacity-50"
            >
              Save
            </button>
            <button
              type="button"
              onClick={() => void handleBackUpNow()}
              disabled={busy}
              className="bg-bg-elevated text-text-primary text-sm font-sans font-semibold px-4 py-1.5 rounded-md hover:bg-bg-elevated/70 transition-colors disabled:opacity-50"
            >
              {busy ? 'Backing up…' : 'Back up now'}
            </button>
            {message && (
              <span
                role={message.kind === 'error' ? 'alert' : 'status'}
                className={`text-sm font-sans ${message.kind === 'error' ? 'text-red-400' : 'text-green-400'}`}
              >
                {message.text}
              </span>
            )}
          </div>
        </>
      )}
    </section>
  );
}
