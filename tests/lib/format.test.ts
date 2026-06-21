import { describe, it, expect } from 'vitest';
import { formatOperatingHours, formatBatteryMode } from '../../src/lib/format';

/**
 * Tests for `formatOperatingHours` — the helper that turns a raw
 * lifetime-hours figure (IR(47-48) work_time_total) into a human-friendly
 * age string for the Inverter page.
 *
 * Boundaries match the unit ladder in the production code:
 *   < 24 h          -> "Nh"
 *   < 7 days        -> "Nd"
 *   < 5 weeks       -> "Nw"
 *   < 12 months     -> "Nmo"
 *   otherwise       -> "Ny" or "Ny Mm"
 */
describe('formatOperatingHours', () => {
  describe('zero / invalid input', () => {
    it('returns empty string for 0', () => {
      expect(formatOperatingHours(0)).toBe('');
    });

    it('returns empty string for negative input', () => {
      // Defensive: a corrupted register should never show as "−5h".
      expect(formatOperatingHours(-100)).toBe('');
    });

    it('returns empty string for NaN / Infinity', () => {
      expect(formatOperatingHours(NaN)).toBe('');
      expect(formatOperatingHours(Infinity)).toBe('');
    });
  });

  describe('hours ladder', () => {
    it.each([
      [1, '1h'],
      [12, '12h'],
      [23, '23h'],
    ])('%i h -> "%s"', (input, expected) => {
      expect(formatOperatingHours(input)).toBe(expected);
    });
  });

  describe('days ladder', () => {
    it.each([
      [24, '1d'],
      [48, '2d'],
      [72, '3d'],
      [167, '7d'], // 167 h ≈ 7 days
    ])('%i h -> "%s"', (input, expected) => {
      expect(formatOperatingHours(input)).toBe(expected);
    });
  });

  describe('weeks ladder', () => {
    it.each([
      [168, '1w'], // exactly 7 days
      [336, '2w'],
      [720, '4w'], // 30 days (round(720/168) = 4)
      [839, '5w'], // last value in the weeks ladder (< 5 * 168 = 840)
    ])('%i h -> "%s"', (input, expected) => {
      expect(formatOperatingHours(input)).toBe(expected);
    });
  });

  describe('months ladder', () => {
    // The weeks ladder covers < 5 * 168 = 840 h, so values from 840
    // onward land in months.
    it.each([
      [840, '1mo'], // 35 days = first value in the months ladder
      [1440, '2mo'],
      [4320, '6mo'],
      [8640, '12mo'], // just under one year (8766 h)
    ])('%i h -> "%s"', (input, expected) => {
      expect(formatOperatingHours(input)).toBe(expected);
    });
  });

  describe('years ladder', () => {
    it('rounds exact 1-year to "1y"', () => {
      // 8766 h = 365.25 days. Past the 12-month threshold, so years apply.
      // Floor: 1 year. Remaining: ~0. Months rounds to 0 -> "1y".
      expect(formatOperatingHours(8766)).toBe('1y');
    });

    it('formats 3-year as "3y"', () => {
      // 26_298 h ≈ 3.0 years exactly. Floor: 3. Remaining: 0.
      expect(formatOperatingHours(26_298)).toBe('3y');
    });

    it('formats 3y 4m correctly', () => {
      // 3.33 years = 3y + 4m (29_160 h = 3 * 8766 + 4 * 730.5)
      const hours = 3 * 8766 + 4 * 730.5;
      const result = formatOperatingHours(hours);
      // Allow ±1 month rounding slack.
      expect(result).toMatch(/^3y\s\d+m$/);
    });

    it('formats 9y 1m for ~80 000 h', () => {
      // 80 000 / 8766 ≈ 9.13 years → 9y + 1-2m
      const result = formatOperatingHours(80_000);
      expect(result).toMatch(/^9y\s[12]m$/);
    });

    it('does not show trailing 0 months', () => {
      // A perfectly-aligned reading (e.g. exactly N years) must not
      // render as "Ny 0m".
      expect(formatOperatingHours(2 * 8766)).toBe('2y');
    });
  });
});

/**
 * Tests for `formatBatteryMode` — converts the snake_case wire format
 * (eco / eco_paused / timed_export / ...) into Upper Camel Case for
 * display on the Inverter page.
 *
 * The helper must be tolerant of unknown / future values (the backend
 * could grow new modes without a frontend update), so a forward-compat
 * test is included to lock that behaviour.
 */
describe('formatBatteryMode', () => {
  describe('known battery_mode values', () => {
    it.each([
      ['eco', 'Eco'],
      ['eco_paused', 'EcoPaused'],
      ['timed_demand', 'TimedDemand'],
      ['timed_export', 'TimedExport'],
      ['export_paused', 'ExportPaused'],
      ['unknown', 'Unknown'],
    ])('%s -> %s', (input, expected) => {
      expect(formatBatteryMode(input)).toBe(expected);
    });
  });

  describe('forward compatibility', () => {
    it('upper-camels any snake_case value the backend may add later', () => {
      // If the backend grows a new mode, the UI should degrade
      // gracefully to a readable form rather than crashing or showing
      // the raw snake_case string.
      expect(formatBatteryMode('foo_bar_baz')).toBe('FooBarBaz');
      expect(formatBatteryMode('force_charge')).toBe('ForceCharge');
    });

    it('handles a single-word value with no underscores', () => {
      expect(formatBatteryMode('cosy')).toBe('Cosy');
    });

    it('lower-cases mid-word characters', () => {
      // Defensive: if a future mode name has mixed case on the wire
      // (e.g. "Eco_Paused"), the helper normalises to UpperCamel.
      expect(formatBatteryMode('Eco_Paused')).toBe('EcoPaused');
    });
  });

  describe('null / undefined / empty', () => {
    it('returns em-dash for undefined', () => {
      expect(formatBatteryMode(undefined)).toBe('—');
    });

    it('returns em-dash for null', () => {
      expect(formatBatteryMode(null)).toBe('—');
    });

    it('returns em-dash for empty string', () => {
      expect(formatBatteryMode('')).toBe('—');
    });

    it('returns em-dash for an all-underscores string', () => {
      // split('_').filter(Boolean) yields [], which is treated as
      // missing — same behaviour as undefined.
      expect(formatBatteryMode('___')).toBe('—');
    });
  });
});
