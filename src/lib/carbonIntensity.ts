/**
 * Grid carbon intensity for the house view (issue #401).
 *
 * `GET /api/carbon-intensity` returns the NESO 24-hour forecast for the
 * weather postcode's region (or nationally); this picks what to show: the
 * current half hour's intensity and band, and the lowest half hour still to
 * come. Pure, so it's unit-tested without a backend.
 */

/** One half hour as the backend sends it. */
export interface CarbonPeriod {
  from: string;
  to: string;
  /** gCO₂/kWh. */
  forecast: number;
  index: string;
}

/** `data` of `GET /api/carbon-intensity`. */
export interface CarbonForecast {
  region: string | null;
  fetched_at: string;
  periods: CarbonPeriod[];
}

export type CarbonBand = 'very low' | 'low' | 'moderate' | 'high' | 'very high';

const BANDS: readonly CarbonBand[] = ['very low', 'low', 'moderate', 'high', 'very high'];

/** Colour per band, greens to red; an unknown band is drawn grey. */
export const CARBON_BAND_COLORS: Record<CarbonBand, string> = {
  'very low': '#16A34A',
  low: '#22C55E',
  moderate: '#F59E0B',
  high: '#F97316',
  'very high': '#EF4444',
};

export interface CurrentCarbon {
  /** gCO₂/kWh now. */
  intensity: number;
  band: CarbonBand | null;
  region: string | null;
  /** The lowest half hour ahead, when it's lower than now (earliest of ties). */
  lowest: { at: number; intensity: number } | null;
}

/**
 * What to show at `now` (ms), or `null` when no period covers it — no
 * forecast, or one so old (or a clock so wrong) that it has run out.
 */
export function currentCarbon(forecast: CarbonForecast | null | undefined, now: number): CurrentCarbon | null {
  if (!forecast || !Array.isArray(forecast.periods)) return null;
  const periods = forecast.periods
    .map((p) => ({ from: Date.parse(p.from), to: Date.parse(p.to), intensity: p.forecast, index: p.index }))
    .filter((p) => Number.isFinite(p.from) && Number.isFinite(p.to) && p.to > p.from
      && Number.isFinite(p.intensity) && p.intensity >= 0);
  const current = periods.find((p) => p.from <= now && now < p.to);
  if (!current) return null;
  let lowest: CurrentCarbon['lowest'] = null;
  for (const p of periods) {
    if (p.from < current.to) continue;
    if (p.intensity < (lowest?.intensity ?? current.intensity)
      || (lowest && p.intensity === lowest.intensity && p.from < lowest.at)) {
      lowest = { at: p.from, intensity: p.intensity };
    }
  }
  const band = (BANDS as readonly string[]).includes(current.index) ? (current.index as CarbonBand) : null;
  return { intensity: current.intensity, band, region: forecast.region ?? null, lowest };
}

/** Intensity as drawn, e.g. `"183 g/kWh"`. */
export function formatCarbon(g: number): string {
  return `${Math.round(g)} g/kWh`;
}

/** Local clock time of a period start, e.g. `"03:30"`. */
export function formatCarbonTime(ms: number): string {
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

/** Accessible description, e.g. `"Grid carbon 183 g/kWh, high, South England. Lowest 03:30 at 92 g/kWh."`. */
export function carbonLabel(c: CurrentCarbon): string {
  const parts = [`Grid carbon ${formatCarbon(c.intensity)}`];
  if (c.band) parts.push(c.band);
  if (c.region) parts.push(c.region);
  let label = `${parts.join(', ')}.`;
  if (c.lowest) label += ` Lowest ${formatCarbonTime(c.lowest.at)} at ${formatCarbon(c.lowest.intensity)}.`;
  return label;
}
