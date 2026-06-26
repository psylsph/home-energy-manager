/**
 * Vertical AA-cell style battery gauge.
 *
 * Renders state-of-charge as a fill inside a battery outline (rounded body
 * + terminal nub at the top), coloured by [`socColor`] tier. Replaces the
 * circular ring previously used in `BatteryPanel` and is reused small inside
 * the energy-flow diagram's battery node, so the SOC representation is
 * consistent everywhere.
 *
 * Pure SVG, scales via `width` (height follows the AA aspect ratio). The
 * `%` label is centred in the body and hides on the smallest size so it
 * doesn't crowd.
 */

import { memo } from 'react';
import { formatPercent } from '../lib/format';
import { socColor, batteryFillFraction } from '../lib/energyFlow';

interface Props {
  /** State of charge, 0–100. */
  soc: number;
  /** Rendered pixel width. Height is ~2× width (AA aspect). Default 96. */
  width?: number;
  /** Show the numeric `%` label inside the body. Default true. */
  showLabel?: boolean;
}

// ViewBox geometry — a 40×80 cell with a 16×6 terminal nub on top. The fill
// lives inside an inset body so the outline stroke never overlaps it.
const VB_W = 40;
const VB_H = 80;
const BODY_X = 6;
const BODY_W = VB_W - BODY_X * 2; // 28
const BODY_TOP = 10; // below the terminal nub
const BODY_BOTTOM = VB_H - 4;
const BODY_H = BODY_BOTTOM - BODY_TOP;
const BODY_RX = 5;
const NUB_W = 16;
const NUB_H = 6;
const NUB_X = (VB_W - NUB_W) / 2;

function BatteryGaugeInner({ soc, width = 96, showLabel = true }: Props) {
  const color = socColor(soc);
  const frac = batteryFillFraction(soc);
  // Fill grows from the bottom up. Height proportional to SOC; the top edge
  // sits at BODY_BOTTOM - (frac * BODY_H).
  const fillH = frac * BODY_H;
  const fillY = BODY_BOTTOM - fillH;
  const labelVisible = showLabel && width >= 72;
  // Label colour flips to dark ink when the fill reaches the label band so
  // the percentage stays readable against a bright fill; otherwise use the
  // elevated-surface text colour (set via CSS var on the parent).
  const labelOnFill = frac > 0.45;

  return (
    <svg
      viewBox={`0 0 ${VB_W} ${VB_H}`}
      width={width}
      height={width * (VB_H / VB_W)}
      role="img"
      aria-label={`Battery ${formatPercent(soc)} charged`}
      style={{ display: 'block' }}
    >
      {/* Terminal nub */}
      <rect
        x={NUB_X}
        y={2}
        width={NUB_W}
        height={NUB_H}
        rx={2}
        fill="var(--app-bg-elevated, #161B22)"
        stroke={color}
        strokeWidth={1.5}
      />
      {/* Body outline */}
      <rect
        x={BODY_X}
        y={BODY_TOP}
        width={BODY_W}
        height={BODY_H}
        rx={BODY_RX}
        fill="var(--app-bg-elevated, #161B22)"
        stroke={color}
        strokeWidth={2}
      />
      {/* SOC fill */}
      {frac > 0 && (
        <rect
          x={BODY_X + 2}
          y={fillY}
          width={BODY_W - 4}
          height={Math.max(0, fillH - 2)}
          rx={BODY_RX - 2}
          fill={color}
          // Ease the fill transition when SOC ticks so it doesn't snap.
          style={{ transition: 'y 0.6s ease, height 0.6s ease' }}
        />
      )}
      {/* Percentage label, centred in the body */}
      {labelVisible && (
        <text
          x={VB_W / 2}
          y={VB_H / 2 + 4}
          textAnchor="middle"
          fontSize={12}
          fontWeight={700}
          fontFamily="var(--font-mono, monospace)"
          fill={labelOnFill ? 'var(--app-bg-base, #0D1117)' : 'var(--app-text-primary, #E6EDF3)'}
        >
          {formatPercent(soc)}
        </text>
      )}
    </svg>
  );
}

const BatteryGauge = memo(BatteryGaugeInner);
export default BatteryGauge;
