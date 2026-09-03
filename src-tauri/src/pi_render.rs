//! Raspberry Pi WebKitGTK software-rendering workaround (issue #298).
//!
//! On Raspberry Pi OS (Trixie's Wayland/labwc desktop in particular)
//! WebKitGTK's accelerated DMA-BUF renderer produces severe horizontal-line
//! corruption on the first paint; any window resize fixes it for the rest of
//! the session but every fresh launch re-triggers it. Confirmed on multiple
//! Pi models including the Pi 5. Setting `WEBKIT_DISABLE_DMABUF_RENDERER=1`
//! and `WEBKIT_DISABLE_COMPOSITING_MODE=1` before the process starts fixes
//! it cleanly (verified by the reporter in issue #298).
//!
//! The env vars must be in place before GTK/WebKitGTK initialise, so this
//! module is applied at the top of `main()` on the GUI path — before the
//! Tauri builder constructs any webview. Detection is conservative: only
//! hardware whose device-tree model string identifies it as a Raspberry Pi
//! gets the software path; every other Linux desktop keeps hardware
//! acceleration. `scripts/run-with-software-renderer.sh` remains as the
//! manual escape hatch for older installs and forced control.

/// Escape hatch: set to `1` to keep the accelerated renderer even on a Pi.
pub const ALLOW_GPU_RENDERER_ENV: &str = "GIVENERGY_LOCAL_ALLOW_GPU_RENDERER";

/// Where Linux device trees expose the board model (Raspberry Pi OS populates
/// it, e.g. `Raspberry Pi 5 Model B Rev 1.0`).
const DEVICE_TREE_MODEL: &str = "/proc/device-tree/model";

/// Does a device-tree model string identify Raspberry Pi hardware?
///
/// Deliberately a substring check on the full `Raspberry Pi` brand so that
/// unrelated SBCs whose names happen to contain "Pi" (Banana Pi, Orange Pi)
/// do not match. The device tree terminates the string with a NUL byte,
/// which is tolerated.
pub fn is_raspberry_pi_model(model: &str) -> bool {
    model
        .trim_end_matches('\0')
        .to_ascii_lowercase()
        .contains("raspberry pi")
}

/// Which WebKitGTK software-rendering env vars should be applied?
///
/// Returns the `(name, value)` pairs to set, given the current values of
/// `WEBKIT_DISABLE_DMABUF_RENDERER` and `WEBKIT_DISABLE_COMPOSITING_MODE`
/// (`None` = unset). A variable the user has already set — to any value,
/// including `0` to deliberately force the accelerated path — is left
/// untouched, mirroring `scripts/run-with-software-renderer.sh`. When
/// `allow_gpu` is true nothing is set at all.
pub fn software_rendering_overrides(
    dmabuf: Option<&str>,
    compositing: Option<&str>,
    allow_gpu: bool,
) -> &'static [(&'static str, &'static str)] {
    if allow_gpu {
        return &[];
    }
    match (dmabuf.is_none(), compositing.is_none()) {
        (true, true) => &[
            ("WEBKIT_DISABLE_DMABUF_RENDERER", "1"),
            ("WEBKIT_DISABLE_COMPOSITING_MODE", "1"),
        ],
        (true, false) => &[("WEBKIT_DISABLE_DMABUF_RENDERER", "1")],
        (false, true) => &[("WEBKIT_DISABLE_COMPOSITING_MODE", "1")],
        (false, false) => &[],
    }
}

/// Apply the workaround when running on Raspberry Pi hardware.
///
/// Reads the device-tree model; on a Raspberry Pi, sets the WebKitGTK
/// software-rendering env vars the process was launched without (an explicit
/// per-var value from the caller's environment always wins) unless
/// [`ALLOW_GPU_RENDERER_ENV`] is set to a non-empty value other than `0`.
/// A no-op everywhere else. Must run before the Tauri builder constructs a
/// webview — call it at the top of `main()` on the GUI path, where no threads
/// have been spawned yet (env mutation is not thread-safe).
pub fn apply_pi_webkit_workaround() {
    let is_pi = std::fs::read_to_string(DEVICE_TREE_MODEL)
        .map(|model| is_raspberry_pi_model(&model))
        .unwrap_or(false);
    if !is_pi {
        return;
    }
    let allow_gpu = std::env::var(ALLOW_GPU_RENDERER_ENV).is_ok_and(|v| !v.is_empty() && v != "0");
    let dmabuf = std::env::var("WEBKIT_DISABLE_DMABUF_RENDERER").ok();
    let compositing = std::env::var("WEBKIT_DISABLE_COMPOSITING_MODE").ok();
    let overrides =
        software_rendering_overrides(dmabuf.as_deref(), compositing.as_deref(), allow_gpu);
    if overrides.is_empty() {
        return;
    }
    for (name, value) in overrides {
        std::env::set_var(name, value);
    }
    eprintln!(
        "Raspberry Pi detected: forcing WebKitGTK software rendering to avoid \
         first-paint corruption (set {ALLOW_GPU_RENDERER_ENV}=1 to keep the accelerated renderer)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- is_raspberry_pi_model -------------------------------------------------

    #[test]
    fn detects_pi5_model_string() {
        assert!(is_raspberry_pi_model("Raspberry Pi 5 Model B Rev 1.0\n"));
    }

    #[test]
    fn detects_older_pi_families_and_compute_modules() {
        assert!(is_raspberry_pi_model("Raspberry Pi 4 Model B Rev 1.4"));
        assert!(is_raspberry_pi_model("Raspberry Pi 3 Model B Plus Rev 1.3"));
        assert!(is_raspberry_pi_model("Raspberry Pi Zero 2 W Rev 1.0"));
        assert!(is_raspberry_pi_model(
            "Raspberry Pi Compute Module 5 Rev 1.0"
        ));
    }

    #[test]
    fn device_tree_trailing_nul_is_tolerated() {
        assert!(is_raspberry_pi_model("Raspberry Pi 4 Model B Rev 1.4\0"));
    }

    #[test]
    fn detection_is_case_insensitive() {
        assert!(is_raspberry_pi_model("raspberry pi 5 model b"));
    }

    #[test]
    fn non_pi_hardware_is_not_detected() {
        // A different SBC brand that still contains "Pi".
        assert!(!is_raspberry_pi_model("Banana Pi M5"));
        assert!(!is_raspberry_pi_model("Orange Pi 5 Plus"));
        assert!(!is_raspberry_pi_model("Generic x86-64 Desktop"));
        assert!(!is_raspberry_pi_model(""));
        // "raspberry" alone is not the brand.
        assert!(!is_raspberry_pi_model("Raspberry Ripple Test Board"));
    }

    // --- software_rendering_overrides ------------------------------------------

    #[test]
    fn both_vars_set_when_neither_is_in_the_environment() {
        assert_eq!(
            software_rendering_overrides(None, None, false),
            &[
                ("WEBKIT_DISABLE_DMABUF_RENDERER", "1"),
                ("WEBKIT_DISABLE_COMPOSITING_MODE", "1"),
            ]
        );
    }

    #[test]
    fn explicit_user_values_are_respected_per_var() {
        // A value of "0" is a deliberate choice of the accelerated path for
        // that one var; the other still gets its software-rendering value.
        assert_eq!(
            software_rendering_overrides(Some("0"), None, false),
            &[("WEBKIT_DISABLE_COMPOSITING_MODE", "1")]
        );
        assert_eq!(
            software_rendering_overrides(None, Some("0"), false),
            &[("WEBKIT_DISABLE_DMABUF_RENDERER", "1")]
        );
        assert_eq!(
            software_rendering_overrides(Some("1"), Some("1"), false),
            &[]
        );
    }

    #[test]
    fn gpu_opt_out_keeps_everything_alone() {
        assert_eq!(software_rendering_overrides(None, None, true), &[]);
        assert_eq!(software_rendering_overrides(Some("0"), None, true), &[]);
    }
}
