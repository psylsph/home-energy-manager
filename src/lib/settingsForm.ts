// Pure helpers behind the Settings page sections (src/components/settings).

import type { PollSettings } from './types';

/** The poll intervals offered, in seconds. */
export const VALID_INTERVALS = [5, 10, 15, 20, 30, 45, 60];

/** Snap a poll interval to the nearest offered value (the lower on a tie). */
export const clampInterval = (v: number) =>
  VALID_INTERVALS.reduce((a, b) => (Math.abs(b - v) < Math.abs(a - v) ? b : a));

/**
 * A network-scan result's address: older backends name it `ip`, newer ones
 * `host`.
 */
export function scannedHost(found: { host: string }): string {
  return 'ip' in found ? (found as Record<string, unknown>).ip as string : found.host;
}

/** True when both an Octopus account number and API key are saved. */
export function octopusConfigured(s: Partial<PollSettings>): boolean {
  return Boolean(s.octopus_api_key_configured) && (s.octopus_account_number ?? '').trim() !== '';
}

/** A positive number from a form field; blank, junk or ≤ 0 is 0. */
export function positiveOrZero(raw: string): number {
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? n : 0;
}

/** One External CT Meter Array row as the form holds it. */
export interface CtArrayRow {
  meter_address: number;
  name: string;
  rated_kw: string;
}

/**
 * The CT arrays to save: names trimmed, blank or junk kWp as 0, and rows
 * dropped unless their meter is 1–8 — the dongle polls only those external
 * clamps, and 0x00 is the built-in grid CT, never solar.
 */
export function cleanCtArrays(rows: CtArrayRow[]) {
  return rows
    .map((a) => ({
      meter_address: Number(a.meter_address),
      name: a.name.trim(),
      rated_kw: positiveOrZero(a.rated_kw),
    }))
    .filter((a) => Number.isInteger(a.meter_address) && a.meter_address >= 1 && a.meter_address <= 8);
}
