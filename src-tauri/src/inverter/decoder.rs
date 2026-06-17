//! Register-to-model decoder.
//!
//! Translates raw Modbus register values into the typed inverter model structs,
//! using the correct register layout from the givenergy-modbus reference library.
//!
//! # Register Layout
//!
//! INPUT registers (block: start=0, count=60):
//!   IR(0):    status
//!   IR(1):    v_pv1 (/10 V)
//!   IR(2):    v_pv2 (/10 V)
//!   IR(3):    v_p_bus (/10 V)
//!   IR(4):    v_n_bus (/10 V)
//!   IR(5):    v_ac1 — grid voltage (/10 V)
//!   IR(6-7):  e_battery_throughput (uint32 /10 kWh)
//!   IR(8):    i_pv1 (/10 A)
//!   IR(9):    i_pv2 (/10 A)
//!   IR(10):   i_ac1 — grid current (/10 A)
//!   IR(11-12): e_pv_total (uint32 /10 kWh)
//!   IR(13):   f_ac1 — grid frequency (/100 Hz)
//!   IR(14):   charge_status
//!   IR(17):   e_pv1_day (/10 kWh)
//!   IR(18):   p_pv1 (W)
//!   IR(19):   e_pv2_day (/10 kWh)
//!   IR(20):   p_pv2 (W)
//!   IR(21-22): e_grid_out_total (uint32 /10 kWh)
//!   IR(25):   e_grid_out_day — export today (/10 kWh)
//!   IR(26):   e_grid_in_day — import today (/10 kWh)
//!   IR(27-28): e_inverter_in_total (uint32 /10 kWh)
//!   IR(30):   p_grid_out (int16 W, signed, negative=import)
//!   IR(31):   p_backup — EPS (W)
//!   IR(35):   e_ac_charge_day — AC charge from grid today (/10 kWh)
//!   IR(36):   e_battery_charge_day (/10 kWh)
//!   IR(37):   e_battery_discharge_day (/10 kWh)
//!   IR(41):   t_inverter_heatsink (/10 °C)
//!   IR(49):   system_mode
//!   IR(50):   v_battery (/100 V)
//!   IR(51):   i_battery (int16 /100 A)
//!   IR(52):   p_battery (int16 W, signed, positive=charging)
//!   IR(55):   t_charger (/10 °C)
//!   IR(56):   t_battery (/10 °C)
//!   IR(59):   battery_soc (%)
//!
//! HOLDING registers (blocks: start=0/count=60 and start=60/count=60):
//!   HR(0):     device_type_code
//!   HR(1-2):   module (uint32)
//!   HR(3):     num_mppt (high byte), num_phases (low byte)
//!   HR(5):     unused
//!   HR(6):     system_voltage (/100 V) — live voltage, NOT used for capacity calc
//!   HR(7):     enable_ammeter (bool)
//!   HR(8-12):  first_battery_serial_number (10 chars)
//!   HR(13-17): serial_number (10 chars)
//!   HR(18):    bms_firmware_version
//!   HR(19):    dsp_firmware_version
//!   HR(20):    enable_charge_target (bool)
//!   HR(21):    arm_firmware_version
//!   HR(27):    battery_power_mode (0=export, 1=eco)
//!   HR(31-32): charge_slot_2 (timeslot pair)
//!   HR(35-40): system_time (year, month, day, hour, minute, second)
//!   HR(43):    charge_soc (high), discharge_soc (low)
//!   HR(44-45): discharge_slot_2 (timeslot pair)
//!   HR(55):    battery_capacity_ah
//!   HR(56-57): discharge_slot_1 (timeslot pair)
//!   HR(59):    enable_discharge (bool)
//!   HR(94-95): charge_slot_1 (timeslot pair) — in 60-119 block
//!   HR(96):    enable_charge (bool) — in 60-119 block
//!   HR(110):   battery_soc_reserve (%) — in 60-119 block
//!   HR(111):   battery_charge_limit (%) — in 60-119 block
//!   HR(112):   battery_discharge_limit (%) — in 60-119 block
//!   HR(116):   charge_target_soc (%) — in 60-119 block

use crate::modbus::client::BlockRead;
use crate::modbus::registers::{decode_hhmm, RegisterType};

use super::model::{
    BatteryMode, BatteryModule, BatteryState, DeviceType, InverterSnapshot, MeterData, ScheduleSlot,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Safely retrieve a register value by index, returning 0 if out of bounds.
fn get_reg(data: &[u16], index: usize) -> u16 {
    data.get(index).copied().unwrap_or(0)
}

/// Interpret a u16 register value as a signed i16, then widen to i32.
fn signed(raw: u16) -> i32 {
    raw as i16 as i32
}

/// Decode a uint32 value stored across two consecutive registers (high, low).
/// GivEnergy uses big-endian word order for its 32-bit power/energy values
/// (high register first, then low register). See givenergy-modbus framer/PDU
/// for `uint32` converter details.
fn uint32(hi: u16, lo: u16) -> u32 {
    ((hi as u32) << 16) | (lo as u32)
}

/// Decode a lifetime energy total from two registers as deci-kWh.
///
/// Returns 0.0 if the combined value is obviously corrupted (hi register
/// beyond any possible residential installation). At 0.1 kWh resolution,
/// a hi value of 1000 corresponds to 6.5 GWh — impossible for residential.
/// The sanitizer's `check_total_energy_field!` provides the production
/// fallback logic (prev-value, delta checks); this is a hard floor to
/// prevent enormous f32 values from entering the decode pipeline.
fn decode_lifetime_total_kwh(hi: u16, lo: u16) -> f32 {
    // hi > 1000 means >6.5 GWh lifetime → must be dongle corruption.
    // Genuine residential lifetime would be at most ~200 MWh (hi ≈ 30).
    if hi > 1000 {
        return 0.0;
    }
    uint32(hi, lo) as f32 * 0.1
}

/// Decode a timeslot from 2 registers (start HHMM, end HHMM).
///
/// Per the givenergy-modbus reference library, a value of 60 means the slot
/// is disabled (the portal shows '--:--'). Value 0 means midnight (00:00)
/// which is technically valid, but a slot of 00:00–00:00 (zero-length) is
/// treated as disabled since the reference library writes 0 to clear slots.
fn decode_timeslot(data: &[u16], start_idx: usize, end_idx: usize) -> ScheduleSlot {
    let start_val = get_reg(data, start_idx);
    let end_val = get_reg(data, end_idx);

    // Disabled when both start and end are the same value (zero-duration slot).
    // Per givenergy-modbus reference: writing (0, 0) clears a slot, but any
    // equal pair (e.g. 600, 600) is also a zero-length window and effectively
    // disabled. A valid slot always has start != end.
    if start_val == end_val {
        return ScheduleSlot::default();
    }

    match (decode_hhmm(start_val), decode_hhmm(end_val)) {
        (Some((sh, sm)), Some((eh, em))) => ScheduleSlot {
            enabled: true,
            start_hour: sh,
            start_minute: sm,
            end_hour: eh,
            end_minute: em,
            target_soc: 4,
        },
        _ => ScheduleSlot::default(),
    }
}

/// Decode the inverter wall-clock registers HR(35-40).
///
/// The inverter stores a naive wall-clock time (no timezone/DST metadata), so
/// this formats exactly the register values as a local-looking timestamp and
/// deliberately performs no timezone conversion.
fn decode_system_time(data: &[u16]) -> String {
    let year = get_reg(data, 35);
    let month = get_reg(data, 36);
    let day = get_reg(data, 37);
    let hour = get_reg(data, 38);
    let minute = get_reg(data, 39);
    let second = get_reg(data, 40);

    if month == 0 || month > 12 || day == 0 || day > 31 || hour > 23 || minute > 59 || second > 59 {
        return String::new();
    }

    let full_year = if year < 100 { 2000 + year } else { year };
    format!("{full_year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

/// Decode a serial number from consecutive registers.
///
/// Each register holds 2 Latin-1 characters (high byte first, low byte second).
/// `count` is the number of registers (so 5 registers = 10 characters).
pub fn decode_serial(data: &[u16], start: usize, count: usize) -> String {
    let mut s = String::with_capacity(count * 2);
    for i in start..start + count {
        let reg = get_reg(data, i);
        let hi = (reg >> 8) as u8;
        let lo = (reg & 0xFF) as u8;
        if hi != 0 {
            s.push(hi as char);
        }
        if lo != 0 {
            s.push(lo as char);
        }
    }
    s.trim_end().to_string()
}

// ---------------------------------------------------------------------------
// Block identification
// ---------------------------------------------------------------------------

fn block_key(block: &crate::modbus::registers::RegisterBlock) -> &'static str {
    match (block.register_type, block.start) {
        (RegisterType::Input, 0) => "input_0_59",
        (RegisterType::Holding, 0) => "holding_0_59",
        (RegisterType::Holding, 60) => "holding_60_119",
        (RegisterType::Holding, 240) => "holding_240_299",
        (RegisterType::Holding, 300) => "holding_300_359",
        (RegisterType::Holding, 1000) => "holding_1000_1079",
        (RegisterType::Holding, 1080) => "holding_1080_1124",
        (RegisterType::Input, 60) => "battery_input_60_119",
        (RegisterType::Input, 1000) => "input_1000_1059",
        (RegisterType::Input, 1060) => "input_1060_1119",
        (RegisterType::Input, 1120) => "input_1120_1179",
        (RegisterType::Input, 1180) => "input_1180_1239",
        (RegisterType::Input, 1240) => "input_1240_1299",
        (RegisterType::Input, 1300) => "input_1300_1359",
        (RegisterType::Input, 1360) => "input_1360_1413",
        (RegisterType::Input, 180) => "input_180_239",
        // Gateway aggregation bank (IR 1600-1859) — see GATEWAY_INPUT_BLOCKS.
        (RegisterType::Input, 1600) => "input_1600_1659",
        (RegisterType::Input, 1660) => "input_1660_1719",
        (RegisterType::Input, 1720) => "input_1720_1779",
        (RegisterType::Input, 1780) => "input_1780_1830",
        (RegisterType::Input, 1831) => "input_1831_1859",
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------------
// Intermediate state for mode derivation
// ---------------------------------------------------------------------------

/// Raw configuration values needed for battery mode derivation.
struct RawConfig {
    battery_power_mode: u16,
    enable_discharge: bool,
    battery_soc_reserve: u16,
}

// ---------------------------------------------------------------------------
// Main decoder
// ---------------------------------------------------------------------------

/// Decode raw register blocks into an InverterSnapshot.
pub fn decode_snapshot(blocks: &[BlockRead]) -> InverterSnapshot {
    let mut snap = InverterSnapshot {
        timestamp: chrono::Utc::now().timestamp(),
        grid_online: true,
        grid_loss: false,
        inverter_trip: false,
        battery_over_temp: false,
        ..Default::default()
    };
    let mut raw = RawConfig {
        battery_power_mode: 0,
        enable_discharge: false,
        battery_soc_reserve: 0,
    };

    for br in blocks {
        let key = block_key(br.block);
        let data = &br.data;

        match key {
            "input_0_59" => decode_input_0_59(data, &mut snap),
            "holding_0_59" => decode_holding_0_59(data, &mut snap, &mut raw),
            "holding_60_119" => decode_holding_60_119(data, &mut snap, &mut raw),
            "holding_240_299" => decode_holding_240_299(data, &mut snap),
            "holding_300_359" => decode_holding_300_359(data, &mut snap),
            "holding_1000_1079" => decode_holding_1000_1079(data, &mut snap, &mut raw),
            "holding_1080_1124" => decode_holding_1080_1124(data, &mut snap, &mut raw),
            "input_1000_1059" => decode_input_1000_1059(data, &mut snap),
            "input_1060_1119" => decode_input_1060_1119(data, &mut snap),
            "input_1120_1179" => decode_input_1120_1179(data, &mut snap),
            "input_1180_1239" => decode_input_1180_1239(data, &mut snap),
            "input_1240_1299" => decode_input_1240_1299(data, &mut snap),
            "input_1300_1359" => decode_input_1300_1359(data, &mut snap),
            "input_1360_1413" => decode_input_1360_1413(data, &mut snap),
            "input_180_239" => decode_input_180_239(data, &mut snap),
            // Gateway aggregation bank decoders.
            "input_1600_1659" => decode_gateway_1600_1659(data, &mut snap),
            "input_1660_1719" => decode_gateway_1660_1719(data, &mut snap),
            "input_1720_1779" => decode_gateway_1720_1779(data, &mut snap),
            "input_1780_1830" => decode_gateway_1780_1830(data, &mut snap),
            "input_1831_1859" => decode_gateway_1831_1859(data, &mut snap),
            _ => {
                log::warn!("Unknown block '{}' in decode_snapshot", key);
            }
        }
    }

    // Compute home power.
    // Internal sign conventions (after negation of raw battery values):
    //   battery_power > 0 = charging (power flowing INTO battery)
    //   grid_power > 0 = exporting (power flowing OUT to grid, "p_grid_out")
    //
    // Home consumption = solar - battery_charge - grid_export
    //   = solar - battery_power - grid_power
    // Three-phase models expose direct total load at IR(1089-1090); keep that
    // authoritative value when present, otherwise fall back to the derived formula.
    // The Gateway likewise exposes an authoritative, EV-excluding house load
    // (`p_load`, IR 1618) — keep it when present.
    let has_direct_home_power = (snap
        .device_type
        .needs_three_phase_input_blocks()
        || snap.device_type.needs_gateway_input_blocks())
        && snap.home_power > 0;
    if !has_direct_home_power {
        snap.home_power = snap.solar_power - snap.battery_power - snap.grid_power;
    }

    // Gateway: derive grid power from the energy balance. The gateway measures
    // solar (`p_pv`) and house load (`p_load`, excludes EV) directly, plus
    // battery power (`p_aio_total`, GE sign: + = discharge). Grid power has no
    // dedicated register — it is the residual that balances the equation:
    //   grid_export(HEM, +) = solar + battery_discharge - home
    //                        = solar - battery_power(HEM +charge) - home
    // (algebra: grid_import - export = home - p_aio_total_ge - pv → solved for
    // HEM's +export convention; see gateway-design/IMPLEMENTATION-PLAN.md §5.)
    if snap.device_type.needs_gateway_input_blocks()
        && gateway_is_valid(&snap.gateway_software_version)
    {
        snap.grid_power = snap.solar_power - snap.battery_power - snap.home_power;
    }

    // Populate the synthetic built-in grid CT meter (address 0x00) with
    // import/export energy totals. The three-phase meter decoder sets these
    // to 0.0 because the power-only registers IR(1079-1082) carry no energy
    // data — the lifetime totals live in IR(1382-1383)/IR(1386-1387) which
    // are decoded separately by decode_input_1360_1413. Single-phase does
    // not create a synthetic meter with address 0x00 (the grid CT is the
    // inverter itself), so this only affects three-phase/HV models.
    for meter in &mut snap.meters {
        if meter.address == 0x00 {
            meter.e_import_active_kwh = snap.total_import_kwh;
            meter.e_export_active_kwh = snap.total_export_kwh;
        }
    }

    // Compute consumption today from energy balance (matching the GE app).
    // IR(35) is AC charge today, NOT house consumption — the reference library
    // confirmed this via sentinel cross-correlation (#174). Single-phase inverters
    // (Gen1/Gen2/Gen3Hybrid) have NO native consumption register, so consumption
    // is derived from the energy balance formula:
    //   consumption = solar_today + import_today - export_today - ac_charge_today
    // Battery DC charge/discharge throughput nets out and is not a term.
    // Three-phase/HV models expose e_load_today at IR(1396-1397) — when that block
    // was decoded, the direct register value is preserved instead of computing.
    // The direct read is detected by checking whether today_ac_charge_kwh and
    // today_consumption_kwh came from different registers (they are set to the same
    // IR(35) value in decode_input_0_59, but diverge after decode_input_1360_1413).
    let has_direct_consumption =
        snap.today_ac_charge_kwh != snap.today_consumption_kwh && snap.today_consumption_kwh > 0.0;
    if !has_direct_consumption {
        snap.today_consumption_kwh = snap.today_solar_kwh + snap.today_import_kwh
            - snap.today_export_kwh
            - snap.today_ac_charge_kwh;
        if snap.today_consumption_kwh < 0.0 {
            snap.today_consumption_kwh = 0.0;
        }
    }

    // Derive battery mode from the three key holding registers.
    snap.battery_mode = BatteryMode::from_registers(
        raw.battery_power_mode,
        raw.enable_discharge,
        raw.battery_soc_reserve,
    );

    // Expose max slot counts so the frontend adapts to the device model.
    snap.max_charge_slots = snap.device_type.max_charge_slots();
    snap.max_discharge_slots = snap.device_type.max_discharge_slots();

    // Note: we intentionally do NOT override TimedDemand/TimedExport to
    // Eco/ExportPaused when discharge slots are empty. Doing so prevents the
    // user from switching to timed mode before configuring slots — the decoder
    // would immediately override back to Eco on the next poll. The inverter
    // simply won't discharge if there are no active slots, which is correct.

    // Charge slots: expose effective enabled state to the UI based on the
    // master enable_charge flag. However, the per-slot `enabled` field
    // should always reflect whether the slot has configured times, not the
    // master flag. Gating on enable_charge causes the UI to hide configured
    // charge slots after mode switches (ECO ←→ Timed) when some inverter
    // firmware clears HR 96 during the transition. The slot times are still
    // correctly stored in the registers — only the enable_charge flag is 0.
    //
    // Instead, the frontend (ControlPage) uses enable_charge to derive
    // force_charge_active (enable_charge && in_charge_window), which correctly
    // shows whether the schedule is currently active — without hiding the
    // schedule configuration from the user.
    //
    // Discharge slots are intentionally NOT gated on enable_discharge here
    // (for the same reason — see commits around issue #41).
    snap
}

// ---------------------------------------------------------------------------
// Per-block decoders
// ---------------------------------------------------------------------------

// IR(0) `status` follows the givenergy-modbus `Status` enum:
//   0 = Waiting, 1 = Normal, 2 = Warning, 3 = Fault, 4 = Flashing update.
// FAULT is the inverter's authoritative self-declared fault/trip state.
const STATUS_FAULT: u16 = 3;

// IR(49) `system_mode` follows the givenergy-modbus `WorkMode` enum:
//   0 = Initialising, 1 = OffGrid (islanded / grid lost), 2 = OnGrid,
//   3 = Fault, 4 = Update. OFF_GRID is the inverter's authoritative
//   self-declared "grid lost" state and the primary grid-loss signal.
const SYSTEM_MODE_OFF_GRID: u16 = 1;

// IR(40) is the low word of the 32-bit `fault_code` packed at IR(39)+IR(40)
// (`uint32 = (IR39 << 16) + IR40`). Bit meanings per the givenergy-modbus
// `_inverter_fault_code` table (inverter.py), documented there as "not verified
// against official firmware docs". Only the "No Utility" bit is used — as a
// corroborating signal for grid_loss (system_mode is authoritative). There is
// no "inverter trip" or "battery over temperature" bit in the table, so those
// two conditions are taken from the `status` and `charger_warning_code`
// registers respectively instead of fault bits.
const GRID_LOSS_MASK_IR40: u16 = 1 << 7; // bit 7 = "No Utility" (grid loss)

fn grid_online_from_ac(voltage: f32, frequency: f32) -> bool {
    // Fallback for models/blocks that do not expose the single-phase IR(40)
    // status word. GivEnergy reports the grid AC reference near zero during an outage.
    voltage > 50.0 && frequency > 1.0
}

fn decode_input_0_59(data: &[u16], snap: &mut InverterSnapshot) {
    // -- PV --
    snap.pv1_power = get_reg(data, 18) as i32; // IR(18): p_pv1 (W)
    snap.pv2_power = get_reg(data, 20) as i32; // IR(20): p_pv2 (W)
    snap.solar_power = snap.pv1_power + snap.pv2_power;
    snap.pv1_voltage = get_reg(data, 1) as f32 * 0.1; // IR(1):  v_pv1 (/10 V)
    snap.pv2_voltage = get_reg(data, 2) as f32 * 0.1; // IR(2):  v_pv2 (/10 V)
    snap.pv1_current = get_reg(data, 8) as f32 * 0.1; // IR(8):  i_pv1 (/10 A)
    snap.pv2_current = get_reg(data, 9) as f32 * 0.1; // IR(9):  i_pv2 (/10 A)

    // -- Battery --
    // IR(52): p_battery (int16 W) — inverter convention: positive = DISCHARGING.
    // We negate so our model uses positive = charging throughout.
    snap.battery_power = -signed(get_reg(data, 52));
    snap.soc = get_reg(data, 59) as u8; // IR(59): battery_soc (%)
    snap.battery_voltage = get_reg(data, 50) as f32 * 0.01; // IR(50): v_battery (/100 V)
                                                            // IR(51): i_battery — negate to match power sign convention (positive = charging current)
    snap.battery_current = -signed(get_reg(data, 51)) as f32 * 0.01;
    snap.battery_state = BatteryState::from_power(snap.battery_power);
    snap.battery_temperature = get_reg(data, 56) as f32 * 0.1; // IR(56): t_battery (/10 °C)

    // -- Grid --
    snap.grid_power = signed(get_reg(data, 30)); // IR(30): p_grid_out (int16 W, +exporting/-importing)
    snap.grid_voltage = get_reg(data, 5) as f32 * 0.1; // IR(5):  v_ac1 (/10 V)
    snap.grid_frequency = get_reg(data, 13) as f32 * 0.01; // IR(13): f_ac1 (/100 Hz)

    // -- Fault / status detection --
    // Each condition uses its authoritative self-declared register rather than
    // the unverified IR(40) fault-word bits (whose layout givenergy-modbus marks
    // "not verified against official firmware docs"):
    //   • grid_loss:         IR(49) `system_mode` == OFF_GRID, corroborated by
    //                         the IR(40) bit 7 "No Utility" fault bit (helps
    //                         during boot before system_mode is populated).
    //   • inverter_trip:     IR(0) `status` == FAULT (the inverter's own flag).
    //   • battery_over_temp: IR(57) `charger_warning_code` == 1 (device-reported).
    //
    // Every device type uses the actual grid AC voltage/frequency as the
    // primary grid-presence signal. GivEnergy hardware reports both near zero
    // during a genuine outage, so `grid_online_from_ac()` is the authoritative
    // electrical ground truth regardless of the software `system_mode` register.
    //
    // AC-coupled inverters (ACCoupled / ACCoupledMk2) treat voltage/frequency
    // as the sole signal because their system_mode register (IR(49)) can report
    // OffGrid during normal grid-connected operation.
    //
    // For all other device types the system_mode register (IR 49) and the
    // IR(40) bit-7 "No Utility" fault bit are used as corroborating signals —
    // grid loss is only reported when BOTH the electrical readings AND the
    // software register(s) agree. This prevents false positives from transient
    // system_mode / fault-bit fluctuations reported by some firmware versions
    // (Gen3 Hybrid devices, notably) while still recognising genuine outages.
    let status = get_reg(data, 0); // IR(0): inverter status (Status enum)
    let system_mode = get_reg(data, 49); // IR(49): system/work mode (WorkMode enum)
    let no_utility = (get_reg(data, 40) & GRID_LOSS_MASK_IR40) != 0; // bit 7
    let ac_present = grid_online_from_ac(snap.grid_voltage, snap.grid_frequency);
    if matches!(
        snap.device_type,
        DeviceType::ACCoupled | DeviceType::ACCoupledMk2
    ) {
        snap.grid_online = ac_present;
        snap.grid_loss = !ac_present;
    } else {
        // Corroborate the software signals against the actual electrical
        // readings. Both must indicate grid loss for it to be reported,
        // avoiding false positives from transient register fluctuations.
        snap.grid_loss =
            (system_mode == SYSTEM_MODE_OFF_GRID || no_utility) && !ac_present;
        snap.grid_online = !snap.grid_loss;
    }
    snap.inverter_trip = status == STATUS_FAULT;
    snap.battery_over_temp = get_reg(data, 57) == 1; // IR(57): charger warning code

    // -- Inverter --
    snap.inverter_temperature = get_reg(data, 41) as f32 * 0.1; // IR(41): t_inverter_heatsink (/10 °C)

    // -- Energy totals (all in /10 kWh) --
    // IR(44) is e_pv_generation_today (per givenergy-modbus sentinel
    // cross-correlation, confirmed against the GE app's Energy-today
    // screen — see #174). Fall back to the older per-string daily
    // registers (IR(17)/IR(19)) when IR(44) is unavailable/zero.
    let pv_generation_today = get_reg(data, 44); // IR(44): e_pv_generation_today
    if pv_generation_today > 0 {
        snap.today_solar_kwh = pv_generation_today as f32 * 0.1;
    } else {
        // Fallback: only include PV2's daily energy if PV2 has panels connected.
        // IR(19) can return stale or garbage data when no second PV string is present.
        let pv2_today = if snap.pv2_voltage > 0.0 {
            get_reg(data, 19) as f32
        } else {
            0.0
        };
        snap.today_solar_kwh = (get_reg(data, 17) as f32 + pv2_today) * 0.1; // IR(17)+IR(19)
    }
    snap.today_import_kwh = get_reg(data, 26) as f32 * 0.1; // IR(26): e_grid_in_day
    snap.today_export_kwh = get_reg(data, 25) as f32 * 0.1; // IR(25): e_grid_out_day
    snap.today_charge_kwh = get_reg(data, 36) as f32 * 0.1; // IR(36): e_battery_charge_day
    snap.today_discharge_kwh = get_reg(data, 37) as f32 * 0.1; // IR(37): e_battery_discharge_day
                                                               // IR(35) is AC charge (grid → battery), NOT house consumption. Per the
                                                               // givenergy-modbus sentinel cross-correlation (#174), the GivTCP-era
                                                               // "e_load_day" label was a mislabel. The three-phase model confirms this:
                                                               // IR(1376/1377) is e_ac_charge_today and IR(1396/1397) is e_load_today.
                                                               // For single-phase there is no direct load register — consumption is
                                                               // computed (matching the reference's e_consumption_today @property).
    snap.today_ac_charge_kwh = get_reg(data, 35) as f32 * 0.1; // IR(35): e_ac_charge_today
                                                               // Consumption = solar + import - export - ac_charge (ref formula).
                                                               // Clamped at 0 to avoid negative from meter rounding noise.
    snap.today_consumption_kwh = (snap.today_solar_kwh + snap.today_import_kwh
        - snap.today_export_kwh
        - snap.today_ac_charge_kwh)
        .max(0.0);
    snap.total_export_kwh = decode_lifetime_total_kwh(get_reg(data, 21), get_reg(data, 22)); // IR(21-22): e_grid_out_total
    snap.total_import_kwh = decode_lifetime_total_kwh(get_reg(data, 32), get_reg(data, 33));
    // IR(32-33): e_grid_in_total // keep raw IR(35) for energy balance
}

/// IR 180-239: Alternative battery energy counters (unverified range).
///
/// Per givenergy-modbus reference, IR(180)/IR(181) carry alternative total
/// battery discharge/charge energy (deci-kWh). IR(182)/IR(183) carry
/// alternative today counters. IR(184-239) are not in the authoritative
/// givenergy-modbus register map and are decoded as 0 — values from this
/// block should be treated as unverified/experimental.
fn decode_input_180_239(data: &[u16], snap: &mut InverterSnapshot) {
    // IR(180)/IR(181) are confirmed by givenergy-modbus as alternative
    // battery total energy counters (deci-kWh).
    snap.total_discharge_kwh = get_reg(data, 0) as f32 * 0.1; // IR(180): e_battery_discharge_total_alt1
    snap.total_charge_kwh = get_reg(data, 1) as f32 * 0.1; // IR(181): e_battery_charge_total_alt1
                                                           // IR(182-239) are NOT in the authoritative givenergy-modbus register
                                                           // map for any model — decoded as 0. These offsets are deliberately
                                                           // left unread to avoid silently shipping values from an unverified
                                                           // address range. If new registers are discovered here in the future,
                                                           // add explicit decoders and a note about which firmware/hardware
                                                           // revision they were confirmed on.
}

/// Decode holding registers 0-59 (configuration part 1).
fn decode_holding_0_59(data: &[u16], snap: &mut InverterSnapshot, raw: &mut RawConfig) {
    // Device type: HR(0)
    let dtc_raw = get_reg(data, 0);
    snap.device_type = DeviceType::from_register(dtc_raw);
    snap.device_type_code = format!("{:04X}", dtc_raw);

    // Serial number: HR(13-17), 5 registers = 10 Latin-1 chars
    snap.inverter_serial = decode_serial(data, 13, 5);

    // ARM firmware version: HR(21)
    let arm_fw = get_reg(data, 21);
    snap.firmware_version = if arm_fw > 0 {
        format!("{}", arm_fw)
    } else {
        String::new()
    };
    // DSP firmware version: HR(19)
    let dsp_fw = get_reg(data, 19);
    snap.dsp_firmware_version = if dsp_fw > 0 {
        format!("{}", dsp_fw)
    } else {
        String::new()
    };
    // Refine 0x20xx hybrid generation using ARM firmware.
    snap.device_type = snap.device_type.refine_with_arm_fw(dtc_raw, arm_fw);
    snap.device_type_display = snap.device_type.display_name().to_string();
    snap.max_ac_power_w =
        DeviceType::max_ac_power_for_dtc(dtc_raw, snap.device_type.max_ac_power_w());

    // Inverter wall-clock time: HR(35-40), displayed exactly as read with no
    // timezone conversion.
    snap.inverter_time = decode_system_time(data);

    // Battery capacity in kWh = HR(55) × nominal_voltage / 1000
    // HR(55) reports total system Ah (inverter firmware accounts for all modules).
    // GivTCP does not scale this value either.
    let capacity_ah = get_reg(data, 55) as f32; // HR(55): battery_capacity_ah
    let nominal_voltage = snap.device_type.nominal_battery_voltage();
    snap.battery_capacity_kwh = capacity_ah * nominal_voltage / 1000.0;

    // Battery power mode: HR(27) — 0=export, 1=eco
    raw.battery_power_mode = get_reg(data, 27);

    // Battery calibration stage: HR(29) — 0=off, 5=balance
    snap.battery_calibration_stage = get_reg(data, 29) as u8;
    // supports_battery_calibration is set in the poll loop after battery modules are decoded,
    // so it can use the actual BMS firmware version rather than inverter device type.

    // External CT ammeter enabled: HR(7) — bool
    snap.enable_ammeter = get_reg(data, 7) != 0;

    // CT clamp reversed: HR(42) — bool
    snap.enable_reversed_ct_clamp = get_reg(data, 42) != 0;

    // External meter type: HR(47) — 0=CT/EM418, 1=EM115
    snap.meter_type = get_reg(data, 47) as u8;

    // Enable charge target (winter mode): HR(20) — bool
    snap.enable_charge_target = get_reg(data, 20) != 0;

    // Enable discharge: HR(59) — bool
    snap.enable_discharge = get_reg(data, 59) != 0;
    raw.enable_discharge = snap.enable_discharge;

    // Active power rate: HR(50) — inverter max output percentage (0-100)
    snap.active_power_rate = get_reg(data, 50) as u8;

    // Max battery power from inverter hardware.
    // Per givenergy-modbus: hybrid (DTC "20xx") uses ARM FW version to determine
    // generation; other models use a lookup table.
    //   Hybrid Gen2/3 (FW century 3,8,9): 3600W, Gen1: 2600W
    //   AC (30xx): 3000W, All-in-One (80xx): varies
    snap.max_battery_power_w = DeviceType::max_battery_power_for_dtc(
        dtc_raw,
        arm_fw,
        snap.device_type.max_battery_power_w(),
    );
    // Cap at half battery capacity (per GivTCP formula)
    let battery_capacity_w = snap.battery_capacity_kwh * 1000.0;
    snap.max_battery_power_w = snap
        .max_battery_power_w
        .min((battery_capacity_w / 2.0) as u32);

    // Charge slot 2: HR(31-32)
    snap.charge_slots[1] = decode_timeslot(data, 31, 32);

    // Discharge slot 1: HR(56-57)
    snap.discharge_slots[0] = decode_timeslot(data, 56, 57);

    // Discharge slot 2: HR(44-45)
    snap.discharge_slots[1] = decode_timeslot(data, 44, 45);
}

/// Decode holding registers 60-119 (configuration part 2).
///
/// Indices within `data` are offset by 60 from the absolute holding register address.
fn decode_holding_60_119(data: &[u16], snap: &mut InverterSnapshot, raw: &mut RawConfig) {
    // Charge slot 1: HR(94-95) → indices 34, 35
    snap.charge_slots[0] = decode_timeslot(data, 94 - 60, 95 - 60);

    // Enable charge: HR(96) → index 36
    snap.enable_charge = get_reg(data, 96 - 60) != 0;

    // Battery SOC reserve: HR(110) → index 50
    snap.battery_reserve = (get_reg(data, 110 - 60) as u8).clamp(4, 100);
    raw.battery_soc_reserve = snap.battery_reserve as u16;

    // Battery charge/discharge limits for DC-coupled hybrids: HR(111/112).
    // AC-coupled inverters use HR(313/314) from the AC config block instead;
    // HR(111/112) can read as 0 on AC models and must not overwrite the real limits.
    if !matches!(
        snap.device_type,
        DeviceType::ACCoupled | DeviceType::ACCoupledMk2
    ) {
        snap.charge_rate = get_reg(data, 111 - 60) as u8;
        snap.discharge_rate = get_reg(data, 112 - 60) as u8;
    }

    // Charge target SOC: HR(116) → index 56
    snap.target_soc = (get_reg(data, 116 - 60) as u8).clamp(4, 100);

    // Apply global charge_target_soc to each enabled charge slot
    let global_target = snap.target_soc;
    for slot in &mut snap.charge_slots {
        if slot.enabled {
            slot.target_soc = global_target;
        }
    }
    // Apply the same global fallback to enabled discharge slots — they have
    // no separate global register, and per-slot SOCs (HR 272/275) only get
    // read when the extended HR240-299 block is polled. Without this fallback
    // discharge slots show 0% unless the extended block is available AND the
    // per-slot register reads > 0.
    for slot in &mut snap.discharge_slots {
        if slot.enabled {
            slot.target_soc = global_target;
        }
    }
}

/// Decode holding registers 240-299 (extended 10-slot scheduling).
///
/// Gen3, AIO, and HV Gen3 devices map charge slots 3-10 at HR 246-268
/// with per-slot target SOCs interleaved (e.g. HR 248 = target for slot 3).
/// Discharge slots 3-10 are at HR 276-298 with the same pattern.
///
/// **Charge slot 2** is also mirrored at HR 243-244 on Gen3 firmware
/// (named `charge_slot_2_x` in givenergy-modbus). The extended-block
/// copy is authoritative — it supersedes the classic HR 31-32 values
/// decoded in `decode_holding_0_59`. This matches GivTCP's behaviour
/// where `RegisterMap.CHARGE_SLOT_2_START` resolves to 243 (the later
/// class attribute assignment shadows the original 31).
fn decode_holding_240_299(data: &[u16], snap: &mut InverterSnapshot) {
    // Charge slot 2 from extended block (HR 243-244).
    // This is the authoritative copy on Gen3/AIO/HV-Gen3 firmware that
    // does NOT use the three-phase schedule map.  Three-phase models read
    // slot 2 from HR 1115-1116 instead, so we must not overwrite their
    // slot 2 with stale HR 243-244 data.
    if snap.device_type.supports_gen3_extended()
        && !snap.device_type.uses_three_phase_schedule_slots()
    {
        let cs2_start = get_reg(data, 243 - 240);
        let cs2_end = get_reg(data, 244 - 240);
        if cs2_start != cs2_end {
            if let (Some((sh, sm)), Some((eh, em))) = (decode_hhmm(cs2_start), decode_hhmm(cs2_end))
            {
                snap.charge_slots[1] = ScheduleSlot {
                    enabled: true,
                    start_hour: sh,
                    start_minute: sm,
                    end_hour: eh,
                    end_minute: em,
                    // target_soc will be set below from HR 245
                    target_soc: snap.charge_slots[1].target_soc,
                };
            }
        } else if cs2_start == 0 && cs2_end == 0 {
            // Both zero means the slot is explicitly disabled on Gen3 firmware.
            snap.charge_slots[1].enabled = false;
        }
    }

    // Extended charge slots 3-10 at HR 246-268, offset by 240 in this block
    // Pattern: start, end, target_soc repeating for each slot
    for i in 0..8u16 {
        let base = (246 + i * 3) as usize;
        let offset = base - 240;
        let start_val = get_reg(data, offset);
        let end_val = get_reg(data, offset + 1);
        let target = (get_reg(data, offset + 2) as u8).clamp(4, 100);

        // Disabled when start == end (zero-duration slot)
        if start_val == end_val {
            continue;
        }

        if let (Some((sh, sm)), Some((eh, em))) = (decode_hhmm(start_val), decode_hhmm(end_val)) {
            let idx = (i + 2) as usize; // 0-based index 2..9 (slots 3..10)
            if idx < snap.charge_slots.len() {
                snap.charge_slots[idx] = ScheduleSlot {
                    enabled: true,
                    start_hour: sh,
                    start_minute: sm,
                    end_hour: eh,
                    end_minute: em,
                    target_soc: target,
                };
            }
        }
    }

    // Per-slot target SOCs for slots 1-2 (HR 242, 245) — these augment
    // the global target_soc set in decode_holding_60_119
    let t1 = (get_reg(data, 242 - 240) as u8).clamp(4, 100);
    let t2 = (get_reg(data, 245 - 240) as u8).clamp(4, 100);
    if snap.charge_slots[0].enabled && t1 > 0 {
        snap.charge_slots[0].target_soc = t1;
    }
    if snap.charge_slots[1].enabled && t2 > 0 {
        snap.charge_slots[1].target_soc = t2;
    }

    // Extended discharge slots 3-10 at HR 276-298, same pattern
    for i in 0..8u16 {
        let base = (276 + i * 3) as usize;
        let offset = base - 240;
        // Check bounds — the 60-register block may not cover all 10 slots
        if offset + 2 >= data.len() {
            break;
        }
        let start_val = get_reg(data, offset);
        let end_val = get_reg(data, offset + 1);
        let target = (get_reg(data, offset + 2) as u8).clamp(4, 100);

        // Disabled when start == end (zero-duration slot)
        if start_val == end_val {
            continue;
        }

        if let (Some((sh, sm)), Some((eh, em))) = (decode_hhmm(start_val), decode_hhmm(end_val)) {
            let idx = (i + 2) as usize; // 0-based index 2..9 (slots 3..10)
            if idx < snap.discharge_slots.len() {
                snap.discharge_slots[idx] = ScheduleSlot {
                    enabled: true,
                    start_hour: sh,
                    start_minute: sm,
                    end_hour: eh,
                    end_minute: em,
                    target_soc: target,
                };
            }
        }
    }

    // Per-slot discharge target SOCs for slots 1-2 (HR 272, 275)
    let dt1 = (get_reg(data, 272 - 240) as u8).clamp(4, 100);
    let dt2 = (get_reg(data, 275 - 240) as u8).clamp(4, 100);
    if snap.discharge_slots[0].enabled && dt1 > 0 {
        snap.discharge_slots[0].target_soc = dt1;
    }
    if snap.discharge_slots[1].enabled && dt2 > 0 {
        snap.discharge_slots[1].target_soc = dt2;
    }
}

/// Decode holding registers 300-359 (AC configuration block).
///
/// Contains export priority (311), AC charge/discharge limits (313/314),
/// EPS enable (317), pause mode (318), and pause slot times (319-320).
fn decode_holding_300_359(data: &[u16], snap: &mut InverterSnapshot) {
    // HR 311: export priority (0=battery, 1=grid, 2=load)
    snap.ac_export_priority = get_reg(data, 311 - 300) as u8;

    // HR 313/314: AC-coupled charge/discharge power percentage limits.
    // AC-coupled inverters do not use the DC hybrid HR 111/112 registers,
    // so expose these through the existing charge_rate/discharge_rate fields.
    snap.charge_rate = get_reg(data, 313 - 300) as u8;
    snap.discharge_rate = get_reg(data, 314 - 300) as u8;

    // HR 317: EPS enable (bool)
    snap.ac_eps_enabled = get_reg(data, 317 - 300) != 0;

    // HR 318: battery pause mode (0=disabled)
    snap.battery_pause_mode = get_reg(data, 318 - 300) as u8;

    // HR 319-320: battery pause slot
    snap.battery_pause_slot = decode_timeslot(data, 319 - 300, 320 - 300);
}

/// Decode holding registers 1080-1124 (three-phase battery/control block).
///
/// Three-phase and commercial/HV models mirror key single-phase battery controls
/// into the 1000-range register bank. Relevant mappings from givenergy-modbus
/// `model/inverter_threephase.py`:
///   HR 1108 = battery_discharge_limit_ac
///   HR 1109 = battery_soc_reserve
///   HR 1110 = battery_charge_limit_ac
///   HR 1111 = charge_target_soc
///   HR 1112 = ac_charge_enable
///   HR 1122 = force_discharge_enable
///   HR 1123 = force_charge_enable
///
/// HR 1000-1079: Three-phase high configuration block.
///
/// Covers registers HR 1005 (REAL_TIME_CONTROL) and HR 1078
/// (BATTERY_RESERVE_PERCENT) that the lower config block (1080-1124)
/// doesn't reach. Additional registers are decoded by the three-phase
/// register getter (inverter_threephase.py).
fn decode_holding_1000_1079(_data: &[u16], _snap: &mut InverterSnapshot, _raw: &mut RawConfig) {
    // Currently a no-op — this block is read to prevent "Unknown block"
    // warnings. HR 1005 and 1078 are in SAFE_WRITE_REGS but not yet
    // displayed on the dashboard.
}

fn decode_holding_1080_1124(data: &[u16], snap: &mut InverterSnapshot, raw: &mut RawConfig) {
    snap.discharge_rate = get_reg(data, 1108 - 1080) as u8;
    snap.battery_reserve = (get_reg(data, 1109 - 1080) as u8).clamp(4, 100);
    raw.battery_soc_reserve = snap.battery_reserve as u16;
    snap.charge_rate = get_reg(data, 1110 - 1080) as u8;
    snap.target_soc = (get_reg(data, 1111 - 1080) as u8).clamp(4, 100);

    // Three-phase schedule slots 1-2 live in this block rather than the
    // single-phase HR31/32, HR44/45, HR56/57 and HR94/95 locations.
    snap.charge_slots[0] = decode_timeslot(data, 1113 - 1080, 1114 - 1080);
    snap.charge_slots[1] = decode_timeslot(data, 1115 - 1080, 1116 - 1080);
    snap.discharge_slots[0] = decode_timeslot(data, 1118 - 1080, 1119 - 1080);
    snap.discharge_slots[1] = decode_timeslot(data, 1120 - 1080, 1121 - 1080);

    // These are distinct from the single-phase HR96/59 flags but represent
    // the equivalent three-phase force/AC-charge state.
    snap.enable_charge = get_reg(data, 1112 - 1080) != 0 || get_reg(data, 1123 - 1080) != 0;
    snap.enable_discharge = get_reg(data, 1122 - 1080) != 0;
    raw.enable_discharge = snap.enable_discharge;
}

// ===========================================================================
// Three-phase input register decoders (IR 1000-1413)
// ===========================================================================
//
// Three-phase and HV/commercial models expose real-time measurements in the
// IR(1000-1414) range instead of the single-phase IR(0-59). The mappings below
// come directly from `givenergy_modbus/model/inverter_threephase.py`.
//
// Convention for our snapshot:
//   battery_power > 0 = charging
//   grid_power    > 0 = exporting (matching single-phase IR(30) sign)

/// IR 1000-1059: PV voltage, current and power.
fn decode_input_1000_1059(data: &[u16], snap: &mut InverterSnapshot) {
    // Offsets within this block (subtract 1000):
    //   1 → IR(1001): v_pv1 (/10 V)
    //   2 → IR(1002): v_pv2 (/10 V)
    //   9 → IR(1009): i_pv1 (/10 A)
    //  10 → IR(1010): i_pv2 (/10 A)
    //  17 → IR(1017)+IR(1018): p_pv1 (uint32 /10 W)
    //  19 → IR(1019)+IR(1020): p_pv2 (uint32 /10 W)
    snap.pv1_voltage = get_reg(data, 1) as f32 * 0.1;
    snap.pv2_voltage = get_reg(data, 2) as f32 * 0.1;
    snap.pv1_current = get_reg(data, 9) as f32 * 0.1;
    snap.pv2_current = get_reg(data, 10) as f32 * 0.1;
    let p_pv1 = uint32(get_reg(data, 17), get_reg(data, 18)) as f32 * 0.1;
    let p_pv2 = uint32(get_reg(data, 19), get_reg(data, 20)) as f32 * 0.1;
    snap.pv1_power = p_pv1 as i32;
    snap.pv2_power = p_pv2 as i32;
    snap.solar_power = snap.pv1_power + snap.pv2_power;
}

/// IR 1060-1119: Grid, inverter output, load and EPS-bound measurements.
fn decode_input_1060_1119(data: &[u16], snap: &mut InverterSnapshot) {
    // Offsets within this block (subtract 1060):
    //   1 → IR(1061): v_ac1 (/10 V)
    //   2 → IR(1062): v_ac2 (/10 V)
    //   3 → IR(1063): v_ac3 (/10 V)
    //   4 → IR(1064): i_ac1 (/10 A)
    //   5 → IR(1065): i_ac2 (/10 A)
    //   6 → IR(1066): i_ac3 (/10 A)
    //   7 → IR(1067): f_ac1 (/100 Hz)
    //   8 → IR(1068): power_factor (int16, /1000)
    //   9 → IR(1069)+IR(1070): p_inverter_out (int32 /10 W)
    //  13 → IR(1073)+IR(1074): p_grid_apparent (uint32 /10 VA)
    //  19 → IR(1079)+IR(1080): p_meter_import (uint32 /10 W)
    //  21 → IR(1081)+IR(1082): p_meter_export (uint32 /10 W)
    //  23 → IR(1083): p_load_ac1 (/10 W)
    //  24 → IR(1084): p_load_ac2 (/10 W)
    //  25 → IR(1085): p_load_ac3 (/10 W)
    //  29 → IR(1089)+IR(1090): p_load_all (uint32 /10 W)
    let v1 = get_reg(data, 1) as f32 * 0.1;
    let v2 = get_reg(data, 2) as f32 * 0.1;
    let v3 = get_reg(data, 3) as f32 * 0.1;
    snap.grid_voltage = v1;
    snap.grid_frequency = get_reg(data, 7) as f32 * 0.01;
    let max_grid_voltage = v1.max(v2).max(v3);
    snap.grid_online = grid_online_from_ac(max_grid_voltage, snap.grid_frequency);

    let i1 = get_reg(data, 4) as f32 * 0.1;
    let i2 = get_reg(data, 5) as f32 * 0.1;
    let i3 = get_reg(data, 6) as f32 * 0.1;

    // Grid power sign: positive = exporting (matches single-phase convention).
    let p_import = uint32(get_reg(data, 19), get_reg(data, 20)) as f32 * 0.1;
    let p_export = uint32(get_reg(data, 21), get_reg(data, 22)) as f32 * 0.1;
    let p_grid = (p_export - p_import) as i32;
    snap.grid_power = p_grid;

    // Home/load power: total load across all three phases (uint32 /10 W).
    snap.home_power = (uint32(get_reg(data, 29), get_reg(data, 30)) as f32 * 0.1) as i32;

    // Create a synthetic meter entry from the inverter's built-in grid CT.
    // Positive total = import (matching MeterData convention).
    let i_total = (i1 + i2 + i3) / 3.0;

    // IR(1068): power_factor (int16, /1000) — three-phase inverter-wide PF.
    let pf_raw = signed(get_reg(data, 8)) as f32 * 0.001;
    // IR(1073-1074): p_grid_apparent (uint32 /10 VA).
    let p_apparent = uint32(get_reg(data, 13), get_reg(data, 14)) as f32 * 0.1;

    snap.meters.push(MeterData {
        address: 0x00, // synthetic "built-in grid CT"
        v_phase_1: v1,
        v_phase_2: v2,
        v_phase_3: v3,
        i_phase_1: i1,
        i_phase_2: i2,
        i_phase_3: i3,
        i_total,
        // Three-phase inverters have no per-phase signed grid power registers.
        // IR(1083-1085) carry unsigned load-only power (p_load_ac1..3), not net
        // grid flow. IR(1091-1093) carry unsigned export-only power (p_out_ac1..3).
        // Neither can produce a signed net per-phase value, so set to 0 (unknown).
        // The UI hides per-phase power for the synthetic CT (address 0x00).
        p_active_phase_1: 0,
        p_active_phase_2: 0,
        p_active_phase_3: 0,
        p_active_total: -p_grid,
        p_reactive_total: 0, // no reactive register available
        p_apparent_total: p_apparent as i32,
        pf_total: pf_raw.abs().clamp(0.0, 1.0),
        frequency: snap.grid_frequency,
        e_import_active_kwh: 0.0,
        e_export_active_kwh: 0.0,
    });
}

/// IR 1120-1179: Battery, BMS, temperatures and battery power.
fn decode_input_1120_1179(data: &[u16], snap: &mut InverterSnapshot) {
    // Offsets within this block (subtract 1120):
    //   8 → IR(1128): t_inverter (/10 °C)
    //  11 → IR(1131): v_battery_bms (/10 V)
    //  12 → IR(1132): battery_soc (%)
    //  16 → IR(1136)+IR(1137): p_battery_discharge (uint32 /10 W)
    //  18 → IR(1138)+IR(1139): p_battery_charge   (uint32 /10 W)
    //  20 → IR(1140): i_battery (int16 /10 A)
    snap.inverter_temperature = get_reg(data, 8) as f32 * 0.1;
    snap.battery_voltage = get_reg(data, 11) as f32 * 0.1;
    snap.soc = get_reg(data, 12) as u8;

    let p_discharge = uint32(get_reg(data, 16), get_reg(data, 17)) as f32 * 0.1;
    let p_charge = uint32(get_reg(data, 18), get_reg(data, 19)) as f32 * 0.1;
    // Our convention: positive = charging.
    snap.battery_power = (p_charge - p_discharge) as i32;
    snap.battery_state = BatteryState::from_power(snap.battery_power);
    snap.battery_current = signed(get_reg(data, 20)) as f32 * 0.1;
}

/// IR 1180-1239: EPS measurements (not currently captured — placeholder).
fn decode_input_1180_1239(_data: &[u16], _snap: &mut InverterSnapshot) {
    // EPS-specific data (v_eps_ac1..3, i_eps_ac1..3, p_eps_ac1..3) lives here.
    // Not yet exposed in InverterSnapshot; reserved for future use.
}

/// IR 1240-1299: Additional power meters (export, secondary meter).
fn decode_input_1240_1299(data: &[u16], snap: &mut InverterSnapshot) {
    // IR(1240-1241): p_export (uint32 /10 W) — alternative address for export
    // IR(1244-1245): p_meter2 (uint32 /10 W) — second CT meter if installed
    let p_meter2 = uint32(get_reg(data, 4), get_reg(data, 5)) as f32 * 0.1;
    if p_meter2 > 0.0 {
        snap.meters.push(MeterData {
            address: 0x09, // second external CT
            v_phase_1: snap.grid_voltage,
            v_phase_2: 0.0,
            v_phase_3: 0.0,
            i_phase_1: 0.0,
            i_phase_2: 0.0,
            i_phase_3: 0.0,
            i_total: 0.0,
            p_active_phase_1: p_meter2 as i32,
            p_active_phase_2: 0,
            p_active_phase_3: 0,
            p_active_total: -p_meter2 as i32, // positive = import
            p_reactive_total: 0,
            p_apparent_total: (p_meter2 * 10.0) as i32, // rough: apparent ≈ active for resistive loads
            pf_total: 1.0,
            frequency: snap.grid_frequency,
            e_import_active_kwh: 0.0,
            e_export_active_kwh: 0.0,
        });
    }
}

/// IR 1300-1359: Fault codes and firmware identification.
fn decode_input_1300_1359(data: &[u16], snap: &mut InverterSnapshot) {
    // Offsets within this block (subtract 1300):
    //  17 → IR(1317)-IR(1319): software version string (3 registers = 6 chars)
    //  20 → IR(1320)-IR(1324): tph_firmware_version string (5 registers = 10 chars)
    //  25 → IR(1325): ac_dsp_firmware_version
    //  26 → IR(1326): dc_dsp_firmware_version
    //  27 → IR(1327): tph_arm_firmware_version
    //
    // Decode the 5-register firmware string (IR 1320-1324) as the display version,
    // matching GivTCP which uses GEInv.tph_firmware_version for the firmware label.
    let fw_string = decode_serial(data, 20, 5);
    let fw_string = fw_string.trim_matches('\0');
    if !fw_string.is_empty() {
        snap.firmware_version = fw_string.to_string();
    }

    // AC-side DSP: IR(1325) as uint16
    let ac_dsp = get_reg(data, 25);
    if ac_dsp > 0 {
        snap.dsp_firmware_version = format!("{}", ac_dsp);
    }

    // DC-side DSP: IR(1326) as uint16
    let dc_dsp = get_reg(data, 26);
    if dc_dsp > 0 {
        snap.dc_dsp_firmware_version = format!("{}", dc_dsp);
    }
}

/// IR 1360-1413: Daily and total energy counters.
fn decode_input_1360_1413(data: &[u16], snap: &mut InverterSnapshot) {
    // Offsets within this block (subtract 1360):
    //  0+1 → IR(1360)+IR(1361): e_inverter_out_today (uint32 /10 kWh)
    //  6+7 → IR(1366)+IR(1367): e_pv1_today        (uint32 /10 kWh)
    //  8+9 → IR(1368)+IR(1369): e_pv1_total        (uint32 /10 kWh)
    // 10+11 → IR(1370)+IR(1371): e_pv2_today       (uint32 /10 kWh)
    // 14+15 → IR(1374)+IR(1375): e_pv_total        (uint32 /10 kWh, lifetime)
    // 16+17 → IR(1376)+IR(1377): e_ac_charge_today (uint32 /10 kWh)
    // 20+21 → IR(1380)+IR(1381): e_import_today    (uint32 /10 kWh)
    // 24+25 → IR(1384)+IR(1385): e_export_today    (uint32 /10 kWh)
    // 28+29 → IR(1388)+IR(1389): e_battery_discharge_today (uint32 /10 kWh)
    // 32+33 → IR(1392)+IR(1393): e_battery_charge_today    (uint32 /10 kWh)
    // 36+37 → IR(1396)+IR(1397): e_load_today              (uint32 /10 kWh)
    // 52+53 → IR(1412)+IR(1413): e_pv_today                (uint32 /10 kWh)
    let pv_today = uint32(get_reg(data, 52), get_reg(data, 53));
    snap.today_solar_kwh = if pv_today > 0 {
        pv_today as f32 * 0.1
    } else {
        (uint32(get_reg(data, 6), get_reg(data, 7)) + uint32(get_reg(data, 10), get_reg(data, 11)))
            as f32
            * 0.1
    };
    snap.today_import_kwh = uint32(get_reg(data, 20), get_reg(data, 21)) as f32 * 0.1;
    snap.today_export_kwh = uint32(get_reg(data, 24), get_reg(data, 25)) as f32 * 0.1;
    snap.today_charge_kwh = uint32(get_reg(data, 32), get_reg(data, 33)) as f32 * 0.1;
    snap.today_discharge_kwh = uint32(get_reg(data, 28), get_reg(data, 29)) as f32 * 0.1;
    snap.today_consumption_kwh = uint32(get_reg(data, 36), get_reg(data, 37)) as f32 * 0.1;
    snap.today_ac_charge_kwh = uint32(get_reg(data, 16), get_reg(data, 17)) as f32 * 0.1;
    snap.total_import_kwh = decode_lifetime_total_kwh(get_reg(data, 22), get_reg(data, 23)); // IR(1382-1383): e_import_total
    snap.total_export_kwh = decode_lifetime_total_kwh(get_reg(data, 26), get_reg(data, 27)); // IR(1386-1387): e_export_total
    snap.total_charge_kwh = decode_lifetime_total_kwh(get_reg(data, 34), get_reg(data, 35)); // IR(1394-1395): e_battery_charge_total
    snap.total_discharge_kwh = decode_lifetime_total_kwh(get_reg(data, 30), get_reg(data, 31));
    // IR(1390-1391): e_battery_discharge_total
}

/// Decode meter data from raw register values (IR 60-89) into a MeterData struct.
///
/// The register layout matches MeterRegisterGetter from the reference library:
///   IR(60-62): v_phase_1..3 (/10 V)
///   IR(63-67): i_phase_1..3, i_ln, i_total (/100 A)
///   IR(68-71): p_active_phase_1..3, p_active_total (int16 W)
///   IR(72-75): p_reactive_phase_1..3, p_reactive_total (int16 var)
///   IR(76-79): p_apparent_phase_1..3, p_apparent_total (int16 VA)
///   IR(80-83): pf_phase_1..3, pf_total (/1000)
///   IR(84):    frequency (/100 Hz)
///   IR(85-86): e_import_active, e_import_reactive (/10 kWh)
///   IR(87-88): e_export_active, e_export_reactive (/10 kWh)
pub fn decode_meter_data(data: &[u16], address: u8) -> MeterData {
    let get = |idx: usize| -> u16 { data.get(idx).copied().unwrap_or(0) };
    let signed = |idx: usize| -> i32 { get(idx) as i16 as i32 };

    MeterData {
        address,
        v_phase_1: get(0) as f32 * 0.1,
        v_phase_2: get(1) as f32 * 0.1,
        v_phase_3: get(2) as f32 * 0.1,
        i_phase_1: get(3) as f32 * 0.01,
        i_phase_2: get(4) as f32 * 0.01,
        i_phase_3: get(5) as f32 * 0.01,
        i_total: get(7) as f32 * 0.01,
        p_active_phase_1: signed(8),
        p_active_phase_2: signed(9),
        p_active_phase_3: signed(10),
        p_active_total: signed(11),
        p_reactive_total: signed(15),
        p_apparent_total: signed(19),
        pf_total: get(23) as f32 * 0.001,
        frequency: get(24) as f32 * 0.01,
        e_import_active_kwh: get(25) as f32 * 0.1,
        e_export_active_kwh: get(27) as f32 * 0.1,
    }
}

/// Validate that raw meter data represents a real, connected meter.
///
/// Per givenergy-modbus, a meter is present when `v_phase_1` (IR 60) is
/// non-zero. Empty/ghost meter slots always return zero voltage. We reject
/// only extreme corruption (>500V) to avoid accepting garbage, but the
/// presence check is `v_phase_1 != 0` — not `> 100V` — matching the
/// reference library.
///
/// Returns `(is_present, v_phase_1)` so the caller can log the raw voltage
/// for diagnostics.
pub fn validate_meter_data(data: &[u16]) -> (bool, f32) {
    let v1 = data.first().copied().unwrap_or(0) as f32 * 0.1;
    let is_present = v1 > 0.0 && v1 < 500.0;
    (is_present, v1)
}

/// Decode battery block data from a single data slice into a BatteryModule.
///
/// LV Battery BMS input registers (IR 60-119) per givenergy-modbus reference:
///   IR(60-75):   cell voltages in mV (up to 16 cells)
///   IR(76-79):   cell group temperatures in 0.1 °C (groups of 4 cells)
///   IR(80):      v_cells_sum in mV
///   IR(81):      t_bms_mosfet in 0.1 °C
///   IR(82-83):   v_out (uint32, mV)
///   IR(84-85):   cap_calibrated (uint32, 0.01 Ah)
///   IR(86-87):   cap_design (uint32, 0.01 Ah)
///   IR(88-89):   cap_remaining (uint32, 0.01 Ah)
///   IR(90-94):   status/warning packed bytes
///   IR(96):      num_cycles
///   IR(97):      num_cells
///   IR(98):      bms_firmware_version
///   IR(100):     soc (%)
///   IR(103):     t_max (0.1 °C)
///   IR(104):     t_min (0.1 °C)
///   IR(110-114): serial_number (5 regs = 10 Latin-1 chars)
fn decode_battery_block(data: &[u16], index: usize) -> BatteryModule {
    // Cell voltages: IR(60-75), milli-V → V
    let num_cells_raw = get_reg(data, 97 - 60) as usize; // IR(97): num_cells
    let cell_count = num_cells_raw.clamp(0, 16).min(data.len());
    let cell_voltages: Vec<f32> = (0..cell_count)
        .map(|i| get_reg(data, i) as f32 * 0.001) // mV → V
        .collect();

    // Cell group temperatures: IR(76-79), 0.1 °C
    let cell_temperatures: Vec<f32> = (0..4)
        .map(|i| get_reg(data, 76 - 60 + i) as f32 * 0.1)
        .collect();

    // Total voltage: IR(82-83) uint32, mV → V
    let voltage_raw = ((get_reg(data, 82 - 60) as u32) << 16) | (get_reg(data, 83 - 60) as u32);
    let voltage = voltage_raw as f32 * 0.001;

    // SOC: IR(100)
    let soc = (get_reg(data, 100 - 60) as u8).min(100);

    // Temperature: use t_max from IR(103)
    let temperature = get_reg(data, 103 - 60) as f32 * 0.1;

    // Serial number: IR(110-114)
    let serial = decode_serial(data, 110 - 60, 5);

    // Additional info
    let num_cycles = get_reg(data, 96 - 60);
    let num_cells = get_reg(data, 97 - 60);
    let bms_firmware = get_reg(data, 98 - 60);

    // Capacity registers: IR(84-85) cap_calibrated, IR(86-87) cap_design, IR(88-89) cap_remaining
    // All are uint32 in 0.01 Ah units.
    let cap_calibrated = ((get_reg(data, 84 - 60) as u32) << 16) | (get_reg(data, 85 - 60) as u32);
    let cap_design = ((get_reg(data, 86 - 60) as u32) << 16) | (get_reg(data, 87 - 60) as u32);
    let cap_remaining = ((get_reg(data, 88 - 60) as u32) << 16) | (get_reg(data, 89 - 60) as u32);

    // Raw status/warning bytes: IR(90-94) are undocumented by the public
    // register maps, so expose them verbatim for field investigation.
    let ir90 = get_reg(data, 90 - 60);
    let ir91 = get_reg(data, 91 - 60);
    let ir92 = get_reg(data, 92 - 60);
    let ir93 = get_reg(data, 93 - 60);
    let ir94 = get_reg(data, 94 - 60);
    let bms_status_registers = vec![ir90, ir91, ir92, ir93, ir94];
    let bms_status = vec![
        (ir90 >> 8) as u8,
        (ir90 & 0x00FF) as u8,
        (ir91 >> 8) as u8,
        (ir91 & 0x00FF) as u8,
        (ir92 >> 8) as u8,
        (ir92 & 0x00FF) as u8,
        (ir93 >> 8) as u8,
    ];
    let bms_warnings = vec![(ir94 >> 8) as u8, (ir94 & 0x00FF) as u8];

    BatteryModule {
        index,
        soc,
        temperature,
        voltage,
        current: 0.0, // LV BMS doesn't expose current; use inverter-level battery_current
        serial,
        num_cycles,
        num_cells,
        cell_voltages,
        cell_temperatures,
        bms_firmware,
        capacity_ah: cap_calibrated as f32 * 0.01,
        design_capacity_ah: cap_design as f32 * 0.01,
        remaining_capacity_ah: cap_remaining as f32 * 0.01,
        bms_status_registers,
        bms_status,
        bms_warnings,
    }
}

/// Decode battery input 60-119 into battery modules and append to snapshot.
/// `block_index` is the battery number (0-based).
pub fn decode_battery_block_into(
    data: &[u16],
    block_index: usize,
    snapshot: &mut InverterSnapshot,
    _serial: &str,
) {
    let module = decode_battery_block(data, block_index);
    snapshot.battery_modules.push(module);
}

// ===========================================================================
// HV BCU (Battery Control Unit) cluster decode
// ===========================================================================
//
// HV stackable batteries (GIV-BAT-*-HV) expose cluster-level data on a BCU at
// device address 0x70+i. The register layout (IR 60-119) is distinct from the
// LV pack protocol at 0x32. See givenergy-modbus model/hv_bcu.py and GivTCP
// model/hvbcu.py for the authoritative reference.

/// Parsed BCU cluster-level data from one HV battery stack (IR 60-119 at 0x70+i).
///
/// Carries the fields needed to populate the snapshot for HV systems where the
/// inverter's own register blocks carry no battery temperature or capacity.
/// Per-module cell detail comes separately from the BMU reads (commit 2).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HvBcuCluster {
    /// Pack software version string (validity fingerprint), e.g. "GA000005".
    pub pack_software_version: String,
    /// Number of BMU modules in this stack (IR 64).
    pub number_of_modules: u16,
    /// Cells per module (IR 65).
    pub cells_per_module: u16,
    /// Average cell voltage across the stack in V (IR 67, milli-V).
    pub cluster_cell_voltage: f32,
    /// BCU status flags (IR 70). 0x01 = normal operation.
    pub status: u16,
    /// Pack terminal voltage in V (IR 73, /10).
    pub battery_voltage: f32,
    /// Pack current in A (IR 76, int16 /10).
    pub battery_current: f32,
    /// Pack power in W (IR 79, /1000 → kW × 1000).
    pub battery_power_w: i32,
    /// Highest SOC reported by any module (IR 80 hi byte).
    pub battery_soc_max: u8,
    /// Lowest SOC reported by any module (IR 80 lo byte).
    pub battery_soc_min: u8,
    /// State of health % (IR 81).
    pub battery_soh: u8,
    /// Hottest cell temperature across the cluster in °C (IR 68, ×0.1).
    pub temperature: f32,
    /// Nominal capacity per module in Ah (IR 98, /10).
    pub nominal_capacity_ah: f32,
    /// Remaining capacity per module in Ah (IR 99, /10).
    pub remaining_capacity_ah: f32,
}

impl HvBcuCluster {
    /// Total nominal pack capacity in Ah (per-module Ah × module count).
    pub fn total_capacity_ah(&self) -> f32 {
        self.nominal_capacity_ah * self.number_of_modules as f32
    }

    /// Total remaining pack capacity in Ah (per-module Ah × module count).
    pub fn total_remaining_ah(&self) -> f32 {
        self.remaining_capacity_ah * self.number_of_modules as f32
    }
}

/// Decode the BCU cluster input register block (IR 60-119) into an [`HvBcuCluster`].
///
/// `data` is the 60-register slice read from device 0x70+i. Register indices below
/// are absolute IR addresses (the block starts at IR 60, so subtract 60 to index).
pub fn decode_hv_bcu_cluster(data: &[u16]) -> HvBcuCluster {
    // Pack software version: IR(60-63), gateway_version encoding (2 Latin-1
    // chars per register for the prefix, then 4 ASCII digits). The block starts
    // at IR(60), so these are at slice offsets 0-3.
    let pack_software_version = decode_gateway_version(data, 0);

    let number_of_modules = get_reg(data, 64 - 60);
    let cells_per_module = get_reg(data, 65 - 60);
    let cluster_cell_voltage = get_reg(data, 67 - 60) as f32 * 0.001; // IR(67): milli-V → V
    let status = get_reg(data, 70 - 60); // IR(70): BCU status flags

    let battery_voltage = get_reg(data, 73 - 60) as f32 * 0.1;
    let battery_current = signed(get_reg(data, 76 - 60)) as f32 * 0.1;
    // IR(79) is battery_power in milliwatts (unsigned u16 in reference, but
    // some firmware versions may use two's complement for discharge). Use
    // signed() like battery_current for consistency.
    let battery_power_w = signed(get_reg(data, 79 - 60));

    let soc_packed = get_reg(data, 80 - 60);
    let battery_soc_max = ((soc_packed >> 8) & 0xFF) as u8;
    let battery_soc_min = (soc_packed & 0xFF) as u8;
    let battery_soh = (get_reg(data, 81 - 60) & 0xFF) as u8;

    let temperature = get_reg(data, 68 - 60) as f32 * 0.1; // IR(68): cluster_cell_temperature (deci °C)

    let nominal_capacity_ah = get_reg(data, 98 - 60) as f32 * 0.1;
    let remaining_capacity_ah = get_reg(data, 99 - 60) as f32 * 0.1;

    HvBcuCluster {
        pack_software_version,
        number_of_modules,
        cells_per_module,
        cluster_cell_voltage,
        status,
        battery_voltage,
        battery_current,
        battery_power_w,
        battery_soc_max,
        battery_soc_min,
        battery_soh,
        temperature,
        nominal_capacity_ah,
        remaining_capacity_ah,
    }
}

/// Decode a gateway/pack version string from 4 consecutive registers.
///
/// Mirrors givenergy-modbus `Converter.gateway_version`: the first two
/// registers yield a Latin-1 prefix (2 chars each, NULs stripped), the last
/// two yield 4 ASCII digits. e.g. `[0x4741, 0x3030, 0x0000, 0x0005]` →
/// "GA000005".
fn decode_gateway_version(data: &[u16], start: usize) -> String {
    let regs: [u16; 4] = [
        get_reg(data, start),
        get_reg(data, start + 1),
        get_reg(data, start + 2),
        get_reg(data, start + 3),
    ];
    let prefix = b""
        .iter()
        .copied()
        .chain(regs[0].to_be_bytes())
        .chain(regs[1].to_be_bytes())
        .filter(|&b| b != 0)
        .map(|b| b as char)
        .collect::<String>();
    let digits = regs[2]
        .to_be_bytes()
        .iter()
        .chain(regs[3].to_be_bytes().iter())
        .map(|b| (*b).to_string())
        .collect::<String>();
    prefix + &digits
}

/// Whether a raw BCU cluster block represents a real, present HV stack.
///
/// Per givenergy-modbus `Bcu.is_valid()`: the pack software version (IR 60-63)
/// must be non-empty and not all-zeros/all-spaces. A non-existent BCU (no HV
/// battery attached) returns an empty or all-zero version.
pub fn validate_hv_bcu(data: &[u16]) -> bool {
    let version = decode_gateway_version(data, 0);
    let trimmed = version.trim();
    !trimmed.is_empty() && !trimmed.chars().all(|c| c == '0')
}

// ===========================================================================
// Gateway (DTC 0x70xx) aggregation bank decode — IR 1600-1859
// ===========================================================================
//
// The GivEnergy Gateway is a system controller / AC hub for one or more
// All-in-One (AIO) units. ALL of its live measurements live in a dedicated
// Input Register bank (IR 1600-1859), distinct from every other GivEnergy
// device. Register map, scalings and the V1/V2 variant contract are from
// `dewet22/givenergy-modbus` `model/gateway.py`; see
// `gateway-design/gateway-register-reference.md` for the authoritative
// byte-level reference.
//
// CRITICAL — sign conventions differ from the rest of the codebase:
//   * Battery / AIO power (`p_aio_total`, `p_aio*_inverter`) is signed with
//     the GivEnergy wire convention: **+ = discharging (out), − = charging
//     (in)**. HEM's internal convention is the OPPOSITE (+ = charge), so these
//     are negated when mapped onto `battery_power`.
//   * Grid power has no dedicated register — it is derived in `decode_snapshot`
//     from the energy balance (solar + battery_discharge − home).
//
// V1/V2 variant: selected by IR(1603) (the last register of the version
// string). `< 10` → V1 (uint32 totals high-register-first, AIO serials @
// 1831+); `>= 10` → V2 (totals low-register-first / swapped, serials @ 1841+).
// The simulator emits V1 only; the V2 path is shipped for real hardware on
// GA000010+ firmware.

/// Whether a decoded gateway software-version string indicates a real,
/// present gateway.
///
/// Mirrors `givenergy-modbus` `gateway.is_valid()`: the version (IR 1600-1603)
/// must decode to a non-trivial string. An all-zero bank (non-gateway device,
// or a failed read) decodes to `"00000000"` — treated as absent so gateway
// widgets are never rendered against a hybrid/AIO.
pub fn gateway_is_valid(version: &str) -> bool {
    let trimmed = version.trim();
    !trimmed.is_empty() && !trimmed.chars().all(|c| c == '0')
}

/// Decode a variant-aware uint32 energy total from two registers.
///
/// `hi_off` / `lo_off` are the V1 (high, low) register offsets within the
/// block. V2 swaps the interpretation (low register is the high word).
fn gw_u32(data: &[u16], hi_off: usize, lo_off: usize, is_v2: bool) -> u32 {
    let hi = get_reg(data, hi_off);
    let lo = get_reg(data, lo_off);
    if is_v2 {
        uint32(lo, hi) // V2: low register first (byte order swapped)
    } else {
        uint32(hi, lo) // V1: high register first
    }
}

/// Gateway fault-bitmask → human-readable names.
///
/// 32-bit MSB-first bitmask at IR(1622-1623): table bit 0 = bit 31 of the
/// u32 (MSB), table bit 31 = bit 0 (LSB). Source: `model/gateway.py`
/// `_gateway_fault_code`. Empty entries are reserved/unused.
const GATEWAY_FAULT_NAMES: [&str; 32] = [
    "Relay 1&2 bonding",        // 0
    "Relay 3&4 bonding",        // 1
    "Relay 1&2 disconnect",     // 2
    "Relay 3&4 disconnect",     // 3
    "AC over frequency 1",      // 4
    "AC under frequency 1",     // 5
    "AC over voltage 1",        // 6
    "AC under voltage 1",       // 7
    "AC over frequency 2",      // 8
    "AC under frequency 2",     // 9
    "AC over voltage 2",        // 10
    "AC under voltage 2",       // 11
    "",                          // 12 (reserved)
    "No zero-point protection", // 13
    "Over quarter AC voltage",  // 14
    "Under quarter AC voltage", // 15
    "Over AC voltage long-time",// 16
    "AC over frequency constant",   // 17
    "AC under frequency constant",  // 18
    "AC over voltage constant",     // 19
    "", "", "", "", "", "", "", "", "", "", "", // 20-30 (reserved)
    "Grid mode Off",            // 31
];

fn decode_gateway_faults(raw: u32) -> Vec<String> {
    let mut faults = Vec::new();
    for (i, name) in GATEWAY_FAULT_NAMES.iter().enumerate() {
        if name.is_empty() {
            continue;
        }
        // Bit 0 = MSB (bit 31 of u32), bit 31 = LSB.
        if (raw >> (31 - i)) & 1 == 1 {
            faults.push((*name).to_string());
        }
    }
    faults
}

/// IR 1600-1659: identity, version/variant, instantaneous power, faults,
/// daily + lifetime energy totals.
fn decode_gateway_1600_1659(data: &[u16], snap: &mut InverterSnapshot) {
    // --- Identity & variant ---
    snap.gateway_software_version = decode_gateway_version(data, 0); // IR 1600-1603
    let is_v2 = get_reg(data, 3) >= 10; // IR(1603): last version reg selects V1/V2
    snap.gateway_is_v2 = is_v2;
    snap.gateway_work_mode = get_reg(data, 4); // IR 1604
    snap.first_inverter_serial = decode_serial(data, 27, 5); // IR 1627-1631

    // --- Faults ---
    let fault_raw = (get_reg(data, 22) as u32) << 16 | get_reg(data, 23) as u32; // IR 1622-1623
    snap.gateway_fault_codes = decode_gateway_faults(fault_raw);

    // --- Instantaneous power (mapped to standard snapshot fields) ---
    // IR(1608) v_grid ÷10 V. Grid frequency is not available in the gateway
    // bank — leave it NaN so the sanitizer's [45,55] range check is skipped
    // (NaN comparisons are always false in Rust).
    snap.grid_voltage = get_reg(data, 8) as f32 * 0.1;
    snap.grid_frequency = f32::NAN;
    snap.grid_online = snap.grid_voltage > 50.0;

    // Battery temperature is not available in the Gateway bank (the
    // Gateway aggregates SOC/power/energy but does not expose per-pack
    // temperature — that lives on each AIO's own BMS). Set to NaN so
    // the frontend displays "—" rather than "0.0°C".
    snap.battery_temperature = f32::NAN;

    // Inverter temperature is also not available — the Gateway measures
    // AC system data but has no inverter heatsink sensor to report.
    snap.inverter_temperature = f32::NAN;

    // Battery voltage and current are not available on the Gateway —
    // these live on each AIO's own BMS. Set to NaN so the frontend
    // shows "—" instead of "0.0V" / "0.0A" on the Status, Battery,
    // and Inverter pages.
    snap.battery_voltage = f32::NAN;
    snap.battery_current = f32::NAN;

    // PV voltage is not available on the Gateway (no per-string meter).
    // Set to NaN so the frontend shows "—" instead of "0.0V".
    snap.pv1_voltage = f32::NAN;
    snap.pv2_voltage = f32::NAN;
    // PV current `i_pv` (IR 1612) IS available — set it so the solar
    // node shows a meaningful current rather than "0.0A".
    snap.pv1_current = signed(get_reg(data, 12)) as f32 * 0.1;

    // IR(1617) p_pv — unsigned total PV generation.
    let p_pv = get_reg(data, 17) as i32;
    snap.solar_power = p_pv;
    snap.pv1_power = p_pv;

    // IR(1618) p_load — house load, EXCLUDES the EV charger (gateway defining
    // property). This is the authoritative home_power; decode_snapshot keeps
    // it and derives grid_power from the energy balance instead.
    snap.home_power = get_reg(data, 18) as i32;

    // --- Daily energy totals (÷10 kWh) ---
    snap.today_import_kwh = get_reg(data, 40) as f32 * 0.1; // e_grid_import_today
    snap.today_solar_kwh = get_reg(data, 43) as f32 * 0.1; // e_pv_today
    snap.today_export_kwh = get_reg(data, 46) as f32 * 0.1; // e_grid_export_today
    snap.today_charge_kwh = get_reg(data, 49) as f32 * 0.1; // e_aio_charge_today
    snap.today_discharge_kwh = get_reg(data, 52) as f32 * 0.1; // e_aio_discharge_today
    snap.today_consumption_kwh = get_reg(data, 55) as f32 * 0.1; // e_load_today

    // --- Lifetime energy totals (uint32 ÷10 kWh, V1/V2 byte order) ---
    // Decode with V1/V2 awareness, then apply plausibility check on the result.
    let total_import_u32 = gw_u32(data, 41, 42, is_v2);
    let total_export_u32 = gw_u32(data, 47, 48, is_v2);
    let total_charge_u32 = gw_u32(data, 50, 51, is_v2);
    let total_discharge_u32 = gw_u32(data, 53, 54, is_v2);
    // Plausibility: the hi word of a genuine residential lifetime total never
    // exceeds ~30 (200 MWh). hi > 1000 (>6.5 GWh) is impossible corruption.
    snap.total_import_kwh = if (total_import_u32 >> 16) > 1000 { 0.0 } else { total_import_u32 as f32 * 0.1 };
    snap.total_export_kwh = if (total_export_u32 >> 16) > 1000 { 0.0 } else { total_export_u32 as f32 * 0.1 };
    snap.total_charge_kwh = if (total_charge_u32 >> 16) > 1000 { 0.0 } else { total_charge_u32 as f32 * 0.1 };
    snap.total_discharge_kwh = if (total_discharge_u32 >> 16) > 1000 { 0.0 } else { total_discharge_u32 as f32 * 0.1 };
}

/// IR 1660-1719: AIO stack summary (count, aggregate power, state) + per-AIO
/// charge energy.
fn decode_gateway_1660_1719(data: &[u16], snap: &mut InverterSnapshot) {
    snap.parallel_aio_count = get_reg(data, 40) as u8; // IR 1700
    snap.parallel_aio_online = get_reg(data, 41) as u8; // IR 1701

    // IR(1702) p_aio_total — aggregate AIO inverter power, signed (GE: +=discharge).
    // Map to battery_power with HEM convention (+ = charge): NEGATE.
    let p_aio_total = signed(get_reg(data, 42));
    snap.battery_power = -p_aio_total;
    snap.battery_state = BatteryState::from_power(snap.battery_power);

    // Capacity / max power scale with the configured AIO count (per GivTCP
    // getInvModel: 13.5 kWh × n, 6000 W × n). Guard against a zero/missing
    // count so the UI never shows a zero-power battery.
    let n = snap.parallel_aio_count.max(1) as u32;
    snap.battery_capacity_kwh = 13.5 * n as f32;
    snap.max_battery_power_w = 6000 * n;

    // Per-AIO charge today (÷10 kWh).
    snap.per_aio_charge_today_kwh = [
        get_reg(data, 45) as f32 * 0.1, // IR 1705
        get_reg(data, 48) as f32 * 0.1, // IR 1708
        get_reg(data, 51) as f32 * 0.1, // IR 1711
    ];
}

/// IR 1720-1779: per-AIO discharge energy.
fn decode_gateway_1720_1779(data: &[u16], snap: &mut InverterSnapshot) {
    snap.per_aio_discharge_today_kwh = [
        get_reg(data, 30) as f32 * 0.1, // IR 1750
        get_reg(data, 33) as f32 * 0.1, // IR 1753
        get_reg(data, 36) as f32 * 0.1, // IR 1756
    ];
}

/// IR 1780-1830: aggregate battery energy, per-AIO SOC, per-AIO inverter power.
fn decode_gateway_1780_1830(data: &[u16], snap: &mut InverterSnapshot) {
    // Per-AIO SOC % — IR 1801-1803.
    snap.per_aio_soc = [
        get_reg(data, 21), // IR 1801
        get_reg(data, 22), // IR 1802
        get_reg(data, 23), // IR 1803
    ];

    // Aggregate SOC: average of online AIOs with a non-zero SOC. Falls back to
    // AIO1 for a single-AIO install.
    let count = snap.parallel_aio_online.max(1) as usize;
    let soc_vals: Vec<u16> = snap
        .per_aio_soc
        .iter()
        .take(count)
        .copied()
        .filter(|&s| s > 0)
        .collect();
    let soc_sum: u32 = soc_vals.iter().map(|&s| s as u32).sum();
    if !soc_vals.is_empty() {
        snap.soc = (soc_sum / soc_vals.len() as u32).min(100) as u8;
    } else {
        snap.soc = snap.per_aio_soc[0].min(100) as u8;
    }

    // Per-AIO inverter power — IR 1816-1818, signed (GE: + = discharge).
    // Stored verbatim in per_aio_power (the field documents the GE sign).
    snap.per_aio_power = [
        signed(get_reg(data, 36)), // IR 1816
        signed(get_reg(data, 37)), // IR 1817
        signed(get_reg(data, 38)), // IR 1818
    ];
}

/// IR 1831-1859: per-AIO serial numbers (addresses differ by firmware variant).
fn decode_gateway_1831_1859(data: &[u16], snap: &mut InverterSnapshot) {
    if snap.gateway_is_v2 {
        // V2 (GA000010+): aio1 @ 1841-1845, aio2 @ 1848-1852, aio3 @ 1855-1859.
        snap.per_aio_serial = [
            decode_serial(data, 10, 5),
            decode_serial(data, 17, 5),
            decode_serial(data, 24, 5),
        ];
    } else {
        // V1 (GA000009 and earlier): aio1 @ 1831-1835, aio2 @ 1838-1842, aio3 @ 1845-1849.
        snap.per_aio_serial = [
            decode_serial(data, 0, 5),
            decode_serial(data, 7, 5),
            decode_serial(data, 14, 5),
        ];
    }
}

// ===========================================================================
// HV BMU (Battery Module Unit) per-cell decode
// ===========================================================================
//
// Each BMU (device 0x50+m) exposes one module's cell-level data. Per
// givenergy-modbus model/hv_bcu.py (Bmu) and GivTCP model/hvbmu.py, the
// 60-register read (base 60+120*bcu_offset) always lands v_cell_01 at slice
// offset 0 — the base-register shift aligns it — so decoding is uniform.

/// Number of cells per HV BMU module (all known HV stacks use 24 cells/module).
pub const HV_CELLS_PER_MODULE: usize = 24;

/// Decode a single HV BMU module block into a [`BatteryModule`].
///
/// `data` is the 60-register slice read from device 0x50+m. Layout (slice
/// offsets, since the read base already aligns v_cell_01 to offset 0):
///   0..24:   v_cell_01..24 (milli-V → V)
///   30..54:  t_cell_01..24 (0.1 °C)
///   54..59:  serial_number (5 regs, Latin-1)
///
/// Pack-level voltage/SOC/capacity are NOT exposed per-module on HV stacks —
/// they come from the BCU cluster. The module's terminal voltage (sum of
/// cells) and hottest cell temperature are derived here for the Battery page
/// summary row.
pub fn decode_hv_bmu_block(data: &[u16], index: usize) -> crate::inverter::model::BatteryModule {
    // Cell voltages: 24 cells, milli-V → V.
    let cell_voltages: Vec<f32> = (0..HV_CELLS_PER_MODULE)
        .map(|i| get_reg(data, i) as f32 * 0.001)
        .collect();
    // Cell temperatures: 24 probes, 0.1 °C.
    let cell_temperatures: Vec<f32> = (0..HV_CELLS_PER_MODULE)
        .map(|i| get_reg(data, 30 + i) as f32 * 0.1)
        .collect();
    // Serial number: 5 registers, Latin-1 (slice offsets 54..59).
    let serial = decode_serial(data, 54, 5);

    // Module terminal voltage = sum of cell voltages.
    let voltage = cell_voltages.iter().copied().sum::<f32>();
    // Hottest cell temperature in this module.
    let temperature = cell_temperatures
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);

    crate::inverter::model::BatteryModule {
        index,
        soc: 0, // Per-module SOC is not exposed on HV stacks (stack SOC is BCU-level).
        temperature,
        voltage,
        current: 0.0, // Pack-level; comes from the BCU cluster.
        serial,
        num_cycles: 0,
        num_cells: HV_CELLS_PER_MODULE as u16,
        cell_voltages,
        cell_temperatures,
        bms_firmware: 0,
        capacity_ah: 0.0, // Pack-level; comes from the BCU cluster.
        design_capacity_ah: 0.0,
        remaining_capacity_ah: 0.0,
        bms_status_registers: Vec::new(),
        bms_status: Vec::new(),
        bms_warnings: Vec::new(),
    }
}

/// Whether a raw BMU module block represents a real, present module.
///
/// Per givenergy-modbus `Bmu.is_valid()`: the serial number (slice 54..59)
/// must be non-empty. A non-existent module returns an empty/garbage serial.
pub fn validate_hv_bmu(data: &[u16]) -> bool {
    let serial = decode_serial(data, 54, 5);
    let trimmed = serial.trim();
    trimmed.len() >= 4 && trimmed.chars().all(|c| c.is_ascii_graphic() || c == ' ')
}

/// Backfill per-module SOC and pack-level capacity for HV modules.
///
/// HV BMU registers expose only cell voltages, cell temperatures and serial —
/// there is **no per-module SOC register** (confirmed against GivTCP's
/// `hvbmu.py`). The BCU cluster reports the stack-wide SOC spread
/// (`battery_soc_max` / `battery_soc_min`) and the per-module Ah capacity.
/// Modules in a stack track within a few % of each other, so the cluster SOC
/// average is a defensible per-module fill, and the per-module Ah lets the
/// Battery page show per-module capacity/health for HV (which `decode_hv_bmu_block`
/// cannot derive from the BMU bank alone).
pub fn backfill_hv_module_fields(
    modules: &mut [crate::inverter::model::BatteryModule],
    cluster: &HvBcuCluster,
) {
    let module_soc = cluster
        .battery_soc_max
        .saturating_add(cluster.battery_soc_min)
        / 2;
    let module_soc = module_soc.min(100);
    // Per-module nominal Ah. The cluster reports per-module capacity (IR 98),
    // so each module takes the same value; design == calibrated for HV.
    let per_module_ah = cluster.nominal_capacity_ah;
    for m in modules.iter_mut() {
        m.soc = module_soc;
        if per_module_ah > 0.0 {
            m.capacity_ah = per_module_ah;
            m.design_capacity_ah = per_module_ah;
            m.remaining_capacity_ah = per_module_ah * module_soc as f32 / 100.0;
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modbus::registers::RegisterBlock;

    fn make_block(
        register_type: RegisterType,
        start: u16,
        count: u16,
        name: &'static str,
        data: Vec<u16>,
    ) -> BlockRead {
        let block = Box::leak(Box::new(RegisterBlock {
            start,
            count,
            register_type,
            name,
        }));
        BlockRead { block, data }
    }

    #[test]
    fn decode_battery_block_exposes_raw_bms_status_and_warning_bytes() {
        let mut data = vec![0u16; 60];
        data[90 - 60] = 0x0102;
        data[91 - 60] = 0x0E10;
        data[92 - 60] = 0x2040;
        data[93 - 60] = 0x8000;
        data[94 - 60] = 0xA55A;

        let module = decode_battery_block(&data, 0);

        assert_eq!(
            module.bms_status_registers,
            vec![0x0102, 0x0E10, 0x2040, 0x8000, 0xA55A]
        );
        assert_eq!(module.bms_status, vec![1, 2, 14, 16, 32, 64, 128]);
        assert_eq!(module.bms_warnings, vec![165, 90]);
    }

    fn test_blocks() -> Vec<BlockRead> {
        // Input registers 0-59
        let mut input_data = vec![0u16; 60];
        input_data[1] = 320; // IR(1):  pv1_voltage = 32.0 V
        input_data[2] = 315; // IR(2):  pv2_voltage = 31.5 V
        input_data[5] = 2410; // IR(5):  grid_voltage = 241.0 V
        input_data[8] = 78; // IR(8):  pv1_current = 7.8 A
        input_data[9] = 48; // IR(9):  pv2_current = 4.8 A
        input_data[13] = 5002; // IR(13): grid_frequency = 50.02 Hz
        input_data[17] = 185; // IR(17): pv1_energy_today = 18.5 kWh
        input_data[18] = 2500; // IR(18): pv1_power = 2500 W
        input_data[19] = 95; // IR(19): pv2_energy_today = 9.5 kWh
        input_data[20] = 1500; // IR(20): pv2_power = 1500 W
        input_data[21] = 0;
        input_data[22] = 12345; // IR(21-22): e_grid_out_total = 1234.5 kWh
        input_data[25] = 30; // IR(25): export_today = 3.0 kWh
        input_data[26] = 52; // IR(26): import_today = 5.2 kWh
        input_data[30] = 100; // IR(30): grid_power = +100 W (export)
        input_data[32] = 0;
        input_data[33] = 6789; // IR(32-33): e_grid_in_total = 678.9 kWh
        input_data[35] = 120; // IR(35): ac_charge_today = 12.0 kWh
        input_data[36] = 40; // IR(36): charge_today = 4.0 kWh
        input_data[37] = 25; // IR(37): discharge_today = 2.5 kWh
        input_data[41] = 425; // IR(41): inverter_temp = 42.5 °C
        input_data[50] = 5200; // IR(50): battery_voltage = 52.00 V
        input_data[51] = (-150i16) as u16; // IR(51): battery_current = -1.50 A (inverter: negative = charging)
        input_data[52] = (-800i16) as u16; // IR(52): battery_power = -800 W (inverter: negative = charging)
        input_data[56] = 310; // IR(56): battery_temp = 31.0 °C
        input_data[59] = 75; // IR(59): battery_soc = 75%

        let input_block = make_block(RegisterType::Input, 0, 60, "input_0_59", input_data);

        // Holding registers 0-59
        let mut holding_data = vec![0u16; 60];
        holding_data[0] = 0x2001; // HR(0):  device_type = Gen3Hybrid
        holding_data[6] = 5120; // HR(6):  system_voltage = 51.20 V (/100)
                                // Serial number at HR(13-17): "SA12345678"
        holding_data[13] = 0x5341; // 'S'(0x53), 'A'(0x41)
        holding_data[14] = 0x3132; // '1'(0x31), '2'(0x32)
        holding_data[15] = 0x3334; // '3'(0x33), '4'(0x34)
        holding_data[16] = 0x3536; // '5'(0x35), '6'(0x36)
        holding_data[17] = 0x3738; // '7'(0x37), '8'(0x38)
        holding_data[19] = 999; // HR(19): DSP firmware = 999
        holding_data[21] = 1234; // HR(21): ARM firmware version
        holding_data[27] = 1; // HR(27): eco mode
        holding_data[50] = 80; // HR(50): active_power_rate = 80%
        holding_data[31] = 600; // HR(31): charge_slot_2 start = 06:00
        holding_data[32] = 1000; // HR(32): charge_slot_2 end = 10:00
        holding_data[44] = 1700; // HR(44): discharge_slot_2 start = 17:00
        holding_data[45] = 2000; // HR(45): discharge_slot_2 end = 20:00
        holding_data[55] = 100; // HR(55): battery_capacity_ah = 100 Ah
        holding_data[56] = 1600; // HR(56): discharge_slot_1 start = 16:00
        holding_data[57] = 1900; // HR(57): discharge_slot_1 end = 19:00
        holding_data[59] = 0; // HR(59): enable_discharge = false

        let holding_block = make_block(RegisterType::Holding, 0, 60, "holding_0_59", holding_data);

        // Holding registers 60-119
        let mut holding_60_data = vec![0u16; 60];
        holding_60_data[94 - 60] = 100; // HR(94):  charge_slot_1 start = 01:00
        holding_60_data[95 - 60] = 500; // HR(95):  charge_slot_1 end = 05:00
        holding_60_data[96 - 60] = 1; // HR(96):  enable_charge = true
        holding_60_data[110 - 60] = 4; // HR(110): battery_soc_reserve = 4%
        holding_60_data[111 - 60] = 50; // HR(111): battery_charge_limit = 50%
        holding_60_data[112 - 60] = 50; // HR(112): battery_discharge_limit = 50%
        holding_60_data[116 - 60] = 100; // HR(116): charge_target_soc = 100%

        let holding_60 = make_block(
            RegisterType::Holding,
            60,
            60,
            "holding_60_119",
            holding_60_data,
        );

        vec![input_block, holding_block, holding_60]
    }

    #[test]
    fn decode_full_snapshot() {
        let blocks = test_blocks();
        let snap = decode_snapshot(&blocks);

        // PV
        assert_eq!(snap.pv1_power, 2500);
        assert_eq!(snap.pv2_power, 1500);
        assert_eq!(snap.solar_power, 4000);
        assert!((snap.pv1_voltage - 32.0).abs() < 0.1);
        assert!((snap.pv2_voltage - 31.5).abs() < 0.1);
        assert!((snap.pv1_current - 7.8).abs() < 0.1);
        assert!((snap.pv2_current - 4.8).abs() < 0.1);

        // Battery
        assert_eq!(snap.battery_power, 800);
        assert_eq!(snap.soc, 75);
        assert!((snap.battery_voltage - 52.0).abs() < 0.1);
        assert!((snap.battery_current - 1.5).abs() < 0.1);
        assert!((snap.battery_temperature - 31.0).abs() < 0.1);
        assert_eq!(snap.battery_state, BatteryState::Charging);
        // Capacity: 100 Ah × 51.20 V / 1000 = 5.12 kWh
        assert!((snap.battery_capacity_kwh - 5.12).abs() < 0.01);

        // Grid (raw: positive = exporting, per GivTCP convention)
        assert_eq!(snap.grid_power, 100);
        assert!((snap.grid_voltage - 241.0).abs() < 0.1);
        assert!((snap.grid_frequency - 50.02).abs() < 0.01);
        assert!(snap.grid_online);
        assert!(!snap.grid_loss);
        assert!(!snap.inverter_trip);

        // Inverter
        assert!((snap.inverter_temperature - 42.5).abs() < 0.1);

        // Energy
        assert!((snap.today_solar_kwh - 28.0).abs() < 0.1);
        assert!((snap.today_import_kwh - 5.2).abs() < 0.1);
        assert!((snap.today_export_kwh - 3.0).abs() < 0.1);
        assert!((snap.today_charge_kwh - 4.0).abs() < 0.1);
        assert!((snap.today_discharge_kwh - 2.5).abs() < 0.1);
        // consumption = solar(28.0) + import(5.2) - export(3.0) - ac_charge(12.0) = 18.2
        assert!((snap.today_consumption_kwh - 18.2).abs() < 0.1);
        assert!((snap.today_ac_charge_kwh - 12.0).abs() < 0.1);
        assert!((snap.total_import_kwh - 678.9).abs() < 0.1);
        assert!((snap.total_export_kwh - 1234.5).abs() < 0.1);

        // Config
        assert_eq!(snap.battery_reserve, 4);
        assert_eq!(snap.charge_rate, 50);
        assert_eq!(snap.discharge_rate, 50);
        assert_eq!(snap.active_power_rate, 80);
        assert_eq!(snap.max_battery_power_w, 2560); // min(3600, 5120/2)
        assert_eq!(snap.target_soc, 100);
        assert!(snap.enable_charge);
        assert!(!snap.enable_discharge);

        // Mode: eco=1, discharge=false, reserve=4 (!=100) → Eco
        // Note: discharge slots have valid times and show enabled (their
        // configuration is independent of the master enable_discharge flag,
        // which only controls the battery mode).
        assert_eq!(snap.battery_mode, BatteryMode::Eco);

        // Home power: solar - battery_power - grid_power
        //           = 4000 - 800 - 100 = 3100
        assert_eq!(snap.home_power, 3100);

        // Serial number
        assert_eq!(snap.inverter_serial, "SA12345678");

        // Firmware version
        assert_eq!(snap.firmware_version, "1234");
        assert_eq!(snap.dsp_firmware_version, "999");

        // Charge slot 1: 01:00–05:00, target_soc=100 (from global HR(116))
        assert!(snap.charge_slots[0].enabled);
        assert_eq!(snap.charge_slots[0].start_hour, 1);
        assert_eq!(snap.charge_slots[0].start_minute, 0);
        assert_eq!(snap.charge_slots[0].end_hour, 5);
        assert_eq!(snap.charge_slots[0].end_minute, 0);
        assert_eq!(snap.charge_slots[0].target_soc, 100);

        // Charge slot 2: 06:00–10:00
        assert!(snap.charge_slots[1].enabled);
        assert_eq!(snap.charge_slots[1].start_hour, 6);
        assert_eq!(snap.charge_slots[1].start_minute, 0);
        assert_eq!(snap.charge_slots[1].end_hour, 10);
        assert_eq!(snap.charge_slots[1].end_minute, 0);

        // Charge slot 3: not configured → disabled
        assert!(!snap.charge_slots[2].enabled);

        // Discharge slots show enabled when they have configured times,
        // independent of the master enable_discharge flag. That flag controls
        // the battery mode (Timed Demand), not whether a slot is configured —
        // so users can set up discharge slots in Eco mode without the slot
        // toggle snapping back to disabled on the next poll.
        assert!(snap.discharge_slots[0].enabled);
        assert_eq!(snap.discharge_slots[0].start_hour, 16);
        assert_eq!(snap.discharge_slots[0].end_hour, 19);
        assert!(snap.discharge_slots[1].enabled);
        assert_eq!(snap.discharge_slots[1].start_hour, 17);
        assert_eq!(snap.discharge_slots[1].end_hour, 20);
        assert!(!snap.enable_discharge, "enable_discharge flag is false");
    }

    #[test]
    fn decode_grid_loss_bit_marks_grid_offline() {
        // A genuine outage: voltage/frequency near zero AND the fault bit set.
        let mut blocks = test_blocks();
        blocks[0].data[5] = 0; // IR(5):  grid_voltage = 0.0 V
        blocks[0].data[13] = 0; // IR(13): grid_frequency = 0.00 Hz
        blocks[0].data[40] = GRID_LOSS_MASK_IR40; // IR(40) bit 7: "No Utility" fault

        let snap = decode_snapshot(&blocks);

        assert!(snap.grid_loss);
        assert!(!snap.inverter_trip);
        assert!(!snap.grid_online);
    }

    #[test]
    fn decode_offgrid_system_mode_marks_grid_offline() {
        // A genuine outage: voltage/frequency near zero AND system_mode=OffGrid.
        let mut blocks = test_blocks();
        blocks[0].data[5] = 0; // IR(5):  grid_voltage = 0.0 V
        blocks[0].data[13] = 0; // IR(13): grid_frequency = 0.00 Hz
        blocks[0].data[49] = SYSTEM_MODE_OFF_GRID; // IR(49): OffGrid

        let snap = decode_snapshot(&blocks);

        assert!(snap.grid_loss);
        assert!(!snap.grid_online);
    }

    #[test]
    fn decode_on_grid_system_mode_no_grid_loss() {
        let mut blocks = test_blocks();
        blocks[0].data[49] = 2; // IR(49): OnGrid

        let snap = decode_snapshot(&blocks);

        assert!(!snap.grid_loss);
        assert!(snap.grid_online);
    }

    #[test]
    fn gen3_false_positive_system_mode_offgrid_with_normal_voltage() {
        // Gen3 Hybrid (and similar firmware) can transiently report
        // system_mode=OffGrid while the grid is actually present. If voltage
        // and frequency are normal, grid_loss must stay false.
        let mut blocks = test_blocks();
        blocks[0].data[49] = SYSTEM_MODE_OFF_GRID; // IR(49): OffGrid (transient)
        // Voltage/frequency already at default 241.0V / 50.02 Hz

        let snap = decode_snapshot(&blocks);

        assert!(snap.grid_online, "voltage is normal — grid must be reported as online");
        assert!(!snap.grid_loss, "must NOT false-positive on transient OffGrid");
    }

    #[test]
    fn gen3_false_positive_no_utility_bit_with_normal_voltage() {
        // Same scenario via the IR(40) "No Utility" fault bit: the bit is set
        // transiently but voltage/frequency remain normal.
        let mut blocks = test_blocks();
        blocks[0].data[40] = GRID_LOSS_MASK_IR40; // IR(40) bit 7: "No Utility"
        // Voltage/frequency already at default 241.0V / 50.02 Hz

        let snap = decode_snapshot(&blocks);

        assert!(snap.grid_online, "voltage is normal — grid must be reported as online");
        assert!(!snap.grid_loss, "must NOT false-positive on transient No Utility bit");
    }

    #[test]
    fn decode_inverter_fault_status_marks_trip() {
        let mut blocks = test_blocks();
        blocks[0].data[0] = STATUS_FAULT; // IR(0): status = Fault

        let snap = decode_snapshot(&blocks);

        assert!(snap.inverter_trip);
        assert!(!snap.grid_loss);
        assert!(snap.grid_online); // a fault status does not imply grid loss
    }

    #[test]
    fn decode_charger_warning_marks_battery_over_temp() {
        let mut blocks = test_blocks();
        blocks[0].data[57] = 1; // IR(57): charger warning code = battery over temp

        let snap = decode_snapshot(&blocks);

        assert!(snap.battery_over_temp);
        assert!(!snap.grid_loss);
        assert!(!snap.inverter_trip);
    }

    #[test]
    fn three_phase_input_blocks_populate_dashboard_fields() {
        let mut holding_data = vec![0u16; 60];
        holding_data[0] = 0x4004; // Three Phase 11kW

        let mut ir1000 = vec![0u16; 60];
        ir1000[1001 - 1000] = 6500; // PV1 voltage 650.0V
        ir1000[1002 - 1000] = 6400; // PV2 voltage 640.0V
        ir1000[1009 - 1000] = 12; // PV1 current 1.2A
        ir1000[1010 - 1000] = 34; // PV2 current 3.4A
        ir1000[1017 - 1000] = 0;
        ir1000[1018 - 1000] = 25_000; // PV1 power 2500.0W
        ir1000[1019 - 1000] = 0;
        ir1000[1020 - 1000] = 15_000; // PV2 power 1500.0W

        let mut ir1060 = vec![0u16; 60];
        ir1060[1061 - 1060] = 4150; // grid voltage L1 415.0V (line-to-line)
        ir1060[1062 - 1060] = 4160; // grid voltage L2 416.0V (line-to-line)
        ir1060[1063 - 1060] = 4140; // grid voltage L3 414.0V (line-to-line)
        ir1060[1064 - 1060] = 10; // current L1 1.0A
        ir1060[1065 - 1060] = 12; // current L2 1.2A
        ir1060[1066 - 1060] = 8; // current L3 0.8A
        ir1060[1067 - 1060] = 5000; // grid frequency 50.00Hz
        ir1060[1068 - 1060] = 980; // power factor 0.980
        ir1060[1073 - 1060] = 0;
        ir1060[1074 - 1060] = 15_000; // grid apparent power 1500.0VA
        ir1060[1079 - 1060] = 0;
        ir1060[1080 - 1060] = 3000; // import 300W
        ir1060[1081 - 1060] = 0;
        ir1060[1082 - 1060] = 9000; // export 900W => grid_power +600W
        ir1060[1083 - 1060] = 700; // load L1 70.0W
        ir1060[1084 - 1060] = 800; // load L2 80.0W
        ir1060[1085 - 1060] = 600; // load L3 60.0W
        ir1060[1089 - 1060] = 0;
        ir1060[1090 - 1060] = 22_000; // load total 2200W

        let mut ir1120 = vec![0u16; 60];
        ir1120[1128 - 1120] = 355; // inverter temp 35.5C
        ir1120[1131 - 1120] = 520; // battery voltage 52.0V
        ir1120[1132 - 1120] = 67; // SOC 67%
        ir1120[1136 - 1120] = 0;
        ir1120[1137 - 1120] = 2000; // discharge 200W
        ir1120[1138 - 1120] = 0;
        ir1120[1139 - 1120] = 7000; // charge 700W => battery_power +500W
        ir1120[1140 - 1120] = 25; // battery current 2.5A

        let mut ir1360 = vec![0u16; 54];
        ir1360[1374 - 1360] = 3;
        ir1360[1375 - 1360] = 4641; // lifetime PV total 20124.9kWh; not a daily value
        ir1360[1380 - 1360] = 0;
        ir1360[1381 - 1360] = 45; // import today 4.5kWh
        ir1360[1382 - 1360] = 0;
        ir1360[1383 - 1360] = 888; // import total 88.8kWh
        ir1360[1384 - 1360] = 0;
        ir1360[1385 - 1360] = 67; // export today 6.7kWh
        ir1360[1386 - 1360] = 0;
        ir1360[1387 - 1360] = 999; // export total 99.9kWh
        ir1360[1388 - 1360] = 0;
        ir1360[1389 - 1360] = 89; // discharge today 8.9kWh
        ir1360[1392 - 1360] = 0;
        ir1360[1393 - 1360] = 101; // charge today 10.1kWh
        ir1360[1396 - 1360] = 0;
        ir1360[1397 - 1360] = 111; // load today 11.1kWh
        ir1360[1412 - 1360] = 0;
        ir1360[1413 - 1360] = 123; // PV generation today 12.3kWh

        let blocks = vec![
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", holding_data),
            make_block(RegisterType::Input, 1000, 60, "input_1000_1059", ir1000),
            make_block(RegisterType::Input, 1060, 60, "input_1060_1119", ir1060),
            make_block(RegisterType::Input, 1120, 60, "input_1120_1179", ir1120),
            make_block(RegisterType::Input, 1360, 54, "input_1360_1413", ir1360),
        ];

        let snap = decode_snapshot(&blocks);
        assert_eq!(snap.device_type, DeviceType::ThreePhase);
        assert_eq!(snap.max_ac_power_w, 11_000);
        assert_eq!(snap.solar_power, 4_000);
        assert_eq!(snap.grid_power, 600);
        assert_eq!(snap.battery_power, 500);
        assert_eq!(snap.home_power, 2_200);
        assert_eq!(snap.soc, 67);
        assert!((snap.grid_voltage - 415.0).abs() < 0.1);
        assert!((snap.grid_frequency - 50.0).abs() < 0.01);
        assert!((snap.today_solar_kwh - 12.3).abs() < 0.1);
        assert!((snap.today_import_kwh - 4.5).abs() < 0.1);
        assert!((snap.today_export_kwh - 6.7).abs() < 0.1);
        assert!((snap.today_charge_kwh - 10.1).abs() < 0.1);
        assert!((snap.today_discharge_kwh - 8.9).abs() < 0.1);
        assert!((snap.today_consumption_kwh - 11.1).abs() < 0.1);
        assert!((snap.total_import_kwh - 88.8).abs() < 0.1);
        assert!((snap.total_export_kwh - 99.9).abs() < 0.1);

        // Verify synthetic built-in CT meter was created
        assert_eq!(
            snap.meters.len(),
            1,
            "Should have 1 synthetic meter from 3-phase CT"
        );
        assert_eq!(snap.meters[0].address, 0x00);
        assert_eq!(
            snap.meters[0].p_active_total, -600,
            "Meter total = -grid_power (positive = import)"
        );
        // Per-phase grid power unavailable — no dedicated signed registers.
        assert_eq!(snap.meters[0].p_active_phase_1, 0, "L1 = unknown");
        assert_eq!(snap.meters[0].p_active_phase_2, 0, "L2 = unknown");
        assert_eq!(snap.meters[0].p_active_phase_3, 0, "L3 = unknown");
        // Power factor from IR(1068) — raw 980 → 0.980
        assert!(
            (snap.meters[0].pf_total - 0.980).abs() < 0.001,
            "PF from register"
        );
        // Apparent power from IR(1073-1074) — raw 15000 → 1500VA
        assert_eq!(
            snap.meters[0].p_apparent_total, 1500,
            "Apparent from register"
        );
        // Voltages from IR(1061-1063)
        assert!((snap.meters[0].v_phase_1 - 415.0).abs() < 0.1);
        assert!((snap.meters[0].v_phase_2 - 416.0).abs() < 0.1);
        assert!((snap.meters[0].v_phase_3 - 414.0).abs() < 0.1);
        assert!((snap.meters[0].frequency - 50.0).abs() < 0.01);
        assert!(
            (snap.meters[0].e_import_active_kwh - 88.8).abs() < 0.1,
            "3-phase meter import = total_import_kwh"
        );
        assert!(
            (snap.meters[0].e_export_active_kwh - 99.9).abs() < 0.1,
            "3-phase meter export = total_export_kwh"
        );
    }

    #[test]
    fn three_phase_uses_1000_range_limits() {
        let mut holding_data = vec![0u16; 60];
        holding_data[0] = 0x4001; // Three Phase

        let mut holding_60_data = vec![0u16; 60];
        holding_60_data[110 - 60] = 4;
        holding_60_data[111 - 60] = 11; // single-phase charge limit — should be overwritten
        holding_60_data[112 - 60] = 12; // single-phase discharge limit — should be overwritten
        holding_60_data[116 - 60] = 50;

        let mut three_phase = vec![0u16; 45];
        three_phase[1108 - 1080] = 88; // discharge limit
        three_phase[1109 - 1080] = 25; // reserve
        three_phase[1110 - 1080] = 77; // charge limit
        three_phase[1111 - 1080] = 95; // target SOC
        three_phase[1112 - 1080] = 1; // ac charge enable
        three_phase[1122 - 1080] = 1; // force discharge enable

        let blocks = vec![
            make_block(RegisterType::Input, 0, 60, "input_0_59", vec![0; 60]),
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", holding_data),
            make_block(
                RegisterType::Holding,
                60,
                60,
                "holding_60_119",
                holding_60_data,
            ),
            make_block(
                RegisterType::Holding,
                1080,
                45,
                "holding_1080_1124",
                three_phase,
            ),
        ];
        let snap = decode_snapshot(&blocks);
        assert_eq!(snap.device_type, DeviceType::ThreePhase);
        assert_eq!(snap.charge_rate, 77);
        assert_eq!(snap.discharge_rate, 88);
        assert_eq!(snap.battery_reserve, 25);
        assert_eq!(snap.target_soc, 95);
        assert!(snap.enable_charge);
        assert!(snap.enable_discharge);
    }

    #[test]
    fn three_phase_decodes_schedule_slots_from_three_phase_map() {
        let mut holding_data = vec![0u16; 60];
        holding_data[0] = 0x4001; // Three Phase
                                  // Bogus single-phase slot values must be ignored/overridden for three-phase models.
        holding_data[31] = 101;
        holding_data[32] = 202;
        holding_data[44] = 303;
        holding_data[45] = 404;
        holding_data[56] = 505;
        holding_data[57] = 606;

        let mut holding_60_data = vec![0u16; 60];
        holding_60_data[94 - 60] = 707;
        holding_60_data[95 - 60] = 808;

        let mut three_phase = vec![0u16; 45];
        three_phase[1109 - 1080] = 20;
        three_phase[1113 - 1080] = 130;
        three_phase[1114 - 1080] = 530;
        three_phase[1115 - 1080] = 600;
        three_phase[1116 - 1080] = 900;
        three_phase[1118 - 1080] = 1600;
        three_phase[1119 - 1080] = 1900;
        three_phase[1120 - 1080] = 2000;
        three_phase[1121 - 1080] = 2230;
        // Force/ac-charge flags deliberately remain false: they are not schedule master flags.

        let mut extended = vec![0u16; 60];
        extended[246 - 240] = 2300;
        extended[247 - 240] = 30;
        extended[248 - 240] = 85;
        extended[276 - 240] = 1000;
        extended[277 - 240] = 1200;
        extended[278 - 240] = 40;

        let blocks = vec![
            make_block(RegisterType::Input, 0, 60, "input_0_59", vec![0; 60]),
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", holding_data),
            make_block(
                RegisterType::Holding,
                60,
                60,
                "holding_60_119",
                holding_60_data,
            ),
            make_block(
                RegisterType::Holding,
                1080,
                45,
                "holding_1080_1124",
                three_phase,
            ),
            make_block(RegisterType::Holding, 240, 60, "holding_240_299", extended),
        ];

        let snap = decode_snapshot(&blocks);
        assert_eq!(snap.device_type, DeviceType::ThreePhase);
        assert_eq!(snap.max_charge_slots, 10);
        assert_eq!(snap.max_discharge_slots, 10);

        assert!(snap.charge_slots[0].enabled);
        assert_eq!(snap.charge_slots[0].start_hour, 1);
        assert_eq!(snap.charge_slots[0].start_minute, 30);
        assert_eq!(snap.charge_slots[0].end_hour, 5);
        assert_eq!(snap.charge_slots[0].end_minute, 30);

        assert!(snap.charge_slots[1].enabled);
        assert_eq!(snap.charge_slots[1].start_hour, 6);
        assert_eq!(snap.charge_slots[1].end_hour, 9);

        assert!(snap.discharge_slots[0].enabled);
        assert_eq!(snap.discharge_slots[0].start_hour, 16);
        assert_eq!(snap.discharge_slots[0].end_hour, 19);

        assert!(snap.discharge_slots[1].enabled);
        assert_eq!(snap.discharge_slots[1].start_hour, 20);
        assert_eq!(snap.discharge_slots[1].end_hour, 22);
        assert_eq!(snap.discharge_slots[1].end_minute, 30);

        assert!(snap.charge_slots[2].enabled);
        assert_eq!(snap.charge_slots[2].start_hour, 23);
        assert_eq!(snap.charge_slots[2].end_hour, 0);
        assert_eq!(snap.charge_slots[2].end_minute, 30);
        assert_eq!(snap.charge_slots[2].target_soc, 85);

        assert!(snap.discharge_slots[2].enabled);
        assert_eq!(snap.discharge_slots[2].start_hour, 10);
        assert_eq!(snap.discharge_slots[2].end_hour, 12);
        assert_eq!(snap.discharge_slots[2].target_soc, 40);
    }

    #[test]
    fn single_phase_daily_solar_uses_ir44_not_lifetime_pv_total() {
        let mut input_data = vec![0u16; 60];
        input_data[11] = 3;
        input_data[12] = 4641; // lifetime PV total = 201249 * 0.1 = 20124.9 kWh; not a daily value
        input_data[17] = 0;
        input_data[19] = 0;
        input_data[25] = 10; // export today 1.0 kWh
        input_data[26] = 20; // import today 2.0 kWh
        input_data[35] = 5; // AC charge today 0.5 kWh
        input_data[44] = 37; // PV generation today 3.7 kWh

        let mut holding_data = vec![0u16; 60];
        holding_data[0] = 0x3001; // AC Coupled

        let blocks = vec![
            make_block(RegisterType::Input, 0, 60, "input_0_59", input_data),
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", holding_data),
            make_block(RegisterType::Holding, 60, 60, "holding_60_119", vec![0; 60]),
        ];
        let snap = decode_snapshot(&blocks);

        assert_eq!(snap.device_type, DeviceType::ACCoupled);
        assert!((snap.today_solar_kwh - 3.7).abs() < 0.01);
        // GE app formula for single-phase consumption: PV generation + import - export - AC charge.
        assert!((snap.today_consumption_kwh - 4.2).abs() < 0.01);
        assert!((snap.today_ac_charge_kwh - 0.5).abs() < 0.01);
    }

    #[test]
    fn single_phase_consumption_formula_with_ir44() {
        // Gen3 hybrid with solar from primary IR(44) path (not fallback).
        // Verifies consumption computation when IR(44) is non-zero.
        let mut input_data = vec![0u16; 60];
        input_data[25] = 50; // export today 5.0 kWh
        input_data[26] = 80; // import today 8.0 kWh
        input_data[35] = 30; // AC charge today 3.0 kWh
        input_data[44] = 100; // PV generation today 10.0 kWh
        input_data[59] = 50; // SOC

        let mut holding_data = vec![0u16; 60];
        holding_data[0] = 0x2101; // PolarHybrid (fixed, no ARM refinement needed)
        holding_data[59] = 1; // enable_discharge = true

        let blocks = vec![
            make_block(RegisterType::Input, 0, 60, "input_0_59", input_data),
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", holding_data),
            make_block(RegisterType::Holding, 60, 60, "holding_60_119", vec![0; 60]),
        ];
        let snap = decode_snapshot(&blocks);

        assert_eq!(snap.device_type, DeviceType::PolarHybrid);
        assert!((snap.today_solar_kwh - 10.0).abs() < 0.01);
        assert!((snap.today_import_kwh - 8.0).abs() < 0.01);
        assert!((snap.today_export_kwh - 5.0).abs() < 0.01);
        assert!((snap.today_ac_charge_kwh - 3.0).abs() < 0.01);
        // Consumption = 10.0 + 8.0 - 5.0 - 3.0 = 10.0
        assert!((snap.today_consumption_kwh - 10.0).abs() < 0.01);
    }

    #[test]
    fn ac_coupled_uses_ac_charge_discharge_limits() {
        let mut holding_data = vec![0u16; 60];
        holding_data[0] = 0x3001; // AC Coupled

        let mut holding_60_data = vec![0u16; 60];
        holding_60_data[111 - 60] = 11; // DC hybrid charge limit — should be ignored for AC
        holding_60_data[112 - 60] = 12; // DC hybrid discharge limit — should be ignored for AC

        let mut ac_config = vec![0u16; 60];
        ac_config[313 - 300] = 77;
        ac_config[314 - 300] = 88;

        let blocks = vec![
            make_block(RegisterType::Input, 0, 60, "input_0_59", vec![0; 60]),
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", holding_data),
            make_block(
                RegisterType::Holding,
                60,
                60,
                "holding_60_119",
                holding_60_data,
            ),
            make_block(RegisterType::Holding, 300, 60, "holding_300_359", ac_config),
        ];
        let snap = decode_snapshot(&blocks);
        assert_eq!(snap.device_type, DeviceType::ACCoupled);
        assert_eq!(snap.charge_rate, 77);
        assert_eq!(snap.discharge_rate, 88);
    }

    #[test]
    fn decode_timed_demand_mode() {
        let mut holding_data = vec![0u16; 60];
        holding_data[27] = 1; // eco mode
        holding_data[59] = 1; // discharge enabled
        holding_data[56] = 1600; // discharge slot 1 start = 16:00
        holding_data[57] = 1900; // discharge slot 1 end = 19:00

        let mut holding_60_data = vec![0u16; 60];
        holding_60_data[110 - 60] = 100;

        let blocks = vec![
            make_block(RegisterType::Input, 0, 60, "input_0_59", vec![0; 60]),
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", holding_data),
            make_block(
                RegisterType::Holding,
                60,
                60,
                "holding_60_119",
                holding_60_data,
            ),
        ];
        let snap = decode_snapshot(&blocks);
        assert_eq!(snap.battery_mode, BatteryMode::TimedDemand);
    }

    #[test]
    fn decode_timed_export_mode() {
        let mut holding_data = vec![0u16; 60];
        holding_data[27] = 0; // export mode
        holding_data[59] = 1; // discharge enabled
        holding_data[56] = 1600; // discharge slot 1 start = 16:00
        holding_data[57] = 1900; // discharge slot 1 end = 19:00

        let blocks = vec![
            make_block(RegisterType::Input, 0, 60, "input_0_59", vec![0; 60]),
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", holding_data),
            make_block(RegisterType::Holding, 60, 60, "holding_60_119", vec![0; 60]),
        ];
        let snap = decode_snapshot(&blocks);
        assert_eq!(snap.battery_mode, BatteryMode::TimedExport);
    }

    #[test]
    fn decode_eco_paused_mode() {
        let mut holding_data = vec![0u16; 60];
        holding_data[27] = 1; // eco mode
        holding_data[59] = 0; // discharge disabled

        let mut holding_60_data = vec![0u16; 60];
        holding_60_data[110 - 60] = 100; // reserve = 100 → EcoPaused

        let blocks = vec![
            make_block(RegisterType::Input, 0, 60, "input_0_59", vec![0; 60]),
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", holding_data),
            make_block(
                RegisterType::Holding,
                60,
                60,
                "holding_60_119",
                holding_60_data,
            ),
        ];
        let snap = decode_snapshot(&blocks);
        assert_eq!(snap.battery_mode, BatteryMode::EcoPaused);
    }

    #[test]
    fn decode_export_paused_mode() {
        let mut holding_data = vec![0u16; 60];
        holding_data[27] = 0; // export mode
        holding_data[59] = 0; // discharge disabled

        let mut holding_60_data = vec![0u16; 60];
        holding_60_data[110 - 60] = 10; // reserve != 100 → ExportPaused

        let blocks = vec![
            make_block(RegisterType::Input, 0, 60, "input_0_59", vec![0; 60]),
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", holding_data),
            make_block(
                RegisterType::Holding,
                60,
                60,
                "holding_60_119",
                holding_60_data,
            ),
        ];
        let snap = decode_snapshot(&blocks);
        assert_eq!(snap.battery_mode, BatteryMode::ExportPaused);
    }

    #[test]
    fn timeslot_midnight_zero_length_is_disabled() {
        // Both start and end = 0 means 00:00–00:00, treated as disabled
        // (the reference library writes 0 to clear slots)
        let mut holding_data = vec![0u16; 60];
        holding_data[31] = 0; // charge_slot_2 start = 0 → 00:00
        holding_data[32] = 0; // charge_slot_2 end = 0 → 00:00

        let blocks = vec![
            make_block(RegisterType::Input, 0, 60, "input_0_59", vec![0; 60]),
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", holding_data),
            make_block(RegisterType::Holding, 60, 60, "holding_60_119", vec![0; 60]),
        ];
        let snap = decode_snapshot(&blocks);
        assert!(!snap.charge_slots[1].enabled); // 00:00–00:00 = disabled
        assert_eq!(snap.charge_slots[1].start_hour, 0);
        assert_eq!(snap.charge_slots[1].start_minute, 0);
        assert_eq!(snap.charge_slots[1].end_hour, 0);
        assert_eq!(snap.charge_slots[1].end_minute, 0);
    }

    #[test]
    fn timeslot_60_means_disabled() {
        // 60 is the disabled sentinel per givenergy-modbus reference
        let mut holding_data = vec![0u16; 60];
        holding_data[31] = 60; // charge_slot_2 start = 60 → disabled
        holding_data[32] = 60; // charge_slot_2 end = 60 → disabled

        let blocks = vec![
            make_block(RegisterType::Input, 0, 60, "input_0_59", vec![0; 60]),
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", holding_data),
            make_block(RegisterType::Holding, 60, 60, "holding_60_119", vec![0; 60]),
        ];
        let snap = decode_snapshot(&blocks);
        assert!(!snap.charge_slots[1].enabled);
    }

    #[test]
    fn timeslot_midnight_start_valid() {
        // Start=0 (00:00), end=800 (08:00) is a valid slot.
        // Also set enable_charge=1 so the global override doesn't
        // disable the slot (HR(96) → index 36).
        let mut holding_data = vec![0u16; 60];
        holding_data[34] = 0; // charge_slot_1 start = 0 → 00:00
        holding_data[35] = 800; // charge_slot_1 end = 800 → 08:00
        holding_data[36] = 1; // enable_charge = 1

        let blocks = vec![
            make_block(RegisterType::Input, 0, 60, "input_0_59", vec![0; 60]),
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", vec![0; 60]),
            make_block(
                RegisterType::Holding,
                60,
                60,
                "holding_60_119",
                holding_data,
            ),
        ];
        let snap = decode_snapshot(&blocks);
        assert!(
            snap.charge_slots[0].enabled,
            "00:00-08:00 should be enabled"
        );
        assert_eq!(snap.charge_slots[0].start_hour, 0);
        assert_eq!(snap.charge_slots[0].start_minute, 0);
        assert_eq!(snap.charge_slots[0].end_hour, 8);
        assert_eq!(snap.charge_slots[0].end_minute, 0);
    }

    #[test]
    fn timeslot_non_zero_equal_values_disabled() {
        // start == end (e.g. 600, 600) is zero-duration → disabled
        let mut holding_data = vec![0u16; 60];
        holding_data[34] = 1200; // 12:00
        holding_data[35] = 1200; // 12:00 = no duration

        let blocks = vec![
            make_block(RegisterType::Input, 0, 60, "input_0_59", vec![0; 60]),
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", vec![0; 60]),
            make_block(
                RegisterType::Holding,
                60,
                60,
                "holding_60_119",
                holding_data,
            ),
        ];
        let snap = decode_snapshot(&blocks);
        assert!(
            !snap.charge_slots[0].enabled,
            "12:00-12:00 should be disabled"
        );
    }

    #[test]
    fn battery_state_derivation() {
        let mut input_data = vec![0u16; 60];

        // Battery discharging in inverter convention: raw p_battery = +500 (positive = discharging).
        // After decoder negation: -500 (negative = discharging in our model).
        input_data[52] = 500;

        let blocks = vec![
            make_block(RegisterType::Input, 0, 60, "input_0_59", input_data),
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", vec![0; 60]),
            make_block(RegisterType::Holding, 60, 60, "holding_60_119", vec![0; 60]),
        ];
        let snap = decode_snapshot(&blocks);
        assert_eq!(snap.battery_power, -500);
        assert_eq!(snap.battery_state, BatteryState::Discharging);
    }

    #[test]
    fn grid_power_signed_import() {
        let mut input_data = vec![0u16; 60];
        // Grid importing: p_grid_out = -200 (negative = importing, per GivTCP)
        input_data[30] = (-200i16) as u16;

        let blocks = vec![
            make_block(RegisterType::Input, 0, 60, "input_0_59", input_data),
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", vec![0; 60]),
            make_block(RegisterType::Holding, 60, 60, "holding_60_119", vec![0; 60]),
        ];
        let snap = decode_snapshot(&blocks);
        // Raw -200 kept as-is (negative = importing)
        assert_eq!(snap.grid_power, -200);
    }

    // --- HV BCU cluster decode ---
    //
    // Register values mirror the givenergy-modbus `test_bcu_from_synthetic_registers`
    // fixture so we stay aligned with the reference layout.

    fn hv_bcu_fixture() -> Vec<u16> {
        // 60-register slice for IR 60-119 read from device 0x70.
        let mut d = vec![0u16; 60];
        d[0] = 0x4741; // IR(60): 'G','A'
        d[1] = 0x3030; // IR(61): '0','0'
        d[2] = 0x0000; // IR(62)
        d[3] = 0x0005; // IR(63): version suffix → "GA000005"
        d[64 - 60] = 5; // IR(64): number_of_modules = 5
        d[65 - 60] = 24; // IR(65): cells_per_module = 24
        d[67 - 60] = 3200; // IR(67): cluster_cell_voltage = 3.200 V
        d[68 - 60] = 250; // IR(68): cluster_cell_temperature = 25.0 °C (deci)
        d[70 - 60] = 0x01; // IR(70): status = normal
        d[73 - 60] = 3840; // IR(73): battery_voltage = 384.0 V
        d[74 - 60] = 3820; // IR(74): load_voltage = 382.0 V
        d[76 - 60] = (-125i16) as u16; // IR(76): battery_current = -12.5 A
        d[79 - 60] = 4800; // IR(79): battery_power = 4.8 kW
        d[80 - 60] = (90 << 8) | 85; // IR(80): soc_max=90, soc_min=85
        d[81 - 60] = 98; // IR(81): battery_soh = 98
        d[98 - 60] = 510; // IR(98): nominal_capacity_ah = 51.0 Ah (per module)
        d[99 - 60] = 440; // IR(99): remaining_capacity_ah = 44.0 Ah (per module)
        d
    }

    #[test]
    fn decode_hv_bcu_cluster_matches_reference_layout() {
        let data = hv_bcu_fixture();
        let c = decode_hv_bcu_cluster(&data);
        assert_eq!(c.pack_software_version, "GA000005");
        assert_eq!(c.number_of_modules, 5);
        assert_eq!(c.cells_per_module, 24);
        assert!((c.cluster_cell_voltage - 3.2).abs() < 0.001);
        assert_eq!(c.status, 0x01);
        assert!((c.battery_voltage - 384.0).abs() < 0.001);
        assert!((c.battery_current - -12.5).abs() < 0.001);
        assert_eq!(c.battery_power_w, 4800);
        assert_eq!(c.battery_soc_max, 90);
        assert_eq!(c.battery_soc_min, 85);
        assert_eq!(c.battery_soh, 98);
        assert!((c.temperature - 25.0).abs() < 0.001);
        assert!((c.nominal_capacity_ah - 51.0).abs() < 0.001);
        assert!((c.remaining_capacity_ah - 44.0).abs() < 0.001);
        // Stack totals: per-module Ah × module count.
        assert!((c.total_capacity_ah() - 255.0).abs() < 0.001);
        assert!((c.total_remaining_ah() - 220.0).abs() < 0.001);
    }

    #[test]
    fn validate_hv_bcu_accepts_present_stack() {
        assert!(validate_hv_bcu(&hv_bcu_fixture()));
    }

    #[test]
    fn validate_hv_bcu_rejects_empty_block() {
        // All-zero registers: no pack_software_version → not a real BCU.
        assert!(!validate_hv_bcu(&vec![0u16; 60]));
    }

    #[test]
    fn validate_hv_bcu_rejects_all_zero_version() {
        // A version field that is all '0' characters is the no-stack sentinel
        // per givenergy-modbus Bcu.is_valid() (covers the distinct "present but
        // all-zero" case from the empty-block case above). Prefix regs set to
        // 0x3030 ('0','0') and digit regs zeroed yields "00000000".
        let mut data = vec![0u16; 60];
        data[0] = 0x3030; // '0','0'
        data[1] = 0x3030;
        data[2] = 0x0000;
        data[3] = 0x0000;
        assert!(!validate_hv_bcu(&data));
    }

    #[test]
    fn decode_gateway_version_matches_reference_converter() {
        // Mirrors givenergy-modbus Converter.gateway_version: [0x4741, 0x3030,
        // 0x0000, 0x0005] → "GA000005".
        let data = [0x4741u16, 0x3030, 0x0000, 0x0005, 0, 0];
        assert_eq!(decode_gateway_version(&data, 0), "GA000005");
    }

    // --- HV BMU per-module decode ---

    fn hv_bmu_fixture() -> Vec<u16> {
        // 60-register slice for device 0x50+m read at base 60. v_cell_01 is at
        // offset 0, t_cell_01 at offset 30, serial at offset 54.
        let mut d = vec![0u16; 60];
        // 24 cell voltages, ~3.2V each = 3200 mV. Vary slightly to verify sum.
        for i in 0..24 {
            d[i] = (3200 + i as u16) % 3400;
        }
        // 24 cell temperatures, ~25-28 °C (0.1 °C).
        for i in 0..24 {
            d[30 + i] = 250 + i as u16;
        }
        // Serial at offsets 54-58: "BM0000001"-ish (Latin-1, 5 regs).
        d[54] = 0x4234; // 'B','4'
        d[55] = 0x3030; // '0','0'
        d[56] = 0x3030;
        d[57] = 0x3030;
        d[58] = 0x3031; // '0','1'
        d
    }

    #[test]
    fn decode_hv_bmu_block_extracts_cells_temps_serial() {
        let data = hv_bmu_fixture();
        let m = decode_hv_bmu_block(&data, 2);
        assert_eq!(m.index, 2);
        assert_eq!(m.cell_voltages.len(), 24);
        assert!((m.cell_voltages[0] - 3.2).abs() < 0.001);
        assert!((m.cell_voltages[23] - 3.223).abs() < 0.001);
        // Module terminal voltage = sum of cells.
        let expected_sum: f32 = (0u16..24).map(|i| ((3200 + i) % 3400) as f32 * 0.001).sum();
        assert!((m.voltage - expected_sum).abs() < 0.01);
        assert_eq!(m.cell_temperatures.len(), 24);
        assert!((m.cell_temperatures[0] - 25.0).abs() < 0.001);
        // Hottest cell temp = 25.0 + 23*0.1 = 27.3 °C.
        assert!((m.temperature - 27.3).abs() < 0.001);
        assert_eq!(m.num_cells, 24);
        assert!(m.serial.contains("B4"));
        // Pack-level fields are BCU-sourced, not BMU.
        assert_eq!(m.soc, 0);
        assert_eq!(m.capacity_ah, 0.0);
        assert_eq!(m.current, 0.0);
    }

    #[test]
    fn validate_hv_bmu_accepts_present_module() {
        assert!(validate_hv_bmu(&hv_bmu_fixture()));
    }

    #[test]
    fn validate_hv_bmu_rejects_empty_block() {
        // All-zero serial → not a real module.
        assert!(!validate_hv_bmu(&vec![0u16; 60]));
    }

    #[test]
    fn backfill_hv_module_fields_sets_soc_from_cluster_average() {
        // HV BMU has no per-module SOC register; the BCU cluster reports the
        // stack-wide spread (soc_max=90, soc_min=84 → avg 87). All modules in
        // the stack take the same estimate, plus per-module Ah from IR(98).
        use crate::inverter::decoder::HvBcuCluster;
        use crate::inverter::model::BatteryModule;
        let cluster = HvBcuCluster {
            number_of_modules: 5,
            battery_soc_max: 90,
            battery_soc_min: 84,
            nominal_capacity_ah: 51.0,
            ..Default::default()
        };
        let mut modules: Vec<BatteryModule> = (0..5)
            .map(|i| BatteryModule {
                index: i,
                soc: 0, // decode_hv_bmu_block leaves SOC at 0
                capacity_ah: 0.0,
                design_capacity_ah: 0.0,
                remaining_capacity_ah: 0.0,
                ..Default::default()
            })
            .collect();
        backfill_hv_module_fields(&mut modules, &cluster);
        for m in &modules {
            assert_eq!(m.soc, 87, "module {} SOC not backfilled", m.index);
            assert!((m.capacity_ah - 51.0).abs() < 0.001);
            assert!((m.design_capacity_ah - 51.0).abs() < 0.001);
            // Remaining = 51Ah * 87% = 44.37 Ah.
            assert!((m.remaining_capacity_ah - 44.37).abs() < 0.01);
        }
    }

    #[test]
    fn charge_slot_enabled_not_gated_on_enable_charge_flag() {
        // Regression test: charge slots with valid times must remain
        // enabled=true even when the master enable_charge flag is false.
        // Previously the decoder set all charge slots to enabled=false
        // when enable_charge was 0, which hid configured schedules after
        // mode switches (ECO ←→ Timed) when some inverter firmware clears
        // HR 96 during the transition. The slot times are always preserved
        // in the registers — only the enable_charge flag changes.
        //
        // Discharge slots already have this independence (they are NOT gated
        // on enable_discharge). This test extends the same treatment to
        // charge slots.
        let snap = crate::inverter::model::InverterSnapshot {
            device_type: crate::inverter::model::DeviceType::Gen2Hybrid,
            enable_charge: false, // master flag OFF
            enable_discharge: true,
            charge_slots: [
                crate::inverter::model::ScheduleSlot {
                    enabled: true,
                    start_hour: 1,
                    start_minute: 0,
                    end_hour: 5,
                    end_minute: 0,
                    target_soc: 100,
                },
                crate::inverter::model::ScheduleSlot {
                    enabled: true,
                    start_hour: 6,
                    start_minute: 0,
                    end_hour: 10,
                    end_minute: 0,
                    target_soc: 100,
                },
                crate::inverter::model::ScheduleSlot::default(),
                crate::inverter::model::ScheduleSlot::default(),
                crate::inverter::model::ScheduleSlot::default(),
                crate::inverter::model::ScheduleSlot::default(),
                crate::inverter::model::ScheduleSlot::default(),
                crate::inverter::model::ScheduleSlot::default(),
                crate::inverter::model::ScheduleSlot::default(),
                crate::inverter::model::ScheduleSlot::default(),
            ],
            discharge_slots: [
                crate::inverter::model::ScheduleSlot {
                    enabled: true,
                    start_hour: 16,
                    start_minute: 0,
                    end_hour: 19,
                    end_minute: 0,
                    target_soc: 4,
                },
                crate::inverter::model::ScheduleSlot::default(),
                crate::inverter::model::ScheduleSlot::default(),
                crate::inverter::model::ScheduleSlot::default(),
                crate::inverter::model::ScheduleSlot::default(),
                crate::inverter::model::ScheduleSlot::default(),
                crate::inverter::model::ScheduleSlot::default(),
                crate::inverter::model::ScheduleSlot::default(),
                crate::inverter::model::ScheduleSlot::default(),
                crate::inverter::model::ScheduleSlot::default(),
            ],
            ..Default::default()
        };
        // Since the gating in decode_all was removed, charge_slots retain
        // their per-slot enabled state regardless of enable_charge.
        assert!(
            snap.charge_slots[0].enabled,
            "charge slot 0 must remain enabled when enable_charge is false"
        );
        assert!(
            snap.charge_slots[1].enabled,
            "charge slot 1 must remain enabled when enable_charge is false"
        );
        // Discharge slots were already independent
        assert!(
            snap.discharge_slots[0].enabled,
            "discharge slot 0 must remain enabled (was already independent)"
        );
    }

    // -----------------------------------------------------------------------
    // Meter validation tests
    // -----------------------------------------------------------------------

    #[test]
    fn meter_validation_accepts_nonzero_voltage() {
        // Any non-zero plausible voltage means a meter is present.
        let (valid, v1) = validate_meter_data(&[2300]); // 230.0V
        assert!(valid);
        assert!((v1 - 230.0).abs() < 0.1);
    }

    #[test]
    fn meter_validation_rejects_zero_voltage() {
        let (valid, v1) = validate_meter_data(&[0]);
        assert!(!valid);
        assert_eq!(v1, 0.0);
    }

    #[test]
    fn meter_validation_rejects_empty_data() {
        let (valid, v1) = validate_meter_data(&[]);
        assert!(!valid);
        assert_eq!(v1, 0.0);
    }

    #[test]
    fn meter_validation_accepts_low_but_nonzero_voltage() {
        // givenergy-modbus only checks non-zero. A meter reporting 5.0V
        // is present, even if the reading seems low.
        let (valid, v1) = validate_meter_data(&[50]); // 5.0V
        assert!(valid);
        assert!((v1 - 5.0).abs() < 0.1);
    }

    #[test]
    fn meter_validation_rejects_extreme_corruption() {
        // >500V is implausible corruption, not a real meter.
        let (valid, v1) = validate_meter_data(&[9999]); // 999.9V
        assert!(!valid);
        assert!((v1 - 999.9).abs() < 0.1);
    }

    // -----------------------------------------------------------------------
    // Gateway (DTC 0x70xx) aggregation bank decode
    // -----------------------------------------------------------------------

    /// Build the five gateway input blocks with the given register data
    /// (60 or 29 regs each).
    fn gateway_block1(data_1600_1659: [u16; 60]) -> BlockRead {
        make_block(RegisterType::Input, 1600, 60, "input_1600_1659", data_1600_1659.to_vec())
    }
    fn gateway_block2(data_1660_1719: [u16; 60]) -> BlockRead {
        make_block(RegisterType::Input, 1660, 60, "input_1660_1719", data_1660_1719.to_vec())
    }
    fn gateway_block3(data_1720_1779: [u16; 60]) -> BlockRead {
        make_block(RegisterType::Input, 1720, 60, "input_1720_1779", data_1720_1779.to_vec())
    }
    fn gateway_block4(data_1780_1830: [u16; 51]) -> BlockRead {
        make_block(RegisterType::Input, 1780, 51, "input_1780_1830", data_1780_1830.to_vec())
    }
    fn gateway_block5_v1(data_1831_1859: [u16; 29]) -> BlockRead {
        make_block(RegisterType::Input, 1831, 29, "input_1831_1859", data_1831_1859.to_vec())
    }
    /// Holding register 0-59 block with DTC 0x7001 (Gateway).
    fn gateway_holding_block() -> BlockRead {
        let mut h = [0u16; 60];
        h[0] = 0x7001; // DeviceType::Gateway
        make_block(RegisterType::Holding, 0, 60, "holding_0_59", h.to_vec())
    }

    fn gateway_fixture_blocks_v1() -> Vec<BlockRead> {
        let mut b1 = [0u16; 60];
        // Identity: "GA000009" → V1.
        b1[0] = 0x4741; // 'G','A'
        b1[1] = 0x3030; // '0','0'
        b1[2] = 0x0000; // 0,0
        b1[3] = 0x0009; // 9 (last digit) — V1 (IR(1603) < 10)
        b1[4] = 2; // work_mode = On Grid
        // Grid: v_grid = 2410 → 241.0 V
        b1[8] = 2410;
        // PV: p_pv = 3500 W
        b1[17] = 3500;
        // Load: p_load = 1200 W
        b1[18] = 1200;
        // AIO power: p_ac1 = -500 W (charging in GE sign)
        b1[16] = (-500i16) as u16;
        // First AIO serial "SA24230001" (5 regs).
        b1[27] = 0x5341; // 'S','A'
        b1[28] = 0x3234; // '2','4'
        b1[29] = 0x3233; // '2','3'
        b1[30] = 0x3030; // '0','0'
        b1[31] = 0x3031; // '0','1'
        // Energy today: import=12.3, solar=45.6, export=3.4, charge=20.0, discharge=18.5, load=30.0
        b1[40] = 123; // e_grid_import_today
        b1[43] = 456; // e_pv_today
        b1[46] = 34;  // e_grid_export_today
        b1[49] = 200; // e_aio_charge_today
        b1[52] = 185; // e_aio_discharge_today
        b1[55] = 300; // e_load_today
        // Lifetime totals (V1 hi/lo): import=12345.6, export=543.2, charge=8000.0, discharge=7500.0
        // e_grid_import_total: IR(1641)=0x0001, IR(1642)=0xE240 → (1<<16)|57856 = 123392 /10 = 12339.2
        // Actually use clean values: 1234 = 0x04D2 high, 5678 = 0x162E low → uint32=80913222 /10 ≈ 8091322.2 no
        // Let's use: hi=0x0001=1, lo=0x0000=0 → 65536 /10 = 6553.6
        b1[41] = 1; b1[42] = 0;   // e_grid_import_total high, low
        b1[44] = 0; b1[45] = 1000; // e_pv_total = 1000 /10 = 100.0
        b1[47] = 0; b1[48] = 500;  // e_grid_export_total = 500/10 = 50.0
        b1[50] = 2; b1[51] = 0;    // e_aio_charge_total = 131072/10 = 13107.2
        b1[53] = 1; b1[54] = 5000; // e_aio_discharge_total = (1<<16)|5000 = 70536/10 = 7053.6

        let mut b2 = [0u16; 60];
        b2[40] = 1; // parallel_aio_num = 1
        b2[41] = 1; // parallel_aio_online = 1
        b2[42] = (-800i16) as u16; // p_aio_total = -800 W (charging in GE sign)
        b2[43] = 1; // aio_state = 1 (charging)
        b2[45] = 200; // e_aio1_charge_today = 20.0 kWh

        let mut b3 = [0u16; 60];
        b3[30] = 185; // e_aio1_discharge_today = 18.5 kWh

        let mut b4 = [0u16; 51];
        b4[21] = 75; // aio1_soc = 75%
        b4[36] = 800i16 as u16; // p_aio1_inverter = 800 W (discharging in GE sign)

        let mut b5 = [0u16; 29];
        // V1: aio1 serial @ 1831-1835 (offsets 0-4)
        b5[0] = 0x5341; // 'S','A'
        b5[1] = 0x3234; // '2','4'
        b5[2] = 0x3233; // '2','3'
        b5[3] = 0x3030; // '0','0'
        b5[4] = 0x3031; // '0','1'

        vec![
            gateway_holding_block(),
            gateway_block1(b1),
            gateway_block2(b2),
            gateway_block3(b3),
            gateway_block4(b4),
            gateway_block5_v1(b5),
        ]
    }

    #[test]
    fn gateway_validates_version() {
        // All-zero bank (non-gateway device or failed read) → not valid.
        assert!(!gateway_is_valid("00000000"));
        assert!(!gateway_is_valid(""));
        assert!(!gateway_is_valid("   "));
        // Real gateway version → valid.
        assert!(gateway_is_valid("GA000009"));
        assert!(gateway_is_valid("GA000010"));
    }

    #[test]
    fn gateway_decode_identity_and_faults() {
        let mut b1 = [0u16; 60];
        b1[0] = 0x4741; // 'G','A'
        b1[1] = 0x3030; // '0','0'
        b1[2] = 0x0000;
        b1[3] = 0x000A; // 10 = V2 (GA000010)
        b1[4] = 2; // work_mode = On Grid

        // Set a fault bitmask with bit 0 (Relay 1&2 bonding) and bit 31 (Grid mode Off).
        // Bit 0 = MSB (bit 31 of u32), bit 31 = LSB (bit 0 of u32).
        // Fault bit 0 = 1 → u32 bit 31 = 1 → IR(1622) bit 15 = 1 → IR1622 = 0x8000
        // Fault bit 31 = 1 → u32 bit 0 = 1 → IR(1623) bit 0 = 1 → IR1623 = 0x0001
        b1[22] = 0x8000; // IR 1622 high word
        b1[23] = 0x0001; // IR 1623 low word

        let blocks = vec![
            gateway_holding_block(),
            gateway_block1(b1),
            gateway_block2([0u16; 60]),
            gateway_block3([0u16; 60]),
            gateway_block4([0u16; 51]),
            gateway_block5_v1([0u16; 29]),
        ];
        let snap = decode_snapshot(&blocks);

        assert_eq!(snap.gateway_software_version, "GA0000010");
        assert!(snap.gateway_is_v2);
        assert_eq!(snap.gateway_work_mode, 2);
        assert!(snap.gateway_fault_codes.iter().any(|f| f == "Relay 1&2 bonding"));
        assert!(snap.gateway_fault_codes.iter().any(|f| f == "Grid mode Off"));
        assert!(!snap.gateway_fault_codes.iter().any(|f| f == "Relay 3&4 bonding"));
        assert!(gateway_is_valid(&snap.gateway_software_version));
    }

    #[test]
    fn gateway_decode_power_flow_sign_conventions() {
        let blocks = gateway_fixture_blocks_v1();
        let snap = decode_snapshot(&blocks);

        // solar_power: p_pv = 3500 W (unsigned, direct)
        assert_eq!(snap.solar_power, 3500);
        assert_eq!(snap.pv1_power, 3500);

        // home_power: p_load = 1200 W (authoritative, excludes EV)
        assert_eq!(snap.home_power, 1200);

        // battery_power: -p_aio_total = -(-800) = 800 W (HEM: + = charge)
        assert_eq!(snap.battery_power, 800);
        assert_eq!(snap.battery_state, BatteryState::Charging);

        // grid_power (derived): solar - battery - home = 3500 - 800 - 1200 = 1500 W
        // Positive = export (solar charges battery AND exports to grid)
        assert_eq!(snap.grid_power, 1500);

        // grid_voltage from v_grid = 241.0 V
        assert!((snap.grid_voltage - 241.0).abs() < 0.1);

        // grid_frequency is NaN (not available in gateway bank)
        assert!(snap.grid_frequency.is_nan());

        // Capacities from parallel_aio_num
        assert!((snap.battery_capacity_kwh - 13.5).abs() < 0.01);
        assert_eq!(snap.max_battery_power_w, 6000);
    }

    #[test]
    fn gateway_decode_today_energy_v1_byte_order() {
        let blocks = gateway_fixture_blocks_v1();
        let snap = decode_snapshot(&blocks);

        assert!((snap.today_solar_kwh - 45.6).abs() < 0.01);
        assert!((snap.today_import_kwh - 12.3).abs() < 0.01);
        assert!((snap.today_export_kwh - 3.4).abs() < 0.01);
        assert!((snap.today_charge_kwh - 20.0).abs() < 0.01);
        assert!((snap.today_discharge_kwh - 18.5).abs() < 0.01);
        assert!((snap.today_consumption_kwh - 30.0).abs() < 0.01);
    }

    #[test]
    fn gateway_decode_lifetime_energy_v1_byte_order() {
        let blocks = gateway_fixture_blocks_v1();
        let snap = decode_snapshot(&blocks);

        // e_grid_import_total: hi=1, lo=0 → (1<<16)|0 = 65536 → /10 = 6553.6
        assert!((snap.total_import_kwh - 6553.6).abs() < 0.1);
        // e_pv_total: hi=0, lo=1000 → 1000/10 = 100.0
        assert!((snap.total_export_kwh - 50.0).abs() < 0.01);
        // e_aio_charge_total: hi=2, lo=0 → 131072/10 = 13107.2
        assert!((snap.total_charge_kwh - 13107.2).abs() < 0.1);
        // e_aio_discharge_total: hi=1, lo=5000 → 70536/10 = 7053.6
        assert!((snap.total_discharge_kwh - 7053.6).abs() < 0.1);
    }

    #[test]
    fn gateway_decode_per_aio_fields_v1() {
        let blocks = gateway_fixture_blocks_v1();
        let snap = decode_snapshot(&blocks);

        assert_eq!(snap.parallel_aio_count, 1);
        assert_eq!(snap.parallel_aio_online, 1);
        assert_eq!(snap.soc, 75); // aggregate SOC = AIO1
        assert_eq!(snap.per_aio_soc[0], 75);
        assert_eq!(snap.per_aio_soc[1], 0);
        assert_eq!(snap.per_aio_soc[2], 0);

        // Per-AIO power (GE sign): p_aio1_inverter = 800 W (discharge) → per_aio_power = 800
        assert_eq!(snap.per_aio_power[0], 800);

        // Per-AIO charge/discharge today
        assert!((snap.per_aio_charge_today_kwh[0] - 20.0).abs() < 0.01);
        assert!((snap.per_aio_discharge_today_kwh[0] - 18.5).abs() < 0.01);

        // Serial: "SA24230001"
        assert_eq!(snap.per_aio_serial[0], "SA24230001");
        assert_eq!(snap.first_inverter_serial, "SA24230001");
    }

    #[test]
    fn gateway_decode_v2_uint32_byte_order() {
        let mut b1 = [0u16; 60];
        b1[0] = 0x4741; // 'G','A'
        b1[1] = 0x3030;
        b1[2] = 0x0000;
        b1[3] = 0x000A; // 10 → V2 (GA000010)
        // V2 byte order: uint32 is (low << 16) | high, so hi_off and lo_off swap.
        // For e_grid_import_total: V2 expects IR(1642) as high, IR(1641) as low.
        // The fixture has hi=1@41, lo=0@42. gw_u32 swaps: uint32(lo=0, hi=1) = (0<<16)|1 = 1.
        b1[41] = 1; b1[42] = 0; // hi@41=1, lo@42=0 → V2 reads lo=0 as hi → (0<<16)|1 = 1

        let mut b5 = [0u16; 29];
        // V2: aio1 serial @ 1841-1845 (offsets 10-14)
        b5[10] = 0x5632; // 'V','2'  — to distinguish from V1 serial position
        b5[11] = 0x4149;
        b5[12] = 0x4F31;
        b5[13] = 0x3030;
        b5[14] = 0x3031;

        let blocks = vec![
            gateway_holding_block(),
            gateway_block1(b1),
            gateway_block2([0u16; 60]),
            gateway_block3([0u16; 60]),
            gateway_block4([0u16; 51]),
            gateway_block5_v1(b5),
        ];
        let snap = decode_snapshot(&blocks);

        assert!(snap.gateway_is_v2);
        // V2: (0<<16)|1 = 1, /10 = 0.1
        assert!((snap.total_import_kwh - 0.1).abs() < 0.01);
        // Serial should be at V2 offsets: "V2AIO10001"
        assert_eq!(snap.per_aio_serial[0], "V2AIO10001");
    }

    #[test]
    fn gateway_no_blocks_produces_default_snapshot() {
        // Empty blocks (no gateway data) should produce default values.
        let snap = decode_snapshot(&[]);
        assert!(snap.gateway_software_version.is_empty());
        assert_eq!(snap.parallel_aio_count, 0);
        assert_eq!(snap.soc, 0);
        assert_eq!(snap.battery_power, 0);
        assert_eq!(snap.grid_power, 0);
        assert_eq!(snap.home_power, 0);
    }
}
