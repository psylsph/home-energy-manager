import {
  buildOctopusSummaryPdf,
  type OctopusExportData,
} from './octopusExport';

/** Build and download the PDF directly; no popup window is required. */
export async function downloadOctopusSummaryPdf(
  data: OctopusExportData,
  fileName: string,
): Promise<void> {
  const pdf = await buildOctopusSummaryPdf(data);
  pdf.save(fileName);
}
