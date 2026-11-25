import type { SecondUnitInfo } from './types';

/**
 * One line saying what the house figures did with the second HEM on the last
 * reading (issue #456). Empty when no second HEM is configured, so callers
 * can render nothing.
 */
export function secondUnitStatus(info: SecondUnitInfo | null | undefined): string {
  if (!info) return '';
  switch (info.state) {
    case 'merged': {
      const age = info.age_secs;
      const when = age === undefined ? '' : age < 5 ? ' (just now)' : ` (${Math.round(age)} s ago)`;
      return `Reading the second inverter${when}. Home, Solar and Grid cover both inverters.`;
    }
    case 'unreachable':
      return 'The second HEM hasn’t answered yet. Home shows this inverter only.';
    case 'stale':
      return 'The second HEM went quiet, so it’s left out. Home shows this inverter only.';
    case 'invalid':
      return 'This address can’t be used: it’s this same inverter, a Gateway, or a HEM that reads another one.';
    default:
      return '';
  }
}
