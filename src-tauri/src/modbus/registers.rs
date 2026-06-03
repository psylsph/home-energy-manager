//! GivEnergy Modbus register address constants and block definitions.
//!
//! Register addresses sourced from the givenergy-modbus reference library
//! (SinglePhaseInverterRegisterGetter.REGISTER_LUT). Each poll cycle reads
//! aligned blocks of registers.

// ---------------------------------------------------------------------------
// Register type
// ---------------------------------------------------------------------------

/// Distinguishes Modbus input registers (read-only) from holding registers (read/write).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegisterType {
    /// Input register – read-only telemetry data.
    Input,
    /// Holding register – read/write configuration.
    Holding,
}

// ---------------------------------------------------------------------------
// Register block descriptor
// ---------------------------------------------------------------------------

/// A contiguous range of registers to read in a single Modbus request.
#[derive(Debug, Clone, Copy)]
pub struct RegisterBlock {
    /// Starting register address.
    pub start: u16,
    /// Number of consecutive registers to read.
    pub count: u16,
    /// Whether this is an input or holding register block.
    pub register_type: RegisterType,
    /// Human-readable block name (used in logging / diagnostics).
    pub name: &'static str,
}

// ---------------------------------------------------------------------------
// Standard poll blocks
// ---------------------------------------------------------------------------

/// Blocks read during every poll cycle on all inverter generations.
pub const STANDARD_POLL_BLOCKS: &[RegisterBlock] = &[
    RegisterBlock {
        start: 0,
        count: 60,
        register_type: RegisterType::Input,
        name: "input_0_59",
    },
    RegisterBlock {
        start: 0,
        count: 60,
        register_type: RegisterType::Holding,
        name: "holding_0_59",
    },
    RegisterBlock {
        start: 60,
        count: 60,
        register_type: RegisterType::Holding,
        name: "holding_60_119",
    },
];

// ===========================================================================
// Input Register addresses (read-only telemetry)
// ===========================================================================
//
// Block: Input Registers 0-59
// -----------------------------------------------

/// Inverter status: 0=waiting, 1=normal, 2=warning, 3=fault.
pub const IR_STATUS: u16 = 0;
/// PV1 voltage in 0.1 V units.
pub const IR_PV1_VOLTAGE: u16 = 1;
/// PV2 voltage in 0.1 V units.
pub const IR_PV2_VOLTAGE: u16 = 2;
/// AC grid voltage in 0.1 V units.
pub const IR_GRID_VOLTAGE: u16 = 5;
/// PV1 current in 0.1 A units.
pub const IR_PV1_CURRENT: u16 = 8;
/// PV2 current in 0.1 A units.
pub const IR_PV2_CURRENT: u16 = 9;
/// AC frequency in 0.01 Hz units.
pub const IR_GRID_FREQUENCY: u16 = 13;
/// PV1 energy generated today in 0.1 kWh units.
pub const IR_PV1_ENERGY_TODAY: u16 = 17;
/// PV1 power in watts.
pub const IR_PV1_POWER: u16 = 18;
/// PV2 energy generated today in 0.1 kWh units.
pub const IR_PV2_ENERGY_TODAY: u16 = 19;
/// PV2 power in watts.
pub const IR_PV2_POWER: u16 = 20;
/// Energy exported to grid today in 0.1 kWh units.
pub const IR_TODAY_EXPORT_ENERGY: u16 = 25;
/// Energy imported from grid today in 0.1 kWh units.
pub const IR_TODAY_IMPORT_ENERGY: u16 = 26;
/// Grid power in watts, signed (positive = exporting, negative = importing).
pub const IR_GRID_POWER: u16 = 30;
/// Household consumption today in 0.1 kWh units.
pub const IR_TODAY_CONSUMPTION: u16 = 35;
/// Battery charge energy today in 0.1 kWh units.
pub const IR_TODAY_CHARGE_ENERGY: u16 = 36;
/// Battery discharge energy today in 0.1 kWh units.
pub const IR_TODAY_DISCHARGE_ENERGY: u16 = 37;
/// Inverter heatsink temperature in 0.1 °C units.
pub const IR_INVERTER_TEMPERATURE: u16 = 41;
/// Battery voltage in 0.01 V units.
pub const IR_BATTERY_VOLTAGE: u16 = 50;
/// Battery current in 0.01 A units, signed.
pub const IR_BATTERY_CURRENT: u16 = 51;
/// Battery power in watts, signed (positive = charging).
pub const IR_BATTERY_POWER: u16 = 52;
/// Battery temperature in 0.1 °C units.
pub const IR_BATTERY_TEMPERATURE: u16 = 56;
/// Battery state-of-charge percentage (0-100).
pub const IR_BATTERY_SOC: u16 = 59;

// ===========================================================================
// Holding Register addresses (read/write configuration)
// ===========================================================================
//
// Block: Holding Registers 0-59
// -----------------------------------------------

/// Device type code (hex, e.g. 0x2001 = Gen3 Hybrid).
pub const HR_DEVICE_TYPE: u16 = 0;
/// Serial number encoded as 5 registers of latin1 chars.
pub const HR_SERIAL_NUMBER_START: u16 = 13;
/// Enable charge target (bool).
pub const HR_ENABLE_CHARGE_TARGET: u16 = 20;
/// ARM firmware version.
pub const HR_ARM_FIRMWARE: u16 = 21;
/// Battery power mode: 0 = export, 1 = self-consumption (eco).
pub const HR_BATTERY_POWER_MODE: u16 = 27;
/// Inverter max output active power rate percentage (0-100).
pub const HR_ACTIVE_POWER_RATE: u16 = 50;
/// Charge slot 2: start time as HHMM, end time as HHMM (2 registers).
pub const HR_CHARGE_SLOT_2_START: u16 = 31;
pub const HR_CHARGE_SLOT_2_END: u16 = 32;
/// Discharge slot 2: start/end as HHMM (2 registers).
pub const HR_DISCHARGE_SLOT_2_START: u16 = 44;
pub const HR_DISCHARGE_SLOT_2_END: u16 = 45;
/// Discharge slot 1: start/end as HHMM (2 registers).
pub const HR_DISCHARGE_SLOT_1_START: u16 = 56;
pub const HR_DISCHARGE_SLOT_1_END: u16 = 57;
/// Enable discharge (bool).
pub const HR_ENABLE_DISCHARGE: u16 = 59;

// Block: Holding Registers 60-119
// -----------------------------------------------

/// Charge slot 1: start/end as HHMM (2 registers).
pub const HR_CHARGE_SLOT_1_START: u16 = 94;
pub const HR_CHARGE_SLOT_1_END: u16 = 95;
/// Enable charge (bool).
pub const HR_ENABLE_CHARGE: u16 = 96;
/// Battery SOC reserve percentage (0-100).
pub const HR_BATTERY_SOC_RESERVE: u16 = 110;
/// Battery charge power limit as percentage (0-100, practical max ~50%).
pub const HR_BATTERY_CHARGE_LIMIT: u16 = 111;
/// Battery discharge power limit as percentage (0-100, practical max ~50%).
pub const HR_BATTERY_DISCHARGE_LIMIT: u16 = 112;
/// Charge target SOC percentage (0-100, requires enable_charge_target).
pub const HR_CHARGE_TARGET_SOC: u16 = 116;

// Gen3 per-slot target SOC registers (HR 242-269):
// Each charge slot has its own target SOC, distinct from the global HR 116.
pub const HR_CHARGE_TARGET_SOC_1: u16 = 242;
pub const HR_CHARGE_TARGET_SOC_2: u16 = 245;

// Gen3 discharge per-slot target SOC registers (HR 272-299):
pub const HR_DISCHARGE_TARGET_SOC_1: u16 = 272;
pub const HR_DISCHARGE_TARGET_SOC_2: u16 = 275;

// Block: Holding Registers 300-359 (pause mode)
pub const HR_BATTERY_PAUSE_MODE: u16 = 318;
pub const HR_BATTERY_PAUSE_SLOT_1_START: u16 = 319;
pub const HR_BATTERY_PAUSE_SLOT_1_END: u16 = 320;

/// Battery calibration stage (0=off, 5=balance).
pub const HR_BATTERY_CALIBRATION_STAGE: u16 = 29;

/// Inverter reboot (write 100 to reboot).
pub const HR_INVERTER_REBOOT: u16 = 163;

/// Enable Real Time Clock — persist settings to EEPROM (bool).
pub const HR_ENABLE_RTC: u16 = 166;

// ===========================================================================
// AC-coupled configuration (HR 300-359)
// ===========================================================================

/// Export priority for AC-coupled inverters (0=battery, 1=grid, 2=load).
pub const HR_EXPORT_PRIORITY: u16 = 311;
/// Enable EPS (Emergency Power Supply) mode (bool).
pub const HR_ENABLE_EPS: u16 = 317;

/// Battery discharge min power reserve percentage (4-100).
/// Distinct from HR_BATTERY_SOC_RESERVE (110) — this prevents discharge
/// below the reserve level even in timed modes.
pub const HR_BATTERY_DISCHARGE_MIN_POWER_RESERVE: u16 = 114;

// ===========================================================================
// Extended charge slots 3-10 (HR 246-268) — Gen3, AIO, HV Gen3
// ===========================================================================

pub const HR_CHARGE_SLOT_3_START: u16 = 246;
pub const HR_CHARGE_SLOT_3_END: u16 = 247;
pub const HR_CHARGE_TARGET_SOC_3: u16 = 248;
pub const HR_CHARGE_SLOT_4_START: u16 = 249;
pub const HR_CHARGE_SLOT_4_END: u16 = 250;
pub const HR_CHARGE_TARGET_SOC_4: u16 = 251;
pub const HR_CHARGE_SLOT_5_START: u16 = 252;
pub const HR_CHARGE_SLOT_5_END: u16 = 253;
pub const HR_CHARGE_TARGET_SOC_5: u16 = 254;
pub const HR_CHARGE_SLOT_6_START: u16 = 255;
pub const HR_CHARGE_SLOT_6_END: u16 = 256;
pub const HR_CHARGE_TARGET_SOC_6: u16 = 257;
pub const HR_CHARGE_SLOT_7_START: u16 = 258;
pub const HR_CHARGE_SLOT_7_END: u16 = 259;
pub const HR_CHARGE_TARGET_SOC_7: u16 = 260;
pub const HR_CHARGE_SLOT_8_START: u16 = 261;
pub const HR_CHARGE_SLOT_8_END: u16 = 262;
pub const HR_CHARGE_TARGET_SOC_8: u16 = 263;
pub const HR_CHARGE_SLOT_9_START: u16 = 264;
pub const HR_CHARGE_SLOT_9_END: u16 = 265;
pub const HR_CHARGE_TARGET_SOC_9: u16 = 266;
pub const HR_CHARGE_SLOT_10_START: u16 = 267;
pub const HR_CHARGE_SLOT_10_END: u16 = 268;
pub const HR_CHARGE_TARGET_SOC_10: u16 = 269;

// ===========================================================================
// Extended discharge slots 3-10 (HR 276-298) — Gen3, AIO, HV Gen3
// ===========================================================================

pub const HR_DISCHARGE_SLOT_3_START: u16 = 276;
pub const HR_DISCHARGE_SLOT_3_END: u16 = 277;
pub const HR_DISCHARGE_TARGET_SOC_3: u16 = 278;
pub const HR_DISCHARGE_SLOT_4_START: u16 = 279;
pub const HR_DISCHARGE_SLOT_4_END: u16 = 280;
pub const HR_DISCHARGE_TARGET_SOC_4: u16 = 281;
pub const HR_DISCHARGE_SLOT_5_START: u16 = 282;
pub const HR_DISCHARGE_SLOT_5_END: u16 = 283;
pub const HR_DISCHARGE_TARGET_SOC_5: u16 = 284;
pub const HR_DISCHARGE_SLOT_6_START: u16 = 285;
pub const HR_DISCHARGE_SLOT_6_END: u16 = 286;
pub const HR_DISCHARGE_TARGET_SOC_6: u16 = 287;
pub const HR_DISCHARGE_SLOT_7_START: u16 = 288;
pub const HR_DISCHARGE_SLOT_7_END: u16 = 289;
pub const HR_DISCHARGE_TARGET_SOC_7: u16 = 290;
pub const HR_DISCHARGE_SLOT_8_START: u16 = 291;
pub const HR_DISCHARGE_SLOT_8_END: u16 = 292;
pub const HR_DISCHARGE_TARGET_SOC_8: u16 = 293;
pub const HR_DISCHARGE_SLOT_9_START: u16 = 294;
pub const HR_DISCHARGE_SLOT_9_END: u16 = 295;
pub const HR_DISCHARGE_TARGET_SOC_9: u16 = 296;
pub const HR_DISCHARGE_SLOT_10_START: u16 = 297;
pub const HR_DISCHARGE_SLOT_10_END: u16 = 298;
pub const HR_DISCHARGE_TARGET_SOC_10: u16 = 299;

// ===========================================================================
// System time registers (read/write for clock sync)
// ===========================================================================
//
// Block: Holding Registers 35-40
// -----------------------------------------------

/// System time: year.
pub const HR_SYSTEM_TIME_YEAR: u16 = 35;
/// System time: month (1-12).
pub const HR_SYSTEM_TIME_MONTH: u16 = 36;
/// System time: day (1-31).
pub const HR_SYSTEM_TIME_DAY: u16 = 37;
/// System time: hour (0-23).
pub const HR_SYSTEM_TIME_HOUR: u16 = 38;
/// System time: minute (0-59).
pub const HR_SYSTEM_TIME_MINUTE: u16 = 39;
/// System time: second (0-59).
pub const HR_SYSTEM_TIME_SECOND: u16 = 40;

// ===========================================================================
// Battery module polling (LV batteries)
// ===========================================================================
//
// Per givenergy-modbus reference:
//   Battery #1 shares the inverter's input-register bank at device address 0x32.
//   Its BMS data (IR 60-119) is already captured by the standard poll block
//   `input_0_59` followed by a separate IR 60-119 read on device 0x32.
//   Additional batteries sit at device addresses 0x33, 0x34, … 0x37.
//
// LV Battery IR 60-119 layout (per givenergy-modbus BatteryRegisterGetter):
//   IR(60-75):   cell voltages (milli-V, 16 cells)
//   IR(76-79):   cell temperatures (deci-°C, groups of 4 cells)
//   IR(80):      v_cells_sum (milli-V)
//   IR(81):      t_bms_mosfet (deci-°C)
//   IR(82-83):   v_out (uint32 milli-V)
//   IR(84-85):   cap_calibrated (uint32 centi-Ah)
//   IR(86-87):   cap_design (uint32 centi-Ah)
//   IR(88-89):   cap_remaining (uint32 centi-Ah)
//   IR(90-94):   status/warning packed bytes
//   IR(96):      num_cycles
//   IR(97):      num_cells
//   IR(98):      bms_firmware_version
//   IR(100):     soc (%)
//   IR(101-102): cap_design2 (uint32 centi-Ah)
//   IR(103):     t_max (deci-°C)
//   IR(104):     t_min (deci-°C)
//   IR(110-114): serial_number (5 registers = 10 Latin-1 chars)

/// Device addresses for additional LV batteries.
///
/// Battery #1 lives at 0x32 (same as the inverter) and its BMS registers
/// (IR 60-119) are already read as part of the standard poll. Addresses 0x33+
/// are for multi-battery installations with a second (or third, etc.) battery.
pub const LV_BATTERY_ADDRESSES: &[u8] = &[0x33, 0x34, 0x35, 0x36, 0x37];

/// Block read for each additional LV battery BMS.
pub const BATTERY_POLL_BLOCK: RegisterBlock = RegisterBlock {
    start: 60,
    count: 60,
    register_type: RegisterType::Input,
    name: "battery_input_60_119",
};

/// Block read for the first battery's BMS data on the inverter device (0x32).
/// This is the same IR 60-119 block but on the inverter's device address.
pub const BATTERY_1_POLL_BLOCK: RegisterBlock = RegisterBlock {
    start: 60,
    count: 60,
    register_type: RegisterType::Input,
    name: "battery1_input_60_119",
};

// ---------------------------------------------------------------------------
// Write whitelist — registers that are safe to write to
// ---------------------------------------------------------------------------

/// Holding register addresses that the control encoder is allowed to write.
/// Sourced from the givenergy-modbus reference library's WRITE_SAFE_REGISTERS.
pub const SAFE_WRITE_REGS: &[u16] = &[
    20, 27, 29, 31, 32, 35, 36, 37, 38, 39, 40, 44, 45, 50, 56, 57, 59, 94, 95, 96, 110, 111, 112,
    114, 116, 163, 166,
    // Charge slots 3-10 (Gen3 extended)
    246, 247, 248, 249, 250, 251, 252, 253, 254, 255, 256, 257, 258, 259, 260, 261, 262, 263, 264, 265, 266, 267, 268, 269,
    // Discharge slots 3-10 (Gen3 extended)
    276, 277, 278, 279, 280, 281, 282, 283, 284, 285, 286, 287, 288, 289, 290, 291, 292, 293, 294, 295, 296, 297, 298, 299,
    // Per-slot charge targets (Gen3)
    242, 245,
    // Per-slot discharge targets (Gen3)
    272, 275,
    // AC-coupled features
    311, 317,
    // Pause mode/slot
    318, 319, 320,
];

// ===========================================================================
// Model-aware poll blocks (beyond the standard set)
// ===========================================================================

/// Extended charge/discharge slots + per-slot targets for Gen3 / AIO / HV Gen3.
/// HR 240-299 covers 10-slot scheduling and per-slot target SOCs.
pub const EXTENDED_SLOTS_BLOCK: RegisterBlock = RegisterBlock {
    start: 240,
    count: 60,
    register_type: RegisterType::Holding,
    name: "holding_240_299",
};

/// AC configuration block — export priority, EPS, pause mode, AC limits.
pub const AC_CONFIG_BLOCK: RegisterBlock = RegisterBlock {
    start: 300,
    count: 60,
    register_type: RegisterType::Holding,
    name: "holding_300_359",
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Decode a packed HHMM time value into (hour, minute).
/// Returns None if the value represents an empty/disabled slot.
///
/// The reference library uses 60 as the disabled sentinel (the minute
/// component would be 60 which is invalid). All other values are valid:
///   0   = 00:00 (midnight)
///   30  = 00:30
///   100 = 01:00
///   630 = 06:30
pub fn decode_hhmm(val: u16) -> Option<(u8, u8)> {
    // 60 is the disabled sentinel per givenergy-modbus reference
    if val == 60 {
        return None;
    }
    let hour = (val / 100) as u8;
    let minute = (val % 100) as u8;
    // Guard against minute > 59 (shouldn't happen except 60 sentinel above)
    if minute > 59 {
        return None;
    }
    Some((hour.min(23), minute))
}

/// Encode (hour, minute) into a packed HHMM value.
pub fn encode_hhmm(hour: u8, minute: u8) -> u16 {
    (hour as u16) * 100 + (minute as u16)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_blocks_cover_needed_ranges() {
        assert_eq!(STANDARD_POLL_BLOCKS.len(), 3);
        // Input 0-59 covers all telemetry (IR 0-59)
        assert_eq!(STANDARD_POLL_BLOCKS[0].start, 0);
        assert_eq!(STANDARD_POLL_BLOCKS[0].count, 60);
        // Holding 0-59
        assert_eq!(STANDARD_POLL_BLOCKS[1].start, 0);
        assert_eq!(STANDARD_POLL_BLOCKS[1].count, 60);
        // Holding 60-119 covers charge_slot_1 (94-95), soc_reserve (110), limits (111-112)
        assert_eq!(STANDARD_POLL_BLOCKS[2].start, 60);
        assert_eq!(STANDARD_POLL_BLOCKS[2].count, 60);
    }

    #[test]
    fn decode_hhmm_valid() {
        assert_eq!(decode_hhmm(1600), Some((16, 0)));
        assert_eq!(decode_hhmm(630), Some((6, 30)));
        assert_eq!(decode_hhmm(2359), Some((23, 59)));
        assert_eq!(decode_hhmm(0), Some((0, 0)));
        assert_eq!(decode_hhmm(1), Some((0, 1)));
        assert_eq!(decode_hhmm(100), Some((1, 0)));
    }

    #[test]
    fn decode_hhmm_disabled() {
        // 60 is the disabled sentinel per givenergy-modbus reference
        // 0 is valid 00:00 (disabled only when both start AND end are 0)
        assert_eq!(decode_hhmm(60), None);
    }

    #[test]
    fn decode_hhmm_small_values_valid() {
        // Values 1-59 are valid times (e.g. 30 = 00:30, 1 = 00:01)
        assert_eq!(decode_hhmm(30), Some((0, 30)));
        assert_eq!(decode_hhmm(1), Some((0, 1)));
        assert_eq!(decode_hhmm(59), Some((0, 59)));
    }

    #[test]
    fn encode_hhmm_roundtrip() {
        for (h, m) in [(0, 0), (0, 1), (6, 30), (16, 0), (23, 59)] {
            let encoded = encode_hhmm(h, m);
            let decoded = decode_hhmm(encoded);
            assert_eq!(decoded, Some((h, m)));
        }
    }

    #[test]
    fn safe_write_regs_contains_key_addresses() {
        assert!(SAFE_WRITE_REGS.contains(&HR_BATTERY_POWER_MODE)); // 27
        assert!(SAFE_WRITE_REGS.contains(&HR_BATTERY_CALIBRATION_STAGE)); // 29
        assert!(SAFE_WRITE_REGS.contains(&HR_ACTIVE_POWER_RATE)); // 50
        assert!(SAFE_WRITE_REGS.contains(&HR_ENABLE_DISCHARGE)); // 59
        assert!(SAFE_WRITE_REGS.contains(&HR_CHARGE_SLOT_1_START)); // 94
        assert!(SAFE_WRITE_REGS.contains(&HR_BATTERY_SOC_RESERVE)); // 110
        assert!(SAFE_WRITE_REGS.contains(&HR_BATTERY_DISCHARGE_MIN_POWER_RESERVE)); // 114
        assert!(SAFE_WRITE_REGS.contains(&HR_CHARGE_TARGET_SOC)); // 116
        assert!(SAFE_WRITE_REGS.contains(&HR_INVERTER_REBOOT)); // 163
        assert!(SAFE_WRITE_REGS.contains(&HR_ENABLE_RTC)); // 166
        assert!(SAFE_WRITE_REGS.contains(&HR_SYSTEM_TIME_YEAR)); // 35
        assert!(SAFE_WRITE_REGS.contains(&HR_SYSTEM_TIME_MONTH)); // 36
        assert!(SAFE_WRITE_REGS.contains(&HR_SYSTEM_TIME_DAY)); // 37
        assert!(SAFE_WRITE_REGS.contains(&HR_SYSTEM_TIME_HOUR)); // 38
        assert!(SAFE_WRITE_REGS.contains(&HR_SYSTEM_TIME_MINUTE)); // 39
        assert!(SAFE_WRITE_REGS.contains(&HR_SYSTEM_TIME_SECOND)); // 40
        assert!(SAFE_WRITE_REGS.contains(&HR_EXPORT_PRIORITY)); // 311
        assert!(SAFE_WRITE_REGS.contains(&HR_ENABLE_EPS)); // 317
        assert!(SAFE_WRITE_REGS.contains(&HR_BATTERY_PAUSE_MODE)); // 318
        assert!(SAFE_WRITE_REGS.contains(&HR_BATTERY_PAUSE_SLOT_1_START)); // 319
        assert!(SAFE_WRITE_REGS.contains(&HR_BATTERY_PAUSE_SLOT_1_END)); // 320
        assert!(SAFE_WRITE_REGS.contains(&HR_CHARGE_TARGET_SOC_1)); // 242
        assert!(SAFE_WRITE_REGS.contains(&HR_CHARGE_TARGET_SOC_2)); // 245
        assert!(SAFE_WRITE_REGS.contains(&HR_DISCHARGE_TARGET_SOC_1)); // 272
        assert!(SAFE_WRITE_REGS.contains(&HR_DISCHARGE_TARGET_SOC_2)); // 275
        // Extended slots 3-10
        assert!(SAFE_WRITE_REGS.contains(&HR_CHARGE_SLOT_3_START)); // 246
        assert!(SAFE_WRITE_REGS.contains(&HR_CHARGE_SLOT_10_END)); // 268
        assert!(SAFE_WRITE_REGS.contains(&HR_DISCHARGE_SLOT_3_START)); // 276
        assert!(SAFE_WRITE_REGS.contains(&HR_DISCHARGE_SLOT_10_END)); // 298
        // Per-slot target SOCs
        assert!(SAFE_WRITE_REGS.contains(&HR_CHARGE_TARGET_SOC_3)); // 248
        assert!(SAFE_WRITE_REGS.contains(&HR_DISCHARGE_TARGET_SOC_3)); // 278
    }

    #[test]
    fn register_addresses_match_reference() {
        // Input registers - spot checks against givenergy-modbus reference
        assert_eq!(IR_PV1_POWER, 18);
        assert_eq!(IR_PV2_POWER, 20);
        assert_eq!(IR_BATTERY_POWER, 52);
        assert_eq!(IR_BATTERY_SOC, 59);
        assert_eq!(IR_GRID_POWER, 30);
        assert_eq!(IR_GRID_VOLTAGE, 5);
        assert_eq!(IR_BATTERY_VOLTAGE, 50);
        assert_eq!(IR_BATTERY_CURRENT, 51);
        assert_eq!(IR_INVERTER_TEMPERATURE, 41);
        assert_eq!(IR_BATTERY_TEMPERATURE, 56);

        // Holding registers
        assert_eq!(HR_BATTERY_POWER_MODE, 27);
        assert_eq!(HR_ENABLE_DISCHARGE, 59);
        assert_eq!(HR_ENABLE_CHARGE, 96);
        assert_eq!(HR_BATTERY_SOC_RESERVE, 110);
        assert_eq!(HR_CHARGE_SLOT_1_START, 94);
        assert_eq!(HR_DISCHARGE_SLOT_1_START, 56);
    }
}
