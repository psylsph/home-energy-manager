//! Application settings with file-based persistence.
//!
//! Settings are saved as JSON to `~/.givenergy-local/settings.json`
//! (`%USERPROFILE%\.givenergy-local\settings.json` on Windows).
//! Override with the `GIVENERGY_LOCAL_CONFIG_DIR` environment variable.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

/// Serializes concurrent saves so two async tasks don't race on the same
/// temp file name. Without this, `save()` could fail with "No such file or
/// directory" when the temp was already renamed by another save.
static SETTINGS_LOCK: Mutex<()> = Mutex::new(());

/// A single tariff time window with a rate in £/kWh.
///
/// The day is tiled by an ordered list of these slots covering `[00:00, 24:00)`.
/// The `end` field may be `"24:00"` (internally minute 1440) for the final
/// slot to complete the tiling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TariffSlot {
    /// Start time in "HH:MM" format (24h).
    pub start: String,
    /// End time in "HH:MM" format (24h). Use "24:00" for the final slot.
    pub end: String,
    /// Electricity rate in £/kWh for this window.
    pub rate: f64,
}

/// Tariff configuration as a list of time windows, each with its own price.
///
/// This generalizes the previous fixed peak/off-peak model to support any
/// time-of-use tariff (Octopus Flux, Cosy, Agile, flat, etc.). The day is
/// tiled by an ascending list of [`TariffSlot`]s. See
/// [`rate_for_minutes`](Self::rate_for_minutes) for the lookup logic.
///
/// **Backward compatibility:** a custom `Deserialize` accepts both the old
/// shape (`peak_rate`, `off_peak_rate`, `off_peak_start`, `off_peak_end`)
/// and the new shape (`slots`). Old files are migrated to 3 slots that
/// reproduce the exact previous behaviour, including midnight-crossing
/// windows. After the first save, only the new `slots` shape is written.
#[derive(Debug, Clone, Serialize)]
pub struct TariffConfig {
    /// Time windows in ascending order by start, tiling `[00:00, 24:00)`.
    pub slots: Vec<TariffSlot>,
}

impl Default for TariffConfig {
    fn default() -> Self {
        // Same default rates as the previous peak/off-peak model:
        //   peak 28.5p/kWh, off-peak 9p/kWh, off-peak 00:30–05:30.
        Self {
            slots: vec![
                TariffSlot {
                    start: "00:00".to_string(),
                    end: "00:30".to_string(),
                    rate: 0.285,
                },
                TariffSlot {
                    start: "00:30".to_string(),
                    end: "05:30".to_string(),
                    rate: 0.09,
                },
                TariffSlot {
                    start: "05:30".to_string(),
                    end: "24:00".to_string(),
                    rate: 0.285,
                },
            ],
        }
    }
}

/// Parse a `"HH:MM"` time string into minutes since midnight.
/// Returns `None` if the string is malformed. `"24:00"` is accepted → 1440.
pub(crate) fn parse_hhmm_to_minutes(s: &str) -> Option<u16> {
    let mut it = s.split(':');
    let h: u16 = it.next()?.trim().parse().ok()?;
    let m: u16 = it.next()?.trim().parse().ok()?;
    if h > 24 || m > 59 {
        return None;
    }
    let mins = h * 60 + m;
    if mins > 1440 {
        return None;
    }
    Some(mins)
}

impl TariffConfig {
    /// Look up the rate for a given minute of the day `[0, 1440)`.
    ///
    /// Lookup = first slot whose `[start, end)` contains the minute. If no
    /// slot covers the minute (tail gap from a hand-edited/malformed file),
    /// fall back to the **last slot's rate** to defend against gaps at the
    /// end of the day. Returns `None` only when there are zero slots.
    pub fn rate_for_minutes(&self, minutes: u16) -> Option<f64> {
        if self.slots.is_empty() {
            return None;
        }
        for slot in &self.slots {
            let Some(start) = parse_hhmm_to_minutes(&slot.start) else {
                continue;
            };
            let Some(end) = parse_hhmm_to_minutes(&slot.end) else {
                continue;
            };
            // Normal window (start < end): minute in [start, end).
            // We don't handle midnight-crossing within a single slot — the
            // model tiles the day with non-crossing windows. The legacy
            // deserializer splits crossing windows into non-crossing slots.
            if end > start && minutes >= start && minutes < end {
                return Some(slot.rate);
            }
        }
        // Tail gap fallback: use the last slot's rate.
        self.slots.last().map(|s| s.rate)
    }
}

// --- Custom Deserialize: accept both legacy (peak/off-peak) and new (slots) ---

impl<'de> serde::Deserialize<'de> for TariffConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Shape {
            /// New shape: explicit list of time windows.
            New { slots: Vec<TariffSlot> },
            /// Legacy shape: two scalars + one off-peak window.
            Legacy {
                peak_rate: f64,
                off_peak_rate: f64,
                off_peak_start: String,
                off_peak_end: String,
            },
        }

        match Shape::deserialize(deserializer)? {
            Shape::New { slots } => {
                if slots.is_empty() {
                    // Empty slots list → use default (safe fallback).
                    Ok(TariffConfig::default())
                } else {
                    Ok(TariffConfig { slots })
                }
            }
            Shape::Legacy {
                peak_rate,
                off_peak_rate,
                off_peak_start,
                off_peak_end,
            } => {
                // Synthesize slots that reproduce the exact legacy behaviour.
                let slots = legacy_to_slots(
                    peak_rate,
                    off_peak_rate,
                    &off_peak_start,
                    &off_peak_end,
                );
                tracing::info!(
                    peak_rate,
                    off_peak_rate,
                    off_peak_start,
                    off_peak_end,
                    n_slots = slots.len(),
                    "Migrated legacy tariff config to slots"
                );
                Ok(TariffConfig { slots })
            }
        }
    }
}

/// Convert legacy peak/off-peak fields into a tiled list of slots.
///
/// Two cases:
/// - `start <= end` (normal off-peak inside the day):
///   `[00:00→start]=peak`, `[start→end]=off_peak`, `[end→24:00]=peak`
/// - `start > end` (crosses midnight):
///   `[00:00→end]=off_peak`, `[end→start]=peak`, `[start→24:00]=off_peak`
///
/// Adjacent slots with the same rate are merged so the output is minimal.
fn legacy_to_slots(
    peak_rate: f64,
    off_peak_rate: f64,
    off_peak_start: &str,
    off_peak_end: &str,
) -> Vec<TariffSlot> {
    let start = parse_hhmm_to_minutes(off_peak_start);
    let end = parse_hhmm_to_minutes(off_peak_end);

    let (op_start, op_end) = match (start, end) {
        (Some(s), Some(e)) => (s, e),
        // Malformed times → can't determine the window, emit a flat day at peak.
        _ => {
            tracing::warn!(
                off_peak_start, off_peak_end,
                "Legacy tariff has unparseable off-peak times, using flat peak rate"
            );
            return vec![TariffSlot {
                start: "00:00".to_string(),
                end: "24:00".to_string(),
                rate: peak_rate,
            }];
        }
    };

    // Build raw segments as (start_minute, end_minute, rate).
    let raw: Vec<(u16, u16, f64)> = if op_start <= op_end {
        // Normal: off-peak is inside the day.
        vec![
            (0, op_start, peak_rate),
            (op_start, op_end, off_peak_rate),
            (op_end, 1440, peak_rate),
        ]
    } else {
        // Crosses midnight: off-peak wraps around.
        vec![
            (0, op_end, off_peak_rate),
            (op_end, op_start, peak_rate),
            (op_start, 1440, off_peak_rate),
        ]
    };

    // Convert to TariffSlot strings, then merge adjacent same-rate slots.
    let mut slots: Vec<TariffSlot> = raw
        .into_iter()
        .filter(|(s, e, _)| *s < *e) // drop zero-length segments
        .map(|(s, e, r)| TariffSlot {
            start: minutes_to_hhmm(s),
            end: minutes_to_hhmm(e),
            rate: r,
        })
        .collect();

    // Merge adjacent slots with the same rate (e.g. peak before and after
    // a normal off-peak window collapses into one peak slot spanning
    // 00:00→start + end→24:00 when the off-peak window is in the middle).
    let mut merged: Vec<TariffSlot> = Vec::with_capacity(slots.len());
    for slot in slots.drain(..) {
        if let Some(last) = merged.last_mut() {
            if (last.rate - slot.rate).abs() < 1e-12 && last.end == slot.start {
                last.end = slot.end;
                continue;
            }
        }
        merged.push(slot);
    }

    if merged.is_empty() {
        // Shouldn't happen, but defend against degenerate input.
        return vec![TariffSlot {
            start: "00:00".to_string(),
            end: "24:00".to_string(),
            rate: peak_rate,
        }];
    }

    // Ensure the first slot starts at 00:00.
    if merged[0].start != "00:00" {
        merged.insert(
            0,
            TariffSlot {
                start: "00:00".to_string(),
                end: merged[0].start.clone(),
                rate: peak_rate,
            },
        );
    }
    // Ensure the last slot ends at 24:00.
    if merged.last().unwrap().end != "24:00" {
        merged.last_mut().unwrap().end = "24:00".to_string();
    }

    merged
}

/// Convert minutes-since-midnight to "HH:MM" string. 1440 → "24:00".
fn minutes_to_hhmm(minutes: u16) -> String {
    let h = minutes / 60;
    let m = minutes % 60;
    format!("{h:02}:{m:02}")
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

impl CosySlot {
    /// Check whether a given time in minutes since midnight falls within
    /// this slot, handling slots that cross midnight (e.g. 22:00-05:00).
    pub fn contains_minutes(&self, now_minutes: u16) -> bool {
        if !self.enabled {
            return false;
        }
        let start = self.start_hour as u16 * 60 + self.start_minute as u16;
        let end = self.end_hour as u16 * 60 + self.end_minute as u16;
        if end <= start {
            // Crosses midnight (e.g. 22:00-05:00)
            now_minutes >= start || now_minutes < end
        } else {
            now_minutes >= start && now_minutes < end
        }
    }
}

/// Check if the current time falls within any enabled Cosy slot.
/// Returns the target SOC of the first matching slot, or `None` if no slot matches.
pub fn cosy_active_slot(now_minutes: u16, slots: &[CosySlot]) -> Option<u8> {
    for slot in slots {
        if slot.contains_minutes(now_minutes) {
            return Some(slot.target_soc);
        }
    }
    None
}

/// Find the next upcoming enabled Cosy slot after `now_minutes`.
///
/// Returns `(slot_index, &CosySlot, minutes_until_start)`. Scans today first,
/// then wraps around midnight to check the next day. Returns `None` if there
/// are no enabled slots at all.
pub fn find_next_cosy_slot<'a>(
    now_minutes: u16,
    slots: &'a [CosySlot],
) -> Option<(usize, &'a CosySlot, u16)> {
    let enabled: Vec<(usize, &CosySlot)> = slots
        .iter()
        .enumerate()
        .filter(|(_, s)| s.enabled)
        .collect();
    if enabled.is_empty() {
        return None;
    }

    let now = now_minutes as i32;
    let day_minutes: i32 = 24 * 60;

    let mut best: Option<(usize, &'a CosySlot, i32)> = None;
    for (idx, slot) in &enabled {
        let start = slot.start_hour as i32 * 60 + slot.start_minute as i32;
        // Minutes until this slot starts (may wrap past midnight).
        let until = (start - now).rem_euclid(day_minutes);
        // If the slot is currently active, `until` would be 0 and we should
        // skip it — we want the NEXT slot, not the current one.
        if slot.contains_minutes(now_minutes) {
            continue;
        }
        match best {
            None => best = Some((*idx, *slot, until)),
            Some((_, _, best_until)) if until < best_until => {
                best = Some((*idx, *slot, until));
            }
            _ => {}
        }
    }
    best.map(|(idx, slot, until)| (idx, slot, until as u16))
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

    /// Agile Octopus mode enabled.
    #[serde(default)]
    pub agile_enabled: bool,
    /// Agile Octopus region code (A-P).
    #[serde(default = "default_agile_region")]
    pub agile_region: String,
    /// Agile Octopus charge threshold in p/kWh.
    #[serde(default = "default_agile_charge_threshold")]
    pub agile_charge_threshold: f64,
    /// Agile Octopus discharge threshold in p/kWh.
    #[serde(default = "default_agile_discharge_threshold")]
    pub agile_discharge_threshold: f64,

    /// Cosy charging mode enabled.
    #[serde(default)]
    pub cosy_enabled: bool,
    /// Cosy charging slots (up to 3, stored locally).
    #[serde(default)]
    pub cosy_slots: Vec<CosySlot>,
    /// Persisted mirror of the in-memory `cosy_active` flag. The poll loop
    /// writes this whenever `cosy_active` transitions so a crash/restart can
    /// detect a missed CosyExit (the inverter was left force-charging after
    /// the slot ended but before the app came back up).
    ///
    /// On startup the poll loop initializes in-memory `cosy_active` from
    /// this field. If it's `true` and the current time is outside any Cosy
    /// slot, the normal state machine will fire CosyExit on the first poll.
    #[serde(default)]
    pub cosy_active_persisted: bool,

    /// Persisted mirror of the in-memory `agile_state`, recorded whenever the
    /// Agile Octopus state machine transitions. Stored as a short string
    /// ("idle"/"charging"/"discharging") so a crash/restart can detect that
    /// the inverter was left mid-charge/discharge and re-evaluate on the
    /// first poll (the in-memory state always restarts at Idle, forcing a
    /// fresh decision + command send).
    #[serde(default)]
    pub agile_state_persisted: String,

    /// Persisted `enable_charge_target` saved before winter mode activated.
    /// `Some` means winter mode was active when the last state was saved.
    #[serde(default)]
    pub auto_winter_saved_enable_target: Option<bool>,
    /// Persisted `target_soc` saved before winter mode activated.
    #[serde(default)]
    pub auto_winter_saved_target_soc: Option<u16>,

    // -- Load discharge limiter --
    /// Whether the load discharge limiter is enabled.
    #[serde(default)]
    pub load_limiter_enabled: bool,
    /// Home power threshold in watts.
    #[serde(default = "default_ll_threshold")]
    pub load_limiter_threshold_w: u32,
    /// Minutes the load must stay above/below the threshold.
    #[serde(default = "default_ll_trigger_delay")]
    pub load_limiter_trigger_delay_minutes: u32,
    /// Activation window start hour.
    #[serde(default)]
    pub load_limiter_start_hour: u8,
    /// Activation window start minute.
    #[serde(default)]
    pub load_limiter_start_minute: u8,
    /// Activation window end hour.
    #[serde(default)]
    pub load_limiter_end_hour: u8,
    /// Activation window end minute.
    #[serde(default)]
    pub load_limiter_end_minute: u8,
    /// Persisted active flag so a crash/restart can detect the limiter was mid-pause.
    #[serde(default)]
    pub load_limiter_active_persisted: bool,

    /// Panels hidden from the bottom navigation bar (e.g. ["power", "battery", "solar", "meters", "history"]) .
    #[serde(default)]
    pub hidden_panels: Vec<String>,

    /// EV Charger IP address (standard Modbus TCP, port 502).
    #[serde(default)]
    pub evc_host: String,
    /// EV Charger Modbus port (default 502 — standard Modbus, not proprietary).
    #[serde(default = "default_evc_port")]
    pub evc_port: u16,

    /// When true, skip auto-discovery of the dongle on persistent connection failure.
    #[serde(default)]
    pub disable_auto_discovery: bool,

    /// When true, skip optional model-specific poll blocks (extended slots,
    /// AC config, three-phase config) to reduce per-cycle timeout exposure
    /// on chronically unstable dongles. Standard blocks and battery reads
    /// are always performed. Default: false.
    #[serde(default)]
    pub minimal_telemetry_mode: bool,

    /// Full import tariff config with peak/off-peak rates and times.
    /// Falls back to legacy `import_tariff` if `None`.
    #[serde(default)]
    pub import_tariff_config: Option<TariffConfig>,
    /// Full export tariff config with peak/off-peak rates and times.
    /// Falls back to legacy `export_tariff` if `None`.
    #[serde(default)]
    pub export_tariff_config: Option<TariffConfig>,

    // -- Email alerts config --
    /// Alert thresholds and Brevo email integration.
    #[serde(default)]
    pub alerts_config: AlertsConfig,
}

fn default_http_port() -> u16 {
    7337
}

fn default_evc_port() -> u16 {
    502
}

fn default_ll_threshold() -> u32 {
    3000
}

fn default_ll_trigger_delay() -> u32 {
    5
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

fn default_agile_region() -> String {
    "A".to_string()
}

fn default_agile_charge_threshold() -> f64 {
    10.0
}

fn default_agile_discharge_threshold() -> f64 {
    30.0
}

// ===========================================================================
// Email alerts config
// ===========================================================================

/// Threshold configuration for email alerts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertsConfig {
    /// Master toggle for all alerts.
    #[serde(default)]
    pub enabled: bool,
    /// Telegram bot token (from @BotFather).
    #[serde(default)]
    pub telegram_bot_token: String,
    /// Telegram chat ID to send alerts to.
    #[serde(default)]
    pub telegram_chat_id: String,
    /// Minimum cooldown between same-type alerts (minutes).
    pub cooldown_minutes: u32,

    // -- Thresholds --
    /// Battery temperature alert minimum (°C). 0 = disabled.
    pub batt_temp_min: f32,
    /// Battery temperature alert maximum (°C). 0 = disabled.
    pub batt_temp_max: f32,
    /// Battery SOC alert minimum (%). 0 = disabled.
    pub soc_min: u8,
    /// Battery SOC alert maximum (%). 100 = disabled.
    pub soc_max: u8,
    /// Alert on grid loss.
    pub grid_offline_enabled: bool,
    /// Alert on battery over-temperature flag.
    pub battery_over_temp_enabled: bool,
    /// Alert when solar generation sustains above the clipping ceiling.
    #[serde(default)]
    pub solar_clipping_enabled: bool,
    /// Manual clipping ceiling in watts. Solar generation sustained above
    /// this value triggers [`crate::alerts::AlertType::SolarClipping`].
    /// `0` = no ceiling (alert disabled even if `solar_clipping_enabled`).
    /// Users typically set this around their inverter's rated AC output.
    #[serde(default)]
    pub solar_clipping_ceiling_w: u32,

    /// ntfy.sh (or self-hosted) topic for push notifications.
    #[serde(default)]
    pub ntfy_topic: String,
    /// ntfy server URL (default: https://ntfy.sh).
    #[serde(default = "default_ntfy_server")]
    pub ntfy_server: String,

    /// Pushover app API token. Each user registers their own application at
    /// <https://pushover.net/apps/build> (per Pushover's guidance for
    /// distributed OSS apps) and pastes the token here.
    #[serde(default)]
    pub pushover_app_token: String,
    /// Pushover user key (from the user's Pushover account settings).
    #[serde(default)]
    pub pushover_user_key: String,

    // -- Daily consumption report --
    /// Whether to send a daily consumption report.
    pub daily_report_enabled: bool,
    /// Hour to send the daily report (0-23, local time).
    pub daily_report_hour: u8,
    /// Minute to send the daily report (0-59, local time).
    pub daily_report_minute: u8,
}

fn default_ntfy_server() -> String {
    "https://ntfy.sh".to_string()
}

impl Default for AlertsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            telegram_bot_token: String::new(),
            telegram_chat_id: String::new(),
            cooldown_minutes: 30,
            batt_temp_min: 0.0,
            batt_temp_max: 0.0,
            soc_min: 4,
            soc_max: 100,
            grid_offline_enabled: false,
            battery_over_temp_enabled: false,
            solar_clipping_enabled: false,
            solar_clipping_ceiling_w: 0,
            ntfy_topic: String::new(),
            ntfy_server: default_ntfy_server(),
            pushover_app_token: String::new(),
            pushover_user_key: String::new(),
            daily_report_enabled: false,
            daily_report_hour: 8,
            daily_report_minute: 0,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 8899,
            serial: String::new(),
            poll_interval: 60,
            http_port: default_http_port(),
            evc_host: String::new(),
            evc_port: default_evc_port(),
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
            load_limiter_enabled: false,
            load_limiter_threshold_w: default_ll_threshold(),
            load_limiter_trigger_delay_minutes: default_ll_trigger_delay(),
            load_limiter_start_hour: 0,
            load_limiter_start_minute: 0,
            load_limiter_end_hour: 0,
            load_limiter_end_minute: 0,
            load_limiter_active_persisted: false,
            import_tariff_config: None,
            export_tariff_config: None,
            agile_enabled: false,
            agile_region: default_agile_region(),
            agile_charge_threshold: default_agile_charge_threshold(),
            agile_discharge_threshold: default_agile_discharge_threshold(),
            cosy_enabled: false,
            cosy_slots: (0..3).map(|_| CosySlot::default()).collect(),
            cosy_active_persisted: false,
            agile_state_persisted: String::new(),
            hidden_panels: Vec::new(),
            alerts_config: AlertsConfig::default(),
            disable_auto_discovery: true,
            minimal_telemetry_mode: false,
        }
    }
}

impl Settings {
    /// Get the settings directory path.
    /// Uses `GIVENERGY_LOCAL_CONFIG_DIR` env var if set, otherwise `~/.givenergy-local/`
    /// (or `%USERPROFILE%\.givenergy-local\` on Windows).
    pub fn settings_dir() -> PathBuf {
        if let Some(dir) = std::env::var_os("GIVENERGY_LOCAL_CONFIG_DIR") {
            return PathBuf::from(dir);
        }

        if let Some(home) = dirs::home_dir() {
            return home.join(".givenergy-local");
        }

        if let Some(home) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(home).join(".givenergy-local");
        }

        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".givenergy-local");
        }

        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".givenergy-local")
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
                    tracing::debug!("Loaded settings from {}", path.display());
                    settings
                }
                Err(e) => {
                    tracing::warn!("Failed to parse settings: {}, using defaults", e);
                    Self::default()
                }
            },
            Err(_) => {
                tracing::info!("No settings file found, using defaults");
                // NOTE: do not auto-save defaults here. A `load()` should be
                // side-effect-free so tests can call it safely without
                // polluting the user's real `~/.givenergy-local/` directory.
                // The directory and file are created on the first explicit
                // save (e.g. when the user configures a host/IP in Settings).
                Self::default()
            }
        }
    }

    /// Save current settings to disk using an atomic write (temp file + rename).
    ///
    /// A global mutex serializes concurrent saves so two async tasks don't
    /// race on the same temp file name — the old fixed temp name caused
    /// "No such file or directory" errors when one rename stole the temp
    /// out from under another. The temp file name also includes a timestamp
    /// to avoid collisions if the mutex is ever removed.
    pub fn save(&self) -> Result<(), String> {
        let _lock = SETTINGS_LOCK
            .lock()
            .map_err(|e| format!("Lock error: {e}"))?;

        let path = Self::settings_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create settings dir: {}", e))?;
        }

        // Keep a rolling backup of the previous good version so any user can
        // manually revert if a save corrupts their config (e.g. a migration
        // that rewrites settings on first load). The .bak always holds the
        // file as it was *before* this save.
        let bak_path = path.with_extension("json.bak");
        if path.exists() {
            if let Err(e) = fs::copy(&path, &bak_path) {
                tracing::warn!("Failed to create settings backup: {e}");
            }
        }

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize settings: {}", e))?;

        // Write to a temp file first so a crash mid-write never corrupts
        // the real file. `rename()` is atomic on POSIX — readers either see
        // the old complete file or the new complete file.
        let tmp_path = path.with_extension("json.tmp");
        // Clean up any orphaned temp file from a previous crash.
        let _ = std::fs::remove_file(&tmp_path);
        fs::write(&tmp_path, &json).map_err(|e| format!("Failed to write temp settings: {}", e))?;
        fs::rename(&tmp_path, &path)
            .map_err(|e| format!("Failed to rename settings file: {}", e))?;

        tracing::debug!("Settings saved to {}", path.display());
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
        assert!(s.evc_host.is_empty());
        assert_eq!(s.evc_port, 502);
        assert!(!s.auto_winter_enabled);
        assert_eq!(s.auto_winter_cold_threshold, 8.0);
        assert_eq!(s.auto_winter_recovery_threshold, 12.0);
        assert_eq!(s.auto_winter_target_soc, 80);
        assert_eq!(s.auto_winter_debounce_readings, 10);
        assert_eq!(s.auto_winter_saved_enable_target, None);
        assert_eq!(s.auto_winter_saved_target_soc, None);
        assert!(!s.load_limiter_enabled);
        assert_eq!(s.load_limiter_threshold_w, 3000);
        assert_eq!(s.load_limiter_trigger_delay_minutes, 5);
        assert_eq!(s.load_limiter_start_hour, 0);
        assert_eq!(s.load_limiter_end_hour, 0);
        assert!(!s.load_limiter_active_persisted);
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
            load_limiter_enabled: true,
            load_limiter_threshold_w: 5000,
            load_limiter_trigger_delay_minutes: 10,
            load_limiter_start_hour: 16,
            load_limiter_start_minute: 0,
            load_limiter_end_hour: 20,
            load_limiter_end_minute: 0,
            load_limiter_active_persisted: false,
            evc_host: String::new(),
            evc_port: default_evc_port(),
            import_tariff_config: None,
            export_tariff_config: None,
            agile_enabled: true,
            agile_region: "B".to_string(),
            agile_charge_threshold: 12.5,
            agile_discharge_threshold: 35.0,
            cosy_enabled: false,
            cosy_slots: vec![],
            cosy_active_persisted: false,
            agile_state_persisted: "discharging".to_string(),
            hidden_panels: Vec::new(),
            alerts_config: AlertsConfig::default(),
            disable_auto_discovery: true,
            minimal_telemetry_mode: true,
        };
        let json = serde_json::to_string(&s).unwrap();
        let decoded: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.host, "10.0.0.50");
        assert_eq!(decoded.port, 502);
        assert_eq!(decoded.serial, "TEST123");
        assert_eq!(decoded.poll_interval, 10);
        assert_eq!(decoded.http_port, 8080);
        assert!(!decoded.auto_connect);
        assert!(decoded.minimal_telemetry_mode);
        assert!(decoded.auto_winter_enabled);
        assert_eq!(decoded.auto_winter_cold_threshold, 5.0);
        assert_eq!(decoded.auto_winter_recovery_threshold, 10.0);
        assert_eq!(decoded.auto_winter_target_soc, 90);
        assert_eq!(decoded.auto_winter_debounce_readings, 5);
        assert_eq!(decoded.auto_winter_saved_enable_target, Some(true));
        assert_eq!(decoded.auto_winter_saved_target_soc, Some(80));
        assert!(decoded.load_limiter_enabled);
        assert_eq!(decoded.load_limiter_threshold_w, 5000);
        assert_eq!(decoded.load_limiter_trigger_delay_minutes, 10);
        assert_eq!(decoded.load_limiter_start_hour, 16);
        assert_eq!(decoded.load_limiter_end_hour, 20);
    }

    /// AlertsConfig pushover fields must survive a full JSON round-trip and
    /// default to empty strings (so existing on-disk `settings.json` files
    /// written before Pushover shipped still load without error). See #101.
    #[test]
    fn alerts_config_pushover_roundtrip_and_defaults() {
        let mut cfg = AlertsConfig::default();
        // Defaults are empty — forward-compat with pre-#101 settings files.
        assert_eq!(cfg.pushover_app_token, "");
        assert_eq!(cfg.pushover_user_key, "");

        cfg.pushover_app_token = "test-app-token-123".to_string();
        cfg.pushover_user_key = "test-user-key-456".to_string();
        let json = serde_json::to_string(&cfg).unwrap();
        let decoded: AlertsConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.pushover_app_token, "test-app-token-123");
        assert_eq!(decoded.pushover_user_key, "test-user-key-456");
    }

    /// A settings.json written before Pushover shipped (no pushover_* keys)
    /// must still deserialize into the current struct, with the pushover
    /// fields filling in as empty-string defaults via `#[serde(default)]`.
    #[test]
    fn legacy_alerts_config_without_pushover_loads() {
        let legacy = r#"{
            "enabled": false,
            "telegram_bot_token": "",
            "telegram_chat_id": "",
            "cooldown_minutes": 30,
            "batt_temp_min": 0.0,
            "batt_temp_max": 0.0,
            "soc_min": 4,
            "soc_max": 100,
            "grid_offline_enabled": false,
            "battery_over_temp_enabled": false,
            "solar_clipping_enabled": false,
            "solar_clipping_ceiling_w": 0,
            "ntfy_topic": "",
            "ntfy_server": "https://ntfy.sh",
            "daily_report_enabled": false,
            "daily_report_hour": 8,
            "daily_report_minute": 0
        }"#;
        let decoded: AlertsConfig = serde_json::from_str(legacy).unwrap();
        assert_eq!(decoded.pushover_app_token, "");
        assert_eq!(decoded.pushover_user_key, "");
        assert_eq!(decoded.cooldown_minutes, 30);
        assert_eq!(decoded.ntfy_server, "https://ntfy.sh");
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
            load_limiter_enabled: false,
            load_limiter_threshold_w: 3000,
            load_limiter_trigger_delay_minutes: 5,
            load_limiter_start_hour: 0,
            load_limiter_start_minute: 0,
            load_limiter_end_hour: 0,
            load_limiter_end_minute: 0,
            load_limiter_active_persisted: false,
            evc_host: "192.168.1.200".to_string(),
            evc_port: 502,
            import_tariff_config: None,
            export_tariff_config: None,
            agile_enabled: false,
            agile_region: "A".to_string(),
            agile_charge_threshold: 10.0,
            agile_discharge_threshold: 30.0,
            cosy_enabled: false,
            cosy_slots: vec![],
            cosy_active_persisted: false,
            agile_state_persisted: String::new(),
            hidden_panels: Vec::new(),
            alerts_config: AlertsConfig::default(),
            disable_auto_discovery: true,
            minimal_telemetry_mode: false,
        };

        // We can't easily override the settings path for testing,
        // so just verify serialization works
        let json = serde_json::to_string_pretty(&s).unwrap();
        assert!(json.contains("192.168.1.99"));
        assert!(json.contains("TEST99"));
        assert!(json.contains("192.168.1.200"));
        assert!(json.contains("\"evc_port\": 502"));
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
        let raw = "{\"enabled\":false,\"start_hour\":0,\"start_minute\":0,\"end_hour\":0,\"end_minute\":0,\"target_soc\":100}";

        let slot: CosySlot = serde_json::from_str(raw).unwrap();
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

    // ======================================================================
    // Cosy slot timing logic tests
    // ======================================================================

    #[test]
    fn cosy_slot_does_not_match_when_disabled() {
        let slot = CosySlot {
            enabled: false,
            start_hour: 2,
            start_minute: 0,
            end_hour: 5,
            end_minute: 0,
            target_soc: 100,
        };
        assert!(!slot.contains_minutes(180)); // 03:00, slot is disabled
    }

    #[test]
    fn cosy_slot_matches_normal_range() {
        let slot = CosySlot {
            enabled: true,
            start_hour: 2,
            start_minute: 0,
            end_hour: 5,
            end_minute: 0,
            target_soc: 80,
        };
        // Before start
        assert!(!slot.contains_minutes(119)); // 01:59
                                              // At start
        assert!(slot.contains_minutes(120)); // 02:00
                                             // Middle
        assert!(slot.contains_minutes(180)); // 03:00
                                             // Just before end
        assert!(slot.contains_minutes(299)); // 04:59
                                             // At end (end is exclusive)
        assert!(!slot.contains_minutes(300)); // 05:00
    }

    #[test]
    fn cosy_slot_midnight_crossing() {
        // Slot from 22:00 to 05:30 (crosses midnight)
        let slot = CosySlot {
            enabled: true,
            start_hour: 22,
            start_minute: 0,
            end_hour: 5,
            end_minute: 30,
            target_soc: 100,
        };
        // Before start on the first day
        assert!(!slot.contains_minutes(21 * 60 + 59)); // 21:59
                                                       // After start on the first day
        assert!(slot.contains_minutes(22 * 60)); // 22:00
                                                 // Middle of the night
        assert!(slot.contains_minutes(2 * 60 + 30)); // 02:30
                                                     // Just before end
        assert!(slot.contains_minutes(5 * 60 + 29)); // 05:29
                                                     // At end (exclusive)
        assert!(!slot.contains_minutes(5 * 60 + 30)); // 05:30
                                                      // Middle of the next day (outside slot)
        assert!(!slot.contains_minutes(14 * 60)); // 14:00
    }

    #[test]
    fn cosy_midnight_exact_boundary() {
        // Slot from 00:00 to 06:00 — does not cross midnight
        let slot = CosySlot {
            enabled: true,
            start_hour: 0,
            start_minute: 0,
            end_hour: 6,
            end_minute: 0,
            target_soc: 90,
        };
        assert!(slot.contains_minutes(0)); // 00:00
        assert!(slot.contains_minutes(359)); // 05:59
        assert!(!slot.contains_minutes(360)); // 06:00 (end exclusive)
    }

    #[test]
    fn cosy_active_slot_finds_first_match() {
        let slots = vec![
            CosySlot {
                enabled: true,
                start_hour: 0,
                start_minute: 30,
                end_hour: 5,
                end_minute: 30,
                target_soc: 100,
            },
            CosySlot {
                enabled: true,
                start_hour: 13,
                start_minute: 0,
                end_hour: 16,
                end_minute: 0,
                target_soc: 80,
            },
            CosySlot {
                enabled: true,
                start_hour: 20,
                start_minute: 0,
                end_hour: 22,
                end_minute: 0,
                target_soc: 100,
            },
        ];
        // First slot matches (00:30-05:30)
        assert_eq!(cosy_active_slot(2 * 60, &slots), Some(100));
        // Second slot matches (13:00-16:00)
        assert_eq!(cosy_active_slot(14 * 60 + 30, &slots), Some(80));
        // Third slot matches (20:00-22:00)
        assert_eq!(cosy_active_slot(21 * 60, &slots), Some(100));
        // Gap between slots
        assert_eq!(cosy_active_slot(11 * 60, &slots), None);
        assert_eq!(cosy_active_slot(18 * 60, &slots), None);
        // Exact end-of-slot boundaries (exclusive): the cosy state machine
        // relies on these returning None so it fires CosyExit at the correct
        // tick for every slot, not just slot 1.
        assert_eq!(cosy_active_slot(5 * 60 + 30, &slots), None, "slot 1 end");
        assert_eq!(cosy_active_slot(16 * 60, &slots), None, "slot 2 end");
        assert_eq!(cosy_active_slot(22 * 60, &slots), None, "slot 3 end");
        // And one minute before each end still matches.
        assert_eq!(
            cosy_active_slot(5 * 60 + 29, &slots),
            Some(100),
            "slot 1 last min"
        );
        assert_eq!(
            cosy_active_slot(15 * 60 + 59, &slots),
            Some(80),
            "slot 2 last min"
        );
        assert_eq!(
            cosy_active_slot(21 * 60 + 59, &slots),
            Some(100),
            "slot 3 last min"
        );
    }

    #[test]
    fn cosy_active_slot_returns_none_when_no_slots() {
        assert_eq!(cosy_active_slot(12 * 60, &[]), None);
    }

    #[test]
    fn cosy_active_slot_skips_disabled_slots() {
        let slots = vec![
            CosySlot {
                enabled: false,
                start_hour: 2,
                start_minute: 0,
                end_hour: 5,
                end_minute: 0,
                target_soc: 100,
            },
            CosySlot {
                enabled: true,
                start_hour: 6,
                start_minute: 0,
                end_hour: 8,
                end_minute: 0,
                target_soc: 90,
            },
        ];
        // Disabled slot at 03:00 should not match
        assert_eq!(cosy_active_slot(3 * 60, &slots), None);
        // Enabled slot at 07:00 should match
        assert_eq!(cosy_active_slot(7 * 60, &slots), Some(90));
    }

    #[test]
    fn cosy_active_slot_midnight_crossing_first_preferred() {
        // Two midnight-crossing slots, first one should match
        let slots = vec![
            CosySlot {
                enabled: true,
                start_hour: 22,
                start_minute: 0,
                end_hour: 0,
                end_minute: 30,
                target_soc: 100,
            },
            CosySlot {
                enabled: true,
                start_hour: 0,
                start_minute: 30,
                end_hour: 5,
                end_minute: 0,
                target_soc: 80,
            },
        ];
        // At 23:00, first slot matches
        assert_eq!(cosy_active_slot(23 * 60, &slots), Some(100));
        // At 00:15, first slot matches (it crosses midnight and ends at 00:30)
        assert_eq!(cosy_active_slot(15, &slots), Some(100));
        // At 00:45, second slot matches
        assert_eq!(cosy_active_slot(45, &slots), Some(80));
    }

    // ======================================================================
    // find_next_cosy_slot tests
    // ======================================================================

    #[test]
    fn find_next_slot_basic() {
        let slots = vec![
            CosySlot {
                enabled: true,
                start_hour: 2,
                start_minute: 0,
                end_hour: 5,
                end_minute: 30,
                target_soc: 100,
            },
            CosySlot {
                enabled: true,
                start_hour: 13,
                start_minute: 0,
                end_hour: 16,
                end_minute: 0,
                target_soc: 80,
            },
            CosySlot {
                enabled: true,
                start_hour: 20,
                start_minute: 0,
                end_hour: 22,
                end_minute: 0,
                target_soc: 100,
            },
        ];
        // At 01:00, next slot is slot 0 (starts 02:00, 60 min away)
        let (idx, _, until) = find_next_cosy_slot(60, &slots).unwrap();
        assert_eq!(idx, 0);
        assert_eq!(until, 60);
        // At 10:00, next slot is slot 1 (starts 13:00, 180 min away)
        let (idx, _, until) = find_next_cosy_slot(10 * 60, &slots).unwrap();
        assert_eq!(idx, 1);
        assert_eq!(until, 180);
        // At 17:00, next slot is slot 2 (starts 20:00, 180 min away)
        let (idx, _, until) = find_next_cosy_slot(17 * 60, &slots).unwrap();
        assert_eq!(idx, 2);
        assert_eq!(until, 180);
        // At 23:00, wraps to next day: slot 0 (starts 02:00, 180 min away)
        let (idx, _, until) = find_next_cosy_slot(23 * 60, &slots).unwrap();
        assert_eq!(idx, 0);
        assert_eq!(until, 180);
    }

    #[test]
    fn find_next_slot_skips_currently_active() {
        let slots = vec![
            CosySlot {
                enabled: true,
                start_hour: 2,
                start_minute: 0,
                end_hour: 5,
                end_minute: 0,
                target_soc: 100,
            },
            CosySlot {
                enabled: true,
                start_hour: 13,
                start_minute: 0,
                end_hour: 16,
                end_minute: 0,
                target_soc: 80,
            },
        ];
        // At 03:00 (inside slot 0), next should be slot 1 (starts 13:00, 600 min)
        let (idx, _, until) = find_next_cosy_slot(3 * 60, &slots).unwrap();
        assert_eq!(idx, 1);
        assert_eq!(until, 600);
        // At 14:00 (inside slot 1), wraps to next day: slot 0 (starts 02:00, 720 min)
        let (idx, _, until) = find_next_cosy_slot(14 * 60, &slots).unwrap();
        assert_eq!(idx, 0);
        assert_eq!(until, 720);
    }

    #[test]
    fn find_next_slot_returns_none_when_no_enabled() {
        let slots = vec![CosySlot {
            enabled: false,
            start_hour: 2,
            start_minute: 0,
            end_hour: 5,
            end_minute: 0,
            target_soc: 100,
        }];
        assert!(find_next_cosy_slot(0, &slots).is_none());
    }

    #[test]
    fn find_next_slot_returns_none_when_empty() {
        assert!(find_next_cosy_slot(0, &[] as &[CosySlot]).is_none());
    }

    // ======================================================================
    // TariffConfig slot-based model tests
    // ======================================================================

    #[test]
    fn tariff_default_has_three_slots() {
        let cfg = TariffConfig::default();
        assert_eq!(cfg.slots.len(), 3);
        assert_eq!(cfg.slots[0].start, "00:00");
        assert_eq!(cfg.slots[1].rate, 0.09); // off-peak
        assert_eq!(cfg.slots.last().unwrap().end, "24:00");
    }

    #[test]
    fn tariff_rate_for_minutes_finds_off_peak() {
        let cfg = TariffConfig::default();
        // 02:00 = minute 120 → off-peak (00:30–05:30)
        assert_eq!(cfg.rate_for_minutes(120), Some(0.09));
    }

    #[test]
    fn tariff_rate_for_minutes_finds_peak() {
        let cfg = TariffConfig::default();
        // 12:00 = minute 720 → peak
        assert_eq!(cfg.rate_for_minutes(720), Some(0.285));
    }

    #[test]
    fn tariff_rate_for_minutes_boundary() {
        let cfg = TariffConfig::default();
        // 00:30 = minute 30 → start of off-peak (inclusive)
        assert_eq!(cfg.rate_for_minutes(30), Some(0.09));
        // 05:30 = minute 330 → end of off-peak (exclusive) → peak
        assert_eq!(cfg.rate_for_minutes(330), Some(0.285));
    }

    #[test]
    fn tariff_rate_for_minutes_empty_returns_none() {
        let cfg = TariffConfig { slots: vec![] };
        assert_eq!(cfg.rate_for_minutes(0), None);
    }

    #[test]
    fn tariff_rate_for_minutes_tail_gap_uses_last_rate() {
        // Hand-edited config with a gap at the end: last slot ends at 20:00.
        let cfg = TariffConfig {
            slots: vec![TariffSlot {
                start: "00:00".to_string(),
                end: "20:00".to_string(),
                rate: 0.20,
            }],
        };
        // 22:00 = minute 1320 → no slot covers it → fallback to last rate.
        assert_eq!(cfg.rate_for_minutes(1320), Some(0.20));
    }

    /// Legacy peak/off-peak JSON must deserialize into slots that reproduce
    /// the exact same rate at every minute of the day.
    #[test]
    fn tariff_legacy_deserialize_normal_window() {
        let legacy = r#"{
            "peak_rate": 0.30,
            "off_peak_rate": 0.10,
            "off_peak_start": "00:30",
            "off_peak_end": "05:30"
        }"#;
        let cfg: TariffConfig = serde_json::from_str(legacy).unwrap();
        // Should produce 3 slots: peak(00:00-00:30), off-peak(00:30-05:30), peak(05:30-24:00).
        assert_eq!(cfg.slots.len(), 3);
        // 02:00 → off-peak
        assert_eq!(cfg.rate_for_minutes(120), Some(0.10));
        // 12:00 → peak
        assert_eq!(cfg.rate_for_minutes(720), Some(0.30));
        // 00:00 → peak (before off-peak starts)
        assert_eq!(cfg.rate_for_minutes(0), Some(0.30));
    }

    /// Legacy midnight-crossing off-peak window (start > end) must
    /// deserialize correctly.
    #[test]
    fn tariff_legacy_deserialize_midnight_cross() {
        let legacy = r#"{
            "peak_rate": 0.30,
            "off_peak_rate": 0.10,
            "off_peak_start": "23:00",
            "off_peak_end": "05:00"
        }"#;
        let cfg: TariffConfig = serde_json::from_str(legacy).unwrap();
        // 02:00 → off-peak (past midnight, before 05:00)
        assert_eq!(cfg.rate_for_minutes(120), Some(0.10));
        // 04:59 → off-peak
        assert_eq!(cfg.rate_for_minutes(299), Some(0.10));
        // 05:00 → peak
        assert_eq!(cfg.rate_for_minutes(300), Some(0.30));
        // 12:00 → peak
        assert_eq!(cfg.rate_for_minutes(720), Some(0.30));
        // 23:30 → off-peak (after 23:00)
        assert_eq!(cfg.rate_for_minutes(23 * 60 + 30), Some(0.10));
    }

    /// New shape (explicit slots) round-trips through serialize/deserialize.
    #[test]
    fn tariff_new_shape_roundtrip() {
        let cfg = TariffConfig {
            slots: vec![
                TariffSlot {
                    start: "00:00".to_string(),
                    end: "16:00".to_string(),
                    rate: 0.35,
                },
                TariffSlot {
                    start: "16:00".to_string(),
                    end: "19:00".to_string(),
                    rate: 0.15,
                },
                TariffSlot {
                    start: "19:00".to_string(),
                    end: "24:00".to_string(),
                    rate: 0.35,
                },
            ],
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let decoded: TariffConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.slots.len(), 3);
        // 17:00 = minute 1020 → middle slot (cheap rate)
        assert_eq!(decoded.rate_for_minutes(1020), Some(0.15));
        // 20:00 = minute 1200 → last slot (peak)
        assert_eq!(decoded.rate_for_minutes(1200), Some(0.35));
    }

    /// Legacy unparseable times → safe fallback (flat peak day).
    #[test]
    fn tariff_legacy_malformed_times_falls_back() {
        let legacy = r#"{
            "peak_rate": 0.30,
            "off_peak_rate": 0.10,
            "off_peak_start": "garbage",
            "off_peak_end": "also-garbage"
        }"#;
        let cfg: TariffConfig = serde_json::from_str(legacy).unwrap();
        // Should fall back to a flat day at peak rate.
        assert_eq!(cfg.slots.len(), 1);
        assert_eq!(cfg.rate_for_minutes(0), Some(0.30));
        assert_eq!(cfg.rate_for_minutes(720), Some(0.30));
    }

    /// Empty slots array in new shape → default config (safe fallback).
    #[test]
    fn tariff_empty_slots_uses_default() {
        let json = r#"{ "slots": [] }"#;
        let cfg: TariffConfig = serde_json::from_str(json).unwrap();
        assert!(!cfg.slots.is_empty(), "empty slots must fall back to default");
        assert_eq!(cfg.slots[0].start, "00:00");
    }

    /// The default config (same rates as old default) must produce
    /// bit-identical cost outcomes to the old peak/off-peak model at every
    /// minute. This is the golden regression test: if rate_for_minutes
    /// diverges from the old is_off_peak logic anywhere, cost graphs change.
    #[test]
    fn tariff_default_matches_legacy_at_every_minute() {
        let cfg = TariffConfig::default();
        let old_peak = 0.285_f64;
        let old_off_peak = 0.09_f64;
        // Old off-peak window: 00:30–05:30 (minutes 30–330).
        let old_off_peak_start: u16 = 30;
        let old_off_peak_end: u16 = 330;

        for minute in 0..1440u16 {
            let old_rate = if minute >= old_off_peak_start && minute < old_off_peak_end {
                old_off_peak
            } else {
                old_peak
            };
            let new_rate = cfg.rate_for_minutes(minute).unwrap();
            assert!(
                (old_rate - new_rate).abs() < 1e-12,
                "minute {minute}: old={old_rate} new={new_rate} mismatch"
            );
        }
    }
}
