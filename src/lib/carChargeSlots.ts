import type { SmartChargeDispatch, SmartChargingResponse } from './types';

/**
 * The car's planned Intelligent Octopus Go charge for the Status page's Next
 * Charge card (issue #474), from `GET /api/octopus/dispatches`. Pure: `now`
 * and the formatters are passed in.
 */
export interface CarChargeLine {
  /** `active` while a charge is running, `next` before one starts. */
  state: 'active' | 'next';
  /** `Car 23:30–05:30`, `Car Mon 01:00–03:00`, `Car charging until 05:30`, `… · Boost`. */
  text: string;
  /** Every part of the block is a boost: peak rate, battery not held. */
  boost: boolean;
  /** For the hover title: planned energy and how many more charges follow. */
  title: string;
}

/** How times and days are written; local time by default. */
export interface CarChargeFormat {
  time: (date: Date) => string;
  weekday: (date: Date) => string;
}

function localParts(date: Date, options: Intl.DateTimeFormatOptions): Record<string, string> {
  const out: Record<string, string> = {};
  for (const part of new Intl.DateTimeFormat('en-GB', options).formatToParts(date)) {
    out[part.type] = part.value;
  }
  return out;
}

/** Browser-local `HH:MM` and short weekday, like the Octopus page. */
export const LOCAL_FORMAT: CarChargeFormat = {
  time: (date) => {
    const p = localParts(date, { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
    return `${p.hour}:${p.minute}`;
  },
  weekday: (date) => localParts(date, { weekday: 'short' }).weekday ?? '',
};

const DAY_MS = 24 * 60 * 60 * 1000;

interface Block {
  start: number;
  end: number;
  /** Every part is a boost. */
  boost: boolean;
  /** Sum of the parts' planned energy; null when none says. */
  energyKwh: number | null;
}

function isBoost(dispatch: SmartChargeDispatch): boolean {
  return !dispatch.smart;
}

/**
 * Dispatches not yet over, merged where they touch or overlap (across cars),
 * because Octopus often splits a night into half-hour pieces. A block is a
 * boost only if every part is, so the line never under-reports off-peak.
 */
function blocks(dispatches: readonly SmartChargeDispatch[], now: number): Block[] {
  const parts = dispatches
    .map((d) => ({ start: Date.parse(d.start), end: Date.parse(d.end), dispatch: d }))
    .filter((p) => Number.isFinite(p.start) && Number.isFinite(p.end) && p.end > p.start && p.end > now)
    .sort((a, b) => a.start - b.start);
  const merged: Block[] = [];
  for (const p of parts) {
    const energy = p.dispatch.energy_added_kwh;
    const kwh = typeof energy === 'number' && Number.isFinite(energy) ? energy : null;
    const last = merged[merged.length - 1];
    if (last && p.start <= last.end) {
      last.end = Math.max(last.end, p.end);
      last.boost = last.boost && isBoost(p.dispatch);
      if (kwh != null) last.energyKwh = (last.energyKwh ?? 0) + kwh;
    } else {
      merged.push({ start: p.start, end: p.end, boost: isBoost(p.dispatch), energyKwh: kwh });
    }
  }
  return merged;
}

/**
 * The line to show, or `null` to show none: no answer, an error, no car on
 * the account, a list older than its stale limit, or nothing planned. An
 * unknown list never reads as "no charge planned".
 */
export function carChargeLine(
  response: SmartChargingResponse | null,
  now: Date,
  format: CarChargeFormat = LOCAL_FORMAT,
): CarChargeLine | null {
  if (!response || response.error || !(response.devices > 0) || !response.checked_at) return null;
  const checkedAt = Date.parse(response.checked_at);
  const nowMs = now.getTime();
  if (!Number.isFinite(checkedAt)) return null;
  const staleMs = (Number.isFinite(response.stale_after_minutes) ? response.stale_after_minutes : 15) * 60_000;
  if (nowMs - checkedAt > staleMs) return null;

  const [first, ...rest] = blocks(response.dispatches ?? [], nowMs);
  if (!first) return null;
  const active = first.start <= nowMs;
  const start = new Date(first.start);
  const end = new Date(first.end);
  let text: string;
  if (active) {
    text = `Car charging until ${format.time(end)}`;
  } else {
    const day = first.start - nowMs > DAY_MS ? `${format.weekday(start)} ` : '';
    text = `Car ${day}${format.time(start)}–${format.time(end)}`;
  }
  if (first.boost) text += ' · Boost';

  const title = [
    first.energyKwh != null ? `${first.energyKwh.toFixed(1)} kWh planned` : null,
    rest.length > 0 ? `${rest.length} more planned` : null,
  ]
    .filter(Boolean)
    .join(' · ');
  return { state: active ? 'active' : 'next', text, boost: first.boost, title };
}
