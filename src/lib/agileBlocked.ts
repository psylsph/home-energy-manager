const REASONS: Record<string, string> = {
  timed_export: 'a Timed Export schedule has the inverter',
  manual_mode: 'the inverter is in a manually chosen mode (Timed Demand)',
  pause: 'a battery pause is active',
  force: 'a manual Force Charge or Force Discharge is running',
  calibration: 'a battery calibration cycle is running',
  safety: 'a safety limit (high load or temperature) is holding the battery',
};

/**
 * Explains why Agile is not setting its charge and discharge times, or `null`
 * when nothing is stopping it. The backend reports a short code in
 * `agile_blocked_by`; codes it adds later still produce a generic warning
 * rather than silence.
 */
export function agileBlockedMessage(
  blockedBy: string | null | undefined,
  agileEnabled: boolean | undefined,
): string | null {
  if (!agileEnabled || !blockedBy) return null;
  const reason = REASONS[blockedBy];
  return `Agile is on hold${reason ? `: ${reason}` : ''}. Its charge and discharge times won't be set until this clears, and it will resume by itself.`;
}
