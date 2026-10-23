import { useEffect, useLayoutEffect, useState } from 'react';

/** How long after the last keystroke an edited field saves itself. */
export const DEFERRED_SAVE_DELAY_MS = 800;

type SaveFn = () => Promise<unknown> | unknown;

export interface DeferredSaver {
  /**
   * Record an edit. A valid value saves itself after a pause; an invalid one
   * (an emptied or half-typed field) waits for `flush`, so the user isn't
   * told off mid-edit.
   */
  edited: (valid: boolean) => void;
  /** Save a pending edit now — blur and Enter. No-op without an edit. */
  flush: () => void;
  /** Drop a pending edit — another save has already carried the value. */
  cancel: () => void;
}

/**
 * Saving for fields that used to save only on blur (issue #391): a value
 * changed without the field losing focus — spin arrows, Enter, leaving the
 * page — was never sent. An edit now saves after a pause, on `flush`, and
 * on unmount. Saves never overlap: an edit made while one is in flight
 * saves once it finishes, so an older value can't land after a newer one.
 *
 * `save` is always called in its latest form, so it reads the field's
 * current value; it validates and reports errors itself.
 */
export function useDeferredSave(save: SaveFn, delayMs = DEFERRED_SAVE_DELAY_MS): DeferredSaver {
  const [saver] = useState(() => createDeferredSaver(delayMs));
  useLayoutEffect(() => {
    saver.setSave(save);
  });
  useEffect(() => () => saver.flush(), [saver]);
  return saver;
}

function createDeferredSaver(delayMs: number): DeferredSaver & { setSave: (fn: SaveFn) => void } {
  let save: SaveFn = () => undefined;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let dirty = false;
  let inFlight = false;
  let queued = false;

  const clearTimer = () => {
    if (timer != null) clearTimeout(timer);
    timer = null;
  };

  // Called synchronously when idle, so a blur/Enter save starts in the
  // same event as the keystroke that triggered it.
  const run = () => {
    if (inFlight) {
      queued = true;
      return;
    }
    inFlight = true;
    let result: unknown;
    try {
      result = save();
    } catch {
      result = undefined;
    }
    void Promise.resolve(result)
      .catch(() => {})
      .finally(() => {
        inFlight = false;
        if (queued) {
          queued = false;
          run();
        }
      });
  };

  const flush = () => {
    clearTimer();
    if (!dirty) return;
    dirty = false;
    run();
  };

  return {
    edited: (valid) => {
      dirty = true;
      clearTimer();
      if (valid) timer = setTimeout(flush, delayMs);
    },
    flush,
    cancel: () => {
      clearTimer();
      dirty = false;
    },
    setSave: (fn) => {
      save = fn;
    },
  };
}
