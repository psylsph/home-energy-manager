import { Link } from 'react-router';
import { useInverterStore } from '../store/useInverterStore';
import { COSY_BLOCK_REASONS } from '../lib/cosyBlocked';

/**
 * App-wide banner for a Cosy slot that is running, or about to start, while
 * something is stopping HEM from setting its charge times (issue #366).
 * Between slots the on-page notice is enough; this one is reserved for a
 * charge the user is relying on right now, so it is visible on every page.
 */
export default function CosySlotBlockedBanner() {
  const snapshot = useInverterStore((state) => state.snapshot);
  if (!snapshot?.cosy_enabled || !snapshot.cosy_slot_at_risk || !snapshot.cosy_blocked_by) {
    return null;
  }
  const reason = COSY_BLOCK_REASONS[snapshot.cosy_blocked_by];

  return (
    <div
      role="alert"
      className="bg-amber-950/90 border-b border-amber-500/50 px-4 py-2 text-amber-100 text-sm"
    >
      <div className="max-w-4xl mx-auto flex flex-wrap items-center gap-x-2 gap-y-1">
        <span aria-hidden="true">⚠️</span>
        <strong>Cosy charging is being held back</strong>
        <span className="text-amber-100/85">
          {reason ? `${reason}.` : 'Something is holding the inverter.'} It will resume by itself
          once this clears.
        </span>
        <Link to="/control" className="underline underline-offset-2 hover:text-white">
          Open Control
        </Link>
      </div>
    </div>
  );
}
