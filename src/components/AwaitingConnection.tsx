import { useState, useCallback } from 'react';
import type { ConnectionState } from '../lib/types';
import { apiPost } from '../lib/api';
import { awaitingConnectionMessage } from '../lib/awaitingConnection';

const FAQ_URL = 'https://github.com/psylsph/home-energy-manager/blob/master/FAQ.md';

type AwaitingConnectionProps = {
  /** Current poll-loop state — drives the message line. */
  connectionState: ConnectionState;
  /** Host the backend is trying to reach; shown stripped of its port. */
  connectedHost?: string | null;
  /** Render a "Retry now" button that POSTs `/api/reconnect`. */
  showRetry?: boolean;
  /** Extra page-specific line under the message (e.g. Control's "controls disabled" note). */
  extraNote?: string;
  /** Render the firewall / FAQ help paragraph used by Battery / Solar / Inverter. */
  showFaq?: boolean;
};

/**
 * Full-screen placeholder shown while the backend has no usable connection
 * to the inverter. Replaces the copy-pasted spinner blocks that used to live
 * inline in StatusPage / BatteryPage / SolarPage / InverterPage / ControlPage
 * — they had drifted apart in both wording and gating. Centralising them
 * here keeps the alignment permanent.
 */
export default function AwaitingConnection({
  connectionState,
  connectedHost,
  showRetry = false,
  extraNote,
  showFaq = false,
}: AwaitingConnectionProps) {
  const [retrying, setRetrying] = useState(false);
  const handleRetry = useCallback(async () => {
    setRetrying(true);
    try {
      await apiPost('/api/reconnect');
    } catch {
      // Swallow — the poll loop's own back-off retries regardless of
      // whether this manual poke landed.
    }
    // Reset after a few seconds in case the request doesn't trigger a
    // state change the WS hook would react to.
    setTimeout(() => setRetrying(false), 5000);
  }, []);

  return (
    <div className="flex flex-col items-center justify-center min-h-[60vh] gap-4">
      <div className="w-10 h-10 border-4 border-flow-active border-t-transparent rounded-full animate-spin" />
      <p className="text-text-secondary text-sm font-sans">
        {awaitingConnectionMessage(connectionState)}
      </p>

      {connectedHost && (
        <p className="text-text-secondary/60 text-xs font-sans">
          Host: {connectedHost.replace(/:.*$/, '')}
        </p>
      )}

      {showRetry && (
        <button
          onClick={handleRetry}
          disabled={retrying}
          className="px-4 py-1.5 text-xs font-semibold rounded-lg bg-bg-surface hover:bg-white/10 border border-white/10 transition-colors disabled:opacity-50"
        >
          {retrying ? 'Reconnecting…' : 'Retry now'}
        </button>
      )}

      {extraNote && (
        <p className="text-text-secondary/60 text-xs font-sans text-center max-w-xs">
          {extraNote}
        </p>
      )}

      {showFaq && (
        <p className="text-text-secondary/60 text-xs font-sans text-center max-w-xs">
          If data doesn't appear, try restarting the app and check your firewall settings.
          See the{' '}
          <a
            href={FAQ_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="text-flow-active hover:underline"
          >
            FAQ
          </a>{' '}
          for help.
        </p>
      )}
    </div>
  );
}
