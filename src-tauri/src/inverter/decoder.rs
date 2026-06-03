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
//!   IR(35):   e_load_day — consumption today (/10 kWh)
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

use super::model::{BatteryMode, BatteryModule, BatteryState, DeviceType, InverterSnapshot, ScheduleSlot};

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
        (Some((sh, sm)), Some((eh, em))) => {
            ScheduleSlot {
                enabled: true,
                start_hour: sh,
                start_minute: sm,
                end_hour: eh,
                end_minute: em,
                target_soc: 0,
            }
        }
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
        (RegisterType::Input, 60) => "battery_input_60_119",
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
    let mut snap = InverterSnapshot { timestamp: chrono::Utc::now().timestamp(), ..Default::default() };
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
    snap.home_power = snap.solar_power - snap.battery_power - snap.grid_power;

    // Derive battery mode from the three key holding registers.
    snap.battery_mode = BatteryMode::from_registers(
        raw.battery_power_mode,
        raw.enable_discharge,
        raw.battery_soc_reserve,
    );

    // Note: we intentionally do NOT override TimedDemand/TimedExport to
    // Eco/ExportPaused when discharge slots are empty. Doing so prevents the
    // user from switching to timed mode before configuring slots — the decoder
    // would immediately override back to Eco on the next poll. The inverter
    // simply won't discharge if there are no active slots, which is correct.

    // Apply global enable flags to slot states.
    // If enable_charge is off, all charge slots show as disabled regardless
    // of their time values (which may be stale if individual register writes
    // failed). Likewise for enable_discharge.
    if !snap.enable_charge {
        for slot in &mut snap.charge_slots {
            slot.enabled = false;
        }
    }
    if !snap.enable_discharge {
        for slot in &mut snap.discharge_slots {
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
    // Only include PV2's daily energy if PV2 has panels connected (voltage > 0).
    // IR(19) can return stale or garbage data when no second PV string is present.
    let pv2_today = if snap.pv2_voltage > 0.0 {
        get_reg(data, 19) as f32
    } else {
        0.0
    };
    snap.today_solar_kwh = (get_reg(data, 17) as f32 + pv2_today) * 0.1; // IR(17)+IR(19): pv1+pv2 day
    snap.today_import_kwh = get_reg(data, 26) as f32 * 0.1; // IR(26): e_grid_in_day
    snap.today_export_kwh = get_reg(data, 25) as f32 * 0.1; // IR(25): e_grid_out_day
    snap.today_charge_kwh = get_reg(data, 36) as f32 * 0.1; // IR(36): e_battery_charge_day
    snap.today_discharge_kwh = get_reg(data, 37) as f32 * 0.1; // IR(37): e_battery_discharge_day
    snap.today_consumption_kwh = get_reg(data, 35) as f32 * 0.1; // IR(35): e_load_day
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
    // Refine device type using ARM FW (0x20XX with FW century 1-2 = Gen1)
    snap.device_type = snap.device_type.refine_with_arm_fw(arm_fw);
    snap.device_type_display = snap.device_type.display_name().to_string();
    snap.max_ac_power_w = snap.device_type.max_ac_power_w();

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
    snap.max_battery_power_w = {
        // Use the device-type-specific battery limit, then cap at
        // half battery capacity (per GivTCP formula).
        snap.device_type.max_battery_power_w()
    };
    // Cap at half battery capacity (per GivTCP formula)
    let battery_capacity_w = snap.battery_capacity_kwh * 1000.0;
    snap.max_battery_power_w = snap.max_battery_power_w.min((battery_capacity_w / 2.0) as u32);

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

    // Battery charge limit: HR(111) → index 51
    snap.charge_rate = get_reg(data, 111 - 60) as u8;

    // Battery discharge limit: HR(112) → index 52
    snap.discharge_rate = get_reg(data, 112 - 60) as u8;

    // Charge target SOC: HR(116) → index 56
    snap.target_soc = get_reg(data, 116 - 60) as u8;

    // Apply global charge_target_soc to each enabled charge slot
    let global_target = snap.target_soc;
    for slot in &mut snap.charge_slots {
        if slot.enabled {
            slot.target_soc = global_target;
        }
    }
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
    let voltage_raw =
        ((get_reg(data, 82 - 60) as u32) << 16) | (get_reg(data, 83 - 60) as u32);
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
    let cap_calibrated =
        ((get_reg(data, 84 - 60) as u32) << 16) | (get_reg(data, 85 - 60) as u32);
    let cap_design =
        ((get_reg(data, 86 - 60) as u32) << 16) | (get_reg(data, 87 - 60) as u32);
    let cap_remaining =
        ((get_reg(data, 88 - 60) as u32) << 16) | (get_reg(data, 89 - 60) as u32);

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
        input_data[25] = 30; // IR(25): export_today = 3.0 kWh
        input_data[26] = 52; // IR(26): import_today = 5.2 kWh
        input_data[30] = 100; // IR(30): grid_power = +100 W (export)
        input_data[35] = 120; // IR(35): consumption_today = 12.0 kWh
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
        assert!((snap.today_consumption_kwh - 12.0).abs() < 0.1);

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
        // Note: discharge slots have valid times but show disabled because
        // enable_discharge is false (global override).
        assert_eq!(snap.battery_mode, BatteryMode::Eco);

        // Home power: solar - battery_power - grid_power
        //           = 4000 - 800 - 100 = 3100
        assert_eq!(snap.home_power, 3100);

        // Serial number
        assert_eq!(snap.inverter_serial, "SA12345678");

        // Firmware version
        assert_eq!(snap.firmware_version, "1234");

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

        // Discharge slots: have valid times (16:00–19:00, 17:00–20:00)
        // but show as disabled because enable_discharge = false.
        assert!(!snap.discharge_slots[0].enabled);
        assert!(!snap.discharge_slots[1].enabled);
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
        holding_data[34] = 0;   // charge_slot_1 start = 0 → 00:00
        holding_data[35] = 800; // charge_slot_1 end = 800 → 08:00
        holding_data[36] = 1;   // enable_charge = 1

        let blocks = vec![
            make_block(RegisterType::Input, 0, 60, "input_0_59", vec![0; 60]),
            make_block(RegisterType::Holding, 0, 60, "holding_0_59", vec![0; 60]),
            make_block(RegisterType::Holding, 60, 60, "holding_60_119", holding_data),
        ];
        let snap = decode_snapshot(&blocks);
        assert!(snap.charge_slots[0].enabled, "00:00-08:00 should be enabled");
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
            make_block(RegisterType::Holding, 60, 60, "holding_60_119", holding_data),
        ];
        let snap = decode_snapshot(&blocks);
        assert!(!snap.charge_slots[0].enabled, "12:00-12:00 should be disabled");
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
}
