import type { ExportOnceEvent, ExportOnceState, ScheduleSlot } from './types';

/** Mirrors the backend limit in `inverter::export_once`. */
const MAX_BOOKING_AHEAD_DAYS = 14;

export interface ExportOnceBooking {
  slot: number;
  /** `YYYY-MM-DD` */
  date: string;
  /** `HH:MM` */
  start: string;
  end: string;
  target_soc: number;
}

const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

const DATE_RE = /^(\d{4})-(\d{2})-(\d{2})$/;
const TIME_RE = /^([01]\d|2[0-3]):[0-5]\d$/;

/** Days since the epoch for a `YYYY-MM-DD` date, or null if it is not one. */
function dayNumber(date: string): number | null {
  const m = DATE_RE.exec(date);
  if (!m) return null;
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  const ms = Date.UTC(y, mo - 1, d);
  const check = new Date(ms);
  if (check.getUTCFullYear() !== y || check.getUTCMonth() !== mo - 1 || check.getUTCDate() !== d) {
    return null;
  }
  return ms / 86_400_000;
}

function dateFromDayNumber(day: number): string {
  const d = new Date(day * 86_400_000);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}`;
}

/**
 * Check a booking against the same rules the backend enforces, so the form
 * can explain a problem before posting. `nowLocal` is the inverter-local
 * `YYYY-MM-DD HH:MM`.
 */
export function validateBooking(
  booking: ExportOnceBooking,
  nowLocal: string,
  maxSlots: number,
): string | null {
  if (!Number.isInteger(booking.slot) || booking.slot < 1 || booking.slot > maxSlots) {
    return `Choose a slot between 1 and ${maxSlots}.`;
  }
  const day = dayNumber(booking.date);
  if (day === null) return 'Choose a date.';
  if (!TIME_RE.test(booking.start)) return 'Choose a start time.';
  if (!TIME_RE.test(booking.end)) return 'Choose an end time.';
  if (booking.end <= booking.start) {
    return 'The end must be after the start (a window cannot cross midnight).';
  }
  if (!Number.isInteger(booking.target_soc) || booking.target_soc < 4 || booking.target_soc > 99) {
    return 'The discharge floor must be between 4 and 99%.';
  }
  if (`${booking.date} ${booking.end}` <= nowLocal) {
    return 'That window has already ended.';
  }
  const today = dayNumber(nowLocal.slice(0, 10));
  if (today !== null && day > today + MAX_BOOKING_AHEAD_DAYS) {
    return `Exports can be booked at most ${MAX_BOOKING_AHEAD_DAYS} days ahead.`;
  }
  return null;
}

/** A sensible starting point for the form: the next whole hour, for an hour. */
export function defaultBooking(nowLocal: string): ExportOnceBooking {
  const today = nowLocal.slice(0, 10);
  const hour = Number(nowLocal.slice(11, 13));
  const pad = (n: number) => String(n).padStart(2, '0');
  if (hour >= 22) {
    const day = dayNumber(today);
    return {
      slot: 1,
      date: day === null ? today : dateFromDayNumber(day + 1),
      start: '18:00',
      end: '19:00',
      target_soc: 20,
    };
  }
  return {
    slot: 1,
    date: today,
    start: `${pad(hour + 1)}:00`,
    end: `${pad(hour + 2)}:00`,
    target_soc: 20,
  };
}

function dateLabel(stamp: string): string {
  const day = dayNumber(stamp.slice(0, 10));
  if (day === null) return stamp.slice(0, 10);
  const d = new Date(day * 86_400_000);
  return `${DAYS[d.getUTCDay()]} ${d.getUTCDate()} ${MONTHS[d.getUTCMonth()]}`;
}

/** `Sat 17 Oct, 18:00–19:00` (or both dates when it spans days). */
export function formatEventWindow(event: ExportOnceEvent): string {
  if (event.start.slice(0, 10) === event.end.slice(0, 10)) {
    return `${dateLabel(event.start)}, ${event.start.slice(11, 16)}–${event.end.slice(11, 16)}`;
  }
  return `${dateLabel(event.start)} ${event.start.slice(11, 16)} – ${dateLabel(event.end)} ${event.end.slice(11, 16)}`;
}

export function formatSlotRange(slot: ScheduleSlot): string {
  if (!slot.enabled) return 'not set';
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${pad(slot.start_hour)}:${pad(slot.start_minute)}–${pad(slot.end_hour)}:${pad(slot.end_minute)}`;
}

export function stateLabel(state: ExportOnceState): string {
  switch (state) {
    case 'pending':
      return 'Booked';
    case 'active':
      return 'Running';
    case 'done':
      return 'Finished';
    case 'cancelled':
      return 'Cancelled';
    case 'failed':
      return 'Failed';
  }
}

/** Live events by start time, then the five most recently finished. */
export function splitEvents(events: ExportOnceEvent[]): {
  upcoming: ExportOnceEvent[];
  recent: ExportOnceEvent[];
} {
  const live = (e: ExportOnceEvent) => e.state === 'pending' || e.state === 'active';
  const upcoming = events.filter(live).sort((a, b) => a.start.localeCompare(b.start));
  const recent = events
    .filter((e) => !live(e))
    .sort((a, b) => b.start.localeCompare(a.start))
    .slice(0, 5);
  return { upcoming, recent };
}
