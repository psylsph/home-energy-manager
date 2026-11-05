import { memo } from 'react';
import type { InverterSnapshot } from '../lib/types';
import type { BalanceSegment, BalanceSegmentKey } from '../lib/energyBalance';
import { balanceSummary, todayBalance } from '../lib/energyBalance';
import { formatEnergy } from '../lib/format';
import HelpTip from './HelpTip';

/**
 * Today statistics as two stacked balance bars (issue #429, decision D2:
 * replaces the old SummaryTiles outright). Both bars share the scale of the
 * larger total, so where In and Out differ (three-phase, Gateway, a clamped
 * Home Use) the gap shows rather than hiding behind per-bar stretching (D1); a derived percentages line (D3) sits
 * underneath. Every fill carries its own label and value, stacked, so the
 * text is in the same place and the same form at every width — no legend.
 */

interface Props {
  snapshot: InverterSnapshot;
}

/** Fill per segment: the flow colours, darkened in light mode for white text (`index.css`). */
const SEGMENT_COLOR: Record<BalanceSegmentKey, string> = {
  solar: 'var(--app-fill-solar)',
  grid_import: 'var(--app-fill-grid)',
  battery_out: 'var(--app-fill-battery)',
  home_use: 'var(--app-fill-home)',
  export: 'var(--app-fill-export)',
  battery_in: 'var(--app-fill-battery)',
};

/** In-fill text colour per fill: theme variables that clear 4.5:1 on it. */
const SEGMENT_TEXT_COLOR: Record<BalanceSegmentKey, string> = {
  solar: 'var(--app-on-solar)',
  grid_import: 'var(--app-on-grid)',
  battery_out: 'var(--app-on-battery)',
  home_use: 'var(--app-on-home)',
  export: 'var(--app-on-export)',
  battery_in: 'var(--app-on-battery)',
};

function BalanceBar({ title, segments, total }: {
  title: string;
  segments: BalanceSegment[];
  total: number;
}) {
  const valueOf = (s: BalanceSegment) => formatEnergy(s.kwh);
  return (
    <div data-testid="balance-bar">
      <div className="flex items-baseline justify-between gap-2">
        <span className="min-w-0 text-text-secondary text-xs font-medium">{title}</span>
        <span className="text-text-primary text-xs font-mono font-bold">{formatEnergy(total)}</span>
      </div>
      <div className="mt-1 flex min-h-9 w-full overflow-hidden rounded-full bg-bg-elevated">
        {segments.filter((s) => s.fraction > 0).map((s) => (
          <div
            key={s.key}
            data-testid="balance-fill"
            title={`${s.label} ${valueOf(s)}`}
            className="flex flex-col items-center justify-center overflow-hidden px-0.5 py-1 text-center leading-none motion-safe:transition-[flex-grow] motion-safe:duration-[400ms]"
            style={{
              // Grown by its share from a zero basis, with room for its text:
              // a tiny segment keeps a readable minimum and the others give it
              // up. Shares summing below 1 fill only that fraction of the
              // track, so the shared scale (D1) still holds.
              flexGrow: Number(s.fraction.toFixed(4)),
              flexShrink: 1,
              flexBasis: '0px',
              minWidth: '3rem',
              backgroundColor: SEGMENT_COLOR[s.key],
              color: SEGMENT_TEXT_COLOR[s.key],
            }}
          >
            {/* The label gets a two-line slot (a one-line label centres in
                it) so every value sits on the same line across the bar; the
                label wraps inside narrow fills instead of being shortened. */}
            <span className="flex min-h-[22px] items-center justify-center text-[11px] font-medium break-words">
              {s.label}
            </span>
            <span className="text-[11px] font-medium whitespace-nowrap">{valueOf(s)}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function EnergyBalanceBarsInner({ snapshot }: Props) {
  const balance = todayBalance(snapshot);
  const summary = balanceSummary(balance);
  const empty = balance.inTotal === 0 && balance.outTotal === 0;

  return (
    <section
      data-testid="today-balance"
      aria-label="Today's Energy Balance"
      className="bg-bg-surface rounded-lg p-3 sm:p-4 md:p-5 h-full flex flex-col gap-2"
    >
      <h2 className="hidden md:block text-text-primary text-base font-semibold tracking-wide">Today</h2>
      {empty ? (
        <p className="text-xs text-text-secondary">No energy recorded yet today.</p>
      ) : (
        <>
          <BalanceBar title="Energy In" segments={balance.inSegments} total={balance.inTotal} />
          <BalanceBar title="Energy Out" segments={balance.outSegments} total={balance.outTotal} />
        </>
      )}
      {balance.acCoupledSolarUnmeasured && (
        <p
          data-testid="balance-ac-note"
          className="flex items-center gap-1 text-[11px] leading-snug text-text-secondary"
        >
          Solar from the separate PV system isn't measured.
          <HelpTip label="unmeasured solar">
            The inverter's Solar counter only covers its own PV. The separate
            system's output isn't counted anywhere, so Home Use reads low, or
            zero on days that PV exports.
          </HelpTip>
        </p>
      )}
      {/* The ⓘ rides the end of the summary line. There isn't always a summary
          (just after midnight, or an AC-coupled night when all of In is
          unmeasured) — the tip stays available on its own then. */}
      <p className="text-[11px] leading-snug text-text-secondary">
        {summary && <span>{summary} </span>}
        <HelpTip label="Today's Energy Balance">
          Energy In is everything that came in: solar, grid import and
          battery discharge. Energy Out is everything used: the home, export and
          battery charging. Home Use is worked out from the other five counters
          (In less export and charging), so the two bars match. Three-phase
          inverters and the Gateway count Home Use themselves, so there In
          runs a few percent above Out from conversion and battery losses.
          Export can't be split into solar and battery, so on days the battery
          discharges the solar share is a minimum.
        </HelpTip>
      </p>
      {/* Text alternative for the figure: every value, plus both totals. */}
      <ul className="sr-only">
        <li>
          {`Energy In ${formatEnergy(balance.inTotal)} — ${
            balance.inSegments.map((s) => `${s.label} ${formatEnergy(s.kwh)}`).join(', ')
          }`}
        </li>
        <li>
          {`Energy Out ${formatEnergy(balance.outTotal)} — ${
            balance.outSegments.map((s) => `${s.label} ${formatEnergy(s.kwh)}`).join(', ')
          }`}
        </li>
      </ul>
    </section>
  );
}

const EnergyBalanceBars = memo(EnergyBalanceBarsInner);
export default EnergyBalanceBars;
