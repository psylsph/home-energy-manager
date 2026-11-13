/** Octopus's public API host (issue #469). Mirrors `OFFICIAL_BASE_URL` in
 *  `src-tauri/src/octopus.rs`. */
export const OCTOPUS_API_BASE = 'https://api.oegb-kraken.energy';

/** Agile unit rates for `region` from the start of `day` (YYYY-MM-DD, UTC). */
export function agileRatesUrl(region: string, day: string): string {
  return `${OCTOPUS_API_BASE}/v1/products/AGILE-24-10-01/electricity-tariffs/E-1R-AGILE-24-10-01-${region}/standard-unit-rates/?period_from=${day}T00:00:00Z&page_size=96`;
}
