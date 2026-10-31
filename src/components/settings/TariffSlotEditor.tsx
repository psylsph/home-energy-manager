import type { TariffConfig } from '../../lib/types';
import {
  flatTariffConfig,
  addTariffSlot,
  removeTariffSlot,
  updateTariffSlot,
  halfHourOptions,
  MAX_TARIFF_SLOTS,
  validateTariffConfig,
  isTariffConfigValid,
  FINAL_SLOT_END_MINUTES,
  parseHHMM,
} from '../../lib/tariff';

const TIME_OPTIONS = halfHourOptions();

/** Reusable tariff time-window editor for import/export tariffs. */
export default function TariffSlotEditor({
  label,
  config,
  onChange,
}: {
  label: string;
  config: TariffConfig;
  onChange: (cfg: TariffConfig) => void;
}) {
  const errors = validateTariffConfig(config);
  // Index validation errors by slot for inline display.
  const errorsBySlot = new Map<number, string[]>();
  for (const err of errors) {
    if (err.slotIndex < 0) continue;
    const list = errorsBySlot.get(err.slotIndex) ?? [];
    list.push(err.message);
    errorsBySlot.set(err.slotIndex, list);
  }

  // Allowed options for a slot's end select: must be ≥ slot start.
  // The end is freely selectable up to 23:59 — changing it cascades
  // forward to update the next slot's start, keeping the day tiled.
  const optionsForEnd = (slot: { start: string; end: string }) => {
    const startMin = parseHHMM(slot.start) ?? 0;
    return TIME_OPTIONS.filter((t) => {
      const m = parseHHMM(t);
      if (m === null) return false;
      return m >= startMin;
    });
  };

  // Allowed options for a slot's start select: must be ≥ previous slot's
  // end (or `00:00` if first) AND ≤ this slot's end. Prevents the user
  // from creating gaps or overlaps when editing start directly.
  const optionsForStart = (i: number, slot: { start: string; end: string }) => {
    const prevEndMin =
      i > 0
        ? parseHHMM(config.slots[i - 1]!.end) ?? 0
        : 0;
    const endMin = parseHHMM(slot.end) ?? FINAL_SLOT_END_MINUTES;
    return TIME_OPTIONS.filter((t) => {
      const m = parseHHMM(t);
      if (m === null) return false;
      return m >= prevEndMin && m <= endMin;
    });
  };

  return (
    <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
      <div className="flex items-center justify-between">
        <h3 className="text-text-primary text-sm font-sans font-medium">{label}</h3>
        {config.slots.length < MAX_TARIFF_SLOTS && (
          <button
            onClick={() => onChange(addTariffSlot(config, 0.15))}
            className="text-accent text-xs font-sans hover:opacity-80 transition-opacity"
          >
            + Add Window
          </button>
        )}
      </div>

      {config.slots.map((slot, i) => {
        const isFirst = i === 0;
        const isLast = i === config.slots.length - 1;
        const slotErrors = errorsBySlot.get(i) ?? [];
        // The end <select> is locked to `23:59` for the last slot so the day
        // is always tiled through to the end. Intermediate slots allow
        // choosing any end from the slot's start onward — changing it
        // cascades forward to update the next slot's start automatically.
        const endOptions = optionsForEnd(slot);
        return (
          <div key={i} className="flex flex-col gap-1">
            <div className="grid grid-cols-2 sm:grid-cols-[1fr_1fr_1fr_auto] gap-2 items-end">
              <label className="flex flex-col gap-1">
                <span className="text-text-secondary text-xs font-sans">Start</span>
                <select
                  value={slot.start}
                  // First slot is locked to 00:00 (the day must start at
                  // midnight). All other slots can have their start edited
                  // directly — changing it cascades backward to update the
                  // previous slot's end, keeping the day tiled.
                  onChange={(e) => onChange(updateTariffSlot(config, i, 'start', e.target.value))}
                  disabled={isFirst}
                  className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-accent outline-none transition-colors disabled:opacity-40"
                >
                  {isFirst
                    ? <option value={slot.start}>{slot.start}</option>
                    : optionsForStart(i, slot).map((t) => (
                        <option key={t} value={t}>{t}</option>
                      ))
                  }
                </select>
              </label>
              <label className="flex flex-col gap-1">
                <span className="text-text-secondary text-xs font-sans">End</span>
                <select
                  value={slot.end}
                  onChange={(e) => onChange(updateTariffSlot(config, i, 'end', e.target.value))}
                  disabled={isLast}
                  className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-accent outline-none transition-colors disabled:opacity-40"
                >
                  {endOptions.map((t) => (
                    <option key={t} value={t}>{t}</option>
                  ))}
                </select>
              </label>
              <label className="flex flex-col gap-1">
                <span className="text-text-secondary text-xs font-sans">Rate (p/kWh)</span>
                <input
                  type="number" step="0.001" min="0"
                  value={Math.round(slot.rate * 100000) / 1000}
                  onChange={(e) => onChange(updateTariffSlot(config, i, 'rate', Number(e.target.value) / 100))}
                  className="bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border border-bg-elevated focus:border-accent outline-none transition-colors"
                />
              </label>
              {config.slots.length > 1 && (
                <button
                  onClick={() => onChange(removeTariffSlot(config, i))}
                  className="text-text-secondary hover:text-red-400 text-sm font-sans pb-2 transition-colors"
                  title="Remove Window"
                >
                  ✕
                </button>
              )}
            </div>
            {slotErrors.length > 0 && (
              <ul className="text-red-400 text-xs font-sans pl-1">
                {slotErrors.map((msg, j) => (
                  <li key={j}>{msg}</li>
                ))}
              </ul>
            )}
          </div>
        );
      })}

      {config.slots.length === 1 && (
        <p className="text-text-secondary/60 text-xs font-sans">
          Flat rate for the whole day. Add windows for time-of-use tariffs (e.g. Octopus Flux, Cosy, Eco7).
        </p>
      )}
      {!isTariffConfigValid(config) && (
        <div className="flex items-center justify-between gap-3">
          <p className="text-red-400 text-xs font-sans">
            Tariff configuration is invalid — windows must cover the full 24 hours contiguously with no overlaps. Saving is disabled.
          </p>
          <button
            // Recovery affordance: if the user lands in an unsaveable state
            // (e.g. after editing settings.json by hand), this resets to a
            // valid flat-rate config using the first slot's rate as a
            // reasonable default.
            onClick={() => onChange(flatTariffConfig(config.slots[0]?.rate ?? 0.15))}
            className="text-accent text-xs font-sans hover:opacity-80 transition-opacity whitespace-nowrap"
          >
            Reset to Flat Rate
          </button>
        </div>
      )}
    </div>
  );
}
