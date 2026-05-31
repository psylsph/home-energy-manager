//! Control command encoder.
//!
//! Translates high-level control commands into raw Modbus register writes.
//! Only whitelisted register addresses from SAFE_WRITE_REGS are allowed.

use chrono::{Datelike, Timelike, Utc};

use crate::modbus::registers::{
    HR_BATTERY_CHARGE_LIMIT, HR_BATTERY_DISCHARGE_LIMIT, HR_BATTERY_POWER_MODE,
    HR_BATTERY_SOC_RESERVE, HR_CHARGE_SLOT_1_END, HR_CHARGE_SLOT_1_START, HR_CHARGE_SLOT_2_END,
    HR_CHARGE_SLOT_2_START, HR_CHARGE_TARGET_SOC, HR_DISCHARGE_SLOT_1_END,
    HR_DISCHARGE_SLOT_1_START, HR_DISCHARGE_SLOT_2_END, HR_DISCHARGE_SLOT_2_START,
    HR_ENABLE_CHARGE, HR_ENABLE_CHARGE_TARGET, HR_ENABLE_DISCHARGE,
    HR_SYSTEM_TIME_YEAR, HR_SYSTEM_TIME_MONTH, HR_SYSTEM_TIME_DAY,
    HR_SYSTEM_TIME_HOUR, HR_SYSTEM_TIME_MINUTE, HR_SYSTEM_TIME_SECOND,
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
    /// Set battery SOC reserve (0-100).
    SetBatterySocReserve { reserve: u16 },
    /// Set charge target SOC (0-100).
    SetChargeTargetSoc { soc: u16 },
    /// Set charge slot 1 times (HHMM packed).
    SetChargeSlot1 { start: u16, end: u16 },
    /// Set charge slot 2 times (HHMM packed).
    SetChargeSlot2 { start: u16, end: u16 },
    /// Set discharge slot 1 times (HHMM packed).
    SetDischargeSlot1 { start: u16, end: u16 },
    /// Set discharge slot 2 times (HHMM packed).
    SetDischargeSlot2 { start: u16, end: u16 },
    /// Set battery charge limit percentage (0-50).
    SetChargeLimit { limit: u16 },
    /// Set battery discharge limit percentage (0-50).
    SetDischargeLimit { limit: u16 },
    /// Set Eco mode (self-consumption, no discharge, clear discharge slots).
    SetEcoMode { soc_reserve: u16 },
    /// Set Timed Demand mode (self-consumption + discharge).
    SetTimedDemandMode { soc_reserve: u16 },
    /// Set Timed Export mode (export + discharge).
    SetTimedExportMode { soc_reserve: u16 },
    /// Pause battery (set SOC reserve to 100).
    PauseBattery,
    /// Force charge: enable charging with target SOC and enable_charge.
    ForceCharge,
    /// Force discharge: enable discharge with a full-day discharge slot.
    ForceDischarge,
    /// Sync inverter clock to current system time.
    SyncClock,
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
            ControlCommand::SetBatterySocReserve { reserve } => {
                validate_range(*reserve, 0, 100, "SOC reserve")?;
                vec![rw(HR_BATTERY_SOC_RESERVE, *reserve)]
            }
            ControlCommand::SetChargeTargetSoc { soc } => {
                validate_range(*soc, 0, 100, "target SOC")?;
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
                validate_range(*limit, 0, 100, "charge limit")?;
                vec![rw(HR_BATTERY_CHARGE_LIMIT, *limit)]
            }
            ControlCommand::SetDischargeLimit { limit } => {
                validate_range(*limit, 0, 100, "discharge limit")?;
                vec![rw(HR_BATTERY_DISCHARGE_LIMIT, *limit)]
            }
            ControlCommand::SetEcoMode { soc_reserve } => {
                validate_range(*soc_reserve, 0, 100, "SOC reserve")?;
                vec![
                    rw(HR_BATTERY_POWER_MODE, 1), // self-consumption
                    rw(HR_ENABLE_DISCHARGE, 0),   // no timed discharge
                    rw(HR_BATTERY_SOC_RESERVE, *soc_reserve),
                    rw(HR_DISCHARGE_SLOT_1_START, 0), // clear discharge slot 1
                    rw(HR_DISCHARGE_SLOT_1_END, 0),
                    rw(HR_DISCHARGE_SLOT_2_START, 0), // clear discharge slot 2
                    rw(HR_DISCHARGE_SLOT_2_END, 0),
                ]
            }
            ControlCommand::SetTimedDemandMode { soc_reserve } => {
                validate_range(*soc_reserve, 0, 100, "SOC reserve")?;
                vec![
                    rw(HR_BATTERY_POWER_MODE, 1), // self-consumption
                    rw(HR_ENABLE_DISCHARGE, 1),   // enable timed discharge
                    rw(HR_BATTERY_SOC_RESERVE, *soc_reserve),
                ]
            }
            ControlCommand::SetTimedExportMode { soc_reserve } => {
                validate_range(*soc_reserve, 0, 100, "SOC reserve")?;
                vec![
                    rw(HR_BATTERY_POWER_MODE, 0), // export mode
                    rw(HR_ENABLE_DISCHARGE, 1),   // enable timed discharge
                    rw(HR_BATTERY_SOC_RESERVE, *soc_reserve),
                ]
            }
            ControlCommand::PauseBattery => {
                vec![rw(HR_BATTERY_SOC_RESERVE, 100)]
            }
            ControlCommand::ForceCharge => {
                vec![
                    rw(HR_ENABLE_CHARGE, 1),
                    rw(HR_ENABLE_CHARGE_TARGET, 1),
                    rw(HR_CHARGE_TARGET_SOC, 100),
                ]
            }
            ControlCommand::ForceDischarge => {
                vec![
                    rw(HR_ENABLE_DISCHARGE, 1),
                    rw(HR_DISCHARGE_SLOT_1_START, 0),   // 00:00
                    rw(HR_DISCHARGE_SLOT_1_END, 2359),  // 23:59
                ]
            }
            ControlCommand::SyncClock => {
                let now = Utc::now();
                vec![
                    rw(HR_SYSTEM_TIME_YEAR, now.year() as u16),
                    rw(HR_SYSTEM_TIME_MONTH, now.month() as u16),
                    rw(HR_SYSTEM_TIME_DAY, now.day() as u16),
                    rw(HR_SYSTEM_TIME_HOUR, now.hour() as u16),
                    rw(HR_SYSTEM_TIME_MINUTE, now.minute() as u16),
                    rw(HR_SYSTEM_TIME_SECOND, now.second() as u16),
                ]
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
        assert_eq!(writes.len(), 7);
        assert_eq!(writes[0].address, HR_BATTERY_POWER_MODE);
        assert_eq!(writes[0].value, 1);
        assert_eq!(writes[1].address, HR_ENABLE_DISCHARGE);
        assert_eq!(writes[1].value, 0);
        assert_eq!(writes[2].address, HR_BATTERY_SOC_RESERVE);
        assert_eq!(writes[2].value, 4);
        assert_eq!(writes[3].address, HR_DISCHARGE_SLOT_1_START);
        assert_eq!(writes[3].value, 0);
        assert_eq!(writes[4].address, HR_DISCHARGE_SLOT_1_END);
        assert_eq!(writes[4].value, 0);
        assert_eq!(writes[5].address, HR_DISCHARGE_SLOT_2_START);
        assert_eq!(writes[5].value, 0);
        assert_eq!(writes[6].address, HR_DISCHARGE_SLOT_2_END);
        assert_eq!(writes[6].value, 0);
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
    }

    #[test]
    fn set_soc_reserve_validates() {
        let cmd = ControlCommand::SetBatterySocReserve { reserve: 101 };
        assert!(cmd.encode().is_err());
    }

    #[test]
    fn set_charge_limit_validates() {
        let cmd = ControlCommand::SetChargeLimit { limit: 101 };
        assert!(cmd.encode().is_err());
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
        let commands = vec![
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
            ControlCommand::ForceCharge,
            ControlCommand::ForceDischarge,
            ControlCommand::SyncClock,
        ];
        for cmd in &commands {
            let writes = cmd.encode().unwrap();
            for w in &writes {
                assert!(
                    SAFE_WRITE_REGS.contains(&w.address),
                    "address {} not whitelisted for {:?}",
                    w.address,
                    cmd
                );
            }
        }
    }

    #[test]
    fn force_charge_encodes() {
        let cmd = ControlCommand::ForceCharge;
        let writes = cmd.encode().unwrap();
        assert_eq!(writes.len(), 3);
        assert_eq!(writes[0].address, HR_ENABLE_CHARGE);
        assert_eq!(writes[0].value, 1);
        assert_eq!(writes[1].address, HR_ENABLE_CHARGE_TARGET);
        assert_eq!(writes[1].value, 1);
        assert_eq!(writes[2].address, HR_CHARGE_TARGET_SOC);
        assert_eq!(writes[2].value, 100);
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
        assert_eq!(writes[1].address, HR_SYSTEM_TIME_MONTH);
        assert_eq!(writes[2].address, HR_SYSTEM_TIME_DAY);
        assert_eq!(writes[3].address, HR_SYSTEM_TIME_HOUR);
        assert_eq!(writes[4].address, HR_SYSTEM_TIME_MINUTE);
        assert_eq!(writes[5].address, HR_SYSTEM_TIME_SECOND);
    }
}
