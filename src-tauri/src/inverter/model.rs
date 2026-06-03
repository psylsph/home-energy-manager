//! Inverter data model structs.
//!
//! Defines the typed representation of inverter state, including battery mode
//! derivation from the three key holding registers (HR 27, 59, 110).

// ---------------------------------------------------------------------------
// Battery state
// ---------------------------------------------------------------------------

/// Battery charging/discharging state, derived from battery power sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BatteryState {
    #[default]
    Idle,
    Charging,
    Discharging,
}

impl BatteryState {
    /// Derive battery state from power value.
    pub fn from_power(power: i32) -> Self {
        if power > 0 {
            Self::Charging
        } else if power < 0 {
            Self::Discharging
        } else {
            Self::Idle
        }
    }
}

// ---------------------------------------------------------------------------
// Battery mode (derived from 3 registers)
// ---------------------------------------------------------------------------

/// Battery operating mode, derived from HR(27), HR(59), HR(110).
///
/// Derivation logic from GivTCP read.py:
/// - eco_mode = HR(27) battery_power_mode: 0=export, 1=self-consumption
/// - enable_discharge = HR(59) boolean
/// - battery_soc_reserve = HR(110) percentage
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BatteryMode {
    #[default]
    Unknown,
    /// eco=1, discharge=false, reserve!=100
    Eco,
    /// eco=1, discharge=false, reserve==100
    EcoPaused,
    /// eco=1, discharge=true
    TimedDemand,
    /// eco=0, discharge=true
    TimedExport,
    /// eco=0, discharge=false
    ExportPaused,
}

impl BatteryMode {
    /// Derive the battery mode from the three key holding register values.
    pub fn from_registers(
        battery_power_mode: u16,
        enable_discharge: bool,
        battery_soc_reserve: u16,
    ) -> Self {
        let eco = battery_power_mode == 1;
        match (eco, enable_discharge, battery_soc_reserve == 100) {
            (true, false, false) => Self::Eco,
            (true, false, true) => Self::EcoPaused,
            (true, true, _) => Self::TimedDemand,
            (false, true, _) => Self::TimedExport,
            (false, false, _) => Self::ExportPaused,
        }
    }
}

// ---------------------------------------------------------------------------
// Device type
// ---------------------------------------------------------------------------

/// Inverter hardware variant, read from holding register HR(0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DeviceType {
    Gen1Hybrid,
    Gen2Hybrid,
    Gen3Hybrid,
    Gen3Hybrid8kW,
    Gen3Hybrid10kW,
    ACCoupled,
    ACCoupledMk2,
    ThreePhase,
    AllInOne6kW,
    AllInOne5kW,
    AIO8kW,
    AIO10kW,
    Unknown(u16),
}

impl Default for DeviceType {
    fn default() -> Self {
        Self::Unknown(0)
    }
}

impl DeviceType {
    /// Map a raw HR(0) hex value to DeviceType.
    pub fn from_register(val: u16) -> Self {
        match val {
            0x1001 => Self::Gen1Hybrid,
            0x2001 => Self::Gen3Hybrid, // may be refined by refine_with_arm_fw (Gen2 if FW century 1-2)
            0x2101 => Self::Gen3Hybrid8kW,
            0x2102 => Self::Gen3Hybrid10kW,
            0x3001 => Self::ACCoupled,
            0x3002 => Self::ACCoupledMk2,
            0x4001 => Self::ThreePhase,
            0x8001 => Self::AllInOne6kW,
            0x8002 => Self::AllInOne5kW,
            0x8003 => Self::AllInOne5kW,
            0x8102 => Self::AIO8kW,
            0x8103 => Self::AIO10kW,
            _ => {
                let prefix = val >> 8;
                match prefix {
                    0x10 => Self::Gen1Hybrid,
                    0x20 | 0x21 => Self::Gen3Hybrid, // refined by refine_with_arm_fw
                    0x30 => Self::ACCoupled,
                    0x40 => Self::ThreePhase,
                    0x80 | 0x81 => Self::AllInOne6kW,
                    _ => Self::Unknown(val),
                }
            }
        }
    }

    /// Refine the device type using the ARM firmware version.
    /// 0x20XX with ARM FW century 1 or 2 is Gen2 (2600W), not Gen3 (3600W).
    pub fn refine_with_arm_fw(self, arm_fw: u16) -> Self {
        match self {
            Self::Gen3Hybrid => {
                let century = arm_fw / 100;
                if century == 1 || century == 2 {
                    Self::Gen2Hybrid
                } else {
                    Self::Gen3Hybrid
                }
            }
            other => other,
        }
    }

    /// Nominal battery voltage for capacity calculation.
    ///
    /// Per givenergy-modbus and GivTCP references, the kWh calculation uses
    /// a fixed nominal voltage per device type, NOT the live system voltage.
    pub fn nominal_battery_voltage(&self) -> f32 {
        match self {
            Self::AllInOne6kW | Self::AllInOne5kW | Self::AIO8kW | Self::AIO10kW => 307.0,
            Self::ThreePhase => 76.8,
            _ => 51.2,
        }
    }

    /// Human-readable display name for the device type.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Gen1Hybrid => "Gen 1 Hybrid",
            Self::Gen2Hybrid => "Gen 2 Hybrid",
            Self::Gen3Hybrid => "Gen 3 Hybrid",
            Self::Gen3Hybrid8kW => "Gen 3 Hybrid 8kW",
            Self::Gen3Hybrid10kW => "Gen 3 Hybrid 10kW",
            Self::ACCoupled => "AC Coupled",
            Self::ACCoupledMk2 => "AC Coupled Mk2",
            Self::ThreePhase => "Three Phase",
            Self::AllInOne6kW => "All-in-One 6kW",
            Self::AllInOne5kW => "All-in-One 5kW",
            Self::AIO8kW => "AIO 8kW",
            Self::AIO10kW => "AIO 10kW",
            Self::Unknown(_) => "Unknown",
        }
    }

    /// Maximum battery charge/discharge power in watts.
    ///
    /// Per GivTCP source code, the inverter hardware limits the DC battery
    /// charge/discharge rate regardless of what the register says.
    pub fn max_battery_power_w(&self) -> u32 {
        match self {
            Self::Gen1Hybrid => 2500,
            Self::Gen2Hybrid => 2600,
            Self::Gen3Hybrid => 3600,
            Self::Gen3Hybrid8kW | Self::AIO8kW => 8000,
            Self::Gen3Hybrid10kW | Self::AIO10kW => 10000,
            Self::ACCoupled | Self::ACCoupledMk2 => 3000,
            Self::ThreePhase => 6000,
            Self::AllInOne6kW => 6000,
            Self::AllInOne5kW => 5000,
            Self::Unknown(_) => 3600, // assume Gen3 hybrid as fallback
        }
    }

    /// Maximum AC output power in watts (per device model/spec).
    pub fn max_ac_power_w(&self) -> u32 {
        match self {
            Self::Gen1Hybrid => 5000,
            Self::Gen2Hybrid | Self::Gen3Hybrid => 5000,
            Self::Gen3Hybrid8kW => 8000,
            Self::Gen3Hybrid10kW => 10000,
            Self::ACCoupled | Self::ACCoupledMk2 => 3000,
            Self::ThreePhase => 6000,
            Self::AllInOne6kW => 6000,
            Self::AllInOne5kW => 5000,
            Self::AIO8kW => 8000,
            Self::AIO10kW => 10000,
            Self::Unknown(_) => 5000,
        }
    }

    /// Whether this device supports Gen3 extended registers (per-slot target SOC etc.).
    /// Gen3 hybrids and AIO 8kW/10kW have HR 242+ for per-slot targets.
    pub fn supports_gen3_extended(&self) -> bool {
        matches!(
            self,
            Self::Gen3Hybrid
                | Self::Gen3Hybrid8kW
                | Self::Gen3Hybrid10kW
                | Self::AIO8kW
                | Self::AIO10kW
        )
    }

    /// Maximum number of charge schedule slots this device supports.
    ///
    /// - AC Coupled and Gen1 Hybrid: **1** charge slot (HR 94-95)
    /// - All other single-phase inverters: **2** slots (HR 94-95, HR 31-32)
    /// - Gen3/AIO: up to **10** in extended blocks
    pub fn max_charge_slots(&self) -> u8 {
        match self {
            // AC Coupled and Gen1 have only one charge slot (HR 94-95)
            Self::ACCoupled | Self::ACCoupledMk2 | Self::Gen1Hybrid => 1,
            // Gen3/AIO with extended 10-slot scheduling
            Self::Gen3Hybrid | Self::Gen3Hybrid8kW | Self::Gen3Hybrid10kW
            | Self::AllInOne5kW | Self::AllInOne6kW | Self::AIO8kW | Self::AIO10kW => 10,
            // All others: 2 slots
            _ => 2,
        }
    }

    /// Maximum number of discharge schedule slots this device supports.
    pub fn max_discharge_slots(&self) -> u8 {
        match self {
            Self::ACCoupled | Self::ACCoupledMk2 | Self::Gen1Hybrid => 1,
            Self::Gen3Hybrid | Self::Gen3Hybrid8kW | Self::Gen3Hybrid10kW
            | Self::AllInOne5kW | Self::AllInOne6kW | Self::AIO8kW | Self::AIO10kW => 10,
            _ => 2,
        }
    }

    /// Returns additional register blocks that should be polled for this device type,
    /// beyond the standard set (IR 0-59, HR 0-59, HR 60-119).
    pub fn extra_poll_blocks(&self) -> &'static [crate::modbus::registers::RegisterBlock] {
        use crate::modbus::registers::{AC_CONFIG_BLOCK, EXTENDED_SLOTS_BLOCK};
        match self {
            // Gen3 / AIO / HV Gen3: extended 10-slot scheduling
            Self::Gen3Hybrid | Self::Gen3Hybrid8kW | Self::Gen3Hybrid10kW
            | Self::AllInOne5kW | Self::AllInOne6kW | Self::AIO8kW | Self::AIO10kW => {
                &[EXTENDED_SLOTS_BLOCK]
            }
            // AC-coupled: extended slots + AC config block (export priority, EPS, pause)
            Self::ACCoupled | Self::ACCoupledMk2 => {
                &[AC_CONFIG_BLOCK, EXTENDED_SLOTS_BLOCK]
            }
            // Three-phase: AC config block
            Self::ThreePhase => {
                &[AC_CONFIG_BLOCK]
            }
            // Gen1/Gen2 hybrid: AC config block (pause mode available)
            Self::Gen1Hybrid | Self::Gen2Hybrid => {
                &[AC_CONFIG_BLOCK]
            }
            _ => &[],
        }
    }
}

/// Serde default for max slot counts (2 = safe for all models).
fn default_max_slots() -> u8 {
    2
}

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

/// A single battery module within the battery assembly.
///
/// For LV batteries each physical battery is a "module". For HV stacks
/// (All-in-One, HV Gen3) a module is a BMU within the BCU stack.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BatteryModule {
    /// Module index (0-based).
    pub index: usize,
    /// State of charge (%).
    pub soc: u8,
    /// Temperature (deg C) — max cell temperature from BMS.
    pub temperature: f32,
    /// Voltage (V) — total pack voltage.
    pub voltage: f32,
    /// Current (A) — pack current (not available on LV BMS; 0.0).
    pub current: f32,
    /// Battery serial number (from BMS input registers IR 110-114).
    #[serde(default)]
    pub serial: String,
    /// Number of charge cycles.
    #[serde(default)]
    pub num_cycles: u16,
    /// Number of cells in this module.
    #[serde(default)]
    pub num_cells: u16,
    /// Individual cell voltages in V (from BMS IR 60-75, up to 16 cells).
    #[serde(default)]
    pub cell_voltages: Vec<f32>,
    /// Cell group temperatures in °C (from BMS IR 76-79, up to 4 groups).
    #[serde(default)]
    pub cell_temperatures: Vec<f32>,
    /// BMS firmware version (raw register value).
    #[serde(default)]
    pub bms_firmware: u16,
    /// Calibrated total capacity in Ah (IR 84-85, uint32 0.01 Ah).
    #[serde(default)]
    pub capacity_ah: f32,
    /// Design / nameplate capacity in Ah (IR 86-87, uint32 0.01 Ah).
    #[serde(default)]
    pub design_capacity_ah: f32,
    /// Remaining / available capacity in Ah (IR 88-89, uint32 0.01 Ah).
    #[serde(default)]
    pub remaining_capacity_ah: f32,
}

/// A single charge or discharge schedule slot.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ScheduleSlot {
    /// Whether the slot is active (start_time >= 60).
    pub enabled: bool,
    /// Start hour (0-23).
    pub start_hour: u8,
    /// Start minute (0-59).
    pub start_minute: u8,
    /// End hour (0-23).
    pub end_hour: u8,
    /// End minute (0-59).
    pub end_minute: u8,
    /// Target SOC (from separate register, 0 if not applicable).
    #[serde(default)]
    pub target_soc: u8,
}

/// Complete snapshot of inverter state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct InverterSnapshot {
    /// Unix timestamp of this reading.
    pub timestamp: i64,

    // -- Power (watts) --
    pub solar_power: i32,
    pub pv1_power: i32,
    pub pv2_power: i32,
    pub battery_power: i32,
    pub grid_power: i32,
    pub home_power: i32,

    // -- PV details --
    pub pv1_voltage: f32,
    pub pv2_voltage: f32,
    pub pv1_current: f32,
    pub pv2_current: f32,

    // -- Battery details --
    pub soc: u8,
    pub battery_voltage: f32,
    pub battery_current: f32,
    pub battery_temperature: f32,
    pub battery_state: BatteryState,
    pub battery_capacity_kwh: f32,
    pub battery_modules: Vec<BatteryModule>,

    // -- Grid details --
    pub grid_voltage: f32,
    pub grid_frequency: f32,

    // -- Inverter --
    pub inverter_temperature: f32,

    // -- Energy totals (kWh) --
    pub today_solar_kwh: f32,
    pub today_import_kwh: f32,
    pub today_export_kwh: f32,
    pub today_charge_kwh: f32,
    pub today_discharge_kwh: f32,
    pub today_consumption_kwh: f32,
    /// AC charge from grid today (kWh). IR(35) — NOT house consumption.
    /// Used in the consumption formula: solar + import - export - ac_charge.
    pub today_ac_charge_kwh: f32,

    // -- Configuration --
    pub battery_mode: BatteryMode,
    pub device_type: DeviceType,
    /// Raw 4-char hex device type code from HR(0) (e.g. "2001", "3001").
    pub device_type_code: String,
    /// Human-readable device type name for the frontend.
    #[serde(default)]
    pub device_type_display: String,
    pub battery_reserve: u8,
    pub charge_rate: u8,
    pub discharge_rate: u8,
    /// Inverter max output active power rate (0-100%).
    pub active_power_rate: u8,
    /// Max battery charge/discharge power in watts (per inverter model).
    pub max_battery_power_w: u32,
    /// Max AC output power in watts (per device model).
    #[serde(default)]
    pub max_ac_power_w: u32,
    pub target_soc: u8,
    pub enable_charge: bool,
    pub enable_charge_target: bool,
    pub enable_discharge: bool,
    /// Set to true when the auto-winter state machine has activated winter
    /// mode (distinct from `enable_charge_target` which any write can set).
    #[serde(default)]
    pub auto_winter_active: bool,
    /// True when the Cosy tariff timer is actively force-charging the battery.
    #[serde(default)]
    pub cosy_active: bool,
    /// True when Cosy tariff mode is enabled in settings (may be between slots).
    #[serde(default)]
    pub cosy_enabled: bool,
    /// Battery calibration stage (0=off, 5=balance).
    #[serde(default)]
    pub battery_calibration_stage: u8,
    pub inverter_serial: String,
    pub firmware_version: String,

    // -- Schedules --
    /// Charge slots 0-9 (10 slots for Gen3 extended; slots 3-9 unused on Gen1/2).
    pub charge_slots: [ScheduleSlot; 10],
    /// Discharge slots 0-9 (10 slots for Gen3 extended; slots 2-9 unused on Gen1/2).
    pub discharge_slots: [ScheduleSlot; 10],
    /// Maximum number of charge slots this device supports (frontend hint).
    #[serde(default = "default_max_slots")]
    pub max_charge_slots: u8,
    /// Maximum number of discharge slots this device supports (frontend hint).
    #[serde(default = "default_max_slots")]
    pub max_discharge_slots: u8,

    // -- AC-coupled / extended config (from HR 300-359) --
    /// Export priority (0=battery, 1=grid, 2=load) — HR 311.
    #[serde(default)]
    pub ac_export_priority: u8,
    /// Emergency Power Supply enabled — HR 317.
    #[serde(default)]
    pub ac_eps_enabled: bool,
    /// Battery pause mode (0=disabled) — HR 318.
    #[serde(default)]
    pub battery_pause_mode: u8,
    /// Battery pause time slot — HR 319-320.
    #[serde(default)]
    pub battery_pause_slot: ScheduleSlot,
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- BatteryState --------------------------------------------------------
    #[test]
    fn battery_state_charging() {
        assert_eq!(BatteryState::from_power(1), BatteryState::Charging);
        assert_eq!(BatteryState::from_power(800), BatteryState::Charging);
    }

    #[test]
    fn battery_state_discharging() {
        assert_eq!(BatteryState::from_power(-1), BatteryState::Discharging);
        assert_eq!(BatteryState::from_power(-500), BatteryState::Discharging);
    }

    #[test]
    fn battery_state_idle() {
        assert_eq!(BatteryState::from_power(0), BatteryState::Idle);
    }

    // -- BatteryMode ----------------------------------------------------------
    #[test]
    fn mode_eco() {
        assert_eq!(BatteryMode::from_registers(1, false, 4), BatteryMode::Eco);
    }

    #[test]
    fn mode_eco_paused() {
        assert_eq!(
            BatteryMode::from_registers(1, false, 100),
            BatteryMode::EcoPaused
        );
    }

    #[test]
    fn mode_timed_demand() {
        assert_eq!(
            BatteryMode::from_registers(1, true, 4),
            BatteryMode::TimedDemand
        );
        assert_eq!(
            BatteryMode::from_registers(1, true, 100),
            BatteryMode::TimedDemand
        );
    }

    #[test]
    fn mode_timed_export() {
        assert_eq!(
            BatteryMode::from_registers(0, true, 4),
            BatteryMode::TimedExport
        );
        assert_eq!(
            BatteryMode::from_registers(0, true, 100),
            BatteryMode::TimedExport
        );
    }

    #[test]
    fn mode_export_paused() {
        assert_eq!(
            BatteryMode::from_registers(0, false, 4),
            BatteryMode::ExportPaused
        );
    }

    // -- DeviceType -----------------------------------------------------------
    #[test]
    fn device_type_gen3() {
        assert_eq!(DeviceType::from_register(0x2001), DeviceType::Gen3Hybrid);
    }

    #[test]
    fn device_type_ac() {
        assert_eq!(DeviceType::from_register(0x3001), DeviceType::ACCoupled);
    }

    #[test]
    fn device_type_unknown() {
        assert!(matches!(
            DeviceType::from_register(0x9999),
            DeviceType::Unknown(_)
        ));
    }

    #[test]
    fn device_type_gen1() {
        assert_eq!(DeviceType::from_register(0x1001), DeviceType::Gen1Hybrid);
    }

    #[test]
    fn device_type_gen3_8kw() {
        assert_eq!(DeviceType::from_register(0x2101), DeviceType::Gen3Hybrid8kW);
    }

    #[test]
    fn device_type_gen3_10kw() {
        assert_eq!(DeviceType::from_register(0x2102), DeviceType::Gen3Hybrid10kW);
    }

    #[test]
    fn device_type_ac_mk2() {
        assert_eq!(DeviceType::from_register(0x3002), DeviceType::ACCoupledMk2);
    }

    #[test]
    fn device_type_gen2_refined_by_low_arm_fw() {
        // 0x2001 with ARM FW century 1 → Gen2
        let dt = DeviceType::from_register(0x2001).refine_with_arm_fw(130);
        assert_eq!(dt, DeviceType::Gen2Hybrid);
    }

    #[test]
    fn device_type_gen3_confirmed_by_high_arm_fw() {
        // 0x2001 with ARM FW century 3 → Gen3
        let dt = DeviceType::from_register(0x2001).refine_with_arm_fw(352);
        assert_eq!(dt, DeviceType::Gen3Hybrid);
    }

    #[test]
    fn device_type_ac_unaffected_by_arm_fw() {
        // AC coupled with any ARM FW stays AC
        let dt = DeviceType::from_register(0x3001).refine_with_arm_fw(130);
        assert_eq!(dt, DeviceType::ACCoupled);
    }

    // -- Serialization --------------------------------------------------------
    #[test]
    fn battery_mode_serializes_snake_case() {
        let modes = [
            (BatteryMode::Eco, "\"eco\""),
            (BatteryMode::EcoPaused, "\"eco_paused\""),
            (BatteryMode::TimedDemand, "\"timed_demand\""),
            (BatteryMode::TimedExport, "\"timed_export\""),
            (BatteryMode::ExportPaused, "\"export_paused\""),
            (BatteryMode::Unknown, "\"unknown\""),
        ];
        for (mode, expected) in modes {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(json, expected);
        }
    }

    #[test]
    fn schedule_slot_default_is_disabled() {
        let slot = ScheduleSlot::default();
        assert!(!slot.enabled);
        assert_eq!(slot.target_soc, 0);
    }

    #[test]
    fn snapshot_default() {
        let snap = InverterSnapshot::default();
        assert_eq!(snap.timestamp, 0);
        assert_eq!(snap.solar_power, 0);
        assert_eq!(snap.charge_slots.len(), 10);
        assert_eq!(snap.discharge_slots.len(), 10);
    }

    // -- DeviceType: all known codes from registry -------------------------
    #[test]
    fn device_type_all_known_codes() {
        let cases: &[(u16, &str, u32, u32, f32)] = &[
            // (code, display_name, max_battery_w, max_ac_w, nominal_voltage)
            (0x1001, "Gen 1 Hybrid", 2500, 5000, 51.2),
            (0x2001, "Gen 3 Hybrid", 3600, 5000, 51.2), // before ARM FW refinement
            (0x2101, "Gen 3 Hybrid 8kW", 8000, 8000, 51.2),
            (0x2102, "Gen 3 Hybrid 10kW", 10000, 10000, 51.2),
            (0x3001, "AC Coupled", 3000, 3000, 51.2),
            (0x3002, "AC Coupled Mk2", 3000, 3000, 51.2),
            (0x4001, "Three Phase", 6000, 6000, 76.8),
            (0x8001, "All-in-One 6kW", 6000, 6000, 307.0),
            (0x8002, "All-in-One 5kW", 5000, 5000, 307.0),
            (0x8003, "All-in-One 5kW", 5000, 5000, 307.0),
            (0x8102, "AIO 8kW", 8000, 8000, 307.0),
            (0x8103, "AIO 10kW", 10000, 10000, 307.0),
        ];
        for (code, expected_name, expected_batt_w, expected_ac_w, expected_voltage) in cases {
            let dt = DeviceType::from_register(*code);
            assert_eq!(
                dt.display_name(), *expected_name,
                "display_name mismatch for 0x{:04X}", code
            );
            assert_eq!(
                dt.max_battery_power_w(), *expected_batt_w,
                "max_battery_power_w mismatch for 0x{:04X}", code
            );
            assert_eq!(
                dt.max_ac_power_w(), *expected_ac_w,
                "max_ac_power_w mismatch for 0x{:04X}", code
            );
            assert!((
                dt.nominal_battery_voltage() - expected_voltage
            ).abs() < 0.01,
                "nominal_battery_voltage mismatch for 0x{:04X}: got {} expected {}",
                code, dt.nominal_battery_voltage(), expected_voltage
            );
        }
    }

    #[test]
    fn device_type_unknown_fallbacks() {
        let dt = DeviceType::Unknown(0x9999);
        assert_eq!(dt.display_name(), "Unknown");
        assert_eq!(dt.max_battery_power_w(), 3600);
        assert_eq!(dt.max_ac_power_w(), 5000);
    }

    #[test]
    fn device_type_prefix_fallback() {
        // Unknown codes with known prefixes should fall back to the generic variant
        assert_eq!(DeviceType::from_register(0x1002), DeviceType::Gen1Hybrid);
        assert_eq!(DeviceType::from_register(0x2099), DeviceType::Gen3Hybrid);
        assert_eq!(DeviceType::from_register(0x3099), DeviceType::ACCoupled);
        assert_eq!(DeviceType::from_register(0x4099), DeviceType::ThreePhase);
        assert_eq!(DeviceType::from_register(0x8099), DeviceType::AllInOne6kW);
    }

    #[test]
    fn gen2_refined_from_arm_fw_century_2() {
        // ARM FW with century=2 (e.g. 252 → year 2025) → Gen2
        let dt = DeviceType::from_register(0x2001).refine_with_arm_fw(252);
        assert_eq!(dt, DeviceType::Gen2Hybrid);
        assert_eq!(dt.max_battery_power_w(), 2600);
    }

    #[test]
    fn gen3_stays_gen3_for_arm_fw_century_8() {
        let dt = DeviceType::from_register(0x2001).refine_with_arm_fw(852);
        assert_eq!(dt, DeviceType::Gen3Hybrid);
        assert_eq!(dt.max_battery_power_w(), 3600);
    }
}
