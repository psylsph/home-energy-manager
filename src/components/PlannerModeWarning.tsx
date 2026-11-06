import type { InverterSnapshot } from '../lib/types';
import { plannerModeConflict } from '../lib/plannerMode';
import ControlPageLink from './ControlPageLink';

/**
 * Beside Automatic Planning: names the charging mode that will overwrite the
 * plan (Cosy, Agile) or hold it back (Adaptive Charge), and links to Control
 * where the mode is set (issue #437). Renders nothing in Standard mode.
 */
export default function PlannerModeWarning({ snapshot }: { snapshot: InverterSnapshot | null }) {
  const conflict = plannerModeConflict(snapshot);
  if (!conflict) return null;
  return (
    <p role="status" data-testid="planner-mode-warning" className="text-[11px] text-amber-300">
      {conflict.message} Switch Charging Mode to Standard on the <ControlPageLink />.
    </p>
  );
}
