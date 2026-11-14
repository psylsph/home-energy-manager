import { addressIsReadOnly } from '../lib/readOnly';
import { useInverterStore } from '../store/useInverterStore';

/**
 * Read-only mode as App's nav sees it: the store flag, or a `?RO` address
 * App's effect hasn't copied into the store yet. Without the address check,
 * controls would show as live for the first render of a read-only link.
 */
export function useIsReadOnly(): boolean {
  const readOnly = useInverterStore((s) => s.readOnly);
  if (readOnly) return true;
  if (typeof window === 'undefined') return false;
  return addressIsReadOnly(window.location.search, window.location.hash);
}
