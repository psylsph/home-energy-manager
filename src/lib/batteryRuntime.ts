/**
 * How long the battery lasts (issue #472), for the house view's Battery
 * Level card: the `GET /api/battery/runtime` answer turned into one line of
 * text and a one-to-three-dot confidence. Pure: `now` and the formatters are
 * passed in.
 */

export type RuntimeConfidence = 'high' | 'medium' | 'low';

export type BatteryRuntime =
  | { outcome: 'empty_at'; at: number; earliest: number | null; latest: number | null; confidence: RuntimeConfidence; computed_at?: number }
  | {
    outcome: 'recharges_first'; at: number; by: 'charge' | 'solar';
    /** When a busy day would run out first, and how sure; absent from an older backend. */
    risk?: number; confidence?: RuntimeConfidence; computed_at?: number;
  }
  | { outcome: 'lasts_beyond'; hours: number; computed_at?: number }
  | { outcome: 'at_reserve'; computed_at?: number }
  | { outcome: 'below_reserve'; computed_at?: number }
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
    case 'recharges_first': {
      if (!isNum(data.at) || (data.by !== 'charge' && data.by !== 'solar')) return null;
      // A risk travels with its confidence; half of that pair is no answer.
      const hasRisk = data.risk !== undefined || data.confidence !== undefined;
      if (hasRisk && !(isNum(data.risk) && CONFIDENCES.includes(data.confidence as RuntimeConfidence))) return null;
      return data as BatteryRuntime;
    }
    case 'lasts_beyond':
      return isNum(data.hours) ? (data as BatteryRuntime) : null;
    case 'full_at':
      return isNum(data.at) && isNum(data.soc) ? (data as BatteryRuntime) : null;
    case 'paused':
    case 'at_reserve':
    case 'below_reserve':
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
  /** The hour of day (0–23), to tell an evening from earlier today. */
  hour: (date: Date) => number;
  /** A key for the calendar day, to tell today from tomorrow. */
  day: (date: Date) => string;
  weekday: (date: Date) => string;
}

const pad = (n: number) => String(n).padStart(2, '0');

export const LOCAL_RUNTIME_FORMAT: RuntimeFormat = {
  time: (d) => `${pad(d.getHours())}:${pad(d.getMinutes())}`,
  hour: (d) => d.getHours(),
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

/** What the ⓘ says the dots mean when a busy day could run out before `refill`. */
function riskDotWords(confidence: RuntimeConfidence, refill: 'charge' | 'solar'): string {
  const before = refill === 'charge' ? 'the charge' : 'solar takes over';
  const within = refill === 'charge' ? 'the charge' : 'solar taking over';
  switch (confidence) {
    case 'high': return `Three dots: a busier day would run out within an hour of ${within}, a close call.`;
    case 'medium': return `Two dots: a busier day would run out up to three hours before ${before}.`;
    case 'low': return `One dot: a busier day would run out more than three hours before ${before}, or there's under two weeks of history.`;
  }
}

/** Why an `unknown` answer had no time, for the ⓘ (issue #476). */
const UNKNOWN_REASONS = new Map<string, string>([
  ['no_snapshot', "There's no live reading from the inverter yet."],
  ['not_connected', "There's no live reading from the inverter while it's disconnected."],
  ['no_battery_capacity', "The inverter hasn't reported its battery capacity or power limits yet."],
  ['insufficient_consumption_history', "There isn't enough usage history yet to forecast your load."],
]);
const UNKNOWN_FALLBACK = "The forecast couldn't be run from the current readings.";

const PAUSED_HELP = "The battery won't discharge until the pause ends (the Pause button, a limiter, the Octopus hold or the inverter's own pause). HEM can't tell when that will be, so there's no time to give.";

const CHARGING_HELP = "The battery is charging, but the forecast for this hour doesn't predict it: a sunnier day than forecast, or a Force Charge, which the forecast doesn't model. A time returns once the charge stops or the forecast catches up.";

/** The backend reuses an answer for five minutes; past this it's stale. */
const MAX_AGE_SECS = 15 * 60;
/** A time this far behind the clock is gone (issue #472); a little slack for clock drift. */
const PAST_SLACK_SECS = 60;
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

/** From this hour, a time later today is "tonight". */
const EVENING_HOUR = 18;

/** `tonight` for an evening still to come today, else the day word (`null` for earlier today). */
function whenWord(at: number, now: Date, f: RuntimeFormat): string | null {
  const day = dayWord(at, now, f);
  if (day) return day;
  return f.hour(new Date(at * 1000)) >= EVENING_HOUR ? 'tonight' : null;
}

/** The day word with a space after it (`tonight `, `tomorrow `, `Mon `), or nothing earlier today. */
function whenPrefix(at: number, now: Date, f: RuntimeFormat): string {
  const word = whenWord(at, now, f);
  return word ? `${word} ` : '';
}

/** `tonight at 22:30`, `tomorrow at 06:00`, `Mon at 06:00`, or just `at 22:30` earlier today. */
function atTime(at: number, now: Date, f: RuntimeFormat): string {
  return `${whenPrefix(at, now, f)}at ${f.time(new Date(at * 1000))}`;
}

/** `at about 22:00` with its day in front: `tonight at about 22:00`, `tomorrow at about 06:00`. */
function aboutTime(at: number, now: Date, f: RuntimeFormat): string {
  return `${whenPrefix(at, now, f)}at about ${f.time(new Date(at * 1000))}`;
}

/** The ends of a range closer than this are one time, said "about". */
const SAME_TIME_SECS = 30 * 60;

/** The card's line, or `null` to show none (stale, or an outcome this build doesn't know). */
export function batteryRuntimeLine(
  runtime: BatteryRuntime | null,
  now: Date,
  format: RuntimeFormat = LOCAL_RUNTIME_FORMAT,
): BatteryRuntimeLine | null {
  if (!runtime) return null;
  if (isNum(runtime.computed_at) && now.getTime() / 1000 - runtime.computed_at > MAX_AGE_SECS) return null;
  // A cached answer can outlive its time: show nothing, not a time that has gone.
  if ('at' in runtime && isNum(runtime.at) && runtime.at < now.getTime() / 1000 - PAST_SLACK_SECS) return null;
  const t = (secs: number) => format.time(new Date(secs * 1000));
  switch (runtime.outcome) {
    case 'empty_at': {
      const { earliest, latest } = runtime;
      // A range start that has gone is no start: say the likely time.
      const started = earliest != null && earliest < now.getTime() / 1000 - PAST_SLACK_SECS;
      const wide = earliest != null && latest != null && latest - earliest > SAME_TIME_SECS;
      let text: string;
      let range: string | null = null;
      if (wide && !started) {
        text = `Empty ${whenPrefix(earliest, now, format)}between ${t(earliest)} and ${t(latest)}`;
        range = `between ${t(earliest)} and ${t(latest)}`;
      } else if (earliest != null && latest == null && !started) {
        text = `Empty ${atTime(earliest, now, format)} or later`;
        range = `not before about ${t(earliest)}`;
      } else {
        text = `Empty ${aboutTime(runtime.at, now, format)}`;
        if (wide) range = `between ${t(earliest)} and ${t(latest)}`;
      }
      const most = `Most likely ${t(runtime.at)}`;
      return {
        text,
        dots: DOTS[runtime.confidence],
        confidence: runtime.confidence,
        title: [most, range, `${runtime.confidence} confidence`].filter(Boolean).join(' · '),
        help: [
          `${most}${range ? `, ${range}` : ''}.`,
          'This is when the battery reaches its reserve, from your usage history and the solar forecast. A busier day could empty it sooner, a quiet one later.',
          DOT_WORDS[runtime.confidence],
        ].join(' '),
      };
    }
    case 'recharges_first': {
      const charge = runtime.by === 'charge';
      if (runtime.risk != null && runtime.confidence) {
        // A busy evening could run out first: the line says so, with dots.
        const { risk, at, confidence } = runtime;
        const riskDay = dayWord(risk, now, format);
        const atDay = dayWord(at, now, format);
        const where = atDay && atDay !== riskDay ? ` ${atDay}` : '';
        const refill = charge ? `charge${where} at ${t(at)}` : `solar${where} at about ${t(at)}`;
        // A start that has gone is no start (a cached answer outliving it).
        const gone = risk < now.getTime() / 1000 - PAST_SLACK_SECS;
        const text = gone
          ? `May run out before ${charge ? `${atDay ? `${atDay}'s` : 'the'} ${t(at)} charge` : `solar${atDay ? ` ${atDay}` : ''} at about ${t(at)}`}`
          : `Empty ${atTime(risk, now, format)} or later, ${refill}`;
        const most = charge
          ? `Most likely it lasts until the ${t(at)} charge`
          : `Most likely it lasts until solar at about ${t(at)}`;
        const busy = `a busier day than usual (more use, less sun) could empty it from ${t(risk)}`;
        return {
          text,
          dots: DOTS[confidence],
          confidence,
          title: `${most}, but ${busy} · ${confidence} confidence`,
          help: `${most}, but ${busy}. ${riskDotWords(confidence, runtime.by)}`,
        };
      }
      const day = dayWord(runtime.at, now, format);
      const text = charge
        ? `Lasts until ${day ? `${day}'s` : 'the'} ${t(runtime.at)} charge`
        : `Lasts until solar${day ? ` ${day}` : ''} at about ${t(runtime.at)}`;
      return { text, dots: null, confidence: null, title: text, help: null };
    }
    case 'lasts_beyond':
      return { text: `Lasts over ${runtime.hours} hours`, dots: null, confidence: null, title: '', help: null };
    case 'at_reserve':
      return { text: 'At Reserve', dots: null, confidence: null, title: '', help: null };
    case 'below_reserve':
      return { text: 'Below Reserve', dots: null, confidence: null, title: '', help: null };
    case 'full_at': {
      const day = dayWord(runtime.at, now, format);
      const text = `${runtime.soc >= 100 ? 'Full' : `${Math.round(runtime.soc)}%`}${day ? ` ${day}` : ''} at about ${t(runtime.at)}`;
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
