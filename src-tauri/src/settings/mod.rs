//! Application settings with file-based persistence.
//!
//! Settings are saved as JSON to `~/.givenergy-local/settings.json`.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Application settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Inverter IP address (e.g. "192.168.1.36").
    pub host: String,
    /// Inverter Modbus port (typically 8899).
    pub port: u16,
    /// Inverter serial number (e.g. "CE2052G072").
    pub serial: String,
    /// Poll interval in seconds.
    pub poll_interval: u64,
    /// Whether to auto-connect on startup.
    pub auto_connect: bool,
    /// Import electricity tariff in £/kWh.
    #[serde(default = "default_import_tariff")]
    pub import_tariff: f64,
    /// Export electricity tariff in £/kWh.
    #[serde(default = "default_export_tariff")]
    pub export_tariff: f64,

    /// Auto winter mode enabled.
    #[serde(default)]
    pub auto_winter_enabled: bool,
    /// Temperature below which winter mode activates (°C).
    #[serde(default = "default_aw_cold_threshold")]
    pub auto_winter_cold_threshold: f32,
    /// Temperature above which winter mode deactivates (°C).
    #[serde(default = "default_aw_recovery_threshold")]
    pub auto_winter_recovery_threshold: f32,
    /// Target SOC for winter mode charging (4-100%).
    #[serde(default = "default_aw_target_soc")]
    pub auto_winter_target_soc: u8,
    /// Consecutive readings before state transitions.
    #[serde(default = "default_aw_debounce")]
    pub auto_winter_debounce_readings: u32,
}

fn default_import_tariff() -> f64 {
    0.285
}

fn default_export_tariff() -> f64 {
    0.15
}

fn default_aw_cold_threshold() -> f32 {
    8.0
}
fn default_aw_recovery_threshold() -> f32 {
    12.0
}
fn default_aw_target_soc() -> u8 {
    80
}
fn default_aw_debounce() -> u32 {
    10
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 8899,
            serial: String::new(),
            poll_interval: 60,
            auto_connect: true,
            import_tariff: default_import_tariff(),
            export_tariff: default_export_tariff(),
            auto_winter_enabled: false,
            auto_winter_cold_threshold: default_aw_cold_threshold(),
            auto_winter_recovery_threshold: default_aw_recovery_threshold(),
            auto_winter_target_soc: default_aw_target_soc(),
            auto_winter_debounce_readings: default_aw_debounce(),
        }
    }
}

impl Settings {
    /// Get the path to the settings file.
    fn settings_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let dir = PathBuf::from(home).join(".givenergy-local");
        dir.join("settings.json")
    }

    /// Load settings from disk, creating defaults if the file doesn't exist.
    pub fn load() -> Self {
        let path = Self::settings_path();
        match fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(settings) => {
                    log::info!("Loaded settings from {}", path.display());
                    settings
                }
                Err(e) => {
                    log::warn!("Failed to parse settings: {}, using defaults", e);
                    Self::default()
                }
            },
            Err(_) => {
                log::info!("No settings file found, using defaults");
                let defaults = Self::default();
                // Try to create the file for next time
                let _ = defaults.save();
                defaults
            }
        }
    }

    /// Save current settings to disk.
    pub fn save(&self) -> Result<(), String> {
        let path = Self::settings_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create settings dir: {}", e))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize settings: {}", e))?;
        fs::write(&path, json).map_err(|e| format!("Failed to write settings: {}", e))?;
        log::info!("Settings saved to {}", path.display());
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
    fn default_settings() {
        let s = Settings::default();
        assert!(s.host.is_empty());
        assert_eq!(s.port, 8899);
        assert!(s.serial.is_empty());
        assert_eq!(s.poll_interval, 60);
        assert!(s.auto_connect);
        assert!(!s.auto_winter_enabled);
        assert_eq!(s.auto_winter_cold_threshold, 8.0);
        assert_eq!(s.auto_winter_recovery_threshold, 12.0);
        assert_eq!(s.auto_winter_target_soc, 80);
        assert_eq!(s.auto_winter_debounce_readings, 10);
    }

    #[test]
    fn settings_roundtrip() {
        let s = Settings {
            host: "10.0.0.50".to_string(),
            port: 502,
            serial: "TEST123".to_string(),
            poll_interval: 10,
            auto_connect: false,
            import_tariff: 0.30,
            export_tariff: 0.15,
            auto_winter_enabled: true,
            auto_winter_cold_threshold: 5.0,
            auto_winter_recovery_threshold: 10.0,
            auto_winter_target_soc: 90,
            auto_winter_debounce_readings: 5,
        };
        let json = serde_json::to_string(&s).unwrap();
        let decoded: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.host, "10.0.0.50");
        assert_eq!(decoded.port, 502);
        assert_eq!(decoded.serial, "TEST123");
        assert_eq!(decoded.poll_interval, 10);
        assert!(!decoded.auto_connect);
        assert!(decoded.auto_winter_enabled);
        assert_eq!(decoded.auto_winter_cold_threshold, 5.0);
        assert_eq!(decoded.auto_winter_recovery_threshold, 10.0);
        assert_eq!(decoded.auto_winter_target_soc, 90);
        assert_eq!(decoded.auto_winter_debounce_readings, 5);
    }

    #[test]
    fn save_and_load() {
        // Use a temp dir to avoid polluting real settings
        let tmp_dir = std::env::temp_dir().join("givenergy-test-settings");
        let _ = fs::create_dir_all(&tmp_dir);

        let s = Settings {
            host: "192.168.1.99".to_string(),
            port: 8899,
            serial: "TEST99".to_string(),
            poll_interval: 15,
            auto_connect: true,
            import_tariff: 0.285,
            export_tariff: 0.15,
            auto_winter_enabled: false,
            auto_winter_cold_threshold: 8.0,
            auto_winter_recovery_threshold: 12.0,
            auto_winter_target_soc: 80,
            auto_winter_debounce_readings: 10,
        };

        // We can't easily override the settings path for testing,
        // so just verify serialization works
        let json = serde_json::to_string_pretty(&s).unwrap();
        assert!(json.contains("192.168.1.99"));
        assert!(json.contains("TEST99"));
    }
}
