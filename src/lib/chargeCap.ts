/**
 * Optional charge cap (issue #426): stop charging the battery at a chosen
 * SOC and let it charge again once it has fallen to a lower resume level.
 * HEM enforces it from the poll loop with the inverter's own pause, so it
 * only works while HEM is running.
 */

import { formatPercent } from './format';

export type ChargeCapStateName =
  | 'off'
  | 'unsupported'
  | 'waiting'
  | 'pending'
  | 'holding'
  | 'releasing'
  | 'yielding'
  | 'blocked'
  | 'unreadable';

/** `GET`/`POST /api/charge-cap`. */
export interface ChargeCapStatus {
  ok: boolean;
  enabled: boolean;
  cap_soc: number;
  resume_soc: number;
  state: ChargeCapStateName;
  supported: boolean;
  connected: boolean;
  /** A pause the cap started may still be on the inverter. */
  holding: boolean;
  min_cap_soc: number;
  max_cap_soc: number;
  min_resume_soc: number;
  min_gap: number;
  /** Unix seconds of the last reading at 100%, if there is one. */
  last_full_charge_at?: number | null;
  /** The full-charge reminder's interval, in days. */
  full_charge_days?: number;
}

export type ChargeCapLimits = Pick<ChargeCapStatus, 'min_cap_soc' | 'max_cap_soc' | 'min_resume_soc' | 'min_gap'>;

/** A whole-number SOC typed into a field, or null when it isn't one. */
export function parseSoc(text: string): number | null {
  const trimmed = text.trim();
  if (!/^\d{1,3}$/.test(trimmed)) return null;
  return Number(trimmed);
}

/** Why the levels can't be saved, or null when they can (mirrors the backend's checks). */
export function chargeCapError(cap: number | null, resume: number | null, limits: ChargeCapLimits): string | null {
  if (cap == null || cap < limits.min_cap_soc || cap > limits.max_cap_soc) {
    return `Choose a cap between ${limits.min_cap_soc}% and ${limits.max_cap_soc}%.`;
  }
  if (resume == null || resume < limits.min_resume_soc) {
    return `Resume charging at ${limits.min_resume_soc}% or more.`;
  }
  if (resume + limits.min_gap > cap) {
    return `Resume charging at least ${limits.min_gap}% below the cap.`;
  }
  return null;
}

/** What the cap is doing now, as a sentence for the Control page. */
export function describeChargeCapState(
  state: ChargeCapStateName,
  cap: number,
  resume: number,
  pauseUnreadable = false,
): string {
  const text = describeState(state, cap, resume);
  // A pause the cap started stays on until a read lets it release it.
  if (pauseUnreadable && (state === 'holding' || state === 'releasing')) {
    return `${text} HEM can't read the pause, so it can't let go yet.`;
  }
  return text;
}

function describeState(state: ChargeCapStateName, cap: number, resume: number): string {
  switch (state) {
    case 'off':
      return 'Off.';
    case 'unsupported':
      return "This inverter can't pause charging, so the charge cap isn't available.";
    case 'waiting':
      return `Charging normally until the battery reaches ${cap}%.`;
    case 'pending':
      return `At ${cap}%, pausing charging after one more reading.`;
    case 'holding':
      return `Charging paused at ${cap}%. Charges again at ${resume}%.`;
    case 'releasing':
      return 'Letting the battery charge again.';
    case 'yielding':
      return 'A charge slot or calibration is charging the battery; the cap waits until it ends.';
    case 'blocked':
      return 'Another pause is set on the inverter; the cap waits until it ends.';
    case 'unreadable':
      return "HEM can't read this inverter's pause setting right now, so the cap can't start.";
  }
}

/**
 * Colour of the charge-cap marker on every battery gauge: the app accent,
 * which no SOC tier (red, amber, green) uses, so it stands out on both themes.
 */
export const CAP_MARKER_COLOR = 'var(--app-accent, #22D3EE)';

/** Tooltip and accessible text for the cap marker. */
export function capTitle(capSoc: number): string {
  return `Charge cap ${formatPercent(capSoc)}`;
}

/**
 * Where to draw the cap line on a battery gauge, or null for none: only while
 * the cap is on and supported (`charge_cap_enabled` covers both) and below 100%.
 */
export function capLineSoc(snapshot: { charge_cap_enabled?: boolean; charge_cap_soc?: number } | null | undefined): number | null {
  if (!snapshot?.charge_cap_enabled) return null;
  const cap = snapshot.charge_cap_soc;
  if (typeof cap !== 'number' || !Number.isFinite(cap) || cap <= 0 || cap >= 100) return null;
  return cap;
}

/** Whole days since `atSeconds`, or null when there's no reading. */
export function daysSince(atSeconds: number | null | undefined, nowMs: number): number | null {
  if (atSeconds == null || !Number.isFinite(atSeconds)) return null;
  return Math.max(0, Math.floor((nowMs - atSeconds * 1000) / 86_400_000));
}

/** "Last full charge: today / yesterday / N days ago / not in the history". */
export function describeLastFullCharge(atSeconds: number | null | undefined, nowMs: number): string {
  const days = daysSince(atSeconds, nowMs);
  if (days == null) return 'Last full charge: not in the history yet.';
  if (days === 0) return 'Last full charge: today.';
  if (days === 1) return 'Last full charge: yesterday.';
  return `Last full charge: ${days} days ago.`;
}
