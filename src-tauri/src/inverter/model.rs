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
    ///
    /// Convention matches givenergy-modbus / GivTCP: positive power =
    /// discharging (current flowing OUT of the battery), negative = charging.
    pub fn from_power(power: i32) -> Self {
        if power > 0 {
            Self::Discharging
        } else if power < 0 {
            Self::Charging
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
    PolarHybrid,
    Gen3PlusHybrid,
    PvInverter,
    ACCoupled,
    ACCoupledMk2,
    ThreePhase,
    AioCommercial,
    ACThreePhase,
    Ems,
    EmsCommercial,
    Gateway,
    AllInOne6kW,
    AllInOne3_6kW,
    AllInOne5kW,
    HybridHvGen3,
    AllInOneHybrid,
    Gen4Hybrid,
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
            // 0x20xx hybrids can only be generation-refined with ARM firmware.
            0x2001..=0x20ff => Self::Gen1Hybrid,
            0x2101..=0x21ff => Self::PolarHybrid,
            0x2201..=0x22ff => Self::Gen3PlusHybrid,
            0x2301..=0x23ff => Self::PvInverter,
            0x3001 => Self::ACCoupled,
            0x3002 => Self::ACCoupledMk2,
            0x4001..=0x40ff => Self::ThreePhase,
            0x4101..=0x41ff => Self::AioCommercial,
            0x5001..=0x50ff => Self::Ems,
            0x5101..=0x51ff => Self::EmsCommercial,
            0x6001..=0x60ff => Self::ACThreePhase,
            0x7001..=0x70ff => Self::Gateway,
            0x8001 => Self::AllInOne6kW,
            0x8002 => Self::AllInOne3_6kW,
            0x8003 => Self::AllInOne5kW,
            0x8101..=0x81ff => Self::HybridHvGen3,
            0x8201..=0x82ff => Self::AllInOneHybrid,
            0x8301..=0x83ff => Self::Gen4Hybrid,
            _ => {
                let prefix = val >> 8;
                match prefix {
                    0x10 => Self::Gen1Hybrid,
                    0x20 => Self::Gen1Hybrid,
                    0x21 => Self::PolarHybrid,
                    0x22 => Self::Gen3PlusHybrid,
                    0x23 => Self::PvInverter,
                    0x30 => Self::ACCoupled,
                    0x40 => Self::ThreePhase,
                    0x41 => Self::AioCommercial,
                    0x50 => Self::Ems,
                    0x51 => Self::EmsCommercial,
                    0x60 => Self::ACThreePhase,
                    0x70 => Self::Gateway,
                    0x80 => Self::AllInOne6kW,
                    0x81 => Self::HybridHvGen3,
                    0x82 => Self::AllInOneHybrid,
                    0x83 => Self::Gen4Hybrid,
                    _ => Self::Unknown(val),
                }
            }
        }
    }

    /// Refine the device type using the ARM firmware version.
    /// Reference behaviour for 0x20xx hybrids:
    /// ARM FW century 3 -> Gen3, 8/9 -> Gen2, anything else -> Gen1.
    pub fn refine_with_arm_fw(self, raw_dtc: u16, arm_fw: u16) -> Self {
        if raw_dtc >> 8 != 0x20 {
            return self;
        }
        match arm_fw / 100 {
            3 => Self::Gen3Hybrid,
            8 | 9 => Self::Gen2Hybrid,
            _ => Self::Gen1Hybrid,
        }
    }

    /// Nominal battery voltage for capacity calculation.
    ///
    /// Per givenergy-modbus and GivTCP references, the kWh calculation uses
    /// a fixed nominal voltage per device type, NOT the live system voltage.
    pub fn nominal_battery_voltage(&self) -> f32 {
        match self {
            Self::AllInOne6kW | Self::AllInOne3_6kW | Self::AllInOne5kW => 307.0,
            Self::ThreePhase | Self::ACThreePhase | Self::AioCommercial => 76.8,
            // Stackable HV batteries (GIV-BAT-3.4-HV modules) use 76.8V per
            // module; the capacity formula multiplies by module count. The AIO
            // and Gen4 hybrids are fixed single-unit batteries at 307.0V.
            // Note: GivTCP uses 317V for AIO family "8"; 307V matches
            // givenergy-modbus. The 3.2% difference is ~0.6 kWh on a 19.6 kWh
            // pack — negligible for displayed capacity.
            Self::HybridHvGen3 => 76.8,
            Self::AllInOneHybrid | Self::Gen4Hybrid => 307.0,
            _ => 51.2,
        }
    }

    /// Human-readable display name for the device type.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Gen1Hybrid => "Gen 1 Hybrid",
            Self::Gen2Hybrid => "Gen 2 Hybrid",
            Self::Gen3Hybrid => "Gen 3 Hybrid",
            Self::PolarHybrid => "Polar Hybrid",
            Self::Gen3PlusHybrid => "Gen 3 Plus Hybrid",
            Self::PvInverter => "PV Inverter",
            Self::ACCoupled => "AC Coupled",
            Self::ACCoupledMk2 => "AC Coupled Mk2",
            Self::ThreePhase => "Three Phase",
            Self::AioCommercial => "AIO Commercial",
            Self::ACThreePhase => "AC Three Phase",
            Self::Ems => "EMS",
            Self::EmsCommercial => "EMS Commercial",
            Self::Gateway => "Gateway",
            Self::AllInOne6kW => "All-in-One 6kW",
            Self::AllInOne3_6kW => "All-in-One 3.6kW",
            Self::AllInOne5kW => "All-in-One 5kW",
            Self::HybridHvGen3 => "Hybrid HV Gen3",
            Self::AllInOneHybrid => "All-in-One Hybrid",
            Self::Gen4Hybrid => "Gen 4 Hybrid",
            Self::Unknown(_) => "Unknown",
        }
    }

    /// Maximum battery charge/discharge power in watts.
    ///
    /// Per GivTCP source code, the inverter hardware limits the DC battery
    /// charge/discharge rate regardless of what the register says.
    pub fn max_battery_power_w(&self) -> u32 {
        match self {
            Self::Gen1Hybrid | Self::PolarHybrid | Self::Gen3PlusHybrid | Self::PvInverter => 2600,
            Self::Gen2Hybrid | Self::Gen3Hybrid => 3600,
            Self::ACCoupled | Self::ACCoupledMk2 => 3000,
            Self::ThreePhase | Self::AioCommercial | Self::ACThreePhase => 6000,
            Self::AllInOne6kW => 6000,
            Self::AllInOne3_6kW => 3600,
            Self::AllInOne5kW => 5000,
            Self::HybridHvGen3 | Self::AllInOneHybrid | Self::Gen4Hybrid => 6000,
            Self::Ems | Self::EmsCommercial | Self::Gateway | Self::Unknown(_) => 0,
        }
    }

    /// Maximum AC output power in watts (coarse fallback by model family).
    pub fn max_ac_power_w(&self) -> u32 {
        match self {
            Self::Gen1Hybrid | Self::Gen2Hybrid | Self::Gen3Hybrid => 5000,
            Self::PolarHybrid | Self::Gen3PlusHybrid | Self::PvInverter => 5000,
            Self::ACCoupled => 3000,
            Self::ACCoupledMk2 => 3600,
            Self::ThreePhase | Self::AioCommercial | Self::ACThreePhase => 6000,
            Self::Gateway => 12000,
            Self::AllInOne6kW | Self::HybridHvGen3 | Self::AllInOneHybrid | Self::Gen4Hybrid => {
                6000
            }
            Self::AllInOne3_6kW => 3600,
            Self::AllInOne5kW => 5000,
            Self::Ems | Self::EmsCommercial | Self::Unknown(_) => 0,
        }
    }

    /// Maximum AC output power in watts from the full DTC code.
    pub fn max_ac_power_for_dtc(raw_dtc: u16, fallback: u32) -> u32 {
        match raw_dtc {
            0x2001 | 0x2101 | 0x2201 | 0x2301 => 5000,
            0x2002 | 0x2102 | 0x2202 | 0x2302 => 4600,
            0x2003 | 0x2103 | 0x2203 | 0x2303 => 3600,
            0x2104 | 0x2204 | 0x2304 => 6000,
            0x2105 | 0x2205 => 7000,
            0x2106 | 0x2206 => 8000,
            0x3001 => 3000,
            0x3002 => 3600,
            0x4001 => 6000,
            0x4002 => 8000,
            0x4003 => 10000,
            0x4004 => 11000,
            0x7001 => 12000,
            0x8001 | 0x8101 | 0x8201 | 0x8304 => 6000,
            0x8002 => 3600,
            0x8003 => 5000,
            0x8102 | 0x8202 => 8000,
            0x8103 | 0x8203 => 10000,
            0x8204 => 12000,
            _ => fallback,
        }
    }

    /// Maximum battery charge/discharge power in watts from DTC + ARM firmware.
    pub fn max_battery_power_for_dtc(raw_dtc: u16, arm_fw: u16, fallback: u32) -> u32 {
        if raw_dtc >> 8 == 0x20 {
            return if matches!(arm_fw / 100, 3 | 8 | 9) {
                3600
            } else {
                2600
            };
        }
        match raw_dtc {
            0x2101..=0x21ff => 2600,
            0x2201 => 5400,
            0x3001 | 0x3002 => 3000,
            0x8001 => 6000,
            0x8002 => 3600,
            0x8003 => 5000,
            0x8102 => 8000,
            0x8103 => 10000,
            _ => fallback,
        }
    }

    /// Whether this device supports the extended 10-slot register map.
    pub fn supports_gen3_extended(&self) -> bool {
        matches!(
            self,
            Self::Gen3Hybrid
                | Self::AllInOne6kW
                | Self::AllInOne3_6kW
                | Self::AllInOne5kW
                | Self::HybridHvGen3
                | Self::AllInOneHybrid
                | Self::Gen4Hybrid
        )
    }

    /// Whether this device uses the three-phase schedule register map.
    ///
    /// Slots 1-2 live in HR1113-1121; slots 3-10 reuse the extended
    /// HR240-299 schedule block.
    pub fn uses_three_phase_schedule_slots(&self) -> bool {
        matches!(
            self,
            Self::ThreePhase
                | Self::ACThreePhase
                | Self::AioCommercial
                | Self::HybridHvGen3
                | Self::AllInOneHybrid
                | Self::Gateway
        )
    }

    /// Whether this device uses the extended HR240-299 slot/target block.
    pub fn uses_extended_schedule_slots(&self) -> bool {
        self.supports_gen3_extended() || self.uses_three_phase_schedule_slots()
    }

    /// Maximum number of charge schedule slots this device supports.
    ///
    /// - AC Coupled, Gen1 Hybrid, and Gen2 Hybrid: **1** charge slot (HR 94-95)
    /// - Gen3/AIO/HV/Gen4 and three-phase families: up to **10** slots
    /// - Other single-phase inverters: **2** slots
    ///
    /// Gen2 hybrids physically have the HR 31-32 register pair but the firmware
    /// does not honour a second charge slot — the official GivEnergy app and
    /// GivTCP both expose only one charge slot for Gen2.
    pub fn max_charge_slots(&self) -> u8 {
        match self {
            Self::ACCoupled | Self::ACCoupledMk2 | Self::Gen1Hybrid | Self::Gen2Hybrid => 1,
            dt if dt.uses_extended_schedule_slots() => 10,
            _ => 2,
        }
    }

    /// Whether this device type is a legacy model that may need manual calibration via HR(29).
    ///
    /// This is used as a **fallback** when battery BMS firmware is unavailable (HV stacks,
    /// read failures). The primary check uses `bms_firmware < 3000` from the battery module.
    /// Gen3+ inverters and their derivatives use batteries with BMS-managed OCV auto-calibration.
    /// Batteryless devices (EMS, Gateway, PV Inverter) return false.
    pub fn supports_manual_battery_calibration(&self) -> bool {
        matches!(
            self,
            Self::Gen1Hybrid | Self::Gen2Hybrid | Self::PolarHybrid
        )
    }

    /// Maximum number of discharge schedule slots this device supports.
    ///
    /// Gen2 hybrids support 2 discharge slots (HR 56-57 and HR 44-45) unlike
    /// charge slots where only 1 is functional.
    pub fn max_discharge_slots(&self) -> u8 {
        match self {
            Self::ACCoupled | Self::ACCoupledMk2 | Self::Gen1Hybrid => 1,
            dt if dt.uses_extended_schedule_slots() => 10,
            _ => 2,
        }
    }

    /// Returns additional register blocks that should be polled for this device type,
    /// beyond the standard set (IR 0-59, HR 0-59, HR 60-119).
    pub fn extra_poll_blocks(&self) -> &'static [crate::modbus::registers::RegisterBlock] {
        use crate::modbus::registers::{
            AC_CONFIG_BLOCK, AC_EXTENDED_AND_THREE_PHASE_BLOCKS, EXTENDED_AND_AC_CONFIG_BLOCKS,
            EXTENDED_AND_THREE_PHASE_BLOCKS, EXTENDED_SLOTS_BLOCK,
        };
        match self {
            Self::ACThreePhase => AC_EXTENDED_AND_THREE_PHASE_BLOCKS,
            Self::HybridHvGen3
            | Self::AllInOneHybrid
            | Self::ThreePhase
            | Self::AioCommercial
            | Self::Gateway => EXTENDED_AND_THREE_PHASE_BLOCKS,
            Self::AllInOne6kW | Self::AllInOne3_6kW | Self::AllInOne5kW => {
                EXTENDED_AND_AC_CONFIG_BLOCKS
            }
            dt if dt.supports_gen3_extended() => &[EXTENDED_SLOTS_BLOCK],
            Self::ACCoupled | Self::ACCoupledMk2 => &[AC_CONFIG_BLOCK],
            _ => &[],
        }
    }

    /// Whether this device is a three-phase model that needs the IR(1000-1414)
    /// measurement blocks polled. These models store all real-time PV/grid/battery
    /// measurements in the 1000+ range instead of IR 0-59.
    pub fn needs_three_phase_input_blocks(&self) -> bool {
        matches!(
            self,
            Self::ThreePhase
                | Self::ACThreePhase
                | Self::AioCommercial
                | Self::HybridHvGen3
                | Self::AllInOneHybrid
        )
    }

    /// Whether this device's battery uses the HV BCU/BMU stack protocol
    /// (device addresses 0x70/0x50) rather than the LV pack protocol (0x32).
    ///
    /// Mirrors givenergy-modbus `_HV_MODELS` / `PlantCapabilities.is_hv`:
    /// coarse families "4" (HYBRID_3PH), "6" (AC_3PH) and "8" (ALL_IN_ONE and
    /// variants) all use HV stacks. AIO Commercial (family "41") is excluded —
    /// it resolves to its own specific model, not the coarse HV family.
    ///
    /// For these models the LV BMS read at 0x32 will not respond; battery
    /// temperature/capacity must come from the BCU cluster read at 0x70.
    pub fn uses_hv_battery(&self) -> bool {
        matches!(
            self,
            Self::ThreePhase
                | Self::ACThreePhase
                | Self::AllInOne6kW
                | Self::AllInOne3_6kW
                | Self::AllInOne5kW
                | Self::HybridHvGen3
                | Self::AllInOneHybrid
        )
    }

    /// Whether schedule (charge/discharge slot) writes/reads are supported for this device.
    pub fn supports_schedule_slots(&self) -> bool {
        !matches!(self, Self::Ems | Self::EmsCommercial | Self::PvInverter)
    }

    /// Preferred Modbus slave address for operational inverter register reads.
    ///
    /// Matches givenergy-modbus/GivTCP: `0x11` is canonical for detection and
    /// most models; AC-coupled and Gen1 Hybrid expose operational registers at
    /// `0x31`. Battery BMS reads remain separate at `0x32`/`0x33+`.
    pub fn preferred_read_slave_address(&self) -> u8 {
        match self {
            Self::ACCoupled | Self::ACCoupledMk2 | Self::Gen1Hybrid => 0x31,
            _ => 0x11,
        }
    }

    /// Whether this device is the GivEnergy Gateway (system controller / AC
    /// distribution hub for one or more All-in-One units).
    ///
    /// Mirrors `givenergy-modbus` `PlantCapabilities.is_gateway`: the Gateway
    /// exposes a unique aggregation Input Register bank at IR 1600–1859 (grid,
    /// PV, load, per-AIO battery SOC/power/energy, faults) instead of the
    /// standard IR 0-59 / IR 1000-1414 telemetry ranges used by hybrids and
    /// three-phase models. Detection is by DTC family 0x70xx (HR(0)) confirmed
    /// by a `GW`-prefixed serial on HR(13-17).
    pub fn needs_gateway_input_blocks(&self) -> bool {
        matches!(self, Self::Gateway)
    }

    /// Whether this device has no directly-attached battery.
    ///
    /// Used to skip the LV pack BMS read (slave 0x32) and the HV BCU/BMU stack
    /// probe (0xA0/0x70/0x50), which serve nothing meaningful on these models
    /// and only burn poll-cycle time. The Gateway aggregates battery data from
    /// its child AIO(s) in its own register bank; EMS/PvInverter simply have no
    /// battery. `Unknown` is intentionally excluded so an unidentified device
    /// still gets the standard LV probe during detection.
    pub fn is_batteryless(&self) -> bool {
        matches!(
            self,
            Self::Gateway | Self::Ems | Self::EmsCommercial | Self::PvInverter
        )
    }

    /// Whether this device exposes the Emergency Power Supply (EPS) enable
    /// register at HR 317.
    ///
    /// Mirrors givenergy-modbus `_AC_CONFIG_BLOCK_MODELS`: EPS is AC-coupled
    /// (and AC-three-phase) and All-in-One only. DC hybrids (Gen1/2/3/4,
    /// Polar, Gen3+, AIO Commercial) have no AC output stage and lack the
    /// register; writing HR 317 there is silently dropped by the firmware
    /// (or worse, corrupts unrelated state). Used by `set_eps` to refuse the
    /// write and by the frontend to hide the toggle.
    pub fn supports_eps(&self) -> bool {
        matches!(
            self,
            Self::ACCoupled
                | Self::ACCoupledMk2
                | Self::ACThreePhase
                | Self::AllInOne6kW
                | Self::AllInOne3_6kW
                | Self::AllInOne5kW
        )
    }
}

/// Serde default for max slot counts (2 = safe for all models).
fn default_max_slots() -> u8 {
    2
}

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

/// Data from one external CT clamp meter (device address 0x01-0x08).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MeterData {
    /// Device address (0x01-0x08).
    pub address: u8,
    /// Phase 1-3 voltage in V.
    pub v_phase_1: f32,
    pub v_phase_2: f32,
    pub v_phase_3: f32,
    /// Phase 1-3 current in A.
    pub i_phase_1: f32,
    pub i_phase_2: f32,
    pub i_phase_3: f32,
    /// Total current in A.
    pub i_total: f32,
    /// Phase 1-3 active power in W (signed, positive = import).
    pub p_active_phase_1: i32,
    pub p_active_phase_2: i32,
    pub p_active_phase_3: i32,
    /// Total active power in W (signed).
    pub p_active_total: i32,
    /// Total reactive power in var (signed).
    pub p_reactive_total: i32,
    /// Total apparent power in VA.
    pub p_apparent_total: i32,
    /// Power factor (0.000-1.000).
    pub pf_total: f32,
    /// Frequency in Hz.
    pub frequency: f32,
    /// Cumulative import energy in kWh.
    pub e_import_active_kwh: f32,
    /// Cumulative export energy in kWh.
    pub e_export_active_kwh: f32,
}

/// A single battery module within the battery assembly.
///
/// For LV batteries each physical battery is a "module". For HV stacks
/// (All-in-One, HV Gen3) a module is a BMU within the BCU stack.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
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
    /// Raw LV BMS status/warning registers IR 90-94.
    #[serde(default)]
    pub bms_status_registers: Vec<u16>,
    /// Raw LV BMS status bytes status_1..status_7 split from IR 90-93.
    #[serde(default)]
    pub bms_status: Vec<u8>,
    /// Raw LV BMS warning bytes warning_1..warning_2 split from IR 94.
    #[serde(default)]
    pub bms_warnings: Vec<u8>,
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
    /// Target SOC (from separate register, min 4 to protect battery).
    #[serde(default = "default_target_soc")]
    pub target_soc: u8,
}

fn default_target_soc() -> u8 {
    4
}

fn default_soc_reserve() -> u8 {
    4
}

fn default_grid_online() -> bool {
    true
}

fn default_grid_loss() -> bool {
    false
}

fn default_inverter_trip() -> bool {
    false
}

fn default_battery_over_temp() -> bool {
    false
}

impl Default for ScheduleSlot {
    fn default() -> Self {
        Self {
            enabled: false,
            start_hour: 0,
            start_minute: 0,
            end_hour: 0,
            end_minute: 0,
            target_soc: 4,
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
    /// Instantaneous Emergency Power Supply (EPS) output power in watts
    /// (IR(31) `p_backup`).
    ///
    /// Only meaningful on device families with an AC output stage that
    /// supports EPS mode (see [`DeviceType::supports_eps`]) — single-phase
    /// AC-coupled (3001/3002) and residential All-in-One (80xx). Other
    /// families poll IR 1000-1414 instead of IR 0-59, where EPS
    /// telemetry lives at IR 1180-1239 (not currently captured). On those
    /// models this field stays at its default of 0.
    ///
    /// The reference library (`givenergy-modbus` `p_backup`, max=50000) and
    /// GivTCP (`p_eps_backup`) both treat the register as uint16, so we
    /// also store it as a non-negative value: 0 when EPS is idle or
    /// grid-connected, >0 when feeding the backup loads during an outage.
    /// Sanitized against a 10 kW residential ceiling; values exceeding the
    /// int16 saturation fingerprint (≥32767) are treated as dongle
    /// corruption and replaced with the previous reading.
    #[serde(default)]
    pub eps_power_w: u32,

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
    /// True when the inverter reports a live grid AC reference.
    ///
    /// Kept separate from the sanitized voltage/frequency values so a genuine
    /// power cut is not hidden by corruption filtering that carries previous
    /// readings forward.
    #[serde(default = "default_grid_online")]
    pub grid_online: bool,
    /// True when grid power is lost: the inverter's `system_mode` (IR(49))
    /// reports OFF_GRID, and/or the fault/status word reports `No Utility`.
    #[serde(default = "default_grid_loss")]
    pub grid_loss: bool,
    /// True when the inverter reports itself in a fault/trip state
    /// (IR(0) `status` == FAULT).
    #[serde(default = "default_inverter_trip")]
    pub inverter_trip: bool,
    /// True when the inverter reports battery over-temperature
    /// (IR(57) `charger_warning_code` == 1).
    #[serde(default = "default_battery_over_temp")]
    pub battery_over_temp: bool,

    // -- Inverter --
    pub inverter_temperature: f32,
    /// Inverter wall-clock time read directly from HR(35-40), formatted as
    /// `YYYY-MM-DD HH:MM:SS` without applying any timezone conversion.
    #[serde(default)]
    pub inverter_time: String,

    // -- Energy totals (kWh) --
    pub today_solar_kwh: f32,
    pub today_import_kwh: f32,
    pub today_export_kwh: f32,
    pub today_charge_kwh: f32,
    pub today_discharge_kwh: f32,
    /// Derived balance (solar + import - export - ac_charge). NOT a true
    /// cumulative counter — can legitimately decrease when the battery
    /// continues AC-charging from the grid after solar stops. Prefer
    /// [`home_energy_today_kwh`] for user-facing consumption.
    pub today_consumption_kwh: f32,
    /// Cumulative home energy consumption today (kWh), integrated from
    /// [`home_power`] by the sanitizer. Always monotonic during the day;
    /// resets to 0 on midnight rollover or a long poll gap.
    pub home_energy_today_kwh: f32,
    /// Lifetime total import from grid (kWh).
    /// Single-phase: IR(32-33) e_grid_in_total (uint32 /10 kWh)
    /// Three-phase:  IR(1382-1383) e_import_total (uint32 /10 kWh)
    pub total_import_kwh: f32,
    /// Lifetime total export to grid (kWh).
    /// Single-phase: IR(21-22) e_grid_out_total (uint32 /10 kWh)
    /// Three-phase:  IR(1386-1387) e_export_total (uint32 /10 kWh)
    pub total_export_kwh: f32,
    /// Lifetime total solar generation (kWh).
    /// Single-phase: IR(11-12) e_pv_total (uint32 /10 kWh)
    /// Three-phase:  IR(1374-1375) e_pv_total (uint32 /10 kWh)
    pub total_solar_kwh: f32,
    /// Lifetime total battery charge energy (kWh).
    /// Single-phase: IR(181) e_battery_charge_total_alt1 (deci)
    /// Three-phase:  IR(1394-1395) e_battery_charge_total (uint32 /10 kWh)
    pub total_charge_kwh: f32,
    /// Lifetime total battery discharge energy (kWh).
    /// Single-phase: IR(180) e_battery_discharge_total_alt1 (deci)
    /// Three-phase:  IR(1390-1391) e_battery_discharge_total (uint32 /10 kWh)
    pub total_discharge_kwh: f32,
    /// Lifetime total battery throughput energy (kWh).
    /// Single-phase: IR(6-7) e_battery_throughput (uint32 /10 kWh)
    /// Three-phase:  Sum of charge + discharge totals (IR 1390-1395)
    pub total_throughput_kwh: f32,
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
    /// Battery SOC reserve (HR 110 / HR 1109), clamped to min 4 to protect battery.
    #[serde(default = "default_soc_reserve")]
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
    /// Charge target SOC (HR 116 / HR 1111), clamped to min 4 to protect battery.
    #[serde(default = "default_soc_reserve")]
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
    /// True when the Agile Octopus state machine is actively force-charging or
    /// force-discharging the battery.
    pub agile_active: bool,
    /// Current Agile Octopus state: "idle", "charging", or "discharging".
    #[serde(default)]
    pub agile_state: String,
    /// True when Agile Octopus mode is enabled in settings (may be between
    /// price thresholds).
    #[serde(default)]
    pub agile_enabled: bool,
    /// Battery calibration stage (0=off, 5=balance). Only meaningful for legacy Gen1/Gen2/Polar devices.
    #[serde(default)]
    pub battery_calibration_stage: u8,
    /// True when the load discharge limiter is actively pausing discharge.
    #[serde(default)]
    pub load_limiter_active: bool,
    /// Whether this device supports manual battery calibration via HR(29).
    /// False for Gen3+ (auto-calibrates via BMS) and batteryless devices.
    #[serde(default)]
    pub supports_battery_calibration: bool,
    pub inverter_serial: String,
    /// ARM firmware version (HR(21)). For 0x20xx hybrids the century
    /// (`arm_fw / 100`) determines generation: 3 → Gen3, 8/9 → Gen2,
    /// anything else → Gen1.
    pub firmware_version: String,
    /// DSP firmware version (HR(19)). Shown for diagnostic purposes alongside
    /// the ARM firmware — the two chips run independently and mismatched
    /// versions can indicate a partial firmware update.
    #[serde(default)]
    pub dsp_firmware_version: String,
    /// DC-side DSP firmware version (three-phase only, IR(1326)).
    #[serde(default)]
    pub dc_dsp_firmware_version: String,

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

    // -- External CT configuration (single-phase only) --
    /// Whether the external CT ammeter is enabled — HR(7).
    #[serde(default)]
    pub enable_ammeter: bool,
    /// Whether the CT clamp is installed reversed — HR(42).
    #[serde(default)]
    pub enable_reversed_ct_clamp: bool,
    /// Installed external meter type — HR(47).
    #[serde(default)]
    pub meter_type: u8,

    // -- External CT meters --
    /// Detected external clamp meters (device addresses 0x01-0x08).
    #[serde(default)]
    pub meters: Vec<MeterData>,

    // -- Gateway-specific (unpopulated on every other device) --
    //
    // The GivEnergy Gateway (DTC 0x70xx) aggregates telemetry from its child
    // All-in-One unit(s) in a dedicated Input Register bank (IR 1600–1859).
    // These fields are populated only for `DeviceType::Gateway`; all default
    // to empty/zero otherwise. See `gateway-design/gateway-register-reference.md`.
    /// Number of AIOs configured in the stack (1–3) — IR(1700).
    #[serde(default)]
    pub parallel_aio_count: u8,
    /// Number of AIOs currently online — IR(1701).
    #[serde(default)]
    pub parallel_aio_online: u8,
    /// Per-AIO state of charge % — IR(1801-1803). 0 if the slot is absent.
    #[serde(default)]
    pub per_aio_soc: [u16; 3],
    /// Per-AIO inverter power (W, GivEnergy sign: + = discharging/out).
    /// IR(1816-1818). 0 if the slot is absent.
    #[serde(default)]
    pub per_aio_power: [i32; 3],
    /// Per-AIO battery charge energy today (kWh) — IR(1705/1708/1711).
    #[serde(default)]
    pub per_aio_charge_today_kwh: [f32; 3],
    /// Per-AIO battery discharge energy today (kWh) — IR(1750/1753/1756).
    #[serde(default)]
    pub per_aio_discharge_today_kwh: [f32; 3],
    /// Per-AIO serial number (Latin-1) — IR(1831+/1841+, variant-dependent).
    #[serde(default)]
    pub per_aio_serial: [String; 3],
    /// Gateway firmware version string (e.g. "GA000009") — IR(1600-1603).
    #[serde(default)]
    pub gateway_software_version: String,
    /// Gateway firmware variant flag: true when IR(1603) >= 10 (GA000010+).
    #[serde(default)]
    pub gateway_is_v2: bool,
    /// Gateway work mode enum (2 = On Grid) — IR(1604).
    #[serde(default)]
    pub gateway_work_mode: u16,
    /// Decoded gateway fault names from the 32-bit bitmask — IR(1622-1623).
    #[serde(default)]
    pub gateway_fault_codes: Vec<String>,
    /// Serial of the primary (master) AIO — IR(1627-1631).
    #[serde(default)]
    pub first_inverter_serial: String,
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
        // Negative power = charging (power flowing INTO battery).
        assert_eq!(BatteryState::from_power(-1), BatteryState::Charging);
        assert_eq!(BatteryState::from_power(-800), BatteryState::Charging);
    }

    #[test]
    fn battery_state_discharging() {
        // Positive power = discharging (power flowing OUT of battery).
        assert_eq!(BatteryState::from_power(1), BatteryState::Discharging);
        assert_eq!(BatteryState::from_power(500), BatteryState::Discharging);
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
    fn device_type_20xx_defaults_to_gen1_until_refined() {
        assert_eq!(DeviceType::from_register(0x2001), DeviceType::Gen1Hybrid);
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
    fn device_type_gen1_legacy_code_not_arm_refined() {
        let dt = DeviceType::from_register(0x1001).refine_with_arm_fw(0x1001, 352);
        assert_eq!(dt, DeviceType::Gen1Hybrid);
    }

    #[test]
    fn device_type_polar_hybrid() {
        assert_eq!(DeviceType::from_register(0x2101), DeviceType::PolarHybrid);
    }

    #[test]
    fn device_type_gen3_plus_hybrid() {
        assert_eq!(
            DeviceType::from_register(0x2201),
            DeviceType::Gen3PlusHybrid
        );
    }

    #[test]
    fn device_type_ac_mk2() {
        assert_eq!(DeviceType::from_register(0x3002), DeviceType::ACCoupledMk2);
    }

    #[test]
    fn device_type_20xx_refines_to_gen1_for_unmapped_arm_fw() {
        let dt = DeviceType::from_register(0x2001).refine_with_arm_fw(0x2001, 3);
        assert_eq!(dt, DeviceType::Gen1Hybrid);
    }

    #[test]
    fn device_type_20xx_refines_to_gen3_for_arm_fw_century_3() {
        let dt = DeviceType::from_register(0x2001).refine_with_arm_fw(0x2001, 352);
        assert_eq!(dt, DeviceType::Gen3Hybrid);
    }

    #[test]
    fn device_type_20xx_refines_to_gen2_for_arm_fw_century_8_or_9() {
        let dt = DeviceType::from_register(0x2001).refine_with_arm_fw(0x2001, 852);
        assert_eq!(dt, DeviceType::Gen2Hybrid);
        let dt = DeviceType::from_register(0x2001).refine_with_arm_fw(0x2001, 952);
        assert_eq!(dt, DeviceType::Gen2Hybrid);
    }

    #[test]
    fn device_type_ac_unaffected_by_arm_fw() {
        let dt = DeviceType::from_register(0x3001).refine_with_arm_fw(0x3001, 130);
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
        assert_eq!(slot.target_soc, 4);
    }

    #[test]
    fn snapshot_default() {
        let snap = InverterSnapshot::default();
        assert_eq!(snap.timestamp, 0);
        assert_eq!(snap.solar_power, 0);
        assert_eq!(snap.charge_slots.len(), 10);
        assert_eq!(snap.discharge_slots.len(), 10);
    }

    // -- DeviceType: known DTC families from references ---------------------
    #[test]
    fn device_type_all_known_dtc_families() {
        let cases: &[(u16, DeviceType, &str, f32)] = &[
            (0x1001, DeviceType::Gen1Hybrid, "Gen 1 Hybrid", 51.2),
            (0x2001, DeviceType::Gen1Hybrid, "Gen 1 Hybrid", 51.2),
            (0x2101, DeviceType::PolarHybrid, "Polar Hybrid", 51.2),
            (
                0x2201,
                DeviceType::Gen3PlusHybrid,
                "Gen 3 Plus Hybrid",
                51.2,
            ),
            (0x2301, DeviceType::PvInverter, "PV Inverter", 51.2),
            (0x3001, DeviceType::ACCoupled, "AC Coupled", 51.2),
            (0x3002, DeviceType::ACCoupledMk2, "AC Coupled Mk2", 51.2),
            (0x4001, DeviceType::ThreePhase, "Three Phase", 76.8),
            (0x4101, DeviceType::AioCommercial, "AIO Commercial", 76.8),
            (0x5001, DeviceType::Ems, "EMS", 51.2),
            (0x5101, DeviceType::EmsCommercial, "EMS Commercial", 51.2),
            (0x6001, DeviceType::ACThreePhase, "AC Three Phase", 76.8),
            (0x7001, DeviceType::Gateway, "Gateway", 51.2),
            (0x8001, DeviceType::AllInOne6kW, "All-in-One 6kW", 307.0),
            (0x8002, DeviceType::AllInOne3_6kW, "All-in-One 3.6kW", 307.0),
            (0x8003, DeviceType::AllInOne5kW, "All-in-One 5kW", 307.0),
            (0x8102, DeviceType::HybridHvGen3, "Hybrid HV Gen3", 76.8),
            (
                0x8204,
                DeviceType::AllInOneHybrid,
                "All-in-One Hybrid",
                307.0,
            ),
            (0x8304, DeviceType::Gen4Hybrid, "Gen 4 Hybrid", 307.0),
        ];
        for (code, expected_type, expected_name, expected_voltage) in cases {
            let dt = DeviceType::from_register(*code);
            assert_eq!(dt, *expected_type, "type mismatch for 0x{:04X}", code);
            assert_eq!(
                dt.display_name(),
                *expected_name,
                "name mismatch for 0x{:04X}",
                code
            );
            assert!((dt.nominal_battery_voltage() - expected_voltage).abs() < 0.01);
        }
    }

    #[test]
    fn dtc_specific_ac_power_table_matches_reference() {
        let cases: &[(u16, u32)] = &[
            (0x2001, 5000),
            (0x2002, 4600),
            (0x2003, 3600),
            (0x2101, 5000),
            (0x2102, 4600),
            (0x2103, 3600),
            (0x2104, 6000),
            (0x2105, 7000),
            (0x2106, 8000),
            (0x2201, 5000),
            (0x2202, 4600),
            (0x2203, 3600),
            (0x2204, 6000),
            (0x2205, 7000),
            (0x2206, 8000),
            (0x2301, 5000),
            (0x2302, 4600),
            (0x2303, 3600),
            (0x2304, 6000),
            (0x3001, 3000),
            (0x3002, 3600),
            (0x4001, 6000),
            (0x4002, 8000),
            (0x4003, 10000),
            (0x4004, 11000),
            (0x7001, 12000),
            (0x8001, 6000),
            (0x8002, 3600),
            (0x8003, 5000),
            (0x8101, 6000),
            (0x8102, 8000),
            (0x8103, 10000),
            (0x8201, 6000),
            (0x8202, 8000),
            (0x8203, 10000),
            (0x8204, 12000),
            (0x8304, 6000),
        ];
        for (code, expected) in cases {
            assert_eq!(
                DeviceType::max_ac_power_for_dtc(*code, 0),
                *expected,
                "0x{:04X}",
                code
            );
        }
    }

    #[test]
    fn dtc_specific_battery_power_table_matches_reference() {
        assert_eq!(DeviceType::max_battery_power_for_dtc(0x2001, 3, 0), 2600);
        assert_eq!(DeviceType::max_battery_power_for_dtc(0x2001, 352, 0), 3600);
        assert_eq!(DeviceType::max_battery_power_for_dtc(0x2001, 852, 0), 3600);
        assert_eq!(DeviceType::max_battery_power_for_dtc(0x2101, 0, 0), 2600);
        assert_eq!(DeviceType::max_battery_power_for_dtc(0x2201, 0, 0), 5400);
        assert_eq!(DeviceType::max_battery_power_for_dtc(0x3002, 0, 0), 3000);
        assert_eq!(DeviceType::max_battery_power_for_dtc(0x8002, 0, 0), 3600);
        assert_eq!(DeviceType::max_battery_power_for_dtc(0x8103, 0, 0), 10000);
    }

    #[test]
    fn device_type_unknown_fallbacks() {
        let dt = DeviceType::Unknown(0x9999);
        assert_eq!(dt.display_name(), "Unknown");
        assert_eq!(dt.max_battery_power_w(), 0);
        assert_eq!(dt.max_ac_power_w(), 0);
    }

    #[test]
    fn device_type_prefix_fallback() {
        assert_eq!(DeviceType::from_register(0x1002), DeviceType::Gen1Hybrid);
        assert_eq!(DeviceType::from_register(0x2099), DeviceType::Gen1Hybrid);
        assert_eq!(DeviceType::from_register(0x2199), DeviceType::PolarHybrid);
        assert_eq!(
            DeviceType::from_register(0x2299),
            DeviceType::Gen3PlusHybrid
        );
        assert_eq!(DeviceType::from_register(0x3099), DeviceType::ACCoupled);
        assert_eq!(DeviceType::from_register(0x4099), DeviceType::ThreePhase);
        assert_eq!(DeviceType::from_register(0x8099), DeviceType::AllInOne6kW);
        assert_eq!(DeviceType::from_register(0x8199), DeviceType::HybridHvGen3);
    }

    #[test]
    fn three_phase_schedule_models_expose_10_slots_and_required_blocks() {
        use crate::modbus::registers::{EXTENDED_SLOTS_BLOCK, THREE_PHASE_CONFIG_BLOCK};

        for dt in [
            DeviceType::ThreePhase,
            DeviceType::AioCommercial,
            DeviceType::ACThreePhase,
            DeviceType::HybridHvGen3,
            DeviceType::AllInOneHybrid,
            DeviceType::Gateway,
        ] {
            assert!(
                dt.supports_schedule_slots(),
                "{dt:?} should expose schedules"
            );
            assert!(
                dt.uses_three_phase_schedule_slots(),
                "{dt:?} should use the three-phase HR1113+ slot map"
            );
            assert_eq!(dt.max_charge_slots(), 10, "{dt:?} charge slot count");
            assert_eq!(dt.max_discharge_slots(), 10, "{dt:?} discharge slot count");

            let extras = dt.extra_poll_blocks();
            assert!(
                extras
                    .iter()
                    .any(|b| b.start == THREE_PHASE_CONFIG_BLOCK.start),
                "{dt:?} should poll HR1080-1124"
            );
            assert!(
                extras.iter().any(|b| b.start == EXTENDED_SLOTS_BLOCK.start),
                "{dt:?} should poll HR240-299 for slots 3-10"
            );
        }
    }

    #[test]
    fn residential_aio_polls_extended_slots_and_ac_config_but_not_three_phase_blocks() {
        use crate::modbus::registers::{
            AC_CONFIG_BLOCK, EXTENDED_SLOTS_BLOCK, THREE_PHASE_CONFIG_BLOCK,
            THREE_PHASE_HIGH_CONFIG_BLOCK,
        };

        for dt in [
            DeviceType::AllInOne6kW,
            DeviceType::AllInOne3_6kW,
            DeviceType::AllInOne5kW,
        ] {
            assert!(
                dt.supports_gen3_extended(),
                "{dt:?} should support extended slots"
            );
            assert!(
                !dt.uses_three_phase_schedule_slots(),
                "{dt:?} is residential single-phase AIO"
            );
            assert!(
                !dt.needs_three_phase_input_blocks(),
                "{dt:?} must not poll IR1000+"
            );

            let extras = dt.extra_poll_blocks();
            assert!(
                extras.iter().any(|b| b.start == EXTENDED_SLOTS_BLOCK.start),
                "{dt:?} should poll HR240-299 for extended slots"
            );
            assert!(
                extras.iter().any(|b| b.start == AC_CONFIG_BLOCK.start),
                "{dt:?} should poll HR300-359 for AC-output config"
            );
            assert!(
                !extras
                    .iter()
                    .any(|b| b.start == THREE_PHASE_CONFIG_BLOCK.start),
                "{dt:?} should not poll HR1080-1124"
            );
            assert!(
                !extras
                    .iter()
                    .any(|b| b.start == THREE_PHASE_HIGH_CONFIG_BLOCK.start),
                "{dt:?} should not poll HR1000-1079"
            );
        }
    }

    #[test]
    fn preferred_read_slave_address_matches_reference() {
        assert_eq!(DeviceType::ACCoupled.preferred_read_slave_address(), 0x31);
        assert_eq!(
            DeviceType::ACCoupledMk2.preferred_read_slave_address(),
            0x31
        );
        assert_eq!(DeviceType::Gen1Hybrid.preferred_read_slave_address(), 0x31);
        assert_eq!(DeviceType::Gen2Hybrid.preferred_read_slave_address(), 0x11);
        assert_eq!(DeviceType::Gen3Hybrid.preferred_read_slave_address(), 0x11);
        assert_eq!(DeviceType::AllInOne6kW.preferred_read_slave_address(), 0x11);
    }

    #[test]
    fn uses_hv_battery_matches_reference_hv_models() {
        // Mirrors givenergy-modbus _HV_MODELS / PlantCapabilities.is_hv:
        // coarse families 4 (HYBRID_3PH), 6 (AC_3PH), 8 (ALL_IN_ONE + variants).
        assert!(DeviceType::ThreePhase.uses_hv_battery()); // 0x40xx (GIV-3HY-11)
        assert!(DeviceType::ACThreePhase.uses_hv_battery()); // 0x60xx
        assert!(DeviceType::AllInOne6kW.uses_hv_battery()); // 0x8001
        assert!(DeviceType::AllInOne3_6kW.uses_hv_battery()); // 0x8002
        assert!(DeviceType::AllInOne5kW.uses_hv_battery()); // 0x8003
        assert!(DeviceType::HybridHvGen3.uses_hv_battery()); // 0x81xx
        assert!(DeviceType::AllInOneHybrid.uses_hv_battery()); // 0x82xx

        // Gen4Hybrid (0x83xx) is treated by GivTCP as battery-less (same
        // branch as EMS/GATEWAY) — no BCU probing.
        assert!(!DeviceType::Gen4Hybrid.uses_hv_battery());

        // LV / non-HV models use the 0x32 pack protocol instead.
        assert!(!DeviceType::Gen3Hybrid.uses_hv_battery());
        assert!(!DeviceType::Gen2Hybrid.uses_hv_battery());
        assert!(!DeviceType::Gen1Hybrid.uses_hv_battery());
        assert!(!DeviceType::ACCoupled.uses_hv_battery());
        assert!(!DeviceType::ACCoupledMk2.uses_hv_battery());
        // AIO Commercial (0x41xx) resolves to its own specific model, not the
        // coarse HV family 4 — excluded per the reference.
        assert!(!DeviceType::AioCommercial.uses_hv_battery());
    }

    #[test]
    fn needs_gateway_input_blocks_only_for_gateway() {
        assert!(DeviceType::Gateway.needs_gateway_input_blocks());
        // No other device family should request the gateway aggregation bank.
        assert!(!DeviceType::Gen3Hybrid.needs_gateway_input_blocks());
        assert!(!DeviceType::ThreePhase.needs_gateway_input_blocks());
        assert!(!DeviceType::AllInOne6kW.needs_gateway_input_blocks());
        assert!(!DeviceType::Ems.needs_gateway_input_blocks());
    }

    #[test]
    fn is_batteryless_covers_batteryless_models() {
        // Gateway aggregates battery data from child AIOs — no direct battery.
        assert!(DeviceType::Gateway.is_batteryless());
        assert!(DeviceType::Ems.is_batteryless());
        assert!(DeviceType::EmsCommercial.is_batteryless());
        assert!(DeviceType::PvInverter.is_batteryless());

        // Devices with attached batteries must NOT be flagged batteryless.
        assert!(!DeviceType::Gen3Hybrid.is_batteryless());
        assert!(!DeviceType::AllInOne6kW.is_batteryless());
        assert!(!DeviceType::ThreePhase.is_batteryless());
        assert!(!DeviceType::ACCoupled.is_batteryless());

        // Unknown is intentionally excluded so detection still probes the
        // standard LV BMS at 0x32.
        assert!(!DeviceType::Unknown(0).is_batteryless());
    }

    // -----------------------------------------------------------------------
    // supports_eps on each device type — mirrors givenergy-modbus
    // `_AC_CONFIG_BLOCK_MODELS = {AC, AC_3PH, ALL_IN_ONE}`.
    // -----------------------------------------------------------------------

    #[test]
    fn supports_eps_for_ac_coupled_and_aio_families() {
        // AC-coupled single-phase (HR 300-359 block, HR 317 writable).
        assert!(DeviceType::ACCoupled.supports_eps());
        assert!(DeviceType::ACCoupledMk2.supports_eps());

        // AC three-phase — same AC config block at HR 300-359.
        assert!(DeviceType::ACThreePhase.supports_eps());

        // Residential All-in-One (DTC family 0x80) carries the AC output
        // stage and exposes HR 317.
        assert!(DeviceType::AllInOne6kW.supports_eps());
        assert!(DeviceType::AllInOne3_6kW.supports_eps());
        assert!(DeviceType::AllInOne5kW.supports_eps());
    }

    #[test]
    fn supports_eps_rejects_dc_hybrid_and_pure_three_phase() {
        // DC hybrids have no AC output stage — HR 317 is undefined.
        assert!(!DeviceType::Gen1Hybrid.supports_eps());
        assert!(!DeviceType::Gen2Hybrid.supports_eps());
        assert!(!DeviceType::Gen3Hybrid.supports_eps());
        assert!(!DeviceType::Gen4Hybrid.supports_eps());
        assert!(!DeviceType::PolarHybrid.supports_eps());
        assert!(!DeviceType::Gen3PlusHybrid.supports_eps());

        // Pure three-phase (no AC-coupled prefix) and AIO Commercial lack
        // HR 317 — they expose EPS telemetry in IR 1180-1239 but not the
        // enable register. Confirm via the reference library's
        // _AC_CONFIG_BLOCK_MODELS exclusion.
        assert!(!DeviceType::ThreePhase.supports_eps());
        assert!(!DeviceType::AioCommercial.supports_eps());
        assert!(!DeviceType::HybridHvGen3.supports_eps());
        assert!(!DeviceType::AllInOneHybrid.supports_eps());

        // Devices with no inverter control surface at all.
        assert!(!DeviceType::Gateway.supports_eps());
        assert!(!DeviceType::Ems.supports_eps());
        assert!(!DeviceType::EmsCommercial.supports_eps());
        assert!(!DeviceType::PvInverter.supports_eps());

        // Unknown is conservatively rejected so the API returns a clear
        // error rather than writing to a register we can't prove exists.
        assert!(!DeviceType::Unknown(0).supports_eps());
    }

    #[test]
    fn supports_eps_matches_extra_poll_blocks_for_ac_config() {
        // The set of devices that poll HR 300-359 (and therefore can read
        // back HR 317 on the next snapshot) must equal the set that supports
        // EPS, otherwise we either:
        //   - poll HR 317 on a device that ignores it (wasted cycle), or
        //   - miss the EPS state on a device that genuinely has it.
        // Models whose `extra_poll_blocks()` include `AC_CONFIG_BLOCK`:
        //   - ACCoupled, ACCoupledMk2 → &[AC_CONFIG_BLOCK]
        //   - ACThreePhase             → AC_EXTENDED_AND_THREE_PHASE_BLOCKS
        //   - AllInOne{6kW,3_6kW,5kW}  → EXTENDED_AND_AC_CONFIG_BLOCKS
        // All other families (DC hybrids, pure three-phase, AIO Commercial,
        // AIO Hybrid, HV Gen3, Gateway, EMS, PV inverter) deliberately
        // don't poll HR 300-359.
        let ac_block_models = [
            DeviceType::ACCoupled,
            DeviceType::ACCoupledMk2,
            DeviceType::ACThreePhase,
            DeviceType::AllInOne6kW,
            DeviceType::AllInOne3_6kW,
            DeviceType::AllInOne5kW,
            DeviceType::AllInOneHybrid,
            DeviceType::HybridHvGen3,
            DeviceType::ThreePhase,
            DeviceType::AioCommercial,
            DeviceType::Gateway,
        ];
        for dt in ac_block_models {
            let has_ac_config = dt
                .extra_poll_blocks()
                .iter()
                .any(|b| b.start == 300);
            assert_eq!(
                dt.supports_eps(),
                has_ac_config,
                "supports_eps / AC_CONFIG_BLOCK polling mismatch for {:?}",
                dt
            );
        }
    }

    // -----------------------------------------------------------------------
    // supports_gen3_extended on each device type
    // -----------------------------------------------------------------------

    #[test]
    fn supports_gen3_extended_for_all_device_types() {
        let extended: &[DeviceType] = &[
            DeviceType::Gen3Hybrid,
            DeviceType::AllInOne6kW,
            DeviceType::AllInOne3_6kW,
            DeviceType::AllInOne5kW,
            DeviceType::HybridHvGen3,
            DeviceType::AllInOneHybrid,
            DeviceType::Gen4Hybrid,
        ];
        for dt in extended {
            assert!(
                dt.supports_gen3_extended(),
                "{dt:?} should support gen3 extended"
            );
        }

        let non_extended: &[DeviceType] = &[
            DeviceType::Gen1Hybrid,
            DeviceType::Gen2Hybrid,
            DeviceType::PolarHybrid,
            DeviceType::Gen3PlusHybrid,
            DeviceType::PvInverter,
            DeviceType::ACCoupled,
            DeviceType::ACCoupledMk2,
            DeviceType::ThreePhase,
            DeviceType::AioCommercial,
            DeviceType::ACThreePhase,
            DeviceType::Ems,
            DeviceType::EmsCommercial,
            DeviceType::Gateway,
        ];
        for dt in non_extended {
            assert!(
                !dt.supports_gen3_extended(),
                "{dt:?} should NOT support gen3 extended"
            );
        }
    }

    // -----------------------------------------------------------------------
    // uses_three_phase_schedule_slots on each device type
    // -----------------------------------------------------------------------

    #[test]
    fn uses_three_phase_schedule_slots_for_all_device_types() {
        let three_phase: &[DeviceType] = &[
            DeviceType::ThreePhase,
            DeviceType::ACThreePhase,
            DeviceType::AioCommercial,
            DeviceType::HybridHvGen3,
            DeviceType::AllInOneHybrid,
            DeviceType::Gateway,
        ];
        for dt in three_phase {
            assert!(
                dt.uses_three_phase_schedule_slots(),
                "{dt:?} should use three-phase schedule slots"
            );
        }

        let single_phase: &[DeviceType] = &[
            DeviceType::Gen1Hybrid,
            DeviceType::Gen2Hybrid,
            DeviceType::Gen3Hybrid,
            DeviceType::PolarHybrid,
            DeviceType::Gen3PlusHybrid,
            DeviceType::PvInverter,
            DeviceType::ACCoupled,
            DeviceType::ACCoupledMk2,
            DeviceType::AllInOne6kW,
            DeviceType::AllInOne3_6kW,
            DeviceType::AllInOne5kW,
            DeviceType::Gen4Hybrid,
            DeviceType::Ems,
            DeviceType::EmsCommercial,
        ];
        for dt in single_phase {
            assert!(
                !dt.uses_three_phase_schedule_slots(),
                "{dt:?} should NOT use three-phase schedule slots"
            );
        }
    }

    // -----------------------------------------------------------------------
    // preferred_read_slave_address on each device type
    // -----------------------------------------------------------------------

    #[test]
    fn preferred_read_slave_address_for_all_device_types() {
        let slave31: &[DeviceType] = &[
            DeviceType::ACCoupled,
            DeviceType::ACCoupledMk2,
            DeviceType::Gen1Hybrid,
        ];
        for dt in slave31 {
            assert_eq!(
                dt.preferred_read_slave_address(),
                0x31,
                "{dt:?} should use slave 0x31"
            );
        }

        let slave11: &[DeviceType] = &[
            DeviceType::Gen2Hybrid,
            DeviceType::Gen3Hybrid,
            DeviceType::PolarHybrid,
            DeviceType::Gen3PlusHybrid,
            DeviceType::PvInverter,
            DeviceType::ThreePhase,
            DeviceType::AioCommercial,
            DeviceType::ACThreePhase,
            DeviceType::Ems,
            DeviceType::EmsCommercial,
            DeviceType::Gateway,
            DeviceType::AllInOne6kW,
            DeviceType::AllInOne3_6kW,
            DeviceType::AllInOne5kW,
            DeviceType::HybridHvGen3,
            DeviceType::AllInOneHybrid,
            DeviceType::Gen4Hybrid,
        ];
        for dt in slave11 {
            assert_eq!(
                dt.preferred_read_slave_address(),
                0x11,
                "{dt:?} should use slave 0x11"
            );
        }
    }

    // -----------------------------------------------------------------------
    // needs_gateway_input_blocks on gateway and non-gateway types
    // -----------------------------------------------------------------------

    #[test]
    fn needs_gateway_input_blocks_for_all_device_types() {
        assert!(DeviceType::Gateway.needs_gateway_input_blocks());

        let non_gateway: &[DeviceType] = &[
            DeviceType::Gen1Hybrid,
            DeviceType::Gen2Hybrid,
            DeviceType::Gen3Hybrid,
            DeviceType::PolarHybrid,
            DeviceType::Gen3PlusHybrid,
            DeviceType::PvInverter,
            DeviceType::ACCoupled,
            DeviceType::ACCoupledMk2,
            DeviceType::ThreePhase,
            DeviceType::AioCommercial,
            DeviceType::ACThreePhase,
            DeviceType::Ems,
            DeviceType::EmsCommercial,
            DeviceType::AllInOne6kW,
            DeviceType::AllInOne3_6kW,
            DeviceType::AllInOne5kW,
            DeviceType::HybridHvGen3,
            DeviceType::AllInOneHybrid,
            DeviceType::Gen4Hybrid,
        ];
        for dt in non_gateway {
            assert!(
                !dt.needs_gateway_input_blocks(),
                "{dt:?} should NOT need gateway input blocks"
            );
        }
    }
}
