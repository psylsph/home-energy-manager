//! Control command encoder.
//!
//! Translates high-level control commands into raw Modbus register writes.
//! Only whitelisted register addresses from SAFE_WRITE_REGS are allowed.

use chrono::{Datelike, Timelike, Utc};

use crate::modbus::registers::{
    HR_3PH_BATTERY_CHARGE_LIMIT,
    HR_3PH_BATTERY_DISCHARGE_LIMIT,
    HR_3PH_BATTERY_SOC_RESERVE,
    HR_3PH_CHARGE_TARGET_SOC,
    HR_ACTIVE_POWER_RATE,
    HR_AC_BATTERY_CHARGE_LIMIT,
    HR_AC_BATTERY_DISCHARGE_LIMIT,
    HR_BATTERY_CALIBRATION_STAGE,
    HR_BATTERY_CHARGE_LIMIT,
    HR_BATTERY_DISCHARGE_LIMIT,
    HR_BATTERY_DISCHARGE_MIN_POWER_RESERVE,
    HR_BATTERY_PAUSE_MODE,
    HR_BATTERY_PAUSE_SLOT_1_END,
    HR_BATTERY_PAUSE_SLOT_1_START,
    HR_BATTERY_POWER_MODE,
    HR_BATTERY_SOC_RESERVE,
    HR_CHARGE_SLOT_10_END,
    HR_CHARGE_SLOT_10_START,
    HR_CHARGE_SLOT_1_END,
    HR_CHARGE_SLOT_1_START,
    HR_CHARGE_SLOT_2_END,
    HR_CHARGE_SLOT_2_START,
    HR_CHARGE_SLOT_3_END,
    // Extended slots 3-10
    HR_CHARGE_SLOT_3_START,
    HR_CHARGE_SLOT_4_END,
    HR_CHARGE_SLOT_4_START,
    HR_CHARGE_SLOT_5_END,
    HR_CHARGE_SLOT_5_START,
    HR_CHARGE_SLOT_6_END,
    HR_CHARGE_SLOT_6_START,
    HR_CHARGE_SLOT_7_END,
    HR_CHARGE_SLOT_7_START,
    HR_CHARGE_SLOT_8_END,
    HR_CHARGE_SLOT_8_START,
    HR_CHARGE_SLOT_9_END,
    HR_CHARGE_SLOT_9_START,
    HR_CHARGE_TARGET_SOC,
    // Per-slot target SOCs
    HR_CHARGE_TARGET_SOC_1,
    HR_CHARGE_TARGET_SOC_10,
    HR_CHARGE_TARGET_SOC_2,
    HR_CHARGE_TARGET_SOC_3,
    HR_CHARGE_TARGET_SOC_4,
    HR_CHARGE_TARGET_SOC_5,
    HR_CHARGE_TARGET_SOC_6,
    HR_CHARGE_TARGET_SOC_7,
    HR_CHARGE_TARGET_SOC_8,
    HR_CHARGE_TARGET_SOC_9,
    HR_DISCHARGE_SLOT_10_END,
    HR_DISCHARGE_SLOT_10_START,
    HR_DISCHARGE_SLOT_1_END,
    HR_DISCHARGE_SLOT_1_START,
    HR_DISCHARGE_SLOT_2_END,
    HR_DISCHARGE_SLOT_2_START,
    HR_DISCHARGE_SLOT_3_END,
    HR_DISCHARGE_SLOT_3_START,
    HR_DISCHARGE_SLOT_4_END,
    HR_DISCHARGE_SLOT_4_START,
    HR_DISCHARGE_SLOT_5_END,
    HR_DISCHARGE_SLOT_5_START,
    HR_DISCHARGE_SLOT_6_END,
    HR_DISCHARGE_SLOT_6_START,
    HR_DISCHARGE_SLOT_7_END,
    HR_DISCHARGE_SLOT_7_START,
    HR_DISCHARGE_SLOT_8_END,
    HR_DISCHARGE_SLOT_8_START,
    HR_DISCHARGE_SLOT_9_END,
    HR_DISCHARGE_SLOT_9_START,
    HR_DISCHARGE_TARGET_SOC_1,
    HR_DISCHARGE_TARGET_SOC_10,
    HR_DISCHARGE_TARGET_SOC_2,
    HR_DISCHARGE_TARGET_SOC_3,
    HR_DISCHARGE_TARGET_SOC_4,
    HR_DISCHARGE_TARGET_SOC_5,
    HR_DISCHARGE_TARGET_SOC_6,
    HR_DISCHARGE_TARGET_SOC_7,
    HR_DISCHARGE_TARGET_SOC_8,
    HR_DISCHARGE_TARGET_SOC_9,
    HR_ENABLE_CHARGE,
    HR_ENABLE_CHARGE_TARGET,
    HR_ENABLE_DISCHARGE,
    HR_ENABLE_EPS,
    HR_ENABLE_RTC,
    HR_EXPORT_PRIORITY,
    HR_INVERTER_REBOOT,
    HR_SYSTEM_TIME_DAY,
    HR_SYSTEM_TIME_HOUR,
    HR_SYSTEM_TIME_MINUTE,
    HR_SYSTEM_TIME_MONTH,
    HR_SYSTEM_TIME_SECOND,
    HR_SYSTEM_TIME_YEAR,
    SAFE_WRITE_REGS,
};

// ---------------------------------------------------------------------------
// Write request
// ---------------------------------------------------------------------------

/// A single holding-register write (address, value).
#[derive(Debug, Clone)]
pub struct RegisterWrite {
    pub address: u16,
    pub value: u16,
}

// ---------------------------------------------------------------------------
// Control commands
// ---------------------------------------------------------------------------

/// High-level control commands that can be sent to the inverter.
#[derive(Debug, Clone)]
pub enum ControlCommand {
    /// Set battery power mode: 0=export, 1=self-consumption.
    SetBatteryPowerMode { mode: u16 },
    /// Enable or disable timed discharge.
    SetEnableDischarge { enabled: bool },
    /// Enable or disable timed charge.
    SetEnableCharge { enabled: bool },
    /// Set battery SOC reserve (4-100).
    SetBatterySocReserve { reserve: u16 },
    /// Set charge target SOC (0-100).
    SetChargeTargetSoc { soc: u16 },
    /// Exit cosy mode: disable charge, disable charge target, re-enable discharge,
    /// restore eco power mode. Puts the inverter back to normal Eco self-consumption
    /// after a cosy force-charge slot ends.
    CosyExit,
    /// Set charge slot 1 times (HHMM packed).
    SetChargeSlot1 { start: u16, end: u16 },
    /// Set charge slot 2 times (HHMM packed).
    SetChargeSlot2 { start: u16, end: u16 },
    /// Set discharge slot 1 times (HHMM packed).
    SetDischargeSlot1 { start: u16, end: u16 },
    /// Set discharge slot 2 times (HHMM packed).
    SetDischargeSlot2 { start: u16, end: u16 },
    /// Set DC-coupled hybrid battery charge limit percentage (0-50, HR 111).
    SetChargeLimit { limit: u16 },
    /// Set DC-coupled hybrid battery discharge limit percentage (0-50, HR 112).
    SetDischargeLimit { limit: u16 },
    /// Set AC-coupled battery charge limit percentage (1-100, HR 313).
    SetAcChargeLimit { limit: u16 },
    /// Set AC-coupled battery discharge limit percentage (1-100, HR 314).
    SetAcDischargeLimit { limit: u16 },
    /// Set three-phase battery charge power limit percentage (1-100, HR 1110).
    SetThreePhaseChargeLimit { limit: u16 },
    /// Set three-phase battery discharge power limit percentage (1-100, HR 1108).
    SetThreePhaseDischargeLimit { limit: u16 },
    /// Set three-phase battery SOC reserve (4-100, HR 1109).
    SetThreePhaseBatterySocReserve { reserve: u16 },
    /// Set three-phase charge target SOC (4-100, HR 1111).
    SetThreePhaseChargeTargetSoc { soc: u16 },
    /// Set inverter max output active power rate percentage (0-100).
    SetActivePowerRate { rate: u16 },
    /// Set Eco mode (self-consumption, no discharge, clear discharge slots).
    SetEcoMode { soc_reserve: u16 },
    /// Set Timed Demand mode (self-consumption + discharge).
    SetTimedDemandMode { soc_reserve: u16 },
    /// Set Timed Export mode (export + discharge).
    SetTimedExportMode { soc_reserve: u16 },
    /// Pause battery (set SOC reserve to 100).
    PauseBattery,
    /// Force charge: enable charging with target SOC and enable_charge.
    ForceCharge { target_soc: u16 },
    /// Force discharge: enable discharge with a full-day discharge slot.
    ForceDischarge,
    /// Sync inverter clock to current system time.
    SyncClock,
    /// Set battery calibration stage (0=off, 5=balance).
    SetCalibrationStage { stage: u16 },
    /// Reboot the inverter (write 100 to HR 163).
    RebootInverter,
    /// Set battery discharge min power reserve (HR 114, 4-100%).
    SetPowerReserve { reserve: u16 },
    /// Enable or disable the Real Time Clock (HR 166, persists settings to EEPROM).
    SetRtc { enabled: bool },
    /// Set export priority for AC-coupled inverters (HR 311).
    SetExportPriority { priority: u16 },
    /// Enable or disable Emergency Power Supply mode (HR 317).
    SetEps { enabled: bool },
    /// Set battery pause mode (HR 318, 0=disabled).
    SetPauseMode { mode: u16 },
    /// Set battery pause time slot (HR 319-320).
    SetPauseSlot { start: u16, end: u16 },
    /// Set per-slot charge target SOC (HR 242/245 for slots 1/2).
    SetChargeTargetSocSlot { slot: u8, soc: u16 },
    /// Set per-slot discharge target SOC (HR 272/275 for slots 1/2).
    SetDischargeTargetSocSlot { slot: u8, soc: u16 },
    /// Set charge slot N times (N=3..10, Gen3 extended).
    SetChargeSlotN { slot: u8, start: u16, end: u16 },
    /// Set discharge slot N times (N=3..10, Gen3 extended).
    SetDischargeSlotN { slot: u8, start: u16, end: u16 },
}

impl ControlCommand {
    /// Encode the command into one or more register writes.
    /// Returns an error if any target register is not in the whitelist.
    pub fn encode(&self) -> Result<Vec<RegisterWrite>, String> {
        let writes = match self {
            ControlCommand::SetBatteryPowerMode { mode } => {
                vec![rw(HR_BATTERY_POWER_MODE, *mode)]
            }
            ControlCommand::SetEnableDischarge { enabled } => {
                vec![rw(HR_ENABLE_DISCHARGE, if *enabled { 1 } else { 0 })]
            }
            ControlCommand::SetEnableCharge { enabled } => {
                vec![rw(HR_ENABLE_CHARGE, if *enabled { 1 } else { 0 })]
            }
            ControlCommand::CosyExit => {
                vec![
                    rw(HR_ENABLE_CHARGE, 0),        // stop force charge
                    rw(HR_ENABLE_CHARGE_TARGET, 0), // clear charge target
                    rw(HR_BATTERY_POWER_MODE, 1),   // eco mode
                    rw(HR_ENABLE_DISCHARGE, 1),     // allow discharge again
                ]
            }
            ControlCommand::SetBatterySocReserve { reserve } => {
                // Reference bounds: [4-100]. Below 4% causes issues.
                validate_range(*reserve, 4, 100, "SOC reserve")?;
                vec![rw(HR_BATTERY_SOC_RESERVE, *reserve)]
            }
            ControlCommand::SetChargeTargetSoc { soc } => {
                // Reference bounds: [4-100].
                validate_range(*soc, 4, 100, "target SOC")?;
                vec![
                    rw(HR_ENABLE_CHARGE_TARGET, 1),
                    rw(HR_CHARGE_TARGET_SOC, *soc),
                ]
            }
            ControlCommand::SetChargeSlot1 { start, end } => {
                vec![
                    rw(HR_CHARGE_SLOT_1_START, *start),
                    rw(HR_CHARGE_SLOT_1_END, *end),
                ]
            }
            ControlCommand::SetChargeSlot2 { start, end } => {
                vec![
                    rw(HR_CHARGE_SLOT_2_START, *start),
                    rw(HR_CHARGE_SLOT_2_END, *end),
                ]
            }
            ControlCommand::SetDischargeSlot1 { start, end } => {
                vec![
                    rw(HR_DISCHARGE_SLOT_1_START, *start),
                    rw(HR_DISCHARGE_SLOT_1_END, *end),
                ]
            }
            ControlCommand::SetDischargeSlot2 { start, end } => {
                vec![
                    rw(HR_DISCHARGE_SLOT_2_START, *start),
                    rw(HR_DISCHARGE_SLOT_2_END, *end),
                ]
            }
            ControlCommand::SetChargeLimit { limit } => {
                // Reference bounds: [0-50]. Above 50% the dongle can become
                // unresponsive — this matches the reference library's limit.
                validate_range(*limit, 0, 50, "charge limit")?;
                vec![rw(HR_BATTERY_CHARGE_LIMIT, *limit)]
            }
            ControlCommand::SetDischargeLimit { limit } => {
                // Reference bounds: [0-50]. Same reasoning as charge limit.
                validate_range(*limit, 0, 50, "discharge limit")?;
                vec![rw(HR_BATTERY_DISCHARGE_LIMIT, *limit)]
            }
            ControlCommand::SetAcChargeLimit { limit } => {
                // givenergy-modbus set_battery_charge_limit_ac bounds: [1-100].
                validate_range(*limit, 1, 100, "AC charge limit")?;
                vec![rw(HR_AC_BATTERY_CHARGE_LIMIT, *limit)]
            }
            ControlCommand::SetAcDischargeLimit { limit } => {
                // givenergy-modbus set_battery_discharge_limit_ac bounds: [1-100].
                validate_range(*limit, 1, 100, "AC discharge limit")?;
                vec![rw(HR_AC_BATTERY_DISCHARGE_LIMIT, *limit)]
            }
            ControlCommand::SetThreePhaseChargeLimit { limit } => {
                validate_range(*limit, 1, 100, "three-phase charge limit")?;
                vec![rw(HR_3PH_BATTERY_CHARGE_LIMIT, *limit)]
            }
            ControlCommand::SetThreePhaseDischargeLimit { limit } => {
                validate_range(*limit, 1, 100, "three-phase discharge limit")?;
                vec![rw(HR_3PH_BATTERY_DISCHARGE_LIMIT, *limit)]
            }
            ControlCommand::SetThreePhaseBatterySocReserve { reserve } => {
                validate_range(*reserve, 4, 100, "three-phase SOC reserve")?;
                vec![rw(HR_3PH_BATTERY_SOC_RESERVE, *reserve)]
            }
            ControlCommand::SetThreePhaseChargeTargetSoc { soc } => {
                validate_range(*soc, 4, 100, "three-phase target SOC")?;
                vec![rw(HR_3PH_CHARGE_TARGET_SOC, *soc)]
            }
            ControlCommand::SetActivePowerRate { rate } => {
                validate_range(*rate, 0, 100, "active power rate")?;
                vec![rw(HR_ACTIVE_POWER_RATE, *rate)]
            }
            ControlCommand::SetEcoMode { soc_reserve } => {
                validate_range(*soc_reserve, 4, 100, "SOC reserve")?;
                // Preserve discharge slot times. The reference set_mode_dynamic()
                // disables timed discharge via HR 59 only; clearing slot registers
                // loses the user's configured schedule.
                vec![
                    rw(HR_BATTERY_POWER_MODE, 1), // self-consumption
                    rw(HR_ENABLE_DISCHARGE, 0),   // no timed discharge
                    rw(HR_BATTERY_SOC_RESERVE, *soc_reserve),
                ]
            }
            ControlCommand::SetTimedDemandMode { soc_reserve } => {
                validate_range(*soc_reserve, 4, 100, "SOC reserve")?;
                vec![
                    rw(HR_BATTERY_POWER_MODE, 1), // self-consumption
                    rw(HR_ENABLE_DISCHARGE, 1),   // enable timed discharge
                    rw(HR_BATTERY_SOC_RESERVE, *soc_reserve),
                ]
            }
            ControlCommand::SetTimedExportMode { soc_reserve } => {
                validate_range(*soc_reserve, 4, 100, "SOC reserve")?;
                vec![
                    rw(HR_BATTERY_POWER_MODE, 0), // export mode
                    rw(HR_ENABLE_DISCHARGE, 1),   // enable timed discharge
                    rw(HR_BATTERY_SOC_RESERVE, *soc_reserve),
                ]
            }
            ControlCommand::PauseBattery => {
                vec![rw(HR_BATTERY_SOC_RESERVE, 100)]
            }
            ControlCommand::ForceCharge { target_soc } => {
                validate_range(*target_soc, 4, 100, "target SOC")?;
                // NOTE: we do NOT clear the charge slot registers here.
                // The inverter handles priority internally, and writing 4
                // extra registers (HR 94/95/31/32) adds unnecessary Modbus
                // traffic that can trigger function code mismatches and
                // timeouts on slow dongles (observed with Gen1/AC inverters).
                // ForceCharge only needs eco mode + charge flags + target SOC.
                vec![
                    rw(HR_BATTERY_POWER_MODE, 1), // eco mode — required for charge to work
                    rw(HR_ENABLE_CHARGE, 1),
                    rw(HR_ENABLE_CHARGE_TARGET, 1),
                    rw(HR_CHARGE_TARGET_SOC, *target_soc),
                ]
            }
            ControlCommand::ForceDischarge => {
                vec![
                    rw(HR_ENABLE_DISCHARGE, 1),
                    rw(HR_DISCHARGE_SLOT_1_START, 0),  // 00:00
                    rw(HR_DISCHARGE_SLOT_1_END, 2359), // 23:59
                ]
            }
            ControlCommand::SyncClock => {
                let now = Utc::now();
                vec![
                    rw(HR_SYSTEM_TIME_YEAR, (now.year() - 2000) as u16),
                    rw(HR_SYSTEM_TIME_MONTH, now.month() as u16),
                    rw(HR_SYSTEM_TIME_DAY, now.day() as u16),
                    rw(HR_SYSTEM_TIME_HOUR, now.hour() as u16),
                    rw(HR_SYSTEM_TIME_MINUTE, now.minute() as u16),
                    rw(HR_SYSTEM_TIME_SECOND, now.second() as u16),
                ]
            }
            ControlCommand::SetCalibrationStage { stage } => {
                validate_range(*stage, 0, 7, "calibration stage")?;
                vec![rw(HR_BATTERY_CALIBRATION_STAGE, *stage)]
            }
            ControlCommand::RebootInverter => {
                vec![rw(HR_INVERTER_REBOOT, 100)]
            }
            ControlCommand::SetPowerReserve { reserve } => {
                // HR 114: battery discharge min power reserve (4-100%).
                // Distinct from HR 110 (SOC reserve) — this prevents discharge
                // below the reserve level even in timed modes.
                validate_range(*reserve, 4, 100, "power reserve")?;
                vec![rw(HR_BATTERY_DISCHARGE_MIN_POWER_RESERVE, *reserve)]
            }
            ControlCommand::SetRtc { enabled } => {
                vec![rw(HR_ENABLE_RTC, if *enabled { 1 } else { 0 })]
            }
            ControlCommand::SetExportPriority { priority } => {
                validate_range(*priority, 0, 2, "export priority")?;
                vec![rw(HR_EXPORT_PRIORITY, *priority)]
            }
            ControlCommand::SetEps { enabled } => {
                vec![rw(HR_ENABLE_EPS, if *enabled { 1 } else { 0 })]
            }
            ControlCommand::SetPauseMode { mode } => {
                vec![rw(HR_BATTERY_PAUSE_MODE, *mode)]
            }
            ControlCommand::SetPauseSlot { start, end } => {
                vec![
                    rw(HR_BATTERY_PAUSE_SLOT_1_START, *start),
                    rw(HR_BATTERY_PAUSE_SLOT_1_END, *end),
                ]
            }
            ControlCommand::SetChargeTargetSocSlot { slot, soc } => {
                validate_range(*soc, 0, 100, "per-slot target SOC")?;
                let reg = charge_target_soc_for_slot(*slot)?;
                vec![rw(reg, *soc)]
            }
            ControlCommand::SetDischargeTargetSocSlot { slot, soc } => {
                validate_range(*soc, 0, 100, "per-slot discharge target SOC")?;
                let reg = discharge_target_soc_for_slot(*slot)?;
                vec![rw(reg, *soc)]
            }
            ControlCommand::SetChargeSlotN { slot, start, end } => {
                let (s, e) = extended_charge_slot(*slot)?;
                vec![rw(s, *start), rw(e, *end)]
            }
            ControlCommand::SetDischargeSlotN { slot, start, end } => {
                let (s, e) = extended_discharge_slot(*slot)?;
                vec![rw(s, *start), rw(e, *end)]
            }
        };

        // Validate all addresses are in the whitelist
        for w in &writes {
            if !SAFE_WRITE_REGS.contains(&w.address) {
                return Err(format!(
                    "register address {} not in safe write list",
                    w.address
                ));
            }
        }

        Ok(writes)
    }
}

fn rw(address: u16, value: u16) -> RegisterWrite {
    RegisterWrite { address, value }
}

fn validate_range(val: u16, min: u16, max: u16, name: &str) -> Result<(), String> {
    if val < min || val > max {
        Err(format!("{} must be {}-{}, got {}", name, min, max, val))
    } else {
        Ok(())
    }
}

/// Map slot index (1-10) to charge target SOC register.
fn charge_target_soc_for_slot(slot: u8) -> Result<u16, String> {
    match slot {
        1 => Ok(HR_CHARGE_TARGET_SOC_1),
        2 => Ok(HR_CHARGE_TARGET_SOC_2),
        3 => Ok(HR_CHARGE_TARGET_SOC_3),
        4 => Ok(HR_CHARGE_TARGET_SOC_4),
        5 => Ok(HR_CHARGE_TARGET_SOC_5),
        6 => Ok(HR_CHARGE_TARGET_SOC_6),
        7 => Ok(HR_CHARGE_TARGET_SOC_7),
        8 => Ok(HR_CHARGE_TARGET_SOC_8),
        9 => Ok(HR_CHARGE_TARGET_SOC_9),
        10 => Ok(HR_CHARGE_TARGET_SOC_10),
        _ => Err(format!("Charge target SOC slot must be 1-10, got {}", slot)),
    }
}

/// Map slot index (1-10) to discharge target SOC register.
fn discharge_target_soc_for_slot(slot: u8) -> Result<u16, String> {
    match slot {
        1 => Ok(HR_DISCHARGE_TARGET_SOC_1),
        2 => Ok(HR_DISCHARGE_TARGET_SOC_2),
        3 => Ok(HR_DISCHARGE_TARGET_SOC_3),
        4 => Ok(HR_DISCHARGE_TARGET_SOC_4),
        5 => Ok(HR_DISCHARGE_TARGET_SOC_5),
        6 => Ok(HR_DISCHARGE_TARGET_SOC_6),
        7 => Ok(HR_DISCHARGE_TARGET_SOC_7),
        8 => Ok(HR_DISCHARGE_TARGET_SOC_8),
        9 => Ok(HR_DISCHARGE_TARGET_SOC_9),
        10 => Ok(HR_DISCHARGE_TARGET_SOC_10),
        _ => Err(format!(
            "Discharge target SOC slot must be 1-10, got {}",
            slot
        )),
    }
}

/// Map slot index (3-10) to extended charge slot register pair.
fn extended_charge_slot(slot: u8) -> Result<(u16, u16), String> {
    match slot {
        3 => Ok((HR_CHARGE_SLOT_3_START, HR_CHARGE_SLOT_3_END)),
        4 => Ok((HR_CHARGE_SLOT_4_START, HR_CHARGE_SLOT_4_END)),
        5 => Ok((HR_CHARGE_SLOT_5_START, HR_CHARGE_SLOT_5_END)),
        6 => Ok((HR_CHARGE_SLOT_6_START, HR_CHARGE_SLOT_6_END)),
        7 => Ok((HR_CHARGE_SLOT_7_START, HR_CHARGE_SLOT_7_END)),
        8 => Ok((HR_CHARGE_SLOT_8_START, HR_CHARGE_SLOT_8_END)),
        9 => Ok((HR_CHARGE_SLOT_9_START, HR_CHARGE_SLOT_9_END)),
        10 => Ok((HR_CHARGE_SLOT_10_START, HR_CHARGE_SLOT_10_END)),
        _ => Err(format!("Extended charge slot must be 3-10, got {}", slot)),
    }
}

/// Map slot index (3-10) to extended discharge slot register pair.
fn extended_discharge_slot(slot: u8) -> Result<(u16, u16), String> {
    match slot {
        3 => Ok((HR_DISCHARGE_SLOT_3_START, HR_DISCHARGE_SLOT_3_END)),
        4 => Ok((HR_DISCHARGE_SLOT_4_START, HR_DISCHARGE_SLOT_4_END)),
        5 => Ok((HR_DISCHARGE_SLOT_5_START, HR_DISCHARGE_SLOT_5_END)),
        6 => Ok((HR_DISCHARGE_SLOT_6_START, HR_DISCHARGE_SLOT_6_END)),
        7 => Ok((HR_DISCHARGE_SLOT_7_START, HR_DISCHARGE_SLOT_7_END)),
        8 => Ok((HR_DISCHARGE_SLOT_8_START, HR_DISCHARGE_SLOT_8_END)),
        9 => Ok((HR_DISCHARGE_SLOT_9_START, HR_DISCHARGE_SLOT_9_END)),
        10 => Ok((HR_DISCHARGE_SLOT_10_START, HR_DISCHARGE_SLOT_10_END)),
        _ => Err(format!(
            "Extended discharge slot must be 3-10, got {}",
            slot
        )),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_battery_power_mode() {
        let cmd = ControlCommand::SetBatteryPowerMode { mode: 1 };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].address, HR_BATTERY_POWER_MODE);
        assert_eq!(writes[0].value, 1);
    }

    #[test]
    fn set_eco_mode() {
        let cmd = ControlCommand::SetEcoMode { soc_reserve: 4 };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 3);
        assert_eq!(writes[0].address, HR_BATTERY_POWER_MODE);
        assert_eq!(writes[0].value, 1);
        assert_eq!(writes[1].address, HR_ENABLE_DISCHARGE);
        assert_eq!(writes[1].value, 0);
        assert_eq!(writes[2].address, HR_BATTERY_SOC_RESERVE);
        assert_eq!(writes[2].value, 4);
        assert!(
            !writes.iter().any(|w| matches!(
                w.address,
                HR_DISCHARGE_SLOT_1_START
                    | HR_DISCHARGE_SLOT_1_END
                    | HR_DISCHARGE_SLOT_2_START
                    | HR_DISCHARGE_SLOT_2_END
            )),
            "Eco mode must preserve discharge slot times"
        );
    }

    #[test]
    fn set_timed_export_mode() {
        let cmd = ControlCommand::SetTimedExportMode { soc_reserve: 10 };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 3);
        assert_eq!(writes[0].value, 0); // export
        assert_eq!(writes[1].value, 1); // enable discharge
    }

    #[test]
    fn set_charge_slot() {
        let cmd = ControlCommand::SetChargeSlot1 {
            start: 600,
            end: 1000,
        };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].address, HR_CHARGE_SLOT_1_START);
        assert_eq!(writes[0].value, 600);
        assert_eq!(writes[1].address, HR_CHARGE_SLOT_1_END);
        assert_eq!(writes[1].value, 1000);
        // IMPORTANT: SetChargeSlot1 does NOT write enable_charge.
        // Setting enable_charge=1 triggers immediate force charge.
        // The slot times define WHEN charging is permitted; the
        // enable_charge flag is managed separately.
        assert!(
            !writes.iter().any(|w| w.address == HR_ENABLE_CHARGE),
            "SetChargeSlot1 must NOT include enable_charge register"
        );
    }

    #[test]
    fn set_soc_reserve_validates() {
        // Now min 4, max 100
        assert!(ControlCommand::SetBatterySocReserve { reserve: 3 }
            .encode()
            .is_err());
        assert!(ControlCommand::SetBatterySocReserve { reserve: 4 }
            .encode()
            .is_ok());
        assert!(ControlCommand::SetBatterySocReserve { reserve: 101 }
            .encode()
            .is_err());
    }

    #[test]
    fn set_charge_target_soc_validates() {
        // Now min 4, max 100
        assert!(ControlCommand::SetChargeTargetSoc { soc: 3 }
            .encode()
            .is_err());
        assert!(ControlCommand::SetChargeTargetSoc { soc: 4 }
            .encode()
            .is_ok());
        assert!(ControlCommand::SetChargeTargetSoc { soc: 101 }
            .encode()
            .is_err());
    }

    #[test]
    fn set_charge_limit_validates() {
        // New bound: 0-50
        let cmd = ControlCommand::SetChargeLimit { limit: 51 };
        assert!(cmd.encode().is_err());
        let ok = ControlCommand::SetChargeLimit { limit: 50 };
        assert!(ok.encode().is_ok());
    }

    #[test]
    fn set_ac_charge_limits_use_ac_registers() {
        let writes = ControlCommand::SetAcChargeLimit { limit: 100 }
            .encode()
            .unwrap();
        assert_eq!(writes[0].address, HR_AC_BATTERY_CHARGE_LIMIT);
        assert_eq!(writes[0].value, 100);
        assert!(ControlCommand::SetAcChargeLimit { limit: 0 }
            .encode()
            .is_err());

        let writes = ControlCommand::SetAcDischargeLimit { limit: 1 }
            .encode()
            .unwrap();
        assert_eq!(writes[0].address, HR_AC_BATTERY_DISCHARGE_LIMIT);
        assert_eq!(writes[0].value, 1);
        assert!(ControlCommand::SetAcDischargeLimit { limit: 101 }
            .encode()
            .is_err());
    }

    #[test]
    fn set_three_phase_limits_use_1000_range_registers() {
        let writes = ControlCommand::SetThreePhaseChargeLimit { limit: 100 }
            .encode()
            .unwrap();
        assert_eq!(writes[0].address, HR_3PH_BATTERY_CHARGE_LIMIT);
        assert_eq!(writes[0].value, 100);
        assert!(ControlCommand::SetThreePhaseChargeLimit { limit: 0 }
            .encode()
            .is_err());

        let writes = ControlCommand::SetThreePhaseDischargeLimit { limit: 1 }
            .encode()
            .unwrap();
        assert_eq!(writes[0].address, HR_3PH_BATTERY_DISCHARGE_LIMIT);
        assert_eq!(writes[0].value, 1);
        assert!(ControlCommand::SetThreePhaseDischargeLimit { limit: 101 }
            .encode()
            .is_err());

        assert_eq!(
            ControlCommand::SetThreePhaseBatterySocReserve { reserve: 20 }
                .encode()
                .unwrap()[0]
                .address,
            HR_3PH_BATTERY_SOC_RESERVE
        );
        assert_eq!(
            ControlCommand::SetThreePhaseChargeTargetSoc { soc: 95 }
                .encode()
                .unwrap()[0]
                .address,
            HR_3PH_CHARGE_TARGET_SOC
        );
    }

    #[test]
    fn pause_battery() {
        let cmd = ControlCommand::PauseBattery;
        let writes = cmd.encode().unwrap();
        assert_eq!(writes[0].address, HR_BATTERY_SOC_RESERVE);
        assert_eq!(writes[0].value, 100);
    }

    #[test]
    fn all_writes_are_safe() {
        // Verify all command encodings only produce whitelisted addresses
        let commands: Vec<ControlCommand> = vec![
            ControlCommand::SetBatteryPowerMode { mode: 0 },
            ControlCommand::SetBatteryPowerMode { mode: 1 },
            ControlCommand::SetEnableDischarge { enabled: true },
            ControlCommand::SetEnableDischarge { enabled: false },
            ControlCommand::SetEnableCharge { enabled: true },
            ControlCommand::SetBatterySocReserve { reserve: 50 },
            ControlCommand::SetChargeTargetSoc { soc: 80 },
            ControlCommand::SetChargeSlot1 {
                start: 600,
                end: 1000,
            },
            ControlCommand::SetChargeSlot2 { start: 0, end: 0 },
            ControlCommand::SetDischargeSlot1 {
                start: 1600,
                end: 1900,
            },
            ControlCommand::SetDischargeSlot2 { start: 0, end: 0 },
            ControlCommand::SetChargeLimit { limit: 30 },
            ControlCommand::SetDischargeLimit { limit: 40 },
            ControlCommand::SetEcoMode { soc_reserve: 4 },
            ControlCommand::SetTimedDemandMode { soc_reserve: 10 },
            ControlCommand::SetTimedExportMode { soc_reserve: 10 },
            ControlCommand::PauseBattery,
            ControlCommand::ForceCharge { target_soc: 100 },
            ControlCommand::ForceDischarge,
            ControlCommand::SyncClock,
            ControlCommand::SetPowerReserve { reserve: 10 },
            ControlCommand::SetRtc { enabled: true },
            ControlCommand::SetRtc { enabled: false },
            ControlCommand::SetExportPriority { priority: 0 },
            ControlCommand::SetEps { enabled: true },
            ControlCommand::SetPauseMode { mode: 0 },
            ControlCommand::SetPauseSlot { start: 0, end: 0 },
            ControlCommand::SetChargeTargetSocSlot { slot: 1, soc: 80 },
            ControlCommand::SetDischargeTargetSocSlot { slot: 2, soc: 60 },
            ControlCommand::SetChargeSlotN {
                slot: 3,
                start: 600,
                end: 1000,
            },
            ControlCommand::SetDischargeSlotN {
                slot: 4,
                start: 1600,
                end: 1900,
            },
        ];
        for cmd in &commands {
            match cmd.encode() {
                Ok(writes) => {
                    for w in &writes {
                        assert!(
                            SAFE_WRITE_REGS.contains(&w.address),
                            "address {} not whitelisted for {:?}",
                            w.address,
                            cmd
                        );
                    }
                }
                Err(e) => {
                    panic!("Command {:?} failed to encode: {}", cmd, e);
                }
            }
        }
    }

    #[test]
    fn force_charge_encodes() {
        let cmd = ControlCommand::ForceCharge { target_soc: 80 };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 4);
        assert_eq!(writes[0].address, HR_BATTERY_POWER_MODE);
        assert_eq!(writes[0].value, 1); // eco mode
        assert_eq!(writes[1].address, HR_ENABLE_CHARGE);
        assert_eq!(writes[1].value, 1);
        assert_eq!(writes[2].address, HR_ENABLE_CHARGE_TARGET);
        assert_eq!(writes[2].value, 1);
        assert_eq!(writes[3].address, HR_CHARGE_TARGET_SOC);
        assert_eq!(writes[3].value, 80);
    }

    #[test]
    fn force_discharge_encodes() {
        let cmd = ControlCommand::ForceDischarge;
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 3);
        assert_eq!(writes[0].address, HR_ENABLE_DISCHARGE);
        assert_eq!(writes[0].value, 1);
        assert_eq!(writes[1].address, HR_DISCHARGE_SLOT_1_START);
        assert_eq!(writes[1].value, 0);
        assert_eq!(writes[2].address, HR_DISCHARGE_SLOT_1_END);
        assert_eq!(writes[2].value, 2359);
    }

    #[test]
    fn sync_clock_encodes() {
        let cmd = ControlCommand::SyncClock;
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 6);
        // All system time registers in order
        assert_eq!(writes[0].address, HR_SYSTEM_TIME_YEAR);
        assert!(
            writes[0].value <= 99,
            "Year must be 2-digit (offset from 2000), got {}",
            writes[0].value
        );
        assert_eq!(writes[1].address, HR_SYSTEM_TIME_MONTH);
        assert!(writes[1].value >= 1 && writes[1].value <= 12);
        assert_eq!(writes[2].address, HR_SYSTEM_TIME_DAY);
        assert!(writes[2].value >= 1 && writes[2].value <= 31);
        assert_eq!(writes[3].address, HR_SYSTEM_TIME_HOUR);
        assert_eq!(writes[4].address, HR_SYSTEM_TIME_MINUTE);
        assert_eq!(writes[5].address, HR_SYSTEM_TIME_SECOND);
    }

    #[test]
    fn cosy_exit_encodes() {
        let cmd = ControlCommand::CosyExit;
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 4);
        assert_eq!(writes[0].address, HR_ENABLE_CHARGE);
        assert_eq!(writes[0].value, 0);
        assert_eq!(writes[1].address, HR_ENABLE_CHARGE_TARGET);
        assert_eq!(writes[1].value, 0);
        assert_eq!(writes[2].address, HR_BATTERY_POWER_MODE);
        assert_eq!(writes[2].value, 1); // eco mode
        assert_eq!(writes[3].address, HR_ENABLE_DISCHARGE);
        assert_eq!(writes[3].value, 1);
    }

    #[test]
    fn set_charge_slot2_encodes() {
        let cmd = ControlCommand::SetChargeSlot2 {
            start: 2300,
            end: 500,
        };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].address, HR_CHARGE_SLOT_2_START);
        assert_eq!(writes[0].value, 2300);
        assert_eq!(writes[1].address, HR_CHARGE_SLOT_2_END);
        assert_eq!(writes[1].value, 500);
    }

    #[test]
    fn set_discharge_slot1_encodes() {
        let cmd = ControlCommand::SetDischargeSlot1 {
            start: 1600,
            end: 1900,
        };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].address, HR_DISCHARGE_SLOT_1_START);
        assert_eq!(writes[0].value, 1600);
        assert_eq!(writes[1].address, HR_DISCHARGE_SLOT_1_END);
        assert_eq!(writes[1].value, 1900);
    }

    #[test]
    fn set_discharge_slot2_encodes() {
        let cmd = ControlCommand::SetDischargeSlot2 {
            start: 2000,
            end: 2200,
        };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].address, HR_DISCHARGE_SLOT_2_START);
        assert_eq!(writes[0].value, 2000);
        assert_eq!(writes[1].address, HR_DISCHARGE_SLOT_2_END);
        assert_eq!(writes[1].value, 2200);
    }

    #[test]
    fn set_active_power_rate_encodes() {
        let cmd = ControlCommand::SetActivePowerRate { rate: 75 };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].address, HR_ACTIVE_POWER_RATE);
        assert_eq!(writes[0].value, 75);
    }

    #[test]
    fn set_active_power_rate_validates() {
        let cmd = ControlCommand::SetActivePowerRate { rate: 101 };
        assert!(cmd.encode().is_err());
    }

    #[test]
    fn set_charge_target_soc_encodes() {
        let cmd = ControlCommand::SetChargeTargetSoc { soc: 80 };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].address, HR_ENABLE_CHARGE_TARGET);
        assert_eq!(writes[0].value, 1);
        assert_eq!(writes[1].address, HR_CHARGE_TARGET_SOC);
        assert_eq!(writes[1].value, 80);
    }

    #[test]
    fn set_calibration_stage_encodes() {
        let cmd = ControlCommand::SetCalibrationStage { stage: 5 };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].address, HR_BATTERY_CALIBRATION_STAGE);
        assert_eq!(writes[0].value, 5);
    }

    #[test]
    fn set_calibration_stage_validates() {
        let cmd = ControlCommand::SetCalibrationStage { stage: 8 };
        assert!(cmd.encode().is_err());
    }

    #[test]
    fn set_enable_charge_encodes() {
        let on = ControlCommand::SetEnableCharge { enabled: true };
        assert_eq!(on.encode().unwrap()[0].value, 1);
        let off = ControlCommand::SetEnableCharge { enabled: false };
        assert_eq!(off.encode().unwrap()[0].value, 0);
    }

    #[test]
    fn set_enable_discharge_encodes() {
        let on = ControlCommand::SetEnableDischarge { enabled: true };
        assert_eq!(on.encode().unwrap()[0].value, 1);
        let off = ControlCommand::SetEnableDischarge { enabled: false };
        assert_eq!(off.encode().unwrap()[0].value, 0);
    }

    #[test]
    fn set_discharge_limit_validates() {
        // New bound: 0-50
        assert!(ControlCommand::SetDischargeLimit { limit: 51 }
            .encode()
            .is_err());
        assert!(ControlCommand::SetDischargeLimit { limit: 50 }
            .encode()
            .is_ok());
    }

    // -----------------------------------------------------------------------
    // New command tests (items 3-9 from audit)
    // -----------------------------------------------------------------------

    #[test]
    fn set_power_reserve_encodes() {
        let cmd = ControlCommand::SetPowerReserve { reserve: 10 };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].address, HR_BATTERY_DISCHARGE_MIN_POWER_RESERVE);
        assert_eq!(writes[0].value, 10);
    }

    #[test]
    fn set_power_reserve_validates() {
        assert!(ControlCommand::SetPowerReserve { reserve: 3 }
            .encode()
            .is_err());
        assert!(ControlCommand::SetPowerReserve { reserve: 101 }
            .encode()
            .is_err());
        assert!(ControlCommand::SetPowerReserve { reserve: 4 }
            .encode()
            .is_ok());
    }

    #[test]
    fn set_rtc_encodes() {
        let on = ControlCommand::SetRtc { enabled: true };
        assert_eq!(on.encode().unwrap()[0].value, 1);
        let off = ControlCommand::SetRtc { enabled: false };
        assert_eq!(off.encode().unwrap()[0].value, 0);
    }

    #[test]
    fn set_export_priority_encodes() {
        let cmd = ControlCommand::SetExportPriority { priority: 1 };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes[0].address, HR_EXPORT_PRIORITY);
        assert_eq!(writes[0].value, 1);
    }

    #[test]
    fn set_export_priority_validates() {
        assert!(ControlCommand::SetExportPriority { priority: 3 }
            .encode()
            .is_err());
        assert!(ControlCommand::SetExportPriority { priority: 0 }
            .encode()
            .is_ok());
    }

    #[test]
    fn set_eps_encodes() {
        let on = ControlCommand::SetEps { enabled: true };
        assert_eq!(on.encode().unwrap()[0].value, 1);
        assert!(on.encode().unwrap()[0].address == HR_ENABLE_EPS);
    }

    #[test]
    fn set_pause_mode_encodes() {
        let cmd = ControlCommand::SetPauseMode { mode: 1 };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes[0].address, HR_BATTERY_PAUSE_MODE);
        assert_eq!(writes[0].value, 1);
    }

    #[test]
    fn set_pause_slot_encodes() {
        let cmd = ControlCommand::SetPauseSlot {
            start: 1400,
            end: 1600,
        };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].address, HR_BATTERY_PAUSE_SLOT_1_START);
        assert_eq!(writes[0].value, 1400);
        assert_eq!(writes[1].address, HR_BATTERY_PAUSE_SLOT_1_END);
        assert_eq!(writes[1].value, 1600);
    }

    #[test]
    fn set_charge_target_soc_slot_encodes() {
        let cmd = ControlCommand::SetChargeTargetSocSlot { slot: 1, soc: 80 };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].address, HR_CHARGE_TARGET_SOC_1);
        assert_eq!(writes[0].value, 80);

        let cmd2 = ControlCommand::SetChargeTargetSocSlot { slot: 2, soc: 60 };
        let writes2 = cmd2.encode().unwrap();
        assert_eq!(writes2[0].address, HR_CHARGE_TARGET_SOC_2);

        let cmd3 = ControlCommand::SetChargeTargetSocSlot { slot: 3, soc: 50 };
        let writes3 = cmd3.encode().unwrap();
        assert_eq!(writes3[0].address, HR_CHARGE_TARGET_SOC_3);

        let cmd10 = ControlCommand::SetChargeTargetSocSlot { slot: 10, soc: 50 };
        let writes10 = cmd10.encode().unwrap();
        assert_eq!(writes10[0].address, HR_CHARGE_TARGET_SOC_10);

        // Invalid slot
        assert!(ControlCommand::SetChargeTargetSocSlot { slot: 11, soc: 50 }
            .encode()
            .is_err());
    }

    #[test]
    fn set_discharge_target_soc_slot_encodes() {
        let cmd = ControlCommand::SetDischargeTargetSocSlot { slot: 1, soc: 40 };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes[0].address, HR_DISCHARGE_TARGET_SOC_1);

        let cmd2 = ControlCommand::SetDischargeTargetSocSlot { slot: 2, soc: 30 };
        let writes2 = cmd2.encode().unwrap();
        assert_eq!(writes2[0].address, HR_DISCHARGE_TARGET_SOC_2);

        let cmd3 = ControlCommand::SetDischargeTargetSocSlot { slot: 3, soc: 50 };
        let writes3 = cmd3.encode().unwrap();
        assert_eq!(writes3[0].address, HR_DISCHARGE_TARGET_SOC_3);

        let cmd10 = ControlCommand::SetDischargeTargetSocSlot { slot: 10, soc: 50 };
        let writes10 = cmd10.encode().unwrap();
        assert_eq!(writes10[0].address, HR_DISCHARGE_TARGET_SOC_10);

        // Invalid slot
        assert!(
            ControlCommand::SetDischargeTargetSocSlot { slot: 11, soc: 50 }
                .encode()
                .is_err()
        );
    }

    #[test]
    fn extended_charge_slots_encode() {
        let cmd = ControlCommand::SetChargeSlotN {
            slot: 3,
            start: 600,
            end: 1000,
        };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes[0].address, HR_CHARGE_SLOT_3_START);
        assert_eq!(writes[1].address, HR_CHARGE_SLOT_3_END);

        let cmd10 = ControlCommand::SetChargeSlotN {
            slot: 10,
            start: 0,
            end: 0,
        };
        let writes10 = cmd10.encode().unwrap();
        assert_eq!(writes10[0].address, HR_CHARGE_SLOT_10_START);
        assert_eq!(writes10[1].address, HR_CHARGE_SLOT_10_END);

        assert!(ControlCommand::SetChargeSlotN {
            slot: 2,
            start: 0,
            end: 0
        }
        .encode()
        .is_err());
        assert!(ControlCommand::SetChargeSlotN {
            slot: 11,
            start: 0,
            end: 0
        }
        .encode()
        .is_err());
    }

    #[test]
    fn extended_discharge_slots_encode() {
        let cmd = ControlCommand::SetDischargeSlotN {
            slot: 5,
            start: 1600,
            end: 1900,
        };
        let writes = cmd.encode().unwrap();
        assert_eq!(writes[0].address, HR_DISCHARGE_SLOT_5_START);
        assert_eq!(writes[1].address, HR_DISCHARGE_SLOT_5_END);

        assert!(ControlCommand::SetDischargeSlotN {
            slot: 1,
            start: 0,
            end: 0
        }
        .encode()
        .is_err());
    }

    #[test]
    fn reboot_inverter_encodes() {
        let cmd = ControlCommand::RebootInverter;
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].address, HR_INVERTER_REBOOT);
        assert_eq!(writes[0].value, 100);
    }
}
