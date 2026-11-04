/**
 * The charge cap's marker on a battery gauge (issue #426): one solid line in
 * the accent colour with a "Charge cap N%" tooltip, drawn the same on every
 * gauge (Battery page, orbit ring and glyph, house cabinet) so they match.
 */
import { CAP_MARKER_COLOR, capTitle } from '../lib/chargeCap';

interface Props {
  testId: string;
  capSoc: number;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  /** Stroke width in the gauge's own units. */
  width?: number;
}

export default function CapMarker({ testId, capSoc, x1, y1, x2, y2, width = 3 }: Props) {
  return (
    <line
      data-testid={testId}
      x1={x1}
      y1={y1}
      x2={x2}
      y2={y2}
      stroke={CAP_MARKER_COLOR}
      strokeWidth={width}
      strokeLinecap="round"
    >
      <title>{capTitle(capSoc)}</title>
    </line>
  );
}
