export interface HistoryCsvPoint {
  t: number;
  v: number;
}

export interface HistoryCsvChart {
  fields: { field: string }[];
  requires?: string[];
  preprocess?: (rows: Record<string, number>[]) => Record<string, number>[];
}

/** Build CSV content for one or more history charts, including derived data. */
export function buildHistoryCsv(
  charts: HistoryCsvChart[],
  data: Record<string, HistoryCsvPoint[]>,
): string {
  // Collect all unique field names across all charts.
  const allFields = [...new Set(charts.flatMap((chart) => [
    ...chart.fields.map((field) => field.field),
    ...(chart.requires ?? []),
  ]))];

  const timestamps = new Set<number>();
  for (const field of allFields) {
    const points = data[field];
    if (points) {
      for (const point of points) timestamps.add(point.t);
    }
  }
  const sortedTimestamps = [...timestamps].sort((a, b) => a - b);

  // Each preprocessor receives the output of the previous one. This keeps
  // all derived columns when a tab has more than one derived chart.
  const derivedCharts = charts.filter((chart) => chart.preprocess);
  let processed: Record<string, number>[] = [];
  if (derivedCharts.length > 0) {
    processed = sortedTimestamps.map((timestamp) => {
      const row: Record<string, number> = { t: timestamp };
      for (const field of allFields) {
        const point = data[field]?.find((candidate) => candidate.t === timestamp);
        if (point) row[field] = point.v;
      }
      return row;
    });
    for (const chart of derivedCharts) {
      if (chart.preprocess) processed = chart.preprocess(processed);
    }
  }

  const header = ['Timestamp', ...allFields];
  const rows = sortedTimestamps.map((timestamp) => {
    const processedRow = processed.find((row) => row.t === timestamp);
    const iso = new Date(timestamp).toISOString();
    const values = allFields.map((field) => {
      if (processedRow && field in processedRow) return processedRow[field]?.toString() ?? '';
      const point = data[field]?.find((candidate) => candidate.t === timestamp);
      return point?.v?.toString() ?? '';
    });
    return [iso, ...values];
  });

  return [header.join(','), ...rows.map((row) => row.join(','))].join('\n');
}
