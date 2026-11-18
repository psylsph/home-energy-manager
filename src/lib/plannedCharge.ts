/**
 * The Planner's coming write to charge slot 1, for the Status page's Next
 * Charge card (issue #481), from `GET /api/forecast/plan/upcoming`.
 *
 * The card's window is always what the inverter holds; this line only says the
 * Planner is about to change it. The backend names the tariff's cheap period,
 * not the slot it will write (the plan may write a shorter one inside it), so
 * the line says when, never what. Pure: the times are passed in.
 */

export interface UpcomingPlanWrite {
  trigger: 'auto_apply' | 'refresh';
  /** The cheap period the write plans, `HH:MM` on the backend's clock. */
  cheapStart: string;
  cheapEnd: string;
  /** When the backend writes, `HH:MM` on its clock. */
  appliesAt: string;
  /** Minutes from the answer to the write (real elapsed time). */
  appliesInMinutes: number;
  /** Calendar days from the answer to the write, on the backend's clock: 0 today, 1 tomorrow. */
  appliesDay: number;
  /** The cheap period is already under way when it is planned. */
  running: boolean;
}

export interface PlannedChargeLine {
  /** `Planner updates at 21:30`, `Planner updates tomorrow at 01:30`. */
  text: string;
  /** For the hover title: what is being updated and why. */
  title: string;
}

/** An answer older than this is dropped (the backend is unreachable). */
export const PLANNED_CHARGE_STALE_MS = 5 * 60_000;

const HHMM = /^([01]\d|2[0-3]):[0-5]\d$/;

/**
 * Reads the endpoint's body. `null`: nothing is coming. `undefined`: the
 * answer is unusable, so the caller keeps its last one.
 */
export function readUpcomingPlanWrite(body: unknown): UpcomingPlanWrite | null | undefined {
  if (typeof body !== 'object' || body === null) return undefined;
  const { ok, data } = body as { ok?: unknown; data?: unknown };
  if (ok !== true || data === undefined) return undefined;
  if (data === null) return null;
  if (typeof data !== 'object') return undefined;
  const d = data as Record<string, unknown>;
  const { trigger, cheap_start: cheapStart, cheap_end: cheapEnd, applies_at: appliesAt } = d;
  const minutes = d.applies_in_minutes;
  const day = d.applies_day;
  if (trigger !== 'auto_apply' && trigger !== 'refresh') return undefined;
  if (![cheapStart, cheapEnd, appliesAt].every((t) => typeof t === 'string' && HHMM.test(t))) return undefined;
  if (typeof minutes !== 'number' || !Number.isFinite(minutes) || minutes < 0) return undefined;
  if (typeof day !== 'number' || !Number.isInteger(day) || day < 0) return undefined;
  return {
    trigger,
    cheapStart: cheapStart as string,
    cheapEnd: cheapEnd as string,
    appliesAt: appliesAt as string,
    appliesInMinutes: minutes,
    appliesDay: day,
    running: d.running === true,
  };
}

/**
 * The line to show, or `null` when nothing is coming, the write is due this
 * minute or has passed (the card follows the inverter by then), it is more
 * than a day away (nothing to say yet), or the answer is stale. `fetchedAtMs` is when the answer arrived; the minutes since are
 * taken off, so the line goes at the write time without waiting for a refresh.
 */
export function plannedChargeLine(
  upcoming: UpcomingPlanWrite | null,
  fetchedAtMs: number,
  nowMs: number,
): PlannedChargeLine | null {
  if (!upcoming) return null;
  const elapsedMs = nowMs - fetchedAtMs;
  if (elapsedMs < 0 || elapsedMs > PLANNED_CHARGE_STALE_MS) return null;
  const remaining = upcoming.appliesInMinutes - Math.floor(elapsedMs / 60_000);
  if (remaining <= 0) return null;
  // The backend says which day; two days out says nothing useful yet.
  if (upcoming.appliesDay > 1) return null;
  const tomorrow = upcoming.appliesDay === 1;
  const when = `${tomorrow ? 'tomorrow ' : ''}at ${upcoming.appliesAt}`;
  const who = upcoming.trigger === 'refresh' ? 'The nightly auto-refresh' : 'Auto-apply';
  const period = `${upcoming.cheapStart}–${upcoming.cheapEnd}`;
  const state = upcoming.running ? 'under way' : 'coming up';
  return {
    text: `Planner updates ${when}`,
    title: `${who} rewrites charge slot 1 ${when} for the ${period} cheap period (${state}), so the window above may change then.`,
  };
}
