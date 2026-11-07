import type { ReactNode } from 'react';
import { Link, useInRouterContext } from 'react-router';

/**
 * A link to another app page, or its plain text when rendered outside a
 * router (pages mounted alone in tests).
 */
export default function PageLink({ to, children }: { to: string; children: ReactNode }) {
  if (!useInRouterContext()) return <>{children}</>;
  return (
    <Link to={to} className="underline underline-offset-2 hover:text-white">
      {children}
    </Link>
  );
}
