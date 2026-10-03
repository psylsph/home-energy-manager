import type { InverterSnapshot } from './types';

/**
 * Whether the device is an AC-coupled battery inverter (0x30xx family).
 *
 * The backend (`model.rs` `DeviceType::from_register`) maps 0x3001 ->
 * ACCoupled, 0x3002 -> ACCoupledMk2, and falls back to ACCoupled for any
 * other 0x30xx code — so the whole family is classified broadly, not just
 * the two explicitly listed codes. Single source of truth for AC-coupled
 * classification: ControlPage's charge/discharge power-limit routing and
 * the Adaptive charge section both consume this.
 */
export function isAcCoupledDevice(code: string | null | undefined): boolean {
  return !!code && code.startsWith('30');
}

/**
 * Whether the device uses the three-phase-bank charge/discharge power limit
 * registers (HR 1108/1110, already 1-100%): 0x40/60/81/82 families.
 *
 * The Gateway (0x70xx) is not one of them: it keeps its limits in the
 * single-phase AC-limit bank, HR 313/314 — see {@link usesAcLimitRegisters}.
 *
 * Mirrors the backend's `DeviceType::uses_three_phase_schedule_slots`, which
 * routes these limits to HR 1108/1110 (decoded from
 * `THREE_PHASE_CONFIG_BLOCK`, HR 1080-1124).
 */
export function isThreePhaseLimitModel(code: string | null | undefined): boolean {
  return !!code
    && (code.startsWith('40')
      || code.startsWith('60')
      || code.startsWith('81')
      || code.startsWith('82'));
}

/**
 * Whether the device keeps its charge/discharge power limits in the
 * single-phase AC-limit bank (HR 313/314, direct 1-100%): AC-coupled models
 * plus the Gateway (0x70xx). GivTCP writes the Gateway's rate with
 * `set_battery_charge_limit_ac` and reads it back from HR 313/314.
 * Mirrors the backend's `DeviceType::uses_ac_limit_registers`.
 */
export function usesAcLimitRegisters(code: string | null | undefined): boolean {
  return isAcCoupledDevice(code) || (!!code && code.startsWith('70'));
}

/**
 * Whether the charge/discharge power limit registers for this device are
 * already a direct 1-100% percentage (AC-limit bank HR313/314 and three-phase
 * HR1110/1108), rather than the 0-50 DC-hybrid HR111/112 scale that the UI
 * doubles for display. Used by ControlPage's rate sliders and the Adaptive
 * charge section's kW estimates so both agree with the registers the
 * backend actually writes.
 */
export function usesDirectChargeLimit(code: string | null | undefined): boolean {
  return usesAcLimitRegisters(code) || isThreePhaseLimitModel(code);
}

/** DTC prefixes the backend maps to a known family outside the 1000-range layout. */
const HR50_ACTIVE_POWER_RATE_PREFIXES = ['10', '20', '21', '22', '23', '30', '50', '70', '80', '83'];

/**
 * Whether `active_power_rate` lives at HR 50 for this device — the only place
 * HEM both reads and writes it, so the only place the Inverter Active Power
 * Limit control can work.
 *
 * The 1000-range-layout families (`isThreePhaseLimitModel`) store the
 * inverter's max-output percentage at HR 1002, which
 * `decode_holding_1000_1079` decodes *after* HR 50 and therefore supersedes
 * it. `SetActivePowerRate` encodes HR 50 unconditionally, and HR 1002 is not in
 * the backend's `SAFE_WRITE_REGS` — `givenergy-modbus` ships only an HR 50
 * setter (`commands.py::set_active_power_rate`). On those families the slider
 * would write a register the layout ignores and then read the previous value
 * back, so the control is hidden there and the backend refuses the write
 * (issue #346). Mirrors the backend's `DeviceType::uses_hr50_active_power_rate`.
 *
 * Only the DTC families the backend recognises qualify. Unknown codes —
 * including the unsupported commercial 0x41xx/0x51xx families — use a
 * register map HEM does not implement, so they get no control.
 *
 * Returns false when the device type code is missing (pre-snapshot state) so
 * the slider doesn't flash before the backend would accept a write.
 */
export function deviceUsesHr50ActivePowerRate(code: string | null | undefined): boolean {
  return !!code && HR50_ACTIVE_POWER_RATE_PREFIXES.some((prefix) => code.startsWith(prefix));
}

/**
 * Whether the inverter exposes a confirmed, safely writable Emergency Power
 * Supply (EPS) enable register at HR 317.
 *
 * Mirrors the backend's `DeviceType::supports_eps` and the reference safe-write
 * map:
 *
 *   - 0x30xx — AC-coupled (single-phase AC battery inverter)
 *   - 0x60xx — AC three-phase
 *   - 0x80xx — Residential All-in-One (AIO 6kW, 3.6kW, 5kW)
 *
 * Hybrid HV Gen3 exposes read-back state at HR 1105, but that register is not
 * in either reference implementation's safe-write set. It and other DC hybrid
 * families remain hidden until a safe write is confirmed.
 *
 * Returns false when the device type code is missing (pre-snapshot state) so
 * the UI doesn't briefly flash a control that will be rejected.
 */
export function deviceSupportsEps(
  snapshot: Pick<InverterSnapshot, 'device_type_code'> | null | undefined,
): boolean {
  const code = snapshot?.device_type_code;
  if (!code) return false;
  return (
    code === '3001'
    || code === '3002'
    || code.startsWith('60')
    || code.startsWith('80')
  );
}

/**
 * Whether the inverter exposes a configurable grid export limit register.
 *
 * Mirrors the register routing in the backend `set_export_limit` handler
 * (`server/api.rs`), which is the authoritative source for which device
 * families have a user-writable export limit:
 *
 *   - 0x70xx — Gateway        → HR 2071 (plant-level export limit, raw W)
 *   - 0x50xx — EMS            → HR 2071
 *   - 0x40xx — three-phase    → HR 1063 (`p_export_limit`, deci-W)
 *   - 0x60xx — AC three-phase → HR 1063
 *   - 0x81xx — HV Gen3 hybrid → HR 1063
 *   - 0x82xx — All-in-One hybrid → HR 1063
 *
 * Single-phase / AC-coupled hybrids (Gen1/2/3/4, Polar, Gen3+, AC, AC Mk2)
 * and residential All-in-One 0x80xx models have NO configurable export limit:
 * their HR(26) `grid_port_max_power_output` is the read-only rated hardware
 * max output, not an export-limit setting, and `/api/control/export-limit`
 * refuses the write for them. Used by `InverterPage` to show the "Grid Export
 * Limit" row only where it is meaningful.
 *
 * Returns false when the device type code is missing (pre-snapshot state).
 */
export function deviceSupportsExportLimit(
  snapshot: Pick<InverterSnapshot, 'device_type_code'> | null | undefined,
): boolean {
  const code = snapshot?.device_type_code;
  if (!code) return false;
  return (
    code.startsWith('40')
    || code.startsWith('50')
    || code.startsWith('60')
    || code.startsWith('70')
    || code.startsWith('81')
    || code.startsWith('82')
  );
}

/**
 * Whether the inverter supports the portal-style single-slot "Timed
 * Discharge" feature.
 *
 * The feature is implemented with the battery pause registers —
 * `battery_pause_mode` (HR 318) and `battery_pause_slot` (HR 319-320) — which
 * live in the HR 300-359 AC-config block. That block is present only on
 * AC-three-phase and residential All-in-One models. Legacy AC-coupled models
 * expose the AC-config block for EPS, but field logs show HR319/320 reject
 * Timed Discharge slot writes with Modbus exception 1, so they are gated out
 * until a safe write path is confirmed. On every other family (AC-coupled,
 * DC hybrids incl. Gen1/2/4, Polar, Gen3+, pure three-phase, AIO Hybrid,
 * HV Gen3, Gateway, EMS, PV inverter) the registers don't exist:
 * the write is dropped/times out and `battery_pause_mode` never reflects an
 * enabled state, so the toggle appeared broken (the originally reported
 * Gen1 Hybrid symptom).
 *
 * Gen3 Hybrid is the deliberate exception: the full HR 300-359 block times
 * out on this family, but a targeted 3-register probe of HR 318-320 succeeds
 * on ARM firmware >= 312 (reported working on fw 318). So Gen3 Hybrid with
 * `device_type_code` 0x20xx + ARM fw century 3 (>= 312) qualifies here, and
 * the backend probes those registers out-of-band in `poll.rs`.
 *
 * Mirrors the backend's `DeviceType::supports_timed_discharge` (which takes
 * the ARM firmware version).
 *
 * Used by `ControlPage` to hide both the Quick Action button and the Timed
 * Discharge schedule section, and kept as a dedicated predicate (rather than
 * reusing `deviceSupportsEps`) so the two features can diverge if firmware
 * ever decouples them.
 *
 * Returns false when the device type code is missing (pre-snapshot state) so
 * the UI doesn't briefly flash a control the backend will reject with 400.
 */
export function deviceSupportsTimedDischarge(
  snapshot: Pick<InverterSnapshot, 'device_type_code' | 'firmware_version'> | null | undefined,
): boolean {
  const code = snapshot?.device_type_code;
  if (!code) return false;
  if (
    code.startsWith('60')
    || code.startsWith('80')
  ) {
    return true;
  }
  // Gen3 Hybrid (device code 0x20xx, ARM firmware century 3) reaches the
  // pause registers via a targeted 3-register probe rather than the full
  // HR 300-359 block (which times out on this family). Enabled only at
  // ARM fw >= 312. Mirrors the backend's
  // `DeviceType::Gen3Hybrid && arm_fw >= 312` rule.
  if (code.startsWith('20')) {
    const armFw = parseInt(snapshot?.firmware_version ?? '', 10);
    if (Number.isFinite(armFw) && Math.floor(armFw / 100) === 3 && armFw >= 312) {
      return true;
    }
  }
  return false;
}
