import { describe, it, expect } from 'vitest';
import {
  NO_APPLIED,
  followableParts,
  isTariffPreview,
  joinParts,
  lockedParts,
  partStatus,
  partsReplaced,
  penceText,
  updatedText,
  type TariffPreview,
} from '../../src/lib/tariffSource';

const slots = [{ start: '00:00', end: '23:59', rate: 0.245 }];

function preview(over: Partial<TariffPreview> = {}): TariffPreview {
  return {
    source: 'manual',
    applied: NO_APPLIED,
    gas_enabled: true,
    import: { ok: true, tariff_code: 'E-1R-GO', slots, standing_charge_p_per_day: 54.86 },
    export: { ok: false, reason: 'The rates change 48 times a day, too often for fixed windows (as on Agile)' },
    gas: { ok: true, tariff_code: 'G-1R-X', unit_rate_p_per_kwh: 6.1, standing_charge_p_per_day: 29.4, unit_rate_used: true },
    ...over,
  };
}

describe('lockedParts', () => {
  const applied = { import: 'E-1R-GO', export: null, gas: 'G-1R-X', at: 1 };
  it('locks only the parts Octopus filled, and only while following', () => {
    expect(lockedParts('octopus', applied)).toEqual({ import: true, export: false, gas: true });
  });
  it('locks nothing in Manual, whatever is left in applied', () => {
    expect(lockedParts('manual', applied)).toEqual({ import: false, export: false, gas: false });
  });
});

describe('followableParts / joinParts / partsReplaced', () => {
  it('lists the parts the preview could follow', () => {
    expect(followableParts(preview())).toEqual(['import', 'gas']);
    expect(followableParts(null)).toEqual([]);
  });
  it('joins names for a sentence', () => {
    expect(joinParts([])).toBe('');
    expect(joinParts(['import'])).toBe('Import');
    expect(joinParts(['import', 'gas'])).toBe('Import and Gas');
    expect(joinParts(['import', 'export', 'gas'])).toBe('Import, Export and Gas');
  });
  it('replaces only followable parts that hold typed values', () => {
    const hand = { import: true, export: true, gas: false };
    expect(partsReplaced(preview(), hand)).toEqual(['import']);
    expect(partsReplaced(preview(), { import: false, export: true, gas: false })).toEqual([]);
    expect(partsReplaced(null, hand)).toEqual([]);
  });
});

describe('partStatus', () => {
  it('names the tariff a locked part follows', () => {
    const applied = { ...NO_APPLIED, import: 'E-1R-GO' };
    expect(partStatus('import', preview(), applied)).toBe('Import follows Octopus tariff E-1R-GO');
  });
  it('gives the reason a part stays manual', () => {
    expect(partStatus('export', preview(), NO_APPLIED)).toMatch(/^Export stays manual: The rates change 48 times/);
  });
  it('copes with no preview', () => {
    expect(partStatus('gas', null, NO_APPLIED)).toBe('Gas stays manual');
  });
});

describe('updatedText', () => {
  it('formats a Unix time', () => {
    expect(updatedText(1_800_000_000, 'en-GB')).toMatch(/^Last updated \d{1,2} \w{3} 2027/);
  });
  it('is null when never or invalid', () => {
    expect(updatedText(null)).toBeNull();
    expect(updatedText(undefined)).toBeNull();
    expect(updatedText(Number.NaN)).toBeNull();
  });
});

describe('penceText', () => {
  it('is blank for none and exact otherwise', () => {
    expect(penceText(0)).toBe('');
    expect(penceText(undefined)).toBe('');
    expect(penceText(null)).toBe('');
    expect(penceText(-3)).toBe('');
    expect(penceText(54.86)).toBe('54.86');
    expect(penceText(6.1)).toBe('6.1');
  });
});

describe('isTariffPreview', () => {
  it('accepts a real preview', () => {
    expect(isTariffPreview(preview())).toBe(true);
  });
  it('rejects anything else, so an odd reply is ignored', () => {
    for (const bad of [null, undefined, 5, 'x', {}, { source: 'manual' }, { ...preview(), source: 'agile' }, { ...preview(), import: null }, { ...preview(), applied: null }]) {
      expect(isTariffPreview(bad), JSON.stringify(bad)).toBe(false);
    }
  });
});
