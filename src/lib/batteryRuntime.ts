/**
 * How long the battery lasts (issue #472), for the house view's Battery
 * Level card: the `GET /api/battery/runtime` answer turned into one line of
 * text and a one-to-three-dot confidence. Pure: `now` and the formatters are
 * passed in.
 */

export type RuntimeConfidence = 'high' | 'medium' | 'low';

export type BatteryRuntime =
  | { outcome: 'empty_at'; at: number; earliest: number | null; latest: number | null; confidence: RuntimeConfidence; computed_at?: number }
  | { outcome: 'recharges_first'; at: number; by: 'charge' | 'solar'; computed_at?: number }
  | { outcome: 'lasts_beyond'; hours: number; computed_at?: number }
  | { outcome: 'at_reserve'; computed_at?: number }
  | { outcome: 'full_at'; at: number; soc: number; computed_at?: number }
  | { outcome: 'paused'; computed_at?: number }
  | { outcome: 'charging'; computed_at?: number }
  | { outcome: 'unknown'; reason?: string; computed_at?: number };

const CONFIDENCES: readonly RuntimeConfidence[] = ['high', 'medium', 'low'];
const isNum = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);
const isNumOrNull = (v: unknown) => v === null || isNum(v);

/** Accept only a well-formed answer; anything else means "don't show". */
export function readBatteryRuntime(value: unknown): BatteryRuntime | null {
  if (!value || typeof value !== 'object') return null;
  const { ok, data } = value as { ok?: unknown; data?: Record<string, unknown> };
  if (ok !== true || !data || typeof data !== 'object') return null;
  switch (data.outcome) {
    case 'empty_at':
      return isNum(data.at) && isNumOrNull(data.earliest) && isNumOrNull(data.latest)
        && CONFIDENCES.includes(data.confidence as RuntimeConfidence)
        ? (data as BatteryRuntime) : null;
    case 'recharges_first':
      return isNum(data.at) && (data.by === 'charge' || data.by === 'solar') ? (data as BatteryRuntime) : null;
    case 'lasts_beyond':
      return isNum(data.hours) ? (data as BatteryRuntime) : null;
    case 'full_at':
      return isNum(data.at) && isNum(data.soc) ? (data as BatteryRuntime) : null;
    case 'paused':
    case 'at_reserve':
    case 'charging':
    case 'unknown':
      return data as BatteryRuntime;
    default:
      return null;
  }
}

/** How times and days are written; local time by default. */
export interface RuntimeFormat {
  time: (date: Date) => string;
  /** A key for the calendar day, to tell today from tomorrow. */
  day: (date: Date) => string;
  weekday: (date: Date) => string;
}

const pad = (n: number) => String(n).padStart(2, '0');

export const LOCAL_RUNTIME_FORMAT: RuntimeFormat = {
  time: (d) => `${pad(d.getHours())}:${pad(d.getMinutes())}`,
  day: (d) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`,
  weekday: (d) => d.toLocaleDateString('en-GB', { weekday: 'short' }),
};

export interface BatteryRuntimeLine {
  text: string;
  /** Lit dots out of three; `null` when the line isn't a prediction of empty. */
  dots: 1 | 2 | 3 | null;
  confidence: RuntimeConfidence | null;
  /** For the hover title and screen readers. */
  title: string;
  /** For the ⓘ: what the time and the dots mean, or why there's no time; `null` when the line says it all. */
  help: string | null;
}

const DOT_WORDS: Record<RuntimeConfidence, string> = {
  high: 'High confidence (three dots): a busy and a quiet day are within an hour of each other.',
  medium: 'Medium confidence (two dots): a busy and a quiet day are within three hours of each other.',
  low: "Low confidence (one dot): a busy and a quiet day are more than three hours apart, or there's under two weeks of history.",
};

/** Why an `unknown` answer had no time, for the ⓘ (issue #476). */
const UNKNOWN_REASONS = new Map<string, string>([
  ['no_snapshot', "There's no live reading from the inverter yet."],
  ['not_connected', "There's no live reading from the inverter while it's disconnected."],
  ['no_battery_capacity', "The inverter hasn't reported its battery capacity or power limits yet."],
  ['insufficient_consumption_history', "There isn't enough usage history yet to forecast your load."],
]);
const UNKNOWN_FALLBACK = "The forecast couldn't be run from the current readings.";

const PAUSED_HELP = "The battery won't discharge until the pause ends (the Pause button, a limiter, the Octopus hold or the inverter's own pause), and when that is isn't known, so there's no time to give.";

const CHARGING_HELP = "The battery is charging, but the forecast for this hour doesn't predict it: a sunnier day than forecast, or a Force Charge, which the forecast doesn't model. A time returns once the charge stops or the forecast catches up.";

/** The backend reuses an answer for five minutes; past this it's stale. */
const MAX_AGE_SECS = 15 * 60;
const DOTS: Record<RuntimeConfidence, 1 | 2 | 3> = { high: 3, medium: 2, low: 1 };

/** Which day `at` is on: `null` for today, else `tomorrow` or a weekday. */
function dayWord(at: number, now: Date, f: RuntimeFormat): string | null {
  const date = new Date(at * 1000);
  // A day on by the clock, then named by `format.day`, so tests can pin
  // the time zone through the formatters.
  const tomorrow = new Date(now.getTime() + 24 * 60 * 60 * 1000);
  if (f.day(date) === f.day(now)) return null;
  if (f.day(date) === f.day(tomorrow)) return 'tomorrow';
  return f.weekday(date);
}

/** `at 22:30`, `tomorrow at 06:00` or `Mon at 06:00`, relative to `now`. */
function when(at: number, now: Date, f: RuntimeFormat): string {
  const day = dayWord(at, now, f);
  const time = f.time(new Date(at * 1000));
  return day ? `${day} at ${time}` : `at ${time}`;
}

/** The card's line, or `null` to show none (stale, or an outcome this build doesn't know). */
export function batteryRuntimeLine(
  runtime: BatteryRuntime | null,
  now: Date,
  format: RuntimeFormat = LOCAL_RUNTIME_FORMAT,
): BatteryRuntimeLine | null {
  if (!runtime) return null;
  if (isNum(runtime.computed_at) && now.getTime() / 1000 - runtime.computed_at > MAX_AGE_SECS) return null;
  switch (runtime.outcome) {
    case 'empty_at': {
      const t = (s: number) => format.time(new Date(s * 1000));
      const range = runtime.earliest != null && runtime.latest != null
        ? `Likely between ${t(runtime.earliest)} and ${t(runtime.latest)}`
        : runtime.earliest != null ? `Not before about ${t(runtime.earliest)}` : null;
      return {
        text: `Empty ${when(runtime.at, now, format)}`,
        dots: DOTS[runtime.confidence],
        confidence: runtime.confidence,
        title: range ? `${range} · ${runtime.confidence} confidence` : `${runtime.confidence[0]!.toUpperCase()}${runtime.confidence.slice(1)} confidence`,
        help: [
          range ? `${range}.` : null,
          'This is when the battery reaches its reserve, from your usage history and the solar forecast, to the nearest half hour.',
          DOT_WORDS[runtime.confidence],
        ].filter(Boolean).join(' '),
      };
    }
    case 'recharges_first': {
      const day = dayWord(runtime.at, now, format);
      const time = format.time(new Date(runtime.at * 1000));
      const text = runtime.by === 'charge'
        ? `Lasts until ${day ? `${day}'s` : 'the'} ${time} charge`
        : `Lasts until solar ${when(runtime.at, now, format)}`;
      return { text, dots: null, confidence: null, title: text, help: null };
    }
    case 'lasts_beyond':
      return { text: `Lasts over ${runtime.hours} hours`, dots: null, confidence: null, title: '', help: null };
    case 'at_reserve':
      return { text: 'At its reserve', dots: null, confidence: null, title: '', help: null };
    case 'full_at': {
      const text = `${runtime.soc >= 100 ? 'Full' : `${Math.round(runtime.soc)}%`} ${when(runtime.at, now, format)}`;
      return { text, dots: null, confidence: null, title: '', help: null };
    }
    // With the toggle on, a blank card looks broken: say why there's no time.
    case 'paused':
      return { text: 'Discharge paused', dots: null, confidence: null, title: '', help: PAUSED_HELP };
    case 'charging':
      return { text: 'Unable to calculate: charging faster than forecast', dots: null, confidence: null, title: '', help: CHARGING_HELP };
    case 'unknown':
      return {
        text: 'Unable to calculate',
        dots: null,
        confidence: null,
        title: '',
        help: UNKNOWN_REASONS.get(runtime.reason ?? '') ?? UNKNOWN_FALLBACK,
      };
    default:
      return null;
  }
}
