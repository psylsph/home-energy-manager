import type { ConnectionState } from '../lib/types';
import { awaitingConnectionMessage, DONGLE_ADVICE_AFTER_FAILURES } from '../lib/awaitingConnection';
import { useReconnect } from '../hooks/useReconnect';
import { useDonglePage } from '../hooks/useDonglePage';
import { openExternal } from '../lib/openExternal';
import DongleLoginTip from './DongleLoginTip';

const FAQ_URL = 'https://github.com/psylsph/home-energy-manager/blob/master/FAQ.md';

type AwaitingConnectionProps = {
  /** Current poll-loop state — drives the message line. */
  connectionState: ConnectionState;
  /** Host the backend is trying to reach; shown stripped of its port. */
  connectedHost?: string | null;
  /** Render a "Retry Now" button that POSTs `/api/reconnect`. */
  showRetry?: boolean;
  /** Label for the standard retry button. */
  retryLabel?: string;
  /** Number of failed connection attempts used for the dongle advice banner. */
  connectFailures?: number;
  /** Extra page-specific line under the message (e.g. Control's "controls disabled" note). */
  extraNote?: string;
  /** Render the firewall / FAQ help paragraph used by Battery / Solar / Inverter. */
  showFaq?: boolean;
  /** Additional troubleshooting sentence for the FAQ paragraph. */
  faqExtraNote?: string;
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
  retryLabel = 'Retry Now',
  connectFailures = 0,
  extraNote,
  showFaq = false,
  faqExtraNote,
}: AwaitingConnectionProps) {
  const { reconnect, reconnecting } = useReconnect();
  const showFailureAdvice = connectionState === 'disconnected' && connectFailures >= DONGLE_ADVICE_AFTER_FAILURES;

  return (
    <div className="flex flex-col items-center justify-center min-h-[60vh] gap-4">
      <div className="w-10 h-10 border-4 border-accent border-t-transparent rounded-full animate-spin" />
      <p className="text-text-secondary text-sm font-sans">
        {awaitingConnectionMessage(connectionState)}
      </p>

      {connectedHost && (
        <p className="text-text-secondary/60 text-xs font-sans">
          Host: {connectedHost.replace(/:.*$/, '')}
        </p>
      )}

      {showFailureAdvice && (
        <DongleFailureAdvice
          connectFailures={connectFailures}
          reconnect={reconnect}
          reconnecting={reconnecting}
        />
      )}

      {showRetry && (
        <button
          onClick={reconnect}
          disabled={reconnecting}
          className="px-4 py-1.5 text-xs font-semibold rounded-lg bg-bg-surface hover:bg-white/10 border border-white/10 transition-colors disabled:opacity-50"
        >
          {reconnecting ? 'Reconnecting…' : retryLabel}
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
          {faqExtraNote ? ` ${faqExtraNote}` : ''}
          See the{' '}
          <a
            href={FAQ_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="text-accent hover:underline"
          >
            FAQ
          </a>{' '}
          for help.
        </p>
      )}
    </div>
  );
}

/**
 * The amber "can't reach the dongle" card. Mounted only while it shows, so the
 * dongle-page probe (issue #452) runs only then: once on appearing, again
 * after Retry Now. The dongle's own restart is gentler than an isolator
 * power-cycle, so it comes first when its page answers.
 */
function DongleFailureAdvice({
  connectFailures,
  reconnect,
  reconnecting,
}: {
  connectFailures: number;
  reconnect: () => void;
  reconnecting: boolean;
}) {
  const { url, loading, recheck } = useDonglePage();
  const isolator = (
    <>(AC isolator off for 30&nbsp;seconds, then on); it should reconnect within a few minutes.</>
  );

  return (
    <div className="rounded-2xl border border-amber-500/40 bg-amber-950/30 px-5 py-4 text-amber-100 shadow-lg max-w-md">
      <div className="flex items-start gap-3">
        <span className="text-2xl" aria-hidden="true">💡</span>
        <div className="flex flex-col gap-2">
          <p className="text-sm font-semibold">
            Can't reach the dongle after {Math.min(connectFailures, 99)}+ attempts
          </p>
          {url ? (
            <>
              <p className="text-xs text-amber-100/80 leading-relaxed">
                The dongle has probably locked up. Restart it from its web page: Device Management,
                then Restart.
              </p>
              <div className="flex items-center gap-2">
                <button
                  onClick={() => void openExternal(url)}
                  className="px-4 py-1.5 text-xs font-semibold rounded-lg bg-amber-600/30 hover:bg-amber-600/50 border border-amber-500/30 transition-colors"
                >
                  Open Dongle Page
                </button>
                <DongleLoginTip />
              </div>
              <p className="text-xs text-amber-100/80 leading-relaxed">
                If that doesn't help, <strong>power-cycle the inverter</strong> {isolator}
              </p>
            </>
          ) : loading ? (
            // Don't advise the isolator before the gentler restart has been ruled out.
            <p className="text-xs text-amber-100/80 leading-relaxed">
              The dongle has probably locked up. Checking whether its web page answers…
            </p>
          ) : (
            <p className="text-xs text-amber-100/80 leading-relaxed">
              The dongle has probably locked up, and its own web page isn&apos;t answering either.{' '}
              <strong>Power-cycle the inverter</strong> {isolator}
            </p>
          )}
          <button
            onClick={() => {
              reconnect();
              recheck();
            }}
            disabled={reconnecting}
            className="self-start mt-1 px-4 py-1.5 text-xs font-semibold rounded-lg bg-amber-600/30 hover:bg-amber-600/50 border border-amber-500/30 transition-colors disabled:opacity-50"
          >
            {reconnecting ? 'Reconnecting…' : 'Retry Now'}
          </button>
        </div>
      </div>
    </div>
  );
}
