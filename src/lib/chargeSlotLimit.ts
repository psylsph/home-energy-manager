import type { ScheduleSlot } from './types';
import { formatPowerLimitLabel } from './powerLimit';

/**
 * What a timed-charge slot can deliver at the charge limit the inverter holds
 * (issue #362). The limit is one global setting rather than a per-slot one, so
 * a long overnight slot can be throttled by a limit set for another purpose.
 * These helpers only describe that; nothing here changes a limit.
 */

type SlotWindow = Pick<ScheduleSlot, 'start_hour' | 'start_minute' | 'end_hour' | 'end_minute'>;

const MINUTES_PER_DAY = 24 * 60;

/**
 * A battery size outside this range is a corrupt read rather than a pack (see
 * `powerLimit.ts`), and a shortfall computed from it would be noise.
 */
const MIN_PLAUSIBLE_CAPACITY_KWH = 1;
const MAX_PLAUSIBLE_CAPACITY_KWH = 150;

/**
 * Percentage points of slack before a slot is called short. SOC reads in whole
 * percent and the limit is snapped, so a gap this small is rounding, not a
 * throttled slot.
 */
const SHORTFALL_TOLERANCE_PERCENT = 2;

/**
 * Minutes of charging the slot offers. The whole window (wrapping past
 * midnight), or only what is left of it when `nowMinute` falls inside it. An
 * empty window (start equals end) is zero: the inverter treats it as disabled,
 * not as a full day.
 */
export function slotWindowMinutes(slot: SlotWindow, nowMinute?: number | null): number {
  const start = slot.start_hour * 60 + slot.start_minute;
  const end = slot.end_hour * 60 + slot.end_minute;
  if (start === end) return 0;
  const length = end > start ? end - start : MINUTES_PER_DAY - start + end;
  const elapsed = elapsedInWindow(start, end, nowMinute);
  return elapsed == null ? length : length - elapsed;
}

/** Minutes since the window opened, or `null` when the clock is outside it or unknown. */
function elapsedInWindow(start: number, end: number, nowMinute: number | null | undefined): number | null {
  if (nowMinute == null || !Number.isFinite(nowMinute)) return null;
  if (end > start) {
    return nowMinute >= start && nowMinute < end ? nowMinute - start : null;
  }
  if (nowMinute >= start) return nowMinute - start;
  if (nowMinute < end) return MINUTES_PER_DAY - start + nowMinute;
  return null;
}

export interface ChargeSlotAssessmentInput {
  slot: ScheduleSlot;
  /** The inverter's charge limit in watts, or `null` when it is unknown. */
  limitWatts: number | null | undefined;
  capacityKwh: number | null | undefined;
  /** Battery level now, or `null` when unknown. */
  socPercent: number | null | undefined;
  /** Inverter clock as minutes past midnight, or `null` when unknown. */
  nowMinute: number | null | undefined;
  /**
   * The schedule's other enabled slots. They charge too, so a slot is short
   * only if the slots together cannot reach the target. Only those with at
   * least this slot's target count. Overlapping windows would be counted
   * twice; the inverter does not normally allow them.
   */
  otherSlots?: readonly ScheduleSlot[];
}

export type ChargeSlotAssessment =
  | { kind: 'none' }
  | ChargeSlotCapacity & { kind: 'ok' }
  | ChargeSlotCapacity & {
      kind: 'short';
      socPercent: number;
      targetPercent: number;
      reachPercent: number;
    };

interface ChargeSlotCapacity {
  addableKwh: number;
  /** Energy the slot can add as a share of the pack; above 100 means it can fill an empty battery. */
  addablePercent: number;
  hours: number;
  /** True when the slot is running and only its remaining time was counted. */
  inProgress: boolean;
}

/**
 * Work out what the slot can add at the current limit and whether that falls
 * short of the target from the battery's current level.
 *
 * The comparison starts from the level now. A slot later tonight will in fact
 * start lower, so a slot reported as short really is; a slot reported as fine
 * may not be if the battery drains first. Solar topping up in between is the
 * one way the warning can be pessimistic, and the wording says "from the
 * current" level so it reads as an estimate.
 */
export function assessChargeSlot({
  slot,
  limitWatts,
  capacityKwh,
  socPercent,
  nowMinute,
  otherSlots,
}: ChargeSlotAssessmentInput): ChargeSlotAssessment {
  // Zero is a real limit - the register accepts it, and it stops charging - so
  // it is assessed (and flagged) rather than treated as unknown.
  if (limitWatts == null || !Number.isFinite(limitWatts) || limitWatts < 0) return { kind: 'none' };
  if (
    capacityKwh == null
    || !Number.isFinite(capacityKwh)
    || capacityKwh < MIN_PLAUSIBLE_CAPACITY_KWH
    || capacityKwh > MAX_PLAUSIBLE_CAPACITY_KWH
  ) {
    return { kind: 'none' };
  }
  const ownMinutes = slotWindowMinutes(slot, nowMinute);
  if (ownMinutes <= 0) return { kind: 'none' };
  // A neighbour that stops at a lower target (per-slot targets on Gen3) cannot
  // carry the battery up to this slot's target, so it is not counted.
  const helpers = (otherSlots ?? []).filter((other) => other.target_soc >= slot.target_soc);
  const minutes = helpers.reduce(
    (total, other) => total + slotWindowMinutes(other, nowMinute),
    ownMinutes,
  );

  const hours = minutes / 60;
  const addableKwh = (limitWatts / 1000) * hours;
  const addablePercent = (addableKwh / capacityKwh) * 100;
  const inProgress = ownMinutes < slotWindowMinutes(slot);
  const capacity: ChargeSlotCapacity = { addableKwh, addablePercent, hours, inProgress };

  if (socPercent == null || !Number.isFinite(socPercent)) return { kind: 'ok', ...capacity };
  const targetPercent = slot.target_soc;
  const needed = targetPercent - socPercent;
  if (needed <= 0 || addablePercent >= needed - SHORTFALL_TOLERANCE_PERCENT) {
    return { kind: 'ok', ...capacity };
  }
  return {
    kind: 'short',
    ...capacity,
    socPercent,
    targetPercent,
    reachPercent: socPercent + addablePercent,
  };
}

/** Everything about the charge limit and battery that does not depend on which slot is shown. */
export interface ChargeSlotLimitContext extends Omit<ChargeSlotAssessmentInput, 'slot'> {
  /** The inverter's charge limit as a percentage, or `null` when unknown. */
  limitPercent: number | null | undefined;
  /** Name of an automation that rewrites the limit itself, if one is active. */
  managedBy?: string | null;
  /** False when the schedule's master switch is off, so no slot will run. Defaults to true. */
  armed?: boolean;
}

export interface ChargeSlotLimitDescriptionInput extends ChargeSlotLimitContext {
  slot: ScheduleSlot;
}

export interface ChargeSlotLimitDescription {
  /** Always shown when the limit is known. */
  limitLine: string | null;
  /** Shown only when the slot cannot reach its target at this limit. */
  shortfall: string | null;
}

function kwhText(kwh: number): string {
  return kwh.toFixed(1);
}

/** The two lines shown on a charge slot card. */
export function describeChargeSlotLimit(input: ChargeSlotLimitDescriptionInput): ChargeSlotLimitDescription {
  if (input.limitPercent == null || !Number.isFinite(input.limitPercent)) {
    return { limitLine: null, shortfall: null };
  }
  const label = formatPowerLimitLabel(input.limitPercent, input.limitWatts ?? null);
  if (input.managedBy) {
    // The automation will move the limit, so a warning about today's value
    // would be wrong by the time the slot runs.
    return { limitLine: `Charge limit: ${label}. ${input.managedBy} adjusts this.`, shortfall: null };
  }
  const limitLine = `Charge limit: ${label}, set under Battery and Power Controls.`;

  // A switched-off schedule runs nothing, so there is nothing to fall short.
  if (input.armed === false) return { limitLine, shortfall: null };
  const assessment = assessChargeSlot(input);
  if (assessment.kind !== 'short') return { limitLine, shortfall: null };

  const together = (input.otherSlots ?? []).some((other) => other.target_soc >= input.slot.target_soc);
  const lead = assessment.inProgress
    ? `In the time left ${together ? 'your enabled slots together' : 'this slot'}`
    : `At this limit ${together ? 'your enabled slots together' : 'the slot'}`;
  const shortfall =
    `${lead} can add about ${kwhText(assessment.addableKwh)} kWh `
    + `(${Math.round(assessment.addablePercent)}% of the battery). `
    + `From the current ${Math.round(assessment.socPercent)}% it would reach about `
    + `${Math.round(assessment.reachPercent)}%, short of the ${assessment.targetPercent}% target.`;
  return { limitLine, shortfall };
}
