import { useDonglePage } from '../hooks/useDonglePage';
import { openExternal } from '../lib/openExternal';
import DongleLoginTip from './DongleLoginTip';

/**
 * Developer Controls (issue #452): open the WiFi dongle's own web page, where
 * its Device Management menu can restart just the dongle. Reachable even when
 * the connection-problem banner isn't showing, e.g. when the dongle answers
 * Modbus but needs a restart. Asks the backend which page answers, so a dead
 * link is never offered.
 */
export default function DongleWebPageSection() {
  const { url, loading, checking, recheck } = useDonglePage();

  return (
    <div className="space-y-3">
      <h3 className="text-text-primary font-medium text-base">Dongle Web Page</h3>
      <div className="bg-bg-surface rounded-xl p-3 space-y-2">
        <p className="text-text-secondary text-xs">
          Opens the dongle&apos;s own web page; the ⓘ has the login. <DongleLoginTip />
        </p>
        {url ? (
          <button
            onClick={() => void openExternal(url)}
            className="w-full py-2 bg-accent/20 text-text-primary rounded-lg text-xs font-medium hover:bg-accent/30 transition border border-accent/30"
          >
            Open Dongle Page
          </button>
        ) : loading ? (
          <p className="text-text-secondary text-xs" role="status">Checking for the dongle&apos;s web page…</p>
        ) : (
          <>
            <p className="text-text-secondary text-xs" role="status">
              The dongle&apos;s web page isn&apos;t answering. If it is stuck, switch the inverter&apos;s
              AC isolator off for 30&nbsp;seconds, then on.
            </p>
            <button
              onClick={recheck}
              disabled={checking}
              className="w-full py-2 bg-bg-elevated text-text-primary rounded-lg text-xs font-medium hover:bg-white/10 transition border border-white/10 disabled:opacity-50"
            >
              {checking ? 'Checking…' : 'Check Again'}
            </button>
          </>
        )}
      </div>
    </div>
  );
}
