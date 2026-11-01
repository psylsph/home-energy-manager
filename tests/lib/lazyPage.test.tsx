import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { Suspense } from 'react';
import {
  CHUNK_RELOAD_KEY,
  CHUNK_RELOAD_WINDOW_MS,
  lazyPage,
  loadOrReload,
  type ReloadDeps,
} from '../../src/lib/lazyPage';

function memoryDeps(now: number, stored: string | null = null) {
  const store = new Map<string, string>();
  if (stored !== null) store.set(CHUNK_RELOAD_KEY, stored);
  const deps: ReloadDeps = {
    storage: {
      getItem: (k) => store.get(k) ?? null,
      setItem: (k, v) => { store.set(k, v); },
    },
    reload: vi.fn(),
    now: () => now,
  };
  return { deps, store };
}

const chunkError = new TypeError('Failed to fetch dynamically imported module');
const failing = () => Promise.reject(chunkError);

/** Resolves true when `p` has settled within a few microtask turns. */
async function settles(p: Promise<unknown>): Promise<boolean> {
  let done = false;
  p.then(() => { done = true; }, () => { done = true; });
  for (let i = 0; i < 10; i++) await Promise.resolve();
  return done;
}

describe('loadOrReload', () => {
  it('returns the module when it loads', async () => {
    const { deps } = memoryDeps(1_000_000);
    await expect(loadOrReload(() => Promise.resolve('page'), deps)).resolves.toBe('page');
    expect(deps.reload).not.toHaveBeenCalled();
  });

  it('reloads once and records the time when a chunk fails to load', async () => {
    const { deps, store } = memoryDeps(1_000_000);
    const pending = loadOrReload(failing, deps);
    expect(await settles(pending)).toBe(false); // never renders an error before the reload
    expect(deps.reload).toHaveBeenCalledTimes(1);
    expect(store.get(CHUNK_RELOAD_KEY)).toBe('1000000');
  });

  it('does not reload again within the window, so a broken build cannot loop', async () => {
    const now = 1_000_000;
    const { deps } = memoryDeps(now, String(now - CHUNK_RELOAD_WINDOW_MS + 1));
    await expect(loadOrReload(failing, deps)).rejects.toBe(chunkError);
    expect(deps.reload).not.toHaveBeenCalled();
  });

  it('reloads again once the window has passed', async () => {
    const now = 1_000_000;
    const { deps } = memoryDeps(now, String(now - CHUNK_RELOAD_WINDOW_MS));
    void loadOrReload(failing, deps);
    await settles(Promise.resolve());
    expect(deps.reload).toHaveBeenCalledTimes(1);
  });

  it('reloads after the clock jumps far backwards rather than blocking for ever', async () => {
    const now = 1_000_000;
    const { deps } = memoryDeps(now, String(now + 60 * 60 * 1000));
    void loadOrReload(failing, deps);
    await settles(Promise.resolve());
    expect(deps.reload).toHaveBeenCalledTimes(1);
  });

  it('treats a garbled record as no record', async () => {
    const { deps } = memoryDeps(1_000_000, 'not-a-number');
    void loadOrReload(failing, deps);
    await settles(Promise.resolve());
    expect(deps.reload).toHaveBeenCalledTimes(1);
  });

  it('surfaces the error instead of reloading when storage cannot be read', async () => {
    const { deps } = memoryDeps(1_000_000);
    deps.storage.getItem = () => { throw new Error('SecurityError'); };
    await expect(loadOrReload(failing, deps)).rejects.toBe(chunkError);
    expect(deps.reload).not.toHaveBeenCalled();
  });

  it('surfaces the error instead of reloading when storage cannot be written', async () => {
    const { deps } = memoryDeps(1_000_000);
    deps.storage.setItem = () => { throw new Error('QuotaExceededError'); };
    await expect(loadOrReload(failing, deps)).rejects.toBe(chunkError);
    expect(deps.reload).not.toHaveBeenCalled();
  });

  it('uses session storage and the real clock by default', async () => {
    window.sessionStorage.setItem(CHUNK_RELOAD_KEY, String(Date.now()));
    await expect(loadOrReload(failing)).rejects.toBe(chunkError);
    window.sessionStorage.removeItem(CHUNK_RELOAD_KEY);
  });
});

describe('lazyPage', () => {
  afterEach(cleanup);

  it('renders the page once its chunk loads', async () => {
    const Page = lazyPage(() => Promise.resolve({ default: () => <p>Lazy page</p> }));
    render(<Suspense fallback={<p>Loading…</p>}><Page /></Suspense>);
    expect(screen.getByText('Loading…')).toBeDefined();
    expect(await screen.findByText('Lazy page')).toBeDefined();
  });
});
