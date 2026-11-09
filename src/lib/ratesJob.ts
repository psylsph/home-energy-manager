/** Helpers for re-pricing History and filling Octopus rates (issue #455). */

export type RatesJobKind = 'reprice' | 'backfill';

export interface RatesJobRecord {
  kind: RatesJobKind;
  /** Unix seconds when the job ended. */
  at: number;
  ok: boolean;
  /** History rows the job looked at. */
  rows: number;
  error: string | null;
}

export interface RatesJobStatus {
  running: boolean;
  kind: RatesJobKind | null;
  done: number;
  total: number;
  last: RatesJobRecord | null;
}

export interface RatesJobResponse {
  ok: boolean;
  data: RatesJobStatus;
}

/** Today's date in the browser's zone as `YYYY-MM-DD`, the latest a re-price can start. */
export function todayIso(now: Date = new Date()): string {
  const month = String(now.getMonth() + 1).padStart(2, '0');
  const day = String(now.getDate()).padStart(2, '0');
  return `${now.getFullYear()}-${month}-${day}`;
}

/** Whether `value` is a real `YYYY-MM-DD` date no later than `today`. */
export function isRepriceDateValid(value: string, today: string): boolean {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!match) return false;
  const [, y, m, d] = match;
  const parsed = new Date(Number(y), Number(m) - 1, Number(d));
  if (
    parsed.getFullYear() !== Number(y) ||
    parsed.getMonth() !== Number(m) - 1 ||
    parsed.getDate() !== Number(d)
  ) {
    return false;
  }
  return value <= today;
}

function rowsText(n: number): string {
  return `${n.toLocaleString('en-GB')} ${n === 1 ? 'row' : 'rows'}`;
}

/** The progress line while a job runs. */
export function progressText(status: RatesJobStatus): string {
  const what = status.kind === 'backfill' ? 'Filling rates' : 'Re-pricing';
  if (status.total <= 0) return `${what}…`;
  return `${what}… ${status.done.toLocaleString('en-GB')} of ${rowsText(status.total)}`;
}

/** How the latest job ended, as one sentence. */
export function resultText(last: RatesJobRecord): string {
  if (!last.ok) {
    const what = last.kind === 'backfill' ? 'Filling rates' : 'Re-pricing';
    return `${what} failed: ${last.error ?? 'unknown error'}`;
  }
  return last.kind === 'backfill'
    ? `Checked ${rowsText(last.rows)} for missing rates`
    : `Re-priced ${rowsText(last.rows)}`;
}
