import type { ConnectionState } from './types';

/**
 * The single source of truth for the "waiting for the inverter" message.
 * Every tab that gates on connectivity renders this string (via the
 * <AwaitingConnection/> component) so the vocabulary a user sees never
 * depends on which page they happen to be on. Keep this in sync with the
 * assertions in `tests/pages/controlPageConnectionGate.test.tsx` and
 * `tests/pages/awaitingConnection.test.tsx` — they lock the wording down.
 *
 * Lives in `lib/` (not next to the component) so the component file only
 * exports a component, satisfying `react-refresh/only-export-components`.
 */
export function awaitingConnectionMessage(state: ConnectionState): string {
  if (state === 'reconnecting') return 'Connection lost — reconnecting…';
  if (state === 'disconnected') return 'Disconnected — will retry automatically';
  return 'Waiting for data…';
}
