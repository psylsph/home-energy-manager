import { agileBlockedMessage } from '../lib/agileBlocked';

interface Props {
  blockedBy: string | null | undefined;
  enabled: boolean | undefined;
}

/** Warns that Agile is enabled but something is stopping it from acting. */
export default function AgileBlockedNotice({ blockedBy, enabled }: Props) {
  const message = agileBlockedMessage(blockedBy, enabled);
  if (!message) return null;
  return (
    <div
      role="status"
      className="rounded-xl border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-300"
    >
      {message}
    </div>
  );
}
