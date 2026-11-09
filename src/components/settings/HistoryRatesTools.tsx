import { useCallback, useEffect, useState } from 'react';
import { apiGet, apiPost } from '../../lib/api';
import {
  isRepriceDateValid,
  progressText,
  resultText,
  todayIso,
  type RatesJobResponse,
  type RatesJobStatus,
} from '../../lib/ratesJob';
import { SettingsGroup } from './SettingsSection';

const POLL_MS = 2000;
const BUTTON =
  'bg-bg-elevated text-text-primary font-sans font-semibold text-sm px-4 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 disabled:cursor-not-allowed transition-opacity';
const DANGER_BUTTON =
  'bg-accent text-on-accent font-sans font-semibold text-sm px-4 py-2 rounded-lg hover:opacity-90 disabled:opacity-40 disabled:cursor-not-allowed transition-opacity';

/**
 * Re-price History from a date with the tariffs as last saved, and (with an
 * Octopus account) fill the rates rows lack from what Octopus charged
 * (issue #455). History rows keep the rates they were recorded with, so this
 * is the only way to change the past.
 */
export default function HistoryRatesTools({ octopusConfigured }: { octopusConfigured: boolean }) {
  const [status, setStatus] = useState<RatesJobStatus | null>(null);
  const [from, setFrom] = useState('');
  const [confirming, setConfirming] = useState(false);
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const res = await apiGet<RatesJobResponse>('/api/history/reprice');
      setStatus(res.data);
    } catch {
      // The status is a convenience; the buttons still work without it.
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const res = await apiGet<RatesJobResponse>('/api/history/reprice');
        if (!cancelled) setStatus(res.data);
      } catch {
        // Not shown until a job is started; the buttons work without it.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Follow a job until it ends, so the progress and result don't go stale.
  const running = status?.running === true;
  useEffect(() => {
    if (!running) return;
    const timer = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(timer);
  }, [running, refresh]);

  const start = async (path: string, body: unknown, failure: string) => {
    setStarting(true);
    setError(null);
    try {
      await apiPost(path, body);
    } catch (e) {
      setError(e instanceof Error ? e.message : failure);
    } finally {
      setConfirming(false);
      await refresh();
      setStarting(false);
    }
  };

  const today = todayIso();
  const dateOk = isRepriceDateValid(from, today);
  const busy = starting || running;

  return (
    <SettingsGroup
      title="Fix Past Costs"
      description="Past History costs keep the rates they were recorded with."
      help="Saving a tariff changes costs from now on. To change earlier costs, re-price History from a date: every row from that date takes the rates you last saved. This can't be undone. With an Octopus account, you can also fill rows that have no recorded rate with the rates Octopus charged at the time."
    >
      <div className="flex flex-wrap items-end gap-3">
        <label className="flex flex-col gap-1">
          <span className="text-text-primary text-sm font-sans">Re-Price from</span>
          <input
            type="date"
            value={from}
            max={today}
            onChange={(e) => {
              setFrom(e.target.value);
              setConfirming(false);
            }}
            disabled={busy}
            aria-label="Re-Price from Date"
            className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-accent outline-none transition-colors"
          />
        </label>
        <button
          type="button"
          className={BUTTON}
          disabled={busy || !dateOk || confirming}
          onClick={() => setConfirming(true)}
        >
          Re-Price History
        </button>
      </div>
      <p className="text-text-secondary text-xs font-sans">Uses the tariffs as last saved, so save them first.</p>

      {confirming && dateOk && (
        <div role="alert" className="border border-white/10 rounded-xl p-3 flex flex-col gap-2">
          <p className="text-text-primary text-sm font-sans">
            Every History row from {from} (in the time zone of the computer running HEM) will take the
            tariffs as last saved. Costs from that date will change, and the rates now recorded
            can&apos;t be restored.
          </p>
          <div className="flex gap-3">
            <button
              type="button"
              className={DANGER_BUTTON}
              disabled={busy}
              onClick={() => void start('/api/history/reprice', { from }, 'Could not start re-pricing')}
            >
              Confirm Re-Price
            </button>
            <button type="button" className={BUTTON} onClick={() => setConfirming(false)}>
              Cancel
            </button>
          </div>
        </div>
      )}

      {octopusConfigured && (
        <div className="flex flex-col gap-1">
          <button
            type="button"
            className={`${BUTTON} self-start`}
            disabled={busy}
            onClick={() => void start('/api/octopus/backfill-rates', undefined, 'Could not start filling rates')}
          >
            Fill Missing Rates from Octopus
          </button>
          <span className="text-text-secondary text-xs font-sans">
            Fills rows with no recorded rate. Rates already recorded are never changed.
          </span>
        </div>
      )}

      {running && status && (
        <p role="status" className="text-text-primary text-sm font-sans">
          {progressText(status)}
        </p>
      )}
      {error && (
        <p role="alert" className="text-red-400 text-sm font-sans">
          {error}
        </p>
      )}
      {!running && status?.last && (
        <p className={`text-xs font-sans ${status.last.ok ? 'text-text-secondary' : 'text-red-400'}`}>
          {resultText(status.last)}
        </p>
      )}
    </SettingsGroup>
  );
}
