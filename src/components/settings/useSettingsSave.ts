import { useCallback, useState } from 'react';

/**
 * The Settings page toast. `sticky` keeps the message up until the next one
 * (used for a newly generated API key, which the user must read).
 */
export type Flash = (text: string, ok: boolean, opts?: { sticky?: boolean }) => void;

/** An error's message, or `fallback` when it has none. */
export function errorText(e: unknown, fallback: string): string {
  return e instanceof Error && e.message ? e.message : fallback;
}

/**
 * The save plumbing every Settings section repeats: a busy flag for the
 * button, the request, then a toast either way.
 *
 * `run(action, fail)` sets `busy`, awaits `action`, and flashes the string
 * it resolves to (nothing when it resolves to nothing — the action flashed
 * for itself). A throw flashes the error's message, or `fail` when it has
 * none; pass a function to word the failure yourself. `busy` clears either
 * way. `sticky` keeps the toast up until the next one.
 */
export function useSettingsSave(flash: Flash) {
  const [busy, setBusy] = useState(false);
  const run = useCallback(
    async (
      action: () => Promise<string | void>,
      fail: string | ((e: unknown) => string),
      opts?: { sticky?: boolean },
    ) => {
      const show = (text: string, ok: boolean) => (opts ? flash(text, ok, opts) : flash(text, ok));
      setBusy(true);
      try {
        const ok = await action();
        if (ok) show(ok, true);
      } catch (e) {
        show(typeof fail === 'function' ? fail(e) : errorText(e, fail), false);
      } finally {
        setBusy(false);
      }
    },
    [flash],
  );
  return { busy, run };
}

/**
 * A boolean setting that saves the moment it's flipped. The switch moves at
 * once; if the save fails it moves back, so it never shows a state that
 * didn't persist, and the toast says why.
 */
export function useSettingToggle(
  initial: boolean,
  persist: (next: boolean) => Promise<unknown>,
  flash: Flash,
  text: { on: string; off: string; fail: string },
): [boolean, (next: boolean) => Promise<void>] {
  const [value, setValue] = useState(initial);
  const set = useCallback(
    async (next: boolean) => {
      const previous = value;
      setValue(next);
      try {
        await persist(next);
        flash(next ? text.on : text.off, true);
      } catch (e) {
        setValue(previous);
        flash(`${text.fail}: ${e instanceof Error ? e.message : String(e)}`, false);
      }
    },
    [value, persist, flash, text.on, text.off, text.fail],
  );
  return [value, set];
}
