import { lazy } from 'react';
import type { ComponentType } from 'react';

// Pages load on demand so the first screen doesn't wait for every chart and
// form in the app. The catch: a tab left open across an upgrade still holds
// the old index, whose page chunks (hashed file names) no longer exist on the
// server. When a page fails to load, reload once to pick up the new build;
// if it fails again straight after, let the error reach the page's
// ErrorBoundary instead of reloading in a loop.

export const CHUNK_RELOAD_KEY = 'chunkReloadAt';
export const CHUNK_RELOAD_WINDOW_MS = 10_000;

export interface ReloadDeps {
  storage: Pick<Storage, 'getItem' | 'setItem'>;
  reload: () => void;
  now: () => number;
}

const browserDeps = (): ReloadDeps => ({
  storage: window.sessionStorage,
  reload: () => window.location.reload(),
  now: () => Date.now(),
});

export async function loadOrReload<T>(load: () => Promise<T>, deps: ReloadDeps = browserDeps()): Promise<T> {
  try {
    return await load();
  } catch (err) {
    let last: number | null;
    try {
      const raw = deps.storage.getItem(CHUNK_RELOAD_KEY);
      last = raw === null ? null : Number(raw);
    } catch {
      // Storage unavailable: without a record we can't prevent a loop.
      throw err;
    }
    // abs(): a clock that jumped backwards mustn't block reloads for ever.
    if (last !== null && Number.isFinite(last) && Math.abs(deps.now() - last) < CHUNK_RELOAD_WINDOW_MS) {
      throw err;
    }
    try {
      deps.storage.setItem(CHUNK_RELOAD_KEY, String(deps.now()));
    } catch {
      throw err;
    }
    deps.reload();
    // The page is going away; never settle so nothing renders an error first.
    return new Promise<T>(() => {});
  }
}

export function lazyPage(load: () => Promise<{ default: ComponentType }>) {
  return lazy(() => loadOrReload(load));
}
