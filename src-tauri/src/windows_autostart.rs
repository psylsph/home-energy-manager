//! Windows Startup folder autostart (fallback for registry ACL failures).
//!
//! The primary autostart mechanism uses `tauri-plugin-autostart` which writes
//! to the Windows registry (`HKCU\…\Run`). When that fails (e.g. the registry
//! key has restrictive ACLs — see issue #117), this module provides a fallback
//! that creates a `.lnk` shortcut in the per-user Startup folder
//! (`%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\`).
//!
//! The Startup folder approach:
//! - Requires no admin rights (per-user folder)
//! - Avoids registry ACL issues entirely
//! - Shows up in Task Manager's Startup tab
//! - Is how Windows natively manages startup apps
//!
//! On non-Windows platforms all functions are no-ops.

#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;

/// Returns the path to the current user's Windows Startup folder.
fn startup_folder() -> PathBuf {
    let appdata =
        std::env::var("APPDATA").expect("APPDATA environment variable must be set on Windows");
    PathBuf::from(appdata).join("Microsoft\\Windows\\Start Menu\\Programs\\Startup")
}

/// The filename used for the shortcut in the Startup folder.
const SHORTCUT_NAME: &str = "Home Energy Manager.lnk";

/// Create a `.lnk` shortcut in the Windows Startup folder using PowerShell's
/// `WScript.Shell` COM object (available on all Windows 10+ installs).
///
/// Returns `Ok(())` on success, or an error message string on failure.
pub fn enable() -> Result<(), String> {
    let startup = startup_folder();
    let shortcut_path = startup.join(SHORTCUT_NAME);

    // Get the current executable path.
    let exe_path =
        std::env::current_exe().map_err(|e| format!("Failed to get executable path: {e}"))?;
    let exe_str = exe_path.to_string_lossy().to_string();

    // PowerShell one-liner to create a shortcut via COM.
    let ps_script = format!(
        r#"$ws = New-Object -ComObject WScript.Shell; $s = $ws.CreateShortcut('{}'); $s.TargetPath = '{}'; $s.Save()"#,
        shortcut_path
            .to_string_lossy()
            .to_string()
            .replace('\'', "''"),
        exe_str.replace('\'', "''"),
    );

    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_script])
        .output()
        .map_err(|e| format!("Failed to run PowerShell: {e}"))?;

    if output.status.success() {
        tracing::info!(
            "Created Startup folder shortcut at: {}",
            shortcut_path.display()
        );
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("PowerShell shortcut creation failed: {stderr}"))
    }
}

/// Remove the `.lnk` shortcut from the Windows Startup folder.
///
/// Returns `Ok(())` on success, or an error message string on failure.
/// Succeeds if the shortcut doesn't exist (already removed).
pub fn disable() -> Result<(), String> {
    let startup = startup_folder();
    let shortcut_path = startup.join(SHORTCUT_NAME);

    if !shortcut_path.exists() {
        return Ok(());
    }

    std::fs::remove_file(&shortcut_path)
        .map_err(|e| format!("Failed to remove Startup folder shortcut: {e}"))?;

    tracing::info!(
        "Removed Startup folder shortcut at: {}",
        shortcut_path.display()
    );
    Ok(())
}

/// Check whether the `.lnk` shortcut exists in the Windows Startup folder.
///
/// Returns `Ok(true)` if the shortcut is present, `Ok(false)` if not.
pub fn is_enabled() -> Result<bool, String> {
    let startup = startup_folder();
    let shortcut_path = startup.join(SHORTCUT_NAME);
    Ok(shortcut_path.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `startup_folder()` must return a path ending with the expected
    /// Startup folder suffix and must exist (APPDATA is always set in
    /// test environments that run on Windows).
    #[test]
    fn startup_folder_returns_valid_path() {
        let path = startup_folder();
        let path_str = path.to_string_lossy();
        assert!(
            path_str.contains("Start Menu\\Programs\\Startup"),
            "Startup folder path should contain the expected suffix: {path_str}"
        );
        // The parent APPDATA dir must exist.
        assert!(
            path.parent().map_or(false, |p| p.exists()),
            "APPDATA path should exist: {}",
            path.parent().unwrap().display()
        );
    }

    /// `enable()` must create the shortcut file, and `is_enabled()` must
    /// return true after creation.
    #[test]
    fn enable_creates_shortcut() {
        // Only run on Windows.
        if !cfg!(windows) {
            return;
        }

        // Clean up any pre-existing shortcut.
        let _ = disable();

        assert!(
            !is_enabled().unwrap(),
            "shortcut should not exist before enable()"
        );

        enable().expect("enable() should succeed");

        assert!(
            is_enabled().unwrap(),
            "shortcut should exist after enable()"
        );

        // Clean up.
        disable().expect("disable() should succeed");
        assert!(
            !is_enabled().unwrap(),
            "shortcut should not exist after disable()"
        );
    }

    /// `disable()` must succeed even when the shortcut doesn't exist
    /// (idempotent cleanup).
    #[test]
    fn disable_is_idempotent() {
        if !cfg!(windows) {
            return;
        }

        // Remove if present.
        let _ = disable();
        // Should still succeed.
        disable().expect("disable() should be idempotent");
    }

    /// `is_enabled()` must return false when no shortcut exists.
    #[test]
    fn is_enabled_returns_false_when_not_enabled() {
        if !cfg!(windows) {
            return;
        }

        let _ = disable();
        assert!(!is_enabled().unwrap(), "should return false when disabled");
    }

    /// `enable()` must create a valid `.lnk` file (not a directory or
    /// empty file).
    #[test]
    fn enable_creates_valid_lnk_file() {
        if !cfg!(windows) {
            return;
        }

        let _ = disable();
        enable().expect("enable() should succeed");

        let startup = startup_folder();
        let shortcut_path = startup.join(SHORTCUT_NAME);
        assert!(shortcut_path.exists(), "shortcut file should exist");
        assert!(
            shortcut_path.is_file(),
            "shortcut should be a file, not a directory"
        );
        // A .lnk file should be at least a few hundred bytes.
        let metadata = shortcut_path.metadata().expect("metadata");
        assert!(
            metadata.len() > 100,
            ".lnk file should be larger than 100 bytes (was {})",
            metadata.len()
        );

        let _ = disable();
    }
}
