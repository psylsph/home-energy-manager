/** Helpers for the Settings → History Backup panel (issue #379). */

export interface HistoryBackupConfig {
  enabled: boolean;
  destination: string;
  keep: number;
  interval_hours: number;
  include_settings: boolean;
}

export interface BackupRecord {
  /** Unix seconds. */
  at: number;
  ok: boolean;
  trigger: 'scheduled' | 'manual';
  path: string | null;
  size_bytes: number | null;
  settings_path: string | null;
  error: string | null;
}

export interface BackupStatusResponse {
  ok: boolean;
  data: {
    config: HistoryBackupConfig;
    default_destination: string;
    running: boolean;
    last: BackupRecord | null;
    last_success_at: number | null;
    next_due_at: number | null;
  };
}

export const KEEP_MAX = 365;
export const INTERVAL_HOURS_MAX = 24 * 7;

/** Text fields as typed, before they are parsed for saving. */
export interface BackupForm {
  destination: string;
  keep: string;
  intervalHours: string;
}

export function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null || !Number.isFinite(bytes) || bytes < 0) return '—';
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

/** Date and time of a unix-seconds timestamp in the viewer's locale. */
export function formatBackupTime(epochSecs: number | null | undefined): string {
  if (epochSecs == null || !Number.isFinite(epochSecs)) return '—';
  return new Date(epochSecs * 1000).toLocaleString([], {
    day: 'numeric',
    month: 'short',
    year: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

/**
 * When the next scheduled backup runs. A time already reached reads "soon":
 * the server checks every few minutes, so it will run shortly, not at the
 * past time shown.
 */
export function formatNextDue(nextDueAt: number, nowSecs: number): string {
  return nextDueAt <= nowSecs ? 'soon' : formatBackupTime(nextDueAt);
}

function parseWhole(text: string, max: number): number | null {
  const trimmed = text.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const value = Number(trimmed);
  return value >= 1 && value <= max ? value : null;
}

/** A full path on Linux/macOS (`/…`) or Windows (`C:\…`, `\\server\share`). */
export function isAbsolutePath(path: string): boolean {
  return path.startsWith('/') || /^[A-Za-z]:[\\/]/.test(path) || path.startsWith('\\\\');
}

/**
 * Check the form and return either the fields to save or a message for the
 * first problem. The server re-checks everything, including that the folder
 * exists.
 */
export function parseBackupForm(
  form: BackupForm,
): { ok: true; value: Pick<HistoryBackupConfig, 'destination' | 'keep' | 'interval_hours'> } | { ok: false; error: string } {
  const destination = form.destination.trim();
  if (destination && !isAbsolutePath(destination)) {
    return { ok: false, error: 'Enter the folder as a full path, or leave it blank for the default.' };
  }
  const intervalHours = parseWhole(form.intervalHours, INTERVAL_HOURS_MAX);
  if (intervalHours == null) {
    return { ok: false, error: `Back up every 1 to ${INTERVAL_HOURS_MAX} hours.` };
  }
  const keep = parseWhole(form.keep, KEEP_MAX);
  if (keep == null) {
    return { ok: false, error: `Keep between 1 and ${KEEP_MAX} backups.` };
  }
  return { ok: true, value: { destination, keep, interval_hours: intervalHours } };
}
