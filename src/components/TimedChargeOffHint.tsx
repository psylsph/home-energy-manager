import { useInverterStore } from '../store/useInverterStore';
import { plannerTimedChargeOff } from '../lib/plannerMode';
import PageLink from './PageLink';

/**
 * Status page hint (issue #448): Automatic Planning left a charge slot set,
 * but Timed Charge is off, so the planned charge won't run. Shown in both the
 * orbit and house views; nothing when there is nothing to fix.
 */
export default function TimedChargeOffHint() {
  const snapshot = useInverterStore((s) => s.snapshot);
  const connected = useInverterStore((s) => s.connectionState === 'connected');
  // A stale snapshot while reconnecting says nothing reliable about Timed Charge.
  if (!connected || !plannerTimedChargeOff(snapshot)) return null;
  return (
    <section
      role="status"
      data-testid="timed-charge-off-hint"
      className="rounded-lg border border-amber-500/40 bg-amber-950/40 px-3 py-2 text-xs text-amber-100"
    >
      Timed Charge is off, so the planned charge won&apos;t run. Turn it on from the{' '}
      <PageLink to="/control">Control page</PageLink>.
    </section>
  );
}
