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
  /** Rendered pixel width. Height follows the chosen orientation. Default 96. */
  width?: number;
  /** Show the numeric `%` label inside the body. Default true. */
  showLabel?: boolean;
  /** Battery orientation. Mobile panels use the horizontal variant. */
  orientation?: 'vertical' | 'horizontal';
}

// ViewBox geometry — a 40×80 cell with a 16×6 terminal nub on top. The fill
// lives inside an inset body so the outline stroke never overlaps it.
const VB_W = 40;
const VB_H = 80;
// Body inset — left wide enough that a 4-char label ("100%") fits inside
// the outline at the scaled-down font size used for that case.
const BODY_X = 5;
const BODY_W = VB_W - BODY_X * 2; // 30
const BODY_TOP = 10; // below the terminal nub
const BODY_BOTTOM = VB_H - 4;
const BODY_H = BODY_BOTTOM - BODY_TOP;
const BODY_RX = 5;
const NUB_W = 16;
const NUB_H = 6;
const NUB_X = (VB_W - NUB_W) / 2;

function BatteryGaugeInner({ soc, width = 96, showLabel = true, orientation = 'vertical' }: Props) {
  const color = socColor(soc);
  const frac = batteryFillFraction(soc);
  const horizontal = orientation === 'horizontal';
  // Vertical fill grows from bottom up. Horizontal fill grows left to right.
  const fillH = frac * BODY_H;
  const fillY = BODY_BOTTOM - fillH;
  const labelVisible = showLabel && width >= 72;
  // Label colour always follows the app theme (light text in dark mode,
  // dark in light mode). We deliberately do NOT flip to a dark ink when the
  // fill covers the text band: during the SOC transition the fill only
  // partially covers the glyph, so a hard flip leaves half the text
  // unreadable against whichever half it isn't on. A single theme-bound
  // colour stays legible across the whole range and at both extremes.
  //
  // Font size scales down for the 4-char "100%" case so it never overflows
  // the body outline (2/3-char labels use the full size).
  const labelText = formatPercent(soc);
  const labelFontSize = labelText.length >= 4 ? 10 : 12;

  if (horizontal) {
    const hBodyX = 4;
    const hBodyY = 6;
    const hBodyW = 68;
    const hBodyH = 28;
    const hFillW = Math.max(0, frac * (hBodyW - 4));
    return (
      <svg
        data-orientation="horizontal"
        viewBox="0 0 80 40"
        width={width}
        height={width * 0.5}
        role="img"
        aria-label={`Battery ${formatPercent(soc)} charged`}
        style={{ display: 'block' }}
      >
        <rect
          x={hBodyX}
          y={hBodyY}
          width={hBodyW}
          height={hBodyH}
          rx={5}
          fill="var(--app-bg-elevated, #161B22)"
          stroke={color}
          strokeWidth={2}
        />
        <rect
          x={73}
          y={15}
          width={5}
          height={10}
          rx={2}
          fill="var(--app-bg-elevated, #161B22)"
          stroke={color}
          strokeWidth={1.5}
        />
        {frac > 0 && (
          <rect
            x={hBodyX + 2}
            y={hBodyY + 2}
            width={hFillW}
            height={hBodyH - 4}
            rx={3}
            fill={color}
            style={{ transition: 'width 0.6s ease' }}
          />
        )}
        {labelVisible && (
          <text
            x={hBodyX + hBodyW / 2}
            y={hBodyY + hBodyH / 2 + 4}
            textAnchor="middle"
            fontSize={labelFontSize}
            fontWeight={700}
            fontFamily="var(--font-mono, monospace)"
            fill="var(--app-text-primary, #E6EDF3)"
          >
            {labelText}
          </text>
        )}
      </svg>
    );
  }

  return (
    <svg
      data-orientation="vertical"
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
          fontSize={labelFontSize}
          fontWeight={700}
          fontFamily="var(--font-mono, monospace)"
          fill="var(--app-text-primary, #E6EDF3)"
        >
          {labelText}
        </text>
      )}
    </svg>
  );
}

const BatteryGauge = memo(BatteryGaugeInner);
export default BatteryGauge;
