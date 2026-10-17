import type { SavingSession, SavingSessionsResponse } from './types';

const UK = 'Europe/London';

function parts(date: Date, options: Intl.DateTimeFormatOptions): Record<string, string> {
  const out: Record<string, string> = {};
  for (const part of new Intl.DateTimeFormat('en-GB', { timeZone: UK, ...options }).formatToParts(date)) {
    out[part.type] = part.value;
  }
  return out;
}

/**
 * `Mon 19 Oct, 18:00–19:00` in UK time, which is how Octopus announces
 * sessions. Built from parts so the result does not depend on the browser's
 * locale punctuation.
 */
export function formatSessionWindow(startIso: string, endIso: string): string {
  const start = new Date(startIso);
  const end = new Date(endIso);
  if (Number.isNaN(start.getTime()) || Number.isNaN(end.getTime())) return 'Time Unknown';
  const day = parts(start, { weekday: 'short', day: 'numeric', month: 'short' });
  const clock = (date: Date) => {
    const p = parts(date, { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
    return `${p.hour}:${p.minute}`;
  };
  return `${day.weekday} ${day.day} ${day.month}, ${clock(start)}–${clock(end)}`;
}

export function formatReward(session: SavingSession): string | null {
  return session.reward_octopoints_per_kwh == null
    ? null
    : `${session.reward_octopoints_per_kwh} OctoPoints per kWh`;
}

/** Accept only a well-formed response; anything else means "unavailable". */
export function readSessionsResponse(value: unknown): SavingSessionsResponse | null {
  if (!value || typeof value !== 'object') return null;
  const v = value as Partial<SavingSessionsResponse>;
  if (
    v.ok !== true
    || (v.mode !== 'off' && v.mode !== 'alert' && v.mode !== 'export')
    || !Array.isArray(v.sessions)
  ) {
    return null;
  }
  return v as SavingSessionsResponse;
}

export interface ExportStatusLabel {
  text: string;
  tone: 'good' | 'bad' | 'neutral';
}

/** One line on how the export for a session stands, or null when there is none. */
export function exportStatusLabel(session: SavingSession): ExportStatusLabel | null {
  if (session.skipped) return { text: 'Skipped', tone: 'neutral' };
  const status = session.export;
  if (!status) return null;
  const withReason = (text: string) => (status.detail ? `${text}: ${status.detail}` : text);
  switch (status.state) {
    case 'booked':
      return { text: 'Export Booked', tone: 'good' };
    case 'running':
      return { text: 'Exporting Now', tone: 'good' };
    case 'done':
      return { text: 'Exported', tone: 'neutral' };
    case 'cancelled':
      return { text: 'Export Cancelled', tone: 'neutral' };
    case 'skipped':
      return { text: 'Skipped', tone: 'neutral' };
    case 'failed':
      return { text: withReason('Export Failed'), tone: 'bad' };
    case 'error':
      return { text: withReason('Not Booked'), tone: 'bad' };
  }
}
