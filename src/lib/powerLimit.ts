/**
 * Conversion of a battery / inverter power-limit percentage into the kilowatt
 * figure shown next to its slider on the Control page.
 *
 * ## Why this is a percentage of the inverter's maximum
 *
 * The DC-hybrid limit registers `HR_BATTERY_CHARGE_LIMIT` / `HR_BATTERY_DISCHARGE_LIMIT`
 * (HR 111/112) run 0–50, so the UI doubles them for display; the AC-coupled
 * (HR 313/314) and three-phase (HR 1110/1108) registers already run 1–100.
 * Either way, **the full-scale register value means 100 %**, which makes the
 * display percentage a fraction of the inverter's maximum battery power — the
 * same reading `givenergy-modbus` documents (`set_battery_charge_limit(val)`:
 * "Charge power limit (0–50%)"), the same one the Inverter Active Power Limit
 * already uses for HR 50, and the same one the backend forecast applies in
 * `forecast::battery_rate_limits_kw`.
 *
 * ## What this replaces
 *
 * The Control page previously derived the figure from battery *capacity*
 * (`percent / 200 × capacity_w`) following GivTCP's `read.py`, then clamped it
 * to the inverter's maximum. The two conventions agree only when
 * `capacity / 2` equals the inverter's rated battery power. On a pack larger
 * than about half the inverter's rating they diverge, and the clamp then pinned
 * the readout to the maximum for the whole top of the slider — a 13.5 kWh pack
 * behind a 2600 W inverter read "2.6 kW" for every position from 39 % upward
 * (issue #346).
 *
 * Battery capacity is deliberately **not** an input here: there is nothing left
 * to clamp, so the readout cannot saturate, and the figure always means
 * "this percentage of the inverter's stated maximum".
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

/**
 * The largest raw value the charge/discharge limit register can hold for a
 * family: 50 on the DC-hybrid half scale (HR 111/112), 100 on the direct
 * registers (HR 313/314, HR 1108/1110).
 */
export function limitRegisterMax(usesDirect: boolean): number {
  return usesDirect ? MAX_PERCENT : MAX_PERCENT / 2;
}

/**
 * Convert a raw charge/discharge limit register into the 0-100 percentage every
 * UI surface shows. Half-scale DC-hybrid registers are doubled; direct
 * registers pass through. Corrupt values above the family maximum are clamped
 * rather than rendered as an impossible percentage.
 *
 * Single source of truth for the scale: the Control page sliders, the Inverter
 * page readout and the Adaptive Charge editor must all agree, otherwise the
 * same setting reads differently on different pages (issue #346).
 */
export function rawLimitToPercent(raw: number, usesDirect: boolean): number {
  const max = limitRegisterMax(usesDirect);
  const clamped = Math.max(0, Math.min(max, raw));
  return usesDirect ? clamped : clamped * 2;
}

/**
 * Inverse of {@link rawLimitToPercent}: the 0-100 display percentage back to
 * the raw register value. The half scale quantises to the nearest integer,
 * matching the backend's `div_ceil(percent, 2)` for HR 111/112.
 */
export function percentToRawLimit(percent: number, usesDirect: boolean): number {
  const clamped = Math.max(0, Math.min(MAX_PERCENT, percent));
  return usesDirect ? Math.round(clamped) : Math.round(clamped / 2);
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
