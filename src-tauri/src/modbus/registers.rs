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
    // IR(180)/IR(181) are alternative battery lifetime discharge/charge totals.
    // IR(182)/IR(183) are alternative *daily* battery discharge/charge totals
    // that are authoritative for Gen1 Hybrid inverters on firmware where the
    // primary IR(36)/IR(37) registers read 0. The decoder routes by device type
    // (see `decode_input_180_181` in `decoder.rs`). Reading 4 registers costs
    // one extra Modbus frame per single-phase poll cycle.
    RegisterBlock {
        start: 180,
        count: 4,
        register_type: RegisterType::Input,
        name: "input_180_181",
    },
];

/// Lean standard blocks for three-phase models.
///
/// Three-phase inverters read all real-time telemetry (PV, grid, battery,
/// daily/lifetime energy totals) from the IR(1000-1414) range, which
/// completely supersedes the single-phase `input_0_59` and `input_180_181`
/// blocks. Reading those two blocks on every cycle wastes ~300 ms of
/// inter-request delay and adds two opportunities for a timeout to kill the
/// entire poll.
///
/// On the first poll (before model detection) the full `STANDARD_POLL_BLOCKS`
/// set is used. Once a three-phase device type is confirmed, the poll loop
/// switches to this leaner set.
pub const STANDARD_POLL_BLOCKS_3PH: &[RegisterBlock] = &[
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
/// AC charge energy today in 0.1 kWh units (e_ac_charge_today, NOT house consumption).
pub const IR_TODAY_AC_CHARGE: u16 = 35;
/// Battery charge energy today in 0.1 kWh units.
pub const IR_TODAY_CHARGE_ENERGY: u16 = 36;
/// Battery discharge energy today in 0.1 kWh units.
pub const IR_TODAY_DISCHARGE_ENERGY: u16 = 37;
/// Inverter heatsink temperature in 0.1 °C units.
pub const IR_INVERTER_TEMPERATURE: u16 = 41;
/// Battery voltage in 0.01 V units.
pub const IR_BATTERY_VOLTAGE: u16 = 50;
/// Battery current in 0.01 A units, signed
/// (positive = discharging, negative = charging) — matches the raw wire
/// convention used by givenergy-modbus and GivTCP.
pub const IR_BATTERY_CURRENT: u16 = 51;
/// Battery power in watts, signed (positive = discharging, negative = charging)
/// — matches the raw wire convention used by givenergy-modbus and GivTCP.
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
/// Battery self-heating enable (bool). Hardware/batch-gated.
pub const HR_BATTERY_SELF_HEATING: u16 = 104;
/// Manual battery heater enable (bool). Likely hardware-gated.
pub const HR_MANUAL_BATTERY_HEATER: u16 = 172;
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
/// Battery SOC reserve percentage (4-100).
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

// Gen3 charge slot 2 — extended-block copy (HR 243-244).
// On Gen3/AIO/HV-Gen3 firmware, GivTCP's model map makes this the later
// charge_slot_2 definition and its command RegisterMap resolves slot 2 writes
// here. The current givenergy-modbus SlotMap still writes slot 2 to HR 31-32
// while decoding this extended copy, so keep this explicit until upstream
// reference behavior converges.
pub const HR_CHARGE_SLOT_2_GEN3_START: u16 = 243;
pub const HR_CHARGE_SLOT_2_GEN3_END: u16 = 244;

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
/// AC-coupled battery charge power limit percentage (1-100).
pub const HR_AC_BATTERY_CHARGE_LIMIT: u16 = 313;
/// AC-coupled battery discharge power limit percentage (1-100).
pub const HR_AC_BATTERY_DISCHARGE_LIMIT: u16 = 314;
/// Enable EPS (Emergency Power Supply) mode (bool).
pub const HR_ENABLE_EPS: u16 = 317;

/// Battery discharge min power reserve percentage (4-100).
/// Distinct from HR_BATTERY_SOC_RESERVE (110) — this prevents discharge
/// below the reserve level even in timed modes.
pub const HR_BATTERY_DISCHARGE_MIN_POWER_RESERVE: u16 = 114;

/// Export power limit (W) — read from all single-phase / AC-coupled models.
/// GivTCP `baseinverter.py:55` defines `grid_port_max_power_output` as HR(26).
pub const HR_EXPORT_LIMIT: u16 = 26;

/// Three-phase export power limit (deci-W, /10 → W) — read/write on three-phase / HV models.
/// givenergy-modbus `threephase.py:91` maps `p_export_limit` → HR(1063).
/// Distinct from single-phase HR(26) which uses raw uint16 W.
pub const HR_3PH_EXPORT_LIMIT: u16 = 1063;

/// Export power limit for EMS/Gateway plant-level control (W).
/// GivTCP `commands.py:215` — HR 2071 in the EMS plant-level block (2040-2071).
/// Only present on EMS/Gateway hardware; writing on non-EMS models silently fails.
pub const HR_EMS_EXPORT_POWER_LIMIT: u16 = 2071;

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
// Three-phase battery/control registers (HR 1080-1124)
// ===========================================================================

/// Three-phase battery discharge power limit percentage (HR 1108).
pub const HR_3PH_BATTERY_DISCHARGE_LIMIT: u16 = 1108;
/// Three-phase battery SOC reserve / discharge floor percentage (HR 1109).
pub const HR_3PH_BATTERY_SOC_RESERVE: u16 = 1109;
/// Three-phase battery charge power limit percentage (HR 1110).
pub const HR_3PH_BATTERY_CHARGE_LIMIT: u16 = 1110;
/// Three-phase charge target SOC percentage (HR 1111).
pub const HR_3PH_CHARGE_TARGET_SOC: u16 = 1111;
/// Three-phase AC charge enable (HR 1112).
pub const HR_3PH_AC_CHARGE_ENABLE: u16 = 1112;
/// Three-phase charge slot 1 start/end (HR 1113-1114).
pub const HR_3PH_CHARGE_SLOT_1_START: u16 = 1113;
pub const HR_3PH_CHARGE_SLOT_1_END: u16 = 1114;
/// Three-phase charge slot 2 start/end (HR 1115-1116).
pub const HR_3PH_CHARGE_SLOT_2_START: u16 = 1115;
pub const HR_3PH_CHARGE_SLOT_2_END: u16 = 1116;
/// Three-phase discharge slot 1 start/end (HR 1118-1119).
pub const HR_3PH_DISCHARGE_SLOT_1_START: u16 = 1118;
pub const HR_3PH_DISCHARGE_SLOT_1_END: u16 = 1119;
/// Three-phase discharge slot 2 start/end (HR 1120-1121).
pub const HR_3PH_DISCHARGE_SLOT_2_START: u16 = 1120;
pub const HR_3PH_DISCHARGE_SLOT_2_END: u16 = 1121;
/// Three-phase force discharge enable (HR 1122).
pub const HR_3PH_FORCE_DISCHARGE_ENABLE: u16 = 1122;
/// Three-phase force charge enable (HR 1123).
pub const HR_3PH_FORCE_CHARGE_ENABLE: u16 = 1123;

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

// ===========================================================================
// HV battery BCU/BMU polling (HV stackable batteries)
// ===========================================================================
//
// Per givenergy-modbus reference (model/hv_bcu.py) and GivTCP: HV stackable
// batteries (e.g. GIV-BAT-3.4-HV modules in a GIV-BAT-17.0-HV stack) do NOT
// answer at the LV battery address 0x32. They use a separate cluster protocol:
//
//   BMS aggregation:  0xA0 — IR(61) holds the number of BCUs present
//   BCU (per stack):  0x70–0x8F — cluster-level data, IR 60-119
//   BMU (per module): 0x50–0x6F — per-cell data, IR 60-119 (stride 120 per BCU)
//
// BCU cluster input registers (IR 60-119) layout per the reference:
//   IR(60-63):   pack_software_version (validity fingerprint, e.g. "GA000005")
//   IR(64):      number_of_modules (uint16)
//   IR(65):      cells_per_module (uint16)
//   IR(67):      cluster_cell_voltage (uint16, mV — max cell in cluster)
//   IR(68):      cluster_cell_temperature (uint16, 0.1 °C — max cell in cluster)
//   IR(70):      status (uint16)
//   IR(73):      battery_voltage (/10 V) — pack terminal voltage
//   IR(74):      load_voltage (/10 V)
//   IR(76):      battery_current (int16 /10 A)
//   IR(79):      battery_power (/1000 → kW)
//   IR(80):      battery_soc_max (hi byte) / battery_soc_min (lo byte)
//   IR(81):      battery_soh (uint16)
//   IR(82-83):   charge_energy_total (uint32 /10 kWh)
//   IR(84-85):   discharge_energy_total (uint32 /10 kWh)
//   IR(90-91):   charge_energy_today (uint32 /10 kWh)
//   IR(92-93):   discharge_energy_today (uint32 /10 kWh)
//   IR(98):      battery_nominal_capacity_ah (/10 Ah — PER MODULE)
//   IR(99):      remaining_battery_capacity_ah (/10 Ah — PER MODULE)
//   IR(100):     number_of_cycles (/10)
//   IR(102-105): voltage/current limits (/10)

/// BMS aggregation device address — reports the number of BCUs at IR(61).
pub const HV_BMS_ADDRESS: u8 = 0xA0;

/// First BCU device address (one per physical HV stack: 0x70, 0x71, … 0x8F).
pub const HV_BCU_BASE_ADDRESS: u8 = 0x70;

/// First BMU device address (one per module within a stack: 0x50, 0x51, … 0x6F).
pub const HV_BMU_BASE_ADDRESS: u8 = 0x50;

/// BCU cluster block read — IR(60, 60) per stack.
pub const HV_BCU_POLL_BLOCK: RegisterBlock = RegisterBlock {
    start: 60,
    count: 60,
    register_type: RegisterType::Input,
    name: "hv_bcu_input_60_119",
};

// ---------------------------------------------------------------------------
// Write whitelist — registers that are safe to write to
// ---------------------------------------------------------------------------

/// Holding register addresses that the control encoder is allowed to write.
/// Sourced from the givenergy-modbus reference library's WRITE_SAFE_REGISTERS.
pub const SAFE_WRITE_REGS: &[u16] = &[
    20, 27, 29, 31, 32, 35, 36, 37, 38, 39, 40, 44, 45, 50, 56, 57, 59, 94, 95, 96, 110, 111, 112,
    114, 116, 163, 166,
    // Battery heater controls (givenergy-modbus #167, confirmed via GE Android app)
    104, // ENABLE_BATTERY_SELF_HEATING — hardware/batch-gated
    172, // ENABLE_MANUAL_BATTERY_HEATER — likely hardware-gated like 104
    // Charge slot 2 — Gen3 extended (HR 243-244, mirrors classic HR 31-32)
    243, 244, // Charge slots 3-10 (Gen3 extended)
    246, 247, 248, 249, 250, 251, 252, 253, 254, 255, 256, 257, 258, 259, 260, 261, 262, 263, 264,
    265, 266, 267, 268, 269, // Discharge slots 3-10 (Gen3 extended)
    276, 277, 278, 279, 280, 281, 282, 283, 284, 285, 286, 287, 288, 289, 290, 291, 292, 293, 294,
    295, 296, 297, 298, 299, // Per-slot charge targets (Gen3)
    242, 245, // Per-slot discharge targets (Gen3)
    272, 275, // AC-coupled features
    311, 313, 314, 317, // Pause mode/slot
    318, 319, 320, // Three-phase controls
    1108, 1109, 1110, 1111, 1112, 1113, 1114, 1115, 1116, 1118, 1119, 1120, 1121, 1122, 1123,
    1005, // REAL_TIME_CONTROL (three-phase mirror of HR166)
    1078, // BATTERY_POWER_CUTOFF (three-phase battery power derating %)
    // EMS plant-level control / plant_status + discharge slots
    2040, 2044, 2045, 2046, 2047, 2048, 2049, 2050, 2051, 2052,
    // EMS charge and export slots (givenergy-modbus WRITE_SAFE_REGISTERS)
    2053, 2054, 2055, 2056, 2057, 2058, 2059, 2060, 2061, 2062, 2063, 2064, 2065, 2066, 2067, 2068,
    2069, 2070, 2071,
    // App-confirmed writable registers (givenergy-modbus #167)
    199, // ENABLE_INVERTER_PARALLEL_MODE
    331, // FORCE_OFF_GRID — non-damaging, but sustained islanding state
    // Export limit — three-phase plant-level (1063, deci-W) and EMS/Gateway plant (2071, W)
    1063, 2071, // Smart Load slots 1-10 (app-confirmed, bounded HHMM values)
    554, 555, 556, 557, 558, 559, 560, 561, 562, 563, 564, 565, 566, 567, 568, 569, 570, 571, 572,
    573,
    // Other app-confirmed registers
    5010, // RESTART_HARDWARE — same class as HR163 REBOOT
    5014, // ENABLE_CALCULATED_LOAD
];

// ===========================================================================
// Meter (CT clamp) input register addresses — IR 60-89
// ===========================================================================
//
// Per givenergy-modbus reference: meters live at device addresses 0x01-0x08
// and expose data via IR 60-89 (input registers, function code 0x04).
// Each meter is probed by reading IR 60-89 and checking if V_phase_1 is non-zero.
//
// Register layout (MeterRegisterGetter from reference library):
//   IR(60):   v_phase_1 (/10 V)
//   IR(61):   v_phase_2 (/10 V)
//   IR(62):   v_phase_3 (/10 V)
//   IR(63):   i_phase_1 (/100 A)
//   IR(64):   i_phase_2 (/100 A)
//   IR(65):   i_phase_3 (/100 A)
//   IR(66):   i_ln (/100 A)
//   IR(67):   i_total (/100 A)
//   IR(68):   p_active_phase_1 (int16 W)
//   IR(69):   p_active_phase_2 (int16 W)
//   IR(70):   p_active_phase_3 (int16 W)
//   IR(71):   p_active_total (int16 W)
//   IR(72):   p_reactive_phase_1 (int16 var)
//   IR(73):   p_reactive_phase_2 (int16 var)
//   IR(74):   p_reactive_phase_3 (int16 var)
//   IR(75):   p_reactive_total (int16 var)
//   IR(76):   p_apparent_phase_1 (int16 VA)
//   IR(77):   p_apparent_phase_2 (int16 VA)
//   IR(78):   p_apparent_phase_3 (int16 VA)
//   IR(79):   p_apparent_total (int16 VA)
//   IR(80):   pf_phase_1 (/1000)
//   IR(81):   pf_phase_2 (/1000)
//   IR(82):   pf_phase_3 (/1000)
//   IR(83):   pf_total (/1000)
//   IR(84):   frequency (/100 Hz)
//   IR(85):   e_import_active (/10 kWh)
//   IR(86):   e_import_reactive (/10 kvarh)
//   IR(87):   e_export_active (/10 kWh)
//   IR(88):   e_export_reactive (/10 kvarh)
//
// Device addresses to scan for meters (0x01-0x08).
pub const METER_ADDRESSES: &[u8] = &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];

/// Block read for each external CT meter (IR 60-89, 30 registers).
pub const METER_POLL_BLOCK: RegisterBlock = RegisterBlock {
    start: 60,
    count: 30,
    register_type: RegisterType::Input,
    name: "meter_input_60_89",
};

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

/// Targeted 3-register probe of the battery pause registers (HR 318-320).
/// Used on Gen3 Hybrid (ARM fw >= 312), where the full `AC_CONFIG_BLOCK`
/// (HR 300-359) times out on the dongle (#162) but this narrow read
/// succeeds. Polled separately in `poll.rs`, not part of any model's
/// `extra_poll_blocks`. The writes themselves go through `SAFE_WRITE_REGS`
/// (which already lists 318-320) and are independent of this read.
pub const PAUSE_BLOCK: RegisterBlock = RegisterBlock {
    start: HR_BATTERY_PAUSE_MODE, // 318
    count: 3,
    register_type: RegisterType::Holding,
    name: "holding_318_320",
};

/// Three-phase configuration block — HR 1080-1124 mirror the key single-phase
/// battery/control settings at different addresses.
pub const THREE_PHASE_CONFIG_BLOCK: RegisterBlock = RegisterBlock {
    start: 1080,
    count: 45,
    register_type: RegisterType::Holding,
    name: "holding_1080_1124",
};

/// Three-phase high configuration block — HR 1000-1079 covers real-time control
/// (HR 1005), battery reserve (HR 1078) and other settings in the 1000-1079
/// range that the main config block (1080-1124) doesn't reach.
pub const THREE_PHASE_HIGH_CONFIG_BLOCK: RegisterBlock = RegisterBlock {
    start: 1000,
    count: 80,
    register_type: RegisterType::Holding,
    name: "holding_1000_1079",
};

/// Three-phase input register measurement blocks.
/// Per givenergy-modbus reference library, three-phase inverters expose their
/// real-time measurements (PV, AC grid, battery, energy totals) in the
/// IR(1000-1414) range, split into 60-register chunks. Without these blocks the
/// dashboard shows zero for solar/grid/battery power and all daily energy totals.
pub const THREE_PHASE_INPUT_BLOCK_1: RegisterBlock = RegisterBlock {
    start: 1000,
    count: 60,
    register_type: RegisterType::Input,
    name: "input_1000_1059",
};
pub const THREE_PHASE_INPUT_BLOCK_2: RegisterBlock = RegisterBlock {
    start: 1060,
    count: 60,
    register_type: RegisterType::Input,
    name: "input_1060_1119",
};
pub const THREE_PHASE_INPUT_BLOCK_3: RegisterBlock = RegisterBlock {
    start: 1120,
    count: 60,
    register_type: RegisterType::Input,
    name: "input_1120_1179",
};
pub const THREE_PHASE_INPUT_BLOCK_4: RegisterBlock = RegisterBlock {
    start: 1180,
    count: 60,
    register_type: RegisterType::Input,
    name: "input_1180_1239",
};
pub const THREE_PHASE_INPUT_BLOCK_5: RegisterBlock = RegisterBlock {
    start: 1240,
    count: 60,
    register_type: RegisterType::Input,
    name: "input_1240_1299",
};
pub const THREE_PHASE_INPUT_BLOCK_6: RegisterBlock = RegisterBlock {
    start: 1300,
    count: 60,
    register_type: RegisterType::Input,
    name: "input_1300_1359",
};
pub const THREE_PHASE_INPUT_BLOCK_7: RegisterBlock = RegisterBlock {
    start: 1360,
    count: 54,
    register_type: RegisterType::Input,
    name: "input_1360_1413",
};

/// All seven three-phase input register blocks, in poll order.
pub const THREE_PHASE_INPUT_BLOCKS: &[RegisterBlock] = &[
    THREE_PHASE_INPUT_BLOCK_1,
    THREE_PHASE_INPUT_BLOCK_2,
    THREE_PHASE_INPUT_BLOCK_3,
    THREE_PHASE_INPUT_BLOCK_4,
    THREE_PHASE_INPUT_BLOCK_5,
    THREE_PHASE_INPUT_BLOCK_6,
    THREE_PHASE_INPUT_BLOCK_7,
];

/// Extra blocks for models that need both AC config and three-phase config.
pub const AC_AND_THREE_PHASE_BLOCKS: &[RegisterBlock] =
    &[AC_CONFIG_BLOCK, THREE_PHASE_CONFIG_BLOCK];

/// Extra blocks for HV/three-phase models that also use extended schedules.
pub const EXTENDED_AND_THREE_PHASE_BLOCKS: &[RegisterBlock] = &[
    EXTENDED_SLOTS_BLOCK,
    THREE_PHASE_HIGH_CONFIG_BLOCK,
    THREE_PHASE_CONFIG_BLOCK,
];

/// Extra blocks for residential All-in-One models: extended slots plus AC-output config.
pub const EXTENDED_AND_AC_CONFIG_BLOCKS: &[RegisterBlock] =
    &[EXTENDED_SLOTS_BLOCK, AC_CONFIG_BLOCK];

/// Extra blocks for AC three-phase models: AC config plus full three-phase schedule/config.
pub const AC_EXTENDED_AND_THREE_PHASE_BLOCKS: &[RegisterBlock] = &[
    AC_CONFIG_BLOCK,
    EXTENDED_SLOTS_BLOCK,
    THREE_PHASE_CONFIG_BLOCK,
];

/// Gateway aggregation Input Register blocks — IR 1600–1859.
///
/// The GivEnergy Gateway (DTC family 0x70xx) is an AC distribution / control
/// hub that aggregates telemetry from its child All-in-One (AIO) unit(s). All
/// of its live measurements live in a unique Input Register bank at IR
/// 1600–1859 (system state, grid/PV/load power, daily + lifetime energy,
/// per-AIO SOC/power/energy/serials, faults). This bank replaces the standard
/// IR 0-59 / IR 1000-1414 telemetry ranges used by hybrids and three-phase
/// models — those ranges are unmapped on the Gateway and reading them only
/// wastes poll-cycle time / invites timeouts.
///
/// Sourced from `dewet22/givenergy-modbus` `client/commands.py` `refresh()`
/// (reads IR 1600–1859 in 60-register chunks) and `model/gateway.py`. The
/// chunking deliberately swallows the unmapped gaps (e.g. 1605–1607,
/// 1632–1639) as zeros — the decoders read only specific offsets, so the gaps
/// are harmless. See `gateway-design/gateway-register-reference.md` §4.
pub const GATEWAY_INPUT_BLOCK_1: RegisterBlock = RegisterBlock {
    start: 1600,
    count: 60,
    register_type: RegisterType::Input,
    name: "input_1600_1659",
};
pub const GATEWAY_INPUT_BLOCK_2: RegisterBlock = RegisterBlock {
    start: 1660,
    count: 60,
    register_type: RegisterType::Input,
    name: "input_1660_1719",
};
pub const GATEWAY_INPUT_BLOCK_3: RegisterBlock = RegisterBlock {
    start: 1720,
    count: 60,
    register_type: RegisterType::Input,
    name: "input_1720_1779",
};
pub const GATEWAY_INPUT_BLOCK_4: RegisterBlock = RegisterBlock {
    start: 1780,
    count: 51,
    register_type: RegisterType::Input,
    name: "input_1780_1830",
};
/// Block 5 starts at IR 1831 (not 1840) so that the V1 AIO serial addresses
/// (aio1 @ 1831-1835, aio2 @ 1838-1842, aio3 @ 1845-1849) — which straddle the
/// 1839/1840 boundary under plain 60-register chunking — are fully contained
/// within a single block's data slice. V2 serials (1841+) fall here too.
pub const GATEWAY_INPUT_BLOCK_5: RegisterBlock = RegisterBlock {
    start: 1831,
    count: 29,
    register_type: RegisterType::Input,
    name: "input_1831_1859",
};

/// All five Gateway aggregation Input Register blocks, in poll order
/// (ascending IR address).
pub const GATEWAY_INPUT_BLOCKS: &[RegisterBlock] = &[
    GATEWAY_INPUT_BLOCK_1,
    GATEWAY_INPUT_BLOCK_2,
    GATEWAY_INPUT_BLOCK_3,
    GATEWAY_INPUT_BLOCK_4,
    GATEWAY_INPUT_BLOCK_5,
];

// ===========================================================================
// EMS / Gateway plant-level holding registers (HR 2040-2075)
// ===========================================================================
//
// Per givenergy-modbus `commands.py:327` and the EMS plant model, the
// plant-level holding register block is exactly 36 registers (HR 2040-2075).
// Key registers:
//   HR 2040  EMS_PLANT_ENABLE              (bool)
//   HR 2044-2052  EMS_DISCHARGE_SLOT_1..3 + target SOC
//   HR 2053-2061  EMS_CHARGE_SLOT_1..3 + target SOC
//   HR 2062-2070  EXPORT_SLOT_1..3 + target SOC
//   HR 2071  EXPORT_POWER_LIMIT             (uint16 W — GivTCP `set_export_limit`)
//   HR 2072  CAR_CHARGE_MODE
//   HR 2073  CAR_CHARGE_BOOST
//
// Only polled on EMS / Gateway / EmsCommercial devices — see the device-type
// extra_poll_blocks() routing in `inverter::model`.
//
// 36-register block starting at HR 2040 covers HR 2040-2075 inclusive.
pub const EMS_PLANT_HOLDING_BLOCK: RegisterBlock = RegisterBlock {
    start: 2040,
    count: 36,
    register_type: RegisterType::Holding,
    name: "holding_2040_2075",
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
        assert_eq!(STANDARD_POLL_BLOCKS.len(), 4);
        // Input 0-59 covers all telemetry (IR 0-59)
        assert_eq!(STANDARD_POLL_BLOCKS[0].start, 0);
        assert_eq!(STANDARD_POLL_BLOCKS[0].count, 60);
        // Holding 0-59
        assert_eq!(STANDARD_POLL_BLOCKS[1].start, 0);
        assert_eq!(STANDARD_POLL_BLOCKS[1].count, 60);
        // Holding 60-119 covers charge_slot_1 (94-95), soc_reserve (110), limits (111-112)
        assert_eq!(STANDARD_POLL_BLOCKS[2].start, 60);
        assert_eq!(STANDARD_POLL_BLOCKS[2].count, 60);
        // Input 180-181/183 covers alternative battery lifetime totals
        // (IR 180-181) plus the Gen1-authoritative alternative daily totals
        // (IR 182-183). A full 60-register window is not needed.
        assert_eq!(STANDARD_POLL_BLOCKS[3].start, 180);
        assert_eq!(STANDARD_POLL_BLOCKS[3].count, 4);
        assert_eq!(STANDARD_POLL_BLOCKS[3].name, "input_180_181");
        assert_eq!(STANDARD_POLL_BLOCKS[3].register_type, RegisterType::Input);
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
        assert!(SAFE_WRITE_REGS.contains(&HR_BATTERY_SELF_HEATING)); // 104
        assert!(SAFE_WRITE_REGS.contains(&HR_MANUAL_BATTERY_HEATER)); // 172
        assert!(SAFE_WRITE_REGS.contains(&HR_SYSTEM_TIME_YEAR)); // 35
        assert!(SAFE_WRITE_REGS.contains(&HR_SYSTEM_TIME_MONTH)); // 36
        assert!(SAFE_WRITE_REGS.contains(&HR_SYSTEM_TIME_DAY)); // 37
        assert!(SAFE_WRITE_REGS.contains(&HR_SYSTEM_TIME_HOUR)); // 38
        assert!(SAFE_WRITE_REGS.contains(&HR_SYSTEM_TIME_MINUTE)); // 39
        assert!(SAFE_WRITE_REGS.contains(&HR_SYSTEM_TIME_SECOND)); // 40
        assert!(SAFE_WRITE_REGS.contains(&HR_EXPORT_PRIORITY)); // 311
        assert!(SAFE_WRITE_REGS.contains(&HR_AC_BATTERY_CHARGE_LIMIT)); // 313
        assert!(SAFE_WRITE_REGS.contains(&HR_AC_BATTERY_DISCHARGE_LIMIT)); // 314
        assert!(SAFE_WRITE_REGS.contains(&HR_ENABLE_EPS)); // 317
        assert!(SAFE_WRITE_REGS.contains(&HR_BATTERY_PAUSE_MODE)); // 318
        assert!(SAFE_WRITE_REGS.contains(&HR_BATTERY_PAUSE_SLOT_1_START)); // 319
        assert!(SAFE_WRITE_REGS.contains(&HR_BATTERY_PAUSE_SLOT_1_END)); // 320
        assert!(SAFE_WRITE_REGS.contains(&HR_3PH_BATTERY_DISCHARGE_LIMIT)); // 1108
        assert!(SAFE_WRITE_REGS.contains(&HR_3PH_BATTERY_SOC_RESERVE)); // 1109
        assert!(SAFE_WRITE_REGS.contains(&HR_3PH_BATTERY_CHARGE_LIMIT)); // 1110
        assert!(SAFE_WRITE_REGS.contains(&HR_3PH_CHARGE_TARGET_SOC)); // 1111
        assert!(SAFE_WRITE_REGS.contains(&HR_3PH_CHARGE_SLOT_1_START)); // 1113
        assert!(SAFE_WRITE_REGS.contains(&HR_3PH_CHARGE_SLOT_1_END)); // 1114
        assert!(SAFE_WRITE_REGS.contains(&HR_3PH_CHARGE_SLOT_2_START)); // 1115
        assert!(SAFE_WRITE_REGS.contains(&HR_3PH_CHARGE_SLOT_2_END)); // 1116
        assert!(SAFE_WRITE_REGS.contains(&HR_3PH_DISCHARGE_SLOT_1_START)); // 1118
        assert!(SAFE_WRITE_REGS.contains(&HR_3PH_DISCHARGE_SLOT_1_END)); // 1119
        assert!(SAFE_WRITE_REGS.contains(&HR_3PH_DISCHARGE_SLOT_2_START)); // 1120
        assert!(SAFE_WRITE_REGS.contains(&HR_3PH_DISCHARGE_SLOT_2_END)); // 1121
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
