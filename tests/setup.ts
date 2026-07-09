/**
 * Vitest global setup. Runs once per test file, before any test imports the
 * code under test.
 *
 * Why this exists
 * ---------------
 * The current vitest + jsdom + node-25 combination ships `localStorage` as a
 * bare object stub — it has the right identity but `getItem` / `setItem` /
 * `removeItem` / `clear` are not functions. Code under test that touches
 * `localStorage` (or `window.localStorage`) — e.g. SettingsPage's saved-host
 * lookup, the inverter store's persistence layer, ControlPage's
 * `forceDurationMinutes` reader — then throws `localStorage.X is not a
 * function` on first access and the test fails for the wrong reason.
 *
 * We replace both the bare `localStorage` global and `window.localStorage`
 * with a real `Storage`-shaped object backed by an in-memory `Map`, so
 * `localStorage.getItem(...)` / `setItem(...)` / `removeItem(...)` /
 * `clear()` all work.
 *
 * Compatibility with `Object.keys(localStorage)`
 * ------------------------------------------------
 * Real browser / jsdom `localStorage` (in spec-violating but practical
 * fashion) also exposes stored keys as own enumerable properties of the
 * storage object — e.g. `setItem('a', '1')` makes `'a'` appear in
 * `Object.keys(localStorage)`. Several tests in this project rely on that
 * (e.g. `useInverterStore.gridLineWeight.test.ts` snapshots which keys
 * the store writes). We mirror that behaviour by reflecting the Map entries
 * through a `Proxy` that returns the stored values for known keys and
 * `undefined` otherwise. This keeps both the standard `Storage` API and the
 * de-facto `Object.keys` enumeration working.
 *
 * State isolation
 * ---------------
 * Every fresh `Storage` starts empty, and the `beforeEach` hook below clears
 * it so persistence tests start from a known baseline.
 */

import { beforeEach } from 'vitest';

function createMemoryStorage(): Storage {
  const store = new Map<string, string>();
  const storage = {
    get length() {
      return store.size;
    },
    clear() {
      store.clear();
    },
    getItem(key: string) {
      return store.has(key) ? store.get(key)! : null;
    },
    key(index: number) {
      return Array.from(store.keys())[index] ?? null;
    },
    removeItem(key: string) {
      store.delete(key);
    },
    setItem(key: string, value: string) {
      store.set(key, String(value));
    },
  };
  // Wrap in a Proxy that reflects stored keys as own properties. This is
  // the spec-violating-but-practical behaviour of real browser localStorage
  // that several tests rely on. `Object.keys` / spread / `for…in` all see
  // the stored keys; standard `getItem` / `setItem` still work; unknown
  // keys return `undefined` (no leak of internal Map methods).
  return new Proxy(storage, {
    has(target, prop) {
      if (typeof prop === 'string' && store.has(prop)) return true;
      return Reflect.has(target, prop);
    },
    get(target, prop, receiver) {
      if (typeof prop === 'string' && store.has(prop)) {
        return store.get(prop);
      }
      return Reflect.get(target, prop, receiver);
    },
    ownKeys() {
      return Array.from(store.keys());
    },
    getOwnPropertyDescriptor(target, prop) {
      if (typeof prop === 'string' && store.has(prop)) {
        return {
          configurable: true,
          enumerable: true,
          value: store.get(prop),
          writable: true,
        };
      }
      return Reflect.getOwnPropertyDescriptor(target, prop);
    },
  }) as Storage;
}

const localStorageShim = createMemoryStorage();
const sessionStorageShim = createMemoryStorage();

// Replace the bare globals. We also need `window.localStorage` to be the
// same object (production code reads `window.localStorage.foo`, not just
// the bare `localStorage` global). jsdom exposes `window` as a getter on
// globalThis, so defineProperty is the safe way to overwrite its
// `localStorage` property without losing the rest of the window.
(globalThis as { localStorage: Storage }).localStorage = localStorageShim;
(globalThis as { sessionStorage: Storage }).sessionStorage = sessionStorageShim;

try {
  Object.defineProperty(window, 'localStorage', {
    configurable: true,
    get: () => localStorageShim,
  });
} catch {
  // jsdom 29 freezes this in some configs; the globalThis patch above is
  // enough on its own.
}
try {
  Object.defineProperty(window, 'sessionStorage', {
    configurable: true,
    get: () => sessionStorageShim,
  });
} catch {
  // Same as above.
}

// Reset state between tests so persistence tests start from a known baseline.
beforeEach(() => {
  localStorageShim.clear();
  sessionStorageShim.clear();
});

