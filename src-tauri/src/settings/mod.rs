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
/// The day is tiled by an ordered list of these slots covering `[00:00, 23:59]`.
/// The `end` field may be `"23:59"` (internally minute 1439) for the final
/// slot — that slot is **inclusive** on both ends, so `23:59` covers minute
/// 1439 (the last minute of the day). All earlier slots use the half-open
/// convention `[start, end)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TariffSlot {
    /// Start time in "HH:MM" format (24h).
    pub start: String,
    /// End time in "HH:MM" format (24h). Use "23:59" for the final slot
    /// (inclusive — covers minute 1439). Intermediate slots are half-open,
    /// so e.g. "05:30" means up-to-but-not-including 05:30.
    pub end: String,
    /// Electricity rate in £/kWh for this window.
    pub rate: f64,
}

/// Tariff configuration as a list of time windows, each with its own price.
///
/// This generalizes the previous fixed peak/off-peak model to support any
/// time-of-use tariff (Octopus Flux, Cosy, Agile, flat, etc.). The day is
/// tiled by an ascending list of [`TariffSlot`]s. All slots except the last
/// are half-open `[start, end)`; the final slot is closed `[start, end]`,
/// using `"23:59"` (minute 1439) so it covers the very last minute of the
/// day. See [`rate_for_minutes`](Self::rate_for_minutes) for the lookup
/// logic.
///
/// **Backward compatibility:** a custom `Deserialize` accepts both the old
/// shape (`peak_rate`, `off_peak_rate`, `off_peak_start`, `off_peak_end`)
/// and the new shape (`slots`). Old files are migrated to slots that
/// reproduce the exact previous behaviour, including midnight-crossing
/// windows. After the first save, only the new `slots` shape is written.
#[derive(Debug, Clone, Serialize)]
pub struct TariffConfig {
    /// Time windows in ascending order by start, tiling `[00:00, 23:59]`.
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
                // Final slot: end is inclusive so "23:59" covers minute 1439
                // (the last minute of the day). Half-hour granularity means
                // there's no cleaner real-time representation.
                TariffSlot {
                    start: "05:30".to_string(),
                    end: "23:59".to_string(),
                    rate: 0.285,
                },
            ],
        }
    }
}

/// Parse a `"HH:MM"` time string into minutes since midnight.
/// Returns `None` if the string is malformed. `"23:59"` → 1439 (no
/// `"24:00"` — the final slot uses `"23:59"` and is treated as inclusive).
pub(crate) fn parse_hhmm_to_minutes(s: &str) -> Option<u16> {
    let mut it = s.split(':');
    let h: u16 = it.next()?.trim().parse().ok()?;
    let m: u16 = it.next()?.trim().parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(h * 60 + m)
}

/// Look up the rate for `minutes` in slots pre-parsed by
/// [`TariffConfig::parsed_slots`]. Same rule as [`TariffConfig::rate_for_minutes`]:
/// intermediate slots are half-open `[start, end)`, the final slot is closed
/// `[start, end]` (so "23:59" covers minute 1439), and a tail gap falls back to
/// the last slot's rate. Returns `None` only for empty input.
pub fn rate_for_parsed_minutes(
    parsed: &[(Option<u16>, Option<u16>, f64)],
    minutes: u16,
) -> Option<f64> {
    if parsed.is_empty() {
        return None;
    }
    let last_idx = parsed.len() - 1;
    for (i, &(start, end, rate)) in parsed.iter().enumerate() {
        let (Some(start), Some(end)) = (start, end) else {
            continue;
        };
        if i == last_idx {
            if end >= start && minutes >= start && minutes <= end {
                return Some(rate);
            }
        } else if end > start && minutes >= start && minutes < end {
            return Some(rate);
        }
    }
    parsed.last().map(|&(_, _, rate)| rate)
}

impl TariffConfig {
    /// A flat-rate config: a single slot covering the whole day at `rate`.
    ///
    /// Used as the fallback when only the legacy scalar tariff is set (no
    /// structured `slots`), so cost calculation always has a usable config.
    pub fn flat(rate: f64) -> Self {
        TariffConfig {
            slots: vec![TariffSlot {
                start: "00:00".to_string(),
                end: "23:59".to_string(),
                rate,
            }],
        }
    }

    /// Pre-parse the slots into `(start_min, end_min, rate)` triples once
    /// (minutes since midnight; `None` for an unparseable bound), so a hot
    /// loop can price many timestamps without re-parsing the `HH:MM` strings
    /// on every reading. Pair with [`rate_for_parsed_minutes`].
    pub fn parsed_slots(&self) -> Vec<(Option<u16>, Option<u16>, f64)> {
        self.slots
            .iter()
            .map(|s| {
                (
                    parse_hhmm_to_minutes(&s.start),
                    parse_hhmm_to_minutes(&s.end),
                    s.rate,
                )
            })
            .collect()
    }

    /// Look up the rate for a given minute of the day `[0, 1440)`.
    ///
    /// Lookup = first slot whose `[start, end)` contains the minute. The
    /// **final slot** is closed `[start, end]` so its `end = "23:59"` covers
    /// minute 1439 (the last minute of the day). If no slot covers the
    /// minute (tail gap from a hand-edited/malformed file), fall back to
    /// the **last slot's rate** to defend against gaps at the end of the
    /// day. Returns `None` only when there are zero slots.
    pub fn rate_for_minutes(&self, minutes: u16) -> Option<f64> {
        rate_for_parsed_minutes(&self.parsed_slots(), minutes)
    }

    /// Validate the config for well-formedness. Returns `Ok(())` if every
    /// slot is parseable, rates are finite non-negative numbers, and the
    /// slots tile the full day contiguously (first starts at 00:00, last
    /// ends at 23:59, no gaps or overlaps). Returns `Err(msg)` with a
    /// human-readable description of the first failure otherwise.
    ///
    /// This is the server-side counterpart to the frontend's
    /// `validateTariffConfig` — both must stay in sync. Mirroring the rules
    /// here means a hand-edited `settings.json` or a direct API call
    /// cannot poison the rate lookup.
    pub fn validate(&self) -> Result<(), String> {
        if self.slots.is_empty() {
            return Err("at least one tariff window is required".into());
        }

        // Parse + per-slot checks.
        let parsed: Vec<(u16, u16)> = self
            .slots
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let start = parse_hhmm_to_minutes(&s.start);
                let end = parse_hhmm_to_minutes(&s.end);
                match (start, end) {
                    (Some(s), Some(e)) if s <= e => Ok((s, e)),
                    (Some(_), Some(_)) => Err(format!(
                        "slot {}: end ({}) must be at or after start ({})",
                        i + 1,
                        s.end,
                        s.start
                    )),
                    (None, _) => Err(format!(
                        "slot {}: start (\"{}\") is not a valid HH:MM time",
                        i + 1,
                        s.start
                    )),
                    (_, None) => Err(format!(
                        "slot {}: end (\"{}\") is not a valid HH:MM time",
                        i + 1,
                        s.end
                    )),
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        for (i, slot) in self.slots.iter().enumerate() {
            if !slot.rate.is_finite() {
                return Err(format!("slot {}: rate must be a finite number", i + 1));
            }
            if slot.rate < 0.0 {
                return Err(format!("slot {}: rate cannot be negative", i + 1));
            }
        }

        // First slot must start at 00:00.
        if parsed[0].0 != 0 {
            return Err(format!(
                "slot 1: must start at 00:00 (currently {})",
                self.slots[0].start
            ));
        }
        // Last slot must end at 23:59 (inclusive end-of-day marker).
        const LAST_MIN: u16 = 23 * 60 + 59;
        let last = parsed.last().unwrap();
        if last.1 != LAST_MIN {
            return Err(format!(
                "slot {}: must end at 23:59 (currently {})",
                self.slots.len(),
                self.slots.last().unwrap().end
            ));
        }
        // Contiguous tiling: each slot's start == previous slot's end.
        for i in 1..parsed.len() {
            let prev_end = parsed[i - 1].1;
            let curr_start = parsed[i].0;
            if curr_start > prev_end {
                return Err(format!(
                    "gap between slot {} (ends {}) and slot {} (starts {}): windows must cover the full 24 hours contiguously",
                    i,
                    self.slots[i - 1].end,
                    i + 1,
                    self.slots[i].start
                ));
            }
            if curr_start < prev_end {
                return Err(format!(
                    "slot {} (starts {}) overlaps slot {} (ends {})",
                    i + 1,
                    self.slots[i].start,
                    i,
                    self.slots[i - 1].end
                ));
            }
        }

        Ok(())
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
                let slots =
                    legacy_to_slots(peak_rate, off_peak_rate, &off_peak_start, &off_peak_end);
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
///   `[00:00→start]=peak`, `[start→end]=off_peak`, `[end→23:59]=peak`
/// - `start > end` (crosses midnight):
///   `[00:00→end]=off_peak`, `[end→start]=peak`, `[start→23:59]=off_peak`
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
                off_peak_start,
                off_peak_end,
                "Legacy tariff has unparseable off-peak times, using flat peak rate"
            );
            return vec![TariffSlot {
                start: "00:00".to_string(),
                end: "23:59".to_string(),
                rate: peak_rate,
            }];
        }
    };

    // Build raw segments as (start_minute, end_minute, rate).
    // 23:59 is the latest representable end-of-day clock time; the final
    // slot is treated as inclusive in `rate_for_minutes`.
    const LAST_MIN: u16 = 23 * 60 + 59; // 1439
    let raw: Vec<(u16, u16, f64)> = if op_start <= op_end {
        // Normal: off-peak is inside the day.
        vec![
            (0, op_start, peak_rate),
            (op_start, op_end, off_peak_rate),
            (op_end, LAST_MIN, peak_rate),
        ]
    } else {
        // Crosses midnight: off-peak wraps around.
        vec![
            (0, op_end, off_peak_rate),
            (op_end, op_start, peak_rate),
            (op_start, LAST_MIN, off_peak_rate),
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
    // 00:00→start + end→23:59 when the off-peak window is in the middle).
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
            end: "23:59".to_string(),
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
    // Ensure the last slot ends at 23:59 (inclusive — covers minute 1439).
    if merged.last().unwrap().end != "23:59" {
        merged.last_mut().unwrap().end = "23:59".to_string();
    }

    merged
}

/// Convert minutes-since-midnight to "HH:MM" string. Minutes are clamped
/// to `[0, 1439]` so `"23:59"` (1439) is the maximum representable clock
/// time — the final tariff slot's inclusive end.
fn minutes_to_hhmm(minutes: u16) -> String {
    let clamped = minutes.min(23 * 60 + 59);
    let h = clamped / 60;
    let m = clamped % 60;
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

/// Local weather integration settings.
///
/// Drives the optional Phase-2 weather fetcher (Open-Meteo forecast for
/// current conditions, Open-Meteo archive for historical backfill). Every
/// field carries `#[serde(default)]` so a `settings.json` written before
/// this feature shipped loads unchanged — see the `legacy_*` tests in
/// this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeatherConfig {
    /// Master toggle. When false the fetcher skips every cycle and the
    /// History tab shows the standard "No data available" empty state.
    #[serde(default)]
    pub enabled: bool,
    /// User-entered postcode (display only; we re-resolve to lat/lon on
    /// enable and store the coords). Persisted so the Settings UI can
    /// show what the user originally typed.
    #[serde(default)]
    pub postcode: String,
    /// Resolved latitude. Populated by the backend after a successful
    /// postcode lookup, or by manual coordinate entry.
    #[serde(default)]
    pub latitude: Option<f64>,
    /// Resolved longitude. See `latitude`.
    #[serde(default)]
    pub longitude: Option<f64>,
    /// Last calendar date (UTC) that the backfill loop has fully
    /// populated. The loop advances this by one day per successful
    /// monthly chunk, so a crash mid-backfill resumes from the last
    /// completed month on next launch.
    #[serde(default)]
    pub last_backfill_completed: Option<chrono::NaiveDate>,
    /// URL of the Open-Meteo *forecast* endpoint. Defaults to the
    /// free non-commercial API; a self-hosted instance can be configured
    /// here. The archive endpoint is derived by swapping `api.` for
    /// `archive-api.`.
    #[serde(default = "default_open_meteo_base_url")]
    pub open_meteo_base_url: String,
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            postcode: String::new(),
            latitude: None,
            longitude: None,
            last_backfill_completed: None,
            open_meteo_base_url: default_open_meteo_base_url(),
        }
    }
}

pub(crate) fn default_open_meteo_base_url() -> String {
    "https://api.open-meteo.com".to_string()
}

/// A snapshot of a single discharge schedule slot, persisted to settings
/// so the user's pre-Eco schedule can be restored when they switch back
/// to Timed mode.
///
/// Mirrors [`crate::inverter::model::ScheduleSlot`] field-for-field but
/// lives in `settings/` to avoid a circular dependency (the settings
/// module is loaded by the inverter module, not the other way around).
/// Keep the two structs in sync — adding a field to `ScheduleSlot` should
/// be reflected here. See issue #137.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DischargeSlotBackup {
    /// Whether the slot is active.
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
    pub target_soc: u8,
}

impl From<&crate::inverter::model::ScheduleSlot> for DischargeSlotBackup {
    fn from(slot: &crate::inverter::model::ScheduleSlot) -> Self {
        Self {
            enabled: slot.enabled,
            start_hour: slot.start_hour,
            start_minute: slot.start_minute,
            end_hour: slot.end_hour,
            end_minute: slot.end_minute,
            target_soc: slot.target_soc,
        }
    }
}

/// Which sides of the inverter the Agile Octopus mode drives.
///
/// Replaces the old boolean `agile_enabled` flag with three explicit
/// modes plus an "off" sentinel:
///
/// - `Off` — Agile is disabled; the inverter obeys the user's manual
///   charge/discharge schedule (the "Standard" mode on the front-end).
/// - `Full` — prices drive both charging (cheap windows) and discharging
///   (expensive windows). Same as the pre-existing `agile_enabled = true`
///   behaviour.
/// - `ChargeOnly` — prices drive charging only; the user's discharge
///   schedule keeps full control of the discharge side. The discharge
///   schedule section remains visible on the Control page but is
///   rendered greyed out with a "controlled by manual timer" label.
/// - `DischargeOnly` — symmetric: prices drive discharging only; the
///   user's charge schedule keeps full control of the charge side.
///
/// The variant serialises as `"off" | "full" | "charge_only" |
/// "discharge_only"` (snake_case). Default is `Off`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgileScope {
    #[default]
    Off,
    Full,
    ChargeOnly,
    DischargeOnly,
}

impl AgileScope {
    /// True when this scope is enabled in any direction.
    pub fn is_enabled(self) -> bool {
        !matches!(self, AgileScope::Off)
    }

    /// True when this scope drives the charge side (cheap-window force
    /// charge). True for `Full` and `ChargeOnly`.
    pub fn owns_charge(self) -> bool {
        matches!(self, AgileScope::Full | AgileScope::ChargeOnly)
    }

    /// True when this scope drives the discharge side (expensive-window
    /// export). True for `Full` and `DischargeOnly`.
    pub fn owns_discharge(self) -> bool {
        matches!(self, AgileScope::Full | AgileScope::DischargeOnly)
    }

    /// Back-compat shim for the legacy `agile_enabled: bool` wire field.
    /// `enabled = scope != Off` so existing frontends that read the
    /// boolean keep working unchanged.
    pub fn as_enabled(self) -> bool {
        self.is_enabled()
    }
}

/// An external solar array tracked via a GivEnergy CT clamp (issue #110).
///
/// For AC-coupled systems whose panels feed a separate third-party PV
/// inverter (not a GivEnergy hybrid), the inverter's DC-input registers
/// (IR 18/20) read zero — there is no per-string solar data on the
/// GivEnergy box itself. The array's production is instead measured by a
/// CT clamp on the solar inverter's AC output, which the GivEnergy dongle
/// already polls as an external meter at device address 0x01–0x08. The
/// user labels that meter as a solar array here and enters the array's
/// rated peak capacity (kWp) so the UI can show output as a percentage of
/// its maximum, alongside the kW value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SolarArrayConfig {
    /// CT meter device address this array is wired to (1–8). Addresses
    /// outside this range are ignored at compute time — 0x00 is the
    /// synthetic built-in grid CT, not a real clamp.
    pub meter_address: u8,
    /// Display name (e.g. "East roof", "Garage"). Empty → the UI falls
    /// back to a default label derived from the meter address.
    #[serde(default)]
    pub name: String,
    /// Rated peak capacity in kW (kWp). 0 hides the % display for this
    /// array (power is still shown).
    #[serde(default)]
    pub rated_kw: f64,
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
    /// Daily fixed Standing Charge for the import direction, in pence/day.
    /// UK-style tariffs (Octopus Flux, etc.) charge a flat daily fee on top
    /// of the per-kWh rate; the History cost series adds
    /// `standing_charge × days_in_window` once per query window so the
    /// cumulative cost graph includes the fixed component. Defaults to 0
    /// (no Standing Charge) for back-compat with settings.json files written
    /// before this field was added — see issue #131.
    #[serde(default)]
    pub import_standing_charge_p_per_day: f64,

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
    /// Agile Octopus scope (Off / Full / ChargeOnly / DischargeOnly).
    ///
    /// Replaces the boolean `agile_enabled` for new code paths. The
    /// legacy `agile_enabled` field is preserved above for backwards
    /// compatibility with settings.json files written by v0.47.0 and
    /// earlier — see `agile_scope_for_settings` for the migration rule
    /// (legacy `agile_enabled = true` maps to `Full`, false maps to
    /// `Off`).
    #[serde(default)]
    pub agile_scope: AgileScope,
    /// Agile Octopus region code (A-P).
    #[serde(default = "default_agile_region")]
    pub agile_region: String,
    /// Agile Octopus charge threshold in p/kWh.
    #[serde(default = "default_agile_charge_threshold")]
    pub agile_charge_threshold: f64,
    /// Agile Octopus discharge threshold in p/kWh.
    #[serde(default = "default_agile_discharge_threshold")]
    pub agile_discharge_threshold: f64,
    /// Override base URL for the Octopus Agile pricing API. Defaults to
    /// the real Octopus endpoint. Self-hosters can point at a mirror;
    /// tests point at a local mock server that returns canned prices
    /// so the slot-based state machine can be exercised end-to-end
    /// without network access. Empty string means "use the default".
    #[serde(default)]
    pub agile_api_base_url: String,

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
    /// Persisted battery SOC reserve saved before the load limiter paused
    /// discharge. `Some` means the limiter was active when last saved.
    /// Restored on restart so the user's custom reserve isn't lost.
    #[serde(default)]
    pub load_limiter_saved_reserve: Option<u16>,

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
    #[serde(default = "default_disable_auto_discovery")]
    pub disable_auto_discovery: bool,

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

    /// Local weather integration. Drives the Phase-2 Open-Meteo fetcher.
    /// See [`WeatherConfig`].
    #[serde(default)]
    pub weather_config: WeatherConfig,

    // -- Octopus Energy customer consumption (issue #212) --
    /// Opt-in gate for authenticated Octopus account consumption sync.
    #[serde(default)]
    pub octopus_enabled: bool,
    /// Octopus customer API key. Never returned by the settings API or logged.
    #[serde(default)]
    pub octopus_api_key: String,
    /// Octopus account number, e.g. A-1234ABCD.
    #[serde(default)]
    pub octopus_account_number: String,
    /// Override for tests/self-hosted mirrors. Empty uses the official API.
    #[serde(default)]
    pub octopus_api_base_url: String,

    /// Whether the user opted in to launching the app automatically when
    /// they log in (Windows: HKCU\…\Run, macOS: LaunchAgent,
    /// Linux: ~/.config/autostart/*.desktop). Persisted so the in-app
    /// toggle can show the right state on next launch, and so the startup
    /// self-heal path can re-enable the OS-level autostart entry if the
    /// platform silently removed it (see plugins-workspace#771). See #117.
    #[serde(default)]
    pub autostart_enabled: bool,

    // -- Read-only API (external access) --
    /// API key for the read-only external API server.
    /// When non-empty, a second HTTP server is started on `api_port` that
    /// serves only `GET /api/snapshot` with Bearer-token authentication.
    /// The main server on `http_port` is unaffected (full access, no auth).
    #[serde(default)]
    pub api_key: String,
    /// Port for the read-only external API server (default 7338).
    /// Only started when `api_key` is also non-empty. Set to 0 to disable.
    #[serde(default = "default_api_port")]
    pub api_port: u16,

    /// Persisted copy of the user's discharge schedule captured on the way
    /// into Eco / Pause / Export Paused. The backend needs to zero the
    /// slot registers on those mode switches (the Gen3 firmware re-asserts
    /// `enable_discharge` whenever any slot register is non-zero, which
    /// would otherwise prevent Eco from "sticking"), but doing so loses
    /// the user's configured schedule. We snapshot it here first and
    /// restore it atomically (before `HR_ENABLE_DISCHARGE=1`) when the
    /// user switches back to Timed Demand / Timed Export without
    /// explicitly posting a new schedule. See issue #137.
    #[serde(default)]
    pub discharge_slots_backup: Option<Vec<DischargeSlotBackup>>,

    // -- Solar array capacities (issue #110) --
    /// Rated peak capacity (kWp) of the PV1 DC string on a hybrid /
    /// DC-coupled inverter. 0 (default) = not configured, in which case
    /// PV1 is omitted from the per-array "% of max" display and behaves
    /// exactly as before. Hybrid users with multiple aspects set this
    /// (and `pv2_rated_kw`) to see each string's output as a percentage.
    #[serde(default)]
    pub pv1_rated_kw: f64,
    /// Rated peak capacity (kWp) of the PV2 DC string. See `pv1_rated_kw`.
    #[serde(default)]
    pub pv2_rated_kw: f64,
    /// External solar arrays measured by GivEnergy CT clamps (AC-coupled /
    /// separate inverters). Each entry labels an existing external meter
    /// (0x01–0x08) as a solar array with a rated kWp. See
    /// [`SolarArrayConfig`]. Empty by default.
    #[serde(default)]
    pub solar_arrays: Vec<SolarArrayConfig>,
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

fn default_api_port() -> u16 {
    7338
}

fn default_disable_auto_discovery() -> bool {
    true
}

fn default_agile_charge_threshold() -> f64 {
    10.0
}

fn default_agile_discharge_threshold() -> f64 {
    30.0
}

/// Resolve the effective Agile scope for a loaded `Settings`, applying
/// the v0.47 → v0.48+ migration.
///
/// Settings written before the `agile_scope` field existed store the
/// mode intent as a boolean `agile_enabled`. We default `agile_scope`
/// to `Off` via `#[serde(default)]`, which means a legacy
/// `agile_enabled = true` file would silently turn Agile off. This
/// helper consults both fields and returns the right scope:
///
/// - New settings files (post-migration) carry an explicit
///   `agile_scope` so the helper returns it verbatim.
/// - Legacy settings files have `agile_enabled = true` and no
///   `agile_scope`. The helper maps this to `Full`, preserving the
///   user's original intent.
///
/// Returns `AgileScope::Off` only when both fields indicate off, so a
/// fresh-defaults settings file (both off) stays off.
pub fn agile_scope_for_settings(settings: &Settings) -> AgileScope {
    // New-format files: explicit scope wins.
    if settings.agile_scope != AgileScope::Off {
        return settings.agile_scope;
    }
    // Legacy: boolean was the source of truth. Migrate on read.
    if settings.agile_enabled {
        return AgileScope::Full;
    }
    AgileScope::Off
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
    /// Inverter temperature alert minimum (°C). Defaults to 8°C.
    #[serde(default = "default_inverter_temp_min")]
    pub inverter_temp_min: f32,
    /// Inverter temperature alert maximum (°C). Defaults to 60°C.
    #[serde(default = "default_inverter_temp_max")]
    pub inverter_temp_max: f32,
    /// Battery SOC alert minimum (%). 0 = disabled.
    pub soc_min: u8,
    /// Battery SOC alert maximum (%). 100 = disabled.
    pub soc_max: u8,
    /// Alert on grid loss.
    pub grid_offline_enabled: bool,
    /// Alert when the inverter reports a fault/trip state.
    #[serde(default)]
    pub inverter_trip_enabled: bool,
    /// Alert when the poll loop loses contact with the inverter. A single
    /// notification fires on disconnect, and a second on reconnect.
    #[serde(default)]
    pub connection_lost_enabled: bool,
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

fn default_inverter_temp_min() -> f32 {
    8.0
}

fn default_inverter_temp_max() -> f32 {
    60.0
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
            inverter_temp_min: default_inverter_temp_min(),
            inverter_temp_max: default_inverter_temp_max(),
            soc_min: 4,
            soc_max: 100,
            grid_offline_enabled: false,
            inverter_trip_enabled: false,
            connection_lost_enabled: false,
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
            // Issue #131: zero by default — older installs have no standing
            // charge and we don't want to silently start adding one to the
            // History cost graph for users who haven't configured it.
            import_standing_charge_p_per_day: 0.0,
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
            load_limiter_saved_reserve: None,
            import_tariff_config: None,
            export_tariff_config: None,
            agile_enabled: false,
            agile_scope: AgileScope::default(),
            agile_region: default_agile_region(),
            agile_charge_threshold: default_agile_charge_threshold(),
            agile_discharge_threshold: default_agile_discharge_threshold(),
            agile_api_base_url: String::new(),
            cosy_enabled: false,
            cosy_slots: (0..3).map(|_| CosySlot::default()).collect(),
            cosy_active_persisted: false,
            agile_state_persisted: String::new(),
            hidden_panels: Vec::new(),
            alerts_config: AlertsConfig::default(),
            weather_config: WeatherConfig::default(),
            octopus_enabled: false,
            octopus_api_key: String::new(),
            octopus_account_number: String::new(),
            octopus_api_base_url: String::new(),
            disable_auto_discovery: true,
            autostart_enabled: false,
            api_key: String::new(),
            api_port: 7338,
            discharge_slots_backup: None,
            // Issue #110: solar array capacities default to unset so a
            // fresh install (and every existing install on upgrade) sees
            // no behaviour change until the user opts in via Settings.
            pv1_rated_kw: 0.0,
            pv2_rated_kw: 0.0,
            solar_arrays: Vec::new(),
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
        // Autostart must default to OFF — we never silently register the
        // app to launch on login, the user has to opt in via Settings.
        // See issue #117.
        assert!(!s.autostart_enabled);
        // Read-only API key must be empty by default; port must default to 7338.
        assert_eq!(s.api_key, "");
        assert_eq!(s.api_port, 7338);
        // Discharge-slot backup (issue #137) defaults to None — no schedule
        // to restore until the user enters Eco / Pause / Export Paused at
        // least once with a configured discharge schedule.
        assert_eq!(s.discharge_slots_backup, None);
        // Issue #110: solar array capacities default to unset so an existing
        // install sees no behaviour change until the user opts in.
        assert_eq!(s.pv1_rated_kw, 0.0);
        assert_eq!(s.pv2_rated_kw, 0.0);
        assert!(s.solar_arrays.is_empty());
        // Issue #212 is strictly opt-in and starts with no supplier credentials.
        assert!(!s.octopus_enabled);
        assert!(s.octopus_api_key.is_empty());
        assert!(s.octopus_account_number.is_empty());
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
            import_standing_charge_p_per_day: 54.86,
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
            load_limiter_saved_reserve: None,
            evc_host: String::new(),
            evc_port: default_evc_port(),
            import_tariff_config: None,
            export_tariff_config: None,
            agile_enabled: true,
            agile_scope: AgileScope::Full,
            agile_region: "B".to_string(),
            agile_charge_threshold: 12.5,
            agile_discharge_threshold: 35.0,
            agile_api_base_url: String::new(),
            cosy_enabled: false,
            cosy_slots: vec![],
            cosy_active_persisted: false,
            agile_state_persisted: "discharging".to_string(),
            hidden_panels: Vec::new(),
            alerts_config: AlertsConfig::default(),
            weather_config: WeatherConfig {
                enabled: true,
                postcode: "SW1A 1AA".to_string(),
                latitude: Some(51.501009),
                longitude: Some(-0.141588),
                last_backfill_completed: Some(
                    chrono::NaiveDate::from_ymd_opt(2025, 1, 15).unwrap(),
                ),
                open_meteo_base_url: "https://api.open-meteo.com".to_string(),
            },
            octopus_enabled: true,
            octopus_api_key: "sk_test".to_string(),
            octopus_account_number: "A-1234ABCD".to_string(),
            octopus_api_base_url: String::new(),
            disable_auto_discovery: true,
            autostart_enabled: true,
            api_key: String::new(),
            api_port: 0,
            discharge_slots_backup: Some(vec![
                DischargeSlotBackup {
                    enabled: true,
                    start_hour: 16,
                    start_minute: 0,
                    end_hour: 19,
                    end_minute: 30,
                    target_soc: 4,
                },
                DischargeSlotBackup {
                    enabled: true,
                    start_hour: 21,
                    start_minute: 0,
                    end_hour: 23,
                    end_minute: 0,
                    target_soc: 4,
                },
            ]),
            // Issue #110: solar array capacities must round-trip exactly.
            pv1_rated_kw: 6.0,
            pv2_rated_kw: 4.2,
            solar_arrays: vec![
                SolarArrayConfig {
                    meter_address: 1,
                    name: "East roof".to_string(),
                    rated_kw: 6.0,
                },
                SolarArrayConfig {
                    meter_address: 2,
                    name: String::new(),
                    rated_kw: 4.2,
                },
            ],
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
        assert!(decoded.load_limiter_enabled);
        assert_eq!(decoded.load_limiter_threshold_w, 5000);
        assert_eq!(decoded.load_limiter_trigger_delay_minutes, 10);
        assert_eq!(decoded.load_limiter_start_hour, 16);
        assert_eq!(decoded.load_limiter_end_hour, 20);
        assert!(decoded.autostart_enabled);
        // Weather config roundtrips with all fields populated.
        assert!(decoded.weather_config.enabled);
        assert_eq!(decoded.weather_config.postcode, "SW1A 1AA");
        assert_eq!(decoded.weather_config.latitude, Some(51.501009));
        assert_eq!(decoded.weather_config.longitude, Some(-0.141588));
        assert_eq!(
            decoded.weather_config.last_backfill_completed,
            Some(chrono::NaiveDate::from_ymd_opt(2025, 1, 15).unwrap())
        );
        assert_eq!(
            decoded.weather_config.open_meteo_base_url,
            "https://api.open-meteo.com"
        );
        // Discharge-slot backup roundtrips with all fields populated (issue #137).
        let backup = decoded
            .discharge_slots_backup
            .as_ref()
            .expect("backup must round-trip through JSON");
        assert_eq!(backup.len(), 2);
        assert!(backup[0].enabled);
        assert_eq!(backup[0].start_hour, 16);
        assert_eq!(backup[0].start_minute, 0);
        assert_eq!(backup[0].end_hour, 19);
        assert_eq!(backup[0].end_minute, 30);
        assert_eq!(backup[0].target_soc, 4);
        assert_eq!(backup[1].start_hour, 21);
        assert_eq!(backup[1].end_hour, 23);
        // Issue #110: solar array capacities round-trip with names + ratings.
        assert_eq!(decoded.pv1_rated_kw, 6.0);
        assert_eq!(decoded.pv2_rated_kw, 4.2);
        assert_eq!(decoded.solar_arrays.len(), 2);
        assert_eq!(decoded.solar_arrays[0].meter_address, 1);
        assert_eq!(decoded.solar_arrays[0].name, "East roof");
        assert_eq!(decoded.solar_arrays[0].rated_kw, 6.0);
        assert_eq!(decoded.solar_arrays[1].meter_address, 2);
        assert!(decoded.solar_arrays[1].name.is_empty());
        assert_eq!(decoded.solar_arrays[1].rated_kw, 4.2);
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
        assert_eq!(decoded.inverter_temp_min, 8.0);
        assert_eq!(decoded.inverter_temp_max, 60.0);
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
        assert_eq!(decoded.inverter_temp_min, 8.0);
        assert_eq!(decoded.inverter_temp_max, 60.0);
    }

    /// `settings.json` written before the weather feature shipped (no
    /// `weather_config` key at all) must still load. The struct's
    /// `#[serde(default)]` on every field produces `WeatherConfig::default()`
    /// — enabled is false, lat/lon are None, the URL stays at the
    /// non-commercial endpoint.
    #[test]
    fn legacy_settings_without_weather_loads() {
        // Minimal Settings JSON missing every optional field. Real legacy
        // files have many more fields; serde ignores unknowns not present
        // in the struct, so the absence of `weather_config` is the only
        // thing we exercise here.
        let legacy = r#"{
            "host": "192.168.1.50",
            "port": 8899,
            "serial": "",
            "poll_interval": 60,
            "auto_connect": true,
            "import_tariff": 0.285,
            "export_tariff": 0.15,
            "hidden_panels": [],
            "evc_host": "",
            "disable_auto_discovery": true
        }"#;
        let decoded: Settings = serde_json::from_str(legacy).unwrap();
        // Defaults populated via #[serde(default)] on WeatherConfig fields.
        assert!(!decoded.weather_config.enabled);
        assert_eq!(decoded.weather_config.postcode, "");
        assert_eq!(decoded.weather_config.latitude, None);
        assert_eq!(decoded.weather_config.longitude, None);
        assert_eq!(decoded.weather_config.last_backfill_completed, None);
        assert_eq!(
            decoded.weather_config.open_meteo_base_url,
            "https://api.open-meteo.com"
        );
        // Legacy files (pre-#137) carry no `discharge_slots_backup` key.
        // The `#[serde(default)]` on the new field produces `None` so the
        // upgrade path is silent — see the dedicated
        // `legacy_settings_without_discharge_slots_backup_loads` test.
        assert_eq!(decoded.discharge_slots_backup, None);
    }

    /// `settings.json` written before auto-discovery became opt-in must
    /// load with auto-discovery disabled. This prevents upgraded installs
    /// with older settings files from unexpectedly scanning the LAN and
    /// switching dongle IPs after repeated connection failures.
    #[test]
    fn legacy_settings_without_auto_discovery_flag_defaults_disabled() {
        let legacy = r#"{
            "host": "192.168.1.50",
            "port": 8899,
            "serial": "",
            "poll_interval": 60,
            "auto_connect": true,
            "import_tariff": 0.285,
            "export_tariff": 0.15,
            "hidden_panels": [],
            "evc_host": ""
        }"#;
        let decoded: Settings = serde_json::from_str(legacy).unwrap();
        assert!(
            decoded.disable_auto_discovery,
            "missing disable_auto_discovery must default to true (auto-discovery off)"
        );
    }

    /// `settings.json` written before the discharge-slot backup feature
    /// shipped (no `discharge_slots_backup` key at all) must still load.
    /// The `#[serde(default)]` on the new field produces `None` so an
    /// upgrade user gets the "no schedule to restore" state, not a panic.
    /// See issue #137.
    #[test]
    fn legacy_settings_without_discharge_slots_backup_loads() {
        let legacy = r#"{
            "host": "192.168.1.50",
            "port": 8899,
            "serial": "",
            "poll_interval": 60,
            "auto_connect": true,
            "import_tariff": 0.285,
            "export_tariff": 0.15,
            "hidden_panels": [],
            "evc_host": "",
            "disable_auto_discovery": true
        }"#;
        let decoded: Settings = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            decoded.discharge_slots_backup, None,
            "missing field must default to None, not fail to parse"
        );
    }

    /// `DischargeSlotBackup` survives a full JSON round-trip independently
    /// of the surrounding `Settings` struct. Pins the on-disk shape so a
    /// later rename or field reorder can't break a stored backup file.
    #[test]
    fn discharge_slot_backup_roundtrip() {
        let original = DischargeSlotBackup {
            enabled: true,
            start_hour: 16,
            start_minute: 30,
            end_hour: 19,
            end_minute: 45,
            target_soc: 80,
        };
        let json = serde_json::to_string(&original).unwrap();
        let decoded: DischargeSlotBackup = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, original);
    }

    /// `SolarArrayConfig` survives a full JSON round-trip independently of
    /// the surrounding `Settings` struct. Pins the on-disk shape so a later
    /// rename or field reorder can't break a stored array entry (issue #110).
    #[test]
    fn solar_array_config_roundtrip() {
        let original = SolarArrayConfig {
            meter_address: 3,
            name: "Garage roof".to_string(),
            rated_kw: 3.68,
        };
        let json = serde_json::to_string(&original).unwrap();
        let decoded: SolarArrayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, original);
    }

    /// A `settings.json` written before issue #110 shipped (no solar array
    /// fields) must load without error, defaulting the new fields to unset
    /// values. Mirrors the legacy forward-compat tests above.
    #[test]
    fn legacy_settings_without_solar_array_fields_loads() {
        let legacy = r#"{
            "host": "192.168.1.50",
            "port": 8899,
            "serial": "",
            "poll_interval": 60,
            "auto_connect": true,
            "import_tariff": 0.285,
            "export_tariff": 0.15,
            "hidden_panels": [],
            "evc_host": "",
            "disable_auto_discovery": true
        }"#;
        let decoded: Settings = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            decoded.pv1_rated_kw, 0.0,
            "missing field must default to 0.0, not fail to parse"
        );
        assert_eq!(decoded.pv2_rated_kw, 0.0);
        assert!(
            decoded.solar_arrays.is_empty(),
            "missing field must default to empty, not fail to parse"
        );
    }

    /// WeatherConfig JSON missing the optional `open_meteo_base_url` field
    /// defaults to the non-commercial endpoint. Mirrors the same
    /// forward-compat pattern as the alert-config test above.
    #[test]
    fn weather_config_without_base_url_defaults() {
        let legacy = r#"{
            "enabled": true,
            "postcode": "SW1A 1AA",
            "latitude": 51.501,
            "longitude": -0.141
        }"#;
        let decoded: WeatherConfig = serde_json::from_str(legacy).unwrap();
        assert_eq!(decoded.open_meteo_base_url, "https://api.open-meteo.com");
        assert!(decoded.enabled);
    }

    /// `settings.json` written before the read-only API feature shipped
    /// (no `api_key` or `api_port` keys) must still load, with `api_key`
    /// defaulting to "" and `api_port` defaulting to 7338.
    ///
    /// This is the critical upgrade-path bug that was fixed by changing
    /// `#[serde(default)]` on `api_port` to `#[serde(default = "default_api_port")]`
    /// — previously serde used `u16::default()` = 0, which disabled the
    /// read-only server on every upgrade until the user manually re-entered
    /// the port.
    #[test]
    fn legacy_settings_without_api_fields_loads_with_default_port() {
        let legacy = r#"{
            "host": "192.168.1.50",
            "port": 8899,
            "serial": "",
            "poll_interval": 60,
            "auto_connect": true,
            "import_tariff": 0.285,
            "export_tariff": 0.15,
            "hidden_panels": [],
            "evc_host": "",
            "disable_auto_discovery": true
        }"#;
        let decoded: Settings = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            decoded.api_key, "",
            "api_key should default to empty string"
        );
        assert_eq!(
            decoded.api_port, 7338,
            "api_port should default to 7338 (not 0) for legacy settings files"
        );
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
            import_standing_charge_p_per_day: 0.0,
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
            load_limiter_saved_reserve: None,
            evc_host: "192.168.1.200".to_string(),
            evc_port: 502,
            import_tariff_config: None,
            export_tariff_config: None,
            agile_enabled: false,
            agile_scope: AgileScope::default(),
            agile_region: "A".to_string(),
            agile_charge_threshold: 10.0,
            agile_discharge_threshold: 30.0,
            agile_api_base_url: String::new(),
            cosy_enabled: false,
            cosy_slots: vec![],
            cosy_active_persisted: false,
            agile_state_persisted: String::new(),
            hidden_panels: Vec::new(),
            alerts_config: AlertsConfig::default(),
            weather_config: WeatherConfig::default(),
            octopus_enabled: false,
            octopus_api_key: String::new(),
            octopus_account_number: String::new(),
            octopus_api_base_url: String::new(),
            disable_auto_discovery: true,
            autostart_enabled: false,
            api_key: String::new(),
            api_port: 0,
            discharge_slots_backup: None,
            pv1_rated_kw: 0.0,
            pv2_rated_kw: 0.0,
            solar_arrays: Vec::new(),
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
    // AgileScope settings tests
    // ======================================================================

    #[test]
    fn agile_scope_defaults_to_off() {
        let s = Settings::default();
        assert_eq!(s.agile_scope, AgileScope::Off);
        assert!(!s.agile_scope.is_enabled());
        assert!(!s.agile_scope.owns_charge());
        assert!(!s.agile_scope.owns_discharge());
    }

    #[test]
    fn agile_scope_full_owns_both_sides() {
        let scope = AgileScope::Full;
        assert!(scope.is_enabled());
        assert!(scope.owns_charge());
        assert!(scope.owns_discharge());
        assert!(scope.as_enabled());
    }

    #[test]
    fn agile_scope_charge_only_owns_only_charge() {
        let scope = AgileScope::ChargeOnly;
        assert!(scope.is_enabled());
        assert!(scope.owns_charge());
        assert!(!scope.owns_discharge());
    }

    #[test]
    fn agile_scope_discharge_only_owns_only_discharge() {
        let scope = AgileScope::DischargeOnly;
        assert!(scope.is_enabled());
        assert!(!scope.owns_charge());
        assert!(scope.owns_discharge());
    }

    #[test]
    fn agile_scope_serialises_as_snake_case() {
        assert_eq!(serde_json::to_string(&AgileScope::Off).unwrap(), "\"off\"");
        assert_eq!(
            serde_json::to_string(&AgileScope::Full).unwrap(),
            "\"full\""
        );
        assert_eq!(
            serde_json::to_string(&AgileScope::ChargeOnly).unwrap(),
            "\"charge_only\""
        );
        assert_eq!(
            serde_json::to_string(&AgileScope::DischargeOnly).unwrap(),
            "\"discharge_only\""
        );
    }

    #[test]
    fn agile_scope_roundtrip() {
        // Saving and reloading preserves all four variants.
        for scope in [
            AgileScope::Off,
            AgileScope::Full,
            AgileScope::ChargeOnly,
            AgileScope::DischargeOnly,
        ] {
            let s = Settings {
                agile_scope: scope,
                ..Settings::default()
            };
            let json = serde_json::to_string(&s).unwrap();
            let decoded: Settings = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded.agile_scope, scope, "roundtrip for {scope:?}");
        }
    }

    #[test]
    fn agile_scope_migration_legacy_enabled_true_becomes_full() {
        // A legacy v0.47 settings.json has `agile_enabled = true` and no
        // `agile_scope` field. The migration helper should map that to
        // Full so the user's original intent survives the upgrade.
        let s = Settings {
            agile_enabled: true,
            agile_scope: AgileScope::default(), // missing in legacy JSON
            ..Settings::default()
        };
        assert_eq!(agile_scope_for_settings(&s), AgileScope::Full);
    }

    #[test]
    fn agile_scope_migration_legacy_enabled_false_stays_off() {
        let s = Settings {
            agile_enabled: false,
            agile_scope: AgileScope::default(),
            ..Settings::default()
        };
        assert_eq!(agile_scope_for_settings(&s), AgileScope::Off);
    }

    #[test]
    fn agile_scope_explicit_field_wins_over_legacy_enabled() {
        // If a new-format file somehow has both fields, the explicit
        // scope field wins. This catches a regression where a buggy
        // settings UI writes `agile_enabled = false` alongside
        // `agile_scope = ChargeOnly` — the user explicitly picked
        // ChargeOnly, so respect that.
        let s = Settings {
            agile_enabled: false,
            agile_scope: AgileScope::ChargeOnly,
            ..Settings::default()
        };
        assert_eq!(agile_scope_for_settings(&s), AgileScope::ChargeOnly);
    }

    #[test]
    fn legacy_settings_json_without_scope_field_parses_with_default() {
        // A real v0.47 settings.json (no `agile_scope` field) must parse
        // without error and yield AgileScope::Off. This is the on-disk
        // upgrade path. We mirror the minimum-required field set used by
        // the existing `legacy_*` tests below.
        let legacy_json = r#"{
            "host": "192.168.1.50",
            "port": 8899,
            "serial": "",
            "poll_interval": 60,
            "auto_connect": true,
            "import_tariff": 0.285,
            "export_tariff": 0.15,
            "hidden_panels": [],
            "evc_host": "",
            "disable_auto_discovery": true,
            "agile_enabled": true
        }"#;
        let s: Settings =
            serde_json::from_str(legacy_json).expect("legacy v0.47 settings.json must parse");
        assert_eq!(s.agile_scope, AgileScope::Off); // serde default
        assert!(s.agile_enabled);
        // Migration helper produces the user's actual intent.
        assert_eq!(agile_scope_for_settings(&s), AgileScope::Full);
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
                                             // Final slot ends at "23:59" (the latest representable clock time);
                                             // its end is inclusive so it covers minute 1439.
        assert_eq!(cfg.slots.last().unwrap().end, "23:59");
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
        // Last slot is the only slot AND its end (20:00 = 1200) is inclusive
        // since it's the final slot → 20:00 (minute 1200) itself is covered.
        assert_eq!(cfg.rate_for_minutes(1200), Some(0.20));
    }

    /// The final slot is inclusive on both ends so its "23:59" end actually
    /// covers minute 1439 — without this the last minute of the day would
    /// be uncovered by the new model.
    #[test]
    fn tariff_final_slot_is_inclusive() {
        let cfg = TariffConfig::default();
        // 23:59 = minute 1439 → final slot (peak)
        assert_eq!(cfg.rate_for_minutes(1439), Some(0.285));
        // 23:58 = minute 1438 → final slot (peak)
        assert_eq!(cfg.rate_for_minutes(1438), Some(0.285));
        // 23:59 → still peak (inclusive), not the off-peak slot which ended
        // at 05:30.
        assert_ne!(cfg.rate_for_minutes(1439), Some(0.09));
    }

    /// Intermediate slots are still half-open: a slot ending at "05:30"
    /// covers up-to-but-not-including minute 330.
    #[test]
    fn tariff_intermediate_slot_is_half_open() {
        let cfg = TariffConfig {
            slots: vec![
                TariffSlot {
                    start: "00:00".to_string(),
                    end: "05:30".to_string(),
                    rate: 0.10,
                },
                TariffSlot {
                    start: "05:30".to_string(),
                    end: "23:59".to_string(),
                    rate: 0.30,
                },
            ],
        };
        // 05:29 = minute 329 → first slot
        assert_eq!(cfg.rate_for_minutes(329), Some(0.10));
        // 05:30 = minute 330 → second slot (first slot's end is exclusive)
        assert_eq!(cfg.rate_for_minutes(330), Some(0.30));
    }

    /// parse_hhmm_to_minutes rejects "24:00" (no longer a valid clock time).
    #[test]
    fn tariff_parser_rejects_24_00() {
        assert_eq!(parse_hhmm_to_minutes("24:00"), None);
        assert_eq!(parse_hhmm_to_minutes("23:59"), Some(1439));
        assert_eq!(parse_hhmm_to_minutes("00:00"), Some(0));
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
        // Should produce 3 slots: peak(00:00-00:30), off-peak(00:30-05:30), peak(05:30-23:59).
        assert_eq!(cfg.slots.len(), 3);
        // 02:00 → off-peak
        assert_eq!(cfg.rate_for_minutes(120), Some(0.10));
        // 12:00 → peak
        assert_eq!(cfg.rate_for_minutes(720), Some(0.30));
        // 00:00 → peak (before off-peak starts)
        assert_eq!(cfg.rate_for_minutes(0), Some(0.30));
        // Last slot is "23:59" (inclusive) — verify the migrated shape uses
        // the new clock-time representation.
        assert_eq!(cfg.slots.last().unwrap().end, "23:59");
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
        // Migrated final slot is "23:59" inclusive (the rest of the day
        // after 23:00 is off-peak per the legacy midnight-crossing window).
        assert_eq!(cfg.slots.last().unwrap().end, "23:59");
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
                    end: "23:59".to_string(),
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
        // 23:59 = minute 1439 → last slot (peak, inclusive)
        assert_eq!(decoded.rate_for_minutes(1439), Some(0.35));
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
        assert!(
            !cfg.slots.is_empty(),
            "empty slots must fall back to default"
        );
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

    // ======================================================================
    // TariffConfig::validate tests (server-side validation, mirrored by
    // frontend's validateTariffConfig).
    // ======================================================================

    #[test]
    fn validate_default_passes() {
        assert!(TariffConfig::default().validate().is_ok());
    }

    #[test]
    fn validate_flat_passes() {
        let cfg = TariffConfig {
            slots: vec![TariffSlot {
                start: "00:00".to_string(),
                end: "23:59".to_string(),
                rate: 0.20,
            }],
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_slots() {
        let cfg = TariffConfig { slots: vec![] };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_first_slot_not_at_midnight() {
        let cfg = TariffConfig {
            slots: vec![TariffSlot {
                start: "01:00".to_string(),
                end: "23:59".to_string(),
                rate: 0.20,
            }],
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("00:00"), "got: {err}");
    }

    #[test]
    fn validate_rejects_last_slot_not_at_23_59() {
        let cfg = TariffConfig {
            slots: vec![TariffSlot {
                start: "00:00".to_string(),
                end: "22:00".to_string(),
                rate: 0.20,
            }],
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("23:59"), "got: {err}");
    }

    #[test]
    fn validate_rejects_gap_between_slots() {
        let cfg = TariffConfig {
            slots: vec![
                TariffSlot {
                    start: "00:00".to_string(),
                    end: "05:00".to_string(),
                    rate: 0.20,
                },
                // Gap: next slot starts at 06:00 instead of 05:00.
                TariffSlot {
                    start: "06:00".to_string(),
                    end: "23:59".to_string(),
                    rate: 0.30,
                },
            ],
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("gap"), "got: {err}");
    }

    #[test]
    fn validate_rejects_overlapping_slots() {
        let cfg = TariffConfig {
            slots: vec![
                TariffSlot {
                    start: "00:00".to_string(),
                    end: "06:00".to_string(),
                    rate: 0.20,
                },
                // Overlap: starts at 05:00, before the previous ends at 06:00.
                TariffSlot {
                    start: "05:00".to_string(),
                    end: "23:59".to_string(),
                    rate: 0.30,
                },
            ],
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("overlap"), "got: {err}");
    }

    #[test]
    fn validate_rejects_negative_rate() {
        let cfg = TariffConfig {
            slots: vec![TariffSlot {
                start: "00:00".to_string(),
                end: "23:59".to_string(),
                rate: -0.10,
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_non_finite_rate() {
        let cfg = TariffConfig {
            slots: vec![TariffSlot {
                start: "00:00".to_string(),
                end: "23:59".to_string(),
                rate: f64::NAN,
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_malformed_time_string() {
        let cfg = TariffConfig {
            slots: vec![TariffSlot {
                start: "garbage".to_string(),
                end: "23:59".to_string(),
                rate: 0.20,
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_24_00_time_string() {
        // "24:00" is no longer a valid time — the final slot must be
        // "23:59" (inclusive).
        let cfg = TariffConfig {
            slots: vec![TariffSlot {
                start: "00:00".to_string(),
                end: "24:00".to_string(),
                rate: 0.20,
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_start_after_end() {
        let cfg = TariffConfig {
            slots: vec![TariffSlot {
                start: "10:00".to_string(),
                end: "09:00".to_string(),
                rate: 0.20,
            }],
        };
        assert!(cfg.validate().is_err());
    }
}
