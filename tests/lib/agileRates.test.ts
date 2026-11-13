import { readFileSync } from 'node:fs';
import { describe, it, expect } from 'vitest';
import { agileRatesUrl, OCTOPUS_API_BASE } from '../../src/lib/agileRates';

describe('agileRatesUrl (issue #469)', () => {
  it('uses the Kraken API host', () => {
    expect(OCTOPUS_API_BASE).toBe('https://api.oegb-kraken.energy');
    expect(agileRatesUrl('C', '2026-11-13')).toMatch(/^https:\/\/api\.oegb-kraken\.energy\/v1\/products\//);
  });

  it('carries the region and anchors to the start of the given day', () => {
    expect(agileRatesUrl('H', '2026-11-13')).toBe(
      'https://api.oegb-kraken.energy/v1/products/AGILE-24-10-01/electricity-tariffs/E-1R-AGILE-24-10-01-H/standard-unit-rates/?period_from=2026-11-13T00:00:00Z&page_size=96',
    );
  });

  it('matches the backend default so the two cannot drift apart', () => {
    const rust = readFileSync('src-tauri/src/octopus.rs', 'utf8');
    const match = /const OFFICIAL_BASE_URL: &str = "([^"]+)";/.exec(rust);
    expect(match?.[1]).toBe(OCTOPUS_API_BASE);
  });
});
