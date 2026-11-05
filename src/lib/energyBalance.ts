import { hasNoPvInput } from './deviceCapabilities';
import type { InverterSnapshot } from './types';

/**
 * Today statistics as balance bars (issue #429).
 *
 * The old tiles are replaced by two stacked bars sharing one scale — the
 * larger of the two totals (decision D1) — so any In ≠ Out gap stays visible
 * instead of each bar stretching to 100% and hiding it.
 *
 * When the totals differ: on single-phase, All-in-One and AC-coupled models
 * the decoder derives `home_energy_today_kwh` as solar + import + discharge −
 * export − charge, so In equals Out exactly unless that sum clamps at zero.
 * Three-phase inverters and the Gateway read Home Use from their own load
 * register, and there conversion and battery losses leave In a few percent
 * above Out. On AC-coupled inverters and the residential All-in-Ones
 * `today_solar_kwh` covers only the inverter's own PV — which is none — so
 * the separate system's output reaches no counter and Home Use reads low;
 * that case is flagged so the component can say so
 * (`acCoupledSolarUnmeasured`).
 */

/** Identifies a segment so the component can pick its theme colour. */
export type BalanceSegmentKey =
  | 'solar'
  | 'grid_import'
  | 'battery_out'
  | 'home_use'
  | 'export'
  | 'battery_in';

export interface BalanceSegment {
  key: BalanceSegmentKey;
  /** Title Case label, e.g. `Grid Import`. */
  label: string;
  kwh: number;
  /** Width as a fraction of `scaleKwh`; 0 segments are listed but not drawn. */
  fraction: number;
}

export interface TodayBalance {
  inSegments: BalanceSegment[];
  outSegments: BalanceSegment[];
  inTotal: number;
  outTotal: number;
  /** The larger of the two totals: one scale for both bars (decision D1). */
  scaleKwh: number;
  /** AC-coupled or All-in-One: today's Solar counter misses the separate PV system. */
  acCoupledSolarUnmeasured: boolean;
}

/** A corrupted or unreported reading counts as 0, never negative or NaN. */
function counter(v: number): number {
  return Number.isFinite(v) && v > 0 ? v : 0;
}

function withFractions(
  segments: Array<Omit<BalanceSegment, 'fraction'>>,
  scale: number,
): BalanceSegment[] {
  return segments.map((s) => ({ ...s, fraction: s.kwh / scale }));
}

export function todayBalance(snapshot: InverterSnapshot): TodayBalance {
  const inSeed = [
    { key: 'solar' as const, label: 'Solar', kwh: counter(snapshot.today_solar_kwh) },
    { key: 'grid_import' as const, label: 'Grid Import', kwh: counter(snapshot.today_import_kwh) },
    { key: 'battery_out' as const, label: 'Battery Out', kwh: counter(snapshot.today_discharge_kwh) },
  ];
  const outSeed = [
    { key: 'home_use' as const, label: 'Home Use', kwh: counter(snapshot.home_energy_today_kwh) },
    { key: 'export' as const, label: 'Grid Export', kwh: counter(snapshot.today_export_kwh) },
    { key: 'battery_in' as const, label: 'Battery In', kwh: counter(snapshot.today_charge_kwh) },
  ];
  const inTotal = inSeed.reduce((t, s) => t + s.kwh, 0);
  const outTotal = outSeed.reduce((t, s) => t + s.kwh, 0);
  const scaleKwh = Math.max(inTotal, outTotal);
  // Given no measurement at all (just after midnight), the scale is 0; keep
  // fractions at 0 rather than dividing into NaN (invariant 6).
  const scale = scaleKwh > 0 ? scaleKwh : 1;
  return {
    inSegments: withFractions(inSeed, scale),
    outSegments: withFractions(outSeed, scale),
    inTotal,
    outTotal,
    scaleKwh,
    acCoupledSolarUnmeasured: hasNoPvInput(snapshot.device_type_code),
  };
}

/**
 * The derived summary line (decision D3): how much of today's solar stayed
 * in the home (used or stored — i.e. not exported) and how much of the total
 * came off the grid. Each part is dropped when it would be meaningless
 * (unmeasured or zero solar; an empty In bar) and the whole line is null
 * when nothing is left to say.
 */
export function balanceSummary(balance: TodayBalance): string | null {
  const part = (key: BalanceSegmentKey) =>
    balance.inSegments.find((s) => s.key === key)?.kwh
    ?? balance.outSegments.find((s) => s.key === key)?.kwh
    ?? 0;
  const parts: string[] = [];
  if (!balance.acCoupledSolarUnmeasured) {
    const solar = part('solar');
    if (solar > 0) {
      // The export counter doesn't split solar from battery export, so solar
      // less export is exact only when the battery never discharged; after a
      // discharge it's the least that stayed home, and a zero says nothing.
      const percent = Math.round((Math.max(0, solar - part('export')) / solar) * 100);
      if (part('battery_out') === 0) {
        parts.push(`${percent}% of solar used or stored at home`);
      } else if (percent > 0) {
        parts.push(`At least ${percent}% of solar used or stored at home`);
      }
    }
  }
  if (balance.inTotal > 0) {
    parts.push(`${Math.round((part('grid_import') / balance.inTotal) * 100)}% of energy in came from the grid`);
  }
  return parts.length > 0 ? parts.join(' · ') : null;
}
