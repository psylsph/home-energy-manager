import { isAnySlotActive } from './energyFlow';
import type { InverterSnapshot, ScheduleSlot } from './types';

/**
 * State of one of the four independent battery mechanisms surfaced on the
 * Inverter page battery summary.
 *
 * - `off`     — register is not armed (or unsupported on this inverter).
 * - `armed`   — register is set, but the mechanism is not currently active
 *             (e.g. outside a configured slot window, or the battery isn't
 *             actually charging/discharging).
 * - `active`  — register is set, the slot window is active, and the battery
 *             is actually doing the corresponding thing.
 */
export type MechanismState = 'off' | 'armed' | 'active';

export interface MechanismRow {
  /** Stable React key. */
  key: 'eco' | 'timed_charge' | 'timed_export' | 'timed_discharge';
  /** Human-readable mechanism name. */
  label: string;
  /** Current mechanism state. */
  state: MechanismState;
  /** Human-readable description shown to the right of the dot. */
  description: string;
}

function isCharging(snapshot: InverterSnapshot): boolean {
  return snapshot.battery_state === 'charging';
}

function isDischarging(snapshot: InverterSnapshot): boolean {
  return snapshot.battery_state === 'discharging';
}

/**
 * Derive the four-row battery-mechanism summary used in place of the old
 * single `battery_mode` label on the Inverter page.
 *
 * Each mechanism is evaluated independently, matching the four co-existing
 * registers the reporter described: HR27 (Eco/self-consumption), HR96
 * (Timed Charge), HR59 (Timed Export) and HR318 (Timed Discharge / pause).
 * `now` is injectable so tests can be deterministic around slot-window
 * boundaries.
 */
export function deriveBatteryModeRows(
  snapshot: InverterSnapshot,
  now: Date = new Date(),
): MechanismRow[] {
  const rows: MechanismRow[] = [];

  // Eco / self-consumption is the always-on baseline (HR27 = 1).
  const ecoOn = snapshot.battery_power_mode === 1;
  rows.push({
    key: 'eco',
    label: 'Eco',
    state: ecoOn ? 'active' : 'off',
    description: ecoOn ? 'on' : 'off',
  });

  // Timed Charge: HR96 + charge slots + battery actually charging.
  const chargeArmed = snapshot.enable_charge;
  const chargeSlotActive = isAnySlotActive(snapshot.charge_slots, now);
  const chargeActive = chargeArmed && chargeSlotActive && isCharging(snapshot);
  rows.push({
    key: 'timed_charge',
    label: 'Timed Charge',
    state: chargeActive ? 'active' : chargeArmed ? 'armed' : 'off',
    description: chargeActive
      ? 'armed · charging now'
      : chargeArmed
        ? 'armed'
        : 'off',
  });

  // Timed Export: HR59 + discharge slots + battery actually discharging.
  const exportArmed = snapshot.enable_discharge;
  const exportSlotActive = isAnySlotActive(snapshot.discharge_slots, now);
  const exportActive = exportArmed && exportSlotActive && isDischarging(snapshot);
  rows.push({
    key: 'timed_export',
    label: 'Timed Export',
    state: exportActive ? 'active' : exportArmed ? 'armed' : 'off',
    description: exportActive
      ? 'armed · exporting now'
      : exportArmed
        ? 'armed'
        : 'off',
  });

  // Timed Discharge: HR318=2 + visible pause-slot window + discharging.
  // The snapshot's `battery_pause_slot` is already the inverse-decoded
  // visible slot (e.g. user slot 03:00-04:00), so a simple window check
  // gives "inside the demand window".
  const tdArmed = snapshot.battery_pause_mode === 2;
  const tdSlot: ScheduleSlot | undefined =
    snapshot.battery_pause_slot?.enabled ? snapshot.battery_pause_slot : undefined;
  const tdSlotActive = tdSlot ? isAnySlotActive([tdSlot], now) : false;
  const tdActive = tdArmed && tdSlotActive && isDischarging(snapshot);
  rows.push({
    key: 'timed_discharge',
    label: 'Timed Discharge',
    state: tdActive ? 'active' : tdArmed ? 'armed' : 'off',
    description: tdActive
      ? 'armed · covering demand now'
      : tdArmed
        ? 'armed'
        : 'off',
  });

  return rows;
}
