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
/// Battery charge power limit as percentage (0-50).
pub const HR_BATTERY_CHARGE_LIMIT: u16 = 111;
/// Battery discharge power limit as percentage (0-50).
pub const HR_BATTERY_DISCHARGE_LIMIT: u16 = 112;
/// Charge target SOC percentage (0-100, requires enable_charge_target).
pub const HR_CHARGE_TARGET_SOC: u16 = 116;

// Block: Holding Registers 300-359 (pause mode)
pub const HR_BATTERY_PAUSE_MODE: u16 = 318;
pub const HR_BATTERY_PAUSE_SLOT_1_START: u16 = 319;
pub const HR_BATTERY_PAUSE_SLOT_1_END: u16 = 320;

// ---------------------------------------------------------------------------
// Write whitelist — registers that are safe to write to
// ---------------------------------------------------------------------------

/// Holding register addresses that the control encoder is allowed to write.
/// Sourced from GivTCP safe_regs.
pub const SAFE_WRITE_REGS: &[u16] = &[
    20, 27, 31, 32, 44, 45, 50, 56, 57, 59, 94, 95, 96, 110, 111, 112, 116, 318, 319, 320,
];

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
    }

    #[test]
    fn decode_hhmm_disabled() {
        // 60 is the disabled sentinel
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
        for (h, m) in [(0, 0), (6, 30), (16, 0), (23, 59)] {
            let encoded = encode_hhmm(h, m);
            let decoded = decode_hhmm(encoded);
            assert_eq!(decoded, Some((h, m)));
        }
    }

    #[test]
    fn safe_write_regs_contains_key_addresses() {
        assert!(SAFE_WRITE_REGS.contains(&HR_BATTERY_POWER_MODE)); // 27
        assert!(SAFE_WRITE_REGS.contains(&HR_ENABLE_DISCHARGE)); // 59
        assert!(SAFE_WRITE_REGS.contains(&HR_CHARGE_SLOT_1_START)); // 94
        assert!(SAFE_WRITE_REGS.contains(&HR_BATTERY_SOC_RESERVE)); // 110
        assert!(SAFE_WRITE_REGS.contains(&HR_CHARGE_TARGET_SOC)); // 116
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
