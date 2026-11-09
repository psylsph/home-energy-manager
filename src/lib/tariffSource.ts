/** Helpers for Energy Tariffs following the rates Octopus charges (issue #455). */
import type { TariffConfig } from './types';

export type TariffSource = 'manual' | 'octopus';

/** The tariff code each part last followed; a part that is set is locked while following. */
export interface AppliedParts {
  import: string | null;
  export: string | null;
  gas: string | null;
  at: number | null;
}

export const NO_APPLIED: AppliedParts = { import: null, export: null, gas: null, at: null };

export type PartName = 'import' | 'export' | 'gas';

export interface TariffPart {
  ok: true;
  tariff_code: string;
  slots: TariffConfig['slots'];
  standing_charge_p_per_day: number | null;
}

export interface GasPart {
  ok: true;
  tariff_code: string;
  unit_rate_p_per_kwh: number;
  standing_charge_p_per_day: number | null;
  /** False when the gas readings aren't known to be kWh, so no unit rate is filled. */
  unit_rate_used: boolean;
}

export interface NotFollowable {
  ok: false;
  reason: string;
}

/** What `GET /api/octopus/tariff-preview` (and the source switch) returns. */
export interface TariffPreview {
  source: TariffSource;
  applied: AppliedParts;
  gas_enabled: boolean;
  import: TariffPart | NotFollowable;
  export: TariffPart | NotFollowable;
  gas: GasPart | NotFollowable;
}

export interface TariffPreviewResponse {
  ok: boolean;
  data: TariffPreview;
}

/** Whether `value` has the shape of a preview, so an unexpected response is ignored rather than trusted. */
export function isTariffPreview(value: unknown): value is TariffPreview {
  if (typeof value !== 'object' || value === null) return false;
  const v = value as Record<string, unknown>;
  const part = (x: unknown) => typeof x === 'object' && x !== null && typeof (x as { ok?: unknown }).ok === 'boolean';
  return (
    (v.source === 'manual' || v.source === 'octopus') &&
    typeof v.applied === 'object' &&
    v.applied !== null &&
    part(v.import) &&
    part(v.export) &&
    part(v.gas)
  );
}

export const PART_LABELS: Record<PartName, string> = {
  import: 'Import',
  export: 'Export',
  gas: 'Gas',
};

/** Which parts are read-only: only those the last Octopus fill wrote, and only while following. */
export function lockedParts(source: TariffSource, applied: AppliedParts): Record<PartName, boolean> {
  const following = source === 'octopus';
  return {
    import: following && applied.import != null,
    export: following && applied.export != null,
    gas: following && applied.gas != null,
  };
}

/** The parts the preview could follow, in display order. */
export function followableParts(preview: TariffPreview | null): PartName[] {
  if (!preview) return [];
  return (['import', 'export', 'gas'] as const).filter((p) => preview[p].ok);
}

/** "Import and Export" / "Import, Export and Gas". */
export function joinParts(parts: PartName[]): string {
  const names = parts.map((p) => PART_LABELS[p]);
  if (names.length <= 1) return names.join('');
  return `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`;
}

/**
 * The parts switching to Octopus would overwrite: followable ones that hold
 * hand-entered values.
 */
export function partsReplaced(
  preview: TariffPreview | null,
  handEntered: Record<PartName, boolean>,
): PartName[] {
  return followableParts(preview).filter((p) => handEntered[p]);
}

/** "Following Octopus tariff E-1R-GO" for a locked part, else why it isn't followed. */
export function partStatus(part: PartName, preview: TariffPreview | null, applied: AppliedParts): string {
  const label = PART_LABELS[part];
  const code = applied[part];
  if (code) return `${label} follows Octopus tariff ${code}`;
  const p = preview?.[part];
  if (p && !p.ok) return `${label} stays manual: ${p.reason}`;
  return `${label} stays manual`;
}

/** "Last updated 9 Nov 2026, 07:30" from Unix seconds, or null when never. */
export function updatedText(at: number | null | undefined, locale?: string): string | null {
  if (at == null || !Number.isFinite(at)) return null;
  const when = new Date(at * 1000).toLocaleString(locale, {
    day: 'numeric',
    month: 'short',
    year: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
  return `Last updated ${when}`;
}

/** Pence as the text of an input: blank for none, no trailing zeros. */
export function penceText(value: number | undefined | null): string {
  return value != null && value > 0 ? String(value) : '';
}
