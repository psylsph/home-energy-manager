import { useInverterStore } from '../store/useInverterStore';
import type { InverterSnapshot } from './types';

/**
 * How a wait for the inverter to confirm a write ended: a newer snapshot showed
 * the value, the timeout passed first, or the caller went away (the page was
 * closed) and no longer wants the answer.
 */
export type ReadbackResult = 'confirmed' | 'timeout' | 'aborted';

/**
 * Control writes are queued by the API and then applied register by register by
 * the poll loop, so a POST that returns means "queued", not "applied". Resolve
 * once a snapshot NEWER than `snapshotBeforeSave` satisfies `predicate`, which
 * is the only proof the inverter took the value. The snapshot the save started
 * from never counts, even if it already matches: it predates the write.
 *
 * Settles exactly once and always releases its store subscription and timer,
 * including when `signal` aborts, so a closed page leaves nothing behind.
 */
export function waitForSnapshotReadback(
  predicate: (snapshot: InverterSnapshot) => boolean,
  snapshotBeforeSave: InverterSnapshot | null,
  timeoutMs: number,
  signal?: AbortSignal,
): Promise<ReadbackResult> {
  return new Promise((resolve) => {
    if (signal?.aborted) {
      resolve('aborted');
      return;
    }
    let settled = false;
    let unsubscribe = () => {};
    const finish = (result: ReadbackResult) => {
      if (settled) return;
      settled = true;
      window.clearTimeout(timeout);
      unsubscribe();
      signal?.removeEventListener('abort', onAbort);
      resolve(result);
    };
    const onAbort = () => finish('aborted');
    const check = () => {
      const current = useInverterStore.getState().snapshot;
      if (current && current !== snapshotBeforeSave && predicate(current)) {
        finish('confirmed');
      }
    };
    const timeout = window.setTimeout(() => finish('timeout'), timeoutMs);
    signal?.addEventListener('abort', onAbort);
    unsubscribe = useInverterStore.subscribe(check);
    check();
  });
}
