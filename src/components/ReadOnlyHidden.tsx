import type { ReactNode } from 'react';
import { useIsReadOnly } from '../hooks/useIsReadOnly';

/**
 * Hides controls that change something (a write to the inverter or a saved
 * setting) on a `?RO` read-only link (issues #114, #471), so a page can stay
 * visible for what it shows while its buttons and inputs drop out. Wrap the
 * control, not the page: new controls on a shared page then only need wrapping
 * to stay out of read-only links.
 *
 * A convenience for family dashboards, not a security boundary — the API is
 * unchanged.
 */
export default function ReadOnlyHidden({ children }: { children: ReactNode }) {
  const readOnly = useIsReadOnly();
  return readOnly ? null : <>{children}</>;
}
