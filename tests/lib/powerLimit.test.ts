import { describe, it, expect } from 'vitest';
import { percentToWatts, formatPowerLimitLabel, rawLimitToPercent, percentToRawLimit, resolveLimitDraft } from '../../src/lib/powerLimit';

// ---------------------------------------------------------------------------
// Issue #346: the bracketed kW figure beside the Battery Charge / Discharge
// Power Limit sliders was derived from *battery capacity* and then clamped to
// the inverter's maximum, so on any pack above roughly half the inverter's
// rating the clamp swallowed the answer and the readout sat at the maximum for
// the whole top of the slider. A reporter set 66% and read "2.6 kW" (the
// maximum) instead of 1.7 kW.
//
// The limit registers are a percentage in both families once the DC-hybrid
// 0-50 register is doubled for display, so the only correct conversion is
// percent/100 x the inverter's maximum battery power. Battery capacity must not
// be an input at all, otherwise the clamp comes back.
// ---------------------------------------------------------------------------

describe('percentToWatts', () => {
  it('converts a percentage of the inverter maximum', () => {
    expect(percentToWatts(66, 2600)).toBe(1716);
    expect(percentToWatts(100, 2600)).toBe(2600);
    expect(percentToWatts(33, 5000)).toBe(1650);
  });

  it('keeps tracking the slider across the whole range', () => {
    // The regression: at 13.5 kWh behind a 2600 W inverter the old
    // capacity-based figure was pinned at 2600 W for every value from 39%
    // upward. Every step must now be distinct and monotonic.
    const watts = [10, 39, 40, 66, 80, 100].map((pct) => percentToWatts(pct, 2600));
    expect(watts).toEqual([260, 1014, 1040, 1716, 2080, 2600]);
    for (let i = 1; i < watts.length; i += 1) {
      expect(watts[i]).toBeGreaterThan(watts[i - 1]);
    }
  });

  it('reports 0% as 0 W and never exceeds the maximum', () => {
    expect(percentToWatts(0, 2600)).toBe(0);
    for (const pct of [0, 1, 50, 99, 100, 150]) {
      const watts = percentToWatts(pct, 2600);
      expect(watts).toBeGreaterThanOrEqual(0);
      expect(watts).toBeLessThanOrEqual(2600);
    }
  });

  it('clamps out-of-range percentages into 0-100', () => {
    expect(percentToWatts(-5, 2600)).toBe(0);
    expect(percentToWatts(140, 2600)).toBe(2600);
  });

  it('returns null when the percentage is unknown', () => {
    expect(percentToWatts(null, 2600)).toBeNull();
    expect(percentToWatts(undefined, 2600)).toBeNull();
  });

  it('returns null when the inverter maximum is unknown', () => {
    // Gateway / EMS / PV-inverter report 0, meaning "rating unknown". A
    // percentage against an unknown maximum has no watt equivalent and must
    // not be rendered as 0 kW, which would read as "no limit".
    expect(percentToWatts(66, 0)).toBeNull();
  });

  it('returns null for a non-finite percentage or maximum', () => {
    expect(percentToWatts(Number.NaN, 2600)).toBeNull();
    expect(percentToWatts(66, Number.NaN)).toBeNull();
  });
});

describe('formatPowerLimitLabel', () => {
  it('shows the percentage with the watt figure it represents', () => {
    expect(formatPowerLimitLabel(66, 1716)).toBe('66% (1.7 kW)');
    expect(formatPowerLimitLabel(100, 2600)).toBe('100% (2.6 kW)');
    expect(formatPowerLimitLabel(100, 5000)).toBe('100% (5.0 kW)');
  });

  it('shows the percentage alone when no watt figure is known', () => {
    expect(formatPowerLimitLabel(66, null)).toBe('66%');
    expect(formatPowerLimitLabel(0, null)).toBe('0%');
  });

  it('shows a dash when the percentage is unknown', () => {
    expect(formatPowerLimitLabel(null, null)).toBe('—');
    expect(formatPowerLimitLabel(undefined, 1716)).toBe('—');
  });

  it('omits the bracket when the limit is 0 W', () => {
    // 0% is a real setting, not a missing one, but "(0.0 kW)" adds nothing.
    expect(formatPowerLimitLabel(0, 0)).toBe('0%');
  });
});

// ---------------------------------------------------------------------------
// The raw register scale differs by family: HR 111/112 run 0-50 (the UI
// doubles them), while HR 313/314 and HR 1108/1110 run 1-100. Every surface
// that shows a charge/discharge limit must apply the same scale, otherwise the
// Control page and the Inverter page disagree for the same setting. Before this
// was shared, the Inverter page rendered the raw register, so a DC-hybrid user
// who set 66% saw "33%" there (issue #346).
// ---------------------------------------------------------------------------

describe('rawLimitToPercent / percentToRawLimit', () => {
  it('doubles the half-scale DC-hybrid register', () => {
    expect(rawLimitToPercent(25, false)).toBe(50);
    expect(rawLimitToPercent(33, false)).toBe(66);
    expect(rawLimitToPercent(50, false)).toBe(100);
    expect(rawLimitToPercent(0, false)).toBe(0);
  });

  it('passes the direct 1-100 register through unchanged', () => {
    expect(rawLimitToPercent(66, true)).toBe(66);
    expect(rawLimitToPercent(100, true)).toBe(100);
    expect(rawLimitToPercent(1, true)).toBe(1);
  });

  it('clamps a corrupt raw register to the family maximum', () => {
    expect(rawLimitToPercent(255, false)).toBe(100);
    expect(rawLimitToPercent(255, true)).toBe(100);
    expect(rawLimitToPercent(60, false)).toBe(100);
  });

  it('round-trips a display percentage back to the raw register', () => {
    // The direct register is lossless. The half-scale register quantises odd
    // display percentages to the nearest even one, matching the backend's
    // `div_ceil(percent, 2)` and the Control page's `round(percent / 2)`.
    for (const percent of [0, 1, 33, 50, 66, 99, 100]) {
      expect(percentToRawLimit(percent, true)).toBe(percent);
      expect(rawLimitToPercent(percentToRawLimit(percent, true), true)).toBe(percent);
    }
    for (const percent of [0, 50, 66, 100]) {
      expect(rawLimitToPercent(percentToRawLimit(percent, false), false)).toBe(percent);
    }
    expect(percentToRawLimit(66, false)).toBe(33);
    expect(percentToRawLimit(100, false)).toBe(50);
    expect(percentToRawLimit(33, false)).toBe(17);
  });

  // Exhaustive checks for the Gen1 Hybrid half scale the bug was reported on.
  it('round-trips every raw HR 111/112 value 0-50 losslessly', () => {
    for (let raw = 0; raw <= 50; raw += 1) {
      const percent = rawLimitToPercent(raw, false);
      expect(percent % 2).toBe(0);
      expect(percentToRawLimit(percent, false)).toBe(raw);
    }
  });

  it('never produces a half-scale register value outside 0-50', () => {
    for (let percent = -10; percent <= 150; percent += 1) {
      const raw = percentToRawLimit(percent, false);
      expect(raw).toBeGreaterThanOrEqual(0);
      expect(raw).toBeLessThanOrEqual(50);
    }
  });

  it('halves exactly like the backend `div_ceil(percent, 2)` for every percentage', () => {
    for (let percent = 0; percent <= 100; percent += 1) {
      expect(percentToRawLimit(percent, false)).toBe(Math.ceil(percent / 2));
    }
  });

  it('round-trips every direct register value 1-100 losslessly', () => {
    for (let raw = 1; raw <= 100; raw += 1) {
      expect(percentToRawLimit(rawLimitToPercent(raw, true), true)).toBe(raw);
    }
  });

  it('gives the Gen1 reporter (2600 W, 13.5 kWh) a distinct watt figure at every even step', () => {
    const labels = new Set<string>();
    for (let raw = 1; raw <= 50; raw += 1) {
      const percent = rawLimitToPercent(raw, false);
      const watts = percentToWatts(percent, 2600);
      expect(watts).toBe(Math.round((percent / 100) * 2600));
      labels.add(formatPowerLimitLabel(percent, watts));
    }
    expect(labels.size).toBe(50);
  });
});

describe('resolveLimitDraft', () => {
  it('shows the live value when there is no draft', () => {
    expect(resolveLimitDraft(null, 66)).toBe(66);
    expect(resolveLimitDraft(null, undefined)).toBeUndefined();
  });

  it('shows the draft while the register still holds the value it was edited from', () => {
    // Saved but not yet read back: the slider must not jump back.
    expect(resolveLimitDraft({ value: 80, base: 66 }, 66)).toBe(80);
  });

  it('hands over to the live value once the save is read back', () => {
    expect(resolveLimitDraft({ value: 80, base: 66 }, 80)).toBe(80);
  });

  it('follows an external change made after the save', () => {
    expect(resolveLimitDraft({ value: 80, base: 66 }, 20)).toBe(20);
  });

  it('does not stick when the read-back can never equal the draft', () => {
    // An odd draft on the half scale saves 17 → reads back 34%.
    expect(resolveLimitDraft({ value: 33, base: 66 }, 34)).toBe(34);
  });

  it('drops a draft made before the first snapshot arrived', () => {
    expect(resolveLimitDraft({ value: 50, base: undefined }, 66)).toBe(66);
    expect(resolveLimitDraft({ value: 50, base: undefined }, undefined)).toBe(50);
  });
});
