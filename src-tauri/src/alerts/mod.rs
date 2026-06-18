//! Alert evaluation engine and Telegram notification sender.
//!
//! Evaluates inverter snapshot against user-configured thresholds after
//! sanitization, then sends notifications via the Telegram Bot API.
//!
//! Also handles daily consumption report generation and sending.

pub mod report;

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::inverter::model::InverterSnapshot;
use crate::settings::AlertsConfig;

// ---------------------------------------------------------------------------
// Alert types
// ---------------------------------------------------------------------------

/// Alert types that can fire independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertType {
    BatteryTempHigh,
    BatteryTempLow,
    BatterySocHigh,
    BatterySocLow,
    /// The inverter/BMS reported a battery warning via its hardware fault
    /// register (`IR(57) charger_warning_code`). This is distinct from
    /// [`BatteryTempHigh`], which is gated by the user's °C threshold — this
    /// variant is the device's own warning flag and is *not* tied to a
    /// configured temperature. It is subject to a consecutive-read
    /// confirmation (see [`AlertDebounce::confirm_battery_warning`]) so a
    /// single corrupted register read cannot trigger it.
    BatteryOverTemp,
    /// Solar generation has sustained above the user-configured clipping
    /// ceiling (`solar_clipping_ceiling_w`) for several consecutive cycles,
    /// indicating the inverter is likely curtailing output and the user is
    /// losing potential generation. Gated by a consecutive-read confirmation
    /// (see [`AlertDebounce::confirm_solar_clipping`]) so a momentary
    /// cloud-edge spike above the ceiling does not fire it.
    SolarClipping,
    GridOffline,
}

impl AlertType {
    /// Human-readable name for the alert.
    pub fn human_name(&self) -> &'static str {
        match self {
            Self::BatteryTempHigh => "Battery Temperature High",
            Self::BatteryTempLow => "Battery Temperature Low",
            Self::BatterySocHigh => "Battery SOC High",
            Self::BatterySocLow => "Battery SOC Low",
            Self::GridOffline => "Grid Offline",
            // Deliberately distinct from "Battery Temperature High": this one
            // is the inverter's own hardware warning flag (IR 57), not the
            // user's °C threshold being breached. Renamed from
            // "Battery Over-Temperature" to avoid users conflating it with
            // the threshold-based alert.
            Self::BatteryOverTemp => "Inverter Battery Warning",
            Self::SolarClipping => "Solar Clipping",
        }
    }
}

// ---------------------------------------------------------------------------
// Debounce tracker
// ---------------------------------------------------------------------------

/// Per-alert-type debounce tracker to prevent notification floods.
/// Number of consecutive poll cycles the inverter's hardware battery
/// warning flag (`IR(57)`) must read `true` before the `BatteryOverTemp`
/// alert is allowed to fire. The GivEnergy data adapter occasionally returns
/// a transiently corrupted value on this register, and a single blip should
/// never fire a warning. See [`AlertDebounce::confirm_battery_warning`].
pub(crate) const BATTERY_WARNING_CONFIRM_CYCLES: u32 = 3;

/// Number of consecutive poll cycles solar generation must exceed the
/// configured clipping ceiling before the [`AlertType::SolarClipping`] alert
/// is allowed to fire. Prevents a momentary cloud-edge spike above the
/// ceiling from triggering a clipping alert. See
/// [`AlertDebounce::confirm_solar_clipping`].
pub(crate) const SOLAR_CLIPPING_CONFIRM_CYCLES: u32 = 3;

#[derive(Debug)]
pub struct AlertDebounce {
    /// Map from alert type to the last time it was sent.
    last_sent: HashMap<AlertType, Instant>,
    /// Set of alert types currently in an active (fired) state.
    active: std::collections::HashSet<AlertType>,
    /// Consecutive-cycle count of the inverter's raw `IR(57)` warning flag.
    /// Reset to 0 the moment the flag reads `false`. The `BatteryOverTemp`
    /// alert is only confirmed once this reaches
    /// [`BATTERY_WARNING_CONFIRM_CYCLES`].
    battery_warning_streak: u32,
    /// Consecutive-cycle count of "solar above ceiling". Reset to 0 the moment
    /// solar drops back below the ceiling. The `SolarClipping` alert is only
    /// confirmed once this reaches [`SOLAR_CLIPPING_CONFIRM_CYCLES`].
    solar_clipping_streak: u32,
}

impl AlertDebounce {
    pub fn new() -> Self {
        Self {
            last_sent: HashMap::new(),
            active: std::collections::HashSet::new(),
            battery_warning_streak: 0,
            solar_clipping_streak: 0,
        }
    }

    /// Feed the inverter's raw `IR(57)` warning flag for this cycle and return
    /// `true` only if the flag has now read `true` for
    /// [`BATTERY_WARNING_CONFIRM_CYCLES`] consecutive cycles. A single
    /// `false` resets the streak to 0, so a genuine warning that blinks off
    /// for one cycle has to build the streak back up before re-firing.
    ///
    /// This is the register-corruption defence for the over-temp alert:
    /// transient garbage on `IR(57)` (which is not otherwise sanitised) cannot
    /// trigger a warning on its own.
    pub fn confirm_battery_warning(&mut self, flag: bool) -> bool {
        if flag {
            self.battery_warning_streak = self.battery_warning_streak.saturating_add(1);
            self.battery_warning_streak >= BATTERY_WARNING_CONFIRM_CYCLES
        } else {
            self.battery_warning_streak = 0;
            false
        }
    }

    /// Feed this cycle's "solar generation exceeds the configured ceiling"
    /// flag and return `true` only if it has held `true` for
    /// [`SOLAR_CLIPPING_CONFIRM_CYCLES`] consecutive cycles. A single `false`
    /// (solar dropped back below the ceiling) resets the streak to 0.
    ///
    /// This is the precision defence for the solar-clipping alert: a
    /// momentary cloud-edge spike above the ceiling does not fire it, only a
    /// sustained over-ceiling state does.
    pub fn confirm_solar_clipping(&mut self, over_ceiling: bool) -> bool {
        if over_ceiling {
            self.solar_clipping_streak = self.solar_clipping_streak.saturating_add(1);
            self.solar_clipping_streak >= SOLAR_CLIPPING_CONFIRM_CYCLES
        } else {
            self.solar_clipping_streak = 0;
            false
        }
    }

    /// Returns `true` if this alert type should fire (cooldown has elapsed).
    pub fn should_fire(&mut self, alert_type: AlertType, cooldown_minutes: u32) -> bool {
        let cooldown = std::time::Duration::from_secs(cooldown_minutes as u64 * 60);
        match self.last_sent.get(&alert_type) {
            Some(last) if last.elapsed() < cooldown => false,
            _ => {
                self.last_sent.insert(alert_type, Instant::now());
                self.active.insert(alert_type);
                true
            }
        }
    }

    /// Compute which previously-active alerts are no longer triggered,
    /// and remove them from the active set.
    pub fn extract_cleared(&mut self, currently_triggered: &[AlertType]) -> Vec<AlertType> {
        let triggered_set: std::collections::HashSet<_> =
            currently_triggered.iter().copied().collect();
        let cleared: Vec<_> = self.active.difference(&triggered_set).copied().collect();
        for c in &cleared {
            self.active.remove(c);
        }
        cleared
    }

    /// Number of entries in the debounce map (for API display).
    /// Remove an alert type from both last_sent and active sets,
    /// clearing its cooldown and re-enabling immediate re-fire.
    pub fn reset_for_type(&mut self, alert_type: AlertType) {
        self.last_sent.remove(&alert_type);
        self.active.remove(&alert_type);
    }

    /// Clear ALL debounce state — use when the user saves alert settings
    /// so previously-fired alerts can re-trigger immediately. Also resets
    /// the hardware battery-warning confirmation streak so a stale confirmed
    /// flag doesn't carry across a config change.
    pub fn clear(&mut self) {
        self.last_sent.clear();
        self.active.clear();
        self.battery_warning_streak = 0;
        self.solar_clipping_streak = 0;
    }

    pub fn len(&self) -> usize {
        self.last_sent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.last_sent.is_empty()
    }
}

impl Default for AlertDebounce {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Alert evaluation
// ---------------------------------------------------------------------------

/// Evaluate the snapshot against configured thresholds.
/// Returns all alert types that are currently breached.
pub fn evaluate_alerts(snapshot: &InverterSnapshot, config: &AlertsConfig) -> Vec<AlertType> {
    if !config.enabled {
        return Vec::new();
    }
    let has_telegram = !config.telegram_bot_token.is_empty() && !config.telegram_chat_id.is_empty();
    let has_ntfy = !config.ntfy_topic.is_empty();

    if !has_telegram && !has_ntfy {
        return Vec::new();
    }

    let mut alerts = Vec::new();

    // Battery temperature
    let temp = snapshot.battery_temperature;
    if config.batt_temp_max > 0.0 && temp > config.batt_temp_max {
        alerts.push(AlertType::BatteryTempHigh);
    }
    if config.batt_temp_min > 0.0 && temp < config.batt_temp_min {
        alerts.push(AlertType::BatteryTempLow);
    }

    // Battery SOC
    let soc = snapshot.soc;
    if config.soc_min > 0 && soc < config.soc_min {
        alerts.push(AlertType::BatterySocLow);
    }
    if config.soc_max < 100 && soc > config.soc_max {
        alerts.push(AlertType::BatterySocHigh);
    }
    // Grid offline — match the frontend hasGridFault() logic:
    // also trigger when grid_online is false even if grid_loss is
    // not set (they come from separate register decodes).
    if config.grid_offline_enabled && (snapshot.grid_loss || !snapshot.grid_online) {
        alerts.push(AlertType::GridOffline);
    }

    // Battery over-temperature
    if config.battery_over_temp_enabled && snapshot.battery_over_temp {
        alerts.push(AlertType::BatteryOverTemp);
    }

    // Solar clipping — solar generation above the configured ceiling.
    // `ceiling_w == 0` means disabled (no ceiling set). The consecutive-read
    // confirmation that filters transient spikes is applied by the caller via
    // [`AlertDebounce::confirm_solar_clipping`].
    if config.solar_clipping_enabled
        && config.solar_clipping_ceiling_w > 0
        && snapshot.solar_power > config.solar_clipping_ceiling_w as i32
    {
        alerts.push(AlertType::SolarClipping);
    }

    alerts
}

// ---------------------------------------------------------------------------
// Notification body builder
// ---------------------------------------------------------------------------

/// Build the plain-text notification message for triggered alerts.
pub fn build_alert_message(snapshot: &InverterSnapshot, alerts: &[AlertType]) -> String {
    let time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let mut msg = format!("⚡ HEM Alert — {}\n", time);
    msg.push_str("━━━━━━━━━━━━━━━━━━━━━━━━\n");

    msg.push_str("Triggers:\n");
    for alert in alerts {
        msg.push_str(&format!("  🔸 {}\n", alert.human_name()));
    }

    msg.push_str("\nSystem Status:\n");
    msg.push_str(&format!(
        "  Battery temp: {:.1}°C\n  Battery SOC: {}%\n  Solar: {} W\n  Grid: {} W\n  Home: {} W\n  Battery Pwr: {} W\n  Grid Online: {}\n  Inverter temp: {:.1}°C\n",
        snapshot.battery_temperature,
        snapshot.soc,
        snapshot.solar_power,
        snapshot.grid_power,
        snapshot.home_power,
        snapshot.battery_power,
        if snapshot.grid_online { "Yes" } else { "No" },
        snapshot.inverter_temperature,
    ));

    msg
}

/// Build a "problem cleared" notification message.
pub fn build_cleared_message(snapshot: &InverterSnapshot, alerts: &[AlertType]) -> String {
    let time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let mut msg = format!("✅ HEM All Clear — {}\n", time);
    msg.push_str("━━━━━━━━━━━━━━━━━━━━━━━━\n");

    msg.push_str("Resolved:\n");
    for alert in alerts {
        msg.push_str(&format!("  ▪ {} — back to normal\n", alert.human_name()));
    }

    msg.push_str("\nSystem Status:\n");
    msg.push_str(&format!(
        "  Battery temp: {:.1}°C\n  Battery SOC: {}%\n  Solar: {} W\n  Grid: {} W\n  Home: {} W\n  Battery Pwr: {} W\n  Grid Online: {}\n  Inverter temp: {:.1}°C\n",
        snapshot.battery_temperature,
        snapshot.soc,
        snapshot.solar_power,
        snapshot.grid_power,
        snapshot.home_power,
        snapshot.battery_power,
        if snapshot.grid_online { "Yes" } else { "No" },
        snapshot.inverter_temperature,
    ));

    msg
}

// ---------------------------------------------------------------------------
// Telegram sender
// ---------------------------------------------------------------------------

/// Hard end-to-end timeout for any single Telegram Bot API HTTP call.
///
/// `getUpdates` uses a server-side long-poll (`timeout=10`), so the server
/// legitimately holds an idle connection open for up to 10s. This constant
/// must exceed that by a comfortable margin; 20s gives ~10s headroom for a
/// healthy empty poll while bounding any network-layer stall to 20s instead
/// of the OS TCP timeout (which is **minutes** and froze the whole poller).
///
/// See `telegram_agent()`.
const TELEGRAM_HTTP_TIMEOUT: u64 = 20;

/// Shared HTTP agent for all Telegram Bot API calls.
///
/// Configured with a global end-to-end timeout so a single stalled call —
/// DNS, connect, or (most commonly in containerised deployments) a held-open
/// `getUpdates` long-poll that the network layer has silently stalled —
/// cannot freeze the single-threaded poll loop for the OS-level TCP timeout.
/// A stalled call now dies after [`TELEGRAM_HTTP_TIMEOUT`] and the loop
/// continues, so command latency is bounded (~23s worst case) instead of
/// minutes-to-forever.
///
/// The agent is shared via [`OnceLock`] so its connection pool is reused
/// across calls; clones are cheap (inner `Arc`).
fn telegram_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(TELEGRAM_HTTP_TIMEOUT)))
            .build();
        ureq::Agent::new_with_config(config)
    })
}

/// Send a notification via the Telegram Bot API.
///
/// Uses `ureq` (synchronous) — call from `tokio::task::spawn_blocking`.
pub fn send_telegram_message(bot_token: &str, chat_id: &str, text: &str) -> Result<(), String> {
    let url = format!("https://api.telegram.org/bot{}/sendMessage", bot_token);

    let payload = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "HTML",
    });

    let body = serde_json::to_string(&payload).map_err(|e| format!("Failed to serialize: {e}"))?;

    let resp = match telegram_agent()
        .post(&url)
        .content_type("application/json")
        .send(&body)
    {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(code)) => {
            return Err(format!("Telegram API {} (check token and chat ID)", code));
        }
        Err(e) => return Err(format!("HTTP transport error: {e}")),
    };

    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        let text = resp
            .into_body()
            .read_to_string()
            .unwrap_or_else(|_| "<read error>".to_string());
        Err(format!("Telegram API {}: {}", status, text))
    }
}

/// Send a notification via ntfy.sh (or a self-hosted ntfy server).
///
/// Uses `ureq` (synchronous) — call from `tokio::task::spawn_blocking`.
pub fn send_ntfy_message(topic: &str, server: &str, text: &str) -> Result<(), String> {
    let url = format!("{}/{}", server.trim_end_matches('/'), topic);

    let server_display = server;
    let resp = match ureq::post(&url)
        .content_type("text/plain")
        .send(text.to_string())
    {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(code)) => {
            return Err(format!("ntfy API {} at {}", code, server_display));
        }
        Err(e) => return Err(format!("HTTP transport error to {}: {e}", server_display)),
    };

    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("ntfy API {} at {}", status, server_display))
    }
}

// ---------------------------------------------------------------------------
// Telegram command polling (e.g., /status)
// ---------------------------------------------------------------------------

/// Build a status message from the current inverter snapshot.
fn build_status_message(snapshot: &InverterSnapshot) -> String {
    let time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let mut msg = format!("📡 <b>System Status</b> — {}\n", time);
    msg.push_str("━━━━━━━━━━━━━━━━━━━━━━━━\n");
    msg.push_str(&format!(
        "☀️ Solar: <b>{} W</b>  (PV1: {} W, PV2: {})\n",
        snapshot.solar_power, snapshot.pv1_power, snapshot.pv2_power
    ));
    msg.push_str(&format!("🏠 Home: <b>{} W</b>\n", snapshot.home_power));
    msg.push_str(&format!(
        "🔋 Battery: <b>{} W</b>  (SOC: <b>{}%</b>)\n",
        snapshot.battery_power, snapshot.soc
    ));
    msg.push_str(&format!("⚡ Grid: <b>{} W</b>\n", snapshot.grid_power));
    msg.push_str(&format!(
        "🌡️ Battery temp: {:.1}°C  |  Inverter: {:.1}°C\n",
        snapshot.battery_temperature, snapshot.inverter_temperature
    ));
    msg.push_str(&format!(
        "📶 Grid: {}\n",
        if snapshot.grid_online {
            "🟢 Online"
        } else {
            "🔴 Offline"
        }
    ));
    msg.push_str(&format!(
        "☀️ Today generated: <b>{:.1} kWh</b>\n",
        snapshot.today_solar_kwh
    ));
    msg.push_str(&format!(
        "📥 Today imported: <b>{:.1} kWh</b>\n",
        snapshot.today_import_kwh
    ));
    msg.push_str(&format!(
        "📤 Today exported: <b>{:.1} kWh</b>\n",
        snapshot.today_export_kwh
    ));
    msg
}

/// Per-module battery detail (for `/battery`).
fn build_battery_message(snapshot: &InverterSnapshot) -> String {
    let time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let mut msg = format!("🔋 <b>Battery Detail</b> — {time}\n");
    msg.push_str("━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    msg.push_str(&format!(
        "Overall: <b>{}%</b> SOC · {:.1}°C · {:.1}V · {} W\n",
        snapshot.soc,
        snapshot.battery_temperature,
        snapshot.battery_voltage,
        snapshot.battery_power
    ));
    msg.push_str(&format!(
        "Capacity: {:.1} kWh · State: {}\n",
        snapshot.battery_capacity_kwh,
        match snapshot.battery_state {
            crate::inverter::model::BatteryState::Charging => "Charging",
            crate::inverter::model::BatteryState::Discharging => "Discharging",
            crate::inverter::model::BatteryState::Idle => "Idle",
        }
    ));

    if snapshot.battery_modules.is_empty() {
        msg.push_str("\n<i>No per-module BMS data available.</i>");
    } else {
        msg.push_str(&format!(
            "\n<b>Modules ({})</b>:\n",
            snapshot.battery_modules.len()
        ));
        for m in &snapshot.battery_modules {
            msg.push_str(&format!(
                "  #{}: {}% · {:.1}°C · {:.1}V · {:.1}/{:.0}Ah · {} cycles\n",
                m.index,
                m.soc,
                m.temperature,
                m.voltage,
                m.remaining_capacity_ah,
                m.capacity_ah,
                m.num_cycles
            ));
        }
        msg.pop(); // trailing newline
    }
    msg
}

/// Battery mode and configuration (for `/mode`).
fn build_mode_message(snapshot: &InverterSnapshot) -> String {
    let time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let mut msg = format!("⚙️ <b>Battery Mode</b> — {time}\n");
    msg.push_str("━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    msg.push_str(&format!(
        "Mode: <b>{}</b>\n",
        match snapshot.battery_mode {
            crate::inverter::model::BatteryMode::Eco => "Eco",
            crate::inverter::model::BatteryMode::EcoPaused => "Eco (Paused)",
            crate::inverter::model::BatteryMode::TimedDemand => "Timed Demand",
            crate::inverter::model::BatteryMode::TimedExport => "Timed Export",
            crate::inverter::model::BatteryMode::ExportPaused => "Export (Paused)",
            crate::inverter::model::BatteryMode::Unknown => "Unknown",
        }
    ));
    msg.push_str(&format!(
        "Reserve: <b>{}%</b> · Target SOC: <b>{}%</b>\n",
        snapshot.battery_reserve, snapshot.target_soc
    ));
    msg.push_str(&format!(
        "Charge rate: <b>{}%</b> · Discharge rate: <b>{}%</b>\n",
        snapshot.charge_rate, snapshot.discharge_rate
    ));

    // Active automation / status flags
    let mut flags: Vec<&str> = Vec::new();
    if snapshot.cosy_active {
        flags.push("Cosy charging");
    } else if snapshot.cosy_enabled {
        flags.push("Cosy idle");
    }
    if snapshot.agile_active {
        flags.push("Agile active");
    } else if snapshot.agile_enabled {
        flags.push("Agile idle");
    }
    if snapshot.auto_winter_active {
        flags.push("Auto-winter");
    }
    if snapshot.load_limiter_active {
        flags.push("Load limiter");
    }
    if flags.is_empty() {
        msg.push_str("Automation: none active");
    } else {
        msg.push_str(&format!("Active: {}", flags.join(", ")));
    }
    msg
}

/// System / firmware info (for `/version`).
fn build_version_message(snapshot: &InverterSnapshot) -> String {
    let mut msg = "📋 <b>System Info</b>\n".to_string();
    msg.push_str("━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    msg.push_str(&format!("App: <b>v{}</b>\n", env!("CARGO_PKG_VERSION")));
    if !snapshot.device_type_display.is_empty() {
        msg.push_str(&format!("Device: {}\n", snapshot.device_type_display));
    }
    msg.push_str(&format!(
        "Serial: <code>{}</code>\n",
        snapshot.inverter_serial
    ));
    msg.push_str(&format!("ARM firmware: {}", snapshot.firmware_version));
    if !snapshot.dsp_firmware_version.is_empty() {
        msg.push_str(&format!(
            "\nDSP firmware: {}",
            snapshot.dsp_firmware_version
        ));
    }
    msg
}

/// List of available commands (for `/help` and `/start`).
fn build_help_message() -> String {
    let mut msg = "🤖 <b>Home Energy Manager</b>\n".to_string();
    msg.push_str("━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    msg.push_str("<b>Commands</b>:\n");
    msg.push_str("/status — Live system status\n");
    msg.push_str("/today — Today's energy summary & cost\n");
    msg.push_str("/battery — Battery & module details\n");
    msg.push_str("/mode — Battery mode & settings\n");
    msg.push_str("/version — System & firmware info\n");
    msg.push_str("/help — Show this help");
    msg
}

/// Register the bot's command menu with Telegram so that typing `/`
/// auto-suggests the commands in the chat. Best-effort: failures are logged
/// but never fatal.
pub fn register_telegram_commands(bot_token: &str) {
    let url = format!("https://api.telegram.org/bot{}/setMyCommands", bot_token);
    let payload = serde_json::json!({
        "commands": [
            { "command": "status",  "description": "Live system status" },
            { "command": "today",   "description": "Today's energy summary & cost" },
            { "command": "battery", "description": "Battery & module details" },
            { "command": "mode",    "description": "Battery mode & settings" },
            { "command": "version", "description": "System & firmware info" },
            { "command": "help",    "description": "Show available commands" },
        ]
    });
    let Ok(body) = serde_json::to_string(&payload) else {
        return;
    };
    match telegram_agent()
        .post(&url)
        .content_type("application/json")
        .send(&body)
    {
        Ok(r) if r.status().is_success() => {
            tracing::info!("Telegram command menu registered");
        }
        Ok(r) => {
            tracing::warn!("Telegram setMyCommands returned {}", r.status());
        }
        Err(e) => {
            tracing::warn!("Failed to register Telegram commands: {e}");
        }
    }
}

/// Extract the lowercase command name from an incoming message: trims
/// whitespace, takes the first token, and strips an optional `@botname`
/// suffix (Telegram sends this in group chats).
fn parse_command(text: &str) -> String {
    let first = text.split_whitespace().next().unwrap_or("");
    let without_suffix = first.split('@').next().unwrap_or(first);
    without_suffix
        .strip_prefix('/')
        .unwrap_or(without_suffix)
        .to_ascii_lowercase()
}

/// Build the `/today` summary by querying today's history and the configured
/// tariffs. Async because it locks the history DB and reads settings.
async fn build_today_reply(state: &crate::inverter::poll::AppState) -> String {
    let today = chrono::Local::now().date_naive();
    let date_str = today.format("%A %d %B").to_string();

    let db_guard = state.history.lock().await;
    let db = db_guard.clone();
    drop(db_guard);

    let Some(db) = db else {
        return "⚠️ History database not available.".to_string();
    };

    let rows = match db.get_readings_for_date(today) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("Telegram /today query failed: {e}");
            return format!("⚠️ Could not read history: {e}");
        }
    };
    if rows.is_empty() {
        return "⚠️ No history data for today yet.".to_string();
    }

    let settings = crate::settings::Settings::load();
    match crate::alerts::report::generate_daily_summary_text(&rows, &date_str, &settings) {
        Some(s) => s,
        None => "⚠️ Not enough data to summarise today yet.".to_string(),
    }
}

/// Spawns a background task that polls Telegram for commands and replies
/// with inverter data. Supported commands: `/status`, `/today`, `/battery`,
/// `/mode`, `/version`, `/help`.
///
/// Security: only the chat id configured in `alert_config.telegram_chat_id`
/// receives replies — every other chat is silently ignored. The task reads
/// `state.alert_config` on each cycle so config changes (token / chat id
/// updates) take effect without restart.
pub fn spawn_telegram_poller(state: std::sync::Arc<crate::inverter::poll::AppState>) {
    tracing::debug!("Telegram poller started");
    tokio::spawn(async move {
        let mut offset: i64 = 0;
        let mut commands_registered = false;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;

            let config = state.alert_config.lock().await.clone();
            if !config.enabled || config.telegram_bot_token.is_empty() {
                // Token removed/alerts disabled — re-register the command menu
                // the next time a token is configured.
                commands_registered = false;
                continue;
            }
            let token = config.telegram_bot_token.clone();
            // Only the configured chat may interact with the bot. If no chat
            // id is set, nobody can issue commands (consistent with alerts).
            let allowed_chat = config.telegram_chat_id.parse::<i64>().ok();

            // Register the command menu once per token (so `/` autocompletes).
            if !commands_registered {
                let reg_token = token.clone();
                tokio::task::spawn_blocking(move || register_telegram_commands(&reg_token))
                    .await
                    .ok();
                commands_registered = true;
            }

            let cur_offset = offset;
            let poll_token = token.clone();

            // Run the HTTP poll on a blocking thread so we don't stall the async runtime
            let updates = tokio::task::spawn_blocking(move || -> Vec<(i64, i64, String)> {
                let url = format!(
                    "https://api.telegram.org/bot{}/getUpdates?offset={}&timeout=10",
                    poll_token, cur_offset
                );
                let result = telegram_agent().get(&url).call();
                match result {
                    Ok(r) => {
                        let body = r.into_body().read_to_string().unwrap_or_default();
                        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&body) {
                            let mut msgs = Vec::new();
                            if let Some(results) = data["result"].as_array() {
                                for update in results {
                                    let update_id = update["update_id"].as_i64().unwrap_or(0);
                                    if let Some(msg) = update.get("message") {
                                        let chat_id = msg["chat"]["id"].as_i64().unwrap_or(0);
                                        let text = msg["text"].as_str().unwrap_or("").to_string();
                                        msgs.push((update_id, chat_id, text));
                                    }
                                }
                            }
                            msgs
                        } else {
                            Vec::new()
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Telegram poll error: {e}");
                        Vec::new()
                    }
                }
            })
            .await
            .unwrap_or_default();

            for (update_id, chat_id, text) in &updates {
                if *update_id >= offset {
                    offset = update_id + 1;
                }

                // Allowlist: only respond to the configured chat.
                match allowed_chat {
                    Some(allowed) if *chat_id == allowed => {}
                    _ => continue,
                }

                let cmd = parse_command(text);
                if cmd.is_empty() {
                    continue;
                }

                let reply = match cmd.as_str() {
                    "start" | "help" => build_help_message(),
                    "status" => {
                        let snapshot = state.latest_snapshot.lock().await;
                        match &*snapshot {
                            Some(s) => build_status_message(s),
                            None => "⚠️ No inverter data available yet. Waiting for connection..."
                                .to_string(),
                        }
                    }
                    "battery" => {
                        let snapshot = state.latest_snapshot.lock().await;
                        match &*snapshot {
                            Some(s) => build_battery_message(s),
                            None => "⚠️ No inverter data available yet.".to_string(),
                        }
                    }
                    "mode" => {
                        let snapshot = state.latest_snapshot.lock().await;
                        match &*snapshot {
                            Some(s) => build_mode_message(s),
                            None => "⚠️ No inverter data available yet.".to_string(),
                        }
                    }
                    "version" => {
                        let snapshot = state.latest_snapshot.lock().await;
                        match &*snapshot {
                            Some(s) => build_version_message(s),
                            None => "⚠️ No inverter data available yet.".to_string(),
                        }
                    }
                    "today" => build_today_reply(&state).await,
                    // Unrecognized command from the allowed chat → help.
                    _ => build_help_message(),
                };

                let cid_str = chat_id.to_string();
                let token_c = token.clone();
                tracing::info!("Telegram: replying to '/{cmd}' for chat {chat_id}");
                tokio::task::spawn_blocking(move || {
                    if let Err(e) = send_telegram_message(&token_c, &cid_str, &reply) {
                        tracing::warn!("Telegram reply failed: {e}");
                    }
                });
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::InverterSnapshot;

    fn make_snapshot() -> InverterSnapshot {
        InverterSnapshot {
            battery_temperature: 35.0,
            soc: 80,
            solar_power: 5200,
            pv1_power: 3000,
            pv2_power: 2200,
            grid_power: -500,
            home_power: 1200,
            battery_power: -300,
            grid_online: true,
            grid_loss: false,
            battery_over_temp: false,
            max_ac_power_w: 6000,
            inverter_temperature: 42.0,
            ..Default::default()
        }
    }

    fn alerts_config() -> AlertsConfig {
        AlertsConfig {
            enabled: true,
            telegram_bot_token: "test:token".to_string(),
            telegram_chat_id: "12345".to_string(),
            cooldown_minutes: 30,
            batt_temp_min: 0.0,
            batt_temp_max: 45.0,
            soc_min: 10,
            soc_max: 95,
            grid_offline_enabled: false,
            battery_over_temp_enabled: false,
            solar_clipping_enabled: false,
            solar_clipping_ceiling_w: 0,
            ntfy_topic: String::new(),
            ntfy_server: "https://ntfy.sh".to_string(),
            daily_report_enabled: false,
            daily_report_hour: 8,
            daily_report_minute: 0,
        }
    }

    // ================================================================
    // evaluate_alerts
    // ================================================================

    #[test]
    fn test_no_alerts_when_disabled() {
        let mut config = alerts_config();
        config.enabled = false;
        let alerts = evaluate_alerts(&make_snapshot(), &config);
        assert!(alerts.is_empty());
    }

    #[test]
    fn test_no_alerts_without_bot_token() {
        let mut config = alerts_config();
        config.telegram_bot_token.clear();
        let alerts = evaluate_alerts(&make_snapshot(), &config);
        assert!(alerts.is_empty());
    }

    #[test]
    fn test_no_alerts_without_chat_id() {
        let mut config = alerts_config();
        config.telegram_chat_id.clear();
        let alerts = evaluate_alerts(&make_snapshot(), &config);
        assert!(alerts.is_empty());
    }

    #[test]
    fn test_battery_temp_high() {
        let snap = make_snapshot();
        let config = alerts_config();
        let alerts = evaluate_alerts(&snap, &config);
        assert!(
            !alerts.contains(&AlertType::BatteryTempHigh),
            "35°C should not trigger 45°C threshold"
        );
    }

    #[test]
    fn test_battery_temp_high_triggers() {
        let mut snap = make_snapshot();
        snap.battery_temperature = 50.0;
        let config = alerts_config();
        let alerts = evaluate_alerts(&snap, &config);
        assert!(alerts.contains(&AlertType::BatteryTempHigh));
    }

    #[test]
    fn test_battery_temp_low() {
        let mut snap = make_snapshot();
        snap.battery_temperature = 5.0;
        let mut config = alerts_config();
        config.batt_temp_min = 10.0;
        let alerts = evaluate_alerts(&snap, &config);
        assert!(alerts.contains(&AlertType::BatteryTempLow));
    }

    #[test]
    fn test_battery_temp_no_alert_when_ok() {
        let snap = make_snapshot();
        let mut config = alerts_config();
        config.batt_temp_min = 0.0;
        config.batt_temp_max = 0.0;
        let alerts = evaluate_alerts(&snap, &config);
        assert!(!alerts.contains(&AlertType::BatteryTempHigh));
        assert!(!alerts.contains(&AlertType::BatteryTempLow));
    }

    #[test]
    fn test_soc_low() {
        let mut snap = make_snapshot();
        snap.soc = 5;
        let config = alerts_config();
        let alerts = evaluate_alerts(&snap, &config);
        assert!(alerts.contains(&AlertType::BatterySocLow));
    }

    #[test]
    fn test_soc_high() {
        let mut snap = make_snapshot();
        snap.soc = 99;
        let config = alerts_config();
        let alerts = evaluate_alerts(&snap, &config);
        assert!(alerts.contains(&AlertType::BatterySocHigh));
    }

    #[test]
    fn test_grid_offline() {
        let mut snap = make_snapshot();
        snap.grid_loss = true;
        let mut config = alerts_config();
        config.grid_offline_enabled = true;
        let alerts = evaluate_alerts(&snap, &config);
        assert!(alerts.contains(&AlertType::GridOffline));
    }

    #[test]
    fn test_battery_over_temp() {
        let mut snap = make_snapshot();
        snap.battery_over_temp = true;
        let mut config = alerts_config();
        config.battery_over_temp_enabled = true;
        let alerts = evaluate_alerts(&snap, &config);
        assert!(alerts.contains(&AlertType::BatteryOverTemp));
    }

    // ================================================================
    // Debounce
    // ================================================================

    #[test]
    fn test_debounce_len() {
        let d = AlertDebounce::new();
        assert_eq!(d.len(), 0);
    }

    #[test]
    fn test_debounce_allows_first_fire() {
        let mut d = AlertDebounce::new();
        assert!(d.should_fire(AlertType::GridOffline, 30));
    }

    #[test]
    fn test_debounce_blocks_second_fire() {
        let mut d = AlertDebounce::new();
        d.should_fire(AlertType::GridOffline, 30); // first
        assert!(!d.should_fire(AlertType::GridOffline, 30)); // blocked
    }

    #[test]
    fn test_debounce_allows_different_types() {
        let mut d = AlertDebounce::new();
        d.should_fire(AlertType::GridOffline, 30);
        assert!(d.should_fire(AlertType::BatterySocLow, 30)); // different type allowed
    }

    // ================================================================
    // Hardware battery warning confirmation (IR 57 transient-read defence)
    // ================================================================

    #[test]
    fn test_battery_warning_not_confirmed_on_single_read() {
        // A single transient `true` on IR(57) must NOT fire — this is the
        // exact regression for the reported 21.5°C over-temp false positive.
        let mut d = AlertDebounce::new();
        assert!(!d.confirm_battery_warning(true));
    }

    #[test]
    fn test_battery_warning_confirmed_after_threshold_cycles() {
        let mut d = AlertDebounce::new();
        // Below threshold: never confirmed.
        for _ in 0..(BATTERY_WARNING_CONFIRM_CYCLES - 1) {
            assert!(!d.confirm_battery_warning(true));
        }
        // On the Nth consecutive true, it confirms.
        assert!(d.confirm_battery_warning(true));
        // And stays confirmed while the flag remains set.
        assert!(d.confirm_battery_warning(true));
    }

    #[test]
    fn test_battery_warning_streak_resets_on_false() {
        let mut d = AlertDebounce::new();
        // Build up almost to the threshold.
        for _ in 0..(BATTERY_WARNING_CONFIRM_CYCLES - 1) {
            d.confirm_battery_warning(true);
        }
        // A single `false` resets the streak — a genuine warning that blinks
        // off for one cycle has to rebuild.
        assert!(!d.confirm_battery_warning(false));
        // One true is not enough again.
        assert!(!d.confirm_battery_warning(true));
    }

    #[test]
    fn test_battery_warning_clears_streak_independently_of_debounce_clear() {
        // clear() (called when saving settings) must also wipe the streak so
        // a stale confirmed flag doesn't leak across a config change.
        let mut d = AlertDebounce::new();
        for _ in 0..BATTERY_WARNING_CONFIRM_CYCLES {
            d.confirm_battery_warning(true);
        }
        d.clear();
        assert!(!d.confirm_battery_warning(true));
    }

    // ================================================================
    // Solar clipping
    // ================================================================

    #[test]
    fn test_solar_clipping_fires_when_over_ceiling_and_enabled() {
        // make_snapshot() has solar_power = 5200.
        let mut config = alerts_config();
        config.solar_clipping_enabled = true;
        config.solar_clipping_ceiling_w = 5000;
        let alerts = evaluate_alerts(&make_snapshot(), &config);
        assert!(alerts.contains(&AlertType::SolarClipping));
    }

    #[test]
    fn test_solar_clipping_no_alert_when_disabled() {
        let mut config = alerts_config();
        config.solar_clipping_enabled = false;
        config.solar_clipping_ceiling_w = 5000; // ceiling set but toggle off
        let alerts = evaluate_alerts(&make_snapshot(), &config);
        assert!(!alerts.contains(&AlertType::SolarClipping));
    }

    #[test]
    fn test_solar_clipping_no_alert_when_ceiling_zero() {
        // ceiling_w == 0 means disabled even if the toggle is on.
        let mut config = alerts_config();
        config.solar_clipping_enabled = true;
        config.solar_clipping_ceiling_w = 0;
        let alerts = evaluate_alerts(&make_snapshot(), &config);
        assert!(!alerts.contains(&AlertType::SolarClipping));
    }

    #[test]
    fn test_solar_clipping_no_alert_when_under_ceiling() {
        let mut config = alerts_config();
        config.solar_clipping_enabled = true;
        config.solar_clipping_ceiling_w = 6000; // above solar_power (5200)
        let alerts = evaluate_alerts(&make_snapshot(), &config);
        assert!(!alerts.contains(&AlertType::SolarClipping));
    }

    #[test]
    fn test_solar_clipping_not_confirmed_on_single_read() {
        // A single cycle over the ceiling must NOT fire — precision defence
        // against a momentary cloud-edge spike.
        let mut d = AlertDebounce::new();
        assert!(!d.confirm_solar_clipping(true));
    }

    #[test]
    fn test_solar_clipping_confirmed_after_threshold_cycles() {
        let mut d = AlertDebounce::new();
        for _ in 0..(SOLAR_CLIPPING_CONFIRM_CYCLES - 1) {
            assert!(!d.confirm_solar_clipping(true));
        }
        assert!(d.confirm_solar_clipping(true)); // Nth consecutive cycle
        assert!(d.confirm_solar_clipping(true)); // stays confirmed while over
    }

    #[test]
    fn test_solar_clipping_streak_resets_on_drop_below_ceiling() {
        let mut d = AlertDebounce::new();
        for _ in 0..(SOLAR_CLIPPING_CONFIRM_CYCLES - 1) {
            d.confirm_solar_clipping(true);
        }
        // Solar drops back below the ceiling for one cycle — streak resets.
        assert!(!d.confirm_solar_clipping(false));
        assert!(!d.confirm_solar_clipping(true)); // not enough again
    }

    // ================================================================
    // Build message
    // ================================================================

    #[test]
    fn test_build_alert_message_includes_alerts() {
        let snap = make_snapshot();
        let alerts = vec![AlertType::GridOffline];
        let msg = build_alert_message(&snap, &alerts);
        assert!(msg.contains("Grid Offline"));
        assert!(msg.contains("Battery temp:"));
        assert!(msg.contains("System Status:"));
    }

    #[test]
    fn test_parse_command_strips_suffix_and_case() {
        assert_eq!(parse_command("/status"), "status");
        assert_eq!(parse_command("  /STATUS  "), "status");
        assert_eq!(parse_command("/status@hem_bot"), "status");
        assert_eq!(parse_command("/Help@MyBot extra args"), "help");
        assert_eq!(parse_command("/today"), "today");
        assert_eq!(parse_command("hello"), "hello");
        assert_eq!(parse_command(""), "");
        assert_eq!(parse_command("   "), "");
    }

    #[test]
    fn test_build_help_lists_all_commands() {
        let msg = build_help_message();
        for cmd in [
            "/status", "/today", "/battery", "/mode", "/version", "/help",
        ] {
            assert!(msg.contains(cmd), "help missing {cmd}");
        }
    }
}
