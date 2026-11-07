import { useCallback, useEffect, useState } from 'react';
import { apiGet } from '../lib/api';

export interface DonglePageState {
  /** The dongle web page that answered, or `null` when none did (or the backend couldn't be asked). */
  url: string | null;
  /** True until the first probe has settled; a re-check keeps the last answer on show. */
  loading: boolean;
  /** True while a re-check is out, so a button can show it was pressed. */
  checking: boolean;
  /** Probe again, e.g. after Retry Now. */
  recheck: () => void;
}

/**
 * Asks the backend which of the WiFi dongle's own web pages answers
 * (`GET /api/dongle-page`, issue #452). Probes once when mounted and again on
 * `recheck()`, never on a timer, so mount it only where the answer is wanted.
 * A failed request is the same as no answer.
 */
export function useDonglePage(): DonglePageState {
  const [attempt, setAttempt] = useState(0);
  // `undefined` until the first probe settles; `null` is "nothing answered".
  const [url, setUrl] = useState<string | null | undefined>(undefined);
  const [checking, setChecking] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      let found: string | null = null;
      try {
        const res = await apiGet<{ ok: boolean; url?: string | null }>('/api/dongle-page');
        if (res.ok && typeof res.url === 'string') found = res.url;
      } catch {
        found = null;
      }
      if (!cancelled) {
        setUrl(found);
        setChecking(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [attempt]);

  const recheck = useCallback(() => {
    setChecking(true);
    setAttempt((n) => n + 1);
  }, []);
  return { url: url ?? null, loading: url === undefined, checking, recheck };
}
