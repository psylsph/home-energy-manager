import { cosyBlockedMessage } from '../lib/cosyBlocked';

interface Props {
  blockedBy: string | null | undefined;
  enabled: boolean | undefined;
}

/** Warns that Cosy is enabled but something is stopping it from running. */
export default function CosyBlockedNotice({ blockedBy, enabled }: Props) {
  const message = cosyBlockedMessage(blockedBy, enabled);
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
