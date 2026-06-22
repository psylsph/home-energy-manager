import type { TariffConfig, TariffSlot } from './types';

/**
 * Parse a "HH:MM" time string into minutes since midnight.
 * Returns null if the string is malformed. "24:00" → 1440.
 */
export function parseHHMM(s: string): number | null {
  const parts = s.split(':');
  if (parts.length !== 2) return null;
  const h = parseInt(parts[0]!.trim(), 10);
  const m = parseInt(parts[1]!.trim(), 10);
  if (isNaN(h) || isNaN(m)) return null;
  if (h < 0 || h > 24 || m < 0 || m > 59) return null;
  const mins = h * 60 + m;
  if (mins > 1440) return null;
  return mins;
}

/**
 * Convert minutes since midnight to "HH:MM" string. 1440 → "24:00".
 */
export function minutesToHHMM(minutes: number): string {
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
}

/**
 * Look up the rate (£/kWh) for a given timestamp's minute of the day.
 *
 * Lookup = first slot whose [start, end) contains the minute. If no slot
 * covers the minute (tail gap), fall back to the last slot's rate.
 * Returns null if the config has no slots.
 */
export function rateForTimestamp(cfg: TariffConfig, ts: number): number | null {
  const d = new Date(ts);
  const minutes = d.getHours() * 60 + d.getMinutes();
  return rateForMinutes(cfg, minutes);
}

/**
 * Look up the rate for a given minute of the day [0, 1440).
 */
export function rateForMinutes(cfg: TariffConfig, minutes: number): number | null {
  if (cfg.slots.length === 0) return null;
  for (const slot of cfg.slots) {
    const start = parseHHMM(slot.start);
    const end = parseHHMM(slot.end);
    if (start === null || end === null) continue;
    if (end > start && minutes >= start && minutes < end) {
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
      { start: '05:30', end: '24:00', rate: 0.285 },
    ],
  };
}

/**
 * Create a flat-rate tariff config (single slot covering the whole day).
 */
export function flatTariffConfig(rate: number): TariffConfig {
  return {
    slots: [{ start: '00:00', end: '24:00', rate }],
  };
}

/**
 * Generate a list of half-hour time options from 00:00 to 24:00.
 * Used to populate <select> dropdowns for slot start/end times.
 */
export function halfHourOptions(): string[] {
  const opts: string[] = [];
  for (let h = 0; h <= 24; h++) {
    for (const m of [0, 30]) {
      if (h === 24 && m === 30) break;
      const mins = h * 60 + m;
      if (mins > 1440) break;
      opts.push(minutesToHHMM(mins));
    }
  }
  return opts;
}

/**
 * Maximum number of tariff windows (slots) per tariff (import/export).
 */
export const MAX_TARIFF_SLOTS = 6;

/**
 * Add a new slot to a tariff config, seeded from the previous slot's end.
 * Returns a new config object (immutable update).
 */
export function addTariffSlot(cfg: TariffConfig, defaultRate: number): TariffConfig {
  if (cfg.slots.length >= MAX_TARIFF_SLOTS) return cfg;
  const lastEnd = cfg.slots.length > 0
    ? cfg.slots[cfg.slots.length - 1]!.end
    : '00:00';
  return {
    slots: [...cfg.slots, { start: lastEnd, end: '24:00', rate: defaultRate }],
  };
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
  return { slots: newSlots };
}
