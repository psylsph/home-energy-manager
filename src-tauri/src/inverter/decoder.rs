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
            target_soc: 0,
        },
        _ => ScheduleSlot::default(),
    }
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
            "holding_1080_1124" => decode_holding_1080_1124(data, &mut snap, &mut raw),
            "input_1000_1059" => decode_input_1000_1059(data, &mut snap),
            "input_1060_1119" => decode_input_1060_1119(data, &mut snap),
            "input_1120_1179" => decode_input_1120_1179(data, &mut snap),
            "input_1180_1239" => decode_input_1180_1239(data, &mut snap),
            "input_1240_1299" => decode_input_1240_1299(data, &mut snap),
            "input_1300_1359" => decode_input_1300_1359(data, &mut snap),
            "input_1360_1413" => decode_input_1360_1413(data, &mut snap),
            "input_180_239" => decode_input_180_239(data, &mut snap),
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
    if !(snap.device_type.needs_three_phase_input_blocks() && snap.home_power > 0) {
        snap.home_power = snap.solar_power - snap.battery_power - snap.grid_power;
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
    // master enable_charge flag. This prevents toggling schedules off/on from
    // clearing configured times: HR 96 is the master enable flag, while the
    // slot registers retain the configured windows.
    //
    // Discharge slots are intentionally NOT gated on enable_discharge here.
    // That flag is the master "timed discharge" switch, controlled by the
    // battery mode (Timed Demand/Export) — not by individual slot writes.
    // A discharge slot's enabled state therefore reflects whether it has
    // configured times (decode_timeslot), so users can set up discharge
    // slots while in Eco mode without forcing an immediate mode switch.
    // The schedule only becomes active when the user selects Timed Demand.
    if !snap.device_type.uses_three_phase_schedule_slots()
        && !snap.enable_charge
    {
        for slot in &mut snap.charge_slots {
            slot.enabled = false;
        }
    }

    snap
}

// ---------------------------------------------------------------------------
// Per-block decoders
// ---------------------------------------------------------------------------

/// Decode input registers 0-59 (telemetry).
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
    snap.today_consumption_kwh = (snap.today_solar_kwh
        + snap.today_import_kwh
        - snap.today_export_kwh
        - snap.today_ac_charge_kwh)
        .max(0.0);
    snap.total_export_kwh = uint32(get_reg(data, 21), get_reg(data, 22)) as f32 * 0.1; // IR(21-22): e_grid_out_total
    snap.total_import_kwh = uint32(get_reg(data, 32), get_reg(data, 33)) as f32 * 0.1; // IR(32-33): e_grid_in_total // keep raw IR(35) for energy balance
}

/// IR 180-239: Alternative battery energy counters.
///
/// Per givenergy-modbus reference, IR(180)/IR(181) carry alternative total
/// battery discharge/charge energy (deci-kWh). IR(182)/IR(183) carry
/// alternative today counters; IR(184-239) are currently unused.
fn decode_input_180_239(data: &[u16], snap: &mut InverterSnapshot) {
    snap.total_discharge_kwh = get_reg(data, 0) as f32 * 0.1; // IR(180): e_battery_discharge_total_alt1
    snap.total_charge_kwh = get_reg(data, 1) as f32 * 0.1; // IR(181): e_battery_charge_total_alt1
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
    snap.battery_reserve = get_reg(data, 110 - 60) as u8;
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
    snap.target_soc = get_reg(data, 116 - 60) as u8;

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
fn decode_holding_240_299(data: &[u16], snap: &mut InverterSnapshot) {
    // Extended charge slots 3-10 at HR 246-268, offset by 240 in this block
    // Pattern: start, end, target_soc repeating for each slot
    for i in 0..8u16 {
        let base = (246 + i * 3) as usize;
        let offset = base - 240;
        let start_val = get_reg(data, offset);
        let end_val = get_reg(data, offset + 1);
        let target = get_reg(data, offset + 2) as u8;

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
    let t1 = get_reg(data, 242 - 240) as u8;
    let t2 = get_reg(data, 245 - 240) as u8;
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
        let target = get_reg(data, offset + 2) as u8;

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
    let dt1 = get_reg(data, 272 - 240) as u8;
    let dt2 = get_reg(data, 275 - 240) as u8;
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
fn decode_holding_1080_1124(data: &[u16], snap: &mut InverterSnapshot, raw: &mut RawConfig) {
    snap.discharge_rate = get_reg(data, 1108 - 1080) as u8;
    snap.battery_reserve = get_reg(data, 1109 - 1080) as u8;
    raw.battery_soc_reserve = snap.battery_reserve as u16;
    snap.charge_rate = get_reg(data, 1110 - 1080) as u8;
    snap.target_soc = get_reg(data, 1111 - 1080) as u8;

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
    //   9 → IR(1069)+IR(1070): p_inverter_out (int32 /10 W)
    //  19 → IR(1079)+IR(1080): p_meter_import (uint32 /10 W)
    //  21 → IR(1081)+IR(1082): p_meter_export (uint32 /10 W)
    //  29 → IR(1089)+IR(1090): p_load_all (uint32 /10 W)
    let v1 = get_reg(data, 1) as f32 * 0.1;
    let v2 = get_reg(data, 2) as f32 * 0.1;
    let v3 = get_reg(data, 3) as f32 * 0.1;
    snap.grid_voltage = v1;
    snap.grid_frequency = get_reg(data, 7) as f32 * 0.01;

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
    snap.meters.push(MeterData {
        address: 0x00, // synthetic "built-in grid CT"
        v_phase_1: v1,
        v_phase_2: v2,
        v_phase_3: v3,
        i_phase_1: i1,
        i_phase_2: i2,
        i_phase_3: i3,
        i_total,
        p_active_phase_1: -p_grid / 3,
        p_active_phase_2: -p_grid / 3,
        p_active_phase_3: -p_grid / 3,
        p_active_total: -p_grid,
        p_reactive_total: 0,
        p_apparent_total: (i_total * (v1 + v2 + v3) / 3.0) as i32,
        pf_total: 1.0,
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
    snap.total_import_kwh = uint32(get_reg(data, 22), get_reg(data, 23)) as f32 * 0.1; // IR(1382-1383): e_import_total
    snap.total_export_kwh = uint32(get_reg(data, 26), get_reg(data, 27)) as f32 * 0.1; // IR(1386-1387): e_export_total
    snap.total_charge_kwh = uint32(get_reg(data, 34), get_reg(data, 35)) as f32 * 0.1; // IR(1394-1395): e_battery_charge_total
    snap.total_discharge_kwh = uint32(get_reg(data, 30), get_reg(data, 31)) as f32 * 0.1; // IR(1390-1391): e_battery_discharge_total
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
/// A meter is valid if phase 1 voltage is non-zero and plausible (>100V).
pub fn validate_meter_data(data: &[u16]) -> bool {
    let v1 = data.first().copied().unwrap_or(0) as f32 * 0.1;
    v1 > 100.0 && v1 < 300.0
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

    let battery_voltage = get_reg(data, 73 - 60) as f32 * 0.1;
    let battery_current = signed(get_reg(data, 76 - 60)) as f32 * 0.1;
    // IR(79) is /1000 → kW; convert to watts to match the snapshot convention.
    let battery_power_w = (get_reg(data, 79 - 60) as f32 * 0.001 * 1000.0) as i32;

    let soc_packed = get_reg(data, 80 - 60);
    let battery_soc_max = ((soc_packed >> 8) & 0xFF) as u8;
    let battery_soc_min = (soc_packed & 0xFF) as u8;
    let battery_soh = (get_reg(data, 81 - 60) & 0xFF) as u8;

    let temperature = get_reg(data, 68 - 60) as f32 * 0.1;

    let nominal_capacity_ah = get_reg(data, 98 - 60) as f32 * 0.1;
    let remaining_capacity_ah = get_reg(data, 99 - 60) as f32 * 0.1;

    HvBcuCluster {
        pack_software_version,
        number_of_modules,
        cells_per_module,
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
        capacity_ah: 0.0,    // Pack-level; comes from the BCU cluster.
        design_capacity_ah: 0.0,
        remaining_capacity_ah: 0.0,
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
    let module_soc = cluster.battery_soc_max.saturating_add(cluster.battery_soc_min) / 2;
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
        ir1060[1061 - 1060] = 2310; // grid voltage 231.0V
        ir1060[1067 - 1060] = 5000; // grid frequency 50.00Hz
        ir1060[1079 - 1060] = 0;
        ir1060[1080 - 1060] = 3000; // import 300W
        ir1060[1081 - 1060] = 0;
        ir1060[1082 - 1060] = 9000; // export 900W => grid_power +600W
        ir1060[1089 - 1060] = 0;
        ir1060[1090 - 1060] = 22_000; // load 2200W

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
        assert!((snap.grid_voltage - 231.0).abs() < 0.1);
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
        assert!((snap.meters[0].frequency - 50.0).abs() < 0.01);
        assert!((snap.meters[0].e_import_active_kwh - 88.8).abs() < 0.1, "3-phase meter import = total_import_kwh");
        assert!((snap.meters[0].e_export_active_kwh - 99.9).abs() < 0.1, "3-phase meter export = total_export_kwh");
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
        d[68 - 60] = 295; // IR(68): cluster_cell_temperature = 29.5 °C
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
        assert!((c.battery_voltage - 384.0).abs() < 0.001);
        assert!((c.battery_current - -12.5).abs() < 0.001);
        assert_eq!(c.battery_power_w, 4800);
        assert_eq!(c.battery_soc_max, 90);
        assert_eq!(c.battery_soc_min, 85);
        assert_eq!(c.battery_soh, 98);
        assert!((c.temperature - 29.5).abs() < 0.001);
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
}
