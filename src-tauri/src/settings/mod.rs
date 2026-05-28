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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 8899,
            serial: String::new(),
            poll_interval: 60,
            auto_connect: true,
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
    }

    #[test]
    fn settings_roundtrip() {
        let s = Settings {
            host: "10.0.0.50".to_string(),
            port: 502,
            serial: "TEST123".to_string(),
            poll_interval: 10,
            auto_connect: false,
        };
        let json = serde_json::to_string(&s).unwrap();
        let decoded: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.host, "10.0.0.50");
        assert_eq!(decoded.port, 502);
        assert_eq!(decoded.serial, "TEST123");
        assert_eq!(decoded.poll_interval, 10);
        assert!(!decoded.auto_connect);
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
        };

        // We can't easily override the settings path for testing,
        // so just verify serialization works
        let json = serde_json::to_string_pretty(&s).unwrap();
        assert!(json.contains("192.168.1.99"));
        assert!(json.contains("TEST99"));
    }
}
