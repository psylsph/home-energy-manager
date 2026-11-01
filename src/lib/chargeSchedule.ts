/**
 * Charge and export slot summary for the house view's schedule card (issue #422).
 *
 * Pure: the caller supplies the minute of day, evaluated on the inverter's
 * clock (`inverterMinuteOfDay`) because the slots are in inverter local
 * time, and HEM may run in a UTC container. The caller may fall back to the
 * browser clock when the snapshot has no readable `inverter_time`.
 */
import type { InverterSnapshot, ScheduleSlot } from './types';
import { formatHHMM, isInMergedWindow, mergeScheduleWindows, telemetryConfirmsExport } from './ecoTimedExport';

export interface ChargeWindow {
  /** `active`: now is inside the window; `next`: the soonest window to start. */
  state: 'active' | 'next';
  /** e.g. `"23:30"`. */
  start: string;
  /** e.g. `"05:30"`; equal to `start` only for a window covering the whole day. */
  end: string;
}

const MINUTES_PER_DAY = 24 * 60;

interface RankedWindow {
  window: ChargeWindow;
  /** -1 while running, else minutes until it starts. */
  wait: number;
}

/**
 * The window running now, or the next to start, ranked. Adjacent and
 * overlapping slots are merged first (16:00–17:00 + 17:00–18:00 is one
 * export to 18:00), the same way the Control page reports "Eco resumes".
 */
function nextWindow(slots: ScheduleSlot[], armed: boolean, nowMinute: number): RankedWindow | null {
  if (!armed) return null;
  let best: RankedWindow | null = null;
  for (const w of mergeScheduleWindows(slots)) {
    const wait = isInMergedWindow([w], nowMinute) ? -1 : (w.start - nowMinute + MINUTES_PER_DAY) % MINUTES_PER_DAY;
    if (best === null || wait < best.wait) {
      best = { wait, window: { state: wait < 0 ? 'active' : 'next', start: formatHHMM(w.start), end: formatHHMM(w.end) } };
    }
  }
  return best;
}

/** The HEM-managed Timed Export schedule (`GET /api/timed-export`, issue #289). */
export interface TimedExportSchedule {
  schedule_enabled: boolean;
  slots: ScheduleSlot[];
}

/** A Timed Charge slot, a Timed Export slot, or a Timed Demand discharge slot. */
export type BatteryEventKind = 'charge' | 'export' | 'discharge';

export interface BatteryEvent extends ChargeWindow {
  kind: BatteryEventKind;
}

type ScheduleSnapshot = Pick<
  InverterSnapshot,
  | 'charge_slots' | 'enable_charge' | 'discharge_slots' | 'enable_discharge' | 'battery_mode'
  | 'max_charge_slots' | 'max_discharge_slots' | 'battery_pause_mode' | 'battery_pause_slot'
>;

/**
 * Only the slots this inverter has. The decoder can fill positions past the
 * model's count from registers it doesn't use (e.g. HR 31-32 on a one-slot
 * Gen1), which the Control page never shows and the user can't clear.
 */
function capped(slots: ScheduleSlot[] | undefined, max: number | undefined): ScheduleSlot[] {
  const list = slots ?? [];
  return max && max > 0 ? list.slice(0, max) : list;
}

interface Source {
  kind: BatteryEventKind;
  slots: ScheduleSlot[];
  armed: boolean;
}

/**
 * Every schedule that can move the battery, in tie-break order (a charge wins
 * a tie):
 *
 * - Timed Charge: the charge slots while `enable_charge` is armed.
 * - The HEM-managed Timed Export schedule. Outside its windows the inverter
 *   sits in Eco with discharge disarmed (and, under the re-arm fallback, its
 *   slots cleared), so the snapshot can't show the next export.
 * - The inverter's own armed discharge slots: exports in Timed Export / Export
 *   Paused (also how Force Discharge, calibration and Agile export, so a
 *   manual export running now isn't hidden behind the schedule), discharges
 *   to the house in Timed Demand.
 * - Timed Discharge, kept in the pause registers: pause mode 2 with the
 *   visible window the backend has already decoded.
 */
function sources(snapshot: ScheduleSnapshot, timedExport: TimedExportSchedule | null): Source[] {
  const exporting = snapshot.battery_mode === 'timed_export' || snapshot.battery_mode === 'export_paused';
  const list: Source[] = [
    { kind: 'charge', slots: capped(snapshot.charge_slots, snapshot.max_charge_slots), armed: snapshot.enable_charge },
    { kind: exporting ? 'export' : 'discharge', slots: capped(snapshot.discharge_slots, snapshot.max_discharge_slots), armed: snapshot.enable_discharge },
  ];
  if (timedExport?.schedule_enabled) {
    list.splice(1, 0, { kind: 'export', slots: capped(timedExport.slots, snapshot.max_discharge_slots), armed: true });
  }
  if (snapshot.battery_pause_mode === 2 && snapshot.battery_pause_slot) {
    list.push({ kind: 'discharge', slots: [snapshot.battery_pause_slot], armed: true });
  }
  return list;
}

/**
 * The battery schedule event to show: a window running now beats one still
 * to come, then the soonest to start wins; ties go to the earlier source in
 * [`sources`] (so a charge wins).
 */
export function batteryEvent(
  snapshot: ScheduleSnapshot,
  timedExport: TimedExportSchedule | null,
  nowMinute: number,
): BatteryEvent | null {
  let best: { kind: BatteryEventKind; ranked: RankedWindow } | null = null;
  for (const source of sources(snapshot, timedExport)) {
    const ranked = nextWindow(source.slots, source.armed, nowMinute);
    if (ranked && (best === null || ranked.wait < best.ranked.wait)) best = { kind: source.kind, ranked };
  }
  return best && { kind: best.kind, ...best.ranked.window };
}

/**
 * Whether the battery is really doing what the running window is for, so the
 * card can say "…Now" and pulse. A charge or a discharge needs battery power
 * above the noise floor in the right direction (+ = discharging). An export
 * uses the Control page's own test (`telemetryConfirmsExport`: the battery
 * discharging or the grid exporting), so the two pages never disagree.
 */
export function doingIt(kind: BatteryEventKind, snapshot: InverterSnapshot, noiseW: number): boolean {
  if (kind === 'charge') return snapshot.battery_power < -noiseW;
  if (kind === 'export') return telemetryConfirmsExport(snapshot);
  return snapshot.battery_power > noiseW;
}

/** Energy held in the battery, or `null` when the capacity isn't known. */
export function batteryStoredKwh(soc: number, capacityKwh: number): number | null {
  if (!Number.isFinite(capacityKwh) || capacityKwh <= 0 || !Number.isFinite(soc)) return null;
  return (Math.max(0, Math.min(100, soc)) / 100) * capacityKwh;
}
