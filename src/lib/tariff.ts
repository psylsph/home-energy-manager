import type { TariffConfig, TariffSlot } from './types';

/**
 * Parse a "HH:MM" time string into minutes since midnight.
 * Returns null if the string is malformed. The latest representable clock
 * time is "23:59" (minute 1439); "24:00" is rejected (it's not a real time).
 */
export function parseHHMM(s: string): number | null {
  const parts = s.split(':');
  if (parts.length !== 2) return null;
  const h = parseInt(parts[0]!.trim(), 10);
  const m = parseInt(parts[1]!.trim(), 10);
  if (isNaN(h) || isNaN(m)) return null;
  if (h < 0 || h > 23 || m < 0 || m > 59) return null;
  return h * 60 + m;
}

/**
 * Convert minutes since midnight to "HH:MM" string. Minutes are clamped
 * to `[0, 1439]` so the maximum is "23:59".
 */
export function minutesToHHMM(minutes: number): string {
  const clamped = Math.min(Math.max(minutes, 0), 23 * 60 + 59);
  const h = Math.floor(clamped / 60);
  const m = clamped % 60;
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
}

/**
 * Look up the rate (£/kWh) for a given timestamp's minute of the day.
 *
 * Lookup = first slot whose `[start, end)` contains the minute. The final
 * slot is closed `[start, end]` so its `end = "23:59"` actually covers
 * minute 1439 (the last minute of the day). If no slot covers the minute
 * (tail gap), fall back to the last slot's rate.
 * Returns null if the config has no slots.
 */
export function rateForTimestamp(cfg: TariffConfig, ts: number): number | null {
  const d = new Date(ts);
  const minutes = d.getHours() * 60 + d.getMinutes();
  return rateForMinutes(cfg, minutes);
}

/**
 * Look up the rate for a given minute of the day [0, 1440).
 *
 * Intermediate slots are half-open `[start, end)`; the final slot is
 * closed `[start, end]` so "23:59" covers minute 1439.
 */
export function rateForMinutes(cfg: TariffConfig, minutes: number): number | null {
  if (cfg.slots.length === 0) return null;
  const lastIdx = cfg.slots.length - 1;
  for (let i = 0; i < cfg.slots.length; i++) {
    const slot = cfg.slots[i]!;
    const start = parseHHMM(slot.start);
    const end = parseHHMM(slot.end);
    if (start === null || end === null) continue;
    if (i === lastIdx) {
      // Final slot: closed [start, end] so "23:59" (1439) is included.
      // Defend against malformed input where end < start.
      if (end >= start && minutes >= start && minutes <= end) {
        return slot.rate;
      }
    } else if (end > start && minutes >= start && minutes < end) {
      return slot.rate;
    }
  }
  // Tail gap fallback: use the last slot's rate.
  return cfg.slots[cfg.slots.length - 1]!.rate;
}

/**
 * Default tariff config (same rates as the old peak/off-peak model).
 * Peak 28.5p, off-peak 9p, off-peak 00:30–05:30.
 */
export function defaultTariffConfig(): TariffConfig {
  return {
    slots: [
      { start: '00:00', end: '00:30', rate: 0.285 },
      { start: '00:30', end: '05:30', rate: 0.09 },
      { start: '05:30', end: '23:59', rate: 0.285 },
    ],
  };
}

/**
 * Create a flat-rate tariff config (single slot covering the whole day).
 */
export function flatTariffConfig(rate: number): TariffConfig {
  return {
    slots: [{ start: '00:00', end: '23:59', rate }],
  };
}

/**
 * Generate a list of half-hour time options from 00:00 to 23:30 plus the
 * final option "23:59" (used for the last slot's inclusive end).
 * Used to populate <select> dropdowns for slot start/end times.
 */
export function halfHourOptions(): string[] {
  const opts: string[] = [];
  for (let h = 0; h < 24; h++) {
    for (const m of [0, 30]) {
      opts.push(minutesToHHMM(h * 60 + m));
    }
  }
  // Final inclusive end-of-day marker — the last slot ends here (inclusive).
  opts.push('23:59');
  return opts;
}

/**
 * Maximum number of tariff windows (slots) per tariff (import/export).
 */
export const MAX_TARIFF_SLOTS = 6;

/**
 * Add a new tariff window by **splitting** the longest existing window at
 * its midpoint. This keeps the day tiled without creating a dead-end state
 * where the new slot is stuck at an unreachable start time. The new slot
 * is seeded with the supplied `defaultRate` so a flat-rate user can switch
 * to a different rate for the new window without editing afterwards.
 *
 * The midpoint is biased to the nearest half-hour, with a minimum 30-minute
 * width for both halves so the user always has a usable range to adjust.
 *
 * Returns a new config object (immutable update). If `MAX_TARIFF_SLOTS` is
 * already reached, the config is returned unchanged.
 */
export function addTariffSlot(cfg: TariffConfig, defaultRate: number): TariffConfig {
  if (cfg.slots.length >= MAX_TARIFF_SLOTS) return cfg;
  if (cfg.slots.length === 0) {
    // Defensive: empty list shouldn't happen (validation prevents it), but
    // produce a sensible flat rate rather than crashing.
    return flatTariffConfig(defaultRate);
  }

  // Find the longest slot — that's the most useful one to split.
  let longestIdx = 0;
  let longestWidth = -1;
  for (let i = 0; i < cfg.slots.length; i++) {
    const s = parseHHMM(cfg.slots[i]!.start);
    const e = parseHHMM(cfg.slots[i]!.end);
    if (s === null || e === null) continue;
    const width = e - s;
    if (width > longestWidth) {
      longestWidth = width;
      longestIdx = i;
    }
  }

  const longest = cfg.slots[longestIdx]!;
  const startMin = parseHHMM(longest.start);
  const endMin = parseHHMM(longest.end);
  if (startMin === null || endMin === null || endMin - startMin < 60) {
    // Can't split a window narrower than 1 hour into two valid halves —
    // fall back to appending a window at the end (rare edge case).
    const lastEnd = cfg.slots.at(-1)!.end;
    return {
      slots: [
        ...cfg.slots,
        { start: lastEnd, end: '23:59', rate: defaultRate },
      ],
    };
  }

  // Midpoint, biased to half-hour granularity and clamped so both halves
  // have at least 30 minutes.
  const midMin = Math.max(
    startMin + 30,
    Math.min(endMin - 30, Math.round((startMin + endMin) / 2 / 30) * 30),
  );
  const mid = minutesToHHMM(midMin);

  const newSlots = [...cfg.slots];
  newSlots[longestIdx] = { ...longest, end: mid };
  newSlots.splice(longestIdx + 1, 0, {
    start: mid,
    end: longest.end,
    rate: defaultRate,
  });
  return { slots: newSlots };
}

/**
 * Remove a slot at the given index, extending the previous slot's end
 * to the deleted slot's end (day remains tiled).
 * If deleting the first slot, the second slot's start becomes "00:00".
 */
export function removeTariffSlot(cfg: TariffConfig, index: number): TariffConfig {
  if (cfg.slots.length <= 1) {
    // Can't delete the only slot — return a flat default.
    return flatTariffConfig(cfg.slots[0]?.rate ?? 0.285);
  }
  const newSlots = [...cfg.slots];
  const removed = newSlots.splice(index, 1)[0]!;
  if (index === 0) {
    // First slot deleted: second slot (now first) starts at 00:00.
    newSlots[0] = { ...newSlots[0]!, start: '00:00' };
  } else if (index - 1 < newSlots.length) {
    // Extend the previous slot's end to the deleted slot's end.
    newSlots[index - 1] = { ...newSlots[index - 1]!, end: removed.end };
  }
  return { slots: newSlots };
}

/**
 * Update a single field of a slot at the given index.
 *
 * For `end` edits on a non-final slot, the next slot's `start` is
 * **cascaded** to match so the tiling stays contiguous. This is what
 * makes the disabled start-select work: the user only ever edits an end,
 * and every later slot's start follows along automatically.
 */
export function updateTariffSlot(
  cfg: TariffConfig,
  index: number,
  field: keyof TariffSlot,
  value: string | number,
): TariffConfig {
  const newSlots = cfg.slots.map((slot, i) =>
    i === index ? { ...slot, [field]: value } : slot,
  );
  // Cascade end-change → next slot's start so the day stays tiled.
  if (field === 'end' && index + 1 < newSlots.length) {
    const newEnd = value as string;
    newSlots[index + 1] = { ...newSlots[index + 1]!, start: newEnd };
  }
  return { slots: newSlots };
}

/**
 * Validation error returned by {@link validateTariffConfig}.
 *
 * `slotIndex` is the index of the offending slot (or -1 for config-level
 * issues that don't apply to a single slot, e.g. "empty list").
 */
export interface TariffValidationError {
  slotIndex: number;
  field: 'start' | 'end' | 'rate' | 'config';
  message: string;
}

/** Inclusive end-of-day clock time expressed as minutes since midnight. */
export const FINAL_SLOT_END_MINUTES = 23 * 60 + 59; // 1439

/**
 * Validate a {@link TariffConfig} for well-formedness.
 *
 * Rules:
 * 1. The slot list must not be empty.
 * 2. The first slot must start at `"00:00"` and the last must end at
 *    `"23:59"` (the day must be fully tiled with no gaps).
 * 3. Slots must be in ascending order with no overlaps: each slot's start
 *    must equal the previous slot's end (contiguous tiling).
 * 4. Within a slot, `start <= end` and both must parse as valid HH:MM times
 *    in `[00:00, 23:59]`.
 * 5. Rates must be finite, non-negative numbers.
 *
 * Returns an empty array if the config is valid.
 */
export function validateTariffConfig(cfg: TariffConfig): TariffValidationError[] {
  const errors: TariffValidationError[] = [];

  if (cfg.slots.length === 0) {
    errors.push({
      slotIndex: -1,
      field: 'config',
      message: 'At least one tariff window is required.',
    });
    return errors;
  }

  const parsed = cfg.slots.map((slot, i) => {
    const start = parseHHMM(slot.start);
    const end = parseHHMM(slot.end);
    return { i, slot, start, end };
  });

  // Rule 4: parseable times and start <= end.
  parsed.forEach(({ i, slot, start, end }) => {
    if (start === null) {
      errors.push({
        slotIndex: i,
        field: 'start',
        message: `Start time "${slot.start}" is not a valid HH:MM time.`,
      });
    }
    if (end === null) {
      errors.push({
        slotIndex: i,
        field: 'end',
        message: `End time "${slot.end}" is not a valid HH:MM time.`,
      });
    }
    if (start !== null && end !== null && start > end) {
      errors.push({
        slotIndex: i,
        field: 'end',
        message: `End time (${slot.end}) must be at or after start time (${slot.start}).`,
      });
    }
    if (start !== null && start < 0) {
      errors.push({
        slotIndex: i,
        field: 'start',
        message: `Start time cannot be negative.`,
      });
    }
    if (start !== null && start > FINAL_SLOT_END_MINUTES) {
      errors.push({
        slotIndex: i,
        field: 'start',
        message: `Start time cannot be later than 23:59.`,
      });
    }
  });

  // Rule 5: rates must be finite non-negative numbers.
  parsed.forEach(({ i, slot }) => {
    if (typeof slot.rate !== 'number' || !Number.isFinite(slot.rate)) {
      errors.push({
        slotIndex: i,
        field: 'rate',
        message: 'Rate must be a finite number.',
      });
    } else if (slot.rate < 0) {
      errors.push({
        slotIndex: i,
        field: 'rate',
        message: 'Rate cannot be negative.',
      });
    }
  });

  // Skip coverage / contiguity checks if individual parse errors would
  // make them misleading. We can still detect ascending order below.
  const allParsed = parsed.every((p) => p.start !== null && p.end !== null);

  // Rule 2/3 (only if all slots parseable): ascending order, contiguous
  // tiling, starts at 00:00, ends at 23:59.
  if (allParsed) {
    // First slot must start at 00:00.
    if (parsed[0]!.start !== 0) {
      errors.push({
        slotIndex: 0,
        field: 'start',
        message: `The first window must start at 00:00 (currently ${parsed[0]!.slot.start}).`,
      });
    }
    // Last slot must end at 23:59.
    const last = parsed[parsed.length - 1]!;
    if (last.end !== FINAL_SLOT_END_MINUTES) {
      errors.push({
        slotIndex: last.i,
        field: 'end',
        message: `The last window must end at 23:59 (currently ${last.slot.end}).`,
      });
    }
    // Contiguous tiling + ascending order: each slot's start == prev end,
    // and prev end < this start is impossible (we already enforced order
    // via contiguity check, so ascending follows).
    for (let i = 1; i < parsed.length; i++) {
      const prev = parsed[i - 1]!;
      const curr = parsed[i]!;
      if (curr.start !== prev.end) {
        if (curr.start! > prev.end!) {
          errors.push({
            slotIndex: i,
            field: 'start',
            message: `Gap between window ${i} (ends ${prev.slot.end}) and window ${i + 1} (starts ${curr.slot.start}). Windows must cover the full 24 hours contiguously.`,
          });
        } else {
          errors.push({
            slotIndex: i,
            field: 'start',
            message: `Window ${i + 1} (starts ${curr.slot.start}) overlaps the previous window (ends ${prev.slot.end}).`,
          });
        }
      }
    }
  }

  return errors;
}

/**
 * Convenience: returns true if {@link validateTariffConfig} produces no
 * errors.
 */
export function isTariffConfigValid(cfg: TariffConfig): boolean {
  return validateTariffConfig(cfg).length === 0;
}