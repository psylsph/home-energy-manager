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
    ACCoupled,
    AllInOne,
    ThreePhase,
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
        let prefix = val >> 8;
        match prefix {
            0x20 => Self::Gen3Hybrid, // simplified; real detection needs ARM FW
            0x21 => Self::Gen3Hybrid,
            0x30 => Self::ACCoupled,
            0x40 => Self::ThreePhase,
            0x80 => Self::AllInOne,
            _ => Self::Unknown(val),
        }
    }

    /// Nominal battery voltage for capacity calculation.
    ///
    /// Per givenergy-modbus and GivTCP references, the kWh calculation uses
    /// a fixed nominal voltage per device type, NOT the live system voltage.
    pub fn nominal_battery_voltage(&self) -> f32 {
        match self {
            Self::AllInOne => 307.0,
            Self::ThreePhase => 76.8,
            _ => 51.2,
        }
    }
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
    /// BMS firmware version.
    #[serde(default)]
    pub bms_firmware: u16,
}

/// A single charge or discharge schedule slot.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

impl Default for ScheduleSlot {
    fn default() -> Self {
        Self {
            enabled: false,
            start_hour: 0,
            start_minute: 0,
            end_hour: 0,
            end_minute: 0,
            target_soc: 0,
        }
    }
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

    // -- Configuration --
    pub battery_mode: BatteryMode,
    pub device_type: DeviceType,
    pub battery_reserve: u8,
    pub charge_rate: u8,
    pub discharge_rate: u8,
    pub target_soc: u8,
    pub enable_charge: bool,
    pub enable_discharge: bool,
    pub inverter_serial: String,
    pub firmware_version: String,

    // -- Schedules --
    pub charge_slots: [ScheduleSlot; 3],
    pub discharge_slots: [ScheduleSlot; 2],
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
        assert_eq!(snap.charge_slots.len(), 3);
        assert_eq!(snap.discharge_slots.len(), 2);
    }
}
