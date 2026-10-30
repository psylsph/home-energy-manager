export const COSY_BLOCK_REASONS: Record<string, string> = {
  pause: 'a battery pause is active',
  force: 'a manual Force Charge or Force Discharge is running',
  calibration: 'a battery calibration cycle is running',
  safety: 'a safety limit (high load or temperature) is holding the battery',
};

/**
 * Explains why Cosy is not setting its charge times, or `null` when nothing is
 * stopping it. The backend reports a short code in `cosy_blocked_by`; codes it
 * adds later still produce a generic warning rather than silence.
 */
export function cosyBlockedMessage(
  blockedBy: string | null | undefined,
  cosyEnabled: boolean | undefined,
): string | null {
  if (!cosyEnabled || !blockedBy) return null;
  const reason = COSY_BLOCK_REASONS[blockedBy];
  return `Cosy is on hold${reason ? `: ${reason}` : ''}. Its charge times won't be set until this clears, and it will resume by itself.`;
}
