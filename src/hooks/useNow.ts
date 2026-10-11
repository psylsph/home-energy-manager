import { useEffect, useRef, useState } from 'react';

/**
 * Current time in ms, refreshed every `intervalMs`, so something on screen
 * ages without a new snapshot. Pass `null` to run no clock at all. The clock
 * read stays in an effect, never in render.
 */
export function useNow(intervalMs: number | null): number {
  const [now, setNow] = useState(() => Date.now());
  const paused = useRef(false);
  useEffect(() => {
    if (intervalMs == null) {
      paused.current = true;
      return;
    }
    const tick = () => setNow(Date.now());
    // Resuming after a pause: the value may date from before the clock was
    // switched off, and the first interval tick could be a whole interval
    // away, so catch up now. A fresh mount does not need it.
    const catchUp = paused.current ? setTimeout(tick, 0) : null;
    paused.current = false;
    const id = setInterval(tick, intervalMs);
    return () => {
      if (catchUp != null) clearTimeout(catchUp);
      clearInterval(id);
    };
  }, [intervalMs]);
  return now;
}
