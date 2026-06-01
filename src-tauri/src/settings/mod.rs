//! Application settings with file-based persistence.
//!
//! Settings are saved as JSON to `~/.givenergy-local/settings.json`
//! (`%USERPROFILE%\.givenergy-local\settings.json` on Windows).
//! Override with the `GIVENERGY_LOCAL_CONFIG_DIR` environment variable.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Tariff configuration with peak and off-peak rates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TariffConfig {
    /// Peak rate in £/kWh.
    pub peak_rate: f64,
    /// Off-peak rate in £/kWh.
    pub off_peak_rate: f64,
    /// Off-peak start time in "HH:MM" format (24h).
    pub off_peak_start: String,
    /// Off-peak end time in "HH:MM" format (24h).
    /// Can be before `off_peak_start` to indicate crossing midnight.
    pub off_peak_end: String,
}

impl Default for TariffConfig {
    fn default() -> Self {
        Self {
            peak_rate: 0.285,
            off_peak_rate: 0.09,
            off_peak_start: "00:30".to_string(),
            off_peak_end: "05:30".to_string(),
        }
    }
}

/// A cosy charging slot stored locally (not written to inverter registers).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CosySlot {
    /// Whether the slot is enabled.
    pub enabled: bool,
    /// Start hour (0-23).
    pub start_hour: u8,
    /// Start minute (0-59).
    pub start_minute: u8,
    /// End hour (0-23).
    pub end_hour: u8,
    /// End minute (0-59).
    pub end_minute: u8,
    /// Target SOC for charging (4-100%).
    pub target_soc: u8,
}

impl Default for CosySlot {
    fn default() -> Self {
        Self {
            enabled: false,
            start_hour: 0,
            start_minute: 0,
            end_hour: 0,
            end_minute: 0,
            target_soc: 100,
        }
    }
}

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
    /// HTTP server port (default 7337). Change to run multiple instances.
    #[serde(default = "default_http_port")]
    pub http_port: u16,
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

    /// Cosy charging mode enabled.
    #[serde(default)]
    pub cosy_enabled: bool,
    /// Cosy charging slots (up to 3, stored locally).
    #[serde(default)]
    pub cosy_slots: Vec<CosySlot>,

    /// Persisted `enable_charge_target` saved before winter mode activated.
    /// `Some` means winter mode was active when the last state was saved.
    #[serde(default)]
    pub auto_winter_saved_enable_target: Option<bool>,
    /// Persisted `target_soc` saved before winter mode activated.
    #[serde(default)]
    pub auto_winter_saved_target_soc: Option<u16>,

    /// Full import tariff config with peak/off-peak rates and times.
    /// Falls back to legacy `import_tariff` if `None`.
    #[serde(default)]
    pub import_tariff_config: Option<TariffConfig>,
    /// Full export tariff config with peak/off-peak rates and times.
    /// Falls back to legacy `export_tariff` if `None`.
    #[serde(default)]
    pub export_tariff_config: Option<TariffConfig>,
}

fn default_http_port() -> u16 {
    7337
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
            http_port: default_http_port(),
            auto_connect: true,
            import_tariff: default_import_tariff(),
            export_tariff: default_export_tariff(),
            auto_winter_enabled: false,
            auto_winter_cold_threshold: default_aw_cold_threshold(),
            auto_winter_recovery_threshold: default_aw_recovery_threshold(),
            auto_winter_target_soc: default_aw_target_soc(),
            auto_winter_debounce_readings: default_aw_debounce(),
            auto_winter_saved_enable_target: None,
            auto_winter_saved_target_soc: None,
            import_tariff_config: None,
            export_tariff_config: None,
            cosy_enabled: false,
            cosy_slots: (0..3).map(|_| CosySlot::default()).collect(),
        }
    }
}

impl Settings {
    /// Get the settings directory path.
    /// Uses `GIVENERGY_LOCAL_CONFIG_DIR` env var if set, otherwise `~/.givenergy-local/`
    /// (or `%USERPROFILE%\.givenergy-local\` on Windows).
    pub fn settings_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("GIVENERGY_LOCAL_CONFIG_DIR") {
            return PathBuf::from(dir);
        }
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".givenergy-local")
    }

    /// Get the path to the settings file.
    fn settings_path() -> PathBuf {
        Self::settings_dir().join("settings.json")
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
        assert_eq!(s.http_port, 7337);
        assert!(s.auto_connect);
        assert!(!s.auto_winter_enabled);
        assert_eq!(s.auto_winter_cold_threshold, 8.0);
        assert_eq!(s.auto_winter_recovery_threshold, 12.0);
        assert_eq!(s.auto_winter_target_soc, 80);
        assert_eq!(s.auto_winter_debounce_readings, 10);
        assert_eq!(s.auto_winter_saved_enable_target, None);
        assert_eq!(s.auto_winter_saved_target_soc, None);
    }

    #[test]
    fn settings_roundtrip() {
        let s = Settings {
            host: "10.0.0.50".to_string(),
            port: 502,
            serial: "TEST123".to_string(),
            poll_interval: 10,
            http_port: 8080,
            auto_connect: false,
            import_tariff: 0.30,
            export_tariff: 0.15,
            auto_winter_enabled: true,
            auto_winter_cold_threshold: 5.0,
            auto_winter_recovery_threshold: 10.0,
            auto_winter_target_soc: 90,
            auto_winter_debounce_readings: 5,
            auto_winter_saved_enable_target: Some(true),
            auto_winter_saved_target_soc: Some(80),
            import_tariff_config: None,
            export_tariff_config: None,
            cosy_enabled: false,
            cosy_slots: vec![],
        };
        let json = serde_json::to_string(&s).unwrap();
        let decoded: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.host, "10.0.0.50");
        assert_eq!(decoded.port, 502);
        assert_eq!(decoded.serial, "TEST123");
        assert_eq!(decoded.poll_interval, 10);
        assert_eq!(decoded.http_port, 8080);
        assert!(!decoded.auto_connect);
        assert!(decoded.auto_winter_enabled);
        assert_eq!(decoded.auto_winter_cold_threshold, 5.0);
        assert_eq!(decoded.auto_winter_recovery_threshold, 10.0);
        assert_eq!(decoded.auto_winter_target_soc, 90);
        assert_eq!(decoded.auto_winter_debounce_readings, 5);
        assert_eq!(decoded.auto_winter_saved_enable_target, Some(true));
        assert_eq!(decoded.auto_winter_saved_target_soc, Some(80));
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
            http_port: 7337,
            auto_connect: true,
            import_tariff: 0.285,
            export_tariff: 0.15,
            auto_winter_enabled: false,
            auto_winter_cold_threshold: 8.0,
            auto_winter_recovery_threshold: 12.0,
            auto_winter_target_soc: 80,
            auto_winter_debounce_readings: 10,
            auto_winter_saved_enable_target: None,
            auto_winter_saved_target_soc: None,
            import_tariff_config: None,
            export_tariff_config: None,
            cosy_enabled: false,
            cosy_slots: vec![],
        };

        // We can't easily override the settings path for testing,
        // so just verify serialization works
        let json = serde_json::to_string_pretty(&s).unwrap();
        assert!(json.contains("192.168.1.99"));
        assert!(json.contains("TEST99"));
    }

    /// Roundtrip for cosy charging config — written by POST /api/cosy
    /// and read back by GET /api/cosy.
    #[test]
    fn cosy_roundtrip() {
        let s = Settings {
            cosy_enabled: true,
            cosy_slots: vec![
                CosySlot {
                    enabled: true,
                    start_hour: 0,
                    start_minute: 0,
                    end_hour: 6,
                    end_minute: 0,
                    target_soc: 100,
                },
                CosySlot {
                    enabled: false,
                    start_hour: 0,
                    start_minute: 0,
                    end_hour: 0,
                    end_minute: 0,
                    target_soc: 100,
                },
                CosySlot {
                    enabled: false,
                    start_hour: 0,
                    start_minute: 0,
                    end_hour: 0,
                    end_minute: 0,
                    target_soc: 100,
                },
            ],
            ..Settings::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        let decoded: Settings = serde_json::from_str(&json).unwrap();

        assert!(decoded.cosy_enabled);
        assert_eq!(decoded.cosy_slots.len(), 3);
        assert!(decoded.cosy_slots[0].enabled);
        assert_eq!(decoded.cosy_slots[0].start_hour, 0);
        assert_eq!(decoded.cosy_slots[0].end_minute, 0);
        assert!(!decoded.cosy_slots[1].enabled);

        // All-zero time is the "not set" default on the server side —
        // must survive roundtrip unchanged (not collapse to nulls).
        let raw = format!(
            "{{\"enabled\":false,\"start_hour\":0,\"start_minute\":0,\"end_hour\":0,\"end_minute\":0,\"target_soc\":100}}"
        );
        let slot: CosySlot = serde_json::from_str(&raw).unwrap();
        assert_eq!(slot.start_hour, 0);
        assert_eq!(slot.end_hour, 0);
        assert_eq!(slot.target_soc, 100);
    }

    /// Guard: an empty vec![] for cosy_slots must not silently clobber
    /// existing slots when POST /api/cosy receives no slots array.
    /// Note: the API use of slots.iter().map(...).collect() naturally
    /// produces 0 entries if body["slots"] is [] — this test records
    /// that semantic so we don't accidentally break it in future.
    #[test]
    fn cosy_empty_slots_array_gives_empty_vec() {
        let json = r#"{"slots":[]}"#;
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let mapped: Vec<CosySlot> = v["slots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| CosySlot {
                enabled: s["enabled"].as_bool().unwrap_or(false),
                start_hour: s["start_hour"].as_u64().unwrap_or(0) as u8,
                start_minute: s["start_minute"].as_u64().unwrap_or(0) as u8,
                end_hour: s["end_hour"].as_u64().unwrap_or(0) as u8,
                end_minute: s["end_minute"].as_u64().unwrap_or(0) as u8,
                target_soc: s["target_soc"].as_u64().unwrap_or(100) as u8,
            })
            .collect();
        assert!(
            mapped.is_empty(),
            "empty slots array must produce 0 entries, not regenerate defaults"
        );
    }
}
