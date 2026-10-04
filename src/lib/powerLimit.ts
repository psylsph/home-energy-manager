/**
 * Conversion between a battery / inverter power-limit percentage, the raw
 * register value that stores it, and the kilowatt figure shown beside the
 * Control page sliders.
 *
 * The slider always means **a percentage of the inverter's maximum battery
 * power**. How that is stored depends on the register family:
 *
 * - **Direct registers** (AC-coupled HR 313/314, three-phase / HV HR
 *   1110/1108, Gateway HR 313/314) hold 1-100 directly as a percentage of the
 *   inverter's maximum (GivTCP `read.py`: `batmaxrate * limit / 100`).
 * - **Half-scale registers** (every single-phase DC hybrid, HR 111/112, 0-50)
 *   hold a percentage of battery *capacity*, with 50 meaning 0.5C. GivTCP
 *   `write.py` stores `watts / (capacity / 2) * 50` and `read.py` reads
 *   `min(reg / 100 * capacity_w, inverter_max)`. Storing "62% of the maximum" as
 *   register 31 therefore asks for 0.31C - 2945 W on a 9.5 kWh pack - which is
 *   above a 2.6 kW inverter and limits nothing (issue #346). The register has
 *   to be derived through the pack size: `percent / 100 * max_w / capacity_w *
 *   100`.
 *
 * Once the maximum is itself capped at `capacity / 2` (as the backend decoder
 * does), a small pack degenerates to the old "double the register" mapping, so
 * only packs larger than twice the inverter's rating behave differently.
 *
 * When the maximum or the capacity is unknown there is nothing to scale by, and
 * the half-scale register falls back to that plain doubling.
 */

/** Highest percentage any limit register can express after display scaling. */
const MAX_PERCENT = 100;

/**
 * Convert a limit percentage into watts against the inverter's stated maximum.
 *
 * @param percent Display percentage (0–100), or `null`/`undefined` when the
 *   register is unknown.
 * @param maxWatts The inverter's maximum for this limit, in watts. `0` means
 *   the rating is unknown (Gateway / EMS / PV inverter), not "no limit".
 * @returns Watts, or `null` when either input is unknown — a percentage has no
 *   watt equivalent without a maximum, and rendering `0` would read as a
 *   disabled limit.
 */
export function percentToWatts(
  percent: number | null | undefined,
  maxWatts: number | null | undefined,
): number | null {
  if (percent == null || maxWatts == null) return null;
  if (!Number.isFinite(percent) || !Number.isFinite(maxWatts)) return null;
  if (maxWatts <= 0) return null;
  const clamped = Math.max(0, Math.min(MAX_PERCENT, percent));
  return Math.round((clamped / MAX_PERCENT) * maxWatts);
}

/**
 * Render the `66% (1.7 kW)` readout that accompanies a limit slider.
 *
 * @param percent Display percentage, or `null`/`undefined` when unknown.
 * @param watts Result of {@link percentToWatts}, or `null` when unknown.
 */
export function formatPowerLimitLabel(
  percent: number | null | undefined,
  watts: number | null,
): string {
  if (percent == null || !Number.isFinite(percent)) return '—';
  const label = `${Math.max(0, Math.min(MAX_PERCENT, percent))}%`;
  // A zero-watt figure carries no information the percentage does not already
  // give, so 0% renders bare rather than as "0% (0.0 kW)".
  if (watts == null || !Number.isFinite(watts) || watts <= 0) return label;
  return `${label} (${(watts / 1000).toFixed(1)} kW)`;
}

/** What a charge/discharge limit register stores, for one device. */
export interface LimitScale {
  /** True for the direct 1-100 registers; false for the half-scale HR 111/112. */
  usesDirect: boolean;
  /** The inverter's maximum battery power in watts; 0 when unknown. */
  maxWatts: number;
  /** Total battery capacity in kWh; 0 when unknown. */
  capacityKwh: number;
}

/** Largest value the half-scale register can hold. */
const HALF_SCALE_MAX_RAW = 50;

/**
 * Plausible range for a pack's capacity in kWh. The smallest GivEnergy battery
 * is 2.6 kWh and the largest supported systems are a few tens of kWh. HR 55 is
 * a raw u16 with no sanitiser on single-phase inverters, so one corrupt dongle
 * read can report thousands of kWh; acting on that would collapse the ratio and
 * write register 0 or 1 for a 40% request, so a capacity outside this range is
 * treated as unknown. Keep in step with `MIN/MAX_PLAUSIBLE_CAPACITY_KWH` in
 * `src-tauri/src/inverter/power_limit.rs`.
 */
const MIN_PLAUSIBLE_CAPACITY_KWH = 1;
const MAX_PLAUSIBLE_CAPACITY_KWH = 150;

/**
 * Half-scale register units per displayed percent: a register unit is 1% of
 * battery capacity, so 1% of the inverter's maximum is `max_w / capacity_w`
 * units. Falls back to 0.5 (the plain half scale) when the maximum is unknown or
 * the capacity is unknown or implausible.
 */
function rawPerPercent(scale: LimitScale): number {
  const capacityKnown = Number.isFinite(scale.capacityKwh)
    && scale.capacityKwh >= MIN_PLAUSIBLE_CAPACITY_KWH
    && scale.capacityKwh <= MAX_PLAUSIBLE_CAPACITY_KWH;
  if (capacityKnown && Number.isFinite(scale.maxWatts) && scale.maxWatts > 0) {
    const capacityW = scale.capacityKwh * 1000;
    // Register 50 is 0.5C, so nothing above capacity / 2 can be reached however
    // the maximum is stated (the backend decoder applies the same cap).
    return Math.min(scale.maxWatts, capacityW / 2) / capacityW;
  }
  return HALF_SCALE_MAX_RAW / MAX_PERCENT;
}

/**
 * The largest raw value the charge/discharge limit register can hold for a
 * family: 50 on the DC-hybrid half scale (HR 111/112), 100 on the direct
 * registers (HR 313/314, HR 1108/1110).
 */
export function limitRegisterMax(usesDirect: boolean): number {
  return usesDirect ? MAX_PERCENT : HALF_SCALE_MAX_RAW;
}

/**
 * Convert a raw charge/discharge limit register into the 0-100 percentage of
 * the inverter's maximum that every UI surface shows. Anything at or above the
 * value that reaches the inverter's maximum (including the factory default of
 * 50) reads as 100%. Corrupt values above the family maximum are clamped.
 *
 * Single source of truth for the scale: the Control page sliders, the Inverter
 * page readout and the Adaptive Charge editor must all agree, otherwise the
 * same setting reads differently on different pages (issue #346).
 */
export function rawLimitToPercent(raw: number, scale: LimitScale): number {
  const clamped = Math.max(0, Math.min(limitRegisterMax(scale.usesDirect), raw));
  if (scale.usesDirect) return clamped;
  return Math.min(MAX_PERCENT, Math.round(clamped / rawPerPercent(scale)));
}

/**
 * Inverse of {@link rawLimitToPercent}: the 0-100 percentage of the inverter's
 * maximum back to the raw register value, rounded to the nearest register step.
 *
 * 100% on the half-scale register writes 50 (0.5C), the register maximum and
 * the factory default. That means "no limit": it is above any inverter's rating
 * and, unlike a value derived from the pack size, cannot throttle if the
 * capacity is ever misread.
 */
export function percentToRawLimit(percent: number, scale: LimitScale): number {
  const clamped = Math.max(0, Math.min(MAX_PERCENT, percent));
  if (scale.usesDirect) return Math.round(clamped);
  if (clamped >= MAX_PERCENT) return HALF_SCALE_MAX_RAW;
  // A non-zero percentage must never round down to register 0, which stops
  // charging; only a deliberate 0% writes 0.
  const floor = clamped > 0 ? 1 : 0;
  return Math.max(floor, Math.min(HALF_SCALE_MAX_RAW, Math.round(clamped * rawPerPercent(scale))));
}

/**
 * The percentage the inverter will actually hold for a slider position: the
 * register quantises to whole units (about 4% apart on a 9.5 kWh pack behind a
 * 2.6 kW inverter), so the label should show what a save will really do.
 */
export function snapLimitPercent(percent: number, scale: LimitScale): number {
  return rawLimitToPercent(percentToRawLimit(percent, scale), scale);
}

/**
 * An unsaved (or saved but not yet read back) slider position, remembered with
 * the register value it was edited from.
 */
export interface LimitDraft {
  value: number;
  base: number | undefined;
}

/**
 * The value a limit slider should show. The draft wins only while the inverter
 * still reports the value it was edited from; once the register changes —
 * our own save being read back, or a change from the GivEnergy app — the live
 * value takes over. Comparing the draft to the read-back instead left the
 * slider stuck on its draft whenever the two could never match (odd percentages
 * on the half scale) and hid every later external change.
 */
export function resolveLimitDraft(
  draft: LimitDraft | null,
  snapshotValue: number | undefined,
): number | undefined {
  if (draft != null && draft.base === snapshotValue) return draft.value;
  return snapshotValue;
}
