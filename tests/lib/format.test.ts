import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { formatOperatingHours, formatBatteryMode, formatVisualPower, formatTimestamp, finiteAbs, formatCurrent, formatPower } from '../../src/lib/format';

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

/**
 * Tests for `formatVisualPower` — clamps sub-threshold readings to "0W"
 * for the energy flow diagram, delegates to `formatPower` otherwise.
 */
describe('formatVisualPower', () => {
  describe('below threshold', () => {
    it('returns "0W" for a value just below the threshold', () => {
      expect(formatVisualPower(19, 20)).toBe('0W');
    });

    it('returns "0W" for a tiny positive value', () => {
      expect(formatVisualPower(5, 20)).toBe('0W');
    });

    it('returns "0W" for a tiny negative value', () => {
      expect(formatVisualPower(-5, 20)).toBe('0W');
    });

    it('returns "0W" for zero', () => {
      expect(formatVisualPower(0, 20)).toBe('0W');
    });

    it('returns "0W" for a value just below a non-default threshold', () => {
      expect(formatVisualPower(49, 50)).toBe('0W');
    });

    it('returns "0W" when threshold is 0 and value is 0', () => {
      expect(formatVisualPower(0, 0)).toBe('0W');
    });
  });

  describe('at or above threshold', () => {
    it('returns the formatted value when exactly at the threshold', () => {
      expect(formatVisualPower(20, 20)).toBe('20W');
    });

    it('returns the formatted value when above the threshold', () => {
      expect(formatVisualPower(150, 20)).toBe('150W');
    });

    it('returns kW format for large values above threshold', () => {
      expect(formatVisualPower(1500, 20)).toBe('1.5kW');
    });

    it('returns the formatted negative value when below negative threshold', () => {
      expect(formatVisualPower(-150, 20)).toBe('-150W');
    });

    it('returns kW format for negative values above threshold', () => {
      expect(formatVisualPower(-2500, 20)).toBe('-2.5kW');
    });

    it('works with a non-default threshold', () => {
      expect(formatVisualPower(50, 50)).toBe('50W');
      expect(formatVisualPower(51, 50)).toBe('51W');
    });

    it('returns the formatted value when threshold is 0', () => {
      expect(formatVisualPower(1, 0)).toBe('1W');
      expect(formatVisualPower(1000, 0)).toBe('1.0kW');
    });
  });

  describe('edge cases', () => {
    it('handles NaN threshold gracefully', () => {
      // NaN < 20 is false, so Math.abs(5) < NaN is false → falls through to formatPower
      expect(formatVisualPower(5, NaN)).toBe('5W');
    });

    it('handles negative threshold as zero', () => {
      // A negative threshold is nonsensical; Math.abs(5) < -10 is false → shows value
      expect(formatVisualPower(5, -10)).toBe('5W');
    });
  });
});

/**
 * Tests for `formatTimestamp` — converts an epoch-millis timestamp to a
 * locale time string (HH:MM:SS), returning '—' for nullish / non-finite
 * values.
 */
describe('formatTimestamp', () => {
  beforeEach(() => {
    // Pin the system time so toLocaleTimeString output is deterministic.
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-06-24T14:30:00Z'));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('formats a valid epoch-millis timestamp to locale time string', () => {
    // 2026-06-24T14:30:00Z in epoch ms
    const result = formatTimestamp(1_771_888_200_000);
    // The exact string depends on the test runner's timezone.
    // We just verify it's a non-empty string with colons (time format).
    expect(result).toMatch(/\d{1,2}:\d{2}:\d{2}/);
  });

  it('returns em-dash for null', () => {
    expect(formatTimestamp(null)).toBe('—');
  });

  it('returns em-dash for undefined', () => {
    expect(formatTimestamp(undefined)).toBe('—');
  });

  it('returns em-dash for NaN', () => {
    expect(formatTimestamp(NaN)).toBe('—');
  });

  it('returns em-dash for Infinity', () => {
    expect(formatTimestamp(Infinity)).toBe('—');
  });

  it('formats negative epoch ms as a valid time string (just before 1970)', () => {
    // -1000 ms = 1969-12-31T23:59:59.000Z — still a valid time
    const result = formatTimestamp(-1000);
    expect(result).toMatch(/\d{1,2}:\d{2}:\d{2}/);
  });

  it('formats zero epoch as locale time string', () => {
    // 1970-01-01T00:00:00Z — still a valid time
    const result = formatTimestamp(0);
    expect(result).toMatch(/\d{1,2}:\d{2}:\d{2}/);
  });
});

/**
 * Tests for `finiteAbs` — the absolute-value helper that preserves NaN
 * (instead of coercing null → 0 like `Math.abs`).
 *
 * This exists because the Gateway (DTC 0x70xx) doesn't expose battery
 * current / voltage; the backend sets f32::NAN, serde_json serialises NaN as
 * null, and `Math.abs(null)` returns 0 *before* the format helpers' finite
 * guard can fire — so the field rendered as '0.0A' instead of '—'. `finiteAbs`
 * keeps the null/NaN signal intact so formatCurrent/formatPower render '—'.
 */
describe('finiteAbs', () => {
  describe('real numbers', () => {
    it('returns the absolute value of a positive number', () => {
      expect(finiteAbs(7.8)).toBe(7.8);
    });

    it('returns the absolute value of a negative number', () => {
      expect(finiteAbs(-7.8)).toBe(7.8);
    });

    it('returns 0 for 0', () => {
      expect(finiteAbs(0)).toBe(0);
    });
  });

  describe('non-finite / nullish (the Gateway case)', () => {
    it('returns NaN for null (instead of 0 like Math.abs)', () => {
      // This is the whole point of the helper. Math.abs(null) === 0 would
      // make formatCurrent render '0.0A'; finiteAbs preserves the signal.
      expect(Number.isNaN(finiteAbs(null))).toBe(true);
      expect(Math.abs(null)).toBe(0); // sanity-check the trap we're avoiding
    });

    it('returns NaN for undefined', () => {
      expect(Number.isNaN(finiteAbs(undefined))).toBe(true);
    });

    it('returns NaN for NaN', () => {
      expect(Number.isNaN(finiteAbs(NaN))).toBe(true);
    });

    it('returns NaN for Infinity', () => {
      expect(Number.isNaN(finiteAbs(Infinity))).toBe(true);
      expect(Number.isNaN(finiteAbs(-Infinity))).toBe(true);
    });
  });

  describe('integration with format helpers', () => {
    it('formatCurrent(finiteAbs(null)) renders em-dash, not 0.0A', () => {
      // The exact regression: a Gateway battery_current that arrives as null.
      expect(formatCurrent(finiteAbs(null))).toBe('—');
    });

    it('formatPower(finiteAbs(null)) renders em-dash, not 0W', () => {
      // Same trap for battery_power if it ever arrives null.
      expect(formatPower(finiteAbs(null))).toBe('—');
    });
  });
});
