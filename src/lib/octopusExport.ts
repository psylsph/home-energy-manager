import type { jsPDF } from 'jspdf';

export interface OctopusExportBillingSummary {
  electricity_import_kwh: number;
  electricity_export_kwh: number;
  gas_usage: number;
  electricity_energy_cost_gbp: number;
  electricity_standing_cost_gbp: number;
  electricity_total_cost_gbp: number;
  export_income_gbp: number;
  gas_energy_cost_gbp: number | null;
  gas_standing_cost_gbp: number;
  gas_total_cost_gbp: number | null;
  net_cost_gbp: number | null;
  pricing_complete: boolean;
}

export interface OctopusExportBillingPeriod extends OctopusExportBillingSummary {
  period: string;
}

export interface OctopusExportComparisonDay {
  date: string;
  octopus_import_kwh: number | null;
  hem_import_kwh: number | null;
  import_difference_kwh: number | null;
  import_difference_percent: number | null;
  octopus_export_kwh: number | null;
  hem_export_kwh: number | null;
  export_difference_kwh: number | null;
  export_difference_percent: number | null;
  expected_import_intervals: number;
  import_intervals: number;
  missing_import_intervals: number;
  expected_export_intervals: number;
  export_intervals: number;
  missing_export_intervals: number;
  expected_gas_intervals: number;
  gas_intervals: number;
  missing_gas_intervals: number;
}

export interface OctopusExportComparisonTotals {
  octopus_import_kwh: number;
  hem_import_kwh: number;
  import_difference_kwh: number;
  octopus_export_kwh: number;
  hem_export_kwh: number;
  export_difference_kwh: number;
  expected_import_intervals: number;
  import_intervals: number;
  missing_import_intervals: number;
  expected_export_intervals: number;
  export_intervals: number;
  missing_export_intervals: number;
  expected_gas_intervals: number;
  gas_intervals: number;
  missing_gas_intervals: number;
}

export interface OctopusExportData {
  rangeLabel: string;
  generatedAt: Date;
  gasUnit: 'unknown' | 'kwh' | 'm3';
  costPeriods: OctopusExportBillingPeriod[];
  billing: {
    totals: OctopusExportBillingSummary;
    daily: OctopusExportBillingPeriod[];
    monthly: OctopusExportBillingPeriod[];
    yearly: OctopusExportBillingPeriod[];
    gas_cost_available: boolean;
  };
  comparison: {
    totals: OctopusExportComparisonTotals;
    days: OctopusExportComparisonDay[];
    import_stream_available: boolean;
    export_stream_available: boolean;
    gas_stream_available: boolean;
  };
}

function csvCell(value: string | number | boolean | null): string {
  if (value == null) return '';
  const text = String(value);
  return /[",\n\r]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

function fixed(value: number | null, decimals: number): string {
  return value == null ? '' : value.toFixed(decimals);
}

function csvRows(rows: Array<Array<string | number | boolean | null>>): string {
  return rows.map((row) => row.map(csvCell).join(',')).join('\n');
}

export function buildOctopusSummaryCsv(data: OctopusExportData): string {
  const totals = data.billing.totals;
  const comparison = data.comparison.totals;
  const rows: Array<Array<string | number | boolean | null>> = [
    ['Report', 'Octopus Energy Summary'],
    ['Period', data.rangeLabel],
    ['Generated', data.generatedAt.toISOString()],
    ['Estimated', true],
    ['Pricing complete', totals.pricing_complete],
    ['Gas unit', data.gasUnit],
    [],
    ['Selected Period Totals'],
    ['Metric', 'Value'],
    ['Electricity import kWh', fixed(totals.electricity_import_kwh, 3)],
    ['Electricity energy cost GBP', fixed(totals.electricity_energy_cost_gbp, 2)],
    ['Electricity standing cost GBP', fixed(totals.electricity_standing_cost_gbp, 2)],
    ['Electricity total cost GBP', fixed(totals.electricity_total_cost_gbp, 2)],
    ['Electricity export kWh', fixed(totals.electricity_export_kwh, 3)],
    ['Export income GBP', fixed(totals.export_income_gbp, 2)],
    ['Gas usage', fixed(totals.gas_usage, 3)],
    ['Gas energy cost GBP', fixed(totals.gas_energy_cost_gbp, 2)],
    ['Gas standing cost GBP', fixed(totals.gas_standing_cost_gbp, 2)],
    ['Gas total cost GBP', fixed(totals.gas_total_cost_gbp, 2)],
    ['Net cost GBP', fixed(totals.net_cost_gbp, 2)],
    [],
    ['Monthly Summary'],
    ['Month', 'Import kWh', 'Energy cost GBP', 'Electricity standing GBP', 'Import total GBP', 'Export kWh', 'Export income GBP', 'Gas usage', 'Gas energy GBP', 'Gas standing GBP', 'Gas total GBP', 'Net GBP', 'Pricing complete'],
  ];

  for (const period of data.billing.monthly) {
    rows.push([
      period.period,
      fixed(period.electricity_import_kwh, 3),
      fixed(period.electricity_energy_cost_gbp, 2),
      fixed(period.electricity_standing_cost_gbp, 2),
      fixed(period.electricity_total_cost_gbp, 2),
      fixed(period.electricity_export_kwh, 3),
      fixed(period.export_income_gbp, 2),
      fixed(period.gas_usage, 3),
      fixed(period.gas_energy_cost_gbp, 2),
      fixed(period.gas_standing_cost_gbp, 2),
      fixed(period.gas_total_cost_gbp, 2),
      fixed(period.net_cost_gbp, 2),
      period.pricing_complete,
    ]);
  }

  rows.push(
    [],
    ['Yearly Summary'],
    ['Year', 'Import kWh', 'Import total GBP', 'Export kWh', 'Export income GBP', 'Gas usage', 'Gas total GBP', 'Net GBP', 'Pricing complete'],
  );
  for (const period of data.billing.yearly) {
    rows.push([
      period.period,
      fixed(period.electricity_import_kwh, 3),
      fixed(period.electricity_total_cost_gbp, 2),
      fixed(period.electricity_export_kwh, 3),
      fixed(period.export_income_gbp, 2),
      fixed(period.gas_usage, 3),
      fixed(period.gas_total_cost_gbp, 2),
      fixed(period.net_cost_gbp, 2),
      period.pricing_complete,
    ]);
  }

  rows.push(
    [],
    ['HEM Comparison Totals'],
    ['Direction', 'Octopus kWh', 'HEM kWh', 'Difference kWh (HEM minus Octopus)'],
    ['Import', fixed(comparison.octopus_import_kwh, 3), fixed(comparison.hem_import_kwh, 3), fixed(comparison.import_difference_kwh, 3)],
    ['Export', fixed(comparison.octopus_export_kwh, 3), fixed(comparison.hem_export_kwh, 3), fixed(comparison.export_difference_kwh, 3)],
    [],
    ['Daily Comparison and Missing Data'],
    ['Date', 'Octopus import kWh', 'HEM import kWh', 'Import difference kWh', 'Import difference %', 'Octopus export kWh', 'HEM export kWh', 'Export difference kWh', 'Export difference %', 'Import intervals', 'Expected import', 'Missing import', 'Export intervals', 'Expected export', 'Missing export', 'Gas intervals', 'Expected gas', 'Missing gas'],
  );
  for (const day of data.comparison.days) {
    rows.push([
      day.date,
      fixed(day.octopus_import_kwh, 3),
      fixed(day.hem_import_kwh, 3),
      fixed(day.import_difference_kwh, 3),
      fixed(day.import_difference_percent, 1),
      fixed(day.octopus_export_kwh, 3),
      fixed(day.hem_export_kwh, 3),
      fixed(day.export_difference_kwh, 3),
      fixed(day.export_difference_percent, 1),
      day.import_intervals,
      day.expected_import_intervals,
      day.missing_import_intervals,
      day.export_intervals,
      day.expected_export_intervals,
      day.missing_export_intervals,
      day.gas_intervals,
      day.expected_gas_intervals,
      day.missing_gas_intervals,
    ]);
  }

  return csvRows(rows);
}

export interface OctopusCostPoint {
  period: string;
  electricity_import_cost: number;
  gas_cost: number | null;
  net_cost: number;
  export_income: number;
}

export function buildOctopusCostSeries(
  periods: OctopusExportBillingPeriod[],
): OctopusCostPoint[] {
  return periods.map((period) => ({
    period: period.period,
    electricity_import_cost: period.electricity_total_cost_gbp,
    gas_cost: period.gas_total_cost_gbp,
    net_cost: period.net_cost_gbp
      ?? period.electricity_total_cost_gbp - period.export_income_gbp,
    export_income: period.export_income_gbp,
  }));
}

function money(value: number | null): string {
  return value == null ? 'Unavailable' : `GBP ${value.toFixed(2)}`;
}

function kwh(value: number | null): string {
  return value == null ? '-' : `${value.toFixed(3)} kWh`;
}

function drawCostGraph(doc: jsPDF, points: OctopusCostPoint[], y: number): number {
  doc.setFontSize(14);
  doc.text('Costs and export income', 14, y);
  y += 7;
  if (points.length === 0) {
    doc.setFontSize(9);
    doc.text('No cost periods are available for this selection.', 14, y + 5);
    return y + 14;
  }

  const left = 18;
  const top = y + 4;
  const width = 174;
  const height = 54;
  const values = points.flatMap((point) => [
    point.electricity_import_cost,
    point.gas_cost ?? 0,
    point.net_cost,
    point.export_income,
  ]);
  const minValue = Math.min(0, ...values);
  const maxValue = Math.max(1, ...values);
  const valueRange = Math.max(1, maxValue - minValue);
  const xFor = (index: number) => left + (index / Math.max(1, points.length - 1)) * width;
  const yFor = (value: number) => top + height - ((value - minValue) / valueRange) * height;

  doc.setDrawColor(210, 218, 228);
  doc.setLineWidth(0.2);
  for (let index = 0; index <= 4; index += 1) {
    const lineY = top + index / 4 * height;
    doc.line(left, lineY, left + width, lineY);
  }

  const series = [
    { key: 'electricity_import_cost' as const, label: 'Electricity import', color: [239, 68, 68] as const },
    { key: 'gas_cost' as const, label: 'Gas', color: [245, 158, 11] as const },
    { key: 'net_cost' as const, label: 'Net cost', color: [59, 130, 246] as const },
    { key: 'export_income' as const, label: 'Export income', color: [34, 197, 94] as const },
  ];
  for (const item of series) {
    doc.setDrawColor(item.color[0], item.color[1], item.color[2]);
    doc.setLineWidth(0.7);
    let previous: [number, number] | null = null;
    points.forEach((point, index) => {
      const raw = point[item.key];
      if (raw == null) {
        previous = null;
        return;
      }
      const current: [number, number] = [xFor(index), yFor(raw)];
      if (previous) doc.line(previous[0], previous[1], current[0], current[1]);
      doc.circle(current[0], current[1], 0.7, 'F');
      previous = current;
    });
  }

  doc.setFontSize(7);
  doc.setTextColor(80, 90, 105);
  doc.text(`GBP ${maxValue.toFixed(2)}`, 14, top + 2, { align: 'right' });
  doc.text(`GBP ${minValue.toFixed(2)}`, 14, top + height, { align: 'right' });
  const labelEvery = Math.max(1, Math.ceil(points.length / 8));
  points.forEach((point, index) => {
    if (index % labelEvery === 0 || index === points.length - 1) {
      doc.text(point.period, xFor(index), top + height + 4, { align: 'center' });
    }
  });
  series.forEach((item, index) => {
    const legendX = left + index * 42;
    doc.setFillColor(item.color[0], item.color[1], item.color[2]);
    doc.rect(legendX, top - 4, 3, 2, 'F');
    doc.setTextColor(60, 70, 85);
    doc.text(item.label, legendX + 4, top - 2.2);
  });
  doc.setTextColor(0, 0, 0);
  return top + height + 11;
}

function drawTable(
  doc: jsPDF,
  title: string,
  headers: string[],
  rows: string[][],
  startY: number,
): number {
  let y = startY;
  const pageHeight = doc.internal.pageSize.getHeight();
  const widths = headers.map(() => 182 / headers.length);
  const drawHeader = () => {
    doc.setFillColor(226, 232, 240);
    doc.rect(14, y, 182, 7, 'F');
    doc.setFont('helvetica', 'bold');
    doc.setFontSize(6.5);
    let x = 14;
    headers.forEach((header, index) => {
      doc.text(header, x + 1, y + 4.5, { maxWidth: widths[index] - 2 });
      x += widths[index];
    });
    doc.setFont('helvetica', 'normal');
    y += 7;
  };
  const newPage = () => {
    doc.addPage();
    y = 14;
    doc.setFontSize(13);
    doc.text(`${title} (continued)`, 14, y);
    y += 5;
    drawHeader();
  };

  if (y > pageHeight - 30) {
    doc.addPage();
    y = 14;
  }
  doc.setFontSize(13);
  doc.setFont('helvetica', 'bold');
  doc.text(title, 14, y);
  doc.setFont('helvetica', 'normal');
  y += 5;
  drawHeader();
  for (const row of rows) {
    if (y > pageHeight - 12) newPage();
    let x = 14;
    doc.setFontSize(6.5);
    row.forEach((cell, index) => {
      doc.text(cell, x + 1, y + 4, { maxWidth: widths[index] - 2 });
      x += widths[index];
    });
    doc.setDrawColor(230, 235, 241);
    doc.line(14, y + 6, 196, y + 6);
    y += 6;
  }
  return y + 6;
}

export async function buildOctopusSummaryPdf(data: OctopusExportData): Promise<jsPDF> {
  const { jsPDF: JsPdf } = await import('jspdf');
  const doc = new JsPdf({ unit: 'mm', format: 'a4' });
  const totals = data.billing.totals;
  doc.setProperties({
    title: `Octopus Energy Summary - ${data.rangeLabel}`,
    subject: 'Home Energy Manager supplier summary',
    creator: 'Home Energy Manager',
  });
  doc.setFontSize(22);
  doc.setFont('helvetica', 'bold');
  doc.text('Octopus Energy Summary', 14, 18);
  doc.setFont('helvetica', 'normal');
  doc.setFontSize(9);
  doc.setTextColor(90, 100, 115);
  doc.text(`Home Energy Manager | ${data.rangeLabel} | Generated ${data.generatedAt.toLocaleString()}`, 14, 24);
  doc.text('Supplier costs are estimates and may differ from the final Octopus bill.', 14, 29);
  doc.setTextColor(0, 0, 0);

  const cards = [
    ['Electricity import', kwh(totals.electricity_import_kwh), money(totals.electricity_total_cost_gbp)],
    ['Electricity export', kwh(totals.electricity_export_kwh), `${money(totals.export_income_gbp)} income`],
    ['Gas', `${totals.gas_usage.toFixed(3)} ${data.gasUnit}`, money(totals.gas_total_cost_gbp)],
    ['Net cost', money(totals.net_cost_gbp ?? totals.electricity_total_cost_gbp - totals.export_income_gbp), ''],
  ];
  cards.forEach((card, index) => {
    const x = 14 + index * 46;
    doc.setFillColor(248, 250, 252);
    doc.setDrawColor(220, 226, 234);
    doc.roundedRect(x, 35, 43, 22, 2, 2, 'FD');
    doc.setFontSize(7);
    doc.setTextColor(90, 100, 115);
    doc.text(card[0], x + 3, 41);
    doc.setTextColor(20, 30, 45);
    doc.setFontSize(10);
    doc.setFont('helvetica', 'bold');
    doc.text(card[1], x + 3, 48, { maxWidth: 38 });
    doc.setFont('helvetica', 'normal');
    doc.setFontSize(7);
    doc.text(card[2], x + 3, 54, { maxWidth: 38 });
  });
  if (!totals.pricing_complete) {
    doc.setFontSize(8);
    doc.setTextColor(161, 98, 7);
    doc.text('Some historical tariff prices could not be matched; cost estimates are incomplete.', 14, 63);
    doc.setTextColor(0, 0, 0);
  }

  let y = drawCostGraph(doc, buildOctopusCostSeries(data.costPeriods), 67);
  const monthlyRows = [...data.billing.monthly].reverse().map((period) => [
    period.period,
    kwh(period.electricity_import_kwh),
    money(period.electricity_total_cost_gbp),
    kwh(period.electricity_export_kwh),
    money(period.export_income_gbp),
    money(period.gas_total_cost_gbp),
    money(period.net_cost_gbp),
  ]);
  y = drawTable(doc, 'Monthly summary', ['Month', 'Import', 'Import cost', 'Export', 'Income', 'Gas cost', 'Net'], monthlyRows, y + 3);
  const yearlyRows = [...data.billing.yearly].reverse().map((period) => [
    period.period,
    kwh(period.electricity_import_kwh),
    money(period.electricity_total_cost_gbp),
    kwh(period.electricity_export_kwh),
    money(period.export_income_gbp),
    money(period.gas_total_cost_gbp),
    money(period.net_cost_gbp),
  ]);
  y = drawTable(doc, 'Yearly summary', ['Year', 'Import', 'Import cost', 'Export', 'Income', 'Gas cost', 'Net'], yearlyRows, y);
  const comparisonRows = [...data.comparison.days].reverse().map((day) => [
    day.date,
    kwh(day.octopus_import_kwh),
    kwh(day.hem_import_kwh),
    kwh(day.import_difference_kwh),
    kwh(day.octopus_export_kwh),
    kwh(day.hem_export_kwh),
    kwh(day.export_difference_kwh),
    `${day.missing_import_intervals}/${day.missing_export_intervals}/${day.missing_gas_intervals}`,
  ]);
  drawTable(doc, 'Octopus versus HEM and missing data', ['Date', 'Oct imp.', 'HEM imp.', 'Diff.', 'Oct exp.', 'HEM exp.', 'Diff.', 'Missing I/E/G'], comparisonRows, y);
  return doc;
}
