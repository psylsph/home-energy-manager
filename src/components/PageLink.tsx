import type { ReactNode } from 'react';
import { Link, useInRouterContext } from 'react-router';
import { hiddenInReadOnly } from '../lib/navItems';
import { useIsReadOnly } from '../hooks/useIsReadOnly';

/**
 * A link to another app page, or its plain text when rendered outside a
 * router (pages mounted alone in tests), or when a `?RO` read-only link
 * can't reach the page (Control, Settings, Octopus; issue #471) — the text
 * stays, so a sentence like "…from the Control page" still reads.
 */
export default function PageLink({
  to,
  children,
  className = 'underline underline-offset-2 hover:text-white',
}: {
  to: string;
  children: ReactNode;
  className?: string;
}) {
  const inRouter = useInRouterContext();
  const unreachable = useIsReadOnly() && hiddenInReadOnly(to);
  if (!inRouter || unreachable) return <>{children}</>;
  return (
    <Link to={to} className={className}>
      {children}
    </Link>
  );
}
