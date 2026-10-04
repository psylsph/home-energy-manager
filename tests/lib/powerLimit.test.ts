import { describe, it, expect } from 'vitest';
import { percentToWatts, formatPowerLimitLabel, rawLimitToPercent, percentToRawLimit, resolveLimitDraft, snapLimitPercent } from '../../src/lib/powerLimit';
import type { LimitScale } from '../../src/lib/powerLimit';
import vectors from '../fixtures/power-limit-vectors.json';

// ---------------------------------------------------------------------------
// Issue #346. percentToWatts turns a percentage of the inverter's maximum
// battery power into the kilowatt figure beside a slider. It must never take
// battery capacity as an input: the pack size only matters for converting a
// percentage into a register value (see the conversion tests further down),
// and clamping a capacity-derived figure to the inverter's maximum is what made
// the readout sit at the maximum across most of the slider.
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
// What the DC-hybrid HR 111/112 register means (issue #346).
//
// GivTCP is the only reference validated on hardware. Its `write.py` stores
//   target = min(watts / (capacity_wh / 2) * 50, 50)
// and its `read.py` reads back
//   watts  = min(reg / 100 * capacity_wh, inverter_max)
// so register 50 is 0.5C - a percentage of battery CAPACITY, not of the
// inverter's maximum. A Gen1 Hybrid user (9.5 kWh behind a 2.6 kW inverter) set
// the slider to 62% and it kept charging at the inverter maximum, because the
// old conversion (percent / 2) wrote 31 = 2945 W, above what the inverter can
// deliver. The slider means "% of the inverter's maximum", so the register has
// to be derived through the pack size. The direct 1-100 registers (AC-coupled,
// three-phase) are already percentages of the maximum and pass through.
//
// The expected values below come from the GivTCP formula, not from this
// module's own arithmetic.
// ---------------------------------------------------------------------------

/** GivTCP `write.py` for a DC hybrid: the register that requests `watts`. */
function givTcpRegisterFor(watts: number, capacityWh: number): number {
  return Math.min((watts / (capacityWh / 2)) * 50, 50);
}

/** GivTCP `read.py` for a DC hybrid: the watts a register actually allows. */
function givTcpWattsFor(register: number, capacityWh: number, inverterMaxW: number): number {
  return Math.min((register / 100) * capacityWh, inverterMaxW);
}

const GEN1_9_5: LimitScale = { usesDirect: false, maxWatts: 2600, capacityKwh: 9.5 };
const SMALL_5_12: LimitScale = { usesDirect: false, maxWatts: 2560, capacityKwh: 5.12 };
const DIRECT_3KW: LimitScale = { usesDirect: true, maxWatts: 3000, capacityKwh: 9.5 };

describe('percentToRawLimit (half-scale DC hybrid)', () => {
  it('writes the reporter\'s 62% on a 9.5 kWh / 2.6 kW Gen1 as register 17, not 31', () => {
    expect(percentToRawLimit(62, GEN1_9_5)).toBe(17);
    // The register the old conversion wrote asks for 2945 W, above the 2600 W
    // inverter, so it limited nothing: the bug.
    expect(givTcpWattsFor(31, 9500, 2600)).toBe(2600);
    expect(givTcpWattsFor(17, 9500, 2600)).toBeCloseTo(1615, 6);
  });

  it('matches GivTCP\'s watts-to-register formula to within one register step', () => {
    for (let percent = 1; percent <= 99; percent += 1) {
      const watts = (percent / 100) * GEN1_9_5.maxWatts;
      const expected = givTcpRegisterFor(watts, GEN1_9_5.capacityKwh * 1000);
      expect(Math.abs(percentToRawLimit(percent, GEN1_9_5) - expected)).toBeLessThanOrEqual(1);
    }
  });

  it('delivers the requested power, to within one register step (95 W on 9.5 kWh)', () => {
    for (let percent = 1; percent <= 99; percent += 1) {
      const raw = percentToRawLimit(percent, GEN1_9_5);
      const delivered = givTcpWattsFor(raw, 9500, 2600);
      const wanted = (percent / 100) * 2600;
      expect(Math.abs(delivered - wanted)).toBeLessThanOrEqual(95);
    }
  });

  it('writes 0 for 0% so charging can be stopped, and never exceeds the register range', () => {
    expect(percentToRawLimit(0, GEN1_9_5)).toBe(0);
    for (let percent = -10; percent <= 150; percent += 1) {
      const raw = percentToRawLimit(percent, GEN1_9_5);
      expect(raw).toBeGreaterThanOrEqual(0);
      expect(raw).toBeLessThanOrEqual(50);
    }
  });

  it('writes the register maximum for 100%, whatever the capacity', () => {
    // 100% means "no limit": register 50 (0.5C) is above any inverter's rating,
    // and unlike a derived 28 it cannot throttle if the capacity is misread. It
    // is also the factory default.
    for (const capacityKwh of [0, 2.6, 5.12, 9.5, 20]) {
      expect(percentToRawLimit(100, { ...GEN1_9_5, capacityKwh })).toBe(50);
    }
    expect(givTcpWattsFor(50, 9500, 2600)).toBe(2600);
  });

  it('is monotonic across the slider', () => {
    // Includes 100% -> 50, a deliberate jump past the derived 28.
    let previous = -1;
    for (let percent = 0; percent <= 100; percent += 1) {
      const raw = percentToRawLimit(percent, GEN1_9_5);
      expect(raw).toBeGreaterThanOrEqual(previous);
      previous = raw;
    }
  });

  it('keeps the old half-scale mapping on a small pack, where capacity / 2 is the maximum', () => {
    // 5.12 kWh: the maximum is capacity / 2, so register 50 really is 100%.
    expect(percentToRawLimit(100, SMALL_5_12)).toBe(50);
    expect(percentToRawLimit(62, SMALL_5_12)).toBe(31);
    expect(percentToRawLimit(50, SMALL_5_12)).toBe(25);
    expect(percentToRawLimit(0, SMALL_5_12)).toBe(0);
  });

  it('falls back to the plain half scale when the capacity or maximum is unknown', () => {
    const unknownCapacity: LimitScale = { usesDirect: false, maxWatts: 2600, capacityKwh: 0 };
    const unknownMax: LimitScale = { usesDirect: false, maxWatts: 0, capacityKwh: 9.5 };
    for (const scale of [unknownCapacity, unknownMax]) {
      expect(percentToRawLimit(100, scale)).toBe(50);
      expect(percentToRawLimit(62, scale)).toBe(31);
      expect(rawLimitToPercent(31, scale)).toBe(62);
      expect(rawLimitToPercent(50, scale)).toBe(100);
    }
  });
});

describe('a stated maximum above capacity / 2', () => {
  // The backend decoder caps the maximum at capacity / 2, but register 50 is
  // 0.5C however the maximum is stated, so it must always be the top of the
  // scale: nothing above it can be written or reached.
  const OVERSTATED: LimitScale = { usesDirect: false, maxWatts: 5000, capacityKwh: 9.5 };

  it('reads register 50 as 100% and writes 50 for 100%', () => {
    expect(rawLimitToPercent(50, OVERSTATED)).toBe(100);
    expect(percentToRawLimit(100, OVERSTATED)).toBe(50);
  });

  it('behaves like the capped maximum below the top', () => {
    const capped: LimitScale = { usesDirect: false, maxWatts: 4750, capacityKwh: 9.5 };
    for (let percent = 0; percent <= 100; percent += 1) {
      expect(percentToRawLimit(percent, OVERSTATED)).toBe(percentToRawLimit(percent, capped));
    }
  });
});

describe('rawLimitToPercent (half-scale DC hybrid)', () => {
  it('reads register 17 on a 9.5 kWh / 2.6 kW Gen1 as 62% of the inverter maximum', () => {
    expect(rawLimitToPercent(17, GEN1_9_5)).toBe(62);
  });

  it('reads anything at or above the register that reaches the maximum as 100%', () => {
    // The factory default (50) and the reporter\'s old 31 are both far above
    // what a 2.6 kW inverter can deliver on this pack.
    expect(rawLimitToPercent(28, GEN1_9_5)).toBe(100);
    expect(rawLimitToPercent(31, GEN1_9_5)).toBe(100);
    expect(rawLimitToPercent(50, GEN1_9_5)).toBe(100);
  });

  it('reads the watts GivTCP says the register allows', () => {
    for (let raw = 0; raw <= 50; raw += 1) {
      const watts = givTcpWattsFor(raw, 9500, 2600);
      expect(rawLimitToPercent(raw, GEN1_9_5)).toBe(Math.round((watts / 2600) * 100));
    }
  });

  it('clamps a corrupt register and reads 0 as 0%', () => {
    expect(rawLimitToPercent(255, GEN1_9_5)).toBe(100);
    expect(rawLimitToPercent(0, GEN1_9_5)).toBe(0);
    expect(rawLimitToPercent(-3, GEN1_9_5)).toBe(0);
  });

  it('keeps the doubled reading on a small pack', () => {
    expect(rawLimitToPercent(25, SMALL_5_12)).toBe(50);
    expect(rawLimitToPercent(50, SMALL_5_12)).toBe(100);
  });

  it('round-trips every register that is below the maximum', () => {
    for (let raw = 0; raw <= 27; raw += 1) {
      expect(percentToRawLimit(rawLimitToPercent(raw, GEN1_9_5), GEN1_9_5)).toBe(raw);
    }
  });
});

describe('snapLimitPercent', () => {
  it('snaps a slider position to the nearest percentage the register can hold', () => {
    // Registers are ~3.7% apart on this pack: 17 = 62%, 18 = 66%.
    expect(snapLimitPercent(62, GEN1_9_5)).toBe(62);
    expect(snapLimitPercent(63, GEN1_9_5)).toBe(62);
    expect(snapLimitPercent(64, GEN1_9_5)).toBe(66);
    expect(snapLimitPercent(100, GEN1_9_5)).toBe(100);
    expect(snapLimitPercent(0, GEN1_9_5)).toBe(0);
  });

  it('is the identity on a direct register', () => {
    for (const percent of [1, 33, 62, 100]) {
      expect(snapLimitPercent(percent, DIRECT_3KW)).toBe(percent);
    }
  });
});

describe('direct 1-100 registers (AC-coupled, three-phase, Gateway)', () => {
  it('pass the percentage through whatever the pack size', () => {
    for (const capacityKwh of [0, 5.12, 9.5, 20]) {
      const scale: LimitScale = { ...DIRECT_3KW, capacityKwh };
      expect(rawLimitToPercent(66, scale)).toBe(66);
      expect(percentToRawLimit(66, scale)).toBe(66);
    }
  });

  it('clamp a corrupt register and round-trip every value 1-100', () => {
    expect(rawLimitToPercent(255, DIRECT_3KW)).toBe(100);
    for (let raw = 1; raw <= 100; raw += 1) {
      expect(percentToRawLimit(rawLimitToPercent(raw, DIRECT_3KW), DIRECT_3KW)).toBe(raw);
    }
  });
});

// ---------------------------------------------------------------------------
// Cross-language contract: tests/fixtures/power-limit-vectors.json is also
// asserted by the Rust conversion (src-tauri/src/inverter/power_limit.rs), so the
// two implementations cannot drift apart.
// ---------------------------------------------------------------------------

describe('shared cross-language vectors', () => {
  for (const entry of vectors.scales) {
    const scale: LimitScale = {
      usesDirect: entry.bank === 'direct',
      maxWatts: entry.max_w,
      capacityKwh: entry.capacity_kwh,
    };
    it(`${entry.name}: writes`, () => {
      for (const write of entry.writes) {
        expect(percentToRawLimit(write.percent, scale), `${write.percent}%`).toBe(write.raw);
      }
    });
    it(`${entry.name}: reads`, () => {
      for (const read of entry.reads) {
        expect(rawLimitToPercent(read.raw, scale), `register ${read.raw}`).toBe(read.percent);
      }
    });
  }
});

describe('small percentages and implausible capacities', () => {
  it('never writes register 0 for a non-zero percentage', () => {
    // 1% of 2600 W on a 9.5 kWh pack is register 0.27; 0 would stop charging.
    for (const [maxWatts, capacityKwh] of [[2600, 9.5], [2600, 13.5], [3600, 20], [2560, 5.12]]) {
      const scale: LimitScale = { usesDirect: false, maxWatts, capacityKwh };
      expect(percentToRawLimit(0, scale)).toBe(0);
      for (let percent = 1; percent <= 99; percent += 1) {
        expect(percentToRawLimit(percent, scale), `${percent}% on ${capacityKwh} kWh`).toBeGreaterThanOrEqual(1);
      }
    }
    expect(rawLimitToPercent(1, GEN1_9_5)).toBe(4);
  });

  it('treats a capacity outside 1-150 kWh as unknown, like the backend', () => {
    // HR 55 is a raw u16 with no sanitiser on single-phase inverters, so one
    // corrupt read can report thousands of kWh.
    for (const capacityKwh of [3355, 151, 0.9, 0, Number.NaN, Number.POSITIVE_INFINITY, -5]) {
      const scale: LimitScale = { usesDirect: false, maxWatts: 2600, capacityKwh };
      expect(percentToRawLimit(40, scale), `${capacityKwh} kWh`).toBe(20);
      expect(rawLimitToPercent(20, scale), `${capacityKwh} kWh`).toBe(40);
    }
    // At the edges of the plausible range the pack size is still used.
    const at = (capacityKwh: number): LimitScale => ({ usesDirect: false, maxWatts: 2600, capacityKwh });
    expect(percentToRawLimit(40, at(9.5))).toBe(11);
    expect(percentToRawLimit(40, at(81))).toBe(1);
    expect(percentToRawLimit(40, at(150))).toBe(1);
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
    // A draft between two registers saves the nearest one and reads back as
    // a different percentage.
    expect(resolveLimitDraft({ value: 64, base: 56 }, 62)).toBe(62);
  });

  it('drops a draft made before the first snapshot arrived', () => {
    expect(resolveLimitDraft({ value: 50, base: undefined }, 66)).toBe(66);
    expect(resolveLimitDraft({ value: 50, base: undefined }, undefined)).toBe(50);
  });
});
