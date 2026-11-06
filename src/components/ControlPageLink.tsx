import { Link, useInRouterContext } from 'react-router';

/**
 * "Control page" as a link to `/control`, or plain text when the component is
 * rendered outside a router (pages mounted alone in tests).
 */
export default function ControlPageLink({ children = 'Control page' }: { children?: string }) {
  if (!useInRouterContext()) return <>{children}</>;
  return (
    <Link to="/control" className="underline underline-offset-2 hover:text-white">
      {children}
    </Link>
  );
}
