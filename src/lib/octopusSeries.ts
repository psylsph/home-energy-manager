export interface OctopusPoint {
  t: number;
  v: number;
}

export type OctopusSeries = Record<string, OctopusPoint[]>;

/** Merge supplier series onto a shared timestamp axis for Recharts. */
export function mergeOctopusSeries(
  series: OctopusSeries,
  fields: string[],
): Record<string, number>[] {
  const rows = new Map<number, Record<string, number>>();
  for (const field of fields) {
    for (const point of series[field] ?? []) {
      if (!Number.isFinite(point.t) || !Number.isFinite(point.v)) continue;
      const row = rows.get(point.t) ?? { t: point.t };
      row[field] = point.v;
      rows.set(point.t, row);
    }
  }
  return [...rows.values()].sort((a, b) => a.t - b.t);
}

/**
 * Convert interval consumption into running totals over the selected window.
 * Every output row carries all requested fields so a sparse export/gas series
 * does not make another cumulative line disappear between its own readings.
 */
export function cumulativeOctopusSeries(
  series: OctopusSeries,
  fields: string[],
): Record<string, number>[] {
  const merged = mergeOctopusSeries(series, fields);
  const totals = Object.fromEntries(fields.map((field) => [field, 0])) as Record<string, number>;
  return merged.map((row) => {
    for (const field of fields) {
      const value = row[field];
      if (Number.isFinite(value)) totals[field] += value;
    }
    return { t: row.t, ...totals };
  });
}

export function octopusSeriesTotal(points: OctopusPoint[] | undefined): number {
  return (points ?? []).reduce(
    (total, point) => Number.isFinite(point.v) ? total + point.v : total,
    0,
  );
}
