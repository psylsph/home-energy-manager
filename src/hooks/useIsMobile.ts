import { useSyncExternalStore } from 'react';

/** Viewports at or below this pixel width are treated as mobile. */
const MOBILE_QUERY = '(max-width: 767px)';

function subscribe(callback: () => void): () => void {
  const mql = window.matchMedia(MOBILE_QUERY);
  mql.addEventListener('change', callback);
  return () => mql.removeEventListener('change', callback);
}

function getSnapshot(): boolean {
  return window.matchMedia(MOBILE_QUERY).matches;
}

function getServerSnapshot(): boolean {
  return false;
}

/**
 * Returns `true` when the viewport is below the Tailwind `md` breakpoint
 * (768px). Used by the energy-flow diagram to switch to larger symbols and
 * fonts so it stays legible when the SVG scales down on phones — desktop
 * keeps the standard (smaller) sizing.
 */
export function useIsMobile(): boolean {
  return useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);
}
