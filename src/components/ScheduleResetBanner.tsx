import { useState } from 'react';
import { apiPost } from '../lib/api';
import { formatSavedAt } from '../lib/scheduleReset';
import { useInverterStore } from '../store/useInverterStore';
import ReadOnlyHidden from './ReadOnlyHidden';

const BUTTON =
  'font-sans text-sm font-semibold px-4 py-1.5 rounded-lg transition-colors disabled:opacity-50';

/**
 * Control page notice for a dongle factory reset (issue #397): the
 * inverter's schedule went back to defaults while HEM couldn't see it, as a
 * crashed dongle does. HEM kept the last schedule it saw, so one button puts
 * it back. Nothing is written until the user presses Restore; the notice
 * clears when the inverter reads back a schedule that has held.
 */
export default function ScheduleResetBanner() {
  const info = useInverterStore((s) => s.snapshot?.schedule_reset);
  const [busy, setBusy] = useState<'restore' | 'dismiss' | null>(null);
  const [result, setResult] = useState<{ text: string; ok: boolean } | null>(null);
  // The notice that was dismissed, so the banner goes at once instead of
  // waiting for the next poll's snapshot to stop carrying it.
  const [dismissedAt, setDismissedAt] = useState<number | null>(null);
  if (!info || info.detected_at === dismissedAt) return null;

  const run = async (action: 'restore' | 'dismiss') => {
    setBusy(action);
    setResult(null);
    try {
      await apiPost(`/api/control/schedule-reset/${action}`);
      if (action === 'dismiss') {
        setDismissedAt(info.detected_at);
      } else {
        setResult({
          text: 'Restore sent. This clears once the inverter reads back the saved schedule.',
          ok: true,
        });
      }
    } catch (e: unknown) {
      setResult({
        text: e instanceof Error ? e.message : `Failed to ${action} the schedule`,
        ok: false,
      });
    } finally {
      setBusy(null);
    }
  };

  return (
    <div
      role="alert"
      className="rounded-xl border border-amber-500/50 bg-amber-950/60 p-4 text-amber-100 flex flex-col gap-3"
    >
      <div className="flex flex-col gap-1">
        <strong className="font-sans text-sm">Inverter Schedule Was Reset</strong>
        <p className="font-sans text-sm text-amber-100/85">
          The schedule went back to factory defaults while the connection was down, as a dongle reset
          does. Restore the schedule from {formatSavedAt(info.saved_at)}?
        </p>
      </div>
      <ReadOnlyHidden>
        <div className="flex flex-wrap gap-2">
          <button
            type="button"
            onClick={() => void run('restore')}
            disabled={busy !== null}
            className={`${BUTTON} bg-accent text-on-accent hover:opacity-90`}
          >
            {busy === 'restore' ? 'Restoring…' : 'Restore Schedule'}
          </button>
          <button
            type="button"
            onClick={() => void run('dismiss')}
            disabled={busy !== null}
            className={`${BUTTON} bg-bg-elevated text-text-primary hover:bg-bg-base`}
          >
            {busy === 'dismiss' ? 'Dismissing…' : 'Dismiss'}
          </button>
        </div>
      </ReadOnlyHidden>
      {result && (
        <p
          role="status"
          className={`font-sans text-xs ${result.ok ? 'text-amber-100/85' : 'text-red-300'}`}
        >
          {result.text}
        </p>
      )}
    </div>
  );
}
