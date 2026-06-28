import { deriveBatteryModeRows, type MechanismRow } from '../lib/batteryMode';
import type { InverterSnapshot } from '../lib/types';

interface Props {
  snapshot: InverterSnapshot;
  now?: Date;
}

function StateDot({ state }: { state: MechanismRow['state'] }) {
  return (
    <span
      aria-hidden="true"
      className={
        state === 'off'
          ? 'inline-block w-2.5 h-2.5 rounded-full border-2 border-text-secondary/40'
          : 'inline-block w-2.5 h-2.5 rounded-full bg-battery'
      }
    />
  );
}

/**
 * Four-row battery-mechanism summary for the Inverter page.
 *
 * Replaces the old single `battery_mode` label with one row per
 * independent mechanism (Eco, Timed Charge, Timed Export, Timed Discharge),
 * showing whether each is off, armed, or actively doing its thing. The
 * filled/empty dots reuse the battery accent colour from the Control page
 * mechanism buttons so the two surfaces read consistently.
 */
export function BatteryModeSummary({ snapshot, now }: Props) {
  const rows = deriveBatteryModeRows(snapshot, now);

  return (
    <div className="space-y-1">
      {rows.map((row) => (
        <div
          key={row.key}
          data-testid={`battery-mode-${row.key}`}
          data-state={row.state}
          className="flex items-center justify-between gap-3 text-sm"
        >
          <span className="flex items-center gap-2 min-w-0">
            <StateDot state={row.state} />
            <span
              className={
                row.state === 'off' ? 'text-text-secondary' : 'text-text-primary'
              }
            >
              {row.label}
            </span>
          </span>
          <span
            data-testid={`battery-mode-desc-${row.key}`}
            className={
              row.state === 'active'
                ? 'font-mono text-battery whitespace-nowrap'
                : 'font-mono text-text-secondary whitespace-nowrap'
            }
          >
            {row.description}
          </span>
        </div>
      ))}
    </div>
  );
}

export default BatteryModeSummary;
